# Audit 8 — Radarr: routes, settings, commands and the server outside the bodies

November 2026, at the end of milestone 4d. Radarr tree: tag `v6.4.4.10685`,
checked out under `.cache/src/Radarr` and read, never compiled. Unarr: the
branch `radarr/phase-4d-library`.

The six audits of `README.md` were made against Sonarr. This one applies the
same method to Radarr, where the method could be run by machine:

| Part | How | Examined | Gaps |
|------|-----|----------|------|
| Routes the original declares | `scripts/route-audit.py` | 259 (method, path) rows | 65 before, 31 after, all low |
| Requests the web UI makes | the UI bundle's paths against the router | 79 API paths | 2 before (restart, shutdown), 0 after |
| Settings stored but not applied | each key of `config_writes.rs` against its readers | 15 settings | 1 before (the finished-download interval), 0 after |
| Commands the original has | its `*Command` classes against `CommandBody` | 35 commands | 7 before, 0 after |
| The server outside the API bodies | `make http-audit`, both running on one fixture | 324 requests | 7 before, 3 after, all artefacts of the run |

The UI at several screen densities (audit 5) and the event handlers beyond
the commands (audit 6) were not repeated: Radarr's pages are served by its
own bundle, which the goldens of 4a already exercise, and its handlers were
ported next to Sonarr's in each milestone.

## What was found, and what was done

Every fix below was recorded from the real Radarr first, into
`tests/fixtures/radarr/golden-audit/`, and is replayed by
`the_audit_goldens_match`.

1. **`POST /system/restart` and `POST /system/shutdown` were not routed**
   (the header menu's two actions, and the restart a backup restore asks
   for). Medium: the UI's buttons failed. Fixed, with the originals'
   answers (`{"restarting": true}`, `{"shuttingDown": true}`).
2. **Settings > Metadata could not be saved**: `PUT /metadata/{id}` was
   not routed for Radarr, nor `POST /metadata/test` and `/testall`.
   Medium, the same finding Sonarr's audit numbered 11. Fixed; a name
   another consumer has is the 400 the original's shared rules answer,
   before it reads the provider.
3. **Six commands the UI posts were refused** (`ClearLog`,
   `DeleteLogFiles`, `DeleteUpdateLogFiles`, `ResetApiKey`,
   `ClearBlocklist`, `ApplicationCheckUpdate`): Radarr's personality took
   none of them, and the host hooks that act on the log and the API key
   were installed for Sonarr only. Medium, Sonarr's finding 10 again.
   Fixed. Radarr spells the update check `ApplicationCheckUpdate` where
   Sonarr has `ApplicationUpdateCheck`; both names are now taken and
   answered as each application spells them.
4. **`RenameMovieFolder`** is a command Radarr's API takes and no service
   of its runs: it is accepted, kept out of the client's updates (its
   class declares the property read-only, which the controller cannot
   force on) and fails with the exception the original raises. Low, but
   the movie editor posts it.
5. **`GET /movie/{id}/folder`** was missing: the folder the naming
   settings would give a movie, which the editor reads before it moves
   one. Low. Fixed.
6. **`POST /importlist/movie`** was missing: the Discover page adds the
   movies its lists named, the same way the library import does. Low (the
   maintainer does not use Discover). Fixed, and the page's own listing
   (`GET /importlist/movie`) came with milestone 4d.
7. **The settings pages' reads by id** (`GET /config/host/{id}` and the
   other six) answered 405. Low: the UI reads them without the id. Fixed.
8. **`CheckForFinishedDownloadInterval` was stored and not applied**: the
   refresh of the download clients ran every minute whatever the setting
   said. Low (the default is a minute). Fixed for both personalities,
   with the originals' floor of one minute.
9. **A filter the query cannot bind answered `application/problem+json`**
   where Radarr answers plain JSON. Low. Fixed, measured against the
   running original: the body of a paged list's filter error is the same,
   the content type is not.
10. **Basic authentication was still served.** Radarr 6.4.4 has none: it
    writes `forms` over `Basic` in `config.xml` at start, says `forms` in
    `config/host` and sends the browser to its login page, where Unarr's
    Radarr challenged for Basic credentials. Medium: an installation
    moved from an older Radarr would have kept a login the original no
    longer offers. Found on 19 November 2026, while the HTTP rows of
    `7-triage.md` were recorded on both originals; fixed (`Basic` is
    taken for Forms, at start and after a save), test
    `tests/http.rs` `radarr_takes_basic_for_forms`.

## Open, and why

- **The id-less `DELETE` forms** (`DELETE /tag`, `DELETE /movie`, and
  eighteen more). `[RestDeleteById]` is `{id:int?}`, so the original
  routes them and deletes the row numbered zero, which answers 404;
  Unarr answers 405. Low, and the same on Sonarr's side.
- **`GET /movie/import`, `GET /movie/import/{id}`, `GET /movie/lookup/{id}`**:
  actions the original inherits and answers with
  `NotImplementedException`. Harmless, as Sonarr's audit said of its seven.
- **`GET /system/routes` and `/system/routes/duplicate`**: a listing of
  the routes the framework knows, for debugging. Out of scope, as in
  Sonarr's audit.
- **The metadata consumers' create, delete, bulk and action routes**:
  Radarr's consumers are fixed rows the UI only edits, and the metadata
  writers are out of this phase's scope (`docs/phase-4-radarr.md`).
- **A paging or filter parameter that is not a number** (`?page=x`,
  `?tmdbId=x`) is ignored where the original refuses the request with a
  400. Low, true of both personalities and of every paged route; no
  golden records it, and the UI never sends one.
- **The Discover page's other lists** (`includeRecommendations`,
  `includeTrending`, `includePopular`) are answered as if they were off:
  they ask the metadata server for lists Unarr does not fetch.

## How to run the two tools again

    python3 -I scripts/route-audit.py --app radarr --source .cache/src/Radarr/src

    scripts/radarr-fixture.sh shell &
    unarrd --data .cache/unarr-audit --personalities radarr \
        --import-from tests/fixtures/radarr --bind 127.0.0.1   # on 7879
    make http-audit ORIGINAL_URL=http://127.0.0.1:7878 RADARR_URL=http://127.0.0.1:7879

The three rows the HTTP audit still prints are artefacts of running both
on one machine: the fixture script empties `/movies`, so Unarr's scan
removes the file rows the original has not rescanned yet, and the
original's manual import fails on the folder that is gone.
