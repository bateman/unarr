# Radarr fixture

A `config.xml` and `radarr.db` from a real Radarr 6.4.4.10685 installation,
anonymised, plus the responses the real Radarr gives on that data. The
contract tests import the database into Unarr and replay the same requests
against it.

| File | Origin |
|------|--------|
| `config.xml` | The installation's file passed through `scripts/arr-anonymize.py --app radarr`: API key `0123456789abcdef0123456789abcdef`, forms authentication (set to always required afterwards, as the goldens assume the login form), allowed hosts `*`, no trusted networks, telemetry off. |
| `radarr.db` | The installation's database (migration level 242) passed through the same script: one user `unarr` / `fixture`, every credential, URL, host and e-mail replaced, release ids pseudonymised, root folder `/movies`, raw ffprobe output dropped, commands and provider status emptied. Movies, credits, collections, translations and release names are public data and kept. |
| `golden/requests.txt` | The requests replayed, one per line: `name METHOD path [body file]`. `sleep N` pauses the recording while the original's background work runs (the replay moves its clock by as much and waits for its commands to finish); `{id:name}` in a path is the id the answer recorded as `name` carried, or its first item's. |
| `golden/bodies/*.json` | The bodies of the writes. One with `$from` and `$set` is derived from an earlier answer: the resource a `PUT` sends back changed. |
| `golden/*.json` | `{"request", "recordedAt", "status", "body"}` as recorded by `scripts/record-golden.py` against the real Radarr. `recordedAt` pins Unarr's clock during the replay, so availability, the calendar and the wanted lists do not drift as the calendar moves on. |
| `golden-hub/` | A second, short request list with its answers and `signalr.json`, recorded the same way (the list's header has the command) and replayed by `the_hub_and_the_refresh_after_a_scan_are_the_originals`. Steps about the hub or the command list that the long list does not have go here, so the long list is not recorded again for them. |

`make golden-radarr` re-records the responses: it downloads the official
linux-core-x64 release (self-contained, no Docker needed) into
`.cache/radarr/`, pinned by SHA-256, starts it on a copy of the fixture with
every scheduled task marked as just run (so it neither refreshes movies and
collections from its metadata server nor syncs the import list while
recording) and replays `requests.txt`. ICU must be installed, as for Sonarr:
without it the original sorts by code point, and the script refuses to
record. `make radarr` starts the real application on a copy of the fixture
for manual comparison; `RADARR_PORT` moves it off 7878, to run it beside an
Unarr that serves the same fixture (`make http-audit`).

Since milestone 4b the recording also starts `scripts/mock-servarr.py` on
port 18989: two indexers that serve the movie feeds of
`tests/fixtures/indexers/movies` and the four download clients. The original
runs without the environment's proxy and reaches nothing else; the fixture's
own indexers and clients point at `.invalid` hosts. The clients report their
downloads under `/downloads` and the movies live under `/movies`: the script
creates both when missing and empties `/downloads` only when it carries the
marker `.unarr-fixture`, so a folder that is someone's is never touched.

## What the requests do, in order

1. **Reads** (milestone 4a): every endpoint the UI and Bazarr read, the
   errors (404, no key, a wrong key), the calendar feed. Two calendar
   requests cover pairs of movies that share a cinema date, chosen so that
   their TMDB ids, titles and ids disagree on the order.
2. **Provider writes**: indexers and download clients created on the mock
   (ids 20 and 21, 4 to 7), tested, refused when invalid or unreachable.
3. **Search, grab, queue, history**: an interactive search by id (The
   Holdovers) and one that falls back to the title (Nightmare Alley), the
   RSS list, pushes; three grabs, one per kind of client; the queue as the
   UI, Unpackerr and Cleanuparr read it; a removal, a removal with
   blocklist.
4. **Commands**: the fixture's clients and the mock NZBGet disabled, then
   `RssSync` (one grab), `MoviesSearch` (one that grabs, one whose movie is
   queued), `RefreshMonitoredDownloads`, the errors, the task list and the
   health checks after them.
5. **Pending releases**: a delay of a year on the default profile, two
   pushes that wait in the queue, one grabbed from there, one removed and
   blocklisted, the delay taken off again.
6. **Provider updates and deletes**, last, since they remove what the rest
   uses.

The releases the mock lists are made up; the ones meant to be approved are
under 8 GB and one is 4 TB, so that the free space check rejects that one on
any machine and none of the others. The replay therefore needs some 9 GB
free on the disk `/movies` would be on, and says so when it finds less.

## Contents

193 movies (100 with a file, 93 without; 183 released, 3 in cinemas, 7
announced, some with a release still to come, which is what makes the pinned
clock necessary), 284 metadata rows (the others belong to collections and to
the import list), 32 collections, 9,935 credits, 1,550 alternative titles,
6,037 translations, 669 history rows, 86 blocklist rows, 644 download history
rows, 12 quality profiles, 62 custom formats (TRaSH-style), 12 indexers
(Newznab and Torznab), 3 download clients (NZBGet, two qBittorrent), 3
notifications (Discord, webhook, Plex), one import list (IMDb) with 88 list
movies, 21 import exclusions, 5 metadata consumers, two tags (`ita`,
`usenet-only`), two delay profiles, three custom filters, 75 subtitle files,
22 extra files and 156 metadata files. Movie ids are not contiguous (removed
movies), and the fixture has no `MediaCover` images, so the UI shows
placeholders.

The media files themselves are not part of the fixture: Unarr serves what the
database says about them.

## Replacing the fixture with another database

Take a backup from the real Radarr (System > Backup > Backup Now, then
download the zip) and run

    scripts/arr-anonymize.py --app radarr --from ~/Downloads/radarr_backup.zip \
        --out .cache/anonymized --api-key 0123456789abcdef0123456789abcdef

The script re-reads its output and refuses to write if any original value is
still present, naming the table and column where it survives. Copy the two
files here, set `AuthenticationRequired` to `Enabled` in `config.xml`, adjust
the ids in `golden/requests.txt` to ones that exist, then run
`make golden-radarr`.
