//! The spike's load and measurements, shared by every prototype (spec, section 20).
//!
//! - a 10 000-line Java-like file with syntax highlighting spans;
//! - a file tree of about a thousand nodes for keyboard navigation and accessibility;
//! - benchmark modes, chosen by `SPIKE_BENCH`: `scroll` scrolls the editor one line per frame
//!   and reports frame times; `type` injects a key every few frames and reports the time from the
//!   key to the frame that shows it; `start` reports the cold start and exits after the first
//!   frame. `SPIKE_T0` (Unix time in microseconds, set by the runner just before exec) makes the
//!   cold start include process creation.
//! - reports are one JSON line on stdout and, with `SPIKE_REPORT`, appended to that file.

use std::fmt::Write as _;
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const LINES: usize = 10_000;

/// Frames scrolled or keys typed before a benchmark reports.
pub const SCROLL_FRAMES: usize = 600;
pub const TYPED_KEYS: usize = 200;

/// Marker of the key-to-pixel probe: logical position in the window and size.
pub const MARKER_X: f32 = 300.0;
pub const MARKER_Y: f32 = 12.0;
pub const MARKER_SIZE: f32 = 32.0;

/// Token kinds with their colours (0xRRGGBB), shared so every prototype paints the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Keyword,
    Type,
    String,
    Number,
    Comment,
}

impl Kind {
    pub fn rgb(self) -> u32 {
        match self {
            Kind::Plain => 0xd8d4cc,
            Kind::Keyword => 0xc98b5b,
            Kind::Type => 0x8fb3a6,
            Kind::String => 0xa9b97a,
            Kind::Number => 0xd1a45f,
            Kind::Comment => 0x7d766c,
        }
    }

    /// Components in 0.0..=1.0.
    pub fn rgb_f32(self) -> [f32; 3] {
        let c = self.rgb();
        [(c >> 16) as u8, (c >> 8) as u8, c as u8].map(|v| f32::from(v) / 255.0)
    }
}

/// Background and gutter colours.
pub const BACKGROUND: u32 = 0x1f1d1a;
pub const GUTTER: u32 = 0x6b655c;

/// A highlighted span of a line: byte range and kind.
pub type Span = (Range<usize>, Kind);

const TEMPLATES: [&str; 16] = [
    "package org.example.kumiko.spike;",
    "",
    "/** Generated class number {n} for the scrolling benchmark. */",
    "public final class Sample{n} extends AbstractService implements Runnable {",
    "    private static final int LIMIT_{n} = {n};",
    "    private final Map<String, List<Integer>> index = new HashMap<>();",
    "    // keep the cache warm: {n} entries",
    "    @Override",
    "    public void run() {",
    "        for (int i = 0; i < LIMIT_{n}; i++) {",
    "            String key = \"entry-\" + i + \"-{n}\";",
    "            index.computeIfAbsent(key, k -> new ArrayList<>()).add(i * 31 + {n});",
    "        }",
    "        if (index.size() > 0x7fff && !closed) { throw new IllegalStateException(\"full\"); }",
    "    }",
    "}",
];

/// Source text of `lines` lines, deterministic.
pub fn java_source(lines: usize) -> Vec<String> {
    (0..lines)
        .map(|i| TEMPLATES[i % TEMPLATES.len()].replace("{n}", &(i / TEMPLATES.len()).to_string()))
        .collect()
}

const KEYWORDS: &[&str] = &[
    "package",
    "public",
    "final",
    "class",
    "extends",
    "implements",
    "private",
    "static",
    "int",
    "new",
    "void",
    "for",
    "if",
    "throw",
    "return",
];

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Highlighting spans of a line; they cover the whole line.
pub fn highlight(line: &str) -> Vec<Span> {
    let bytes = line.as_bytes();
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let rest = &line[i..];
        let kind = if rest.starts_with("//") || rest.starts_with("/*") {
            i = bytes.len();
            Kind::Comment
        } else if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            Kind::String
        } else if bytes[i].is_ascii_digit() {
            while i < bytes.len() && is_word(bytes[i]) {
                i += 1;
            }
            Kind::Number
        } else if is_word(bytes[i]) || bytes[i] == b'@' {
            i += 1;
            while i < bytes.len() && is_word(bytes[i]) {
                i += 1;
            }
            let word = &line[start..i];
            if KEYWORDS.contains(&word) {
                Kind::Keyword
            } else if word.starts_with(|c: char| c.is_ascii_uppercase() || c == '@') {
                Kind::Type
            } else {
                Kind::Plain
            }
        } else {
            i += 1;
            while i < bytes.len() && !is_word(bytes[i]) && !matches!(bytes[i], b'"' | b'/' | b'@') {
                i += 1;
            }
            Kind::Plain
        };
        spans.push((start..i, kind));
    }
    spans
}

/// A node of the file tree, in display order.
#[derive(Debug, Clone)]
pub struct Node {
    pub depth: usize,
    pub name: String,
    pub dir: bool,
}

/// About a thousand nodes: modules, packages and classes, like a Maven project.
pub fn tree() -> Vec<Node> {
    let node = |depth, name: String, dir| Node { depth, name, dir };
    let mut nodes = vec![node(0, "kumiko-spike".into(), true)];
    for module in ["core", "api", "service", "web"] {
        nodes.push(node(1, module.into(), true));
        nodes.push(node(2, "src/main/java".into(), true));
        for package in 0..12 {
            nodes.push(node(3, format!("org.example.{module}.p{package}"), true));
            for class in 0..18 {
                nodes.push(node(4, format!("Sample{class}.java"), false));
            }
        }
        nodes.push(node(2, "pom.xml".into(), false));
    }
    nodes
}

