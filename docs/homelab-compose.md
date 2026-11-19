# Replacing the *arr services with Unarr in the homelab compose

What to change in the `homelab` repository when Unarr takes the place of
the `sonarr` and `radarr` services for good. That is done: the homelab
repository has the `unarr` service since its pull requests #571 and #572
(19 November 2026), where the two originals' services are removed and
`UPGRADE.md` says how to go back. Before, the NAS ran Unarr from a local,
uncommitted edit of `docker/compose.media.yml`. What follows is kept as the
record of what the change had to cover, and for the next personalities.

**The container is named `unarr` and answers to the applications' names
as network aliases** (`sonarr`, `radarr`, later `prowlarr` and `lidarr`):
the maintainer's decision of 17 November 2026. The name says what runs, no
personality is the "real" one, and Prowlarr, Bazarr, Recyclarr, the
download clients, Traefik and the Uptime Kuma monitor keep addressing
`sonarr:8989` and `radarr:7878` as before, with the API keys Unarr keeps.
The first runs on the NAS used a container still called `sonarr` with a
`radarr` alias; the examples below are the final form.

What the name changes, once: whatever addresses the container and not the
network (`docker exec`, `docker logs`, backup filters, tests that stub
`container:sonarr`), and Traefik, where each router has to name its own
service and port because one container now has several.

## The service

```yaml
  unarr:
    # Unarr, in place of Sonarr. Pin a release tag once there is one:
    # `main` moves at every push and Watchtower follows it.
    image: ghcr.io/bateman/unarr:main
    container_name: unarr
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
      media_net:
        aliases: [sonarr]
      proxy:
    restart: unless-stopped
    logging: *common-logging
    healthcheck:
      # No shell and no curl in the image: the daemon checks itself.
      test: ["CMD", "/usr/local/bin/unarrd", "healthcheck"]
      <<: *common-healthcheck
    deploy:            # unchanged; lower the limit once measured on the NAS
    depends_on:        # unchanged
    labels:            # watchtower; the Traefik router names its service:
      - traefik.http.routers.sonarr.service=sonarr
      - traefik.http.services.sonarr.loadbalancer.server.port=8989
```

Differences from the Sonarr service, and why:

| Key | Change | Reason |
|-----|--------|--------|
| service and `container_name` | `unarr` | one container for every personality |
| `networks` | alias `sonarr` on `media_net` | what addressed `sonarr:8989` still resolves |
| `image` | `ghcr.io/bateman/unarr:<tag>` | |
| `user` | added | distroless image, no s6 init to honour `PUID`/`PGID` |
| `command` | added | data directory and one-time import source |
| `environment` | `TZ` only, no `*common-env` | `PUID`, `PGID`, `UMASK` mean nothing to it. Do not set `RUST_LOG`: it overrides the Log Level of Settings > General, which is where to ask for debug or trace |
| `volumes` | `./config/unarr:/config`, Sonarr's config read-only | Unarr writes `<data>/sonarr/…`; the original files stay intact, so going back is restoring the old service |
| `healthcheck.test` | `unarrd healthcheck` | `curl` does not exist in the image |

The old `sonarr` service is disabled, not deleted (`profiles:
["disabled"]`, and taken out of the `depends_on` of the services that name
it, which then depend on `unarr`), so that its `container_name` and the
alias do not collide and going back is one edit. Going back to Sonarr is
the old service block again: its `config` directory was never written. What Unarr did in the meantime (history, imported files)
is then unknown to Sonarr.

## The Radarr service, from milestone 4d

