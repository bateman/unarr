//! The releases of the originals Unarr emulates, as `upstream/<app>.pin`
//! pins them (issue #9). Everything that says which Sonarr or Radarr Unarr
//! is — the identity clients read, the user agent towards the metadata
//! servers, what Plex is told, the tests that check them — reads these,
//! so that a new pin is one file. `build.rs` generates them from the pin
//! files and refuses one it cannot read.

use crate::Personality;

/// One pinned release of an original.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin {
    /// `BuildInfo.Version`, e.g. `4.0.20.3014`.
    pub version: &'static str,
    /// `BuildInfo.BuildDateTime`, RFC 3339 in UTC.
    pub build_time: &'static str,
    /// The release's migration level.
    pub migration: i64,
}

include!(concat!(env!("OUT_DIR"), "/pins.rs"));

/// The pin of a personality; `None` for one that emulates no release yet.
pub const fn of(personality: Personality) -> Option<Pin> {
    match personality {
        Personality::Sonarr => Some(SONARR),
        Personality::Radarr => Some(RADARR),
        Personality::Lidarr => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pins_are_read() {
        for pin in [SONARR, RADARR] {
            assert_eq!(pin.version.split('.').count(), 4, "{pin:?}");
            assert!(
                chrono::DateTime::parse_from_rfc3339(pin.build_time).is_ok(),
                "{pin:?}"
            );
            assert!(pin.migration > 0, "{pin:?}");
        }
        assert_eq!(of(Personality::Sonarr), Some(SONARR));
        assert_eq!(of(Personality::Radarr), Some(RADARR));
        assert_eq!(of(Personality::Lidarr), None);
    }
}