/// Which benchmark runs, from `SPIKE_BENCH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bench {
    Interactive,
    Start,
    Scroll,
    Type,
    /// Interactive, plus a marker that flips on every `x`, for `latency-probe`.
    Probe,
}

impl Bench {
    pub fn from_env() -> Self {
        match std::env::var("SPIKE_BENCH").as_deref() {
            Ok("start") => Bench::Start,
            Ok("scroll") => Bench::Scroll,
            Ok("type") => Bench::Type,
            Ok("probe") => Bench::Probe,
            _ => Bench::Interactive,
        }
    }
}

fn unix_micros() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0)
}

/// Collects the measurements of one run.
pub struct Recorder {
    pub toolkit: &'static str,
    pub bench: Bench,
    started: Instant,
    started_unix: u128,
    t0_micros: Option<u128>,
    first_frame: Option<Duration>,
    last_frame: Option<Instant>,
    frame_times: Vec<Duration>,
    pending_key: Option<Instant>,
    key_latencies: Vec<Duration>,
    pub frames: usize,
    pub keys: usize,
}

impl Recorder {
    pub fn new(toolkit: &'static str) -> Self {
        Self {
            toolkit,
            bench: Bench::from_env(),
            started: Instant::now(),
            started_unix: unix_micros(),
            t0_micros: std::env::var("SPIKE_T0").ok().and_then(|t| t.parse().ok()),
            first_frame: None,
            last_frame: None,
            frame_times: Vec::new(),
            pending_key: None,
            key_latencies: Vec::new(),
            frames: 0,
            keys: 0,
        }
    }

    /// Call when a frame has been drawn (as late in the frame as the toolkit allows).
    pub fn frame(&mut self) {
        let now = Instant::now();
        if self.first_frame.is_none() {
            self.first_frame = Some(now - self.started);
        }
        if let Some(last) = self.last_frame {
            self.frame_times.push(now - last);
        }
        self.last_frame = Some(now);
        self.frames += 1;
        if let Some(key) = self.pending_key.take() {
            self.key_latencies.push(now - key);
        }
    }

    /// Call when a key event reaches the application.
    pub fn key(&mut self) {
        if self.pending_key.is_none() {
            self.pending_key = Some(Instant::now());
        }
        self.keys += 1;
    }

    /// Whether a key is waiting for its frame.
    pub fn key_pending(&self) -> bool {
        self.pending_key.is_some()
    }

    /// Whether the benchmark has what it needs; the prototype then reports and exits.
    pub fn done(&self) -> bool {
        match self.bench {
            Bench::Interactive | Bench::Probe => false,
            Bench::Start => self.frames >= 1,
            Bench::Scroll => self.frames >= SCROLL_FRAMES,
            Bench::Type => self.key_latencies.len() >= TYPED_KEYS,
        }
    }

    /// Cold start in ms: from `SPIKE_T0` if the runner set it, otherwise from `main`.
    fn cold_start_ms(&self) -> Option<f64> {
        let first = self.first_frame?;
        Some(match self.t0_micros {
            Some(t0) => (self.started_unix + first.as_micros()).saturating_sub(t0) as f64 / 1000.0,
            None => first.as_secs_f64() * 1000.0,
        })
    }

    pub fn report(&self, extra: &str) -> String {
        let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
        let stats = |v: &[Duration]| -> String {
            if v.is_empty() {
                return "null".into();
            }
            let mut sorted: Vec<f64> = v.iter().map(ms).collect();
            sorted.sort_by(f64::total_cmp);
            let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p).round() as usize];
            let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
            format!(
                "{{\"n\":{},\"mean\":{mean:.2},\"p50\":{:.2},\"p95\":{:.2},\"max\":{:.2},\"fps\":{:.1}}}",
                sorted.len(),
                pct(0.5),
                pct(0.95),
                pct(1.0),
                1000.0 / mean
            )
        };
        let mut json = format!(
            "{{\"toolkit\":\"{}\",\"bench\":\"{:?}\",\"label\":\"{}\"",
            self.toolkit,
            self.bench,
            std::env::var("SPIKE_LABEL").unwrap_or_default(),
        );
        if let Some(start) = self.cold_start_ms() {
            let _ = write!(json, ",\"cold_start_ms\":{start:.1}");
        }
        let _ = write!(json, ",\"frame_ms\":{}", stats(&self.frame_times));
        let _ = write!(json, ",\"key_to_frame_ms\":{}", stats(&self.key_latencies));
        let _ = write!(json, ",\"rss_mb\":{:.1}", rss_mb());
        if !extra.is_empty() {
            let _ = write!(json, ",{extra}");
        }
        json.push('}');
        json
    }

    /// Prints the report and appends it to `SPIKE_REPORT` if set.
    pub fn finish(&self, extra: &str) {
        let line = self.report(extra);
        println!("{line}");
        if let Ok(path) = std::env::var("SPIKE_REPORT") {
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{line}");
            }
        }
    }
}

/// Resident memory of this process, in MiB.
pub fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_cover_lines() {
        for line in java_source(64) {
            let mut at = 0;
            for (range, _) in highlight(&line) {
                assert_eq!(range.start, at, "{line}");
                at = range.end;
            }
            assert_eq!(at, line.len(), "{line}");
        }
        assert!(tree().len() > 900);
    }

    #[test]
    fn highlighting_kinds() {
        let spans = highlight("    private int x = 42; // note");
        let kinds: Vec<Kind> = spans.iter().map(|(_, k)| *k).collect();
        assert!(kinds.contains(&Kind::Keyword));
        assert!(kinds.contains(&Kind::Number));
        assert_eq!(*kinds.last().unwrap(), Kind::Comment);
    }
}
