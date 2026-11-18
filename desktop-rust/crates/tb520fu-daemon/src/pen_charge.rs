// SPDX-License-Identifier: MIT
use std::ffi::CString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::hardware::LenovoHqSysfs;

const HALL_INPUT_NAME: &str = "hall-keys";
const SW_TABLET_MODE: u32 = 0x01;
const SW_PEN_INSERTED: u32 = 0x0f;
const EVIOCGSW: libc::c_ulong = 0x8008_451B;
const ENABLE_LINE: u32 = 178; // TLMM GPIO 178 high = wireless TX enabled

const PROP_ATTACHED: i32 = 43; // 0 = not charging, 1 = charging, 2 = full
const PROP_LEVEL: i32 = 44;
const PROP_HALL: i32 = 45;
const PROP_TX_IIN: i32 = 47;
const PROP_TX_CMD: i32 = 50;
const TX_CMD_START: i32 = 402;
const QI_STATE_CHARGING: i32 = 1;
const QI_STATE_FULL: i32 = 2;

// Wireless top-up window: charge only after the pen has dropped below START, stop at STOP.
// Keeps the pen out of the 90-100 % range and avoids ~1 W of coil losses for every 1 % top-up.
const PEN_CHARGE_START: i32 = 85;
const PEN_CHARGE_STOP: i32 = 90;
// While docked with the coil off, the level is only readable over the Qi link, so pulse TX this often.
const PEN_REPROBE: Duration = Duration::from_secs(30 * 60);

const GPIO_GET_CHIPINFO: libc::c_ulong = 0x8044_B401;
const GPIO_V2_GET_LINE: libc::c_ulong = 0xC250_B407;
const GPIO_V2_LINE_SET_VALUES: libc::c_ulong = 0xC010_B40F;
const FLAG_OUTPUT: u64 = 1 << 3;

pub struct PenTelemetry {
    pub docked: AtomicBool,
    pub tx_on: AtomicBool,
    pub charging: AtomicBool,
    pub level: AtomicI32,
    pub iin_ma: AtomicI32,
}

impl Default for PenTelemetry {
    fn default() -> Self {
        Self {
            docked: AtomicBool::new(false),
            tx_on: AtomicBool::new(false),
            charging: AtomicBool::new(false),
            level: AtomicI32::new(-1),
            iin_ma: AtomicI32::new(0),
        }
    }
}

#[inline]
pub fn is_actively_charging(st: i32, lvl: i32, iin_ma: i32) -> bool {
    st == QI_STATE_CHARGING && !(lvl >= 100 && iin_ma <= 5)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenAction {
    Charge,
    Stop,
    /// Level not readable yet; keep the coil on and look again.
    Wait,
}

/// `session` = a top-up already started (level was below `PEN_CHARGE_START`).
pub fn pen_charge_action(session: bool, st: i32, lvl: i32, iin_ma: i32) -> PenAction {
    if !(0..=100).contains(&lvl) {
        return PenAction::Wait;
    }
    let fw_full = (st == QI_STATE_FULL && lvl >= 100) || (st != QI_STATE_CHARGING && lvl >= 100 && iin_ma <= 5);
    if fw_full {
        return PenAction::Stop;
    }
    let limit = if session { PEN_CHARGE_STOP } else { PEN_CHARGE_START };
    if lvl < limit {
        PenAction::Charge
    } else {
        PenAction::Stop
    }
}

#[repr(C)]
struct GpioChipInfo {
    name: [u8; 32],
    label: [u8; 32],
    lines: u32,
}

#[repr(C)]
struct GpioV2LineValues {
    bits: u64,
    mask: u64,
}

fn find_tlmm_chip() -> Option<i32> {
    let Ok(entries) = fs::read_dir("/dev") else {
        return None;
    };
    let mut chips: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|s| s.starts_with("gpiochip"))
        })
        .collect();
    chips.sort();

    for path in chips {
        let Ok(cpath) = CString::new(path.to_string_lossy().as_bytes()) else {
            continue;
        };
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            continue;
        }
        let mut info = GpioChipInfo {
            name: [0; 32],
            label: [0; 32],
            lines: 0,
        };
        let rc = unsafe { libc::ioctl(fd, GPIO_GET_CHIPINFO as _, &mut info as *mut GpioChipInfo) };
        if rc == 0 {
            let label_len = info.label.iter().position(|&b| b == 0).unwrap_or(32);
            let label = String::from_utf8_lossy(&info.label[..label_len]);
            if label.contains("pinctrl") && info.lines > ENABLE_LINE {
                return Some(fd);
            }
        }
        unsafe {
            libc::close(fd);
        }
    }
    None
}

