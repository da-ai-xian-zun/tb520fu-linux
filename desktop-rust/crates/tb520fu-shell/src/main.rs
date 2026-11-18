// SPDX-License-Identifier: MIT
pub mod app_menu;
pub mod curtain;
pub mod lockscreen;
pub mod niri_ipc;
pub mod notifications;
pub mod pam_greetd;
pub mod statusbar;
pub mod stylus_tools;
pub mod sys_state;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tb520fu_osk::layouts::KbMode;
use tb520fu_osk::render::OskRenderer;
use tb520fu_osk::state::{OskState, OutgoingAction, PanelKind};
use tb520fu_ui::canvas::{Canvas, RectF};
use tb520fu_ui::clipboard::spawn_native_clipboard;
use tb520fu_ui::color::{Palette, Rgba, Skin};
use tb520fu_ui::haptics::{HapticEngine, HapticKind};
use tb520fu_ui::svg_icons::IconSet;
use tb520fu_ui::text::TextEngine;
use tb520fu_ui::wayland::{KbFocus, LayerConfig, LayerKind, SurfaceId, UiEvent, WaylandApp};

use crate::curtain::{Curtain, CurtainEffect};
use crate::lockscreen::Lockscreen;
use crate::niri_ipc::{
    apply_portrait_policy, send_niri_request, spawn_niri_listener, ExternalOutput, NiriSnapshot,
    WindowInfo, WorkspaceInfo,
};
use crate::notifications::{BannerAction, BannerOsdView, HwOsdCmd, NotifItem, ShellBusEvent};
use crate::statusbar::{BarAction, PullWing, StatusBar, BAR_HEIGHT};
use crate::stylus_tools::{CompanionTools, InkStroke, StrokePt, ToolEffect};
use crate::sys_state::{
    scan_desktop_apps, send_async_bus_cmd, send_shell_ctrl_sock, set_brightness_pct,
    set_volume_pct, spawn_shell_async_runtime, sync_display_ctm_and_hz, toggle_mic_mute,
    toggle_mute, AsyncBusCmd, BtDevKind, BtPeripheral, ShellPrefs, SysSnapshot, WifiNetwork,
};

static SIGUSR1_LOCK: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigusr1(_sig: libc::c_int) {
    SIGUSR1_LOCK.store(true, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineOskTarget {
    None,
    AppSearch,
    WifiPassword,
    QuickNote,
}

impl InlineOskTarget {
    #[inline]
    fn on_curtain(self) -> bool {
        matches!(self, Self::AppSearch | Self::WifiPassword)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--greeter") {
        if let Err(e) = run_greeter(&args) {
            eprintln!("tb520fu-shell --greeter fatal: {e}");
            std::process::exit(1);
        }
        return;
    }

    if let Some(cmd) = args.first().map(|s| s.as_str()) {
        match cmd {
            "vol" => {
                let raw_delta = args
                    .get(1)
                    .and_then(|s| s.parse::<i8>().ok())
                    .unwrap_or(5);
                if !send_shell_ctrl_sock(&format!("vol {raw_delta}")) {
                    let step = if raw_delta >= 0 {
                        format!("{}%+", raw_delta.unsigned_abs())
                    } else {
                        format!("{}%-", raw_delta.unsigned_abs())
                    };
                    let _ = std::process::Command::new("wpctl")
                        .args(["set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", &step])
                        .status();
                }
                return;
            }
            "mute" => {
                if !send_shell_ctrl_sock("mute") {
                    let _ = std::process::Command::new("wpctl")
                        .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
                        .status();
                }
                return;
            }
            "mic-mute" => {
                if !send_shell_ctrl_sock("mic-mute") {
                    let _ = std::process::Command::new("wpctl")
                        .args(["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"])
                        .status();
                }
                return;
            }
            "bri" => {
                let raw_delta = args
                    .get(1)
                    .and_then(|s| s.parse::<i8>().ok())
                    .unwrap_or(6);
                if !send_shell_ctrl_sock(&format!("bri {raw_delta}"))
                    && !crate::sys_state::adjust_brightness_via_daemon(raw_delta)
                {
                    let step = if raw_delta >= 0 {
                        format!("{}%+", raw_delta.unsigned_abs())
                    } else {
                        format!("{}%-", raw_delta.unsigned_abs())
                    };
                    let _ = std::process::Command::new("brightnessctl")
                        .args(["set", &step])
                        .status();
                }
                return;
            }
            "lock" => {
                if !send_shell_ctrl_sock("lock") && call_shell_dbus("Lock").is_err() {
                    let _ = std::process::Command::new("loginctl")
                        .arg("lock-session")
                        .status();
                    let _ = std::process::Command::new("pkill")
                        .args(["-USR1", "-x", "tb520fu-shell"])
                        .status();
                }
                return;
            }
            "unlock" => {
                if !send_shell_ctrl_sock("unlock") {
                    let _ = call_shell_dbus("Unlock");
                }
                return;
            }
            "standby" => {
                if !send_shell_ctrl_sock("standby") {
                    let _ = call_shell_dbus("StartStandby");
                }
                return;
            }
            "control-center" | "console" => {
                if !send_shell_ctrl_sock("console") {
                    let _ = call_shell_dbus("ToggleControlCenter");
                }
                return;
            }
            "notifications" => {
                if !send_shell_ctrl_sock("notifications") {
                    let _ = call_shell_dbus("ToggleNotifications");
                }
                return;
            }
            "annot" | "screenshot-annot" => {
                if !send_shell_ctrl_sock("annot") {
                    let _ = call_shell_dbus("StartAnnot");
                }
                return;
            }
            "quicknote" | "note" => {
                if !send_shell_ctrl_sock("quicknote") {
                    let _ = call_shell_dbus("ToggleQuickNote");
                }
                return;
            }
            "get-prefs" => {
                match call_shell_get_prefs() {
                    Ok(js) => println!("{js}"),
                    Err(_) => {
                        let p = ShellPrefs::load();
                        println!("{}", serde_json::to_string_pretty(&p).unwrap_or_default());
                    }
                }
                return;
            }
            "set-pref" => {
                let Some(key) = args.get(1) else {
                    eprintln!("usage: tb520fu-shell set-pref <key> <value_json>");
                    std::process::exit(2);
                };
                let val = args.iter().skip(2).cloned().collect::<Vec<_>>().join(" ");
                if val.is_empty() {
                    eprintln!("usage: tb520fu-shell set-pref <key> <value_json>");
                    std::process::exit(2);
                }
                match call_shell_set_pref(key, &val) {
                    Ok(true) => {}
                    Ok(false) => {
                        eprintln!("invalid pref key or value: {key} = {val}");
                        std::process::exit(1);
                    }
                    Err(_) => {
                        if !send_shell_ctrl_sock(&format!("set-pref {key} {val}")) {
                            let mut p = ShellPrefs::load();
                            if !p.apply_set_pref(key, &val) {
                                eprintln!("invalid pref key or value: {key} = {val}");
                                std::process::exit(1);
                            }
                            p.save();
                        }
                    }
                }
                return;
            }
            "width-presets" => {
                let val = args.iter().skip(1).cloned().collect::<Vec<_>>().join(",");
                if val.is_empty() {
                    let p = ShellPrefs::load();
                    let list: Vec<String> = p
                        .normalized_width_presets()
                        .into_iter()
                        .map(|n| format!("{n}%"))
                        .collect();
                    println!("{}", list.join(" "));
                    return;
                }
                match call_shell_set_pref("win_width_presets", &val) {
                    Ok(true) => {}
                    Ok(false) => {
                        eprintln!("invalid width presets (expected 2..=5 numbers in 20..=100): {val}");
                        std::process::exit(1);
                    }
                    Err(_) => {
                        if !send_shell_ctrl_sock(&format!("width-presets {val}")) {
                            let mut p = ShellPrefs::load();
                            if !p.apply_set_pref("win_width_presets", &val) {
                                eprintln!("invalid width presets (expected 2..=5 numbers in 20..=100): {val}");
                                std::process::exit(1);
                            }
                            p.save();
                        }
                    }
                }
                return;
            }
            _ => {}
        }
    }

    if let Some(pos) = args.iter().position(|a| a == "--render-png") {
        let Some(out_path) = args.get(pos + 1) else {
            eprintln!(
                "usage: tb520fu-shell --render-png <out.png> [--scene desktop-topbar-bt|console-right|console-left-bt|console-wifi-light|island-quicknote|annot|lockscreen|standby]"
            );
            std::process::exit(2);
        };
        if let Err(e) = run_headless_png(out_path, &args) {
            eprintln!("render-png failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(e) = run_shell_daemon() {
        eprintln!("tb520fu-shell fatal: {e}");
        std::process::exit(1);
    }
}

fn call_shell_dbus(method: &str) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let conn = zbus::Connection::session().await.map_err(|e| e.to_string())?;
        conn.call_method(
            Some("fyi.tb520fu.Shell"),
            "/Shell",
            Some("fyi.tb520fu.Shell"),
            method,
            &(),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    })
}

fn call_shell_get_prefs() -> Result<String, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let conn = zbus::Connection::session().await.map_err(|e| e.to_string())?;
        let msg = conn
            .call_method(
                Some("fyi.tb520fu.Shell"),
                "/Shell",
                Some("fyi.tb520fu.Shell"),
                "GetPrefs",
                &(),
            )
            .await
            .map_err(|e| e.to_string())?;
        msg.body().deserialize::<String>().map_err(|e| e.to_string())
    })
}

fn call_shell_set_pref(key: &str, value_json: &str) -> Result<bool, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let conn = zbus::Connection::session().await.map_err(|e| e.to_string())?;
        let msg = conn
            .call_method(
                Some("fyi.tb520fu.Shell"),
                "/Shell",
                Some("fyi.tb520fu.Shell"),
                "SetPref",
                &(key, value_json),
            )
            .await
            .map_err(|e| e.to_string())?;
        msg.body().deserialize::<bool>().map_err(|e| e.to_string())
    })
}

fn call_osk_dbus_async(method: &'static str) {
    if send_async_bus_cmd(AsyncBusCmd::OskMethod(method)) {
        return;
    }
    let _ = call_shell_or_osk_fallback(method);
}

fn call_shell_or_osk_fallback(method: &'static str) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let conn = zbus::Connection::session().await.map_err(|e| e.to_string())?;
        conn.call_method(
            Some("fyi.tb520fu.Osk"),
            "/Osk",
            Some("fyi.tb520fu.Osk"),
            method,
            &(),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    })
}

/// Compute screen-space origin `(ox, oy)` of the embedded in-process Y700 OSK on a full-screen `sw x sh` surface.
pub(crate) fn inline_osk_origin(osk: &OskState, sw: f32, sh: f32) -> (f32, f32) {
    let surf_w = osk.metrics.surf_w as f32;
    let surf_h = osk.metrics.surf_h as f32;
    if osk.prefs.mode == KbMode::Float {
        (
            (osk.prefs.float_x as f32).clamp(0.0, (sw - surf_w).max(0.0)),
            (osk.prefs.float_y as f32).clamp(0.0, (sh - surf_h).max(0.0)),
        )
    } else {
        ((sw - surf_w) * 0.5, sh - surf_h)
    }
}

pub(crate) fn composite_subcanvas(
    dst: &mut Canvas,
    sub: &Canvas,
    logical_dx: f32,
    logical_dy: f32,
) {
    let pdx = logical_dx * dst.scale;
    let pdy = logical_dy * dst.scale;
    for mut inst in sub.instances.iter().cloned() {
        // MODE_CAPSULE_LINE stores `[x0, y0, x1, y1]` in `rect`; all other modes store `[x, y, w, h]`.
        if (inst.params[0] - tb520fu_ui::canvas::MODE_CAPSULE_LINE).abs() < 0.1 {
            inst.rect[0] += pdx;
            inst.rect[1] += pdy;
            inst.rect[2] += pdx;
            inst.rect[3] += pdy;
        } else {
            inst.rect[0] += pdx;
            inst.rect[1] += pdy;
        }
        if inst.clip_rect[2] > 0.0 && inst.clip_rect[3] > 0.0 {
            inst.clip_rect[0] += pdx;
            inst.clip_rect[1] += pdy;
        }
        if (inst.params[0] - tb520fu_ui::canvas::MODE_RADIAL_GLOW).abs() < 0.1 {
            inst.uv_rect[0] += pdx;
            inst.uv_rect[1] += pdy;
        }
        dst.instances.push(inst);
    }
    for b in &sub.blur_rects {
        let mut moved = *b;
        moved.rect.x += logical_dx;
        moved.rect.y += logical_dy;
        dst.blur_rects.push(moved);
    }
}

