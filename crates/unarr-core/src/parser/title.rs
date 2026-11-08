//! Port of the originals' `Parser`: series title, numbering (standard,
//! multi-episode, season packs, daily, anime absolute and special), release
//! group and hash from a release name or a file path. The patterns are the
//! originals' own, run through [`super::netregex`]; the control flow follows
//! `Parser.cs` line by line, quirks included, because the ported test vectors
//! and the rest of the pipeline depend on them.

use chrono::NaiveDate;
use unicode_normalization::UnicodeNormalization;

use crate::model::ReleaseType;
use crate::net_regex;
use crate::parser::language::parse_languages;
use crate::parser::media_extensions::{is_media_extension, path_extension};
use crate::parser::netregex::{Lazy, NetCaptures};
use crate::parser::quality::{parse_quality, parse_quality_name, ParsedQuality};

/// `SeriesTitleInfo`: the parsed series title split up.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeriesTitleInfo {
    pub title: String,
    pub title_without_year: String,
    pub year: i64,
    /// Alternate titles when the name carries several (`A (B)`, `A | B`,
    /// `A AKA B`, `A / B / S01E01`); empty otherwise.
    pub all_titles: Vec<String>,
}

/// `ParsedEpisodeInfo`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedEpisodeInfo {
    pub release_title: String,
    pub series_title: String,
    pub series_title_info: SeriesTitleInfo,
    pub quality: ParsedQuality,
    pub season_number: i64,
    pub episode_numbers: Vec<i64>,
    pub absolute_episode_numbers: Vec<i64>,
    pub special_absolute_episode_numbers: Vec<f64>,
    /// `YYYY-MM-DD` for daily releases.
    pub air_date: Option<String>,
    pub languages: Vec<i64>,
    pub full_season: bool,
    pub is_partial_season: bool,
    pub is_multi_season: bool,
    pub is_season_extra: bool,
    pub is_split_episode: bool,
    pub is_mini_series: bool,
    pub special: bool,
    pub release_group: Option<String>,
    /// Empty when the name has none, as the originals leave it.
    pub release_hash: String,
    pub season_part: i64,
    /// What follows the numbering in the release title; languages and
    /// `Special` are read from it.
    pub release_tokens: String,
    pub daily_part: Option<i64>,
}

impl ParsedEpisodeInfo {
    pub fn is_daily(&self) -> bool {
        self.air_date.as_deref().is_some_and(|d| !d.trim().is_empty())
    }

    pub fn is_absolute_numbering(&self) -> bool {
        !self.absolute_episode_numbers.is_empty()
    }

    pub fn is_possible_special_episode(&self) -> bool {
        let no_title = self.series_title.trim().is_empty();
        (!self.is_daily() && no_title && (self.episode_numbers.is_empty() || self.season_number == 0))
            || (!no_title && self.special)
            || (self.episode_numbers.len() == 1 && self.episode_numbers[0] == 0)
    }

    pub fn is_possible_scene_season_special(&self) -> bool {
        self.season_number != 0 && self.episode_numbers.len() == 1 && self.episode_numbers[0] == 0
    }

    pub fn release_type(&self) -> ReleaseType {
        if self.episode_numbers.len() > 1 || self.absolute_episode_numbers.len() > 1 {
            ReleaseType::MultiEpisode
        } else if self.episode_numbers.len() == 1 || self.absolute_episode_numbers.len() == 1 {
            ReleaseType::SingleEpisode
        } else if self.full_season {
            ReleaseType::SeasonPack
        } else {
            ReleaseType::Unknown
        }
    }
}

// --- patterns, verbatim from Parser.cs

/// Case-sensitive in the originals (no `IgnoreCase` option).
static PRE_SUBSTITUTIONS: [(Lazy, &str); 12] = [
    // Korean series without season number, replace with S01Exxx and remove airdate
    (
        net_regex!(r"\.E(\d{2,4})\.\d{6}\.(.*-NEXT)$", case_sensitive),
        ".S01E$1.$2",
    ),
    // Some Chinese anime releases contain both English and Chinese titles, remove the Chinese title and replace with normal anime pattern
    (
        net_regex!(
            r"^\[(?:(?<subgroup>[^\]]+?)(?:[一-鿌]+)?)\]\[(?<title>[^\]]+?)(?:\s(?<chinesetitle>[一-鿌][^\]]*?))\]\[(?:(?:[一-鿌]+?)?(?<episode>\d{1,4})(?:[一-鿌]+?)?)\]",
            case_sensitive
        ),
        "[${subgroup}] ${title} - ${episode} - ",
    ),
    // Chinese LoliHouse/ZERO/Lilith-Raws/Skymoon-Raws/orion origin releases don't use the expected brackets, normalize using brackets
    (
        net_regex!(
            r"^\[(?<subgroup>[^\]]*?(?:LoliHouse|ZERO|Lilith-Raws|Skymoon-Raws|orion origin)[^\]]*?)\](?<title>[^\[\]]+?)(?: - (?<episode>[0-9-]+)\s*|\[第?(?<episode>[0-9]+(?:-[0-9]+)?)话?(?:END|完)?\])\[",
            case_sensitive
        ),
        "[${subgroup}][${title}][${episode}][",
    ),
    // Most Chinese anime releases contain additional brackets/separators for chinese and non-chinese titles, remove junk first and if it has S0x as season number, convert it to Sx
    (
        net_regex!(
            r"^\[(?<subgroup>[^\]]+)\](?:\s?★[^\[ -]+\s?)?\[?(?:(?<chinesetitle>(?=[^\]]*?[一-鿌])[^\]]*?)(?:\]\[|\s*[_/·]\s*)){0,2}(?<title>[^\[\]]+?)(?:\s(?:S?(?<!\d+)((0)(?<season>\d)|(?<season>[1-9]\d))(?!\d+)))\]?(?:\[\d{4}\])?\[第?(?<episode>[0-9]+(?:-[0-9]+)?)(?:话|集)?(?: ?END|完| ?Fin)?\]",
            case_sensitive
        ),
        "[${subgroup}] ${title} S${season} - ${episode} ",
    ),
    // Some Chinese releases don't include a separation between Chinese and English titles within the same bracketed group
    (
        net_regex!(
            r"^\[(?<subgroup>[^\]]+)\]\[(?<chinesetitle>(?<![^a-zA-Z0-9])[^a-zA-Z0-9]+)(?<title>[^\]]+?)\](?:\[\d{4}\])?\[第?(?<episode>[0-9]+(?:-[0-9]+)?)(?:话|集)?(?: ?END|完| ?Fin)?\]",
            case_sensitive
        ),
        "[${subgroup}] ${title} - ${episode} ",
    ),
    // Most Chinese anime releases contain additional brackets/separators for chinese and non-chinese titles, remove junk and replace with normal anime pattern
    (
        net_regex!(
            r"^\[(?<subgroup>[^\]]+)\](?:\s?★[^\[ -]+\s?)?\[?(?:(?<chinesetitle>(?=[^\]]*?[一-鿌])[^\]]*?)(?:\]\[|\s*[_/·]\s*)){0,2}(?<title>[^\]]+?)\]?(?:\[\d{4}\])?\[第?(?<episode>[0-9]{1,4}(?:-[0-9]{1,4})?)(?:话|集)?(?: ?END|完| ?Fin)?\]",
            case_sensitive
        ),
        "[${subgroup}] ${title} - ${episode} ",
    ),
    // Some Chinese anime releases contain both Chinese and English titles, remove the Chinese title first and if it has S0x as season number, convert it to Sx
    (
        net_regex!(
            r"^\[(?<subgroup>[^\]]+)\](?:\s)(?:(?<chinesetitle>(?=[^\]]*?[一-鿌])[^\]]*?)(?:\s/\s))(?<title>[^\[\]]+?)(?:\s(?:S?(?<!\d+)((0)(?<season>\d)|(?<season>[1-9]\d))(?!\d+)))(?:[- ]+)(?<episode>[0-9]+(?:-[0-9]+)?)话?(?:END|完)?",
            case_sensitive
        ),
        "[${subgroup}] ${title} S${season} - ${episode} ",
    ),
    // Some Chinese anime releases contain both English and Chinese titles, remove the Chinese title and replace with normal anime pattern
    (
        net_regex!(
            r"^\[(?<subgroup>[^\]]+)\](?:\s)(?:(?<title>[^\]]+?)(?:\s/\s))(?<chinesetitle>(?=[^\]]*?[一-鿌])[^\]]*?)(?:[- ]+)(?<episode>[0-9]+(?:-[0-9]+)?(?![a-z]))话?(?:END|完)?",
            case_sensitive
        ),
        "[${subgroup}] ${title} - ${episode} ",
    ),
    // Some Chinese anime releases contain both Chinese and English titles, remove the Chinese title and replace with normal anime pattern
    (
        net_regex!(
            r"^\[(?<subgroup>[^\]]+)\](?:\s)(?:(?<chinesetitle>(?=[^\]]*?[一-鿌])[^\]]*?)(?:\s/\s))(?<title>[^\]]+?)(?:[- ]+)(?<episode>[0-9]+(?:-[0-9]+)?(?![a-z]))话?(?:END|完)?",
            case_sensitive
        ),
        "[${subgroup}] ${title} - ${episode} ",
    ),
    // GM-Team releases with lots of square brackets
    (
        net_regex!(
            r"^\[(?<subgroup>[^\]]+)\](?:(?<chinesubgroup>\[(?=[^\]]*?[一-鿌])[^\]]*\])+)\[(?<title>[^\]]+?)\](?<junk>\[[^\]]+\])*\[(?<episode>[0-9]+(?:-[0-9]+)?)( END| Fin)?\]",
            case_sensitive
        ),
        "[${subgroup}] ${title} - ${episode} ",
    ),
    // Some Chinese anime releases contain both Chinese and English titles separated by | instead of /, remove the Chinese title and replace with normal anime pattern
    (
        net_regex!(
            r"^\[(?<subgroup>[^\]]+)\](?:\s)(?:(?<chinesetitle>(?=[^\]]*?[一-鿌])[^\]]*?)(?:\s\|\s))(?<title>[^\]]+?)(?:[- ]+)(?<episode>[0-9]+(?:-[0-9]+)?(?![a-z]))话?(?:END|完)?",
            case_sensitive
        ),
        "[${subgroup}] ${title} - ${episode} ",
    ),
    // Spanish releases with information in brackets
    (
        net_regex!(
            r"^(?<title>.+?(?=[ ._-]\()).+?\((?<year>\d{4})\/(?<info>S[^\/]+)",
            case_sensitive
        ),
        "${title} (${year}) - ${info} ",
    ),
];

