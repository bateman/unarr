#!/usr/bin/env python3
"""Browser smoke test of the embedded web UIs: open every page, fail on errors.

Unarr serves the official Sonarr and Radarr UIs, taken from the pinned
releases' tarballs (``make ui``). When a pin moves, a new UI arrives over
Unarr's backend; this script says whether it works. For each personality it:

- builds ``unarrd`` (``cargo build``, the binary at the path cargo reports;
  ``--unarrd`` names a prebuilt one) and starts it on a fresh copy of the
  fixture (``tests/fixtures/<app>``, never the fixture itself), both
  personalities in one process as ``make run-both`` does, on free ports of
  127.0.0.1;
- opens a headless Chromium (Playwright), logs in with the fixture's user
  when the UI asks, and opens every page of the UI's menu: the sidebar is
  read on each page, so a section's sub-pages are found once the section is
  open (Activity, Wanted, Settings, System). The library page gives the
  detail page of its first series or movie. ``MINIMUM`` below is a fixed
  list, a safety net: those pages are opened even when the menu no longer
  shows them, and the report says so;
- fails a page on a console error, an unhandled exception of the page, a
  request to unarrd answered with 4xx or 5xx (or not answered), and a page
  still showing the UI's loading indicator after ``--settle`` seconds or
  showing its error state (an error boundary, the error page, "not found",
  a danger alert). The selectors are the UI's CSS module classes
  (``SELECTORS``); the run checks that the UI still defines each of them, so
  a renamed component fails the run instead of blinding it.

Nothing leaves the machine. The browser aborts every request that is not to
unarrd and lists it. unarrd's metadata hosts (Skyhook, api.radarr.video,
...) are redirected (``UNARR_METADATA_UPSTREAM``) to a stand-in on the
loopback that answers 503 and lists what it was asked; the fixtures' download
clients, indexers and notifications point at ``.invalid`` hosts.

A failing answer that is right for the fixture is admitted only by an entry
of ``ALLOWED``: personality, method, path (a regular expression matched
against the whole path, without the query) and status, narrowed further by
a pattern its body must hold or a file that must be missing, with the reason
written beside it. An entry no page needed is reported as unused.

Usage::

    make ui-smoke                     # installs Playwright, builds unarrd, opens both UIs
    make ui-smoke SMOKE_APP=radarr UI_SMOKE_CHANNEL=chrome
    .cache/ui-smoke-venv/bin/python -I scripts/ui-smoke.py --app sonarr --only '^/settings'
    .cache/ui-smoke-venv/bin/python -I scripts/ui-smoke.py --app radarr --only '^$' --page /system/logs/files/update

Needs Python's ``playwright`` (``scripts/ui-smoke-requirements.txt``;
``make ui-smoke`` installs it in ``<UNARR_CACHE or .cache>/ui-smoke-venv``)
and a Chromium: Playwright's own, installed once with
``.cache/ui-smoke-venv/bin/python -m playwright install chromium``, or an
installed Google Chrome with ``--channel chrome`` (``UI_SMOKE_CHANNEL=chrome``),
as CI does. ``make test-scripts`` tests the rest without either.

Output: a summary on stdout, the progress on stderr, and under ``--out``
(default ``<UNARR_CACHE or .cache>/ui-smoke``): ``report.md`` for a person,
``report.json`` for a program (every page, every failure with its request,
status and the start of the answer's body), ``screenshots/`` of the pages
that failed and ``unarrd.log``. Exit status: 0 when every page passed, 1 when
one failed, 2 when the run could not start.
"""

from __future__ import annotations

import argparse
import http.server
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlsplit

REPO = Path(__file__).resolve().parent.parent
APPS = ("sonarr", "radarr")
# The fixtures' forms login (tests/fixtures/<app>/README.md; make golden uses it too).
LOGIN = ("unarr", "fixture")
MARKER = ".ui-smoke"


@dataclass(frozen=True)
class Allowed:
    """A failing answer that is right for the fixture."""

    apps: tuple[str, ...]
    method: str
    path: str  # regular expression, matched against the whole path without the query
    status: int
    reason: str
    # A regular expression the answer's body must contain.
    body: str | None = None
    # A path under the personality's data folder, formatted with the groups
    # of `path`: the answer is admitted only while nothing is there.
    absent: str | None = None

    def matches(self, app: str, method: str, path: str, status: int, body: str, data: Path) -> bool:
        if app not in self.apps or method != self.method or status != self.status:
            return False
        m = re.fullmatch(self.path, path)
        if m is None:
            return False
        if self.body is not None and re.search(self.body, body) is None:
            return False
        if self.absent is not None and (data / app / self.absent.format(*m.groups())).exists():
            return False
        return True


