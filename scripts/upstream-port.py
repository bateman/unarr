#!/usr/bin/env python3
"""What the workflow of the port's agent decides, apart from the agent.

`.github/workflows/upstream-port.yml` starts the agent that follows
`upstream/PORTING.md` on a port's draft PR (issue #9). The workflow calls
this script, from `main`, never from the PR's branch::

    scripts/upstream-port.py plan --pr-json pr.json --repository O/R --github-output F
    scripts/upstream-port.py commit-msg .git/COMMIT_EDITMSG     # the commit-msg hook
    scripts/upstream-port.py verify --base SHA --head SHA --pr-json pr.json
    scripts/upstream-port.py review --pr N --file comments.json # what the agent calls

``plan`` refuses a PR that is not a port's draft (`<app>/upstream-<version>`
of this repository, into `main`, open, a draft) and writes the job's
outputs: the app, the branch, its head, the limits of the run by the kind of
jump (a `major` label gets the higher ones: issue #9, section 6) and the
agent's prompt, `upstream/port-prompt.md` with its placeholders filled from
those checked values only. The deadline, which depends on when the agent
starts, is filled in by the job (``@DEADLINE@``).

``commit-msg`` is the hook the job installs: a message with a trailer that
names someone else, or a line that names a tool, an agent, a model or a
session, is refused (`CLAUDE.md`, "Commit authorship"). ``Upstream:`` lines
are the port's own (`PORTING.md`, section 4) and pass.

``verify`` runs after the agent, whatever became of it: every commit the run
added is authored and committed as the maintainer, its message passes
``commit-msg``, the branch only moved forward, and the PR's title and body
name no tool. It changes nothing; a failure fails the job and says what to
correct.

``review`` posts the agent's review comments on its own PR as one review of
kind "comment", the only kind it can post, on the PR's head: the agent
needs no `gh api`, which could also approve or merge.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import subprocess
import sys
import urllib.request
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO = SCRIPTS.parent
PROMPT = REPO / "upstream" / "port-prompt.md"

AUTHOR = ("Fabio Calefato", "fcalefato@gmail.com")

# The limit of a run (PORTING.md, fixed rule 5), by the kind of jump: the
# agent's own minutes (the job adds its set-up: a hosted runner stops a job
# at 360), its turns and its spend.
# First values, to review after the first runs (issue #9).
LIMITS = {
    "minor": {"minutes": 150, "turns": 400, "budget": 40},
    "major": {"minutes": 270, "turns": 800, "budget": 100},
}
# Minutes between the deadline the agent is given and the job's stop: what
# fixed rule 5 asks before the limit (push, log rows, PR body).
MARGIN = 30
# Minutes of the job around the agent: the checkout, the toolchain, the web
# UIs, the check after the run.
SETUP = 60

# Lines refused in a commit message, a PR title or body, a comment the
# agent writes. CLAUDE.md is the repository's file of that name, not a
# tool: it is taken out before the check.
TRAILER = re.compile(r"^\s*(co-authored-by|signed-off-by|assisted-by|generated-by|helped-by|session|"
                     r"claude-session|agent|model)\s*:", re.IGNORECASE)
TOOL = re.compile(r"\b(claude|anthropic|copilot|chatgpt|openai|gpt-?\d|gemini|codex|cursor ai|llm|"
                  r"language model|ai agent|ai assistant)\b|🤖|generated with|claude\.ai|"
                  r"session_[0-9A-Za-z]{8,}", re.IGNORECASE)
ALLOWED_NAMES = re.compile(r"CLAUDE\.md")


class PortError(SystemExit):
    pass


def die(msg: str) -> None:
    raise PortError(f"upstream-port: {msg}")


def load_checks():
    spec = importlib.util.spec_from_file_location("upstream_checks_for_port", SCRIPTS / "upstream-checks.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def forbidden(text: str) -> list[str]:
    """The lines of `text` that name a tool or carry a trailer."""
    out = []
    for line in text.splitlines():
        if line.startswith("#"):  # git's own comments in the message file
            continue
        bare = ALLOWED_NAMES.sub("", line)
        if TRAILER.match(bare) or TOOL.search(bare):
            out.append(line.strip())
    return out


# --- plan -------------------------------------------------------------------


def render_prompt(template: str, pr: dict, repository: str, limits: dict, helper: str) -> str:
    values = {"@PR@": str(pr["number"]), "@REPOSITORY@": repository, "@BRANCH@": pr["branch"],
              "@TURNS@": str(limits["turns"]), "@BUDGET@": str(limits["budget"]), "@MARGIN@": str(MARGIN),
              "@HELPER@": helper}
    for key, value in values.items():
        template = template.replace(key, value)
    left = set(re.findall(r"@[A-Z]+@", template)) - {"@DEADLINE@"}
    if left:
        die(f"the prompt has placeholders nobody fills: {', '.join(sorted(left))}")
    return template


def outputs_text(values: dict) -> str:
    """GITHUB_OUTPUT's format, a delimiter for values on several lines."""
    out = []
    for key, value in values.items():
        value = str(value)
        if "\n" in value:
            delim = "EOF_" + os.urandom(8).hex()
            if delim in value:
                die("a delimiter found in the value")
            out.append(f"{key}<<{delim}\n{value}\n{delim}")
        else:
            out.append(f"{key}={value}")
    return "\n".join(out) + "\n"