static REPORT_TITLE: [Lazy; 98] = [
    // Daily episode with year in series title and air time after date (Plex DVR format)
    net_regex!(
        r"^^(?<title>.+?\((?<titleyear>\d{4})\))[-_. ]+(?<airyear>19[4-9]\d|20\d\d)(?<sep>[-_]?)(?<airmonth>0\d|1[0-2])\k<sep>(?<airday>[0-2]\d|3[01])[-_. ]\d{2}[-_. ]\d{2}[-_. ]\d{2}"
    ),
    // Daily episodes without title (2018-10-12, 20181012) (Strict pattern to avoid false matches)
    net_regex!(
        r"^(?<airyear>19[6-9]\d|20\d\d)(?<sep>[-_]?)(?<airmonth>0\d|1[0-2])\k<sep>(?<airday>[0-2]\d|3[01])(?!\d)"
    ),
    // Multi-Part episodes without a title (S01E05.S01E06)
    net_regex!(
        r"^(?:\W*S(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:e{1,2}(?<episode>\d{1,3}(?!\d+)))+){2,}"
    ),
    // Multi-Part episodes without a title (1x05.1x06)
    net_regex!(
        r"^(?:\W*(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:x{1,2}(?<episode>\d{1,3}(?!\d+)))+){2,}"
    ),
    // Episodes without a title, Multi (S01E04E05, 1x04x05, etc)
    net_regex!(
        r"^(?:S?(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:[-_]|[ex]){1,2}(?<episode>\d{2,3}(?!\d+))){2,})"
    ),
    // Split episodes (S01E05a, S01E05b, etc)
    net_regex!(
        r"^(?<title>.+?)(?:S?(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:[-_ ]?[ex])(?<episode>\d{2,3}(?!\d+))(?<splitepisode>[a-d])(?:[ _.])))"
    ),
    // Episodes without a title, Single (S01E05, 1x05)
    net_regex!(
        r"^(?:S?(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:[-_ ]?[ex])(?<episode>\d{2,3}(?!\d+))))"
    ),
    // Anime - [SubGroup] Title Absolute (Season+Episode)
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+))(?:[-_. ])+\((?:S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+))))(?:v\d+)?(?:\)(?!\d+)).*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - [SubGroup] Title Absolute - Season+Episode
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+))(?:[-_. ](?<![()\[!]))+(?:S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+))))(?:v\d+)?(?:[_. ](?!\d+)).*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - [SubGroup] Title Season+Episode
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)(?:[-_\W](?<![()\[!]))+(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+)))+)(?:v\d+)?(?:[_. ](?!\d+)).*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - [SubGroup] Title Episode Absolute Episode Number ([SubGroup] Series Title Episode 01)
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>.+?)[-_. ]+?(?:Episode)(?:[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+)))+.*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - [SubGroup] Title Absolute Episode Number + Season+Episode
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+(?<absoluteepisode>\d{2,3}(\.\d{1,2})?))+(?:_|-|\s|\.)+(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:\-|[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+)))+).*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.)"
    ),
    // Anime - [SubGroup] Title Season+Episode + Absolute Episode Number
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)(?:[-_\W](?<![()\[!]))+(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:\-|[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+)))+)(?:(?:_|-|\s|\.)+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+|\-[a-z])))+.*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - [SubGroup] Title with trailing number Absolute Episode Number - Batch separated with tilde
    net_regex!(
        r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?[^-]+?)(?:(?<![-_. ]|\b[0]\d+) - )[-_. ]?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))\s?~\s?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)"
    ),
    // Anime - [SubGroup] Title with season number in brackets Absolute Episode Number
    net_regex!(
        r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>[^-]+?)[_. ]+?\(Season[_. ](?<season>\d+)\)[-_. ]+?(?:[-_. ]?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+)))+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)"
    ),
    // Anime - [SubGroup] Title with trailing 3-digit number and sub title - Absolute Episode Number
    net_regex!(
        r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>[^]]+?)(?:[-_. ]{3}?(?<absoluteepisode>\d{2}(\.\d{1,2})?(?!-?\d+|-[a-z]+)))+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)"
    ),
    // Anime - [SubGroup] Title with trailing number Absolute Episode Number
    net_regex!(
        r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>[^-]+?)(?:(?<![-_. ]|\b[0]\d+) - )(?:[-_. ]?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+)))+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)"
    ),
    // Anime - [SubGroup] Title with trailing number S## (Full season)
    net_regex!(
        r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?)[-_. ]+(?:S(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?![ex]?\d+))).+?(?:$|\.mkv)"
    ),
    // Anime - [SubGroup] Title with trailing number Absolute Episode Number
    net_regex!(
        r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>[^-]+?)(?:(?<![-_. ]|\b[0]\d+)[_ ]+)(?:[-_. ]?(?<absoluteepisode>\d{3}(\.\d{1,2})?(?!\d+|-[a-z]+)))+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)"
    ),
    // Anime - [SubGroup] Title - Absolute Episode Number
    net_regex!(
        r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?)(?:(?<!\b[0]\d+))(?:[. ]-[. ](?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+|[-])))+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)"
    ),
    // Anime - [SubGroup] Title Absolute Episode Number - Absolute Episode Number (batches without full separator between title and absolute episode numbers)
    net_regex!(
        r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?)(?:(?<!\b[0]\d+))(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+|[-]))[. ]-[. ](?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+|[-]))(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)"
    ),
    // Anime - [SubGroup] Title Absolute Episode Number
    net_regex!(
        r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?)[-_. ]+\(?(?:[-_. ]?#?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+|-[a-z]+)))+\)?(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)"
    ),
    // Multi-episode Repeated (S01E05 - S01E06)
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:e|[-_. ]e){1,2}(?<episode>\d{1,3}(?!\d+)))+){2,}"
    ),
    // Multi-episode Repeated (1x05 - 1x06)
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:x{1,2}(?<episode>\d{1,3}(?!\d+)))+){2,}"
    ),
    // Single episodes with a title (S01E05, 1x05, etc) followed by ".5 [SP]"
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[ex]|\W[ex]|_){1,2}(?<episode>\d{2,3}(?!\d+|(?:[ex]|\W[ex]|_|-){1,2}\d+))(?<special>\.5[ .]\[SP\]))"
    ),
    // Single episodes with a title (S01E05, 1x05, etc) and trailing info in slashes
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[ex]|\W[ex]|_){1,2}(?<episode>\d{2,3}(?!\d+|(?:[ex]|\W[ex]|_|-){1,2}\d+))).+?(?:\[.+?\])(?!\\)"
    ),
    // Anime - Title Season EpisodeNumber + Absolute Episode Number [SubGroup]
    net_regex!(
        r"^(?<title>.+?)(?:[-_\W](?<![()\[!]))+(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:[ex]|\W[ex]|-){1,2}(?<episode>(?<!\d+)\d{2}(?!\d+)))+)[-_. (]+?(?:[-_. ]?(?<absoluteepisode>(?<!\d+)\d{3}(\.\d{1,2})?(?!\d+|[pi])))+.+?\[(?<subgroup>.+?)\](?:$|\.mkv)"
    ),
    // Multi-Episode with a title (S01E05E06, S01E05-06, S01E05 E06, etc) and trailing info in slashes
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[ex]|\W[ex]|_){1,2}(?<episode>\d{2,3}(?!\d+))(?:(?:\-|[ex]|\W[ex]|_){1,2}(?<episode>\d{2,3}(?!\d+)))+).+?(?:\[.+?\])(?!\\)"
    ),
    // Anime - Title Absolute Episode Number [SubGroup] [Hash]? (Series Title Episode 99-100 [RlsGroup] [ABCD1234])
    net_regex!(
        r"^(?<title>.+?)[-_. ]Episode(?:[-_. ]+(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+)))+(?:.+?)\[(?<subgroup>.+?)\].*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - Title Absolute Episode Number [SubGroup] [Hash]
    net_regex!(
        r"^(?<title>.+?)(?:(?:_|-|\s|\.)+(?<absoluteepisode>\d{3}(\.\d{1,2})(?!\d+)))+(?:.+?)\[(?<subgroup>.+?)\].*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - Title Absolute Episode Number (Year) [SubGroup]
    net_regex!(
        r"^(?<title>.+?)[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2}(?!\d+))[-_. ](\(\d{4}\))[-_. ]\[(?<subgroup>.+?)\]"
    ),
    // Anime - Title with trailing number, Absolute Episode Number and hash
    net_regex!(
        r"^(?<title>[^-]+?)(?:(?<![-_. ]|\b[0]\d+) - )(?:[-_. ]?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+)))+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])(?:$|\.mkv)"
    ),
    // Anime - Title Absolute Episode Number [Hash]
    net_regex!(
        r"^(?<title>.+?)(?:(?:_|-|\s|\.)+(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+)))+(?:[-_. ]+(?<special>special|ova|ovd))?[-_. ]+.*?(?<hash>[(\[]\w{8}[)\]])$"
    ),
    // Episodes with airdate AND season/episode number, capture season/episode only
    net_regex!(
        r"^(?<title>.+?)?\W*(?<airdate>\d{4}\W+[0-1][0-9]\W+[0-3][0-9])(?!\W+[0-3][0-9])[-_. ](?:s?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+)))(?:[ex](?<episode>(?<!\d+)(?:\d{1,3})(?!\d+)))"
    ),
    // Episodes with airdate AND season/episode number
    net_regex!(
        r"^(?<title>.+?)?\W*(?<airyear>\d{4})\W+(?<airmonth>[0-1][0-9])\W+(?<airday>[0-3][0-9])(?!\W+[0-3][0-9]).+?(?:s?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+)))(?:[ex](?<episode>(?<!\d+)(?:\d{1,3})(?!\d+)))"
    ),
    // Episodes with absolute episode number AND airdate (TJET Wrestling)
    net_regex!(
        r"^(?<title>.+?)?[-_. ](?:e\d{2,3}(?!\d+))[-_. ](?<airyear>\d{4})\W+(?<airmonth>[0-1][0-9])\W+(?<airday>[0-3][0-9])(?!\W+[0-3][0-9]).+?(?:\[[a-z]+\])"
    ),
    // Single or multi episode releases with multiple titles, then season and episode numbers after the last title. (Title1 / Title2 / ... / S1E1-2 of 6)
    net_regex!(
        r"^((?<title>.*?)[ ._]\/[ ._])+\(?S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:\W|_)?E?[ ._]?(?<episode>(?<!\d+)\d{1,2}(?!\d+))(?:-(?<episode>(?<!\d+)\d{1,2}(?!\d+)))?(?:[ ._]of[ ._](?<episodecount>\d{1,2}))?\)?[ ._][\(\[]"
    ),
    // Multi-episode with title (S01E99-100, S01E05-06)
    net_regex!(
        r"^(?<title>.+?)(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))E(?<episode>\d{2,3}(?!\d+))(?:-(?<episode>\d{2,3}(?!\d+)))+(?:[-_. ]|$)"
    ),
    // Multi-episode with title (S01E05-06, S01E05-6)
    net_regex!(
        r"^(?<title>.+?)(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))E(?<episode>\d{1,2}(?!\d+))(?:-(?<episode>\d{1,2}(?!\d+)))+(?:[-_. ]|$)"
    ),
    // Episodes with a title, Single episodes (S01E05, 1x05, etc) & Multi-episode (S01E05E06, S01E05-06, S01E05 E06, etc)
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[ex]|\W[ex]){1,2}(?<episode>\d{2,3}(?!\d+))(?:(?:\-|[ex]|\W[ex]|_){1,2}(?<episode>\d{2,3}(?!\d+)))*)(?:[-_. ]|$)"
    ),
    // Episodes with a title, 4 digit season number, Single episodes (S2016E05, etc) & Multi-episode (S2016E05E06, S2016E05-06, S2016E05 E06, etc)
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{4})(?!\d+))(?:e|\We|_){1,2}(?<episode>\d{2,4}(?!\d+))(?:(?:\-|e|\We|_){1,2}(?<episode>\d{2,3}(?!\d+)))*)\W?(?!\\)"
    ),
    // Episodes with a title, 4 digit season number, Single episodes (2016x05, etc) & Multi-episode (2016x05x06, 2016x05-06, 2016x05 x06, etc)
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+(?<season>(?<!\d+)(?:\d{4})(?!\d+))(?:x|\Wx){1,2}(?<episode>\d{2,4}(?!\d+))(?:(?:\-|x|\Wx|_){1,2}(?<episode>\d{2,3}(?!\d+)))*)\W?(?!\\)"
    ),
    // Episodes with a title, Single episodes (s01.05)
    net_regex!(
        r"^(?<title>.+?)(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{2})(?!\d+))(?:\.)(?<episode>\d{2,3}(?!\d+))(?:[-_. ]|$)"
    ),
    // Multi-season pack
    net_regex!(
        r"^(?<title>.+?)(Complete Series)?[-_. ]+(?:S|(?:Season|Saison|Series|Stagione)[_. ])(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[-_. ]{1}|[-_. ]{3})(?:S|(?:Season|Saison|Series|Stagione)[_. ])?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))"
    ),
    // Partial season pack
    net_regex!(
        r"^(?<title>.+?)(?:\W+S(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))\W+(?:(?:(?:Part|Vol)\W?|(?<!\d+\W+)e|p)(?<seasonpart>\d{1,2}(?!\d+)))+)"
    ),
    // Anime - 4 digit absolute episode number
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>.+?)[-_. ]+?(?<absoluteepisode>\d{4}(\.\d{1,2})?(?!\d+))"
    ),
    // Anime - Title 4-digit Absolute Episode Number [SubGroup]
    net_regex!(r"^(?<title>.+?)[-_. ]+(?<absoluteepisode>(?<!\d+)\d{4}(?!\d+))[-_. ]\[(?<subgroup>.+?)\]"),
    // Mini-Series with year in title, treated as season 1, episodes are labelled as Part01, Part 01, Part.1
    net_regex!(r"^(?<title>.+?\d{4})(?:\W+(?:(?:Part\W?|e)(?<episode>\d{1,2}(?!\d+)))+)"),
    // Mini-Series, treated as season 1, multi episodes are labelled as E1-E2
    net_regex!(
        r"^(?<title>.+?)(?:[-._ ][e])(?<episode>\d{2,3}(?!\d+))(?:(?:\-?[e])(?<episode>\d{2,3}(?!\d+)))+"
    ),
    // Episodes with airdate and part (2018.04.28.Part.2)
    net_regex!(
        r"^(?<title>.+?)?\W*(?<airyear>\d{4})[-_. ]+(?<airmonth>[0-1][0-9])[-_. ]+(?<airday>[0-3][0-9])(?![-_. ]+[0-3][0-9])[-_. ]+Part[-_. ]?(?<part>[1-9])"
    ),
    // Mini-Series, treated as season 1, episodes are labelled as Part01, Part 01, Part.1
    net_regex!(r"^(?<title>.+?)(?:\W+(?:(?:(?<!\()Part\W?|(?<!\d+\W+)e)(?<episode>\d{1,2}(?!\d+|\))))+)"),
    // Mini-Series, treated as season 1, episodes are labelled as Part One/Two/Three/...Nine, Part.One, Part_One
    net_regex!(
        r"^(?<title>.+?)(?:\W+(?:Part[-._ ](?<episode>One|Two|Three|Four|Five|Six|Seven|Eight|Nine)(?>[-._ ])))"
    ),
    // Mini-Series, treated as season 1, episodes are labelled as XofY
    net_regex!(r"^(?<title>.+?)(?:\W+(?:(?<episode>(?<!\d+)\d{1,2}(?!\d+))of\d+)+)"),
    // Supports Season 01 Episode 03
    net_regex!(
        r#"(?:.*(?:\"|^))(?<title>.*?)(?:[-_\W](?<![()\[]))+(?:\W?Season\W?)(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:\W|_)+(?:Episode\W)(?:[-_. ]?(?<episode>(?<!\d+)\d{1,2}(?!\d+)))+"#
    ),
    // Multi-episode with episodes in square brackets (Series Title [S01E11E12] or Series Title [S01E11-12])
    net_regex!(
        r"(?:.*(?:^))(?<title>.*?)[-._ ]+\[S(?<season>(?<!\d+)\d{2}(?!\d+))(?:[E-]{1,2}(?<episode>(?<!\d+)\d{2}(?!\d+)))+\]"
    ),
    // Multi-episode with episodes in brackets (Series Title (S01E11E12) or Series Title (S01E11-12))
    net_regex!(
        r"(?:.*(?:^))(?<title>.*?)[-._ ]+\(S(?<season>(?<!\d+)\d{2}(?!\d+))(?:[E-]{1,2}(?<episode>(?<!\d+)\d{2}(?!\d+)))+\)"
    ),
    // Multi-episode release with no space between series title and season (S01E11E12)
    net_regex!(
        r"(?:.*(?:^))(?<title>.*?)S(?<season>(?<!\d+)\d{2}(?!\d+))(?:E(?<episode>(?<!\d+)\d{2}(?!\d+)))+"
    ),
    // Multi-episode with single episode numbers (S6.E1-E2, S6.E1E2, S6E1E2, etc)
    net_regex!(
        r"^(?<title>.+?)[-_. ]S(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:[-_. ]?[ex]?(?<episode>(?<!\d+)\d{1,2}(?!\d+)))+"
    ),
    // Single or multi episode releases with multiple titles, each followed by season and episode numbers in brackets
    net_regex!(
        r"^(?<title>.*?)[ ._]\(S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:\W|_)?E?[ ._]?(?<episode>(?<!\d+)\d{1,2}(?!\d+))(?:-(?<episode>(?<!\d+)\d{1,2}(?!\d+)))?\)(?:[ ._]\/[ ._])(?<title>.*?)[ ._]\("
    ),
    // Single episode season or episode S1E1 or S1-E1 or S1.Ep1 or S01.Ep.01
    net_regex!(
        r#"(?:.*(?:\"|^))(?<title>.*?)(?:\W?|_)S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:\W|_)?Ep?[ ._]?(?<episode>(?<!\d+)\d{1,2}(?!\d+))"#
    ),
    // 3 digit season S010E05
    net_regex!(
        r#"(?:.*(?:\"|^))(?<title>.*?)(?:\W?|_)S(?<season>(?<!\d+)\d{3}(?!\d+))(?:\W|_)?E(?<episode>(?<!\d+)\d{1,2}(?!\d+))"#
    ),
    // 5 digit episode number with a title
    net_regex!(
        r"^(?:(?<title>.+?)(?:_|-|\s|\.)+)(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+)))(?:(?:\-|[ex]|\W[ex]|_){1,2}(?<episode>(?<!\d+)\d{5}(?!\d+)))"
    ),
    // 5 digit multi-episode with a title
    net_regex!(
        r"^(?:(?<title>.+?)(?:_|-|\s|\.)+)(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+)))(?:(?:[-_. ]{1,3}ep){1,2}(?<episode>(?<!\d+)\d{5}(?!\d+)))+"
    ),
    // Separated season and episode numbers S01 - E01
    net_regex!(
        r"^(?<title>.+?)(?:_|-|\s|\.)+S(?<season>\d{2}(?!\d+))(\W-\W)E(?<episode>(?<!\d+)\d{2}(?!\d+))(?!\\)"
    ),
    // Season and episode numbers in square brackets (single and multi-episode)
    net_regex!(
        r"^(?<title>.+?)?(?:[-_\W](?<![()\[!]))+\[(?:s)?(?<season>(?<!\d+)\d{1,2})(?:(?:[ex])(?<episode>\d{2}))(?:(?:[-ex]){1,2}(?<episode>\d{2}))*\].+?(?:\.|$)"
    ),
    // Anime - Title with season number - Absolute Episode Number (Title S01 - EP14)
    net_regex!(r"^(?<title>.+?S\d{1,2})[-_. ]{3,}(?:EP)?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+|[-]))"),
    // Anime - French titles with single episode numbers, with or without leading sub group ([RlsGroup] Title - Episode 1)
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)[-_. ]+?(?:Episode[-_. ]+?)(?<absoluteepisode>\d{1}(\.\d{1,2})?(?!\d+))"
    ),
    // Anime - Absolute episode number in square brackets
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>.+?)[-_. ]+?\[(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))\]"
    ),
    // Japanese variety shows with leading date
    net_regex!(
        r"^(?<airyear>\d{2})(?<airmonth>[0-1][0-9])(?<airday>[0-3][0-9])(?![-_. ]+[0-3][0-9])[-_. ](?<title>.+?)[-_. ](?:Season[-_. ]?(?<season>\d{1,2})[-_. ])?(?:ep|#)(?<episode>\d{2,3})"
    ),
    // Season only releases followed by year
    net_regex!(
        r"^(?<title>.+?)[-_. ]+?(?:S|Season|Saison|Series|Stagione)[-_. ]?(?<season>\d{1,2}(?=[-_. ]\d{4}[-_. ]+))(?<extras>EXTRAS|SUBPACK)?(?!\\)"
    ),
    // Season only releases
    net_regex!(
        r"^(?<title>.+?)[-_. ]+?(?:S|Season|Saison|Series|Stagione)[-_. ]?(?<season>\d{1,2}(?![-_. ]?\d+))(?:[-_. ]|$)+(?<extras>EXTRAS|SUBPACK)?(?!\\)"
    ),
    // 4 digit season only releases
    net_regex!(
        r"^(?<title>.+?)[-_. ]+?(?:S|Season|Saison|Series|Stagione)[-_. ]?(?<season>\d{4}(?![-_. ]?\d+))(\W+|_|$)(?<extras>EXTRAS|SUBPACK)?(?!\\)"
    ),
    // Spanish tracker releases
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_. ]+?Temporada.+?|\[.+?\])\[Cap)(?:[-_. ]+(?<season>(?<!\d+)\d{1,2})(?<episode>(?<!e|x)(?:[1-9][0-9]|[0][1-9])))+(?:\])"
    ),
    // Supports 103/113 naming
    net_regex!(
        r"^(?<title>.+?)?(?:(?:[_.-](?<![()\[!]))+(?<season>(?<!\d+)[1-9])(?<episode>[1-9][0-9]|[0][1-9])(?![a-z]|\d+))+(?:[_.]|$)"
    ),
    // 4 digit episode number
    // Episodes without a title, Single (S01E05, 1x05) AND Multi (S01E04E05, 1x04x05, etc)
    net_regex!(
        r"^(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:\-|[ex]|\W[ex]|_){1,2}(?<episode>\d{4}(?!\d+|i|p)))+)(\W+|_|$)(?!\\)"
    ),
    // 4 digit episode number
    // Episodes with a title, Single episodes (S01E05, 1x05, etc) & Multi-episode (S01E05E06, S01E05-06, S01E05 E06, etc)
    net_regex!(
        r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]|\d{1,2}-))+S?(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:\-|[ex]|\W[ex]|_){1,2}(?<episode>\d{4}(?!\d+|i|p)))+)\W?(?!\\)"
    ),
    // Episodes with airdate (2018.04.28)
    net_regex!(
        r"^(?<title>.+?)?\W*(?<airyear>\d{4})[-_. ]+(?<airmonth>[0-1][0-9])[-_. ]+(?<airday>[0-3][0-9])(?![-_. ]+[0-3][0-9])"
    ),
    // Turkish tracker releases (01 BLM, 3. Blm, 04.Bolum, etc)
    net_regex!(r"^(?<title>.+?)[_. ](?<absoluteepisode>\d{1,4})(?:[_. ]+)(?:BLM|B[oö]l[uü]m)"),
    // Episodes with airdate (04.28.2018)
    net_regex!(
        r"^(?<title>.+?)?\W*(?<ambiguousairmonth>[0-1][0-9])[-_. ]+(?<ambiguousairday>[0-3][0-9])[-_. ]+(?<airyear>\d{4})(?!\d+)"
    ),
    // Episodes with airdate (28.04.2018)
    net_regex!(
        r"^(?<title>.+?)?\W*(?<ambiguousairday>[0-3][0-9])[-_. ]+(?<ambiguousairmonth>[0-1][0-9])[-_. ]+(?<airyear>\d{4})(?!\d+)"
    ),
    // Episodes with airdate (20180428)
    net_regex!(
        r"^(?<title>.+?)?\W*(?<!\d+)(?<airyear>\d{4})(?<airmonth>[0-1][0-9])(?<airday>[0-3][0-9])(?!\d+)"
    ),
    // Anime - Title Absolute Episode Number (E195 or E1206)
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)(?:(?:_|-|\s|\.)+(?:e|ep)(?<absoluteepisode>(\d{3}|\d{4})(\.\d{1,2})?))+[-_. ].*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Supports 1103/1113 naming
    net_regex!(
        r"^(?<title>.+?)?(?:(?:[-_. ](?<![()\[!]))*(?<!\d{1,2}-)(?<season>(?<!\d+|\(|\[|e|x)\d{2})(?<episode>(?<!e|x)(?:[1-9][0-9]|[0][1-9])(?!p|i|\d+|\)|\]|\W\d+|\W(?:e|ep|x)\d+)))+([-_. ]+|$)(?!\\)"
    ),
    // Dutch/Flemish release titles
    net_regex!(
        r"^(?<title>.+?)[-_. ](?:Se\.(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:[-_ ]?afl\.)(?<episode>\d{1,3}(?!\d+))(?:(?:[-]|[-_ ]en[-_ ])(?<episode>\d{1,3}(?!\d+)))*))"
    ),
    // Episodes with single digit episode number (S01E1, S01E5E6, etc)
    net_regex!(
        r"^(?<title>.*?)(?:(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:\-|[ex]){1,2}(?<episode>\d{1}))+)+(\W+|_|$)(?!\\)"
    ),
    // iTunes Season 1\05 Title (Quality).ext
    net_regex!(
        r"^(?:Season(?:_|-|\s|\.)(?<season>(?<!\d+)\d{1,2}(?!\d+)))(?:_|-|\s|\.)(?<episode>(?<!\d+)\d{1,2}(?!\d+))"
    ),
    // iTunes 1-05 Title (Quality).ext
    net_regex!(r"^(?:(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:-(?<episode>\d{2,3}(?!\d+))))"),
    // Anime Range - Title Absolute Episode Number (ep01-12)
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)(?:_|\s|\.)+(?:e|ep)(?<absoluteepisode>\d{2,3}(\.\d{1,2})?)-(?<absoluteepisode>(?<!\d+)\d{1,2}(\.\d{1,2})?(?!\d+|-)).*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - Title Absolute Episode Number (e66)
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)(?:(?:_|-|\s|\.)+(?:e|ep)(?<absoluteepisode>\d{2,4}(\.\d{1,2})?))+[-_. ].*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - Title Episode Absolute Episode Number (Series Title Episode 01)
    net_regex!(
        r"^(?<title>.+?)[-_. ](?:Episode)(?:[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+)))+.*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime Range - Title Absolute Episode Number (1 or 2 digit absolute episode numbers in a range, 1-10)
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)[_. ]+(?<absoluteepisode>(?<!\d+)\d{1,2}(\.\d{1,2})?(?!\d+))-(?<absoluteepisode>(?<!\d+)\d{1,2}(\.\d{1,2})?(?!\d+|-)).*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - Title Episode/Episodio Absolute Episode Number
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)[-_. ]+(?:Episode|Episodio)(?:[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,4}(\.\d{1,2})?(?!\d+|[ip])))+.*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - Title [Absolute Episode Number] from AniLibriaTV
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)(?:[-_. ]\[)(?:(?:-?)(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+|[ip])))+(?:\][-_. ]).*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - Title Absolute Episode Number
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)(?:[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,4}(\.\d{1,2})?(?!\d+|[ip])))+.*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Anime - Title {Absolute Episode Number}
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+|[ip])))+.*?(?<hash>[(\[]\w{8}[)\]])?$"
    ),
    // Extant, terrible multi-episode naming (extant.10708.hdtv-lol.mp4)
    net_regex!(r"^(?<title>.+?)[-_. ](?<season>[0]?\d?)(?:(?<episode>\d{2}){2}(?!\d+))[-_. ]"),
    // Season only releases for poorly named anime
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ])?(?<title>.+?)[-_. ]+?[\[(](?:S|Season|Saison|Series|Stagione)[-_. ]?(?<season>\d{1,2}(?![-_. ]?\d+))(?:[-_. )\]]|$)+(?<extras>EXTRAS|SUBPACK)?(?!\\)"
    ),
    // Episodes without a title, Single episode numbers (S1E1, 1x1)
    net_regex!(
        r"^(?:S?(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:[-_ ]?[ex])(?<episode>\d{1}(?!\d+))))"
    ),
];

