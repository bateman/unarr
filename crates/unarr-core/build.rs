//! The pinned originals, from `upstream/<app>.pin` (issue #9): the one
//! place each version is written. Generates `$OUT_DIR/pins.rs`, which
//! `unarr_core::pin` includes, and fails the build on a pin it cannot read.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The personalities with a pin, by file name and constant.
const PINS: &[(&str, &str)] = &[("sonarr", "SONARR"), ("radarr", "RADARR")];

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let upstream = manifest.join("../../upstream");
    let mut out = String::new();
    for (app, name) in PINS {
        let path = upstream.join(format!("{app}.pin"));
        println!("cargo:rerun-if-changed={}", path.display());
        let pin = read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        writeln!(
            out,
            "pub const {name}: Pin = Pin {{ version: {:?}, build_time: {:?}, migration: {} }};",
            pin["version"], pin["build_time"], pin["migration"]
        )
        .unwrap();
    }
    let dest = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("pins.rs");
    std::fs::write(&dest, out).unwrap_or_else(|err| panic!("{}: {err}", dest.display()));
}

/// The `key = value` lines of a pin, `#` comments aside, each checked.
fn read(path: &Path) -> Result<HashMap<String, String>, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let mut values = HashMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected `key = value`", n + 1))?;
        let (key, value) = (key.trim(), value.trim());
        let ok = match key {
            "version" => {
                let parts: Vec<&str> = value.split('.').collect();
                parts.len() == 4
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            }
            "build_time" => {
                // `YYYY-MM-DDTHH:MM:SSZ`, as `buildTime` gives it.
                value.len() == 20
                    && value.bytes().enumerate().all(|(i, b)| match i {
                        4 | 7 => b == b'-',
                        10 => b == b'T',
                        13 | 16 => b == b':',
                        19 => b == b'Z',
                        _ => b.is_ascii_digit(),
                    })
            }
            "migration" => !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) && value.len() < 10,
            "sha256" => value.len() == 64 && value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            _ => return Err(format!("line {}: unknown key `{key}`", n + 1)),
        };
        if !ok {
            return Err(format!("line {}: `{key}` is not valid: {value:?}", n + 1));
        }
        if values.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("line {}: `{key}` given twice", n + 1));
        }
    }
    for key in ["version", "build_time", "migration", "sha256"] {
        if !values.contains_key(key) {
            return Err(format!("missing `{key}`"));
        }
    }
    Ok(values)
}
