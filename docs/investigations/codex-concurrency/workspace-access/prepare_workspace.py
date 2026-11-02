#!/usr/bin/env python3
"""Build a freestanding guest probe and deterministic initramfs in the persistent lab."""
from pathlib import Path
import gzip
import hashlib
import json
import os
import struct
import subprocess
import sys

REPO = Path(__file__).resolve().parents[4]
LAB = REPO / '.lab/workspace-access'
os.environ['RELAY_NATIVE_LAB'] = str(LAB)
sys.path.insert(0, str(Path(__file__).parent.parent / 'native-authority'))
from prepare_vm import cpio_entry

source = Path(__file__).parent
subprocess.run(['xcrun', 'clang', '-target', 'aarch64-none-elf', '-c', str(source/'mmap_probe.S'), '-o', str(LAB/'mmap_probe.o')], check=True)
obj = (LAB/'mmap_probe.o').read_bytes()
shoff = struct.unpack_from('<Q', obj, 40)[0]
entsize, count, strings = struct.unpack_from('<HHH', obj, 58)
sections = [struct.unpack_from('<IIQQQQIIQQ', obj, shoff+i*entsize) for i in range(count)]
table = sections[strings]
names = obj[table[4]:table[4]+table[5]]
assert not any(section[1] in (4, 9) and section[5] for section in sections), 'guest assembly contains unresolved relocations'
text = next(section for section in sections if names[section[0]:].split(b'\0')[0] == b'.text')
code = obj[text[4]:text[4]+text[5]]
ident = b'\x7fELF\x02\x01\x01' + b'\0'*9
header = ident + struct.pack('<HHIQQQIHHHHHH', 2, 183, 1, 0x401000, 64, 0, 0, 64, 56, 1, 0, 0, 0)
program = struct.pack('<IIQQQQQQ', 1, 5, 0, 0x400000, 0x400000, 4096+len(code), 4096+len(code), 4096)
probe = (header+program).ljust(4096, b'\0') + code
(LAB/'mmap-probe').write_bytes(probe)
base = (LAB/'vm-images/base-initramfs').read_bytes()
assert hashlib.sha256(base).hexdigest() == 'aee79b02c16c87fb0b6180a4d652d51b0115d2f2c8a69c20f8e917ff602fe26e'
entries = [cpio_entry('init', (source/'guest_workspace_init.sh').read_bytes(), 0o100755, 0x720001),
           cpio_entry('workspace-mmap-probe', probe, 0o100755, 0x720002),
           cpio_entry('dev/console', b'', 0o020600, 0x720003, 5, 1),
           cpio_entry('dev/null', b'', 0o020666, 0x720004, 1, 3),
           cpio_entry('TRAILER!!!', b'', 0, 0x720005)]
image = LAB/'vm-images/workspace-initramfs'
image.write_bytes(base + gzip.compress(b''.join(entries), mtime=0))
result = {'probeSHA256': hashlib.sha256(probe).hexdigest(), 'initramfsSHA256': hashlib.sha256(image.read_bytes()).hexdigest(),
          'probeBytes': len(probe), 'initramfsBytes': image.stat().st_size, 'downloads': 0,
          'probeABI': 'Linux aarch64 direct syscalls; no libc; relocation-free ELF64'}
(source/'evidence/workspace-image.json').write_text(json.dumps(result, indent=2)+'\n')
print(json.dumps(result))