# Keep each entry narrow (one method, one path, one status, and a body or a
# file that shows why) and say why the answer is right: a broad pattern
# would hide the defects this test is for.
ALLOWED: list[Allowed] = [
    Allowed(
        ("sonarr", "radarr"), "GET", r"/MediaCover/(\d+)/(?:poster|fanart|banner|clearlogo|headshot)(?:-\d+)?\.(?:jpg|png)", 404,
        "the fixture holds no artwork: the originals download it from the metadata servers, which the "
        "smoke test does not reach, and answer 404 for a cover that is not on disk, as Unarr does; the "
        "UI then shows its placeholder. Admitted only while the item has no cover folder.",
        absent="MediaCover/{0}",
    ),
    Allowed(
        ("radarr",), "GET", r"/MediaCoverProxy/[0-9a-f]{64}/[A-Za-z0-9_-]+\.(?:jpg|png|gif)", 500,
        "the cast and crew photos of a movie's detail page go through the cover proxy, which fetches "
        "them from image.tmdb.org when asked; here the metadata stand-in answers 503 and the proxy "
        "answers 500 with that error. Admitted only with the stand-in's 503 in the body.",
        body=r"HTTP request failed: \[503:Service Unavailable\]",
    ),
]

# The pages opened even if the menu does not show them, the same in both
# personalities: one of each kind the issue names. A page here that the UI
# no longer has fails as "not found", which says to update this list.
MINIMUM = [
    "/", "/calendar", "/activity/queue", "/activity/history", "/activity/blocklist",
    "/wanted/missing", "/wanted/cutoffunmet", "/settings", "/settings/mediamanagement",
    "/settings/profiles", "/settings/quality", "/settings/customformats", "/settings/indexers",
    "/settings/downloadclients", "/settings/importlists", "/settings/connect", "/settings/metadata",
    "/settings/tags", "/settings/general", "/settings/ui", "/system/status", "/system/tasks",
    "/system/backup", "/system/events", "/system/logs/files",
]

# The detail page of an item, linked from the library page.
DETAIL = {"sonarr": "/series/", "radarr": "/movie/"}

# The UI's CSS module classes are `<Component>-<name>-<hash>`: these match
# whatever the hash. `prefix` is what the UI's style sheets must still define.
SELECTORS = {
    "sidebar": '[class*="PageSidebar-sidebar-"]',
    "loading": '[class*="LoadingIndicator-loading-"], [class*="LoadingPage-page-"]',
    "error boundary": '[class*="ErrorBoundaryError-container-"]',
    "error page": '[class*="ErrorPage-page-"]',
    "not found": '[class*="NotFound-container-"]',
    "danger alert": '[class*="Alert-danger-"]',
}
PREFIXES = {
    "sidebar": ["PageSidebar-sidebar-"],
    "loading": ["LoadingIndicator-loading-", "LoadingPage-page-"],
    "error boundary": ["ErrorBoundaryError-container-"],
    "error page": ["ErrorPage-page-"],
    "not found": ["NotFound-container-"],
    "danger alert": ["Alert-danger-"],
}
ERROR_STATES = ("error boundary", "error page", "not found", "danger alert")

JS_VISIBLE = """
(sels) => {
  const visible = e => !!(e.offsetWidth || e.offsetHeight || e.getClientRects().length)
    && getComputedStyle(e).visibility !== 'hidden';
  const out = {};
  for (const [name, sel] of Object.entries(sels)) {
    out[name] = [...document.querySelectorAll(sel)].filter(visible)
      .map(e => (e.innerText || '').trim().replace(/\\s+/g, ' ').slice(0, 300));
  }
  return out;
}
"""

JS_NOT_LOADING = """
(sel) => ![...document.querySelectorAll(sel)].some(
  e => !!(e.offsetWidth || e.offsetHeight || e.getClientRects().length))
"""

JS_CLASSES = """
(prefixes) => {
  const found = new Set();
  for (const sheet of document.styleSheets) {
    let rules;
    try { rules = sheet.cssRules; } catch (e) { continue; }
    for (const rule of rules || []) {
      const text = rule.selectorText || '';
      for (const p of prefixes) if (text.includes('.' + p)) found.add(p);
    }
  }
  return [...found];
}
"""


def log(*a):
    print("[ui-smoke]", *a, file=sys.stderr, flush=True)


def cache_dir() -> Path:
    """UNARR_CACHE holds what .cache holds, as in the fixture scripts."""
    cache = os.environ.get("UNARR_CACHE")
    return Path(cache) if cache else REPO / ".cache"


# --- what a page did ------------------------------------------------------------


@dataclass
class Page:
    path: str
    source: str  # "menu", "library", "minimum" or "login"
    failures: list[dict] = field(default_factory=list)
    admitted: list[dict] = field(default_factory=list)
    blocked: list[str] = field(default_factory=list)
    requests: int = 0
    seconds: float = 0.0
    screenshot: str | None = None
    note: str | None = None

    @property
    def ok(self) -> bool:
        return not self.failures


