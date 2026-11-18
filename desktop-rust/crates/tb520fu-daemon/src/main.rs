// SPDX-License-Identifier: MIT
pub mod autobright;
pub mod haptics_stylus;
pub mod hardware;
pub mod pen_charge;
pub mod power_key;

use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::autobright::{
    edr_active, edr_end, edr_set, edr_watchdog_tick, hbm_status, notify_als_for_hbm,
    read_backlight_raw, read_brightness, read_max_brightness, set_manual_hbm,
    spawn_autobright_task, write_brightness, AutoBrightConf, AutoBrightCore, SharedAutoBright,
};
use crate::haptics_stylus::{spawn_stylus_evdev_thread, RootHaptics, StylusInputState};
use crate::hardware::{
    apply_charge_limit, normalize_charge_limit, read_battery_maintenance, set_battery_maintenance,
    set_torch, HardwareConf, LenovoHqSysfs, BYPASS_CHARGE_LIMIT_PCT,
};
use crate::pen_charge::{spawn_pen_charge_thread, PenTelemetry};
use crate::power_key::{is_panel_on, spawn_power_key_thread};

pub const SOCK_PATH: &str = "/run/tb520fu-daemon.sock";
const SUSPEND_LOG_PATH: &str = "/var/lib/tb520fu/suspend-log.jsonl";

const SDAM0_NVMEM_PATH: &str = "/sys/bus/nvmem/devices/spmi_sdam0/nvmem";
const SDAM0_OF_NODE_LINK: &str = "/sys/bus/nvmem/devices/spmi_sdam0/of_node";
const SDAM0_EXPECTED_SUFFIX: &str = "/pmic@0/nvram@7100";
const BOOTFLAG_OFFSET: u64 = 0x50;
const BOOTFLAG_ANDROID_MAGIC: u8 = 0xa5;

fn verify_sdam0_node() -> Result<(), String> {
    let target = fs::canonicalize(SDAM0_OF_NODE_LINK)
        .map_err(|e| format!("canonicalize {SDAM0_OF_NODE_LINK}: {e}"))?;
    let s = target.to_string_lossy();
    if !s.ends_with(SDAM0_EXPECTED_SUFFIX) {
        return Err(format!("spmi_sdam0 is {s}, expected *{SDAM0_EXPECTED_SUFFIX}"));
    }
    Ok(())
}

fn sdam2_read_bootflag() -> Result<u8, String> {
    verify_sdam0_node()?;
    let mut f = fs::File::open(SDAM0_NVMEM_PATH)
        .map_err(|e| format!("open {SDAM0_NVMEM_PATH}: {e}"))?;
    f.seek(SeekFrom::Start(BOOTFLAG_OFFSET))
        .map_err(|e| format!("seek {BOOTFLAG_OFFSET:#x}: {e}"))?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b).map_err(|e| format!("read bootflag: {e}"))?;
    Ok(b[0])
}

fn sdam2_write_bootflag(val: u8) -> Result<(), String> {
    verify_sdam0_node()?;
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(SDAM0_NVMEM_PATH)
        .map_err(|e| format!("open rw {SDAM0_NVMEM_PATH}: {e}"))?;
    f.seek(SeekFrom::Start(BOOTFLAG_OFFSET))
        .map_err(|e| format!("seek {BOOTFLAG_OFFSET:#x}: {e}"))?;
    f.write_all(&[val]).map_err(|e| format!("write {val:#04x}: {e}"))?;
    f.seek(SeekFrom::Start(BOOTFLAG_OFFSET))
        .map_err(|e| format!("seek verify {BOOTFLAG_OFFSET:#x}: {e}"))?;
    let mut got = [0u8; 1];
    f.read_exact(&mut got).map_err(|e| format!("readback: {e}"))?;
    if got[0] != val {
        return Err(format!("bootflag readback {:#04x} != {val:#04x}", got[0]));
    }
    Ok(())
}

fn default_max_hbm() -> u8 {
    2
}

fn default_hbm_gain() -> f64 {
    1.0
}

