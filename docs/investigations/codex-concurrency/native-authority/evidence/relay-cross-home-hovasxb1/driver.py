#!/usr/bin/env python3
"""Try an unprotected native broker against an exact enrolled rollout path."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

from check_native import Peer, environment, socket_quiet

parser = argparse.ArgumentParser()
parser.add_argument("--app-server", type=Path, required=True)
parser.add_argument("--source-root", type=Path, required=True)
parser.add_argument("--share-store", action="store_true")
args = parser.parse_args()
source = json.loads((args.source_root / "result.json").read_text())["bindings"]["A"]
rollouts = list((args.source_root / "codex/sessions").rglob(f"*{source}*.jsonl"))
assert len(rollouts) == 1
root = Path(tempfile.mkdtemp(prefix="relay-cross-home-", dir="/private/tmp"))
shutil.copyfile(__file__, root / "driver.py")
env = environment(root)
if args.share_store:
    config = root / "codex/config.toml"
    config.write_text("sqlite_home = " + json.dumps(str(args.source_root / "codex")) + "\n" + config.read_text())
sock = root / "native.sock"
result = {"root": str(root), "sourceRoot": str(args.source_root),
          "sourceThread": source, "rollout": str(rollouts[0]),
          "handoffPermitted": False, "fullParity": False,
          "sharedNativeStore": args.share_store}
peer = None
with (root / "host.stderr").open("wb") as output:
    process = subprocess.Popen([str(args.app_server), "--listen", f"unix://{sock}"],
                               env=env, cwd=root, stdin=subprocess.DEVNULL,
                               stdout=output, stderr=output, start_new_session=True)
    try:
        deadline = time.monotonic() + 25
        while socket_quiet(sock):
            if process.poll() is not None:
                raise AssertionError(f"native host exited {process.returncode}")
            if time.monotonic() > deadline:
                raise AssertionError("native endpoint deadline")
            time.sleep(.05)
        try:
            peer = Peer(sock, env["CODEX_HOME"])
            reply = peer.call("thread/resume", {"threadId": source, "path": str(rollouts[0]),
                                                "cwd": str(root), "model": "research-model",
                                                "modelProvider": "research"})
            result.update(boundaryBypassed=reply["thread"]["id"] == source,
                          nativeReply=reply, rejected=False)
        except Exception as error:
            result.update(rejected=True, rejection=str(error), boundaryBypassed=False)
    except BaseException as error:
        result.update(error=repr(error))
    finally:
        if peer:
            peer.close()
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
        result["cleanup"] = {"pid": process.pid, "exitCode": process.returncode,
                             "socketQuiet": socket_quiet(sock)}
(root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result, indent=2))
raise SystemExit(0 if result.get("rejected") and "RELAY_NATIVE_AUTHORITY" in result.get("rejection", "") else 1)
