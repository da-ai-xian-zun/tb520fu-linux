// SPDX-License-Identifier: MIT
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const HW_CONF_PATH: &str = "/var/lib/tb520fu/hardware.json";

/// Lenovo/Huaqin SM8650 ADSP `0x60` (`BC_GENERIC_PROP_*`) battery properties exposed via
/// `/sys/bus/auxiliary/devices/pmic_glink.power-supply.*/lenovo_qi_{prop,value}`.
/// Note: Lenovo firmware hard-disables standard Qualcomm `0x48` charge limit (`msg.enable = 0` in
/// stock `qti_battery_charger.c`), leaving `qcom-battmgr-bat/charge_control_end_threshold` at an
/// unused default of `80`. Real charge protection on TB520FU uses `HQ_PROP_PROTECTION_SETTING*`.
pub const HQ_PROP_CHARGING_ENABLE: i32 = 28;
pub const HQ_PROP_PROTECTION_SETTING: i32 = 29;
pub const HQ_PROP_PROTECTION_SETTING_EU: i32 = 30;
pub const HQ_PROP_RECHARGE_SETTING: i32 = 31;
pub const HQ_PROP_BATTERY_MAINTENANCE: i32 = 32;
pub const HQ_PROP_BATTERY_MAINTENANCE20: i32 = 33;
pub const HQ_PROP_BATTERY_CV: i32 = 37;

pub const BYPASS_CHARGE_LIMIT_PCT: u8 = 90;

/// Thread-safe, mutex-serialized accessor for `pmic_glink.power-supply.*/lenovo_qi_{prop,value}`
/// shared between battery protection/maintenance controls and stylus Qi wireless charging so
/// two threads can never interleave selector (`lenovo_qi_prop`) and data (`lenovo_qi_value`) I/O.
#[derive(Default)]
pub struct LenovoHqSysfs {
    dir: Mutex<Option<PathBuf>>,
}

impl LenovoHqSysfs {
    pub fn new() -> Self {
        Self {
            dir: Mutex::new(None),
        }
    }

