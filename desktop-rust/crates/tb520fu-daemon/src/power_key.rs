// SPDX-License-Identifier: MIT
use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const DPMS_PATH: &str = "/sys/class/drm/card1-DSI-1/dpms";
const KTZ_REGS_FALLBACK: &str = "/sys/kernel/debug/regmap/1-0011/registers";
const BL_DIR: &str = "/sys/class/backlight/ktz8866-backlight";

const LONG_PRESS_MS: f64 = 800.0;
const RESUME_DEBOUNCE_S: f64 = 1.5;
const RESUME_GAP_S: f64 = 2.0;

const EV_KEY: u16 = 1;
const EV_SW: u16 = 5;
const KEY_POWER: u16 = 116;
const SW_LID: u16 = 0;
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
const EVIOCGSW: libc::c_ulong = 0x8008_451B;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEvent {
    tv_sec: libc::c_long,
    tv_usec: libc::c_long,
    type_: u16,
    code: u16,
    value: i32,
}

fn clock_sec(clk: libc::clockid_t) -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(clk, &mut ts);
    }
    (ts.tv_sec as f64) + (ts.tv_nsec as f64) * 1e-9
}

#[inline]
fn boottime() -> f64 {
    clock_sec(libc::CLOCK_BOOTTIME)
}

#[inline]
fn slept_sec() -> f64 {
    clock_sec(libc::CLOCK_BOOTTIME) - clock_sec(libc::CLOCK_MONOTONIC)
}

pub fn is_panel_on() -> bool {
    fs::read_to_string(DPMS_PATH)
        .map(|s| s.trim() == "On")
        .unwrap_or(true)
}

fn find_pwrkey_dev() -> Option<PathBuf> {
    let entries = fs::read_dir("/sys/class/input").ok()?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|s| s.starts_with("event"))
        .collect();
    names.sort();
    for ev in names {
        let name_path = format!("/sys/class/input/{ev}/device/name");
        if fs::read_to_string(name_path).is_ok_and(|s| s.trim() == "pmic_pwrkey") {
            return Some(PathBuf::from(format!("/dev/input/{ev}")));
        }
    }
    None
}

/// Find any input device advertising `SW_LID` (`bit 0` in `capabilities/sw`, e.g.
/// `Lenovo Keyboard Pack For Yoga Tab Lid` on Pogo USB). Opened WITHOUT `EVIOCGRAB`
/// so `systemd-logind` continues receiving lid-switch suspend events unimpeded.
fn find_lid_dev() -> Option<PathBuf> {
    let entries = fs::read_dir("/sys/class/input").ok()?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|s| s.starts_with("event"))
        .collect();
    names.sort();
    for ev in names {
        let cap_path = format!("/sys/class/input/{ev}/device/capabilities/sw");
        let Ok(cap_txt) = fs::read_to_string(cap_path) else {
            continue;
        };
        let last_word = cap_txt.split_whitespace().last().unwrap_or("0");
        if u64::from_str_radix(last_word, 16).is_ok_and(|mask| (mask & (1u64 << SW_LID)) != 0) {
            return Some(PathBuf::from(format!("/dev/input/{ev}")));
        }
    }
    None
}

fn open_lid_readonly(p: &PathBuf) -> i32 {
    let Ok(cpath) = std::ffi::CString::new(p.to_string_lossy().as_bytes()) else {
        return -1;
    };
    unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) }
}

fn query_lid_closed_fd(fd: i32) -> Option<bool> {
    if fd < 0 {
        return None;
    }
    let mut bits: u64 = 0;
    let rc = unsafe { libc::ioctl(fd, EVIOCGSW as _, &mut bits as *mut u64) };
    if rc < 0 {
        None
    } else {
        Some((bits & (1u64 << SW_LID)) != 0)
    }
}

pub fn is_lid_closed(cached_lid_fd: &mut i32) -> bool {
    if let Some(closed) = query_lid_closed_fd(*cached_lid_fd) {
        return closed;
    }
    if *cached_lid_fd >= 0 {
        unsafe {
            libc::close(*cached_lid_fd);
        }
        *cached_lid_fd = -1;
    }
    if let Some(p) = find_lid_dev() {
        let fd = open_lid_readonly(&p);
        if fd >= 0 {
            *cached_lid_fd = fd;
            if let Some(closed) = query_lid_closed_fd(fd) {
                return closed;
            }
        }
    }
    false
}