def classify_response(app: str, method: str, url: str, status: int, body: str, data: Path,
                      allowed: list[Allowed]) -> Allowed | None:
    """The entry of ALLOWED that admits this answer, if one does."""
    path = urlsplit(url).path
    for entry in allowed:
        if entry.matches(app, method, path, status, body, data):
            return entry
    return None


def is_resource_error(text: str) -> bool:
    """Chromium's console line for a request that failed: the request itself
    is reported (or admitted) once, as a request, not again as a console error."""
    return text.startswith("Failed to load resource")


def merge_pages(discovered: list[str], minimum: list[str]) -> list[tuple[str, str]]:
    """The pages in the order found, then the fixed ones the menu did not show."""
    seen = set(discovered)
    return [(p, "menu") for p in discovered] + [(p, "minimum") for p in minimum if p not in seen]


def menu_path(href: str | None, origin: str) -> str | None:
    """A sidebar link as a page path, or None for a link out of the UI."""
    if not href:
        return None
    parts = urlsplit(href)
    if parts.scheme or parts.netloc:
        if f"{parts.scheme}://{parts.netloc}" != origin:
            return None
    path = parts.path or "/"
    if not path.startswith("/"):
        return None
    return path


# --- unarrd and the metadata stand-in ---------------------------------------------


class StandIn(http.server.ThreadingHTTPServer):
    """What unarrd asks its metadata hosts for, answered 503."""

    daemon_threads = True

    def __init__(self):
        self.asked: list[str] = []

        server = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def answer(self):
                server.asked.append(f"{self.command} {self.path}")
                body = b"ui-smoke: no metadata server in the smoke test\n"
                self.send_response(503)
                self.send_header("Content-Type", "text/plain")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            do_GET = do_POST = do_PUT = do_DELETE = do_HEAD = answer

            def log_message(self, *a):
                pass

        # Bound now, so its URL is known; served from start(), once unarrd
        # is forked (no thread may run while the child is set up).
        super().__init__(("127.0.0.1", 0), Handler)

    def start(self):
        self.serving = threading.Thread(target=self.serve_forever, daemon=True)
        self.serving.start()

    def stop(self):
        # shutdown() waits for serve_forever, which never ran if start() was not reached.
        if getattr(self, "serving", None) is not None:
            self.shutdown()
        self.server_close()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server_address[1]}"


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def stage(apps: list[str], data: Path) -> dict[str, int]:
    """A fresh copy of each fixture under `data`, each on a free port."""
    shutil.rmtree(data, ignore_errors=True)
    ports = {}
    for app in apps:
        target = data / app
        target.mkdir(parents=True)
        shutil.copy2(REPO / "tests/fixtures" / app / f"{app}.db", target)
        config = (REPO / "tests/fixtures" / app / "config.xml").read_text()
        ports[app] = free_port()
        config, n = re.subn(r"<Port>\d+</Port>", f"<Port>{ports[app]}</Port>", config)
        if n != 1:
            raise SystemExit(f"tests/fixtures/{app}/config.xml: no <Port> to move")
        (target / "config.xml").write_text(config)
    return ports


def death_signal():
    """What the child runs before exec, on Linux: SIGTERM when this script
    dies, even by SIGKILL. libc's prctl is looked up here, in the parent, so
    the child imports and loads nothing between fork and exec."""
    if sys.platform != "linux":
        return None
    try:
        import ctypes

        prctl = ctypes.CDLL(None, use_errno=True).prctl
    except (OSError, AttributeError):
        return None
    return lambda: prctl(1, signal.SIGTERM)  # PR_SET_PDEATHSIG


def start_unarrd(unarrd: Path, data: Path, apps: list[str], metadata: str, log_file: Path) -> subprocess.Popen:
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("SONARR__", "RADARR__", "LIDARR__", "UNARR_"))}
    env["UNARR_METADATA_UPSTREAM"] = metadata
    out = open(log_file, "wb")
    cmd = [str(unarrd), "--data", str(data), "--personalities", ",".join(apps), "--bind", "127.0.0.1"]
    return subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, env=env, cwd=data,
                            preexec_fn=death_signal())


def wait_until_up(proc: subprocess.Popen, ports: dict[str, int], log_file: Path, timeout: float = 180):
    deadline = time.monotonic() + timeout
    pending = dict(ports)
    while pending:
        if proc.poll() is not None:
            tail = log_file.read_text(errors="replace").splitlines()[-30:]
            raise SystemExit("unarrd exited with status {} before answering:\n{}".format(
                proc.returncode, "\n".join(tail)))
        for app, port in list(pending.items()):
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/ping", timeout=2) as r:
                    if r.status == 200:
                        del pending[app]
            except (urllib.error.URLError, OSError):
                pass
        if time.monotonic() > deadline:
            raise SystemExit(f"unarrd did not answer /ping within {timeout:.0f} s: {sorted(pending)} (see {log_file})")
        time.sleep(0.25)


