// SPDX-License-Identifier: MIT
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const DAEMON_SOCK: &str = "/run/tb520fu-daemon.sock";
pub const SUSPEND_LOG_PATH: &str = "/var/lib/tb520fu/suspend-log.jsonl";
const BAT_DIR: &str = "/sys/class/power_supply/qcom-battmgr-bat";
const USB_DIR: &str = "/sys/class/power_supply/qcom-battmgr-usb";

fn default_max_hbm() -> u8 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub ok: bool,
    pub autobright: bool,
    pub lux: f64,
    pub factor: f64,
    pub brightness_pct: u8,
    #[serde(default)]
    pub edr: bool,
    #[serde(default)]
    pub hbm: u8,
    #[serde(default = "default_max_hbm")]
    pub max_hbm: u8,
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
}

#[derive(Debug, Clone, Default)]
pub struct BatterySnapshot {
    pub percent: u8,
    pub charging: bool,
    pub usb_online: bool,
    pub bypass_active: bool,
    pub watts: f32,
    pub temp_c: f32,
    pub health_pct: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SuspendRecord {
    pub ts_start: i64,
    pub ts_end: i64,
    pub slept_s: f64,
    pub cap_before: i64,
    pub cap_after: i64,
    pub uah_before: i64,
    pub uah_after: i64,
    pub drain_mah: f64,
    pub avg_ma: f64,
}

#[derive(Debug, Clone)]
pub struct FormattedSuspendRow {
    pub started_at: String,
    pub duration: String,
    pub delta_cap: String,
    pub avg_power: String,
}

#[derive(Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum DaemonReq<'a> {
    Status,
    SetAutobright { enabled: bool },
    SetBrightness { percent: u8 },
    SetHbm { level: u8 },
    SetChargeLimit { limit: u8 },
    RebootAndroid,
    WifiRescan,
    WifiConnect {
        ssid: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        password: Option<&'a str>,
    },
}

fn send_req(req: &DaemonReq<'_>) -> Result<DaemonStatus, String> {
    let mut s =
        UnixStream::connect(DAEMON_SOCK).map_err(|e| format!("connect {DAEMON_SOCK}: {e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(1800)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(1800)));
    let mut js = serde_json::to_string(req).map_err(|e| e.to_string())?;
    js.push('\n');
    s.write_all(js.as_bytes()).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(s);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    serde_json::from_str(line.trim()).map_err(|e| e.to_string())
}

pub fn get_status() -> Result<DaemonStatus, String> {
    send_req(&DaemonReq::Status)
}

pub fn set_brightness(percent: u8) -> Result<DaemonStatus, String> {
    send_req(&DaemonReq::SetBrightness {
        percent: percent.clamp(1, 100),
    })
}

pub fn set_autobright(enabled: bool) -> Result<DaemonStatus, String> {
    send_req(&DaemonReq::SetAutobright { enabled })
}

pub fn set_hbm(level: u8) -> Result<DaemonStatus, String> {
    send_req(&DaemonReq::SetHbm {
        level: level.min(2),
    })
}

pub fn set_bypass_charge(enabled: bool) -> Result<DaemonStatus, String> {
    send_req(&DaemonReq::SetChargeLimit {
        limit: if enabled { 90 } else { 100 },
    })
}

pub fn wifi_rescan() -> Result<DaemonStatus, String> {
    send_req(&DaemonReq::WifiRescan)
}

pub fn wifi_connect(ssid: &str, password: Option<&str>) -> Result<DaemonStatus, String> {
    send_req(&DaemonReq::WifiConnect { ssid, password })
}

pub fn reboot_android() -> Result<DaemonStatus, String> {
    send_req(&DaemonReq::RebootAndroid)
}

fn read_i64(path: &str) -> Option<i64> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
}

pub fn read_battery_snapshot(charge_limit: u8) -> BatterySnapshot {
    let pct = read_i64(&format!("{BAT_DIR}/capacity"))
        .unwrap_or(90)
        .clamp(0, 100) as u8;
    let usb_online = read_i64(&format!("{USB_DIR}/online")).unwrap_or(0) > 0;
    let raw_status = fs::read_to_string(format!("{BAT_DIR}/status")).unwrap_or_default();
    let st = raw_status.trim();
    let current_ua = read_i64(&format!("{BAT_DIR}/current_now"));
    let power_uw = read_i64(&format!("{BAT_DIR}/power_now")).filter(|uw| *uw != 0);
    let derived_uw = match (current_ua, read_i64(&format!("{BAT_DIR}/voltage_now"))) {
        (Some(ua), Some(uv)) => Some(((ua.abs() as f64) * (uv.abs() as f64) / 1e6) as i64),
        _ => None,
    };
    let watts = power_uw
        .or(derived_uw)
        .map(|uw| ((uw.abs() as f32 / 1_000_000.0) * 10.0).round() * 0.1)
        .unwrap_or(0.0);

    let bypass_active = usb_online && charge_limit > 0 && charge_limit < 100 && pct >= charge_limit;
    let charging = !bypass_active && st.eq_ignore_ascii_case("Charging");

    let temp_c = read_i64(&format!("{BAT_DIR}/temp"))
        .map(|t| (t as f32) * 0.1)
        .unwrap_or(28.0);

    let full = read_i64(&format!("{BAT_DIR}/charge_full")).unwrap_or(0);
    let design = read_i64(&format!("{BAT_DIR}/charge_full_design")).unwrap_or(0);
    let health_pct = if full > 0 && design > 0 {
        (((full as f64 / design as f64) * 100.0).round().clamp(50.0, 100.0)) as u8
    } else {
        100
    };

    BatterySnapshot {
        percent: pct,
        charging,
        usb_online,
        bypass_active,
        watts,
        temp_c,
        health_pct,
    }
}

fn format_epoch_local(ts: i64) -> String {
    unsafe {
        let t = ts as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return ts.to_string();
        }
        format!(
            "{:02}-{:02} {:02}:{:02}",
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min
        )
    }
}

