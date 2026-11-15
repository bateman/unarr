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
  their release tarballs, two processes. Unarr is `unarrd` with both
  personalities in one process; the script has cargo build it from the
  checkout first (nothing to do when it is current), takes the binary
  cargo reports, wherever `CARGO_TARGET_DIR` puts it, and copies it into
  `--out` before the run: a rebuild during the hours of a run cannot
  change what runs. The report names the commit and the hash of that
  copy. A prebuilt binary given with `--unarrd` is named by its path and
  hash. The two sides run one at a time, each on a fresh copy of the
  library.
- **The two starts.** Each side starts once and runs five minutes, not
  measured: migrations, Unarr's import, the overdue scheduled tasks. Then
  it starts again and is measured:
  - the time to the first answer on both ports, polled every 5 ms;
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
  `UNARR_*` settings, the originals' own `SONARR__*` and `RADARR__*`
  settings (a log level, a key, a port) and the `DOTNET_*` and `COMPlus_*`
  knobs are not passed to the processes measured, except `DOTNET_SYSTEM_GLOBALIZATION_INVARIANT`, which passes
  on as the caller set it, and which the script sets itself where libicu
  is missing, as the fixture scripts do, since .NET does not start
  otherwise. The script's own requests ignore
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
  done, to the end of the window: the tasks both sides run every minute
  (the monitored downloads). The RSS sync, every 60 minutes for Sonarr and
  100 for Radarr in these libraries, runs during the first start and falls
  in neither side's window; with no network it would fail at once anyway.
  Idle CPU in the tail is counted from its first minute, past the
  clean-up right after the reads. With `--idle` under six minutes, or a
  tail under two, the report gives n/a.
- **Log level.** Each copy of the library is set to `LogLevel` `info`.
  The originals default to `debug`, and the maintainer's install, where
  the fixtures come from, runs at `debug` too; at that level the originals
  write a line for every API request and Unarr writes none, so the reads
  would compare logging rather than work. An install left at `debug`
  therefore saves more than these figures say.
- **Stopping.** A run stopped by Ctrl-C, SIGTERM or SIGHUP, or by a
  read that fails, stops the side it was measuring and exits; under
  `nohup` SIGHUP stays ignored and the run goes on. The sides also get
  SIGTERM if the script dies by SIGKILL. A side process that dies during
  the measurement stops the run. The script refuses to start while
  another comparison runs, or while a process of the originals, of
  `target/release/unarrd` or of the binary it is about to run (or its copy
  under `--out`) is alive,
  even one whose file has since been replaced (a side left over in an
  earlier run's namespace); with `--network` it also refuses when
  anything listens on 8989 or 7878.
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
  `results.json` every sample. A run first removes the previous series'
  two files; `results.json` is written after each side of each run, so an
  interrupted series keeps what it measured, and `report.md` only at the
  end. A run where the originals had no libicu says so in the report: they
  take less memory without ICU data.

## Results

Each library has the tables of its latest `report.md`, as the script
writes them. Measured on 2026-11-14 in the cloud session (x86-64, four
cores, 16 GB) with the script's prototype, and **not comparable with the
runs to come**: both libraries logged at debug level, which costs the
originals a line per API request and Unarr none, the idle window was 15
minutes, Radarr's `wanted/missing` was not read, and the scaled originals
ran without the heap limit and a first start of only 60 seconds.

### Real library (38 series, 193 movies)

Library `real`; Unarr `ae1283b`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; 2026-11-14.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 1.54 s | 0.21 s | 7.5× sooner |
| Idle, at the end of the idle window: RSS | 496 MiB | 28 MiB | 94 % |
| Same, anonymous memory | 276 MiB | 10 MiB | 96 % |
| Idle CPU, from minute 5 of the window | 18.4 s/h | 1.6 s/h | 91 % |
| The reads: CPU | 8.03 s | 1.62 s | 80 % |
| The reads: time | 6.33 s | 1.82 s | 71 % |
| Peak anonymous memory during the reads | 370 MiB | 39 MiB | 89 % |
| Anonymous memory at the end of the tail | 395 MiB | 36 MiB | 91 % |
| Idle CPU in the tail, from its first minute | 8.5 s/h | 1.0 s/h | 88 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 12.5 / 17.6 | 3.1 / 3.7 |
| Sonarr /api/v3/history | 34.7 / 802.7 | 6.2 / 28.2 |
| Sonarr /api/v3/wanted/missing | 4.5 / 17.1 | 3.2 / 4.3 |
| Sonarr /api/v3/calendar | 4.5 / 11.1 | 3.1 / 3.6 |
| Sonarr /api/v3/queue | 2.0 / 27.2 | 1.7 / 2.2 |
| Radarr /api/v3/movie | 30.7 / 224.8 | 17.2 / 25.6 |
| Radarr /api/v3/history | 33.5 / 1129.9 | 22.1 / 96.5 |
| Radarr /api/v3/calendar | 11.9 / 41.8 | 13.7 / 19.5 |
| Radarr /api/v3/queue | 1.9 / 46.3 | 3.8 / 4.3 |
| Sonarr /api/v3/episode | 9.7 / 389.3 | 2.9 / 38.1 |

### Scaled library (950 series, 2,123 movies)

Library `scaled`; Unarr `ae1283b`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 60 s, idle 900 s, tail 300 s; .NET heap limit none; 2026-11-14.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 1.69 s | 0.21 s | 8.3× sooner |
| Idle, at the end of the idle window: RSS | 849 MiB | 45 MiB | 95 % |
| Same, anonymous memory | 621 MiB | 24 MiB | 96 % |
| Idle CPU, from minute 5 of the window | 28.5 s/h | 1.8 s/h | 94 % |
| The reads: CPU | 24.82 s | 10.29 s | 59 % |
| The reads: time | 20.66 s | 10.74 s | 48 % |
| Peak anonymous memory during the reads | 1842 MiB | 124 MiB | 93 % |
| Anonymous memory at the end of the tail | 1835 MiB | 47 MiB | 97 % |
| Idle CPU in the tail, from its first minute | 10.3 s/h | 1.3 s/h | 87 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 240.0 / 635.2 | 51.5 / 60.8 |
| Sonarr /api/v3/history | 72.5 / 858.7 | 23.1 / 87.0 |
| Sonarr /api/v3/wanted/missing | 13.1 / 43.9 | 23.8 / 28.3 |
| Sonarr /api/v3/calendar | 64.2 / 102.1 | 20.9 / 23.0 |
| Sonarr /api/v3/queue | 1.5 / 48.1 | 1.5 / 1.9 |
| Radarr /api/v3/movie | 280.8 / 529.2 | 214.5 / 325.4 |
| Radarr /api/v3/history | 43.9 / 1175.2 | 38.5 / 118.3 |
| Radarr /api/v3/calendar | 104.5 / 158.8 | 113.5 / 141.8 |
| Radarr /api/v3/queue | 1.8 / 54.3 | 17.4 / 21.2 |
| Sonarr /api/v3/episode | 10.9 / 424.4 | 4.4 / 31.3 |

Where Unarr is slower than the original:

- Sonarr `/wanted/missing` on the scaled library: 24 ms against 13.
- Radarr `/calendar`: 13.7 ms against 11.9 on the real library, 114
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
| 2026-11-14 | real | `ae1283b` | 1 | 496 MiB / 18.4 s/h | 28 MiB / 1.6 s/h | prototype; debug logs, 15-minute idle |
| 2026-11-14 | scaled | `ae1283b` | 1 | 849 MiB / 28.5 s/h | 45 MiB / 1.8 s/h | prototype; debug logs, 15-minute idle, no heap limit |
