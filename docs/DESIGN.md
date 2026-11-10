# Unarr design notes

Status: draft, November 2026. This document records the reasoning behind the
project and the decisions taken so far. It is meant to be revised.

## 1. Problem

Sonarr, Radarr and Lidarr are .NET applications. In a homelab each one idles at
roughly 150 to 400 MB of resident memory, before Prowlarr and Bazarr. On a
Raspberry Pi or a small NUC that is the single largest line item.

The runtime is not the whole cost, but it is the part a reimplementation can
remove. A Rust daemon built on tokio and axum starts around 5 MB and a realistic
Unarr with caches and three personalities should stay under 100 MB in total.

## 2. Goals and non-goals

Goals, in priority order:

1. **Drop-in.** Prowlarr, Bazarr, Overseerr/Jellyseerr, Recyclarr, Unpackerr,
   Notifiarr and dashboards such as Homepage keep working unchanged: same ports,
   same API prefixes, same API keys, same response shapes.
2. **Import, do not migrate.** Read `sonarr.db`, `radarr.db`, `lidarr.db` and
   `config.xml` as the originals leave them.
3. **Small and fast.** One static binary, SQLite inside, a distroless image.
4. **Familiar.** The look and feel of the originals.

Non-goals:

- A new data model. The originals' model has known limits (no editions, no
  packs, no file shared between libraries); accepting them is the price of
  compatibility. makearr/Solarr took the opposite road, deliberately.
- Running as a multi-tenant service.
- Replacing Prowlarr or Bazarr. They stay, and talk to Unarr.

## 3. Key observations

**The three apps are one codebase.** All are forks of NzbDrone. Indexers,
download clients, quality, custom formats, tags, root folders, notifications,
history and queue are shared code. Unarr is therefore one core with three
*personalities* (media kinds), not three rewrites.

**The frontends are reusable.** The UIs are React applications that talk only to
the REST API and to a SignalR hub. If Unarr implements the API and a minimal
SignalR endpoint (negotiate plus the JSON hub protocol over WebSocket), the
official UI runs unchanged. That gives the UI for free and, more importantly, a
permanent compatibility oracle: if the official UI works, the ecosystem works.

**Licensing follows from reuse.** The originals are GPL-3.0. Reusing their
frontend, parser regexes, quality definitions and custom-format specifications
means Unarr is GPL-3.0-or-later. Decided.

## 4. Architecture

```
unarrd  ──┬── personality Sonarr  :8989  /api/v3  /signalr
          ├── personality Radarr  :7878  /api/v3  /signalr
          └── personality Lidarr  :8686  /api/v1  /signalr
                     │
        ┌────────────┴────────────┐
        │        unarr-core       │  domain: media, quality, custom formats,
        │                         │  parser, decision engine, indexers,
        │                         │  download clients, history, scheduler
        └────────────┬────────────┘
                     │
             SQLite (WAL), one file per personality
```

Crates:

| Crate          | Role |
|----------------|------|
| `unarr-core`   | Domain model and services shared by every personality. No HTTP. |
| `unarr-api`    | axum routers reproducing the original REST surface, plus SignalR. |
| `unarr-import` | Reads the original `config.xml` and SQLite databases. |
| `unarrd`       | The daemon: CLI, configuration, one listener per personality. |

Why one process with several listeners rather than one process per app: the
ecosystem configures each app by host, port and API key, so separate ports keep
compatibility, while one process shares the HTTP client, the scheduler, the
indexer cache and the parser tables.

## 5. Storage

SQLite, write-ahead logging, accessed through `rusqlite` (bundled) from a
dedicated thread per database. Volumes are small (thousands of rows), access is
single-user, and the originals already use SQLite, which makes the importer a
mapping exercise rather than a conversion.

The Unarr schema is its own. It is not a mirror of the original schema, which
is tied to .NET conventions and to Dapper. The importer inspects the original
`VersionInfo` table to know which migration level it is reading and maps what
it knows, logging what it skips.

`config.xml` is read for: port, URL base, API key, authentication method,
instance name, SSL settings. Reusing the API key is what makes the switch
transparent: every connected client already has it.

## 6. Compatible API surface

Priority is set by what the ecosystem actually calls:

| Client     | Endpoints it depends on |
|------------|-------------------------|
| Prowlarr   | `system/status`, `indexer`, `indexer/schema`, `downloadclient`, `tag` |
| Bazarr     | `series`, `episode`, `episodefile`, `movie`, `moviefile`, `history`, `rootfolder`, `languageprofile` (Sonarr v3 legacy), `system/status` |
| Recyclarr  | `qualitydefinition`, `qualityprofile`, `customformat` (read and write) |
| Overseerr  | `series`/`movie` add, `qualityprofile`, `rootfolder`, `tag`, `languageprofile` |
| Dashboards | `queue`, `calendar`, `wanted/missing`, `system/status`, `health` |
| Official UI| everything, plus `/signalr` and `/initialize.json` |

Verification strategy: contract tests with golden files. Run the real
application on a known database, record responses, replay the same requests
against Unarr on the imported database, diff the JSON. Without this,
compatibility is a hope.

How it is done in practice (phase 1): the fixture under
`tests/fixtures/sonarr/` is a real homelab installation (38 series, 346 files,
two thousand history rows, 40 custom formats, 15 indexers) passed through
`scripts/sonarr-anonymize.py`, which replaces every credential, URL, host,
e-mail and path, pseudonymises release identifiers, and refuses to write if
any original value survives. `make golden` downloads the official
self-contained Sonarr release pinned by SHA-256, runs it without Docker on a
copy of the fixture with its scheduled tasks frozen, and records every request
listed in `golden/requests.txt` plus the provider schemas the UI needs.
`crates/unarr-api/tests/contract.rs` replays the list through the axum router
and diffs status and body, ignoring only keys that depend on the machine or
the moment. Each golden carries the instant it was recorded and the test pins
Unarr's clock to it, so answers that depend on today (`nextAiring`, season
statistics, the wanted lists) stay comparable as time passes. Phase 1 started
on a small synthetic fixture built by driving Sonarr's API (the script was
removed once the real database arrived: it would have overwritten it); moving
to the real database found seven
behaviours the synthetic one could not: regex compilation dominating response
times, culture-aware sort orders, release groups parsed from titles, blocklist
paging, tie order in the wanted lists, the naming example renderer, and the
recorded clock.

The emulated identity is deliberate: `system/status.version` reports the
Sonarr version the goldens were recorded from (`4.0.20.3014`), because clients
branch on it; Unarr's own version goes in `packageVersion` and
`packageAuthor`. Quirks of the original are mirrored rather than fixed (paging
without `sortDirection` sorts descending, `wanted/missing` includes specials
while `wanted/cutoff` does not, paths are matched case-insensitively because
Bazarr calls `episodeFile`), since clients depend on them.

## 7. Metadata providers

The originals do not call TVDB, TMDB and MusicBrainz directly. They go through
proxies hosted by the Servarr team (Skyhook, radarr-api, api.lidarr.audio).
Those are reachable, but intended for the official clients. Phase 1 did not
need any of them: Unarr serves the metadata the importer found in `sonarr.db`,
and the fixture's posters come from the `MediaCover` directory Sonarr left on
disk. Phase 2 did not need them either: searching, deciding and downloading
work on the series the import brought, and `RefreshSeries` completes without
effect. The decision is deferred to phase 3, which adds series and refreshes
them; what is known so far, so the choice can be made there:

- **Servarr proxies (Skyhook for Sonarr).** For: the exact shapes the
  originals consume, so `series/lookup`, `POST /series` and the refresh are
  ports rather than designs, scene mappings and the TVDB-to-TVMaze/TMDB ids
  come from the same place, no key for the user to obtain, one HTTP client
  for all three personalities. Against: a service run for the official
  clients, with no published terms for third parties; it may be closed to an
  unknown user agent at any time, and using it under the original's user
  agent would be deceptive. Unarr would carry its availability as a risk the
  user cannot mitigate.
