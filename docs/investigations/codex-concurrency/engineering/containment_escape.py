#!/usr/bin/env python3
"""Adversarial supplement to the cgroup kill test, NOT a containment adapter.

Only moves newly spawned probe processes. Explicit delegation required. A child
tries to move itself to the supplied parent before the manager kills its scope.
If it can, cgroup.kill on that scope cannot establish executor quiescence.
On macOS this records BLOCKED, exits 2, and never substitutes process-group kill.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--delegated-parent", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = {"scope": "CGROUP_ESCAPE_ADVERSARY_NOT_NATIVE_EXECUTOR", "full_parity": False}
    code = 2
    group, child = None, None
    try:
        if sys.platform != "linux" or args.delegated_parent is None:
            result.update(status="BLOCKED", reason="Linux and explicit research cgroup delegation required")
            return
        parent = args.delegated_parent.resolve(strict=True)
        if not (parent / "cgroup.procs").is_file() or parent.stat().st_uid != os.getuid():
            raise ValueError("parent must be explicitly delegated to this uid")
        root = Path(tempfile.mkdtemp(prefix="relay-cgroup-escape-"))
        result["root"] = str(root)
        group = parent / ("relay-escape-probe-" + uuid.uuid4().hex)
        group.mkdir()
        if not (group / "cgroup.kill").exists():
            raise ValueError("new scope lacks cgroup.kill")
        script = root / "child.py"
        script.write_text("import json,os,sys,time\nfrom pathlib import Path\n"
            "assert os.read(int(sys.argv[1]),1)==b'G'\n"
            "os.setsid()\n"
            "try:\n"
            " Path(sys.argv[2]).write_text(str(os.getpid()))\n"
            " outcome={'escaped':True}\n"
            "except OSError as e:outcome={'escaped':False,'errno':e.errno}\n"
            "Path(sys.argv[3]).write_text(json.dumps(outcome))\n"
            "end=time.monotonic()+3\n"
            "with Path(sys.argv[4]).open('a',buffering=1) as f:\n"
            " while time.monotonic()<end:f.write('tick\\n');time.sleep(.02)\n")
        read_fd, write_fd = os.pipe()
        try:
            child = subprocess.Popen([sys.executable, str(script), str(read_fd),
                str(parent / "cgroup.procs"), str(root / "attempt.json"), str(root / "effects")],
                pass_fds=(read_fd,), cwd=root, env={"HOME": str(root), "PATH": "/usr/bin:/bin"},
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            (group / "cgroup.procs").write_text(str(child.pid))
            os.write(write_fd, b"G")
        finally:
            os.close(read_fd)
            os.close(write_fd)
        deadline = time.monotonic() + 2
        while not (root / "attempt.json").exists() or not (root / "effects").exists():
            if time.monotonic() > deadline:
                raise RuntimeError("child did not publish escape attempt")
            time.sleep(.01)
        result["attempt"] = json.loads((root / "attempt.json").read_text())
        (group / "cgroup.kill").write_text("1")
        before = (root / "effects").stat().st_size
        time.sleep(.15)
        result["writes_after_scope_kill"] = (root / "effects").stat().st_size > before
        if result["attempt"]["escaped"]:
            result["status"] = "FALSIFIED_DELEGATION_IS_EXECUTOR_AUTHORITY"
            code = 1
        elif result["attempt"].get("errno") in {1, 13, 30}:
            result["status"] = "ONE_ESCAPE_PATH_DENIED_NOT_FULL_CONTAINMENT"
            code = 0
        else:
            result["status"] = "INCONCLUSIVE_ESCAPE_ERROR"
    except BaseException as error:
        result.update(status="FAILED_OR_BLOCKED", reason=repr(error))
        code = 1
    finally:
        try:
            if group and (group / "cgroup.kill").exists():
                (group / "cgroup.kill").write_text("1")
            if child:
                # The only possibly escaped process is this bounded, exact child;
                # wait rather than scan/signal a guessed PID or a shared profile.
                child.wait(timeout=5)
            if group and group.exists():
                group.rmdir()
        except BaseException as error:
            result["cleanup_error"] = repr(error)
            code = 1
        args.output.write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
        # A blocked critical experiment is never a successful command.
        if code:
            raise SystemExit(code)


if __name__ == "__main__":
    main()
