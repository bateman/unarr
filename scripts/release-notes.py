#!/usr/bin/env python3
"""Print the entry of one version from CHANGELOG.md: what a GitHub Release says.

    scripts/release-notes.py 0.1.0            # the `## 0.1.0` entry, without its heading
    scripts/release-notes.py 0.1.0 --check    # exit 1 if there is none

The release job of CI runs it on the tag's version; a tag whose version has
no entry fails there, before anything is published.
"""

import argparse
import re
import sys
from pathlib import Path

CHANGELOG = Path(__file__).resolve().parent.parent / "CHANGELOG.md"


def entry(text: str, version: str) -> str | None:
    """The body under `## <version>` (an optional ` — date` after it), up to the next `## `."""
    heading = re.compile(rf"^## {re.escape(version)}(?:\s+[—-]\s+\S.*)?$", re.M)
    found = heading.search(text)
    if not found:
        return None
    rest = text[found.end():]
    following = re.search(r"^## ", rest, re.M)
    return (rest[: following.start()] if following else rest).strip() + "\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("version", help="the version, without the v of its tag")
    ap.add_argument("--check", action="store_true", help="only say whether the entry exists")
    args = ap.parse_args()
    body = entry(CHANGELOG.read_text(), args.version.removeprefix("v"))
    if body is None or not body.strip():
        print(f"no entry for {args.version} in {CHANGELOG}", file=sys.stderr)
        return 1
    if not args.check:
        sys.stdout.write(body)
    return 0


if __name__ == "__main__":
    sys.exit(main())
