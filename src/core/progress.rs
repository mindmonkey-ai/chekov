//! Download progress for `chekov pull`, on stderr only.
//!
//! stdout keeps its one-line-per-shard script-friendly messages; everything
//! here goes to stderr, so `chekov pull > log` still reads cleanly. Rendering
//! is a pure function of `(bytes done, now)` so it is tested without a network
//! or a terminal.

use std::collections::VecDeque;
use std::io::{IsTerminal, Read, Write};
use std::time::{Duration, Instant};

/// Rate is averaged over at most this much history, so a stall shows up
/// quickly instead of being hidden by a fast first minute.
const RATE_WINDOW: Duration = Duration::from_secs(5);

/// The status line is redrawn no more often than this; a 40 GB shard would
/// otherwise spend real time formatting.
const TICK: Duration = Duration::from_secs(1);

/// Sizes read as decimal gigabytes (what the hub's listings publish) and as
/// mebibytes below that, where the binary unit is what a transfer feels like.
const GB: u64 = 1_000_000_000;
const MIB: u64 = 1024 * 1024;

/// Where a shard's progress goes. Chosen once per pull from stderr, so a
/// redirected log never collects thousands of carriage returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    /// A terminal: one line, redrawn in place with `\r`.
    Tty,
    /// A pipe or a file: one plain line per 10% of the shard.
    Plain,
}

impl Sink {
    /// `Tty` when stderr is a terminal, `Plain` otherwise.
    #[must_use]
    pub fn for_stderr() -> Self {
        if std::io::stderr().is_terminal() {
            Self::Tty
        } else {
            Self::Plain
        }
    }
}

/// Which shard of the plan is being fetched. Bundled because §3.4 caps
/// `Progress::new` at three arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shard {
    /// 1-based position in the plan.
    pub index: usize,
    pub count: usize,
    /// The repo-relative filename, named in the resume and restart notices.
    pub label: String,
    /// The API's byte size, when it published one.
    pub total: Option<u64>,
}

/// One shard's status line and the samples its rate is measured from.
#[derive(Debug)]
pub struct Progress {
    shard: Shard,
    resumed_from: u64,
    sink: Sink,
    start: Instant,
    samples: VecDeque<(Instant, u64)>,
    last_width: usize,
    last_decile: u64,
}

impl Progress {
    #[must_use]
    pub fn new(shard: Shard, resumed_from: u64, sink: Sink) -> Self {
        Self {
            shard,
            resumed_from,
            sink,
            start: Instant::now(),
            samples: VecDeque::new(),
            last_width: 0,
            last_decile: 0,
        }
    }

    #[must_use]
    pub const fn total(&self) -> Option<u64> {
        self.shard.total
    }

    #[must_use]
    pub const fn resumed_from(&self) -> u64 {
        self.resumed_from
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.shard.label
    }

    /// Forget the resumed offset: the server is sending the file from zero.
    pub fn restart(&mut self) {
        self.resumed_from = 0;
        self.start = Instant::now();
        self.samples.clear();
    }

    /// Record a `(when, bytes done)` sample and drop the ones that have aged
    /// out of the rate window. The oldest sample still inside the window is
    /// kept, so the rate is always measured over as much history as it has.
    pub fn observe(&mut self, done: u64, now: Instant) {
        self.samples.push_back((now, done));
        while self.samples.len() > 1 && now.duration_since(self.samples[1].0) > RATE_WINDOW {
            self.samples.pop_front();
        }
    }

    /// Bytes per second across the rate window; `None` until any time has
    /// passed, because a rate over zero seconds is a guess.
    fn rate(&self, done: u64, now: Instant) -> Option<u64> {
        let (since, from) = self
            .samples
            .front()
            .copied()
            .unwrap_or((self.start, self.resumed_from));
        let millis = u64::try_from(now.duration_since(since).as_millis()).ok()?;
        if millis == 0 {
            return None;
        }
        Some(done.saturating_sub(from).saturating_mul(1000) / millis)
    }

    /// The status line for `done` bytes at `now`. Pure — no clock, no output.
    #[must_use]
    pub fn line(&self, done: u64, now: Instant) -> String {
        let rate = self.rate(done, now);
        let head = format!("  shard {}/{}", self.shard.index, self.shard.count);
        let line = self.shard.total.map_or_else(
            || format!("{head}  {}  {}", format_size(done), rate_text(rate)),
            |total| {
                format!(
                    "{head}  {}  {}%  {}  ETA {}",
                    size_pair(done, total),
                    percent(done, total),
                    rate_text(rate),
                    eta_text(total.saturating_sub(done), rate)
                )
            },
        );
        if self.resumed_from > 0 && done == self.resumed_from {
            return format!("{line}  resumed at {}", format_size(self.resumed_from));
        }
        line
    }

