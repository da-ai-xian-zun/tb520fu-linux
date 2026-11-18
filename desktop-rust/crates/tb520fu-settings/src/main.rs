// SPDX-License-Identifier: GPL-3.0-only
mod perf;
mod search;

use std::cell::RefCell;
use std::fs::File;
use std::io::BufWriter;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tokio::sync::{mpsc, oneshot};

use tb520fu_sys::audio::{self, AudioState};
use tb520fu_sys::bluetooth::{self, BtState, PairingPrompt};
use tb520fu_sys::daemon::{self, BatterySnapshot, DaemonStatus, FormattedSuspendRow};
use tb520fu_sys::default_apps::{self, AppRole, DefaultAppsSnapshot};
use tb520fu_sys::niri_cfg::{self, NiriDisplaySnapshot, NiriManagedPrefs};
use tb520fu_sys::osk_ime::{self, OskImeState, IME_TRIGGER_LABELS, OSK_SOUND_IDS};
use tb520fu_sys::power_idle::{self, IdleConfig};
use tb520fu_sys::shell_prefs::{self, ShellPrefsDto, ACCENT_IDS, ACCENT_LABELS};
use tb520fu_sys::system_info::{self, SystemSummary, COMMON_TIMEZONES};
use tb520fu_sys::wallpaper::{self, LocalWallpaperItem, WALLPAPER_PRESETS};
use tb520fu_sys::wifi::{self, WifiState};

#[allow(deprecated)]
mod slint_ui {
    slint::slint! {
        export { MainWindow, WifiRow, BtRow, AudioRow, SuspendRow, NotifAppRow, SearchHit } from "ui/app.slint";
    }
}
use slint_ui::{AudioRow, BtRow, MainWindow, NotifAppRow, SearchHit, SuspendRow, WifiRow};

const APP_ID: &str = "fyi.tb520fu.Settings";
const DBUS_NAME: &str = "fyi.tb520fu.Settings";
const DBUS_PATH: &str = "/Settings";

const PRESET_CHOICES: [&str; 3] = ["[33,50,67,100]", "[25,50,75,100]", "[50,100]"];
const PRESET_LABELS: [&str; 3] = [
    "33% · 50% · 67% · 100%",
    "25% · 50% · 75% · 100%",
    "50% · 100%",
];
const SCALE_CHOICES: [f64; 4] = [1.5, 1.75, 2.0, 2.25];
const LOCK_SECS_CHOICES: [i32; 5] = [60, 120, 300, 600, -1];
const LOCK_LABELS: [&str; 5] = ["1 分钟", "2 分钟", "5 分钟", "10 分钟", "永不"];
const SUSPEND_SECS_CHOICES: [i32; 4] = [0, 300, 600, 1800];
const SUSPEND_LABELS: [&str; 4] = ["不自动休眠", "5 分钟", "10 分钟", "30 分钟"];
const PROFILE_IDS: [&str; 3] = ["power-saver", "balanced", "performance"];
const LANG_IDS: [&str; 2] = ["zh_CN.UTF-8", "en_US.UTF-8"];
const LANG_LABELS: [&str; 2] = ["简体中文 (zh_CN.UTF-8)", "English (en_US.UTF-8)"];

const DND_START_CHOICES: [&str; 4] = ["22:00", "23:00", "00:00", "12:30"];
const DND_END_CHOICES: [&str; 4] = ["07:00", "07:30", "08:30", "14:00"];
const BANNER_TIMEOUT_MS: [u32; 3] = [3000, 4200, 8000];
const LOCK_NOTIF_IDS: [&str; 3] = ["full", "redact", "hide"];
const LOCK_NOTIF_LABELS: [&str; 3] = [
    "显示完整内容",
    "隐藏通知内容",
    "不显示通知",
];

enum BgCmd {
    PageChanged(String),
    SetShellPref {
        key: String,
        val_json: String,
        toast: Option<String>,
    },
    ToggleAppNotif {
        app_name: String,
        enabled: bool,
    },
    SendTestNotif,
    SetBrightness(u8),
    SetAutobright(bool),
    SetHbm(u8),
    UpdateNiri(Box<dyn FnOnce(&mut NiriManagedPrefs) + Send>),
    ShowHotkeyOverlay,
    SetOskField {
        key: String,
        val: serde_json::Value,
        toast: Option<String>,
    },
    SetImeTrigger(usize),
    SetDefaultApp {
        role: AppRole,
        idx: usize,
    },
    ChangePassword {
        cur: String,
        new1: String,
        new2: String,
    },
    LaunchStorageAnalyzer,
    WifiToggle(bool),
    AirplaneToggle(bool),
    WifiRescan,
    WifiConnect {
        ssid: String,
        pw: Option<String>,
    },
    WifiForget(String),
    BtToggle(bool),
    BtScan(bool),
    BtConnectToggle {
        path: String,
        connected: bool,
    },
    BtForget(String),
    BtPair {
        path: String,
        name: String,
    },
    AudioOutVol(u8),
    AudioOutMute(bool),
    AudioMicVol(u8),
    AudioMicMute(bool),
    AudioSelectNode(u32),
    SetBypass(bool),
    SetPowerProfile(String),
    SetIdleLock(i32),
    SetIdleSuspend(i32),
    SetNtp(bool),
    SetTimezone(String),
    SetLocale(String),
    SetHostname(String),
    RebootAndroid,
}

#[derive(Clone)]
struct FullSnapshot {
    daemon: Option<DaemonStatus>,
    bat: BatterySnapshot,
    shell_prefs: Option<ShellPrefsDto>,
    wifi: WifiState,
    bt: BtState,
    audio: AudioState,
    power_profile: String,
    idle: IdleConfig,
    suspend_logs: Vec<FormattedSuspendRow>,
    niri_prefs: NiriManagedPrefs,
    niri_out: NiriDisplaySnapshot,
    osk_ime: OskImeState,
    default_apps: DefaultAppsSnapshot,
    sys: SystemSummary,
    boot_switch_ready: bool,
}

struct SingleInstanceIface {
    ui_weak: slint::Weak<MainWindow>,
    cmd_tx: mpsc::UnboundedSender<BgCmd>,
}

#[zbus::interface(name = "fyi.tb520fu.Settings1")]
impl SingleInstanceIface {
    #[zbus(name = "OpenPage")]
    fn open_page(&self, page: String) {
        let norm = search::normalize_page_id(&page).to_string();
        let _ = self.cmd_tx.send(BgCmd::PageChanged(norm.clone()));
        let weak = self.ui_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_switch_to_page(norm.into());
            }
        });
        niri_cfg::focus_window_by_app_id(APP_ID);
    }
}

fn try_delegate_to_existing_instance(initial_page: Option<&str>) -> bool {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    rt.block_on(async move {
        let Ok(conn) = zbus::Connection::session().await else {
            return false;
        };
        let page = initial_page.unwrap_or("display");
        let ok = conn
            .call_method(
                Some(DBUS_NAME),
                DBUS_PATH,
                Some("fyi.tb520fu.Settings1"),
                "OpenPage",
                &(page,),
            )
            .await
            .is_ok();
        if ok {
            niri_cfg::focus_window_by_app_id(APP_ID);
        }
        ok
    })
}

fn parse_cli_page() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--page" || a == "-p" {
            return args.next().map(|v| search::normalize_page_id(&v).to_string());
        }
        if let Some(v) = a.strip_prefix("--page=") {
            return Some(search::normalize_page_id(v).to_string());
        }
    }
    None
}

fn show_toast(ui_weak: &slint::Weak<MainWindow>, msg: impl Into<String>) {
    let text: SharedString = msg.into().into();
    let w1 = ui_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = w1.upgrade() {
            ui.set_toast_text(text.clone());
            let w2 = ui.as_weak();
            let expected = text;
            slint::Timer::single_shot(Duration::from_millis(2200), move || {
                if let Some(u) = w2.upgrade() {
                    if u.get_toast_text() == expected {
                        u.set_toast_text(SharedString::default());
                    }
                }
            });
        }
    });
}

