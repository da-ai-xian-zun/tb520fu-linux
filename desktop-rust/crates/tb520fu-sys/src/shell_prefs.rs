// SPDX-License-Identifier: MIT
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zbus::zvariant::Value;

pub const ACCENT_IDS: [&str; 6] = ["blue", "indigo", "teal", "amber", "rose", "emerald"];
pub const ACCENT_LABELS: [&str; 6] = [
    "经典钴蓝",
    "星辰靛紫",
    "青碧湖绿",
    "琥珀暖橙",
    "绯红珊瑚",
    "翡翠森绿",
];

fn default_true() -> bool {
    true
}

fn default_accent() -> String {
    "blue".into()
}

fn default_wallpaper() -> String {
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

fn default_presets() -> Vec<u8> {
    vec![33, 50, 67, 100]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellPrefsDto {
    #[serde(default)]
    pub skin: String,
    #[serde(default = "default_accent")]
    pub accent: String,
    #[serde(default = "default_wallpaper")]
    pub wallpaper: String,
    #[serde(default = "default_true")]
    pub auto_rotate: bool,
    #[serde(default)]
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
    #[serde(default)]
    pub pen_corners: bool,
    #[serde(default)]
    pub pen_orb: bool,
    #[serde(default = "default_presets")]
    pub win_width_presets: Vec<u8>,
}

impl Default for ShellPrefsDto {
    fn default() -> Self {
        Self {
            skin: "glass-dark".into(),
            accent: default_accent(),
            wallpaper: default_wallpaper(),
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
            win_width_presets: default_presets(),
        }
    }
}

impl ShellPrefsDto {
    pub fn is_light_skin(&self) -> bool {
        matches!(self.skin.as_str(), "glass-light" | "solid-light")
    }

    pub fn accent_idx(&self) -> usize {
        ACCENT_IDS
            .iter()
            .position(|&id| id == self.accent.trim())
            .unwrap_or(0)
    }

    pub fn presets_csv(&self) -> String {
        self.win_width_presets
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn is_app_muted(&self, app_name: &str) -> bool {
        let target = app_name.trim();
        !target.is_empty()
            && self
                .muted_notif_apps
                .iter()
                .any(|m| m.eq_ignore_ascii_case(target) || m == target)
    }
}

pub async fn get_shell_prefs(conn: &zbus::Connection) -> Option<ShellPrefsDto> {
    let reply = conn
        .call_method(
            Some("fyi.tb520fu.Shell"),
            "/Shell",
            Some("fyi.tb520fu.Shell"),
            "GetPrefs",
            &(),
        )
        .await
        .ok()?;
    let json_str: String = reply.body().deserialize().ok()?;
    serde_json::from_str(&json_str).ok()
}

pub async fn set_shell_pref(conn: &zbus::Connection, key: &str, value_json: &str) -> bool {
    let Ok(reply) = conn
        .call_method(
            Some("fyi.tb520fu.Shell"),
            "/Shell",
            Some("fyi.tb520fu.Shell"),
            "SetPref",
            &(key, value_json),
        )
        .await
    else {
        return false;
    };
    reply.body().deserialize::<bool>().unwrap_or(false)
}

pub async fn send_test_notification(conn: &zbus::Connection) -> bool {
    let hints: HashMap<&str, Value<'_>> = HashMap::new();
    let actions: Vec<&str> = Vec::new();
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "系统设置",
            0u32,
            "preferences-system",
            "通知与专注测试",
            "桌面顶部横幅、通知中心与锁屏通知隐私均已正常联通。",
            actions,
            hints,
            4000i32,
        ),
    )
    .await
    .is_ok()
}
