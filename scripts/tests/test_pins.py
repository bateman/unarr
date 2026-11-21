"""The pinned originals are written in one place, upstream/<app>.pin (issue #9).

Fails when a pinned value (the version, the build time, the tarball's
SHA-256) is written in a tracked file that is not the pin, so that a new pin
is one file and nothing is left behind. What may hold such a value, and why:

- upstream/<app>.pin, the source;
- upstream/<app>.lock, the triage's cursor: its release is where the port
  stands, not a copy of the pin, and test_upstream_triage.py fails while
  the two differ, as they do on a port branch that is not finished;
- tests/fixtures/, the recordings: they hold what the original said when it
  was recorded and change only when recorded again;
- Markdown (docs/, CHANGELOG.md, READMEs): prose and history, which says
  which release something was measured or recorded on;
- scripts/tests/test_upstream_triage.py: a made-up upstream history whose
  tags happen to be real version numbers;
- comment lines, which name the release a behaviour was recorded on;
- backup file names (`<app>_backup_v<version>_<stamp>.zip`), which are data:
  any version goes there.

Run with `make test-scripts` (part of `make check`). Needs a git checkout,
whose tracked files it reads.
"""

from __future__ import annotations

import re
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
APPS = ("sonarr", "radarr")
# The keys whose values are distinctive enough to search for: a migration
# level is a small number found everywhere.
SEARCHED = ("version", "build_time", "sha256")

EXEMPT_PATHS = {f"upstream/{app}.{ext}" for app in APPS for ext in ("pin", "lock")} | {
    "scripts/tests/test_upstream_triage.py",
}
EXEMPT_PREFIXES = ("tests/fixtures/",)
EXEMPT_SUFFIXES = (".md",)
# The comment marker of each kind of file whose comments are skipped.
COMMENTS = {".rs": "//", ".sh": "#", ".py": "#", ".yml": "#", ".yaml": "#", ".toml": "#", "Makefile": "#",
            "Dockerfile": "#"}


def read_pin(path: Path) -> dict[str, str]:
    values = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            key, _, value = line.partition("=")
            values[key.strip()] = value.strip()
    return values


def tracked_files() -> list[str]:
    # Tracked, and new files not ignored: a copy is found before it is committed.
    done = subprocess.run(["git", "-C", str(REPO), "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
                          capture_output=True)
    if done.returncode != 0:
        raise unittest.SkipTest("not a git checkout")
    return [p for p in done.stdout.decode().split("\0") if p]


def exempt(path: str) -> bool:
    return path in EXEMPT_PATHS or path.startswith(EXEMPT_PREFIXES) or path.endswith(EXEMPT_SUFFIXES)


def copies(path: str, text: str, needles: dict[str, str]) -> list[str]:
    """The lines of `text` that write one of the needles outside a comment
    and outside a backup file name."""
    name = Path(path).name
    marker = COMMENTS.get(Path(path).suffix) or COMMENTS.get(name)
    found = []
    for n, line in enumerate(text.splitlines(), 1):
        if marker and line.lstrip().startswith(marker):
            continue
        for what, needle in needles.items():
            for m in re.finditer(re.escape(needle), line):
                if line[:m.start()].endswith("_backup_v"):
                    continue
                found.append(f"{path}:{n}: {what} {needle}: {line.strip()[:120]}")
    return found


class OneSourcePerPin(unittest.TestCase):
    def setUp(self):
        self.needles = {}
        for app in APPS:
            pin = read_pin(REPO / "upstream" / f"{app}.pin")
            for key in SEARCHED:
                self.assertIn(key, pin, f"upstream/{app}.pin")
                self.needles[f"{app} {key}"] = pin[key]

    def test_no_pinned_value_is_written_outside_its_pin(self):
        found = []
        for path in tracked_files():
            if exempt(path):
                continue
            try:
                text = (REPO / path).read_text(encoding="utf-8")
            except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
                continue
            found += copies(path, text, self.needles)
        self.assertEqual(found, [], "read the value from upstream/<app>.pin instead (unarr_core::pin in Rust):\n"
                         + "\n".join(found))

    def test_a_copy_is_found(self):
        version = self.needles["sonarr version"]
        self.assertEqual(len(copies("crates/x/src/a.rs", f'const V: &str = "{version}";', self.needles)), 1)
        self.assertEqual(len(copies("scripts/x.sh", f'V="${{V:-{version}}}"', self.needles)), 1)
        self.assertEqual(len(copies("x.yml", f"key: v{version}", self.needles)), 1)

    def test_comments_and_backup_names_are_not_copies(self):
        version = self.needles["radarr version"]
        self.assertEqual(copies("crates/x/src/a.rs", f"    /// recorded on Radarr {version}", self.needles), [])
        self.assertEqual(copies("scripts/x.py", f"  # recorded on Radarr {version}", self.needles), [])
        self.assertEqual(copies("crates/x/src/a.rs", f'let n = "radarr_backup_v{version}_2026.zip";',
                                self.needles), [])


if __name__ == "__main__":
    unittest.main()