Unarr serves both personalities from one process, and the compose file can
do either: a second container, or one container that answers on both ports.
One container is what the measurements of `docs/DESIGN.md` section 9 were
taken on (38 MB idle for both, against two containers' two runtimes), and
it is what the maintainer's run tries first.

```yaml
  unarr:
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

Radarr's database is imported from a copy, made with Radarr stopped and
before the Unarr container is started with Radarr:

```sh
docker compose stop radarr
sudo mkdir -p config/radarr-import
sudo cp -a config/radarr/. config/radarr-import/
ls -la config/radarr-import/radarr.db
```

The `/.` matters: if `config/radarr-import` already exists (Docker creates
a missing bind-mount source, empty, when the container starts before the
copy), `cp -a config/radarr config/radarr-import` copies into
`config/radarr-import/radarr/` and the database is not where Unarr looks.
Unarr refuses to start a personality whose database it finds nowhere and
that it never imported, so a copy in the wrong place stops the start
rather than bringing Radarr up blank.

Not from `config/radarr` mounted read-only: Radarr and Sonarr both keep
their databases in WAL mode, and SQLite opens such a file, even to read
it, only when it can create the `-shm` file next to it. On a read-only
mount the first start fails with `unable to open database file` (error 14)
and that personality is left down, reported by the log and by the health
check, while the other one starts (before the panic decision of
`docs/DESIGN.md`, section 20, the container restarted in a loop with both
down). Sonarr's read-only mount above works only while Sonarr left a
`-shm` file behind; a copy made the same way is the safer source for it
too. The copy is read once, at the first start; it can be deleted once
the import is done, and the original's folder is never touched, so going
back stays possible.

Then the `radarr` service is disabled, not deleted (see below:
`profiles: ["disabled"]`, and `radarr` taken out of the `depends_on` of
the four services that name it), and its name freed for the alias:
Prowlarr, Bazarr, Recyclarr and the Uptime Kuma monitor address Radarr by
name, and the alias keeps the rest of the repository unchanged:

```yaml
    networks:
      media_net:
        aliases: [sonarr, radarr]   # later: prowlarr, lidarr
      proxy:
```

When Prowlarr is a personality too, `prowlarr`, `sonarr` and `radarr`
resolve to the same container and the indexer sync goes over the network
to itself. It works; it is also why the panic decision below matters more
with each personality added.

The homelab's `scripts/auto-recover-cron.sh` runs every fifteen minutes
and reconciles the stack with `docker compose up -d --remove-orphans` over
a fixed list of files (`COMPOSE_FILES`, `compose.yml` and
`compose.media.yml`). An override kept in a file of its own is undone at
its next run: the container is recreated without Radarr and the `radarr`
service, if it is only stopped, comes back. On the run of 2026-11-13 this
happened twelve minutes in, and the checks that followed were answered by
the original Radarr without anything saying so. So either:

- the change goes into `compose.media.yml` itself, before the script
  next runs, with the `radarr` service given `profiles: ["disabled"]`
  rather than commented out (the block stays, for going back). Four
  services have `radarr` in their `depends_on` (`unpackerr`,
  `cleanuparr`, `recyclarr` and `bazarr`, each next to `sonarr`): it is
  removed there, or compose refuses the whole file with `depends on
  undefined service "radarr"`, and with it every later `make` target and
  the script's own reconcile. Validate the edited file with
  `docker compose -f compose.yml -f <new file> config --quiet` before it
  replaces the old one; or
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
- [ ] The writable copy `config/radarr-import`, taken with Radarr stopped,
      with `radarr.db` directly inside it.
- [ ] `auto-recover-cron.sh` either knows of the change or is paused.
- [ ] The `radarr` service has `profiles: ["disabled"]` and no service
      lists it in `depends_on`; `docker compose config --quiet` passes.
- [ ] `make_dir "${CONFIG_ROOT}/unarr"` already exists from the Sonarr
      step; nothing to add for Radarr, which lands under
      `config/unarr/radarr/`.
- [ ] The memory limit raised to cover both personalities, or measured
      again on the NAS: 38 MB idle on 950 series and 2,123 movies, 197 MB
      while the two databases are imported, once.

`docker compose up --dry-run` is no check on the NAS: run as the user it
fails on Container Station's home folder, and run with `sudo` it reports
every container of the stack as to be recreated.

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

- A panic in one personality no longer ends the process: it costs the
  request or the command it happened in, and the other personality keeps
  answering (`docs/DESIGN.md`, section 20). What to look for instead is
  the same line again and again in `logs/*.txt`: `Request Failed. ...
  panicked at` for a request, a command that ends `failed` with
  `panicked at` for its exception. Each is a bug to report.
- A personality that cannot start (its database does not open, its
  import or a migration fails, its port is taken) is left down and the
  other one starts. `unarrd healthcheck` then says `radarr: did not
  start` and the container is unhealthy, so `auto-recover-cron.sh`
  restarts it at its next run: the personality that works is away for
  the second the restart takes, once per run, instead of being down with
  the other.
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
| `scripts/lib/report-common.sh` | finding the container works as it is, as the maintainer reports: `arr_container` looks for an application by container name first and by network alias second, so a container `unarr` with the aliases is found for both. What **breaks** is the request: `ARR_GET` runs `docker exec <container> sh -c …`, and the image has no shell. `make check-usenet` and `make check-seeders` lose their Sonarr rows until the request is made from another container or from the host. Their self-tests (`make test`) do not see it: they stub `docker` |
| `scripts/assess-logs.sh` | **breaks quietly**: `internal_log_paths` reads `/config/logs/sonarr.txt` through `docker exec … sh`. The file is now `/config/sonarr/logs/sonarr.txt` and there is no `sh` nor `tail`: read it from the host path `docker/config/unarr/sonarr/logs/` instead. `docker logs` keeps working |
| `Makefile` `check_arr_health` | finds the `unarr` container through the aliases, like `arr_container` (reads `Health.Status`). Its `Failed to bind` restart-loop count never matches Unarr's log: harmless, but dead for this container. If a personality is still reported as `NOT RUNNING`, ask Unarr itself (`docker exec <container> /usr/local/bin/unarrd healthcheck` prints `radarr: ok` per personality that answers) |
| `scripts/auto-recover-cron.sh` | **undoes an override**: besides restarting unhealthy containers it runs `docker compose up -d --remove-orphans` over the fixed `COMPOSE_FILES` of its line 47, so a change kept in another compose file is reverted within fifteen minutes. Put the change in `compose.media.yml`, or add the file to that list |
| `docker/duplicati-filters.conf`, `docs/operations/runbook-backup-restore.md` | back up `config/unarr` (one sub-folder per personality); the restore steps name Sonarr's files |
| `scripts/check-usenet.sh`, `scripts/test-check-usenet.sh`, the comments in `Makefile` and `UPGRADE.md` | name `sonarr` in comments and in tests; the test stubs Unarr as `container:sonarr`. It would keep passing, and should say `unarr` with the aliases once the compose does |
| `docs/diagram/services.yaml` | the service is now `unarr`: rename `svc: sonarr` and `svc: radarr` or map them to it, with the display name and the one-line role |
| `docs/network/rack-homelab-config.md` service table, `docs/setup/nas-setup.md`, `docs/setup/arr-tuning-guide.md` | one container `unarr` answers on 8989 and 7878 under the names `sonarr` and `radarr`; the Trusted Networks step of the *arr checklist has no counterpart to set by hand if the import carried it over (check on the NAS) |
| `docs/operations/uptime-kuma-monitors.md` | nothing: `http://sonarr:8989/ping` answers the same |
| `docker/recyclarr.yml`, Authelia, Pi-hole records, firewall docs | nothing: same names through the aliases, same ports and API keys |
| Traefik labels | on the `unarr` service: one router and one service per personality, each router naming its service (`traefik.http.routers.<app>.service=<app>`) with its port |
| `docker/compose.media.yml` `depends_on` | services that waited for `sonarr` or `radarr` wait for `unarr` |
| `UPGRADE.md` | the steps for an existing install: create `config/unarr` owned by `PUID:PGID`, `docker pull`, `make up`, wait for the import (start period), `make check-health` |

## To verify on the NAS before committing there

- The memory limit: Sonarr's is 1G; measure Unarr and lower it.
- Whether Watchtower should follow the tag or a pinned release is updated by
  hand.
- That the import carried the authentication settings Authelia relies on.
- Whether one container for both personalities or two containers suits the
  NAS better: one process shares the runtime and the memory, two keep a
  panic in one from ending the other.
