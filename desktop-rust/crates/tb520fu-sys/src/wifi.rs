// SPDX-License-Identifier: MIT
use std::collections::{HashMap, HashSet};
use std::process::Command;

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::daemon;

#[derive(Debug, Clone)]
pub struct WifiAp {
    pub ssid: String,
    pub strength: u8,
    pub secure: bool,
    pub security_label: String,
    pub in_use: bool,
    pub saved: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WifiConnectionDetail {
    pub ssid: String,
    pub security: String,
    pub ip4: String,
    pub gateway: String,
    pub mac: String,
}

#[derive(Debug, Clone, Default)]
pub struct WifiState {
    pub enabled: bool,
    pub active: Option<WifiConnectionDetail>,
    pub wired_summary: Option<String>,
    pub networks: Vec<WifiAp>,
}

fn read_active_wired_summary() -> Option<String> {
    let out = Command::new("nmcli")
        .args(["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"])
        .output()
        .ok()?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.splitn(4, ':').collect();
        if parts.len() < 4 {
            continue;
        }
        let dev = parts[0].trim();
        let ty = parts[1].trim();
        let st = parts[2].trim();
        let conn = parts[3].trim();
        if ty == "ethernet" && st == "connected" && !dev.is_empty() {
            let ip_out = Command::new("nmcli")
                .args(["-t", "-f", "IP4.ADDRESS", "device", "show", dev])
                .output()
                .ok();
            let ip = ip_out
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .find_map(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
                })
                .map(|v| v.split('/').next().unwrap_or(&v).to_string())
                .unwrap_or_default();
            let kind = if dev.starts_with("usb") || conn.to_ascii_lowercase().contains("usb") {
                "USB 共享网络"
            } else {
                "有线拓展坞以太网"
            };
            return if ip.is_empty() {
                Some(format!("{kind} ({dev}) 已连接"))
            } else {
                Some(format!("{kind} ({dev}) · IP {ip}"))
            };
        }
    }
    None
}

fn find_wifi_iface() -> Option<String> {
    let out = Command::new("nmcli")
        .args(["-t", "-f", "DEVICE,TYPE", "device"])
        .output()
        .ok()?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((dev, ty)) = line.split_once(':') else {
            continue;
        };
        if ty.trim() == "wifi" && !dev.trim().is_empty() {
            return Some(dev.trim().to_string());
        }
    }
    None
}

fn read_active_wifi_detail(iface: &str) -> Option<WifiConnectionDetail> {
    let out = Command::new("nmcli")
        .args([
            "-t",
            "-f",
            "GENERAL.STATE,GENERAL.CONNECTION,GENERAL.HWADDR,IP4.ADDRESS,IP4.GATEWAY",
            "device",
            "show",
            iface,
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut connected = false;
    let mut ssid = String::new();
    let mut mac = String::new();
    let mut ip4 = String::new();
    let mut gw = String::new();

    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let val = v.trim();
        if k == "GENERAL.STATE" && val.starts_with("100") {
            connected = true;
        } else if k == "GENERAL.CONNECTION" && val != "--" {
            ssid = val.to_string();
        } else if k == "GENERAL.HWADDR" {
            mac = val.replace("\\:", ":");
        } else if k.starts_with("IP4.ADDRESS") && ip4.is_empty() {
            ip4 = val.split('/').next().unwrap_or(val).to_string();
        } else if k == "IP4.GATEWAY" && val != "--" {
            gw = val.to_string();
        }
    }
    if !connected || ssid.is_empty() {
        return None;
    }
    Some(WifiConnectionDetail {
        ssid,
        security: "WPA2/WPA3".into(),
        ip4,
        gateway: gw,
        mac,
    })
}

async fn fetch_saved_ssids(conn: &zbus::Connection) -> HashSet<String> {
    let mut saved = HashSet::new();
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
        return saved;
    };
    let Ok(paths) = reply.body().deserialize::<Vec<OwnedObjectPath>>() else {
        return saved;
    };
    for p in paths {
        let Ok(r) = conn
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
        type SettingsMap = HashMap<String, HashMap<String, OwnedValue>>;
        let Ok(map) = r.body().deserialize::<SettingsMap>() else {
            continue;
        };
        if let Some(w) = map.get("802-11-wireless") {
            if let Some(ssid_val) = w.get("ssid") {
                if let Ok(bytes) = Vec::<u8>::try_from(ssid_val.clone()) {
                    let s = String::from_utf8_lossy(&bytes).trim().to_string();
                    if !s.is_empty() {
                        saved.insert(s);
                    }
                }
            }
        }
        if let Some(c) = map.get("connection") {
            let is_wifi = c
                .get("type")
                .and_then(|v| <&str>::try_from(v).ok())
                .is_some_and(|t| t == "802-11-wireless");
            if is_wifi {
                if let Some(id) = c.get("id").and_then(|v| <&str>::try_from(v).ok()) {
                    let s = id.trim();
                    if !s.is_empty() {
                        saved.insert(s.to_string());
                    }
                }
            }
        }
    }
    saved
}

