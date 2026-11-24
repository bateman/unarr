"""Tests of scripts/upstream-workflow.py, the decisions of the workflow
`upstream` (issue #9): on made-up data and local git repositories, no
network, no GitHub. Run with `make test-scripts` (part of `make check`)."""

from __future__ import annotations

import contextlib
import importlib.util
import io
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


def load(name: str, file: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / file)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


wf = sys.modules.get("upstream_workflow") or load("upstream_workflow", "upstream-workflow.py")

REPO_SLUG = "bateman/unarr"


def report(target="7.0.3.10950", jump="major", cursor="6.4.4.10600"):
    """A triage report as scripts/upstream-triage.py makes it."""
    return {
        "app": "radarr", "branch": "master", "cursor": "a96bf7c3ecb23cb76993770727285dc17a613fbc",
        "cursor_release": cursor, "target": target, "target_tag": f"v{target}" if target else None,
        "jump": jump if target else "none", "newest": target, "releases_from": "api", "merges": 0,
        "compare": f"https://github.com/Radarr/Radarr/compare/a96bf7c3ecb2...v{target}" if target else "",
        "summary": {"wontport": 0, "schema": 8, "routes": 17, "backend": 47, "ui": 15, "test": 2, "unknown": 0,
                    "none": 20},
        "migrations": [243], "migrations_removed": [], "warnings": [],
        "commits": [{"sha": "0" * 40, "class": "none"}] * 109,
    }


def pull(number, ref, repo=REPO_SLUG, draft=True):
    """An open pull request as GET /repos/{repo}/pulls lists it."""
    return {"number": number, "draft": draft, "title": "x",
            "head": {"ref": ref, "repo": {"full_name": repo}}}


# The case of today on main, with made-up versions (a pinned one may be written only in its pin):
# the cursor at 6.4 and #28 open from the branch of the release the triage chooses.
PR_28 = pull(28, "radarr/upstream-7.0.3.10950")


class Decide(unittest.TestCase):
    def decide(self, rep, pulls=(), branches=(), event="schedule", scheduled=("radarr",), target_given=False,
               app="radarr"):
        return wf.decide(app, event, rep, list(pulls), list(branches), REPO_SLUG, list(scheduled), target_given)

    def test_today_radarr_ends_on_the_port_already_open(self):
        for event in ("schedule", "workflow_dispatch"):
            plan = self.decide(report(), [PR_28, pull(43, "core/porting-after-radarr-trial")],
                               ["radarr/upstream-7.0.3.10950"], event=event)
            self.assertEqual(plan["action"], "open")
            self.assertEqual(plan["pr"], 28)
            self.assertTrue(plan["message"].startswith("port already open: #28"), plan["message"])

    def test_a_port_toward_another_release_still_holds_the_personality(self):
        plan = self.decide(report("7.0.4.11000", "major"), [PR_28])
        self.assertEqual(plan["action"], "open")
        self.assertEqual(plan["pr"], 28)
        self.assertIn("never in a second pull request", plan["message"])

    def test_a_port_of_the_other_personality_or_of_a_fork_does_not_count(self):
        plan = self.decide(report("6.4.5.10700", "patch"),
                           [pull(5, "sonarr/upstream-4.0.21.3100"), pull(6, "radarr/upstream-6.4.5.10700", "x/fork")])
        self.assertEqual(plan["action"], "bump")

    def test_no_newer_release_does_nothing(self):
        plan = self.decide(report(target=None), [PR_28])
        self.assertEqual(plan["action"], "none")
        self.assertIn("nothing to do", plan["message"])

    def test_a_branch_left_without_its_pull_request_is_not_pushed_over(self):
        plan = self.decide(report("6.4.5.10700", "patch"), [], ["radarr/upstream-6.4.5.10700"])
        self.assertEqual(plan["action"], "branch")
        self.assertEqual(plan["branch"], "radarr/upstream-6.4.5.10700")

    def test_a_major_on_the_schedule_is_announced_and_not_ported(self):
        self.assertEqual(self.decide(report())["action"], "announce")

    def test_a_major_started_by_hand_is_bumped(self):
        self.assertEqual(self.decide(report(), event="workflow_dispatch")["action"], "bump")
        self.assertEqual(self.decide(report(), event="workflow_dispatch", target_given=True)["action"], "bump")

    def test_a_patch_or_a_minor_on_the_schedule_is_bumped(self):
        for target, jump in (("6.4.5.10700", "patch"), ("6.5.0.10800", "minor")):
            plan = self.decide(report(target, jump))
            self.assertEqual(plan["action"], "bump")
            self.assertEqual(plan["branch"], f"radarr/upstream-{target}")
            self.assertEqual(plan["jump"], jump)

    def test_a_personality_off_for_the_schedule_runs_only_by_hand(self):
        plan = self.decide(None, app="sonarr")
        self.assertEqual(plan["action"], "disabled")
        rep = dict(report("4.0.21.3100", "patch", "4.0.20.3000"), app="sonarr")
        rep["target_tag"] = "v4.0.21.3100"
        self.assertEqual(self.decide(rep, app="sonarr", event="workflow_dispatch")["action"], "bump")

    def test_the_branch_name_follows_claude_md(self):
        self.assertEqual(wf.branch_name("radarr", "7.0.3.10950"), "radarr/upstream-7.0.3.10950")
        for app, target in (("radarr", "7.0.3"), ("lidarr", "1.0.0.1"), ("radarr", "7.0.3.1; rm -rf /")):
            with self.assertRaises(SystemExit):
                wf.branch_name(app, target)


