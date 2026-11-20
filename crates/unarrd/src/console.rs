//! The console, as `ReconfigureLogging` sets it: a level of its own
//! (`ConsoleLogLevel`) and a format (`ConsoleLogFormat`), either Unarr's
//! usual lines or CLEF, the compact log event format a log shipper reads:
//! one JSON object a line, `@t` the time, `@l` the level, `@m` the
//! message, `@x` the error.
//!
//! Both can change while the program runs, when Settings > General is
//! saved; the switches are shared with the layer that writes.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tracing::level_filters::LevelFilter;
use tracing::{Event, Subscriber};
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;
use unarr_api::logbuf::Record;
use unarr_core::config::{ConsoleLogFormat, LogLevel};

/// A level that can change, for a per-layer filter.
#[derive(Clone)]
pub struct SharedLevel(Arc<AtomicUsize>);

impl SharedLevel {
    pub fn new(level: LevelFilter) -> SharedLevel {
        SharedLevel(Arc::new(AtomicUsize::new(encode(level))))
    }

    pub fn set(&self, level: LevelFilter) {
        self.0.store(encode(level), Ordering::Relaxed);
    }

    pub fn get(&self) -> LevelFilter {
        decode(self.0.load(Ordering::Relaxed))
    }

    /// Whether a line at `level` passes.
    pub fn enabled(&self, level: &tracing::Level) -> bool {
        self.get() >= *level
    }
}

fn encode(level: LevelFilter) -> usize {
    match level.into_level() {
        None => 0,
        Some(tracing::Level::ERROR) => 1,
        Some(tracing::Level::WARN) => 2,
        Some(tracing::Level::INFO) => 3,
        Some(tracing::Level::DEBUG) => 4,
        Some(tracing::Level::TRACE) => 5,
    }
}

fn decode(n: usize) -> LevelFilter {
    match n {
        0 => LevelFilter::OFF,
        1 => LevelFilter::ERROR,
        2 => LevelFilter::WARN,
        3 => LevelFilter::INFO,
        4 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    }
}

/// An NLog level as `tracing` filters by it. `tracing` has no fatal
/// level: at Fatal the original writes only fatal lines, which Unarr has
/// none of, so nothing passes.
pub fn level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Trace => LevelFilter::TRACE,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Fatal | LogLevel::Off => LevelFilter::OFF,
    }
}

/// The console's switches: its level and whether it writes CLEF.
#[derive(Clone)]
pub struct Console {
    level: SharedLevel,
    clef: Arc<AtomicBool>,
}

impl Console {
    /// Every line, in the usual format: what the global filter lets
    /// through decides, until the settings are applied.
    pub fn new(format: ConsoleLogFormat) -> Console {
        Console {
            level: SharedLevel::new(LevelFilter::TRACE),
            clef: Arc::new(AtomicBool::new(format == ConsoleLogFormat::Clef)),
        }
    }

    pub fn set_level(&self, level: LevelFilter) {
        self.level.set(level);
    }

    pub fn format(&self) -> ConsoleLogFormat {
        if self.clef.load(Ordering::Relaxed) {
            ConsoleLogFormat::Clef
        } else {
            ConsoleLogFormat::Standard
        }
    }

    pub fn set_format(&self, format: ConsoleLogFormat) {
        self.clef
            .store(format == ConsoleLogFormat::Clef, Ordering::Relaxed);
    }

    /// The console's layer, writing to `writer`: Unarr's usual lines or
    /// CLEF, at the console's level.
    pub fn layer<S, W>(&self, writer: W, ansi: bool) -> impl Layer<S>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
        W: for<'a> MakeWriter<'a> + Clone + Send + Sync + 'static,
    {
        let (level, clef) = (self.level.clone(), self.clef.clone());
        let standard = tracing_subscriber::fmt::layer()
            .with_ansi(ansi)
            .with_writer(writer.clone())
            .with_filter(filter_fn(move |meta| {
                !clef.load(Ordering::Relaxed) && level.enabled(meta.level())
            }));
        let (level, clef) = (self.level.clone(), self.clef.clone());
        let compact = Clef { writer }.with_filter(filter_fn(move |meta| {
            clef.load(Ordering::Relaxed) && level.enabled(meta.level())
        }));
        standard.and_then(compact)
    }
}

/// The layer that writes CLEF.
struct Clef<W> {
    writer: W,
}

impl<S, W> Layer<S> for Clef<W>
where
    S: Subscriber,
    W: for<'a> MakeWriter<'a> + 'static,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let line = clef_line(&Record::from_event(event));
        let _ = self.writer.make_writer().write_all(line.as_bytes());
    }
}