static SPECIAL_EPISODE_TITLE: [Lazy; 2] = [
    net_regex!(r"\.S\d+E00\.(?<episodetitle>.+?)(?:\.(?:720p|1080p|2160p|HDTV|WEB|WEBRip|WEB-DL)\.|$)"),
    net_regex!(r"\.S\d+\.Special\.(?<episodetitle>.+?)(?:\.(?:720p|1080p|2160p|HDTV|WEB|WEBRip|WEB-DL)\.|$)"),
];

static REJECT_HASHED_RELEASES: [Lazy; 14] = [
    // Generic match for md5 and mixed-case hashes.
    net_regex!(r"^[0-9a-zA-Z]{32}", case_sensitive),
    // Generic match for shorter lower-case hashes.
    net_regex!(r"^[a-z0-9]{24}$", case_sensitive),
    // Format seen on some NZBGeek releases
    net_regex!(r"^[A-Z]{11}\d{3}$", case_sensitive),
    net_regex!(r"^[a-z]{12}\d{3}$", case_sensitive),
    // Backup filename (Unknown origins)
    net_regex!(r"^Backup_\d{5,}S\d{2}-\d{2}$", case_sensitive),
    // 123 - Started appearing December 2014
    net_regex!(r"^123$", case_sensitive),
    // abc - Started appearing January 2015
    net_regex!(r"^abc$"),
    // abc - Started appearing 2020
    net_regex!(r"^abc[-_. ]xyz"),
    // b00bs - Started appearing January 2015
    net_regex!(r"^b00bs$"),
    // 170424_26 - Started appearing August 2018
    net_regex!(r"^\d{6}_\d{2}$", case_sensitive),
    // additional Generic match for mixed-case hashes.
    net_regex!(r"^[0-9a-zA-Z]{30}", case_sensitive),
    net_regex!(r"^[0-9a-zA-Z]{26}", case_sensitive),
    net_regex!(r"^[0-9a-zA-Z]{39}", case_sensitive),
    net_regex!(r"^[0-9a-zA-Z]{24}", case_sensitive),
];