pub async fn get_wifi_state(sys_bus: &zbus::Connection) -> WifiState {
    let enabled = sys_bus
        .call_method(
            Some("org.freedesktop.NetworkManager"),
            "/org/freedesktop/NetworkManager",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.NetworkManager", "WirelessEnabled"),
        )
        .await
        .ok()
        .and_then(|r| r.body().deserialize::<OwnedValue>().ok())
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(true);

    let wired_summary = read_active_wired_summary();

    if !enabled {
        return WifiState {
            enabled: false,
            active: None,
            wired_summary,
            networks: Vec::new(),
        };
    }

    let saved = fetch_saved_ssids(sys_bus).await;
    let iface = find_wifi_iface();
    let mut active = iface.as_deref().and_then(read_active_wifi_detail);

    // Query NM wireless access points via `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY dev wifi list --rescan no`
    // (`--rescan no` returns cached kernel/NM scan results in <15ms without blocking).
    let mut by_ssid: HashMap<String, WifiAp> = HashMap::new();
    if let Ok(out) = Command::new("nmcli")
        .args([
            "-t",
            "-f",
            "IN-USE,SSID,SIGNAL,SECURITY",
            "device",
            "wifi",
            "list",
            "--rescan",
            "no",
        ])
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            // Unescape `:` in SSID safely from the right: last two fields are SIGNAL and SECURITY.
            let mut rparts = line.rsplitn(3, ':');
            let Some(sec_raw) = rparts.next() else {
                continue;
            };
            let Some(sig_raw) = rparts.next() else {
                continue;
            };
            let Some(prefix) = rparts.next() else {
                continue;
            };
            let Some((in_use_raw, ssid_escaped)) = prefix.split_once(':') else {
                continue;
            };
            let ssid = ssid_escaped.replace("\\:", ":").trim().to_string();
            if ssid.is_empty() {
                continue;
            }
            let in_use = in_use_raw.trim() == "*";
            let strength = sig_raw.trim().parse::<u8>().unwrap_or(0).min(100);
            let sec_trim = sec_raw.trim();
            let secure = !sec_trim.is_empty() && sec_trim != "--";
            let security_label = if !secure {
                "开放".into()
            } else {
                sec_trim.replace(' ', " / ")
            };
            let is_saved = in_use || saved.contains(&ssid);
            if in_use {
                if let Some(act) = active.as_mut() {
                    act.security = security_label.clone();
                }
            }
            let cand = WifiAp {
                ssid: ssid.clone(),
                strength,
                secure,
                security_label,
                in_use,
                saved: is_saved,
            };
            match by_ssid.get_mut(&ssid) {
                Some(existing) => {
                    if cand.in_use || (!existing.in_use && cand.strength > existing.strength) {
                        *existing = cand;
                    }
                }
                None => {
                    by_ssid.insert(ssid, cand);
                }
            }
        }
    }

    let mut networks: Vec<WifiAp> = by_ssid
        .into_values()
        .filter(|ap| !ap.in_use)
        .collect();
    networks.sort_by(|a, b| {
        b.saved
            .cmp(&a.saved)
            .then_with(|| b.strength.cmp(&a.strength))
            .then_with(|| a.ssid.cmp(&b.ssid))
    });

    WifiState {
        enabled,
        active,
        wired_summary,
        networks,
    }
}

