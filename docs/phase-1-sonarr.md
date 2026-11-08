# Phase 1: Sonarr, read-only

Work order for the first implementation phase. Read `docs/DESIGN.md` first: it
holds the reasoning, the architecture and the decisions already taken.

## Goal

Run the official Sonarr v4 web UI and Bazarr against Unarr, on data imported
from a real Sonarr installation, without either noticing the difference.

## Scope

1. **Importer** (`unarr-import`)
   - Parse `config.xml`: port, URL base, API key, authentication method,
     instance name. The API key is what every connected client already has.
   - Read `sonarr.db`: check `VersionInfo` for the migration level, then map
     series, seasons, episodes, episode files, quality profiles, quality
     definitions, custom formats, root folders, tags, languages, naming config,
     media management config, history. Log what is skipped.
   - Write into Unarr's own SQLite schema (one file per personality, WAL,
     `rusqlite`, dedicated thread). The schema is Unarr's, not a mirror.
2. **Read endpoints** under `/api/v3`, with the original JSON shapes
   (camelCase, same field names, same nesting): `system/status`,
   `health`, `series`, `series/{id}`, `episode?seriesId=`, `episode/{id}`,
   `episodefile?seriesId=`, `qualityprofile`, `qualitydefinition`,
   `customformat`, `rootfolder`, `tag`, `language`, `config/naming`,
   `config/mediamanagement`, `config/host`, `config/ui`, `history`,
   `queue`, `calendar`, `wanted/missing`, `wanted/cutoff`,
   `indexer`, `downloadclient`, `notification`, `importlist`, `diskspace`,
   `system/task`, `log`, `update`. Empty collections are fine where Unarr has
   no data yet; wrong shapes are not.
3. **Authentication** as the originals: `X-Api-Key` header or `apikey` query
   parameter for the API; `/initialize.json` and `/login` for the UI.
4. **SignalR** minimal: `POST /signalr/messages/negotiate`, then the JSON hub
   protocol over WebSocket, enough for the UI to connect and stay connected.
5. **Frontend**: serve the official Sonarr v4 frontend (GPL-3.0, as Unarr)
   embedded in the binary with `rust-embed`, with `/initialize.json` filled in
   from config.
6. **Contract tests**: golden files recorded from a real Sonarr v4 against a
   fixture database, replayed against Unarr on the imported copy, diffed as
   JSON. The fixture and the recorded responses live under `tests/fixtures/`.

Out of scope in this phase: searching, downloading, importing files, renaming,
notifications, Radarr, Lidarr.

## Exit criteria

- `make check` green.
- The official Sonarr UI loads, lists series, opens a series page, shows the
  calendar and the settings pages, against Unarr on port 8989.
- Bazarr connects to Unarr as a Sonarr instance and lists series and episodes.
- Contract tests pass on the read endpoints above.
- Idle RSS measured and recorded in `docs/DESIGN.md` section 9.

## Status, November 2026

| Exit criterion | Status |
|----------------|--------|
| `make check` green | Yes: fmt, clippy `-D warnings`, 67 tests including the golden replay. |
| Official UI loads, lists series, series page, calendar, settings, on 8989 | Yes, with Playwright against the release binary on the real fixture: login, series index, series pages, calendar, history, queue, wanted, every settings page, system status, tasks and events. Posters are placeholders because the fixture has no `MediaCover`. |
| Bazarr connects and lists series and episodes | Verified by `scripts/bazarr-replay.py` (`make bazarr`), which issues Bazarr's requests and checks every field it reads, on 1,012 episodes and 346 files; Bazarr itself was not run in the session. |
| Contract tests pass on the read endpoints | 89 goldens recorded from the real Sonarr on an anonymised real database, none pending. |
| Idle RSS recorded in `docs/DESIGN.md` section 9 | Yes, for both fixtures. |

The fixture was first a synthetic database built by driving Sonarr's API, then
replaced by the maintainer's own installation anonymised with
`scripts/sonarr-anonymize.py`. The second brought 13 golden mismatches and a
two-second endpoint; all fixed, see `docs/DESIGN.md` sections 6, 9 and 11.

An independent review of the branch then listed twenty findings (ten to fix
before merging: CI without the UI, the image's `/config` owned by root, orphan
rows stopping the import, a URL base with capitals, the `Oidc` mode opening
everything, deep links skipping the login, an open redirect through
`returnUrl`, unknown providers showing secrets, `BindAddress` ignored, no
SIGTERM handling; nine should-fix on scale, consistency and tests; one nice to
have). All twenty are addressed; `docs/DESIGN.md` section 11 records the
decisions. A second pass found eight more (an unenforced `AllowedHosts`, a
tab in `returnUrl`, a silent blank start on a wrong `--import-from`, two
stalls, three cleanups), also addressed and recorded there. Two of the reviewer's measurements stand as targets for phase 2:
a full `history/since` on 50,000 rows builds a 48 MB response and the
process keeps the memory afterwards, and `/series` at 950 series takes 59 ms.

Deviations from the scope above, all deliberate: `queue` is served empty (no
downloads yet); `indexer`, `downloadclient`, `notification`, `importlist`
return the imported providers with their field schemas, read-only; `log`,
`system/task`, `update`, `diskspace` match the shape, not the values.

## Fixture database

Ask the user first for an anonymised `sonarr.db` and `config.xml`. If none is
available, build the fixture from the Sonarr source: the migrations under
`src/NzbDrone.Core/Datastore/Migration/` in github.com/Sonarr/Sonarr define the
schema; populate a handful of series with synthetic data. A Docker daemon may
not be available in the cloud session, so do not depend on running Sonarr in a
container; if one is available, prefer recording golden files from the real
application.

*Outcome:* the user provided a backup of their installation; it is the fixture
now, anonymised by `scripts/sonarr-anonymize.py` (see
`tests/fixtures/sonarr/README.md`). The synthetic generator was removed once it
had served its purpose: left in the tree it could overwrite the real fixture.
The official release runs without Docker, so the goldens are recorded from it.

*Closing check, from `main` after the merge:* one squashed commit, `make ui
&& make check` green (67 tests), CI green including the multi-arch image,
12.4 MiB release binary, 25.4 MB RSS after importing the fixture and 32.9 MB
after 300 requests to `/series`; every write endpoint answers 405 and
`release` 404, as expected before phase 2. The next phase is
`docs/phase-2-acquisition.md`.

## Conventions

- Rust 2021, `make check` before every push (fmt, clippy `-D warnings`, tests).
- Crate names and identifiers are `unarr`; prose says Unarr.
- Commits: imperative subject, body explains why. No model identifiers.
- The binary is `unarrd`; the image is `ghcr.io/bateman/unarr`.
- Keep `docs/DESIGN.md` current when a decision changes.
