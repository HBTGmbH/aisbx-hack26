//! Live resource usage for a running sandbox, read out of its `/proc` by one `sbx exec` of
//! [`PROBE`] ([`crate::sbx::probe`]). The numbers are the sandbox VM's own, not the host's.

use std::collections::{HashMap, VecDeque};

use crate::model::Sandbox;

/// Widest gap, in seconds of the sandbox's uptime, between two resource readings that still
/// yields a current CPU percentage.
pub const MAX_CPU_INTERVAL: f64 = 30.0;

/// How much of a sandbox's history is kept: this many readings, over at most this many seconds
/// of its uptime.
pub const HISTORY_SAMPLES: usize = 120;
pub const HISTORY_SPAN_S: f64 = 300.0;

/// One shell line of only `grep`, `cat` and `df`, which every sbx template has (no `nproc`).
pub const PROBE: &str = "grep -E '^cpu' /proc/stat; \
                         grep -E '^(MemTotal|MemAvailable):' /proc/meminfo; \
                         cat /proc/uptime; \
                         df -k /";

/// One reading. CPU is cumulative jiffies — a percentage needs two samples, see
/// [`Sample::cpu_percent_since`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub cpus: usize,
    /// Sum of every field on the aggregate `cpu` line.
    pub cpu_total: u64,
    /// The idle + iowait fields of that line.
    pub cpu_idle: u64,
    pub mem_total_kb: u64,
    pub mem_avail_kb: u64,
    pub disk_total_kb: u64,
    pub disk_used_kb: u64,
    pub uptime_s: f64,
}

impl Sample {
    pub fn parse(text: &str) -> Option<Self> {
        let mut cpus = 0usize;
        let mut cpu_total = 0u64;
        let mut cpu_idle = 0u64;
        let mut mem_total_kb = 0u64;
        let mut mem_avail_kb = 0u64;
        let mut uptime_s = None;
        let mut disk = None;

        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("cpu") {
                // "cpu  1 2 3 …" is the aggregate; "cpu0 …", "cpu1 …" are the individual ones.
                if rest.starts_with(char::is_whitespace) {
                    let fields: Vec<u64> = rest
                        .split_whitespace()
                        .filter_map(|f| f.parse::<u64>().ok())
                        .collect();
                    if fields.len() >= 5 {
                        cpu_total = fields.iter().sum();
                        // user nice system idle iowait …
                        cpu_idle = fields[3] + fields[4];
                    }
                } else if rest.starts_with(|c: char| c.is_ascii_digit()) {
                    cpus += 1;
                }
            } else if let Some(rest) = line.strip_prefix("MemTotal:") {
                mem_total_kb = first_number(rest).unwrap_or(0);
            } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                mem_avail_kb = first_number(rest).unwrap_or(0);
            } else if uptime_s.is_none() && line.starts_with(|c: char| c.is_ascii_digit()) {
                // `/proc/uptime`: "505.37 5046.83"
                uptime_s = line.split_whitespace().next().and_then(|f| f.parse().ok());
            } else if disk.is_none() && line.contains('%') {
                // The `df -k /` body row: "overlay 20466256 320668 19080628 2% /"
                let fields: Vec<&str> = line.split_whitespace().collect();
                if let Some(pos) = fields.iter().position(|f| f.ends_with('%')) {
                    if pos >= 3 {
                        let total = fields[pos - 3].parse().ok();
                        let used = fields[pos - 2].parse().ok();
                        if let (Some(total), Some(used)) = (total, used) {
                            disk = Some((total, used));
                        }
                    }
                }
            }
        }

        let (disk_total_kb, disk_used_kb) = disk?;
        if cpu_total == 0 || mem_total_kb == 0 {
            return None;
        }
        Some(Self {
            cpus: cpus.max(1),
            cpu_total,
            cpu_idle,
            mem_total_kb,
            mem_avail_kb,
            disk_total_kb,
            disk_used_kb,
            uptime_s: uptime_s.unwrap_or(0.0),
        })
    }

    /// Busy share of all CPUs between `earlier` and this sample, 0.0–1.0. `None` when the two
    /// samples are the same reading, or when the sandbox restarted in between (counters reset).
    pub fn cpu_percent_since(&self, earlier: &Self) -> Option<f32> {
        let total = self.cpu_total.checked_sub(earlier.cpu_total)?;
        let idle = self.cpu_idle.checked_sub(earlier.cpu_idle)?;
        if total == 0 {
            return None;
        }
        Some(((total - idle.min(total)) as f32 / total as f32).clamp(0.0, 1.0))
    }

    pub fn mem_used_kb(&self) -> u64 {
        self.mem_total_kb.saturating_sub(self.mem_avail_kb)
    }

    pub fn mem_fraction(&self) -> f32 {
        fraction(self.mem_used_kb(), self.mem_total_kb)
    }

    pub fn disk_fraction(&self) -> f32 {
        fraction(self.disk_used_kb, self.disk_total_kb)
    }
}

