// SPDX-License-Identifier: MIT
//! Opt-in frame timing. Set `TB520FU_PERF=1` in the environment and every second in which something
//! was drawn prints one line per measured span to stderr (journal), e.g.
//!
//! ```text
//! perf: present[3]   n=118 avg=7.9ms max=24.1ms  >8.3ms:40 >16.7ms:3
//! ```
//!
//! `>8.3ms` / `>16.7ms` count the samples that took longer than one 120 Hz / one 60 Hz refresh.
//! Costs nothing when the variable is unset.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Default, Clone, Copy)]
struct Acc {
    n: u32,
    sum: Duration,
    max: Duration,
    over_120: u32,
    over_60: u32,
}

struct State {
    gauges: BTreeMap<String, (u32, u64, u64)>,
    spans: BTreeMap<String, Acc>,
    last_seen: BTreeMap<String, Instant>,
    last_flush: Instant,
}

const FRAME_120: Duration = Duration::from_micros(8_333);
const FRAME_60: Duration = Duration::from_micros(16_667);

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("TB520FU_PERF").map(|v| v != "0" && !v.is_empty()).unwrap_or(false))
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(State {
            gauges: BTreeMap::new(),
            spans: BTreeMap::new(),
            last_seen: BTreeMap::new(),
            last_flush: Instant::now(),
        })
    })
}

/// `Some(now)` when measuring, so call sites pay for the clock only when enabled.
pub fn start() -> Option<Instant> {
    enabled().then(Instant::now)
}

/// Record the time since `t0` (from [`start`]) under `name`.
pub fn end(name: &str, t0: Option<Instant>) {
    if let Some(t0) = t0 {
        record(name, t0.elapsed());
    }
}

pub fn record(name: &str, d: Duration) {
    if !enabled() {
        return;
    }
    let mut st = state().lock().unwrap();
    let acc = st.spans.entry(name.to_string()).or_default();
    acc.n += 1;
    acc.sum += d;
    acc.max = acc.max.max(d);
    if d > FRAME_120 {
        acc.over_120 += 1;
    }
    if d > FRAME_60 {
        acc.over_60 += 1;
    }
}

/// Record the gap since the previous `tick` with the same name (frame-to-frame interval).
/// Gaps over 100 ms are a pause between animations, not a slow frame, and are dropped.
pub fn tick(name: &str) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let prev = state().lock().unwrap().last_seen.insert(name.to_string(), now);
    if let Some(prev) = prev {
        let gap = now - prev;
        if gap < Duration::from_millis(100) {
            record(name, gap);
        }
    }
}

/// Record a plain number (instance count, pixels, ...) under `name`; printed as avg / max.
pub fn gauge(name: &str, v: u64) {
    if !enabled() {
        return;
    }
    let mut st = state().lock().unwrap();
    let g = st.gauges.entry(name.to_string()).or_insert((0, 0, 0));
    g.0 += 1;
    g.1 += v;
    g.2 = g.2.max(v);
}

/// With `TB520FU_PERF_SYNC=1` call sites wait for the GPU right after submitting, so the wait can be
/// recorded as the frame's GPU time. Changes the timing it measures; for diagnosis only.
pub fn sync_gpu() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| enabled() && std::env::var("TB520FU_PERF_SYNC").map(|v| v == "1").unwrap_or(false))
}

/// Print and reset the accumulated spans, at most once a second.
pub fn flush_if_due() {
    if !enabled() {
        return;
    }
    let mut st = state().lock().unwrap();
    if st.last_flush.elapsed() < Duration::from_secs(1) {
        return;
    }
    st.last_flush = Instant::now();
    let spans = std::mem::take(&mut st.spans);
    let gauges = std::mem::take(&mut st.gauges);
    drop(st);
    for (name, (n, sum, max)) in gauges {
        if n > 0 {
            eprintln!("perf: {name:<18} n={n:<4} avg={} max={max}", sum / n as u64);
        }
    }
    for (name, a) in spans {
        if a.n == 0 {
            continue;
        }
        let avg = a.sum.as_secs_f64() * 1000.0 / a.n as f64;
        eprintln!(
            "perf: {name:<18} n={:<4} avg={avg:.1}ms max={:.1}ms  >8.3ms:{} >16.7ms:{}",
            a.n,
            a.max.as_secs_f64() * 1000.0,
            a.over_120,
            a.over_60
        );
    }
}
