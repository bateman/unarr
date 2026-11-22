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
# Requirements: bash, curl, tar, python3, openssl; libicu for `golden`. `golden` and
# `shell` also start scripts/mock-servarr.py on port 18989 (MOCK_PORT). While
# `golden` records, the mock is Sonarr's proxy from its first request: it
# answers for Skyhook, the Servarr services (update check, recent releases,
# notifications, clock), XEM and the artwork host from
# tests/fixtures/metadata and refuses any other host at once, so a
# recording waits on nothing outside the machine (issue #9; the hosts
# refused are printed at the end). `shell`, for comparing on real data,
# leaves Sonarr its direct way out, without the environment's proxy. They also
# need /tv and /downloads (TV_DIR, DOWNLOADS_DIR): the fixture's root folder
# and the directory the mock download clients report, so that the real
# Sonarr can import what the mock completes. They are created if missing
# (root, or a writable /) and emptied before each recording; the script
# refuses to empty a directory it did not mark as its own.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
# The release is pinned in upstream/sonarr.pin, the one place its version is
# written (issue #9); SONARR_VERSION and SONARR_SHA256 still override it.
PIN="$REPO/upstream/sonarr.pin"
[ -r "$PIN" ] || { echo "[fixture] cannot read $PIN, where the release is pinned" >&2; exit 1; }
pinned() {
    sed -n "/^$1[[:space:]]*=/{s/^[^=]*=[[:space:]]*//;s/[[:space:]]*$//;p;q;}" "$PIN"
}
SONARR_VERSION="${SONARR_VERSION:-$(pinned version)}"
SONARR_SHA256="${SONARR_SHA256:-$(pinned sha256)}"
[ -n "$SONARR_VERSION" ] && [ -n "$SONARR_SHA256" ] || { echo "[fixture] no version or sha256 in $PIN" >&2; exit 1; }
SONARR_URL="https://github.com/Sonarr/Sonarr/releases/download/v${SONARR_VERSION}/Sonarr.main.${SONARR_VERSION}.linux-x64.tar.gz"
SONARR_PORT="${SONARR_PORT:-8989}"

CACHE="${UNARR_CACHE:-$REPO/.cache/sonarr}"
FIXTURE_DIR="$REPO/tests/fixtures/sonarr"
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

MOCK_PORT="${MOCK_PORT:-18989}"
MOCK_PID=""
TV_DIR="${TV_DIR:-/tv}"
DOWNLOADS_DIR="${DOWNLOADS_DIR:-/downloads}"
MARKER=".unarr-fixture"

# The library and download directories the recording imports through. The
# fixture database says /tv, the mock says /downloads, so both must exist for
# real; each gets a marker so that a later run empties only what this
# script owns.
prepare_dirs() {
    local d
    for d in "$TV_DIR" "$DOWNLOADS_DIR"; do
        if [ -d "$d" ]; then
            [ -f "$d/$MARKER" ] || die "$d exists but was not created by this script (no $d/$MARKER); point TV_DIR/DOWNLOADS_DIR elsewhere or create the marker yourself"
            find "$d" -mindepth 1 -delete
        else
            mkdir -p "$d" 2>/dev/null || die "cannot create $d; run as root or set TV_DIR and DOWNLOADS_DIR to writable paths"
        fi
        touch "$d/$MARKER"
        log "using $d (emptied)"
    done
}
# The mock indexers and download clients the real Sonarr is pointed at while
# recording; their requests are logged for the request builders to mirror.
start_mock() {
    local logfile="$CACHE/mock-requests.jsonl"
    # The mock's log and certificate go to the cache, which the download
    # makes only later on a fresh clone.
    mkdir -p "$CACHE"
    rm -f "$logfile"
    # A stale mock from an earlier run would answer the health check below
    # and swallow the request log.
    if curl -fsS -o /dev/null "http://127.0.0.1:$MOCK_PORT/newznab/api?t=caps&apikey=$API_KEY" 2>/dev/null; then
        die "something already listens on port $MOCK_PORT; stop it (pkill -f mock-servarr.py) and retry"
    fi
    local cert="$CACHE/mock-tls.crt" key="$CACHE/mock-tls.key" record=()
    # Made anew at each start, so that a certificate left in the cache by
    # an older script never lacks a host added since. The original trusts
    # it alone (SSL_CERT_FILE, start_sonarr below).
    openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=unarr-mock" \
        -addext "subjectAltName=DNS:skyhook.sonarr.tv,DNS:services.sonarr.tv,DNS:thexem.info,DNS:artworks.thetvdb.com,DNS:api.simkl.com" \
        -keyout "$key" -out "$cert" 2>/dev/null || die "openssl could not make the mock's certificate"
    [ "${RECORD_METADATA:-0}" = 1 ] && record=(--record-metadata) && log "metadata fixtures missing from tests/fixtures/metadata are fetched from the real hosts"
    python3 -I "$REPO/scripts/mock-servarr.py" --listen "127.0.0.1:$MOCK_PORT" --fixtures "$REPO/tests/fixtures" --log "$logfile" --downloads "$DOWNLOADS_DIR" --tv "$TV_DIR" \
        --tls-cert "$cert" --tls-key "$key" "${record[@]}" 2>"$CACHE/mock.stderr.log" &
    MOCK_PID=$!
    for _ in $(seq 1 20); do
        if curl -fsS -o /dev/null "http://127.0.0.1:$MOCK_PORT/newznab/api?t=caps&apikey=$API_KEY" 2>/dev/null; then
            log "mock services up on port $MOCK_PORT, requests logged to $logfile"
            return
        fi
        sleep 0.5
    done
    die "the mock did not come up, see $CACHE/mock.stderr.log"
}

