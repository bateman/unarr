"""Tests of scripts/mock-servarr.py as the recording original's proxy: the
Servarr services answered from recordings whatever version asks, every other
host refused at once. The mock listens on a free loopback port; no network.
Run with `make test-scripts` (part of `make check`)."""

from __future__ import annotations

import http.client
import importlib.util
import io
import json
import shutil
import sys
import tempfile
import threading
import unittest
import urllib.error
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


servarr = load("mock_servarr", "mock-servarr.py")


class MockAsProxy(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        servarr.Handler.fixtures = REPO / "tests" / "fixtures"
        servarr.Handler.logfile = None
        servarr.Handler.record_metadata = False
        servarr.log = lambda msg: None
        cls.server = servarr.Server(("127.0.0.1", 0), servarr.Handler)
        cls.port = cls.server.server_address[1]
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def ask(self, method: str, target: str) -> tuple[int, bytes]:
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        try:
            conn.request(method, target)
            response = conn.getresponse()
            return response.status, response.read()
        finally:
            conn.close()

    def test_the_servarr_services_answer_whatever_version_asks(self):
        recorded = REPO / "tests/fixtures/metadata/radarr.servarr.com/v1/update/master.json"
        for version in ("1.0.0.1", "99.0.0.1"):
            status, body = self.ask(
                "GET", f"http://radarr.servarr.com/v1/update/master?version={version}&os=linux&runtimeVer=8.0.27")
            self.assertEqual(status, 200, version)
            self.assertEqual(json.loads(body), json.loads(recorded.read_text()), version)

    def test_the_clock_is_the_mocks(self):
        status, body = self.ask("GET", "http://services.sonarr.tv/v1/time")
        self.assertEqual(status, 200)
        self.assertIn("dateTimeUtc", json.loads(body))

    def test_other_hosts_are_refused_at_once(self):
        self.assertEqual(self.ask("CONNECT", "plex.tv:443")[0], 403)
        self.assertEqual(self.ask("GET", "http://example.com/anything")[0], 403)

    def test_the_query_still_names_other_recordings(self):
        self.assertEqual(servarr.metadata_name("/v1/tvdb/search/en/", "term=the%20pitt"), "v1/tvdb/search/en/term-the-pitt")


class RecordingFromTheRealHost(unittest.TestCase):
    """`--record-metadata` with urlopen replaced: no request leaves."""

    def fetch(self, error: Exception) -> Path:
        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root)
        stored = root / "v1" / "update" / "master" / "changes.json"
        handler = servarr.Handler.__new__(servarr.Handler)
        handler.headers = {"User-Agent": "Radarr/test"}
        with mock.patch.object(servarr.urllib.request, "urlopen", side_effect=error), \
                mock.patch.object(servarr, "log", lambda msg: None):
            handler.fetch_upstream("radarr.servarr.com", "/v1/update/master/changes", "version=1", stored)
        return stored

    def test_a_server_error_is_not_kept(self):
        error = urllib.error.HTTPError("https://radarr.servarr.com/", 526, "", {}, io.BytesIO(b"error code: 526"))
        stored = self.fetch(error)
        self.assertFalse(stored.exists())
        self.assertFalse(stored.with_suffix(".status").exists())

    def test_a_host_that_does_not_answer_leaves_it_missing(self):
        self.assertFalse(self.fetch(urllib.error.URLError("timed out")).exists())

    def test_a_client_error_is_the_hosts_answer(self):
        error = urllib.error.HTTPError("https://radarr.servarr.com/", 404, "", {}, io.BytesIO(b'{"message": "NotFound"}'))
        stored = self.fetch(error)
        self.assertEqual(json.loads(stored.read_text()), {"message": "NotFound"})
        self.assertEqual(stored.with_suffix(".status").read_text(), "404\n")


if __name__ == "__main__":
    unittest.main()
