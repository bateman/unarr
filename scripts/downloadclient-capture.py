#!/usr/bin/env python3
"""Record the HTTP exchanges between Sonarr and a download client, anonymised.

The mocks under `tests/fixtures/downloadclients/` answer like the real
SABnzbd, NZBGet, qBittorrent and Transmission because they replay exchanges
recorded from real ones. Recording takes two steps, both with this script and
the Python standard library only: a capture (from Sonarr's own trace log, or
through a proxy) and the anonymisation.

1a. From Sonarr's trace log, touching nothing in the installation. Sonarr's
    HTTP client logs every request and, for the download clients, the body of
    every response once the log level is Trace (Settings > General > Logging
    > Log Level). Leave it there for ten to fifteen minutes of normal use,
    put it back to Info, and take `sonarr.trace*.txt` from the `logs`
    directory of Sonarr's config: the files roll over at 1 MB and five are
    kept, so copy them promptly. Then::

        scripts/downloadclient-capture.py from-log --client nzbget \\
            --in logs/sonarr.trace.txt --in logs/sonarr.trace.0.txt \\
            --out nzbget.jsonl

    Only the exchanges with that kind of client are kept, recognised by their
    API path (`/jsonrpc`, `/api?mode=`, `/api/v2/`, `/transmission/rpc`);
    `--target HOST[:PORT]` narrows them to one client when several of the
    same kind exist. Request bodies come as the one-line summary Sonarr logs
    (`listgroups(0)`, form fields), response headers are not logged; what
    the mocks need is the response bodies, which are complete.

1b. Through a proxy, when the trace log is not an option. Run it on a machine
    Sonarr can reach (the one running Sonarr is simplest; from a container,
    the host's LAN address), pointed at the download client, and tell Sonarr
    the download client now lives at the proxy's address and port (Settings >
    Download Clients > the client > Host and Port, then Test and Save). Every
    request Sonarr sends is forwarded unchanged and the pair is appended to
    the capture file::

        scripts/downloadclient-capture.py proxy --listen 0.0.0.0:18080 \\
            --target http://192.168.1.20:8080 --out qbittorrent.jsonl

    Then exercise the client from Sonarr: Test, grab a release from an
    interactive search, let the queue poll it for a few minutes, remove an
    item with "remove from download client", let a download finish. Stop the
    proxy with Ctrl-C and put the original host and port back in Sonarr.

2. Anonymise. Rewrite the capture into one JSON file for the fixture tree::

       scripts/downloadclient-capture.py anonymize --client qbittorrent \\
           --in qbittorrent.jsonl --out tests/fixtures/downloadclients

   What is removed or replaced, keeping every shape the client exposes:

   - Hosts: the target's host and every `Host` header seen become
     `downloadclient.invalid`; any other host or address named with `--host`
     too. Values that look like IPv4 addresses become 127.0.0.1.
   - The installation's exposure: qBittorrent's peer ports (`listen_port`,
     `ssl_listen_port`) and the networks it lets in without a login
     (`bypass_auth_subnet_whitelist`) become a fresh install's values.
   - Credentials: `Authorization` headers become `unarr:fixture`; API keys in
     the query string (`apikey=`), in JSON (`apikey`, `api_key`, `password`,
     `username`...) and in form bodies are replaced; qBittorrent session
     cookies and Transmission session ids get fixed values.
   - URLs: every `http(s)://` value becomes `http://host.invalid/<pseudonym>`
     (trackers are `udp://tracker.invalid:6969/announce`; magnet links keep a
     pseudonymised info hash and the display name only).
   - Uploaded content: NZB and torrent files in multipart bodies, and base64
     blobs longer than 1 KiB in JSON (`append`, `metainfo`), are replaced by
     a placeholder of the same kind; they carry server names and passkeys.
   - Identifiers: torrent info hashes, SABnzbd `nzo_id`s and NZBGet ids are
     replaced by stable pseudonyms, so rows that referred to the same
     download still do.
   - Paths: every absolute path keeps its last component (two if it names a
     file inside a folder) under `/downloads/`.
   - Release names are kept, as `arr-anonymize.py` keeps them: they are
     public and the parser needs them. `--replace OLD=NEW` rewrites anything
     else you spot while reviewing the output; it accepts several.

   Before writing, the script re-reads the output and fails if any original
   host, credential, URL host or identifier is still present. A second
   client of the same kind (a program speaking qBittorrent's API, say) goes
   under its own directory with `--name`.

   The unarr-mock crate replays a capture under `/capture/<name>/`, and
   `crates/unarr-mock/tests/captures.rs` runs Unarr's clients against it;
   `/capture/<name>@<seqs>/` replays only the exchanges with those `seq`
   numbers (`1,18-22`), one moment of a long proxy recording, which
   `crates/unarr-mock/tests/proxy_captures.rs` compares request by request
   with what Unarr sends. SABnzbd only finishes a job with a news server:
   `scripts/nntp-stub.py` stands in for one.
"""

