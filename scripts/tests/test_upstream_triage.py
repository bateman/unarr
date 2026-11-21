"""Tests of scripts/upstream-triage.py, on made-up data and a local git
repository: no network. Run with `make test-scripts` (part of `make check`)."""

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
import urllib.error
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent.parent
REPO = SCRIPTS.parent

_spec = importlib.util.spec_from_file_location("upstream_triage", SCRIPTS / "upstream-triage.py")
triage = importlib.util.module_from_spec(_spec)
# Registered first: dataclasses look their module up by name.
sys.modules[_spec.name] = triage
_spec.loader.exec_module(triage)

V = triage.Version.parse


def commit(changes, sha="a" * 40, subject="s"):
    return triage.Commit(sha, "2026-01-01T00:00:00+00:00", subject, list(changes))


class ClassifyPath(unittest.TestCase):
    def test_the_table_of_the_issue(self):
        cases = {
            "frontend/src/Movie/MovieIndex.tsx": "ui",
            "src/Radarr.Api.V3/Movies/MovieController.cs": "routes",
            "src/Sonarr.Api.V3/Series/SeriesController.cs": "routes",
            "src/Radarr.Http/Authentication/AuthenticationController.cs": "routes",
            "src/NzbDrone.Core/Datastore/Migration/243_add_thing.cs": "schema",
            "src/NzbDrone.Core/Datastore/Migration/Framework/MigrationController.cs": "schema",
            "src/NzbDrone.Core/Parser/Parser.cs": "backend",
            "src/NzbDrone.Core.Test/ParserTests/ParserFixture.cs": "test",
            "src/NzbDrone.Integration.Test/ApiTests/MovieFixture.cs": "test",
            "src/NzbDrone.Test.Common/TestBase.cs": "test",
            "src/NzbDrone.Core/Localization/Core/en.json": "none",
            # Code beside the strings is backend: it serves /localization.
            "src/NzbDrone.Core/Localization/LocalizationService.cs": "backend",
            ".github/workflows/ci.yml": "none",
            "azure-pipelines.yml": "none",
            "build.sh": "none",
            "README.md": "none",
            "src/Directory.Build.props": "none",
        }
        for path, cls in cases.items():
            with self.subTest(path=path):
                self.assertEqual(triage.classify_path(path)[0], cls)

    def test_beyond_the_table(self):
        cases = {
            # A project file under the API is a package bump, not a route.
            "src/Radarr.Api.V3/Radarr.Api.V3.csproj": "none",
            "src/NzbDrone.Core/Radarr.Core.csproj": "none",
            "src/Sonarr.Api.V5/Series/SeriesController.cs": "routes",
            "src/NzbDrone.Common/Http/HttpClient.cs": "backend",
            "src/NzbDrone.SignalR/SignalRMessage.cs": "backend",
            "src/NzbDrone.Windows/Disk/DiskProvider.cs": "none",
            "src/NzbDrone.Update/UpdateEngine/InstallUpdateService.cs": "none",
            "package.json": "none",
            "yarn.lock": "none",
            "src/Something.New/File.cs": "unknown",
            "weird/place.txt": "unknown",
        }
        for path, cls in cases.items():
            with self.subTest(path=path):
                self.assertEqual(triage.classify_path(path)[0], cls)


