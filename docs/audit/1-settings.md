# Audit 1 — settings Unarr stores and shows but does not apply

Static audit, read-only, 2026-11-10. Nothing was compiled or run.

- `S:` = `<sonarr>/src/` (`Core/` = `NzbDrone.Core/`)
- `U:` = `crates/` (`core/` = `unarr-core/src/`, `api/` = `unarr-api/src/`)
- "Grep" always means `grep -rniE` over `crates --include=*.rs`, tests excluded, for the PascalCase key in quotes, the camelCase name and the snake_case field.
- Status: APPLIED (consumer line given) / NOT APPLIED / PARTIAL / OUT OF SCOPE / UNSURE.
- Severity: high = wrong behaviour on files, downloads, security; medium = a feature silently not working; low = cosmetic.

## Summary of findings (NOT APPLIED and PARTIAL), by severity

| # | Setting | Status | Severity |
|---|---------|--------|----------|
| 1 | `config/host` PUT: `authenticationMethod`, `authenticationRequired`, `username`/`password`, `apiKey` | NOT APPLIED on write (accepted with 202, dropped) | high |
| 2 | Proxy (`ProxyEnabled`, `ProxyType`, `ProxyHostname`, `ProxyPort`, `ProxyUsername`, `ProxyPassword`, `ProxyBypassFilter`, `ProxyBypassLocalAddresses`) | NOT APPLIED | high |
| 3 | `RecycleBinCleanupDays` | NOT APPLIED | high |
| 4 | `ImportExtraFiles` / `ExtraFileExtensions` | NOT APPLIED | medium |
| 5 | `UseScriptImport` / `ScriptImportPath` | NOT APPLIED | medium |
| 6 | `CertificateValidation` | NOT APPLIED | medium |
| 7 | `TrustCgnatIpAddresses` | NOT APPLIED | medium |
| 8 | `LogLevel` / `ConsoleLogLevel` | NOT APPLIED | medium |
| 9 | Environment overrides `Sonarr__Server__*`, `Sonarr__Auth__*`, `Sonarr__Log__*`, `Sonarr__App__*` | NOT APPLIED (not read at all) | medium |
| 10 | `FileDate` | PARTIAL (import and rename only, not on rescan) | medium |
| 11 | `EnableMediaInfo` | PARTIAL (no backfill of files without media info on rescan) | low |
| 12 | `UILanguage` | NOT APPLIED | low |
| 13 | `LaunchBrowser` | NOT APPLIED | low |
| 14 | `ConsoleLogFormat`, `LogDbEnabled`, `LogSql` | NOT APPLIED (not parsed) | low |

## 1. config.xml (`S:Core/Configuration/ConfigFileProvider.cs`)

