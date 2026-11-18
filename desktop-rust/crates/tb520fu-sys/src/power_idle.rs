// SPDX-License-Identifier: MIT
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};
use zbus::zvariant::{OwnedValue, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleConfig {
    pub lock_secs: i32,
    pub suspend_secs: i32,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            lock_secs: 300,
            suspend_secs: 0,
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/debian"))
}

fn idle_conf_path() -> PathBuf {
    home_dir().join(".config/tb520fu/idle.conf")
}

fn idle_user_unit_path() -> PathBuf {
    home_dir().join(".config/systemd/user/tb520fu-idle.service")
}

pub fn load_idle_config() -> IdleConfig {
    fs::read_to_string(idle_conf_path())
        .ok()
        .and_then(|s| serde_json::from_str::<IdleConfig>(&s).ok())
        .unwrap_or_default()
}

pub fn build_swayidle_exec(cfg: &IdleConfig) -> String {
    let mut parts = vec!["/usr/bin/swayidle".to_string(), "-w".to_string()];
    if cfg.lock_secs > 0 {
        parts.push("timeout".into());
        parts.push(cfg.lock_secs.to_string());
        parts.push("'loginctl lock-session'".into());
    }
    if cfg.suspend_secs > 0 {
        let total = if cfg.lock_secs > 0 {
            cfg.lock_secs + cfg.suspend_secs
        } else {
            cfg.suspend_secs
        };
        parts.push("timeout".into());
        parts.push(total.to_string());
        parts.push("'systemctl suspend'".into());
    }
    parts.push("before-sleep".into());
    parts.push("'loginctl lock-session'".into());
    parts.join(" ")
}

pub fn apply_idle_config(cfg: &IdleConfig) -> Result<(), String> {
    let conf_p = idle_conf_path();
    if let Some(parent) = conf_p.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let js = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(&conf_p, js).map_err(|e| e.to_string())?;

    let unit_p = idle_user_unit_path();
    if let Some(parent) = unit_p.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let exec_line = build_swayidle_exec(cfg);
    let unit_text = format!(
        "[Unit]\n\
         Description=TB520FU Idle Lock & Auto-Suspend Manager (configured by tb520fu-settings)\n\
         PartOf=graphical-session.target\n\
         After=graphical-session.target tb520fu-shell.service\n\
         ConditionUser=!@system\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec_line}\n\
         Restart=always\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target niri.service\n"
    );
    fs::write(&unit_p, unit_text).map_err(|e| e.to_string())?;

    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let _ = Command::new("systemctl")
        .args(["--user", "restart", "tb520fu-idle.service"])
        .status();
    Ok(())
}

pub async fn get_power_profile(sys_bus: &zbus::Connection) -> String {
    sys_bus
        .call_method(
            Some("net.hadess.PowerProfiles"),
            "/net/hadess/PowerProfiles",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("net.hadess.PowerProfiles", "ActiveProfile"),
        )
        .await
        .ok()
        .and_then(|r| r.body().deserialize::<OwnedValue>().ok())
        .and_then(|v| String::try_from(v).ok())
        .unwrap_or_else(|| "balanced".into())
}

pub async fn set_power_profile(sys_bus: &zbus::Connection, profile: &str) -> bool {
    sys_bus
        .call_method(
            Some("net.hadess.PowerProfiles"),
            "/net/hadess/PowerProfiles",
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &(
                "net.hadess.PowerProfiles",
                "ActiveProfile",
                Value::from(profile),
            ),
        )
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_swayidle_exec_for_lock_and_optional_suspend() {
        let c1 = IdleConfig {
            lock_secs: 300,
            suspend_secs: 0,
        };
        assert_eq!(
            build_swayidle_exec(&c1),
            "/usr/bin/swayidle -w timeout 300 'loginctl lock-session' before-sleep 'loginctl lock-session'"
        );
        let c2 = IdleConfig {
            lock_secs: 300,
            suspend_secs: 600,
        };
        assert_eq!(
            build_swayidle_exec(&c2),
            "/usr/bin/swayidle -w timeout 300 'loginctl lock-session' timeout 900 'systemctl suspend' before-sleep 'loginctl lock-session'"
        );
    }
}
