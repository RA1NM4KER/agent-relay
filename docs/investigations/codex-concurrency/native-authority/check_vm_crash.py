#!/usr/bin/env python3
"""A killed realm manager cannot supply a trusted quiescence receipt."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument("--manager", required=True)
parser.add_argument("--images", type=Path, required=True)
args = parser.parse_args()
root = Path(tempfile.mkdtemp(prefix="relay-kernel-crash-"))
shutil.copyfile(__file__, root / "driver.py")
result = {"root": str(root), "handoffPermitted": False,
          "nativeExecutionIntegrated": False, "quiescence": "UNKNOWN"}
with (root / "console.log").open("wb") as output:
    process = subprocess.Popen([args.manager, str(args.images / "kernel"),
                                str(args.images / "realm-initramfs"), "A",
                                str(root / "state.json")], stdin=subprocess.DEVNULL,
                               stdout=output, stderr=subprocess.STDOUT)
    try:
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            if "RELAY_GUEST A detached 10" in (root / "console.log").read_text(errors="replace"):
                break
            time.sleep(.05)
        else:
            raise AssertionError("guest detached execution did not begin")
        process.kill()
        assert process.wait(timeout=5) == -9
        state = json.loads((root / "state.json").read_text())
        assert state["vmRunning"] and not state["vmStopped"]
        result.update(passed=True, lastTrustedState=state,
                      reason="manager died before trusted kernel stop acknowledgement; retain ownership reservation")
    except BaseException as error:
        result.update(passed=False, error=repr(error))
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
        result["cleanup"] = {"pid": process.pid, "exitCode": process.returncode,
                             "kernelQuiescenceProven": False}
(root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result, indent=2))
raise SystemExit(0 if result.get("passed") else 1)
