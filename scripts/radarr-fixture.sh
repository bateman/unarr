#!/usr/bin/env bash
# Run the real Radarr on the phase 4a fixture: record golden responses, fetch the UI.
#
#   scripts/radarr-fixture.sh golden    # tests/fixtures/radarr/golden/*.json
#   scripts/radarr-fixture.sh shell     # start Radarr on a copy of the fixture, keep it up
#   scripts/radarr-fixture.sh ui        # copy the official web UI into crates/unarr-api/ui/radarr
#
# The twin of scripts/sonarr-fixture.sh, kept apart from it on purpose: each
# script records the goldens of one application, and a change to one cannot
# disturb the recordings of the other. What both need, such as the ICU check
# below, is changed in both. The official linux-core-x64 release is
# self-contained (.NET runtime included), so no Docker daemon is needed. The
# tarball is downloaded once, pinned by SHA-256, into .cache/radarr/. The
# fixture itself is an anonymised real database, see
# tests/fixtures/radarr/README.md.
#
# Requirements: bash, curl, tar, python3; libicu for `golden`. `golden` and
# `shell` need /movies (MOVIES_DIR), the fixture's root folder, and
# /downloads (DOWNLOADS_DIR), where the mock download clients say their
# files are, to exist: they are created if missing (root, or a writable /)
# and emptied before each recording; the script refuses to empty a directory
# it did not mark as its own. Both commands start scripts/mock-servarr.py,
# whose movie indexers and download clients the requests of milestone 4b
# register; Radarr runs without the environment's proxy and reaches nothing
# else.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
# The release is pinned in upstream/radarr.pin, the one place its version is
# written (issue #9); RADARR_VERSION and RADARR_SHA256 still override it.
PIN="$REPO/upstream/radarr.pin"
[ -r "$PIN" ] || { echo "[fixture] cannot read $PIN, where the release is pinned" >&2; exit 1; }
pinned() {
    sed -n "/^$1[[:space:]]*=/{s/^[^=]*=[[:space:]]*//;s/[[:space:]]*$//;p;q;}" "$PIN"
}
RADARR_VERSION="${RADARR_VERSION:-$(pinned version)}"
RADARR_SHA256="${RADARR_SHA256:-$(pinned sha256)}"
[ -n "$RADARR_VERSION" ] && [ -n "$RADARR_SHA256" ] || { echo "[fixture] no version or sha256 in $PIN" >&2; exit 1; }
RADARR_URL="https://github.com/Radarr/Radarr/releases/download/v${RADARR_VERSION}/Radarr.master.${RADARR_VERSION}.linux-core-x64.tar.gz"
RADARR_PORT="${RADARR_PORT:-7878}"

CACHE="${UNARR_CACHE:-$REPO/.cache/radarr}"
FIXTURE_DIR="$REPO/tests/fixtures/radarr"
GOLDEN_DIR="$FIXTURE_DIR/golden"
API_KEY="0123456789abcdef0123456789abcdef"

# .NET needs ICU for culture-aware comparisons and language names, which is
# what the Docker images ship. Without it, fall back to invariant
# globalization and warn; `golden` refuses instead, since sort orders and
# language tokens in the goldens would differ. The library list is read
# whole before it is searched: `ldconfig -p | grep -q` under pipefail fails
# now and then, when grep leaves before ldconfig has finished writing, and
# a recording then ran without ICU although it was installed.
HAVE_ICU=1
if [ "$(uname -s)" = Linux ]; then
    libraries="$(ldconfig -p 2>/dev/null || true)"
    case "$libraries" in
        *libicuuc*) ;;
        *) HAVE_ICU=0 ;;
    esac
fi
if [ "$HAVE_ICU" = 0 ]; then
    echo "[fixture] libicu not found: running Radarr with invariant globalization" >&2
    export DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1
fi

log() { printf '\e[1;34m[fixture]\e[0m %s\n' "$*" >&2; }
die() { printf '\e[1;31m[fixture]\e[0m %s\n' "$*" >&2; exit 1; }

download() {
    local tarball="$CACHE/Radarr.master.${RADARR_VERSION}.linux-core-x64.tar.gz"
    mkdir -p "$CACHE"
    if [ ! -x "$CACHE/Radarr/Radarr" ]; then
        if [ ! -f "$tarball" ]; then
            log "downloading Radarr $RADARR_VERSION"
            curl -fsSL -o "$tarball" "$RADARR_URL"
        fi
        echo "$RADARR_SHA256  $tarball" | sha256sum -c - >/dev/null || die "checksum mismatch for $tarball"
        log "extracting"
        tar -xzf "$tarball" -C "$CACHE"
    fi
}

MOCK_PORT="${MOCK_PORT:-18989}"
MOCK_PID=""
MOVIES_DIR="${MOVIES_DIR:-/movies}"
DOWNLOADS_DIR="${DOWNLOADS_DIR:-/downloads}"
MARKER=".unarr-fixture"

