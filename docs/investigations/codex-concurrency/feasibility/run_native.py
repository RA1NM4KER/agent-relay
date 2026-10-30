#!/usr/bin/env python3
"""Eight logical controllers, one real stdio native backend, one disposable home.

No real TUI integration/model/quota proof is implied. Refused full transfers are
an intentional safety result. No existing daemon/account/process is contacted.
"""
import concurrent.futures
import json
import os
from pathlib import Path
import resource
import shlex
import shutil
import sys
import tempfile
import time

from controller import Client, NativeRPC, Refused, Registry
from stdio_probe import environment


def until(predicate, seconds=5):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.02)
    raise AssertionError("experiment effect did not arrive before deadline")


def main():
    root = Path(tempfile.mkdtemp(prefix="relay-native-feasibility-", dir="/private/tmp"))
    env = environment(root)
    registry = Registry(root / "admission", env["CODEX_HOME"])
    native = None
    result = {"root": str(root), "scope": "REAL_CODEX_STDIO_WITH_LOGICAL_CLIENTS_NOT_TUIS",
              "checks": [], "full_parity": False, "version": "0.166.0"}
    started = time.monotonic()
    def check(name, condition):
        if not condition:
            raise AssertionError(name)
        result["checks"].append(name)
    try:
        native = NativeRPC(shutil.which("codex"), env)
        clients = [Client(registry, native, f"pane-{i}") for i in range(8)]
        projects = [root / "project-a", root / "project-b"]
        for project in projects:
            project.mkdir()
        def launch(pair):
            i, client = pair
            t0 = time.monotonic()
            binding = client.transition("thread/start", {"cwd": str(projects[i % 2]),
                "model": "research-model", "modelProvider": "research",
                "approvalPolicy": "never", "sandbox": "read-only"})
            return {**binding, "latency_ms": (time.monotonic() - t0) * 1000}
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            bindings = list(pool.map(launch, enumerate(clients)))
        result["initial_bindings"] = bindings
        check("eight exact distinct native thread IDs", len({x["thread"] for x in bindings}) == 8)
        heartbeat = root / "heartbeat.py"
        heartbeat.write_text("import os,sys,time\nfrom pathlib import Path\n"
            "if len(sys.argv)>2 and os.fork()==0:\n"
            " os.setsid()\n"
            " for fd in [0,1,2]:\n"
            "  null=os.open(os.devnull,os.O_RDWR);os.dup2(null,fd);os.close(null)\n"
            " end=time.monotonic()+9\n"
            " with Path(sys.argv[2]).open('a',buffering=1) as f:\n"
            "  while time.monotonic()<end:\n"
            "   f.write(str(time.monotonic_ns())+'\\n');time.sleep(.02)\n"
            " os._exit(0)\n"
            "end=time.monotonic()+9\n"
            "with Path(sys.argv[1]).open('a',buffering=1) as f:\n"
            " while time.monotonic()<end:\n"
            "  f.write(str(os.getpid())+' '+str(time.monotonic_ns())+'\\n');time.sleep(.02)\n")
        logs = [root / f"work-{i}.jsonl" for i in range(8)]
        def execute(pair):
            i, client = pair
            command = [sys.executable, str(heartbeat), str(logs[i])]
            if i == 0:
                command.append(str(root / "escaped-a.jsonl"))
            return client.write("thread/shellCommand", {"threadId": bindings[i]["thread"],
                "command": shlex.join(command), "timeoutMs": 12000},
                bindings[i]["sequence"])
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(execute, enumerate(clients)))
        def counts():
            return [len(p.read_text().splitlines()) if p.exists() else 0 for p in logs]
        until(lambda: min(counts()) >= 2)
        until(lambda: all(any(e.get("method") == "turn/started" for e in native.events_for(x["thread"])) for x in bindings))
        first = counts()
        time.sleep(.1)
        second = counts()
        check("eight native commands execute concurrently", all(b > a for a, b in zip(first, second)))

        before = len(native.calls)
        foreign = Client(registry, native, "cross-project-attacker")
        try:
            foreign.transition("thread/resume", {"threadId": bindings[0]["thread"], "excludeTurns": True})
            raise AssertionError("foreign native write ownership accepted")
        except Refused:
            check("foreign resume refused before native request", len(native.calls) == before)

        # Exercise new/fork/resume equivalents concurrently without interpreting rollouts.
        def navigate(pair):
            i, client = pair
            old = bindings[i]
            fork = client.transition("thread/fork", {"threadId": old["thread"], "excludeTurns": True})
            new = client.transition("thread/start", {"cwd": str(projects[(i + 1) % 2]),
                "model": "research-model", "modelProvider": "research",
                "approvalPolicy": "never", "sandbox": "read-only"})
            resumed = client.transition("thread/resume", {"threadId": old["thread"], "excludeTurns": True})
            try:
                client.write("thread/shellCommand", {"threadId": old["thread"], "command": "true"}, old["sequence"])
                raise AssertionError("stale selection accepted")
            except Refused:
                pass
            return {"pane": i, "fork": fork, "new": new, "resume": resumed}
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            transitions = list(pool.map(navigate, enumerate(clients)))
        result["transitions"] = transitions
        check("eight concurrent fork/new/resume transitions have exact responses",
              all(t["resume"]["thread"] == bindings[t["pane"]]["thread"]
                  and t["fork"]["thread"] != bindings[t["pane"]]["thread"] for t in transitions))
        check("background roots retain ownership after navigation", len(registry.snapshot()["owners"]) == 24)

        receipt = clients[0].fence_and_drain(bindings[0]["thread"])
        result["turn_drain_receipt"] = receipt
        first = counts()
        time.sleep(.2)
        second = counts()
        check("fencing and native interruption stop A while seven neighbors continue",
              first[0] == second[0] and all(b > a for a, b in zip(first[1:], second[1:])))
        escaped = root / "escaped-a.jsonl"
        until(lambda: escaped.exists())
        escape_before = escaped.read_text()
        time.sleep(.1)
        check("detached A child still mutates after native turn completion", escaped.read_text() != escape_before)
        try:
            clients[0].transfer(bindings[0]["thread"], "successor", receipt)
            raise AssertionError("turn drain promoted to execution quiescence")
        except Refused as error:
            result["handoff_refusal"] = str(error)
            check("full transfer refuses insufficient execution proof", True)
        before = len(native.calls)
        try:
            clients[0].write("thread/shellCommand", {"threadId": bindings[0]["thread"], "command": "true"}, clients[0].sequence)
            raise AssertionError("fenced owner wrote")
        except Refused:
            check("fenced epoch cannot forward another mutation", len(native.calls) == before)
        result["observation_counts"] = [len(native.events_for(b["thread"])) for b in bindings]
        check("scoped native observation covers all eight exact roots", min(result["observation_counts"]) > 0)
        check("no provider usage polling issued", not any(c["method"].startswith("account/") for c in native.calls))

        # Disprove a frontend-only admission fence using an unmediated native call.
        bypass = root / "unmediated-native-write"
        native.call("thread/shellCommand", {"threadId": bindings[0]["thread"],
            "command": shlex.join([sys.executable, "-c", f"from pathlib import Path;Path({str(bypass)!r}).write_text('research')"]),
            "timeoutMs": 2000})
        until(bypass.exists)
        check("unmodified native host accepts a mutation bypassing frontend fence", True)

        first = counts()
        native.close(kill=True)
        until(lambda: native.closed)
        time.sleep(.2)
        second = counts()
        result["crash_work_continued"] = [b > a for a, b in zip(first, second)]
        check("native backend crash alone does not drain surviving work", any(result["crash_work_continued"][1:]))
        try:
            clients[1].write("thread/shellCommand", {"threadId": bindings[1]["thread"], "command": "true"}, clients[1].sequence)
            raise AssertionError("write accepted after loss of native coverage")
        except Refused:
            check("disconnected native connection fails closed", True)
        restored = Registry(root / "admission", env["CODEX_HOME"])
        try:
            restored.claim("new-controller-after-crash", bindings[1]["thread"])
            raise AssertionError("crash reservation stolen")
        except Refused:
            check("durable reservation survives backend death and registry reopen", True)
        result["registry"] = restored.snapshot()
        result["native_methods"] = [x["method"] for x in native.calls]
    except BaseException as error:
        result["exception"] = repr(error)
    finally:
        if native:
            native.close()
        # Every test command is finite. Never discover/kill children by profile or pid guess.
        deadline = started + 12
        if time.monotonic() < deadline:
            time.sleep(deadline - time.monotonic())
        result["elapsed_s"] = time.monotonic() - started
        result["controller_cpu_s"] = resource.getrusage(resource.RUSAGE_SELF).ru_utime + resource.getrusage(resource.RUSAGE_SELF).ru_stime
        (root / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({k: result[k] for k in ["root", "checks", "full_parity"]} |
                         {"exception": result.get("exception")}, indent=2))
        if result.get("exception"):
            raise SystemExit(1)


if __name__ == "__main__":
    main()
