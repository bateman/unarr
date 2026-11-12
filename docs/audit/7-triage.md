# Triage of the audit's rows, 12 November 2026

The reports of this folder describe Unarr at `77a39ce`. The high and medium
findings of the README were fixed afterwards and the README closed with
"still open: the low ones", but no row was marked again, and work since
(the fixes themselves, the live HTTP run, phases 4a to 4c) closed many of
them on the way. This is every row the four static reports gave as other
than present or same, read against `main` at `0a61101`, Sonarr personality
and the layer both personalities share. Report 5 (the UI in a browser) had
no finding.

Updated the same day, after fifteen of the open and partial rows were
settled (items 1, 2, 4, 5, 7 and 11 of the list below; fourteen closed,
one a decision to differ): the rows say so with the code, and the golden
or the test, that settled them, and the counts are the new ones. Each was recorded from Sonarr 4.0.20, or
tried on it, first; "What was closed afterwards" says how.

## Result

| Report | Rows | Closed | Open | Partial | Deliberate | Not applicable |
|--------|------|--------|------|---------|------------|----------------|
| 1. Settings | 18 | 11 | 4 | 1 | 0 | 2 |
| 2. HTTP | 58 | 21 | 27 | 6 | 3 | 1 |
| 3 and 4. Routes and UI calls | 78 | 36 | 29 | 2 | 2 | 9 |
| 6. Events, commands, housekeeping | 39 | 29 | 6 | 1 | 2 | 1 |
| Together | 193 | 97 | 66 | 10 | 7 | 13 |

Before the update: 83 closed, 78 open, 13 partial, 6 deliberate. Some gaps
are counted in two reports (the SignalR transports, the file dates), so the
distinct open items are about seventy. States: **closed**, does what the report says the original
does; **open**; **partial**; **deliberate**, a recorded decision to differ;
**not applicable**, nothing to port.

"Still open: the low ones" was wrong on two counts. Nine findings the
reports rated medium were never in the README's numbered list and are open
or partial: script import and file dates after a series scan
(`1-settings.md`), and rows 1.3, 1.8, 1.10, 1.24, 1.29, 2.11 and 4.5 of
`2-http.md`. And well over half of the low rows are closed.

How far to trust it: reports 1, 3, 4 and 6 were read against the code, not
run; report 2 was read and, where that did not settle it, tried on a running
`unarrd`. "Closed" means the code does it, with a file and line, a test or a
golden; it was not compared again with the original. The Sonarr source is
not in the tree, so what the original does is taken from the reports.

## What is open, by how much it shows

Would show in ordinary use:

1. ~~Health checks are not re-run after a save~~ (`6-events.md`,
   `CheckOn`). Closed.
2. ~~The web app manifest is served with its placeholders~~ (4.5, 4.6).
   Closed.
3. **Open pages do not repaint on a grab**: SignalR `calendar`,
   `wanted/missing`, `wanted/cutoff`, and `episode` with `grabbed=true`.
   Small in code; `docs/DESIGN.md` section 18 ties any change to Sonarr's
   hub to recording it from the original first. That no longer means
   recording the long list again: `tests/fixtures/sonarr/golden-hub` is a
   list of its own with the hub listened to, and a grab added to it
   records these. The same recording shows two more the hub sends and
   Unarr does not: `health` after every run of the checks, and `command`
   sync after a `MessagingCleanup`.
4. ~~The blocklist's Protocol filter shows every row~~. Closed.
5. ~~A queue item imported by a rescan or a drop-folder scan lingers~~.
   Closed.
6. **Static files are fetched again on each page load** (4.9 with 4.3): the
   links carry no hash, so only the hashed bundles are cached. Small.
7. ~~Lists in a second tab are stale~~: SignalR `tag`, `rootfolder`,
   `qualitydefinition`, `system/task`. Closed.

Would show in a particular set-up:

8. **A URL base**: a request outside it is a 404, `/ping` included, where
   the original redirects into it (1.10); one saved from the UI is half
   applied until the restart (1.35); it is matched with its case (1.11).
   Small.
9. **A reverse proxy that rewrites `Host`, with Allowed Hosts set** (1.3):
   every request is refused. Small.
10. **A proxy that does not pass WebSocket upgrades** (2.11): no live
    updates. Larger.
11. ~~A body over 2 MiB~~ (1.29). Closed: the limit is 64 MiB, a decision
    to differ from the original, which has none.
12. **Log-based intrusion detection** (1.8): no line for a failed or
    successful login. Small.
13. **Change File Date switched on** (`FileDate`): nothing after a series
    scan; the movie side does it. Small.
14. **Import Using Script** (`UseScriptImport`): stored and ignored, the
    import goes Unarr's own way without a warning. Larger.
15. **A UI language other than English** (`UILanguage`): only the English
    dictionary is in the tree. Larger, by the 39 dictionaries.