pub fn format_suspend_record(r: &SuspendRecord) -> FormattedSuspendRow {
    let mins = (r.slept_s / 60.0).round() as i64;
    let duration = if mins < 1 {
        format!("{:.0} 秒", r.slept_s)
    } else if mins < 60 {
        format!("{mins} 分钟")
    } else {
        format!("{} 小时 {:02} 分", mins / 60, mins % 60)
    };

    let dcap = r.cap_after - r.cap_before;
    let delta_cap = if r.cap_before >= 0 && r.cap_after >= 0 {
        if dcap > 0 || r.drain_mah < -5.0 {
            format!("{}% → {}% (充电)", r.cap_before, r.cap_after)
        } else {
            format!("{}% → {}% ({}%)", r.cap_before, r.cap_after, dcap)
        }
    } else {
        "—".into()
    };

    let avg_power = if r.drain_mah < -5.0 {
        format!("+{:.0} mAh 补电", -r.drain_mah)
    } else if r.avg_ma > 0.01 {
        let mw = r.avg_ma * 3.85;
        format!("~{:.0} mW ({:.1} mA)", mw, r.avg_ma)
    } else {
        "< 35 mW".into()
    };

    FormattedSuspendRow {
        started_at: format_epoch_local(r.ts_start),
        duration,
        delta_cap,
        avg_power,
    }
}

pub fn read_suspend_logs(max_rows: usize) -> Vec<FormattedSuspendRow> {
    let Ok(raw) = fs::read_to_string(SUSPEND_LOG_PATH) else {
        return Vec::new();
    };
    let mut parsed: Vec<SuspendRecord> = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<SuspendRecord>(line.trim()).ok())
        .filter(|r| r.slept_s >= 60.0)
        .collect();
    // If no >=60s sessions exist yet, fall back to showing recent shorter test sleeps.
    if parsed.is_empty() {
        parsed = raw
            .lines()
            .filter_map(|line| serde_json::from_str::<SuspendRecord>(line.trim()).ok())
            .collect();
    }
    parsed
        .iter()
        .rev()
        .take(max_rows)
        .map(format_suspend_record)
        .collect()
}
