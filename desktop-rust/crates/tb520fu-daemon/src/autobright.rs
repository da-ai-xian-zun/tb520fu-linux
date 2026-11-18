// SPDX-License-Identifier: MIT
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::power_key::is_panel_on;

const BL_DIR: &str = "/sys/class/backlight/ktz8866-backlight";
const HBM_PATH: &str = "/sys/class/backlight/ktz8866-backlight/hbm";
const MAX_HBM_PATH: &str = "/sys/class/backlight/ktz8866-backlight/max_hbm";
const BAT_TEMP_PATH: &str = "/sys/class/power_supply/qcom-battmgr-bat/temp";
const CONF_PATH: &str = "/var/lib/tb520fu/autobright.json";

/// KTZ8866 full-scale LED current ratios (`0x9d` = 20.4 mA ~650 nit, `0xcd` = 25.2 mA ~803 nit,
/// `0xed` = 28.4 mA ~905 nit) from Lenovo Android DT `hbm-levels = <0x9d 0xcd 0xed>`.
pub const HBM_GAINS: [f64; 3] = [1.0, 25.2 / 20.4, 28.4 / 20.4];
const HBM_WINDOW_S: f64 = 1800.0;
const HBM_BUDGET_S: f64 = 300.0;
const HBM_STEP1_FLOOR_S: f64 = 18.0;
const HBM_REENTER_FLOOR_S: f64 = 30.0;
const HBM_KEEPALIVE_S: f64 = 60.0;
const HBM_MANUAL_MAX_S: f64 = 300.0;

/// Interpolated over `log10(lux + 1)`, ported 1:1 from Y700 `autobright`.
const CURVE: [(f64, f64); 9] = [
    (0.0, 0.06),
    (3.0, 0.10),
    (10.0, 0.16),
    (40.0, 0.26),
    (100.0, 0.36),
    (300.0, 0.50),
    (800.0, 0.65),
    (2000.0, 0.82),
    (5000.0, 1.00),
];
const BAND: f64 = 0.25;
const BRIGHTEN_S: f64 = 1.5;
const DARKEN_S: f64 = 4.0;
const TAU_S: f64 = 1.5;
const MIN_FRAC: f64 = 0.02;
const MIN_STEP: f64 = 0.015;
const RAMP_STEPS: usize = 8;
const RAMP_S: f64 = 0.10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoBrightConf {
    pub enabled: bool,
    pub factor: f64,
}

impl Default for AutoBrightConf {
    fn default() -> Self {
        Self {
            enabled: true,
            factor: 1.0,
        }
    }
}

impl AutoBrightConf {
    pub fn load() -> Self {
        fs::read_to_string(CONF_PATH)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let p = Path::new(CONF_PATH);
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = PathBuf::from(format!("{CONF_PATH}.tmp"));
            if fs::write(&tmp, s).is_ok() {
                let _ = fs::rename(&tmp, p);
            }
        }
    }
}

pub fn curve(lux: f64) -> f64 {
    let x = (lux.max(0.0) + 1.0).log10();
    let pts = CURVE.map(|(l, f)| ((l + 1.0).log10(), f));
    if x <= pts[0].0 {
        return pts[0].1;
    }
    for w in pts.windows(2) {
        let ((x0, f0), (x1, f1)) = (w[0], w[1]);
        if x <= x1 {
            return f0 + (f1 - f0) * (x - x0) / (x1 - x0);
        }
    }
    pts[pts.len() - 1].1
}

#[inline]
fn clamp_factor(f: f64) -> f64 {
    f.clamp(0.25, 8.0)
}

pub fn read_max_brightness() -> i64 {
    fs::read_to_string(format!("{BL_DIR}/max_brightness"))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&m| m > 0)
        .unwrap_or(2047)
}

fn sysfs_read_brightness() -> Option<i64> {
    fs::read_to_string(format!("{BL_DIR}/brightness"))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
}

fn sysfs_write_brightness(raw: i64) {
    let _ = fs::write(format!("{BL_DIR}/brightness"), format!("{raw}\n"));
}

