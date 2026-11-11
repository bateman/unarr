# Phase 3b: metadata, series management, settings writes

Work order for the second half of phase 3. It depends on a decision that has
not been taken yet, the metadata provider (below); everything else in the
scope can start before it and most of it does not touch it. Read
`docs/DESIGN.md` first (section 7 holds the metadata trade-offs so far), then
`docs/phase-3a-import.md`.

## Goal

Unarr manages the library, not just the one it imported: series can be looked
up, added, refreshed, edited and deleted from the UI and from the maintainer's
Simkl import list; Recyclarr syncs quality definitions, profiles and custom
formats; every settings page saves; and the posters come back.

**Scope decided with the maintainer at the start of the phase.** Overseerr and
Jellyseerr are not used and will not be: their replay leaves the scope and
the exit criterion that named them is restated on the UI's Add Series flow
and the Simkl import list. The only import list in use is Simkl (the
maintainer moved to it from Trakt): Simkl is the one implementation driven,
the others round-trip. Skyhook may be contacted whenever development needs
it, with Sonarr's user agent, while Unarr stays a private project tested on
the maintainer's homelab.

## The metadata provider decision

**Taken, November 2026: option A, Skyhook**, with the original's user agent
during development and testing and a written exit condition for switching
to Unarr's own; the full statement is in `docs/DESIGN.md` section 7 and
binds this phase. The alternatives are kept below for the record and for
the fallback (B with TVMaze and XEM) should Skyhook ever refuse. What was
known in November 2026 (the terms of third-party services move; check them
again):

**A. The Servarr proxy (Skyhook, `skyhook.sonarr.tv`).**
- For: the exact JSON the originals consume, so lookup, add and refresh are
  ports and the goldens come from the oracle unchanged; one call returns the
  series, seasons, episodes, images, ratings, alternate titles, TVMaze, TMDB
  and IMDb ids and the XEM scene mappings the parser needs; no key for the
  user; the fixture's data came from it, so the model already fits; one
  approach for Radarr (`api.radarr.video`) and Lidarr (`api.lidarr.audio`)
  later.
- Against: a service the Servarr team runs for their own clients, with no
  published terms for others; it can refuse an unknown user agent at any
  time and the user cannot do anything about it; using it under Sonarr's
  user agent would be deceptive, so Unarr identifies itself and accepts the
  risk; a popular Unarr would cost the Servarr team bandwidth they did not
  agree to. Mitigations: an honest user agent, a daily cache, a refresh
  interval no shorter than the originals', and asking the Servarr team.
- Size: smallest. A port.

**B. Public providers directly.**
- TVDB v4: the data the originals ultimately show, absolute numbering,
  artwork. Needs an API key: TVDB grants project keys to open-source
  projects, but under its current model end users of such a project need a
  TVDB subscription unless the project negotiates a licence; that is the
  friction the Servarr proxies exist to hide. Terms to re-check.
- TMDB: free key with attribution, generous limits, TV data with external
  ids, but TMDB episode numbering differs from TVDB on some shows and scene
  releases follow TVDB, so matching would break where it matters.
- TVMaze: no key, public, 20 requests per 10 seconds, TVDB ids available
  through its lookup, good episode data; weaker on anime and absolute
  numbering, no images beyond poster and background, no scene mappings.
- XEM (`thexem.info`): public API for the scene mappings Skyhook bundles,
  usable directly.
- For: honest, independent of the Servarr team's goodwill, cacheable and
  replaceable per provider.
- Against: every field the UI shows has to be assembled from two or three
  sources into the originals' resource shapes; the goldens from the oracle
  compare shape, not values; some fields have no source; Radarr and Lidarr
  will each need their own assembly.
- Size: two to three times A for Sonarr; MusicBrainz for Lidarr later is the
  largest port of the project.

