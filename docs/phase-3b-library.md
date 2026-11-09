# Phase 3b: metadata, series management, settings writes

Work order for the second half of phase 3. It depends on a decision that has
not been taken yet, the metadata provider (below); everything else in the
scope can start before it and most of it does not touch it. Read
`docs/DESIGN.md` first (section 7 holds the metadata trade-offs so far), then
`docs/phase-3a-import.md`.

## Goal

Unarr manages the library, not just the one it imported: series can be looked
up, added, refreshed, edited and deleted; Overseerr adds a series through the
API; Recyclarr syncs quality definitions, profiles and custom formats; every
settings page saves; and the posters come back.

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
   `GET /api/v3/series/{id}/folder`, `GET /api/v3/parse?title=` if phase 2
   did not add it, `GET /api/v3/filesystem*` for the UI's folder browser.
   SignalR `series` and `episode` messages for each.
3. **Settings writes.** `PUT` on `config/host`, `config/naming`,
   `config/mediamanagement`, `config/ui`, `config/indexer`,
   `config/downloadclient`, `config/importlist`; CRUD on `tag`,
   `rootfolder`, `qualityprofile`, `qualitydefinition` (and `/update`),
   `customformat` (and `/bulk`), `delayprofile` (and `/reorder`),
   `releaseprofile`, `remotepathmapping`, `customfilter`, `autotagging`,
   `importlistexclusion`; `GET /api/v3/qualityprofile/schema` and
   `/customformat/schema`. Validation errors in the originals' shape. The
   UI's settings pages save and reload; Recyclarr's sync replayed
   (`scripts/recyclarr-replay.py`).
4. **Import lists.** Provider CRUD and `test` for kind `ImportList`, the
   `ImportListSync` command for the implementation the fixture has (Simkl)
   plus the two most common (Trakt list, Sonarr instance), using the
   metadata provider to add. The rest keep round-tripping, untested.
5. **Housekeeping the UI reaches.** `system/backup` list, create, restore and
   delete (the originals' zip layout, so a Sonarr backup restores into Unarr
   through the importer), `log/file` listing, `system/routes` if the UI asks.
6. **Measurements and docs.** Lookup and add latency, refresh of the whole
   fixture, RSS after a full refresh, in `docs/DESIGN.md` section 9.

Out of scope: Kodi and Plex metadata file writers (kind `Metadata`),
Radarr, Lidarr, HTTPS, the update mechanism.

## Exit criteria

- `make check` green.
- Overseerr's add flow replayed against Unarr (`scripts/overseerr-replay.py`:
  lookup by tvdb id, profiles, root folders, tags, `POST /series`, the
  resulting searches) with the original responses; the UI's Add Series page
  finds, adds and shows a series with posters.
- Recyclarr sync replayed: definitions, profiles and custom formats created
  and updated, round-tripping byte for byte on a second run.
- `RefreshSeries` on the fixture against the recorded metadata updates
  episodes and images; the pinned-clock goldens still pass.
- Every settings page saves and reloads; the PUT goldens match.
- Bazarr and Prowlarr replays still pass.
- Measurements recorded.
- On a homelab: Overseerr adds a series that gets searched, grabbed and
  imported end to end. The cloud session cannot meet this one.

## Verification strategy and conventions

As `docs/phase-3a-import.md`. Metadata responses used by tests are recorded
once from the chosen provider and from Skyhook for the oracle run; they are
public data and live under `tests/fixtures/metadata/`.
