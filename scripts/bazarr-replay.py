#!/usr/bin/env python3
"""Replay the requests Bazarr makes to a Sonarr instance, against Unarr.

Bazarr (github.com/morpheus65535/bazarr, bazarr/sonarr/sync/*.py) reads the
version from system/status, then the language profiles, tags, every series,
and per series the episodes with their files; it keeps a handful of fields
from each. This script issues the same requests, with the API key in the
query string as Bazarr does, and checks that every field Bazarr touches is
present with the expected type. It is a stand-in for running Bazarr itself.

    scripts/bazarr-replay.py --base-url http://127.0.0.1:8989 --api-key KEY
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.request


def get(base: str, path: str, key: str):
    sep = "&" if "?" in path else "?"
    with urllib.request.urlopen(f"{base}/api/v3/{path}{sep}apikey={key}", timeout=60) as r:
        return json.loads(r.read())


def expect(obj: dict, field: str, kinds: tuple, where: str, problems: list):
    if field not in obj:
        problems.append(f"{where}: missing {field!r}")
    elif not isinstance(obj[field], kinds):
        problems.append(f"{where}: {field!r} is {type(obj[field]).__name__}, expected {kinds}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--api-key", required=True)
    args = ap.parse_args()
    base, key = args.base_url.rstrip("/"), args.api_key
    problems: list[str] = []

    status = get(base, "system/status", key)
    version = status.get("version", "")
    print(f"[bazarr] system/status version {version!r}")
    if version.startswith(("0.", "2.")):
        problems.append("version looks like Sonarr v2: Bazarr would use the legacy API")

    # Sonarr v3+: Bazarr still asks for the (deprecated) language profiles.
    profiles = get(base, "languageprofile", key)
    for p in profiles:
        expect(p, "id", (int,), "languageprofile", problems)
        expect(p, "name", (str,), "languageprofile", problems)
    tags = get(base, "tag", key)
    for t in tags:
        expect(t, "id", (int,), "tag", problems)
        expect(t, "label", (str,), "tag", problems)
    root_folders = get(base, "rootfolder", key)
    for r in root_folders:
        expect(r, "path", (str,), "rootfolder", problems)

    series = get(base, "series", key)
    print(f"[bazarr] {len(series)} series, {len(tags)} tags, {len(profiles)} language profiles, {len(root_folders)} root folders")
    episodes_seen = files_seen = 0
    for show in series:
        where = f"series {show.get('id')}"
        for field, kinds in [
            ("id", (int,)), ("title", (str,)), ("path", (str,)), ("tvdbId", (int,)), ("tags", (list,)),
            ("alternateTitles", (list,)), ("sortTitle", (str,)), ("year", (int,)), ("seriesType", (str,)),
            ("monitored", (bool,)), ("qualityProfileId", (int,)), ("languageProfileId", (int,)),
            ("images", (list,)), ("ended", (bool,)), ("originalLanguage", (dict,)),
        ]:
            expect(show, field, kinds, where, problems)
        # Optional in the originals too (absent when null).
        for field, kinds in [("imdbId", (str,)), ("overview", (str,)), ("lastAired", (str,))]:
            if field in show:
                expect(show, field, kinds, where, problems)

        one = get(base, f"series/{show['id']}", key)
        if one.get("id") != show["id"]:
            problems.append(f"{where}: series/{{id}} returned id {one.get('id')}")

        episodes = get(base, f"episode?seriesId={show['id']}&includeEpisodeFile=true", key)
        files = get(base, f"episodeFile?seriesId={show['id']}", key)
        files_by_id = {f["id"]: f for f in files}
        episodes_seen += len(episodes)
        files_seen += len(files)
        for ep in episodes:
            ew = f"{where} episode {ep.get('id')}"
            for field, kinds in [
                ("id", (int,)), ("seriesId", (int,)), ("seasonNumber", (int,)), ("episodeNumber", (int,)),
                ("monitored", (bool,)), ("hasFile", (bool,)), ("episodeFileId", (int,)),
            ]:
                expect(ep, field, kinds, ew, problems)
            if "title" in ep:
                expect(ep, "title", (str,), ew, problems)
            if ep.get("hasFile"):
                if "episodeFile" not in ep:
                    problems.append(f"{ew}: hasFile but no episodeFile embedded")
                    continue
                f = ep["episodeFile"]
                for field, kinds in [("id", (int,)), ("path", (str,)), ("size", (int,)), ("languages", (list,)), ("quality", (dict,))]:
                    expect(f, field, kinds, ew + " file", problems)
                if "mediaInfo" in f:
                    for field in ("videoCodec", "audioCodec"):
                        expect(f["mediaInfo"], field, (str,), ew + " mediaInfo", problems)
                if "quality" in f and "-" not in f["quality"]["quality"]["name"] and f["quality"]["quality"]["name"] not in ("Unknown", "SDTV", "DVD", "Raw-HD"):
                    problems.append(f"{ew}: quality name {f['quality']['quality']['name']!r} is not NAME-RES")
                if f["id"] not in files_by_id:
                    problems.append(f"{ew}: file {f['id']} not in episodeFile?seriesId")
                elif files_by_id[f["id"]]["path"] != f["path"]:
                    problems.append(f"{ew}: episodeFile path differs between the two endpoints")
                single = get(base, f"episodeFile/{f['id']}", key)
                if single.get("path") != f["path"]:
                    problems.append(f"{ew}: episodeFile/{{id}} path differs")
                # Bazarr looks the grab up in history to learn the release.
                history = get(base, f"history?eventType=1&episodeId={ep['id']}", key)
                expect(history, "records", (list,), ew + " history", problems)

    print(f"[bazarr] {episodes_seen} episodes, {files_seen} files checked")
    if problems:
        print("[bazarr] PROBLEMS:")
        for p in problems[:40]:
            print("  " + p)
        sys.exit(1)
    print("[bazarr] every field Bazarr reads is present with the expected type")


if __name__ == "__main__":
    main()