# The hosts outside the machine the original asked for and the mock
# refused (each refusal is also in the mock's log): the recording did not
# wait on them, and did not reach them either.
report_refused() {
    local logfile="$CACHE/mock-requests.jsonl" hosts
    [ -f "$logfile" ] || return 0
    hosts="$(python3 -I -c 'import json,re,sys
seen=set()
for line in open(sys.argv[1], encoding="utf-8"):
    m=re.fullmatch(r"(?:connect|proxy) (\S+) refused", json.loads(line).get("served", ""))
    if m: seen.add(m.group(1))
print(" ".join(sorted(seen)))' "$logfile")"
    [ -z "$hosts" ] || log "refused by the mock, so never reached: $hosts"
}

stop_mock() {
    [ -n "$MOCK_PID" ] || return 0
    # TERM, not INT: a background job of a non-interactive shell ignores INT.
    kill -TERM "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
    MOCK_PID=""
}

SONARR_PID=""
# Set by `golden`: the original goes out only through the mock.
MOCK_IS_PROXY=0
start_sonarr() {
    local data="$1"
    log "starting Sonarr on port $SONARR_PORT, data dir $data"
    # The fixture's config names the default port; SONARR_PORT moves the
    # copy, for a run beside an Unarr that serves the same fixture.
    sed -i "s|<Port>[0-9]*</Port>|<Port>$SONARR_PORT</Port>|" "$data/config.xml"
    # While recording, the mock is the original's only way out, from its
    # first request: the update check, the notifications and the clock of
    # the Servarr services come in the main request list, before any list
    # turns the proxy on in the settings. .NET takes its default proxy from
    # the environment, so HTTPS goes to the mock, which answers for the
    # metadata hosts and refuses every other CONNECT at once; plain HTTP to
    # another host gets the same refusal. NO_PROXY keeps the mock's own
    # services and the fixture's dead *.invalid providers on the direct
    # path, where they fail as before; SSL_CERT_FILE makes the mock's
    # certificate the only one the original trusts, so its settings
    # (certificate validation included) and the goldens that show them stay
    # as recorded. `shell` is for comparing on real data: there the
    # original goes out directly, without the environment's proxy
    # variables, as before.
    local network=(-u HTTPS_PROXY -u https_proxy -u HTTP_PROXY -u http_proxy -u ALL_PROXY -u all_proxy)
    if [ "$MOCK_IS_PROXY" = 1 ]; then
        network=(-u ALL_PROXY -u all_proxy -u https_proxy -u http_proxy -u no_proxy -u SSL_CERT_DIR
            HTTPS_PROXY="http://127.0.0.1:$MOCK_PORT" HTTP_PROXY="http://127.0.0.1:$MOCK_PORT"
            NO_PROXY="localhost,127.0.0.1,::1,.invalid" SSL_CERT_FILE="$CACHE/mock-tls.crt")
    fi
    env "${network[@]}" "$CACHE/Sonarr/Sonarr" -nobrowser -data="$data" >"$data/sonarr.stdout.log" 2>&1 &
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
trap 'stop_sonarr; stop_mock' EXIT

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
    # GOLDEN_SETUP names a script that changes the copy before Sonarr
    # starts, for a recording of what the original does at start with
    # another installation (one with no user yet, say). It is run with
    # bash and gets the data directory and the database's file name.
    if [ -n "${GOLDEN_SETUP:-}" ]; then
        log "preparing the copy with $GOLDEN_SETUP"
        bash "$GOLDEN_SETUP" "$data" sonarr.db
    fi
    start_sonarr "$data"
}

cmd_golden() {
    [ "$HAVE_ICU" = 1 ] || die "libicu is needed to record: without it Sonarr sorts and names things differently"
    prepare_dirs
    start_mock
    MOCK_IS_PROXY=1
    start_on_fixture_copy
    # GOLDEN_REQUESTS and GOLDEN_OUT record another request list somewhere
    # else: a probe of the original, or new requests tried on their own
    # before they join the list (bodies are read next to the list).
    # GOLDEN_SIGNALR=1 also listens to the hub and writes signalr.json
    # beside the answers: what the UI was told after each step. For a list
    # of its own (golden-hub); the main list is compared by answers only.
    local requests="${GOLDEN_REQUESTS:-$GOLDEN_DIR/requests.txt}" out="${GOLDEN_OUT:-$GOLDEN_DIR}" hub=()
    [ "${GOLDEN_SIGNALR:-0}" = 1 ] && hub=(--signalr)
    mkdir -p "$out"
    python3 -I "$REPO/scripts/record-golden.py" \
        --base-url "http://127.0.0.1:$SONARR_PORT" --api-key "$API_KEY" --mock-url "http://127.0.0.1:$MOCK_PORT" \
        --requests "$requests" --out "$out" --login unarr:fixture --data-dir "$CACHE/golden-data" "${hub[@]}"
    # SCHEMAS_OUT records the schemas somewhere else too: the recording of
    # a release not pinned yet (scripts/pin-bump.py), which writes nothing
    # in the repository.
    if [ -n "${SCHEMAS_OUT:-}" ] || [ "$out" = "$GOLDEN_DIR" ]; then
        record_schemas
    fi
    stop_sonarr
    stop_mock
    report_refused
    log "golden files written to $out"
}

# Provider and specification schemas: the field metadata (labels, help text,
# select options) the API returns for each implementation. Unarr embeds them
# to render the same `fields` the originals do. Also the UI strings.
record_schemas() {
    local out="${SCHEMAS_OUT:-$REPO/crates/unarr-api/schemas/sonarr}"
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
    prepare_dirs
    start_mock
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
