#!/usr/bin/env python3
"""Real Codex, disposable HOME/CODEX_HOME, stdio only; no real credentials/provider.

Run: python3 docs/investigations/codex-concurrency/experiments/stdio_probe.py
The backend is an intentionally unreachable loopback address. No real model is used.
Only child processes created here are stopped. Results include the retained temp root.
"""
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time


def environment(root):
    home = root / "home"
    config = root / "codex"
    home.mkdir(exist_ok=True)
    config.mkdir(exist_ok=True)
    (config / "config.toml").write_text('''model = "research-model"
model_provider = "research"
check_for_update_on_startup = false
[model_providers.research]
name = "Offline research: unreachable loopback only"
base_url = "http://127.0.0.1:1/v1"
wire_api = "responses"
requires_openai_auth = false
request_max_retries = 0
stream_max_retries = 0
[analytics]
enabled = false
''')
    return {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(home),
            "CODEX_HOME": str(config), "TMPDIR": str(root), "TERM": "xterm-256color",
            "LANG": "en_US.UTF-8", "RUST_LOG": "error"}


class Server:
    def __init__(self, executable, env, name, arguments=None):
        self.name = name
        self.events = []
        self.serial = 0
        self.responses = queue.Queue()
        self.stderr = []
        self.child = subprocess.Popen([executable, *(arguments if arguments is not None else ["app-server"])], env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, bufsize=1, start_new_session=True, cwd=env["HOME"])
        def read():
            for line in self.child.stdout:
                try:
                    self.responses.put(json.loads(line))
                except json.JSONDecodeError:
                    self.events.append({"non_json": line[:200]})
            self.responses.put({"research_server_eof": True})
        threading.Thread(target=read, daemon=True).start()
        threading.Thread(target=lambda: self.stderr.extend(self.child.stderr), daemon=True).start()
        try:
            self.call("initialize", {"clientInfo": {"name": "relay-concurrency-research", "version": "0"},
                "capabilities": {"experimentalApi": True}})
            self.send({"method": "initialized", "params": {}})
        except Exception:
            self.close()
            raise

    def send(self, value):
        self.child.stdin.write(json.dumps(value) + "\n")
        self.child.stdin.flush()

    def call(self, method, params, timeout=25):
        self.serial += 1
        serial = self.serial
        start = time.monotonic()
        self.send({"id": serial, "method": method, "params": params})
        deadline = start + timeout
        while time.monotonic() < deadline:
            try:
                value = self.responses.get(timeout=max(0.01, deadline - time.monotonic()))
            except queue.Empty:
                break
            if value.get("research_server_eof"):
                raise RuntimeError(f"{self.name}: EOF during {method}; exit={self.child.poll()}; stderr={''.join(self.stderr)[-1000:]}")
            if value.get("id") == serial:
                return value
            self.events.append(value)
        raise TimeoutError(f"{self.name}: {method}; exit={self.child.poll()}; stderr={''.join(self.stderr)[-1000:]}")

    def close(self, kill=False):
        if self.child.poll() is None:
            (self.child.kill if kill else self.child.terminate)()
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(timeout=5)


def main():
    root = Path(tempfile.mkdtemp(prefix="relay-concurrency-", dir="/private/tmp"))
    env = environment(root)
    executable = shutil.which("codex")
    project = root / "project"
    project.mkdir()
    second_project = root / "other-project" if "--different-projects" in sys.argv else project
    second_project.mkdir(exist_ok=True)
    results = {"root": str(root), "codex": executable, "version": subprocess.check_output(
        [executable, "--version"], env=env, text=True).strip(), "cases": []}
    servers = []
    try:
        for name in ("A", "B"):
            servers.append(Server(executable, env, name))
        a, b = servers
        params = {"cwd": str(project), "model": "research-model", "modelProvider": "research",
            "approvalPolicy": "never", "sandbox": "read-only"}
        start_a = a.call("thread/start", params)
        results["cases"].append({"name": "A_start", "response": start_a})
        start_b = b.call("thread/start", {**params, "cwd": str(second_project)})
        results["cases"].append({"name": "B_start", "response": start_b})
        ta = start_a.get("result", {}).get("thread", {}).get("id")
        tb = start_b.get("result", {}).get("thread", {}).get("id")
        if ta and tb:
            turns = []
            for server, thread in [(a, ta), (b, tb)]:
                response = server.call("turn/start", {"threadId": thread,
                    "input": [{"type": "text", "text": "Offline research turn; reply READY.", "text_elements": []}]})
                results["cases"].append({"name": server.name + "_offline_turn", "response": response})
                turns.append((server, thread, response["result"]["turn"]["id"]))
            time.sleep(.4)
            for server, thread, turn in turns:
                results["cases"].append({"name": server.name + "_interrupt_offline_turn", "response":
                    server.call("turn/interrupt", {"threadId": thread, "turnId": turn})})
            # Real turns are expected to fail against the intentionally unreachable loopback.
            # Persistence is lazy until a turn starts, so wait for each terminal event.
            for server in (a, b):
                deadline = time.monotonic() + 3
                if any(x.get("method") == "turn/completed" for x in server.events):
                    continue
                while time.monotonic() < deadline:
                    try:
                        value = server.responses.get(timeout=max(0.01, deadline - time.monotonic()))
                    except queue.Empty:
                        break
                    server.events.append(value)
                    if value.get("method") == "turn/completed":
                        break
            for name, server, method, p in [
                ("A_read_own", a, "thread/read", {"threadId": ta}),
                ("B_read_A", b, "thread/read", {"threadId": ta}),
                ("B_resume_A_while_A_live", b, "thread/resume", {"threadId": ta, "excludeTurns": True}),
                ("B_fork_A", b, "thread/fork", {"threadId": ta, "excludeTurns": True}),
                ("A_loaded", a, "thread/loaded/list", {}),
                ("B_loaded", b, "thread/loaded/list", {}),
            ]:
                results["cases"].append({"name": name, "response": server.call(method, p)})
            a.close(kill=True)
            results["cases"].append({"name": "B_resume_A_after_A_SIGKILL", "response":
                b.call("thread/resume", {"threadId": ta, "excludeTurns": True})})
            results["cases"].append({"name": "B_read_B_after_A_SIGKILL", "response":
                b.call("thread/read", {"threadId": tb})})
    except Exception as error:
        results["exception"] = repr(error)
    finally:
        for server in servers:
            server.close()
            results[server.name + "_events"] = server.events
            results[server.name + "_stderr"] = "".join(server.stderr)
        (root / "results.json").write_text(json.dumps(results, indent=2))
        print(json.dumps({"root": str(root), "cases": [{"name": c["name"],
            "error": c["response"].get("error"), "ok": "result" in c["response"]}
            for c in results["cases"]], "exception": results.get("exception")}, indent=2))


if __name__ == "__main__":
    main()
