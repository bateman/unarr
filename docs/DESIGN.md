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
- Replacing Bazarr. It stays, and talks to Unarr: the maintainer's
  decision, not to be proposed again. (Prowlarr was a non-goal too until
  16 November 2026; it is now phase 5, section 8.)

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

What one process costs is that a fault in one personality can reach the
others. A panic does not: it ends the request or the command it happened
in, and a personality that cannot start is left down while the others
serve (section 20).

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
`scripts/arr-anonymize.py`, which replaces every credential, URL, host,
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

**On hold, 12 November 2026.** The maintainer keeps the originals' user
agents towards the Servarr proxies until he says otherwise: the exit
condition above no longer fires by itself when a phase closes. No milestone,
review or work order schedules the switch or the conversation with the
Servarr team; they happen when the maintainer asks for them, for all
personalities at once.

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
4. **Radarr** as a second personality on the existing core, in
   milestones that retrace the Sonarr phases: 4a read-only (importer, read
   endpoints, the Radarr UI, both personalities in one process), 4b
   acquisition (movie parser, `t=movie` searches, decision engine, grab,
   queue), 4c library (import, rename, notifications, `api.radarr.video`,
   movie management, settings writes, an audit, the homelab run). First
   expected at half of phase 2; the map taken after phase 3b shows that
   everything carrying the originals' rules is bound to series and
   episodes, so a movie twin of each has to be written beside the shared
   pieces: about phase 2's size. See `docs/phase-4-radarr.md`.
   *4a done, November 2026*: 145 of the 146 read goldens recorded from the
   real Radarr 6.4.4 on the maintainer's anonymised installation replay
   identically (the one left, the naming examples, needs 4c's file name
   builder); the embedded Radarr UI browses every page of the milestone
   against Unarr on 7878 while Sonarr answers on 8989 in the same process;
   `scripts/bazarr-replay.py --app radarr` confirms the fields Bazarr
   reads. Section 15 records the decisions.
   *4b done, November 2026, against mocks*: an `RssSync` and a
   `MoviesSearch` each end in a grab with the queue, history and commands
   equal to the oracle; 312 Radarr goldens, 1,076 parser cases and the
   decision vectors pass; Prowlarr's Radarr sync replays. The memory target
   does not hold with both personalities (74 MB at rest, section 9).
   Section 16 records the decisions.
   The library milestone was then cut in two, as phase 3 was: **4c** import,
   rename and notifications, with the memory point to settle; **4d**
   `api.radarr.video`, movie management, settings writes, Recyclarr, the
   IMDb watchlist, the audit and the homelab run.
   *4c done, November 2026, against mocks*: the mock clients' completed
   downloads end as named movie files with media info, history and the
   Discord, webhook and Plex notifications equal to the oracle; 463 Radarr
   goldens replay, none pending; the import specifications carry the
   original's fixtures as tests; the pages of 4b and 4c were driven in a
   browser. The compiled patterns are released when idle, which brings the
   idle figure back under the target and leaves the working one above it,
   a miss recorded and deferred (section 9). Section 17 records the
   decisions.
5. **Prowlarr**, by the maintainer's decision of 16 November 2026, as a
   fourth personality (port 9696, `/api/v1`, `prowlarr.db`): the indexer
   manager the other personalities are fed by. New against the shared core:
   the indexer definitions and the engine that runs them, the applications
   it syncs to, its own search, history and statistics. No work order yet.
6. **Lidarr** last: MusicBrainz, per-track matching, Chromaprint fingerprints.
   Expected at phase 2's size or above; the most divergent personality.
   The maintainer runs Lidarr, so the phase has what the others had: his
   installation, anonymised, as the fixture, and a run on his NAS to close.

Not phases, and not decided: doing inside Unarr what Recyclarr (the sync
of guide profiles and custom formats) and Cleanuparr (cleaning the queue)
do beside it today. To be weighed after the personalities, when the
maintainer asks.

## 9. Non-functional targets

Self-set, to be measured in CI against a fixed fixture database:

| Metric | Target |
|--------|--------|
| Idle RSS, three personalities, 1,000 series + 2,000 movies | under 60 MB |
| Container image, compressed | under 15 MB |
| `GET /api/v3/series` on the fixture, p99 | under 10 ms |
| Cold start to first response | under 1 s |

**Make it run, then make it fast** (the maintainer's decision, November
2026, at the end of phase 4c). The targets stay as written. Until Unarr
has shown itself correct and fit to replace the originals day by day,
a measurement that misses one is recorded in the table below and left:
no milestone is held back by it and no behaviour of the originals is
given up for it. Each milestone measures again and updates the table.
The misses are taken up together once the bugs are out and Unarr is the
maintainer's daily driver.

| Target | Measured | Since | State |
|--------|----------|-------|-------|
| Idle RSS under 60 MB | Idle for five minutes: 45 MB (glibc), 40 MB (static musl), both personalities on two small libraries. Working, and until the compiled patterns are released: 100 to 125 MB. The patterns of the two title parsers are 54 MB | 4b | Missed while working; deferred |
| The same, on 1,000 series and 2,000 movies, three personalities | Two personalities on 950 series and 2,123 movies: 51 MB idle after the start and again after five idle minutes; 101 MB after reading the libraries, 130 MB after the parsers worked. Lidarr does not exist | 4c | Met idle with two personalities; missed while working; deferred |
| `GET /api/v3/series` p99 under 10 ms | 5.8 ms on the fixture's 38 series; 53 to 59 ms on 950 series (104 ms with Radarr beside it and a refresh running). No target was set for `GET /api/v3/movie`: 338 ms at p99 on 2,123 movies | phase 1 review | Missed at 950 series; deferred |
| Container image under 15 MB compressed | Not built since both UIs are embedded. The binary is 29.6 MiB, 11.9 MiB under `gzip -9` | | Unknown; close to the limit |
| Cold start to first response under 1 s | 0.20 to 0.46 s on the fixtures, import included (0.46 s with both personalities); 6.1 s on the scaled libraries, which is the import of both databases, done once | 4c | Met on the fixtures; the first start on a large library is the import |
| Idle RSS under 60 MB, measured again at the end of 4d | Both personalities on 950 series and 2,123 movies, started on the databases they had already imported: 0.23 s to the first answer, 23.6 MB at start, 36.0 MB after a minute idle, 38.2 MB after five. Radarr alone: 27.3 MB idle, 89.2 MB while it reads its 2,123 movies, 28.5 MB two minutes later. The run that imports both databases peaks at 197 MB and settles at 72 MB after a minute, 91 MB after the libraries have been read | 4d | Met idle, in the steady state the homelab runs; the import is the peak |
| `GET /api/v3/series` p99 under 10 ms, measured again at the end of 4d | 70 ms on 950 series, with Radarr beside it. No target for the movie routes: `GET /api/v3/movie` 289 ms on 2,123 movies, `GET /api/v3/collection` 123 ms, a page of `GET /api/v3/history` 150 ms | 4d | Missed; deferred, as in 4c |
| Container image under 15 MB compressed, measured again at the end of 4d | The binary with both UIs embedded is 30.6 MiB, 12.3 MiB under `gzip -9` | 4d | Unknown as an image; the binary alone is close to the limit |
| Container image under 15 MB compressed, once the release profile unwinds | The binary is 34.9 MiB, 14.4 MiB under `gzip -9` (15.07 MB): 4.1 MiB and 2.0 MiB more than aborting | #14 | Unknown as an image; the binary alone is at the limit, and over it if the target is read in decimal megabytes |
| Container image under 15 MB compressed, once Brotli answers (`docs/audit/2-http.md`, row 1.19) | The binary is 35.9 MiB, 14.9 MiB under `gzip -9` (15.61 MB), from 35.0 and 14.4 (15.14 MB): Brotli's encoder is 1.07 MiB of it, 0.50 MiB under `gzip -9`; the rest of the change saves 0.2 MiB (`mime-guess` gone) | HTTP low rows | Unknown as an image; the binary alone is under the limit in binary megabytes and over it in decimal ones |
| Idle RSS under 60 MB, once the release profile unwinds | Both personalities on the real library (38 series, 193 movies), fifteen minutes idle: 30 MiB, 11 MiB of it anonymous; 28 and 10 aborting, in the same container | #14 | Met idle |

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
section 19 says what the next step would be.

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
a query; section 19 asks which library size the target is for.

End of phase 4a (release build, x86-64, in the cloud session, a client
script over the loopback). The two real fixtures: Sonarr's (38 series,
1,012 episodes) and Radarr's (193 movies, 100 files, 9,935 credits, 669
history rows). The idle figure is read 35 seconds after the start, once a
memory release has run; "after the requests" is right after some 900
requests to the Radarr side (200 each of the endpoints below, 50 of the
collections, one full `history/since`) and 200 `GET /series`; "at rest" 35
seconds later:

| Metric | Sonarr alone | Radarr alone | Both in one process |
|--------|--------------|--------------|---------------------|
| Binary, with both embedded UIs | 27.3 MiB | | |
| Cold start to first response, import included | 0.27 s | 0.20 s | 0.46 s |
| Idle RSS | 35 to 38 MB | 18 MB | 39 to 45 MB |
| RSS after the requests | 36 MB | 54 MB | 73 MB |
| RSS at rest | 30 MB | 36 MB | 48 MB |
| `GET /api/v3/movie` (193 movies, 0.7 MB), p50 / p99 | | 15.5 / 23.3 ms | 13.9 / 20.1 ms |
| `GET /api/v3/movie/{id}`, p50 / p99 | | 3.2 / 4.8 ms | 2.5 / 3.5 ms |
| `GET /api/v3/history` page of 20 with the movie, p50 / p99 | | 10.6 / 17.7 ms | 10.9 / 17.5 ms |
| `GET /api/v3/wanted/missing` page of 20, p50 / p99 | | 7.8 / 11.4 ms | 8.1 / 11.4 ms |
| `GET /api/v3/calendar` one month, p50 / p99 | | 8.8 / 14.3 ms | 9.2 / 13.1 ms |
| `GET /api/v3/collection` (32 collections, 0.2 MB), p50 / p99 | | 7.8 / 9.6 ms | 8.1 / 11.7 ms |
| `GET /api/v3/history/since` from the epoch with movies (669 rows, 1.9 MB) | | 0.37 s | 0.31 s |
| `GET /api/v3/series`, p50 / p99 | 2.3 / 3.8 ms | | 2.5 / 3.9 ms |

The second personality costs less than the first: the two idle at 39 to
45 MB where Sonarr alone idles at 35 to 38, since the process, its
allocator arenas and the parser tables are there once. That leaves room
under the 60 MB target, but on two small libraries: the target is for
1,000 series and 2,000 movies, and neither side has been scaled in this
milestone. The binary grew by the Radarr frontend (4.4 MB) and its
schemas. The first measurement found Radarr alone at 54 MB at rest after
the requests and never lower: it runs no command runtime yet, so nothing
called `malloc_trim` for it; the daemon now releases memory on a
thirty-second timer for a personality without a scheduler (36 MB).
`GET /movie` builds 0.7 MB of JSON in 15 ms for 193 movies, the files'
media info and the alternative titles included; at 2,000 movies that is
the same order as `/series` at 950 series, and the same question
(section 19).

End of phase 4b (release build, x86-64, in the cloud session, a client
script over the loopback). The Radarr fixture with the mock indexers and
download clients of `scripts/mock-servarr.py`; the fixture's own twelve
indexers and three clients point at hosts that do not resolve, and are
asked all the same. The idle figure is read 35 seconds after the start; "at
rest" 35 seconds after the last request:

| Metric | Radarr alone | Both in one process |
|--------|--------------|---------------------|
| Binary, with both embedded UIs | 28.2 MiB | |
| Cold start to first response, import included | 0.24 s | |
| Idle RSS, before any search | 27 MB | 44 MB |
| RSS after the first interactive search | 51 MB | |
| RSS after the requests | 68 MB | 84 MB |
| RSS at rest | 63 MB | 74 MB |
| `GET /release?movieId=` first, then p50 / p99 (9 releases, two indexers answering) | 0.14 s, then 15 / 20 ms | |
| `GET /release` (the RSS list, 14 releases) | 29 to 40 ms | |
| `RssSync` command (14 reports, 3 grabs on the first run) | 0.05 to 0.07 s | |
| `MoviesSearch` command, one movie | 0.02 s | |
| `GET /queue` page of 20 with the movie (3 items), p50 / p99 | 3.3 / 5.6 ms | |
| `GET /queue` as Unpackerr asks (page of 500), p50 / p99 | 3.3 / 7.4 ms | |
| `GET /queue/status`, p50 / p99 | 0.4 / 1.6 ms | |
| `GET /history` page of 20 with the movie, p50 / p99 | 19.3 / 33.2 ms | |
| `GET /command`, `GET /health`, p50 | 0.2, 0.1 ms | |

