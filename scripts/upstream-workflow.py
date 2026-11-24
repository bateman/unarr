#!/usr/bin/env python3
"""The decisions of the workflow `upstream`, up to the draft PR (issue #9).

`.github/workflows/upstream.yml` calls these through Make, one personality
at a time; what decides is here, with its tests in
``scripts/tests/test_upstream_workflow.py``, so that it is tried without
GitHub::

    scripts/upstream-workflow.py plan --app radarr --event schedule --out DIR
    scripts/upstream-workflow.py announce --app radarr --plan DIR/plan.json
    scripts/upstream-workflow.py assemble --trusted DIR --untrusted DIR --work DIR
    scripts/upstream-workflow.py pr --app radarr --plan DIR/plan.json --work DIR

``plan``
    What this run does for the personality, in this order:

    - ``disabled``: a scheduled run, and the personality is not among those
      the weekly run follows (``--scheduled``, ``UPSTREAM_SCHEDULED`` in the
      Makefile). A manual start always goes on.
    - ``none``: no stable release newer than the cursor (the triage's rule,
      ``scripts/upstream-triage.py``, or the target given by hand).
    - ``open``: a port of this personality is already open, a pull request
      from a ``<app>/upstream-*`` branch of this repository: never a second
      one (issue #9, section 6). Its target may be this one or an older one.
    - ``branch``: the branch ``<app>/upstream-<target>`` exists with no open
      pull request: nothing is pushed over it.
    - ``announce``: a scheduled run found a new major: the issue
      "<App> <version> is out" is opened (``announce``) and nothing else;
      the port of a major starts by hand.
    - ``bump``: the bump runs, then the draft PR opens (``pr``).

    Writes ``plan.json`` and ``triage.md`` in ``--out``, the outputs of the
    step (``--github-output``) and a line in the run's summary.
``announce``
    Opens the issue of a major unless one with the same title is open.
``assemble``
    The work directory of the job that writes, from the two artifacts:
    what the job without secrets recorded (untrusted: copied as it is,
    links kept as links, for ``apply`` to refuse) and, over it, what the
    planning job fetched (``bump.json``, ``triage.json``, ``triage.md``),
    so that the release, its commit and its checksum are the ones fetched,
    whatever the recording job left.
``pr``
    After ``make bump-<app> STEPS=apply,test,summary``: checks that the
    pin is the plan's target and that only the files the bump writes have
    changed, commits them as ``Pin <App> <target>``, pushes the branch
    ``<app>/upstream-<target>``, opens the draft PR into the base branch
    with the short body of ``upstream/PORTING.md`` (section 1, point 4),
    labels it with the kind of jump and posts the bump's summary as its
    first comment. Its outputs (``pr``, ``branch``, ``app``, ``target``,
    ``jump``, ``url``) are what a later workflow hooks on.

Upstream text (commit subjects, what the original answered) is data: no
subject reaches a title, a body or an issue; the summary that is posted
quotes the original's answers in code spans (``scripts/pin-bump.py``
checks them first).
"""

from __future__ import annotations

import argparse
import base64
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO = SCRIPTS.parent
UPSTREAM = REPO / "upstream"


def _load(name: str, file: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / file)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


triage = sys.modules.get("upstream_triage") or _load("upstream_triage", "upstream-triage.py")
bump = sys.modules.get("pin_bump") or _load("pin_bump", "pin-bump.py")

APPS = triage.APPS
EVENTS = ("schedule", "workflow_dispatch")
ACTIONS = ("disabled", "none", "open", "branch", "announce", "bump")
JUMPS = ("patch", "minor", "major")
VERSION = re.compile(r"^\d+\.\d+\.\d+\.\d+$")
SLUG = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
SHA = re.compile(r"^[0-9a-f]{40}$")
# Every commit as CLAUDE.md says: the maintainer authors and commits it.
AUTHOR = ("Fabio Calefato", "fcalefato@gmail.com")
# The colours of the labels the workflow creates when missing (`major` exists).
LABEL_COLOURS = {"patch": "c2e0c6", "minor": "fbca04", "major": "b60205"}
# A comment holds at most 65536 characters.
COMMENT_LIMIT = 60000


