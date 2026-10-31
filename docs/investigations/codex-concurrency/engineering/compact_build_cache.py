#!/usr/bin/env python3
"""Lossless macOS filesystem compaction of this disposable lab's build cache.

Paths and decoded bytes remain usable. Only verified redundant backup/temp copies
are unlinked. No source, executable, profile, evidence, or Cargo fingerprint is
removed. Hold the existing Cargo lock; rollback the current file on interruption.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import re


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lab", type=Path, required=True)
    parser.add_argument("--run-id", default="")
    args = parser.parse_args()
    lab = args.lab.resolve(strict=True)
    if lab.parent != Path("/private/tmp") or not lab.name.startswith("relay-native-build-"):
        raise ValueError("expected the disposable native-build lab")
    # Identify the lab by its preserved verified baseline package. The mutable
    # debug CLI may now contain a newer, explicitly authorized native experiment.
    binary = lab / "target/research-package/bin/codex"
    expected = "dfc5fa0a2e9b3522665ad8d8db49dd98898f174f2306f8f6203ab3f2a2ee173e"
    if digest(binary) != expected:
        raise ValueError("unexpected native build; review before compacting its cache")
    cache = lab / "target/debug/deps"
    if args.run_id and not re.fullmatch(r"[a-zA-Z0-9-]+", args.run_id):
        raise ValueError("invalid distinct compaction run ID")
    suffix = "-" + args.run_id if args.run_id else ""
    work = lab / f"cache-compaction{suffix}-work"
    work.mkdir(exist_ok=False)
    journal = lab / f"cache-compaction{suffix}.jsonl"
    summary = {"free_before": shutil.disk_usage(lab).free, "files": 0,
               "allocated_bytes_saved": 0, "decoded_bytes_preserved": True}

    def interrupted(signum, frame):
        raise InterruptedError("compaction stopped; restoring current cache artifact")

    signal.signal(signal.SIGTERM, interrupted)
    with (lab / "target/debug/.cargo-lock").open("a+b") as guard, journal.open("x") as log:
        fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        files = sorted([*cache.glob("*.rlib"), *cache.glob("*.rmeta")],
                       key=lambda p: p.stat().st_size, reverse=True)
        for source in files:
            if shutil.disk_usage(lab).free >= 7 * 1024 ** 3:
                break
            before = source.stat()
            if source.is_symlink() or before.st_flags & stat.UF_COMPRESSED:
                continue
            if shutil.disk_usage(lab).free < 1536 * 1024 ** 2 + before.st_size:
                raise RuntimeError("insufficient compaction scratch space above reserve")
            original = digest(source)
            temporary = work / (source.name + ".compressed")
            backup = work / (source.name + ".backup")
            try:
                subprocess.run(["/usr/bin/ditto", "--hfsCompression", str(source), str(temporary)],
                               check=True, capture_output=True)
                after = temporary.stat()
                if not after.st_flags & stat.UF_COMPRESSED or after.st_blocks >= before.st_blocks:
                    temporary.unlink()
                    continue
                os.utime(temporary, ns=(before.st_atime_ns, before.st_mtime_ns))
                after = temporary.stat()
                if (digest(temporary) != original or after.st_size != before.st_size
                        or stat.S_IMODE(after.st_mode) != stat.S_IMODE(before.st_mode)
                        or (after.st_uid, after.st_gid, after.st_mtime_ns)
                        != (before.st_uid, before.st_gid, before.st_mtime_ns)):
                    raise RuntimeError("compressed copy changed artifact contents or metadata")
                current = source.stat()
                if (current.st_ino, current.st_size, current.st_mtime_ns) != (
                        before.st_ino, before.st_size, before.st_mtime_ns):
                    raise RuntimeError("cache artifact changed concurrently")
                os.link(source, backup)
                os.replace(temporary, source)
                if digest(source) != original or source.stat().st_mtime_ns != before.st_mtime_ns:
                    raise RuntimeError("published cache artifact failed verification")
                backup.unlink()  # redundant original; verified artifact retained at source
                saved = (before.st_blocks - source.stat().st_blocks) * 512
                row = {"file": source.name, "sha256": original, "logical_bytes": before.st_size,
                       "allocated_bytes_saved": saved, "mtime_ns_preserved": before.st_mtime_ns}
                log.write(json.dumps(row) + "\n")
                log.flush()
                summary["files"] += 1
                summary["allocated_bytes_saved"] += saved
            except BaseException:
                if backup.exists():
                    if source.exists() and source.samefile(backup):
                        backup.unlink()
                    else:
                        os.replace(backup, source)
                raise
            finally:
                if temporary.exists():
                    temporary.unlink()
    if digest(binary) != expected:
        raise RuntimeError("native executable changed unexpectedly")
    summary["free_after"] = shutil.disk_usage(lab).free
    (lab / f"cache-compaction{suffix}-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
