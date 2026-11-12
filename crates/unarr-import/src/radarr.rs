//! Import of a Radarr `radarr.db` into Unarr's schema.
//!
//! The twin of [`crate::sonarr`]: the tables both applications have are read
//! by [`crate::shared`], the movie side here. Radarr keeps what its metadata
//! server knows in `MovieMetadata` (one row per TMDB id, also for movies a
//! collection or an import list only names) and the user's side in
//! `Movies`; Unarr keeps the same split.

use anyhow::Context;
use chrono::Utc;
use serde_json::{json, Value};
use unarr_core::model::*;
use unarr_core::movies::model::*;
use unarr_core::movies::store as movies;
use unarr_core::store;
use unarr_core::store::import_lists::SyncStatus;
use unarr_core::store::provider_status::ProviderStatus;
use unarr_core::{time, Db};

use crate::legacy::{
    open_legacy, protocol, provider_row as provider, quality_model, read_statuses, read_table,
    skipped_tables, ImportReport, LegacyRow,
};
use crate::shared::{provider_sequences, SharedTables};

/// Oldest migration level accepted: 208 is where `MovieMetadata` and
/// `Collections` have the shape read here. Only 242 (Radarr 6.4) was
/// checked against a real database; the levels in between are best effort,
/// columns a later migration added being defaulted when absent.
pub const MIN_MIGRATION: i64 = 208;
/// Newest migration level the mapping was written against (Radarr 6.4.4).
pub const MAX_KNOWN_MIGRATION: i64 = 242;

/// Legacy tables the importer reads. Anything else with rows is logged.
const HANDLED_TABLES: &[&str] = &[
    "Tags",
    "RootFolders",
    "QualityDefinitions",
    "CustomFormats",
    "QualityProfiles",
    "MovieMetadata",
    "Movies",
    "MovieFiles",
    "AlternativeTitles",
    "MovieTranslations",
    "Credits",
    "Collections",
    "ImportExclusions",
    "ImportListMovies",
    "History",
    "Indexers",
    "DownloadClients",
    "Notifications",
    "ImportLists",
    "Metadata",
    "DelayProfiles",
    "ReleaseProfiles",
    "RemotePathMappings",
    "CustomFilters",
    "AutoTagging",
    "Users",
    "ScheduledTasks",
    "Blocklist",
    "SubtitleFiles",
    "ExtraFiles",
    "MetadataFiles",
    "NamingConfig",
    "Config",
    "PendingReleases",
    "DownloadHistory",
    "IndexerStatus",
    "DownloadClientStatus",
    "ImportListStatus",
    "VersionInfo",
    "sqlite_sequence",
];

fn movie_status(v: i64) -> MovieStatus {
    MovieStatus::from_number(v).unwrap_or(MovieStatus::Tba)
}

/// Radarr's `MonitorTypes`, in declaration order.
fn monitor_type(v: i64) -> &'static str {
    match v {
        1 => "movieAndCollection",
        2 => "none",
        _ => "movieOnly",
    }
}

/// A row of `PendingReleases`, kept as the original stored it until the
/// acquisition milestone reads it.
struct LegacyPending {
    id: i64,
    movie_id: i64,
    title: String,
    added: String,
    release: Value,
    parsed_info: Option<Value>,
    reason: i64,
    additional_info: Option<Value>,
}

