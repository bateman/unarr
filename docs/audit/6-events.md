# Audit 6 — reactions to internal events, commands, housekeeping: Sonarr vs Unarr

Static, read-only. Nothing was compiled or run. Sonarr paths are relative to
`src/NzbDrone.Core/` unless prefixed (`Api:` = `src/Sonarr.Api.V3/`, `Http:` = `src/Sonarr.Http/`).
Unarr paths are relative to `crates/`.

Status: PRESENT / PARTIAL / MISSING / DOCUMENTED (deliberate, with the place) / OOS (out of scope) / N/A / UNSURE.
Severity: high = data loss, wrong files, duplicate or lost downloads; medium = stale UI or a feature silently not working; low = cosmetic / log-only.

Enumeration basis: 139 `IHandle<>`/`IHandleAsync<>` declarations, 36 `IExecute<>`, 28 housekeepers, 11 scheduled tasks.

## Headline findings (MISSING / PARTIAL, by severity)

| # | What | Sev |
|---|------|-----|
| 1 | `Housekeeping` command is a no-op (`unarr-core/src/commands/executor.rs:748-752`, "nothing to do for this command yet"): none of the 28 housekeepers run, no VACUUM | medium |
| 2 | `CleanUpRecycleBin` is a no-op (same lines); `recycle_bin_cleanup_days` is loaded (`store/config.rs:263`) and never read. Files are still moved into the bin (`mediafiles/transfer.rs` `recycle_file`), so it grows without bound | medium |
| 3 | `SeriesScanSkippedEvent` from `DiskScanService.Scan` (root folder missing / root folder empty) is not raised: `mediafiles/scan.rs:185-193` returns with no event, so `after_scan` (`executor.rs:1257`) never runs | medium (see row) |
| 4 | `UpdateMediaInfoService.Handle(SeriesScannedEvent)`: files with no media info or an old schema revision are never re-probed (`MINIMUM_MEDIA_INFO_SCHEMA_REVISION` at `mediafiles/mediainfo.rs:21` has no user) | medium |
| 5 | `SeriesEditedService.Handle(SeriesEditedEvent)`: a changed series type does not queue `RefreshSeries` | medium |
| 6 | Startup defaults: `QualityDefinitionService.InsertMissingDefinitions` and `QualityProfileService` default profiles | medium (blank start) |
| 7 | `CustomFormatDeletedEvent`: the profile's `MinFormatScore/CutoffFormatScore/MinUpgradeFormatScore` are not reset when its last format goes | medium |
| 8 | Health `CheckOn` triggers: only provider (indexer / download client) changes, status changes, import outcome, start and `CheckHealth` re-run checks | low-medium |
| 9 | `UpdateEpisodeFileService.Handle(SeriesScannedEvent)`: file dates are set at import/rename only, not after a scan | low |
| 10 | SignalR: no `tag`, `system/task`, `rootfolder`, `qualitydefinition` broadcasts; no `episode` with `grabbed=true` on grab | low |
| 11 | Commands not known to `CommandBody::parse` (POST /command answers 500 "Sequence contains no matching element", `unarr-api/src/routes/command.rs:91`): `ResetQualityDefinitions`, `ResetApiKey`, `ClearLog`, `DeleteLogFiles`, `DeleteUpdateLogFiles`, `CheckForFinishedDownload`, `Restart`, `Shutdown` | low |
| 12 | Entry points whose event chain therefore never starts: `POST /series/import` (`SeriesImportedEvent`), `POST /history/failed/{id}` (`DownloadFailedEvent` by hand), `DELETE /blocklist/{id}` and `/blocklist/bulk` — none in `unarr-api/src/routes/mod.rs` | medium (route gap, not an event gap) |

## (a) Event handlers

