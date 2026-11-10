#!/usr/bin/env python3
"""Replay Recyclarr's `sync sonarr` against a Sonarr API, Unarr's or the original's.

Recyclarr (github.com/recyclarr/recyclarr) syncs TRaSH guide data into Sonarr:
custom formats, quality sizes, naming, media management, quality profiles.
tests/fixtures/recyclarr/sonarr-exchange.json is what Recyclarr 8.8.1 and the
original Sonarr said to each other on the phase 1 fixture, with the
maintainer's configuration, in four phases:

- `old-config`: a sync with an older configuration, one that also scores
  FreeLeech (a format the instance does not have: it is created).
- `drift`: what a homelab drifts into between two syncs, issued by this
  script: MULTi deleted, a release group format edited, two sizes changed,
  propers preferred again, a profile's scores, cutoff and qualities edited.
- `sync-1`: the sync with the configuration as it is. It creates MULTi,
  repairs the edited format, deletes FreeLeech (`delete_old_custom_formats`),
  rewrites sizes, naming and propers, updates the seven profiles.
- `sync-2`: the same sync again. It writes the naming Recyclarr always
  writes, and nothing else.

Two ways to use it:

    # Replay: send the recorded requests, compare the answers. Needs nothing
    # but a running instance on a fresh copy of the fixture.
    scripts/recyclarr-replay.py --base-url http://127.0.0.1:8989 --api-key KEY

    # Live: run the real Recyclarr through a recording proxy, then compare
    # with the recording (--expect) or write a new one (--record).
    scripts/recyclarr-replay.py --base-url ... --api-key ... \\
        --recyclarr 'dotnet /path/to/recyclarr.dll' --guide /path/to/TRaSH-Guides \\
        --expect tests/fixtures/recyclarr/sonarr-exchange.json

The replay is sound because the answers are compared: Recyclarr decides what
to write from what it reads, so an instance that answers every recorded
request as the original did is one Recyclarr would have sent the same
requests to. The live mode checks that directly.

One answer is not compared: the original answers `PUT
qualitydefinition/update` from a five-second cache, with the sizes that were
there before; Unarr with the ones it wrote. The settings read back after each
sync are compared instead. For the same reason a live run against the
original needs `--settle 6`: without the wait Recyclarr reads stale sizes.
"""

from __future__ import annotations

import argparse
import copy
import difflib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from collections import defaultdict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent.parent / "tests" / "fixtures" / "recyclarr"
PREFIX = "/api/v3/"
# Read back after each sync.
SNAPSHOT = ["customformat", "qualitydefinition", "qualityprofile", "config/naming", "config/mediamanagement"]
# The format the older configuration scored and the current one dropped.
OLD_FORMAT = ("d7c747094a7c65f4c2de083c24899e8b", "FreeLeech")
OLD_CONFIG = f"""      - trash_ids:
          - {OLD_FORMAT[0]}  # {OLD_FORMAT[1]}
        assign_scores_to:
          - name: WEB-1080p
            score: 1
"""
# Answered from the original's five-second cache, see the module docstring.
CACHED = "qualitydefinition/update"


