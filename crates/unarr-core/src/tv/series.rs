//! `SeriesService`, `AddSeriesService` and the series side of the API
//! controllers that write: lookup, add, edit (one series, the editor's
//! bulk edit, the season pass), delete.
//!
//! Writes go by columns: a series read from a list carries no cast
//! (`docs/DESIGN.md` section 14), and [`store::series::update_settings`]
//! never touches it.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::Db;
use crate::metadata::{MetadataError, MetadataProvider};
use crate::model::{NewItemMonitor, Season, Series, SeriesType};
use crate::store;
use crate::validation::ValidationFailure;

use super::monitoring::{self, MonitorType, MonitoringOptions};

/// `SearchForNewSeries` with the library first: a `tvdb:` term is answered
/// from the library when the series is there, and every result the library
/// already holds is the library's series.
pub async fn lookup(
    db: &Db,
    provider: &dyn MetadataProvider,
    term: &str,
) -> Result<Vec<Series>, MetadataError> {
    let lower = term.to_lowercase();
    if let Some(slug) = lower
        .strip_prefix("tvdb:")
        .or_else(|| lower.strip_prefix("tvdbid:"))
    {
        let slug = slug.trim();
        let Some(tvdb_id) = slug
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0 && !slug.contains(char::is_whitespace))
        else {
            return Ok(Vec::new());
        };
        if let Some(existing) = db
            .call(move |conn| store::series::find_by_tvdb_id(conn, tvdb_id))
            .await
            .map_err(|e| MetadataError::Invalid(e.to_string()))?
        {
            return Ok(vec![existing]);
        }
        return match provider.series_info(tvdb_id).await {
            Ok(info) => Ok(vec![info.series]),
            Err(MetadataError::SeriesNotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e),
        };
    }
    let found = provider.search(term).await?;
    let ids: Vec<i64> = found.iter().map(|s| s.tvdb_id).collect();
    let library = db
        .call(move |conn| {
            let mut out = std::collections::HashMap::new();
            for id in ids {
                if let Some(s) = store::series::find_by_tvdb_id(conn, id)? {
                    out.insert(id, s);
                }
            }
            Ok(out)
        })
        .await
        .map_err(|e| MetadataError::Invalid(e.to_string()))?;
    Ok(found
        .into_iter()
        .map(|s| library.get(&s.tvdb_id).cloned().unwrap_or(s))
        .collect())
}

/// What a client sends to add a series: the fields `ApplyChanges` takes
/// from the resource, the rest comes from the provider.
#[derive(Debug, Clone)]
pub struct NewSeries {
    pub tvdb_id: i64,
    pub title: Option<String>,
    pub path: Option<String>,
    pub root_folder_path: Option<String>,
    pub quality_profile_id: i64,
    pub season_folder: bool,
    pub monitored: bool,
    pub monitor_new_items: NewItemMonitor,
    pub series_type: SeriesType,
    pub tags: Vec<i64>,
    pub seasons: Vec<Season>,
    pub add_options: Option<Value>,
}