The 60 MB target is not met with both personalities once Radarr has
searched: 74 MB at rest, on two small libraries. Nothing leaks: one
search costs what a hundred do. The first search compiles the movie title
parser's patterns, some 21 MB that stay for the life of the process (30 MB
with the providers created, 51 after one search, 52 after a hundred more),
and the command runtime Radarr now runs adds some 9 MB to its idle figure
(18 MB in 4a, 27 now). Sonarr's parser is a second set of patterns of the
same kind; the two were ported apart, as the originals are, and share
nothing. Whether to bring the patterns down (compile fewer of them, or
build them so that the two parsers share the automata they have in
common) or to state the target per personality is an open question
(section 19). The history page takes twice what it did in 4a (10.6 ms):
each row's custom formats are now computed from the release title as the
movie parser reads it, edition and group included, where 4a went without
a parser.

End of phase 4c (release build, x86-64, in the cloud session). Both
personalities in one process on the two fixtures, with the mocks of
`scripts/mock-servarr.py`; the work is the Radarr request list up to the
imports (an interactive search, an RSS sync, three downloads completed,
two imported) with Sonarr idle beside it. The static musl build is the
one the image holds; the image itself was not built.

| RSS, both personalities | glibc | static musl |
|-------------------------|-------|-------------|
| Idle, 35 s after the start | 37.6 MB | 30.1 MB |
| After the work | 122.9 MB | 97.9 MB |
| At rest, 35 s after the last request | 104.4 MB | 97.8 MB |
| After 330 s without a request | 45.4 MB | 39.5 MB |
| After the same work again | 125.1 MB | 98.2 MB |

What the compiled patterns cost was measured apart
(`cargo run --release --example parser_memory -p unarr-core`): the series
parser's 42 MB, the movie parser's 14 MB, both 54 MB; 57.6 MB falls to
10.4 MB when they are released, and all of them compile again in under a
second. The weight is in the series parser, not in the 21 MB of movie
patterns phase 4b pointed at. Two things made the compiled patterns
smaller: lowering the engine's cache limit, some 10 MB for a quarter of
the parsing speed, and unrolling the counted repetitions less, another
10 MB, which changes what the patterns match. Neither reaches the target
and the second gives up equality with the originals. Compiling the
automata the two parsers have in common once was not tried: they were
ported apart and no measurement says how much they share. So the patterns are now
dropped after five minutes without a match and compiled again when next
needed (section 17): the idle figure is back under the target, on glibc
and on musl, and the working figure is not, because parsing one release
title needs most of one parser's patterns. On musl the "at rest" figure
does not fall on its own, as expected of an allocator without
`malloc_trim`; the release of the patterns returns the memory all the
same, since the automata are large allocations that go back to the
system when freed. The musl column is musl's own allocator: the image's
is jemalloc since section 21.6, measured there.

The 60 MB target therefore holds for a process left alone for five
minutes, and not for one that searches every few minutes. With the RSS
sync at the fixture's interval (100 minutes) the process spends most of
its time under the target and rises to some 100 to 125 MB at each sync
and each import. The maintainer's decision: the target stays, the working
figure is a recorded miss (the table at the head of this section) and is
taken up after Unarr has proved itself as a daily driver.
The patterns do not grow with the library.

The same binary on libraries at the targets' size (`make scale-fixture`:
950 series with 25,300 episodes, 2,123 movies with 1,100 files, 50,000
history rows each), both personalities, glibc, fifty requests each over
the loopback. The metadata hosts are pointed at nothing, so Sonarr's
overdue refresh of its 950 series ran and failed through the first
minutes, beside the requests:

| Metric | Scaled libraries |
|--------|------------------|
| Cold start to both personalities answering, both imports included | 6.1 s |
| Idle RSS, 35 s after the start | 50.5 MB |
| RSS after the reads below | 101.1 MB |
| RSS after the parsers worked | 129.5 MB |
| RSS at rest, 35 s later | 109.7 MB |
| RSS after 330 s without a request | 51.6 MB |
| `GET /series` (2.3 MB), p50 / p99 | 72 / 104 ms |
| Sonarr `GET /history` page of 20, p50 / p99 | 31 / 94 ms |
| Sonarr `GET /wanted/missing` page of 20, p50 / p99 | 38 / 69 ms |
| Sonarr `GET /calendar`, p50 / p99 | 34 / 46 ms |
| `GET /movie` (7.4 MB), p50 / p99 | 264 / 338 ms |
| `GET /movie/{id}`, p50 / p99 | 20 / 36 ms |
| Radarr `GET /history` page of 20 with the movie, p50 / p99 | 48 / 130 ms |
| Radarr `GET /wanted/missing` page of 20, p50 / p99 | 111 / 174 ms |
| `GET /moviefile?movieId=`, p50 / p99 | 24 / 36 ms |
| Radarr `GET /queue` page of 20, p50 / p99 | 23 / 42 ms |
| `GET /collection` (0.6 MB), p50 / p99 | 102 / 121 ms |
| Radarr `GET /calendar`, p50 / p99 | 106 / 146 ms |

Idle, the two personalities stay under 60 MB at this size: a library
ten times larger costs 13 MB more than the fixtures' 38. What grows with
the library is the time of the answers built from every row. `GET
/movie` reads and encodes 2,123 movies with their alternative titles,
translations' titles and files on each call, 7.4 MB of JSON in a quarter
of a second, and the UI asks for it on every visit to the index; a page
of twenty of the wanted list or a month of the calendar costs a tenth of
a second, which says they read all the movies to return a few. `GET
/movie/{id}` at 20 ms for one movie says the same of the single read.
None of these has a target; they are recorded with the misses and left,
by the decision at the head of this section. Since then `/calendar` and
the iCal feed ask SQLite for the movies released in the range, and the
two wanted lists sort light rows and read only the page's movies in
full, with the same answers byte for byte. Main against that change,
p50 / p99 over 30 calls on the scaled library:

| Route | Before | After |
|---|--:|--:|
| `GET /calendar`, one month | 103 / 136 ms | 47 / 58 ms |
| `GET /feed/v3/calendar/Radarr.ics`, 90 days | 44 / 87 ms | 6 / 25 ms |
| `GET /wanted/missing`, a page of 20 | 80 to 84 / 106 to 122 ms | 14 to 17 / 17 to 22 ms |
| `GET /wanted/cutoff`, a page of 20 | 78 to 82 / 96 to 105 ms | 16 / 17 to 20 ms |
| `GET /calendar` over thirty years (every movie) | 1,008 ms | 895 ms |

`/queue` and `/movie/{id}` are not changed yet. The originals' times on
the same libraries, with this change, are in
`docs/originals-comparison.md`: on the scaled library Unarr's Radarr
answered `/movie` in 227 ms at p50 (the original 312), `/calendar` in 54
(133), `/wanted/missing` in 23 (34), `/history` in 45 (43) and `/queue`
in 17 (1.6), far less ahead than its Sonarr on the series routes.

### Unwinding instead of aborting

The release profile aborted on a panic since the first commit, for a
smaller binary. It unwinds since issue #14 (section 20 has the decision
and what catches what). Measured on 17 November 2026 in the cloud session
(x86-64, glibc), `main` at `d6c61de` against the branch at `57ca42c`,
both built with the same profile but for `panic`, both personalities on
the real library, with `make compare-originals` (one run each;
`docs/originals-comparison.md` keeps the comparison and its History
row):

| | Aborting | Unwinding | More |
|---|--:|--:|--:|
| Binary, stripped, both UIs embedded | 32,228,728 bytes (30.7 MiB) | 36,558,840 bytes (34.9 MiB) | 4.1 MiB, 13 % |
| The same under `gzip -9` | 12,929,294 bytes (12.3 MiB) | 15,066,465 bytes (14.4 MiB) | 2.0 MiB, 17 % |
| Idle RSS after fifteen idle minutes | 28 MiB | 30 MiB | 2 MiB |
| Anonymous memory, same moment | 10 MiB | 11 MiB | 1 MiB |
| Peak anonymous memory during the reads | 43 MiB | 44 MiB | 1 MiB |
| Anonymous memory five minutes after the reads | 36 MiB | 37 MiB | 1 MiB |
| First answer on both ports | 0.26 s | 0.17 s | within the runs' noise |

The 4.1 MiB are code and tables, not Unarr's own catchers (a few hundred
lines): the clean-up code each function now carries for the values it
would drop while unwinding (`.text` 2.7 MiB larger), the tables that say
where that code is (`.gcc_except_table`, 9 KiB before, 1.0 MiB now) and
the frame descriptions (`.eh_frame`, 0.4 MiB more). At rest it is 2 MiB
of RSS, half of it pages of the larger file. No route is slower for it:
the two binaries alternated in the same container answer within 14 % of
each other in both directions (`docs/originals-comparison.md`).

The image target (15 MB compressed) was "close to the limit" at 12.3 MiB
for the binary alone and is now at it: 14.4 MiB, which is 15.07 MB. The
image itself has still not been built with both UIs, so the row stays
"unknown", and by the decision at the head of this section a miss here
is recorded and left. The alternatives to unwinding that would have kept
the size are in section 20, with why they were not taken.

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
  Unarr does not drive them. Of the plex.tv side, the daily ping that
  keeps the token alive is sent as the originals send it (before each
  library update and connection test, once a day per token, its answer
  ignored); the sign-in and the server list are not offered, since they
  need the user's account, and the `action` endpoint answers the
  originals' empty object. A notification
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
- **A database started from nothing gets the originals' defaults.** At
  every start, as the originals do: a quality definition for every
  quality, the six quality profiles of a new installation when there is
  no profile, the delay profile when there is none. The default profile
  is checked against one the original made (the fixture's "HD-720p").
- **The calendar feed is written by hand.** `/feed/v3/calendar/Sonarr.ics`
  writes what the originals' iCalendar library writes for the same
  episodes (property order, folding at 74 characters, escaping), checked
  line by line against recordings; `DTSTAMP` is the instant of the
  request. The all-day dates are those of the server's time zone, as the
  originals'.
- **Subtitles and other extras are one table.** The originals keep
  `SubtitleFiles`, `ExtraFiles` and `MetadataFiles`; Unarr keeps the
  first two in `extra_files` and imports their rows. With "Import Extra
  Files" on, the listed extensions come in with a download. With it on
  or off, a scan remembers the extras next to episode files, a rename
  takes them along and deleting the episode file sends them to the
  recycling bin, as in the originals. Unarr drives no metadata consumer
  (section 19), and the `MetadataFiles` rows are not imported. Of the
  files that are not subtitles, a scan remembers only those named exactly
  as their episode file (an nfo): a rename gives an other extra the
  episode file's name and nothing else, which would turn a media
  centre's `-thumb.jpg` into a second `.jpg`. The originals hand those
  to their metadata consumers; here they stay where they are.
- **The recycling bin must be a folder of its own.** The daily clean-up
  deletes what is in it, so a save refuses, with the originals' rules and
  messages, a folder that is a root folder or above one, a series'
  folder, the program's or a system folder, and one that is not there or
  cannot be written.
- **Media info is read off the database's thread when it is missing.**
  After a scan the files without media info, or with one older than the
  least schema still read, are probed again, as the originals do; a
  library imported without it would otherwise hold the database for
  minutes.
- **Outgoing connections follow Settings > General while running.** The
  proxy (HTTP, SOCKS4, SOCKS5, with the bypass list as the originals'
  runtime reads it: regular expressions over `scheme://host[:port]`, plus
  addresses, networks and ranges) and the certificate validation level
  are asked for every request, so a save needs no restart. "Bypass Proxy
  for Local Addresses" means what it means in the originals: this
  machine and names without a dot, not the private networks. A save also
  forgets the TLS sessions kept for resuming, or a host accepted while
  validation was off would keep being accepted. One process has one
  HTTP client, so with several personalities in one process
  (`--personalities sonarr,radarr`) the settings of the last one started
  or saved apply to all (section 19).
- **The environment wins over the settings.** The originals' variables
  (`Sonarr__Server__Port`, `Sonarr__Auth__ApiKey`, `Sonarr__Log__Level`
  and the others of the Server, Auth, App, Log and Update sections that
  Unarr has a setting for) are laid over `config.xml` and over what was
  saved, at start and after every save. A blank API key there is ignored:
  an unset variable of a compose file would otherwise make `apikey=`,
  with nothing after it, the key.
- **Log level.** The Log Level of Settings > General applies at once:
  Unarr's own code logs at it and the libraries stay at info; the debug
  and trace files are written only at their levels. `RUST_LOG`, when
  set, is the operator's word: it decides, and the setting is ignored.
- **The console has a level and a format of its own.** `ConsoleLogLevel`
  (`config.xml`, `<App>__Log__ConsoleLevel`, `PUT config/host`) when
  set, else info, or the Log Level when that is quieter, as
  `ReconfigureLogging` has it: debug and trace go to the files, not to
  `docker logs`. The filter in front of everything lets through the more
  verbose of the two levels; the console, the files and the recent lines
  of `GET /log` each keep their own. `ConsoleLogFormat` `Clef` writes the
  console as the originals' `CompactJsonLayout` does (`@t`, `@l`, `@x`,
  `@m`, one object a line). `RUST_LOG` leaves the format to the settings.
