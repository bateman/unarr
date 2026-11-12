# Audit against the original's source, November 2026

After phase 3b, two defects found on the first day of running Unarr in
place of Sonarr on the maintainer's NAS had the same shape: behaviour of
the original that is not in the JSON body of an API answer, so the
golden replay could not see it (`TrustedNetworks` stored but not applied;
a resized cover that does not exist answered with 404 instead of the full
cover). Six audits looked for the rest, comparing Unarr at `77a39ce` with
the Sonarr source it ports. `<sonarr>` in the reports is a checkout of
that source.

| Audit | Report | Examined | Gaps |
|-------|--------|----------|------|
| 1. Settings stored but not applied | [1-settings.md](1-settings.md) | 78 settings | 13 not applied, 2 partial |
| 2. The server outside the API bodies | [2-http.md](2-http.md) | 89 behaviours | 56 different or missing: 3 high, 14 medium, 39 low |
| 3. Routes the original declares | [3-4-routes.md](3-4-routes.md), table A | 280 routes | 48 missing, 7 of them harmless |
| 4. Requests the web UI makes | [3-4-routes.md](3-4-routes.md), table B | 242 calls | 23 missing, 3 parameters ignored |
| 5. The UI at several screen densities | [5-browser.md](5-browser.md) | 36 pages on 5 screens | none |
| 6. Event handlers, commands, housekeeping | [6-events.md](6-events.md) | 214 entries | 29 missing, 6 partial, none high |
| 7. Where each row stands now | [7-triage.md](7-triage.md) | 193 rows of reports 1, 2, 3, 4 and 6 | 78 open, 13 partial |

The search, grab, import and delete chains are complete. The gaps are at
the edges.

## How far to trust them

Audits 1, 2, 3, 4 and 6 are static comparisons: nothing was compiled or
run. A route counted as present has the same method and path, not
necessarily the same answer. Rows of audit 2 marked `[fw]` rest on the
default behaviour of the original's framework, which is in neither
repository, and were not tried against a running Sonarr. Who calls a route
(Bazarr, mobile clients) is from memory where the report marks it. The
high findings and the medium ones marked below were checked against
Unarr's code afterwards.

## The findings together, by severity

High (all checked):

1. `PUT config/host` does not store the authentication method, the user
   name and password or the API key; it answers as if it had.
2. The proxy settings are not applied: indexers and trackers are reached
   directly.
3. `GET /ping` does not exist; it falls to the UI's fallback.
4. `CleanUpRecycleBin` does nothing: the recycling bin is never emptied.
5. `GET /api/v3/mediacover/{id}/{file}` and
   `GET /feed/v3/calendar/sonarr.ics` do not exist.

Medium, actions of the UI that fail:

6. Removing one blocklist entry, or the selected ones (checked).
7. "Mark as failed" from the history.
8. Library Import (`POST /series/import`).
9. Restart and shutdown from the UI.
10. The commands `ClearLog`, `DeleteLogFiles`, `ResetQualityDefinitions`
    and `ResetApiKey` answer 500 (checked).
11. Saving Settings > Metadata answers 405 (checked).
12. The naming examples ignore the form while it is edited (checked).
13. Deleting the selected import list exclusions.

Medium, silent:

14. A series added under a root folder that is empty or not mounted never
    gets its post-add actions: the scan is skipped without the event that
    triggers them (checked).
15. `Housekeeping` does nothing.
16. Extra files in a release are not imported.
17. Files without media info are never probed again.
18. Deleting the last custom format does not reset the profiles' scores.
19. A blank start has no default quality profiles or definitions.
20. Changing a series' type does not queue a refresh.
21. Certificate validation cannot be turned off.
22. The log level saved from the UI and the `Sonarr__…` environment
    overrides are not read.
23. The session lasts one day from the login, where the original's is
    seven days and sliding; its cookie is never `Secure`.
24. `/initialize.json`, which holds the API key, is sent without
    `no-store`.
25. `TrustCgnatIpAddresses` is not applied.

Low: some sixty, in the reports: cache headers and compression, SignalR
messages for secondary pages, health checks not re-run after a save,
pending releases that never expire, `robots.txt`, the web app manifest,
the UI language, the blocklist's protocol filter.

## What the maintainer's installation uses

No proxy, no recycling bin, no self-signed certificates, Import Extra
Files off: findings 2, 4, 16 and 21 do not affect it and can wait.

## What has been fixed since