static GLOBAL_SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigterm(_sig: libc::c_int) {
    GLOBAL_SHUTDOWN.store(true, Ordering::Relaxed);
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum DaemonReq {
    Status,
    SetAutobright { enabled: bool },
    SetBrightness { percent: u8 },
    SetHbm { level: u8 },
    /// Panel EDR for niri: `Some(raw)` raises the backlight to `raw` (with optional HBM 0..2 boost
    /// at full backlight) and keeps the user's brightness aside; `None` restores it.
    Edr {
        raw: Option<i64>,
        #[serde(default)]
        hbm: u8,
    },
    SetTorch { enabled: bool },
    ToggleTorch,
    SetChargeLimit { limit: u8 },
    SetBatteryMaintenance { enabled: bool },
    Haptic {
        side: String,
        strength: u16,
        duration_ms: u16,
    },
    GpuBoost {
        duration_ms: u16,
    },
    PowerOff,
    Reboot,
    RebootAndroid,
    WifiRescan,
    WifiConnect {
        ssid: String,
        #[serde(default)]
        password: Option<String>,
    },
}

const GPU_MIN_FREQ_PATH: &str = "/sys/class/devfreq/3d00000.gpu/min_freq";
const GPU_IDLE_MIN_HZ: u64 = 231_000_000;
const GPU_BOOST_MIN_HZ: u64 = 422_000_000;

struct GpuBooster {
    deadline: Mutex<Option<Instant>>,
    cvar: std::sync::Condvar,
}

impl GpuBooster {
    fn spawn(shutdown: Arc<AtomicBool>) -> Arc<Self> {
        let idle_hz = fs::read_to_string(GPU_MIN_FREQ_PATH)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&hz| hz > 0 && hz <= 350_000_000)
            .unwrap_or(GPU_IDLE_MIN_HZ);
        let me = Arc::new(Self {
            deadline: Mutex::new(None),
            cvar: std::sync::Condvar::new(),
        });
        let worker = me.clone();
        std::thread::Builder::new()
            .name("tb520fu-gpu-boost".into())
            .spawn(move || {
                let mut boosted = false;
                loop {
                    if shutdown.load(Ordering::Relaxed) {
                        if boosted {
                            let _ = fs::write(GPU_MIN_FREQ_PATH, format!("{idle_hz}\n"));
                        }
                        break;
                    }
                    let mut guard = worker.deadline.lock().unwrap();
                    match *guard {
                        None => {
                            if boosted {
                                let _ = fs::write(GPU_MIN_FREQ_PATH, format!("{idle_hz}\n"));
                                boosted = false;
                            }
                            let (g, _) = worker
                                .cvar
                                .wait_timeout(guard, Duration::from_millis(500))
                                .unwrap();
                            drop(g);
                        }
                        Some(until) => {
                            let now = Instant::now();
                            if now >= until {
                                *guard = None;
                                drop(guard);
                                if boosted {
                                    let _ = fs::write(GPU_MIN_FREQ_PATH, format!("{idle_hz}\n"));
                                    boosted = false;
                                }
                            } else {
                                if !boosted {
                                    let _ = fs::write(
                                        GPU_MIN_FREQ_PATH,
                                        format!("{GPU_BOOST_MIN_HZ}\n"),
                                    );
                                    boosted = true;
                                }
                                let wait_dur = until.duration_since(now).min(Duration::from_millis(250));
                                let (g, _) = worker.cvar.wait_timeout(guard, wait_dur).unwrap();
                                drop(g);
                            }
                        }
                    }
                }
            })
            .ok();
        me
    }

    fn boost(&self, duration_ms: u16) {
        let ms = duration_ms.clamp(80, 1500) as u64;
        let target = Instant::now() + Duration::from_millis(ms);
        let mut guard = self.deadline.lock().unwrap();
        if guard.is_none_or(|cur| target > cur) {
            *guard = Some(target);
            self.cvar.notify_one();
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub ok: bool,
    pub autobright: bool,
    pub lux: f64,
    pub factor: f64,
    pub brightness_pct: u8,
    /// The user's brightness, raw (unaffected by panel EDR).
    #[serde(default)]
    pub brightness_raw: i64,
    /// The backlight value in sysfs (raised above `brightness_raw` during panel EDR).
    #[serde(default)]
    pub backlight_raw: i64,
    #[serde(default)]
    pub max_brightness: i64,
    #[serde(default)]
    pub edr: bool,
    #[serde(default)]
    pub hbm: u8,
    #[serde(default = "default_max_hbm")]
    pub max_hbm: u8,
    #[serde(default = "default_hbm_gain")]
    pub hbm_gain: f64,
    #[serde(default)]
    pub hbm_throttled: bool,
    pub torch: bool,
    pub charge_limit: u8,
    #[serde(default)]
    pub battery_maintenance: bool,
    #[serde(default)]
    pub battery_cv_mv: i32,
    pub pen_docked: bool,
    pub pen_tx: bool,
    #[serde(default)]
    pub pen_charging: bool,
    pub pen_level: i32,
    pub pen_iin_ma: i32,
    #[serde(default)]
    pub pen_hover: bool,
    #[serde(default)]
    pub pen_action_seq: u64,
    #[serde(default)]
    pub pen_action_code: u32,
}

#[derive(Clone)]
struct HwRuntimeState {
    conf: HardwareConf,
    torch_on: bool,
    maint_active: bool,
    battery_cv_mv: i32,
}

struct DaemonCtx {
    ab: Arc<Mutex<SharedAutoBright>>,
    hq: Arc<LenovoHqSysfs>,
    pen: Arc<PenTelemetry>,
    stylus: Arc<StylusInputState>,
    haptics: RootHaptics,
    gpu_boost: Arc<GpuBooster>,
    hw: Mutex<HwRuntimeState>,
}

fn wake_eventfd(fd: i32) {
    if fd >= 0 {
        let one: u64 = 1;
        unsafe {
            let _ = libc::write(fd, &one as *const u64 as *const libc::c_void, 8);
        }
    }
}

fn read_i64_sysfs(path: &str) -> Option<i64> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
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

#[derive(Serialize)]
struct SuspendLogEntry {
    ts_start: i64,
    ts_end: i64,
    slept_s: f64,
    cap_before: i64,
    cap_after: i64,
    uah_before: i64,
    uah_after: i64,
    drain_mah: f64,
    avg_ma: f64,
}

fn append_suspend_log(entry: &SuspendLogEntry) {
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    let p = std::path::Path::new(SUSPEND_LOG_PATH);
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut lines: Vec<String> = fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect();
    lines.push(line);
    if lines.len() > 200 {
        let drop_n = lines.len() - 200;
        lines.drain(0..drop_n);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    let _ = fs::write(p, out);
}

async fn next_bus_msg(stream: &mut zbus::MessageStream) -> Option<zbus::Message> {
    loop {
        let item = std::future::poll_fn(|cx| {
            zbus::export::futures_core::Stream::poll_next(std::pin::Pin::new(&mut *stream), cx)
        })
        .await?;
        if let Ok(msg) = item {
            return Some(msg);
        }
    }
}

async fn acquire_sleep_inhibitor(conn: &zbus::Connection) -> Option<zbus::zvariant::OwnedFd> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Inhibit",
            &(
                "sleep",
                "tb520fu-daemon",
                "Pause stylus wireless TX and record battery before S3",
                "delay",
            ),
        )
        .await
        .ok()?;
    reply.body().deserialize::<zbus::zvariant::OwnedFd>().ok()
}

fn spawn_sleep_monitor(
    shutdown: Arc<AtomicBool>,
    sleeping: Arc<AtomicBool>,
    pen_wake_fd: i32,
    ctx: Arc<DaemonCtx>,
) {
    std::thread::Builder::new()
        .name("tb520fu-sleep-mon".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                let Ok(conn) = zbus::Connection::system().await else {
                    return;
                };
                let Ok(rule) = zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .interface("org.freedesktop.login1.Manager")
                    .and_then(|b| b.member("PrepareForSleep"))
                    .map(|b| b.build())
                else {
                    return;
                };
                let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule, &conn, Some(8)).await
                else {
                    return;
                };

                let mut inhibit_fd = acquire_sleep_inhibitor(&conn).await;
                let mut pre_sleep: Option<(i64, f64, i64, i64)> = None;
                let bat_dir = "/sys/class/power_supply/qcom-battmgr-bat";

                while !shutdown.load(Ordering::Relaxed) {
                    let Some(msg) = next_bus_msg(&mut stream).await else {
                        break;
                    };
                    let Ok(entering) = msg.body().deserialize::<bool>() else {
                        continue;
                    };
                    if entering {
                        sleeping.store(true, Ordering::Release);
                        wake_eventfd(pen_wake_fd);
                        for _ in 0..25 {
                            if !ctx.pen.tx_on.load(Ordering::Acquire) {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        let ts0 = clock_sec(libc::CLOCK_REALTIME).round() as i64;
                        let slept0 =
                            clock_sec(libc::CLOCK_BOOTTIME) - clock_sec(libc::CLOCK_MONOTONIC);
                        let cap0 = read_i64_sysfs(&format!("{bat_dir}/capacity")).unwrap_or(-1);
                        let uah0 =
                            read_i64_sysfs(&format!("{bat_dir}/charge_counter")).unwrap_or(-1);
                        pre_sleep = Some((ts0, slept0, cap0, uah0));
                        // Release delay lock immediately so S3 entry proceeds with zero delay
                        inhibit_fd = None;
                    } else {
                        sleeping.store(false, Ordering::Release);
                        wake_eventfd(pen_wake_fd);
                        if inhibit_fd.is_none() {
                            inhibit_fd = acquire_sleep_inhibitor(&conn).await;
                        }
                        if let Some((ts0, slept0, cap0, uah0)) = pre_sleep.take() {
                            let ts1 = clock_sec(libc::CLOCK_REALTIME).round() as i64;
                            let slept1 =
                                clock_sec(libc::CLOCK_BOOTTIME) - clock_sec(libc::CLOCK_MONOTONIC);
                            let dslept = (slept1 - slept0).max(0.0);
                            if dslept >= 1.0 {
                                let cap1 =
                                    read_i64_sysfs(&format!("{bat_dir}/capacity")).unwrap_or(-1);
                                let uah1 = read_i64_sysfs(&format!("{bat_dir}/charge_counter"))
                                    .unwrap_or(-1);
                                let drain_mah = if uah0 > 0 && uah1 > 0 {
                                    ((uah0 - uah1) as f64) / 1000.0
                                } else {
                                    0.0
                                };
                                let avg_ma = if dslept > 1.0 {
                                    drain_mah * 3600.0 / dslept
                                } else {
                                    0.0
                                };
                                append_suspend_log(&SuspendLogEntry {
                                    ts_start: ts0,
                                    ts_end: ts1,
                                    slept_s: (dslept * 10.0).round() / 10.0,
                                    cap_before: cap0,
                                    cap_after: cap1,
                                    uah_before: uah0,
                                    uah_after: uah1,
                                    drain_mah: (drain_mah * 100.0).round() / 100.0,
                                    avg_ma: (avg_ma * 100.0).round() / 100.0,
                                });
                            }
                        }

                        // Re-assert Lenovo ADSP battery settings across S3 resume
                        let conf = ctx.hw.lock().unwrap().conf.clone();
                        let _ = set_battery_maintenance(&ctx.hq, conf.battery_maintenance);
                        let _ = apply_charge_limit(&ctx.hq, conf.charge_limit);
                        let (maint_active, cv_mv) = read_battery_maintenance(&ctx.hq);
                        let mut hw = ctx.hw.lock().unwrap();
                        hw.maint_active = maint_active;
                        hw.battery_cv_mv = cv_mv;
                    }
                }
            });
        })
        .ok();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("ctl") {
        if let Err(e) = run_ctl(&args[1..]) {
            eprintln!("tb520fu-daemon ctl error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Install SIGTERM/SIGINT handler so wireless pen coil TX is guaranteed off on service stop!
    unsafe {
        libc::signal(libc::SIGTERM, on_sigterm as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_sigterm as *const () as libc::sighandler_t);
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_watcher = shutdown.clone();
    std::thread::spawn(move || {
        while !GLOBAL_SHUTDOWN.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
        shutdown_watcher.store(true, Ordering::Relaxed);
        // Connect to socket to unblock `accept()` immediately
        let _ = UnixStream::connect(SOCK_PATH);
    });

    // 0. Clear any residual one-shot Android bootflag left in PMK8550 SDAM2 (0x7150)
    if let Ok(BOOTFLAG_ANDROID_MAGIC) = sdam2_read_bootflag() {
        match sdam2_write_bootflag(0) {
            Ok(()) => eprintln!("tb520fu-daemon: cleared residual Android bootflag (0x7150=0xa5 -> 0x00)"),
            Err(e) => eprintln!("tb520fu-daemon: failed to clear residual bootflag: {e}"),
        }
    }

    // 1. Lenovo/Huaqin ADSP charger + Qi shared mutex & hardware preferences
    //    (Lenovo `battery_maintenance` disabled by default; `旁路供电` threshold = 90%)
    let hq = Arc::new(LenovoHqSysfs::new());
    let (hw_conf, had_conf) = HardwareConf::load_or_probe(&hq);
    if !had_conf {
        hw_conf.save();
    }
    let _ = set_battery_maintenance(&hq, hw_conf.battery_maintenance);
    let _ = apply_charge_limit(&hq, hw_conf.charge_limit);
    let (maint_active, battery_cv_mv) = read_battery_maintenance(&hq);

    // 2. Power key short-press DPMS + Pogo lid switch (`SW_LID`) + ktz8866 dark-boot recovery
    spawn_power_key_thread(shutdown.clone());

    // 3. Magnetic Hall sensor + TLMM GPIO 178 + Lenovo Qi stylus wireless charger
    let sleeping = Arc::new(AtomicBool::new(false));
    let pen_wake_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    let pen_telemetry = Arc::new(PenTelemetry::default());
    pen_telemetry.level.store(-1, Ordering::Relaxed);
    spawn_pen_charge_thread(
        shutdown.clone(),
        sleeping.clone(),
        pen_wake_fd,
        hq.clone(),
        pen_telemetry.clone(),
    );

    // 3b. Lenovo Tab Pen Plus hover + button evdev listener & dual aw86927 root haptics
    let stylus_state = Arc::new(StylusInputState::default());
    spawn_stylus_evdev_thread(shutdown.clone(), stylus_state.clone());
    let root_haptics = RootHaptics::spawn();

    // 4. Ambient light sensor auto-brightness (`iio-sensor-proxy` + `ktz8866-backlight`)
    let ab_conf = AutoBrightConf::load();
    let max_b = read_max_brightness();
    let ab_state = Arc::new(Mutex::new(SharedAutoBright {
        core: AutoBrightCore::new(&ab_conf, max_b, is_panel_on(), 0.0),
        last_lux: 0.0,
        claimed: false,
        t0: Instant::now(),
    }));
    spawn_autobright_task(ab_state.clone());
    {
        let shutdown = shutdown.clone();
        let _ = std::thread::Builder::new()
            .name("tb520fu-edr-watchdog".into())
            .spawn(move || {
                while !shutdown.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    edr_watchdog_tick();
                }
                edr_end();
            });
    }

    let gpu_booster = GpuBooster::spawn(shutdown.clone());

    let ctx = Arc::new(DaemonCtx {
        ab: ab_state,
        hq,
        pen: pen_telemetry,
        stylus: stylus_state,
        haptics: root_haptics,
        gpu_boost: gpu_booster,
        hw: Mutex::new(HwRuntimeState {
            conf: hw_conf,
            torch_on: false,
            maint_active,
            battery_cv_mv,
        }),
    });

    // 4b. `systemd-logind` `PrepareForSleep` monitor (pauses stylus wireless TX before S3 & logs battery drain)
    spawn_sleep_monitor(shutdown.clone(), sleeping, pen_wake_fd, ctx.clone());

    // 5. IPC Unix socket `/run/tb520fu-daemon.sock` (`0666` so unprivileged desktop shell talks directly)
    let _ = fs::remove_file(SOCK_PATH);
    let listener = match UnixListener::bind(SOCK_PATH) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tb520fu-daemon: cannot bind {SOCK_PATH}: {e}");
            std::process::exit(1);
        }
    };
    let _ = fs::set_permissions(SOCK_PATH, fs::Permissions::from_mode(0o666));
    eprintln!("tb520fu-daemon: started (listening on {SOCK_PATH})");

    for stream in listener.incoming() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(s) = stream {
            let ctx_cl = ctx.clone();
            std::thread::spawn(move || handle_client(s, &ctx_cl));
        }
    }

    // Turn off torch and wait 300ms for pen thread to de-assert GPIO 178
    wake_eventfd(pen_wake_fd);
    let _ = set_torch(false);
    let _ = fs::remove_file(SOCK_PATH);
    std::thread::sleep(Duration::from_millis(300));
    if pen_wake_fd >= 0 {
        unsafe {
            libc::close(pen_wake_fd);
        }
    }
}