    fn resolve_dir_locked(slot: &mut Option<PathBuf>) -> Option<PathBuf> {
        if slot.is_none() {
            let entries = fs::read_dir("/sys/bus/auxiliary/devices").ok()?;
            for e in entries.flatten() {
                let p = e.path();
                if p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|s| s.starts_with("pmic_glink.power-supply."))
                    && p.join("lenovo_qi_value").exists()
                {
                    *slot = Some(p);
                    break;
                }
            }
        }
        slot.clone()
    }

    pub fn set(&self, prop: i32, value: i32) -> bool {
        for _ in 0..12 {
            {
                let mut guard = self.dir.lock().unwrap();
                if let Some(d) = Self::resolve_dir_locked(&mut guard) {
                    if fs::write(d.join("lenovo_qi_prop"), prop.to_string()).is_ok()
                        && fs::write(d.join("lenovo_qi_value"), value.to_string()).is_ok()
                    {
                        return true;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(120));
        }
        false
    }

    pub fn get(&self, prop: i32) -> Option<i32> {
        for _ in 0..12 {
            {
                let mut guard = self.dir.lock().unwrap();
                if let Some(d) = Self::resolve_dir_locked(&mut guard) {
                    if fs::write(d.join("lenovo_qi_prop"), prop.to_string()).is_ok() {
                        if let Ok(s) = fs::read_to_string(d.join("lenovo_qi_value")) {
                            if let Ok(v) = s.trim().parse::<i32>() {
                                return Some(v);
                            }
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(120));
        }
        None
    }

    pub fn set_pen_attached(&self, attached: bool) {
        let mut guard = self.dir.lock().unwrap();
        if let Some(d) = Self::resolve_dir_locked(&mut guard) {
            let _ = fs::write(d.join("pen_attached"), if attached { "1\n" } else { "0\n" });
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareConf {
    pub charge_limit: u8,
    #[serde(default)]
    pub battery_maintenance: bool,
}

impl Default for HardwareConf {
    fn default() -> Self {
        Self {
            charge_limit: 100,
            battery_maintenance: false,
        }
    }
}

impl HardwareConf {
    /// Load persisted `/var/lib/tb520fu/hardware.json`, or probe ADSP `HQ_PROP_PROTECTION_SETTING`
    /// (`095100` -> `100`, `085090` -> `90`) when no config exists yet (with Lenovo's long-charge
    /// `battery_maintenance` disabled by default). Returns `(conf, from_disk)`.
    pub fn load_or_probe(hq: &LenovoHqSysfs) -> (Self, bool) {
        if let Ok(s) = fs::read_to_string(HW_CONF_PATH) {
            if let Ok(mut parsed) = serde_json::from_str::<Self>(&s) {
                parsed.charge_limit = normalize_charge_limit(parsed.charge_limit);
                return (parsed, true);
            }
        }
        let probed = hq
            .get(HQ_PROP_PROTECTION_SETTING)
            .and_then(decode_protection_limit)
            .unwrap_or(100);
        (
            Self {
                charge_limit: probed,
                battery_maintenance: false,
            },
            false,
        )
    }

    pub fn save(&self) {
        let p = Path::new(HW_CONF_PATH);
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = PathBuf::from(format!("{HW_CONF_PATH}.tmp"));
            if fs::write(&tmp, s).is_ok() {
                let _ = fs::rename(&tmp, p);
            }
        }
    }
}

#[inline]
pub fn normalize_charge_limit(limit: u8) -> u8 {
    if limit < 100 {
        limit.clamp(60, 95)
    } else {
        100
    }
}

/// Encode `(encoded_rrrmmm, recharge_start_pct)` for Lenovo `HQ_PROP_PROTECTION_SETTING*`:
/// - `90%` limit -> `(85090, 85)` (`085090` = recharge at 85%, stop/bypass at 90%)
/// - `80%` limit -> `(75080, 75)` (`075080` = recharge at 75%, stop/bypass at 80%)
/// - `100%` normal -> `(95100, 95)` (`095100` = recharge at 95%, full at 100%)
#[inline]
pub fn encode_protection_setting(limit: u8) -> (i32, i32) {
    let stop = normalize_charge_limit(limit) as i32;
    let start = (stop - 5).max(50);
    (start * 1000 + stop, start)
}

#[inline]
pub fn decode_protection_limit(raw: i32) -> Option<u8> {
    if raw <= 0 {
        return None;
    }
    let stop = raw % 1000;
    if (50..=100).contains(&stop) {
        Some(normalize_charge_limit(stop as u8))
    } else {
        None
    }
}

/// Apply 90% / 100% battery charge protection ("旁路供电") to Lenovo ADSP firmware
/// (`HQ_PROP_PROTECTION_SETTING`, `HQ_PROP_PROTECTION_SETTING_EU`, `HQ_PROP_RECHARGE_SETTING`),
/// and mirror the target value onto standard sysfs `charge_control_end_threshold` for consistency.
pub fn apply_charge_limit(hq: &LenovoHqSysfs, limit: u8) -> bool {
    let normalized = normalize_charge_limit(limit);
    let (encoded, recharge_start) = encode_protection_setting(normalized);

    let mut wrote = false;
    if hq.set(HQ_PROP_PROTECTION_SETTING, encoded) {
        wrote = true;
    }
    let _ = hq.set(HQ_PROP_PROTECTION_SETTING_EU, encoded);
    let _ = hq.set(HQ_PROP_RECHARGE_SETTING, recharge_start);

    // Ensure charging itself remains enabled (`prop 28 = 1`) so ADSP regulates at the target stop %
    let _ = hq.set(HQ_PROP_CHARGING_ENABLE, 1);

    let candidates = [
        "/sys/class/power_supply/qcom-battmgr-bat/charge_control_end_threshold",
        "/sys/class/power_supply/battery/charge_control_end_threshold",
        "/sys/class/power_supply/qcom-battmgr-bat/charge_stop_threshold",
    ];
    for path in candidates {
        if Path::new(path).exists() && fs::write(path, format!("{normalized}\n")).is_ok() {
            wrote = true;
        }
    }
    wrote
}

pub fn read_charge_limit_from_adsp(hq: &LenovoHqSysfs, fallback: u8) -> u8 {
    hq.get(HQ_PROP_PROTECTION_SETTING)
        .and_then(decode_protection_limit)
        .unwrap_or(fallback)
}

pub fn read_battery_maintenance(hq: &LenovoHqSysfs) -> (bool, i32) {
    let maint = hq.get(HQ_PROP_BATTERY_MAINTENANCE).unwrap_or(0) != 0
        || hq.get(HQ_PROP_BATTERY_MAINTENANCE20).unwrap_or(0) != 0;
    let cv_mv = hq.get(HQ_PROP_BATTERY_CV).unwrap_or(0);
    (maint, cv_mv)
}

pub fn set_battery_maintenance(hq: &LenovoHqSysfs, enabled: bool) -> bool {
    let v = i32::from(enabled);
    let ok1 = hq.set(HQ_PROP_BATTERY_MAINTENANCE, v);
    let ok2 = hq.set(HQ_PROP_BATTERY_MAINTENANCE20, v);
    ok1 || ok2
}

fn find_flash_v4l_subdevs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/video4linux") else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let s = name.to_string_lossy();
        if !s.starts_with("v4l-subdev") {
            continue;
        }
        let sub_name = fs::read_to_string(e.path().join("name"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        if sub_name.contains("flash")
            || sub_name.contains("torch")
            || sub_name.contains("lm364")
            || sub_name.contains("aw365")
            || sub_name.contains("pm8550b")
        {
            out.push(PathBuf::from(format!("/dev/{s}")));
        }
    }
    out.sort();
    out
}

/// Toggle rear camera dual-LED torch (`v4l-subdev*` `led_mode=2`/`0` first since camera stack holds LED sysfs,
/// falling back to `/sys/class/leds/*torch*|*flash*`).
pub fn set_torch(on: bool) -> bool {
    // 1. V4L2 flash subdevices (`intensity_torch_mode=100000` + `led_mode=2`, or `led_mode=0`)
    let subdevs = find_flash_v4l_subdevs();
    if !subdevs.is_empty() {
        let mut any_ok = false;
        for subdev in subdevs {
            let dev_s = subdev.to_string_lossy().into_owned();
            let st = if on {
                Command::new("v4l2-ctl")
                    .args(["-d", &dev_s, "-c", "intensity_torch_mode=100000", "-c", "led_mode=2"])
                    .status()
            } else {
                Command::new("v4l2-ctl")
                    .args(["-d", &dev_s, "-c", "led_mode=0"])
                    .status()
            };
            if st.is_ok_and(|s| s.success()) {
                any_ok = true;
            }
        }
        if any_ok {
            return true;
        }
    }

    // 2. Fallback: sysfs LED torch nodes
    if let Ok(entries) = fs::read_dir("/sys/class/leds") {
        let mut hit = false;
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            if (n.contains("torch") || n.contains("flash")) && !n.contains("kbd") {
                let max_b = fs::read_to_string(e.path().join("max_brightness"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(100);
                let target = if on { (max_b / 2).max(1) } else { 0 };
                if fs::write(e.path().join("brightness"), format!("{target}\n")).is_ok() {
                    hit = true;
                }
            }
        }
        if hit {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_and_decode_lenovo_protection_setting() {
        assert_eq!(encode_protection_setting(BYPASS_CHARGE_LIMIT_PCT), (85_090, 85));
        assert_eq!(encode_protection_setting(80), (75_080, 75));
        assert_eq!(encode_protection_setting(100), (95_100, 95));
        assert_eq!(encode_protection_setting(60), (55_060, 55));

        assert_eq!(decode_protection_limit(95_100), Some(100));
        assert_eq!(decode_protection_limit(85_090), Some(90));
        assert_eq!(decode_protection_limit(75_080), Some(80));
        assert_eq!(decode_protection_limit(55_060), Some(60));
        assert_eq!(decode_protection_limit(0), None);
        assert_eq!(decode_protection_limit(-1), None);
    }
}