fn run_headless_png(out_path: &str, args: &[String]) -> Result<(), String> {
    // no compositor here: glass samples the scene's own pre-blurred wallpaper
    tb520fu_ui::canvas::set_own_backdrop_default(true);
    let sw = 1472u32;
    let sh = 920u32;
    let scale = 2.0_f32;
    let pw = ((sw as f32) * scale).round() as u32;
    let ph = ((sh as f32) * scale).round() as u32;

    let mut scene = "console-right";
    let mut skin = Skin::GlassDark;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--scene" if i + 1 < args.len() => {
                scene = args[i + 1].as_str();
                i += 1;
            }
            "--skin" if i + 1 < args.len() => {
                skin = Skin::from_name(&args[i + 1]);
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    if scene == "console-wifi-light" {
        skin = Skin::GlassLight;
    }

    let pal = Palette::for_skin(skin);
    let icons = IconSet::new();
    let mut text = TextEngine::new();
    let mut prefs = ShellPrefs::default();
    prefs.skin = skin;
    prefs.night_light = false;

    let mut sys = SysSnapshot {
        time_hm: "14:36".into(),
        date_short: "11月14日 周六".into(),
        date_long: "11月14日 星期六".into(),
        bat_pct: 84,
        bat_charging: true,
        usb_online: true,
        bat_watts: 22.5,
        bat_temp_c: 31.4,
        pen_docked: true,
        pen_charging: false,
        pen_pct: 98,
        pen_hover: false,
        pen_action_seq: 0,
        pen_action_code: 0,
        brightness_pct: 68,
        autobright: true,
        lux: 245.0,
        torch: false,
        charge_limit: 90,
        volume_pct: 62,
        muted: false,
        mic_muted: false,
        wifi_on: true,
        wifi_ssid: "YogaPad-5G".into(),
        wifi_signal: 92,
        wifi_list: vec![
            WifiNetwork {
                ssid: "YogaPad-5G".into(),
                signal: 92,
                active: true,
                secured: true,
                saved: true,
            },
            WifiNetwork {
                ssid: "HomeLab-WiFi7".into(),
                signal: 81,
                active: false,
                secured: true,
                saved: true,
            },
            WifiNetwork {
                ssid: "TB520FU-Hotspot".into(),
                signal: 66,
                active: false,
                secured: true,
                saved: false,
            },
        ],
        bt_on: true,
        bt_device: "Sony WH-1000XM5".into(),
        bt_peripherals: vec![
            BtPeripheral {
                name: "Sony WH-1000XM5".into(),
                mac: "AA:11:22:33:44:55".into(),
                kind: BtDevKind::Audio,
                battery_pct: Some(82),
                official_pen: false,
                connected: true,
                paired: true,
                charging: false,
            },
            BtPeripheral {
                name: "MX Anywhere 3S".into(),
                mac: "BB:11:22:33:44:55".into(),
                kind: BtDevKind::Mouse,
                battery_pct: Some(18),
                official_pen: false,
                connected: true,
                paired: true,
                charging: false,
            },
            BtPeripheral {
                name: "Keychron K3 Pro".into(),
                mac: "CC:11:22:33:44:55".into(),
                kind: BtDevKind::Keyboard,
                battery_pct: Some(64),
                official_pen: false,
                connected: true,
                paired: true,
                charging: false,
            },
        ],
        power_profile: "balanced".into(),
        media_playing: true,
        media_title: "Clair de Lune".into(),
        media_artist: "Claude Debussy".into(),
    };

    if scene == "console-empty" {
        sys.brightness_pct = 10;
    }

    // Pure-blood Niri "左一右二" ("left full-height 1, right stacked 2") + occupied WS 2 + trailing empty WS 3 `○`!
    let mut niri = NiriSnapshot {
        connected: true,
        workspaces: vec![
            WorkspaceInfo {
                id: 1,
                idx: 1,
                output: "DSI-1".into(),
                is_active: true,
                is_focused: true,
            },
            WorkspaceInfo {
                id: 2,
                idx: 2,
                output: "DSI-1".into(),
                is_active: false,
                is_focused: false,
            },
        ],
        windows: vec![
            WindowInfo {
                id: 10,
                title: "nvim — desktop-rust".into(),
                app_id: "foot".into(),
                workspace_id: Some(1),
                is_focused: true,
                col_index: Some(1),
                row_index: Some(1),
                tile_width: 718.0,
                tile_height: 850.0,
                ..Default::default()
            },
            WindowInfo {
                id: 11,
                title: "Firefox".into(),
                app_id: "firefox".into(),
                workspace_id: Some(1),
                is_focused: false,
                col_index: Some(2),
                row_index: Some(1),
                tile_width: 718.0,
                tile_height: 430.0,
                ..Default::default()
            },
            WindowInfo {
                id: 12,
                title: "cargo test".into(),
                app_id: "foot".into(),
                workspace_id: Some(1),
                is_focused: false,
                col_index: Some(2),
                row_index: Some(2),
                tile_width: 718.0,
                tile_height: 408.0,
                ..Default::default()
            },
            WindowInfo {
                id: 20,
                title: "Rnote".into(),
                app_id: "rnote".into(),
                workspace_id: Some(2),
                is_focused: false,
                col_index: Some(1),
                row_index: Some(1),
                tile_width: 1200.0,
                tile_height: 850.0,
                ..Default::default()
            },
        ],
        overview: false,
        external_dp: Some(ExternalOutput {
            name: "DP-1".into(),
            enabled: true,
        }),
    };

    let mut notifs = vec![
        NotifItem {
            id: 1,
            app_name: "手写笔".into(),
            summary: "Tab Pen Pro 已充满".into(),
            body: "电量 98%".into(),
            time_hm: "14:20".into(),
            created_at: Instant::now(),
        },
        NotifItem {
            id: 2,
            app_name: "蓝牙".into(),
            summary: "MX Anywhere 3S 电量偏低".into(),
            body: "剩余电量 18%".into(),
            time_hm: "14:32".into(),
            created_at: Instant::now(),
        },
    ];
    if matches!(scene, "empty-home" | "console-empty") {
        notifs.clear();
    }
    if scene == "empty-home" {
        niri.windows.clear();
        niri.workspaces.truncate(1);
    }

    let mut desktop = Canvas::new(pw, ph, scale);
    desktop.set_screen_context(0.0, 0.0, sw as f32, sh as f32);

    if scene == "lockscreen-glance" {
        let mut lock = Lockscreen::new_session_lock(None);
        lock.enter_lock(false);
        lock.render(&mut desktop, &icons, &mut text, &pal, &sys, &notifs);
        return desktop.save_png(out_path);
    }
    if scene == "lockscreen" {
        let mut lock = Lockscreen::new_session_lock(None);
        lock.enter_lock(false);
        lock.open_auth_stage(true);
        lock.auth_reveal.snap(1.0);
        lock.osk.slide_y.snap(0.0);
        lock.pw_buf = "debian".into();
        lock.render(&mut desktop, &icons, &mut text, &pal, &sys, &notifs);
        return desktop.save_png(out_path);
    }
    if scene == "standby" {
        let mut lock = Lockscreen::new_session_lock(None);
        lock.enter_lock(true);
        lock.render(&mut desktop, &icons, &mut text, &pal, &sys, &notifs);
        return desktop.save_png(out_path);
    }
    if scene == "standby-frame" {
        let mut lock = Lockscreen::new_session_lock(None);
        lock.enter_lock(true);
        lock.set_standby_page(1);
        lock.page_spring.snap(1.0);
        lock.render(&mut desktop, &icons, &mut text, &pal, &sys, &notifs);
        return desktop.save_png(out_path);
    }
    if scene == "annot" {
        let mut tools = CompanionTools::new();
        tools.annot_open = true;
        tools.annot_strokes.push(InkStroke {
            color: Rgba::hex(0xff453a),
            base_width: 5.5,
            pts: vec![
                StrokePt { x: 380.0, y: 310.0, pressure: 0.7 },
                StrokePt { x: 520.0, y: 265.0, pressure: 0.9 },
                StrokePt { x: 690.0, y: 295.0, pressure: 0.85 },
                StrokePt { x: 840.0, y: 250.0, pressure: 0.6 },
            ],
        });
        tools.render(&mut desktop, &icons, &mut text, &pal, &sys, &niri, &prefs);
        return desktop.save_png(out_path);
    }

    // Render TB520FU stock wallpaper + clean borderless Niri "左一右二" tiled windows (8px radius)
    desktop.clear(Rgba::hex(0x0b1120));
    desktop.draw_wallpaper_background(RectF::new(0.0, 0.0, sw as f32, sh as f32), 1.0);

    if scene != "empty-home" {
        let wy = (BAR_HEIGHT as f32) + 12.0;
        let wh = (sh as f32) - wy - 12.0;
        let left_w = (sw as f32) * 0.5 - 18.0;
        let left_win = RectF::new(12.0, wy, left_w, wh);
        desktop.draw_soft_shadow(left_win, 8.0, 14.0, 5.0, 0.34);
        desktop.fill_round_rect(left_win, 8.0, Rgba::rgba_f(14, 19, 30, 0.88));
        desktop.stroke_round_rect(left_win, 8.0, 2.0, pal.acc.with_alpha(0.85));

        let rx = 12.0 + left_w + 12.0;
        let rw = (sw as f32) - rx - 12.0;
        let rh_top = wh * 0.52 - 6.0;
        let top_right = RectF::new(rx, wy, rw, rh_top);
        let bot_right = RectF::new(rx, wy + rh_top + 12.0, rw, wh - rh_top - 12.0);
        for wr in [top_right, bot_right] {
            desktop.draw_soft_shadow(wr, 8.0, 14.0, 5.0, 0.30);
            desktop.fill_round_rect(wr, 8.0, Rgba::rgba_f(18, 24, 38, 0.84));
            desktop.stroke_round_rect(wr, 8.0, 1.0, Rgba::rgba_f(255, 255, 255, 0.12));
        }
    }

    // TopBar (`46px`)
    let bar_ph = ((BAR_HEIGHT as f32) * scale).round() as u32;
    let mut bar_canvas = Canvas::new(pw, bar_ph, scale);
    bar_canvas.set_screen_context(0.0, 0.0, sw as f32, sh as f32);
    let mut bar = StatusBar::new();
    bar.sync_target(&niri);
    bar.glass_reveal.snap(bar.glass_reveal.target);
    if scene == "topbar-osd" {
        bar.show_volume_osd(68, false);
        bar.osd_reveal.snap(1.0);
        bar.osd_val.snap(0.68);
    } else if scene == "topbar-osd-bri" {
        bar.show_brightness_osd(76);
        bar.osd_reveal.snap(1.0);
        bar.osd_val.snap(0.76);
    }
    bar.step(0.35, true, &niri);
    bar.render(
        &mut bar_canvas,
        &icons,
        &mut text,
        &pal,
        &sys,
        &niri,
        &prefs,
        notifs.len(),
    );
    composite_subcanvas(&mut desktop, &bar_canvas, 0.0, 0.0);

    if scene == "notif-banner" {
        let mut banner = BannerOsdView::default();
        if let Some(first) = notifs.first().cloned() {
            banner.push_banner(first);
            banner.reveal.snap(1.0);
        }
        let bw = 460u32;
        let bh = 104u32;
        let ox = ((sw - bw) as f32) * 0.5;
        let oy = 52.0_f32;
        let mut b_canvas = Canvas::new(
            ((bw as f32) * scale).round() as u32,
            ((bh as f32) * scale).round() as u32,
            scale,
        );
        b_canvas.set_screen_context(ox, oy, sw as f32, sh as f32);
        let _ = banner.render(&mut b_canvas, &icons, &mut text, &pal);
        composite_subcanvas(&mut desktop, &b_canvas, ox, oy);
    }

    // Companion Tools (Window Pill Menu + Media Popup / QuickNote)
    let mut tools = CompanionTools::new();
    if scene == "island-quicknote" {
        tools.island_open = true;
        tools.note_open = true;
        tools.note_focused = true;
        tools.note_text = "阅读摘录\n- Debussy: Suite bergamasque\n- Wayland fractional-scale-v1".into();
    }
    let mut tools_canvas = Canvas::new(pw, ph, scale);
    tools_canvas.set_screen_context(0.0, 0.0, sw as f32, sh as f32);
    tools.render(
        &mut tools_canvas,
        &icons,
        &mut text,
        &pal,
        &sys,
        &niri,
        &prefs,
    );
    composite_subcanvas(&mut desktop, &tools_canvas, 0.0, 0.0);

    if matches!(
        scene,
        "console-right" | "console-empty" | "console-left-bt" | "console-wifi-light" | "controls"
    ) {
        let mut curtain = Curtain::new(scan_desktop_apps());
        curtain.open = true;
        curtain.progress.snap(1.0);
        if scene == "console-left-bt" {
            curtain.wing = PullWing::Left;
            curtain.bt_open = true;
            curtain.bt_spring.snap(1.0);
            curtain.app_menu.search_focused = true;
            curtain.app_menu.query = "f".into();
        } else if scene == "console-wifi-light" {
            curtain.wing = PullWing::Right;
            curtain.wifi_open = true;
            curtain.wifi_spring.snap(1.0);
        } else {
            curtain.wing = PullWing::Right;
        }

        let mut osk_inset = 0.0_f32;
        let mut osk_opt: Option<(OskState, OskRenderer)> = None;
        if scene == "console-left-bt" {
            let mut osk = OskState::new(sw, sh);
            osk.prefs.skin = skin;
            osk.pal = pal;
            osk.prefs.mode = KbMode::Split;
            osk.enter_label = Some("打开");
            osk.rebuild_geometry();
            osk.shown = true;
            osk.slide_y.snap(0.0);
            osk_inset = osk.metrics.kb_h + 12.0;
            osk_opt = Some((osk, OskRenderer::new()));
        }

        let mut cc_canvas = Canvas::new(pw, ph, scale);
        cc_canvas.set_screen_context(0.0, 0.0, sw as f32, sh as f32);
        curtain.render(
            &mut cc_canvas,
            &icons,
            &mut text,
            &pal,
            &sys,
            &niri,
            &prefs,
            &notifs,
            osk_inset,
        );
        composite_subcanvas(&mut desktop, &cc_canvas, 0.0, 0.0);

        if let Some((mut osk, mut osk_ren)) = osk_opt {
            let (ox, oy) = inline_osk_origin(&osk, sw as f32, sh as f32);
            let mut osk_canvas = Canvas::new(
                ((osk.metrics.surf_w as f32) * scale).round() as u32,
                ((osk.metrics.surf_h as f32) * scale).round() as u32,
                scale,
            );
            osk_canvas.set_screen_context(ox, oy, sw as f32, sh as f32);
            osk_ren.render(&mut osk, &mut osk_canvas);
            composite_subcanvas(&mut desktop, &osk_canvas, ox, oy);
        }
    }

    desktop.save_png(out_path)
}