- **TVDB v4, TMDB and MusicBrainz directly.** For: public APIs with terms
  (TMDB free with attribution, TVDB v4 with a subscription or a project key
  under its licence, MusicBrainz at one request per second with a named user
  agent), no dependency on the Servarr team's goodwill. Against: the
  responses do not match the originals' shapes, so every metadata field the
  UI shows (images, ratings, alternate titles, scene numbering) has to be
  mapped and some have no source (Skyhook adds the XEM scene mappings and
  `sceneSeasonNumber`); TVDB needs a key the user provides or Unarr ships;
  Lidarr's MusicBrainz model (release groups, releases, tracks, fingerprints)
  is the largest port of the three.
- **A hybrid.** Direct providers for identity and metadata, the Servarr
  proxies only for what has no other source (scene mappings), with a
  graceful fallback when the proxy is unavailable. The likely outcome: the
  cost is bounded by phase 3's scope (Sonarr only) and it keeps the user's
  installation working if either side disappears.

The alternatives are laid out with sizes, the TVDB licensing friction, TVMaze
and the public XEM API in `docs/phase-3b-library.md`.

**Decision, November 2026: option A, the Servarr proxies**, behind one
`MetadataProvider` trait in `unarr-core` whose output is the originals'
resource shapes, so that a direct-provider implementation stays additive.
Skyhook for Sonarr now; `api.radarr.video` and `api.lidarr.audio` when those
personalities come.

**User agent: the original's, for now.** During development and testing Unarr
identifies itself to the Servarr proxies exactly as the application it
emulates (Sonarr's user agent and version for the Sonarr personality, later
Radarr's and Lidarr's), so that the responses, rate limits and any
user-agent-dependent behaviour are the ones the goldens were recorded with,
and so that the project does not show up as an unknown client while its
correctness is still being established. This is a deliberate, temporary
exception to the rule that the emulated identity stops at the API surface
(section 12). Its exit condition is written down so it is not forgotten:
once phase 3b's exit criteria are met and the metadata path is known to be
correct and complete, the user agent becomes Unarr's own, honest one
(`Unarr/<version> (+https://github.com/bateman/unarr)`), the Servarr team is
told, and the fallback path (B, TVMaze plus XEM) is what Unarr implements if
they refuse. The user agent is one constant in the HTTP client, read from a
single place, so the switch is a one-line change plus a golden re-record;
nothing else in the code may depend on which one is in use. Towards every
other upstream (indexers, download clients, notification targets) Unarr
already uses its own user agent and keeps doing so.

Keys, where a provider needs one, are the user's or Unarr's own in every
case.

## 8. Phases

1. **Sonarr, read-only.** Import `sonarr.db` and `config.xml`; read endpoints;
   `system/status`; minimal SignalR. Exit criterion: the official Sonarr UI and
   Bazarr work against Unarr. *Done, November 2026*: 89 golden requests
   recorded on an anonymised real database replay identically, the embedded
   UI browses series, calendar, activity, wanted, settings and system pages
   after a forms login, and `scripts/bazarr-replay.py` confirms every field
   Bazarr reads. See `docs/phase-1-sonarr.md` for the status against each
   exit criterion.
2. **Acquisition.** Release parser, quality and custom formats, Newznab/Torznab
   search, decision engine, SABnzbd, NZBGet, qBittorrent, Transmission, queue.
   Exit criterion: Prowlarr-driven searches end in a download. *Done,
   November 2026, against mocks*: the release parser and decision engine
   carry the originals' own test vectors, the 219 goldens include every
   write, search, grab, queue, command and health shape recorded from the
   real Sonarr pointed at mock indexers and mock download clients, and
   `scripts/prowlarr-replay.py` drives the indexer sync. The homelab run (a
   real Prowlarr, indexer and client) is the one criterion the cloud session
   could not meet. See `docs/phase-2-acquisition.md`.
3. **Library**, split in two after phase 2 landed, each on its own branch and
   squashed on its own:
   - **3a, import, rename, notify.** Completed download handling with the
     originals' import specifications, disk scan, media info through
     ffprobe, rename on the `FileNameBuilder` port, manual import, and
     notifications limited to Plex, Discord and the generic Webhook (the
     maintainer uses the first two; the other implementations keep their
     imported settings untouched). Exit criterion: a mock client's completed
     download ends as a named episode file with history, SignalR and
     notifications matching the oracle. *Done, November 2026, against
     mocks*: 342 goldens recorded from the real Sonarr pointed at the mocks
     (the mock clients completing real clips, a mock Plex, Discord and
     webhook keeping what they receive) replay identically, none pending;
     the import specifications carry the originals' fixtures as unit tests;
     the homelab run (a real NZBGet download found, grabbed and imported on
     the maintainer's NAS in place of Sonarr, Discord notified, the real
     Plex refreshed) passed on 9 November 2026. See
     `docs/phase-3a-import.md`.
   - **3b, metadata, series management, settings writes.** The metadata
     provider decision (section 7), lookup, add, refresh and delete of
     series, MediaCover, the remaining write endpoints (settings pages,
     profiles, custom formats, tags, root folders, import lists), backups.
     Exit criterion: the UI and the Simkl import list add series, and
     Recyclarr syncs profiles through Unarr (Overseerr, in the original
     plan, is not used by the maintainer and left the scope). *Done,
     November 2026, against mocks*: 630 goldens recorded from the real
     Sonarr replay identically, the metadata hosts and Simkl answered by
     recorded and hand-made fixtures; the Sonarr UI adds a series and saves
     every settings page in a browser; the exchange between the real
     Recyclarr 8.8.1 and the real Sonarr replays against Unarr. The homelab
     run is optional for this phase and was not done. See
     `docs/phase-3b-library.md`.
   Sizing from phase 2's 32k lines: 3a about two thirds of it, 3b about half,
   together about equal to phase 2.
4. **Radarr** as a second personality on the existing core. Expected at half
   of phase 2: the core is shared, the importer, movie model, Radarr UI and
   goldens from the Radarr release are new.
5. **Lidarr** last: MusicBrainz, per-track matching, Chromaprint fingerprints.
   Expected at phase 2's size or above; the most divergent personality.

## 9. Non-functional targets

Self-set, to be measured in CI against a fixed fixture database:

| Metric | Target |
|--------|--------|
| Idle RSS, three personalities, 1,000 series + 2,000 movies | under 60 MB |
| Container image, compressed | under 15 MB |
| `GET /api/v3/series` on the fixture, p99 | under 10 ms |
| Cold start to first response | under 1 s |

First measurement, skeleton only (one `system/status` route per personality):
1.7 MiB binary, 4.3 MB idle RSS, unchanged after 500 requests.

End of phase 1 (Sonarr personality only, release build, embedded Sonarr
4.0.20 frontend, measured in the cloud session on x86-64). Two fixtures: the
synthetic one phase 1 started with (4 series, 279 episodes) and the anonymised
real database it ended with (38 series, 1,012 episodes, 346 files, 2,000
history rows, 40 custom formats):

| Metric | Synthetic fixture | Real fixture |
|--------|-------------------|--------------|
| Binary, stripped, with the embedded UI | 12.0 MiB | 12.3 MiB |
| Cold start to first response, import included | under 1 s | 0.34 s |
| Idle RSS after import | 17.8 MB | 25.2 MB |
| RSS after 2,000 `GET /api/v3/series` | 17.9 MB | 27.4 MB |
| RSS after 1,500 more requests across endpoints | | 33.3 MB |
| `GET /api/v3/series`, p50 / p99 | 1.3 / 2.0 ms | 3.9 / 4.7 ms |
| `GET /api/v3/episode?seriesId=&includeEpisodeFile=true` (58 episodes, 53 files), p50 / p99 | | 12.6 / 21.0 ms |
| `GET /api/v3/history` page of 20 with series and episode, p50 / p99 | | 6.9 / 9.0 ms |
| `GET /api/v3/wanted/missing` page of 20, p50 / p99 | | 3.8 / 4.9 ms |
| `GET /api/v3/calendar` one month, p50 / p99 | | 4.5 / 5.4 ms |

