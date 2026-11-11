//! An in-memory ring of recent log events, so the `log` endpoint and the
//! UI's System > Logs page have something to show. Installed as a
//! `tracing_subscriber` layer by the daemon.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub id: i64,
    pub time: DateTime<Utc>,
    /// `fatal`, `error`, `warn`, `info`, `debug`, `trace`, as the API spells them.
    pub level: &'static str,
    pub logger: String,
    pub message: String,
    pub exception: Option<String>,
}

#[derive(Clone, Default)]
pub struct LogBuffer {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    next_id: i64,
    entries: VecDeque<LogEntry>,
}

const CAPACITY: usize = 2000;

impl LogBuffer {
    pub fn new() -> LogBuffer {
        LogBuffer::default()
    }

    pub fn push(&self, level: &'static str, logger: &str, message: String, exception: Option<String>) {
        self.push_at(Utc::now(), level, logger, message, exception);
    }

    fn push_at(
        &self,
        time: DateTime<Utc>,
        level: &'static str,
        logger: &str,
        message: String,
        exception: Option<String>,
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.next_id += 1;
        let entry = LogEntry {
            id: inner.next_id,
            time,
            level,
            logger: logger.to_string(),
            message,
            exception,
        };
        if inner.entries.len() >= CAPACITY {
            inner.entries.pop_front();
        }
        inner.entries.push_back(entry);
    }

    /// `ClearLog`: the lines gone; ids keep counting.
    pub fn clear(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clear();
    }

    /// Newest first.
    pub fn entries(&self) -> Vec<LogEntry> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.entries.iter().rev().cloned().collect()
    }
}

/// An event's message, with its other fields after it, and its `error`.
#[derive(Default)]
pub(crate) struct MessageVisitor {
    pub(crate) message: String,
    pub(crate) error: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "error" => self.error = Some(format!("{value:?}")),
            name => {
                if !self.message.is_empty() {
                    self.message.push(' ');
                }
                self.message.push_str(&format!("{name}={value:?}"));
            }
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "error" => self.error = Some(value.to_string()),
            name => {
                if !self.message.is_empty() {
                    self.message.push(' ');
                }
                self.message.push_str(&format!("{name}={value}"));
            }
        }
    }
}

/// A log event taken out of `tracing`, to be kept or handed on.
#[derive(Debug, Clone)]
pub struct Record {
    pub time: DateTime<Utc>,
    pub level: tracing::Level,
    pub logger: String,
    pub message: String,
    pub error: Option<String>,
}

impl Record {
    pub fn from_event(event: &Event<'_>) -> Record {
        let meta = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        Record {
            time: Utc::now(),
            level: *meta.level(),
            logger: meta.target().to_string(),
            // `CleanseLogMessage`: here, so that neither the recent lines nor
            // the files ever hold what it removes.
            message: crate::cleanse::cleanse(&visitor.message).into_owned(),
            error: visitor.error.map(|e| crate::cleanse::cleanse(&e).into_owned()),
        }
    }
}

impl LogBuffer {
    /// Keep an event, at the time it was logged.
    pub fn record(&self, record: &Record) {
        let level = match record.level {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warn",
            tracing::Level::INFO => "info",
            tracing::Level::DEBUG => "debug",
            tracing::Level::TRACE => "trace",
        };
        self.push_at(
            record.time,
            level,
            &record.logger,
            record.message.clone(),
            record.error.clone(),
        );
    }
}

impl<S: Subscriber> Layer<S> for LogBuffer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        self.record(&Record::from_event(event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_newest_first_and_bounded() {
        let buf = LogBuffer::new();
        for i in 0..(CAPACITY + 5) {
            buf.push("info", "test", format!("m{i}"), None);
        }
        let entries = buf.entries();
        assert_eq!(entries.len(), CAPACITY);
        assert_eq!(entries[0].message, format!("m{}", CAPACITY + 4));
        assert!(entries[0].id > entries[1].id);
    }
}