class Stop(SystemExit):
    pass


def die(msg: str) -> None:
    raise Stop(f"[upstream] {msg}")


def log(msg: str) -> None:
    print(f"[upstream] {msg}", file=sys.stderr, flush=True)


def branch_name(app: str, target: str) -> str:
    """`<app>/upstream-<target>`, the name CLAUDE.md and PORTING.md give."""
    if app not in APPS or not VERSION.match(target):
        die(f"no branch name for {app!r} and {target!r}")
    return f"{app}/upstream-{target}"


def one_line(text: str, limit: int = 300) -> str:
    """A value for $GITHUB_OUTPUT: one line, no control character."""
    text = "".join(c if c.isprintable() else " " for c in str(text))
    return " ".join(text.split())[:limit]


# --- the decision -------------------------------------------------------------


def port_branches(app: str, pulls: list, repo: str) -> list[dict]:
    """The open pull requests of this repository from a `<app>/upstream-*`
    branch: the ports of this personality under way."""
    out = []
    for pr in pulls:
        head = pr.get("head") or {}
        ref = str(head.get("ref", ""))
        from_repo = ((head.get("repo") or {}).get("full_name"))
        if ref.startswith(f"{app}/upstream-") and from_repo == repo and isinstance(pr.get("number"), int):
            out.append({"number": pr["number"], "branch": ref, "draft": bool(pr.get("draft"))})
    return sorted(out, key=lambda p: p["number"])


def decide(app: str, event: str, report: dict | None, pulls: list, branches: list[str], repo: str,
           scheduled: list[str], target_given: bool) -> dict:
    """What the run does for `app`: a plan, with the action, the release
    and a message for the run's log. `report` is the triage's (None when
    the personality is off for the schedule and no triage ran)."""
    plan: dict = {"app": app, "event": event, "action": "", "message": ""}
    if event == "schedule" and app not in scheduled:
        plan["action"] = "disabled"
        plan["message"] = (f"{APPS[app]} is not followed by the weekly run (UPSTREAM_SCHEDULED in the Makefile: "
                           f"{', '.join(scheduled) or 'none'}); a manual start runs it")
        return plan
    assert report is not None
    plan.update({
        "from": report["cursor_release"],
        "target": report.get("target"),
        "tag": report.get("target_tag"),
        "jump": report.get("jump"),
        "newest": report.get("newest"),
        "releases_from": report.get("releases_from"),
    })
    if report.get("target") is None:
        plan["action"] = "none"
        plan["message"] = f"no stable release of {APPS[app]} newer than {report['cursor_release']}: nothing to do"
        return plan
    target = report["target"]
    branch = branch_name(app, target)
    plan["branch"] = branch
    ports = port_branches(app, pulls, repo)
    if ports:
        same = [p for p in ports if p["branch"] == branch]
        port = (same or ports)[0]
        plan["action"] = "open"
        plan["pr"] = port["number"]
        if same:
            plan["message"] = f"port already open: #{port['number']} ({port['branch']})"
        else:
            plan["message"] = (f"a port of {APPS[app]} is already open: #{port['number']} ({port['branch']}); "
                               f"{target} comes with it or after it, never in a second pull request")
        return plan
    if branch in branches:
        plan["action"] = "branch"
        plan["message"] = (f"the branch {branch} exists with no open pull request: nothing pushed over it; "
                           "open its pull request or delete it")
        return plan
    if report.get("jump") == "major" and event == "schedule" and not target_given:
        plan["action"] = "announce"
        plan["message"] = (f"{APPS[app]} {target} is a new major: the issue is opened, and the port starts by "
                           "hand (workflow_dispatch)")
        return plan
    plan["action"] = "bump"
    plan["message"] = (f"{APPS[app]} {report['cursor_release']} -> {target} ({report.get('jump')}): bump, then "
                       f"the draft pull request from {branch}")
    return plan


