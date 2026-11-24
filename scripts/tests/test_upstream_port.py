"""Tests of scripts/upstream-port.py and of the guard that keeps the port's
agent from running an original: made-up data, a throwaway git repository, no
network. Run with `make test-scripts` (part of `make check`)."""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent.parent
REPO = SCRIPTS.parent

_spec = importlib.util.spec_from_file_location("upstream_port", SCRIPTS / "upstream-port.py")
port = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = port
_spec.loader.exec_module(port)

HEAD = "a" * 40


def pr(**over):
    base = {"number": 28, "state": "OPEN", "isDraft": True, "baseRefName": "main",
            "headRefName": "radarr/upstream-9.1.2.3456", "headRefOid": HEAD, "isCrossRepository": False,
            "headRepository": {"name": "unarr"}, "headRepositoryOwner": {"login": "bateman"}, "labels": [],
            "title": "Radarr 8.0.0.1 → 9.1.2.3456", "body": "As CLAUDE.md says."}
    base.update(over)
    return base


class Plan(unittest.TestCase):
    def plan(self, data: dict) -> dict:
        with tempfile.TemporaryDirectory() as tmp:
            path, out = Path(tmp) / "pr.json", Path(tmp) / "out"
            path.write_text(json.dumps(data))
            port.main(["plan", "--pr-json", str(path), "--pr", "28", "--repository", "bateman/unarr",
                       "--helper", "/tmp/runner/upstream-port.py", "--github-output", str(out)])
            text = out.read_text()
        values, lines = {}, iter(text.splitlines())
        for line in lines:
            if "<<" in line:
                key, delim = line.split("<<", 1)
                body = []
                for inner in lines:
                    if inner == delim:
                        break
                    body.append(inner)
                values[key] = "\n".join(body)
            else:
                key, value = line.split("=", 1)
                values[key] = value
        return values

    def test_a_minor_and_a_major_get_their_limits(self):
        minor = self.plan(pr())
        major = self.plan(pr(labels=[{"name": "major"}]))
        self.assertEqual((minor["jump"], int(minor["minutes"]), int(minor["turns"])),
                         ("minor", port.LIMITS["minor"]["minutes"], port.LIMITS["minor"]["turns"]))
        self.assertEqual((major["jump"], int(major["budget"])), ("major", port.LIMITS["major"]["budget"]))
        self.assertEqual(int(major["step_minutes"]), int(major["minutes"]) + port.MARGIN)
        self.assertGreater(int(major["job_minutes"]), int(major["step_minutes"]))
        # A hosted runner stops a job at six hours.
        self.assertLessEqual(int(major["job_minutes"]), 360)

    def test_the_prompt_is_the_repositorys_with_checked_values(self):
        got = self.plan(pr())["prompt"]
        self.assertIn("Follow `upstream/PORTING.md` on pull request #28 of bateman/unarr", got)
        self.assertIn("`radarr/upstream-9.1.2.3456`", got)
        self.assertIn("python3 -I /tmp/runner/upstream-port.py review --pr 28", got)
        self.assertIn("@DEADLINE@", got)  # filled in when the agent starts
        self.assertEqual(set(__import__("re").findall(r"@[A-Z]+@", got)), {"@DEADLINE@"})
        # Nothing of the PR's own text: title and body are data.
        self.assertNotIn("CLAUDE.md says", got)
        self.assertNotIn("8.0.0.1", got)

    def test_only_a_ports_draft(self):
        for case in (pr(isDraft=False), pr(headRefName="core/x"), pr(state="MERGED")):
            with self.subTest(case=case), self.assertRaises(SystemExit):
                self.plan(case)


