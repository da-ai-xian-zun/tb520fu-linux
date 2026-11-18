// SPDX-License-Identifier: MIT
//! Magnetic keyboard presence / tablet-mode watcher (settings-app DESIGN.md step 0).
//!
//! The Pogo keyboard pack exposes three input devices under stable by-path names:
//!   - `platform-888000.serial-event-kbd`  "Lenovo Keyboard Pack For Yoga Tab"
//!   - `platform-888000.serial-event-mouse` touchpad
//!   - `platform-888000.serial-event`      "Lid" switch device (SW_LID + SW_TABLET_MODE)
//!
//! fcitx5 only auto-pops the OSK on input focus while in `OnScreenKeyboard` mode,
//! and any physical key press drops it back to `PhysicalKeyboard` mode. So when the
//! keyboard is detached or folded back into tablet mode we must re-prime the mode
//! (see `FcitxCmd::PrimeOnScreenMode` in `fcitx.rs`).
//!
//! Polling (500 ms) is deliberate: it is hot-plug resilient with zero udev dependency
//! and the latency is imperceptible for mode priming. `debian` is in the `input`
//! group, so opening the lid device read-only needs no privileges.

use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::fcitx::FcitxCmd;

const KBD_PATH: &str = "/dev/input/by-path/platform-888000.serial-event-kbd";
const LID_PATH: &str = "/dev/input/by-path/platform-888000.serial-event";

/// `EVIOCGSW(8)` = `_IOC(_IOC_READ, 'E', 0x1b, 8)` on aarch64/x86_64.
const EVIOCGSW_8: libc::c_ulong = 0x8008_451b;
const SW_TABLET_MODE: u64 = 1 << 1;

/// Shared snapshot of the keyboard pack state, readable from the fcitx bridge.
#[derive(Clone, Default)]
pub struct KbdState {
    /// Keyboard pack is physically attached (kbd input device exists).
    pub present: Arc<AtomicBool>,
    /// Lid reports SW_TABLET_MODE=1 (folded back into tablet mode).
    pub tablet_mode: Arc<AtomicBool>,
}

impl KbdState {
    pub fn new() -> Self {
        let s = Self::default();
        s.refresh();
        s
    }

    /// Re-read device presence and lid switches; returns `(present, tablet_mode)`.
    pub fn refresh(&self) -> (bool, bool) {
        let present = Path::new(KBD_PATH).exists();
        let tablet = present && read_lid_tablet_mode();
        self.present.store(present, Ordering::Relaxed);
        self.tablet_mode.store(tablet, Ordering::Relaxed);
        (present, tablet)
    }

    /// Prime the fcitx5 on-screen mode when the user is (or just became) a
    /// touch-only user: no keyboard attached, or folded into tablet mode.
    pub fn should_prime(&self) -> bool {
        !self.present.load(Ordering::Relaxed) || self.tablet_mode.load(Ordering::Relaxed)
    }
}

fn read_lid_tablet_mode() -> bool {
    let Ok(file) = std::fs::File::open(LID_PATH) else {
        return false;
    };
    let mut sw = [0u64; 1];
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGSW_8, sw.as_mut_ptr()) };
    rc >= 0 && (sw[0] & SW_TABLET_MODE) != 0
}

/// Spawn the watcher thread. Sends `PrimeOnScreenMode` on detach and on fold-into-
/// tablet transitions; the bridge decides whether priming is actually needed.
pub fn spawn_kbd_watcher(
    state: KbdState,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<FcitxCmd>,
) {
    let _ = std::thread::Builder::new()
        .name("tb520fu-osk-kbd".into())
        .spawn(move || {
            let mut prev_present = state.present.load(Ordering::Relaxed);
            let mut prev_tablet = state.tablet_mode.load(Ordering::Relaxed);
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let (present, tablet) = state.refresh();
                if prev_present && !present {
                    eprintln!("tb520fu-osk: pogo keyboard detached");
                    let _ = cmd_tx.send(FcitxCmd::PrimeOnScreenMode);
                }
                if !prev_tablet && tablet {
                    eprintln!("tb520fu-osk: keyboard folded into tablet mode");
                    let _ = cmd_tx.send(FcitxCmd::PrimeOnScreenMode);
                }
                prev_present = present;
                prev_tablet = tablet;
            }
        })
        .ok();
}
