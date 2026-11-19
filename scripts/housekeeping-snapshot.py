#!/usr/bin/env python3
"""What the housekeeping left in an original's database, for the goldens.

    scripts/housekeeping-snapshot.py <database> <out.json>

Writes the users (their names) and the scheduled tasks (type, interval,
and whether the last run is still ahead of the moment the snapshot is
taken, right after the recording). Read only; the database is the
recording's copy.
"""

import datetime
import json
import sqlite3
import sys


def main() -> None:
    db, out = sys.argv[1], sys.argv[2]
    now = datetime.datetime.now(datetime.timezone.utc)
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    users = [r[0] for r in c.execute('SELECT "Username" FROM "Users" ORDER BY "Id"')]
    tasks = []
    for type_name, interval, last in c.execute(
        'SELECT "TypeName", "Interval", "LastExecution" FROM "ScheduledTasks" ORDER BY "TypeName"'
    ):
        at = datetime.datetime.fromisoformat(last.replace("Z", "+00:00"))
        if at.tzinfo is None:
            at = at.replace(tzinfo=datetime.timezone.utc)
        tasks.append({"typeName": type_name, "interval": interval, "inTheFuture": at > now})
    json.dump({"users": users, "tasks": tasks}, open(out, "w"), indent=1, sort_keys=True)
    open(out, "a").write("\n")


if __name__ == "__main__":
    main()
