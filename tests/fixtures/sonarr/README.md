# Sonarr fixture

A `config.xml` and `sonarr.db` from a real Sonarr 4.0.20.3014 installation,
anonymised, plus the responses the real Sonarr gives on that data. The
contract tests import the database into Unarr and replay the same requests
against it.

| File | Origin |
|------|--------|
| `config.xml` | The installation's file passed through `scripts/arr-anonymize.py`: API key `0123456789abcdef0123456789abcdef`, forms authentication (always required), allowed hosts `*`, no trusted networks, telemetry off. |
| `sonarr.db` | The installation's database (migration level 217) passed through the same script: one user `unarr` / `fixture`, every credential, URL, host and e-mail replaced, release ids pseudonymised, root folder `/tv`, raw ffprobe output dropped, commands and provider status emptied. |
| `golden/requests.txt` | The requests replayed, one per line: `name METHOD path`. |
| `golden/*.json` | `{"request", "recordedAt", "status", "body"}` as recorded by `scripts/record-golden.py` against the real Sonarr. `recordedAt` pins Unarr's clock during the replay, so `nextAiring`, statistics and the wanted lists do not drift as the calendar moves on. |
| `golden-hub/` | A second, short request list with its answers and `signalr.json`, what the hub sent after each step: recorded with `GOLDEN_SIGNALR=1` (the list's header has the command), replayed by `the_hub_tells_other_tabs_what_the_original_tells_them`. Steps that need the hub recorded go here, so the long list is not recorded again for them. |
| `golden-audit/` | The routes the audit's triage (`docs/audit/7-triage.md`, "Routes and UI calls") found open: an episode by id under the calendar and the wanted lists, the settings pages and the dictionary by id, the quality size limits, the deprecated language profile's writes, an exclusion by id, the metadata consumers added, tested and deleted. A list of its own, recorded with `GOLDEN_REQUESTS` and `GOLDEN_OUT` (the list's header has the command), replayed by `the_audit_goldens_match`. |
| `golden-blank/` | What Sonarr puts in a database it creates at its first start: the quality definitions, the six quality profiles, the delay profile, the metadata consumers. `setup.sh` removes the fixture's database before the start (`GOLDEN_SETUP`); the list's header has the command. Replayed by `a_blank_start_has_the_originals_defaults`, on a blank database with what Unarr's start ensures. |

`make golden` re-records the responses: it downloads the official linux-x64
release (self-contained, no Docker needed) into `.cache/sonarr/`, pinned by
SHA-256, starts it on a copy of the fixture with every scheduled task marked as
just run (so it neither refreshes series from Skyhook nor cleans house while
recording) and replays `requests.txt`. ICU must be installed: without it .NET
sorts ordinally and leaves language codes untouched, and the goldens would not
describe the Docker images. `make sonarr` starts the real application on a copy
of the fixture for manual comparison.

## Contents

38 series, 1012 episodes, 346 files, about 2000 history rows, 220 blocklist
rows, 10 quality profiles, 40 custom formats (TRaSH-style: streaming services,
language, repack and tier formats), 15 indexers (Newznab and Torznab), 3
download clients (qBittorrent, NZBGet), 3 notifications (Discord, webhook,
Plex), one import list (Simkl), two tags (`ita`, `usenet-only`), two delay
profiles, one custom filter, 16k scene mappings. Series with files:

| Id | Series | Status | Monitored | Profile | Episodes | Files |
|----|--------|--------|-----------|---------|----------|-------|
| 3 | Deadwood (2004) | ended | yes | Any | 53 | 36 |
| 6 | Band of Brothers (2001) | ended | no | HD-1080p - ITA | 15 | 10 |
| 8 | Constellation (2024) | ended | no | HD-1080p | 8 | 8 |
| 14 | Heated Rivalry (2025) | continuing | yes | HD-1080p - ITA | 6 | 6 |
| 17 | Mayans M.C. (2018) | ended | yes | WEB-1080p | 50 | 50 |
| 18 | MobLand (2025) | continuing | yes | HD-1080p - ITA | 20 | 7 |
| 19 | Peacemaker (2022) | ended | yes | WEB-1080p | 30 | 16 |
| 21 | Shōgun (2024) | continuing | yes | WEB-1080p | 34 | 10 |
| 22 | Silicon Valley (2014) | ended | no | WEB-1080p | 58 | 53 |
| 23 | Silo (2023) | continuing | yes | HD-1080p - ITA | 35 | 9 |
| 27 | ted (2024) | ended | yes | WEB-1080p | 17 | 15 |
| 29 | The Orville (2017) | ended | yes | WEB-1080p | 37 | 10 |
| 30 | The Penguin (2024) | ended | no | HD-1080p | 34 | 8 |
| 33 | Yellowjackets (2021) | continuing | yes | WEB-1080p | 39 | 29 |
| 34 | Foundation (2021) | continuing | yes | HD-1080p | 31 | 30 |
| 35 | Sugar (2024) | ended | yes | HD-1080p - ITA | 17 | 16 |
| 40 | Star City (2026) | continuing | yes | HD-1080p | 8 | 8 |
| 41 | Maximum Pleasure Guaranteed (2026) | continuing | yes | HD-1080p - ITA | 10 | 10 |
| 47 | Station Eleven (2021) | ended | yes | HD-1080p - ITA | 10 | 10 |
| 48 | Small Prophets (2026) | continuing | yes | HD-1080p - ITA | 6 | 5 |

The other 18 series (The Pitt is id 1, Daredevil: Born Again id 2) have
episodes but no files; 11 series are continuing with episodes still to air,
which is what makes the pinned clock necessary. Series ids are not contiguous
(removed series), and the fixture has no `MediaCover` images, so the UI shows
placeholders.

The media files themselves are not part of the fixture: Unarr serves what the
database says about them.

## Volatile fields

Some responses depend on the machine or the moment: `system/status`
(`startTime`, `appData`, `osVersion`...), `diskspace`, `rootfolder.freeSpace`,
`log`, `system/task` timings, `signalr` `connectionId`. The contract tests list
these per endpoint and compare the rest.

## Replacing the fixture with another database

Take a backup from the real Sonarr (System > Backup > Backup Now, then
download the zip; it holds a consistent `config.xml` and `sonarr.db`) and run

    scripts/arr-anonymize.py --from ~/Downloads/sonarr_backup_v4.0.20.3014_2026.11.07_10.00.00.zip \
        --out .cache/anonymized --api-key 0123456789abcdef0123456789abcdef

The script re-reads its output and refuses to write if any original value is
still present, naming the table and column where it survives. Release names
are kept (`--scrub-releases` blanks them). The fixture key above is the one
the golden recorder, the recorded requests and the contract tests use. Copy
the two files here, set `AuthenticationRequired` to `Enabled` in `config.xml`
if it is not (the goldens assume the login form is always shown), adjust the
ids in `golden/requests.txt` to ones that exist, then run `make golden`.
