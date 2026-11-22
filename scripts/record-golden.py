#!/usr/bin/env python3
"""Replay a list of API requests and store each response as a golden file.

Request list format, one per line (blank lines and ``#`` comments ignored)::

    <name> <METHOD> <path-with-query> [body-file]

The optional fourth column names a JSON file, relative to the request list,
sent as the request body with ``Content-Type: application/json``. A body
file holding ``{"$from": "<name>", "$set": {...}}`` is derived instead: the
body the response recorded as ``<name>`` carried, its top-level keys
replaced by those of ``$set`` (what a client does that reads a resource,
changes it and sends it back whole, the UI's series editor for one). A path may
carry ``{id:name}`` (a body carries it quoted, ``"{id:name}"``, and gets the
number): the ``id`` the response recorded as ``name`` returned,
for resources the application numbers itself (commands). ``sleep N`` lines
pause the recording for the application's background work.

``mock <name> <METHOD> <path> [body-file]`` sends the request to the mock
services (``--mock-url``, the ``scripts/mock-servarr.py`` the application is
pointed at) instead of the application: to complete a download, or to read
back what the mock's notification targets received. The response is recorded
as ``<name>.json`` like any other, unless ``<name>`` is ``-``.

Each golden file is ``<name>.json`` holding ``{"request": {...}, "status": N,
"body": <json or null>}``. Pretty-printed with sorted keys so diffs stay readable.

``--signalr`` also listens to the application's SignalR hub for the whole
recording, as the UI does, and writes ``signalr.json`` next to the goldens:
the messages the hub sent, grouped under the name of the request (or of the
``sleep`` or ``mock`` line, by its position) they followed. It is what the UI
was told between two answers, which no answer shows.

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
import base64
import datetime
import http.cookiejar
import json
import os
import re
import socket
import struct
import sys
import threading
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


REMEMBER_ME = re.compile(r"<input\b[^>]*\bname=\"rememberMe\"[^>]*>", re.S)


def checkbox_value(page: str) -> str:
    """What a browser sends for the login page's checked `rememberMe` box:
    its `value`, or `on` when it has none. Sonarr 4 and Radarr 6 have none;
    Radarr 7 says `true` and refuses `on` (its form field became a bool)."""
    tag = REMEMBER_ME.search(page)
    value = re.search(r"\bvalue=\"([^\"]*)\"", tag.group(0)) if tag else None
    return value.group(1) if value else "on"


def remember_me_value(base: str) -> str:
    try:
        with urllib.request.urlopen(base + "/login", timeout=60) as resp:
            return checkbox_value(resp.read().decode(errors="replace"))
    except (urllib.error.URLError, OSError):
        return "on"


def derive_body(raw: bytes, out: Path) -> bytes:
    """A ``$from``/``$set`` body file resolved against the goldens recorded so far."""
    try:
        spec = json.loads(raw)
    except json.JSONDecodeError:
        return raw
    if not isinstance(spec, dict) or "$from" not in spec:
        return raw
    source = json.loads((out / f"{spec['$from']}.json").read_text())["body"]
    if isinstance(source, list):
        source = source[0]
    source.update(spec.get("$set", {}))
    return json.dumps(source).encode()


class SignalrTap(threading.Thread):
    """A SignalR client over a WebSocket, JSON protocol, standard library only.

    It connects straight to the hub (no negotiation, which the WebSocket
    transport does not need), completes the protocol handshake and keeps
    what the hub invokes, `receiveMessage` with one `{name, body}` argument,
    under the step the recording is at.
    """

    SEPARATOR = "\x1e"

    def __init__(self, base_url: str, api_key: str):
        super().__init__(daemon=True)
        url = urllib.parse.urlsplit(base_url)
        self.host, self.port = url.hostname, url.port or 80
        self.path = f"{url.path.rstrip('/')}/signalr/messages?access_token={urllib.parse.quote(api_key)}"
        self.lock = threading.Lock()
        self.step = "(start)"
        self.steps: list[dict] = [{"after": self.step, "messages": []}]
        self.sock: socket.socket | None = None
        self.buffer = b""
        self.closed = threading.Event()

    def connect(self) -> None:
        sock = socket.create_connection((self.host, self.port), timeout=30)
        key = base64.b64encode(os.urandom(16)).decode()
        sock.sendall(
            (
                f"GET {self.path} HTTP/1.1\r\nHost: {self.host}:{self.port}\r\n"
                "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = sock.recv(4096)
            if not chunk:
                sys.exit("[golden] signalr: the hub closed the connection during the upgrade")
            head += chunk
        head, _, self.buffer = head.partition(b"\r\n\r\n")
        status = head.split(b"\r\n", 1)[0].decode(errors="replace")
        if " 101 " not in status:
            sys.exit(f"[golden] signalr: no WebSocket upgrade: {status}")
        sock.settimeout(None)
        self.sock = sock
        self.send_text(json.dumps({"protocol": "json", "version": 1}) + self.SEPARATOR)
        self.start()
        print(f"[golden] signalr: listening on {self.host}:{self.port}", file=sys.stderr)

    def send_frame(self, opcode: int, payload: bytes) -> None:
        # Client frames are masked (RFC 6455, 5.3).
        mask = os.urandom(4)
        head = bytes([0x80 | opcode])
        if len(payload) < 126:
            head += bytes([0x80 | len(payload)])
        elif len(payload) < 65536:
            head += bytes([0x80 | 126]) + struct.pack(">H", len(payload))
        else:
            head += bytes([0x80 | 127]) + struct.pack(">Q", len(payload))
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        assert self.sock is not None
        self.sock.sendall(head + mask + masked)

    def send_text(self, text: str) -> None:
        self.send_frame(0x1, text.encode())

    def read(self, n: int) -> bytes:
        assert self.sock is not None
        while len(self.buffer) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("closed")
            self.buffer += chunk
        data, self.buffer = self.buffer[:n], self.buffer[n:]
        return data

    def read_message(self) -> tuple[int, bytes]:
        """One message, its fragments joined; control frames answered."""
        opcode, payload = 0, b""
        while True:
            first, second = self.read(2)
            length = second & 0x7F
            if length == 126:
                (length,) = struct.unpack(">H", self.read(2))
            elif length == 127:
                (length,) = struct.unpack(">Q", self.read(8))
            mask = self.read(4) if second & 0x80 else b""
            data = self.read(length)
            if mask:
                data = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
            code = first & 0x0F
            if code == 0x9:  # ping
                self.send_frame(0xA, data)
                continue
            if code == 0xA:  # pong
                continue
            if code == 0x8:
                raise ConnectionError("close frame")
            if code:
                opcode = code
            payload += data
            if first & 0x80:
                return opcode, payload

    def run(self) -> None:
        pending = ""
        try:
            while True:
                opcode, payload = self.read_message()
                if opcode != 0x1:
                    continue
                pending += payload.decode()
                *records, pending = pending.split(self.SEPARATOR)
                for record in records:
                    if record.strip():
                        self.handle(json.loads(record))
        except (ConnectionError, OSError):
            pass
        finally:
            self.closed.set()

    def handle(self, record: dict) -> None:
        kind = record.get("type")
        if kind == 6:  # the hub's keep-alive: answer in kind
            self.send_text(json.dumps({"type": 6}) + self.SEPARATOR)
        elif kind == 1 and record.get("target") == "receiveMessage":
            with self.lock:
                self.steps[-1]["messages"].extend(record.get("arguments") or [])
        elif kind == 7:
            raise ConnectionError(record.get("error") or "closed by the hub")

    def at(self, step: str) -> None:
        """What arrives from now on followed `step`."""
        with self.lock:
            self.step = step
            self.steps.append({"after": step, "messages": []})

    def finish(self, out: Path, scrub=lambda text: text) -> None:
        # Let the last messages arrive, then stop listening.
        time.sleep(2)
        with self.lock:
            steps = [s for s in self.steps if s["messages"]]
        if self.closed.is_set():
            print("[golden] signalr: the connection closed before the end of the recording", file=sys.stderr)
        (out / "signalr.json").write_text(
            scrub(json.dumps(steps, indent=2, sort_keys=True, ensure_ascii=False)) + "\n"
        )
        total = sum(len(s["messages"]) for s in steps)
        print(f"[golden] signalr: {total} messages after {len(steps)} steps -> signalr.json", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--api-key", required=True)
    ap.add_argument("--requests", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--login", help="user:password for the forms login used by session-* requests")
    ap.add_argument("--mock-url", default="http://127.0.0.1:18989", help="where `mock` lines are sent")
    ap.add_argument("--signalr", action="store_true", help="listen to the SignalR hub and write signalr.json")
    ap.add_argument(
        "--data-dir",
        help="the application's data directory: where it appears in an answer or in an exception "
        "(a stack that names a folder), it is written down as `/config`, so a recording carries "
        "no path of the machine that made it",
    )
    args = ap.parse_args()

    # The data directory of the machine that records, written down as the
    # path a container would have: a recording is read by everyone and the
    # answers of the originals quote it in stacks and in error messages.
    data_dir = str(Path(args.data_dir).resolve()) if args.data_dir else None

    def scrub(text: str) -> str:
        if not data_dir:
            return text
        return text.replace(json.dumps(data_dir)[1:-1], "/config").replace(data_dir, "/config")


    args.out.mkdir(parents=True, exist_ok=True)
    base = args.base_url.rstrip("/")

    # Two clients: one bare (API key per request), one with a cookie jar that
    # has been through the login form.
    bare = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    session = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    if args.login:
        user, _, password = args.login.partition(":")
        form = urllib.parse.urlencode({"username": user, "password": password,
                                       "rememberMe": remember_me_value(base)}).encode()
        with session.open(urllib.request.Request(base + "/login", data=form, method="POST"), timeout=60) as resp:
            print(f"[golden] login as {user!r}: {resp.status} {resp.geturl()}", file=sys.stderr)
    tap = None
    if args.signalr:
        tap = SignalrTap(base, args.api_key)
        tap.connect()
    count = 0
    # A step of the list by its position, for the lines that have no name.
    position = 0
    # Ids the responses carried, for `{id:name}` in later paths.
    ids: dict[str, str] = {}
    for line in args.requests.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        name, method, path, *rest = line.split(maxsplit=3) if not line.startswith("sleep ") else (line.split()[0], line.split()[1], "", [])
        position += 1
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
            data = derive_body((args.requests.parent / rest[0]).read_bytes(), args.out)
            # `"{id:name}"` in a body is that id, as a number.
            data = re.sub(rb'"\{id:([^}]+)\}"', lambda m: ids[m.group(1).decode()].encode(), data)
        path = substitute_ids(path, ids)
        if tap:
            # The hub's messages from here on follow this request; a line
            # without a name of its own is known by its place in the list.
            tap.at(name if name != "-" else f"#{position}")
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
        elif isinstance(body, list) and body and isinstance(body[0], dict) and isinstance(body[0].get("id"), int):
            # A list names its first item: the backup a listing just showed.
            ids[name] = str(body[0]["id"])
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
        (args.out / f"{name}.json").write_text(scrub(json.dumps(golden, indent=2, sort_keys=True, ensure_ascii=False)) + "\n")
        print(f"[golden] {status} {method} {path} -> {name}.json", file=sys.stderr)
        count += 1
    if tap:
        tap.finish(args.out, scrub)
    print(f"[golden] {count} responses recorded in {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
