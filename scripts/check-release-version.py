#!/usr/bin/env python3
"""Refuse a stable release tag that disagrees with the workspace package version."""

import re
import sys
import tomllib
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 2 or re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", sys.argv[1]) is None:
        print("expected one stable tag: vX.Y.Z", file=sys.stderr)
        return 1
    manifest = Path(__file__).resolve().parent.parent / "Cargo.toml"
    version = tomllib.loads(manifest.read_text())["workspace"]["package"]["version"]
    if sys.argv[1] != f"v{version}":
        print(f"release tag {sys.argv[1]} disagrees with workspace version {version}", file=sys.stderr)
        return 1
    print(f"release tag and workspace version agree: v{version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
