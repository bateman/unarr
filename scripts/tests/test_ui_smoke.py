"""Tests of scripts/ui-smoke.py without a browser or unarrd: the exceptions,
the page list, the report. Run with `make test-scripts` (part of `make check`);
`make ui-smoke` runs the script itself."""

from __future__ import annotations

import importlib.util
import re
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
REPO = SCRIPTS.parent

_spec = importlib.util.spec_from_file_location("ui_smoke", SCRIPTS / "ui-smoke.py")
smoke = importlib.util.module_from_spec(_spec)
# Registered first: dataclasses look their module up by name.
sys.modules[_spec.name] = smoke
_spec.loader.exec_module(smoke)

COVER = next(a for a in smoke.ALLOWED if a.path.startswith("/MediaCover/"))
PROXY = next(a for a in smoke.ALLOWED if a.path.startswith("/MediaCoverProxy/"))
HASH = "3cad0a0195c68f286d21e1cfbffd27c77456fe0dd62072d2f9b24be8ca9ff60a"
UPSTREAM_503 = '{"message":"HTTP request failed: [503:Service Unavailable] [GET] at [https://image.tmdb.org/t/p/original/x.jpg]"}'


class Imports(unittest.TestCase):
    def test_without_playwright(self):
        # make test-scripts runs on the system Python: Playwright is imported in main() only.
        self.assertNotIn("playwright", sys.modules)