    /// Report an in-flight total. Errors are swallowed by the caller: a
    /// progress line must never be what fails a download.
    pub fn emit(&mut self, out: &mut dyn Write, done: u64) -> std::io::Result<()> {
        let now = Instant::now();
        let text = self.line(done, now);
        self.observe(done, now);
        if self.sink == Sink::Tty {
            return self.redraw(out, &text);
        }
        if self.crossed_decile(done) {
            return writeln!(out, "{text}");
        }
        Ok(())
    }

    /// The shard's last line, always terminated so the next message starts on
    /// a fresh row.
    pub fn finish(&mut self, out: &mut dyn Write, done: u64) -> std::io::Result<()> {
        let text = self.line(done, Instant::now());
        if self.sink == Sink::Plain {
            return writeln!(out, "{text}");
        }
        self.redraw(out, &text)?;
        out.write_all(b"\n")
    }

    /// Redraw in place, padded to the previous line's width so a shorter line
    /// never leaves the tail of a longer one behind it.
    fn redraw(&mut self, out: &mut dyn Write, text: &str) -> std::io::Result<()> {
        let width = text.chars().count();
        let pad = self.last_width.saturating_sub(width);
        self.last_width = width;
        write!(out, "\r{text}{:pad$}", "")
    }

    /// True the first time `done` enters a new tenth of the shard. The last
    /// tenth belongs to `finish`, so a redirected log gets at most ten lines.
    fn crossed_decile(&mut self, done: u64) -> bool {
        let Some(total) = self.shard.total.filter(|total| *total > 0) else {
            return false;
        };
        let decile = done.saturating_mul(10) / total;
        if decile <= self.last_decile || decile >= 10 {
            return false;
        }
        self.last_decile = decile;
        true
    }
}

/// A reader that counts what passes through it and reports the running total
/// at most once a second, and always at EOF.
pub struct CountingReader<R, F> {
    inner: R,
    done: u64,
    last_tick: Instant,
    on_tick: F,
}

impl<R: Read, F: FnMut(u64)> CountingReader<R, F> {
    /// `start` is the resumed offset, so the reported total is a position in
    /// the file rather than a count of this connection's bytes.
    #[must_use]
    pub fn new(inner: R, start: u64, on_tick: F) -> Self {
        Self {
            inner,
            done: start,
            last_tick: Instant::now(),
            on_tick,
        }
    }
}

impl<R: Read, F: FnMut(u64)> Read for CountingReader<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.done = self
            .done
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if read == 0 || self.last_tick.elapsed() >= TICK {
            self.last_tick = Instant::now();
            (self.on_tick)(self.done);
        }
        Ok(read)
    }
}

/// `12.3 GB` at or above a gigabyte, `812.5 MiB` below it.
#[must_use]
pub fn format_size(bytes: u64) -> String {
    if bytes >= GB {
        return format!("{}.{} GB", bytes / GB, (bytes % GB) / (GB / 10));
    }
    format!("{}.{} MiB", bytes / MIB, (bytes % MIB) * 10 / MIB)
}

/// `12.3 / 39.9 GB` when both sides share a unit, both units otherwise.
fn size_pair(done: u64, total: u64) -> String {
    let (left, right) = (format_size(done), format_size(total));
    match (left.split_once(' '), right.split_once(' ')) {
        (Some((number, unit)), Some((_, whole))) if unit == whole => format!("{number} / {right}"),
        _ => format!("{left} / {right}"),
    }
}

/// Rounded, so a shard that is 30.6% done does not read as 30%.
const fn percent(done: u64, total: u64) -> u64 {
    if total == 0 {
        return 100;
    }
    (done.saturating_mul(100).saturating_add(total / 2)) / total
}

fn rate_text(rate: Option<u64>) -> String {
    rate.map_or_else(
        || "? MiB/s".to_owned(),
        |bytes| format!("{} MiB/s", bytes / MIB),
    )
}

