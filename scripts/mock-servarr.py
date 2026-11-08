#!/usr/bin/env python3
"""Stand-ins for the services Unarr talks to, answering from the fixture tree.

One process, one port, several services told apart by path prefix:

    /newznab/api      a Newznab indexer   (tests/fixtures/indexers/newznab/)
    /torznab/api      a Torznab indexer   (tests/fixtures/indexers/torznab/)
    /newznab/download/<guid>.nzb, /torznab/download/<guid>.torrent
    /sabnzbd/api          SABnzbd 4.4       (api key below, or ma_username=mock&ma_password=mock)
    /nzbget/jsonrpc       NZBGet 24         (basic auth mock:mock)
    /qbittorrent/api/v2/  qBittorrent 5.1   (Bearer api key, or the login form mock/mock)
    /transmission/rpc     Transmission 4.0  (basic auth mock:mock, X-Transmission-Session-Id)

`make golden` starts it and registers these services in the real Sonarr
running on the fixture copy, so the goldens for searches, grabs and the
queue come from the oracle and the exact requests the original sends are
captured in the log (`--log`, one JSON object per line). The Rust crate
`unarr-mock` serves the same files with the same rules for the tests.

Routing, by the `t` query parameter (missing files answer `empty.xml`):

    t=caps                                caps.xml
    t=tvsearch&tvdbid=N&season=S&ep=E     tvsearch-tvdbid{N}-S{S}E{E}.xml
    t=tvsearch&tvdbid=N&season=S          tvsearch-tvdbid{N}-S{S}.xml
    t=tvsearch&tvdbid=N                   tvsearch-tvdbid{N}.xml
    (rid= and tvmazeid= likewise, with the `rid` and `tvmazeid` prefixes)
    t=tvsearch|search&q=TEXT[&season&ep]  search-{slug(TEXT)}[-S{S}[E{E}]].xml
    t=tvsearch with no q and no id        rss.xml
    offset=N>0                            the same name with `-offset{N}`, else empty.xml

`slug` lower-cases, replaces every run of non-alphanumerics with `-` and
trims. An `apikey` other than 0123456789abcdef0123456789abcdef gets the
Newznab `100` error. Downloads come from `nzb/` and `torrent/` next to the
XML, or are synthesised when the file is missing.

The download clients keep their queues in memory, as the real ones answer
their proxies: adding returns an id, listing shows the item downloading with
nothing done, removing drops it. Tests move items along through control
endpoints the real clients do not have:

    POST /mock/reset                                   forget every item
    POST /mock/<client>/complete?id=<download id>      the item finished and is in history / seeding
    POST /mock/<client>/fail?id=<download id>          the item failed (usenet: in history as failed)

`<client>` is sabnzbd, nzbget, qbittorrent or transmission. The ids are the
ones the client reported when adding (SABnzbd nzo id, NZBGet drone id,
torrent info hash).

    scripts/mock-servarr.py --listen 127.0.0.1:18989 --fixtures tests/fixtures --log .cache/mock-requests.jsonl
"""

from __future__ import annotations

import argparse
import base64
import threading
import uuid
import datetime
import hashlib
import http.server
import json
import re
import socketserver
import sys
import urllib.parse
from pathlib import Path

API_KEY = "0123456789abcdef0123456789abcdef"

ERROR_100 = '<?xml version="1.0" encoding="UTF-8"?>\n<error code="100" description="Incorrect user credentials"/>\n'
EMPTY_RSS = (
    '<?xml version="1.0" encoding="UTF-8"?>\n'
    '<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" '
    'xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">\n'
    "<channel><title>Fixture</title><newznab:response offset=\"0\" total=\"0\"/></channel></rss>\n"
)


def log(msg: str) -> None:
    print(f"[mock] {msg}", file=sys.stderr, flush=True)


def slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def result_file(q: dict[str, str]) -> str:
    """The fixture file name a Newznab/Torznab query maps to (rules above)."""
    t = q.get("t", "")
    if t == "caps":
        return "caps.xml"
    season, ep = q.get("season"), q.get("ep")
    suffix = ""
    if season:
        suffix += f"-S{season}"
        if ep:
            suffix += f"E{ep}"
    name = None
    for key in ("tvdbid", "rid", "tvmazeid", "imdbid"):
        if q.get(key):
            name = f"tvsearch-{key}{q[key]}{suffix}"
            break
    if name is None and q.get("q"):
        name = f"search-{slug(q['q'])}{suffix}"
    if name is None:
        name = "rss" if t == "tvsearch" else "empty"
    offset = q.get("offset", "0")
    if offset not in ("", "0"):
        name += f"-offset{offset}"
    return name + ".xml"