End of phase 2 (release build, x86-64, in the cloud session, measured with
`scripts/scale-fixture.py` and a client script over the loopback). Two
fixtures again: the real one (38 series, 1,012 episodes, 1,998 history rows)
and the same scaled 25 times (950 series, 25,300 episodes, 8,650 files,
50,000 history rows), both with the mock indexers and the mock SABnzbd:

| Metric | Real fixture | Scaled fixture |
|--------|--------------|----------------|
| Binary, stripped, with the embedded UI, `reqwest` and `rustls` | 17.7 MiB | 17.7 MiB |
| Cold start to first response, import included | under 1 s | 2.7 s (the import) |
| Idle RSS after import | 22.9 MB | 26.0 MB |
| RSS after an RSS sync and three searches | 56.8 MB | 68.3 MB |
| RSS after a full `history/since` with series and episodes | 73.9 MB | 88.7 MB |
| `GET /api/v3/series`, p50 / p99 | 2.8 / 3.6 ms | 49.5 / 60.0 ms |
| `GET /api/v3/queue` page of 20, p50 / p99 | 1.3 / 1.7 ms | 1.5 / 1.9 ms |
| `GET /api/v3/health`, `system/task`, `command`, p99 | under 1 ms | under 1 ms |
| `RssSync` command (18 reports from the mocks) | 0.17 s | 0.18 s |
| `GET /api/v3/release?episodeId=` (one indexer round trip each), p50 | 76 ms | 74 ms |
| `GET /api/v3/history/since` from the epoch, with series and episodes | 1,998 rows, 5.7 MB, 0.27 s | 50,000 rows, 144 MB, 6.8 s |

The two targets the phase 1 review left: the full `history/since` is
streamed a thousand rows at a time, so the 144 MB answer passes through
without staying (the process is 20 MB heavier after it, not 150, and glibc
gives the rest back on the next scheduler tick); `/series` on 950 series went
from 86 ms to 50 ms by not decoding the cast (2.5 MB of JSON no resource
shows) and loading only the queue's series for the queue, and the rest is the
950 rows' remaining JSON columns (35 ms) and the season statistics over
25,000 episodes (15 ms). The design target of 10 ms at that size is not met;
section 15 says what the next step would be.

The idle figures are what `malloc_trim` leaves after the import: without it
the scaled fixture's import left the process at 232 MB, holding pages it no
longer used. The real fixture is still a small library next to the
1,000-series target, so the RSS figures remain a floor; they do say the
per-series cost is a few hundred kilobytes. The first measurement on the real fixture found the
episode-file endpoints at 2 s and the full history at 49 s: every file was
matched against 249 custom-format specifications whose regexes were compiled
on each call. A per-pattern cache brought them to the figures above; no other
cache exists yet, every answer is built from SQLite on request.

