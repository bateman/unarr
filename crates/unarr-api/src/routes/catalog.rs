//! Tags, root folders, languages, quality definitions and profiles, custom
//! formats, disk space.

use std::path::Path as FsPath;

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};
use unarr_core::model::{CustomFormat, ProfileItem, ProviderKind, QualityProfile, RootFolder};
use unarr_core::quality::{Quality, LANGUAGES, LAST_RELEASED_LANGUAGE_ID};
use unarr_core::store;
use unarr_core::Personality;

use crate::error::{found, ApiResult};
use crate::fs;
use crate::resources::best_root_folder_path;
use crate::schemas::{self, SchemaKind};
use crate::state::AppState;

// --- tags

pub async fn tags(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let mut tags = state.db().call(|c| store::catalog::list_tags(c)).await?;
    tags.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(Json(
        tags.iter()
            .map(|t| json!({"id": t.id, "label": t.label}))
            .collect(),
    ))
}

pub async fn tag(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let tags = state.db().call(|c| store::catalog::list_tags(c)).await?;
    let t = found(tags.into_iter().find(|t| t.id == id))?;
    Ok(Json(json!({"id": t.id, "label": t.label})))
}

pub async fn tag_details(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(tag_details_all(&state).await?))
}

pub async fn tag_detail(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = tag_details_all(&state).await?;
    Ok(Json(found(all.into_iter().find(|t| t["id"] == id))?))
}

async fn tag_details_all(state: &AppState) -> anyhow::Result<Vec<Value>> {
    state
        .db()
        .call(|conn| {
            let mut tags = store::catalog::list_tags(conn)?;
            tags.sort_by(|a, b| a.label.cmp(&b.label));
            let series = store::series::list(conn)?;
            let delay = store::providers::list_delay_profiles(conn)?;
            let release = store::providers::list_release_profiles(conn)?;
            let auto = store::providers::list_auto_tagging(conn)?;
            let providers = |kind| store::providers::list_providers(conn, kind);
            let indexers = providers(ProviderKind::Indexer)?;
            let clients = providers(ProviderKind::DownloadClient)?;
            let notifications = providers(ProviderKind::Notification)?;
            let lists = providers(ProviderKind::ImportList)?;
            let with = |ids: Vec<(i64, &Vec<i64>)>, tag: i64| -> Vec<i64> {
                ids.into_iter()
                    .filter(|(_, tags)| tags.contains(&tag))
                    .map(|(id, _)| id)
                    .collect()
            };
            Ok(tags
                .iter()
                .map(|t| {
                    json!({
                        "id": t.id,
                        "label": t.label,
                        "delayProfileIds": with(delay.iter().map(|d| (d.id, &d.tags)).collect(), t.id),
                        "importListIds": with(lists.iter().map(|p| (p.id, &p.tags)).collect(), t.id),
                        "notificationIds": with(notifications.iter().map(|p| (p.id, &p.tags)).collect(), t.id),
                        "restrictionIds": with(release.iter().map(|r| (r.id, &r.tags)).collect(), t.id),
                        "indexerIds": with(indexers.iter().map(|p| (p.id, &p.tags)).collect(), t.id),
                        "downloadClientIds": with(clients.iter().map(|p| (p.id, &p.tags)).collect(), t.id),
                        "autoTagIds": with(auto.iter().map(|a| (a.id, &a.tags)).collect(), t.id),
                        "seriesIds": with(series.iter().map(|s| (s.id, &s.tags)).collect(), t.id),
                    })
                })
                .collect())
        })
        .await
}

// --- root folders

/// `RootFolderService.SpecialFolders`: folders a file system or a NAS
/// keeps for itself, never offered as unmapped.
const SPECIAL_FOLDERS: [&str; 9] = [
    "$recycle.bin",
    "system volume information",
    "recycler",
    "lost+found",
    ".appledb",
    ".appledesktop",
    ".appledouble",
    "@eadir",
    ".grab",
];

/// Radarr's resource has no `totalSpace`. The unmapped folders are
/// `GetUnmappedFolders`'s: the library's own folders, hidden ones (a name
/// starting with a dot, as .NET reads it on Linux), the recycling bin and
/// the special folders left out, sorted by name in any case.
fn root_folder_resource(
    r: &RootFolder,
    series_paths: &[String],
    total_space: bool,
    recycle_bin: &str,
) -> Value {
    let path = FsPath::new(&r.path);
    let mut out =
        json!({"id": r.id, "path": r.path.trim_end_matches('/'), "accessible": false, "unmappedFolders": []});
    if path.is_dir() {
        out["accessible"] = json!(true);
        if let Some((free, total)) = fs::disk_space(path) {
            out["freeSpace"] = json!(free);
            if total_space {
                out["totalSpace"] = json!(total);
            }
        }
        let mut unmapped = Vec::new();
        if let Ok(entries) = std::fs::read_dir(path) {
            let mut dirs: Vec<_> = entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .collect();
            dirs.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
            for e in dirs {
                let full = e.path().display().to_string();
                let name = e.file_name().to_string_lossy().to_lowercase();
                let excluded = name.starts_with('.')
                    || SPECIAL_FOLDERS.contains(&name.as_str())
                    || (!recycle_bin.trim().is_empty()
                        && unarr_core::mediafiles::path_equals(&full, recycle_bin));
                if !excluded
                    && !series_paths
                        .iter()
                        .any(|p| unarr_core::mediafiles::path_equals(p, &full))
                {
                    unmapped.push(json!({
                        "name": e.file_name().to_string_lossy(),
                        "path": full,
                        "relativePath": e.file_name().to_string_lossy(),
                    }));
                }
            }
        }
        out["unmappedFolders"] = Value::Array(unmapped);
    }
    out
}