16. **A clock that jumps back** (`FixFutureRunScheduledTasks`): scheduled
    tasks wait for the old time. Trivial.
17. **A manual import of an untracked file**: no Import Complete
    notification. Small.

Worth a decision rather than a fix:

18. **A panic ends the process** (1.13): the release profile aborts, and one
    process holds every personality. A catch layer does nothing until the
    profile changes, at some binary size.
19. **Paths under `/api` that are not v3** (1.24) reach the page fallback,
    and wrong-case paths outside the API (1.15) reach the UI guard, which
    redirects a wrong-case SignalR negotiation with its token in
    `Location`. No client is known to do either.

The rest is routes nobody is known to call (`GET` by id on the `config/*`
resources, the metadata and `languageprofile` writes,
`qualitydefinition/limits`), headers nobody is known to read (`Server`, the
Basic realm, three static content types, the backup's type), `robots.txt`,
Brotli, the 503 while starting, the console log settings, the deprecated
`CheckForFinishedDownload`, the legacy `config.xml` user.

The maintainer's installation, by the fixtures: no URL base, no script
import, Change File Date off, English UI, Traefik in front (which passes
`Host` and WebSockets). Items 8 to 10 and 13 to 15 do not touch it.

## What was closed afterwards

Each on the original first. `<golden>` is
`tests/fixtures/sonarr/golden`, `<hub>` is
`tests/fixtures/sonarr/golden-hub`, a second request list recorded with the
hub listened to (`GOLDEN_SIGNALR=1` with `make golden`, its header says
how), which the long list did not have to be recorded again for.

- **Health checks after a save.** The sets are the `CheckOn` attributes of
  Sonarr 4.0.20's checks, read from `Sonarr.Core.dll`, and its trace log
  shows the same checks starting five seconds after each save: every
  settings page but the naming runs `ImportMechanismCheck` and
  `ProxyCheck`, the host page included; a root folder added runs the three
  download client checks of folders and `ImportListRootFolderCheck`, not
  `RootFolderCheck`, which follows a series deleted or moved; a mapping
  added or changed runs the same three and `RemotePathMappingCheck`.
  Against the row: a root folder or a mapping removed runs nothing in the
  original, which deletes them by id without an event, and neither does
  Unarr. Not a golden, the health dictionary being what it is: tests.
- **The manifest and the tile description.** Two goldens, and the same
  tried on Radarr 6.4.4 and under a URL base with another instance name.
- **The blocklist's Protocol filter.** Eight goldens: names in any case or
  numbers, repeated; what the framework cannot bind is a 400 naming it.
- **The refresh after a scan imports.** `<hub>`: five seconds after a
  `RescanSeries` and after a `DownloadedEpisodesScan` that imported, the
  original's command list shows a `RefreshMonitoredDownloads` nobody
  scheduled.
- **The lists of a second tab.** `<hub>/signalr.json`. Against the rows:
  the original sends `tag` sync and no resource; `rootfolder` only when
  one is added; `qualitydefinition` only after the reset, not after a
  save of the definitions.
- **The body limit.** Sonarr answered 202 to a `PUT` of 120 MiB. Unarr
  takes 64 MiB once the key is good, the ceiling it already had on what it
  reads from indexers and clients; no limit at all would let one request
  end the process every personality runs in.

Found on the way and left, with no row of their own: with a proxy whose
name does not resolve the original's `ProxyCheck` throws and the whole run
ends without a verdict, where Unarr reports "Failed to resolve the IP
Address"; a check that throws (`UpdateCheck` without a network, for one)
silently ends the original's run with it.

## Documentation the triage found stale

- `docs/DESIGN.md` section 14 said `X-Forwarded-Proto` and
  `X-Forwarded-Host` are not read, and that the debug and trace files
  follow `RUST_LOG` alone. Both corrected with this triage.
- `docs/DESIGN.md` section 11 and `api/hosts.rs` say the original drops
  `0.0.0.0` and `[::]` from Allowed Hosts; `2-http.md` row 1.5 says it does
  not. Unsettled.
- The line numbers inside the four reports are those of `77a39ce`.

## The server outside the API bodies (`2-http.md`)

58 rows were DIFFERENT, MISSING or UNSURE: 21 closed, 27 open, 6 partial, 3
deliberate, 1 not applicable. Read, and where reading did not settle it tried
on a debug build of `unarrd` on the Sonarr fixture ("live"). The original's
side of rows 1.6, 1.11, 1.15, 1.18, 1.30, 1.34, 2.12, 4.11 and 4.20 was never
measured: `scripts/http-audit.py` probes no static file, no wrong-case path
outside the API, no `Accept` header, no refused host and no URL base. `api/`
is `crates/unarr-api/src/`.

| Row | Finding | Severity then | State | Evidence | What is left, who notices |
|---|---|---|---|---|---|
| 1.2 | `X-Forwarded-Proto` gives the scheme | low | CLOSED | `api/ui.rs:390`; tests `tests/http.rs:491`; live | |
| 1.3 | `X-Forwarded-Host` before the host filter | medium | PARTIAL | `api/hosts.rs:114` reads `Host` only; the header serves the login redirect alone; live: 400 | a reverse proxy that rewrites `Host`, with Allowed Hosts set: every request is refused |
| 1.5 | `0.0.0.0` and `[::]` in Allowed Hosts dropped | low | DELIBERATE | `docs/DESIGN.md` section 11, `api/hosts.rs:33`. The report disputes that the original drops them; reading its `AllowedHostsParser.cs` would settle it | nobody in practice |
| 1.6 | body of the 400 for a refused host | low | OPEN | `api/hosts.rs:119`: empty | nobody |
| 1.7 | request log | low | OPEN | `api/lib.rs:354`: a debug span, no line per request | whoever reads the logs |
| 1.8 | `Auth-Failure`, `Auth-Success`, `Auth-Logout` log lines | medium | OPEN | no log call in `api/ui.rs`; live: a failed login logs nothing | fail2ban and the like see nothing |
| 1.10 | a request outside the URL base | medium | OPEN | `tests/http.rs:360` asserts 404; live | no 307 into the base: a browser on `host:8989/`, a probe on `/ping` without the base |
| 1.11 | URL base matched without case | low | OPEN | `api/lib.rs:366`; live | a bookmark or proxy rule in another case |
| 1.13 | a failure in a handler becomes a 500 | low | OPEN | no catch layer, and `Cargo.toml` sets `panic = "abort"` | a panic ends the process and every personality in it, not the connection; only when Unarr has a bug |
| 1.15 | routing without case outside the API | low | OPEN | `api/lib.rs:325`; live. A wrong-case SignalR negotiation under Forms is redirected with its `access_token` copied into `Location` | nobody known |
| 1.16 | one trailing slash ignored | medium | CLOSED | `api/lib.rs:41`, `api/http.rs:330`; tests | |
| 1.17 | CORS on the API | medium | CLOSED | `api/http.rs:246`; test | |
| 1.18 | CORS on `/Content` | low | OPEN | `api/ui.rs:156`; live | another origin loading icons or fonts |
| 1.19 | compression | low | PARTIAL | `api/http.rs:286`: gzip only | no Brotli; `Vary` also to a client that sent no `Accept-Encoding` |
| 1.20 | version header once the key is accepted | low | CLOSED | `api/http.rs:201`; tests | |
| 1.21 | 503 while starting | low | OPEN | `unarrd/src/main.rs:683` binds after the runtime started; live: connection refused | a proxy shows 502 where the original shows 503 |
| 1.22 | no-cache headers on the API | low | CLOSED | `api/http.rs:207`; test | |
| 1.23 | `/initialize.json` with `no-store` | medium | CLOSED | `api/routes/system.rs:144` | |
| 1.24 | `/api`, `/api/v1/…`, other paths that are not v3 | medium | PARTIAL | `GET /api` answers; live: `/api/v1/foo` and `/api/foo` reach the page fallback, a 302 to the login under Forms, key or not | a client that probes versions or mistypes a path gets a redirect, not 401 or 404 |
| 1.25 | unknown v3 route without a key: 401 | low | CLOSED | `api/lib.rs:269`; test | |
| 1.27 | `If-Modified-Since` | low | NOT APPLICABLE | the original's never fires | |
| 1.29 | request body limit | medium | DELIBERATE | 64 MiB once the key is good (`api/lib.rs:231`, `:285`), where the original has none (202 to a `PUT` of 120 MiB); the login form keeps 2 MiB; test `tests/http.rs:427` | a body over 64 MiB: 413 |
| 1.30 | 406 for an `Accept` without JSON | low | OPEN | live: 200 JSON | nobody |
| 1.31 | `application/json; charset=utf-8` | low | CLOSED | `api/http.rs:89` | |
| 1.32 | `HEAD` | low | PARTIAL | API and feed 405, `/ping` 200 (`api/http.rs:63`); pages and static files still answer | nobody |
| 1.34 | `Server` header | low | OPEN | none sent | nobody |
| 1.35 | what applies without a restart | unsure | PARTIAL | settled live: a saved URL base changes `/initialize.json`, the page links, the login redirect and the cookie path at once while the routes stay where they were; allowed hosts and trusted networks apply at once where the original waits | changing the URL base from the UI: broken until the restart |
| 2.1 | `/ping` | high | CLOSED | `api/lib.rs:326` | |
| 2.2 | the calendar feed | high | CLOSED | `api/routes/mod.rs:47` | |
| 2.3 | `GET /api` | low | CLOSED | `api/routes/system.rs:157` | |
| 2.4 | covers with the key | high | CLOSED | `api/routes/mod.rs:80`; live | |
| 2.8 | backup download headers | low | OPEN | `application/zip` and a disposition (`api/routes/backup.rs:137`) | nobody; the file downloads |
| 2.9 | `/MediaCoverProxy/{hash}/{file}` | low | OPEN | the file name is ignored, the type guessed from the remote address (`api/routes/series_writes.rs:57`); a wrong name is a 500 | a GIF poster labelled JPEG on Add Series; browsers sniff |
| 2.11 | SignalR over server-sent events and long polling | medium | OPEN | advertised, not served (`api/routes/signalr.rs:83`) | a browser behind a proxy that does not pass WebSocket upgrades: no live updates |
| 2.12 | negotiate version | low | OPEN | always version 0 (`signalr.rs:84`) | nobody; the client takes either |
| 2.13 | `version` message to all clients | low | OPEN | to the new socket only (`signalr.rs:149`) | nobody |
| 3.2 | the key as a bearer token | medium | CLOSED | `api/auth.rs:26`; test | |
| 3.4 | cookie `Secure` and `Path` | medium | CLOSED | `api/ui.rs:330`; tests | |
| 3.5 | cookie name with a non-ASCII instance name | low | OPEN | `api/state.rs:366` drops the letters, no folding | nobody |
| 3.6 | session seven days, sliding | medium | CLOSED | `api/ui.rs:317`; test | |
| 3.8 | forms login when the method is None | low | OPEN | `api/ui.rs:194` never looks at the method; live | nobody |
| 3.9 | challenge under Forms | low | PARTIAL | absolute address and escaped `returnUrl` done; no 401 for `X-Requested-With`, and under a URL base `returnUrl` lacks the base | nobody known |
| 3.11 | Basic realm | low | OPEN | `realm="Unarr"` (`api/ui.rs:543`) | a browser asks once more for credentials saved under "Sonarr" |
| 3.15 | `TrustCgnatIpAddresses` | medium | CLOSED | `api/ui.rs:497`; test | |
| 3.16 | IPv6 site-local counts as local | low | CLOSED | `core/outbound.rs:208`; test | |
| 3.17 | `/favicon.ico` behind the login | low | OPEN | anonymous (`api/lib.rs:325`) | nobody |
| 4.3 | `?h=` cache breaker on asset links | low | OPEN | `api/ui.rs:92` adds the base only | stands in the way of 4.9 |
| 4.5 | `manifest.json` placeholders | medium | CLOSED | `api/ui.rs:177`; golden `noauth-content-manifest`; test `tests/http.rs:379`, both personalities | its cache lifetime is row 4.9 |
| 4.6 | `browserconfig.xml` placeholder | low | CLOSED | same handler; golden `noauth-content-browserconfig`; same test | |
| 4.7 | `/robots.txt` | low | OPEN | no route; `/Content/robots.txt` is served | crawlers, on an instance exposed without a login |
| 4.9 | cache headers on static files | low | OPEN | `api/ui.rs:63`: hashed bundles one year, the rest `no-cache`. `/ping` uncached is deliberate | fonts, icons and styles are fetched again on each page load |
| 4.10 | cache headers on HTML | low | CLOSED | `api/ui.rs:116` | |
| 4.11 | cache headers on `/MediaCover` | low | OPEN | no `Cache-Control`, the file's own `Last-Modified` and an `ETag` (`api/lib.rs:302`) | the browser revalidates more often |
| 4.15 | source maps | low | DELIBERATE | dropped (`docs/DESIGN.md` section 11) | browser developer tools |
| 4.16 | files the original refuses | low | OPEN | `/index.html` raw, licence files (`api/ui.rs:142`) | nobody |
| 4.18 | `text/html` without charset | low | CLOSED | `api/ui.rs:119` | |
| 4.19 | type of other static files | unsure | OPEN | settled against the framework's table in the release: `.js`, `.ttf` and the charset of `.txt` differ | nobody; browsers take both |
| 4.20 | 404 for a file-like path | low | OPEN | empty (`api/ui.rs:148`) | nobody |

What the triage found against the report and the README:

- Seven medium rows of this report were never in the README's numbered
  list and were open or partial: 1.3, 1.8, 1.10, 1.24, 1.29, 2.11, 4.5
  (1.29 and 4.5 since closed).
- Row 1.35 was unsure and is a real difference: a URL base saved from the
  UI is half applied until the restart.
- Row 1.13 understates: the release profile aborts on a panic, so the
  process ends.
- `docs/DESIGN.md` section 14 said `X-Forwarded-Proto` and
  `X-Forwarded-Host` are not read; both are, except by the host filter.
- Row 12 of the live-run table ("same") holds without a URL base only.

## Routes and UI calls (`3-4-routes.md`)

78 rows were MISSING, PARTIAL, IGNORED or UNSURE: 36 closed, 29 open, 2
partial, 2 deliberate, 9 not applicable. Static: nothing was run. `mod.rs`
is `crates/unarr-api/src/routes/mod.rs` (Sonarr's list is
`sonarr_api_router`); the other files are under `crates/unarr-api/src/` and
its `routes/`; `core/` is `crates/unarr-core/src/`; golden names are lines of
`tests/fixtures/sonarr/golden/requests.txt`. For a route that is not
declared, 404 and 405 are what the router gives by construction.

| Table | Method and path, or UI call | State | Evidence | Who calls it, what fails without it |
|---|---|---|---|---|
| A | `DELETE /blocklist/{id}` | CLOSED | `mod.rs:199`, `activity.rs:411`; goldens `blocklist-delete`, `blocklist-delete-missing` | UI, Blocklist |
| A | `DELETE /blocklist/bulk` | CLOSED | `mod.rs:198`, `activity.rs:420`; goldens `blocklist-bulk-delete`, `blocklist-after-removal` | UI, Remove Selected |
| A | `GET /calendar/{id}` | OPEN | only `/calendar` is declared (`mod.rs:184`); 404 | nobody known |
| A | `GET /config/downloadclient/{id}` | OPEN | PUT only (`mod.rs:71`); 405 | nobody known |
| A | `GET /config/host/{id}` | OPEN | PUT only (`mod.rs:67`); 405 | nobody known |
| A | `GET /config/importlist/{id}` | OPEN | PUT only (`mod.rs:76`); 405 | nobody known |
| A | `GET /config/indexer/{id}` | OPEN | PUT only (`mod.rs:209`); 405 | nobody known |
| A | `GET /config/mediamanagement/{id}` | OPEN | PUT only (`mod.rs:204`); 405 | nobody known |
| A | `GET /config/naming/{id}` | OPEN | PUT only (`mod.rs:202`); 405 | nobody known |
| A | `GET /config/ui/{id}` | OPEN | PUT only (`mod.rs:69`); 405 | nobody known |
| A | `GET /health/{id}` | NOT APPLICABLE | the original's action throws | nobody |
| A | `POST /history/failed/{id}` | CLOSED | `mod.rs:200`, `activity.rs:436`, `core/downloads/failed.rs:120`; goldens `history-mark-failed*` | UI, Mark as failed |
| A | `DELETE /importlistexclusion/bulk` | CLOSED | `mod.rs:299`, `settings_writes.rs:1756`; golden `importlistexclusion-bulk-delete` | UI, exclusions |
| A | `GET /importlistexclusion/{id}` | OPEN | PUT and DELETE only (`mod.rs:303`); 405 | nobody known |
| A | `GET /release/{id}`, `GET /release/push/{id}` | NOT APPLICABLE | the original's actions throw | nobody |
| A | `GET /localization/{id}` | OPEN | not declared (`mod.rs:383`); 404 | nobody known |
| A | `GET /mediacover/{seriesId}/{filename}` | CLOSED | `mod.rs:80`, `series_writes.rs:95`; goldens for the failures, the served cover in `tests/contract.rs:1277` | Bazarr, mobile apps |
| A | `POST /metadata` | OPEN | GET only (`mod.rs:285`); 405 | nobody; the UI never creates a consumer |
| A | `PUT /metadata`, `PUT /metadata/{id}` | CLOSED | `lib.rs:106`, `mod.rs:287`, `providers.rs:364`; goldens `metadata-update`, `-after-update`, `-restore`. No metadata file is written: documented | UI, Settings > Metadata |
| A | `PUT /metadata/bulk`, `DELETE /metadata/{id}`, `DELETE /metadata/bulk`, `POST /metadata/test`, `/testall`, `/action/{name}` | OPEN (6) | not declared | nobody known |
| A | `POST`, `PUT`, `PUT /{id}`, `DELETE /{id}` on `languageprofile`, `GET languageprofile/schema` | OPEN (5) | GET only (`mod.rs:211`) | nobody known; Bazarr only reads |
| A | `GET /languageprofile/schema/{id}` | NOT APPLICABLE | the original's action throws | nobody |
| A | `GET /qualitydefinition/limits` | OPEN | falls to `/qualitydefinition/{id}` (`mod.rs:127`) | nobody known; the 4.0.20 UI does not ask |
| A | `GET /queue/{id}`, `/queue/details/{id}`, `/queue/status/{id}` | NOT APPLICABLE (3) | the original's actions throw | nobody |
| A | `POST /series/import` | CLOSED | `mod.rs:79`, `series_writes.rs:312`; goldens `series-import*` | UI, Library Import |
| A | `GET /system/routes`, `/system/routes/duplicate` | DELIBERATE (2) | `docs/DESIGN.md` section 14, `docs/phase-3b-library.md`: the UI does not ask | nobody known |
| A | `POST /system/shutdown`, `POST /system/restart` | CLOSED (2) | `mod.rs:365`, `system.rs:180`, `unarrd/src/main.rs:816`; test `contract.rs:1169`. The restart re-executes the program, on Unix | UI, header menu and after a restore |
| A | `GET /wanted/cutoff/{id}`, `GET /wanted/missing/{id}` | OPEN (2) | not declared; 404 | nobody known |
| A | `GET /ping`, `HEAD /ping` | CLOSED (2) | `lib.rs:326`, `system.rs:163`; golden `noauth-ping`, test `tests/http.rs:211` | health checks, monitors |
| A | `GET /api` | CLOSED | `mod.rs:55`, `system.rs:157`; test `tests/http.rs:99` | nobody known |
| A | `GET /feed/v3/calendar/Sonarr.ics` | CLOSED | `mod.rs:56`, `feed.rs:145`; goldens `feed-calendar*` | UI iCal link, calendar apps |
| A | `/signalr/messages` over server-sent events and long polling | PARTIAL | advertised (`signalr.rs:86`), only WebSocket served; WebSocket-only is the stated scope | UI behind a proxy that blocks WebSockets |
| B | restart, shutdown, `series/import`, `history/failed/{id}`, blocklist delete and bulk, exclusions bulk, `PUT metadata/{id}` | CLOSED (8) | as Table A | UI |
| B | `GET /delayprofile/schema` | NOT APPLICABLE | dead UI code; the original has no such route | nobody |
| B | the iCal link's parameters | CLOSED | `feed.rs:159` reads `unmonitored`, `premieresOnly`, `asAllDay`, `tags`, `pastDays`, `futureDays` | UI, calendar apps |
| B | SignalR `calendar`, `wanted/missing`, `wanted/cutoff` updated | OPEN (3) | `relay.rs` sends `episodefile`, `episode`, `series` only | UI: the open page does not repaint on a grab |
| B | SignalR `tag` sync | CLOSED | `settings_writes.rs:1073`, after a tag or an auto tagging rule is added, changed or removed; `golden-hub/signalr.json`, test `tests/contract.rs:1616` | UI, second tab |
| B | SignalR `rootfolder`, `qualitydefinition` | CLOSED (2) | `settings_writes.rs:1237` (`created`; the original sends nothing for a removal), `state.rs:302` (`sync` after `ResetQualityDefinitions`; the original sends nothing after a save); same recording and test | UI, second tab |
| B | SignalR `system/task` sync | CLOSED | `core/commands/executor.rs:760`, `state.rs:302`: after every command, for Sonarr as for Radarr; same recording and test | UI, System > Tasks |
| B | SignalR `command` sync | OPEN | `state.rs:293` sends `updated` only; the recording shows `sync` after `MessagingCleanup` | nothing visible |
| B | SignalR `metadata` updated | CLOSED | `providers.rs:408` | UI, second tab |
| B | commands `ClearLog`, `DeleteLogFiles`, `ResetQualityDefinitions`, `ResetApiKey` | CLOSED (4) | `core/commands/mod.rs:712-718`, `executor.rs:911-934`, `state.rs:535-553`; goldens for the first three, none for `ResetApiKey` | UI buttons |
| B | `GET /config/naming/examples` with the pending form | CLOSED | `settings.rs:155`; goldens `config-naming-examples-form`, `-invalid`, `-no-id` | UI, naming examples while typing |
| B | `GET /blocklist?protocols=` | CLOSED | `activity.rs:385`, `core/store/history.rs:237`; goldens `blocklist-protocol-*` (8) | UI, custom filter on Protocol |
| B | `PUT /series/{id}?moveFolder=true` | NOT APPLICABLE | parity: the original reads `moveFiles`, and so does `series_writes.rs:375` | nothing |
| B | SignalR transport fallback | PARTIAL | the Table A row | as above |
| B | `/Content/manifest.json`, `/Content/browserconfig.xml` | CLOSED | `ui.rs:177`; goldens `noauth-content-manifest`, `noauth-content-browserconfig`; test `tests/http.rs:379` | a browser installing the UI as an app |
| B | `GET /oauth.html` | CLOSED | in the embedded bundle, served by the page fallback (`lib.rs:313`) behind the login | UI: the OAuth pop-up of an import list |

What the triage found against the report and the README:

- The report's Table B says the manifest rewrite "only matters with a URL
  base"; the bundled files carry both placeholders, so it matters without
  one too, as `2-http.md` rows 4.5 and 4.6 say.
- `DeleteUpdateLogFiles` (README finding 10, fixed) is accepted and
  completes without doing anything: Unarr has no updater and no update log.
- The restart of finding 9 replaces the process on Unix only.
- The five goldens of the cover route are all failure answers; a cover
  actually served is checked by the contract test, not against the original.

## Events, commands, housekeeping (`6-events.md`)

39 rows were MISSING, PARTIAL or UNSURE: 29 closed, 6 open, 1 partial, 2
deliberate, 1 not applicable. Static. `core/` is `crates/unarr-core/src/`,
`api/` is `crates/unarr-api/src/`, `import/` is `crates/unarr-import/src/`.
The line numbers inside the report itself are stale; these are current.

| Row | Finding | Severity then | State | Evidence | What is left, who notices |
|---|---|---|---|---|---|
| (a) start: quality definitions | missing ones not inserted | medium | CLOSED | `core/defaults.rs:30`, called at every start (`unarrd/src/main.rs:476`) | |
| (a) start: default profiles | none on a blank start | medium | CLOSED | `core/defaults.rs:137`, `:184` | |
| (a) start: legacy user | `config.xml` user and password moved into Users | low | OPEN | `import/config_xml.rs:52` parses neither element; no start-up step | only a `config.xml` never started on Sonarr v3 or later: the login would have no user |
| (a) tags, auto tags changed | no SignalR `tag` sync | low | CLOSED | `api/routes/settings_writes.rs:1073`; `golden-hub/signalr.json`, test `api/tests/contract.rs:1616` | |
| (a) command executed: `system/task` | Tasks page not told to sync | low | CLOSED | `core/commands/executor.rs:760`, `api/state.rs:302`; same recording and test | |
| (a) command executed: `qualitydefinition` | no sync after a reset | low | CLOSED | `api/state.rs:302`; same recording and test | |
| (a) config file saved: logging | saved log level not applied | low | CLOSED | `settings_writes.rs:1004`, `unarrd/src/main.rs:765`; `RUST_LOG`, when set, still decides | |
| (a) config saved: authorisation | re-read not traced | unsure | CLOSED | `api/ui.rs:476` reads the settings on every request | |
| (a) custom format deleted | scores not reset | medium | CLOSED | `settings_writes.rs:2052` | |
| (a) episode grabbed | no `episode` with `grabbed=true` | low | OPEN | no `grabbed` in Sonarr's routes; deferred with the hub recording | the row flips to "downloading" when the queue details arrive |
| (a) episode imported: client refresh | an import outside the queue asks no client | low | CLOSED | `executor.rs:2301`, after a rescan (`:2290`) and a drop-folder import (`:2385`) that brought a file in; `golden-hub/command-after-rescan`, `command-after-downloaded-scan`, test `api/tests/contract.rs:1616` | |
| (a) health `CheckOn` triggers | checks re-run on some events only | low-medium | CLOSED | `core/health.rs:250` (the sets), `api/state.rs:347`, `settings_writes.rs:159`, `:1233`, `:1313`, `:1332`, `importlist_writes.rs:290`, `executor.rs:2156`, `:2216`; tests `core/health.rs:1276`, `api/tests/contract.rs:1397` | the checks Unarr does not have are not run: `RemovedSeriesCheck`, `ImportListStatusCheck`, `ApiKeyValidationCheck` |
| (a) root folder changed | no SignalR `rootfolder` | low | CLOSED | `settings_writes.rs:1237`: `created`; the original sends nothing for a removal; `golden-hub/signalr.json`, same test | |
| (a) indexer settings cache | equivalent not found | low | NOT APPLICABLE | settings are read from the database for every decision (`core/decision/lookup.rs:189`) | |
| (a) series edited | a type change queues no refresh | medium | CLOSED | `api/routes/series_writes.rs:427`; test `tests/contract.rs:1333` | |
| (a) series imported | no `POST /series/import` | medium | CLOSED | `api/routes/mod.rs:79` | |
| (a) scan skipped | no event when the root folder is missing or empty | medium | CLOSED | `core/mediafiles/scan.rs:233`; test `:509` | |
| (a) series scanned: media info | files without it never probed again | medium | CLOSED | `scan.rs:147`, `:344`; test `:407` | |
| (a) series scanned: file dates | not applied after a scan | low | OPEN | `change_file_date` is reached from the import and the rename only (`core/mediafiles/approve.rs:321`, `:361`, `:410`); the movie twin does it after a scan | with Change File Date on, the existing library keeps its dates |
| (a) untracked download completed | not raised by a manual import | low | OPEN | `core/mediafiles/manual.rs:735` raises nothing for an untracked file | no Import Complete notification after a manual import without a download id |
| (b) `CheckForFinishedDownload` | deprecated alias unknown | low | OPEN | not among the names (`core/commands/mod.rs:690`); 500 | old third-party callers |
| (b) `Housekeeping`, `CleanUpRecycleBin` | did nothing | medium | CLOSED (2) | `executor.rs:904`, `:936`; `core/housekeeping.rs:43`, `core/mediafiles/transfer.rs:204` | |
| (b) `ResetQualityDefinitions`, `ResetApiKey`, `ClearLog`, `DeleteLogFiles`, `DeleteUpdateLogFiles` | unknown | low | CLOSED (5 names, 4 rows) | `commands/mod.rs:712-718`, `api/state.rs:535-553`; `DeleteUpdateLogFiles` has nothing to delete | |
| (b) restart, shutdown | no routes | low | CLOSED | `api/routes/mod.rs:365` | |
| (c) orphaned history, episode files | partial | low | CLOSED (2) | `housekeeping.rs:71`, `:87` | |
| (c) pending releases older than fourteen days | missing | low-medium | CLOSED | `housekeeping.rs:123`; tests | |
| (c) provider status times in the future | missing | low | CLOSED | `housekeeping.rs:156`; tests | |
| (c) scheduled tasks last run in the future | missing | low | OPEN | not among the housekeepers; `core/commands/scheduler.rs:188` treats a future last run as not due | after the clock jumps back, tasks wait until the old time |
| (c) profile format items repair | no repair pass | low | DELIBERATE | the score reset is done; the pass is declared not ported in `housekeeping.rs:9` | a hand-edited database only |
| (c) unused tags | missing | low | CLOSED | `housekeeping.rs:179`; tests | |
| (c) clean titles | no pass | low | DELIBERATE | `housekeeping.rs:9`; the refresh rewrites it (`core/tv/refresh.rs:165`) | a stale clean title lasts until the next refresh |
| (c) additional naming rows, users | keep one | low | PARTIAL | naming is one row by schema; users are not trimmed | nobody expected |
| (c) `VACUUM`, recycle bin | missing | low, medium | CLOSED (2) | `housekeeping.rs:56`; as `CleanUpRecycleBin` | |

## Settings (`1-settings.md`)

18 rows were NOT APPLIED or PARTIAL in the detailed tables: 11 closed, 4
open, 1 partial, 2 not applicable.

| Setting | Finding | Severity then | State | Evidence | What is left, who notices |
|---|---|---|---|---|---|
| `LaunchBrowser` | stored, never used | low | NOT APPLICABLE | a daemon in a container has no browser to open | |
| API key through `PUT config/host` | not changed on save | high | CLOSED | the finding was wrong about the original (golden `config-host-put-apikey`); `ResetApiKey` changes it | |
| authentication method and requirement, user name, password through PUT | not stored | high | CLOSED (2) | `settings_writes.rs:745`, `:985` | |
| `LogLevel` | not applied | medium | CLOSED | `settings_writes.rs:1004`; test `unarrd/src/main.rs:1278` | `RUST_LOG` overrides it when set |
| `ConsoleLogLevel` | not applied, not stored | low | OPEN | `api/routes/config.rs:93` answers a constant | a console quieter or louder than the files |
| `ConsoleLogFormat` | not parsed | low | OPEN | no occurrence | a log shipper expecting CLEF |
| `LogDbEnabled`, `LogSql` | not parsed | low | NOT APPLICABLE | no log database exists | |
| `Sonarr__*` environment overrides | not read | medium | CLOSED | `core/config.rs:101`; test `:231` | |
| `RecycleBinCleanupDays` | never read | high | CLOSED | `core/mediafiles/transfer.rs:204` | |
| `FileDate` | import and rename only | medium | PARTIAL | the events row above: nothing after a series scan | with Change File Date on, the existing library and corrected air dates |
| `EnableMediaInfo` | no backfill on rescan | low | CLOSED | `core/mediafiles/scan.rs:147` | |
| `UseScriptImport`, `ScriptImportPath` | not applied | medium | OPEN | stored and shown (`settings_writes.rs:349`), no occurrence in `unarr-core` | whoever imports through a script gets Unarr's own move or hardlink, with no warning |
| `ImportExtraFiles`, `ExtraFileExtensions` | not applied | medium | CLOSED | `core/mediafiles/extras.rs:334` | |
| `UILanguage` | not applied | low | OPEN | `api/routes/system.rs:364` always serves the English dictionary, the only one in the tree | whoever picks another language: the interface stays English |
| proxy (8 settings) | not applied | high | CLOSED | `core/http.rs:71`, `core/outbound.rs:308`; test `core/http.rs:325` | `ProxyCheck` runs again five seconds after a save of the settings (`core/health.rs:250`) |
| `CertificateValidation` | not applied | medium | CLOSED | `core/outbound.rs:317`; test `:604` | |
| `TrustCgnatIpAddresses` | not applied | medium | CLOSED | `api/ui.rs:500`, `core/outbound.rs:227` | |

What the triage found against the reports and the README:

- Two medium findings of `1-settings.md` were never in the README's
  numbered list and are not closed: script import, and file dates after a
  series scan (the fix of `e0a81e4` was for movies). The `CheckOn` row,
  low-medium, was unchanged at the triage and is closed since.
- `docs/DESIGN.md` section 14, "Logs", said the debug and trace files
  follow `RUST_LOG` alone, while the saved Log Level applies at once.
- The same gap carries different severities in two reports (the recycling
  bin: high and medium; file dates: medium and low).
