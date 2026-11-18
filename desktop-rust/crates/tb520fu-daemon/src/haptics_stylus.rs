// SPDX-License-Identifier: MIT
use std::ffi::CString;
use std::fs;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_FF: u16 = 0x15;
const FF_RUMBLE: u16 = 0x50;
const EVIOCSFF: libc::c_ulong = 0x4030_4580;

const BTN_TOOL_PEN: u16 = 0x140; // 320
const BTN_STYLUS: u16 = 0x14b; // 331
const BTN_STYLUS2: u16 = 0x14c; // 332

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

#[repr(C, align(8))]
#[derive(Clone, Copy)]
union FfEffectUnion {
    rumble: FfRumbleEffect,
    _pad: [u64; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FfEffect {
    effect_type: u16,
    id: i16,
    direction: u16,
    trigger: FfTrigger,
    replay: FfReplay,
    u: FfEffectUnion,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInputEvent {
    tv_sec: libc::time_t,
    tv_usec: libc::suseconds_t,
    type_: u16,
    code: u16,
    value: i32,
}

struct MotorDev {
    _fd: OwnedFd,
    raw: i32,
    slot_id: i16,
}

impl MotorDev {
    fn open(path: &Path) -> Option<Self> {
        let cpath = CString::new(path.as_os_str().as_bytes()).ok()?;
        let raw = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if raw < 0 {
            return None;
        }
        Some(Self {
            _fd: unsafe { OwnedFd::from_raw_fd(raw) },
            raw,
            slot_id: -1,
        })
    }

    fn rumble(&mut self, mag: u16, dur_ms: u16) {
        let mut eff = FfEffect {
            effect_type: FF_RUMBLE,
            id: self.slot_id,
            direction: 0,
            trigger: FfTrigger::default(),
            replay: FfReplay {
                length: dur_ms.clamp(4, 250),
                delay: 0,
            },
            u: FfEffectUnion {
                rumble: FfRumbleEffect {
                    strong_magnitude: mag,
                    weak_magnitude: mag / 2,
                },
            },
        };
        let rc = unsafe { libc::ioctl(self.raw, EVIOCSFF as _, &mut eff as *mut FfEffect) };
        if rc < 0 {
            self.slot_id = -1;
            eff.id = -1;
            if unsafe { libc::ioctl(self.raw, EVIOCSFF as _, &mut eff as *mut FfEffect) } < 0 {
                return;
            }
        }
        self.slot_id = eff.id;
        let play = RawInputEvent {
            tv_sec: 0,
            tv_usec: 0,
            type_: EV_FF,
            code: eff.id as u16,
            value: 1,
        };
        let syn = RawInputEvent {
            tv_sec: 0,
            tv_usec: 0,
            type_: EV_SYN,
            code: 0,
            value: 0,
        };
        unsafe {
            let _ = libc::write(
                self.raw,
                &play as *const RawInputEvent as *const libc::c_void,
                std::mem::size_of::<RawInputEvent>(),
            );
            let _ = libc::write(
                self.raw,
                &syn as *const RawInputEvent as *const libc::c_void,
                std::mem::size_of::<RawInputEvent>(),
            );
        }
    }
}

fn discover_aw86927() -> (Option<MotorDev>, Option<MotorDev>) {
    let mut left = None;
    let mut right = None;
    let mut fallback = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/input") else {
        return (None, None);
    };
    for e in entries.flatten() {
        let s = e.file_name().to_string_lossy().to_string();
        if !s.starts_with("event") {
            continue;
        }
        let sys_path = e.path();
        let dev_name = fs::read_to_string(sys_path.join("device/name"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !dev_name.contains("aw86927") && !dev_name.contains("haptic") {
            continue;
        }
        let node = PathBuf::from(format!("/dev/input/{s}"));
        let hw = fs::canonicalize(sys_path.join("device/device"))
            .ok()
            .map(|p| p.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if hw.ends_with("5a") || hw.contains("-005a") {
            left = MotorDev::open(&node);
        } else if hw.ends_with("5b") || hw.contains("-005b") {
            right = MotorDev::open(&node);
        } else {
            fallback.push(node);
        }
    }
    for n in fallback {
        if left.is_none() {
            left = MotorDev::open(&n);
        } else if right.is_none() {
            right = MotorDev::open(&n);
        }
    }
    (left, right)
}

#[derive(Clone)]
pub struct RootHaptics {
    tx: mpsc::SyncSender<(String, u16, u16)>,
}

impl RootHaptics {
    pub fn spawn() -> Self {
        assert_eq!(std::mem::size_of::<FfEffect>(), 48);
        let (tx, rx) = mpsc::sync_channel::<(String, u16, u16)>(12);
        std::thread::Builder::new()
            .name("tb520fu-root-haptics".into())
            .spawn(move || {
                let (mut left, mut right) = discover_aw86927();
                while let Ok((side, mag, dur_ms)) = rx.recv() {
                    if left.is_none() && right.is_none() {
                        let (l, r) = discover_aw86927();
                        left = l;
                        right = r;
                    }
                    match side.as_str() {
                        "left" => {
                            if let Some(m) = left.as_mut() {
                                m.rumble(mag, dur_ms);
                            } else if let Some(m) = right.as_mut() {
                                m.rumble(mag, dur_ms);
                            }
                            if let Some(m) = right.as_mut() {
                                m.rumble(mag / 4, (dur_ms / 2).max(4));
                            }
                        }
                        "right" => {
                            if let Some(m) = right.as_mut() {
                                m.rumble(mag, dur_ms);
                            } else if let Some(m) = left.as_mut() {
                                m.rumble(mag, dur_ms);
                            }
                            if let Some(m) = left.as_mut() {
                                m.rumble(mag / 4, (dur_ms / 2).max(4));
                            }
                        }
                        _ => {
                            if let Some(m) = left.as_mut() {
                                m.rumble(mag, dur_ms);
                            }
                            if let Some(m) = right.as_mut() {
                                m.rumble(mag, dur_ms);
                            }
                        }
                    }
                }
            })
            .ok();
        Self { tx }
    }

    pub fn pulse(&self, side: String, strength: u16, duration_ms: u16) {
        if strength > 0 && duration_ms > 0 {
            let _ = self.tx.try_send((side, strength, duration_ms));
        }
    }
}

#[derive(Default)]
pub struct StylusInputState {
    pub hover: AtomicBool,
    pub action_seq: AtomicU64,
    pub action_code: AtomicU32,
}

fn find_stylus_evdev_nodes() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/input") else {
        return out;
    };
    for e in entries.flatten() {
        let s = e.file_name().to_string_lossy().to_string();
        if !s.starts_with("event") {
            continue;
        }
        let name = fs::read_to_string(e.path().join("device/name"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.contains("pen")
            || name.contains("stylus")
            || name.contains("goodix")
            || name.contains("lenovo tab")
        {
            out.push(PathBuf::from(format!("/dev/input/{s}")));
        }
    }
    out
}

/// Watch Lenovo Tab Pen Plus (`BLE` + `Goodix` digitizer) for `BTN_TOOL_PEN` hover (`144Hz ↔ 120Hz`)
/// and hardware barrel/tail keycodes (`601` Screenshot+Annotate, `612` Quick Note, `613` Action, `331`/`332`).
pub fn spawn_stylus_evdev_thread(shutdown: Arc<AtomicBool>, st: Arc<StylusInputState>) {
    std::thread::Builder::new()
        .name("tb520fu-stylus-evdev".into())
        .spawn(move || {
            let mut fds: Vec<OwnedFd> = Vec::new();
            let mut last_scan = std::time::Instant::now() - Duration::from_secs(10);

            while !shutdown.load(Ordering::Relaxed) {
                if fds.is_empty() || last_scan.elapsed() >= Duration::from_secs(6) {
                    last_scan = std::time::Instant::now();
                    fds.clear();
                    for node in find_stylus_evdev_nodes() {
                        if let Ok(cpath) = CString::new(node.as_os_str().as_bytes()) {
                            let raw = unsafe {
                                libc::open(
                                    cpath.as_ptr(),
                                    libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                                )
                            };
                            if raw >= 0 {
                                fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
                            }
                        }
                    }
                    if fds.is_empty() {
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                }

                use std::os::fd::AsRawFd;
                let mut pfds: Vec<libc::pollfd> = fds
                    .iter()
                    .map(|f| libc::pollfd {
                        fd: f.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    })
                    .collect();

                let rc = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, 500) };
                if rc <= 0 {
                    continue;
                }

                let mut any_hup = false;
                let ev_sz = std::mem::size_of::<RawInputEvent>();
                for pfd in &pfds {
                    if (pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)) != 0 {
                        any_hup = true;
                        continue;
                    }
                    if (pfd.revents & libc::POLLIN) == 0 {
                        continue;
                    }
                    let mut buf = [RawInputEvent::default(); 16];
                    let n = unsafe {
                        libc::read(
                            pfd.fd,
                            buf.as_mut_ptr() as *mut libc::c_void,
                            buf.len() * ev_sz,
                        )
                    };
                    if n <= 0 {
                        continue;
                    }
                    let count = (n as usize) / ev_sz;
                    for ev in &buf[..count] {
                        if ev.type_ == EV_KEY {
                            match ev.code {
                                BTN_TOOL_PEN => {
                                    st.hover.store(ev.value != 0, Ordering::Relaxed);
                                }
                                601 | 612 | 613 | BTN_STYLUS | BTN_STYLUS2 if ev.value == 1 => {
                                    st.action_code.store(ev.code as u32, Ordering::Relaxed);
                                    st.action_seq.fetch_add(1, Ordering::Release);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                if any_hup {
                    fds.clear();
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_ff_effect_size_is_48() {
        assert_eq!(std::mem::size_of::<FfEffect>(), 48);
    }
}
