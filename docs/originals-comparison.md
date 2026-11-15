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
  - the time to the first answer on both ports, each polled every 5 ms in
    a thread of its own;
  - fifteen idle minutes, sampled every five seconds;
  - the same reads over both APIs: about 220 requests, the list of each
    app, history, wanted, calendar, queue and twenty series' episodes,
    after unmeasured passes over them: the first call of a route on a
    fresh .NET process pays its JIT compilation, and a route called often
    is compiled again a pass or two later, which a running install pays
    once. The same number of passes on both sides (`--warmup`, default 5:
    the originals' CPU per pass settles by the fourth or fifth), so the
    heaps compared afterwards have served the same requests; the report
    says when a side's last two passes still differ by more than 10 %, and
    `results.json` keeps each pass's CPU (`warmup_cpu`);
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
  already has, so after a new pin remove the folder it extracted the
  original into first (`.cache/sonarr/Sonarr` or `.cache/radarr/Radarr`,
  under `UNARR_CACHE` when it is set), as the report says.
- **RAM** is the RSS and the anonymous memory (the heaps) of
  `/proc/<pid>/smaps_rollup`, summed over a side's processes.
- **CPU** is `utime + stime` from `/proc/<pid>/stat`, plus the children
  a process has waited for: Unarr's `ffprobe` runs count. Their memory
  does not: it is sampled on the side's own processes only. Idle CPU is
  counted from the fifth minute of the window, once the start-up work is
  done, to the end of the window: the tasks both sides run on a one-minute
  interval (the monitored downloads, every 90 s in practice: a run ends
  just after the scheduler tick that pushed it, so it is not yet due on
  the tick a minute later) and on a five-minute one (the import list
  sync, the messaging clean-up). The fixtures' download clients point at
  hosts that do not exist: once the fifteen minutes of grace after a start
  are over, which is when the idle window ends, both sides back them off
  (five minutes, then fifteen) and stop asking them, so the tail idles on
  less work than the window. The RSS sync, every 60 minutes for Sonarr and
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
  even one whose file has since been replaced, or the copy of Unarr any
  earlier run left under its `--out` (a side left over in an earlier run's
  namespace); with `--network` it also refuses when
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
- **Output.** `--out` must be a new or empty folder, or one an earlier
  run marked (`.compare-originals`): the script deletes and rewrites names
  inside it, so it refuses any other. The default folder under `.cache`,
  which only the script writes, is taken unmarked too when it holds
  nothing but what a run writes. `.cache/compare/<library>/report.md`
  holds the tables below,
  `results.json` every sample. A run first removes the previous series'
  two files; `results.json` is written after each side of each run, so an
  interrupted series keeps what it measured, and `report.md` only at the
  end. A run where the originals had no libicu says so in the report: they
  take less memory without ICU data.

## Results

Each library has the tables of its latest `report.md`, as the script
writes them. Measured on 2026-11-15 in the cloud session (x86-64, four
cores, 16 GB), one run per library: the real library at `3e81473`, with
the scheduler on the originals' cadence (`core/scheduler-originals-cadence`),
the scaled one at `3e51a78`, main with the calendar and wanted-list reads
of `radarr/calendar-date-range-query`.

### Real library (38 series, 193 movies)

Library `real`; Unarr `3e81473, sha256 fb90444313f0`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; 5 warm-up passes; 2026-11-15.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 2.36 s | 0.24 s | 10.0× sooner |
| Idle, at the end of the idle window: RSS | 479 MiB | 28 MiB | 94 % |
| Same, anonymous memory | 266 MiB | 10 MiB | 96 % |
| Idle CPU, from minute 5 of the window | 18.1 s/h | 1.1 s/h | 94 % |
| The reads: CPU | 2.83 s | 1.54 s | 46 % |
| The reads: time | 2.77 s | 1.70 s | 39 % |
| Peak anonymous memory during the reads | 384 MiB | 45 MiB | 88 % |
| Anonymous memory at the end of the tail | 394 MiB | 37 MiB | 91 % |
| Idle CPU in the tail, from its first minute | 9.3 s/h | 0.7 s/h | 92 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 8.0 / 12.8 | 3.3 / 4.0 |
| Sonarr /api/v3/history | 25.5 / 36.1 | 6.9 / 9.1 |
| Sonarr /api/v3/wanted/missing | 4.5 / 42.7 | 3.3 / 4.9 |
| Sonarr /api/v3/calendar | 4.2 / 5.8 | 3.0 / 4.2 |
| Sonarr /api/v3/queue | 1.7 / 2.3 | 1.9 / 2.2 |
| Radarr /api/v3/movie | 19.7 / 60.3 | 19.9 / 24.4 |
| Radarr /api/v3/history | 21.3 / 26.7 | 23.8 / 32.4 |
| Radarr /api/v3/wanted/missing | 19.4 / 40.6 | 5.2 / 7.9 |
| Radarr /api/v3/calendar | 13.1 / 16.4 | 6.7 / 7.6 |
| Radarr /api/v3/queue | 1.5 / 2.5 | 3.4 / 4.0 |
| Sonarr /api/v3/episode | 5.1 / 36.4 | 3.5 / 13.9 |

### Scaled library (950 series, 2,123 movies)

Library `scaled`; Unarr `3e51a78, sha256 c143c71589ad`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; 5 warm-up passes; 2026-11-15.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 2.06 s | 0.33 s | 6.2× sooner |
| Idle, at the end of the idle window: RSS | 532 MiB | 43 MiB | 92 % |
| Same, anonymous memory | 316 MiB | 26 MiB | 92 % |
| Idle CPU, from minute 5 of the window | 17.9 s/h | 2.6 s/h | 86 % |
| The reads: CPU | 20.17 s | 9.97 s | 51 % |
| The reads: time | 18.62 s | 10.30 s | 45 % |
| Peak anonymous memory during the reads | 561 MiB | 186 MiB | 67 % |
| Anonymous memory at the end of the tail | 497 MiB | 46 MiB | 91 % |
| Idle CPU in the tail, from its first minute | 9.9 s/h | 2.2 s/h | 77 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 210.7 / 292.0 | 49.2 / 62.1 |
| Sonarr /api/v3/history | 60.2 / 78.9 | 25.6 / 28.8 |
| Sonarr /api/v3/wanted/missing | 20.8 / 23.4 | 29.6 / 31.0 |
| Sonarr /api/v3/calendar | 78.9 / 107.9 | 23.9 / 28.1 |
| Sonarr /api/v3/queue | 1.5 / 2.7 | 2.0 / 2.4 |
| Radarr /api/v3/movie | 311.7 / 419.0 | 226.8 / 275.9 |
| Radarr /api/v3/history | 42.8 / 92.6 | 45.4 / 61.2 |
| Radarr /api/v3/wanted/missing | 33.9 / 42.2 | 22.9 / 32.4 |
| Radarr /api/v3/calendar | 132.8 / 170.4 | 53.6 / 68.5 |
| Radarr /api/v3/queue | 1.6 / 2.4 | 17.2 / 20.6 |
| Sonarr /api/v3/episode | 5.7 / 53.9 | 3.3 / 13.9 |

Where Unarr is slower than the original, in these tables (whoever updates
the tables updates this list from them, as CLAUDE.md says):

- Radarr `/queue`: 3.4 ms against 1.5 on the real library, 17.2 against
  1.6 on the scaled one, with an empty queue.
- Sonarr `/wanted/missing` on the scaled library: 29.6 ms against 20.8.
- Radarr `/history`: 23.8 ms against 21.3 on the real library, 45.4
  against 42.8 on the scaled one.
- Sonarr `/queue`: 1.9 ms against 1.7 on the real library, 2.0 against
  1.5 on the scaled one.
- Radarr `/movie` on the real library: level, 19.9 ms against 19.7.

Radarr's `/calendar` and `/wanted/missing`, slower than the original
before `radarr/calendar-date-range-query`, now ask SQLite for the movies
they show: 6.7 ms against 13.1 and 5.2 against 19.4 on the real library,
53.6 against 132.8 and 22.9 against 33.9 on the scaled one. `/movie`
still reads and encodes the whole library, as the original does.

### In production, before the switch

The homelab NAS (QNAP TS-435XeU, ARM64, 16 GB) measured the originals in
their 1 GiB containers before Unarr replaced them:

- **2026-09-27, cgroup counters over 7.6 hours.** Radarr: 59 MiB RSS plus
  90 MiB of swap, CPU 0.7 % of one core on average. Sonarr had just been
  restarted: 160 MiB after 0.2 hours.
- **2026-10-01, `docker stats`** (cache excluded), two hours after the
  power-on: Sonarr 214 MiB, Radarr 260 MiB.

Those 474 MiB for both are the same order as the 479 MiB of RSS the real
library gives the originals here. The saving on the NAS is about 450 MiB
of RAM.

## History

One row per run, newest first: the library, the Unarr commit, idle RSS and
idle CPU of each side.

| Date | Library | Unarr | Runs | Originals: idle RSS / CPU | Unarr: idle RSS / CPU | Note |
|---|---|---|--:|--:|--:|---|
| 2026-11-15 | real | `3e81473` | 1 | 479 MiB / 18.1 s/h | 28 MiB / 1.1 s/h | the download monitor every 90 s, as the originals' |
| 2026-11-15 | scaled | `3e51a78` | 1 | 532 MiB / 17.9 s/h | 43 MiB / 2.6 s/h | calendar and wanted lists read in SQL |
| 2026-11-15 | real | `3e51a78` | 1 | 483 MiB / 19.2 s/h | 28 MiB / 1.6 s/h | calendar and wanted lists read in SQL; Unarr's warm-up flagged, noise |
| 2026-11-14 | real | `ae1283b` | 1 | 496 MiB / 18.4 s/h | 28 MiB / 1.6 s/h | prototype; debug logs |
| 2026-11-14 | scaled | `ae1283b` | 1 | 849 MiB / 28.5 s/h | 45 MiB / 1.8 s/h | prototype; debug logs, no heap limit |
