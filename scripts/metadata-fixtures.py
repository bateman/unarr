#!/usr/bin/env python3
"""Keep tests/fixtures/metadata/ small and consistent after a recording.

`scripts/mock-servarr.py --record-metadata` stores whatever the real hosts
answer. Two answers are larger than the tests need and are cut down to the
series the fixtures know (the shows recorded under
skyhook.sonarr.tv/v1/tvdb/shows/en/):

- services.sonarr.tv/v1/scenemapping (about 12,000 mappings, 2 MB): only
  the mappings of those series;
- services.sonarr.tv/v1/dailyseries: kept whole (a few hundred ids).

XEM (thexem.info) refuses connections from data-centre networks, so its
answers cannot be recorded from a cloud session; `xem` writes hand-made
answers in the shape of its public API (`havemap`, `all`, `allNames`) for
the series listed in XEM_SERIES below, which is what the originals' XemProxy
parses. Replace them with real recordings when one is available.

    python3 -I scripts/metadata-fixtures.py trim
    python3 -I scripts/metadata-fixtures.py xem
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent / "tests" / "fixtures" / "metadata"
SHOWS = ROOT / "skyhook.sonarr.tv" / "v1" / "tvdb" / "shows" / "en"
XEM = ROOT / "thexem.info" / "map"

# Series XEM knows about in the hand-made answers: Frieren (anime, absolute
# numbering), whose second cour scene groups release as season 2.
XEM_SERIES = {
    424536: {
        "names": [{"Sousou no Frieren": -1}, {"Frieren Beyond Journey's End": -1}, {"Sousou no Frieren 2nd Season": 2}],
        # tvdb season 1 episodes 15-28 are scene season 2 episodes 1-14
        "split": (1, 15, 2),
    },
}


def write(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    status = path.with_suffix(".status")
    if status.exists():
        status.unlink()


def recorded_tvdb_ids() -> set[int]:
    return {int(p.stem) for p in SHOWS.glob("*.json") if not p.with_suffix(".status").exists()}


def cmd_trim() -> None:
    ids = recorded_tvdb_ids()
    path = ROOT / "services.sonarr.tv" / "v1" / "scenemapping.json"
    mappings = json.loads(path.read_text(encoding="utf-8"))
    kept = [m for m in mappings if m.get("tvdbId") in ids]
    write(path, kept)
    print(f"scenemapping: kept {len(kept)} of {len(mappings)} mappings for {len(ids)} recorded series")


def cmd_xem() -> None:
    write(XEM / "havemap" / "origin-tvdb.json", {"result": "success", "data": [str(i) for i in XEM_SERIES], "message": ""})
    write(
        XEM / "allNames" / "seasonnumbers-true-origin-tvdb.json",
        {"result": "success", "data": {str(i): s["names"] for i, s in XEM_SERIES.items()}, "message": ""},
    )
    for tvdb_id, spec in XEM_SERIES.items():
        show = json.loads((SHOWS / f"{tvdb_id}.json").read_text(encoding="utf-8"))
        season, first, scene_season = spec["split"]
        data = []
        for ep in show["episodes"]:
            if ep["seasonNumber"] != season:
                continue
            tvdb = {"season": ep["seasonNumber"], "episode": ep["episodeNumber"], "absolute": ep.get("absoluteEpisodeNumber") or 0}
            if ep["episodeNumber"] >= first:
                scene = {"season": scene_season, "episode": ep["episodeNumber"] - first + 1, "absolute": tvdb["absolute"]}
            else:
                scene = dict(tvdb)
            data.append({"scene": scene, "tvdb": tvdb})
        write(XEM / "all" / f"id-{tvdb_id}-origin-tvdb.json", {"result": "success", "data": data, "message": ""})
    print(f"xem: hand-made answers for {sorted(XEM_SERIES)}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("command", choices=("trim", "xem"))
    args = ap.parse_args()
    {"trim": cmd_trim, "xem": cmd_xem}[args.command]()


if __name__ == "__main__":
    main()
