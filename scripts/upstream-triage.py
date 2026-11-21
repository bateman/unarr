#!/usr/bin/env python3
"""Triage the upstream commits of a personality since the last one handled.

The deterministic first step of following Sonarr and Radarr (issue #9): read
the cursor in ``upstream/<app>.lock``, find the stable release to go to, list
the upstream commits between the cursor and that release, and classify each
one by the paths it touches, so a port starts from a table and not from a
raw diff::

    scripts/upstream-triage.py --app radarr --fetch
    scripts/upstream-triage.py --app radarr --target 7.0.0.10815 --format json

The target release (``choose_target``):

- the releases are the version tags (``vX.Y.Z.B``) reachable from the stable
  branch, ``git tag --merged``, never the tags by name alone: a clone also
  holds tags of other branches and of older projects;
- within the pin's major, the newest of them (a patch or minor jump);
- when the newest is in a later major, the first release of the next major,
  ``x.0.0`` (a major jump); the releases after it come with the next runs;
- none newer than the pin: nothing to do.

Git cannot tell a stable release from a pre-release. ``--releases`` takes
the JSON of the GitHub API (``gh api --paginate --slurp
repos/<owner>/<repo>/releases``), where it is available, and keeps only the
tags it marks neither draft nor prerelease; without it every tag on the
stable branch counts as stable. ``--target`` names the release outright.

The classes (``CLASSES``, by priority: a commit takes the highest class of
its paths and lists the others after it):

- ``schema``: a migration of ``*.Core/Datastore/Migration/``;
- ``routes``: ``*.Api.*/`` and ``*.Http/``;
- ``backend``: the rest of ``*.Core/``, and the server code beside it
  (``NzbDrone.Common``, ``.Host``, ``.SignalR``, ``.Mono``);
- ``ui``: ``frontend/``;
- ``test``: the upstream test projects, ``*.Test/`` and ``*.Test.*/``;
- ``unknown``: a path no rule covers, to be looked at;
- ``none``: localisation, build, CI, docs, packaging and the platforms
  Unarr does not run on (Windows, the updater, the console host);
- ``wontport``: every path of the commit, or its SHA, is in
  ``upstream/<app>-wontport.md``. A commit with a migration stays
  ``schema`` even then.

Upstream text (commit subjects, file names) is untrusted data: this script
prints it and never acts on it. In the Markdown output it goes into code
spans, without control characters, so it can neither break the table nor
render as a link, a mention or HTML.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys
import unicodedata
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
UPSTREAM = REPO / "upstream"

APPS = {"sonarr": "Sonarr", "radarr": "Radarr"}

# By priority, highest first.
CLASSES = ("wontport", "schema", "routes", "backend", "ui", "test", "unknown", "none")
# The order a commit's own classes are weighed in: `wontport` is decided
# per commit, not per path, so it is not among them.
PATH_PRIORITY = ("schema", "routes", "backend", "ui", "test", "unknown", "none")

# (regex on the path, class, why). The first match wins. The rules of the
# issue's table, plus the paths it does not name (marked "beyond #9").
RULES: list[tuple[re.Pattern[str], str, str]] = [
    (re.compile(r"^src/[^/]*\.Core/Datastore/Migration/"), "schema", "migration"),
    # Build files first: a .csproj under *.Api.V3/ is a package bump, not a route.
    (
        re.compile(r"(^|/)([^/]*\.(csproj|sln|props|targets|runsettings)|NuGet\.config|global\.json|stylecop\.json)$"),
        "none",
        "build (beyond #9: wins over the directory)",
    ),
    (re.compile(r"^src/[^/]*\.Core/Localization/"), "none", "localisation"),
    (re.compile(r"^frontend/"), "ui", "frontend"),
    (re.compile(r"^src/[^/]*\.Test(\.[^/]*)?/"), "test", "upstream test"),
    (re.compile(r"^src/[^/]*\.Api\.V3/"), "routes", "API v3"),
    (re.compile(r"^src/[^/]*\.Api(\.[^/]*)?/"), "routes", "another API (beyond #9)"),
    (re.compile(r"^src/[^/]*\.Http/"), "routes", "HTTP layer"),
    (re.compile(r"^src/[^/]*\.Core/"), "backend", "core"),
    (
        re.compile(r"^src/NzbDrone\.(Common|Host|SignalR|Mono)/"),
        "backend",
        "server code beside Core (beyond #9)",
    ),
    (
        re.compile(r"^src/(NzbDrone|NzbDrone\.(Windows|Update|Console)|ServiceHelpers|[^/]*\.RuntimePatches)/"),
        "none",
        "a platform Unarr does not run on (beyond #9)",
    ),
    (
        re.compile(r"^(\.github|\.devcontainer|\.vscode|\.idea|distribution|Logo|schemas)/"),
        "none",
        "CI, packaging, editor",
    ),
    (
        re.compile(r"^[^/]*\.(md|sh|yml|yaml|py|txt)$|^(LICENSE[^/]*|\.[^/]+)$"),
        "none",
        "docs, CI, repository files",
    ),
    # The UI's toolchain: its effect arrives with the pinned tarball.
    (
        re.compile(r"^(package\.json|yarn\.lock|pnpm-lock\.yaml|tsconfig\.json|[^/]*\.config\.(js|cjs|mjs|ts))$"),
        "none",
        "frontend toolchain (beyond #9)",
    ),
]

MIGRATION_FILE = re.compile(r"^src/[^/]*\.Core/Datastore/Migration/(\d+)_[^/]*\.cs$")
VERSION_TAG = re.compile(r"^v(\d+)\.(\d+)\.(\d+)\.(\d+)$")
SHA = re.compile(r"^[0-9a-f]{40}$")
WONTPORT_ITEM = re.compile(r"^[-*] +`([^`]+)`(.*)$")
STATUS = re.compile(r"^[ACDMRTUXB][0-9]*$")


# --- versions and the target -------------------------------------------------


@dataclass(frozen=True, order=True)
class Version:
    parts: tuple[int, int, int, int]

    @classmethod
    def parse(cls, text: str) -> Version:
        text = text.strip()
        match = VERSION_TAG.match(text if text.startswith("v") else "v" + text)
        if not match:
            raise ValueError(f"not a version X.Y.Z.B: {text!r}")
        return cls(tuple(int(g) for g in match.groups()))  # type: ignore[arg-type]

    @property
    def major(self) -> int:
        return self.parts[0]

    @property
    def tag(self) -> str:
        return "v" + str(self)

    def __str__(self) -> str:
        return ".".join(map(str, self.parts))


def jump_kind(pin: Version, target: Version) -> str:
    """patch, minor or major: how far `target` is from `pin`."""
    if target.major != pin.major:
        return "major"
    if target.parts[1] != pin.parts[1]:
        return "minor"
    return "patch"


def choose_target(pin: Version, releases: list[Version]) -> tuple[Version | None, str]:
    """The release a PR goes to, and the kind of jump; (None, "none") when
    no release is newer than the pin."""
    newer = sorted(v for v in set(releases) if v > pin)
    if not newer:
        return None, "none"
    newest = newer[-1]
    if newest.major == pin.major:
        return newest, jump_kind(pin, newest)
    next_major = min(v.major for v in newer if v.major > pin.major)
    return min(v for v in newer if v.major == next_major), "major"


def stable_from_api(path: Path) -> set[str]:
    """The tags the GitHub API marks as published stable releases. Takes
    one page (a list) or `--slurp` pages (a list of lists)."""
    data = json.loads(path.read_text())
    stack, tags = [data], set()
    while stack:
        item = stack.pop()
        if isinstance(item, list):
            stack.extend(item)
        elif isinstance(item, dict) and not item.get("draft") and not item.get("prerelease"):
            tag = item.get("tag_name")
            if isinstance(tag, str):
                tags.add(tag)
    return tags


# --- the cursor and the wontport list -----------------------------------------


@dataclass
class Lock:
    repo: str
    branch: str
    release: Version
    commit: str


def read_lock(path: Path) -> Lock:
    values: dict[str, str] = {}
    for n, line in enumerate(path.read_text().splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep:
            raise ValueError(f"{path}:{n}: expected `key = value`")
        values[key.strip()] = value.strip()
    missing = {"repo", "branch", "release", "commit"} - values.keys()
    if missing:
        raise ValueError(f"{path}: missing {', '.join(sorted(missing))}")
    if not SHA.match(values["commit"]):
        raise ValueError(f"{path}: commit is not a full SHA: {values['commit']!r}")
    if not re.match(r"^[A-Za-z0-9._/-]+$", values["branch"]):
        raise ValueError(f"{path}: odd branch name: {values['branch']!r}")
    return Lock(values["repo"], values["branch"], Version.parse(values["release"]), values["commit"])


def pinned_version(app: str) -> Version | None:
    """The pin of scripts/<app>-fixture.sh, to check the cursor against."""
    script = REPO / "scripts" / f"{app}-fixture.sh"
    var = app.upper() + "_VERSION"
    if not script.exists():
        return None
    match = re.search(rf'^{var}="\$\{{{var}:-([0-9.]+)\}}"', script.read_text(), re.M)
    return Version.parse(match.group(1)) if match else None


@dataclass
class Wontport:
    shas: list[str] = field(default_factory=list)
    globs: list[str] = field(default_factory=list)

    def matches(self, sha: str, paths: list[str]) -> bool:
        if any(sha.startswith(s) for s in self.shas):
            return True
        return bool(paths) and bool(self.globs) and all(
            any(fnmatch.fnmatchcase(p, g) for g in self.globs) for p in paths
        )


def read_wontport(path: Path) -> Wontport:
    """The list items at the start of a line whose first element is a code
    span: a SHA (7 to 40 hex digits) or a path glob. The rest is prose."""
    out = Wontport()
    if not path.exists():
        return out
    for line in path.read_text().splitlines():
        match = WONTPORT_ITEM.match(line)
        if not match:
            continue
        entry = match.group(1).strip()
        if re.fullmatch(r"[0-9a-fA-F]{7,40}", entry):
            out.shas.append(entry.lower())
        elif entry:
            out.globs.append(entry)
    return out


# --- classification ---------------------------------------------------------


def classify_path(path: str) -> tuple[str, str]:
    for pattern, cls, why in RULES:
        if pattern.search(path):
            return cls, why
    return "unknown", "no rule"


@dataclass
class Commit:
    sha: str
    date: str
    subject: str
    changes: list[tuple[str, str]]  # (status, path)
    cls: str = ""
    also: list[str] = field(default_factory=list)
    migrations: list[int] = field(default_factory=list)
    migrations_changed: list[int] = field(default_factory=list)
    unknown_paths: list[str] = field(default_factory=list)


def classify_commit(commit: Commit, wontport: Wontport) -> Commit:
    classes = set()
    for status, path in commit.changes:
        cls, _ = classify_path(path)
        classes.add(cls)
        if cls == "unknown":
            commit.unknown_paths.append(path)
        match = MIGRATION_FILE.match(path)
        if match:
            (commit.migrations if status == "A" else commit.migrations_changed).append(int(match.group(1)))
    ordered = [c for c in PATH_PRIORITY if c in classes] or ["none"]
    paths = [p for _, p in commit.changes]
    if wontport.matches(commit.sha, paths) and ordered[0] != "schema":
        commit.cls, commit.also = "wontport", ordered
    else:
        commit.cls, commit.also = ordered[0], ordered[1:]
    return commit


# --- git ----------------------------------------------------------------------


def git(repo: Path, *args: str) -> str:
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0", LC_ALL="C")
    result = subprocess.run(
        ["git", "-C", str(repo), "-c", "core.quotePath=false", "-c", "gc.auto=0", *args],
        capture_output=True,
        env=env,
    )
    if result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout.decode("utf-8", errors="replace")


def fetch(repo_dir: Path, lock: Lock) -> None:
    """A bare, blobless clone of the stable branch, and its tags (followed
    from the branch's history). Trees are enough to list the paths."""
    if not repo_dir.exists():
        repo_dir.parent.mkdir(parents=True, exist_ok=True)
        print(f"cloning {lock.repo} ({lock.branch}) into {repo_dir}", file=sys.stderr)
        subprocess.run(
            ["git", "-c", "gc.auto=0", "clone", "--quiet", "--bare", "--filter=blob:none", "--single-branch",
             "--branch", lock.branch, lock.repo, str(repo_dir)],
            check=True,
            env=dict(os.environ, GIT_TERMINAL_PROMPT="0"),
        )
        return
    print(f"fetching {lock.branch} into {repo_dir}", file=sys.stderr)
    git(repo_dir, "fetch", "--quiet", "--filter=blob:none", "origin",
        f"+refs/heads/{lock.branch}:refs/heads/{lock.branch}")


def releases_on_branch(repo_dir: Path, branch: str) -> dict[Version, str]:
    """Version -> tag, for the version tags reachable from the branch. Two
    tags of one version (a rebuild) keep the higher build number anyway."""
    out = {}
    for tag in git(repo_dir, "tag", "--merged", f"refs/heads/{branch}").split():
        if VERSION_TAG.match(tag):
            out[Version.parse(tag)] = tag
    return out


def commits_between(repo_dir: Path, base: str, tip: str) -> list[Commit]:
    """The non-merge commits in base..tip, oldest first, with the paths each
    one touches (renames as a deletion and an addition: no blob is fetched)."""
    rng = f"{base}..{tip}"
    meta = git(repo_dir, "log", "-z", "--reverse", "--no-merges", "--format=%H%x00%aI%x00%s", rng).split("\0")
    commits: dict[str, Commit] = {}
    order = []
    for i in range(0, len(meta) - 2, 3):
        sha = meta[i].strip()
        if not SHA.match(sha):
            raise RuntimeError(f"unexpected git log output near {sha[:50]!r}")
        commits[sha] = Commit(sha, meta[i + 1], meta[i + 2], [])
        order.append(sha)
    tokens = git(repo_dir, "log", "-z", "--reverse", "--no-merges", "--no-renames", "--name-status",
                 "--format=%H", rng).split("\0")
    current = None
    i = 0
    while i < len(tokens):
        token = tokens[i].lstrip("\n")
        if SHA.match(token):
            current = commits[token]
            i += 1
        elif STATUS.match(token) and current is not None and i + 1 < len(tokens):
            current.changes.append((token, tokens[i + 1]))
            i += 2
        elif not token:
            i += 1
        else:
            raise RuntimeError(f"unexpected git log output near {token[:50]!r}")
    return [commits[s] for s in order]


# --- output -------------------------------------------------------------------


def md_code(text: str, limit: int = 100) -> str:
    """Untrusted text as a Markdown code span that stays inside its table
    cell: no control or format characters (newlines, escapes, bidi
    overrides), no backticks, pipes escaped the GFM table way."""
    text = "".join(" " if unicodedata.category(c) in ("Cc", "Cf", "Zl", "Zp") else c for c in text)
    text = " ".join(text.split()).replace("`", "'")
    if len(text) > limit:
        text = text[: limit - 1] + "…"
    return "`" + (text or " ").replace("|", "\\|") + "`"


def class_cell(commit: Commit) -> str:
    return commit.cls + (f" (+{', '.join(commit.also)})" if commit.also else "")


def summary(commits: list[Commit]) -> dict[str, int]:
    counts = {c: 0 for c in CLASSES}
    for commit in commits:
        counts[commit.cls] += 1
    return counts


def render_markdown(report: dict, commits: list[Commit]) -> str:
    app = report["app"]
    lines = [f"## {APPS[app]}: {report['cursor_release']} → {report['target'] or 'no newer release'} "
             f"({report['jump']})", ""]
    lines.append(f"Branch `{report['branch']}`, cursor `{report['cursor'][:12]}`; "
                 f"newest stable release on the branch: {report['newest'] or 'none'}.")
    if report["releases_from"] == "tags":
        lines.append("Stable releases read from the tags on the branch (no API data given).")
    if report["target"] is None:
        lines.append("")
        lines.append("Nothing to do: no stable release newer than the cursor.")
        return "\n".join(lines) + "\n"
    lines.append(f"{len(commits)} commits ({report['merges']} merge commits left out), "
                 f"{report['compare']}")
    lines += ["", "| SHA | Class | Title |", "|---|---|---|"]
    for c in commits:
        lines.append(f"| `{c.sha[:10]}` | {class_cell(c)} | {md_code(c.subject)} |")
    lines += ["", "| Class | Commits |", "|---|---:|"]
    for cls, n in report["summary"].items():
        if n:
            lines.append(f"| {cls} | {n} |")
    lines.append(f"| **total** | **{len(commits)}** |")
    lines.append("")
    def by_migration(attr: str) -> list[str]:
        shas: dict[int, list[str]] = {}
        for c in commits:
            for m in getattr(c, attr):
                shas.setdefault(m, []).append(f"`{c.sha[:10]}`")
        return [f"{m} ({', '.join(s)})" for m, s in sorted(shas.items())]

    added, changed = by_migration("migrations"), by_migration("migrations_changed")
    lines.append(f"Migrations added: {len(added)}" + (", " + ", ".join(added) if added else "") + ".")
    if changed:
        lines.append(f"Migrations changed: {', '.join(changed)}.")
    unknown = sorted({p for c in commits for p in c.unknown_paths})
    if unknown:
        lines.append("")
        lines.append(f"Paths no rule covers ({len(unknown)}):")
        lines += [f"- {md_code(p, 160)}" for p in unknown]
    if report["warnings"]:
        lines.append("")
        lines += [f"Warning: {w}" for w in report["warnings"]]
    return "\n".join(lines) + "\n"


# --- main ---------------------------------------------------------------------


def cache_dir() -> Path:
    """UNARR_CACHE holds what .cache holds, as in the fixture scripts."""
    cache = os.environ.get("UNARR_CACHE")
    return Path(cache) if cache else REPO / ".cache"


def triage(app: str, repo_dir: Path, lock: Lock, wontport: Wontport, target_arg: str | None,
           api_releases: Path | None) -> tuple[dict, list[Commit]]:
    warnings = []
    pin = pinned_version(app)
    if pin is not None and pin != lock.release:
        warnings.append(f"the cursor's release {lock.release} is not the pin {pin} of scripts/{app}-fixture.sh")
    try:
        git(repo_dir, "cat-file", "-e", f"{lock.commit}^{{commit}}")
    except RuntimeError:
        raise SystemExit(f"the cursor {lock.commit} is not in {repo_dir}: run with --fetch")
    tags = releases_on_branch(repo_dir, lock.branch)
    releases_from = "tags"
    if api_releases is not None:
        stable = stable_from_api(api_releases)
        tags = {v: t for v, t in tags.items() if t in stable}
        releases_from = "api"
    newest = max(tags) if tags else None
    if target_arg:
        target = Version.parse(target_arg)
        if target not in tags:
            raise SystemExit(f"{target} is not a {'stable ' if api_releases else ''}release on {lock.branch}")
        if target <= lock.release:
            raise SystemExit(f"{target} is not newer than the cursor's release {lock.release}")
        jump = jump_kind(lock.release, target)
    else:
        target, jump = choose_target(lock.release, list(tags))
    commits: list[Commit] = []
    merges = 0
    compare = ""
    if target is not None:
        tip = git(repo_dir, "rev-parse", f"{tags[target]}^{{commit}}").strip()
        try:
            git(repo_dir, "merge-base", "--is-ancestor", lock.commit, tip)
        except RuntimeError:
            raise SystemExit(f"the cursor {lock.commit[:12]} is not an ancestor of {tags[target]}")
        commits = [classify_commit(c, wontport) for c in commits_between(repo_dir, lock.commit, tip)]
        merges = int(git(repo_dir, "rev-list", "--count", "--merges", f"{lock.commit}..{tip}").strip())
        base_url = lock.repo.removesuffix(".git")
        compare = f"{base_url}/compare/{lock.commit[:12]}...{tags[target]}"
    report = {
        "app": app,
        "branch": lock.branch,
        "cursor": lock.commit,
        "cursor_release": str(lock.release),
        "target": str(target) if target else None,
        "target_tag": tags[target] if target else None,
        "jump": jump,
        "newest": str(newest) if newest else None,
        "releases_from": releases_from,
        "merges": merges,
        "compare": compare,
        "summary": summary(commits),
        "migrations": sorted(m for c in commits for m in c.migrations),
        "warnings": warnings,
    }
    return report, commits


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--app", required=True, choices=sorted(APPS))
    ap.add_argument("--repo", type=Path, help="a clone of the upstream repository "
                    "(default: <UNARR_CACHE or .cache>/upstream/<App>.git)")
    ap.add_argument("--fetch", action="store_true", help="clone or update the upstream repository first")
    ap.add_argument("--target", help="the release to go to, instead of the rule (e.g. 7.0.0.10815)")
    ap.add_argument("--releases", type=Path, help="the GitHub API's releases JSON, to keep the stable ones only")
    ap.add_argument("--lock", type=Path, help="the cursor (default: upstream/<app>.lock)")
    ap.add_argument("--wontport", type=Path, help="the list (default: upstream/<app>-wontport.md)")
    ap.add_argument("--format", choices=("markdown", "json"), default="markdown")
    args = ap.parse_args(argv)

    lock = read_lock(args.lock or UPSTREAM / f"{args.app}.lock")
    wontport = read_wontport(args.wontport or UPSTREAM / f"{args.app}-wontport.md")
    repo_dir = args.repo or cache_dir() / "upstream" / f"{APPS[args.app]}.git"
    if args.fetch:
        fetch(repo_dir, lock)
    elif not repo_dir.exists():
        raise SystemExit(f"no upstream clone in {repo_dir}: run with --fetch")

    report, commits = triage(args.app, repo_dir, lock, wontport, args.target, args.releases)
    for warning in report["warnings"]:
        print(f"warning: {warning}", file=sys.stderr)
    if args.format == "json":
        report["commits"] = [
            {
                "sha": c.sha,
                "date": c.date,
                "subject": c.subject,
                "class": c.cls,
                "also": c.also,
                "migrations": c.migrations,
                "migrations_changed": c.migrations_changed,
                "paths": [{"status": s, "path": p, "class": classify_path(p)[0]} for s, p in c.changes],
            }
            for c in commits
        ]
        json.dump(report, sys.stdout, indent=2, ensure_ascii=True)
        print()
    else:
        sys.stdout.write(render_markdown(report, commits))
    return 0


if __name__ == "__main__":
    sys.exit(main())