def build() -> Path:
    """unarrd as the checkout builds it (debug), at the path cargo reports,
    wherever CARGO_TARGET_DIR or a cargo config put it."""
    log("cargo build --bin unarrd")
    done = subprocess.run(["cargo", "build", "--bin", "unarrd", "--message-format=json-render-diagnostics"],
                          cwd=REPO, stdout=subprocess.PIPE, text=True)
    if done.returncode != 0:
        raise SystemExit("cargo build failed")
    for line in done.stdout.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") == "compiler-artifact" and msg.get("target", {}).get("name") == "unarrd" \
                and msg.get("executable"):
            return Path(msg["executable"])
    raise SystemExit("cargo build named no unarrd executable")


def serves_ui(ports: dict[str, int]):
    """unarrd answers 503 for every page when it was built without the UIs
    (before make ui): the run cannot start, which is not a login failure."""
    for app, port in ports.items():
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=10).close()
        except urllib.error.HTTPError as e:
            if e.code == 503:
                body = e.read().decode(errors="replace").strip()
                raise SystemExit(f"unarrd serves no {app} web UI: {body}\n"
                                 "Build it after make ui (without --unarrd, the script builds it).")


def stop(proc: subprocess.Popen):
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


# --- the browser --------------------------------------------------------------------


class Run:
    """One personality in the browser."""

    def __init__(self, app: str, origin: str, page, args, data: Path, shots: Path):
        self.app, self.origin, self.page, self.args, self.data, self.shots = app, origin, page, args, data, shots
        self.current = Page("(start)", "login")
        self.pages: list[Page] = []
        self.found_classes: set[str] = set()
        self.used: set[Allowed] = set()
        self.pending: list[tuple[Page, object]] = []
        self.sockets: list[str] = []
        self.problems: list[dict] = []
        # Requests to unarrd sent and not yet answered (or failed).
        self.inflight: dict[int, object] = {}
        page.on("console", self.on_console)
        page.on("pageerror", self.on_pageerror)
        page.on("response", self.on_response)
        page.on("requestfailed", self.on_requestfailed)
        page.on("request", self.on_request)
        page.on("requestfinished", self.on_requestfinished)
        page.on("websocket", self.on_websocket)

    # Events land on the page being opened.

    def mine(self, url: str) -> bool:
        return url.startswith(self.origin + "/") or url == self.origin

    def on_request(self, request):
        if self.mine(request.url):
            self.current.requests += 1
            self.inflight[id(request)] = request

    def on_requestfinished(self, request):
        self.inflight.pop(id(request), None)

    def on_console(self, msg):
        if msg.type != "error":
            return
        text = msg.text
        url = (msg.location or {}).get("url", "")
        if is_resource_error(text):
            # Reported as the request it is (on_response, on_requestfailed, blocked).
            return
        self.current.failures.append({"kind": "console", "text": text[:2000], "source": url})

    def on_pageerror(self, error):
        stack = getattr(error, "stack", None) or ""
        self.current.failures.append({"kind": "exception", "text": str(error)[:2000], "stack": stack[:4000]})

    def on_response(self, response):
        # Judged once the page has settled (classify): an exception may need the body.
        if self.mine(response.url) and response.status >= 400:
            self.pending.append((self.current, response))

    def on_requestfailed(self, request):
        self.inflight.pop(id(request), None)
        if not self.mine(request.url):
            return
        failure = request.failure or ""
        # A request still running when the page is left is cancelled by the
        # browser; the pages are left only once settled.
        if "ERR_ABORTED" in failure:
            return
        self.current.failures.append({"kind": "request", "method": request.method,
                                      "url": request.url[len(self.origin):], "error": failure})

    def on_websocket(self, ws):
        page = self.current
        if self.mine(ws.url.replace("ws://", "http://", 1)):
            # The path only: the query holds the API key.
            self.sockets.append(urlsplit(ws.url).path)

        def error(message):
            page.failures.append({"kind": "websocket", "url": ws.url, "error": str(message)})

        ws.on("socketerror", error)

    def route(self, route):
        url = route.request.url
        if self.mine(url) or url.startswith(("data:", "blob:")):
            route.continue_()
        else:
            self.current.blocked.append(f"{route.request.method} {url}")
            route.abort("blockedbyclient")

    def classify(self):
        """Each failing answer of the pages settled so far: admitted by ALLOWED,
        or a failure with the start of its body. Read before the page is left."""
        for record, response in self.pending:
            method, url = response.request.method, response.url
            try:
                body = response.text()
            except Exception as e:
                body = f"(not readable: {str(e).splitlines()[0]})"
            entry = classify_response(self.app, method, url, response.status, body, self.data, ALLOWED)
            f = {"kind": "response", "method": method, "url": url[len(self.origin):],
                 "status": response.status, "status_text": response.status_text}
            if entry:
                self.used.add(entry)
                f["allowed"] = entry.path
                record.admitted.append(f)
            else:
                f["body"] = body[:1000]
                record.failures.append(f)
        self.pending.clear()

    # Pages.

    def login(self) -> bool:
        """Log in when the UI asks. The record holds the login and the
        library page it leads to."""
        self.current = record = Page("/login", "login")
        self.pages.append(record)
        start = time.monotonic()
        page = self.page
        try:
            page.goto(self.origin + "/", wait_until="load", timeout=self.args.settle * 1000)
            if urlsplit(page.url).path.rstrip("/").endswith("/login"):
                page.fill("input[name=username]", LOGIN[0])
                page.fill("input[name=password]", LOGIN[1])
                page.click("button[type=submit]")
                page.wait_for_url(lambda u: not urlsplit(u).path.rstrip("/").endswith("/login"),
                                  timeout=self.args.settle * 1000)
            else:
                record.note = "no login form: the UI opened directly"
        except Exception as e:
            record.failures.append({"kind": "login", "text": f"cannot log in as {LOGIN[0]} at {page.url}: "
                                    + str(e).strip().splitlines()[0]})
            self.screenshot(record)
            return False
        finally:
            record.seconds = time.monotonic() - start
        self.settle_network()
        self.classify()
        record.seconds = time.monotonic() - start
        return True

    def settle_network(self) -> bool:
        """Wait until no request has been running for half a second. A
        request to unarrd still unanswered after --settle seconds fails the
        page: leaving it would cancel the request, and a cancel is not
        reported. False when the wait ran out."""
        try:
            self.page.wait_for_load_state("networkidle", timeout=self.args.settle * 1000)
            return True
        except Exception:
            pass
        hanging = list(self.inflight.values())
        for request in hanging:
            self.current.failures.append({"kind": "request", "method": request.method,
                                          "url": request.url[len(self.origin):],
                                          "error": f"no answer after {self.args.settle:.0f} s"})
        if not hanging:
            self.current.note = f"requests out of unarrd still running after {self.args.settle:.0f} s"
        self.inflight.clear()
        return False

    def look(self, path: str):
        """Open a page only to read its menu (a page --only leaves out): what
        it does is not judged, and it is not in the report."""
        self.inflight.clear()
        self.current = Page(path, "menu")
        try:
            self.page.goto(self.origin + path, wait_until="load", timeout=self.args.settle * 1000)
            self.settle_network()
            self.page.wait_for_function(JS_NOT_LOADING, arg=SELECTORS["loading"], timeout=self.args.settle * 1000)
            self.found_classes.update(self.page.evaluate(JS_CLASSES, [p for ps in PREFIXES.values() for p in ps]))
        except Exception:
            pass
        self.pending.clear()

    def visit(self, path: str, source: str) -> Page:
        self.inflight.clear()
        self.current = record = Page(path, source)
        self.pages.append(record)
        start = time.monotonic()
        page = self.page
        try:
            page.goto(self.origin + path, wait_until="load", timeout=self.args.settle * 1000)
        except Exception as e:
            record.failures.append({"kind": "navigation", "text": str(e).strip().splitlines()[0]})
        else:
            settled = self.settle_network()
            try:
                page.wait_for_function(JS_NOT_LOADING, arg=SELECTORS["loading"], timeout=self.args.settle * 1000)
            except Exception:
                record.failures.append({"kind": "loading",
                                        "text": f"the loading indicator is still shown after {self.args.settle:.0f} s"})
            # What the data, once loaded, asks for in turn (covers, a second
            # round); not waited for again when the first wait ran out.
            if settled:
                self.settle_network()
            states = page.evaluate(JS_VISIBLE, {k: SELECTORS[k] for k in ERROR_STATES})
            for name in ERROR_STATES:
                for text in states.get(name, []):
                    record.failures.append({"kind": "state", "state": name, "text": text})
            self.found_classes.update(page.evaluate(JS_CLASSES, [p for ps in PREFIXES.values() for p in ps]))
        self.classify()
        record.seconds = time.monotonic() - start
        if record.failures:
            self.screenshot(record)
        log(f"{self.app} {path}: {'ok' if record.ok else 'FAILED'} ({record.seconds:.1f} s)")
        return record

    def screenshot(self, record: Page):
        name = f"{self.app}{re.sub(r'[^A-Za-z0-9]+', '-', record.path).rstrip('-') or '-library'}.png"
        try:
            self.shots.mkdir(parents=True, exist_ok=True)
            self.page.screenshot(path=str(self.shots / name))
            record.screenshot = str(self.shots / name)
        except Exception:
            pass

    def links(self, selector: str) -> list[str]:
        try:
            return self.page.eval_on_selector_all(selector, "els => els.map(e => e.getAttribute('href'))")
        except Exception:
            return []

    def menu(self) -> list[str]:
        out = []
        for href in self.links(SELECTORS["sidebar"] + " a[href]"):
            path = menu_path(href, self.origin)
            if path and path not in out:
                out.append(path)
        return out

    def crawl(self):
        """Every page of the menu, breadth first, with the library's first
        item; then the fixed pages the menu did not show, then --page. With
        --only the whole menu is still walked, to find the pages; only those
        that match are judged."""
        wanted = (lambda path: re.search(self.args.only, path)) if self.args.only else (lambda path: True)
        queue, seen, found = ["/"], {"/"}, []
        while queue:
            path = queue.pop(0)
            source = "library" if path.startswith(DETAIL[self.app]) else "menu"
            if wanted(path):
                self.visit(path, source)
            elif source == "menu":
                self.look(path)
            else:
                continue
            found.append(path)
            for link in self.menu():
                if link not in seen:
                    seen.add(link)
                    queue.append(link)
            if path == "/":
                items = [h for h in self.links(f'a[href^="{DETAIL[self.app]}"]') if menu_path(h, self.origin)]
                if items:
                    seen.add(items[0])
                    queue.insert(0, items[0])
                else:
                    self.problems.append({"kind": "library",
                                          "text": f"the library page links to no {DETAIL[self.app]}... detail page"})
        if len(found) <= 2:
            self.problems.append({"kind": "menu", "text": "the sidebar shows no page: "
                                  "SELECTORS['sidebar'] in scripts/ui-smoke.py no longer matches the UI"})
        for path, source in merge_pages(found, MINIMUM):
            if source == "minimum" and wanted(path):
                record = self.visit(path, source)
                record.note = "not in the menu: opened from MINIMUM in scripts/ui-smoke.py"
        for path in self.args.page or []:
            self.visit(path, "--page")