| Event | Sonarr handler (file:line) and effect | Unarr | Consequence | Sev |
|---|---|---|---|---|
| ApplicationStartedEvent | `Jobs/Scheduler.cs:54`, `Jobs/TaskManager.cs:65`, `Messaging/Commands/CommandExecutor.cs:126`: start workers, reconcile scheduled tasks | PRESENT `unarr-core/src/commands/scheduler.rs:75`, `commands/executor.rs:436` | — | — |
| | `Messaging/Commands/CommandQueueManager.cs:276`: orphan started commands, requeue | PRESENT `commands/mod.rs:869-875`, `store/commands.rs:122` | — | — |
| | `HealthCheck/HealthCheckService.cs:152`: startup checks | PRESENT `commands/executor.rs:445` | — | — |
| | `Qualities/QualityDefinitionService.cs:104`: insert missing quality definitions, drop unknown ones | MISSING (grep `INSERT INTO quality_definitions` only in `store/catalog.rs`/`providers.rs` writers; no seeding in `db.rs`, `unarrd/src/main.rs`, importer) | A blank start has no definitions; a database imported from an older Sonarr lacks qualities added since | medium |
| | `Profiles/Qualities/QualityProfileService.cs:86`: six default profiles when none exist | MISSING (same search; no `default` profile builder outside tests) | Blank start: no profile to add a series with. Not documented as deliberate (docs mention blank starts only for config.xml) | medium |
| | `ThingiProvider/ProviderFactory.cs:170`: delete providers whose implementation is gone | DOCUMENTED `docs/DESIGN.md` §11 (unknown implementations are kept and shown masked) | — | — |
| | `Configuration/ConfigFileProvider.cs:474`: migrate / write defaults into config.xml | DOCUMENTED DESIGN §12 (an original config.xml is never rewritten) | — | — |
| | `Authentication/UserService.cs:160`: move legacy config.xml user into `Users` | UNSURE (importer reads `Users`, DESIGN §11; a config.xml-only legacy login was not traced) | — | low |
| | `ProgressMessaging/ProgressMessageTarget.cs:57` | N/A (progress goes through `Runtime::progress`, `executor.rs:611`) | — | — |
| | `Update/*`, `Instrumentation/ReconfigureSentry.cs:37` | OOS | — | — |
| ApplicationShutdownRequested | `Jobs/Scheduler.cs:64`, `CommandExecutor.cs:137`, `Instrumentation/DatabaseTarget.cs:154` | PRESENT `commands/executor.rs:472` (DESIGN §12, eight-second grace); no log DB | — | — |
| ApplicationStartingEvent | `Update/InstallUpdateService.cs:316` | OOS | — | — |
| TagsUpdatedEvent / AutoTagsUpdatedEvent | `Api:Tags/TagController.cs:62,68`: SignalR `tag` sync | MISSING (`settings_writes.rs` `tag_create/update/delete`, `auto_tagging_*` broadcast nothing; hub call sites listed by grep `hub()`) | A tag created in another tab/client appears only after reload | low |
| CommandExecutedEvent | `Jobs/TaskManager.cs:202`: last execution of the scheduled task | PRESENT `commands/scheduler.rs:141` | — | — |
| | `Api:System/Tasks/TaskController.cs:64`: SignalR `system/task` sync | MISSING (no `system/task` broadcast anywhere) | System > Tasks does not update while open | low |
| | `Api:Qualities/QualityDefinitionController.cs:72`: sync after `ResetQualityDefinitions` | MISSING (command itself missing, table b) | — | low |
| CommandUpdatedEvent | `Api:Commands/CommandController.cs:98` | PRESENT `unarr-api/src/state.rs:199` | — | — |
| ConfigFileSavedEvent | `Instrumentation/ReconfigureLogging.cs:162`: apply log level | MISSING, implicitly documented (DESIGN §14: levels follow `RUST_LOG`; `logLevel` is stored and echoed, `settings_writes.rs:556`) | Changing Log Level in the UI has no effect | low |
| ConfigSavedEvent | `Jobs/TaskManager.cs:222`: RSS and backup intervals | PRESENT `commands/executor.rs:507` (every tick; DESIGN §12) | — | — |
| | `Localization/LocalizationService.cs:193` | N/A | — | — |
| | `Http:Authentication/UiAuthorizationHandler.cs:50`: re-read AuthenticationRequired | UNSURE (not traced; DESIGN §14 says saved host settings apply in memory) | — | — |
| CustomFormatAddedEvent | `Profiles/Qualities/QualityProfileService.cs:154`: format added to every profile at 0 | PRESENT `unarr-api/src/routes/settings_writes.rs:1661-1684` | — | — |
| CustomFormatDeletedEvent | `QualityProfileService.cs:170`: removed from every profile; when none are left, MinFormatScore=0, CutoffFormatScore=0, MinUpgradeFormatScore=1 | PARTIAL `settings_writes.rs:1712-1728`: items removed, scores not reset | After the last custom format is deleted, a profile with a positive minimum score rejects every release | medium |
| DeleteCompletedEvent | `Notifications/NotificationService.cs:533`: flush media-server queue | PRESENT `commands/executor.rs:1401-1407`, `mediafiles/deletion.rs:142` | — | — |
| DownloadCanBeRemovedEvent | `Download/DownloadEventHub.cs:71` | PRESENT `mediafiles/completed.rs:319` | — | — |
| DownloadCompletedEvent | `DownloadEventHub.cs:48` (mark imported, remove) | PRESENT `mediafiles/completed.rs:292` | — | — |
| | `Download/History/DownloadHistoryService.cs:165` | PRESENT `mediafiles/events.rs:333` | — | — |
| | `NotificationService.cs:212` | PRESENT `notifications/mod.rs:723` | — | — |
| DownloadFailedEvent | `History/HistoryService.cs:233`, `Blocklisting/BlocklistService.cs:174`, `DownloadHistoryService.cs:186`, `Download/RedownloadFailedDownloadService.cs:32`, `DownloadEventHub.cs:26` | PRESENT `downloads/failed.rs:210-344` (`:279` blocklist, `:314` download history, `:340` redownload, `:344` removal), `downloads/redownload.rs:20` | — | — |
| DownloadIgnoredEvent | `HistoryService.cs:328`, `DownloadHistoryService.cs:211` | PRESENT `downloads/failed.rs:118-170` | — | — |
| DownloadsProcessedEvent | `Download/TrackedDownloads/DownloadMonitoringService.cs:184` (queue refreshed event) | PRESENT `commands/executor.rs:843-845` | — | — |
| | `NotificationService.cs:538` | PRESENT `commands/executor.rs:846` | — | — |
| | `Extras/ExtraService.cs:173` | OOS (extras) | — | — |
| EpisodeFileAddedEvent | `Tv/EpisodeService.cs:284`: link episodes, drop from the unmonitor cache | PRESENT `mediafiles/approve.rs:530`; cache replaced by the "still missing" filter in `mediafiles/scan.rs:320-327` | — | — |
| | `Api:EpisodeFiles/EpisodeFileController.cs:225` | PRESENT `unarr-api/src/relay.rs:16-21` | — | — |
| EpisodeFileDeletedEvent | `Tv/EpisodeService.cs:257` | PRESENT `mediafiles/deletion.rs:33-46` | — | — |
| | `History/HistoryService.cs:259` | PRESENT `mediafiles/events.rs:257` | — | — |
| | `MediaFiles/MediaFileDeletionService.cs:130` (empty folders) | PRESENT `mediafiles/deletion.rs:57`, called from `deletion.rs:140`, `approve.rs:460` | — | — |
| | `NotificationService.cs:392` | PRESENT `notifications/mod.rs:750` | — | — |
| | `Api:` EpisodeFile/Episode/Series controllers (`:231`, `:145`, `:303`) | PRESENT `unarr-api/src/relay.rs:22-33` | — | — |
| | `Extras/Files/ExtraFileService.cs:104` | OOS | — | — |
| EpisodeFileRenamedEvent | `HistoryService.cs:295` | PRESENT `mediafiles/events.rs:301` | — | — |
| EpisodeFolderCreatedEvent | `Extras/ExtraService.cs:142` | OOS | — | — |
| EpisodeGrabbedEvent | `HistoryService.cs:138`, `DownloadHistoryService.cs:95` | PRESENT `downloads/grab.rs:153,173` (`record_grab`) | — | — |
| | `Download/Pending/PendingReleaseService.cs:513` | PRESENT `downloads/grab.rs:160`, `downloads/pending.rs:523` | — | — |
| | `DownloadMonitoringService.cs:169` (debounced refresh) | PRESENT `commands/executor.rs:759-768` | — | — |
| | `NotificationService.cs:138` | PRESENT `downloads/grab.rs:154`, `notifications/mod.rs:488` | — | — |
| | `Api:Episodes/EpisodeControllerWithSignalR.cs:124`: `episode` updated with `grabbed=true` | MISSING (no `grabbed` in `unarr-api/src`; no episode broadcast on grab) | The episode row does not flip to "downloading" until the queue details arrive | low |
| EpisodeImportedEvent | `HistoryService.cs:190`, `DownloadHistoryService.cs:124` | PRESENT `mediafiles/events.rs:141` | — | — |
| | `DownloadMonitoringService.cs:179` (debounced client refresh) | PARTIAL: queue rebuilt/broadcast after processing (`executor.rs:845,882-884`); a rescan or drop-folder import (`executor.rs:1533`) asks no client | Queue item of an import made outside the queue lingers up to a minute (DESIGN §15 notes the same for memory) | low |
| | `NotificationService.cs:171` | PRESENT `notifications/mod.rs:694` | — | — |
| | `Api:` Episode `:136`, Series `:297` | PRESENT `unarr-api/src/relay.rs:16-21` | — | — |
| EpisodeInfoRefreshedEvent | `Download/TrackedDownloads/TrackedDownloadService.cs:267` | PRESENT `downloads/mod.rs:212,260`, `commands/executor.rs:1123` | — | — |
| | `Tv/EpisodeRefreshedService.cs:54` | PRESENT `commands/executor.rs:1146,1184` | — | — |
| HealthCheckFailed/Restored/CompleteEvent | `NotificationService.cs:480,508,548`; `Api:Health/HealthController.cs:44` | PRESENT `notifications/mod.rs:860,879`, `commands/executor.rs:234`, `unarr-api/src/state.rs:204` | — | — |
| IEvent (any) | `HealthCheck/HealthCheckService.cs:165`: after the 15-minute grace, re-run and notify | PRESENT `commands/executor.rs:283-297` | — | — |
| | same handler, `CheckOn` attributes: re-run a check when its event fires | PARTIAL. Wired: provider add/update/delete for indexers and download clients (`unarr-api/src/state.rs:227`, callers `indexer_writes.rs:210`, `downloadclient_writes.rs:193`), provider status change (`executor.rs:365`), import ok/failed (`executor.rs:333`). Not wired: `ConfigSavedEvent` (ImportMechanismCheck), `ModelEvent<RootFolder>` and `ModelEvent<RemotePathMapping>` (root folder, remote path, sorting checks), `ProviderUpdated/Deleted<IImportList>` (ImportListRootFolderCheck), `SeriesDeletedEvent`/`SeriesMovedEvent` (RootFolderCheck), `SeriesUpdatedEvent`/`SeriesRefreshCompleteEvent` (RemovedSeriesCheck, which does not exist in Unarr) | A health warning stays, or stays hidden, until the 6-hour `CheckHealth` or an unrelated provider edit | low-medium |
| ManualInteractionRequiredEvent | `NotificationService.cs:340`; `DownloadMonitoringService.cs:174` | PRESENT `notifications/mod.rs:808`; refresh as for EpisodeImportedEvent | — | — |
| MediaCoversUpdatedEvent | `Api:Series/SeriesController.cs:352` | PRESENT `commands/executor.rs:1223-1225` | — | — |
| | `Extras/ExtraService.cs:118` | OOS | — | — |
| ModelEvent<RootFolder> (and other `RestControllerWithSignalR`) | `Http:REST/RestControllerWithSignalR.cs:33`: SignalR `rootfolder` | MISSING (`settings_writes.rs:904,928` broadcast nothing) | Root folder list stale in other tabs | low |
| QueueUpdatedEvent / PendingReleasesUpdatedEvent | `Api:Queue/Queue*.cs` (`:397,403`, `:75,81`, `:62,68`) | PRESENT `unarr-api/src/routes/queue.rs:60-69` | — | — |
| ProviderAdded/Updated/DeletedEvent<T> | `Api:ProviderControllerBase.cs:20-22`: SignalR | PRESENT `indexer_writes.rs:206`, `downloadclient_writes.rs:189`, `notification_writes.rs:143`, `importlist_writes.rs:227`; metadata OOS | — | — |
| ProviderDeletedEvent<T> | `ThingiProvider/Status/ProviderStatusServiceBase.cs:20`: status row deleted | PRESENT `store/providers.rs:79` | — | — |
| ProviderDeletedEvent<IImportList> | `ImportLists/ImportListItems/ImportListItemService.cs:74`; `ImportLists/ImportListSyncService.cs:340` (TryCleanLibrary) | PRESENT `store/providers.rs:81` (`import_lists.rs:187`), `commands/executor.rs:202`, `importlist_writes.rs:337,488` | — | — |
| ProviderUpdatedEvent<IImportList> | `ImportLists/ImportListUpdatedHandler.cs:16`: sync that list | PRESENT `importlist_writes.rs:324,464` | — | — |
| ProviderUpdated/DeletedEvent<IIndexer> | `Indexers/CachedIndexerSettingsProvider.cs:64,69`: clear cache | UNSURE / likely N/A (no equivalent cache found by name; not traced whether seed criteria are cached) | — | low |
| RenameCompletedEvent | `NotificationService.cs:543` | PRESENT `commands/executor.rs:916,952` | — | — |
| RssSyncCompleteEvent | `PendingReleaseService.cs:518` | PRESENT `commands/executor.rs:810-818`, `downloads/pending.rs:560` | — | — |
| SeriesAddCompletedEvent | `NotificationService.cs:428` | PRESENT `commands/executor.rs:1290` | — | — |
| SeriesAddedEvent | `Tv/SeriesAddedHandler.cs:20`, `DataAugmentation/Scene/SceneMappingService.cs:290`, `TrackedDownloadService.cs:292` | PRESENT `commands/executor.rs:1333-1358`, caller `series_writes.rs:269`, import lists `executor.rs:729` | — | — |
| SeriesBulkEditedEvent | `Api:Series/SeriesController.cs:343` | PRESENT `series_writes.rs:438-443` | — | — |
| SeriesDeletedEvent | `BlocklistService.cs:207`, `DownloadHistoryService.cs:230`, `PendingReleaseService.cs:508`, `HistoryService.cs:359`, `MediaFiles/MediaFileService.cs:111`, `Tv/EpisodeService.cs:304` | PRESENT `store/series.rs:187-208` (one transaction) | — | — |
| | `TrackedDownloadService.cs:308` | PRESENT `commands/executor.rs:1378` | — | — |
| | `ImportLists/Exclusions/ImportListExclusionService.cs:70` | PRESENT `tv/series.rs:594-611` | — | — |
| | `MediaFiles/MediaFileDeletionService.cs:91` | PRESENT `tv/series.rs:618`, `commands/executor.rs:1379-1391`. Nit: the original aborts the whole handler on a parent/equal path, Unarr skips that one series | — | — |
| | `MediaCover/MediaCoverService.cs:240` | PRESENT `commands/executor.rs:1395-1400` | — | — |
| | `NotificationService.cs:455`; `Api:Series/SeriesController.cs:328` | PRESENT `commands/executor.rs:1394`, `series_writes.rs:376-380` | — | — |
| | `Extras/Files/ExtraFileService.cs:98` | OOS | — | — |
| SeriesEditedEvent | `Tv/SeriesEditedService.cs:18`: series type changed -> `RefreshSeriesCommand` | MISSING (`series_writes.rs:281-350` and `tv/series.rs:361-420` write the type and queue nothing; grep `RefreshSeries` in both) | Switching a series to Anime/Daily leaves episodes (absolute numbers, scene data) as they were until a later refresh finds the series due | medium |
| | `Api:Series/SeriesController.cs:320` | PRESENT `series_writes.rs:346` | — | — |
| SeriesImportedEvent | `Tv/SeriesAddedHandler.cs:25` (refresh all imported), `SceneMappingService.cs:298` | MISSING: no `POST /series/import` route (`unarr-api/src/routes/mod.rs:48-60`), so nothing publishes it | The UI's Library Import page cannot add existing folders in bulk; not listed in the docs | medium |
| SeriesRefreshStartingEvent | `SceneMappingService.cs:282`, `DataAugmentation/Xem/XemService.cs:257` | PRESENT `commands/executor.rs:1000-1005`, `tv/scene.rs:30,151` | — | — |
| SeriesRenamedEvent | `NotificationService.cs:298`; `Api:Series/SeriesController.cs:337` | PRESENT `notifications/mod.rs:782`, `unarr-api/src/relay.rs:37` | — | — |
| | `Extras/ExtraService.cs:162` | OOS | — | — |
| SeriesScanSkippedEvent | `Tv/SeriesScannedHandler.cs:81`, published by `Tv/RefreshSeriesService.cs:179,186` (rescan setting) | PRESENT `commands/executor.rs:1243-1246` | — | — |
| | same handler, published by `MediaFiles/DiskScanService.cs:88,95` (root folder missing / empty) | MISSING: `mediafiles/scan.rs:185-193` returns `Ok(events)` empty; `emit_import_events` (`executor.rs:965`) only reacts to `SeriesScanned` | A series added while its root folder is empty (first series of a new library) or unmounted never gets its post-add actions: the monitoring preset is not applied, no `SeriesAdd` notification, no initial search; `add_options` stays set, so `episodes_refreshed` (`executor.rs:1148`) also skips the search of newly aired episodes. The actions then fire late, on the first scan that completes. With a preset other than "all", episodes that should be unmonitored stay monitored meanwhile | medium (unwanted grabs possible) |
| SeriesScannedEvent | `Tv/SeriesScannedHandler.cs:76` | PRESENT `commands/executor.rs:965,1257` | — | — |
| | `Tv/EpisodeService.cs:310` (unmonitor files gone from disk) | PRESENT `mediafiles/scan.rs:302-330` | — | — |
| | `MediaFiles/MediaInfo/UpdateMediaInfoService.cs:40`: re-probe every file with null media info or schema below the minimum | MISSING: `mediafiles/scan.rs:262-284` re-probes only files whose size changed | Files imported from Sonarr without media info (or scanned while ffprobe was absent) keep none: media-info naming tokens and custom formats on existing files silently do not match | medium |
| | `MediaFiles/UpdateEpisodeFileService.cs:146`: file dates per `FileDate` setting | MISSING: `change_file_date` has one caller, the import (`mediafiles/approve.rs:317`) | With "Change File Date" on, existing files and corrected air dates are not applied by a scan | low |
| | `Extras/ExistingExtraFileService.cs:44`, `Extras/ExtraService.cs:131` | OOS | — | — |
| SeriesUpdatedEvent | `Xem/XemService.cs:235`, `MediaCover/MediaCoverService.cs:233`, `Api:Series/SeriesController.cs:314` | PRESENT `commands/executor.rs:1217-1227` | — | — |
| TrackedDownloadRefreshedEvent | `Queue/QueueService.cs:97` | PRESENT `downloads/mod.rs:319-326` | — | — |
| TrackedDownloadsRemovedEvent | `DownloadMonitoringService.cs:191` | PRESENT `downloads/mod.rs:202-209`; broadcasts `unarr-api/src/routes/queue.rs:451-544` | — | — |
| UntrackedDownloadCompletedEvent | `NotificationService.cs:256` | PRESENT handler `notifications/mod.rs:723`; the manual-import emission for an untracked download was not traced (UNSURE) | — | low |
| UpdateInstalledEvent | `NotificationService.cs:318` | OOS when audited; PRESENT since 21 November 2026: `Runtime::application_started` (`commands/executor.rs`) records Unarr's version in `update_history` (`store/update_history.rs`) at each start and, when it differs from the last one, sends Application Update on both personalities | — | — |

