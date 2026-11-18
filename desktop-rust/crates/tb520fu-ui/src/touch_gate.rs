// SPDX-License-Identifier: MIT
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const MAGIC: u64 = 0x5442_3532_3047_4154; // "TB520GAT"

/// Lock-free 64-byte shared memory gate between `niri` (writer) and `tb520fu-osk` (reader).
///
/// Lives at `/run/user/$UID/tb520fu-touch-gate`.
/// Prevents false virtual keyboard popups when:
/// 1. The user drags/scrolls a window whose previous focus was a text box (`is_tap == 0`), or
/// 2. Multi-finger gestures occur (`max_fingers > 1`), or
/// 3. The tap landed on a layer-shell panel/bar (`on_layer == 1`), or
/// 4. Focus changed via typing on a physical keyboard (`last_hw_key_ns` newer than `up_ns`).
#[repr(C, align(64))]
pub struct TouchGateRaw {
    pub magic: AtomicU64,
    /// Monotonic nanoseconds when the latest touch sequence began.
    pub down_ns: AtomicU64,
    /// Monotonic nanoseconds when the latest touch sequence lifted (`< down_ns` while finger is down).
    pub up_ns: AtomicU64,
    /// Bit 0: `is_tap` (single finger, moved < 14 logical px, duration < 550 ms).
    /// Bit 1: `on_layer` (touch started on a layer-shell surface rather than an app window).
    /// Bits 8..16: `max_fingers` seen during the sequence.
    pub flags: AtomicU64,
    /// Monotonic nanoseconds of the latest physical (libinput) keyboard press.
    pub last_hw_key_ns: AtomicU64,
    /// Monotonic nanoseconds of the latest Niri heartbeat (so reader knows gate is live).
    pub heartbeat_ns: AtomicU64,
    pub _reserved: [AtomicU64; 2],
}

pub const FLAG_IS_TAP: u64 = 1 << 0;
pub const FLAG_ON_LAYER: u64 = 1 << 1;

#[derive(Debug, Clone, Copy)]
pub struct TouchGateSnapshot {
    pub alive: bool,
    pub down_ns: u64,
    pub up_ns: u64,
    pub is_tap: bool,
    pub on_layer: bool,
    pub max_fingers: u8,
    pub last_hw_key_ns: u64,
    pub now_ns: u64,
}

impl TouchGateSnapshot {
    /// True when a finger is currently touching the screen (wait for lift before deciding).
    pub fn finger_down(&self) -> bool {
        self.alive && self.down_ns > self.up_ns && self.now_ns.saturating_sub(self.down_ns) < 3_000_000_000
    }