/// One event as NLog's `CompactJsonLayout` writes it: the local time
/// with its offset and seven decimals, NLog's name of the level, the
/// error, then the message; one line. The message and the error are
/// cleansed of secrets, as everywhere else.
pub fn clef_line(record: &Record) -> String {
    let local = record.time.with_timezone(&chrono::Local);
    let time = format!(
        "{}.{:07}{}",
        local.format("%Y-%m-%dT%H:%M:%S"),
        local.timestamp_subsec_nanos() / 100,
        local.format("%:z")
    );
    let level = match record.level {
        tracing::Level::TRACE => "Trace",
        tracing::Level::DEBUG => "Debug",
        tracing::Level::INFO => "Info",
        tracing::Level::WARN => "Warn",
        tracing::Level::ERROR => "Error",
    };
    let text = |s: &str| serde_json::Value::String(s.to_string()).to_string();
    let mut line = format!("{{\"@t\":{},\"@l\":{}", text(&time), text(level));
    if let Some(error) = &record.error {
        line.push_str(&format!(",\"@x\":{}", text(error)));
    }
    line.push_str(&format!(",\"@m\":{}}}\n", text(&record.message)));
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tracing_subscriber::layer::SubscriberExt;

    /// What the console wrote, kept for the test.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Captured {
            self.clone()
        }
    }

    impl Captured {
        fn take(&self) -> String {
            String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
        }
    }

    /// The format the original writes, recorded on Sonarr 4.0.20 with
    /// `SONARR__LOG__CONSOLEFORMAT=Clef`:
    /// `{"@t":"2026-11-19T21:30:24.9801909+00:00","@l":"Error","@x":"…","@m":"Unknown error occurred in DownloadClientHistoryRetentionCheck HealthCheck"}`.
    #[test]
    fn clef_lines_are_what_the_original_writes() {
        let record = Record {
            time: chrono::DateTime::parse_from_rfc3339("2026-11-19T21:30:24.980190987Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            level: tracing::Level::ERROR,
            logger: "unarr_core::health".into(),
            message: "Unknown error occurred in \"a\" check".into(),
            error: Some("connection refused\n at line 1".into()),
        };
        let line = clef_line(&record);
        assert!(line.ends_with("}\n") && line.matches('\n').count() == 1, "{line}");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(sorted, ["@l", "@m", "@t", "@x"]);
        assert!(line.starts_with("{\"@t\":\""), "{line}");
        assert!(
            line.contains("\"@l\":\"Error\",\"@x\":\"connection refused\\n at line 1\",\"@m\":"),
            "{line}"
        );
        assert_eq!(v["@m"], "Unknown error occurred in \"a\" check");
        let time = chrono::DateTime::parse_from_rfc3339(v["@t"].as_str().unwrap()).unwrap();
        // To the tenth of a microsecond, as .NET keeps it.
        assert_eq!(
            (record.time - time.with_timezone(&chrono::Utc)).num_nanoseconds(),
            Some(87)
        );
        // Seven decimals, then the offset.
        let t = v["@t"].as_str().unwrap();
        assert_eq!(t.split('.').nth(1).unwrap().len(), 7 + 6, "{t}");
    }

    /// `ConsoleLogLevel` and `ConsoleLogFormat` change what the console
    /// writes, and how, while the program runs.
    #[test]
    fn the_console_follows_its_level_and_format() {
        let captured = Captured::default();
        let console = Console::new(ConsoleLogFormat::Standard);
        let subscriber = tracing_subscriber::registry().with(console.layer(captured.clone(), false));
        tracing::subscriber::with_default(subscriber, || {
            console.set_level(LevelFilter::INFO);
            tracing::debug!("not at info");
            tracing::info!("plain at info");
            let out = captured.take();
            assert!(
                out.contains("plain at info") && !out.contains("not at info"),
                "{out}"
            );
            assert!(!out.starts_with('{'), "{out}");

            console.set_format(ConsoleLogFormat::Clef);
            console.set_level(LevelFilter::WARN);
            tracing::info!("not at warn");
            tracing::warn!(
                error = "GET http://indexer/api?t=caps&apikey=0123456789abcdef0123456789abcdef: refused",
                "a warning"
            );
            let out = captured.take();
            let lines: Vec<serde_json::Value> =
                out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
            assert_eq!(lines.len(), 1, "{out}");
            assert_eq!(lines[0]["@l"], "Warn");
            assert_eq!(lines[0]["@m"], "a warning");
            // Cleansed as every other place a line goes to.
            assert!(!out.contains("0123456789abcdef"), "{out}");
            assert!(lines[0]["@x"].as_str().unwrap().contains("(removed)"), "{out}");

            console.set_level(LevelFilter::OFF);
            tracing::error!("not at off");
            assert_eq!(captured.take(), "");
        });
    }

    #[test]
    fn nlog_levels_map_to_filters() {
        assert_eq!(level_filter(LogLevel::Trace), LevelFilter::TRACE);
        assert_eq!(level_filter(LogLevel::Warn), LevelFilter::WARN);
        assert_eq!(level_filter(LogLevel::Fatal), LevelFilter::OFF);
        let shared = SharedLevel::new(LevelFilter::DEBUG);
        assert!(shared.enabled(&tracing::Level::DEBUG) && !shared.enabled(&tracing::Level::TRACE));
        for level in [
            LevelFilter::OFF,
            LevelFilter::ERROR,
            LevelFilter::WARN,
            LevelFilter::INFO,
            LevelFilter::DEBUG,
            LevelFilter::TRACE,
        ] {
            shared.set(level);
            assert_eq!(shared.get(), level);
        }
    }
}