## (b) Commands (`IExecute<>`)

| Command | Sonarr (file:line) | Unarr | Consequence | Sev |
|---|---|---|---|---|
| RssSync | `Indexers/RssSyncService.cs:66` | PRESENT `commands/executor.rs:778` | — | — |
| RefreshMonitoredDownloads | `DownloadMonitoringService.cs:158` | PRESENT `executor.rs:824` | — | — |
| ProcessMonitoredDownloads | `Download/DownloadProcessingService.cs:47` | PRESENT `executor.rs:840` | — | — |
| CheckForFinishedDownload (deprecated alias) | `DownloadMonitoringService.cs:163` | MISSING (not in `commands/mod.rs` names) | Old third-party callers get a 500 | low |
| EpisodeSearch / SeasonSearch / SeriesSearch / MissingEpisodeSearch / CutoffUnmetEpisodeSearch | `IndexerSearch/*.cs` | PRESENT `executor.rs:645-688,1614,1680,1720` | — | — |
| ClearBlocklist | `BlocklistService.cs:169` | PRESENT `executor.rs:690`, `store/history.rs:306` | — | — |
| MessagingCleanup | `Messaging/Commands/CleanupCommandMessagingService.cs:12` | PRESENT `executor.rs:694`, `commands/mod.rs:992` | — | — |
| RefreshSeries | `Tv/RefreshSeriesService.cs:211` | PRESENT `executor.rs:992` | — | — |
| RescanSeries | `MediaFiles/DiskScanService.cs:279` | PRESENT `executor.rs:1508` (see the skipped-scan and media-info rows above) | — | — |
| DownloadedEpisodesScan | `MediaFiles/DownloadedEpisodesCommandService.cs:69` | PRESENT `executor.rs:1539` | — | — |
| ManualImport | `MediaFiles/EpisodeImport/Manual/ManualImportService.cs:480` | PRESENT `executor.rs:857` | — | — |
| RenameFiles / RenameSeries | `MediaFiles/RenameEpisodeFileService.cs:161,173` | PRESENT `executor.rs:889,921` | — | — |
| MoveSeries / BulkMoveSeries | `Tv/MoveSeriesService.cs:94,100` | PRESENT `executor.rs:1412,1485` (no `SeriesMovedEvent` health re-check, see table a) | — | — |
| UpdateSceneMapping | `DataAugmentation/Scene/SceneMappingService.cs:306` | PRESENT `executor.rs:714` | — | — |
| CheckHealth | `HealthCheck/HealthCheckService.cs:137` | PRESENT `executor.rs:718` | — | — |
| ImportListSync | `ImportLists/ImportListSyncService.cs:268` | PRESENT `executor.rs:722` (Simkl only, DESIGN §14) | — | — |
| Backup | `Backup/BackupService.cs:256` incl. `CleanupOldBackups` | PRESENT `executor.rs:736`, retention `backup.rs:153-210` | — | — |
| Housekeeping | `Housekeeping/HousekeepingService.cs:45` | MISSING: accepted, does nothing (`executor.rs:748-752`). `docs/phase-2-acquisition.md:157` lists housekeeping under the scheduler's scope; no doc records it as deferred | See table c | medium |
| CleanUpRecycleBin | `MediaFiles/RecycleBinProvider.cs:210` | MISSING: accepted, does nothing (`executor.rs:748-752`) | Recycling bin never emptied; the Cleanup Days setting is inert | medium |
| ResetQualityDefinitions | `Qualities/QualityDefinitionService.cs:111` | MISSING | Settings > Quality "Reset Definitions" fails | low |
| ResetApiKey | `Configuration/ConfigFileProvider.cs:481` | MISSING | "Reset API Key" fails | low |
| ClearLog | `Instrumentation/LogService.cs:26` | MISSING | System > Events "Clear" fails | low |
| DeleteLogFiles / DeleteUpdateLogFiles | `Instrumentation/DeleteLogFilesService.cs:27,33` | MISSING | System > Log Files "Delete" fails; rotation itself is covered (DESIGN §14) | low |
| Restart / Shutdown | `Lifecycle/LifecycleService.cs:58,63` (via `/system/restart`, `/system/shutdown`) | MISSING (no routes) | UI buttons fail; DESIGN §14 says settings apply without a restart | low |
| ApplicationUpdateCheck | `Update/InstallUpdateService.cs:279` | OOS; accepted as a no-op `executor.rs:749` | — | — |
| ApplicationUpdate | `Update/InstallUpdateService.cs:287` | OOS | — | — |
| TestCommand / UnknownCommand | `Messaging/Commands/*.cs` | N/A | — | — |

