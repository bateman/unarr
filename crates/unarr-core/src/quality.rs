//! The fixed quality and language tables the originals compile in. Their
//! numeric ids appear in databases and in the API, so they are reproduced
//! exactly, in the originals' order.

use serde::{Deserialize, Serialize};

/// Where a release came from, spelled as the API spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QualitySource {
    Unknown,
    Television,
    TelevisionRaw,
    Web,
    WebRip,
    #[serde(rename = "dvd")]
    Dvd,
    Bluray,
    BlurayRaw,
}

/// One entry of the quality table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
    pub id: i64,
    pub name: &'static str,
    pub source: QualitySource,
    pub resolution: i64,
}

/// A quality definition default: the quality, its sort weight and the size
/// limits (MB per minute) the originals start with.
pub struct QualityDefault {
    pub quality: Quality,
    pub weight: i64,
    pub min_size: f64,
    pub max_size: Option<f64>,
    pub preferred_size: f64,
}

macro_rules! q {
    ($id:expr, $name:expr, $source:ident, $res:expr) => {
        Quality {
            id: $id,
            name: $name,
            source: QualitySource::$source,
            resolution: $res,
        }
    };
}

/// Sonarr's qualities, in definition order (which fixes the `weight`).
pub const SONARR_QUALITIES: &[QualityDefault] = &[
    QualityDefault {
        quality: q!(0, "Unknown", Unknown, 0),
        weight: 1,
        min_size: 1.0,
        max_size: Some(199.9),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(1, "SDTV", Television, 480),
        weight: 2,
        min_size: 2.0,
        max_size: Some(100.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(12, "WEBRip-480p", WebRip, 480),
        weight: 3,
        min_size: 2.0,
        max_size: Some(100.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(8, "WEBDL-480p", Web, 480),
        weight: 3,
        min_size: 2.0,
        max_size: Some(100.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(2, "DVD", Dvd, 480),
        weight: 4,
        min_size: 2.0,
        max_size: Some(100.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(13, "Bluray-480p", Bluray, 480),
        weight: 5,
        min_size: 2.0,
        max_size: Some(100.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(22, "Bluray-576p", Bluray, 576),
        weight: 6,
        min_size: 2.0,
        max_size: Some(100.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(4, "HDTV-720p", Television, 720),
        weight: 7,
        min_size: 3.0,
        max_size: Some(125.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(9, "HDTV-1080p", Television, 1080),
        weight: 8,
        min_size: 4.0,
        max_size: Some(125.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(10, "Raw-HD", TelevisionRaw, 1080),
        weight: 9,
        min_size: 4.0,
        max_size: None,
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(14, "WEBRip-720p", WebRip, 720),
        weight: 10,
        min_size: 3.0,
        max_size: Some(130.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(5, "WEBDL-720p", Web, 720),
        weight: 10,
        min_size: 3.0,
        max_size: Some(130.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(6, "Bluray-720p", Bluray, 720),
        weight: 11,
        min_size: 4.0,
        max_size: Some(130.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(15, "WEBRip-1080p", WebRip, 1080),
        weight: 12,
        min_size: 4.0,
        max_size: Some(130.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(3, "WEBDL-1080p", Web, 1080),
        weight: 12,
        min_size: 4.0,
        max_size: Some(130.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(7, "Bluray-1080p", Bluray, 1080),
        weight: 13,
        min_size: 4.0,
        max_size: Some(155.0),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(20, "Bluray-1080p Remux", BlurayRaw, 1080),
        weight: 14,
        min_size: 35.0,
        max_size: None,
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(16, "HDTV-2160p", Television, 2160),
        weight: 15,
        min_size: 35.0,
        max_size: Some(199.9),
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(17, "WEBRip-2160p", WebRip, 2160),
        weight: 16,
        min_size: 35.0,
        max_size: None,
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(18, "WEBDL-2160p", Web, 2160),
        weight: 16,
        min_size: 35.0,
        max_size: None,
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(19, "Bluray-2160p", Bluray, 2160),
        weight: 17,
        min_size: 35.0,
        max_size: None,
        preferred_size: 95.0,
    },
    QualityDefault {
        quality: q!(21, "Bluray-2160p Remux", BlurayRaw, 2160),
        weight: 18,
        min_size: 35.0,
        max_size: None,
        preferred_size: 95.0,
    },
];

impl Quality {
    /// Look a quality up by its numeric id; unknown ids map to `Unknown`
    /// with that id kept, as the originals do for ids they do not know.
    pub fn by_id(id: i64) -> Quality {
        SONARR_QUALITIES
            .iter()
            .map(|d| d.quality)
            .find(|q| q.id == id)
            .unwrap_or(Quality {
                id,
                name: "Unknown",
                source: QualitySource::Unknown,
                resolution: 0,
            })
    }

    /// Sort weight of a quality, from its position in the definition table.
    pub fn weight(id: i64) -> i64 {
        SONARR_QUALITIES
            .iter()
            .find(|d| d.quality.id == id)
            .map(|d| d.weight)
            .unwrap_or(0)
    }
}

/// A language the originals know, by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Language {
    pub id: i64,
    pub name: &'static str,
}

/// Sonarr's language table (`Language.All`), in id order. `Original` (-2) is
/// the pseudo-language meaning "the series' own language". Ids above
/// [`LAST_RELEASED_LANGUAGE_ID`] exist only on Sonarr's develop branch.
pub const LANGUAGES: &[Language] = &[
    Language {
        id: -2,
        name: "Original",
    },
    Language {
        id: 0,
        name: "Unknown",
    },
    Language {
        id: 1,
        name: "English",
    },
    Language {
        id: 2,
        name: "French",
    },
    Language {
        id: 3,
        name: "Spanish",
    },
    Language {
        id: 4,
        name: "German",
    },
    Language {
        id: 5,
        name: "Italian",
    },
    Language {
        id: 6,
        name: "Danish",
    },
    Language { id: 7, name: "Dutch" },
    Language {
        id: 8,
        name: "Japanese",
    },
    Language {
        id: 9,
        name: "Icelandic",
    },
    Language {
        id: 10,
        name: "Chinese",
    },
    Language {
        id: 11,
        name: "Russian",
    },
    Language {
        id: 12,
        name: "Polish",
    },
    Language {
        id: 13,
        name: "Vietnamese",
    },
    Language {
        id: 14,
        name: "Swedish",
    },
    Language {
        id: 15,
        name: "Norwegian",
    },
    Language {
        id: 16,
        name: "Finnish",
    },
    Language {
        id: 17,
        name: "Turkish",
    },
    Language {
        id: 18,
        name: "Portuguese",
    },
    Language {
        id: 19,
        name: "Flemish",
    },
    Language {
        id: 20,
        name: "Greek",
    },
    Language {
        id: 21,
        name: "Korean",
    },
    Language {
        id: 22,
        name: "Hungarian",
    },
    Language {
        id: 23,
        name: "Hebrew",
    },
    Language {
        id: 24,
        name: "Lithuanian",
    },
    Language {
        id: 25,
        name: "Czech",
    },
    Language {
        id: 26,
        name: "Arabic",
    },
    Language {
        id: 27,
        name: "Hindi",
    },
    Language {
        id: 28,
        name: "Bulgarian",
    },
    Language {
        id: 29,
        name: "Malayalam",
    },
    Language {
        id: 30,
        name: "Ukrainian",
    },
    Language {
        id: 31,
        name: "Slovak",
    },
    Language { id: 32, name: "Thai" },
    Language {
        id: 33,
        name: "Portuguese (Brazil)",
    },
    Language {
        id: 34,
        name: "Spanish (Latino)",
    },
    Language {
        id: 35,
        name: "Romanian",
    },
    Language {
        id: 36,
        name: "Latvian",
    },
    Language {
        id: 37,
        name: "Persian",
    },
    Language {
        id: 38,
        name: "Catalan",
    },
    Language {
        id: 39,
        name: "Croatian",
    },
    Language {
        id: 40,
        name: "Serbian",
    },
    Language {
        id: 41,
        name: "Bosnian",
    },
    Language {
        id: 42,
        name: "Estonian",
    },
    Language {
        id: 43,
        name: "Tamil",
    },
    Language {
        id: 44,
        name: "Indonesian",
    },
    Language {
        id: 45,
        name: "Macedonian",
    },
    Language {
        id: 46,
        name: "Slovenian",
    },
    Language {
        id: 47,
        name: "Azerbaijani",
    },
    Language {
        id: 48,
        name: "Uzbek",
    },
    Language {
        id: 49,
        name: "Malay",
    },
    Language { id: 50, name: "Urdu" },
    Language {
        id: 51,
        name: "Romansh",
    },
    Language {
        id: 52,
        name: "Georgian",
    },
    Language {
        id: 53,
        name: "Irish",
    },
];

/// Highest language id known to Sonarr 4.0.20, the release Unarr mirrors.
pub const LAST_RELEASED_LANGUAGE_ID: i64 = 46;

impl Language {
    /// Look a language up by id; unknown ids become `Unknown` (0), which is
    /// what Sonarr 4.0.20 does with ids it does not recognise.
    pub fn by_id(id: i64) -> Language {
        LANGUAGES
            .iter()
            .copied()
            .find(|l| l.id == id)
            .unwrap_or(Language {
                id: 0,
                name: "Unknown",
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_lookups_work() {
        let mut ids: Vec<i64> = SONARR_QUALITIES.iter().map(|d| d.quality.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SONARR_QUALITIES.len());
        assert_eq!(Quality::by_id(7).name, "Bluray-1080p");
        assert_eq!(Quality::by_id(99).name, "Unknown");
        assert_eq!(Quality::by_id(99).id, 99);
        assert_eq!(Quality::weight(3), 12);
        assert_eq!(
            serde_json::to_value(QualitySource::WebRip).unwrap(),
            serde_json::json!("webRip")
        );
        assert_eq!(
            serde_json::to_value(QualitySource::Dvd).unwrap(),
            serde_json::json!("dvd")
        );
        assert_eq!(Language::by_id(8).name, "Japanese");
        assert_eq!(Language::by_id(999).id, 0);
    }
}