/// Panel EDR (HDR headroom requested by niri): while active, the backlight runs at the level
/// niri asks for and the user's brightness is kept here instead of in sysfs. The slider and
/// auto-brightness keep working on `user_raw` through [`read_brightness`]/[`write_brightness`];
/// niri dims SDR content by `user / backlight` so the desktop looks unchanged.
struct EdrOverride {
    user_raw: i64,
    last_req: Instant,
}

static EDR: Mutex<Option<EdrOverride>> = Mutex::new(None);

/// Restore the user's brightness if niri stops refreshing its request (crash, hang).
const EDR_WATCHDOG: Duration = Duration::from_millis(2500);

/// The user's brightness (not the EDR backlight level while EDR is active).
pub fn read_brightness() -> Option<i64> {
    if let Some(o) = EDR.lock().unwrap().as_ref() {
        return Some(o.user_raw);
    }
    sysfs_read_brightness()
}

/// Sets the user's brightness (deferred until EDR ends while EDR is active).
pub fn write_brightness(val: i64) {
    let max = read_max_brightness();
    let clamped = val.clamp(1, max);
    let mut edr = EDR.lock().unwrap();
    if let Some(o) = edr.as_mut() {
        o.user_raw = clamped;
        if sysfs_read_brightness().unwrap_or(0) < clamped {
            sysfs_write_brightness(clamped);
        }
        return;
    }
    sysfs_write_brightness(clamped);
}

/// The backlight value actually in sysfs.
pub fn read_backlight_raw() -> Option<i64> {
    sysfs_read_brightness()
}

pub fn edr_active() -> bool {
    EDR.lock().unwrap().is_some()
}

#[inline]
pub fn hbm_gain_for_level(level: u8) -> f64 {
    HBM_GAINS.get(level as usize).copied().unwrap_or(1.0)
}

pub fn read_max_hbm() -> u8 {
    fs::read_to_string(MAX_HBM_PATH)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(2)
        .clamp(0, 2) as u8
}

fn sysfs_read_hbm() -> u8 {
    fs::read_to_string(HBM_PATH)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, 2) as u8
}

fn sysfs_write_hbm(level: u8) {
    let _ = fs::write(HBM_PATH, format!("{level}\n"));
}

fn read_battery_temp_c() -> f64 {
    fs::read_to_string(BAT_TEMP_PATH)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(|t10| (t10 as f64) * 0.1)
        .unwrap_or(30.0)
}

fn mono_secs() -> f64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Rolling duty-cycle & thermal safety policy for KTZ8866 High Brightness Mode (`0x15` 20.4/25.2/28.4 mA):
/// - Caps cumulative HBM (`level >= 1`) to 300s per rolling 1800s window (`5 min / 30 min`).
/// - Smoothly steps `2 -> 1` in the final 18s of budget, latches `0` at exhaustion until >=30s recovers.
/// - Steps down on battery thermal rise (`>= 41.5°C` -> max 1, `>= 43.5°C` -> 0 with hysteresis).
/// - Refreshes kernel `hbm` sysfs every 60s while active so kernel's 300s safety net never cliff-drops mid-scene.
pub struct HbmPolicy {
    history: VecDeque<(f64, f64)>,
    pub manual_level: u8,
    manual_until: Option<f64>,
    pub edr_level: u8,
    pub als_level: u8,
    als_hi_since: Option<f64>,
    als_autobright: bool,
    als_ema_lux: Option<f64>,
    exhausted_latched: bool,
    thermal_cap: u8,
    pub applied_level: u8,
    pub throttled: bool,
    last_tick_s: f64,
    last_write_s: f64,
}

impl HbmPolicy {
    pub fn new(now_s: f64) -> Self {
        Self {
            history: VecDeque::with_capacity(64),
            manual_level: 0,
            manual_until: None,
            edr_level: 0,
            als_level: 0,
            als_hi_since: None,
            als_autobright: true,
            als_ema_lux: None,
            exhausted_latched: false,
            thermal_cap: 2,
            applied_level: 0,
            throttled: false,
            last_tick_s: now_s,
            last_write_s: now_s,
        }
    }

