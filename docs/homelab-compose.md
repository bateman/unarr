# Replacing Sonarr with Unarr in the homelab compose

What to change in the `homelab` repository when Unarr reaches alpha and
takes the place of the `sonarr` service for good. Until then the NAS runs it
from a local, uncommitted edit of `docker/compose.media.yml`.

The container keeps the name `sonarr`: Prowlarr, Bazarr, Recyclarr, the
download clients, Traefik and the Uptime Kuma monitor all address it by
that name and port, and Unarr keeps Sonarr's API key.

## The service

```yaml
  sonarr:
    # Unarr, in place of Sonarr. Pin a release tag once there is one:
    # `main` moves at every push and Watchtower follows it.
    image: ghcr.io/bateman/unarr:main
    container_name: sonarr
    # The image runs as 65532 and ignores PUID/PGID: without this the
    # daemon cannot write its data directory nor hardlink into /data.
    user: "${PUID}:${PGID}"
    command: ["--data", "/config", "--import-from", "/sonarr-config"]
    environment:
      TZ: ${TZ}
      UNARR_PERSONALITIES: sonarr
    volumes:
      - ./config/unarr:/config
      # Read once, at the first start, to import config.xml and sonarr.db;
      # also where the posters are reused from. Never written.
      - ./config/sonarr:/sonarr-config:ro
      - /share/data:/data
    dns: *common-dns
    dns_opt: *common-dns-opt
    networks:
      - media_net
      - proxy
    restart: unless-stopped
    logging: *common-logging
    healthcheck:
      # No shell and no curl in the image: the daemon checks itself.
      test: ["CMD", "/usr/local/bin/unarrd", "healthcheck"]
      <<: *common-healthcheck
    deploy:            # unchanged; lower the limit once measured on the NAS
    depends_on:        # unchanged
    labels:            # unchanged: watchtower, Traefik router and port 8989
```

Differences from the Sonarr service, and why:

| Key | Change | Reason |
|-----|--------|--------|
| `image` | `ghcr.io/bateman/unarr:<tag>` | |
| `user` | added | distroless image, no s6 init to honour `PUID`/`PGID` |
| `command` | added | data directory and one-time import source |
| `environment` | `TZ` only, no `*common-env` | `PUID`, `PGID`, `UMASK` mean nothing to it. Do not set `RUST_LOG`: it overrides the Log Level of Settings > General, which is where to ask for debug or trace |
| `volumes` | `./config/unarr:/config`, Sonarr's config read-only | Unarr writes `<data>/sonarr/…`; the original files stay intact, so going back is restoring the old service |
| `healthcheck.test` | `unarrd healthcheck` | `curl` does not exist in the image |

Going back to Sonarr is the old service block again: its `config` directory
was never written. What Unarr did in the meantime (history, imported files)
is then unknown to Sonarr.

## The Radarr service, from milestone 4d

