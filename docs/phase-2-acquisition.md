# Phase 2: acquisition

Work order for the second implementation phase. Read `docs/DESIGN.md` first
(reasoning, architecture, decisions, measurements) and `docs/phase-1-sonarr.md`
(what phase 1 delivered and how it was verified). This document says what
phase 2 adds, where it plugs into the phase 1 code, and how it is verified.

## Goal

A Sonarr installation imported into Unarr searches, decides and downloads as
the original does: RSS sync and searches driven by Prowlarr-managed indexers
end with a release handed to a download client and shown in the queue, with
the same API shapes, SignalR messages and history rows the ecosystem expects.
Nothing is imported from the download folder yet: completed download handling
and renaming are phase 3.

## Where phase 1 left things

The map below comes from reading `main` at the end of phase 1 (one commit,
13.5k lines of Rust, 67 tests, 89 goldens, `make check` green, 12.4 MiB
binary, 25 MB RSS after importing the fixture).

- **No network code.** No HTTP client, no XML parser in `unarr-core`
  (`roxmltree` is used only by `unarr-import`), no scheduler, no background
  task besides the listeners.
- **Every API route is a GET.** `POST /signalr/.../negotiate` is the only
  exception. `POST /api/v3/series`, `POST /api/v3/command` and `GET
  /api/v3/release` answer 405 or 404. The store (`unarr-core/src/store/`) has
  `insert_*`, `list_*`, `get_*` and three `INSERT OR REPLACE` config writers;
  no update or delete functions.
- **Release parsing is limited to the release group.**
  `unarr_core::customformat::parse_release_group` and
  `strip_media_extension` are verbatim ports. Nothing parses title, season,
  episode, daily or absolute numbering, quality, language, proper or repack.
- **Custom formats already evaluate.** `customformat::Input` (title, file
  name, group, quality, languages, size, indexer flags, release type, series
  original language), `matches`, `evaluate`, with a per-pattern compiled
  regex cache; `profile::custom_format_score`, `quality_cutoff_not_met`,
  `qualities_below_cutoff`.
- **Quality is a static table.** `quality::SONARR_QUALITIES`, `Quality::by_id`,
  `Quality::weight`, `LANGUAGES`. Nothing derives a quality from a name.
- **Providers are read-only.** `routes/providers.rs` lists and gets
  indexers, download clients, notifications, import lists, metadata, and
  serves `*/schema` from the JSON recorded in
  `crates/unarr-api/schemas/sonarr/`. Settings are stored verbatim as JSON in
  the `providers` table (`settings`, `attributes`), secrets masked on output.
- **The queue is served empty** (`routes/activity.rs`: `queue`,
  `queue_status`, `queue_details` are static). `commands()` and `health()`
  return `[]`. `system/task` renders the imported `scheduled_tasks` rows
  with `nextExecution = lastExecution + interval`; nothing runs them.
- **SignalR can broadcast.** `routes/signalr.rs`: `Hub::broadcast(name,
  body)` over a `tokio::sync::broadcast` channel; the server sends `version`
  on connect and pings every 15 s. Nothing in production calls `broadcast`.
- **Database access** goes through `unarr_core::db::Db`: one dedicated
  thread per database owning the `rusqlite::Connection`, `call(closure)`
  from async code, migrations in `db.rs::MIGRATIONS` tracked by
  `user_version`. Phase 2 tables go in `0002_*.sql`.
- **Importer** (`unarr-import/src/sonarr.rs`) brings indexers, download
  clients, notifications, custom formats, quality profiles and definitions,
  delay profiles, release profiles, remote path mappings, blocklist, history,
  scheduled tasks, scene mappings, the `Config` table. It does not import,
  and does not log, `PendingReleases`, `DownloadHistory`, `IndexerStatus`,
  `DownloadClientStatus`, `Commands`.
- **Tests.** `crates/unarr-api/tests/contract.rs` replays
  `tests/fixtures/sonarr/golden/requests.txt` (`name METHOD path`) through
  the router with the clock pinned to each recording; requests carry no body,
  so POST and PUT goldens need the format and the harness extended. `make
  golden` re-records from the pinned official Sonarr release without Docker.
- **Fixture.** 38 series, 1,012 episodes, 346 files, 15 indexers (Newznab
  and Torznab), 3 download clients (two qBittorrent, one NZBGet), 2 delay
  profiles, 40 custom formats, 220 blocklist rows, 16k scene mappings, no
  pending releases. Credentials and URLs are anonymised, so the imported
  providers cannot be contacted: every network test runs against mocks.

