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

## Other files of the homelab repository

Its `CLAUDE.md` asks to search the whole repository before a compose
change. Found on 2026-11-11:

| File | What to do |
|------|------------|
| `scripts/setup-folders.sh` | add `make_dir "${CONFIG_ROOT}/unarr"`; keep `sonarr` while the import source is mounted |
| `scripts/lib/report-common.sh` | **breaks**: `ARR_GET` runs `docker exec <container> sh -c …`, and the image has no shell. `make check-usenet` and `make check-seeders` lose their Sonarr rows until the request is made from another container or from the host. Their self-tests (`make test`) do not see it: they stub `docker` |
| `scripts/assess-logs.sh` | **breaks quietly**: `internal_log_paths` reads `/config/logs/sonarr.txt` through `docker exec … sh`. The file is now `/config/sonarr/logs/sonarr.txt` and there is no `sh` nor `tail`: read it from the host path `docker/config/unarr/sonarr/logs/` instead. `docker logs` keeps working |
| `Makefile` `check_arr_health` | works as is (reads `Health.Status`). Its `Failed to bind` restart-loop count never matches Unarr's log: harmless, but dead for this container |
| `scripts/auto-recover-cron.sh` | works as is (reads `Health.Status`) |
| `docker/duplicati-filters.conf`, `docs/operations/runbook-backup-restore.md` | back up `config/unarr`; the restore steps name Sonarr's files |
| `docs/diagram/services.yaml` | `svc: sonarr` still exists, so the workflow passes; change the display name and the one-line role if the diagram should say Unarr |
| `docs/network/rack-homelab-config.md` service table, `docs/setup/nas-setup.md`, `docs/setup/arr-tuning-guide.md` | say which application answers on 8989; the Trusted Networks step of the *arr checklist has no counterpart to set by hand if the import carried it over (check on the NAS) |
| `docs/operations/uptime-kuma-monitors.md` | nothing: `http://sonarr:8989/ping` answers the same |
| `docker/recyclarr.yml`, Traefik labels, Authelia, Pi-hole records, firewall docs | nothing: same name, port and API key |
| `UPGRADE.md` | the steps for an existing install: create `config/unarr` owned by `PUID:PGID`, `docker pull`, `make up`, wait for the import (start period), `make check-health` |

## To verify on the NAS before committing there

- The memory limit: Sonarr's is 1G; measure Unarr and lower it.
- Whether Watchtower should follow the tag or a pinned release is updated by
  hand.
- That the import carried the authentication settings Authelia relies on.