# The fixture database says /movies and the mock clients say /downloads, so
# both must exist for real or the root folder is reported missing and the
# clients' folders unreachable; each gets a marker so that a later run
# empties only what this script owns.
prepare_dirs() {
    local d
    for d in "$MOVIES_DIR" "$DOWNLOADS_DIR"; do
        if [ -d "$d" ]; then
            [ -f "$d/$MARKER" ] || die "$d exists but was not created by this script (no $d/$MARKER); point MOVIES_DIR/DOWNLOADS_DIR elsewhere or create the marker yourself"
            find "$d" -mindepth 1 -delete
        else
            mkdir -p "$d" 2>/dev/null || die "cannot create $d; run as root or set MOVIES_DIR and DOWNLOADS_DIR to writable paths"
        fi
        touch "$d/$MARKER"
        log "using $d (emptied)"
    done
}

# The mock indexers and download clients the real Radarr is pointed at while
# recording (milestone 4b on); their requests are logged for the request
# builders to mirror. The indexers are the ones under /movies.
start_mock() {
    local logfile="$CACHE/mock-requests.jsonl"
    # The mock's log and certificate go to the cache, which the download
    # makes only later on a fresh clone.
    mkdir -p "$CACHE"
    rm -f "$logfile"
    # A stale mock from an earlier run would answer the health check below
    # and swallow the request log.
    if curl -fsS -o /dev/null "http://127.0.0.1:$MOCK_PORT/movies/newznab/api?t=caps&apikey=$API_KEY" 2>/dev/null; then
        die "something already listens on port $MOCK_PORT; stop it (pkill -f mock-servarr.py) and retry"
    fi
    # The mock is also the HTTP proxy the library's request list turns on
    # (milestone 4d): it answers for api.radarr.video, the Servarr services
    # and TMDB's image host from tests/fixtures/metadata, inside TLS tunnels
    # under this certificate. RECORD_METADATA=1 lets it fetch an answer it
    # does not have from the real host, once, and keep it.
    local cert="$CACHE/mock-tls.crt" key="$CACHE/mock-tls.key" record=()
    if [ ! -f "$cert" ]; then
        openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=unarr-mock" \
            -addext "subjectAltName=DNS:api.radarr.video,DNS:radarr.servarr.com,DNS:image.tmdb.org" \
            -keyout "$key" -out "$cert" 2>/dev/null || die "openssl could not make the mock's certificate"
    fi
    [ "${RECORD_METADATA:-0}" = 1 ] && record=(--record-metadata) && log "metadata fixtures missing from tests/fixtures/metadata are fetched from the real hosts"
    python3 -I "$REPO/scripts/mock-servarr.py" --listen "127.0.0.1:$MOCK_PORT" --fixtures "$REPO/tests/fixtures" --log "$logfile" --downloads "$DOWNLOADS_DIR" --movies "$MOVIES_DIR" \
        --tls-cert "$cert" --tls-key "$key" "${record[@]}" 2>"$CACHE/mock.stderr.log" &
    MOCK_PID=$!
    for _ in $(seq 1 20); do
        if curl -fsS -o /dev/null "http://127.0.0.1:$MOCK_PORT/movies/newznab/api?t=caps&apikey=$API_KEY" 2>/dev/null; then
            log "mock services up on port $MOCK_PORT, requests logged to $logfile"
            return
        fi
        sleep 0.5
    done
    die "the mock did not come up, see $CACHE/mock.stderr.log"
}

stop_mock() {
    [ -n "$MOCK_PID" ] || return 0
    # TERM, not INT: a background job of a non-interactive shell ignores INT.
    kill -TERM "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
    MOCK_PID=""
}

RADARR_PID=""
start_radarr() {
    local data="$1"
    log "starting Radarr on port $RADARR_PORT, data dir $data"
    # The fixture's config names the default port; RADARR_PORT moves the
    # copy, for a run beside an Unarr that serves the same fixture.
    sed -i "s|<Port>[0-9]*</Port>|<Port>$RADARR_PORT</Port>|" "$data/config.xml"
    # Without the environment's proxy variables: .NET honours them, and the
    # recording must reach nothing but the mock.
    env -u HTTPS_PROXY -u https_proxy -u HTTP_PROXY -u http_proxy -u ALL_PROXY -u all_proxy \
        "$CACHE/Radarr/Radarr" -nobrowser -data="$data" >"$data/radarr.stdout.log" 2>&1 &
    RADARR_PID=$!
    for _ in $(seq 1 120); do
        if curl -fsS -o /dev/null -H "X-Api-Key: $API_KEY" "http://127.0.0.1:$RADARR_PORT/api/v3/system/status" 2>/dev/null; then
            log "Radarr is up"
            return
        fi
        kill -0 "$RADARR_PID" 2>/dev/null || die "Radarr exited early, see $data/radarr.stdout.log"
        sleep 1
    done
    die "Radarr did not come up, see $data/radarr.stdout.log"
}