## Scope

In the order the dependencies suggest; each step ends with `make check` green
and something observable.

1. **Infrastructure.** An HTTP client in `unarr-core` (`reqwest` with
   `rustls`, no OpenSSL, one shared client with timeouts and a Unarr user
   agent) and `roxmltree` for Newznab/Torznab. A mock-server harness for
   tests: an axum app or `wiremock` serving recorded Newznab XML and the
   download clients' JSON. Store `update_*` and `delete_*` functions.
2. **Release parser** (`unarr_core::parser`). A port of the originals'
   `Parser`: title, season and episode numbering (standard, multi-episode,
   season pack, multi-season, daily, anime absolute and special), quality
   (`QualityParser`: source, resolution, proper, repack, real, version),
   language (`LanguageParser`), release group (reuse the existing port),
   hash and edition cleanup. Test vectors ported from the Sonarr repository
   (`src/NzbDrone.Core.Test/ParserTests/*`): several hundred cases, GPL like
   Unarr. Nothing else in this phase is right if the parser is wrong.
3. **Provider write endpoints.** `POST`, `PUT`, `DELETE` on
   `/api/v3/indexer` and `/api/v3/downloadclient`, `*/test`, `*/testall`,
   `*/action/*` where the schema uses them, `*/bulk`. Validation errors in
   the originals' shape (400 with `propertyName`, `errorMessage`). This is
   what Prowlarr does: it pushes and updates indexers and checks
   `system/status` and `tag`. Verified with a replay script of Prowlarr's
   sync requests (`scripts/prowlarr-replay.py`, in the style of
   `scripts/bazarr-replay.py`) and by Prowlarr itself when a homelab is at
   hand. The SignalR `indexer` and `downloadclient` messages the UI expects
   after a change.
4. **Indexer clients.** Newznab and Torznab request builders (RSS, `tvsearch`
   by `tvdbid`, `rid`, `tvmazeid`, `q` with season and episode, daily and
   anime variants, categories, `limit` and `offset` paging, `apikey`),
   capabilities (`caps`) with cache, response parsing into a `ReleaseInfo`
   (title, GUID, size, age, indexer, protocol, download URL, info hash and
   seeders for Torznab, indexer flags), per-indexer status and escalating
   back-off (`IndexerStatus`), the `minimumAge`, `retention`, `maximumSize`,
   `rssSyncInterval` settings already served by `config/indexer`.
5. **Release to library mapping and decision engine.** Map a parsed release
   to a series (title, year, scene mappings, tvdb id from the indexer) and to
   episodes (standard, daily, anime numbering, scene numbering); then the
   specifications of the originals' `DecisionEngine`: quality allowed by the
   profile, cutoff and upgrade rules, custom format score and minimum, size
   limits from quality definitions, minimum age and retention, blocklist,
   already-grabbed and already-have, release profile required and ignored
   terms, protocol enabled on a download client, maximum size, season pack
   rules, anime standard format, raw-HD, `Real` and `Proper` handling,
   series monitored, delay profile. Test vectors ported from
   `src/NzbDrone.Core.Test/DecisionEngineTests/*`. Rejections carry the
   originals' reasons: the UI shows them.
6. **Interactive search.** `GET /api/v3/release?episodeId=` and
   `?seriesId=&seasonNumber=` with the original resource (rejections,
   custom formats and score, quality, languages, `episodeRequested`,
   `downloadAllowed`), `POST /api/v3/release` to grab one, `POST
   /api/v3/release/push` for Prowlarr-less setups. Goldens recorded from the
   real Sonarr pointed at the mock indexer, so the shapes come from the
   oracle, not from reading its source.
7. **Download clients.** SABnzbd, NZBGet, qBittorrent, Transmission: add
   (by URL and by content), list, remove, categories or labels, priorities,
   `removeCompletedDownloads` and `removeFailedDownloads`, remote path
   mappings, per-client status and back-off. Each verified against a mock
   that answers like the real one (record the real exchanges once, from a
   homelab, into `tests/fixtures/downloadclients/`). Deluge, rTorrent,
   Aria2, Flood, Hadouken, Vuze, Download Station and Pneumatic stay out of
   this phase; their imported settings keep round-tripping untouched.
