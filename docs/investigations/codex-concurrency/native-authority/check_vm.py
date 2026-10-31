#!/usr/bin/env python3
"""Bounded two-kernel isolation experiment; never authorizes a Relay handoff."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manager", required=True)
    parser.add_argument("--images", type=Path, required=True)
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix="relay-kernel-realms-"))
    shutil.copyfile(__file__, root / "driver.py")
    processes, streams, checks = {}, {}, []
    result = {"root": str(root), "checks": checks,
              "nativeExecutionIntegrated": False, "handoffPermitted": False}

    def state(label):
        try:
            return json.loads((root / f"{label}.state.json").read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            return {}

    def console(label):
        return (root / f"{label}.console.log").read_text(errors="replace")

    def wait(predicate, description, timeout=12):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                checks.append(description)
                return
            time.sleep(.05)
        raise AssertionError(description)

    try:
        for label in ("A", "B"):
            streams[label] = (root / f"{label}.console.log").open("wb")
            processes[label] = subprocess.Popen(
                [args.manager, str(args.images / "kernel"),
                 str(args.images / "realm-initramfs"), label,
                 str(root / f"{label}.state.json")],
                stdin=subprocess.DEVNULL, stdout=streams[label],
                stderr=subprocess.STDOUT)
        wait(lambda: all(state(x).get("vmRunning") for x in processes),
             "two independent VZ kernels running")
        assert state("A")["realmID"] != state("B")["realmID"]
        checks.append("distinct host-generated realm identities")
        for label in processes:
            wait(lambda label=label: f"RELAY_GUEST {label} escaped_naive_cgroup" in console(label)
                 and f"RELAY_GUEST {label} cgroup_kill_requested" in console(label)
                 and f"RELAY_GUEST {label} detached 10" in console(label),
                 f"{label} privileged detached guest survives naive cgroup kill")
            assert all(state(label)[key] == 0 for key in
                       ("networkDevices", "sharedDirectories", "socketDevices", "storageDevices"))
            checks.append(f"{label} has no external execution or filesystem devices")
        processes["A"].terminate()
        assert processes["A"].wait(timeout=8) == 0
        assert state("A")["vmStopped"] and state("A")["admissionClosed"]
        assert state("A")["status"] == "STOPPED"
        checks.append("forced exact-VM A stop completes with trusted VZ stopped state")
        before = console("B").count("RELAY_GUEST B detached ")
        wait(lambda: state("B").get("vmRunning") and processes["B"].poll() is None
             and console("B").count("RELAY_GUEST B detached ") >= before + 5,
             "B detached execution continues after A kernel stop", timeout=3)
        before_a = len(console("A"))
        time.sleep(.3)
        assert len(console("A")) == before_a
        checks.append("A console remains closed after exact manager exit (supporting evidence only)")
        processes["B"].terminate()
        assert processes["B"].wait(timeout=8) == 0
        assert state("B")["vmStopped"] and state("B")["admissionClosed"]
        checks.append("B independently stops with trusted VZ stopped state")
        result["passed"] = True
    except BaseException as error:
        result.update(passed=False, error=repr(error))
    finally:
        cleanup = {}
        for label, process in processes.items():
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
            cleanup[label] = {"pid": process.pid, "exitCode": process.returncode,
                              "state": state(label)}
            streams[label].close()
        result["cleanup"] = cleanup
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
    return 0 if result.get("passed") else 1


if __name__ == "__main__":
    raise SystemExit(main())
