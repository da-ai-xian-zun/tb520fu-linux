// SPDX-License-Identifier: MIT
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const OSK_MODE_IDS: [&str; 3] = ["dock", "split", "float"];
pub const OSK_MODE_LABELS: [&str; 3] = ["整块停靠", "三段分裂", "自由悬浮"];

pub const OSK_SOUND_IDS: [&str; 3] = ["std", "soft", "mech"];
pub const OSK_SOUND_LABELS: [&str; 3] = ["清脆标准", "轻柔水滴", "机械微动"];

pub const IME_TRIGGER_LABELS: [&str; 3] = [
    "Ctrl + Space",
    "左 Shift",
    "Super + Space",
];

#[derive(Debug, Clone)]
pub struct OskImeState {
    pub auto_popup: bool,
    pub mode_idx: usize,
    pub sound_on: bool,
    pub sound_idx: usize,
    pub ime_trigger_idx: usize,
}

impl Default for OskImeState {
    fn default() -> Self {
        Self {
            auto_popup: true,
            mode_idx: 0,
            sound_on: true,
            sound_idx: 0,
            ime_trigger_idx: 0,
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/debian"))
}

fn osk_json_path() -> PathBuf {
    home_dir().join(".config/tb520fu/osk.json")
}

fn fcitx_cfg_path() -> PathBuf {
    home_dir().join(".config/fcitx5/config")
}

pub fn load_osk_ime_state() -> OskImeState {
    let mut st = OskImeState::default();
    if let Ok(raw) = fs::read_to_string(osk_json_path()) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(ap) = val.get("auto_popup").and_then(|v| v.as_bool()) {
                st.auto_popup = ap;
            }
            if let Some(m) = val.get("mode").and_then(|v| v.as_str()) {
                st.mode_idx = match m.to_ascii_lowercase().as_str() {
                    "split" => 1,
                    "float" => 2,
                    _ => 0,
                };
            }
            if let Some(on) = val.get("sound_on").and_then(|v| v.as_bool()) {
                st.sound_on = on;
            }
            if let Some(snd) = val.get("sound").and_then(|v| v.as_str()) {
                st.sound_idx = OSK_SOUND_IDS.iter().position(|&s| s == snd).unwrap_or(0);
            }
        }
    }
    if let Ok(fc_raw) = fs::read_to_string(fcitx_cfg_path()) {
        st.ime_trigger_idx = parse_fcitx_trigger_idx(&fc_raw);
    }
    st
}

pub fn merge_osk_json_field(existing_raw: &str, key: &str, val: serde_json::Value) -> String {
    let mut map = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(existing_raw)
        .unwrap_or_else(|_| {
            serde_json::json!({
                "skin": "glass-dark",
                "accent": "blue",
                "auto_popup": true,
                "mode": "dock",
                "hadj": 1.0,
                "vol": 5,
                "haptic_level": 3,
                "sound": "std",
                "sound_on": true,
                "bubble": true,
                "flick": true,
                "cursor_swipe": true,
                "tap_glow": true,
                "sym_lock": false,
                "float_x": 560,
                "float_y": 360,
                "pinned": [],
                "recent_emoji": []
            })
            .as_object()
            .cloned()
            .unwrap_or_default()
        });
    map.insert(key.to_string(), val);
    serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_else(|_| "{}".into())
}

pub async fn set_osk_field_and_reload(
    session_bus: Option<&zbus::Connection>,
    key: &str,
    val: serde_json::Value,
) -> bool {
    let path = osk_json_path();
    if update_osk_file(&path, key, val).is_err() {
        return false;
    }
    if let Some(conn) = session_bus {
        let _ = conn
            .call_method(
                Some("fyi.tb520fu.Osk"),
                "/Osk",
                Some("fyi.tb520fu.Osk"),
                "reload_prefs",
                &(),
            )
            .await;
    }
    true
}

fn update_osk_file(path: &Path, key: &str, val: serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let next = merge_osk_json_field(&existing, key, val);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, next).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub fn osk_mode_json_value(mode_idx: usize) -> serde_json::Value {
    match mode_idx {
        1 => serde_json::Value::String("split".into()),
        2 => serde_json::Value::String("float".into()),
        _ => serde_json::Value::String("dock".into()),
    }
}

pub fn parse_fcitx_trigger_idx(raw: &str) -> usize {
    let mut section = "";
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed;
            continue;
        }
        if section == "[Hotkey/TriggerKeys]" {
            if let Some((_, v)) = trimmed.split_once('=') {
                let vl = v.trim().to_ascii_lowercase();
                if vl.contains("super+space") {
                    return 2;
                }
                if vl == "shift_l" || vl == "shift" {
                    return 1;
                }
            }
        }
    }
    0
}

pub fn update_fcitx_trigger_ini(raw: &str, trigger_idx: usize) -> String {
    let primary_val = match trigger_idx {
        1 => "Shift_L",
        2 => "Super+space",
        _ => "Control+space",
    };

    let mut out = Vec::<String>::new();
    let mut found_trigger = false;
    let mut in_trigger = false;
    let mut wrote_zero = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_trigger && !wrote_zero {
                out.push(format!("0={primary_val}"));
                wrote_zero = true;
            }
            in_trigger = trimmed == "[Hotkey/TriggerKeys]";
            if in_trigger {
                found_trigger = true;
            }
            out.push(line.to_string());
            continue;
        }
        if in_trigger {
            if let Some((k, _)) = trimmed.split_once('=') {
                if k.trim() == "0" {
                    out.push(format!("0={primary_val}"));
                    wrote_zero = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }

    if !found_trigger {
        if !out.is_empty() && !out.last().unwrap().is_empty() {
            out.push(String::new());
        }
        out.push("[Hotkey/TriggerKeys]".into());
        out.push(format!("0={primary_val}"));
    } else if in_trigger && !wrote_zero {
        out.push(format!("0={primary_val}"));
    }

    let mut res = out.join("\n");
    if !res.ends_with('\n') {
        res.push('\n');
    }
    res
}

pub fn save_fcitx_trigger(trigger_idx: usize) -> Result<(), String> {
    let path = fcitx_cfg_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let updated = update_fcitx_trigger_ini(&existing, trigger_idx);
    let tmp = path.with_extension("config.tmp");
    fs::write(&tmp, updated).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    let _ = Command::new("fcitx5-remote").arg("-r").status();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osk_json_merge_preserves_pinned_and_fcitx_ini_roundtrips() {
        let initial = r#"{"skin":"glass-dark","mode":"Dock","pinned":["ssh debian@local"],"sound_on":true}"#;
        let merged = merge_osk_json_field(initial, "auto_popup", serde_json::Value::Bool(false));
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["auto_popup"], serde_json::Value::Bool(false));
        assert_eq!(v["pinned"][0], "ssh debian@local");

        let fcitx_ini = "[Hotkey]\nEnumerateWithTriggerKeys=True\n\n[Hotkey/TriggerKeys]\n0=Control+space\n";
        let next_ini = update_fcitx_trigger_ini(fcitx_ini, 2);
        assert_eq!(parse_fcitx_trigger_idx(&next_ini), 2);
        assert!(next_ini.contains("EnumerateWithTriggerKeys=True"));
    }
}