8. **Grab, queue and history.** Grabbing writes the `Grabbed` history row
   with the original `data` fields (indexer, size, GUID, protocol, age,
   published date, download client, custom format score...), tracks the
   download, and the queue endpoints become real: `queue` paged, with the
   `includeUnknownSeriesItems` and `includeSeries` and `includeEpisode`
   flags, `queue/status`, `queue/details`, `DELETE /queue/{id}` and
   `/queue/bulk` with `removeFromClient`, `blocklist`, `skipRedownload`;
   `DownloadHistory` rows; SignalR `queue`, `queue/status`, `queue/details`
   broadcasts the UI reduces. Status messages (download client errors,
   missing path) in the original wording where the UI matches on them.
9. **Commands and scheduler.** `POST` and `GET /api/v3/command`,
   `command/{id}`, `DELETE`: `EpisodeSearch`, `SeasonSearch`,
   `SeriesSearch`, `MissingEpisodeSearch`, `CutoffUnmetEpisodeSearch`,
   `RssSync`, `RefreshMonitoredDownloads`, `ClearBlocklist`, plus
   `RefreshSeries` and `RescanSeries` as accepted-but-deferred (phase 3 and
   the metadata decision). A command queue with the original status
   lifecycle (`queued`, `started`, `completed`, `failed`, `aborted`),
   `commands` and `command` SignalR messages, the `system/task` rows driven
   by a real scheduler: RSS sync on `rssSyncInterval`, monitored downloads
   every minute, pending-release processing, housekeeping. Delay profiles
   become `PendingReleases` with `Delay`, `DownloadClientUnavailable`,
   `Fallback` reasons.
10. **Importer and health.** Import `PendingReleases`, `DownloadHistory`,
    `IndexerStatus` and `DownloadClientStatus`; log every table it still
    skips. Health checks the originals raise for this scope: no indexers, no
    download clients, indexers or clients unavailable, indexer RSS or
    search disabled, download client sorting enabled, remote path mapping
    problems. SignalR `health` messages.
11. **Measurements and docs.** RSS and latency on the fixture after an RSS
    sync and a few searches, in `docs/DESIGN.md` section 9; the two phase 1
    targets left open: a full `history/since` on 50,000 rows must stream or
    page rather than build a 48 MB response the process keeps, and
    `/series` at 950 series must come down from 59 ms. Decisions into
    section 11, open questions into section 12.

Out of scope: completed download handling, import and rename (phase 3),
notifications (phase 3), adding or refreshing series and any metadata
provider call (see below), Radarr and Lidarr, HTTPS.

**Series add and refresh.** Overseerr adds series through `POST
/api/v3/series`, and refresh needs a metadata provider. `docs/DESIGN.md`
section 7 leaves the choice open: the Servarr proxies (Skyhook) or TVDB
directly. Phase 2 does not need either for its exit criterion, but it is the
right time to decide: record the decision in section 11 and, if the choice is
Skyhook, implement `series/lookup` and `POST /series` as a last step; if it is
TVDB, note what key and licence it requires and leave the implementation to
phase 3.

## Exit criteria

- `make check` green; parser and decision-engine vectors ported and passing.
- Prowlarr sync replayed against Unarr: indexers created, updated, tested,
  deleted, with the original responses.
- On the fixture with mock indexers and a mock download client: an
  `RssSync` command grabs a wanted episode, a `Grabbed` history row appears
  with the original `data`, the queue shows the item with the original
  resource, SignalR delivers `queue` and `command` messages, and the UI's
  Activity and Queue pages render it.
- Interactive search goldens match the real Sonarr on the mock indexer.
- Contract tests extended to POST, PUT and DELETE with bodies.
- Measurements recorded; the two phase 1 performance targets met.
- When a homelab is available: real Prowlarr, a real indexer and a real
  download client end a search in a download. This is the only criterion
  that cannot be met inside the cloud session; say so if it is not.

## Status, November 2026

