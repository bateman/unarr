//! Domain model shared by every personality (Sonarr, Radarr, Lidarr).
//!
//! Nothing here knows about HTTP or about the on-disk layout of the original
//! applications; that lives in `unarr-api` and `unarr-import` respectively.

pub mod collation;
pub mod commands;
pub mod config;
pub mod customformat;
pub mod db;
pub mod decision;
pub mod downloadclients;
pub mod downloads;
pub mod fs;
pub mod health;
pub mod http;
pub mod indexers;
pub mod model;
pub mod naming;
pub mod parser;
pub mod profile;
pub mod quality;
pub mod search;
pub mod store;
pub mod time;
pub mod torrent;
pub mod validation;

pub use db::Db;

/// The media kind a personality manages. One Unarr process may run several.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Personality {
    /// TV series, API-compatible with Sonarr v4 (`/api/v3`, port 8989).
    Sonarr,
    /// Movies, API-compatible with Radarr v5 (`/api/v3`, port 7878).
    Radarr,
    /// Music, API-compatible with Lidarr v2 (`/api/v1`, port 8686).
    Lidarr,
}

impl Personality {
    /// Default listening port of the original application.
    pub const fn default_port(self) -> u16 {
        match self {
            Personality::Sonarr => 8989,
            Personality::Radarr => 7878,
            Personality::Lidarr => 8686,
        }
    }

    /// API prefix the original application exposes.
    pub const fn api_prefix(self) -> &'static str {
        match self {
            Personality::Sonarr | Personality::Radarr => "/api/v3",
            Personality::Lidarr => "/api/v1",
        }
    }

    /// Name of the original application, as `appName` reports it.
    pub const fn app_name(self) -> &'static str {
        match self {
            Personality::Sonarr => "Sonarr",
            Personality::Radarr => "Radarr",
            Personality::Lidarr => "Lidarr",
        }
    }

    /// Name of the original database file, used by the importer.
    pub const fn legacy_db_file(self) -> &'static str {
        match self {
            Personality::Sonarr => "sonarr.db",
            Personality::Radarr => "radarr.db",
            Personality::Lidarr => "lidarr.db",
        }
    }

    /// Name of Unarr's own database file for this personality.
    pub const fn db_file(self) -> &'static str {
        match self {
            Personality::Sonarr => "unarr-sonarr.db",
            Personality::Radarr => "unarr-radarr.db",
            Personality::Lidarr => "unarr-lidarr.db",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_and_prefixes_match_the_originals() {
        assert_eq!(Personality::Sonarr.default_port(), 8989);
        assert_eq!(Personality::Radarr.default_port(), 7878);
        assert_eq!(Personality::Lidarr.default_port(), 8686);
        assert_eq!(Personality::Lidarr.api_prefix(), "/api/v1");
        assert_eq!(Personality::Sonarr.api_prefix(), "/api/v3");
        assert_eq!(Personality::Sonarr.app_name(), "Sonarr");
    }
}
