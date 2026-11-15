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
ENV = {k: v for k, v in os.environ.items() if k.lower() not in ("https_proxy", "http_proxy", "all_proxy", "no_proxy")}


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

    def start(self) -> dict[int, float]:
        t0 = time.time()
        for label, argv, data in self.commands:
            out = open(data / f"{label}.stdout.log", "ab")
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
        self.procs = {}

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
    tail_cpu = (tail[-1]["cpu"] - tail[0]["cpu"]) / max(tail[-1]["t"] - tail[0]["t"], 1) * 3600
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


def report(res: dict, meta: dict) -> str:
    o, u = summary(res["originals"]), summary(res["unarr"])

    def less(a, b) -> str:
        return f"{(a - b) / a * 100:.0f} %" if a and b is not None else "n/a"

    def per_hour(x) -> str:
        return "n/a" if x is None else f"{x:.1f} s/h"

    rows = [
        ("First answer on both ports", f"{o['first']:.2f} s", f"{u['first']:.2f} s", f"{o['first'] / u['first']:.0f}× sooner"),
        ("Idle, at the end of the idle window: RSS", f"{o['idle_rss']:.0f} MiB", f"{u['idle_rss']:.0f} MiB", less(o["idle_rss"], u["idle_rss"])),
        ("Same, anonymous memory", f"{o['idle_anon']:.0f} MiB", f"{u['idle_anon']:.0f} MiB", less(o["idle_anon"], u["idle_anon"])),
        ("Idle CPU, from minute 5 of the window", per_hour(o["idle_cpu_h"]), per_hour(u["idle_cpu_h"]), less(o["idle_cpu_h"], u["idle_cpu_h"])),
        ("The reads: CPU", f"{o['reads_cpu']:.2f} s", f"{u['reads_cpu']:.2f} s", less(o["reads_cpu"], u["reads_cpu"])),
        ("The reads: time", f"{o['reads_s']:.2f} s", f"{u['reads_s']:.2f} s", less(o["reads_s"], u["reads_s"])),
        ("Peak anonymous memory during the reads", f"{o['peak_anon']:.0f} MiB", f"{u['peak_anon']:.0f} MiB", less(o["peak_anon"], u["peak_anon"])),
        ("Anonymous memory at the end of the tail", f"{o['tail_anon']:.0f} MiB", f"{u['tail_anon']:.0f} MiB", less(o["tail_anon"], u["tail_anon"])),
        ("Idle CPU in the tail", f"{o['tail_cpu_h']:.1f} s/h", f"{u['tail_cpu_h']:.1f} s/h", less(o["tail_cpu_h"], u["tail_cpu_h"])),
    ]
    out = [
        f"Library `{meta['library']}`; Unarr `{meta['unarr']}`; Sonarr {meta['sonarr']}, Radarr {meta['radarr']}; "
        f"first start {meta['first']} s, idle {meta['idle']} s, tail {meta['tail']} s; "
        f".NET heap limit {'768 MiB' if meta['cap'] else 'none'}; {meta['date']}.",
        "",
        "| | Sonarr + Radarr | Unarr | Less |",
        "|---|--:|--:|--:|",
    ]
    out += [f"| {a} | {b} | {c} | {d} |" for a, b, c, d in rows]
    out += ["", "| p50 / max, ms | Originals | Unarr |", "|---|--:|--:|"]
    for key in res["originals"]["latency"]:
        a = sorted(res["originals"]["latency"][key])
        b = sorted(res["unarr"]["latency"].get(key, [0.0]))
        out.append(f"| {key} | {a[len(a) // 2]:.1f} / {a[-1]:.1f} | {b[len(b) // 2]:.1f} / {b[-1]:.1f} |")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--library", choices=("real", "scaled"), default="real")
    ap.add_argument("--first", type=int, default=300, help="seconds of the first, unmeasured run")
    ap.add_argument("--idle", type=int, default=900, help="idle seconds sampled after the measured start")
    ap.add_argument("--tail", type=int, default=300, help="idle seconds sampled after the reads")
    ap.add_argument("--no-cap", action="store_true", help="no .NET heap limit for the originals")
    ap.add_argument("--out", type=Path, help="default .cache/compare/<library>")
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
        + ("" if a.unarrd == REPO / "target/release/unarrd" else f" ({a.unarrd})"),
        "sonarr": pinned("sonarr-fixture.sh", "SONARR_VERSION"),
        "radarr": pinned("radarr-fixture.sh", "RADARR_VERSION"),
        "first": a.first, "idle": a.idle, "tail": a.tail, "cap": not a.no_cap,
        "date": time.strftime("%Y-%m-%d"),
    }
    env = dict(ENV)
    if not a.no_cap:
        env["DOTNET_GCHeapHardLimit"] = "0x30000000"

    lib = stage(a.library, out / "originals")
    originals = Side("originals", [
        ("sonarr", [str(sonarr), "-nobrowser", f"-data={lib / 'sonarr'}"], lib / "sonarr"),
        ("radarr", [str(radarr), "-nobrowser", f"-data={lib / 'radarr'}"], lib / "radarr"),
    ], [SONARR_PORT, RADARR_PORT], env)
    res = {"meta": meta, "originals": measure(originals, a.first, a.idle, a.tail)}

    lib = stage(a.library, out / "unarr-library")
    data = out / "unarr"
    shutil.rmtree(data, ignore_errors=True)
    data.mkdir(parents=True)
    unarr = Side("unarr", [
        ("unarr", [str(unarrd), "--data", str(data), "--personalities", "sonarr,radarr",
                   "--import-from", f"{lib / 'sonarr'},{lib / 'radarr'}"], data),
    ], [SONARR_PORT, RADARR_PORT], ENV)
    res["unarr"] = measure(unarr, a.first, a.idle, a.tail)

    (out / "results.json").write_text(json.dumps(res, indent=1))
    text = report(res, meta)
    (out / "report.md").write_text(text)
    print(text)
    log("written", out / "results.json", "and", out / "report.md")


if __name__ == "__main__":
    main()