| Setting | Where Sonarr consumes it | Unarr status | User-visible consequence | Severity |
|---|---|---|---|---|
| BindAddress | ConfigFileProvider.cs:162, Kestrel URL | APPLIED `U:unarrd/src/main.rs:384` (at start; `--bind` overrides) | none | - |
| Port | ConfigFileProvider.cs:178 | APPLIED `U:unarrd/src/main.rs:376` (at start) | none | - |
| SslPort, EnableSsl, SslCertPath, SslCertPassword | :180-182, :252-253 | OUT OF SCOPE (warns at `main.rs:377`) | - | - |
| LaunchBrowser | `S:NzbDrone.Host/AppLifetime.cs:62`, opens a browser at start | NOT APPLIED. Parsed `U:unarr-import/src/config_xml.rs:60`, stored as override `api/routes/settings_writes.rs:544-545`, shown `api/routes/config.rs:83`. Grep `launch_browser\|LaunchBrowser`: only parse/store/serialise | none on a headless daemon | low |
| ApiKey (read) | ConfigFileProvider.cs:186, API authentication | APPLIED `api/auth.rs:77` | none | - |
| ApiKey (change through `PUT config/host`) | `S:Sonarr.Api.V3/Config/HostConfigController.cs:122-135` `SaveConfigDictionary` writes every property | NOT APPLIED. `settings_writes.rs:688-707` copies 14 keys into the overrides; `apiKey` is not among them and `apply_host_overrides` (`:530-577`) has no branch for it. Grep `api_key\s*=`: only `config_xml.rs:52` | "Reset API key" in Settings > General answers 202 and the key stays the old one; a leaked key cannot be rotated from the UI | high |
| AuthenticationMethod (read) | `S:Sonarr.Http/Authentication/AuthenticationService.cs:26` | APPLIED `api/ui.rs:390,422` | none | - |
| AuthenticationRequired (read) | `S:Sonarr.Http/Authentication/UiAuthorizationHandler.cs:28` | APPLIED `api/ui.rs:409` | none | - |
| AuthenticationMethod / AuthenticationRequired (change through `PUT config/host`) | HostConfigController.cs:122-130 | NOT APPLIED. Body value is only read for validation (`settings_writes.rs:626`); neither key is in the list at `:688-707` nor in `apply_host_overrides`. Grep `authentication_method\s*=\|authentication_required\s*=`: no assignment outside `config_xml.rs` and `main.rs:177` (defaults) | Changing the method (e.g. None to Forms, or Enabled to "Disabled for Local Addresses") is answered 202 and the GET that follows returns the old value. A blank install (generated config says `None`, `main.rs:177`) can never be given authentication from the UI | high |
| Username / Password (`PUT config/host`) | HostConfigController.cs:132-135 `_userService.Upsert` | NOT APPLIED. `store::providers::insert_user` (`core/store/providers.rs:297`) has one caller, the importer (`U:unarr-import/src/sonarr.rs:1138`). Grep `insert_user\|update_user\|set_password\|upsert` | A password change is silently discarded: the old password keeps working, the new one never does. A compromised password cannot be changed | high |
| AllowedHosts | `S:NzbDrone.Host/ConfigureHostFilteringOptions.cs:30` | APPLIED `api/hosts.rs:116` (`api/state.rs:36`) | none | - |
| TrustedNetworks | - | already fixed (`api/forwarded.rs`), not re-reported | - | - |
| UrlBase | `S:NzbDrone.Host/Startup.cs:247` | APPLIED `api/lib.rs:276-280`, `api/ui.rs:60` | none (change needs a restart, as in Sonarr) | - |
| InstanceName | `S:Sonarr.Http/Frontend/InitializeJsonController.cs:52`, `Core/Notifications/Webhook/WebhookBase.cs:40` | APPLIED `api/state.rs:43`, `api/routes/system.rs:135`, `core/notifications/webhook.rs:93` | none | - |
| Theme | `InitializeJsonController.cs:53`, `Frontend/Mappers/LoginHtmlMapper.cs:42` | APPLIED `api/routes/system.rs:136`, `api/ui.rs:164` | none | - |
| LogLevel | `S:Core/Instrumentation/ReconfigureLogging.cs:30`, minimum level of the files and console; `debug`/`trace` create `sonarr.debug.txt`/`sonarr.trace.txt` | NOT APPLIED. Parsed `config_xml.rs:76`, override `settings_writes.rs:557`, shown `config.rs:91`. The level comes from `RUST_LOG` only (`U:unarrd/src/main.rs:448-451`). Grep `log_level\|LogLevel`: those four places and `core/config.rs:81` | Settings > General > Log Level saves and reloads, the logs do not change; a user asked for a trace log gets an info log | medium |
| ConsoleLogLevel | ReconfigureLogging.cs:33-35 | NOT APPLIED, and not stored: `config.rs:93` answers the constant `""`, not parsed by `config_xml.rs` | as above, console only | low |
| ConsoleLogFormat | ConfigFileProvider.cs:234 (Standard/Clef) | NOT APPLIED, not parsed (grep `ConsoleLogFormat\|clef`: nothing) | none unless a log shipper expects CLEF | low |
| LogDbEnabled, LogSql | `S:NzbDrone.Host/Startup.cs:224`; :248 | NOT APPLIED, not parsed. Unarr has no log database (`GET /log` is the in-memory buffer, `api/logbuf.rs`) | none | low |
| LogRotate, LogSizeLimit | :249-250 | known, documented as fixed defaults (DESIGN §14 Logs) | - | - |
| TrustCgnatIpAddresses (xml) | ConfigFileProvider.cs:486 (legacy source of the Config-table value) | see section 2 | - | - |
| Branch, UpdateAutomatically, UpdateMechanism, UpdateScriptPath | - | OUT OF SCOPE | - | - |
| AnalyticsEnabled, FilterSentryEvents | - | OUT OF SCOPE (always off, DESIGN §11) | - | - |
| Postgres*, Syslog* | - | OUT OF SCOPE | - | - |
| Environment overrides (`Sonarr__Server__Port`, `__UrlBase`, `__BindAddress`, `Sonarr__Auth__ApiKey`, `__Method`, `__Required`, `Sonarr__Log__Level`, `Sonarr__App__InstanceName`, `__Theme`) | `S:NzbDrone.Host/Bootstrap.cs:109-114`; each property is `_xxxOptions.X ?? GetValue(...)` (ConfigFileProvider.cs:162-302) | NOT APPLIED. Grep `SONARR__\|Sonarr__`: nothing; the only variables read are `UNARR_*` (`unarrd/src/main.rs:19-38`), `RUST_LOG`, `HOSTNAME` | A compose file that sets `SONARR__AUTH__APIKEY` or `SONARR__SERVER__URLBASE` (common with Helm charts and Homepage setups) is ignored: Unarr serves under the config.xml values and the ecosystem gets 401 or 404 | medium |