End of phase 3a (release build, x86-64, in the cloud session, the Python
mock on the loopback, the originals' bundled ffprobe). The same two
fixtures; the clip the mock completes is a 120 KB Matroska file, so moving
it costs nothing and the figures are the pipeline's own:

| Metric | Real fixture | Scaled fixture |
|--------|--------------|----------------|
| Binary, stripped, with the embedded UI, notifications included | 19.3 MiB | 19.3 MiB |
| Cold start to first response, import included | 0.21 s | 1.9 s (the import) |
| Idle RSS after import | 18.7 MB | 21.6 MB |
| 200 downloads the mock SABnzbd completed, one `ProcessMonitoredDownloads` | 21 s, 9.5 files/s | |
| 152 more with Discord, webhook and Plex notifications on | 16 s, 9.5 files/s | |
| RSS after those imports, and eight seconds later | 81 MB, 69 MB | |
| `RescanSeries` over every series | 1 s (38 series; the fixture's 346 files are not on disk, so their rows went, as the originals' would) | 38 s (950 series, 8,650 files linked on disk with another size, so each is probed again) |
| `RescanSeries` again, nothing changed | | 1 s |
| RSS after the rescans | 71 MB | 38 MB |

An import costs about 105 ms per file: 45 of them wait on the mock client
(the removal call, which the Python mock serves while deleting the item's
folder), 5 are ffprobe, the rest is Unarr's parsing, decision, move, rows
and SignalR relay; the three notifications add nothing measurable (three
posts to the loopback per import). The RSS the imports leave is the
tracked downloads the client still lists, each with its series and
episodes, until the next refresh drops them, plus what glibc keeps for the
blocking threads; the scheduler tick's `malloc_trim` returned twelve of
the forty megabytes within eight seconds. The scaled rescan's 38 s are the
8,650 ffprobe runs a changed size asks for, in one thread per series; the
walk itself, the second pass, takes a second.

End of phase 3b (release build, x86-64, in the cloud session, the Python
mock on the loopback answering for Skyhook, the image hosts and Simkl with
the recorded fixtures). The scaled fixture's 950 series ask the mock for
tvdb ids shifted by multiples of ten million, each answered with a copy of
one of the 40 recorded shows, posters and fanart included:

| Metric | Real fixture | Scaled fixture |
|--------|--------------|----------------|
| Binary, stripped, with the embedded UI | 21.6 MiB | 21.6 MiB |
| Cold start to first response, import included | 0.42 s | 2.2 s (the import) |
| Idle RSS after import | 19.5 MB | 26.3 MB |
| `GET /api/v3/series/lookup?term=`, p50 / p99 | 2.7 / 3.7 ms | 2.9 / 6.9 ms |
| `GET /api/v3/series/lookup?term=tvdb:`, p50 / p99 | 1.7 / 2.5 ms | 1.9 / 3.5 ms |
| RSS after 400 lookups | 22.4 MB | 28.9 MB |
| `POST /api/v3/series` (Frieren, 65 episodes) | 22 ms, episodes there after 0.19 s | 22 ms, 0.24 s |
| `POST /api/v3/series` (The Pete Holmes Show, 81 episodes) | 5 ms, 0.17 s | 11 ms, 0.17 s |
| `RefreshSeries`, every series, covers fetched and resized | 5.0 s (40 series) | 129 s (952 series, 7.4 a second) |
| RSS right after it | 29.5 MB | 39.9 MB |
| `RefreshSeries` again, nothing changed | 0.4 s | 8.6 s |
| `GET /api/v3/series` after the refresh, p50 / p99 | 4.1 / 5.8 ms | 81.5 / 105.6 ms |
| RSS at rest | 28.2 MB | 38.9 MB |
| `MediaCover` on disk | 4.7 MB | 112 MB |

A lookup is one round trip to the provider and no database write, so the
figures are the mock's answer plus the mapping; the real Skyhook adds its
own hundred milliseconds or more. The add answers once the row is written
and refreshes in the background, as the originals do: the episodes are
there two tenths of a second later. The first refresh is the covers: three
images per series downloaded, decoded and resized to the originals' three
widths, four series at a time; the second pass finds every `Last-Modified`
unchanged and costs 9 ms a series.

Two things these measurements found. The first refresh left the process at
123 to 133 MB and never gave it back: decoding a 1920x1080 fanart and its
resize intermediates are 20 MB allocations, glibc raises its mmap threshold
to the size of each mapping it frees (up to 32 MiB), and from then on such
buffers come from the per-thread arenas, where `malloc_trim` cannot return
what sits below a live allocation. `unarrd` now pins the threshold at 1 MiB
at start (`fs::tune_allocator`), so large buffers are mapped and unmapped
one by one: 29.5 MB after the same refresh, and the refresh itself went
from 10.9 s to 5.0 s. The other is the mock, not Unarr: Python's HTTP
server writes headers and body separately, and on a kept-alive connection
Nagle's algorithm held the second write for the client's delayed ACK,
40 ms on every answer after the first. The lookups measured 44 ms before
`TCP_NODELAY` was set on the mock; the 45 ms an import "waits on the mock
client" in the phase 3a figures above and the 76 ms release search of
phase 2 include the same delay, and were not measured again.

`GET /series` on 950 series is slower than phase 2 left it: 59 ms before
any refresh on this machine and build, 82 ms right after one, 63 ms on the
same data after a restart. Looked at afterwards, call by call: 35 ms are
the database (the season statistics, an aggregate over 25,000 episodes
that fetched each row from the table, 4,700 page reads a request; then
the 950 series rows and their JSON columns), 7 ms build the resources
(four `stat` calls per series for the covers' `lastWrite`, new in this
phase), the rest serialises and sends 2.3 MB. An index holding the
columns the statistics read (migration 8) and remembering the covers'
times between the writes that change them bring it to 59 ms; neither
changes the order of magnitude. Keeping the answer between requests while
no row of the database had changed was built and measured (2.8 ms when
kept, 53 ms when rebuilt after a write) and then removed: the UI asks for
the list once per page load and follows the changes over SignalR, so it
bought fifty milliseconds nobody waits for at the cost of a second code
path and a validity rule tied to the clock. Ten milliseconds at this size
is, for 2.3 MB of JSON built on request, not within reach of an index or
a query; section 15 asks which library size the target is for.

## 10. Naming and distribution

- Project: **Unarr**. The name collides with an unrelated C decompression
  library packaged as `unarr`/`libunarr` in most distributions, and with an
  unrelated commercial product at unarr.app. Accepted knowingly.
- Binary: **`unarrd`**, to avoid clashing with the `unarr` executable shipped
  by Debian and FreeBSD.
- Container image: **`ghcr.io/bateman/unarr`**, built by GitHub Actions for
  amd64 and arm64. The top-level Docker Hub namespace `unarr` belongs to
  someone else.
- Crate names `unarr`, `unarr-core` and friends are free on crates.io as of
  November 2026 and should be reserved early.

## 11. Decisions taken in phase 1

- **Frontend embedded in the binary.** `make ui` copies the official Sonarr
  frontend out of the pinned release into `crates/unarr-api/ui/sonarr/`
  (ignored by git, source maps dropped) and `rust-embed` compiles it in. A
  build without the copy still works and serves a plain page saying so. The
  GPL obligation is met by the licence of Unarr itself. Serving a checkout
  from disk was rejected: it makes the drop-in story depend on a second
  download.
- **Sonarr v4 only, with the legacy `languageprofile` stub.** Bazarr still
  asks for `languageprofile` on v4 and only needs an `id` and a `name`; Unarr
  answers with the single fixed `Deprecated` profile Sonarr v4 itself returns.
  No v3 shapes anywhere else.
- **Authentication as the originals.** API key in `X-Api-Key` or `apikey=`
  for everything under `/api`; for pages, the method from `config.xml`: none,
  basic, or forms with the original PBKDF2 and legacy SHA-256 password hashes
  imported from the `Users` table, so the user's existing login keeps working.
  The API key does not open UI pages and unauthenticated pages redirect to
  `/login?returnUrl=`, exactly as Sonarr does; the UI relies on both.
  `AuthenticationRequired=DisabledForLocalAddresses` is honoured through the
  peer address.
- **Fixture from the official release, not from the migrations.** Running the
  self-contained Linux build proved possible without Docker, and recording
  from the real application catches shape details (omitted nulls, second
  precision dates, sort orders) that reading the migrations would not.
- **Emulated version identity** (section 6).
- **Telemetry off.** `initialize.json` always reports `analytics: false`, so
  the embedded UI never loads the Sentry client, whatever `config.xml` says.
- **Problem-details errors.** 404 on a missing resource returns the
  `application/problem+json` body Sonarr returns; unknown API routes return an
  empty 404, as Sonarr does.
- **Culture-aware ordering.** .NET sorts names with ICU collation: letters
  compare case-insensitively first, lower case before upper on ties, digits
  and punctuation before letters. Release groups, custom formats and providers
  follow that (`unarr_core::collation`), not byte order, because the goldens
  recorded with ICU showed `cakes, CAKES, NTb` and `Language not Italian, LQ`.
  The golden recorder requires ICU for the same reason.
- **Ties keep insertion order.** Where the originals sort on one column
  (blocklist, wanted lists), SQLite's sorter leaves equal keys in scan order,
  so Unarr breaks ties by id ascending whatever the direction.
- **Naming built by a port, not a sample renderer.** The naming examples the
  settings page shows come from `unarr_core::naming`, a port of the originals'
  `FileNameBuilder` (tokens with prefix, suffix, separator and custom format,
  the six multi-episode styles, media info and custom format tokens), checked
  against the real Sonarr for every style and token. Phase 3 renames files
  with it.
- **Release groups parsed like the originals.** Custom formats on history and
  blocklist rows depend on the release group, which the originals extract
  from the title with a set of regexes (bracketed anime groups, known
  exceptions, `-AsRequested`-style suffixes). Those regexes are ported
  verbatim (`customformat::parse_release_group`).
- **From the phase 1 review.** An independent audit of the branch at the end
  of phase 1 found twenty issues; the decisions it led to:
  - The web UI is fetched by CI (`make ui`, tarball cached by release) before
    the tests and before the image build, so a clean checkout and the
    published image both have it. `make test` refuses to run without it
    rather than failing on the login page.
  - The image creates `/config` owned by the nonroot user; a mounted host
    directory needs `--user`.
  - Only `None`, `Basic`, `Forms` and `External` are authentication methods;
    anything else stops start-up. `External` means a proxy authenticates and
    opens the pages, as in the originals. The page guard covers the SPA
    fallback, so deep links redirect to the login too; `returnUrl` must be a
    local path without backslashes; the login failure redirect encodes it.
  - Unknown provider implementations (a newer original) show every text value
    masked, since without a schema nothing says which are secrets.
  - `BindAddress` from `config.xml` is honoured (`--bind` overrides), the
    daemon stops on SIGTERM, a missing `/dev/urandom` is an error rather than
    a weak key, and request spans log the path without the query string.
  - Rows whose series no longer exists are dropped by the importer with a
    note, as unknown tags already were.
  - Lists of ids travel to SQLite as one JSON array (`json_each`), not one
    parameter each, so long histories stay under the 32,766-parameter limit.
  - Resources are rendered on a blocking worker, not on the database thread:
    a full `history/since` no longer stalls every other request.
  - Requests about a few episodes load only their series, not the library.
  - `/series` reads its query like every other route (case-insensitive,
    empty values ignored). Dotted field names (`seedCriteria.seedRatio`) read
    nested settings.
  - The anonymiser regenerates every passphrase and salt in `Config`. The
    first fixture commit on the phase 1 branch held the installation's LAN
    host names in `config.xml`; the branch was squashed into the single
    phase 1 commit on `main`, so no published history carries them. Each
    phase lands the same way.
  - Dead code from earlier steps and the synthetic fixture generator were
    removed.
- **From the second review.** A follow-up audit found eight more:
  - `AllowedHosts` is enforced (`unarr_api::hosts`) with the originals'
    rules: empty or `*` allows everything; otherwise the listed hosts,
    `*.domain` wildcards, the loopback names and the machine's host name
    pass, a request without `Host` passes, anything else is a 400. It is what
    stands between a DNS-rebinding page and `/initialize.json`.
  - `returnUrl` also rejects control characters: browsers strip a tab, so
    `/<tab>/host` would have become `//host`.
  - A `--import-from` directory that holds no database for any selected
    personality, with nothing imported before, stops start-up. A typo used to
    start a blank installation with a fresh key and no login. "Imported" is
    what the database records (`source.app`), not whether the file exists: a
    blank first start creates the file too. For the same reason a generated
    `config.xml` yields to the original's when the import happens later.
  - `0.0.0.0` and `[::]` in `AllowedHosts` are bind addresses, not hosts, and
    are dropped as the originals' parser drops them; alone they would have
    locked every real host out.
  - Basic authentication reads the user on the database thread and verifies
    the password on a blocking worker, after the local-address exemption, so
    a poster-heavy page no longer serialises every request behind PBKDF2.
  - `rootfolder` and `diskspace` touch the disks on the blocking pool: a hung
    network mount parks a blocking thread, not an async worker.
  - The original database is opened read-only, full stop; a leftover
    rollback journal is an error with instructions rather than a repair of
    the user's file.
  - The history event numbers live once, in `HistoryEvent::from_number`; the
    SignalR connection id is a counter, since nothing reads it back.

## 12. Decisions taken in phase 2

- **The originals' sources are the specification, their binary the oracle.**
  Every shape was recorded from the real Sonarr before being implemented:
  `make golden` points the pinned release at a mock Newznab/Torznab server
  and mock SABnzbd, NZBGet, qBittorrent and Transmission
  (`scripts/mock-servarr.py`, mirrored in Rust by `unarr-mock` for the
  tests), and the request list carries bodies, `sleep` lines for the
  originals' background work and `{id:name}` references for the ids they
  number themselves. The replay moves its pinned clock across the sleeps and
  waits for Unarr's own queue instead. Where the originals' answer is not a
  function of the request (queue order on equal time left, the health
  dictionary, the command list with the scheduler's own commands), the
  comparison says so rather than pinning an accident.
- **Ported test vectors, as data.** The parser and the decision engine run
  the originals' own test cases (`tests/vectors/*`, extracted by
  `scripts/port-vectors.py`); a divergence there is a bug in Unarr, not a
  judgement call.
- **Mocks, not credentials.** The fixture's providers stay anonymised; the
  network tests run against the mocks on a free port, with the fixtures'
  recorded addresses rewritten to it. No real indexer, tracker or client was
  contacted from the cloud session, and none of the user's keys exist in
  the tree.
- **One HTTP client, no OpenSSL.** `reqwest` with `rustls`, shared, with the
  originals' timeouts and a Unarr user agent towards indexers and clients.
  The emulated Sonarr identity stops at the API surface.
- **Commands as the unit of work.** A port of the originals' command queue,
  executor and scheduler rather than a Rust-native task system: the UI and
  the ecosystem drive `POST /command`, read the queue back, and expect the
  scheduled tasks to move. Three workers, the same priority and exclusivity
  rules, the same thirty-second tick, commands stored for a day.
- **Pending releases follow the originals, quirks included.** A release held
  back by a delay profile is re-decided on every RSS sync together with the
  feed; the copy from the table turns the stored row into a fallback that
  leaves the queue, as the originals' `AddMany` does. Documented in the tests
  rather than fixed: a client that reads the queue sees what it would see.
- **Provider back-off with the originals' grace periods.** The first failure
  sets level one; download clients are not blocked within five minutes of it;
  nothing escalates in the quarter hour after the start, and no block then
  outlasts five minutes. Without the grace, a single unreachable client
  disappeared from the rotation at once and the health page lit up where the
  originals' stayed quiet.
- **Health verdicts kept by source, run on events.** Startup after five
  seconds, the `CheckHealth` command, a provider change, a provider status
  change; the UI gets a `health` sync each time something changed. The
  checks are the acquisition subset of the originals' with their messages
  and wiki links verbatim.
- **Streaming where a response may not fit.** `history/since` renders and
  serialises a thousand rows at a time behind a cursor on `(date, id)`; the
  full history of a 50,000-row library is 144 MB of JSON and the process no
  longer keeps it. Everything else is small enough to build at once.
- **Memory given back.** glibc keeps what an import or a large response
  freed; `malloc_trim` after the import and on every scheduler tick returns
  it. Idle RSS on the 950-series fixture went from 232 MB to 22 MB with
  nothing else changed.
- **Metadata deferred, with the trade-offs written down** (section 7).
- **`RefreshSeries`, `RescanSeries` and the importer's completed downloads**
  are accepted and complete without effect: the UI's buttons work and the
  scheduler's rows move, the work itself is phase 3's. (Phase 3a did the
  rescan and the completed downloads; `RefreshSeries` waits for 3b.)
- **From the phase 2 review.** An independent audit of `main` at the end of
  phase 2 found 23 issues (9 must fix, 10 should fix, 4 nice to have) and
  some code to delete, treating indexer and client responses as untrusted.
  A recheck of the fixes confirmed them, found one division left unguarded
  and a 24th finding the first pass had missed. All are addressed; the
  decisions:
  - An original `config.xml` is never rewritten. One that lives in Unarr's
    own folder (the installation mounted as Unarr's data, or copied there)
    is read in place; one Unarr did not write is kept even when another
    original is pointed at. A marker (`.config.xml.unarr`) names the files
    Unarr itself wrote, which are the only ones a later import replaces. The
    phase 1 fix had rewritten the user's file with the settings Unarr keeps.
  - A scene mapping with a season but no scene season shifts nothing: the
    originals compare a nullable, which is false on null. Over a hundred
    fixture rows have that shape; `Dragons.Defenders.of.Berk.S02E05` mapped
    to S04E05 before.
  - NZBGet's own field names (`NZBID`, `NZBName`, `ID`) are read; serde is
    case-sensitive where the originals' JSON reader is not, so every queue
    item had id 0 and no title. Both mocks spelled them the wrong way, which
    is why the tests passed; the real captures had empty queues.
  - `POST /release/push` processes the decision it reports, under the
    originals' push lock, honouring `downloadClientId`; it used to answer
    "approved" and grab nothing.
  - Deleting a download's files refuses a path with `..`, one with fewer than
    two components, and one that is, holds or is inside a library root
    folder, with a warning. The originals delete whatever path the client
    reports; this is a deliberate deviation, since one click could remove a
    library when a client sorts into it.
  - Hostile input no longer ends the daemon: bencode nesting is capped at
    128, `size_suffix` saturates on `i64::MIN`, an NZB with an `<!ENTITY`
    declaration is invalid (a 130 KB one expanded to a gigabyte), every
    response body is read through one helper with a 64 MiB ceiling, a
    release profile term with an unfinished group fails to compile instead
    of slicing past the end, a `PUT` whose body is not an object is a 400,
    client time-left figures clamp at ten years and date additions are
    checked, feed dates outside years 1 to 9999 are refused, and titles
    over 512 bytes are not parsed (6,001 bytes of `-` took 26 seconds of the
    database thread).
  - Secrets stay out of logs and API errors: `http::redact` blanks the
    secret query values (`apikey`, `passkey`, `token`, SABnzbd's login
    fields...) in every HTTP error text and download log line, `reqwest`
    errors are logged without their URL, and the shared client sends no
    `Referer` on redirects, which carried the first URL's key to the next
    host.
  - An unknown HTML entity in a feed becomes text (`&amp;oacute;`) rather
    than failing the page and backing the indexer off.
  - `history.download_id` is indexed (migration 5): the per-item lookup on
    every refresh was a table scan, 20 ms on 50,000 rows.
  - One queue refresh at a time, with the dirty flag cleared before the
    clients are asked and restored on failure, so two readers of a stale
    queue poll once and a grab during a refresh is not lost.
  - The rows a grab, a failure, an ignore or a pending release write land in
    one transaction, and a download is marked `Failed` only once its rows
    exist.
  - Tracked downloads no client reports any more leave the cache; the
    originals keep them as not trackable for the life of the process.
  - On SIGTERM the workers take no further command and the daemon waits up
    to eight seconds for the running ones, so a grab is not cut between the
    client and its history row.
  - A bulk grab skips ids a pack already took; a bulk removal stops tracking
    what it removed before reporting a later failure.
  - The blocklist matches titles as the originals' `LIKE '%title%'` does:
    inside the text, ignoring ASCII case.
  - Removed: `IndexerClient::download`, `store::pending::delete_by_series`
    and `added_at`, `CommandQueue::set_result`, the weaker magnet parser in
    `rss` and the second `remove_file_extension`. `scheduler::refresh_intervals`
    had no caller because the hook was missing, not because it was dead: it
    runs on every scheduler tick now, so a changed interval setting applies
    within thirty seconds. `torrent::summary` and `magnet_display_name` stay
    in `unarr-core` although only the mock uses them: they need the bencode
    parser, and moving them means a second one.
  - Importing after a blank first start works again (finding 24): the
    start had seeded `scheduled_tasks` with the default tasks, and the
    import inserted the original's rows with the same ids. The import now
    replaces the seeded rows, last runs included, inside its transaction;
    the scheduler reconciles the table with the defaults on the next start
    as before. NZBGet's time-left division is checked too, since a client
    reporting a negative rate could overflow it.
  - Left as is: a blank start on a build before the marker file wrote no
    marker, so a later import keeps that generated `config.xml`. The log
    line says so and says to delete the file; only unreleased builds are
    affected.
- **From the review of the last phase 2 step.** A separate review of the
  branch's final commit (the measurements step) found one bug and three
  cleanups, all applied: `history/since` answered `[` without closing the
  array when nothing was newer than the date asked, the normal case for a
  client polling for news (the stream ended with the cursor, and an empty
  first page has none); four doc comments had landed on the wrong items;
  the old whole-list `since` query had no caller; the timers and the
  hand-rolled serialisation that measured `/series` are gone, the numbers
  being in section 9. Its two unverified notes are recorded above: the
  memory figures are from a glibc build, not the musl image, and listing
  series without the cast is safe only until something writes a series back
  whole.

## 13. Decisions taken in phase 3a

- **The oracle again, with files this time.** `make golden` points the
  pinned Sonarr at the mocks as before, and the mocks now complete items
  with real clips (`tests/fixtures/media/`, four Matroska files of a few
  hundred kilobytes, one with two audio tracks, one a sample), put files
  on disk for the drop-folder and rescan flows, and stand in for a Plex
  Media Server, a Discord webhook and a generic webhook that keep every
  request they receive. 342 goldens replay, the notification targets'
  recordings included. The replay runs in a temporary directory: the
  library and the clients' downloads are rewritten to it before the import
  and mapped back before comparing, so a golden naming `/tv/The Pitt/...`
  matches what Unarr wrote there. The directory's name carries no dash:
  the release-group parser read `unarr-contract-123` off a file path.
- **ffprobe, not a native reader.** Media info comes from `ffprobe`,
  parsed into the originals' `MediaInfoModel` (schema revision 11). The
  binary is `--ffprobe`/`UNARR_FFPROBE`, else the one on the `PATH`; the
  replay uses the ffprobe the pinned Sonarr ships with when `make golden`
  left it under `.cache`, so codec names and channel counts are the
  oracle's own. The image does not bundle it; the user installs it next to
  Unarr or mounts it in.
- **The import pipeline is a port, step for step.** `mediafiles::{decide,
  specs, aggregate, approve, downloaded, completed, scan, deletion, rename,
  manual}` follow `MediaFiles/EpisodeImport` and its neighbours: the
  fourteen specifications in the originals' order with their messages, the
  quality, language, release-group and scene-name aggregators, the
  upgrade's file deletion into the recycling bin, the folder cleanup, the
  rename on the `FileNameBuilder` port. The specification fixtures of the
  originals assert on mocks, so they are ported as unit tests rather than
  as data; the sample detection and scene-name rules likewise.
- **Imports run on blocking threads; the database is asked, not held.**
  Each import or scan runs under `spawn_blocking`; every database touch is
  one `call_blocking` on the single writer, and ffprobe runs on every file
  before the database phase starts, so the writer never waits on a
  process. A command's result reports the originals' messages (`Failed to
  import`, `Manually imported N files`).
- **Events are returned, not published.** The pipeline hands the runtime a
  list of `ImportEvent`s (`EpisodeImported`, `EpisodeFileDeleted`,
  `DownloadCompleted`, `SeriesRenamed`, `ManualInteractionRequired`...)
  and writes the history rows itself; the runtime relays them to SignalR
  (`episodefile`, `episode`, `series` with its statistics), to the
  notifications and to the event-driven health checks. There is no event
  bus to subscribe to; the order is the call order.
- **Three notification implementations, the others round-trip.** Discord,
  the generic webhook and the Plex Media Server send with the originals'
  payloads (embed fields, colours and titles; the webhook's event types,
  `PUT` or `POST`, basic credentials and headers; Plex's identifying query
  parameters, the section whose location holds the series' root folder,
  the two-hour version cache). The remaining implementations keep their
  settings and flags and answer a test with a validation error saying
  Unarr does not drive them. The plex.tv side (sign-in, server list, the
  ping on every update) is not called: it needs the user's account, and
  the `action` endpoint answers the originals' empty object. A notification
  that keeps failing backs off with the originals' periods (five levels,
  no block within five minutes of the first failure) in the shared
  `provider_status` table; a test whose target never answers escapes as a
  500, as the originals' does.
- **Plex updates are queued and flushed where the originals flush them.**
  The series to refresh queue per server and go out on
  `DownloadsProcessedEvent` (after the monitored downloads are processed),
  `RenameCompletedEvent`, `DeleteCompletedEvent` and
  `HealthCheckCompleteEvent`. The last one is what flushes after a library
  scan or a manual import: an import, successful or not, runs the
  event-driven health checks through the originals' five-second debouncer,
  and the run's completion flushes the queue. The replay compares a
  delete's notifications as a multiset (the originals' flush runs on a
  background handler) and leaves the Plex refreshes around the library
  scans out, since they rode on the originals' minute refresh during the
  recording.