- **Secrets are taken out of every log line.** `CleanseLogMessage`'s
  rules, one for one (API keys, passkeys and tokens in URLs, Plex's
  token, user names in paths, download clients' and webhooks' secrets,
  the middle of a remote address), apply to the files, the recent lines
  of `GET /log` and the console alike. An error of an HTTP request
  carries its URL: without this a failed request to Plex wrote its token
  to the log. Each rule is tried only on a line that holds a word it
  needs, which brings the cost from 70 to 1 microsecond a line. The
  console is coloured only on a terminal.
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
  logs at those levels (the saved Log Level, or `RUST_LOG` when set). The names follow the personality, so
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
  ask for a restart. `X-Forwarded-Proto` and `X-Forwarded-Host` were not
  read then; since the live HTTP run they are, from a trusted proxy, for
  the cookie's `Secure` and the login redirect's absolute address, and
  since `core/http-audit-medium-rows` for the host filter too: the three
  headers are walked together, as the framework walks them, and the host a
  trusted proxy names is the one Allowed Hosts checks
  (`docs/audit/7-triage.md`, row 1.3). The setting is not validated on
  save yet (the originals refuse a list that does not parse).
  - What that costs, kept for parity with the originals (recorded on
    Sonarr 4.0.20 and Radarr 6.4.4), found by the review of the branch:
    Allowed Hosts no longer stops DNS rebinding for a browser whose
    request reaches Unarr from the loopback or a trusted network with an
    `X-Forwarded-Host` of the page's choosing. A page rebound to Unarr's
    address is of the same origin as its requests and may add that header,
    and a listed host in it lets `Host: evil.example` through. That
    happens to a browser on the machine Unarr runs on, or behind a proxy
    that passes the client's `X-Forwarded-Host` on: nginx does unless
    configured to set it, Traefik and Caddy replace it with the host the
    client asked for. The homelab's Traefik does.
  - The alternatives weighed: read `X-Forwarded-Host` for the redirects
    only, as Unarr did before, which refuses what the originals let
    through behind a proxy that rewrites `Host`; or read it from the
    networks of `TrustedNetworks` but not from the loopback, which the
    framework trusts on its own. Both differ from the originals, and the
    first brings back the 400 of row 1.3 to whoever runs such a proxy.
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

## 15. Decisions taken in phase 4a

- **The oracle is the Radarr the maintainer runs.** `scripts/radarr-fixture.sh`
  pins the official 6.4.4.10685 release by SHA-256 and runs it without
  Docker on a copy of the fixture with its scheduled tasks frozen, as the
  Sonarr script does; it is a twin of that script, not a mode of it, so
  the script the Sonarr goldens depend on did not change. Milestone 4a
  reads only, so the recording starts no mock: Radarr runs without the
  environment's proxy and reaches nothing. `make ui` fetches both
  frontends, `make golden-radarr` records, `make radarr` keeps the
  original up for comparison.
- **The fixture is real from the first commit.** The maintainer's Radarr
  backup arrived before any code, so the synthetic fixture the work order
  allowed for was never built. The anonymiser takes the application
  (`scripts/arr-anonymize.py --app radarr`, renamed since it serves
  both): the database name, the `/movies` root, the movie and movie-file
  tables and the collections' root folders are Radarr's; the scan that
  refuses to write when an original value survives is the same. The raw
  backup never entered the repository; before the squash the branch's
  history was scanned for 73 original values (keys, hosts, user names,
  provider names, paths) taken from it.
- **A migration set per personality, shared tables written once.** The
  tables both applications have live under `migrations/shared` and are
  applied by both sets. Sonarr's set composes them with its series side
  into the same nine steps, so existing `unarr-sonarr.db` files keep
  their `user_version` and a fresh one has the same schema (checked
  before and after the split). Radarr's is one step: the shared tables as
  they stand today plus the movie side. A `unarr-radarr.db` left by the
  clone behaviour of earlier builds holds Sonarr's tables and nothing
  else; it is **refused** with what to do (move it away, start again with
  the original to import) rather than migrated or silently replaced.
- **Parallel modules, as the work order decided.** `unarr_core::movies`
  (model, store, qualities, availability, custom formats of files and
  history, folder naming) next to the series modules; `unarr_import::radarr`
  next to `sonarr`; `routes::radarr` next to the Sonarr route table, which
  did not change. What was extracted, each at its second use and in its
  own commit: the importer's database reading and shared tables
  (`legacy.rs`, `shared.rs`), the replay's helpers (`tests/common`), the
  feed's line writer (opened to the crate). The custom-format matcher is
  shared and learned three things when its input is a movie's: Radarr's
  own numbering of sources, the quality modifier, and the edition and
  year conditions.
- **Shared handlers serve both; a personality has its own where the
  shape differs.** Tags, root folders, disk space, custom formats, the
  providers with their schemas, delay and release profiles, host
  settings, tasks, logs, backups list and updates are the same code for
  both, reading the schema catalogue, the strings and the library of the
  personality in the state. Radarr has its own quality definitions and
  profiles (modifier, language), languages, indexer flags, tag details,
  config resources and everything about movies. Radarr's 404 problem
  details name RFC 9110 where Sonarr's name RFC 7231: a marker on the
  shared 404 lets Radarr's router say so, instead of a second error type
  through every handler.
- **Radarr runs no command runtime yet.** The queue, the scheduler and
  what they execute are bound to series and episodes; starting them on a
  movie database would fail at the first tick. Until 4b gives Radarr its
  commands: `GET /command` is empty, `health` is empty, `system/task`
  shows the imported rows unchanged, the queue endpoints answer empty in
  the original's shapes, and nothing runs in the background. Every write
  answers 405 and the routes of later milestones the original's empty
  404, as Sonarr's did after phase 1.
- **HTTP clients per personality.** The outgoing settings (proxy,
  certificate validation) and the clients built over them belong to the
  personality's state. The series side keeps taking the process-wide
  clients, which only the Sonarr personality now sets; any other
  personality builds its own, and whatever shared machinery it comes to
  use in 4b (the indexer client, the download client service) is handed
  them through the constructors that already take a client. This closes
  the question phase 3b left; a test saves a proxy for each personality
  and checks the other's is untouched. The user agent towards the Servarr
  proxies is one constant per personality (`Radarr/6.4.4.10685 (ubuntu
  24.04)` for `api.radarr.video`), read by one function; nothing calls
  that host yet.
- **Two originals, two folders.** Each original has a folder of its own,
  with its `config.xml` and its database. `--import-from` takes several
  (repeated, or comma separated in `UNARR_IMPORT_FROM`): each personality
  imports from the one that holds its database, so the homelab keeps its
  read-only mounts, one per application. Without the option each
  personality still looks in `<data>/<personality>` and then in `<data>`.
  A directory given that holds no database of a selected personality
  stops the start, as before.
- **What the original computes is ported and checked on the whole
  fixture, not sampled.** Availability and the release date agree with
  the recorded library for all 193 movies; the custom formats and scores
  of the 100 files, the 669 history rows and the 86 blocklist entries
  replay; the collections' folders come from a port of the folder format
  (`movies::naming`), the file name builder being 4c's.
- **Asked of the original rather than assumed.** A cover's local address
  ends in `?h=` and the first twenty hex digits of the SHA-256 of its
  remote URL (Radarr 6 replaced the file time Sonarr appends); credits
  asked for one movie show their headshots through the cover proxy, one
  credit or all of them do not; `popularity` is a single-precision
  number, printed as such; `/localization` spells its key `Strings`;
  the importer keeps empty strings, since the API shows `""` where the
  original stored it. `wanted/missing` leaves one monitored movie
  without a file out: the fixture could not say whether for its year of
  zero or for having no release date, so the real Radarr was started on
  a copy with one movie's year zeroed and another's dates removed; it
  drops the first and lists the second. Movies sharing a cinema date
  were taken to come out of `calendar` in TMDB id order, inferred from
  two pairs in the goldens; 4b probed it and found otherwise
  (section 16).
- **Left for their milestone, on purpose.** `config/naming/examples`
  (recorded, pending then: the movie file name builder it needs was
  written after 4b, in `movies::naming`, and the golden replays);
  `movie/lookup`, `importlist/movie`, `release`, `rename`, `manualimport`,
  `parse`, `filesystem` (the UI asks for them on pages 4b and 4c make
  work); Radarr's defaults on a blank start (an imported installation has
  them; they come with the settings writes); the edition of a history
  row's release, which the title parser of 4b finds.

## 16. Decisions taken in phase 4b

- **Every shape was recorded before it was written, on mocks.** The
  recording now starts `scripts/mock-servarr.py`: two indexers under
  `/movies` that serve made-up movie feeds
  (`tests/fixtures/indexers/movies`) and the four download clients. The
  original runs without the environment's proxy and the fixture's own
  providers point at `.invalid` hosts, so it reaches nothing else. The
  maintainer's installation contributed facts, not data: that Prowlarr
  manages the indexers (so its sync is what must work), that the delay
  profiles are at zero, and the two queue queries Unpackerr and Cleanuparr
  make, which are goldens.
- **Movie modules next to the series ones; no abstraction over the two.**
  `movies::{parser, request, decision, search, downloads, commands}` are
  ports of Radarr's services, as `parser`, `decision`, `search`,
  `downloads` are of Sonarr's. What they share was extracted when the
  second use arrived, each time in a commit of its own with Sonarr's
  goldens replaying before and after: the download clients take a
  `GrabRequest` (the release, whether what it is of is recent, the seed
  limits, where it came from) instead of a remote episode, and an `App`
  (the application's name and the keys of its category and priority
  settings); the indexer client and the HTTP clients are one per
  personality; the queue's states, ids and removal options, the grab's
  errors and the pending reasons are the series side's types, used as
  they are.
- **One command runtime, told which application it serves.** The plan
  was a queue generic over the command. What the two applications
  actually share is everything but the commands' work: the queue, its
  persistence, the workers, the scheduler, the health service. So
  `CommandBody` lists both applications' commands, `parse_for` and
  `runs_in` say which an application has (a Sonarr name is unknown to
  Radarr, as in the original, and the other way round), and the runtime
  hands Radarr's commands to `movies::commands`. The series-side fields
  of the runtime are built for Radarr and never asked. Radarr's commands
  carry `isTypeExclusive`, which Sonarr's do not, and the queue honours
  it as Radarr's does: no import list sync starts while another runs; an
  imported database continues the original's command ids.
- **Radarr schedules its own list and runs what the milestone brought.**
  The eleven tasks are the original's. `RssSync`,
  `RefreshMonitoredDownloads`, `MessagingCleanup` and `CheckHealth` run;
  `RefreshMovie`, `RefreshCollections`, `Housekeeping`, `Backup`,
  `CleanUpRecycleBin`, `ImportListSync` and the update check are listed
  and skipped until their milestones, so they show as overdue rather
  than as run. Posting one of them answers as for an unknown name.
- **The title parser is Radarr's, quirks included**, with its 1,076
  vectors ported as data: reversed names are not read; the title is cut
  where the simplified title says, counted in UTF-16 units; the German
  tracker pattern's unbounded look-behind is emulated with a group
  checked afterwards. `NetRegex` turned out to mishandle a look-behind
  that ends in an optional back-reference; the release group pattern is
  written as two look-behinds instead. The history and the blocklist now
  compute their custom formats from the title as this parser reads it.
