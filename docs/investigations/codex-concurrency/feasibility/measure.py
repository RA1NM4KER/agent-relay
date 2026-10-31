#!/usr/bin/env python3
"""Matched eight-thread/start cost comparison, not native-TUI or full-design cost.

Reports per-worker and backend memory separately. Peak worker RSS is not summed
process-tree RSS; transient callback processes are not included in that figure.
"""
import concurrent.futures
import json
import os
from pathlib import Path
import resource
import shutil
import subprocess
import sys
import tempfile
import time
from controller import Client, NativeRPC, Registry
from stdio_probe import environment


def worker(mode):
    root = Path(tempfile.mkdtemp(prefix="relay-feasibility-cost-", dir="/private/tmp"))
    env = environment(root)
    registry = Registry(root / "admission", env["CODEX_HOME"])
    native = NativeRPC(shutil.which("codex"), env)
    callback = root / "callback"
    callback.write_text(f'#!{sys.executable}\nimport sys\nsys.path.insert(0,{str(Path(__file__).resolve().parent)!r})\nfrom native_callback import main\nmain()\n')
    callback.chmod(0o700)
    params = {"cwd": str(root), "model": "research-model", "modelProvider": "research", "approvalPolicy": "never", "sandbox": "read-only"}
    # Same warmed native connection and database in each mode.
    native.call("thread/start", params)
    clients = [Client(registry, native, f"pane-{i}") for i in range(8)]
    def start(i):
        t0 = time.monotonic()
        if mode == "direct":
            thread = native.call("thread/start", params)["thread"]["id"]
        elif mode == "controller":
            thread = clients[i].transition("thread/start", params)["thread"]
        else:
            binary = "/private/tmp/relay-codex-feasibility/target/release/relay-native-bridge-probe"
            child = subprocess.Popen([binary, "thread/start", "-"], cwd=str(root),
                env={**env, "RELAY_RESEARCH_CALLBACK": str(callback), "RELAY_RESEARCH_CLIENT": f"pane-{i}",
                     "RELAY_RESEARCH_REGISTRY": str(root / "admission")},
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                assert json.loads(child.stdout.readline())["phase"] == "prepared"
                native_thread = native.call("thread/start", params)["thread"]
                thread = native_thread["id"]
                child.stdin.write(json.dumps({"outcome": "ok", "thread": thread, "ephemeral": native_thread["ephemeral"]}) + "\n")
                child.stdin.flush()
                assert json.loads(child.stdout.readline())["phase"] == "committed"
                child.wait(timeout=3)
                assert child.returncode == 0
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=3)
                for pipe in [child.stdin, child.stdout, child.stderr]:
                    pipe.close()
        return {"thread": thread, "latency_ms": (time.monotonic() - t0) * 1000}
    t0 = time.monotonic()
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            operations = list(pool.map(start, range(8)))
        elapsed = time.monotonic() - t0
        assert len({x["thread"] for x in operations}) == 8
        diagnostics = native.call("server/diagnostics", {})
        calls = list(native.calls)
    finally:
        native.close()
    own = resource.getrusage(resource.RUSAGE_SELF)
    children = resource.getrusage(resource.RUSAGE_CHILDREN)
    result = {"mode": mode, "root": str(root), "operations": operations, "wall_s_8_starts": elapsed,
        "worker_cpu_s": own.ru_utime + own.ru_stime, "children_cpu_s": children.ru_utime + children.ru_stime,
        "worker_peak_rss": own.ru_maxrss, "rss_units": "bytes" if sys.platform == "darwin" else "KiB",
        "backend_diagnostics": diagnostics, "actual_native_methods": [x["method"] for x in calls]}
    (root / "result.json").write_text(json.dumps(result, indent=2))
    return result


def main():
    if len(sys.argv) > 1:
        print(json.dumps(worker(sys.argv[1])))
        return
    results = []
    for repetition in range(3):
        for mode in ["direct", "controller", "bridge"]:
            completed = subprocess.run([sys.executable, __file__, mode], text=True, capture_output=True, check=True)
            result = json.loads(completed.stdout)
            result["repetition"] = repetition
            results.append(result)
    output = Path("/private/tmp/relay-codex-feasibility/costs.json")
    output.write_text(json.dumps(results, indent=2))
    archive = Path(tempfile.mkdtemp(prefix="cost-run-", dir="/private/tmp/relay-codex-feasibility")) / "costs.json"
    archive.write_text(json.dumps(results, indent=2))
    print(json.dumps({"output": str(output), "archive": str(archive), "runs": len(results)}))


if __name__ == "__main__":
    main()
