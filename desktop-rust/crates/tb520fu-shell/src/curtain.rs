// SPDX-License-Identifier: MIT
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use cosmic_text::Align;
use tb520fu_ui::canvas::{Canvas, RectF};
use tb520fu_ui::color::{Palette, Rgba};
use tb520fu_ui::haptics::{HapticEngine, HapticKind};
use tb520fu_ui::spring::Spring1D;
use tb520fu_ui::svg_icons::IconSet;
use tb520fu_ui::text::{FontWeight, TextEngine};
use tb520fu_ui::widgets::draw_origin_pill_slider;

use crate::app_menu::{AppMenuColumn, AppMenuEvent};
use crate::niri_ipc::NiriSnapshot;
use crate::notifications::NotifItem;
use crate::statusbar::PullWing;
use crate::sys_state::{
    connect_wifi_ssid, connect_wifi_with_password, is_bt_discovering, media_cmd, set_autobright,
    set_brightness_pct, set_bt_discovery, set_bt_enabled, set_charge_limit, set_volume_pct,
    set_wifi_enabled, sync_display_ctm_and_hz, system_poweroff, system_reboot,
    system_reboot_android, toggle_bt_device_mac, toggle_torch, trigger_wifi_scan, DesktopApp,
    ShellPrefs, SysSnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PortraitTab {
    #[default]
    ControlNotif,
    Apps,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurtainEffect {
    None,
    OpenOskForSearch,
    OpenOskForWifiPassword,
    HideInlineOsk,
    ToggleOsk,
    OpenOskClipboard,
    StartScreenshotAnnot,
    ToggleQuickNote,
    LockScreen,
    StartStandby,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuickToggleId {
    AutoRotate,
    AutoBrightness,
    HighRefresh,
    BypassPower,
    Dnd,
    Torch,
    StandbyDisplay,
    VirtualKeyboard,
}

#[derive(Debug, Clone)]
enum CcHit {
    PortraitTab(PortraitTab),
    HeaderPowerToggle,
    PowerReboot,
    PowerRebootAndroid,
    PowerShutdown,
    WifiToggle,
    WifiExpand,
    WifiRescan,
    WifiSelect {
        ssid: String,
        secured: bool,
        saved: bool,
        active: bool,
    },
    WifiPasswordInput,
    WifiSubmitPassword,
    BtToggle,
    BtExpand,
    BtRescan,
    BtDevice {
        mac: String,
        connected: bool,
        paired: bool,
    },
    MediaPrev,
    MediaPlayPause,
    MediaNext,
    QuickToggle(QuickToggleId),
    ClearAllNotifs,
    DismissNotif(u32),
}

#[derive(Debug, Clone, Copy)]
enum ActiveGesture {
    None,
    VolSlider { slot: i32, rect: RectF },
    BriSlider { slot: i32, rect: RectF },
    AppMenu { slot: i32 },
    DrawerScroll {
        slot: i32,
        is_wifi: bool,
        x0: f32,
        y0: f32,
        scroll0: f32,
        scrolling: bool,
    },
    CcTapOrScroll {
        slot: i32,
        x0: f32,
        y0: f32,
        t0: Instant,
        scroll0: f32,
        scrolling: bool,
    },
    NotifScroll {
        slot: i32,
        x0: f32,
        y0: f32,
        scroll0: f32,
        scrolling: bool,
    },
    ScrimOrSwipeUp {
        slot: i32,
        x0: f32,
        y0: f32,
        last_y: f32,
        last_t: Instant,
        vy: f32,
        tap_ok: bool,
    },
}

pub struct Curtain {
    pub open: bool,
    pub wing: PullWing,
    pub portrait_tab: PortraitTab,
    pub progress: Spring1D,
    pub power_open: bool,
    pub power_spring: Spring1D,
    android_confirm_until: Option<Instant>,
    pub wifi_open: bool,
    pub bt_open: bool,
    pub wifi_spring: Spring1D,
    pub bt_spring: Spring1D,
    pub wifi_pw_ssid: Option<String>,
    pub wifi_pw_buf: String,
    wifi_scan_until: Option<Instant>,
    wifi_connecting: Option<(String, Instant)>,
    bt_connecting: Option<(String, bool, Instant)>,
    pub app_menu: AppMenuColumn,

    cc_scroll: f32,
    cc_max_scroll: f32,
    wifi_scroll: f32,
    wifi_max_scroll: f32,
    bt_scroll: f32,
    bt_max_scroll: f32,
    notif_scroll: f32,
    notif_max_scroll: f32,

    cc_panel_rect: RectF,
    wifi_drawer_rect: RectF,
    bt_drawer_rect: RectF,
    notif_panel_rect: RectF,
    app_panel_rect: RectF,
    vol_rect: RectF,
    bri_rect: RectF,
    last_screen_w: f32,
    last_screen_h: f32,
    active_slide_cache_key: u64,
    hits: Vec<(RectF, CcHit)>,
    gesture: ActiveGesture,
    pub scroll_pull_active: bool,
    scroll_pull_vy: f32,
    scroll_pull_last_at: Option<Instant>,
    pub touchpad_edge_pull_active: bool,
    pub swipe_3f_active: bool,
    swipe_3f_vy: f32,
    swipe_3f_last_at: Option<Instant>,
}

impl Curtain {
    pub fn new(apps: Vec<DesktopApp>) -> Self {
        let mut progress = Spring1D::normalized(0.0, 19.5, 1.0);
        progress.snap(0.0);
        let mut power_spring = Spring1D::normalized(0.0, 26.0, 1.0);
        power_spring.snap(0.0);
        let mut wifi_spring = Spring1D::normalized(0.0, 26.0, 1.0);
        wifi_spring.snap(0.0);
        let mut bt_spring = Spring1D::normalized(0.0, 26.0, 1.0);
        bt_spring.snap(0.0);

        Self {
            open: false,
            wing: PullWing::Right,
            portrait_tab: PortraitTab::ControlNotif,
            progress,
            power_open: false,
            power_spring,
            android_confirm_until: None,
            wifi_open: false,
            bt_open: false,
            wifi_spring,
            bt_spring,
            wifi_pw_ssid: None,
            wifi_pw_buf: String::new(),
            wifi_scan_until: None,
            wifi_connecting: None,
            bt_connecting: None,
            app_menu: AppMenuColumn::new(apps),
            cc_scroll: 0.0,
            cc_max_scroll: 0.0,
            wifi_scroll: 0.0,
            wifi_max_scroll: 0.0,
            bt_scroll: 0.0,
            bt_max_scroll: 0.0,
            notif_scroll: 0.0,
            notif_max_scroll: 0.0,
            cc_panel_rect: RectF::default(),
            wifi_drawer_rect: RectF::default(),
            bt_drawer_rect: RectF::default(),
            notif_panel_rect: RectF::default(),
            app_panel_rect: RectF::default(),
            vol_rect: RectF::default(),
            bri_rect: RectF::default(),
            last_screen_w: 1472.0,
            last_screen_h: 920.0,
            active_slide_cache_key: 0,
            hits: Vec::with_capacity(56),
            gesture: ActiveGesture::None,
            scroll_pull_active: false,
            scroll_pull_vy: 0.0,
            scroll_pull_last_at: None,
            touchpad_edge_pull_active: false,
            swipe_3f_active: false,
            swipe_3f_vy: 0.0,
            swipe_3f_last_at: None,
        }
    }

    #[inline]
    fn push_clipped_hit(&mut self, r: RectF, clip: RectF, hit: CcHit) {
        if let Some(cr) = r.intersect(clip) {
            if cr.h >= 10.0 && cr.w >= 10.0 {
                self.hits.push((cr, hit));
            }
        }
    }

    pub fn is_mapped(&self) -> bool {
        self.open || self.progress.pos > 0.004 || self.progress.is_animating()
    }

    pub fn is_animating(&self) -> bool {
        self.progress.is_animating()
            || self.power_spring.is_animating()
            || self.wifi_spring.is_animating()
            || self.bt_spring.is_animating()
            || self.scroll_pull_active
    }

    pub fn open_with_wing(&mut self, wing: PullWing) {
        crate::sys_state::request_gpu_boost(650);
        self.scroll_pull_active = false;
        self.touchpad_edge_pull_active = false;
        self.swipe_3f_active = false;
        self.wing = wing;
        self.open = true;
        self.progress.set_target(1.0);
    }

    pub fn close(&mut self) {
        crate::sys_state::request_gpu_boost(650);
        self.scroll_pull_active = false;
        self.touchpad_edge_pull_active = false;
        self.swipe_3f_active = false;
        self.open = false;
        self.power_open = false;
        self.power_spring.set_target(0.0);
        self.android_confirm_until = None;
        self.wifi_pw_ssid = None;
        self.wifi_pw_buf.clear();
        if self.bt_open {
            self.bt_open = false;
            self.bt_spring.set_target(0.0);
        }
        set_bt_discovery(false);
        self.app_menu.search_focused = false;
        self.progress.set_target(0.0);
    }

    pub fn submit_wifi_password(&mut self) -> bool {
        let Some(ssid) = self.wifi_pw_ssid.take() else {
            return false;
        };
        let pw = std::mem::take(&mut self.wifi_pw_buf);
        if pw.trim().is_empty() {
            return false;
        }
        self.wifi_connecting = Some((ssid.clone(), Instant::now()));
        connect_wifi_with_password(ssid, pw);
        true
    }

    /// Touchpad physical top-edge 2-finger pull-down (`ABS_MT_POSITION_Y <= 295`).
    pub fn on_touchpad_edge_pull(
        &mut self,
        wing: PullWing,
        frac: f32,
        velocity: f32,
        finished: bool,
        screen_h: f32,
    ) {
        crate::sys_state::request_gpu_boost(650);
        let sh = screen_h.max(400.0);
        self.last_screen_h = sh;
        self.scroll_pull_active = false;
        if !finished {
            self.touchpad_edge_pull_active = true;
            self.wing = wing;
            self.open = true;
            self.progress.snap(frac.clamp(0.015, 1.0));
        } else {
            self.touchpad_edge_pull_active = false;
            self.finish_pull(wing, velocity, sh);
        }
    }

    /// Touchpad 3-finger swipe intercepted by Niri while Console is mapped.
    pub fn on_3f_swipe_begin(&mut self) {
        crate::sys_state::request_gpu_boost(650);
        self.swipe_3f_active = true;
        self.scroll_pull_active = false;
        self.swipe_3f_vy = 0.0;
        self.swipe_3f_last_at = Some(Instant::now());
        self.app_menu.cancel_press();
    }

    pub fn on_3f_swipe_update(&mut self, uninverted_dy: f32, screen_h: f32) -> bool {
        if !self.swipe_3f_active && !self.is_mapped() {
            return false;
        }
        crate::sys_state::request_gpu_boost(650);
        self.swipe_3f_active = true;
        let sh = screen_h.max(400.0);
        self.last_screen_h = sh;
        let now = Instant::now();
        let dt = self
            .swipe_3f_last_at
            .map(|t| now.duration_since(t).as_secs_f32().clamp(0.001, 0.08))
            .unwrap_or(0.008);
        self.swipe_3f_last_at = Some(now);
        let step_px = uninverted_dy * 1.75;
        self.swipe_3f_vy = self.swipe_3f_vy * 0.45 + (step_px / dt) * 0.55;
        let next_p = (self.progress.pos + step_px / sh).clamp(0.015, 1.0);
        self.open = true;
        self.progress.snap(next_p);
        true
    }

    pub fn on_3f_swipe_end(&mut self, screen_h: f32) -> bool {
        if !self.swipe_3f_active {
            return false;
        }
        self.swipe_3f_active = false;
        self.swipe_3f_last_at = None;
        self.finish_pull(self.wing, self.swipe_3f_vy, screen_h);
        true
    }

    /// Touchpad 2-finger downward scroll starting on `tb520fu-bar` (`dy < 0` with `natural-scroll`).
    pub fn on_topbar_scroll(&mut self, x: f32, dy: f32, screen_w: f32, screen_h: f32) -> bool {
        if self.touchpad_edge_pull_active || self.swipe_3f_active {
            return false;
        }
        if !self.scroll_pull_active && dy >= -0.4 {
            return false;
        }
        crate::sys_state::request_gpu_boost(650);
        let sh = screen_h.max(400.0);
        self.last_screen_h = sh;
        if !self.scroll_pull_active {
            self.wing = if x < screen_w * 0.5 {
                PullWing::Left
            } else {
                PullWing::Right
            };
            self.scroll_pull_active = true;
            self.scroll_pull_vy = 0.0;
            self.open = true;
            if self.progress.pos < 0.015 {
                self.progress.snap(0.02);
            }
        }
        let now = Instant::now();
        let dt = self
            .scroll_pull_last_at
            .map(|t| now.duration_since(t).as_secs_f32().clamp(0.001, 0.08))
            .unwrap_or(0.008);
        self.scroll_pull_last_at = Some(now);
        let step_px = -dy * 1.65;
        self.scroll_pull_vy = self.scroll_pull_vy * 0.45 + (step_px / dt) * 0.55;
        let next_p = (self.progress.pos + step_px / sh).clamp(0.015, 1.0);
        self.progress.snap(next_p);
        true
    }

    pub fn on_scroll_stop(&mut self, screen_h: f32) -> bool {
        if !self.scroll_pull_active {
            return false;
        }
        self.scroll_pull_active = false;
        self.scroll_pull_last_at = None;
        self.finish_pull(self.wing, self.scroll_pull_vy, screen_h);
        true
    }

    /// Exact 1:1 fingertip pixel-locked pull-down tracking (`vis_h == finger_y`).
    pub fn drag_open(&mut self, wing: PullWing, finger_y: f32, screen_h: f32) {
        crate::sys_state::request_gpu_boost(650);
        self.wing = wing;
        self.open = true;
        let sh = screen_h.max(400.0);
        self.last_screen_h = sh;
        let p = (finger_y / sh).clamp(0.015, 1.0);
        self.progress.snap(p);
    }

    pub fn finish_pull(&mut self, wing: PullWing, vy: f32, screen_h: f32) {
        crate::sys_state::request_gpu_boost(650);
        self.wing = wing;
        let sh = screen_h.max(400.0);
        self.last_screen_h = sh;
        if vy > 320.0 || (self.progress.pos > 0.32 && vy > -240.0) {
            self.open = true;
            self.progress.fling(1.0, (vy / sh).clamp(-2.5, 4.5));
        } else {
            self.open = false;
            self.power_open = false;
            self.power_spring.set_target(0.0);
            self.android_confirm_until = None;
            self.wifi_pw_ssid = None;
            self.wifi_pw_buf.clear();
            set_bt_discovery(false);
            self.app_menu.search_focused = false;
            self.progress.fling(0.0, (vy / sh).clamp(-4.5, 1.5));
        }
    }

    pub fn on_touch_cancel(&mut self) {
        let g = std::mem::replace(&mut self.gesture, ActiveGesture::None);
        if matches!(g, ActiveGesture::ScrimOrSwipeUp { .. }) {
            if self.progress.pos < 0.65 {
                self.close();
            } else {
                self.open = true;
                self.progress.set_target(1.0);
            }
        }
    }

    /// Returns `(needs_redraw, should_unmap, long_press_launched)`.
    pub fn tick(&mut self, dt: f32, niri: &NiriSnapshot) -> (bool, bool, bool) {
        let mut rd = false;
        if self.scroll_pull_active
            && self
                .scroll_pull_last_at
                .is_some_and(|t| t.elapsed() >= std::time::Duration::from_millis(240))
        {
            let sh = self.last_screen_h;
            let _ = self.on_scroll_stop(sh);
            rd = true;
        }
        if self.progress.step(dt) {
            rd = true;
        }
        if self.power_spring.step(dt) {
            rd = true;
        }
        if self.wifi_spring.step(dt) {
            rd = true;
        }
        if self.bt_spring.step(dt) {
            rd = true;
        }
        if self.wifi_scan_until.is_some_and(|t| Instant::now() >= t) {
            self.wifi_scan_until = None;
            rd = true;
        }
        if self.android_confirm_until.is_some_and(|t| Instant::now() >= t) {
            self.android_confirm_until = None;
            rd = true;
        }
        let lp = self.app_menu.tick_long_press(niri);
        if lp {
            self.close();
            rd = true;
        }
        if !self.open && self.progress.pos <= 0.0025 {
            self.progress.snap(0.0);
            self.active_slide_cache_key = 0;
        }
        let unmap = !self.open && !self.progress.is_animating() && self.progress.pos <= 0.005;
        (rd, unmap, lp)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        pal: &Palette,
        sys: &SysSnapshot,
        niri: &NiriSnapshot,
        prefs: &ShellPrefs,
        notifs: &[NotifItem],
        osk_bottom_inset: f32,
    ) {
        self.hits.clear();
        canvas.clear(Rgba::TRANSPARENT);

        let p = self.progress.pos.clamp(0.0, 1.0);
        if p <= 0.002 {
            self.active_slide_cache_key = 0;
            return;
        }

        let sw = canvas.logical_width();
        let sh = canvas.logical_height();
        self.last_screen_w = sw.max(1.0);
        self.last_screen_h = sh.max(1.0);

        let can_use_slide_cache = p < 0.998
            && !canvas.own_backdrop
            && osk_bottom_inset < 1.0
            && !self.power_spring.is_animating()
            && !self.wifi_spring.is_animating()
            && !self.bt_spring.is_animating();

        if can_use_slide_cache {
            let cache_key = if self.active_slide_cache_key != 0
                && canvas.has_cached_snapshot(self.active_slide_cache_key)
            {
                self.active_slide_cache_key
            } else {
                let k = self.compute_slide_cache_key(sw, sh, sys, niri, prefs, notifs);
                if !canvas.has_cached_snapshot(k) {
                    self.render_sheet_contents(
                        canvas, icons, text, pal, sys, niri, prefs, notifs, sw, sh, 1.0, 0.0,
                    );
                    let _ = canvas.store_cached_snapshot(k);
                    self.hits.clear();
                    canvas.clear(Rgba::TRANSPARENT);
                }
                self.active_slide_cache_key = k;
                k
            };
            let _ = cache_key;
            let vis_h = (p * sh).clamp(2.0, sh);
            canvas.draw_cached_snapshot(
                RectF::new(0.0, 0.0, sw, vis_h),
                [0.0, 1.0 - p, 1.0, 1.0],
                1.0,
            );
            if pal.glass {
                canvas.push_blur_rect(RectF::new(0.0, 0.0, sw, vis_h), [0.0; 4]);
            }
            return;
        }

        self.active_slide_cache_key = 0;
        self.render_sheet_contents(
            canvas,
            icons,
            text,
            pal,
            sys,
            niri,
            prefs,
            notifs,
            sw,
            sh,
            p,
            osk_bottom_inset,
        );
    }

    fn compute_slide_cache_key(
        &self,
        sw: f32,
        sh: f32,
        sys: &SysSnapshot,
        niri: &NiriSnapshot,
        prefs: &ShellPrefs,
        notifs: &[NotifItem],
    ) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        sw.to_bits().hash(&mut h);
        sh.to_bits().hash(&mut h);
        (self.wing as u8).hash(&mut h);
        (self.portrait_tab as u8).hash(&mut h);
        self.power_open.hash(&mut h);
        self.android_confirm_until.is_some().hash(&mut h);
        self.wifi_open.hash(&mut h);
        self.bt_open.hash(&mut h);
        self.wifi_pw_ssid.hash(&mut h);
        self.wifi_pw_buf.hash(&mut h);
        self.wifi_scan_until.is_some().hash(&mut h);
        (self.wifi_scroll as i32).hash(&mut h);
        (self.bt_scroll as i32).hash(&mut h);
        sys.wifi_list.len().hash(&mut h);
        sys.bt_peripherals.len().hash(&mut h);
        sys.time_hm.hash(&mut h);
        sys.date_short.hash(&mut h);
        sys.volume_pct.hash(&mut h);
        sys.muted.hash(&mut h);
        sys.brightness_pct.hash(&mut h);
        sys.autobright.hash(&mut h);
        sys.wifi_on.hash(&mut h);
        sys.wifi_ssid.hash(&mut h);
        sys.bt_on.hash(&mut h);
        sys.bt_device.hash(&mut h);
        sys.media_playing.hash(&mut h);
        sys.media_title.hash(&mut h);
        notifs.len().hash(&mut h);
        notifs.last().map(|n| n.id).hash(&mut h);
        niri.windows.len().hash(&mut h);
        (prefs.skin as u8).hash(&mut h);
        (prefs.launch_mode as u8).hash(&mut h);
        prefs.auto_rotate.hash(&mut h);
        prefs.dnd.hash(&mut h);
        prefs.night_light.hash(&mut h);
        prefs.reading_mode.hash(&mut h);
        prefs.high_refresh_144.hash(&mut h);
        sys.charge_limit.hash(&mut h);
        (self.app_menu.category as u8).hash(&mut h);
        self.app_menu.query.hash(&mut h);
        self.app_menu.search_focused.hash(&mut h);
        (self.app_menu.scroll_y as i32).hash(&mut h);
        (self.cc_scroll as i32).hash(&mut h);
        (self.notif_scroll as i32).hash(&mut h);
        h.finish() | 1
    }

    #[allow(clippy::too_many_arguments)]
    fn render_sheet_contents(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        pal: &Palette,
        sys: &SysSnapshot,
        niri: &NiriSnapshot,
        prefs: &ShellPrefs,
        notifs: &[NotifItem],
        sw: f32,
        sh: f32,
        p: f32,
        osk_bottom_inset: f32,
    ) {
        let portrait = sw < sh;

        // 1. Single Full-Screen `.pull` Liquid Glass Sheet (1 BlurRect, 0 nested glass overdraw!)
        canvas.fill_glass_corners_styled(
            RectF::new(0.0, 0.0, sw, sh),
            [0.0; 4],
            pal,
            pal.glass_tint,
            Rgba::TRANSPARENT,
            0.0,
            p,
        );

        let slide_dy = 0.0_f32;

        if portrait {
            self.render_portrait_layout(
                canvas,
                icons,
                text,
                pal,
                sys,
                niri,
                prefs,
                notifs,
                sw,
                sh,
                p,
                slide_dy,
                osk_bottom_inset,
            );
            return;
        }

        // 2. Landscape 3-Column Symmetric Layout (tuned for `1472x920` @ `2.0x` and wider external displays):
        //    - Outer columns (`通知` & `控制中心`) flush-aligned to left and right edges (`edge_pad = 20..24`).
        //    - Center column (`应用菜单`) centered with symmetric `gap` from both outer columns,
        //      stretching all the way to the bottom (`一直到底下`, `sh - 18` or above OSK when typing search!).
        let edge_pad = (sw * 0.0136).clamp(18.0, 24.0).round();
        let side_w = (sw * 0.258).clamp(376.0, 446.0).round();
        let gap = ((sw - 2.0 * edge_pad - 2.0 * side_w - 628.0) * 0.5)
            .clamp(22.0, 54.0)
            .round();
        let app_x = edge_pad + side_w + gap;
        let app_w = (sw - 2.0 * app_x).max(480.0);

        let top_y = 20.0 + slide_dy;
        let col_bottom_y = (sh - 18.0 - osk_bottom_inset).max(top_y + 320.0);

        let left_x = edge_pad;
        let right_x = sw - edge_pad - side_w;

        let (notif_x, cc_x) = match self.wing {
            PullWing::Right => (left_x, right_x),
            PullWing::Left => (right_x, left_x),
        };

        // 2a. Render Outer Notification Column (borderless directly on `.pull`)
        let notif_h = (col_bottom_y - top_y).min(680.0);
        let notif_panel = RectF::new(notif_x, top_y, side_w, notif_h);
        self.notif_panel_rect = notif_panel;
        self.render_notif_column(canvas, icons, text, pal, notifs, notif_panel, p);

        // 2b. Render Center Full-Height App Menu Column (`一直到底下`)
        let app_panel = RectF::new(app_x, top_y, app_w, col_bottom_y - top_y);
        self.app_panel_rect = app_panel;
        self.app_menu
            .render(canvas, icons, text, pal, niri, prefs, app_panel, p);

        // 2c. Render Outer Control Center Column (borderless directly on `.pull`)
        let max_cc_h = col_bottom_y - top_y;
        self.render_cc_column(
            canvas, icons, text, pal, sys, niri, prefs, cc_x, top_y, side_w, max_cc_h, p,
        );

        // Bottom subtle swipe-up pill
        if osk_bottom_inset < 10.0 {
            let pill_w = 54.0;
            let pill_h = 4.5;
            canvas.fill_round_rect(
                RectF::new((sw - pill_w) * 0.5, sh - 10.0, pill_w, pill_h),
                2.25,
                Rgba::WHITE.with_alpha(0.32 * p),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_portrait_layout(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        pal: &Palette,
        sys: &SysSnapshot,
        niri: &NiriSnapshot,
        prefs: &ShellPrefs,
        notifs: &[NotifItem],
        sw: f32,
        sh: f32,
        p: f32,
        slide_dy: f32,
        osk_bottom_inset: f32,
    ) {
        let tab_w = 260.0;
        let tab_h = 38.0;
        let tab_r = RectF::new((sw - tab_w) * 0.5, 16.0 + slide_dy, tab_w, tab_h);
        canvas.fill_round_rect(tab_r, 19.0, pal.chip.with_alpha(p));
        canvas.stroke_round_rect(tab_r, 19.0, 1.0, pal.sep.with_alpha(p));
        let inner = tab_r.inset(3.0, 3.0);
        for (ti, (tab, lbl)) in [
            (PortraitTab::ControlNotif, "控制与通知"),
            (PortraitTab::Apps, "应用菜单"),
        ]
        .into_iter()
        .enumerate()
        {
            let ir = RectF::new(inner.x + (ti as f32) * (inner.w * 0.5), inner.y, inner.w * 0.5, inner.h);
            let sel = self.portrait_tab == tab;
            if sel {
                canvas.fill_round_rect(ir, 16.0, pal.acc.with_alpha(p));
            }
            text.draw_in_rect(
                canvas,
                lbl,
                ir,
                13.5,
                FontWeight::SemiBold,
                if sel {
                    Rgba::WHITE.with_alpha(p)
                } else {
                    pal.ink3.with_alpha(p)
                },
                Align::Center,
            );
            self.hits.push((ir, CcHit::PortraitTab(tab)));
        }

        let top_y = tab_r.y + tab_h + 14.0;
        if self.portrait_tab == PortraitTab::Apps {
            self.cc_panel_rect = RectF::default();
            self.notif_panel_rect = RectF::default();
            let aw = (sw - 40.0).min(760.0);
            let panel = RectF::new(
                (sw - aw) * 0.5,
                top_y,
                aw,
                (sh - top_y - 20.0 - osk_bottom_inset).max(320.0),
            );
            self.app_panel_rect = panel;
            self.app_menu
                .render(canvas, icons, text, pal, niri, prefs, panel, p);
        } else {
            self.app_panel_rect = RectF::default();
            let col_w = ((sw - 54.0) * 0.5).clamp(360.0, 480.0);
            if sw >= 780.0 {
                let x0 = (sw - (2.0 * col_w + 18.0)) * 0.5;
                let np = RectF::new(x0, top_y, col_w, (sh - top_y - 24.0).min(780.0));
                self.notif_panel_rect = np;
                self.render_notif_column(canvas, icons, text, pal, notifs, np, p);
                self.render_cc_column(
                    canvas,
                    icons,
                    text,
                    pal,
                    sys,
                    niri,
                    prefs,
                    x0 + col_w + 18.0,
                    top_y,
                    col_w,
                    (sh - top_y - 24.0 - osk_bottom_inset).max(320.0),
                    p,
                );
            } else {
                let cw = (sw - 36.0).min(480.0);
                let cx = (sw - cw) * 0.5;
                self.notif_panel_rect = RectF::default();
                self.render_cc_column(
                    canvas,
                    icons,
                    text,
                    pal,
                    sys,
                    niri,
                    prefs,
                    cx,
                    top_y,
                    cw,
                    (sh - top_y - 24.0 - osk_bottom_inset).max(320.0),
                    p,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_notif_column(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        pal: &Palette,
        notifs: &[NotifItem],
        panel: RectF,
        alpha: f32,
    ) {
        let pad = 4.0;
        let ix = panel.x + pad;
        let iw = panel.w - 2.0 * pad;
        let hy = panel.y + 4.0;

        let hdr_lbl = if notifs.is_empty() {
            "通知".to_string()
        } else {
            format!("通知 · {}", notifs.len())
        };
        text.draw_in_rect(
            canvas,
            &hdr_lbl,
            RectF::new(ix + 2.0, hy, 160.0, 30.0),
            14.2,
            FontWeight::SemiBold,
            pal.ink3.with_alpha(alpha),
            Align::Left,
        );
        if !notifs.is_empty() {
            let clr_r = RectF::new(panel.x + panel.w - pad - 58.0, hy + 1.0, 58.0, 28.0);
            canvas.fill_round_rect(clr_r, 14.0, pal.chip.with_alpha(alpha));
            text.draw_in_rect(
                canvas,
                "清除",
                clr_r,
                12.5,
                FontWeight::Medium,
                pal.ink.with_alpha(alpha),
                Align::Center,
            );
            self.push_clipped_hit(clr_r, panel, CcHit::ClearAllNotifs);
        }

        let list_y = hy + 40.0;
        let list_h = (panel.y + panel.h - pad - list_y).max(60.0);
        let list_r = RectF::new(ix, list_y, iw, list_h);

        if notifs.is_empty() {
            self.notif_max_scroll = 0.0;
            self.notif_scroll = 0.0;
            text.draw_in_rect(
                canvas,
                "暂无通知",
                RectF::new(ix + 2.0, list_y + 2.0, iw - 4.0, 28.0),
                14.0,
                FontWeight::Normal,
                pal.ink3.with_alpha(0.68 * alpha),
                Align::Left,
            );
            return;
        }

        let item_h = 90.0_f32;
        let item_gap = 10.0_f32;
        let total_h = (notifs.len() as f32) * item_h
            + ((notifs.len().saturating_sub(1)) as f32) * item_gap;
        self.notif_max_scroll = (total_h - list_h).max(0.0);
        self.notif_scroll = self.notif_scroll.clamp(0.0, self.notif_max_scroll);

        canvas.set_clip_round_rect(Some((list_r, [20.0; 4])));
        for (ni, n) in notifs.iter().rev().enumerate() {
            let ny = list_r.y + (ni as f32) * (item_h + item_gap) - self.notif_scroll;
            if ny + item_h < list_r.y - 2.0 || ny > list_r.y + list_r.h + 2.0 {
                continue;
            }
            let nr = RectF::new(ix, ny, iw, item_h);
            canvas.fill_round_rect(nr, 20.0, pal.card.with_alpha(alpha));
            canvas.stroke_round_rect(nr, 20.0, 1.0, pal.sep.with_alpha(alpha));

            // Crafted colorful squircle app icon (`36x36`, `10.5px` radius)
            let (c_top, c_bot, glyph) = notif_app_style(&n.app_name);
            let ico_r = RectF::new(nr.x + 14.0, nr.y + 14.0, 36.0, 36.0);
            canvas.fill_v_gradient(ico_r, 10.5, c_top.with_alpha(alpha), c_bot.with_alpha(alpha));
            canvas.stroke_round_rect(ico_r, 10.5, 1.0, Rgba::WHITE.with_alpha(0.24 * alpha));
            icons.draw(
                canvas,
                glyph,
                ico_r.inset(8.5, 8.5),
                Rgba::WHITE.with_alpha(alpha),
                2.0,
            );

            let tx = nr.x + 62.0;
            let tw = (nr.w - 77.0).max(60.0);
            text.draw_in_rect(
                canvas,
                &n.app_name,
                RectF::new(tx, nr.y + 11.0, tw - 52.0, 18.0),
                12.0,
                FontWeight::Medium,
                pal.ink3.with_alpha(alpha),
                Align::Left,
            );
            text.draw_in_rect(
                canvas,
                &n.time_hm,
                RectF::new(nr.x + nr.w - 63.0, nr.y + 11.0, 48.0, 18.0),
                12.0,
                FontWeight::Normal,
                pal.ink3.with_alpha(alpha),
                Align::Right,
            );
            text.draw_in_rect(
                canvas,
                &n.summary,
                RectF::new(tx, nr.y + 31.0, tw, 22.0),
                14.2,
                FontWeight::SemiBold,
                pal.ink.with_alpha(alpha),
                Align::Left,
            );
            if !n.body.is_empty() {
                text.draw_in_rect(
                    canvas,
                    &n.body,
                    RectF::new(tx, nr.y + 54.0, tw, 22.0),
                    12.6,
                    FontWeight::Normal,
                    pal.ink3.with_alpha(alpha),
                    Align::Left,
                );
            }
            self.push_clipped_hit(nr, list_r, CcHit::DismissNotif(n.id));
        }
        canvas.set_clip_round_rect(None);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_cc_column(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        pal: &Palette,
        sys: &SysSnapshot,
        _niri: &NiriSnapshot,
        prefs: &ShellPrefs,
        col_x: f32,
        top_y: f32,
        col_w: f32,
        max_h: f32,
        alpha: f32,
    ) {
        let pad = 4.0_f32;
        let vgap = 11.0_f32;
        let iw = col_w - 2.0 * pad;
        self.wifi_drawer_rect = RectF::default();
        self.bt_drawer_rect = RectF::default();

        // Compute exact natural height of Control Center stack:
        // Stacked Header (66) + [Power row] + vgap + Wi-Fi & BT Row (66) + [Wi-Fi/BT drawer] + vgap
        // + Full-width Volume & Brightness Bars (2*54 + 10) + vgap + Media (68) + vgap + 4x2 Quick Toggles Box
        let header_h = 66.0_f32;
        let power_full_h = 50.0_f32;
        let pp = self.power_spring.pos.clamp(0.0, 1.0);
        let power_cur_h = if pp > 0.005 {
            vgap * pp + (power_full_h * pp).max(1.0)
        } else {
            0.0
        };

        let hero_card_h = 66.0_f32;
        let hero_gap = 10.0_f32;
        let slider_h = 54.0_f32;
        let slider_gap = 10.0_f32;
        let sliders_total_h = 2.0 * slider_h + slider_gap;

        let wifi_hdr_h = 34.0_f32;
        let has_pw_row = sys.wifi_on
            && self
                .wifi_pw_ssid
                .as_ref()
                .is_some_and(|s| sys.wifi_list.iter().any(|n| &n.ssid == s));
        let pw_row_h = if has_pw_row { 44.0_f32 } else { 0.0_f32 };
        let (wifi_full_h, wifi_list_vis_h) = if !sys.wifi_on || sys.wifi_list.is_empty() {
            self.wifi_max_scroll = 0.0;
            self.wifi_scroll = 0.0;
            (wifi_hdr_h + 44.0, 0.0)
        } else {
            let wifi_content_h = (sys.wifi_list.len() as f32) * 38.0 + pw_row_h;
            let vis = wifi_content_h.min(5.0 * 38.0 + pw_row_h.min(22.0));
            self.wifi_max_scroll = (wifi_content_h - vis).max(0.0);
            self.wifi_scroll = self.wifi_scroll.clamp(0.0, self.wifi_max_scroll);
            (wifi_hdr_h + vis + 8.0, vis)
        };
        let wp = self.wifi_spring.pos.clamp(0.0, 1.0);
        let wifi_cur_h = if wp > 0.005 {
            vgap * wp + (wifi_full_h * wp).max(1.0)
        } else {
            0.0
        };

        let bt_hdr_h = 34.0_f32;
        let bt_items = build_bt_drawer_items(sys);
        let (bt_full_h, bt_list_vis_h) = if !sys.bt_on || bt_items.is_empty() {
            self.bt_max_scroll = 0.0;
            self.bt_scroll = 0.0;
            (bt_hdr_h + 44.0, 0.0)
        } else {
            let bt_content_h = (bt_items.len() as f32) * 38.0;
            let vis = bt_content_h.min(5.0 * 38.0);
            self.bt_max_scroll = (bt_content_h - vis).max(0.0);
            self.bt_scroll = self.bt_scroll.clamp(0.0, self.bt_max_scroll);
            (bt_hdr_h + vis + 8.0, vis)
        };
        let bp = self.bt_spring.pos.clamp(0.0, 1.0);
        let bt_cur_h = if bp > 0.005 {
            vgap * bp + (bt_full_h * bp).max(1.0)
        } else {
            0.0
        };

        let media_h = 68.0_f32;
        let toggles = build_quick_toggles(sys, prefs);
        let qt_cols = 4usize;
        let qt_rows = toggles.len().div_ceil(qt_cols).max(1);
        let qt_pad_y = 13.0_f32;
        let qt_pad_x = 10.0_f32;
        let qt_row_h = 70.0_f32;
        let qt_row_gap = 10.0_f32;
        let qt_box_h = 2.0 * qt_pad_y
            + (qt_rows as f32) * qt_row_h
            + ((qt_rows.saturating_sub(1)) as f32) * qt_row_gap;

        let content_h = header_h
            + power_cur_h
            + vgap
            + hero_card_h
            + wifi_cur_h
            + bt_cur_h
            + vgap
            + sliders_total_h
            + vgap
            + media_h
            + vgap
            + qt_box_h;
        let natural_panel_h = content_h + 2.0 * pad;
        let panel_h = natural_panel_h.min(max_h);

        self.cc_max_scroll = (natural_panel_h - panel_h).max(0.0);
        self.cc_scroll = self.cc_scroll.clamp(0.0, self.cc_max_scroll);

        let panel = RectF::new(col_x, top_y, col_w, panel_h);
        self.cc_panel_rect = panel;

        let body_clip = panel;
        canvas.set_clip_round_rect(Some((body_clip, [0.0; 4])));

        let ix = col_x + pad;
        let mut cy = top_y + pad - self.cc_scroll;

        // --- Block 1: Stacked Clock + Date Header (`.phead`) + Single Power Button (`电源键`) ---
        text.draw_in_rect(
            canvas,
            &sys.time_hm,
            RectF::new(ix, cy - 2.0, 180.0, 44.0),
            42.0,
            FontWeight::Normal,
            pal.ink.with_alpha(alpha),
            Align::Left,
        );
        let date_lbl = if !sys.date_long.is_empty() {
            sys.date_long.as_str()
        } else {
            sys.date_short.as_str()
        };
        text.draw_in_rect(
            canvas,
            date_lbl,
            RectF::new(ix + 2.0, cy + 43.0, 210.0, 20.0),
            13.0,
            FontWeight::Normal,
            pal.ink3.with_alpha(alpha),
            Align::Left,
        );
        let pwr_btn = RectF::new(ix + iw - 38.0, cy + 25.0, 38.0, 38.0);
        if self.power_open {
            canvas.fill_round_rect(pwr_btn, 19.0, Rgba::hex(0xef4444).with_alpha(alpha));
        } else {
            canvas.fill_round_rect(pwr_btn, 19.0, pal.chip.with_alpha(alpha));
            canvas.stroke_round_rect(pwr_btn, 19.0, 1.0, pal.sep.with_alpha(alpha));
        }
        icons.draw(
            canvas,
            "power",
            pwr_btn.inset(10.0, 10.0),
            if self.power_open {
                Rgba::WHITE.with_alpha(alpha)
            } else {
                pal.ink.with_alpha(0.92 * alpha)
            },
            2.05,
        );
        self.push_clipped_hit(pwr_btn, body_clip, CcHit::HeaderPowerToggle);
        cy += header_h;

        // --- Block 1b: Expandable Power Menu (`重启` / `进安卓` / `关机`) ---
        if pp > 0.005 {
            cy += vgap * pp;
            let vis_h = (power_full_h * pp).max(1.0);
            let pr = RectF::new(ix, cy, iw, vis_h);
            canvas.set_clip_round_rect(Some((pr, [18.0; 4])));

            let btn_gap = 8.0_f32;
            let w3 = ((iw - 2.0 * btn_gap) / 3.0).floor();
            let reboot_r = RectF::new(ix, cy, w3, power_full_h);
            let and_r = RectF::new(ix + w3 + btn_gap, cy, w3, power_full_h);
            let off_r = RectF::new(
                ix + 2.0 * (w3 + btn_gap),
                cy,
                iw - 2.0 * (w3 + btn_gap),
                power_full_h,
            );
            let and_armed = self
                .android_confirm_until
                .is_some_and(|u| Instant::now() < u);

            // 1. Reboot
            canvas.fill_round_rect(reboot_r, 17.0, pal.card.with_alpha(alpha * pp));
            canvas.stroke_round_rect(reboot_r, 17.0, 1.0, pal.sep.with_alpha(alpha * pp));
            icons.draw(
                canvas,
                "restart",
                RectF::new(
                    reboot_r.x + reboot_r.w * 0.5 - 30.0,
                    reboot_r.y + (power_full_h - 17.5) * 0.5,
                    17.5,
                    17.5,
                ),
                pal.ink.with_alpha(alpha * pp),
                2.0,
            );
            text.draw_in_rect(
                canvas,
                "重启",
                RectF::new(
                    reboot_r.x + reboot_r.w * 0.5 - 8.0,
                    reboot_r.y,
                    48.0,
                    power_full_h,
                ),
                13.5,
                FontWeight::SemiBold,
                pal.ink.with_alpha(alpha * pp),
                Align::Left,
            );

            // 2. Reboot to Android (2-tap confirm so single accidental touch never reboots)
            if and_armed {
                canvas.fill_round_rect(
                    and_r,
                    17.0,
                    Rgba::hex(0x059669).with_alpha(0.92 * alpha * pp),
                );
                canvas.stroke_round_rect(
                    and_r,
                    17.0,
                    1.0,
                    Rgba::WHITE.with_alpha(0.24 * alpha * pp),
                );
                text.draw_in_rect(
                    canvas,
                    "点此确认",
                    and_r,
                    13.2,
                    FontWeight::SemiBold,
                    Rgba::WHITE.with_alpha(alpha * pp),
                    Align::Center,
                );
            } else {
                canvas.fill_round_rect(and_r, 17.0, pal.card.with_alpha(alpha * pp));
                canvas.stroke_round_rect(and_r, 17.0, 1.0, pal.sep.with_alpha(alpha * pp));
                icons.draw(
                    canvas,
                    "rotate",
                    RectF::new(
                        and_r.x + and_r.w * 0.5 - 36.0,
                        and_r.y + (power_full_h - 17.0) * 0.5,
                        17.0,
                        17.0,
                    ),
                    pal.ink.with_alpha(alpha * pp),
                    2.0,
                );
                text.draw_in_rect(
                    canvas,
                    "进安卓",
                    RectF::new(
                        and_r.x + and_r.w * 0.5 - 14.0,
                        and_r.y,
                        56.0,
                        power_full_h,
                    ),
                    13.5,
                    FontWeight::SemiBold,
                    pal.ink.with_alpha(alpha * pp),
                    Align::Left,
                );
            }

            // 3. Shutdown
            canvas.fill_round_rect(off_r, 17.0, Rgba::hex(0xdc2626).with_alpha(0.90 * alpha * pp));
            canvas.stroke_round_rect(
                off_r,
                17.0,
                1.0,
                Rgba::WHITE.with_alpha(0.20 * alpha * pp),
            );
            icons.draw(
                canvas,
                "power",
                RectF::new(
                    off_r.x + off_r.w * 0.5 - 30.0,
                    off_r.y + (power_full_h - 17.5) * 0.5,
                    17.5,
                    17.5,
                ),
                Rgba::WHITE.with_alpha(alpha * pp),
                2.0,
            );
            text.draw_in_rect(
                canvas,
                "关机",
                RectF::new(
                    off_r.x + off_r.w * 0.5 - 8.0,
                    off_r.y,
                    48.0,
                    power_full_h,
                ),
                13.5,
                FontWeight::SemiBold,
                Rgba::WHITE.with_alpha(alpha * pp),
                Align::Left,
            );

            if pp > 0.75 {
                if let Some(pclip) = pr.intersect(body_clip) {
                    self.push_clipped_hit(reboot_r, pclip, CcHit::PowerReboot);
                    self.push_clipped_hit(and_r, pclip, CcHit::PowerRebootAndroid);
                    self.push_clipped_hit(off_r, pclip, CcHit::PowerShutdown);
                }
            }
            canvas.set_clip_round_rect(Some((body_clip, [0.0; 4])));
            cy += vis_h;
        }
        cy += vgap;

        // --- Block 2: Same-Row Wi-Fi + Bluetooth Hero Cards (`1fr : 1fr`) ---
        let left_w = ((iw - hero_gap) * 0.5).round();
        let right_w = iw - hero_gap - left_w;
        let rx = ix + left_w + hero_gap;

        // Wi-Fi Hero Card (Left)
        let wifi_r = RectF::new(ix, cy, left_w, hero_card_h);
        let wifi_sub = if !sys.wifi_on {
            "已关闭"
        } else if sys.wifi_ssid.is_empty() {
            "未连接"
        } else {
            sys.wifi_ssid.as_str()
        };
        draw_expandable_hero_card(
            canvas,
            icons,
            text,
            pal,
            wifi_r,
            if sys.wifi_on { "wifi" } else { "wifi_off" },
            "Wi-Fi",
            wifi_sub,
            sys.wifi_on,
            self.wifi_open,
            alpha,
        );
        self.push_clipped_hit(
            RectF::new(wifi_r.x, wifi_r.y, 50.0, wifi_r.h),
            body_clip,
            CcHit::WifiToggle,
        );
        self.push_clipped_hit(
            RectF::new(wifi_r.x + 50.0, wifi_r.y, wifi_r.w - 50.0, wifi_r.h),
            body_clip,
            CcHit::WifiExpand,
        );

        // Bluetooth Hero Card (Right, same row!)
        let bt_r = RectF::new(rx, cy, right_w, hero_card_h);
        let ble_conn_cnt = sys.bt_peripherals.iter().filter(|d| d.connected).count();
        let bt_sub_owned = if !sys.bt_on {
            "已关闭".to_string()
        } else if ble_conn_cnt > 1 {
            format!("{ble_conn_cnt} 台已连接")
        } else if !sys.bt_device.is_empty() {
            sys.bt_device.clone()
        } else if is_bt_discovering() {
            "搜索中…".to_string()
        } else {
            "已开启".to_string()
        };
        draw_expandable_hero_card(
            canvas,
            icons,
            text,
            pal,
            bt_r,
            if sys.bt_on { "bluetooth" } else { "bluetooth_off" },
            "蓝牙",
            &bt_sub_owned,
            sys.bt_on,
            self.bt_open,
            alpha,
        );
        self.push_clipped_hit(
            RectF::new(bt_r.x, bt_r.y, 50.0, bt_r.h),
            body_clip,
            CcHit::BtToggle,
        );
        self.push_clipped_hit(
            RectF::new(bt_r.x + 50.0, bt_r.y, bt_r.w - 50.0, bt_r.h),
            body_clip,
            CcHit::BtExpand,
        );
        cy += hero_card_h;

        // --- Block 2b: Quick-Connect Wi-Fi Drawer (Active Scan + Saved/Open 1-Tap + Inline WPA Password) ---
        if wp > 0.005 {
            cy += vgap * wp;
            let vis_h = (wifi_full_h * wp).max(1.0);
            let dr = RectF::new(ix, cy, iw, vis_h);
            self.wifi_drawer_rect = dr.intersect(body_clip).unwrap_or_default();
            canvas.fill_round_rect(dr, 18.0, pal.card.with_alpha(alpha * wp));
            canvas.stroke_round_rect(dr, 18.0, 1.0, pal.sep.with_alpha(alpha * wp));
            canvas.set_clip_round_rect(Some((dr, [18.0; 4])));

            let scanning = self.wifi_scan_until.is_some_and(|t| Instant::now() < t);
            text.draw_in_rect(
                canvas,
                "可用 WLAN",
                RectF::new(dr.x + 14.0, dr.y + 5.0, 120.0, 26.0),
                12.2,
                FontWeight::SemiBold,
                pal.ink3.with_alpha(alpha * wp),
                Align::Left,
            );
            if sys.wifi_on {
                let scan_btn = RectF::new(dr.x + dr.w - 76.0, dr.y + 5.0, 66.0, 25.0);
                canvas.fill_round_rect(scan_btn, 12.5, pal.chip.with_alpha(alpha * wp));
                text.draw_in_rect(
                    canvas,
                    if scanning { "搜索中…" } else { "刷新" },
                    scan_btn,
                    11.8,
                    FontWeight::Medium,
                    if scanning {
                        pal.active_drawer_ink.with_alpha(alpha * wp)
                    } else {
                        pal.ink2.with_alpha(alpha * wp)
                    },
                    Align::Center,
                );
                if wp > 0.75 {
                    if let Some(dclip) = dr.intersect(body_clip) {
                        self.push_clipped_hit(scan_btn, dclip, CcHit::WifiRescan);
                    }
                }
            }

            if !sys.wifi_on {
                text.draw_in_rect(
                    canvas,
                    "Wi-Fi 已关闭",
                    RectF::new(dr.x + 16.0, dr.y + wifi_hdr_h, dr.w - 32.0, 38.0),
                    13.5,
                    FontWeight::Medium,
                    pal.ink2.with_alpha(alpha * wp),
                    Align::Left,
                );
            } else if sys.wifi_list.is_empty() {
                text.draw_in_rect(
                    canvas,
                    "正在扫描附近网络…",
                    RectF::new(dr.x + 16.0, dr.y + wifi_hdr_h, dr.w - 32.0, 38.0),
                    13.5,
                    FontWeight::Medium,
                    pal.ink2.with_alpha(alpha * wp),
                    Align::Left,
                );
            } else {
                let list_vp = RectF::new(dr.x + 4.0, dr.y + wifi_hdr_h, dr.w - 8.0, wifi_list_vis_h);
                let eff_vp = list_vp.intersect(dr).unwrap_or_default();
                canvas.set_clip_round_rect(Some((eff_vp, [12.0; 4])));

                let mut ry = list_vp.y - self.wifi_scroll;
                for net in &sys.wifi_list {
                    let row_r = RectF::new(dr.x + 8.0, ry + 2.0, dr.w - 16.0, 34.0);
                    let is_connecting = self
                        .wifi_connecting
                        .as_ref()
                        .is_some_and(|(s, t)| s == &net.ssid && t.elapsed().as_secs() < 12)
                        && !net.active;
                    let pw_open_here = self.wifi_pw_ssid.as_deref() == Some(net.ssid.as_str());

                    if row_r.y + row_r.h >= eff_vp.y - 2.0 && row_r.y <= eff_vp.y + eff_vp.h + 2.0 {
                        if net.active || pw_open_here {
                            canvas.fill_round_rect(row_r, 11.0, pal.acc_soft.with_alpha(alpha * wp));
                        }
                        let fg = if net.active || pw_open_here {
                            pal.active_drawer_ink.with_alpha(alpha * wp)
                        } else {
                            pal.ink.with_alpha(alpha * wp)
                        };
                        icons.draw(
                            canvas,
                            "wifi",
                            RectF::new(row_r.x + 10.0, row_r.y + 8.5, 17.0, 17.0),
                            fg,
                            1.95,
                        );
                        let ssid_fit = fit_text_ellipsis(
                            text,
                            &net.ssid,
                            13.4,
                            if net.active {
                                FontWeight::SemiBold
                            } else {
                                FontWeight::Medium
                            },
                            canvas.scale,
                            (row_r.w - 134.0).max(60.0),
                        );
                        text.draw_in_rect(
                            canvas,
                            &ssid_fit,
                            RectF::new(row_r.x + 34.0, row_r.y, row_r.w - 130.0, row_r.h),
                            13.4,
                            if net.active {
                                FontWeight::SemiBold
                            } else {
                                FontWeight::Medium
                            },
                            fg,
                            Align::Left,
                        );

                        let right_lbl = if net.active {
                            "已连接".to_string()
                        } else if is_connecting {
                            "连接中…".to_string()
                        } else if net.saved {
                            format!("已保存 · {}%", net.signal)
                        } else {
                            format!("{}%", net.signal)
                        };
                        if !net.active && !is_connecting && !net.saved && net.secured {
                            icons.draw(
                                canvas,
                                "lock",
                                RectF::new(row_r.x + row_r.w - 60.0, row_r.y + 9.5, 14.5, 14.5),
                                pal.ink3.with_alpha(alpha * wp),
                                1.85,
                            );
                        }
                        text.draw_in_rect(
                            canvas,
                            &right_lbl,
                            RectF::new(row_r.x + row_r.w - 98.0, row_r.y, 88.0, row_r.h),
                            11.8,
                            if net.active || is_connecting {
                                FontWeight::SemiBold
                            } else {
                                FontWeight::Medium
                            },
                            if net.active || is_connecting {
                                pal.active_drawer_ink.with_alpha(alpha * wp)
                            } else {
                                pal.ink3.with_alpha(alpha * wp)
                            },
                            Align::Right,
                        );

                        if wp > 0.75 {
                            if let Some(dclip) = eff_vp.intersect(body_clip) {
                                self.push_clipped_hit(
                                    row_r,
                                    dclip,
                                    CcHit::WifiSelect {
                                        ssid: net.ssid.clone(),
                                        secured: net.secured,
                                        saved: net.saved,
                                        active: net.active,
                                    },
                                );
                            }
                        }
                    }
                    ry += 38.0;

                    if pw_open_here {
                        let pw_y = ry + 2.0;
                        let inp_r = RectF::new(dr.x + 12.0, pw_y, dr.w - 86.0, 36.0);
                        let sub_r = RectF::new(dr.x + dr.w - 68.0, pw_y, 56.0, 36.0);
                        if pw_y + 38.0 >= eff_vp.y - 2.0 && pw_y <= eff_vp.y + eff_vp.h + 2.0 {
                            let well = if pal.light {
                                pal.chip.with_alpha(alpha * wp)
                            } else {
                                Rgba::rgba_f(0, 0, 0, 0.28 * alpha * wp)
                            };
                            canvas.fill_round_rect(inp_r, 12.0, well);
                            canvas.stroke_round_rect(
                                inp_r,
                                12.0,
                                1.3,
                                pal.acc.with_alpha(alpha * wp),
                            );
                            let pw_empty = self.wifi_pw_buf.is_empty();
                            let pw_disp = if pw_empty {
                                "输入 Wi-Fi 密码…".to_string()
                            } else {
                                format!("{}|", "•".repeat(self.wifi_pw_buf.chars().count()))
                            };
                            text.draw_in_rect(
                                canvas,
                                &pw_disp,
                                RectF::new(inp_r.x + 11.0, inp_r.y, inp_r.w - 22.0, inp_r.h),
                                12.6,
                                FontWeight::Medium,
                                if pw_empty {
                                    pal.ink3.with_alpha(alpha * wp)
                                } else {
                                    pal.ink.with_alpha(alpha * wp)
                                },
                                Align::Left,
                            );

                            let can_sub = !self.wifi_pw_buf.trim().is_empty();
                            canvas.fill_round_rect(
                                sub_r,
                                12.0,
                                if can_sub {
                                    pal.acc.with_alpha(alpha * wp)
                                } else {
                                    pal.chip.with_alpha(alpha * wp)
                                },
                            );
                            text.draw_in_rect(
                                canvas,
                                "连接",
                                sub_r,
                                12.6,
                                FontWeight::SemiBold,
                                if can_sub {
                                    Rgba::WHITE.with_alpha(alpha * wp)
                                } else {
                                    pal.ink3.with_alpha(alpha * wp)
                                },
                                Align::Center,
                            );
                            if wp > 0.75 {
                                if let Some(dclip) = eff_vp.intersect(body_clip) {
                                    self.push_clipped_hit(inp_r, dclip, CcHit::WifiPasswordInput);
                                    self.push_clipped_hit(sub_r, dclip, CcHit::WifiSubmitPassword);
                                }
                            }
                        }
                        ry += pw_row_h;
                    }
                }
            }
            canvas.set_clip_round_rect(Some((body_clip, [0.0; 4])));
            cy += vis_h;
        }

        // --- Block 2c: Quick-Connect Bluetooth Drawer (Live Discovery + Paired & Nearby 1-Tap Connect) ---
        if bp > 0.005 {
            cy += vgap * bp;
            let vis_h = (bt_full_h * bp).max(1.0);
            let dr = RectF::new(ix, cy, iw, vis_h);
            self.bt_drawer_rect = dr.intersect(body_clip).unwrap_or_default();
            canvas.fill_round_rect(dr, 18.0, pal.card.with_alpha(alpha * bp));
            canvas.stroke_round_rect(dr, 18.0, 1.0, pal.sep.with_alpha(alpha * bp));
            canvas.set_clip_round_rect(Some((dr, [18.0; 4])));

            let discovering = is_bt_discovering();
            text.draw_in_rect(
                canvas,
                "蓝牙设备",
                RectF::new(dr.x + 14.0, dr.y + 5.0, 120.0, 26.0),
                12.2,
                FontWeight::SemiBold,
                pal.ink3.with_alpha(alpha * bp),
                Align::Left,
            );
            if sys.bt_on {
                let scan_btn = RectF::new(dr.x + dr.w - 78.0, dr.y + 5.0, 68.0, 25.0);
                canvas.fill_round_rect(scan_btn, 12.5, pal.chip.with_alpha(alpha * bp));
                text.draw_in_rect(
                    canvas,
                    if discovering { "搜索中…" } else { "搜索设备" },
                    scan_btn,
                    11.6,
                    FontWeight::Medium,
                    if discovering {
                        pal.active_drawer_ink.with_alpha(alpha * bp)
                    } else {
                        pal.ink2.with_alpha(alpha * bp)
                    },
                    Align::Center,
                );
                if bp > 0.75 {
                    if let Some(dclip) = dr.intersect(body_clip) {
                        self.push_clipped_hit(scan_btn, dclip, CcHit::BtRescan);
                    }
                }
            }

            if !sys.bt_on {
                text.draw_in_rect(
                    canvas,
                    "蓝牙已关闭",
                    RectF::new(dr.x + 16.0, dr.y + bt_hdr_h, dr.w - 32.0, 38.0),
                    13.5,
                    FontWeight::Medium,
                    pal.ink2.with_alpha(alpha * bp),
                    Align::Left,
                );
            } else if bt_items.is_empty() {
                text.draw_in_rect(
                    canvas,
                    if discovering {
                        "正在搜索附近可连接蓝牙设备…"
                    } else {
                        "未发现蓝牙设备，点右上角搜索"
                    },
                    RectF::new(dr.x + 16.0, dr.y + bt_hdr_h, dr.w - 32.0, 38.0),
                    13.2,
                    FontWeight::Medium,
                    pal.ink2.with_alpha(alpha * bp),
                    Align::Left,
                );
            } else {
                let list_vp = RectF::new(dr.x + 4.0, dr.y + bt_hdr_h, dr.w - 8.0, bt_list_vis_h);
                let eff_vp = list_vp.intersect(dr).unwrap_or_default();
                canvas.set_clip_round_rect(Some((eff_vp, [12.0; 4])));

                for (ri, dev) in bt_items.iter().enumerate() {
                    let ry = list_vp.y + (ri as f32) * 38.0 - self.bt_scroll;
                    let row_r = RectF::new(dr.x + 8.0, ry + 2.0, dr.w - 16.0, 34.0);
                    if row_r.y + row_r.h < eff_vp.y - 2.0 || row_r.y > eff_vp.y + eff_vp.h + 2.0 {
                        continue;
                    }
                    let is_qi_only = dev.mac == "QI:PEN";
                    let pending_act = self
                        .bt_connecting
                        .as_ref()
                        .filter(|(m, _, t)| m == &dev.mac && t.elapsed().as_secs() < 10)
                        .map(|(_, want_conn, _)| *want_conn);

                    if dev.connected && !is_qi_only {
                        canvas.fill_round_rect(row_r, 11.0, pal.acc_soft.with_alpha(alpha * bp));
                    }
                    let fg = if dev.connected && !is_qi_only {
                        pal.active_drawer_ink.with_alpha(alpha * bp)
                    } else {
                        pal.ink.with_alpha(alpha * bp)
                    };
                    icons.draw(
                        canvas,
                        dev.kind.icon_name(),
                        RectF::new(row_r.x + 10.0, row_r.y + 8.5, 17.0, 17.0),
                        fg,
                        1.95,
                    );
                    let dev_fit = fit_text_ellipsis(
                        text,
                        &dev.name,
                        13.4,
                        if dev.connected && !is_qi_only {
                            FontWeight::SemiBold
                        } else {
                            FontWeight::Medium
                        },
                        canvas.scale,
                        (row_r.w - 134.0).max(60.0),
                    );
                    text.draw_in_rect(
                        canvas,
                        &dev_fit,
                        RectF::new(row_r.x + 34.0, row_r.y, row_r.w - 130.0, row_r.h),
                        13.4,
                        if dev.connected && !is_qi_only {
                            FontWeight::SemiBold
                        } else {
                            FontWeight::Medium
                        },
                        fg,
                        Align::Left,
                    );

                    let right_stat = if let Some(want_conn) = pending_act {
                        if want_conn && !dev.connected {
                            if dev.paired {
                                "连接中…".to_string()
                            } else {
                                "配对中…".to_string()
                            }
                        } else if !want_conn && dev.connected {
                            "断开中…".to_string()
                        } else if dev.connected {
                            match dev.battery_pct {
                                Some(pct) => format!("{pct}% · 已连接"),
                                None => "已连接".into(),
                            }
                        } else {
                            "已配对".into()
                        }
                    } else if is_qi_only {
                        match dev.battery_pct {
                            Some(pct) => format!("磁吸 · {pct}%"),
                            None => "磁吸在位".into(),
                        }
                    } else if dev.connected {
                        match dev.battery_pct {
                            Some(pct) => format!("{pct}% · 已连接"),
                            None => "已连接".into(),
                        }
                    } else if dev.paired {
                        "点击连接".into()
                    } else {
                        "点击配对".into()
                    };

                    let stat_col = if dev.battery_pct.is_some_and(|pct| pct <= 20) && dev.connected {
                        pal.warn.with_alpha(alpha * bp)
                    } else if (dev.connected && !is_qi_only) || !dev.paired || pending_act.is_some()
                    {
                        pal.active_drawer_ink.with_alpha(alpha * bp)
                    } else {
                        pal.ink3.with_alpha(alpha * bp)
                    };
                    text.draw_in_rect(
                        canvas,
                        &right_stat,
                        RectF::new(row_r.x + row_r.w - 98.0, row_r.y, 88.0, row_r.h),
                        11.8,
                        if dev.connected || !dev.paired {
                            FontWeight::SemiBold
                        } else {
                            FontWeight::Medium
                        },
                        stat_col,
                        Align::Right,
                    );

                    if bp > 0.75 && !is_qi_only {
                        if let Some(dclip) = eff_vp.intersect(body_clip) {
                            self.push_clipped_hit(
                                row_r,
                                dclip,
                                CcHit::BtDevice {
                                    mac: dev.mac.clone(),
                                    connected: dev.connected,
                                    paired: dev.paired,
                                },
                            );
                        }
                    }
                }
            }
            canvas.set_clip_round_rect(Some((body_clip, [0.0; 4])));
            cy += vis_h;
        }

        // --- Block 3: Full-Width Horizontal Volume & Brightness Bars (same width as Media Control) ---
        cy += vgap;
        let vol_r = RectF::new(ix, cy, iw, slider_h);
        self.vol_rect = vol_r
            .intersect(body_clip)
            .filter(|r| r.h >= 12.0)
            .unwrap_or_default();
        let vol_frac = if sys.muted {
            0.0
        } else {
            (sys.volume_pct as f32) / 100.0
        };
        let vol_lbl = if sys.muted {
            "静音".into()
        } else {
            format!("{}%", sys.volume_pct)
        };
        draw_origin_pill_slider(
            canvas,
            icons,
            text,
            pal,
            vol_r,
            vol_frac,
            if sys.muted { "volume_off" } else { "sound" },
            "",
            &vol_lbl,
            1.0,
        );
        canvas.stroke_round_rect(vol_r, 20.0, 1.0, pal.sep.with_alpha(alpha));
        cy += slider_h + slider_gap;

        let bri_r = RectF::new(ix, cy, iw, slider_h);
        self.bri_rect = bri_r
            .intersect(body_clip)
            .filter(|r| r.h >= 12.0)
            .unwrap_or_default();
        draw_origin_pill_slider(
            canvas,
            icons,
            text,
            pal,
            bri_r,
            (sys.brightness_pct as f32) / 100.0,
            "sun",
            "",
            &format!("{}%", sys.brightness_pct),
            1.0,
        );
        canvas.stroke_round_rect(bri_r, 20.0, 1.0, pal.sep.with_alpha(alpha));
        cy += slider_h;

        // --- Block 4: MPRIS Media Card ---
        cy += vgap;
        let mr = RectF::new(ix, cy, iw, media_h);
        canvas.fill_round_rect(mr, 20.0, pal.card.with_alpha(alpha));
        canvas.stroke_round_rect(mr, 20.0, 1.0, pal.sep.with_alpha(alpha));

        let art_r = RectF::new(mr.x + 14.0, mr.y + (media_h - 44.0) * 0.5, 44.0, 44.0);
        let idle_media = sys.media_title.is_empty() && !sys.media_playing;
        if idle_media {
            canvas.fill_round_rect(art_r, 12.0, pal.chip.with_alpha(alpha));
            icons.draw(
                canvas,
                "music",
                art_r.inset(12.0, 12.0),
                pal.ink3.with_alpha(alpha),
                1.9,
            );
            text.draw_in_rect(
                canvas,
                "暂无播放",
                RectF::new(art_r.x + 56.0, mr.y, mr.w - 196.0, media_h),
                14.0,
                FontWeight::Medium,
                pal.ink3.with_alpha(alpha),
                Align::Left,
            );
        } else {
            canvas.fill_v_gradient(
                art_r,
                12.0,
                Rgba::hex(0xa855f7).with_alpha(alpha),
                Rgba::hex(0xec4899).with_alpha(alpha),
            );
            canvas.fill_radial_glow(
                art_r,
                12.0,
                art_r.x + art_r.w * 0.85,
                art_r.y + art_r.h * 0.88,
                art_r.w * 0.85,
                Rgba::hex(0xfb923c).with_alpha(0.75 * alpha),
            );
            icons.draw(
                canvas,
                "music",
                art_r.inset(12.5, 12.5),
                Rgba::WHITE.with_alpha(0.92 * alpha),
                1.9,
            );

            let m_title = if sys.media_title.is_empty() {
                "正在播放"
            } else {
                sys.media_title.as_str()
            };
            let m_sub = if sys.media_artist.is_empty() {
                if sys.media_playing { "正在播放" } else { "已暂停" }
            } else {
                sys.media_artist.as_str()
            };
            text.draw_in_rect(
                canvas,
                m_title,
                RectF::new(art_r.x + 56.0, mr.y + 15.0, mr.w - 196.0, 22.0),
                13.8,
                FontWeight::SemiBold,
                pal.ink.with_alpha(alpha),
                Align::Left,
            );
            text.draw_in_rect(
                canvas,
                m_sub,
                RectF::new(art_r.x + 56.0, mr.y + 37.0, mr.w - 196.0, 19.0),
                12.0,
                FontWeight::Normal,
                pal.ink3.with_alpha(alpha),
                Align::Left,
            );
        }

        for (mi, (ic, hit, main_btn)) in [
            ("skip_prev", CcHit::MediaPrev, false),
            (
                if sys.media_playing { "pause" } else { "play" },
                CcHit::MediaPlayPause,
                true,
            ),
            ("skip_next", CcHit::MediaNext, false),
        ]
        .into_iter()
        .enumerate()
        {
            let bsz = 34.0_f32;
            let bx = mr.x + mr.w - 122.0 + (mi as f32) * 38.0;
            let br = RectF::new(bx, mr.y + (media_h - bsz) * 0.5, bsz, bsz);
            if main_btn {
                canvas.fill_round_rect(br, 17.0, pal.chip.with_alpha(alpha));
            }
            icons.draw(
                canvas,
                ic,
                br.inset(9.5, 9.5),
                pal.ink.with_alpha(0.92 * alpha),
                1.9,
            );
            self.push_clipped_hit(br, body_clip, hit);
        }
        cy += media_h;

        // --- Block 4 (BOTTOMMOST): Expanded Quick Toggles Box (`.ztiles-box`) ---
        cy += vgap;
        let qt_box = RectF::new(ix, cy, iw, qt_box_h);
        canvas.fill_round_rect(qt_box, 24.0, pal.card.with_alpha(alpha));
        canvas.stroke_round_rect(qt_box, 24.0, 1.0, pal.sep.with_alpha(alpha));

        let grid_x = qt_box.x + qt_pad_x;
        let grid_w = qt_box.w - 2.0 * qt_pad_x;
        let cell_w = grid_w / (qt_cols as f32);
        let orb_sz = 50.0_f32;

        for (ti, &(qid, ic, lbl, active)) in toggles.iter().enumerate() {
            let row = ti / qt_cols;
            let col = ti % qt_cols;
            let tx = grid_x + (col as f32) * cell_w;
            let ty = qt_box.y + qt_pad_y + (row as f32) * (qt_row_h + qt_row_gap);
            let orb_r = RectF::new(tx + (cell_w - orb_sz) * 0.5, ty, orb_sz, orb_sz);

            if active {
                canvas.fill_round_rect(orb_r, orb_sz * 0.5, pal.acc.with_alpha(alpha));
            } else {
                canvas.fill_round_rect(orb_r, orb_sz * 0.5, pal.chip.with_alpha(alpha));
            }
            icons.draw(
                canvas,
                ic,
                orb_r.inset(14.0, 14.0),
                if active {
                    Rgba::WHITE.with_alpha(alpha)
                } else {
                    pal.ink.with_alpha(0.88 * alpha)
                },
                1.95,
            );
            text.draw_in_rect(
                canvas,
                lbl,
                RectF::new(tx - 2.0, orb_r.y + orb_sz + 4.0, cell_w + 4.0, 18.0),
                12.4,
                if active {
                    FontWeight::SemiBold
                } else {
                    FontWeight::Normal
                },
                if active {
                    pal.ink.with_alpha(alpha)
                } else {
                    pal.ink3.with_alpha(alpha)
                },
                Align::Center,
            );
            self.push_clipped_hit(
                RectF::new(tx, ty, cell_w, qt_row_h),
                body_clip,
                CcHit::QuickToggle(qid),
            );
        }

        canvas.set_clip_round_rect(None);
    }

    pub fn on_touch_down(
        &mut self,
        slot: i32,
        x: f32,
        y: f32,
        sys: &mut SysSnapshot,
    ) -> CurtainEffect {
        // 0. Bottom grab-handle zone (`sh - 42..sh`): always route to 1:1 swipe-up dismiss
        if self.last_screen_h > 200.0
            && !self.app_menu.search_focused
            && self.wifi_pw_ssid.is_none()
            && y >= self.last_screen_h - 42.0
        {
            self.gesture = ActiveGesture::ScrimOrSwipeUp {
                slot,
                x0: x,
                y0: y,
                last_y: y,
                last_t: Instant::now(),
                vy: 0.0,
                tap_ok: true,
            };
            return CurtainEffect::None;
        }

        // 1. Continuous Pill Sliders inside Control Center
        if self.cc_panel_rect.contains(x, y) && self.vol_rect.contains(x, y) {
            self.gesture = ActiveGesture::VolSlider {
                slot,
                rect: self.vol_rect,
            };
            return apply_vol_drag(x, self.vol_rect, sys);
        }
        if self.cc_panel_rect.contains(x, y) && self.bri_rect.contains(x, y) {
            self.gesture = ActiveGesture::BriSlider {
                slot,
                rect: self.bri_rect,
            };
            return apply_bri_drag(x, self.bri_rect, sys);
        }

        // 2. Center App Menu Column
        if self.app_panel_rect.contains(x, y) {
            self.app_menu.on_touch_down(slot, x, y);
            self.gesture = ActiveGesture::AppMenu { slot };
            return CurtainEffect::None;
        }

        // 3. Control Center Column (check scrollable Wi-Fi / BT drawers first!)
        if self.cc_panel_rect.contains(x, y) {
            if self.wifi_open && self.wifi_drawer_rect.contains(x, y) && self.wifi_max_scroll > 0.5
            {
                self.gesture = ActiveGesture::DrawerScroll {
                    slot,
                    is_wifi: true,
                    x0: x,
                    y0: y,
                    scroll0: self.wifi_scroll,
                    scrolling: false,
                };
                return CurtainEffect::None;
            }
            if self.bt_open && self.bt_drawer_rect.contains(x, y) && self.bt_max_scroll > 0.5 {
                self.gesture = ActiveGesture::DrawerScroll {
                    slot,
                    is_wifi: false,
                    x0: x,
                    y0: y,
                    scroll0: self.bt_scroll,
                    scrolling: false,
                };
                return CurtainEffect::None;
            }
            self.gesture = ActiveGesture::CcTapOrScroll {
                slot,
                x0: x,
                y0: y,
                t0: Instant::now(),
                scroll0: self.cc_scroll,
                scrolling: false,
            };
            return CurtainEffect::None;
        }

        // 4. Notification Column
        if self.notif_panel_rect.contains(x, y) {
            self.gesture = ActiveGesture::NotifScroll {
                slot,
                x0: x,
                y0: y,
                scroll0: self.notif_scroll,
                scrolling: false,
            };
            return CurtainEffect::None;
        }

        // 5. Scrim or Portrait Tab / Swipe-up dismiss
        self.gesture = ActiveGesture::ScrimOrSwipeUp {
            slot,
            x0: x,
            y0: y,
            last_y: y,
            last_t: Instant::now(),
            vy: 0.0,
            tap_ok: true,
        };
        CurtainEffect::None
    }

    pub fn on_touch_motion(
        &mut self,
        slot: i32,
        x: f32,
        y: f32,
        sys: &mut SysSnapshot,
    ) -> CurtainEffect {
        let mut promote_swipe_up: Option<f32> = None;
        match &mut self.gesture {
            ActiveGesture::VolSlider { slot: s, rect } if *s == slot => {
                return apply_vol_drag(x, *rect, sys);
            }
            ActiveGesture::BriSlider { slot: s, rect } if *s == slot => {
                return apply_bri_drag(x, *rect, sys);
            }
            ActiveGesture::AppMenu { slot: s } if *s == slot => {
                self.app_menu.on_touch_motion(slot, x, y);
                if !self.app_menu.is_actively_scrolling() && !self.app_menu.can_scroll_down_further() {
                    if let Some((x0, y0)) = self.app_menu.press_origin() {
                        let dy = y - y0;
                        if dy < -14.0 && dy.abs() > (x - x0).abs() * 1.15 {
                            promote_swipe_up = Some(x0);
                        }
                    }
                }
            }
            ActiveGesture::DrawerScroll {
                slot: s,
                is_wifi,
                x0,
                y0,
                scroll0,
                scrolling,
            } if *s == slot => {
                let dy = y - *y0;
                let (cur_sc, max_sc) = if *is_wifi {
                    (self.wifi_scroll, self.wifi_max_scroll)
                } else {
                    (self.bt_scroll, self.bt_max_scroll)
                };
                let can_scroll_dir =
                    (dy < 0.0 && cur_sc < max_sc - 0.5) || (dy > 0.0 && cur_sc > 0.5);
                if !*scrolling && dy.abs() > 10.0 && can_scroll_dir {
                    *scrolling = true;
                }
                if *scrolling {
                    let next = (*scroll0 - dy).clamp(0.0, max_sc);
                    if *is_wifi {
                        self.wifi_scroll = next;
                    } else {
                        self.bt_scroll = next;
                    }
                } else if dy < -14.0
                    && dy.abs() > (x - *x0).abs() * 1.15
                    && cur_sc >= max_sc - 0.5
                {
                    promote_swipe_up = Some(*x0);
                }
            }
            ActiveGesture::CcTapOrScroll {
                slot: s,
                x0,
                y0,
                scroll0,
                scrolling,
                ..
            } if *s == slot => {
                let dy = y - *y0;
                let can_scroll_dir = (dy < 0.0 && self.cc_scroll < self.cc_max_scroll - 0.5)
                    || (dy > 0.0 && self.cc_scroll > 0.5);
                if !*scrolling && dy.abs() > 12.0 && can_scroll_dir {
                    *scrolling = true;
                }
                if *scrolling {
                    self.cc_scroll = (*scroll0 - dy).clamp(0.0, self.cc_max_scroll);
                } else if dy < -14.0
                    && dy.abs() > (x - *x0).abs() * 1.15
                    && self.cc_scroll >= self.cc_max_scroll - 0.5
                {
                    promote_swipe_up = Some(*x0);
                }
            }
            ActiveGesture::NotifScroll {
                slot: s,
                x0,
                y0,
                scroll0,
                scrolling,
            } if *s == slot => {
                let dy = y - *y0;
                let can_scroll_dir = (dy < 0.0 && self.notif_scroll < self.notif_max_scroll - 0.5)
                    || (dy > 0.0 && self.notif_scroll > 0.5);
                if !*scrolling && dy.abs() > 10.0 && can_scroll_dir {
                    *scrolling = true;
                }
                if *scrolling {
                    self.notif_scroll = (*scroll0 - dy).clamp(0.0, self.notif_max_scroll);
                } else if dy < -14.0
                    && dy.abs() > (x - *x0).abs() * 1.15
                    && self.notif_scroll >= self.notif_max_scroll - 0.5
                {
                    promote_swipe_up = Some(*x0);
                }
            }
            ActiveGesture::ScrimOrSwipeUp {
                slot: s,
                y0,
                last_y,
                last_t,
                vy,
                ..
            } if *s == slot => {
                crate::sys_state::request_gpu_boost(650);
                let now = Instant::now();
                let dt = now.duration_since(*last_t).as_secs_f32().max(0.001);
                let step_dy = y - *last_y;
                *vy = *vy * 0.45 + (step_dy / dt) * 0.55;
                *last_y = y;
                *last_t = now;

                let dy = y - *y0;
                if dy < -4.0 {
                    let sh = self.last_screen_h.max(400.0);
                    let p = (1.0 + dy / sh).clamp(0.015, 1.0);
                    self.progress.snap(p);
                }
            }
            _ => {}
        }

        if let Some(orig_x0) = promote_swipe_up {
            self.app_menu.cancel_press();
            crate::sys_state::request_gpu_boost(650);
            let drag_y0 = y + 4.5;
            let sh = self.last_screen_h.max(400.0);
            let p = (1.0 - 4.5 / sh).clamp(0.015, 1.0);
            self.progress.snap(p);
            self.gesture = ActiveGesture::ScrimOrSwipeUp {
                slot,
                x0: orig_x0,
                y0: drag_y0,
                last_y: y,
                last_t: Instant::now(),
                vy: -320.0,
                tap_ok: false,
            };
        }
        CurtainEffect::None
    }

    pub fn on_scroll(&mut self, x: f32, y: f32, dy: f32) -> bool {
        if self.touchpad_edge_pull_active || self.swipe_3f_active || dy.abs() < 0.01 {
            return false;
        }

        // If a 2-finger scroll pull/dismiss is already underway (e.g. handed off from TopBar),
        // continue tracking curtain progress 1:1 until `AxisStop` (`on_scroll_stop`)!
        if self.scroll_pull_active {
            crate::sys_state::request_gpu_boost(650);
            let sh = self.last_screen_h.max(400.0);
            let now = Instant::now();
            let dt = self
                .scroll_pull_last_at
                .map(|t| now.duration_since(t).as_secs_f32().clamp(0.001, 0.08))
                .unwrap_or(0.008);
            self.scroll_pull_last_at = Some(now);
            let step_px = -dy * 1.65;
            self.scroll_pull_vy = self.scroll_pull_vy * 0.45 + (step_px / dt) * 0.55;
            let next_p = (self.progress.pos + step_px / sh).clamp(0.015, 1.0);
            self.progress.snap(next_p);
            return true;
        }

        if self.app_panel_rect.contains(x, y) {
            return self.app_menu.on_scroll(dy);
        }
        if self.notif_panel_rect.contains(x, y) && self.notif_max_scroll > 0.5 {
            let can_scroll = (dy > 0.0 && self.notif_scroll < self.notif_max_scroll - 0.5)
                || (dy < 0.0 && self.notif_scroll > 0.5);
            if can_scroll {
                self.notif_scroll =
                    (self.notif_scroll + dy * 1.35).clamp(0.0, self.notif_max_scroll);
                return true;
            }
        }
        if self.wifi_open && self.wifi_drawer_rect.contains(x, y) && self.wifi_max_scroll > 0.5 {
            let can_scroll = (dy > 0.0 && self.wifi_scroll < self.wifi_max_scroll - 0.5)
                || (dy < 0.0 && self.wifi_scroll > 0.5);
            if can_scroll {
                self.wifi_scroll =
                    (self.wifi_scroll + dy * 1.35).clamp(0.0, self.wifi_max_scroll);
                return true;
            }
        }
        if self.bt_open && self.bt_drawer_rect.contains(x, y) && self.bt_max_scroll > 0.5 {
            let can_scroll = (dy > 0.0 && self.bt_scroll < self.bt_max_scroll - 0.5)
                || (dy < 0.0 && self.bt_scroll > 0.5);
            if can_scroll {
                self.bt_scroll = (self.bt_scroll + dy * 1.35).clamp(0.0, self.bt_max_scroll);
                return true;
            }
        }
        if self.cc_panel_rect.contains(x, y) && self.cc_max_scroll > 0.5 {
            let can_scroll = (dy > 0.0 && self.cc_scroll < self.cc_max_scroll - 0.5)
                || (dy < 0.0 && self.cc_scroll > 0.5);
            if can_scroll {
                self.cc_scroll = (self.cc_scroll + dy * 1.35).clamp(0.0, self.cc_max_scroll);
                return true;
            }
        }

        // 2-finger upward scroll (`dy > 0` with `natural-scroll`) over scrim / Control Center /
        // non-scrolling Notifications pushes the Console curtain upward 1:1 to dismiss!
        if dy > 0.4 && self.open && !self.app_panel_rect.contains(x, y) {
            crate::sys_state::request_gpu_boost(650);
            let sh = self.last_screen_h.max(400.0);
            self.scroll_pull_active = true;
            self.scroll_pull_last_at = Some(Instant::now());
            let step_px = -dy * 1.65;
            self.scroll_pull_vy = step_px / 0.012;
            let next_p = (self.progress.pos + step_px / sh).clamp(0.015, 1.0);
            self.progress.snap(next_p);
            return true;
        }

        false
    }

    #[allow(clippy::too_many_arguments)]
    pub fn on_touch_up(
        &mut self,
        slot: i32,
        x: f32,
        y: f32,
        sys: &mut SysSnapshot,
        niri: &mut NiriSnapshot,
        prefs: &mut ShellPrefs,
        auto_rotate_flag: &AtomicBool,
        notifs: &mut Vec<NotifItem>,
    ) -> CurtainEffect {
        let g = std::mem::replace(&mut self.gesture, ActiveGesture::None);
        match g {
            ActiveGesture::VolSlider { slot: s, .. } | ActiveGesture::BriSlider { slot: s, .. }
                if s == slot =>
            {
                return CurtainEffect::None;
            }
            ActiveGesture::AppMenu { slot: s } if s == slot => {
                match self.app_menu.on_touch_up(slot, x, y, prefs, niri) {
                    AppMenuEvent::FocusSearchBar => return CurtainEffect::OpenOskForSearch,
                    AppMenuEvent::AppLaunched => {
                        self.close();
                        return CurtainEffect::None;
                    }
                    _ => return CurtainEffect::None,
                }
            }
            ActiveGesture::DrawerScroll {
                slot: s,
                x0,
                y0,
                scrolling,
                ..
            } if s == slot => {
                if scrolling || (x - x0).hypot(y - y0) > 18.0 {
                    return CurtainEffect::None;
                }
                return self.dispatch_hit(x, y, sys, niri, prefs, auto_rotate_flag, notifs);
            }
            ActiveGesture::CcTapOrScroll {
                slot: s,
                x0,
                y0,
                t0,
                scrolling,
                ..
            } if s == slot => {
                if scrolling || (x - x0).hypot(y - y0) > 18.0 {
                    return CurtainEffect::None;
                }
                if t0.elapsed() >= std::time::Duration::from_millis(420) {
                    if let Some(page) = self.resolve_long_press_settings_page(x, y) {
                        HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);
                        self.close();
                        crate::sys_state::open_system_settings_page(Some(page));
                        return CurtainEffect::HideInlineOsk;
                    }
                }
                return self.dispatch_hit(x, y, sys, niri, prefs, auto_rotate_flag, notifs);
            }
            ActiveGesture::NotifScroll {
                slot: s,
                x0,
                y0,
                scrolling,
                ..
            } if s == slot => {
                if !scrolling && (x - x0).abs() > 72.0 && (y - y0).abs() < 38.0 {
                    for (r, hit) in self.hits.iter().rev() {
                        if let CcHit::DismissNotif(id) = *hit {
                            if r.contains(x0, y0) {
                                HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);
                                notifs.retain(|n| n.id != id);
                                return CurtainEffect::None;
                            }
                        }
                    }
                    return CurtainEffect::None;
                }
                if scrolling || (x - x0).hypot(y - y0) > 18.0 {
                    return CurtainEffect::None;
                }
                for (r, hit) in self.hits.iter().rev() {
                    if matches!(hit, CcHit::ClearAllNotifs) && r.contains(x, y) {
                        HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);
                        notifs.clear();
                        return CurtainEffect::None;
                    }
                }
                return CurtainEffect::None;
            }
            ActiveGesture::ScrimOrSwipeUp {
                slot: s,
                x0,
                y0,
                vy,
                tap_ok,
                ..
            } if s == slot => {
                crate::sys_state::request_gpu_boost(650);
                // Check portrait tab hits first
                if tap_ok && (x - x0).hypot(y - y0) <= 18.0 {
                    for (r, hit) in self.hits.iter().rev() {
                        if let CcHit::PortraitTab(t) = *hit {
                            if r.contains(x, y) {
                                self.portrait_tab = t;
                                HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);
                                return CurtainEffect::None;
                            }
                        }
                    }
                    // Tap on blank scrim dismisses Console!
                    self.close();
                } else {
                    let sh = self.last_screen_h.max(400.0);
                    if vy < -260.0 || (self.progress.pos < 0.72 && vy < 200.0) {
                        self.open = false;
                        self.power_open = false;
                        self.power_spring.set_target(0.0);
                        self.wifi_pw_ssid = None;
                        self.wifi_pw_buf.clear();
                        set_bt_discovery(false);
                        self.app_menu.search_focused = false;
                        self.progress.fling(0.0, (vy / sh).clamp(-4.5, 1.5));
                    } else {
                        self.open = true;
                        self.progress.fling(1.0, (vy / sh).clamp(-2.5, 4.5));
                    }
                }
            }
            _ => {}
        }
        CurtainEffect::None
    }

    fn resolve_long_press_settings_page(&self, x: f32, y: f32) -> Option<&'static str> {
        for (r, hit) in self.hits.iter().rev() {
            if r.contains(x, y) {
                return match hit {
                    CcHit::WifiToggle | CcHit::WifiExpand => Some("wifi"),
                    CcHit::BtToggle | CcHit::BtExpand => Some("bt"),
                    CcHit::QuickToggle(
                        QuickToggleId::AutoRotate
                        | QuickToggleId::AutoBrightness
                        | QuickToggleId::HighRefresh,
                    ) => Some("display"),
                    CcHit::QuickToggle(
                        QuickToggleId::BypassPower | QuickToggleId::StandbyDisplay,
                    ) => Some("power"),
                    CcHit::QuickToggle(QuickToggleId::VirtualKeyboard) => Some("pen"),
                    CcHit::QuickToggle(QuickToggleId::Dnd) => Some("notif"),
                    CcHit::QuickToggle(QuickToggleId::Torch) => Some("sys"),
                    _ => None,
                };
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_hit(
        &mut self,
        x: f32,
        y: f32,
        sys: &mut SysSnapshot,
        _niri: &mut NiriSnapshot,
        prefs: &mut ShellPrefs,
        auto_rotate_flag: &AtomicBool,
        notifs: &mut Vec<NotifItem>,
    ) -> CurtainEffect {
        let mut matched: Option<CcHit> = None;
        for (r, hit) in self.hits.iter().rev() {
            if r.contains(x, y) {
                matched = Some(hit.clone());
                break;
            }
        }
        let Some(hit) = matched else {
            return CurtainEffect::None;
        };
        HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);

        match hit {
            CcHit::PortraitTab(t) => {
                self.portrait_tab = t;
            }
            CcHit::HeaderPowerToggle => {
                crate::sys_state::request_gpu_boost(450);
                self.power_open = !self.power_open;
                self.android_confirm_until = None;
                self.power_spring
                    .set_target(if self.power_open { 1.0 } else { 0.0 });
            }
            CcHit::PowerReboot => {
                self.close();
                system_reboot();
            }
            CcHit::PowerRebootAndroid => {
                if self
                    .android_confirm_until
                    .is_some_and(|u| Instant::now() < u)
                {
                    self.close();
                    system_reboot_android();
                } else {
                    self.android_confirm_until =
                        Some(Instant::now() + std::time::Duration::from_millis(3800));
                }
            }
            CcHit::PowerShutdown => {
                self.close();
                system_poweroff();
            }
            CcHit::WifiToggle => {
                sys.wifi_on = !sys.wifi_on;
                set_wifi_enabled(sys.wifi_on);
                if sys.wifi_on && self.wifi_open {
                    self.wifi_scan_until =
                        Some(Instant::now() + std::time::Duration::from_millis(4200));
                    trigger_wifi_scan();
                } else if !sys.wifi_on {
                    self.wifi_pw_ssid = None;
                    self.wifi_pw_buf.clear();
                    return CurtainEffect::HideInlineOsk;
                }
            }
            CcHit::WifiExpand => {
                crate::sys_state::request_gpu_boost(500);
                self.wifi_open = !self.wifi_open;
                self.wifi_spring
                    .set_target(if self.wifi_open { 1.0 } else { 0.0 });
                if self.wifi_open {
                    self.wifi_scroll = 0.0;
                    if self.bt_open {
                        self.bt_open = false;
                        self.bt_spring.set_target(0.0);
                        set_bt_discovery(false);
                    }
                    if sys.wifi_on {
                        self.wifi_scan_until =
                            Some(Instant::now() + std::time::Duration::from_millis(4200));
                        trigger_wifi_scan();
                    }
                } else {
                    self.wifi_pw_ssid = None;
                    self.wifi_pw_buf.clear();
                    return CurtainEffect::HideInlineOsk;
                }
            }
            CcHit::WifiRescan => {
                if sys.wifi_on {
                    self.wifi_scan_until =
                        Some(Instant::now() + std::time::Duration::from_millis(4200));
                    trigger_wifi_scan();
                }
            }
            CcHit::WifiSelect {
                ssid,
                secured,
                saved,
                active,
            } => {
                if active {
                    return CurtainEffect::None;
                }
                if saved || !secured {
                    self.wifi_pw_ssid = None;
                    self.wifi_pw_buf.clear();
                    self.wifi_connecting = Some((ssid.clone(), Instant::now()));
                    connect_wifi_ssid(ssid);
                    return CurtainEffect::HideInlineOsk;
                }
                if self.wifi_pw_ssid.as_deref() == Some(ssid.as_str()) {
                    self.wifi_pw_ssid = None;
                    self.wifi_pw_buf.clear();
                    return CurtainEffect::HideInlineOsk;
                }
                if let Some(idx) = sys.wifi_list.iter().position(|n| n.ssid == ssid) {
                    let pw_bottom = ((idx + 1) as f32) * 38.0 + 44.0;
                    if pw_bottom > self.wifi_scroll + 212.0 {
                        self.wifi_scroll = (pw_bottom - 212.0).max(0.0);
                    }
                }
                self.wifi_pw_ssid = Some(ssid);
                self.wifi_pw_buf.clear();
                return CurtainEffect::OpenOskForWifiPassword;
            }
            CcHit::WifiPasswordInput => {
                return CurtainEffect::OpenOskForWifiPassword;
            }
            CcHit::WifiSubmitPassword => {
                if self.submit_wifi_password() {
                    return CurtainEffect::HideInlineOsk;
                }
            }
            CcHit::BtToggle => {
                sys.bt_on = !sys.bt_on;
                set_bt_enabled(sys.bt_on);
                if sys.bt_on && self.bt_open {
                    set_bt_discovery(true);
                } else if !sys.bt_on {
                    set_bt_discovery(false);
                }
            }
            CcHit::BtExpand => {
                crate::sys_state::request_gpu_boost(500);
                self.bt_open = !self.bt_open;
                self.bt_spring
                    .set_target(if self.bt_open { 1.0 } else { 0.0 });
                if self.bt_open {
                    self.bt_scroll = 0.0;
                    if self.wifi_open {
                        self.wifi_open = false;
                        self.wifi_spring.set_target(0.0);
                        self.wifi_pw_ssid = None;
                        self.wifi_pw_buf.clear();
                    }
                    if sys.bt_on {
                        set_bt_discovery(true);
                    }
                    return CurtainEffect::HideInlineOsk;
                } else {
                    set_bt_discovery(false);
                }
            }
            CcHit::BtRescan => {
                if sys.bt_on {
                    set_bt_discovery(true);
                }
            }
            CcHit::BtDevice {
                mac,
                connected,
                paired,
            } => {
                self.bt_connecting = Some((mac.clone(), !connected, Instant::now()));
                toggle_bt_device_mac(mac, connected, paired);
            }
            CcHit::MediaPrev => media_cmd("previous"),
            CcHit::MediaPlayPause => {
                sys.media_playing = !sys.media_playing;
                media_cmd("play-pause");
            }
            CcHit::MediaNext => media_cmd("next"),
            CcHit::ClearAllNotifs => {
                notifs.clear();
            }
            CcHit::DismissNotif(id) => {
                notifs.retain(|n| n.id != id);
            }
            CcHit::QuickToggle(qid) => {
                return self.activate_quick_toggle(qid, sys, prefs, auto_rotate_flag);
            }
        }
        CurtainEffect::None
    }

    fn activate_quick_toggle(
        &mut self,
        qid: QuickToggleId,
        sys: &mut SysSnapshot,
        prefs: &mut ShellPrefs,
        auto_rotate_flag: &AtomicBool,
    ) -> CurtainEffect {
        match qid {
            QuickToggleId::AutoRotate => {
                prefs.auto_rotate = !prefs.auto_rotate;
                auto_rotate_flag.store(prefs.auto_rotate, Ordering::Relaxed);
                prefs.save();
            }
            QuickToggleId::AutoBrightness => {
                sys.autobright = !sys.autobright;
                set_autobright(sys.autobright);
            }
            QuickToggleId::HighRefresh => {
                prefs.high_refresh_144 = !prefs.high_refresh_144;
                prefs.save();
                sync_display_ctm_and_hz(prefs, sys.pen_hover);
            }
            QuickToggleId::BypassPower => {
                let next = if sys.charge_limit < 100 {
                    100
                } else {
                    crate::sys_state::BYPASS_CHARGE_LIMIT_PCT
                };
                sys.charge_limit = next;
                if sys.usb_online && next < 100 && sys.bat_pct >= next {
                    sys.bat_charging = false;
                }
                set_charge_limit(next);
            }
            QuickToggleId::Dnd => {
                prefs.dnd = !prefs.dnd;
                prefs.save();
            }
            QuickToggleId::Torch => {
                sys.torch = !sys.torch;
                toggle_torch();
            }
            QuickToggleId::StandbyDisplay => {
                prefs.standby_companion = !prefs.standby_companion;
                prefs.save();
            }
            QuickToggleId::VirtualKeyboard => {
                self.close();
                return CurtainEffect::ToggleOsk;
            }
        }
        CurtainEffect::None
    }
}

fn build_bt_drawer_items(sys: &SysSnapshot) -> Vec<crate::sys_state::BtPeripheral> {
    let mut list: Vec<crate::sys_state::BtPeripheral> = Vec::with_capacity(sys.bt_peripherals.len() + 1);
    for d in &sys.bt_peripherals {
        let mut item = d.clone();
        if item.official_pen && sys.pen_docked && sys.pen_pct >= 0 {
            item.battery_pct = Some(sys.pen_pct.clamp(0, 100) as u8);
            item.charging = sys.pen_charging;
        }
        list.push(item);
    }
    list.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.paired.cmp(&a.paired))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.name.cmp(&b.name))
    });
    if sys.pen_docked && !list.iter().any(|d| d.official_pen) {
        list.push(crate::sys_state::BtPeripheral {
            name: "Lenovo Tab Pen Pro".into(),
            mac: "QI:PEN".into(),
            kind: crate::sys_state::BtDevKind::Stylus,
            battery_pct: (sys.pen_pct >= 0).then_some(sys.pen_pct.clamp(0, 100) as u8),
            official_pen: true,
            paired: true,
            connected: true,
            charging: sys.pen_charging,
        });
    }
    list
}

fn build_quick_toggles(
    sys: &SysSnapshot,
    prefs: &ShellPrefs,
) -> [(QuickToggleId, &'static str, &'static str, bool); 8] {
    [
        (QuickToggleId::AutoRotate, "rotate", "自动旋转", prefs.auto_rotate),
        (
            QuickToggleId::AutoBrightness,
            "auto_brightness",
            "自动亮度",
            sys.autobright,
        ),
        (QuickToggleId::HighRefresh, "refresh_hz", "高刷新率", prefs.high_refresh_144),
        (QuickToggleId::BypassPower, "bypass_power", "旁路供电", sys.charge_limit < 100),
        (QuickToggleId::Dnd, "moon_dnd", "勿扰模式", prefs.dnd),
        (QuickToggleId::Torch, "torch", "手电筒", sys.torch),
        (QuickToggleId::StandbyDisplay, "photo_frame", "待机显示", prefs.standby_companion),
        (QuickToggleId::VirtualKeyboard, "keyboard", "虚拟键盘", false),
    ]
}

pub(crate) fn notif_app_style(app_name: &str) -> (Rgba, Rgba, &'static str) {
    let low = app_name.to_ascii_lowercase();
    if low.contains("chat")
        || low.contains("telegram")
        || low.contains("wechat")
        || app_name.contains("聊天")
        || app_name.contains("消息")
    {
        (Rgba::hex(0x22c55e), Rgba::hex(0x15803d), "chat")
    } else if low.contains("cal") || app_name.contains("日历") || app_name.contains("提醒") {
        (Rgba::hex(0xf43f5e), Rgba::hex(0xbe123c), "cal")
    } else if low.contains("mail") || app_name.contains("邮件") {
        (Rgba::hex(0x38bdf8), Rgba::hex(0x0284c7), "mail")
    } else if app_name.contains("笔") || low.contains("pen") || app_name.contains("守护") {
        (Rgba::hex(0x3b82f6), Rgba::hex(0x1d4ed8), "stylus")
    } else if app_name.contains("蓝牙") || low.contains("blue") {
        (Rgba::hex(0x6366f1), Rgba::hex(0x4338ca), "bluetooth")
    } else {
        (Rgba::hex(0x14b8a6), Rgba::hex(0x0f766e), "bell")
    }
}

fn fit_text_ellipsis(
    text: &mut TextEngine,
    s: &str,
    px: f32,
    weight: FontWeight,
    scale: f32,
    max_w: f32,
) -> String {
    let (w, _) = text.measure(s, px, weight, scale);
    if w <= max_w {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let cand: String = chars[..mid].iter().collect::<String>() + "…";
        let (cw, _) = text.measure(&cand, px, weight, scale);
        if cw <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        "…".to_string()
    } else {
        chars[..lo].iter().collect::<String>() + "…"
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_expandable_hero_card(
    canvas: &mut Canvas,
    icons: &IconSet,
    text: &mut TextEngine,
    pal: &Palette,
    rect: RectF,
    icon_name: &str,
    title: &str,
    sub: &str,
    radio_on: bool,
    drawer_open: bool,
    alpha: f32,
) {
    let rad = 20.0;
    canvas.fill_round_rect(
        rect,
        rad,
        if radio_on {
            pal.acc.with_alpha(alpha)
        } else {
            pal.chip.with_alpha(alpha)
        },
    );
    if !radio_on {
        canvas.stroke_round_rect(rect, rad, 1.0, pal.sep.with_alpha(alpha));
    }

    let orb = RectF::new(rect.x + 11.0, rect.y + (rect.h - 36.0) * 0.5, 36.0, 36.0);
    canvas.fill_round_rect(
        orb,
        18.0,
        if radio_on {
            Rgba::WHITE.with_alpha(0.18 * alpha)
        } else if pal.light {
            Rgba::rgba_f(0, 0, 0, 0.06 * alpha)
        } else {
            Rgba::WHITE.with_alpha(0.08 * alpha)
        },
    );
    icons.draw(
        canvas,
        icon_name,
        orb.inset(9.5, 9.5),
        if radio_on {
            Rgba::WHITE.with_alpha(alpha)
        } else {
            pal.ink.with_alpha(0.88 * alpha)
        },
        1.95,
    );

    let exp_r = RectF::new(rect.x + rect.w - 30.0, rect.y + (rect.h - 22.0) * 0.5, 22.0, 22.0);
    let tx = orb.x + 43.0;
    let tw = (exp_r.x - 4.0 - tx).max(28.0);
    text.draw_in_rect(
        canvas,
        title,
        RectF::new(tx, rect.y + 13.0, tw, 21.0),
        13.8,
        FontWeight::SemiBold,
        if radio_on {
            Rgba::WHITE.with_alpha(alpha)
        } else {
            pal.ink.with_alpha(alpha)
        },
        Align::Left,
    );
    let fitted_sub = fit_text_ellipsis(text, sub, 11.8, FontWeight::Normal, canvas.scale, tw);
    text.draw_in_rect(
        canvas,
        &fitted_sub,
        RectF::new(tx, rect.y + 34.5, tw + 2.0, 19.0),
        11.8,
        FontWeight::Normal,
        if radio_on {
            Rgba::WHITE.with_alpha(0.84 * alpha)
        } else {
            pal.ink3.with_alpha(alpha)
        },
        Align::Left,
    );

    if drawer_open {
        canvas.fill_round_rect(
            exp_r,
            11.0,
            if radio_on {
                Rgba::WHITE.with_alpha(0.20 * alpha)
            } else {
                pal.card.with_alpha(alpha)
            },
        );
    }
    icons.draw(
        canvas,
        if drawer_open { "chevron_up" } else { "collapse" },
        exp_r.inset(5.8, 5.8),
        if radio_on {
            Rgba::WHITE.with_alpha(0.86 * alpha)
        } else {
            pal.ink3.with_alpha(alpha)
        },
        1.9,
    );
}

fn apply_vol_drag(x: f32, rect: RectF, sys: &mut SysSnapshot) -> CurtainEffect {
    crate::sys_state::request_gpu_boost(450);
    let frac = ((x - rect.x) / rect.w.max(1.0)).clamp(0.0, 1.0);
    let pct = (frac * 100.0).round() as u8;
    if pct != sys.volume_pct {
        if (pct as i16 - sys.volume_pct as i16).abs() >= 6 {
            HapticEngine::global().pulse_norm(HapticKind::Tick, 0.75);
        }
        sys.volume_pct = pct;
        sys.muted = pct == 0;
        set_volume_pct(pct);
    }
    CurtainEffect::None
}

fn apply_bri_drag(x: f32, rect: RectF, sys: &mut SysSnapshot) -> CurtainEffect {
    crate::sys_state::request_gpu_boost(450);
    let frac = ((x - rect.x) / rect.w.max(1.0)).clamp(0.02, 1.0);
    let pct = (frac * 100.0).round() as u8;
    if pct != sys.brightness_pct {
        if (pct as i16 - sys.brightness_pct as i16).abs() >= 6 {
            HapticEngine::global().pulse_norm(HapticKind::Tick, 0.75);
        }
        sys.brightness_pct = pct;
        set_brightness_pct(pct);
    }
    CurtainEffect::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tb520fu_ui::color::Skin;

    #[test]
    fn cc_hitboxes_clipped_above_embedded_osk() {
        let mut curtain = Curtain::new(Vec::new());
        curtain.open_with_wing(PullWing::Left);
        curtain.progress.snap(1.0);
        curtain.bt_open = true;
        curtain.bt_spring.snap(1.0);

        let mut canvas = Canvas::new(2944, 1840, 2.0);
        let icons = IconSet::new();
        let mut text = TextEngine::new();
        let pal = Palette::for_skin(Skin::GlassDark);
        let sys = SysSnapshot::default();
        let niri = NiriSnapshot::default();
        let prefs = ShellPrefs::default();

        let osk_inset = 360.0;
        curtain.render(
            &mut canvas,
            &icons,
            &mut text,
            &pal,
            &sys,
            &niri,
            &prefs,
            &[],
            osk_inset,
        );

        let max_allowed_y = canvas.logical_height() - 18.0 - osk_inset + 0.5;
        for (r, hit) in &curtain.hits {
            assert!(
                r.y + r.h <= max_allowed_y,
                "hitbox {hit:?} extends to y={} past col_bottom_y={max_allowed_y}",
                r.y + r.h
            );
        }
    }

    #[test]
    fn pull_down_tracks_1_to_1_and_never_stalls_at_90_percent() {
        let mut curtain = Curtain::new(Vec::new());
        let niri = NiriSnapshot::default();
        let sh = 920.0;

        // Finger dragged 90% down the screen (`828px / 920px` @ `2.0x`)
        curtain.drag_open(PullWing::Right, 828.0, sh);
        assert!((curtain.progress.pos - 0.90).abs() < 1e-4);

        // Release finger gently (`vy = 0`)
        curtain.finish_pull(PullWing::Right, 0.0, sh);
        assert!(curtain.is_animating(), "curtain must animate remaining 10% after release");

        let mut redraw_frames = 0usize;
        for _ in 0..240 {
            let (rd, _unmap, _lp) = curtain.tick(1.0 / 120.0, &niri);
            if rd {
                redraw_frames += 1;
            }
            if !curtain.is_animating() {
                break;
            }
        }
        assert!(redraw_frames >= 10, "expected smooth settle frames, got {redraw_frames}");
        assert!(
            (curtain.progress.pos - 1.0).abs() < 1e-6,
            "curtain must reach exact 1.0, got {}",
            curtain.progress.pos
        );
    }

    #[test]
    fn swipe_up_from_any_non_scrolling_column_promotes_to_dismiss() {
        let mut curtain = Curtain::new(Vec::new());
        let mut canvas = Canvas::new(2944, 1840, 2.0);
        let icons = IconSet::new();
        let mut text = TextEngine::new();
        let pal = Palette::for_skin(Skin::GlassDark);
        let mut sys = SysSnapshot::default();
        let mut niri = NiriSnapshot::default();
        let mut prefs = ShellPrefs::default();
        let auto_rot = AtomicBool::new(false);
        let mut notifs = Vec::new();

        for start_x in [200.0_f32, 736.0, 1260.0] {
            curtain.open_with_wing(PullWing::Right);
            curtain.progress.snap(1.0);
            curtain.render(
                &mut canvas,
                &icons,
                &mut text,
                &pal,
                &sys,
                &niri,
                &prefs,
                &notifs,
                0.0,
            );
            assert_eq!(
                curtain.cc_max_scroll, 0.0,
                "control center at 1472x920 (2.0x) must fit without clipping/scrolling"
            );

            curtain.on_touch_down(0, start_x, 620.0, &mut sys);
            for step in 1..=20 {
                let y = 620.0 - (step as f32) * 18.0;
                curtain.on_touch_motion(0, start_x, y, &mut sys);
            }
            assert!(
                curtain.progress.pos < 0.70,
                "upward swipe at x={start_x} must track console upward, got pos={}",
                curtain.progress.pos
            );
            curtain.on_touch_up(
                0,
                start_x,
                260.0,
                &mut sys,
                &mut niri,
                &mut prefs,
                &auto_rot,
                &mut notifs,
            );
            assert!(!curtain.open, "upward swipe at x={start_x} must close console");

            // Step swipe-up tail to unmap: every single mapped frame before unmap MUST carry a non-empty blur_rect!
            for _ in 0..240 {
                let (_rd, unmap, _) = curtain.tick(1.0 / 120.0, &niri);
                if unmap {
                    break;
                }
                curtain.render(
                    &mut canvas,
                    &icons,
                    &mut text,
                    &pal,
                    &sys,
                    &niri,
                    &prefs,
                    &notifs,
                    0.0,
                );
                assert!(
                    !canvas.blur_rects.is_empty(),
                    "mapped swipe-up frame at pos={} must never drop blur_rects to empty",
                    curtain.progress.pos
                );
            }
        }
    }

    #[test]
    fn quick_toggles_hide_placeholder_and_unimplemented_tiles() {
        let sys = SysSnapshot::default();
        let prefs = ShellPrefs::default();
        let toggles = build_quick_toggles(&sys, &prefs);
        let labels: Vec<&str> = toggles.iter().map(|t| t.2).collect();
        assert_eq!(labels.len(), 8);
        assert!(labels.contains(&"自动亮度"));
        for hidden in [
            "护眼模式",
            "阅读模式",
            "外接屏幕",
            "实时字幕",
            "角落手势",
            "手写笔球",
            "静音",
            "深色模式",
            "浅色模式",
            "便签",
        ] {
            assert!(!labels.contains(&hidden), "{hidden} must be hidden");
        }
    }

    #[test]
    fn power_menu_and_quick_connect_drawers_behave_cleanly() {
        use crate::sys_state::{BtDevKind, BtPeripheral, WifiNetwork};
        let mut curtain = Curtain::new(Vec::new());
        let mut canvas = Canvas::new(2944, 1840, 2.0);
        let icons = IconSet::new();
        let mut text = TextEngine::new();
        let pal = Palette::for_skin(Skin::GlassDark);
        let mut sys = SysSnapshot {
            wifi_on: true,
            wifi_ssid: "DZT".into(),
            wifi_list: vec![
                WifiNetwork {
                    ssid: "DZT".into(),
                    signal: 90,
                    active: true,
                    secured: true,
                    saved: true,
                },
                WifiNetwork {
                    ssid: "504-Guest".into(),
                    signal: 74,
                    active: false,
                    secured: true,
                    saved: false,
                },
            ],
            bt_on: true,
            pen_docked: true,
            pen_charging: true,
            pen_pct: 100,
            bt_peripherals: vec![BtPeripheral {
                name: "Lenovo Tab Pen Pro".into(),
                mac: "02:00:00:00:4B:9F".into(),
                kind: BtDevKind::Stylus,
                battery_pct: Some(100),
                official_pen: true,
                connected: false,
                paired: true,
                charging: false,
            }],
            ..SysSnapshot::default()
        };
        let mut niri = NiriSnapshot::default();
        let mut prefs = ShellPrefs::default();
        let auto_rot = AtomicBool::new(false);
        let mut notifs = Vec::new();

        // 1. Real paired BlueZ stylus must NOT be shadowed by unclickable QI:PEN
        let bt_items = build_bt_drawer_items(&sys);
        assert_eq!(bt_items.len(), 1);
        assert_eq!(bt_items[0].mac, "02:00:00:00:4B:9F");

        // 2. Single Power button expands Reboot + Shutdown
        curtain.open_with_wing(PullWing::Right);
        curtain.progress.snap(1.0);
        curtain.render(
            &mut canvas,
            &icons,
            &mut text,
            &pal,
            &sys,
            &niri,
            &prefs,
            &notifs,
            0.0,
        );
        let pwr_rect = curtain
            .hits
            .iter()
            .find(|(_, h)| matches!(h, CcHit::HeaderPowerToggle))
            .map(|(r, _)| *r)
            .expect("HeaderPowerToggle must be rendered");
        let (px, py) = pwr_rect.center();
        curtain.on_touch_down(0, px, py, &mut sys);
        curtain.on_touch_up(
            0,
            px,
            py,
            &mut sys,
            &mut niri,
            &mut prefs,
            &auto_rot,
            &mut notifs,
        );
        assert!(curtain.power_open);
        curtain.power_spring.snap(1.0);
        curtain.render(
            &mut canvas,
            &icons,
            &mut text,
            &pal,
            &sys,
            &niri,
            &prefs,
            &notifs,
            0.0,
        );
        assert!(curtain.hits.iter().any(|(_, h)| matches!(h, CcHit::PowerReboot)));
        assert!(curtain
            .hits
            .iter()
            .any(|(_, h)| matches!(h, CcHit::PowerRebootAndroid)));
        assert!(curtain.hits.iter().any(|(_, h)| matches!(h, CcHit::PowerShutdown)));

        // 3. Tapping an unsaved secured Wi-Fi network opens inline WPA password input + OSK
        curtain.wifi_open = true;
        curtain.wifi_spring.snap(1.0);
        curtain.render(
            &mut canvas,
            &icons,
            &mut text,
            &pal,
            &sys,
            &niri,
            &prefs,
            &notifs,
            0.0,
        );
        let guest_rect = curtain
            .hits
            .iter()
            .find(|(_, h)| {
                matches!(h, CcHit::WifiSelect { ssid, .. } if ssid == "504-Guest")
            })
            .map(|(r, _)| *r)
            .expect("504-Guest row must be clickable");
        let (gx, gy) = guest_rect.center();
        curtain.on_touch_down(0, gx, gy, &mut sys);
        let eff = curtain.on_touch_up(
            0,
            gx,
            gy,
            &mut sys,
            &mut niri,
            &mut prefs,
            &auto_rot,
            &mut notifs,
        );
        assert_eq!(eff, CurtainEffect::OpenOskForWifiPassword);
        assert_eq!(curtain.wifi_pw_ssid.as_deref(), Some("504-Guest"));
    }
}
