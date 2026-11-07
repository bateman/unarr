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
disk. The decision moves to phase 2, which is the first to add series.

Two options:

- Use the Servarr proxies, accepting that they may be closed to third parties.
- Talk to TMDB (free key), TVDB v4 (key, licence to check) and MusicBrainz
  (1 request per second) directly. More honest, more work, hardest for Lidarr.

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
   Exit criterion: Prowlarr-driven searches end in a download.
3. **Import and rename.** Folder scanning, media info (ffprobe or a native
   Matroska/MP4 reader), naming tokens, notifications.
4. **Radarr** as a second personality on the existing core.
5. **Lidarr** last: MusicBrainz, per-track matching, Chromaprint fingerprints.

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

The real fixture is still a small library next to the 1,000-series target, so
the RSS figures remain a floor; they do say the per-series cost is a few
hundred kilobytes. The first measurement on the real fixture found the
episode-file endpoints at 2 s and the full history at 49 s: every file was
matched against 249 custom-format specifications whose regexes were compiled
on each call. A per-pattern cache brought them to the figures above; no other
cache exists yet, every answer is built from SQLite on request.

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
    first fixture commit on the branch still holds the installation's LAN
    host names in `config.xml`; the branch history has to be rewritten before
    the repository goes public.
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

## 12. Open questions

- Metadata provider strategy (section 7), needed when phase 2 adds series.
- SignalR beyond keep-alive: phase 1 broadcasts nothing because nothing
  changes. Phase 2 has to emit `version`, `series`, `episode`, `queue` and
  `command` messages with the shapes the UI reduces.
- The collation approximates ICU for ASCII names. Titles with accented or
  non-Latin characters may sort differently from the originals; a full UCA
  implementation is not worth its size until a golden shows the difference.
- `MediaCover` images: the fixture has none, so the UI shows placeholders.
  Phase 2 will fetch covers when it adds series; for imports, Unarr reuses the
  original `MediaCover` directory when it is next to the imported files.