def send(base: str, key: str, method: str, path: str, body=None, headers=None) -> tuple[int, bytes, dict]:
    data = None if body is None else (body if isinstance(body, bytes) else json.dumps(body).encode())
    req = urllib.request.Request(f"{base.rstrip('/')}{PREFIX}{path}", data=data, method=method)
    for k, v in (headers or {"Accept": "application/json"}).items():
        req.add_header(k, v)
    if key:
        req.add_header("X-Api-Key", key)
    if data is not None and not req.has_header("Content-type"):
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return r.status, r.read(), dict(r.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read(), dict(e.headers)


def parse(raw: bytes | None):
    if not raw:
        return None
    try:
        return json.loads(raw)
    except ValueError:
        return raw.decode("utf-8", "replace")


class Api:
    """A client that keeps the exchange."""

    def __init__(self, base: str, key: str):
        self.base, self.key = base, key
        self.log: list[dict] = []

    def call(self, method: str, path: str, body=None):
        status, raw, _ = send(self.base, self.key, method, path, body)
        answer = parse(raw)
        self.log.append({"method": method, "path": path, "body": body, "status": status, "response": answer})
        if status >= 400:
            raise SystemExit(f"{method} {path}: {status} {raw[:500]!r}")
        return answer


def drift(api: Api):
    """What the maintainer could have done in the UI between two syncs, so
    that the next one creates, updates and repairs."""
    formats = api.call("GET", "customformat")
    by_name = {c["name"]: c for c in formats}
    api.call("DELETE", f"customformat/{by_name['MULTi']['id']}")
    tier = copy.deepcopy(by_name["WEB Tier 01"])
    tier["specifications"] = tier["specifications"][:-1]
    tier["includeCustomFormatWhenRenaming"] = True
    api.call("PUT", f"customformat/{tier['id']}", tier)
    sizes = api.call("GET", "qualitydefinition")
    edited = []
    for d in sizes:
        if d["quality"]["name"] in ("HDTV-720p", "WEBDL-1080p"):
            d = copy.deepcopy(d)
            d["minSize"], d["preferredSize"], d["maxSize"] = 1, 50, 100
            edited.append(d)
    api.call("PUT", "qualitydefinition/update", edited)
    mm = copy.deepcopy(api.call("GET", "config/mediamanagement"))
    mm["downloadPropersAndRepacks"] = "preferAndUpgrade"
    api.call("PUT", f"config/mediamanagement/{mm['id']}", mm)
    profiles = api.call("GET", "qualityprofile")
    hd = copy.deepcopy(next(p for p in profiles if p["name"] == "HD-1080p"))
    hd["upgradeAllowed"] = False
    for item in hd["items"]:
        item["allowed"] = not item["allowed"]
        for child in item.get("items", []):
            child["allowed"] = item["allowed"]
    hd["cutoff"] = next((i["quality"]["id"] if "quality" in i else i["id"]) for i in hd["items"] if i["allowed"])
    for fi in hd["formatItems"]:
        fi["score"] = 10
    api.call("PUT", f"qualityprofile/{hd['id']}", hd)


def snapshot(base: str, key: str) -> dict:
    api = Api(base, key)
    return {path: api.call("GET", path) for path in SNAPSHOT}


# --- live: the real Recyclarr through a recording proxy


def proxy(upstream: str, log: list) -> ThreadingHTTPServer:
    """A reverse proxy on a free local port that appends each exchange to `log`."""

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_):
            pass

        def forward(self):
            n = int(self.headers.get("Content-Length") or 0)
            data = self.rfile.read(n) if n else None
            skip = ("host", "accept-encoding", "connection", "content-length")
            headers = {k: v for k, v in self.headers.items() if k.lower() not in skip}
            path = self.path[len(PREFIX):] if self.path.startswith(PREFIX) else self.path
            status, raw, answered = send(upstream, "", self.command, path, data, headers)
            log.append({"method": self.command, "path": path, "body": parse(data), "status": status, "response": parse(raw)})
            self.send_response(status)
            for k, v in answered.items():
                if k.lower() not in ("transfer-encoding", "connection", "content-length", "content-encoding"):
                    self.send_header(k, v)
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        do_GET = do_POST = do_PUT = do_DELETE = forward

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def live(args) -> list[dict]:
    """Run the real Recyclarr: the older configuration, the drift, then the
    configuration as it is, twice. Its state (which formats it manages) lives
    in the configuration directory, so the three runs share one."""
    marker = "    #   # Example: Penalize unwanted formats\n"
    text = Path(args.config).read_text()
    if marker not in text or "sonarr:\n  series:\n" not in text:
        raise SystemExit(f"{args.config}: not the configuration this script knows how to extend")
    log: list[dict] = []
    server = proxy(args.base_url, log)
    work = Path(tempfile.mkdtemp(prefix="recyclarr-"))
    (work / "templates").mkdir()
    # Local providers only: neither the guide nor the templates are fetched.
    (work / "settings.yml").write_text(
        "resource_providers:\n"
        f"  - name: guide\n    type: trash-guides\n    path: {Path(args.guide).resolve()}\n    replace_default: true\n"
        f"  - name: templates\n    type: config-templates\n    path: {work / 'templates'}\n    replace_default: true\n"
    )
    connection = f"sonarr:\n  series:\n    base_url: http://127.0.0.1:{server.server_port}\n    api_key: {args.api_key}\n"
    current = text.replace("sonarr:\n  series:\n", connection, 1)
    older = current.replace(marker, OLD_CONFIG + "\n" + marker, 1)
    env = {k: v for k, v in os.environ.items() if k.lower() not in ("http_proxy", "https_proxy", "all_proxy")}
    env.update(RECYCLARR_CONFIG_DIR=str(work), NO_COLOR="1")

    def recyclarr(name: str, config: str) -> dict:
        (work / "recyclarr.yml").write_text(config)
        del log[:]
        done = subprocess.run(shlex.split(args.recyclarr) + ["sync", "sonarr"], env=env, capture_output=True, text=True, timeout=600)
        if done.returncode:
            raise SystemExit(f"recyclarr ({name}) failed:\n{done.stdout[-3000:]}\n{done.stderr[-3000:]}")
        output = [re.sub(r"Completed at .*", "Completed", line) for line in done.stdout.splitlines()]
        time.sleep(args.settle)
        return {"name": name, "exchange": list(log), "output": output, "after": None}

    try:
        phases = [recyclarr("old-config", older)]
        api = Api(args.base_url, args.api_key)
        drift(api)
        time.sleep(args.settle)
        phases.append({"name": "drift", "exchange": api.log, "output": None, "after": None})
        for n in (1, 2):
            phase = recyclarr(f"sync-{n}", current)
            phase["after"] = snapshot(args.base_url, args.api_key)
            phases.append(phase)
    finally:
        server.shutdown()
        shutil.rmtree(work, ignore_errors=True)
    return phases