/// Why an add or an edit was refused.
#[derive(Debug)]
pub enum WriteError {
    /// FluentValidation's failures, as the 400 lists them.
    Invalid(Vec<ValidationFailure>),
    /// A `ValidationException` raised by the service with plain failures
    /// (the series the provider does not know).
    InvalidPlain(Vec<ValidationFailure>),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for WriteError {
    fn from(e: anyhow::Error) -> Self {
        WriteError::Other(e)
    }
}

fn blank(s: &Option<String>) -> bool {
    s.as_deref().is_none_or(|v| v.trim().is_empty())
}

/// `PathValidator`: an absolute path of the current OS.
fn valid_path(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\0')
}

fn path_failure(property: &str, path: &str) -> ValidationFailure {
    let mut f = ValidationFailure::rule(
        property,
        "PathValidator",
        &format!("Invalid Path: '{path}'"),
        json!(path),
    );
    f.placeholders = vec![("path".into(), json!(path))];
    f
}

/// The rules of `SeriesController` shared by POST and PUT, on the path and
/// the quality profile.
fn shared_rules(
    conn: &Connection,
    series_id: i64,
    path: Option<&str>,
    quality_profile_id: i64,
    failures: &mut Vec<ValidationFailure>,
) -> anyhow::Result<()> {
    if let Some(path) = path.filter(|p| !p.trim().is_empty()) {
        if let Some(f) = path_rules(conn, series_id, path)? {
            failures.push(f);
        }
    }
    // `ValidId`, then `QualityProfileExistsValidator` (0 passes).
    if quality_profile_id < 0 {
        failures.push(ValidationFailure::greater_than(
            "QualityProfileId",
            json!(quality_profile_id),
            0,
        ));
    } else if quality_profile_id != 0
        && !store::catalog::list_quality_profiles(conn)?
            .iter()
            .any(|p| p.id == quality_profile_id)
    {
        failures.push(ValidationFailure::rule(
            "QualityProfileId",
            "QualityProfileExistsValidator",
            "Quality Profile does not exist",
            json!(quality_profile_id),
        ));
    }
    Ok(())
}

/// `IsValidPath`, `RootFolderValidator`, `SeriesPathValidator`,
/// `SeriesAncestorValidator`: the first that fails (the cascade stops).
fn path_rules(conn: &Connection, series_id: i64, path: &str) -> anyhow::Result<Option<ValidationFailure>> {
    if !valid_path(path) {
        return Ok(Some(path_failure("Path", path)));
    }
    let with_path = |code: &str, message: String| {
        let mut f = ValidationFailure::rule("Path", code, &message, json!(path));
        f.placeholders = vec![("path".into(), json!(path))];
        f
    };
    let roots = store::catalog::list_root_folders(conn)?;
    if roots
        .iter()
        .any(|r| crate::mediafiles::path_equals(&r.path, path))
    {
        return Ok(Some(with_path(
            "RootFolderValidator",
            format!("Path '{path}' is already configured as a root folder"),
        )));
    }
    let others = store::series::all_paths(conn)?;
    if others
        .iter()
        .any(|(id, p)| *id != series_id && crate::mediafiles::path_equals(p, path))
    {
        return Ok(Some(with_path(
            "SeriesPathValidator",
            format!("Path '{path}' is already configured for another series"),
        )));
    }
    if others
        .iter()
        .any(|(_, p)| crate::mediafiles::is_parent_path(path, p))
    {
        return Ok(Some(with_path(
            "SeriesAncestorValidator",
            format!("Path '{path}' is an ancestor of an existing series"),
        )));
    }
    Ok(None)
}

/// `SeriesController`'s POST rules, in the order FluentValidation runs them.
pub fn validate_post(conn: &Connection, s: &NewSeries) -> anyhow::Result<Vec<ValidationFailure>> {
    let mut failures = Vec::new();
    shared_rules(conn, 0, s.path.as_deref(), s.quality_profile_id, &mut failures)?;
    if blank(&s.root_folder_path) {
        if blank(&s.path) {
            failures.push(ValidationFailure::not_empty(
                "Path",
                s.path.clone().map_or(Value::Null, Value::from),
            ));
        } else if !valid_path(s.path.as_deref().unwrap_or("")) {
            failures.push(path_failure("Path", s.path.as_deref().unwrap_or("")));
        }
    }
    if blank(&s.path) {
        let root = s.root_folder_path.clone().unwrap_or_default();
        if blank(&s.root_folder_path) {
            failures.push(ValidationFailure::not_empty(
                "RootFolderPath",
                s.root_folder_path.clone().map_or(Value::Null, Value::from),
            ));
        } else if !valid_path(&root) {
            failures.push(path_failure("RootFolderPath", &root));
        } else if !store::catalog::list_root_folders(conn)?
            .iter()
            .any(|r| crate::mediafiles::path_equals(&r.path, &root))
        {
            let mut f = ValidationFailure::rule(
                "RootFolderPath",
                "RootFolderExistsValidator",
                &format!("Root folder '{root}' does not exist"),
                json!(root),
            );
            f.placeholders = vec![("path".into(), json!(root))];
            failures.push(f);
        }
    }
    if blank(&s.title) {
        failures.push(ValidationFailure::not_empty(
            "Title",
            s.title.clone().map_or(Value::Null, Value::from),
        ));
    }
    if s.tvdb_id <= 0 {
        failures.push(ValidationFailure::greater_than("TvdbId", json!(s.tvdb_id), 0));
    } else if store::series::find_by_tvdb_id(conn, s.tvdb_id)?.is_some() {
        failures.push(ValidationFailure::rule(
            "TvdbId",
            "SeriesExistsValidator",
            "This series has already been added",
            json!(s.tvdb_id),
        ));
    }
    Ok(failures)
}

/// `AddSeriesService.AddSeries` after the controller's validation: the
/// provider's series with the client's choices applied, the folder named,
/// validated again, inserted. The caller publishes `SeriesAddedEvent`.
pub async fn add(
    db: &Db,
    provider: &dyn MetadataProvider,
    new: NewSeries,
    now: DateTime<Utc>,
) -> Result<Series, WriteError> {
    let (mut series, root) = with_provider_data(provider, new, now).await?;
    db.call(move |conn| {
        let failures = place_and_validate(conn, &mut series, root.as_deref())?;
        if !failures.is_empty() {
            return Ok(Err(failures));
        }
        // The series, its seasons and its tags, or none of them.
        let tx = conn.unchecked_transaction()?;
        let series = insert_new(&tx, series)?;
        tx.commit()?;
        Ok(Ok(series))
    })
    .await?
    .map_err(WriteError::Invalid)
}

/// `AddSeriesService.AddSeries(List<Series>)`, as Library Import calls
/// it: every series is looked up and validated against the library as it
/// stands, and one that fails refuses the whole request. One already in
/// the library, or named twice in the request, is left out without a
/// word. The rest are added together.
pub async fn add_many(
    db: &Db,
    provider: &dyn MetadataProvider,
    list: Vec<NewSeries>,
    now: DateTime<Utc>,
) -> Result<Vec<Series>, WriteError> {
    let mut prepared = Vec::new();
    for new in list {
        prepared.push(with_provider_data(provider, new, now).await?);
    }
    db.call(move |conn| {
        let existing = store::series::tvdb_ids(conn)?;
        let mut to_add: Vec<Series> = Vec::new();
        for (mut series, root) in prepared {
            let failures = place_and_validate(conn, &mut series, root.as_deref())?;
            if !failures.is_empty() {
                return Ok(Err(failures));
            }
            if existing.contains(&series.tvdb_id) {
                tracing::debug!(
                    "TVDB ID {} was not added due to validation failure: Series {} already exists in database",
                    series.tvdb_id,
                    series.title
                );
                continue;
            }
            if to_add.iter().any(|s| s.tvdb_id == series.tvdb_id) {
                continue;
            }
            if let Some(other) = to_add.iter().find(|s| s.title_slug == series.title_slug) {
                tracing::debug!(
                    "TVDB ID {} was not added due to validation failure: Duplicate Slug {} used by series {}",
                    series.tvdb_id,
                    series.title_slug,
                    other.tvdb_id
                );
                continue;
            }
            to_add.push(series);
        }
        // All of them, or none: a series that cannot be written (a tag
        // that does not exist) leaves nothing half added behind.
        let tx = conn.unchecked_transaction()?;
        let mut added = Vec::new();
        for series in to_add {
            added.push(insert_new(&tx, series)?);
        }
        tx.commit()?;
        Ok(Ok(added))
    })
    .await?
    .map_err(WriteError::Invalid)
}

/// `AddSkyhookData` and the request laid over it.
async fn with_provider_data(
    provider: &dyn MetadataProvider,
    new: NewSeries,
    now: DateTime<Utc>,
) -> Result<(Series, Option<String>), WriteError> {
    let info = match provider.series_info(new.tvdb_id).await {
        Ok(info) => info,
        Err(MetadataError::SeriesNotFound(_)) => {
            tracing::error!(
                "Series {} with TVDB ID {} was not found, it may have been removed from TheTVDB. Path: {}",
                new.title.as_deref().unwrap_or(""),
                new.tvdb_id,
                new.path.as_deref().unwrap_or("")
            );
            return Err(WriteError::InvalidPlain(vec![ValidationFailure::setting(
                "TvdbId",
                &format!(
                    "A series with this ID was not found. Path: {}",
                    new.path.as_deref().unwrap_or("")
                ),
                json!(new.tvdb_id),
            )]));
        }
        Err(e) => return Err(WriteError::Other(e.into())),
    };
    let mut series = info.series;
    // `newSeries.Seasons ?? series.Seasons`, then `ApplyChanges`.
    if !new.seasons.is_empty() {
        series.seasons = new.seasons;
    }
    series.tvdb_id = new.tvdb_id;
    series.quality_profile_id = new.quality_profile_id;
    series.season_folder = new.season_folder;
    series.monitored = new.monitored;
    series.monitor_new_items = new.monitor_new_items;
    series.series_type = new.series_type;
    series.tags = new.tags;
    series.add_options = new.add_options;
    series.path = new.path.clone().unwrap_or_default();
    series.added = now;
    if series
        .add_options
        .as_ref()
        .is_some_and(|o| MonitoringOptions::from_json(o).monitor == MonitorType::None)
    {
        series.monitored = false;
    }
    Ok((series, new.root_folder_path))
}

/// `SetPropertiesAndValidate`: the folder named when the request gave
/// none, then `AddSeriesValidator`, every rule of it.
fn place_and_validate(
    conn: &Connection,
    series: &mut Series,
    root: Option<&str>,
) -> anyhow::Result<Vec<ValidationFailure>> {
    if series.path.trim().is_empty() {
        let naming = store::config::naming(conn)?;
        let folder =
            crate::naming::series_folder(&crate::mediafiles::approve::naming_series(series), &naming);
        series.path = crate::mediafiles::join(root.unwrap_or(""), &folder);
    }
    let mut failures = Vec::new();
    failures.extend(path_rules(conn, 0, &series.path)?);
    if let Some(taken_by) = store::series::title_of_slug(conn, &series.title_slug)? {
        let mut f = ValidationFailure::rule(
            "TitleSlug",
            "SeriesTitleSlugValidator",
            &format!(
                "Title slug '{}' is in use by series '{taken_by}'. Check the FAQ for more information",
                series.title_slug
            ),
            json!(series.title_slug),
        );
        f.placeholders = vec![
            ("seriesTitle".into(), json!(taken_by)),
            ("slug".into(), json!(series.title_slug)),
        ];
        failures.push(f);
    }
    Ok(failures)
}

fn insert_new(conn: &Connection, mut series: Series) -> anyhow::Result<Series> {
    tracing::info!("Adding Series {} Path: [{}]", series.title, series.path);
    series.id = store::sequence::next_id(conn, "series")?;
    store::series::insert(conn, &series)?;
    Ok(series)
}

/// What a client sends back to edit a series (`SeriesResource.ToModel` and
/// `ApplyChanges` onto the stored one).
#[derive(Debug, Clone)]
pub struct SeriesEdit {
    pub id: i64,
    pub tvdb_id: i64,
    pub path: String,
    pub quality_profile_id: i64,
    pub season_folder: bool,
    pub monitored: bool,
    pub monitor_new_items: NewItemMonitor,
    pub series_type: SeriesType,
    pub tags: Vec<i64>,
    /// Season numbers and monitoring; the images do not come back.
    pub seasons: Vec<Season>,
}

/// `SeriesController`'s PUT rules.
pub fn validate_put(conn: &Connection, edit: &SeriesEdit) -> anyhow::Result<Vec<ValidationFailure>> {
    let mut failures = Vec::new();
    shared_rules(
        conn,
        edit.id,
        Some(&edit.path),
        edit.quality_profile_id,
        &mut failures,
    )?;
    if edit.path.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("Path", json!(edit.path)));
    } else if !valid_path(&edit.path) {
        failures.push(path_failure("Path", &edit.path));
    }
    Ok(failures)
}