def run_app(browser, app: str, port: int, args, out: Path) -> dict:
    origin = f"http://127.0.0.1:{port}"
    context = browser.new_context(viewport={"width": 1440, "height": 900})
    page = context.new_page()
    run = Run(app, origin, page, args, out / "data", out / "screenshots")
    context.route("**/*", run.route)
    start = time.monotonic()
    problems = []
    try:
        if run.login():
            run.crawl()
    except Exception as e:
        # A browser or driver error ends this personality's run, not the report.
        problems.append({"kind": "run", "text": f"stopped at {run.current.path}: {type(e).__name__}: "
                         + (str(e).strip().splitlines() or [""])[0]})
    finally:
        context.close()
    problems += run.problems
    logged_in = all(p.ok for p in run.pages if p.source == "login")
    missing = sorted({name for name, prefixes in PREFIXES.items() for p in prefixes if p not in run.found_classes})
    if missing and logged_in:
        problems.append({"kind": "selectors", "text": "the UI's style sheets no longer define "
                         + ", ".join(f"{n} ({' / '.join(PREFIXES[n])})" for n in missing)
                         + ": the test cannot see that state; update SELECTORS and PREFIXES in scripts/ui-smoke.py"})
    # The UI's live updates (SignalR): a socket that never opens falls back
    # to long polling, or to nothing, without an error on the page.
    if not run.sockets and logged_in:
        problems.append({"kind": "signalr", "text": "the UI opened no websocket to unarrd: its live updates do not work"})
    version = REPO / "crates/unarr-api/ui" / app / "VERSION"
    return {
        "app": app,
        "ui_version": version.read_text().strip() if version.exists() else None,
        "origin": origin,
        "seconds": round(time.monotonic() - start, 1),
        "problems": problems,
        "websockets": sorted(set(run.sockets)),
        "pages": [page_dict(p) for p in run.pages],
        "allowed_used": [allowed_dict(a) for a in ALLOWED if app in a.apps and a in run.used],
        "allowed_unused": [allowed_dict(a) for a in ALLOWED if app in a.apps and a not in run.used and not args.only and logged_in],
    }