fn fraction(used: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    (used as f32 / total as f32).clamp(0.0, 1.0)
}

fn first_number(s: &str) -> Option<u64> {
    s.split_whitespace().next()?.parse().ok()
}

/// Are two readings close enough together for a CPU percentage between them?
pub fn within_interval(prev: &Sample, next: &Sample) -> bool {
    (0.0..=MAX_CPU_INTERVAL).contains(&(next.uptime_s - prev.uptime_s))
}

/// The busy share of all CPUs between two consecutive readings, 0.0 to 1.0: `None` when they are
/// too far apart (or out of order), the same reading, or across a restart.
pub fn cpu_between(prev: &Sample, next: &Sample) -> Option<f32> {
    match within_interval(prev, next) {
        true => next.cpu_percent_since(prev),
        false => None,
    }
}

/// How full a meter is, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Normal,
    High,
    Critical,
}

/// ≥ 0.9 is critical, ≥ 0.7 high, anything below normal.
pub fn level(fraction: f32) -> Level {
    match fraction {
        f if f >= 0.9 => Level::Critical,
        f if f >= 0.7 => Level::High,
        _ => Level::Normal,
    }
}

/// One reading in a sandbox's history.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub uptime_s: f64,
    /// The CPU share since the reading before, where [`cpu_between`] gives one.
    pub cpu: Option<f32>,
    pub mem_used_kb: u64,
    pub mem_total_kb: u64,
    /// The reading before is too far away, or the counters went back: a chart starts a new
    /// segment here rather than draw a line across the gap.
    pub gap_before: bool,
}

impl Point {
    /// The share of memory in use, 0.0–1.0.
    pub fn mem_fraction(&self) -> f32 {
        fraction(self.mem_used_kb, self.mem_total_kb)
    }
}

/// One sandbox's recent readings, oldest first.
#[derive(Debug, Default)]
pub struct History {
    points: VecDeque<Point>,
    last: Option<Sample>,
}

impl History {
    pub fn points(&self) -> impl Iterator<Item = &Point> {
        self.points.iter()
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    fn record(&mut self, sample: &Sample) {
        let (cpu, gap_before) = match &self.last {
            Some(prev) => {
                let reset = sample.cpu_total < prev.cpu_total || sample.cpu_idle < prev.cpu_idle;
                // Uptime going back is a restart: the old readings are on another timeline.
                if sample.uptime_s < prev.uptime_s {
                    self.points.clear();
                }
                let gap = reset || !within_interval(prev, sample);
                (cpu_between(prev, sample), gap)
            }
            None => (None, false),
        };
        self.points.push_back(Point {
            uptime_s: sample.uptime_s,
            cpu,
            mem_used_kb: sample.mem_used_kb(),
            mem_total_kb: sample.mem_total_kb,
            gap_before,
        });
        self.last = Some(*sample);
        while self.points.len() > HISTORY_SAMPLES {
            self.points.pop_front();
        }
        let oldest = sample.uptime_s - HISTORY_SPAN_S;
        while self.points.front().is_some_and(|p| p.uptime_s < oldest) {
            self.points.pop_front();
        }
    }
}

/// The histories behind the Resources tab's charts, one per sandbox name. They survive a selection
/// away and back, unlike the tab's own data.
#[derive(Debug, Default)]
pub struct Histories {
    rings: HashMap<String, History>,
}

impl Histories {
    pub fn record(&mut self, name: &str, sample: &Sample) {
        self.rings
            .entry(name.to_string())
            .or_default()
            .record(sample);
    }

