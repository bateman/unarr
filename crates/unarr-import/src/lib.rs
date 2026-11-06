//! Importer for the original applications' data directories.
//!
//! Reads `config.xml` (port, API key, auth, URL base) and the SQLite database of
//! each personality. The original schema is versioned through its
//! `VersionInfo` table; the importer checks that version and maps what it knows.

use std::path::Path;

use unarr_core::Personality;

/// Minimal description of what an import would do. Filled in during phase 1.
#[derive(Debug)]
pub struct ImportPlan {
    pub personality: Personality,
    pub legacy_db: std::path::PathBuf,
}

/// Locate the legacy database for `personality` under `config_dir`.
pub fn plan(personality: Personality, config_dir: &Path) -> anyhow::Result<ImportPlan> {
    let legacy_db = config_dir.join(personality.legacy_db_file());
    anyhow::ensure!(
        legacy_db.is_file(),
        "no {} in {}",
        personality.legacy_db_file(),
        config_dir.display()
    );
    Ok(ImportPlan {
        personality,
        legacy_db,
    })
}