fn apply_shell_prefs_to_ui(ui: &MainWindow, prefs: &ShellPrefsDto) {
    let is_light = prefs.is_light_skin();
    ui.set_light_skin(is_light);
    ui.set_skin_idx(if is_light { 1 } else { 0 });

    let acc_idx = prefs.accent_idx();
    ui.set_accent_idx(acc_idx as i32);
    ui.set_accent_label(ACCENT_LABELS[acc_idx].into());

    let wp_preset = wallpaper::preset_index(&prefs.wallpaper)
        .map(|i| i as i32)
        .unwrap_or(-1);
    ui.set_wallpaper_preset_idx(wp_preset);
    ui.set_wallpaper_label(wallpaper::describe_wallpaper_spec(&prefs.wallpaper).into());

    ui.set_night_light(prefs.night_light);
    ui.set_reading_mode(prefs.reading_mode);
    ui.set_high_refresh(prefs.high_refresh_144);
    ui.set_auto_rotate(prefs.auto_rotate);
    ui.set_pen_corners(prefs.pen_corners);
    ui.set_pen_orb(prefs.pen_orb);

    let p_idx = match prefs.presets_csv().as_str() {
        "25,50,75,100" => 1,
        "50,100" => 2,
        _ => 0,
    };
    ui.set_presets_idx(p_idx as i32);
    ui.set_presets_label(PRESET_LABELS[p_idx].into());

    // Notifications & Focus
    ui.set_dnd(prefs.dnd);
    ui.set_dnd_fullscreen(prefs.dnd_fullscreen);
    ui.set_dnd_schedule_enabled(prefs.dnd_schedule_enabled);

    let ds_idx = DND_START_CHOICES
        .iter()
        .position(|&s| s == prefs.dnd_start.trim())
        .unwrap_or(1);
    ui.set_dnd_start_idx(ds_idx as i32);
    ui.set_dnd_start_label(prefs.dnd_start.clone().into());

    let de_idx = DND_END_CHOICES
        .iter()
        .position(|&s| s == prefs.dnd_end.trim())
        .unwrap_or(1);
    ui.set_dnd_end_idx(de_idx as i32);
    ui.set_dnd_end_label(prefs.dnd_end.clone().into());

    let bt_idx = if prefs.banner_timeout_ms <= 3400 {
        0
    } else if prefs.banner_timeout_ms >= 6500 {
        2
    } else {
        1
    };
    ui.set_banner_timeout_idx(bt_idx);

    let ln_idx = LOCK_NOTIF_IDS
        .iter()
        .position(|&m| m == prefs.lock_notif_mode.trim())
        .unwrap_or(0);
    ui.set_lock_notif_idx(ln_idx as i32);
    ui.set_lock_notif_label(LOCK_NOTIF_LABELS[ln_idx].into());

    ui.set_sum_notif(
        if prefs.dnd {
            "勿扰中".into()
        } else if prefs.dnd_schedule_enabled {
            "定时勿扰".into()
        } else {
            "开启".into()
        },
    );

    let app_rows: Vec<NotifAppRow> = prefs
        .seen_notif_apps
        .iter()
        .map(|name| NotifAppRow {
            name: name.clone().into(),
            enabled: !prefs.is_app_muted(name),
        })
        .collect();
    ui.set_notif_apps(ModelRc::new(VecModel::from(app_rows)));
}

