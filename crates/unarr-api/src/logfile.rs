//! Log files under `<app data>/logs`, as the originals' `NzbDroneLogger`
//! writes them and under the names it gives them: for the Sonarr
//! personality `sonarr.txt` for what is logged at info and above,
//! `sonarr.debug.txt` and `sonarr.trace.txt` when the process logs at
//! those levels, each rolled into `sonarr.0.txt`, `sonarr.1.txt`... above
//! a megabyte. The UI's System > Log Files page lists and downloads them.
//!
//! One process serves several personalities. Each has its own files, and
//! the daemon runs each on threads whose `tracing` dispatcher writes to
//! them, so a line lands in the files of the personality that logged it.
//! What the process logs outside any of them goes to all, through
//! [`ProcessLog`].

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::logbuf::{LogBuffer, Record};

/// `ArchiveAboveSize`: `LogSizeLimit`, one megabyte.
const SIZE_LIMIT: u64 = 1024 * 1024;
/// `MaxArchiveFiles`: `LogRotate`'s default.
const MAX_ARCHIVES: usize = 50;
/// Lines of the start kept for the personalities still to be added.
const EARLY_LIMIT: usize = 2000;

/// The three files, by what follows the application's name and the least
/// severe level each takes.
const FILES: [(&str, Level); 3] = [
    ("", Level::INFO),
    (".debug", Level::DEBUG),
    (".trace", Level::TRACE),
];

/// One file and its archives.
struct Rolling {
    dir: PathBuf,
    /// The name without `.txt`.
    stem: String,
    file: Option<File>,
    size: u64,
    limit: u64,
    archives: usize,
    failed: bool,
}

impl Rolling {
    fn new(dir: &Path, stem: &str, limit: u64, archives: usize) -> Rolling {
        Rolling {
            dir: dir.to_path_buf(),
            stem: stem.to_string(),
            file: None,
            size: 0,
            limit,
            archives,
            failed: false,
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(format!("{}.txt", self.stem))
    }

    fn archive(&self, n: usize) -> PathBuf {
        self.dir.join(format!("{}.{n}.txt", self.stem))
    }

    fn open(&mut self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let file = OpenOptions::new().create(true).append(true).open(self.path())?;
        self.size = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }

    /// `ArchiveNumberingMode.Rolling`: the file becomes archive 0, every
    /// archive moves up one, the oldest goes.
    fn roll(&mut self) -> std::io::Result<()> {
        self.file = None;
        if self.archives == 0 {
            std::fs::remove_file(self.path())?;
            return Ok(());
        }
        let _ = std::fs::remove_file(self.archive(self.archives - 1));
        for n in (0..self.archives - 1).rev() {
            let from = self.archive(n);
            if from.exists() {
                std::fs::rename(from, self.archive(n + 1))?;
            }
        }
        std::fs::rename(self.path(), self.archive(0))
    }

    fn write(&mut self, line: &str) {
        if self.failed {
            return;
        }
        let result = (|| {
            // `DeleteLogFiles` removes the file under the writer: a new one
            // is started (the originals do not keep theirs open either).
            if self.file.is_some() && !self.path().exists() {
                self.file = None;
            }
            if self.file.is_none() {
                self.open()?;
            }
            if self.size > 0 && self.size + line.len() as u64 > self.limit {
                self.roll()?;
                self.open()?;
            }
            let file = self.file.as_mut().expect("opened above");
            file.write_all(line.as_bytes())?;
            self.size += line.len() as u64;
            Ok::<(), std::io::Error>(())
        })();
        if let Err(e) = result {
            // Not through `tracing`: this runs inside its layer.
            eprintln!("log file {} disabled: {e}", self.path().display());
            self.failed = true;
            self.file = None;
        }
    }
}

/// One personality's log files; clones share them.
#[derive(Clone)]
pub struct LogFiles {
    /// The files, by the least severe level each takes.
    files: Arc<Mutex<Vec<(Level, Rolling)>>>,
    /// The level being logged at: the debug and trace files are written
    /// only at theirs.
    enabled: Arc<std::sync::RwLock<tracing::level_filters::LevelFilter>>,
    /// Whose rules cleanse what is written (`crate::cleanse::cleanse_for`).
    personality: Option<unarr_core::Personality>,
}

impl LogFiles {
    /// The same files, cleansed with the rules of `personality`'s original.
    pub fn for_personality(mut self, personality: unarr_core::Personality) -> LogFiles {
        self.personality = Some(personality);
        self
    }

