#!/usr/bin/env python3
"""Grow an anonymised fixture to measure Unarr at the design targets.

Copies ``tests/fixtures/sonarr`` and multiplies its series (with their
episodes, files and history) ``--factor`` times, each copy with new ids,
TVDB ids, slugs and paths, then pads the history up to ``--history`` rows by
repeating grab events. The result is a legacy ``sonarr.db`` the importer
reads like any other, next to the fixture's ``config.xml``::

    scripts/scale-fixture.py --factor 25 --history 50000 --out .cache/scaled/sonarr
    target/release/unarrd --data .cache/scaled/unarr --import-from .cache/scaled/sonarr

The default factor gives 950 series, the library size the design targets
name; the default history gives the 50,000 rows the phase 1 review measured
``history/since`` on.

``--app radarr`` does the same to ``tests/fixtures/radarr``: the movies with
their metadata, files, alternative titles, translations, credits and
history, each copy with new ids, TMDB ids and paths. Its default factor
gives 2,123 movies, past the 2,000 the targets name::

    scripts/scale-fixture.py --app radarr --out .cache/scaled/radarr

``make scale-fixture`` writes both, under those two directories.
"""

from __future__ import annotations

import argparse
import shutil
import sqlite3
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent.parent / "tests" / "fixtures"
FIXTURE = FIXTURES / "sonarr"


def columns(conn: sqlite3.Connection, table: str) -> list[str]:
    return [r[1] for r in conn.execute(f"PRAGMA table_info({table})")]


def copy_rows(conn: sqlite3.Connection, table: str, rows: list[dict]) -> None:
    if not rows:
        return
    names = list(rows[0].keys())
    quoted = ", ".join(f'"{n}"' for n in names)  # Credits has a column called Order
    sql = f"INSERT INTO {table} ({quoted}) VALUES ({', '.join('?' for _ in names)})"
    conn.executemany(sql, [[r[n] for n in names] for r in rows])


def pad_history(conn: sqlite3.Connection, wanted: int) -> None:
    """Repeat the history rows there are, under new ids, up to `wanted`."""
    total = conn.execute("SELECT count(*) FROM History").fetchone()[0]
    next_id = conn.execute("SELECT max(Id) FROM History").fetchone()[0] + 1
    all_history = [dict(r) for r in conn.execute("SELECT * FROM History ORDER BY Id")]
    padding = []
    i = 0
    while total + len(padding) < wanted:
        n = dict(all_history[i % len(all_history)])
        n["Id"] = next_id + len(padding)
        padding.append(n)
        i += 1
    copy_rows(conn, "History", padding)


def scale_radarr(args) -> None:
    fixture = FIXTURES / "radarr"
    args.out.mkdir(parents=True, exist_ok=True)
    db = args.out / "radarr.db"
    shutil.copy(fixture / "radarr.db", db)
    shutil.copy(fixture / "config.xml", args.out / "config.xml")
    conn = sqlite3.connect(db)
    conn.row_factory = sqlite3.Row

    def rows(sql: str) -> list[dict]:
        return [dict(r) for r in conn.execute(sql)]

    movies = rows("SELECT * FROM Movies ORDER BY Id")
    # Only the metadata movies point at: the rest belongs to list items and
    # collections, which a larger library does not multiply.
    metadata = rows("SELECT * FROM MovieMetadata WHERE Id IN (SELECT MovieMetadataId FROM Movies) ORDER BY Id")
    owned = "(SELECT MovieMetadataId FROM Movies)"
    files = rows("SELECT * FROM MovieFiles ORDER BY Id")
    titles = rows(f"SELECT * FROM AlternativeTitles WHERE MovieMetadataId IN {owned} ORDER BY Id")
    translations = rows(f"SELECT * FROM MovieTranslations WHERE MovieMetadataId IN {owned} ORDER BY Id")
    credits = rows(f"SELECT * FROM Credits WHERE MovieMetadataId IN {owned} ORDER BY Id")
    history = rows("SELECT * FROM History ORDER BY Id")
    step = 1_000_000
    tmdb_step = 10_000_000

    for k in range(1, args.factor):
        new_metadata = []
        for m in metadata:
            n = dict(m)
            n["Id"] = m["Id"] + k * step
            n["TmdbId"] = m["TmdbId"] + k * tmdb_step
            n["ImdbId"] = None
            n["Title"] = f"{m['Title']} ({k})"
            n["SortTitle"] = f"{m['SortTitle']} ({k})"
            n["CleanTitle"] = f"{m['CleanTitle']}{k}"
            # One collection stays one collection, however many movies.
            new_metadata.append(n)
        copy_rows(conn, "MovieMetadata", new_metadata)
        new_movies = []
        for m in movies:
            n = dict(m)
            n["Id"] = m["Id"] + k * step
            n["MovieMetadataId"] = m["MovieMetadataId"] + k * step
            n["MovieFileId"] = m["MovieFileId"] + k * step if m["MovieFileId"] else m["MovieFileId"]
            n["Path"] = f"{m['Path']} ({k})"
            new_movies.append(n)
        copy_rows(conn, "Movies", new_movies)
        new_files = []
        for f in files:
            n = dict(f)
            n["Id"] = f["Id"] + k * step
            n["MovieId"] = f["MovieId"] + k * step
            new_files.append(n)
        copy_rows(conn, "MovieFiles", new_files)
        for table, originals in (("AlternativeTitles", titles), ("MovieTranslations", translations), ("Credits", credits)):
            copies = []
            for r in originals:
                n = dict(r)
                n["Id"] = r["Id"] + k * step
                n["MovieMetadataId"] = r["MovieMetadataId"] + k * step
                if table == "AlternativeTitles":
                    n["CleanTitle"] = f"{r['CleanTitle']}copy{k}"  # unique in the table
                if table == "Credits":
                    n["CreditTmdbId"] = f"{r['CreditTmdbId']}-{k}"
                copies.append(n)
            copy_rows(conn, table, copies)
        new_history = []
        for h in history:
            n = dict(h)
            n["Id"] = h["Id"] + k * step
            n["MovieId"] = h["MovieId"] + k * step
            new_history.append(n)
        copy_rows(conn, "History", new_history)

    pad_history(conn, args.history)
    conn.commit()
    tables = ("Movies", "MovieMetadata", "MovieFiles", "AlternativeTitles", "MovieTranslations", "Credits", "History")
    counts = {t: conn.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in tables}
    conn.execute("VACUUM")
    conn.close()
    print(f"wrote {db}: {counts}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--app", choices=("sonarr", "radarr"), default="sonarr", help="which fixture to grow (default sonarr)")
    ap.add_argument("--factor", type=int, help="how many copies of the library to hold (default 25 for sonarr, 11 for radarr)")
    ap.add_argument("--history", type=int, default=50_000, help="history rows to reach (default 50000)")
    ap.add_argument("--out", type=Path, required=True, help="directory to write the database and config.xml into")
    args = ap.parse_args()
    if args.factor is None:
        args.factor = 11 if args.app == "radarr" else 25
    if args.app == "radarr":
        scale_radarr(args)
        return

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
    pad_history(conn, args.history)
    conn.commit()
    counts = {t: conn.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in ("Series", "Episodes", "EpisodeFiles", "History")}
    conn.execute("VACUUM")
    conn.close()
    print(f"wrote {db}: {counts}")


if __name__ == "__main__":
    main()