class AllowedEntries(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.data = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_each_entry_is_narrow_and_says_why(self):
        for entry in smoke.ALLOWED:
            with self.subTest(entry.path):
                self.assertIn(entry.method, {"GET", "POST", "PUT", "DELETE"})
                self.assertTrue(400 <= entry.status <= 599)
                self.assertTrue(set(entry.apps) <= set(smoke.APPS) and entry.apps)
                self.assertGreater(len(entry.reason), 60, "the reason says why the answer is right")
                re.compile(entry.path)
                for broad in ("/", "/api/v3/series", "/api/v3/calendar", "/MediaCover", "/MediaCover/1",
                              "/signalr/messages", "/index.js"):
                    self.assertIsNone(re.fullmatch(entry.path, broad), broad)

    def test_a_missing_cover_is_admitted(self):
        for app in ("sonarr", "radarr"):
            for path in ("/MediaCover/3/poster-250.jpg", "/MediaCover/308/fanart.jpg", "/MediaCover/12/banner.jpg"):
                self.assertTrue(COVER.matches(app, "GET", path, 404, "", self.data), (app, path))

    def test_a_cover_on_disk_is_not(self):
        (self.data / "sonarr/MediaCover/3").mkdir(parents=True)
        self.assertFalse(COVER.matches("sonarr", "GET", "/MediaCover/3/poster-250.jpg", 404, "", self.data))
        # Another item, or the same id in the other personality, still is.
        self.assertTrue(COVER.matches("sonarr", "GET", "/MediaCover/4/poster-250.jpg", 404, "", self.data))
        self.assertTrue(COVER.matches("radarr", "GET", "/MediaCover/3/poster-250.jpg", 404, "", self.data))

    def test_cover_other_method_status_or_path(self):
        self.assertFalse(COVER.matches("sonarr", "POST", "/MediaCover/3/poster.jpg", 404, "", self.data))
        self.assertFalse(COVER.matches("sonarr", "GET", "/MediaCover/3/poster.jpg", 500, "", self.data))
        self.assertFalse(COVER.matches("sonarr", "GET", "/MediaCover/3/poster.jpg/x", 404, "", self.data))
        self.assertFalse(COVER.matches("sonarr", "GET", "/MediaCover/3/../../config.xml", 404, "", self.data))
        self.assertFalse(COVER.matches("sonarr", "GET", "/api/v3/MediaCover/3/poster.jpg", 404, "", self.data))

    def test_the_proxy_only_with_the_upstream_503(self):
        path = f"/MediaCoverProxy/{HASH}/yq4rYmaTRC5degaOYmJQFpaiho1.jpg"
        self.assertTrue(PROXY.matches("radarr", "GET", path, 500, UPSTREAM_503, self.data))
        self.assertFalse(PROXY.matches("radarr", "GET", path, 500, '{"message":"Url no longer in cache"}', self.data))
        self.assertFalse(PROXY.matches("radarr", "GET", path, 500, "", self.data))
        self.assertFalse(PROXY.matches("sonarr", "GET", path, 500, UPSTREAM_503, self.data))
        self.assertFalse(PROXY.matches("radarr", "GET", path, 404, UPSTREAM_503, self.data))

    def test_classify_ignores_the_query(self):
        url = "http://127.0.0.1:1234/MediaCover/3/poster-250.jpg?h=c1abb18df3048b437540"
        self.assertIs(smoke.classify_response("sonarr", "GET", url, 404, "", self.data, smoke.ALLOWED), COVER)
        api = "http://127.0.0.1:1234/api/v3/calendar?start=x"
        self.assertIsNone(smoke.classify_response("sonarr", "GET", api, 500, "", self.data, smoke.ALLOWED))


class Pages(unittest.TestCase):
    def test_menu_path(self):
        origin = "http://127.0.0.1:8989"
        self.assertEqual(smoke.menu_path("/settings/ui", origin, ""), "/settings/ui")
        self.assertEqual(smoke.menu_path("/settings/ui?x=1#y", origin, ""), "/settings/ui")
        self.assertEqual(smoke.menu_path(origin + "/calendar", origin, ""), "/calendar")
        self.assertEqual(smoke.menu_path("/sonarr/calendar", origin, "/sonarr"), "/calendar")
        self.assertEqual(smoke.menu_path("/sonarr", origin, "/sonarr"), "/")
        self.assertIsNone(smoke.menu_path("https://wiki.servarr.com/sonarr", origin, ""))
        self.assertIsNone(smoke.menu_path("relative", origin, ""))
        self.assertIsNone(smoke.menu_path(None, origin, ""))

    def test_the_fixed_list_after_the_menu(self):
        self.assertEqual(smoke.merge_pages(["/", "/calendar"], ["/", "/system/status", "/calendar"]),
                         [("/", "menu"), ("/calendar", "menu"), ("/system/status", "minimum")])

    def test_minimum_covers_what_the_issue_names(self):
        for app in smoke.APPS:
            pages = set(smoke.MINIMUM[app])
            for path in ("/", "/calendar", "/activity/queue", "/activity/history", "/activity/blocklist",
                         "/wanted/missing", "/settings", "/system/status", "/system/tasks", "/system/backup",
                         "/system/logs/files", "/system/events"):
                self.assertIn(path, pages, (app, path))

    def test_resource_errors_are_requests(self):
        self.assertTrue(smoke.is_resource_error("Failed to load resource: the server responded with a status of 404 (Not Found)"))
        self.assertFalse(smoke.is_resource_error("Uncaught TypeError: x is undefined"))


class Stage(unittest.TestCase):
    def test_a_copy_on_its_own_port(self):
        with tempfile.TemporaryDirectory() as tmp:
            data = Path(tmp) / "data"
            ports = smoke.stage(["sonarr", "radarr"], data)
            for app in ("sonarr", "radarr"):
                config = (data / app / "config.xml").read_text()
                self.assertIn(f"<Port>{ports[app]}</Port>", config)
                self.assertTrue((data / app / f"{app}.db").exists())
            # The fixtures themselves are not touched.
            self.assertIn("<Port>8989</Port>", (REPO / "tests/fixtures/sonarr/config.xml").read_text())
            self.assertIn("<Port>7878</Port>", (REPO / "tests/fixtures/radarr/config.xml").read_text())


def report(pages, problems=(), unused=()):
    return {
        "date": "2026-11-21 10:00:00", "unarrd": "target/debug/unarrd", "browser": "chromium 1", "seconds": 12.0,
        "metadata_asked": ["GET /skyhook.sonarr.tv/v1/tvdb/shows/en/1"],
        "apps": [{"app": "sonarr", "ui_version": "4.0.20.3014", "seconds": 10.0, "problems": list(problems),
                  "websockets": ["/signalr/messages"], "pages": pages, "allowed_used": [],
                  "allowed_unused": list(unused)}],
    }


def page(path, failures=(), source="menu", admitted=()):
    return {"path": path, "source": source, "ok": not failures, "seconds": 1.0, "requests": 3,
            "failures": list(failures), "admitted": list(admitted), "blocked": []}


class Report(unittest.TestCase):
    def test_all_ok(self):
        r = report([page("/login", source="login"), page("/"), page("/calendar")])
        self.assertFalse(smoke.failed(r))
        text = smoke.summary(r, Path("/out"))
        self.assertIn("sonarr 4.0.20.3014: 2 pages, 2 ok, 0 failed", text)
        self.assertTrue(text.splitlines()[-1].startswith("ok;"))

    def test_a_failure_names_the_page_and_the_request(self):
        bad = {"kind": "response", "method": "GET", "url": "/api/v3/calendar?start=x", "status": 500,
               "status_text": "Internal Server Error", "body": '{"message":"boom"}'}
        state = {"kind": "state", "state": "danger alert", "text": "Unable to load the calendar"}
        r = report([page("/"), page("/calendar", [bad, state])])
        self.assertTrue(smoke.failed(r))
        text = smoke.summary(r, Path("/out"))
        self.assertIn("2 pages, 1 ok, 1 failed", text)
        self.assertIn("FAIL /calendar", text)
        self.assertIn('GET /api/v3/calendar?start=x -> 500 Internal Server Error: {"message":"boom"}', text)
        self.assertIn("danger alert: Unable to load the calendar", text)
        self.assertTrue(text.splitlines()[-1].startswith("FAILED;"))
        md = smoke.markdown(r)
        self.assertIn("| `/calendar` | menu | **failed** |", md)
        self.assertIn("### `/calendar`", md)

    def test_a_problem_fails_the_run(self):
        r = report([page("/")], problems=[{"kind": "signalr", "text": "no websocket"}])
        self.assertTrue(smoke.failed(r))
        self.assertIn("FAIL signalr: no websocket", smoke.summary(r, Path("/out")))

    def test_an_unused_exception_is_reported(self):
        unused = {"method": "GET", "path": "/x", "status": 404, "reason": "r"}
        text = smoke.summary(report([page("/")], unused=[unused]), Path("/out"))
        self.assertIn("exception not needed by any page: GET /x 404", text)


if __name__ == "__main__":
    unittest.main()
