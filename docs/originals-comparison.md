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
writes them.

The real library's tables are of 2026-11-17 at `57ca42c`
(`core/survive-a-panic`, issue #14): the release profile unwinds on a
panic instead of aborting, and a panic is caught where a request or a
command starts. The report says `-dirty` because the documents of the
same branch were not committed yet; the code is that commit's. What the
change costs was measured against `main` (`d6c61de`) in the same
container, one run each:

| | `main`, aborting | `57ca42c`, unwinding |
|---|--:|--:|
| Binary, stripped | 30.7 MiB | 34.9 MiB |
| The same under `gzip -9` | 12.3 MiB | 14.4 MiB |
| First answer on both ports | 0.26 s | 0.17 s |
| Idle RSS, at the end of the idle window | 28 MiB | 30 MiB |
| Same, anonymous memory | 10 MiB | 11 MiB |
| Peak anonymous memory during the reads | 43 MiB | 44 MiB |
| Anonymous memory at the end of the tail | 36 MiB | 37 MiB |

The times of the two runs are not compared: the `main` run shared the
machine with builds, and this run's own times differ from it both ways
(Sonarr's `/history` 6.5 ms against 9.6, Radarr's `/movie` 25.0 against
19.2). The two binaries alternated in this container on the real library
(before, after, after, before, then after, before, before, after; each a
fresh process, five warm-up passes, 200 calls a route) answer within
14 % of each other, in both directions, on every route that takes more
than a millisecond: `/series` 3.7 and 3.8 ms, `/movie` 21.5 and 19.6,
Radarr's `/history` 23.1 and 19.9, its `/wanted/missing` 4.1 and 4.2
(`main` first). The queues and `/ping`, 0.2 to 0.7 ms, vary more between
two processes of one binary than between the binaries. The catch around
a request costs nothing that shows.

The scaled library was not measured again for this change, which is not
one to the speed or the memory of the routes: its tables are still those
of the run described next. That run was on 2026-11-17 in the cloud
session (x86-64, four cores, 16 GB), one run per library, at `a6e0cc0`
(`sonarr/series-list-off-async`, on top of
`sonarr/wanted-cutoff-light-rows` and `main`; rebased since, with no
change to the code): the scheduler on the originals' cadence, Sonarr's
wanted lists and calendar reading only the series they show, Radarr's
answers reading only the translations of the movies they show, a queue
answer with nothing to embed reading no reference data in either app,
Sonarr's cutoff list sorting light rows before it reads the page, and
Sonarr's series list built off the async workers.

The session's container was restarted again between the `f57dd02` run
and the `54ecf53` one (in the History), and it is faster. Queues aside
(under 1.5 ms on both sides), in the `54ecf53` run the originals' routes
took 16 to 43 % less time than in the `f57dd02` one and Unarr's 18 to
40 % less, except two of Unarr's that kept their time on the real
library: Radarr's `/history` (18.5 ms, then 18.3) and Sonarr's `/series`
(3.3 both times). That change touches only Sonarr's cutoff list, which
the reads do not call (it is measured on its own below).

The `a6e0cc0` run is in the same container as the `54ecf53` one. On the
real library both sides moved together (Radarr's `/movie`: the original 29 %
slower, Unarr 24 %). On the scaled library, queues aside, the originals'
routes are within 6 % of that run, and Unarr's are slower, by up to
64 % (Radarr's `/history`, 31.4 ms against 19.1), with its warm-up
flagged: the last pass took 11 % more CPU than the one before. The change is not the
cause. The two binaries the two runs measured, alternated in this
container twice on the scaled library (before, after, after, before, then
after, before, before, after; each a fresh process with five warm-up
passes of the reads and five measured ones), answer every route within
4 % of each other over the two series, queues aside (under 0.4 ms), and
in both series the binary measured second and third was the slower on
most routes; on the real library, once, within 8 %. Radarr's `/history`
took 19.7 to 21.3 ms on the scaled library in those runs, whichever the
binary.

### Real library (38 series, 193 movies)

Library `real`; Unarr `57ca42c-dirty, sha256 8c44763a5c64`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; 5 warm-up passes; 2026-11-17.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 1.70 s | 0.17 s | 9.9× sooner |
| Idle, at the end of the idle window: RSS | 478 MiB | 30 MiB | 94 % |
| Same, anonymous memory | 262 MiB | 11 MiB | 96 % |
| Idle CPU, from minute 5 of the window | 16.6 s/h | 1.1 s/h | 93 % |
| The reads: CPU | 2.48 s | 1.39 s | 44 % |
| The reads: time | 2.33 s | 1.52 s | 35 % |
| Peak anonymous memory during the reads | 380 MiB | 44 MiB | 89 % |
| Anonymous memory at the end of the tail | 375 MiB | 37 MiB | 90 % |
| Idle CPU in the tail, from its first minute | 7.2 s/h | 0.7 s/h | 90 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 7.6 / 11.6 | 3.2 / 5.6 |
| Sonarr /api/v3/history | 25.5 / 28.2 | 6.5 / 7.2 |
| Sonarr /api/v3/wanted/missing | 4.0 / 4.5 | 2.5 / 3.2 |
| Sonarr /api/v3/calendar | 4.2 / 7.6 | 2.0 / 2.7 |
| Sonarr /api/v3/queue | 1.3 / 1.7 | 0.7 / 1.2 |
| Radarr /api/v3/movie | 22.1 / 42.5 | 25.0 / 29.3 |
| Radarr /api/v3/history | 19.1 / 22.4 | 21.4 / 28.5 |
| Radarr /api/v3/wanted/missing | 15.1 / 24.9 | 4.9 / 5.7 |
| Radarr /api/v3/calendar | 9.4 / 11.1 | 6.0 / 7.4 |
| Radarr /api/v3/queue | 1.2 / 1.5 | 0.7 / 0.8 |
| Sonarr /api/v3/episode | 2.0 / 3.1 | 2.0 / 2.8 |

### Scaled library (950 series, 2,123 movies)

Library `scaled`; Unarr `a6e0cc0, sha256 ea509357877a`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; **warm-up not settled for unarr: raise --warmup**; 5 warm-up passes; 2026-11-17.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 1.38 s | 0.28 s | 5.0× sooner |
| Idle, at the end of the idle window: RSS | 530 MiB | 32 MiB | 94 % |
| Same, anonymous memory | 314 MiB | 14 MiB | 95 % |
| Idle CPU, from minute 5 of the window | 14.1 s/h | 1.0 s/h | 93 % |
| The reads: CPU | 13.90 s | 7.05 s | 49 % |
| The reads: time | 12.64 s | 7.27 s | 42 % |
| Peak anonymous memory during the reads | 539 MiB | 196 MiB | 64 % |
| Anonymous memory at the end of the tail | 505 MiB | 45 MiB | 91 % |
| Idle CPU in the tail, from its first minute | 7.3 s/h | 1.9 s/h | 73 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 156.3 / 209.5 | 44.2 / 65.7 |
| Sonarr /api/v3/history | 45.5 / 56.8 | 24.0 / 28.6 |
| Sonarr /api/v3/wanted/missing | 10.2 / 14.6 | 10.0 / 11.4 |
| Sonarr /api/v3/calendar | 52.3 / 84.0 | 5.4 / 5.7 |
| Sonarr /api/v3/queue | 1.0 / 1.7 | 0.4 / 0.7 |
| Radarr /api/v3/movie | 216.9 / 256.7 | 201.7 / 252.4 |
| Radarr /api/v3/history | 26.6 / 58.5 | 31.4 / 34.6 |
| Radarr /api/v3/wanted/missing | 16.1 / 25.2 | 6.9 / 8.0 |
| Radarr /api/v3/calendar | 68.8 / 102.0 | 27.5 / 37.9 |
| Radarr /api/v3/queue | 1.1 / 2.4 | 0.2 / 1.1 |
| Sonarr /api/v3/episode | 4.2 / 31.2 | 3.0 / 14.5 |

Where Unarr is slower than the original, in these tables (whoever updates
the tables updates this list from them, as CLAUDE.md says):

- Radarr's `/history` on the real library: 21.4 ms against 19.1 (20.1
  against 16.4 in the `a6e0cc0` run, 18.3 against 14.2 in the `54ecf53`
  one). Unarr's time is about that of every run (18.3 to 21.4); the
  original's moves with the container, 14 to 25.5 over the runs so far.
  Unarr's time hardly grows with the library, the original's does.
- Radarr's `/movie` on the real library, in the `57ca42c` run only: 25.0
  ms against 22.1 (19.7 against 19.8 in the `a6e0cc0` run). It is that
  run's noise, not the change it measured: with the two binaries
  alternated in the same container `/movie` took 19.6 ms on this one and
  21.5 on `main`.
- Radarr's `/history` on the scaled library: 31.4 ms against 26.6. This
  run's figure is the outlier described above: 19.1 in the `54ecf53` run,
  19.7 to 21.3 with the binaries alternated.

Sonarr's `/wanted/missing`, slower than the original on the scaled
library before `sonarr/scoped-series-reads` (29.6 ms against 20.8), now
reads only the page's series: 10.0 ms against 10.2 in the tables above
(12.1 against 17.2 in the `f57dd02` run, in the slower container).

Sonarr's `/wanted/cutoff` is not in the reads. Until
`sonarr/wanted-cutoff-light-rows` it read every episode with a file in
full (overview, images, ratings) to filter and sort them, and on the
scaled library its unmonitored list was slower than the original's. It
now sorts light rows (the file's quality, the series' profile and the
sort keys) and reads in full only the page it returns; the cutoff search
command reads its candidates the same way. Measured on its own on
2026-11-17, in the container of the tables: the original (one process,
the same heap limit, settled for two minutes), then the `main` binary
before the change and the branch's, alternately (before, after, after,
before), each a fresh process settled for a minute; then 30 unmeasured
rounds of the four queries and 30 measured ones, pages of 20 with their
series. The cell is
the p50 in ms, the median of the two processes' for the binaries. All
three give the same totals: 2,075 monitored and 2,100 unmonitored
episodes below the cutoff on the scaled library, 83 and 84 on the real
one.