## 2. Config table (`S:Core/Configuration/ConfigService.cs`)

| Setting | Where Sonarr consumes it | Unarr status | User-visible consequence | Severity |
|---|---|---|---|---|
| AutoUnmonitorPreviouslyDownloadedEpisodes | `Core/Tv/EpisodeService.cs:263`; `DecisionEngine/Specifications/RssSync/DeletedEpisodeFileSpecification.cs:31` | APPLIED `core/mediafiles/deletion.rs:38`, `core/mediafiles/scan.rs:307`, `core/decision/specs.rs:844` | none | - |
| Retention | `DecisionEngine/Specifications/RetentionSpecification.cs:31` | APPLIED `core/decision/specs.rs:773` | none | - |
| RecycleBin | `MediaFiles/RecycleBinProvider.cs:43,72`; `HealthCheck/Checks/RecyclingBinCheck.cs:26` | APPLIED `core/mediafiles/transfer.rs:113,166`, `core/health.rs:826` | none | - |
| RecycleBinCleanupDays | `RecycleBinProvider.cs:161-190` (`CleanUpRecycleBinCommand`, daily): files older than N days are deleted | NOT APPLIED. Loaded into `MediaManagementConfig` (`core/store/config.rs:263`) and never read; the command is scheduled (`core/commands/scheduler.rs:49`) and executes as a no-op (`core/commands/executor.rs:748-753`, "nothing to do for this command yet"). Grep `recycle_bin_cleanup_days`: store and API only | With a recycling bin configured every upgraded or deleted file stays in it forever; the disk fills while the UI says "7 days". The "Empty Recycle Bin" action (`CleanUpRecycleBin` from System > Tasks) also does nothing | high |
| RssSyncInterval | `Jobs/TaskManager.cs:187`; `Download/Pending/PendingReleaseService.cs:359` | APPLIED `core/commands/scheduler.rs:23-27` (same clamps), `core/downloads/pending.rs:387` | none | - |
| MaximumSize | `DecisionEngine/Specifications/MaximumSizeSpecification.cs:26` | APPLIED `core/decision/specs.rs:434` | none | - |
| MinimumAge | `MinimumAgeSpecification.cs:32`; `PendingReleaseService.cs:432` | APPLIED `core/decision/specs.rs:455`, `core/downloads/pending.rs:292` | none | - |
| DownloadPropersAndRepacks | UpgradeSpecification.cs:29, RepackSpecification.cs:35, UpgradableSpecification.cs:35, AnimeVersionUpgradeSpecification.cs:37, QueueSpecification.cs:105, ProperSpecification.cs:34, DownloadDecisionComparer.cs:75 | APPLIED `core/mediafiles/specs.rs:466`, `core/decision/specs.rs:270,624,733,1019`, `core/decision/upgradable.rs:63`, `core/decision/engine.rs:456` | none | - |
| EnableCompletedDownloadHandling | `Download/DownloadProcessingService.cs:49`, `TrackedDownloads/DownloadMonitoringService.cs:150`, AlreadyImportedSpecification.cs:32, HistorySpecification.cs:44, health checks | APPLIED `core/mediafiles/completed.rs:360`, `core/downloads/monitor.rs:106`, `core/decision/specs.rs:220,874`, `core/health.rs:564,758,920` | none | - |
| AutoRedownloadFailed, AutoRedownloadFailedFromInteractiveSearch | `Download/RedownloadFailedDownloadService.cs:40,46` | APPLIED `core/downloads/redownload.rs:39-40` | none | - |
| CreateEmptySeriesFolders | `MediaFiles/DiskScanService.cs:104` | APPLIED `core/mediafiles/scan.rs:196` | none | - |
| DeleteEmptyFolders | `DiskScanService.cs:106,268`; `MediaFileDeletionService.cs:132` | APPLIED `core/mediafiles/scan.rs:197,290`, `core/mediafiles/deletion.rs:61` | none | - |
| FileDate | `MediaFiles/UpdateEpisodeFileService.cs:51` (on import/move) and `:146-175` `Handle(SeriesScannedEvent)`: every file of the series is re-dated after each scan | PARTIAL. `change_file_date` (`core/mediafiles/approve.rs:249`) has one caller, the transfer into the library (`approve.rs:317`). Grep `change_file_date\|file_date`: nothing in `scan.rs` or the executor's `after_scan` | Turning the setting on does nothing for the existing library, and an air date corrected by a refresh is never propagated; only files imported or renamed afterwards get the date. Media servers sorting "recently added" by mtime differ from Sonarr | medium |
| DownloadClientWorkingFolders | `EpisodeImport/Specifications/NotUnpackingSpecification.cs:33` | APPLIED `core/mediafiles/specs.rs:397` | none | - |
| DownloadClientHistoryLimit | `Download/Clients/Nzbget/Nzbget.cs:113`, `Sabnzbd/Sabnzbd.cs:110` | APPLIED `core/downloadclients/service.rs:100`, `nzbget.rs:364`, `sabnzbd.rs:470` | none | - |
| SkipFreeSpaceCheckWhenImporting, MinimumFreeSpaceWhenImporting | both `FreeSpaceSpecification.cs` (import :27,:50; decision :29,:55) | APPLIED `core/mediafiles/specs.rs:205,217`, `core/decision/specs.rs:392,396` | none | - |
| CopyUsingHardlinks | `MediaFiles/EpisodeFileMovingService.cs:100` (and `Extras/Files/ExtraFileManager.cs:74`) | APPLIED `core/mediafiles/approve.rs:393` (the extras half falls under ImportExtraFiles) | none | - |
| EnableMediaInfo | `MediaInfo/UpdateMediaInfoService.cs:42` `Handle(SeriesScannedEvent)`: files with no media info or an old schema revision are probed after every scan; `:61`; `EpisodeImport/Aggregation/AggregationService.cs:58` | PARTIAL. Applied at `core/mediafiles/decide.rs:206` and `core/mediafiles/scan.rs:155`, but `scan.rs:262-285` re-probes only files whose size changed; there is no pass over files with `media_info == None` or an older schema | Files imported from a Sonarr database without media info (or scanned while ffprobe was missing) never get it, so `{MediaInfo ...}` naming tokens stay empty and renames differ from Sonarr | low |
| UseScriptImport, ScriptImportPath | `MediaFiles/ScriptImportDecider.cs:120,184-186`: an external script moves the file instead of Sonarr | NOT APPLIED. Stored and shown by `api/routes/settings_writes.rs` and `api/routes/settings.rs` only. Grep `ScriptImport\|script_import` in `unarr-core`: nothing | A user who imports through a script (e.g. to transcode or to copy across file systems) gets Unarr's own move/hardlink instead, without a warning | medium |
| ImportExtraFiles, ExtraFileExtensions | `Extras/ExtraService.cs:63,70`: files with the listed extensions next to the video are imported with it | NOT APPLIED. Loaded (`core/store/config.rs:305-306`) and never read. Grep `import_extra_files\|extra_file_extensions`: store and API only; grep `subtitle\|\.srt` in `core/mediafiles`: media-info streams only. `docs/phase-3a-import.md:98-100` lists "subtitle and extra files" as out of scope for 3a; no later phase picks it up | Subtitles and `.nfo` shipped inside a release are left in the download folder (and deleted with it). Related, not setting-driven: Sonarr also moves known subtitle files on rename (`ExtraService.cs:152-169`); Unarr has no such step, so Bazarr's `.srt` keep the old name after a rename (not verified by running) | medium |
| RescanAfterRefresh | `Tv/RefreshSeriesService.cs:170` | APPLIED `core/commands/executor.rs:1230-1246` | none | - |
| EpisodeTitleRequired | `EpisodeImport/Specifications/EpisodeTitleSpecification.cs:39` | APPLIED `core/mediafiles/specs.rs:149` | none | - |
| SetPermissionsLinux, ChmodFolder, ChownGroup | `MediaFiles/MediaFileAttributeService.cs:80-87` (file at `EpisodeFileMovingService.cs:172`, folders at `:248`); `DiskScanService.cs:115,250-257` | APPLIED `core/mediafiles/transfer.rs:272-300`, called at `approve.rs:324` (file) and `transfer.rs:365` (every folder `create_folder` makes, used by the scan and the import) | none | - |
| ListSyncLevel, ListSyncTag | `ImportLists/ImportListSyncService.cs:282-327` | APPLIED `core/importlists/mod.rs:550-601` | none | - |
| FirstDayOfWeek, CalendarWeekColumnHeader, ShortDateFormat, LongDateFormat, TimeFormat, ShowRelativeDates, EnableColorImpairedMode | no server consumer; the frontend reads `config/ui` | APPLIED in the only place they are consumed: served by `api/routes/config.rs:125-131`, stored by `settings_writes.rs:310-322` | none | - |
| UILanguage | `Core/Localization/LocalizationService.cs:89`: picks the dictionary `GET /localization` serves (39 languages under `Core/Localization/Core/`) and the language of server-built messages | NOT APPLIED. Stored `settings_writes.rs:330`, shown `config.rs:133`; `api/routes/system.rs:319-331` always serves the embedded `localization-en.json` and `{"identifier":"en"}` | Choosing Italian in Settings > UI saves and the interface stays English | low |
| CleanupMetadataImages | `Housekeeping/Housekeepers/DeleteBadMediaCovers.cs:35` | OUT OF SCOPE (metadata file writers; `Housekeeping` is a no-op, `executor.rs:748`) | - | - |
| PlexClientIdentifier | `Notifications/Plex/Server/PlexServerProxy.cs:99` | APPLIED `core/notifications/plex.rs:109-118` | none | - |
| RijndaelPassphrase, HmacPassphrase, RijndaelSalt, HmacSalt | internal secrets, no behaviour to port | not a setting (regenerated by the anonymiser only) | - | - |
| ProxyEnabled, ProxyType, ProxyHostname, ProxyPort, ProxyUsername, ProxyPassword, ProxyBypassFilter, ProxyBypassLocalAddresses | `Core/Http/HttpProxySettingsProvider.cs:38-50`: every outgoing request (indexers, clients, Skyhook, notifications) goes through the proxy; `HealthCheck/Checks/ProxyCheck.cs:35` | NOT APPLIED. Stored `settings_writes.rs:674-681`, shown `config.rs:105-112` (a golden covers the round trip: `tests/fixtures/sonarr/golden/config-host-put-proxy.json`). The two HTTP clients are built with static settings (`core/http.rs:45-54`, `:160-166`) and nothing else builds one (grep `reqwest::Client::(new\|builder)`). Grep `ProxyEnabled\|proxy_enabled\|\.proxy(`: API only. Note: reqwest still honours `HTTP(S)_PROXY` from the environment, since `.no_proxy()` is not called | A user who sends indexer and tracker traffic through a proxy for privacy, or whose host has no direct route out, gets direct connections (real IP shown to indexers) or timeouts, while the settings page shows the proxy as enabled. No proxy health check either | high |
| CertificateValidation | `Core/Security/X509CertificateValidationService.cs:59`: `Disabled` accepts any certificate, `DisabledForLocalAddresses` accepts invalid ones from local hosts | NOT APPLIED. Stored `settings_writes.rs:682`, shown `config.rs:113`. Grep `CertificateValidation\|danger_accept\|certificate`: API only; rustls always validates | A download client, indexer or Plex behind HTTPS with a self-signed certificate works in Sonarr with validation disabled and fails in Unarr on every request (fails closed) | medium |
| ApplicationUrl | `Notifications/Webhook/WebhookBase.cs:41` (custom scripts out of scope) | APPLIED `core/notifications/mod.rs:419`, `webhook.rs` payload | none | - |
| BackupFolder, BackupRetention | `Core/Backup/BackupService.cs:174,230` | APPLIED `core/backup.rs:116,164-170` | none | - |
| BackupInterval | `Jobs/TaskManager.cs:170` | APPLIED `core/commands/scheduler.rs:19-22` | none | - |
| TrustCgnatIpAddresses | `S:Sonarr.Http/Authentication/UiAuthorizationHandler.cs:34-35`: with "Disabled for Local Addresses", 100.64.0.0/10 counts as local | NOT APPLIED. Stored `settings_writes.rs:686`, shown `config.rs:117`. `is_local_address` (`api/ui.rs:366-380`) knows loopback, RFC 1918, link-local and ULA only. Grep `TrustCgnat\|cgnat\|100\.64`: API store/serialise only | A Tailscale client (100.64/10) that Sonarr let in without a login is sent to the login page (fails closed) | medium |