- **Health checks for the import, with the originals' conditions.** The
  recycling bin's writability, read-only mounts under series (from
  `/proc/mounts`), completed download handling off, clients that remove
  completed downloads, and what a failed import says about remote paths
  join the checks; the event-driven ones run only while passing
  (`SuccessfulOnly`) or only while failing (`FailedOnly`) as their
  `CheckOn` attributes say. Health verdicts reach the notifications only
  after the fifteen-minute startup grace, when the checks run once more
  and every standing failure is sent, as `HealthCheckService` does.
- **Quirks kept.** Reprocessing a manual-import item whose name does not
  parse answers 500 (`AugmentingFailedException`); a missing episode file
  answers the `ModelNotFoundException` body; a manual-import item of the
  library has no `name`; the naming's codec tokens read the scene name
  first and the file name second (`GetSceneOrFileName`); the Discord embed
  author is the machine name unless set; the Discord test message carries
  the local time in the invariant culture's shape.
- **Ids the originals make up are pinned or ignored.** NZBGet's drone id
  is a random GUID at grab time; the replay pins it to the recorded one.
  Manual-import item ids are a hash of the path (the originals' too, with
  another function), so the goldens ignore them.

## 14. Decisions taken in phase 3b

- **The provider behind a trait, its output the originals' model.**
  `unarr_core::metadata::MetadataProvider` returns series and episodes as
  `SkyHookProxy` maps them, so the refresh, the lookup and the add are ports
  and another provider has one place to plug in. The user agent towards the
  Servarr proxies is one constant (`unarr_core::http::SERVARR_PROXY_USER_AGENT`),
  still the original's. `UNARR_METADATA_UPSTREAM` points every metadata
  host (Skyhook, the Servarr services, XEM, the artwork hosts, Simkl) at
  one base URL, `/<host><path>`; the tests and the measurements use it, and
  the daemon warns when it is set.