| `/wanted/cutoff`, p50 ms | Scaled: original | before | after | Real: original | before | after |
|---|--:|--:|--:|--:|--:|--:|
| Monitored, by air date | 75.6 | 72.7 | 10.5 | 10.2 | 3.8 | 2.4 |
| Monitored, by series title | 74.5 | 73.7 | 10.3 | 9.3 | 3.8 | 2.2 |
| Unmonitored, by air date | 65.6 | 72.1 | 9.4 | 9.7 | 3.8 | 2.4 |
| Monitored, page 5 | 77.4 | 72.8 | 10.7 | 9.3 | 3.6 | 2.0 |

Sonarr's `/series` builds the whole library's list, about a megabyte of
JSON on the scaled library. Until `sonarr/series-list-off-async` it built
it on the async workers, which serve every other request meanwhile; it
now builds it on the blocking pool, as Radarr's `/movie` does. One
request at a time it costs the same; under load the list is served
faster and the other requests wait less. Measured on 2026-11-17, in the
container of the tables, on the scaled library: the binaries before and
after the change alternately (before, after, after, before), each a
fresh Sonarr-only process settled for a minute, ten unmeasured rounds,
30 lists one at a time, then four clients asking for `/series` without
pause for 30 s while a fifth asks for `/system/status` every 50 ms. The
cell is the median of the two processes'.

