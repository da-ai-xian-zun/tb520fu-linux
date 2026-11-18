// SPDX-License-Identifier: MIT
use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use libloading::Library;

const RATE: usize = 48_000;
const CHUNK_FRAMES: usize = 256; // ~5.3 ms mixer quantum

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum ClickKind {
    Char = 0,
    Space = 1,
    Del = 2,
    Enter = 3,
    Tool = 4,
    Expand = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum SoundStyle {
    Standard = 0,
    Soft = 1,
    Mechanical = 2,
}

impl SoundStyle {
    pub fn from_name(s: &str) -> Self {
        match s {
            "soft" => Self::Soft,
            "mech" => Self::Mechanical,
            _ => Self::Standard,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "std",
            Self::Soft => "soft",
            Self::Mechanical => "mech",
        }
    }
}

fn db_to_gain(db: f64) -> f32 {
    (10.0_f64.powf(db / 20.0)) as f32
}

/// OriginOS "标准" / "柔和": 6- or 8-cycle sine/triangle burst with fast linear attack and two-step decay.
fn sine_burst(f: f64, db: f64, cycles: usize, triangle: bool) -> Vec<f32> {
    let period = 1.0 / f;
    let end = (cycles as f64) * period;
    let n = (end * (RATE as f64)).ceil() as usize + 2;
    let amp = db_to_gain(db) as f64;
    let mut out = vec![0.0_f32; n];
    for (i, sample) in out.iter_mut().enumerate() {
        let t = (i as f64) / (RATE as f64);
        let env = if t < 1e-4 {
            t / 1e-4
        } else if t < period {
            1.0 - 0.29 * (t - 1e-4) / (period - 1e-4)
        } else if t < end - period {
            0.71 - 0.21 * (t - period) / (end - 2.0 * period).max(1e-6)
        } else if t < end {
            0.5 * (end - t) / period
        } else {
            0.0
        };
        let ph = (t * f).rem_euclid(1.0);
        let w = if triangle {
            if ph < 0.5 { 4.0 * ph - 1.0 } else { 3.0 - 4.0 * ph }
        } else {
            (std::f64::consts::TAU * f * t).sin()
        };
        *sample = (amp * env * w) as f32;
    }
    out
}

/// Woodblock mallet for toolbar buttons and candidate expander.
fn mallet(f: f64, tau: f64, db: f64, partial_db: f64) -> Vec<f32> {
    let n = ((tau * 4.6 + 0.004) * (RATE as f64)) as usize;
    let amp = db_to_gain(db) as f64;
    let p_amp = amp * (db_to_gain(partial_db) as f64);
    let attack = 1.5 / f;
    let mut out = vec![0.0_f32; n];
    for (i, sample) in out.iter_mut().enumerate() {
        let t = (i as f64) / (RATE as f64);
        let a = if t < attack { t / attack } else { 1.0 };
        let d1 = if t < attack { 1.0 } else { (-(t - attack) / tau).exp() };
        let d2 = if t < attack { 1.0 } else { (-(t - attack) / (tau * 0.5)).exp() };
        let s = (std::f64::consts::TAU * f * t).sin() * amp * d1
            + (std::f64::consts::TAU * 4.0 * f * t).sin() * p_amp * d2;
        *sample = (a * s) as f32;
    }
    out
}

/// Mechanical switch: RBJ band-passed noise click + short low thump.
fn mech_click(band: f64, thump: f64, mut seed: u32) -> Vec<f32> {
    let n = (0.036 * (RATE as f64)) as usize;
    let mut out = vec![0.0_f32; n];
    let w0 = std::f64::consts::TAU * band / (RATE as f64);
    let q = 0.9;
    let alpha = w0.sin() / (2.0 * q);
    let (b0, b2, a0, a1, a2) = (alpha, -alpha, 1.0 + alpha, -2.0 * w0.cos(), 1.0 - alpha);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    let nn = (0.012 * (RATE as f64)) as usize;
    for s in out.iter_mut().take(nn) {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let i_f = 0.0;
        let _ = i_f;
        let x = (seed as f64) / 4_294_967_295.0 * 2.0 - 1.0;
        let y = (b0 * x + b2 * x2 - a1 * y1 - a2 * y2) / a0;
        x2 = x1;
        x1 = x;
        y2 = y1;
        y1 = y;
        *s += (0.5 * y) as f32;
    }
    for (i, s) in out.iter_mut().take(nn).enumerate() {
        *s *= (-(i as f32) / ((nn as f32) / 5.0)).exp();
    }
    for (i, s) in out.iter_mut().enumerate() {
        let t = (i as f64) / (RATE as f64);
        *s += (0.18 * (-t / 0.0065).exp() * (std::f64::consts::TAU * thump * t).sin()) as f32;
    }
    out
}

fn build_sound_bank() -> [[[Vec<f32>; 6]; 3]; 1] {
    let tones = [(2093.0, -14.7), (1568.0, -14.6), (1760.0, -16.9), (1109.0, -14.8)];
    let bands = [4200.0, 2600.0, 4200.0, 4200.0];
    let thumps = [210.0, 150.0, 210.0, 170.0];
    let tool = mallet(1174.7, 0.009, -12.8, -17.0);
    let expand = mallet(784.0, 0.0115, -10.4, -15.0);

    let mk_style = |style: SoundStyle| -> [Vec<f32>; 6] {
        let mut arr: [Vec<f32>; 6] = Default::default();
        for k in 0..4 {
            arr[k] = match style {
                SoundStyle::Standard => sine_burst(tones[k].0, tones[k].1, 6, false),
                SoundStyle::Soft => sine_burst(tones[k].0 * 0.5, tones[k].1 - 2.0, 8, true),
                SoundStyle::Mechanical => mech_click(bands[k], thumps[k], 0x0123_4567 + (k as u32) * 977),
            };
        }
        arr[4] = tool.clone();
        arr[5] = expand.clone();
        arr
    };

    [[
        mk_style(SoundStyle::Standard),
        mk_style(SoundStyle::Soft),
        mk_style(SoundStyle::Mechanical),
    ]]
}

// ---- Runtime dlopen wrapper around libpulse-simple.so.0 (served by pipewire-pulse, zero build-time C deps!) ----

#[repr(C)]
struct PaSampleSpec {
    format: c_int, // PA_SAMPLE_FLOAT32LE = 5
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct PaBufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

struct PulseSimple {
    _lib: Library,
    handle: *mut c_void,
    fn_write: unsafe extern "C" fn(*mut c_void, *const c_void, usize, *mut c_int) -> c_int,
    fn_free: unsafe extern "C" fn(*mut c_void),
}

impl PulseSimple {
    fn open() -> Option<Self> {
        unsafe {
            let lib = Library::new("libpulse-simple.so.0").ok()?;
            type FnNew = unsafe extern "C" fn(
                *const c_char,
                *const c_char,
                c_int,
                *const c_char,
                *const c_char,
                *const PaSampleSpec,
                *const c_void,
                *const PaBufferAttr,
                *mut c_int,
            ) -> *mut c_void;
            let fn_new: FnNew = *lib.get(b"pa_simple_new\0").ok()?;
            let fn_write = *lib.get(b"pa_simple_write\0").ok()?;
            let fn_free = *lib.get(b"pa_simple_free\0").ok()?;

            let app = CString::new("TB520FU OSK").unwrap();
            let stream = CString::new("key-clicks").unwrap();
            let ss = PaSampleSpec {
                format: 5, // PA_SAMPLE_FLOAT32LE
                rate: RATE as u32,
                channels: 1,
            };
            let attr = PaBufferAttr {
                maxlength: 4096,
                tlength: (CHUNK_FRAMES * 4 * 2) as u32,
                prebuf: u32::MAX,
                minreq: u32::MAX,
                fragsize: u32::MAX,
            };
            let mut err: c_int = 0;
            let handle = fn_new(
                std::ptr::null(),
                app.as_ptr(),
                1, // PA_STREAM_PLAYBACK
                std::ptr::null(),
                stream.as_ptr(),
                &ss,
                std::ptr::null(),
                &attr,
                &mut err,
            );
            if handle.is_null() {
                return None;
            }
            Some(Self {
                _lib: lib,
                handle,
                fn_write,
                fn_free,
            })
        }
    }

    fn write_f32(&self, samples: &[f32]) -> bool {
        let mut err: c_int = 0;
        let rc = unsafe {
            (self.fn_write)(
                self.handle,
                samples.as_ptr() as *const c_void,
                std::mem::size_of_val(samples),
                &mut err,
            )
        };
        rc == 0
    }
}

impl Drop for PulseSimple {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { (self.fn_free)(self.handle) };
        }
    }
}

