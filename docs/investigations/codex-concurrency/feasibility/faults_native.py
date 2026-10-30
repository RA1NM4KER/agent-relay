#!/usr/bin/env python3
"""Try to falsify atomic admission and lost-response handling on a real backend."""
import concurrent.futures
import contextlib
import json
from pathlib import Path
import shutil
import tempfile
import sys
import threading
import time
from controller import Client, NativeRPC, Refused, Registry
from stdio_probe import environment


def main():
    root = Path(tempfile.mkdtemp(prefix="relay-native-faults-", dir="/private/tmp"))
    env = environment(root)
    registry = Registry(root / "admission", env["CODEX_HOME"])
    if "--unsafe-research-mutant" in sys.argv:
        registry.admission_lock = lambda _thread: contextlib.nullcontext()
    native = None
    result = {"root": str(root), "scope": "REAL_NATIVE_REQUESTS_WITH_CONTROLLED_FAILURE_INJECTION", "checks": [],
              "unsafe_mutant": "--unsafe-research-mutant" in sys.argv}
    def check(name, value):
        if not value:
            raise AssertionError(name)
        result["checks"].append(name)
    try:
        native = NativeRPC(shutil.which("codex"), env)
        a, b = Client(registry, native, "A"), Client(registry, native, "B")
        params = {"cwd": str(root), "model": "research-model", "modelProvider": "research", "approvalPolicy": "never", "sandbox": "read-only"}
        ta, tb = a.transition("thread/start", params), b.transition("thread/start", params)
        prepared, release = threading.Event(), threading.Event()
        class DelayForward:
            @property
            def closed(self):
                return native.closed
            def call(self, method, values):
                prepared.set()
                if not release.wait(3):
                    raise TimeoutError("test failed to release admitted RPC")
                return native.call(method, values)
        a.native = DelayForward()
        duplicate = Client(registry, native, "A")
        duplicate.epochs = dict(a.epochs)
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            writer = pool.submit(a.write, "thread/shellCommand", {"threadId": ta["thread"], "command": "true", "timeoutMs": 2000}, ta["sequence"])
            assert prepared.wait(2)
            fencer = pool.submit(duplicate.fence_and_drain, ta["thread"])
            time.sleep(.05)
            check("concurrent fence cannot pass an admitted but not forwarded mutation", not fencer.done())
            neighbor = pool.submit(b.write, "thread/shellCommand", {"threadId": tb["thread"], "command": "true", "timeoutMs": 2000}, tb["sequence"])
            neighbor.result(timeout=2)
            check("per-thread admission lock does not block neighbor", True)
            release.set()
            writer.result(timeout=3)
            receipt = fencer.result(timeout=5)
            check("ordered admitted mutation and fence produce only a turn-drain receipt", receipt["kind"] == "TURN_DRAIN_ONLY")
        class LoseReply:
            @property
            def closed(self):
                return native.closed
            def call(self, method, values):
                reply = native.call(method, values)
                result["native_created_before_reply_loss"] = reply["thread"]["id"]
                raise ConnectionError("injected loss after native creation, before binding publication")
        lost = Client(registry, LoseReply(), "lost-reply")
        try:
            lost.transition("thread/start", params)
            raise AssertionError("lost reply allowed")
        except ConnectionError:
            check("lost native reply quarantines its controller", lost.quarantined)
        try:
            registry.claim("stranger", result["native_created_before_reply_loss"])
            raise AssertionError("unbound created thread adopted after reply loss")
        except Refused:
            check("unknown native target is not adopted from broadcast or recency", True)
        b.write("thread/shellCommand", {"threadId": tb["thread"], "command": "true", "timeoutMs": 2000}, tb["sequence"])
        check("neighbor existing work survives another controller's lost reply", True)
        result["registry"] = registry.snapshot()
    except BaseException as error:
        result["exception"] = repr(error)
    finally:
        if native:
            native.close()
        (root / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({"root": str(root), "checks": result["checks"], "exception": result.get("exception")}, indent=2))
        if result.get("exception"):
            raise SystemExit(1)


if __name__ == "__main__":
    main()
