#!/usr/bin/env bash
# Run the real Sonarr on the phase 1 fixture: record golden responses, fetch the UI.
#
#   scripts/sonarr-fixture.sh golden    # tests/fixtures/sonarr/golden/*.json
#   scripts/sonarr-fixture.sh shell     # start Sonarr on a copy of the fixture, keep it up
#   scripts/sonarr-fixture.sh ui        # copy the official web UI into crates/unarr-api/ui/sonarr
#
# The official linux-x64 release is self-contained (.NET runtime included), so
# no Docker daemon is needed. The tarball is downloaded once, pinned by SHA-256,
# into .cache/sonarr/. The fixture itself is an anonymised real database, see
# tests/fixtures/sonarr/README.md.
#
# Requirements: bash, curl, tar, python3; libicu for `golden`.
set -euo pipefail

SONARR_VERSION="${SONARR_VERSION:-4.0.20.3014}"
SONARR_SHA256="${SONARR_SHA256:-1fc48544b5a3401b2fc3df8d0642c1e6a73a28e41a351169edd9cd8f54770db5}"
SONARR_URL="https://github.com/Sonarr/Sonarr/releases/download/v${SONARR_VERSION}/Sonarr.main.${SONARR_VERSION}.linux-x64.tar.gz"
SONARR_PORT="${SONARR_PORT:-8989}"

REPO="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="${UNARR_CACHE:-$REPO/.cache/sonarr}"
FIXTURE_DIR="$REPO/tests/fixtures/sonarr"
GOLDEN_DIR="$FIXTURE_DIR/golden"
API_KEY="0123456789abcdef0123456789abcdef"

# .NET needs ICU for culture-aware comparisons and language names, which is
# what the Docker images ship. Without it, fall back to invariant globalization
# and warn: sort orders and language tokens in the goldens would differ.
if [ "$(uname -s)" = Linux ] && ! ldconfig -p 2>/dev/null | grep -q libicuuc; then
    echo "[fixture] libicu not found: running Sonarr with invariant globalization" >&2
    export DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1
fi

log() { printf '\e[1;34m[fixture]\e[0m %s\n' "$*" >&2; }
die() { printf '\e[1;31m[fixture]\e[0m %s\n' "$*" >&2; exit 1; }

download() {
    local tarball="$CACHE/Sonarr.main.${SONARR_VERSION}.linux-x64.tar.gz"
    mkdir -p "$CACHE"
    if [ ! -x "$CACHE/Sonarr/Sonarr" ]; then
        if [ ! -f "$tarball" ]; then
            log "downloading Sonarr $SONARR_VERSION"
            curl -fsSL -o "$tarball" "$SONARR_URL"
        fi
        echo "$SONARR_SHA256  $tarball" | sha256sum -c - >/dev/null || die "checksum mismatch for $tarball"
        log "extracting"
        tar -xzf "$tarball" -C "$CACHE"
    fi
}

SONARR_PID=""
start_sonarr() {
    local data="$1"
    log "starting Sonarr on port $SONARR_PORT, data dir $data"
    "$CACHE/Sonarr/Sonarr" -nobrowser -data="$data" >"$data/sonarr.stdout.log" 2>&1 &
    SONARR_PID=$!
    for _ in $(seq 1 120); do
        if curl -fsS -o /dev/null -H "X-Api-Key: $API_KEY" "http://127.0.0.1:$SONARR_PORT/api/v3/system/status" 2>/dev/null; then
            log "Sonarr is up"
            return
        fi
        kill -0 "$SONARR_PID" 2>/dev/null || die "Sonarr exited early, see $data/sonarr.stdout.log"
        sleep 1
    done
    die "Sonarr did not come up, see $data/sonarr.stdout.log"
}