/// `4m29s`. A stalled or unmeasured transfer says `?` rather than a number
/// that would be wrong.
fn eta_text(remaining: u64, rate: Option<u64>) -> String {
    let Some(rate) = rate.filter(|rate| *rate > 0) else {
        return "?".to_owned();
    };
    let secs = remaining / rate;
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::{CountingReader, Progress, Shard, Sink, format_size};
    use std::io::{Cursor, Read};
    use std::time::{Duration, Instant};

    fn shard(total: Option<u64>) -> Shard {
        Shard {
            index: 2,
            count: 5,
            label: "model-00002-of-00005.gguf".to_owned(),
            total,
        }
    }

    /// A progress whose rate anchor is exactly `(t0, from)`, so `line` is a
    /// pure function of the arguments and never of the wall clock.
    fn anchored(total: Option<u64>, from: u64, t0: Instant) -> Progress {
        let mut progress = Progress::new(shard(total), from, Sink::Plain);
        progress.observe(from, t0);
        progress
    }

    #[test]
    fn a_known_total_line_shows_percent_and_eta() {
        let t0 = Instant::now();
        let progress = anchored(Some(39_900_000_000), 0, t0);
        let line = progress.line(12_300_000_000, t0 + Duration::from_mins(2));
        assert_eq!(
            line,
            "  shard 2/5  12.3 / 39.9 GB  31%  97 MiB/s  ETA 4m29s"
        );
    }

    #[test]
    fn an_unknown_total_line_omits_percent_and_eta() {
        let t0 = Instant::now();
        let progress = anchored(None, 0, t0);
        let line = progress.line(12_300_000_000, t0 + Duration::from_mins(2));
        assert_eq!(line, "  shard 2/5  12.3 GB  97 MiB/s");
    }

    #[test]
    fn a_stalled_shard_reports_an_unknown_eta() {
        let t0 = Instant::now();
        let progress = anchored(Some(39_900_000_000), 0, t0);
        let line = progress.line(12_300_000_000, t0);
        assert!(line.ends_with("? MiB/s  ETA ?"), "{line}");
    }

    #[test]
    fn a_shard_that_stopped_moving_reports_a_zero_rate() {
        let t0 = Instant::now();
        let progress = anchored(Some(39_900_000_000), 0, t0);
        let line = progress.line(0, t0 + Duration::from_secs(4));
        assert!(line.ends_with("0 MiB/s  ETA ?"), "{line}");
    }

    #[test]
    fn a_resumed_shard_says_where_it_picked_up() {
        let t0 = Instant::now();
        let progress = anchored(Some(39_900_000_000), 12_300_000_000, t0);
        let line = progress.line(12_300_000_000, t0 + Duration::from_secs(1));
        assert!(line.ends_with("resumed at 12.3 GB"), "{line}");
    }

    #[test]
    fn sizes_below_a_gigabyte_render_in_mib() {
        assert_eq!(format_size(0), "0.0 MiB");
        assert_eq!(format_size(812_500_000), "774.8 MiB");
        assert_eq!(format_size(1_000_000_000), "1.0 GB");
        assert_eq!(format_size(39_900_000_000), "39.9 GB");
    }

    #[test]
    fn counting_reader_copies_every_byte_and_reports_the_total_at_eof() {
        let bytes: Vec<u8> = (0..4096_u32)
            .map(|n| u8::try_from(n % 251).unwrap_or_default())
            .collect();
        let mut seen = Vec::new();
        let mut out = Vec::new();
        {
            let mut reader = CountingReader::new(Cursor::new(bytes.clone()), 0, |done| {
                seen.push(done);
            });
            std::io::copy(&mut reader, &mut out).unwrap();
        }
        assert_eq!(out, bytes);
        assert_eq!(seen.last().copied(), Some(4096));
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
    }

    #[test]
    fn counting_reader_totals_start_from_the_resume_offset() {
        let mut seen = Vec::new();
        let mut sunk = Vec::new();
        {
            let mut reader = CountingReader::new(Cursor::new(vec![7_u8; 10]), 90, |done| {
                seen.push(done);
            });
            let mut buf = [0_u8; 4];
            while reader.read(&mut buf).unwrap() > 0 {
                sunk.push(buf[0]);
            }
        }
        assert_eq!(seen.last().copied(), Some(100));
        assert_eq!(sunk.len(), 3);
    }

    #[test]
    fn plain_sink_prints_one_line_per_decile_and_ends_with_a_newline() {
        let mut progress = Progress::new(shard(Some(1000)), 0, Sink::Plain);
        let mut out: Vec<u8> = Vec::new();
        for done in (0..=1000).step_by(10) {
            progress.emit(&mut out, done).unwrap();
        }
        progress.finish(&mut out, 1000).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with('\n'), "{text:?}");
        let lines = text.lines().count();
        assert!((2..=11).contains(&lines), "{lines} lines: {text}");
    }

    #[test]
    fn plain_sink_stays_silent_until_a_decile_is_crossed() {
        let mut progress = Progress::new(shard(Some(1000)), 0, Sink::Plain);
        let mut out: Vec<u8> = Vec::new();
        for done in 0..99 {
            progress.emit(&mut out, done).unwrap();
        }
        assert!(out.is_empty(), "{:?}", String::from_utf8(out.clone()));
        progress.emit(&mut out, 100).unwrap();
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 1);
    }
}
