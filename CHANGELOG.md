# Changelog

Unarr's own versions, newest first. Unarr's version is not the version of
the originals it answers as: `system/status` reports the original's in
`version`, which clients branch on, and Unarr's in `packageVersion`.

Each entry says what that version promises: the originals it answers as, the
database levels it imports, the clients it was verified with. A new pin of
an original (issue #9) is an entry of its own. How a version is cut:
`docs/releasing.md`.

## Unreleased

The first version, to be cut as 0.1.0: a preview until the soak of issue
#10 is done.

**Answers as**

| Original | Version | Port | API | Web UI |
|---|---|--:|---|---|
| Sonarr | 4.0.20.3014 | 8989 | `/api/v3` | the release's, embedded |
| Radarr | 6.4.4.10685 | 7878 | `/api/v3` | the release's, embedded |

Both run in one process (`--personalities sonarr,radarr`), each with its own
database, settings, API key and log files.

**Imports**, at a personality's first start (`--import-from`): `sonarr.db`
up to migration 240 and `radarr.db` up to 242, with their `config.xml`. The
original's folder is read, never written: going back is starting the
original again, which does not know what Unarr did meanwhile.

**Verified with**

- Recyclarr 8.8.1: its exchange with each original recorded, and Recyclarr
  itself run against Unarr (`make recyclarr`, `make recyclarr-radarr`).
- Prowlarr and Bazarr: the requests they send, replayed from their source
  and from the maintainer's logs (Prowlarr 2.7.0.5723), not yet the programs
  themselves.
- Download clients: NZBGet, qBittorrent and a qBittorrent-compatible client
  replayed from exchanges recorded on the maintainer's installation;
  SABnzbd 4.2.2 and Transmission 4.0.5 replayed from exchanges recorded
  with the original Sonarr and Radarr in a container (the grab, the queue,
  an import, a failure and the removals; SABnzbd's news server a stand-in).
  Not recorded: SABnzbd's repair and unpacking, Transmission with a
  password, a magnet link, Transmission before 4.0.
- Notifications: Discord, the webhook and Plex (a library update, and the
  daily ping to plex.tv that keeps the token alive). Plex's *Authenticate
  with Plex.tv* and its *Server* menu, recorded on both originals and
  served against a mock plex.tv: the PIN, the sign-in page and the token
  as theirs, the server list from a resources list built from plex.tv's
  documented shape (no account's list recorded).

**Differs from the originals**, by decision or not yet done:
`docs/audit/7-triage.md` lists every row, and `docs/DESIGN.md` section 21
the decisions with their options. In short: import scripts are not run
(a health warning says so), the hub is served over WebSockets only, the UI
is in English only, allowed hosts and trusted networks apply without a
restart, a backup download is never kept by a cache (the originals let one
keep it a year), notifications other than Discord, the webhook and Plex keep
their settings and send nothing. Towards indexers, download clients and
notification targets each personality goes by a name of its own,
`UnarrSonarr/<version>` or `UnarrRadarr/<version>`, so that Prowlarr's
statistics count them apart (`docs/DESIGN.md`, section 21.13).

**Fixed before the first version**: the Plex connection's *Authenticate
with Plex.tv* gave no token and its *Server* menu stayed empty, every
action answering `{}` (#16). Both work as in the originals now; a token
imported from them was never affected.

Radarr's *On Movie Added* and *On Movie Delete* sent nothing: Discord and
the webhook were not told, and Plex was not asked to refresh the folder of
a movie deleted with its files, so the movie stayed in Plex (On Deck
included) until Plex's own next scan. Both are sent as the original sends
them now, the webhook's `addMethod` and `movieFolderSize` included, as
recorded from Radarr 6.4.4 (`tests/fixtures/radarr/golden-notifications`).

*On Application Update* sent nothing on either personality: Unarr is not
updated by the originals' updater, and nothing recorded the version a start
ran. Each database now keeps the versions it was started with
(`update_history`, schema 10 for `unarr-sonarr.db`, 2 for
`unarr-radarr.db`), and a start on another version than the last one tells
Discord and the webhook as the originals do ("Radarr updated from 0.1.0 to
0.2.0"). The versions are Unarr's own: the one the originals answer as
moves only with a new pin. The first start after this change records its
version and tells nobody.

**From a `main` build before `4600d61`**: Sonarr's "Backup Now" wrote its
backups under `Backups/scheduled`: listed as scheduled, and deleted by
the next scheduled backup once older than the retention (28 days unless
set). Move them to `Backups/manual`, where only a manual backup's
clean-up reaches them; Radarr's were already there.

**The image**: `ghcr.io/bateman/unarr`, a static musl build on distroless,
for amd64 and arm64, with jemalloc as its allocator (musl's own served a
fifth of the requests under load: `docs/DESIGN.md`, section 21.6).
