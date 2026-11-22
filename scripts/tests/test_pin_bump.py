"""Tests of scripts/pin-bump.py (and of what it needs from record-golden.py
and port-vectors.py), on made-up data and the repository's own files: no
network, no original run. Run with `make test-scripts` (part of `make check`)."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import shutil
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent.parent
REPO = SCRIPTS.parent


def load(name: str, file: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / file)
    module = importlib.util.module_from_spec(spec)
    # Registered first: dataclasses look their module up by name.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


bump = load("pin_bump", "pin-bump.py")
recorder = load("record_golden", "record-golden.py")
vectors = load("port_vectors", "port-vectors.py")

RADARR, SONARR = bump.APPS["radarr"], bump.APPS["sonarr"]


class Temp(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="pin-bump-test-"))
        self.addCleanup(shutil.rmtree, self.tmp)


class PinAndCursor(Temp):
    VALUES = {"version": "7.1.0.300", "build_time": "2026-11-16T10:00:00Z", "migration": 243,
              "sha256": "1" * 64}

    def test_the_pin_gets_the_new_values_and_keeps_its_comments(self):
        pin = self.tmp / "radarr.pin"
        shutil.copy(REPO / "upstream" / "radarr.pin", pin)
        before = pin.read_text()
        bump.write_pin(pin, self.VALUES)
        after = pin.read_text()
        self.assertEqual(bump.read_pin(pin), {k: str(v) for k, v in self.VALUES.items()})
        comments = [line for line in before.splitlines() if line.startswith("#")]
        self.assertEqual(comments, [line for line in after.splitlines() if line.startswith("#")])
        self.assertEqual(len(before.splitlines()), len(after.splitlines()))

    def test_odd_values_are_refused_and_nothing_is_written(self):
        pin = self.tmp / "radarr.pin"
        shutil.copy(REPO / "upstream" / "radarr.pin", pin)
        before = pin.read_text()
        for key, value in (("sha256", "abc"), ("build_time", "2026-11-16 10:00"), ("migration", "243"),
                           ("version", "7.0"), ("migration", 0)):
            with self.subTest(key=key, value=value), self.assertRaises(SystemExit):
                bump.write_pin(pin, dict(self.VALUES, **{key: value}))
        self.assertEqual(pin.read_text(), before)

    def test_a_key_written_twice_or_missing_is_refused(self):
        pin = self.tmp / "x.pin"
        pin.write_text("version = 1.0.0.1\nversion = 1.0.0.2\nbuild_time = x\nmigration = 1\nsha256 = y\n")
        with self.assertRaises(SystemExit):
            bump.write_pin(pin, self.VALUES)
        pin.write_text("version = 1.0.0.1\nbuild_time = x\nmigration = 1\n")
        with self.assertRaises(SystemExit):
            bump.write_pin(pin, self.VALUES)

    def test_the_cursor_moves_to_the_release_and_its_commit(self):
        lock = self.tmp / "radarr.lock"
        shutil.copy(REPO / "upstream" / "radarr.lock", lock)
        bump.write_lock(lock, "7.1.0.300", "0123456789abcdef0123456789abcdef01234567")
        read = bump.triage.read_lock(lock)
        self.assertEqual(str(read.release), "7.1.0.300")
        self.assertEqual(read.commit, "0123456789abcdef0123456789abcdef01234567")
        self.assertEqual(read.branch, "master")
        with self.assertRaises(SystemExit):
            bump.write_lock(lock, "7.1.0.300", "af772b0")


class RequestLists(Temp):
    def test_names_recorded_by_a_list(self):
        text = "# header\n\nsystem-status GET /api/v3/system/status\nsleep 5\nmock - POST /control/x\n" \
               "mock mock-sent GET /control/sent\nmovie-add POST /api/v3/movie bodies/movie.json\n"
        self.assertEqual(bump.parse_requests(text), ["system-status", "mock-sent", "movie-add"])
        with self.assertRaises(SystemExit):
            bump.parse_requests("../escape GET /x\n")

    def test_every_list_of_the_repository_is_understood(self):
        for app in (SONARR, RADARR):
            lists = bump.golden_lists(app)
            self.assertEqual(lists[0].key, "golden")
            for g in lists:
                with self.subTest(app=app.key, list=g.key):
                    self.assertTrue(g.names)
                    # What the bump expects a recording to write is what was committed.
                    for name in g.outputs(app):
                        self.assertTrue((g.dir / name).exists(), f"{g.dir / name}")
        hub = next(g for g in bump.golden_lists(SONARR) if g.key == "golden-hub")
        self.assertEqual(hub.env.get("GOLDEN_SIGNALR"), "1")
        self.assertIn("signalr.json", hub.outputs(SONARR))
        main = bump.golden_lists(SONARR)[0]
        self.assertNotIn("signalr.json", main.outputs(SONARR))
        self.assertIn("signalr.json", bump.golden_lists(RADARR)[0].outputs(RADARR))
        legacy = next(g for g in bump.golden_lists(RADARR) if g.key == "golden-legacy-user")
        self.assertIn("config-after.xml", legacy.outputs(RADARR))
        self.assertTrue(legacy.env["GOLDEN_SETUP"].endswith("golden-legacy-user/setup.sh"))

    def test_a_header_the_bump_cannot_follow_is_refused(self):
        d = "tests/fixtures/radarr/golden-x"
        good = f"# A list.\n#\n#   GOLDEN_REQUESTS={d}/requests.txt \\\n#       GOLDEN_OUT={d} make golden-radarr\n"
        self.assertEqual(bump.parse_header(good, d), {"GOLDEN_REQUESTS": f"{d}/requests.txt", "GOLDEN_OUT": d})
        for header in (
            f"#   NEW_KNOB=1 GOLDEN_OUT={d} make golden-radarr\n",
            f"#   GOLDEN_OUT=tests/fixtures/radarr/golden make golden-radarr\n",
            f"#   GOLDEN_SETUP=/tmp/setup.sh GOLDEN_OUT={d} make golden-radarr\n",
        ):
            with self.subTest(header=header), self.assertRaises(SystemExit):
                bump.parse_header(header, d)
        # Prose that names a variable, with no command after it, sets nothing.
        self.assertEqual(bump.parse_header("# GOLDEN_SIGNALR=1 is what the hub list used\nx GET /\n", d), {})

    def test_a_list_asked_for_that_does_not_exist(self):
        with self.assertRaises(SystemExit):
            bump.golden_lists(RADARR, ["golden-nothing"])
        self.assertEqual([g.key for g in bump.golden_lists(RADARR, ["golden-hub"])], ["golden-hub"])


def golden(body, status=200, at="2026-11-12T14:19:06Z", path="/api/v3/x"):
    return {"request": {"method": "GET", "path": path}, "recordedAt": at, "status": status, "body": body}


class Comparing(unittest.TestCase):
    def test_kinds_of_change(self):
        old = golden({"title": "A", "added": "2026-01-01T00:00:00Z", "size": 1.0})
        cases = {
            "same": golden({"title": "A", "added": "2026-01-01T00:00:00Z", "size": 1.0}, at="2026-12-01T00:00:00Z"),
            "time": golden({"title": "A", "added": "2026-12-01T10:11:12.5Z", "size": 1}),
            "values": golden({"title": "B", "added": "2026-01-01T00:00:00Z", "size": 1.0}),
            "shape": golden({"title": "A", "added": "2026-01-01T00:00:00Z", "size": 1.0, "streams": []}),
            "status": golden({"message": "gone"}, status=404),
        }
        for kind, new in cases.items():
            with self.subTest(kind=kind):
                self.assertEqual(bump.compare_json(old, new)["kind"], kind)
        self.assertEqual(bump.compare_json(None, old)["kind"], "new")
        self.assertEqual(bump.compare_json(old, None)["kind"], "gone")
        shape = bump.compare_json(old, cases["shape"])
        self.assertEqual(shape["added"], ["/streams", "/streams[]"])
        self.assertEqual(bump.compare_json(old, cases["values"])["values"], ['/title: "A" -> "B"'])

    def test_list_items_and_covers(self):
        old = golden([{"id": 1, "remoteUrl": "/c/poster.jpg?lastWrite=638000"}])
        new = golden([{"id": 1, "remoteUrl": "/c/poster.jpg?lastWrite=639999"}, {"id": 2}])
        change = bump.compare_json(old, new)
        self.assertEqual(change["kind"], "values")
        self.assertIn("1 items -> 2", change["values"][0])
        self.assertEqual(bump.compare_json(old, golden([{"id": 1, "remoteUrl": "/c/poster.jpg?lastWrite=1"}]))["kind"],
                         "time")

    def test_any_json_and_text(self):
        self.assertEqual(bump.compare_json({"steps": [1]}, {"steps": [1]})["kind"], "same")
        self.assertEqual(bump.compare_json({"steps": [1]}, {"steps": [1, 2]})["kind"], "values")
        self.assertEqual(bump.compare_text("<a/>", "<a/>")["kind"], "same")
        self.assertEqual(bump.compare_text("<a/>", "<b/>")["kind"], "values")
        self.assertEqual(bump.compare_text(None, "<b/>")["kind"], "new")

    def test_schemas_by_implementation_and_field(self):
        old = [{"implementation": "Kodi", "fields": [{"name": "host"}, {"name": "port"}], "infoLink": "a"},
               {"implementation": "Plex", "fields": [{"name": "host"}]}]
        new = [{"implementation": "Kodi", "fields": [{"name": "host"}, {"name": "port"}, {"name": "useSsl"}]},
               {"implementation": "Emby", "fields": []}]
        change = bump.compare_schema(old, new)
        self.assertEqual(change["kind"], "shape")
        self.assertEqual(change["implementations_added"], ["Emby"])
        self.assertEqual(change["implementations_removed"], ["Plex"])
        self.assertEqual(change["fields"], {"Kodi": {"added": ["useSsl"], "removed": []}})
        self.assertEqual(bump.compare_schema(old, old)["kind"], "same")
        relabelled = [dict(old[0], infoLink="b"), old[1]]
        self.assertEqual(bump.compare_schema(old, relabelled), {
            "implementations_added": [], "implementations_removed": [], "fields": {}, "changed": ["Kodi"],
            "kind": "values"})

    def test_localisation_strings(self):
        change = bump.compare_strings({"A": "a", "B": "b", "C": "c"}, {"A": "a", "B": "bee", "D": "d"})
        self.assertEqual((change["kind"], change["added"], change["removed"], change["changed"]),
                         ("shape", ["D"], ["C"], ["B"]))
        self.assertEqual(bump.compare_strings({"A": "a"}, {"A": "a"})["kind"], "same")

    def test_vectors_by_method_and_case(self):
        old = {"source": "s", "methods": {"m": {"params": ["t"], "cases": [["a"], ["b"]]},
                                          "gone": {"params": [], "cases": [[]]}}}
        new = {"source": "s", "methods": {"m": {"params": ["t"], "cases": [["a"], ["c"], ["d"]]},
                                          "added": {"params": [], "cases": [[]]}}}
        change = bump.compare_vectors(old, new)
        self.assertEqual((change["kind"], change["methods_added"], change["methods_removed"]),
                         ("changed", ["added"], ["gone"]))
        self.assertEqual((change["methods"]["m"]["added"], change["methods"]["m"]["removed"]), (2, 1))
        self.assertEqual(bump.compare_vectors(old, old)["kind"], "same")
        self.assertEqual(bump.compare_vectors(None, new)["kind"], "new")
        self.assertEqual(bump.compare_vectors(old, None)["kind"], "gone")

    def test_areas(self):
        self.assertEqual(bump.area_of("downloadclient-sab-test.json"), "downloadclient")
        self.assertEqual(bump.area_of("noauth-system-status.json"), "system")
        self.assertEqual(bump.area_of("apikey-query-system-status.json"), "system")
        self.assertEqual(bump.area_of("signalr.json"), "hub")


CARGO = """\
   Compiling unarr-core v0.0.1
     Running unittests src/lib.rs (target/debug/deps/unarr_import-0123456789abcdef)