| Exit criterion | Status |
|----------------|--------|
| `make check` green; parser and decision-engine vectors ported and passing | Yes: fmt, clippy `-D warnings`, 183 tests including the golden replay, and 1,533 cases from the originals' parser and decision-engine fixtures (`tests/vectors/`, extracted by `scripts/port-vectors.py`) passing as data. |
| Prowlarr sync replayed: indexers created, updated, tested, deleted, with the original responses | Yes: `scripts/prowlarr-replay.py` (`make prowlarr`) issues the sync against the mock indexer and checks each answer; the same shapes are goldens (`indexer-create`, `-update`, `-test`, `-bulk`, `-delete`...). Prowlarr itself was not run in the session. |
| `RssSync` grabs a wanted episode; `Grabbed` history row with the original `data`; queue shows the item; SignalR `queue` and `command`; the UI renders it | Yes on the fixture with the mock indexers and the mock SABnzbd (`crates/unarr-mock/tests/commands.rs`, `downloads.rs`); the history `data` and the queue resource are goldens recorded from the real Sonarr on the same mocks; the SignalR messages are relayed from the runtime's events; the UI's Queue and Activity pages were opened with Playwright against the release binary with an item in the queue. |
| Interactive search goldens match the real Sonarr on the mock indexer | Yes: `release-episode-*`, `release-season`, `release-rss`, `release-push*`, `release-grab-*`, 219 goldens in all, none pending. |
| Contract tests extended to POST, PUT and DELETE with bodies | Yes, with request bodies, `sleep` lines and `{id:name}` references in `requests.txt`. |
| Measurements recorded; the two phase 1 performance targets met | Recorded in `docs/DESIGN.md` section 9 on a fixture scaled to 950 series and 50,000 history rows (`scripts/scale-fixture.py`). `history/since` streams: the 144 MB response no longer stays in the process (idle RSS 22 MB after the import, about 100 MB after everything the measurement does). `/series` at 950 series costs 60 ms in the handler, from 80: not the order of magnitude the design targets ask for; section 13 says what the next step would be. |
| Homelab: real Prowlarr, a real indexer and a real download client end a search in a download | Partly met: the cloud session has no homelab. Exchanges recorded from the maintainer's Sonarr with a real NZBGet and two clients speaking qBittorrent's API are replayed by the mock (`tests/fixtures/downloadclients/`, `crates/unarr-mock/tests/captures.rs`): Unarr's clients read their versions, settings and categories and derive the same directories. The queues were empty in the recording, so a grab ending in a download against a real client is still the open item. |

An independent review of `main` at the end of the phase listed 23 findings
(nine to fix before running against a real installation: the original
`config.xml` rewritten on first start, a scene mapping picking the wrong
episode, NZBGet ids and titles never read, a push that reported a grab it
never made, a queue removal able to delete a library folder, two crashes and
two memory exhaustions from indexer responses; ten should-fix on secrets in
logs, panics on absurd input, a missing index, concurrent refreshes and
transactions; four nice to have) plus some dead code. A recheck of the fixes
confirmed them and added a 24th finding (an import failing after a blank first
start). All are addressed, with a test for each; `docs/DESIGN.md` section 12
records the decisions. A separate review of the branch's final commit found
`history/since` answering an unterminated array when nothing was new, plus
three cleanups; fixed and recorded there too.

Deviations from the scope above, all deliberate: `RefreshSeries`, `RescanSeries`
and the completed-download handling are accepted and complete without effect
(phase 3); the metadata provider decision is deferred to phase 3 with the
alternatives weighed in `docs/DESIGN.md` section 7; the download clients beyond
the four (Deluge, rTorrent, Aria2, Flood, Hadouken, Vuze, Download Station,
Pneumatic) keep their imported settings and are neither tested nor driven;
Radarr and Lidarr are untouched.

## Verification strategy

- **Oracle first.** For every endpoint shape, record from the real Sonarr
  (`make golden` runs the pinned release) before implementing. The release
  can be pointed at a mock Newznab server and a mock download client on
  localhost, which also captures the exact requests the original sends
  (query parameters, categories, paging, user agent) to mirror them.
- **Ported test vectors.** The Sonarr repository's parser and decision engine
  tests are the specification; port them as data, not as prose.
- **Mocks, not credentials.** The fixture's providers are anonymised. Never
  put a real URL, key or password in the tree; the anonymiser refuses to
  write if one survives, keep it that way.
- **No Docker in the cloud session**, as in phase 1. The official release runs
  without it.

## Conventions

- Rust 2021, `make check` before every push (fmt, clippy `-D warnings`,
  tests). `make ui` first on a clean checkout.
- Crate names and identifiers are `unarr`; prose says Unarr.
- Commits: imperative subject, body explains why. No model identifiers.
- One branch for the phase; squash into one commit on `main` at the end, as
  phase 1 did, with the fixture history clean.
- Keep `docs/DESIGN.md` current when a decision changes, and update the
  status table in this document at the end as `docs/phase-1-sonarr.md` does.