/// Find active Niri IPC socket under `/run/user/*`, preferring `seat0`'s active logind session UID
/// so both `_greetd` login greeter and `debian` desktop session work seamlessly!
fn find_niri_socket() -> Option<PathBuf> {
    // 1. Check `/run/systemd/seats/seat0` -> `ACTIVE=<sid>` -> `/run/systemd/sessions/<sid>` -> `UID=<uid>` (0ms, no fork)
    if let Ok(seat_txt) = fs::read_to_string("/run/systemd/seats/seat0") {
        if let Some(sid) = seat_txt
            .lines()
            .find_map(|l| l.strip_prefix("ACTIVE="))
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Ok(sess_txt) = fs::read_to_string(format!("/run/systemd/sessions/{sid}")) {
                if let Some(uid) = sess_txt
                    .lines()
                    .find_map(|l| l.strip_prefix("UID="))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if let Ok(files) = fs::read_dir(format!("/run/user/{uid}")) {
                        for f in files.flatten() {
                            let s = f.file_name();
                            let s = s.to_string_lossy();
                            if s.starts_with("niri.") && s.ends_with(".sock") {
                                return Some(f.path());
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback: scan `/run/user/<uid>/niri.*.sock`
    let run_user = fs::read_dir("/run/user").ok()?;
    for entry in run_user.flatten() {
        let dir = entry.path();
        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let fname = f.file_name();
            let s = fname.to_string_lossy();
            if s.starts_with("niri.") && s.ends_with(".sock") {
                return Some(f.path());
            }
        }
    }
    None
}

/// Toggle Niri monitor power directly over `$NIRI_SOCKET` JSON IPC in < 0.3 ms!
fn set_niri_monitors(blank: bool, sock_path: &PathBuf) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock_path) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let payload = if blank {
        "{\"Action\":{\"PowerOffMonitors\":{}}}\n"
    } else {
        "{\"Action\":{\"PowerOnMonitors\":{}}}\n"
    };
    stream.write_all(payload.as_bytes()).is_ok()
}

pub fn set_screen_blank(blank: bool) {
    if let Some(sock) = find_niri_socket() {
        if set_niri_monitors(blank, &sock) {
            return;
        }
    }
    // Fallback for GDM / GNOME session if running
    let mode = if blank { "3" } else { "0" };
    let _ = Command::new("sh")
        .args([
            "-c",
            &format!(
                "sid=$(loginctl show-seat seat0 -p ActiveSession --value 2>/dev/null); \
                 uid=$(loginctl show-session \"$sid\" -p User --value 2>/dev/null); \
                 [ -n \"$uid\" ] && runuser -u \"$(id -nu \"$uid\")\" -- env DBUS_SESSION_BUS_ADDRESS=\"unix:path=/run/user/$uid/bus\" \
                 busctl --user set-property org.gnome.Mutter.DisplayConfig /org/gnome/Mutter/DisplayConfig org.gnome.Mutter.DisplayConfig PowerSaveMode i {mode} >/dev/null 2>&1"
            ),
        ])
        .status();
}

/// Resolve the primary KTZ8866 regmap debugfs file via `/sys/class/backlight/ktz8866-backlight/device`.
fn ktz_regmap_registers_path() -> PathBuf {
    if let Ok(dev) = fs::canonicalize(format!("{BL_DIR}/device")) {
        if let Some(name) = dev.file_name().and_then(|s| s.to_str()) {
            let candidate = PathBuf::from(format!("/sys/kernel/debug/regmap/{name}/registers"));
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from(KTZ_REGS_FALLBACK)
}

/// Read a kernel `debugfs` file via raw `libc::read` until `0` (true EOF).
///
/// Do NOT use `std::fs::read_to_string` on `/sys/kernel/debug/regmap/*/registers`:
/// kernel `regmap_read_debugfs` rounds each `read()` down to whole 7-byte lines (`"00: 00\n"`),
/// returning 56 bytes (`00..07`) into Rust's initial small buffer; `std::fs` treats a short read
/// on `S_IFREG` (`st_size = 0`) as EOF and stops before ever reaching `0x12`/`0x13`!
fn read_debugfs_all(path: &std::path::Path) -> Option<String> {
    let cpath = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let mut out = Vec::with_capacity(512);
    let mut buf = [0u8; 4096];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            out.extend_from_slice(&buf[..n as usize]);
            if out.len() > 65_536 {
                break;
            }
        } else if n == 0 {
            break;
        } else {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            unsafe {
                libc::close(fd);
            }
            return None;
        }
    }
    unsafe {
        libc::close(fd);
    }
    String::from_utf8(out).ok()
}

fn parse_ktz_pwm_zero(regs_txt: &str) -> bool {
    let (mut r12, mut r13) = (None, None);
    for line in regs_txt.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if let (Ok(reg), Ok(val)) = (
                u32::from_str_radix(k.trim(), 16),
                u32::from_str_radix(v.trim(), 16),
            ) {
                if reg == 0x12 {
                    r12 = Some(val);
                } else if reg == 0x13 {
                    r13 = Some(val);
                }
            }
        }
    }
    matches!((r12, r13), (Some(0), Some(0)))
}

