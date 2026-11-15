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

- **The sides.** The originals are the official Sonarr and Radarr from
  their release tarballs, two processes. Unarr is `target/release/unarrd`
  with both personalities in one process. The two sides run one at a time,
  each on a fresh copy of the library. Neither reaches the network.
- **The two starts.** Each side starts once and runs five minutes, not
  measured: migrations, Unarr's import, the overdue scheduled tasks. Then
  it starts again and is measured:
  - the time to the first answer on both ports;
  - fifteen idle minutes, sampled every five seconds;
  - the same reads over both APIs: about 220 requests, the list of each
    app, history, wanted, calendar, queue and twenty series' episodes;
  - five more idle minutes.
- **RAM** is the RSS and the anonymous memory (the heaps) of
  `/proc/<pid>/smaps_rollup`, summed over a side's processes.
- **CPU** is `utime + stime` from `/proc/<pid>/stat`. Idle CPU is counted
  from the fifth minute of the window, once the start-up work is done.
- **Heap limit.** The originals run with `DOTNET_GCHeapHardLimit` at
  768 MiB: that is what .NET takes from a 1 GiB container such as the
  homelab's.
- **The libraries.**
  - `real` is the anonymised fixtures, the maintainer's own library: 38
    series, 193 movies.
  - `scaled` is `make scale-fixture`: 950 series, 25,300 episodes, 2,123
    movies, 50,000 history rows each.
- **Output.** `.cache/compare/<library>/report.md` holds the tables below,
  `results.json` every sample.

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

On the movie routes the Radarr personality gains far less than the Sonarr
one gains on the series routes. It reads the whole library to answer a
few rows; `/calendar`, the iCal feed and the wanted lists stop doing so in
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

| Date | Library | Unarr | Originals: idle RSS / CPU | Unarr: idle RSS / CPU | Note |
|---|---|---|--:|--:|---|
| 2026-11-14 | real | `ae1283b` | 496 MiB / 18 s/h | 28 MiB / 1.6 s/h | prototype of the script |
| 2026-11-14 | scaled | `ae1283b` | 837 MiB / 16 s/h | 45 MiB / 2 s/h | prototype; originals without heap limit |
