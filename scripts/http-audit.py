#!/usr/bin/env python3
"""Compare what two servers answer outside the JSON body: status and headers.

The golden replay compares bodies. This sends the same requests to the
original (``--original``) and to Unarr (``--unarr``), both running on the
same fixture, and reports where the status line or the headers a client can
depend on differ. It only reads: the GET lines of a golden request list,
plus probes of the edges (no key, a wrong key, an unknown route, a method
the route does not take, a body that is not JSON, a preflight).

    scripts/radarr-fixture.sh shell &                 # the original on 7878
    unarrd --personalities radarr ...                  # Unarr on another port
    scripts/http-audit.py --original http://127.0.0.1:7878 \
        --unarr http://127.0.0.1:7879 \
        --requests tests/fixtures/radarr/golden/requests.txt

Exit status 1 when something differs, so it can gate a release.
"""

import argparse
import http.client
import sys
import urllib.parse
from pathlib import Path

API_KEY = "0123456789abcdef0123456789abcdef"

# Headers whose presence and value a client, a proxy or a browser acts on.
COMPARED = [
    "content-type",
    "cache-control",
    "pragma",
    "expires",
    "vary",
    "location",
    "www-authenticate",
    "content-encoding",
    "content-disposition",
    "access-control-allow-origin",
    "access-control-allow-methods",
    "access-control-allow-headers",
    "x-application-version",
    "x-content-type-options",
    "allow",
]

# Present or absent, whatever the value.
PRESENCE = ["set-cookie", "etag", "last-modified"]


def probes(api: str) -> list[tuple[str, str, str, dict[str, str], bytes | None]]:
    """The edges: `(name, method, path, headers, body)`."""
    key = {"X-Api-Key": API_KEY}
    json = {"X-Api-Key": API_KEY, "Content-Type": "application/json"}
    return [
        ("no-key", "GET", f"{api}/system/status", {}, None),
        ("wrong-key", "GET", f"{api}/system/status", {"X-Api-Key": "nope"}, None),
        ("key-in-query", "GET", f"{api}/system/status?apikey={API_KEY}", {}, None),
        ("bearer-key", "GET", f"{api}/system/status", {"Authorization": f"Bearer {API_KEY}"}, None),
        ("unknown-route", "GET", f"{api}/nosuchthing", key, None),
        ("unknown-id", "GET", f"{api}/movie/999999", key, None),
        ("id-not-a-number", "GET", f"{api}/movie/abc", key, None),
        ("method-not-taken", "DELETE", f"{api}/system/status", key, None),
        ("head", "HEAD", f"{api}/system/status", key, None),
        ("preflight", "OPTIONS", f"{api}/movie", {"Origin": "http://example.invalid", "Access-Control-Request-Method": "GET"}, None),
        ("gzip", "GET", f"{api}/movie", {**key, "Accept-Encoding": "gzip"}, None),
        ("trailing-slash", "GET", f"{api}/tag/", key, None),
        ("upper-case-path", "GET", f"{api.upper()}/TAG", key, None),
        ("not-json", "POST", f"{api}/command", json, b"{not json"),
        ("empty-body", "POST", f"{api}/command", json, b""),
        ("form-body", "POST", f"{api}/command", {**key, "Content-Type": "application/x-www-form-urlencoded"}, b"name=RssSync"),
        ("command-no-name", "POST", f"{api}/command", json, b"{}"),
        ("command-unknown", "POST", f"{api}/command", json, b'{"name":"Nope"}'),
        ("release-grab-invalid", "POST", f"{api}/release", json, b'{"guid":"","indexerId":0}'),
        ("release-grab-uncached", "POST", f"{api}/release", json, b'{"guid":"nope","indexerId":1}'),
        ("release-push-invalid", "POST", f"{api}/release/push", json, b'{"title":""}'),
        ("queue-delete-missing", "DELETE", f"{api}/queue/123456", key, None),
        ("queue-grab-missing", "POST", f"{api}/queue/grab/123456", json, b""),
        ("command-get-missing", "GET", f"{api}/command/999999", key, None),
        ("command-cancel-missing", "DELETE", f"{api}/command/999999", key, None),
        ("ping", "GET", "/ping", {}, None),
        ("root", "GET", "/", {}, None),
        ("login-page", "GET", "/login", {}, None),
        ("initialize", "GET", "/initialize.json", {}, None),
        ("feed-no-key", "GET", "/feed/v3/calendar/Radarr.ics", {}, None),
    ]


def golden_gets(requests: Path) -> list[tuple[str, str, str, dict[str, str], bytes | None]]:
    out = []
    for line in requests.read_text().splitlines():
        parts = line.split()
        if len(parts) < 3 or line.startswith("#") or parts[1] != "GET" or "{id:" in parts[2]:
            continue
        name, _, path = parts[:3]
        headers = {} if name.startswith(("noauth", "session")) or "apikey=" in path else {"X-Api-Key": API_KEY}
        out.append((name, "GET", path, headers, None))
    return out


def ask(base: str, method: str, path: str, headers: dict[str, str], body: bytes | None):
    url = urllib.parse.urlsplit(base)
    conn = http.client.HTTPConnection(url.hostname, url.port, timeout=30)
    try:
        conn.request(method, path, body=body, headers=headers)
        response = conn.getresponse()
        response.read()
        return response.status, {k.lower(): v for k, v in response.getheaders()}
    finally:
        conn.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--original", required=True, help="base URL of the original")
    parser.add_argument("--unarr", required=True, help="base URL of Unarr")
    parser.add_argument("--requests", type=Path, help="a golden request list; its GET lines are replayed")
    parser.add_argument("--api", default="/api/v3")
    args = parser.parse_args()

    cases = probes(args.api)
    if args.requests:
        cases += golden_gets(args.requests)
    different = 0
    for name, method, path, headers, body in cases:
        try:
            want_status, want = ask(args.original, method, path, headers, body)
            got_status, got = ask(args.unarr, method, path, headers, body)
        except OSError as e:
            print(f"{name}: {method} {path}: {e}")
            different += 1
            continue
        problems = []
        if want_status != got_status:
            problems.append(f"status {want_status} -> {got_status}")
        for header in COMPARED:
            if want.get(header) != got.get(header):
                problems.append(f"{header}: {want.get(header)!r} -> {got.get(header)!r}")
        for header in PRESENCE:
            if (header in want) != (header in got):
                problems.append(f"{header}: {'present' if header in want else 'absent'} -> {'present' if header in got else 'absent'}")
        if problems:
            different += 1
            print(f"{name}: {method} {path}")
            for problem in problems:
                print(f"    {problem}")
    print(f"{len(cases)} requests, {different} with a difference (original -> Unarr)")
    return 1 if different else 0


if __name__ == "__main__":
    sys.exit(main())