fn request_gpio_line(chip_fd: i32, offset: u32, flags: u64) -> Option<i32> {
    // Exact 592-byte `struct gpio_v2_line_request` layout from `<linux/gpio.h>`
    let mut req = [0u8; 592];
    req[0..4].copy_from_slice(&offset.to_ne_bytes());
    let consumer = b"tb520fu-pen";
    req[256..256 + consumer.len()].copy_from_slice(consumer);
    req[288..296].copy_from_slice(&flags.to_ne_bytes());
    req[560..564].copy_from_slice(&1u32.to_ne_bytes());

    let rc = unsafe { libc::ioctl(chip_fd, GPIO_V2_GET_LINE as _, req.as_mut_ptr()) };
    if rc < 0 {
        return None;
    }
    let line_fd = i32::from_ne_bytes(req[588..592].try_into().unwrap());
    (line_fd >= 0).then_some(line_fd)
}

fn set_gpio_value(line_fd: i32, high: bool) {
    let vals = GpioV2LineValues {
        bits: u64::from(high),
        mask: 1,
    };
    unsafe {
        let _ = libc::ioctl(line_fd, GPIO_V2_LINE_SET_VALUES as _, &vals as *const GpioV2LineValues);
    }
}

fn open_hall_input() -> Option<i32> {
    let entries = fs::read_dir("/sys/class/input").ok()?;
    for e in entries.flatten() {
        let ev_name = e.file_name();
        let s = ev_name.to_string_lossy();
        if !s.starts_with("event") {
            continue;
        }
        let name_file = format!("/sys/class/input/{s}/device/name");
        if fs::read_to_string(name_file).is_ok_and(|n| n.trim() == HALL_INPUT_NAME) {
            let dev = CString::new(format!("/dev/input/{s}")).ok()?;
            let fd = unsafe { libc::open(dev.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
            if fd >= 0 {
                return Some(fd);
            }
        }
    }
    None
}

fn is_pen_attached(hall_fd: i32) -> bool {
    let mut bits: u64 = 0;
    let rc = unsafe { libc::ioctl(hall_fd, EVIOCGSW as _, &mut bits as *mut u64) };
    if rc < 0 {
        return false;
    }
    let mask = (1u64 << SW_TABLET_MODE) | (1u64 << SW_PEN_INSERTED);
    (bits & mask) != 0
}

fn set_touch_pen_charge(docked: bool) -> bool {
    let Ok(dir) = fs::read_dir("/sys/bus/spi/drivers/NVT-ts-spi") else {
        return false;
    };
    let val = if docked { "1\n" } else { "0\n" };
    let mut wrote = false;
    for entry in dir.flatten() {
        let p = entry.path().join("pen_charge_state");
        if p.exists() && fs::write(&p, val).is_ok() {
            wrote = true;
        }
    }
    wrote
}

fn drain_wake_fd(wake_fd: i32) {
    if wake_fd >= 0 {
        let mut val: u64 = 0;
        unsafe {
            let _ = libc::read(wake_fd, &mut val as *mut u64 as *mut libc::c_void, 8);
        }
    }
}

fn wait_interruptible_ms(wake_fd: i32, shutdown: &AtomicBool, sleeping: &AtomicBool, ms: i32) -> bool {
    if shutdown.load(Ordering::Relaxed) || sleeping.load(Ordering::Acquire) {
        return false;
    }
    if wake_fd >= 0 {
        let mut pfd = libc::pollfd {
            fd: wake_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc > 0 && (pfd.revents & libc::POLLIN) != 0 {
            drain_wake_fd(wake_fd);
        }
    } else {
        std::thread::sleep(Duration::from_millis(ms.max(0) as u64));
    }
    !shutdown.load(Ordering::Relaxed) && !sleeping.load(Ordering::Acquire)
}

pub fn spawn_pen_charge_thread(
    shutdown: Arc<AtomicBool>,
    sleeping: Arc<AtomicBool>,
    wake_fd: i32,
    hq: Arc<LenovoHqSysfs>,
    telemetry: Arc<PenTelemetry>,
) {
    std::thread::Builder::new()
        .name("tb520fu-pen-qi".into())
        .spawn(move || {
            let chip_fd = loop {
                if shutdown.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(fd) = find_tlmm_chip() {
                    break fd;
                }
                std::thread::sleep(Duration::from_secs(2));
            };

            let Some(enable_fd) = request_gpio_line(chip_fd, ENABLE_LINE, FLAG_OUTPUT) else {
                eprintln!("tb520fu-daemon[pen]: failed to claim TLMM GPIO {ENABLE_LINE}");
                unsafe {
                    libc::close(chip_fd);
                }
                return;
            };

            let hall_fd = loop {
                if shutdown.load(Ordering::Relaxed) {
                    set_gpio_value(enable_fd, false);
                    unsafe {
                        libc::close(enable_fd);
                        libc::close(chip_fd);
                    }
                    return;
                }
                if let Some(fd) = open_hall_input() {
                    break fd;
                }
                std::thread::sleep(Duration::from_secs(1));
            };

            let mut docked: Option<bool> = None;
            let mut touch_synced: Option<bool> = None;
            let mut tx_on = false;
            let mut session = false; // top-up in progress (started below PEN_CHARGE_START)
            let mut resume_session = false; // a top-up was interrupted by suspend
            let mut next_probe: Option<Instant> = None;
            let mut paused_for_sleep = false;

            let tx_stop = |hq: &LenovoHqSysfs, tx_on: &mut bool| {
                let _ = hq.set(PROP_HALL, 0);
                set_gpio_value(enable_fd, false);
                *tx_on = false;
                telemetry.tx_on.store(false, Ordering::Release);
                telemetry.charging.store(false, Ordering::Relaxed);
                telemetry.iin_ma.store(0, Ordering::Relaxed);
            };

            while !shutdown.load(Ordering::Relaxed) {
                let attached = is_pen_attached(hall_fd);
                telemetry.docked.store(attached, Ordering::Relaxed);

                if touch_synced != Some(attached) && set_touch_pen_charge(attached) {
                    touch_synced = Some(attached);
                }

                // Cut wireless coil power (`GPIO 178`) BEFORE entering S3 deep suspend so the
                // 5V wireless TX booster cannot remain energized unmanaged across multi-hour sleep!
                if sleeping.load(Ordering::Acquire) {
                    if tx_on {
                        tx_stop(&hq, &mut tx_on);
                        paused_for_sleep = true;
                        resume_session = session;
                        eprintln!("tb520fu-daemon[pen]: system suspending -> wireless TX paused");
                    }
                    if !attached && docked != Some(false) {
                        telemetry.level.store(-1, Ordering::Relaxed);
                        docked = Some(false);
                        hq.set_pen_attached(false);
                    }
                } else {
                    if paused_for_sleep {
                        paused_for_sleep = false;
                        if attached {
                            docked = None; // Re-evaluate (and resume an interrupted top-up) after S3
                        }
                    }
                    // Docked with the coil off: periodically pulse TX to read the level again.
                    if attached
                        && !tx_on
                        && docked == Some(true)
                        && next_probe.is_some_and(|t| Instant::now() >= t)
                    {
                        next_probe = None;
                        docked = None;
                    }

                    if docked != Some(attached) {
                        if attached {
                            set_gpio_value(enable_fd, true);
                            let _ = hq.set(PROP_HALL, 1);
                            tx_on = true;
                            telemetry.tx_on.store(true, Ordering::Release);
                            if wait_interruptible_ms(wake_fd, &shutdown, &sleeping, 1000)
                                && is_pen_attached(hall_fd)
                            {
                                let _ = hq.set(PROP_TX_CMD, TX_CMD_START);
                                if wait_interruptible_ms(wake_fd, &shutdown, &sleeping, 1000)
                                    && is_pen_attached(hall_fd)
                                {
                                    let st = hq.get(PROP_ATTACHED).unwrap_or(0);
                                    let lvl = hq
                                        .get(PROP_LEVEL)
                                        .filter(|v| (0..=100).contains(v))
                                        .unwrap_or(-1);
                                    let iin = hq.get(PROP_TX_IIN).unwrap_or(0);
                                    let chg = is_actively_charging(st, lvl, iin);
                                    telemetry.level.store(lvl, Ordering::Relaxed);
                                    telemetry.iin_ma.store(iin, Ordering::Relaxed);
                                    telemetry.charging.store(chg, Ordering::Relaxed);
                                    let action = pen_charge_action(resume_session, st, lvl, iin);
                                    resume_session = false;
                                    match action {
                                        PenAction::Charge => {
                                            session = true;
                                            eprintln!(
                                                "tb520fu-daemon[pen]: attached -> top-up to {PEN_CHARGE_STOP}% (state={st}, level={lvl}%, iin={iin}mA)"
                                            );
                                        }
                                        PenAction::Stop => {
                                            session = false;
                                            tx_stop(&hq, &mut tx_on);
                                            next_probe = Some(Instant::now() + PEN_REPROBE);
                                            eprintln!(
                                                "tb520fu-daemon[pen]: attached, level {lvl}% (state={st}) -> TX off, top-up below {PEN_CHARGE_START}%"
                                            );
                                        }
                                        PenAction::Wait => {
                                            session = false;
                                            eprintln!(
                                                "tb520fu-daemon[pen]: attached -> TX on, level not reported yet (state={st}, iin={iin}mA)"
                                            );
                                        }
                                    }
                                    docked = Some(true);
                                    hq.set_pen_attached(true);
                                } else {
                                    tx_stop(&hq, &mut tx_on);
                                    if sleeping.load(Ordering::Acquire) {
                                        paused_for_sleep = true;
                                    }
                                }
                            } else if sleeping.load(Ordering::Acquire) {
                                tx_stop(&hq, &mut tx_on);
                                paused_for_sleep = true;
                            } else {
                                tx_stop(&hq, &mut tx_on);
                                telemetry.docked.store(false, Ordering::Relaxed);
                                telemetry.level.store(-1, Ordering::Relaxed);
                                docked = Some(false);
                                hq.set_pen_attached(false);
                            }
                        } else {
                            if tx_on {
                                tx_stop(&hq, &mut tx_on);
                            }
                            session = false;
                            resume_session = false;
                            next_probe = None;
                            // ADSP Qi firmware retains the last docked battery level in prop 44 forever
                            // after the pen is removed; clear `level` to -1 so user-space switches to BLE.
                            telemetry.level.store(-1, Ordering::Relaxed);
                            telemetry.charging.store(false, Ordering::Relaxed);
                            telemetry.iin_ma.store(0, Ordering::Relaxed);
                            eprintln!(
                                "tb520fu-daemon[pen]: removed -> TX off, level cleared, digitizer enabled"
                            );
                            docked = Some(false);
                            hq.set_pen_attached(false);
                        }
                    } else if attached && tx_on {
                        let st = hq.get(PROP_ATTACHED).unwrap_or(0);
                        let lvl = hq
                            .get(PROP_LEVEL)
                            .filter(|v| (0..=100).contains(v))
                            .unwrap_or(-1);
                        let iin = hq.get(PROP_TX_IIN).unwrap_or(0);
                        let chg = is_actively_charging(st, lvl, iin);
                        telemetry.level.store(lvl, Ordering::Relaxed);
                        telemetry.iin_ma.store(iin, Ordering::Relaxed);
                        telemetry.charging.store(chg, Ordering::Relaxed);
                        match pen_charge_action(session, st, lvl, iin) {
                            PenAction::Charge => session = true,
                            PenAction::Stop => {
                                session = false;
                                tx_stop(&hq, &mut tx_on);
                                next_probe = Some(Instant::now() + PEN_REPROBE);
                                eprintln!(
                                    "tb520fu-daemon[pen]: level {lvl}% (state={st}, iin={iin}mA) -> TX off while docked"
                                );
                            }
                            PenAction::Wait => {}
                        }
                    }
                }

                let timeout_ms = if touch_synced != Some(attached) {
                    2000
                } else if attached && tx_on && !session {
                    5000 // Level not known yet: look again soon instead of leaving the coil on
                } else if attached && tx_on {
                    30_000
                } else {
                    2500 // Check shutdown flag periodically with minimal wakeups
                };

                let mut pfds = [
                    libc::pollfd {
                        fd: hall_fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: wake_fd,
                        events: if wake_fd >= 0 { libc::POLLIN } else { 0 },
                        revents: 0,
                    },
                ];
                let nfds = if wake_fd >= 0 { 2 } else { 1 };
                let rc = unsafe { libc::poll(pfds.as_mut_ptr(), nfds, timeout_ms) };
                if rc > 0 {
                    if wake_fd >= 0 && (pfds[1].revents & libc::POLLIN) != 0 {
                        drain_wake_fd(wake_fd);
                    }
                    if (pfds[0].revents & libc::POLLIN) != 0 {
                        let mut sink = [0u8; 384];
                        unsafe {
                            let _ = libc::read(
                                hall_fd,
                                sink.as_mut_ptr() as *mut libc::c_void,
                                sink.len(),
                            );
                        }
                        std::thread::sleep(Duration::from_millis(150));
                    }
                }
            }

            // Safe shutdown: always cut wireless charging coil power!
            tx_stop(&hq, &mut tx_on);
            let _ = set_touch_pen_charge(is_pen_attached(hall_fd));
            eprintln!("tb520fu-daemon[pen]: shutdown -> wireless TX off");
            unsafe {
                libc::close(hall_fd);
                libc::close(enable_fd);
                libc::close(chip_fd);
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pen_top_up_window() {
        // Docking: only start below 85 %.
        assert_eq!(pen_charge_action(false, QI_STATE_CHARGING, 84, 150), PenAction::Charge);
        assert_eq!(pen_charge_action(false, QI_STATE_CHARGING, 85, 150), PenAction::Stop);
        assert_eq!(pen_charge_action(false, QI_STATE_CHARGING, 99, 138), PenAction::Stop);
        // Running top-up: continue until 90 %.
        assert_eq!(pen_charge_action(true, QI_STATE_CHARGING, 87, 150), PenAction::Charge);
        assert_eq!(pen_charge_action(true, QI_STATE_CHARGING, 90, 150), PenAction::Stop);
        // Firmware full always stops; unknown level waits.
        assert_eq!(pen_charge_action(true, QI_STATE_FULL, 100, 0), PenAction::Stop);
        assert_eq!(pen_charge_action(false, QI_STATE_CHARGING, -1, 0), PenAction::Wait);
    }

    #[test]
    fn pen_charging_indicator_respects_full_and_idle_coil() {
        assert!(is_actively_charging(QI_STATE_CHARGING, 72, 180));
        assert!(is_actively_charging(QI_STATE_CHARGING, 100, 136));
        assert!(!is_actively_charging(QI_STATE_FULL, 100, 0));
        assert!(!is_actively_charging(QI_STATE_FULL, 100, 42));
        assert!(!is_actively_charging(0, 100, 0));
        assert!(!is_actively_charging(QI_STATE_CHARGING, 100, 0));
    }
}
