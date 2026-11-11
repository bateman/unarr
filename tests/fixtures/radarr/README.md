# Radarr fixture

A `config.xml` and `radarr.db` from a real Radarr 6.4.4.10685 installation,
anonymised, plus the responses the real Radarr gives on that data. The
contract tests import the database into Unarr and replay the same requests
against it.

| File | Origin |
|------|--------|
| `config.xml` | The installation's file passed through `scripts/arr-anonymize.py --app radarr`: API key `0123456789abcdef0123456789abcdef`, forms authentication (set to always required afterwards, as the goldens assume the login form), allowed hosts `*`, no trusted networks, telemetry off. |
| `radarr.db` | The installation's database (migration level 242) passed through the same script: one user `unarr` / `fixture`, every credential, URL, host and e-mail replaced, release ids pseudonymised, root folder `/movies`, raw ffprobe output dropped, commands and provider status emptied. Movies, credits, collections, translations and release names are public data and kept. |
| `golden/requests.txt` | The requests replayed, one per line: `name METHOD path`. |
| `golden/*.json` | `{"request", "recordedAt", "status", "body"}` as recorded by `scripts/record-golden.py` against the real Radarr. `recordedAt` pins Unarr's clock during the replay, so availability, the calendar and the wanted lists do not drift as the calendar moves on. |

`make golden-radarr` re-records the responses: it downloads the official
linux-core-x64 release (self-contained, no Docker needed) into
`.cache/radarr/`, pinned by SHA-256, starts it on a copy of the fixture with
every scheduled task marked as just run (so it neither refreshes movies and
collections from its metadata server nor syncs the import list while
recording) and replays `requests.txt`. ICU must be installed, as for Sonarr.
`make radarr` starts the real application on a copy of the fixture for manual
comparison.

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