def synth_nzb(guid: str) -> bytes:
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">\n'
        '<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">\n'
        f'<file poster="fixture@fixture.invalid" date="1736478720" subject="{guid} (1/1)">\n'
        "<groups><group>alt.binaries.fixture</group></groups>\n"
        f'<segments><segment bytes="768000" number="1">{guid}-1@fixture.invalid</segment></segments>\n'
        "</file>\n</nzb>\n"
    ).encode()


def bencode(v) -> bytes:
    if isinstance(v, int):
        return b"i%de" % v
    if isinstance(v, bytes):
        return b"%d:" % len(v) + v
    if isinstance(v, str):
        return bencode(v.encode())
    if isinstance(v, list):
        return b"l" + b"".join(bencode(x) for x in v) + b"e"
    if isinstance(v, dict):
        return b"d" + b"".join(bencode(k) + bencode(v[k]) for k in sorted(v)) + b"e"
    raise TypeError(type(v))


def synth_torrent(guid: str) -> bytes:
    # A single-file torrent whose info hash is stable per guid.
    pieces = hashlib.sha1(guid.encode()).digest()
    return bencode(
        {
            "announce": "udp://tracker.invalid:6969/announce",
            "info": {"length": 768000, "name": f"{guid}.mkv", "piece length": 1048576, "pieces": pieces, "private": 1},
        }
    )

MOCK_USER = "mock"
MOCK_PASSWORD = "mock"
QBIT_SESSION = "mock-session-cookie"
TRANSMISSION_SESSION = "mock-transmission-session-id"
ITEM_SIZE = 768000


def bdecode(data: bytes, i: int = 0):
    """Decode one bencoded value at `i`; dictionaries record where their
    `info` value starts and ends under `__info__` so its hash can be taken."""
    c = data[i : i + 1]
    if c == b"i":
        j = data.index(b"e", i)
        return int(data[i + 1 : j]), j + 1
    if c == b"l":
        i += 1
        out = []
        while data[i : i + 1] != b"e":
            v, i = bdecode(data, i)
            out.append(v)
        return out, i + 1
    if c == b"d":
        i += 1
        out = {}
        while data[i : i + 1] != b"e":
            k, i = bdecode(data, i)
            start = i
            v, i = bdecode(data, i)
            out[k] = v
            if k == b"info":
                out[b"__info__"] = (start, i)
        return out, i + 1
    j = data.index(b":", i)
    n = int(data[i:j])
    return data[j + 1 : j + 1 + n], j + 1 + n


def torrent_summary(data: bytes) -> tuple[str, str, int]:
    """(info hash upper-case hex, name, total size) of a .torrent file."""
    meta, _ = bdecode(data)
    start, end = meta[b"__info__"]
    info = meta[b"info"]
    size = info.get(b"length") or sum(f.get(b"length", 0) for f in info.get(b"files", []))
    return hashlib.sha1(data[start:end]).hexdigest().upper(), info[b"name"].decode(), int(size)


def magnet_summary(url: str) -> tuple[str, str]:
    """(info hash, display name) of a magnet link; the hash is required."""
    q = urllib.parse.parse_qs(url.partition("?")[2])
    xt = next((x for x in q.get("xt", []) if x.startswith("urn:btih:")), "")
    hash_ = xt.rpartition(":")[2].upper()
    if len(hash_) != 40:
        raise ValueError("magnet without a 40 character btih")
    return hash_, (q.get("dn") or [hash_])[0]


def parse_multipart(body: bytes, content_type: str) -> dict[str, tuple[str | None, bytes]]:
    """Form parts by name: (filename, data)."""
    boundary = content_type.split("boundary=", 1)[1].split(";")[0].strip().strip('"').encode()
    out = {}
    for part in body.split(b"--" + boundary)[1:]:
        if part.startswith(b"--"):
            break
        head, _, data = part.lstrip(b"\r\n").partition(b"\r\n\r\n")
        if data.endswith(b"\r\n"):
            data = data[:-2]
        name, filename = None, None
        for line in head.decode("latin-1").split("\r\n"):
            if line.lower().startswith("content-disposition:"):
                for piece in line.split(";")[1:]:
                    k, _, v = piece.strip().partition("=")
                    v = v.strip('"')
                    if k == "name":
                        name = v
                    elif k == "filename":
                        filename = v
        if name is not None:
            out[name] = (filename, data)
    return out


