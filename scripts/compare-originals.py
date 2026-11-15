#!/usr/bin/env python3
"""Measure the original Sonarr and Radarr beside Unarr: RAM, CPU, latency.

The same machine and the same library for both sides, one side at a time:

- the originals: the official Sonarr and Radarr that ``make ui`` downloads
  into ``.cache/`` (the versions pinned in ``scripts/sonarr-fixture.sh`` and
  ``scripts/radarr-fixture.sh``), two processes;
- Unarr: ``target/release/unarrd`` (``make build``) with both personalities,
  one process.

Each side starts once on a fresh copy of the library and runs ``--first``
seconds (migrations, Unarr's import, the overdue scheduled tasks); that run
is not measured. It then starts again and is measured: the time to the first
answer on each port, ``--idle`` seconds sampled every five, the same reads
over both APIs, ``--tail`` idle seconds. RAM is the anonymous memory and the
RSS of ``/proc/<pid>/smaps_rollup`` summed over the side's processes; CPU is
``utime + stime`` (and the children's) of ``/proc/<pid>/stat``. Neither side
reaches the network: the proxy variables are removed, as the fixture scripts
do. The originals run with ``DOTNET_GCHeapHardLimit`` at 768 MiB, what .NET
takes from a 1 GiB container such as the homelab's (``--no-cap`` lifts it).

Libraries: ``real`` is the anonymised fixtures (``tests/fixtures``), the
maintainer's own library; ``scaled`` is ``.cache/scaled`` from
``make scale-fixture`` (950 series, 2,123 movies)::

    make ui build
    python3 -I scripts/compare-originals.py --library real
    make compare-originals LIBRARY=scaled

The results go to ``.cache/compare/<library>/``: ``results.json`` with every
sample and ``report.md`` with the tables ``docs/originals-comparison.md``
keeps.
"""

from __future__ import annotations

import argparse
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
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
KEY = "0123456789abcdef0123456789abcdef"
SONARR_PORT, RADARR_PORT = 8989, 7878
TICK = os.sysconf("SC_CLK_TCK")
MIB = 1 << 20
# The caller's environment, without what would make one side work
# differently from a plain start: the proxy (neither side reaches the
# network), Unarr's log filter and the .NET runtime's knobs (the heap
# limit is set on purpose below).
ENV = {
    k: v
    for k, v in os.environ.items()
    if k.lower() not in ("https_proxy", "http_proxy", "all_proxy", "no_proxy", "rust_log")
    and not k.startswith(("DOTNET_", "COMPlus_"))
}


def log(*a):
    print("[compare]", *a, file=sys.stderr, flush=True)


def get(port: int, path: str, timeout: float = 600) -> bytes:
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", headers={"X-Api-Key": KEY})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def listening(port: int) -> bool:
    """Something accepts connections on the port, whatever it is."""
    with socket.socket() as s:
        s.settimeout(1)
        return s.connect_ex(("127.0.0.1", port)) == 0


def answers(port: int) -> bool:
    try:
        get(port, "/api/v3/system/status", timeout=5)
        return True
    except Exception:
        return False


def memory(pid: int) -> dict:
    out = {"rss": 0, "anon": 0}
    try:
        for line in open(f"/proc/{pid}/smaps_rollup"):
            key, _, rest = line.partition(":")
            if key == "Rss":
                out["rss"] = int(rest.split()[0]) * 1024
            elif key == "Anonymous":
                out["anon"] = int(rest.split()[0]) * 1024
    except FileNotFoundError:
        pass
    return out


def cpu_seconds(pid: int) -> float:
    try:
        fields = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    except FileNotFoundError:
        return 0.0
    # After the command: state is fields[0]; utime, stime, cutime, cstime are 11 to 14.
    return sum(int(x) for x in fields[11:15]) / TICK