static SEASON_FOLDER_REJECT_RE: Lazy = net_regex!(r"^(Season[ ._-]*\d+|Specials)$", case_sensitive);
static REVERSED_TITLE_RE: Lazy = net_regex!(r"(?:^|[-._ ])(p027|p0801|\d{2,3}E\d{2}S)[-._ ]", case_sensitive);
static NORMALIZE_RE: Lazy = net_regex!(r"((?:\b|_)(?<!^)([aà](?!$)|an|the|and|or|of)(?!$)(?:\b|_))|\W|_");
static PERCENT_RE: Lazy = net_regex!(r"(?<=\b\d+)%", case_sensitive);
static FILE_EXTENSION_RE: Lazy = net_regex!(r"\.[a-z0-9]{2,4}$");
static SIMPLE_TITLE_RE: Lazy = net_regex!(
    r"(?:(480|540|576|720|1080|2160)[ip]|[xh][\W_]?26[45]|DD\W?5\W1|[<>?*]|848x480|1280x720|1920x1080|3840x2160|4096x2160|(?<![a-f0-9])(8|10)(b(?![a-z0-9])|bit)|10-bit)\s*?"
);
static WEBSITE_PREFIX_RE: Lazy = net_regex!(
    r"^(?:(?:\[|\()\s*)?(?:www\.)?[-a-z0-9-]{1,256}\.(?<!Naruto-Kun\.)(?:[a-z]{2,6}\.[a-z]{2,6}|xn--[a-z0-9-]{4,}|[a-z]{2,})\b(?:\s*(?:\]|\))|[ -]{2,})[ -]*"
);
static WEBSITE_POSTFIX_RE: Lazy =
    net_regex!(r"(?:\[\s*)?(?:www\.)?[-a-z0-9-]{1,256}\.(?:xn--[a-z0-9-]{4,}|[a-z]{2,6})\b(?:\s*\])$");
