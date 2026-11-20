"""Unarr's allocators compared: several builds of unarrd (each a binary and
its environment), run in the order A B C .. C B A on one library of
`scripts/compare-originals.py`, so that a drift of the machine falls on
every build alike. Unarr alone: the originals are not started.

Per fresh process: settle; idle memory; 5 warm-up passes of the comparison's
reads; one measured pass (time, CPU, peak anonymous memory sampled every
0.2 s, p50 per route); 60 s of 4 clients without pause over the reads'
routes (requests per second, p50/p95, peak); 60 s idle after the load.

The medians of each build are printed as a Markdown table.

usage: python3 -I scripts/allocator-probe.py OUT_DIR NAME=BINARY[;VAR=VALUE...] ... [--library real|scaled]

Example, the image's jemalloc settings against jemalloc's defaults, which
the environment restores (docs/originals-comparison.md, "The image's
allocator"):

    cargo build --release --target x86_64-unknown-linux-musl --bin unarrd
    B=target/x86_64-unknown-linux-musl/release/unarrd
    python3 -I scripts/allocator-probe.py .cache/allocators "tuned=$B" \
        "defaults=$B;_RJEM_MALLOC_CONF=background_thread:false,narenas:16,dirty_decay_ms:10000"
"""
import argparse, importlib.util, json, random, shutil, statistics, threading, time
from pathlib import Path

spec = importlib.util.spec_from_file_location("co", Path(__file__).resolve().parent / "compare-originals.py")
co = importlib.util.module_from_spec(spec)
spec.loader.exec_module(co)
ap = argparse.ArgumentParser()
ap.add_argument("out"); ap.add_argument("variants", nargs="+")
ap.add_argument("--library", default="scaled")
ap.add_argument("--settle", type=int, default=60)
args = ap.parse_args()
co.isolate()
out = Path(args.out).resolve(); out.mkdir(parents=True, exist_ok=True)
S, R = co.SONARR_PORT, co.RADARR_PORT
MIB = 1024 * 1024


class Peak:
    def __init__(self, pid):
        self.pid, self.peak, self.done = pid, 0, threading.Event()
        self.t = threading.Thread(target=self.run, daemon=True)

    def run(self):
        while not self.done.is_set():
            self.peak = max(self.peak, co.memory(self.pid)["anon"])
            time.sleep(0.2)

    def __enter__(self):
        self.t.start(); return self

    def __exit__(self, *a):
        self.done.set(); self.t.join()


def idle(pid, seconds):
    samples = []
    for _ in range(seconds):
        samples.append(co.memory(pid)); time.sleep(1)
    tail = samples[-10:]
    return {"rss_mib": statistics.median(s["rss"] for s in tail) / MIB,
            "anon_mib": statistics.median(s["anon"] for s in tail) / MIB}


def load(pid, seconds=60, clients=4, seed=7):
    routes = [(p, path) for p, path, _ in co.READS]
    lat, stop = [], time.monotonic() + seconds

    def client(i):
        rnd = random.Random(seed + i)
        while time.monotonic() < stop:
            port, path = rnd.choice(routes)
            t = time.perf_counter(); co.get(port, path); lat.append((time.perf_counter() - t) * 1000)

    with Peak(pid) as pk:
        threads = [threading.Thread(target=client, args=(i,)) for i in range(clients)]
        t0 = time.perf_counter()
        for t in threads: t.start()
        for t in threads: t.join()
        elapsed = time.perf_counter() - t0
    lat.sort()
    return {"rps": len(lat) / elapsed, "p50_ms": lat[len(lat) // 2], "p95_ms": lat[int(len(lat) * 0.95)],
            "peak_anon_mib": pk.peak / MIB}


variants = {}
for v in args.variants:
    name, rest = v.split("=", 1)
    binary, *env = rest.split(";")
    variants[name] = (binary, dict(e.split("=", 1) for e in env))
names = list(variants)
res = {n: [] for n in names}
for i, tag in enumerate(names + names[::-1]):
    unarrd, extra = variants[tag]
    lib = co.stage(args.library, out / f"lib{i}")
    data = out / f"data{i}"; shutil.rmtree(data, ignore_errors=True); data.mkdir(parents=True)
    side = co.Side("unarr", [("unarr", [unarrd, "--data", str(data), "--personalities", "sonarr,radarr",
        "--import-from", str(lib / "sonarr"), "--import-from", str(lib / "radarr")], data)], [S, R], {**co.ENV, **extra})
    side.start()
    pid = next(iter(side.procs.values())).pid
    co.log(f"{i} {tag}: up, settling {args.settle} s")
    try:
        r = {"idle": idle(pid, args.settle)}
        for _ in range(5): co.reads()
        c0 = co.cpu_seconds(pid); t0 = time.perf_counter()
        with Peak(pid) as pk:
            lat = co.reads()
        r["reads"] = {"time_s": time.perf_counter() - t0, "cpu_s": co.cpu_seconds(pid) - c0,
                      "peak_anon_mib": pk.peak / MIB,
                      "p50_ms": {k: statistics.median(v) for k, v in lat.items()}}
        r["load4"] = load(pid)
        r["after_load"] = idle(pid, 60)
        res[tag].append(r)
        co.log(tag, json.dumps({k: v for k, v in r.items() if k != "reads"}))
    finally:
        side.stop(); shutil.rmtree(lib, ignore_errors=True); shutil.rmtree(data, ignore_errors=True)

(out / "results.json").write_text(json.dumps(res, indent=1))


def med(tag, f):
    return statistics.median(f(x) for x in res[tag])


rows = [
    ("Idle after the start: RSS / anonymous, MiB", lambda x: (x["idle"]["rss_mib"], x["idle"]["anon_mib"])),
    ("The reads: time, s", lambda x: x["reads"]["time_s"]),
    ("The reads: CPU, s", lambda x: x["reads"]["cpu_s"]),
    ("The reads: peak anonymous, MiB", lambda x: x["reads"]["peak_anon_mib"]),
    ("4 clients: requests per second", lambda x: x["load4"]["rps"]),
    ("4 clients: p50 / p95, ms", lambda x: (x["load4"]["p50_ms"], x["load4"]["p95_ms"])),
    ("4 clients: peak anonymous, MiB", lambda x: x["load4"]["peak_anon_mib"]),
    ("Idle after the load: RSS / anonymous, MiB", lambda x: (x["after_load"]["rss_mib"], x["after_load"]["anon_mib"])),
]
print(f"| {args.library} | " + " | ".join(names) + " |\n|---|" + "--:|" * len(names))
for name, f in rows:
    cells = []
    for tag in names:
        vals = [f(x) for x in res[tag]]
        if isinstance(vals[0], tuple):
            cells.append(" / ".join(f"{statistics.median(v[j] for v in vals):.1f}" for j in range(2)))
        else:
            cells.append(f"{statistics.median(vals):.2f}")
    print(f"| {name} | " + " | ".join(cells) + " |")
print("| route p50, ms | " + " | ".join(names) + " |")
for k in res[names[0]][0]["reads"]["p50_ms"]:
    print(f"| {k} | " + " | ".join(f"{statistics.median(x['reads']['p50_ms'][k] for x in res[t]):.1f}" for t in names) + " |")