    pub fn get(&self, name: &str) -> Option<&History> {
        self.rings.get(name)
    }

    /// Drop the history of every sandbox `sbx ls` no longer lists as running.
    pub fn retain_running(&mut self, sandboxes: &[Sandbox]) {
        self.rings
            .retain(|name, _| sandboxes.iter().any(|s| s.running && &s.name == name));
    }
}

/// KiB -> "15.8 GB": binary units under the "GB" label, as sbx writes them, to one decimal.
pub fn gb(kb: u64) -> String {
    format!("{:.1} GB", kb as f64 / 1024.0 / 1024.0)
}

/// Seconds → the compact "3m", "2h 5m", "4d 1h" spelling `sbx inspect` uses.
pub fn duration(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86400,
        (secs % 86400) / 3600,
        (secs % 3600) / 60,
        secs % 60,
    );
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m {s}s"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim output of [`PROBE`] from a claude sandbox.
    const REAL: &str = "\
cpu  34 0 28 9631 9 0 18 0 0 0
cpu0 1 0 12 956 0 0 12 0 0 0
cpu1 0 0 0 968 0 0 0 0 0 0
MemTotal:       16595744 kB
MemAvailable:   16237568 kB
505.37 5046.83
Filesystem     1K-blocks   Used Available Use% Mounted on
overlay         20466256 320668  19080628   2% /
";

    #[test]
    fn parses_a_real_probe() {
        let s = Sample::parse(REAL).unwrap();
        assert_eq!(s.cpus, 2);
        assert_eq!(s.cpu_total, 34 + 28 + 9631 + 9 + 18);
        assert_eq!(s.cpu_idle, 9631 + 9);
        assert_eq!(s.mem_total_kb, 16_595_744);
        assert_eq!(s.mem_used_kb(), 16_595_744 - 16_237_568);
        assert_eq!(s.disk_total_kb, 20_466_256);
        assert_eq!(s.disk_used_kb, 320_668);
        assert!((s.uptime_s - 505.37).abs() < 0.01);
        assert_eq!(gb(s.mem_total_kb), "15.8 GB");
    }

    #[test]
    fn cpu_percent_needs_two_distinct_samples() {
        let a = Sample::parse(REAL).unwrap();
        assert_eq!(a.cpu_percent_since(&a), None);

        let mut b = a;
        b.cpu_total += 200;
        b.cpu_idle += 150;
        assert_eq!(b.cpu_percent_since(&a), Some(0.25));

        let mut restarted = a;
        restarted.cpu_total = 5;
        restarted.cpu_idle = 1;
        assert_eq!(restarted.cpu_percent_since(&a), None);
    }

    #[test]
    fn rejects_output_that_is_missing_a_section() {
        assert!(Sample::parse("").is_none());
        assert!(Sample::parse("MemTotal: 1 kB\n").is_none());
        let no_df = REAL.lines().take(6).collect::<Vec<_>>().join("\n");
        assert!(Sample::parse(&no_df).is_none());
    }

    /// `REAL`, `secs` later, with `busy` of `total` new jiffies busy.
    fn later(prev: &Sample, secs: f64, total: u64, busy: u64) -> Sample {
        let mut next = *prev;
        next.uptime_s += secs;
        next.cpu_total += total;
        next.cpu_idle += total - busy;
        next
    }

    #[test]
    fn cpu_between_needs_two_readings_close_together() {
        let a = Sample::parse(REAL).unwrap();
        assert_eq!(cpu_between(&a, &later(&a, 2.5, 100, 25)), Some(0.25));
        assert_eq!(
            cpu_between(&a, &later(&a, MAX_CPU_INTERVAL, 100, 50)),
            Some(0.5)
        );
        assert_eq!(
            cpu_between(&a, &later(&a, 30.5, 100, 25)),
            None,
            "too far apart"
        );
        assert_eq!(cpu_between(&later(&a, 5.0, 0, 0), &a), None, "out of order");
        assert_eq!(cpu_between(&a, &a), None, "one jiffy");
    }

    #[test]
    fn levels_turn_at_seventy_and_ninety_percent() {
        assert_eq!(level(0.69), Level::Normal);
        assert_eq!(level(0.7), Level::High);
        assert_eq!(level(0.89), Level::High);
        assert_eq!(level(0.9), Level::Critical);
    }