class Side:
    """One side: the processes it starts and the ports it answers on."""

    def __init__(self, name: str, commands: list[tuple[str, list[str], Path]], ports: list[int], env: dict):
        self.name, self.commands, self.ports, self.env = name, commands, ports, env
        self.procs: dict[str, subprocess.Popen] = {}
        self.logs: list = []

    def start(self) -> dict[int, float]:
        t0 = time.time()
        for label, argv, data in self.commands:
            out = open(data / f"{label}.stdout.log", "ab")
            self.logs.append(out)
            self.procs[label] = subprocess.Popen(argv, stdout=out, stderr=subprocess.STDOUT, env=self.env)
        first: dict[int, float] = {}
        while len(first) < len(self.ports):
            for port in self.ports:
                if port not in first and answers(port):
                    first[port] = round(time.time() - t0, 3)
            for label, proc in self.procs.items():
                if proc.poll() is not None:
                    raise SystemExit(f"{label} exited early, see its stdout.log")
            if time.time() - t0 > 900:
                raise SystemExit(f"{self.name} did not answer within 15 minutes")
            time.sleep(0.05)
        return first

    def stop(self):
        for proc in self.procs.values():
            proc.send_signal(signal.SIGTERM)
        for proc in self.procs.values():
            try:
                proc.wait(60)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        self.procs = {}
        for f in self.logs:
            f.close()
        self.logs = []

    def sample(self) -> dict:
        s = {"t": time.time(), "rss": 0, "anon": 0, "cpu": 0.0}
        for proc in self.procs.values():
            m = memory(proc.pid)
            s["rss"] += m["rss"]
            s["anon"] += m["anon"]
            s["cpu"] += cpu_seconds(proc.pid)
        return s


def idle(side: Side, seconds: int) -> list[dict]:
    rows, end = [], time.time() + seconds
    while True:
        rows.append(side.sample())
        if time.time() >= end:
            return rows
        time.sleep(5)


READS = [
    (SONARR_PORT, "/api/v3/series", 20),
    (SONARR_PORT, "/api/v3/history?page=1&pageSize=20&includeSeries=true&includeEpisode=true", 20),
    (SONARR_PORT, "/api/v3/wanted/missing?page=1&pageSize=20&includeSeries=true", 20),
    (SONARR_PORT, "/api/v3/calendar?start=2026-11-01&end=2026-12-01", 20),
    (SONARR_PORT, "/api/v3/queue?page=1&pageSize=20", 20),
    (RADARR_PORT, "/api/v3/movie", 20),
    (RADARR_PORT, "/api/v3/history?page=1&pageSize=20&includeMovie=true", 20),
    (RADARR_PORT, "/api/v3/wanted/missing?page=1&pageSize=20", 20),
    (RADARR_PORT, "/api/v3/calendar?start=2026-11-01&end=2026-12-01", 20),
    (RADARR_PORT, "/api/v3/queue?page=1&pageSize=20", 20),
]


def reads() -> dict[str, list[float]]:
    """The same requests on each side; the latency of each, in ms."""
    lat: dict[str, list[float]] = {}

    def timed(port: int, path: str, n: int, key: str):
        for _ in range(n):
            t = time.perf_counter()
            get(port, path)
            lat.setdefault(key, []).append((time.perf_counter() - t) * 1000)

    app = {SONARR_PORT: "Sonarr", RADARR_PORT: "Radarr"}
    for port, path, n in READS:
        timed(port, path, n, f"{app[port]} {path.split('?')[0]}")
    series = json.loads(get(SONARR_PORT, "/api/v3/series"))
    for s in series[:20]:
        timed(SONARR_PORT, f"/api/v3/episode?seriesId={s['id']}&includeEpisodeFile=true", 1, "Sonarr /api/v3/episode")
    return lat


def measure(side: Side, first_seconds: int, idle_seconds: int, tail_seconds: int) -> dict:
    """Both starts of one side; its processes are stopped however it ends."""
    try:
        return run_side(side, first_seconds, idle_seconds, tail_seconds)
    finally:
        side.stop()


