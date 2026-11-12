# Phase 4: Radarr

Work order for the second personality. Read `docs/DESIGN.md` first (sections
4 to 9 for the architecture and targets, 11 to 14 for the decisions each
Sonarr phase took, 15 and 16 for those of milestones 4a and 4b, 17 for what is open), then `docs/audit/README.md` (what
goldens cannot see, and how it was found), then this document. The four
Sonarr work orders (`docs/phase-1-sonarr.md` to `docs/phase-3b-library.md`)
are the template: Radarr goes through the same ground in the same order,
with the method already proven.

## Goal

`unarrd --personalities sonarr,radarr` replaces both applications in one
process. A Radarr installation imported into Unarr serves the official Radarr
web UI, Prowlarr, Bazarr and Recyclarr unchanged, searches, grabs, imports,
renames and notifies for movies as the original does, and the two
personalities together stay inside the memory target of `docs/DESIGN.md`
section 9.

## Where the Sonarr phases left things

From `main` at `86d12d4` (11 November 2026): 80k lines of Rust, 353 tests,
827 golden requests, CI green, the Sonarr personality complete and audited,
running on the maintainer's NAS in place of Sonarr
(`docs/homelab-compose.md`).

**Already parameterised by personality:** ports, API prefix, application
name, database and log file names, data directory, backup names, the
emulated version and migration (`unarr-api/src/compat.rs`, placeholders for
Radarr), `unarr_import::config_xml`, the container health check, one runtime,
scheduler and log dispatcher per personality.

**What `--personalities radarr` does today:** it starts, opens
`unarr-radarr.db` with the Sonarr-shaped migrations, imports nothing, gets no
defaults, and serves the Sonarr router, schemas and UI under Radarr's name
and port. A Sonarr clone with another label; nothing of it is to be kept as
behaviour.

**Hard-wired to Sonarr, to be made a choice by personality:**

- `unarrd/src/main.rs` `open_database`: the importer (`unarr_import::sonarr`
  or a warning) and `unarr_core::defaults::ensure` (Sonarr only).
- `unarr-api`: `routes::api_router` and `ui_router` take the state and ignore
  it (one flat list of about 160 Sonarr routes); the feed path
  `/feed/v3/calendar/Sonarr.ics`; `ui.rs` embeds `ui/sonarr/` only;
  `schemas.rs` and `routes/system.rs` `include_str!` from `schemas/sonarr/`
  into one static catalogue.