## (c) Housekeepers and scheduled tasks

Scheduled tasks (`Jobs/TaskManager.cs:65-135`): all eleven are registered with the same intervals in
`unarr-core/src/commands/scheduler.rs:25-62`. Two of them run empty (Housekeeping, CleanUpRecycleBin), one is OOS (ApplicationUpdateCheck).

| Housekeeper (`Housekeeping/Housekeepers/`) | Effect | Unarr | Consequence | Sev |
|---|---|---|---|---|
| CleanupOrphanedHistoryItems | history rows of a missing series or missing episode | PARTIAL: series side by `store/series.rs:187`; episode side MISSING (a refresh deletes episodes at `tv/refresh.rs:113`, `store/episodes.rs:79`, history untouched) | History rows pointing at episodes that no longer exist accumulate; how the History page renders them was not traced (UNSURE) | low-medium |
| CleanupOrphanedEpisodeFiles | file rows linked to no episode | PARTIAL: done per series by the scan (`mediafiles/scan.rs:133-137`), not by a schedule | Rows linger until that series is scanned | low |
| CleanupOrphanedEpisodes / Blocklist / PendingReleases | rows of a missing series | PRESENT by construction (`store/series.rs:187`); importer drops orphans (DESIGN §11) | — | — |
| CleanupDownloadClientUnavailablePendingReleases | pending releases with reason DownloadClientUnavailable or Fallback older than 14 days | MISSING (only deletes in `store/pending.rs:104` by id; no age rule) | Such rows are re-decided on every RSS sync indefinitely while a client stays unavailable | low-medium |
| CleanupOrphaned{Indexer,DownloadClient,ImportList,Notification}Status | status rows of deleted providers | PRESENT by construction `store/providers.rs:79` | — | — |
| FixFuture{Indexer,DownloadClient,ImportList,Notification}StatusTimes | clamp back-off times that lie in the future beyond the level's period | MISSING | After a clock jump back, a provider can stay disabled far longer than intended | low |
| FixFutureRunScheduledTasks | `LastExecution` in the future set to now | MISSING | After a clock jump back, scheduled tasks do not run until the old time is reached | low |
| CleanupQualityProfileFormatItems | profiles reconciled with the custom formats that exist | PARTIAL: kept in line on create/delete (`settings_writes.rs:1661,1712`); no repair pass, no score reset | Only matters for an imported or hand-edited database | low |
| CleanupUnusedTags | delete tags used by nothing | MISSING | Unused tags stay listed | low |
| CleanupCommandQueue | `CleanCommands` | PRESENT via MessagingCleanup `commands/mod.rs:992` | — | — |
| UpdateCleanTitleForSeries | recompute `CleanTitle` | MISSING as a pass (UNSURE whether the refresh rewrites it) | Title matching of a series whose stored clean title is stale | low |
| CleanupAdditionalNamingSpecs / CleanupAdditionalUsers | keep one row | MISSING | None expected | low |
| TrimLogDatabase | trim `Logs` | N/A: `GET /log` is an in-memory buffer (DESIGN §14) | — | — |
| main database VACUUM (`HousekeepingService.cs:43`) | compact | MISSING (only `VACUUM INTO` for backups, `backup.rs:189`) | Database file does not shrink | low |
| CleanupAbsolutePathMetadataFiles, CleanupDuplicateMetadataFiles, CleanupOrphanedMetadataFiles, DeleteBadMediaCovers | metadata files | OOS | — | — |
| CleanupTemporaryUpdateFiles | update sandbox | OOS | — | — |
| Recycle bin cleanup (`MediaFiles/RecycleBinProvider.cs:161`) | delete bin files older than `RecycleBinCleanupDays` | MISSING (table b) | Disk fills | medium |
| Old backups (`Backup/BackupService.cs:228`) | retention | PRESENT `backup.rs:207` | — | — |
| Log files | size/rotation | DOCUMENTED DESIGN §14 (1 MB, 50 archives, not settings) | — | — |
| MediaCover folders of series gone outside the API | — | DOCUMENTED DESIGN §15 | — | — |