- **The specifications run in the order of the original's source files,
  a folder's own before its subfolders'.** The first reading had them in
  path order, which put the `RssSync` folder's (availability, delay,
  history, tags, monitored, pending, proper) among the others. A release
  rejected for several reasons while a delay holds it showed the real
  order: the rejections come out as the files are compiled. The 39
  decision vectors do not see the order; the golden does.
- **A torrent whose feed names its hash is blocklisted by hash alone.**
  The download a client made of it has the client's hash, so a removal
  with blocklist does not stop the same release from being grabbed
  again when the mock's hashes differ from the feed's, which is what the
  original did in the recording. Kept as it is.
- **Radarr's actions that return a string are negotiated as its
  framework does.** `POST /indexer/test`, `POST /downloadclient/test`
  and `GET /localization` return a string: the text as `text/plain` for
  a client with no `Accept`, with `*/*` in it or asking for plain text;
  the text as a JSON string for one asking for JSON alone (Prowlarr);
  406 otherwise. Asked of the original with each.
- **A queue item's movie shows how it was found.** A download matched
  through the history of its grab carries the movie as read by id, with
  its alternative titles; one matched by its title carries none. The
  original's two code paths differ so, and the queue shows it.
- **The queue and the health verdicts have no order to keep.** The
  original holds its tracked downloads and its verdicts in hash tables;
  the replay compares both as sets. The sort keys of `GET /queue` do
  order it, and those are compared.
- **The queue status sent over SignalR is read after the refresh and
  counts the pending releases.** The first version sent what the cache
  held when a grab ended, without the releases that wait; an assertion on
  the hub's messages in the replay found it. SignalR itself was not
  recorded from the original: the replay asserts that a command's start
  and end, the queue's sync and status and the health sync are sent, and
  that nothing goes out under a name only Sonarr's UI knows.
- **Pending releases are the original's, by movie.** One release per
  movie is listed, the best by quality and then by the preferred
  protocol; a grab drops the pending releases of its quality or lower;
  what an RSS sync rejects is dropped. The stored release is Unarr's
  `ReleaseInfo` as JSON, whose `publishDate` the delay specification
  reads; a row whose release cannot be read (a shape the original stored
  and this port does not know) is skipped with a warning.
- **The health checks are shared and told the application.** The checks
  of the acquisition (indexers, download clients, remote paths, the
  import mechanism, root folders, mounts) are the same code reading the
  movies' folders, linking Radarr's wiki pages and, for the paths of a
  download, keeping the two spaces Radarr's strings have between
  sentences. Radarr's import mechanism check has no hints for a setting
  never saved. Its own checks (collections, removed movies, metadata)
  come with the library milestone; its notifications are not sent yet,
  so a verdict stays in the list.
- **Completed downloads wait.** The refresh marks a completed download
  `importPending` or `importBlocked` with the original's messages and
  deals with failed ones; importing is 4c, so
  `ProcessMonitoredDownloads` only tells the UI the queue changed.
- **The calendar sorts by three dates.** 4a inferred TMDB id order among
  movies of one cinema date. Two pairs chosen so that TMDB ids, titles
  and ids disagree were recorded: the original orders by cinema date,
  then digital release, then physical release, as its controller says,
  and the four pairs the fixture has all follow it.
- **The free space check reads the real disk.** The mock's releases
  meant to be approved are under 8 GB and one is 4 TB, so the check
  rejects that one anywhere; the replay refuses to start with less than
  9 GB free where `/movies` would be and says why, since with less the
  approved releases are rejected and the goldens differ for a reason
  that is not in the code.
- **Status and headers were compared on the running original.**
  `scripts/http-audit.py` (`make http-audit`) sends the GET requests of
  the golden list and thirty probes of the edges to the original and to
  Unarr on the same fixture: 225 requests. Of what differed, one thing
  was Radarr's (the string answers above, fixed). The rest is the
  server both personalities share, most of it listed by the November
  audit as low: no cache headers on API answers, no charset on the JSON
  content type, no compression, no CORS preflight, `HEAD` answered, the
  key as a bearer token refused, a trailing slash not ignored, the
  version header sent on errors too. Not changed in that milestone: it
  was not Radarr's. Since compared on Sonarr 4.0.20 as well, found the
  same there, and brought into line in `unarr-api/src/http.rs`, item by
  item in `docs/audit/2-http.md` ("Live run, and what was changed");
  `/ping` alone is deliberately not made cacheable for a year.
- **Implemented from the source, not replayed.** `MissingMoviesSearch`
  and `CutoffUnmetMoviesSearch` (on the original they would search some
  ninety movies at the indexers' pace) and a release kept pending for
  want of a client. The first two are tried against the mock indexers
  and clients (`crates/unarr-mock/tests/movie_commands.rs`); the pending
  bookkeeping has a unit test.
- **What a review of the branch found, and the recordings it led to.**
  A failed download was recorded, blocklisted and removed, and never
  searched for again: the movie side had no
  `RedownloadFailedDownloadService`. The mock now fails a download in
  the recording, and the original's answer (the failure, the blocklist
  row, a new grab by a search nobody posted) is a golden. A pending
  torrent blocklisted from the queue lost its hash, so the blocklist,
  which knows a torrent by its hash, let it back in; the original's
  `Block` keeps the hash and no flags. A queue removal naming an id
  twice blocklisted twice; the original removes each item once, which a
  golden now shows. A torrent file was fetched through the process-wide
  client, that is through Sonarr's proxy settings: the download clients
  now carry their personality's own. A movie file without a scene name
  was scored by its whole original path, folders included; the original
  reads the file's name. A release pushed for a client that does not
  exist was kept as pending, and an error past the client ended the
  whole batch: both are the original's "any other exception", which
  skips that release, and both copies (series and movies) say so now.
  These were drift between near-copies of the series code. The choice of
  parallel modules stands, for the reasons above; what it costs is that
  a fix to one side must be looked for on the other, and the rules with
  no test of their own (queue, history, blocklist, free space, upgrade,
  repack) now have one.

- **A second review, of `main` after the merge.** A grab whose history
  could not be written was, since the fix above, a release skipped: the
  next release of the same movie or episode went to a client as well.
  The originals publish the grab as an event whose handlers' failures
  are logged; so the grab now counts once the client has the release,
  in both copies, and a test makes the database refuse the history to
  show one release sent. Radarr's history had no "Mark as failed"
  (`POST /history/failed/{id}`): it is there, recorded from the original
  with the search it is followed by. Under a URL base the root with a
  slash after it (`/sonarr/`) was a 404, which is where a reverse proxy
  and the login form send a browser; the slash is now ignored there as
  everywhere. `{Movie TitleFirstCharacter}` and its series twin shed
  marks for the Latin-1 capitals only; they now use the parser's
  `RemoveDiacritics`, so `Š` is `S`, while a letter with no mark to shed
  (`Ø`, `Ł`) stays, as in the originals. A client that refuses the
  answer unencoded (`identity;q=0`) got a 406 after the handler had run;
  what it refuses is no longer shown to the compression layer. Three
  language codes of the originals' map were missing (`bur`, `mao`,
  `tib`). The proper and already-imported rules have their tests, and
  the HTTP tests build either personality and hold the rows that only a
  hand check held. Two of the review's findings rested on memory of the
  original and its source says otherwise, so nothing changed: the movie
  folder name is built from the movie, release date and id tokens alone
  (`GetMovieFolder`), and `{Quality Proper}` is `Proper` for a revision
  above one and never `Repack` (`GetQualityProper`). The naming code
  that repeats Sonarr's is left as it is, with the rest of the
  near-copies.

## 17. Decisions taken in phase 4c

- **One recording, taken in one run.** The Radarr list is recorded whole
  each time it grows. Two recordings hours apart were mixed once and the
  replay's clock, pinned to the recording's, disagreed with the status
  rows of the indexers; the answers carry the time in too many places to
  patch. The recorder freezes the scheduled tasks, and the replay now does
  the same, after an extra health check ran in the middle of one.
- **The mocks complete downloads into a movie library and remember what
  they are told.** `--movies` (and `MockDirs::movies`) gives the mock
  clients' output and the mock Plex's section a place; what Discord, the
  webhook and Plex receive is compared as goldens of their own. Plex's
  updates are sent from a background flush some seconds after the event,
  so the list pauses before it asks the mock what arrived; without the
  pause a deletion's update was recorded after the section had moved.
- **The mock Plex sees the library under another path**, as the
  maintainer's does: the path mapping of the Plex connection is exercised
  by the recording, not by a unit test alone.
- **`RefreshMovie` is not in the recording.** Posted to the original, it
  reached the metadata server despite the environment (twice, changing
  the votes of a movie between recordings). It is accepted and does
  nothing until 4d, when the metadata host gets recorded answers.
- **A parallel import pipeline**, as for 4b: `movies::import` is a port of
  Radarr's `MediaFiles` and `MovieImport`, beside `import`. The ten
  specifications run in the original's order (already imported, free
  space, grabbed release quality, audio track, matches folder, matches
  grab, not multi-part, not sample, not unpacking, upgrade), with its
  messages. What the two sides share was extracted where it already had
  no media notion: the transfer (hardlink, copy, move, verification), the
  recycling bin, the notification transports and their settings, the
  backup. The Sonarr goldens replayed before and after each extraction.
- **The commands kept the original's names, so two of them have two
  meanings.** `RenameFiles` and `ManualImport` exist in both
  applications with different bodies; `parse_for` reads the body for the
  application the runtime serves. `DownloadedMoviesScan` does not send
  its updates to the client and has no "failed to import" message, and a
  posted `Backup` is always a manual one in Radarr: each was the
  recording's correction of a guess.
- **Housekeeping covers what Unarr stores for movies**: seventeen of the
  original's housekeepers, on the tables the importer fills.
- **The collections' root folders have a health check of their own**
  (`MovieCollectionRootFolderCheck`), which the original runs beside the
  movies' one; the fixture triggered it.
- **Compiled patterns are evictable.** A pattern holds its automaton in a
  slot that a registry can empty; `fs::release_idle_memory` empties those
  not matched for five minutes, on the scheduler's tick and on the
  daemon's thirty-second timer, and the custom formats' cache does the
  same. A match on an emptied pattern compiles it again, for both
  personalities and in the same code. Rejected after measuring: a lower
  cache limit on the engine (10 MB for a quarter of the speed) and less
  unrolling of counted repetitions (10 MB, and a different behaviour).
  Not tried: one set of automata shared by the two parsers, and a leaner
  matcher for the patterns without look-around, which would be a second
  engine to keep equal to the originals' one. Five minutes is below the
  RSS interval, so a sync finds the patterns gone and pays the
  compilation, under a second, once per sync.
- **What is stored and not applied, or not sent**, each because nothing
  on the fixture exercises it: the notifications for a movie added, movies
  deleted and an application update. The ping to plex.tv before a library
  update was on this list too, until it was sent (section 13). The file
  date setting and the 3D token were on this list when
  the milestone closed and were done right after. The file date is
  compared as the original compares it, the UTC day the file was written
  against the wanted date, so a file already dated is left alone at the
  next scan, and one whose date cannot be set (a share the process does
  not own) is warned about and not counted as changed; a
  hardlinked file shares its date with the download it came from, in the
  original too. The view count is the probe's to write (two when the
  video stream carries a `stereo_mode` tag), for both personalities,
  though only the movie names read it. A 3D file probed before this
  change keeps no view count until it is probed again: the schema
  revision was not raised for it, which would have sent every file the
  originals probed back to the probe.
- **The maintainer's Radarr logs were used as facts only.** They are not
  anonymised and are not in the tree, in any commit of the branch: they
  confirmed the transfer modes in use, the absence of a recycling bin,
  the Plex mapping and the order of an import's steps.