/// The folders of the library: one per series, or one per movie.
fn library_paths(conn: &rusqlite::Connection, personality: Personality) -> anyhow::Result<Vec<String>> {
    Ok(match personality {
        Personality::Radarr => unarr_core::movies::store::list_movies(conn)?
            .into_iter()
            .map(|m| m.path)
            .collect(),
        Personality::Sonarr | Personality::Lidarr => {
            store::series::list(conn)?.into_iter().map(|s| s.path).collect()
        }
    })
}

async fn root_folders_all(state: &AppState) -> anyhow::Result<Vec<Value>> {
    let personality = state.personality();
    let (roots, paths, bin) = state
        .db()
        .call(move |conn| {
            let roots = store::catalog::list_root_folders(conn)?;
            let paths = library_paths(conn, personality)?;
            let bin = store::config::config(conn, "recyclebin")?.unwrap_or_default();
            Ok((roots, paths, bin))
        })
        .await?;
    // Free space and directory listings touch the disks: a hung network
    // mount must not park an async worker.
    let total_space = personality != Personality::Radarr;
    crate::blocking::run(move || {
        roots
            .iter()
            .map(|r| root_folder_resource(r, &paths, total_space, &bin))
            .collect()
    })
    .await
}

pub async fn root_folders(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(root_folders_all(&state).await?))
}

pub async fn root_folder(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = root_folders_all(&state).await?;
    Ok(Json(found(all.into_iter().find(|r| r["id"] == id))?))
}

// --- languages

pub async fn languages() -> Json<Vec<Value>> {
    let mut all: Vec<_> = LANGUAGES
        .iter()
        .copied()
        .filter(|l| l.id <= LAST_RELEASED_LANGUAGE_ID)
        .collect();
    // The originals: pseudo-languages (id <= 0) first, then by name.
    all.sort_by(|a, b| {
        (a.id > 0)
            .cmp(&(b.id > 0))
            .then_with(|| unarr_core::collation::compare(a.name, b.name))
    });
    Json(
        all.iter()
            .map(|l| json!({"id": l.id, "name": l.name, "nameLower": l.name.to_lowercase()}))
            .collect(),
    )
}

pub async fn language(Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let l = found(LANGUAGES.iter().find(|l| l.id == id))?;
    Ok(Json(
        json!({"id": l.id, "name": l.name, "nameLower": l.name.to_lowercase()}),
    ))
}

// --- quality definitions and profiles

fn quality_definition_resource(d: &unarr_core::model::QualityDefinition) -> Value {
    let mut out = json!({
        "id": d.id,
        "quality": Quality::by_id(d.quality_id),
        "title": d.title,
        "weight": Quality::weight(d.quality_id),
    });
    for (key, v) in [
        ("minSize", d.min_size),
        ("maxSize", d.max_size),
        ("preferredSize", d.preferred_size),
    ] {
        if let Some(v) = v {
            out[key] = json!(v);
        }
    }
    out
}

pub async fn quality_definitions(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let defs = state
        .db()
        .call(|c| store::catalog::list_quality_definitions(c))
        .await?;
    Ok(Json(defs.iter().map(quality_definition_resource).collect()))
}

pub async fn quality_definition(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let defs = state
        .db()
        .call(|c| store::catalog::list_quality_definitions(c))
        .await?;
    Ok(Json(found(
        defs.iter().find(|d| d.id == id).map(quality_definition_resource),
    )?))
}

fn profile_item_resource(item: &ProfileItem) -> Value {
    let mut out = json!({
        "items": item.items.iter().map(profile_item_resource).collect::<Vec<_>>(),
        "allowed": item.allowed,
    });
    if let Some(q) = item.quality {
        out["quality"] = json!(Quality::by_id(q));
    }
    if let Some(id) = item.id {
        out["id"] = json!(id);
    }
    if let Some(name) = &item.name {
        out["name"] = json!(name);
    }
    out
}

fn quality_profile_resource(p: &QualityProfile, formats: &[CustomFormat]) -> Value {
    json!({
        "id": p.id,
        "name": p.name,
        "upgradeAllowed": p.upgrade_allowed,
        "cutoff": p.cutoff,
        "items": p.items.iter().map(profile_item_resource).collect::<Vec<_>>(),
        "minFormatScore": p.min_format_score,
        "cutoffFormatScore": p.cutoff_format_score,
        "minUpgradeFormatScore": p.min_upgrade_format_score,
        "formatItems": p.format_items.iter().map(|fi| json!({
            "format": fi.format,
            "name": formats.iter().find(|f| f.id == fi.format).map(|f| f.name.clone()),
            "score": fi.score,
        })).collect::<Vec<_>>(),
    })
}

