//! A pattern's twin ([`narrow_classes`]) must match every text it is used
//! for ([`twin_matches`]) exactly as the pattern does: same
//! matches, same groups. Every pattern the two title parsers build while
//! they parse the fixture libraries' release names, scene names and file
//! paths is matched, in both forms, against each of those the twin is for,
//! the ones the movie parser simplifies them to, and names written with
//! the code points the fixtures have few of (accents, Greek, Cyrillic,
//! typographic punctuation and spaces); every group of every match must
//! have the same span. The patterns are the ones built so far in this test
//! binary ([`recorded`]): the parsers', and those of the other tests.

use std::collections::BTreeSet;
use std::path::Path;

use super::{compile, narrow_classes, recorded, twin_matches};
use crate::sync::Recover;

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn read(db: &Path, sql: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let rows = stmt
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .filter_map(|r| r.unwrap())
        .collect();
    rows
}

type Spans = Vec<Option<std::ops::Range<usize>>>;

fn spans(caps: &fancy_regex::Captures<'_, str>) -> Spans {
    (0..caps.len()).map(|i| caps.get(i).map(|m| m.range())).collect()
}

/// Names with the non-ASCII code points a twin is made for: word
/// characters, digits and spaces among them, and punctuation that is none.
const WIDER: &[&str] = &[
    "Amélie.2001.FRENCH.1080p.BluRay.x264-GRP",
    "Shōgun.2024.S01E03.Domani.è.un.altro.giorno.ITA.ENG.1080p.DSNP.WEB-DL.H264-MeM",
    "Брат.1997.RUS.1080p.WEB-DL.H264-GRP",
    "Ελληνική.Σειρά.S02E05.720p.WEB-DL-GRP",
    "Can’t Hardly Wait (1998) – 1080p BluRay…",
    "School Spirits S02x03 Giovani, pazzi e spettrali - Can’t Hauntly Wait ITA.ENG 1080p H265 sub ita.eng",
    "Señorita\u{00A0}2019\u{2009}1080p\u{2003}WEB-DL-GRP",
    "Mañana.S01E01.१२.Hindi.720p-GRP",
    "Ünter.dem.Mond.2020.GERMAN.DL.1080p.WEB.h264-ÄÖÜ",
    "[Grüppe] Título Ánime - 12 [1080p][ABCD1234]",
    "/tv/Shōgun/Season 1/Shogun.2024.S01E01.Anjin.ITA.ENG.1080p.DSNP.WEB-DL.H264-MeM.GP.mkv",
];

#[test]
fn twins_match_as_their_patterns_do() {
    let mut texts = BTreeSet::new();
    let sonarr = fixtures().join("sonarr/sonarr.db");
    let radarr = fixtures().join("radarr/radarr.db");
    for (db, sql) in [
        (&sonarr, "SELECT DISTINCT SourceTitle FROM History"),
        (&sonarr, "SELECT DISTINCT SceneName FROM EpisodeFiles"),
        (&sonarr, "SELECT DISTINCT RelativePath FROM EpisodeFiles"),
        (&radarr, "SELECT DISTINCT SourceTitle FROM History"),
        (&radarr, "SELECT DISTINCT SceneName FROM MovieFiles"),
        (&radarr, "SELECT DISTINCT RelativePath FROM MovieFiles"),
    ] {
        texts.extend(read(db, sql));
    }
    let mut inputs: BTreeSet<String> = BTreeSet::new();
    for text in &texts {
        // Both parsers, so that their patterns are built (and recorded).
        let _ = crate::parser::parse_title(text);
        if let Some(parsed) = crate::movies::parser::parse_movie_title(text, false) {
            inputs.insert(parsed.simple_release_title);
            inputs.extend(parsed.release_group);
        }
        inputs.insert(text.clone());
    }
    for name in WIDER {
        let _ = crate::parser::parse_title(name);
        if let Some(parsed) = crate::movies::parser::parse_movie_title(name, false) {
            inputs.insert(parsed.simple_release_title);
        }
        inputs.insert(name.to_string());
    }
    let all = inputs.len();
    inputs.retain(|t| twin_matches(t));
    assert!(
        all - inputs.len() < 5,
        "{} of {all} inputs not for a twin",
        all - inputs.len()
    );
    let patterns = recorded().lock().recover().clone();
    let twins: Vec<(&String, String)> = patterns
        .iter()
        .filter_map(|p| narrow_classes(p).map(|twin| (p, twin)))
        .collect();
    assert!(
        patterns.len() > 150 && twins.len() > 100 && inputs.len() > 2000,
        "{} patterns, {} with a twin, {} inputs",
        patterns.len(),
        twins.len(),
        inputs.len()
    );
    eprintln!(
        "{} patterns built, {} with a twin, {} inputs",
        patterns.len(),
        twins.len(),
        inputs.len()
    );
    let mut differences = Vec::new();
    for (pattern, twin) in &twins {
        let unicode = compile(pattern).unwrap();
        let narrow =
            compile(twin).unwrap_or_else(|e| panic!("the twin of {pattern:?} does not compile: {e}"));
        // The first match is the first of every match, with its groups.
        let every = |re: &fancy_regex::Regex, input: &str| -> Vec<Spans> {
            re.captures_iter(input)
                .filter_map(|c| c.ok())
                .map(|c| spans(&c))
                .collect()
        };
        for input in &inputs {
            if every(&unicode, input) != every(&narrow, input) {
                differences.push(format!("{pattern:?} on {input:?}"));
            }
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences, the first: {:#?}",
        differences.len(),
        &differences[..differences.len().min(10)]
    );
}
