// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tb520fu_ui::color::{AccentColor, Skin};
use tb520fu_ui::widgets::BitmapIcon;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::niri_ipc::{set_dsi_refresh_hz, set_dsi_transform};
use crate::notifications::{HwOsdCmd, ShellBusEvent};

const DAEMON_SOCK: &str = "/run/tb520fu-daemon.sock";

fn default_true() -> bool {
    true
}

fn default_win_width_presets() -> Vec<u8> {
    vec![33, 50, 67, 100]
}

fn default_wallpaper_spec() -> String {
    "stock".into()
}

fn default_dnd_start() -> String {
    "23:00".into()
}

fn default_dnd_end() -> String {
    "07:30".into()
}

fn default_lock_notif_mode() -> String {
    "full".into()
}

fn default_banner_timeout_ms() -> u32 {
    4200
}

fn default_seen_notif_apps() -> Vec<String> {
    vec!["系统通知".into(), "日历".into()]
}

static LIVE_PREFS_SNAP: OnceLock<Mutex<ShellPrefs>> = OnceLock::new();
static EMITTED_PREFS_JSON: OnceLock<Mutex<Option<serde_json::Map<String, serde_json::Value>>>> =
    OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchMode {
    #[default]
    NewCol,
    Stack,
    NewWs,
}

impl LaunchMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::NewCol => "新分栏",
            Self::Stack => "叠放",
            Self::NewWs => "新工作区",
        }
    }

    pub const ALL: [Self; 3] = [Self::NewCol, Self::Stack, Self::NewWs];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppCategory {
    #[default]
    All,
    Fav,
    Dev,
    Office,
    Media,
    System,
}

impl AppCategory {
    pub const ALL: [Self; 6] = [
        Self::All,
        Self::Fav,
        Self::Dev,
        Self::Office,
        Self::Media,
        Self::System,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Fav => "常用",
            Self::Dev => "开发与终端",
            Self::Office => "办公与笔记",
            Self::Media => "影音与创作",
            Self::System => "系统工具",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellPrefs {
    pub skin: Skin,
    #[serde(default)]
    pub accent: AccentColor,
    #[serde(default = "default_wallpaper_spec")]
    pub wallpaper: String,
    pub auto_rotate: bool,
    pub dnd: bool,
    #[serde(default = "default_true")]
    pub dnd_fullscreen: bool,
    #[serde(default)]
    pub dnd_schedule_enabled: bool,
    #[serde(default = "default_dnd_start")]
    pub dnd_start: String,
    #[serde(default = "default_dnd_end")]
    pub dnd_end: String,
    #[serde(default = "default_lock_notif_mode")]
    pub lock_notif_mode: String,
    #[serde(default = "default_banner_timeout_ms")]
    pub banner_timeout_ms: u32,
    #[serde(default = "default_seen_notif_apps")]
    pub seen_notif_apps: Vec<String>,
    #[serde(default)]
    pub muted_notif_apps: Vec<String>,
    #[serde(default = "default_true")]
    pub standby_companion: bool,
    #[serde(default)]
    pub night_light: bool,
    #[serde(default)]
    pub reading_mode: bool,
    #[serde(default = "default_true")]
    pub high_refresh_144: bool,
    /// Bottom corner stylus/touch hotkeys — OFF by default per user spec!
    #[serde(default)]
    pub pen_corners: bool,
    /// Stylus floating quick-action orb — OFF by default per user spec!
    #[serde(default)]
    pub pen_orb: bool,
    #[serde(default)]
    pub live_captions: bool,
    #[serde(default)]
    pub launch_mode: LaunchMode,
    #[serde(default = "default_win_width_presets")]
    pub win_width_presets: Vec<u8>,
    pub favorites: Vec<String>,
}

impl Default for ShellPrefs {
    fn default() -> Self {
        Self {
            skin: Skin::GlassDark,
            accent: AccentColor::Blue,
            wallpaper: default_wallpaper_spec(),
            auto_rotate: true,
            dnd: false,
            dnd_fullscreen: true,
            dnd_schedule_enabled: false,
            dnd_start: default_dnd_start(),
            dnd_end: default_dnd_end(),
            lock_notif_mode: default_lock_notif_mode(),
            banner_timeout_ms: default_banner_timeout_ms(),
            seen_notif_apps: default_seen_notif_apps(),
            muted_notif_apps: Vec::new(),
            standby_companion: true,
            night_light: false,
            reading_mode: false,
            high_refresh_144: true,
            pen_corners: false,
            pen_orb: false,
            live_captions: false,
            launch_mode: LaunchMode::NewCol,
            win_width_presets: default_win_width_presets(),
            favorites: vec![
                "fyi.tb520fu.Settings".into(),
                "foot".into(),
                "firefox".into(),
                "chromium".into(),
                "org.gnome.Nautilus".into(),
                "mpv".into(),
                "code".into(),
            ],
        }
    }
}

fn parse_bool_pref(v: &str) -> Option<bool> {
    match v.trim() {
        "true" | "1" | "\"true\"" => Some(true),
        "false" | "0" | "\"false\"" => Some(false),
        _ => None,
    }
}

fn parse_width_presets_input(raw: &str) -> Option<Vec<u8>> {
    let trimmed = raw.trim();
    let nums: Vec<u16> = if let Ok(v) = serde_json::from_str::<Vec<u16>>(trimmed) {
        v
    } else if let Ok(s) = serde_json::from_str::<String>(trimmed) {
        s.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|tok| !tok.is_empty())
            .map(|tok| tok.trim_end_matches('%').parse::<u16>())
            .collect::<Result<Vec<u16>, _>>()
            .ok()?
    } else {
        trimmed
            .trim_matches(|c| c == '[' || c == ']')
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|tok| !tok.is_empty())
            .map(|tok| tok.trim_end_matches('%').parse::<u16>())
            .collect::<Result<Vec<u16>, _>>()
            .ok()?
    };
    let mut cleaned: Vec<u8> = nums
        .into_iter()
        .map(|n| n.clamp(20, 100) as u8)
        .collect();
    cleaned.sort_unstable();
    cleaned.dedup();
    if !(2..=5).contains(&cleaned.len()) {
        return None;
    }
    Some(cleaned)
}

pub fn cached_prefs_json() -> String {
    let guard = LIVE_PREFS_SNAP
        .get_or_init(|| Mutex::new(ShellPrefs::default()))
        .lock()
        .unwrap();
    serde_json::to_string(&*guard).unwrap_or_else(|_| "{}".into())
}

pub fn current_lock_notif_mode() -> String {
    LIVE_PREFS_SNAP
        .get_or_init(|| Mutex::new(ShellPrefs::default()))
        .lock()
        .unwrap()
        .lock_notif_mode
        .clone()
}

pub fn current_accent() -> AccentColor {
    LIVE_PREFS_SNAP
        .get_or_init(|| Mutex::new(ShellPrefs::default()))
        .lock()
        .unwrap()
        .accent
}

pub fn parse_hm_minutes(hm: &str) -> Option<u16> {
    let (h_s, m_s) = hm.trim().split_once(':')?;
    let h: u16 = h_s.parse().ok()?;
    let m: u16 = m_s.parse().ok()?;
    if h >= 24 || m >= 60 {
        return None;
    }
    Some(h * 60 + m)
}

pub fn is_hm_in_window(now_mins: u16, start_hm: &str, end_hm: &str) -> bool {
    let Some(s) = parse_hm_minutes(start_hm) else {
        return false;
    };
    let Some(e) = parse_hm_minutes(end_hm) else {
        return false;
    };
    if s == e {
        return false;
    }
    if s < e {
        now_mins >= s && now_mins < e
    } else {
        // Overnight span (e.g., 23:00 -> 07:30)
        now_mins >= s || now_mins < e
    }
}

pub fn local_minutes_of_day() -> u16 {
    let mut now: libc::time_t = 0;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::time(&mut now);
        libc::localtime_r(&now, &mut tm);
    }
    (tm.tm_hour.clamp(0, 23) as u16) * 60 + (tm.tm_min.clamp(0, 59) as u16)
}

pub fn validate_and_stage_set_pref(key: &str, value_json: &str) -> bool {
    let slot = LIVE_PREFS_SNAP.get_or_init(|| Mutex::new(ShellPrefs::default()));
    let mut guard = slot.lock().unwrap();
    let mut candidate = guard.clone();
    if !candidate.apply_set_pref(key, value_json) {
        return false;
    }
    *guard = candidate;
    true
}

impl ShellPrefs {
    pub fn path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".config/tb520fu/shell.json")
    }

    pub fn normalized_width_presets(&self) -> Vec<u8> {
        let mut out: Vec<u8> = self
            .win_width_presets
            .iter()
            .map(|&n| n.clamp(20, 100))
            .collect();
        out.sort_unstable();
        out.dedup();
        out.truncate(5);
        if out.len() < 2 {
            default_win_width_presets()
        } else {
            out
        }
    }

    pub fn preset_pct_to_proportion(pct: u8) -> f64 {
        match pct {
            33 => 33.3333,
            67 => 66.6667,
            p => (p.clamp(20, 100)) as f64,
        }
    }

    pub fn apply_set_pref(&mut self, key: &str, value_json: &str) -> bool {
        let trimmed = value_json.trim();
        match key.trim() {
            "skin" => {
                if let Ok(sk) = serde_json::from_str::<Skin>(trimmed) {
                    self.skin = sk;
                    true
                } else {
                    match trimmed {
                        "glass-dark" => {
                            self.skin = Skin::GlassDark;
                            true
                        }
                        "glass-light" => {
                            self.skin = Skin::GlassLight;
                            true
                        }
                        "solid-dark" => {
                            self.skin = Skin::SolidDark;
                            true
                        }
                        "solid-light" => {
                            self.skin = Skin::SolidLight;
                            true
                        }
                        _ => false,
                    }
                }
            }
            "auto_rotate" => parse_bool_pref(trimmed).map(|v| self.auto_rotate = v).is_some(),
            "dnd" => parse_bool_pref(trimmed).map(|v| self.dnd = v).is_some(),
            "standby_companion" => parse_bool_pref(trimmed)
                .map(|v| self.standby_companion = v)
                .is_some(),
            "night_light" => parse_bool_pref(trimmed).map(|v| self.night_light = v).is_some(),
            "reading_mode" => parse_bool_pref(trimmed).map(|v| self.reading_mode = v).is_some(),
            "high_refresh_144" => parse_bool_pref(trimmed)
                .map(|v| self.high_refresh_144 = v)
                .is_some(),
            "pen_corners" => parse_bool_pref(trimmed).map(|v| self.pen_corners = v).is_some(),
            "pen_orb" => parse_bool_pref(trimmed).map(|v| self.pen_orb = v).is_some(),
            "live_captions" => parse_bool_pref(trimmed)
                .map(|v| self.live_captions = v)
                .is_some(),
            "launch_mode" => {
                if let Ok(lm) = serde_json::from_str::<LaunchMode>(trimmed) {
                    self.launch_mode = lm;
                    true
                } else {
                    match trimmed {
                        "new-col" => {
                            self.launch_mode = LaunchMode::NewCol;
                            true
                        }
                        "stack" => {
                            self.launch_mode = LaunchMode::Stack;
                            true
                        }
                        "new-ws" => {
                            self.launch_mode = LaunchMode::NewWs;
                            true
                        }
                        _ => false,
                    }
                }
            }
            "win_width_presets" => {
                let Some(presets) = parse_width_presets_input(trimmed) else {
                    return false;
                };
                self.win_width_presets = presets;
                true
            }
            "favorites" => {
                let Ok(favs) = serde_json::from_str::<Vec<String>>(trimmed) else {
                    return false;
                };
                self.favorites = favs;
                true
            }
            "accent" => {
                let raw = serde_json::from_str::<String>(trimmed)
                    .unwrap_or_else(|_| trimmed.trim_matches('"').to_string());
                if !matches!(
                    raw.trim(),
                    "blue" | "indigo" | "teal" | "amber" | "rose" | "emerald"
                ) {
                    return false;
                }
                self.accent = AccentColor::from_name(&raw);
                true
            }
            "wallpaper" => {
                let raw = serde_json::from_str::<String>(trimmed)
                    .unwrap_or_else(|_| trimmed.trim_matches('"').to_string());
                let clean = raw.trim();
                if clean.is_empty() || clean.len() > 512 {
                    return false;
                }
                self.wallpaper = clean.to_string();
                true
            }
            "dnd_fullscreen" => parse_bool_pref(trimmed)
                .map(|v| self.dnd_fullscreen = v)
                .is_some(),
            "dnd_schedule_enabled" => parse_bool_pref(trimmed)
                .map(|v| self.dnd_schedule_enabled = v)
                .is_some(),
            "dnd_start" => {
                let raw = serde_json::from_str::<String>(trimmed)
                    .unwrap_or_else(|_| trimmed.trim_matches('"').to_string());
                if parse_hm_minutes(&raw).is_none() {
                    return false;
                }
                self.dnd_start = raw.trim().to_string();
                true
            }
            "dnd_end" => {
                let raw = serde_json::from_str::<String>(trimmed)
                    .unwrap_or_else(|_| trimmed.trim_matches('"').to_string());
                if parse_hm_minutes(&raw).is_none() {
                    return false;
                }
                self.dnd_end = raw.trim().to_string();
                true
            }
            "lock_notif_mode" => {
                let raw = serde_json::from_str::<String>(trimmed)
                    .unwrap_or_else(|_| trimmed.trim_matches('"').to_string());
                if !matches!(raw.trim(), "full" | "redact" | "hide") {
                    return false;
                }
                self.lock_notif_mode = raw.trim().to_string();
                true
            }
            "banner_timeout_ms" => {
                let Ok(ms) = trimmed.trim_matches('"').parse::<u32>() else {
                    return false;
                };
                self.banner_timeout_ms = ms.clamp(1500, 15000);
                true
            }
            "muted_notif_apps" => {
                let Ok(list) = serde_json::from_str::<Vec<String>>(trimmed) else {
                    return false;
                };
                self.muted_notif_apps = list;
                true
            }
            "seen_notif_apps" => {
                let Ok(list) = serde_json::from_str::<Vec<String>>(trimmed) else {
                    return false;
                };
                self.seen_notif_apps = list;
                true
            }
            _ => false,
        }
    }

    pub fn is_app_muted(&self, app_name: &str) -> bool {
        let target = app_name.trim();
        !target.is_empty()
            && self
                .muted_notif_apps
                .iter()
                .any(|m| m.eq_ignore_ascii_case(target) || m == target)
    }

    pub fn record_seen_notif_app(&mut self, app_name: &str) -> bool {
        let clean = app_name.trim();
        if clean.is_empty() || self.seen_notif_apps.iter().any(|s| s == clean) {
            return false;
        }
        self.seen_notif_apps.push(clean.to_string());
        if self.seen_notif_apps.len() > 32 {
            self.seen_notif_apps.remove(0);
        }
        true
    }

    pub fn should_suppress_banner(
        &self,
        now_mins: u16,
        focused_fullscreen: bool,
        app_name: &str,
    ) -> bool {
        if self.dnd || self.is_app_muted(app_name) {
            return true;
        }
        if self.dnd_fullscreen && focused_fullscreen {
            return true;
        }
        if self.dnd_schedule_enabled && is_hm_in_window(now_mins, &self.dnd_start, &self.dnd_end) {
            return true;
        }
        false
    }

    pub fn load() -> Self {
        let mut loaded: Self = fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        loaded.win_width_presets = loaded.normalized_width_presets();
        *LIVE_PREFS_SNAP
            .get_or_init(|| Mutex::new(loaded.clone()))
            .lock()
            .unwrap() = loaded.clone();
        if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(&loaded) {
            *EMITTED_PREFS_JSON
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap() = Some(map);
        }
        loaded
    }

    pub fn save(&self) {
        let mut normalized = self.clone();
        normalized.win_width_presets = normalized.normalized_width_presets();

        let p = Self::path();
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(js) = serde_json::to_string_pretty(&normalized) {
            let _ = fs::write(p, js);
        }

        *LIVE_PREFS_SNAP
            .get_or_init(|| Mutex::new(normalized.clone()))
            .lock()
            .unwrap() = normalized.clone();

        if let Ok(serde_json::Value::Object(new_map)) = serde_json::to_value(&normalized) {
            let mut prev_guard = EMITTED_PREFS_JSON
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap();
            let mut diffs: Vec<(String, String)> = Vec::new();
            if let Some(ref prev_map) = *prev_guard {
                for (k, v) in &new_map {
                    if prev_map.get(k) != Some(v) {
                        diffs.push((k.clone(), v.to_string()));
                    }
                }
            } else {
                for (k, v) in &new_map {
                    diffs.push((k.clone(), v.to_string()));
                }
            }
            *prev_guard = Some(new_map);
            drop(prev_guard);

            for (key, value_json) in diffs {
                let _ = send_async_bus_cmd(AsyncBusCmd::EmitPrefsChanged { key, value_json });
            }
        }
    }
}

/// Device category priority for Bluetooth peripheral tie-breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BtDevKind {
    Stylus = 0,
    Keyboard = 1,
    Mouse = 2,
    Audio = 3,
    Gamepad = 4,
    Other = 5,
}