def run_side(side: Side, first_seconds: int, idle_seconds: int, tail_seconds: int) -> dict:
    log(side.name, f"first start, {first_seconds} s, not measured")
    side.start()
    time.sleep(first_seconds)
    side.stop()
    log(side.name, "measured start")
    first = side.start()
    # What actually runs, as each application reports it.
    versions = {port: json.loads(get(port, "/api/v3/system/status")).get("version", "?") for port in side.ports}
    samples = idle(side, idle_seconds)
    before = side.sample()
    peak = dict(before)
    done = threading.Event()

    def watch():
        while not done.is_set():
            s = side.sample()
            for k in ("rss", "anon"):
                peak[k] = max(peak[k], s[k])
            time.sleep(0.2)

    watcher = threading.Thread(target=watch)
    watcher.start()
    t0 = time.time()
    latencies = reads()
    done.set()
    watcher.join()
    after = side.sample()
    tail = idle(side, tail_seconds)
    side.stop()
    return {
        "versions": versions,
        "first": first,
        "idle": samples,
        "reads": {"seconds": after["t"] - t0, "cpu": after["cpu"] - before["cpu"],
                  "peak": peak, "after": after},
        "latency": latencies,
        "tail": tail,
    }


def stage(library: str, root: Path) -> Path:
    """A fresh copy of the library for one side."""
    shutil.rmtree(root, ignore_errors=True)
    for app in ("sonarr", "radarr"):
        dst = root / app
        if library == "real":
            dst.mkdir(parents=True)
            for f in ("config.xml", f"{app}.db"):
                shutil.copy2(REPO / "tests/fixtures" / app / f, dst / f)
        else:
            src = REPO / ".cache/scaled" / app
            if not (src / f"{app}.db").exists():
                raise SystemExit(f"no {src}/{app}.db: run make scale-fixture first")
            shutil.copytree(src, dst)
    return root


def pinned(script: str, var: str) -> str:
    m = re.search(rf'{var}="\$\{{{var}:-([^}}]+)\}}"', (REPO / "scripts" / script).read_text())
    return m.group(1) if m else "?"


def stale(binary: Path) -> str:
    """A note when the binary predates the last commit of the checkout that
    touched the code, or one of its uncommitted changes to it: then the
    commit named is not what runs. A binary built from another branch after
    those goes unnoticed; make compare-originals builds first."""
    code = ["crates", "Cargo.toml", "Cargo.lock"]
    last = subprocess.run(["git", "-C", str(REPO), "log", "-1", "--format=%ct", "--", *code],
                          capture_output=True, text=True).stdout.strip()
    newest = int(last) if last.isdigit() else 0
    changed = subprocess.run(["git", "-C", str(REPO), "diff", "--name-only", "HEAD", "--", *code],
                             capture_output=True, text=True).stdout.split()
    for name in changed:
        f = REPO / name
        if f.exists():
            newest = max(newest, int(f.stat().st_mtime))
    if binary.stat().st_mtime < newest:
        log(f"warning: {binary} is older than the checkout; make build first")
        return ", binary older than the checkout"
    return ""


def at(rows: list[dict], seconds: float) -> dict:
    t0 = rows[0]["t"]
    return min(rows, key=lambda r: abs(r["t"] - t0 - seconds))


def summary(r: dict) -> dict:
    i, tail = r["idle"], r["tail"]
    end = i[-1]
    five = at(i, 300)
    # Counted from minute 5, once the start-up work is done: a shorter
    # window has nothing to count.
    idle_cpu = (end["cpu"] - five["cpu"]) / (end["t"] - five["t"]) * 3600 if end["t"] - five["t"] >= 60 else None
    span = tail[-1]["t"] - tail[0]["t"]
    tail_cpu = (tail[-1]["cpu"] - tail[0]["cpu"]) / span * 3600 if span >= 60 else None
    return {
        "first": max(r["first"].values()),
        "idle_anon": end["anon"] / MIB,
        "idle_rss": end["rss"] / MIB,
        "idle_cpu_h": idle_cpu,
        "tail_cpu_h": tail_cpu,
        "reads_cpu": r["reads"]["cpu"],
        "reads_s": r["reads"]["seconds"],
        "peak_anon": r["reads"]["peak"]["anon"] / MIB,
        "tail_anon": tail[-1]["anon"] / MIB,
        "tail_rss": tail[-1]["rss"] / MIB,
    }


def runs(res: dict, side: str) -> list[dict]:
    """A side's runs: a list since --repeat, one run in older results."""
    r = res[side]
    return r if isinstance(r, list) else [r]