static SIX_DIGIT_AIR_DATE_RE: Lazy = net_regex!(
    r"(?<=[_.-])(?<airdate>(?<!\d)(?<airyear>[1-9]\d{1})(?<airmonth>[0-1][0-9])(?<airday>[0-3][0-9]))(?=[_.-])"
);
static CLEAN_RELEASE_GROUP_RE: Lazy = net_regex!(
    r"^(.*?[-._ ](S\d+E\d+)[-._ ])|(-(RP|1|NZBGeek|Obfuscated|Scrambled|sample|Pre|postbot|xpost|Rakuv[a-z0-9]*|WhiteRev|BUYMORE|AsRequested|AlternativeToRequested|GEROV|Z0iDS3N|Chamele0n|4P|4Planet|AlteZachen|RePACKPOST))+$"
);
static CLEAN_TORRENT_SUFFIX_RE: Lazy = net_regex!(r"\[(?:ettv|rartv|rarbg|cttv|publichd)\]$");
static CLEAN_QUALITY_BRACKETS_RE: Lazy = net_regex!(r"\[[a-z0-9 ._-]+\]$");
static RELEASE_GROUP_RE: Lazy = net_regex!(
    r"-(?<releasegroup>[a-z0-9]+(?<part2>-[a-z0-9]+)?(?!.+?(?:480p|576p|720p|1080p|2160p)))(?<!(?:WEB-DL|Blu-Ray|480p|576p|720p|1080p|2160p|DTS-HD|DTS-X|DTS-MA|DTS-ES|-ES|-EN|-CAT|[ ._]\d{4}-\d{2}|-\d{2})(?:\k<part2>)?)(?:\b|[-._ ]|$)|[-._ ]\[(?<releasegroup>[a-z0-9]+)\]$"
);
static INVALID_RELEASE_GROUP_RE: Lazy = net_regex!(r"^([se]\d+|[0-9a-f]{8})$");
static ANIME_RELEASE_GROUP_RE: Lazy = net_regex!(r"^(?:\[(?<subgroup>(?!\s).+?(?<!\s))\](?:_|-|\s|\.)?)");
static EXCEPTION_RELEASE_GROUP_EXACT_RE: Lazy = net_regex!(
    r"(?<releasegroup>(?:D\-Z0N3|Fight-BB|VARYG|E\.N\.D|KRaLiMaRKo|BluDragon|DarQ|KCRT|BEN[_. ]THE[_. ]MEN)\b)"
);
static EXCEPTION_RELEASE_GROUP_RE: Lazy = net_regex!(
    r"(?<=[._ \[])(?<releasegroup>(Silence|afm72|Panda|Ghost|MONOLITH|Tigole|Joy|ImE|UTR|t3nzin|Anime Time|Project Angel|Hakata Ramen|HONE|Vyndros|SEV|Garshasp|Kappa|Natty|RCVR|SAMPA|YOGI|r00t|EDGE2020|RZeroX)(?=\]|\)))"
);
static YEAR_IN_TITLE_RE: Lazy = net_regex!(r"^(?<title>.+?)[-_. ]+?[\(\[]?(?<year>\d{4})[\]\)]?");
static TITLE_COMPONENTS_RE: Lazy = net_regex!(
    r"^(?:(?<title>.+?) \((?<title>.+?)\)|(?<title>.+?) \| (?<title>.+?)|(?<title>.+?) AKA (?<title>.+?))$"
);
static PART_RE: Lazy = net_regex!(r"\(\d+\)$", case_sensitive);
static PUNCTUATION_RE: Lazy = net_regex!(r"[^\w\s]", case_sensitive);
static ARTICLE_WORD_RE: Lazy = net_regex!(r"^(a|an|the)\s");
static DUPLICATE_SPACES_RE: Lazy = net_regex!(r"\s{2,}", case_sensitive);
static SEASON_FOLDER_RE: Lazy = net_regex!(
    r"^(?:S|Season|Saison|Series|Stagione)[-_. ]*(?<season>(?<!\d+)\d{1,4}(?!\d+))(?:[_. ]+(?!\d+)|$)"
);
static SIMPLE_EPISODE_NUMBER_RE: Lazy = net_regex!(
    r"^[ex]?(?<episode>(?<!\d+)\d{1,3}(?!\d+))(?:[ex-](?<episode>(?<!\d+)\d{1,3}(?!\d+)))?(?:[_. ](?!\d+)(?<remaining>.+)|$)"
);
static REQUEST_INFO_RE: Lazy = net_regex!(r"^(?:\[.+?\])+", case_sensitive);

