#!/usr/bin/env python3
"""Real native execution + replay journal + epochs; intentionally falsify scope.

Eight logical clients, not native TUIs. No credentials, inference or live homes.
All commands self-expire. Results archive in a fresh temp root, even on failure.
"""
import concurrent.futures
import json
from pathlib import Path
import shlex
import shutil
import sys
import tempfile
import time
from host_contract import HostContract
from controller import NativeRPC, Refused, Registry
from stdio_probe import environment


def main():
    root = Path(tempfile.mkdtemp(prefix="relay-host-contract-", dir="/private/tmp"))
    env = environment(root)
    result = {"root": str(root), "scope": "CONTRACT_SERVICE_WITH_REAL_NATIVE_BACKEND_NOT_NATIVE_HOST",
              "checks": [], "full_parity": False, "native_host_enforcement": False}
    native, host = None, None
    began = time.monotonic()

    def check(name, condition=True):
        if not condition:
            raise AssertionError(name)
        result["checks"].append(name)

    def until(predicate):
        deadline = time.monotonic() + 4
        while not predicate():
            if time.monotonic() >= deadline:
                raise AssertionError("effect deadline")
            time.sleep(.02)

    def refused(action, name):
        before = len(native.calls)
        try:
            action()
        except Refused:
            check(name, before == len(native.calls))
        else:
            raise AssertionError(name)

    try:
        native = NativeRPC(shutil.which("codex"), env)
        registry = Registry(root / "registry", env["CODEX_HOME"])
        host = HostContract(root / "host", registry, native)
        peers = [host.connect() for _ in range(8)]
        projects = [root / "project-a", root / "project-b"]
        for project in projects:
            project.mkdir()
        params = [{"cwd": str(projects[i % 2]), "model": "research-model",
                   "modelProvider": "research", "approvalPolicy": "never", "sandbox": "read-only"}
                  for i in range(8)]
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            bindings = list(pool.map(lambda i: host.submit(peers[i], "create", "thread/start", params[i]), range(8)))
        check("eight exact native IDs across two projects", len({b["thread"] for b in bindings}) == 8)
        result["bindings"] = bindings
        initial_bindings = bindings
        # Simulate a reply lost AFTER durable commit. Eight concurrent retries
        # must return one exact result without forwarding another native start.
        before = len(native.calls)
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            retries = list(pool.map(lambda _: host.submit(peers[0], "create", "thread/start", params[0]), range(8)))
        check("lost committed reply replays exact binding without duplicate creation",
              all(r == bindings[0] for r in retries) and len(native.calls) == before)
        refused(lambda: host.submit(peers[0], "create", "thread/start", params[1]), "token substitution rejected before native")
        refused(lambda: host.submit("forged", "x", "thread/start", params[0]), "unknown peer capability rejected before native")
        refused(lambda: HostContract(root / "host", registry, native), "second service owner refused")

        script = root / "work.py"
        script.write_text("import os,sys,time\nfrom pathlib import Path\n"
            "if len(sys.argv)>2 and os.fork()==0:\n"
            " os.setsid()\n"
            " for fd in [0,1,2]:\n"
            "  n=os.open(os.devnull,os.O_RDWR);os.dup2(n,fd);os.close(n)\n"
            " path=Path(sys.argv[2])\n"
            "else:path=Path(sys.argv[1])\n"
            "end=time.monotonic()+7\n"
            "with path.open('a',buffering=1) as f:\n"
            " while time.monotonic()<end:f.write('tick\\n');time.sleep(.02)\n")
        logs = [root / f"effect-{i}" for i in range(8)]
        escaped = root / "escaped"

        def work(i):
            argv = [sys.executable, str(script), str(logs[i])]
            if i == 0:
                argv.append(str(escaped))
            return host.submit(peers[i], "work", "thread/shellCommand", {
                "threadId": bindings[i]["thread"], "command": shlex.join(argv), "timeoutMs": 9000},
                bindings[i]["sequence"])

        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(work, range(8)))
        until(lambda: all(p.exists() for p in logs) and escaped.exists())
        counts = lambda: [p.stat().st_size for p in logs]
        a = counts()
        time.sleep(.15)
        check("eight commands progress simultaneously", all(y > x for x, y in zip(a, counts())))
        def navigate(i):
            original = bindings[i]
            fork = host.submit(peers[i], "fork", "thread/fork", {
                "threadId": original["thread"], "excludeTurns": True})
            new = host.submit(peers[i], "new", "thread/start", params[(i + 1) % 8])
            resumed = host.submit(peers[i], "resume", "thread/resume", {
                "threadId": original["thread"], "excludeTurns": True})
            assert len({fork["thread"], new["thread"], resumed["thread"]}) == 3
            assert resumed["thread"] == original["thread"]
            return resumed
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            bindings = list(pool.map(navigate, range(8)))
        check("eight concurrent fork/new/resume flows retain exact original IDs")
        check("24 foreground and background roots remain reserved", len(registry.snapshot()["owners"]) == 24)
        receipt = host.fence(peers[0], bindings[0]["thread"])
        result["receipt"] = receipt
        a = counts()
        e = escaped.stat().st_size
        time.sleep(.2)
        b = counts()
        check("A turn stops while seven neighbors progress", b[0] == a[0] and all(y > x for x, y in zip(a[1:], b[1:])))
        check("detached A still writes after turn drain", escaped.stat().st_size > e)
        refused(lambda: host.submit(peers[0], "late", "thread/shellCommand", {
            "threadId": bindings[0]["thread"], "command": "true"}, bindings[0]["sequence"]),
            "retired epoch cannot forward fresh work through service")
        refused(lambda: host.transfer(peers[0], bindings[0]["thread"], {**receipt, "execution_quiescent": True}),
                "caller forged quiescence cannot release reservation")

        # Expose the exact missing boundary: the actual native dispatcher is
        # unchanged. Direct calls are deliberately kept only by the test driver.
        bypass = root / "native-bypass"
        native.call("thread/shellCommand", {"threadId": bindings[0]["thread"],
            "command": shlex.join([sys.executable, "-c", f"from pathlib import Path;Path({str(bypass)!r}).touch()"]),
            "timeoutMs": 2000})
        until(bypass.exists)
        check("native dispatcher still bypasses service fence: integration REQUIRED")

        # Inject crash-window failure after native + registry application but
        # before operation-result commit. This is NOT a killed native host.
        def lose(_):
            raise RuntimeError("injected pre-commit result loss")
        try:
            host.submit(peers[2], "uncertain", "thread/start", params[2], after_apply=lose)
        except RuntimeError:
            pass
        check("pre-commit uncertainty durably recorded", host.status(peers[2], "uncertain")["state"] == "UNKNOWN")
        refused(lambda: host.submit(peers[2], "uncertain", "thread/start", params[2]),
                "uncertain creation never blindly retried")
        before = len(native.calls)
        # A neighbor with no uncertain operations remains usable.
        host.submit(peers[1], "neighbor", "thread/shellCommand", {
            "threadId": bindings[1]["thread"], "command": "true", "timeoutMs": 2000}, bindings[1]["sequence"])
        check("neighbor admission survives another client uncertainty", len(native.calls) == before + 1)
        check("events scoped to eight exact roots", all(native.events_for(b["thread"]) for b in bindings))
        check("no account polling", not any(c["method"].startswith("account/") for c in native.calls))
        old_generation = host.generation
        host.close()
        host = HostContract(root / "host", registry, native)
        check("reopen recovers committed exact result without native replay",
              host.status(peers[0], "create")["response"] == initial_bindings[0])
        check("reopen retains uncertain status", host.status(peers[2], "uncertain")["state"] == "UNKNOWN")
        refused(lambda: host.submit(peers[1], "after-restart", "thread/start", params[1]),
                "old generation cannot mutate after host restart")
        check("restart changes host generation", host.generation != old_generation)
        result["native_calls"] = native.calls
        result["elapsed_before_cleanup_s"] = time.monotonic() - began
    except BaseException as error:
        result["exception"] = repr(error)
    finally:
        if host:
            host.close()
        if native:
            native.close()
        # Bounded detached jobs self-expire; no PID discovery or broad killing.
        remaining = began + 12 - time.monotonic()
        if remaining > 0:
            time.sleep(remaining)
        (root / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({k: v for k, v in result.items() if k not in {"bindings", "native_calls"}}, indent=2))
        if result.get("exception"):
            raise SystemExit(1)


if __name__ == "__main__":
    main()