class Plan(unittest.TestCase):
    """`plan` end to end, the triage and GitHub given as files."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="upstream-workflow-test-"))
        self.addCleanup(shutil.rmtree, self.tmp)

    def run_plan(self, rep, pulls, branches, event="schedule", target="", scheduled="radarr", app="radarr"):
        (self.tmp / "pulls.json").write_text(json.dumps(pulls))
        (self.tmp / "branches.json").write_text(json.dumps(branches))
        output = self.tmp / "github-output"
        output.write_text("")
        out = io.StringIO()
        with mock.patch.object(wf, "run_triage", return_value=(rep, "## triage\n")) as triage, \
                mock.patch.dict(os.environ, {"GITHUB_REPOSITORY": REPO_SLUG, "GITHUB_STEP_SUMMARY": ""}), \
                contextlib.redirect_stdout(out):
            wf.main(["plan", "--app", app, "--event", event, "--target", target, "--scheduled", scheduled,
                     "--out", str(self.tmp / "out"), "--pulls", str(self.tmp / "pulls.json"),
                     "--branches", str(self.tmp / "branches.json"), "--github-output", str(output)])
        outputs = dict(line.split("=", 1) for line in output.read_text().splitlines())
        return json.loads((self.tmp / "out" / "plan.json").read_text()), outputs, triage

    def test_today_on_radarr_the_run_is_green_and_says_28(self):
        plan, outputs, _ = self.run_plan(report(), [PR_28], ["radarr/upstream-7.0.3.10950"])
        self.assertEqual(outputs["action"], "open")
        self.assertEqual(outputs["pr"], "28")
        self.assertEqual(outputs["message"], "port already open: #28 (radarr/upstream-7.0.3.10950)")
        self.assertEqual(plan["triage"]["commits"], 109)

    def test_sonarr_off_for_the_schedule_runs_no_triage(self):
        plan, outputs, triage = self.run_plan(None, [], [], app="sonarr")
        self.assertEqual(outputs["action"], "disabled")
        triage.assert_not_called()

    def test_a_target_by_hand_must_be_a_version(self):
        for bad in ("7.0.3", "v7.0.3.10950", "7.0.3.1$(id)"):
            with self.assertRaises(SystemExit):
                self.run_plan(report(), [], [], event="workflow_dispatch", target=bad)

    def test_an_unknown_personality_in_the_switch_stops(self):
        with self.assertRaises(SystemExit):
            self.run_plan(report(), [], [], scheduled="radarr,lidarr")

    def test_outputs_are_one_line_each(self):
        rep = report("6.4.5.10700", "patch")
        _, outputs, _ = self.run_plan(rep, [], [])
        self.assertEqual(outputs["action"], "bump")
        self.assertEqual(outputs["branch"], "radarr/upstream-6.4.5.10700")
        self.assertEqual(wf.one_line("a\nb\r\x1b[31mc"), "a b [31mc")


class Texts(unittest.TestCase):
    def plan(self, **over):
        plan = wf.decide("radarr", "workflow_dispatch", report(), [], [], REPO_SLUG, [], False)
        plan["triage"] = wf.triage_facts(report())
        plan.update(over)
        return wf.check_plan(plan)

    def test_the_issue_of_a_major_names_the_release_and_counts_no_subject(self):
        plan = self.plan(action="announce", event="schedule")
        title, body = wf.render_issue(plan)
        self.assertEqual(title, "Radarr 7.0.3.10950 is out")
        self.assertIn("109 upstream commits", body)
        self.assertIn("8 schema, 17 routes, 47 backend, 15 ui, 2 test, 20 none", body)
        plan["triage"]["summary"] = dict(sorted(plan["triage"]["summary"].items()))
        self.assertIn("8 schema, 17 routes, 47 backend, 15 ui, 2 test, 20 none", wf.render_issue(plan)[1])
        self.assertIn("Migrations added: 243.", body)
        self.assertIn("https://github.com/Radarr/Radarr/compare/a96bf7c3ecb2...v7.0.3.10950", body)
        self.assertNotIn("@", body)

    def test_an_open_issue_of_the_same_title_is_not_opened_again(self):
        issues = [{"number": 3, "title": "Radarr 7.0.3.10950 is out", "pull_request": {}},
                  {"number": 50, "title": "Radarr 7.0.3.10950 is out"}]
        self.assertEqual(wf.find_open_issue(issues, "Radarr 7.0.3.10950 is out"), 50)
        self.assertIsNone(wf.find_open_issue(issues[:1], "Radarr 7.0.3.10950 is out"))

    SUMMARY = {"app": "radarr", "to": {"version": "7.0.3.10950", "tag": "v7.0.3.10950",
                                       "commit": "af772b051ac27a0615c7f8b7f78622b6dc183be0"},
               "recordings": {"files": 908, "changed": 256, "failed_lists": []},
               "tests": {"passed": 792, "failed": 10, "failures": [],
                         "scripts_failures": [{"name": "test_the_shipped_cursors_are_at_the_pins", "target": "x"}]},
               "importer": {"migration": 243, "known": 242}, "vectors": {"changed": 2, "deleted": []}}

    def test_the_pull_request_opens_with_the_short_body_of_porting(self):
        title, body = wf.render_pr(self.plan(), self.SUMMARY, "242", "https://github.com/bateman/unarr/actions/runs/1",
                                   own_token=True)
        self.assertEqual(title, "Radarr 6.4.4.10600 → 7.0.3.10950")
        self.assertTrue(body.startswith("## Release\n"))
        self.assertIn("a **major** jump. Tag `v7.0.3.10950` at `af772b051ac27a0615c7f8b7f78622b6dc183be0`.", body)
        self.assertIn("908 recordings, 256 changed beyond the instants", body)
        self.assertIn("`cargo test --workspace` 792 passed, 10 failed", body)
        self.assertIn("`make test-scripts` failed on `test_the_shipped_cursors_are_at_the_pins`", body)
        self.assertIn("migration level 242 → 243", body)
        self.assertIn("comes with the first update", body)
        self.assertNotIn("CI has not run", body)
        for word in ("Claude", "claude", "Co-authored", "Generated", "agent"):
            self.assertNotIn(word, title + body)

    def test_with_the_job_token_the_body_says_ci_has_not_run(self):
        _, body = wf.render_pr(self.plan(), self.SUMMARY, "242", "", own_token=False)
        self.assertIn("CI has not run", body)

    def test_odd_values_of_the_summary_are_left_out(self):
        summary = dict(self.SUMMARY, recordings={"files": "[x](https://e.vil)", "changed": 1, "failed_lists": []},
                       tests={"passed": 1, "failed": 0, "scripts_failures": [{"name": "[a](b) @someone"}]},
                       to={"commit": "not a sha"})
        _, body = wf.render_pr(self.plan(), summary, "242", "", own_token=True)
        self.assertNotIn("e.vil", body)
        self.assertNotIn("@someone", body)
        self.assertNotIn("not a sha", body)

    def test_a_long_summary_is_cut_for_the_comment(self):
        comment = wf.render_comment(self.plan(), "line\n" * 30000)
        self.assertLessEqual(len(comment), wf.COMMENT_LIMIT)
        self.assertIn("artifact `bump-radarr`", comment)

    def test_a_plan_with_odd_values_is_refused(self):
        for over in ({"target": "7.0.3.10950; id"}, {"jump": "huge"}, {"branch": "main"}, {"app": "lidarr"},
                     {"action": "merge"}):
            with self.assertRaises(SystemExit):
                self.plan(**over)


class Files(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="upstream-workflow-test-"))
        self.addCleanup(shutil.rmtree, self.tmp)

    def test_only_the_files_the_bump_writes_may_be_committed(self):
        ok = ["upstream/radarr.pin", "tests/fixtures/radarr/golden/system-status.json",
              "tests/fixtures/radarr/golden-hub/requests.txt", "crates/unarr-api/schemas/radarr/indexer-schema.json",
              "tests/vectors/radarr/parser/quality.json", "tests/vectors/radarr/decisionengine/x.json"]
        for path in ok:
            self.assertTrue(wf.allowed_path("radarr", path), path)
        bad = ["upstream/radarr.lock", "upstream/sonarr.pin", ".github/workflows/ci.yml", "Makefile",
               "tests/fixtures/radarr/radarr.db", "tests/fixtures/radarr/golden/sub/x.json",
               "tests/fixtures/sonarr/golden/system-status.json", "tests/vectors/sonarr-only.json",
               "crates/unarr-import/src/radarr.rs", "tests/vectors/radarr/parser/../../../Makefile"]
        for path in bad:
            self.assertFalse(wf.allowed_path("radarr", path), path)
        self.assertTrue(wf.allowed_path("sonarr", "tests/vectors/parser/quality.json"))
        self.assertFalse(wf.allowed_path("sonarr", "tests/vectors/radarr/parser/quality.json"))

    def test_the_fetched_files_win_over_what_the_recording_job_left(self):
        trusted, untrusted, work = self.tmp / "fetched", self.tmp / "recorded", self.tmp / "work"
        trusted.mkdir()
        (untrusted / "recorded" / "golden").mkdir(parents=True)
        (untrusted / "original").mkdir()
        (untrusted / "original" / "Radarr.tar.gz").write_text("binary")
        for name in wf.TRUSTED:
            (trusted / name).write_text(f"trusted {name}")
        (untrusted / "record.json").write_text("{}")
        (untrusted / "triage.md").write_text("from the original")
        outside = self.tmp / "outside.json"
        outside.write_text("untouched")
        (untrusted / "bump.json").symlink_to(outside)
        (untrusted / "recorded" / "golden" / "link.json").symlink_to(outside)
        wf.main(["assemble", "--trusted", str(trusted), "--untrusted", str(untrusted), "--work", str(work)])
        for name in wf.TRUSTED:
            self.assertFalse((work / name).is_symlink())
            self.assertEqual((work / name).read_text(), f"trusted {name}")
        self.assertEqual(outside.read_text(), "untouched")
        self.assertTrue((work / "recorded" / "golden" / "link.json").is_symlink())
        self.assertTrue((work / "record.json").exists())
        self.assertFalse((work / "original").exists())

    def test_the_pin_values_are_read_by_key(self):
        text = (REPO / "upstream" / "radarr.pin").read_text()
        self.assertRegex(wf.pin_value(text, "version"), r"^\d+\.\d+\.\d+\.\d+$")
        self.assertRegex(wf.pin_value(text, "migration"), r"^\d+$")

    def test_changed_paths_lists_new_and_modified_files(self):
        repo = self.tmp / "repo"
        repo.mkdir()
        run = lambda *a: subprocess.run(["git", "-C", str(repo), *a], check=True, capture_output=True)  # noqa: E731
        run("init", "-q")
        (repo / "a.txt").write_text("a")
        run("add", "a.txt")
        run("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "a")
        (repo / "a.txt").write_text("b")
        (repo / "dir").mkdir()
        (repo / "dir" / "new file.json").write_text("{}")
        with mock.patch.object(wf, "REPO", repo):
            self.assertEqual(wf.changed_paths(), ["a.txt", "dir/new file.json"])


class Workflow(unittest.TestCase):
    """The workflow file keeps the rules of issue #9 it was written to."""

    TEXT = (REPO / ".github" / "workflows" / "upstream.yml").read_text()

    def test_third_party_actions_are_pinned_to_a_commit(self):
        import re
        uses = re.findall(r"uses:\s*([^\s#]+)", self.TEXT)
        self.assertTrue(uses)
        for ref in uses:
            self.assertRegex(ref, r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@[0-9a-f]{40}$", ref)

    def test_no_expression_inside_a_run_block(self):
        lines = self.TEXT.splitlines()
        inside, indent = False, 0
        for line in lines:
            stripped = line.lstrip()
            depth = len(line) - len(stripped)
            if inside and stripped and depth <= indent:
                inside = False
            if inside:
                self.assertNotIn("${{", line, line)
            if stripped.startswith("run:") or stripped.startswith("- run:"):
                self.assertNotIn("${{", line, line)
                inside, indent = True, depth

    def test_every_job_has_a_timeout_and_its_own_permissions(self):
        import re
        jobs = re.split(r"\n  (?=[a-z][a-z0-9-]*:\n)", self.TEXT.split("\njobs:\n", 1)[1])
        names = []
        for job in jobs:
            name = job.split(":", 1)[0].strip()
            names.append(name)
            self.assertIn("timeout-minutes:", job, name)
            self.assertIn("permissions:", job, name)
        self.assertIn("record-radarr", names)
        record = next(j for j in jobs if j.startswith("record-radarr"))
        self.assertIn("permissions: {}", record)
        self.assertNotIn("secrets.", record)
        self.assertNotIn("github.token", record)

    def test_the_makefile_has_the_targets_the_workflow_calls(self):
        import re
        makefile = (REPO / "Makefile").read_text()
        for target in set(re.findall(r"make ([a-z][a-z-]*)-\"\$APP\"", self.TEXT)):
            for app in ("sonarr", "radarr"):
                self.assertRegex(makefile, rf"(?m)^{target}-{app}:", target)


if __name__ == "__main__":
    unittest.main()