running 3 tests
test radarr::tests::the_pinned_release_is_within_the_known_migrations ... FAILED
test radarr::tests::imports_the_fixture_table_by_table ... ok
test sonarr::tests::x ... ignored

failures:

---- radarr::tests::the_pinned_release_is_within_the_known_migrations stdout ----

thread 'radarr::tests::the_pinned_release_is_within_the_known_migrations' panicked at src/radarr.rs:757:9:
Radarr pinned at migration 243, the importer knows 208..=242

failures:
    radarr::tests::the_pinned_release_is_within_the_known_migrations

test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s

error: test failed, to rerun pass `-p unarr-import --lib`
     Running tests/contract_radarr.rs (target/debug/deps/contract_radarr-fedcba9876543210)

running 2 tests
test golden_responses_match ... FAILED
test the_hub_goldens_match ... ok

failures:

---- golden_responses_match stdout ----
system-status:
    /version: "6.1.0.100" expected, "7.1.0.300" got

failures:
    golden_responses_match

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 12.00s

     Running unittests src/main.rs (target/debug/deps/unarrd-00112233445566aa)

running 1 test
test tests::starts ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests unarr_core

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
"""


class TestOutput(unittest.TestCase):
    def test_cargo_failures_with_their_target_and_output(self):
        result = bump.parse_cargo(CARGO)
        self.assertEqual((result["passed"], result["failed"]), (3, 2))
        self.assertEqual([(f["name"], f["target"]) for f in result["failures"]], [
            ("radarr::tests::the_pinned_release_is_within_the_known_migrations", "unarr-import (lib)"),
            ("golden_responses_match", "tests/contract_radarr"),
        ])
        self.assertEqual(result["failures"][0]["args"], ["-p", "unarr-import", "--lib"])
        self.assertEqual(result["failures"][1]["args"], ["--workspace", "--test", "contract_radarr"])
        self.assertIn("Radarr pinned at migration 243, the importer knows 208..=242", result["failures"][0]["output"])
        self.assertIn('    /version: "6.1.0.100" expected, "7.1.0.300" got', result["failures"][1]["output"])
        self.assertNotIn("failures:", "\n".join(result["failures"][1]["output"]))
        self.assertEqual(result["compile_errors"], [])
        self.assertEqual(result["failed_targets"], ["-p unarr-import --lib"])
        tail = "error: 2 targets failed:\n    `-p unarr-import --lib`\n    `-p unarr-api --test contract_radarr`\n"
        self.assertEqual(bump.parse_cargo(CARGO + tail)["failed_targets"],
                         ["-p unarr-import --lib", "-p unarr-api --test contract_radarr"])

    def test_a_build_that_fails(self):
        result = bump.parse_cargo("error[E0425]: cannot find value `X`\nerror: could not compile `unarr-core`\n")
        self.assertEqual(len(result["compile_errors"]), 2)

    def test_targets(self):
        self.assertEqual(bump.binary_target("     Running unittests src/main.rs (target/debug/deps/unarrd-0a)")["args"],
                         ["-p", "unarrd", "--bin", "unarrd"])
        self.assertEqual(bump.binary_target("   Doc-tests unarr_core")["target"], "unarr-core (doc)")
        self.assertIsNone(bump.binary_target("   Compiling unarr-core v0.0.1"))

    def test_unittest_failures(self):
        out = "FAIL: test_x (test_pins.OneSourcePerPin.test_x)\nERROR: test_y (test_pin_bump.T.test_y)\nok\n"
        self.assertEqual([f["name"] for f in bump.parse_unittest(out)], ["test_x", "test_y"])


class TheTarball(Temp):
    def tarball(self, entries):
        path = self.tmp / "t.tar.gz"
        with tarfile.open(path, "w:gz") as tar:
            for name, kind, link in entries:
                info = tarfile.TarInfo(name)
                if kind == "dir":
                    info.type = tarfile.DIRTYPE
                elif kind == "sym":
                    info.type, info.linkname = tarfile.SYMTYPE, link
                else:
                    info.size = 2
                tar.addfile(info, io.BytesIO(b"hi") if kind == "file" else None)
        return path

    def test_a_release_archive(self):
        good = self.tarball([("Radarr", "dir", ""), ("Radarr/Radarr", "file", ""), ("Radarr/UI/index.html", "file", ""),
                             ("Radarr/libx.so", "sym", "libx.so.1")])
        self.assertEqual(bump.check_tarball(good, RADARR), 4)

    def test_archives_that_would_write_elsewhere_or_lack_the_binary(self):
        for entries in (
            [("Radarr/Radarr", "file", ""), ("../evil", "file", "")],
            [("Radarr/Radarr", "file", ""), ("/etc/evil", "file", "")],
            [("Radarr/Radarr", "file", ""), ("Other/x", "file", "")],
            [("Radarr/Radarr", "file", ""), ("Radarr/link", "sym", "../../etc/passwd")],
            [("Radarr/UI/index.html", "file", "")],
        ):
            with self.subTest(entries=entries), self.assertRaises(SystemExit):
                bump.check_tarball(self.tarball(entries), RADARR)

    def test_the_release_url_and_where_it_may_end(self):
        lock = bump.triage.read_lock(REPO / "upstream" / "radarr.lock")
        url, name = bump.asset_url(RADARR, lock, "7.1.0.300")
        self.assertEqual(url, "https://github.com/Radarr/Radarr/releases/download/v7.1.0.300/"
                              "Radarr.master.7.1.0.300.linux-core-x64.tar.gz")
        self.assertEqual(name, "Radarr.master.7.1.0.300.linux-core-x64.tar.gz")
        lock = bump.triage.read_lock(REPO / "upstream" / "sonarr.lock")
        self.assertEqual(bump.asset_url(SONARR, lock, "5.0.0.1")[1], "Sonarr.main.5.0.0.1.linux-x64.tar.gz")
        self.assertEqual(bump.check_asset_host("https://release-assets.githubusercontent.com/github-production/x?sig=1"),
                         "release-assets.githubusercontent.com")
        for url in ("https://example.com/Radarr.tar.gz", "http://objects.githubusercontent.com/x",
                    "https://github.com.evil.example/x"):
            with self.subTest(url=url), self.assertRaises(SystemExit):
                bump.check_asset_host(url)

    def test_the_digest_the_api_publishes(self):
        path = self.tmp / "releases.json"
        path.write_text(json.dumps([[{"tag_name": "v7.1.0.300", "assets": [
            {"name": "Radarr.master.7.1.0.300.linux-core-x64.tar.gz", "digest": "sha256:" + "a" * 64},
            {"name": "other.zip", "digest": "sha256:" + "b" * 64}]}]]))
        name = "Radarr.master.7.1.0.300.linux-core-x64.tar.gz"
        self.assertEqual(bump.api_digest(path, "v7.1.0.300", name), "sha256:" + "a" * 64)
        self.assertIsNone(bump.api_digest(path, "v7.0.2.1", name))
        self.assertIsNone(bump.api_digest(None, "v7.1.0.300", name))


class UntrustedWork(Temp):
    def test_links_large_or_broken_files_are_refused(self):
        target = self.tmp / "real.json"
        target.write_text("{}")
        link = self.tmp / "link.json"
        link.symlink_to(target)
        with self.assertRaises(SystemExit):
            bump.checked_file(link, True)
        broken = self.tmp / "broken.json"
        broken.write_text("{not json")
        with self.assertRaises(SystemExit):
            bump.checked_file(broken, True)
        self.assertEqual(bump.checked_file(target, True), b"{}")
        self.assertEqual(bump.checked_file(broken, False), b"{not json")

    def ctx(self):
        return bump.Context(app=RADARR, work=self.tmp)

    def test_a_recorded_list_is_compared_with_head_and_only_its_files_are_copied(self):
        ctx = self.ctx()
        committed = REPO / "tests" / "fixtures" / "radarr" / "golden-legacy-user"
        out = self.tmp / "recorded" / "golden-legacy-user"
        out.mkdir(parents=True)
        for name in ("config-host.json", "noauth-initialize.json", "session-initialize.json", "signalr.json",
                     "config-after.xml"):
            shutil.copy(committed / name, out / name)
        changed = json.loads((out / "config-host.json").read_text())
        changed["recordedAt"] = "2030-01-01T00:00:00Z"
        changed["body"]["newSetting"] = True
        (out / "config-host.json").write_text(json.dumps(changed))
        (out / "planted.json").write_text("{}")
        record = {"lists": [{"list": "golden-legacy-user", "ok": True}]}
        result = bump.recorded_changes(ctx, record)
        files = result["lists"]["golden-legacy-user"]
        self.assertEqual(files["config-host.json"]["kind"], "shape")
        self.assertEqual(files["config-host.json"]["added"], ["/newSetting"])
        for name in ("noauth-initialize.json", "session-initialize.json", "signalr.json", "config-after.xml"):
            self.assertEqual(files[name]["kind"], "same", name)
        copied = {Path(dest).name for _, dest in result["copy"]}
        self.assertNotIn("planted.json", copied)
        self.assertEqual(len(copied), 5)
        (out / "signalr.json").unlink()
        (out / "signalr.json").symlink_to(committed / "signalr.json")
        with self.assertRaises(SystemExit):
            bump.recorded_changes(ctx, record)

    def test_a_list_that_failed_is_not_copied(self):
        result = bump.recorded_changes(self.ctx(), {"lists": [{"list": "golden-hub", "ok": False}]})
        self.assertEqual(result, {"lists": {}, "copy": []})


class Steps(Temp):
    def test_steps_run_in_their_order(self):
        self.assertEqual(bump.parse_steps(None), list(bump.STEPS))
        self.assertEqual(bump.parse_steps("summary,apply"), ["apply", "summary"])
        with self.assertRaises(SystemExit):
            bump.parse_steps("fetch,deploy")

    def test_a_dry_run_writes_nothing(self):
        work = self.tmp / "work"
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            bump.main(["--app", "radarr", "--steps", "record,apply,test,summary", "--dry-run", "--work", str(work)])
        text = out.getvalue()
        self.assertFalse(work.exists())
        for expected in ("record: runs Radarr", "golden-library", "GOLDEN_SETUP=tests/fixtures/radarr/golden-blank",
                         "apply: writes upstream/radarr.pin", "upstream/radarr.lock", "make ui-radarr",
                         "tests/vectors/radarr/{parser,decisionengine}", "test: cargo test", "summary:"):
            self.assertIn(expected, text)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            bump.main(["--app", "sonarr", "--steps", "apply", "--dry-run", "--keep-cursor", "--work", str(work)])
        self.assertNotIn("sonarr.lock", out.getvalue())

    def test_a_step_needs_what_the_one_before_left(self):
        with self.assertRaises(SystemExit) as caught:
            bump.main(["--app", "radarr", "--steps", "record", "--work", str(self.tmp)])
        self.assertIn("run the fetch step first", str(caught.exception))
        (self.tmp / "bump.json").write_text(json.dumps({"app": "sonarr"}))
        with self.assertRaises(SystemExit) as caught:
            bump.main(["--app", "radarr", "--steps", "apply", "--work", str(self.tmp)])
        self.assertIn("not radarr", str(caught.exception))

    def test_the_importer_range_is_read_not_raised(self):
        for app in (SONARR, RADARR):
            before = (REPO / app.importer).read_text()
            known = bump.importer_range(app, 1)
            self.assertFalse(known["raise"])
            self.assertTrue(bump.importer_range(app, known["known"] + 1)["raise"])
            line = before.splitlines()[known["line"] - 1]
            self.assertIn(f"MAX_KNOWN_MIGRATION: i64 = {known['known']};", line)
            self.assertEqual((REPO / app.importer).read_text(), before)

    def test_the_caches(self):
        with mock.patch.dict(os.environ, {"UNARR_CACHE": "/c"}):
            self.assertEqual(bump.fixture_cache(RADARR), Path("/c"))
            self.assertEqual(bump.cache_dir(), Path("/c"))
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(bump.fixture_cache(RADARR), REPO / ".cache" / "radarr")


def write_work(work: Path, subject: str = "Fix `x` | y") -> None:
    bump.write_json(work / "bump.json", {
        "app": "radarr", "from": {"version": "6.1.0.100", "commit": "a" * 40},
        "to": {"version": "7.1.0.300", "tag": "v7.1.0.300", "commit": "b" * 40}, "jump": "major",
        "releases_from": "tags", "url": "https://github.com/Radarr/Radarr/releases/download/v7.1.0.300/R.tar.gz",
        "served_from": "release-assets.githubusercontent.com", "asset": "R.tar.gz", "sha256": "c" * 64,
        "size": 10, "entries": 2, "api_digest": "not read: RELEASES=tags"})
    bump.write_json(work / "triage.json", {
        "summary": {"schema": 1, "backend": 1}, "migrations": [243],
        "commits": [{"sha": "d" * 40, "subject": subject, "class": "schema",
                     "paths": [{"status": "A", "path": "src/NzbDrone.Core/Datastore/Migration/243_streams.cs"}]}]})
    bump.write_json(work / "record.json", {
        "version": "7.1.0.300", "build_time": "2026-11-16T10:00:00Z", "migration": 243, "seconds": 600,
        "migrate_seconds": 7, "metadata_misses": {"golden-library": ["GET api.radarr.video/v1/movie/1"]},
        "lists": [{"list": "golden", "ok": True, "exit": 0, "seconds": 500},
                  {"list": "golden-hub", "ok": False, "exit": 1, "seconds": 9, "missing": ["a.json"]}],
        "baseline": {"lists": [], "metadata_misses": {}}})
    bump.write_json(work / "apply.json", {
        "pin": {"version": "7.1.0.300", "build_time": "2026-11-16T10:00:00Z", "migration": 243,
                "sha256": "c" * 64},
        "cursor": {"release": "7.1.0.300", "commit": "b" * 40},
        "recordings": {"golden": {
            "system-status.json": {"kind": "values", "values": ['/version: "6.1.0.100" -> "7.1.0.300"']},
            "movie-1.json": {"kind": "shape", "added": ["/mediaInfo/audioStreams"], "removed": [], "values": []},
            "queue.json": {"kind": "values", "values": ["/records[0]/timeleft: \"`|`\" -> \"b\""]},
            "calendar.json": {"kind": "time"}, "tag.json": {"kind": "same"}}},
        "schemas": {"localization-en.json": {"kind": "shape", "added": ["AudioStreams"], "removed": [], "changed": []},
                    "indexer-schema.json": {"kind": "same"}},
        "baseline": {"recorded": {"lists": {"golden": {"queue.json": {"kind": "values", "values": []}}}},
                     "schemas": {"files": {}}},
        "vectors": {"files": {"parser/LanguageParserFixture.json": {
            "kind": "changed", "methods_added": ["should_parse_x"], "methods_removed": [],
            "methods": {"should_y": {"added": 2, "removed": 0, "params": False}}}}, "written": 27, "deleted": []},
        "importer": {"file": "crates/unarr-import/src/radarr.rs", "line": 32, "known": 242, "migration": 243,
                     "raise": True},
        "ui_seconds": 9, "seconds": 30})
    bump.write_json(work / "tests.json", {
        "seconds": 400, "scripts": {"exit": 0, "failures": []},
        "cargo": {"exit": 101, "passed": 790, "failed": 2, "compile_errors": [], "failures": [
            {"name": "golden_responses_match", "target": "tests/contract_radarr", "args": [], "output": ["system"],
             "rerun": "failed"},
            {"name": "golden_responses_match", "target": "tests/contract", "args": [], "output": [], "rerun": "passed"},
        ]}})


class Summary(Temp):
    def test_the_summary_says_what_changed_and_what_fails(self):
        write_work(self.tmp)
        text, data = bump.render_summary(self.tmp)
        for expected in (
            "# Radarr 6.1.0.100 → 7.1.0.300 (major)",
            "Build time `2026-11-16T10:00:00Z`, migration level 243",
            "5 files in 1 lists; 3 changed beyond the instants",
            "| `golden` | 5 | 1 | 1 | 2 | 1 | 0 | 0 | 0 | 0 |",
            "`golden/movie-1.json` shape: `+/mediaInfo/audioStreams`",
            "`golden/queue.json` values *noisy*",
            "Lists not recorded: `golden-hub` (exit 1, missing 1)",
            "`localization-en.json`: 1 strings added",
            "methods added `should_parse_x`",
            "`crates/unarr-import/src/radarr.rs:32`",
            "raise it to 243",
            "src/NzbDrone.Core/Datastore/Migration/243_streams.cs",
            "790 passed, 2 failed",
            "(passed when run again: flaky) (known flaky)",
            "GET api.radarr.video/v1/movie/1",
            "tests/fixtures/recyclarr/radarr-exchange.json",
            "make prowlarr-radarr",
        ):
            self.assertIn(expected, text)
        self.assertEqual(data["recordings"]["changed"], 3)
        self.assertEqual(data["recordings"]["areas"]["system"], {"values": 1})
        self.assertEqual(data["tests"]["failed"], 2)

    def test_untrusted_text_stays_in_code_spans(self):
        write_work(self.tmp)
        text, _ = bump.render_summary(self.tmp)
        queue = next(line for line in text.splitlines() if "golden/queue.json" in line)
        self.assertNotIn("`|`", queue)
        self.assertIn("\\|", queue)

    def test_a_summary_after_fetch_alone(self):
        write_work(self.tmp)
        for name in ("record.json", "apply.json", "tests.json"):
            (self.tmp / name).unlink()
        text, data = bump.render_summary(self.tmp)
        self.assertIn("## The new original", text)
        self.assertNotIn("## Recordings", text)
        self.assertIn("## Left to check by hand", text)


class Recorder(unittest.TestCase):
    def test_the_login_sends_what_the_browser_sends(self):
        radarr6 = '<input\n  type="checkbox"\n  name="rememberMe"\n  id="rememberMe"\n  checked="checked"\n/>'
        radarr7 = '<input\n  type="checkbox"\n  name="rememberMe"\n  id="rememberMe"\n  value="true"\n  checked="checked"\n/>'
        self.assertEqual(recorder.checkbox_value(radarr6), "on")
        self.assertEqual(recorder.checkbox_value(radarr7), "true")
        self.assertEqual(recorder.checkbox_value("<html>no form</html>"), "on")


class Vectors(Temp):
    def test_the_source_is_the_path_in_the_original_tree(self):
        checkout = self.tmp / "somewhere" / "src" / "Radarr"
        fixture = checkout / "src" / "NzbDrone.Core.Test" / "ParserTests" / "XFixture.cs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text('[TestCase("a")]\npublic void should_x(string title)\n{\n}\n')
        data = vectors.extract(fixture, checkout)
        self.assertEqual(data["source"], "src/NzbDrone.Core.Test/ParserTests/XFixture.cs")
        self.assertEqual(data["methods"], {"should_x": {"params": ["title"], "cases": [["a"]]}})

    def test_the_committed_vectors_say_where_they_come_from(self):
        for path in sorted((REPO / "tests" / "vectors").rglob("*.json")):
            with self.subTest(path=path.name):
                self.assertTrue(json.loads(path.read_text())["source"].startswith("src/NzbDrone.Core.Test/"))


if __name__ == "__main__":
    unittest.main()
