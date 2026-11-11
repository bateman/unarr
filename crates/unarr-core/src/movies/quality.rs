//! Radarr's catalogues: its qualities, its languages and its indexer flags.
//!
//! Radarr's qualities differ from Sonarr's where movies do: the sources a
//! cinema release goes through before a proper one exists (workprint, cam,
//! telesync, telecine), screeners and regional releases, disc images, and
//! remuxes ranked above the encodes. A quality is a source, a resolution
//! and a modifier; the ids both applications share do not mean the same
//! weight, so each has its own table.

use serde::Serialize;

/// A quality as the API shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MovieQuality {
    pub id: i64,
    pub name: &'static str,
    pub source: &'static str,
    pub resolution: i64,
    pub modifier: &'static str,
}

/// A quality with its place in the order (`Quality.DefaultQualityDefinitions`):
/// WEBDL and WEBRip of one resolution share a weight.
pub struct MovieQualityDefault {
    pub quality: MovieQuality,
    pub weight: i64,
}

macro_rules! q {
    ($id:expr, $name:expr, $source:expr, $res:expr, $modifier:expr, $weight:expr) => {
        MovieQualityDefault {
            quality: MovieQuality {
                id: $id,
                name: $name,
                source: $source,
                resolution: $res,
                modifier: $modifier,
            },
            weight: $weight,
        }
    };
}

/// Every quality Radarr knows, in weight order.
pub const RADARR_QUALITIES: &[MovieQualityDefault] = &[
    q!(0, "Unknown", "unknown", 0, "none", 1),
    q!(24, "WORKPRINT", "workprint", 0, "none", 2),
    q!(25, "CAM", "cam", 0, "none", 3),
    q!(26, "TELESYNC", "telesync", 0, "none", 4),
    q!(27, "TELECINE", "telecine", 0, "none", 5),
    q!(29, "REGIONAL", "dvd", 480, "regional", 6),
    q!(28, "DVDSCR", "dvd", 480, "screener", 7),
    q!(1, "SDTV", "tv", 480, "none", 8),
    q!(2, "DVD", "dvd", 0, "none", 9),
    q!(23, "DVD-R", "dvd", 480, "remux", 10),
    q!(8, "WEBDL-480p", "webdl", 480, "none", 11),
    q!(12, "WEBRip-480p", "webrip", 480, "none", 11),
    q!(20, "Bluray-480p", "bluray", 480, "none", 12),
    q!(21, "Bluray-576p", "bluray", 576, "none", 13),
    q!(4, "HDTV-720p", "tv", 720, "none", 14),
    q!(5, "WEBDL-720p", "webdl", 720, "none", 15),
    q!(14, "WEBRip-720p", "webrip", 720, "none", 15),
    q!(6, "Bluray-720p", "bluray", 720, "none", 16),
    q!(9, "HDTV-1080p", "tv", 1080, "none", 17),
    q!(3, "WEBDL-1080p", "webdl", 1080, "none", 18),
    q!(15, "WEBRip-1080p", "webrip", 1080, "none", 18),
    q!(7, "Bluray-1080p", "bluray", 1080, "none", 19),
    q!(30, "Remux-1080p", "bluray", 1080, "remux", 20),
    q!(16, "HDTV-2160p", "tv", 2160, "none", 21),
    q!(18, "WEBDL-2160p", "webdl", 2160, "none", 22),
    q!(17, "WEBRip-2160p", "webrip", 2160, "none", 22),
    q!(19, "Bluray-2160p", "bluray", 2160, "none", 23),
    q!(31, "Remux-2160p", "bluray", 2160, "remux", 24),
    q!(22, "BR-DISK", "bluray", 1080, "brdisk", 25),
    q!(10, "Raw-HD", "tv", 1080, "rawhd", 26),
];

impl MovieQuality {
    pub const UNKNOWN: MovieQuality = RADARR_QUALITIES[0].quality;

    /// Every quality, in weight order.
    pub fn all() -> impl Iterator<Item = MovieQuality> {
        RADARR_QUALITIES.iter().map(|d| d.quality)
    }

    /// The quality with this id; an id Radarr does not know is `Unknown`.
    pub fn by_id(id: i64) -> MovieQuality {
        RADARR_QUALITIES
            .iter()
            .find(|d| d.quality.id == id)
            .map(|d| d.quality)
            .unwrap_or(MovieQuality::UNKNOWN)
    }