    pub fn used_budget_secs(&self) -> f64 {
        self.history.iter().map(|&(_, d)| d).sum()
    }

    fn prune_and_account(&mut self, now_s: f64) {
        let dt = (now_s - self.last_tick_s).clamp(0.0, 10.0);
        self.last_tick_s = now_s;
        if self.applied_level > 0 && dt > 0.0 {
            self.history.push_back((now_s, dt));
        }
        let cutoff = now_s - HBM_WINDOW_S;
        while let Some(&(end_t, dur)) = self.history.front() {
            let start_t = end_t - dur;
            if end_t <= cutoff {
                self.history.pop_front();
            } else if start_t < cutoff {
                let keep = (end_t - cutoff).max(0.0);
                self.history[0] = (end_t, keep);
                break;
            } else {
                break;
            }
        }
    }

    pub fn set_manual(&mut self, level: u8, now_s: f64) {
        let lv = level.min(2);
        self.manual_level = lv;
        self.manual_until = (lv > 0).then_some(now_s + HBM_MANUAL_MAX_S);
    }

    pub fn set_edr(&mut self, level: u8) {
        self.edr_level = level.min(2);
    }

    pub fn update_als(&mut self, autobright_on: bool, ema_lux: Option<f64>) {
        self.als_autobright = autobright_on;
        self.als_ema_lux = ema_lux;
    }

    pub fn eval(
        &mut self,
        panel_on: bool,
        max_hbm: u8,
        bat_temp_c: f64,
        now_s: f64,
    ) -> Option<u8> {
        self.prune_and_account(now_s);

        if self.manual_until.is_some_and(|u| now_s >= u) {
            self.manual_level = 0;
            self.manual_until = None;
        }

        // Update sunlight ALS auto-HBM (`>= 11k lux` -> 1, `>= 22k lux` -> 2, `< 7.5k lux` -> 0)
        if !panel_on || !self.als_autobright {
            self.als_level = 0;
            self.als_hi_since = None;
        } else if let Some(lux) = self.als_ema_lux {
            if lux < 7_500.0 {
                self.als_level = 0;
                self.als_hi_since = None;
            } else if lux >= 11_000.0 {
                let since = *self.als_hi_since.get_or_insert(now_s);
                if now_s - since >= 2.5 {
                    self.als_level = if lux >= 22_000.0 { 2 } else { 1 };
                }
            }
        }

        // Thermal hysteresis
        if bat_temp_c >= 43.5 {
            self.thermal_cap = 0;
        } else if bat_temp_c >= 41.5 {
            self.thermal_cap = self.thermal_cap.min(1);
        } else if self.thermal_cap == 0 && bat_temp_c <= 41.2 {
            self.thermal_cap = if bat_temp_c <= 39.8 { 2 } else { 1 };
        } else if self.thermal_cap == 1 && bat_temp_c <= 39.8 {
            self.thermal_cap = 2;
        }

        // Rolling 300s / 1800s duty-cycle budget
        let remaining = (HBM_BUDGET_S - self.used_budget_secs()).max(0.0);
        if remaining <= 0.05 {
            self.exhausted_latched = true;
        } else if self.exhausted_latched && remaining >= HBM_REENTER_FLOOR_S {
            self.exhausted_latched = false;
        }
        let budget_cap: u8 = if self.exhausted_latched {
            0
        } else if remaining < HBM_STEP1_FLOOR_S {
            1
        } else {
            2
        };

        let wanted = if panel_on {
            self.manual_level
                .max(self.edr_level)
                .max(self.als_level)
                .min(max_hbm.min(2))
        } else {
            0
        };
        let target = wanted.min(self.thermal_cap).min(budget_cap);
        self.throttled = wanted > 0 && target < wanted;

        let need_write = target != self.applied_level
            || (target > 0 && now_s - self.last_write_s >= HBM_KEEPALIVE_S);
        self.applied_level = target;
        if need_write {
            self.last_write_s = now_s;
            Some(target)
        } else {
            None
        }
    }
}