def page_dict(p: Page) -> dict:
    d = {"path": p.path, "source": p.source, "ok": p.ok, "seconds": round(p.seconds, 2), "requests": p.requests,
         "failures": p.failures, "admitted": p.admitted, "blocked": p.blocked}
    if p.screenshot:
        d["screenshot"] = p.screenshot
    if p.note:
        d["note"] = p.note
    return d


def allowed_dict(a: Allowed) -> dict:
    return {"method": a.method, "path": a.path, "status": a.status, "reason": a.reason}


# --- the report ------------------------------------------------------------------------


def describe(f: dict) -> str:
    kind = f["kind"]
    if kind == "response":
        body = (f.get("body") or "").strip().replace("\n", " ")
        return f"{f['method']} {f['url']} -> {f['status']} {f.get('status_text', '')}".rstrip() + (f": {body[:200]}" if body else "")
    if kind == "request":
        return f"{f['method']} {f['url']} not answered: {f['error']}"
    if kind == "state":
        return f"shows the UI's {f['state']}: {f['text'][:200]}"
    if kind == "websocket":
        return f"websocket {f['url']}: {f['error']}"
    return f"{f.get('text', '')[:300]}"


def failed(report: dict) -> bool:
    return any(r["problems"] or any(not p["ok"] for p in r["pages"]) for r in report["apps"])