    /// The weight of the quality with this id, 0 when unknown.
    pub fn weight(id: i64) -> i64 {
        RADARR_QUALITIES
            .iter()
            .find(|d| d.quality.id == id)
            .map(|d| d.weight)
            .unwrap_or(0)
    }
}

/// `QualitySource`, as Radarr numbers it.
pub fn source_number(source: &str) -> i64 {
    match source {
        "cam" => 1,
        "telesync" => 2,
        "telecine" => 3,
        "workprint" => 4,
        "dvd" => 5,
        "tv" => 6,
        "webdl" => 7,
        "webrip" => 8,
        "bluray" => 9,
        _ => 0,
    }
}

/// `Modifier`, as Radarr numbers it.
pub fn modifier_number(modifier: &str) -> i64 {
    match modifier {
        "regional" => 1,
        "screener" => 2,
        "rawhd" => 3,
        "brdisk" => 4,
        "remux" => 5,
        _ => 0,
    }
}

/// A language of Radarr's table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieLanguage {
    pub id: i64,
    pub name: &'static str,
}

/// Radarr's languages (`Language.All`) in the order the API lists them:
/// the pseudo-languages first (`Any` -1, `Original` -2, `Unknown` 0), the
/// others by name.
pub const RADARR_LANGUAGES: &[MovieLanguage] = &[
    MovieLanguage { id: -1, name: "Any" },
    MovieLanguage {
        id: -2,
        name: "Original",
    },
    MovieLanguage {
        id: 0,
        name: "Unknown",
    },
    MovieLanguage {
        id: 51,
        name: "Afrikaans",
    },
    MovieLanguage {
        id: 50,
        name: "Albanian",
    },
    MovieLanguage {
        id: 31,
        name: "Arabic",
    },
    MovieLanguage {
        id: 34,
        name: "Bengali",
    },
    MovieLanguage {
        id: 41,
        name: "Bosnian",
    },
    MovieLanguage {
        id: 29,
        name: "Bulgarian",
    },
    MovieLanguage {
        id: 38,
        name: "Catalan",
    },
    MovieLanguage {
        id: 10,
        name: "Chinese",
    },
    MovieLanguage {
        id: 39,
        name: "Croatian",
    },
    MovieLanguage {
        id: 25,
        name: "Czech",
    },
    MovieLanguage {
        id: 6,
        name: "Danish",
    },
    MovieLanguage { id: 7, name: "Dutch" },
    MovieLanguage {
        id: 1,
        name: "English",
    },
    MovieLanguage {
        id: 42,
        name: "Estonian",
    },
    MovieLanguage {
        id: 16,
        name: "Finnish",
    },
    MovieLanguage {
        id: 19,
        name: "Flemish",
    },
    MovieLanguage {
        id: 2,
        name: "French",
    },
    MovieLanguage {
        id: 57,
        name: "Georgian",
    },
    MovieLanguage {
        id: 4,
        name: "German",
    },
    MovieLanguage {
        id: 20,
        name: "Greek",
    },
    MovieLanguage {
        id: 23,
        name: "Hebrew",
    },
    MovieLanguage {
        id: 26,
        name: "Hindi",
    },
    MovieLanguage {
        id: 22,
        name: "Hungarian",
    },
    MovieLanguage {
        id: 9,
        name: "Icelandic",
    },
    MovieLanguage {
        id: 44,
        name: "Indonesian",
    },
    MovieLanguage {
        id: 5,
        name: "Italian",
    },
    MovieLanguage {
        id: 8,
        name: "Japanese",
    },
    MovieLanguage {
        id: 49,
        name: "Kannada",
    },
    MovieLanguage {
        id: 21,
        name: "Korean",
    },
    MovieLanguage {
        id: 36,
        name: "Latvian",
    },
    MovieLanguage {
        id: 24,
        name: "Lithuanian",
    },
    MovieLanguage {
        id: 46,
        name: "Macedonian",
    },
    MovieLanguage {
        id: 48,
        name: "Malayalam",
    },
    MovieLanguage {
        id: 52,
        name: "Marathi",
    },
    MovieLanguage {
        id: 56,
        name: "Mongolian",
    },
    MovieLanguage {
        id: 15,
        name: "Norwegian",
    },
    MovieLanguage {
        id: 33,
        name: "Persian",
    },
    MovieLanguage {
        id: 12,
        name: "Polish",
    },
    MovieLanguage {
        id: 18,
        name: "Portuguese",
    },
    MovieLanguage {
        id: 30,
        name: "Portuguese (Brazil)",
    },
    MovieLanguage {
        id: 27,
        name: "Romanian",
    },
    MovieLanguage {
        id: 55,
        name: "Romansh",
    },
    MovieLanguage {
        id: 11,
        name: "Russian",
    },
    MovieLanguage {
        id: 40,
        name: "Serbian",
    },
    MovieLanguage {
        id: 35,
        name: "Slovak",
    },
    MovieLanguage {
        id: 47,
        name: "Slovenian",
    },
    MovieLanguage {
        id: 3,
        name: "Spanish",
    },
    MovieLanguage {
        id: 37,
        name: "Spanish (Latino)",
    },
    MovieLanguage {
        id: 14,
        name: "Swedish",
    },
    MovieLanguage {
        id: 53,
        name: "Tagalog",
    },
    MovieLanguage {
        id: 43,
        name: "Tamil",
    },
    MovieLanguage {
        id: 45,
        name: "Telugu",
    },
    MovieLanguage { id: 28, name: "Thai" },
    MovieLanguage {
        id: 17,
        name: "Turkish",
    },
    MovieLanguage {
        id: 32,
        name: "Ukrainian",
    },
    MovieLanguage { id: 54, name: "Urdu" },
    MovieLanguage {
        id: 13,
        name: "Vietnamese",
    },
];

