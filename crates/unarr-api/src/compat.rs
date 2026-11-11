//! Identity Unarr presents to clients. Clients branch on `version` (Bazarr
//! picks its Sonarr v4 code path from it, the UI shows it), so each
//! personality reports the version of the original whose API it reproduces.
//! Unarr's own identity goes in `packageVersion`/`packageAuthor`, fields the
//! originals use for the distribution package.

use unarr_core::Personality;

/// Version of the original application whose API surface this build mirrors.
pub const fn emulated_version(personality: Personality) -> &'static str {
    match personality {
        Personality::Sonarr => "4.0.20.3014",
        Personality::Radarr => "6.4.4.10685",
        Personality::Lidarr => "2.0.0.0",
    }
}

/// Build time of that release, as `buildTime` reports it.
pub const fn emulated_build_time(personality: Personality) -> &'static str {
    match personality {
        Personality::Sonarr => "2026-09-16T16:37:42Z",
        Personality::Radarr => "2026-09-14T15:54:37Z",
        Personality::Lidarr => "2026-01-01T00:00:00Z",
    }
}

/// Migration level of that release, reported when nothing was imported.
pub const fn emulated_migration(personality: Personality) -> i64 {
    match personality {
        Personality::Sonarr => 217,
        Personality::Radarr => 242,
        Personality::Lidarr => 0,
    }
}

/// Unarr's own version, for `packageVersion`.
pub const UNARR_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const UNARR_PACKAGE_AUTHOR: &str = "Unarr";