async fn profiles_and_formats(state: &AppState) -> anyhow::Result<(Vec<QualityProfile>, Vec<CustomFormat>)> {
    state
        .db()
        .call(|conn| {
            Ok((
                store::catalog::list_quality_profiles(conn)?,
                store::catalog::list_custom_formats(conn)?,
            ))
        })
        .await
}

pub async fn quality_profiles(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let (profiles, formats) = profiles_and_formats(&state).await?;
    Ok(Json(
        profiles
            .iter()
            .map(|p| quality_profile_resource(p, &formats))
            .collect(),
    ))
}

pub async fn quality_profile(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (profiles, formats) = profiles_and_formats(&state).await?;
    let p = found(profiles.iter().find(|p| p.id == id))?;
    Ok(Json(quality_profile_resource(p, &formats)))
}

// --- custom formats

/// Stored specifications are `[{type, body}]`; the API shows each with its
/// schema's field metadata and the body's values filled in.
pub fn custom_format_resource(personality: Personality, cf: &CustomFormat) -> Value {
    let specs: Vec<Value> = cf
        .specifications
        .as_array()
        .map(|list| {
            list.iter()
                .map(|spec| {
                    let implementation = spec["type"].as_str().unwrap_or("");
                    let body = &spec["body"];
                    let mut out = json!({
                        "name": body["name"],
                        "implementation": implementation,
                        "implementationName": body["implementationName"],
                        "infoLink": body["infoLink"],
                        "negate": body["negate"].as_bool().unwrap_or(false),
                        "required": body["required"].as_bool().unwrap_or(false),
                        "fields": [],
                    });
                    if let Some(schema) =
                        schemas::lookup(personality, SchemaKind::CustomFormatSpecification, implementation)
                    {
                        out["implementationName"] = schema["implementationName"].clone();
                        out["infoLink"] = schema["infoLink"].clone();
                        out["fields"] = Value::Array(schemas::render_fields(schema, body));
                    }
                    out
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": cf.id,
        "name": cf.name,
        "includeCustomFormatWhenRenaming": cf.include_when_renaming,
        "specifications": specs,
    })
}

pub async fn custom_formats(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let formats = state
        .db()
        .call(|c| store::catalog::list_custom_formats(c))
        .await?;
    Ok(Json(
        formats
            .iter()
            .map(|f| custom_format_resource(state.personality(), f))
            .collect(),
    ))
}

pub async fn custom_format(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let formats = state
        .db()
        .call(|c| store::catalog::list_custom_formats(c))
        .await?;
    Ok(Json(found(
        formats
            .iter()
            .find(|f| f.id == id)
            .map(|f| custom_format_resource(state.personality(), f)),
    )?))
}

// --- disk space

pub async fn disk_space(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let personality = state.personality();
    let (roots, paths) = state
        .db()
        .call(move |conn| {
            let roots: Vec<String> = store::catalog::list_root_folders(conn)?
                .into_iter()
                .map(|r| r.path)
                .collect();
            let paths = library_paths(conn, personality)?;
            Ok((roots, paths))
        })
        .await?;
    let disks = crate::blocking::run(move || {
        // The mounts holding series, then every other disk, by path.
        let mounts = fs::disk_mounts();
        let mut candidates: Vec<String> = paths
            .iter()
            .map(|p| best_root_folder_path(p, &roots))
            .filter(|r| FsPath::new(r).is_dir())
            .map(|r| mount_point_of(&r, &mounts))
            .collect();
        for m in mounts.iter().cloned() {
            if !candidates.contains(&m) {
                candidates.push(m);
            }
        }
        candidates.sort_by_key(|a| a.to_lowercase());
        candidates.dedup();
        candidates
            .iter()
            .filter_map(|path| {
                fs::disk_space(FsPath::new(path)).map(|(free, total)| {
                    json!({"path": path, "label": "", "freeSpace": free, "totalSpace": total})
                })
            })
            .collect::<Vec<Value>>()
    })
    .await?;
    Ok(Json(disks))
}

/// The mount point a path lives on, from the mount table.
fn mount_point_of(path: &str, mounts: &[String]) -> String {
    mounts
        .iter()
        .filter(|m| *m == "/" || path == *m || path.starts_with(&format!("{m}/")))
        .max_by_key(|m| m.len())
        .cloned()
        .unwrap_or_else(|| "/".to_string())
}

// --- indexer flags

/// The `IndexerFlags` enumeration of the mirrored release.
pub async fn indexer_flags() -> Json<Vec<Value>> {
    Json(
        [
            (1, "Freeleech"),
            (2, "Halfleech"),
            (4, "DoubleUpload"),
            (8, "Internal"),
            (16, "Scene"),
            (32, "Freeleech75"),
            (64, "Freeleech25"),
            (128, "Nuked"),
        ]
        .iter()
        .map(|(id, name)| json!({"id": id, "name": name, "nameLower": name.to_lowercase()}))
        .collect(),
    )
}