stop_radarr() {
    [ -n "$RADARR_PID" ] || return 0
    log "stopping Radarr"
    kill -TERM "$RADARR_PID" 2>/dev/null || true
    for _ in $(seq 1 60); do
        kill -0 "$RADARR_PID" 2>/dev/null || break
        sleep 1
    done
    kill -KILL "$RADARR_PID" 2>/dev/null || true
    wait "$RADARR_PID" 2>/dev/null || true
    RADARR_PID=""
}
trap 'stop_radarr; stop_mock' EXIT

# Start Radarr on a throwaway copy of the fixture, so recording never mutates it.
start_on_fixture_copy() {
    download
    [ -f "$FIXTURE_DIR/radarr.db" ] || die "no fixture at $FIXTURE_DIR/radarr.db"
    local data="$CACHE/golden-data"
    rm -rf "$data"
    mkdir -p "$data"
    cp "$FIXTURE_DIR/radarr.db" "$data/radarr.db"
    cp "$FIXTURE_DIR/config.xml" "$data/config.xml"
    # Mark every scheduled task as just run, so Radarr does not refresh
    # movies and collections from its metadata server, sync RSS or the
    # import lists, or clean house while we record: the goldens must
    # describe the fixture as committed.
    python3 -I -c "import sqlite3,sys,datetime; c=sqlite3.connect(sys.argv[1]); c.execute('update ScheduledTasks set LastExecution=?', (datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%d %H:%M:%SZ'),)); c.commit(); c.close()" "$data/radarr.db"
    # GOLDEN_SETUP names a script that changes the copy before Radarr
    # starts, for a recording of what the original does at start with
    # another installation (one with no user yet, say). It is run with
    # bash and gets the data directory and the database's file name.
    if [ -n "${GOLDEN_SETUP:-}" ]; then
        log "preparing the copy with $GOLDEN_SETUP"
        bash "$GOLDEN_SETUP" "$data" radarr.db
    fi
    start_radarr "$data"
}

cmd_golden() {
    [ "$HAVE_ICU" = 1 ] || die "libicu is needed to record: without it Radarr sorts and names things differently"
    prepare_dirs
    start_mock
    start_on_fixture_copy
    # GOLDEN_REQUESTS and GOLDEN_OUT record another request list somewhere
    # else: a probe of the original, or new requests tried on their own
    # before they join the list (bodies are read next to the list).
    local requests="${GOLDEN_REQUESTS:-$GOLDEN_DIR/requests.txt}" out="${GOLDEN_OUT:-$GOLDEN_DIR}"
    mkdir -p "$out"
    python3 -I "$REPO/scripts/record-golden.py" \
        --base-url "http://127.0.0.1:$RADARR_PORT" --api-key "$API_KEY" --mock-url "http://127.0.0.1:$MOCK_PORT" \
        --requests "$requests" --out "$out" --login unarr:fixture --signalr \
        --data-dir "$CACHE/golden-data"
    [ "$out" = "$GOLDEN_DIR" ] && record_schemas
    stop_radarr
    stop_mock
    log "golden files written to $out"
}

# Provider and specification schemas: the field metadata (labels, help text,
# select options) the API returns for each implementation. Unarr embeds them
# to render the same `fields` the originals do. Also the UI strings.
record_schemas() {
    local out="$REPO/crates/unarr-api/schemas/radarr"
    mkdir -p "$out"
    for r in indexer downloadclient notification importlist metadata customformat autotagging; do
        curl -fsS -H "X-Api-Key: $API_KEY" "http://127.0.0.1:$RADARR_PORT/api/v3/$r/schema" \
            | python3 -I -c "import json,sys; json.dump(json.load(sys.stdin), sys.stdout, indent=1, sort_keys=True, ensure_ascii=False); print()" \
            >"$out/$r-schema.json"
        log "recorded $r/schema"
    done
    cp "$CACHE/Radarr/Localization/Core/en.json" "$out/localization-en.json"
}

# The official frontend (GPL-3.0, like Unarr) is embedded in unarrd by
# rust-embed from crates/unarr-api/ui/radarr. Source maps are left out: they
# triple the size and only matter for debugging the UI itself.
cmd_ui() {
    download
    local dest="$REPO/crates/unarr-api/ui/radarr"
    mkdir -p "$dest"
    find "$dest" -mindepth 1 ! -name .gitkeep -delete
    # Plain cp -R: GNU-only flags such as --parents fail on macOS.
    cp -R "$CACHE/Radarr/UI/." "$dest/"
    find "$dest" -name '*.map' -type f -delete
    echo "$RADARR_VERSION" >"$dest/VERSION"
    log "UI $RADARR_VERSION copied to $dest ($(du -sh "$dest" | cut -f1))"
}

cmd_shell() {
    prepare_dirs
    start_mock
    start_on_fixture_copy
    log "Radarr running at http://127.0.0.1:$RADARR_PORT with API key $API_KEY; Ctrl-C to stop"
    wait "$RADARR_PID"
}

case "${1:-}" in
    golden) cmd_golden ;;
    shell) cmd_shell ;;
    ui) cmd_ui ;;
    *) die "usage: $0 {golden|shell|ui}" ;;
esac
