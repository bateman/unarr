#!/usr/bin/env python3
"""A news server stand-in, so that a real SABnzbd can finish (or fail) a job.

Used to record SABnzbd talking to the original Sonarr and Radarr
(`tests/fixtures/downloadclients/sabnzbd`, `radarr-sabnzbd`): SABnzbd
fetches articles from it like from any news server, decodes them and
post-processes the job, so the original sees a completed download and
imports it. Articles it does not know answer 430, so a job made of those
fails as "Not on your server(s)". Standard library only.

    scripts/nntp-stub.py --make FILE OUT.nzb --store DIR
        cut FILE into yEnc articles, write the NZB that names them and
        remember them in DIR/articles.json
    scripts/nntp-stub.py --listen 127.0.0.1:11119 --store DIR
        serve the remembered articles (MODE READER, GROUP, BODY, ARTICLE,
        HEAD, STAT, QUIT); unknown message ids answer 430

Point SABnzbd at it (no SSL, no login), for instance through its API:
`mode=set_config&section=servers&keyword=stub&host=127.0.0.1&port=11119`.
To make a job fail, make its NZB, then drop its articles from
articles.json (or make it with another --store).
"""
import binascii
import hashlib
import json
import os
import socketserver
import sys
from pathlib import Path

PART = 400_000
STORE = Path(".")

def yenc(data: bytes, name: str, part: int, total: int, begin: int, full_size: int) -> bytes:
    out, line = [], bytearray()
    for i, b in enumerate(data):
        o = (b + 42) % 256
        if o in (0, 10, 13, 61) or (o in (9, 32) and (len(line) == 0)) or (o == 46 and len(line) == 0):
            line += b"=" + bytes([(o + 64) % 256])
        else:
            line.append(o)
        if len(line) >= 128:
            out.append(bytes(line)); line = bytearray()
    if line:
        out.append(bytes(line))
    crc = binascii.crc32(data) & 0xFFFFFFFF
    head = f"=ybegin part={part} total={total} line=128 size={full_size} name={name}\r\n=ypart begin={begin + 1} end={begin + len(data)}\r\n".encode()
    tail = f"\r\n=yend size={len(data)} part={part} pcrc32={crc:08x}\r\n".encode()
    return head + b"\r\n".join(out) + tail

def make(path: str, nzb_out: str):
    data = Path(path).read_bytes(); name = Path(path).name
    parts = [data[i:i + PART] for i in range(0, len(data), PART)]
    store = articles()
    segs = []
    for n, chunk in enumerate(parts, 1):
        mid = hashlib.sha1(f"{path}:{n}".encode()).hexdigest()[:16] + f".{n}@stub.invalid"
        store[mid] = [path, (n - 1) * PART, len(chunk), n, len(parts)]
        segs.append(f'      <segment bytes="{len(chunk)}" number="{n}">{mid}</segment>')
    (STORE / "articles.json").write_text(json.dumps(store))
    Path(nzb_out).write_text(f'''<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head><meta type="title">{Path(nzb_out).stem}</meta></head>
  <file poster="stub@stub.invalid" date="1763550000" subject="&quot;{name}&quot; yEnc (1/{len(parts)})">
    <groups><group>alt.binaries.test</group></groups>
    <segments>
{chr(10).join(segs)}
    </segments>
  </file>
</nzb>
''')

def articles() -> dict:
    return json.loads((STORE / "articles.json").read_text()) if (STORE / "articles.json").exists() else {}


class Handler(socketserver.StreamRequestHandler):
    def send(self, s): self.wfile.write(s if isinstance(s, bytes) else s.encode() + b"\r\n")
    def handle(self):
        store = articles()
        self.send("200 stub news server ready")
        for raw in self.rfile:
            parts = raw.decode(errors="replace").strip().split()
            if not parts: continue
            cmd = parts[0].upper()
            if cmd == "QUIT": self.send("205 bye"); return
            if cmd == "CAPABILITIES": self.send("101 caps\r\nVERSION 2\r\nREADER\r\n."); continue
            if cmd == "MODE": self.send("200 reader"); continue
            if cmd == "AUTHINFO": self.send("281 ok"); continue
            if cmd == "DATE": self.send("111 20261119120000"); continue
            if cmd == "GROUP": self.send(f"211 1 1 1 {parts[1] if len(parts) > 1 else 'x'}"); continue
            if cmd in ("BODY", "ARTICLE", "HEAD", "STAT") and len(parts) > 1:
                mid = parts[1].strip("<>")
                if mid not in store: self.send("430 no such article"); continue
                path, off, length, n, total = store[mid]
                with open(path, "rb") as f: f.seek(off); chunk = f.read(length)
                body = yenc(chunk, Path(path).name, n, total, off, os.path.getsize(path))
                lines = [(b"." + l if l.startswith(b".") else l) for l in body.split(b"\r\n")]
                code = {"BODY": 222, "ARTICLE": 220, "HEAD": 221, "STAT": 223}[cmd]
                if cmd == "STAT": self.send(f"223 0 <{mid}>"); continue
                self.send(f"{code} 0 <{mid}>")
                if cmd in ("HEAD", "ARTICLE"): self.send(f"Message-ID: <{mid}>\r\nSubject: x")
                if cmd == "ARTICLE": self.send("")
                if cmd != "HEAD": self.wfile.write(b"\r\n".join(lines) + b"\r\n")
                self.send("."); continue
            self.send("500 what?")

if __name__ == "__main__":
    args = sys.argv[1:]
    if "--store" in args:
        i = args.index("--store")
        STORE = Path(args[i + 1])
        STORE.mkdir(parents=True, exist_ok=True)
        del args[i : i + 2]
    if len(args) == 3 and args[0] == "--make":
        make(args[1], args[2])
    elif len(args) == 2 and args[0] == "--listen":
        host, port = args[1].rsplit(":", 1)
        socketserver.ThreadingTCPServer.allow_reuse_address = True
        with socketserver.ThreadingTCPServer((host, int(port)), Handler) as srv:
            srv.serve_forever()
    else:
        sys.exit(__doc__)