fn apply_snapshot_to_ui(ui: &MainWindow, snap: &FullSnapshot) {
    ui.set_shell_online(snap.shell_prefs.is_some());
    let prefs = snap.shell_prefs.clone().unwrap_or_default();
    apply_shell_prefs_to_ui(ui, &prefs);

    if let Some(d) = &snap.daemon {
        ui.set_brightness(d.brightness_pct as i32);
        ui.set_autobright(d.autobright);
        ui.set_hbm_idx(d.hbm.min(2) as i32);
    }
    ui.set_dsi_hdr(snap.niri_prefs.dsi_hdr);

    // Display scale & external monitor
    let cur_scale = snap.niri_out.dsi_scale;
    let s_idx = SCALE_CHOICES
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            ((*a - cur_scale).abs())
                .partial_cmp(&((*b - cur_scale).abs()))
                .unwrap()
        })
        .map(|(i, _)| i as i32)
        .unwrap_or(2);
    ui.set_scale_idx(s_idx);
    ui.set_scale_value_text(format!("{:.2}×", cur_scale).into());
    let lw = (2944.0 / cur_scale).round() as i32;
    let lh = (1840.0 / cur_scale).round() as i32;
    ui.set_logical_res_text(format!("{lw}×{lh}").into());
    ui.set_sum_display(
        format!(
            "{:.2}× · {}",
            cur_scale,
            if prefs.high_refresh_144 { "120Hz" } else { "60Hz" }
        )
        .replace(".00", "")
        .into(),
    );

    if let Some(ext) = &snap.niri_out.external {
        ui.set_ext_connected(true);
        ui.set_ext_name(ext.name.clone().into());
        ui.set_ext_enabled(ext.enabled);
        let modes: Vec<SharedString> = ext.available_modes.iter().map(|m| m.into()).collect();
        let m_idx = ext
            .available_modes
            .iter()
            .position(|m| m == &ext.current_mode)
            .unwrap_or(0);
        let pos_idx = match ext.pos.as_str() {
            "left" => 0,
            "above" => 1,
            _ => 2,
        };
        ui.set_ext_modes(ModelRc::new(VecModel::from(modes)));
        ui.set_ext_mode_idx(m_idx as i32);
        ui.set_ext_mode_label(ext.current_mode.clone().into());
        ui.set_ext_pos_idx(pos_idx);
    } else {
        ui.set_ext_connected(false);
    }

    // Wi-Fi & Airplane Mode
    let airplane = !snap.wifi.enabled && !snap.bt.enabled;
    ui.set_airplane_mode(airplane);
    ui.set_wired_summary(snap.wifi.wired_summary.clone().unwrap_or_default().into());
    ui.set_wifi_enabled(snap.wifi.enabled);
    if airplane {
        ui.set_sum_wifi("飞行模式".into());
        ui.set_wifi_has_active(false);
    } else if !snap.wifi.enabled {
        ui.set_sum_wifi("已关闭".into());
        ui.set_wifi_has_active(false);
    } else if let Some(act) = &snap.wifi.active {
        ui.set_sum_wifi(act.ssid.clone().into());
        ui.set_wifi_has_active(true);
        ui.set_wifi_active_ssid(act.ssid.clone().into());
        ui.set_wifi_active_detail(
            format!(
                "{} · IP {} · 网关 {} · MAC {}",
                act.security,
                if act.ip4.is_empty() { "获取中" } else { &act.ip4 },
                if act.gateway.is_empty() { "—" } else { &act.gateway },
                if act.mac.is_empty() { "—" } else { &act.mac }
            )
            .into(),
        );
    } else {
        ui.set_sum_wifi("未连接".into());
        ui.set_wifi_has_active(false);
    }
    let wifi_rows: Vec<WifiRow> = snap
        .wifi
        .networks
        .iter()
        .map(|ap| WifiRow {
            ssid: ap.ssid.clone().into(),
            subtitle: if ap.saved {
                format!("{} · 已保存", ap.security_label).into()
            } else {
                ap.security_label.clone().into()
            },
            strength: ap.strength as i32,
            secure: ap.secure,
            saved: ap.saved,
        })
        .collect();
    ui.set_wifi_networks(ModelRc::new(VecModel::from(wifi_rows)));

    // Bluetooth
    ui.set_bt_enabled(snap.bt.enabled);
    ui.set_bt_discovering(snap.bt.discovering);
    let conn_cnt = snap.bt.paired.iter().filter(|d| d.connected).count();
    ui.set_sum_bt(
        if !snap.bt.enabled {
            "已关闭".into()
        } else if conn_cnt > 0 {
            format!("{conn_cnt} 台已连").into()
        } else {
            "已开启".into()
        },
    );
    let paired_rows: Vec<BtRow> = snap
        .bt
        .paired
        .iter()
        .map(|d| BtRow {
            path: d.path.clone().into(),
            mac: d.mac.clone().into(),
            name: d.name.clone().into(),
            subtitle: format!(
                "{} · {}",
                if d.connected { "已连接" } else { "未连接" },
                d.kind_label
            )
            .into(),
            is_pen: d.kind_label == "手写笔",
            connected: d.connected,
            has_battery: d.battery.is_some(),
            battery_text: d.battery.map(|b| format!("电量 {b}%")).unwrap_or_default().into(),
        })
        .collect();
    let disc_rows: Vec<BtRow> = snap
        .bt
        .discovered
        .iter()
        .map(|d| BtRow {
            path: d.path.clone().into(),
            mac: d.mac.clone().into(),
            name: d.name.clone().into(),
            subtitle: format!("{} · 点击发起配对", d.kind_label).into(),
            is_pen: d.kind_label == "手写笔",
            connected: false,
            has_battery: false,
            battery_text: SharedString::default(),
        })
        .collect();
    ui.set_bt_paired(ModelRc::new(VecModel::from(paired_rows)));
    ui.set_bt_discovered(ModelRc::new(VecModel::from(disc_rows)));

    // Sound
    ui.set_out_vol(snap.audio.out_vol_pct as i32);
    ui.set_out_muted(snap.audio.out_muted);
    ui.set_mic_vol(snap.audio.mic_vol_pct as i32);
    ui.set_mic_muted(snap.audio.mic_muted);
    ui.set_sum_sound(
        if snap.audio.out_muted {
            "已静音".into()
        } else {
            format!("{}%", snap.audio.out_vol_pct).into()
        },
    );
    let sink_rows: Vec<AudioRow> = snap
        .audio
        .sinks
        .iter()
        .map(|n| {
            let nl = n.name.to_ascii_lowercase();
            let is_bt = nl.contains("buds") || nl.contains("inzone") || nl.contains("airpods") || nl.contains("bluez");
            let subtitle = if nl.contains("built-in") {
                "内置六扬声器"
            } else if is_bt {
                "蓝牙音频"
            } else {
                "音频输出"
            };
            AudioRow {
                id: n.id as i32,
                name: n.name.clone().into(),
                subtitle: subtitle.into(),
                is_bt,
                active: n.is_default,
            }
        })
        .collect();
    let src_rows: Vec<AudioRow> = snap
        .audio
        .sources
        .iter()
        .map(|n| {
            let nl = n.name.to_ascii_lowercase();
            let subtitle = if nl.contains("built-in") {
                "内置双麦克风"
            } else {
                "外接麦克风"
            };
            AudioRow {
                id: n.id as i32,
                name: n.name.clone().into(),
                subtitle: subtitle.into(),
                is_bt: false,
                active: n.is_default,
            }
        })
        .collect();
    ui.set_audio_sinks(ModelRc::new(VecModel::from(sink_rows)));
    ui.set_audio_sources(ModelRc::new(VecModel::from(src_rows)));

    // Power & Battery
    let bypass_en = snap
        .daemon
        .as_ref()
        .map(|d| d.charge_limit <= 90)
        .unwrap_or(true);
    ui.set_bypass_enabled(bypass_en);
    ui.set_bypass_active(snap.bat.bypass_active);
    ui.set_battery_pct(snap.bat.percent as i32);
    let pwr_str = if snap.bat.bypass_active {
        "旁路直供 · 0.0W".to_string()
    } else if snap.bat.charging {
        format!("充电中 · +{:.1}W", snap.bat.watts)
    } else if snap.bat.usb_online {
        format!("外接供电 · {:.1}W", snap.bat.watts)
    } else {
        format!("使用电池 · -{:.1}W", snap.bat.watts)
    };
    ui.set_battery_headline(pwr_str.into());
    ui.set_battery_detail(
        if snap.bat.bypass_active {
            format!(
                "已达 90%，电源直接供电 · 温度 {:.1}°C · 健康度 {}%",
                snap.bat.temp_c, snap.bat.health_pct
            )
        } else {
            format!(
                "温度 {:.1}°C · 健康度 {}%",
                snap.bat.temp_c, snap.bat.health_pct
            )
        }
        .into(),
    );
    ui.set_device_subtitle(
        format!(
            "{}% · {}",
            snap.bat.percent,
            if snap.bat.bypass_active {
                "旁路直供"
            } else if snap.bat.charging {
                "充电中"
            } else {
                "电池供电"
            }
        )
        .into(),
    );
    ui.set_sum_power(
        if snap.bat.bypass_active {
            format!("{}% · 旁路", snap.bat.percent).into()
        } else {
            format!("{}%", snap.bat.percent).into()
        },
    );

    let prof_idx = PROFILE_IDS
        .iter()
        .position(|p| *p == snap.power_profile)
        .unwrap_or(1) as i32;
    ui.set_profile_idx(prof_idx);
    let lock_idx = LOCK_SECS_CHOICES
        .iter()
        .position(|&s| s == snap.idle.lock_secs)
        .unwrap_or(2);
    let sus_idx = SUSPEND_SECS_CHOICES
        .iter()
        .position(|&s| s == snap.idle.suspend_secs)
        .unwrap_or(0);
    ui.set_lock_idx(lock_idx as i32);
    ui.set_lock_label(LOCK_LABELS[lock_idx].into());
    ui.set_suspend_idx(sus_idx as i32);
    ui.set_suspend_label(SUSPEND_LABELS[sus_idx].into());

    let sus_rows: Vec<SuspendRow> = snap
        .suspend_logs
        .iter()
        .map(|r| SuspendRow {
            started: r.started_at.clone().into(),
            duration: r.duration.clone().into(),
            delta_cap: r.delta_cap.clone().into(),
            avg_power: r.avg_power.clone().into(),
        })
        .collect();
    ui.set_suspend_logs(ModelRc::new(VecModel::from(sus_rows)));

    // Pen, Touchpad, OSK & Keyboard
    let ble_pen_bat = snap
        .bt
        .paired
        .iter()
        .find(|d| d.kind_label == "手写笔")
        .and_then(|d| d.battery);
    if let Some(d) = &snap.daemon {
        let lvl = if d.pen_docked && d.pen_level >= 0 {
            Some(d.pen_level as u8)
        } else {
            ble_pen_bat
        };
        if d.pen_docked {
            ui.set_pen_status_text(
                if d.pen_charging {
                    "磁吸充电中".into()
                } else {
                    "已磁吸就绪".into()
                },
            );
        } else {
            ui.set_pen_status_text("使用中".into());
        }
        if let Some(p) = lvl {
            ui.set_pen_badge_text(format!("电量 {p}%").into());
            ui.set_sum_pen(format!("笔 {p}%").into());
        } else {
            ui.set_pen_badge_text(if d.pen_docked { "已吸附".into() } else { "就绪".into() });
            ui.set_sum_pen(if d.pen_docked { "已吸附".into() } else { "就绪".into() });
        }
    }
    ui.set_tp_tap(snap.niri_prefs.tp_tap);
    ui.set_tp_natural(snap.niri_prefs.tp_natural_scroll);
    ui.set_tp_speed(snap.niri_prefs.tp_speed_pct as i32);
    ui.set_kb_caps_ctrl(snap.niri_prefs.kb_caps_ctrl);

    ui.set_osk_auto_popup(snap.osk_ime.auto_popup);
    ui.set_osk_mode_idx(snap.osk_ime.mode_idx as i32);
    ui.set_osk_sound_on(snap.osk_ime.sound_on);
    ui.set_osk_sound_idx(snap.osk_ime.sound_idx as i32);
    let ime_idx = snap.osk_ime.ime_trigger_idx.min(IME_TRIGGER_LABELS.len() - 1);
    ui.set_ime_trigger_idx(ime_idx as i32);
    ui.set_ime_trigger_label(IME_TRIGGER_LABELS[ime_idx].into());

    // Default Apps
    let to_names = |list: &[default_apps::DesktopAppChoice]| -> ModelRc<SharedString> {
        let v: Vec<SharedString> = list.iter().map(|c| c.name.clone().into()).collect();
        ModelRc::new(VecModel::from(v))
    };
    ui.set_browser_app_names(to_names(&snap.default_apps.browsers));
    ui.set_browser_app_idx(snap.default_apps.browser_idx as i32);
    ui.set_default_browser_label(snap.default_apps.browser_label.clone().into());

    ui.set_pdf_app_names(to_names(&snap.default_apps.pdf_apps));
    ui.set_pdf_app_idx(snap.default_apps.pdf_idx as i32);
    ui.set_default_pdf_label(snap.default_apps.pdf_label.clone().into());

    ui.set_image_app_names(to_names(&snap.default_apps.image_apps));
    ui.set_image_app_idx(snap.default_apps.image_idx as i32);
    ui.set_default_image_label(snap.default_apps.image_label.clone().into());

    ui.set_media_app_names(to_names(&snap.default_apps.media_apps));
    ui.set_media_app_idx(snap.default_apps.media_idx as i32);
    ui.set_default_media_label(snap.default_apps.media_label.clone().into());

    // System & About
    ui.set_ntp_enabled(snap.sys.ntp_enabled);
    ui.set_current_tz(snap.sys.timezone.clone().into());
    let mut tzs: Vec<String> = COMMON_TIMEZONES.iter().map(|s| (*s).to_string()).collect();
    if !tzs.iter().any(|t| t == &snap.sys.timezone) {
        tzs.push(snap.sys.timezone.clone());
    }
    let tz_idx = tzs
        .iter()
        .position(|t| t == &snap.sys.timezone)
        .unwrap_or(0) as i32;
    let tz_models: Vec<SharedString> = tzs.iter().map(|s| s.into()).collect();
    ui.set_tz_options(ModelRc::new(VecModel::from(tz_models)));
    ui.set_tz_idx(tz_idx);
    ui.set_sum_sys(
        snap.sys
            .timezone
            .rsplit('/')
            .next()
            .unwrap_or(&snap.sys.timezone)
            .into(),
    );

    let l_idx = if snap.sys.lang.starts_with("en") { 1 } else { 0 };
    ui.set_lang_idx(l_idx as i32);
    ui.set_lang_label(LANG_LABELS[l_idx].into());
    ui.set_current_user(snap.sys.current_user.clone().into());
    ui.set_hostname(snap.sys.hostname.clone().into());
    ui.set_kernel(snap.sys.kernel.clone().into());
    ui.set_os_pretty(snap.sys.os_pretty.clone().into());
    ui.set_ram_gb(snap.sys.ram_gb.clone().into());
    ui.set_storage_summary(snap.sys.storage_summary.clone().into());
    ui.set_storage_tool_label(snap.sys.storage_tool_label.clone().into());
    ui.set_boot_switch_ready(snap.boot_switch_ready);
}

