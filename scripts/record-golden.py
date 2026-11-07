#!/usr/bin/env python3
"""Replay a list of API requests and store each response as a golden file.

Request list format, one per line (blank lines and ``#`` comments ignored)::

    <name> <METHOD> <path-with-query>

Each golden file is ``<name>.json`` holding ``{"request": {...}, "status": N,
"body": <json or null>}``. Pretty-printed with sorted keys so diffs stay readable.

Names starting with ``noauth`` are sent without credentials. Names starting
with ``session`` are sent with the cookie obtained by a forms login
(``--login user:password``) instead of the API key, which is how a browser
reaches ``/initialize.json`` when forms authentication is on.

Point it at the real Sonarr to record, or at Unarr to compare by hand::

    scripts/record-golden.py --base-url http://127.0.0.1:8989 --api-key KEY \
        --requests tests/fixtures/sonarr/golden/requests.txt --out /tmp/unarr-golden
"""

from __future__ import annotations

import argparse
import datetime
import http.cookiejar
import json
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--api-key", required=True)
    ap.add_argument("--requests", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--login", help="user:password for the forms login used by session-* requests")
    args = ap.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    base = args.base_url.rstrip("/")

    # Two clients: one bare (API key per request), one with a cookie jar that
    # has been through the login form.
    bare = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    session = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    if args.login:
        user, _, password = args.login.partition(":")
        form = urllib.parse.urlencode({"username": user, "password": password, "rememberMe": "on"}).encode()
        with session.open(urllib.request.Request(base + "/login", data=form, method="POST"), timeout=60) as resp:
            print(f"[golden] login as {user!r}: {resp.status} {resp.geturl()}", file=sys.stderr)
    count = 0
    for line in args.requests.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        name, method, path = line.split(maxsplit=2)
        req = urllib.request.Request(base + path, method=method)
        client = bare
        if name.startswith("session"):
            client = session
        elif "apikey=" not in path and not name.startswith("noauth"):
            req.add_header("X-Api-Key", args.api_key)
        try:
            with client.open(req, timeout=60) as resp:
                status, raw = resp.status, resp.read()
        except urllib.error.HTTPError as e:
            status, raw = e.code, e.read()
        try:
            body = json.loads(raw) if raw else None
        except json.JSONDecodeError:
            body = raw.decode(errors="replace")
        golden = {
            "request": {"method": method, "path": path},
            # The instant the answer was computed for: the contract tests pin
            # Unarr's clock to it, so fields such as nextAiring or the wanted
            # list do not drift as the calendar moves on.
            "recordedAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "status": status,
            "body": body,
        }
        (args.out / f"{name}.json").write_text(json.dumps(golden, indent=2, sort_keys=True, ensure_ascii=False) + "\n")
        print(f"[golden] {status} {method} {path} -> {name}.json", file=sys.stderr)
        count += 1
    print(f"[golden] {count} responses recorded in {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
