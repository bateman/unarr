//! Port of Radarr's `ReleaseGroupParser`. The patterns are Radarr's: its
//! exception lists differ from Sonarr's, a group is not read out of a
//! TMDB or IMDb id at the end of a name, and the exact exceptions are
//! tried before the bracketed ones.

use crate::movies::parser::{remove_file_extension, CLEAN_TORRENT_SUFFIX_RE, WEBSITE_PREFIX_RE};
use crate::net_regex;
use crate::parser::netregex::Lazy;

/// What may not come right before the end of a group: the original's
/// look-behind, without the names of its two id groups.
macro_rules! not_a_group {
    () => {
        r"(?<!WEB-(DL|Rip)|Blu-Ray|480p|576p|720p|1080p|2160p|DTS-HD|DTS-X|DTS-MA|DTS-ES|-ES|-EN|-CAT|-ENG|-JAP|-GER|-FRA|-FRE|-ITA|-HDRip|\d{1,2}-bit|[ ._]\d{4}-\d{2}|-\d{2}|tmdb(id)?-(\d+)|(tt\d{7,8}))"
    };
}

// The original's look-behind ends in an optional back-reference to the
// second part of the group, `(?<!(?:...)(?:\k<part2>)?)`: a .NET
// look-behind reads right to left, so it refuses a group whose first part
// ends in one of those words as well as one that ends in them as a whole.
// The engine here reads look-behinds left to right and gets that
// back-reference wrong once the second part was tried and given up, so the
// same condition is written as two look-behinds: one before the second
// part, one at the end.
static RELEASE_GROUP_RE: Lazy = net_regex!(concat!(
    r"-(?<releasegroup>[a-z0-9]+(?:",
    not_a_group!(),
    r"(?<part2>-[a-z0-9]+))?(?!.+?(?:480p|576p|720p|1080p|2160p)))",
    not_a_group!(),
    r"(?:\b|[-._ ]|$)|[-._ ]\[(?<releasegroup>[a-z0-9]+)\]$"
));
static INVALID_RELEASE_GROUP_RE: Lazy = net_regex!(r"^([se]\d+|[0-9a-f]{8})$");
static ANIME_RELEASE_GROUP_RE: Lazy = net_regex!(r"^(?:\[(?<subgroup>(?!\s).+?(?<!\s))\](?:_|-|\s|\.)?)");
static EXCEPTION_RELEASE_GROUP_EXACT_RE: Lazy = net_regex!(
    r"\b(?<releasegroup>KRaLiMaRKo|E\.N\.D|D\-Z0N3|Koten_Gars|BluDragon|ZØNEHD|HQMUX|VARYG|YIFY|YTS(.(MX|LT|AG))?|TMd|Eml HDTeam|LMain|DarQ|BEN THE MEN|TAoE|QxR|126811)\b"
);
static EXCEPTION_RELEASE_GROUP_RE: Lazy = net_regex!(
    r"(?<=[._ \[])(?<releasegroup>(Silence|afm72|Panda|Ghost|MONOLITH|Tigole|Joy|ImE|UTR|t3nzin|Anime Time|Project Angel|Hakata Ramen|HONE|GiLG|Vyndros|SEV|Garshasp|Kappa|Natty|RCVR|SAMPA|YOGI|r00t|EDGE2020|RZeroX|FreetheFish|Anna|Bandi|Qman|theincognito|HDO|DusIctv|DHD|CtrlHD|-ZR-|ADC|XZVN|RH|Kametsu|Celdra)(?=\]|\)))"
);
static CLEAN_RELEASE_GROUP_RE: Lazy = net_regex!(
    r"(-(RP|1|NZBGeek|Obfuscated|Obfuscation|Scrambled|sample|Pre|postbot|xpost|Rakuv[a-z0-9]*|WhiteRev|BUYMORE|AsRequested|AlternativeToRequested|GEROV|Z0iDS3N|Chamele0n|4P|4Planet|AlteZachen|RePACKPOST))+$"
);

/// `ReleaseGroupParser.ParseReleaseGroup`.
pub fn parse_release_group(title: &str) -> Option<String> {
    let mut title = remove_file_extension(title.trim());
    title = WEBSITE_PREFIX_RE.replace_all(&title, "");
    title = CLEAN_TORRENT_SUFFIX_RE.replace_all(&title, "");

    if let Some(anime) = ANIME_RELEASE_GROUP_RE.captures(&title) {
        return anime.value("subgroup").map(str::to_string);
    }
    title = CLEAN_RELEASE_GROUP_RE.replace_all(&title, "");

    if let Some(last) = EXCEPTION_RELEASE_GROUP_EXACT_RE.captures_iter(&title).last() {
        return last.value("releasegroup").map(str::to_string);
    }
    if let Some(last) = EXCEPTION_RELEASE_GROUP_RE.captures_iter(&title).last() {
        return last.value("releasegroup").map(str::to_string);
    }
    let last = RELEASE_GROUP_RE.captures_iter(&title).into_iter().last()?;
    let group = last.value("releasegroup")?;
    if group.trim().parse::<i32>().is_ok() || INVALID_RELEASE_GROUP_RE.is_match(group) {
        return None;
    }
    Some(group.to_string())
}