fn panel_pwm_missing() -> bool {
    if !is_panel_on() {
        return false;
    }
    let bl_power = fs::read_to_string(format!("{BL_DIR}/bl_power")).unwrap_or_default();
    if bl_power.trim() != "0" {
        return false;
    }
    let bright: i32 = fs::read_to_string(format!("{BL_DIR}/brightness"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if bright <= 0 {
        return false;
    }
    let regs_path = ktz_regmap_registers_path();
    let Some(regs_txt) = read_debugfs_all(&regs_path) else {
        return false;
    };
    parse_ktz_pwm_zero(&regs_txt)
}

fn fix_dark_boot() {
    let deadline = boottime() + 90.0;
    while boottime() < deadline {
        if find_niri_socket().is_some() {
            if panel_pwm_missing() {
                std::thread::sleep(Duration::from_secs(2));
                if panel_pwm_missing() {
                    eprintln!("tb520fu-daemon[pwrkey]: dark boot detected (PWM2DIG=0) -> cycling panel");
                    set_screen_blank(true);
                    std::thread::sleep(Duration::from_millis(1500));
                    set_screen_blank(false);
                    return;
                }
            } else if is_panel_on() {
                return;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn lock_and_suspend() {
    eprintln!("tb520fu-daemon[pwrkey]: long press -> lock session (suspend disabled)");
    let _ = Command::new("pkill").args(["-USR1", "-x", "tb520fu-shell"]).status();
    let _ = Command::new("loginctl").arg("lock-sessions").status();
}

pub fn spawn_power_key_thread(shutdown: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("tb520fu-pwrkey".into())
        .spawn(move || {
            if boottime() < 300.0 {
                std::thread::Builder::new()
                    .name("tb520fu-darkboot".into())
                    .spawn(fix_dark_boot)
                    .ok();
            }

            let mut dev_path = loop {
                if shutdown.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(p) = find_pwrkey_dev() {
                    break p;
                }
                std::thread::sleep(Duration::from_secs(2));
            };

            let open_grab = |p: &PathBuf| -> i32 {
                let Ok(cpath) = std::ffi::CString::new(p.to_string_lossy().as_bytes()) else {
                    return -1;
                };
                let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
                if fd >= 0 {
                    let rc = unsafe { libc::ioctl(fd, EVIOCGRAB as _, 1 as libc::c_int) };
                    if rc == 0 {
                        eprintln!("tb520fu-daemon[pwrkey]: grabbed {}", p.display());
                    } else {
                        eprintln!("tb520fu-daemon[pwrkey]: opened {} (EVIOCGRAB busy)", p.display());
                    }
                }
                fd
            };

            let mut fd = open_grab(&dev_path);
            let mut lid_fd: i32 = find_lid_dev().map(|p| open_lid_readonly(&p)).unwrap_or(-1);
            let mut last_lid_probe = boottime();
            let mut press_t: Option<f64> = None;
            let mut long_fired = false;
            let mut last_slept = slept_sec();
            let mut ignore_until = 0.0_f64;

            let check_resume = |last_slept: &mut f64,
                                ignore_until: &mut f64,
                                press_t: &mut Option<f64>,
                                long_fired: &mut bool,
                                lid_fd: &mut i32| {
                let s = slept_sec();
                if s - *last_slept > RESUME_GAP_S {
                    *ignore_until = boottime() + RESUME_DEBOUNCE_S;
                    *press_t = None;
                    *long_fired = false;
                    if is_lid_closed(lid_fd) {
                        eprintln!(
                            "tb520fu-daemon[pwrkey]: resume detected (slept {:.1}s) while lid closed -> keep panel blank",
                            s - *last_slept
                        );
                        if is_panel_on() {
                            set_screen_blank(true);
                        }
                    } else {
                        eprintln!(
                            "tb520fu-daemon[pwrkey]: resume detected (slept {:.1}s) -> wake panel",
                            s - *last_slept
                        );
                        set_screen_blank(false);
                    }
                }
                *last_slept = s;
            };

            let ev_sz = std::mem::size_of::<InputEvent>();
            let mut ev_buf = [InputEvent::default(); 32];

            while !shutdown.load(Ordering::Relaxed) {
                if fd < 0 {
                    std::thread::sleep(Duration::from_millis(800));
                    if let Some(p) = find_pwrkey_dev() {
                        dev_path = p;
                    }
                    fd = open_grab(&dev_path);
                    continue;
                }

                let now = boottime();
                if lid_fd < 0 && now - last_lid_probe >= 5.0 {
                    last_lid_probe = now;
                    if let Some(p) = find_lid_dev() {
                        lid_fd = open_lid_readonly(&p);
                    }
                }

                check_resume(
                    &mut last_slept,
                    &mut ignore_until,
                    &mut press_t,
                    &mut long_fired,
                    &mut lid_fd,
                );
                let now = boottime();
                if let Some(t0) = press_t {
                    if !long_fired && (now - t0) * 1000.0 >= LONG_PRESS_MS {
                        long_fired = true;
                        lock_and_suspend();
                        check_resume(
                            &mut last_slept,
                            &mut ignore_until,
                            &mut press_t,
                            &mut long_fired,
                            &mut lid_fd,
                        );
                        press_t = None;
                        ignore_until = boottime() + RESUME_DEBOUNCE_S;
                    }
                }

                let timeout_ms = if press_t.is_some() && !long_fired { 40 } else { 2500 };
                let mut pfds = [
                    libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: lid_fd,
                        events: if lid_fd >= 0 { libc::POLLIN } else { 0 },
                        revents: 0,
                    },
                ];
                let nfds = if lid_fd >= 0 { 2 } else { 1 };
                let rc = unsafe { libc::poll(pfds.as_mut_ptr(), nfds, timeout_ms) };
                if rc <= 0 {
                    continue;
                }
                check_resume(
                    &mut last_slept,
                    &mut ignore_until,
                    &mut press_t,
                    &mut long_fired,
                    &mut lid_fd,
                );

                // 1. Handle Pogo folio lid switch (`SW_LID`: 1 = closed -> blank, 0 = opened -> wake)
                if lid_fd >= 0
                    && (pfds[1].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP)) != 0
                {
                    let n_lid = unsafe {
                        libc::read(
                            lid_fd,
                            ev_buf.as_mut_ptr() as *mut libc::c_void,
                            ev_sz * ev_buf.len(),
                        )
                    };
                    if n_lid <= 0 {
                        unsafe {
                            libc::close(lid_fd);
                        }
                        lid_fd = -1;
                        last_lid_probe = boottime();
                    } else {
                        let cnt = (n_lid as usize) / ev_sz;
                        for ev in &ev_buf[..cnt] {
                            if ev.type_ == EV_SW && ev.code == SW_LID {
                                let closed = ev.value != 0;
                                eprintln!(
                                    "tb520fu-daemon[pwrkey]: lid {} -> blank={closed}",
                                    if closed { "closed" } else { "opened" }
                                );
                                set_screen_blank(closed);
                            }
                        }
                    }
                }

                if (pfds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP)) == 0 {
                    continue;
                }

                let n = unsafe {
                    libc::read(
                        fd,
                        ev_buf.as_mut_ptr() as *mut libc::c_void,
                        ev_sz * ev_buf.len(),
                    )
                };
                if n <= 0 {
                    unsafe {
                        libc::close(fd);
                    }
                    fd = -1;
                    continue;
                }

                let count = (n as usize) / ev_sz;
                for ev in &ev_buf[..count] {
                    if ev.type_ != EV_KEY || ev.code != KEY_POWER {
                        continue;
                    }
                    let t = boottime();
                    if t < ignore_until {
                        continue;
                    }
                    if ev.value == 1 {
                        press_t = Some(t);
                        long_fired = false;
                    } else if ev.value == 0 {
                        if let Some(t0) = press_t {
                            if !long_fired {
                                let blank = is_panel_on();
                                eprintln!(
                                    "tb520fu-daemon[pwrkey]: short press ({:.0}ms) -> blank={blank}",
                                    (t - t0) * 1000.0
                                );
                                set_screen_blank(blank);
                            }
                        }
                        press_t = None;
                        long_fired = false;
                    }
                }
            }

            if lid_fd >= 0 {
                unsafe {
                    libc::close(lid_fd);
                }
            }
            if fd >= 0 {
                unsafe {
                    libc::close(fd);
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ktz_pwm_zero_rejects_truncated_debugfs_and_detects_real_dark_boot() {
        // Short-read truncated debugfs (`00..07`) must NEVER falsely trigger dark-boot cycling!
        let truncated = "00: 00\n01: 01\n02: bb\n03: 7d\n04: 07\n05: 55\n06: 22\n07: 00\n";
        assert!(!parse_ktz_pwm_zero(truncated));

        // Healthy lit panel (`PWM2DIG` = `0x07bb`)
        let lit = "00: 00\n08: 1f\n12: 03\n13: bb\n15: 9d\n";
        assert!(!parse_ktz_pwm_zero(lit));

        // Genuine dark-boot (`0x12 == 0` AND `0x13 == 0`)
        let dark = "00: 00\n08: 1f\n12: 00\n13: 00\n15: 9d\n";
        assert!(parse_ktz_pwm_zero(dark));
    }
}