/// `SeriesService.UpdateSeries` (with the episodes following a season whose
/// monitoring changed) for a PUT. Returns the stored series before and
/// after.
pub fn update(conn: &Connection, edit: &SeriesEdit) -> anyhow::Result<Option<(Series, Series)>> {
    let Some(stored) = store::series::get(conn, edit.id)? else {
        return Ok(None);
    };
    let mut series = stored.clone();
    series.tvdb_id = edit.tvdb_id;
    series.path = edit.path.clone();
    series.quality_profile_id = edit.quality_profile_id;
    series.season_folder = edit.season_folder;
    series.monitored = edit.monitored;
    series.monitor_new_items = edit.monitor_new_items;
    series.series_type = edit.series_type;
    series.tags = edit.tags.clone();
    series.seasons = edit.seasons.clone();
    for season in &series.seasons {
        if let Some(old) = stored
            .seasons
            .iter()
            .find(|s| s.season_number == season.season_number)
        {
            if old.monitored != season.monitored {
                store::episodes::set_monitored_by_season(
                    conn,
                    series.id,
                    season.season_number,
                    season.monitored,
                )?;
            }
        }
    }
    // `UpdateTags`, on every update of a series.
    if super::autotag::Rules::load(conn)?.apply(&mut series) {
        series.tags.sort_unstable();
    }
    store::series::update_settings(conn, &series)?;
    Ok(Some((stored, series)))
}

