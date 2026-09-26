#!/usr/bin/env python3
"""Update the gh-pages stable-version badge/link and hero meta-row to a newly published
GitHub Release tag.

Only ever run this against a stable release tag (`vX.Y.Z`): the rolling dogfood pre-release
and any dev/prerelease tag must never reach the public site. Callers (the
`pages-stable-version` workflow) are expected to enforce that; this script re-validates the
tag shape itself so it also fails closed when run by hand.

Touches exactly two spots in `index.html`:
- the nav badge (`<a class="badge" href=".../releases/tag/vX.Y.Z">vX.Y.Z</a>`);
- the hero meta-row's version span (`<div class="meta-row"><span>vX.Y.Z</span>...`).

Fails loudly (non-zero exit, no write) if either spot is missing or appears more than once,
rather than guessing or leaving a partial update. Idempotent: re-running with the tag already
in place makes no change.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

TAG_PATTERN = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+$")
VERSION = r"v[0-9]+\.[0-9]+\.[0-9]+"

BADGE_PATTERN = re.compile(
    r'(<a class="badge" href="https://github\.com/RA1NM4KER/agent-relay/releases/tag/)'
    r"(" + VERSION + r")"
    r'(">)'
    r"(" + VERSION + r")"
    r"(</a>)"
)

META_ROW_PATTERN = re.compile(
    r'(<div class="meta-row">\s*<span>)' r"(" + VERSION + r")" r"(</span>)"
)


class UpdateError(Exception):
    pass


def _substitute_one(pattern: re.Pattern[str], html: str, tag: str, label: str) -> str:
    matches = pattern.findall(html)
    if len(matches) != 1:
        raise UpdateError(
            f"expected exactly one {label} in index.html, found {len(matches)}"
        )

    def replace(match: re.Match[str]) -> str:
        groups = list(match.groups())
        # Every version-shaped capture group becomes the new tag; literal groups pass through.
        rebuilt = []
        for group in groups:
            rebuilt.append(tag if re.fullmatch(VERSION, group) else group)
        return "".join(rebuilt)

    return pattern.sub(replace, html, count=1)


def update(html: str, tag: str) -> str:
    if not TAG_PATTERN.match(tag):
        raise UpdateError(f"not a stable version tag (expected vX.Y.Z): {tag!r}")

    html = _substitute_one(BADGE_PATTERN, html, tag, "nav badge")
    html = _substitute_one(META_ROW_PATTERN, html, tag, "hero meta-row version span")
    return html


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("index_html", type=Path, help="path to gh-pages/index.html")
    parser.add_argument("tag", help="stable release tag, e.g. v0.4.2")
    args = parser.parse_args(argv)

    try:
        original = args.index_html.read_text(encoding="utf-8")
        updated = update(original, args.tag)
    except UpdateError as error:
        print(f"update-pages-stable-version: {error}", file=sys.stderr)
        return 1

    if updated == original:
        print(f"{args.index_html}: already at {args.tag}, no change")
        return 0

    args.index_html.write_text(updated, encoding="utf-8")
    print(f"{args.index_html}: updated to {args.tag}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
