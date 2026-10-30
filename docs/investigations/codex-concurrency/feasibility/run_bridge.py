#!/usr/bin/env python3
"""Compile-tested insertion core, actual native backend, synthetic frontend caller.

This does not execute a rebuilt TUI. The source patch must be built/tested in a
listener-capable lab before its frontend coverage can be accepted.
"""
import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from controller import NativeRPC, Registry, Refused
from stdio_probe import environment


def main():
    root = Path(tempfile.mkdtemp(prefix="relay-bridge-feasibility-", dir="/private/tmp"))
    env = environment(root)
    registry = Registry(root / "admission", env["CODEX_HOME"])
    binary = Path("/private/tmp/relay-codex-feasibility/target/release/relay-native-bridge-probe")
    callback = root / "callback"
    source_dir = str(Path(__file__).resolve().parent)
    callback.write_text(f'#!{sys.executable}\nimport sys,json\nsys.path.insert(0,{source_dir!r})\nfrom native_callback import main\n'
                        'try:main()\nexcept BaseException as e:\n print(json.dumps({"allow":False,"reason":type(e).__name__}),flush=True);sys.exit(1)\n')
    callback.chmod(0o700)
    native = None
    result = {"root": str(root), "scope": "COMPILED_INSERTION_CORE_WITH_REAL_BACKEND_NOT_REBUILT_TUI",
              "checks": [], "full_parity": False, "operations": []}
    def check(name, passed):
        if not passed:
            raise AssertionError(name)
        result["checks"].append(name)
    def bridge(owner, method, params, selection=False, crash=False):
        process = subprocess.Popen([str(binary), method, params.get("threadId", "-")],
            env={**env, "RELAY_RESEARCH_CALLBACK": str(callback), "RELAY_RESEARCH_CLIENT": owner,
                 "RELAY_RESEARCH_REGISTRY": str(root / "admission")}, cwd=str(root),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        t0 = time.monotonic()
        try:
            line = process.stdout.readline()
            if not line:
                raise Refused(process.stderr.read())
            prepared = json.loads(line)
            if crash:
                process.kill()
                process.wait(timeout=3)
                return {"crashed_after_prepare": True}
            if selection:
                process.wait(timeout=3)
                return prepared
            if prepared["phase"] != "prepared":
                return prepared
            reply = native.call(method, params)
            thread = reply.get("thread", {}).get("id")
            process.stdin.write(json.dumps({"outcome": "ok", "thread": thread}) + "\n")
            process.stdin.flush()
            committed = process.stdout.readline()
            process.wait(timeout=3)
            if process.returncode or not committed:
                raise Refused(process.stderr.read())
            result["operations"].append({"owner": owner, "method": method, "thread": thread or params.get("threadId"),
                "latency_ms": (time.monotonic() - t0) * 1000})
            return reply
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=3)
            for pipe in [process.stdin, process.stdout, process.stderr]:
                pipe.close()
    try:
        native = NativeRPC(shutil.which("codex"), env)
        project = root / "project"
        project.mkdir()
        params = {"cwd": str(project), "model": "research-model", "modelProvider": "research",
                  "approvalPolicy": "never", "sandbox": "read-only"}
        def start(i):
            response = bridge(f"pane-{i}", "thread/start", params)
            thread = response["thread"]["id"]
            bridge(f"pane-{i}", "selection", {"threadId": thread}, selection=True)
            bridge(f"pane-{i}", "thread/shellCommand", {"threadId": thread, "command": "true", "timeoutMs": 2000})
            return thread
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            threads = list(pool.map(start, range(8)))
        check("eight callbacks reserve exact real native threads before granting frontend continuation", len(set(threads)) == 8)
        before = len(native.calls)
        try:
            bridge("foreign", "thread/resume", {"threadId": threads[0], "excludeTurns": True})
            raise AssertionError("foreign native resume allowed")
        except Refused:
            check("cross-client conflict is denied before native resume", before == len(native.calls))
        before = len(native.calls)
        try:
            bridge("pane-1", "process/spawn", {"command": ["false"]})
            raise AssertionError("unclassified mutation allowed")
        except Refused:
            check("unclassified native mutation refuses rather than bypasses gate", before == len(native.calls))
        # A misleading display event grants no ownership of the foreign thread.
        bridge("foreign", "selection", {"threadId": threads[0]}, selection=True)
        try:
            bridge("foreign", "thread/shellCommand", {"threadId": threads[0], "command": "true"})
            raise AssertionError("display selection conferred ownership")
        except Refused:
            check("display state is separate from write ownership", True)
        bridge("pane-0", "thread/start", params, crash=True)
        time.sleep(.1)
        pending = [i for i in registry.snapshot()["intents"] if i[1] == "pane-0" and i[3] in {"PENDING", "UNKNOWN"}]
        check("crashed frontend leaves durable unresolved creation", bool(pending))
        bridge("pane-1", "thread/shellCommand", {"threadId": threads[1], "command": "true", "timeoutMs": 2000})
        check("neighbor existing conversation still accepts a native command after A crash", True)
        try:
            bridge("pane-2", "thread/start", params)
            raise AssertionError("unresolved creation ignored")
        except Refused:
            check("unresolved creation blocks new adoption instead of guessing orphan identity", True)
        check("no provider polling", not any(c["method"].startswith("account/") for c in native.calls))
        result["registry"] = registry.snapshot()
    except BaseException as error:
        result["exception"] = repr(error)
    finally:
        if native:
            native.close()
        (root / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({"root": str(root), "checks": result["checks"],
            "exception": result.get("exception"), "full_parity": False}, indent=2))
        if result.get("exception"):
            raise SystemExit(1)


if __name__ == "__main__":
    main()
