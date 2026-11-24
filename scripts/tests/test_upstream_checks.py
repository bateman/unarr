"""Tests of scripts/upstream-checks.py, on made-up data and the repository's
own files: no network, no original run. Run with `make test-scripts` (part of
`make check`)."""

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

SCRIPTS = Path(__file__).resolve().parent.parent
REPO = SCRIPTS.parent

_spec = importlib.util.spec_from_file_location("upstream_checks", SCRIPTS / "upstream-checks.py")
checks = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = checks
_spec.loader.exec_module(checks)

HEAD = "a" * 40


def pr(**over):
    base = {"number": 28, "state": "OPEN", "isDraft": True, "baseRefName": "main",
            "headRefName": "radarr/upstream-9.1.2.3456", "headRefOid": HEAD, "isCrossRepository": False,
            "headRepository": {"name": "unarr"}, "headRepositoryOwner": {"login": "bateman"},
            "labels": [{"name": "major"}], "title": "Radarr 8.0.0.1 → 9.1.2.3456"}
    base.update(over)
    return base


class ThePR(unittest.TestCase):
    def test_a_port_pr_gives_its_app_branch_and_head(self):
        got = checks.check_port_pr(pr(), "bateman/unarr")
        self.assertEqual((got["app"], got["branch"], got["head"], got["version"], got["major"], got["draft"]),
                         ("radarr", "radarr/upstream-9.1.2.3456", HEAD, "9.1.2.3456", True, True))

    def test_what_is_not_a_port_pr_is_refused(self):
        cases = {
            "another branch": pr(headRefName="core/upstream-workflow"),
            "a branch with a tail": pr(headRefName="radarr/upstream-9.1.2.3456-x"),
            "a name to inject": pr(headRefName="radarr/upstream-9.1.2.3456;rm"),
            "closed": pr(state="CLOSED"),
            "not into main": pr(baseRefName="develop"),
            "a fork": pr(isCrossRepository=True),
            "another repository": pr(headRepositoryOwner={"login": "someone"}),
            "no head": pr(headRefOid="abc"),
        }
        for what, case in cases.items():
            with self.subTest(what), self.assertRaises(checks.ChecksError):
                checks.check_port_pr(case, "bateman/unarr")


