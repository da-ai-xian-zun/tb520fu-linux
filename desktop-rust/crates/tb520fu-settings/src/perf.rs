// SPDX-License-Identifier: GPL-3.0-only
use std::time::Instant;

pub struct FramePerf {
    last: Option<Instant>,
    iv: Vec<f32>,
}

impl FramePerf {
    pub fn new() -> Self {
        Self {
            last: None,
            iv: Vec::new(),
        }
    }

    pub fn record_frame(&mut self) {
        let now = Instant::now();
        if let Some(prev) = self.last {
            let ms = now.duration_since(prev).as_secs_f32() * 1000.0;
            if ms > 100.0 {
                self.flush();
            } else {
                self.iv.push(ms);
                if self.iv.len() >= 240 {
                    self.flush();
                }
            }
        }
        self.last = Some(now);
    }

    pub fn flush(&mut self) {
        if self.iv.len() >= 8 {
            let n = self.iv.len();
            let sum: f32 = self.iv.iter().sum();
            let max = self.iv.iter().cloned().fold(0.0, f32::max);
            let over9 = self.iv.iter().filter(|&&x| x > 9.0).count();
            let over17 = self.iv.iter().filter(|&&x| x > 16.7).count();
            let mut s = self.iv.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let p50 = s[n / 2];
            let p99 = s[(n * 99 / 100).min(n - 1)];
            eprintln!(
                "PERF settings burst frames={} avg={:.2}ms p50={:.2} p99={:.2} max={:.2} >9ms={} >16.7ms={}",
                n + 1,
                sum / n as f32,
                p50,
                p99,
                max,
                over9,
                over17
            );
        }
        self.iv.clear();
    }
}
