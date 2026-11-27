#!/usr/bin/env python3
"""Replay the requests Prowlarr makes to a Sonarr or Radarr instance, against Unarr.

Prowlarr (github.com/Prowlarr/Prowlarr, src/NzbDrone.Core/Applications/Sonarr
and .../Radarr) syncs its indexers into the application through
`SonarrV3Proxy` or `RadarrV3Proxy`, which make the same calls: it reads
`system/status`, the indexer schema and the indexer list, tests each indexer
it built from the Newznab or Torznab schema (`POST indexer/test`, reading the
`X-Application-Version` header), adds it (`POST indexer`, retried with
`?forceSave=true` on a 400), updates it (`PUT indexer/{id}?forceSave=true`),
reads it back (`GET indexer/{id}`, 404 when gone) and removes it
(`DELETE indexer/{id}`). This script issues those requests with the `X-Api-Key`
header as Prowlarr does and checks the status codes, the bodies Prowlarr reads
and the validation failures it shows. It is a stand-in for running Prowlarr.

The indexer it registers points at the mock indexer of `scripts/mock-servarr.py`
(the fixtures under tests/fixtures/indexers), which must be reachable from
the Unarr under test; `--mock` starts one here on 127.0.0.1:18989.

    scripts/prowlarr-replay.py --base-url http://127.0.0.1:8989 --api-key KEY --mock
    scripts/prowlarr-replay.py --app radarr --base-url http://127.0.0.1:7878 --api-key KEY --mock

With `--app radarr` the indexers are the mock's movie ones (under /movies,
categories 2000 on), no anime categories are sent, and one more case is
replayed: an indexer whose feed is empty, which Radarr refuses with and
without `forceSave`. That is what the maintainer's Radarr answered to
Prowlarr 2.7.0.5723 twelve times in its trace log (POST indexer, 400, then
POST indexer?forceSave=true, 400), so Prowlarr keeps such an indexer out.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

MOCK_API_KEY = "0123456789abcdef0123456789abcdef"
MIN_VERSION = (3, 0, 5)


class Api:
    def __init__(self, base: str, key: str):
        self.base = base.rstrip("/")
        self.key = key

    def call(self, method: str, path: str, body=None, key: str | None = None):
        """(status, headers, decoded body); errors are answers, not exceptions."""
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(f"{self.base}/api/v3/{path}", data=data, method=method)
        req.add_header("X-Api-Key", self.key if key is None else key)
        req.add_header("Accept", "application/json")
        if data is not None:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                raw = r.read()
                return r.status, dict(r.headers), decode(raw)
        except urllib.error.HTTPError as e:
            return e.code, dict(e.headers), decode(e.read())


def decode(raw: bytes):
    if not raw:
        return None
    try:
        return json.loads(raw)
    except ValueError:
        return raw.decode(errors="replace")


APPS = {
    "sonarr": {
        "name": "Sonarr",
        "prefix": "",
        "categories": [5030, 5040, 5045],
        "extra": {"animeCategories": [5070]},
        "empty": None,
        "test_status": 200,
        "test_answer": {},
    },
    "radarr": {
        "name": "Radarr",
        "prefix": "/movies",
        "categories": [2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060],
        "extra": {},
        "empty": "/movies/empty",
        # Radarr 7 answers a passed test 204 with no body (typed results);
        # Radarr 6.4.4 answered 200 with the string `{}`.
        "test_status": 204,
        "test_answer": None,
    },
}


class Replay:
    def __init__(self, api: Api, indexer_url: str, indexer_key: str, keep: bool, app: str = "sonarr"):
        self.app = APPS[app]
        self.api = api
        self.indexer_url = indexer_url.rstrip("/")
        self.indexer_key = indexer_key
        self.keep = keep
        self.problems: list[str] = []
        self.created: list[int] = []

    def check(self, ok: bool, what: str):
        print(f"[prowlarr] {'ok  ' if ok else 'FAIL'} {what}")
        if not ok:
            self.problems.append(what)

    # -- the proxy's requests ------------------------------------------------

    def status(self):
        code, headers, body = self.api.call("GET", "system/status")
        self.check(code == 200 and isinstance(body, dict), "GET system/status answers 200")
        version = parse_version(header(headers, "X-Application-Version") or "")
        self.check(
            version >= MIN_VERSION,
            f"X-Application-Version {header(headers, 'X-Application-Version')!r} is at least 3.0.5",
        )
        self.check(body.get("appName") == self.app["name"], f"system/status appName is {self.app['name']}")
        code, _, body = self.api.call("GET", "system/status", key="wrong")
        self.check(code == 401, "a wrong X-Api-Key is refused with 401 (Prowlarr: 'API Key is invalid')")

    def schema(self) -> dict:
        code, _, schemas = self.api.call("GET", "indexer/schema")
        self.check(code == 200 and isinstance(schemas, list), "GET indexer/schema answers a list")
        by_impl = {s.get("implementation"): s for s in schemas or []}
        for impl, contract in [("Newznab", "NewznabSettings"), ("Torznab", "TorznabSettings")]:
            s = by_impl.get(impl)
            self.check(s is not None and s.get("configContract") == contract, f"schema has {impl} with {contract}")
            names = {f.get("name") for f in (s or {}).get("fields", [])}
            self.check(
                {"baseUrl", "apiPath", "apiKey", "categories", *self.app["extra"]} <= names,
                f"{impl} schema lists the fields Prowlarr sets",
            )
        return by_impl

    def build(self, schema: dict, name: str, base_url: str, torznab: bool) -> dict:
        """`BuildSonarrIndexer` / `BuildRadarrIndexer`: the schema's fields with Prowlarr's values."""
        values = {
            "baseUrl": base_url,
            "apiPath": "/api",
            "apiKey": self.indexer_key,
            "categories": self.app["categories"],
            **self.app["extra"],
        }
        if torznab:
            values.update({"minimumSeeders": 2, "seedCriteria.seedRatio": 1.5, "seedCriteria.seedTime": 120})
        fields = []
        for f in schema["fields"]:
            field = {"name": f["name"]}
            if f["name"] in values:
                field["value"] = values[f["name"]]
            elif "value" in f:
                field["value"] = f["value"]
            fields.append(field)
        return {
            "name": name,
            "enableRss": True,
            "enableAutomaticSearch": True,
            "enableInteractiveSearch": True,
            "priority": 25,
            "implementation": schema["implementation"],
            "configContract": schema["configContract"],
            "fields": fields,
            "tags": [],
        }

    def test(self, body: dict, expect_ok: bool, what: str):
        code, headers, answer = self.api.call("POST", "indexer/test", body)
        if expect_ok:
            # Sonarr answers `{}`; Radarr 7 answers 204 with no body, as its
            # recordings show (Radarr 6.4.4 answered the string `{}`).
            # Prowlarr reads the status and the version header only.
            status, passed = self.app["test_status"], self.app["test_answer"]
            self.check(
                code == status and answer == passed,
                f"POST indexer/test {what} answers {status} {json.dumps(passed)}"
                + ("" if code == status and answer == passed else f" (got {code} {short(answer)})"),
            )
            self.check(header(headers, "X-Application-Version") is not None, "the test answer carries X-Application-Version")
        else:
            self.check(code == 400 and isinstance(answer, list) and answer, f"POST indexer/test {what} answers 400 with failures")
            if isinstance(answer, list):
                for f in answer:
                    self.check(
                        isinstance(f, dict) and "propertyName" in f and "errorMessage" in f,
                        f"failure has propertyName/errorMessage: {short(f)}",
                    )

    def add(self, body: dict, what: str) -> int | None:
        """`AddIndexer`: POST, then POST ?forceSave=true on a 400."""
        code, headers, answer = self.api.call("POST", "indexer", body)
        if code == 400:
            code, headers, answer = self.api.call("POST", "indexer?forceSave=true", body)
        self.check(code == 201 and isinstance(answer, dict) and isinstance(answer.get("id"), int), f"POST indexer {what} answers 201 with an id")
        if not isinstance(answer, dict) or not isinstance(answer.get("id"), int):
            return None
        self.check(bool(header(headers, "Location")), "the 201 carries a Location header")
        self.created.append(answer["id"])
        return answer["id"]

    def add_unreachable(self, body: dict):
        """The originals refuse an indexer they cannot reach even with `forceSave=true`:
        the flag only drops warnings, and a connection failure is an error."""
        for path in ("indexer", "indexer?forceSave=true"):
            code, _, answer = self.api.call("POST", path, body)
            self.check(
                code == 400
                and isinstance(answer, list)
                and any("Unable to connect to indexer" in (f.get("errorMessage") or "") for f in answer),
                f"POST {path} for an unreachable indexer answers 400 'Unable to connect to indexer'",
            )

    def add_empty(self, body: dict):
        """Radarr refuses an indexer whose test feed has no release, forced or not."""
        for path in ("indexer", "indexer?forceSave=true"):
            code, _, answer = self.api.call("POST", path, body)
            self.check(
                code == 400
                and isinstance(answer, list)
                and any("no results in the configured categories" in (f.get("errorMessage") or "") for f in answer),
                f"POST {path} for an indexer without releases answers 400 'no results in the configured categories'",
            )

    def get(self, id: int) -> dict | None:
        code, _, answer = self.api.call("GET", f"indexer/{id}")
        if code == 404:
            return None
        self.check(code == 200 and isinstance(answer, dict), f"GET indexer/{id} answers 200")
        return answer if isinstance(answer, dict) else None

    def update(self, resource: dict):
        code, _, answer = self.api.call("PUT", f"indexer/{resource['id']}?forceSave=true", resource)
        self.check(code == 202 and isinstance(answer, dict), f"PUT indexer/{resource['id']} answers 202")
        return answer

    def remove(self, id: int):
        code, _, answer = self.api.call("DELETE", f"indexer/{id}")
        self.check(code == 200, f"DELETE indexer/{id} answers 200")

    # -- the sync Prowlarr performs -----------------------------------------

    def run(self):
        self.status()
        schemas = self.schema()
        if self.problems:
            return
        code, _, before = self.api.call("GET", "indexer")
        self.check(code == 200 and isinstance(before, list), "GET indexer answers a list")
        before_ids = {i.get("id") for i in before or []}

        indexers = self.indexer_url + self.app["prefix"]
        newznab = self.build(schemas["Newznab"], "Mock Newznab (Prowlarr replay)", f"{indexers}/newznab", False)
        torznab = self.build(schemas["Torznab"], "Mock Torznab (Prowlarr replay)", f"{indexers}/torznab", True)
        self.test(newznab, True, "Newznab")
        self.test(torznab, True, "Torznab")
        wrong_key = dict(newznab, name="Mock Newznab (wrong key)")
        wrong_key["fields"] = [dict(f, value="not-the-key") if f["name"] == "apiKey" else f for f in newznab["fields"]]
        self.test(wrong_key, False, "with a wrong indexer key")

        newznab_id = self.add(newznab, "Newznab")
        torznab_id = self.add(torznab, "Torznab")
        unreachable = self.build(schemas["Newznab"], "Mock Newznab (unreachable)", "http://127.0.0.1:18999/newznab", False)
        self.add_unreachable(unreachable)
        if self.app["empty"]:
            empty = self.build(schemas["Newznab"], "Mock Newznab (no releases)", self.indexer_url + self.app["empty"], False)
            self.add_empty(empty)
        code, _, answer = self.api.call("POST", "indexer", newznab)
        self.check(
            code == 400 and isinstance(answer, list) and any("unique" in (f.get("errorMessage") or "").lower() for f in answer),
            "POST indexer with a name already taken answers 400 'Should be unique'",
        )

        if newznab_id is not None:
            got = self.get(newznab_id)
            fields = {f["name"]: f.get("value") for f in (got or {}).get("fields", [])}
            self.check(fields.get("baseUrl") == f"{indexers}/newznab", "the stored baseUrl is what Prowlarr sent")
            self.check(fields.get("categories") == self.app["categories"], "the stored categories are what Prowlarr sent")
            self.check(got is not None and got.get("priority") == 25, "the stored priority is what Prowlarr sent")
            if got is not None:
                got["priority"] = 10
                got["enableRss"] = False
                updated = self.update(got)
                self.check(updated.get("priority") == 10 and updated.get("enableRss") is False, "PUT changes priority and enableRss")
                again = self.get(newznab_id) or {}
                self.check(again.get("priority") == 10, "GET after PUT shows the change")
        if torznab_id is not None:
            got = self.get(torznab_id) or {}
            fields = {f["name"]: f.get("value") for f in got.get("fields", [])}
            self.check(fields.get("minimumSeeders") == 2, "Torznab minimumSeeders is stored")
            self.check(fields.get("seedCriteria.seedRatio") == 1.5, "Torznab seedCriteria.seedRatio is stored")
            self.check(fields.get("seedCriteria.seedTime") == 120, "Torznab seedCriteria.seedTime is stored")

        code, _, listed = self.api.call("GET", "indexer")
        ids = {i.get("id") for i in listed or []}
        self.check(
            {i for i in (newznab_id, torznab_id) if i is not None} <= ids,
            "GET indexer lists the indexers Prowlarr added",
        )
        self.check(before_ids <= ids, "the indexers that were there before are still listed")
        self.check(self.get(999_999) is None, "GET indexer/{missing} answers 404")

        if not self.keep:
            for id in list(self.created):
                self.remove(id)
            self.remove(999_999)  # the originals answer 200 for a missing id
            code, _, listed = self.api.call("GET", "indexer")
            ids = {i.get("id") for i in listed or []}
            self.check(not (set(self.created) & ids), "DELETE removed the indexers Prowlarr added")
            self.check(all(self.get(i) is None for i in self.created), "GET indexer/{id} answers 404 after DELETE")


def header(headers: dict, name: str) -> str | None:
    for k, v in headers.items():
        if k.lower() == name.lower():
            return v
    return None


def parse_version(text: str) -> tuple:
    parts = []
    for p in text.split("."):
        try:
            parts.append(int(p))
        except ValueError:
            break
    return tuple(parts)


def short(v) -> str:
    s = json.dumps(v)
    return s if len(s) < 100 else s[:97] + "..."


def start_mock(fixtures: Path, listen: str) -> subprocess.Popen:
    script = Path(__file__).with_name("mock-servarr.py")
    proc = subprocess.Popen(
        [sys.executable, "-I", str(script), "--listen", listen, "--fixtures", str(fixtures)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    host, port = listen.rsplit(":", 1)
    for _ in range(50):
        try:
            urllib.request.urlopen(f"http://{host}:{port}/newznab/api?t=caps&apikey={MOCK_API_KEY}", timeout=1).read()
            return proc
        except Exception:
            time.sleep(0.1)
    proc.kill()
    raise SystemExit(f"the mock indexer did not come up on {listen}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--app", choices=sorted(APPS), default="sonarr", help="the application Prowlarr syncs into")
    ap.add_argument("--base-url", required=True, help="Unarr, e.g. http://127.0.0.1:8989 (7878 for Radarr)")
    ap.add_argument("--api-key", required=True, help="Unarr's API key")
    ap.add_argument("--indexer-url", default="http://127.0.0.1:18989", help="the mock indexer as Unarr reaches it")
    ap.add_argument("--indexer-key", default=MOCK_API_KEY)
    ap.add_argument("--mock", action="store_true", help="start scripts/mock-servarr.py on 127.0.0.1:18989 for the run")
    ap.add_argument("--fixtures", default=str(Path(__file__).resolve().parents[1] / "tests" / "fixtures"))
    ap.add_argument("--keep", action="store_true", help="leave the indexers added by the replay in place")
    args = ap.parse_args()

    mock = start_mock(Path(args.fixtures), "127.0.0.1:18989") if args.mock else None
    try:
        replay = Replay(Api(args.base_url, args.api_key), args.indexer_url, args.indexer_key, args.keep, args.app)
        replay.run()
    finally:
        if mock is not None:
            mock.terminate()
            mock.wait(timeout=5)
    if replay.problems:
        print(f"[prowlarr] {len(replay.problems)} problems")
        sys.exit(1)
    print(f"[prowlarr] every request Prowlarr makes is answered as {APPS[args.app]['name']} answers it")


if __name__ == "__main__":
    main()