impl BtDevKind {
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Stylus => "stylus",
            Self::Keyboard => "keyboard",
            Self::Mouse => "mouse",
            Self::Audio => "headphones",
            Self::Gamepad => "gamepad",
            Self::Other => "bluetooth",
        }
    }

    pub fn from_bluez_hints(icon_hint: &str, name: &str) -> Self {
        let ic = icon_hint.to_ascii_lowercase();
        let nm = name.to_ascii_lowercase();
        if nm.contains("pen") || nm.contains("stylus") || nm.contains("pencil") || ic.contains("tablet") {
            Self::Stylus
        } else if ic.contains("keyboard") || nm.contains("keyboard") || nm.contains("keychron") || nm.contains("hhkb") {
            Self::Keyboard
        } else if ic.contains("mouse") || nm.contains("mouse") || nm.contains("mx master") || nm.contains("trackpad") {
            Self::Mouse
        } else if ic.contains("audio") || ic.contains("headset") || ic.contains("headphones") || nm.contains("buds") || nm.contains("xm") || nm.contains("airpods") {
            Self::Audio
        } else if ic.contains("gaming") || ic.contains("joypad") || nm.contains("controller") || nm.contains("xbox") || nm.contains("dualsense") {
            Self::Gamepad
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtPeripheral {
    pub name: String,
    pub mac: String,
    pub kind: BtDevKind,
    pub battery_pct: Option<u8>,
    pub official_pen: bool,
    pub paired: bool,
    pub connected: bool,
    pub charging: bool,
}

impl BtPeripheral {
    pub fn short_label(&self) -> String {
        if self.official_pen {
            return "Tab Pen".into();
        }
        let trimmed = self.name.trim();
        if trimmed.chars().count() <= 12 {
            trimmed.to_string()
        } else {
            let s: String = trimmed.chars().take(11).collect();
            format!("{s}…")
        }
    }
}

#[derive(Clone)]
pub struct DesktopApp {
    pub id: String,
    pub name: String,
    pub exec_argv: Vec<String>,
    pub icon_name: String,
    pub categories: Vec<String>,
    pub icon: Option<BitmapIcon>,
}

impl DesktopApp {
    pub fn matches_category(&self, cat: AppCategory, favs: &[String]) -> bool {
        match cat {
            AppCategory::All => true,
            AppCategory::Fav => favs.iter().any(|f| {
                self.id.eq_ignore_ascii_case(f)
                    || self.id.to_ascii_lowercase().contains(&f.to_ascii_lowercase())
            }),
            AppCategory::Dev => {
                self.categories.iter().any(|c| {
                    matches!(
                        c.as_str(),
                        "Development" | "IDE" | "TerminalEmulator" | "Engineering"
                    )
                }) || self.id.contains("foot")
                    || self.id.contains("code")
                    || self.id.contains("term")
            }
            AppCategory::Office => self.categories.iter().any(|c| {
                matches!(
                    c.as_str(),
                    "Office" | "Network" | "WebBrowser" | "TextEditor" | "Education"
                )
            }),
            AppCategory::Media => self.categories.iter().any(|c| {
                matches!(
                    c.as_str(),
                    "AudioVideo" | "Audio" | "Video" | "Graphics" | "Photography" | "Game"
                )
            }),
            AppCategory::System => self.categories.iter().any(|c| {
                matches!(
                    c.as_str(),
                    "System" | "Settings" | "Utility" | "FileTools" | "FileManager"
                )
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u8,
    pub active: bool,
    pub secured: bool,
    pub saved: bool,
}

pub const BYPASS_CHARGE_LIMIT_PCT: u8 = 90;

#[derive(Debug, Clone, PartialEq)]
pub struct SysSnapshot {
    pub time_hm: String,
    pub date_short: String,
    pub date_long: String,

    pub bat_pct: u8,
    pub bat_charging: bool,
    pub usb_online: bool,
    pub bat_watts: f32,
    pub bat_temp_c: f32,

    pub pen_docked: bool,
    pub pen_charging: bool,
    pub pen_pct: i32,
    pub pen_hover: bool,
    pub pen_action_seq: u64,
    pub pen_action_code: u16,

    pub brightness_pct: u8,
    pub autobright: bool,
    pub lux: f32,
    pub torch: bool,
    pub charge_limit: u8,

    pub volume_pct: u8,
    pub muted: bool,
    pub mic_muted: bool,

    pub wifi_on: bool,
    pub wifi_ssid: String,
    pub wifi_signal: u8,
    pub wifi_list: Vec<WifiNetwork>,

    pub bt_on: bool,
    pub bt_device: String,
    pub bt_peripherals: Vec<BtPeripheral>,

    pub power_profile: String, // "power-saver" | "balanced" | "performance"
    pub media_playing: bool,
    pub media_title: String,
    pub media_artist: String,
}

impl Default for SysSnapshot {
    fn default() -> Self {
        Self {
            time_hm: "12:00".into(),
            date_short: "11月13日 周五".into(),
            date_long: "11月13日 星期五".into(),
            bat_pct: 86,
            bat_charging: false,
            usb_online: false,
            bat_watts: 4.2,
            bat_temp_c: 31.0,
            pen_docked: false,
            pen_charging: false,
            pen_pct: -1,
            pen_hover: false,
            pen_action_seq: 0,
            pen_action_code: 0,
            brightness_pct: 65,
            autobright: true,
            lux: 180.0,
            torch: false,
            charge_limit: 100,
            volume_pct: 55,
            muted: false,
            mic_muted: false,
            wifi_on: true,
            wifi_ssid: "Wi-Fi".into(),
            wifi_signal: 80,
            wifi_list: Vec::new(),
            bt_on: true,
            bt_device: String::new(),
            bt_peripherals: Vec::new(),
            power_profile: "balanced".into(),
            media_playing: false,
            media_title: String::new(),
            media_artist: String::new(),
        }
    }
}

impl SysSnapshot {
    /// Compare only fields that actually affect `StatusBar`, `Curtain`, or `Lockscreen` visuals,
    /// ignoring invisible sensor drift (`lux`, `bat_temp_c`, `pen_hover`, `pen_action_*`).
    pub fn ui_visible_eq(&self, other: &Self) -> bool {
        self.time_hm == other.time_hm
            && self.date_short == other.date_short
            && self.date_long == other.date_long
            && self.bat_pct == other.bat_pct
            && self.bat_charging == other.bat_charging
            && self.usb_online == other.usb_online
            && (self.bat_watts * 10.0).round() as i32 == (other.bat_watts * 10.0).round() as i32
            && self.pen_docked == other.pen_docked
            && self.pen_charging == other.pen_charging
            && self.pen_pct == other.pen_pct
            && self.brightness_pct == other.brightness_pct
            && self.autobright == other.autobright
            && self.torch == other.torch
            && self.charge_limit == other.charge_limit
            && self.volume_pct == other.volume_pct
            && self.muted == other.muted
            && self.mic_muted == other.mic_muted
            && self.wifi_on == other.wifi_on
            && self.wifi_ssid == other.wifi_ssid
            && self.wifi_signal == other.wifi_signal
            && self.wifi_list == other.wifi_list
            && self.bt_on == other.bt_on
            && self.bt_device == other.bt_device
            && self.bt_peripherals == other.bt_peripherals
            && self.power_profile == other.power_profile
            && self.media_playing == other.media_playing
            && self.media_title == other.media_title
            && self.media_artist == other.media_artist
    }

    /// Returns connected peripherals ordered strictly per user spec:
    /// 1. Official Stylus (`Lenovo Tab Pen Plus`, Qi docked or BLE connected) ALWAYS #1.
    /// 2. Other connected Bluetooth peripherals sorted by ascending remaining battery `%`
    ///    (`None` after known percentages), tie-broken by `BtDevKind` then `name`.
    pub fn sorted_topbar_peripherals(&self) -> Vec<BtPeripheral> {
        let mut out: Vec<BtPeripheral> = Vec::new();

        // 1. Official Lenovo Tab Pen Pro/Plus:
        //    - While physically docked (`pen_docked == true`), use Qi wireless cradle telemetry
        //      (`pen_pct`, real-time over coil and unaffected by BLE sleep/flapping on dock).
        //    - Once removed (`pen_docked == false`), NEVER overwrite BLE with Qi's stale last-docked
        //      firmware value; use BlueZ `org.bluez.Battery1` real-time reading (`ble_pen.battery_pct`).
        if let Some(ble_pen) = self
            .bt_peripherals
            .iter()
            .find(|d| d.official_pen && d.connected)
        {
            let mut p = ble_pen.clone();
            if self.pen_docked && self.pen_pct >= 0 {
                p.battery_pct = Some(self.pen_pct.clamp(0, 100) as u8);
            }
            p.charging = self.pen_docked && self.pen_charging;
            out.push(p);
        } else if self.pen_docked {
            let paired_pen_name = self
                .bt_peripherals
                .iter()
                .find(|d| d.official_pen)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "Lenovo Tab Pen Pro".into());
            out.push(BtPeripheral {
                name: paired_pen_name,
                mac: "QI:PEN".into(),
                kind: BtDevKind::Stylus,
                battery_pct: (self.pen_pct >= 0).then_some(self.pen_pct.clamp(0, 100) as u8),
                official_pen: true,
                paired: true,
                connected: true,
                charging: self.pen_charging,
            });
        }

        // 2. Other connected Bluetooth peripherals sorted by ascending battery %, then kind, then name
        if self.bt_on {
            let mut others: Vec<BtPeripheral> = self
                .bt_peripherals
                .iter()
                .filter(|d| d.connected && !d.official_pen)
                .cloned()
                .collect();
            others.sort_by(|a, b| {
                let pa = a.battery_pct.unwrap_or(255);
                let pb = b.battery_pct.unwrap_or(255);
                pa.cmp(&pb)
                    .then_with(|| a.kind.cmp(&b.kind))
                    .then_with(|| a.name.cmp(&b.name))
            });
            out.extend(others);
        }

        out
    }
}

pub fn format_local_clock() -> (String, String, String) {
    let mut now: libc::time_t = 0;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::time(&mut now);
        libc::localtime_r(&now, &mut tm);
    }
    let wday_short = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"]
        .get(tm.tm_wday.clamp(0, 6) as usize)
        .copied()
        .unwrap_or("周一");
    let wday_long = [
        "星期日", "星期一", "星期二", "星期三", "星期四", "星期五", "星期六",
    ]
    .get(tm.tm_wday.clamp(0, 6) as usize)
    .copied()
    .unwrap_or("星期一");
    let hm = format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
    let ds = format!("{}月{}日 {}", tm.tm_mon + 1, tm.tm_mday, wday_short);
    let dl = format!("{}月{}日 {}", tm.tm_mon + 1, tm.tm_mday, wday_long);
    (hm, ds, dl)
}

fn read_i64_file(p: &str) -> Option<i64> {
    fs::read_to_string(p).ok()?.trim().parse().ok()
}

static DAEMON_CONN: OnceLock<Mutex<Option<(UnixStream, BufReader<UnixStream>)>>> = OnceLock::new();

fn try_daemon_roundtrip(
    conn: &mut (UnixStream, BufReader<UnixStream>),
    line: &str,
) -> Option<serde_json::Value> {
    conn.0.write_all(line.as_bytes()).ok()?;
    conn.0.flush().ok()?;
    let mut out = String::new();
    let n = conn.1.read_line(&mut out).ok()?;
    if n == 0 {
        return None;
    }
    serde_json::from_str(&out).ok()
}

fn open_daemon_conn() -> Option<(UnixStream, BufReader<UnixStream>)> {
    let s = UnixStream::connect(DAEMON_SOCK).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(350)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(350)));
    let r = BufReader::new(s.try_clone().ok()?);
    Some((s, r))
}

fn call_daemon_json(payload: &str) -> Option<serde_json::Value> {
    let mut line = payload.trim().to_string();
    line.push('\n');
    let slot = DAEMON_CONN.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().ok()?;
    if let Some(ref mut conn) = *guard {
        if let Some(v) = try_daemon_roundtrip(conn, &line) {
            return Some(v);
        }
    }
    *guard = open_daemon_conn();
    let conn = guard.as_mut()?;
    let res = try_daemon_roundtrip(conn, &line);
    if res.is_none() {
        *guard = None;
    }
    res
}

/// Write `/run/user/$UID/tb520fu-display-ctl` for Niri's DRM CRTC CTM hook (Night Light + Grayscale Reading Mode)
/// and sync `DSI-1` refresh rate (`144Hz` normal vs `120Hz` power-save or `240Hz`-aligned Stylus hover!).
pub fn sync_display_ctm_and_hz(prefs: &ShellPrefs, pen_hover: bool) {
    let uid = unsafe { libc::getuid() };
    let ctl_path = format!("/run/user/{uid}/tb520fu-display-ctl");
    let tmp_path = format!("{ctl_path}.tmp");
    let content = format!(
        "night_light={}\nreading_mode={}\n",
        if prefs.night_light { 1 } else { 0 },
        if prefs.reading_mode { 1 } else { 0 },
    );
    if fs::write(&tmp_path, content).is_ok() {
        let _ = fs::rename(&tmp_path, &ctl_path);
    }

    let target_hz = if !prefs.high_refresh_144 {
        60.0
    } else if pen_hover {
        120.0
    } else {
        144.0
    };
    std::thread::spawn(move || {
        set_dsi_refresh_hz(target_hz);
    });
}

static PENDING_VOL_PCT: std::sync::atomic::AtomicI16 = std::sync::atomic::AtomicI16::new(-1);
static PENDING_BRI_PCT: std::sync::atomic::AtomicI16 = std::sync::atomic::AtomicI16::new(-1);
static LAST_VOL_DRAG_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAST_BRI_DRAG_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAST_GPU_BOOST_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CMD_DAEMON_CONN: OnceLock<Mutex<Option<(UnixStream, BufReader<UnixStream>)>>> =
    OnceLock::new();
static BOOST_DAEMON_CONN: OnceLock<Mutex<Option<UnixStream>>> = OnceLock::new();

fn mono_millis() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

pub fn is_vol_drag_recent() -> bool {
    PENDING_VOL_PCT.load(std::sync::atomic::Ordering::Relaxed) >= 0
        || mono_millis().saturating_sub(LAST_VOL_DRAG_MS.load(std::sync::atomic::Ordering::Relaxed))
            < 450
}

pub fn is_bri_drag_recent() -> bool {
    PENDING_BRI_PCT.load(std::sync::atomic::Ordering::Relaxed) >= 0
        || mono_millis().saturating_sub(LAST_BRI_DRAG_MS.load(std::sync::atomic::Ordering::Relaxed))
            < 450
}

fn call_cmd_daemon_json(payload: &str) -> Option<serde_json::Value> {
    let mut line = payload.trim().to_string();
    line.push('\n');
    let slot = CMD_DAEMON_CONN.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().ok()?;
    if let Some(ref mut conn) = *guard {
        if let Some(v) = try_daemon_roundtrip(conn, &line) {
            return Some(v);
        }
    }
    *guard = open_daemon_conn();
    let conn = guard.as_mut()?;
    let res = try_daemon_roundtrip(conn, &line);
    if res.is_none() {
        *guard = None;
    }
    res
}

fn send_daemon_boost_now(duration_ms: u16) {
    let line = format!("{{\"cmd\":\"gpu_boost\",\"duration_ms\":{duration_ms}}}\n");
    let slot = BOOST_DAEMON_CONN.get_or_init(|| Mutex::new(None));
    let Ok(mut guard) = slot.lock() else {
        return;
    };
    if let Some(ref mut s) = *guard {
        if s.write_all(line.as_bytes()).is_ok() {
            return;
        }
    }
    if let Ok(mut s) = UnixStream::connect(DAEMON_SOCK) {
        let _ = s.set_write_timeout(Some(Duration::from_millis(25)));
        if s.write_all(line.as_bytes()).is_ok() {
            *guard = Some(s);
            return;
        }
    }
    *guard = None;
}

fn apply_brightness_blocking(pct: u8) {
    let p = pct.clamp(1, 100);
    if call_cmd_daemon_json(&format!("{{\"cmd\":\"set_brightness\",\"percent\":{p}}}")).is_some() {
        return;
    }
    let max_b =
        read_i64_file("/sys/class/backlight/ktz8866-backlight/max_brightness").unwrap_or(2047);
    let raw = (((p as i64) * max_b + 50) / 100).clamp(1, max_b);
    let _ = Command::new("busctl")
        .args([
            "call",
            "org.freedesktop.login1",
            "/org/freedesktop/login1/session/auto",
            "org.freedesktop.login1.Session",
            "SetBrightness",
            "ssu",
            "backlight",
            "ktz8866-backlight",
            &raw.to_string(),
        ])
        .output();
}

pub fn set_brightness_pct(pct: u8) {
    let p = pct.clamp(1, 100);
    LAST_BRI_DRAG_MS.store(mono_millis(), std::sync::atomic::Ordering::Relaxed);
    let prev = PENDING_BRI_PCT.swap(p as i16, std::sync::atomic::Ordering::AcqRel);
    if ASYNC_BUS_TX.get().is_some() {
        if prev < 0 {
            let _ = send_async_bus_cmd(AsyncBusCmd::FlushSliders);
        }
    } else {
        let _ = PENDING_BRI_PCT.swap(-1, std::sync::atomic::Ordering::AcqRel);
        std::thread::spawn(move || apply_brightness_blocking(p));
    }
}

pub fn set_autobright(on: bool) {
    LAST_BRI_DRAG_MS.store(mono_millis(), std::sync::atomic::Ordering::Relaxed);
    if !send_async_bus_cmd(AsyncBusCmd::SetAutobright(on)) {
        std::thread::spawn(move || {
            let _ = call_cmd_daemon_json(&format!("{{\"cmd\":\"set_autobright\",\"enabled\":{on}}}"));
        });
    }
}

pub fn toggle_torch() {
    if !send_async_bus_cmd(AsyncBusCmd::ToggleTorch) {
        std::thread::spawn(|| {
            let _ = call_cmd_daemon_json("{\"cmd\":\"toggle_torch\"}");
        });
    }
}

pub fn set_charge_limit(limit: u8) {
    if !send_async_bus_cmd(AsyncBusCmd::SetChargeLimit(limit)) {
        std::thread::spawn(move || {
            let _ = call_cmd_daemon_json(&format!("{{\"cmd\":\"set_charge_limit\",\"limit\":{limit}}}"));
        });
    }
}

pub fn set_volume_pct(pct: u8) {
    let p = pct.min(100);
    LAST_VOL_DRAG_MS.store(mono_millis(), std::sync::atomic::Ordering::Relaxed);
    let prev = PENDING_VOL_PCT.swap(p as i16, std::sync::atomic::Ordering::AcqRel);
    if ASYNC_BUS_TX.get().is_some() {
        if prev < 0 {
            let _ = send_async_bus_cmd(AsyncBusCmd::FlushSliders);
        }
    } else {
        let _ = PENDING_VOL_PCT.swap(-1, std::sync::atomic::Ordering::AcqRel);
        std::thread::spawn(move || {
            let _ = Command::new("wpctl")
                .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{p}%")])
                .status();
        });
    }
}

pub fn toggle_mute() {
    LAST_VOL_DRAG_MS.store(mono_millis(), std::sync::atomic::Ordering::Relaxed);
    if !send_async_bus_cmd(AsyncBusCmd::ToggleMute) {
        std::thread::spawn(|| {
            let _ = Command::new("wpctl")
                .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
                .status();
        });
    }
}

pub fn toggle_mic_mute() {
    LAST_VOL_DRAG_MS.store(mono_millis(), std::sync::atomic::Ordering::Relaxed);
    if !send_async_bus_cmd(AsyncBusCmd::ToggleMicMute) {
        std::thread::spawn(|| {
            let _ = Command::new("wpctl")
                .args(["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"])
                .status();
        });
    }
}

pub fn shell_ctrl_sock_path() -> String {
    if let Ok(rdir) = std::env::var("XDG_RUNTIME_DIR") {
        let trimmed = rdir.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return format!("{trimmed}/tb520fu-shell.sock");
        }
    }
    let uid = unsafe { libc::getuid() };
    format!("/run/user/{uid}/tb520fu-shell.sock")
}

pub fn send_shell_ctrl_sock(cmd_line: &str) -> bool {
    let path = shell_ctrl_sock_path();
    let Ok(mut stream) = UnixStream::connect(&path) else {
        return false;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(40)));
    let mut msg = cmd_line.trim().to_string();
    msg.push('\n');
    stream.write_all(msg.as_bytes()).is_ok()
}

