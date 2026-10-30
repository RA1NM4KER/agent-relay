#!/usr/bin/env python3
"""Eight rebuilt native TUIs in a disposable home, for a suitable build/lab host.

Not run here: no rebuilt Codex workspace, and shared mode needs socket listeners.
Fails on missing source coverage; never substitutes delayed hooks or rollout guesses.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time
from controller import NativeRPC, Registry
from stdio_probe import environment


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--mode", choices=["embedded", "shared"], required=True)
    args = parser.parse_args()
    executable = args.codex.resolve(strict=True)
    # Require an explicit disposable build, never resolve installed Codex from PATH.
    if "target" not in executable.parts:
        raise ValueError("expected an explicit source-build target path")
    root = Path(tempfile.mkdtemp(prefix="relay-native-tui-lab-"))
    env = environment(root)
    registry = Registry(root / "admission", env["CODEX_HOME"])
    callback = root / "callback"
    callback.write_text(f'#!{sys.executable}\nimport sys\nsys.path.insert(0,{str(Path(__file__).resolve().parent)!r})\nfrom native_callback import main\nmain()\n')
    callback.chmod(0o700)
    result = {"root": str(root), "mode": args.mode, "checks": [], "full_parity": False}
    terminals = []
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
            return dict(db.execute("SELECT owner,thread FROM selections"))
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
        check("eight initial native selections acknowledged before any prompt", lambda: len(selections()) == 8)
        original = selections()
        command_all("Offline source-integration prompt.")
        for terminal in terminals:
            os.write(terminal["fd"], b"\x03")
        pump(1)
        command_all("/new")
        check("eight native new selections before their prompts", lambda: all(selections().get(k) != v for k, v in original.items()))
        new = selections()
        command_all("Offline prompt to materialize new histories.")
        for terminal in terminals:
            os.write(terminal["fd"], b"\x03")
        pump(1)
        command_all("/fork")
        check("eight fork selections before new prompts", lambda: all(selections().get(k) != v for k, v in new.items()))
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
    except BaseException as error:
        result["exception"] = repr(error)
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
                    env=env, cwd=str(root), capture_output=True, text=True, timeout=15)
                result["isolated_daemon_cleanup_exit"] = stopped.returncode
            except subprocess.TimeoutExpired:
                result["isolated_daemon_cleanup"] = "timed out; inspect this lab home only"
        (root / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({k: v for k, v in result.items() if k != "registry"}, indent=2))
        if result.get("exception"):
            raise SystemExit(1)


if __name__ == "__main__":
    main()