class ClientState:
    """What the four mock download clients remember between requests."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.reset()

    def reset(self) -> None:
        self.sab: list[dict] = []
        self.nzbget: list[dict] = []
        self.qbit: list[dict] = []
        self.qbit_categories: dict[str, str] = {"tv": "", "movies": "/downloads/movies"}
        self.transmission: list[dict] = []
        self.seq = 0

    def next_seq(self) -> int:
        self.seq += 1
        return self.seq

    # Each client keeps dicts with `id`, `title`, `category`, `size`, `done`
    # (bytes), `state` in {"downloading", "completed", "failed"} plus what
    # its protocol needs. `complete`/`fail` move an item along.
    def items(self, client: str) -> list[dict]:
        return {"sabnzbd": self.sab, "nzbget": self.nzbget, "qbittorrent": self.qbit, "transmission": self.transmission}[client]

    def advance(self, client: str, id_: str, state: str) -> bool:
        for item in self.items(client):
            if item["id"].lower() == id_.lower():
                item["state"] = state
                item["done"] = item["size"] if state == "completed" else item["done"]
                return True
        return False


STATE = ClientState()


def basic_auth_ok(headers) -> bool:
    auth = headers.get("Authorization", "")
    if not auth.startswith("Basic "):
        return False
    try:
        user, _, password = base64.b64decode(auth[6:]).decode().partition(":")
    except Exception:
        return False
    return user == MOCK_USER and password == MOCK_PASSWORD


def hms(seconds: int) -> str:
    return f"{seconds // 3600}:{seconds % 3600 // 60:02d}:{seconds % 60:02d}"


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    fixtures: Path
    logfile: Path | None

    def log_message(self, fmt, *args):
        pass

    def record(self, status: int, served: str) -> None:
        parsed = urllib.parse.urlparse(self.path)
        entry = {
            "time": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "method": self.command,
            "path": parsed.path,
            "query": dict(urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)),
            "userAgent": self.headers.get("User-Agent", ""),
            "status": status,
            "served": served,
        }
        if self.logfile:
            with self.logfile.open("a", encoding="utf-8") as f:
                f.write(json.dumps(entry, ensure_ascii=False) + "\n")
        log(f"{status} {self.command} {parsed.path}?{parsed.query} -> {served}")

    def reply(self, status: int, body: bytes, content_type: str, served: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
        self.record(status, served)

    def do_GET(self):
        self.dispatch(b"")

    do_HEAD = do_GET

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        self.dispatch(self.rfile.read(length))

    def dispatch(self, body: bytes) -> None:
        parsed = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(parsed.query, keep_blank_values=True))
        path = parsed.path
        for kind in ("newznab", "torznab"):
            prefix = f"/{kind}"
            if path == f"{prefix}/api":
                return self.indexer(kind, q)
            if path.startswith(f"{prefix}/download/"):
                return self.download(kind, path.rsplit("/", 1)[-1])
        with STATE.lock:
            if path == "/sabnzbd/api":
                return self.sabnzbd(q, body)
            if path == "/nzbget/jsonrpc":
                return self.nzbget(body)
            if path.startswith("/qbittorrent/api/v2/"):
                return self.qbittorrent(path[len("/qbittorrent/api/v2/"):], q, body)
            if path == "/transmission/rpc":
                return self.transmission(body)
            if path.startswith("/mock/"):
                return self.control(path[len("/mock/"):], q)
        self.reply(404, b"not found\n", "text/plain", "404")

    def json(self, obj, served: str, status: int = 200, headers: dict | None = None) -> None:
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
        self.record(status, served)

    def text(self, status: int, text: str, served: str, headers: dict | None = None) -> None:
        body = text.encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
        self.record(status, served)

    def form(self, body: bytes) -> dict[str, str]:
        """A posted form, urlencoded or multipart (file parts as bytes)."""
        ctype = self.headers.get("Content-Type", "")
        if ctype.startswith("multipart/form-data"):
            return {k: (v[1] if v[0] is not None else v[1].decode()) for k, v in parse_multipart(body, ctype).items()}
        return {k: v for k, v in urllib.parse.parse_qsl(body.decode(), keep_blank_values=True)}

    # --- control

    def control(self, rest: str, q: dict[str, str]) -> None:
        if rest == "reset":
            STATE.reset()
            return self.json({"ok": True}, "mock/reset")
        client, _, action = rest.partition("/")
        if client in ("sabnzbd", "nzbget", "qbittorrent", "transmission") and action in ("complete", "fail"):
            found = STATE.advance(client, q.get("id", ""), "completed" if action == "complete" else "failed")
            return self.json({"ok": found}, f"mock/{client}/{action}", 200 if found else 404)
        self.reply(404, b"not found\n", "text/plain", "404")

    # --- SABnzbd

    def sabnzbd(self, q: dict[str, str], body: bytes) -> None:
        mode = q.get("mode", "")
        if "apikey" in q:
            if q["apikey"] != API_KEY:
                return self.json({"status": False, "error": "API Key Incorrect"}, "sabnzbd/key-incorrect")
        elif q.get("ma_username") != MOCK_USER or q.get("ma_password") != MOCK_PASSWORD:
            return self.json({"status": False, "error": "API Key Required"}, "sabnzbd/key-required")
        if mode == "version":
            return self.json({"version": "4.4.1"}, "sabnzbd/version")
        if mode == "get_config":
            return self.json(
                {
                    "config": {
                        "misc": {
                            "complete_dir": "/downloads/complete",
                            "tv_categories": ["sorted"],
                            "enable_tv_sorting": True,
                            "movie_categories": [],
                            "enable_movie_sorting": False,
                            "date_categories": [],
                            "enable_date_sorting": False,
                            "pre_check": False,
                            "history_retention": "",
                            "history_retention_option": "all",
                            "history_retention_number": 0,
                        },
                        "categories": [
                            {"name": "*", "dir": "", "priority": 0, "pp": "3", "script": "None", "newzbin": ""},
                            {"name": "tv", "dir": "tv", "priority": -100, "pp": "", "script": "Default", "newzbin": ""},
                            {"name": "nojob", "dir": "nojob*", "priority": -100, "pp": "", "script": "Default", "newzbin": ""},
                            {"name": "sorted", "dir": "sorted", "priority": -100, "pp": "", "script": "Default", "newzbin": ""},
                        ],
                        "servers": [],
                        "sorters": [],
                    }
                },
                "sabnzbd/get_config",
            )
        if mode == "fullstatus":
            return self.json({"status": {"completedir": "/downloads/complete"}}, "sabnzbd/fullstatus")
        if mode == "addfile":
            parts = parse_multipart(body, self.headers.get("Content-Type", ""))
            filename, data = parts.get("name", (None, b""))
            if not filename or not data:
                return self.json({"status": False, "error": "no nzb uploaded"}, "sabnzbd/addfile-empty")
            item = {
                "id": f"SABnzbd_nzo_mock{STATE.next_seq()}",
                "title": filename[:-4] if filename.lower().endswith(".nzb") else filename,
                "category": q.get("cat", "*"),
                "priority": int(q.get("priority", "-100")),
                "size": ITEM_SIZE,
                "done": 0,
                "state": "downloading",
            }
            STATE.sab.append(item)
            return self.json({"status": True, "nzo_ids": [item["id"]]}, "sabnzbd/addfile")
        if mode == "queue":
            if q.get("name") == "delete":
                ids = q.get("value", "").split(",")
                STATE.sab = [i for i in STATE.sab if not (i["id"] in ids and i["state"] == "downloading")]
                return self.json({"status": True, "nzo_ids": ids}, "sabnzbd/queue-delete")
            slots = []
            for n, i in enumerate(x for x in STATE.sab if x["state"] == "downloading"):
                if q.get("category") and i["category"] != q["category"]:
                    continue
                slots.append(
                    {
                        "status": "Downloading",
                        "index": n,
                        "timeleft": hms(600),
                        "mb": f"{i['size'] / 1048576:.2f}",
                        "filename": i["title"],
                        "priority": {-100: "Default", -2: "Paused", -1: "Low", 0: "Normal", 1: "High", 2: "Force"}[i["priority"]],
                        "cat": i["category"],
                        "mbleft": f"{(i['size'] - i['done']) / 1048576:.2f}",
                        "percentage": str(i["done"] * 100 // i["size"]),
                        "nzo_id": i["id"],
                    }
                )
            return self.json({"queue": {"my_home": "/downloads", "paused": False, "slots": slots}}, "sabnzbd/queue")
        if mode == "history":
            if q.get("name") == "delete":
                ids = q.get("value", "").split(",")
                STATE.sab = [i for i in STATE.sab if not (i["id"] in ids and i["state"] != "downloading")]
                return self.json({"status": True, "nzo_ids": ids}, "sabnzbd/history-delete")
            slots = []
            for i in STATE.sab:
                if i["state"] == "downloading" or (q.get("category") and i["category"] != q["category"]):
                    continue
                failed = i["state"] == "failed"
                slots.append(
                    {
                        "fail_message": "Unpacking failed, archive requires a password" if failed else "",
                        "bytes": i["size"],
                        "category": i["category"],
                        "nzb_name": i["title"] + ".nzb",
                        "download_time": 60,
                        "storage": "" if failed else f"/downloads/complete/{i['category']}/{i['title']}",
                        "status": "Failed" if failed else "Completed",
                        "nzo_id": i["id"],
                        "name": i["title"],
                    }
                )
            return self.json({"history": {"paused": False, "slots": slots}}, "sabnzbd/history")
        return self.json({"status": False, "error": f"not supported by the mock: {mode}"}, f"sabnzbd/{mode}")

    # --- NZBGet

    def nzbget(self, body: bytes) -> None:
        if not basic_auth_ok(self.headers):
            return self.text(401, "Unauthorized", "nzbget/401", {"WWW-Authenticate": 'Basic realm="NZBGet"'})
        try:
            call = json.loads(body or b"{}")
        except ValueError:
            return self.json({"version": "1.1", "error": {"name": "JSONRPCError", "code": 1, "message": "bad json"}, "result": None}, "nzbget/bad-json")
        method = call.get("method", "")
        params = call.get("params", [])
        rid = call.get("id")

        def ok(result, served):
            return self.json({"version": "1.1", "result": result, "error": None, "id": rid}, f"nzbget/{served}")

        def lohi(n):
            return n & 0xFFFFFFFF, n >> 32

        if method == "version":
            return ok("24.3", "version")
        if method == "config":
            cfg = [
                ("MainDir", "/downloads"), ("DestDir", "${MainDir}/completed"), ("InterDir", "${MainDir}/intermediate"),
                ("AppendCategoryDir", "yes"), ("KeepHistory", "7"),
                ("Category1.Name", "tv"), ("Category1.DestDir", ""), ("Category1.Unpack", "yes"), ("Category1.DefScript", ""), ("Category1.Aliases", ""),
                ("Category2.Name", "movies"), ("Category2.DestDir", "/downloads/movies"), ("Category2.Unpack", "yes"), ("Category2.DefScript", ""), ("Category2.Aliases", ""),
            ]
            return ok([{"Name": k, "Value": v} for k, v in cfg], "config")
        if method == "status":
            remaining = sum(i["size"] - i["done"] for i in STATE.nzbget if i["state"] == "downloading")
            lo, hi = lohi(remaining)
            return ok({"RemainingSizeLo": lo, "RemainingSizeHi": hi, "DownloadedSizeLo": 0, "DownloadedSizeHi": 0, "DownloadRate": 1048576, "AverageDownloadRate": 1048576, "DownloadLimit": 0, "DownloadPaused": False}, "status")
        if method == "listgroups":
            groups = []
            for i in STATE.nzbget:
                if i["state"] != "downloading":
                    continue
                size_lo, size_hi = lohi(i["size"])
                rem_lo, rem_hi = lohi(i["size"] - i["done"])
                groups.append({"NZBID": i["nzb_id"], "FirstID": i["nzb_id"], "LastID": i["nzb_id"], "NZBName": i["title"], "Category": i["category"], "FileSizeLo": size_lo, "FileSizeHi": size_hi, "RemainingSizeLo": rem_lo, "RemainingSizeHi": rem_hi, "PausedSizeLo": 0, "PausedSizeHi": 0, "MinPriority": i["priority"], "MaxPriority": i["priority"], "ActiveDownloads": 1, "Parameters": [{"Name": "drone", "Value": i["id"]}]})
            return ok(groups, "listgroups")
        if method == "history":
            hist = []
            for i in reversed(STATE.nzbget):
                if i["state"] == "downloading":
                    continue
                size_lo, size_hi = lohi(i["size"])
                failed = i["state"] == "failed"
                hist.append({"ID": i["nzb_id"], "Name": i["title"], "Category": i["category"], "FileSizeLo": size_lo, "FileSizeHi": size_hi, "ParStatus": "FAILURE" if failed else "SUCCESS", "UnpackStatus": "NONE" if failed else "SUCCESS", "MoveStatus": "NONE" if failed else "SUCCESS", "ScriptStatus": "NONE", "DeleteStatus": "NONE", "MarkStatus": "NONE", "DestDir": f"/downloads/completed/{i['category']}/{i['title']}", "FinalDir": "", "Parameters": [{"Name": "drone", "Value": i["id"]}]})
            return ok(hist, "history")
        if method == "append":
            # append(NZBFilename, Content, Category, Priority, AddToTop, AddPaused, DupeKey, DupeScore, DupeMode, PPParameters)
            if len(params) < 10:
                return ok(0, "append-short")
            title, content, category, priority = params[0], params[1], params[2], params[3]
            try:
                base64.b64decode(content, validate=True)
            except Exception:
                return ok(0, "append-bad-content")
            pp = params[9] or []
            drone = pp[1] if len(pp) >= 2 and pp[0] == "drone" else None
            nzb_id = STATE.next_seq()
            STATE.nzbget.append({"id": drone or str(nzb_id), "nzb_id": nzb_id, "title": title[:-4] if title.lower().endswith(".nzb") else title, "category": category, "priority": priority, "size": ITEM_SIZE, "done": 0, "state": "downloading"})
            return ok(nzb_id, "append")
        if method == "editqueue":
            command, ids = params[0], params[3]
            ids = ids if isinstance(ids, list) else [ids]
            if command in ("GroupFinalDelete", "GroupDelete"):
                STATE.nzbget = [i for i in STATE.nzbget if not (i["nzb_id"] in ids and i["state"] == "downloading")]
            elif command == "HistoryDelete":
                STATE.nzbget = [i for i in STATE.nzbget if not (i["nzb_id"] in ids and i["state"] != "downloading")]
            elif command == "HistoryRedownload":
                for i in STATE.nzbget:
                    if i["nzb_id"] in ids:
                        i["state"], i["done"] = "downloading", 0
            return ok(True, f"editqueue-{command}")
        return self.json({"version": "1.1", "result": None, "error": {"name": "JSONRPCError", "code": 1, "message": f"Method not found: {method}"}, "id": rid}, f"nzbget/{method}")

    # --- qBittorrent (Web API v2)

    def qbittorrent(self, rest: str, q: dict[str, str], body: bytes) -> None:
        if rest == "auth/login":
            f = self.form(body)
            if f.get("username") == MOCK_USER and f.get("password") == MOCK_PASSWORD:
                return self.text(200, "Ok.", "qbittorrent/login", {"Set-Cookie": f"SID={QBIT_SESSION}; HttpOnly; path=/"})
            return self.text(200, "Fails.", "qbittorrent/login-fails")
        auth = self.headers.get("Authorization", "")
        cookies = {c.strip().partition("=")[0]: c.strip().partition("=")[2] for c in self.headers.get("Cookie", "").split(";") if "=" in c}
        if auth != f"Bearer {API_KEY}" and cookies.get("SID") != QBIT_SESSION:
            return self.text(403, "Forbidden", "qbittorrent/403")
        f = self.form(body) if body else {}
        if rest == "app/webapiVersion":
            return self.text(200, "2.11.4", "qbittorrent/webapiVersion")
        if rest == "app/version":
            return self.text(200, "v5.1.2", "qbittorrent/version")
        if rest == "app/preferences":
            return self.json({"save_path": "/downloads/", "max_ratio_enabled": False, "max_ratio": -1, "max_seeding_time_enabled": False, "max_seeding_time": -1, "max_inactive_seeding_time_enabled": False, "max_inactive_seeding_time": -1, "max_ratio_act": 0, "queueing_enabled": True, "dht": True}, "qbittorrent/preferences")
        if rest == "torrents/categories":
            return self.json({name: {"name": name, "savePath": path} for name, path in STATE.qbit_categories.items()}, "qbittorrent/categories")
        if rest == "torrents/createCategory":
            STATE.qbit_categories.setdefault(f.get("category", ""), f.get("savePath", ""))
            return self.text(200, "", "qbittorrent/createCategory")
        if rest == "torrents/info":
            out = []
            for t in STATE.qbit:
                if q.get("category") and t["category"] != q["category"]:
                    continue
                save_path = f"/downloads/{t['category']}/" if t["category"] else "/downloads/"
                state = {"downloading": "downloading", "completed": "stoppedUP", "failed": "error"}[t["state"]]
                progress = t["done"] / t["size"]
                out.append({"hash": t["id"].lower(), "name": t["title"], "size": t["size"], "progress": progress, "eta": 8640000 if progress >= 1 else 600, "state": state, "category": t["category"], "save_path": save_path, "content_path": save_path + t["title"], "ratio": 0.0 if progress < 1 else 1.0, "ratio_limit": -2, "seeding_time_limit": -2, "inactive_seeding_time_limit": -2, "last_activity": 1736478720, "added_on": 1736478720})
            return self.json(out, "qbittorrent/info")
        if rest == "torrents/properties":
            for t in STATE.qbit:
                if t["id"].lower() == q.get("hash", "").lower():
                    return self.json({"hash": t["id"].lower(), "save_path": f"/downloads/{t['category']}/" if t["category"] else "/downloads/", "seeding_time": 0}, "qbittorrent/properties")
            return self.text(404, "Not Found", "qbittorrent/properties-404")
        if rest == "torrents/files":
            for t in STATE.qbit:
                if t["id"].lower() == q.get("hash", "").lower():
                    return self.json([{"name": f"{t['title']}/{t['title']}.mkv", "size": t["size"], "progress": t["done"] / t["size"], "priority": 1}], "qbittorrent/files")
            return self.text(404, "Not Found", "qbittorrent/files-404")
        if rest == "torrents/add":
            try:
                if "urls" in f:
                    hash_, name = magnet_summary(f["urls"]) if f["urls"].startswith("magnet:") else (hashlib.sha1(f["urls"].encode()).hexdigest().upper(), f["urls"].rsplit("/", 1)[-1])
                    size = ITEM_SIZE
                else:
                    hash_, name, size = torrent_summary(f["torrents"])
            except Exception as e:
                log(f"qbittorrent add rejected: {e}")
                return self.text(200, "Fails.", "qbittorrent/add-fails")
            if any(t["id"] == hash_ for t in STATE.qbit):
                return self.text(200, "Ok.", "qbittorrent/add-duplicate")
            STATE.qbit.append({"id": hash_, "title": name[:-4] if name.endswith(".mkv") else name, "category": f.get("category", ""), "size": size or ITEM_SIZE, "done": 0, "state": "downloading", "paused": f.get("stopped", f.get("paused", "false")) == "true", "form": {k: v for k, v in f.items() if k != "torrents"}})
            return self.text(200, "Ok.", "qbittorrent/add")
        if rest == "torrents/delete":
            hashes = f.get("hashes", "").lower().split("|")
            STATE.qbit = [t for t in STATE.qbit if t["id"].lower() not in hashes]
            return self.text(200, "", "qbittorrent/delete")
        if rest == "torrents/setCategory":
            for t in STATE.qbit:
                if t["id"].lower() in f.get("hashes", "").lower().split("|"):
                    t["category"] = f.get("category", "")
            return self.text(200, "", "qbittorrent/setCategory")
        if rest in ("torrents/setShareLimits", "torrents/topPrio", "torrents/setForceStart", "torrents/pause", "torrents/stop", "torrents/resume", "torrents/start"):
            return self.text(200, "", f"qbittorrent/{rest.split('/')[1]}")
        return self.text(404, "Not Found", f"qbittorrent/{rest}-404")

    # --- Transmission

    def transmission(self, body: bytes) -> None:
        if not basic_auth_ok(self.headers):
            return self.text(401, "401: Unauthorized", "transmission/401", {"WWW-Authenticate": 'Basic realm="Transmission"'})
        if self.headers.get("X-Transmission-Session-Id") != TRANSMISSION_SESSION:
            return self.text(409, "409: Conflict", "transmission/409", {"X-Transmission-Session-Id": TRANSMISSION_SESSION})
        try:
            call = json.loads(body or b"{}")
        except ValueError:
            return self.json({"result": "bad json", "arguments": {}}, "transmission/bad-json")
        method = call.get("method", "")
        args = call.get("arguments") or {}

        def ok(arguments, served):
            return self.json({"result": "success", "arguments": arguments}, f"transmission/{served}")

        def torrent_json(t):
            return {"id": t["tid"], "hashString": t["id"].lower(), "name": t["title"], "downloadDir": t["dir"], "totalSize": t["size"], "leftUntilDone": t["size"] - t["done"], "isFinished": t["state"] == "completed", "eta": -1 if t["state"] != "downloading" else 600, "status": {"downloading": 4, "completed": 6, "failed": 0}[t["state"]], "secondsDownloading": 60, "secondsSeeding": 0, "errorString": "Tracker gave an error: unregistered torrent" if t["state"] == "failed" else "", "downloadedEver": t["done"], "uploadedEver": 0, "seedRatioLimit": 2.0, "seedRatioMode": 0, "seedIdleLimit": 30, "seedIdleMode": 0, "file-count": 1, "labels": t["labels"]}

        def selected():
            ids = args.get("ids")
            if ids is None:
                return list(STATE.transmission)
            ids = [str(i).lower() for i in (ids if isinstance(ids, list) else [ids])]
            return [t for t in STATE.transmission if t["id"].lower() in ids or str(t["tid"]) in ids]

        if method == "session-get":
            return ok({"rpc-version": 17, "version": "4.0.6 (38c164933e)", "download-dir": "/downloads", "seedRatioLimit": 2.0, "seedRatioLimited": False, "idle-seeding-limit": 30, "idle-seeding-limit-enabled": False}, "session-get")
        if method == "torrent-get":
            return ok({"torrents": [torrent_json(t) for t in selected()]}, "torrent-get")
        if method == "torrent-add":
            try:
                if "metainfo" in args:
                    hash_, name, size = torrent_summary(base64.b64decode(args["metainfo"]))
                else:
                    filename = args.get("filename", "")
                    hash_, name = magnet_summary(filename) if filename.startswith("magnet:") else (hashlib.sha1(filename.encode()).hexdigest().upper(), filename.rsplit("/", 1)[-1])
                    size = ITEM_SIZE
            except Exception as e:
                return self.json({"result": f"invalid or corrupt torrent file: {e}", "arguments": {}}, "transmission/add-invalid")
            for t in STATE.transmission:
                if t["id"] == hash_:
                    return ok({"torrent-duplicate": {"id": t["tid"], "hashString": hash_.lower(), "name": t["title"]}}, "torrent-add-duplicate")
            tid = STATE.next_seq()
            t = {"id": hash_, "tid": tid, "title": name[:-4] if name.endswith(".mkv") else name, "category": "", "dir": args.get("download-dir") or "/downloads", "labels": args.get("labels") or [], "size": size or ITEM_SIZE, "done": 0, "state": "downloading", "paused": bool(args.get("paused"))}
            STATE.transmission.append(t)
            return ok({"torrent-added": {"id": tid, "hashString": hash_.lower(), "name": t["title"]}}, "torrent-add")
        if method == "torrent-set":
            for t in selected():
                if "labels" in args:
                    t["labels"] = list(args["labels"])
                for k in ("seedRatioLimit", "seedRatioMode", "seedIdleLimit", "seedIdleMode"):
                    if k in args:
                        t[k] = args[k]
            return ok({}, "torrent-set")
        if method == "torrent-remove":
            gone = {t["id"] for t in selected()}
            STATE.transmission = [t for t in STATE.transmission if t["id"] not in gone]
            return ok({}, "torrent-remove")
        if method in ("queue-move-top", "torrent-start", "torrent-stop", "torrent-start-now"):
            return ok({}, method)
        return self.json({"result": f"method name not recognized: {method}", "arguments": {}}, f"transmission/{method}")

    def indexer(self, kind: str, q: dict[str, str]) -> None:
        if q.get("apikey") != API_KEY:
            return self.reply(200, ERROR_100.encode(), "application/xml", "error-100")
        name = result_file(q)
        path = self.fixtures / "indexers" / kind / name
        if path.is_file():
            return self.reply(200, path.read_bytes(), "application/rss+xml", f"{kind}/{name}")
        empty = self.fixtures / "indexers" / kind / "empty.xml"
        body = empty.read_bytes() if empty.is_file() else EMPTY_RSS.encode()
        self.reply(200, body, "application/rss+xml", f"{kind}/{name} (missing: empty)")

    def download(self, kind: str, filename: str) -> None:
        guid, _, ext = filename.rpartition(".")
        sub = "nzb" if ext == "nzb" else "torrent"
        path = self.fixtures / "indexers" / kind / sub / filename
        if path.is_file():
            body = path.read_bytes()
        elif sub == "nzb":
            body = synth_nzb(guid)
        else:
            body = synth_torrent(guid)
        content_type = "application/x-nzb" if sub == "nzb" else "application/x-bittorrent"
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Disposition", f'attachment; filename="{filename}"')
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        self.record(200, f"{kind}/{sub}/{filename}" + ("" if path.is_file() else " (synthesised)"))


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--listen", default="127.0.0.1:18989")
    ap.add_argument("--fixtures", default="tests/fixtures", type=Path)
    ap.add_argument("--log", type=Path, help="append one JSON line per request here")
    args = ap.parse_args()
    host, _, port = args.listen.rpartition(":")
    Handler.fixtures = args.fixtures
    Handler.logfile = args.log
    if args.log:
        args.log.parent.mkdir(parents=True, exist_ok=True)
    server = Server((host or "127.0.0.1", int(port)), Handler)
    log(f"serving {args.fixtures} on {args.listen}; Ctrl-C to stop")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
