#!/usr/bin/env python3
"""Replay a list of API requests and store each response as a golden file.

Request list format, one per line (blank lines and ``#`` comments ignored)::

    <name> <METHOD> <path-with-query> [body-file]

The optional fourth column names a JSON file, relative to the request list,
sent as the request body with ``Content-Type: application/json``. A path may
carry ``{id:name}``: the ``id`` the response recorded as ``name`` returned,
for resources the application numbers itself (commands). ``sleep N`` lines
pause the recording for the application's background work.

``mock <name> <METHOD> <path> [body-file]`` sends the request to the mock
services (``--mock-url``, the ``scripts/mock-servarr.py`` the application is
pointed at) instead of the application: to complete a download, or to read
back what the mock's notification targets received. The response is recorded
as ``<name>.json`` like any other, unless ``<name>`` is ``-``.

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
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def substitute_ids(path: str, ids: dict[str, str]) -> str:
    """Replace each ``{id:name}`` with the id the response named ``name`` carried."""
    while "{id:" in path:
        start = path.index("{id:")
        end = path.index("}", start)
        name = path[start + 4:end]
        if name not in ids:
            sys.exit(f"[golden] {path}: no id recorded for {name!r}")
        path = path[:start] + ids[name] + path[end + 1:]
    return path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--api-key", required=True)
    ap.add_argument("--requests", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--login", help="user:password for the forms login used by session-* requests")
    ap.add_argument("--mock-url", default="http://127.0.0.1:18989", help="where `mock` lines are sent")
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
    # Ids the responses carried, for `{id:name}` in later paths.
    ids: dict[str, str] = {}
    for line in args.requests.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        name, method, path, *rest = line.split(maxsplit=3) if not line.startswith("sleep ") else (line.split()[0], line.split()[1], "", [])
        if name == "sleep":
            # `sleep N`: let the originals' background work (debounced
            # refreshes, command queue) catch up before the next request.
            print(f"[golden] sleeping {method}s", file=sys.stderr)
            time.sleep(float(method))
            continue
        # `mock <name> METHOD path [body]`: aimed at the mock services.
        to_mock = name == "mock"
        if to_mock:
            name, method, path, *rest = line.split(maxsplit=4)[1:]
        data = None
        if rest:
            data = (args.requests.parent / rest[0]).read_bytes()
        path = substitute_ids(path, ids)
        target = args.mock_url.rstrip("/") if to_mock else base
        req = urllib.request.Request(target + urllib.parse.quote(path, safe="/?&=:%+,-._~"), data=data, method=method)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        client = bare
        if to_mock:
            pass
        elif name.startswith("session"):
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
        if isinstance(body, dict) and isinstance(body.get("id"), int):
            ids[name] = str(body["id"])
        if to_mock and name == "-":
            print(f"[golden] mock {status} {method} {path} (not recorded)", file=sys.stderr)
            continue
        golden = {
            "request": {"method": method, "path": path, **({"body": rest[0]} if rest else {})},
            # The instant the answer was computed for: the contract tests pin
            # Unarr's clock to it, so fields such as nextAiring or the wanted
            # list do not drift as the calendar moves on.
            "recordedAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "status": status,
            "body": body,
        }
        if to_mock:
            golden["request"]["target"] = "mock"
        (args.out / f"{name}.json").write_text(json.dumps(golden, indent=2, sort_keys=True, ensure_ascii=False) + "\n")
        print(f"[golden] {status} {method} {path} -> {name}.json", file=sys.stderr)
        count += 1
    print(f"[golden] {count} responses recorded in {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