def triage_facts(report: dict) -> dict:
    """What the issue and the PR say of the triage: counts, no subject."""
    commits = report.get("commits", [])
    return {
        "commits": len(commits) if isinstance(commits, list) else 0,
        "merges": report.get("merges", 0),
        "summary": {c: n for c, n in report.get("summary", {}).items() if n},
        "migrations": report.get("migrations", []),
        "migrations_removed": report.get("migrations_removed", []),
        "compare": report.get("compare", ""),
        "warnings": len(report.get("warnings", [])),
    }


def check_plan(plan: dict) -> dict:
    """plan.json, read back: the values the later steps act on, checked."""
    ok = (plan.get("app") in APPS and plan.get("action") in ACTIONS and plan.get("event") in EVENTS)
    if ok and plan["action"] not in ("disabled", "none"):
        ok = (VERSION.match(str(plan.get("target"))) and VERSION.match(str(plan.get("from")))
              and plan.get("jump") in JUMPS and bump.TAG.match(str(plan.get("tag")))
              and plan.get("branch") == branch_name(plan["app"], plan["target"]))
    if not ok:
        die(f"an odd plan: {json.dumps(plan)[:300]}")
    return plan


def counts_text(summary: dict) -> str:
    return ", ".join(f"{n} {c}" for c, n in summary.items() if isinstance(n, int) and c in triage.CLASSES) or "none"


def compare_link(facts: dict) -> str:
    url = str(facts.get("compare", ""))
    return url if re.fullmatch(r"https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/compare/[0-9a-f]{12}\.\.\.v[0-9.]+",
                               url) else ""


def render_issue(plan: dict) -> tuple[str, str]:
    """The issue a scheduled run opens for a new major (issue #9, section 6)."""
    app = APPS[plan["app"]]
    facts = plan.get("triage", {})
    migrations = ", ".join(str(m) for m in facts.get("migrations", []) if isinstance(m, int)) or "none"
    title = f"{app} {plan['target']} is out"
    lines = [
        f"{app} {plan['target']} (`{plan['tag']}`) is the first stable release of a new major: "
        f"{plan['from']} → {plan['target']}, a **major** jump.",
        "",
        f"- Triage: {facts.get('commits', 0)} upstream commits since the cursor "
        f"({facts.get('merges', 0)} merge commits left out): {counts_text(facts.get('summary', {}))}.",
        f"- Migrations added: {migrations}.",
    ]
    link = compare_link(facts)
    if link:
        lines.append(f"- Upstream: {link}")
    lines += [
        "",
        "A scheduled run does not port a major (issue #9, section 6). To start the port: Actions, "
        f"workflow `upstream`, *Run workflow*, personality `{plan['app']}`, target empty (or `{plan['target']}`). "
        "It bumps, and opens the draft pull request labelled `major`.",
        "",
        "The weekly run does not open this issue again while it is open.",
    ]
    return title, "\n".join(lines) + "\n"


# --- GitHub ---------------------------------------------------------------------


