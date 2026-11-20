"""Probe the real Sonarr/Radarr, or Unarr, on a copy of the fixture, one config per scenario.

usage: [TARGET=original|unarr] [UNARRD=path] python3 -I scripts/http-probe.py sonarr|radarr OUT.json [scenario ...]

The probes behind the HTTP rows of `docs/audit/2-http.md` and their
triage (`docs/audit/7-triage.md`): forwarded headers and allowed hosts,
the URL base, the authentication methods and their log lines, and the
low rows (`lowhttp*`). The original is the release `make ui` unpacks
under `.cache/<app>/`; Unarr is `target/debug/unarrd` unless `UNARRD`
names another binary. Compare two outputs with `scripts/http-probe-diff.py`.

Each scenario edits config.xml, starts the original, sends its probes with
http.client (no redirect followed, Host and headers as given, from
127.0.0.1 or from 192.0.2.2), stops it, and keeps the log lines of its
`logs/<app>.txt` written during the probes. Writes one JSON with every
answer: status, headers, the first 300 bytes of the body.
"""
import datetime, http.client, json, os, re, shutil, signal, sqlite3, socket, subprocess, sys, time, urllib.parse
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
KEY = "0123456789abcdef0123456789abcdef"
TARGET = os.environ.get("TARGET", "original")
UNARRD = os.environ.get("UNARRD", str(REPO / "target/debug/unarrd"))
APP = sys.argv[1]
OUT = Path(sys.argv[2])
WANTED = sys.argv[3:]
PORT = {"sonarr": 18989, "radarr": 17878}[APP]
BIN = REPO / f".cache/{APP}/{APP.capitalize()}/{APP.capitalize()}"
FIX = REPO / f"tests/fixtures/{APP}"
WORK = Path(os.environ.get("PROBE_WORK", REPO / ".cache/http-probe")) / f"{TARGET}-{APP}"
OTHER = "192.0.2.2"
# The hashed names of the bundle `make ui` unpacked, which differ between the two apps.
_UI = REPO / f"crates/unarr-api/ui/{APP}"
INDEX_JS = "/" + next(p.name for p in _UI.glob("index-*.js"))
CSS = "/Content/" + next(p.name for p in (_UI / "Content").glob("*-*.css"))
LICENCE = "/" + next(p.name for p in _UI.glob("*.LICENSE.txt"))


def setconf(cfg: Path, values: dict):
    text = cfg.read_text()
    for k, v in values.items():
        if re.search(f"<{k}>.*?</{k}>", text, re.S):
            text = re.sub(f"<{k}>.*?</{k}>", f"<{k}>{v}</{k}>", text, flags=re.S)
        else:
            text = text.replace("</Config>", f"  <{k}>{v}</{k}>\n</Config>")
    cfg.write_text(text)


