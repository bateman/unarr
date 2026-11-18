//! The seek pre-filter of `fancy-regex`, which the parser's patterns and
//! the custom formats' conditions are built with, is marked experimental
//! upstream: it must never change a match. Every condition of the two
//! fixture libraries' custom formats is matched against every release name
//! of their history, raw and as the movie parser simplifies it, with and
//! without seek. The parser itself is covered by the parser vectors.

use std::collections::BTreeSet;
use std::path::Path;

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

#[test]
fn seek_changes_no_custom_format_match() {
    let mut patterns = BTreeSet::new();
    let mut names = BTreeSet::new();
    for app in ["sonarr", "radarr"] {
        let db = fixtures().join(app).join(format!("{app}.db"));
        for specs in read(&db, "SELECT Specifications FROM CustomFormats") {
            let specs: serde_json::Value = serde_json::from_str(&specs).unwrap();
            for spec in specs.as_array().unwrap() {
                if let Some(value) = spec["body"]["value"].as_str() {
                    if !value.trim().is_empty() {
                        patterns.insert(value.to_string());
                    }
                }
            }
        }
        names.extend(read(&db, "SELECT DISTINCT SourceTitle FROM History"));
    }
    let mut inputs: Vec<String> = Vec::new();
    for name in &names {
        inputs.push(name.clone());
        if let Some(parsed) = unarr_core::movies::parser::parse_movie_title(name, false) {
            inputs.push(parsed.simple_release_title);
            inputs.extend(parsed.release_group);
        }
    }
    assert!(
        patterns.len() > 300 && inputs.len() > 1000,
        "{} patterns, {} inputs",
        patterns.len(),
        inputs.len()
    );
    let mut differences = Vec::new();
    for pattern in &patterns {
        // As `customformat::compiled` builds them, with and without seek.
        let build = |seek| {
            fancy_regex::RegexBuilder::new(pattern)
                .case_insensitive(true)
                .seek(seek)
                .build()
        };
        let (Ok(plain), Ok(seeking)) = (build(false), build(true)) else {
            assert!(
                build(false).is_err() && build(true).is_err(),
                "{pattern}: built one way only"
            );
            continue;
        };
        for input in &inputs {
            let span = |re: &fancy_regex::Regex| re.find(input).ok().flatten().map(|m| m.range());
            if span(&plain) != span(&seeking) {
                differences.push(format!("{pattern:?} on {input:?}"));
            }
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences: {:#?}",
        differences.len(),
        &differences[..differences.len().min(10)]
    );
}
