#!/usr/bin/env python3
"""Two real embedded Codex TUIs in disposable PTYs; no real backend or credentials.

Hook trust bypass is restricted to the script we create in this disposable home.
Terminal bytes are research diagnostics, never an identity or safety oracle.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import struct
import subprocess
import tempfile
import termios
import time

from stdio_probe import Server, environment


class Terminal:
    def __init__(self, executable, env, project, name, extra=()):
        self.name = name
        self.raw = bytearray()
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
        self.child = subprocess.Popen([executable, "--no-daemon", "--no-alt-screen",
            "--dangerously-bypass-hook-trust", "-C", str(project), *extra],
            env={**env, "RESEARCH_CLIENT": name}, stdin=slave, stdout=slave,
            stderr=slave, start_new_session=True)
        os.close(slave)

    def read(self):
        if not select.select([self.master], [], [], 0)[0]:
            return
        try:
            value = os.read(self.master, 65536)
        except OSError:
            return
        self.raw.extend(value)
        if b"\x1b[6n" in value:
            os.write(self.master, b"\x1b[1;1R")
        if b"\x1b[c" in value:
            os.write(self.master, b"\x1b[?1;2c")

    def send(self, value):
        os.write(self.master, value)

    def close(self):
        if self.child.poll() is None:
            self.child.terminate()
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(timeout=5)
        self.read()
        os.close(self.master)


def main():
    root = Path(tempfile.mkdtemp(prefix="relay-tui-", dir="/private/tmp"))
    env = environment(root)
    project = root / "project"
    project.mkdir()
    hook = root / "hook.py"
    output = root / "hooks.jsonl"
    hook.write_text("import json,os,sys,time\n" +
        f"with open({str(output)!r}, 'a') as f:\n" +
        " f.write(json.dumps({'event':json.load(sys.stdin),'marker':os.environ.get('RESEARCH_CLIENT'),"
        "'pid':os.getpid(),'ppid':os.getppid(),'time_ns':time.monotonic_ns()})+'\\n')\n")
    with (root / "codex" / "config.toml").open("a") as f:
        f.write(f'\n[projects.{json.dumps(str(project))}]\ntrust_level = "trusted"\n')
        for event in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"]:
            f.write(f'\n[[hooks.{event}]]\n[[hooks.{event}.hooks]]\ntype = "command"\n'
                    f'command = {json.dumps(shutil.which("python3") + " " + str(hook))}\ntimeout = 3\n')
    terminals = []
    actions = []
    snapshots = []
    exception = None
    try:
        # Separate bootstrap/migration races from steady-state concurrent TUI behavior.
        warm = Server(shutil.which("codex"), env, "database-bootstrap")
        warm.close()
        terminals = [Terminal(shutil.which("codex"), env, project, n) for n in ["A", "B"]]
        def pump(seconds):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                for terminal in terminals:
                    terminal.read()
                time.sleep(0.02)
        def snapshot(label):
            events = [json.loads(x) for x in output.read_text().splitlines()] if output.exists() else []
            headers = []
            for path in (root / "codex" / "sessions").rglob("*.jsonl"):
                lines = path.read_text().splitlines()
                if lines:
                    headers.append(json.loads(lines[0]))
            snapshots.append({"label": label, "time_ns": time.monotonic_ns(),
                              "hooks": events, "headers": headers})
        pump(5)
        snapshot("startup_before_any_prompt")
        for terminal in terminals:
            terminal.send(b"Offline research prompt.")
        pump(0.5)
        for terminal in terminals:
            terminal.send(b"\r")
        pump(3)
        terminals[0].send(b"\x03")
        pump(1)
        terminals[0].send(b"/new")
        pump(0.5)
        terminals[0].send(b"\r")
        actions.append("A /new")
        pump(2)
        snapshot("after_new_before_prompt")
        terminals[0].send(b"Second offline research prompt.")
        pump(0.5)
        terminals[0].send(b"\r")
        pump(3)
        terminals[0].send(b"\x03")
        pump(1)
        terminals[0].send(b"/fork")
        pump(0.5)
        terminals[0].send(b"\r")
        actions.append("A /fork")
        pump(3)
        snapshot("after_fork_before_prompt")
        terminals[0].send(b"After fork offline prompt.")
        pump(.5)
        terminals[0].send(b"\r")
        pump(3)
        snapshot("after_fork_prompt")
        terminals[0].send(b"\x03")
        pump(1)
        # Exercise the native saved-chat picker. Input is a UUID from this run's
        # own A hook; no timestamp/cwd/file-recency identity inference is used.
        events = [json.loads(x) for x in output.read_text().splitlines()]
        original = next(x["event"]["session_id"] for x in events if x["marker"] == "A")
        terminals[0].send(b"/resume")
        pump(.5)
        terminals[0].send(b"\r")
        pump(2)
        terminals[0].send(original.encode())
        pump(.5)
        terminals[0].send(b"\r")
        actions.append("A /resume picker searched original A UUID")
        pump(3)
        snapshot("after_resume_before_prompt")
        terminals[0].send(b"After resume offline prompt.")
        pump(.5)
        terminals[0].send(b"\r")
        pump(3)
        snapshot("after_resume_prompt")
    except Exception as error:
        exception = repr(error)
    finally:
        for terminal in terminals:
            terminal.close()
            (root / (terminal.name + ".terminal")).write_bytes(terminal.raw)
        hooks = [json.loads(x) for x in output.read_text().splitlines()] if output.exists() else []
        results = {"root": str(root), "actions": actions, "hooks": hooks,
            "snapshots": snapshots, "exits": {t.name: t.child.returncode for t in terminals}, "exception": exception}
        (root / "results.json").write_text(json.dumps(results, indent=2))
        print(json.dumps({"root": str(root), "actions": actions, "exception": exception,
            "snapshots": [{"label": x["label"], "hooks": len(x["hooks"]),
                "headers": len(x["headers"])} for x in snapshots]}, indent=2))


if __name__ == "__main__":
    main()