## 3. Per-entity settings

All rows below were checked for a consumer, not for parsing; the consumer line is given.

| Entity / field | Where Sonarr consumes it | Unarr status |
|---|---|---|
| Quality profile: `Items` (allowed), `Cutoff`, `UpgradeAllowed` | QualityAllowedByProfileSpecification, UpgradableSpecification | APPLIED `core/decision/specs.rs:509`, `core/decision/upgradable.rs:66,103,162`, `core/profile.rs:43` |
| Quality profile: `MinFormatScore`, `CutoffFormatScore`, `MinUpgradeFormatScore`, `FormatItems` | CustomFormatAllowedByProfileSpecification, UpgradableSpecification | APPLIED `core/decision/specs.rs:377`, `core/decision/upgradable.rs:80,83,104` |
| Quality definition: `MinSize`, `MaxSize`, `PreferredSize`, `Title` | AcceptableSizeSpecification, DownloadDecisionComparer, FileNameBuilder | APPLIED `core/decision/specs.rs:190`, `core/decision/engine.rs:558`, `core/mediafiles/approve.rs:183` |
| Delay profile: `EnableUsenet`, `EnableTorrent`, `PreferredProtocol`, `UsenetDelay`, `TorrentDelay`, `BypassIfHighestQuality`, `BypassIfAboveCustomFormatScore`, `MinimumCustomFormatScore`, `Order`, `Tags` | ProtocolSpecification, DelaySpecification, PendingReleaseService, comparer | APPLIED `core/decision/specs.rs:500-503,789-793,807,822`, `core/downloads/pending.rs:287`, `core/decision/engine.rs:476`, `core/decision/lookup.rs:239` |
| Release profile: `Enabled`, `Required`, `Ignored`, `IndexerId`, `Tags` | ReleaseRestrictionsSpecification, `ReleaseProfileService.EnabledForTags` | APPLIED `core/decision/lookup.rs:244-250`, `core/decision/specs.rs:695-699` |
| Custom format: the 8 specification types, `negate`, `required`, `IncludeCustomFormatWhenRenaming` | CustomFormatCalculationService, FileNameBuilder | APPLIED `core/customformat.rs:113-142`, `core/naming.rs:482` |
| Naming: `RenameEpisodes`, `ReplaceIllegalCharacters`, `ColonReplacementFormat`, `CustomColonReplacementFormat`, `MultiEpisodeStyle`, the three episode formats, `SeriesFolderFormat`, `SeasonFolderFormat`, `SpecialsFolderFormat` | `Organizer/FileNameBuilder.cs` | APPLIED `core/naming.rs:288-300,962,1109-1117,1228,1305,1317-1319`; series folder used on add and move (`core/tv/series.rs:321,491`, `core/commands/executor.rs:1494`), season folder at `core/mediafiles/approve.rs:148` |
| Root folders | RootFolderService, import, add | APPLIED `core/mediafiles/scan.rs:181`, `core/downloadclients/service.rs:103` |
| Remote path mappings | `RemotePathMappingService.RemapRemoteToLocal` | APPLIED `core/downloadclients/mod.rs:296` and each client (`nzbget.rs:374`, `sabnzbd.rs:502`, `qbittorrent.rs:709`, `transmission.rs:398`) |
| Indexer: `EnableRss`, `EnableAutomaticSearch`, `EnableInteractiveSearch` | IndexerFactory | APPLIED `core/indexers/service.rs:95-97` |
| Indexer: `Priority`, `DownloadClientId`, `SeasonSearchMaximumSingleEpisodeAge`, `Tags` | comparer, DownloadClientProvider, SeasonPackOnlySpecification, IndexerTagSpecification | APPLIED `core/decision/engine.rs:506`, `core/downloadclients/service.rs:194`, `core/decision/specs.rs:1169`, `core/decision/specs.rs:987` |
| Newznab/Torznab: `Categories`, `AnimeCategories`, `AnimeStandardFormatSearch`, `AdditionalParameters`, `ApiPath`, `MultiLanguages`, `FailDownloads`, `MinimumSeeders`, `SeedCriteria.*`, `RejectBlocklistedTorrentHashesWhileGrabbing` | NewznabRequestGenerator, parsers, TorrentSeedingSpecification, SeedConfigProvider | APPLIED `core/indexers/request.rs:203-355,494-496`, `core/decision/engine.rs:289,330`, `core/mediafiles/completed.rs:101-106`, `core/decision/specs.rs:1213`, `core/decision/lookup.rs:132-134`, `core/downloadclients/download.rs:334` |
| Download client: `Enable`, `Priority`, `Tags`, `RemoveCompletedDownloads`, `RemoveFailedDownloads` | DownloadClientProvider, CompletedDownloadService, FailedDownloadService | APPLIED `core/downloadclients/service.rs:170-234`, `core/mediafiles/completed.rs:313,327`, `core/downloads/failed.rs:348`, `transmission.rs:432`, `qbittorrent.rs:665` |
| SABnzbd / NZBGet / qBittorrent / Transmission settings (host, port, useSsl, urlBase, credentials, tvCategory, tvImportedCategory, tvDirectory, recent/olderTvPriority, addPaused, initialState, sequentialOrder, firstAndLast, contentLayout) | each client's proxy | APPLIED. Every field of `core/downloadclients/settings.rs` has a reader outside that file (loop over the field names; the two priorities through `priority()`/`move_to_top()`, called at `nzbget.rs:309`, `sabnzbd.rs:425`, `qbittorrent.rs:587`, `transmission.rs:349`) |
| Notification triggers: `OnGrab`, `OnDownload`, `OnUpgrade`, `OnImportComplete`, `OnRename`, `OnSeriesAdd`, `OnSeriesDelete`, `OnEpisodeFileDelete`, `OnEpisodeFileDeleteForUpgrade`, `OnHealthIssue`, `IncludeHealthWarnings`, `OnHealthRestored`, `OnManualInteractionRequired`, `Tags` | NotificationService | APPLIED `core/notifications/mod.rs:521,647,671,704,707,734,760,763,792,827,863,882,165,170` |
| Notification: `OnApplicationUpdate` | NotificationService on update installed | OUT OF SCOPE (update mechanism); parsed at `mod.rs:125`, no sender |
| Discord (`WebHookUrl`, `Username`, `Avatar`, `Author`, `GrabFields`, `ImportFields`, `ManualInteractionFields`), Webhook (`Url`, `Method`, `Username`, `Password`, `Headers`), Plex Server (`Host`, `Port`, `UseSsl`, `UrlBase`, `AuthToken`, `UpdateLibrary`, `MapFrom`, `MapTo`) | the three implementations | APPLIED `core/notifications/discord.rs:129,241-245,294-301,368-375`, `webhook.rs:286-297`, `plex.rs:162,324-328`, `mod.rs:901,926` |
| Other notifications, custom scripts | - | OUT OF SCOPE |
| Import list: `EnableAutomaticAdd`, `SearchForMissingEpisodes`, `ShouldMonitor`, `MonitorNewItems`, `RootFolderPath`, `QualityProfileId`, `SeriesType`, `SeasonFolder`, `Tags`; Simkl `ListType`, `ShowType`; exclusions | ImportListSyncService, SimklUserRequestGenerator | APPLIED `core/importlists/mod.rs:344,416,430,489-507`, `core/importlists/simkl.rs:87-88` |
| Import lists other than Simkl | - | OUT OF SCOPE |
| Series: `Monitored`, `SeasonFolder`, `SeriesType`, `UseSceneNumbering`, `MonitorNewItems`, `QualityProfileId`, `Tags` | specifications, FileNameBuilder, RefreshEpisodeService | APPLIED `core/decision/specs.rs:996`, `core/mediafiles/approve.rs:145`, `core/naming.rs:1234-1237`, `core/decision/mapping.rs:282`, `core/tv/refresh.rs:209` |
| Auto tagging rules (`RemoveTagsAutomatically`, 9 specifications) | AutoTaggingService | APPLIED `core/tv/autotag.rs:109` |
| Metadata consumers (Kodi, Roksbox, WDTV, Plex) | - | OUT OF SCOPE |

## 4. Notes on evidence and limits

- Finding 1 is a different shape from `TrustedNetworks`: the value is not stored either. It is here because the effect is the same (the UI accepts a setting that then has no effect) and the golden tests cannot see it: the recorded `PUT config/host` bodies send the values they already have.
- The `LaunchBrowser`, `ConsoleLogFormat`, `LogDbEnabled` and `LogSql` rows are probably intentional for a headless daemon; they are listed because nothing in `docs/DESIGN.md` says so.
- Not checked: whether each APPLIED consumer is correct in every branch (that is the golden tests' job); the Radarr and Lidarr personalities; anything that needs the code to run. The subtitle-on-rename remark under `ImportExtraFiles` is an inference from the absence of any subtitle code in `core/mediafiles`, not an observation.
- `Housekeeping` executing as a no-op (`core/commands/executor.rs:748`) is not a setting and is not counted, but it is the same pattern as `CleanUpRecycleBin`: a scheduled task whose row moves and whose work is not done.