def summary(report: dict, out: Path) -> str:
    lines = []
    for r in report["apps"]:
        pages = [p for p in r["pages"] if p["source"] != "login"]
        bad = [p for p in pages if not p["ok"]]
        lines.append(f"{r['app']} {r['ui_version']}: {len(pages)} pages, {len(pages) - len(bad)} ok, "
                     f"{len(bad)} failed, {r['seconds']:.0f} s")
        # The login first: when it fails, no page is opened.
        for p in [p for p in r["pages"] if p["source"] == "login" and not p["ok"]] + bad:
            label = "login" if p["source"] == "login" else p["path"]
            lines.append(f"  FAIL {label}" + (" (not in the menu)" if p["source"] == "minimum" else ""))
            for f in p["failures"]:
                lines.append(f"    {f['kind']:<9} {describe(f)}")
        for prob in r["problems"]:
            lines.append(f"  FAIL {prob['kind']}: {prob['text']}")
        for a in r["allowed_unused"]:
            lines.append(f"  note: exception not needed by any page: {a['method']} {a['path']} {a['status']}")
        blocked = sorted({b for p in r["pages"] for b in p["blocked"]})
        if blocked:
            lines.append(f"  note: {len(blocked)} request(s) out of unarrd blocked, e.g. {blocked[0]}")
    asked = report["metadata_asked"]
    if asked:
        lines.append(f"metadata stand-in asked {len(asked)} time(s), e.g. {asked[0]}")
    lines.append(f"{'FAILED' if failed(report) else 'ok'}; details in {out / 'report.md'} and {out / 'report.json'}")
    return "\n".join(lines)


def markdown(report: dict) -> str:
    md = [f"# UI smoke test, {report['date']}", "",
          f"unarrd: `{report['unarrd']}`; browser: {report['browser']}; {report['seconds']:.0f} s in all.", ""]
    for r in report["apps"]:
        md += [f"## {r['app']} {r['ui_version']}", "", "| page | found in | result | requests | s |", "|---|---|---|--:|--:|"]
        for p in r["pages"]:
            md.append(f"| `{p['path']}` | {p['source']} | {'ok' if p['ok'] else '**failed**'} | {p['requests']} | {p['seconds']:.1f} |")
        md.append("")
        for p in r["pages"]:
            if p["ok"] and not p["admitted"] and not p.get("note"):
                continue
            md.append(f"### `{p['path']}`")
            if p.get("note"):
                md.append(f"_{p['note']}_")
            for f in p["failures"]:
                md.append(f"- **{f['kind']}**: {describe(f)}")
                if f.get("stack"):
                    md += ["", "  ```", *("  " + line for line in f["stack"].splitlines()[:15]), "  ```"]
            groups: dict[tuple, list[dict]] = {}
            for a in p["admitted"]:
                groups.setdefault((a["method"], a["allowed"], a["status"]), []).append(a)
            for (method, pattern, status), answers in groups.items():
                md.append(f"- admitted by {method} `{pattern}` {status}: {len(answers)} answer(s), "
                          f"e.g. `{answers[0]['url']}`")
            if p.get("screenshot"):
                md.append(f"- screenshot: `{p['screenshot']}`")
            md.append("")
        for prob in r["problems"]:
            md += [f"**{prob['kind']}**: {prob['text']}", ""]
        if r["allowed_used"] or r["allowed_unused"]:
            md += ["Exceptions (`ALLOWED`):", ""]
            md += [f"- used: {a['method']} `{a['path']}` {a['status']}: {a['reason']}" for a in r["allowed_used"]]
            md += [f"- **unused**: {a['method']} `{a['path']}` {a['status']}: {a['reason']}" for a in r["allowed_unused"]]
            md.append("")
        blocked = sorted({b for p in r["pages"] for b in p["blocked"]})
        if blocked:
            md += ["Requests out of unarrd, blocked:", "", *(f"- `{b}`" for b in blocked), ""]
    if report["metadata_asked"]:
        md += ["## Metadata stand-in", "", "What unarrd asked its metadata hosts for (answered 503):", "",
               *(f"- `{a}`" for a in report["metadata_asked"]), ""]
    return "\n".join(md)


# --- main ------------------------------------------------------------------------------------