# --- replay: the recorded requests, as they were sent


def replay(args, expected: list[dict]) -> list[dict]:
    phases = []
    for want in expected:
        exchange = []
        for e in want["exchange"]:
            status, raw, _ = send(args.base_url, args.api_key, e["method"], e["path"], e["body"])
            exchange.append({**e, "status": status, "response": parse(raw)})
        after = snapshot(args.base_url, args.api_key) if want["after"] else None
        phases.append({"name": want["name"], "exchange": exchange, "output": want["output"], "after": after})
    return phases


# --- comparison


def compare(expected, got, path="") -> list:
    if isinstance(expected, dict) and isinstance(got, dict):
        out = []
        for k in sorted(set(expected) | set(got)):
            if k not in got:
                out.append(f"{path}/{k}: missing")
            elif k not in expected:
                out.append(f"{path}/{k}: unexpected")
            else:
                out += compare(expected[k], got[k], f"{path}/{k}")
        return out
    if isinstance(expected, list) and isinstance(got, list):
        if len(expected) != len(got):
            return [f"{path}: {len(expected)} items expected, {len(got)} got"]
        return [d for i, (a, b) in enumerate(zip(expected, got)) for d in compare(a, b, f"{path}[{i}]")]
    if expected != got and not (isinstance(expected, (int, float)) and isinstance(got, (int, float)) and expected == got):
        return [f"{path}: expected {json.dumps(expected)[:80]}, got {json.dumps(got)[:80]}"]
    return []


# Values of `system/status` the two implementations do not share.
VOLATILE = {"buildTime", "startTime", "osVersion", "runtimeVersion", "packageVersion", "databaseVersion",
            "appData", "startupPath", "databaseType", "isProduction", "isDebug", "packageAuthor", "packageUpdateMechanism",
            "packageUpdateMechanismMessage", "mode", "branch", "authentication", "sqliteVersion", "migrationVersion",
            "urlBase", "runtimeName", "isNetCore", "isLinux", "isOsx", "isWindows", "isDocker", "osName",
            "isUserInteractive", "isAdmin", "instanceName", "version", "appName"}


