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
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time
from controller import NativeRPC
from native_callback import CallbackRegistry
from stdio_probe import environment


def changed_bindings(current, previous):
    return (current.keys() == previous.keys() and
            all(isinstance(v, str) and v and v != previous[k] for k, v in current.items()) and
            len(set(current.values())) == len(current))


def completed_foreground(events, bindings, after):
    if any(e["kind"] == "coverage-lost" for e in events):
        raise AssertionError("native control-connection event coverage lost")
    for owner, thread in bindings.items():
        rows = [e for e in events if e["client"] == owner and e["thread"] == thread
                and e["sequence"] > after.get(owner, -1)]
        started = {e["turn"] for e in rows if e["kind"] == "turn/started"}
        accepted = {e["turn"] for e in rows if e["kind"] == "turn/accepted"}
        completed = {e["turn"] for e in rows if e["kind"] == "turn/completed"}
        if not started or not accepted or not (started | accepted) <= completed:
            return False
    return True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--mode", choices=["embedded", "shared"], required=True)
    args = parser.parse_args()
    executable = args.codex.resolve(strict=True)
    # Require an explicit disposable build, never resolve installed Codex from PATH.
    if "target" not in executable.parts:
        raise ValueError("expected an explicit source-build target path")
    root = Path(tempfile.mkdtemp(prefix="relay-native-tui-lab-", dir="/private/tmp"))
    env = environment(root)
    registry = CallbackRegistry(root / "admission", env["CODEX_HOME"])
    callback = root / "callback"
    callback.write_text(f'#!{sys.executable}\nimport sys\nsys.path.insert(0,{str(Path(__file__).resolve().parent)!r})\nfrom native_callback import main\nmain()\n')
    callback.chmod(0o700)
    result = {"root": str(root), "mode": args.mode, "checks": [], "full_parity": False}
    terminals = []
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
    def prompt_and_interrupt(text, bindings):
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
        for terminal in terminals:
            os.write(terminal["fd"], b"\x03")
        check("eight exact foreground turns complete before navigation",
              lambda: completed_foreground(events(), bindings, after))
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
                return
            pump(.1)
        raise AssertionError(name)
    try:
        # Serialize provider first-use initialization; no session/thread is resumed.
        warm = NativeRPC(str(executable), env)
        warm.close()
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
            digest = hashlib.sha256(managed.read_bytes()).hexdigest()
            if digest != hashlib.sha256(executable.read_bytes()).hexdigest():
                raise AssertionError("daemon does not run the exact rebuilt binary")
            if not daemon.get("pid") or not Path(daemon["socketPath"]).is_socket():
                raise AssertionError("isolated native daemon identity/socket unavailable")
            result["daemon_binary_sha256"] = digest
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
        check("eight initial native selections acknowledged before any prompt",
              lambda: len(selections()) == 8 and all(selections().values()) and len(set(selections().values())) == 8)
        original = selections()
        prompt_and_interrupt("Offline source-integration prompt.", original)
        command_all("/new")
        check("eight native new selections before their prompts", lambda: changed_bindings(selections(), original))
        new = selections()
        prompt_and_interrupt("Offline prompt to materialize new histories.", new)
        command_all("/fork")
        check("eight fork selections before new prompts", lambda: changed_bindings(selections(), new))
        command_all("/resume")
        for i, terminal in enumerate(terminals):
            os.write(terminal["fd"], original[f"pane-{i}"].encode())
        pump(.5)
        for terminal in terminals:
            os.write(terminal["fd"], b"\r")
        check("eight native picker resumes bind exact original IDs", lambda: selections() == original)
        terminals[0]["child"].kill()
        terminals[0]["child"].wait(timeout=5)
        pump(.5)
        check("seven native frontends remain alive after A crash", lambda: all(t["child"].poll() is None for t in terminals[1:]))
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
        for i, terminal in enumerate(terminals):
            child = terminal["child"]
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
            os.close(terminal["fd"])
            (root / f"pane-{i}.terminal").write_bytes(terminal["raw"])
        if args.mode == "shared":
            try:
                stopped = subprocess.run([str(executable), "app-server", "daemon", "stop"],
                    env=env, cwd=str(root), capture_output=True, text=True, timeout=180)
                result["isolated_daemon_cleanup_exit"] = stopped.returncode
                if stopped.returncode:
                    result.setdefault("exception", "isolated native daemon cleanup refused")
            except subprocess.TimeoutExpired:
                result["isolated_daemon_cleanup"] = "timed out; inspect this lab home only"
                result.setdefault("exception", "isolated native daemon cleanup timed out")
        (root / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({k: v for k, v in result.items() if k != "registry"}, indent=2))
        if result.get("exception"):
            raise SystemExit(1)


if __name__ == "__main__":
    main()
