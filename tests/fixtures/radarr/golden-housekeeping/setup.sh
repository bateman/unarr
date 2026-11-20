# The fixture with what the housekeeping repairs: a second user, and the
# health check's last run a day ahead, as a clock that went back leaves
# it. Run by scripts/radarr-fixture.sh before Radarr starts (GOLDEN_SETUP),
# with the data directory and the database's file name.
set -euo pipefail
python3 -I - "$1/$2" <<'PY'
import datetime, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
c.execute("INSERT INTO Users (Identifier, Username, Password) VALUES ('5f0c6a3e-0000-4000-8000-000000000002', 'second', 'x')")
ahead = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=1)
c.execute("UPDATE ScheduledTasks SET LastExecution = ? WHERE TypeName LIKE '%.CheckHealthCommand'", (ahead.strftime('%Y-%m-%d %H:%M:%SZ'),))
c.commit()
c.close()
PY