- **No test contacts a metadata host.** `tests/fixtures/metadata/` holds
  the answers by host: Skyhook's for the fixture's series and the ones the
  goldens add, recorded once with the original's user agent; the scene
  mappings cut to those series; XEM's and Simkl's hand-made (XEM refuses
  data-centre networks, Simkl needs an account). The recording Sonarr
  reaches the same files through `scripts/mock-servarr.py` acting as its
  HTTP proxy.
- **Series are written by columns.** The refresh writes the provider's
  fields and the cast, never the user's settings; `PUT /series`, the
  editor, the season pass, the import lists' Clean Library and the move
  update the columns they own. A series read from a list (without its
  cast) is never written back whole; a test checks the cast survives each
  of those writes.
- **Ids continue the original's sequences.** The importer seeds the id
  sequence of every table 3b writes to, so a series added after an import
  gets the id Sonarr would have given. Ids are never handed out again, as
  the originals' `AUTOINCREMENT` tables do: asked on the fixture, the real
  Sonarr gives id 2 to an exclusion created after the only one was deleted.
- **Covers.** Each refreshed series has its poster, fanart and banner
  downloaded when `Last-Modified` changed and resized to the originals'
  widths under `<data>/MediaCover/<id>/`; image URLs carry `?lastWrite=`.
  The daemon copies the original's `MediaCover` folder into its own once
  instead of serving, and now writing into, the original's. Lookups of
  series not in the library show covers through `/MediaCoverProxy`.