    /// Verdict on whether an automatic `ShowVirtualKeyboard` from `fcitx5` should pop up the OSK.
    pub fn allow_auto_show(&self) -> GateVerdict {
        if !self.alive {
            // Gate not present (e.g. unpatched compositor): fail open so keyboard still works.
            return GateVerdict::Allow("gate inactive -> fail open");
        }
        if self.finger_down() {
            return GateVerdict::WaitFingerUp;
        }
        // Did a physical keyboard key happen very recently without a subsequent touch?
        if self.last_hw_key_ns > self.up_ns
            && self.now_ns.saturating_sub(self.last_hw_key_ns) < 5_000_000_000
        {
            return GateVerdict::Suppress("physical keyboard active");
        }
        // Require a recent tap on an app window within the last 750 ms.
        if self.up_ns == 0 || self.now_ns.saturating_sub(self.up_ns) > 750_000_000 {
            return GateVerdict::Suppress("no recent touch behind focus-in");
        }
        if !self.is_tap {
            return GateVerdict::Suppress("touch was a drag/hold/gesture, not a tap");
        }
        if self.on_layer {
            return GateVerdict::Suppress("tap was on a shell/layer surface");
        }
        GateVerdict::Allow("confirmed single-finger tap on app window")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    Allow(&'static str),
    Suppress(&'static str),
    WaitFingerUp,
}

pub struct TouchGateShm {
    ptr: NonNull<TouchGateRaw>,
}

unsafe impl Send for TouchGateShm {}
unsafe impl Sync for TouchGateShm {}

impl TouchGateShm {
    pub fn default_path() -> PathBuf {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(dir).join("tb520fu-touch-gate")
        } else {
            let uid = unsafe { libc::getuid() };
            PathBuf::from(format!("/run/user/{uid}/tb520fu-touch-gate"))
        }
    }

    pub fn open_or_create(path: Option<PathBuf>) -> std::io::Result<Self> {
        let p = path.unwrap_or_else(Self::default_path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(p)?;
        file.set_len(std::mem::size_of::<TouchGateRaw>() as u64)?;
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                std::mem::size_of::<TouchGateRaw>(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let ptr = NonNull::new(raw as *mut TouchGateRaw).unwrap();
        Ok(Self { ptr })
    }

    #[inline]
    fn raw(&self) -> &TouchGateRaw {
        unsafe { self.ptr.as_ref() }
    }

    pub fn monotonic_ns() -> u64 {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
    }

    pub fn record_touch_down(&self, on_layer: bool, fingers: u8) {
        let now = Self::monotonic_ns();
        let r = self.raw();
        let mut flags = FLAG_IS_TAP | ((fingers as u64) << 8);
        if on_layer {
            flags |= FLAG_ON_LAYER;
        }
        r.flags.store(flags, Ordering::Relaxed);
        r.down_ns.store(now, Ordering::Release);
        r.heartbeat_ns.store(now, Ordering::Release);
        r.magic.store(MAGIC, Ordering::Release);
    }

    pub fn record_touch_not_tap(&self, fingers: u8) {
        let now = Self::monotonic_ns();
        let r = self.raw();
        let prev = r.flags.load(Ordering::Relaxed);
        let max_f = ((prev >> 8) as u8).max(fingers);
        let next = (prev & !FLAG_IS_TAP & !(0xFF << 8)) | ((max_f as u64) << 8);
        r.flags.store(next, Ordering::Release);
        r.heartbeat_ns.store(now, Ordering::Release);
    }

    pub fn record_touch_up(&self, max_duration: Duration) {
        let now = Self::monotonic_ns();
        let r = self.raw();
        let down = r.down_ns.load(Ordering::Acquire);
        if now.saturating_sub(down) > max_duration.as_nanos() as u64 {
            let prev = r.flags.load(Ordering::Relaxed);
            r.flags.store(prev & !FLAG_IS_TAP, Ordering::Release);
        }
        r.up_ns.store(now, Ordering::Release);
        r.heartbeat_ns.store(now, Ordering::Release);
        r.magic.store(MAGIC, Ordering::Release);
    }

    pub fn record_hw_key(&self) {
        let now = Self::monotonic_ns();
        let r = self.raw();
        r.last_hw_key_ns.store(now, Ordering::Release);
        r.heartbeat_ns.store(now, Ordering::Release);
        r.magic.store(MAGIC, Ordering::Release);
    }

    pub fn snapshot(&self) -> TouchGateSnapshot {
        let now_ns = Self::monotonic_ns();
        let r = self.raw();
        let magic = r.magic.load(Ordering::Acquire);
        let hb = r.heartbeat_ns.load(Ordering::Acquire);
        // Consider alive once Niri has initialized magic (remains valid across idle periods)
        let alive = magic == MAGIC && hb > 0;
        let down_ns = r.down_ns.load(Ordering::Acquire);
        let up_ns = r.up_ns.load(Ordering::Acquire);
        let flags = r.flags.load(Ordering::Acquire);
        let last_hw_key_ns = r.last_hw_key_ns.load(Ordering::Acquire);
        TouchGateSnapshot {
            alive,
            down_ns,
            up_ns,
            is_tap: (flags & FLAG_IS_TAP) != 0 && ((flags >> 8) as u8) <= 1,
            on_layer: (flags & FLAG_ON_LAYER) != 0,
            max_fingers: (flags >> 8) as u8,
            last_hw_key_ns,
            now_ns,
        }
    }
}

impl Drop for TouchGateShm {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(
                self.ptr.as_ptr() as *mut libc::c_void,
                std::mem::size_of::<TouchGateRaw>(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_distinguishes_tap_drag_and_hw_key() {
        let tmp = std::env::temp_dir().join(format!("tb520fu-gate-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let gate = TouchGateShm::open_or_create(Some(tmp.clone())).unwrap();

        // 1. Before initialization -> fail open
        assert!(matches!(gate.snapshot().allow_auto_show(), GateVerdict::Allow(_)));

        // 2. Finger down -> WaitFingerUp
        gate.record_touch_down(false, 1);
        assert_eq!(gate.snapshot().allow_auto_show(), GateVerdict::WaitFingerUp);

        // 3. Clean lift -> Allow
        gate.record_touch_up(Duration::from_millis(500));
        assert!(matches!(gate.snapshot().allow_auto_show(), GateVerdict::Allow(_)));

        // 4. Drag -> Suppress
        gate.record_touch_down(false, 1);
        gate.record_touch_not_tap(1);
        gate.record_touch_up(Duration::from_millis(500));
        assert!(matches!(gate.snapshot().allow_auto_show(), GateVerdict::Suppress(_)));

        // 5. Physical Pogo keyboard press -> Suppress
        gate.record_touch_down(false, 1);
        gate.record_touch_up(Duration::from_millis(500));
        std::thread::sleep(Duration::from_millis(2));
        gate.record_hw_key();
        assert!(matches!(gate.snapshot().allow_auto_show(), GateVerdict::Suppress(_)));

        let _ = std::fs::remove_file(&tmp);
    }
}
