#!/usr/bin/env python3
"""Measure unchanged production RolloutWatcher, including setup/cleanup in CPU totals.

Build watcher-probe first as documented in README.md. This is a local synthetic-file
baseline, not a measured production implementation of any proposed architecture.
"""
import json
from pathlib import Path
import resource
import subprocess
import sys
import os
import time

binary = "/private/tmp/relay-codex-concurrency/target/release/relay-concurrency-watcher-probe"
results = []
mode = sys.argv[1] if len(sys.argv) > 1 else "production"
for history in [0, 1000]:
    for watchers in [1, 2, 8]:
        for repetition in range(3):
            start = time.monotonic()
            child = subprocess.Popen([binary, str(watchers), str(history), "3", "idle", mode],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            _, status, usage = os.wait4(child.pid, 0)
            child.returncode = os.waitstatus_to_exitcode(status)
            stdout, stderr = child.communicate()
            if child.returncode:
                raise RuntimeError(stderr)
            elapsed = time.monotonic() - start
            result = json.loads(stdout)
            result.update(repetition=repetition, wall_s=elapsed,
                          cpu_s=usage.ru_utime + usage.ru_stime,
                          maxrss=usage.ru_maxrss,
                          maxrss_units="bytes" if sys.platform == "darwin" else "KiB")
            results.append(result)
            print(json.dumps(result), flush=True)
Path(f"/private/tmp/relay-codex-concurrency/watcher-{mode}-measurements.json").write_text(json.dumps(results, indent=2))
