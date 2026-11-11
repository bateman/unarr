//! Importer for the original applications' data directories.
//!
//! Reads `config.xml` (port, API key, auth, URL base) and the SQLite database of
//! each personality. The original schema is versioned through its
//! `VersionInfo` table; the importer checks that version and maps what it knows.

pub mod config_xml;
mod legacy;
pub mod radarr;
mod shared;
pub mod sonarr;

pub use legacy::ImportReport;
pub use unarr_core::config::{AuthenticationMethod, AuthenticationRequired, HostConfig};
