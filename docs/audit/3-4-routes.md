# Audit 3-4 — routes: Sonarr API v3 and web UI versus Unarr

Static, read-only. Sonarr tree: commit `cab419a` ("Bump version to 4.0.20"). Nothing was compiled or run.

## How to read this

- Sonarr paths are relative to `src/` (Table A; `Sonarr.Api.V3/` omitted) and to `frontend/src/` (Table B). Unarr paths are relative to `crates/unarr-api/src/` unless they start with `unarr-core/`.
- **Route match** = same method and same path shape, compared case-insensitively (Unarr lower-cases API paths, `lib.rs:38`). It says nothing about the response body.
- `[RestPutById]` is `HttpPut("{id:int?}")`, so each one is listed twice: with and without the id. Unarr serves the id-less form by rewriting the path from the body's id (`lib.rs:63-88`, `lib.rs:112-150`).
- `(via X)` = action inherited from a base class (`Sonarr.Http/REST/RestController.cs:49` `[RestGetById]`, `ProviderControllerBase.cs`, `Config/ConfigController.cs`, `Logs/LogFileControllerBase.cs`), counted once per subclass.
- **in goldens?** = number of lines of `tests/fixtures/sonarr/golden/requests.txt` whose method and path match that row (`mock` and `sleep` lines excluded).
- **who calls it**: `UI: …` is proven by Table B. Names with `*` (Bazarr*, Prowlarr*, Recyclarr*) are **from memory of those projects, not checked against their source in this audit** — treat as leads.
- Out of scope, listed as such: the update endpoints, Swagger/OpenAPI (not routed by the controllers; no row), Radarr/Lidarr (Unarr's other personalities; no row). Accepted limits (notifications other than Discord/Webhook/Plex, import lists other than Simkl, metadata writers, other download clients and indexers) were **not** used to excuse a missing schema/CRUD route.

## Counts

- **Table A**: 280 (method, path) rows — PRESENT 228, MISSING 48, PARTIAL 1, OUT OF SCOPE 3. Exercised by the goldens: 174.
- Of the MISSING rows in Table A, 7 are `GET …/{id}` actions that Sonarr itself answers with `NotImplementedException` (marked harmless).
- **Table B**: 242 rows — HANDLED 210, IGNORED PARAMETER 3, MISSING 23, OUT OF SCOPE 3, PARTIAL 1, UNSURE 2.

## Findings, most visible first

Missing routes the UI calls:

1. `DELETE /blocklist/{id}` and `DELETE /blocklist/bulk` — Activity > Blocklist remove buttons.
2. `POST /history/failed/{id}` — "Mark as failed" in the three history views.
3. `POST /series/import` — Library Import.
4. `POST /system/restart`, `POST /system/shutdown` — header menu, and the automatic restart after a backup restore.
5. `GET /feed/v3/calendar/Sonarr.ics` — the iCal link the Calendar page hands out.
6. `PUT /metadata/{id}` — Settings > Metadata save (405).
7. `DELETE /importlistexclusion/bulk` — delete selected exclusions.
8. `POST /command` with `ClearLog`, `DeleteLogFiles`, `ResetQualityDefinitions`, `ResetApiKey` — the route exists, the command name is unknown and answers 500.

Ignored parameters:

9. `GET /config/naming/examples` ignores the whole query — naming examples do not follow the form while typing.
10. `GET /blocklist?protocols=` — custom filter by protocol has no effect.

SignalR messages never broadcast: `calendar`, `wanted/missing`, `wanted/cutoff`, `tag`, `rootfolder`, `system/task`, `qualitydefinition`, `metadata`, and `command` with action `sync`. Low impact each (see Table B). ServerSentEvents/LongPolling are advertised but not served.

Missing routes the UI does not call:

- `GET /ping`, `HEAD /ping` — container healthchecks and uptime monitors get HTML or a login redirect.
- `GET /api/v3/mediacover/{seriesId}/{filename}` — the API-key-authenticated cover route (Bazarr* and mobile clients fetch posters through it).
- `GET /api`, `GET /system/routes`, `GET /system/routes/duplicate`, `GET /qualitydefinition/limits`, `GET /importlistexclusion/{id}`, `GET /config/*/{id}` (7), `GET /localization/{id}`, `GET /calendar/{id}`, `GET /wanted/missing/{id}`, `GET /wanted/cutoff/{id}`.
- The rest of the metadata provider surface (`POST`, `DELETE`, `bulk`, `test`, `testall`, `action/{name}`) and the deprecated `languageprofile` writes and schema.

Present in Unarr but in no golden (23): `autotagging/schema`, `autotagging/{id}`, `customfilter/{id}`, `customformat/schema`, `downloadclient/schema`, `indexer/schema`, `indexer/testall`, `localization/language`, `log/file/{filename}`, `metadata/schema`, `metadata/{id}`, `notification/bulk` (PUT, DELETE), `notification/schema`, `notification/action/{name}`, `delayprofile/{id}`, `language/{id}`, `releaseprofile/{id}`, `queue/grab/bulk`, `remotepathmapping/{id}`, `system/backup/restore/upload`, `system/task/{id}`, `tag/detail/{id}` — plus every non-versioned route except `/initialize.json` and the SignalR negotiate.

## Not verified

- Response bodies, status codes and validation of the PRESENT routes (only what the goldens cover is known to match).
- Whether `oauth.html`, `manifest.json` and `browserconfig.xml` are in the embedded UI bundle (`ui/sonarr/`), and the URL-base rewrite of the last two.
- The external callers marked `*`.
- Request **bodies** the UI sends (only command names were checked), and client-side sort/filter of the series index, which never reaches the server.


## Table A — server side

| METHOD path | Sonarr controller file:line | Unarr | in goldens? | who calls it |
|---|---|---|---|---|
| POST `/api/v3/autotagging` | AutoTagging/AutoTaggingController.cs:52 | PRESENT (routes/mod.rs:263) | yes (2) | UI: Auto tagging save (new) |
| PUT `/api/v3/autotagging` | AutoTagging/AutoTaggingController.cs:63 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/autotagging/{id}` | AutoTagging/AutoTaggingController.cs:63 | PRESENT (routes/mod.rs:268) | yes (1) | UI: Auto tagging save (edit) |
| GET `/api/v3/autotagging` | AutoTagging/AutoTaggingController.cs:76 | PRESENT (routes/mod.rs:263) | yes (2) | UI: Settings > Tags > Auto Tagging |
| DELETE `/api/v3/autotagging/{id}` | AutoTagging/AutoTaggingController.cs:83 | PRESENT (routes/mod.rs:268) | yes (1) | UI: Auto tagging delete |
| GET `/api/v3/autotagging/schema` | AutoTagging/AutoTaggingController.cs:89 | PRESENT (routes/mod.rs:267) | no | UI: Auto tagging condition picker |
| GET `/api/v3/autotagging/{id}` | Sonarr.Http/REST/RestController.cs:49 (via AutoTaggingController.cs) | PRESENT (routes/mod.rs:268) | no | nobody known |
| GET `/api/v3/blocklist` | Blocklist/BlocklistController.cs:28 | PRESENT (routes/mod.rs:163) | yes (4) | UI: Activity > Blocklist |
| DELETE `/api/v3/blocklist/{id}` | Blocklist/BlocklistController.cs:57 | MISSING | no | UI: Activity > Blocklist: remove one row (X button) |
| DELETE `/api/v3/blocklist/bulk` | Blocklist/BlocklistController.cs:63 | MISSING | no | UI: Activity > Blocklist: Remove Selected |
| GET `/api/v3/calendar` | Calendar/CalendarController.cs:32 | PRESENT (routes/mod.rs:150) | yes (4) | UI: Calendar |
| GET `/api/v3/calendar/{id}` | Sonarr.Http/REST/RestController.cs:49 (via CalendarController.cs) | MISSING | no | nobody known |
| POST `/api/v3/command` | Commands/CommandController.cs:51 | PRESENT (routes/mod.rs:323) | yes (27) | UI: Every "run" button (see command names below), Bazarr* |
| GET `/api/v3/command` | Commands/CommandController.cs:81 | PRESENT (routes/mod.rs:323) | yes (2) | UI: App boot, command polling |
| DELETE `/api/v3/command/{id}` | Commands/CommandController.cs:91 | PRESENT (routes/mod.rs:324) | yes (1) | UI: System > Tasks > Queue: cancel |
| GET `/api/v3/command/{id}` | Sonarr.Http/REST/RestController.cs:49 (via CommandController.cs) | PRESENT (routes/mod.rs:324) | yes (21) | nobody known |
| GET `/api/v3/config/downloadclient` | Config/ConfigController.cs:25 (via DownloadClientConfigController.cs) | PRESENT (routes/mod.rs:41) | yes (2) | UI: Settings > Download Clients options |
| PUT `/api/v3/config/downloadclient` | Config/ConfigController.cs:35 (via DownloadClientConfigController.cs) | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | UI: Download client options save |
| PUT `/api/v3/config/downloadclient/{id}` | Config/ConfigController.cs:35 (via DownloadClientConfigController.cs) | PRESENT (routes/mod.rs:42) | yes (1) | nobody known |
| GET `/api/v3/config/downloadclient/{id}` | Sonarr.Http/REST/RestController.cs:49 (via DownloadClientConfigController.cs) | MISSING | no | nobody known |
| GET `/api/v3/config/host` | Config/HostConfigController.cs:107 | PRESENT (routes/mod.rs:37) | yes (2) | UI: Settings > General |
| PUT `/api/v3/config/host` | Config/HostConfigController.cs:122 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | UI: Settings > General save |
| PUT `/api/v3/config/host/{id}` | Config/HostConfigController.cs:122 | PRESENT (routes/mod.rs:38) | yes (2) | nobody known |
| GET `/api/v3/config/host/{id}` | Sonarr.Http/REST/RestController.cs:49 (via HostConfigController.cs) | MISSING | no | nobody known |
| GET `/api/v3/config/importlist` | Config/ConfigController.cs:25 (via ImportListConfigController.cs) | PRESENT (routes/mod.rs:46) | yes (2) | UI: Settings > Import Lists options |
| PUT `/api/v3/config/importlist` | Config/ConfigController.cs:35 (via ImportListConfigController.cs) | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | UI: Import list options save |
| PUT `/api/v3/config/importlist/{id}` | Config/ConfigController.cs:35 (via ImportListConfigController.cs) | PRESENT (routes/mod.rs:47) | yes (2) | nobody known |
| GET `/api/v3/config/importlist/{id}` | Sonarr.Http/REST/RestController.cs:49 (via ImportListConfigController.cs) | MISSING | no | nobody known |
| GET `/api/v3/config/indexer` | Config/ConfigController.cs:25 (via IndexerConfigController.cs) | PRESENT (routes/mod.rs:171) | yes (2) | UI: Settings > Indexers options |
| PUT `/api/v3/config/indexer` | Config/ConfigController.cs:35 (via IndexerConfigController.cs) | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | UI: Indexer options save |
| PUT `/api/v3/config/indexer/{id}` | Config/ConfigController.cs:35 (via IndexerConfigController.cs) | PRESENT (routes/mod.rs:172) | yes (2) | nobody known |
| GET `/api/v3/config/indexer/{id}` | Sonarr.Http/REST/RestController.cs:49 (via IndexerConfigController.cs) | MISSING | no | nobody known |
| GET `/api/v3/config/mediamanagement` | Config/ConfigController.cs:25 (via MediaManagementConfigController.cs) | PRESENT (routes/mod.rs:166) | yes (2) | UI: Settings > Media Management, Recyclarr* |
| PUT `/api/v3/config/mediamanagement` | Config/ConfigController.cs:35 (via MediaManagementConfigController.cs) | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | UI: Settings > Media Management save |
| PUT `/api/v3/config/mediamanagement/{id}` | Config/ConfigController.cs:35 (via MediaManagementConfigController.cs) | PRESENT (routes/mod.rs:167) | yes (2) | Recyclarr* |
| GET `/api/v3/config/mediamanagement/{id}` | Sonarr.Http/REST/RestController.cs:49 (via MediaManagementConfigController.cs) | MISSING | no | nobody known |
| GET `/api/v3/config/naming` | Config/NamingConfigController.cs:47 | PRESENT (routes/mod.rs:164) | yes (2) | UI: Settings > Media Management naming, Recyclarr* |
| PUT `/api/v3/config/naming` | Config/NamingConfigController.cs:56 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | UI: Naming save |
| PUT `/api/v3/config/naming/{id}` | Config/NamingConfigController.cs:56 | PRESENT (routes/mod.rs:165) | yes (2) | Recyclarr* |
| GET `/api/v3/config/naming/examples` | Config/NamingConfigController.cs:67 | PRESENT (routes/mod.rs:173) | yes (1) | UI: Naming examples (see the ignored parameters below) |
| GET `/api/v3/config/naming/{id}` | Sonarr.Http/REST/RestController.cs:49 (via NamingConfigController.cs) | MISSING | no | nobody known |
| PUT `/api/v3/config/ui` | Config/UiConfigController.cs:34 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | yes (2) | UI: Settings > UI save |
| PUT `/api/v3/config/ui/{id}` | Config/UiConfigController.cs:34 | PRESENT (routes/mod.rs:40) | yes (1) | nobody known |
| GET `/api/v3/config/ui` | Config/ConfigController.cs:25 (via UiConfigController.cs) | PRESENT (routes/mod.rs:39) | yes (2) | UI: App boot, Settings > UI |
| GET `/api/v3/config/ui/{id}` | Sonarr.Http/REST/RestController.cs:49 (via UiConfigController.cs) | MISSING | no | nobody known |
| GET `/api/v3/customfilter` | CustomFilters/CustomFilterController.cs:25 | PRESENT (routes/mod.rs:311) | yes (2) | UI: App boot |
| POST `/api/v3/customfilter` | CustomFilters/CustomFilterController.cs:32 | PRESENT (routes/mod.rs:311) | yes (1) | UI: Custom filter save (new) |
| PUT `/api/v3/customfilter` | CustomFilters/CustomFilterController.cs:41 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/customfilter/{id}` | CustomFilters/CustomFilterController.cs:41 | PRESENT (routes/mod.rs:315) | yes (1) | UI: Custom filter save (edit) |
| DELETE `/api/v3/customfilter/{id}` | CustomFilters/CustomFilterController.cs:49 | PRESENT (routes/mod.rs:315) | yes (1) | UI: Custom filter delete |
| GET `/api/v3/customfilter/{id}` | Sonarr.Http/REST/RestController.cs:49 (via CustomFilterController.cs) | PRESENT (routes/mod.rs:315) | no | nobody known |
| GET `/api/v3/customformat` | CustomFormats/CustomFormatController.cs:50 | PRESENT (routes/mod.rs:111) | yes (2) | UI: Settings > Custom Formats, Recyclarr* |
| POST `/api/v3/customformat` | CustomFormats/CustomFormatController.cs:57 | PRESENT (routes/mod.rs:111) | yes (4) | UI: Custom format save (new), Recyclarr* |
| PUT `/api/v3/customformat` | CustomFormats/CustomFormatController.cs:68 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/customformat/{id}` | CustomFormats/CustomFormatController.cs:68 | PRESENT (routes/mod.rs:120) | yes (1) | UI: Custom format save (edit), Recyclarr* |
| PUT `/api/v3/customformat/bulk` | CustomFormats/CustomFormatController.cs:81 | PRESENT (routes/mod.rs:115) | yes (1) | UI: Manage custom formats: edit |
| DELETE `/api/v3/customformat/{id}` | CustomFormats/CustomFormatController.cs:103 | PRESENT (routes/mod.rs:120) | yes (1) | UI: Custom format delete, Recyclarr* |
| DELETE `/api/v3/customformat/bulk` | CustomFormats/CustomFormatController.cs:109 | PRESENT (routes/mod.rs:115) | yes (1) | UI: Manage custom formats: delete |
| GET `/api/v3/customformat/schema` | CustomFormats/CustomFormatController.cs:118 | PRESENT (routes/mod.rs:274) | no | UI: Custom format condition picker |
| GET `/api/v3/customformat/{id}` | Sonarr.Http/REST/RestController.cs:49 (via CustomFormatController.cs) | PRESENT (routes/mod.rs:120) | yes (4) | nobody known |
| GET `/api/v3/diskspace` | DiskSpace/DiskSpaceController.cs:18 | PRESENT (routes/mod.rs:126) | yes (1) | UI: System > Status |
| GET `/api/v3/downloadclient` | ProviderControllerBase.cs:60 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:191) | yes (3) | UI: Settings > Download Clients |
| POST `/api/v3/downloadclient` | ProviderControllerBase.cs:78 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:191) | yes (11) | UI: Download client save (new) |
| PUT `/api/v3/downloadclient` | ProviderControllerBase.cs:95 (via DownloadClientController.cs) | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/downloadclient/{id}` | ProviderControllerBase.cs:95 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:206) | yes (3) | UI: Download client save (edit) |
| PUT `/api/v3/downloadclient/bulk` | ProviderControllerBase.cs:127 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:202) | yes (3) | UI: Manage clients: edit |
| DELETE `/api/v3/downloadclient/{id}` | ProviderControllerBase.cs:180 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:206) | yes (2) | UI: Download client delete |
| DELETE `/api/v3/downloadclient/bulk` | ProviderControllerBase.cs:188 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:202) | yes (1) | UI: Manage clients: delete |
| GET `/api/v3/downloadclient/schema` | ProviderControllerBase.cs:197 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:195) | no | UI: Add download client |
| POST `/api/v3/downloadclient/test` | ProviderControllerBase.cs:221 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:196) | yes (21) | UI: Download client test |
| POST `/api/v3/downloadclient/testall` | ProviderControllerBase.cs:233 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:197) | yes (1) | UI: Test all download clients |
| POST `/api/v3/downloadclient/action/{name}` | ProviderControllerBase.cs:260 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:198) | yes (1) | UI: Download client dynamic selects |
| GET `/api/v3/downloadclient/{id}` | Sonarr.Http/REST/RestController.cs:49 (via DownloadClientController.cs) | PRESENT (routes/mod.rs:206) | yes (2) | nobody known |
| GET `/api/v3/episodefile` | EpisodeFiles/EpisodeFileController.cs:58 | PRESENT (routes/mod.rs:133) | yes (14) | UI: Series details, Calendar, Cutoff Unmet, Bazarr* |
| PUT `/api/v3/episodefile` | EpisodeFiles/EpisodeFileController.cs:91 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/episodefile/{id}` | EpisodeFiles/EpisodeFileController.cs:91 | PRESENT (routes/mod.rs:139) | yes (1) | nobody known |
| PUT `/api/v3/episodefile/editor` | EpisodeFiles/EpisodeFileController.cs:112 | PRESENT (routes/mod.rs:134) | yes (2) | nobody known |
| DELETE `/api/v3/episodefile/{id}` | EpisodeFiles/EpisodeFileController.cs:148 | PRESENT (routes/mod.rs:139) | yes (2) | UI: Delete episode file |
| DELETE `/api/v3/episodefile/bulk` | EpisodeFiles/EpisodeFileController.cs:163 | PRESENT (routes/mod.rs:135) | yes (1) | UI: Manage episodes: delete selected |
| PUT `/api/v3/episodefile/bulk` | EpisodeFiles/EpisodeFileController.cs:178 | PRESENT (routes/mod.rs:135) | yes (1) | UI: Manage episodes: edit selected |
| GET `/api/v3/episodefile/{id}` | Sonarr.Http/REST/RestController.cs:49 (via EpisodeFileController.cs) | PRESENT (routes/mod.rs:139) | yes (5) | UI: Episode details modal, Bazarr* |
| GET `/api/v3/episode` | Episodes/EpisodeController.cs:26 | PRESENT (routes/mod.rs:127) | yes (18) | UI: Series details, Queue, History, manual import, Bazarr* |
| PUT `/api/v3/episode` | Episodes/EpisodeController.cs:51 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/episode/{id}` | Episodes/EpisodeController.cs:51 | PRESENT (routes/mod.rs:129) | yes (1) | UI: Episode monitor toggle |
| PUT `/api/v3/episode/monitor` | Episodes/EpisodeController.cs:62 | PRESENT (routes/mod.rs:128) | yes (1) | UI: Season/bulk monitor toggle |
| GET `/api/v3/episode/{id}` | Sonarr.Http/REST/RestController.cs:49 (via EpisodeController.cs) | PRESENT (routes/mod.rs:129) | yes (7) | Bazarr* |
| GET `/api/v3/rename` | Episodes/RenameEpisodeController.cs:18 | PRESENT (routes/mod.rs:149) | yes (7) | UI: Organize/rename preview |
| GET `/api/v3/filesystem` | FileSystem/FileSystemController.cs:28 | PRESENT (routes/mod.rs:63) | yes (3) | UI: File browser, path inputs |
| GET `/api/v3/filesystem/type` | FileSystem/FileSystemController.cs:35 | PRESENT (routes/mod.rs:64) | yes (1) | nobody known |
| GET `/api/v3/filesystem/mediafiles` | FileSystem/FileSystemController.cs:48 | PRESENT (routes/mod.rs:65) | yes (1) | nobody known |
| GET `/api/v3/health` | Health/HealthController.cs:36 | PRESENT (routes/mod.rs:36) | yes (3) | UI: System > Status, sidebar badge |
| GET `/api/v3/health/{id}` | Sonarr.Http/REST/RestController.cs:49 (via HealthController.cs) | MISSING (harmless: Sonarr's own action throws NotImplementedException) | no | nobody known |
| GET `/api/v3/history` | History/HistoryController.cs:63 | PRESENT (routes/mod.rs:153) | yes (17) | UI: Activity > History, episode History tab, Bazarr* |
| GET `/api/v3/history/since` | History/HistoryController.cs:100 | PRESENT (routes/mod.rs:154) | yes (1) | nobody known |
| GET `/api/v3/history/series` | History/HistoryController.cs:107 | PRESENT (routes/mod.rs:155) | yes (3) | UI: Series/season History modal |
| POST `/api/v3/history/failed/{id}` | History/HistoryController.cs:131 | MISSING | no | UI: "Mark as failed" on a grabbed row: Activity > History, episode History tab, series/season History modal |
| GET `/api/v3/importlist` | ProviderControllerBase.cs:60 (via ImportListController.cs) | PRESENT (routes/mod.rs:230) | yes (3) | UI: Settings > Import Lists |
| POST `/api/v3/importlist` | ProviderControllerBase.cs:78 (via ImportListController.cs) | PRESENT (routes/mod.rs:230) | yes (6) | UI: Import list save (new) |
| PUT `/api/v3/importlist` | ProviderControllerBase.cs:95 (via ImportListController.cs) | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/importlist/{id}` | ProviderControllerBase.cs:95 (via ImportListController.cs) | PRESENT (routes/mod.rs:242) | yes (3) | UI: Import list save (edit) |
| PUT `/api/v3/importlist/bulk` | ProviderControllerBase.cs:127 (via ImportListController.cs) | PRESENT (routes/mod.rs:238) | yes (2) | UI: Manage lists: edit |
| DELETE `/api/v3/importlist/{id}` | ProviderControllerBase.cs:180 (via ImportListController.cs) | PRESENT (routes/mod.rs:242) | yes (2) | UI: Import list delete |
| DELETE `/api/v3/importlist/bulk` | ProviderControllerBase.cs:188 (via ImportListController.cs) | PRESENT (routes/mod.rs:238) | yes (1) | UI: Manage lists: delete |
| GET `/api/v3/importlist/schema` | ProviderControllerBase.cs:197 (via ImportListController.cs) | PRESENT (routes/mod.rs:234) | yes (1) | UI: Add import list |
| POST `/api/v3/importlist/test` | ProviderControllerBase.cs:221 (via ImportListController.cs) | PRESENT (routes/mod.rs:235) | yes (4) | UI: Import list test |
| POST `/api/v3/importlist/testall` | ProviderControllerBase.cs:233 (via ImportListController.cs) | PRESENT (routes/mod.rs:236) | yes (1) | UI: Test all lists |
| POST `/api/v3/importlist/action/{name}` | ProviderControllerBase.cs:260 (via ImportListController.cs) | PRESENT (routes/mod.rs:237) | yes (2) | UI: Import list OAuth, dynamic selects |
| GET `/api/v3/importlist/{id}` | Sonarr.Http/REST/RestController.cs:49 (via ImportListController.cs) | PRESENT (routes/mod.rs:242) | yes (2) | nobody known |
| GET `/api/v3/importlistexclusion` | ImportLists/ImportListExclusionController.cs:37 | PRESENT (routes/mod.rs:251) | yes (4) | nobody known |
| GET `/api/v3/importlistexclusion/paged` | ImportLists/ImportListExclusionController.cs:45 | PRESENT (routes/mod.rs:255) | yes (8) | UI: Import list exclusions table |
| POST `/api/v3/importlistexclusion` | ImportLists/ImportListExclusionController.cs:63 | PRESENT (routes/mod.rs:251) | yes (3) | UI: Exclusion save (new) |
| PUT `/api/v3/importlistexclusion` | ImportLists/ImportListExclusionController.cs:72 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/importlistexclusion/{id}` | ImportLists/ImportListExclusionController.cs:72 | PRESENT (routes/mod.rs:259) | yes (1) | UI: Exclusion save (edit) |
| DELETE `/api/v3/importlistexclusion/{id}` | ImportLists/ImportListExclusionController.cs:80 | PRESENT (routes/mod.rs:259) | yes (1) | UI: Exclusion delete |
| DELETE `/api/v3/importlistexclusion/bulk` | ImportLists/ImportListExclusionController.cs:86 | MISSING | no | UI: Settings > Import Lists > exclusions: delete selected |
| GET `/api/v3/importlistexclusion/{id}` | Sonarr.Http/REST/RestController.cs:49 (via ImportListExclusionController.cs) | MISSING | no | nobody known |
| GET `/api/v3/indexer` | ProviderControllerBase.cs:60 (via IndexerController.cs) | PRESENT (routes/mod.rs:176) | yes (3) | UI: Settings > Indexers, Prowlarr* |
| POST `/api/v3/indexer` | ProviderControllerBase.cs:78 (via IndexerController.cs) | PRESENT (routes/mod.rs:176) | yes (6) | UI: Indexer save (new), Prowlarr* |
| PUT `/api/v3/indexer` | ProviderControllerBase.cs:95 (via IndexerController.cs) | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/indexer/{id}` | ProviderControllerBase.cs:95 (via IndexerController.cs) | PRESENT (routes/mod.rs:185) | yes (2) | UI: Indexer save (edit), Prowlarr* |
| PUT `/api/v3/indexer/bulk` | ProviderControllerBase.cs:127 (via IndexerController.cs) | PRESENT (routes/mod.rs:181) | yes (2) | UI: Manage indexers: edit |
| DELETE `/api/v3/indexer/{id}` | ProviderControllerBase.cs:180 (via IndexerController.cs) | PRESENT (routes/mod.rs:185) | yes (2) | UI: Indexer delete, Prowlarr* |
| DELETE `/api/v3/indexer/bulk` | ProviderControllerBase.cs:188 (via IndexerController.cs) | PRESENT (routes/mod.rs:181) | yes (1) | UI: Manage indexers: delete |
| GET `/api/v3/indexer/schema` | ProviderControllerBase.cs:197 (via IndexerController.cs) | PRESENT (routes/mod.rs:177) | no | UI: Add indexer, Prowlarr* |
| POST `/api/v3/indexer/test` | ProviderControllerBase.cs:221 (via IndexerController.cs) | PRESENT (routes/mod.rs:178) | yes (5) | UI: Indexer test, Prowlarr* |
| POST `/api/v3/indexer/testall` | ProviderControllerBase.cs:233 (via IndexerController.cs) | PRESENT (routes/mod.rs:179) | no | UI: Test all indexers |
| POST `/api/v3/indexer/action/{name}` | ProviderControllerBase.cs:260 (via IndexerController.cs) | PRESENT (routes/mod.rs:180) | yes (2) | UI: Indexer dynamic selects, captcha |
| GET `/api/v3/indexer/{id}` | Sonarr.Http/REST/RestController.cs:49 (via IndexerController.cs) | PRESENT (routes/mod.rs:185) | yes (4) | Prowlarr* |
| GET `/api/v3/indexerflag` | Indexers/IndexerFlagController.cs:13 | PRESENT (routes/mod.rs:34) | yes (1) | UI: App boot |
| POST `/api/v3/release` | Indexers/ReleaseController.cs:69 | PRESENT (routes/mod.rs:289) | yes (11) | UI: Interactive search: grab |
| GET `/api/v3/release` | Indexers/ReleaseController.cs:171 | PRESENT (routes/mod.rs:289) | yes (6) | UI: Interactive search |
| GET `/api/v3/release/{id}` | Sonarr.Http/REST/RestController.cs:49 (via ReleaseController.cs) | MISSING (harmless: Sonarr's own action throws NotImplementedException) | no | nobody known |
| POST `/api/v3/release/push` | Indexers/ReleasePushController.cs:50 | PRESENT (routes/mod.rs:290) | yes (3) | nobody known |
| GET `/api/v3/release/push/{id}` | Sonarr.Http/REST/RestController.cs:49 (via ReleasePushController.cs) | MISSING (harmless: Sonarr's own action throws NotImplementedException) | no | nobody known |
| GET `/api/v3/localization` | Localization/LocalizationController.cs:23 | PRESENT (routes/mod.rs:337) | yes (1) | UI: App boot (all strings) |
| GET `/api/v3/localization/language` | Localization/LocalizationController.cs:30 | PRESENT (routes/mod.rs:338) | no | UI: Language names |
| GET `/api/v3/localization/{id}` | Sonarr.Http/REST/RestController.cs:49 (via LocalizationController.cs) | MISSING | no | nobody known |
| GET `/api/v3/log` | Logs/LogController.cs:24 | PRESENT (routes/mod.rs:325) | yes (1) | UI: System > Events |
| GET `/api/v3/log/file` | Logs/LogFileControllerBase.cs:28 (via LogFileController.cs) | PRESENT (routes/mod.rs:326) | yes (1) | UI: System > Log Files |
| GET `/api/v3/log/file/{filename}` | Logs/LogFileControllerBase.cs:54 (via LogFileController.cs) | PRESENT (routes/mod.rs:328) | no | nobody known |
| GET `/api/v3/log/file/update` | Logs/LogFileControllerBase.cs:28 (via UpdateLogFileController.cs) | PRESENT (routes/mod.rs:327) | yes (1) | UI: System > Log Files > Updater log files |
| GET `/api/v3/log/file/update/{filename}` | Logs/LogFileControllerBase.cs:54 (via UpdateLogFileController.cs) | OUT OF SCOPE (update; no route) | no | nobody known |
| GET `/api/v3/manualimport` | ManualImport/ManualImportController.cs:24 | PRESENT (routes/mod.rs:145) | yes (7) | UI: Manual import |
| POST `/api/v3/manualimport` | ManualImport/ManualImportController.cs:36 | PRESENT (routes/mod.rs:145) | yes (2) | UI: Manual import: reprocess |
| GET `/api/v3/mediacover/{seriesId}/{filename}` | MediaCovers/MediaCoverController.cs:28 | MISSING | no | Bazarr* |
| GET `/api/v3/metadata` | ProviderControllerBase.cs:60 (via MetadataController.cs) | PRESENT (routes/mod.rs:248) | yes (1) | UI: Settings > Metadata |
| POST `/api/v3/metadata` | ProviderControllerBase.cs:78 (via MetadataController.cs) | MISSING | no | nobody known |
| PUT `/api/v3/metadata` | ProviderControllerBase.cs:95 (via MetadataController.cs) | MISSING | no | nobody known |
| PUT `/api/v3/metadata/{id}` | ProviderControllerBase.cs:95 (via MetadataController.cs) | MISSING | no | UI: Settings > Metadata: save a consumer (Kodi/Roksbox/WDTV/Plex) |
| PUT `/api/v3/metadata/bulk` | ProviderControllerBase.cs:127 (via MetadataController.cs) | MISSING | no | nobody known |
| DELETE `/api/v3/metadata/{id}` | ProviderControllerBase.cs:180 (via MetadataController.cs) | MISSING | no | nobody known |
| DELETE `/api/v3/metadata/bulk` | ProviderControllerBase.cs:188 (via MetadataController.cs) | MISSING | no | nobody known |
| GET `/api/v3/metadata/schema` | ProviderControllerBase.cs:197 (via MetadataController.cs) | PRESENT (routes/mod.rs:249) | no | nobody known |
| POST `/api/v3/metadata/test` | ProviderControllerBase.cs:221 (via MetadataController.cs) | MISSING | no | nobody known |
| POST `/api/v3/metadata/testall` | ProviderControllerBase.cs:233 (via MetadataController.cs) | MISSING | no | nobody known |
| POST `/api/v3/metadata/action/{name}` | ProviderControllerBase.cs:260 (via MetadataController.cs) | MISSING | no | nobody known |
| GET `/api/v3/metadata/{id}` | Sonarr.Http/REST/RestController.cs:49 (via MetadataController.cs) | PRESENT (routes/mod.rs:250) | no | nobody known |
| GET `/api/v3/notification` | ProviderControllerBase.cs:60 (via NotificationController.cs) | PRESENT (routes/mod.rs:212) | yes (3) | UI: Settings > Connect |
| POST `/api/v3/notification` | ProviderControllerBase.cs:78 (via NotificationController.cs) | PRESENT (routes/mod.rs:212) | yes (4) | UI: Connection save (new) |
| PUT `/api/v3/notification` | ProviderControllerBase.cs:95 (via NotificationController.cs) | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/notification/{id}` | ProviderControllerBase.cs:95 (via NotificationController.cs) | PRESENT (routes/mod.rs:224) | yes (2) | UI: Connection save (edit) |
| PUT `/api/v3/notification/bulk` | ProviderControllerBase.cs:127 (via NotificationController.cs) | PRESENT (routes/mod.rs:220) | no | nobody known |
| DELETE `/api/v3/notification/{id}` | ProviderControllerBase.cs:180 (via NotificationController.cs) | PRESENT (routes/mod.rs:224) | yes (2) | UI: Connection delete |
| DELETE `/api/v3/notification/bulk` | ProviderControllerBase.cs:188 (via NotificationController.cs) | PRESENT (routes/mod.rs:220) | no | nobody known |
| GET `/api/v3/notification/schema` | ProviderControllerBase.cs:197 (via NotificationController.cs) | PRESENT (routes/mod.rs:216) | no | UI: Add connection |
| POST `/api/v3/notification/test` | ProviderControllerBase.cs:221 (via NotificationController.cs) | PRESENT (routes/mod.rs:217) | yes (6) | UI: Connection test |
| POST `/api/v3/notification/testall` | ProviderControllerBase.cs:233 (via NotificationController.cs) | PRESENT (routes/mod.rs:218) | yes (1) | nobody known |
| POST `/api/v3/notification/action/{name}` | ProviderControllerBase.cs:260 (via NotificationController.cs) | PRESENT (routes/mod.rs:219) | no | UI: Connection devices/OAuth |
| GET `/api/v3/notification/{id}` | Sonarr.Http/REST/RestController.cs:49 (via NotificationController.cs) | PRESENT (routes/mod.rs:224) | yes (2) | nobody known |
| GET `/api/v3/parse` | Parse/ParseController.cs:29 | PRESENT (routes/mod.rs:62) | yes (5) | UI: Settings > Media Management > Parse test (toolbar) |
| POST `/api/v3/delayprofile` | Profiles/Delay/DelayProfileController.cs:36 | PRESENT (routes/mod.rs:275) | yes (3) | UI: Delay profile save (new) |
| DELETE `/api/v3/delayprofile/{id}` | Profiles/Delay/DelayProfileController.cs:46 | PRESENT (routes/mod.rs:283) | yes (2) | UI: Delay profile delete |
| PUT `/api/v3/delayprofile` | Profiles/Delay/DelayProfileController.cs:57 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/delayprofile/{id}` | Profiles/Delay/DelayProfileController.cs:57 | PRESENT (routes/mod.rs:283) | yes (1) | UI: Delay profile save (edit) |
| GET `/api/v3/delayprofile` | Profiles/Delay/DelayProfileController.cs:71 | PRESENT (routes/mod.rs:275) | yes (3) | UI: Settings > Profiles |
| PUT `/api/v3/delayprofile/reorder/{id}` | Profiles/Delay/DelayProfileController.cs:78 | PRESENT (routes/mod.rs:279) | yes (1) | UI: Delay profile drag reorder |
| GET `/api/v3/delayprofile/{id}` | Sonarr.Http/REST/RestController.cs:49 (via DelayProfileController.cs) | PRESENT (routes/mod.rs:283) | no | nobody known |
| GET `/api/v3/language` | Profiles/Languages/LanguageController.cs:24 | PRESENT (routes/mod.rs:86) | yes (1) | UI: App boot |
| GET `/api/v3/language/{id}` | Sonarr.Http/REST/RestController.cs:49 (via LanguageController.cs) | PRESENT (routes/mod.rs:87) | no | nobody known |
| POST `/api/v3/languageprofile` | Profiles/Languages/LanguageProfileController.cs:15 | MISSING | no | nobody known |
| DELETE `/api/v3/languageprofile/{id}` | Profiles/Languages/LanguageProfileController.cs:23 | MISSING | no | nobody known |
| PUT `/api/v3/languageprofile` | Profiles/Languages/LanguageProfileController.cs:28 | MISSING | no | nobody known |
| PUT `/api/v3/languageprofile/{id}` | Profiles/Languages/LanguageProfileController.cs:28 | MISSING | no | nobody known |
| GET `/api/v3/languageprofile/{id}` | Profiles/Languages/LanguageProfileController.cs:36 | PRESENT (routes/mod.rs:175) | yes (1) | nobody known |
| GET `/api/v3/languageprofile` | Profiles/Languages/LanguageProfileController.cs:57 | PRESENT (routes/mod.rs:174) | yes (1) | Bazarr* |
| GET `/api/v3/languageprofile/schema` | Profiles/Languages/LanguageProfileSchemaController.cs:14 | MISSING | no | nobody known |
| GET `/api/v3/languageprofile/schema/{id}` | Sonarr.Http/REST/RestController.cs:49 (via LanguageProfileSchemaController.cs) | MISSING (harmless: Sonarr's own action throws NotImplementedException) | no | nobody known |
| POST `/api/v3/qualityprofile` | Profiles/Quality/QualityProfileController.cs:46 | PRESENT (routes/mod.rs:97) | yes (2) | UI: Quality profile save (new), Recyclarr* |
| DELETE `/api/v3/qualityprofile/{id}` | Profiles/Quality/QualityProfileController.cs:55 | PRESENT (routes/mod.rs:105) | yes (2) | UI: Quality profile delete |
| PUT `/api/v3/qualityprofile` | Profiles/Quality/QualityProfileController.cs:61 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/qualityprofile/{id}` | Profiles/Quality/QualityProfileController.cs:61 | PRESENT (routes/mod.rs:105) | yes (1) | UI: Quality profile save (edit), Recyclarr* |
| GET `/api/v3/qualityprofile` | Profiles/Quality/QualityProfileController.cs:77 | PRESENT (routes/mod.rs:97) | yes (2) | UI: App boot, Settings > Profiles, Bazarr*, Recyclarr* |
| GET `/api/v3/qualityprofile/{id}` | Sonarr.Http/REST/RestController.cs:49 (via QualityProfileController.cs) | PRESENT (routes/mod.rs:105) | yes (4) | nobody known |
| GET `/api/v3/qualityprofile/schema` | Profiles/Quality/QualityProfileSchemaController.cs:17 | PRESENT (routes/mod.rs:101) | yes (1) | UI: New quality profile, Recyclarr* |
| POST `/api/v3/releaseprofile` | Profiles/Release/ReleaseProfileController.cs:49 | PRESENT (routes/mod.rs:291) | yes (2) | UI: Release profile save (new) |
| DELETE `/api/v3/releaseprofile/{id}` | Profiles/Release/ReleaseProfileController.cs:57 | PRESENT (routes/mod.rs:295) | yes (1) | UI: Release profile delete |
| PUT `/api/v3/releaseprofile` | Profiles/Release/ReleaseProfileController.cs:63 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/releaseprofile/{id}` | Profiles/Release/ReleaseProfileController.cs:63 | PRESENT (routes/mod.rs:295) | yes (1) | UI: Release profile save (edit) |
| GET `/api/v3/releaseprofile` | Profiles/Release/ReleaseProfileController.cs:78 | PRESENT (routes/mod.rs:291) | yes (2) | UI: Settings > Profiles |
| GET `/api/v3/releaseprofile/{id}` | Sonarr.Http/REST/RestController.cs:49 (via ReleaseProfileController.cs) | PRESENT (routes/mod.rs:295) | no | nobody known |
| PUT `/api/v3/qualitydefinition` | Qualities/QualityDefinitionController.cs:32 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/qualitydefinition/{id}` | Qualities/QualityDefinitionController.cs:32 | PRESENT (routes/mod.rs:93) | yes (1) | nobody known |
| GET `/api/v3/qualitydefinition` | Qualities/QualityDefinitionController.cs:45 | PRESENT (routes/mod.rs:88) | yes (2) | UI: Settings > Quality, Recyclarr* |
| PUT `/api/v3/qualitydefinition/update` | Qualities/QualityDefinitionController.cs:51 | PRESENT (routes/mod.rs:89) | yes (1) | UI: Settings > Quality save, Recyclarr* |
| GET `/api/v3/qualitydefinition/limits` | Qualities/QualityDefinitionController.cs:63 | MISSING | no | nobody known |
| GET `/api/v3/qualitydefinition/{id}` | Sonarr.Http/REST/RestController.cs:49 (via QualityDefinitionController.cs) | PRESENT (routes/mod.rs:93) | yes (1) | nobody known |
| POST `/api/v3/queue/grab/{id}` | Queue/QueueActionController.cs:23 | PRESENT (routes/mod.rs:161) | yes (1) | UI: Queue: grab pending release |
| POST `/api/v3/queue/grab/bulk` | Queue/QueueActionController.cs:38 | PRESENT (routes/mod.rs:160) | no | UI: Queue: grab selected |
| DELETE `/api/v3/queue/{id}` | Queue/QueueController.cs:73 | PRESENT (routes/mod.rs:162) | yes (3) | UI: Queue: remove |
| DELETE `/api/v3/queue/bulk` | Queue/QueueController.cs:96 | PRESENT (routes/mod.rs:159) | yes (2) | UI: Queue: remove selected |
| GET `/api/v3/queue` | Queue/QueueController.cs:137 | PRESENT (routes/mod.rs:156) | yes (17) | UI: Activity > Queue |
| GET `/api/v3/queue/{id}` | Sonarr.Http/REST/RestController.cs:49 (via QueueController.cs) | MISSING (harmless: Sonarr's own action throws NotImplementedException) | no | nobody known |
| GET `/api/v3/queue/details` | Queue/QueueDetailsController.cs:40 | PRESENT (routes/mod.rs:158) | yes (5) | UI: Series index/details, Calendar, Wanted (per-episode queue icons) |
| GET `/api/v3/queue/details/{id}` | Sonarr.Http/REST/RestController.cs:49 (via QueueDetailsController.cs) | MISSING (harmless: Sonarr's own action throws NotImplementedException) | no | nobody known |
| GET `/api/v3/queue/status` | Queue/QueueStatusController.cs:44 | PRESENT (routes/mod.rs:157) | yes (4) | UI: Sidebar queue badge |
| GET `/api/v3/queue/status/{id}` | Sonarr.Http/REST/RestController.cs:49 (via QueueStatusController.cs) | MISSING (harmless: Sonarr's own action throws NotImplementedException) | no | nobody known |
| POST `/api/v3/remotepathmapping` | RemotePathMappings/RemotePathMappingController.cs:42 | PRESENT (routes/mod.rs:301) | yes (2) | UI: Remote path mapping save (new) |
| GET `/api/v3/remotepathmapping` | RemotePathMappings/RemotePathMappingController.cs:51 | PRESENT (routes/mod.rs:301) | yes (3) | UI: Settings > Download Clients |
| DELETE `/api/v3/remotepathmapping/{id}` | RemotePathMappings/RemotePathMappingController.cs:58 | PRESENT (routes/mod.rs:305) | yes (1) | UI: Remote path mapping delete |
| PUT `/api/v3/remotepathmapping` | RemotePathMappings/RemotePathMappingController.cs:64 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/remotepathmapping/{id}` | RemotePathMappings/RemotePathMappingController.cs:64 | PRESENT (routes/mod.rs:305) | yes (1) | UI: Remote path mapping save (edit) |
| GET `/api/v3/remotepathmapping/{id}` | Sonarr.Http/REST/RestController.cs:49 (via RemotePathMappingController.cs) | PRESENT (routes/mod.rs:305) | no | nobody known |
| POST `/api/v3/rootfolder` | RootFolders/RootFolderController.cs:51 | PRESENT (routes/mod.rs:78) | yes (3) | UI: Add root folder |
| GET `/api/v3/rootfolder` | RootFolders/RootFolderController.cs:60 | PRESENT (routes/mod.rs:78) | yes (3) | UI: Add series, Library Import, Media Management, Bazarr* |
| DELETE `/api/v3/rootfolder/{id}` | RootFolders/RootFolderController.cs:67 | PRESENT (routes/mod.rs:82) | yes (1) | UI: Remove root folder |
| GET `/api/v3/rootfolder/{id}` | Sonarr.Http/REST/RestController.cs:49 (via RootFolderController.cs) | PRESENT (routes/mod.rs:82) | yes (1) | nobody known |
| POST `/api/v3/seasonpass` | SeasonPass/SeasonPassController.cs:20 | PRESENT (routes/mod.rs:61) | yes (1) | UI: Season pass / monitoring modal |
| GET `/api/v3/series` | Series/SeriesController.cs:108 | PRESENT (routes/mod.rs:48) | yes (8) | UI: Series index, app boot, Bazarr* |
| GET `/api/v3/series/{id}` | Series/SeriesController.cs:138 | PRESENT (routes/mod.rs:54) | yes (23) | Bazarr* |
| POST `/api/v3/series` | Series/SeriesController.cs:168 | PRESENT (routes/mod.rs:48) | yes (8) | UI: Add New Series |
| PUT `/api/v3/series` | Series/SeriesController.cs:178 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | no | nobody known |
| PUT `/api/v3/series/{id}` | Series/SeriesController.cs:178 | PRESENT (routes/mod.rs:54) | yes (4) | UI: Edit series, monitor toggles |
| DELETE `/api/v3/series/{id}` | Series/SeriesController.cs:207 | PRESENT (routes/mod.rs:54) | yes (2) | UI: Delete series |
| PUT `/api/v3/series/editor` | Series/SeriesEditorController.cs:27 | PRESENT (routes/mod.rs:50) | yes (2) | UI: Series index select mode: edit |
| DELETE `/api/v3/series/editor` | Series/SeriesEditorController.cs:109 | PRESENT (routes/mod.rs:50) | yes (1) | UI: Series index select mode: delete |
| GET `/api/v3/series/{id}/folder` | Series/SeriesFolderController.cs:20 | PRESENT (routes/mod.rs:60) | yes (1) | UI: Edit series > root folder change |
| POST `/api/v3/series/import` | Series/SeriesImportController.cs:18 | MISSING | no | UI: Library Import (Add > Library Import > Import N series) |
| GET `/api/v3/series/lookup` | Series/SeriesLookupController.cs:26 | PRESENT (routes/mod.rs:49) | yes (10) | UI: Add New Series search, Library Import matching |
| GET `/api/v3/system/backup` | System/Backup/BackupController.cs:34 | PRESENT (routes/mod.rs:329) | yes (3) | UI: System > Backup |
| DELETE `/api/v3/system/backup/{id}` | System/Backup/BackupController.cs:52 | PRESENT (routes/mod.rs:330) | yes (2) | UI: System > Backup delete |
| POST `/api/v3/system/backup/restore/{id}` | System/Backup/BackupController.cs:74 | PRESENT (routes/mod.rs:335) | yes (2) | UI: System > Backup restore |
| POST `/api/v3/system/backup/restore/upload` | System/Backup/BackupController.cs:94 | PRESENT (routes/mod.rs:331) | no | UI: System > Backup restore from upload |
| GET `/api/v3/system/status` | System/SystemController.cs:56 | PRESENT (routes/mod.rs:35) | yes (3) | UI: App boot, System > Status, Bazarr*, Prowlarr*, Recyclarr* |
| GET `/api/v3/system/routes` | System/SystemController.cs:96 | MISSING | no | nobody known |
| GET `/api/v3/system/routes/duplicate` | System/SystemController.cs:108 | MISSING | no | nobody known |
| POST `/api/v3/system/shutdown` | System/SystemController.cs:115 | MISSING | no | UI: Header menu > Shutdown |
| POST `/api/v3/system/restart` | System/SystemController.cs:122 | MISSING | no | UI: Header menu > Restart; restart after a backup restore; restart prompt in Settings > General |
| GET `/api/v3/system/task` | System/Tasks/TaskController.cs:25 | PRESENT (routes/mod.rs:321) | yes (2) | UI: System > Tasks |
| GET `/api/v3/system/task/{id}` | Sonarr.Http/REST/RestController.cs:49 (via TaskController.cs) | PRESENT (routes/mod.rs:322) | no | UI: System > Tasks row refresh |
| GET `/api/v3/tag` | Tags/TagController.cs:33 | PRESENT (routes/mod.rs:69) | yes (3) | UI: App boot, Bazarr* |
| POST `/api/v3/tag` | Tags/TagController.cs:40 | PRESENT (routes/mod.rs:69) | yes (2) | UI: Tag input: new tag |
| PUT `/api/v3/tag` | Tags/TagController.cs:47 | PRESENT (id-less PUT rewritten to /{id} from the body: lib.rs:63, lib.rs:112) | yes (2) | nobody known |
| PUT `/api/v3/tag/{id}` | Tags/TagController.cs:47 | PRESENT (routes/mod.rs:72) | yes (3) | nobody known |
| DELETE `/api/v3/tag/{id}` | Tags/TagController.cs:55 | PRESENT (routes/mod.rs:72) | yes (1) | UI: Settings > Tags delete |
| GET `/api/v3/tag/{id}` | Sonarr.Http/REST/RestController.cs:49 (via TagController.cs) | PRESENT (routes/mod.rs:72) | yes (3) | nobody known |
| GET `/api/v3/tag/detail` | Tags/TagDetailsController.cs:24 | PRESENT (routes/mod.rs:70) | yes (2) | UI: Settings > Tags |
| GET `/api/v3/tag/detail/{id}` | Sonarr.Http/REST/RestController.cs:49 (via TagDetailsController.cs) | PRESENT (routes/mod.rs:71) | no | nobody known |
| GET `/api/v3/update` | Update/UpdateController.cs:26 | OUT OF SCOPE (update; a route exists: routes/mod.rs:336) | yes (1) | UI: System > Updates, UI: System > Updates |
| GET `/api/v3/wanted/cutoff` | Wanted/CutoffController.cs:31 | PRESENT (routes/mod.rs:152) | yes (1) | UI: Wanted > Cutoff Unmet |
| GET `/api/v3/wanted/cutoff/{id}` | Sonarr.Http/REST/RestController.cs:49 (via CutoffController.cs) | MISSING | no | nobody known |
| GET `/api/v3/wanted/missing` | Wanted/MissingController.cs:27 | PRESENT (routes/mod.rs:151) | yes (3) | UI: Wanted > Missing |
| GET `/api/v3/wanted/missing/{id}` | Sonarr.Http/REST/RestController.cs:49 (via MissingController.cs) | MISSING | no | nobody known |
| GET `/ping` | Sonarr.Http/Ping/PingController.cs:24 | MISSING (falls to the SPA fallback lib.rs:241 behind the UI auth guard: HTML or a login redirect, never `{"status":"OK"}`) | no | container healthchecks and uptime monitors (from memory); UI: none |
| HEAD `/ping` | Sonarr.Http/Ping/PingController.cs:25 | MISSING (same) | no | monitoring probes |
| GET `/api` | Sonarr.Http/ApiInfoController.cs:8 | MISSING (`/api` is outside the `/api/v3` nest, lib.rs:260; falls to the SPA fallback) | no | nobody known |
| POST `/login` | Sonarr.Http/Authentication/AuthenticationController.cs:26 | PRESENT (lib.rs:255) | no | UI: login form |
| GET `/logout` | Sonarr.Http/Authentication/AuthenticationController.cs:63 | PRESENT (lib.rs:256) | no | UI: header menu |
| GET `/login` | Sonarr.Http/Frontend/StaticResourceController.cs:30 | PRESENT (lib.rs:255) | no | UI: login page |
| GET `/content/{**path}` | Sonarr.Http/Frontend/StaticResourceController.cs:38 | PRESENT (lib.rs:253, lib.rs:254) | no | UI: every static asset |
| GET `/ and /{**path} (index.html and the disk mappers)` | Sonarr.Http/Frontend/StaticResourceController.cs:44-45 | PRESENT (lib.rs:209, fallback lib.rs:241) | no | UI: deep links |
| GET `/initialize.json` | Sonarr.Http/Frontend/InitializeJsonController.cs:33 | PRESENT (lib.rs:210) | yes (2) | UI: app boot |
| GET `/feed/v3/calendar/Sonarr.ics` | Sonarr.Api.V3/Calendar/CalendarFeedController.cs:30 (`[V3FeedController("calendar")]` :16) | MISSING (no `/feed` route anywhere; a path with a dot that is not an asset answers 404 from ui.rs:122-126) | no | UI: Calendar > iCal Link (the URL it hands out); calendar apps |
| POST `/signalr/messages/negotiate` | NzbDrone.Host/Startup.cs:281 (`MapHub<MessageHub>`) | PRESENT (routes/mod.rs:344) | yes (1) | UI: SignalRConnector; Bazarr* |
| GET `/signalr/messages (WebSocket)` | NzbDrone.Host/Startup.cs:281 | PRESENT (routes/mod.rs:345) | no | UI: SignalRConnector; Bazarr* |
| GET/POST `/signalr/messages (ServerSentEvents, LongPolling transports)` | NzbDrone.Host/Startup.cs:281 | PARTIAL (negotiate advertises both, signalr.rs:84-87, but `connect` only takes a WebSocket upgrade, signalr.rs:93-103, and no POST is routed) | no | UI only when WebSockets are blocked by a proxy |
| GET `/MediaCover/{seriesId}/{file} (mapper)` | Sonarr.Http/Frontend/Mappers/MediaCoverMapper.cs:48 | PRESENT (lib.rs:230; case-sensitive prefix, Sonarr's is case-insensitive) | no | UI: posters, banners, fanart |
| GET `/MediaCoverProxy/{hash}/{file} (mapper)` | Sonarr.Http/Frontend/Mappers/MediaCoverProxyMapper.cs:31 | PRESENT (lib.rs:214) | no | UI: lookup result posters |
| GET `/backup/{type}/{file} (mapper)` | Sonarr.Http/Frontend/Mappers/BackupFileMapper.cs:29 | PRESENT (lib.rs:218) | no | UI: System > Backup download |
| GET `/logfile/{file}.txt (mapper)` | Sonarr.Http/Frontend/Mappers/LogFileMapper.cs:31 | PRESENT (lib.rs:222) | no | UI: System > Log Files download |
| GET `/updatelogfile/{file}.txt (mapper)` | Sonarr.Http/Frontend/Mappers/UpdateLogFileMapper.cs:31 | OUT OF SCOPE (update; a route exists, lib.rs:226) | no | UI: updater log download |

## Table B — client side

| UI call | frontend file:line | Unarr | which page or action breaks |
|---|---|---|---|
| `GET /initialize.json` | index.ts:8 | HANDLED (lib.rs:210) | — |
| `POST /login` | login.html:218 | HANDLED (lib.rs:255) | — |
| `GET /login` | Sonarr.Http redirect target (login.html) | HANDLED (lib.rs:255) | — |
| `GET /logout` | Components/Page/Header/PageHeaderActionsMenu.tsx:75 | HANDLED (lib.rs:256) | — |
| `GET /api/v3/system/status` | Store/Actions/appActions.js:98; systemActions.js:256 | HANDLED (routes/mod.rs:35) | — |
| `GET /api/v3/health` | Store/Actions/systemActions.js:257 | HANDLED (routes/mod.rs:36) | — |
| `GET /api/v3/diskspace` | Store/Actions/systemActions.js:258 | HANDLED (routes/mod.rs:126) | — |
| `GET /api/v3/system/task` | Store/Actions/systemActions.js:260 | HANDLED (routes/mod.rs:321) | — |
| `GET /api/v3/system/task/{id}` | Store/Actions/systemActions.js:259 (System/Tasks/Scheduled/ScheduledTaskRow.tsx:101) | HANDLED (routes/mod.rs:322) | — |
| `GET /api/v3/system/backup` | Store/Actions/systemActions.js:262 | HANDLED (routes/mod.rs:329) | — |
| `DELETE /api/v3/system/backup/{id}` | Store/Actions/systemActions.js:325 | HANDLED (routes/mod.rs:330) | — |
| `POST /api/v3/system/backup/restore/{id}` | Store/Actions/systemActions.js:279 | HANDLED (routes/mod.rs:335) | — |
| `POST /api/v3/system/backup/restore/upload` | Store/Actions/systemActions.js:292 | HANDLED (routes/mod.rs:331) | — |
| `POST /api/v3/system/restart` | Store/Actions/systemActions.js:349 (PageHeaderActionsMenu.tsx:31, GeneralSettingsConnector.js:81, RestoreBackupModalContentConnector.js:32) | MISSING | Header menu > Restart; restart after a backup restore; restart prompt in Settings > General — the request 404s; the UI never shows "restarting", and a restored backup is not applied until the process is restarted by hand |
| `POST /api/v3/system/shutdown` | Store/Actions/systemActions.js:361 (PageHeaderActionsMenu.tsx:35) | MISSING | Header menu > Shutdown — the request 404s, nothing happens |
| `GET /api/v3/update` | Store/Actions/systemActions.js:327 | OUT OF SCOPE (update; a route exists: routes/mod.rs:332) | — |
| `GET /api/v3/log` | Store/Actions/systemActions.js:333 | HANDLED (routes/mod.rs:325) | — |
| `GET /api/v3/log/file` | Store/Actions/systemActions.js:328 | HANDLED (routes/mod.rs:326) | — |
| `GET /api/v3/log/file/update` | Store/Actions/systemActions.js:329 | HANDLED (routes/mod.rs:327) | — |
| `GET /api/v3/series` | Store/Actions/seriesActions.js:539 | HANDLED (routes/mod.rs:48) | — |
| `POST /api/v3/series` | Store/Actions/addSeriesActions.js:125 | HANDLED (routes/mod.rs:48) | — |
| `PUT /api/v3/series/{id}` | Store/Actions/seriesActions.js:540, :558, :616 | HANDLED (routes/mod.rs:54) | — |
| `DELETE /api/v3/series/{id}` | Store/Actions/seriesActions.js:541 | HANDLED (routes/mod.rs:54) | — |
| `GET /api/v3/series/lookup` | Store/Actions/addSeriesActions.js:86; importSeriesActions.js:146 | HANDLED (routes/mod.rs:49) | — |
| `POST /api/v3/series/import` | Store/Actions/importSeriesActions.js:243 | MISSING | Library Import (Add > Library Import > Import N series) — the import button fails with 404: existing series folders cannot be bulk-imported from the UI (each must be added one by one) |
| `GET /api/v3/series/{id}/folder` | Series/Edit/RootFolder/RootFolderModalContent.tsx:40 | HANDLED (routes/mod.rs:60) | — |
| `PUT /api/v3/series/editor` | Store/Actions/seriesActions.js:748 | HANDLED (routes/mod.rs:50) | — |
| `DELETE /api/v3/series/editor` | Store/Actions/seriesActions.js:797 | HANDLED (routes/mod.rs:50) | — |
| `POST /api/v3/seasonPass` | Store/Actions/seriesActions.js:711 | HANDLED (routes/mod.rs:61) | — |
| `GET /api/v3/episode` | Store/Actions/episodeActions.js:185; episodeSelectionActions.js:50 | HANDLED (routes/mod.rs:127) | — |
| `PUT /api/v3/episode/{id}` | Store/Actions/episodeActions.js:201 | HANDLED (routes/mod.rs:129) | — |
| `PUT /api/v3/episode/monitor` | Store/Actions/episodeActions.js:245; Creators/createBatchToggleEpisodeMonitoredHandler.js:19 | HANDLED (routes/mod.rs:128) | — |
| `GET /api/v3/episodeFile` | Store/Actions/episodeFileActions.js:61 | HANDLED (routes/mod.rs:133) | — |
| `GET /api/v3/episodeFile/{id}` | Store/Actions/episodeFileActions.js:60 (Episode/Summary/EpisodeSummary.tsx:113) | HANDLED (routes/mod.rs:139) | — |
| `DELETE /api/v3/episodeFile/{id}` | Store/Actions/episodeFileActions.js:54 | HANDLED (routes/mod.rs:139) | — |
| `DELETE /api/v3/episodeFile/bulk` | Store/Actions/episodeFileActions.js:97 | HANDLED (routes/mod.rs:135) | — |
| `PUT /api/v3/episodeFile/bulk` | Store/Actions/episodeFileActions.js:150 | HANDLED (routes/mod.rs:135) | — |
| `GET /api/v3/calendar` | Store/Actions/calendarActions.js:283 | HANDLED (routes/mod.rs:150) | — |
| `GET /api/v3/history` | Store/Actions/historyActions.js:263; episodeHistoryActions.js:55 | HANDLED (routes/mod.rs:153) | — |
| `GET /api/v3/history/series` | Store/Actions/seriesHistoryActions.js:46 | HANDLED (routes/mod.rs:155) | — |
| `POST /api/v3/history/failed/{id}` | Store/Actions/historyActions.js:286; episodeHistoryActions.js:89; seriesHistoryActions.js:81 | MISSING | "Mark as failed" on a grabbed row: Activity > History, episode History tab, series/season History modal — the button returns 404: a bad download cannot be failed (blocklisted and searched again) from history |
| `GET /api/v3/queue` | Store/Actions/queueActions.js:325 | HANDLED (routes/mod.rs:156) | — |
| `GET /api/v3/queue/details` | Store/Actions/queueActions.js:295 | HANDLED (routes/mod.rs:158) | — |
| `GET /api/v3/queue/status` | Store/Actions/queueActions.js:302 | HANDLED (routes/mod.rs:157) | — |
| `POST /api/v3/queue/grab/{id}` | Store/Actions/queueActions.js:346 | HANDLED (routes/mod.rs:161) | — |
| `POST /api/v3/queue/grab/bulk` | Store/Actions/queueActions.js:391 | HANDLED (routes/mod.rs:160) | — |
| `DELETE /api/v3/queue/{id}` | Store/Actions/queueActions.js:446 | HANDLED (routes/mod.rs:162) | — |
| `DELETE /api/v3/queue/bulk` | Store/Actions/queueActions.js:481 | HANDLED (routes/mod.rs:159) | — |
| `GET /api/v3/blocklist` | Store/Actions/blocklistActions.js:146 | HANDLED (routes/mod.rs:163) | — |
| `DELETE /api/v3/blocklist/{id}` | Store/Actions/blocklistActions.js:155 | MISSING | Activity > Blocklist: remove one row (X button) — 404: a blocklisted release cannot be un-blocklisted individually |
| `DELETE /api/v3/blocklist/bulk` | Store/Actions/blocklistActions.js:175 | MISSING | Activity > Blocklist: Remove Selected — 404: only "Clear" (ClearBlocklist command, everything) works |
| `GET /api/v3/wanted/missing` | Store/Actions/wantedActions.js:262 | HANDLED (routes/mod.rs:151) | — |
| `GET /api/v3/wanted/cutoff` | Store/Actions/wantedActions.js:280 | HANDLED (routes/mod.rs:152) | — |
| `GET /api/v3/release` | Store/Actions/releaseActions.js:305 | HANDLED (routes/mod.rs:289) | — |
| `POST /api/v3/release` | Store/Actions/releaseActions.js:330 | HANDLED (routes/mod.rs:289) | — |
| `GET /api/v3/manualimport` | Store/Actions/interactiveImportActions.js:110 | HANDLED (routes/mod.rs:145) | — |
| `POST /api/v3/manualimport` | Store/Actions/interactiveImportActions.js:180 | HANDLED (routes/mod.rs:145) | — |
| `GET /api/v3/rename` | Store/Actions/organizePreviewActions.js:38 | HANDLED (routes/mod.rs:149) | — |
| `GET /api/v3/filesystem` | Store/Actions/pathActions.js:54 | HANDLED (routes/mod.rs:63) | — |
| `GET /api/v3/parse` | Store/Actions/parseActions.ts:65 | HANDLED (routes/mod.rs:62) | — |
| `GET /api/v3/command` | Store/Actions/commandActions.js:165 | HANDLED (routes/mod.rs:323) | — |
| `POST /api/v3/command` | Store/Actions/commandActions.js:146 | HANDLED (routes/mod.rs:323) | — |
| `DELETE /api/v3/command/{id}` | Store/Actions/commandActions.js:171 | HANDLED (routes/mod.rs:324) | — |
| `GET /api/v3/tag` | Store/Actions/tagActions.js:50 | HANDLED (routes/mod.rs:69) | — |
| `POST /api/v3/tag` | Store/Actions/tagActions.js:54 | HANDLED (routes/mod.rs:69) | — |
| `DELETE /api/v3/tag/{id}` | Store/Actions/tagActions.js:69 | HANDLED (routes/mod.rs:72) | — |
| `GET /api/v3/tag/detail` | Store/Actions/tagActions.js:70 | HANDLED (routes/mod.rs:70) | — |
| `GET /api/v3/rootFolder` | Store/Actions/rootFolderActions.js:45 | HANDLED (routes/mod.rs:78) | — |
| `POST /api/v3/rootFolder` | Store/Actions/rootFolderActions.js:62 | HANDLED (routes/mod.rs:78) | — |
| `DELETE /api/v3/rootFolder/{id}` | Store/Actions/rootFolderActions.js:49 | HANDLED (routes/mod.rs:82) | — |
| `GET /api/v3/customFilter` | Store/Actions/customFilterActions.js:45 | HANDLED (routes/mod.rs:311) | — |
| `POST /api/v3/customFilter` | Store/Actions/customFilterActions.js:47 | HANDLED (routes/mod.rs:311) | — |
| `PUT /api/v3/customFilter/{id}` | Store/Actions/customFilterActions.js:47 | HANDLED (routes/mod.rs:315) | — |
| `DELETE /api/v3/customFilter/{id}` | Store/Actions/customFilterActions.js:49 | HANDLED (routes/mod.rs:315) | — |
| `GET /api/v3/localization` | Utilities/String/translate.ts:7 | HANDLED (routes/mod.rs:337) | — |
| `GET /api/v3/localization/language` | Utilities/String/getLanguageName.ts:11 | HANDLED (routes/mod.rs:338) | — |
| `GET /api/v3/language` | Store/Actions/Settings/languages.js:38 | HANDLED (routes/mod.rs:86) | — |
| `GET /api/v3/indexerFlag` | Store/Actions/Settings/indexerFlags.js:38 | HANDLED (routes/mod.rs:34) | — |
| `GET /api/v3/config/host` | Store/Actions/Settings/general.js:53 | HANDLED (routes/mod.rs:37) | — |
| `PUT /api/v3/config/host` | Store/Actions/Settings/general.js:54 (Creators/createSaveHandler.js:14, no id in the URL) | HANDLED (lib.rs:112 rewrites to /{id}; routes/mod.rs:38) | — |
| `GET /api/v3/config/ui` | Store/Actions/Settings/ui.js:53 | HANDLED (routes/mod.rs:39) | — |
| `PUT /api/v3/config/ui` | Store/Actions/Settings/ui.js:54 | HANDLED (lib.rs:112 rewrites to /{id}; routes/mod.rs:40) | — |
| `GET /api/v3/config/mediamanagement` | Store/Actions/Settings/mediaManagement.js:53 | HANDLED (routes/mod.rs:166) | — |
| `PUT /api/v3/config/mediamanagement` | Store/Actions/Settings/mediaManagement.js:54 | HANDLED (lib.rs:112 rewrites to /{id}; routes/mod.rs:167) | — |
| `GET /api/v3/config/naming` | Store/Actions/Settings/naming.js:53 | HANDLED (routes/mod.rs:164) | — |
| `PUT /api/v3/config/naming` | Store/Actions/Settings/naming.js:54 | HANDLED (lib.rs:112 rewrites to /{id}; routes/mod.rs:165) | — |
| `GET /api/v3/config/naming/examples` | Store/Actions/Settings/namingExamples.js:46 | HANDLED (routes/mod.rs:173) | — |
| `GET /api/v3/config/indexer` | Store/Actions/Settings/indexerOptions.js:53 | HANDLED (routes/mod.rs:171) | — |
| `PUT /api/v3/config/indexer` | Store/Actions/Settings/indexerOptions.js:54 | HANDLED (lib.rs:112 rewrites to /{id}; routes/mod.rs:172) | — |
| `GET /api/v3/config/downloadclient` | Store/Actions/Settings/downloadClientOptions.js:53 | HANDLED (routes/mod.rs:41) | — |
| `PUT /api/v3/config/downloadclient` | Store/Actions/Settings/downloadClientOptions.js:54 | HANDLED (lib.rs:112 rewrites to /{id}; routes/mod.rs:42) | — |
| `GET /api/v3/config/importlist` | Store/Actions/Settings/importListOptions.js:53 | HANDLED (routes/mod.rs:46) | — |
| `PUT /api/v3/config/importlist` | Store/Actions/Settings/importListOptions.js:54 | HANDLED (lib.rs:112 rewrites to /{id}; routes/mod.rs:47) | — |
| `GET /api/v3/qualitydefinition` | Store/Actions/Settings/qualityDefinitions.js:54 | HANDLED (routes/mod.rs:88) | — |
| `PUT /api/v3/qualitydefinition/update` | Store/Actions/Settings/qualityDefinitions.js:80 | HANDLED (routes/mod.rs:89) | — |
| `GET /api/v3/qualityprofile` | Store/Actions/Settings/qualityProfiles.js:72 | HANDLED (routes/mod.rs:97) | — |
| `GET /api/v3/qualityprofile/schema` | Store/Actions/Settings/qualityProfiles.js:73 | HANDLED (routes/mod.rs:101) | — |
| `POST /api/v3/qualityprofile` | Store/Actions/Settings/qualityProfiles.js:74 | HANDLED (routes/mod.rs:97) | — |
| `PUT /api/v3/qualityprofile/{id}` | Store/Actions/Settings/qualityProfiles.js:74 | HANDLED (routes/mod.rs:105) | — |
| `DELETE /api/v3/qualityprofile/{id}` | Store/Actions/Settings/qualityProfiles.js:75 | HANDLED (routes/mod.rs:105) | — |
| `GET /api/v3/customformat` | Store/Actions/Settings/customFormats.js:92 | HANDLED (routes/mod.rs:111) | — |
| `GET /api/v3/customformat/schema` | Store/Actions/Settings/customFormatSpecifications.js:92 | HANDLED (routes/mod.rs:274) | — |
| `POST /api/v3/customformat` | Store/Actions/Settings/customFormats.js:106 | HANDLED (routes/mod.rs:111) | — |
| `PUT /api/v3/customformat/{id}` | Store/Actions/Settings/customFormats.js:106 | HANDLED (routes/mod.rs:120) | — |
| `DELETE /api/v3/customformat/{id}` | Store/Actions/Settings/customFormats.js:94 | HANDLED (routes/mod.rs:120) | — |
| `PUT /api/v3/customformat/bulk` | Store/Actions/Settings/customFormats.js:109 | HANDLED (routes/mod.rs:115) | — |
| `DELETE /api/v3/customformat/bulk` | Store/Actions/Settings/customFormats.js:110 | HANDLED (routes/mod.rs:115) | — |
| `GET /api/v3/delayprofile` | Store/Actions/Settings/delayProfiles.js:65 | HANDLED (routes/mod.rs:275) | — |
| `GET /api/v3/delayprofile/schema` | Store/Actions/Settings/delayProfiles.js:66 | MISSING (harmless: `fetchDelayProfileSchema` is never dispatched anywhere in frontend/src, and Sonarr declares no such route either) | none — dead UI code |
| `POST /api/v3/delayprofile` | Store/Actions/Settings/delayProfiles.js:68 | HANDLED (routes/mod.rs:275) | — |
| `PUT /api/v3/delayprofile/{id}` | Store/Actions/Settings/delayProfiles.js:68 | HANDLED (routes/mod.rs:283) | — |
| `DELETE /api/v3/delayprofile/{id}` | Store/Actions/Settings/delayProfiles.js:69 | HANDLED (routes/mod.rs:283) | — |
| `PUT /api/v3/delayprofile/reorder/{id}` | Store/Actions/Settings/delayProfiles.js:87 | HANDLED (routes/mod.rs:279) | — |
| `GET /api/v3/releaseprofile` | Store/Actions/Settings/releaseProfiles.js:57 | HANDLED (routes/mod.rs:291) | — |
| `POST /api/v3/releaseprofile` | Store/Actions/Settings/releaseProfiles.js:59 | HANDLED (routes/mod.rs:291) | — |
| `PUT /api/v3/releaseprofile/{id}` | Store/Actions/Settings/releaseProfiles.js:59 | HANDLED (routes/mod.rs:295) | — |
| `DELETE /api/v3/releaseprofile/{id}` | Store/Actions/Settings/releaseProfiles.js:61 | HANDLED (routes/mod.rs:295) | — |
| `GET /api/v3/remotepathmapping` | Store/Actions/Settings/remotePathMappings.js:57 | HANDLED (routes/mod.rs:301) | — |
| `POST /api/v3/remotepathmapping` | Store/Actions/Settings/remotePathMappings.js:58 | HANDLED (routes/mod.rs:301) | — |
| `PUT /api/v3/remotepathmapping/{id}` | Store/Actions/Settings/remotePathMappings.js:58 | HANDLED (routes/mod.rs:305) | — |
| `DELETE /api/v3/remotepathmapping/{id}` | Store/Actions/Settings/remotePathMappings.js:59 | HANDLED (routes/mod.rs:305) | — |
| `GET /api/v3/indexer` | Store/Actions/Settings/indexers.js:114 | HANDLED (routes/mod.rs:176) | — |
| `GET /api/v3/indexer/schema` | Store/Actions/Settings/indexers.js:115 | HANDLED (routes/mod.rs:177) | — |
| `POST /api/v3/indexer` | Store/Actions/Settings/indexers.js:117 | HANDLED (routes/mod.rs:176) | — |
| `PUT /api/v3/indexer/{id}` | Store/Actions/Settings/indexers.js:117 | HANDLED (routes/mod.rs:185) | — |
| `DELETE /api/v3/indexer/{id}` | Store/Actions/Settings/indexers.js:119 | HANDLED (routes/mod.rs:185) | — |
| `POST /api/v3/indexer/test` | Store/Actions/Settings/indexers.js:120 | HANDLED (routes/mod.rs:178) | — |
| `POST /api/v3/indexer/testall` | Store/Actions/Settings/indexers.js:122 | HANDLED (routes/mod.rs:179) | — |
| `PUT /api/v3/indexer/bulk` | Store/Actions/Settings/indexers.js:123 | HANDLED (routes/mod.rs:181) | — |
| `DELETE /api/v3/indexer/bulk` | Store/Actions/Settings/indexers.js:124 | HANDLED (routes/mod.rs:181) | — |
| `GET /api/v3/downloadclient` | Store/Actions/Settings/downloadClients.js:109 | HANDLED (routes/mod.rs:191) | — |
| `GET /api/v3/downloadclient/schema` | Store/Actions/Settings/downloadClients.js:110 | HANDLED (routes/mod.rs:195) | — |
| `POST /api/v3/downloadclient` | Store/Actions/Settings/downloadClients.js:112 | HANDLED (routes/mod.rs:191) | — |
| `PUT /api/v3/downloadclient/{id}` | Store/Actions/Settings/downloadClients.js:112 | HANDLED (routes/mod.rs:206) | — |
| `DELETE /api/v3/downloadclient/{id}` | Store/Actions/Settings/downloadClients.js:114 | HANDLED (routes/mod.rs:206) | — |
| `POST /api/v3/downloadclient/test` | Store/Actions/Settings/downloadClients.js:115 | HANDLED (routes/mod.rs:196) | — |
| `POST /api/v3/downloadclient/testall` | Store/Actions/Settings/downloadClients.js:117 | HANDLED (routes/mod.rs:197) | — |
| `PUT /api/v3/downloadclient/bulk` | Store/Actions/Settings/downloadClients.js:118 | HANDLED (routes/mod.rs:202) | — |
| `DELETE /api/v3/downloadclient/bulk` | Store/Actions/Settings/downloadClients.js:119 | HANDLED (routes/mod.rs:202) | — |
| `GET /api/v3/importlist` | Store/Actions/Settings/importLists.js:98 | HANDLED (routes/mod.rs:230) | — |
| `GET /api/v3/importlist/schema` | Store/Actions/Settings/importLists.js:99 | HANDLED (routes/mod.rs:234) | — |
| `POST /api/v3/importlist` | Store/Actions/Settings/importLists.js:100 | HANDLED (routes/mod.rs:230) | — |
| `PUT /api/v3/importlist/{id}` | Store/Actions/Settings/importLists.js:100 | HANDLED (routes/mod.rs:242) | — |
| `DELETE /api/v3/importlist/{id}` | Store/Actions/Settings/importLists.js:102 | HANDLED (routes/mod.rs:242) | — |
| `POST /api/v3/importlist/test` | Store/Actions/Settings/importLists.js:103 | HANDLED (routes/mod.rs:235) | — |
| `POST /api/v3/importlist/testall` | Store/Actions/Settings/importLists.js:105 | HANDLED (routes/mod.rs:236) | — |
| `PUT /api/v3/importlist/bulk` | Store/Actions/Settings/importLists.js:106 | HANDLED (routes/mod.rs:238) | — |
| `DELETE /api/v3/importlist/bulk` | Store/Actions/Settings/importLists.js:107 | HANDLED (routes/mod.rs:238) | — |
| `GET /api/v3/importlistexclusion/paged` | Store/Actions/Settings/importListExclusions.js:77 | HANDLED (routes/mod.rs:255) | — |
| `POST /api/v3/importlistexclusion` | Store/Actions/Settings/importListExclusions.js:85 | HANDLED (routes/mod.rs:251) | — |
| `PUT /api/v3/importlistexclusion/{id}` | Store/Actions/Settings/importListExclusions.js:85 | HANDLED (routes/mod.rs:259) | — |
| `DELETE /api/v3/importlistexclusion/{id}` | Store/Actions/Settings/importListExclusions.js:86 | HANDLED (routes/mod.rs:259) | — |
| `DELETE /api/v3/importlistexclusion/bulk` | Store/Actions/Settings/importListExclusions.js:87 | MISSING | Settings > Import Lists > exclusions: delete selected — 404: exclusions can only be deleted one at a time |
| `GET /api/v3/notification` | Store/Actions/Settings/notifications.js:95 | HANDLED (routes/mod.rs:212) | — |
| `GET /api/v3/notification/schema` | Store/Actions/Settings/notifications.js:96 | HANDLED (routes/mod.rs:216) | — |
| `POST /api/v3/notification` | Store/Actions/Settings/notifications.js:98 | HANDLED (routes/mod.rs:212) | — |
| `PUT /api/v3/notification/{id}` | Store/Actions/Settings/notifications.js:98 | HANDLED (routes/mod.rs:224) | — |
| `DELETE /api/v3/notification/{id}` | Store/Actions/Settings/notifications.js:100 | HANDLED (routes/mod.rs:224) | — |
| `POST /api/v3/notification/test` | Store/Actions/Settings/notifications.js:101 | HANDLED (routes/mod.rs:217) | — |
| `GET /api/v3/metadata` | Store/Actions/Settings/metadata.js:63 | HANDLED (routes/mod.rs:248) | — |
| `PUT /api/v3/metadata/{id}` | Store/Actions/Settings/metadata.js:64 | MISSING | Settings > Metadata: save a consumer (Kodi/Roksbox/WDTV/Plex) — 405/404 on save: a metadata consumer cannot be enabled or edited (the writers themselves are an accepted limit; the CRUD route is not) |
| `GET /api/v3/autoTagging` | Store/Actions/Settings/autoTaggings.js:72 | HANDLED (routes/mod.rs:263) | — |
| `GET /api/v3/autoTagging/schema` | Store/Actions/Settings/autoTaggingSpecifications.js:92 | HANDLED (routes/mod.rs:267) | — |
| `POST /api/v3/autoTagging` | Store/Actions/Settings/autoTaggings.js:86 | HANDLED (routes/mod.rs:263) | — |
| `PUT /api/v3/autoTagging/{id}` | Store/Actions/Settings/autoTaggings.js:86 | HANDLED (routes/mod.rs:268) | — |
| `DELETE /api/v3/autoTagging/{id}` | Store/Actions/Settings/autoTaggings.js:74 | HANDLED (routes/mod.rs:268) | — |
| `POST /api/v3/indexer/action/{name}` | Utilities/requestAction.js:27 (providerOptionActions.js:72, captchaActions.js:49/:89) | HANDLED (routes/mod.rs:180) | — |
| `POST /api/v3/downloadclient/action/{name}` | Utilities/requestAction.js:27 (providerOptionActions.js:72) | HANDLED (routes/mod.rs:198) | — |
| `POST /api/v3/importlist/action/{name}` | Utilities/requestAction.js:27 (oAuthActions.js:94/:117/:144, providerOptionActions.js:72) | HANDLED (routes/mod.rs:237) | — |
| `POST /api/v3/notification/action/{name}` | Utilities/requestAction.js:27 (Components/Form/Tag/DeviceInput.tsx:78, oAuthActions.js) | HANDLED (routes/mod.rs:219) | — |
| `GET /Content/... (icons, fonts.css, styles.css, logo.svg, thetvdb.png, error.png, 404.png)` | index.ejs:22-53; login.html:25-58,212; Components/Page/Header/PageHeader.js:61; Settings/MetadataSource/TheTvdb.js:11; Components/Error/ErrorBoundaryError.tsx:48; Components/NotFound.tsx:20 | HANDLED (lib.rs:253-254, ui.rs:134) | — |
| `GET /Content/manifest.json, /Content/browserconfig.xml` | index.ejs:36,50; login.html:41,55 | UNSURE (served as plain assets by ui.rs:134; Sonarr's ManifestMapper/BrowserConfig rewrite the URL base inside them, and no such rewrite was found in ui.rs — only matters with a URL base) | PWA install icon paths under a URL base |
| `GET /favicon.ico` | index.ejs:45 | HANDLED (lib.rs:252) | — |
| `GET /MediaCover/{id}/poster-{250,500 x devicePixelRatio}.jpg, banner-{size}.jpg, fanart.jpg` | Series/SeriesImage.tsx:13,30,70 (sizes: Series/Index/Posters/SeriesIndexPoster.tsx:182, Series/Index/Overview/SeriesIndexOverview.tsx:169, Series/Details/SeriesDetails.js:345); fanart Series/Details/SeriesDetails.js:49 | HANDLED (lib.rs:230-240; a missing resized file falls back to the full cover, ui.rs:469) | — |
| `GET /MediaCoverProxy/{hash}/{file}` | server-built `remoteUrl`/`url` in lookup results shown by AddSeries/AddNewSeries/AddNewSeriesSearchResult.js:96 | HANDLED (lib.rs:214) | — |
| `GET {urlBase}{backup.path} (/backup/{type}/{file})` | System/Backup/BackupRow.js:102 | HANDLED (lib.rs:218) | — |
| `GET {logFile.downloadUrl} (/logfile/{file})` | System/Logs/Files/LogFilesTableRow.js:32 | HANDLED (lib.rs:222) | — |
| `GET /feed/v3/calendar/Sonarr.ics?unmonitored&premieresOnly&asAllDay&tags&apikey` | Calendar/iCal/CalendarLinkModalContent.tsx:46,67 | MISSING | Calendar > iCal Link: the modal shows a URL that answers 404, so calendar apps cannot subscribe |
| `GET /oauth.html` | Store/Actions/oAuthActions.js:16; oauth.html:7 | UNSURE (served by the SPA asset path ui.rs:122-125 only if `oauth.html` is in the embedded `ui/sonarr/` bundle, which this audit did not open) | OAuth pop-up of an import list or connection (Simkl uses a device/redirect flow through it) |
| `SignalR connect: POST /signalr/messages/negotiate?access_token=, then WebSocket /signalr/messages` | Components/SignalRConnector.js:104-125 | HANDLED (routes/mod.rs:344-345; routes/signalr.rs:55-103) | — |
| `SignalR transport fallback (ServerSentEvents / LongPolling)` | Components/SignalRConnector.js:106-117 (default transports of @microsoft/signalr) | PARTIAL (advertised by negotiate, signalr.rs:84-87; not served: `connect` needs a WebSocket upgrade, signalr.rs:93-103) | "Connection lost" banner behind a proxy that blocks WebSockets |
| `SignalR `version` {version}` | Components/SignalRConnector.js:288 | HANDLED (sent once on connect, routes/signalr.rs:147-150) | — |
| `SignalR `command` action=updated` | Components/SignalRConnector.js:161-178 | HANDLED (state.rs:199-202) | — |
| `SignalR `command` action=sync` | Components/SignalRConnector.js:162-165 | MISSING (Sonarr: Commands/CommandController.cs:125; Unarr sends only `updated`) | none seen: each update carries the resource |
| `SignalR `health` (any action)` | Components/SignalRConnector.js:215 | HANDLED (state.rs:204) | — |
| `SignalR `series` action=updated` | Components/SignalRConnector.js:257-264 | HANDLED (relay.rs:93; routes/series_writes.rs:346, :442, :498) | — |
| `SignalR `series` action=deleted` | Components/SignalRConnector.js:265-267 | HANDLED (routes/series_writes.rs:379) | — |
| `SignalR `episode` action=updated` | Components/SignalRConnector.js:180-188 | HANDLED (relay.rs:85; routes/series_writes.rs:556, :583) — but only on import/delete/monitor: no broadcast on grab (Sonarr: Episodes/EpisodeControllerWithSignalR.cs:131, EpisodeGrabbedEvent; Unarr's event loop state.rs:199-206 has no grabbed event) | none visible: the queue broadcast repaints the episode's download icon |
| `SignalR `episodefile` action=updated` | Components/SignalRConnector.js:190-197 | HANDLED (relay.rs:63) | — |
| `SignalR `episodefile` action=deleted` | Components/SignalRConnector.js:198-202 | HANDLED (relay.rs:24-27) | — |
| `SignalR `queue` (any action)` | Components/SignalRConnector.js:274-278 | HANDLED (routes/queue.rs:63) | — |
| `SignalR `queue/details` (any action)` | Components/SignalRConnector.js:280-282 | HANDLED (routes/queue.rs:64) | — |
| `SignalR `queue/status` resource` | Components/SignalRConnector.js:284-286 | HANDLED (routes/queue.rs:65-68) | — |
| `SignalR `downloadclient` created/updated/deleted` | Components/SignalRConnector.js:205-213 | HANDLED (routes/downloadclient_writes.rs:189-192) | — |
| `SignalR `indexer` created/updated/deleted` | Components/SignalRConnector.js:229-237 | HANDLED (routes/indexer_writes.rs:206-209) | — |
| `SignalR `importlist` created/updated/deleted` | Components/SignalRConnector.js:219-227 | HANDLED (routes/importlist_writes.rs:227-230) | — |
| `SignalR `notification` created/updated/deleted` | Components/SignalRConnector.js:247-255 | HANDLED (routes/notification_writes.rs:143-146) | — |
| `SignalR `calendar` action=updated` | Components/SignalRConnector.js:151-159 | MISSING (no `broadcast("calendar"` in crates/; Sonarr: Calendar/CalendarController.cs:17 via EpisodeControllerWithSignalR) | Calendar: an event's colour does not change on grab; on import the page repopulates anyway (Calendar/Calendar.tsx:97) |
| `SignalR `wanted/missing` action=updated` | Components/SignalRConnector.js:304-312 | MISSING (no broadcast; Sonarr: Wanted/MissingController.cs:16) | Wanted > Missing: a row is not refreshed on grab; it repopulates on import (MissingConnector.js:50) |
| `SignalR `wanted/cutoff` action=updated` | Components/SignalRConnector.js:294-302 | MISSING (no broadcast; Sonarr: Wanted/CutoffController.cs:16) | Wanted > Cutoff Unmet: same |
| `SignalR `tag` action=sync` | Components/SignalRConnector.js:322-328 | MISSING (no broadcast; Sonarr: Tags/TagController.cs:64, :70) | Tags created by another tab or by auto-tagging/import lists appear only after a page reload |
| `SignalR `rootfolder` (any action)` | Components/SignalRConnector.js:318-320 | MISSING (no broadcast; Sonarr: RootFolders/RootFolderController.cs:15 model events) | Root folders changed in another tab appear only after reload |
| `SignalR `system/task` (any action)` | Components/SignalRConnector.js:314-316 | MISSING (no broadcast; Sonarr: System/Tasks/TaskController.cs:66) | none seen: the handler only refetches /command, which `command` updates already cover |
| `SignalR `qualitydefinition` (any action)` | Components/SignalRConnector.js:270-272 | MISSING (no broadcast; Sonarr: Qualities/QualityDefinitionController.cs:76) | Settings > Quality in a second tab is stale until reload |
| `SignalR `metadata` action=updated` | Components/SignalRConnector.js:239-245 | MISSING (no broadcast; and no write route to cause one) | none beyond the missing PUT /metadata |
| `POST /command name=RefreshSeries, RescanSeries, RssSync, EpisodeSearch, SeasonSearch, SeriesSearch, MissingEpisodeSearch, CutoffUnmetEpisodeSearch, DownloadedEpisodesScan, ManualImport, RenameFiles, RenameSeries, MoveSeries, RefreshMonitoredDownloads, ClearBlocklist, Backup (+ scheduled task names)` | Commands/commandNames.js; Store/Actions/commandActions.js:146 | HANDLED (routes/command.rs:91; names: unarr-core/src/commands/mod.rs:275-350) | — |
| `POST /command name=ClearLog` | System/Events/LogsTableConnector.js:100 | MISSING (name not in unarr-core/src/commands/mod.rs:275; routes/command.rs:91-92 answers 500 "Sequence contains no matching element") | System > Events > Clear fails with an error toast |
| `POST /command name=DeleteLogFiles` | System/Logs/Files/LogFilesConnector.js:63 | MISSING (same) | System > Log Files > Delete fails |
| `POST /command name=ResetQualityDefinitions` | Settings/Quality/Reset/ResetQualityDefinitionsModalContentConnector.js:31 | MISSING (same) | Settings > Quality > Reset Definitions fails |
| `POST /command name=ResetApiKey` | Settings/General/GeneralSettingsConnector.js:77 | MISSING (same) | Settings > General > Reset API key fails |
| `POST /command name=DeleteUpdateLogFiles` | System/Logs/Updates/UpdateLogFilesConnector.js:67 | OUT OF SCOPE (update) | — |
| `POST /command name=ApplicationUpdate` | System/Updates/Updates.tsx:111,120 | OUT OF SCOPE (update) | — |
| `GET /config/naming/examples?renameEpisodes&standardEpisodeFormat&dailyEpisodeFormat&animeEpisodeFormat&seriesFolderFormat&seasonFolderFormat&specialsFolderFormat&multiEpisodeStyle&colonReplacementFormat&... (the pending form)` | Store/Actions/Settings/namingExamples.js:45-48 | IGNORED PARAMETER (routes/settings.rs:153 takes no query and always renders the saved config; Sonarr: Config/NamingConfigController.cs:68-73 uses the query when its `id` is not 0) | Settings > Media Management > Episode Naming: the example file names do not follow what is typed, they change only after Save |
| `GET /blocklist?protocols=` | Store/Actions/blocklistActions.js:92-104 (custom filter `protocols`) | IGNORED PARAMETER (routes/activity.rs:379-389 reads only `seriesIds`; Sonarr: Blocklist/BlocklistController.cs:30) | Activity > Blocklist: a custom filter on Protocol shows every row |
| `GET /blocklist?seriesIds=` | Store/Actions/blocklistActions.js:92-104 | HANDLED (routes/activity.rs:389) | — |
| `GET /blocklist sortKey=date, series.sortTitle, sourceTitle, indexer` | Store/Actions/blocklistActions.js:27,37,43,65,71 | HANDLED (routes/activity.rs:386) | — |
| `GET /history sortKey=date, series.sortTitle` | Store/Actions/historyActions.js:28,42,74; episodeHistoryActions.js:49 | HANDLED (routes/activity.rs:199) | — |
| `GET /history?eventType=&seriesIds=&quality=&languages=&episodeId=` | Store/Actions/historyActions.js:118-215; episodeHistoryActions.js:46-52 | HANDLED (routes/activity.rs:203-211) | — |
| `GET /history/series?seriesId=&seasonNumber=` | Store/Actions/seriesHistoryActions.js:45-48 | HANDLED (routes/activity.rs:317-320) | — |
| `GET /queue sortKey=timeleft, status, series.sortTitle, episode, episodes.title, episodes.airDateUtc, languages, quality, protocol, indexer, downloadClient, title, size, estimatedCompletionTime, added, progress` | Store/Actions/queueActions.js:53-170 | HANDLED (routes/queue.rs:26-46) | — |
| `GET /queue?includeUnknownSeriesItems=` | Store/Actions/queueActions.js:31,238-240 | HANDLED (routes/queue.rs:224) | — |
| `GET /queue?seriesIds=&quality=&languages=&protocol=&status=` | Store/Actions/queueActions.js:191-225 | HANDLED (routes/queue.rs:227-235) | — |
| `GET /queue/details?seriesId= / episodeIds= / all=true / time=&view=` | Store/Actions/queueActions.js:304-320 (Series/Details/SeriesDetailsConnector.js:199, Calendar/Calendar.tsx:93,135, Series/Index/SeriesIndex.tsx:107) | HANDLED (routes/queue.rs:398-401; `all`, `time`, `view` are ignored by Sonarr too: Queue/QueueDetailsController.cs:42) | — |
| `DELETE /queue/{id} and /queue/bulk ?removeFromClient&blocklist&skipRedownload&changeCategory` | Store/Actions/queueActions.js:446,481 | HANDLED (routes/queue.rs:425-428) | — |
| `GET /wanted/missing, /wanted/cutoff sortKey=episodes.airDateUtc, series.sortTitle, episodes.lastSearchTime` | Store/Actions/wantedActions.js:25,34,50,56,104,112,128,134 | HANDLED (routes/episodes.rs:389-393) | — |
| `GET /wanted/missing, /wanted/cutoff ?monitored=` | Store/Actions/wantedActions.js:72-95,155-178 | HANDLED (routes/episodes.rs:412, :437) | — |
| `GET /log sortKey=time, ?level=info/warn/error` | Store/Actions/systemActions.js:77,93,122-157 | HANDLED (routes/system.rs:233-239) | — |
| `GET /importlistexclusion/paged sortKey=title, tvdbId` | Store/Actions/Settings/importListExclusions.js:45-54 | HANDLED (routes/providers.rs:268) | — |
| `GET /calendar?start=&end=&unmonitored=&tags=` | Store/Actions/calendarActions.js:265-285 | HANDLED (routes/episodes.rs:343-348) | — |
| `GET /episode?seriesId= / seasonNumber= / episodeIds=` | Series/Details/SeriesDetailsConnector.js:197; InteractiveImport/Episode/SelectEpisodeModalContent.tsx:177; Activity/Queue/Queue.tsx:231 | HANDLED (routes/episodes.rs:235-238) | — |
| `GET /episodeFile?seriesId= / episodeFileIds=` | Series/Details/SeriesDetailsConnector.js:198; Calendar/Calendar.tsx:139 | HANDLED (routes/episodes.rs:289-290) | — |
| `GET /release?episodeId= / seriesId=&seasonNumber=` | InteractiveSearch/InteractiveSearch.tsx:168 | HANDLED (routes/release.rs:32-37) | — |
| `GET /manualimport?downloadId&seriesId&seasonNumber&folder&filterExistingFiles` | InteractiveImport/Interactive/InteractiveImportModalContent.tsx:411-417 | HANDLED (routes/mediafiles.rs:132-136) | — |
| `GET /rename?seriesId=&seasonNumber=` | Organize/OrganizePreviewModalContentConnector.js:48-51 | HANDLED (routes/mediafiles.rs:266-267) | — |
| `GET /filesystem?path&allowFoldersWithoutTrailingSlashes&includeFiles` | Store/Actions/pathActions.js:53-59 | HANDLED (routes/series_writes.rs:692-693, :796) | — |
| `GET /series/lookup?term=` | Store/Actions/addSeriesActions.js:86-89 | HANDLED (routes/series_writes.rs:142) | — |
| `GET /parse?title=` | Store/Actions/parseActions.ts:64-68 | HANDLED (routes/series_writes.rs:639) | — |
| `DELETE /series/{id}?deleteFiles&addImportListExclusion` | Series/Delete/DeleteSeriesModalContentConnector.js:32-36 | HANDLED (routes/series_writes.rs:359-360) | — |
| `PUT /series/{id}?moveFolder=true` | Store/Actions/seriesActions.js:526-529 | IGNORED PARAMETER — parity, not a gap: Sonarr reads `moveFiles` (Series/SeriesController.cs:181) and ignores `moveFolder` too; Unarr reads `moveFiles` (routes/series_writes.rs:293) | none versus Sonarr |
| `PUT /delayprofile/reorder/{id}?after=` | Store/Actions/Settings/delayProfiles.js:83-87 | HANDLED (routes/settings_writes.rs:1210) | — |
| `POST/PUT provider ?forceSave=true, POST {provider}/test?forceTest=true` | Store/Actions/Creators/createSaveProviderHandler.js:39-41; createTestProviderHandler.js:36-38 | HANDLED (routes/indexer_writes.rs:215,254,312; downloadclient_writes.rs:198,241,298; importlist_writes.rs:252,290,348; notification_writes.rs:151,190,247) | — |
| `GET /series?includeSeasonImages=` | not sent by this UI build (Store/Actions/seriesActions.js:539 sends no query); a server parameter (Series/SeriesController.cs:110) | HANDLED (routes/series.rs:111) | — |
