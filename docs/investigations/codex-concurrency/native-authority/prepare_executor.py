#!/usr/bin/env python3
"""Reuse verified retained media; create only the distinct executor initramfs."""
import gzip
import hashlib
from pathlib import Path

from prepare_vm import ROOT, cpio_entry

base = (ROOT / "base-initramfs").read_bytes()
assert hashlib.sha256(base).hexdigest() == "aee79b02c16c87fb0b6180a4d652d51b0115d2f2c8a69c20f8e917ff602fe26e"
script = (Path(__file__).parent / "guest_exec_init.sh").read_bytes()
entries = [cpio_entry("init", script, 0o100755, 0x710001),
           cpio_entry("dev/console", b"", 0o020600, 0x710002, 5, 1),
           cpio_entry("dev/null", b"", 0o020666, 0x710003, 1, 3),
           cpio_entry("TRAILER!!!", b"", 0, 0x710004)]
data = base + gzip.compress(b"".join(entries), mtime=0)
target = ROOT / "executor-initramfs"
if target.exists():
    assert target.read_bytes() == data, "existing executor image differs; preserve it"
else:
    target.write_bytes(data)
print({"path": str(target), "bytes": len(data),
       "sha256": hashlib.sha256(data).hexdigest(), "downloads": 0})
