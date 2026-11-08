#!/usr/bin/env python3
"""Grow the anonymised Sonarr fixture to measure Unarr at the design targets.

Copies ``tests/fixtures/sonarr`` and multiplies its series (with their
episodes, files and history) ``--factor`` times, each copy with new ids,
TVDB ids, slugs and paths, then pads the history up to ``--history`` rows by
repeating grab events. The result is a legacy ``sonarr.db`` the importer
reads like any other, next to the fixture's ``config.xml``::

    scripts/scale-fixture.py --factor 25 --history 50000 --out .cache/scaled
    target/release/unarrd --data .cache/scaled/unarr --import-from .cache/scaled

The default factor gives 950 series, the library size the design targets
name; the default history gives the 50,000 rows the phase 1 review measured
``history/since`` on.
"""

from __future__ import annotations

import argparse
import shutil
import sqlite3
from pathlib import Path

FIXTURE = Path(__file__).resolve().parent.parent / "tests" / "fixtures" / "sonarr"


def columns(conn: sqlite3.Connection, table: str) -> list[str]:
    return [r[1] for r in conn.execute(f"PRAGMA table_info({table})")]


def copy_rows(conn: sqlite3.Connection, table: str, rows: list[dict]) -> None:
    if not rows:
        return
    names = list(rows[0].keys())
    sql = f"INSERT INTO {table} ({', '.join(names)}) VALUES ({', '.join('?' for _ in names)})"
    conn.executemany(sql, [[r[n] for n in names] for r in rows])


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--factor", type=int, default=25, help="how many copies of the series to hold (default 25)")
    ap.add_argument("--history", type=int, default=50_000, help="history rows to reach (default 50000)")
    ap.add_argument("--out", type=Path, required=True, help="directory to write sonarr.db and config.xml into")
    args = ap.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    db = args.out / "sonarr.db"
    shutil.copy(FIXTURE / "sonarr.db", db)
    shutil.copy(FIXTURE / "config.xml", args.out / "config.xml")
    conn = sqlite3.connect(db)
    conn.row_factory = sqlite3.Row

    series = [dict(r) for r in conn.execute("SELECT * FROM Series ORDER BY Id")]
    episodes = [dict(r) for r in conn.execute("SELECT * FROM Episodes ORDER BY Id")]
    files = [dict(r) for r in conn.execute("SELECT * FROM EpisodeFiles ORDER BY Id")]
    history = [dict(r) for r in conn.execute("SELECT * FROM History ORDER BY Id")]
    series_step = 1000
    episode_step = 100_000
    file_step = 100_000
    history_step = 100_000
    tvdb_step = 10_000_000

    for k in range(1, args.factor):
        series_map = {s["Id"]: s["Id"] + k * series_step for s in series}
        file_map = {f["Id"]: f["Id"] + k * file_step for f in files}
        new_series = []
        for s in series:
            n = dict(s)
            n["Id"] = series_map[s["Id"]]
            n["TvdbId"] = s["TvdbId"] + k * tvdb_step
            n["TvMazeId"] = (s["TvMazeId"] or 0) + k * tvdb_step
            n["TmdbId"] = (s["TmdbId"] or 0) + k * tvdb_step
            n["TvRageId"] = (s["TvRageId"] or 0) + k * tvdb_step
            n["ImdbId"] = None
            n["Title"] = f"{s['Title']} ({k})"
            n["SortTitle"] = f"{s['SortTitle']} ({k})"
            n["CleanTitle"] = f"{s['CleanTitle']}{k}"
            n["TitleSlug"] = f"{s['TitleSlug']}-{k}"
            n["Path"] = f"{s['Path']} ({k})"
            new_series.append(n)
        copy_rows(conn, "Series", new_series)
        new_episodes = []
        for e in episodes:
            n = dict(e)
            n["Id"] = e["Id"] + k * episode_step
            n["SeriesId"] = series_map[e["SeriesId"]]
            n["EpisodeFileId"] = file_map.get(e["EpisodeFileId"], 0)
            if e["TvdbId"]:
                n["TvdbId"] = e["TvdbId"] + k * tvdb_step
            new_episodes.append(n)
        copy_rows(conn, "Episodes", new_episodes)
        new_files = []
        for f in files:
            n = dict(f)
            n["Id"] = file_map[f["Id"]]
            n["SeriesId"] = series_map[f["SeriesId"]]
            new_files.append(n)
        copy_rows(conn, "EpisodeFiles", new_files)
        new_history = []
        for h in history:
            n = dict(h)
            n["Id"] = h["Id"] + k * history_step
            n["SeriesId"] = series_map[h["SeriesId"]]
            n["EpisodeId"] = h["EpisodeId"] + k * episode_step
            new_history.append(n)
        copy_rows(conn, "History", new_history)

    # Pad the history with more copies of the grabs, spread over the ids above.
    total = conn.execute("SELECT count(*) FROM History").fetchone()[0]
    next_id = conn.execute("SELECT max(Id) FROM History").fetchone()[0] + 1
    all_history = [dict(r) for r in conn.execute("SELECT * FROM History ORDER BY Id")]
    padding = []
    i = 0
    while total + len(padding) < args.history:
        h = all_history[i % len(all_history)]
        n = dict(h)
        n["Id"] = next_id + len(padding)
        padding.append(n)
        i += 1
    copy_rows(conn, "History", padding)
    conn.commit()
    counts = {t: conn.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in ("Series", "Episodes", "EpisodeFiles", "History")}
    conn.execute("VACUUM")
    conn.close()
    print(f"wrote {db}: {counts}")


if __name__ == "__main__":
    main()