class Messages(unittest.TestCase):
    def test_what_is_refused(self):
        for text in [
            "Port a fix\n\nCo-Authored-By: Claude <noreply@anthropic.com>",
            "Port a fix\n\nco-authored-by: someone <a@b>",
            "Port a fix\n\nGenerated with Claude Code",
            "Port a fix\n\n🤖 made here",
            "Port a fix\n\nhttps://claude.ai/code/session_0123456789abcdef",
            "Port a fix\n\nSigned-off-by: Fabio Calefato <fcalefato@gmail.com>",
            "Port a fix written by an AI agent",
            "Port a fix\n\nClaude-Session: x",
        ]:
            with self.subTest(text=text):
                self.assertTrue(port.forbidden(text))

    def test_what_passes(self):
        for text in [
            "Answer Radarr's strings as JSON whatever the Accept\n\nUpstream: 56426048e3\n\nAs CLAUDE.md says.",
            "Record Radarr's Recyclarr exchange again\n\n# Please enter the commit message (a git comment)",
            "Log 46 decisions of the Radarr port",
        ]:
            with self.subTest(text=text):
                self.assertEqual(port.forbidden(text), [])

    def test_the_hook(self):
        with tempfile.TemporaryDirectory() as tmp:
            msg = Path(tmp) / "MSG"
            msg.write_text("Port\n\nCo-authored-by: x <y>\n")
            with mock.patch("sys.stderr"):
                self.assertEqual(port.main(["commit-msg", str(msg)]), 1)
            msg.write_text("Port\n\nUpstream: abc\n")
            self.assertEqual(port.main(["commit-msg", str(msg)]), 0)


class Verify(unittest.TestCase):
    def setUp(self):
        self.repo = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.repo)
        self.git("init", "-q")
        self.commit("start")
        self.base = self.git("rev-parse", "HEAD").strip()

    def git(self, *args, name="Fabio Calefato", email="fcalefato@gmail.com"):
        env = dict(os.environ, GIT_AUTHOR_NAME=name, GIT_AUTHOR_EMAIL=email, GIT_COMMITTER_NAME=name,
                   GIT_COMMITTER_EMAIL=email)
        return subprocess.run(["git", "-C", str(self.repo), *args], env=env, capture_output=True, text=True,
                              check=True).stdout

    def commit(self, msg, **who):
        self.git("commit", "-q", "--allow-empty", "-m", msg, **who)
        return self.git("rev-parse", "HEAD").strip()

    def test_commits_of_the_maintainer_pass(self):
        head = self.commit("Port a fix\n\nUpstream: 0123456789")
        self.assertEqual(port.problems_of_commits(self.repo, self.base, head), [])

    def test_another_author_a_trailer_or_a_rewrite(self):
        self.commit("Port a fix", name="claude[bot]", email="41898282+claude[bot]@users.noreply.github.com")
        head = self.commit("Port another\n\nCo-Authored-By: Claude <noreply@anthropic.com>")
        problems = port.problems_of_commits(self.repo, self.base, head)
        self.assertEqual(len(problems), 3, problems)
        self.git("checkout", "-q", "--orphan", "other")
        other = self.commit("unrelated")
        self.assertIn("rewritten", port.problems_of_commits(self.repo, head, other)[0])

    def test_the_pr_title_and_body(self):
        self.assertEqual(port.problems_of_pr(pr()), [])
        self.assertTrue(port.problems_of_pr(pr(body="Done.\n\n🤖 Generated with [Claude Code](https://x)")))