/// A series folder to move: the series and where its folder is now.
pub type SeriesMove = (i64, String);

/// The series editor's bulk edit (`SeriesEditorResource`).
#[derive(Debug, Clone, Default)]
pub struct BulkEdit {
    pub series_ids: Vec<i64>,
    pub monitored: Option<bool>,
    pub monitor_new_items: Option<NewItemMonitor>,
    pub quality_profile_id: Option<i64>,
    pub series_type: Option<SeriesType>,
    pub season_folder: Option<bool>,
    pub root_folder_path: Option<String>,
    pub tags: Option<Vec<i64>>,
    /// `add`, `remove` or `replace`.
    pub apply_tags: String,
    pub move_files: bool,
}

/// `SeriesEditorController.SaveAll` and `UpdateSeries(list,
/// useExistingRelativeFolder)`: the series as stored after the edit, and
/// the moves to queue (series id, source path) when files move.
pub fn update_many(conn: &Connection, edit: &BulkEdit) -> anyhow::Result<(Vec<Series>, Vec<SeriesMove>)> {
    let naming = store::config::naming(conn)?;
    let roots = store::catalog::list_root_folders(conn)?;
    let mut out = Vec::new();
    let mut moves = Vec::new();
    for &id in &edit.series_ids {
        let Some(mut s) = store::series::get(conn, id)? else {
            anyhow::bail!(
                "Expected query to return {} rows but returned {}",
                edit.series_ids.len(),
                out.len()
            );
        };
        if let Some(v) = edit.monitored {
            s.monitored = v;
        }
        if let Some(v) = edit.monitor_new_items {
            s.monitor_new_items = v;
        }
        if let Some(v) = edit.quality_profile_id {
            s.quality_profile_id = v;
        }
        if let Some(v) = edit.series_type {
            s.series_type = v;
        }
        if let Some(v) = edit.season_folder {
            s.season_folder = v;
        }
        if let Some(root) = edit.root_folder_path.as_deref().filter(|r| !r.trim().is_empty()) {
            moves.push((s.id, s.path.clone()));
            // `SeriesPathBuilder.BuildPath`: the folder it has under its
            // current root, or the one the naming builds.
            let folder = if !edit.move_files {
                let current_root = roots
                    .iter()
                    .filter(|r| crate::mediafiles::is_parent_path(&r.path, &s.path))
                    .map(|r| r.path.clone())
                    .next();
                match current_root {
                    Some(r) => crate::mediafiles::relative_path(&r, &s.path).to_string(),
                    None => crate::mediafiles::file_name(&s.path).to_string(),
                }
            } else {
                crate::naming::series_folder(&crate::mediafiles::approve::naming_series(&s), &naming)
            };
            s.path = crate::mediafiles::join(root, &folder);
        }
        if let Some(tags) = &edit.tags {
            match edit.apply_tags.to_ascii_lowercase().as_str() {
                "add" => {
                    for t in tags {
                        if !s.tags.contains(t) {
                            s.tags.push(*t);
                        }
                    }
                }
                "remove" => s.tags.retain(|t| !tags.contains(t)),
                "replace" => {
                    s.tags = tags.clone();
                    s.tags.dedup();
                }
                _ => {}
            }
            s.tags.sort_unstable();
        }
        out.push(s);
    }
    let rules = super::autotag::Rules::load(conn)?;
    for s in &mut out {
        if rules.apply(s) {
            s.tags.sort_unstable();
        }
        store::series::update_settings(conn, s)?;
    }
    if !edit.move_files {
        moves.clear();
    }
    Ok((out, moves))
}

