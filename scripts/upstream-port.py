#!/usr/bin/env python3
"""What the workflow of the port's agent decides, apart from the agent.

`.github/workflows/upstream-port.yml` starts the agent that follows
`upstream/PORTING.md` on a port's draft PR (issue #9). The workflow calls
this script, from `main`, never from the PR's branch::

    scripts/upstream-port.py plan --pr-json pr.json --repository O/R --github-output F
    scripts/upstream-port.py commit-msg .git/COMMIT_EDITMSG     # the commit-msg hook
    scripts/upstream-port.py verify --base SHA --head SHA --pr-json pr.json
    scripts/upstream-port.py review --pr N --file comments.json # what the agent calls
    scripts/upstream-port.py ready --pr-json pr.json ...         # upstream-loop.yml: may the agent start
    scripts/upstream-port.py next --pr-json pr.json --round N ... # upstream-loop.yml: after a run of it

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

``ready`` and ``next`` are the decisions of `.github/workflows/upstream-loop.yml`,
which joins the agent and the checks with the original on one PR, in at
most ``ROUNDS`` runs of the agent per run of the workflow (the rounds are
written out in the YAML, one job each: no job starts another run).
``ready`` says whether the agent may start at all: a port's draft PR, the
secret ``ANTHROPIC_API_KEY`` set, ``main`` protected from the job's token;
when one is missing it says which and the run ends green. ``next`` reads,
after each run of the agent, the one thing the agent hands the workflow:
the block ``upstream-next`` at the end of the PR's body (`upstream/PORTING.md`,
section 7, "What comes next"), a fixed set of keys whose values are checked
here, never free text. It gives the next round: ``checks`` (with the inputs
of `upstream-checks.yml`, checked as that workflow checks them), ``port``, or
``stop``. Each round's spend is its share of ``TOTAL_BUDGET``
(``round_limits``), so that a run of the workflow never allows more than that.
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
# upstream-loop.yml: the runs of the agent in one run of the workflow, and
# what they may spend together. Round n gets the jump's limit, or what is
# left of the total after the rounds before it if that is less: 40, 40 and
# 20 USD for a patch or minor, 100, 100 and 50 for a major. The YAML writes
# out ROUNDS jobs of the agent and ROUNDS - 1 of the checks
# (scripts/tests/test_upstream_port.py checks it).
ROUNDS = 3
TOTAL_BUDGET = {"minor": 100, "major": 250}
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


def round_limits(jump: str, round_: int) -> dict:
    """The limits of the agent's run `round_` (1 to ROUNDS) of a run of
    upstream-loop.yml: the jump's, with the spend cut to what the rounds
    before it leave of TOTAL_BUDGET. A run started on its own (by hand) is
    round 1."""
    if not isinstance(round_, int) or isinstance(round_, bool) or not 1 <= round_ <= ROUNDS:
        die(f"the round is from 1 to {ROUNDS}, not {round_!r}")
    limits = dict(LIMITS[jump])
    spent = sum(round_limits(jump, r)["budget"] for r in range(1, round_))
    limits["budget"] = min(limits["budget"], TOTAL_BUDGET[jump] - spent)
    if limits["budget"] <= 0:
        die(f"round {round_} of a {jump} has nothing left of {TOTAL_BUDGET[jump]} USD")
    return limits


def write_outputs(path: str | None, values: dict) -> None:
    text = outputs_text(values)
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(text)
    else:
        sys.stdout.write(text)


def append_summary(text: str) -> None:
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(text + "\n")


def cmd_plan(args) -> int:
    checks = load_checks()
    pr = checks.check_port_pr(json.loads(Path(args.pr_json).read_text()), args.repository)
    if args.pr is not None and int(args.pr) != pr["number"]:
        die(f"asked for PR #{args.pr}, read #{pr['number']}")
    if not pr["draft"]:
        die(f"PR #{pr['number']} is not a draft: the agent works only on a draft (issue #9, section 6)")
    if not re.match(r"^[\w.-]+/[\w.-]+$", args.repository):
        die("--repository is owner/name")
    if os.environ.get("HAS_KEY") == "false":
        die("the secret ANTHROPIC_API_KEY is not set: the agent cannot start (upstream/README.md, "
            "\"What the maintainer sets\")")
    jump = "major" if pr["major"] else "minor"
    limits = round_limits(jump, args.round)
    prompt = render_prompt(PROMPT.read_text(), pr, args.repository, limits, args.helper)
    values = {"app": pr["app"], "branch": pr["branch"], "head": pr["head"], "jump": jump,
              "round": args.round, "minutes": limits["minutes"], "step_minutes": limits["minutes"] + MARGIN,
              "job_minutes": limits["minutes"] + MARGIN + SETUP,
              "turns": limits["turns"], "budget": limits["budget"], "margin": MARGIN, "prompt": prompt}
    write_outputs(args.github_output, values)
    return 0


# --- ready and next: upstream-loop.yml -------------------------------------

NEXT_FENCE = "```upstream-next"
NEXT_KEYS = {"next", "head"}
CHECK_KEYS = {"recordings", "http_audit", "compare", "replays"}
CHECK_OPTIONAL = {"old_pin_request", "old_pin_wait"}
NEXT_LINE = re.compile(r"^(?P<key>[a-z_]+):(?: (?P<value>\S.*))?$")
SHA = re.compile(r"^[0-9a-f]{40}$")


def parse_next(body: str) -> dict | None:
    """The block `upstream-next` of the PR's body (PORTING.md, section 7,
    "What comes next"), checked: None when there is none. A block out of
    shape stops: the loop never guesses what the agent meant."""
    lines = [line.rstrip("\r") for line in (body or "").split("\n")]
    starts = [i for i, line in enumerate(lines) if line.strip() == NEXT_FENCE]
    if not starts:
        return None
    if len(starts) > 1:
        die("the PR's body has more than one block upstream-next")
    block = []
    for line in lines[starts[0] + 1:]:
        if line.strip() == "```":
            break
        block.append(line.strip())
    else:
        die("the block upstream-next is not closed")
    if len(block) > 12:
        die("the block upstream-next has more lines than it has keys")
    values: dict[str, str] = {}
    for line in filter(None, block):
        m = NEXT_LINE.match(line)
        if not m:
            die(f"a line of the block upstream-next is not `key: value`: {line[:80]!r}")
        key = m["key"]
        if key in values:
            die(f"the block upstream-next names {key} twice")
        if key not in NEXT_KEYS | CHECK_KEYS | CHECK_OPTIONAL:
            die(f"the block upstream-next has an unknown key {key!r}")
        values[key] = (m["value"] or "").strip()
    if not NEXT_KEYS <= set(values):
        die("the block upstream-next needs next and head")
    if values["next"] not in ("port", "checks", "done"):
        die(f"next is port, checks or done, not {values['next'][:40]!r}")
    if not SHA.match(values["head"]):
        die("head is the full commit the agent left the branch at (40 hexadecimal digits)")
    given = set(values) - NEXT_KEYS
    if values["next"] == "checks":
        missing = CHECK_KEYS - given
        if missing:
            die(f"next: checks names every check: {', '.join(sorted(missing))} missing")
        c = load_checks().parse_checks(values["recordings"], values["http_audit"], values["compare"],
                                       values["replays"], values.get("old_pin_request", ""),
                                       values.get("old_pin_wait", "0"))
        compare = next(k for k, v in load_checks().COMPARE_CHOICES.items() if list(v) == c["compare"])
        old = c["old_pin"] or {"request": "", "wait": 0}
        return {"next": "checks", "head": values["head"], "checks": {
            "recordings": ",".join(c["recordings"]) or "none", "http_audit": str(c["http_audit"]).lower(),
            "compare": compare, "replays": str(c["replays"]).lower(), "old_pin_request": old["request"],
            "old_pin_wait": str(old["wait"])}}
    if given:
        die(f"next: {values['next']} takes no checks ({', '.join(sorted(given))})")
    return {"next": values["next"], "head": values["head"]}


def ready_decision(pr: dict, has_key: bool, protection) -> tuple[bool, str]:
    """May the agent start on `pr` (check_port_pr's)? `protection` is called
    only when the rest holds: it asks GitHub why main is not protected."""
    n = pr["number"]
    if not pr["draft"]:
        return False, f"PR #{n} is not a draft: the agent works only on a draft, nothing to do"
    if not has_key:
        return False, (f"the secret ANTHROPIC_API_KEY is not set: the agent does not start on PR #{n}, nor the "
                       "checks it would ask for; set it, then start upstream-loop by hand with the PR's number "
                       "(upstream/README.md, \"What the maintainer sets\")")
    problems = protection()
    if problems:
        return False, (f"main is not protected from the workflows' token ({'; '.join(problems)}): the agent "
                       f"does not start on PR #{n}. " + load_checks().PROTECT_HINT
                       + " Then start upstream-loop by hand with the PR's number.")
    return True, f"PR #{n}: the agent starts"


def cmd_ready(args) -> int:
    checks = load_checks()
    pr = checks.check_port_pr(json.loads(Path(args.pr_json).read_text()), args.repository)
    if args.pr is not None and int(args.pr) != pr["number"]:
        die(f"asked for PR #{args.pr}, read #{pr['number']}")
    has_key = os.environ.get("HAS_KEY") == "true"
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN") or ""
    go, message = ready_decision(pr, has_key, lambda: checks.main_protection(args.repository, token))
    print(message)
    append_summary(f"**upstream-loop**, PR #{pr['number']}: {message}")
    write_outputs(args.github_output, {"go": str(go).lower(), "message": message})
    return 0


def next_decision(pr: dict, body: str, round_: int, port_result: str, checked: list[str]) -> dict:
    """What follows run `round_` of the agent on `pr` (check_port_pr's,
    read after it): {"action": "port" | "checks" | "stop", "message",
    "failed": bool, "head", "checks"}. `checked` holds the commits the
    checks already ran at in this run of the workflow."""
    n = pr["number"]
    restart = f"start upstream-loop by hand with PR {n} to go on"
    out = {"action": "stop", "failed": False, "head": pr["head"], "checks": None}
    if port_result != "success":
        out["message"] = (f"run {round_} of the agent ended {port_result or 'unknown'}: the loop stops here; "
                          f"read that run, then {restart}")
        return out
    if not pr["draft"]:
        out["message"] = f"PR #{n} is ready for review: the port is done, the loop stops"
        return out
    block = parse_next(body)
    if block is None:
        out.update(failed=True, message=(f"run {round_} of the agent left no block upstream-next in the body of "
                                         f"PR #{n} (upstream/PORTING.md, section 7): the loop stops; {restart}"))
        return out
    if block["head"] != pr["head"]:
        out.update(failed=True, message=(f"the block upstream-next names {block['head'][:12]}, the branch is at "
                                         f"{pr['head'][:12]}: it was not written at the end of run {round_}; "
                                         f"the loop stops; {restart}"))
        return out
    if block["next"] == "done":
        out["message"] = (f"run {round_} of the agent says it is done for now (the PR's body says what it waits "
                          "for): the loop stops")
        return out
    if round_ >= ROUNDS:
        out["message"] = (f"run {round_} of the agent asks for {block['next']}, but {ROUNDS} runs of the agent is the "
                          f"limit of one run of the workflow: the loop stops here; {restart}")
        return out
    if block["next"] == "port":
        out.update(action="port", message=f"run {round_} of the agent asks for another run: run {round_ + 1} starts")
        return out
    if pr["head"] in checked:
        out["message"] = (f"run {round_} of the agent asks for the checks again at {pr['head'][:12]}, where they "
                          f"already ran in this run of the workflow: the loop stops; {restart}")
        return out
    asked = ", ".join(f"{k} {v}" for k, v in block["checks"].items() if v not in ("", "none", "false", "0"))
    out.update(action="checks", checks=block["checks"],
               message=(f"run {round_} of the agent asks for the checks with the original at {pr['head'][:12]} "
                        f"({asked}); then run {round_ + 1} of the agent"))
    return out


def cmd_next(args) -> int:
    checks = load_checks()
    data = json.loads(Path(args.pr_json).read_text())
    pr = checks.check_port_pr(data, args.repository)
    if args.pr is not None and int(args.pr) != pr["number"]:
        die(f"asked for PR #{args.pr}, read #{pr['number']}")
    checked = [c for c in (args.checked or "").split(",") if c]
    for c in checked:
        if not SHA.match(c):
            die(f"--checked holds commits, not {c[:40]!r}")
    try:
        got = next_decision(pr, data.get("body") or "", args.round, args.port_result, checked)
    except SystemExit as err:  # a block out of shape: this script's, or the checks' own refusal
        got = {"action": "stop", "failed": True, "head": pr["head"], "checks": None,
               "message": f"{err}: the loop stops; start upstream-loop by hand with PR {pr['number']} to go on"}
    print(got["message"])
    append_summary(f"**upstream-loop**, PR #{pr['number']}, after run {args.round} of the agent: {got['message']}")
    values = {"action": got["action"], "head": got["head"],
              "checked": ",".join(checked + ([got["head"]] if got["action"] == "checks" else []))}
    values.update(got["checks"] or {"recordings": "none", "http_audit": "false", "compare": "none",
                                     "replays": "false", "old_pin_request": "", "old_pin_wait": "0"})
    write_outputs(args.github_output, values)
    return 1 if got["failed"] else 0


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
    p.add_argument("--round", type=int, default=1, help=f"the run of the agent in upstream-loop.yml, 1 to {ROUNDS}")
    y = sub.add_parser("ready", help="upstream-loop.yml: may the agent start on the PR (HAS_KEY, GH_TOKEN)")
    y.add_argument("--pr-json", required=True)
    y.add_argument("--pr", type=int)
    y.add_argument("--repository", required=True)
    y.add_argument("--github-output")
    n = sub.add_parser("next", help="upstream-loop.yml: what follows a run of the agent, from the PR's body")
    n.add_argument("--pr-json", required=True, help="gh pr view --json, with the body")
    n.add_argument("--pr", type=int)
    n.add_argument("--repository", required=True)
    n.add_argument("--round", type=int, required=True)
    n.add_argument("--port-result", required=True, help="the result of that run's job (success, failure, ...)")
    n.add_argument("--checked", default="", help="the commits the checks already ran at in this run, comma-separated")
    n.add_argument("--github-output")
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
    return {"plan": cmd_plan, "ready": cmd_ready, "next": cmd_next, "commit-msg": cmd_commit_msg,
            "verify": cmd_verify, "review": cmd_review}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
