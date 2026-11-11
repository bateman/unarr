//! The tables every original application has, read the same way by each
//! importer: tags, root folders, quality definitions, custom formats, the
//! profiles that carry no media notion, users, scheduled tasks and the
//! key/value settings.

use rusqlite::{Connection, Transaction};
use serde_json::json;
use unarr_core::model::*;
use unarr_core::store;

use crate::legacy::{protocol, read_table, LegacyRow};

pub(crate) struct SharedTables {
    pub tags: Vec<Tag>,
    pub root_folders: Vec<RootFolder>,
    pub quality_definitions: Vec<QualityDefinition>,
    pub custom_formats: Vec<CustomFormat>,
    pub delay_profiles: Vec<DelayProfile>,
    pub release_profiles: Vec<ReleaseProfile>,
    pub remote_path_mappings: Vec<RemotePathMapping>,
    pub custom_filters: Vec<CustomFilter>,
    pub auto_tagging: Vec<AutoTagging>,
    pub users: Vec<User>,
    pub scheduled_tasks: Vec<ScheduledTask>,
    pub config: Vec<(String, String)>,
}

impl SharedTables {
    pub(crate) fn read(legacy: &Connection) -> anyhow::Result<SharedTables> {
        // Older schemas kept Required/Ignored as newline-separated text.
        let term_list = |r: &LegacyRow, column: &str| -> anyhow::Result<Vec<String>> {
            match r.opt::<String>(column)? {
                None => Ok(Vec::new()),
                Some(s) if s.trim_start().starts_with('[') => Ok(serde_json::from_str(&s)?),
                Some(s) => Ok(s
                    .split(['\n', ','])
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()),
            }
        };
        Ok(SharedTables {
            tags: read_table(legacy, "Tags", |r| {
                Ok(Tag {
                    id: r.req("Id")?,
                    label: r.req("Label")?,
                })
            })?,
            root_folders: read_table(legacy, "RootFolders", |r| {
                Ok(RootFolder {
                    id: r.req("Id")?,
                    path: r.req("Path")?,
                })
            })?,
            quality_definitions: read_table(legacy, "QualityDefinitions", |r| {
                Ok(QualityDefinition {
                    id: r.req("Id")?,
                    quality_id: r.req("Quality")?,
                    title: r.req("Title")?,
                    min_size: r.opt("MinSize")?,
                    max_size: r.opt("MaxSize")?,
                    preferred_size: r.opt("PreferredSize")?,
                })
            })?,
            custom_formats: read_table(legacy, "CustomFormats", |r| {
                Ok(CustomFormat {
                    id: r.req("Id")?,
                    name: r.req("Name")?,
                    include_when_renaming: r.flag("IncludeCustomFormatWhenRenaming", false)?,
                    specifications: r.json_opt("Specifications")?.unwrap_or(json!([])),
                })
            })?,
            delay_profiles: read_table(legacy, "DelayProfiles", |r| {
                Ok(DelayProfile {
                    id: r.req("Id")?,
                    enable_usenet: r.flag("EnableUsenet", true)?,
                    enable_torrent: r.flag("EnableTorrent", true)?,
                    preferred_protocol: protocol(r.or("PreferredProtocol", 1)?),
                    usenet_delay: r.or("UsenetDelay", 0)?,
                    torrent_delay: r.or("TorrentDelay", 0)?,
                    order: r.or("Order", i32::MAX as i64)?,
                    tags: r.json("Tags")?,
                    bypass_if_highest_quality: r.flag("BypassIfHighestQuality", false)?,
                    bypass_if_above_custom_format_score: r.flag("BypassIfAboveCustomFormatScore", false)?,
                    minimum_custom_format_score: r.or("MinimumCustomFormatScore", 0)?,
                })
            })?,
            release_profiles: read_table(legacy, "ReleaseProfiles", |r| {
                Ok(ReleaseProfile {
                    id: r.req("Id")?,
                    name: r.text("Name")?,
                    enabled: r.flag("Enabled", true)?,
                    required: term_list(r, "Required")?,
                    ignored: term_list(r, "Ignored")?,
                    indexer_id: r.or("IndexerId", 0)?,
                    tags: r.json("Tags")?,
                })
            })?,
            remote_path_mappings: read_table(legacy, "RemotePathMappings", |r| {
                Ok(RemotePathMapping {
                    id: r.req("Id")?,
                    host: r.req("Host")?,
                    remote_path: r.req("RemotePath")?,
                    local_path: r.req("LocalPath")?,
                })
            })?,
            custom_filters: read_table(legacy, "CustomFilters", |r| {
                Ok(CustomFilter {
                    id: r.req("Id")?,
                    filter_type: r.req("Type")?,
                    label: r.req("Label")?,
                    filters: r.json_opt("Filters")?.unwrap_or(json!([])),
                })
            })?,
            auto_tagging: read_table(legacy, "AutoTagging", |r| {
                Ok(AutoTagging {
                    id: r.req("Id")?,
                    name: r.req("Name")?,
                    remove_tags_automatically: r.flag("RemoveTagsAutomatically", false)?,
                    tags: r.json("Tags")?,
                    specifications: r.json_opt("Specifications")?.unwrap_or(json!([])),
                })
            })?,
            users: read_table(legacy, "Users", |r| {
                Ok(User {
                    id: r.req("Id")?,
                    identifier: r.req("Identifier")?,
                    username: r.req("Username")?,
                    password: r.req("Password")?,
                    salt: r.text("Salt")?,
                    iterations: r.opt("Iterations")?,
                })
            })?,
            scheduled_tasks: read_table(legacy, "ScheduledTasks", |r| {
                Ok(ScheduledTask {
                    id: r.req("Id")?,
                    type_name: r.req("TypeName")?,
                    interval_minutes: r.or("Interval", 0)?,
                    last_execution: r.timestamp("LastExecution")?,
                    last_start_time: r.timestamp("LastStartTime")?,
                })
            })?,
            config: read_table(legacy, "Config", |r| {
                Ok((r.req::<String>("Key")?, r.or("Value", String::new())?))
            })?,
        })
    }