class Review(unittest.TestCase):
    def test_comments_are_checked(self):
        good = [{"path": "crates/unarr-api/src/lib.rs", "line": 12, "body": "Chose A over B, because C."},
                {"path": "scripts/x.py", "start_line": 3, "line": 5, "body": "Kept."}]
        got = port.check_comments(good)
        self.assertEqual(got[0], {"path": "crates/unarr-api/src/lib.rs", "line": 12, "side": "RIGHT",
                                  "body": "Chose A over B, because C."})
        self.assertEqual((got[1]["start_line"], got[1]["start_side"]), (3, "RIGHT"))
        for bad in ([], [{"path": "../etc/passwd", "line": 1, "body": "x"}],
                    [{"path": "a.rs", "line": 0, "body": "x"}],
                    [{"path": "a.rs", "line": 2, "body": "x", "event": "APPROVE"}],
                    [{"path": "a.rs", "line": 2, "body": "As Claude, I chose"}],
                    [{"path": "a.rs", "line": 2, "start_line": 2, "body": "x"}]):
            with self.subTest(bad=bad), self.assertRaises(SystemExit):
                port.check_comments(bad)

    def test_only_a_review_of_kind_comment_is_posted(self):
        calls = []

        def fake(method, url, token, body=None):
            calls.append((method, url, body))
            return {"head": {"sha": HEAD}} if method == "GET" else {"id": 7}

        with tempfile.TemporaryDirectory() as tmp:
            f = Path(tmp) / "c.json"
            f.write_text(json.dumps([{"path": "a.rs", "line": 2, "body": "x"}]))
            env = {"GITHUB_REPOSITORY": "bateman/unarr", "GH_TOKEN": "t"}
            with mock.patch.object(port, "api", fake), mock.patch.dict(os.environ, env), mock.patch("sys.stdout"):
                port.main(["review", "--pr", "28", "--file", str(f)])
        self.assertEqual(calls[1][0], "POST")
        self.assertTrue(calls[1][1].endswith("/repos/bateman/unarr/pulls/28/reviews"))
        self.assertEqual((calls[1][2]["event"], calls[1][2]["commit_id"]), ("COMMENT", HEAD))


class NoOriginal(unittest.TestCase):
    """UNARR_NO_ORIGINAL, set in the agent's job: no script runs an original."""

    def test_the_fixture_scripts_refuse_to_run_one(self):
        env = dict(os.environ, UNARR_NO_ORIGINAL="1")
        for app in ("sonarr", "radarr"):
            for cmd in ("golden", "shell"):
                with self.subTest(app=app, cmd=cmd):
                    done = subprocess.run(["bash", str(SCRIPTS / f"{app}-fixture.sh"), cmd], env=env,
                                          capture_output=True, text=True, timeout=60)
                    self.assertNotEqual(done.returncode, 0)
                    self.assertIn("UNARR_NO_ORIGINAL is set", done.stderr)

    def test_the_comparison_refuses_too(self):
        spec = importlib.util.spec_from_file_location("compare_for_port", SCRIPTS / "compare-originals.py")
        co = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(co)
        with mock.patch.dict(os.environ, {"UNARR_NO_ORIGINAL": "1"}), self.assertRaises(SystemExit):
            co.original_binaries()

    def test_the_checks_job_refuses_to_run_under_it(self):
        checks = port.load_checks()
        with mock.patch.dict(os.environ, {"UNARR_NO_ORIGINAL": "1"}), self.assertRaises(SystemExit):
            checks.main(["run", "--app", "radarr", "--checks", "{}", "--out", "/nonexistent"])


OTHER = "b" * 40


def block(*lines: str) -> str:
    return "\n".join(["```upstream-next", *lines, "```"])


CHECKS_BLOCK = block("next: checks", f"head: {HEAD}", "recordings: recyclarr", "http_audit: true",
                     "compare: real", "replays: true")


