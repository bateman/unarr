# Phase 3a: import, rename, notify

Work order for the first half of phase 3. Phase 3 was split in two after
phase 2 landed: this half finishes the download pipeline on the library the
import brought (completed downloads, rename, notifications), the other half
(`docs/phase-3b-library.md`) adds metadata, series management and the rest of
the write surface. Read `docs/DESIGN.md` first, then `docs/phase-2-acquisition.md`
for how phase 2 verified its work; this phase verifies the same way.

## Goal

A download that phase 2 handed to a client and tracked in the queue ends up as
an episode file in the library, named by the naming settings, with the history
rows, SignalR messages and notifications the originals produce. The user's Plex
sees the new file; their Discord gets the message.

## Where phase 2 left things

- `RefreshMonitoredDownloads` tracks downloads and renders the queue, but the
  completed-download handling is accepted and completes without effect
  (`docs/DESIGN.md` section 12). `RescanSeries` and `DownloadedEpisodesScan`
  are in the same state.
- `unarr_core::naming` is a full port of `FileNameBuilder`, used so far only
  for the naming examples. Rename is wiring, not a port.
- `unarr_core::parser` parses release and file names; the import decision
  reuses it on file names.
- Notifications exist as imported providers (`providers` table, kind
  `Notification`), listed read-only with their schema; nothing sends.
- `unarr_core::fs` has the disk-space and mount helpers; nothing moves files.
- Media info in `unarr_api::mediainfo` renders the ffprobe summary the
  importer found in `sonarr.db`; nothing probes a file.
- The mock download clients in `unarr-mock` can be told to complete an item;
  the Rust mock mirrors `scripts/mock-servarr.py`, which the golden recorder
  points the real Sonarr at.

## Scope

1. **Completed download handling.** From a tracked download reported complete
   by the client (`outputPath`, remote path mappings applied): scan the
   folder, parse each video file, match to the grabbed episodes (falling back
   to the file name), run the import specifications of the originals
   (`MediaFiles/EpisodeImport/Specifications`: not a sample, not still being
   unpacked, free space, not an upgrade for a worse file unless allowed,
   full season or matching episode count, same episodes not imported twice,
   absolute episode number for anime, grab-matches-file quality), then move
   or hardlink or copy per `config/mediamanagement`, set permissions, write
   the `EpisodeFile` row with media info, mark the download imported, remove
   it from the client when `removeCompletedDownloads` says so, record
   `DownloadFolderImported` (and `EpisodeFileDeleted` for the file it
   replaces, into the recycling bin when configured). Test vectors ported from
   `src/NzbDrone.Core.Test/MediaFiles/EpisodeImport/*`.
2. **Disk scan.** `RescanSeries` and the scheduled rescan: walk the series
   folder, import unmapped files, drop `EpisodeFile` rows whose file is
   gone, respect the `.unarr`/`.plexignore`-style skips the originals have
   (`.AppleDouble`, `@eaDir`, `.grab`, `Extras`, `Featurettes`). Root
   folder missing: stop, do not delete. `DownloadedEpisodesScan` on a path
   for the drop-folder flow and for Unpackerr.
3. **Media info.** `ffprobe` when present on the path (the image ships
   without it: document that the user adds it or mounts it), parsed into the
   originals' `MediaInfo` fields (codecs, bit rates, resolution, scan type,
   runtime, audio channels and languages, subtitle languages, HDR format).
   A native Matroska/MP4 reader is out of scope unless ffprobe proves
   unavailable in practice.
4. **Rename.** `GET /api/v3/rename?seriesId=` and `/renameepisode` previews,
   the `RenameFiles` and `RenameSeries` commands, `EpisodeFileRenamed`
   history, series and season folder renames on `PUT /series` path changes
   (the path change itself lands in 3b; the move is implemented here and
   called there). Uses `unarr_core::naming` as is; any divergence from the
   oracle is fixed in the port, not around it.