static HBM_POLICY: Mutex<Option<HbmPolicy>> = Mutex::new(None);

fn drive_hbm_locked(pol: &mut HbmPolicy, now_s: f64) -> (u8, bool) {
    let max_hbm = read_max_hbm();
    let panel_on = is_panel_on();
    let temp_c = read_battery_temp_c();
    if let Some(lv) = pol.eval(panel_on, max_hbm, temp_c, now_s) {
        sysfs_write_hbm(lv);
    } else if !panel_on && pol.applied_level == 0 && sysfs_read_hbm() != 0 {
        sysfs_write_hbm(0);
    }
    (pol.applied_level, pol.throttled)
}

pub fn set_manual_hbm(level: u8) {
    let now_s = mono_secs();
    let mut guard = HBM_POLICY.lock().unwrap();
    let pol = guard.get_or_insert_with(|| HbmPolicy::new(now_s));
    pol.set_manual(level, now_s);
    let _ = drive_hbm_locked(pol, now_s);
}

pub fn notify_als_for_hbm(autobright_on: bool, ema_lux: Option<f64>) {
    let now_s = mono_secs();
    let mut guard = HBM_POLICY.lock().unwrap();
    let pol = guard.get_or_insert_with(|| HbmPolicy::new(now_s));
    pol.update_als(autobright_on, ema_lux);
    let _ = drive_hbm_locked(pol, now_s);
}

pub fn hbm_status() -> (u8, u8, f64, bool) {
    let max_hbm = read_max_hbm();
    let actual = sysfs_read_hbm().min(max_hbm);
    let throttled = HBM_POLICY
        .lock()
        .unwrap()
        .as_ref()
        .map(|p| p.throttled)
        .unwrap_or(false);
    (actual, max_hbm, hbm_gain_for_level(actual), throttled)
}

/// Starts or updates panel EDR with the given backlight level (never below the user's) and
/// optional HBM boost (`0..=2`, applied only when `target == max_brightness` and policy allows).
pub fn edr_set(raw: i64, want_hbm: u8) {
    let max = read_max_brightness();
    let target = {
        let mut edr = EDR.lock().unwrap();
        let o = edr.get_or_insert_with(|| {
            let user_raw = sysfs_read_brightness().unwrap_or(max / 2).clamp(1, max);
            eprintln!("tb520fu-daemon: panel EDR start (user brightness {user_raw})");
            EdrOverride {
                user_raw,
                last_req: Instant::now(),
            }
        });
        o.last_req = Instant::now();
        let t = raw.clamp(1, max).max(o.user_raw);
        if sysfs_read_brightness() != Some(t) {
            sysfs_write_brightness(t);
        }
        t
    };

    let eff_edr_hbm = if target >= max { want_hbm.min(2) } else { 0 };
    let now_s = mono_secs();
    let mut guard = HBM_POLICY.lock().unwrap();
    let pol = guard.get_or_insert_with(|| HbmPolicy::new(now_s));
    pol.set_edr(eff_edr_hbm);
    let _ = drive_hbm_locked(pol, now_s);
}

/// Ends panel EDR, drops EDR HBM boost, and puts the user's brightness back.
pub fn edr_end() {
    let restored = {
        let mut edr = EDR.lock().unwrap();
        edr.take().map(|o| o.user_raw)
    };
    {
        let now_s = mono_secs();
        let mut guard = HBM_POLICY.lock().unwrap();
        let pol = guard.get_or_insert_with(|| HbmPolicy::new(now_s));
        if pol.edr_level != 0 {
            pol.set_edr(0);
            let _ = drive_hbm_locked(pol, now_s);
        }
    }
    if let Some(user_raw) = restored {
        sysfs_write_brightness(user_raw);
        eprintln!("tb520fu-daemon: panel EDR end (brightness {user_raw})");
    }
}