- **Auto tagging** (`unarr_core::tv::autotag`) is `AutoTaggingService`
  with its nine specifications, applied where `SeriesService.UpdateTags`
  is: after each series of a refresh, refreshed or skipped, on a
  `PUT /series` and in the editor. One deliberate difference: when every
  series is refreshed the originals pass the rules the series as it was
  before the refresh and, if its tags change, write that stale copy back
  whole; Unarr gives the rules what the refresh just stored and writes the
  tags alone. No golden covers the rules (the fixture has none); they are
  unit tests against the originals' code.
- **Tracked downloads follow the library**, as
  `TrackedDownloadService`'s three handlers do: when a refresh removes
  episodes, when a series is added and when one is deleted, the downloads
  concerned are matched again from their title at once and the queue goes
  out, instead of waiting for the next poll of the clients.
- **A series added** is refreshed and scanned as new, then the post-add
  actions run once: the monitoring preset, the `SeriesAdd` notification,
  the searches asked for. Its folder is created only when Create Empty
  Series Folders is set, as the originals do. The series an
  import list brings are added one after the other, each in its own
  transaction, where the originals insert the batch at once.
- **Host settings never rewrite the original's `config.xml`.** A saved
  `config/host` is kept as overrides in the database, applied at start and
  in memory. The session cookie keeps the name it had at start, as the
  originals', so renaming the instance does not log the browser out.
  A save changes the authentication method and requirement, the user name
  and the password; the API key it carries is ignored, as the originals
  ignore it (recorded), and changes only with `ResetApiKey`. Sessions
  last seven days from their last use and are signed with a random key
  kept in the database, not one derived from the API key.
- **Restart from the UI replaces the process.** `POST /system/restart`
  stops the daemon as a signal would and executes the same program again
  with the same arguments; under Docker the container keeps running.
  `POST /system/shutdown` ends the process, and the container's restart
  policy decides what follows.
- **PUT without a route id.** The UI saves settings pages with `PUT` on
  the collection path and the id in the body. As recorded: the body id
  wins over the route id, the route id fills in when the body has none,
  and neither answers 400 with the `'Id'` validation failure.
- **Quality definitions after a bulk update** are compared by shape in the
  replay: the original answers `GET /qualitydefinition` from a five-second
  cache, Unarr from the database. Recyclarr reads them back at once, so
  its recorded exchange differs there and nowhere else
  (`scripts/recyclarr-replay.py`, `--settle` when run against the
  original).
- **Recyclarr is verified against the real client's recording.**
  `tests/fixtures/recyclarr/sonarr-exchange.json` is what Recyclarr 8.8.1
  and the original Sonarr said to each other through a proxy, on the
  maintainer's configuration, over a drifted instance and two syncs.
  `make recyclarr` sends the recorded requests to a running `unarrd` and
  compares the answers. A first stand-in written from Recyclarr's source
  assumed a deletion the real client does not make, and was removed.
- **Import lists: Simkl only.** Provider CRUD, `test`, `testall` and
  `action` work for every implementation's settings; `ImportListSync`
  fetches Simkl lists and nothing else, the others keep round-tripping
  untested. Clean Library runs once every list has synced and one lost an
  item, matching ids as the originals do. The health checks tied to
  provider statuses run before the command that changed them is recorded
  as ended; the replay still runs one `RefreshMonitoredDownloads` by hand
  before `health-after-importlist-sync`, where the recorder's scheduler
  had run it.
- **Backups.** The `Backup` command writes a zip of Unarr's own database
  and `config.xml`, in the originals' layout; a restore, from a listed
  backup or an uploaded zip, is staged under `<data>/restore` and applied
  at the next start: an Unarr database is moved into place, an original
  Sonarr backup goes through the importer, and the previous files are set
  aside as `*.before-restore` and put back if the import fails. The
  originals restore in place and ask for a restart.
- **Logs.** Besides the standard output and the recent lines kept for
  `GET /log`, the daemon writes log files where the originals keep theirs
  and under their names: `<data>/sonarr/logs/sonarr.txt` in the originals'
  layout (`date|Level|logger|message`), info and above, rolled into
  `sonarr.0.txt`, `sonarr.1.txt`... above one megabyte, fifty archives
  kept (`LogSizeLimit` and `LogRotate`'s defaults; neither is a setting
  yet); `sonarr.debug.txt` and `sonarr.trace.txt` exist when the process
  logs at those levels (`RUST_LOG`). The names follow the personality, so
  what reads `sonarr.txt` today finds it, and Radarr's and Lidarr's will
  be `radarr.txt` and `lidarr.txt` in their own folders. A data folder
  that already holds the original's `sonarr.txt` has it continued, and
  rolled with its archives. `log/file` lists the files, the last written
  first,
  `log/file/{name}` and `/logfile/{name}` serve them; the download takes
  the browser's session, as the backups do. `contentsUrl` is a working
  path where the original's is `/api/v1//<name>`. Lines are not passed
  through the originals' `CleanseLogMessage`: Unarr keeps secrets out of
  what it logs in the first place (section 12). `system/routes` is not
  implemented, the UI does not ask for it.
- **A resized cover that was not made is the full cover**
  (`MediaCoverMapper.MapPath`): `/MediaCover/<id>/poster-1000.jpg`, which
  the series page asks for on a screen with a pixel ratio of two, is
  answered with `poster.jpg` when the file is missing or empty. Unarr
  served the folder as it is and answered 404; the browser checks ran at
  a ratio of one and never asked. Found on the first day on the NAS.
- **`TrustedNetworks` is applied** (`unarr_api::forwarded`), as the
  originals do through ASP.NET's forwarded-headers middleware: a proxy on
  one of the listed networks, or on the loopback, is believed, the address
  it names in `X-Forwarded-For` becomes the client's and its entry is
  consumed, from the last entry back until an address nobody trusted
  vouches for. "Disabled for Local Addresses" exempts a client only when
  no entry is left and its address is local. Until the first days of
  running in place of Sonarr on the maintainer's NAS the setting was
  stored and shown but not read, so every request through Traefik was
  asked to log in. A saved setting applies at once, where the originals
  ask for a restart. `X-Forwarded-Proto` and `X-Forwarded-Host` are not
  read: Unarr's redirects are relative. The setting is not validated on
  save yet (the originals refuse a list that does not parse).