class TheInputs(unittest.TestCase):
    def test_every_check(self):
        got = checks.parse_checks("recyclarr", "true", "both", "true", "/api/v3/log?page=1&pageSize=50", "30")
        self.assertEqual(got, {"recordings": ["recyclarr"], "old_pin": {"request": "/api/v3/log?page=1&pageSize=50",
                                                                         "wait": 30},
                               "http_audit": True, "compare": ["real", "scaled"], "replays": True})

    def test_none_and_empty_mean_no_recording(self):
        for value in ("", "none", " , "):
            with self.subTest(value=value):
                got = checks.parse_checks(value, "true", "none", "false", "", "0")
                self.assertEqual(got["recordings"], [])
                self.assertIsNone(got["old_pin"])

    def test_inputs_that_are_refused(self):
        cases = [
            ("recyclarr,unknown", "false", "none", "false", "", "0"),
            ("recyclarr,recyclarr", "false", "none", "false", "", "0"),
            ("", "maybe", "none", "false", "", "0"),
            ("", "false", "everything", "false", "", "0"),
            ("", "false", "none", "false", "/api/v3/log; curl x", "0"),
            ("", "false", "none", "false", "http://example.invalid/api/v3/log", "0"),
            ("", "false", "none", "false", "/api/v3/log", "-1"),
            ("", "false", "none", "false", "/api/v3/log", "100000"),
            ("", "false", "none", "false", "", "0"),  # nothing asked
        ]
        for case in cases:
            with self.subTest(case=case), self.assertRaises(checks.ChecksError):
                checks.parse_checks(*case)

    def test_the_plan_prints_the_outputs(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "pr.json"
            path.write_text(json.dumps(pr()))
            import contextlib
            import io
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                checks.main(["plan", "--pr-json", str(path), "--pr", "28", "--repository", "bateman/unarr",
                             "--recordings", "recyclarr", "--http-audit", "true"])
        out = dict(line.split("=", 1) for line in buf.getvalue().splitlines())
        self.assertEqual(out["app"], "radarr")
        self.assertEqual(out["head"], HEAD)
        self.assertEqual(json.loads(out["checks"])["recordings"], ["recyclarr"])


class TheRecordings(unittest.TestCase):
    def test_their_paths_exist_for_what_is_automated(self):
        for app in checks.APPS:
            path = REPO / checks.recording_path(checks.RECORDINGS["recyclarr"], app)
            self.assertTrue(path.is_file(), path)
        self.assertTrue((REPO / checks.recording_path(checks.RECORDINGS["transmission"], "radarr")).is_file())

    def test_only_a_recording_whose_test_compares_answers_is_committed(self):
        self.assertFalse(checks.RECORDINGS["recyclarr"].follows_ids)
        self.assertTrue(checks.RECORDINGS["transmission"].follows_ids)
        self.assertTrue(checks.RECORDINGS["sabnzbd"].follows_ids)

    def test_recyclarr_as_the_fixtures_readme_names_it(self):
        readme = (REPO / "tests/fixtures/recyclarr/README.md").read_text()
        for value in (checks.RECYCLARR_VERSION, checks.RECYCLARR_SHA256, checks.TRASH_GUIDES_COMMIT):
            self.assertIn(value, readme)
        self.assertIn(checks.RECYCLARR_ASSET.split(".")[0], readme)


REPORT = """Library `real`; Unarr `ff4c366, sha256 8b64ade6cabf`; Sonarr 1.2.3.4, Radarr 5.6.7.8; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; **warm-up not settled for originals and unarr: raise --warmup**; 5 warm-up passes; 2026-11-24.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 2.79 s | 0.20 s | 13.7× sooner |
| Idle, at the end of the idle window: RSS | 527 MiB | 31 MiB | 94 % |
| Same, anonymous memory | 300 MiB | 11 MiB | 96 % |
| Idle CPU, from minute 5 of the window | 22.0 s/h | 1.1 s/h | 95 % |
| The reads: CPU | 2.58 s | 0.86 s | 67 % |
| The reads: time | 2.50 s | 0.95 s | 62 % |
| Peak anonymous memory during the reads | 384 MiB | 42 MiB | 89 % |
| Anonymous memory at the end of the tail | 384 MiB | 33 MiB | 91 % |
| Idle CPU in the tail, from its first minute | 7.0 s/h | 0.7 s/h | 89 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 7.5 / 15.6 | 3.1 / 4.5 |
| Radarr /api/v3/movie | 20.9 / 26.1 | 18.8 / 22.4 |
| Radarr /api/v3/queue | 1.2 / 1.8 | 0.6 / 0.8 |
"""


class TheComparison(unittest.TestCase):
    def test_a_report_as_compare_originals_writes_it(self):
        r = checks.parse_report(REPORT, "real")
        self.assertEqual((r["unarr"], r["date"], r["runs"], r["warmup"]), ("ff4c366", "2026-11-24", 1, True))
        self.assertEqual(r["summary"]["Idle, at the end of the idle window: RSS"], ("527 MiB", "31 MiB", "94 %"))
        self.assertEqual(len(r["latency"]), 3)

    def test_several_runs(self):
        text = REPORT.replace("5 warm-up passes; ", "5 warm-up passes; 3 runs per side, median (lowest–highest); ")
        text = text.replace("| 527 MiB |", "| 527 MiB (520–530) |").replace("| 7.5 / 15.6 |", "| 7.5 (7.0–8.0) / 15.6 |")
        r = checks.parse_report(text, "real")
        self.assertEqual(r["runs"], 3)
        self.assertEqual(checks.strip_range(r["summary"]["Idle, at the end of the idle window: RSS"][0]), "527 MiB")

    def test_a_report_that_is_not_the_measurements_is_refused(self):
        cases = {
            "another library": REPORT.replace("Library `real`", "Library `scaled`"),
            "a link": REPORT.replace("2026-11-24.", "[x](http://example.invalid) 2026-11-24."),
            "a mention": REPORT.replace("idle 900 s", "@someone 900 s"),
            "a cell": REPORT.replace("| 31 MiB |", "| lots |"),
            "a row added": REPORT.replace("| Radarr /api/v3/queue", "| <script> |\n| Radarr /api/v3/queue"),
            "a route": REPORT.replace("Radarr /api/v3/queue", "Radarr /api/v3/queue)[x](y"),
            "no latency": REPORT.split("\n\n| p50")[0] + "\n",
        }
        for what, text in cases.items():
            with self.subTest(what), self.assertRaises(checks.ChecksError):
                checks.parse_report(text, "real")

    def test_the_document_gets_the_tables_the_list_and_a_history_row(self):
        doc = (REPO / "docs/originals-comparison.md").read_text()
        r = checks.parse_report(REPORT, "real")
        new = checks.update_comparison(doc, r, "a note")
        self.assertIn(REPORT.split("\n")[0], new)
        self.assertIn("| Radarr /api/v3/queue | 1.2 / 1.8 | 0.6 / 0.8 |", new)
        self.assertIn("| 2026-11-24 | real | `ff4c366` | 1 | 527 MiB / 22.0 s/h | 31 MiB / 1.1 s/h | a note |", new)
        self.assertIn("On the real library, none, at p50 or at the maximum: Radarr's `/movie` comes closest, 18.8 ms "
                      "at p50 against 20.9.", new.replace("\n", " "))
        # The scaled library and everything else are as they were.
        def section(text, start, end):
            return text[text.index(start):text.index(end)]
        self.assertEqual(section(doc, "### Scaled library", "Where Unarr is slower"),
                         section(new, "### Scaled library", "Where Unarr is slower"))
        self.assertEqual(doc[:doc.index("### Real library")], new[:new.index("### Real library")])
        self.assertIn(section(doc, "On the scaled library", "\n\n"), new)
        self.assertEqual(doc[doc.index("## History"):].count("\n") + 1, new[new.index("## History"):].count("\n"))
        self.assertEqual(new.count("| p50 / max, ms |"), doc.count("| p50 / max, ms |"))

    def test_routes_where_unarr_is_slower(self):
        text = REPORT.replace("| 0.6 / 0.8 |", "| 1.5 / 0.8 |").replace("| 3.1 / 4.5 |", "| 3.1 / 16.0 |")
        r = checks.parse_report(text, "real")
        found, _ = checks.slower(r)
        self.assertEqual(found, ["Sonarr's `/series` at the maximum (16.0 ms against 15.6)",
                                 "Radarr's `/queue` at p50 (1.5 ms against 1.2)"])
        self.assertTrue(checks.slower_sentence(r).startswith("On the real library, measured at `ff4c366`: Sonarr's"))
        self.assertIn("2 slower than the original's", checks.history_note(r, "radarr", "9.1.2.3456", 28))


class TheAudit(unittest.TestCase):
    def test_how_it_ended_is_read_from_its_summary(self):
        diff = "x: GET /x\n    status 500 -> 200\n324 requests, 2 with a difference (original -> Unarr), 2 more\n" \
               "make: *** [Makefile:150: http-audit] Error 1\n"
        self.assertEqual(checks.audit_result(2, diff), {"status": "differences", "exit": 2, "requests": 324,
                                                        "different": 2})
        self.assertEqual(checks.audit_result(0, "3 requests, 0 with a difference\n")["status"], "ok")
        self.assertEqual(checks.audit_result(2, "Traceback: ConnectionRefusedError\n")["status"], "failed")
        # An error after a clean summary is not a clean audit.
        self.assertEqual(checks.audit_result(2, "3 requests, 0 with a difference\n")["status"], "failed")


class TheArtifact(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.art = self.tmp / "artifact"
        self.art.mkdir()
        (self.art / "report.json").write_text(json.dumps({"app": "radarr", "head": HEAD, "version": "9.1.2.3456",
                                                          "checks": {}}))

    def put(self, rel: str, data: bytes):
        path = self.art / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def test_the_files_it_expects(self):
        self.put("recordings/tests/fixtures/recyclarr/radarr-exchange.json", b"[]")
        self.put("http-audit.txt", b"3 requests, 0 with a difference\n")
        self.put("compare/real/report.md", REPORT.encode())
        files = checks.checked_artifact(self.art, "radarr")
        self.assertEqual(sorted(files), ["compare/real/report.md", "http-audit.txt", "recordings/tests/fixtures/"
                                         "recyclarr/radarr-exchange.json", "report.json"])

    def test_what_stops_it(self):
        cases = {
            "another app's recording": ("recordings/tests/fixtures/recyclarr/sonarr-exchange.json", b"[]"),
            "a file elsewhere in the tree": ("recordings/crates/unarr-api/src/lib.rs", b"x"),
            "a workflow": ("recordings/.github/workflows/ci.yml", b"x"),
            "a recording that is not JSON": ("recordings/tests/fixtures/recyclarr/radarr-exchange.json", b"{"),
            "a report that is not UTF-8": ("http-audit.txt", b"\xff\xfe"),
            "an unknown file": ("notes.txt", b"x"),
        }
        for what, (rel, data) in cases.items():
            with self.subTest(what):
                path = self.put(rel, data)
                with self.assertRaises(checks.ChecksError):
                    checks.checked_artifact(self.art, "radarr")
                path.unlink()

    def test_a_link_stops_it(self):
        target = self.tmp / "secret"
        target.write_text("x")
        link = self.art / "http-audit.txt"
        link.symlink_to(target)
        with self.assertRaises(checks.ChecksError):
            checks.checked_artifact(self.art, "radarr")

    def test_a_file_too_large_stops_it(self):
        self.put("http-audit.txt", b"x" * (checks.MAX_TEXT + 1))
        with self.assertRaises(checks.ChecksError):
            checks.checked_artifact(self.art, "radarr")

    def test_the_report_is_checked_field_by_field(self):
        good = {"app": "radarr", "head": HEAD, "version": "9.1.2.3456",
                "checks": {"http-audit": {"status": "differences", "exit": 1, "requests": 3, "different": 1},
                           "replays": {"status": "ok", "exits": {"bazarr": 0, "prowlarr": 0, "recyclarr": 0}}}}
        self.assertEqual(checks.check_report(good, "radarr", HEAD)["checks"]["http-audit"]["different"], 1)
        bad = [
            {**good, "app": "sonarr"},
            {**good, "head": "b" * 40},
            {**good, "version": "9.1.2.3456 [x](y)"},
            {**good, "checks": {"rm -rf": {"status": "ok"}}},
            {**good, "checks": {"http-audit": {"status": "`fine`"}}},
            {**good, "checks": {"http-audit": {"status": "ok", "requests": "3 @someone"}}},
            {**good, "checks": {"replays": {"status": "ok", "exits": {"bazarr": "0"}}}},
        ]
        for case in bad:
            with self.subTest(case=case), self.assertRaises(checks.ChecksError):
                checks.check_report(case, "radarr", HEAD)

    def test_text_from_the_originals_cannot_leave_its_code_block(self):
        out = checks.fence("```\n@someone #1 [x](http://example.invalid)\x1b[31m")
        self.assertTrue(out.startswith("```text\n") and out.endswith("\n```"))
        self.assertEqual(out.count("```"), 2)
        self.assertNotIn("\x1b", out)

    def test_the_comment_stays_within_githubs_limit(self):
        result = {"commits": [], "kept": [], "rows": [],
                  "details": [(f"part {i}", ("y" * 100 + "\n") * 79) for i in range(12)]}
        text = checks.render_comment(result, "radarr", "9.1.2.3456", HEAD, "")
        self.assertLessEqual(len(text), checks.MAX_COMMENT + 200)
        self.assertIn("in the artifact `upstream-checks-results` (too long", text)


class Applying(unittest.TestCase):
    """apply on a throwaway repository: what it commits, and what it leaves."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.repo = self.tmp / "repo"
        rec = self.repo / "tests/fixtures/recyclarr/radarr-exchange.json"
        rec.parent.mkdir(parents=True)
        rec.write_text("[1]\n")
        (self.repo / "upstream").mkdir()
        (self.repo / "upstream/radarr.pin").write_text("version = 9.1.2.3456\n")
        (self.repo / "docs").mkdir()
        shutil.copyfile(REPO / "docs/originals-comparison.md", self.repo / "docs/originals-comparison.md")
        env = dict(os.environ, GIT_AUTHOR_NAME="A", GIT_AUTHOR_EMAIL="a@example.invalid",
                   GIT_COMMITTER_NAME="A", GIT_COMMITTER_EMAIL="a@example.invalid")
        self.env = env
        for cmd in (["init", "-q"], ["add", "."], ["commit", "-q", "-m", "start"]):
            subprocess.run(["git", "-C", str(self.repo), *cmd], check=True, env=env, capture_output=True)
        self.head = subprocess.run(["git", "-C", str(self.repo), "rev-parse", "HEAD"], capture_output=True,
                                   text=True, check=True).stdout.strip()
        self.art = self.tmp / "artifact"

    def apply(self, report_checks: dict, files: dict[str, bytes]) -> tuple[dict, str]:
        shutil.rmtree(self.art, ignore_errors=True)
        self.art.mkdir()
        (self.art / "report.json").write_text(json.dumps({"app": "radarr", "head": self.head,
                                                          "version": "9.1.2.3456", "checks": report_checks}))
        for rel, data in files.items():
            (self.art / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.art / rel).write_bytes(data)
        comment = self.tmp / "comment.md"
        import contextlib
        import io
        buf = io.StringIO()
        old = {k: os.environ.get(k) for k in self.env}
        os.environ.update(self.env)
        try:
            with contextlib.redirect_stdout(buf):
                checks.main(["apply", "--artifact", str(self.art), "--repo", str(self.repo), "--app", "radarr",
                             "--pr", "28", "--head", self.head, "--comment", str(comment),
                             "--run-url", "https://github.com/bateman/unarr/actions/runs/1"])
        finally:
            for k, v in old.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        return json.loads(buf.getvalue()), comment.read_text()

    def log(self) -> list[str]:
        return subprocess.run(["git", "-C", str(self.repo), "log", "--format=%s", f"{self.head}..HEAD"],
                              capture_output=True, text=True, check=True).stdout.splitlines()

    def test_a_recording_and_the_comparison_one_commit_each(self):
        got, comment = self.apply(
            {"record:recyclarr": {"status": "ok", "exit": 0, "changed": True},
             "http-audit": {"status": "differences", "exit": 1, "requests": 3, "different": 1},
             "compare:real": {"status": "ok", "exit": 0}},
            {"recordings/tests/fixtures/recyclarr/radarr-exchange.json": b"[2]\n",
             "http-audit.txt": b"localization: GET /api/v3/localization\n    content-type: 'a' -> 'b'\n",
             "compare/real/report.md": REPORT.encode()})
        self.assertEqual(len(got["commits"]), 2)
        self.assertEqual(self.log(), ["Measure the originals again on the real library with Radarr 9.1.2.3456",
                                      "Record Radarr's Recyclarr exchange again from Radarr 9.1.2.3456"])
        self.assertEqual((self.repo / "tests/fixtures/recyclarr/radarr-exchange.json").read_bytes(), b"[2]\n")
        self.assertIn("3 requests, 1 with a difference", comment)
        self.assertIn("```text\nlocalization: GET /api/v3/localization", comment)
        self.assertIn("the commits start no CI by themselves", comment)

    def test_a_recording_unchanged_or_failed_is_not_committed(self):
        got, comment = self.apply({"record:recyclarr": {"status": "ok", "exit": 0, "changed": False}},
                                  {"recordings/tests/fixtures/recyclarr/radarr-exchange.json": b"[1]\n"})
        self.assertEqual((got["commits"], self.log()), ([], []))
        self.assertIn("the same as the one committed", comment)
        got, comment = self.apply({"record:recyclarr": {"status": "failed", "exit": 2}}, {})
        self.assertEqual(self.log(), [])
        self.assertIn("failed (exit 2)", comment)

    def test_a_capture_is_never_committed(self):
        got, comment = self.apply({"record:transmission": {"status": "skipped"}}, {})
        self.assertEqual(self.log(), [])
        self.assertIn("not automated: left to a session", comment)

    def test_a_report_made_at_another_commit_stops_it(self):
        self.head, real = "c" * 40, self.head
        with self.assertRaises(checks.ChecksError):
            self.apply({}, {})
        self.head = real


if __name__ == "__main__":
    unittest.main()