def cmd_plan(args) -> int:
    checks = load_checks()
    pr = checks.check_port_pr(json.loads(Path(args.pr_json).read_text()), args.repository)
    if args.pr is not None and int(args.pr) != pr["number"]:
        die(f"asked for PR #{args.pr}, read #{pr['number']}")
    if not pr["draft"]:
        die(f"PR #{pr['number']} is not a draft: the agent works only on a draft (issue #9, section 6)")
    if not re.match(r"^[\w.-]+/[\w.-]+$", args.repository):
        die("--repository is owner/name")
    limits = LIMITS["major" if pr["major"] else "minor"]
    prompt = render_prompt(PROMPT.read_text(), pr, args.repository, limits, args.helper)
    values = {"app": pr["app"], "branch": pr["branch"], "head": pr["head"],
              "jump": "major" if pr["major"] else "minor",
              "minutes": limits["minutes"], "step_minutes": limits["minutes"] + MARGIN,
              "job_minutes": limits["minutes"] + MARGIN + SETUP,
              "turns": limits["turns"], "budget": limits["budget"], "margin": MARGIN, "prompt": prompt}
    text = outputs_text(values)
    if args.github_output:
        with open(args.github_output, "a", encoding="utf-8") as f:
            f.write(text)
    else:
        sys.stdout.write(text)
    return 0


# --- commit-msg and verify ----------------------------------------------------


def cmd_commit_msg(args) -> int:
    bad = forbidden(Path(args.file).read_text(errors="replace"))
    if bad:
        print("commit refused: the message names a tool or carries a trailer (CLAUDE.md, Commit authorship):",
              file=sys.stderr)
        for line in bad:
            print(f"    {line}", file=sys.stderr)
        return 1
    return 0


def git(*args: str, repo: Path) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=True).stdout


def problems_of_commits(repo: Path, base: str, head: str) -> list[str]:
    problems = []
    if subprocess.run(["git", "-C", str(repo), "merge-base", "--is-ancestor", base, head]).returncode != 0:
        return [f"{head[:12]} does not descend from {base[:12]}: the branch was rewritten"]
    raw = git("log", "--format=%H%x00%an%x00%ae%x00%cn%x00%ce%x00%B%x01", f"{base}..{head}", repo=repo)
    for entry in filter(None, (e.strip("\n") for e in raw.split("\x01"))):
        sha, an, ae, cn, ce, body = entry.split("\x00", 5)
        if (an, ae) != AUTHOR:
            problems.append(f"{sha[:12]}: authored by {an} <{ae}>")
        if (cn, ce) != AUTHOR:
            problems.append(f"{sha[:12]}: committed by {cn} <{ce}>")
        for line in forbidden(body):
            problems.append(f"{sha[:12]}: message line {line!r}")
    return problems


def problems_of_pr(pr: dict) -> list[str]:
    out = [f"PR title: {line!r}" for line in forbidden(pr.get("title") or "")]
    out += [f"PR body: {line!r}" for line in forbidden(pr.get("body") or "")]
    return out