| `/series`, scaled library | Before | After |
|---|--:|--:|
| One at a time: p50 | 47.2 ms | 45.7 ms |
| 4 clients: lists per second | 23.9 | 28.6 |
| 4 clients: p50 of a list | 168.1 ms | 138.0 ms |
| Meanwhile `/system/status`: p50 / p95 / max | 117.4 / 143.8 / 174.5 ms | 88.0 / 114.8 / 137.3 ms |

`/system/status` still waits: it reads a setting from the database,
which answers one call at a time, behind the lists' reads, and the
list's encoding into bytes still runs on the async workers, as Radarr's
`/movie`'s does.

Sonarr's `/queue` read the series catalog (quality profiles, custom
formats) for every answer, even an empty page. Since
`sonarr/empty-queue-catalog` it reads it only for an answer that embeds
an episode (`includeEpisode`, and an item that has one), as Radarr's
queue does. On the empty queue of both fixtures, with the binaries
before and after the change run alternately in the container of the
`f57dd02` run, it went from 1.42 ms to 0.57 on the real library and from
1.59 to 0.39 on the scaled one (the originals, in that run: 1.5 and
1.3).

Radarr's `/calendar` and `/wanted/missing`, slower than the original
before `radarr/calendar-date-range-query`, now ask SQLite for the movies
they show. Since `radarr/movie-context-translations` the routes that show
a page of movies (queue, history, blocklist, wanted, calendar, one movie
and its files) read only those movies' translations, not the whole
library's: on the scaled library `/queue` went from 15.8 ms to 2.2,
`/history` from 40.2 to 27.0 (the original: 32.2), `/wanted/missing`
from 20.1 to 5.8 (21.7) and `/calendar` from 43.1 to 28.8 (102.6).
`/movie` still reads and encodes the whole library, as the original does.
Since `radarr/empty-queue-context` a queue page with no movie, such as
the empty queue of both fixtures, reads no movie context at all (root
folders, quality profiles, custom formats, two settings): `/queue` went
from 2.2 ms to 0.3 on the scaled library (the original: 1.4) and from
2.1 to 0.3 on the real one (1.5).