class GitHub:
    """The REST calls the workflow needs, with the job's token."""

    def __init__(self, repo: str, token: str | None, api: str = "https://api.github.com"):
        if not SLUG.match(repo):
            die(f"not a repository: {repo!r}")
        self.repo, self.token, self.api = repo, token, api.rstrip("/")

    def call(self, method: str, path: str, body: dict | None = None, ok404: bool = False):
        url = f"{self.api}/repos/{self.repo}{path}"
        headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
                   "User-Agent": "unarr-upstream-workflow"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=60) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as err:
            if err.code == 404 and ok404:
                return None
            detail = err.read()[:300].decode(errors="replace")
            die(f"{method} {path}: {err.code} {detail}")
        except (urllib.error.URLError, OSError) as err:
            die(f"{method} {path}: {err}")
        return json.loads(raw) if raw else None

    def pages(self, path: str, limit: int = 10) -> list:
        out: list = []
        sep = "&" if "?" in path else "?"
        for page in range(1, limit + 1):
            data = self.call("GET", f"{path}{sep}per_page=100&page={page}")
            if not isinstance(data, list):
                die(f"GET {path}: not a list")
            out += data
            if len(data) < 100:
                break
        return out

    def open_pulls(self) -> list:
        return self.pages("/pulls?state=open")

    def upstream_branches(self, app: str) -> list[str]:
        refs = self.call("GET", f"/git/matching-refs/heads/{app}/upstream-") or []
        return [r["ref"].removeprefix("refs/heads/") for r in refs if isinstance(r, dict)
                and str(r.get("ref", "")).startswith("refs/heads/")]


def github_from_env(token_vars: tuple[str, ...] = ("GITHUB_TOKEN",)) -> GitHub:
    token = next((os.environ[v] for v in token_vars if os.environ.get(v)), None)
    return GitHub(os.environ.get("GITHUB_REPOSITORY", ""), token,
                  os.environ.get("GITHUB_API_URL") or "https://api.github.com")


def write_outputs(path: str | None, values: dict) -> None:
    lines = [f"{k}={'' if v is None else one_line(v)}" for k, v in values.items()]
    for line in lines:
        print(line)
    if path:
        with open(path, "a", encoding="utf-8") as out:
            out.write("\n".join(lines) + "\n")


def append_summary(text: str) -> None:
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as out:
            out.write(text.rstrip("\n") + "\n\n")


# --- plan -----------------------------------------------------------------------


def run_triage(app: str, target: str | None, releases: str) -> tuple[dict, str]:
    """The triage, as `make upstream-<app>` runs it: clone or fetch, then
    the target and the commits."""
    lock = triage.read_lock(UPSTREAM / f"{app}.lock")
    wontport = triage.read_wontport(UPSTREAM / f"{app}-wontport.md")
    repo_dir = triage.cache_dir() / "upstream" / f"{APPS[app]}.git"
    triage.fetch(repo_dir, lock)
    report, commits = triage.triage(app, repo_dir, lock, wontport, target, releases)
    text = triage.render_markdown(report, commits)
    report["commits"] = [{"sha": c.sha, "class": c.cls} for c in commits]
    return report, text


def cmd_plan(args) -> int:
    target = (args.target or "").strip() or None
    if target is not None and not VERSION.match(target):
        die(f"the target must be X.Y.Z.BUILD, not {target!r}")
    if args.event not in EVENTS:
        die(f"unknown event {args.event!r}: {', '.join(EVENTS)}")
    scheduled = [a for a in (args.scheduled or "").replace(",", " ").split() if a]
    unknown = [a for a in scheduled if a not in APPS]
    if unknown:
        die(f"unknown personality in UPSTREAM_SCHEDULED: {', '.join(unknown)}")
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    report, text = None, ""
    if not (args.event == "schedule" and args.app not in scheduled):
        report, text = run_triage(args.app, target, args.releases)
    pulls: list = []
    branches: list[str] = []
    repo = os.environ.get("GITHUB_REPOSITORY", "")
    if report is not None and report.get("target"):
        if args.pulls is not None:
            pulls = json.loads(Path(args.pulls).read_text())
            branches = json.loads(Path(args.branches).read_text()) if args.branches else []
        else:
            gh = github_from_env()
            repo = gh.repo
            pulls, branches = gh.open_pulls(), gh.upstream_branches(args.app)
    plan = decide(args.app, args.event, report, pulls, branches, repo, scheduled, target is not None)
    if report is not None:
        plan["triage"] = triage_facts(report)
    check_plan(plan)
    bump.write_json(out / "plan.json", plan)
    (out / "triage.md").write_text(text)
    print(plan["message"])
    write_outputs(args.github_output, {k: plan.get(k, "") for k in
                                       ("action", "app", "target", "tag", "jump", "from", "branch", "pr", "message")})
    append_summary(f"**{APPS[args.app]}**: {plan['message']}" + (f"\n\n{text}" if text else ""))
    return 0


