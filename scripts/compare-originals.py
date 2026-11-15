#!/usr/bin/env python3
"""Measure the original Sonarr and Radarr beside Unarr: RAM, CPU, latency.

The same machine and the same library for both sides, one side at a time:

- the originals: the official Sonarr and Radarr that ``make ui`` downloads
  into ``.cache/`` (the versions pinned in ``scripts/sonarr-fixture.sh`` and
  ``scripts/radarr-fixture.sh``), two processes;
- Unarr: ``unarrd``, which the script has cargo build from the checkout first
  (``--unarrd`` names a prebuilt one instead) and copies into ``--out``, where
  the copy runs, with both personalities, one process.

Each side starts once on a fresh copy of the library and runs ``--first``
seconds (migrations, Unarr's import, the overdue scheduled tasks); that run
is not measured. It then starts again and is measured: the time to the first
answer on each port, ``--idle`` seconds sampled every five, the same reads
over both APIs, ``--tail`` idle seconds. RAM is the anonymous memory and the
RSS of ``/proc/<pid>/smaps_rollup`` summed over the side's processes; CPU is
``utime + stime`` (and the children's) of ``/proc/<pid>/stat``. Neither side
reaches the network: the script runs itself in a network namespace where only
the loopback is up (``unshare -rn``; ``--network`` measures with the network,
not comparably). The originals run with ``DOTNET_GCHeapHardLimit`` at 768 MiB, what .NET
takes from a 1 GiB container such as the homelab's (``--no-cap`` lifts it).

Libraries: ``real`` is the anonymised fixtures (``tests/fixtures``), the
maintainer's own library; ``scaled`` is ``.cache/scaled`` from
``make scale-fixture`` (950 series, 2,123 movies)::

    make ui
    python3 -I scripts/compare-originals.py --library real
    make compare-originals LIBRARY=scaled

The results go to ``.cache/compare/<library>/``: ``results.json`` with every
sample and ``report.md`` with the tables ``docs/originals-comparison.md``
keeps.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import socket
import struct
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
# differently from a plain start: the proxy, Unarr's log filter and its
# UNARR_* settings (a bind address, an ffprobe, a metadata upstream), the
# originals' own SONARR__* and RADARR__* settings (a log level, a key, a
# port), the .NET runtime's knobs (the heap limit is set on purpose below; the
# globalization switch is what lets .NET start without libicu).
ENV = {
    k: v
    for k, v in os.environ.items()
    if k.lower() not in ("https_proxy", "http_proxy", "all_proxy", "no_proxy", "rust_log")
    # Prefixes compared ignoring case, as .NET's configuration and Unarr
    # read them (Sonarr__Log__Level counts as SONARR__LOG__LEVEL).
    and (not k.upper().startswith(("DOTNET_", "COMPLUS_")) or k.upper() == "DOTNET_SYSTEM_GLOBALIZATION_INVARIANT")
    and not k.upper().startswith(("UNARR_", "SONARR__", "RADARR__", "LIDARR__"))
}


def without_icu() -> bool:
    """.NET does not start without ICU unless told to do without it, as the
    fixture scripts tell it when `ldconfig -p` lists no libicuuc."""
    ldconfig = shutil.which("ldconfig") or next(
        (p for p in ("/sbin/ldconfig", "/usr/sbin/ldconfig") if os.path.exists(p)), None
    )
    listed = ""
    if ldconfig:
        listed = subprocess.run([ldconfig, "-p"], capture_output=True, text=True).stdout
    # No list at all reads as no libicu, as `ldconfig -p 2>/dev/null || true`
    # does in the fixture scripts.
    return "libicuuc" not in listed


# The script's own requests go straight to the loopback, whatever proxy the
# caller's environment names.
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def leftovers(unarrds: list[Path]) -> list[str]:
    """Running processes of the originals and of the unarrd binaries given
    (the one this run will start and target/release/unarrd)."""
    cache = os.environ.get("UNARR_CACHE")
    # Resolved: /proc/<pid>/exe names the file itself, past any symlink in
    # the way (a .cache linked from a worktree, a relative UNARR_CACHE).
    wanted = {
        str(p.resolve())
        for p in (
            *unarrds,
            (Path(cache) / "Sonarr/Sonarr") if cache else REPO / ".cache/sonarr/Sonarr/Sonarr",
            (Path(cache) / "Radarr/Radarr") if cache else REPO / ".cache/radarr/Radarr/Radarr",
        )
    }
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            exe = os.readlink(entry / "exe")
        except OSError:
            continue
        # A binary replaced on disk (a rebuild, make ui after a new pin)
        # still runs under its old path, which the link marks as deleted.
        exe = exe.removesuffix(" (deleted)")
        # Any run's own copy, whatever its --out: <out>/bin/unarrd beside
        # the marker of a folder the script claimed.
        copy = Path(exe).name == "unarrd" and (Path(exe).parent.parent / MARKER).exists()
        if exe in wanted or copy:
            found.append(f"pid {entry.name} {Path(exe).name}")
    return found


def other_comparisons() -> list[str]:
    """Running instances of this script other than this one and the shells
    or makes that started it (whose command lines may name it too)."""
    mine, pid = set(), os.getpid()
    while pid > 1:
        mine.add(pid)
        try:
            pid = int(open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            break
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) in mine:
            continue
        try:
            argv = (entry / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        # The interpreter running the script, not a shell or an editor
        # whose arguments happen to mention it.
        if any(Path(x.decode(errors="replace")).name == "compare-originals.py" for x in argv[1:3]) \
                and Path(argv[0].decode(errors="replace")).name.startswith(("python", "unshare")):
            found.append(f"pid {entry.name}")
    return found


MARKER = ".compare-originals"


# What a run writes under --out, marker aside.
OWN = {"results.json", "report.md", "bin", "originals", "unarr", "unarr-library"}


def claim(out: Path, default: bool):
    """Make --out this script's own folder, or refuse it: it must be new,
    empty, or marked by an earlier run. The default folder under .cache,
    which only this script writes, is also taken when it holds nothing but
    what a run writes (versions before the marker left it unmarked). Not a
    folder given by hand: a ~/work holding a clone named unarr would pass."""
    if out.exists() and not out.is_dir():
        raise SystemExit(f"--out {out} is not a folder")
    unmarked_own = default and {p.name for p in out.iterdir()} <= OWN if out.is_dir() else False
    if out.is_dir() and any(out.iterdir()) and not (out / MARKER).exists() and not unmarked_own:
        raise SystemExit(
            f"--out {out} holds files this script did not write; "
            "give a new or empty folder (the script deletes and rewrites names inside it)"
        )
    out.mkdir(parents=True, exist_ok=True)
    (out / MARKER).write_text("Results of scripts/compare-originals.py; the next run rewrites this folder.\n")


def isolate():
    """Run this script again in a network namespace of its own where only
    the loopback is up (`unshare -rn`, no privilege needed): neither side
    can reach the network, whatever the machine can. The child brings the
    loopback up itself; `ip` is not needed."""
    if os.environ.get("COMPARE_NETNS") == "1":
        import fcntl

        siocgifflags, siocsifflags, iff_up = 0x8913, 0x8914, 0x1
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            ifr = struct.pack("16sH14x", b"lo", 0)
            flags = struct.unpack("16sH14x", fcntl.ioctl(s, siocgifflags, ifr))[1]
            fcntl.ioctl(s, siocsifflags, struct.pack("16sH14x", b"lo", flags | iff_up))
        return
    unshare = shutil.which("unshare")
    if unshare is None:
        raise SystemExit("unshare not found: install util-linux, or pass --network (not comparable)")
    probe = subprocess.run([unshare, "-rn", "true"], capture_output=True, text=True)
    if probe.returncode != 0:
        raise SystemExit(
            f"unshare -rn failed ({probe.stderr.strip()}): user namespaces are off here; "
            "pass --network to measure with the network (not comparable)"
        )
    os.environ["COMPARE_NETNS"] = "1"
    os.execv(unshare, [unshare, "-rn", sys.executable, "-I", *sys.argv])


def log(*a):
    print("[compare]", *a, file=sys.stderr, flush=True)


def get(port: int, path: str, timeout: float = 600) -> bytes:
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", headers={"X-Api-Key": KEY})
    with OPENER.open(req, timeout=timeout) as r:
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
    except (FileNotFoundError, ProcessLookupError):
        pass
    return out


def cpu_seconds(pid: int) -> float:
    try:
        fields = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    except (FileNotFoundError, ProcessLookupError):
        return 0.0
    # After the command: state is fields[0]; utime, stime, cutime, cstime are 11 to 14.
    return sum(int(x) for x in fields[11:15]) / TICK


def die_with_parent():
    """In the child, before exec: SIGTERM when the script dies, even by
    SIGKILL, so no side keeps running in the namespace and loads the next
    comparison."""
    import ctypes

    pr_set_pdeathsig = 1
    ctypes.CDLL(None, use_errno=True).prctl(pr_set_pdeathsig, signal.SIGTERM)


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
            self.procs[label] = subprocess.Popen(
                # No terminal to read: an original that fails at start-up asks
                # to "press enter" and would wait for it instead of exiting.
                argv, stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT, env=self.env,
                preexec_fn=die_with_parent,
            )
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
            # Fine enough for Unarr's tenths of a second: a closed port
            # refuses at once, so a probe costs next to nothing.
            time.sleep(0.005)
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
        for label, proc in self.procs.items():
            if proc.poll() is not None:
                raise SystemExit(f"{label} exited during the measurement, see its stdout.log")
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
    # A side that crashed during its first start would leave the measured
    # one a half-prepared copy and the start-up work to do.
    for label, proc in side.procs.items():
        if proc.poll() is not None:
            raise SystemExit(f"{label} exited during its first start, see its stdout.log")
    side.stop()
    log(side.name, "measured start")
    first = side.start()
    # What actually runs, as each application reports it.
    versions = {port: json.loads(get(port, "/api/v3/system/status")).get("version", "?") for port in side.ports}
    samples = idle(side, idle_seconds)
    before = side.sample()
    peak = dict(before)
    done = threading.Event()

    died: list[BaseException] = []

    def watch():
        try:
            while not done.is_set():
                s = side.sample()
                for k in ("rss", "anon"):
                    peak[k] = max(peak[k], s[k])
                time.sleep(0.2)
        except SystemExit as e:
            # A side that died: kept for the main thread, which a thread's
            # SystemExit would not reach.
            died.append(e)

    watcher = threading.Thread(target=watch, daemon=True)
    watcher.start()
    t0 = time.time()
    try:
        latencies = reads()
        t1 = time.time()
    except Exception:
        # A read that failed because a side died says so, not with the
        # request's traceback.
        done.set()
        watcher.join()
        if died:
            raise died[0] from None
        raise
    finally:
        # Also when a read fails: the watcher would otherwise sample a
        # stopped side forever and keep the interpreter from exiting.
        done.set()
        watcher.join()
    if died:
        raise died[0]
    after = side.sample()
    # The sample right after the reads counts too: the watcher may have
    # slept through the last moments of them.
    for k in ("rss", "anon"):
        peak[k] = max(peak[k], after[k])
    tail = idle(side, tail_seconds)
    side.stop()
    return {
        "versions": versions,
        "first": first,
        "idle": samples,
        "reads": {"seconds": t1 - t0, "cpu": after["cpu"] - before["cpu"],
                  "peak": peak, "after": after},
        "latency": latencies,
        "tail": tail,
    }


def stage(library: str, root: Path) -> Path:
    """A fresh copy of the library for one side, at log level info. The
    originals default to debug, as the maintainer's install (the fixtures)
    does, and at debug they write a line for every API request while Unarr
    writes none: the reads would compare logging, not work."""
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
        config = dst / "config.xml"
        config.write_text(re.sub(r"<LogLevel>[^<]*</LogLevel>", "<LogLevel>info</LogLevel>", config.read_text()))
    return root


def pinned(script: str, var: str) -> str:
    m = re.search(rf'{var}="\$\{{{var}:-([^}}]+)\}}"', (REPO / "scripts" / script).read_text())
    return m.group(1) if m else "?"


def build() -> Path:
    """unarrd as the checkout builds it: cargo rebuilds what changed and does
    nothing otherwise, so the binary is the checkout's. Its path is the one
    cargo reports, wherever CARGO_TARGET_DIR or a cargo config put it."""
    log("cargo build --release --bin unarrd")
    done = subprocess.run(
        ["cargo", "build", "--release", "--bin", "unarrd", "--message-format=json-render-diagnostics"],
        cwd=REPO, stdout=subprocess.PIPE, text=True,
    )
    if done.returncode != 0:
        raise SystemExit("cargo build failed")
    for line in done.stdout.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") == "compiler-artifact" and msg.get("target", {}).get("name") == "unarrd" \
                and msg.get("executable"):
            return Path(msg["executable"])
    raise SystemExit("cargo build named no unarrd executable")


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
    # From the tail's first minute on, past the clean-up right after the reads.
    one = at(tail, 60)
    span = tail[-1]["t"] - one["t"]
    tail_cpu = (tail[-1]["cpu"] - one["cpu"]) / span * 3600 if one["t"] - tail[0]["t"] >= 55 and span >= 60 else None
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
        if not a or not b:
            return "n/a"
        return f"{a / b:.1f}× sooner" if a >= b else f"{b / a:.1f}× slower"

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
    def ran(port: int, app: str) -> str:
        """The version that ran, and the pinned one when they differ: make ui
        keeps a binary it already has, so a new pin needs .cache/<app>
        removed first."""
        v = reported.get(str(port), reported.get(port, meta[app]))
        where = meta.get("dirs", {}).get(app, f".cache/{app}/{app.capitalize()}")
        return v if v == meta[app] else f"{v} (**pinned {meta[app]}**: remove {where} and run make ui)"

    sonarr_v = ran(SONARR_PORT, "sonarr")
    radarr_v = ran(RADARR_PORT, "radarr")
    out = [
        f"Library `{meta['library']}`; Unarr `{meta['unarr']}`; Sonarr {sonarr_v}, Radarr {radarr_v}; "
        f"first start {meta['first']} s, idle {meta['idle']} s, tail {meta['tail']} s; "
        f".NET heap limit {'768 MiB' if meta['cap'] else 'none'}; "
        + ("**with the network, not comparable**; " if meta.get("network") else "")
        + ("**originals without ICU (invariant globalization)**; " if meta.get("invariant") else "")
        + ("**first start under 60 s, start-up tasks may be in the measured start**; "
           if meta["first"] < 60 else "")
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
    # Idle CPU is counted from minute 5 to the end: the tasks that run every
    # minute (monitored downloads) and every five (import list sync,
    # messaging clean-up) on both sides. The RSS sync, every 60 and
    # 100 minutes in these libraries, runs during the first start and not
    # in the window, on either side.
    ap.add_argument("--idle", type=int, default=900, help="idle seconds sampled after the measured start")
    ap.add_argument("--tail", type=int, default=300, help="idle seconds sampled after the reads")
    ap.add_argument("--no-cap", action="store_true", help="no .NET heap limit for the originals")
    ap.add_argument("--out", type=Path, help="default .cache/compare/<library>")
    ap.add_argument("--repeat", type=int, default=1,
                    help="runs per side, the order alternating between runs (default 1)")
    ap.add_argument("--unarrd", type=Path,
                    help="a prebuilt Unarr binary, named in the report by its path "
                         "(default: unarrd built from the checkout first, where cargo puts it)")
    ap.add_argument("--network", action="store_true",
                    help="do not isolate the network: both sides reach it, and the figures are not comparable")
    a = ap.parse_args()
    # A side left over by an earlier run lives in that run's namespace, out
    # of reach of a port check: look for the binaries among the processes.
    # The port check still catches a service of the host on 8989 or 7878,
    # which a --network run would collide with.
    if os.environ.get("COMPARE_NETNS") != "1":
        # Another comparison would load the machine during this one and, with
        # the same --out, delete the copies it measures.
        others = other_comparisons()
        if others:
            raise SystemExit(f"another comparison runs ({', '.join(others)}): wait for it or stop it")
        # Before the namespace, where cargo could not fetch a crate it lacks;
        # the path reaches the run in the namespace through the environment.
        if a.unarrd is None:
            os.environ["COMPARE_BUILT"] = str(build())
        # After the build: the binary this run will start is known.
        mine = a.unarrd or Path(os.environ["COMPARE_BUILT"])
        copy = (a.out or REPO / ".cache/compare" / a.library).resolve() / "bin" / "unarrd"
        left = leftovers([mine, REPO / "target/release/unarrd", copy])
        if left:
            raise SystemExit(f"a side is still running ({', '.join(left)}): stop it first")
        if a.network:
            for port in (SONARR_PORT, RADARR_PORT):
                if listening(port):
                    raise SystemExit(f"something already listens on {port}: stop it first")
    if not a.network:
        isolate()
    # A SIGTERM or SIGHUP ends the run through measure()'s finally, as
    # Ctrl-C does, so the side's processes are stopped.
    # A SIGHUP ignored by the caller (nohup) stays ignored, for the run and
    # for the sides it starts: a long run survives the end of the session.
    for sig in (signal.SIGTERM, signal.SIGHUP):
        if signal.getsignal(sig) is not signal.SIG_IGN:
            signal.signal(sig, lambda signum, _frame: sys.exit(128 + signum))

    # Absolute: the originals refuse a relative -data.
    out = (a.out or REPO / ".cache/compare" / a.library).resolve()
    # unarrd splits every --import-from value on commas, repeated or not:
    # refused now rather than after the originals' half hour.
    if "," in str(out):
        raise SystemExit(f"--out {out}: unarrd cannot import from a path with a comma")
    # The script deletes and rewrites fixed names under --out (unarr,
    # originals, bin/unarrd...): only in a folder it made itself, or an
    # empty one. --out ~ would otherwise delete a clone named ~/unarr.
    claim(out, default=a.out is None)
    # Where make ui put them: UNARR_CACHE moves .cache/<app> as in the
    # fixture scripts.
    cache = os.environ.get("UNARR_CACHE")
    sonarr = Path(cache) / "Sonarr/Sonarr" if cache else REPO / ".cache/sonarr/Sonarr/Sonarr"
    radarr = Path(cache) / "Radarr/Radarr" if cache else REPO / ".cache/radarr/Radarr/Radarr"
    for binary in (sonarr, radarr):
        if not binary.exists():
            raise SystemExit(f"{binary} missing: run make ui first")
    if a.unarrd is not None and not a.unarrd.exists():
        raise SystemExit(f"{a.unarrd} missing")
    # Built by build() above, before the namespace, when not given. Copied
    # into --out: a rebuild or a checkout during the hours of a run would
    # otherwise change the binary under the commit the report names.
    built = a.unarrd.resolve() if a.unarrd is not None else Path(os.environ["COMPARE_BUILT"])
    unarrd = out / "bin" / "unarrd"
    unarrd.parent.mkdir(parents=True, exist_ok=True)
    # --unarrd may name the copy an earlier run left here, to measure it again.
    if built != unarrd.resolve():
        shutil.copy2(built, unarrd)
    digest = hashlib.sha256(unarrd.read_bytes()).hexdigest()[:12]
    meta = {
        "library": a.library,
        # The checkout's commit for the binary built from it; a binary given
        # with --unarrd was built from code this script cannot know, so it
        # is named by its path. Either way the hash of the copy that runs.
        "unarr": (str(built) if a.unarrd is not None else subprocess.run(
            ["git", "-C", str(REPO), "describe", "--always", "--dirty"], capture_output=True, text=True
        ).stdout.strip()) + f", sha256 {digest}",
        # Where make ui put each original, for the hint when its version is
        # not the pinned one (UNARR_CACHE moves it).
        # The extracted folder only (make ui extracts again when it is
        # missing), not the cache that also holds the other original.
        "dirs": {"sonarr": str(sonarr.parent), "radarr": str(radarr.parent)},
        "sonarr": pinned("sonarr-fixture.sh", "SONARR_VERSION"),
        "radarr": pinned("radarr-fixture.sh", "RADARR_VERSION"),
        "first": a.first, "idle": a.idle, "tail": a.tail, "cap": not a.no_cap,
        "network": a.network,
        "date": time.strftime("%Y-%m-%d"),
    }
    env = dict(ENV)
    if without_icu():
        env.setdefault("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "1")
        log("libicu not found: the originals run with invariant globalization")
    # Without ICU data mapped the originals take less memory: a report from
    # such a run says so, not comparable with one made with ICU.
    # As .NET reads the switch: only 1 and true, in any case, turn it on.
    meta["invariant"] = env.get("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "").strip().lower() in ("1", "true")
    if not a.no_cap:
        env["DOTNET_GCHeapHardLimit"] = "0x30000000"

    if a.repeat < 1:
        raise SystemExit("--repeat must be at least 1")
    meta["repeat"] = a.repeat
    # Both schedulers run their overdue tasks about 30 s after a start: a
    # shorter first start leaves them, or their remains, to the measured one.
    if a.first < 60:
        log(f"warning: --first {a.first} is under 60 s; the overdue tasks may run in the measured start")

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
            # One --import-from per folder; a comma in --out is refused above,
            # since unarrd splits each value on commas.
            ("unarr", [str(unarrd), "--data", str(data), "--personalities", "sonarr,radarr",
                       "--import-from", str(lib / "sonarr"), "--import-from", str(lib / "radarr")], data),
        ], [SONARR_PORT, RADARR_PORT], ENV)

    # A previous series' files would otherwise sit beside this one's until
    # it ends, and an interrupted run would leave the old report.md.
    for name in ("results.json", "report.md"):
        (out / name).unlink(missing_ok=True)
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
            # Kept after every side: an interrupted series keeps what it measured.
            (out / "results.json").write_text(json.dumps(res, indent=1))

    (out / "results.json").write_text(json.dumps(res, indent=1))
    text = report(res, meta)
    (out / "report.md").write_text(text)
    print(text)
    log("written", out / "results.json", "and", out / "report.md")


if __name__ == "__main__":
    main()
