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
Files off: findings 2, 4, 16 and 21 do not affect it and can wait. None
of the findings is fixed in this commit.
