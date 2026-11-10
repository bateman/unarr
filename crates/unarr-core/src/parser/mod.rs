//! Release name parsing: a port of the originals' `Parser`, `QualityParser`
//! and `LanguageParser`, checked against their test vectors
//! (`tests/vectors/parser/`, extracted by `scripts/port-vectors.py`).

pub mod language;
pub mod media_extensions;
pub mod netregex;
pub mod quality;
pub mod subtitles;
pub mod title;

pub use quality::{Detection, ParsedQuality};
pub use title::{
    clean_series_title, normalize_episode_title, normalize_title, parse_path, parse_release_group,
    parse_series_name, parse_title, parse_title_at, remove_file_extension, ParsedEpisodeInfo,
    SeriesTitleInfo,
};