/// Import `legacy_db` into `db`, replacing nothing: the target must be empty.
pub fn import(legacy_db: &std::path::Path, db: &Db) -> anyhow::Result<ImportReport> {
    let legacy = open_legacy(legacy_db).with_context(|| format!("opening {}", legacy_db.display()))?;
    let migration_version: i64 = legacy
        .query_row("SELECT max(Version) FROM VersionInfo", [], |r| r.get(0))
        .context("reading VersionInfo: is this a Radarr database?")?;
    let mut report = ImportReport {
        migration_version,
        ..ImportReport::default()
    };
    let has_movies: bool = legacy.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'MovieMetadata')",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        has_movies,
        "{} has no MovieMetadata table: it is not a Radarr database, or one older than Radarr 4.1",
        legacy_db.display()
    );
    tracing::info!(
        version = report.migration_version,
        "legacy database migration level"
    );
    anyhow::ensure!(
        report.migration_version >= MIN_MIGRATION,
        "radarr.db is at migration {}, older than {} which this importer supports; upgrade Radarr first",
        report.migration_version,
        MIN_MIGRATION
    );
    if report.migration_version > MAX_KNOWN_MIGRATION {
        let note = format!(
            "radarr.db is at migration {}, newer than {} which this importer knows; unknown columns are ignored",
            report.migration_version, MAX_KNOWN_MIGRATION
        );
        tracing::warn!("{note}");
        report.notes.push(note);
    }

    // --- read everything first, so a mapping error leaves the target untouched
    let shared = SharedTables::read(&legacy)?;
    let sequences = provider_sequences(&legacy);
    let mut profile_languages = Vec::new();
    let quality_profiles = read_table(&legacy, "QualityProfiles", |r| {
        let id: i64 = r.req("Id")?;
        profile_languages.push((id, r.or("Language", -1)?));
        Ok(QualityProfile {
            id,
            name: r.req("Name")?,
            cutoff: r.req("Cutoff")?,
            upgrade_allowed: r.flag("UpgradeAllowed", false)?,
            items: r.json("Items")?,
            format_items: r.json("FormatItems")?,
            min_format_score: r.or("MinFormatScore", 0)?,
            cutoff_format_score: r.or("CutoffFormatScore", 0)?,
            min_upgrade_format_score: r.or("MinUpgradeFormatScore", 1)?,
        })
    })?;
    let metadata = read_table(&legacy, "MovieMetadata", |r| {
        Ok(MovieMetadata {
            id: r.req("Id")?,
            tmdb_id: r.req("TmdbId")?,
            imdb_id: r.opt::<String>("ImdbId")?,
            title: r.or("Title", String::new())?,
            sort_title: r.opt::<String>("SortTitle")?,
            clean_title: r.opt::<String>("CleanTitle")?,
            original_title: r.opt::<String>("OriginalTitle")?,
            clean_original_title: r.opt::<String>("CleanOriginalTitle")?,
            original_language_id: r.or("OriginalLanguage", 1)?,
            status: movie_status(r.or("Status", 0)?),
            overview: r.opt::<String>("Overview")?,
            images: r.json("Images")?,
            genres: r.json("Genres")?,
            keywords: r.json("Keywords")?,
            ratings: r.json_opt("Ratings")?.unwrap_or(json!({})),
            recommendations: r.json("Recommendations")?,
            runtime: r.or("Runtime", 0)?,
            in_cinemas: r.timestamp("InCinemas")?,
            physical_release: r.timestamp("PhysicalRelease")?,
            digital_release: r.timestamp("DigitalRelease")?,
            year: r.or("Year", 0)?,
            secondary_year: r.opt("SecondaryYear")?,
            certification: r.opt::<String>("Certification")?,
            youtube_trailer_id: r.opt::<String>("YouTubeTrailerId")?,
            studio: r.opt::<String>("Studio")?,
            website: r.opt::<String>("Website")?,
            popularity: r.or("Popularity", 0.0)?,
            collection_tmdb_id: r.or("CollectionTmdbId", 0)?,
            collection_title: r.opt::<String>("CollectionTitle")?,
            last_info_sync: r.timestamp("LastInfoSync")?,
        })
    })?;
    let by_metadata: std::collections::HashMap<i64, &MovieMetadata> =
        metadata.iter().map(|m| (m.id, m)).collect();
    let mut orphans = 0;
    let all_movies = read_table(&legacy, "Movies", |r| {
        let metadata_id: i64 = r.req("MovieMetadataId")?;
        let Some(md) = by_metadata.get(&metadata_id) else {
            orphans += 1;
            return Ok(None);
        };
        Ok(Some(Movie {
            id: r.req("Id")?,
            metadata: (*md).clone(),
            path: r.req("Path")?,
            monitored: r.flag("Monitored", true)?,
            quality_profile_id: r.req("QualityProfileId")?,
            minimum_availability: movie_status(r.or("MinimumAvailability", 3)?),
            movie_file_id: r.or("MovieFileId", 0)?,
            tags: r.json("Tags")?,
            added: r.timestamp("Added")?,
            add_options: r.json_opt("AddOptions")?,
            last_search_time: r.timestamp("LastSearchTime")?,
        }))
    })?;
    if orphans > 0 {
        report.notes.push(format!(
            "dropped {orphans} movie(s) whose metadata row no longer exists"
        ));
    }
    // Tags a movie names but `Tags` does not hold are dropped, with a note.
    let tag_ids: std::collections::HashSet<i64> = shared.tags.iter().map(|t| t.id).collect();
    let all_movies: Vec<Movie> = all_movies
        .into_iter()
        .flatten()
        .map(|mut m| {
            let before = m.tags.len();
            m.tags.retain(|t| tag_ids.contains(t));
            if m.tags.len() != before {
                report.notes.push(format!(
                    "movie {} ({}): dropped {} unknown tag id(s)",
                    m.id,
                    m.metadata.title,
                    before - m.tags.len()
                ));
            }
            m
        })
        .collect();
    let movie_files = read_table(&legacy, "MovieFiles", |r| {
        Ok(MovieFile {
            id: r.req("Id")?,
            movie_id: r.req("MovieId")?,
            relative_path: r.or("RelativePath", String::new())?,
            original_file_path: r.opt::<String>("OriginalFilePath")?,
            size: r.or("Size", 0)?,
            date_added: r.timestamp_req("DateAdded")?,
            scene_name: r.opt::<String>("SceneName")?,
            release_group: r.opt::<String>("ReleaseGroup")?,
            edition: r.opt::<String>("Edition")?,
            quality: quality_model(r.json_opt("Quality")?),
            languages: r.json("Languages")?,
            media_info: r.json_opt("MediaInfo")?,
            indexer_flags: r.or("IndexerFlags", 0)?,
        })
    })?;
    let alternative_titles = read_table(&legacy, "AlternativeTitles", |r| {
        Ok(AlternativeTitle {
            id: r.req("Id")?,
            movie_metadata_id: r.req("MovieMetadataId")?,
            title: r.or("Title", String::new())?,
            clean_title: r.opt::<String>("CleanTitle")?,
            // `SourceType`: Tmdb, Mappings, User, Indexer.
            source_type: match r.or("SourceType", 0)? {
                1 => "mappings",
                2 => "user",
                3 => "indexer",
                _ => "tmdb",
            }
            .to_string(),
        })
    })?;
    let translations = read_table(&legacy, "MovieTranslations", |r| {
        Ok(MovieTranslation {
            id: r.req("Id")?,
            movie_metadata_id: r.req("MovieMetadataId")?,
            language_id: r.or("Language", 0)?,
            title: r.opt::<String>("Title")?,
            clean_title: r.opt::<String>("CleanTitle")?,
            overview: r.opt::<String>("Overview")?,
        })
    })?;
    let credits = read_table(&legacy, "Credits", |r| {
        Ok(Credit {
            id: r.req("Id")?,
            movie_metadata_id: r.req("MovieMetadataId")?,
            credit_tmdb_id: r.opt::<String>("CreditTmdbId")?,
            person_tmdb_id: r.or("PersonTmdbId", 0)?,
            name: r.opt::<String>("Name")?,
            images: r.json("Images")?,
            character: r.opt::<String>("Character")?,
            order: r.or("Order", 0)?,
            job: r.opt::<String>("Job")?,
            department: r.opt::<String>("Department")?,
            credit_type: if r.or("Type", 0)? == 1 { "crew" } else { "cast" }.to_string(),
        })
    })?;
    let collections = read_table(&legacy, "Collections", |r| {
        Ok(Collection {
            id: r.req("Id")?,
            tmdb_id: r.req("TmdbId")?,
            title: r.or("Title", String::new())?,
            sort_title: r.opt::<String>("SortTitle")?,
            clean_title: r.opt::<String>("CleanTitle")?,
            overview: r.opt::<String>("Overview")?,
            images: r.json("Images")?,
            monitored: r.flag("Monitored", false)?,
            quality_profile_id: r.or("QualityProfileId", 0)?,
            root_folder_path: r.opt::<String>("RootFolderPath")?,
            minimum_availability: movie_status(r.or("MinimumAvailability", 3)?),
            search_on_add: r.flag("SearchOnAdd", false)?,
            tags: r.json("Tags")?,
            added: r.timestamp("Added")?,
            last_info_sync: r.timestamp("LastInfoSync")?,
        })
    })?;
    let exclusions = read_table(&legacy, "ImportExclusions", |r| {
        Ok(ImportExclusion {
            id: r.req("Id")?,
            tmdb_id: r.req("TmdbId")?,
            movie_title: r.opt::<String>("MovieTitle")?,
            movie_year: r.or("MovieYear", 0)?,
        })
    })?;
    let import_list_movies = read_table(&legacy, "ImportListMovies", |r| {
        Ok((
            r.req::<i64>("Id")?,
            r.req::<i64>("ListId")?,
            r.req::<i64>("MovieMetadataId")?,
        ))
    })?;
    let history = read_table(&legacy, "History", |r| {
        Ok(MovieHistoryEntry {
            id: r.req("Id")?,
            movie_id: r.or("MovieId", 0)?,
            source_title: r.opt::<String>("SourceTitle")?,
            date: r.timestamp_req("Date")?,
            event_type: MovieHistoryEvent::from_number(r.or("EventType", 0)?)
                .unwrap_or(MovieHistoryEvent::Unknown),
            quality: quality_model(r.json_opt("Quality")?),
            languages: r.json("Languages")?,
            data: r.json_opt("Data")?.unwrap_or(json!({})),
            download_id: r.opt::<String>("DownloadId")?,
        })
    })?;
    let blocklist = read_table(&legacy, "Blocklist", |r| {
        Ok(MovieBlocklistEntry {
            id: r.req("Id")?,
            movie_id: r.or("MovieId", 0)?,
            source_title: r.opt::<String>("SourceTitle")?,
            quality: quality_model(r.json_opt("Quality")?),
            date: r.timestamp_req("Date")?,
            published_date: r.timestamp("PublishedDate")?,
            size: r.opt("Size")?,
            protocol: protocol(r.or("Protocol", 0)?),
            indexer: r.opt::<String>("Indexer")?,
            message: r.opt::<String>("Message")?,
            torrent_info_hash: r.opt::<String>("TorrentInfoHash")?,
            languages: r.json("Languages")?,
            indexer_flags: r.or("IndexerFlags", 0)?,
        })
    })?;
    let download_history = read_table(&legacy, "DownloadHistory", |r| {
        Ok(MovieDownloadHistoryEntry {
            id: r.req("Id")?,
            event_type: r.or("EventType", 0)?,
            movie_id: r.or("MovieId", 0)?,
            download_id: r.or("DownloadId", String::new())?,
            source_title: r.or("SourceTitle", String::new())?,
            date: r.timestamp_req("Date")?,
            protocol: protocol(r.or("Protocol", 0)?),
            indexer_id: r.or("IndexerId", 0)?,
            download_client_id: r.or("DownloadClientId", 0)?,
            release: r.json_opt("Release")?,
            data: r.json_opt("Data")?.unwrap_or(json!({})),
        })
    })?;
    let pending_releases = read_table(&legacy, "PendingReleases", |r| {
        Ok(LegacyPending {
            id: r.req("Id")?,
            movie_id: r.or("MovieId", 0)?,
            title: r.or("Title", String::new())?,
            added: time::format_utc(r.timestamp_req("Added")?),
            release: r.json_opt("Release")?.unwrap_or(Value::Null),
            parsed_info: r.json_opt("ParsedMovieInfo")?,
            reason: r.or("Reason", 0)?,
            additional_info: r.json_opt("AdditionalInfo")?,
        })
    })?;
    let extra = |kind: ExtraKind| {
        move |r: &LegacyRow| -> anyhow::Result<MovieExtraFile> {
            let subtitle = kind == ExtraKind::Subtitle;
            let metadata = kind == ExtraKind::Metadata;
            Ok(MovieExtraFile {
                kind,
                id: r.req("Id")?,
                movie_id: r.req("MovieId")?,
                movie_file_id: r.opt("MovieFileId")?,
                relative_path: r.opt::<String>("RelativePath")?.unwrap_or_default(),
                extension: r.opt::<String>("Extension")?.unwrap_or_default(),
                added: r.timestamp("Added")?,
                last_updated: r.timestamp("LastUpdated")?,
                language: if subtitle { r.or("Language", 0)? } else { 0 },
                language_tags: if subtitle {
                    r.json("LanguageTags")?
                } else {
                    Vec::new()
                },
                title: if subtitle { r.opt::<String>("Title")? } else { None },
                copy: if subtitle { r.or("Copy", 0)? } else { 0 },
                consumer: if metadata {
                    r.opt::<String>("Consumer")?
                } else {
                    None
                },
                metadata_type: if metadata { r.opt("Type")? } else { None },
                hash: if metadata { r.opt::<String>("Hash")? } else { None },
            })
        }
    };
    let subtitle_files = read_table(&legacy, "SubtitleFiles", extra(ExtraKind::Subtitle))?;
    let metadata_files = read_table(&legacy, "MetadataFiles", extra(ExtraKind::Metadata))?;
    let other_extra_files = read_table(&legacy, "ExtraFiles", extra(ExtraKind::Other))?;

    let mut providers = read_table(&legacy, "Indexers", |r| {
        let attributes = json!({
            "enableRss": r.flag("EnableRss", false)?,
            "enableAutomaticSearch": r.flag("EnableAutomaticSearch", false)?,
            "enableInteractiveSearch": r.flag("EnableInteractiveSearch", false)?,
            "priority": r.or("Priority", 25)?,
            "downloadClientId": r.or("DownloadClientId", 0)?,
        });
        provider(r, ProviderKind::Indexer, attributes)
    })?;
    providers.extend(read_table(&legacy, "DownloadClients", |r| {
        let attributes = json!({
            "enable": r.flag("Enable", false)?,
            "priority": r.or("Priority", 1)?,
            "removeCompletedDownloads": r.flag("RemoveCompletedDownloads", true)?,
            "removeFailedDownloads": r.flag("RemoveFailedDownloads", true)?,
        });
        provider(r, ProviderKind::DownloadClient, attributes)
    })?);
    providers.extend(read_table(&legacy, "Notifications", |r| {
        let mut attributes = serde_json::Map::new();
        for (column, name) in [
            ("OnGrab", "onGrab"),
            ("OnDownload", "onDownload"),
            ("OnUpgrade", "onUpgrade"),
            ("OnRename", "onRename"),
            ("OnMovieAdded", "onMovieAdded"),
            ("OnMovieDelete", "onMovieDelete"),
            ("OnMovieFileDelete", "onMovieFileDelete"),
            ("OnMovieFileDeleteForUpgrade", "onMovieFileDeleteForUpgrade"),
            ("OnHealthIssue", "onHealthIssue"),
            ("IncludeHealthWarnings", "includeHealthWarnings"),
            ("OnHealthRestored", "onHealthRestored"),
            ("OnApplicationUpdate", "onApplicationUpdate"),
            ("OnManualInteractionRequired", "onManualInteractionRequired"),
        ] {
            attributes.insert(name.into(), json!(r.flag(column, false)?));
        }
        provider(r, ProviderKind::Notification, Value::Object(attributes))
    })?);
    providers.extend(read_table(&legacy, "ImportLists", |r| {
        let attributes = json!({
            "enabled": r.flag("Enabled", false)?,
            "enableAuto": r.flag("EnableAuto", false)?,
            "monitor": monitor_type(r.or("Monitor", 0)?),
            "rootFolderPath": r.opt::<String>("RootFolderPath")?,
            "qualityProfileId": r.or("QualityProfileId", 0)?,
            "searchOnAdd": r.flag("SearchOnAdd", false)?,
            "minimumAvailability": movie_status(r.or("MinimumAvailability", 3)?).as_str(),
        });
        provider(r, ProviderKind::ImportList, attributes)
    })?);
    providers.extend(read_table(&legacy, "Metadata", |r| {
        let attributes = json!({ "enable": r.flag("Enable", false)? });
        provider(r, ProviderKind::Metadata, attributes)
    })?);

    // The last release each indexer's RSS sync saw is the acquisition
    // milestone's to read; the failure state is the same for every kind.
    let without_release = |rows: Vec<(i64, ProviderStatus)>| -> Vec<(i64, ProviderStatus)> {
        rows.into_iter()
            .map(|(id, mut status)| {
                status.last_rss_sync_release_info = None;
                (id, status)
            })
            .collect()
    };
    let provider_exists =
        |kind: ProviderKind, id: i64| providers.iter().any(|p| p.kind == kind && p.id == id);
    let indexer_statuses: Vec<_> = without_release(read_statuses(&legacy, "IndexerStatus")?)
        .into_iter()
        .filter(|(id, _)| provider_exists(ProviderKind::Indexer, *id))
        .collect();
    let download_client_statuses: Vec<_> = without_release(read_statuses(&legacy, "DownloadClientStatus")?)
        .into_iter()
        .filter(|(id, _)| provider_exists(ProviderKind::DownloadClient, *id))
        .collect();
    let import_list_statuses: Vec<_> = read_table(&legacy, "ImportListStatus", |r| {
        Ok((
            r.req::<i64>("ProviderId")?,
            ProviderStatus {
                initial_failure: r.timestamp("InitialFailure")?,
                most_recent_failure: r.timestamp("MostRecentFailure")?,
                escalation_level: r.or("EscalationLevel", 0)?,
                disabled_till: r.timestamp("DisabledTill")?,
                last_rss_sync_release_info: None,
            },
            SyncStatus {
                last_info_sync: r.timestamp("LastInfoSync")?,
                has_removed_item_since_last_clean: false,
            },
        ))
    })?
    .into_iter()
    .filter(|(id, _, _)| provider_exists(ProviderKind::ImportList, *id))
    .collect();

    let naming = read_table(&legacy, "NamingConfig", |r| {
        let d = MovieNamingConfig::default();
        Ok(MovieNamingConfig {
            rename_movies: r.flag("RenameMovies", d.rename_movies)?,
            replace_illegal_characters: r.flag("ReplaceIllegalCharacters", d.replace_illegal_characters)?,
            colon_replacement_format: r.or("ColonReplacementFormat", d.colon_replacement_format)?,
            standard_movie_format: r.or("StandardMovieFormat", d.standard_movie_format)?,
            movie_folder_format: r.or("MovieFolderFormat", d.movie_folder_format)?,
        })
    })?;

    report.skipped = skipped_tables(&legacy, HANDLED_TABLES)?;
    for (table, rows) in &report.skipped {
        tracing::info!(table, rows, "legacy table not imported");
    }

    let count_kind = |kind: ProviderKind| providers.iter().filter(|p| p.kind == kind).count();
    let counts = [
        ("Tags", shared.tags.len()),
        ("RootFolders", shared.root_folders.len()),
        ("QualityDefinitions", shared.quality_definitions.len()),
        ("CustomFormats", shared.custom_formats.len()),
        ("QualityProfiles", quality_profiles.len()),
        ("MovieMetadata", metadata.len()),
        ("Movies", all_movies.len()),
        ("MovieFiles", movie_files.len()),
        ("AlternativeTitles", alternative_titles.len()),
        ("MovieTranslations", translations.len()),
        ("Credits", credits.len()),
        ("Collections", collections.len()),
        ("ImportExclusions", exclusions.len()),
        ("ImportListMovies", import_list_movies.len()),
        ("History", history.len()),
        ("Indexers", count_kind(ProviderKind::Indexer)),
        ("DownloadClients", count_kind(ProviderKind::DownloadClient)),
        ("Notifications", count_kind(ProviderKind::Notification)),
        ("ImportLists", count_kind(ProviderKind::ImportList)),
        ("Metadata", count_kind(ProviderKind::Metadata)),
        ("DelayProfiles", shared.delay_profiles.len()),
        ("ReleaseProfiles", shared.release_profiles.len()),
        ("RemotePathMappings", shared.remote_path_mappings.len()),
        ("CustomFilters", shared.custom_filters.len()),
        ("AutoTagging", shared.auto_tagging.len()),
        ("Users", shared.users.len()),
        ("ScheduledTasks", shared.scheduled_tasks.len()),
        ("Blocklist", blocklist.len()),
        ("SubtitleFiles", subtitle_files.len()),
        ("MetadataFiles", metadata_files.len()),
        ("ExtraFiles", other_extra_files.len()),
        ("NamingConfig", naming.len()),
        ("Config", shared.config.len()),
        ("PendingReleases", pending_releases.len()),
        ("DownloadHistory", download_history.len()),
        ("IndexerStatus", indexer_statuses.len()),
        ("DownloadClientStatus", download_client_statuses.len()),
        ("ImportListStatus", import_list_statuses.len()),
    ];
    report.counts.extend(counts);
    let migration_version = report.migration_version;
    let source = legacy_db.display().to_string();

    db.call_blocking(move |conn| {
        anyhow::ensure!(
            movies::count_movies(conn)? == 0,
            "target database already holds data; import into an empty database"
        );
        let tx = conn.transaction()?;
        shared.write(&tx)?;
        for p in &quality_profiles {
            store::catalog::insert_quality_profile(&tx, p)?;
        }
        for (id, language) in &profile_languages {
            movies::set_profile_language(&tx, *id, *language)?;
        }
        for m in &metadata {
            movies::insert_metadata(&tx, m)
                .with_context(|| format!("movie metadata {} ({})", m.id, m.title))?;
        }
        for m in &all_movies {
            movies::insert_movie(&tx, m).with_context(|| format!("movie {} ({})", m.id, m.metadata.title))?;
        }
        for f in &movie_files {
            movies::insert_movie_file(&tx, f).with_context(|| format!("movie file {}", f.id))?;
        }
        for t in &alternative_titles {
            movies::insert_alternative_title(&tx, t)?;
        }
        for t in &translations {
            movies::insert_translation(&tx, t)?;
        }
        for c in &credits {
            movies::insert_credit(&tx, c)?;
        }
        for c in &collections {
            movies::insert_collection(&tx, c).with_context(|| format!("collection {}", c.id))?;
        }
        for e in &exclusions {
            movies::insert_import_exclusion(&tx, e)?;
        }
        for (id, list, metadata_id) in &import_list_movies {
            movies::insert_import_list_movie(&tx, *id, *list, *metadata_id)?;
        }
        for h in &history {
            movies::insert_history(&tx, h).with_context(|| format!("history {}", h.id))?;
        }
        for b in &blocklist {
            movies::insert_blocklist(&tx, b)?;
        }
        for h in &download_history {
            movies::insert_download_history(&tx, h)?;
        }
        for p in &pending_releases {
            tx.execute(
                "INSERT INTO pending_releases (id, movie_id, title, added, release, parsed_info, reason,
                    additional_info) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    p.id,
                    p.movie_id,
                    p.title,
                    p.added,
                    p.release.to_string(),
                    p.parsed_info.as_ref().map(Value::to_string),
                    p.reason,
                    p.additional_info.as_ref().map(Value::to_string),
                ],
            )?;
        }
        for e in subtitle_files
            .iter()
            .chain(&metadata_files)
            .chain(&other_extra_files)
        {
            movies::insert_extra_file(&tx, e)?;
        }
        // The originals never reuse an id once its row is deleted; the
        // counters continue where theirs stopped.
        for (table, ours) in [
            ("Movies", "movies"),
            ("MovieMetadata", "movie_metadata"),
            ("MovieFiles", "movie_files"),
            ("AlternativeTitles", "alternative_titles"),
            ("MovieTranslations", "movie_translations"),
            ("Credits", "credits"),
            ("Collections", "collections"),
            ("ImportExclusions", "import_exclusions"),
            ("ImportListMovies", "import_list_movies"),
            ("History", "history"),
            ("Blocklist", "blocklist"),
            ("DownloadHistory", "download_history"),
            ("PendingReleases", "pending_releases"),
            ("Commands", "commands"),
            ("Tags", "tags"),
            ("RootFolders", "root_folders"),
            ("QualityProfiles", "quality_profiles"),
            ("CustomFormats", "custom_formats"),
            ("DelayProfiles", "delay_profiles"),
            ("ReleaseProfiles", "release_profiles"),
            ("RemotePathMappings", "remote_path_mappings"),
            ("CustomFilters", "custom_filters"),
            ("AutoTagging", "auto_tagging"),
        ] {
            if let Ok(seq) =
                legacy.query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| {
                    r.get::<_, i64>(0)
                })
            {
                store::sequence::seed(&tx, ours, seq)?;
            }
        }
        // The extra files keep the ids of the three tables they come from,
        // and each kind counts on from its own table's sequence.
        for (table, kind) in [
            ("SubtitleFiles", ExtraKind::Subtitle),
            ("MetadataFiles", ExtraKind::Metadata),
            ("ExtraFiles", ExtraKind::Other),
        ] {
            if let Ok(seq) =
                legacy.query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| {
                    r.get::<_, i64>(0)
                })
            {
                movies::seed_extra_file_sequence(&tx, kind, seq)?;
            }
        }
        for p in &providers {
            store::providers::insert_provider(&tx, p)?;
        }
        for (id, status) in &indexer_statuses {
            store::provider_status::upsert_provider_status(&tx, ProviderKind::Indexer, *id, status)?;
        }
        for (id, status) in &download_client_statuses {
            store::provider_status::upsert_provider_status(&tx, ProviderKind::DownloadClient, *id, status)?;
        }
        for (id, status, sync) in &import_list_statuses {
            store::provider_status::upsert_provider_status(&tx, ProviderKind::ImportList, *id, status)?;
            store::import_lists::set_sync_status(&tx, *id, sync)?;
        }
        if let Some(n) = naming.first() {
            movies::set_naming(&tx, n)?;
        }
        for (kind, last_id) in &sequences {
            store::providers::set_provider_sequence(&tx, *kind, *last_id)?;
        }
        store::config::set_meta(&tx, "source.app", "Radarr")?;
        store::config::set_meta(&tx, "source.path", &source)?;
        store::config::set_meta(&tx, "source.migration", &migration_version.to_string())?;
        store::config::set_meta(&tx, "imported_at", &time::format_utc(Utc::now()))?;
        tx.commit()?;
        Ok(())
    })?;

    for (table, n) in &report.counts {
        tracing::info!(table, rows = n, "imported");
    }
    for note in &report.notes {
        tracing::warn!("{note}");
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unarr_core::Personality;

    fn fixture() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/radarr/radarr.db")
    }

    #[test]
    fn imports_the_fixture_table_by_table() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let report = import(&fixture(), &db).unwrap();
        assert_eq!(report.migration_version, 242);
        for (table, rows) in [
            ("Movies", 193),
            ("MovieMetadata", 284),
            ("MovieFiles", 100),
            ("AlternativeTitles", 1550),
            ("MovieTranslations", 6037),
            ("Credits", 9935),
            ("Collections", 32),
            ("ImportExclusions", 21),
            ("ImportListMovies", 88),
            ("History", 669),
            ("Blocklist", 86),
            ("DownloadHistory", 644),
            ("QualityProfiles", 12),
            ("QualityDefinitions", 30),
            ("CustomFormats", 62),
            ("Indexers", 12),
            ("DownloadClients", 3),
            ("Notifications", 3),
            ("ImportLists", 1),
            ("Metadata", 5),
            ("SubtitleFiles", 75),
            ("MetadataFiles", 156),
            ("ExtraFiles", 22),
            ("Tags", 2),
            ("Users", 1),
            ("ScheduledTasks", 11),
            ("NamingConfig", 1),
        ] {
            assert_eq!(report.counts.get(table).copied(), Some(rows), "{table}");
        }
        // The anonymiser emptied the status tables and Commands; nothing
        // else of the original goes unread.
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert!(report.notes.is_empty(), "{:?}", report.notes);

        let (movies, files, alien) = db
            .call_blocking(|conn| {
                Ok((
                    movies::list_movies(conn)?,
                    movies::list_movie_files(conn)?,
                    movies::get_movie(conn, 6)?,
                ))
            })
            .unwrap();
        assert_eq!(movies.len(), 193);
        assert_eq!(files.len(), 100);
        let alien = alien.unwrap();
        assert_eq!(alien.metadata.title, "Alien: Romulus");
        assert_eq!(alien.metadata.tmdb_id, 945961);
        assert_eq!(alien.metadata.status, MovieStatus::Released);
        assert_eq!(alien.movie_file_id, 213);
        assert_eq!(alien.path, "/movies/Alien Romulus (2024)");
        // Ids continue where the original's sequences stopped.
        let next = db
            .call_blocking(|conn| store::sequence::next_id(conn, "movies"))
            .unwrap();
        assert_eq!(next, 328);
        // The extra files count on within their kind.
        let next = db
            .call_blocking(|conn| {
                Ok([
                    movies::next_extra_file_id(conn, ExtraKind::Subtitle)?,
                    movies::next_extra_file_id(conn, ExtraKind::Metadata)?,
                    movies::next_extra_file_id(conn, ExtraKind::Other)?,
                ])
            })
            .unwrap();
        assert_eq!(next, [212, 5076, 27]);
    }

    #[test]
    fn refuses_a_second_import_and_a_sonarr_database() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        import(&fixture(), &db).unwrap();
        assert!(import(&fixture(), &db)
            .unwrap_err()
            .to_string()
            .contains("already holds data"));
        let sonarr =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sonarr/sonarr.db");
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        assert!(import(&sonarr, &db)
            .unwrap_err()
            .to_string()
            .contains("not a Radarr database"));
    }
}