# --- announce ---------------------------------------------------------------------


def find_open_issue(issues: list, title: str) -> int | None:
    for issue in issues:
        if "pull_request" not in issue and issue.get("title") == title and isinstance(issue.get("number"), int):
            return issue["number"]
    return None


def cmd_announce(args) -> int:
    plan = check_plan(json.loads(Path(args.plan).read_text()))
    if plan["action"] != "announce" or plan["app"] != args.app:
        die(f"the plan is {plan['action']} for {plan['app']}, not announce for {args.app}")
    title, body = render_issue(plan)
    gh = github_from_env()
    found = find_open_issue(gh.pages("/issues?state=open"), title)
    if found is not None:
        msg = f"already open: #{found} ({title})"
        print(msg)
        write_outputs(args.github_output, {"issue": found, "created": "false"})
        append_summary(f"**{APPS[args.app]}**: {msg}")
        return 0
    issue = gh.call("POST", "/issues", {"title": title, "body": body})
    number = issue.get("number") if isinstance(issue, dict) else None
    msg = f"opened #{number}: {title}"
    print(msg)
    write_outputs(args.github_output, {"issue": number, "created": "true"})
    append_summary(f"**{APPS[args.app]}**: {msg}")
    return 0


# --- assemble ---------------------------------------------------------------------

TRUSTED = ("bump.json", "triage.json", "triage.md")


def cmd_assemble(args) -> int:
    trusted, untrusted, work = Path(args.trusted), Path(args.untrusted), Path(args.work)
    if work.exists():
        shutil.rmtree(work)
    if untrusted.is_dir():
        # Links stay links: `apply` refuses them where it reads.
        shutil.copytree(untrusted, work, symlinks=True)
    else:
        die(f"no recording in {untrusted}")
    for name in TRUSTED:
        src = trusted / name
        if not src.is_file() or src.is_symlink():
            die(f"{src} missing: the planning job's artifact is incomplete")
        dest = work / name
        if dest.is_symlink() or dest.exists():
            if dest.is_dir() and not dest.is_symlink():
                shutil.rmtree(dest)
            else:
                dest.unlink()
        shutil.copyfile(src, dest)
    # What the recording job ran, only so that `record` does not run again:
    # the original's tarball is not handed on.
    shutil.rmtree(work / "original", ignore_errors=True)
    log(f"work directory {work}: the recording, with {', '.join(TRUSTED)} from the planning job")
    return 0


# --- pr ---------------------------------------------------------------------------


def allowed_path(app_key: str, path: str) -> bool:
    """The files `make bump-<app>` writes in the tree (pin-bump.py, apply)."""
    app = bump.APPS[app_key]
    fixtures = app.fixtures.relative_to(REPO).as_posix()
    if path == f"upstream/{app_key}.pin":
        return True
    if re.match(rf"^{re.escape(fixtures)}/golden[^/]*/[^/]+$", path):
        return True
    if re.match(rf"^{re.escape(app.schemas)}/[^/]+\.json$", path):
        return True
    return any(re.match(rf"^{re.escape(app.vectors)}/{re.escape(area)}/[^/]+\.json$", path)
               for area in bump.VECTOR_AREAS.values())


def pin_value(text: str, key: str) -> str | None:
    match = re.search(rf"^{key}[ \t]*=[ \t]*(\S+)[ \t]*$", text, re.M)
    return match.group(1) if match else None


