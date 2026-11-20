"""Side by side: every answer of two `http-probe.py` outputs, all headers but the volatile ones.

usage: python3 -I scripts/http-probe-diff.py ORIGINAL.json UNARR.json [NAME_PREFIX]
"""
import json, sys
a = json.load(open(sys.argv[1])); b = json.load(open(sys.argv[2]))
only = sys.argv[3] if len(sys.argv) > 3 else ""
SKIP = {"date", "content-length", "transfer-encoding", "connection", "keep-alive", "set-cookie"}
for scen in a:
    rb = {x["name"]: x for x in b.get(scen, {}).get("answers", [])}
    for x in a[scen]["answers"]:
        if only and not x["name"].startswith(only): continue
        y = rb.get(x["name"], {"status": None, "headers": {}, "body": "", "length": 0})
        print(f"== {scen} {x['name']}")
        for side, r in (("O", x), ("U", y)):
            h = {k: v for k, v in r["headers"].items() if k not in SKIP}
            print(f"  {side} {r['status']} len={r.get('length')} {json.dumps(h)[:400]}")
            print(f"    body: {r['body'][:160]!r}")
