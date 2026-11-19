# The fixture as an installation from before the user moved into the
# database leaves it until its first start: no user in the database, the
# user and its password in clear in config.xml. Run by scripts/radarr-fixture.sh before
# Radarr starts (GOLDEN_SETUP), with the data directory and the database's
# file name.
set -euo pipefail
data="$1"
db="$2"
python3 -I -c "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('DELETE FROM Users'); c.commit(); c.close()" "$data/$db"
python3 -I - "$data/config.xml" <<'PY'
import sys
path = sys.argv[1]
text = open(path, encoding="utf-8").read()
text = text.replace("</Config>", "  <Username>Unarr</Username>\n  <Password>fixture</Password>\n</Config>")
open(path, "w", encoding="utf-8").write(text)
PY