async fn collect_full_snapshot(
    sys_bus: Option<&zbus::Connection>,
    ses_bus: Option<&zbus::Connection>,
) -> FullSnapshot {
    let daemon_st = daemon::get_status().ok();
    let charge_lim = daemon_st.as_ref().map(|d| d.charge_limit).unwrap_or(90);
    let pen_docked_lvl = daemon_st
        .as_ref()
        .filter(|d| d.pen_docked && d.pen_level >= 0)
        .map(|d| d.pen_level.min(100) as u8);
    let bat = daemon::read_battery_snapshot(charge_lim);

    let shell_prefs = match ses_bus {
        Some(c) => shell_prefs::get_shell_prefs(c).await,
        None => None,
    };
    let wifi = match sys_bus {
        Some(c) => wifi::get_wifi_state(c).await,
        None => WifiState::default(),
    };
    let bt = match sys_bus {
        Some(c) => bluetooth::get_bt_state(c, pen_docked_lvl).await,
        None => BtState::default(),
    };
    let power_profile = match sys_bus {
        Some(c) => power_idle::get_power_profile(c).await,
        None => "balanced".into(),
    };
    let sys = match sys_bus {
        Some(c) => system_info::get_system_summary(c).await,
        None => SystemSummary::default(),
    };

    let audio = audio::get_audio_state();
    let idle = power_idle::load_idle_config();
    let suspend_logs = daemon::read_suspend_logs(5);
    let niri_prefs = niri_cfg::load_managed_prefs();
    let niri_out = niri_cfg::query_niri_outputs();
    let osk_ime = osk_ime::load_osk_ime_state();
    let default_apps = default_apps::query_default_apps();
    let boot_switch_ready =
        std::path::Path::new("/usr/local/sbin/reboot-to-android").exists() || daemon_st.is_some();

    FullSnapshot {
        daemon: daemon_st,
        bat,
        shell_prefs,
        wifi,
        bt,
        audio,
        power_profile,
        idle,
        suspend_logs,
        niri_prefs,
        niri_out,
        osk_ime,
        default_apps,
        sys,
        boot_switch_ready,
    }
}