    /// Write the rows into the target, inside the import's transaction.
    pub(crate) fn write(&self, tx: &Transaction) -> anyhow::Result<()> {
        for t in &self.tags {
            store::catalog::insert_tag(tx, t)?;
        }
        for r in &self.root_folders {
            store::catalog::insert_root_folder(tx, r)?;
        }
        for d in &self.quality_definitions {
            store::catalog::insert_quality_definition(tx, d)?;
        }
        for c in &self.custom_formats {
            store::catalog::insert_custom_format(tx, c)?;
        }
        for d in &self.delay_profiles {
            store::providers::insert_delay_profile(tx, d)?;
        }
        for r in &self.release_profiles {
            store::providers::insert_release_profile(tx, r)?;
        }
        for m in &self.remote_path_mappings {
            store::providers::insert_remote_path_mapping(tx, m)?;
        }
        for f in &self.custom_filters {
            store::providers::insert_custom_filter(tx, f)?;
        }
        for a in &self.auto_tagging {
            store::providers::insert_auto_tagging(tx, a)?;
        }
        for u in &self.users {
            store::providers::insert_user(tx, u)?;
        }
        // A blank first start seeds the default tasks with ids of its own;
        // the original's rows, last runs included, take their place.
        tx.execute("DELETE FROM scheduled_tasks", [])?;
        for t in &self.scheduled_tasks {
            store::providers::insert_scheduled_task(tx, t)?;
        }
        for (k, v) in &self.config {
            store::config::set_config(tx, k, v)?;
        }
        Ok(())
    }
}

/// AUTOINCREMENT sequences of the provider tables, so ids created later
/// continue where the original left off rather than reusing deleted ones.
pub(crate) fn provider_sequences(legacy: &Connection) -> Vec<(ProviderKind, i64)> {
    [
        ("Indexers", ProviderKind::Indexer),
        ("DownloadClients", ProviderKind::DownloadClient),
        ("Notifications", ProviderKind::Notification),
        ("ImportLists", ProviderKind::ImportList),
        ("Metadata", ProviderKind::Metadata),
    ]
    .into_iter()
    .filter_map(|(table, kind)| {
        legacy
            .query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| {
                r.get::<_, i64>(0)
            })
            .ok()
            .map(|seq| (kind, seq))
    })
    .collect()
}