pub(crate) fn parse_shell_ctrl_line(line: &str) -> Option<ShellBusEvent> {
    let mut parts = line.split_whitespace();
    let verb = parts.next()?;
    match verb {
        "vol" => {
            let delta = parts.next()?.parse::<i8>().ok()?;
            Some(ShellBusEvent::HardwareKeyOsd(HwOsdCmd::VolumeDelta(delta)))
        }
        "mute" => Some(ShellBusEvent::HardwareKeyOsd(HwOsdCmd::ToggleMute)),
        "mic-mute" => Some(ShellBusEvent::HardwareKeyOsd(HwOsdCmd::ToggleMicMute)),
        "bri" => {
            let delta = parts.next()?.parse::<i8>().ok()?;
            Some(ShellBusEvent::HardwareKeyOsd(HwOsdCmd::BrightnessDelta(
                delta,
            )))
        }
        "lock" => Some(ShellBusEvent::LockRequested),
        "unlock" => Some(ShellBusEvent::UnlockRequested),
        "standby" => Some(ShellBusEvent::StartStandby),
        "console" | "control-center" => Some(ShellBusEvent::ToggleControlCenter),
        "notifications" => Some(ShellBusEvent::ToggleNotifications),
        "annot" => Some(ShellBusEvent::StartAnnot),
        "quicknote" => Some(ShellBusEvent::ToggleQuickNote),
        "win-menu" => {
            let win_id = parts.next()?.parse::<u64>().ok()?;
            let cx = parts.next()?.parse::<f32>().ok()?;
            let ty = parts.next()?.parse::<f32>().ok()?;
            let is_floating = parts
                .next()
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            Some(ShellBusEvent::OpenWindowMenu {
                win_id,
                cx,
                ty,
                is_floating,
            })
        }
        "curtain-swipe-begin" => Some(ShellBusEvent::CurtainSwipeBegin),
        "curtain-swipe" => {
            let dy = parts.next()?.parse::<f32>().ok()?;
            Some(ShellBusEvent::CurtainSwipeUpdate(dy))
        }
        "curtain-swipe-end" => Some(ShellBusEvent::CurtainSwipeEnd),
        "width-presets" => {
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                return None;
            }
            let val = rest.join(",");
            parse_width_presets_input(&val)?;
            Some(ShellBusEvent::SetPref {
                key: "win_width_presets".into(),
                value_json: val,
            })
        }
        "set-pref" => {
            let key = parts.next()?.to_string();
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                return None;
            }
            Some(ShellBusEvent::SetPref {
                key,
                value_json: rest.join(" "),
            })
        }
        _ => None,
    }
}