    fn points(h: &Histories, name: &str) -> Vec<Point> {
        h.get(name).unwrap().points().copied().collect()
    }

    #[test]
    fn a_history_takes_cpu_from_consecutive_readings() {
        let a = Sample::parse(REAL).unwrap();
        let mut h = Histories::default();
        let b = later(&a, 2.5, 100, 25);
        let c = later(&b, 2.5, 100, 75);
        for s in [a, b, c] {
            h.record("one", &s);
        }
        let got = points(&h, "one");
        let cpu: Vec<Option<f32>> = got.iter().map(|p| p.cpu).collect();
        assert_eq!(cpu, [None, Some(0.25), Some(0.75)]);
        assert!(got.iter().all(|p| !p.gap_before));
        assert_eq!(got[2].mem_used_kb, c.mem_used_kb());
        assert_eq!(got[2].mem_total_kb, c.mem_total_kb);
        assert_eq!(got[1].uptime_s, b.uptime_s);
        h.record("two", &a);
        assert_eq!(
            (h.get("one").unwrap().len(), h.get("two").unwrap().len()),
            (3, 1)
        );
        assert!(h.get("three").is_none());
    }

    #[test]
    fn a_history_keeps_120_readings_over_at_most_300_seconds() {
        let mut s = Sample::parse(REAL).unwrap();
        let mut h = Histories::default();
        for _ in 0..200 {
            s = later(&s, 1.0, 100, 10);
            h.record("one", &s);
        }
        assert_eq!(h.get("one").unwrap().len(), HISTORY_SAMPLES);
        let mut h = Histories::default();
        for _ in 0..40 {
            s = later(&s, 10.0, 100, 10);
            h.record("one", &s);
        }
        let got = points(&h, "one");
        let span = got.last().unwrap().uptime_s - got[0].uptime_s;
        assert!(span <= HISTORY_SPAN_S, "{span}");
        assert_eq!(got.len(), 31);
    }

    #[test]
    fn a_gap_or_a_restart_starts_a_new_segment() {
        let a = Sample::parse(REAL).unwrap();
        let mut h = Histories::default();
        let away = later(&a, MAX_CPU_INTERVAL + 1.0, 100, 50);
        let back = later(&away, 2.5, 100, 50);
        for s in [a, away, back] {
            h.record("one", &s);
        }
        let got = points(&h, "one");
        assert_eq!((got[1].gap_before, got[1].cpu), (true, None));
        assert_eq!((got[2].gap_before, got[2].cpu), (false, Some(0.5)));

        let mut reset = later(&back, 2.5, 0, 0);
        reset.cpu_total = 10;
        reset.cpu_idle = 5;
        h.record("one", &reset);
        let last = *points(&h, "one").last().unwrap();
        assert_eq!((last.gap_before, last.cpu), (true, None));

        let mut restarted = reset;
        restarted.uptime_s = 3.0;
        h.record("one", &restarted);
        let got = points(&h, "one");
        assert_eq!(got.len(), 1);
        assert!(got[0].gap_before && got[0].cpu.is_none());
    }

    #[test]
    fn a_history_is_dropped_once_its_sandbox_stops_or_goes() {
        let a = Sample::parse(REAL).unwrap();
        let mut h = Histories::default();
        for name in ["up", "stopped", "gone"] {
            h.record(name, &a);
        }
        let mut up = crate::model::sample("up", Some("claude-anthropic"), &[]);
        up.running = true;
        let stopped = crate::model::sample("stopped", Some("claude-anthropic"), &[]);
        h.retain_running(&[up, stopped]);
        assert!(h.get("up").is_some());
        assert!(h.get("stopped").is_none() && h.get("gone").is_none());
    }

    #[test]
    fn formats_sizes_and_durations() {
        assert_eq!(gb(1024 * 1024), "1.0 GB");
        assert_eq!(gb(20 * 1024 * 1024), "20.0 GB");
        assert_eq!(gb(20_466_256), "19.5 GB");
        assert_eq!(duration(45), "45s");
        assert_eq!(duration(185), "3m 5s");
        assert_eq!(duration(7_260), "2h 1m");
        assert_eq!(duration(360_000), "4d 4h");
    }
}