pub fn edr_watchdog_tick() {
    let expired = EDR
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|o| o.last_req.elapsed() > EDR_WATCHDOG);
    if expired {
        eprintln!("tb520fu-daemon: panel EDR watchdog expired");
        edr_end();
    }
    let now_s = mono_secs();
    let mut guard = HBM_POLICY.lock().unwrap();
    let pol = guard.get_or_insert_with(|| HbmPolicy::new(now_s));
    let _ = drive_hbm_locked(pol, now_s);
}

pub struct AutoBrightCore {
    pub enabled: bool,
    pub max: i64,
    pub factor: f64,
    pub was_on: bool,
    pub ema: Option<f64>,
    pub ref_lux: Option<f64>,
    pub out_since: Option<f64>,
    pub ramp: VecDeque<f64>,
    pub ramp_at: f64,
    pub last_set_raw: Option<i64>,
    pub last_t: f64,
}

impl AutoBrightCore {
    pub fn new(conf: &AutoBrightConf, max: i64, screen_on: bool, now: f64) -> Self {
        Self {
            enabled: conf.enabled,
            max: max.max(1),
            factor: clamp_factor(conf.factor),
            was_on: screen_on,
            ema: None,
            ref_lux: None,
            out_since: None,
            ramp: VecDeque::new(),
            ramp_at: 0.0,
            last_set_raw: read_brightness(),
            last_t: now,
        }
    }

    pub fn target_frac(&self, lux: f64) -> f64 {
        (curve(lux) * self.factor).clamp(MIN_FRAC, 1.0)
    }

    pub fn on_reading(&mut self, lux: f64, cur_raw: Option<i64>, now: f64) {
        let dt = (now - self.last_t).max(0.0);
        self.last_t = now;
        self.ema = Some(match self.ema {
            None => lux,
            Some(e) => e + (1.0 - (-dt / TAU_S).exp()) * (lux - e),
        });
        if self.enabled {
            self.evaluate(cur_raw, now);
        }
    }

    pub fn on_screen(&mut self, on: bool, cur_raw: Option<i64>, now: f64) -> bool {
        let changed = on != self.was_on;
        if changed {
            self.ema = None;
            self.ref_lux = None;
            self.out_since = None;
            self.ramp.clear();
        }
        self.was_on = on;
        if on && self.enabled {
            self.evaluate(cur_raw, now);
        }
        changed
    }

    /// Called when the user drags the brightness slider (via IPC or external sysfs write).
    /// Returns `Some(new_factor)` if `factor` changed enough to persist!
    pub fn learn_owner_choice(&mut self, new_raw: i64) -> Option<f64> {
        self.ramp.clear();
        self.last_set_raw = Some(new_raw);
        let ema = self.ema?.max(0.0);
        let f = clamp_factor((new_raw as f64 / self.max as f64) / curve(ema).max(1e-3));
        self.ref_lux = Some(ema);
        self.out_since = None;
        if (f - self.factor).abs() > 0.02 {
            self.factor = f;
            Some(f)
        } else {
            None
        }
    }

    fn threshold(&self, ema: f64, r: f64) -> f64 {
        if ema > r * (1.0 + BAND) + 2.0 {
            BRIGHTEN_S
        } else {
            DARKEN_S
        }
    }

    pub fn evaluate(&mut self, cur_raw: Option<i64>, now: f64) {
        if !self.enabled || !self.was_on || !self.ramp.is_empty() {
            return;
        }
        let Some(ema) = self.ema else { return };
        let Some(r) = self.ref_lux else {
            self.start_ramp(ema, cur_raw, now);
            return;
        };
        let (hi, lo) = (r * (1.0 + BAND) + 2.0, r * (1.0 - BAND) - 2.0);
        if lo <= ema && ema <= hi {
            self.out_since = None;
            return;
        }
        let since = *self.out_since.get_or_insert(now);
        if now >= since + self.threshold(ema, r) {
            self.start_ramp(ema, cur_raw, now);
        }
    }