const NUMBERS: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

// --- helpers

/// Length in UTF-16 code units, which is what .NET string indices count.
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Text from UTF-16 index `n` on; `None` when `n` is past the end.
fn utf16_from(s: &str, n: usize) -> Option<&str> {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= n {
            return Some(&s[i..]);
        }
        units += c.len_utf16();
    }
    if units >= n && n == units {
        Some("")
    } else {
        None
    }
}

/// `ConvertToNumerals` over an NFKC-normalised string: every numeric
/// character becomes its ASCII digit.
fn numerals(value: &str) -> String {
    value
        .nfkc()
        .map(|c| match digit_value(c) {
            Some(d) => char::from(b'0' + d as u8),
            None => c,
        })
        .collect()
}

/// Decimal value of a Unicode digit (`Nd`), by block.
fn digit_value(c: char) -> Option<u32> {
    if let Some(d) = c.to_digit(10) {
        return Some(d);
    }
    // Zero code points of the decimal digit blocks most likely to appear.
    const ZEROS: &[u32] = &[
        0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6, 0x0D66,
        0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
        0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    ];
    let u = c as u32;
    ZEROS.iter().find(|z| u >= **z && u < **z + 10).map(|z| u - z)
}

fn parse_number(value: &str) -> Option<i64> {
    let normalized = numerals(value);
    if let Ok(n) = normalized.parse::<i64>() {
        return Some(n);
    }
    NUMBERS
        .iter()
        .position(|w| *w == value.to_lowercase())
        .map(|i| i as i64)
}

fn parse_decimal(value: &str) -> Option<f64> {
    numerals(value).parse::<f64>().ok()
}

/// `RemoveFileExtension`: drop a trailing media (or `.nzb`, `.par2`) extension.
pub fn remove_file_extension(title: &str) -> String {
    FILE_EXTENSION_RE.replace_all_with(title, |m| {
        let ext = m.whole().to_lowercase();
        if is_media_extension(&ext) || ext == ".par2" || ext == ".nzb" {
            String::new()
        } else {
            m.whole().to_string()
        }
    })
}

/// Longer than any release name; past it the backtracking patterns cost
/// seconds on the database thread.
const MAX_TITLE_LEN: usize = 512;

fn validate_before_parsing(title: &str) -> bool {
    if title.len() > MAX_TITLE_LEN {
        return false;
    }
    let lower = title.to_lowercase();
    if lower.contains("password") && lower.contains("yenc") {
        return false;
    }
    if !title.chars().any(|c| c.is_alphanumeric()) {
        return false;
    }
    let without_extension = remove_file_extension(title);
    if REJECT_HASHED_RELEASES
        .iter()
        .any(|re| re.is_match(&without_extension))
    {
        return false;
    }
    if SEASON_FOLDER_REJECT_RE.is_match(&without_extension) {
        return false;
    }
    true
}

/// Reverse the name when it was posted backwards (`p027.10E10S...`).
fn unreverse(title: &str) -> String {
    if !REVERSED_TITLE_RE.is_match(title) {
        return title.to_string();
    }
    let without_extension = remove_file_extension(title);
    let reversed: String = without_extension.chars().rev().collect();
    format!("{reversed}{}", &title[without_extension.len()..])
}

/// `Parser.ParseTitle`, with the clock at today.
pub fn parse_title(title: &str) -> Option<ParsedEpisodeInfo> {
    parse_title_at(title, chrono::Utc::now().date_naive())
}

/// `Parser.ParseTitle`: `None` when the name does not parse. `today` bounds
/// daily releases: an air date past tomorrow is a parse error.
pub fn parse_title_at(title: &str, today: NaiveDate) -> Option<ParsedEpisodeInfo> {
    if !validate_before_parsing(title) {
        return None;
    }
    let title = unreverse(title);
    let mut release_title = remove_file_extension(&title);
    release_title = release_title.replace('【', "[").replace('】', "]");
    for (re, replacement) in PRE_SUBSTITUTIONS.iter() {
        re.try_replace(&mut release_title, replacement);
    }

    let mut simple_title = SIMPLE_TITLE_RE.replace_all(&release_title, "");
    simple_title = WEBSITE_PREFIX_RE.replace_all(&simple_title, "");
    simple_title = WEBSITE_POSTFIX_RE.replace_all(&simple_title, "");
    simple_title = CLEAN_TORRENT_SUFFIX_RE.replace_all(&simple_title, "");
    simple_title = CLEAN_QUALITY_BRACKETS_RE.replace_all_with(&simple_title, |m| {
        if parse_quality_name(m.whole()).quality.quality_id != 0 {
            String::new()
        } else {
            m.whole().to_string()
        }
    });

    if let Some(m) = SIX_DIGIT_AIR_DATE_RE.captures(&simple_title) {
        let (year, month, day) = (
            m.value("airyear").unwrap_or(""),
            m.value("airmonth").unwrap_or(""),
            m.value("airday").unwrap_or(""),
        );
        if month != "00" || day != "00" {
            let fixed = format!("20{year}.{month}.{day}");
            let airdate = m.value("airdate").unwrap_or("").to_string();
            simple_title = simple_title.replace(&airdate, &fixed);
        }
    }

    for re in REPORT_TITLE.iter() {
        let matches = re.captures_iter(&simple_title);
        if matches.is_empty() {
            continue;
        }
        match parse_match_collection(&matches, &simple_title, &release_title, today) {
            Ok(Some(mut result)) => {
                if result.full_season && result.release_tokens.to_lowercase().contains("special") {
                    result.full_season = false;
                    result.special = true;
                }
                result.languages = parse_languages(&result.release_tokens);
                result.quality = parse_quality(&title);
                result.release_group = parse_release_group(&release_title);
                if let Some(sub_group) = matches[0].value("subgroup").filter(|s| !s.trim().is_empty()) {
                    result.release_group = Some(sub_group.to_string());
                }
                result.release_hash = release_hash(&matches[0]);
                return Some(result);
            }
            Ok(None) => continue,
            // An invalid date ends the search, as the originals' `break` does;
            // any other failure is their caught exception: no result either.
            Err(_) => return None,
        }
    }
    None
}

