# Unarr

**One *arr, written in Rust.** A lightweight, single-binary reimplementation of
Sonarr, Radarr and Lidarr that keeps their APIs, so Prowlarr, Bazarr, Overseerr,
Recyclarr and your dashboards keep working unchanged. Drop it in, import your
existing databases and config, free a few hundred megabytes of RAM.

Status: design phase. The skeleton builds, listens on the three ports and answers
`system/status`; nothing else works yet. See [docs/DESIGN.md](docs/DESIGN.md).

## Goals

- **Drop-in**: same ports, same `/api/v3` and `/api/v1` surface, same API keys, same SignalR feed the official UIs use.
- **Import, don't migrate**: reads `sonarr.db`, `radarr.db`, `lidarr.db` and `config.xml` as they are.
- **Small**: one static binary, SQLite inside, tens of megabytes of RAM for all three personalities.
- **Familiar**: the look and feel of the original apps.

## Name

"Unarr" reads as *un arr* ("one arr" in Italian) and *un-arr*. Note that an
unrelated C decompression library of the same name exists in most Linux
distributions, and an unrelated commercial product uses the name at unarr.app.
Container images are published under this repository's own namespace, not under
a top-level `unarr` namespace.

## Running

    docker run --rm -p 8989:8989 -p 7878:7878 -p 8686:8686 ghcr.io/bateman/unarr

The daemon is `unarrd`. Each personality listens on the port of the application it
replaces: Sonarr on 8989, Radarr on 7878, Lidarr on 8686. Select them with
`--personalities sonarr,radarr` or `UNARR_PERSONALITIES`.

## Development

    make help          # list targets
    make check         # fmt check, clippy, tests (what CI runs)
    make run           # run unarrd from source
    make docker        # build ghcr.io/bateman/unarr:dev locally
    make check-name    # name availability across registries (NAME=...)

Workspace layout: `unarr-core` (domain), `unarr-api` (compatible HTTP surface),
`unarr-import` (reads the original databases), `unarrd` (the daemon).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). Unarr reuses code from the GPL-licensed
Sonarr, Radarr and Lidarr projects where it makes sense.
