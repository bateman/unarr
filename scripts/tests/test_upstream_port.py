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


if __name__ == "__main__":
    unittest.main()