enum Abort {
    InvalidDate,
    Format,
}

fn parse_match_collection(
    matches: &[NetCaptures<'_, '_>],
    simple_title: &str,
    release_title: &str,
    today: NaiveDate,
) -> Result<Option<ParsedEpisodeInfo>, Abort> {
    let first = &matches[0];
    let series_name = first.value("title").unwrap_or("").replace(['.', '_'], " ");
    let series_name = REQUEST_INFO_RE
        .replace_all(&series_name, "")
        .trim_matches(' ')
        .to_string();

    let air_year: i64 = first.value("airyear").and_then(|v| v.parse().ok()).unwrap_or(0);
    // Indices are .NET string positions in the simplified title, applied to
    // the release title afterwards, as the originals do.
    let u16_end = |byte_end: usize| utf16_len(&simple_title[..byte_end]);
    let mut last_index = first.end_of("title").map(u16_end).unwrap_or(0);

    let mut result = ParsedEpisodeInfo {
        release_title: release_title.to_string(),
        ..ParsedEpisodeInfo::default()
    };

    if air_year < 1900 {
        for m in matches {
            let episode_captures = m.all("episode");
            let absolute_captures = m.all("absoluteepisode");

            if !episode_captures.is_empty() {
                let first_ep = parse_number(episode_captures[0].0).ok_or(Abort::Format)?;
                let last_ep =
                    parse_number(episode_captures[episode_captures.len() - 1].0).ok_or(Abort::Format)?;
                if first_ep > last_ep {
                    return Ok(None);
                }
                result.episode_numbers = (first_ep..=last_ep).collect();
                last_index = last_index.max(u16_end(episode_captures[episode_captures.len() - 1].1));
                if m.success("special") {
                    result.special = true;
                }
                if m.success("splitepisode") {
                    result.is_split_episode = true;
                }
            }

            if !absolute_captures.is_empty() {
                let first_abs = parse_decimal(absolute_captures[0].0).ok_or(Abort::Format)?;
                let last_abs =
                    parse_decimal(absolute_captures[absolute_captures.len() - 1].0).ok_or(Abort::Format)?;
                if first_abs > last_abs {
                    return Ok(None);
                }
                if first_abs.fract() != 0.0 || last_abs.fract() != 0.0 {
                    if absolute_captures.len() != 1 {
                        return Ok(None); // Multiple matches not allowed for specials
                    }
                    result.special_absolute_episode_numbers = vec![first_abs];
                    result.special = true;
                    last_index = last_index.max(u16_end(absolute_captures[0].1));
                } else {
                    result.absolute_episode_numbers = ((first_abs as i64)..=(last_abs as i64)).collect();
                    if m.success("special") {
                        result.special = true;
                    }
                    last_index = last_index.max(u16_end(absolute_captures[absolute_captures.len() - 1].1));
                }
            }

            if episode_captures.is_empty() && absolute_captures.is_empty() {
                if first.value("extras").is_some_and(|v| !v.trim().is_empty()) {
                    result.is_season_extra = true;
                }
                match first.value("seasonpart").filter(|v| !v.trim().is_empty()) {
                    Some(part) => {
                        result.season_part = part.parse().map_err(|_| Abort::Format)?;
                        result.is_partial_season = true;
                    }
                    None => result.full_season = true,
                }
            }

            if episode_captures.len() == 2
                && first.success("episodecount")
                && episode_captures[1].0 == first.value("episodecount").unwrap_or("")
            {
                result.episode_numbers = Vec::new();
                result.full_season = true;
            }
        }

        let mut seasons: Vec<i64> = Vec::new();
        for (value, end) in first.all("season") {
            if let Ok(season) = value.parse::<i64>() {
                seasons.push(season);
                last_index = last_index.max(u16_end(end));
            }
        }
        let mut distinct = seasons.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() > 1 {
            result.is_multi_season = true;
        }
        if let Some(season) = seasons.first() {
            result.season_number = *season;
        } else if result.absolute_episode_numbers.is_empty() && !result.episode_numbers.is_empty() {
            result.season_number = 1;
            result.is_mini_series = true;
        }
    } else {
        // Try to parse as a daily show
        let (air_month, air_day) = if first.success("ambiguousairmonth") && first.success("ambiguousairday") {
            let month: u32 = first
                .value("ambiguousairmonth")
                .unwrap()
                .parse()
                .map_err(|_| Abort::Format)?;
            let day: u32 = first
                .value("ambiguousairday")
                .unwrap()
                .parse()
                .map_err(|_| Abort::Format)?;
            if day <= 12 && month <= 12 {
                return Err(Abort::InvalidDate);
            }
            (month, day)
        } else {
            let month: u32 = first
                .value("airmonth")
                .unwrap_or("")
                .parse()
                .map_err(|_| Abort::Format)?;
            let day: u32 = first
                .value("airday")
                .unwrap_or("")
                .parse()
                .map_err(|_| Abort::Format)?;
            // Swap day and month if month is bigger than 12 (scene fail)
            if month > 12 {
                (day, month)
            } else {
                (month, day)
            }
        };
        let air_date =
            NaiveDate::from_ymd_opt(air_year as i32, air_month, air_day).ok_or(Abort::InvalidDate)?;
        // An episode in the future is most likely a parse error.
        if air_date > today + chrono::Duration::days(1) {
            return Err(Abort::InvalidDate);
        }
        if air_date < NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
            && first.value("titleyear").is_none_or(|v| v.trim().is_empty())
        {
            return Err(Abort::InvalidDate);
        }
        for group in ["airyear", "airmonth", "airday"] {
            if let Some(end) = first.end_of(group) {
                last_index = last_index.max(u16_end(end));
            }
        }
        result.air_date = Some(air_date.format("%Y-%m-%d").to_string());
        if let Some(part) = first.value("part") {
            result.daily_part = Some(part.parse().map_err(|_| Abort::Format)?);
        }
    }

    result.release_tokens = match utf16_from(release_title, last_index) {
        Some(rest) if last_index < utf16_len(release_title) => rest.to_string(),
        _ => release_title.to_string(),
    };
    result.series_title = series_name;
    result.series_title_info = series_title_info(&result.series_title, first);
    Ok(Some(result))
}

fn series_title_info(title: &str, first: &NetCaptures<'_, '_>) -> SeriesTitleInfo {
    let mut info = SeriesTitleInfo {
        title: title.to_string(),
        ..SeriesTitleInfo::default()
    };
    match YEAR_IN_TITLE_RE.captures(title) {
        None => info.title_without_year = title.to_string(),
        Some(m) => {
            info.title_without_year = m.value("title").unwrap_or("").to_string();
            info.year = m.value("year").and_then(|y| y.parse().ok()).unwrap_or(0);
        }
    }
    if let Some(components) = TITLE_COMPONENTS_RE.captures(&info.title_without_year) {
        info.all_titles = components
            .all("title")
            .iter()
            .map(|(v, _)| v.to_string())
            .collect();
    } else {
        let titles = first.all("title");
        if titles.len() > 1 {
            info.all_titles = titles.iter().map(|(v, _)| v.replace(['.', '_'], " ")).collect();
        }
    }
    info
}

fn release_hash(first: &NetCaptures<'_, '_>) -> String {
    match first.value("hash") {
        Some(hash) => {
            let value = hash.trim_matches(['[', ']', '(', ')']);
            if value == "1280x720" {
                String::new()
            } else {
                value.to_string()
            }
        }
        None => String::new(),
    }
}

