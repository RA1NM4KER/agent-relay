#!/usr/bin/env python3
"""Repeat concurrent first-use database initialization in disposable Codex homes."""
import concurrent.futures
import json
from pathlib import Path
import shutil
import tempfile
from stdio_probe import Server, environment


root = Path(tempfile.mkdtemp(prefix="relay-bootstrap-", dir="/private/tmp"))
results = []
for i in range(10):
    trial = root / str(i)
    trial.mkdir()
    env = environment(trial)
    def start(name):
        server = None
        try:
            server = Server(shutil.which("codex"), env, name)
            response = server.call("thread/start", {"cwd": str(trial), "model": "research-model",
                "modelProvider": "research", "approvalPolicy": "never", "sandbox": "read-only"})
            return {"name": name, "response": response}
        except Exception as error:
            return {"name": name, "exception": repr(error)}
        finally:
            if server:
                server.close()
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        results.append({"trial": i, "outcomes": list(pool.map(start, ["A", "B"]))})
(root / "results.json").write_text(json.dumps(results, indent=2))
print(json.dumps({"root": str(root), "trials": [{"trial": x["trial"], "outcomes":
    [{"name": v["name"], "exception": v.get("exception"), "error": v.get("response", {}).get("error")}
     for v in x["outcomes"]]} for x in results]}, indent=2))