Unarr's peak anonymous memory during the reads on the scaled library
goes from about 140 MiB to about 260 from one run to the next (140, 140,
196, 257, 261, 205 and 196 in the last seven), and the code is not what
decides it.
glibc gives a thread that allocates under contention an arena of its
own, so a large answer (Radarr's `/movie`, 2,123 movies) built on a
thread whose arena has not served one since the last trim makes that
arena grow too; the scheduler's trim, every 30 s, gives the pages back;
the more arenas have grown, the higher the peak. One process of the
`f251780` binary, whose run gave 257, gave 195, 142, 146, 145, 140 and
206 MiB in six passes. The idle and tail rows are not affected: a trim
has run by then.

Capping the arenas would fix the swing, but it costs far more than it
saves, so `unarrd` leaves glibc's default (8 per core). Measured on
2026-11-16, in another 4-core x86-64 container than the tables above,
with the `749a330` binary on the scaled library: three fresh processes
per setting; per process three cycles of 5 warm-up passes and a measured
pass of the reads (memory sampled every 0.2 s), then 4 and 8 parallel
clients for 60 s each over the ten routes of the reads, as the web
interface sends several requests at once.

| `MALLOC_ARENA_MAX` | unset | 4 | 2 | 1 |
|---|--:|--:|--:|--:|
| Peak during a pass, min / median / max | 137 / 193 / 200 MiB | 133 / 143 / 192 MiB | 131 / 140 / 190 MiB | 124 / 127 / 129 MiB |
| 4 clients: requests per second | 57.4 | 47.2 (−18 %) | 30.5 (−47 %) | 13.7 (−76 %) |
| 4 clients: p50 / p95, all routes | 36 / 301 ms | 43 / 358 ms | 48 / 644 ms | 71 / 1,790 ms |
| 4 clients: peak anonymous memory | 357-418 MiB | 349-373 MiB | 295-336 MiB | 230-251 MiB |
| 8 clients: requests per second | 61.2 | 38.2 (−38 %) | 27.7 (−55 %) | 14.0 (−77 %) |
| 8 clients: p50 / p95, all routes | 76 / 440 ms | 94 / 818 ms | 125 / 1,138 ms | 241 / 2,713 ms |
| 8 clients: peak anonymous memory | 422-532 MiB | 408-459 MiB | 335-426 MiB | 268-288 MiB |
| Idle after the load, anonymous / RSS | 61 / 80 MiB | 58 / 76 MiB | 59 / 78 MiB | 68 / 87 MiB |

One request at a time, the latency is the same in every setting (Radarr
`/movie` 215-224 ms at p50). Under concurrent requests the threads queue
on the arena locks: even 4 arenas cost a fifth to two fifths of the
throughput, and only one arena makes the sequential peak stable, at a
quarter of the throughput. Two and four arenas still reach 190 MiB in
some passes.

Four or eight clients that never pause are more than Unarr meets: it has
one user, whose browser sends a burst of requests when a page opens and
nothing until the next click. The same binary, setup and container,
loaded that way: bursts of 4 to 6 parallel requests, the app's main list
(`/series` or `/movie`) and some of queue, history, wanted, calendar and
`system/status`, then a pause of 2 to 5 s, for 5 minutes; three fresh
processes per setting, the same seeded bursts for each, 246-247 bursts
in all. A page load is the time to the last answer of its burst.

| `MALLOC_ARENA_MAX` | unset | 4 | 2 | 1 |
|---|--:|--:|--:|--:|
| Sonarr page load, p50 / p95 | 76 / 100 ms | 78 / 108 ms | 81 / 106 ms | 88 / 131 ms |
| Radarr page load, p50 / p95 | 252 / 311 ms | 260 / 315 ms | 280 / 347 ms | 294 / 346 ms |
| Peak anonymous memory, per process | 205-207 MiB | 217-265 MiB | 186-192 MiB | 131 MiB |
| Median anonymous memory, per process | 140-144 MiB | 143-158 MiB | 136-141 MiB | 127-130 MiB |

For one user the cost is smaller, and so is the saving: one arena makes a
page load 16-17 % slower at p50 (40 ms on Radarr's movie index) to keep
the peak 75 MiB lower for the seconds before the next trim; four arenas
cost 3 % and save nothing. On the NAS's 16 GB neither is worth a cap.
The largest peaks come from concurrent requests, and what would lower
them is answers that allocate less (`/movie` and `/series` built whole
before they are sent), not fewer arenas. Not measured: the ARM64 NAS.

All of this is about glibc, the allocator of a `cargo build` on Linux
and of these tables. The Docker image is a static musl build, whose
allocator has no per-thread arenas and no `malloc_trim`
(`release_memory` does nothing there), so neither the swing nor a cap
applies to it; its memory under the same load has not been measured
here. Who runs a glibc build and prefers the lower peak can set
`MALLOC_ARENA_MAX=1` in the environment, without a change to the code.

### In production, before the switch

The homelab NAS (QNAP TS-435XeU, ARM64, 16 GB) measured the originals in
their 1 GiB containers before Unarr replaced them:

- **2026-09-27, cgroup counters over 7.6 hours.** Radarr: 59 MiB RSS plus
  90 MiB of swap, CPU 0.7 % of one core on average. Sonarr had just been
  restarted: 160 MiB after 0.2 hours.
- **2026-10-01, `docker stats`** (cache excluded), two hours after the
  power-on: Sonarr 214 MiB, Radarr 260 MiB.

Those 474 MiB for both are the same order as the 495 MiB of RSS the real
library gives the originals here. The saving on the NAS is about 450 MiB
of RAM.

## History

One row per run, newest first: the library, the Unarr commit, idle RSS and
idle CPU of each side.

| Date | Library | Unarr | Runs | Originals: idle RSS / CPU | Unarr: idle RSS / CPU | Note |
|---|---|---|--:|--:|--:|---|
| 2026-11-17 | real | `57ca42c` | 1 | 478 MiB / 16.6 s/h | 30 MiB / 1.1 s/h | the release profile unwinds and a panic is caught per request and per command (#14): against `main` in the same container, the binary 30.7 to 34.9 MiB, idle RSS 28 to 30 MiB; times within noise with the two binaries alternated; the scaled library not measured |
| 2026-11-17 | scaled | `a6e0cc0` | 1 | 530 MiB / 14.1 s/h | 32 MiB / 1.0 s/h | Sonarr's series list built off the async workers; Unarr's warm-up flagged (last pass 11 % above the one before) and its routes up to 64 % slower than in the run before, noise: the binaries alternated are within 4 % |
| 2026-11-17 | real | `a6e0cc0` | 1 | 495 MiB / 16.5 s/h | 27 MiB / 0.9 s/h | Sonarr's series list built off the async workers |
| 2026-11-17 | scaled | `54ecf53` | 1 | 552 MiB / 18.6 s/h | 33 MiB / 0.8 s/h | Sonarr's cutoff list sorts light rows (not in the reads); a faster container than the run before |
| 2026-11-17 | real | `54ecf53` | 1 | 479 MiB / 16.2 s/h | 27 MiB / 0.8 s/h | Sonarr's cutoff list sorts light rows (not in the reads); a faster container than the run before |
| 2026-11-17 | scaled | `f57dd02` | 1 | 538 MiB / 16.2 s/h | 32 MiB / 1.1 s/h | a Sonarr queue answer with no episode reads no catalog; a slower container than the run before |
| 2026-11-16 | real | `f57dd02` | 1 | 489 MiB / 19.8 s/h | 27 MiB / 1.1 s/h | a Sonarr queue answer with no episode reads no catalog; a slower container than the run before; the originals' warm-up flagged (last pass 12 % below the one before), noise |
| 2026-11-16 | scaled | `f251780` | 1 | 540 MiB / 14.4 s/h | 32 MiB / 0.9 s/h | a Radarr queue page with no movie reads no movie context; both warm-ups flagged (last pass 15-18 % above the one before), noise |
| 2026-11-16 | real | `f251780` | 1 | 479 MiB / 16.8 s/h | 28 MiB / 1.0 s/h | a Radarr queue page with no movie reads no movie context |
| 2026-11-15 | scaled | `efd0320` | 1 | 546 MiB / 23.9 s/h | 32 MiB / 1.1 s/h | Radarr reads only the shown movies' translations, on top of the cadence and the Sonarr reads |
| 2026-11-15 | real | `efd0320` | 1 | 476 MiB / 17.1 s/h | 28 MiB / 1.1 s/h | Radarr reads only the shown movies' translations, on top of the cadence and the Sonarr reads |
| 2026-11-15 | scaled | `d772fd6` | 1 | 535 MiB / 15.9 s/h | 44 MiB / 2.5 s/h | Radarr reads only the shown movies' translations; before the Sonarr reads and the cadence |
| 2026-11-15 | real | `d772fd6` | 1 | 484 MiB / 18.4 s/h | 28 MiB / 1.6 s/h | Radarr reads only the shown movies' translations; before the Sonarr reads and the cadence |
| 2026-11-15 | scaled | `51fbc56` | 1 | 538 MiB / 16.3 s/h | 43 MiB / 2.5 s/h | Sonarr reads only the series it shows |
| 2026-11-15 | real | `51fbc56` | 1 | 479 MiB / 18.5 s/h | 27 MiB / 1.3 s/h | Sonarr reads only the series it shows |
| 2026-11-15 | real | `3e81473` | 1 | 479 MiB / 18.1 s/h | 28 MiB / 1.1 s/h | the download monitor every 90 s, as the originals' |
| 2026-11-15 | scaled | `3e51a78` | 1 | 532 MiB / 17.9 s/h | 43 MiB / 2.6 s/h | calendar and wanted lists read in SQL |
| 2026-11-15 | real | `3e51a78` | 1 | 483 MiB / 19.2 s/h | 28 MiB / 1.6 s/h | calendar and wanted lists read in SQL; Unarr's warm-up flagged, noise |
| 2026-11-14 | real | `ae1283b` | 1 | 496 MiB / 18.4 s/h | 28 MiB / 1.6 s/h | prototype; debug logs |
| 2026-11-14 | scaled | `ae1283b` | 1 | 849 MiB / 28.5 s/h | 45 MiB / 1.8 s/h | prototype; debug logs, no heap limit |
