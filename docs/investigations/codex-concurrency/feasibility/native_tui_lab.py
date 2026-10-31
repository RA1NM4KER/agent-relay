#!/usr/bin/env python3
"""Eight rebuilt native TUIs in a disposable home, for a suitable build/lab host.

Native source/control-connection evidence supplies identity and turn completion.
Fails on missing source coverage; never substitutes delayed hooks or rollout guesses.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import signal
import socket
import shutil
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time
from controller import NativeRPC
from holding_provider import HoldingProvider
from native_callback import CallbackRegistry
from stdio_probe import Server, environment


def file_sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def changed_bindings(current, previous):
    return (bool(previous) and current.keys() == previous.keys() and
            all(isinstance(v, str) and v and v != previous[k] for k, v in current.items()) and
            len(set(current.values())) == len(current))


def completed_foreground(events, bindings, after):
    if any(e["kind"] == "coverage-lost" for e in events):
        raise AssertionError("native control-connection event coverage lost")
    if not bindings:
        return False
    for owner, thread in bindings.items():
        rows = [e for e in events if e["client"] == owner and e["thread"] == thread
                and e["sequence"] > after.get(owner, -1)]
        started = {e["turn"] for e in rows if e["kind"] == "turn/started"}
        accepted = {e["turn"] for e in rows if e["kind"] == "turn/accepted"}
        completed = {e["turn"] for e in rows if e["kind"] == "turn/completed"}
        if not started or not accepted or not (started | accepted) <= completed:
            return False
    return True


def loaded_bindings(data, bindings):
    if (not isinstance(data, list) or any(not isinstance(v, str) or not v for v in data)
            or len(set(data)) != len(data)):
        raise AssertionError("invalid daemon loaded-thread inventory")
    return bool(bindings) and set(bindings.values()) <= set(data)


def socket_quiet(path):
    # A stale socket inode is not evidence of a live daemon. Unknown errors
    # remain errors; only absence/refusal establishes this narrow observation.
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
        probe.settimeout(.5)
        try:
            probe.connect(str(path))
        except (FileNotFoundError, ConnectionRefusedError):
            return True
    return False


class SharedLoadedObserver(Server):
    """Supported socket proxy, restricted to reads; never resumes a writer."""
    def __init__(self, executable, env, socket_path):
        self.calls = []
        super().__init__(str(executable), env, "shared-loaded-proof",
                         arguments=["app-server", "proxy", "--sock", socket_path])
        if Path(self.home).resolve(strict=True) != Path(env["CODEX_HOME"]).resolve(strict=True):
            self.close()
            raise AssertionError("shared observer resolved a foreign home")

    def call(self, method, params, timeout=25):
        if method not in {"initialize", "thread/loaded/list", "thread/turns/list"}:
            raise AssertionError("read-only shared proof cannot mutate or resume")
        self.calls.append(method)
        response = super().call(method, params, timeout)
        if "error" in response or "result" not in response:
            raise AssertionError(f"shared proof RPC refused: {response}")
        if method == "initialize":
            self.home = response["result"].get("codexHome", "")
        return response

    def loaded(self):
        response = self.call("thread/loaded/list", {"limit": 256})["result"]
        if response.get("nextCursor"):
            raise AssertionError("shared proof inventory exceeded its evidence bound")
        return response["data"]

    def turn_states(self, thread):
        response = self.call("thread/turns/list", {"threadId": thread, "limit": 64})["result"]
        if response.get("nextCursor"):
            raise AssertionError("shared turn-state proof exceeded its evidence bound")
        return {turn["id"]: turn["status"] for turn in response["data"]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--mode", choices=["embedded", "shared"], required=True)
    parser.add_argument("--holding-provider", action="store_true",
                        help="hold real HTTP response streams to prove cancellation")
    args = parser.parse_args()
    executable = args.codex.resolve(strict=True)
    # Require an explicit disposable build, never resolve installed Codex from PATH.
    if "target" not in executable.parts:
        raise ValueError("expected an explicit source-build target path")
    free = shutil.disk_usage("/private/tmp").free
    if args.mode == "shared" and free < 4 * 1024 ** 3:
        raise RuntimeError(f"shared lab preflight requires 4 GiB free; observed {free} bytes; no runtime launched")
    root = Path(tempfile.mkdtemp(prefix="relay-native-tui-lab-", dir="/private/tmp"))
    env = environment(root)
    registry = CallbackRegistry(root / "admission", env["CODEX_HOME"])
    callback = root / "callback"
    callback.write_text(f'#!{sys.executable}\nimport sys\nsys.path.insert(0,{str(Path(__file__).resolve().parent)!r})\nfrom native_callback import main\nmain()\n')
    callback.chmod(0o700)
    result = {"root": str(root), "harness_pid": os.getpid(), "mode": args.mode, "checks": [], "full_parity": False,
              "inference_fixture": "holding" if args.holding_provider else "unreachable",
              "binding_phases": {}, "shared_loaded_proof": [], "shared_turn_states": []}
    terminals = []
    observer = None
    provider = None
    daemon_pid = None
    daemon_socket = None
    def flush_progress():
        pending = root / "progress.pending"
        pending.write_text(json.dumps(result, indent=2) + "\n")
        pending.replace(root / "progress.json")
    flush_progress()
    def terminate_lab(signum, frame):
        raise InterruptedError("lab received SIGTERM; preserving evidence and cleaning up")
    signal.signal(signal.SIGTERM, terminate_lab)
    def pump(seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            for terminal in terminals:
                fd = terminal["fd"]
                if select.select([fd], [], [], 0)[0]:
                    try:
                        value = os.read(fd, 65536)
                    except OSError:
                        continue
                    terminal["raw"].extend(value)
                    if len(terminal["raw"]) > 8 * 1024 * 1024:
                        raise AssertionError("native terminal evidence limit")
                    if b"\x1b[6n" in value:
                        os.write(fd, b"\x1b[1;1R")
            time.sleep(.02)
    def selections():
        with registry.connect() as db:
            return dict(db.execute("SELECT owner,primary_thread FROM native_views"))
    def events():
        rows = []
        for i, terminal in enumerate(terminals):
            path = root / "admission" / f"pane-{i}-{terminal['child'].pid}.events.jsonl"
            if not path.exists():
                continue
            data = path.read_bytes()
            if len(data) > 1024 * 1024:
                raise AssertionError("native event evidence limit")
            sequence = -1
            for line in data.splitlines(keepends=True):
                if not line.endswith(b"\n"):
                    break
                event = json.loads(line)
                if event["client"] != f"pane-{i}" or event["pid"] != terminal["child"].pid:
                    raise AssertionError("wrong native event client/process")
                if event["sequence"] <= sequence:
                    raise AssertionError("native event sequence replay/reordering")
                sequence = event["sequence"]
                rows.append(event)
        return rows
    def event_cursor():
        return {f"pane-{i}": max((e["sequence"] for e in events() if e["client"] == f"pane-{i}"), default=-1)
                for i in range(len(terminals))}
    def start_foreground(text, bindings):
        after = event_cursor()
        command_all(text)
        def starts_observed():
            rows = events()
            if any(e["kind"] == "coverage-lost" for e in rows):
                raise AssertionError("native event coverage lost before interrupt")
            return all(any(e["client"] == owner and e["thread"] == thread
                           and e["kind"] == "turn/started" and e["sequence"] > after.get(owner, -1)
                           for e in rows) for owner, thread in bindings.items())
        check("eight exact foreground native starts observed", starts_observed)
        if provider:
            check("eight exact foreground response streams open concurrently",
                  lambda: set(provider.active(set(bindings.values()))) == set(bindings.values()))
            record_turn_states(bindings, after, "inProgress")
        return after

    def interrupt_foreground(bindings, after, indices):
        for i in indices:
            os.write(terminals[i]["fd"], b"\x03")
        check("exact interrupted foreground turns complete before navigation",
              lambda: completed_foreground(events(), bindings, after))
        if provider:
            check("interruption disconnects exact held foreground responses",
                  lambda: provider.disconnected(set(bindings.values())))
            record_turn_states(bindings, after, "interrupted")

    def prompt_and_interrupt(text, bindings):
        after = start_foreground(text, bindings)
        interrupt_foreground(bindings, after, range(8))

    def record_bindings(phase, bindings):
        result["binding_phases"][phase] = dict(bindings)
        with registry.connect() as db:
            for owner, thread in bindings.items():
                row = db.execute("SELECT owner,state FROM owners WHERE home=? AND thread=?",
                                 (registry.home, thread)).fetchone()
                role = db.execute("SELECT owner,ephemeral FROM native_roles WHERE home=? AND thread=?",
                                  (registry.home, thread)).fetchone()
                if row != (owner, "ACTIVE") or role != (owner, 0):
                    raise AssertionError("foreground binding lacks exact active owner/native role")
        if observer:
            data = observer.loaded()
            result["shared_loaded_proof"].append({"phase": phase, "expected": bindings, "loaded": data})
            check("exact foreground bindings loaded in actual shared daemon: " + phase,
                  lambda: loaded_bindings(data, bindings))

    def record_turn_states(bindings, after, status):
        if observer:
            rows = events()
            for owner, thread in bindings.items():
                accepted = {e["turn"] for e in rows if e["client"] == owner and e["thread"] == thread
                            and e["kind"] == "turn/accepted" and e["sequence"] > after.get(owner, -1)}
                if len(accepted) != 1:
                    raise AssertionError("turn-state proof requires one exact accepted foreground turn")
                states = observer.turn_states(thread)
                proof = {"owner": owner, "thread": thread, "expected_turns": sorted(accepted),
                         "expected_status": status, "states": states}
                result["shared_turn_states"].append(proof)
                if any(states.get(turn) != status for turn in accepted):
                    raise AssertionError("shared daemon reports unexpected exact foreground turn status")
    def command_all(text):
        for terminal in terminals:
            os.write(terminal["fd"], text.encode())
        pump(.5)
        for terminal in terminals:
            os.write(terminal["fd"], b"\r")
        pump(3)
    def check(name, predicate):
        end = time.monotonic() + 10
        while time.monotonic() < end:
            if predicate():
                result["checks"].append(name)
                flush_progress()
                return
            pump(.1)
        raise AssertionError(name)
    try:
        # Serialize provider first-use initialization; no session/thread is resumed.
        warm = NativeRPC(str(executable), env)
        warm.close()
        if args.holding_provider:
            provider = HoldingProvider(root / "provider.events.jsonl")
            config = root / "codex/config.toml"
            config.write_text(config.read_text().replace("http://127.0.0.1:1/v1", provider.url))
            result["provider_url"] = provider.url
        for name in ["project-a", "project-b"]:
            project = root / name
            project.mkdir()
            with (root / "codex/config.toml").open("a") as cfg:
                cfg.write(f'\n[projects.{json.dumps(str(project))}]\ntrust_level="trusted"\n')
        if args.mode == "shared":
            # A debug package hashes/copies nearly a GiB before server launch.
            # This bounds installation only; the native identity/turn gates stay strict.
            try:
                started = subprocess.run([str(executable), "app-server", "daemon", "start"],
                    env={**env, "RUST_LOG": "error,codex_app_server_daemon=info"},
                    cwd=str(root), capture_output=True, text=True, timeout=180)
            except subprocess.TimeoutExpired as timeout:
                def decoded(value):
                    return value.decode(errors="replace") if isinstance(value, bytes) else value
                result["daemon_start_timeout"] = {"stdout": decoded(timeout.stdout),
                    "stderr": decoded(timeout.stderr), "seconds": timeout.timeout}
                raise
            result["daemon_start"] = {"exit_code": started.returncode,
                "stdout": started.stdout, "stderr": started.stderr}
            if started.returncode:
                raise AssertionError("isolated native daemon start failed")
            daemon = json.loads(started.stdout)
            managed = Path(daemon["managedCodexPath"]).resolve(strict=True)
            if not managed.is_relative_to(Path(env["CODEX_HOME"]).resolve()):
                raise AssertionError("daemon executable escaped isolated home")
            digest = file_sha256(managed)
            if digest != file_sha256(executable):
                raise AssertionError("daemon does not run the exact rebuilt binary")
            if not daemon.get("pid") or not Path(daemon["socketPath"]).is_socket():
                raise AssertionError("isolated native daemon identity/socket unavailable")
            daemon_pid = daemon["pid"]
            daemon_socket = Path(daemon["socketPath"])
            result["daemon_binary_sha256"] = digest
            result["daemon_pid"] = daemon_pid
            result["daemon_socket"] = str(daemon_socket)
            flush_progress()
            observer = SharedLoadedObserver(executable, env, daemon["socketPath"])
            if observer.loaded():
                raise AssertionError("fresh shared daemon unexpectedly has loaded roots")
        for i in range(8):
            project = root / ("project-a" if i % 2 == 0 else "project-b")
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
            argv = [str(executable), "--no-alt-screen", "-C", str(project)]
            if args.mode == "embedded":
                argv.append("--no-daemon")
            child = subprocess.Popen(argv, cwd=str(root), env={**env,
                "RELAY_RESEARCH_CALLBACK": str(callback), "RELAY_RESEARCH_CLIENT": f"pane-{i}",
                "RELAY_RESEARCH_REGISTRY": str(root / "admission")},
                stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
            os.close(slave)
            terminals.append({"child": child, "fd": master, "raw": bytearray()})
        result["frontend_pids"] = [t["child"].pid for t in terminals]
        flush_progress()
        check("eight initial native selections acknowledged before any prompt",
              lambda: len(selections()) == 8 and all(selections().values()) and len(set(selections().values())) == 8)
        original = selections()
        record_bindings("initial", original)
        prompt_and_interrupt("Offline source-integration prompt.", original)
        command_all("/new")
        check("eight native new selections before their prompts", lambda: changed_bindings(selections(), original))
        new = selections()
        record_bindings("new", new)
        prompt_and_interrupt("Offline prompt to materialize new histories.", new)
        command_all("/fork")
        check("eight fork selections before new prompts", lambda: changed_bindings(selections(), new))
        record_bindings("fork", selections())
        command_all("/resume")
        for i, terminal in enumerate(terminals):
            os.write(terminal["fd"], original[f"pane-{i}"].encode())
        pump(.5)
        for terminal in terminals:
            os.write(terminal["fd"], b"\r")
        check("eight native picker resumes bind exact original IDs", lambda: selections() == original)
        record_bindings("resume", original)
        if provider:
            after = start_foreground("Independent cancellation and frontend termination probe.", original)
            neighbors = {k: v for k, v in original.items() if k != "pane-0"}
            held = provider.active(set(neighbors.values()))
            interrupt_foreground({"pane-0": original["pane-0"]}, after, [0])
            def neighbors_progress():
                active = provider.active(set(neighbors.values()))
                return (set(active) == set(held) and all(
                    active[t]["request"] == held[t]["request"] and
                    active[t]["heartbeats"] > held[t]["heartbeats"] for t in held)
                    and not any(e["kind"] == "turn/completed" and e["thread"] in neighbors.values()
                                and e["sequence"] > after.get(e["client"], -1) for e in events()))
            check("A cancellation leaves seven exact native streams progressing", neighbors_progress)
            record_turn_states(neighbors, after, "inProgress")
        terminals[0]["child"].kill()
        terminals[0]["child"].wait(timeout=5)
        pump(.5)
        check("seven native frontends remain alive after A crash", lambda: all(t["child"].poll() is None for t in terminals[1:]))
        if provider:
            check("A frontend crash leaves seven exact native streams progressing", neighbors_progress)
            record_bindings("after-A-crash", neighbors)
            record_turn_states(neighbors, after, "inProgress")
            interrupt_foreground(neighbors, after, range(1, 8))
        result["registry"] = registry.snapshot()
        result["native_turn_events"] = events()
    except BaseException as error:
        result["exception"] = repr(error)
        try:
            result["registry"] = registry.snapshot()
            result["native_turn_events"] = events()
        except BaseException as evidence_error:
            result["partial_evidence_error"] = repr(evidence_error)
    finally:
        def cleanup_failure(stage, error):
            result.setdefault("cleanup_errors", []).append({"stage": stage, "error": repr(error)})
            result.setdefault("exception", f"lab cleanup failed: {stage}")
        if observer:
            result["shared_observer_methods"] = observer.calls
            try:
                observer.close()
            except BaseException as cleanup_error:
                cleanup_failure("read-only observer", cleanup_error)
            result["shared_observer_cleanup_exit"] = observer.child.returncode
        for i, terminal in enumerate(terminals):
            child = terminal["child"]
            try:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=5)
            except BaseException as cleanup_error:
                cleanup_failure(f"frontend {i}", cleanup_error)
            try:
                os.close(terminal["fd"])
            except OSError as cleanup_error:
                cleanup_failure(f"frontend {i} PTY", cleanup_error)
        result["frontend_cleanup"] = [{"client": f"pane-{i}", "pid": t["child"].pid,
                                       "exit_code": t["child"].returncode}
                                      for i, t in enumerate(terminals)]
        if args.mode == "shared":
            try:
                stopped = subprocess.run([str(executable), "app-server", "daemon", "stop"],
                    env=env, cwd=str(root), capture_output=True, text=True, timeout=180)
                result["isolated_daemon_cleanup_exit"] = stopped.returncode
                result["isolated_daemon_cleanup_stdout"] = stopped.stdout
                result["isolated_daemon_cleanup_stderr"] = stopped.stderr
                if stopped.returncode:
                    result.setdefault("exception", "isolated native daemon cleanup refused")
                if daemon_pid:
                    try:
                        os.kill(daemon_pid, 0)
                    except ProcessLookupError:
                        result["daemon_pid_absent_after_cleanup"] = True
                    else:
                        result["daemon_pid_absent_after_cleanup"] = False
                        result.setdefault("exception", "daemon PID still exists after native cleanup")
                if daemon_socket:
                    result["daemon_socket_quiet_after_cleanup"] = socket_quiet(daemon_socket)
                    if not result["daemon_socket_quiet_after_cleanup"]:
                        result.setdefault("exception", "daemon endpoint still accepts connections after cleanup")
            except subprocess.TimeoutExpired as timeout:
                result["isolated_daemon_cleanup"] = "timed out; inspect this lab home only"
                result["isolated_daemon_cleanup_stdout"] = repr(timeout.stdout)
                result["isolated_daemon_cleanup_stderr"] = repr(timeout.stderr)
                result.setdefault("exception", "isolated native daemon cleanup timed out")
            except BaseException as cleanup_error:
                cleanup_failure("native daemon stop/readback", cleanup_error)
        if provider:
            try:
                provider.close()
            except BaseException as cleanup_error:
                cleanup_failure("holding provider", cleanup_error)
            result["provider_requests"] = provider.snapshot()
        # Evidence I/O must never prevent attempts to stop the remaining owned
        # processes. A full disk may make evidence unavailable, never cleanup green.
        for i, terminal in enumerate(terminals):
            try:
                (root / f"pane-{i}.terminal").write_bytes(terminal["raw"])
            except OSError as evidence_error:
                cleanup_failure(f"frontend {i} terminal evidence", evidence_error)
        (root / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({k: v for k, v in result.items() if k != "registry"}, indent=2))
        if result.get("exception"):
            raise SystemExit(1)


if __name__ == "__main__":
    main()