    /// The files under `dir`, named after `app` (the original
    /// application's name in lower case: `sonarr.txt`). The debug and
    /// trace files exist only when the process logs at those levels.
    pub fn open(dir: &Path, app: &str) -> LogFiles {
        LogFiles::open_with(
            dir,
            app,
            SIZE_LIMIT,
            MAX_ARCHIVES,
            tracing::level_filters::LevelFilter::current(),
        )
    }

    fn open_with(
        dir: &Path,
        app: &str,
        limit: u64,
        archives: usize,
        enabled: tracing::level_filters::LevelFilter,
    ) -> LogFiles {
        let files = FILES
            .iter()
            .map(|(suffix, least)| {
                (
                    *least,
                    Rolling::new(dir, &format!("{app}{suffix}"), limit, archives),
                )
            })
            .collect();
        LogFiles {
            files: Arc::new(Mutex::new(files)),
            enabled: Arc::new(std::sync::RwLock::new(enabled)),
            personality: None,
        }
    }

    /// `ReconfigureLogging`: from now on the debug and trace files take
    /// lines only if `level` reaches them. Files already written stay.
    pub fn set_level(&self, level: tracing::level_filters::LevelFilter) {
        *self.enabled.write().unwrap_or_else(|e| e.into_inner()) = level;
    }

    /// Write an event to the files its level reaches.
    pub fn record(&self, record: &Record) {
        let line = layout(
            record.time.with_timezone(&chrono::Local),
            record.level,
            &record.logger,
            &record.message,
            record.error.as_deref(),
        );
        self.log(record.level, &line);
    }

    fn log(&self, level: Level, line: &str) {
        let enabled = *self.enabled.read().unwrap_or_else(|e| e.into_inner());
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        for (least, file) in files.iter_mut() {
            // `Level` orders the more severe lower.
            if level <= *least && (*least == Level::INFO || enabled >= *least) {
                file.write(line);
            }
        }
    }
}

/// What the process logs outside any personality (its start, its
/// shutdown), handed to every personality's recent lines and files: one
/// process serves several, and each of their logs should tell the whole
/// story of its own.
///
/// A personality is added when its folder is known. Until [`seal`], what
/// was logged before is kept and given to each one added, so the lines of
/// the start reach the personality opened last too.
///
/// [`seal`]: ProcessLog::seal
#[derive(Clone, Default)]
pub struct ProcessLog {
    inner: Arc<Mutex<ProcessInner>>,
}

#[derive(Default)]
struct ProcessInner {
    targets: Vec<(LogBuffer, LogFiles)>,
    /// The lines so far, while personalities are still being added.
    early: Vec<Record>,
    sealed: bool,
}

impl ProcessLog {
    pub fn new() -> ProcessLog {
        ProcessLog::default()
    }

    /// A personality's recent lines and files: they get what the process
    /// logged so far, and what it logs from now on.
    pub fn add(&self, buffer: LogBuffer, files: LogFiles) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for record in &inner.early {
            buffer.record(record);
            files.record(record);
        }
        inner.targets.push((buffer, files));
    }

    /// Every personality was added: the early lines are no longer needed.
    pub fn seal(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.sealed = true;
        inner.early = Vec::new();
    }

    fn record(&self, record: Record) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for (buffer, files) in &inner.targets {
            buffer.record(&record);
            files.record(&record);
        }
        if !inner.sealed && inner.early.len() < EARLY_LIMIT {
            inner.early.push(record);
        }
    }
}

