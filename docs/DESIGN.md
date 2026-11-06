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
application in a container on a known database, record responses, replay the
same requests against Unarr on the imported database, diff the JSON. Without
this, compatibility is a hope.

## 7. Metadata providers

The originals do not call TVDB, TMDB and MusicBrainz directly. They go through
proxies hosted by the Servarr team (Skyhook, radarr-api, api.lidarr.audio).
Those are reachable, but intended for the official clients.

Two options, to be decided before phase 1 ends:

- Use the Servarr proxies, accepting that they may be closed to third parties.
- Talk to TMDB (free key), TVDB v4 (key, licence to check) and MusicBrainz
  (1 request per second) directly. More honest, more work, hardest for Lidarr.

## 8. Phases

1. **Sonarr, read-only.** Import `sonarr.db` and `config.xml`; read endpoints;
   `system/status`; minimal SignalR. Exit criterion: the official Sonarr UI and
   Bazarr work against Unarr.
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

## 11. Open questions

- Metadata provider strategy (section 7).
- Whether to serve the official frontends from the binary (embedded, GPL
  obligations trivially met) or require the user to point Unarr at a checkout.
- Sonarr v3 compatibility (`languageprofile`) versus v4 only. Bazarr supports
  both; starting with v4 only is simpler.
- Authentication: the originals support none, basic and forms. Forms auth is
  what the UI expects; API key auth is what every client uses.
