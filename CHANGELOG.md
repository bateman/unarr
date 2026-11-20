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
  daily ping to plex.tv that keeps the token alive).

**Differs from the originals**, by decision or not yet done:
`docs/audit/7-triage.md` lists every row, and `docs/DESIGN.md` section 21
the decisions with their options. In short: import scripts are not run
(a health warning says so), the hub is served over WebSockets only, the UI
is in English only, notifications other than Discord, the webhook and Plex
keep their settings and send nothing.

**The image**: `ghcr.io/bateman/unarr`, a static musl build on distroless,
for amd64 and arm64, with jemalloc as its allocator (musl's own served a
fifth of the requests under load: `docs/DESIGN.md`, section 21.6).