**C. A hybrid.** Direct providers for identity, episodes and images (TVMaze
first because it needs no key, TVDB when the user supplies a key), XEM
directly for scene mappings, Skyhook as an optional source behind the same
trait for users who accept it, with a clear fallback when any of them is
unavailable.
- For: keeps the installation working whichever upstream disappears; the
  user chooses what they are comfortable with.
- Against: the most code and the most testing, two code paths for every
  field, and the mapping work of B anyway.

**D. Stay import-only.** No lookup, no add, no refresh; the library changes
only by re-importing from a Sonarr. Rejected: it blocks Overseerr and the
UI's Add Series, and it keeps a Sonarr installation alive to feed Unarr.

The provider sits behind one `MetadataProvider` trait in `unarr-core` with
the originals' resource shapes as its output, so a second implementation is
additive. The oracle for the shapes is the real Sonarr pointed at a mock
Skyhook serving recorded responses (public data, nothing to anonymise); the
same recordings feed Unarr's tests, so no test contacts Skyhook. The user
agent sent to Skyhook is one constant in `unarr_core::http`, read nowhere
else; this phase ships it as Sonarr's, and the switch to Unarr's own is the
first item after the exit criteria, not part of them.

## Where phase 3a left things

From `main` after the merge of phase 3a (9 November 2026): 60k lines of Rust,
249 tests, 448 golden requests, CI green, and a homelab run in which Unarr
replaced the maintainer's Sonarr on the NAS, searched through Prowlarr,
grabbed on a real NZBGet, imported the file, posted to Discord and refreshed
the real Plex.

- **Writes that exist**: indexers, download clients and notifications (CRUD,
  `test`, `testall`, `action`, bulk), `release` and `release/push`, `queue`
  removals and grabs, `command`, `episodefile` delete, bulk and editor.
  Everything else is read-only: `series`, `episode`, `tag`, `rootfolder`,
  `qualityprofile`, `customformat`, `importlist`, `importlistexclusion` and
  every `config/*` answer GET only. There is no `series/lookup`, no
  `parse`, no `filesystem`, no `qualityprofile/schema`, no `system/backup`;
  `customformat/schema` exists.
- **Commands accepted without effect**: `RefreshSeries` (and the scheduled
  refresh) completes and moves its task row, but fetches nothing. The
  disk side is real since 3a: `RescanSeries`, `DownloadedEpisodesScan`,
  `RenameFiles`, `RenameSeries`, completed download handling.
- **Left to 3b by 3a**: the series folder move on a `PUT /series` path
  change (3a renames files and folders inside a series; moving the series
  folder itself comes with the series writes); `MediaCover` fetching and
  resizing; the `series` and `episode` SignalR messages a metadata refresh
  emits (the import already emits them through
  `crates/unarr-api/src/relay.rs`).
- **A trap for series writes** (`docs/DESIGN.md` section 17): listing series
  skips the `actors` column, the largest JSON one; a write must load the
  cast first or update columns, never write back a series read from a list.
- **`/series` latency** on the 950-series fixture is still tens of
  milliseconds against the 10 ms target; a per-series cache invalidated by
  writes is the next step section 17 names, and 3b is the phase that adds
  the writes to invalidate on.
- **Notifications**: Discord, Webhook and Plex Media Server are driven; plex.tv
  is never called. 3b's series add and delete fire the `SeriesAdd` and
  `SeriesDelete` events those three already know how to send.
- **Metadata**: decided (`docs/DESIGN.md` section 7, option A, Skyhook with
  the original's user agent during development); nothing implemented. The
  HTTP client lives in `unarr_core::http`.

## Scope

1. **Metadata provider** per the decision: lookup by term and by `tvdb:`,
   `tvmaze:`, `imdb:`, `tmdb:` ids, series and episode fetch, images with
   the originals' resizing (posters at 250 and 500, fanart at 360, banners
   at 70, written under `MediaCover/{seriesId}/`), scene mapping refresh,
   cache and rate limiting, the `RefreshSeries` command made real (new
   episodes, changed air dates, ended status, season monitoring of new
   seasons per `monitorNewItems`).