## Documented, not re-reported

- Notifications other than Discord/Webhook/Plex send nothing (DESIGN §13, §15). The plex.tv ping is sent since 19 November 2026, except for a blank token (DESIGN §21.5).
- Import lists other than Simkl not driven (DESIGN §14, §15).
- Kodi/Plex metadata writers and extras not driven (phase-3b "Out of scope", DESIGN §15).
- Health checks are a subset: acquisition (DESIGN §12) plus import (§13). Absent and not individually listed in the docs: RemovedSeriesCheck, ImportListStatusCheck, NotificationStatusCheck, ProxyCheck, ApiKeyValidationCheck, AllowedHostsCheck, SystemTimeCheck, AppDataLocationCheck, IndexerJackettAllCheck, PackageGlobalMessageCheck.
- Tracked downloads no client reports leave the cache (DESIGN §12); restore applied at next start (§14); `system/routes` absent (§14); `.unarr` ignore file not read (phase-3a).
- Auto-tagging sees the refreshed series, not the stale copy (DESIGN §14).

## Not verified

- Whether the History page tolerates rows whose episode is gone.
- Whether `UntrackedDownloadCompletedEvent` is raised by a manual import without a download id.
- `UiAuthorizationHandler` and `CachedIndexerSettingsProvider` equivalents.
- (Verified: the UI posts to `/series/import`, `frontend/src/Store/Actions/importSeriesActions.js:243`; controller `Api:Series/SeriesImportController.cs:8`.)