/// `Parser.ParseReleaseGroup`.
pub fn parse_release_group(title: &str) -> Option<String> {
    let mut title = remove_file_extension(title.trim());
    for (re, replacement) in PRE_SUBSTITUTIONS.iter() {
        if re.try_replace(&mut title, replacement) {
            break;
        }
    }
    title = WEBSITE_PREFIX_RE.replace_all(&title, "");
    title = CLEAN_TORRENT_SUFFIX_RE.replace_all(&title, "");

    if let Some(anime) = ANIME_RELEASE_GROUP_RE.captures(&title) {
        return anime.value("subgroup").map(str::to_string);
    }
    title = CLEAN_RELEASE_GROUP_RE.replace_all(&title, "");

    if let Some(last) = EXCEPTION_RELEASE_GROUP_RE.captures_iter(&title).last() {
        return last.value("releasegroup").map(str::to_string);
    }
    if let Some(last) = EXCEPTION_RELEASE_GROUP_EXACT_RE.captures_iter(&title).last() {
        return last.value("releasegroup").map(str::to_string);
    }
    let last = RELEASE_GROUP_RE.captures_iter(&title).into_iter().last()?;
    let group = last.value("releasegroup")?;
    if group.parse::<i64>().is_ok() || INVALID_RELEASE_GROUP_RE.is_match(group) {
        return None;
    }
    Some(group.to_string())
}

/// `Parser.ParseSeriesName`: the series title of a release, or the cleaned
/// input when nothing parses.
pub fn parse_series_name(title: &str) -> String {
    match parse_title(title) {
        Some(parsed) => parsed.series_title,
        None => clean_series_title(title),
    }
}

/// `CleanSeriesTitle`: the comparison key the originals match series by.
pub fn clean_series_title(title: &str) -> String {
    if title.trim().is_empty() {
        return title.to_string();
    }
    // A title made of digits is kept as is.
    if title.parse::<i64>().is_ok() {
        return title.to_string();
    }
    let title = PERCENT_RE.replace_all(title, "percent");
    remove_accent(&NORMALIZE_RE.replace_all(&title, "").to_lowercase())
}

/// `RemoveAccent`: decompose and drop the non-spacing marks.
pub fn remove_accent(text: &str) -> String {
    text.nfd()
        .filter(|c| !is_non_spacing_mark(*c))
        .collect::<String>()
        .nfc()
        .collect()
}

fn is_non_spacing_mark(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
}

/// `NormalizeEpisodeTitle`.
pub fn normalize_episode_title(title: &str) -> String {
    let mut title = title.to_string();
    if let Some(m) = SPECIAL_EPISODE_TITLE.iter().find_map(|re| re.captures(&title)) {
        title = m.value("episodetitle").unwrap_or("").to_string();
    }
    title = PART_RE.replace_all(&title, "");
    title = PUNCTUATION_RE.replace_all(&title, " ");
    title = DUPLICATE_SPACES_RE.replace_all(&title, " ");
    title.trim().to_lowercase()
}

/// `NormalizeTitle`.
pub fn normalize_title(title: &str) -> String {
    let title = PUNCTUATION_RE.replace_all(title, "");
    let title = ARTICLE_WORD_RE.replace_all(&title, "");
    let title = DUPLICATE_SPACES_RE.replace_all(&title, " ");
    title.trim().to_lowercase()
}

/// `NormalizeImdbId`: `tt` plus at least seven digits, or `None`.
pub fn normalize_imdb_id(imdb_id: &str) -> Option<String> {
    static IMDB_RE: Lazy = net_regex!(r"^(\d{1,10}|(tt)\d{1,10})$", case_sensitive);
    if !IMDB_RE.is_match(imdb_id) {
        return None;
    }
    if imdb_id.len() > 2 {
        let digits = imdb_id.replace("tt", "");
        return Some(format!("tt{digits:0>7}"));
    }
    None
}

/// `SceneChecker.IsSceneTitle`: prefers false negatives, as the originals
/// say: a title that might be scene is not used unless it parses fully.
pub fn is_scene_title(title: &str) -> bool {
    if !title.contains('.') || title.contains(' ') {
        return false;
    }
    match parse_title(title) {
        Some(parsed) => {
            parsed.release_group.is_some()
                && parsed.quality.quality.quality_id != 0
                && !parsed.series_title.trim().is_empty()
        }
        None => false,
    }
}

/// `Parser.ParsePath`: parse a file path, falling back on the folder name.
/// `/` and `\` both separate components.
pub fn parse_path(path: &str) -> Option<ParsedEpisodeInfo> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let (dir_name, file_name) = match trimmed.rfind(['/', '\\']) {
        Some(i) => {
            let dir = trimmed[..i].trim_end_matches(['/', '\\']);
            let dir_name = dir.rfind(['/', '\\']).map(|j| &dir[j + 1..]).unwrap_or(dir);
            (dir_name, &trimmed[i + 1..])
        }
        None => ("", trimmed),
    };
    let extension = path_extension(file_name);
    let mut result = parse_title(file_name);

    // Parse using the folder and file separately, but combine if they both parse correctly.
    if let Some(episode_match) = SIMPLE_EPISODE_NUMBER_RE.captures(file_name) {
        let eligible = result
            .as_ref()
            .is_none_or(|r| r.is_mini_series || !r.absolute_episode_numbers.is_empty());
        if eligible && !dir_name.is_empty() {
            if let Some(season_match) = SEASON_FOLDER_RE.captures(dir_name) {
                if let Some(season) = season_match.value("season") {
                    let captures = episode_match.all("episode");
                    let first = parse_number(captures[0].0)?;
                    let last = parse_number(captures[captures.len() - 1].0)?;
                    let mut path_title = format!("S{season}E{first:02}");
                    if first != last {
                        path_title.push_str(&format!("-E{last:02}"));
                    }
                    if let Some(remaining) = episode_match.value("remaining") {
                        path_title.push(' ');
                        path_title.push_str(remaining);
                    }
                    return parse_title(&path_title);
                }
            }
        }
    }

    let stem = match file_name.rfind('.') {
        Some(i) => &file_name[..i],
        None => file_name,
    };
    if result.is_none() {
        if let Ok(number) = stem.parse::<i64>() {
            result = parse_title(dir_name).and_then(|mut r| {
                if r.absolute_episode_numbers.contains(&number) {
                    r.absolute_episode_numbers = vec![number];
                    Some(r)
                } else if r.episode_numbers.contains(&number) {
                    r.episode_numbers = vec![number];
                    Some(r)
                } else {
                    None
                }
            });
        }
    }
    if result.is_none() {
        result = parse_title(&format!("{dir_name} {file_name}"));
    }
    if result.is_none() {
        result = parse_title(&format!("{dir_name}{extension}"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_standard_episode() {
        let p = parse_title("The.Pitt.S01E01.1080p.AMZN.WEB-DL.DDP5.1.H.264-FLUX").unwrap();
        assert_eq!(p.series_title, "The Pitt");
        assert_eq!(p.season_number, 1);
        assert_eq!(p.episode_numbers, vec![1]);
        assert_eq!(p.release_group.as_deref(), Some("FLUX"));
        assert_eq!(p.quality.quality.quality_id, 3);
        assert_eq!(p.release_tokens, ".1080p.AMZN.WEB-DL.DDP5.1.H.264-FLUX");
        assert_eq!(p.release_type(), ReleaseType::SingleEpisode);

        let p = parse_title("Show.S01E05E06E07.720p.HDTV.x264-GRP").unwrap();
        assert_eq!(p.episode_numbers, vec![5, 6, 7]);
        assert_eq!(p.release_type(), ReleaseType::MultiEpisode);

        let p = parse_title("Show.S02.1080p.BluRay.x264-GRP").unwrap();
        assert!(p.full_season);
        assert_eq!(p.season_number, 2);

        let p = parse_title_at(
            "A.Late.Show.2010.10.11.Guest.HDTV-GRP",
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(p.air_date.as_deref(), Some("2010-10-11"));
        assert!(p.is_daily());

        assert!(parse_title("Season 3").is_none());
        assert!(parse_title("0e895c37245186812cb08aab1529cf8ee389dd05.mkv").is_none());
    }

    #[test]
    fn helpers() {
        assert_eq!(
            clean_series_title("The Series Title (2010)"),
            "theseriestitle2010"
        );
        assert_eq!(clean_series_title("Series of the Title"), "seriestitle");
        assert_eq!(clean_series_title("Seri\u{00E0}es"), "seriaes");
        assert_eq!(clean_series_title("3%"), "3percent");
        assert_eq!(
            normalize_episode_title("Show.S01E00.The.Special.720p.HDTV"),
            "the special"
        );
        assert_eq!(normalize_imdb_id("tt123").as_deref(), Some("tt0000123"));
        assert_eq!(normalize_imdb_id("x"), None);
        assert_eq!(remove_file_extension("Show.S01E01.mkv"), "Show.S01E01");
        assert_eq!(remove_file_extension("Show.S01E01.x264"), "Show.S01E01.x264");
        // A title longer than any release name is refused before the
        // patterns, which backtrack for seconds on repeated text.
        let started = std::time::Instant::now();
        assert!(parse_title(&"-".repeat(6_001)).is_none());
        assert!(parse_title(&format!("Show.S01E01.{}", "a".repeat(600))).is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(parse_number("Three"), Some(3));
        assert_eq!(parse_number("１５８"), Some(158));
        assert_eq!(utf16_from("aé", 2), Some(""));
        assert_eq!(utf16_from("aéb", 2), Some("b"));
    }
}