2. **Series management.** `GET /api/v3/series/lookup`, `POST /api/v3/series`
   with `addOptions` (monitor presets, search for missing and cutoff unmet,
   ignore episodes with files), `PUT /api/v3/series/{id}` (path moves call
   3a's folder rename), `DELETE` with `deleteFiles` and `addImportListExclusion`,
   `PUT /api/v3/series/editor` and `DELETE /series/editor`, `POST
   /api/v3/seasonpass`, `PUT /api/v3/episode/{id}` and `/episode/monitor`,
   `GET /api/v3/series/{id}/folder`, `GET /api/v3/parse?title=` (on the phase 2
   parser), `GET /api/v3/filesystem*` for the UI's folder browser.
   SignalR `series` and `episode` messages for each.
3. **Settings writes.** `PUT` on `config/host`, `config/naming`,
   `config/mediamanagement`, `config/ui`, `config/indexer`,
   `config/downloadclient`, `config/importlist`; CRUD on `tag`,
   `rootfolder`, `qualityprofile`, `qualitydefinition` (and `/update`),
   `customformat` (and `/bulk`), `delayprofile` (and `/reorder`),
   `releaseprofile`, `remotepathmapping`, `customfilter`, `autotagging`,
   `importlistexclusion`; `GET /api/v3/qualityprofile/schema`
   (`/customformat/schema` exists since phase 1). Validation errors in the originals' shape. The
   UI's settings pages save and reload; Recyclarr's sync replayed
   (`scripts/recyclarr-replay.py`).
4. **Import lists.** Provider CRUD and `test` for kind `ImportList`, the
   `ImportListSync` command for Simkl (the fixture's and the maintainer's
   only list), using the metadata provider to add. The rest, Trakt and
   Sonarr included, keep round-tripping, untested.
5. **Housekeeping the UI reaches.** `system/backup` list, create, restore and
   delete (the originals' zip layout, so a Sonarr backup restores into Unarr
   through the importer), `log/file` listing, `system/routes` if the UI asks.
6. **Measurements and docs.** Lookup and add latency, refresh of the whole
   fixture, RSS after a full refresh, in `docs/DESIGN.md` section 9.

Out of scope: Kodi and Plex metadata file writers (kind `Metadata`),
Radarr, Lidarr, HTTPS, the update mechanism.

## Exit criteria

- `make check` green.
- The UI's Add Series flow against Unarr: lookup by term and by `tvdb:`,
  profiles, root folders, tags, `POST /series` with `addOptions`, the
  resulting searches, all recorded as goldens from the original; the Add
  Series page finds, adds and shows a series with posters (Playwright).
- A Simkl `ImportListSync` against a mock Simkl adds the listed series,
  respecting the exclusions, as the original does.
- Recyclarr sync replayed: definitions, profiles and custom formats created
  and updated, round-tripping byte for byte on a second run.
- `RefreshSeries` on the fixture against the recorded metadata updates
  episodes and images; the pinned-clock goldens still pass.
- Every settings page saves and reloads; the PUT goldens match.
- Bazarr and Prowlarr replays still pass.
- Measurements recorded.
- On a homelab, optional: a series added from the UI (or by the Simkl list)
  is searched, grabbed and imported end to end. The cloud session cannot
  meet this one.

## Verification strategy and conventions

As `docs/phase-3a-import.md`. Metadata responses used by tests are recorded
once from the chosen provider and from Skyhook for the oracle run; they are
public data and live under `tests/fixtures/metadata/` (its README says how
each host's answers got there). The recording Sonarr reaches them through
`scripts/mock-servarr.py`, which is the HTTP proxy the request list turns on
with a `PUT /config/host` (certificate validation off, `*.invalid` bypassed
so the fixture's dead providers fail as before) and which terminates the
HTTPS tunnels to the metadata hosts with a self-signed certificate.

## Status, November 2026

| Exit criterion | Status |
|----------------|--------|
| `make check` green | Yes: fmt, clippy `-D warnings`, the unit tests and the golden replay (630 requests recorded from Sonarr 4.0.20.3014, none pending). |
| The UI's Add Series flow: lookup by term and by `tvdb:`, profiles, root folders, tags, `POST /series` with `addOptions`, the resulting searches, recorded as goldens; the Add Series page finds, adds and shows a series with posters | Yes: lookups by term and by every id prefix, the five validation failures of an add, three adds with different monitoring presets and searches, the edits, the move, the editor, the season pass and the deletes are goldens recorded from the original, the metadata hosts answered by the recorded fixtures. With Playwright against the release binary the Add New page found a series, added it and showed its page with poster, fanart and seasons, with no failed request. |
| A Simkl `ImportListSync` against a mock Simkl adds the listed series, respecting the exclusions | Yes: the provider writes, `test`, `testall`, the sync, the series it adds, the excluded one it skips and Clean Library are goldens recorded from the original against the mock Simkl (hand-made answers under `tests/fixtures/metadata/api.simkl.com`). Only Simkl is fetched; the other implementations round-trip their settings, untested. |
| Recyclarr sync replayed: definitions, profiles and custom formats created and updated, round-tripping on a second run | Yes, and against the real client: `tests/fixtures/recyclarr/sonarr-exchange.json` is the exchange between Recyclarr 8.8.1 and the original Sonarr, recorded through a proxy on the maintainer's configuration (a drifted instance, then two syncs). `make recyclarr` replays the requests against `unarrd` and compares every answer; the one difference is the quality definitions read right after their bulk update, which the original serves from a five-second cache (compared by shape). The real Recyclarr was also run against Unarr itself, with the same exchange. |
| `RefreshSeries` on the fixture against the recorded metadata updates episodes and images; the pinned-clock goldens still pass | Yes: goldens for a series gaining episodes, one with changed titles and every series, with the scene mappings and XEM numbering; covers land under `MediaCover/` in the originals' sizes. The auto tagging rules run after each series, and the tracked downloads of removed episodes are matched again (both added after the first status, with unit tests rather than goldens). |
| Every settings page saves and reloads; the PUT goldens match | Yes: each `config/*` PUT and the CRUD of every catalogue are goldens. With Playwright each settings page was changed, saved, reloaded, checked and restored, with no failed request. That run found three gaps, recorded from Sonarr and fixed: the UI's `PUT` without a route id, the session lost when the instance is renamed, and `importlistexclusion/paged`. The fixture's `AllowedHosts` of `*` makes the General page unsavable in Sonarr too; it was checked after correcting that field. |
| Bazarr and Prowlarr replays still pass | Yes, both against the release binary. |
| Measurements recorded | `docs/DESIGN.md` section 9: lookups at 2 to 3 ms on the mock, an add answered in 5 to 22 ms with its episodes there 0.2 s later, the first refresh of 40 series in 5 s and of 952 in 129 s, a repeated one in 0.4 s and 8.6 s, 28 MB and 39 MB at rest afterwards. They found the memory a refresh left behind (123 MB, now 29) and a 40 ms delay in the mock that earlier phases' figures include. |
| Homelab, optional: a series added from the UI or by the Simkl list is searched, grabbed and imported end to end | Not done; the cloud session cannot. No automated check sends Unarr's own requests to the real Skyhook or the real Simkl; that run is where they would first meet. |

Housekeeping (scope item 5): `system/backup` lists, creates, deletes,
downloads and restores, an original Sonarr backup included, the restore
applied at the next start; the daemon writes `logs/sonarr.txt` with its
archives and `log/file` lists and serves them; `system/routes` is not
implemented, the UI does not ask for it. `docs/DESIGN.md` section 14
records the decisions and the divergences, section 17 what stays open.
