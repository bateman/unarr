# Unarr beside the originals

What Unarr saves against the Sonarr and Radarr it replaces, in RAM, CPU
and latency, measured on the same machine and the same library. The tables
below are kept current: **after each substantial change** (a milestone, a
change made for speed or memory, a new pinned version of the originals),
run the comparison again and update this file in the same branch.

## How it is measured

`scripts/compare-originals.py`, through `make compare-originals`:

    make ui                                  # the originals, pinned in scripts/*-fixture.sh
    make compare-originals                   # the real library, about an hour
    make scale-fixture
    make compare-originals LIBRARY=scaled    # 950 series and 2,123 movies
    make compare-originals REPEAT=3          # three runs per side, about three hours

- **The sides.** The originals are the official Sonarr and Radarr from
  their release tarballs, two processes. Unarr is `target/release/unarrd`
  (or `--unarrd`) with both personalities in one process. The two sides
  run one at a time, each on a fresh copy of the library. The report names
  the checkout's commit, and says so when the binary is older than a source
  it is built from (the crates unarrd links, their tests aside); a binary
  given with `--unarrd` is named by its path.
- **The two starts.** Each side starts once and runs five minutes, not
  measured: migrations, Unarr's import, the overdue scheduled tasks. Then
  it starts again and is measured:
  - the time to the first answer on both ports;
  - fifteen idle minutes, sampled every five seconds;
  - the same reads over both APIs: about 220 requests, the list of each
    app, history, wanted, calendar, queue and twenty series' episodes;
  - five more idle minutes.
- **No network.** The script runs itself again in a network namespace of
  its own (`unshare -rn`, no privilege needed) where only the loopback is
  up, so neither side reaches the network, whatever the machine can: RSS
  sync, metadata refreshes and update checks fail at once on both. Where
  user namespaces are off, the script stops. `--network` measures with
  the network; the report then says the figures are not comparable.
- **Latency** is measured by the script's own client, Python's `urllib`,
  with a new connection for each request, the same for both sides.
- **Environment.** The caller's proxy variables, `RUST_LOG`, the
  `UNARR_*` settings and the `DOTNET_*` and `COMPlus_*` knobs are not passed to the processes
  measured, except `DOTNET_SYSTEM_GLOBALIZATION_INVARIANT`: the script
  sets it itself where libicu is missing, as the fixture scripts do,
  since .NET does not start otherwise. The script's own requests ignore
  the caller's proxy. The originals are found where `make ui` put them,
  under `UNARR_CACHE` when it is set. The report gives the versions the
  applications themselves report, and marks one that differs from the
  version pinned in `scripts/*-fixture.sh`: `make ui` keeps a binary it
  already has, so after a new pin remove `.cache/sonarr` or `.cache/radarr`
  first.
- **RAM** is the RSS and the anonymous memory (the heaps) of
  `/proc/<pid>/smaps_rollup`, summed over a side's processes.
- **CPU** is `utime + stime` from `/proc/<pid>/stat`, plus the children
  a process has waited for: Unarr's `ffprobe` runs count. Their memory
  does not: it is sampled on the side's own processes only. Idle CPU is
  counted from the fifth minute of the window, once the start-up work is
  done. Idle CPU in the tail is counted from its first minute, past the
  clean-up right after the reads. With `--idle` under six minutes, or a
  tail under two, the report gives n/a.
- **Log level.** Each copy of the library is set to `LogLevel` `info`,
  the default of an install. The fixtures ask for `debug`, at which the
  originals write a line for every API request and Unarr writes none, so
  the reads would cost the originals logging that Unarr does not do.
- **Stopping.** A run stopped by Ctrl-C, SIGTERM or SIGHUP, or by a
  read that fails, stops the side it was measuring and exits. The sides
  also get SIGTERM if the script dies by SIGKILL. A side process that dies
  during the measurement stops the run. The script refuses to start while
  a process of the three binaries runs (a side left over in another run's
  namespace, or another comparison); with `--network` it also refuses
  when anything listens on 8989 or 7878.
- **Heap limit.** The originals run with `DOTNET_GCHeapHardLimit` at
  768 MiB: that is what .NET takes from a 1 GiB container such as the
  homelab's.
- **The libraries.**
  - `real` is the anonymised fixtures, the maintainer's own library: 38
    series, 193 movies.
  - `scaled` is `make scale-fixture`: 950 series, 25,300 episodes, 2,123
    movies, 50,000 history rows each.
- **Repetitions.** `--repeat N` (`REPEAT=N`) measures each side N times.
  The order alternates between runs, originals first in odd runs and Unarr
  first in even ones, so a drift of the machine favours neither side. A
  cell then reads median (lowest–highest), and *Less* compares the
  medians; where Unarr takes more, it reads "N % more", and a later first
  answer reads "N× slower". For latency, the cell is the median of the runs' p50s, with
  the highest single call after the slash. One run is enough for a
  routine update; numbers meant for a paper or a release note want three
  or more.