- **Each personality logs on its own.** One process serves several
  personalities and a log line does not say whose it is. Rather than mark
  every task (a span on each spawn, and a line lost to the wrong file
  wherever one is forgotten), the daemon gives each personality a Tokio
  runtime of its own, whose threads, workers and blocking ones alike, are
  started with a `tracing` dispatcher writing to that personality's
  console lines, recent lines (`GET /log`, which is per personality too
  now) and files. A thread started outside the runtime takes the
  dispatcher of the thread that starts it; there is one, the database
  thread, and a new one must do the same or its lines go to the process.
  One case is not covered: the personalities share one HTTP client
  (`http::shared()`), which reuses connections, and a connection's
  background task runs on the runtime of the personality that opened it.
  With two personalities talking to the same host at `RUST_LOG=debug`,
  the HTTP library's own debug and trace lines for one's request can land
  in the other's debug file; Unarr's lines are not affected. A client per
  personality would close it and is not worth it for debug lines.
  What the process logs outside any personality (the start before the
  first is opened, the shutdown) reaches all of them through `ProcessLog`.
  The price is a set of worker threads per personality instead of one for
  all. Considered and not taken: one shared `unarr.txt` listed by every
  UI (no attribution, and not the names the originals' tooling reads),
  spans on every task, and one process per personality (it gives up the
  shared process this project is for).
- **Paging of the exclusions.** `importlistexclusion/paged` answers no
  record for `pageSize=0`, as recorded; the other paged resources clamp
  the size to one, as their goldens say.
- **The allocator's mmap threshold is pinned** (`fs::tune_allocator`, glibc
  only); section 9 says why.
- **Found by driving the UI in a browser** (Playwright against the release
  binary): the three gaps above (PUT without an id, the session cookie,
  the paged exclusions), each recorded from Sonarr before the fix. The
  fixture's `AllowedHosts` of `*` makes the General page refuse to save in
  Sonarr as well; the page was checked after correcting the field.

## 15. Open questions

- Metadata provider: decided (section 7, option A) and implemented behind
  a trait. Open is the switch to Unarr's own user agent towards the Servarr
  proxies, due now that phase 3b's exit criteria are met, and the
  conversation with the Servarr team that goes with it.
- SignalR: `series` and `episode` messages follow the refresh, the adds and
  the edits since 3b. The UI's pages were driven in a browser; no client
  other than the UI listens.
- The collation approximates ICU for ASCII names. Titles with accented or
  non-Latin characters may sort differently from the originals; a full UCA
  implementation is not worth its size until a golden shows the difference.
- `MediaCover` images are fetched by the refresh since 3b (112 MB for 950
  series). Nothing removes the covers of a folder whose series row went
  away outside the API.
- Import lists other than Simkl, and the Kodi and Plex metadata file
  writers, keep their imported settings and are not driven.
- The download clients were ported from the originals' proxies and verified
  against mocks that answer as those proxies expect. Exchanges recorded from
  the maintainer's Sonarr (its trace log read by
  `scripts/downloadclient-capture.py from-log`, anonymised) with a real
  NZBGet 24, a real qBittorrent 5 and a program speaking qBittorrent's API
  under a URL base of its own are in `tests/fixtures/downloadclients/`; the
  mock replays them under `/capture/<name>/` and
  `crates/unarr-mock/tests/captures.rs` runs Unarr's clients against them.
  Those queues were empty when they were taken, so they verify versions,
  settings, categories and the directories derived from them; the
  `nzbget-import` capture from phase 3a adds a queue of five groups, a
  finished one in the history and its deletion; `qbittorrent-import` a
  finished torrent stopped at qBittorrent's inactive-seeding limit next to
  a season pack still downloading, and its removal with data after the
  import; `qbittorrent-compat-import` the same for the program speaking
  API 2.0, where the output path comes from `files` and `properties`
  rather than from the listing. Any exchange with SABnzbd or Transmission
  would extend the replays.
- The memory figures in section 9 come from a glibc build, where
  `malloc_trim` returns freed pages. The published image is a static musl
  build: musl's allocator has no `malloc_trim` (the call is compiled out)
  and behaves differently, so the image's idle footprint after an import
  is not measured yet.
- Listing series skips the `actors` column, the largest JSON one. Every
  series write of 3b updates columns, so nothing writes back a series read
  from a list; a new write must keep to that.
- The `GET /series` target. Section 9's table says under 10 ms at p99 "on
  the fixture": the real fixture's 38 series meet it (5.8 ms). Since the
  phase 1 review it has also been read against the 950-series fixture,
  where it stands at 53 to 59 ms and is out of reach without keeping the
  answer. Which library size the target is for, and at what figure, is
  the maintainer's call; the original's time on the same library has not
  been measured.
- Phase 3b has not run on the homelab: a series added from the UI or by
  the Simkl list, searched, grabbed and imported end to end, against the
  real Skyhook and the real Simkl.
- The import was verified against the mocks and the recorded oracle. The
  maintainer's trace logs of an evening of real use (a real NZBGet
  downloading a season, Sonarr importing each episode as it finished, two
  qBittorrent instances idle, Discord posted, the Plex update failing
  because the server was unreachable from Sonarr at the time) confirm the
  sequence Unarr reproduces: the sample arrives first and is rejected, the
  download stays pending and is imported on a later refresh once the main
  file is unpacked, the RAR files leave a warning, the history entry is
  deleted after the import. One refresh cycle of the NZBGet exchanges is
  the `nzbget-import` capture (`tests/fixtures/downloadclients/`), the
  first recording with downloads in flight; the Discord bodies are not in
  the trace log, so the payloads stay verified by the mock goldens only.
  A second day of logs (fifteen imports: seven NZBGet episodes, a torrent
  season pack, a torrent episode, five single-file items from the
  qBittorrent-compatible program) added the two torrent captures above
  and confirmed, against the behaviour Unarr already has: a season pack
  grabbed for one episode imports that episode only, the other files are
  rejected by `MatchesGrabSpecification` ("Episode 1x06 was not found in
  the grabbed release") and the download waits with
  `ManualInteractionRequired` until it is removed and blocklisted (here by
  a queue-cleaning tool through `DELETE /queue/{id}`), which fails the
  download and removes the torrent from the client a second time; a
  torrent is removed right after its import when qBittorrent has stopped
  it (`stoppedUP`) at a global inactive-seeding limit; and Sonarr's
  `PlexServer` pings plex.tv (`/api/v2/ping`) with the token on every
  import before scheduling the library update, which Unarr does not do
  (see the next point). The real Plex server was off during those logs
  (it follows the television), so Sonarr's refreshes failed. On 9 November
  2026 Unarr itself replaced Sonarr on the maintainer's NAS for one
  episode, from search to the real Plex, which is the homelab run of
  `docs/phase-3a-import.md`.
- The Disk Space page lists every mount on a fixed or network file system
  outside `/etc`, `/boot` and the container runtimes' storage, as Sonarr
  does on Linux, so a single file bind-mounted into the container (an
  ffprobe mounted under `/usr/local/bin`) shows as a disk in both.
  Filtering mounts that are files would be a departure from the original;
  it is left as is.
- Notifications beyond Discord, the webhook and Plex keep their settings
  and send nothing; Plex's sign-in through plex.tv is not offered, the
  token is entered by hand. Health notifications arm fifteen minutes after
  the runtime's own clock started, so a long replay with a pinned clock
  never sends them.
- An import costs about 100 ms per file against the Python mock, most of
  it the client's removal call; against a real client the share is the
  client's. The tracked downloads an import leaves behind stay in memory
  until the next refresh drops them, which is the originals' behaviour
  too, and glibc keeps part of what the blocking threads freed.
- `/series` on a 950-series library costs tens of milliseconds, most of it
  reading and decoding 950 rows with their JSON columns and aggregating
  25,000 episodes on every call. The design keeps every answer built from
  SQLite on request; a per-series cache invalidated by the writes would be
  the next step if the UI's refresh rate makes it matter.
