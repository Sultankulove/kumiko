//! Logging: to stderr, and into the IDE log output channel the Output panel shows.

use std::fmt::Write as _;

use kumiko_core::output::{LOG_CHANNEL, Output};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer};

/// Installs the global subscriber; `KUMIKO_LOG` sets the filter (`info` by default).
pub fn init(output: Output) {
    let filter = EnvFilter::try_from_env("KUMIKO_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(OutputLayer { output })
        .init();
}

/// Writes each event as `HH:MM:SS.mmm LEVEL target: message key=value`.
struct OutputLayer {
    output: Output,
}

impl<S: Subscriber> Layer<S> for OutputLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut line = format!(
            "{} {:<5} {}: ",
            clock(),
            level(metadata.level()),
            metadata.target()
        );
        let mut fields = Fields::default();
        event.record(&mut fields);
        line.push_str(&fields.message);
        line.push_str(&fields.rest);
        self.output.append(LOG_CHANNEL, &line);
    }
}

fn level(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

/// UTC time of day.
fn clock() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let (secs, millis) = (now.as_secs() % 86_400, now.subsec_millis());
    format!(
        "{:02}:{:02}:{:02}.{millis:03}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.rest, " {}={value}", field.name());
        }
    }
}