- **What a review of `main` after the merge found.** A path compared by
  slicing a `str` at the other path's length stopped the process when the
  cut fell inside a character (a download named in Japanese against a
  movie folder in ASCII; the release build aborts on a panic, so both
  personalities went with it, and the same download was tried again at
  the next start): `is_parent_path` compares bytes, and resolves `.` and
  `..` first, as the originals' `DirectoryInfo` does, so a manual import
  cannot name a file of another movie as one already in place. A copy
  that failed half way left its partial file in the library, where the
  original deletes the target on any error. A tracked download imported
  with the movie as it was when the download was matched: two releases
  of one movie arriving together skipped the upgrade rule, and the worse
  file could replace the better; the movie is now read again before an
  import. The original reads its copy the same way, so this is a
  deliberate departure, visible only in that race. Commands read back
  from the table were parsed as Sonarr's whatever the application, which
  lost a Radarr `ManualImport` queued at a restart. Radarr's
  notifications went out through the process-wide HTTP client, that is
  through Sonarr's proxy and certificate settings; they have Radarr's
  own. A Plex error carried its URL, token included, into the log and
  into the answer of a connection test. The movie scan did not probe
  again the files without media info (`UpdateMediaInfoService` on
  `MovieScannedEvent`); it does, held to the probe's own schema revision
  rather than Radarr's 14, which the shared probe does not write.
  Sonarr's extras lacked the guard the movie copy has against an extra
  that would take the video's own name, which with a video extension
  among the extras and a move deleted the episode just imported. A
  rename whose row could not be written ended the whole command, where
  the original logs the file and goes on; `RenameMovie` with an id that
  is no movie renamed the others before failing, where the original
  fails first. Smaller: the alternative titles were read once per file
  shown by a manual import, the pattern cache took its two locks in
  either order, and the golden of the tasks after the library's commands
  was compared by shape only. One finding was not taken: `RescanMovie`
  can run while a rename does, and so it can in the original, whose
  command does not ask for the disk.
- **Correctness first; the targets' misses are recorded, not chased.**
  Decided by the maintainer when the memory figures were put to them:
  section 9 keeps one table of the targets missed or not measured, and
  they wait until Unarr is a daily driver.
- **The hub is recorded with the goldens, and compared by what the UI is
  told, not when.** `scripts/record-golden.py --signalr` connects to
  `/signalr/messages` as the UI does (a WebSocket and the JSON protocol,
  written on the standard library) and keeps the messages under the
  request they followed, in `signalr.json`. The replay cuts the list into
  stretches at its `sleep` lines, where both the recording and the replay
  wait for the background work to end. Messages that carry a resource
  are compared stretch by stretch as sets: the name, the action, the
  resource's id and, for a movie, `grabbed` and `hasFile`; a command the
  list posted counts when it ends, by name and last message. Messages
  that only say "read this again" (`queue`, `queue/details`,
  `system/task`, `health`) are compared over the whole list by name: how
  often and when the original sends them is the work of its debounce
  timers, as are the queue's counts and a command's states before its
  end, and a scheduled command is the clock's doing. The bodies are not
  compared beyond those fields. The order is checked where the original
  has one worth keeping, around each movie file: an import is the file,
  the two wanted lists, the movie; a deletion the movie with its root
  folder, the two wanted lists, the file, the movie again. The recording
  is held to it as the replay is. The movie events reach the hub on a
  task of their own, in the order they were raised, with the context
  the resources are built from read once per run of events: a scan that
  drops a hundred files no longer holds the command and queue messages
  behind a hundred reads of the library's translations. What the comparison found is in
  `docs/phase-4-radarr.md`; two things it settled on the way: a deleted
  file is announced as the original does while it deletes (the movie
  with its root folder, the two wanted lists, the file, the movie
  again, all without a file, even when an upgrade has already put the
  new one in), and the event channels hold 4,096 messages where they
  held 256, since a scan that drops a hundred files sends four hundred
  at once and a listener that fell behind lost the oldest.

## 18. Decisions taken in phase 4d

- **The metadata server is asked for movies behind a movie-shaped trait**
  (`movies::metadata::MovieMetadataProvider`), as section 7 decided for
  the series side: `api.radarr.video` through Radarr's own HTTP client
  and user agent, with every answer the tests need recorded under
  `tests/fixtures/metadata/api.radarr.video/`. No test reaches the real
  server: the original reached it through `scripts/mock-servarr.py` while
  the goldens were recorded, and the replay asks the Rust mock for the
  same files.
- **A movie and the collection it brings are written in one order**, which
  the original leaves to chance: the collection's row first, its refresh
  queued, the titles, translations and credits written, then the movie's
  metadata row once that command has ended (ten seconds at most). The
  original lets the two commands race for the same row, so its popularity
  and votes differ from one recording to the next; Unarr lets the movie's
  own refresh have the last word.
- **The settings writes follow each controller, not one shape.** Where
  Radarr's rules differ from Sonarr's they are Radarr's: a quality
  definition is validated against its neighbours, a profile keeps the
  language its column holds and a taken name is the database's 409, a
  profile in use by a movie, a list or a collection cannot go, a tag's
  label is matched against the original's expression, a delay profile
  takes the order it is sent with. The rules both originals share were
  brought to Sonarr too (a release profile's indexer must exist, a host's
  remote path is mapped once).
