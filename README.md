# Unarr

**One *arr, written in Rust.** A lightweight, single-binary reimplementation of
Sonarr, Radarr and Lidarr that keeps their APIs, so Prowlarr, Bazarr, Overseerr,
Recyclarr and your dashboards keep working unchanged. Drop it in, import your
existing databases and config, free a few hundred megabytes of RAM.

Status: the Sonarr personality is complete (phases 1 to 3b) and audited.
Unarr imports `sonarr.db` and `config.xml`, serves `/api/v3` with the original
JSON, runs the official Sonarr v4 web UI from the binary, searches Newznab and
Torznab indexers (Prowlarr can manage them), decides with the originals'
rules, hands releases to SABnzbd, NZBGet, qBittorrent or Transmission, imports
and renames what they finish, notifies Discord, webhooks and Plex, looks up,
adds and refreshes series, and saves every settings page (Recyclarr syncs
through it). Radarr is under way ([docs/phase-4-radarr.md](docs/phase-4-radarr.md)):
`--personalities sonarr,radarr` imports `radarr.db` and serves the official
Radarr web UI, Bazarr and every read of the API next to Sonarr in one
process, and since milestone 4b searches for movies, decides with Radarr's
rules, grabs, and keeps the queue, the history and the pending releases
(Prowlarr can manage its indexers). It does not import what the clients
finish yet, so it cannot replace Radarr: importing, the library's commands
and the settings writes for movies are the next milestone.
`--personalities lidarr` starts but is not usable. See
[docs/DESIGN.md](docs/DESIGN.md), the work orders of each phase under `docs/`,
[docs/audit/README.md](docs/audit/README.md) and, for running it in place of
Sonarr, [docs/homelab-compose.md](docs/homelab-compose.md). What it saves
against the originals, in RAM, CPU and latency, is measured in
[docs/originals-comparison.md](docs/originals-comparison.md).

## Goals

- **Drop-in**: same ports, same `/api/v3` and `/api/v1` surface, same API keys, same SignalR feed the official UIs use.
- **Import, don't migrate**: reads `sonarr.db`, `radarr.db`, `lidarr.db` and `config.xml` as they are.
- **Small**: one static binary, SQLite inside, tens of megabytes of RAM for all three personalities.
- **Familiar**: the look and feel of the original apps.

## Name

"Unarr" reads as *un arr* ("one arr" in Italian) and *un-arr*.

## Running

    docker run --rm -p 8989:8989 -p 7878:7878 -p 8686:8686 ghcr.io/bateman/unarr

The daemon is `unarrd`. Each personality listens on the port of the application it
replaces: Sonarr on 8989, Radarr on 7878, Lidarr on 8686, on the `BindAddress`
of its `config.xml` unless `--bind` says otherwise. Select them with
`--personalities sonarr,radarr` or `UNARR_PERSONALITIES`; with several, name
each original's folder with `--import-from` (repeated, or comma separated in
`UNARR_IMPORT_FROM`) and each personality imports from the one that holds its
database. The image runs as
user 65532; when mounting a host directory on `/config`, pass `--user` with
its owner, or the daemon cannot create its files there.

The log level is the Log Level of Settings > General (info by default), and
changes as soon as it is saved. Setting `RUST_LOG` on the container
overrides it: leave it unset unless a level per crate is needed, such as
`RUST_LOG=info,unarr=trace`. As in the originals, the console (what
`docker logs` shows) stays at info whatever that level is, unless
`ConsoleLogLevel` in `config.xml` or `Sonarr__Log__ConsoleLevel` names
another; `ConsoleLogFormat` `Clef`, or `SONARR__LOG__CONSOLEFORMAT=Clef`,
writes it as one JSON object a line for a log shipper (`RADARR__` for
Radarr).

### Health check

The image has no shell and no `curl`, so the daemon checks itself:
`unarrd healthcheck` asks `GET /ping` of every personality the running
daemon serves, at the address and under the URL base it recorded when it
started listening (`<data>/listening`), and exits with 0 when all answer.
The image declares it as its `HEALTHCHECK`; in a compose file that sets its
own:

    healthcheck:
      test: ["CMD", "/usr/local/bin/unarrd", "healthcheck"]

The check is a second process: of the daemon's arguments it needs only the
data directory, which the image sets through `UNARR_CONFIG`. A data
directory given to the daemon as `--data` must be given to the check too.

`GET /ping` itself needs no key and answers `{"status":"OK"}`, for monitors
such as Uptime Kuma.

### Importing an existing Sonarr

    unarrd --data /config --import-from /path/to/sonarr/config

`--data` (or `UNARR_CONFIG`, default `/config`) is where Unarr keeps its own
files, one directory per personality. On first start, if that directory has no
database, Unarr reads `config.xml` and `sonarr.db` from `--import-from`, keeps
the port, URL base, API key and authentication settings, and reuses the
`MediaCover` folder next to them for posters. The original files are not
modified. Every client that held the Sonarr API key keeps working; the web UI
login is the one configured in Sonarr.

### Web UI

The official Sonarr and Radarr frontends are embedded at build time. `make ui`
fetches them from the pinned releases into `crates/unarr-api/ui/sonarr/` and
`ui/radarr/` before `cargo build`; without them the binary serves the API and
a placeholder page.

## Development

    make help          # list targets
    make check         # fmt check, clippy, tests (what CI runs)
    make panic-test    # what a panic costs, on a release build with the test-only switch (CI runs it too)
    make run           # run unarrd from source on the Sonarr fixture
    make run-both      # Sonarr on 8989 and Radarr on 7878, on both fixtures
    make ui            # fetch the official Sonarr and Radarr UIs for embedding (needed by make check)
    make golden        # re-record the Sonarr contract-test goldens (golden-radarr for Radarr's)
    make bazarr        # replay Bazarr's requests against a running unarrd (bazarr-radarr for movies)
    make anonymize     # anonymised copy of a backup zip (FROM=..., APP=sonarr|radarr, OUT=...)
    make docker        # build ghcr.io/bateman/unarr:dev locally
    make check-name    # name availability across registries (NAME=...)

The contract tests replay requests recorded from the real Sonarr
(`tests/fixtures/sonarr/golden/`) against Unarr on the imported fixture, an
anonymised real database (`make anonymize` produces one from a Sonarr backup).
`make golden` re-records the responses by running the official Sonarr release.

Workspace layout: `unarr-core` (domain), `unarr-api` (compatible HTTP surface),
`unarr-import` (reads the original databases), `unarrd` (the daemon).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). Unarr reuses code from the GPL-licensed
Sonarr, Radarr and Lidarr projects where it makes sense.
