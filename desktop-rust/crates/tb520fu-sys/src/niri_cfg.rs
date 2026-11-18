// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

const SETTINGS_JSON_NAME: &str = "niri-settings.json";
const FRAG_KDL_NAME: &str = "tb520fu-settings.kdl";

fn default_true() -> bool {
    true
}

fn default_hdr_peak() -> u16 {
    900
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NiriManagedPrefs {
    pub dsi_scale: f64,
    #[serde(default = "default_true")]
    pub dsi_hdr: bool,
    #[serde(default = "default_hdr_peak")]
    pub dsi_hdr_peak: u16,
    pub tp_tap: bool,
    pub tp_natural_scroll: bool,
    pub tp_speed_pct: u8,
    #[serde(default)]
    pub kb_caps_ctrl: bool,
    pub ext_name: Option<String>,
    pub ext_enabled: bool,
    pub ext_mode: Option<String>,
    /// `"left"` | `"above"` | `"right"`
    pub ext_pos: String,
}

impl Default for NiriManagedPrefs {
    fn default() -> Self {
        Self {
            dsi_scale: 2.0,
            dsi_hdr: true,
            dsi_hdr_peak: 900,
            tp_tap: true,
            tp_natural_scroll: true,
            tp_speed_pct: 55,
            kb_caps_ctrl: false,
            ext_name: None,
            ext_enabled: true,
            ext_mode: None,
            ext_pos: "right".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExternalOutputInfo {
    pub name: String,
    pub enabled: bool,
    pub current_mode: String,
    pub available_modes: Vec<String>,
    pub pos: String,
}

#[derive(Debug, Clone)]
pub struct NiriDisplaySnapshot {
    pub dsi_scale: f64,
    pub external: Option<ExternalOutputInfo>,
}

#[derive(Deserialize)]
struct RawMode {
    width: u32,
    height: u32,
    refresh_rate: u32,
}

#[derive(Deserialize)]
struct RawLogical {
    x: i32,
    y: i32,
    #[allow(dead_code)]
    width: u32,
    #[allow(dead_code)]
    height: u32,
    scale: f64,
}

#[derive(Deserialize)]
struct RawOutput {
    name: String,
    modes: Vec<RawMode>,
    current_mode: Option<usize>,
    logical: Option<RawLogical>,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/debian"))
}

fn state_json_path() -> PathBuf {
    home_dir().join(".config/tb520fu").join(SETTINGS_JSON_NAME)
}

fn niri_dir() -> PathBuf {
    home_dir().join(".config/niri")
}

pub fn load_managed_prefs() -> NiriManagedPrefs {
    fs::read_to_string(state_json_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn find_niri_socket() -> Option<String> {
    if let Ok(s) = std::env::var("NIRI_SOCKET") {
        if !s.is_empty() && Path::new(&s).exists() {
            return Some(s);
        }
    }
    let uid = unsafe { libc::getuid() };
    let run_dir = format!("/run/user/{uid}");
    let entries = fs::read_dir(run_dir).ok()?;
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("niri.") && name.ends_with(".sock") {
            return Some(e.path().to_string_lossy().to_string());
        }
    }
    None
}

fn niri_cmd() -> Command {
    let mut c = Command::new("niri");
    if let Some(sock) = find_niri_socket() {
        c.env("NIRI_SOCKET", sock);
    }
    c
}

pub fn focus_window_by_app_id(app_id: &str) {
    let Ok(out) = niri_cmd().args(["msg", "-j", "windows"]).output() else {
        return;
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return;
    };
    let Some(arr) = val.as_array() else { return };
    for w in arr {
        if w.get("app_id").and_then(|v| v.as_str()) == Some(app_id) {
            if let Some(id) = w.get("id").and_then(|v| v.as_u64()) {
                let _ = niri_cmd()
                    .args(["msg", "action", "focus-window", "--id", &id.to_string()])
                    .status();
                break;
            }
        }
    }
}

pub fn query_niri_outputs() -> NiriDisplaySnapshot {
    let Ok(out) = niri_cmd().args(["msg", "-j", "outputs"]).output() else {
        return NiriDisplaySnapshot {
            dsi_scale: 2.0,
            external: None,
        };
    };
    let Ok(map) = serde_json::from_slice::<HashMap<String, RawOutput>>(&out.stdout) else {
        return NiriDisplaySnapshot {
            dsi_scale: 2.0,
            external: None,
        };
    };

    let dsi_scale = map
        .get("DSI-1")
        .and_then(|o| o.logical.as_ref())
        .map(|l| l.scale)
        .unwrap_or(2.0);
    let dsi_x = map
        .get("DSI-1")
        .and_then(|o| o.logical.as_ref())
        .map(|l| l.x)
        .unwrap_or(0);

    let mut external = None;
    for (k, o) in &map {
        if k == "DSI-1" {
            continue;
        }
        let mut seen = Vec::new();
        for m in &o.modes {
            let hz = (m.refresh_rate as f64) / 1000.0;
            let tag = format!("{}x{}@{:.3}", m.width, m.height, hz);
            if !seen.contains(&tag) && seen.len() < 8 {
                seen.push(tag);
            }
        }
        let current_mode = o
            .current_mode
            .and_then(|idx| o.modes.get(idx))
            .map(|m| format!("{}x{}@{:.3}", m.width, m.height, (m.refresh_rate as f64) / 1000.0))
            .or_else(|| seen.first().cloned())
            .unwrap_or_else(|| "3840x2160@144.000".into());

        let pos = match &o.logical {
            Some(l) if l.y < 0 => "above".into(),
            Some(l) if l.x < dsi_x => "left".into(),
            _ => "right".into(),
        };

        external = Some(ExternalOutputInfo {
            name: o.name.clone(),
            enabled: o.logical.is_some(),
            current_mode,
            available_modes: seen,
            pos,
        });
        break;
    }

    NiriDisplaySnapshot {
        dsi_scale,
        external,
    }
}

pub fn render_fragment_kdl(p: &NiriManagedPrefs) -> String {
    render_fragment_kdl_inner(p, true)
}

fn render_fragment_kdl_inner(p: &NiriManagedPrefs, allow_hdr_node: bool) -> String {
    let dsi_logical_w = ((2944.0 / p.dsi_scale).round() as i32).max(640);
    let speed = ((p.tp_speed_pct as f64 / 50.0) - 1.0).clamp(-1.0, 1.0);

    let mut s = String::new();
    s.push_str("// Managed by tb520fu-settings — validated via `niri validate` before atomic replace.\n");
    s.push_str(&format!(
        "output \"DSI-1\" {{\n    scale {:.2}\n    position x=0 y=0\n",
        p.dsi_scale
    ));
    if allow_hdr_node && p.dsi_hdr {
        let peak = p.dsi_hdr_peak.clamp(650, 950);
        s.push_str(&format!(
            "    hdr {{\n        panel-peak-luminance {peak}\n    }}\n"
        ));
    }
    s.push_str("}\n\n");

    if let Some(ext) = p.ext_name.as_deref().filter(|n| !n.is_empty()) {
        let escaped = ext.replace('\\', "\\\\").replace('"', "\\\"");
        s.push_str(&format!("output \"{escaped}\" {{\n"));
        if !p.ext_enabled {
            s.push_str("    off\n");
        } else {
            if let Some(mode) = p.ext_mode.as_deref().filter(|m| !m.is_empty()) {
                s.push_str(&format!("    mode \"{mode}\"\n"));
            }
            match p.ext_pos.as_str() {
                "left" => s.push_str("    position x=-2560 y=0\n"),
                "above" => s.push_str("    position x=0 y=-1440\n"),
                _ => s.push_str(&format!("    position x={dsi_logical_w} y=0\n")),
            }
        }
        s.push_str("}\n\n");
    }

    s.push_str("input {\n");
    s.push_str("    keyboard {\n        xkb {\n");
    if p.kb_caps_ctrl {
        s.push_str("            options \"ctrl:nocaps\"\n");
    } else {
        s.push_str("            options \"\"\n");
    }
    s.push_str("        }\n        numlock\n    }\n");
    s.push_str("    touchpad {\n");
    if p.tp_tap {
        s.push_str("        tap\n");
    }
    if p.tp_natural_scroll {
        s.push_str("        natural-scroll\n");
    }
    s.push_str(&format!("        accel-speed {:.2}\n", speed));
    s.push_str("    }\n}\n");
    s
}

pub fn show_hotkey_overlay() -> bool {
    niri_cmd()
        .args(["msg", "action", "show-hotkey-overlay"])
        .status()
        .is_ok_and(|s| s.success())
}

/// Removes hardcoded `tap`/`natural-scroll` inside `touchpad {}` and `hdr {}` inside `output "DSI-1"`
/// once when adding `include "tb520fu-settings.kdl"`, so `tb520fu-settings.kdl` can cleanly toggle
/// touchpad flags and internal-panel HDR on or off!
fn migrate_main_config_once(main_kdl: &str) -> String {
    let mut out = Vec::new();
    let mut in_touchpad = false;
    let mut tp_depth = 0i32;
    let mut in_dsi = false;
    let mut dsi_depth = 0i32;
    let mut skip_hdr_depth = 0i32;

    for line in main_kdl.lines() {
        let trimmed = line.trim();
        let brace_delta = |s: &str| -> i32 {
            s.chars()
                .map(|ch| match ch {
                    '{' => 1,
                    '}' => -1,
                    _ => 0,
                })
                .sum()
        };

        if skip_hdr_depth > 0 {
            let d = brace_delta(trimmed);
            skip_hdr_depth += d;
            dsi_depth += d;
            if dsi_depth <= 0 {
                in_dsi = false;
            }
            continue;
        }

        if !in_dsi
            && trimmed.starts_with("output")
            && trimmed.contains("\"DSI-1\"")
            && trimmed.ends_with('{')
        {
            in_dsi = true;
            dsi_depth = brace_delta(trimmed);
            out.push(line.to_string());
            continue;
        }
        if in_dsi {
            if (trimmed == "hdr {" || trimmed.starts_with("hdr ")) && trimmed.ends_with('{') {
                let d = brace_delta(trimmed);
                skip_hdr_depth = d;
                dsi_depth += d;
                continue;
            }
            dsi_depth += brace_delta(trimmed);
            if dsi_depth <= 0 {
                in_dsi = false;
            }
            out.push(line.to_string());
            continue;
        }

        if !in_touchpad && trimmed.starts_with("touchpad") && trimmed.ends_with('{') {
            in_touchpad = true;
            tp_depth = 1;
            out.push(line.to_string());
            continue;
        }
        if in_touchpad {
            tp_depth += brace_delta(trimmed);
            if tp_depth <= 0 {
                in_touchpad = false;
                out.push(line.to_string());
                continue;
            }
            if trimmed == "tap" || trimmed == "natural-scroll" {
                continue;
            }
        }
        out.push(line.to_string());
    }
    let mut res = out.join("\n");
    if !res.contains(FRAG_KDL_NAME) {
        res.push_str(&format!("\ninclude \"{FRAG_KDL_NAME}\"\n"));
    } else {
        res.push('\n');
    }
    res
}

pub fn save_and_apply_niri_prefs(p: &NiriManagedPrefs) -> Result<(), String> {
    let dir = niri_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let frag_path = dir.join(FRAG_KDL_NAME);
    let tmp_frag = dir.join(".tb520fu-settings.kdl.tmp");
    let tmp_validate = dir.join(".tb520fu-validate.kdl.tmp");

    let mut kdl_text = render_fragment_kdl_inner(p, true);
    fs::write(&tmp_frag, &kdl_text).map_err(|e| e.to_string())?;

    // Validate fragment syntax with `niri validate` before touching live config!
    let val_doc = format!("include \"{}\"\n", tmp_frag.display());
    fs::write(&tmp_validate, &val_doc).map_err(|e| e.to_string())?;
    let mut check = Command::new("niri")
        .args(["validate", "-c", &tmp_validate.to_string_lossy()])
        .output()
        .map_err(|e| format!("无法执行 niri validate: {e}"))?;
    if !check.status.success() && p.dsi_hdr {
        // Graceful fallback if running on a pre-HDR niri binary without `hdr` node support
        kdl_text = render_fragment_kdl_inner(p, false);
        fs::write(&tmp_frag, &kdl_text).map_err(|e| e.to_string())?;
        check = Command::new("niri")
            .args(["validate", "-c", &tmp_validate.to_string_lossy()])
            .output()
            .map_err(|e| format!("无法执行 niri validate: {e}"))?;
    }
    let _ = fs::remove_file(&tmp_validate);
    if !check.status.success() {
        let _ = fs::remove_file(&tmp_frag);
        return Err(format!(
            "niri validate 未通过: {}",
            String::from_utf8_lossy(&check.stderr).trim()
        ));
    }

    // Ensure main config.kdl includes `tb520fu-settings.kdl` (and migrate presence-only blocks once)
    let main_path = dir.join("config.kdl");
    if let Ok(main_raw) = fs::read_to_string(&main_path) {
        if !main_raw.contains(FRAG_KDL_NAME) || main_raw.contains("panel-peak-luminance") {
            let migrated = migrate_main_config_once(&main_raw);
            let tmp_main = dir.join(".config.kdl.settings-tmp");
            fs::write(&tmp_main, &migrated).map_err(|e| e.to_string())?;
            fs::rename(&tmp_main, &main_path).map_err(|e| e.to_string())?;
        }
    }

    fs::rename(&tmp_frag, &frag_path).map_err(|e| e.to_string())?;

    let state_p = state_json_path();
    if let Some(parent) = state_p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(js) = serde_json::to_string_pretty(p) {
        let _ = fs::write(state_p, js);
    }

    // Also apply live scale immediately via Niri IPC for instant visual feedback
    let _ = niri_cmd()
        .args([
            "msg",
            "output",
            "DSI-1",
            "scale",
            &format!("{:.2}", p.dsi_scale),
        ])
        .status();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_clean_kdl_and_migrates_touchpad_flags_once() {
        let mut p = NiriManagedPrefs::default();
        p.dsi_scale = 2.0;
        p.tp_tap = false;
        p.tp_natural_scroll = true;
        p.tp_speed_pct = 60;
        p.kb_caps_ctrl = true;
        let kdl = render_fragment_kdl(&p);
        assert!(kdl.contains("scale 2.00"));
        assert!(!kdl.contains("\n        tap\n"));
        assert!(kdl.contains("natural-scroll"));
        assert!(kdl.contains("accel-speed 0.20"));
        assert!(kdl.contains("options \"ctrl:nocaps\""));

        let sample_main = "output \"DSI-1\" {\n    scale 2.0\n    hdr {\n        panel-peak-luminance 650\n    }\n}\ninput {\n    touchpad {\n        tap\n        dwt\n        natural-scroll\n    }\n}\n";
        let migrated = migrate_main_config_once(sample_main);
        assert!(!migrated.contains("\n        tap\n"));
        assert!(!migrated.contains("natural-scroll"));
        assert!(!migrated.contains("panel-peak-luminance"));
        assert!(migrated.contains("dwt"));
        assert!(migrated.contains("include \"tb520fu-settings.kdl\""));
        assert!(kdl.contains("panel-peak-luminance 900"));

        p.dsi_hdr = false;
        let kdl_off = render_fragment_kdl(&p);
        assert!(!kdl_off.contains("panel-peak-luminance"));
    }
}