fn spawn_backend_worker(
    ui_weak: slint::Weak<MainWindow>,
    mut cmd_rx: mpsc::UnboundedReceiver<BgCmd>,
    cmd_tx_for_dbus: mpsc::UnboundedSender<BgCmd>,
    pair_reply_slot: Arc<Mutex<Option<oneshot::Sender<bool>>>>,
    initial_page: String,
) {
    std::thread::Builder::new()
        .name("tb520fu-settings-bg".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            else {
                return;
            };

            rt.block_on(async move {
                let sys_bus = zbus::Connection::system().await.ok();
                let ses_bus = zbus::Connection::session().await.ok();

                if let Some(sb) = &ses_bus {
                    let iface = SingleInstanceIface {
                        ui_weak: ui_weak.clone(),
                        cmd_tx: cmd_tx_for_dbus,
                    };
                    let _ = sb.object_server().at(DBUS_PATH, iface).await;
                    let _ = sb.request_name(DBUS_NAME).await;

                    let sb_watch = sb.clone();
                    let w_prefs = ui_weak.clone();
                    tokio::spawn(async move {
                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .interface("fyi.tb520fu.Shell")
                            .and_then(|b| b.member("PrefsChanged"))
                            .map(|b| b.build())
                        else {
                            return;
                        };
                        let Ok(mut stream) =
                            zbus::MessageStream::for_match_rule(rule, &sb_watch, Some(16)).await
                        else {
                            return;
                        };
                        while let Some(item) = std::future::poll_fn(|cx| {
                            zbus::export::futures_core::Stream::poll_next(
                                std::pin::Pin::new(&mut stream),
                                cx,
                            )
                        })
                        .await
                        {
                            if item.is_ok() {
                                if let Some(prefs) = shell_prefs::get_shell_prefs(&sb_watch).await {
                                    let w = w_prefs.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = w.upgrade() {
                                            apply_shell_prefs_to_ui(&ui, &prefs);
                                        }
                                    });
                                }
                            }
                        }
                    });
                }

                let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel::<PairingPrompt>();
                let mut agent_registered = false;
                if initial_page == "bt" {
                    if let Some(b) = &sys_bus {
                        agent_registered =
                            bluetooth::register_pairing_agent(b, prompt_tx.clone()).await;
                    }
                }

                let snap0 = collect_full_snapshot(sys_bus.as_ref(), ses_bus.as_ref()).await;
                {
                    let w = ui_weak.clone();
                    let s = snap0.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = w.upgrade() {
                            apply_snapshot_to_ui(&ui, &s);
                        }
                    });
                }

                let mut niri_state = snap0.niri_prefs;
                let mut idle_state = snap0.idle;
                let mut def_apps_state = snap0.default_apps;
                let mut tick = tokio::time::interval(Duration::from_millis(2500));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    tokio::select! {
                        _ = tick.tick() => {
                            let snap = collect_full_snapshot(sys_bus.as_ref(), ses_bus.as_ref()).await;
                            def_apps_state = snap.default_apps.clone();
                            let w = ui_weak.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = w.upgrade() {
                                    apply_snapshot_to_ui(&ui, &snap);
                                }
                            });
                        }
                        Some(prompt) = prompt_rx.recv() => {
                            *pair_reply_slot.lock().unwrap() = Some(prompt.responder);
                            let w = ui_weak.clone();
                            let dev_name = prompt.device_name;
                            let code = prompt.passkey;
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = w.upgrade() {
                                    ui.set_modal_kind("bt-pair".into());
                                    ui.set_modal_title(format!("配对 “{dev_name}”").into());
                                    ui.set_modal_subtitle("请确认蓝牙外设配对码一致后点击“确认配对”".into());
                                    ui.set_modal_passkey(code.into());
                                }
                            });
                        }
                        maybe_cmd = cmd_rx.recv() => {
                            let Some(cmd) = maybe_cmd else { break; };
                            let mut need_refresh = true;
                            match cmd {
                                BgCmd::PageChanged(p) => {
                                    if p == "bt" && !agent_registered {
                                        if let Some(b) = &sys_bus {
                                            agent_registered = bluetooth::register_pairing_agent(b, prompt_tx.clone()).await;
                                        }
                                    } else if p != "bt" && agent_registered {
                                        if let Some(b) = &sys_bus {
                                            bluetooth::set_bt_discovery(b, false).await;
                                            bluetooth::unregister_pairing_agent(b).await;
                                        }
                                        agent_registered = false;
                                    }
                                    need_refresh = p == "wifi" || p == "bt" || p == "sys" || p == "notif";
                                }
                                BgCmd::SetShellPref { key, val_json, toast } => {
                                    if key == "skin" || key == "accent" {
                                        if let Ok(s) = serde_json::from_str::<String>(&val_json) {
                                            let _ = osk_ime::set_osk_field_and_reload(
                                                ses_bus.as_ref(),
                                                &key,
                                                serde_json::Value::String(s),
                                            )
                                            .await;
                                        }
                                    }
                                    if let Some(sb) = &ses_bus {
                                        if shell_prefs::set_shell_pref(sb, &key, &val_json).await {
                                            if let Some(msg) = toast {
                                                show_toast(&ui_weak, msg);
                                            }
                                        }
                                    }
                                }
                                BgCmd::ToggleAppNotif { app_name, enabled } => {
                                    if let Some(sb) = &ses_bus {
                                        if let Some(mut prefs) = shell_prefs::get_shell_prefs(sb).await {
                                            prefs.muted_notif_apps.retain(|m| !m.eq_ignore_ascii_case(&app_name) && m != &app_name);
                                            if !enabled {
                                                prefs.muted_notif_apps.push(app_name.clone());
                                            }
                                            if let Ok(js) = serde_json::to_string(&prefs.muted_notif_apps) {
                                                let _ = shell_prefs::set_shell_pref(sb, "muted_notif_apps", &js).await;
                                                show_toast(&ui_weak, if enabled {
                                                    format!("已允许 “{app_name}” 发送通知")
                                                } else {
                                                    format!("已静默屏蔽 “{app_name}” 的通知")
                                                });
                                            }
                                        }
                                    }
                                }
                                BgCmd::SendTestNotif => {
                                    if let Some(sb) = &ses_bus {
                                        if shell_prefs::send_test_notification(sb).await {
                                            show_toast(&ui_weak, "已发送预览通知");
                                        }
                                    }
                                }
                                BgCmd::SetBrightness(pct) => {
                                    let _ = daemon::set_brightness(pct);
                                    need_refresh = false;
                                }
                                BgCmd::SetAutobright(on) => {
                                    let _ = daemon::set_autobright(on);
                                }
                                BgCmd::SetHbm(lv) => {
                                    let _ = daemon::set_hbm(lv);
                                    show_toast(&ui_weak, match lv {
                                        0 => "户外高亮模式已关闭",
                                        1 => "户外高亮模式已开启",
                                        _ => "户外超高亮模式已开启",
                                    });
                                }
                                BgCmd::UpdateNiri(mutator) => {
                                    let out_snap = niri_cfg::query_niri_outputs();
                                    if let Some(ext) = &out_snap.external {
                                        niri_state.ext_name = Some(ext.name.clone());
                                        if niri_state.ext_mode.is_none() {
                                            niri_state.ext_mode = Some(ext.current_mode.clone());
                                        }
                                    }
                                    mutator(&mut niri_state);
                                    match niri_cfg::save_and_apply_niri_prefs(&niri_state) {
                                        Ok(()) => show_toast(&ui_weak, "设置已更新"),
                                        Err(e) => show_toast(&ui_weak, e),
                                    }
                                }
                                BgCmd::ShowHotkeyOverlay => {
                                    let _ = niri_cfg::show_hotkey_overlay();
                                    need_refresh = false;
                                }
                                BgCmd::SetOskField { key, val, toast } => {
                                    if osk_ime::set_osk_field_and_reload(ses_bus.as_ref(), &key, val).await {
                                        if let Some(msg) = toast {
                                            show_toast(&ui_weak, msg);
                                        }
                                    }
                                }
                                BgCmd::SetImeTrigger(idx) => {
                                    match osk_ime::save_fcitx_trigger(idx) {
                                        Ok(()) => show_toast(&ui_weak, format!("输入法切换键已设为 {}", IME_TRIGGER_LABELS[idx.min(2)])),
                                        Err(e) => show_toast(&ui_weak, e),
                                    }
                                }
                                BgCmd::SetDefaultApp { role, idx } => {
                                    let list = match role {
                                        AppRole::Browser => &def_apps_state.browsers,
                                        AppRole::Pdf => &def_apps_state.pdf_apps,
                                        AppRole::Image => &def_apps_state.image_apps,
                                        AppRole::Media => &def_apps_state.media_apps,
                                    };
                                    if let Some(choice) = list.get(idx) {
                                        let name = choice.name.clone();
                                        let did = choice.desktop_id.clone();
                                        match default_apps::set_default_app(role, &did) {
                                            Ok(()) => {
                                                def_apps_state = default_apps::query_default_apps();
                                                show_toast(&ui_weak, format!("默认打开方式已设为 {name}"));
                                            }
                                            Err(e) => show_toast(&ui_weak, e),
                                        }
                                    }
                                }
                                BgCmd::ChangePassword { cur, new1, new2 } => {
                                    if new1 != new2 {
                                        show_toast(&ui_weak, "两次输入的新密码不一致");
                                    } else {
                                        match system_info::change_user_password(&cur, &new1) {
                                            Ok(()) => show_toast(&ui_weak, "登录与锁屏密码已更新生效"),
                                            Err(e) => show_toast(&ui_weak, e),
                                        }
                                    }
                                    need_refresh = false;
                                }
                                BgCmd::LaunchStorageAnalyzer => {
                                    if !system_info::launch_storage_analyzer() {
                                        show_toast(&ui_weak, "未找到已安装的存储分析工具");
                                    }
                                    need_refresh = false;
                                }
                                BgCmd::WifiToggle(on) => {
                                    wifi::set_wifi_enabled(on);
                                    tokio::time::sleep(Duration::from_millis(350)).await;
                                }
                                BgCmd::AirplaneToggle(on) => {
                                    if let Some(b) = &sys_bus {
                                        wifi::set_airplane_mode(b, on).await;
                                    } else {
                                        wifi::set_wifi_enabled(!on);
                                    }
                                    show_toast(&ui_weak, if on { "已开启飞行模式" } else { "已关闭飞行模式，无线连接恢复中" });
                                    tokio::time::sleep(Duration::from_millis(450)).await;
                                }
                                BgCmd::WifiRescan => {
                                    let w = ui_weak.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = w.upgrade() { ui.set_wifi_scanning(true); }
                                    });
                                    wifi::trigger_wifi_scan();
                                    tokio::time::sleep(Duration::from_millis(1600)).await;
                                    let w2 = ui_weak.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = w2.upgrade() { ui.set_wifi_scanning(false); }
                                    });
                                    show_toast(&ui_weak, "Wi-Fi 网络列表已刷新");
                                }
                                BgCmd::WifiConnect { ssid, pw } => {
                                    show_toast(&ui_weak, format!("正在连接 {ssid}…"));
                                    wifi::connect_wifi(&ssid, pw.as_deref());
                                    tokio::time::sleep(Duration::from_millis(1800)).await;
                                }
                                BgCmd::WifiForget(ssid) => {
                                    if let Some(b) = &sys_bus {
                                        wifi::forget_wifi(b, &ssid).await;
                                    }
                                    show_toast(&ui_weak, format!("已忘记网络 {ssid}"));
                                }
                                BgCmd::BtToggle(on) => {
                                    if let Some(b) = &sys_bus {
                                        bluetooth::set_bt_enabled(b, on).await;
                                    }
                                    tokio::time::sleep(Duration::from_millis(300)).await;
                                }
                                BgCmd::BtScan(start) => {
                                    if let Some(b) = &sys_bus {
                                        bluetooth::set_bt_discovery(b, start).await;
                                    }
                                }
                                BgCmd::BtConnectToggle { path, connected } => {
                                    if let Some(b) = &sys_bus {
                                        bluetooth::toggle_bt_connection(b, &path, connected).await;
                                    }
                                }
                                BgCmd::BtForget(path) => {
                                    if let Some(b) = &sys_bus {
                                        bluetooth::forget_bt_device(b, &path).await;
                                    }
                                    show_toast(&ui_weak, "已取消配对设备");
                                }
                                BgCmd::BtPair { path, name } => {
                                    show_toast(&ui_weak, format!("正在配对 {name}…"));
                                    if let Some(b) = &sys_bus {
                                        let b_cl = b.clone();
                                        let w_cl = ui_weak.clone();
                                        tokio::spawn(async move {
                                            match bluetooth::pair_and_connect_bt(&b_cl, &path).await {
                                                Ok(()) => show_toast(&w_cl, format!("{name} 已配对并连接")),
                                                Err(e) => show_toast(&w_cl, e),
                                            }
                                        });
                                    }
                                }
                                BgCmd::AudioOutVol(v) => {
                                    audio::set_output_volume(v);
                                    need_refresh = false;
                                }
                                BgCmd::AudioOutMute(m) => audio::set_output_mute(m),
                                BgCmd::AudioMicVol(v) => {
                                    audio::set_mic_volume(v);
                                    need_refresh = false;
                                }
                                BgCmd::AudioMicMute(m) => audio::set_mic_mute(m),
                                BgCmd::AudioSelectNode(id) => {
                                    audio::set_default_node(id);
                                    show_toast(&ui_weak, "音频设备已切换");
                                }
                                BgCmd::SetBypass(on) => {
                                    let _ = daemon::set_bypass_charge(on);
                                    show_toast(&ui_weak, if on { "90% 旁路供电保护已开启" } else { "90% 旁路供电保护已关闭" });
                                }
                                BgCmd::SetPowerProfile(p) => {
                                    if let Some(b) = &sys_bus {
                                        let _ = power_idle::set_power_profile(b, &p).await;
                                    }
                                }
                                BgCmd::SetIdleLock(secs) => {
                                    idle_state.lock_secs = secs;
                                    if power_idle::apply_idle_config(&idle_state).is_ok() {
                                        show_toast(&ui_weak, "空闲自动锁屏时间已生效");
                                    }
                                }
                                BgCmd::SetIdleSuspend(secs) => {
                                    idle_state.suspend_secs = secs;
                                    if power_idle::apply_idle_config(&idle_state).is_ok() {
                                        show_toast(&ui_weak, "空闲自动休眠策略已生效");
                                    }
                                }
                                BgCmd::SetNtp(on) => {
                                    if let Some(b) = &sys_bus {
                                        let _ = system_info::set_ntp(b, on).await;
                                    }
                                }
                                BgCmd::SetTimezone(tz) => {
                                    if let Some(b) = &sys_bus {
                                        if system_info::set_timezone(b, &tz).await {
                                            show_toast(&ui_weak, format!("系统时区已更新为 {tz}"));
                                        }
                                    }
                                }
                                BgCmd::SetLocale(lang) => {
                                    if let Some(b) = &sys_bus {
                                        if system_info::set_system_locale(b, &lang).await {
                                            show_toast(&ui_weak, "语言已更新，重新登录会话后生效");
                                        }
                                    }
                                }
                                BgCmd::SetHostname(h) => {
                                    if let Some(b) = &sys_bus {
                                        if system_info::set_static_hostname(b, &h).await {
                                            show_toast(&ui_weak, format!("主机名已更新为 {h}"));
                                        }
                                    }
                                }
                                BgCmd::RebootAndroid => {
                                    if daemon::reboot_android().is_err() {
                                        let _ = std::process::Command::new("/usr/local/sbin/reboot-to-android")
                                            .status();
                                    }
                                }
                            }

                            if need_refresh {
                                let snap = collect_full_snapshot(sys_bus.as_ref(), ses_bus.as_ref()).await;
                                def_apps_state = snap.default_apps.clone();
                                let w = ui_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = w.upgrade() {
                                        apply_snapshot_to_ui(&ui, &snap);
                                    }
                                });
                            }
                        }
                    }
                }

                if agent_registered {
                    if let Some(b) = &sys_bus {
                        bluetooth::set_bt_discovery(b, false).await;
                        bluetooth::unregister_pairing_agent(b).await;
                    }
                }
            });
        })
        .ok();
}