pub async fn set_airplane_mode(sys_bus: &zbus::Connection, airplane: bool) {
    let radio_on = !airplane;
    set_wifi_enabled(radio_on);
    crate::bluetooth::set_bt_enabled(sys_bus, radio_on).await;
}

pub fn set_wifi_enabled(enabled: bool) {
    let _ = Command::new("nmcli")
        .args(["radio", "wifi", if enabled { "on" } else { "off" }])
        .status();
}

pub fn trigger_wifi_scan() {
    if daemon::wifi_rescan().is_err() {
        let _ = Command::new("nmcli")
            .args(["device", "wifi", "rescan"])
            .status();
    }
}

pub fn connect_wifi(ssid: &str, password: Option<&str>) {
    let trimmed = ssid.trim();
    if trimmed.is_empty() {
        return;
    }
    let clean_pw = password.map(str::trim).filter(|s| !s.is_empty());
    if daemon::wifi_connect(trimmed, clean_pw).is_err() {
        if let Some(pw) = clean_pw {
            let _ = Command::new("nmcli")
                .args(["device", "wifi", "connect", trimmed, "password", pw])
                .status();
        } else {
            let ok = Command::new("nmcli")
                .args(["connection", "up", trimmed])
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                let _ = Command::new("nmcli")
                    .args(["device", "wifi", "connect", trimmed])
                    .status();
            }
        }
    }
}

pub async fn forget_wifi(sys_bus: &zbus::Connection, ssid: &str) {
    let target = ssid.trim();
    if target.is_empty() {
        return;
    }
    let mut deleted_via_dbus = false;
    if let Ok(reply) = sys_bus
        .call_method(
            Some("org.freedesktop.NetworkManager"),
            "/org/freedesktop/NetworkManager/Settings",
            Some("org.freedesktop.NetworkManager.Settings"),
            "ListConnections",
            &(),
        )
        .await
    {
        if let Ok(paths) = reply.body().deserialize::<Vec<OwnedObjectPath>>() {
            for p in paths {
                let Ok(r) = sys_bus
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
                type SettingsMap = HashMap<String, HashMap<String, OwnedValue>>;
                let Ok(map) = r.body().deserialize::<SettingsMap>() else {
                    continue;
                };
                let mut matches = false;
                if let Some(c) = map.get("connection") {
                    if c.get("id")
                        .and_then(|v| <&str>::try_from(v).ok())
                        .is_some_and(|id| id == target)
                    {
                        matches = true;
                    }
                }
                if let Some(w) = map.get("802-11-wireless") {
                    if let Some(ssid_val) = w.get("ssid") {
                        if let Ok(bytes) = Vec::<u8>::try_from(ssid_val.clone()) {
                            if String::from_utf8_lossy(&bytes).trim() == target {
                                matches = true;
                            }
                        }
                    }
                }
                if matches {
                    if sys_bus
                        .call_method(
                            Some("org.freedesktop.NetworkManager"),
                            p.as_str(),
                            Some("org.freedesktop.NetworkManager.Settings.Connection"),
                            "Delete",
                            &(),
                        )
                        .await
                        .is_ok()
                    {
                        deleted_via_dbus = true;
                    }
                }
            }
        }
    }
    if !deleted_via_dbus {
        let _ = Command::new("nmcli")
            .args(["connection", "delete", target])
            .status();
    }
}