impl MovieLanguage {
    pub fn by_id(id: i64) -> Option<MovieLanguage> {
        RADARR_LANGUAGES.iter().copied().find(|l| l.id == id)
    }

    /// The name of the language with this id; `Unknown` for one Radarr
    /// does not know, as the originals read an unexpected stored value.
    pub fn name_of(id: i64) -> &'static str {
        MovieLanguage::by_id(id).map(|l| l.name).unwrap_or("Unknown")
    }
}

/// Radarr's indexer flags (`IndexerFlags`), a bit each.
pub const RADARR_INDEXER_FLAGS: &[(i64, &str)] = &[
    (1, "G_Freeleech"),
    (2, "G_Halfleech"),
    (4, "G_DoubleUpload"),
    (8, "PTP_Golden"),
    (16, "PTP_Approved"),
    (32, "G_Internal"),
    (128, "G_Scene"),
    (256, "G_Freeleech75"),
    (512, "G_Freeleech25"),
    (2048, "Nuked"),
];

/// The names of the flags set in `flags`, as the history's `data` and the
/// notifications spell them: comma separated, in bit order.
pub fn indexer_flag_names(flags: i64) -> Vec<&'static str> {
    RADARR_INDEXER_FLAGS
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// `IndexerFlags.ToString()`: the flag names, or the number when no bit
/// has one (`0`).
pub fn indexer_flags_text(flags: i64) -> String {
    let names = indexer_flag_names(flags);
    if names.is_empty() {
        flags.to_string()
    } else {
        names.join(", ")
    }
}

/// `Enum.TryParse<IndexerFlags>(text, ignoreCase)`: the names (or the
/// number) the history stored, back to bits; `None` for text that is not a
/// flag list.
pub fn parse_indexer_flags(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(n) = text.parse::<i64>() {
        return Some(n);
    }
    let mut flags = 0;
    for part in text.split(',') {
        let (bit, _) = RADARR_INDEXER_FLAGS
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(part.trim()))?;
        flags |= bit;
    }
    Some(flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualities_are_radarrs() {
        assert_eq!(RADARR_QUALITIES.len(), 30);
        let remux = MovieQuality::by_id(30);
        assert_eq!(
            (remux.name, remux.source, remux.modifier),
            ("Remux-1080p", "bluray", "remux")
        );
        // WEBDL and WEBRip of one resolution weigh the same.
        assert_eq!(MovieQuality::weight(3), MovieQuality::weight(15));
        // A remux outranks the encode, a disc image the remux.
        assert!(MovieQuality::weight(30) > MovieQuality::weight(7));
        assert!(MovieQuality::weight(22) > MovieQuality::weight(31));
        assert_eq!(MovieQuality::by_id(999), MovieQuality::UNKNOWN);
    }

    #[test]
    fn languages_and_flags() {
        assert_eq!(MovieLanguage::name_of(5), "Italian");
        assert_eq!(MovieLanguage::name_of(-2), "Original");
        assert_eq!(MovieLanguage::name_of(9999), "Unknown");
        assert_eq!(indexer_flag_names(2 | 32), vec!["G_Halfleech", "G_Internal"]);
    }
}
