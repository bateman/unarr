//! What the title parsers cost in memory: parse release names with the
//! movie parser, the series parser or both, and print the resident set
//! size before and after.
//!
//!     cargo run --release -p unarr-core --example parser_memory -- movies titles.txt
//!     cargo run --release -p unarr-core --example parser_memory -- both titles.txt
//!
//! `titles.txt` holds one release name a line; without it a handful of
//! built-in names are parsed, which already compiles most patterns.

use std::io::BufRead;

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

const BUILT_IN: &[&str] = &[
    "The.Holdovers.2023.iTA.ENG.1080p.BluRay.x264-MIRCrew",
    "Nightmare.Alley.2021.MULTi.iTA.ENG.2160p.UHD.BluRay.REMUX.HDR.HEVC.DTS-HD.MA.5.1-GRP",
    "Movie Title (2019) [tt1234567] [Bluray-1080p][DTS 5.1][x264]-GRP",
    "Some.Show.S01E02.1080p.WEB.H264-GRP",
    "[Group] Anime Title - 12 [1080p][ABCD1234]",
    "Der.Film.2020.German.DL.1080p.BluRay.x264-GRP",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let which = args.first().map(String::as_str).unwrap_or("both");
    let titles: Vec<String> = match args.get(1) {
        Some(path) => std::io::BufReader::new(std::fs::File::open(path).expect("titles file"))
            .lines()
            .map_while(Result::ok)
            .filter(|l| !l.trim().is_empty())
            .collect(),
        None => BUILT_IN.iter().map(|s| s.to_string()).collect(),
    };
    let rounds: usize = std::env::var("ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let start = rss_kb();
    println!("start                {:>8} kB", start);
    let started = std::time::Instant::now();
    let mut parsed = 0usize;
    for round in 0..rounds {
        for title in &titles {
            if which == "movies" || which == "both" {
                parsed += usize::from(unarr_core::movies::parser::parse_movie_title(title, false).is_some());
            }
            if which == "series" || which == "both" {
                parsed += usize::from(unarr_core::parser::parse_title(title).is_some());
            }
        }
        if round == 0 {
            println!(
                "after the first pass {:>8} kB  (+{} kB, {} titles, {} parsed, {:?})",
                rss_kb(),
                rss_kb().saturating_sub(start),
                titles.len(),
                parsed,
                started.elapsed()
            );
        }
    }
    unarr_core::fs::release_memory();
    println!(
        "at the end           {:>8} kB  (+{} kB, {} rounds, {:?})",
        rss_kb(),
        rss_kb().saturating_sub(start),
        rounds,
        started.elapsed()
    );
    // What the daemon does once the patterns have been idle for a while.
    let released = unarr_core::parser::netregex::release_idle(std::time::Duration::ZERO);
    unarr_core::fs::release_memory();
    println!(
        "patterns released    {:>8} kB  (+{} kB, {} patterns let go)",
        rss_kb(),
        rss_kb().saturating_sub(start),
        released
    );
    let again = std::time::Instant::now();
    for title in &titles {
        if which == "movies" || which == "both" {
            let _ = unarr_core::movies::parser::parse_movie_title(title, false);
        }
        if which == "series" || which == "both" {
            let _ = unarr_core::parser::parse_title(title);
        }
    }
    println!(
        "parsed again         {:>8} kB  (+{} kB, {:?} to compile and parse)",
        rss_kb(),
        rss_kb().saturating_sub(start),
        again.elapsed()
    );
}