/// `SeasonPassController.UpdateAll` for one series, then
/// `SetEpisodeMonitoredStatus`.
pub fn season_pass(
    conn: &Connection,
    series_id: i64,
    monitored: Option<bool>,
    seasons: &[Season],
    options: Option<&MonitoringOptions>,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<Series>> {
    let Some(mut series) = store::series::get(conn, series_id)? else {
        return Ok(None);
    };
    if let Some(v) = monitored {
        series.monitored = v;
    }
    for s in series.seasons.iter_mut() {
        if let Some(asked) = seasons.iter().find(|x| x.season_number == s.season_number) {
            s.monitored = asked.monitored;
        }
    }
    if options.is_some_and(|o| o.monitor == MonitorType::None) {
        series.monitored = false;
    }
    if let Some(options) = options {
        set_episode_monitored_status(conn, &mut series, options, now)?;
    } else {
        store::series::update_settings(conn, &series)?;
    }
    Ok(Some(series))
}

/// `SetEpisodeMonitoredStatus`: the episodes and seasons by the preset,
/// then the series saved without touching the episodes again.
pub fn set_episode_monitored_status(
    conn: &Connection,
    series: &mut Series,
    options: &MonitoringOptions,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let mut episodes = store::episodes::list_episodes(conn, series.id, None)?;
    if monitoring::apply(series, &mut episodes, options, now) {
        for e in &episodes {
            store::episodes::update_episode(conn, e)?;
        }
    }
    store::series::update_settings(conn, series)
}

/// `SeriesService.DeleteSeries` and the database side of its handlers:
/// the series, its episodes, files, history, download history, blocklist
/// and pending releases go; an exclusion is added when asked. The series
/// as they were are returned for the rest (files, covers, notifications).
pub fn delete(conn: &Connection, ids: &[i64], add_exclusion: bool) -> anyhow::Result<Vec<Series>> {
    let mut series = Vec::new();
    for &id in ids {
        match store::series::get(conn, id)? {
            Some(s) => series.push(s),
            None => anyhow::bail!(
                "Expected query to return {} rows but returned {}",
                ids.len(),
                series.len()
            ),
        }
    }
    store::series::delete_cascade(conn, ids)?;
    if add_exclusion {
        let existing = store::providers::list_import_list_exclusions(conn)?;
        let mut seen = std::collections::HashSet::new();
        for s in &series {
            if !seen.insert(s.tvdb_id) || existing.iter().any(|e| e.tvdb_id == s.tvdb_id) {
                continue;
            }
            let id = store::sequence::next_id(conn, "import_list_exclusions")?;
            store::providers::insert_import_list_exclusion(
                conn,
                &crate::model::ImportListExclusion {
                    id,
                    tvdb_id: s.tvdb_id,
                    title: s.title.clone(),
                },
            )?;
        }
    }
    Ok(series)
}

/// `MediaFileDeletionService.HandleAsync(SeriesDeletedEvent)`: the series
/// folder into the recycling bin (or gone), unless another series lives in
/// or at it.
pub fn delete_series_folder(
    config: &crate::store::config::MediaManagementConfig,
    series: &Series,
    remaining_paths: &[(i64, String)],
) {
    for (_, p) in remaining_paths {
        if crate::mediafiles::is_parent_path(&series.path, p) {
            tracing::error!(
                "Series path: '{}' is a parent of another series, not deleting files.",
                series.path
            );
            return;
        }
        if crate::mediafiles::path_equals(&series.path, p) {
            tracing::error!(
                "Series path: '{}' is the same as another series, not deleting files.",
                series.path
            );
            return;
        }
    }
    let path = std::path::Path::new(&series.path);
    if path.is_dir() {
        if let Err(e) = crate::mediafiles::transfer::recycle_folder(config, path) {
            tracing::error!(error = %e, "Unable to delete the folder of {}", series.title);
        }
    }
}

#[cfg(test)]
mod add_many_tests {
    use super::*;
    use crate::metadata::BoxFuture;
    use crate::metadata::SeriesInfo;

    /// A provider that knows two series, by the ids the store's sample
    /// series carry.
    struct Known;

    impl MetadataProvider for Known {
        fn series_info(&self, tvdb_id: i64) -> BoxFuture<'_, Result<SeriesInfo, MetadataError>> {
            Box::pin(async move {
                let id = tvdb_id - 1000;
                if !(1..=2).contains(&id) {
                    return Err(MetadataError::SeriesNotFound(tvdb_id));
                }
                let mut series = crate::store::series::tests::sample(id);
                series.id = 0;
                series.title = format!("Show {id}");
                series.title_slug = format!("show-{id}");
                series.tags = Vec::new();
                Ok(SeriesInfo {
                    series,
                    episodes: Vec::new(),
                })
            })
        }

        fn search(&self, _: &str) -> BoxFuture<'_, Result<Vec<Series>, MetadataError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    fn new(tvdb_id: i64, path: &str, tags: Vec<i64>) -> NewSeries {
        NewSeries {
            tvdb_id,
            title: None,
            path: Some(path.to_string()),
            root_folder_path: None,
            quality_profile_id: 1,
            season_folder: true,
            monitored: true,
            monitor_new_items: NewItemMonitor::All,
            series_type: SeriesType::Standard,
            tags,
            seasons: Vec::new(),
            add_options: None,
        }
    }

    async fn library() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.call(|conn| {
            conn.execute_batch(
                "INSERT INTO quality_profiles (id, name, cutoff, items) VALUES (1, 'Any', 1, '[]');
                 INSERT INTO tags (id, label) VALUES (1, 'drama');",
            )?;
            Ok(())
        })
        .await
        .unwrap();
        db
    }

    async fn titles(db: &Db) -> Vec<String> {
        db.call(|conn| Ok(store::series::list(conn)?.into_iter().map(|s| s.title).collect()))
            .await
            .unwrap()
    }

    /// A series that cannot be written takes the others of the request
    /// with it: nothing is left half added, and the same request, put
    /// right, goes through.
    #[tokio::test]
    async fn a_series_that_cannot_be_written_leaves_nothing_behind() {
        let db = library().await;
        let now = Utc::now();
        // The second names a tag that does not exist.
        let request = |tags: Vec<i64>| vec![new(1001, "/tv/Show 1", vec![1]), new(1002, "/tv/Show 2", tags)];
        let refused = add_many(&db, &Known, request(vec![999]), now).await;
        assert!(matches!(refused, Err(WriteError::Other(_))), "{refused:?}");
        assert!(titles(&db).await.is_empty());
        let added = add_many(&db, &Known, request(vec![1]), now).await.unwrap();
        assert_eq!(added.len(), 2);
        assert_eq!(titles(&db).await.len(), 2);
    }

    /// One nobody knows refuses the request, and so does one already
    /// there; one named twice is added once.
    #[tokio::test]
    async fn unknown_series_refuse_and_known_ones_are_left_out() {
        let db = library().await;
        let now = Utc::now();
        let refused = add_many(
            &db,
            &Known,
            vec![new(1001, "/tv/Show 1", vec![]), new(5, "/tv/Nobody", vec![])],
            now,
        )
        .await;
        assert!(matches!(refused, Err(WriteError::InvalidPlain(_))), "{refused:?}");
        assert!(titles(&db).await.is_empty());
        let twice = vec![
            new(1001, "/tv/Show 1", vec![]),
            new(1001, "/tv/Show 1 again", vec![]),
        ];
        assert_eq!(add_many(&db, &Known, twice, now).await.unwrap().len(), 1);
        // Already in the library: its title slug is taken, which refuses
        // the request before the library is looked at for its id (as
        // recorded: `series-import-again`), and the other with it.
        let again = vec![
            new(1001, "/tv/Show 1 once more", vec![]),
            new(1002, "/tv/Show 2", vec![]),
        ];
        let refused = add_many(&db, &Known, again, now).await;
        assert!(matches!(refused, Err(WriteError::Invalid(_))), "{refused:?}");
        assert_eq!(titles(&db).await, ["Show 1"]);
        // The same single add, on the same rules.
        let single = add(&db, &Known, new(1001, "/tv/Show 1 elsewhere", vec![]), now).await;
        assert!(matches!(single, Err(WriteError::Invalid(_))), "{single:?}");
    }
}