    fn start_ramp(&mut self, lux: f64, cur_raw: Option<i64>, now: f64) {
        self.ref_lux = Some(lux);
        self.out_since = None;
        let cur = cur_raw.unwrap_or(self.max / 2) as f64 / self.max as f64;
        let tgt = self.target_frac(lux);
        if (tgt - cur).abs() < MIN_STEP {
            return;
        }
        self.ramp = (1..=RAMP_STEPS)
            .map(|i| cur + (tgt - cur) * (i as f64) / (RAMP_STEPS as f64))
            .collect();
        self.ramp_at = now + RAMP_S;
    }

    /// Step ramp / hysteresis timers; returns `Some(raw_brightness_to_write)` when stepping.
    pub fn step_timer(&mut self, cur_raw: Option<i64>, now: f64) -> Option<i64> {
        if !self.enabled || !self.was_on {
            self.ramp.clear();
            return None;
        }
        // Detect if external tool (`brightnessctl`) changed sysfs behind our back
        if self.ramp.is_empty() {
            if let (Some(actual), Some(expected)) = (cur_raw, self.last_set_raw) {
                if (actual - expected).abs() > (self.max / 50).max(12) {
                    if let Some(nf) = self.learn_owner_choice(actual) {
                        let conf = AutoBrightConf {
                            enabled: self.enabled,
                            factor: nf,
                        };
                        conf.save();
                    }
                    return None;
                }
            }
        }
        if !self.ramp.is_empty() && now >= self.ramp_at {
            let frac = self.ramp.pop_front().unwrap();
            let raw = (frac * self.max as f64).round() as i64;
            let clamped = raw.clamp(1, self.max);
            self.last_set_raw = Some(clamped);
            if !self.ramp.is_empty() {
                self.ramp_at = now + RAMP_S;
            } else {
                self.evaluate(Some(clamped), now);
            }
            return Some(clamped);
        }
        self.evaluate(cur_raw, now);
        None
    }
}

pub struct SharedAutoBright {
    pub core: AutoBrightCore,
    pub last_lux: f64,
    pub claimed: bool,
    pub t0: Instant,
}

impl SharedAutoBright {
    pub fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }
}

