#!/usr/bin/env python3
"""Validate the held-response fixture against actual pinned native stdio.

This is a fixture compatibility/cancellation check, not shared-TUI certification.
No command/tool is requested and no credentialed provider is contacted.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import time
from controller import NativeRPC
from holding_provider import HoldingProvider
from stdio_probe import environment


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codex", type=Path, required=True)
    args = parser.parse_args()
    executable = args.codex.resolve(strict=True)
    if "target" not in executable.parts:
        raise ValueError("explicit disposable source build required")
    root = Path(tempfile.mkdtemp(prefix="relay-holding-native-", dir="/private/tmp"))
    env = environment(root)
    native = None
    provider = None
    result = {"root": str(root), "scope": "NATIVE_STDIO_FIXTURE_COMPATIBILITY", "full_parity": False}
    try:
        version = subprocess.check_output([str(executable), "--version"], env=env, text=True, timeout=45).strip()
        if version != "codex-cli 0.166.0":
            raise AssertionError("wrong native version")
        provider = HoldingProvider(root / "provider.events.jsonl")
        config = root / "codex/config.toml"
        config.write_text(config.read_text().replace("http://127.0.0.1:1/v1", provider.url))
        native = NativeRPC(str(executable), env)
        thread = native.call("thread/start", {"cwd": str(root), "model": "research-model",
            "modelProvider": "research", "approvalPolicy": "never", "sandbox": "read-only"})["thread"]["id"]
        result["thread"] = thread
        turn = native.call("turn/start", {"threadId": thread,
            "input": [{"type": "text", "text": "Held local fixture cancellation check.", "text_elements": []}]})["turn"]["id"]
        result["turn"] = turn
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if thread in provider.active({thread}):
                break
            time.sleep(.02)
        else:
            raise AssertionError("actual native request did not open a held response")
        result["interrupt_ack"] = native.call("turn/interrupt", {"threadId": thread, "turnId": turn})
        # Send one interruption. drain_turns() would issue a second interrupt
        # before the first one's completion notification necessarily arrives.
        deadline = time.monotonic() + 10
        completed = []
        while time.monotonic() < deadline:
            completed = [e["params"]["turn"] for e in native.events_for(thread)
                         if e.get("method") == "turn/completed" and e["params"]["turn"]["id"] == turn]
            if completed:
                break
            time.sleep(.02)
        if len(completed) != 1 or completed[0]["status"] != "interrupted":
            raise AssertionError("native did not report the exact turn interrupted")
        deadline = time.monotonic() + 5
        while not provider.disconnected({thread}) and time.monotonic() < deadline:
            time.sleep(.02)
        if not provider.disconnected({thread}):
            raise AssertionError("native cancellation did not disconnect its held response")
        result.update(status="PASSED", thread=thread, turn=turn, turn_status=completed[0]["status"],
                      receipt={"kind": "TURN_DRAIN_ONLY", "thread": thread, "turns": [turn],
                               "execution_quiescent": False})
    except BaseException as error:
        result["exception"] = repr(error)
    finally:
        if native:
            result["rpc_methods"] = [c["method"] for c in native.calls]
            result["turn_events"] = [{"method": e.get("method"),
                "thread": e.get("params", {}).get("threadId"),
                "turn": {k: e.get("params", {}).get("turn", {}).get(k) for k in ["id", "status"]}}
                for e in native.events_for(result.get("thread")) if e.get("method", "").startswith("turn/")]
            native.close()
            result["native_cleanup_exit"] = native.base.child.returncode
            (root / "native.stderr").write_text("".join(native.base.stderr)[-1024 * 1024:])
        if provider:
            provider.close()
            result["provider_requests"] = provider.snapshot()
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
    if result.get("exception"):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