5. **Manual import.** `GET /api/v3/manualimport?folder=&downloadId=` and
   `POST /api/v3/manualimport` (the UI's Manual Import and the queue's
   "Manual import" action), `GET /api/v3/episodefile`, `DELETE
   /api/v3/episodefile/{id}` and `/bulk`, `PUT /api/v3/episodefile/editor`
   (quality and language edits).
6. **Notifications.** Provider CRUD, `test` and `testall` for kind
   `Notification` (as phase 2 did for indexers), the event hooks (grab,
   download, upgrade, rename, episode file delete, series add and delete,
   health issue and restored, application update) with the originals'
   payloads, and three implementations:
   - **Plex**: `PlexServer` library update and partial scan (the path-based
     refresh the originals do), authentication token, section mapping.
   - **Discord**: webhook with the embeds the originals send (fields,
     thumbnails, colours per event type).
   - **Webhook**: the generic JSON webhook, because it costs little and
     Notifiarr, Homarr and home automation consume it.
   The other 22 implementations in the schema keep round-tripping their
   imported settings and are neither tested nor driven; selecting one in the
   UI shows it with a validation error saying so.
7. **Health.** The import subset of the originals' checks: root folder
   missing, download client root folder inside a library path, remote path
   mapping needed or wrong, completed download handling disabled, import
   mechanism unavailable, mount read-only.
8. **Measurements and docs.** Import throughput on a mock client completing
   many items, rescan time on the 950-series fixture, RSS after both, in
   `docs/DESIGN.md` section 9; decisions into a new section, open questions
   into the last.

Out of scope: adding, refreshing or deleting series, metadata providers,
MediaCover, the remaining settings writes (all 3b); subtitle and extra files
beyond keeping Bazarr's files where they are; Radarr and Lidarr.

## Exit criteria

- `make check` green; the import specifications carry the originals' vectors.
- On the fixture with the mock SABnzbd and mock qBittorrent completing items:
  the files appear under `/tv` named by the fixture's naming settings, the
  `EpisodeFile` rows carry media info from ffprobe, history shows
  `DownloadFolderImported` (and the delete for an upgrade), the queue empties,
  SignalR delivers `episodefile`, `episode`, `series` and `queue` updates, and
  the UI's Activity, Series and Manual Import pages render the result.
- Plex, Discord and Webhook mocks receive the originals' payloads, recorded
  as goldens from the real Sonarr pointed at the same mocks.
- Bazarr replay still passes on the imported files (`episodeFile` paths,
  media info fields it reads).
- Measurements recorded.
- On a homelab: one real completed download imported and the real Plex
  library refreshed. As before, the only criterion the cloud session cannot
  meet.

## Verification strategy

As phase 2: record from the real Sonarr first (`make golden` with the mocks
completing items and `ffprobe` available on the recorder), port the originals'
tests as data, no real credentials in the tree. File operations run in
temporary directories inside the tests; hardlink and reflink paths are tested
where the filesystem allows and skipped with a note where it does not.

## Conventions

As `docs/phase-2-acquisition.md`: `make ui && make check` before every push,
`unarr` in identifiers and Unarr in prose, no model identifiers in commits,
one branch squashed into one commit on `main` at the end, status table
appended to this document when done.

## Status, November 2026

