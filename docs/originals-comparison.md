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
writes them. Measured on 2026-11-18 in the cloud session (x86-64, four
cores, 16 GB), one run per library, at `4bba6eb`
(`radarr/history-one-call-streamed-since`, on top of `main` at
`e1f238e`, before the unwinding below): the
scheduler on the originals' cadence, Sonarr's wanted lists and calendar
reading only the series they show, Radarr's answers reading only the
translations of the movies they show, a queue answer with nothing to
embed reading no reference data in either app, Sonarr's cutoff list
sorting light rows before it reads the page, Sonarr's series list built
off the async workers, the parser's and the custom formats' regexes
built with seek, the custom formats' conditions decoded once per
request, and Radarr's history read in one database call, its
`/history/since` streamed.

The machine is noisy from one run to the next. Against the `c53a302`
runs of the same day (in the History), the originals' routes, queues
aside, took 12 to 63 % more time on the scaled library, and from 23 %
less to 23 % more on the real one. On the real library Unarr's Radarr
routes took 12 to 46 % more time than in the `c53a302` run, and its
Sonarr routes from 14 % less to 14 % more, though the change does not
slow them: the binaries before and after, alternated in this container
after the runs, are within 7 % of each other on every route of the
reads, queues aside (below the list of slower routes). Unarr's warm-up
is flagged on both libraries: its last pass took 10 % more CPU than the
one before on the real library, 11 % on the scaled one.

Since those runs the release profile unwinds on a panic instead of
aborting, and a panic is caught where a request or a command starts
(`core/survive-a-panic`, issue #14). What that costs was measured on
2026-11-17 against `main` at `d6c61de`, before the seek change, in one
container, one run each on the real library:

| | `d6c61de`, aborting | `57ca42c`, unwinding |
|---|--:|--:|
| Binary, stripped | 30.7 MiB | 34.9 MiB |
| The same under `gzip -9` | 12.3 MiB | 14.4 MiB |
| Idle RSS, at the end of the idle window | 28 MiB | 30 MiB |
| Same, anonymous memory | 10 MiB | 11 MiB |
| Peak anonymous memory during the reads | 43 MiB | 44 MiB |
| Anonymous memory at the end of the tail | 36 MiB | 37 MiB |

The times of those two runs are not compared: the `d6c61de` run shared
the machine with builds. The two binaries alternated in that container
on the real library (before, after, after, before, then after, before,
before, after; each a fresh process, five warm-up passes, 200 calls a
route) answer within 14 % of each other, in both directions, on every
route that takes more than a millisecond: `/series` 3.7 and 3.8 ms,
`/movie` 21.5 and 19.6, Radarr's `/history` 23.1 and 19.9, its
`/wanted/missing` 4.1 and 4.2 (`d6c61de` first). The catch around a
request costs nothing that shows.

The tables below were measured again on 2026-11-20 in a cloud session
(x86-64, four cores, 16 GB), one run per library, at `376a687`
(`core/audit-http-low-rows`): the server outside the API as the
originals' pipeline (cache headers, Brotli, content negotiation, the
request log, the answer while starting). The script's client sends no
`Accept-Encoding`, so neither side compresses the reads; the request log
writes nothing at `info`, the level the script sets on both sides. Every route of both libraries
answers sooner than the original's. What follows describes the
`4bba6eb` runs these tables replace (in the History).

