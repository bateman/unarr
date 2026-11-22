//! Identity Unarr presents to clients. Clients branch on `version` (Bazarr
//! picks its Sonarr v4 code path from it, the UI shows it), so each
//! personality reports the version of the original whose API it reproduces.
//! Unarr's own identity goes in `packageVersion`/`packageAuthor`, fields the
//! originals use for the distribution package.
//!
//! The release each personality emulates is pinned in `upstream/<app>.pin`
//! and read through [`unarr_core::pin`]; Lidarr, which emulates none yet,
//! has placeholders here.

use unarr_core::{pin, Personality};

/// Version of the original application whose API surface this build mirrors.
pub const fn emulated_version(personality: Personality) -> &'static str {
    match pin::of(personality) {
        Some(pin) => pin.version,
        None => "2.0.0.0",
    }
}

/// Build time of that release, as `buildTime` reports it.
pub const fn emulated_build_time(personality: Personality) -> &'static str {
    match pin::of(personality) {
        Some(pin) => pin.build_time,
        None => "2026-01-01T00:00:00Z",
    }
}

/// Migration level of that release, reported when nothing was imported.
pub const fn emulated_migration(personality: Personality) -> i64 {
    match pin::of(personality) {
        Some(pin) => pin.migration,
        None => 0,
    }
}

/// Unarr's own version, for `packageVersion`.
pub const UNARR_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const UNARR_PACKAGE_AUTHOR: &str = "Unarr";
