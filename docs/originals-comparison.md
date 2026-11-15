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
cores, 16 GB), one run per library, at `51fbc56`
(`sonarr/scoped-series-reads`): Sonarr's wanted lists and calendar read
only the series they show, and one series' statistics go through the
`series_id` index. That run predates the scheduler on the originals'
cadence (`core/scheduler-originals-cadence`, measured alone at `3e81473`
in the History table): with it, Unarr's idle CPU on the real library is
1.1 s/h instead of these tables' 1.3.

### Real library (38 series, 193 movies)

Library `real`; Unarr `51fbc56, sha256 739d2b600ac9`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; 5 warm-up passes; 2026-11-15.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 1.68 s | 0.18 s | 9.3× sooner |
| Idle, at the end of the idle window: RSS | 479 MiB | 27 MiB | 94 % |
| Same, anonymous memory | 265 MiB | 10 MiB | 96 % |
| Idle CPU, from minute 5 of the window | 18.5 s/h | 1.3 s/h | 93 % |
| The reads: CPU | 3.05 s | 1.55 s | 49 % |
| The reads: time | 3.01 s | 1.70 s | 44 % |
| Peak anonymous memory during the reads | 385 MiB | 44 MiB | 88 % |
| Anonymous memory at the end of the tail | 398 MiB | 37 MiB | 91 % |
| Idle CPU in the tail, from its first minute | 8.7 s/h | 0.9 s/h | 90 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 10.1 / 15.2 | 3.6 / 4.3 |
| Sonarr /api/v3/history | 28.7 / 45.0 | 7.8 / 8.9 |
| Sonarr /api/v3/wanted/missing | 5.4 / 10.6 | 2.8 / 3.1 |
| Sonarr /api/v3/calendar | 4.9 / 6.3 | 1.9 / 2.3 |
| Sonarr /api/v3/queue | 1.7 / 2.0 | 1.9 / 2.1 |
| Radarr /api/v3/movie | 21.6 / 29.0 | 23.6 / 28.6 |
| Radarr /api/v3/history | 24.8 / 58.2 | 22.5 / 25.9 |
| Radarr /api/v3/wanted/missing | 21.8 / 27.5 | 5.9 / 7.6 |
| Radarr /api/v3/calendar | 14.0 / 17.5 | 7.4 / 10.7 |
| Radarr /api/v3/queue | 1.5 / 2.4 | 3.6 / 4.5 |
| Sonarr /api/v3/episode | 6.2 / 55.3 | 2.8 / 11.0 |

### Scaled library (950 series, 2,123 movies)

Library `scaled`; Unarr `51fbc56, sha256 069d6e895f66`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; 5 warm-up passes; 2026-11-15.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 2.18 s | 0.27 s | 8.1× sooner |
| Idle, at the end of the idle window: RSS | 538 MiB | 43 MiB | 92 % |
| Same, anonymous memory | 322 MiB | 26 MiB | 92 % |
| Idle CPU, from minute 5 of the window | 16.3 s/h | 2.5 s/h | 85 % |
| The reads: CPU | 17.51 s | 8.43 s | 52 % |
| The reads: time | 16.12 s | 8.76 s | 46 % |
| Peak anonymous memory during the reads | 523 MiB | 140 MiB | 73 % |
| Anonymous memory at the end of the tail | 478 MiB | 45 MiB | 91 % |
| Idle CPU in the tail, from its first minute | 10.0 s/h | 1.2 s/h | 88 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 200.6 / 263.6 | 49.1 / 58.1 |
| Sonarr /api/v3/history | 59.9 / 76.3 | 27.0 / 30.5 |
| Sonarr /api/v3/wanted/missing | 13.4 / 17.8 | 11.3 / 27.4 |
| Sonarr /api/v3/calendar | 66.2 / 115.7 | 6.8 / 7.9 |
| Sonarr /api/v3/queue | 1.5 / 3.8 | 1.6 / 1.9 |
| Radarr /api/v3/movie | 278.0 / 346.8 | 207.2 / 251.1 |
| Radarr /api/v3/history | 31.2 / 42.4 | 40.2 / 52.3 |
| Radarr /api/v3/wanted/missing | 22.0 / 36.1 | 20.1 / 21.2 |
| Radarr /api/v3/calendar | 85.7 / 135.6 | 43.1 / 57.6 |
| Radarr /api/v3/queue | 1.4 / 2.3 | 15.8 / 18.1 |
| Sonarr /api/v3/episode | 5.4 / 33.4 | 3.3 / 14.0 |

Where Unarr is slower than the original, in these tables (whoever updates
the tables updates this list from them, as CLAUDE.md says):

- Radarr `/queue`: 3.6 ms against 1.5 on the real library, 15.8 against
  1.4 on the scaled one, with an empty queue.
- Radarr `/history` on the scaled library: 40.2 ms against 31.2.
- Radarr `/movie` on the real library: 23.6 ms against 21.6.
- Sonarr `/queue`: 1.9 ms against 1.7 on the real library, 1.6 against
  1.5 on the scaled one, an empty queue either way.

Sonarr's `/wanted/missing`, slower than the original on the scaled
library before `sonarr/scoped-series-reads` (29.6 ms against 20.8), now
reads only the page's series: 11.3 ms against 13.4.

Radarr's `/calendar` and `/wanted/missing`, slower than the original
before `radarr/calendar-date-range-query`, now ask SQLite for the movies
they show: 7.4 ms against 14.0 and 5.9 against 21.8 on the real library,
43.1 against 85.7 and 20.1 against 22.0 on the scaled one. `/movie`
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
| 2026-11-15 | scaled | `d772fd6` | 1 | 535 MiB / 15.9 s/h | 44 MiB / 2.5 s/h | Radarr reads only the shown movies' translations; before the Sonarr reads and the cadence |
| 2026-11-15 | real | `d772fd6` | 1 | 484 MiB / 18.4 s/h | 28 MiB / 1.6 s/h | Radarr reads only the shown movies' translations; before the Sonarr reads and the cadence |
| 2026-11-15 | scaled | `51fbc56` | 1 | 538 MiB / 16.3 s/h | 43 MiB / 2.5 s/h | Sonarr reads only the series it shows |
| 2026-11-15 | real | `51fbc56` | 1 | 479 MiB / 18.5 s/h | 27 MiB / 1.3 s/h | Sonarr reads only the series it shows |
| 2026-11-15 | real | `3e81473` | 1 | 479 MiB / 18.1 s/h | 28 MiB / 1.1 s/h | the download monitor every 90 s, as the originals' |
| 2026-11-15 | scaled | `3e51a78` | 1 | 532 MiB / 17.9 s/h | 43 MiB / 2.6 s/h | calendar and wanted lists read in SQL |
| 2026-11-15 | real | `3e51a78` | 1 | 483 MiB / 19.2 s/h | 28 MiB / 1.6 s/h | calendar and wanted lists read in SQL; Unarr's warm-up flagged, noise |
| 2026-11-14 | real | `ae1283b` | 1 | 496 MiB / 18.4 s/h | 28 MiB / 1.6 s/h | prototype; debug logs |
| 2026-11-14 | scaled | `ae1283b` | 1 | 849 MiB / 28.5 s/h | 45 MiB / 1.8 s/h | prototype; debug logs, no heap limit |
