# Triage of the audit's rows, 12 November 2026

The reports of this folder describe Unarr at `77a39ce`. The high and medium
findings of the README were fixed afterwards and the README closed with
"still open: the low ones", but no row was marked again, and work since
(the fixes themselves, the live HTTP run, phases 4a to 4c) closed many of
them on the way. This is every row the four static reports gave as other
than present or same, read against `main` at `0a61101`, Sonarr personality
and the layer both personalities share. Report 5 (the UI in a browser) had
no finding.

Updated twice since, after twenty of the open and partial rows were
settled (items 1 to 5, 7 and 11 of the list below; eighteen closed, one a
decision to differ, one that turned out to describe nothing the original
does): the rows say so with the code, and the golden or the test, that
settled them, and the counts are the new ones. Each was recorded from Sonarr 4.0.20, or
tried on it, first; "What was closed afterwards" says how.

Updated a third time on 17 November 2026: row 1.13 of `2-http.md`, item 18
of the list below, is closed. A panic costs the request or the command it
happened in (`docs/DESIGN.md`, section 20).

Updated a fourth time on 19 November 2026: four of the rows the reports
rated medium (1.3, 1.8, 1.10 and 1.24 of `2-http.md`, items 8, 9, 12 and 19
below) are closed, and with them four low rows of the same code (1.6, 1.11,
3.11, 3.17); so is a fifth, the file dates after a series scan (`FileDate`
of `1-settings.md`, item 13, with its row of `6-events.md`). The last two,
script import and SignalR without WebSockets (items 14 and 10), are
**deliberate** by the maintainer's decision of the same day
(`docs/DESIGN.md`, sections 21.3 and 21.4, with the options weighed):
Import Using Script stays unapplied and a
health check says so while it is on; the hub is served over WebSockets
only. Each was recorded first on Sonarr 4.0.20 and Radarr 6.4.4
under a changed `config.xml`; "What was closed afterwards" says how.

## Result

| Report | Rows | Closed | Open | Partial | Deliberate | Not applicable |
|--------|------|--------|------|---------|------------|----------------|
| 1. Settings | 18 | 12 | 3 | 0 | 1 | 2 |
| 2. HTTP | 58 | 30 | 19 | 4 | 4 | 1 |
| 3 and 4. Routes and UI calls | 78 | 39 | 25 | 0 | 4 | 10 |
| 6. Events, commands, housekeeping | 39 | 31 | 4 | 1 | 2 | 1 |
| Together | 193 | 112 | 51 | 5 | 11 | 14 |

Before the first update: 83 closed, 78 open, 13 partial, 6 deliberate;
before the fourth, 102 closed, 60 open, 10 partial. Some gaps
are counted in two reports (the SignalR transports, the file dates), so the
distinct open items are some sixty-five. States: **closed**, does what the report says the original
does; **open**; **partial**; **deliberate**, a recorded decision to differ;
**not applicable**, nothing to port.

"Still open: the low ones" was wrong on two counts. Nine findings the
reports rated medium were never in the README's numbered list and were open
or partial: script import and file dates after a series scan
(`1-settings.md`), and rows 1.3, 1.8, 1.10, 1.24, 1.29, 2.11 and 4.5 of
`2-http.md`; all of `2-http.md`'s but 2.11 are closed since, 2.11 is a
decision, and so is script import. And well over
half of the low rows are closed.

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
3. ~~Open pages do not repaint on a grab~~: `episode` with
   `grabbed=true`, `wanted/missing`, `wanted/cutoff`. Closed; there is no
   `calendar` message to send.
4. ~~The blocklist's Protocol filter shows every row~~. Closed.
5. ~~A queue item imported by a rescan or a drop-folder scan lingers~~.
   Closed.
6. **Static files are fetched again on each page load** (4.9 with 4.3): the
   links carry no hash, so only the hashed bundles are cached. Small.
7. ~~Lists in a second tab are stale~~: SignalR `tag`, `rootfolder`,
   `qualitydefinition`, `system/task`. Closed.

Would show in a particular set-up:

8. **A URL base**: ~~a request outside it is a 404, `/ping` included, where
   the original redirects into it (1.10)~~, ~~it is matched with its case
   (1.11)~~, both closed; one saved from the UI is half applied until the
   restart (1.35). Small.
9. ~~**A reverse proxy that rewrites `Host`, with Allowed Hosts set** (1.3):
   every request is refused~~. Closed.
10. **A proxy that does not pass WebSocket upgrades** (2.11): no live
    updates. Deliberate: the hub is served over WebSockets only, which
    every current reverse proxy passes (Traefik, nginx with the upgrade
    headers, Caddy); a proxy that does not is to be configured to.
11. ~~A body over 2 MiB~~ (1.29). Closed: the limit is 64 MiB, a decision
    to differ from the original, which has none.
12. ~~**Log-based intrusion detection** (1.8): no line for a failed or
    successful login~~. Closed.
13. ~~**Change File Date switched on** (`FileDate`): nothing after a series
    scan; the movie side does it~~. Closed.
