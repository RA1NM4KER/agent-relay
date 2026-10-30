#!/usr/bin/env python3
"""Optional cgroup-v2 lab, operating ONLY on newly created research cgroups.

Requires an explicitly supplied empty/delegated parent on Linux. Never uses sudo,
existing cgroups, user sessions, or profile scans. Not a Codex executor integration.
On macOS/no delegation, records BLOCKED rather than claiming this experiment ran.
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
    args = parser.parse_args()
    result = {"scope": "OS_CONTAINMENT_ONLY_NOT_CODEX_INTEGRATION", "passed": False}
    if sys.platform != "linux" or args.delegated_parent is None:
        result.update(status="BLOCKED", reason="requires Linux and an explicitly delegated research cgroup parent")
        print(json.dumps(result, indent=2))
        return
    parent = args.delegated_parent.resolve(strict=True)
    if not (parent / "cgroup.procs").exists() or parent.stat().st_uid != os.getuid():
        raise ValueError("parent is not a cgroup delegated to this uid")
    root = Path(tempfile.mkdtemp(prefix="relay-containment-"))
    result["root"] = str(root)
    script = root / "job.py"
    script.write_text("import os,sys,time\nfrom pathlib import Path\n"
        "assert os.read(int(sys.argv[1]),1)==b'G'\n"
        "if os.fork()==0:\n"
        " os.setsid(); path=Path(sys.argv[2]+'.detached')\n"
        "else:path=Path(sys.argv[2])\n"
        "end=time.monotonic()+8\n"
        "with path.open('a',buffering=1) as f:\n"
        " while time.monotonic()<end:f.write(str(time.monotonic_ns())+'\\n');time.sleep(.02)\n")
    groups, processes = [], []
    try:
        for label in ["A", "B"]:
            group = parent / ("relay-research-" + uuid.uuid4().hex)
            group.mkdir()
            groups.append(group)
            if not (group / "cgroup.kill").exists():
                raise RuntimeError("delegation lacks cgroup.kill")
            read_fd, write_fd = os.pipe()
            try:
                child = subprocess.Popen([sys.executable, str(script), str(read_fd), str(root / label)],
                    pass_fds=(read_fd,), env={"PATH": "/usr/bin:/bin", "HOME": str(root)},
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                processes.append(child)
                (group / "cgroup.procs").write_text(str(child.pid))
                os.write(write_fd, b"G")
            finally:
                os.close(read_fd)
                os.close(write_fd)
        time.sleep(.4)
        assert all((root / (n + ".detached")).exists() for n in ["A", "B"])
        (groups[0] / "cgroup.kill").write_text("1")
        deadline = time.monotonic() + 3
        while "populated 1" in (groups[0] / "cgroup.events").read_text():
            if time.monotonic() > deadline:
                raise RuntimeError("A containment did not empty")
            time.sleep(.01)
        before = {n: (root / n).read_text() for n in ["A", "A.detached", "B", "B.detached"]}
        time.sleep(.3)
        after = {n: (root / n).read_text() for n in before}
        assert all(after[n] == before[n] for n in ["A", "A.detached"])
        assert all(after[n] != before[n] for n in ["B", "B.detached"])
        result.update(status="OBSERVED", passed=True, a_group_empty=True, b_continues=True)
    except BaseException as error:
        result.update(status="FAILED_OR_BLOCKED", reason=repr(error))
    finally:
        for group in groups:
            if (group / "cgroup.kill").exists():
                (group / "cgroup.kill").write_text("1")
        for child in processes:
            child.wait(timeout=9)
        for group in groups:
            try:
                group.rmdir()
            except OSError:
                pass
        (root / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