def median(xs: list[float]) -> float:
    xs = sorted(xs)
    n = len(xs)
    return xs[n // 2] if n % 2 else (xs[n // 2 - 1] + xs[n // 2]) / 2


def report(res: dict, meta: dict) -> str:
    """The tables of report.md. With several runs a cell is the median and,
    in brackets, the lowest and the highest run; Less compares the medians."""
    o_runs = [summary(r) for r in runs(res, "originals")]
    u_runs = [summary(r) for r in runs(res, "unarr")]

    def values(rs: list[dict], key: str) -> list[float] | None:
        xs = [r[key] for r in rs]
        return None if any(x is None for x in xs) else xs

    def cell(rs: list[dict], key: str, fmt: str, unit: str) -> str:
        xs = values(rs, key)
        if xs is None:
            return "n/a"
        text = f"{median(xs):{fmt}} {unit}"
        if len(xs) > 1:
            text += f" ({min(xs):{fmt}}–{max(xs):{fmt}})"
        return text

    def mid(rs: list[dict], key: str) -> float | None:
        xs = values(rs, key)
        return None if xs is None else median(xs)

    def less(key: str) -> str:
        a, b = mid(o_runs, key), mid(u_runs, key)
        if not a or b is None:
            return "n/a"
        d = (a - b) / a * 100
        return f"{d:.0f} %" if d >= 0 else f"{-d:.0f} % more"

    def sooner(key: str) -> str:
        a, b = mid(o_runs, key), mid(u_runs, key)
        return f"{a / b:.0f}× sooner" if a and b else "n/a"

    rows = [
        ("First answer on both ports", "first", ".2f", "s", sooner),
        ("Idle, at the end of the idle window: RSS", "idle_rss", ".0f", "MiB", less),
        ("Same, anonymous memory", "idle_anon", ".0f", "MiB", less),
        ("Idle CPU, from minute 5 of the window", "idle_cpu_h", ".1f", "s/h", less),
        ("The reads: CPU", "reads_cpu", ".2f", "s", less),
        ("The reads: time", "reads_s", ".2f", "s", less),
        ("Peak anonymous memory during the reads", "peak_anon", ".0f", "MiB", less),
        ("Anonymous memory at the end of the tail", "tail_anon", ".0f", "MiB", less),
        ("Idle CPU in the tail, from its first minute", "tail_cpu_h", ".1f", "s/h", less),
    ]
    n = len(o_runs)
    reported = runs(res, "originals")[0].get("versions", {})
    sonarr_v = reported.get(str(SONARR_PORT), reported.get(SONARR_PORT, meta["sonarr"]))
    radarr_v = reported.get(str(RADARR_PORT), reported.get(RADARR_PORT, meta["radarr"]))
    out = [
        f"Library `{meta['library']}`; Unarr `{meta['unarr']}`; Sonarr {sonarr_v}, Radarr {radarr_v}; "
        f"first start {meta['first']} s, idle {meta['idle']} s, tail {meta['tail']} s; "
        f".NET heap limit {'768 MiB' if meta['cap'] else 'none'}; "
        + (f"{n} runs per side, median (lowest–highest); " if n > 1 else "")
        + f"{meta['date']}.",
        "",
        "| | Sonarr + Radarr | Unarr | Less |",
        "|---|--:|--:|--:|",
    ]
    out += [f"| {label} | {cell(o_runs, k, f, u)} | {cell(u_runs, k, f, u)} | {how(k)} |" for label, k, f, u, how in rows]

    # Latency: each run's p50, then their median; the highest call of all runs.
    def p50s(side: str, key: str) -> list[float]:
        return [median(r["latency"][key]) for r in runs(res, side) if key in r["latency"]]

    def tops(side: str, key: str) -> float:
        return max((max(r["latency"][key]) for r in runs(res, side) if key in r["latency"]), default=0.0)

    def lat(side: str, key: str) -> str:
        xs = p50s(side, key)
        if not xs:
            return "n/a"
        text = f"{median(xs):.1f}"
        if len(xs) > 1:
            text += f" ({min(xs):.1f}–{max(xs):.1f})"
        return f"{text} / {tops(side, key):.1f}"

    out += ["", "| p50 / max, ms | Originals | Unarr |", "|---|--:|--:|"]
    for key in runs(res, "originals")[0]["latency"]:
        out.append(f"| {key} | {lat('originals', key)} | {lat('unarr', key)} |")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--library", choices=("real", "scaled"), default="real")
    ap.add_argument("--first", type=int, default=300, help="seconds of the first, unmeasured run")
    ap.add_argument("--idle", type=int, default=900, help="idle seconds sampled after the measured start")
    ap.add_argument("--tail", type=int, default=300, help="idle seconds sampled after the reads")
    ap.add_argument("--no-cap", action="store_true", help="no .NET heap limit for the originals")
    ap.add_argument("--out", type=Path, help="default .cache/compare/<library>")
    ap.add_argument("--repeat", type=int, default=1,
                    help="runs per side, the order alternating between runs (default 1)")
    ap.add_argument("--unarrd", type=Path, default=REPO / "target/release/unarrd",
                    help="the Unarr binary (default target/release/unarrd, built by make build)")
    a = ap.parse_args()

    out = a.out or REPO / ".cache/compare" / a.library
    sonarr = REPO / ".cache/sonarr/Sonarr/Sonarr"
    radarr = REPO / ".cache/radarr/Radarr/Radarr"
    unarrd = a.unarrd
    for binary, how in ((sonarr, "make ui"), (radarr, "make ui"), (unarrd, "make build")):
        if not binary.exists():
            raise SystemExit(f"{binary} missing: run {how} first")
    for port in (SONARR_PORT, RADARR_PORT):
        if listening(port):
            raise SystemExit(f"something already listens on {port}: stop it first")

    meta = {
        "library": a.library,
        # The checkout's commit: true of the binary when make built it.
        "unarr": subprocess.run(["git", "-C", str(REPO), "describe", "--always", "--dirty"],
                                capture_output=True, text=True).stdout.strip()
        + ("" if a.unarrd == REPO / "target/release/unarrd" else f" ({a.unarrd})")
        + stale(unarrd),
        "sonarr": pinned("sonarr-fixture.sh", "SONARR_VERSION"),
        "radarr": pinned("radarr-fixture.sh", "RADARR_VERSION"),
        "first": a.first, "idle": a.idle, "tail": a.tail, "cap": not a.no_cap,
        "date": time.strftime("%Y-%m-%d"),
    }
    env = dict(ENV)
    if not a.no_cap:
        env["DOTNET_GCHeapHardLimit"] = "0x30000000"

    if a.repeat < 1:
        raise SystemExit("--repeat must be at least 1")
    meta["repeat"] = a.repeat

    def originals() -> Side:
        lib = stage(a.library, out / "originals")
        return Side("originals", [
            ("sonarr", [str(sonarr), "-nobrowser", f"-data={lib / 'sonarr'}"], lib / "sonarr"),
            ("radarr", [str(radarr), "-nobrowser", f"-data={lib / 'radarr'}"], lib / "radarr"),
        ], [SONARR_PORT, RADARR_PORT], env)

    def unarr() -> Side:
        lib = stage(a.library, out / "unarr-library")
        data = out / "unarr"
        shutil.rmtree(data, ignore_errors=True)
        data.mkdir(parents=True)
        return Side("unarr", [
            ("unarr", [str(unarrd), "--data", str(data), "--personalities", "sonarr,radarr",
                       "--import-from", f"{lib / 'sonarr'},{lib / 'radarr'}"], data),
        ], [SONARR_PORT, RADARR_PORT], ENV)

    res: dict = {"meta": meta, "originals": [], "unarr": []}
    for run in range(a.repeat):
        # The originals first in odd runs, Unarr first in even ones: a drift
        # of the machine over the hours favours neither side.
        order = [("originals", originals), ("unarr", unarr)]
        if run % 2:
            order.reverse()
        for name, make in order:
            log(f"run {run + 1} of {a.repeat}")
            res[name].append(measure(make(), a.first, a.idle, a.tail))
        # Kept after every run: an interrupted series still has its results.
        (out / "results.json").write_text(json.dumps(res, indent=1))

    (out / "results.json").write_text(json.dumps(res, indent=1))
    text = report(res, meta)
    (out / "report.md").write_text(text)
    print(text)
    log("written", out / "results.json", "and", out / "report.md")


if __name__ == "__main__":
    main()