The `4bba6eb` tables were without the unwinding, which came into `main` after
the commit they measure: it is not a change to the speed or the memory
of the routes.
The real library was measured with it on 2026-11-18 at `a402d7c`, that
branch merged with `main` at `e1f238e` (the code of the `c53a302` run
with the unwinding), in a container faster than this one (the
originals' `/series` 6.4 ms, their `/history` 22.4). Unarr held 30 MiB
at rest there, where it holds 27 here, about what the table above gives
for the unwinding (28 to 30); its row is in the History.

### Real library (38 series, 193 movies)

Library `real`; Unarr `376a687, sha256 5b9dc256e59c`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; 5 warm-up passes; 2026-11-20.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 1.95 s | 0.20 s | 10.0× sooner |
| Idle, at the end of the idle window: RSS | 483 MiB | 30 MiB | 94 % |
| Same, anonymous memory | 268 MiB | 10 MiB | 96 % |
| Idle CPU, from minute 5 of the window | 17.6 s/h | 1.0 s/h | 95 % |
| The reads: CPU | 3.07 s | 1.11 s | 64 % |
| The reads: time | 2.93 s | 1.23 s | 58 % |
| Peak anonymous memory during the reads | 381 MiB | 46 MiB | 88 % |
| Anonymous memory at the end of the tail | 382 MiB | 39 MiB | 90 % |
| Idle CPU in the tail, from its first minute | 7.3 s/h | 0.9 s/h | 88 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 9.3 / 12.3 | 4.4 / 6.3 |
| Sonarr /api/v3/history | 29.2 / 36.1 | 3.7 / 4.6 |
| Sonarr /api/v3/wanted/missing | 4.1 / 10.3 | 2.5 / 2.9 |
| Sonarr /api/v3/calendar | 4.1 / 5.8 | 2.1 / 2.8 |
| Sonarr /api/v3/queue | 1.4 / 1.7 | 0.8 / 1.3 |
| Radarr /api/v3/movie | 29.9 / 57.8 | 23.0 / 37.9 |
| Radarr /api/v3/history | 24.7 / 33.3 | 11.1 / 13.2 |
| Radarr /api/v3/wanted/missing | 21.9 / 27.3 | 4.3 / 5.9 |
| Radarr /api/v3/calendar | 10.1 / 14.8 | 3.7 / 6.2 |
| Radarr /api/v3/queue | 1.2 / 1.5 | 0.8 / 0.9 |
| Sonarr /api/v3/episode | 5.4 / 41.1 | 3.0 / 6.0 |

### Scaled library (950 series, 2,123 movies)

Library `scaled`; Unarr `376a687, sha256 5b9dc256e59c`; Sonarr 4.0.20.3014, Radarr 6.4.4.10685; first start 300 s, idle 900 s, tail 300 s; .NET heap limit 768 MiB; 5 warm-up passes; 2026-11-20.

| | Sonarr + Radarr | Unarr | Less |
|---|--:|--:|--:|
| First answer on both ports | 2.46 s | 0.29 s | 8.6× sooner |
| Idle, at the end of the idle window: RSS | 541 MiB | 34 MiB | 94 % |
| Same, anonymous memory | 324 MiB | 15 MiB | 95 % |
| Idle CPU, from minute 5 of the window | 14.9 s/h | 1.0 s/h | 93 % |
| The reads: CPU | 17.70 s | 7.15 s | 60 % |
| The reads: time | 16.26 s | 7.51 s | 54 % |
| Peak anonymous memory during the reads | 537 MiB | 257 MiB | 52 % |
| Anonymous memory at the end of the tail | 494 MiB | 47 MiB | 91 % |
| Idle CPU in the tail, from its first minute | 7.0 s/h | 1.0 s/h | 85 % |

| p50 / max, ms | Originals | Unarr |
|---|--:|--:|
| Sonarr /api/v3/series | 205.9 / 245.8 | 59.5 / 71.4 |
| Sonarr /api/v3/history | 51.2 / 71.4 | 23.8 / 29.1 |
| Sonarr /api/v3/wanted/missing | 11.8 / 23.1 | 10.9 / 13.6 |
| Sonarr /api/v3/calendar | 72.6 / 125.2 | 6.6 / 7.6 |
| Sonarr /api/v3/queue | 1.2 / 1.8 | 0.3 / 0.8 |
| Radarr /api/v3/movie | 275.2 / 356.0 | 227.1 / 276.3 |
| Radarr /api/v3/history | 34.4 / 79.8 | 11.5 / 20.6 |
| Radarr /api/v3/wanted/missing | 21.3 / 36.0 | 6.5 / 9.3 |
| Radarr /api/v3/calendar | 87.9 / 145.7 | 9.9 / 18.6 |
| Radarr /api/v3/queue | 1.2 / 2.1 | 0.6 / 0.8 |
| Sonarr /api/v3/episode | 4.8 / 42.8 | 2.8 / 7.2 |

Where Unarr is slower than the original, in these tables (whoever updates
the tables updates this list from them, as CLAUDE.md says):

None, on either library, in the `376a687` runs. Radarr's `/movie` on the
real library, slower in the `4bba6eb` run (24.8 ms against 23.4), took
23.0 ms against 29.9.


For the `4bba6eb` runs, the binaries before (`main`) and after their change,
alternated in this container after the runs (before, after, after,
before, each a fresh process with five warm-up passes of the reads and
five measured ones), p50 in ms:

| Route | Real: before | after | Scaled: before | after |
|---|--:|--:|--:|--:|
| Radarr `/movie` | 19.9 | 21.0 | 224.8 | 225.1 |
| Radarr `/history` | 7.8 | 7.7 | 10.7 | 10.8 |
| Radarr `/wanted/missing` | 3.8 | 3.7 | 6.0 | 6.0 |
| Radarr `/calendar` | 3.6 | 3.6 | 10.6 | 10.2 |

Radarr's `/history` and `/blocklist` read a page, its movies and the
movie context in three calls to the database, each waiting for its turn
on the database thread; they now read them in one, as Sonarr's
`/history` does. One request at a time this saves nothing measurable;
what it saves is the waits behind other requests' calls, not measured
here. `/history/since`, which answers every history row since a date,
built the whole list in memory before sending it, as the original does;
it now reads the rows' ids, then reads and encodes them in pages of
1,000 while the answer is sent, as Sonarr's does. The first page is
read before the answer starts, so an error there is still an error
status. On the scaled library, Radarr alone, the binaries before and
after alternated (before, after, after, before, each a fresh process
settled for a minute), `/history/since` from 1900 with its movies, all
50,000 rows: two unmeasured calls, then three, each with the process's
anonymous memory sampled every 20 ms; the cell is the median of the
two processes'.

| Radarr, scaled library | Before | After |
|---|--:|--:|
| `/history`, a page of 20 with movies: p50 | 10.1 ms | 10.1 ms |
| `/history/since`, 50,000 rows with movies: time | 9.77 s | 9.70 s |
| Same: peak anonymous memory above the process's rest | 142 MiB | 6 MiB |

The original Radarr, with the 768 MiB heap limit of the tables, does not
answer that call: it fails with an `OutOfMemoryException` and status
500.

Radarr's `/history` was slower than the original on the real library in
the runs of 2026-11-17 (18.3 ms against 14.2, then 20.1 against 16.4),
and took about as long on the scaled library: most of its time did not
grow with the library. Timed inside the route, on a page of 20 rows with
their movies on the real library, it spent 2.2 ms reading the database,
5.3 ms parsing the rows' release names again, 8.4 ms matching them
against the library's custom formats (62 formats, 503 conditions, 439 of
them regexes) and under a millisecond on the movies and the encoding.
The parser and the conditions spent their time in `fancy-regex`'s
backtracking VM, which runs the patterns with lookarounds (a tenth of
the conditions, most of the parser's) and tries them at every position
of a name; and each row read the conditions' JSON again.

Since `core/faster-title-and-format-matching` both build their regexes
with the engine's seek pre-filter, which first asks a regular
approximation of the pattern, never stricter than the pattern, where a
match can start, and the conditions are decoded once per request (a
`FormatSet`, held by the decision engine's catalog, Radarr's movie
context and Sonarr's route catalog). On the fixtures' release names, a
movie name parses in about 140 µs instead of 260 and the conditions'
regexes take 23 µs instead of 211; the parser's compiled patterns weigh
55.7 MB instead of 51.5 while they are kept (they are let go after five
minutes unused, as before). The engine marks seek experimental: with and
without it, the fixtures' 2,402 distinct release names parse the same
with the movie and the series parser, and their 397 distinct conditions
match the same on the 5,777 names, simplified names and release groups;
`crates/unarr-core/tests/regex_seek.rs` repeats the second check on the
history's names. The binaries before (`main`) and after,
alternated before the `c53a302` runs (before, after, after, before, each
a fresh process with five warm-up passes of the reads and five measured
ones), p50 in ms:

| Route | Real: before | after | Scaled: before | after |
|---|--:|--:|--:|--:|
| Radarr `/history` | 19.1 | 7.5 | 23.9 | 10.9 |
| Radarr `/calendar` | 5.1 | 3.4 | 29.1 | 10.8 |
| Sonarr `/history` | 6.2 | 3.8 | 26.0 | 24.0 |
| Sonarr `/episode` | 2.8 | 2.2 | 3.3 | 2.8 |

Radarr's calendar shows each movie's file with its custom formats. The
other routes of the reads move by less than 10 % either way, queues
aside (under 0.7 ms). Searches, release decisions and imports, which
parse names and match formats too, are not in the reads.

Sonarr's `/wanted/missing`, slower than the original on the scaled
library before `sonarr/scoped-series-reads` (29.6 ms against 20.8), now
reads only the page's series: 11.1 ms against 11.5 in the tables
above.

Sonarr's `/wanted/cutoff` is not in the reads. Until
`sonarr/wanted-cutoff-light-rows` it read every episode with a file in
full (overview, images, ratings) to filter and sort them, and on the
scaled library its unmonitored list was slower than the original's. It
now sorts light rows (the file's quality, the series' profile and the
sort keys) and reads in full only the page it returns; the cutoff search
command reads its candidates the same way. Measured on its own on
2026-11-17, in the container of the `54ecf53` run: the original (one
process, the same heap limit, settled for two minutes), then the `main`
binary before the change and the branch's, alternately (before, after,
after, before), each a fresh process settled for a minute; then 30
unmeasured rounds of the four queries and 30 measured ones, pages of 20
with their series. The cell is the p50 in ms, the median of the two
processes' for the binaries. All three give the same totals: 2,075
monitored and 2,100 unmonitored episodes below the cutoff on the scaled
library, 83 and 84 on the real one.

| `/wanted/cutoff`, p50 ms | Scaled: original | before | after | Real: original | before | after |
|---|--:|--:|--:|--:|--:|--:|
| Monitored, by air date | 75.6 | 72.7 | 10.5 | 10.2 | 3.8 | 2.4 |
| Monitored, by series title | 74.5 | 73.7 | 10.3 | 9.3 | 3.8 | 2.2 |
| Unmonitored, by air date | 65.6 | 72.1 | 9.4 | 9.7 | 3.8 | 2.4 |
| Monitored, page 5 | 77.4 | 72.8 | 10.7 | 9.3 | 3.6 | 2.0 |

Sonarr's `/series` builds the whole library's list, about a megabyte of
JSON on the scaled library. Until `sonarr/series-list-off-async` it
built it on the async workers, which serve every other request
meanwhile; it now builds it on the blocking pool, as Radarr's `/movie`
does. One request at a time it costs the same; under load the list is
served faster and the other requests wait less. Measured on 2026-11-17,
in the container of the `a6e0cc0` run, on the scaled library: the
binaries before and after the change alternately (before, after, after,
before), each a fresh Sonarr-only process settled for a minute, ten
unmeasured rounds, 30 lists one at a time, then four clients asking for
`/series` without pause for 30 s while a fifth asks for `/system/status`
every 50 ms. The cell is the median of the two processes'.

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
goes from about 140 MiB to about 260 from one run to the next (140, 196,
257, 261, 205, 196, 202 and 249 in the last eight), and the code is not
what decides it.
glibc gives a thread that allocates under contention an arena of its
own, so a large answer (Radarr's `/movie`, 2,123 movies) built on a
thread whose arena has not served one since the last trim makes that
arena grow too; the scheduler's trim, every 30 s, gives the pages back;
the more arenas have grown, the higher the peak. One process of the
`f251780` binary, whose run gave 257, gave 195, 142, 146, 145, 140 and
206 MiB in six passes. The idle and tail rows are not affected: a trim
has run by then.

Capping the arenas would fix the swing, but it costs far more than it
saves, so `unarrd` leaves glibc's default (8 per core): the maintainer's
decision, with the options weighed, is `docs/DESIGN.md` section 21.2. Measured on
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
and of these tables. Who runs a glibc build and prefers the lower peak can
set `MALLOC_ARENA_MAX=1` in the environment, without a change to the code.
The Docker image holds a static musl build, whose allocator is jemalloc:
the next section.

### The image's allocator

The image holds a static musl build. musl's own allocator takes one lock
for every allocation, on which the threads that serve requests at the
same time queue, and it has no `malloc_trim` (`release_memory` does
nothing there). Since 20 November 2026 the musl build uses jemalloc
instead, with three settings of its own, compiled in
(`crates/unarrd/src/main.rs`, overridden by `_RJEM_MALLOC_CONF` when it
is set): a background thread that returns freed pages, 4 arenas instead of
4 per core, and freed pages returned after a second instead of ten. The
choice is the maintainer's, among the options below, with its reasons in
`docs/DESIGN.md` section 21.6. A glibc build keeps glibc's allocator.

Measured with `scripts/allocator-probe.py` in a 4-core x86-64 container,
on the scaled library, Unarr alone. Each build runs in fresh processes,
alternated A B B A (A B C C B A for three builds) so that a drift of the
machine falls on each alike; per process: 60 s idle after the start, the
comparison's reads (5 warm-up passes and one measured, memory sampled
every 0.2 s), 60 s of 4 clients without pause over the routes of the
reads, 60 s idle after the load. Each cell is the median of two
processes. The image's binary against musl's own, on 20 November:

| Scaled library | musl's own allocator | jemalloc, as the image builds it |
|---|--:|--:|
| Idle after the start: RSS / anonymous | 37.7 / 16.8 MiB | 45.9 / 22.8 MiB |
| The reads: time / CPU | 10.94 / 10.42 s | 8.76 / 8.44 s |
| The reads: peak anonymous | 124 MiB | 208 MiB |
| 4 clients: requests per second | 13.3 | 59.2 |
| 4 clients: p50 / p95 | 68 / 1,882 ms | 28 / 308 ms |
| 4 clients: peak anonymous | 220 MiB | 295 MiB |
| Idle after the load: RSS / anonymous | 78.1 / 56.4 MiB | 90.0 / 66.3 MiB |
| Sonarr `/series`, p50 | 81 ms | 70 ms |
| Radarr `/movie`, p50 | 361 ms | 275 ms |

The musl binary is `bc038a4`, the jemalloc one `e73c586` (the same code
with the allocator, and the branches merged before it). Every build that
was weighed, in six runs on 19 and 20 November, the settings of runs 3 to
5 given in the environment; between runs the figures of one build moved
by up to 6 %:

| Build | Run | Idle: RSS / anon, MiB | The reads | 4 clients: req/s | p95 | Peak, MiB | After the load: RSS / anon, MiB |
|---|--:|--:|--:|--:|--:|--:|--:|
| glibc, default arenas | 1 | 56.5 / 31.2 | 7.22 s | 59.1 | 345 ms | 308 | 75.2 / 49.6 |
| musl's own | 1 | 39.9 / 17.6 | 11.34 s | 13.0 | 1,954 ms | 228 | 80.5 / 57.5 |
| musl's own | 2 | 39.0 / 16.7 | 11.65 s | 13.2 | 1,925 ms | 221 | 79.1 / 56.1 |
| mimalloc 3, its defaults | 2 | 90.3 / 67.7 | 9.05 s | 57.9 | 329 ms | 440 | 234.3 / 211.3 |
| mimalloc 3, `MIMALLOC_PURGE_DELAY=0` | 3 | 77.0 / 54.4 | 10.13 s | 47.6 | 411 ms | 297 | 181.5 / 158.4 |
| mimalloc 2, its defaults | 3 | 406.2 / 383.6 | 9.80 s | 54.6 | 336 ms | 649 | 387.3 / 364.3 |
| jemalloc, its defaults | 3 | 52.4 / 29.5 | 9.04 s | 58.6 | 308 ms | 552 | 122.6 / 99.0 |
| jemalloc, background thread | 3 | 50.1 / 27.3 | 8.78 s | 59.8 | 299 ms | 570 | 99.3 / 75.8 |
| jemalloc, background thread | 4 | 50.3 / 27.5 | 8.90 s | 60.0 | 304 ms | 559 | 99.1 / 75.6 |
| jemalloc, background thread, 1 s decay | 4 | 47.6 / 24.8 | 9.18 s | 58.8 | 320 ms | 381 | 97.6 / 74.1 |
| jemalloc, background thread, 4 arenas | 4 | 48.5 / 25.7 | 8.71 s | 60.9 | 301 ms | 406 | 91.2 / 67.6 |
| jemalloc, background thread, 4 arenas | 5 | 48.0 / 25.2 | 8.64 s | 61.8 | 295 ms | 442 | 91.7 / 68.2 |
| jemalloc, all three | 5 | 45.7 / 22.9 | 8.54 s | 60.0 | 306 ms | 326 | 90.9 / 67.3 |
| jemalloc, all three, compiled in | 6 | 45.9 / 22.8 | 8.76 s | 59.2 | 308 ms | 295 | 90.0 / 66.3 |
| musl's own | 6 | 37.7 / 16.8 | 10.94 s | 13.3 | 1,882 ms | 220 | 78.1 / 56.4 |

Under concurrent requests musl's own allocator serves 22 % of what glibc's
or jemalloc serve, with a p95 six times as long. One
request at a time it is slower too: Radarr's `/movie` at 361 ms against
275 ms with jemalloc and 232 ms with glibc (run 1). Every other allocator
brings the speed back; they differ in what they keep. mimalloc keeps what
it took (234 MiB after the load), even told to purge at once, and its
version 2 starts at 406 MiB. jemalloc's defaults keep 123 MiB; its
background thread brings that to 99 MiB, four arenas to 91 MiB, and the
shorter decay lowers the peak of the reads from 440 to about 230 MiB. All
three together cost, against musl's own, 8 MiB at rest after the start,
12 MiB after the load and 75 MiB at the peak under four clients, for
4.4 times the requests. One request at a time the reads take 18 to 21 %
longer than with glibc (8.54-8.76 s against 7.22 s in run 1, a run of its
own).
The binary grows by 611,200 bytes. Not measured: the ARM64 NAS, and one
user's bursts of page loads, which the glibc arenas were also measured
with (above).

### In production, before the switch

The homelab NAS (QNAP TS-435XeU, ARM64, 16 GB) measured the originals in
their 1 GiB containers before Unarr replaced them:

- **2026-09-27, cgroup counters over 7.6 hours.** Radarr: 59 MiB RSS plus
  90 MiB of swap, CPU 0.7 % of one core on average. Sonarr had just been
  restarted: 160 MiB after 0.2 hours.
- **2026-10-01, `docker stats`** (cache excluded), two hours after the
  power-on: Sonarr 214 MiB, Radarr 260 MiB.

Those 474 MiB for both are the same order as the 486 MiB of RSS the real
library gives the originals here. The saving on the NAS is about 450 MiB
of RAM.

## History

One row per run, newest first: the library, the Unarr commit, idle RSS and
idle CPU of each side.

| Date | Library | Unarr | Runs | Originals: idle RSS / CPU | Unarr: idle RSS / CPU | Note |
|---|---|---|--:|--:|--:|---|
| 2026-11-20 | scaled | `376a687` | 1 | 541 MiB / 14.9 s/h | 34 MiB / 1.0 s/h | the server outside the API as the originals' pipeline (the low rows of the HTTP audit); the reads not compressed on either side; no route slower than the original's |
| 2026-11-20 | real | `376a687` | 1 | 483 MiB / 17.6 s/h | 30 MiB / 1.0 s/h | the server outside the API as the originals' pipeline (the low rows of the HTTP audit); the reads not compressed on either side; no route slower than the original's; Radarr's `/movie` 23.0 ms against 29.9 |
| 2026-11-18 | scaled | `4bba6eb` | 1 | 541 MiB / 17.0 s/h | 32 MiB / 1.2 s/h | Radarr's history read in one database call, its `/history/since` (not in the reads) streamed; the originals' routes 12-63 % slower than in the `c53a302` run; Unarr's warm-up flagged (last pass 11 % above the one before), noise |
| 2026-11-18 | real | `4bba6eb` | 1 | 486 MiB / 18.5 s/h | 27 MiB / 1.1 s/h | Radarr's history read in one database call, its `/history/since` (not in the reads) streamed; Unarr's warm-up flagged (last pass 10 % above the one before) and its Radarr routes 12-46 % slower than in the `c53a302` run, noise: the binaries alternated are within 7 % |
| 2026-11-18 | real | `a402d7c` | 1 | 481 MiB / 14.4 s/h | 30 MiB / 1.1 s/h | the unwinding of #14 merged with the seek change; the binary 34.8 MiB; a faster container than the `c53a302` run; Unarr's warm-up flagged (last pass 14 % below the one before); the scaled library not measured |
| 2026-11-18 | scaled | `c53a302` | 1 | 533 MiB / 15.3 s/h | 32 MiB / 1.1 s/h | regexes built with seek, custom format conditions decoded once per request; a container slower for the originals; Unarr's warm-up flagged (last pass 11 % above the one before), noise |
| 2026-11-18 | real | `c53a302` | 1 | 485 MiB / 17.9 s/h | 27 MiB / 1.0 s/h | regexes built with seek, custom format conditions decoded once per request; a container slower for the originals |
| 2026-11-17 | real | `57ca42c` | 1 | 478 MiB / 16.6 s/h | 30 MiB / 1.1 s/h | the release profile unwinds and a panic is caught per request and per command (#14), before the seek change: against `d6c61de` in the same container, the binary 30.7 to 34.9 MiB, idle RSS 28 to 30 MiB; times within noise with the two binaries alternated; its tables are not kept above |
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