// ---------------- Graphical `greetd` Greeter Mode (`--greeter`) ----------------

fn run_greeter(args: &[String]) -> Result<(), String> {
    let mut username = "debian".to_string();
    let mut session_cmd = vec!["niri-session".to_string()];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--user" if i + 1 < args.len() => {
                username = args[i + 1].clone();
                i += 1;
            }
            "--cmd" if i + 1 < args.len() => {
                session_cmd = args[i + 1]
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    let mut wl = WaylandApp::connect()?;
    let wakeup_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    let wakeup_opt = if wakeup_fd >= 0 { Some(wakeup_fd) } else { None };

    let auto_rotate_flag = Arc::new(AtomicBool::new(true));
    let sys_shared = Arc::new(Mutex::new(SysSnapshot::default()));
    let greeter_bus_rx =
        spawn_shell_async_runtime(sys_shared.clone(), auto_rotate_flag, wakeup_opt, false);

    let dsi_out = wl
        .outputs()
        .iter()
        .find(|o| o.name == "DSI-1")
        .map(|o| o.output.clone());

    let surf_id = wl.create_layer_surface(
        "tb520fu-greeter",
        LayerConfig {
            layer: LayerKind::Overlay,
            anchor_top: true,
            anchor_bottom: true,
            anchor_left: true,
            anchor_right: true,
            width: 0,
            height: 0,
            exclusive_zone: -1,
            margin_top: 0,
            margin_right: 0,
            margin_bottom: 0,
            margin_left: 0,
            kb_focus: KbFocus::Exclusive,
        },
        dsi_out.as_ref(),
    )?;

    let pal = Palette::for_skin(Skin::GlassDark);
    let icons = IconSet::new();
    let mut text = TextEngine::new();
    let mut greeter = Lockscreen::new_greeter(username, session_cmd, wakeup_opt);
    greeter.enter_lock(false);

    let mut canvas = Canvas::new(64, 64, 2.0);
    canvas.own_backdrop = true; // the greeter paints its own wallpaper
    let mut touch_coords: HashMap<i32, (f32, f32)> = HashMap::new();
    let mut sys_local = sys_shared.lock().unwrap().clone();
    let mut dirty = true;
    let mut last_tick = Instant::now();

    loop {
        while let Ok(bev) = greeter_bus_rx.try_recv() {
            match bev {
                ShellBusEvent::SystemResumed => {
                    greeter.on_system_resumed();
                    dirty = true;
                }
                ShellBusEvent::TouchpadLockWake => {
                    if greeter.phase == crate::lockscreen::LockPhase::StandbyCompanion
                        || greeter.auth_reveal.target < 0.5
                    {
                        greeter.open_auth_stage(false);
                        dirty = true;
                    }
                }
                _ => {}
            }
        }
        {
            let latest = sys_shared.lock().unwrap().clone();
            if latest != sys_local {
                sys_local = latest;
                dirty = true;
            }
        }

        let events = wl.poll_events()?;
        for ev in events {
            match ev {
                UiEvent::Configured { surface, .. } if surface == surf_id => {
                    dirty = true;
                }
                UiEvent::TouchDown {
                    surface,
                    slot,
                    x,
                    y,
                    ..
                } if surface == surf_id => {
                    touch_coords.insert(slot, (x, y));
                    if greeter.on_touch_down(slot, x, y) {
                        dirty = true;
                    }
                }
                UiEvent::TouchMotion {
                    surface,
                    slot,
                    x,
                    y,
                    ..
                } if surface == surf_id => {
                    touch_coords.insert(slot, (x, y));
                    if greeter.on_touch_motion(slot, x, y) {
                        dirty = true;
                    }
                }
                UiEvent::TouchUp { surface, slot, .. } if surface == surf_id => {
                    let (x, y) = touch_coords.remove(&slot).unwrap_or((0.0, 0.0));
                    if greeter.on_touch_up(slot, x, y) {
                        dirty = true;
                    }
                }
                UiEvent::TouchCancel { surface } if surface == surf_id => {
                    touch_coords.clear();
                    greeter.on_touch_cancel();
                    dirty = true;
                }
                UiEvent::Scroll {
                    surface, dx, dy, ..
                } if surface == surf_id => {
                    if greeter.on_scroll(dx, dy) {
                        dirty = true;
                    }
                }
                UiEvent::ScrollStop { surface } if surface == surf_id => {
                    if greeter.on_scroll_stop() {
                        dirty = true;
                    }
                }
                UiEvent::PointerActivity { surface } if surface == surf_id => {
                    greeter.on_pointer_activity();
                }
                UiEvent::PointerSecondaryClick { surface, .. } if surface == surf_id => {
                    if greeter.on_secondary_click() {
                        dirty = true;
                    }
                }
                UiEvent::KeyPress {
                    evdev_code,
                    pressed,
                } => {
                    if greeter.on_evdev_key_event(evdev_code, pressed) {
                        dirty = true;
                    }
                }
                _ => {}
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(last_tick).as_secs_f32().min(0.05);
        if dt >= 0.001 {
            last_tick = now;
            let (rd, logged_in) = greeter.tick(dt, &sys_local, true);
            dirty |= rd;
            if logged_in {
                let _ = send_niri_request("{\"Action\":{\"Quit\":{\"skip_confirmation\":true}}}");
                return Ok(());
            }
        }

        if dirty && wl.frame_ready(surf_id) {
            if let Some((lw, lh, sc)) = wl.surface_geometry(surf_id) {
                canvas.resize(
                    ((lw as f32) * sc).round() as u32,
                    ((lh as f32) * sc).round() as u32,
                    sc,
                );
                canvas.set_screen_context(0.0, 0.0, lw as f32, lh as f32);
                greeter.render(&mut canvas, &icons, &mut text, &pal, &sys_local, &[]);
                if wl.present(surf_id, &canvas, greeter.is_animating()) {
                    dirty = false;
                }
            }
        }

        let timeout_ms = if greeter.is_animating() {
            7
        } else if greeter.verifying || greeter.error_msg.is_some() {
            100
        } else {
            1000
        };

        let mut pfds = [
            libc::pollfd {
                fd: wl.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wakeup_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let nfds = if wakeup_fd >= 0 { 2 } else { 1 };
        unsafe {
            let timeout_ms = if wl.has_queued_events() { 0 } else { timeout_ms };
            let _ = libc::poll(pfds.as_mut_ptr(), nfds as libc::nfds_t, timeout_ms);
            if wakeup_fd >= 0 && (pfds[1].revents & libc::POLLIN) != 0 {
                let mut sink = 0u64;
                let _ = libc::read(wakeup_fd, &mut sink as *mut u64 as *mut libc::c_void, 8);
            }
        }
    }
}

// ---------------- Desktop Session Shell Daemon ----------------

fn run_shell_daemon() -> Result<(), String> {
    unsafe {
        libc::signal(libc::SIGUSR1, on_sigusr1 as *const () as libc::sighandler_t);
    }

    let mut wl = WaylandApp::connect()?;
    let wakeup_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    let wakeup_opt = if wakeup_fd >= 0 { Some(wakeup_fd) } else { None };

    let mut prefs = ShellPrefs::load();
    tb520fu_ui::vk_renderer::set_active_wallpaper_spec(&prefs.wallpaper);
    let mut pal = Palette::for_skin_and_accent(prefs.skin, prefs.accent);
    let auto_rotate_flag = Arc::new(AtomicBool::new(prefs.auto_rotate));

    let niri_shared = Arc::new(Mutex::new(NiriSnapshot::default()));
    spawn_niri_listener(niri_shared.clone(), wakeup_opt);

    let sys_shared = Arc::new(Mutex::new(SysSnapshot::default()));
    let bus_rx =
        spawn_shell_async_runtime(sys_shared.clone(), auto_rotate_flag.clone(), wakeup_opt, true);
    let (clip_item_tx, clip_rx) = std::sync::mpsc::channel::<String>();
    let _clip_tx = spawn_native_clipboard(clip_item_tx, wakeup_opt);

    let dsi_out = wl
        .outputs()
        .iter()
        .find(|o| o.name == "DSI-1")
        .map(|o| o.output.clone());

    // 1. Thicker 46px Liquid Glass Top Status Bar (`Layer::Top`, `exclusive_zone = 46`)
    let bar_id = wl.create_layer_surface(
        "tb520fu-bar",
        LayerConfig {
            layer: LayerKind::Top,
            anchor_top: true,
            anchor_bottom: false,
            anchor_left: true,
            anchor_right: true,
            width: 0,
            height: BAR_HEIGHT,
            exclusive_zone: BAR_HEIGHT as i32,
            margin_top: 0,
            margin_right: 0,
            margin_bottom: 0,
            margin_left: 0,
            kb_focus: KbFocus::None,
        },
        dsi_out.as_ref(),
    )?;

    // 2. Companion Tools & Interactive Niri Column/Stack Gutter Layer (`Layer::Top`, `wl_region` click-through!)
    let tools_id = wl.create_layer_surface(
        "tb520fu-tools",
        LayerConfig {
            layer: LayerKind::Top,
            anchor_top: true,
            anchor_bottom: true,
            anchor_left: true,
            anchor_right: true,
            width: 0,
            height: 0,
            exclusive_zone: -1,
            margin_top: 0,
            margin_right: 0,
            margin_bottom: 0,
            margin_left: 0,
            kb_focus: KbFocus::OnDemand,
        },
        dsi_out.as_ref(),
    )?;
    wl.set_input_region(tools_id, Some(&[]));
    wl.set_keep_swapchain(tools_id, true);

    // 3. Full-Screen 3-Column Liquid Glass Pull-Down Console (`Layer::Overlay`)
    let curtain_id = wl.create_layer_surface(
        "tb520fu-curtain",
        LayerConfig {
            layer: LayerKind::Overlay,
            anchor_top: true,
            anchor_bottom: true,
            anchor_left: true,
            anchor_right: true,
            width: 0,
            height: 0,
            exclusive_zone: -1,
            margin_top: 0,
            margin_right: 0,
            margin_bottom: 0,
            margin_left: 0,
            kb_focus: KbFocus::OnDemand,
        },
        dsi_out.as_ref(),
    )?;
    wl.set_input_region(curtain_id, Some(&[]));
    wl.set_keep_swapchain(curtain_id, true);

    // 4. Floating Incoming Notification Banner (`Layer::Overlay`, anchored below TopBar at `margin_top = 52`)
    let osd_id = wl.create_layer_surface(
        "tb520fu-osd",
        LayerConfig {
            layer: LayerKind::Overlay,
            anchor_top: true,
            anchor_bottom: false,
            anchor_left: false,
            anchor_right: false,
            width: 460,
            height: 104,
            exclusive_zone: -1,
            margin_top: 52,
            margin_right: 0,
            margin_bottom: 0,
            margin_left: 0,
            kb_focus: KbFocus::None,
        },
        dsi_out.as_ref(),
    )?;
    wl.set_input_region(osd_id, Some(&[]));
    wl.set_keep_swapchain(osd_id, true);

    // 5. Desktop Wallpaper Surface (`Layer::Background`, created after id 1..4 so perf IDs stay unchanged)
    let wall_id = wl.create_layer_surface(
        "tb520fu-wallpaper",
        LayerConfig {
            layer: LayerKind::Background,
            anchor_top: true,
            anchor_bottom: true,
            anchor_left: true,
            anchor_right: true,
            width: 0,
            height: 0,
            exclusive_zone: -1,
            margin_top: 0,
            margin_right: 0,
            margin_bottom: 0,
            margin_left: 0,
            kb_focus: KbFocus::None,
        },
        dsi_out.as_ref(),
    )?;
    wl.set_input_region(wall_id, Some(&[]));
    wl.set_prefer_shm(wall_id, true);

    let icons = IconSet::new();
    let mut text = TextEngine::new();
    let mut bar = StatusBar::new();
    let mut curtain = Curtain::new(scan_desktop_apps());
    let mut tools = CompanionTools::new();
    let mut osd_banner = BannerOsdView::default();
    let mut lockscreen = Lockscreen::new_session_lock(wakeup_opt);
    let mut lock_surfaces: Vec<SurfaceId> = Vec::new();

    // Embedded in-process Y700 OSK for 0ms Chinese/English typing into Console App Search & Quick Note!
    let mut inline_osk = OskState::new(1472, 920);
    inline_osk.direct_text_sink = true;
    inline_osk.fcitx_online = false;
    inline_osk.prefs.skin = prefs.skin;
    inline_osk.pal = pal;
    let mut inline_osk_ren = OskRenderer::new();
    let mut osk_target = InlineOskTarget::None;

    let mut touch_coords: HashMap<(SurfaceId, i32), (f32, f32)> = HashMap::new();
    let mut osk_captured_slots: HashMap<(SurfaceId, i32), (f32, f32)> = HashMap::new();
    let mut notifs: Vec<NotifItem> = Vec::new();

    let mut bar_canvas = Canvas::new(64, 64, 2.0);
    let mut tools_canvas = Canvas::new(64, 64, 2.0);
    let mut curtain_canvas = Canvas::new(64, 64, 2.0);
    let mut osd_canvas = Canvas::new(64, 64, 2.0);
    let mut lock_canvas = Canvas::new(64, 64, 2.0);
    lock_canvas.own_backdrop = true; // paints its own wallpaper; nothing for the compositor to blur
    let mut osk_sub_canvas = Canvas::new(64, 64, 2.0);

    let mut sys_local = sys_shared.lock().unwrap().clone();
    let mut niri_local = niri_shared.lock().unwrap().clone();
    let mut prev_vol = sys_local.volume_pct;
    let mut prev_muted = sys_local.muted;
    let mut prev_mic_muted = sys_local.mic_muted;
    let mut prev_bright = sys_local.brightness_pct;
    let mut last_hw_osd_at: Option<Instant> = None;
    let mut prev_pen_hover = sys_local.pen_hover;
    let mut prev_pen_seq = sys_local.pen_action_seq;
    let mut was_portrait: Option<bool> = None;

    sync_display_ctm_and_hz(&prefs, sys_local.pen_hover);

    let mut screen_logical = (1472.0_f32, 920.0_f32);
    let mut dirty_bar = true;
    let mut dirty_tools = true;
    let mut dirty_curtain = false;
    let mut dirty_osd = false;
    let mut dirty_wall = true;
    let mut dirty_lock = false;
    let mut prewarm_curtain_done = false;
    let daemon_started_at = Instant::now();
    let mut last_tick = Instant::now();

    loop {
        let t_iter = tb520fu_ui::perf::start();
        if SIGUSR1_LOCK.swap(false, Ordering::Relaxed) && !lockscreen.locked {
            if let Ok(ids) = wl.lock_session() {
                lock_surfaces = ids;
                lockscreen.enter_lock(false);
                dirty_lock = true;
            }
        }

        while let Ok(clip_txt) = clip_rx.try_recv() {
            inline_osk.push_clipboard(clip_txt);
        }

        while let Ok(bev) = bus_rx.try_recv() {
            match bev {
                ShellBusEvent::HardwareKeyOsd(hw_cmd) => {
                    crate::sys_state::request_gpu_boost(450);
                    last_hw_osd_at = Some(Instant::now());
                    match hw_cmd {
                        HwOsdCmd::VolumeDelta(delta) => {
                            let next_v =
                                (sys_local.volume_pct as i16 + delta as i16).clamp(0, 100) as u8;
                            if delta > 0 && sys_local.muted {
                                sys_local.muted = false;
                                prev_muted = false;
                                toggle_mute();
                            }
                            sys_local.volume_pct = next_v;
                            prev_vol = next_v;
                            set_volume_pct(next_v);
                            if !curtain.open && !lockscreen.locked {
                                bar.show_volume_osd(next_v, sys_local.muted);
                            }
                        }
                        HwOsdCmd::ToggleMute => {
                            sys_local.muted = !sys_local.muted;
                            prev_muted = sys_local.muted;
                            toggle_mute();
                            if !curtain.open && !lockscreen.locked {
                                bar.show_volume_osd(sys_local.volume_pct, sys_local.muted);
                            }
                        }
                        HwOsdCmd::ToggleMicMute => {
                            sys_local.mic_muted = !sys_local.mic_muted;
                            prev_mic_muted = sys_local.mic_muted;
                            toggle_mic_mute();
                            if !curtain.open && !lockscreen.locked {
                                bar.show_mic_osd(sys_local.mic_muted);
                            }
                        }
                        HwOsdCmd::BrightnessDelta(delta) => {
                            let next_b = (sys_local.brightness_pct as i16 + delta as i16)
                                .clamp(1, 100) as u8;
                            sys_local.brightness_pct = next_b;
                            prev_bright = next_b;
                            set_brightness_pct(next_b);
                            if !curtain.open && !lockscreen.locked {
                                bar.show_brightness_osd(next_b);
                            }
                        }
                    }
                    sync_optimistic_sys(&sys_shared, &sys_local);
                    dirty_bar = true;
                    if curtain.is_mapped() {
                        dirty_curtain = true;
                    }
                    if lockscreen.locked {
                        dirty_lock = true;
                    }
                }
                ShellBusEvent::NotifAdded(item) => {
                    if prefs.record_seen_notif_app(&item.app_name) {
                        prefs.save();
                    }
                    if prefs.is_app_muted(&item.app_name) {
                        continue;
                    }
                    let focused_fullscreen = niri_local
                        .focused_window()
                        .is_some_and(|w| w.tile_height >= (screen_logical.1 as f64) - 6.0);
                    let now_mins = crate::sys_state::local_minutes_of_day();
                    if !prefs.should_suppress_banner(now_mins, focused_fullscreen, &item.app_name)
                        && !curtain.open
                        && !lockscreen.locked
                    {
                        osd_banner.push_banner_with_timeout(item.clone(), prefs.banner_timeout_ms);
                        dirty_osd = true;
                    }
                    notifs.retain(|n| n.id != item.id);
                    notifs.push(item);
                    if notifs.len() > 60 {
                        notifs.remove(0);
                    }
                    sync_optimistic_sys(&sys_shared, &sys_local);
                    dirty_bar = true;
                    if curtain.is_mapped() {
                        dirty_curtain = true;
                    }
                    if lockscreen.locked {
                        dirty_lock = true;
                    }
                }
                ShellBusEvent::NotifClosed(id) => {
                    notifs.retain(|n| n.id != id);
                    sync_optimistic_sys(&sys_shared, &sys_local);
                    dirty_bar = true;
                    if curtain.is_mapped() {
                        dirty_curtain = true;
                    }
                    if lockscreen.locked {
                        dirty_lock = true;
                    }
                }
                ShellBusEvent::LockRequested => {
                    if !lockscreen.locked {
                        if let Ok(ids) = wl.lock_session() {
                            lock_surfaces = ids;
                            lockscreen.enter_lock(false);
                            dirty_lock = true;
                        }
                    }
                }
                ShellBusEvent::StartStandby => {
                    if !lockscreen.locked {
                        if let Ok(ids) = wl.lock_session() {
                            lock_surfaces = ids;
                            lockscreen.enter_lock(true);
                            dirty_lock = true;
                        }
                    } else {
                        lockscreen.switch_to_standby();
                        dirty_lock = true;
                    }
                }
                ShellBusEvent::UnlockRequested => {
                    if lockscreen.locked {
                        crate::sys_state::request_gpu_boost(650);
                        wl.unlock_session(&lock_surfaces);
                        lock_surfaces.clear();
                        lockscreen.locked = false;
                    }
                }
                ShellBusEvent::ToggleControlCenter => {
                    if curtain.open {
                        curtain.close();
                        inline_osk.hide(false);
                        osk_target = InlineOskTarget::None;
                    } else {
                        curtain.open_with_wing(PullWing::Right);
                    }
                    dirty_curtain = true;
                }
                ShellBusEvent::ToggleNotifications => {
                    if curtain.open && curtain.wing == PullWing::Left {
                        curtain.close();
                        inline_osk.hide(false);
                        osk_target = InlineOskTarget::None;
                    } else {
                        curtain.open_with_wing(PullWing::Left);
                    }
                    dirty_curtain = true;
                }
                ShellBusEvent::StartAnnot => {
                    curtain.close();
                    inline_osk.hide(false);
                    osk_target = InlineOskTarget::None;
                    tools.start_screenshot_annotation();
                    dirty_curtain = true;
                    dirty_tools = true;
                }
                ShellBusEvent::ToggleQuickNote => {
                    curtain.close();
                    handle_curtain_effect(
                        CurtainEffect::ToggleQuickNote,
                        &mut inline_osk,
                        &mut osk_target,
                        &mut tools,
                        &mut osd_banner,
                        &mut dirty_tools,
                        &mut dirty_osd,
                        &mut wl,
                        &mut lockscreen,
                        &mut lock_surfaces,
                        &mut dirty_lock,
                    );
                    dirty_curtain = true;
                }
                ShellBusEvent::OpenWindowMenu {
                    win_id,
                    cx,
                    ty,
                    is_floating,
                } => {
                    if !lockscreen.locked && !curtain.open {
                        tools.island_open = false;
                        tools.open_window_menu(win_id, cx, ty, is_floating);
                        dirty_tools = true;
                    }
                }
                ShellBusEvent::CurtainSwipeBegin => {
                    if !lockscreen.locked && curtain.is_mapped() {
                        curtain.on_3f_swipe_begin();
                        dirty_curtain = true;
                    }
                }
                ShellBusEvent::CurtainSwipeUpdate(dy) => {
                    if !lockscreen.locked && curtain.on_3f_swipe_update(dy, screen_logical.1) {
                        if curtain.progress.pos < 0.996 && osk_target.on_curtain() {
                            inline_osk.hide(false);
                            osk_target = InlineOskTarget::None;
                            curtain.app_menu.search_focused = false;
                        }
                        dirty_curtain = true;
                    }
                }
                ShellBusEvent::CurtainSwipeEnd => {
                    if !lockscreen.locked && curtain.on_3f_swipe_end(screen_logical.1) {
                        if !curtain.open && osk_target.on_curtain() {
                            inline_osk.hide(false);
                            osk_target = InlineOskTarget::None;
                        }
                        dirty_curtain = true;
                    }
                }
                ShellBusEvent::TouchpadEdgePull {
                    wing,
                    frac,
                    velocity,
                    finished,
                } => {
                    if !lockscreen.locked {
                        if tools.island_open || tools.win_menu.is_some() {
                            tools.island_open = false;
                            tools.win_menu = None;
                            dirty_tools = true;
                        }
                        curtain.on_touchpad_edge_pull(
                            wing,
                            frac,
                            velocity,
                            finished,
                            screen_logical.1,
                        );
                        if !curtain.open && osk_target.on_curtain() {
                            inline_osk.hide(false);
                            osk_target = InlineOskTarget::None;
                        }
                        dirty_curtain = true;
                    }
                }
                ShellBusEvent::SetPref { key, value_json } => {
                    let prev_skin = prefs.skin;
                    let prev_accent = prefs.accent;
                    let prev_wall = prefs.wallpaper.clone();
                    let prev_auto_rot = prefs.auto_rotate;
                    let prev_disp = (
                        prefs.night_light,
                        prefs.reading_mode,
                        prefs.high_refresh_144,
                    );
                    if prefs.apply_set_pref(&key, &value_json) {
                        prefs.save();
                        if prefs.skin != prev_skin || prefs.accent != prev_accent {
                            pal = Palette::for_skin_and_accent(prefs.skin, prefs.accent);
                            inline_osk.prefs.skin = prefs.skin;
                            inline_osk.prefs.accent = prefs.accent;
                            inline_osk.pal = pal;
                        }
                        if prefs.wallpaper != prev_wall {
                            tb520fu_ui::vk_renderer::set_active_wallpaper_spec(&prefs.wallpaper);
                            if let Some(vk_mtx) = tb520fu_ui::vk_renderer::VkRenderer::global() {
                                let _ = vk_mtx.lock().unwrap().reload_wallpaper();
                            }
                            dirty_wall = true;
                            dirty_lock = true;
                        }
                        if prefs.auto_rotate != prev_auto_rot {
                            auto_rotate_flag.store(prefs.auto_rotate, Ordering::Relaxed);
                            if prefs.auto_rotate {
                                crate::niri_ipc::set_dsi_transform("normal");
                            }
                        }
                        if (
                            prefs.night_light,
                            prefs.reading_mode,
                            prefs.high_refresh_144,
                        ) != prev_disp
                        {
                            sync_display_ctm_and_hz(&prefs, sys_local.pen_hover);
                        }
                        dirty_bar = true;
                        dirty_tools = true;
                        if curtain.is_mapped() {
                            dirty_curtain = true;
                        }
                        if lockscreen.locked {
                            dirty_lock = true;
                        }
                    }
                }
                ShellBusEvent::TouchpadLockWake => {
                    if lockscreen.locked
                        && (lockscreen.phase == crate::lockscreen::LockPhase::StandbyCompanion
                            || lockscreen.auth_reveal.target < 0.5)
                    {
                        lockscreen.open_auth_stage(false);
                        dirty_lock = true;
                    }
                }
                ShellBusEvent::SystemResumed => {
                    sys_local = sys_shared.lock().unwrap().clone();
                    lockscreen.on_system_resumed();
                    dirty_bar = true;
                    if curtain.is_mapped() {
                        dirty_curtain = true;
                    }
                    if lockscreen.locked {
                        dirty_lock = true;
                    }
                }
            }
        }

        // Sync shared Niri & System snapshots
        {
            let latest_niri = niri_shared.lock().unwrap().clone();
            if !niri_local.overview && latest_niri.overview {
                if tools.island_open || tools.win_menu.is_some() {
                    tools.island_open = false;
                    tools.win_menu = None;
                    dirty_tools = true;
                }
                if inline_osk.shown {
                    inline_osk.hide(false);
                    osk_target = InlineOskTarget::None;
                }
                call_osk_dbus_async("Hide");
            }
            let bar_niri_changed = latest_niri.windows.len() != niri_local.windows.len()
                || latest_niri.workspaces.len() != niri_local.workspaces.len()
                || latest_niri.focused_window().map(|w| (&w.id, &w.title, w.tile_width as i32))
                    != niri_local.focused_window().map(|w| (&w.id, &w.title, w.tile_width as i32))
                || latest_niri.overview != niri_local.overview;
            let tools_niri_changed = latest_niri.windows.len() != niri_local.windows.len()
                || latest_niri.workspaces.len() != niri_local.workspaces.len()
                || latest_niri
                    .focused_window()
                    .map(|w| (w.id, w.tile_width as i32, w.tile_height as i32))
                    != niri_local
                        .focused_window()
                        .map(|w| (w.id, w.tile_width as i32, w.tile_height as i32))
                || latest_niri.overview != niri_local.overview;
            if bar_niri_changed {
                bar.sync_target(&latest_niri);
                dirty_bar = true;
            }
            if let Some(wm) = tools.win_menu {
                if !latest_niri.windows.iter().any(|w| w.id == wm.win_id) {
                    tools.win_menu = None;
                    dirty_tools = true;
                }
            }
            if tools_niri_changed && tools.has_visible_layer(&prefs, &latest_niri) {
                dirty_tools = true;
            }
            niri_local = latest_niri;
        }
        {
            let mut latest_sys = sys_shared.lock().unwrap().clone();
            if crate::sys_state::is_vol_drag_recent() {
                latest_sys.volume_pct = sys_local.volume_pct;
                latest_sys.muted = sys_local.muted;
                latest_sys.mic_muted = sys_local.mic_muted;
            }
            if crate::sys_state::is_bri_drag_recent() {
                latest_sys.brightness_pct = sys_local.brightness_pct;
                latest_sys.autobright = sys_local.autobright;
            }
            if latest_sys != sys_local {
                // Stylus hover dynamic 144Hz <-> 120Hz switch
                if latest_sys.pen_hover != prev_pen_hover {
                    prev_pen_hover = latest_sys.pen_hover;
                    sync_display_ctm_and_hz(&prefs, latest_sys.pen_hover);
                }
                // Lenovo Tab Pen Plus barrel/tail button actions (`601` / `612` / `613`)
                if latest_sys.pen_action_seq != prev_pen_seq && latest_sys.pen_action_seq > 0 {
                    prev_pen_seq = latest_sys.pen_action_seq;
                    HapticEngine::global().pulse_norm(HapticKind::Medium, 0.5);
                    match latest_sys.pen_action_code {
                        601 | 331 => {
                            tools.start_screenshot_annotation();
                            dirty_tools = true;
                        }
                        _ => {
                            tools.note_open = !tools.note_open;
                            dirty_tools = true;
                        }
                    }
                }

                let recent_hw_osd = last_hw_osd_at
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(550));
                let startup_settled =
                    daemon_started_at.elapsed() >= std::time::Duration::from_secs(4);
                // Only show passive OSD for post-startup external audio changes (e.g. BT headset AVRCP);
                // NEVER pop brightness OSD from background sysfs/daemon polling (hardware brightness keys
                // already trigger `ShellBusEvent::HardwareKeyOsd(BrightnessDelta)` directly).
                if startup_settled && !curtain.open && !lockscreen.locked && !recent_hw_osd {
                    if latest_sys.volume_pct != prev_vol || latest_sys.muted != prev_muted {
                        bar.show_volume_osd(latest_sys.volume_pct, latest_sys.muted);
                        dirty_bar = true;
                    } else if latest_sys.mic_muted != prev_mic_muted {
                        bar.show_mic_osd(latest_sys.mic_muted);
                        dirty_bar = true;
                    }
                }
                prev_vol = latest_sys.volume_pct;
                prev_muted = latest_sys.muted;
                prev_mic_muted = latest_sys.mic_muted;
                prev_bright = latest_sys.brightness_pct;
                let _ = prev_bright;
                let ui_changed = !latest_sys.ui_visible_eq(&sys_local);
                sys_local = latest_sys;
                if ui_changed {
                    dirty_bar = true;
                    if curtain.is_mapped() {
                        dirty_curtain = true;
                    }
                    if tools.island_open {
                        dirty_tools = true;
                    }
                    if lockscreen.locked {
                        dirty_lock = true;
                    }
                }
            }
        }

        // Dispatch Wayland input & configure events
        let events = wl.poll_events()?;
        for ev in events {
            match ev {
                UiEvent::Configured {
                    surface,
                    width,
                    height,
                    ..
                } => {
                    if (surface == curtain_id || surface == tools_id) && width > 0 && height > 0 {
                        screen_logical = (width as f32, height as f32);
                        inline_osk.set_screen_size(width, height);
                        let now_portrait = width < height;
                        if was_portrait != Some(now_portrait) {
                            was_portrait = Some(now_portrait);
                            apply_portrait_policy(now_portrait);
                        }
                    }
                    if surface == bar_id {
                        dirty_bar = true;
                    } else if surface == tools_id {
                        if tools.has_visible_layer(&prefs, &niri_local) {
                            dirty_tools = true;
                        } else {
                            wl.unmap_surface(tools_id);
                        }
                    } else if surface == curtain_id {
                        if curtain.is_mapped() {
                            dirty_curtain = true;
                        } else {
                            wl.unmap_surface(curtain_id);
                        }
                    } else if surface == osd_id {
                        if osd_banner.is_visible() {
                            dirty_osd = true;
                        } else {
                            wl.unmap_surface(osd_id);
                        }
                    } else if surface == wall_id {
                        dirty_wall = true;
                    } else if lock_surfaces.contains(&surface) {
                        dirty_lock = true;
                    }
                }
                UiEvent::TouchDown {
                    surface,
                    slot,
                    x,
                    y,
                    ..
                } => {
                    touch_coords.insert((surface, slot), (x, y));
                    if lock_surfaces.contains(&surface) {
                        if lockscreen.on_touch_down(slot, x, y) {
                            dirty_lock = true;
                        }
                        continue;
                    }

                    // Route touch to embedded Y700 OSK first if active on this surface!
                    if inline_osk.shown
                        && ((surface == curtain_id && osk_target.on_curtain())
                            || (surface == tools_id && osk_target == InlineOskTarget::QuickNote))
                    {
                        let (ox, oy) = inline_osk_origin(&inline_osk, screen_logical.0, screen_logical.1);
                        if inline_osk.on_touch_down(slot, x - ox, y - oy) {
                            osk_captured_slots.insert((surface, slot), (ox, oy));
                            drain_inline_osk(
                                &mut inline_osk,
                                &mut osk_target,
                                &mut curtain,
                                &mut tools,
                                &niri_local,
                                &prefs,
                            );
                            dirty_curtain |= surface == curtain_id;
                            dirty_tools |= surface == tools_id;
                            continue;
                        }
                    }

                    if surface == bar_id {
                        if let BarAction::ScrubOsd { is_brightness, pct } =
                            bar.on_touch_down(slot, x, y, screen_logical.0)
                        {
                            last_hw_osd_at = Some(Instant::now());
                            apply_osd_scrub(
                                is_brightness,
                                pct,
                                &mut sys_local,
                                &sys_shared,
                                &mut prev_vol,
                                &mut prev_muted,
                                &mut prev_bright,
                            );
                            dirty_bar = true;
                        }
                    } else if surface == curtain_id && curtain.is_mapped() {
                        let eff = curtain.on_touch_down(slot, x, y, &mut sys_local);
                        sync_optimistic_sys(&sys_shared, &sys_local);
                        handle_curtain_effect(
                            eff,
                            &mut inline_osk,
                            &mut osk_target,
                            &mut tools,
                            &mut osd_banner,
                            &mut dirty_tools,
                            &mut dirty_osd,
                            &mut wl,
                            &mut lockscreen,
                            &mut lock_surfaces,
                            &mut dirty_lock,
                        );
                        dirty_curtain = true;
                    } else if surface == tools_id {
                        if tools.on_touch_down(
                            slot,
                            x,
                            y,
                            screen_logical.0,
                            screen_logical.1,
                            &prefs,
                        ) {
                            dirty_tools = true;
                        }
                    } else if surface == osd_id {
                        if osd_banner.on_touch_down(slot, x, y) {
                            dirty_osd = true;
                        }
                    }
                }
                UiEvent::TouchMotion {
                    surface,
                    slot,
                    x,
                    y,
                    ..
                } => {
                    touch_coords.insert((surface, slot), (x, y));
                    if lock_surfaces.contains(&surface) {
                        if lockscreen.on_touch_motion(slot, x, y) {
                            dirty_lock = true;
                        }
                        continue;
                    }
                    if let Some(&(ox, oy)) = osk_captured_slots.get(&(surface, slot)) {
                        inline_osk.on_touch_motion(slot, x - ox, y - oy);
                        drain_inline_osk(
                            &mut inline_osk,
                            &mut osk_target,
                            &mut curtain,
                            &mut tools,
                            &niri_local,
                            &prefs,
                        );
                        dirty_curtain |= surface == curtain_id;
                        dirty_tools |= surface == tools_id;
                        continue;
                    }

                    if surface == bar_id {
                        match bar.on_touch_motion(slot, x, y) {
                            BarAction::DragConsole { wing, finger_y } => {
                                if tools.island_open {
                                    tools.island_open = false;
                                    dirty_tools = true;
                                }
                                curtain.drag_open(wing, finger_y, screen_logical.1);
                                dirty_curtain = true;
                            }
                            BarAction::ScrubOsd { is_brightness, pct } => {
                                last_hw_osd_at = Some(Instant::now());
                                apply_osd_scrub(
                                    is_brightness,
                                    pct,
                                    &mut sys_local,
                                    &sys_shared,
                                    &mut prev_vol,
                                    &mut prev_muted,
                                    &mut prev_bright,
                                );
                                dirty_bar = true;
                            }
                            _ => {}
                        }
                    } else if surface == curtain_id && curtain.is_mapped() {
                        let eff = curtain.on_touch_motion(slot, x, y, &mut sys_local);
                        sync_optimistic_sys(&sys_shared, &sys_local);
                        if curtain.progress.pos < 0.996 && osk_target.on_curtain() {
                            inline_osk.shown = false;
                            inline_osk
                                .slide_y
                                .snap(inline_osk.metrics.surf_h as f32 + 24.0);
                            osk_target = InlineOskTarget::None;
                            curtain.app_menu.search_focused = false;
                        }
                        handle_curtain_effect(
                            eff,
                            &mut inline_osk,
                            &mut osk_target,
                            &mut tools,
                            &mut osd_banner,
                            &mut dirty_tools,
                            &mut dirty_osd,
                            &mut wl,
                            &mut lockscreen,
                            &mut lock_surfaces,
                            &mut dirty_lock,
                        );
                        dirty_curtain = true;
                    } else if surface == tools_id {
                        if tools.on_touch_motion(slot, x, y) {
                            dirty_tools = true;
                        }
                    } else if surface == osd_id {
                        if osd_banner.on_touch_motion(slot, x, y) {
                            dirty_osd = true;
                        }
                    }
                }
                UiEvent::TouchUp { surface, slot, .. } => {
                    let (x, y) = touch_coords.remove(&(surface, slot)).unwrap_or((0.0, 0.0));
                    if osk_captured_slots.remove(&(surface, slot)).is_some() {
                        inline_osk.on_touch_up(slot);
                        drain_inline_osk(
                            &mut inline_osk,
                            &mut osk_target,
                            &mut curtain,
                            &mut tools,
                            &niri_local,
                            &prefs,
                        );
                        dirty_curtain |= surface == curtain_id;
                        dirty_tools |= surface == tools_id;
                        continue;
                    }

                    if lock_surfaces.contains(&surface) {
                        if lockscreen.on_touch_up(slot, x, y) {
                            dirty_lock = true;
                        }
                    } else if surface == bar_id {
                        match bar.on_touch_up(slot, x, y) {
                            BarAction::OpenConsole { wing } => {
                                if tools.island_open {
                                    tools.island_open = false;
                                    dirty_tools = true;
                                }
                                if curtain.open && curtain.wing == wing {
                                    curtain.close();
                                    inline_osk.shown = false;
                                    inline_osk
                                        .slide_y
                                        .snap(inline_osk.metrics.surf_h as f32 + 24.0);
                                    osk_target = InlineOskTarget::None;
                                } else {
                                    curtain.open_with_wing(wing);
                                }
                                dirty_curtain = true;
                            }
                            BarAction::FinishPull { wing, vy } => {
                                curtain.finish_pull(wing, vy, screen_logical.1);
                                if !curtain.open {
                                    inline_osk.shown = false;
                                    inline_osk
                                        .slide_y
                                        .snap(inline_osk.metrics.surf_h as f32 + 24.0);
                                    osk_target = InlineOskTarget::None;
                                }
                                dirty_curtain = true;
                            }
                            BarAction::ToggleIslandPop => {
                                let has_media =
                                    !sys_local.media_title.is_empty() || sys_local.media_playing;
                                tools.island_open = if has_media { !tools.island_open } else { false };
                                dirty_tools = true;
                            }
                            BarAction::ScrubOsd { is_brightness, pct } => {
                                last_hw_osd_at = Some(Instant::now());
                                apply_osd_scrub(
                                    is_brightness,
                                    pct,
                                    &mut sys_local,
                                    &sys_shared,
                                    &mut prev_vol,
                                    &mut prev_muted,
                                    &mut prev_bright,
                                );
                                dirty_bar = true;
                            }
                            _ => {}
                        }
                    } else if surface == curtain_id && curtain.is_mapped() {
                        let prev_skin = prefs.skin;
                        let prev_pen_tools = (prefs.pen_corners, prefs.pen_orb);
                        let eff = curtain.on_touch_up(
                            slot,
                            x,
                            y,
                            &mut sys_local,
                            &mut niri_local,
                            &mut prefs,
                            &auto_rotate_flag,
                            &mut notifs,
                        );
                        sync_optimistic_sys(&sys_shared, &sys_local);
                        if !curtain.open && osk_target.on_curtain() {
                            inline_osk.shown = false;
                            inline_osk
                                .slide_y
                                .snap(inline_osk.metrics.surf_h as f32 + 24.0);
                            osk_target = InlineOskTarget::None;
                        }
                        if prefs.skin != prev_skin {
                            pal = Palette::for_skin(prefs.skin);
                            inline_osk.prefs.skin = prefs.skin;
                            inline_osk.pal = pal;
                            dirty_tools = true;
                            dirty_bar = true;
                        } else if (prefs.pen_corners, prefs.pen_orb) != prev_pen_tools {
                            dirty_tools = true;
                        }
                        handle_curtain_effect(
                            eff,
                            &mut inline_osk,
                            &mut osk_target,
                            &mut tools,
                            &mut osd_banner,
                            &mut dirty_tools,
                            &mut dirty_osd,
                            &mut wl,
                            &mut lockscreen,
                            &mut lock_surfaces,
                            &mut dirty_lock,
                        );
                        dirty_curtain = true;
                        if curtain.open {
                            dirty_bar = true;
                        }
                    } else if surface == tools_id {
                        match tools.on_touch_up(slot, x, y, &mut sys_local, &niri_local) {
                            ToolEffect::FocusQuickNoteOsk => {
                                osk_target = InlineOskTarget::QuickNote;
                                inline_osk.direct_text_sink = true;
                                inline_osk.enter_label = Some("换行");
                                inline_osk.show(true);
                            }
                            ToolEffect::SavedToast(msg) => {
                                bar.show_toast_osd("check", msg);
                            }
                            ToolEffect::None => {
                                if !tools.note_open && osk_target == InlineOskTarget::QuickNote {
                                    inline_osk.hide(false);
                                    osk_target = InlineOskTarget::None;
                                }
                            }
                        }
                        dirty_tools = true;
                        dirty_bar = true;
                    } else if surface == osd_id {
                        match osd_banner.on_touch_up(slot, x, y) {
                            BannerAction::OpenNotifications => {
                                curtain.open_with_wing(PullWing::Left);
                                dirty_curtain = true;
                                dirty_osd = true;
                            }
                            BannerAction::Dismiss | BannerAction::None => {
                                dirty_osd = true;
                            }
                        }
                    }
                }
                UiEvent::TouchCancel { surface } => {
                    touch_coords.retain(|&(s, _), _| s != surface);
                    if lock_surfaces.contains(&surface) {
                        lockscreen.on_touch_cancel();
                        dirty_lock = true;
                    }
                    if osk_captured_slots.keys().any(|&(s, _)| s == surface) {
                        osk_captured_slots.retain(|&(s, _), _| s != surface);
                        if inline_osk.on_touch_cancel() {
                            dirty_curtain |= surface == curtain_id;
                            dirty_tools |= surface == tools_id;
                        }
                    }
                    if surface == bar_id {
                        if let BarAction::FinishPull { wing, vy } = bar.on_touch_cancel() {
                            curtain.finish_pull(wing, vy, screen_logical.1);
                            if !curtain.open {
                                inline_osk.hide(false);
                                osk_target = InlineOskTarget::None;
                            }
                            dirty_curtain = true;
                        }
                        dirty_bar = true;
                    } else if surface == curtain_id && curtain.is_mapped() {
                        curtain.on_touch_cancel();
                        dirty_curtain = true;
                    } else if surface == osd_id {
                        osd_banner.on_touch_cancel();
                        dirty_osd = true;
                    }
                }
                UiEvent::Scroll {
                    surface,
                    x,
                    y,
                    dx,
                    dy,
                } => {
                    if lock_surfaces.contains(&surface) {
                        if lockscreen.on_scroll(dx, dy) {
                            dirty_lock = true;
                        }
                    } else if surface == bar_id && !lockscreen.locked {
                        let allow_bar_scroll = curtain.scroll_pull_active
                            || !bar.is_in_center_no_pull(x, screen_logical.0);
                        if allow_bar_scroll
                            && curtain.on_topbar_scroll(x, dy, screen_logical.0, screen_logical.1)
                        {
                            if tools.island_open || tools.win_menu.is_some() {
                                tools.island_open = false;
                                tools.win_menu = None;
                                dirty_tools = true;
                            }
                            dirty_curtain = true;
                        }
                    } else if surface == curtain_id && curtain.is_mapped() {
                        if curtain.on_scroll(x, y, dy) {
                            if curtain.progress.pos < 0.996 && osk_target.on_curtain() {
                                inline_osk.hide(false);
                                osk_target = InlineOskTarget::None;
                                curtain.app_menu.search_focused = false;
                            }
                            dirty_curtain = true;
                        }
                    }
                }
                UiEvent::ScrollStop { surface } => {
                    if lock_surfaces.contains(&surface) {
                        if lockscreen.on_scroll_stop() {
                            dirty_lock = true;
                        }
                    } else if (surface == bar_id || surface == curtain_id)
                        && curtain.on_scroll_stop(screen_logical.1)
                    {
                        if !curtain.open && osk_target.on_curtain() {
                            inline_osk.hide(false);
                            osk_target = InlineOskTarget::None;
                        }
                        dirty_curtain = true;
                    }
                }
                UiEvent::PointerActivity { surface } => {
                    if lock_surfaces.contains(&surface) {
                        lockscreen.on_pointer_activity();
                    }
                }
                UiEvent::PointerSecondaryClick { surface, .. } => {
                    if lock_surfaces.contains(&surface) && lockscreen.on_secondary_click() {
                        dirty_lock = true;
                    }
                }
                UiEvent::KeyPress {
                    evdev_code,
                    pressed,
                } => {
                    if lockscreen.locked {
                        if lockscreen.on_evdev_key_event(evdev_code, pressed) {
                            dirty_lock = true;
                        }
                    } else if pressed && evdev_code == 1 {
                        // Escape dismisses Console, Media Popup, Window Menu, or Annotation
                        if curtain.open {
                            curtain.close();
                            inline_osk.hide(false);
                            osk_target = InlineOskTarget::None;
                            dirty_curtain = true;
                        } else if tools.island_open {
                            tools.island_open = false;
                            dirty_tools = true;
                        } else if tools.win_menu.is_some() {
                            tools.win_menu = None;
                            dirty_tools = true;
                        } else if tools.annot_open {
                            tools.annot_open = false;
                            dirty_tools = true;
                        }
                    }
                }
                _ => {}
            }
        }

        // Advance springs, timers, inline OSK & PAM auth
        let now = Instant::now();
        let dt = now.duration_since(last_tick).as_secs_f32().min(0.05);
        if dt >= 0.001 {
            last_tick = now;

            if bar.step(dt, sys_local.media_playing, &niri_local) {
                dirty_bar = true;
            }

            let (c_rd, c_unmap, _) = curtain.tick(dt, &niri_local);
            dirty_curtain |= c_rd;
            if c_unmap {
                wl.unmap_surface(curtain_id);
                wl.set_input_region(curtain_id, Some(&[]));
            }

            let (osk_rd, _) = inline_osk.tick(dt);
            if osk_rd {
                let prev_target = osk_target;
                drain_inline_osk(
                    &mut inline_osk,
                    &mut osk_target,
                    &mut curtain,
                    &mut tools,
                    &niri_local,
                    &prefs,
                );
                if osk_target.on_curtain() || prev_target.on_curtain() {
                    dirty_curtain = true;
                } else if osk_target == InlineOskTarget::QuickNote
                    || prev_target == InlineOskTarget::QuickNote
                {
                    dirty_tools = true;
                }
            }

            if tools.tick(dt, &prefs) {
                dirty_tools = true;
                dirty_bar = true;
            }

            if (curtain.is_mapped() || lockscreen.locked) && osd_banner.active_banner.is_some() {
                osd_banner.active_banner = None;
                osd_banner.expires_at = None;
                osd_banner.reveal.snap(0.0);
                wl.unmap_surface(osd_id);
                wl.set_input_region(osd_id, Some(&[]));
                wl.set_keep_swapchain(osd_id, true);
                dirty_osd = false;
            } else if osd_banner.step(dt) {
                if osd_banner.is_visible() {
                    dirty_osd = true;
                } else {
                    wl.unmap_surface(osd_id);
                    wl.set_input_region(osd_id, Some(&[]));
                    wl.set_keep_swapchain(osd_id, true);
                    dirty_osd = false;
                }
            }

            if lockscreen.locked {
                let (lk_rd, unlocked) = lockscreen.tick(dt, &sys_local, prefs.standby_companion);
                dirty_lock |= lk_rd;
                if unlocked {
                    crate::sys_state::request_gpu_boost(650);
                    wl.unlock_session(&lock_surfaces);
                    lock_surfaces.clear();
                    lockscreen.locked = false;
                    crate::sys_state::unlock_logind_session();
                }
            }
        }

        // Present dirty surfaces. Each is drawn at most once per display refresh: until the
        // compositor's frame callback for the previous frame arrives it stays dirty, and the callback
        // itself wakes the poll below.
        if dirty_wall && wl.frame_ready(wall_id) {
            if let Some((lw, lh, sc)) = wl.surface_geometry(wall_id) {
                // Full native physical resolution (`2944x1840`) static `wl_shm` buffer:
                // 0 MB Vulkan WSI swapchain VRAM, razor-sharp Empty Home wallpaper, and Monet hue extraction!
                let phys_w = ((lw as f32) * sc).round().max(1.0) as u32;
                let phys_h = ((lh as f32) * sc).round().max(1.0) as u32;
                if wl.present_shm_argb8888(wall_id, phys_w, phys_h, |dst| {
                    tb520fu_ui::vk_renderer::render_wallpaper_bgra_and_monet(dst, phys_w, phys_h);
                }) {
                    dirty_wall = false;
                    dirty_bar = true;
                }
            }
        }

        if !prewarm_curtain_done && !dirty_wall {
            if let Some((lw, lh, sc)) = wl.surface_geometry(curtain_id) {
                prewarm_curtain_done = true;
                let pw = ((lw as f32) * sc).round().max(1.0) as u32;
                let ph = ((lh as f32) * sc).round().max(1.0) as u32;
                curtain_canvas.resize(pw, ph, sc);
                curtain_canvas.set_screen_context(0.0, 0.0, lw as f32, lh as f32);
                let saved_p = curtain.progress.pos;
                curtain.progress.pos = 0.5;
                curtain.render(
                    &mut curtain_canvas,
                    &icons,
                    &mut text,
                    &pal,
                    &sys_local,
                    &niri_local,
                    &prefs,
                    &notifs,
                    0.0,
                );
                curtain.progress.pos = saved_p;
                curtain.render(
                    &mut curtain_canvas,
                    &icons,
                    &mut text,
                    &pal,
                    &sys_local,
                    &niri_local,
                    &prefs,
                    &notifs,
                    0.0,
                );
                curtain_canvas.clear(Rgba::TRANSPARENT);

                let osk_pw = ((inline_osk.metrics.surf_w.max(1) as f32) * sc).round() as u32;
                let osk_ph = ((inline_osk.metrics.surf_h.max(1) as f32) * sc).round() as u32;
                osk_sub_canvas.resize(osk_pw, osk_ph, sc);
                let was_shown = inline_osk.shown;
                inline_osk.shown = true;
                inline_osk_ren.render(&mut inline_osk, &mut osk_sub_canvas);
                inline_osk.shown = was_shown;
                osk_sub_canvas.clear(Rgba::TRANSPARENT);

                lock_canvas.resize(pw, ph, sc);
                lockscreen.render(
                    &mut lock_canvas,
                    &icons,
                    &mut text,
                    &pal,
                    &sys_local,
                    &notifs,
                );
                lock_canvas.clear(Rgba::TRANSPARENT);

                if let Some(vk_mtx) = tb520fu_ui::vk_renderer::VkRenderer::global() {
                    let _ = vk_mtx.lock().unwrap().flush_pending_work();
                }
                wl.prewarm_wsi_swapchain(curtain_id, pw, ph);
                wl.set_keep_swapchain(tools_id, true);
                wl.prewarm_wsi_swapchain(tools_id, pw, ph);
                if let Some((olw, olh, osc)) = wl.surface_geometry(osd_id) {
                    wl.prewarm_wsi_swapchain(
                        osd_id,
                        ((olw as f32) * osc).round().max(1.0) as u32,
                        ((olh as f32) * osc).round().max(1.0) as u32,
                    );
                }
            }
        }

        if dirty_bar && curtain.progress.pos < 0.995 && wl.frame_ready(bar_id) {
            if let Some((lw, lh, sc)) = wl.surface_geometry(bar_id) {
                bar_canvas.resize(
                    ((lw as f32) * sc).round() as u32,
                    ((lh as f32) * sc).round() as u32,
                    sc,
                );
                bar_canvas.set_screen_context(0.0, 0.0, screen_logical.0, screen_logical.1);
                bar.render(
                    &mut bar_canvas,
                    &icons,
                    &mut text,
                    &pal,
                    &sys_local,
                    &niri_local,
                    &prefs,
                    notifs.len(),
                );
                if wl.present(
                    bar_id,
                    &bar_canvas,
                    sys_local.media_playing || bar.is_animating(),
                ) {
                    dirty_bar = false;
                }
            }
        }

        if dirty_tools && wl.frame_ready(tools_id) {
            let show_osk_here = osk_target == InlineOskTarget::QuickNote
                && (inline_osk.shown || inline_osk.slide_y.is_animating());
            if tools.has_visible_layer(&prefs, &niri_local) || show_osk_here {
                if let Some((lw, lh, sc)) = wl.surface_geometry(tools_id) {
                    tools_canvas.resize(
                        ((lw as f32) * sc).round() as u32,
                        ((lh as f32) * sc).round() as u32,
                        sc,
                    );
                    tools_canvas.set_screen_context(0.0, 0.0, lw as f32, lh as f32);
                    let mut regions = tools.render(
                        &mut tools_canvas,
                        &icons,
                        &mut text,
                        &pal,
                        &sys_local,
                        &niri_local,
                        &prefs,
                    );
                    let was_saving = tools.annot_open && regions.len() == 1 && tools.annot_strokes.is_empty();
                    let _ = was_saving;
                    if show_osk_here {
                        let (ox, oy) = inline_osk_origin(&inline_osk, lw as f32, lh as f32);
                        osk_sub_canvas.resize(
                            ((inline_osk.metrics.surf_w as f32) * sc).round() as u32,
                            ((inline_osk.metrics.surf_h as f32) * sc).round() as u32,
                            sc,
                        );
                        osk_sub_canvas.set_screen_context(ox, oy, lw as f32, lh as f32);
                        inline_osk_ren.render(&mut inline_osk, &mut osk_sub_canvas);
                        composite_subcanvas(&mut tools_canvas, &osk_sub_canvas, ox, oy);
                        regions.push(RectF::new(
                            ox,
                            oy,
                            inline_osk.metrics.surf_w as f32,
                            inline_osk.metrics.surf_h as f32,
                        ));
                    }
                    wl.set_input_region(tools_id, Some(&regions));
                    if wl.present(tools_id, &tools_canvas, inline_osk.slide_y.is_animating()) {
                        dirty_tools = false;
                    }
                }
            } else {
                wl.unmap_surface(tools_id);
                wl.set_input_region(tools_id, Some(&[]));
                dirty_tools = false;
            }
        }

        if dirty_curtain && curtain.is_mapped() && wl.frame_ready(curtain_id) {
            if let Some((lw, lh, sc)) = wl.surface_geometry(curtain_id) {
                curtain_canvas.resize(
                    ((lw as f32) * sc).round() as u32,
                    ((lh as f32) * sc).round() as u32,
                    sc,
                );
                curtain_canvas.set_screen_context(0.0, 0.0, lw as f32, lh as f32);

                let show_osk_here = osk_target.on_curtain()
                    && (inline_osk.shown || inline_osk.slide_y.is_animating());
                let osk_inset = if show_osk_here && inline_osk.prefs.mode != KbMode::Float {
                    (inline_osk.metrics.kb_h - inline_osk.current_slide_offset() + 12.0).max(0.0)
                } else {
                    0.0
                };

                curtain.render(
                    &mut curtain_canvas,
                    &icons,
                    &mut text,
                    &pal,
                    &sys_local,
                    &niri_local,
                    &prefs,
                    &notifs,
                    osk_inset,
                );

                if show_osk_here {
                    let (ox, oy) = inline_osk_origin(&inline_osk, lw as f32, lh as f32);
                    osk_sub_canvas.resize(
                        ((inline_osk.metrics.surf_w as f32) * sc).round() as u32,
                        ((inline_osk.metrics.surf_h as f32) * sc).round() as u32,
                        sc,
                    );
                    osk_sub_canvas.set_screen_context(ox, oy, lw as f32, lh as f32);
                    inline_osk_ren.render(&mut inline_osk, &mut osk_sub_canvas);
                    composite_subcanvas(&mut curtain_canvas, &osk_sub_canvas, ox, oy);
                }

                wl.set_input_region(curtain_id, None);
                if wl.present(
                    curtain_id,
                    &curtain_canvas,
                    curtain.is_animating() || inline_osk.slide_y.is_animating(),
                ) {
                    dirty_curtain = false;
                }
            }
        }

        if dirty_osd && osd_banner.is_visible() && wl.frame_ready(osd_id) {
            if let Some((lw, lh, sc)) = wl.surface_geometry(osd_id) {
                osd_canvas.resize(
                    ((lw as f32) * sc).round() as u32,
                    ((lh as f32) * sc).round() as u32,
                    sc,
                );
                osd_canvas.set_screen_context(
                    (screen_logical.0 - lw as f32) * 0.5,
                    52.0,
                    screen_logical.0,
                    screen_logical.1,
                );
                let regions = osd_banner.render(&mut osd_canvas, &icons, &mut text, &pal);
                wl.set_input_region(osd_id, Some(&regions));
                if wl.present(osd_id, &osd_canvas, osd_banner.is_animating()) {
                    dirty_osd = false;
                }
            }
        }

        if dirty_lock && lockscreen.locked && lock_surfaces.iter().all(|&id| wl.frame_ready(id)) {
            let mut all_ok = true;
            let anim = lockscreen.is_animating();
            for &ls_id in &lock_surfaces {
                if let Some((lw, lh, sc)) = wl.surface_geometry(ls_id) {
                    lock_canvas.resize(
                        ((lw as f32) * sc).round() as u32,
                        ((lh as f32) * sc).round() as u32,
                        sc,
                    );
                    lock_canvas.set_screen_context(0.0, 0.0, lw as f32, lh as f32);
                    lockscreen.render(
                        &mut lock_canvas,
                        &icons,
                        &mut text,
                        &pal,
                        &sys_local,
                        &notifs,
                    );
                    if !wl.present(ls_id, &lock_canvas, anim) {
                        all_ok = false;
                    }
                } else {
                    all_ok = false;
                }
            }
            if all_ok {
                dirty_lock = false;
            }
        }

        let animating = bar.is_animating()
            || curtain.is_animating()
            || osd_banner.is_animating()
            || inline_osk.slide_y.is_animating()
            || (lockscreen.locked && lockscreen.is_animating());
        let timeout_ms = if animating {
            7
        } else if osd_banner.is_visible()
            || bar.has_pending_osd_timeout()
            || sys_local.media_playing
            || (lockscreen.locked && (lockscreen.verifying || lockscreen.error_msg.is_some()))
        {
            120
        } else {
            1000
        };

        let mut pfds = [
            libc::pollfd {
                fd: wl.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wakeup_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let nfds = if wakeup_fd >= 0 { 2 } else { 1 };
        // time spent busy this iteration = how long input had to wait before being looked at again
        tb520fu_ui::perf::end("loop.busy", t_iter);
        unsafe {
            let timeout_ms = if wl.has_queued_events() { 0 } else { timeout_ms };
            let _ = libc::poll(pfds.as_mut_ptr(), nfds as libc::nfds_t, timeout_ms);
            if wakeup_fd >= 0 && (pfds[1].revents & libc::POLLIN) != 0 {
                let mut sink = 0u64;
                let _ = libc::read(wakeup_fd, &mut sink as *mut u64 as *mut libc::c_void, 8);
            }
        }
    }
}

fn sync_optimistic_sys(shared: &Arc<Mutex<SysSnapshot>>, local: &SysSnapshot) {
    let mut g = shared.lock().unwrap();
    g.volume_pct = local.volume_pct;
    g.muted = local.muted;
    g.mic_muted = local.mic_muted;
    g.brightness_pct = local.brightness_pct;
    g.autobright = local.autobright;
    g.torch = local.torch;
    g.charge_limit = local.charge_limit;
    g.bat_charging = local.bat_charging;
    g.wifi_on = local.wifi_on;
    g.bt_on = local.bt_on;
}

fn apply_osd_scrub(
    is_brightness: bool,
    pct: u8,
    sys_local: &mut SysSnapshot,
    sys_shared: &Arc<Mutex<SysSnapshot>>,
    prev_vol: &mut u8,
    prev_muted: &mut bool,
    prev_bright: &mut u8,
) {
    if is_brightness {
        let p = pct.clamp(1, 100);
        sys_local.brightness_pct = p;
        *prev_bright = p;
        set_brightness_pct(p);
    } else {
        let p = pct.min(100);
        if p > 0 && sys_local.muted {
            sys_local.muted = false;
            *prev_muted = false;
            toggle_mute();
        }
        sys_local.volume_pct = p;
        *prev_vol = p;
        set_volume_pct(p);
    }
    sync_optimistic_sys(sys_shared, sys_local);
}

fn drain_inline_osk(
    osk: &mut OskState,
    target: &mut InlineOskTarget,
    curtain: &mut Curtain,
    tools: &mut CompanionTools,
    niri: &NiriSnapshot,
    prefs: &ShellPrefs,
) {
    let actions = std::mem::take(&mut osk.outbox);
    for act in actions {
        match act {
            OutgoingAction::DirectText(s) => match *target {
                InlineOskTarget::AppSearch => {
                    curtain.app_menu.query.push_str(&s);
                    curtain.app_menu.scroll_y = 0.0;
                }
                InlineOskTarget::WifiPassword => {
                    curtain.wifi_pw_buf.push_str(&s);
                }
                InlineOskTarget::QuickNote => {
                    tools.note_text.push_str(&s);
                    tools.save_note();
                }
                InlineOskTarget::None => {}
            },
            OutgoingAction::DirectBackspace => match *target {
                InlineOskTarget::AppSearch => {
                    curtain.app_menu.query.pop();
                    curtain.app_menu.scroll_y = 0.0;
                }
                InlineOskTarget::WifiPassword => {
                    curtain.wifi_pw_buf.pop();
                }
                InlineOskTarget::QuickNote => {
                    tools.note_text.pop();
                    tools.save_note();
                }
                InlineOskTarget::None => {}
            },
            OutgoingAction::DirectClearAll => match *target {
                InlineOskTarget::AppSearch => {
                    curtain.app_menu.query.clear();
                    curtain.app_menu.scroll_y = 0.0;
                }
                InlineOskTarget::WifiPassword => {
                    curtain.wifi_pw_buf.clear();
                }
                InlineOskTarget::QuickNote => {
                    tools.note_text.clear();
                    tools.save_note();
                }
                InlineOskTarget::None => {}
            },
            OutgoingAction::DirectEnter => match *target {
                InlineOskTarget::AppSearch => {
                    let indices = curtain.app_menu.filtered_indices(prefs);
                    if let Some(&first_idx) = indices.first() {
                        if let Some(app) = curtain.app_menu.apps.get(first_idx) {
                            crate::niri_ipc::spawn_app_with_mode(
                                app.exec_argv.clone(),
                                prefs.launch_mode,
                                niri,
                            );
                            curtain.close();
                            osk.hide(false);
                            *target = InlineOskTarget::None;
                        }
                    }
                }
                InlineOskTarget::WifiPassword => {
                    if curtain.submit_wifi_password() {
                        osk.hide(false);
                        *target = InlineOskTarget::None;
                    }
                }
                InlineOskTarget::QuickNote => {
                    tools.note_text.push('\n');
                    tools.save_note();
                }
                InlineOskTarget::None => {}
            },
            OutgoingAction::UnmapSurface => {
                if !osk.shown {
                    curtain.app_menu.search_focused = false;
                    tools.note_focused = false;
                    *target = InlineOskTarget::None;
                }
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_curtain_effect(
    eff: CurtainEffect,
    inline_osk: &mut OskState,
    osk_target: &mut InlineOskTarget,
    tools: &mut CompanionTools,
    _osd: &mut BannerOsdView,
    dirty_tools: &mut bool,
    _dirty_osd: &mut bool,
    wl: &mut WaylandApp,
    lockscreen: &mut Lockscreen,
    lock_surfaces: &mut Vec<SurfaceId>,
    dirty_lock: &mut bool,
) {
    match eff {
        CurtainEffect::None => {}
        CurtainEffect::OpenOskForSearch => {
            crate::sys_state::request_gpu_boost(550);
            *osk_target = InlineOskTarget::AppSearch;
            inline_osk.direct_text_sink = true;
            inline_osk.enter_label = Some("打开");
            inline_osk.panel = PanelKind::None;
            inline_osk.show(true);
        }
        CurtainEffect::OpenOskForWifiPassword => {
            crate::sys_state::request_gpu_boost(550);
            *osk_target = InlineOskTarget::WifiPassword;
            inline_osk.direct_text_sink = true;
            inline_osk.enter_label = Some("连接");
            inline_osk.panel = PanelKind::None;
            inline_osk.show(true);
        }
        CurtainEffect::HideInlineOsk => {
            if *osk_target == InlineOskTarget::WifiPassword {
                inline_osk.hide(false);
                *osk_target = InlineOskTarget::None;
            }
        }
        CurtainEffect::ToggleOsk => {
            crate::sys_state::request_gpu_boost(550);
            call_osk_dbus_async("toggle");
        }
        CurtainEffect::OpenOskClipboard => {
            crate::sys_state::request_gpu_boost(550);
            call_osk_dbus_async("show");
        }
        CurtainEffect::StartScreenshotAnnot => {
            inline_osk.hide(false);
            *osk_target = InlineOskTarget::None;
            tools.start_screenshot_annotation();
            *dirty_tools = true;
        }
        CurtainEffect::ToggleQuickNote => {
            crate::sys_state::request_gpu_boost(550);
            inline_osk.hide(false);
            *osk_target = InlineOskTarget::None;
            tools.note_open = !tools.note_open;
            if tools.note_open {
                tools.note_focused = true;
                *osk_target = InlineOskTarget::QuickNote;
                inline_osk.direct_text_sink = true;
                inline_osk.enter_label = Some("换行");
                inline_osk.show(true);
            }
            *dirty_tools = true;
        }
        CurtainEffect::LockScreen => {
            crate::sys_state::request_gpu_boost(650);
            inline_osk.hide(false);
            *osk_target = InlineOskTarget::None;
            if !lockscreen.locked {
                if let Ok(ids) = wl.lock_session() {
                    *lock_surfaces = ids;
                    lockscreen.enter_lock(false);
                    *dirty_lock = true;
                }
            }
        }
        CurtainEffect::StartStandby => {
            crate::sys_state::request_gpu_boost(650);
            inline_osk.hide(false);
            *osk_target = InlineOskTarget::None;
            if !lockscreen.locked {
                if let Ok(ids) = wl.lock_session() {
                    *lock_surfaces = ids;
                    lockscreen.enter_lock(true);
                    *dirty_lock = true;
                }
            } else {
                lockscreen.switch_to_standby();
                *dirty_lock = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_consecutive_volume_and_brightness_steps_never_snap_back_to_first_step() {
        let mut init = SysSnapshot::default();
        init.volume_pct = 40;
        init.brightness_pct = 16;
        let sys_shared = Arc::new(Mutex::new(init.clone()));
        let mut sys_local = init;
        let mut bar = StatusBar::new();

        // Simulate 6 rapid Volume +5 key presses interleaved with main-loop sys_shared sync
        for step in 1..=6 {
            let next_v = (sys_local.volume_pct as i16 + 5).clamp(0, 100) as u8;
            sys_local.volume_pct = next_v;
            sync_optimistic_sys(&sys_shared, &sys_local);
            bar.show_volume_osd(next_v, sys_local.muted);

            let latest_sys = sys_shared.lock().unwrap().clone();
            if latest_sys != sys_local {
                sys_local = latest_sys;
            }
            assert_eq!(sys_local.volume_pct, 40 + step * 5);
        }
        assert_eq!(sys_local.volume_pct, 70);
        assert!((bar.osd_val.target - 0.70).abs() < 1e-4);

        // Simulate 6 rapid Brightness +6 key presses interleaved with main-loop sys_shared sync
        for step in 1..=6 {
            let next_b = (sys_local.brightness_pct as i16 + 6).clamp(1, 100) as u8;
            sys_local.brightness_pct = next_b;
            sync_optimistic_sys(&sys_shared, &sys_local);
            bar.show_brightness_osd(next_b);

            let latest_sys = sys_shared.lock().unwrap().clone();
            if latest_sys != sys_local {
                sys_local = latest_sys;
            }
            assert_eq!(sys_local.brightness_pct, 16 + step * 6);
        }
        assert_eq!(sys_local.brightness_pct, 52);
        assert!((bar.osd_val.target - 0.52).abs() < 1e-4);
    }
}
