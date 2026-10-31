#!/usr/bin/env python3
"""Apply the retained authority patch only to the exact disposable native lab."""
from pathlib import Path
import argparse
import shutil
import subprocess

PIN = "6ff1e1ca3b30fc8b914720681fcf5fe62e95f856"
ROOT = Path("/private/tmp/relay-native-build-20261030-9fw2zv7w/codex")
SOURCE = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--apply", action="store_true", help="mutate the verified disposable checkout")
    args = parser.parse_args()
    if ROOT.resolve() != ROOT or not ROOT.is_dir():
        raise SystemExit("exact retained disposable checkout required")
    pin = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if pin != PIN:
        raise SystemExit("native pin mismatch")
    for target in ("core/src/relay_authority.rs", "app-server/src/relay_authority.rs"):
        if (ROOT / "codex-rs" / target).exists():
            raise SystemExit("authority already installed; do not apply twice")
    subprocess.run(["git", "apply", "--check", str(SOURCE / "authority.patch")], cwd=ROOT, check=True)
    if args.apply:
        subprocess.run(["git", "apply", str(SOURCE / "authority.patch")], cwd=ROOT, check=True)
        for source, target in (("core_authority.rs", "core"), ("host_authority.rs", "app-server")):
            shutil.copyfile(SOURCE / source, ROOT / "codex-rs" / target / "src/relay_authority.rs")
        print("Applied research authority; production and prior native bridge untouched")
    else:
        print("Pinned disposable checkout accepts authority patch; no files changed")


if __name__ == "__main__":
    main()