// ---------------- Headless Software PNG Renderer (`--render-png <out.png>`) ----------------

struct HeadlessPngPlatform {
    window: Rc<slint::platform::software_renderer::MinimalSoftwareWindow>,
}

impl slint::platform::Platform for HeadlessPngPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn run_headless_png(args: &[String]) -> Result<(), String> {
    let mut out_path = String::new();
    let mut page = "display".to_string();
    let mut width: u32 = 1280;
    let mut height: u32 = 960;
    let mut light = false;
    let mut accent_idx: i32 = 0;
    let mut index_only = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--render-png" if i + 1 < args.len() => {
                out_path = args[i + 1].clone();
                i += 1;
            }
            "--page" | "-p" if i + 1 < args.len() => {
                page = search::normalize_page_id(&args[i + 1]).to_string();
                i += 1;
            }
            "--width" if i + 1 < args.len() => {
                width = args[i + 1].parse().unwrap_or(1280).clamp(360, 2560);
                i += 1;
            }
            "--height" if i + 1 < args.len() => {
                height = args[i + 1].parse().unwrap_or(960).clamp(420, 2400);
                i += 1;
            }
            "--accent" if i + 1 < args.len() => {
                accent_idx = args[i + 1].parse::<i32>().unwrap_or(0).clamp(0, 5);
                i += 1;
            }
            "--light" => light = true,
            "--index" => index_only = true,
            _ => {}
        }
        i += 1;
    }
    if out_path.is_empty() {
        return Err("missing <out.png> path after --render-png".into());
    }

    let sw_win = slint::platform::software_renderer::MinimalSoftwareWindow::new(
        slint::platform::software_renderer::RepaintBufferType::NewBuffer,
    );
    slint::platform::set_platform(Box::new(HeadlessPngPlatform {
        window: sw_win.clone(),
    }))
    .map_err(|e| e.to_string())?;
    sw_win.set_size(slint::PhysicalSize::new(width, height));

    let ui = MainWindow::new().map_err(|e| e.to_string())?;

    // Populate realistic showcase state for offline visual inspection
    ui.set_light_skin(light);
    ui.set_skin_idx(if light { 1 } else { 0 });
    ui.set_accent_idx(accent_idx);
    ui.set_accent_label(ACCENT_LABELS[accent_idx as usize].into());
    ui.set_wallpaper_preset_idx(2);
    ui.set_wallpaper_label("极光青翠".into());
    ui.set_device_subtitle("90% · 旁路直供".into());
    ui.set_battery_headline("旁路直供 · 0.0W".into());
    ui.set_battery_detail("已达 90%，电源直接供电 · 温度 29.8°C · 健康度 100%".into());

    ui.set_sum_wifi("TB520FU-5G".into());
    ui.set_sum_bt("2 台已连".into());
    ui.set_sum_display("2× · 120Hz".into());
    ui.set_sum_sound("55%".into());
    ui.set_sum_notif("定时勿扰".into());
    ui.set_sum_power("90% · 旁路".into());
    ui.set_sum_pen("笔 90%".into());
    ui.set_sum_sys("Shanghai".into());

    ui.set_wifi_enabled(true);
    ui.set_airplane_mode(false);
    ui.set_wired_summary("USB 共享网络 (usb0) · IP 192.168.42.18".into());
    ui.set_wifi_has_active(true);
    ui.set_wifi_active_ssid("TB520FU-5G".into());
    ui.set_wifi_active_detail("WPA2 / WPA3 · IP 192.168.1.100 · 网关 192.168.1.1 · MAC 02:00:00:00:89:10".into());
    ui.set_wifi_networks(ModelRc::new(VecModel::from(vec![
        WifiRow { ssid: "Studio-WiFi6E".into(), subtitle: "WPA3 · 已保存".into(), strength: 88, secure: true, saved: true },
        WifiRow { ssid: "Guest-Lobby".into(), subtitle: "开放".into(), strength: 62, secure: false, saved: false },
    ])));

    ui.set_dnd(false);
    ui.set_dnd_fullscreen(true);
    ui.set_dnd_schedule_enabled(true);
    ui.set_dnd_start_label("23:00".into());
    ui.set_dnd_end_label("07:30".into());
    ui.set_banner_timeout_idx(1);
    ui.set_lock_notif_label("隐藏通知内容".into());
    ui.set_notif_apps(ModelRc::new(VecModel::from(vec![
        NotifAppRow { name: "系统通知".into(), enabled: true },
        NotifAppRow { name: "日历 (GNOME Calendar)".into(), enabled: true },
        NotifAppRow { name: "Firefox 浏览器".into(), enabled: false },
    ])));

    ui.set_pen_status_text("已磁吸就绪".into());
    ui.set_pen_badge_text("电量 90%".into());
    ui.set_osk_auto_popup(true);
    ui.set_osk_mode_idx(0);
    ui.set_osk_sound_on(true);
    ui.set_osk_sound_idx(0);
    ui.set_kb_caps_ctrl(true);
    ui.set_ime_trigger_label("Ctrl + Space".into());

    ui.set_default_browser_label("Firefox ESR".into());
    ui.set_default_pdf_label("文档查看器 (Papers)".into());
    ui.set_default_image_label("图像查看器 (Loupe)".into());
    ui.set_default_media_label("Amberol / mpv".into());
    ui.set_current_user("debian".into());
    ui.set_hostname("yoga-tb520fu".into());
    ui.set_kernel("7.3.3-full4".into());
    ui.set_os_pretty("Debian GNU/Linux 13 (trixie)".into());
    ui.set_ram_gb("16 GB LPDDR5X".into());
    ui.set_storage_summary("已用 28.4 GB / 118.0 GB".into());
    ui.set_storage_tool_label("存储详情 ↗".into());
    ui.set_boot_switch_ready(true);

    ui.set_active_page(page.into());
    ui.set_drill(!index_only);
    ui.show().map_err(|e| e.to_string())?;

    slint::platform::update_timers_and_animations();
    let mut pixels = vec![slint::Rgb8Pixel::new(0, 0, 0); (width * height) as usize];
    sw_win.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width as usize);
    });

    let file = File::create(&out_path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    let raw_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(pixels.as_ptr() as *const u8, pixels.len() * 3)
    };
    writer.write_image_data(raw_bytes).map_err(|e| e.to_string())?;
    Ok(())
}