Each fix was made against goldens recorded from the original first
(`tests/fixtures/sonarr/golden/requests.txt`, the section the audit
opened), and the recording corrected two of the findings.

| Finding | State |
|---------|-------|
| 1. `PUT config/host` credentials | Fixed for the authentication method, the requirement, the user name and the password (same hash scheme as the original, so its users table stays readable by it). The API key part of the finding was wrong: the original does not change the key on a save either, it answers 202 and keeps the old one; only `ResetApiKey` changes it. Unarr now does the same. |
| 3. `GET /ping` | Fixed: anonymous, `{"status":"OK"}`, 500 when the database does not answer. |
| 6. Blocklist removal, one and bulk | Fixed. The original answers 200 for an id that is not there; the ids of new entries continue past the removed ones. |
| 7. "Mark as failed" | Fixed. |
| 9. Restart and shutdown | Fixed: the daemon stops as on a signal; a restart replaces the process with a fresh one. |
| 10. `ClearLog`, `DeleteLogFiles`, `DeleteUpdateLogFiles`, `ResetQualityDefinitions`, `ResetApiKey` | Fixed. |
| 11. Settings > Metadata save | Fixed. |
| 12. Naming examples follow the form | Fixed, and an example whose name does not parse back to the sample's episodes is left out, as the original does to mark the format invalid. |
| 14. Post-add actions under a missing or empty root folder | Fixed. |
| 15. `Housekeeping` | Fixed for what Unarr stores: orphaned blocklist entries, episode files, episodes, history and pending releases, pending releases waiting for a download client for more than fourteen days, status rows of providers that are gone, status times in the future, unused tags, then `VACUUM`. |
| 18. Scores after the last custom format | Fixed. |
| 2. Proxy | Fixed, HTTP and SOCKS, with the bypass list and the `ProxyCheck` health check. Tried against a local proxy; SOCKS only as far as the URL handed to the client. |
| 21. Certificate validation | Fixed: enabled, disabled for local addresses, disabled; this machine is always let through, as in the original. Tried against a local server with a self-signed certificate. |
| 22. Log level and environment overrides | Fixed. `RUST_LOG`, when set, still decides the level. `ConsoleLogLevel` and the syslog settings are not read. |
| 25. `TrustCgnatIpAddresses` | Fixed, read from `config.xml`, the saved settings and the environment. |
| 4. `CleanUpRecycleBin` | Fixed: files in the bin for longer than the configured days go, with the folders that leaves empty. |
| 16. Import Extra Files | Fixed: the listed extensions come in with a download (subtitles named by language and tags, anything else under the episode file's name). Beyond the finding: extras found by a scan are remembered, follow a rename and leave with the episode file, and the original's subtitle rows are imported. Metadata consumers stay undriven. |
| 17. Files without media info | Fixed: probed again after every scan of their series. |
| 5. `GET /api/v3/mediacover/…` and the calendar feed | Fixed. The feed is compared line by line with recordings of the original's. |
| 8. Library Import | Fixed: `POST /series/import` adds the series together, refuses them all when one does not validate and leaves out those already there, as recorded. |
| 13. Deleting the selected import list exclusions | Fixed. |
| 19. Blank start | Fixed: quality definitions, the six default quality profiles and the delay profile are put in at start when missing. |
| 20. Series type change | Fixed: the series is refreshed. |
| 23. Session length and `Secure` | Fixed: seven days from the last use, renewed past half its life; `Secure` when a trusted proxy says the request came over HTTPS. Sessions open at the upgrade end once: the signing key is now a random one kept in the database, not derived from the API key. |
| 24. `/initialize.json` without `no-store` | Fixed. |

A review of these fixes found more, fixed in turn: the recycling bin's
folder was not validated (a root folder set as the bin would have been
emptied by the clean-up), a blank API key from the environment opened
the API, an absurd range stopped the calendar feed's process, series
were added outside a transaction, a login could be switched on without
a user, and the scan remembered extras only when the series folder was
missing. Two of its findings describe what the original does and were
left: an imported extra replaces the file already at its name, and
"Mark as failed" on a row that is not a grab blocklists from that row.

Not recorded against the original, and so not changed: `PUT
config/naming` does not yet refuse a format whose sample name fails the
same parse-back check.

Still open: see [7-triage.md](7-triage.md), which read every row of the
four static reports against `main` on 12 November 2026. Of 193 rows, 83 are
closed, 78 open, 13 partial, 6 deliberate and 13 not applicable. "The low
ones" was not the whole of it: nine findings the reports rated medium were
never in the numbered list above and are open or partial.