def scrub(v):
    if isinstance(v, dict):
        return {k: scrub(x) for k, x in v.items() if k not in VOLATILE}
    if isinstance(v, list):
        return [scrub(x) for x in v]
    return v


def key(e: dict) -> str:
    return f"{e['method']} {e['path']}"


def writes(exchange: list) -> list:
    return [key(e) for e in exchange if e["method"] != "GET"]


def invariants(phases: list[dict]) -> list[str]:
    """What must hold of any run, whatever it is compared with."""
    by_name = {p["name"]: p for p in phases}
    failures = []
    extra = [w for w in writes(by_name["sync-2"]["exchange"]) if not w.startswith("PUT config/naming/")]
    if extra:
        failures.append(f"the second sync wrote {extra}")
    first, second = by_name["sync-1"]["after"], by_name["sync-2"]["after"]
    if json.dumps(first, sort_keys=True) != json.dumps(second, sort_keys=True):
        failures += [f"after the second sync {d}" for d in compare(first, second)]
    return failures


def differences(expected: list[dict], got: list[dict]) -> list[str]:
    failures = []
    for want, have in zip(expected, got):
        name = want["name"]
        # Recyclarr sends some reads concurrently: pair by request, not by position.
        if sorted(map(key, want["exchange"])) != sorted(map(key, have["exchange"])):
            failures.append(f"{name}: requests differ:\n  expected {list(map(key, want['exchange']))}\n  got      {list(map(key, have['exchange']))}")
            continue
        queues = defaultdict(list)
        for e in have["exchange"]:
            queues[key(e)].append(e)
        for we in want["exchange"]:
            ge = queues[key(we)].pop(0)
            where = f"{name}: {key(we)}"
            if we["status"] != ge["status"]:
                failures.append(f"{where}: status {we['status']} expected, {ge['status']} got")
            failures += [f"{where} body{d}" for d in compare(scrub(we["body"]), scrub(ge["body"]))]
            if we["path"] != CACHED:
                failures += [f"{where} response{d}" for d in compare(scrub(we["response"]), scrub(ge["response"]))]
        if want["after"]:
            failures += [f"{name}: settings after{d}" for d in compare(scrub(want["after"]), scrub(have["after"]))]
        if want["output"] != have["output"]:
            diff = difflib.unified_diff(want["output"] or [], have["output"] or [], lineterm="", n=0)
            failures.append(f"{name}: Recyclarr's output differs:\n  " + "\n  ".join(diff))
    return failures


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--api-key", required=True)
    ap.add_argument("--expect", default=str(FIXTURES / "sonarr-exchange.json"), help="the recording to replay or compare with")
    ap.add_argument("--recyclarr", help="live: the command that runs the real Recyclarr, e.g. 'dotnet recyclarr.dll'")
    ap.add_argument("--guide", help="live: a checkout of TRaSH-Guides (the commit in the fixtures' README)")
    ap.add_argument("--config", default=str(FIXTURES / "recyclarr.yml"), help="live: the Recyclarr configuration")
    ap.add_argument("--record", help="live: write the exchange here instead of comparing")
    ap.add_argument("--settle", type=float, default=0, help="live: seconds to wait after each write phase (6 for the original)")
    args = ap.parse_args()

    if args.recyclarr:
        if not args.guide:
            ap.error("--recyclarr needs --guide")
        got = live(args)
    elif args.record:
        ap.error("--record needs --recyclarr")
    else:
        got = replay(args, json.loads(Path(args.expect).read_text()))
    for p in got:
        if p["name"] != "drift":
            print(f"{p['name']}: {len(p['exchange'])} requests, writes: {', '.join(writes(p['exchange'])) or 'none'}")

    failures = invariants(got)
    if args.record:
        Path(args.record).write_text(json.dumps(got, indent=1, sort_keys=True, ensure_ascii=False) + "\n")
    else:
        failures += differences(json.loads(Path(args.expect).read_text()), got)
    for f in failures:
        print("FAIL", f)
    if not failures:
        print("ok: the second sync changed nothing" + ("" if args.record else ", the exchange matches the original's"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
