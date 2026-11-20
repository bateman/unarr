"""The percentiles of the routes `docs/DESIGN.md` section 9 sets targets
for, on the original Sonarr and Radarr and on Unarr, one side at a time, on
one library of `scripts/compare-originals.py` and in its network namespace
(neither side reaches the network).

Per side: a fresh copy of the library, started and left --settle seconds
for the overdue scheduled tasks (Unarr imports the originals' databases
then); five warm-up passes over the routes; then --calls rounds, each
asking every route once in turn, each call timed from the request to the
answer's last byte. The originals run with the comparison's .NET heap limit
(768 MiB). Printed: p50, p95, p99 and the maximum per route and side, in
ms, as a Markdown table; every time goes to OUT/results.json.

usage: python3 -I scripts/route-percentiles.py [--library real|scaled] [--calls 200] [--unarrd PATH] [--out DIR]

    cargo build --release --bin unarrd
    python3 -I scripts/route-percentiles.py --library real
    python3 -I scripts/route-percentiles.py --library scaled
"""
import argparse, importlib.util, json, math, shutil, time
from pathlib import Path

spec = importlib.util.spec_from_file_location("co", Path(__file__).resolve().parent / "compare-originals.py")
co = importlib.util.module_from_spec(spec)
spec.loader.exec_module(co)
ap = argparse.ArgumentParser()
ap.add_argument("--library", choices=("real", "scaled"), default="real")
ap.add_argument("--calls", type=int, default=200, help="rounds over the routes, each route once a round")
ap.add_argument("--settle", type=int, default=120, help="seconds after the start, before the warm-up")
ap.add_argument("--unarrd", type=Path, default=co.REPO / "target/release/unarrd")
ap.add_argument("--out", type=Path)
args = ap.parse_args()
co.isolate()
S, R = co.SONARR_PORT, co.RADARR_PORT
# What the pages ask: the series and movie indexes, the collections page, a
# page of the history with its movies (the comparison's reads ask the same).
ROUTES = [
    (S, "/api/v3/series"),
    (R, "/api/v3/movie"),
    (R, "/api/v3/collection"),
    (R, "/api/v3/history?page=1&pageSize=20&includeMovie=true"),
]
out = (args.out or co.REPO / ".cache/route-percentiles" / args.library).resolve()
out.mkdir(parents=True, exist_ok=True)
sonarr, radarr = co.original_binaries()
if not args.unarrd.exists():
    raise SystemExit(f"{args.unarrd} missing: cargo build --release --bin unarrd")


def originals() -> "co.Side":
    env = dict(co.ENV)
    if co.without_icu():
        env.setdefault("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "1")
    env["DOTNET_GCHeapHardLimit"] = "0x30000000"
    lib = co.stage(args.library, out / "originals")
    return co.Side("originals", [
        ("sonarr", [str(sonarr), "-nobrowser", "-exitimmediately", f"-data={lib / 'sonarr'}"], lib / "sonarr"),
        ("radarr", [str(radarr), "-nobrowser", "-exitimmediately", f"-data={lib / 'radarr'}"], lib / "radarr"),
    ], [S, R], env)


def unarr() -> "co.Side":
    lib = co.stage(args.library, out / "unarr-library")
    data = out / "unarr"
    shutil.rmtree(data, ignore_errors=True)
    data.mkdir(parents=True)
    return co.Side("unarr", [
        ("unarr", [str(args.unarrd.resolve()), "--data", str(data), "--personalities", "sonarr,radarr",
                   "--import-from", str(lib / "sonarr"), "--import-from", str(lib / "radarr")], data),
    ], [S, R], co.ENV)


def timed(port: int, path: str) -> float:
    t = time.perf_counter()
    co.get(port, path)
    return (time.perf_counter() - t) * 1000


def percentile(xs: list[float], q: float) -> float:
    """Nearest rank: the smallest time at least q of the calls took no more than."""
    xs = sorted(xs)
    return xs[max(0, math.ceil(q * len(xs)) - 1)]


results = {}
for make in (originals, unarr):
    side = make()
    side.start()
    co.log(f"{side.name}: up, settling {args.settle} s")
    try:
        time.sleep(args.settle)
        for _ in range(5):
            for port, path in ROUTES:
                timed(port, path)
        lat = {path: [] for _, path in ROUTES}
        for _ in range(args.calls):
            for port, path in ROUTES:
                lat[path].append(timed(port, path))
        results[side.name] = lat
        co.log(side.name, "done")
    finally:
        side.stop()

(out / "results.json").write_text(json.dumps(
    {"library": args.library, "calls": args.calls, "settle": args.settle, "date": time.strftime("%Y-%m-%d"),
     "results": results}, indent=1))
print(f"| {args.library}, {args.calls} calls, ms | side | p50 | p95 | p99 | max |\n|---|---|--:|--:|--:|--:|")
for _, path in ROUTES:
    for name in results:
        xs = results[name][path]
        print(f"| `{path.split('?')[0]}` | {name} | {percentile(xs, .5):.1f} | {percentile(xs, .95):.1f} "
              f"| {percentile(xs, .99):.1f} | {max(xs):.1f} |")