- `unarr-core`: `quality::SONARR_QUALITIES` (no CAM, TELESYNC, TELECINE,
  DVDSCR, REGIONAL, WORKPRINT, BR-DISK, Remux tiers as Radarr orders them);
  `downloadclients::APP_NAME = "Sonarr"` and the `tv_category` setting
  (Radarr's is `movieCategory`, default `radarr`); health wiki anchors;
  `http.rs`: one process-wide client and one
  `SERVARR_PROXY_USER_AGENT` constant (Sonarr's).
- Tooling: `scripts/sonarr-fixture.sh`, `scripts/arr-anonymize.py`,
  `scripts/mock-servarr.py` and `unarr-mock` (indexers answer `t=tvsearch`
  and `t=search` only), the Makefile targets, the CI cache and `make ui`.
- Tests: every harness points at `tests/fixtures/sonarr` and builds
  `AppState::new(Personality::Sonarr, ...)`.

**Bound to series and episodes throughout** (the bulk of the work):
the migrations (every history, queue, file, blocklist, pending and extra-file
table carries series and episode keys), `model.rs`, `store/{series,episodes,
stats,scene}.rs`, `parser/title.rs` (returns `ParsedEpisodeInfo`; there is no
movie title parsing), `decision/` (subjects are `RemoteEpisode`; mapping,
lookup and eight of the specifications are episode notions), `search.rs`,
`indexers/request.rs`, `downloads/`, `mediafiles/` (specifications, approve,
aggregate, manual, extras, scan, rename, events), `naming.rs` tokens,
`notifications/` messages and payloads, `commands/` names and executor arms,
`tv/`, `metadata/` (the trait returns `SeriesInfo`), `importlists/`, and in
`unarr-api` `resources.rs`, `relay.rs`, the release cache in `state.rs` and
most of `routes/`.

**Reusable as they are:** `parser/{quality,language,subtitles,netregex,
media_extensions}.rs`, `customformat.rs` (plus Radarr's Edition
specification, minus Sonarr's ReleaseType), `profile.rs`, the provider
stores and status, `indexers/{client,rss,service,settings,status,caps}.rs`,
`downloadclients/` (four clients, captures), `mediafiles/{mediainfo,sample,
transfer}.rs`, the notification transports (Discord, webhook, Plex),
`health.rs`, `backup.rs`, `housekeeping.rs`, `validation.rs`, `outbound.rs`,
`config.rs`, `collation.rs`, the command queue, executor and scheduler
machinery, auth, hosts, forwarded headers, SignalR hub, log files, the
contract-test harness and `scripts/record-golden.py`.

## How to add the second personality

Decided here so the phase does not start with a rewrite:

- **Parallel modules, shared generics, no grand abstraction.** The originals
  are forks: Radarr is Sonarr with `Movie` where `Series`/`Episode` were, and
  eight years of drift. Mirror that: a `movies/` family next to the
  series-bound modules (model, store, title parser, mapping and
  specifications, search, tracked downloads, import, naming tokens,
  notification messages, commands, metadata), calling the shared pieces
  listed above. A generic `MediaItem` trait over series and movies is not
  attempted: the two differ exactly where the originals' rules live
  (seasons, packs, scene numbering, absolute numbers on one side; editions,
  minimum availability, collections, one file per movie on the other), and
  a trait would have to carry both.
- **Extract only on second use.** When a movie module needs something that
  sits inside a series-bound file, move it to a neutral place then, in its
  own commit, with the Sonarr goldens green before and after. No preparatory
  refactor of code Radarr has not yet touched.
- **The 827 Sonarr goldens are the safety net.** They pass at every commit of
  this phase. A refactor that needs a Sonarr golden re-recorded is wrong.
- **Migrations per personality.** `unarr-radarr.db` gets its own migration
  set (shared tables written once, included by both sets). Existing
  `unarr-sonarr.db` files keep their `user_version` sequence untouched; a
  `unarr-radarr.db` created by today's clone behaviour holds nothing worth
  keeping and may be refused or recreated, say which in the decisions.
- **Router, schemas, UI and localisation by personality.** `api_router` and
  `ui_router` finally read their state: a Radarr route list, a
  `schemas/radarr/` catalogue recorded from the original, `ui/radarr/`
  embedded next to `ui/sonarr/`, `/feed/v3/calendar/Radarr.ics`.
- **One HTTP client per personality** (`docs/DESIGN.md` section 18): each
  personality's proxy and certificate settings apply to its own requests.
  This is a precondition for running two, not a nicety.
- **Metadata: `api.radarr.video`** behind a movie-shaped trait next to the
  series one (`docs/DESIGN.md` section 7, option A). During development the
  user agent towards it is Radarr's, one constant per personality read in one
  place, under the same written exit condition as Sonarr's. Tests never
  contact it: recorded answers under `tests/fixtures/metadata/api.radarr.video/`
  (and the TMDB image host for covers) serve both the oracle and Unarr.

## Scope, in four milestones

Each milestone lands on `main` on its own, squashed, with its status table in
this document, as the Sonarr phases did. If a conversation grows long, the
next milestone starts in a new one from this document.

### 4a. Radarr, read-only (the Sonarr phase 1 ground)

1. **Oracle and fixture.** `scripts/radarr-fixture.sh` beside the Sonarr one
   (or one script taking the application): the official Radarr release pinned
   by version and SHA-256, the version the maintainer runs, started without
   Docker with its scheduled tasks frozen; `make ui-radarr`, `make
   golden-radarr`. The anonymiser extended to Radarr's schema (movies,
   movie files, paths, credits and collections left as public data,
   every credential, host and URL replaced, refusing to write if an original
   value survives). The fixture is the maintainer's Radarr backup, asked for
   at the start; until it arrives, a small one built by driving the real
   Radarr's API, to be replaced.
2. **Per-personality plumbing** as decided above: migrations, router,
   schemas, UI embed, HTTP client, importer and defaults selection, test
   harness taking the personality and its fixture directory, CI fetching and
   caching both UIs, the image embedding both.
3. **Importer** (`unarr_import::radarr`): `radarr.db` at the migration levels
   current Radarr releases produce (state the accepted range), `Movies`,
   `MovieMetadata`, `MovieFiles`, `MovieTranslations`, `AlternativeTitles`,
   `Credits`, `Collections`, `ImportExclusions`, quality profiles with their
   language, quality definitions (Radarr's table), custom formats, providers,
   delay and release profiles, remote path mappings, history, blocklist,
   pending releases, download history, provider status, naming config,
   config, users, scheduled tasks, extra and subtitle files. Every table it
   skips is logged.
4. **Read endpoints** under `/api/v3` with Radarr's shapes: `movie`,
   `movie/{id}`, `moviefile`, `collection`, `credit`, `alttitle`,
   `extrafile`, `calendar`, `wanted/missing`, `wanted/cutoff`, `history`,
   `history/movie`, `history/since`, `blocklist`, `queue*` (empty until 4b),
   `qualityprofile`, `qualitydefinition`, `customformat`, `language`,
   `indexerflag`, `tag`, `rootfolder`, every `config/*`, the providers with
   their schemas, `system/*`, `health`, `diskspace`, `log`, `update`,
   `localization`, `initialize.json`, the ICS feed. Authentication, allowed
   hosts, SignalR and the embedded Radarr UI as for Sonarr.
5. **Bazarr's movie side** verified by extending `scripts/bazarr-replay.py`
   (`movie`, `moviefile`, `history`, `rootfolder`, `tag`, `system/status`).

Exit: the Radarr UI browses movies, collections, calendar, activity, wanted,
settings and system pages against Unarr on 7878 with Sonarr running beside it
on 8989 in the same process; the read goldens replay; Bazarr's movie requests
are answered; both personalities' idle memory is measured.

### 4b. Acquisition (the Sonarr phase 2 ground)

1. **Movie title parser** (Radarr's `Parser`: title and year, edition,
   IMDb and TMDB ids in names, hardcoded subs, release group reuse) with the
   originals' vectors through `scripts/port-vectors.py` pointed at
   `Radarr/Radarr` (`src/NzbDrone.Core.Test/ParserTests`), quality with
   Radarr's sources and modifiers, languages.
2. **Provider writes** for indexers and download clients (the Sonarr
   handlers generalised over the schema catalogue), `movieCategory`, and
   Prowlarr's Radarr sync replayed (`scripts/prowlarr-replay.py` extended to
   its `RadarrV3Proxy`).
3. **Indexer requests**: `t=movie` with `imdbid` and `tmdbid`, text search
   fallbacks, categories 2000; the mocks (`scripts/mock-servarr.py` and
   `unarr-mock`) answering them from fixtures.
4. **Mapping and decision engine**: release to movie (title, year,
   alternative titles, ids), Radarr's specifications
   (`DecisionEngine/Specifications`: availability, required and ignored
   terms, upgrade and cutoff, custom format score, size against runtime,
   retention, protocol, blocklist, already imported, hardcoded subs, raw
   disk...) with the originals' vectors, rejections in the originals' words.
5. **Interactive search, grab, queue, history**: `release?movieId=`, `POST
   release`, `release/push`, the tracked downloads keyed by movie, queue
   endpoints and removals, `Grabbed` history with Radarr's `data`.
6. **Commands and scheduler**: `MoviesSearch`, `MissingMoviesSearch`,
   `CutoffUnmetMoviesSearch`, `RssSync`, `RefreshMonitoredDownloads`,
   `ProcessMonitoredDownloads` and the generic ones, Radarr's task list,
   pending releases, health.

Exit: on the fixture with the mocks, an `RssSync` and a `MoviesSearch` each
end in a grab with the queue, history, SignalR and command shapes matching
the oracle; the parser and decision vectors pass; Prowlarr's sync replays.

### 4c. Import, rename, notify (the Sonarr phase 3a ground)

The library milestone was cut in two after 4b landed, as phase 3 was: 4a
and 4b together added 21 thousand lines, and the Sonarr phases this ground
retraces added 35 thousand. One landing would have been as large as the
two before it.

1. **Completed download handling**: from the `importPending` download
   `movies::downloads` leaves with its import item resolved, through
   Radarr's `MovieImport/Specifications` (ported with the original's
   fixtures as tests), the transfer modes of `config/mediamanagement`, the
   `MovieFile` row with media info from the ffprobe the pinned release ships,
   the upgrade's delete into the recycling bin, removal from the client,
   `DownloadFolderImported` and `MovieFileDeleted` history with Radarr's
   `data`, the queue emptying.
2. **Disk scan and the library's commands**: `RescanMovie` first (Bazarr
   posts it and gets a 500 today), `DownloadedMoviesScan`, the scheduled
   rescan, the skips the original applies, extra and subtitle files;
   `RefreshMovie` accepted and completing without effect until 4d, as
   `RefreshSeries` was in phase 2; `Housekeeping`, `Backup` and the other
   listed tasks no longer answering 500.
