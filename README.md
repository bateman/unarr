# Unarr

**One *arr, written in Rust.** A lightweight, single-binary reimplementation of
Sonarr, Radarr and Lidarr that keeps their APIs, so Prowlarr, Bazarr, Overseerr,
Recyclarr and your dashboards keep working unchanged. Drop it in, import your
existing databases and config, free a few hundred megabytes of RAM.

Status: phase 2 done, Sonarr acquires. Unarr imports `sonarr.db` and
`config.xml`, serves `/api/v3` with the original JSON, runs the official
Sonarr v4 web UI from the binary, and now searches Newznab and Torznab
indexers (Prowlarr can manage them), decides with the originals' rules,
hands releases to SABnzbd, NZBGet, qBittorrent or Transmission, tracks the
queue, runs the commands and scheduled tasks, and reports health. Completed
downloads wait for phase 3, which imports and renames them and adds series.
Radarr and Lidarr answer `system/status` only. See
[docs/DESIGN.md](docs/DESIGN.md), [docs/phase-1-sonarr.md](docs/phase-1-sonarr.md),
[docs/phase-2-acquisition.md](docs/phase-2-acquisition.md) and, for what comes
next, [docs/phase-3a-import.md](docs/phase-3a-import.md) and
[docs/phase-3b-library.md](docs/phase-3b-library.md).

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
`--personalities sonarr,radarr` or `UNARR_PERSONALITIES`. The image runs as
user 65532; when mounting a host directory on `/config`, pass `--user` with
its owner, or the daemon cannot create its files there.

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

The official Sonarr frontend is embedded at build time. `make ui` fetches it
from the pinned Sonarr release into `crates/unarr-api/ui/sonarr/` before
`cargo build`; without it the binary serves the API and a placeholder page.

## Development

    make help          # list targets
    make check         # fmt check, clippy, tests (what CI runs)
    make run           # run unarrd from source on the Sonarr fixture
    make ui            # fetch the official Sonarr UI for embedding (needed by make check)
    make golden        # re-record the contract-test goldens
    make bazarr        # replay Bazarr's requests against a running unarrd
    make anonymize     # anonymised copy of a Sonarr backup zip (FROM=..., OUT=...)
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