fn build_status(ctx: &DaemonCtx) -> DaemonStatus {
    let (ab_on, lux, factor, max_b) = {
        let st = ctx.ab.lock().unwrap();
        (st.core.enabled, st.last_lux, st.core.factor, st.core.max)
    };
    let cur_b = read_brightness().unwrap_or(max_b / 2);
    let pct = ((cur_b as f64 / max_b.max(1) as f64) * 100.0).round().clamp(0.0, 100.0) as u8;
    let (hbm, max_hbm, hbm_gain, hbm_throttled) = hbm_status();
    let hw_st = ctx.hw.lock().unwrap().clone();
    let pen_docked = ctx.pen.docked.load(Ordering::Relaxed);
    let pen_tx = pen_docked && ctx.pen.tx_on.load(Ordering::Relaxed);
    let pen_charging = pen_tx && ctx.pen.charging.load(Ordering::Relaxed);
    let pen_level = if pen_docked {
        ctx.pen.level.load(Ordering::Relaxed)
    } else {
        -1
    };
    let pen_iin_ma = if pen_tx {
        ctx.pen.iin_ma.load(Ordering::Relaxed)
    } else {
        0
    };
    DaemonStatus {
        ok: true,
        autobright: ab_on,
        lux,
        factor,
        brightness_pct: pct,
        brightness_raw: cur_b,
        backlight_raw: read_backlight_raw().unwrap_or(cur_b),
        max_brightness: max_b,
        edr: edr_active(),
        hbm,
        max_hbm,
        hbm_gain,
        hbm_throttled,
        torch: hw_st.torch_on,
        charge_limit: hw_st.conf.charge_limit,
        battery_maintenance: hw_st.maint_active,
        battery_cv_mv: hw_st.battery_cv_mv,
        pen_docked,
        pen_tx,
        pen_charging,
        pen_level,
        pen_iin_ma,
        pen_hover: ctx.stylus.hover.load(Ordering::Relaxed),
        pen_action_seq: ctx.stylus.action_seq.load(Ordering::Acquire),
        pen_action_code: ctx.stylus.action_code.load(Ordering::Relaxed),
    }
}

