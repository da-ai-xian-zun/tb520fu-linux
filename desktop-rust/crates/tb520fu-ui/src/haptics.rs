// SPDX-License-Identifier: MIT
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

const EV_FF: u16 = 0x15;
const FF_RUMBLE: u16 = 0x50;
// _IOW('E', 0x80, struct ff_effect) -> size = 48 (0x30) on 64-bit Linux (`aarch64` & `x86_64`)
const EVIOCSFF: libc::c_ulong = 0x4030_4580;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FfTrigger {
    button: u16,
    interval: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FfReplay {
    length: u16,
    delay: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FfRumbleEffect {
    strong_magnitude: u16,
    weak_magnitude: u16,
}

/// Union `u` inside Linux `struct ff_effect` is 8-byte aligned and 32 bytes wide on LP64.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
union FfEffectUnion {
    rumble: FfRumbleEffect,
    _pad: [u64; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FfEffect {
    ty: u16,
    id: i16,
    direction: u16,
    trigger: FfTrigger,
    replay: FfReplay,
    // 2-byte implicit padding here brings `u` to offset 16, total size = 48 bytes
    u: FfEffectUnion,
}

#[repr(C)]
struct RawInputEvent {
    tv_sec: libc::time_t,
    tv_usec: libc::suseconds_t,
    ty: u16,
    code: u16,
    value: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HapticSide {
    Left,
    Right,
    Both,
}

impl HapticSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Both => "both",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s {
            "left" => Self::Left,
            "right" => Self::Right,
            _ => Self::Both,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HapticKind {
    KeyTap,
    KeySpace,
    KeyBackspace,
    KeyEnter,
    ToggleSwitch,
    SliderTick,
    LongPress,
}

#[allow(non_upper_case_globals)]
impl HapticKind {
    pub const Char: Self = Self::KeyTap;
    pub const Space: Self = Self::KeySpace;
    pub const Del: Self = Self::KeyBackspace;
    pub const Enter: Self = Self::KeyEnter;
    pub const Tool: Self = Self::ToggleSwitch;
    pub const Repeat: Self = Self::SliderTick;
    pub const Heavy: Self = Self::LongPress;
    pub const Tick: Self = Self::SliderTick;
    pub const Light: Self = Self::KeyTap;
    pub const Medium: Self = Self::ToggleSwitch;

    pub fn params(self, level: u8) -> (u16, u16) {
        // level: 1..=5 (default 3)
        let scale = match level.clamp(1, 5) {
            1 => 0.45_f32,
            2 => 0.70,
            3 => 1.00,
            4 => 1.32,
            _ => 1.65,
        };
        let (base_mag, dur_ms): (f32, u16) = match self {
            Self::KeyTap => (16500.0, 9),
            Self::KeySpace => (20000.0, 11),
            Self::KeyBackspace => (14500.0, 8),
            Self::KeyEnter => (25500.0, 14),
            Self::ToggleSwitch => (22000.0, 12),
            Self::SliderTick => (10500.0, 6),
            Self::LongPress => (31000.0, 18),
        };
        let mag = (base_mag * scale).round().clamp(2000.0, 65535.0) as u16;
        (mag, dur_ms)
    }
}

#[derive(Clone, Copy)]
struct EffectSlot {
    mag: u16,
    dur_ms: u16,
    effect_id: i16,
}

struct MotorDev {
    fd: OwnedFd,
    slots: Vec<EffectSlot>,
    next_evict: usize,
}

impl MotorDev {
    fn open(path: &PathBuf) -> Option<Self> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?;
        Some(Self {
            fd: OwnedFd::from(f),
            slots: Vec::with_capacity(12),
            next_evict: 0,
        })
    }

    fn rumble(&mut self, mag: u16, dur_ms: u16) {
        if mag == 0 || dur_ms == 0 {
            return;
        }
        let effect_id = if let Some(hit) = self
            .slots
            .iter()
            .find(|s| s.mag == mag && s.dur_ms == dur_ms)
        {
            hit.effect_id
        } else {
            let replace_id = if self.slots.len() < 12 {
                -1i16
            } else {
                let idx = self.next_evict % self.slots.len();
                self.slots[idx].effect_id
            };
            let mut eff = FfEffect {
                ty: FF_RUMBLE,
                id: replace_id,
                direction: 0,
                trigger: FfTrigger::default(),
                replay: FfReplay {
                    length: dur_ms,
                    delay: 0,
                },
                u: FfEffectUnion {
                    rumble: FfRumbleEffect {
                        strong_magnitude: mag,
                        weak_magnitude: mag / 2,
                    },
                },
            };
            let rc = unsafe { libc::ioctl(self.fd.as_raw_fd(), EVIOCSFF as _, &mut eff) };
            if rc < 0 || eff.id < 0 {
                return;
            }
            if self.slots.len() < 12 {
                self.slots.push(EffectSlot {
                    mag,
                    dur_ms,
                    effect_id: eff.id,
                });
            } else {
                let idx = self.next_evict % self.slots.len();
                self.slots[idx] = EffectSlot {
                    mag,
                    dur_ms,
                    effect_id: eff.id,
                };
                self.next_evict = self.next_evict.wrapping_add(1);
            }
            eff.id
        };

        let ev = RawInputEvent {
            tv_sec: 0,
            tv_usec: 0,
            ty: EV_FF,
            code: effect_id as u16,
            value: 1,
        };
        unsafe {
            let _ = libc::write(
                self.fd.as_raw_fd(),
                &ev as *const RawInputEvent as *const libc::c_void,
                std::mem::size_of::<RawInputEvent>(),
            );
        }
    }
}

/// Discover dual `aw86927-haptics` linear motors (`5a` = Left slot 0, `5b` = Right slot 1).
fn discover_aw86927_motors() -> (Option<MotorDev>, Option<MotorDev>) {
    let mut left: Option<MotorDev> = None;
    let mut right: Option<MotorDev> = None;
    let mut fallback: Vec<PathBuf> = Vec::new();

    let Ok(entries) = fs::read_dir("/sys/class/input") else {
        return (None, None);
    };
    for e in entries.flatten() {
        let fname = e.file_name();
        let s = fname.to_string_lossy();
        if !s.starts_with("event") {
            continue;
        }
        let sys_path = e.path();
        let dev_name = fs::read_to_string(sys_path.join("device/name"))
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !dev_name.contains("aw86927") && !dev_name.contains("haptic") {
            continue;
        }
        let dev_node = PathBuf::from(format!("/dev/input/{s}"));
        let hw_link = fs::canonicalize(sys_path.join("device/device"))
            .ok()
            .map(|p| p.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if hw_link.ends_with("5a") || hw_link.contains("-005a") {
            left = MotorDev::open(&dev_node);
        } else if hw_link.ends_with("5b") || hw_link.contains("-005b") {
            right = MotorDev::open(&dev_node);
        } else {
            fallback.push(dev_node);
        }
    }
    for node in fallback {
        if left.is_none() {
            left = MotorDev::open(&node);
        } else if right.is_none() {
            right = MotorDev::open(&node);
        }
    }
    (left, right)
}

#[derive(Clone, Copy)]
struct HapticJob {
    side: HapticSide,
    mag: u16,
    dur_ms: u16,
    allow_daemon_fallback: bool,
}

/// Non-blocking dual-motor `aw86927` spatial haptic driver (ported 1:1 from Y700 `haptics.cpp`).
#[derive(Clone)]
pub struct HapticEngine {
    tx: mpsc::SyncSender<HapticJob>,
    enabled: Arc<AtomicBool>,
    level: Arc<AtomicU8>,
}

impl Default for HapticEngine {
    fn default() -> Self {
        Self::new(true)
    }
}

/// Check sysfs (world-readable, 0 permissions needed) whether this device physically has haptic hardware.
/// Yoga Pad Pro (`TB520FU`) has no vibration motor, so short-circuiting avoids spawning a worker thread
/// or sending useless `/run/tb520fu-daemon.sock` RPCs on every tap!
fn sysfs_has_haptic_hardware() -> bool {
    let Ok(entries) = fs::read_dir("/sys/class/input") else {
        return false;
    };
    for e in entries.flatten() {
        let fname = e.file_name();
        if !fname.to_string_lossy().starts_with("event") {
            continue;
        }
        let dev_name = fs::read_to_string(e.path().join("device/name"))
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if dev_name.contains("aw86927") || dev_name.contains("haptic") {
            return true;
        }
    }
    false
}

impl HapticEngine {
    pub fn global() -> &'static Self {
        static GLOBAL_HAPTICS: OnceLock<HapticEngine> = OnceLock::new();
        GLOBAL_HAPTICS.get_or_init(|| Self::new(true))
    }

    /// `allow_daemon_fallback`: pass `true` in unprivileged desktop apps (`tb520fu-shell`/`tb520fu-osk`),
    /// `false` inside `tb520fu-daemon` itself so daemon never loops back to its own socket!
    pub fn new(allow_daemon_fallback: bool) -> Self {
        let _ = allow_daemon_fallback;
        assert_eq!(std::mem::size_of::<FfEffect>(), 48);
        let has_hw = sysfs_has_haptic_hardware();
        let (tx, rx) = mpsc::sync_channel::<HapticJob>(if has_hw { 8 } else { 1 });
        let enabled = Arc::new(AtomicBool::new(has_hw));
        let level = Arc::new(AtomicU8::new(3));

        if has_hw {
            std::thread::Builder::new()
                .name("tb520fu-haptics".into())
                .spawn(move || {
                    let (mut left, mut right) = discover_aw86927_motors();
                    while let Ok(job) = rx.recv() {
                        if left.is_none() && right.is_none() {
                            // Retry opening direct evdev occasionally, else route via root daemon socket
                            if job.allow_daemon_fallback {
                                send_daemon_haptic(job.side, job.mag, job.dur_ms);
                            }
                            continue;
                        }
                        match job.side {
                            HapticSide::Left => {
                                if let Some(m) = left.as_mut() {
                                    m.rumble(job.mag, job.dur_ms);
                                } else if let Some(m) = right.as_mut() {
                                    m.rumble(job.mag, job.dur_ms);
                                }
                                if let Some(m) = right.as_mut() {
                                    m.rumble(job.mag / 4, (job.dur_ms / 2).max(4));
                                }
                            }
                            HapticSide::Right => {
                                if let Some(m) = right.as_mut() {
                                    m.rumble(job.mag, job.dur_ms);
                                } else if let Some(m) = left.as_mut() {
                                    m.rumble(job.mag, job.dur_ms);
                                }
                                if let Some(m) = left.as_mut() {
                                    m.rumble(job.mag / 4, (job.dur_ms / 2).max(4));
                                }
                            }
                            HapticSide::Both => {
                                if let Some(m) = left.as_mut() {
                                    m.rumble(job.mag, job.dur_ms);
                                }
                                if let Some(m) = right.as_mut() {
                                    m.rumble(job.mag, job.dur_ms);
                                }
                            }
                        }
                    }
                })
                .ok();
        }

        Self { tx, enabled, level }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_level(&self, lv: u8) {
        self.level.store(lv.clamp(1, 5), Ordering::Relaxed);
    }

    pub fn level(&self) -> u8 {
        self.level.load(Ordering::Relaxed)
    }

    pub fn pulse_raw(&self, side: HapticSide, mag: u16, dur_ms: u16, allow_daemon_fallback: bool) {
        if !self.is_enabled() || mag == 0 || dur_ms == 0 {
            return;
        }
        let _ = self.tx.try_send(HapticJob {
            side,
            mag,
            dur_ms,
            allow_daemon_fallback,
        });
    }

    pub fn pulse(&self, side: HapticSide, kind: HapticKind) {
        if !self.is_enabled() {
            return;
        }
        let (mag, dur_ms) = kind.params(self.level());
        self.pulse_raw(side, mag, dur_ms, true);
    }

    /// Automatically route pulse to Left, Right, or Both `aw86927` linear motors based on screen X!
    pub fn pulse_at_x(&self, x: f32, screen_w: f32, kind: HapticKind) {
        let ratio = if screen_w > 1.0 { x / screen_w } else { 0.5 };
        let side = if ratio < 0.40 {
            HapticSide::Left
        } else if ratio > 0.60 {
            HapticSide::Right
        } else {
            HapticSide::Both
        };
        self.pulse(side, kind);
    }

    pub fn pulse_norm(&self, kind: HapticKind, norm_x: f32) {
        self.pulse_at_x(norm_x.clamp(0.0, 1.0), 1.0, kind);
    }
}

fn send_daemon_haptic(side: HapticSide, mag: u16, dur_ms: u16) {
    let Ok(mut s) = UnixStream::connect("/run/tb520fu-daemon.sock") else {
        return;
    };
    let _ = s.set_write_timeout(Some(Duration::from_millis(25)));
    let _ = s.set_read_timeout(Some(Duration::from_millis(25)));
    let msg = format!(
        "{{\"cmd\":\"haptic\",\"side\":\"{}\",\"strength\":{},\"duration_ms\":{}}}\n",
        side.as_str(),
        mag,
        dur_ms
    );
    let _ = s.write_all(msg.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ff_effect_kernel_abi_size() {
        assert_eq!(std::mem::size_of::<FfEffect>(), 48);
    }
}