- **Output.** `.cache/compare/<library>/report.md` holds the tables below,
  `results.json` every sample. `results.json` is written after each side
  of each run, so an interrupted series keeps what it measured.

## Results

Measured on 2026-11-14 in the cloud session (x86-64, four cores, 16 GB),
Unarr at `ae1283b`, Sonarr 4.0.20.3014, Radarr 6.4.4.10685. This first run
used the prototype of `compare-originals.py`. Its reads were the same
except Radarr's `wanted/missing`, and the scaled originals ran without the
heap limit, so their peak is what an unbounded .NET heap keeps.

| | Originals, real | Unarr, real | Originals, scaled | Unarr, scaled |
|---|--:|--:|--:|--:|
| First answer on both ports | 1.5 s | 0.21 s | 1.7 s | 0.21 s |
| Idle after 15 minutes, RSS | 496 MiB | 28 MiB | 837 MiB | 45 MiB |
| Same, anonymous memory | 276 MiB | 10 MiB | 609 MiB | 24 MiB |
| Idle CPU, minutes 5 to 15 | 18 s/h | 1.6 s/h | 16 s/h | 2 s/h |
| The reads: CPU | 8.0 s | 1.6 s | 24.8 s | 10.3 s |
| The reads: time | 6.3 s | 1.8 s | 20.7 s | 10.7 s |
| Peak anonymous memory during the reads | 370 MiB | 39 MiB | 1,842 MiB | 124 MiB |
| Anonymous memory five minutes later | 395 MiB | 36 MiB | 1,835 MiB | 47 MiB |

On the real library Unarr takes 94 % less RAM at rest (468 MiB less RSS)
and 91 % less idle CPU.

Latency, p50 in ms (20 calls each; `episode` is twenty series, one call each):

| Route | Originals, real | Unarr, real | Originals, scaled | Unarr, scaled |
|---|--:|--:|--:|--:|
| Sonarr `/series` | 12.5 | 3.1 | 241 | 52 |
| Sonarr `/episode?seriesId=` | 11.0 | 2.9 | 11.5 | 4.8 |
| Sonarr `/history`, a page | 34.9 | 6.2 | 73.5 | 23.2 |
| Sonarr `/wanted/missing`, a page | 4.5 | 3.2 | 13.1 | 24.0 |
| Sonarr `/calendar`, a month | 4.5 | 3.2 | 64.3 | 20.9 |
| Sonarr `/queue` | 2.0 | 1.8 | 1.5 | 1.5 |
| Radarr `/movie` | 30.9 | 17.3 | 284 | 215 |
| Radarr `/history`, a page | 33.8 | 22.1 | 44.0 | 38.6 |
| Radarr `/calendar`, a month | 12.0 | 13.7 | 105 | 117 |
| Radarr `/queue` | 1.9 | 3.8 | 1.8 | 17.8 |

Where Unarr is slower than the original:

- Sonarr `/wanted/missing` on the scaled library: 24 ms against 13.
- Radarr `/calendar`: 13.7 ms against 12.0 on the real library, 117
  against 105 on the scaled one.
- Radarr `/queue`: twice the original on the real library, ten times on
  the scaled one, with an empty queue.

The movie routes gain far less over Radarr than the series routes gain
over Sonarr. They read the whole library to answer a few rows.
`/calendar`, the iCal feed and the wanted lists stop doing so in
`radarr/calendar-date-range-query`.

### In production, before the switch

The homelab NAS (QNAP TS-435XeU, ARM64, 16 GB) measured the originals in
their 1 GiB containers before Unarr replaced them:

- **2026-09-27, cgroup counters over 7.6 hours.** Radarr: 59 MiB RSS plus
  90 MiB of swap, CPU 0.7 % of one core on average. Sonarr had just been
  restarted: 160 MiB after 0.2 hours.
- **2026-10-01, `docker stats`** (cache excluded), two hours after the
  power-on: Sonarr 214 MiB, Radarr 260 MiB.

Those 474 MiB for both are the same order as the 496 MiB of RSS the real
library gives the originals here. The saving on the NAS is about 450 MiB
of RAM.

## History

One row per run, newest first: the library, the Unarr commit, idle RSS and
idle CPU of each side.

| Date | Library | Unarr | Runs | Originals: idle RSS / CPU | Unarr: idle RSS / CPU | Note |
|---|---|---|--:|--:|--:|---|
| 2026-11-14 | real | `ae1283b` | 1 | 496 MiB / 18 s/h | 28 MiB / 1.6 s/h | prototype of the script |
| 2026-11-14 | scaled | `ae1283b` | 1 | 837 MiB / 16 s/h | 45 MiB / 2 s/h | prototype; originals without heap limit |
