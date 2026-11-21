#!/usr/bin/env python3
"""Triage the upstream commits of a personality since the last one handled.

The deterministic first step of following Sonarr and Radarr (issue #9): read
the cursor in ``upstream/<app>.lock``, find the stable release to go to, list
the upstream commits between the cursor and that release, and classify each
one by the paths it touches, so a port starts from a table and not from a
raw diff::

    scripts/upstream-triage.py --app radarr --fetch
    scripts/upstream-triage.py --app radarr --target 7.0.3.10918 --format json

The target release (``choose_target``):

- the releases are the version tags (``vX.Y.Z.B``) reachable from the stable
  branch, ``git tag --merged``, never the tags by name alone: a clone also
  holds tags of other branches and of older projects; and of those, the
  ones GitHub publishes as stable;
- within the pin's major, the newest of them (a patch or minor jump);
- when the newest is in a later major, the first stable release of the next
  major (a major jump): ``x.0.0`` when that one is stable, a later one when
  it was a pre-release (Radarr published 7.0.0 to 7.0.3.10912 as
  pre-releases on ``master``: 6.4.4 goes to 7.0.3.10918); the releases
  after it come with the next runs;
- none newer than the pin: nothing to do.

Git cannot tell a stable release from a pre-release, and the stable branch
carries both. ``--releases`` says where the difference comes from:

- ``api`` (the default): the GitHub API, read anonymously or with
  ``GITHUB_TOKEN`` / ``GH_TOKEN`` when set; the answer is saved beside the
  clone, ``<cache>/upstream/<app>-releases.json``. A run that cannot read it
  stops, rather than take a pre-release as the target;
- a file: that JSON, as the API gives it (a page, or ``--slurp`` pages);
- ``tags``: every version tag on the branch counts as stable, pre-releases
  included. Only for when the API is out of reach, knowingly.

Drafts and pre-releases are dropped. ``--target`` names the release outright.

The classes (``CLASSES``, by priority: a commit takes the highest class of
its paths and lists the others after it):

- ``schema``: a migration of ``*.Core/Datastore/Migration/``;
- ``routes``: ``*.Api.*/`` and ``*.Http/``;
- ``backend``: the rest of ``*.Core/``, and the server code beside it
  (``NzbDrone.Common``, ``.Host``, ``.SignalR``, ``.Mono``);
- ``ui``: ``frontend/``;
- ``test``: the upstream test projects, ``*.Test/`` and ``*.Test.*/``;
- ``unknown``: a path no rule covers, to be looked at;
- ``none``: the localisation strings, build, CI, docs, packaging and the platforms
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
import urllib.error
import urllib.request
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
    # The strings only: LocalizationService.cs and its friends are code.
    (re.compile(r"^src/[^/]*\.Core/Localization/Core/"), "none", "localisation strings"),
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


def github_slug(repo_url: str) -> str:
    """owner/name of a github.com repository URL."""
    match = re.fullmatch(r"https://github\.com/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+?)(\.git)?/?", repo_url)
    if not match:
        raise ValueError(f"not a github.com repository: {repo_url!r}")
    return f"{match.group(1)}/{match.group(2)}"


def fetch_releases(repo_url: str, dest: Path, pages: int = 10) -> Path:
    """The repository's releases from the GitHub API, newest first, saved
    to `dest` as a list of pages (the shape of `gh api --slurp`)."""
    slug = github_slug(repo_url)
    headers = {"Accept": "application/vnd.github+json", "User-Agent": "unarr-upstream-triage"}
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    out = []
    for page in range(1, pages + 1):
        url = f"https://api.github.com/repos/{slug}/releases?per_page=100&page={page}"
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=30) as resp:
                data = json.load(resp)
        except (urllib.error.URLError, OSError, ValueError) as err:
            raise SystemExit(
                f"cannot read the releases of {slug} from the GitHub API ({err}): without them a pre-release "
                "can be taken for a release. Give RELEASES=<the API's JSON> or, knowingly, RELEASES=tags"
            )
        if not isinstance(data, list):
            raise SystemExit(f"unexpected answer from the GitHub API for {slug}: {str(data)[:200]}")
        out.append(data)
        if len(data) < 100:
            break
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(json.dumps(out))
    return dest


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
    migrations_removed: list[int] = field(default_factory=list)
    unknown_paths: list[str] = field(default_factory=list)


def classify_commit(commit: Commit, wontport: Wontport) -> Commit:
    """Fills the class and the migrations of `commit` from its changes; the
    same result however many times it is called."""
    classes = set()
    commit.unknown_paths = []
    added: set[int] = set()
    deleted: set[int] = set()
    modified: set[int] = set()
    for status, path in commit.changes:
        cls, _ = classify_path(path)
        classes.add(cls)
        if cls == "unknown":
            commit.unknown_paths.append(path)
        match = MIGRATION_FILE.match(path)
        if match:
            {"A": added, "D": deleted}.get(status, modified).add(int(match.group(1)))
    # Without rename detection a renamed file is a deletion and an addition:
    # the same number on both sides is no new level, only a changed one.
    renamed = added & deleted
    commit.migrations = sorted(added - renamed)
    commit.migrations_changed = sorted(modified | renamed)
    commit.migrations_removed = sorted(deleted - renamed)
    ordered = [c for c in PATH_PRIORITY if c in classes] or ["none"]
    paths = [p for _, p in commit.changes]
    if wontport.matches(commit.sha, paths) and ordered[0] != "schema":
        commit.cls, commit.also = "wontport", ordered
    else:
        commit.cls, commit.also = ordered[0], ordered[1:]
    return commit


# --- git ----------------------------------------------------------------------


class GitError(RuntimeError):
    def __init__(self, args: tuple[str, ...], returncode: int, stderr: str):
        super().__init__(f"git {' '.join(args)} (exit {returncode}): {stderr}")
        self.returncode = returncode


def run_git(repo: Path | None, *args: str) -> subprocess.CompletedProcess[bytes]:
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0", LC_ALL="C")
    where = ["-C", str(repo)] if repo is not None else []
    return subprocess.run(
        ["git", *where, "-c", "core.quotePath=false", "-c", "gc.auto=0", *args],
        capture_output=True,
        env=env,
    )


def git(repo: Path | None, *args: str) -> str:
    result = run_git(repo, *args)
    if result.returncode != 0:
        raise GitError(args, result.returncode, result.stderr.decode(errors="replace").strip())
    return result.stdout.decode("utf-8", errors="replace")


def is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    """git merge-base --is-ancestor: exit 1 is a plain no; anything else
    (a missing object, a failed lazy fetch) is an error, not an answer."""
    args = ("merge-base", "--is-ancestor", ancestor, descendant)
    result = run_git(repo, *args)
    if result.returncode in (0, 1):
        return result.returncode == 0
    raise GitError(args, result.returncode, result.stderr.decode(errors="replace").strip())


def is_bare(repo: Path) -> bool:
    return git(repo, "rev-parse", "--is-bare-repository").strip() == "true"


def fetch(repo_dir: Path, lock: Lock) -> None:
    """A bare, blobless clone of the stable branch, and its tags (followed
    from the branch's history). Trees are enough to list the paths."""
    if not repo_dir.exists():
        repo_dir.parent.mkdir(parents=True, exist_ok=True)
        print(f"cloning {lock.repo} ({lock.branch}) into {repo_dir}", file=sys.stderr)
        git(None, "clone", "--quiet", "--bare", "--filter=blob:none", "--single-branch",
            "--branch", lock.branch, lock.repo, str(repo_dir))
        return
    print(f"fetching {lock.branch} into {repo_dir}", file=sys.stderr)
    git(repo_dir, "fetch", "--quiet", "--filter=blob:none", "origin",
        f"+refs/heads/{lock.branch}:refs/heads/{lock.branch}")


def releases_on_branch(repo_dir: Path, ref: str) -> dict[Version, str]:
    """Version -> tag, for the version tags reachable from `ref`. A rebuild
    (same X.Y.Z, another build number) is a release of its own here; the
    caller drops those that bring no commit past the cursor."""
    out = {}
    for tag in git(repo_dir, "tag", "--merged", ref).split():
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
        lines.append("Stable releases taken from the tags alone: a pre-release counts as a release.")
    else:
        lines.append(f"Stable releases as GitHub publishes them ({report['releases_from']}).")
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
    removed = by_migration("migrations_removed")
    if removed:
        lines.append(f"Migrations removed (the importer still meets them): {', '.join(removed)}.")
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
           releases: str) -> tuple[dict, list[Commit]]:
    """`releases`: "api", "tags" or the path of the API's JSON (see the
    module's docstring)."""
    warnings = []
    pin = pinned_version(app)
    if pin is not None and pin != lock.release:
        warnings.append(f"the cursor's release {lock.release} is not the pin {pin} of scripts/{app}-fixture.sh")
    if run_git(repo_dir, "cat-file", "-e", f"{lock.commit}^{{commit}}").returncode != 0:
        raise SystemExit(f"the cursor {lock.commit} is not in {repo_dir}: run with --fetch")
    tags = releases_on_branch(repo_dir, f"refs/heads/{lock.branch}")
    # The cursor's two fields must agree, or the range and the target
    # would be computed from different places.
    if lock.release not in tags:
        raise SystemExit(f"the cursor's release {lock.release} has no tag on {lock.branch}")
    cursor_tag_commit = git(repo_dir, "rev-parse", f"{tags[lock.release]}^{{commit}}").strip()
    if cursor_tag_commit != lock.commit:
        raise SystemExit(f"the cursor's commit {lock.commit[:12]} is not the commit of "
                         f"{tags[lock.release]} ({cursor_tag_commit[:12]}): fix upstream/{app}.lock")
    # A newer tag on a commit the cursor already has (a rebuild of the
    # cursor's release) brings nothing to port.
    tags = {
        v: t for v, t in tags.items()
        if v <= lock.release or not is_ancestor(repo_dir, git(repo_dir, "rev-parse", f"{t}^{{commit}}").strip(),
                                                 lock.commit)
    }
    if releases == "tags":
        releases_from = "tags"
        warnings.append("stable releases taken from the tags alone: a pre-release counts as a release")
    else:
        if releases == "api":
            path = fetch_releases(lock.repo, cache_dir() / "upstream" / f"{app}-releases.json")
            releases_from = "api"
        else:
            path, releases_from = Path(releases), "file"
        stable = stable_from_api(path)
        tags = {v: t for v, t in tags.items() if t in stable}
    newest = max(tags) if tags else None
    if target_arg:
        target = Version.parse(target_arg)
        if target not in tags:
            raise SystemExit(f"{target} is not a {'' if releases == 'tags' else 'stable '}release on {lock.branch}")
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
        if not is_ancestor(repo_dir, lock.commit, tip):
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
        "migrations_removed": sorted(m for c in commits for m in c.migrations_removed),
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
    ap.add_argument("--releases", default="api", metavar="api|tags|FILE",
                    help="where stable releases are told from pre-releases (default: api)")
    ap.add_argument("--lock", type=Path, help="the cursor (default: upstream/<app>.lock)")
    ap.add_argument("--wontport", type=Path, help="the list (default: upstream/<app>-wontport.md)")
    ap.add_argument("--format", choices=("markdown", "json"), default="markdown")
    args = ap.parse_args(argv)

    repo_dir = args.repo or cache_dir() / "upstream" / f"{APPS[args.app]}.git"
    if not args.fetch and not repo_dir.exists():
        raise SystemExit(f"no upstream clone in {repo_dir}: run with --fetch")
    try:
        lock = read_lock(args.lock or UPSTREAM / f"{args.app}.lock")
        wontport = read_wontport(args.wontport or UPSTREAM / f"{args.app}-wontport.md")
        if args.fetch:
            fetch(repo_dir, lock)
        # The bare clone --fetch makes: a working clone's local branch can
        # lag behind origin's, and nothing here would notice.
        if not is_bare(repo_dir):
            raise SystemExit(f"{repo_dir} is not a bare clone: leave out --repo to use the cache clone")
        report, commits = triage(args.app, repo_dir, lock, wontport, args.target, args.releases)
    except (GitError, ValueError) as err:
        raise SystemExit(f"error: {err}")
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
                "migrations_removed": c.migrations_removed,
                "paths": [
                    dict(zip(("status", "path", "class", "why"), (s, p, *classify_path(p)))) for s, p in c.changes
                ],
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
