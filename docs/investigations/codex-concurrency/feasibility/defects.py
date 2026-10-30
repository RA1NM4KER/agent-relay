#!/usr/bin/env python3
"""Independent defect characterization; production files are never patched."""
import concurrent.futures
import contextlib
import fcntl
import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import threading
from controller import Registry
from stdio_probe import Server, environment


def main():
    root = Path(tempfile.mkdtemp(prefix="relay-defects-", dir="/private/tmp"))
    result = {"root": str(root), "database_trials": [], "historical_fork_trials": []}
    for case in ["cold-parallel", "warm-parallel", "short-bootstrap-gate"]:
        for trial in range(8):
            directory = root / f"{case}-{trial}"
            directory.mkdir()
            env = environment(directory)
            params = {"cwd": str(directory), "model": "research-model", "modelProvider": "research",
                      "approvalPolicy": "never", "sandbox": "read-only"}
            if case == "warm-parallel":
                warm = Server(shutil.which("codex"), env, "warm-bootstrap")
                warm.call("thread/start", params)
                warm.close()
            barrier = threading.Barrier(2)
            def endpoint(name):
                server = None
                try:
                    barrier.wait()
                    with (directory / "bootstrap.lock").open("a+") as lock:
                        if case == "short-bootstrap-gate":
                            fcntl.flock(lock, fcntl.LOCK_EX)
                        server = Server(shutil.which("codex"), env, name)
                        response = server.call("thread/start", params)
                    return {"endpoint": name, "ok": "result" in response, "error": response.get("error")}
                except Exception as error:
                    return {"endpoint": name, "ok": False, "exception": repr(error)}
                finally:
                    if server:
                        server.close()
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                outcomes = list(pool.map(endpoint, ["A", "B"]))
            checks = {}
            for database in (directory / "codex").glob("*.sqlite"):
                try:
                    with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as db:
                        checks[database.name] = db.execute("PRAGMA quick_check").fetchone()[0]
                except sqlite3.Error as error:
                    checks[database.name] = str(error)
            result["database_trials"].append({"case": case, "trial": trial, "outcomes": outcomes, "database_quick_checks": checks})
    binary = "/private/tmp/relay-codex-concurrency/target/release/relay-concurrency-watcher-probe"
    for trial in range(8):
        outcome = json.loads(subprocess.check_output([binary, "1", "0", "1", "idle", "production", "historical-fork"], text=True))
        result["historical_fork_trials"].append(outcome)
    result["summary"] = {case: {"failed_endpoints": sum(not o["ok"] for t in result["database_trials"] if t["case"] == case for o in t["outcomes"]),
                               "endpoints": 16} for case in ["cold-parallel", "warm-parallel", "short-bootstrap-gate"]}
    result["summary"]["historical_fork_misbindings"] = sum(any("old-fork" in e for e in t["outcomes"][0]["events"]) for t in result["historical_fork_trials"])
    (root / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps({"root": str(root), "summary": result["summary"]}, indent=2))


if __name__ == "__main__":
    main()