def as_int(value) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def render_pr(plan: dict, summary: dict, old_migration: str | None, run_url: str, own_token: bool) -> tuple[str, str]:
    """The title and the short body of upstream/PORTING.md, section 1,
    point 4 (the Release section of section 7, point 1)."""
    app = APPS[plan["app"]]
    facts = plan.get("triage", {})
    title = f"{app} {plan['from']} → {plan['target']}"
    to = summary.get("to", {})
    commit = to.get("commit") if SHA.match(str(to.get("commit"))) else None
    migrations = ", ".join(str(m) for m in facts.get("migrations", []) if isinstance(m, int)) or "none"
    lines = ["## Release", "",
             f"{app} {plan['from']} → {plan['target']}, a **{plan['jump']}** jump. Tag `{plan['tag']}`"
             + (f" at `{commit}`." if commit else "."), ""]
    lines.append(f"- Triage: {facts.get('commits', 0)} commits ({facts.get('merges', 0)} merge commits left out): "
                 f"{counts_text(facts.get('summary', {}))}. Migrations added: {migrations}.")
    rec = summary.get("recordings", {})
    tests = summary.get("tests", {})
    importer = summary.get("importer", {})
    vectors = summary.get("vectors", {})
    parts = []
    if as_int(rec.get("files")) is not None and as_int(rec.get("changed")) is not None:
        parts.append(f"{rec['files']} recordings, {rec['changed']} changed beyond the instants")
        failed = [f for f in rec.get("failed_lists", []) if isinstance(f, str) and bump.NAME.match(f)]
        if failed:
            parts.append("lists not recorded: " + ", ".join(f"`{f}`" for f in failed))
    if as_int(tests.get("passed")) is not None and as_int(tests.get("failed")) is not None:
        parts.append(f"`cargo test --workspace` {tests['passed']} passed, {tests['failed']} failed")
        scripts = [f.get("name") for f in tests.get("scripts_failures", []) if isinstance(f, dict)]
        scripts = [n for n in scripts if isinstance(n, str) and re.fullmatch(r"[A-Za-z0-9_.]+", n)]
        parts.append("`make test-scripts` " + (f"failed on {', '.join(f'`{n}`' for n in scripts)}" if scripts
                                                else "passed"))
    if as_int(vectors.get("changed")) is not None:
        parts.append(f"{vectors['changed']} vector files changed")
    if as_int(importer.get("migration")) is not None:
        parts.append(f"migration level {old_migration or '?'} → {importer['migration']}")
    run = f", run by the workflow `upstream` in [this run]({run_url})" if run_url else ""
    lines.append(f"- Bump (`make bump-{plan['app']} TARGET={plan['target']}`{run}): " + "; ".join(parts) + ".")
    lines += ["",
              "The table of the upstream commits comes with the first update of the port. Left: all "
              f"{facts.get('commits', 0)} commits, and the checks of `upstream/PORTING.md`, section 8. The bump's "
              "summary is the first comment below; its work directory is the artifact "
              f"`bump-{plan['app']}` of the run."]
    if not own_token:
        lines += ["",
                  "CI has not run: a pull request the workflow's own token opens starts no workflow. The first push "
                  "to the branch starts it; before that, close and reopen this pull request."]
    return title, "\n".join(lines) + "\n"


def render_comment(plan: dict, summary_md: str) -> str:
    head = f"The summary of `make bump-{plan['app']} TARGET={plan['target']}` (`summary.md` of its work directory).\n\n"
    if len(head) + len(summary_md) > COMMENT_LIMIT:
        cut = summary_md[: COMMENT_LIMIT - len(head) - 200]
        cut = cut[: cut.rfind("\n")] if "\n" in cut else cut
        summary_md = cut + (f"\n\n… cut at {COMMENT_LIMIT} characters: the whole summary is in the artifact "
                            f"`bump-{plan['app']}` of the run.\n")
    return head + summary_md