class ClassifyCommit(unittest.TestCase):
    def test_highest_class_first_the_others_after(self):
        c = triage.classify_commit(
            commit([("M", "frontend/a.js"), ("M", "src/NzbDrone.Core.Test/x.cs"), ("M", "src/Radarr.Http/y.cs"),
                    ("M", "src/NzbDrone.Core/z.cs")]),
            triage.Wontport(),
        )
        self.assertEqual((c.cls, c.also), ("routes", ["backend", "ui", "test"]))

    def test_migrations_added_and_changed(self):
        c = triage.classify_commit(
            commit([("A", "src/NzbDrone.Core/Datastore/Migration/243_add_thing.cs"),
                    ("M", "src/NzbDrone.Core/Datastore/Migration/177_language_improvements.cs"),
                    ("M", "src/NzbDrone.Core/Datastore/Migration/Framework/MigrationController.cs")]),
            triage.Wontport(),
        )
        self.assertEqual(c.cls, "schema")
        self.assertEqual(c.migrations, [243])
        self.assertEqual(c.migrations_changed, [177])

    def test_wontport_by_glob_needs_every_path(self):
        wp = triage.Wontport(globs=["src/NzbDrone.Core/ImportLists/Trakt/*", "frontend/src/Settings/ImportLists/*"])
        inside = commit([("M", "src/NzbDrone.Core/ImportLists/Trakt/Smart/TraktSmartList.cs"),
                         ("M", "frontend/src/Settings/ImportLists/Trakt.tsx")])
        partly = commit([("M", "src/NzbDrone.Core/ImportLists/Trakt/Smart/TraktSmartList.cs"),
                         ("M", "src/NzbDrone.Core/Parser/Parser.cs")])
        self.assertEqual(triage.classify_commit(inside, wp).cls, "wontport")
        self.assertEqual(triage.classify_commit(inside, wp).also, ["backend", "ui"])
        self.assertEqual(triage.classify_commit(partly, wp).cls, "backend")

    def test_wontport_by_sha_prefix(self):
        wp = triage.Wontport(shas=["abcdef1"])
        c = commit([("M", "src/NzbDrone.Core/Parser/Parser.cs")], sha="abcdef1" + "0" * 33)
        self.assertEqual(triage.classify_commit(c, wp).cls, "wontport")

    def test_a_migration_is_assessed_even_when_listed(self):
        wp = triage.Wontport(shas=["abcdef1"], globs=["src/*"])
        c = commit([("A", "src/NzbDrone.Core/Datastore/Migration/243_add_thing.cs")], sha="abcdef1" + "0" * 33)
        self.assertEqual(triage.classify_commit(c, wp).cls, "schema")

    def test_a_renamed_migration_is_no_new_level_and_a_deleted_one_is_told(self):
        c = triage.classify_commit(
            commit([("D", "src/NzbDrone.Core/Datastore/Migration/204_foo.cs"),
                    ("A", "src/NzbDrone.Core/Datastore/Migration/204_foo_fixed.cs"),
                    ("D", "src/NzbDrone.Core/Datastore/Migration/205_gone.cs")]),
            triage.Wontport(),
        )
        self.assertEqual((c.migrations, c.migrations_changed, c.migrations_removed), ([], [204], [205]))

    def test_classifying_twice_gives_the_same_result(self):
        c = commit([("A", "src/NzbDrone.Core/Datastore/Migration/243_add_thing.cs"), ("M", "odd/path.cs")])
        for _ in range(2):
            triage.classify_commit(c, triage.Wontport())
        self.assertEqual((c.migrations, c.unknown_paths), ([243], ["odd/path.cs"]))

    def test_an_empty_commit_is_none(self):
        self.assertEqual(triage.classify_commit(commit([]), triage.Wontport()).cls, "none")