def start(conf: dict):
    data = WORK / "data"
    shutil.rmtree(data, ignore_errors=True)
    data.mkdir(parents=True)
    src = WORK / "src" if TARGET == "unarr" else data
    if TARGET == "unarr":
        shutil.rmtree(src, ignore_errors=True); src.mkdir(parents=True)
    shutil.copy(FIX / f"{APP}.db", src / f"{APP}.db")
    shutil.copy(FIX / "config.xml", src / "config.xml")
    setconf(src / "config.xml", {"Port": PORT, "LogLevel": "trace", **conf})
    c = sqlite3.connect(src / f"{APP}.db")
    c.execute("update ScheduledTasks set LastExecution=?", (datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d %H:%M:%SZ"),))
    c.commit(); c.close()
    env = {k: v for k, v in os.environ.items() if k.lower() not in ("https_proxy", "http_proxy", "all_proxy") and not k.startswith("UNARR_") and k != "RUST_LOG"}
    try:
        libs = subprocess.run(["ldconfig", "-p"], capture_output=True, text=True).stdout
    except FileNotFoundError:  # no ldconfig (macOS): ICU is not looked for
        libs = ""
    if sys.platform.startswith("linux") and "libicuuc" not in libs:
        env["DOTNET_SYSTEM_GLOBALIZATION_INVARIANT"] = "1"
    log = open(data.parent / f"stdout-{APP}.log", "wb")
    if TARGET == "unarr":
        cmd = [UNARRD, "--data", str(data), "--personalities", APP, "--import-from", str(src)]
    else:
        cmd = [str(BIN), "-nobrowser", f"-data={data}"]
    p = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, env=env)
    base = conf.get("UrlBase", "").rstrip("/")
    for _ in range(180):
        try:
            s, _, _ = req("GET", f"{base}/ping")
            if s == 200:
                break
        except OSError:
            pass
        if p.poll() is not None:
            sys.exit(f"{APP} exited, see {data.parent}/stdout-{APP}.log")
        time.sleep(1)
    else:
        sys.exit("no ping")
    time.sleep(3)
    covers = (data / APP / "MediaCover" / "1") if TARGET == "unarr" else (data / "MediaCover" / "1")
    covers.mkdir(parents=True, exist_ok=True)
    (covers / "poster.jpg").write_bytes(b"\xff\xd8\xff\xe0probe-cover")
    return p, data


def stop(p):
    p.send_signal(signal.SIGTERM)
    try:
        p.wait(60)
    except subprocess.TimeoutExpired:
        p.kill(); p.wait()


def req(method, path, headers=None, body=None, source="127.0.0.1", host=None):
    conn = http.client.HTTPConnection("127.0.0.1" if source == "127.0.0.1" else OTHER, PORT, timeout=30,
                                      source_address=(source, 0))
    conn.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
    h = {"Host": host if host is not None else f"127.0.0.1:{PORT}", **(headers or {})}
    for k, v in h.items():
        if v is not None:
            conn.putheader(k, v)
    if body is not None:
        conn.putheader("Content-Length", str(len(body)))
    conn.endheaders(body)
    r = conn.getresponse()
    raw = r.read()
    out = (r.status, {k.lower(): v for k, v in r.getheaders()}, raw)
    conn.close()
    return out


def login(base, user="unarr", password="fixture", source="127.0.0.1", headers=None):
    form = urllib.parse.urlencode({"username": user, "password": password, "rememberMe": "on"}).encode()
    return req("POST", f"{base}/login", {"Content-Type": "application/x-www-form-urlencoded", **(headers or {})}, form, source)


def basic(user, pw):
    import base64
    return "Basic " + base64.b64encode(f"{user}:{pw}".encode()).decode()


K = {"X-Api-Key": KEY}
SCEN = {
    # 1.3: X-Forwarded-Host before the host filter.
    "hosts": ({"AllowedHosts": "tv.example.com"}, lambda b: [
        ("host-listed", "GET", "/api/v3/system/status", K, None, "127.0.0.1", "tv.example.com"),
        ("host-not-listed", "GET", "/api/v3/system/status", K, None, "127.0.0.1", "unarr:8989"),
        ("fwd-host-listed-from-loopback", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": "tv.example.com"}, None, "127.0.0.1", "unarr:8989"),
        ("fwd-host-listed-port-from-loopback", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": "tv.example.com:443"}, None, "127.0.0.1", "unarr:8989"),
        ("fwd-host-not-listed-from-loopback", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": "evil.example"}, None, "127.0.0.1", "tv.example.com"),
        ("fwd-host-two-last-listed", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": "evil.example, tv.example.com"}, None, "127.0.0.1", "unarr:8989"),
        ("fwd-host-two-first-listed", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": "tv.example.com, evil.example"}, None, "127.0.0.1", "unarr:8989"),
        ("fwd-host-with-fwd-for", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": "tv.example.com", "X-Forwarded-For": "10.1.2.3"}, None, "127.0.0.1", "unarr:8989"),
        ("fwd-host-two-with-two-fwd-for", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": "evil.example, tv.example.com", "X-Forwarded-For": "10.1.2.3, 127.0.0.1"}, None, "127.0.0.1", "unarr:8989"),
        ("fwd-host-listed-from-other", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": "tv.example.com"}, None, OTHER, "unarr:8989"),
        ("fwd-host-not-listed-from-other", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": "evil.example"}, None, OTHER, "tv.example.com"),
        ("ping-host-not-listed", "GET", "/ping", {}, None, "127.0.0.1", "unarr:8989"),
        ("ping-fwd-host-listed", "GET", "/ping", {"X-Forwarded-Host": "tv.example.com"}, None, "127.0.0.1", "unarr:8989"),
        ("page-fwd-host-listed", "GET", "/", {"X-Forwarded-Host": "tv.example.com", "X-Forwarded-Proto": "https"}, None, "127.0.0.1", "unarr:8989"),
        ("empty-fwd-host", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Host": ""}, None, "127.0.0.1", "unarr:8989"),
        ("no-host-header", "GET", "/api/v3/system/status", {**K}, None, "127.0.0.1", None),
    ]),
    # 1.8 under Forms: the log lines of a login, a failure, a logout.
    "forms-log": ({}, None),
    # 1.8 under Basic.
    "basic-log": ({"AuthenticationMethod": "Basic"}, lambda b: [
        ("basic-wrong", "GET", "/", {"Authorization": basic("unarr", "nope")}, None, "127.0.0.1", None),
        ("basic-unknown-user", "GET", "/", {"Authorization": basic("nobody", "nope")}, None, "127.0.0.1", None),
        ("basic-right", "GET", "/", {"Authorization": basic("unarr", "fixture")}, None, "127.0.0.1", None),
        ("basic-right-again", "GET", "/initialize.json", {"Authorization": basic("unarr", "fixture")}, None, "127.0.0.1", None),
        ("basic-none", "GET", "/", {}, None, "127.0.0.1", None),
        ("api-wrong-key", "GET", "/api/v3/system/status", {"X-Api-Key": "nope"}, None, "127.0.0.1", None),
        ("basic-wrong-from-other", "GET", "/", {"Authorization": basic("unarr", "nope"), "X-Forwarded-For": "10.9.8.7"}, None, OTHER, None),
        ("basic-wrong-from-loopback-fwd", "GET", "/", {"Authorization": basic("unarr", "nope"), "X-Forwarded-For": "10.9.8.7"}, None, "127.0.0.1", None),
    ]),
    # 1.10 and 1.11: a URL base.
    "urlbase": ({"UrlBase": "/tv"}, lambda b: [
        ("root-noauth", "GET", "/", {}, None, "127.0.0.1", None),
        ("page-noauth", "GET", "/series?x=1", {}, None, "127.0.0.1", None),
        ("ping-outside", "GET", "/ping", {}, None, "127.0.0.1", None),
        ("ping-inside", "GET", "/tv/ping", {}, None, "127.0.0.1", None),
        ("api-outside-key", "GET", "/api/v3/system/status", K, None, "127.0.0.1", None),
        ("api-outside-nokey", "GET", "/api/v3/system/status", {}, None, "127.0.0.1", None),
        ("api-outside-key-query", "GET", f"/api/v3/system/status?apikey={KEY}&x=1", {}, None, "127.0.0.1", None),
        ("api-post-outside-key", "POST", "/api/v3/command", {**K, "Content-Type": "application/json"}, b'{"name":"RssSync"}', "127.0.0.1", None),
        ("base-no-slash", "GET", "/tv", {}, None, "127.0.0.1", None),
        ("base-slash", "GET", "/tv/", {}, None, "127.0.0.1", None),
        ("base-upper", "GET", "/TV/ping", {}, None, "127.0.0.1", None),
        ("base-upper-api", "GET", "/TV/api/v3/system/status", K, None, "127.0.0.1", None),
        ("base-prefix-not-segment", "GET", "/tvx/ping", {}, None, "127.0.0.1", None),
        ("login-outside", "GET", "/login", {}, None, "127.0.0.1", None),
        ("content-outside", "GET", "/Content/manifest.json", {}, None, "127.0.0.1", None),
        ("initialize-outside", "GET", "/initialize.json", {}, None, "127.0.0.1", None),
        ("feed-outside-key", "GET", f"/feed/v3/calendar/{APP.capitalize()}.ics?apikey={KEY}", {}, None, "127.0.0.1", None),
        ("signalr-outside", "POST", f"/signalr/messages/negotiate?access_token={KEY}", {}, b"", "127.0.0.1", None),
        ("api-outside-fwd-proto", "GET", "/api/v3/system/status", {**K, "X-Forwarded-Proto": "https", "X-Forwarded-Host": "tv.example.com"}, None, "127.0.0.1", None),
        ("api-outside-encoded", "GET", "/api/v3/system/status?q=a%20b", K, None, "127.0.0.1", None),
        ("head-outside", "HEAD", "/ping", {}, None, "127.0.0.1", None),
        ("mediacover-outside-key", "GET", f"/MediaCover/1/poster.jpg?apikey={KEY}", {}, None, "127.0.0.1", None),
    ]),
    # 1.24: paths under /api that are not v3, Forms.
    "apipaths": ({}, lambda b: [
        ("api-root-key", "GET", "/api", K, None, "127.0.0.1", None),
        ("api-root-nokey", "GET", "/api", {}, None, "127.0.0.1", None),
        ("api-slash-key", "GET", "/api/", K, None, "127.0.0.1", None),
        ("api-v1-key", "GET", "/api/v1/foo", K, None, "127.0.0.1", None),
        ("api-v1-nokey", "GET", "/api/v1/foo", {}, None, "127.0.0.1", None),
        ("api-v1-status-key", "GET", "/api/v1/system/status", K, None, "127.0.0.1", None),
        ("api-v2-key", "GET", "/api/v2/system/status", K, None, "127.0.0.1", None),
        ("api-foo-key", "GET", "/api/foo", K, None, "127.0.0.1", None),
        ("api-foo-nokey", "GET", "/api/foo", {}, None, "127.0.0.1", None),
        ("api-foo-wrongkey", "GET", "/api/foo", {"X-Api-Key": "nope"}, None, "127.0.0.1", None),
        ("api-foo-post-key", "POST", "/api/foo", {**K, "Content-Type": "application/json"}, b"{}", "127.0.0.1", None),
        ("api-foo-cookie", "GET", "/api/foo", "COOKIE", None, "127.0.0.1", None),
        ("api-v3-unknown-cookie", "GET", "/api/v3/nosuchthing", "COOKIE", None, "127.0.0.1", None),
        ("api-v1-cookie", "GET", "/api/v1/foo", "COOKIE", None, "127.0.0.1", None),
        ("api-upper-key", "GET", "/API/v3/system/status", K, None, "127.0.0.1", None),
        ("api-V1-key", "GET", "/API/V1/foo", K, None, "127.0.0.1", None),
        ("apix-key", "GET", "/apix/foo", K, None, "127.0.0.1", None),
        ("apix-nokey", "GET", "/apix/foo", {}, None, "127.0.0.1", None),
        ("api-v1-key-query", "GET", f"/api/v1/foo?apikey={KEY}", {}, None, "127.0.0.1", None),
        ("api-v1-accept-json-nokey", "GET", "/api/v1/foo", {"Accept": "application/json"}, None, "127.0.0.1", None),
    ]),
    # 1.24: methods on paths under /api that no action takes.
    "apimethods": ({}, lambda b: [
        (f"{m.lower()}-{n}", m, path, ({**K, "Content-Type": "application/json"} if m in ("POST", "PUT") else K), (b"{}" if m in ("POST", "PUT") else None), "127.0.0.1", None)
        for n, path in (("v1", "/api/v1/foo"), ("v3-unknown", "/api/v3/nosuchthing"), ("v3-status", "/api/v3/system/status"), ("api-root", "/api"), ("page", "/foo"))
        for m in ("GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS")
    ] + [
        ("post-v1-nokey", "POST", "/api/v1/foo", {"Content-Type": "application/json"}, b"{}", "127.0.0.1", None),
        ("api-root-upper-key", "GET", "/API", K, None, "127.0.0.1", None),
        ("api-v1-trailing", "GET", "/api/v1/foo/", K, None, "127.0.0.1", None),
        ("api-v3-bad-host-400body", "GET", "/api/v3/system/status", K, None, "127.0.0.1", None),
    ]),
    # The low rows of 2-http.md, default configuration (Forms).
    "lowhttp": ({}, lambda b: [
        ("1.15-login-upper", "GET", "/LOGIN", {}, None, "127.0.0.1", None),
        ("1.15-ping-mixed", "GET", "/Ping", {}, None, "127.0.0.1", None),
        ("1.15-initialize-mixed", "GET", "/Initialize.json", {}, None, "127.0.0.1", None),
        ("1.15-signalr-upper", "POST", f"/SIGNALR/messages/negotiate?access_token={KEY}", {}, b"", "127.0.0.1", None),
        ("1.15-logout-upper", "GET", "/LOGOUT", {}, None, "127.0.0.1", None),
        ("1.15-feed-upper", "GET", f"/FEED/v3/calendar/{APP.capitalize()}.ics?apikey={KEY}", {}, None, "127.0.0.1", None),
        ("1.18-content-origin", "GET", "/Content/manifest.json", {"Origin": "http://other.example"}, None, "127.0.0.1", None),
        ("1.18-content-preflight", "OPTIONS", "/Content/manifest.json", {"Origin": "http://other.example", "Access-Control-Request-Method": "GET"}, None, "127.0.0.1", None),
        ("1.19-br", "GET", "/api/v3/system/status", {**K, "Accept-Encoding": "br"}, None, "127.0.0.1", None),
        ("1.19-gzip-br", "GET", "/api/v3/system/status", {**K, "Accept-Encoding": "gzip, br"}, None, "127.0.0.1", None),
        ("1.19-none", "GET", "/api/v3/system/status", K, None, "127.0.0.1", None),
        ("1.19-content-gzip", "GET", "/Content/styles.css", {"Accept-Encoding": "gzip"}, None, "127.0.0.1", None),
        ("1.30-accept-html", "GET", "/api/v3/system/status", {**K, "Accept": "text/html"}, None, "127.0.0.1", None),
        ("1.30-accept-xml", "GET", "/api/v3/system/status", {**K, "Accept": "application/xml"}, None, "127.0.0.1", None),
        ("1.30-accept-any", "GET", "/api/v3/system/status", {**K, "Accept": "*/*"}, None, "127.0.0.1", None),
        ("1.32-head-content", "HEAD", "/Content/manifest.json", {}, None, "127.0.0.1", None),
        ("1.32-head-content-css", "HEAD", "/Content/styles.css", {}, None, "127.0.0.1", None),
        ("1.32-head-login", "HEAD", "/login", {}, None, "127.0.0.1", None),
        ("1.32-head-bundle", "HEAD", INDEX_JS, {}, None, "127.0.0.1", None),
        ("3.9-xrw", "GET", "/", {"X-Requested-With": "XMLHttpRequest"}, None, "127.0.0.1", None),
        ("3.9-xrw-page", "GET", "/series", {"X-Requested-With": "XMLHttpRequest"}, None, "127.0.0.1", None),
        ("4.3-index", "GET", "/", "COOKIE", None, "127.0.0.1", None),
        ("4.3-login-page", "GET", "/login", {}, None, "127.0.0.1", None),
        ("4.9-fonts-css", "GET", "/Content/Fonts/fonts.css", {}, None, "127.0.0.1", None),
        ("4.9-styles", "GET", "/Content/styles.css", {}, None, "127.0.0.1", None),
        ("4.9-icon", "GET", "/Content/Images/Icons/favicon-32x32.png", {}, None, "127.0.0.1", None),
        ("4.9-svg", "GET", "/Content/Images/Icons/safari-pinned-tab.svg", {}, None, "127.0.0.1", None),
        ("4.9-bundle-js", "GET", INDEX_JS, "COOKIE", None, "127.0.0.1", None),
        ("4.9-bundle-css", "GET", CSS, {}, None, "127.0.0.1", None),
        ("4.9-bundle-js-nocookie", "GET", INDEX_JS, {}, None, "127.0.0.1", None),
        ("4.7-robots", "GET", "/robots.txt", {}, None, "127.0.0.1", None),
        ("4.7-robots-content", "GET", "/Content/robots.txt", {}, None, "127.0.0.1", None),
        ("4.16-index-html", "GET", "/index.html", "COOKIE", None, "127.0.0.1", None),
        ("4.16-licence", "GET", LICENCE, "COOKIE", None, "127.0.0.1", None),
        ("4.16-version", "GET", "/VERSION", "COOKIE", None, "127.0.0.1", None),
        ("4.16-login-html", "GET", "/login.html", {}, None, "127.0.0.1", None),
        ("4.16-oauth-html", "GET", "/oauth.html", "COOKIE", None, "127.0.0.1", None),
        ("4.19-font", "GET", "/Content/Fonts/fonts.css", {}, None, "127.0.0.1", None),
        ("4.20-missing-png", "GET", "/Content/nope.png", {}, None, "127.0.0.1", None),
        ("4.20-missing-root-png", "GET", "/nope.png", "COOKIE", None, "127.0.0.1", None),
        ("4.20-missing-js", "GET", "/nope.js", "COOKIE", None, "127.0.0.1", None),
        ("2.12-negotiate-v1", "POST", f"/signalr/messages/negotiate?negotiateVersion=1&access_token={KEY}", {}, b"", "127.0.0.1", None),
        ("2.12-negotiate-v0", "POST", f"/signalr/messages/negotiate?access_token={KEY}", {}, b"", "127.0.0.1", None),
        ("1.7-marker", "GET", "/api/v3/system/status?marker=unarrprobe", K, None, "127.0.0.1", None),
    ]),
    "lowhttp-none": ({"AuthenticationMethod": "None"}, lambda b: [
        ("3.8-login-get", "GET", "/login", {}, None, "127.0.0.1", None),
        ("3.8-index", "GET", "/", {}, None, "127.0.0.1", None),
    ]),
    "lowhttp-unicode": ({"InstanceName": f"{APP.capitalize()} Ünï Çafé"}, lambda b: [
        ("3.5-login-page", "GET", "/login", {}, None, "127.0.0.1", None),
    ]),
    # Second pass on the low rows: what the first left open.
    "lowhttp2": ({}, lambda b: [
        ("1.32-head-content-cookie", "HEAD", "/Content/manifest.json", "COOKIE", None, "127.0.0.1", None),
        ("1.32-head-login-cookie", "HEAD", "/login", "COOKIE", None, "127.0.0.1", None),
        ("1.32-head-index-cookie", "HEAD", "/", "COOKIE", None, "127.0.0.1", None),
        ("1.32-head-content-key", "HEAD", "/Content/manifest.json", K, None, "127.0.0.1", None),
        ("1.32-head-api", "HEAD", "/api/v3/system/status", K, None, "127.0.0.1", None),
        ("1.32-head-mediacover", "HEAD", "/MediaCover/1/poster.jpg", "COOKIE", None, "127.0.0.1", None),
        ("1.18-content-preflight-post", "OPTIONS", "/Content/manifest.json", {"Origin": "http://other.example", "Access-Control-Request-Method": "POST"}, None, "127.0.0.1", None),
        ("1.18-content-preflight-headers", "OPTIONS", "/Content/manifest.json", {"Origin": "http://other.example", "Access-Control-Request-Method": "GET", "Access-Control-Request-Headers": "X-Custom, Content-Type"}, None, "127.0.0.1", None),
        ("1.18-content-preflight-upper", "OPTIONS", "/CONTENT/manifest.json", {"Origin": "http://other.example", "Access-Control-Request-Method": "GET"}, None, "127.0.0.1", None),
        ("1.18-content-noorigin", "GET", "/Content/manifest.json", {}, None, "127.0.0.1", None),
        ("1.18-content-options-plain", "OPTIONS", "/Content/manifest.json", {}, None, "127.0.0.1", None),
        ("1.18-content-missing-origin", "GET", "/Content/nope.png", {"Origin": "http://other.example"}, None, "127.0.0.1", None),
        ("1.18-index-preflight", "OPTIONS", "/", {"Origin": "http://other.example", "Access-Control-Request-Method": "GET"}, None, "127.0.0.1", None),
        ("1.18-api-origin", "GET", "/api/v3/system/status", {**K, "Origin": "http://other.example"}, None, "127.0.0.1", None),
        ("1.30-accept-text-plain", "GET", "/api/v3/system/status", {**K, "Accept": "text/plain"}, None, "127.0.0.1", None),
        ("1.30-accept-app-star", "GET", "/api/v3/system/status", {**K, "Accept": "application/*"}, None, "127.0.0.1", None),
        ("1.30-accept-text-star", "GET", "/api/v3/system/status", {**K, "Accept": "text/*"}, None, "127.0.0.1", None),
        ("1.30-accept-html-star", "GET", "/api/v3/system/status", {**K, "Accept": "text/html, */*;q=0.1"}, None, "127.0.0.1", None),
        ("1.30-accept-textjson", "GET", "/api/v3/system/status", {**K, "Accept": "text/json"}, None, "127.0.0.1", None),
        ("1.30-accept-problem", "GET", "/api/v3/system/status", {**K, "Accept": "application/problem+json"}, None, "127.0.0.1", None),
        ("1.30-accept-empty", "GET", "/api/v3/system/status", {**K, "Accept": ""}, None, "127.0.0.1", None),
        ("1.30-ping-html", "GET", "/ping", {"Accept": "text/html"}, None, "127.0.0.1", None),
        ("1.30-html-404", "GET", "/api/v3/series/999999", {**K, "Accept": "text/html"}, None, "127.0.0.1", None),
        ("1.30-html-400", "POST", "/api/v3/tag", {**K, "Accept": "text/html", "Content-Type": "application/json"}, b"{}", "127.0.0.1", None),
        ("1.30-html-post", "POST", "/api/v3/tag", {**K, "Accept": "text/html", "Content-Type": "application/json"}, b'{"label": "probe406"}', "127.0.0.1", None),
        ("1.30-after-post", "GET", "/api/v3/tag", K, None, "127.0.0.1", None),
        ("1.30-html-list", "GET", "/api/v3/tag", {**K, "Accept": "text/html"}, None, "127.0.0.1", None),
        ("1.30-html-delete", "DELETE", "/api/v3/tag/1", {**K, "Accept": "text/html"}, None, "127.0.0.1", None),
        ("1.30-html-initialize", "GET", "/initialize.json", "COOKIE", None, "127.0.0.1", None),
        ("3.9-xrw-query", "GET", "/?X-Requested-With=XMLHttpRequest", {}, None, "127.0.0.1", None),
        ("3.9-xrw-lower", "GET", "/", {"X-Requested-With": "xmlhttprequest"}, None, "127.0.0.1", None),
        ("3.9-xrw-content", "GET", "/Content/nope.css", {"X-Requested-With": "XMLHttpRequest"}, None, "127.0.0.1", None),
        ("1.15-logout-lower", "GET", "/logout", {}, None, "127.0.0.1", None),
        ("1.15-ping-lower", "GET", "/ping", {}, None, "127.0.0.1", None),
        ("1.15-login-post-upper", "POST", "/LOGIN", {"Content-Type": "application/x-www-form-urlencoded"}, b"username=unarr&password=fixture&rememberMe=on", "127.0.0.1", None),
        ("1.15-content-upper", "GET", "/CONTENT/manifest.json", {}, None, "127.0.0.1", None),
        ("1.15-content-upper-cookie", "GET", "/CONTENT/manifest.json", "COOKIE", None, "127.0.0.1", None),
        ("1.15-initialize-js-mixed", "GET", "/Initialize.js", "COOKIE", None, "127.0.0.1", None),
        ("1.15-feed-mixed-noauth", "GET", f"/Feed/v3/calendar/{APP.capitalize()}.ics", {}, None, "127.0.0.1", None),
        ("1.15-signalr-mixed-nokey", "POST", "/SignalR/messages/negotiate", {}, b"", "127.0.0.1", None),
        ("4.11-mediacover", "GET", "/MediaCover/1/poster.jpg", "COOKIE", None, "127.0.0.1", None),
        ("4.11-mediacover-lastwrite", "GET", "/MediaCover/1/poster.jpg?lastWrite=638000000000000000", "COOKIE", None, "127.0.0.1", None),
        ("4.11-mediacover-api", "GET", "/api/v3/mediacover/1/poster.jpg", K, None, "127.0.0.1", None),
        ("4.11-mediacover-missing", "GET", "/MediaCover/1/fanart.jpg", "COOKIE", None, "127.0.0.1", None),
        ("4.9-content-h", "GET", "/Content/styles.css?h=abc", {}, None, "127.0.0.1", None),
        ("4.9-api-h", "GET", "/api/v3/system/status?h=1", K, None, "127.0.0.1", None),
        ("4.9-login-h", "GET", "/login?h=1", {}, None, "127.0.0.1", None),
        ("1.19-br-content", "GET", "/Content/styles.css", {"Accept-Encoding": "br"}, None, "127.0.0.1", None),
        ("1.19-q", "GET", "/api/v3/system/status", {**K, "Accept-Encoding": "br;q=0.5, gzip"}, None, "127.0.0.1", None),
        ("1.19-identity", "GET", "/api/v3/system/status", {**K, "Accept-Encoding": "identity"}, None, "127.0.0.1", None),
        ("1.19-deflate", "GET", "/api/v3/system/status", {**K, "Accept-Encoding": "deflate"}, None, "127.0.0.1", None),
        ("1.19-png", "GET", "/Content/Images/Icons/favicon-32x32.png", {"Accept-Encoding": "gzip, br"}, None, "127.0.0.1", None),
        ("4.20-missing-content-js", "GET", "/Content/nope.js", "COOKIE", None, "127.0.0.1", None),
        ("4.20-missing-dir", "GET", "/nope/x.png", "COOKIE", None, "127.0.0.1", None),
        ("4.20-missing-noauth", "GET", "/nope.png", {}, None, "127.0.0.1", None),
        ("4.16-login-html-cookie", "GET", "/login.html", "COOKIE", None, "127.0.0.1", None),
        ("4.16-content-index-html", "GET", "/Content/index.html", "COOKIE", None, "127.0.0.1", None),
        ("4.19-woff2", "GET", "/Content/Fonts/Roboto-Light.woff2", {}, None, "127.0.0.1", None),
        ("4.19-ico", "GET", "/favicon.ico", "COOKIE", None, "127.0.0.1", None),
        ("4.19-xml", "GET", "/Content/browserconfig.xml", {}, None, "127.0.0.1", None),
        ("4.19-webmanifest", "GET", "/Content/Images/Icons/manifest.json", {}, None, "127.0.0.1", None),
    ]),
    # Third pass: types, request log, downloads, the cover proxy.
    "lowhttp3": ({}, lambda b: [
        ("4.19-ttf", "GET", "/Content/Fonts/Roboto-Light.ttf", {}, None, "127.0.0.1", None),
        ("4.19-woff", "GET", "/Content/Fonts/Roboto-Light.woff", {}, None, "127.0.0.1", None),
        ("4.19-eot", "GET", "/Content/Fonts/UbuntuMono-Regular.eot", {}, None, "127.0.0.1", None),
        ("4.19-licence", "GET", LICENCE, "COOKIE", None, "127.0.0.1", None),
        ("4.19-robots", "GET", "/Content/robots.txt", {}, None, "127.0.0.1", None),
        ("4.19-manifest", "GET", "/Content/manifest.json", {}, None, "127.0.0.1", None),
        ("4.19-index-js", "GET", INDEX_JS, "COOKIE", None, "127.0.0.1", None),
        ("4.19-png", "GET", "/Content/Images/Icons/favicon-32x32.png", {}, None, "127.0.0.1", None),
        ("4.9-favicon", "GET", "/favicon.ico", "COOKIE", None, "127.0.0.1", None),
        ("4.9-oauth", "GET", "/oauth.html", "COOKIE", None, "127.0.0.1", None),
        ("4.9-initialize-js", "GET", "/initialize.js", "COOKIE", None, "127.0.0.1", None),
        ("4.9-login-fail", "POST", "/login", {"Content-Type": "application/x-www-form-urlencoded"}, b"username=unarr&password=nope", "127.0.0.1", None),
        ("4.9-feed", "GET", f"/feed/v3/calendar/{APP.capitalize()}.ics?apikey={KEY}", {}, None, "127.0.0.1", None),
        ("4.9-signalr", "POST", f"/signalr/messages/negotiate?access_token={KEY}", {}, b"", "127.0.0.1", None),
        ("2.8-backup-missing", "GET", "/backup/manual/nope.zip", "COOKIE", None, "127.0.0.1", None),
        ("2.9-proxy-bad-hash", "GET", "/MediaCoverProxy/0123456789abcdef/poster.jpg", "COOKIE", None, "127.0.0.1", None),
        ("2.9-proxy-bad-name", "GET", "/MediaCoverProxy/0123456789abcdef/poster.txt", "COOKIE", None, "127.0.0.1", None),
        ("1.7-marker-query", "GET", f"/api/v3/system/status?marker=unarrprobe&apikey={KEY}", {"User-Agent": "unarrprobe-agent/1.0"}, None, "127.0.0.1", None),
        ("1.7-marker-page", "GET", "/unarrprobe-page", {}, None, "127.0.0.1", None),
        ("1.7-marker-404", "GET", "/api/v3/series/999999?marker=unarrprobe", K, None, "127.0.0.1", None),
    ]),
    "lowhttp2-none": ({"AuthenticationMethod": "None"}, lambda b: [
        ("1.32-head-content", "HEAD", "/Content/manifest.json", {}, None, "127.0.0.1", None),
        ("1.32-head-login", "HEAD", "/login", {}, None, "127.0.0.1", None),
        ("1.32-head-index", "HEAD", "/", {}, None, "127.0.0.1", None),
        ("1.32-head-mediacover", "HEAD", "/MediaCover/1/poster.jpg", {}, None, "127.0.0.1", None),
        ("1.18-content-options-plain", "OPTIONS", "/Content/manifest.json", {}, None, "127.0.0.1", None),
        ("4.7-robots", "GET", "/robots.txt", {}, None, "127.0.0.1", None),
        ("3.8-login-post", "POST", "/login", {"Content-Type": "application/x-www-form-urlencoded"}, b"username=unarr&password=fixture&rememberMe=on", "127.0.0.1", None),
        ("3.8-login-post-wrong", "POST", "/login", {"Content-Type": "application/x-www-form-urlencoded"}, b"username=unarr&password=nope", "127.0.0.1", None),
        ("3.8-logout", "GET", "/logout", {}, None, "127.0.0.1", None),
        ("4.20-missing", "GET", "/nope.png", {}, None, "127.0.0.1", None),
        ("3.9-xrw", "GET", "/", {"X-Requested-With": "XMLHttpRequest"}, None, "127.0.0.1", None),
    ]),
    "lowhttp2-unicode": ({"InstanceName": f"{APP.capitalize()} Ünï Çafé"}, lambda b: [
        ("3.5-login-post", "POST", "/login", {"Content-Type": "application/x-www-form-urlencoded"}, b"username=unarr&password=fixture&rememberMe=on", "127.0.0.1", None),
    ]),
    # The realm, the returnUrl under a URL base, SignalR without a token outside it.
    "extra": ({"UrlBase": "/tv", "InstanceName": "Sonarr 4K"}, lambda b: [
        ("page-inside", "GET", "/tv/series?x=1", {}, None, "127.0.0.1", None),
        ("page-inside-upper", "GET", "/TV/series", {}, None, "127.0.0.1", None),
        ("signalr-outside-nokey", "POST", "/signalr/messages/negotiate", {}, b"", "127.0.0.1", None),
        ("signalr-inside-nokey", "POST", "/tv/signalr/messages/negotiate", {}, b"", "127.0.0.1", None),
        ("logout-outside", "GET", "/logout", {}, None, "127.0.0.1", None),
        ("favicon-outside", "GET", "/favicon.ico", {}, None, "127.0.0.1", None),
        ("api-root-outside-key", "GET", "/api", K, None, "127.0.0.1", None),
        ("api-v1-outside-key", "GET", "/api/v1/foo", K, None, "127.0.0.1", None),
        ("post-page-outside", "POST", "/foo", {}, b"", "127.0.0.1", None),
        ("page-outside-cookie", "GET", "/series", "COOKIE", None, "127.0.0.1", None),
        ("login-outside-query", "GET", "/login?returnUrl=%2F", {}, None, "127.0.0.1", None),
    ]),
    "pagemethods": ({}, lambda b: [
        ("post-page-cookie", "POST", "/foo", "COOKIE", b"", "127.0.0.1", None),
        ("post-page-nothing", "POST", "/foo", {}, b"", "127.0.0.1", None),
        ("get-page-key", "GET", "/foo", K, None, "127.0.0.1", None),
        ("post-root-nothing", "POST", "/", {}, b"", "127.0.0.1", None),
    ]),
    "pagemethods-base": ({"UrlBase": "/tv"}, lambda b: [
        ("post-page-outside-cookie", "POST", "/foo", "COOKIE", b"", "127.0.0.1", None),
        ("post-page-outside-key", "POST", "/foo", K, b"", "127.0.0.1", None),
        ("get-page-outside-key", "GET", "/foo", K, None, "127.0.0.1", None),
        ("post-ping-outside", "POST", "/ping", {}, b"", "127.0.0.1", None),
        ("post-page-inside-nothing", "POST", "/tv/foo", {}, b"", "127.0.0.1", None),
        ("api-outside-cookie", "GET", "/api/v3/system/status", "COOKIE", None, "127.0.0.1", None),
        ("api-outside-wrongkey", "GET", "/api/v3/system/status", {"X-Api-Key": "nope"}, None, "127.0.0.1", None),
        ("api-outside-bearer", "GET", "/api/v3/system/status", {"Authorization": "Bearer " + KEY}, None, "127.0.0.1", None),
    ]),
    "realm": ({"AuthenticationMethod": "Basic", "InstanceName": "Sonarr 4K"}, lambda b: [
        ("basic-none", "GET", "/", {}, None, "127.0.0.1", None),
    ]),
    # Radarr: what Basic becomes.
    "basic-host": ({"AuthenticationMethod": "Basic"}, lambda b: [
        ("config-host", "GET", "/api/v3/config/host", K, None, "127.0.0.1", None),
        ("login-page", "GET", "/login", {}, None, "127.0.0.1", None),
    ]),
    # 1.24 under no authentication for local addresses.
    "apipaths-local": ({"AuthenticationRequired": "DisabledForLocalAddresses"}, lambda b: [
        ("api-v1-nokey", "GET", "/api/v1/foo", {}, None, "127.0.0.1", None),
        ("api-foo-nokey", "GET", "/api/foo", {}, None, "127.0.0.1", None),
        ("api-v1-key", "GET", "/api/v1/foo", K, None, "127.0.0.1", None),
        ("api-v3-nokey", "GET", "/api/v3/system/status", {}, None, "127.0.0.1", None),
    ]),
    # 1.24 under Basic.
    "apipaths-basic": ({"AuthenticationMethod": "Basic"}, lambda b: [
        ("api-v1-nokey", "GET", "/api/v1/foo", {}, None, "127.0.0.1", None),
        ("api-v1-key", "GET", "/api/v1/foo", K, None, "127.0.0.1", None),
        ("api-v1-basic", "GET", "/api/v1/foo", {"Authorization": basic("unarr", "fixture")}, None, "127.0.0.1", None),
    ]),
}


def summarize(name, status, headers, raw):
    keep = {k: v for k, v in headers.items() if k not in ("date", "content-length", "transfer-encoding", "connection", "keep-alive")}
    return {"name": name, "status": status, "headers": keep, "body": raw[:3000].decode("utf-8", "replace"), "length": len(raw)}


results = {}
for scen in WANTED or list(SCEN):
    conf, build = SCEN[scen]
    p, data = start(conf)
    base = conf.get("UrlBase", "").rstrip("/")
    logfile = (data / APP / "logs" / f"{APP}.txt") if TARGET == "unarr" else (data / "logs" / f"{APP}.txt")
    before = logfile.stat().st_size if logfile.exists() else 0
    rows = []
    try:
        if scen == "forms-log":
            for name, (u, pw, src, hdr) in {
                "login-wrong": ("unarr", "nope", "127.0.0.1", {}),
                "login-unknown-user": ("nobody", "nope", "127.0.0.1", {}),
                "login-wrong-other": ("unarr", "nope", OTHER, {}),
                "login-wrong-fwd-for": ("unarr", "nope", "127.0.0.1", {"X-Forwarded-For": "10.9.8.7"}),
                "login-right": ("unarr", "fixture", "127.0.0.1", {}),
            }.items():
                s, h, r = login(base, u, pw, src, hdr)
                rows.append(summarize(name, s, h, r))
                if name == "login-right":
                    cookie = h.get("set-cookie", "").split(";")[0]
                    s, h, r = req("GET", f"{base}/logout", {"Cookie": cookie})
                    rows.append(summarize("logout", s, h, r))
                    s, h, r = req("GET", f"{base}/logout", {})
                    rows.append(summarize("logout-no-cookie", s, h, r))
            s, h, r = req("GET", "/api/v3/system/status", {"X-Api-Key": "nope"})
            rows.append(summarize("api-wrong-key", s, h, r))
        else:
            cookie = None
            for name, method, path, hdr, body, src, host in build(base):
                if hdr == "COOKIE":
                    if cookie is None:
                        s, h, r = login(base)
                        cookie = h.get("set-cookie", "").split(";")[0]
                    hdr = {"Cookie": cookie}
                s, h, r = req(method, path, hdr, body, src, host)
                rows.append(summarize(name, s, h, r))
        time.sleep(2)
    finally:
        stop(p)
    if scen == "basic-host":
        print((data / "config.xml").read_text(), file=sys.stderr)
    lines = []
    if logfile.exists():
        with open(logfile, "rb") as f:
            f.seek(before)
            lines = [l for l in f.read().decode("utf-8", "replace").splitlines()
                     if re.search(r"\|(Auth|Authentication|UrlBase|HostFiltering|ForwardedHeaders|Microsoft[^|]*)\|", l) or (scen == "lowhttp3" and re.search(r"\|(Http|Api)\|", l)) or "Auth-" in l or "unarrprobe" in l]
    results[scen] = {"config": conf, "answers": rows, "log": lines[:200]}
    print(f"[probe] {APP} {scen}: {len(rows)} answers, {len(lines)} log lines", file=sys.stderr)

OUT.write_text(json.dumps(results, indent=1))