14. **Import Using Script** (`UseScriptImport`): stored and ignored, the
    import goes Unarr's own way. Deliberate: Unarr runs no import script,
    and since 19 November 2026 a health warning says so while the setting
    is on (`ScriptImportCheck`, Unarr's own).
15. **A UI language other than English** (`UILanguage`): only the English
    dictionary is in the tree. Larger, by the 39 dictionaries.
16. **A clock that jumps back** (`FixFutureRunScheduledTasks`): scheduled
    tasks wait for the old time. Trivial.
17. **A manual import of an untracked file**: no Import Complete
    notification. Small.

Worth a decision rather than a fix:

18. ~~A panic ends the process~~ (1.13): the release profile aborted, and
    one process holds every personality. Decided and closed: the profile
    unwinds, and a panic costs one request or one command.
19. ~~**Paths under `/api` that are not v3** (1.24) reach the page
    fallback~~ (closed), and wrong-case paths outside the API (1.15) reach
    the UI guard, which redirects a wrong-case SignalR negotiation with its
    token in `Location`. No client is known to do it.

The rest is routes nobody is known to call (`GET` by id on the `config/*`
resources, the metadata and `languageprofile` writes,
`qualitydefinition/limits`), headers nobody is known to read (`Server`,
three static content types, the backup's type), `robots.txt`,
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

- **A panic costs one request or one command** (1.13). What the originals
  answer for an exception nothing caught was recorded raw from Sonarr
  4.0.20 and Radarr 6.4.4 (`POST /command` with a name neither knows, an
  `InvalidOperationException` in the controller): 500, `Content-Type:
  application/json` with no charset, `Cache-Control: no-cache,no-store`,
  `Expires: -1`, `Pragma: no-cache`, no `X-Application-Version`, not
  compressed for a client that takes gzip, a body of `message` and
  `description`. It is the answer Unarr already gave for the exceptions
  it knew (`<golden>/command-unknown.json`,
  `customformat-create-badregex.json`), now also given for a panic, whose
  description is the place of the panic where theirs is the .NET stack.
  A command that panics ends `failed`, as the originals' does for an
  exception in its handler. `docs/DESIGN.md`, section 20, has what is
  caught where and what is left fatal.
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
- **The pages that list episodes.** `<hub>/signalr.json`, with a grab,
  an import and an episode file deleted in the list. An episode goes out
  under three names, `episode`, `wanted/missing` and `wanted/cutoff`:
  bare and marked as grabbed after a grab, with its series, file and
  images after an import or a deletion. Against the row: the original has
  no `calendar` message; the calendar's controller names its resource
  `episode`, so the original sends that one twice, and Unarr once. The
  recording also showed that a release sent through the API queues a
  refresh of the downloads, as one sent by a command does; Unarr's did
  not.
- **`health` and `command` on the hub.** Same recording: `health` sync
  after every run of the checks, changed verdict or not, and `command`
  sync once a `MessagingCleanup` the API asked for has completed.
- **The body limit.** Sonarr answered 202 to a `PUT` of 120 MiB. Unarr
  takes 64 MiB once the key is good, the ceiling it already had on what it
  reads from indexers and clients; no limit at all would let one request
  end the process every personality runs in.
- **Allowed Hosts behind a proxy, the login lines, the URL base, `/api`
  outside v3** (1.3, 1.6, 1.8, 1.10, 1.11, 1.24, 3.11, 3.17). Each original
  was run on its fixture with `config.xml` changed for the case
  (`AllowedHosts` `tv.example.com`; `UrlBase` `/tv`; Basic; Disabled for
  Local Addresses), and asked from the loopback, which the framework
  trusts, and from another address of the machine, which it does not;
  then Unarr the same way, until nothing differed but what Radarr's Basic
  does (below). What the recording showed:
  - The forwarded headers run before the host filter, all three in one
    walk from their last entries back, with no limit on the entries: a
    trusted proxy's `X-Forwarded-Host` is the host checked, and with no
    `X-Forwarded-For` beside it every host it lists is applied in turn,
    so the leftmost stands. A refused host gets the framework's HTML page
    (`text/html`, no charset) and an info line of
    `Microsoft.AspNetCore.HostFiltering.HostFilteringMiddleware`.
  - `Auth-Failure ip <address> username '<as typed>'` at warn under the
    logger `Auth`, the address the trusted proxies name; `Auth-Logout`
    with an empty name, signed in or not. Sonarr logs `Auth-Success` at
    info, for the form and for every request with Basic credentials;
    Radarr 6.4.4 logs no success. A wrong API key logs nothing. The Basic
    realm is the application's name whatever the instance is called.
  - Outside the URL base the request is routed as if the base were not
    there and authenticated so (the key for the API, the feed and SignalR,
    the login for a page, nothing for `/ping`, `/login`, `/logout` and
    `/Content`), then sent into the base with a 307 that keeps the method
    and the query and carries the version header on an API path. A
    page's login redirect leaves the base out; inside it, the redirect
    and its `returnUrl` keep the base as asked, its case and a slash
    after it included, which closes that part of 3.9. The base is matched
    in any case; `/favicon.ico` is behind the login.
  - Under `/api`, whatever the version, a path no action takes asks for
    the key (401, a cookie does not do), then answers an empty 404 to
    `GET` and 405 `Allow: GET` to any other method, with the API's
    headers (no cache headers on `OPTIONS`); `GET /API` is the API's
    root. `/api/v3` had answered 404 to every method. A page's path
    takes `GET` alone: another method, `HEAD` included, asks for the key
    and gets 405; the covers and the static files were left as they were,
    not recorded.

  Tests `tests/http.rs` (`a_request_outside_the_url_base_is_sent_into_it_once_authenticated`,
  `what_no_action_takes_is_refused_as_the_originals_refuse_it`,
  `a_trusted_proxys_forwarded_host_is_what_allowed_hosts_checks`,
  `logins_and_logouts_are_logged_for_intrusion_detection`), `contract.rs`
  (`url_base_is_matched_in_any_case`), `forwarded.rs` for the walk.
  Found on the way: Radarr 6.4.4 has no Basic authentication any more. It
  writes `forms` over `Basic` in `config.xml` at start and sends the
  browser to its login page, where Unarr's Radarr still challenged for
  Basic credentials: fixed on Radarr's side, see `8-radarr.md`.

- **Change File Date after a series scan** (`FileDate`). Sonarr 4.0.20
  on its fixture, five files of a series (air time 22:00) put on disk
  with a date of now and a quarter of a second, the setting saved through
  `config/mediamanagement`, then `RescanSeries`, under `TZ=Europe/Rome`:
  `localAirDate` dates each file 22:00 local (21:00 UTC in February) by
  the first episode in it, to the second; `utcAirDate` dates it 22:00 UTC
  and keeps the quarter of a second the file had; `none` leaves it. Unarr
  did nothing after a scan; it now does the same, and compares the date
  it wants with the file's to the tick, as the original does, where it
  compared seconds (the import and the rename share that function).

Found on the way, with no row of their own. With a proxy whose name
does not resolve the original's `ProxyCheck` throws, and the whole run of
the checks ends with it, without a verdict and without a word on the hub
(so does any check that throws: `UpdateCheck` without a network, for one).
Unarr reports "Failed to resolve the IP Address for the Configured Proxy
Host", the message the original has for the case and never reaches: kept,
as a decision (`docs/DESIGN.md` section 19). And `GET /blocklist` takes
`seriesIds=x` for no filter where the framework would answer 400: not
recorded for Sonarr, left.

The same gaps on Radarr's side, which the triage did not cover, closed
with them where there was something to close
(`tests/fixtures/radarr/golden`, and a `golden-hub` of Radarr's own):

- `GET /blocklist` read neither `movieIds` nor `protocols`: both are read
  now (`api/routes/radarr/activity.rs:211`, `core/movies/store.rs:1092`;
  ten goldens `blocklist-protocol-*`, `blocklist-movie-ids*`).
- `RescanMovie` and `DownloadedMoviesScan` queue the refresh of the
  downloads when they import (`core/commands/executor.rs:136`; test
  `api/tests/contract_radarr.rs:865`).
- `command` sync after a `MessagingCleanup`; `health` and `system/task`
  were already as recorded.
- The manifest and the body limit are the shared layer's and were closed
  for both.
- Nothing to close yet: Radarr has no writes of settings, root folders,
  remote path mappings, import lists, tags or quality definitions, no
  movie delete or move, and no `ResetQualityDefinitions`, so the health
  checks after a save and the hub's `tag`, `rootfolder` and
  `qualitydefinition` wait for the milestone that adds them. The original
  does there what Sonarr does: its `CheckOn` attributes are the same but
  for the events' names, and its hub was seen to say `qualitydefinition`
  sync after the reset.

## Documentation the triage found stale

- `docs/DESIGN.md` section 14 said `X-Forwarded-Proto` and
  `X-Forwarded-Host` are not read, and that the debug and trace files
  follow `RUST_LOG` alone. Both corrected with this triage.
- `docs/DESIGN.md` section 11 and `api/hosts.rs` say the original drops
  `0.0.0.0` and `[::]` from Allowed Hosts; `2-http.md` row 1.5 says it does
  not. Unsettled.
- The line numbers inside the four reports are those of `77a39ce`.

## The server outside the API bodies (`2-http.md`)

58 rows were DIFFERENT, MISSING or UNSURE: 30 closed, 19 open, 4 partial, 4
deliberate, 1 not applicable. Read, and where reading did not settle it tried
on a debug build of `unarrd` on the Sonarr fixture ("live"). The original's
side of rows 1.6, 1.11, 1.15, 1.18, 1.30, 1.34, 2.12, 4.11 and 4.20 was never
measured: `scripts/http-audit.py` probes no static file, no wrong-case path
outside the API, no `Accept` header, no refused host and no URL base. `api/`
is `crates/unarr-api/src/`.

| Row | Finding | Severity then | State | Evidence | What is left, who notices |
|---|---|---|---|---|---|
| 1.2 | `X-Forwarded-Proto` gives the scheme | low | CLOSED | `api/ui.rs:390`; tests `tests/http.rs:491`; live | |
| 1.3 | `X-Forwarded-Host` before the host filter | medium | CLOSED | `api/forwarded.rs` (`walk`), `api/hosts.rs` (`require_allowed_host`); test `tests/http.rs` (`a_trusted_proxys_forwarded_host_is_what_allowed_hosts_checks`); recorded | as in the originals, a page rebound to Unarr can name a listed host in `X-Forwarded-Host` from the loopback or through a proxy that passes the header on (nginx by default): Allowed Hosts does not stop that DNS rebinding (`docs/DESIGN.md`, section 14) |
| 1.5 | `0.0.0.0` and `[::]` in Allowed Hosts dropped | low | DELIBERATE | `docs/DESIGN.md` section 11, `api/hosts.rs:33`. The report disputes that the original drops them; reading its `AllowedHostsParser.cs` would settle it | nobody in practice |
| 1.6 | body of the 400 for a refused host | low | CLOSED | `api/hosts.rs`: the framework's page as recorded, `text/html`, and the host filter's log line; same test | |
| 1.7 | request log | low | CLOSED | `api/http.rs` (`log_requests`), placed after the host filter (`api/lib.rs`), as `LoggingMiddleware.cs`: `Req:`/`Res:` with a sequence number, path and query, client address and user agent under `Http` at trace, `[GET] path: 404.NotFound (n ms)` under `Api` at debug; the key cleansed as in the originals' files; `unarrd` lets the two loggers through at the levels of the settings; test `tests/http.rs` (`each_request_is_logged_as_the_originals_log_it`); recorded on both (`scripts/http-probe.py`, `lowhttp*`), lines compared in `sonarr.trace.txt` | the duration ends with the headers, not with the body |
| 1.8 | `Auth-Failure`, `Auth-Success`, `Auth-Logout` log lines | medium | CLOSED | `api/ui.rs` (`log_auth`): the form, Basic, the logout; Radarr logs no success, as recorded; test `tests/http.rs` (`logins_and_logouts_are_logged_for_intrusion_detection`) | |
| 1.10 | a request outside the URL base | medium | CLOSED | `api/lib.rs` (`into_url_base` in each group, after its authentication); test `tests/http.rs` (`a_request_outside_the_url_base_is_sent_into_it_once_authenticated`); recorded | |
| 1.11 | URL base matched without case | low | CLOSED | `api/lib.rs` (`normalize_path`); test `tests/contract.rs` (`url_base_is_matched_in_any_case`); recorded | |
| 1.13 | a failure in a handler becomes a 500 | low | CLOSED | `Cargo.toml` sets `panic = "unwind"`; `api/http.rs` `catch_panic` inside the API's pipeline and `catch_panic_anywhere` around the router (`api/lib.rs`) answer the 500 recorded from both originals; `core/panics.rs`; `make panic-test` on a release build | |
| 1.15 | routing without case outside the API | low | CLOSED | `api/lib.rs` (`literal_route`, in `normalize_path`): `/login`, `/logout`, `/ping`, `/initialize.json`, the SignalR pair, the feed and the `/content/` prefix in any case; what the originals' catch-all hands to a mapper (`/favicon.ico`, covers, backups) keeps the mapper's own case rule; test `tests/http.rs` (`routes_outside_the_api_are_matched_without_case`); recorded on both (`scripts/http-probe.py`, `lowhttp*`) | |
| 1.16 | one trailing slash ignored | medium | CLOSED | `api/lib.rs:41`, `api/http.rs:330`; tests | |
| 1.17 | CORS on the API | medium | CLOSED | `api/http.rs:246`; test | |
| 1.18 | CORS on `/Content` | low | CLOSED | `api/http.rs` (`cors_allow_get`) on the content route (`api/lib.rs`, `content_cors`): `Access-Control-Allow-Origin: *` to a request with an origin, a 404 included; a preflight for `GET` or `OPTIONS` answered 204 with `GET,OPTIONS` and the asked headers echoed, in any case of the path; a preflight for another method, or an `OPTIONS` without one, goes on to the 401; test `content_may_be_read_from_any_origin`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | |
| 1.19 | compression | low | CLOSED | `api/http.rs` (`compression`, `compression_as_requested`): Brotli and gzip at level 1 (`CompressionLevel.Fastest`), Brotli when the client weighs the two alike, deflate not offered; `Vary: Accept-Encoding` only to a client that sent `Accept-Encoding`; nothing compressed over HTTPS a trusted proxy names (`EnableForHttps` off); test `json_and_pages_are_compressed_as_the_originals_compress`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | Brotli kept by the maintainer's decision (20 November 2026) at a cost of 1,126,528 bytes to the release binary, 522,738 under `gzip -9` (`docs/DESIGN.md`, section 22); the encoders' bytes differ from .NET's by a few |
| 1.20 | version header once the key is accepted | low | CLOSED | `api/http.rs:201`; tests | |
| 1.21 | 503 while starting | low | CLOSED | `unarrd/src/main.rs` (`serve`): the listener is bound and the router serves before the command runtime starts; `api/http.rs` (`starting_up`) answers 503 until it has, after the authentication and the URL base's redirect in each group, as `StartingUpMiddleware.cs:24-35` (JSON `errorMessage` with the version header under `/api`, plain text elsewhere); test `while_starting_an_authenticated_request_is_503`. From the source: the original's window is too short to record | opening the database, and an import, still come before the listener |
| 1.22 | no-cache headers on the API | low | CLOSED | `api/http.rs:207`; test | |
| 1.23 | `/initialize.json` with `no-store` | medium | CLOSED | `api/routes/system.rs:144` | |
| 1.24 | `/api`, `/api/v1/…`, other paths that are not v3 | medium | CLOSED | `api/lib.rs` (`/api/{*rest}` behind the key), `api/routes/mod.rs` (`unknown_api_route`: 404 for `GET`, 405 `Allow: GET` for the rest, `/api/v3` too); test `tests/http.rs` (`what_no_action_takes_is_refused_as_the_originals_refuse_it`); recorded | |
| 1.25 | unknown v3 route without a key: 401 | low | CLOSED | `api/lib.rs:269`; test | |
| 1.27 | `If-Modified-Since` | low | NOT APPLICABLE | the original's never fires | |
| 1.29 | request body limit | medium | DELIBERATE | 64 MiB once the key is good (`api/lib.rs:231`, `:285`), where the original has none (202 to a `PUT` of 120 MiB); the login form keeps 2 MiB; test `tests/http.rs:427` | a body over 64 MiB: 413 |
| 1.30 | 406 for an `Accept` without JSON | low | CLOSED | `api/http.rs` (`negotiate`, in `finish` and `ping_answer`): once the action has run (a write is done), a JSON result `Accept` does not take is an empty 406 with the other headers, no `Location`; `*/*` anywhere, or no `Accept`, takes it; Radarr's formatter writes `text/json` for `text/json` and `text/*`; the exception handler's answers are not negotiated; test `an_accept_without_json_is_406_once_the_action_has_run`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | `/ping`'s 406 carries no cache headers, `/ping` being uncached by choice |
| 1.31 | `application/json; charset=utf-8` | low | CLOSED | `api/http.rs:89` | |
| 1.32 | `HEAD` | low | CLOSED | `api/lib.rs` (`open_takes_its_methods`, `pages_take_get_only`): `HEAD` finds an action on `/ping` alone; elsewhere, covers and content included, it is the 405 endpoint: 401 without the key under every method, a cookie not taken, 405 `Allow: GET` with it, kept a year outside the API; test `head_finds_no_action_but_on_ping`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | |
| 1.34 | `Server` header | low | CLOSED | `Server: Kestrel` on every answer, as recorded on both originals, by the maintainer's decision (20 November 2026): `api/http.rs` (`server_header`), the outermost layer (`api/lib.rs`), so the host filter's 400 and a panic's 500 carry it too; test `tests/http.rs` (`every_answer_names_kestrel_as_the_originals_do`) | |
| 1.35 | what applies without a restart | unsure | DELIBERATE | the URL base is read once where the originals read it once (`AppState::url_base`): the routes, the pages' links, the manifest and the tile description, the session cookie's path; `/initialize.json` and the login's two redirects read it on each request in both (`InitializeJsonController.cs`, `AuthenticationController.cs`); test `a_url_base_saved_from_the_ui_sends_no_request_round_in_circles` | allowed hosts and trusted networks apply as soon as they are saved, where the original waits for the restart: the maintainer's decision (20 November 2026, `docs/DESIGN.md`, section 22) |
| 2.1 | `/ping` | high | CLOSED | `api/lib.rs:326` | |
| 2.2 | the calendar feed | high | CLOSED | `api/routes/mod.rs:47` | |
| 2.3 | `GET /api` | low | CLOSED | `api/routes/system.rs:157` | |
| 2.4 | covers with the key | high | CLOSED | `api/routes/mod.rs:80`; live | |
| 2.8 | backup download headers | low | CLOSED | `api/routes/backup.rs` (`download`): `application/x-zip-compressed`, no `Content-Disposition`, kept a year as a static file, a missing one the 404 in problem details (recorded); `BackupFileMapper.cs`; unit test `types_are_aspnets_by_extension` | only a backup the list names is served, where the original serves any name its pattern takes in the folder |
| 2.9 | `/MediaCoverProxy/{hash}/{file}` | low | CLOSED | `api/routes/series_writes.rs` (`media_cover_proxy`): a name that does not end in `.jpg`, `.png` or `.gif` (with case) is the 404 in problem details before the hash is looked at, and the type is the name's (`MediaCoverProxyMapper.cs`); test `the_cover_proxy_reads_the_name_first`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | a name with a slash in it is no route here |
| 2.11 | SignalR over server-sent events and long polling | medium | DELIBERATE | WebSockets only (`api/routes/signalr.rs`); the negotiation still lists the three transports, as the original's does (golden `signalr-negotiate`). `docs/DESIGN.md` section 21.4 | a browser behind a proxy that does not pass WebSocket upgrades: no live updates, the proxy is to be configured to pass them |
| 2.12 | negotiate version | low | CLOSED | `api/routes/signalr.rs` (`negotiate`): `negotiateVersion=1` gets version 1 and a `connectionToken` beside the `connectionId`; test `tests/signalr.rs` (`negotiate_version_one_and_version_to_every_client`); recorded on both (`scripts/http-probe.py`, `lowhttp*`) | |
| 2.13 | `version` message to all clients | low | CLOSED | `api/routes/signalr.rs` (`serve`): the new connection's `version` goes out through the hub, to every client (`MessageHub.cs:57`); same test | |
| 3.2 | the key as a bearer token | medium | CLOSED | `api/auth.rs:26`; test | |
| 3.4 | cookie `Secure` and `Path` | medium | CLOSED | `api/ui.rs:330`; tests | |
| 3.5 | cookie name with a non-ASCII instance name | low | CLOSED | `api/state.rs` (`cookie_name_part`): decomposed, its combining marks dropped, then all but ASCII letters and digits; tests; recorded on both (`<App> Ünï Çafé` gives `<App>UniCafeAuth`) | a letter that does not decompose (`ß`, `ø`) is dropped; the original's table (Diacritical.Net) may map it: not recorded |
| 3.6 | session seven days, sliding | medium | CLOSED | `api/ui.rs:317`; test | |
| 3.8 | forms login when the method is None | low | CLOSED | `api/ui.rs` (`login`, `logout`): under None the form's redirect says `loginFailed=true`, sets no cookie and logs nothing; a logout logs nothing; test `the_login_form_fails_when_nothing_is_to_be_signed_into`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | |
| 3.9 | challenge under Forms | low | CLOSED | absolute address, escaped `returnUrl`, the base as asked (as before); `api/ui.rs` (`is_ajax_request`): `X-Requested-With: XMLHttpRequest`, header or query parameter, value with case, gets 401 with the same `Location`; test `an_xhr_is_told_401_instead_of_the_login_redirect`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | |
| 3.11 | Basic realm | low | CLOSED | `realm="Sonarr"`, the application's name whatever the instance's, as recorded (`api/ui.rs`); same test as 1.8 | |
| 3.15 | `TrustCgnatIpAddresses` | medium | CLOSED | `api/ui.rs:497`; test | |
| 3.16 | IPv6 site-local counts as local | low | CLOSED | `core/outbound.rs:208`; test | |
| 3.17 | `/favicon.ico` behind the login | low | CLOSED | in the pages' group (`api/lib.rs`); recorded outside a URL base, test as 1.10 | |
| 4.3 | `?h=` cache breaker on asset links | low | CLOSED | `api/ui.rs` (`rewrite_html`, `with_cache_breaker`): `HtmlMapperBase`'s pattern, the URL base and `?h=` with the file's MD5 in base64, `data-no-hash` honoured and removed; the index and login pages equal the recorded ones byte for byte; test `the_pages_asset_links_carry_cache_breakers` | |
| 4.5 | `manifest.json` placeholders | medium | CLOSED | `api/ui.rs:177`; golden `noauth-content-manifest`; test `tests/http.rs:379`, both personalities | its cache lifetime is row 4.9 |
| 4.6 | `browserconfig.xml` placeholder | low | CLOSED | same handler; golden `noauth-content-browserconfig`; same test | |
| 4.7 | `/robots.txt` | low | CLOSED | `api/ui.rs` (`spa`): `/robots.txt`, in this case, is `Content/robots.txt`, behind the login under Forms and Basic as recorded; test `what_no_mapper_takes_is_a_404_in_problem_details`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | crawlers of an instance behind a login meet the redirect, as with the original |
| 4.9 | cache headers on static files | low | CLOSED | `api/http.rs` (`cacheable`, `cache_headers`, `apply_cache_headers`): `CacheableSpecification` and `CacheHeaderMiddleware` after the authentication in each group, `?h=` on the API too; `Last-Modified` the emulated release's build time (`compat::emulated_build_time`); pages, sign-ins and the exception handler's answers keep theirs; test `static_files_are_kept_a_year_and_typed_as_aspnet_types_them`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | `/ping` as health checks ask it stays uncached by choice; a file is kept a year whatever Unarr's own release (the bundle follows the emulated one) |
| 4.10 | cache headers on HTML | low | CLOSED | `api/ui.rs:116` | |
| 4.11 | cache headers on `/MediaCover` | low | CLOSED | `api/ui.rs` (`media_cover`) in place of the file server: kept a year with the build's date, no `ETag`, no ranges, the folder in its own case, a missing cover the 404 in problem details; test `covers_are_static_files`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | the UI changes `?lastWrite=` when a cover changes, as with the original |
| 4.15 | source maps | low | DELIBERATE | dropped (`docs/DESIGN.md` section 11) | browser developer tools |
| 4.16 | files the original refuses | low | CLOSED | `api/ui.rs` (`spa`, `bundle_file`): only what `StaticResourceMapper` takes comes from the bundle; `/index.html`, the licence files, `/Content/index.html`: the 404 in problem details; `/login.html` the login page, behind the login; test `what_no_mapper_takes_is_a_404_in_problem_details`; recorded on both (`scripts/http-probe.py`, `lowhttp*`) | |
| 4.18 | `text/html` without charset | low | CLOSED | `api/ui.rs:119` | |
| 4.19 | type of other static files | unsure | CLOSED | `api/http.rs` (`content_type`): ASP.NET's table for the extensions of the bundles, covers and backups, `charset=utf-8` on plain text, `text/javascript` on Radarr's newer runtime; each extension the UIs have recorded on both; test `static_files_are_kept_a_year_and_typed_as_aspnet_types_them` | |
| 4.20 | 404 for a file-like path | low | CLOSED | `api/ui.rs` (`static_not_found`): problem details as recorded, indented in ASP.NET's order (RFC 7231 for Sonarr, 9110 for Radarr), `problem+json`, kept a year; same test | |

What the triage found against the report and the README:

- Seven medium rows of this report were never in the README's numbered
  list and were open or partial: 1.3, 1.8, 1.10, 1.24, 1.29, 2.11, 4.5
  (all but 2.11 since closed; 2.11 deliberate).
- Row 1.35 was unsure and is a real difference: a URL base saved from the
  UI is half applied until the restart.
- Row 1.13 understates: the release profile aborts on a panic, so the
  process ends. (Closed since, see above.)
- `docs/DESIGN.md` section 14 said `X-Forwarded-Proto` and
  `X-Forwarded-Host` are not read; both are, except by the host filter.
- Row 12 of the live-run table ("same") holds without a URL base only.

## Routes and UI calls (`3-4-routes.md`)

78 rows were MISSING, PARTIAL, IGNORED or UNSURE: 39 closed, 25 open, 4
deliberate, 10 not applicable. Static: nothing was run. `mod.rs`
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
| A | `/signalr/messages` over server-sent events and long polling | DELIBERATE | advertised (`signalr.rs:86`), only WebSocket served, by decision (`docs/DESIGN.md` section 21.4) | UI behind a proxy that blocks WebSockets |
| B | restart, shutdown, `series/import`, `history/failed/{id}`, blocklist delete and bulk, exclusions bulk, `PUT metadata/{id}` | CLOSED (8) | as Table A | UI |
| B | `GET /delayprofile/schema` | NOT APPLICABLE | dead UI code; the original has no such route | nobody |
| B | the iCal link's parameters | CLOSED | `feed.rs:159` reads `unmonitored`, `premieresOnly`, `asAllDay`, `tags`, `pastDays`, `futureDays` | UI, calendar apps |
| B | SignalR `wanted/missing`, `wanted/cutoff` updated | CLOSED (2) | `relay.rs:75`, `:79`: after a grab, an import and a file deleted, as `episode`; `golden-hub/signalr.json`, test `tests/contract.rs:1650` | UI, Wanted |
| B | SignalR `calendar` updated | NOT APPLICABLE | the original sends no such message: the calendar's controller names its resource `episode` (same recording) | nothing |
| B | SignalR `tag` sync | CLOSED | `settings_writes.rs:1073`, after a tag or an auto tagging rule is added, changed or removed; `golden-hub/signalr.json`, test `tests/contract.rs:1616` | UI, second tab |
| B | SignalR `rootfolder`, `qualitydefinition` | CLOSED (2) | `settings_writes.rs:1237` (`created`; the original sends nothing for a removal), `state.rs:302` (`sync` after `ResetQualityDefinitions`; the original sends nothing after a save); same recording and test | UI, second tab |
| B | SignalR `system/task` sync | CLOSED | `core/commands/executor.rs:760`, `state.rs:302`: after every command, for Sonarr as for Radarr; same recording and test | UI, System > Tasks |
| B | SignalR `command` sync | CLOSED | `state.rs:306`, once a `MessagingCleanup` the API asked for has completed; same recording and test | nothing visible |
| B | SignalR `metadata` updated | CLOSED | `providers.rs:408` | UI, second tab |
| B | commands `ClearLog`, `DeleteLogFiles`, `ResetQualityDefinitions`, `ResetApiKey` | CLOSED (4) | `core/commands/mod.rs:712-718`, `executor.rs:911-934`, `state.rs:535-553`; goldens for the first three, none for `ResetApiKey` | UI buttons |
| B | `GET /config/naming/examples` with the pending form | CLOSED | `settings.rs:155`; goldens `config-naming-examples-form`, `-invalid`, `-no-id` | UI, naming examples while typing |
| B | `GET /blocklist?protocols=` | CLOSED | `activity.rs:385`, `core/store/history.rs:237`; goldens `blocklist-protocol-*` (8) | UI, custom filter on Protocol |
| B | `PUT /series/{id}?moveFolder=true` | NOT APPLICABLE | parity: the original reads `moveFiles`, and so does `series_writes.rs:375` | nothing |
| B | SignalR transport fallback | DELIBERATE | the Table A row | as above |
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

39 rows were MISSING, PARTIAL or UNSURE: 31 closed, 4 open, 1 partial, 2
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
| (a) episode grabbed | no `episode` with `grabbed=true` | low | CLOSED | `core/downloads/grab.rs:161`, `api/relay.rs:79`; `golden-hub/signalr.json`, test `api/tests/contract.rs:1650` | |
| (a) episode imported: client refresh | an import outside the queue asks no client | low | CLOSED | `executor.rs:2301`, after a rescan (`:2290`) and a drop-folder import (`:2385`) that brought a file in; `golden-hub/command-after-rescan`, `command-after-downloaded-scan`, test `api/tests/contract.rs:1616` | |
| (a) health `CheckOn` triggers | checks re-run on some events only | low-medium | CLOSED | `core/health.rs:250` (the sets), `api/state.rs:347`, `settings_writes.rs:159`, `:1233`, `:1313`, `:1332`, `importlist_writes.rs:290`, `executor.rs:2156`, `:2216`; tests `core/health.rs:1276`, `api/tests/contract.rs:1397` | the checks Unarr does not have are not run: `RemovedSeriesCheck`, `ImportListStatusCheck`, `ApiKeyValidationCheck` |
| (a) root folder changed | no SignalR `rootfolder` | low | CLOSED | `settings_writes.rs:1237`: `created`; the original sends nothing for a removal; `golden-hub/signalr.json`, same test | |
| (a) indexer settings cache | equivalent not found | low | NOT APPLICABLE | settings are read from the database for every decision (`core/decision/lookup.rs:189`) | |
| (a) series edited | a type change queues no refresh | medium | CLOSED | `api/routes/series_writes.rs:427`; test `tests/contract.rs:1333` | |
| (a) series imported | no `POST /series/import` | medium | CLOSED | `api/routes/mod.rs:79` | |
| (a) scan skipped | no event when the root folder is missing or empty | medium | CLOSED | `core/mediafiles/scan.rs:233`; test `:509` | |
| (a) series scanned: media info | files without it never probed again | medium | CLOSED | `scan.rs:147`, `:344`; test `:407` | |
| (a) series scanned: file dates | not applied after a scan | low | CLOSED | `core/mediafiles/scan.rs` (`update_file_dates`, after each scan that ran); test `a_scan_dates_the_files_by_their_episodes`; recorded | |
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

18 rows were NOT APPLIED or PARTIAL in the detailed tables: 12 closed, 3
open, 1 deliberate, 2 not applicable.

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
| `FileDate` | import and rename only | medium | CLOSED | the events row above, and the dates compared and set as recorded (`core/mediafiles/approve.rs`, `change_file_date`) | |
| `EnableMediaInfo` | no backfill on rescan | low | CLOSED | `core/mediafiles/scan.rs:147` | |
| `UseScriptImport`, `ScriptImportPath` | not applied | medium | DELIBERATE | stored and shown (`settings_writes.rs:349`), not applied by decision (`docs/DESIGN.md` section 21.3); `ScriptImportCheck` warns while it is on (`core/health.rs`, test `script_import_on_is_said_to_be_ignored`) | whoever imports through a script gets Unarr's own move or hardlink, and a health warning that says so |
| `ImportExtraFiles`, `ExtraFileExtensions` | not applied | medium | CLOSED | `core/mediafiles/extras.rs:334` | |
| `UILanguage` | not applied | low | OPEN | `api/routes/system.rs:364` always serves the English dictionary, the only one in the tree | whoever picks another language: the interface stays English |
| proxy (8 settings) | not applied | high | CLOSED | `core/http.rs:71`, `core/outbound.rs:308`; test `core/http.rs:325` | `ProxyCheck` runs again five seconds after a save of the settings (`core/health.rs:250`) |
| `CertificateValidation` | not applied | medium | CLOSED | `core/outbound.rs:317`; test `:604` | |
| `TrustCgnatIpAddresses` | not applied | medium | CLOSED | `api/ui.rs:500`, `core/outbound.rs:227` | |

What the triage found against the reports and the README:

- Two medium findings of `1-settings.md` were never in the README's
  numbered list and were not closed: script import, and file dates after a
  series scan (the fix of `e0a81e4` was for movies; the series' since);
  script import is deliberate since. The `CheckOn` row,
  low-medium, was unchanged at the triage and is closed since.
- `docs/DESIGN.md` section 14, "Logs", said the debug and trace files
  follow `RUST_LOG` alone, while the saved Log Level applies at once.
- The same gap carries different severities in two reports (the recycling
  bin: high and medium; file dates: medium and low).