class ChooseTarget(unittest.TestCase):
    def test_newest_release_within_the_major(self):
        releases = [V("6.4.3.10645"), V("6.4.4.10685"), V("6.4.5.10700"), V("6.5.0.10750")]
        self.assertEqual(triage.choose_target(V("6.4.4.10685"), releases), (V("6.5.0.10750"), "minor"))
        self.assertEqual(triage.choose_target(V("6.4.4.10685"), releases[:3]), (V("6.4.5.10700"), "patch"))

    def test_across_a_major_stops_at_its_first_release(self):
        releases = [V("6.4.4.10685"), V("6.4.5.10700"), V("7.0.0.10815"), V("7.0.1.10859"),
                    V("7.0.3.10912"), V("7.0.3.10918"), V("8.0.0.11000")]
        self.assertEqual(triage.choose_target(V("6.4.4.10685"), releases), (V("7.0.0.10815"), "major"))

    def test_a_skipped_major_goes_to_the_next_one_there_is(self):
        releases = [V("5.0.0.1"), V("7.0.1.3"), V("7.0.0.2")]
        self.assertEqual(triage.choose_target(V("5.0.0.1"), releases), (V("7.0.0.2"), "major"))

    def test_no_newer_release(self):
        self.assertEqual(triage.choose_target(V("4.0.20.3014"), [V("4.0.19.3011"), V("4.0.20.3014")]),
                         (None, "none"))
        self.assertEqual(triage.choose_target(V("4.0.20.3014"), []), (None, "none"))

    def test_a_rebuild_of_the_same_version_is_a_patch(self):
        self.assertEqual(triage.choose_target(V("7.0.3.10912"), [V("7.0.3.10918")]), (V("7.0.3.10918"), "patch"))

    def test_versions(self):
        self.assertEqual(V("v7.0.0.10815"), V("7.0.0.10815"))
        self.assertLess(V("6.10.0.1"), V("7.0.0.1"))
        self.assertLess(V("6.9.0.1"), V("6.10.0.1"))
        for bad in ("7.0.0", "v7.0.0.1-beta", "x"):
            with self.assertRaises(ValueError):
                V(bad)

    def test_the_api_is_read_page_by_page_and_saved(self):
        pages = [[{"tag_name": f"v7.0.{i}.1", "prerelease": i % 2 == 0} for i in range(100)],
                 [{"tag_name": "v6.4.4.10685"}]]
        calls = []

        def urlopen(request, timeout):
            calls.append(request.full_url)
            return io.BytesIO(json.dumps(pages[len(calls) - 1]).encode())

        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(triage.urllib.request, "urlopen", urlopen):
            path = triage.fetch_releases("https://github.com/Radarr/Radarr.git", Path(tmp) / "radarr-releases.json")
            stable = triage.stable_from_api(path)
        self.assertEqual(calls, [f"https://api.github.com/repos/Radarr/Radarr/releases?per_page=100&page={n}"
                                 for n in (1, 2)])
        self.assertEqual(len(stable), 51)
        self.assertIn("v6.4.4.10685", stable)
        self.assertNotIn("v7.0.0.1", stable)

    def test_an_unreachable_api_stops_the_run(self):
        def urlopen(request, timeout):
            raise urllib.error.HTTPError(request.full_url, 403, "Forbidden", None, None)

        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(triage.urllib.request, "urlopen", urlopen):
            with self.assertRaises(SystemExit) as err:
                triage.fetch_releases("https://github.com/Radarr/Radarr", Path(tmp) / "x.json")
        self.assertIn("RELEASES=tags", str(err.exception))

    def test_github_slugs(self):
        self.assertEqual(triage.github_slug("https://github.com/Sonarr/Sonarr.git"), "Sonarr/Sonarr")
        self.assertEqual(triage.github_slug("https://github.com/Radarr/Radarr"), "Radarr/Radarr")
        with self.assertRaises(ValueError):
            triage.github_slug("https://example.com/Radarr/Radarr.git")

    def test_stable_releases_from_the_api(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "releases.json"
            path.write_text(json.dumps([
                [{"tag_name": "v7.0.0.10815", "draft": False, "prerelease": False},
                 {"tag_name": "v7.0.1.10859", "draft": False, "prerelease": True}],
                [{"tag_name": "v7.0.2.10888", "draft": True, "prerelease": False}],
            ]))
            self.assertEqual(triage.stable_from_api(path), {"v7.0.0.10815"})


class Files(unittest.TestCase):
    def test_wontport_entries_and_prose(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "radarr-wontport.md"
            path.write_text(
                "# Radarr: will not port\n\n"
                "Prose with `src/NotAnEntry/*` in it.\n\n"
                "    - `0000000` an example in a code block\n\n"
                "## Entries\n\n"
                "- `ABCDEF1234` one commit\n"
                "* `src/NzbDrone.Core/ImportLists/Trakt/*` Trakt lists\n"
                "- no code span, ignored\n"
            )
            wp = triage.read_wontport(path)
            self.assertEqual(wp.shas, ["abcdef1234"])
            self.assertEqual(wp.globs, ["src/NzbDrone.Core/ImportLists/Trakt/*"])
            self.assertEqual(triage.read_wontport(Path(tmp) / "missing.md").globs, [])

    def test_the_shipped_lists_have_no_entry_yet(self):
        for app in ("sonarr", "radarr"):
            wp = triage.read_wontport(REPO / "upstream" / f"{app}-wontport.md")
            self.assertEqual((wp.shas, wp.globs), ([], []), app)

    def test_the_shipped_cursors_are_at_the_pins(self):
        for app, branch in (("sonarr", "main"), ("radarr", "master")):
            lock = triage.read_lock(REPO / "upstream" / f"{app}.lock")
            self.assertEqual(lock.branch, branch)
            self.assertEqual(lock.release, triage.pinned_version(app), app)

    def test_a_lock_without_a_full_sha_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "x.lock"
            path.write_text("repo = r\nbranch = main\nrelease = 1.0.0.1\ncommit = abc123\n")
            with self.assertRaises(ValueError):
                triage.read_lock(path)


class Markdown(unittest.TestCase):
    def test_untrusted_text_stays_in_its_cell(self):
        self.assertEqual(triage.md_code("a | b `c`\nd\x1b[31m‮"), "`a \\| b 'c' d [31m`")
        self.assertEqual(triage.md_code("<img src=x> @someone [l](http://x)"),
                         "`<img src=x> @someone [l](http://x)`")
        self.assertEqual(triage.md_code(""), "` `")
        self.assertEqual(len(triage.md_code("x" * 500, limit=100)), 102)


@unittest.skipUnless(shutil.which("git"), "git is not installed")
class AgainstAGitRepository(unittest.TestCase):
    """A small upstream: a stable branch with releases on it, and a tag
    elsewhere that a filter by name alone would take."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.work = self.dir / "work"
        self.up = self.dir / "Radarr.git"
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
                        GIT_AUTHOR_NAME="u", GIT_AUTHOR_EMAIL="u@example.invalid",
                        GIT_COMMITTER_NAME="u", GIT_COMMITTER_EMAIL="u@example.invalid")
        self.git("init", "-q", "-b", "master", str(self.work), cwd=self.dir)
        self.cursor = self.change("src/NzbDrone.Core/a.cs", "Bump version to 6.4.4", tag="v6.4.4.10685")
        self.fix = self.change("src/NzbDrone.Core/Parser/Parser.cs", "Fixed: a | b `c`")
        self.mig = self.change("src/NzbDrone.Core/Datastore/Migration/243_thing.cs", "New: thing")
        self.change("src/NzbDrone.Core/b.cs", "Release 7", tag="v7.0.0.10815")
        self.r701 = self.change("frontend/src/c.js", "After 7.0.0", tag="v7.0.1.10859")
        self.git("checkout", "-q", "-b", "develop", cwd=self.work)
        self.change("src/x.cs", "elsewhere", tag="v9.0.0.1")
        self.git("checkout", "-q", "master", cwd=self.work)
        # Bare, as the cache clone the script makes.
        self.git("clone", "-q", "--bare", str(self.work), str(self.up), cwd=self.dir)
        self.lock = self.dir / "radarr.lock"
        self.lock.write_text(f"repo = https://example.invalid/Radarr.git\nbranch = master\n"
                             f"release = 6.4.4.10685\ncommit = {self.cursor}\n")
        self.wontport = self.dir / "radarr-wontport.md"
        self.wontport.write_text("")

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args, cwd=None):
        return subprocess.run(["git", *args], cwd=cwd or self.up, env=self.env, check=True,
                              capture_output=True, text=True).stdout.strip()

    def change(self, path, subject, tag=None):
        file = self.work / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(subject)
        self.git("add", path, cwd=self.work)
        self.git("commit", "-q", "-m", subject, cwd=self.work)
        if tag:
            self.git("tag", tag, cwd=self.work)
        return self.git("rev-parse", "HEAD", cwd=self.work)

    def run_triage(self, *extra, repo=None):
        # No network: the tags alone, unless a test gives the API's JSON.
        releases = [] if "--releases" in extra else ["--releases", "tags"]
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            triage.main(["--app", "radarr", "--repo", str(repo or self.up), "--lock", str(self.lock),
                         "--wontport", str(self.wontport), *releases, *extra])
        return out.getvalue()

    def test_a_major_stops_at_x_0_0_and_ignores_tags_off_the_branch(self):
        report = json.loads(self.run_triage("--format", "json"))
        self.assertEqual((report["target"], report["jump"], report["newest"]), ("7.0.0.10815", "major", "7.0.1.10859"))
        self.assertEqual([c["sha"] for c in report["commits"]][:2], [self.fix, self.mig])
        self.assertEqual([c["class"] for c in report["commits"]], ["backend", "schema", "backend"])
        self.assertEqual(report["migrations"], [243])
        self.assertEqual(report["commits"][0]["subject"], "Fixed: a | b `c`")
        self.assertEqual(report["commits"][0]["paths"],
                         [{"status": "A", "path": "src/NzbDrone.Core/Parser/Parser.cs", "class": "backend",
                           "why": "core"}])

    def test_the_markdown_table(self):
        text = self.run_triage()
        self.assertIn(f"| `{self.fix[:10]}` | backend | `Fixed: a \\| b 'c'` |", text)
        self.assertIn("| schema | 1 |", text)
        self.assertIn(f"Migrations added: 1, 243 (`{self.mig[:10]}`).", text)

    def test_a_target_given_by_hand(self):
        report = json.loads(self.run_triage("--format", "json", "--target", "7.0.1.10859"))
        self.assertEqual((report["target"], report["jump"], len(report["commits"])), ("7.0.1.10859", "major", 4))
        with self.assertRaises(SystemExit):
            self.run_triage("--target", "9.0.0.1")
        # A typo is a one-line error, not a traceback.
        with self.assertRaises(SystemExit) as err:
            self.run_triage("--target", "7.0.x")
        self.assertIn("not a version", str(err.exception))

    def test_a_lock_without_a_full_sha_is_a_one_line_error(self):
        self.lock.write_text(self.lock.read_text().replace(self.cursor, "abc123"))
        with self.assertRaises(SystemExit) as err:
            self.run_triage()
        self.assertIn("not a full SHA", str(err.exception))

    def move_cursor(self, release, sha):
        self.lock.write_text(self.lock.read_text().replace("6.4.4.10685", release).replace(self.cursor, sha))

    def test_no_newer_release(self):
        self.move_cursor("7.0.1.10859", self.r701)
        report = json.loads(self.run_triage("--format", "json"))
        self.assertEqual((report["target"], report["jump"], report["commits"]), (None, "none", []))

    def test_a_rebuild_on_the_cursor_commit_is_no_new_release(self):
        self.move_cursor("7.0.1.10859", self.r701)
        self.git("tag", "v7.0.1.10860", self.r701)
        report = json.loads(self.run_triage("--format", "json"))
        self.assertEqual((report["target"], report["jump"]), (None, "none"))

    def test_a_cursor_whose_commit_is_not_its_release_is_refused(self):
        self.lock.write_text(self.lock.read_text().replace("6.4.4.10685", "7.0.0.10815"))
        with self.assertRaises(SystemExit) as err:
            self.run_triage()
        self.assertIn("is not the commit of v7.0.0.10815", str(err.exception))

    def test_a_git_failure_is_not_a_no(self):
        self.assertFalse(triage.is_ancestor(self.up, self.r701, self.cursor))
        self.assertTrue(triage.is_ancestor(self.up, self.cursor, self.r701))
        with self.assertRaises(triage.GitError):
            triage.is_ancestor(self.up, "f" * 40, self.r701)

    def test_a_working_clone_is_refused(self):
        # Its local branch can lag behind origin's, silently.
        with self.assertRaises(SystemExit) as err:
            self.run_triage(repo=self.work)
        self.assertIn("is not a bare clone", str(err.exception))

    def test_api_data_drops_a_prerelease(self):
        releases = self.dir / "releases.json"
        releases.write_text(json.dumps([{"tag_name": "v7.0.0.10815", "prerelease": True},
                                        {"tag_name": "v7.0.1.10859"}, {"tag_name": "v6.4.4.10685"}]))
        report = json.loads(self.run_triage("--format", "json", "--releases", str(releases)))
        self.assertEqual((report["target"], report["releases_from"]), ("7.0.1.10859", "file"))

    def test_the_first_stable_release_of_the_next_major(self):
        # As Radarr on master: 7.0.0 and 7.0.1 published as pre-releases.
        releases = self.dir / "releases.json"
        releases.write_text(json.dumps([{"tag_name": "v7.0.0.10815", "prerelease": True},
                                        {"tag_name": "v7.0.1.10859", "prerelease": True},
                                        {"tag_name": "v6.4.4.10685"}]))
        report = json.loads(self.run_triage("--format", "json", "--releases", str(releases)))
        self.assertEqual((report["target"], report["newest"]), (None, "6.4.4.10685"))
        self.git("tag", "v7.0.1.10860", self.r701)
        releases.write_text(json.dumps(json.loads(releases.read_text()) + [{"tag_name": "v7.0.1.10860"}]))
        report = json.loads(self.run_triage("--format", "json", "--releases", str(releases)))
        self.assertEqual((report["target"], report["jump"], len(report["commits"])), ("7.0.1.10860", "major", 4))


if __name__ == "__main__":
    unittest.main()
