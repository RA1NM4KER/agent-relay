#!/usr/bin/env python3
"""Real app-server, two native threads, bounded disposable heartbeat commands.

No model, credentials, network listener, or existing provider process is used.
Each heartbeat exits itself after 12 seconds, even if its server dies.
"""
import json
from pathlib import Path
import queue
import shlex
import shutil
import tempfile
import time

from stdio_probe import Server, environment


def main():
    root = Path(tempfile.mkdtemp(prefix="relay-shell-", dir="/private/tmp"))
    env = environment(root)
    project = root / "project"
    project.mkdir()
    heartbeat = root / "heartbeat.py"
    heartbeat.write_text("import os,sys,time\nfrom pathlib import Path\n"
        "p=Path(sys.argv[1]); end=time.monotonic()+12\n"
        "while time.monotonic()<end:\n"
        " p.write_text(str(os.getpid())+' '+str(time.monotonic_ns()));time.sleep(.1)\n")
    server = None
    result = {"root": str(root), "cases": []}
    try:
        server = Server(shutil.which("codex"), env, "shared-runtime")
        threads = {}
        for name in ["A", "B"]:
            response = server.call("thread/start", {"cwd": str(project), "model": "research-model",
                "modelProvider": "research", "approvalPolicy": "never", "sandbox": "read-only"})
            threads[name] = response["result"]["thread"]["id"]
            command = shlex.join([shutil.which("python3"), str(heartbeat), str(root / (name + ".heartbeat"))])
            result["cases"].append({"name": name + "_shell", "response":
                server.call("thread/shellCommand", {"threadId": threads[name], "command": command, "timeoutMs": 15000})})
        time.sleep(1)
        def snapshots():
            return {n: (root / (n + ".heartbeat")).read_text() if (root / (n + ".heartbeat")).exists() else None
                    for n in ["A", "B"]}
        result["before"] = snapshots()
        turns = {}
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                event = server.responses.get(timeout=.1)
            except queue.Empty:
                continue
            server.events.append(event)
        for event in server.events:
            if event.get("method") == "turn/started":
                p = event["params"]
                turns[p["threadId"]] = p["turn"]["id"]
        result["turns"] = turns
        if threads["A"] in turns:
            result["interrupt_A"] = server.call("turn/interrupt", {"threadId": threads["A"], "turnId": turns[threads["A"]]})
            time.sleep(.5)
            result["after_interrupt_1"] = snapshots()
            time.sleep(.5)
            result["after_interrupt_2"] = snapshots()
        server.close(kill=True)
        result["after_kill_1"] = snapshots()
        time.sleep(.6)
        result["after_kill_2"] = snapshots()
        result["events"] = server.events
    except Exception as error:
        result["exception"] = repr(error)
    finally:
        if server:
            server.close()
        # Bounded owned tasks naturally expire, so cleanup does not guess descendant pids.
        time.sleep(12)
        (root / "results.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({k: v for k, v in result.items() if k != "events"}, indent=2))


if __name__ == "__main__":
    main()