Unarr serves both personalities from one process, and the compose file can
do either: a second container, or one container that answers on both ports.
One container is what the measurements of `docs/DESIGN.md` section 9 were
taken on (38 MB idle for both, against two containers' two runtimes), and
it is what the maintainer's run tries first.

```yaml
  sonarr:
    # ... as above, with Radarr's data and port added
    command:
      ["--data", "/config",
       "--import-from", "/sonarr-config,/radarr-config"]
    environment:
      TZ: ${TZ}
      UNARR_PERSONALITIES: sonarr,radarr
    volumes:
      - ./config/unarr:/config
      - ./config/sonarr:/sonarr-config:ro
      # A copy, writable: see below.
      - ./config/radarr-import:/radarr-config
      - /share/data:/data
    labels:
      # the Sonarr router as it is, plus Radarr's on 7878
      - traefik.http.services.radarr.loadbalancer.server.port=7878
      # ... the router, the middlewares and the host rule as radarr has them
      # Two services on one container: each router names its own, or
      # Traefik drops both ("too many services").
      - traefik.http.routers.radarr.service=radarr
      - traefik.http.routers.sonarr.service=sonarr
```

Radarr's database is imported from a copy, made with Radarr stopped:

```sh
docker compose stop radarr
sudo cp -a config/radarr config/radarr-import
```

Not from `config/radarr` mounted read-only, as Sonarr's is: Radarr keeps its
database in WAL mode, and SQLite opens such a file, even to read it, only
when it can create the `-shm` file next to it. On a read-only mount the
first start fails with `unable to open database file` (error 14) and the
container restarts in a loop, taking Sonarr down with it. The copy is read
once, at the first start; it can be deleted once the import is done, and
`config/radarr` itself is never touched, so going back to Radarr stays
possible.

Then the `radarr` service is commented out, not deleted, and its
`container_name` freed: Prowlarr, Bazarr, Recyclarr and the Uptime Kuma
monitor address Radarr by name, so either the Unarr container takes an
alias on the media network, or those files are pointed at `sonarr:7878`.
Aliases keep the rest of the repository unchanged:

```yaml
    networks:
      media_net:
        aliases: [radarr]
      proxy:
```

The homelab's `scripts/auto-recover-cron.sh` runs every fifteen minutes
and reconciles the stack with `docker compose up -d --remove-orphans` over
a fixed list of files (`COMPOSE_FILES`, `compose.yml` and
`compose.media.yml`). An override kept in a file of its own is undone at
its next run: `sonarr` is recreated without Radarr and the `radarr`
service, if it is only stopped, comes back. On the run of 2026-11-13 this
happened twelve minutes in, and the checks that followed were answered by
the original Radarr without anything saying so. So either:

- the change goes into `compose.media.yml` itself, with the `radarr`
  service commented out (as above), before the script next runs; or
- for a trial, the script's crontab line is commented out for the
  duration and restored at the end (`/etc/config/crontab`, then
  `crontab /etc/config/crontab`), knowing that no unhealthy container is
  restarted meanwhile.

To tell which application answers on 7878: `GET /api/v3/system/status`
says `"packageAuthor": "Unarr"` and `"appData": "/config/radarr"` for
Unarr, and the original's own values otherwise.

Unarr keeps Radarr's API key, as it keeps Sonarr's, so Recyclarr's
`radarr:` section, Bazarr's movie provider and Prowlarr's application
entry need no change.

### The run's checklist

Before, on the NAS:

- [ ] A backup of `config/radarr` beside `config/sonarr` (the restore
      steps of the runbook name both). Some files under `asp/` belong to
      the containers' user: `sudo tar`.
- [ ] The writable copy `config/radarr-import`, taken with Radarr stopped.
- [ ] `auto-recover-cron.sh` either knows of the change or is paused.
- [ ] `make_dir "${CONFIG_ROOT}/unarr"` already exists from the Sonarr
      step; nothing to add for Radarr, which lands under
      `config/unarr/radarr/`.
- [ ] The memory limit raised to cover both personalities, or measured
      again on the NAS: 38 MB idle on 950 series and 2,123 movies, 197 MB
      while the two databases are imported, once.

The run itself, one movie from search to Plex:

- [ ] The two UIs answer: `http://nas:8989` and `http://nas:7878`, both
      logged in with the existing sessions.
- [ ] Prowlarr syncs its indexers into both.
- [ ] Recyclarr syncs both sections (`recyclarr sync` with the homelab's
      `recyclarr.yml`), and the custom format the maintainer made by hand
      survives.
- [ ] The IMDb watchlist adds a movie (Settings > Import Lists, then the
      list's own sync), and the movie is monitored and searched for.
- [ ] A movie is searched interactively, grabbed to qBittorrent or
      SABnzbd, imported under `/data/media/movies` with the naming the
      settings give it, and announced to Discord and to Plex.
- [ ] Plex plays it: the library saw the file at the path the mapping
      gives it.
- [ ] Bazarr fetches its subtitles (it reads `movie`, `moviefile` and
      `RescanMovie`).
- [ ] The backup task writes a zip under `config/unarr/radarr/Backups`.
- [ ] After a day: the queue, the history and the calendar read as
      Radarr's did, and `make check-health` is green for both ports.

What to watch, from what this milestone left open:

- A panic in one personality ends the process and so the other one too
  (`docs/audit/7-triage.md`, row 1.13): the maintainer decides after this
  run whether the release profile keeps `panic = "abort"`.
- `GET /movie` costs 289 ms at p99 on 2,123 movies (section 9): the movie
  index may feel slower than Radarr's.
- The Discover page's recommendations, trending and popular lists are not
  fetched (`docs/audit/8-radarr.md`).

## Other files of the homelab repository

Its `CLAUDE.md` asks to search the whole repository before a compose
change. Found on 2026-11-11:

| File | What to do |
|------|------------|
| `scripts/setup-folders.sh` | add `make_dir "${CONFIG_ROOT}/unarr"`; keep `sonarr` while the import source is mounted |
| `scripts/lib/report-common.sh` | **breaks**: `ARR_GET` runs `docker exec <container> sh -c …`, and the image has no shell. `make check-usenet` and `make check-seeders` lose their Sonarr rows until the request is made from another container or from the host. Their self-tests (`make test`) do not see it: they stub `docker` |
| `scripts/assess-logs.sh` | **breaks quietly**: `internal_log_paths` reads `/config/logs/sonarr.txt` through `docker exec … sh`. The file is now `/config/sonarr/logs/sonarr.txt` and there is no `sh` nor `tail`: read it from the host path `docker/config/unarr/sonarr/logs/` instead. `docker logs` keeps working |
| `Makefile` `check_arr_health` | works as is (reads `Health.Status`). Its `Failed to bind` restart-loop count never matches Unarr's log: harmless, but dead for this container |
| `scripts/auto-recover-cron.sh` | **undoes an override**: besides restarting unhealthy containers it runs `docker compose up -d --remove-orphans` over the fixed `COMPOSE_FILES` of its line 47, so a change kept in another compose file is reverted within fifteen minutes. Put the change in `compose.media.yml`, or add the file to that list |
| `docker/duplicati-filters.conf`, `docs/operations/runbook-backup-restore.md` | back up `config/unarr`; the restore steps name Sonarr's files |
| `docs/diagram/services.yaml` | `svc: sonarr` still exists, so the workflow passes; change the display name and the one-line role if the diagram should say Unarr |
| `docs/network/rack-homelab-config.md` service table, `docs/setup/nas-setup.md`, `docs/setup/arr-tuning-guide.md` | say which application answers on 8989; the Trusted Networks step of the *arr checklist has no counterpart to set by hand if the import carried it over (check on the NAS) |
| `docs/operations/uptime-kuma-monitors.md` | nothing: `http://sonarr:8989/ping` answers the same |
| `docker/recyclarr.yml`, Traefik labels, Authelia, Pi-hole records, firewall docs | nothing: same name, port and API key. With Radarr in the same container, its Traefik router points at the `sonarr` service on 7878 and the `radarr` alias keeps the rest addressable |
| `UPGRADE.md` | the steps for an existing install: create `config/unarr` owned by `PUID:PGID`, `docker pull`, `make up`, wait for the import (start period), `make check-health` |

## To verify on the NAS before committing there

- The memory limit: Sonarr's is 1G; measure Unarr and lower it.
- Whether Watchtower should follow the tag or a pinned release is updated by
  hand.
- That the import carried the authentication settings Authelia relies on.
- Whether one container for both personalities or two containers suits the
  NAS better: one process shares the runtime and the memory, two keep a
  panic in one from ending the other.
