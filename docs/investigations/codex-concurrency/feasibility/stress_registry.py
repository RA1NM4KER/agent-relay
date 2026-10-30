#!/usr/bin/env python3
"""Multiprocess registry proof and crash cases. No native execution proof implied."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from controller import Registry, Refused


def worker():
    root, owner, action = Path(sys.argv[2]), sys.argv[3], sys.argv[4]
    registry = Registry(root / "registry", root / "home")
    try:
        if action == "claim":
            registry.claim(owner, sys.argv[5])
        else:
            registry.intent(owner, "thread/start")
            os._exit(91)
        print("claimed")
    except Refused:
        print("refused")


def main():
    root = Path(tempfile.mkdtemp(prefix="relay-registry-stress-", dir="/private/tmp"))
    (root / "home").mkdir()
    registry = Registry(root / "registry", root / "home")
    results = {"root": str(root), "scope": "REGISTRY_ONLY", "rounds": []}
    def claim(pair):
        round_id, client = pair
        return subprocess.check_output([sys.executable, __file__, "worker", str(root), f"r{round_id}-p{client}", "claim", f"conversation-{round_id}"], text=True).strip()
    for round_id in range(8):
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            answers = list(pool.map(claim, [(round_id, i) for i in range(8)]))
        assert answers.count("claimed") == 1
        results["rounds"].append({"round": round_id, "winners": answers.count("claimed"), "refusals": answers.count("refused")})
    alias = root / "alias"
    alias.symlink_to(root / "home", target_is_directory=True)
    aliased = Registry(root / "registry", alias)
    assert registry.home == aliased.home
    results["canonical_home_alias_same_identity"] = True
    epoch = registry.claim("epoch-owner", "epoch-thread")
    registry.fence("epoch-owner", "epoch-thread", epoch)
    try:
        registry.authorize("epoch-owner", "epoch-thread", "stale-epoch")
        raise AssertionError("stale epoch accepted")
    except Refused:
        results["stale_epoch_refused"] = True
    child = subprocess.run([sys.executable, __file__, "worker", str(root), "crash-owner", "crash-intent"], check=False)
    assert child.returncode == 91
    reopened = Registry(root / "registry", root / "home")
    assert any(x[1] == "crash-owner" and x[3] == "PENDING" for x in reopened.snapshot()["intents"])
    try:
        reopened.claim("would-be-owner", "unknown-native-id")
        raise AssertionError("unknown orphan ignored")
    except Refused:
        results["pending_creation_blocks_unregistered_adoption_after_process_death"] = True
    (root / "result.json").write_text(json.dumps(results, indent=2))
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    worker() if len(sys.argv) > 1 and sys.argv[1] == "worker" else main()