fn main() -> Result<(), slint::PlatformError> {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args.iter().any(|a| a == "--render-png") {
        if let Err(e) = run_headless_png(&raw_args) {
            eprintln!("tb520fu-settings --render-png error: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let cli_page = parse_cli_page();
    if try_delegate_to_existing_instance(cli_page.as_deref()) {
        return Ok(());
    }

    let t0 = Instant::now();
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
        .select()?;
    slint::set_xdg_app_id(APP_ID)?;

    let ui = MainWindow::new()?;
    let init_page = cli_page.unwrap_or_else(|| "display".into());
    ui.set_active_page(init_page.clone().into());
    ui.set_drill(true);

    if std::env::var_os("SETTINGS_PERF").is_some() {
        let perf = Rc::new(RefCell::new(perf::FramePerf::new()));
        let first = Rc::new(RefCell::new(true));
        let _ = ui.window().set_rendering_notifier(move |state, _| {
            if let slint::RenderingState::BeforeRendering = state {
                if first.replace(false) {
                    eprintln!(
                        "PERF settings first-frame {:.0}ms after start",
                        t0.elapsed().as_secs_f32() * 1000.0
                    );
                }
                perf.borrow_mut().record_frame();
            }
        });
    }

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<BgCmd>();
    let pair_reply_slot: Arc<Mutex<Option<oneshot::Sender<bool>>>> = Arc::new(Mutex::new(None));
    let local_walls_cache: Rc<RefCell<Vec<LocalWallpaperItem>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let tx = cmd_tx.clone();
        ui.on_page_selected(move |p| {
            let _ = tx.send(BgCmd::PageChanged(p.to_string()));
        });
    }

    {
        let w = ui.as_weak();
        ui.on_search_edited(move |q: SharedString| {
            let Some(u) = w.upgrade() else { return };
            let hits: Vec<SearchHit> = search::query_search(&q)
                .into_iter()
                .map(|e| SearchHit {
                    page_id: e.page_id.into(),
                    section_tag: e.section_tag.into(),
                    title: e.title.into(),
                    page_label: e.page_label.into(),
                })
                .collect();
            u.set_search_hits(ModelRc::new(VecModel::from(hits)));
        });
    }

    {
        let w = ui.as_weak();
        ui.on_activate_search_hit(move |hit| {
            let Some(u) = w.upgrade() else { return };
            let tag = hit.section_tag.clone();
            u.set_pulse_tag(tag.clone());
            let w2 = u.as_weak();
            slint::Timer::single_shot(Duration::from_millis(1600), move || {
                if let Some(u2) = w2.upgrade() {
                    if u2.get_pulse_tag() == tag {
                        u2.set_pulse_tag(SharedString::default());
                    }
                }
            });
        });
    }

    // Wi-Fi & Airplane callbacks
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_wifi(move |v| {
            let _ = tx.send(BgCmd::WifiToggle(v));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_airplane(move |v| {
            let _ = tx.send(BgCmd::AirplaneToggle(v));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_wifi_rescan(move || {
            let _ = tx.send(BgCmd::WifiRescan);
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_wifi_forget(move |ssid| {
            let _ = tx.send(BgCmd::WifiForget(ssid.to_string()));
        });
    }
    {
        let tx = cmd_tx.clone();
        let w = ui.as_weak();
        ui.on_wifi_select(move |row| {
            if !row.secure || row.saved {
                let _ = tx.send(BgCmd::WifiConnect {
                    ssid: row.ssid.to_string(),
                    pw: None,
                });
            } else if let Some(u) = w.upgrade() {
                u.set_modal_kind("wifi-pw".into());
                u.set_modal_title(format!("连接 “{}”", row.ssid).into());
                u.set_modal_subtitle("请输入无线网络密码".into());
                u.set_modal_passkey(row.ssid);
                u.set_modal_input(SharedString::default());
            }
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_wifi_submit_password(move |ssid, pw| {
            let _ = tx.send(BgCmd::WifiConnect {
                ssid: ssid.to_string(),
                pw: Some(pw.to_string()),
            });
        });
    }

    // Bluetooth callbacks
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_bt(move |v| {
            let _ = tx.send(BgCmd::BtToggle(v));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_bt_toggle_scan(move |v| {
            let _ = tx.send(BgCmd::BtScan(v));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_bt_connect_or_disconnect(move |d| {
            let _ = tx.send(BgCmd::BtConnectToggle {
                path: d.path.to_string(),
                connected: d.connected,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_bt_forget(move |d| {
            let _ = tx.send(BgCmd::BtForget(d.path.to_string()));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_bt_pair(move |d| {
            let _ = tx.send(BgCmd::BtPair {
                path: d.path.to_string(),
                name: d.name.to_string(),
            });
        });
    }
    {
        let slot = pair_reply_slot.clone();
        ui.on_bt_confirm_pair(move |ok| {
            if let Some(sender) = slot.lock().unwrap().take() {
                let _ = sender.send(ok);
            }
        });
    }

    // Display, Accent & Wallpaper callbacks
    {
        let tx = cmd_tx.clone();
        let w = ui.as_weak();
        ui.on_change_skin(move |idx| {
            let light = idx == 1;
            if let Some(u) = w.upgrade() {
                u.set_light_skin(light);
                u.set_skin_idx(idx);
            }
            let skin_val = if light { "\"glass-light\"" } else { "\"glass-dark\"" };
            let _ = tx.send(BgCmd::SetShellPref {
                key: "skin".into(),
                val_json: skin_val.into(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        let w = ui.as_weak();
        ui.on_change_accent(move |idx| {
            let clamped = (idx.clamp(0, 5)) as usize;
            if let Some(u) = w.upgrade() {
                u.set_accent_idx(clamped as i32);
                u.set_accent_label(ACCENT_LABELS[clamped].into());
            }
            let id = ACCENT_IDS[clamped];
            let _ = tx.send(BgCmd::SetShellPref {
                key: "accent".into(),
                val_json: format!("\"{id}\""),
                toast: Some(format!("主题强调色已切换为 {}", ACCENT_LABELS[clamped])),
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        let w = ui.as_weak();
        ui.on_select_wallpaper_preset(move |idx| {
            let clamped = (idx.clamp(0, 4)) as usize;
            let (id, label) = WALLPAPER_PRESETS[clamped];
            if let Some(u) = w.upgrade() {
                u.set_wallpaper_preset_idx(clamped as i32);
                u.set_wallpaper_label(label.into());
            }
            let _ = tx.send(BgCmd::SetShellPref {
                key: "wallpaper".into(),
                val_json: format!("\"{id}\""),
                toast: Some(format!("壁纸已切换为「{label}」")),
            });
        });
    }
    {
        let w = ui.as_weak();
        let cache = local_walls_cache.clone();
        ui.on_refresh_local_wallpapers(move || {
            let scanned = wallpaper::scan_local_wallpapers();
            let labels: Vec<SharedString> = scanned.iter().map(|it| it.label.clone().into()).collect();
            *cache.borrow_mut() = scanned;
            if let Some(u) = w.upgrade() {
                u.set_local_wall_labels(ModelRc::new(VecModel::from(labels)));
            }
        });
    }
    {
        let tx = cmd_tx.clone();
        let w = ui.as_weak();
        let cache = local_walls_cache;
        ui.on_select_local_wallpaper(move |idx| {
            let borrowed = cache.borrow();
            let Some(item) = borrowed.get(idx as usize) else { return };
            let spec = item.spec.clone();
            drop(borrowed);
            let desc = wallpaper::describe_wallpaper_spec(&spec);
            if let Some(u) = w.upgrade() {
                u.set_wallpaper_preset_idx(-1);
                u.set_wallpaper_label(desc.clone().into());
            }
            if let Ok(js) = serde_json::to_string(&spec) {
                let _ = tx.send(BgCmd::SetShellPref {
                    key: "wallpaper".into(),
                    val_json: js,
                    toast: Some(format!("已应用壁纸 {desc}")),
                });
            }
        });
    }
    {
        let tx = cmd_tx.clone();
        let w = ui.as_weak();
        ui.on_submit_custom_wallpaper(move |raw_path| {
            match wallpaper::validate_custom_wallpaper_path(&raw_path) {
                Ok(spec) => {
                    let desc = wallpaper::describe_wallpaper_spec(&spec);
                    if let Some(u) = w.upgrade() {
                        u.set_wallpaper_preset_idx(-1);
                        u.set_wallpaper_label(desc.clone().into());
                    }
                    if let Ok(js) = serde_json::to_string(&spec) {
                        let _ = tx.send(BgCmd::SetShellPref {
                            key: "wallpaper".into(),
                            val_json: js,
                            toast: Some(format!("已应用自定义壁纸 {desc}")),
                        });
                    }
                }
                Err(e) => show_toast(&w, e),
            }
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_brightness(move |v| {
            let _ = tx.send(BgCmd::SetBrightness(v.clamp(1, 100) as u8));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_autobright(move |v| {
            let _ = tx.send(BgCmd::SetAutobright(v));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_hbm(move |i| {
            let _ = tx.send(BgCmd::SetHbm(i.clamp(0, 2) as u8));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_night_light(move |v| {
            let _ = tx.send(BgCmd::SetShellPref {
                key: "night_light".into(),
                val_json: v.to_string(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_reading_mode(move |v| {
            let _ = tx.send(BgCmd::SetShellPref {
                key: "reading_mode".into(),
                val_json: v.to_string(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_dsi_hdr(move |v| {
            let _ = tx.send(BgCmd::UpdateNiri(Box::new(move |p| p.dsi_hdr = v)));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_high_refresh(move |v| {
            let _ = tx.send(BgCmd::SetShellPref {
                key: "high_refresh_144".into(),
                val_json: v.to_string(),
                toast: Some(if v {
                    "已切换至 120Hz 高刷新率".into()
                } else {
                    "已锁定 60Hz 省电刷新率".into()
                }),
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_auto_rotate(move |v| {
            let _ = tx.send(BgCmd::SetShellPref {
                key: "auto_rotate".into(),
                val_json: v.to_string(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_presets(move |i| {
            let js = PRESET_CHOICES.get(i as usize).copied().unwrap_or("[33,50,67,100]");
            let _ = tx.send(BgCmd::SetShellPref {
                key: "win_width_presets".into(),
                val_json: js.into(),
                toast: Some("窗口顶条宽度预设已更新".into()),
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_scale(move |i| {
            let sc = SCALE_CHOICES.get(i as usize).copied().unwrap_or(2.0);
            let _ = tx.send(BgCmd::UpdateNiri(Box::new(move |p| p.dsi_scale = sc)));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_ext_enabled(move |v| {
            let _ = tx.send(BgCmd::UpdateNiri(Box::new(move |p| p.ext_enabled = v)));
        });
    }
    {
        let tx = cmd_tx.clone();
        let w = ui.as_weak();
        ui.on_change_ext_mode(move |i| {
            if let Some(u) = w.upgrade() {
                let modes = u.get_ext_modes();
                if let Some(m) = slint::Model::row_data(&modes, i as usize) {
                    let mode_s = m.to_string();
                    let _ = tx.send(BgCmd::UpdateNiri(Box::new(move |p| p.ext_mode = Some(mode_s))));
                }
            }
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_ext_pos(move |i| {
            let pos = match i {
                0 => "left",
                1 => "above",
                _ => "right",
            };
            let _ = tx.send(BgCmd::UpdateNiri(Box::new(move |p| p.ext_pos = pos.into())));
        });
    }

    // Sound callbacks
    {
        let tx = cmd_tx.clone();
        ui.on_change_out_vol(move |v| {
            let _ = tx.send(BgCmd::AudioOutVol(v.clamp(0, 100) as u8));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_out_mute(move |v| {
            let _ = tx.send(BgCmd::AudioOutMute(v));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_mic_vol(move |v| {
            let _ = tx.send(BgCmd::AudioMicVol(v.clamp(0, 100) as u8));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_mic_mute(move |v| {
            let _ = tx.send(BgCmd::AudioMicMute(v));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_select_audio_node(move |id| {
            if id >= 0 {
                let _ = tx.send(BgCmd::AudioSelectNode(id as u32));
            }
        });
    }

    // Notifications & Focus callbacks
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_dnd(move |v| {
            let _ = tx.send(BgCmd::SetShellPref {
                key: "dnd".into(),
                val_json: v.to_string(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_send_test_notif(move || {
            let _ = tx.send(BgCmd::SendTestNotif);
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_dnd_fullscreen(move |v| {
            let _ = tx.send(BgCmd::SetShellPref {
                key: "dnd_fullscreen".into(),
                val_json: v.to_string(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_dnd_schedule(move |v| {
            let _ = tx.send(BgCmd::SetShellPref {
                key: "dnd_schedule_enabled".into(),
                val_json: v.to_string(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_dnd_start(move |i| {
            let hm = DND_START_CHOICES.get(i as usize).copied().unwrap_or("23:00");
            let _ = tx.send(BgCmd::SetShellPref {
                key: "dnd_start".into(),
                val_json: format!("\"{hm}\""),
                toast: Some(format!("定时勿扰开始时间已设为 {hm}")),
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_dnd_end(move |i| {
            let hm = DND_END_CHOICES.get(i as usize).copied().unwrap_or("07:30");
            let _ = tx.send(BgCmd::SetShellPref {
                key: "dnd_end".into(),
                val_json: format!("\"{hm}\""),
                toast: Some(format!("定时勿扰结束时间已设为 {hm}")),
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_banner_timeout(move |i| {
            let ms = BANNER_TIMEOUT_MS.get(i as usize).copied().unwrap_or(4200);
            let _ = tx.send(BgCmd::SetShellPref {
                key: "banner_timeout_ms".into(),
                val_json: ms.to_string(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_lock_notif_mode(move |i| {
            let clamped = (i.clamp(0, 2)) as usize;
            let mode = LOCK_NOTIF_IDS[clamped];
            let _ = tx.send(BgCmd::SetShellPref {
                key: "lock_notif_mode".into(),
                val_json: format!("\"{mode}\""),
                toast: Some(format!("锁屏通知已设为：{}", LOCK_NOTIF_LABELS[clamped])),
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_app_notif(move |app, en| {
            let _ = tx.send(BgCmd::ToggleAppNotif {
                app_name: app.to_string(),
                enabled: en,
            });
        });
    }

    // Power callbacks
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_bypass(move |v| {
            let _ = tx.send(BgCmd::SetBypass(v));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_profile(move |i| {
            let p = PROFILE_IDS.get(i as usize).copied().unwrap_or("balanced");
            let _ = tx.send(BgCmd::SetPowerProfile(p.into()));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_lock_timeout(move |i| {
            let s = LOCK_SECS_CHOICES.get(i as usize).copied().unwrap_or(300);
            let _ = tx.send(BgCmd::SetIdleLock(s));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_suspend_timeout(move |i| {
            let s = SUSPEND_SECS_CHOICES.get(i as usize).copied().unwrap_or(0);
            let _ = tx.send(BgCmd::SetIdleSuspend(s));
        });
    }

    // Pen, Touchpad, OSK & Keyboard callbacks
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_pen_corners(move |v| {
            let _ = tx.send(BgCmd::SetShellPref {
                key: "pen_corners".into(),
                val_json: v.to_string(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_pen_orb(move |v| {
            let _ = tx.send(BgCmd::SetShellPref {
                key: "pen_orb".into(),
                val_json: v.to_string(),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_tp_tap(move |v| {
            let _ = tx.send(BgCmd::UpdateNiri(Box::new(move |p| p.tp_tap = v)));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_tp_natural(move |v| {
            let _ = tx.send(BgCmd::UpdateNiri(Box::new(move |p| p.tp_natural_scroll = v)));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_tp_speed(move |v| {
            let spd = v.clamp(0, 100) as u8;
            let _ = tx.send(BgCmd::UpdateNiri(Box::new(move |p| p.tp_speed_pct = spd)));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_osk_auto_popup(move |v| {
            let _ = tx.send(BgCmd::SetOskField {
                key: "auto_popup".into(),
                val: serde_json::Value::Bool(v),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_osk_mode(move |i| {
            let _ = tx.send(BgCmd::SetOskField {
                key: "mode".into(),
                val: osk_ime::osk_mode_json_value(i.clamp(0, 2) as usize),
                toast: Some("屏幕键盘默认形态已更新".into()),
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_osk_sound(move |v| {
            let _ = tx.send(BgCmd::SetOskField {
                key: "sound_on".into(),
                val: serde_json::Value::Bool(v),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_osk_sound_style(move |i| {
            let id = OSK_SOUND_IDS.get(i as usize).copied().unwrap_or("std");
            let _ = tx.send(BgCmd::SetOskField {
                key: "sound".into(),
                val: serde_json::Value::String(id.into()),
                toast: None,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_kb_caps_ctrl(move |v| {
            let _ = tx.send(BgCmd::UpdateNiri(Box::new(move |p| p.kb_caps_ctrl = v)));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_ime_trigger(move |i| {
            let _ = tx.send(BgCmd::SetImeTrigger(i.clamp(0, 2) as usize));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_show_hotkey_overlay(move || {
            let _ = tx.send(BgCmd::ShowHotkeyOverlay);
        });
    }

    // System & Default Apps callbacks
    {
        let tx = cmd_tx.clone();
        ui.on_toggle_ntp(move |v| {
            let _ = tx.send(BgCmd::SetNtp(v));
        });
    }
    {
        let tx = cmd_tx.clone();
        let w = ui.as_weak();
        ui.on_change_tz(move |i| {
            if let Some(u) = w.upgrade() {
                let opts = u.get_tz_options();
                if let Some(tz) = slint::Model::row_data(&opts, i as usize) {
                    let _ = tx.send(BgCmd::SetTimezone(tz.to_string()));
                }
            }
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_lang(move |i| {
            let l = LANG_IDS.get(i as usize).copied().unwrap_or("zh_CN.UTF-8");
            let _ = tx.send(BgCmd::SetLocale(l.into()));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_default_browser(move |i| {
            let _ = tx.send(BgCmd::SetDefaultApp {
                role: AppRole::Browser,
                idx: i.max(0) as usize,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_default_pdf(move |i| {
            let _ = tx.send(BgCmd::SetDefaultApp {
                role: AppRole::Pdf,
                idx: i.max(0) as usize,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_default_image(move |i| {
            let _ = tx.send(BgCmd::SetDefaultApp {
                role: AppRole::Image,
                idx: i.max(0) as usize,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_change_default_media(move |i| {
            let _ = tx.send(BgCmd::SetDefaultApp {
                role: AppRole::Media,
                idx: i.max(0) as usize,
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_submit_password_change(move |cur, n1, n2| {
            let _ = tx.send(BgCmd::ChangePassword {
                cur: cur.to_string(),
                new1: n1.to_string(),
                new2: n2.to_string(),
            });
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_submit_hostname(move |h| {
            let _ = tx.send(BgCmd::SetHostname(h.to_string()));
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_open_storage_analyzer(move || {
            let _ = tx.send(BgCmd::LaunchStorageAnalyzer);
        });
    }
    {
        let tx = cmd_tx.clone();
        ui.on_confirm_reboot_android(move || {
            let _ = tx.send(BgCmd::RebootAndroid);
        });
    }

    spawn_backend_worker(ui.as_weak(), cmd_rx, cmd_tx, pair_reply_slot, init_page);
    ui.run()
}
