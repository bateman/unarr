# Phase 4: Radarr

Work order for the second personality. Read `docs/DESIGN.md` first (sections
4 to 9 for the architecture and targets, 11 to 14 for the decisions each
Sonarr phase took, 15 for what is open), then `docs/audit/README.md` (what
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
- Tooling: `scripts/sonarr-fixture.sh`, `scripts/sonarr-anonymize.py`,
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
- **One HTTP client per personality** (`docs/DESIGN.md` section 15): each
  personality's proxy and certificate settings apply to its own requests.
  This is a precondition for running two, not a nicety.
- **Metadata: `api.radarr.video`** behind a movie-shaped trait next to the
  series one (`docs/DESIGN.md` section 7, option A). During development the
  user agent towards it is Radarr's, one constant per personality read in one
  place, under the same written exit condition as Sonarr's. Tests never
  contact it: recorded answers under `tests/fixtures/metadata/api.radarr.video/`
  (and the TMDB image host for covers) serve both the oracle and Unarr.

## Scope, in three milestones

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

### 4c. Library (the Sonarr phases 3a and 3b ground)

1. **Import and rename**: completed download handling with Radarr's
   `MovieImport/Specifications`, disk scan (`RescanMovie`,
   `DownloadedMoviesScan`), media info, manual import, `rename?movieId=` and
   `RenameFiles`/`RenameMovie`, naming with the movie tokens (`{Movie
   Title}`, `{Release Year}`, `{Edition Tags}`, `{ImdbId}`, `{TmdbId}`, the
   movie folder format) checked against the oracle, extra files, recycling
   bin, `moviefile` deletes and editor.
2. **Notifications**: Discord, webhook and Plex with the movie payloads and
   the movie-section refresh; the others keep their settings as in Sonarr.
3. **Metadata and movie management**: `api.radarr.video` client, `movie/lookup`
   (term, `tmdb:`, `imdb:`), `POST movie` with `addOptions`, `PUT`, `DELETE`,
   `movie/editor`, `movie/import`, `RefreshMovie` with covers, minimum
   availability, collections (`RefreshCollections`, `PUT collection`),
   import exclusions.
4. **Settings writes** for everything 4a reads, with validation in the
   originals' shape; Recyclarr's Radarr sync replayed against an exchange
   recorded with the real Recyclarr and the real Radarr, as for Sonarr.
5. **Import lists**: only the implementations the maintainer uses, asked at
   the start; the rest keep their settings.
6. **Backups**, log files, and the audit method of `docs/audit/` applied to
   Radarr once (settings applied, HTTP behaviour outside the bodies, routes
   declared, UI calls, events and commands), fixing the high and medium
   findings before closing.
7. **Measurements**: both personalities running, idle and after an RSS sync,
   an import and a full refresh, on glibc and on the published musl image
   (still unmeasured, `docs/DESIGN.md` section 15); the figures against
   section 9's target for three personalities.

Exit: a mock client's completed movie download ends as a named movie file
with history, SignalR and notifications matching the oracle; the UI adds a
movie and saves every settings page; Recyclarr syncs; the audit's high and
medium findings are closed; and, on the homelab, Unarr replaces Radarr next
to Sonarr for one movie from search to Plex.

## Out of scope

Lidarr; Overseerr (the maintainer does not use it); Radarr's list sync
levels beyond what the chosen import lists need; the Kodi, Plex and
Emby metadata file writers; notification implementations beyond Discord,
webhook and Plex; download clients beyond the four; HTTPS serving; the
switch of the Servarr-proxy user agents to Unarr's own, which is its own
decision (`docs/DESIGN.md` section 15) and not part of this phase.

## Sizing

`docs/DESIGN.md` section 8 expected Radarr at half of phase 2. The map above
says otherwise: the shared pieces are real, but everything that carries the
originals' rules is bound to series and episodes, and a movie twin of each
has to be written. A movie is simpler than a series (no seasons, packs,
scene or absolute numbering, multi-episode files), so each twin is perhaps
half to two thirds of its TV counterpart. Expect 25 to 30 thousand lines and
about 500 new goldens: the size of phase 2, in three milestones of roughly
a quarter, a third and the rest.

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
- The homelab run is part of 4c's exit; `docs/homelab-compose.md` gains the
  Radarr service block and what changes when one container serves both.

## Conventions

`make ui && make check` green before every push (both UIs once 4a adds the
second); `unarr` in identifiers and Unarr in prose; commits with an
imperative subject and a body that says why, no model identifiers; one
branch per milestone (`phase-4a-radarr-readonly`, `phase-4b-radarr-acquisition`,
`phase-4c-radarr-library`), squashed into `main`; before integrating, check
that the branch history holds no homelab data that is not anonymised;
`docs/DESIGN.md` kept current (a decisions section per milestone, the open
questions last), the status table appended here.
