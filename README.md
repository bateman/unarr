# Unarr

**One *arr, written in Rust.** A lightweight, single-binary reimplementation of
Sonarr, Radarr and Lidarr that keeps their APIs, so Prowlarr, Bazarr, Overseerr,
Recyclarr and your dashboards keep working unchanged. Drop it in, import your
existing databases and config, free a few hundred megabytes of RAM.

Status: **Sonarr and Radarr are complete**, audited against the originals'
source, and have run in place of them on the maintainer's homelab since
November 2026, both in one process and one container. No version is cut yet:
the first will be a preview until the soak of
[#10](https://github.com/bateman/unarr/issues/10) is over
([CHANGELOG.md](CHANGELOG.md)).

For each of the two, Unarr imports the original's database and `config.xml`,
serves `/api/v3` with the original JSON and the SignalR feed, runs the
official web UI from the binary, searches Newznab and Torznab indexers
(Prowlarr manages them), decides with the originals' rules, hands releases to
SABnzbd, NZBGet, qBittorrent or Transmission, imports and renames what they
finish, notifies Discord, webhooks and Plex, looks up, adds and refreshes
series and movies from the originals' metadata servers, and saves every
settings page. Prowlarr, Bazarr and Recyclarr were run against both on the
homelab and needed no change. A panic costs one request or one command, not
the process.

What is not there: notifications other than those three, import lists other
than Simkl and the IMDb watchlist, download clients other than those four,
metadata file writers, script import, HTTPS, and a UI language other than
English. `--personalities lidarr` starts but is not usable.

Next: Prowlarr as a fourth personality
([#12](https://github.com/bateman/unarr/issues/12)), then Lidarr
([#13](https://github.com/bateman/unarr/issues/13)). The versions of the
originals Unarr answers as are pinned in `upstream/` and followed release by
release ([#9](https://github.com/bateman/unarr/issues/9)).

See [docs/DESIGN.md](docs/DESIGN.md) for the reasoning and every decision,
the work orders of each phase under `docs/`,
[docs/audit/README.md](docs/audit/README.md) for the audits,
[docs/homelab-compose.md](docs/homelab-compose.md) for running it in place of
the originals, and [docs/originals-comparison.md](docs/originals-comparison.md)
for what it saves against them in RAM, CPU and latency.

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

### Importing an existing Sonarr or Radarr

    unarrd --data /config --import-from /path/to/sonarr/config
    unarrd --data /config --personalities sonarr,radarr \
           --import-from /path/to/sonarr/config,/path/to/radarr/config

`--data` (or `UNARR_CONFIG`, default `/config`) is where Unarr keeps its own
files, one directory per personality. On first start, if that directory has no
database, Unarr reads `config.xml` and `sonarr.db` (or `radarr.db`) from
`--import-from`, keeps the port, URL base, API key and authentication
settings, and reuses the `MediaCover` folder next to them for posters. The
original files are not modified. Every client that held the original's API
key keeps working; the web UI login is the one configured in the original.

### Web UI

The official Sonarr and Radarr frontends are embedded at build time. `make ui`
fetches them from the pinned releases into `crates/unarr-api/ui/sonarr/` and
`ui/radarr/` before `cargo build`; without them the binary serves the API and
a placeholder page.

## Development

    make help          # list targets
    make check         # fmt check, clippy, tests (what CI runs)
    make panic-test    # what a panic costs, on a release build with the test-only switch (CI runs it too)
    make ui-smoke      # every page of both embedded UIs in a headless browser, on the fixtures (CI runs it too)
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