- **A collection edited one by one raises its event** (the UI is told and
  the collections' root folder check runs again); a bulk edit raises
  none and queues the refresh of all of them, as the original does.
- **The import lists name movies through the same service as the series'**
  where the API is concerned, and through a movie-shaped one underneath:
  the IMDb lists are fetched (the watchlist the maintainer syncs), the
  rest keep their settings and are skipped. A list saved or added is
  synced at once; the sync of every list waits for the twelve hours the
  original's interval names, the sync of one list does not.
- **The review's suggestion to share the two pipelines is declined**, with
  the measurement that says why. After renaming series to movies and
  episodes to items, the files the review named still differ in 40 to
  100 per cent of their lines (`downloads/monitor.rs` 126 of 220,
  `failed.rs` 392 of 490, `pending.rs` 717 of 650, `tracked.rs` 278 of
  260): what they share is the shape, not the code. The types differ all
  the way down (`TrackedDownload` against `TrackedMovieDownload`,
  `Catalog` against `MovieCatalog`, the events each side raises), so
  sharing them means a trait with six or eight associated types over
  every pipeline stage, which buys no behaviour and puts both replays at
  risk. Left as it is; the pieces that really are the same (the clients,
  the decision specifications' engine, the custom formats, the history
  and the queue's store) are already shared.
- **`DeleteUpdateLogFiles` fails in Radarr** and does nothing in Sonarr:
  the first is what the real Radarr does on an installation that never
  updated, recorded; the second is what Sonarr's recording leaves unsaid.
- **A command another command waits for starts first, and keeps its
  priority.** A movie's refresh waits (ten seconds at most) for the
  refresh of the collection it just brought, which the original queues
  at normal priority behind whatever else is queued; in a run of single
  adds that is behind every other movie's refresh, and the wait only ran
  out. The queue now starts an awaited command before the others
  (`CommandQueue::expedite`). The priority the client reads stays
  `normal`, as the original records it: raising it to `high` would have
  shown in `/command` and on the hub.
- **A movie's path has two lists of rules, as the original has.**
  `MovieController` (POST and PUT) checks the root folders, the other
  movies, the recycling bin and the system folders; `AddMovieValidator`
  (the library import, the lists, the collections) checks the root
  folders, the bin and the other movies, and no system folder. A library
  import that names no root folder at all is the original's 500
  (`ArgumentNullException`); one whose root folder is blank is a 400 on
  the folder's bare name. All recorded (`golden-review`).
- **Paths are compared against the bin and the system folders as the
  original compares them, measured on it.** Equality goes through
  `CleanFilePath`, which resolves `.`, `..` and repeated slashes, so
  `/data/../lib` is `/lib` and is refused; a child goes through `OsPath`,
  which drops `.` and repeated slashes but walks `..` as a name, so
  `//lib/./x` is inside `/lib` and `/data/../lib/x` is not, and Radarr
  adds a movie there. Unarr does the same: the folder that could be
  deleted whole, the system folder itself, is refused however it is
  spelled.
- **A movie the library has is refused by the insert, after every movie
  of the request validated**, as the original's unique index refuses it
  (409): a path rule a movie breaks is reported first, whether or not the
  movie exists. The Discover page adds with `ignore_errors`, so a movie
  TMDB does not know is left out and the others are added; a movie the
  library has still refuses them all, as recorded. Only the syncs (lists
  and collections), which leave out what the library has before they
  add, skip a movie a request added meanwhile.
- **A selected personality that was never imported and whose original's
  database is in none of the `--import-from` folders stops the start.**
  One personality imported earlier does not vouch for another: a folder
  still empty (Docker creates a missing bind-mount source) would
  otherwise start Radarr blank, with a new API key and no login.

## 19. Open questions

- Metadata provider: decided (section 7, option A) and implemented behind
  a trait. The switch to Unarr's own user agent towards the Servarr
  proxies, and the conversation with the Servarr team that goes with it,
  are on hold until the maintainer asks for them (section 7); not a task
  to propose.
- Sonarr's hub says some things fewer times than the original does: after
  a rescan and after an episode file is deleted the original sends
  `episode updated` four times and `wanted/missing` and `wanted/cutoff`
  twice where Unarr sends each once, and once more after a grab; Unarr
  sends one `command updated` more for `MessagingCleanup`. The hub test
  compares the count of every message and names these five steps as the
  exceptions (`COUNT_NOT_COMPARED` in `tests/contract.rs`); a client
  that redraws on each message sees no difference, so they are left.
- One HTTP client served the process until phase 4a; each personality now
  has its own clients and outgoing settings (section 15). What is left of
  it: the series side still reaches for the process-wide clients rather
  than being handed them, which is correct while only Sonarr is built on
  it and must be kept in mind if a second personality ever reuses a
  series-side service that builds its client itself.
- Radarr, from 4a. The Bazarr movie requests replayed are Bazarr's as
  remembered from its source, not recorded from a running Bazarr, and
  Bazarr itself was not run. The fixture has no `MediaCover` folder,
  so the cover addresses are compared and no cover was served. `GET
  /movie` costs 15 ms for 193 movies; nothing was scaled, so what a
  2,000-movie library costs, in time and in memory, is not known, and
  the 60 MB target of section 9 is so far met only on two small
  libraries.
- Radarr, from 4b. The review's suggestion to share the pieces of the
  two pipelines that carry no media notion (the refresh loop over the
  clients, what a grab's failure means, the release comparisons, the
  push validation) was not taken up in this milestone and is open; so is
  the series side the Radarr runtime builds and never asks, and the
  parser's path, slug and scene-title functions, which only the vectors
  call until 4c. The memory target is exceeded: 74 MB at rest with
  both personalities after a search, 63 MB with Radarr alone, on small
  libraries (section 9); the movie parser's compiled patterns are 21 MB
  of it. Bazarr's `RescanMovie`, and any command of a later milestone,
  answered 500 as an unknown name until 4c, and Radarr sent no
  notification until then. The commands and paths
  listed at the end of section 16 were written from the source and not
  replayed against the original. The order of the queue among downloads
  with the same time left, and of the calendar among movies equal on all
  three dates, is not the original's to reproduce (a hash table) or was
  not probed (the fixture has no such movies). The server-wide header
  differences the live audit measured were brought into line afterwards
  (section 16, and `docs/audit/2-http.md`, "Live run, and what was
  changed"); what that table leaves is Brotli, `HEAD` on pages, the cache
  lifetime of static files and of `/ping`. All but `/ping` were brought
  into line since (section 22).
  Three things found on the way concern Sonarr and were left to tasks of
  their own: the same look-behind defect in its release group pattern
  (fixed on `main` while this milestone was under way), the fixture
  script's ICU check that can pass without ICU and its port variable
  that does not move the copy, and its queue status over SignalR, which
  leaves the pending releases out.
- Radarr, from 4d. The settings pages were not driven with a browser in
  this milestone: what each page writes is a golden, the page itself is
  not. The Discover page's recommendations, trending and popular lists
  are not fetched, and a paging parameter that is not a number is
  ignored where the originals refuse the request
  (`docs/audit/8-radarr.md`). A full `RefreshMovie` of every movie is not
  in the goldens, and the metadata row of a movie the library holds is
  deliberately kept when its collection drops it. Auto tagging does not
  apply its tags on a refresh or an edit, as the original's
  `UpdateTags` does; `ProxyCheck` uses the process-wide client, which is
  Sonarr's. A proxy saved in Settings > General applies to the
  connections opened after the save: one already open to a host stays
  direct, or on the old proxy, until it has been idle for 90 seconds and
  the pool lets it go (the clients read the proxy when they connect, not
  for every request on a connection kept alive). The metadata consumers write no files, as the phase
  says. Of the Discover page only the listing and the add are served.
  The homelab run is the maintainer's, after this milestone; the panic
  decision (`docs/audit/7-triage.md`, row 1.13) was taken before it, on
  17 November 2026, and is section 20.
- Radarr, from 4c. The memory target is met idle and exceeded at work;
  by the maintainer's decision it is a recorded miss, deferred with the
  others (section 9). The eviction
  interval (five minutes) is a constant, not a setting. Nothing was
  scaled. The three notifications listed in section 17 are open, as is the
  place of the metadata
  consumers among the extras, inferred from the source. The image was not
  built, so its figure is the static musl binary's. `RefreshMovie`
  does nothing (it does since 4d). The homelab run for Radarr is left to
  the end of 4d.
- The maintainer's Recyclarr configuration for Radarr
  (`tests/fixtures/recyclarr/radarr.yml`) runs since 4d: the exchange
  between Recyclarr 8.8.1 and the real Radarr is recorded
  (`radarr-exchange.json`) and the real Recyclarr, run against Unarr,
  sends the same requests and prints the same output. It syncs two guide
  profiles, six named ones, the MULTi format, the `plex-tmdb` naming and
  the propers setting; the "UHD Bluray + WEB - ITA" profile and the
  "Language not Italian" format are the maintainer's own, made in the UI,
  and `delete_old_custom_formats` leaves the latter alone, as the
  recording shows the original doing. The import list in use is the IMDb
  watchlist (`IMDbListImport`), which 4d drives against a mock; the
  recordings say nothing about the real watchlist, whose id the fixture
  redacts.
- Media info schema revisions. The probe is one for both applications
  and stamps Sonarr's revision (11); Radarr's is 14, and the rows that
  come from its database say so. A file Unarr probed for Radarr would be
  read as outdated by the original if the database ever went back to it,
  and probed again there, which is harmless. What the three revisions in
  between changed in the model was not compared.
- SignalR. Radarr's hub is recorded from the original and compared
  (section 17). Sonarr's is recorded for a list of its own
  (`tests/fixtures/sonarr/golden-hub`, with `GOLDEN_SIGNALR=1`), not for
  the long one: the tags, the root folders, the quality definitions, the
  tasks and the commands, the health after every run, and an episode
  grabbed, imported or left without its file follow that recording and
  are compared with it. Radarr has such a list too
  (`tests/fixtures/radarr/golden-hub`), for what its long list does not
  show. A change to either hub starts from a step added to one of these
  and recorded, which does not mean recording a long list again. Known
  and left: the originals send an episode's message twice under `episode`
  (the calendar's controller shares the name), Unarr once. The UI's pages
  were driven in a browser; no client other than the UI listens.
- `ProxyCheck` and a proxy whose name does not resolve. The original's
  check throws on it and the run of the checks ends there, with no
  verdict for any of them; the message it has for the case ("Failed to
  resolve the IP Address for the Configured Proxy Host") is never
  reached. Unarr gives that message and lets the other checks finish: a
  decision to differ, found while the health checks after a save were
  tried on the original.
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
  rather than from the listing. Four more come from the maintainer's
  Radarr (`radarr-*`): one movie torrent on qBittorrent 5.1 from the grab
  (answered with a JSON summary, not "Ok.") through a refresh at 27 % to
  the refresh that found it stopped and its removal; the same on the
  qBittorrent-compatible program; and NZBGet 26 answering Radarr with a
  queue of nineteen groups that are all Sonarr's, of which Radarr shows
  none. Unarr's clients, configured as Radarr configures them, passed
  against all four as they were, with no request the real Radarr had not
  made. SABnzbd and Transmission, never seen in the maintainer's
  installations, were recorded in a container instead: the real SABnzbd
  4.2.2 and Transmission 4.0.5 on loopback, the original Sonarr 4.0.20 and
  Radarr 6.4.4 on the fixtures (`scripts/sonarr-fixture.sh shell`,
  `radarr-fixture.sh shell`) talking to them through
  `scripts/downloadclient-capture.py proxy`, which keeps whole requests
  (`sabnzbd`, `transmission`, `radarr-sabnzbd`, `radarr-transmission`).
  Each recording covers the test, two or three grabs pushed with
  `/release/push`, queue refreshes, and the removals: for Transmission a
  torrent whose data was in place before the add, verified at 100 %,
  imported, then removed once stopped at the global ratio, and one left at
  0 % removed from the queue with "remove from client"; for SABnzbd a job
  completed and imported, one failed ("Not on your server(s)"), both
  removed from the history, and one removed from a paused queue. The news
  server SABnzbd downloaded from is `scripts/nntp-stub.py`, a stand-in
  that serves yEnc articles cut from a local file and answers 430 for the
  others: the jobs are real SABnzbd jobs, the Usenet side is not.
  `crates/unarr-mock/tests/proxy_captures.rs` drives Unarr's clients
  through each moment of the recordings (`/capture/<name>@<seqs>`) and
  compares every request with the original's: method, path, query,
  JSON-RPC body or multipart parts, credentials and session headers. The
  SABnzbd client needed no change; the Transmission one did, four
  times: the originals always send basic credentials (empty ones as
  `Basic Og==`), keep the version six hours instead of asking
  `session-get` before each `torrent-get`, read the session's seed limits
  only for a stopped torrent, and fold the doubled separator of a
  download-dir ending in `/`. Not recorded: a completed SABnzbd job with
  par2 repair or unpacking, an authenticated Transmission, a magnet
  added to Transmission, and Transmission before 4.0 (without labels).
- The memory figures in section 9 come from a glibc build, where
  `malloc_trim` returns freed pages. The published image is a static musl
  build, where the call is compiled out; its allocator is jemalloc since
  20 November 2026, which returns pages on its own, and its footprint
  after an import and under load was measured then (section 21.6).
- Listing series skips the `actors` column, the largest JSON one. Every
  series write of 3b updates columns, so nothing writes back a series read
  from a list; a new write must keep to that.
- The `GET /series` target. Section 9's table says under 10 ms at p99 "on
  the fixture": the real fixture's 38 series meet it (5.8 ms). Since the
  phase 1 review it has also been read against the 950-series fixture,
  where it stands at 53 to 59 ms and is out of reach without keeping the
  answer. It is a recorded miss, deferred (section 9); which library
  size the target is for is settled when the misses are taken up. The
  original's time on the same library has not been measured.
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
  `PlexServer` pings plex.tv (`/api/v2/ping`) with the token before
  scheduling the library update, which Unarr has done the same way since
  19 November 2026 (section 13; the source caches the ping a day per
  token). The real Plex server was off during those logs
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

## 20. Surviving a panic

Decided by the maintainer on 17 November 2026 (issue #14, the panic
decision of issue #10 and row 1.13 of `docs/audit/7-triage.md`): **a panic
in Unarr's code costs the request or the command it happened in, and
nothing else.** The release profile unwinds (`panic = "unwind"`; it
aborted since the first commit, for a smaller binary), and the places
where a unit of work starts catch what unwinds to them
(`unarr_core::panics`).

Why: one process holds every personality, and the homelab runs them in one
container. With `abort`, a bug in one stopped all of them, and a panic
that repeats (the same release name at every RSS sync, the same row at
every start) was a restart loop with every personality down until someone
stepped in. The originals are separate processes, and inside each an
exception in a controller is a 500 and one in a command marks that command
failed.

Not chosen, with the reason (the issue has them in full): keeping `abort`
and relying on the restart policy, because the cost of one bug grows with
each personality added; one process per personality in one image, because
it gives up the shared runtime and memory and needs a supervisor the
image does not have (it stays available as two containers); unwinding
around requests only, because the commands are where the parsers, the
import and the indexers' answers run; removing the panics instead, which
is worth doing where cheap and cannot be made complete.

What it costs is in section 9 ("Unwinding instead of aborting").

### What catches what

| Where the panic is | Caught by | What it costs |
|---|---|---|
| An API action, or the calendar feed | `http::catch_panic`, inside the pipeline (`finish`) | That request: the 500 the originals give for an exception nothing caught, logged as `Request Failed. <method> <path>: panicked at <file:line:col>: <message>` |
| A page, `/ping`, the login, SignalR's negotiation, a layer (the key, the host filter, the path rewrite) | `http::catch_panic_anywhere`, the outermost layer, as the originals' exception handler is | That request, with the same answer |
| Work a request hands to the blocking pool (`blocking::run`: rendering, file work) | the helper, which returns the panic as its error | That request: a 500 with the panic for its message, not a request that waits |
| A call on the database thread (`Db::call`, `call_blocking`) | `db::answer`, around each call | That call: its caller gets the panic as an error. The thread and its connection go on; a transaction the call had open is rolled back |
| The body of a command | `Runtime::execute` | That command: it ends `failed`, message `Failed`, the panic as its exception, the hub is told. The worker takes the next one |
| The executor's steps around a command's body (arming the health notifications, recording the start, the checks the body asked for) | the same catch in `Runtime::execute`, which covers all that comes before the end is recorded | That command, failed the same way. Its end and its run are recorded as for any other: a scheduled task whose run was not recorded would be due again at every tick, thirty seconds apart |
| The import of one completed download | the import loop (`mediafiles::completed`, `movies::import::completed`) | That download for that run: it stays as it was and is tried again at the next; the downloads after it are still imported |
| Reading one indexer's answer | `indexers::service::in_indexer_order` | That indexer's releases for that sync or search; the others' are decided on |
| A SignalR connection | the connection's task | That connection; the UI opens another |

The 500 was recorded raw from Sonarr 4.0.20 and Radarr 6.4.4 (`make
sonarr`, `make radarr`; `POST /command` with a name neither knows, which
is an `InvalidOperationException` in the controller): status 500,
`Content-Type: application/json` with no charset, `Cache-Control:
no-cache,no-store`, `Expires: -1`, `Pragma: no-cache`, no
`X-Application-Version`, not compressed for a client that takes gzip,
body `{"message": ..., "description": ...}`. Unarr already answered so for
the exceptions it knew (`ApiError::ServerException`, the `Thrown` marker
of `http::finish`); the panic takes the same path. `message` is the
panic's message and `description` is `panicked at <place>: <message>`,
where the originals' holds the exception's type and its .NET stack. For a
path outside the API the originals' answer was not recorded (no request
is known to make a page throw); their handler is the outermost middleware
and writes one answer whatever the path, which the recorded stack shows,
so Unarr gives the same one there.

### The tasks that are not commands

Each either goes on or starts again after a panic, or its loss is
accepted, or its loss ends the process on purpose.

| Task | Where | After a panic |
|---|---|---|
| Command workers (three per personality) | `Runtime::start` | **Start again** (`panics::supervised`), after one second. A command's own panic never reaches them |
| Scheduler tick, which is also the eviction timer of the compiled patterns and the allocator trim | `Runtime::scheduler_loop` | **Starts again**; the tick that panicked is skipped, the next comes thirty seconds later |
| Memory release timer of a personality without commands | `unarrd::serve` | **Starts again** |
| HTTP server (the accept loop) | `unarrd::serve` | **Not started again**, and no way for it to end is known: the loop holds none of Unarr's code, every request runs in its connection's task, and axum (0.8) retries a failed accept itself and never returns. Should it panic all the same, the panic is logged (`panics::spawn_logged`) and that personality accepts no connection until the restart |
| A connection, and the requests on it | axum, one task each | Lost with its connection, if the panic is outside both catch layers (hyper itself); the client connects again |
| Relay of the runtime's events to the hub | `AppState::start_runtime` | **Goes on**: the event that panicked is not sent, the next is |
| Radarr's movie relay | `routes::radarr::movies::relay_movie_events` | **Goes on**, the same way, with its context read again |
| The download monitor | a scheduled command (`RefreshMonitoredDownloads`, `ProcessMonitoredDownloads`) | A command like the others: failed, and run again at its next turn |
| Health check run after the debounce | `Runtime::check_health_later_with` | **Lost** and logged (`panics::spawn_logged`); the next request for a check starts another |
| Queue status broadcast | `routes::queue::broadcast_queue`, and Radarr's | **Lost** and logged; the next change of the queue sends the status |
| Rescan request after a movie import | `Runtime::emit_movie_events` | **Lost** and logged; the movie is rescanned by the next scheduled or manual scan |
| The log writer | no task: the lines are written by the thread that logs, under a lock that is recovered | |
| The database thread | `Db::start` | **Goes on**: each call is caught where it runs (`db::answer`). A second catch in the thread's loop is for what a job does around its call (the line `answer` logs, the release of a result no caller waits for): none of it is known to panic, but the thread owns the only connection, so nothing in a job may end it. That catch reports on the standard error, since a log layer that panicked is one way to reach it |
| The main thread: the start, then the wait for a signal or for a shutdown or restart asked through the API | `unarrd::main` | **Fatal on purpose.** A panic here is not in any personality's work, and without this thread nothing would stop the process cleanly |
| The `healthcheck` command | a process of its own | Its own exit code |

### What a panic leaves behind

- **Locks.** A `std::sync` lock whose holder panicked is poisoned, and
  `lock().unwrap()` would then panic in every later request. Every such
  lock is **recovered** (`unarr_core::sync::Recover`, `lock().recover()`),
  none is treated as fatal. Decided lock by lock, by what the critical
  section does:

  | Locks | What they guard | Why the value is good after a panic |
  |---|---|---|
  | `AppState`: `settings`, `fixed_now`, `metadata`; `Runtime`: `metadata`, `augment`, `media_cover_dir`, `backup`, `host`, `me`; the outgoing settings (`outbound::Shared::settings`) and their TLS session cache | A value replaced whole, or cloned out | The section is one assignment or one clone |
  | `AppState`: `releases`, `movie_releases`; `series_writes::proxied`; the download clients' session and cookie caches, `PINNED_IDS`, `DownloadClientService::last_used`; `IndexerClient::caps`; `Skyhook::daily`; the scene mappings' timestamps; `Notifier::plex_versions`, `plex_pending`; `MovieNotifier::plex_pending`; `customformat` pattern cache; `covers::LAST_WRITES`; `mediainfo::OVERRIDE` | Caches and small maps | An entry is inserted, read or removed; a missing entry is fetched again |
  | `netregex`: a pattern's slot and the registry | Compiled patterns and when each was used | A slot holds a pattern or nothing, and nothing is compiled again |
  | `CommandQueue`: `items`, `awaited` | The commands in memory | Sections change one command's fields or push or remove one; the table has the same |
  | `Runtime`: `health`, `health_runs`, `health_pending`, `health_statuses`, `refreshed_search`; `NewCollection::command` | The health results, what waits for the debounce, the episodes a refresh wants searched | Replaced or drained whole; the next run fills them again |
  | `Downloads` and `MovieDownloads`: `tracked`, `queue`, `published`, `raised`, `last_refresh` | The tracked downloads and the queue built from them | Built again from the clients at each refresh, at most ninety seconds later |
  | `LogFiles`, `LogBuffer`, `ProcessLog` | The log files and the recent lines | Already recovered before this decision: a log line must never fail |

  The `tokio::sync` locks (`push_lock`, the downloads' `refresh_lock`, the
  notifiers' `flushing`) do not poison: unwinding releases them. A
  `OnceLock` whose initialiser panics stays empty and is tried again; a
  `LazyLock` (compiled patterns of fixed text) would panic again at each
  use, which is the same bug costing each request that meets it.
- **Transactions.** Every transaction is a `rusqlite::Transaction`, which
  rolls back when it is dropped, unwinding included
  (`db::tests::a_call_that_panics_is_an_error_and_the_thread_goes_on`).
  One begun by hand and left open would be joined by every later call, so
  the database thread rolls back whatever a call left open, after each
  call that leaves one.
- **Command rows.** A command that panics is ended `failed` in the queue
  and in the table, so no row is left `started`; the clean-up at the start
  (`store::commands::orphan_started`) is still what handles the rows a
  crash, a kill or an abort leaves.
- **Tracked downloads.** An import works on a copy of the tracked
  download and stores it back when it is done: a panic leaves the stored
  one as it was (`importPending`), not `importing`.
- **Files.** A panic between the move of a file and its row leaves what a
  crash at that point always left: a file on disk without a row, which
  the next scan of the series or the movie picks up.

### The start

A personality that cannot start is **logged, left down, and the others
start**: an error or a panic while its database is opened, restored,
migrated or imported, or while its listener is bound
(`unarrd::start`). The line is in the daemon's output and in every
personality's log; `unarrd healthcheck` says `<name>: did not start` and
fails, so whatever watches the container knows (the data directory has
the names in `down`, beside `listening`). The daemon ends only when no
personality started. Before, the first failure ended it, and under a
restart policy it came back to fail again with every personality down.

That the health check fails is a decision (18 November 2026), not a side
effect. The other choice was a container that stays healthy and says so
in its log only: the personality left down would then be found by its
clients failing to connect. The cost is taken with it: a watcher that
restarts unhealthy containers, like the homelab's `auto-recover-cron.sh`,
restarts this one at each of its runs, and the personality that works is
away for the second that takes. That restart is also the only time the
one left down is tried again.

Left as it was, and fatal on purpose: a mistake in the arguments (an
unknown personality, an `--import-from` folder that holds nothing for any
personality selected, a selected personality with nothing to import and
nothing imported). These are the operator's to fix and say so at once;
starting without the personality asked for would hide them. A database
that is there and cannot be read is no longer one of them.

### What still ends the process

- Out of memory and a stack overflow: unwinding catches neither.
- A panic while another unwinds (in a `Drop`), and a panic in a function
  that may not unwind (an `extern "C"` callback): Rust aborts.
- A panic on the main thread (above).
- A fault in C code: SQLite, the allocator.
- `POST /system/shutdown` and `/system/restart`, and a signal, as before.

### The tests, and the switch they use

`make check` runs the unit tests of each catcher: `panics` and `sync`
(`unarr-core`), the two layers (`http`), the blocking helper, the database
thread, the executor (`commands::executor::panic_tests`: a command that
panics in its body and in the steps around it, more times than there are
workers, ends failed with its run recorded for the scheduler), the indexer
isolation.

`make panic-test` runs `crates/unarrd/tests/panic.rs` on a **release
build** of the daemon, in a target directory of its own
(`target/panic-switch`), with Sonarr and Radarr on the fixtures: a panic
in a handler, on the database thread, on the blocking pool, while holding
the settings' lock and in a page each answer the recorded 500 and the
next request is answered; a command armed to panic ends `failed` five
times running and then completes; Radarr's `/ping`, asked every ten
milliseconds by a thread of its own, answers every time; a personality
whose database is not one, or whose start panics, is left down and
reported while the other serves. CI runs it after `make check`.

The switch that makes the daemon panic **does not exist in a published
binary**: it is the Cargo feature `panic-switch` (`unarrd`, `unarr-api`,
`unarr-core`), off by default, which `make build`, the `Dockerfile` and
the image job of CI never enable. Without it there is no
`/api/v3/system/panic`, no `/panic`, no `UNARR_PANIC_AT_START` and no
code that reads any of them; no variable brings them back. A binary
built with it says so on its first line of output.

## 21. Decisions taken for issue #10

The decisions the maintainer took while Unarr was made the daily driver
(issue #10), each with the question, the options weighed, the choice, why,
what it costs and where the evidence is. The estimates of effort were
given with the question and are estimates.

### 21.1 The scheduled tasks on the originals' cadence

Proposed when the comparison with the originals found the difference, and
decided by the maintainer on 15 November 2026.

- **The question.** Unarr's download monitor (`RefreshMonitoredDownloads`)
  asked the download clients every 60 s where the originals ask every
  90 s, as their logs showed on the fixtures. The likely cause, read from
  Unarr's code and inferred for the originals, not checked in their
  source: they keep a task's last run to the tick and arm their timer 30 s
  after a tick ends, so a one-minute task that ends a few milliseconds
  after the tick that pushed it is due only on the tick after next; Unarr
  stored the last run to the second, so the same task was due a minute
  later.
- **Options.**
  - *Run on the originals' cadence* (chosen): task times stored to the
    microsecond, the scheduler sleeping a full period after each tick.
  - *Keep 60 s*: a finished download would leave the queue up to 30 s
    sooner, for half as many requests again to qBittorrent and NZBGet, and
    the comparison's idle work would no longer be the same on both sides.
- **Why.** Unarr mirrors the originals' behaviour rather than improving on
  it (section 6); the clients see the load they saw before the switch, and
  `docs/originals-comparison.md` compares the same work.
- **Cost.** None found: the 30 s of extra wait are the originals' too.
- **Evidence.** Commit `3e81473`; the idle work in the method of
  `docs/originals-comparison.md`.

### 21.2 glibc's malloc arenas left at their default

The maintainer asked for the measurements before any change; the
recommendation that followed them was accepted, and its record merged by
the maintainer, on 16 November 2026.

- **The question.** Unarr's peak anonymous memory during the comparison's
  reads on the scaled library swings between about 140 and 260 MiB from
  one run to the next: glibc gives a thread that allocates under
  contention an arena of its own, and the more arenas a large answer
  (`/movie`, 2,123 movies) has grown, the higher the peak. Capping
  `MALLOC_ARENA_MAX` at start would steady it.
- **Options, all measured** (three fresh processes per setting, 4-core
  x86-64, scaled library):
  - *The default, 8 per core* (chosen).
  - *4 arenas*: the peak 133-192 MiB; 18 % and 38 % of the throughput lost
    with 4 and 8 clients without pause; one user's page loads 3 % slower,
    and no lower peak in that use.
  - *2 arenas*: 131-190 MiB; 47 % and 55 % lost.
  - *1 arena*: the only stable peak, 124-129 MiB; 76 % and 77 % lost, one
    user's page load 16-17 % slower at p50 (40 ms on Radarr's movie index)
    to keep the peak 75 MiB lower for the seconds before the next trim.
- **Why.** Every cap costs more speed than the memory it saves, and the
  highest peaks come from concurrent requests, which no cap below one
  arena removes. What would lower them is answers that allocate less
  (`/movie` and `/series` built whole before they are sent).
- **Cost.** The peak keeps swinging between runs, which the comparison's
  history notes on each run. Whoever runs a glibc build and prefers the
  steady peak sets `MALLOC_ARENA_MAX=1` without a change to the code. The
  published image is a static musl build, where the question does not
  arise: its allocator is jemalloc, with four arenas, since section 21.6.
  The ARM64 NAS was not measured.
- **Evidence.** `docs/originals-comparison.md`, the two `MALLOC_ARENA_MAX`
  tables; commit `c79f6df`.

### 21.3 Import Using Script is not applied, and a warning says so

Decided by the maintainer on 19 November 2026, among the three options
below, the recommended one; for a row the audit rated medium
(`docs/audit/7-triage.md`, `UseScriptImport`).

- **The question.** Sonarr and Radarr can hand each import to a user's
  script (`UseScriptImport`, `ScriptImportPath`), which decides how the
  file reaches the library. Unarr kept and showed the two settings and
  imported its own way (move, copy or hardlink, as the other media
  management settings say), without a word.
- **Options.**
  - *Implement it*: port the original's script import (the script run with
    the import's environment variables, its answer read back), recorded
    on the original with a test script first. Estimated at several hours,
    on a branch of its own.
  - *Deliberate, with a warning* (chosen): estimated at an hour.
  - *Deliberate, with no warning*: the decision and its reason recorded
    and nothing else; whoever turns the setting on still learns nothing.
- **Why.** Nobody known imports through a script with Unarr, and the
  maintainer's installation has it off: the port would cost several hours
  for no one. What made the row a defect was that the difference was
  silent, and the warning removes that for an hour's work. Noted when the
  decision was written down, not weighed in the question: running a
  program the settings name gives whoever can save settings a way to run
  code on the host, which Unarr does not offer today; the originals offer
  it, and a port would bring it along.
- **Cost.** An installation moved from an original that imports through
  a script loses what the script did; the health page says so from the
  first start.
- **How it is done** (the implementer's choices, not asked): a check of
  Unarr's own, `ScriptImportCheck`, rather than a message added to the
  original's `ImportMechanismCheck`, whose verdicts are compared with the
  originals'; no wiki link, the Servarr wiki not being about Unarr; it
  runs at start, on the schedule and five seconds after a save of the
  settings, as `ImportMechanismCheck` runs (`core/health.rs`).
- **To revisit** when someone needs script import: then the first option.

### 21.4 The hub is served over WebSockets only

Decided by the maintainer on 19 November 2026, between the two options
below, the recommended one; for a row the audit rated medium
(`docs/audit/7-triage.md`, row 2.11 of `2-http.md`).

- **The question.** SignalR's client falls back to server-sent events and
  then to long polling when a WebSocket cannot be opened. Unarr serves
  WebSockets only, so a browser behind a reverse proxy that does not pass
  WebSocket upgrades gets no live updates: its pages change only when
  reloaded.
- **Options.**
  - *Serve the two fallbacks*, recorded on the original first: estimated
    at several hours, and a second and third transport of the hub
    protocol to keep in step with the first from then on.
  - *Deliberate* (chosen).
- **Why.** The maintainer's Traefik passes WebSocket upgrades, so the
  fallbacks would serve no one known, for several hours and a protocol to
  maintain. The common reverse proxies pass them too (Traefik and Caddy on
  their own, nginx with the `Upgrade` and `Connection` headers); a proxy
  that does not is configured to, which costs its owner less than the
  fallbacks cost Unarr.
- **Cost.** Behind such a proxy, no live updates until it is configured.
- **How it is done** (the implementer's choice, not asked): the
  negotiation still lists the three transports, as the original's
  recorded answer does (golden `signalr-negotiate`), rather than
  WebSockets alone. A client that cannot upgrade then tries the two others
  and is refused, where with WebSockets alone listed it would stop sooner;
  it gets no live updates either way, and the answer stays the original's.
- **To revisit** if a user's proxy cannot pass WebSockets.

### 21.5 A blank Plex token is not pinged, and does not fail

Taken by the implementer on 19 November 2026, when the ping to plex.tv
was added; for the maintainer to confirm or overturn.

- **The question.** The originals ping plex.tv through a cache keyed by
  the token, and their cache refuses a blank key
  (`Ensure.That(key).IsNotNullOrWhiteSpace()` in `Cached.Get`). With no
  token, the exception escapes `PlexServer.Test` (the connection test
  fails with an error) and `UpdateIfEnabled` (every import's Plex
  notification fails before the library update is queued). A Plex server
  that lets the local network in without a token is never refreshed.
- **Options.**
  - *As the originals*: throw where they throw, the test answering an
    error and the updates failing. Not recorded on the originals, so the
    error's exact answer would be the source's reading.
  - *Skip the ping* (taken): no ping for a blank token, and the test and
    the update go on to the server, which says whether it needs a token,
    as Unarr did before the ping was added.
- **Why.** The originals' failure is a side effect of a cache, not a
  check of the settings: their validator accepts a blank token, and their
  server requests leave it out when blank. Reproducing it would break a
  setup that works with the server, for no client that depends on it.
- **Cost.** An installation with a blank token sees its Plex refreshed by
  Unarr where the original failed: a difference, in the direction of what
  the settings ask for.
- **To revisit** if the maintainer prefers the originals' behaviour.

### 21.6 The image's allocator: jemalloc, tuned

Decided by the maintainer on 20 November 2026, among the options below,
the recommended one, on the measurements of
`docs/originals-comparison.md` ("The image's allocator").

- **The question.** The published image is a static musl build, and
  musl's own allocator takes one lock for every allocation. On the scaled
  library, with four clients without pause, it served 13 requests a
  second where glibc's served 59, with a p95 of 1.9 s against 0.34 s; one
  request at a time, Radarr's `/movie` took 361 ms against 232 ms. Its
  memory is the lowest of all.
- **Options, all measured** (`scripts/allocator-probe.py`, 4-core x86-64,
  scaled library):
  - *Keep musl's own*: 38-40 MiB at rest after the start, 78-80 MiB after
    the load, a peak of 220-228 MiB under four clients; the speed above.
  - *jemalloc with three settings* (chosen): a background thread, four
    arenas, freed pages returned after a second. 46 / 90 MiB, a peak of
    295-326 MiB, 59-60 requests a second, a p95 of 0.31 s; Radarr's
    `/movie` 275 ms.
  - *jemalloc with its defaults*: the same speed, 52 / 123 MiB, a peak of
    552 MiB.
  - *mimalloc*: the same speed, 90 / 234 MiB in version 3 (182 MiB after
    the load told to purge at once, and slower), 406 / 387 MiB in
    version 2.
  - *A glibc image* (distroless's `cc` base instead of `static`): glibc's
    figures, 56 / 75 MiB and 59 requests a second, but the binary is no
    longer static, and the binaries of a release would need glibc on the
    host.
- **Why.** Four times the throughput and a sixth of the p95 under
  concurrent requests, which the web interface sends when a page opens,
  and a quarter off the heaviest page for one user, for 8 MiB at rest and
  12 MiB after the load. Of the allocators that bring the speed back,
  jemalloc so set keeps the least, and the binary stays static.
- **Cost.** Against musl's own: 8 MiB at rest after the start, 12 MiB
  after the load, 75 MiB more at the peak under four clients; 611,200
  bytes more of binary; jemalloc is C, built from source with the image
  (`make` added to the build stage). One request at a time the
  comparison's reads still take 18 to 21 % longer than with glibc.
- **How it is done** (the implementer's choices, not asked):
  `tikv-jemallocator` for the musl target only, so that a glibc build and
  the comparison with the originals keep glibc's allocator; the settings
  compiled in through `_rjem_malloc_conf`, which `_RJEM_MALLOC_CONF` in
  the environment still overrides, setting by setting; the background
  thread and the decay take the place of `release_memory`, which does
  nothing on musl.
- **To revisit** on the ARM64 NAS, which was not measured, or if the
  memory at rest matters more than the speed: then musl's own.
- **Evidence.** Commit `e73c586`; the two tables of
  `docs/originals-comparison.md`, "The image's allocator".

### 21.7 The interface stays in English

Decided by the maintainer on 20 November 2026, among the four options
below; for a row the audit rated low (`docs/audit/7-triage.md`,
`UILanguage`).

- **The question.** Settings > UI > Interface Language is stored and
  shown, but Unarr serves only the English dictionary, so another choice
  changes nothing. The originals ship 44 more dictionaries an application
  in their release (`Localization/Core/*.json`, which `make ui` already
  downloads): Sonarr's 1.8 MB, of which 8 languages are at least 90 %
  translated, Radarr's 4.4 MB, with 17. They also translate text the
  server writes: some 26 health-check messages of Sonarr and 7 of Radarr,
  and the labels of the providers' fields.
- **Options** (measured on the pinned releases; the web interface needs
  nothing else, it reads `/localization` and `/localization/language` and
  takes the list of languages from `/api/v3/language`):
  - *Everything*: the dictionaries, the health-check messages and the
    field labels; about 1.6 MB of binary stored compressed, and
    recordings per language or translation keys for the schemas. The
    largest.
  - *The interface only*: all the dictionaries and the language
    identifier, the server's text left English; the same size, a few
    hundred lines and a recording in another language.
  - *The well-translated languages only*: the same for the languages at
    least 90 % translated, the rest falling back to English key by key as
    in the originals; about 1.06 MB compressed.
  - *English only* (chosen): nothing.
- **Why.** No reason was given with the choice. What speaks for it: nobody
  known uses another language with Unarr, and the binary is already at the
  image's size target (section 9), which the dictionaries would cross.
- **Cost.** Whoever picks another language keeps an English interface,
  and nothing says why. The setting is kept, saved and returned as the
  originals do, so a database moved back to an original keeps the choice.
- **To revisit** when a user asks for another language: then the third
  option, the cheapest that makes the setting work.

## 22. The server outside the API, as the originals' pipeline

The low rows of `docs/audit/2-http.md` left open or partial by the triage
(`docs/audit/7-triage.md`), recorded on Sonarr 4.0.20 and Radarr 6.4.4 with
`scripts/http-probe.py` (scenarios `lowhttp*`) and compared with
`scripts/http-probe-diff.py`; the original's sources read where a recording
could not settle it. What changed, and what was decided on the way:

- **The pipeline's order is the originals'.** In each group of routes, from
  the outside in: the authentication, the URL base's redirect, the answer
  while starting, the cache headers, the action (`api/lib.rs`). The request
  log sits after the host filter and before the path rewrite, as
  `LoggingMiddleware` sits after `UseHostFiltering`.
- **Static files are kept a year.** `CacheableSpecification` is applied as
  written (`http::cacheable`): every answer outside the API is
  `max-age=31536000, public` with `Last-Modified`, except the bootstrap,
  `index.js`, SignalR, the feed and the log files; under the API only the
  covers, and anything asked with `?h=`. An action that sets its own
  headers keeps them: pages (`no-cache, no-store`), a sign-in or sign-out
  (the cookie handler's `no-cache,no-store` and an `Expires` of 1970) and
  the exception handler's answers. `Last-Modified` is the build time of the
  release Unarr emulates (`compat::emulated_build_time`), as the originals
  send theirs: the embedded UI is that release's, so the date changes when
  the UI does. `/ping` as health checks ask it stays uncached, as decided
  in phase 4 (section 16); `/Ping` is kept as the originals keep it.
- **The pages carry cache breakers.** `HtmlMapperBase`'s pattern adds the
  URL base and `?h=` with the MD5 of the file to each asset link of the
  index and login pages, and drops `data-no-hash`; both pages are now the
  originals' byte for byte. With them a changed file is fetched again even
  though the old one was to be kept a year.
- **What is served is what the originals' mappers take.** A file of the
  bundle only where `StaticResourceMapper` would serve it, `/robots.txt`
  from `Content/`, the login page for a path that starts with `/login`,
  the single-page application for a path without a dot; anything else is
  the static controller's 404 in problem details (`ui::static_not_found`),
  kept a year like the rest. Types come from ASP.NET's table
  (`http::content_type`), which says `text/javascript` on Radarr's newer
  runtime and `application/javascript` on Sonarr's. The covers are served
  by `ui::media_cover` rather than tower-http's file server: no `ETag`, no
  ranges, as `MediaCoverMapper` serves them; the `fs` feature of tower-http
  and the `mime-guess` feature of rust-embed are gone with it.
- **`HEAD` is taken by `/ping` alone.** Elsewhere it finds the framework's
  405 endpoint, which the fallback policy guards with the key: 401 without
  it under every authentication method, a session cookie not taken.
- **Routes outside the API are matched without case** (`literal_route`):
  `/LOGIN`, `/Ping`, `/SIGNALR/messages/negotiate`, the feed, `/CONTENT/`.
  The covers, backups, log files and `/favicon.ico` are found by the
  originals' mappers with their own case rules, and are left to them.
- **Content negotiation.** A JSON result whose `Accept` takes no JSON is an
  empty 406 once the action has run (`ReturnHttpNotAcceptable`): a write is
  done all the same, as in the originals. `*/*` anywhere in `Accept` makes
  it irrelevant (`RespectBrowserAcceptHeader` is off). This joins the
  string actions of Radarr (section 16), which negotiate on their own.
- **Brotli.** `AddResponseCompression()` prefers Brotli when the client
  weighs it as gzip; both at level 1. `Vary: Accept-Encoding` goes only to a
  client that sent `Accept-Encoding`, and nothing is compressed for a
  request a trusted proxy says came over HTTPS (`EnableForHttps` is off),
  so behind a TLS-terminating proxy the answers leave uncompressed, as the
  originals' do. The encoder costs the release binary 1,126,528 bytes,
  522,738 under `gzip -9` (two release builds of this branch, with and
  without it), on a binary already at the image target (section 9).
  Dropping it is one feature of tower-http (`compression-br`) and two
  assertions of a test.
- **503 while starting.** The daemon binds the listener and serves before
  the command runtime starts, and `StartingUpMiddleware`'s answer stands
  for every authenticated request until it has: a proxy shows a 503 rather
  than a refused connection. The database is still opened, and an import
  done, before the listener.
- **The URL base is read once where the originals read it once**: the
  routes, the generated pages, the manifest and the tile description, the
  session cookie's path (`AppState::url_base`). `/initialize.json` and the
  login's redirects read the settings on each request, as the originals'
  controllers do. A base saved from the UI is applied by the restart, as in
  the originals; allowed hosts and trusted networks still apply at once
  (`docs/audit/7-triage.md`, row 1.35).
- **Smaller ones.** The request log under `Http` and `Api` (`unarrd` lets
  them through at trace and debug); the login form signs nobody in when the
  method is None; an XHR is told 401 with the login page's address; the
  session cookie's name sheds the instance name's diacritics before
  anything that is not an ASCII letter or digit; SignalR's negotiation
  version 1 with a connection token, and the `version` message of a new
  connection to every client; the backup download as a static file
  (`application/x-zip-compressed`, no `Content-Disposition`); the cover
  proxy's type from the name, a name that is not an image's a 404 before
  the hash is looked at.
- **Not done: the `Server` header.** The originals send `Server: Kestrel`
  on every answer; Unarr sends none. No client is known to read it; the
  maintainer decides (`docs/audit/7-triage.md`, row 1.34).