/// ZUI 18.5-native Pogo touchpad physical top-edge 2-finger pull-down watcher:
/// Opens `/dev/input/by-path/platform-888000.serial-event-mouse` read-only (no `EVIOCGRAB`,
/// hot-plug resilient across Pogo dock/undock) and emits 1:1 `TouchpadEdgePull` events
/// whenever 2 fingers touch down in the top strip (`ABS_MT_POSITION_Y <= 295`, top ~11.5mm)
/// and pull downward — regardless of where the cursor currently sits on screen!
fn spawn_pogo_touchpad_edge_listener(
    bus_tx: mpsc::Sender<ShellBusEvent>,
    wakeup_fd: Option<i32>,
) {
    let _ = std::thread::Builder::new()
        .name("tb520fu-pogo-edge".into())
        .spawn(move || {
            use crate::statusbar::PullWing;
            use std::os::unix::io::AsRawFd;

            const DEV_PATH: &str = "/dev/input/by-path/platform-888000.serial-event-mouse";
            const EV_SYN: u16 = 0x00;
            const SYN_REPORT: u16 = 0x00;
            const EV_KEY: u16 = 0x01;
            const BTN_LEFT: u16 = 0x110;
            const EV_ABS: u16 = 0x03;
            const ABS_MT_SLOT: u16 = 0x2f;
            const ABS_MT_POSITION_X: u16 = 0x35;
            const ABS_MT_POSITION_Y: u16 = 0x36;
            const ABS_MT_TRACKING_ID: u16 = 0x39;
            // `0..3211` X (`1605` midpoint), `0..1879` Y (`<= 295` = top ~11.8mm edge below spacebar).
            const TOP_EDGE_MAX_Y: i32 = 295;
            const X_MIDPOINT: i32 = 1605;

            #[derive(Clone, Copy, Default)]
            struct MtSlot {
                active: bool,
                has_start_x: bool,
                has_start_y: bool,
                x: i32,
                y: i32,
                start_x: i32,
                start_y: i32,
            }

            #[repr(C)]
            #[derive(Default)]
            struct InputAbsInfo {
                value: i32,
                minimum: i32,
                maximum: i32,
                fuzz: i32,
                flat: i32,
                resolution: i32,
            }

            loop {
                let Ok(file) = fs::File::open(DEV_PATH) else {
                    std::thread::sleep(Duration::from_millis(2500));
                    continue;
                };
                let fd = file.as_raw_fd();

                // Query current kernel MT slot so opening mid-touch never misattributes slot 0.
                let mut cur_slot: usize = 0;
                let mut abs_info = InputAbsInfo::default();
                let eviocgabs_slot: libc::c_ulong = ((2usize << 30)
                    | (std::mem::size_of::<InputAbsInfo>() << 16)
                    | ((b'E' as usize) << 8)
                    | (0x40usize + ABS_MT_SLOT as usize))
                    as libc::c_ulong;
                if unsafe { libc::ioctl(fd, eviocgabs_slot, &mut abs_info) } >= 0 {
                    cur_slot = (abs_info.value.clamp(0, 3)) as usize;
                }

                let mut slots = [MtSlot::default(); 4];
                let mut disarmed = false;
                let mut engaged = false;
                let mut swipe3_fired = false;
                let mut btn_down = false;
                let mut wing = PullWing::Right;
                let mut last_frac: f32 = 0.0;
                let mut last_vel: f32 = 0.0;
                let mut last_t = Instant::now();
                let mut buf = [0u8; std::mem::size_of::<libc::input_event>() * 16];

                loop {
                    let n = unsafe {
                        libc::read(
                            fd,
                            buf.as_mut_ptr() as *mut libc::c_void,
                            buf.len(),
                        )
                    };
                    if n <= 0 {
                        if n < 0 {
                            let err = std::io::Error::last_os_error();
                            if err.kind() == std::io::ErrorKind::Interrupted {
                                continue;
                            }
                        }
                        break;
                    }
                    let ev_size = std::mem::size_of::<libc::input_event>();
                    let count = (n as usize) / ev_size;
                    for i in 0..count {
                        let ev: libc::input_event = unsafe {
                            std::ptr::read_unaligned(
                                buf.as_ptr().add(i * ev_size) as *const libc::input_event
                            )
                        };
                        match (ev.type_, ev.code) {
                            (EV_ABS, ABS_MT_SLOT) => {
                                cur_slot = (ev.value.clamp(0, 3)) as usize;
                            }
                            (EV_ABS, ABS_MT_TRACKING_ID) => {
                                if ev.value >= 0 {
                                    slots[cur_slot] = MtSlot {
                                        active: true,
                                        ..MtSlot::default()
                                    };
                                } else {
                                    slots[cur_slot].active = false;
                                }
                            }
                            (EV_ABS, ABS_MT_POSITION_X) => {
                                let s = &mut slots[cur_slot];
                                s.x = ev.value;
                                if s.active && !s.has_start_x {
                                    s.start_x = ev.value;
                                    s.has_start_x = true;
                                }
                            }
                            (EV_ABS, ABS_MT_POSITION_Y) => {
                                let s = &mut slots[cur_slot];
                                s.y = ev.value;
                                if s.active && !s.has_start_y {
                                    s.start_y = ev.value;
                                    s.has_start_y = true;
                                }
                            }
                            (EV_KEY, BTN_LEFT) => {
                                if ev.value == 1 {
                                    btn_down = true;
                                } else if ev.value == 0 && btn_down {
                                    btn_down = false;
                                    let _ = bus_tx.send(ShellBusEvent::TouchpadLockWake);
                                    wake_fd(wakeup_fd);
                                }
                            }
                            (EV_SYN, SYN_REPORT) => {
                                let mut active_idxs = [0usize; 4];
                                let mut active_n = 0usize;
                                for (si, s) in slots.iter().enumerate() {
                                    if s.active && s.has_start_x && s.has_start_y {
                                        active_idxs[active_n] = si;
                                        active_n += 1;
                                    }
                                }
                                let raw_active = slots.iter().filter(|s| s.active).count();

                                if raw_active == 0 {
                                    if engaged {
                                        let _ = bus_tx.send(ShellBusEvent::TouchpadEdgePull {
                                            wing,
                                            frac: last_frac,
                                            velocity: last_vel,
                                            finished: true,
                                        });
                                        wake_fd(wakeup_fd);
                                    }
                                    engaged = false;
                                    disarmed = false;
                                    swipe3_fired = false;
                                    last_frac = 0.0;
                                    last_vel = 0.0;
                                    continue;
                                }

                                if raw_active >= 3 {
                                    if engaged {
                                        let _ = bus_tx.send(ShellBusEvent::TouchpadEdgePull {
                                            wing,
                                            frac: last_frac,
                                            velocity: last_vel,
                                            finished: true,
                                        });
                                        wake_fd(wakeup_fd);
                                        engaged = false;
                                    }
                                    if !swipe3_fired && active_n >= 3 {
                                        let mut dy_sum = 0i32;
                                        for &idx in &active_idxs[..active_n] {
                                            dy_sum += slots[idx].y - slots[idx].start_y;
                                        }
                                        let avg_dy = (dy_sum as f32) / (active_n as f32);
                                        if avg_dy <= -110.0 {
                                            swipe3_fired = true;
                                            let _ = bus_tx.send(ShellBusEvent::TouchpadLockWake);
                                            wake_fd(wakeup_fd);
                                        }
                                    }
                                    disarmed = true;
                                    continue;
                                }

                                if engaged && active_n < 2 {
                                    let _ = bus_tx.send(ShellBusEvent::TouchpadEdgePull {
                                        wing,
                                        frac: last_frac,
                                        velocity: last_vel,
                                        finished: true,
                                    });
                                    wake_fd(wakeup_fd);
                                    engaged = false;
                                    disarmed = true;
                                    continue;
                                }

                                if !disarmed && active_n == 2 {
                                    let a = slots[active_idxs[0]];
                                    let b = slots[active_idxs[1]];
                                    let both_top =
                                        a.start_y <= TOP_EDGE_MAX_Y && b.start_y <= TOP_EDGE_MAX_Y;
                                    if !both_top {
                                        disarmed = true;
                                        continue;
                                    }
                                    let avg_dy =
                                        ((a.y - a.start_y) + (b.y - b.start_y)) as f32 * 0.5;
                                    let avg_dx =
                                        ((a.x - a.start_x) + (b.x - b.start_x)) as f32 * 0.5;
                                    if !engaged {
                                        if avg_dx.abs() > 140.0 && avg_dx.abs() > avg_dy.abs() * 1.15
                                        {
                                            disarmed = true;
                                            continue;
                                        }
                                        if avg_dy >= 95.0 && avg_dy >= avg_dx.abs() {
                                            engaged = true;
                                            let avg_x0 = (a.start_x + b.start_x) / 2;
                                            wing = if avg_x0 < X_MIDPOINT {
                                                PullWing::Left
                                            } else {
                                                PullWing::Right
                                            };
                                            last_frac = ((avg_dy - 65.0) / 1120.0).clamp(0.02, 1.0);
                                            last_vel = 420.0;
                                            last_t = Instant::now();
                                            let _ = bus_tx.send(ShellBusEvent::TouchpadEdgePull {
                                                wing,
                                                frac: last_frac,
                                                velocity: last_vel,
                                                finished: false,
                                            });
                                            wake_fd(wakeup_fd);
                                        }
                                    } else {
                                        let now = Instant::now();
                                        let dt = now
                                            .duration_since(last_t)
                                            .as_secs_f32()
                                            .clamp(0.001, 0.08);
                                        last_t = now;
                                        let frac = ((avg_dy - 65.0) / 1120.0).clamp(0.015, 1.0);
                                        let inst_vel = ((frac - last_frac) * 920.0) / dt;
                                        last_vel = last_vel * 0.45 + inst_vel * 0.55;
                                        last_frac = frac;
                                        let _ = bus_tx.send(ShellBusEvent::TouchpadEdgePull {
                                            wing,
                                            frac,
                                            velocity: last_vel,
                                            finished: false,
                                        });
                                        wake_fd(wakeup_fd);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }

                std::thread::sleep(Duration::from_millis(2000));
            }
        });
}

pub fn unlock_logind_session() {
    if !send_async_bus_cmd(AsyncBusCmd::UnlockLogindSession) {
        std::thread::spawn(|| {
            let _ = Command::new("loginctl").arg("unlock-session").status();
        });
    }
}

/// Request a transient `422 MHz` Adreno 750 floor boost during interactive shell/overview animations
/// (rate-limited on the UI thread to `<= 1` async send per `240 ms`; `0.0 ms` UI wait).
pub fn request_gpu_boost(duration_ms: u16) {
    let now = mono_millis();
    let last = LAST_GPU_BOOST_MS.load(std::sync::atomic::Ordering::Relaxed);
    if now.saturating_sub(last) < 240 {
        return;
    }
    LAST_GPU_BOOST_MS.store(now, std::sync::atomic::Ordering::Relaxed);
    let _ = send_async_bus_cmd(AsyncBusCmd::GpuBoost(duration_ms));
}

fn sync_pogo_keyboard_leds(sink_muted: bool, source_muted: bool) {
    let Ok(entries) = fs::read_dir("/sys/class/leds") else {
        return;
    };
    for e in entries.flatten() {
        let dir_name = e.file_name();
        let name = dir_name.to_string_lossy();
        let target = if name.ends_with("::mute") {
            Some(sink_muted)
        } else if name.ends_with("::misc") || name.ends_with("::micmute") {
            Some(source_muted)
        } else {
            None
        };
        let Some(on) = target else { continue };
        let dev_name = fs::read_to_string(e.path().join("device/name")).unwrap_or_default();
        if dev_name.trim() != "Lenovo Keyboard Pack For Yoga Tab" {
            continue;
        }
        let want = if on { "1" } else { "0" };
        let bpath = e.path().join("brightness");
        if fs::read_to_string(&bpath).map(|s| s.trim() == want).unwrap_or(false) {
            continue;
        }
        let _ = fs::write(&bpath, format!("{want}\n"));
    }
}

#[derive(Debug, Clone)]
pub enum AsyncBusCmd {
    OskMethod(&'static str),
    SetWifiEnabled(bool),
    TriggerWifiScan,
    ConnectWifi {
        ssid: String,
        password: Option<String>,
    },
    SetBtEnabled(bool),
    SetBtDiscovery(bool),
    ToggleBtDevice {
        mac: String,
        disconnect: bool,
        paired: bool,
    },
    MediaCmd(&'static str),
    FlushSliders,
    SetAutobright(bool),
    ToggleTorch,
    SetChargeLimit(u8),
    ToggleMute,
    ToggleMicMute,
    UnlockLogindSession,
    GpuBoost(u16),
    EmitPrefsChanged { key: String, value_json: String },
}

static ASYNC_BUS_TX: OnceLock<tokio::sync::mpsc::UnboundedSender<AsyncBusCmd>> = OnceLock::new();
static BT_DISCOVERING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn send_async_bus_cmd(cmd: AsyncBusCmd) -> bool {
    if let Some(tx) = ASYNC_BUS_TX.get() {
        tx.send(cmd).is_ok()
    } else {
        false
    }
}

pub fn set_wifi_enabled(on: bool) {
    if !send_async_bus_cmd(AsyncBusCmd::SetWifiEnabled(on)) {
        let arg = if on { "on" } else { "off" };
        std::thread::spawn(move || {
            let _ = Command::new("nmcli").args(["radio", "wifi", arg]).status();
        });
    }
}

pub fn trigger_wifi_scan() {
    if !send_async_bus_cmd(AsyncBusCmd::TriggerWifiScan) {
        std::thread::spawn(|| {
            if call_cmd_daemon_json("{\"cmd\":\"wifi_rescan\"}").is_none() {
                let _ = Command::new("nmcli")
                    .args(["device", "wifi", "rescan"])
                    .status();
            }
        });
    }
}

pub fn connect_wifi_ssid(ssid: String) {
    if !send_async_bus_cmd(AsyncBusCmd::ConnectWifi {
        ssid: ssid.clone(),
        password: None,
    }) {
        std::thread::spawn(move || {
            let payload = serde_json::json!({
                "cmd": "wifi_connect",
                "ssid": ssid,
            })
            .to_string();
            if call_cmd_daemon_json(&payload).is_none() {
                let _ = Command::new("nmcli")
                    .args(["device", "wifi", "connect", &ssid])
                    .status();
            }
        });
    }
}

pub fn connect_wifi_with_password(ssid: String, password: String) {
    if !send_async_bus_cmd(AsyncBusCmd::ConnectWifi {
        ssid: ssid.clone(),
        password: Some(password.clone()),
    }) {
        std::thread::spawn(move || {
            let payload = serde_json::json!({
                "cmd": "wifi_connect",
                "ssid": ssid,
                "password": password,
            })
            .to_string();
            if call_cmd_daemon_json(&payload).is_none() {
                let _ = Command::new("nmcli")
                    .args(["device", "wifi", "connect", &ssid, "password", &password])
                    .status();
            }
        });
    }
}

pub fn set_bt_enabled(on: bool) {
    if !on {
        BT_DISCOVERING.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    if !send_async_bus_cmd(AsyncBusCmd::SetBtEnabled(on)) {
        let arg = if on { "on" } else { "off" };
        std::thread::spawn(move || {
            let _ = Command::new("bluetoothctl").args(["power", arg]).status();
        });
    }
}

pub fn set_bt_discovery(active: bool) {
    let prev = BT_DISCOVERING.swap(active, std::sync::atomic::Ordering::Relaxed);
    if prev != active || active {
        let _ = send_async_bus_cmd(AsyncBusCmd::SetBtDiscovery(active));
    }
}

pub fn is_bt_discovering() -> bool {
    BT_DISCOVERING.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn toggle_bt_device_mac(mac: String, currently_connected: bool, paired: bool) {
    if mac.is_empty() || mac == "QI:PEN" {
        return;
    }
    if !send_async_bus_cmd(AsyncBusCmd::ToggleBtDevice {
        mac: mac.clone(),
        disconnect: currently_connected,
        paired,
    }) {
        std::thread::spawn(move || {
            if currently_connected {
                let _ = Command::new("bluetoothctl")
                    .args(["disconnect", &mac])
                    .status();
            } else {
                if !paired {
                    let _ = Command::new("bluetoothctl").args(["pair", &mac]).status();
                    let _ = Command::new("bluetoothctl").args(["trust", &mac]).status();
                }
                let _ = Command::new("bluetoothctl")
                    .args(["connect", &mac])
                    .status();
            }
        });
    }
}

pub fn system_poweroff() {
    std::thread::spawn(|| {
        if call_cmd_daemon_json("{\"cmd\":\"power_off\"}").is_none() {
            let _ = Command::new("systemctl").arg("poweroff").status();
        }
    });
}

pub fn system_reboot() {
    std::thread::spawn(|| {
        if call_cmd_daemon_json("{\"cmd\":\"reboot\"}").is_none() {
            let _ = Command::new("systemctl").arg("reboot").status();
        }
    });
}

pub fn system_reboot_android() {
    std::thread::spawn(|| {
        if call_cmd_daemon_json("{\"cmd\":\"reboot_android\"}").is_none() {
            let _ = Command::new("/usr/local/sbin/reboot-to-android").status();
        }
    });
}

pub fn adjust_brightness_via_daemon(delta: i8) -> bool {
    let Some(st) = call_cmd_daemon_json("{\"cmd\":\"status\"}") else {
        return false;
    };
    let cur = st
        .get("brightness_pct")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as i16;
    let next = (cur + delta as i16).clamp(1, 100) as u8;
    call_cmd_daemon_json(&format!("{{\"cmd\":\"set_brightness\",\"percent\":{next}}}")).is_some()
}

pub fn media_cmd(cmd: &'static str) {
    if !send_async_bus_cmd(AsyncBusCmd::MediaCmd(cmd)) {
        std::thread::spawn(move || {
            let _ = Command::new("playerctl").arg(cmd).status();
        });
    }
}

pub fn open_system_settings() {
    open_system_settings_page(None);
}

pub fn open_system_settings_page(page: Option<&'static str>) {
    std::thread::spawn(move || {
        if std::path::Path::new("/usr/local/bin/tb520fu-settings").exists() {
            let mut cmd = Command::new("taskset");
            cmd.args(["-c", "2-7", "/usr/local/bin/tb520fu-settings"]);
            if let Some(p) = page {
                cmd.args(["--page", p]);
            }
            if cmd.spawn().is_ok() {
                return;
            }
        }
        for bin in ["tb520fu-settings", "gnome-control-center", "nm-connection-editor", "pavucontrol", "foot"] {
            let mut cmd = Command::new(bin);
            if bin == "tb520fu-settings" {
                if let Some(p) = page {
                    cmd.args(["--page", p]);
                }
            }
            if cmd.spawn().is_ok() {
                break;
            }
        }
    });
}

/// Read kernel HID/BLE peripheral battery lookup map (`model_name/lower -> capacity`).
fn read_sysfs_hid_batteries() -> Vec<(String, u8)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/power_supply") else {
        return out;
    };
    for e in rd.flatten() {
        let dir_name = e.file_name().to_string_lossy().into_owned();
        // Exclude tablet battery/charger nodes and `lenovo-pen` (ADSP Qi register retains stale
        // last-docked percentage when undocked; BLE GATT `org.bluez.Battery1` is used instead).
        if dir_name.starts_with("qcom-battmgr")
            || dir_name.starts_with("ucsi-")
            || dir_name == "lenovo-pen"
        {
            continue;
        }
        let p = e.path();
        if fs::read_to_string(p.join("present"))
            .ok()
            .is_some_and(|s| s.trim() == "0")
        {
            continue;
        }
        let Some(cap) = fs::read_to_string(p.join("capacity"))
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
        else {
            continue;
        };
        let model = fs::read_to_string(p.join("model_name"))
            .unwrap_or(dir_name)
            .trim()
            .to_ascii_lowercase();
        if !model.is_empty() {
            out.push((model, cap.min(100)));
        }
    }
    out
}

#[inline]
fn wake_fd(wakeup_fd: Option<i32>) {
    if let Some(fd) = wakeup_fd {
        let one: u64 = 1;
        unsafe {
            let _ = libc::write(fd, &one as *const u64 as *const libc::c_void, 8);
        }
    }
}

fn update_shared_sys(
    shared: &Arc<Mutex<SysSnapshot>>,
    wakeup_fd: Option<i32>,
    mutate: impl FnOnce(&mut SysSnapshot),
) {
    let mut guard = shared.lock().unwrap();
    let before = guard.clone();
    mutate(&mut guard);
    let changed = *guard != before;
    drop(guard);
    if changed {
        wake_fd(wakeup_fd);
    }
}

/// Determines whether the battery cell is actively charging (`true`) or in bypass / idle (`false`).
///
/// On TB520FU, when Lenovo ADSP charge protection (`HQ prop 29 = 85090`, stop at 90%, recharge below 85%)
/// halts cell charging at 90%, `/sys/class/power_supply/qcom-battmgr-bat/status` continues to report
/// `"Charging"` even after `current_now` drops to `0 uA`. Checking `bat_pct >= charge_limit` and the
/// `(charge_limit - 5)..charge_limit` zero-current hysteresis band distinguishes active charging
/// (`正在充至90%`) from active bypass power (`旁路供电中`).
pub fn compute_bat_charging(
    raw_status: &str,
    usb_online: bool,
    bat_pct: u8,
    charge_limit: u8,
    current_ua: Option<i64>,
) -> bool {
    let s = raw_status.trim();
    if !usb_online {
        return false;
    }
    if charge_limit < 100 {
        if bat_pct >= charge_limit {
            return false;
        }
        let recharge_floor = charge_limit.saturating_sub(5);
        if bat_pct >= recharge_floor && current_ua.is_some_and(|ua| ua <= 50_000) {
            return false;
        }
        return s == "Charging";
    }
    s == "Charging" || s == "Full"
}

/// Pure in-process clock + sysfs battery + `/run/tb520fu-daemon.sock` Unix socket read (`0` child processes!).
fn poll_inprocess_hw_and_clock(prev: &mut SysSnapshot) {
    let (hm, ds, dl) = format_local_clock();
    prev.time_hm = hm;
    prev.date_short = ds;
    prev.date_long = dl;

    // 1. Battery (`qcom-battmgr-bat`)
    let bat_dir = "/sys/class/power_supply/qcom-battmgr-bat";
    if let Some(cap) = read_i64_file(&format!("{bat_dir}/capacity")) {
        prev.bat_pct = cap.clamp(0, 100) as u8;
    }
    let usb_on = read_i64_file("/sys/class/power_supply/qcom-battmgr-usb/online").unwrap_or(0) > 0
        || read_i64_file("/sys/class/power_supply/ucsi-source-psy-pmic_glink.ucsi.01/online")
            .unwrap_or(0)
            > 0;
    prev.usb_online = usb_on;
    let raw_bat_status = fs::read_to_string(format!("{bat_dir}/status")).unwrap_or_default();
    let current_ua = read_i64_file(&format!("{bat_dir}/current_now"));
    // qcom-battmgr exposes `power_now` but the SM8650 firmware always reports 0 there (seen
    // 2026-11-16 while discharging at -2.26 A / 4.03 V), so a zero reading is treated as missing
    // and the value is derived from `current_now × voltage_now` instead.
    let power_uw = read_i64_file(&format!("{bat_dir}/power_now")).filter(|uw| *uw != 0);
    let derived_uw = match (current_ua, read_i64_file(&format!("{bat_dir}/voltage_now"))) {
        (Some(ua), Some(uv)) => Some(((ua.abs() as f64) * (uv.abs() as f64) / 1e6) as i64),
        _ => None,
    };
    if let Some(uw) = power_uw.or(derived_uw) {
        let w = (uw.abs() as f32) / 1_000_000.0;
        prev.bat_watts = (w * 10.0).round() * 0.1;
    }
    if let Some(t10) = read_i64_file(&format!("{bat_dir}/temp")) {
        let tc = (t10 as f32) / 10.0;
        prev.bat_temp_c = (tc * 2.0).round() * 0.5;
    }

    // 2. Hardware daemon (`autobright`, `brightness`, `torch`, `charge_limit`, `pen`, `pen_hover`, `pen_action`)
    let got_daemon = if let Some(v) = call_daemon_json("{\"cmd\":\"status\"}") {
        if !is_bri_drag_recent() {
            prev.autobright = v
                .get("autobright")
                .and_then(|x| x.as_bool())
                .unwrap_or(prev.autobright);
            prev.brightness_pct = v
                .get("brightness_pct")
                .and_then(|x| x.as_u64())
                .unwrap_or(prev.brightness_pct as u64) as u8;
        }
        prev.lux = (v.get("lux").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32).round();
        prev.torch = v.get("torch").and_then(|x| x.as_bool()).unwrap_or(false);
        prev.charge_limit = v
            .get("charge_limit")
            .and_then(|x| x.as_u64())
            .unwrap_or(100) as u8;
        prev.pen_docked = v.get("pen_docked").and_then(|x| x.as_bool()).unwrap_or(false);
        if prev.pen_docked {
            let tx = v.get("pen_tx").and_then(|x| x.as_bool()).unwrap_or(false);
            let iin = v.get("pen_iin_ma").and_then(|x| x.as_i64()).unwrap_or(0);
            prev.pen_pct = v.get("pen_level").and_then(|x| x.as_i64()).unwrap_or(-1) as i32;
            prev.pen_charging = v
                .get("pen_charging")
                .and_then(|x| x.as_bool())
                .unwrap_or_else(|| tx && !(prev.pen_pct >= 100 && iin <= 5));
        } else {
            prev.pen_charging = false;
            prev.pen_pct = -1;
        }
        prev.pen_hover = v.get("pen_hover").and_then(|x| x.as_bool()).unwrap_or(false);
        prev.pen_action_seq = v
            .get("pen_action_seq")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        prev.pen_action_code = v
            .get("pen_action_code")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u16;
        true
    } else {
        false
    };

    if !got_daemon {
        if !is_bri_drag_recent() {
            if let (Some(b), Some(m)) = (
                read_i64_file("/sys/class/backlight/ktz8866-backlight/brightness"),
                read_i64_file("/sys/class/backlight/ktz8866-backlight/max_brightness"),
            ) {
                if m > 0 {
                    prev.brightness_pct = ((b * 100 + m / 2) / m).clamp(1, 100) as u8;
                }
            }
        }
        let docked = fs::read_to_string("/sys/class/power_supply/lenovo-pen/present")
            .map(|s| s.trim() == "1")
            .unwrap_or(false);
        prev.pen_docked = docked;
        if docked {
            prev.pen_pct = read_i64_file("/sys/class/power_supply/lenovo-pen/capacity")
                .map(|p| p as i32)
                .unwrap_or(-1);
        } else {
            prev.pen_pct = -1;
            prev.pen_charging = false;
        }
    }

    if !raw_bat_status.is_empty() {
        prev.bat_charging = compute_bat_charging(
            &raw_bat_status,
            usb_on,
            prev.bat_pct,
            prev.charge_limit,
            current_ua,
        );
    }
}

fn parse_wpctl_volume_line(s: &str) -> (Option<u8>, bool) {
    let muted = s.contains("MUTED");
    let pct = s
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse::<f32>().ok())
        .map(|v| (v * 100.0).round().clamp(0.0, 100.0) as u8);
    (pct, muted)
}

/// Match hardware/master sink, source, and server events from `pactl subscribe` while ignoring
/// high-frequency per-stream `sink-input` / `source-output` / `client` playback churn!
fn is_relevant_pactl_event(line: &str) -> bool {
    line.contains("on sink ") || line.contains("on source ") || line.contains("on server ")
}

async fn refresh_audio_wpctl(shared: &Arc<Mutex<SysSnapshot>>, wakeup_fd: Option<i32>) {
    let mut vol_pct: Option<u8> = None;
    let mut sink_muted: Option<bool> = None;
    let mut source_muted: Option<bool> = None;

    if let Ok(out) = tokio::process::Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .await
    {
        let s = String::from_utf8_lossy(&out.stdout);
        let (p, m) = parse_wpctl_volume_line(&s);
        vol_pct = p;
        sink_muted = Some(m);
    }
    if let Ok(out) = tokio::process::Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SOURCE@"])
        .output()
        .await
    {
        let s = String::from_utf8_lossy(&out.stdout);
        source_muted = Some(s.contains("MUTED"));
    }

    let (sm, srcm) = {
        let cur = shared.lock().unwrap();
        (
            sink_muted.unwrap_or(cur.muted),
            source_muted.unwrap_or(cur.mic_muted),
        )
    };
    sync_pogo_keyboard_leds(sm, srcm);

    update_shared_sys(shared, wakeup_fd, |sys| {
        if !is_vol_drag_recent() {
            if let Some(p) = vol_pct {
                sys.volume_pct = p;
            }
            if let Some(m) = sink_muted {
                sys.muted = m;
            }
        }
        if let Some(mm) = source_muted {
            sys.mic_muted = mm;
        }
    });
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

async fn dbus_get_prop(
    conn: &zbus::Connection,
    dest: &str,
    path: &str,
    iface: &str,
    prop: &str,
) -> Option<OwnedValue> {
    let reply = conn
        .call_method(
            Some(dest),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(iface, prop),
        )
        .await
        .ok()?;
    reply.body().deserialize::<OwnedValue>().ok()
}

async fn dbus_get_all_props(
    conn: &zbus::Connection,
    dest: &str,
    path: &str,
    iface: &str,
) -> Option<HashMap<String, OwnedValue>> {
    let reply = conn
        .call_method(
            Some(dest),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &(iface,),
        )
        .await
        .ok()?;
    reply.body().deserialize::<HashMap<String, OwnedValue>>().ok()
}

fn decode_ssid_bytes(val: &OwnedValue) -> String {
    let v: &Value<'_> = val;
    if let Value::Array(arr) = v {
        let mut bytes = Vec::with_capacity(arr.len());
        for elem in arr.iter() {
            if let Ok(b) = u8::try_from(elem) {
                bytes.push(b);
            }
        }
        return String::from_utf8_lossy(&bytes).trim().to_string();
    }
    String::new()
}

async fn fetch_saved_wifi_ssids(conn: &zbus::Connection) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(reply) = conn
        .call_method(
            Some("org.freedesktop.NetworkManager"),
            "/org/freedesktop/NetworkManager/Settings",
            Some("org.freedesktop.NetworkManager.Settings"),
            "ListConnections",
            &(),
        )
        .await
    else {
        return out;
    };
    let Ok(paths) = reply.body().deserialize::<Vec<OwnedObjectPath>>() else {
        return out;
    };
    type ConnSettings = HashMap<String, HashMap<String, OwnedValue>>;
    for p in paths.into_iter().take(48) {
        let Ok(rep) = conn
            .call_method(
                Some("org.freedesktop.NetworkManager"),
                p.as_str(),
                Some("org.freedesktop.NetworkManager.Settings.Connection"),
                "GetSettings",
                &(),
            )
            .await
        else {
            continue;
        };
        let Ok(settings) = rep.body().deserialize::<ConnSettings>() else {
            continue;
        };
        if let Some(wl) = settings.get("802-11-wireless") {
            if let Some(ssid_val) = wl.get("ssid") {
                let s = decode_ssid_bytes(ssid_val);
                if !s.is_empty() {
                    out.insert(s);
                }
            }
            if let Some(conn_sec) = settings.get("connection") {
                if let Some(id_str) = conn_sec.get("id").and_then(|v| <&str>::try_from(v).ok()) {
                    let trimmed = id_str.trim();
                    if !trimmed.is_empty() {
                        out.insert(trimmed.to_string());
                    }
                }
            }
        }
    }
    out
}

async fn refresh_nm_dbus(
    conn: &zbus::Connection,
    cached_wifi_dev: &mut Option<String>,
    shared: &Arc<Mutex<SysSnapshot>>,
    wakeup_fd: Option<i32>,
) {
    let wifi_on = dbus_get_prop(
        conn,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
        "WirelessEnabled",
    )
    .await
    .and_then(|v| bool::try_from(v).ok())
    .unwrap_or(false);

    if !wifi_on {
        update_shared_sys(shared, wakeup_fd, |sys| {
            sys.wifi_on = false;
            sys.wifi_ssid.clear();
            sys.wifi_list.clear();
        });
        return;
    }

    // Locate `NM_DEVICE_TYPE_WIFI` (`DeviceType == 2`)
    let mut wifi_dev = cached_wifi_dev.clone();
    if let Some(ref p) = wifi_dev {
        let dt = dbus_get_prop(
            conn,
            "org.freedesktop.NetworkManager",
            p,
            "org.freedesktop.NetworkManager.Device",
            "DeviceType",
        )
        .await
        .and_then(|v| u32::try_from(v).ok());
        if dt != Some(2) {
            wifi_dev = None;
        }
    }
    if wifi_dev.is_none() {
        if let Ok(reply) = conn
            .call_method(
                Some("org.freedesktop.NetworkManager"),
                "/org/freedesktop/NetworkManager",
                Some("org.freedesktop.NetworkManager"),
                "GetDevices",
                &(),
            )
            .await
        {
            if let Ok(devs) = reply.body().deserialize::<Vec<OwnedObjectPath>>() {
                for d in devs {
                    let p = d.as_str();
                    let dt = dbus_get_prop(
                        conn,
                        "org.freedesktop.NetworkManager",
                        p,
                        "org.freedesktop.NetworkManager.Device",
                        "DeviceType",
                    )
                    .await
                    .and_then(|v| u32::try_from(v).ok());
                    if dt == Some(2) {
                        wifi_dev = Some(p.to_string());
                        break;
                    }
                }
            }
        }
        *cached_wifi_dev = wifi_dev.clone();
    }

    let saved_ssids = fetch_saved_wifi_ssids(conn).await;
    let mut active_ssid = String::new();
    let mut active_sig = 0u8;
    let mut nets: Vec<WifiNetwork> = Vec::new();

    if let Some(dev_path) = wifi_dev {
        let active_ap = dbus_get_prop(
            conn,
            "org.freedesktop.NetworkManager",
            &dev_path,
            "org.freedesktop.NetworkManager.Device.Wireless",
            "ActiveAccessPoint",
        )
        .await
        .and_then(|v| OwnedObjectPath::try_from(v).ok())
        .map(|o| o.to_string())
        .unwrap_or_default();

        let ap_paths: Vec<String> = dbus_get_prop(
            conn,
            "org.freedesktop.NetworkManager",
            &dev_path,
            "org.freedesktop.NetworkManager.Device.Wireless",
            "AccessPoints",
        )
        .await
        .map(|v| {
            let mut vpaths = Vec::new();
            let v_ref: &Value<'_> = &v;
            if let Value::Array(arr) = v_ref {
                for e in arr.iter() {
                    if let Value::ObjectPath(op) = e {
                        vpaths.push(op.as_str().to_string());
                    }
                }
            }
            vpaths
        })
        .unwrap_or_default();

        for ap in ap_paths.into_iter().take(64) {
            if ap.is_empty() || ap == "/" {
                continue;
            }
            let Some(props) = dbus_get_all_props(
                conn,
                "org.freedesktop.NetworkManager",
                &ap,
                "org.freedesktop.NetworkManager.AccessPoint",
            )
            .await
            else {
                continue;
            };
            let ssid = props.get("Ssid").map(decode_ssid_bytes).unwrap_or_default();
            if ssid.is_empty() {
                continue;
            }
            let sig = props
                .get("Strength")
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(0)
                .min(100);
            let flags = props
                .get("Flags")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0);
            let wpa_flags = props
                .get("WpaFlags")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0);
            let rsn_flags = props
                .get("RsnFlags")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0);
            let secured = (flags & 1) != 0 || wpa_flags != 0 || rsn_flags != 0;
            let is_saved = saved_ssids.contains(&ssid);
            let is_act = ap == active_ap;
            if is_act {
                active_ssid = ssid.clone();
                active_sig = sig;
            }
            if let Some(existing) = nets.iter_mut().find(|n| n.ssid == ssid) {
                existing.signal = existing.signal.max(sig);
                existing.active |= is_act;
                existing.secured |= secured;
                existing.saved |= is_saved;
            } else {
                nets.push(WifiNetwork {
                    ssid,
                    signal: sig,
                    active: is_act,
                    secured,
                    saved: is_saved,
                });
            }
        }
        nets.sort_by(|a, b| {
            b.active
                .cmp(&a.active)
                .then_with(|| b.saved.cmp(&a.saved))
                .then_with(|| b.signal.cmp(&a.signal))
        });
        nets.truncate(18);
    }

    update_shared_sys(shared, wakeup_fd, |sys| {
        sys.wifi_on = true;
        sys.wifi_ssid = active_ssid;
        sys.wifi_signal = active_sig;
        sys.wifi_list = nets;
    });
}

fn looks_like_raw_mac(s: &str) -> bool {
    let t = s.trim();
    if t.len() == 17 {
        let parts: Vec<&str> = t.split(|c| c == ':' || c == '-').collect();
        if parts.len() == 6
            && parts
                .iter()
                .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return true;
        }
    }
    false
}

async fn refresh_bluez_dbus(
    conn: &zbus::Connection,
    ble_pen_since: &mut HashMap<String, Instant>,
    ble_pen_bat_cache: &mut HashMap<String, u8>,
    shared: &Arc<Mutex<SysSnapshot>>,
    wakeup_fd: Option<i32>,
) -> bool {
    type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

    let Ok(reply) = conn
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await
    else {
        return false;
    };
    let Ok(objs) = reply.body().deserialize::<ManagedObjects>() else {
        return false;
    };

    let mut bt_on = false;
    for ifaces in objs.values() {
        if let Some(adp) = ifaces.get("org.bluez.Adapter1") {
            if adp
                .get("Powered")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false)
            {
                bt_on = true;
                break;
            }
        }
    }

    if !bt_on {
        ble_pen_since.clear();
        ble_pen_bat_cache.clear();
        update_shared_sys(shared, wakeup_fd, |sys| {
            sys.bt_on = false;
            sys.bt_device.clear();
            sys.bt_peripherals.clear();
        });
        return false;
    }

    let sysfs_bats = read_sysfs_hid_batteries();
    let mut peripherals: Vec<BtPeripheral> = Vec::new();
    let mut pending_pen_recheck = false;
    for ifaces in objs.values() {
        let Some(dev) = ifaces.get("org.bluez.Device1") else {
            continue;
        };
        let paired = dev
            .get("Paired")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        let connected = dev
            .get("Connected")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        let services_resolved = dev
            .get("ServicesResolved")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        let mac = dev
            .get("Address")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("")
            .trim()
            .to_string();
        if mac.is_empty() {
            continue;
        }
        let raw_name = dev
            .get("Name")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("")
            .trim();
        let name = dev
            .get("Alias")
            .and_then(|v| <&str>::try_from(v).ok())
            .or_else(|| dev.get("Name").and_then(|v| <&str>::try_from(v).ok()))
            .unwrap_or("蓝牙设备")
            .trim()
            .to_string();
        if !paired && !connected {
            let discovering = BT_DISCOVERING.load(std::sync::atomic::Ordering::Relaxed);
            if !discovering
                || raw_name.is_empty()
                || looks_like_raw_mac(&name)
                || name.eq_ignore_ascii_case(&mac)
            {
                continue;
            }
        }
        let icon_hint = dev
            .get("Icon")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("")
            .trim()
            .to_string();
        let ble_bat = ifaces
            .get("org.bluez.Battery1")
            .and_then(|b| b.get("Percentage"))
            .and_then(|v| u8::try_from(v).ok())
            .map(|p| p.min(100));
        let mut bat = ble_bat;

        let nl = name.to_ascii_lowercase();
        let official_pen = (nl.contains("lenovo") && nl.contains("pen"))
            || nl.contains("tab pen")
            || nl.contains("ap500u")
            || nl.contains("ap501u");

        if official_pen {
            if let Some(p) = ble_bat {
                ble_pen_bat_cache.insert(mac.clone(), p);
            } else if connected {
                bat = ble_pen_bat_cache.get(&mac).copied();
            } else {
                ble_pen_bat_cache.remove(&mac);
            }
        } else if bat.is_none() {
            let mac_l = mac.to_ascii_lowercase();
            for (m, cap) in &sysfs_bats {
                if m.contains(&nl) || nl.contains(m) || m.contains(&mac_l) {
                    bat = Some(*cap);
                    break;
                }
            }
        }

        let kind = if official_pen {
            BtDevKind::Stylus
        } else {
            BtDevKind::from_bluez_hints(&icon_hint, &name)
        };
        // Filter 2-second unauthenticated BLE flapping (`PIN or Key Missing` / `read_pnpid_cb`):
        // - Once GATT services resolve or `org.bluez.Battery1` is present, encryption succeeded
        //   and the BLE session is immediately valid (do not block behind a 2.8s timer deadlock).
        // - Otherwise require `>= 2.8s` continuous link and schedule a follow-up recheck.
        let stable_connected = if official_pen {
            if !connected {
                ble_pen_since.remove(&mac);
                false
            } else if ble_bat.is_some() || services_resolved {
                ble_pen_since.entry(mac.clone()).or_insert_with(Instant::now);
                true
            } else {
                let since = *ble_pen_since.entry(mac.clone()).or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(2800) {
                    true
                } else {
                    pending_pen_recheck = true;
                    false
                }
            }
        } else {
            connected
        };
        peripherals.push(BtPeripheral {
            name,
            mac,
            kind,
            battery_pct: bat,
            official_pen,
            paired,
            connected: stable_connected,
            charging: false,
        });
    }
    peripherals.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.paired.cmp(&a.paired))
            .then_with(|| a.name.cmp(&b.name))
    });

    let bt_device = peripherals
        .iter()
        .find(|d| d.connected)
        .map(|d| d.name.clone())
        .unwrap_or_default();

    update_shared_sys(shared, wakeup_fd, |sys| {
        sys.bt_on = true;
        sys.bt_device = bt_device;
        sys.bt_peripherals = peripherals;
    });
    pending_pen_recheck
}