fn handle_client(mut stream: UnixStream, ctx: &DaemonCtx) {
    // Keep persistent UI pollers (`tb520fu-shell` at 1.5s) alive across D-Bus bursts so they never
    // drop to raw sysfs `2047` (`100%`) while panel EDR is active; `EDR_WATCHDOG` runs separately at 2.5s.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let Ok(cloned) = stream.try_clone() else { return };
    let mut reader = BufReader::new(cloned);
    let mut line = String::new();
    let mut used_edr = false;
    while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
        if let Ok(req) = serde_json::from_str::<DaemonReq>(line.trim()) {
            match req {
                DaemonReq::Status => {}
                DaemonReq::SetAutobright { enabled } => {
                    let (factor, ema) = {
                        let mut st = ctx.ab.lock().unwrap();
                        let was_enabled = st.core.enabled;
                        st.core.enabled = enabled;
                        st.core.ramp.clear();
                        if enabled {
                            if !was_enabled {
                                st.core.factor = 1.0;
                            }
                            let now = st.now();
                            let cb = read_brightness();
                            st.core.ref_lux = None;
                            st.core.evaluate(cb, now);
                        }
                        (st.core.factor, st.core.ema)
                    };
                    notify_als_for_hbm(enabled, ema);
                    AutoBrightConf { enabled, factor }.save();
                }
                DaemonReq::SetBrightness { percent } => {
                    let pct = percent.clamp(1, 100);
                    let mut st = ctx.ab.lock().unwrap();
                    let raw = ((pct as f64 / 100.0) * (st.core.max as f64)).round() as i64;
                    write_brightness(raw);
                    if let Some(nf) = st.core.learn_owner_choice(raw) {
                        AutoBrightConf {
                            enabled: st.core.enabled,
                            factor: nf,
                        }
                        .save();
                    }
                }
                DaemonReq::SetHbm { level } => {
                    set_manual_hbm(level);
                }
                DaemonReq::Edr { raw, hbm } => match raw {
                    Some(raw) => {
                        used_edr = true;
                        edr_set(raw, hbm);
                    }
                    None => edr_end(),
                },
                DaemonReq::SetTorch { enabled } => {
                    let mut hw = ctx.hw.lock().unwrap();
                    let _ = set_torch(enabled);
                    hw.torch_on = enabled;
                }
                DaemonReq::ToggleTorch => {
                    let mut hw = ctx.hw.lock().unwrap();
                    let next = !hw.torch_on;
                    let _ = set_torch(next);
                    hw.torch_on = next;
                }
                DaemonReq::SetChargeLimit { limit } => {
                    let normalized = normalize_charge_limit(limit);
                    let _ = apply_charge_limit(&ctx.hq, normalized);
                    let mut hw = ctx.hw.lock().unwrap();
                    hw.conf.charge_limit = normalized;
                    hw.conf.save();
                }
                DaemonReq::SetBatteryMaintenance { enabled } => {
                    let _ = set_battery_maintenance(&ctx.hq, enabled);
                    let (maint_active, cv_mv) = read_battery_maintenance(&ctx.hq);
                    let mut hw = ctx.hw.lock().unwrap();
                    hw.conf.battery_maintenance = enabled;
                    hw.maint_active = maint_active;
                    hw.battery_cv_mv = cv_mv;
                    hw.conf.save();
                }
                DaemonReq::Haptic {
                    side,
                    strength,
                    duration_ms,
                } => {
                    ctx.haptics.pulse(side, strength, duration_ms);
                    line.clear();
                    continue;
                }
                DaemonReq::GpuBoost { duration_ms } => {
                    ctx.gpu_boost.boost(duration_ms);
                    line.clear();
                    continue;
                }
                DaemonReq::PowerOff => {
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_millis(80));
                        let _ = std::process::Command::new("/usr/bin/systemctl")
                            .arg("poweroff")
                            .status();
                    });
                }
                DaemonReq::Reboot => {
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_millis(80));
                        let _ = std::process::Command::new("/usr/bin/systemctl")
                            .arg("reboot")
                            .status();
                    });
                }
                DaemonReq::RebootAndroid => match sdam2_write_bootflag(BOOTFLAG_ANDROID_MAGIC) {
                    Ok(()) => {
                        eprintln!("tb520fu-daemon: set 0x7150=0xa5, rebooting to Android");
                        std::thread::spawn(|| {
                            std::thread::sleep(Duration::from_millis(80));
                            let _ = std::process::Command::new("/usr/bin/systemctl")
                                .arg("reboot")
                                .status();
                        });
                    }
                    Err(e) => {
                        eprintln!("tb520fu-daemon: RebootAndroid aborted ({e})");
                    }
                },
                DaemonReq::WifiRescan => {
                    std::thread::spawn(|| {
                        let _ = std::process::Command::new("nmcli")
                            .args(["device", "wifi", "rescan"])
                            .status();
                    });
                }
                DaemonReq::WifiConnect { ssid, password } => {
                    std::thread::spawn(move || {
                        let trimmed = ssid.trim();
                        if trimmed.is_empty() {
                            return;
                        }
                        if let Some(pw) = password.filter(|s| !s.is_empty()) {
                            let _ = std::process::Command::new("nmcli")
                                .args(["device", "wifi", "connect", trimmed, "password", &pw])
                                .status();
                        } else {
                            let ok = std::process::Command::new("nmcli")
                                .args(["connection", "up", trimmed])
                                .status()
                                .is_ok_and(|s| s.success());
                            if !ok {
                                let _ = std::process::Command::new("nmcli")
                                    .args(["device", "wifi", "connect", trimmed])
                                    .status();
                            }
                        }
                    });
                }
            }
            let resp = build_status(ctx);
            if let Ok(mut js) = serde_json::to_string(&resp) {
                js.push('\n');
                let _ = stream.write_all(js.as_bytes());
            }
        }
        line.clear();
    }
    // The EDR client (niri) went away: don't leave the backlight raised.
    if used_edr {
        edr_end();
    }
}