| Exit criterion | Status |
|----------------|--------|
| `make check` green; the import specifications carry the originals' vectors | Yes: fmt, clippy `-D warnings`, 249 tests including the golden replay. The originals' specification fixtures (`MediaFiles/EpisodeImport/Specifications`, eleven of them) assert on mocks rather than on `[TestCase]` data, so they are ported as unit tests in `crates/unarr-core/src/mediafiles/specs.rs`, the originals' cases grouped by specification, with the sample-detection and scene-name rules alongside; the parser and decision-engine vectors of phase 2 still run as data. |
| Mock SABnzbd and qBittorrent completing items: files under `/tv` named by the naming settings, `EpisodeFile` rows with ffprobe media info, `DownloadFolderImported` history (and the delete for an upgrade), the queue empties, SignalR delivers `episodefile`, `episode`, `series` and `queue`, the UI renders it | Yes: 342 goldens recorded from the real Sonarr pointed at the mocks replay identically, none pending (`import-*`, `upgrade-*`, `manualimport-*`, `rescan-*`, `scan-*`, `rename-*`, `delete-*`, `episodefile-*`, the commands and their progress messages); the replay probes with the ffprobe the pinned Sonarr ships with. The SignalR messages are relayed from the runtime's import events (`crates/unarr-api/src/relay.rs`). The Series, Activity > History and Activity > Queue pages were opened with Playwright against the release binary after importing 352 items from the mock SABnzbd. |
| Plex, Discord and Webhook mocks receive the originals' payloads, recorded as goldens | Yes: `notifications-after-test`, `notifications-after-testall`, `import-grab-notifications`, `import-notifications`, `manualimport-notifications`, `upgrade-notifications`, `rescan-notifications`, `scan-notifications`, `rename-notifications` and `delete-notifications` hold what the mock targets received from the real Sonarr; Unarr's replay matches them (embed timestamps and file `dateAdded` aside; a delete's notifications as a multiset and the Plex refreshes around the library scans left out, see `docs/DESIGN.md` section 13). The CRUD, `test` and `testall` shapes are goldens too, the 500 of an unreachable target included. |
| Bazarr replay still passes on the imported files | Yes: `scripts/bazarr-replay.py` against the release binary after the imports above, 1,012 episodes and 352 files, every field Bazarr reads present with the expected type. |
| Measurements recorded | `docs/DESIGN.md` section 9: 9.5 files per second through `ProcessMonitoredDownloads` on the mock SABnzbd (about 105 ms per file, 45 of them the mock's removal call), the same with Discord, webhook and Plex notifications on; `RescanSeries` over the 950-series fixture in 38 s when every file needs probing and 1 s when nothing changed; RSS after each. |
| Homelab: one real completed download imported and the real Plex library refreshed | Yes, on 9 November 2026. Unarr ran on the maintainer's NAS in place of the stopped Sonarr 4.0.20, with its data imported from a copy of Sonarr's configuration (39 series, 1,027 episodes, 322 files) and the ffprobe Sonarr ships with. An automatic search for a missing episode queried 14 indexers through Prowlarr (271 releases), grabbed one on a real NZBGet and posted the Discord grab embed; when NZBGet finished, Unarr imported the file into the series folder under the name Sonarr gives it, deleted the download folder and the NZBGet history entry, posted Import Complete and refreshed the real Plex server, where the episode appeared among the recently added. The log held no warning or error. Plex's library sees the media under another path than Unarr does: with no path mapping Unarr found no section location matching its own and asked every TV section to refresh the series folder, as Sonarr does; with the mapping set (`/data/media/tv` to `/media/tv`) a later deletion matched the section directly. Deleting an episode file from the UI removed it for good (no recycling bin is configured), as Sonarr's message says, and refreshed Plex the same way. The maintainer's trace logs of two days of Sonarr's own imports confirmed the sequence beforehand and gave the `nzbget-import`, `qbittorrent-import` and `qbittorrent-compat-import` captures replayed by `crates/unarr-mock/tests/captures.rs`. The homelab criterion for rename is the preview (`GET /rename?seriesId=`), as agreed during the phase. |

Deviations from the scope above, all deliberate: the notifications drive
Discord, the generic webhook and the Plex Media Server; the other
implementations keep their imported settings, list with their schema and
answer a test with a validation error saying Unarr does not drive them;
plex.tv (sign-in, server list, ping) is not called. The series folder move
on a `PUT /series` path change lands with the series writes of 3b. The
`.unarr` ignore file is not read; the originals' skips are (hidden
folders, `@eaDir`, `Plex Versions`, the extras folders from `Extras` to
`Trailers`, the `-trailer`/`-featurette` file suffixes, `Thumbs.db` and
`.DS_Store`, samples, the clients' working folders). A health run after an import is debounced
five seconds, as the originals', so the replay takes a minute instead of
half of one. `docs/DESIGN.md` section 13 records the decisions and section
14 what stays open.