async fn refresh_power_profile_dbus(
    conn: &zbus::Connection,
    shared: &Arc<Mutex<SysSnapshot>>,
    wakeup_fd: Option<i32>,
) {
    let mut profile = dbus_get_prop(
        conn,
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
        "ActiveProfile",
    )
    .await
    .and_then(|v| String::try_from(v).ok());

    if profile.is_none() {
        profile = dbus_get_prop(
            conn,
            "net.hadess.PowerProfiles",
            "/net/hadess/PowerProfiles",
            "net.hadess.PowerProfiles",
            "ActiveProfile",
        )
        .await
        .and_then(|v| String::try_from(v).ok());
    }

    if let Some(p) = profile.filter(|s| !s.is_empty()) {
        update_shared_sys(shared, wakeup_fd, |sys| {
            sys.power_profile = p;
        });
    }
}

fn extract_mpris_metadata(meta_val: &OwnedValue) -> (String, String) {
    let mut title = String::new();
    let mut artist = String::new();
    let v_ref: &Value<'_> = meta_val;
    if let Value::Dict(dict) = v_ref {
        for (k, v) in dict.iter() {
            let Ok(key) = <&str>::try_from(k) else {
                continue;
            };
            let inner = match v {
                Value::Value(boxed) => boxed.as_ref(),
                other => other,
            };
            match key {
                "xesam:title" => {
                    if let Ok(s) = <&str>::try_from(inner) {
                        title = s.trim().to_string();
                    }
                }
                "xesam:artist" => {
                    if let Value::Array(arr) = inner {
                        let parts: Vec<&str> = arr
                            .iter()
                            .filter_map(|e| <&str>::try_from(e).ok())
                            .filter(|s| !s.trim().is_empty())
                            .collect();
                        artist = parts.join(" / ");
                    } else if let Ok(s) = <&str>::try_from(inner) {
                        artist = s.trim().to_string();
                    }
                }
                _ => {}
            }
        }
    }
    (title, artist)
}

async fn refresh_mpris_dbus(
    conn: &zbus::Connection,
    active_player: &Arc<Mutex<Option<String>>>,
    shared: &Arc<Mutex<SysSnapshot>>,
    wakeup_fd: Option<i32>,
) {
    let Ok(reply) = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )
        .await
    else {
        return;
    };
    let Ok(names) = reply.body().deserialize::<Vec<String>>() else {
        return;
    };
    let players: Vec<String> = names
        .into_iter()
        .filter(|n| n.starts_with("org.mpris.MediaPlayer2."))
        .collect();

    let mut chosen_dest: Option<String> = None;
    let mut chosen_playing = false;
    let mut chosen_title = String::new();
    let mut chosen_artist = String::new();

    for dest in players {
        let Some(props) = dbus_get_all_props(
            conn,
            &dest,
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        )
        .await
        else {
            continue;
        };
        let playing = props
            .get("PlaybackStatus")
            .and_then(|v| <&str>::try_from(v).ok())
            == Some("Playing");
        let (title, artist) = props
            .get("Metadata")
            .map(extract_mpris_metadata)
            .unwrap_or_default();

        if chosen_dest.is_none() || (playing && !chosen_playing) {
            chosen_dest = Some(dest);
            chosen_playing = playing;
            chosen_title = title;
            chosen_artist = artist;
            if playing {
                break;
            }
        }
    }

    *active_player.lock().unwrap() = chosen_dest;
    update_shared_sys(shared, wakeup_fd, |sys| {
        sys.media_playing = chosen_playing;
        sys.media_title = chosen_title;
        sys.media_artist = chosen_artist;
    });
}

fn apply_sensor_orientation(orient: &str, last_orient: &mut String) {
    if orient.is_empty() || orient == "undefined" || orient == last_orient.as_str() {
        return;
    }
    let tr = match orient {
        "normal" => Some("Normal"),
        "bottom-up" => Some("_180"),
        "left-up" => Some("_90"),
        "right-up" => Some("_270"),
        _ => None,
    };
    if let Some(t) = tr {
        set_dsi_transform(t);
        *last_orient = orient.to_string();
    }
}