fn send_ipc(req: &DaemonReq) -> Result<DaemonStatus, String> {
    let mut s = UnixStream::connect(SOCK_PATH).map_err(|e| format!("connect {SOCK_PATH}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut js = serde_json::to_string(req).map_err(|e| e.to_string())?;
    js.push('\n');
    s.write_all(js.as_bytes()).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(s);
    let mut reply = String::new();
    reader.read_line(&mut reply).map_err(|e| e.to_string())?;
    serde_json::from_str(&reply).map_err(|e| e.to_string())
}

fn run_ctl(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    let req = match sub {
        "status" => DaemonReq::Status,
        "autobright" => {
            let on = args.get(1).map(|s| s.as_str()) != Some("off");
            DaemonReq::SetAutobright { enabled: on }
        }
        "brightness" => {
            let pct: u8 = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or("usage: tb520fu-daemon ctl brightness <1..100>")?;
            DaemonReq::SetBrightness { percent: pct }
        }
        "hbm" => {
            let lv: u8 = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or("usage: tb520fu-daemon ctl hbm <0..2>")?;
            DaemonReq::SetHbm { level: lv }
        }
        "edr" => {
            let raw = match args.get(1).map(|s| s.as_str()) {
                None | Some("off") => None,
                Some(v) => Some(v.parse::<i64>().map_err(|_| "usage: tb520fu-daemon ctl edr <raw|off> [hbm]")?),
            };
            let hbm = args
                .get(2)
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(0)
                .min(2);
            DaemonReq::Edr { raw, hbm }
        }
        "torch" => match args.get(1).map(|s| s.as_str()) {
            Some("on") => DaemonReq::SetTorch { enabled: true },
            Some("off") => DaemonReq::SetTorch { enabled: false },
            _ => DaemonReq::ToggleTorch,
        },
        "charge-limit" => {
            let lim: u8 = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(BYPASS_CHARGE_LIMIT_PCT);
            DaemonReq::SetChargeLimit { limit: lim }
        }
        "maintenance" => {
            let on = matches!(args.get(1).map(|s| s.as_str()), Some("on" | "1" | "true"));
            DaemonReq::SetBatteryMaintenance { enabled: on }
        }
        "reboot-android" => DaemonReq::RebootAndroid,
        other => return Err(format!("unknown subcommand '{other}'")),
    };
    let status = send_ipc(&req)?;
    println!("{}", serde_json::to_string_pretty(&status).unwrap());
    Ok(())
}