def cmd_verify(args) -> int:
    repo = Path(args.repo)
    problems = problems_of_commits(repo, args.base, args.head)
    if args.pr_json:
        problems += problems_of_pr(json.loads(Path(args.pr_json).read_text()))
    if problems:
        print(f"{len(problems)} problem(s) in what the run left (CLAUDE.md, Commit authorship):")
        for p in problems:
            print(f"  - {p}")
        return 1
    count = len(git("rev-list", f"{args.base}..{args.head}", repo=repo).split())
    print(f"{count} commit(s) added by the run, each authored and committed as {AUTHOR[0]} <{AUTHOR[1]}>; "
          "no trailer and no tool named, in the commits and in the PR")
    return 0


# --- review -------------------------------------------------------------------

REVIEW_HELP = """The file is a JSON list, one object per comment:
    [{"path": "crates/unarr-api/src/lib.rs", "line": 120, "body": "Chose ... over ..., because ..."},
     {"path": "...", "start_line": 10, "line": 14, "body": "..."}]
`line` (and `start_line` for a range) are lines of the file at the PR's head,
on the new side of the diff. One review of kind "comment" is posted with all
of them, and an optional --body."""


def check_comments(comments) -> list[dict]:
    if not isinstance(comments, list) or not comments or len(comments) > 100:
        die("the file holds a list of 1 to 100 comments")
    out = []
    for c in comments:
        if not isinstance(c, dict) or set(c) - {"path", "line", "start_line", "body"}:
            die(f"a comment has other keys than path, line, start_line, body: {c!r:.80}")
        path, line, body = c.get("path"), c.get("line"), c.get("body")
        if not isinstance(path, str) or not re.match(r"^[\w./@+-]+$", path) or ".." in path.split("/"):
            die(f"not a path of the repository: {path!r}")
        if not isinstance(line, int) or isinstance(line, bool) or line < 1:
            die(f"{path}: line is a positive number")
        if not isinstance(body, str) or not body.strip() or len(body) > 6000:
            die(f"{path}:{line}: the body is text of at most 6000 characters")
        bad = forbidden(body)
        if bad:
            die(f"{path}:{line}: the body names a tool: {bad[0]!r}")
        item = {"path": path, "line": line, "side": "RIGHT", "body": body}
        if "start_line" in c:
            start = c["start_line"]
            if not isinstance(start, int) or isinstance(start, bool) or not 1 <= start < line:
                die(f"{path}:{line}: start_line comes before line")
            item.update(start_line=start, start_side="RIGHT")
        out.append(item)
    return out


def api(method: str, url: str, token: str, body: dict | None = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={
        "Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28", "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read() or b"{}")


def cmd_review(args) -> int:
    comments = check_comments(json.loads(Path(args.file).read_text()))
    body = args.body or ""
    if forbidden(body):
        die("the review's body names a tool")
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN") or ""
    if not re.match(r"^[\w.-]+/[\w.-]+$", repository) or not token:
        die("GITHUB_REPOSITORY and GH_TOKEN are needed")
    base = f"{os.environ.get('GITHUB_API_URL', 'https://api.github.com')}/repos/{repository}/pulls/{int(args.pr)}"
    head = api("GET", base, token)["head"]["sha"]
    review = {"commit_id": head, "event": "COMMENT", "body": body, "comments": comments}
    got = api("POST", f"{base}/reviews", token, review)
    print(f"review {got.get('id')} posted on {head[:12]}: {len(comments)} comment(s)")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("plan", help="check the PR; write the job's outputs and the prompt")
    p.add_argument("--pr-json", required=True)
    p.add_argument("--pr", type=int)
    p.add_argument("--repository", required=True)
    p.add_argument("--helper", required=True, help="where the job puts this script for the agent")
    p.add_argument("--github-output", help="append the outputs here (default: print them)")
    c = sub.add_parser("commit-msg", help="the commit-msg hook")
    c.add_argument("file")
    v = sub.add_parser("verify", help="check what the run left on the branch and the PR")
    v.add_argument("--repo", default=str(REPO))
    v.add_argument("--base", required=True)
    v.add_argument("--head", required=True)
    v.add_argument("--pr-json")
    r = sub.add_parser("review", help="post review comments on the PR, as one review of kind comment",
                       description=REVIEW_HELP, formatter_class=argparse.RawDescriptionHelpFormatter)
    r.add_argument("--pr", required=True, type=int)
    r.add_argument("--file", required=True)
    r.add_argument("--body", default="")
    args = ap.parse_args(argv)
    return {"plan": cmd_plan, "commit-msg": cmd_commit_msg, "verify": cmd_verify, "review": cmd_review}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
