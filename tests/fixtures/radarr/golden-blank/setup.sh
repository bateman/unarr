# An installation started for the first time: no database, only the
# fixture's config.xml (its API key, forms authentication). Run by
# scripts/radarr-fixture.sh before Radarr starts (GOLDEN_SETUP), with the
# data directory and the database's file name.
set -euo pipefail
data="$1"
db="$2"
rm -f "$data/$db" "$data/$db-wal" "$data/$db-shm"