from __future__ import annotations

import argparse
import base64
import datetime
import hashlib
import http.client
import http.server
import json
import re
import socketserver
import sys
import urllib.parse
from pathlib import Path, PurePosixPath, PureWindowsPath

HOP_BY_HOP = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "content-length",
    "host",
    # Ask for plain bodies so the capture is readable.
    "accept-encoding",
}

MAX_BODY = 4 * 1024 * 1024


def log(msg: str) -> None:
    print(f"[capture] {msg}", file=sys.stderr, flush=True)


# --- proxy


def encode_body(raw: bytes, content_type: str) -> object:
    """Text when the body decodes as UTF-8 text, base64 otherwise."""
    if not raw:
        return None
    is_text = content_type.startswith(("text/", "application/json", "application/x-www-form-urlencoded", "application/xml")) or "json" in content_type or "xml" in content_type
    if is_text or content_type.startswith("multipart/"):
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError:
            pass
    return {"base64": base64.b64encode(raw).decode()}


def make_handler(target: urllib.parse.ParseResult, out: Path):
    seq = {"n": 0}

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, fmt, *args):  # quieter than the default
            pass

        def forward(self):
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length) if length else b""
            headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP_BY_HOP}
            headers["Host"] = target.netloc
            conn_cls = http.client.HTTPSConnection if target.scheme == "https" else http.client.HTTPConnection
            conn = conn_cls(target.hostname, target.port, timeout=120)
            path = (target.path.rstrip("/") + self.path) if target.path.strip("/") else self.path
            try:
                conn.request(self.command, path, body=body or None, headers=headers)
                resp = conn.getresponse()
                resp_body = resp.read()
                status, reason = resp.status, resp.reason
                resp_headers = [(k, v) for k, v in resp.getheaders() if k.lower() not in HOP_BY_HOP]
            except OSError as e:
                status, reason, resp_body, resp_headers = 502, "Bad Gateway", str(e).encode(), [("Content-Type", "text/plain")]
            finally:
                conn.close()

            self.send_response(status, reason)
            for k, v in resp_headers:
                self.send_header(k, v)
            self.send_header("Content-Length", str(len(resp_body)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(resp_body)

            seq["n"] += 1
            req_ct = self.headers.get("Content-Type", "")
            resp_ct = dict((k.lower(), v) for k, v in resp_headers).get("content-type", "")
            record = {
                "seq": seq["n"],
                "time": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "request": {
                    "method": self.command,
                    "path": self.path,
                    "headers": {k: v for k, v in self.headers.items()},
                    "body": encode_body(body[:MAX_BODY], req_ct),
                },
                "response": {
                    "status": status,
                    "headers": {k: v for k, v in resp_headers},
                    "body": encode_body(resp_body[:MAX_BODY], resp_ct),
                },
            }
            with out.open("a", encoding="utf-8") as f:
                f.write(json.dumps(record, ensure_ascii=False) + "\n")
            log(f"{seq['n']:4d} {status} {self.command} {self.path.split('?')[0]} ({len(resp_body)} bytes)")

        do_GET = do_POST = do_PUT = do_DELETE = do_HEAD = do_PATCH = do_OPTIONS = forward

    return Handler


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def cmd_proxy(args) -> None:
    target = urllib.parse.urlparse(args.target)
    if target.scheme not in ("http", "https") or not target.hostname:
        sys.exit(f"--target must be http(s)://host[:port][/base], got {args.target!r}")
    host, _, port = args.listen.rpartition(":")
    out = Path(args.out)
    if not out.exists() or out.stat().st_size == 0:
        # The anonymiser removes the target's host wherever the client echoes it.
        out.write_text(json.dumps({"meta": {"target": args.target}}) + "\n", encoding="utf-8")
    server = Server((host or "0.0.0.0", int(port)), make_handler(target, out))
    log(f"forwarding {args.listen} -> {args.target}, appending to {args.out}; Ctrl-C to stop")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log("stopped")


# --- from Sonarr's trace log

CLIENTS = ("sabnzbd", "nzbget", "qbittorrent", "transmission")

# `yy-M-d HH:mm:ss.f|Level|Logger|message`, the layout of Sonarr's log files.
# A message may span lines (JSON pretty-printed by a client, an HTML error
# page): lines that do not start a record continue the previous one.
LOG_LINE_RE = re.compile(r"^(\d{2,4}-\d{1,2}-\d{1,2} \d{2}:\d{2}:\d{2}\.\d+)\|(\w+)\|([^|]+)\|(.*)$", re.DOTALL)
# `HttpRequest.ToString()`: `Req: [GET] url` or `Req: [POST] url: summary`.
REQ_RE = re.compile(r"^Req: (?:HTTP/[\d.]+ )?\[(\w+)\] (\S+?)(?:: (.*))?$", re.DOTALL)
# `HttpResponse.ToString()` plus the timing the client appends:
# `Res: HTTP/1.1 [GET] url: 200.OK (245 bytes) (3 ms)`; older releases leave
# out the protocol and the size; an error carries the body after a newline.
RES_RE = re.compile(r"^Res: (?:HTTP/[\d.]+ )?\[(\w+)\] (\S+?): (?:HTTP )?(\d{3})\.?(\w*)(.*)$", re.DOTALL)
RES_TIMING_RE = re.compile(r"\s*\((\d+) ms\)\s*$")
RES_SIZE_RE = re.compile(r"\s*\((\d+) bytes\)\s*$")
# `Response content (123 bytes): body`, logged when the request asked for it,
# as every download client proxy does.
CONTENT_RE = re.compile(r"^Response content \((\d+) bytes\): (.*)$", re.DOTALL)

# How each client's API is recognised in a URL: the path, or `mode=` for
# SABnzbd whose path is only `/api`.
CLIENT_URL_RE = {
    "nzbget": re.compile(r"/jsonrpc(?:/|$|\?)", re.IGNORECASE),
    "sabnzbd": re.compile(r"/api\?(?:.*&)?mode=", re.IGNORECASE),
    "qbittorrent": re.compile(r"/api/v2/", re.IGNORECASE),
    "transmission": re.compile(r"/(?:transmission/)?rpc(?:/|$|\?)", re.IGNORECASE),
}


def log_timestamp(value: str) -> str:
    """`2026-11-07 16:33:52.1` or `26-11-7 16:33:52.1` (Sonarr's local time)
    as the capture writes it."""
    date, _, clock = value.partition(" ")
    y, m, d = (int(x) for x in date.split("-"))
    if y < 100:
        y += 2000
    return f"{y:04d}-{m:02d}-{d:02d}T{clock[:8]}Z"


def read_log_records(path: Path) -> list[tuple[str, str, str, str]]:
    """(timestamp, level, logger, message) per record, continuation lines joined."""
    records: list[list[str]] = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        m = LOG_LINE_RE.match(line)
        if m:
            records.append(list(m.groups()))
        elif records:
            records[-1][3] += "\n" + line
    return [tuple(r) for r in records]


def guess_content_type(body: str | None) -> str:
    if not body:
        return ""
    head = body.lstrip()[:1]
    if head in ("{", "["):
        return "application/json"
    if head == "<":
        return "text/html" if "<html" in body[:200].lower() else "application/xml"
    return "text/plain"


def exchanges_from_logs(paths: list[Path]) -> list[dict]:
    """Every HttpClient request paired with its response, in log order."""
    # Log files roll over by size; order them by their first timestamp rather
    # than trusting the archive numbering.
    files = []
    for path in paths:
        records = read_log_records(path)
        if records:
            files.append((records[0][0], str(path), records))
    files.sort(key=lambda f: (log_timestamp(f[0]), f[1]))

    pending_requests: list[dict] = []  # Req lines not yet answered
    exchanges: list[dict] = []
    awaiting_content: list[dict] = []  # Res lines whose body may follow
    seq = 0
    for _, _, records in files:
        for timestamp, level, logger, message in records:
            if logger.strip() != "HttpClient":
                continue
            if m := REQ_RE.match(message):
                method, url, summary = m.groups()
                pending_requests.append({"method": method, "url": url, "summary": summary, "time": timestamp})
                continue
            if m := RES_RE.match(message):
                method, url, status, _reason, rest = m.groups()
                rest = RES_TIMING_RE.sub("", rest)
                size = None
                if s := RES_SIZE_RE.search(rest):
                    size = int(s.group(1))
                    rest = rest[: s.start()]
                error_body = rest.strip("\n") or None
                request = next((r for r in pending_requests if r["method"] == method and r["url"] == url), None)
                if request:
                    pending_requests.remove(request)
                seq += 1
                exchange = {
                    "seq": seq,
                    "time": log_timestamp(request["time"] if request else timestamp),
                    "method": method,
                    "url": url,
                    "summary": request["summary"] if request else None,
                    "status": int(status),
                    "size": size,
                    "body": error_body,
                    "content_logged": False,
                }
                exchanges.append(exchange)
                awaiting_content.append(exchange)
                continue
            if m := CONTENT_RE.match(message):
                # The content line follows its response in the same task; with
                # concurrent requests another client's lines may sit between,
                # so take the most recent response of the same size (when the
                # log says it), else the most recent still without a body.
                length, body = int(m.group(1)), m.group(2)
                candidates = [x for x in reversed(awaiting_content) if x["size"] == length]
                if candidates or awaiting_content:
                    exchange = candidates[0] if candidates else awaiting_content[-1]
                    awaiting_content.remove(exchange)
                    exchange["body"] = body
                    exchange["content_logged"] = True
                continue
    return exchanges


def exchange_record(x: dict) -> dict:
    """An exchange in the proxy's record shape, so `anonymize` reads both."""
    parsed = urllib.parse.urlparse(x["url"])
    path = parsed.path or "/"
    if parsed.query:
        path += "?" + parsed.query
    request_headers = {"Host": parsed.netloc}
    body = x["summary"]
    if body is not None:
        request_headers["Content-Type"] = guess_content_type(body) or "text/plain"
    response_headers = {}
    if x["body"] is not None:
        response_headers["Content-Type"] = guess_content_type(x["body"])
    return {
        "seq": x["seq"],
        "time": x["time"],
        "request": {"method": x["method"], "path": path, "headers": request_headers, "body": body},
        "response": {"status": x["status"], "headers": response_headers, "body": x["body"]},
    }


def cmd_from_log(args) -> None:
    paths = [Path(p) for p in args.input]
    for p in paths:
        if not p.is_file():
            sys.exit(f"{p}: not a file")
    exchanges = exchanges_from_logs(paths)
    if not exchanges:
        sys.exit("no HttpClient trace lines found: is the log level Trace, and are these sonarr.trace*.txt files?")
    client_re = CLIENT_URL_RE[args.client]
    kept = [x for x in exchanges if client_re.search(x["url"]) and (not args.target or args.target in x["url"])]
    if not kept:
        hosts = sorted({urllib.parse.urlparse(x["url"]).netloc for x in exchanges})
        sys.exit(f"{len(exchanges)} exchanges read, none with a {args.client} API path" + (f" at {args.target!r}" if args.target else "") + f"; hosts seen: {', '.join(hosts)}")
    targets = sorted({urllib.parse.urlparse(x["url"])._replace(path="", params="", query="", fragment="").geturl() for x in kept})
    if len(targets) > 1:
        sys.exit(f"exchanges with {len(targets)} different {args.client} hosts: {', '.join(targets)}; pick one with --target")
    without_body = sum(1 for x in kept if not x["content_logged"] and x["body"] is None)
    out = Path(args.out)
    with out.open("w", encoding="utf-8") as f:
        f.write(json.dumps({"meta": {"target": targets[0], "source": "sonarr-trace", "client": args.client}}) + "\n")
        for n, x in enumerate(kept, 1):
            x["seq"] = n
            f.write(json.dumps(exchange_record(x), ensure_ascii=False) + "\n")
    log(f"{len(kept)} {args.client} exchanges with {targets[0]} written to {out} ({len(exchanges)} HTTP exchanges in the logs)")
    if without_body:
        log(f"{without_body} responses have no body in the log; they are kept with status only")
    log(f"next: {sys.argv[0]} anonymize --client {args.client} --in {out}")


# --- anonymise

# Matched anywhere in the key name (`web_ui_username`, `nzbget_password`).
SECRET_JSON_KEY = re.compile(
    r"(apikey|api_key|^key$|password|passwd|^pass$|username|user_name|^user$|login|token|cookie|^sid$|"
    r"session|secret|email|mail|host|hostname|^ip$|address|^server|servers)",
    re.IGNORECASE,
)
HOST_JSON_KEY = re.compile(r"(host|^ip$|address|^server|servers)", re.IGNORECASE)
USER_JSON_KEY = re.compile(r"(username|user_name|^user$|login)", re.IGNORECASE)
PASS_JSON_KEY = re.compile(r"(password|passwd|^pass$)", re.IGNORECASE)
URL_RE = re.compile(r"\b(?:https?|udp|wss?)://[^\s\"'<>\\]+", re.IGNORECASE)
MAGNET_RE = re.compile(r"magnet:\?[^\s\"'<>\\]+", re.IGNORECASE)
HASH40_RE = re.compile(r"\b[0-9a-f]{40}\b", re.IGNORECASE)
HASH64_RE = re.compile(r"\b[0-9a-f]{64}\b", re.IGNORECASE)
NZO_RE = re.compile(r"\bSABnzbd_nzo_[A-Za-z0-9_-]+")
IPV4_RE = re.compile(r"(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}(?![\w.])")
POSIX_PATH_RE = re.compile(r"(?<![\w:/])/(?:[^/\s\"'<>|?*\\]+/)+[^/\s\"'<>|?*\\]*")
WIN_PATH_RE = re.compile(r"\b[A-Za-z]:\\(?:[^\\\s\"'<>|?*]+\\)*[^\\\s\"'<>|?*]*")
B64_BLOB_RE = re.compile(r"[A-Za-z0-9+/]{1024,}={0,2}")
FORM_SECRET_RE = re.compile(r"(?i)\b(apikey|api_key|username|password|user|pass|ma_username|ma_password)=([^&\s]*)")
QUERY_APIKEY_RE = re.compile(r"(?i)([?&](?:apikey|api_key|key|token)=)([^&\s]+)")

PLACEHOLDER_NZB = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\"><file poster=\"unarr@fixture.invalid\" date=\"0\" subject=\"fixture\"><groups><group>alt.binaries.fixture</group></groups><segments><segment bytes=\"1\" number=\"1\">fixture@fixture.invalid</segment></segments></file></nzb>\n"
PLACEHOLDER_TORRENT_B64 = base64.b64encode(
    b"d8:announce30:udp://tracker.invalid:6969/announce4:infod6:lengthi1e4:name7:fixture12:piece lengthi16384e6:pieces20:" + b"\0" * 20 + b"ee"
).decode()


# qBittorrent's preferences that describe the installation's exposure, and
# the values a fresh qBittorrent has for them.
INSTALLATION_DEFAULTS = {
    "listen_port": 6881,
    "ssl_listen_port": 0,
    "bypass_auth_subnet_whitelist": "",
}


class Anonymizer:
    def __init__(self, client: str, hosts: list[str], replacements: list[tuple[str, str]]):
        self.client = client
        self.salt = "unarr-downloadclient-fixture"
        self.originals: dict[str, str] = {}  # value that must not survive -> kind
        self.hosts = set(hosts)
        self.replacements = replacements
        self.hash_map: dict[str, str] = {}

    # Stable pseudonyms: the same original always gets the same replacement.
    def pseudonym(self, value: str, kind: str, length: int = 40) -> str:
        key = value.lower()
        if key in self.hash_map.values():
            return value  # already a pseudonym (text scanned twice)
        if key not in self.hash_map:
            self.hash_map[key] = hashlib.sha256(f"{self.salt}:{kind}:{key}".encode()).hexdigest()[:length]
        self.remember(value, kind)
        return self.hash_map[key]

    def remember(self, value: str, kind: str) -> None:
        if value and len(value) >= 3 and value not in ("127.0.0.1", "localhost"):
            self.originals.setdefault(value, kind)

    # --- the three scopes: header values, text bodies, structured JSON

    def scrub_text(self, text: str, paths: bool = True) -> str:
        """Rewrite one piece of text. `paths` is off for request paths, which are
        API routes, not files."""
        for old, new in self.replacements:
            if old in text:
                self.remember(old, "replace")
                text = text.replace(old, new)

        def magnet(m: re.Match) -> str:
            qs = urllib.parse.parse_qs(m.group(0)[len("magnet:?"):])
            xt = (qs.get("xt") or [""])[0]
            h = xt.rsplit(":", 1)[-1]  # pseudonymised with every other hash below
            dn = (qs.get("dn") or [""])[0]
            out = f"magnet:?xt=urn:btih:{h}"
            if dn:
                out += "&dn=" + urllib.parse.quote(dn)
            return out

        text = MAGNET_RE.sub(magnet, text)

        def url(m: re.Match) -> str:
            u = m.group(0)
            parsed = urllib.parse.urlparse(u)
            if parsed.hostname:
                self.remember(parsed.hostname, "url host")
            if parsed.scheme.lower() == "udp":
                return "udp://tracker.invalid:6969/announce"
            return f"http://host.invalid/{self.pseudonym(u, 'url', 12)}"

        text = URL_RE.sub(url, text)
        for host in sorted(self.hosts, key=len, reverse=True):
            if host in text:
                text = self.word(host).sub("downloadclient.invalid", text)
        text = IPV4_RE.sub(lambda m: self.ip(m.group(0)), text)
        text = HASH64_RE.sub(lambda m: self.pseudonym(m.group(0), "hash64", 64), text)
        text = HASH40_RE.sub(lambda m: self.pseudonym(m.group(0), "hash"), text)
        text = NZO_RE.sub(lambda m: "SABnzbd_nzo_" + self.pseudonym(m.group(0), "nzo", 8), text)
        text = QUERY_APIKEY_RE.sub(lambda m: (self.remember(m.group(2), "api key"), m.group(1) + "0123456789abcdef0123456789abcdef")[1], text)
        text = B64_BLOB_RE.sub(lambda m: PLACEHOLDER_TORRENT_B64, text)
        if paths:
            text = WIN_PATH_RE.sub(lambda m: self.path(m.group(0), windows=True), text)
            text = POSIX_PATH_RE.sub(lambda m: self.path(m.group(0), windows=False), text)
        return text

    def ip(self, value: str) -> str:
        parts = value.split(".")
        if any(int(p) > 255 for p in parts) or value.startswith(("127.", "0.")):
            return value  # a version number, or already local
        self.remember(value, "address")
        return "127.0.0.1"

    def path(self, value: str, windows: bool) -> str:
        p = PureWindowsPath(value) if windows else PurePosixPath(value)
        parts = [x for x in p.parts if x not in ("/", p.anchor)]
        if not parts:
            return value
        if len(parts) >= 2 and "." in parts[-1] and not value.endswith(("/", "\\")):
            keep = parts[-2:]
        else:
            keep = parts[-1:]
        if keep == ["downloads"]:
            keep = []
        new = "/downloads" + "".join("/" + k for k in keep) + ("/" if value.endswith(("/", "\\")) or not keep else "")
        prefix = str(p)[: len(str(p)) - len("/".join(keep))].rstrip("/\\")
        if prefix and prefix not in ("/downloads",):
            self.remember(prefix, "path prefix")
        return new

    def scrub_json(self, v, key: str = ""):
        if isinstance(v, dict):
            # NZBGet's `config` and `loadconfig` answer with `{"Name": "Server1.Host",
            # "Value": "news.example.com"}` pairs: the name is the key that tells
            # whether the value is a secret.
            name = v.get("Name")
            if isinstance(name, str) and "Value" in v and len(v) == 2:
                # `Server1.Active` is not a server address: judge by the last
                # segment, and treat a news server's display name as a host.
                key = "host" if re.fullmatch(r"Server\d+\.Name", name) else name.rsplit(".", 1)[-1]
                return {"Name": name, "Value": self.scrub_json(v["Value"], key)}
            return {k: self.scrub_json(x, k) for k, x in v.items()}
        if isinstance(v, list):
            return [self.scrub_json(x, key) for x in v]
        # What says how the installation is reached from outside, replaced
        # by the client's defaults: the ports it listens on for peers and
        # the networks it lets in without a login.
        if key in INSTALLATION_DEFAULTS and not isinstance(v, bool):
            return INSTALLATION_DEFAULTS[key]
        if isinstance(v, str):
            if SECRET_JSON_KEY.search(key) and v:
                self.remember(v, key)
                # Most specific first: `Server1.Password` names a server too.
                if PASS_JSON_KEY.search(key):
                    return "fixture"
                if USER_JSON_KEY.search(key):
                    return "unarr"
                if HOST_JSON_KEY.search(key):
                    return "downloadclient.invalid"
                return "0123456789abcdef0123456789abcdef"
            return self.scrub_text(v)
        return v

    def scrub_body(self, body, content_type: str):
        if body is None:
            return None
        if isinstance(body, dict) and "base64" in body:
            # Binary upload or download: shape only.
            return {"base64": PLACEHOLDER_TORRENT_B64 if "torrent" in content_type else base64.b64encode(PLACEHOLDER_NZB.encode()).decode()}
        text = body
        if content_type.startswith("multipart/"):
            return self.scrub_multipart(text)
        if "json" in content_type or text.lstrip()[:1] in ("{", "["):
            try:
                return json.dumps(self.scrub_json(json.loads(text)), ensure_ascii=False)
            except json.JSONDecodeError:
                pass
        if content_type.startswith("application/x-www-form-urlencoded"):
            def form(m: re.Match) -> str:
                self.remember(urllib.parse.unquote_plus(m.group(2)), m.group(1))
                name = m.group(1).lower()
                value = "unarr" if "user" in name else "fixture" if "pass" in name else "0123456789abcdef0123456789abcdef"
                return f"{m.group(1)}={value}"

            text = FORM_SECRET_RE.sub(form, text)
        return self.scrub_text(text)

    def scrub_multipart(self, text: str) -> str:
        # Keep part headers; replace the content of file parts with a
        # placeholder of the same kind. NZB and torrent files carry server
        # names, poster addresses and tracker passkeys.
        out = []
        for part in text.split("\r\n--"):
            head, sep, content = part.partition("\r\n\r\n")
            if sep and "filename=" in head:
                lower = head.lower()
                placeholder = PLACEHOLDER_NZB if ".nzb" in lower or "nzb" in lower else base64.b64decode(PLACEHOLDER_TORRENT_B64).decode("latin-1")
                trailing = "\r\n" if content.endswith("\r\n") else ""
                part = head + sep + placeholder + trailing
            else:
                part = self.scrub_body(part, "text/plain") if sep else self.scrub_text(part)
            out.append(part)
        return "\r\n--".join(out)

    def scrub_headers(self, headers: dict, direction: str) -> dict:
        out = {}
        for k, v in headers.items():
            lk = k.lower()
            if lk == "host":
                self.hosts.add(v.split(":")[0])
                self.remember(v, "host header")
                v = "downloadclient.invalid"
            elif lk in ("authorization", "proxy-authorization"):
                scheme, _, cred = v.partition(" ")
                if scheme.lower() == "basic":
                    try:
                        decoded = base64.b64decode(cred).decode("utf-8", "replace")
                        for piece in decoded.split(":", 1):
                            self.remember(piece, "credential")
                    except (ValueError, UnicodeDecodeError):
                        pass
                    self.remember(cred, "credential")
                    v = "Basic " + base64.b64encode(b"unarr:fixture").decode()
                else:
                    self.remember(cred, "credential")
                    v = f"{scheme} 0123456789abcdef0123456789abcdef"
            elif lk in ("cookie", "set-cookie"):
                self.remember(v, "cookie")
                v = re.sub(r"(SID|session|token)=([^;]+)", r"\1=0123456789abcdef0123456789abcdef", v, flags=re.IGNORECASE)
            elif lk == "x-transmission-session-id":
                self.remember(v, "session id")
                v = "0123456789abcdef0123456789abcdef"
            elif lk in ("user-agent", "server", "date", "x-api-key", "x-forwarded-for", "x-real-ip", "referer", "origin"):
                if lk in ("x-api-key",):
                    self.remember(v, "api key")
                    v = "0123456789abcdef0123456789abcdef"
                elif lk in ("referer", "origin", "x-forwarded-for", "x-real-ip"):
                    v = self.scrub_text(v)
            else:
                v = self.scrub_text(v)
            out[k] = v
        return out

    def scrub_record(self, rec: dict) -> dict:
        req, resp = rec["request"], rec["response"]
        req_ct = next((v for k, v in req["headers"].items() if k.lower() == "content-type"), "")
        resp_ct = next((v for k, v in resp["headers"].items() if k.lower() == "content-type"), "")
        return {
            "seq": rec["seq"],
            "time": rec["time"],
            "request": {
                "method": req["method"],
                "path": self.scrub_text(req["path"], paths=False),
                "headers": self.scrub_headers(req["headers"], "request"),
                "body": self.scrub_body(req["body"], req_ct),
            },
            "response": {
                "status": resp["status"],
                "headers": self.scrub_headers(resp["headers"], "response"),
                "body": self.scrub_body(resp["body"], resp_ct),
            },
        }

    @staticmethod
    def word(value: str) -> re.Pattern:
        return re.compile(r"(?<![\w])" + re.escape(value) + r"(?![\w])")

    def redact_echoes(self, text: str) -> str:
        """A secret the client echoes somewhere unexpected (an error page, a
        log line) is replaced wherever it appears, once it is known."""
        for value, kind in sorted(self.originals.items(), key=lambda kv: -len(kv[0])):
            if len(value) >= 4 and kind not in ("path prefix",):
                text = self.word(value).sub("REDACTED", text)
        return text

    def survivors(self, text: str) -> list[str]:
        """Original values still present in the output, with their kind."""
        found = []
        for value, kind in self.originals.items():
            if len(value) < 3:
                continue
            if self.word(value).search(text):
                found.append(f"{kind}: {value[:40]!r}")
        return found


def cmd_anonymize(args) -> None:
    src = Path(args.input)
    lines = [json.loads(line) for line in src.read_text(encoding="utf-8").splitlines() if line.strip()]
    hosts = list(args.host or [])
    for meta in (x["meta"] for x in lines if "meta" in x):
        target = urllib.parse.urlparse(meta.get("target", ""))
        if target.hostname:
            hosts.append(target.hostname)
    records = [x for x in lines if "request" in x]
    if not records:
        sys.exit(f"{src}: no records")
    replacements = []
    for r in args.replace or []:
        old, sep, new = r.partition("=")
        if not sep or not old:
            sys.exit(f"--replace expects OLD=NEW, got {r!r}")
        replacements.append((old, new))
    anon = Anonymizer(args.client, hosts, replacements)
    # Two passes: the first learns every host header, the second rewrites with
    # the full host list, so a host that appears in a body before its first
    # Host header is caught too.
    for rec in records:
        anon.scrub_headers(rec["request"]["headers"], "request")
    out_records = [anon.scrub_record(rec) for rec in records]
    text = json.dumps(out_records, indent=1, sort_keys=True, ensure_ascii=False) + "\n"
    text = anon.redact_echoes(text)
    survivors = anon.survivors(text)
    if survivors:
        print("[capture] refusing to write: original values survive in the output:", file=sys.stderr)
        for s in survivors[:40]:
            print(f"  - {s}", file=sys.stderr)
        print("  add --replace OLD=NEW or --host NAME for each and run again", file=sys.stderr)
        sys.exit(1)
    out_dir = Path(args.out) / (args.name or args.client)
    out_dir.mkdir(parents=True, exist_ok=True)
    out_file = out_dir / "capture.json"
    out_file.write_text(text, encoding="utf-8")
    methods = sorted({(r["request"]["method"], r["request"]["path"].split("?")[0]) for r in out_records})
    log(f"{len(out_records)} exchanges written to {out_file}; {len(anon.originals)} original values removed")
    log("endpoints: " + ", ".join(f"{m} {p}" for m, p in methods))
    log("review the file before committing it: release names are kept on purpose")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("proxy", help="record exchanges by sitting between Sonarr and the download client")
    p.add_argument("--listen", default="0.0.0.0:18080", help="address:port to listen on (default 0.0.0.0:18080)")
    p.add_argument("--target", required=True, help="the download client, e.g. http://192.168.1.20:8080")
    p.add_argument("--out", required=True, help="capture file, one JSON exchange per line, appended")
    p.set_defaults(func=cmd_proxy)
    f = sub.add_parser("from-log", help="read the exchanges from Sonarr's trace log files instead of a proxy")
    f.add_argument("--client", required=True, choices=CLIENTS, help="which client's exchanges to keep")
    f.add_argument("--in", dest="input", action="append", required=True, metavar="FILE", help="a sonarr.trace*.txt file; repeatable")
    f.add_argument("--target", help="keep only URLs containing this, e.g. 192.168.1.20:6789, when several clients of the kind exist")
    f.add_argument("--out", required=True, help="capture file to write, one JSON exchange per line")
    f.set_defaults(func=cmd_from_log)
    a = sub.add_parser("anonymize", help="rewrite a capture for tests/fixtures/downloadclients/<client>/")
    a.add_argument("--client", required=True, choices=CLIENTS)
    a.add_argument("--in", dest="input", required=True, help="capture file from the proxy")
    a.add_argument("--out", default="tests/fixtures/downloadclients", help="fixture root (default tests/fixtures/downloadclients)")
    a.add_argument("--name", help="fixture directory under the root (default: the client), for a second client of the same kind")
    a.add_argument("--host", action="append", help="another host name or address to replace; repeatable")
    a.add_argument("--replace", action="append", metavar="OLD=NEW", help="literal replacement applied everywhere; repeatable")
    a.set_defaults(func=cmd_anonymize)
    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