impl<S: Subscriber> Layer<S> for ProcessLog {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        self.record(Record::from_event(event));
    }
}

/// `FILE_LOG_LAYOUT`: `date|Level|logger|message`, the error after a blank
/// line under the version.
fn layout(
    time: chrono::DateTime<chrono::Local>,
    level: Level,
    logger: &str,
    message: &str,
    error: Option<&str>,
) -> String {
    let name = match level {
        Level::ERROR => "Error",
        Level::WARN => "Warn",
        Level::INFO => "Info",
        Level::DEBUG => "Debug",
        Level::TRACE => "Trace",
    };
    // `yyyy-MM-dd HH:mm:ss.f`: tenths of a second.
    let mut line = format!(
        "{}.{}|{name}|{logger}|{message}",
        time.format("%Y-%m-%d %H:%M:%S"),
        (time.timestamp_subsec_millis() / 100).min(9)
    );
    if let Some(error) = error {
        line.push_str(&format!("\n\n[v{}] {error}\n", env!("CARGO_PKG_VERSION")));
    }
    line.push('\n');
    line
}

impl<S: Subscriber> Layer<S> for LogFiles {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        self.record(&Record::from_event_for(event, self.personality));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::layer::SubscriberExt;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unarr_logfile_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn lines_follow_the_originals_layout() {
        let at = chrono::Local.with_ymd_and_hms(2026, 11, 10, 9, 5, 7).unwrap()
            + chrono::Duration::milliseconds(380);
        assert_eq!(
            layout(at, Level::INFO, "unarrd", "listening", None),
            "2026-11-10 09:05:07.3|Info|unarrd|listening\n"
        );
        assert_eq!(
            layout(
                at,
                Level::ERROR,
                "unarr_core::tv",
                "Couldn't refresh",
                Some("timed out")
            ),
            format!(
                "2026-11-10 09:05:07.3|Error|unarr_core::tv|Couldn't refresh\n\n[v{}] timed out\n\n",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn a_full_file_rolls_and_the_oldest_archive_goes() {
        let dir = temp("roll");
        let mut file = Rolling::new(&dir, "sonarr", 30, 2);
        for i in 0..5 {
            // Twenty bytes each: the second one of a file does not fit.
            file.write(&format!("line {i} 0123456789ab\n"));
        }
        assert_eq!(names(&dir), ["sonarr.0.txt", "sonarr.1.txt", "sonarr.txt"]);
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
        assert!(read("sonarr.txt").starts_with("line 4"));
        assert!(read("sonarr.0.txt").starts_with("line 3"));
        assert!(read("sonarr.1.txt").starts_with("line 2"));
        // A file found at the next start is appended to.
        let mut again = Rolling::new(&dir, "sonarr", 1000, 2);
        again.write("more\n");
        assert!(read("sonarr.txt").ends_with("ab\nmore\n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn levels_pick_their_files() {
        let dir = temp("layer");
        let files = LogFiles::open_with(&dir, "sonarr", SIZE_LIMIT, MAX_ARCHIVES, LevelFilter::DEBUG);
        let subscriber = tracing_subscriber::registry().with(files.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("started");
            tracing::debug!(series = 3, "details");
            tracing::warn!(error = %"boom", "it failed");
        });
        assert_eq!(names(&dir), ["sonarr.debug.txt", "sonarr.txt"]);
        let info = std::fs::read_to_string(dir.join("sonarr.txt")).unwrap();
        assert!(info.contains("|Info|") && info.contains("started"));
        assert!(info.contains("|Warn|") && info.contains("it failed\n\n[v"));
        assert!(!info.contains("details"));
        let debug = std::fs::read_to_string(dir.join("sonarr.debug.txt")).unwrap();
        assert!(debug.contains("|Debug|") && debug.contains("details series=3"));
        assert!(debug.contains("started") && debug.contains("it failed"));

        // At the default level only the info file exists.
        let plain = temp("plain");
        let files = LogFiles::open_with(&plain, "radarr", SIZE_LIMIT, MAX_ARCHIVES, LevelFilter::INFO);
        files.log(Level::INFO, "one\n");
        files.log(Level::DEBUG, "two\n");
        assert_eq!(names(&plain), ["radarr.txt"]);
        assert_eq!(
            std::fs::read_to_string(plain.join("radarr.txt")).unwrap(),
            "one\n"
        );
        for d in [dir, plain] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    /// A secret in a message, a field or the error under the line never
    /// reaches a file.
    #[test]
    fn secrets_do_not_reach_the_files() {
        let dir = temp("cleansed");
        let files = LogFiles::open_with(&dir, "sonarr", SIZE_LIMIT, MAX_ARCHIVES, LevelFilter::INFO);
        let subscriber = tracing_subscriber::registry().with(files.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                error =
                    "error sending request for url (http://plex.test:32400/identity?X-Plex-Token=mySecret)",
                host = "plex.test",
                "failed to update Plex host"
            );
            tracing::info!(
                url = "http://indexer.test/api?apikey=mySecret&t=caps",
                "asking /home/mySecret/x"
            );
        });
        let text = std::fs::read_to_string(dir.join("sonarr.txt")).unwrap();
        assert!(!text.contains("mySecret"), "{text}");
        assert!(text.contains("X-Plex-Token=(removed)"), "{text}");
        assert!(text.contains("apikey=(removed)&t=caps"), "{text}");
        assert!(
            text.contains("failed to update Plex host host=plex.test"),
            "{text}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The process's own lines reach every personality, those of the start
    /// included; a personality's lines stay in its files.
    #[test]
    fn process_lines_reach_every_personality_and_theirs_stay_apart() {
        let root = temp("process");
        let open = |app: &str| {
            let files =
                LogFiles::open_with(&root.join(app), app, SIZE_LIMIT, MAX_ARCHIVES, LevelFilter::INFO);
            (LogBuffer::new(), files)
        };
        let process = ProcessLog::new();
        let global = tracing_subscriber::registry().with(process.clone());
        let read = |app: &str| std::fs::read_to_string(root.join(app).join(format!("{app}.txt"))).unwrap();
        tracing::subscriber::with_default(global, || {
            tracing::info!("process started");
            let (sonarr_lines, sonarr_files) = open("sonarr");
            process.add(sonarr_lines.clone(), sonarr_files.clone());
            tracing::info!("between the two");
            let (radarr_lines, radarr_files) = open("radarr");
            process.add(radarr_lines.clone(), radarr_files.clone());
            process.seal();

            // Each personality logs through a dispatcher of its own.
            let sonarr = tracing_subscriber::registry()
                .with(sonarr_lines.clone())
                .with(sonarr_files);
            tracing::subscriber::with_default(sonarr, || tracing::info!("a series was refreshed"));
            let radarr = tracing_subscriber::registry()
                .with(radarr_lines.clone())
                .with(radarr_files);
            tracing::subscriber::with_default(radarr, || tracing::warn!("a movie is missing"));
            tracing::info!("shutting down");

            let messages = |lines: &LogBuffer| -> Vec<String> {
                lines.entries().into_iter().rev().map(|e| e.message).collect()
            };
            assert_eq!(
                messages(&sonarr_lines),
                [
                    "process started",
                    "between the two",
                    "a series was refreshed",
                    "shutting down"
                ]
            );
            assert_eq!(
                messages(&radarr_lines),
                [
                    "process started",
                    "between the two",
                    "a movie is missing",
                    "shutting down"
                ]
            );
        });
        let (sonarr, radarr) = (read("sonarr"), read("radarr"));
        for both in ["process started", "between the two", "shutting down"] {
            assert_eq!(sonarr.matches(both).count(), 1, "{sonarr}");
            assert_eq!(radarr.matches(both).count(), 1, "{radarr}");
        }
        assert!(sonarr.contains("a series was refreshed") && !sonarr.contains("a movie is missing"));
        assert!(radarr.contains("a movie is missing") && !radarr.contains("a series was refreshed"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