def claim(out: Path):
    """--out is this script's own folder: new, empty or marked by an earlier run."""
    if out.exists() and not out.is_dir():
        raise SystemExit(f"--out {out} is not a folder")
    if out.is_dir() and any(out.iterdir()) and not (out / MARKER).exists():
        raise SystemExit(f"--out {out} holds files this script did not write; give a new or empty folder")
    out.mkdir(parents=True, exist_ok=True)
    (out / MARKER).write_text("Results of scripts/ui-smoke.py; the next run rewrites this folder.\n")
    for name in ("data", "screenshots"):
        shutil.rmtree(out / name, ignore_errors=True)
    for name in ("report.json", "report.md", "unarrd.log"):
        (out / name).unlink(missing_ok=True)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--app", default="sonarr,radarr", help="personalities, comma separated (default both)")
    ap.add_argument("--unarrd", type=Path,
                    help="a prebuilt unarrd (default: cargo build --bin unarrd, the binary cargo reports)")
    ap.add_argument("--out", type=Path, help="results folder (default <UNARR_CACHE or .cache>/ui-smoke)")
    ap.add_argument("--channel", default=os.environ.get("UI_SMOKE_CHANNEL") or None,
                    help="a browser installed on the machine, e.g. chrome (default: Playwright's Chromium; "
                         "env UI_SMOKE_CHANNEL)")
    ap.add_argument("--settle", type=float, default=20,
                    help="seconds a page may take to load and stop showing its loading indicator (default 20)")
    ap.add_argument("--only", help="judge only the pages whose path matches this regular expression (to debug one); "
                         "the menu is still walked to find them")
    ap.add_argument("--page", action="append", help="open this page too, after the others (repeatable)")
    args = ap.parse_args(argv)

    apps = [a for a in args.app.split(",") if a]
    unknown = [a for a in apps if a not in APPS]
    if unknown or not apps:
        raise SystemExit(f"--app: one or more of {', '.join(APPS)}")
    for app in apps:
        if not (REPO / "crates/unarr-api/ui" / app / "index.html").exists():
            raise SystemExit(f"the {app} web UI is not embedded: run 'make ui' first")
    if args.unarrd is not None and not args.unarrd.exists():
        raise SystemExit(f"--unarrd {args.unarrd} does not exist")
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        raise SystemExit("Python's playwright is missing: run 'make ui-smoke', which installs it in a virtual "
                         "environment, or pip install -r scripts/ui-smoke-requirements.txt")

    out = (args.out or cache_dir() / "ui-smoke").resolve()
    claim(out)
    unarrd = args.unarrd or build()
    start = time.monotonic()
    standin = StandIn()
    ports = stage(apps, out / "data")
    log_file = out / "unarrd.log"
    proc = start_unarrd(unarrd.resolve(), out / "data", apps, standin.url, log_file)
    standin.start()
    # A plain kill (SIGTERM) ends Python without its finally blocks, and on
    # macOS there is no death signal: unarrd would be left running. Stop it
    # here and leave at once; unwinding instead would wait on Playwright's
    # half-done call forever. Playwright's driver and its browser end when
    # this process does.
    def on_term(*_):
        stop(proc)
        os._exit(128 + signal.SIGTERM)

    signal.signal(signal.SIGTERM, on_term)
    results = []
    try:
        wait_until_up(proc, ports, log_file)
        serves_ui(ports)
        log(f"unarrd up: " + ", ".join(f"{a} on {p}" for a, p in ports.items()))
        with sync_playwright() as pw:
            try:
                browser = pw.chromium.launch(channel=args.channel)
            except Exception as e:
                first = str(e).strip().splitlines()[0]
                raise SystemExit(f"cannot start the browser ({first}).\nInstall Playwright's Chromium once with "
                                 f"'{sys.executable} -m playwright install chromium', or use an installed Google "
                                 "Chrome: UI_SMOKE_CHANNEL=chrome")
            browser_name = f"{args.channel or 'chromium'} {browser.version}"
            try:
                for app in apps:
                    results.append(run_app(browser, app, ports[app], args, out))
            finally:
                browser.close()
    finally:
        stop(proc)
        standin.stop()

    report = {
        "date": time.strftime("%Y-%m-%d %H:%M:%S"),
        "unarrd": str(unarrd),
        "browser": browser_name,
        "seconds": round(time.monotonic() - start, 1),
        "apps": results,
        "metadata_asked": standin.asked,
        "unarrd_log": str(log_file),
    }
    (out / "report.json").write_text(json.dumps(report, indent=1) + "\n")
    (out / "report.md").write_text(markdown(report) + "\n")
    print(summary(report, out))
    return 1 if failed(report) else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit as e:
        # A run that could not start (no UI, no unarrd, no browser) is 2, not a failed page.
        if isinstance(e.code, str):
            print(e.code, file=sys.stderr)
            sys.exit(2)
        raise