pub fn spawn_autobright_task(state: Arc<Mutex<SharedAutoBright>>) {
    std::thread::Builder::new()
        .name("tb520fu-als".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async move {
                let conn = zbus::Connection::system().await.ok();
                let mut claimed_on_bus = false;

                loop {
                    let (want_claim, sleep_ms) = {
                        let mut st = state.lock().unwrap();
                        let now = st.now();
                        let panel_on = is_panel_on();
                        let cur_b = read_brightness();
                        st.core.on_screen(panel_on, cur_b, now);
                        if let Some(next_b) = st.core.step_timer(cur_b, now) {
                            write_brightness(next_b);
                        }
                        let want = st.core.enabled && panel_on;
                        let ms = if !st.core.ramp.is_empty() {
                            95
                        } else if want {
                            750
                        } else {
                            2000
                        };
                        (want, ms)
                    };

                    if let Some(c) = &conn {
                        if want_claim != claimed_on_bus {
                            let method = if want_claim { "ClaimLight" } else { "ReleaseLight" };
                            let ok = c
                                .call_method(
                                    Some("net.hadess.SensorProxy"),
                                    "/net/hadess/SensorProxy",
                                    Some("net.hadess.SensorProxy"),
                                    method,
                                    &(),
                                )
                                .await
                                .is_ok();
                            if ok {
                                claimed_on_bus = want_claim;
                                state.lock().unwrap().claimed = claimed_on_bus;
                            }
                        }

                        if claimed_on_bus {
                            if let Ok(reply) = c
                                .call_method(
                                    Some("net.hadess.SensorProxy"),
                                    "/net/hadess/SensorProxy",
                                    Some("org.freedesktop.DBus.Properties"),
                                    "Get",
                                    &("net.hadess.SensorProxy", "LightLevel"),
                                )
                                .await
                            {
                                if let Ok(val) = reply.body().deserialize::<zbus::zvariant::OwnedValue>() {
                                    if let Ok(lux) = f64::try_from(val) {
                                        let (ab_en, ema_lux) = {
                                            let mut st = state.lock().unwrap();
                                            st.last_lux = lux;
                                            let now = st.now();
                                            let cur_b = read_brightness();
                                            st.core.on_reading(lux, cur_b, now);
                                            (st.core.enabled, st.core.ema)
                                        };
                                        notify_als_for_hbm(ab_en, ema_lux);
                                    }
                                }
                            }
                        }
                    }

                    tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                }
            });
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_hysteresis_and_owner_learning_work() {
        assert!((curve(0.0) - 0.06).abs() < 1e-6);
        assert!((curve(300.0) - 0.50).abs() < 0.02);
        assert!((curve(5000.0) - 1.0).abs() < 1e-6);

        let conf = AutoBrightConf {
            enabled: true,
            factor: 1.0,
        };
        let mut core = AutoBrightCore::new(&conf, 2000, true, 0.0);
        // Initial reading at 300 lux when screen is at 200 (10%) -> starts 8-step ramp immediately!
        core.on_reading(300.0, Some(200), 0.0);
        assert_eq!(core.ramp.len(), 8);

        let mut last = 200;
        let mut t = 0.0;
        while !core.ramp.is_empty() {
            t += 0.11;
            if let Some(v) = core.step_timer(Some(last), t) {
                last = v;
            }
        }
        assert!((last - 1000).abs() <= 20, "300 lux at factor 1.0 -> ~50% (1000/2000), got {last}");

        // Small +15% lux wobble inside +-25% band -> no ramp
        core.on_reading(340.0, Some(last), t + 2.0);
        assert!(core.ramp.is_empty());

        // Owner drags slider to 1400 (70%) at ~340 lux -> learns factor > 1.3
        let learned = core.learn_owner_choice(1400);
        assert!(learned.is_some());
        assert!(core.factor > 1.25 && core.factor < 1.50);
    }

    #[test]
    fn hbm_policy_enforces_rolling_budget_keepalive_and_thermal_stepdown() {
        let mut pol = HbmPolicy::new(0.0);
        pol.set_edr(2);
        assert_eq!(pol.eval(true, 2, 31.0, 0.0), Some(2));
        assert!((hbm_gain_for_level(2) * 650.0 - 904.9).abs() < 1.0);

        // 60s keepalive refreshes kernel sysfs before 300s driver timeout
        let mut t = 0.0;
        let mut keepalive_writes = 0;
        while t < 275.0 {
            t += 5.0;
            if pol.eval(true, 2, 31.0, t) == Some(2) {
                keepalive_writes += 1;
            }
        }
        assert!(keepalive_writes >= 4, "expected >=4 keepalive writes in 275s");
        assert_eq!(pol.applied_level, 2);

        // Final 18s of 300s budget smoothly steps 2 -> 1
        assert_eq!(pol.eval(true, 2, 31.0, 280.0), None);
        assert_eq!(pol.eval(true, 2, 31.0, 285.0), Some(1));
        assert!(pol.throttled);

        // At 300s budget exhaustion -> steps down to 0 and latches
        assert_eq!(pol.eval(true, 2, 31.0, 293.0), None);
        assert_eq!(pol.eval(true, 2, 31.0, 300.5), Some(0));
        assert_eq!(pol.applied_level, 0);

        // After rolling 1800s window slides past >= 30s of old usage (t = 1835), recovers!
        assert_eq!(pol.eval(true, 2, 31.0, 1835.0), Some(2));
        assert!(!pol.throttled);

        // Battery thermal guard: 42.0C -> cap 1, 44.0C -> cap 0
        assert_eq!(pol.eval(true, 2, 42.0, 1836.0), Some(1));
        assert_eq!(pol.eval(true, 2, 44.0, 1837.0), Some(0));
        assert!(pol.throttled);
    }
}