def git(*args: str, env: dict | None = None) -> str:
    done = subprocess.run(["git", "-C", str(REPO), *args], capture_output=True, text=True,
                          env=dict(os.environ, **(env or {}), GIT_TERMINAL_PROMPT="0"))
    if done.returncode != 0:
        die(f"git {args[0]}: {done.stderr.strip()[:500]}")
    return done.stdout


def changed_paths() -> list[str]:
    out = git("status", "--porcelain=v1", "-z", "--untracked-files=all")
    paths = []
    entries = out.split("\0")
    i = 0
    while i < len(entries):
        entry = entries[i]
        i += 1
        if not entry:
            continue
        status, path = entry[:2], entry[3:]
        if "R" in status or "C" in status:
            i += 1  # the source of a rename follows
        paths.append(path)
    return sorted(paths)


def cmd_pr(args) -> int:
    plan = check_plan(json.loads(Path(args.plan).read_text()))
    if plan["action"] != "bump" or plan["app"] != args.app:
        die(f"the plan is {plan['action']} for {plan['app']}, not bump for {args.app}")
    app_key, target, branch = plan["app"], plan["target"], plan["branch"]
    work = Path(args.work)
    summary = json.loads((work / "summary.json").read_text())
    if summary.get("to", {}).get("version") != target or summary.get("app") != app_key:
        die(f"the summary is not of {app_key} {target}")
    pin_text = (UPSTREAM / f"{app_key}.pin").read_text()
    if pin_value(pin_text, "version") != target:
        die(f"upstream/{app_key}.pin is not at {target}: the bump was not applied")
    old_pin = git("show", f"HEAD:upstream/{app_key}.pin")
    old_migration = pin_value(old_pin, "migration")
    paths = changed_paths()
    if not paths:
        die("the bump changed nothing in the tree")
    stray = [p for p in paths if not allowed_path(app_key, p)]
    if stray:
        die("files the bump does not write have changed, nothing committed: " + ", ".join(stray[:20]))

    write_token = os.environ.get("UPSTREAM_TOKEN") or None
    gh = github_from_env(("UPSTREAM_TOKEN", "GITHUB_TOKEN"))
    # Again, just before writing: a port opened since the plan wins.
    again = decide(app_key, "workflow_dispatch", {"cursor_release": plan["from"], "target": target,
                                                   "target_tag": plan["tag"], "jump": plan["jump"]},
                   gh.open_pulls(), gh.upstream_branches(app_key), gh.repo, [], True)
    if again["action"] != "bump":
        print(again["message"])
        append_summary(f"**{APPS[app_key]}**: nothing pushed, {again['message']}")
        write_outputs(args.github_output, {"pr": again.get("pr", ""), "branch": "", "app": app_key,
                                           "target": target, "jump": plan["jump"], "url": ""})
        return 0

    name, email = AUTHOR
    who = {"GIT_AUTHOR_NAME": name, "GIT_AUTHOR_EMAIL": email, "GIT_COMMITTER_NAME": name,
           "GIT_COMMITTER_EMAIL": email}
    app = APPS[app_key]
    git("switch", "-c", branch)
    git("add", "--all", "--", *paths)
    body = (f"{app} goes from {plan['from']} to {target} ({plan['jump']}): the pin, and what make bump-{app_key} "
            "recorded again from the new original (the recordings, the provider schemas and strings) and the "
            f"test vectors of the whole tag. The cursor stays at {plan['from']} until the port is done.")
    git("commit", "--no-verify", "--no-gpg-sign", "-m", f"Pin {app} {target}", "-m", body, env=who)
    # The token reaches git through the environment of this one command,
    # never the repository's configuration (the checkout keeps none).
    token = gh.token or die("no token to push with (UPSTREAM_TOKEN or GITHUB_TOKEN)")
    basic = base64.b64encode(f"x-access-token:{token}".encode()).decode()
    server = os.environ.get("GITHUB_SERVER_URL") or "https://github.com"
    auth = {"GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": f"http.{server}/.extraheader",
            "GIT_CONFIG_VALUE_0": f"AUTHORIZATION: basic {basic}"}
    # No force: a branch someone pushed in the meantime refuses this one.
    git("push", "origin", f"HEAD:refs/heads/{branch}", env=auth)
    log(f"pushed {branch}")

    run_url = ""
    server, run_id = os.environ.get("GITHUB_SERVER_URL", ""), os.environ.get("GITHUB_RUN_ID", "")
    if re.fullmatch(r"https://[A-Za-z0-9.-]+", server) and run_id.isdigit():
        run_url = f"{server}/{gh.repo}/actions/runs/{run_id}"
    title, pr_body = render_pr(plan, summary, old_migration, run_url, write_token is not None)
    pr = gh.call("POST", "/pulls", {"title": title, "head": branch, "base": args.base, "body": pr_body,
                                     "draft": True, "maintainer_can_modify": True})
    number, url = pr.get("number"), pr.get("html_url", "")
    log(f"opened #{number} {url}")
    label = plan["jump"]
    if gh.call("GET", f"/labels/{label}", ok404=True) is None:
        gh.call("POST", "/labels", {"name": label, "color": LABEL_COLOURS[label],
                                     "description": f"An upstream port across a {label} release"})
    gh.call("POST", f"/issues/{number}/labels", {"labels": [label]})
    summary_md = (work / "summary.md").read_text(encoding="utf-8")
    gh.call("POST", f"/issues/{number}/comments", {"body": render_comment(plan, summary_md)})
    msg = f"opened #{number} from {branch}, labelled {label}"
    print(msg)
    append_summary(f"**{app}**: {msg}: {url}")
    write_outputs(args.github_output, {"pr": number, "branch": branch, "app": app_key, "target": target,
                                       "jump": label, "url": url})
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="command", required=True)
    p = sub.add_parser("plan", help="what this run does for the personality")
    p.add_argument("--app", required=True, choices=sorted(APPS))
    p.add_argument("--event", required=True)
    p.add_argument("--target", default="", help="X.Y.Z.BUILD, or empty for the triage's rule")
    p.add_argument("--releases", default="api", metavar="api|tags|FILE")
    p.add_argument("--scheduled", default="", help="the personalities the weekly run follows")
    p.add_argument("--out", required=True, help="where plan.json and triage.md go")
    p.add_argument("--pulls", help="the open pull requests as the API lists them (a file), instead of asking it")
    p.add_argument("--branches", help="the <app>/upstream-* branches (a JSON list), with --pulls")
    p.add_argument("--github-output", default=os.environ.get("GITHUB_OUTPUT"))
    a = sub.add_parser("announce", help="open the issue of a new major")
    a.add_argument("--app", required=True, choices=sorted(APPS))
    a.add_argument("--plan", required=True)
    a.add_argument("--github-output", default=os.environ.get("GITHUB_OUTPUT"))
    s = sub.add_parser("assemble", help="the work directory of the job that writes, from the two artifacts")
    s.add_argument("--trusted", required=True)
    s.add_argument("--untrusted", required=True)
    s.add_argument("--work", required=True)
    r = sub.add_parser("pr", help="commit the bump, push the branch, open the draft PR")
    r.add_argument("--app", required=True, choices=sorted(APPS))
    r.add_argument("--plan", required=True)
    r.add_argument("--work", required=True)
    r.add_argument("--base", default="main")
    r.add_argument("--github-output", default=os.environ.get("GITHUB_OUTPUT"))
    args = ap.parse_args(argv)
    try:
        return {"plan": cmd_plan, "announce": cmd_announce, "assemble": cmd_assemble, "pr": cmd_pr}[args.command](args)
    except (triage.GitError, ValueError) as err:
        die(str(err))
    return 1


if __name__ == "__main__":
    sys.exit(main())