stop_sonarr() {
    [ -n "$SONARR_PID" ] || return 0
    log "stopping Sonarr"
    kill -TERM "$SONARR_PID" 2>/dev/null || true
    for _ in $(seq 1 60); do
        kill -0 "$SONARR_PID" 2>/dev/null || break
        sleep 1
    done
    kill -KILL "$SONARR_PID" 2>/dev/null || true
    wait "$SONARR_PID" 2>/dev/null || true
    SONARR_PID=""
}
trap stop_sonarr EXIT

# Start Sonarr on a throwaway copy of the fixture, so recording never mutates it.
start_on_fixture_copy() {
    download
    [ -f "$FIXTURE_DIR/sonarr.db" ] || die "no fixture yet, run '$0 fixture' first"
    local data="$CACHE/golden-data"
    rm -rf "$data"
    mkdir -p "$data"
    cp "$FIXTURE_DIR/sonarr.db" "$data/sonarr.db"
    cp "$FIXTURE_DIR/config.xml" "$data/config.xml"
    # Mark every scheduled task as just run, so Sonarr does not refresh
    # series from Skyhook, sync RSS or clean house while we record: the
    # goldens must describe the fixture as committed.
    python3 -I -c "import sqlite3,sys,datetime; c=sqlite3.connect(sys.argv[1]); c.execute('update ScheduledTasks set LastExecution=?', (datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%d %H:%M:%SZ'),)); c.commit(); c.close()" "$data/sonarr.db"
    start_sonarr "$data"
}

cmd_golden() {
    start_on_fixture_copy
    mkdir -p "$GOLDEN_DIR"
    python3 -I "$REPO/scripts/record-golden.py" \
        --base-url "http://127.0.0.1:$SONARR_PORT" --api-key "$API_KEY" \
        --requests "$GOLDEN_DIR/requests.txt" --out "$GOLDEN_DIR" --login unarr:fixture
    record_schemas
    stop_sonarr
    log "golden files written to $GOLDEN_DIR"
}

# Provider and specification schemas: the field metadata (labels, help text,
# select options) the API returns for each implementation. Unarr embeds them
# to render the same `fields` the originals do. Also the UI strings.
record_schemas() {
    local out="$REPO/crates/unarr-api/schemas/sonarr"
    mkdir -p "$out"
    for r in indexer downloadclient notification importlist metadata customformat autotagging; do
        curl -fsS -H "X-Api-Key: $API_KEY" "http://127.0.0.1:$SONARR_PORT/api/v3/$r/schema" \
            | python3 -I -c "import json,sys; json.dump(json.load(sys.stdin), sys.stdout, indent=1, sort_keys=True, ensure_ascii=False); print()" \
            >"$out/$r-schema.json"
        log "recorded $r/schema"
    done
    cp "$CACHE/Sonarr/Localization/Core/en.json" "$out/localization-en.json"
}

# The official frontend (GPL-3.0, like Unarr) is embedded in unarrd by
# rust-embed from crates/unarr-api/ui/sonarr. Source maps are left out: they
# triple the size and only matter for debugging the UI itself.
cmd_ui() {
    download
    local dest="$REPO/crates/unarr-api/ui/sonarr"
    mkdir -p "$dest"
    find "$dest" -mindepth 1 ! -name .gitkeep -delete
    # Plain cp -R: GNU-only flags such as --parents fail on macOS.
    cp -R "$CACHE/Sonarr/UI/." "$dest/"
    find "$dest" -name '*.map' -type f -delete
    echo "$SONARR_VERSION" >"$dest/VERSION"
    log "UI $SONARR_VERSION copied to $dest ($(du -sh "$dest" | cut -f1))"
}

cmd_shell() {
    start_on_fixture_copy
    log "Sonarr running at http://127.0.0.1:$SONARR_PORT with API key $API_KEY; Ctrl-C to stop"
    wait "$SONARR_PID"
}

case "${1:-}" in
    golden) cmd_golden ;;
    shell) cmd_shell ;;
    ui) cmd_ui ;;
    *) die "usage: $0 {golden|shell|ui}" ;;
esac