/// Unified single-thread Tokio runtime driving ALL shell D-Bus services, signal subscriptions
/// (`NetworkManager`, `BlueZ`, `PowerProfiles`, `logind`, `SensorProxy`, `MPRIS`), outgoing D-Bus
/// commands (`Osk`, Wi-Fi/BT/Media), debounced `pactl subscribe` audio watcher, and `2000ms`
/// pure-in-process sysfs/socket telemetry (`0` child processes at idle!).
pub fn spawn_shell_async_runtime(
    shared: Arc<Mutex<SysSnapshot>>,
    auto_rotate_flag: Arc<std::sync::atomic::AtomicBool>,
    wakeup_fd: Option<i32>,
    session_daemon: bool,
) -> mpsc::Receiver<ShellBusEvent> {
    let (bus_tx, bus_rx) = mpsc::channel::<ShellBusEvent>();
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<AsyncBusCmd>();
    let _ = ASYNC_BUS_TX.set(cmd_tx);

    // Seed initial sysfs/clock/PipeWire snapshot synchronously so frame 1 already shows accurate clock, battery & volume
    {
        let mut init = shared.lock().unwrap();
        poll_inprocess_hw_and_clock(&mut init);
        if session_daemon {
            if let Ok(out) = std::process::Command::new("wpctl")
                .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
                .output()
            {
                let s = String::from_utf8_lossy(&out.stdout);
                let (p, m) = parse_wpctl_volume_line(&s);
                if let Some(v) = p {
                    init.volume_pct = v;
                }
                init.muted = m;
            }
            if let Ok(out) = std::process::Command::new("wpctl")
                .args(["get-volume", "@DEFAULT_AUDIO_SOURCE@"])
                .output()
            {
                init.mic_muted = String::from_utf8_lossy(&out.stdout).contains("MUTED");
            }
        }
    }

    std::thread::Builder::new()
        .name("tb520fu-async-bus".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio");
            let local = tokio::task::LocalSet::new();
            rt.block_on(local.run_until(async move {
                let sys_conn = zbus::Connection::system().await.ok();
                let ses_conn = if session_daemon {
                    zbus::Connection::session().await.ok()
                } else {
                    None
                };

                if let Some(ref sc) = ses_conn {
                    crate::notifications::register_session_objects(sc, bus_tx.clone(), wakeup_fd)
                        .await;
                }

                // 0. Sub-millisecond Unix control socket (`/run/user/$UID/tb520fu-shell.sock`)
                //    for zero-lag hardware volume / brightness / mute OSD & shell triggers!
                if session_daemon {
                    spawn_pogo_touchpad_edge_listener(bus_tx.clone(), wakeup_fd);

                    let sock_path = shell_ctrl_sock_path();
                    let _ = fs::remove_file(&sock_path);
                    if let Ok(listener) = tokio::net::UnixListener::bind(&sock_path) {
                        let tx_ctrl = bus_tx.clone();
                        tokio::task::spawn_local(async move {
                            while let Ok((stream, _)) = listener.accept().await {
                                let tx_conn = tx_ctrl.clone();
                                tokio::task::spawn_local(async move {
                                    let mut lines = tokio::io::AsyncBufReadExt::lines(
                                        tokio::io::BufReader::new(stream),
                                    );
                                    while let Ok(Some(line)) = lines.next_line().await {
                                        if let Some(ev) = parse_shell_ctrl_line(&line) {
                                            let _ = tx_conn.send(ev);
                                            wake_fd(wakeup_fd);
                                        }
                                    }
                                });
                            }
                        });
                    }
                }

                // 1. Calm `1500ms` pure in-process sysfs + persistent daemon Unix socket + clock task (`0` forks, `<2s` keepalive!)
                {
                    let shared_hw = shared.clone();
                    tokio::task::spawn_local(async move {
                        let mut ticker = tokio::time::interval(Duration::from_millis(1500));
                        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        loop {
                            ticker.tick().await;
                            update_shared_sys(&shared_hw, wakeup_fd, poll_inprocess_hw_and_clock);
                        }
                    });
                }

                // 2. Persistent `pactl subscribe` audio watcher with `50ms` debounce (`0` forks at idle!)
                if session_daemon {
                    let shared_aud = shared.clone();
                    tokio::task::spawn_local(async move {
                        refresh_audio_wpctl(&shared_aud, wakeup_fd).await;
                        loop {
                            let Ok(mut child) = tokio::process::Command::new("pactl")
                                .arg("subscribe")
                                .stdout(Stdio::piped())
                                .stderr(Stdio::null())
                                .kill_on_drop(true)
                                .spawn()
                            else {
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                continue;
                            };
                            let Some(stdout) = child.stdout.take() else {
                                let _ = child.wait().await;
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                continue;
                            };
                            let mut lines = tokio::io::AsyncBufReadExt::lines(
                                tokio::io::BufReader::new(stdout),
                            );
                            loop {
                                let Ok(Some(line)) = lines.next_line().await else {
                                    break;
                                };
                                if !is_relevant_pactl_event(&line) {
                                    continue;
                                }
                                // 28ms debounce window coalescing volume-slider bursts
                                let deadline =
                                    tokio::time::Instant::now() + Duration::from_millis(28);
                                while let Ok(Ok(Some(_))) =
                                    tokio::time::timeout_at(deadline, lines.next_line()).await
                                {}
                                refresh_audio_wpctl(&shared_aud, wakeup_fd).await;
                            }
                            let _ = child.wait().await;
                            tokio::time::sleep(Duration::from_secs(3)).await;
                        }
                    });
                }

                // 3. System D-Bus tasks (`NetworkManager`, `BlueZ`, `PowerProfiles`, `logind`, `SensorProxy`)
                if let Some(ref sc) = sys_conn {
                    // 3a. NetworkManager
                    let sc_nm = sc.clone();
                    let shared_nm = shared.clone();
                    tokio::task::spawn_local(async move {
                        let mut cached_dev: Option<String> = None;
                        refresh_nm_dbus(&sc_nm, &mut cached_dev, &shared_nm, wakeup_fd).await;
                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .path_namespace("/org/freedesktop/NetworkManager")
                            .map(|b| b.build())
                        else {
                            return;
                        };
                        let Ok(mut stream) =
                            zbus::MessageStream::for_match_rule(rule, &sc_nm, Some(32)).await
                        else {
                            return;
                        };
                        let mut last_ap_refresh = Instant::now();
                        while let Some(msg) = next_bus_msg(&mut stream).await {
                            let is_ap = msg
                                .header()
                                .path()
                                .map(|p| p.as_str().starts_with("/org/freedesktop/NetworkManager/AccessPoint/"))
                                .unwrap_or(false);
                            if is_ap && last_ap_refresh.elapsed() < Duration::from_millis(900) {
                                continue;
                            }
                            let deadline =
                                tokio::time::Instant::now() + Duration::from_millis(120);
                            while tokio::time::timeout_at(deadline, next_bus_msg(&mut stream))
                                .await
                                .is_ok()
                            {}
                            last_ap_refresh = Instant::now();
                            refresh_nm_dbus(&sc_nm, &mut cached_dev, &shared_nm, wakeup_fd).await;
                        }
                    });

                    // 3b. BlueZ (`GetManagedObjects`)
                    let sc_bz = sc.clone();
                    let shared_bz = shared.clone();
                    tokio::task::spawn_local(async move {
                        let mut ble_pen_since: HashMap<String, Instant> = HashMap::new();
                        let mut ble_pen_bat_cache: HashMap<String, u8> = HashMap::new();
                        let mut pending_recheck = refresh_bluez_dbus(
                            &sc_bz,
                            &mut ble_pen_since,
                            &mut ble_pen_bat_cache,
                            &shared_bz,
                            wakeup_fd,
                        )
                        .await;
                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .path_namespace("/org/bluez")
                            .map(|b| b.build())
                        else {
                            return;
                        };
                        let Ok(mut stream) =
                            zbus::MessageStream::for_match_rule(rule, &sc_bz, Some(32)).await
                        else {
                            return;
                        };
                        let mut last_bz_refresh = Instant::now();
                        loop {
                            let woke = if pending_recheck {
                                match tokio::time::timeout(
                                    Duration::from_millis(2900),
                                    next_bus_msg(&mut stream),
                                )
                                .await
                                {
                                    Ok(Some(_)) => true,
                                    Ok(None) => break,
                                    Err(_) => false,
                                }
                            } else {
                                match next_bus_msg(&mut stream).await {
                                    Some(_) => true,
                                    None => break,
                                }
                            };
                            if woke {
                                let discovering =
                                    BT_DISCOVERING.load(std::sync::atomic::Ordering::Relaxed);
                                let min_gap = if discovering {
                                    Duration::from_millis(650)
                                } else {
                                    Duration::from_millis(1800)
                                };
                                let since = last_bz_refresh.elapsed();
                                let wait = if since < min_gap {
                                    (min_gap - since).max(Duration::from_millis(250))
                                } else {
                                    Duration::from_millis(250)
                                };
                                let deadline = tokio::time::Instant::now() + wait;
                                while matches!(
                                    tokio::time::timeout_at(deadline, next_bus_msg(&mut stream))
                                        .await,
                                    Ok(Some(_))
                                ) {}
                            }
                            last_bz_refresh = Instant::now();
                            pending_recheck = refresh_bluez_dbus(
                                &sc_bz,
                                &mut ble_pen_since,
                                &mut ble_pen_bat_cache,
                                &shared_bz,
                                wakeup_fd,
                            )
                            .await;
                        }
                    });

                    // 3c. PowerProfiles (`UPower.PowerProfiles` / `hadess.PowerProfiles`)
                    let sc_pp = sc.clone();
                    let shared_pp = shared.clone();
                    tokio::task::spawn_local(async move {
                        refresh_power_profile_dbus(&sc_pp, &shared_pp, wakeup_fd).await;
                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .interface("org.freedesktop.DBus.Properties")
                            .and_then(|b| b.member("PropertiesChanged"))
                            .map(|b| b.build())
                        else {
                            return;
                        };
                        let Ok(mut stream) =
                            zbus::MessageStream::for_match_rule(rule, &sc_pp, Some(16)).await
                        else {
                            return;
                        };
                        while let Some(msg) = next_bus_msg(&mut stream).await {
                            let is_pp = msg
                                .header()
                                .path()
                                .map(|x| x.as_str().contains("PowerProfiles"))
                                .unwrap_or(false);
                            if is_pp {
                                refresh_power_profile_dbus(&sc_pp, &shared_pp, wakeup_fd).await;
                            }
                        }
                    });

                    // 3d. systemd-logind (`Lock` / `Unlock` + startup LockedHint takeover)
                    let sc_lg = sc.clone();
                    let tx_lg = bus_tx.clone();
                    tokio::task::spawn_local(async move {
                        if session_daemon {
                            let my_uid = unsafe { libc::getuid() };
                            if let Ok(reply) = sc_lg
                                .call_method(
                                    Some("org.freedesktop.login1"),
                                    "/org/freedesktop/login1",
                                    Some("org.freedesktop.login1.Manager"),
                                    "ListSessions",
                                    &(),
                                )
                                .await
                            {
                                type SessRow = (
                                    String,
                                    u32,
                                    String,
                                    String,
                                    zbus::zvariant::OwnedObjectPath,
                                );
                                if let Ok(rows) = reply.body().deserialize::<Vec<SessRow>>() {
                                    for (_id, uid, _user, seat, path) in rows {
                                        if uid == my_uid && seat == "seat0" {
                                            if let Ok(prop) = sc_lg
                                                .call_method(
                                                    Some("org.freedesktop.login1"),
                                                    path.as_str(),
                                                    Some("org.freedesktop.DBus.Properties"),
                                                    "Get",
                                                    &("org.freedesktop.login1.Session", "LockedHint"),
                                                )
                                                .await
                                            {
                                                if let Ok(val) = prop
                                                    .body()
                                                    .deserialize::<zbus::zvariant::OwnedValue>()
                                                {
                                                    if bool::try_from(val).unwrap_or(false) {
                                                        let _ = tx_lg.send(
                                                            crate::notifications::ShellBusEvent::LockRequested,
                                                        );
                                                        wake_fd(wakeup_fd);
                                                    }
                                                }
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                        }

                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .interface("org.freedesktop.login1.Session")
                            .map(|b| b.build())
                        else {
                            return;
                        };
                        let Ok(mut stream) =
                            zbus::MessageStream::for_match_rule(rule, &sc_lg, Some(16)).await
                        else {
                            return;
                        };
                        while let Some(msg) = next_bus_msg(&mut stream).await {
                            let ev_opt = msg
                                .header()
                                .member()
                                .and_then(|m| crate::notifications::decode_logind_signal(m.as_str()));
                            if let Some(ev) = ev_opt {
                                let _ = tx_lg.send(ev);
                                wake_fd(wakeup_fd);
                            }
                        }
                    });

                    // 3d-ii. `PrepareForSleep(false)` instant clock/battery/DPMS sync on S3 resume
                    let sc_resume = sc.clone();
                    let shared_resume = shared.clone();
                    let tx_resume = bus_tx.clone();
                    tokio::task::spawn_local(async move {
                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .interface("org.freedesktop.login1.Manager")
                            .and_then(|b| b.member("PrepareForSleep"))
                            .map(|b| b.build())
                        else {
                            return;
                        };
                        let Ok(mut stream) =
                            zbus::MessageStream::for_match_rule(rule, &sc_resume, Some(8)).await
                        else {
                            return;
                        };
                        while let Some(msg) = next_bus_msg(&mut stream).await {
                            if msg.body().deserialize::<bool>() == Ok(false) {
                                update_shared_sys(
                                    &shared_resume,
                                    wakeup_fd,
                                    poll_inprocess_hw_and_clock,
                                );
                                let _ = tx_resume
                                    .send(crate::notifications::ShellBusEvent::SystemResumed);
                                wake_fd(wakeup_fd);
                            }
                        }
                    });

                    // 3e. `iio-sensor-proxy` accelerometer auto-rotate (event-driven `PropertiesChanged` + 2s claim sync)
                    let sc_rot = sc.clone();
                    tokio::task::spawn_local(async move {
                        let mut claimed = false;
                        let mut last_orient = String::new();
                        let mut stream_opt = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .path("/net/hadess/SensorProxy")
                            .and_then(|b| b.interface("org.freedesktop.DBus.Properties"))
                            .and_then(|b| b.member("PropertiesChanged"))
                            .ok()
                            .map(|b| b.build());
                        let mut prop_stream = match stream_opt.take() {
                            Some(r) => zbus::MessageStream::for_match_rule(r, &sc_rot, Some(8))
                                .await
                                .ok(),
                            None => None,
                        };

                        loop {
                            let panel_on = fs::read_to_string("/sys/class/drm/card1-DSI-1/dpms")
                                .map(|s| s.trim() == "On")
                                .unwrap_or(true);
                            let want = auto_rotate_flag.load(std::sync::atomic::Ordering::Relaxed)
                                && panel_on;
                            if want != claimed {
                                let m = if want {
                                    "ClaimAccelerometer"
                                } else {
                                    "ReleaseAccelerometer"
                                };
                                if sc_rot
                                    .call_method(
                                        Some("net.hadess.SensorProxy"),
                                        "/net/hadess/SensorProxy",
                                        Some("net.hadess.SensorProxy"),
                                        m,
                                        &(),
                                    )
                                    .await
                                    .is_ok()
                                {
                                    claimed = want;
                                    if claimed {
                                        if let Some(val) = dbus_get_prop(
                                            &sc_rot,
                                            "net.hadess.SensorProxy",
                                            "/net/hadess/SensorProxy",
                                            "net.hadess.SensorProxy",
                                            "AccelerometerOrientation",
                                        )
                                        .await
                                        {
                                            if let Ok(orient) = String::try_from(val) {
                                                apply_sensor_orientation(&orient, &mut last_orient);
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some(ref mut st) = prop_stream {
                                if tokio::time::timeout(
                                    Duration::from_millis(2000),
                                    next_bus_msg(st),
                                )
                                .await
                                .is_ok()
                                    && claimed
                                {
                                    if let Some(val) = dbus_get_prop(
                                        &sc_rot,
                                        "net.hadess.SensorProxy",
                                        "/net/hadess/SensorProxy",
                                        "net.hadess.SensorProxy",
                                        "AccelerometerOrientation",
                                    )
                                    .await
                                    {
                                        if let Ok(orient) = String::try_from(val) {
                                            apply_sensor_orientation(&orient, &mut last_orient);
                                        }
                                    }
                                }
                            } else {
                                tokio::time::sleep(Duration::from_millis(2000)).await;
                            }
                        }
                    });
                }

                // 4. Session D-Bus MPRIS media watcher (`0` forks!)
                let active_mpris = Arc::new(Mutex::new(None::<String>));
                if let Some(ref ses) = ses_conn {
                    let ses_mp = ses.clone();
                    let shared_mp = shared.clone();
                    let act_mp = active_mpris.clone();
                    tokio::task::spawn_local(async move {
                        refresh_mpris_dbus(&ses_mp, &act_mp, &shared_mp, wakeup_fd).await;

                        let ses_owner = ses_mp.clone();
                        let shared_owner = shared_mp.clone();
                        let act_owner = act_mp.clone();
                        tokio::task::spawn_local(async move {
                            let Ok(rule) = zbus::MatchRule::builder()
                                .msg_type(zbus::message::Type::Signal)
                                .interface("org.freedesktop.DBus")
                                .and_then(|b| b.member("NameOwnerChanged"))
                                .map(|b| b.build())
                            else {
                                return;
                            };
                            let Ok(mut stream) =
                                zbus::MessageStream::for_match_rule(rule, &ses_owner, Some(16))
                                    .await
                            else {
                                return;
                            };
                            while let Some(msg) = next_bus_msg(&mut stream).await {
                                if let Ok((name, _, _)) =
                                    msg.body().deserialize::<(String, String, String)>()
                                {
                                    if name.starts_with("org.mpris.MediaPlayer2.") {
                                        refresh_mpris_dbus(
                                            &ses_owner,
                                            &act_owner,
                                            &shared_owner,
                                            wakeup_fd,
                                        )
                                        .await;
                                    }
                                }
                            }
                        });

                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .path("/org/mpris/MediaPlayer2")
                            .and_then(|b| b.interface("org.freedesktop.DBus.Properties"))
                            .and_then(|b| b.member("PropertiesChanged"))
                            .map(|b| b.build())
                        else {
                            return;
                        };
                        let Ok(mut stream) =
                            zbus::MessageStream::for_match_rule(rule, &ses_mp, Some(16)).await
                        else {
                            return;
                        };
                        while next_bus_msg(&mut stream).await.is_some() {
                            let deadline =
                                tokio::time::Instant::now() + Duration::from_millis(80);
                            while tokio::time::timeout_at(deadline, next_bus_msg(&mut stream))
                                .await
                                .is_ok()
                            {}
                            refresh_mpris_dbus(&ses_mp, &act_mp, &shared_mp, wakeup_fd).await;
                        }
                    });
                }

                // 5. Outgoing command dispatcher reusing the existing `sys_conn` & `ses_conn`!
                let vol_flushing = std::rc::Rc::new(std::cell::Cell::new(false));
                let bri_flushing = std::rc::Rc::new(std::cell::Cell::new(false));
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        AsyncBusCmd::OskMethod(method) => {
                            if let Some(ref ses) = ses_conn {
                                let _ = ses
                                    .call_method(
                                        Some("fyi.tb520fu.Osk"),
                                        "/Osk",
                                        Some("fyi.tb520fu.Osk"),
                                        method,
                                        &(),
                                    )
                                    .await;
                            }
                        }
                        AsyncBusCmd::SetWifiEnabled(on) => {
                            let mut ok = false;
                            if let Some(ref sc) = sys_conn {
                                ok = sc
                                    .call_method(
                                        Some("org.freedesktop.NetworkManager"),
                                        "/org/freedesktop/NetworkManager",
                                        Some("org.freedesktop.DBus.Properties"),
                                        "Set",
                                        &(
                                            "org.freedesktop.NetworkManager",
                                            "WirelessEnabled",
                                            Value::from(on),
                                        ),
                                    )
                                    .await
                                    .is_ok();
                            }
                            if !ok {
                                let arg = if on { "on" } else { "off" };
                                let _ = tokio::process::Command::new("nmcli")
                                    .args(["radio", "wifi", arg])
                                    .status()
                                    .await;
                            }
                        }
                        AsyncBusCmd::TriggerWifiScan => {
                            let _ = tokio::task::spawn_blocking(|| {
                                if call_cmd_daemon_json("{\"cmd\":\"wifi_rescan\"}").is_none() {
                                    let _ = Command::new("nmcli")
                                        .args(["device", "wifi", "rescan"])
                                        .status();
                                }
                            });
                            if let Some(ref sc) = sys_conn {
                                let sc_scan = sc.clone();
                                let shared_scan = shared.clone();
                                tokio::task::spawn_local(async move {
                                    let mut dev_cache: Option<String> = None;
                                    for delay_ms in [1200u64, 1500, 1600] {
                                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                                        refresh_nm_dbus(
                                            &sc_scan,
                                            &mut dev_cache,
                                            &shared_scan,
                                            wakeup_fd,
                                        )
                                        .await;
                                    }
                                });
                            }
                        }
                        AsyncBusCmd::ConnectWifi { ssid, password } => {
                            let sc_opt = sys_conn.clone();
                            let shared_cw = shared.clone();
                            tokio::task::spawn_local(async move {
                                let s_cl = ssid.clone();
                                let pw_cl = password.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    let payload = serde_json::json!({
                                        "cmd": "wifi_connect",
                                        "ssid": s_cl,
                                        "password": pw_cl,
                                    })
                                    .to_string();
                                    if call_cmd_daemon_json(&payload).is_none() {
                                        if let Some(pw) = pw_cl.filter(|p| !p.is_empty()) {
                                            let _ = Command::new("nmcli")
                                                .args([
                                                    "device",
                                                    "wifi",
                                                    "connect",
                                                    &s_cl,
                                                    "password",
                                                    &pw,
                                                ])
                                                .status();
                                        } else {
                                            let ok = Command::new("nmcli")
                                                .args(["connection", "up", &s_cl])
                                                .status()
                                                .is_ok_and(|st| st.success());
                                            if !ok {
                                                let _ = Command::new("nmcli")
                                                    .args(["device", "wifi", "connect", &s_cl])
                                                    .status();
                                            }
                                        }
                                    }
                                })
                                .await;
                                if let Some(sc) = sc_opt {
                                    let mut dev_cache: Option<String> = None;
                                    refresh_nm_dbus(&sc, &mut dev_cache, &shared_cw, wakeup_fd)
                                        .await;
                                    tokio::time::sleep(Duration::from_millis(2200)).await;
                                    refresh_nm_dbus(&sc, &mut dev_cache, &shared_cw, wakeup_fd)
                                        .await;
                                }
                            });
                        }
                        AsyncBusCmd::SetBtEnabled(on) => {
                            let mut ok = false;
                            if let Some(ref sc) = sys_conn {
                                ok = sc
                                    .call_method(
                                        Some("org.bluez"),
                                        "/org/bluez/hci0",
                                        Some("org.freedesktop.DBus.Properties"),
                                        "Set",
                                        &("org.bluez.Adapter1", "Powered", Value::from(on)),
                                    )
                                    .await
                                    .is_ok();
                            }
                            if !ok {
                                let arg = if on { "on" } else { "off" };
                                let _ = tokio::process::Command::new("bluetoothctl")
                                    .args(["power", arg])
                                    .status()
                                    .await;
                            }
                        }
                        AsyncBusCmd::SetBtDiscovery(active) => {
                            if let Some(ref sc) = sys_conn {
                                let method = if active {
                                    "StartDiscovery"
                                } else {
                                    "StopDiscovery"
                                };
                                let _ = sc
                                    .call_method(
                                        Some("org.bluez"),
                                        "/org/bluez/hci0",
                                        Some("org.bluez.Adapter1"),
                                        method,
                                        &(),
                                    )
                                    .await;
                                let sc_ref = sc.clone();
                                let shared_ref = shared.clone();
                                tokio::task::spawn_local(async move {
                                    let mut pen_s = HashMap::new();
                                    let mut pen_c = HashMap::new();
                                    let _ = refresh_bluez_dbus(
                                        &sc_ref,
                                        &mut pen_s,
                                        &mut pen_c,
                                        &shared_ref,
                                        wakeup_fd,
                                    )
                                    .await;
                                    if active {
                                        for delay_ms in [1800u64, 3800] {
                                            if !BT_DISCOVERING
                                                .load(std::sync::atomic::Ordering::Relaxed)
                                            {
                                                break;
                                            }
                                            tokio::time::sleep(Duration::from_millis(delay_ms))
                                                .await;
                                            let _ = refresh_bluez_dbus(
                                                &sc_ref,
                                                &mut pen_s,
                                                &mut pen_c,
                                                &shared_ref,
                                                wakeup_fd,
                                            )
                                            .await;
                                        }
                                    }
                                });
                            }
                        }
                        AsyncBusCmd::ToggleBtDevice {
                            mac,
                            disconnect,
                            paired,
                        } => {
                            let sc_opt = sys_conn.clone();
                            let shared_bt = shared.clone();
                            tokio::task::spawn_local(async move {
                                let dev_path = format!(
                                    "/org/bluez/hci0/dev_{}",
                                    mac.trim().replace(':', "_").to_ascii_uppercase()
                                );
                                let mut ok = false;
                                if let Some(ref sc) = sc_opt {
                                    if disconnect {
                                        ok = sc
                                            .call_method(
                                                Some("org.bluez"),
                                                dev_path.as_str(),
                                                Some("org.bluez.Device1"),
                                                "Disconnect",
                                                &(),
                                            )
                                            .await
                                            .is_ok();
                                    } else {
                                        if !paired {
                                            let _ = sc
                                                .call_method(
                                                    Some("org.bluez"),
                                                    dev_path.as_str(),
                                                    Some("org.bluez.Device1"),
                                                    "Pair",
                                                    &(),
                                                )
                                                .await;
                                            let _ = sc
                                                .call_method(
                                                    Some("org.bluez"),
                                                    dev_path.as_str(),
                                                    Some("org.freedesktop.DBus.Properties"),
                                                    "Set",
                                                    &(
                                                        "org.bluez.Device1",
                                                        "Trusted",
                                                        Value::from(true),
                                                    ),
                                                )
                                                .await;
                                        }
                                        ok = sc
                                            .call_method(
                                                Some("org.bluez"),
                                                dev_path.as_str(),
                                                Some("org.bluez.Device1"),
                                                "Connect",
                                                &(),
                                            )
                                            .await
                                            .is_ok();
                                    }
                                }
                                if !ok {
                                    if disconnect {
                                        let _ = tokio::process::Command::new("bluetoothctl")
                                            .args(["disconnect", &mac])
                                            .status()
                                            .await;
                                    } else {
                                        if !paired {
                                            let _ = tokio::process::Command::new("bluetoothctl")
                                                .args(["pair", &mac])
                                                .status()
                                                .await;
                                            let _ = tokio::process::Command::new("bluetoothctl")
                                                .args(["trust", &mac])
                                                .status()
                                                .await;
                                        }
                                        let _ = tokio::process::Command::new("bluetoothctl")
                                            .args(["connect", &mac])
                                            .status()
                                            .await;
                                    }
                                }
                                if let Some(ref sc) = sc_opt {
                                    let mut pen_s = HashMap::new();
                                    let mut pen_c = HashMap::new();
                                    let _ = refresh_bluez_dbus(
                                        sc,
                                        &mut pen_s,
                                        &mut pen_c,
                                        &shared_bt,
                                        wakeup_fd,
                                    )
                                    .await;
                                }
                            });
                        }
                        AsyncBusCmd::MediaCmd(mc) => {
                            let method = match mc {
                                "previous" => "Previous",
                                "next" => "Next",
                                _ => "PlayPause",
                            };
                            let dest = active_mpris.lock().unwrap().clone();
                            let mut ok = false;
                            if let (Some(ref ses), Some(ref player)) = (&ses_conn, &dest) {
                                ok = ses
                                    .call_method(
                                        Some(player.as_str()),
                                        "/org/mpris/MediaPlayer2",
                                        Some("org.mpris.MediaPlayer2.Player"),
                                        method,
                                        &(),
                                    )
                                    .await
                                    .is_ok();
                            }
                            if !ok {
                                let _ = tokio::process::Command::new("playerctl")
                                    .arg(mc)
                                    .status()
                                    .await;
                            }
                        }
                        AsyncBusCmd::FlushSliders => {
                            if !vol_flushing.get()
                                && PENDING_VOL_PCT.load(std::sync::atomic::Ordering::Acquire) >= 0
                            {
                                vol_flushing.set(true);
                                let vf = vol_flushing.clone();
                                let shared_vol = shared.clone();
                                tokio::task::spawn_local(async move {
                                    loop {
                                        let v = PENDING_VOL_PCT
                                            .swap(-1, std::sync::atomic::Ordering::AcqRel);
                                        if v < 0 {
                                            break;
                                        }
                                        let v_u8 = (v as u8).min(100);
                                        shared_vol.lock().unwrap().volume_pct = v_u8;
                                        LAST_VOL_DRAG_MS.store(
                                            mono_millis(),
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                        let _ = tokio::process::Command::new("wpctl")
                                            .args([
                                                "set-volume",
                                                "@DEFAULT_AUDIO_SINK@",
                                                &format!("{v_u8}%"),
                                            ])
                                            .status()
                                            .await;
                                        LAST_VOL_DRAG_MS.store(
                                            mono_millis(),
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                    }
                                    vf.set(false);
                                });
                            }
                            if !bri_flushing.get()
                                && PENDING_BRI_PCT.load(std::sync::atomic::Ordering::Acquire) >= 0
                            {
                                bri_flushing.set(true);
                                let bf = bri_flushing.clone();
                                let shared_bri = shared.clone();
                                tokio::task::spawn_local(async move {
                                    loop {
                                        let b = PENDING_BRI_PCT
                                            .swap(-1, std::sync::atomic::Ordering::AcqRel);
                                        if b < 0 {
                                            break;
                                        }
                                        let b_u8 = (b as u8).clamp(1, 100);
                                        shared_bri.lock().unwrap().brightness_pct = b_u8;
                                        LAST_BRI_DRAG_MS.store(
                                            mono_millis(),
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                        let _ = tokio::task::spawn_blocking(move || {
                                            apply_brightness_blocking(b_u8);
                                        })
                                        .await;
                                        LAST_BRI_DRAG_MS.store(
                                            mono_millis(),
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                    }
                                    bf.set(false);
                                });
                            }
                        }
                        AsyncBusCmd::SetAutobright(on) => {
                            shared.lock().unwrap().autobright = on;
                            LAST_BRI_DRAG_MS
                                .store(mono_millis(), std::sync::atomic::Ordering::Relaxed);
                            let _ = tokio::task::spawn_blocking(move || {
                                let _ = call_cmd_daemon_json(&format!(
                                    "{{\"cmd\":\"set_autobright\",\"enabled\":{on}}}"
                                ));
                            })
                            .await;
                            LAST_BRI_DRAG_MS
                                .store(mono_millis(), std::sync::atomic::Ordering::Relaxed);
                        }
                        AsyncBusCmd::ToggleTorch => {
                            let _ = tokio::task::spawn_blocking(|| {
                                let _ = call_cmd_daemon_json("{\"cmd\":\"toggle_torch\"}");
                            })
                            .await;
                        }
                        AsyncBusCmd::SetChargeLimit(limit) => {
                            let _ = tokio::task::spawn_blocking(move || {
                                let _ = call_cmd_daemon_json(&format!(
                                    "{{\"cmd\":\"set_charge_limit\",\"limit\":{limit}}}"
                                ));
                            })
                            .await;
                        }
                        AsyncBusCmd::ToggleMute => {
                            let _ = tokio::process::Command::new("wpctl")
                                .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
                                .status()
                                .await;
                            refresh_audio_wpctl(&shared, wakeup_fd).await;
                        }
                        AsyncBusCmd::ToggleMicMute => {
                            let _ = tokio::process::Command::new("wpctl")
                                .args(["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"])
                                .status()
                                .await;
                            refresh_audio_wpctl(&shared, wakeup_fd).await;
                        }
                        AsyncBusCmd::UnlockLogindSession => {
                            let _ = tokio::process::Command::new("loginctl")
                                .arg("unlock-session")
                                .status()
                                .await;
                        }
                        AsyncBusCmd::GpuBoost(ms) => {
                            let _ = tokio::task::spawn_blocking(move || {
                                send_daemon_boost_now(ms);
                            });
                        }
                        AsyncBusCmd::EmitPrefsChanged { key, value_json } => {
                            if let Some(ref ses) = ses_conn {
                                let _ = ses
                                    .emit_signal(
                                        None::<&str>,
                                        "/Shell",
                                        "fyi.tb520fu.Shell",
                                        "PrefsChanged",
                                        &(key.as_str(), value_json.as_str()),
                                    )
                                    .await;
                            }
                        }
                    }
                }
            }));
        })
        .ok();

    bus_rx
}

// ---------------- Desktop App Scanner ----------------

pub fn scan_desktop_apps() -> Vec<DesktopApp> {
    let mut dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }

    let mut apps: Vec<DesktopApp> = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(app) = parse_desktop_file(&p) {
                if !apps.iter().any(|a| {
                    a.id == app.id || a.name == app.name || a.exec_argv == app.exec_argv
                }) {
                    apps.push(app);
                }
            }
        }
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

fn is_hidden_desktop_id(id: &str) -> bool {
    let low = id.to_ascii_lowercase();
    const EXACT_HIDDEN: &[&str] = &[
        "footclient",
        "foot-server",
        "org.fcitx.fcitx5",
        "org.fcitx.fcitx5-migrator",
        "fcitx5-migrator",
        "fcitx5-wayland-launcher",
        "kbd-layout-viewer5",
        "org.quickshell",
        "sm.puri.osk0",
        "onboard",
        "onboard-settings",
        "im-config",
        "nm-applet",
        "vim",
        "nvim",
    ];
    if EXACT_HIDDEN.contains(&low.as_str()) {
        return true;
    }
    low.starts_with("python3")
        || low.starts_with("xdg-desktop-portal")
        || low.starts_with("waydroid.")
}

fn parse_desktop_file(path: &Path) -> Option<DesktopApp> {
    let txt = fs::read_to_string(path).ok()?;
    let id = path.file_stem()?.to_string_lossy().into_owned();
    if is_hidden_desktop_id(&id) {
        return None;
    }
    let mut in_entry = false;
    let mut name = String::new();
    let mut name_zh = String::new();
    let mut exec = String::new();
    let mut icon_name = String::new();
    let mut categories = Vec::new();
    let mut no_display = false;
    let mut terminal = false;
    let mut app_type = String::new();

    for raw in txt.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "Type" => app_type = v.trim().to_string(),
            "NoDisplay" | "Hidden" if v.trim() == "true" => no_display = true,
            "Terminal" if v.trim() == "true" => terminal = true,
            "Name" if name.is_empty() => name = v.trim().to_string(),
            "Name[zh_CN]" | "Name[zh]" => name_zh = v.trim().to_string(),
            "Exec" if exec.is_empty() => exec = v.trim().to_string(),
            "Icon" if icon_name.is_empty() => icon_name = v.trim().to_string(),
            "Categories" => {
                categories = v
                    .split(';')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => {}
        }
    }

    if no_display || app_type != "Application" || exec.is_empty() {
        return None;
    }
    if terminal
        && !categories
            .iter()
            .any(|c| matches!(c.as_str(), "TerminalEmulator" | "Monitor"))
        && !matches!(id.as_str(), "btop" | "htop")
    {
        return None;
    }
    let chosen_name = if !name_zh.is_empty() {
        name_zh
    } else if id == "foot" && name == "Foot" {
        "终端".to_string()
    } else {
        name
    };
    if chosen_name.is_empty() {
        return None;
    }

    let exec_argv: Vec<String> = exec
        .split_whitespace()
        .filter(|tok| !tok.starts_with('%'))
        .map(|tok| tok.trim_matches('"').to_string())
        .collect();
    if exec_argv.is_empty() {
        return None;
    }

    let icon = resolve_desktop_icon(&icon_name, &id);
    Some(DesktopApp {
        id,
        name: chosen_name,
        exec_argv,
        icon_name,
        categories,
        icon,
    })
}

struct GdkPixbufApi {
    _gdk_lib: libloading::Library,
    _gobj_lib: libloading::Library,
    new_from_file_at_scale: unsafe extern "C" fn(
        filename: *const libc::c_char,
        width: libc::c_int,
        height: libc::c_int,
        preserve_aspect_ratio: libc::c_int,
        error: *mut *mut libc::c_void,
    ) -> *mut libc::c_void,
    get_width: unsafe extern "C" fn(pixbuf: *const libc::c_void) -> libc::c_int,
    get_height: unsafe extern "C" fn(pixbuf: *const libc::c_void) -> libc::c_int,
    get_rowstride: unsafe extern "C" fn(pixbuf: *const libc::c_void) -> libc::c_int,
    get_n_channels: unsafe extern "C" fn(pixbuf: *const libc::c_void) -> libc::c_int,
    get_bits_per_sample: unsafe extern "C" fn(pixbuf: *const libc::c_void) -> libc::c_int,
    get_pixels: unsafe extern "C" fn(pixbuf: *const libc::c_void) -> *const u8,
    g_object_unref: unsafe extern "C" fn(object: *mut libc::c_void),
}

fn gdk_pixbuf_api() -> Option<&'static GdkPixbufApi> {
    static API: OnceLock<Option<GdkPixbufApi>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let gobj_lib = libloading::Library::new("libgobject-2.0.so.0").ok()?;
        let gdk_lib = libloading::Library::new("libgdk_pixbuf-2.0.so.0").ok()?;
        let new_from_file_at_scale = *gdk_lib.get(b"gdk_pixbuf_new_from_file_at_scale\0").ok()?;
        let get_width = *gdk_lib.get(b"gdk_pixbuf_get_width\0").ok()?;
        let get_height = *gdk_lib.get(b"gdk_pixbuf_get_height\0").ok()?;
        let get_rowstride = *gdk_lib.get(b"gdk_pixbuf_get_rowstride\0").ok()?;
        let get_n_channels = *gdk_lib.get(b"gdk_pixbuf_get_n_channels\0").ok()?;
        let get_bits_per_sample = *gdk_lib.get(b"gdk_pixbuf_get_bits_per_sample\0").ok()?;
        let get_pixels = *gdk_lib.get(b"gdk_pixbuf_get_pixels\0").ok()?;
        let g_object_unref = *gobj_lib.get(b"g_object_unref\0").ok()?;
        Some(GdkPixbufApi {
            _gdk_lib: gdk_lib,
            _gobj_lib: gobj_lib,
            new_from_file_at_scale,
            get_width,
            get_height,
            get_rowstride,
            get_n_channels,
            get_bits_per_sample,
            get_pixels,
            g_object_unref,
        })
    })
    .as_ref()
}

fn load_icon_via_gdk_pixbuf(path: &Path, target_px: i32) -> Option<BitmapIcon> {
    use std::os::unix::ffi::OsStrExt;
    let api = gdk_pixbuf_api()?;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    unsafe {
        let pb = (api.new_from_file_at_scale)(
            cpath.as_ptr(),
            target_px,
            target_px,
            1,
            std::ptr::null_mut(),
        );
        if pb.is_null() {
            return None;
        }
        let w = (api.get_width)(pb);
        let h = (api.get_height)(pb);
        let stride = (api.get_rowstride)(pb);
        let n_ch = (api.get_n_channels)(pb);
        let bps = (api.get_bits_per_sample)(pb);
        let pixels = (api.get_pixels)(pb);

        let mut out_pixmap = None;
        if bps == 8
            && w > 0
            && w <= 512
            && h > 0
            && h <= 512
            && (n_ch == 3 || n_ch == 4)
            && stride >= w * n_ch
            && !pixels.is_null()
        {
            if let Some(mut pixmap) = tiny_skia::Pixmap::new(w as u32, h as u32) {
                let total_bytes =
                    ((h - 1) as usize) * (stride as usize) + (w as usize) * (n_ch as usize);
                let src = std::slice::from_raw_parts(pixels, total_bytes);
                let dst = pixmap.data_mut();
                let mut any_visible = false;
                for y in 0..(h as usize) {
                    let row_off = y * (stride as usize);
                    for x in 0..(w as usize) {
                        let si = row_off + x * (n_ch as usize);
                        let di = (y * (w as usize) + x) * 4;
                        let r = src[si];
                        let g = src[si + 1];
                        let b = src[si + 2];
                        if n_ch == 4 {
                            let a = src[si + 3];
                            if a > 8 {
                                any_visible = true;
                            }
                            let au = a as u32;
                            dst[di] = (((r as u32) * au + 127) / 255) as u8;
                            dst[di + 1] = (((g as u32) * au + 127) / 255) as u8;
                            dst[di + 2] = (((b as u32) * au + 127) / 255) as u8;
                            dst[di + 3] = a;
                        } else {
                            any_visible = true;
                            dst[di] = r;
                            dst[di + 1] = g;
                            dst[di + 2] = b;
                            dst[di + 3] = 255;
                        }
                    }
                }
                if any_visible {
                    out_pixmap = Some(BitmapIcon::from_pixmap(pixmap));
                }
            }
        }
        (api.g_object_unref)(pb);
        out_pixmap
    }
}

fn load_icon_file_128(path: &Path) -> Option<BitmapIcon> {
    let p_str = path.to_string_lossy();
    if p_str.contains("-symbolic") || p_str.contains("/symbolic/") {
        return None;
    }
    if let Some(ic) = load_icon_via_gdk_pixbuf(path, 128) {
        return Some(ic);
    }
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    {
        return BitmapIcon::from_png_file_max(path, 128);
    }
    None
}

pub fn resolve_desktop_icon(icon_name: &str, app_id: &str) -> Option<BitmapIcon> {
    let mut roots: Vec<PathBuf> = Vec::with_capacity(5);
    if let Ok(home) = std::env::var("HOME") {
        let hp = PathBuf::from(home);
        roots.push(hp.join(".local/share/icons"));
    }
    roots.push(PathBuf::from("/var/lib/flatpak/exports/share/icons"));
    roots.push(PathBuf::from("/usr/local/share/icons"));
    roots.push(PathBuf::from("/usr/share/icons"));

    const THEMES: &[&str] = &["hicolor", "Papirus", "breeze", "Adwaita", "gnome", "locolor"];
    const SUBDIRS: &[&str] = &["apps", "devices", "categories", "mimetypes", "places"];
    const SIZES_AND_EXTS: &[(&str, &str)] = &[
        ("scalable", "svg"),
        ("scalable", "svgz"),
        ("512x512", "png"),
        ("256x256@2", "png"),
        ("256x256", "png"),
        ("128x128@2", "png"),
        ("128x128", "png"),
        ("96x96", "png"),
        ("64x64", "png"),
        ("48x48", "png"),
        ("32x32", "png"),
    ];

    for raw in [icon_name.trim(), app_id.trim()] {
        if raw.is_empty() || raw.ends_with("-symbolic") {
            continue;
        }
        let direct = Path::new(raw);
        if (direct.is_absolute() || raw.contains('/')) && direct.is_file() {
            if let Some(ic) = load_icon_file_128(direct) {
                return Some(ic);
            }
        }
        let stem = direct
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|_| {
                direct.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    matches!(
                        e.to_ascii_lowercase().as_str(),
                        "png" | "svg" | "svgz" | "xpm" | "ico"
                    )
                })
            })
            .unwrap_or(raw);
        if stem.is_empty() || stem.ends_with("-symbolic") {
            continue;
        }

        for root in &roots {
            if !root.is_dir() {
                continue;
            }
            for theme in THEMES {
                let tdir = root.join(theme);
                if !tdir.is_dir() {
                    continue;
                }
                for &(sz, ext) in SIZES_AND_EXTS {
                    for sub in SUBDIRS {
                        let p = tdir.join(sz).join(sub).join(format!("{stem}.{ext}"));
                        if p.is_file() {
                            if let Some(ic) = load_icon_file_128(&p) {
                                return Some(ic);
                            }
                        }
                    }
                }
            }
        }

        for pix_dir in ["/usr/share/pixmaps", "/usr/local/share/pixmaps"] {
            for ext in ["svg", "png", "xpm"] {
                let p = Path::new(pix_dir).join(format!("{stem}.{ext}"));
                if p.is_file() {
                    if let Some(ic) = load_icon_file_128(&p) {
                        return Some(ic);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_peripherals_puts_official_pen_first_then_ascending_battery() {
        let mut sys = SysSnapshot::default();
        sys.bt_on = true;
        sys.pen_docked = true;
        sys.pen_pct = 98;
        sys.bt_peripherals = vec![
            BtPeripheral {
                name: "Sony WH-1000XM5".into(),
                mac: "AA:11".into(),
                kind: BtDevKind::Audio,
                battery_pct: Some(82),
                official_pen: false,
                paired: true,
                connected: true,
                charging: false,
            },
            BtPeripheral {
                name: "MX Anywhere 3S".into(),
                mac: "BB:22".into(),
                kind: BtDevKind::Mouse,
                battery_pct: Some(18),
                official_pen: false,
                paired: true,
                connected: true,
                charging: false,
            },
            BtPeripheral {
                name: "Keychron K3 Pro".into(),
                mac: "CC:33".into(),
                kind: BtDevKind::Keyboard,
                battery_pct: Some(64),
                official_pen: false,
                paired: true,
                connected: true,
                charging: false,
            },
            BtPeripheral {
                name: "Offline Pad".into(),
                mac: "DD:44".into(),
                kind: BtDevKind::Gamepad,
                battery_pct: Some(5),
                official_pen: false,
                paired: true,
                connected: false,
                charging: false,
            },
        ];

        let sorted = sys.sorted_topbar_peripherals();
        assert_eq!(sorted.len(), 4);
        assert!(sorted[0].official_pen);
        assert_eq!(sorted[0].battery_pct, Some(98));
        assert_eq!(sorted[1].name, "MX Anywhere 3S");
        assert_eq!(sorted[1].battery_pct, Some(18));
        assert_eq!(sorted[2].name, "Keychron K3 Pro");
        assert_eq!(sorted[2].battery_pct, Some(64));
        assert_eq!(sorted[3].name, "Sony WH-1000XM5");
        assert_eq!(sorted[3].battery_pct, Some(82));
    }

    #[test]
    fn default_prefs_keep_corners_and_pen_orb_disabled() {
        let p = ShellPrefs::default();
        assert!(!p.pen_corners);
        assert!(!p.pen_orb);
    }

    #[test]
    fn pactl_filter_matches_sink_and_ignores_sink_input_stream_churn() {
        assert!(is_relevant_pactl_event("Event 'change' on sink #52"));
        assert!(is_relevant_pactl_event("Event 'change' on source #53"));
        assert!(is_relevant_pactl_event("Event 'change' on server #0"));
        assert!(!is_relevant_pactl_event("Event 'change' on sink-input #91"));
        assert!(!is_relevant_pactl_event("Event 'new' on source-output #12"));
        assert!(!is_relevant_pactl_event("Event 'change' on client #44"));
    }

    #[test]
    fn wpctl_volume_parser_handles_normal_and_muted() {
        assert_eq!(parse_wpctl_volume_line("Volume: 0.40\n"), (Some(40), false));
        assert_eq!(
            parse_wpctl_volume_line("Volume: 0.62 [MUTED]\n"),
            (Some(62), true)
        );
    }

    #[test]
    fn shell_ctrl_socket_parser_handles_osd_and_actions() {
        assert!(matches!(
            parse_shell_ctrl_line("vol +5"),
            Some(ShellBusEvent::HardwareKeyOsd(HwOsdCmd::VolumeDelta(5)))
        ));
        assert!(matches!(
            parse_shell_ctrl_line("vol -5"),
            Some(ShellBusEvent::HardwareKeyOsd(HwOsdCmd::VolumeDelta(-5)))
        ));
        assert!(matches!(
            parse_shell_ctrl_line("mute"),
            Some(ShellBusEvent::HardwareKeyOsd(HwOsdCmd::ToggleMute))
        ));
        assert!(matches!(
            parse_shell_ctrl_line("mic-mute"),
            Some(ShellBusEvent::HardwareKeyOsd(HwOsdCmd::ToggleMicMute))
        ));
        assert!(matches!(
            parse_shell_ctrl_line("bri +6"),
            Some(ShellBusEvent::HardwareKeyOsd(HwOsdCmd::BrightnessDelta(6)))
        ));
        assert!(matches!(
            parse_shell_ctrl_line("standby"),
            Some(ShellBusEvent::StartStandby)
        ));
        assert!(matches!(
            parse_shell_ctrl_line("win-menu 42 512.5 58.0 1"),
            Some(ShellBusEvent::OpenWindowMenu {
                win_id: 42,
                is_floating: true,
                ..
            })
        ));
        assert!(matches!(
            parse_shell_ctrl_line("curtain-swipe-begin"),
            Some(ShellBusEvent::CurtainSwipeBegin)
        ));
        assert!(matches!(
            parse_shell_ctrl_line("curtain-swipe -18.5"),
            Some(ShellBusEvent::CurtainSwipeUpdate(v)) if (v + 18.5).abs() < 0.01
        ));
        assert!(matches!(
            parse_shell_ctrl_line("curtain-swipe-end"),
            Some(ShellBusEvent::CurtainSwipeEnd)
        ));
        assert!(matches!(
            parse_shell_ctrl_line("width-presets 25,50,75,100"),
            Some(ShellBusEvent::SetPref {
                ref key,
                ref value_json,
            }) if key == "win_width_presets" && value_json == "25,50,75,100"
        ));
        assert!(matches!(
            parse_shell_ctrl_line("set-pref night_light true"),
            Some(ShellBusEvent::SetPref {
                ref key,
                ref value_json,
            }) if key == "night_light" && value_json == "true"
        ));
    }

    #[test]
    fn shell_prefs_width_presets_normalize_and_set_pref_validates() {
        let mut p = ShellPrefs::default();
        assert_eq!(p.normalized_width_presets(), vec![33, 50, 67, 100]);
        assert!((ShellPrefs::preset_pct_to_proportion(33) - 33.3333).abs() < 1e-4);
        assert!((ShellPrefs::preset_pct_to_proportion(67) - 66.6667).abs() < 1e-4);
        assert!((ShellPrefs::preset_pct_to_proportion(25) - 25.0).abs() < 1e-4);

        assert!(p.apply_set_pref("win_width_presets", "[75, 25, 50, 100, 50]"));
        assert_eq!(p.win_width_presets, vec![25, 50, 75, 100]);

        assert!(p.apply_set_pref("win_width_presets", "10, 60, 120"));
        assert_eq!(p.win_width_presets, vec![20, 60, 100]);

        // Reject single-element or >5-element width preset lists
        assert!(!p.apply_set_pref("win_width_presets", "[50]"));
        assert!(!p.apply_set_pref("win_width_presets", "[20,30,40,50,60,70]"));
        assert!(!p.apply_set_pref("unknown_key", "true"));

        assert!(p.apply_set_pref("night_light", "true"));
        assert!(p.night_light);
        assert!(p.apply_set_pref("skin", "\"glass-light\""));
        assert_eq!(p.skin, Skin::GlassLight);
    }

    #[test]
    fn pen_battery_uses_qi_when_docked_and_ble_when_undocked_without_qi_residual() {
        let mut sys = SysSnapshot::default();
        sys.bt_on = true;
        sys.bt_peripherals = vec![BtPeripheral {
            name: "Lenovo Tab Pen Pro".into(),
            mac: "02:00:00:00:4B:9F".into(),
            kind: BtDevKind::Stylus,
            battery_pct: Some(68),
            official_pen: true,
            paired: true,
            connected: true,
            charging: false,
        }];

        // 1. Undocked + BLE online: must report real-time BLE 68%, even if a stale Qi `pen_pct=100` arrived
        sys.pen_docked = false;
        sys.pen_charging = true;
        sys.pen_pct = 100;
        let undocked_ble = sys.sorted_topbar_peripherals();
        assert_eq!(undocked_ble.len(), 1);
        assert_eq!(undocked_ble[0].name, "Lenovo Tab Pen Pro");
        assert_eq!(undocked_ble[0].battery_pct, Some(68));
        assert!(!undocked_ble[0].charging);

        // 2. Docked + Qi active: Qi reading overrides BLE while magnetically attached
        sys.pen_docked = true;
        sys.pen_charging = true;
        sys.pen_pct = 96;
        let docked_both = sys.sorted_topbar_peripherals();
        assert_eq!(docked_both.len(), 1);
        assert_eq!(docked_both[0].battery_pct, Some(96));
        assert!(docked_both[0].charging);

        // 3. Undocked + BLE asleep/disconnected: must NOT synthesize phantom `QI:PEN` from stale `pen_pct`
        sys.bt_peripherals[0].connected = false;
        sys.pen_docked = false;
        sys.pen_pct = 100;
        let undocked_asleep = sys.sorted_topbar_peripherals();
        assert!(undocked_asleep.is_empty());

        // 4. Docked + BLE asleep/disconnected: synthesizes `QI:PEN` reusing paired BLE alias
        sys.pen_docked = true;
        sys.pen_charging = false;
        sys.pen_pct = 100;
        let docked_qi_only = sys.sorted_topbar_peripherals();
        assert_eq!(docked_qi_only.len(), 1);
        assert_eq!(docked_qi_only[0].mac, "QI:PEN");
        assert_eq!(docked_qi_only[0].name, "Lenovo Tab Pen Pro");
        assert_eq!(docked_qi_only[0].battery_pct, Some(100));
    }

    #[test]
    fn bypass_charge_limit_switches_from_charging_to_bypass_at_90_even_with_charging_status() {
        // Actively charging at 78% toward 90% (`current_now = +3.2A`) -> `bat_charging = true`
        assert!(compute_bat_charging("Charging\n", true, 78, 90, Some(3_200_000)));
        // Reaches 90% (`status` still reports `"Charging"`, `current_now = 0`) -> `bat_charging = false` (`旁路供电中`)
        assert!(!compute_bat_charging("Charging\n", true, 90, 90, Some(0)));
        // In 85..89% recharge hysteresis band with 0 mA -> stays in bypass (`bat_charging = false`)
        assert!(!compute_bat_charging("Charging\n", true, 88, 90, Some(0)));
        // Recharging back up from 85% (`current_now = +2.5A`) -> `bat_charging = true`
        assert!(compute_bat_charging("Charging\n", true, 85, 90, Some(2_500_000)));
        // Normal 100% mode (`charge_limit = 100`) at 90% -> `bat_charging = true`
        assert!(compute_bat_charging("Charging\n", true, 90, 100, Some(2_500_000)));
        // Unplugged (`usb_online = false`) -> always `false`
        assert!(!compute_bat_charging("Full\n", false, 100, 100, Some(0)));
    }

    #[test]
    fn focus_dnd_schedule_and_accent_wallpaper_prefs_work() {
        // Overnight schedule: 23:00 -> 07:30
        assert!(is_hm_in_window(23 * 60 + 15, "23:00", "07:30"));
        assert!(is_hm_in_window(2 * 60, "23:00", "07:30"));
        assert!(!is_hm_in_window(8 * 60, "23:00", "07:30"));
        // Same-day schedule: 12:30 -> 14:00
        assert!(is_hm_in_window(13 * 60, "12:30", "14:00"));
        assert!(!is_hm_in_window(15 * 60, "12:30", "14:00"));

        let mut prefs = ShellPrefs::default();
        assert!(prefs.apply_set_pref("accent", "\"teal\""));
        assert_eq!(prefs.accent, AccentColor::Teal);
        assert!(prefs.apply_set_pref("wallpaper", "\"aurora\""));
        assert_eq!(prefs.wallpaper, "aurora");
        assert!(prefs.apply_set_pref("lock_notif_mode", "\"redact\""));
        assert_eq!(prefs.lock_notif_mode, "redact");

        assert!(prefs.record_seen_notif_app("Telegram"));
        assert!(!prefs.record_seen_notif_app("Telegram"));
        assert!(!prefs.should_suppress_banner(10 * 60, false, "Telegram"));
        // Fullscreen auto-DND suppresses banner
        assert!(prefs.should_suppress_banner(10 * 60, true, "Telegram"));
        // Per-app mute suppresses banner
        assert!(prefs.apply_set_pref("muted_notif_apps", "[\"Telegram\"]"));
        assert!(prefs.is_app_muted("Telegram"));
        assert!(prefs.should_suppress_banner(10 * 60, false, "Telegram"));
    }
}