pub struct SoundEngine {
    tx: mpsc::SyncSender<ClickKind>,
    vol: Arc<AtomicU8>,
    style: Arc<AtomicU8>,
}

impl SoundEngine {
    pub fn new(initial_vol: u8, initial_style: SoundStyle) -> Self {
        let (tx, rx) = mpsc::sync_channel::<ClickKind>(32);
        let vol = Arc::new(AtomicU8::new(initial_vol.min(10)));
        let style = Arc::new(AtomicU8::new(initial_style as u8));
        let vol_bg = Arc::clone(&vol);
        let style_bg = Arc::clone(&style);

        std::thread::Builder::new()
            .name("tb520fu-osk-sfx".into())
            .spawn(move || mixer_loop(rx, vol_bg, style_bg))
            .ok();

        Self { tx, vol, style }
    }

    pub fn set_volume(&self, level: u8) {
        self.vol.store(level.min(10), Ordering::Relaxed);
    }

    pub fn set_style(&self, style: SoundStyle) {
        self.style.store(style as u8, Ordering::Relaxed);
    }

    pub fn play(&self, kind: ClickKind) {
        if self.vol.load(Ordering::Relaxed) == 0 {
            return;
        }
        let _ = self.tx.try_send(kind);
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    style: usize,
    kind: usize,
    pos: f64,
    gain: f32,
    rate: f64,
}

fn mixer_loop(rx: mpsc::Receiver<ClickKind>, vol: Arc<AtomicU8>, style: Arc<AtomicU8>) {
    let [bank] = build_sound_bank();
    let mut voices = [Voice::default(); 8];
    let mut pa: Option<PulseSimple> = None;
    let mut last_key_at = Instant::now();
    let mut seed: u32 = 0x9E37_79B9;
    let mut mix_buf = [0.0_f32; CHUNK_FRAMES];

    let mut next_rnd = || -> f32 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32) / 4_294_967_295.0
    };

    loop {
        let any_active = voices.iter().any(|v| v.active);
        if !any_active {
            // Wait up to 10s while stream is open; after 10s close stream so aw88399 amps power down!
            let timeout = if pa.is_some() {
                Duration::from_secs(10).saturating_sub(last_key_at.elapsed())
            } else {
                Duration::from_secs(3600)
            };
            match rx.recv_timeout(timeout) {
                Ok(kind) => {
                    let v = vol.load(Ordering::Relaxed);
                    if v > 0 {
                        if pa.is_none() {
                            pa = PulseSimple::open();
                        }
                        last_key_at = Instant::now();
                        let st = (style.load(Ordering::Relaxed) as usize).min(2);
                        voices[0] = Voice {
                            active: true,
                            style: st,
                            kind: kind as usize,
                            pos: 0.0,
                            gain: (v as f32 / 10.0) * (0.8 + next_rnd() * 0.4),
                            rate: 0.991 + (next_rnd() as f64) * 0.018,
                        };
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Drop PulseSimple stream so speaker path sleeps cleanly
                    pa = None;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            continue;
        }

        // Drain any concurrent clicks arrived during previous 5ms chunk
        while let Ok(kind) = rx.try_recv() {
            let v = vol.load(Ordering::Relaxed);
            if v == 0 {
                continue;
            }
            last_key_at = Instant::now();
            let st = (style.load(Ordering::Relaxed) as usize).min(2);
            let slot_idx = voices
                .iter()
                .position(|vo| !vo.active)
                .unwrap_or_else(|| {
                    voices
                        .iter()
                        .enumerate()
                        .max_by(|a, b| a.1.pos.partial_cmp(&b.1.pos).unwrap())
                        .map(|(i, _)| i)
                        .unwrap_or(0)
                });
            voices[slot_idx] = Voice {
                active: true,
                style: st,
                kind: kind as usize,
                pos: 0.0,
                gain: (v as f32 / 10.0) * (0.8 + next_rnd() * 0.4),
                rate: 0.991 + (next_rnd() as f64) * 0.018,
            };
        }

        mix_buf.fill(0.0);
        for vo in &mut voices {
            if !vo.active {
                continue;
            }
            let wave = &bank[vo.style][vo.kind];
            let len = wave.len();
            for dst in &mut mix_buf {
                let i0 = vo.pos as usize;
                if i0 + 1 >= len {
                    vo.active = false;
                    break;
                }
                let frac = (vo.pos - (i0 as f64)) as f32;
                *dst += vo.gain * (wave[i0] + (wave[i0 + 1] - wave[i0]) * frac);
                vo.pos += vo.rate;
            }
        }
        for dst in &mut mix_buf {
            *dst = dst.clamp(-1.0, 1.0);
        }
        if let Some(stream) = &pa {
            if !stream.write_f32(&mix_buf) {
                pa = None;
            }
        } else {
            // No audio daemon running (e.g. headless preview): drain voices immediately
            for vo in &mut voices {
                vo.active = false;
            }
        }
    }
}