3. **Rename**: `GET rename?movieId=`, `RenameFiles`, `RenameMovie` and the
   movie folder, on `movies::naming` (already a port of Radarr's
   `FileNameBuilder`, compared with the original on 444 formats); any
   difference the oracle shows is fixed in the port.
4. **Manual import and movie files**: `GET` and `POST manualimport`,
   `moviefile` delete, bulk and editor.
5. **Notifications**: CRUD, `test` and `testall` for Radarr's providers and
   the events (grab, download, upgrade, rename, movie file delete, health,
   manual interaction) to Discord, the webhook and Plex with the movie
   payloads and the movie-section refresh, recorded by the mock targets from
   the real Radarr. The fixture has exactly these three; the other
   implementations keep their settings, as in Sonarr.
6. **Health** of the import (root folders, remote path mappings, completed
   download handling, mounts) with Radarr's messages and links; `DELETE
   blocklist` and its bulk form.
7. **Memory**. Section 9's target does not hold since 4b: 74 MB at rest with
   both personalities once Radarr has searched, 21 MB of it the movie
   parser's compiled patterns. Before adding more, find what brings it back
   (patterns the two parsers have in common compiled once, compilation on
   first use per pattern rather than per parser, the engine's cache limits,
   a leaner matcher for the patterns that need no look-around) and either
   meet 60 MB or put the measured floor to the maintainer as the new target.
   Measure the published musl image as well: it has never been measured.
8. **A look at the screen.** The queue, the interactive search and the
   command messages of 4b were verified by goldens and hub assertions only;
   drive them, and this milestone's pages (Activity, movie page, Manual
   Import, rename preview), in a browser against the release binary.

Exit: on the fixture with the mock SABnzbd and qBittorrent completing
items, the movie files appear under the root folder named by the fixture's
naming settings, with media info, history, SignalR messages and the three
notifications equal to the oracle; `scripts/bazarr-replay.py --app radarr`
passes with `RescanMovie` answered; the pages render; the memory point is
settled one way or the other; the Sonarr goldens are untouched.

### 4d. Metadata, movie management, settings writes (the Sonarr phase 3b ground)

1. **Metadata and movie management**: `api.radarr.video` client behind the
   movie-shaped trait (recorded answers under
   `tests/fixtures/metadata/api.radarr.video/`, Radarr's user agent in one
   constant, per `docs/DESIGN.md` section 7), `movie/lookup` (term, `tmdb:`,
   `imdb:`), `POST movie` with `addOptions`, `PUT` with the folder move,
   `DELETE`, `movie/editor`, `movie/import`, `RefreshMovie` made real with
   covers, minimum availability, collections (`RefreshCollections`, `PUT
   collection`), import exclusions.
2. **Settings writes** for everything 4a reads (every `config/*`, tags, root
   folders, quality profiles and definitions, custom formats, delay and
   release profiles, remote path mappings, custom filters, auto tagging),
   with validation in the originals' shape, on the Sonarr handlers where
   the resource is the same.
3. **Recyclarr**: the exchange between the real Recyclarr and the real
   Radarr recorded from the maintainer's configuration
   (`tests/fixtures/recyclarr/radarr.yml`, already in the tree) and replayed
   against Unarr; `delete_old_custom_formats` must leave alone the formats
   Recyclarr did not create (`docs/DESIGN.md` section 18).
4. **Import list**: the IMDb watchlist (`IMDbListImport`), the one the
   maintainer uses, against a mock; the rest keep their settings.
5. **Backups** and log files for Radarr; the audit method of `docs/audit/`
   applied to Radarr (settings applied, routes declared, UI calls, events
   and commands; the HTTP audit already runs against both, `make
   http-audit`), fixing the high and medium findings before closing.
6. **Measurements**: both personalities, idle and after an RSS sync, an
   import and a full refresh, a library scaled to 2,000 movies beside the
   950-series one, on glibc and on the image. A target missed is added to
   the table of `docs/DESIGN.md` section 9 and left there: correctness
   first, by the maintainer's decision at the end of 4c.
7. **The review's open suggestion** (`docs/DESIGN.md` section 18): share
   between the two pipelines the pieces that carry no media notion (the
   refresh loop over the clients, what a grab's failure means, the release
   comparisons, the push validation). Take it up here if the two sides are
   by then visibly the same code, in commits of their own with both replays
   green before and after; otherwise say why not.

Exit: the UI adds a movie and saves every settings page; the watchlist sync
adds movies against the mock; Recyclarr's exchange replays; the audit's
high and medium findings are closed; and, on the homelab, Unarr replaces
Radarr next to Sonarr in one container for one movie from search to Plex
(`docs/homelab-compose.md` gains the Radarr service block).

## Out of scope

Lidarr; Overseerr (the maintainer does not use it); Radarr's list sync
levels beyond what the chosen import lists need; the Kodi, Plex and
Emby metadata file writers; notification implementations beyond Discord,
webhook and Plex; download clients beyond the four; HTTPS serving; the
switch of the Servarr-proxy user agents to Unarr's own, which is its own
decision (`docs/DESIGN.md` section 18) and not part of this phase.

## Sizing

`docs/DESIGN.md` section 8 expected Radarr at half of phase 2. The map above
says otherwise: the shared pieces are real, but everything that carries the
originals' rules is bound to series and episodes, and a movie twin of each
has to be written. A movie is simpler than a series (no seasons, packs,
scene or absolute numbering, multi-episode files), so each twin is perhaps
half to two thirds of its TV counterpart. Expect 25 to 30 thousand lines and
about 500 new goldens: the size of phase 2, in three milestones of roughly
a quarter, a third and the rest.

Revised after 4b: 4a and 4b came to 21 thousand lines and 312 goldens
between them. The Sonarr ground still to retrace (phases 3a and 3b) was 35
thousand lines; at half to two thirds, 4c and 4d are 9 to 11 thousand lines
each, so the phase ends near 40 thousand, a third above phase 2.

## Verification strategy

As every Sonarr phase: record from the real Radarr before implementing a
shape; port the originals' tests as data; mocks, never the maintainer's
credentials; no Docker in the cloud session. Added by experience:

- Record each milestone's goldens on the real fixture as soon as it exists;
  the synthetic one hid seven behaviours in phase 1.
- Things outside the JSON bodies (headers, status codes on odd paths,
  settings that must take effect, routes the UI calls rarely) are checked by
  the audit method, not by goldens. Do it once per milestone for what the
  milestone added, rather than once at the end.
- The homelab run is part of 4d's exit; `docs/homelab-compose.md` gains the
  Radarr service block and what changes when one container serves both.

## Conventions

`make ui && make check` green before every push (both UIs since 4a);
`unarr` in identifiers and Unarr in prose; commits with an imperative
subject and a body that says why, no model identifiers; one branch per
milestone, named as `CLAUDE.md` asks (`radarr/phase-4a-readonly`,
`radarr/phase-4b-acquisition`, `radarr/phase-4c-import-rename-notify`,
`radarr/phase-4d-library`), squashed into
`main`; before integrating, check
that the branch history holds no homelab data that is not anonymised;
`docs/DESIGN.md` kept current (a decisions section per milestone, the open
questions last), the status table appended here.

## Status of 4a, November 2026

Recorded and replayed on the maintainer's Radarr 6.4.4.10685 installation
(migration 242), anonymised: 193 movies, 100 files, 32 collections, 669
history rows. The fixture was real from the first commit; no synthetic one
was built.

| Exit criterion | Status |
|----------------|--------|
| `make ui && make check` green | Yes: fmt, clippy `-D warnings`, 370 tests, the Sonarr replay unchanged (634 goldens over the 659 requests of its list; the "827" above counts the list's lines). |
| The Radarr UI browses movies, collections, calendar, activity, wanted, settings and system pages on 7878, Sonarr beside it on 8989 in one process | Yes, with Playwright against the release binary running `--personalities sonarr,radarr` on both fixtures: login, the movie index, movie pages, add and import pages, collections, calendar, queue, history, blocklist, wanted missing and cutoff, every settings page, system status, tasks, backup, updates, events and log files; 32 pages, no page error. The requests that failed: the covers (the fixture has no `MediaCover`), `importlist/movie` on Discover and `config/naming/examples` on Media Management, both of 4c. |
| The read goldens replay | 145 of 146, recorded from the real Radarr before each shape was written. Pending: `config-naming-examples`, which needs the movie file name builder of 4c. |
| Bazarr's movie requests are answered | Verified by `scripts/bazarr-replay.py --app radarr` (`make bazarr-radarr`) on 193 movies and 100 files, with the Sonarr side replayed against the same process. The requests are Bazarr's as remembered from its source; Bazarr itself was not run. |
| Both personalities' idle memory is measured | Yes: 39 to 45 MB idle with both, 48 MB at rest after some 1,100 requests; `docs/DESIGN.md` section 9. Neither library was scaled. |

What 4a delivered against its five points:

1. **Oracle and fixture.** `scripts/radarr-fixture.sh` (release pinned by
   version and SHA-256, no Docker, scheduled tasks frozen), `make
   ui-radarr`, `make golden-radarr`, `make radarr`;
   `scripts/arr-anonymize.py --app radarr`; `tests/fixtures/radarr/`.
2. **Per-personality plumbing.** Migration sets, router and feed, schema
   catalogue, strings, embedded UI, PUT resources, HTTP clients and
   outgoing settings, importer selection, a Radarr replay
   (`tests/contract_radarr.rs`) sharing its helpers with Sonarr's, CI
   fetching and caching both UIs. The image embeds both because `make ui`
   fills both folders before the build; no image was built in the session.
3. **Importer.** `unarr_import::radarr`, migrations 208 to 242 accepted,
   242 verified; every table of the list read, `ImportListMovies` and
   `MetadataFiles` included; nothing skipped on the fixture.
4. **Read endpoints.** All of the list. Deviations, all deliberate: the
   queue endpoints, `command` and `health` answer empty and `system/task`
   shows the imported rows, since Radarr runs no command runtime until
   4b; `log`, `update`, `diskspace` match the shape, not the values;
   every write answers 405.
5. **Bazarr's movie side.** As above.

What 4b starts from: Radarr has no command queue, scheduler or health
checks; no title parser (the history's custom formats take the release
group from the shared parser and no edition); provider resources are
read through the schema catalogue and not yet through typed settings
(`movieCategory` is in the schemas, not in `downloadclients::settings`);
the personality's HTTP clients are in `AppState::http()` for the indexer
and download client services to be built on. `docs/DESIGN.md` section 15
has the decisions, section 18 what is open.

## Status of 4b, November 2026

Recorded and replayed on the same fixture, with the mock indexers and
download clients of `scripts/mock-servarr.py` (the recording) and of
`unarr-mock` (the replay). No Docker, no credential: the original ran
without the environment's proxy and its own providers point at hosts that
do not resolve.

| Exit criterion | Status |
|----------------|--------|
| `make ui && make check` green | Yes: fmt, clippy `-D warnings`, 438 tests in 31 test binaries. Sonarr's replay is unchanged: the same 634 goldens, none re-recorded. |
| An `RssSync` ends in a grab with queue, history, commands equal to the oracle | Yes. The original and Unarr both find 14 reports and grab one (Last Night in Soho, to the mock qBittorrent); `command-rsssync-get`, `queue-after-command` and `history-after-command` replay. |
| A `MoviesSearch` ends in a grab, likewise | Yes: The Holdovers, to the mock SABnzbd, with `releaseSource` `UserInvokedSearch`; a second search, for a movie already in the queue, grabs nothing, as in the original. |
| SignalR equal to the oracle | Not recorded from the original, as for Sonarr. The replay asserts what the hub sends: each command's start and end with its last message, the queue's sync, a queue status that counts the downloads and the pending releases, the health sync, and nothing under a series name. That assertion found the status being sent before the refresh and without the pending releases, since fixed. |
| Parser vectors pass | 1,076 of Radarr's parser cases, ported as data by `scripts/port-vectors.py --source`. |
| Decision vectors pass | 39 cases over the 30 specifications; their order is settled by a golden (`release-rss-pending`), not by the vectors. |
| Prowlarr's sync replays | `scripts/prowlarr-replay.py --app radarr` (`make prowlarr-radarr`): 45 checks, passed against the real Radarr and against Unarr. The requests are Prowlarr 2.7's as read from its source and from the maintainer's logs; Prowlarr itself was not run. |
| The goldens replay | All 312 requests, recorded from the real Radarr before each shape was written (`config-naming-examples` replays since the movie file name builder landed on `main`). |

What 4b delivered against its six points:

1. **Title parser.** `movies::parser` (title and year, edition, ids in
   names, hardcoded subs, release group, quality, languages), with the
   original's quirks kept.
2. **Provider writes and Prowlarr.** Create, update, delete, test, test
   all, actions and bulk for indexers and download clients, on the Sonarr
   handlers with the application's schema, category and priority keys;
   `movieCategory` through typed settings.
3. **Indexer requests.** `t=movie` with `imdbid` and `tmdbid` as each
   indexer's capabilities allow, the text search as second tier, the
   movie categories; both mocks serve them, and the replay compares the
   set of requests Unarr made with the set the original made.
4. **Mapping and decisions.** Release to movie by ids, title, year,
   alternative titles and translations; the 30 specifications with the
   original's messages.
5. **Search, grab, queue, history.** `GET release`, `POST release`,
   `release/push`; grabs to SABnzbd, qBittorrent and Transmission (NZBGet
   is tested, not grabbed to); tracked downloads by movie; `queue`,
   `queue/status`, `queue/details` with the filters, sort keys and paging,
   the two queries Unpackerr and Cleanuparr make; removal with and
   without blocklist; `Grabbed` and `DownloadFailed` history with
   Radarr's `data`.
6. **Commands, scheduler, pending releases, health.** `RssSync`,
   `MoviesSearch`, `MissingMoviesSearch`, `CutoffUnmetMoviesSearch`,
   `RefreshMonitoredDownloads`, `ProcessMonitoredDownloads`,
   `MessagingCleanup`, `CheckHealth`; Radarr's eleven tasks; pending
   releases in the queue, grabbed or removed from there; the health
   checks of the acquisition with Radarr's wiki links.

Also done, from what 4a left: the calendar's order among movies of one
date was probed on the original and corrected (three dates, not the TMDB
id); status and headers were compared on the running original
(`make http-audit`, 225 requests).

What does not hold, or was not verified:

- **The memory target.** 74 MB at rest with both personalities once
  Radarr has searched, 63 MB with Radarr alone, against 60 MB; the movie
  parser's patterns are 21 MB of it (`docs/DESIGN.md` section 9).
- **Not replayed against the original**: the two bulk searches (tried on
  the mock) and a release pending for want of a client. A failed
  download, the queue's bulk removal and bulk grab and a grab with
  `shouldOverride` were recorded after the review of the branch and
  replay.
- **Deliberately absent until their milestones**: importing a completed
  download (it waits as `importPending`), the commands of the library
  (`RefreshMovie`, `RescanMovie`, which Bazarr posts, `Housekeeping`,
  `Backup`...: listed as tasks, not run, 500 when posted), Radarr's
  notifications, its own health checks, `DELETE blocklist`, the writes of
  settings other than the providers and `PUT delayprofile/{id}`.
- **The server both personalities share** still differs from the
  originals outside the bodies (cache headers, charset, compression,
  CORS, `HEAD`, bearer key, trailing slash): measured, not changed.
- The UI was not driven in a browser in this milestone: the pages of 4a
  were, and the queue, the interactive search and the command messages
  they now show come from goldens and from the hub's assertions, not
  from a look at the screen.

What 4c starts from: `movies::downloads` leaves a completed download
`importPending` with its import item resolved; the movie file name
builder is written (`movies::naming`, a port of Radarr's
`FileNameBuilder`, landed after 4b for `config/naming/examples`, the
last pending golden; compared with the original on 444 formats of the
settings form) and nothing renames with it yet; the command runtime runs Radarr's commands through
`movies::commands`, where the library's commands go; the scheduled tasks
of the library are listed and skipped by `CommandBody::runs_in`.
`docs/DESIGN.md` section 16 has the decisions, section 18 what is open.

## Status of 4c, November 2026

Recorded and replayed on the same fixture. The recording's mocks now
complete downloads with real clips under a movie library of their own
(`scripts/mock-servarr.py --movies`), and keep what the mock Discord, the
webhook and the mock Plex receive; the replay's mocks (`unarr-mock`) do the
same. The whole list is one recording of the original, taken in one run:
two recordings nine hours apart could not be mixed, because the answers
carry the clock.

| Exit criterion | Status |
|----------------|--------|
| `make ui && make check` green | Yes: fmt, clippy `-D warnings`, 628 tests in 31 test binaries. Sonarr's replay is unchanged: the same 634 goldens, none re-recorded. |
| Completed items end as movie files under the root folder, named by the fixture's naming settings, with media info | Yes. The mock qBittorrent and SABnzbd complete three items; two import (hardlink or copy, the maintainer's mode) and one waits for manual interaction, as in the original. The file names, `movieFile` rows and media info (from the ffprobe the pinned Radarr release ships, also used by the replay) are goldens. |
| History, SignalR and the three notifications equal to the oracle | History: yes, goldens. Notifications: yes, the bodies the mock Discord and webhook received and the requests the mock Plex received are goldens (10 snapshots), with Plex seeing the library under another path as on the maintainer's installation. SignalR: not recorded from the original, as before; the replay asserts what the hub sends. |
| `scripts/bazarr-replay.py --app radarr` passes with `RescanMovie` answered | Yes (`make bazarr-radarr` against the release binary on both fixtures): 193 movies and 100 files checked, `RescanMovie` answered 201 and completed; the Sonarr side replayed against the same process. Bazarr itself was not run. |
| The pages render | Yes, with Playwright against the release binary on both fixtures after 388 requests of the list: login, queue (three rows, one blocked), the queue's manual import, history, blocklist, wanted, a movie page with its imported file, the movie's history, the rename preview, the interactive search (nine releases), Manage Files, tasks, status, Settings > Connect and the Plex form, events, the movie index. Nineteen steps, no page error; the requests that failed are the covers the fixture does not have. |
| The memory point is settled | Yes, by decision: brought down at idle, not at work; the maintainer keeps the target and defers the miss until Unarr is a daily driver (below, and `docs/DESIGN.md` section 9). |
| The Sonarr goldens are untouched | Yes. |
| The goldens replay | All 463 (453 answers and 10 mock snapshots), none pending, recorded from the real Radarr before each shape was written. |

What 4c delivered against its eight points:

1. **Completed download handling.** `movies::import`: the ten
   specifications of Radarr's `MovieImport/Specifications` in the
   original's order with its fixtures as tests, the aggregators, sample
   detection, the three transfer modes and the recycling bin, the upgrade,
   `DownloadFolderImported` and `MovieFileDeleted` history, removal from
   the client, `ManualInteractionRequired`.
2. **Disk scan and the library's commands.** `RescanMovie`,
   `DownloadedMoviesScan`, the scheduled rescan with the original's skips
   and exclusions, extra and subtitle files (found, imported, renamed and
   deleted with their movie file), `Housekeeping` (seventeen housekeepers),
   `Backup`, `CleanUpRecycleBin`; `RefreshMovie` is accepted and completes
   without effect.
3. **Rename.** `GET rename`, `RenameFiles`, `RenameMovie` with the folder
   move, on `movies::naming`.
4. **Manual import and movie files.** `GET` and `POST manualimport`, the
   `ManualImport` command, `moviefile` `PUT`, editor, bulk and `DELETE`,
   `filesystem`, `parse`.
5. **Notifications.** CRUD, `test`, `testall` and the actions; grab,
   download, upgrade, rename, movie file delete, health, health restored
   and manual interaction to Discord, the webhook and Plex, the latter with
   its path mapping and the movie-section refresh.
6. **Health and blocklist.** The import's checks (root folders, the
   collections' root folders, remote path mappings, completed download
   handling, mounts) with Radarr's messages and links; `DELETE blocklist`
   and `blocklist/bulk`.
7. **Memory.** The compiled patterns of both parsers and of the custom
   formats are now released after five minutes without use and compiled
   again on demand (under a second for all of them). Release build, both
   personalities on the two fixtures:

   | | glibc | static musl (the image's build) |
   |---|-------|------|
   | Idle after start | 37.6 MB | 30.1 MB |
   | After a search, an RSS sync and the imports | 122.9 MB | 97.9 MB |
   | At rest, 35 s later | 104.4 MB | 97.8 MB |
   | After 330 s idle | 45.4 MB | 39.5 MB |
   | Working again | 125.1 MB | 98.2 MB |

   So the 60 MB target holds for a process that has been idle for five
   minutes and does not hold while it works or just after: the patterns of
   the two parsers together are 54 MB compiled (series 42, movies 14), and
   what made them smaller cost speed or changed what they match
   (`docs/DESIGN.md` section 9). The
   maintainer's decision: make it run, then make it fast. The target
   stays, the working figure (125 MB on glibc, 100 MB on musl, on two
   small libraries) is recorded as a miss with the others and taken up
   once Unarr is a daily driver. The musl
   build was measured for the first time; the image itself was not built.
8. **A look at the screen.** As in the table; 4b's queue, interactive
   search and command messages included.

What does not hold, or was not verified:

- **Radarr's file date setting** (the file's date set to the cinema or
  release date) is stored and not applied.
- **`{MediaInfo 3D}`** reads the stream's view count only; the fixture has
  no 3D file.
- **Notifications not wired**: movie added, movies deleted, application
  update. The metadata they would carry belongs to 4d. Plex is refreshed
  directly; plex.tv is not pinged first, as on the Sonarr side.
- **The metadata consumers' place** in the order of the extras is partly
  inferred from the source: none is enabled on the fixture.
- **`RefreshMovie` was left out of the recording**: on the original it
  reached the metadata server, so its answers changed between recordings.
  It is 4d's.
- **Working memory** is where 4b left it (above); neither library was
  scaled.
- **Not run**: Bazarr itself; the image; the homelab, by the maintainer's
  choice left to the end of 4d. The maintainer's Radarr logs were read for
  the facts they hold (hardlink or copy from qBittorrent, a move from the
  second torrent client, no recycling bin, Plex under another path, the
  ping to plex.tv) and confirm the import's sequence; none of their
  content is in the tree.

What 4d starts from: `RefreshMovie` is accepted and does nothing, and
everything that depends on fresh metadata (the notifications above,
minimum availability moving with time, covers) waits for it; movies are
never added, edited, moved or deleted through the API (`PUT moviefile` and
the monitored flag an import sets are the only movie-side writes); the
settings writes are those of the providers, the notifications and
`PUT delayprofile/{id}`; `Backup` runs for Radarr, the restore and the log
files do not; the import list is listed and skipped. The audit method has
not been applied to Radarr beyond `make http-audit`. `docs/DESIGN.md`
section 17 has the decisions, section 18 what is open.