class Rounds(unittest.TestCase):
    """What the runs of the agent in one run of upstream-loop.yml may spend."""

    def test_each_round_has_its_share_and_together_no_more_than_the_cap(self):
        for jump in ("minor", "major"):
            budgets = [port.round_limits(jump, r)["budget"] for r in range(1, port.ROUNDS + 1)]
            with self.subTest(jump=jump):
                self.assertEqual(len(port.ROUND_BUDGETS[jump]), port.ROUNDS)
                self.assertEqual(sum(budgets), port.TOTAL_BUDGET[jump])
                self.assertTrue(all(b > 0 for b in budgets))
                # The first run, the port itself, has the largest share; the
                # last, which settles the checks' differences, the smallest.
                self.assertEqual(budgets, sorted(budgets, reverse=True))
                # A run started alone never spends more than a whole loop.
                self.assertEqual(port.LIMITS[jump]["budget"], budgets[0])
                self.assertLessEqual(port.LIMITS[jump]["budget"], port.TOTAL_BUDGET[jump])
        # The caps decided on 2026-11-24, and the values upstream/README.md gives.
        self.assertEqual(port.TOTAL_BUDGET, {"minor": 40, "major": 100})
        self.assertEqual([port.round_limits("minor", r)["budget"] for r in (1, 2, 3)], [20, 12, 8])
        self.assertEqual([port.round_limits("major", r)["budget"] for r in (1, 2, 3)], [50, 30, 20])
        # Two patches or minors and a major in one month stay within the key's 200 USD.
        self.assertLessEqual(2 * port.TOTAL_BUDGET["minor"] + port.TOTAL_BUDGET["major"], 200)
        self.assertEqual(port.round_limits("major", 3)["minutes"], port.LIMITS["major"]["minutes"])

    def test_a_round_out_of_the_loop_is_refused(self):
        for r in (0, port.ROUNDS + 1, -1, True):
            with self.subTest(round=r), self.assertRaises(SystemExit):
                port.round_limits("minor", r)

    def test_the_plan_of_a_later_round_gives_its_share(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, out = Path(tmp) / "pr.json", Path(tmp) / "out"
            path.write_text(json.dumps(pr(labels=[{"name": "major"}])))
            port.main(["plan", "--pr-json", str(path), "--pr", "28", "--repository", "bateman/unarr",
                       "--helper", "/h", "--round", "3", "--github-output", str(out)])
            text = out.read_text()
        self.assertIn("\nbudget=20\n", text)
        self.assertIn("\nround=3\n", text)

    def test_the_plan_refuses_without_the_key(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "pr.json"
            path.write_text(json.dumps(pr()))
            with mock.patch.dict(os.environ, {"HAS_KEY": "false"}), self.assertRaises(SystemExit) as got:
                port.main(["plan", "--pr-json", str(path), "--repository", "bateman/unarr", "--helper", "/h"])
        self.assertIn("ANTHROPIC_API_KEY", str(got.exception))


class TheBlock(unittest.TestCase):
    """The block upstream-next of the PR's body: the one thing the agent
    hands the workflow, checked key by key."""

    def test_port_and_done(self):
        for word in ("port", "done"):
            with self.subTest(word=word):
                got = port.parse_next(f"## Checks\n\nall green\n\n{block(f'next: {word}', f'head: {HEAD}')}\n")
                self.assertEqual(got, {"next": word, "head": HEAD})

    def test_checks_with_their_inputs(self):
        got = port.parse_next("text\r\n" + CHECKS_BLOCK.replace("\n", "\r\n"))
        self.assertEqual(got["next"], "checks")
        self.assertEqual(got["checks"], {"recordings": "recyclarr", "http_audit": "true", "compare": "real",
                                         "replays": "true", "old_pin_request": "", "old_pin_wait": "0"})
        got = port.parse_next(block("next: checks", f"head: {HEAD}", "recordings: none", "http_audit: false",
                                    "compare: none", "replays: false", "old_pin_request: /api/v3/log?page=1",
                                    "old_pin_wait: 60"))
        self.assertEqual((got["checks"]["recordings"], got["checks"]["old_pin_request"],
                          got["checks"]["old_pin_wait"]), ("none", "/api/v3/log?page=1", "60"))

    def test_the_example_of_porting_is_a_block_this_reads(self):
        import re
        text = (REPO / "upstream" / "PORTING.md").read_text()
        example = re.search(r"\n````\n(```upstream-next\n.*?\n```)\n````\n", text, re.S)
        self.assertIsNotNone(example)
        got = port.parse_next(re.sub(r"head: <[^>]*>", f"head: {HEAD}", example.group(1)))
        self.assertEqual((got["next"], got["checks"]["old_pin_wait"]), ("checks", "120"))

    def test_no_block(self):
        self.assertIsNone(port.parse_next("## Release\n\nRadarr 7\n"))
        self.assertIsNone(port.parse_next(""))
        # A block of another kind is not this one.
        self.assertIsNone(port.parse_next("```text\nnext: checks\n```"))

    def test_what_is_refused(self):
        cases = {
            "two blocks": block("next: done", f"head: {HEAD}") + "\n" + block("next: done", f"head: {HEAD}"),
            "not closed": "```upstream-next\nnext: done\nhead: " + HEAD,
            "unknown key": block("next: done", f"head: {HEAD}", "run: rm -rf /"),
            "key twice": block("next: done", "next: port", f"head: {HEAD}"),
            "no head": block("next: done"),
            "short head": block("next: done", "head: abc123"),
            "odd next": block("next: merge", f"head: {HEAD}"),
            "free text": block("next: done", f"head: {HEAD}", "please run the checks"),
            "checks on port": block("next: port", f"head: {HEAD}", "compare: real"),
            "a check missing": block("next: checks", f"head: {HEAD}", "recordings: recyclarr", "compare: real",
                                     "replays: true"),
            "nothing asked": block("next: checks", f"head: {HEAD}", "recordings: none", "http_audit: false",
                                   "compare: none", "replays: false"),
            "odd compare": block("next: checks", f"head: {HEAD}", "recordings: none", "http_audit: true",
                                 "compare: $(id)", "replays: true"),
            "odd recording": block("next: checks", f"head: {HEAD}", "recordings: transmission;id", "http_audit: true",
                                   "compare: none", "replays: true"),
            "odd request": block("next: checks", f"head: {HEAD}", "recordings: none", "http_audit: true",
                                 "compare: none", "replays: true", "old_pin_request: http://evil/x"),
            "long wait": block("next: checks", f"head: {HEAD}", "recordings: none", "http_audit: true",
                               "compare: none", "replays: true", "old_pin_request: /api/v3/log", "old_pin_wait: 99999"),
            "too long": block("next: done", f"head: {HEAD}", *[""] * 20),
        }
        for name, body in cases.items():
            with self.subTest(case=name), self.assertRaises(SystemExit):
                port.parse_next(body)


class Ready(unittest.TestCase):
    def test_the_agent_starts_when_everything_is_set(self):
        go, message = port.ready_decision(port.load_checks().check_port_pr(pr(), "bateman/unarr"), True, lambda: [])
        self.assertTrue(go)
        self.assertIn("#28", message)

    def test_what_is_missing_is_said_and_nothing_is_asked_of_github_before_it(self):
        info = port.load_checks().check_port_pr(pr(), "bateman/unarr")
        asked = []

        def protection():
            asked.append(1)
            return ["no active ruleset on main has the rule \"Restrict updates\""]

        go, message = port.ready_decision(info, False, protection)
        self.assertFalse(go)
        self.assertIn("ANTHROPIC_API_KEY is not set", message)
        self.assertEqual(asked, [])
        go, message = port.ready_decision(info, True, protection)
        self.assertFalse(go)
        self.assertIn("main is not protected", message)
        self.assertIn("Restrict updates", message)
        not_draft = port.load_checks().check_port_pr(pr(isDraft=False), "bateman/unarr")
        self.assertFalse(port.ready_decision(not_draft, True, protection)[0])

    def test_ready_ends_green_with_its_outputs(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, out = Path(tmp) / "pr.json", Path(tmp) / "out"
            path.write_text(json.dumps(pr()))
            with mock.patch.dict(os.environ, {"HAS_KEY": "false", "GITHUB_STEP_SUMMARY": ""}), \
                    mock.patch("sys.stdout"):
                code = port.main(["ready", "--pr-json", str(path), "--pr", "28", "--repository", "bateman/unarr",
                                  "--github-output", str(out)])
            text = out.read_text()
        self.assertEqual(code, 0)
        self.assertIn("go=false\n", text)


class Next(unittest.TestCase):
    """What follows a run of the agent in upstream-loop.yml."""

    def decide(self, body, round_=1, result="success", checked=(), **over):
        info = port.load_checks().check_port_pr(pr(**over), "bateman/unarr")
        return port.next_decision(info, body, round_, result, list(checked))

    def test_the_checks_when_the_port_asks_for_them(self):
        got = self.decide(CHECKS_BLOCK)
        self.assertEqual((got["action"], got["failed"]), ("checks", False))
        self.assertEqual(got["checks"]["compare"], "real")

    def test_another_run_of_the_agent(self):
        self.assertEqual(self.decide(block("next: port", f"head: {HEAD}"), 2)["action"], "port")

    def test_done_stops_green(self):
        got = self.decide(block("next: done", f"head: {HEAD}"))
        self.assertEqual((got["action"], got["failed"]), ("stop", False))

    def test_a_run_that_did_not_end_well_stops_the_loop(self):
        for result in ("failure", "cancelled", ""):
            with self.subTest(result=result):
                got = self.decide(CHECKS_BLOCK, result=result)
                self.assertEqual((got["action"], got["failed"]), ("stop", False))
                self.assertIn("by hand with PR 28", got["message"])

    def test_a_pr_ready_for_review_stops_the_loop(self):
        self.assertEqual(self.decide(CHECKS_BLOCK, isDraft=False)["action"], "stop")

    def test_a_block_missing_out_of_shape_or_stale_stops_it_red(self):
        for body in ("no block", block("next: port"),
                     block("next: port", f"head: {OTHER}")):  # written before the last push
            with self.subTest(body=body):
                info = port.load_checks().check_port_pr(pr(), "bateman/unarr")
                try:
                    got = port.next_decision(info, body, 1, "success", [])
                except SystemExit:
                    continue  # out of shape: cmd_next turns it into a red stop
                self.assertEqual((got["action"], got["failed"]), ("stop", True))

    def test_the_last_round_only_says_what_is_asked(self):
        for body in (CHECKS_BLOCK, block("next: port", f"head: {HEAD}")):
            with self.subTest(body=body):
                got = self.decide(body, port.ROUNDS)
                self.assertEqual((got["action"], got["failed"]), ("stop", False))
                self.assertIn(f"{port.ROUNDS} runs of the agent is the limit", got["message"])

    def test_the_checks_never_run_twice_at_one_commit_in_a_run(self):
        got = self.decide(CHECKS_BLOCK, 2, checked=[HEAD])
        self.assertEqual(got["action"], "stop")

    def test_the_loop_is_bounded_whatever_the_agent_writes(self):
        """Every path through the rounds, whatever each run asks: at most
        ROUNDS runs of the agent and ROUNDS - 1 of the checks."""
        import itertools
        asks = [CHECKS_BLOCK, block("next: port", f"head: {HEAD}"), block("next: done", f"head: {HEAD}")]
        for path in itertools.product(asks, repeat=port.ROUNDS + 2):
            runs, checks, checked, round_ = 0, 0, [], 1
            for body in path:
                runs += 1
                got = self.decide(body, round_, checked=checked)
                if got["action"] == "stop":
                    break
                if got["action"] == "checks":
                    checks += 1
                    checked.append(HEAD)
                round_ += 1
            self.assertLessEqual(runs, port.ROUNDS)
            self.assertLessEqual(checks, port.ROUNDS - 1)

    def run_next(self, body, checked="", round_=1):
        with tempfile.TemporaryDirectory() as tmp:
            path, out = Path(tmp) / "pr.json", Path(tmp) / "out"
            path.write_text(json.dumps(pr(body=body)))
            with mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": ""}), mock.patch("sys.stdout"):
                code = port.main(["next", "--pr-json", str(path), "--pr", "28", "--repository", "bateman/unarr",
                                  "--round", str(round_), "--port-result", "success", "--checked", checked,
                                  "--github-output", str(out)])
            return code, dict(line.split("=", 1) for line in out.read_text().splitlines())

    def test_the_outputs_the_loop_reads(self):
        code, out = self.run_next(CHECKS_BLOCK, checked=OTHER)
        self.assertEqual(code, 0)
        self.assertEqual((out["action"], out["recordings"], out["http_audit"], out["compare"], out["replays"],
                          out["old_pin_request"], out["old_pin_wait"]),
                         ("checks", "recyclarr", "true", "real", "true", "", "0"))
        self.assertEqual(out["checked"], f"{OTHER},{HEAD}")
        code, out = self.run_next(block("next: done", f"head: {HEAD}"), checked=OTHER)
        self.assertEqual((code, out["action"], out["checked"], out["compare"]), (0, "stop", OTHER, "none"))

    def test_a_block_out_of_shape_fails_the_job_and_stops(self):
        code, out = self.run_next(block("next: checks", f"head: {HEAD}", "compare: real; id"))
        self.assertEqual((code, out["action"]), (1, "stop"))
        code, out = self.run_next("no block at all")
        self.assertEqual((code, out["action"]), (1, "stop"))

    def test_checked_holds_commits_only(self):
        with self.assertRaises(SystemExit):
            self.run_next(CHECKS_BLOCK, checked="main")


class Loop(unittest.TestCase):
    """upstream-loop.yml keeps the bounds and the rules the script assumes."""

    TEXT = (REPO / ".github" / "workflows" / "upstream-loop.yml").read_text()
    WORKFLOWS = REPO / ".github" / "workflows"

    def jobs(self, text: str) -> dict[str, str]:
        import re
        parts = re.split(r"\n  (?=[a-z][a-z0-9-]*:\n)", text.split("\njobs:\n", 1)[1])
        return {part.split(":", 1)[0].strip(): part for part in parts}

    def test_the_rounds_are_written_out_within_the_bound(self):
        import re
        jobs = self.jobs(self.TEXT)
        ports = sorted(j for j in jobs if re.fullmatch(r"port-\d+", j))
        checks = sorted(j for j in jobs if re.fullmatch(r"checks-\d+", j))
        nexts = sorted(j for j in jobs if re.fullmatch(r"next-\d+", j))
        self.assertEqual(ports, [f"port-{n}" for n in range(1, port.ROUNDS + 1)])
        self.assertEqual(nexts, [f"next-{n}" for n in range(1, port.ROUNDS + 1)])
        self.assertEqual(checks, [f"checks-{n}" for n in range(1, port.ROUNDS)])
        for n in range(1, port.ROUNDS + 1):
            self.assertIn(f"\n      round: {n}\n", jobs[f"port-{n}"])
            self.assertIn(f'ROUND: "{n}"', jobs[f"next-{n}"])
            self.assertIn(f"PORT_RESULT: ${{{{ needs.port-{n}.result }}}}", jobs[f"next-{n}"])
        # Nothing starts a run of a workflow but these calls: no dispatch, no
        # API call, no token that could.
        self.assertNotIn("actions: write", self.TEXT)
        self.assertNotIn("gh workflow", self.TEXT)
        self.assertNotIn("dispatches", self.TEXT)

    def test_a_round_without_checks_does_not_skip_the_rounds_after_it(self):
        # GitHub skips a job one of whose ancestors was skipped, unless its
        # condition names a status: every job after the first run does.
        for name, job in self.jobs(self.TEXT).items():
            if name in ("ready", "port-1"):
                continue
            with self.subTest(job=name):
                self.assertIn("if: ${{ !cancelled() && ", job)

    def test_each_job_has_its_permissions_and_a_timeout_or_a_called_workflow(self):
        for name, job in self.jobs(self.TEXT).items():
            with self.subTest(job=name):
                self.assertIn("permissions:", job)
                if "uses: ./.github/workflows/" in job:
                    self.assertNotIn("timeout-minutes", job)
                else:
                    self.assertIn("timeout-minutes:", job)
                    # The jobs that decide read, and run main's script.
                    self.assertIn("permissions: { contents: read, pull-requests: read }", job)
                    self.assertTrue("with: { ref: main, persist-credentials: false }" in job
                                    or "steps: *next-steps" in job)
                    self.assertNotIn("secrets.ANTHROPIC_API_KEY }}", job)

    def test_secrets_go_by_name_and_only_to_the_agent(self):
        for path in self.WORKFLOWS.glob("upstream*.yml"):
            text = path.read_text()
            with self.subTest(workflow=path.name):
                self.assertNotIn("secrets: inherit", text)
        for name, job in self.jobs(self.TEXT).items():
            if "ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}" in job:
                self.assertIn("uses: ./.github/workflows/upstream-port.yml", job, name)

    def test_no_expression_inside_a_run_block_and_actions_pinned(self):
        import re
        for path in ("upstream-loop.yml", "upstream-port.yml", "upstream-checks.yml"):
            text = (self.WORKFLOWS / path).read_text()
            inside, indent = False, 0
            for line in text.splitlines():
                stripped = line.lstrip()
                depth = len(line) - len(stripped)
                if inside and stripped and depth <= indent:
                    inside = False
                if inside:
                    self.assertNotIn("${{", line, f"{path}: {line}")
                if stripped.startswith(("run:", "- run:")):
                    self.assertNotIn("${{", line, f"{path}: {line}")
                    # A block scalar runs on while deeper than the key.
                    inside = stripped.rstrip().endswith(("|", ">", "|-", ">-"))
                    indent = depth + (2 if stripped.startswith("- ") else 0)
            for ref in re.findall(r"uses:\s*([^\s#]+)", text):
                if not ref.startswith("./"):
                    self.assertRegex(ref, r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)*@[0-9a-f]{40}$", f"{path}: {ref}")

    def test_the_groups_of_a_caller_and_of_what_it_calls_differ(self):
        import re
        groups = {}
        for path in ("upstream-loop.yml", "upstream-port.yml", "upstream-checks.yml"):
            text = (self.WORKFLOWS / path).read_text()
            found = re.findall(r"(?m)^  group: (.+)$", text)
            self.assertEqual(len(found), 1, path)
            groups[path] = found[0]
        self.assertEqual(len(set(groups.values())), 3, groups)
        for path, group in groups.items():
            self.assertIn("${{ inputs.pr }}", group, path)
        # A caller job holds no group: the called workflow's own is enough.
        for name, job in self.jobs(self.TEXT).items():
            if "uses: ./" in job:
                self.assertNotIn("concurrency", job, name)

    def test_the_weekly_workflow_starts_it_only_on_the_pr_it_opened(self):
        text = (self.WORKFLOWS / "upstream.yml").read_text()
        for app in ("radarr", "sonarr"):
            job = self.jobs(text)[f"loop-{app}"]
            self.assertIn(f"if: needs.publish-{app}.outputs.opened == 'true'", job)
            self.assertIn("uses: ./.github/workflows/upstream-loop.yml", job)
            self.assertIn("ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}", job)
            self.assertIn(f"opened: ${{{{ steps.pr.outputs.opened }}}}", self.jobs(text)[f"publish-{app}"])

    def test_the_makefile_has_the_targets(self):
        import re
        makefile = (REPO / "Makefile").read_text()
        for target in set(re.findall(r"make -s ([a-z][a-z-]*)", self.TEXT)):
            self.assertRegex(makefile, rf"(?m)^{target}:", target)


if __name__ == "__main__":
    unittest.main()
