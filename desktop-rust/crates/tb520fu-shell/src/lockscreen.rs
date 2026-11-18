// SPDX-License-Identifier: MIT
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cosmic_text::Align;
use tb520fu_osk::render::OskRenderer;
use tb520fu_osk::state::{OskState, OutgoingAction};
use tb520fu_ui::canvas::{Canvas, RectF};
use tb520fu_ui::color::{Palette, Rgba, Skin};
use tb520fu_ui::haptics::{HapticEngine, HapticKind};
use tb520fu_ui::spring::Spring1D;
use tb520fu_ui::svg_icons::IconSet;
use tb520fu_ui::text::{FontWeight, TextEngine};
use tb520fu_ui::widgets::BitmapIcon;

use crate::curtain::notif_app_style;
use crate::niri_ipc::{power_off_monitors, power_on_monitors, set_output_power};
use crate::notifications::NotifItem;
use crate::pam_greetd::{current_username, greetd_login, verify_pam_password};
use crate::sys_state::{media_cmd, SysSnapshot};

const STANDBY_IDLE_SECS: u64 = 45;
const UNPLUGGED_BLANK_SECS: u64 = 60;
const SLIDE_INTERVAL_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub enum AuthMode {
    SessionLock {
        username: String,
    },
    Greeter {
        username: String,
        session_cmd: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockPhase {
    AuthPrompt,
    StandbyCompanion,
}

#[derive(Debug, Clone)]
enum LockHit {
    OpenAuth,
    DismissAuth,
    Submit,
    ToggleShowPw,
    ToggleTouchOsk,
    EnterStandby,
    MediaCmd(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct LockGesture {
    slot: i32,
    x0: f32,
    y0: f32,
    last_y: f32,
    last_t: Instant,
    vy: f32,
    start_auth: f32,
    in_standby: bool,
    swiping_auth: bool,
}

struct SlideDeck {
    paths: Vec<PathBuf>,
    last_scan: Option<Instant>,
    idx: usize,
    cur_img: Option<BitmapIcon>,
    prev_img: Option<BitmapIcon>,
    fade: Spring1D,
    last_switch: Instant,
}

impl SlideDeck {
    fn new() -> Self {
        let mut fade = Spring1D::normalized(1.0, 14.0, 1.0);
        fade.snap(1.0);
        Self {
            paths: Vec::new(),
            last_scan: None,
            idx: 0,
            cur_img: None,
            prev_img: None,
            fade,
            last_switch: Instant::now(),
        }
    }

    fn scan_if_needed(&mut self, username: &str) {
        if self
            .last_scan
            .is_some_and(|t| t.elapsed() < Duration::from_secs(45))
        {
            return;
        }
        self.last_scan = Some(Instant::now());

        let mut candidate_dirs = Vec::new();
        if let Ok(h) = std::env::var("HOME") {
            let hp = PathBuf::from(&h);
            candidate_dirs.push(hp.join("Pictures/Standby"));
            candidate_dirs.push(hp.join("Pictures"));
        }
        let user_home = PathBuf::from(format!("/home/{username}"));
        candidate_dirs.push(user_home.join("Pictures/Standby"));
        candidate_dirs.push(user_home.join("Pictures"));
        candidate_dirs.push(PathBuf::from("/usr/share/tb520fu/standby"));

        let mut found: Vec<PathBuf> = Vec::new();
        for dir in candidate_dirs {
            let Ok(rd) = fs::read_dir(&dir) else {
                continue;
            };
            let mut batch: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| is_supported_photo(p))
                .collect();
            if !batch.is_empty() {
                batch.sort();
                found = batch;
                break;
            }
        }

        if found != self.paths {
            self.paths = found;
            self.idx = 0;
            self.cur_img = self
                .paths
                .first()
                .and_then(|p| BitmapIcon::from_photo_file(p, 2200));
            self.prev_img = None;
            self.fade.snap(1.0);
            self.last_switch = Instant::now();
        }
    }

    fn advance(&mut self) {
        self.last_switch = Instant::now();
        if self.paths.is_empty() {
            self.idx = (self.idx + 1) % 4;
            self.prev_img = None;
            self.cur_img = None;
            self.fade.snap(0.0);
            self.fade.set_target(1.0);
            return;
        }
        if self.paths.len() == 1 {
            if self.cur_img.is_none() {
                self.cur_img = BitmapIcon::from_photo_file(&self.paths[0], 2200);
            }
            return;
        }
        self.prev_img = self.cur_img.take();
        for _ in 0..self.paths.len() {
            self.idx = (self.idx + 1) % self.paths.len();
            if let Some(img) = BitmapIcon::from_photo_file(&self.paths[self.idx], 2200) {
                self.cur_img = Some(img);
                break;
            }
        }
        self.fade.snap(0.0);
        self.fade.set_target(1.0);
    }
}

fn is_supported_photo(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    let fname = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if fname.starts_with("screenshot_")
        && !p
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("standby"))
    {
        return false;
    }
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "jpg" | "jpeg" | "png"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CalendarGrid {
    year: i32,
    month: u8,
    today: u8,
    first_wday: u8,
    days_in_month: u8,
}

fn compute_month_calendar(year: i32, month: u8, mday: u8, wday_sun0: u8) -> CalendarGrid {
    let m = month.clamp(1, 12);
    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let days_in_month = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 30,
    };
    let d = mday.clamp(1, days_in_month);
    let first_wday = ((wday_sun0 as i32 - ((d as i32 - 1) % 7) + 7) % 7) as u8;
    CalendarGrid {
        year,
        month: m,
        today: d,
        first_wday,
        days_in_month,
    }
}

fn current_local_calendar() -> CalendarGrid {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        compute_month_calendar(
            tm.tm_year + 1900,
            (tm.tm_mon + 1).clamp(1, 12) as u8,
            tm.tm_mday.clamp(1, 31) as u8,
            tm.tm_wday.clamp(0, 6) as u8,
        )
    }
}

pub struct Lockscreen {
    pub locked: bool,
    pub mode: AuthMode,
    pub phase: LockPhase,
    pub pw_buf: String,
    pub show_pw: bool,
    pub touch_osk_open: bool,
    pub kbd_caps: bool,
    pub phys_shift_l: bool,
    pub phys_shift_r: bool,
    pub verifying: bool,
    pub error_msg: Option<(String, Instant)>,

    /// `0.0` = Stage 1 Glance Lockscreen; `1.0` = Stage 2 Blurred Liquid Glass Password Entry.
    pub auth_reveal: Spring1D,
    /// Damped horizontal shake offset (`px`) when password verification fails.
    pub error_shake: Spring1D,
    /// `0.0` = Lockscreen; `1.0` = IPS-Aware Standby Display (`待机显示`).
    pub standby_reveal: Spring1D,
    /// `0` = Dual-Column Flip Clock + Calendar Hub; `1` = Magazine Photo Frame.
    pub standby_page: usize,
    pub page_spring: Spring1D,

    pub osk: OskState,
    pub osk_ren: OskRenderer,
    osk_sub_canvas: Canvas,
    osk_active_slot: Option<i32>,

    gesture: Option<LockGesture>,
    scroll_auth_active: bool,
    scroll_standby_active: bool,
    scroll_standby_dx: f32,
    scroll_vy: f32,
    scroll_last_at: Option<Instant>,
    pressed_hit: Option<usize>,
    hits: Vec<(RectF, LockHit)>,
    auth_rx: Option<mpsc::Receiver<Result<(), String>>>,
    wakeup_fd: Option<i32>,
    last_activity: Instant,
    dpms_blanked_by_idle: bool,
    prev_panel_on: bool,
    last_screen_size: (u32, u32),
    deck: SlideDeck,
}

#[inline]
fn read_panel_dpms_on() -> bool {
    fs::read_to_string("/sys/class/drm/card1-DSI-1/dpms")
        .map(|s| s.trim() == "On")
        .unwrap_or(true)
}

impl Default for Lockscreen {
    fn default() -> Self {
        Self::new_session_lock(None)
    }
}

impl Lockscreen {
    pub fn new_session_lock(wakeup_fd: Option<i32>) -> Self {
        Self::with_mode(
            AuthMode::SessionLock {
                username: current_username(),
            },
            wakeup_fd,
        )
    }

    pub fn new_greeter(username: String, session_cmd: Vec<String>, wakeup_fd: Option<i32>) -> Self {
        let mut s = Self::with_mode(
            AuthMode::Greeter {
                username,
                session_cmd,
            },
            wakeup_fd,
        );
        s.locked = true;
        s
    }

    fn with_mode(mode: AuthMode, wakeup_fd: Option<i32>) -> Self {
        let is_greeter = matches!(mode, AuthMode::Greeter { .. });
        let mut osk = OskState::new(1472, 920);
        osk.configure_password_mode(if is_greeter { "登录" } else { "解锁" });

        let mut auth_reveal = Spring1D::normalized(0.0, 24.0, 0.96);
        auth_reveal.snap(0.0);
        let mut error_shake = Spring1D::with_params(0.0, 44.0, 0.34);
        error_shake.snap(0.0);
        let mut standby_reveal = Spring1D::normalized(0.0, 20.0, 1.0);
        standby_reveal.snap(0.0);
        let mut page_spring = Spring1D::normalized(0.0, 22.0, 0.96);
        page_spring.snap(0.0);

        Self {
            locked: false,
            mode,
            phase: LockPhase::AuthPrompt,
            pw_buf: String::new(),
            show_pw: false,
            touch_osk_open: false,
            kbd_caps: false,
            phys_shift_l: false,
            phys_shift_r: false,
            verifying: false,
            error_msg: None,
            auth_reveal,
            error_shake,
            standby_reveal,
            standby_page: 0,
            page_spring,
            osk,
            osk_ren: OskRenderer::new(),
            osk_sub_canvas: Canvas::new(2944, 720, 2.0),
            osk_active_slot: None,
            gesture: None,
            scroll_auth_active: false,
            scroll_standby_active: false,
            scroll_standby_dx: 0.0,
            scroll_vy: 0.0,
            scroll_last_at: None,
            pressed_hit: None,
            hits: Vec::new(),
            auth_rx: None,
            wakeup_fd,
            last_activity: Instant::now(),
            dpms_blanked_by_idle: false,
            prev_panel_on: read_panel_dpms_on(),
            last_screen_size: (1472, 920),
            deck: SlideDeck::new(),
        }
    }

    pub fn username(&self) -> &str {
        match &self.mode {
            AuthMode::SessionLock { username } | AuthMode::Greeter { username, .. } => username,
        }
    }

    pub fn is_greeter(&self) -> bool {
        matches!(self.mode, AuthMode::Greeter { .. })
    }

    fn wake_monitors_if_blanked(&mut self) {
        if self.dpms_blanked_by_idle || !read_panel_dpms_on() {
            self.dpms_blanked_by_idle = false;
            self.prev_panel_on = true;
            power_on_monitors();
        }
    }

    pub fn on_system_resumed(&mut self) {
        let on = read_panel_dpms_on();
        self.prev_panel_on = on;
        self.dpms_blanked_by_idle = !on;
        self.last_activity = Instant::now();
    }

    pub fn open_auth_stage(&mut self, with_touch_osk: bool) {
        crate::sys_state::request_gpu_boost(550);
        self.wake_monitors_if_blanked();
        self.last_activity = Instant::now();
        self.phase = LockPhase::AuthPrompt;
        self.standby_reveal.set_target(0.0);
        self.auth_reveal.set_target(1.0);
        self.touch_osk_open = with_touch_osk;
        let enter_lbl = if self.is_greeter() { "登录" } else { "解锁" };
        self.osk.configure_password_mode(enter_lbl);
        if with_touch_osk {
            self.osk.show(true);
        } else {
            self.osk.hide(false);
        }
    }

    pub fn close_auth_stage(&mut self) {
        crate::sys_state::request_gpu_boost(450);
        self.last_activity = Instant::now();
        self.auth_reveal.set_target(0.0);
        self.touch_osk_open = false;
        self.osk.hide(false);
        self.error_msg = None;
    }

    pub fn set_standby_page(&mut self, page: usize) {
        let p = page.min(1);
        self.standby_page = p;
        self.page_spring.set_target(p as f32);
    }

    pub fn enter_lock(&mut self, immediate_standby: bool) {
        self.locked = true;
        self.pw_buf.clear();
        self.show_pw = false;
        self.verifying = false;
        self.auth_rx = None;
        self.error_msg = None;
        self.last_activity = Instant::now();
        self.dpms_blanked_by_idle = false;
        self.prev_panel_on = read_panel_dpms_on();
        self.touch_osk_open = false;
        self.osk.hide(false);
        self.osk.slide_y.snap(self.osk.metrics.kb_h + 40.0);
        self.auth_reveal.snap(0.0);
        self.error_shake.snap(0.0);
        self.scroll_auth_active = false;
        self.scroll_standby_active = false;
        self.scroll_standby_dx = 0.0;
        self.scroll_vy = 0.0;
        self.scroll_last_at = None;
        if immediate_standby {
            self.deck.scan_if_needed(&self.username().to_string());
            self.phase = LockPhase::StandbyCompanion;
            self.standby_reveal.snap(1.0);
            self.deck.last_switch = Instant::now();
        } else {
            self.phase = LockPhase::AuthPrompt;
            self.standby_reveal.snap(0.0);
        }
    }

    pub fn wake_to_prompt(&mut self) {
        crate::sys_state::request_gpu_boost(550);
        self.wake_monitors_if_blanked();
        self.last_activity = Instant::now();
        if self.phase == LockPhase::StandbyCompanion {
            self.phase = LockPhase::AuthPrompt;
            self.standby_reveal.set_target(0.0);
        }
    }

    pub fn switch_to_standby(&mut self) {
        crate::sys_state::request_gpu_boost(550);
        let user = self.username().to_string();
        self.deck.scan_if_needed(&user);
        self.phase = LockPhase::StandbyCompanion;
        self.standby_reveal.set_target(1.0);
        self.auth_reveal.set_target(0.0);
        self.touch_osk_open = false;
        self.osk.hide(false);
        self.deck.last_switch = Instant::now();
        self.dpms_blanked_by_idle = false;
        set_output_power("DSI-1", true);
    }

    pub fn is_animating(&self) -> bool {
        self.standby_reveal.is_animating()
            || self.auth_reveal.is_animating()
            || self.error_shake.is_animating()
            || self.page_spring.is_animating()
            || self.deck.fade.is_animating()
            || self.osk.is_animating()
            || self.scroll_auth_active
            || self.scroll_standby_active
    }

    fn drain_osk_outbox(&mut self) -> bool {
        if self.osk.outbox.is_empty() {
            return false;
        }
        let actions: Vec<OutgoingAction> = self.osk.outbox.drain(..).collect();
        let mut changed = false;
        for act in actions {
            match act {
                OutgoingAction::DirectText(s) => {
                    if !self.verifying {
                        self.error_msg = None;
                        for ch in s.chars() {
                            if self.pw_buf.len() < 128 && !ch.is_control() {
                                self.pw_buf.push(ch);
                                changed = true;
                            }
                        }
                    }
                }
                OutgoingAction::DirectBackspace => {
                    if !self.verifying && self.pw_buf.pop().is_some() {
                        self.error_msg = None;
                        changed = true;
                    }
                }
                OutgoingAction::DirectClearAll => {
                    if !self.verifying && !self.pw_buf.is_empty() {
                        self.pw_buf.clear();
                        self.error_msg = None;
                        changed = true;
                    }
                }
                OutgoingAction::DirectEnter => {
                    if !self.pw_buf.is_empty() {
                        self.submit_password();
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if !self.osk.shown && self.touch_osk_open {
            self.touch_osk_open = false;
            changed = true;
        }
        changed
    }

    /// Periodic tick: returns `(needs_redraw, auth_succeeded)`.
    pub fn tick(&mut self, dt: f32, sys: &SysSnapshot, standby_pref: bool) -> (bool, bool) {
        if !self.locked {
            return (false, false);
        }
        let mut redraw = false;

        // 1. Poll async PAM / greetd verification result
        if let Some(rx) = &self.auth_rx {
            if let Ok(res) = rx.try_recv() {
                self.verifying = false;
                self.auth_rx = None;
                match res {
                    Ok(()) => {
                        self.pw_buf.clear();
                        self.error_msg = None;
                        self.osk.hide(false);
                        return (true, true);
                    }
                    Err(_msg) => {
                        self.pw_buf.clear();
                        self.error_msg = Some((
                            "密码不正确，请重试".into(),
                            Instant::now() + Duration::from_millis(2400),
                        ));
                        self.error_shake.pos = 18.0;
                        self.error_shake.vel = 220.0;
                        self.error_shake.set_target(0.0);
                        HapticEngine::global().pulse_norm(HapticKind::Heavy, 0.5);
                        self.last_activity = Instant::now();
                        redraw = true;
                    }
                }
            }
        }

        // 2. Clear expired error banner
        if self
            .error_msg
            .as_ref()
            .is_some_and(|(_, exp)| Instant::now() >= *exp)
        {
            self.error_msg = None;
            redraw = true;
        }

        // 2b. Auto-settle discrete scroll gestures if no `AxisStop` arrives within 160ms
        if (self.scroll_auth_active || self.scroll_standby_active)
            && self
                .scroll_last_at
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(160))
            && self.on_scroll_stop()
        {
            redraw = true;
        }

        // 3. Step smooth springs + embedded OSK
        if self.standby_reveal.step(dt) {
            redraw = true;
        }
        if !self.scroll_auth_active && self.auth_reveal.step(dt) {
            redraw = true;
        }
        if self.error_shake.step(dt) {
            redraw = true;
        }
        if !self.scroll_standby_active && self.page_spring.step(dt) {
            redraw = true;
        }
        if self.deck.fade.step(dt) {
            redraw = true;
        }
        let (osk_rd, _) = self.osk.tick(dt);
        if osk_rd {
            redraw = true;
        }
        if self.drain_osk_outbox() {
            redraw = true;
        }

        // 4. Standby Display (`待机显示`) vs Unplugged DPMS Off rules (NEVER S3 suspend!)
        //    Sync against real DSI-1 DPMS state so waking the screen via physical Power key or
        //    Pogo lid-open clears `dpms_blanked_by_idle` and grants a fresh 60s timeout window.
        let panel_on = read_panel_dpms_on();
        if panel_on && !self.prev_panel_on {
            self.dpms_blanked_by_idle = false;
            self.last_activity = Instant::now();
        } else if !panel_on {
            self.dpms_blanked_by_idle = true;
        }
        self.prev_panel_on = panel_on;

        let plugged = sys.usb_online || sys.bat_charging;
        match self.phase {
            LockPhase::AuthPrompt => {
                if !self.verifying && self.gesture.is_none() {
                    if plugged && standby_pref {
                        if self.last_activity.elapsed() >= Duration::from_secs(STANDBY_IDLE_SECS) {
                            self.switch_to_standby();
                            redraw = true;
                        }
                    } else if !plugged && !self.is_greeter() && panel_on && !self.dpms_blanked_by_idle {
                        if self.last_activity.elapsed() >= Duration::from_secs(UNPLUGGED_BLANK_SECS)
                        {
                            self.dpms_blanked_by_idle = true;
                            self.prev_panel_on = false;
                            power_off_monitors();
                        }
                    }
                }
            }
            LockPhase::StandbyCompanion => {
                if !plugged || !standby_pref {
                    self.wake_to_prompt();
                    redraw = true;
                } else {
                    let user = self.username().to_string();
                    self.deck.scan_if_needed(&user);
                    if self.deck.last_switch.elapsed() >= Duration::from_secs(SLIDE_INTERVAL_SECS) {
                        self.deck.advance();
                        redraw = true;
                    }
                }
            }
        }

        (redraw, false)
    }

    fn submit_password(&mut self) {
        if self.verifying || self.pw_buf.is_empty() {
            return;
        }
        let pw = self.pw_buf.clone();
        let mode = self.mode.clone();
        let wakeup_fd = self.wakeup_fd;
        let (tx, rx) = mpsc::channel();
        self.verifying = true;
        self.error_msg = None;
        self.auth_rx = Some(rx);

        std::thread::Builder::new()
            .name("tb520fu-pam-auth".into())
            .spawn(move || {
                let res = match mode {
                    AuthMode::SessionLock { username } => verify_pam_password(&username, &pw),
                    AuthMode::Greeter {
                        username,
                        session_cmd,
                    } => greetd_login(&username, &pw, &session_cmd),
                };
                let _ = tx.send(res);
                if let Some(fd) = wakeup_fd {
                    let one: u64 = 1;
                    unsafe {
                        let _ = libc::write(fd, &one as *const u64 as *const libc::c_void, 8);
                    }
                }
            })
            .ok();
    }

    pub fn render(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        _pal: &Palette,
        sys: &SysSnapshot,
        notifs: &[NotifItem],
    ) {
        self.hits.clear();
        let sw = canvas.logical_width();
        let sh = canvas.logical_height();
        let sw_u = (sw.round() as u32).max(320);
        let sh_u = (sh.round() as u32).max(240);
        if self.last_screen_size != (sw_u, sh_u) {
            self.last_screen_size = (sw_u, sh_u);
            self.osk.set_screen_size(sw_u, sh_u);
        }

        let sr = self.standby_reveal.pos.clamp(0.0, 1.0);
        if sr < 0.995 {
            self.render_lock_scene(canvas, icons, text, sys, notifs, sw, sh);
        }
        if sr > 0.005 {
            self.render_standby_scene(canvas, icons, text, sys, sw, sh, sr);
        }
    }

    fn render_lock_scene(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        sys: &SysSnapshot,
        notifs: &[NotifItem],
        sw: f32,
        sh: f32,
    ) {
        let lock_pal =
            Palette::for_skin_and_accent(Skin::GlassDark, crate::sys_state::current_accent());
        let ar = self.auth_reveal.pos.clamp(0.0, 1.0);

        // 1. Crisp Full-Bleed Wallpaper + subtle legibility vignette
        canvas.clear(Rgba::rgb(12, 16, 24));
        canvas.draw_wallpaper_background(RectF::new(0.0, 0.0, sw, sh), 0.94);
        canvas.fill_v_gradient(
            RectF::new(0.0, 0.0, sw, 180.0),
            0.0,
            Rgba::rgba_f(8, 12, 20, 0.34),
            Rgba::TRANSPARENT,
        );
        canvas.fill_v_gradient(
            RectF::new(0.0, sh - 220.0, sw, 220.0),
            0.0,
            Rgba::TRANSPARENT,
            Rgba::rgba_f(8, 12, 20, 0.44),
        );

        // 2. Stage 2 Progressive Full-Screen Liquid Glass Blur (`ar > 0.0`)
        if ar > 0.005 {
            canvas.fill_glass_corners_styled(
                RectF::new(0.0, 0.0, sw, sh),
                [0.0; 4],
                &lock_pal,
                Rgba::rgba_f(11, 15, 25, 0.48),
                Rgba::TRANSPARENT,
                0.025,
                ar,
            );
        }

        // 3. Top-Right Clean Status Cluster (Wi-Fi, Stylus, Bypass Power, Battery, Standby button)
        let plugged = sys.usb_online || sys.bat_charging;
        let top_y = 14.0_f32;
        let top_h = 28.0_f32;
        let mut rx = sw - 22.0;

        // Battery cell + %
        let cell_w = 26.0_f32;
        let cell_h = 13.5_f32;
        rx -= cell_w + 3.0;
        let cell_r = RectF::new(rx, top_y + (top_h - cell_h) * 0.5, cell_w, cell_h);
        canvas.stroke_round_rect(cell_r, 4.0, 1.4, Rgba::WHITE.with_alpha(0.72));
        canvas.fill_round_rect(
            RectF::new(cell_r.x + cell_w + 1.2, top_y + (top_h - 5.5) * 0.5, 2.2, 5.5),
            1.1,
            Rgba::WHITE.with_alpha(0.72),
        );
        let fill_frac = (sys.bat_pct as f32 / 100.0).clamp(0.06, 1.0);
        let inner = cell_r.inset(2.4, 2.4);
        let bat_col = if sys.bat_charging {
            Rgba::hex(0x34d399)
        } else if sys.bat_pct <= 20 {
            Rgba::hex(0xf97316)
        } else {
            Rgba::WHITE.with_alpha(0.92)
        };
        canvas.fill_round_rect(
            RectF::new(inner.x, inner.y, inner.w * fill_frac, inner.h),
            2.0,
            bat_col,
        );
        if sys.bat_charging {
            icons.draw(
                canvas,
                "bolt",
                RectF::new(
                    cell_r.x + (cell_w - 11.0) * 0.5,
                    top_y + (top_h - 11.0) * 0.5,
                    11.0,
                    11.0,
                ),
                Rgba::hex(0x0b1218),
                2.0,
            );
        }

        let bat_lbl = format!("{}%", sys.bat_pct);
        rx -= 46.0;
        text.draw_in_rect(
            canvas,
            &bat_lbl,
            RectF::new(rx, top_y, 42.0, top_h),
            13.6,
            FontWeight::SemiBold,
            Rgba::WHITE.with_alpha(0.94),
            Align::Right,
        );
        rx -= 10.0;

        // Wi-Fi icon
        rx -= 18.0;
        icons.draw(
            canvas,
            if sys.wifi_on { "wifi" } else { "wifi_off" },
            RectF::new(rx, top_y + (top_h - 16.5) * 0.5, 16.5, 16.5),
            Rgba::WHITE.with_alpha(if sys.wifi_on { 0.90 } else { 0.45 }),
            1.9,
        );
        rx -= 12.0;

        // Bypass Power pill (`旁路供电`) when active & plugged in
        if plugged && sys.charge_limit < 100 {
            let bp_w = 88.0_f32;
            rx -= bp_w;
            let bp_r = RectF::new(rx, top_y + 1.0, bp_w, 26.0);
            canvas.fill_round_rect(bp_r, 13.0, Rgba::rgba_f(16, 185, 129, 0.22));
            canvas.stroke_round_rect(bp_r, 13.0, 1.0, Rgba::rgba_f(52, 211, 153, 0.48));
            icons.draw(
                canvas,
                "bypass_power",
                RectF::new(bp_r.x + 8.5, bp_r.y + 5.5, 15.0, 15.0),
                Rgba::hex(0x6ee7b7),
                1.9,
            );
            text.draw_in_rect(
                canvas,
                "旁路供电",
                RectF::new(bp_r.x + 26.0, bp_r.y, bp_w - 32.0, bp_r.h),
                11.8,
                FontWeight::SemiBold,
                Rgba::hex(0xa7f3d0),
                Align::Left,
            );
            rx -= 8.0;
        }

        // Official Stylus battery pill if available
        if let Some(pen_pct) = sys
            .sorted_topbar_peripherals()
            .into_iter()
            .find(|d| d.official_pen)
            .and_then(|d| d.battery_pct)
            .or_else(|| (sys.pen_docked && sys.pen_pct >= 0).then_some(sys.pen_pct.min(100) as u8))
        {
            let ps = format!("{pen_pct}%");
            let pw_pill = 36.0 + (ps.len() as f32) * 7.5;
            rx -= pw_pill;
            let pr = RectF::new(rx, top_y + 1.0, pw_pill, 26.0);
            canvas.fill_round_rect(pr, 13.0, Rgba::rgba_f(255, 255, 255, 0.14));
            icons.draw(
                canvas,
                "stylus",
                RectF::new(pr.x + 8.5, pr.y + 5.5, 15.0, 15.0),
                Rgba::WHITE.with_alpha(0.92),
                1.9,
            );
            text.draw_in_rect(
                canvas,
                &ps,
                RectF::new(pr.x + 25.0, pr.y, pr.w - 30.0, pr.h),
                12.0,
                FontWeight::SemiBold,
                Rgba::WHITE.with_alpha(0.92),
                Align::Left,
            );
            rx -= 8.0;
        }

        // `[ 待机 ]` pill button when plugged in (`ar < 0.5`)
        if plugged && ar < 0.5 {
            let sb_w = 72.0_f32;
            rx -= sb_w;
            let sb_r = RectF::new(rx, top_y + 1.0, sb_w, 26.0);
            canvas.fill_round_rect(sb_r, 13.0, Rgba::rgba_f(14, 18, 28, 0.52));
            canvas.stroke_round_rect(sb_r, 13.0, 1.0, Rgba::rgba_f(255, 255, 255, 0.20));
            icons.draw(
                canvas,
                "clock_standby",
                RectF::new(sb_r.x + 9.0, sb_r.y + 5.5, 14.5, 14.5),
                Rgba::WHITE.with_alpha(0.88),
                1.85,
            );
            text.draw_in_rect(
                canvas,
                "待机",
                RectF::new(sb_r.x + 26.0, sb_r.y, sb_w - 32.0, sb_r.h),
                12.0,
                FontWeight::Medium,
                Rgba::WHITE.with_alpha(0.90),
                Align::Left,
            );
            self.hits.push((sb_r.inset(-4.0, -4.0), LockHit::EnterStandby));
        }

        // Compute visible embedded OSK height so Stage 2 password UI centers cleanly above OSK!
        let osk_vis_h = if self.osk.shown || self.osk.slide_y.is_animating() {
            let (_, oy) = crate::inline_osk_origin(&self.osk, sw, sh);
            let actual_top = oy + self.osk.metrics.fpad + self.osk.current_slide_offset();
            (sh - actual_top).clamp(0.0, sh * 0.58)
        } else {
            0.0
        };
        let avail_h = (sh - osk_vis_h).max(280.0);

        // 4. Hero Clock & Date (glides smoothly from upper-center Glance to compact Stage 2 header)
        let glance_clk_y = 88.0_f32;
        let auth_clk_y = if osk_vis_h > 40.0 {
            (avail_h * 0.16).clamp(44.0, 96.0)
        } else {
            132.0_f32
        };
        let clk_y = glance_clk_y + (auth_clk_y - glance_clk_y) * ar;
        let clk_fs = 96.0 + (56.0 - 96.0) * ar;
        let clk_h = clk_fs * 1.12;

        text.draw_in_rect(
            canvas,
            &sys.time_hm,
            RectF::new(0.0, clk_y, sw, clk_h),
            clk_fs,
            FontWeight::Bold,
            Rgba::WHITE.with_alpha(0.97),
            Align::Center,
        );
        let date_fs = 18.5 + (14.5 - 18.5) * ar;
        text.draw_in_rect(
            canvas,
            &sys.date_long,
            RectF::new(0.0, clk_y + clk_h + 2.0, sw, 26.0),
            date_fs,
            FontWeight::Medium,
            Rgba::WHITE.with_alpha(0.82 - 0.10 * ar),
            Align::Center,
        );

        // 5. Stage 1 Glance Stack: Borderless Liquid Glass Media + Notifications + Bottom Swipe Bar
        let glance_alpha = ((1.0 - ar * 1.55)).clamp(0.0, 1.0);
        if glance_alpha > 0.01 {
            let stack_w = 504.0_f32;
            let stack_x = (sw - stack_w) * 0.5;
            let mut cy = clk_y + clk_h + 46.0 - ar * 28.0;

            if !sys.media_title.is_empty() {
                let mc = RectF::new(stack_x, cy, stack_w, 86.0);
                canvas.draw_soft_shadow(mc, 24.0, 10.0, 3.0, 0.24 * glance_alpha);
                canvas.fill_glass_corners_styled(
                    mc,
                    [24.0; 4],
                    &lock_pal,
                    Rgba::rgba_f(18, 24, 38, 0.56),
                    Rgba::TRANSPARENT,
                    0.04,
                    glance_alpha,
                );

                // Album / Music disc badge
                let disc = RectF::new(mc.x + 16.0, mc.y + 17.0, 52.0, 52.0);
                canvas.fill_v_gradient(
                    disc,
                    16.0,
                    Rgba::hex(0x6366f1).with_alpha(glance_alpha),
                    Rgba::hex(0xa855f7).with_alpha(glance_alpha),
                );
                icons.draw(
                    canvas,
                    "music",
                    RectF::new(disc.x + 14.0, disc.y + 14.0, 24.0, 24.0),
                    Rgba::WHITE.with_alpha(glance_alpha),
                    2.0,
                );

                let tx = disc.x + disc.w + 14.0;
                let text_w = mc.w - (tx - mc.x) - 138.0;
                text.draw_in_rect(
                    canvas,
                    &sys.media_title,
                    RectF::new(tx, mc.y + 19.0, text_w, 24.0),
                    15.0,
                    FontWeight::SemiBold,
                    Rgba::WHITE.with_alpha(0.96 * glance_alpha),
                    Align::Left,
                );
                let sub = if sys.media_artist.is_empty() {
                    if sys.media_playing {
                        "正在播放"
                    } else {
                        "已暂停"
                    }
                } else {
                    sys.media_artist.as_str()
                };
                text.draw_in_rect(
                    canvas,
                    sub,
                    RectF::new(tx, mc.y + 44.0, text_w, 20.0),
                    13.0,
                    FontWeight::Normal,
                    Rgba::WHITE.with_alpha(0.68 * glance_alpha),
                    Align::Left,
                );

                // Transport buttons (`prev`, `play-pause`, `next`)
                let btns = [
                    ("skip_prev", "previous"),
                    (if sys.media_playing { "pause" } else { "play" }, "play-pause"),
                    ("skip_next", "next"),
                ];
                let mut bx = mc.x + mc.w - 126.0;
                for (ic, cmd) in btns {
                    let br = RectF::new(bx, mc.y + 25.0, 36.0, 36.0);
                    let is_main = cmd == "play-pause";
                    if is_main {
                        canvas.fill_round_rect(
                            br,
                            18.0,
                            Rgba::rgba_f(255, 255, 255, 0.18 * glance_alpha),
                        );
                    }
                    icons.draw(
                        canvas,
                        ic,
                        RectF::new(br.x + 8.5, br.y + 8.5, 19.0, 19.0),
                        Rgba::WHITE.with_alpha(0.94 * glance_alpha),
                        2.0,
                    );
                    if ar < 0.25 {
                        self.hits.push((br, LockHit::MediaCmd(cmd)));
                    }
                    bx += 38.0;
                }
                cy += mc.h + 12.0;
            }

            // Notification summary stack (clean wallpaper when empty — respects lock_notif_mode!)
            let lock_notif_mode = crate::sys_state::current_lock_notif_mode();
            if lock_notif_mode != "hide" {
                let redact = lock_notif_mode == "redact";
                for notif in notifs.iter().rev().take(3) {
                    if cy + 74.0 > sh - 86.0 {
                        break;
                    }
                    let nc = RectF::new(stack_x, cy, stack_w, 72.0);
                    canvas.draw_soft_shadow(nc, 20.0, 8.0, 2.0, 0.20 * glance_alpha);
                    canvas.fill_glass_corners_styled(
                        nc,
                        [20.0; 4],
                        &lock_pal,
                        Rgba::rgba_f(18, 24, 38, 0.52),
                        Rgba::TRANSPARENT,
                        0.035,
                        glance_alpha,
                    );
                    let (c1, c2, ic_name) = notif_app_style(&notif.app_name);
                    let badge = RectF::new(nc.x + 14.0, nc.y + 16.0, 40.0, 40.0);
                    canvas.fill_v_gradient(
                        badge,
                        12.0,
                        c1.with_alpha(glance_alpha),
                        c2.with_alpha(glance_alpha),
                    );
                    icons.draw(
                        canvas,
                        ic_name,
                        RectF::new(badge.x + 10.0, badge.y + 10.0, 20.0, 20.0),
                        Rgba::WHITE.with_alpha(glance_alpha),
                        2.0,
                    );
                    let ntx = badge.x + badge.w + 12.0;
                    let nw = nc.w - (ntx - nc.x) - 16.0;
                    let line1 = if redact {
                        notif.app_name.as_str()
                    } else {
                        notif.summary.as_str()
                    };
                    text.draw_in_rect(
                        canvas,
                        line1,
                        RectF::new(ntx, nc.y + 14.0, nw - 56.0, 22.0),
                        14.2,
                        FontWeight::SemiBold,
                        Rgba::WHITE.with_alpha(0.95 * glance_alpha),
                        Align::Left,
                    );
                    text.draw_in_rect(
                        canvas,
                        &notif.time_hm,
                        RectF::new(nc.x + nc.w - 68.0, nc.y + 15.0, 52.0, 18.0),
                        11.8,
                        FontWeight::Normal,
                        Rgba::WHITE.with_alpha(0.60 * glance_alpha),
                        Align::Right,
                    );
                    let line2 = if redact {
                        "通知内容已隐藏"
                    } else if notif.body.trim().is_empty() {
                        notif.app_name.as_str()
                    } else {
                        notif.body.as_str()
                    };
                    text.draw_in_rect(
                        canvas,
                        line2,
                        RectF::new(ntx, nc.y + 37.0, nw, 20.0),
                        12.8,
                        FontWeight::Normal,
                        Rgba::WHITE.with_alpha(0.72 * glance_alpha),
                        Align::Left,
                    );
                    cy += nc.h + 10.0;
                }
                if notifs.len() > 3 && cy + 24.0 <= sh - 74.0 {
                    let more = format!("还有 {} 条通知", notifs.len() - 3);
                    text.draw_in_rect(
                        canvas,
                        &more,
                        RectF::new(stack_x, cy, stack_w, 22.0),
                        12.5,
                        FontWeight::Medium,
                        Rgba::WHITE.with_alpha(0.68 * glance_alpha),
                        Align::Center,
                    );
                }
            }

            // Bottom unlock prompt + pill indicator
            let prompt = if self.is_greeter() {
                "向上轻扫登录"
            } else {
                "向上轻扫解锁"
            };
            let prompt_r = RectF::new((sw - 260.0) * 0.5, sh - 58.0, 260.0, 24.0);
            text.draw_in_rect(
                canvas,
                prompt,
                prompt_r,
                13.5,
                FontWeight::Medium,
                Rgba::WHITE.with_alpha(0.80 * glance_alpha),
                Align::Center,
            );
            let bar_r = RectF::new((sw - 134.0) * 0.5, sh - 22.0, 134.0, 4.5);
            canvas.fill_round_rect(bar_r, 2.25, Rgba::WHITE.with_alpha(0.76 * glance_alpha));
            if ar < 0.25 {
                self.hits.push((
                    RectF::new((sw - 340.0) * 0.5, sh - 76.0, 340.0, 76.0),
                    LockHit::OpenAuth,
                ));
            }
        }

        // 6. Stage 2 Sleek Centered Liquid Glass Password Capsule + Embedded `tb520fu-osk`
        let auth_alpha = ((ar - 0.18) / 0.82).clamp(0.0, 1.0);
        if auth_alpha > 0.01 {
            let pill_w = 452.0_f32;
            let pill_h = 56.0_f32;
            let base_y = (clk_y + clk_h + 58.0)
                .max(avail_h * 0.50 - 8.0)
                .min(avail_h - pill_h - 46.0);
            let slide_off = (1.0 - ar) * 34.0;
            let shake_x = self.error_shake.pos;
            let pill = RectF::new(
                (sw - pill_w) * 0.5 + shake_x,
                base_y + slide_off,
                pill_w,
                pill_h,
            );

            // Backdrop tap above OSK dismisses back to Stage 1 Glance
            if ar > 0.65 {
                self.hits.push((
                    RectF::new(0.0, 0.0, sw, avail_h),
                    LockHit::DismissAuth,
                ));
            }

            // Subtle user label pill above capsule (zero giant avatar circle!)
            let user_lbl = self.username().to_string();
            let (utw, _) = text.measure(&user_lbl, 13.2, FontWeight::Medium, canvas.scale);
            let ucontent_w = 14.0 + 6.0 + utw;
            let ubadge_w = (ucontent_w + 24.0).clamp(78.0, 220.0);
            let ubadge = RectF::new(
                (sw - ubadge_w) * 0.5,
                pill.y - 36.0,
                ubadge_w,
                26.0,
            );
            canvas.fill_round_rect(
                ubadge,
                13.0,
                Rgba::rgba_f(255, 255, 255, 0.10 * auth_alpha),
            );
            let ux0 = ubadge.x + (ubadge.w - ucontent_w) * 0.5;
            icons.draw(
                canvas,
                "lock",
                RectF::new(ux0, ubadge.y + 6.0, 14.0, 14.0),
                Rgba::WHITE.with_alpha(0.82 * auth_alpha),
                1.9,
            );
            text.draw_in_rect(
                canvas,
                &user_lbl,
                RectF::new(ux0 + 20.0, ubadge.y, utw + 6.0, ubadge.h),
                13.2,
                FontWeight::Medium,
                Rgba::WHITE.with_alpha(0.90 * auth_alpha),
                Align::Left,
            );

            // Liquid Glass Password Capsule
            canvas.draw_soft_shadow(pill, 28.0, 14.0, 3.0, 0.30 * auth_alpha);
            canvas.fill_glass_corners_styled(
                pill,
                [28.0; 4],
                &lock_pal,
                Rgba::rgba_f(22, 28, 44, 0.68),
                Rgba::TRANSPARENT,
                0.055,
                auth_alpha,
            );
            let rim_col = if self.error_msg.is_some() {
                Rgba::hex(0xf97316).with_alpha(0.75 * auth_alpha)
            } else if !self.pw_buf.is_empty() {
                Rgba::hex(0x60a5fa).with_alpha(0.52 * auth_alpha)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.22 * auth_alpha)
            };
            canvas.stroke_round_rect(pill, 28.0, 1.2, rim_col);

            // Tapping inside the password field summons touch OSK if currently hidden
            if ar > 0.65 {
                self.hits.push((pill, LockHit::OpenAuth));
            }

            // Left status icon inside capsule
            let left_ic_col = if self.verifying {
                Rgba::hex(0x34d399).with_alpha(auth_alpha)
            } else if !self.pw_buf.is_empty() {
                Rgba::hex(0x60a5fa).with_alpha(auth_alpha)
            } else {
                Rgba::WHITE.with_alpha(0.62 * auth_alpha)
            };
            icons.draw(
                canvas,
                "key",
                RectF::new(pill.x + 19.0, pill.y + (pill_h - 18.0) * 0.5, 18.0, 18.0),
                left_ic_col,
                2.0,
            );

            // Password content (placeholder / plaintext / evenly-spaced bullets)
            let field_x = pill.x + 48.0;
            let field_w = pill.w - 48.0 - 128.0;
            if self.verifying {
                text.draw_in_rect(
                    canvas,
                    "正在验证…",
                    RectF::new(field_x, pill.y, field_w, pill_h),
                    15.0,
                    FontWeight::Medium,
                    Rgba::WHITE.with_alpha(0.88 * auth_alpha),
                    Align::Left,
                );
            } else if self.pw_buf.is_empty() {
                text.draw_in_rect(
                    canvas,
                    "输入密码",
                    RectF::new(field_x, pill.y, field_w, pill_h),
                    15.5,
                    FontWeight::Normal,
                    Rgba::WHITE.with_alpha(0.46 * auth_alpha),
                    Align::Left,
                );
            } else if self.show_pw {
                text.draw_in_rect(
                    canvas,
                    &self.pw_buf,
                    RectF::new(field_x, pill.y, field_w, pill_h),
                    16.0,
                    FontWeight::Medium,
                    Rgba::WHITE.with_alpha(0.96 * auth_alpha),
                    Align::Left,
                );
            } else {
                let n_chars = self.pw_buf.chars().count().min(20);
                let step = if n_chars > 14 { 13.5 } else { 16.0 };
                let dot_r = 4.6_f32;
                let mut dx = field_x + dot_r;
                let cy_dot = pill.y + pill_h * 0.5;
                for _ in 0..n_chars {
                    canvas.fill_round_rect(
                        RectF::new(dx - dot_r, cy_dot - dot_r, dot_r * 2.0, dot_r * 2.0),
                        dot_r,
                        Rgba::WHITE.with_alpha(0.95 * auth_alpha),
                    );
                    dx += step;
                }
            }

            // Right inline buttons: Show/Hide PW, Touch OSK toggle, Submit arrow circle
            let mut btn_rx = pill.x + pill.w - 9.0;

            // Submit circle (`38 x 38`)
            btn_rx -= 38.0;
            let sub_r = RectF::new(btn_rx, pill.y + (pill_h - 38.0) * 0.5, 38.0, 38.0);
            let can_submit = !self.pw_buf.is_empty() && !self.verifying;
            let sub_bg = if can_submit {
                Rgba::hex(0x3b82f6).with_alpha(auth_alpha)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.12 * auth_alpha)
            };
            canvas.fill_round_rect(sub_r, 19.0, sub_bg);
            icons.draw(
                canvas,
                "arrow_right",
                RectF::new(sub_r.x + 9.5, sub_r.y + 9.5, 19.0, 19.0),
                Rgba::WHITE.with_alpha(if can_submit {
                    0.98 * auth_alpha
                } else {
                    0.40 * auth_alpha
                }),
                2.1,
            );
            if ar > 0.65 {
                self.hits.push((sub_r.inset(-3.0, -3.0), LockHit::Submit));
            }

            // Touch OSK toggle icon (`34 x 34`)
            btn_rx -= 38.0;
            let kbd_r = RectF::new(btn_rx, pill.y + (pill_h - 34.0) * 0.5, 34.0, 34.0);
            if self.touch_osk_open {
                canvas.fill_round_rect(
                    kbd_r,
                    17.0,
                    Rgba::rgba_f(96, 165, 250, 0.20 * auth_alpha),
                );
            }
            icons.draw(
                canvas,
                "keyboard",
                RectF::new(kbd_r.x + 8.0, kbd_r.y + 8.0, 18.0, 18.0),
                if self.touch_osk_open {
                    Rgba::hex(0x93c5fd).with_alpha(auth_alpha)
                } else {
                    Rgba::WHITE.with_alpha(0.62 * auth_alpha)
                },
                1.9,
            );
            if ar > 0.65 {
                self.hits.push((kbd_r.inset(-2.0, -2.0), LockHit::ToggleTouchOsk));
            }

            // Eye toggle (`34 x 34`) when password non-empty
            if !self.pw_buf.is_empty() {
                btn_rx -= 36.0;
                let eye_r = RectF::new(btn_rx, pill.y + (pill_h - 34.0) * 0.5, 34.0, 34.0);
                icons.draw(
                    canvas,
                    if self.show_pw { "eye_off" } else { "eye" },
                    RectF::new(eye_r.x + 8.0, eye_r.y + 8.0, 18.0, 18.0),
                    Rgba::WHITE.with_alpha(0.72 * auth_alpha),
                    1.9,
                );
                if ar > 0.65 {
                    self.hits.push((eye_r.inset(-2.0, -2.0), LockHit::ToggleShowPw));
                }
            }

            // Error caption or subtle hint below capsule
            let cap_y = pill.y + pill.h + 12.0;
            if let Some((err, _)) = &self.error_msg {
                text.draw_in_rect(
                    canvas,
                    err,
                    RectF::new(pill.x - 40.0, cap_y, pill.w + 80.0, 22.0),
                    13.2,
                    FontWeight::SemiBold,
                    Rgba::hex(0xfb923c).with_alpha(auth_alpha),
                    Align::Center,
                );
            } else if self.kbd_caps {
                text.draw_in_rect(
                    canvas,
                    "大写锁定已开启",
                    RectF::new(pill.x, cap_y, pill.w, 20.0),
                    12.4,
                    FontWeight::Medium,
                    Rgba::hex(0xfbbf24).with_alpha(0.88 * auth_alpha),
                    Align::Center,
                );
            }
        }

        // 7. Composite Embedded `tb520fu-osk` smoothly at bottom when visible!
        if self.osk.shown || self.osk.slide_y.is_animating() {
            let (ox, oy) = crate::inline_osk_origin(&self.osk, sw, sh);
            let pw_osk = ((self.osk.metrics.surf_w as f32) * canvas.scale)
                .round()
                .max(1.0) as u32;
            let ph_osk = ((self.osk.metrics.surf_h as f32) * canvas.scale)
                .round()
                .max(1.0) as u32;
            self.osk_sub_canvas.resize(pw_osk, ph_osk, canvas.scale);
            self.osk_sub_canvas.set_screen_context(ox, oy, sw, sh);
            self.osk_ren.render(&mut self.osk, &mut self.osk_sub_canvas);
            crate::composite_subcanvas(canvas, &self.osk_sub_canvas, ox, oy);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_standby_scene(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        sys: &SysSnapshot,
        sw: f32,
        sh: f32,
        sr: f32,
    ) {
        // IPS-aware ambient dimming: smooth warm tone in dark rooms (`lux < 5`), NEVER `#000000` fake-OLED
        // gray backlight glow and NEVER transparent bleed-through over underlying Lockscreen!
        let night_lux = sys.lux > 0.05 && sys.lux < 5.0;
        let dim = if night_lux { 0.68_f32 } else { 1.0_f32 };
        let alpha = sr.clamp(0.0, 1.0);

        if alpha >= 0.995 {
            canvas.clear(Rgba::rgb(11, 15, 24));
        } else {
            canvas.fill_rect(
                RectF::new(0.0, 0.0, sw, sh),
                Rgba::rgb(11, 15, 24).with_alpha(alpha),
            );
        }

        let ps = self.page_spring.pos.clamp(0.0, 1.0);
        if ps < 0.995 {
            self.render_standby_desk_hub(
                canvas,
                icons,
                text,
                sys,
                sw,
                sh,
                alpha * (1.0 - ps),
                dim,
                night_lux,
                -ps * sw * 0.16,
            );
        }
        if ps > 0.005 {
            self.render_standby_magazine(
                canvas,
                icons,
                text,
                sys,
                sw,
                sh,
                alpha * ps,
                dim,
                (1.0 - ps) * sw * 0.16,
            );
        }

        // Bottom 2-dot page indicator + subtle wake caption
        let dot_y = sh - 28.0;
        let d0_w = 8.0 + (14.0) * (1.0 - ps);
        let d1_w = 8.0 + (14.0) * ps;
        let tot_w = d0_w + 8.0 + d1_w;
        let dx0 = (sw - tot_w) * 0.5;
        canvas.fill_round_rect(
            RectF::new(dx0, dot_y, d0_w, 6.0),
            3.0,
            Rgba::WHITE.with_alpha((0.35 + 0.55 * (1.0 - ps)) * alpha * dim),
        );
        canvas.fill_round_rect(
            RectF::new(dx0 + d0_w + 8.0, dot_y, d1_w, 6.0),
            3.0,
            Rgba::WHITE.with_alpha((0.35 + 0.55 * ps) * alpha * dim),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn render_standby_desk_hub(
        &self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        sys: &SysSnapshot,
        sw: f32,
        sh: f32,
        alpha: f32,
        dim: f32,
        night_lux: bool,
        x_shift: f32,
    ) {
        // Subtle atmospheric radial glow on deep slate backdrop (avoids flat IPS backlight uniformity artifacts)
        let monet = tb520fu_ui::vk_renderer::active_wallpaper_monet();
        let glow_c = if night_lux {
            Rgba::rgba_f(180, 108, 48, 0.16 * alpha)
        } else {
            monet.accent.with_alpha(0.18 * alpha)
        };
        canvas.fill_radial_glow(
            RectF::new(0.0, 0.0, sw, sh),
            0.0,
            sw * 0.28 + x_shift,
            sh * 0.42,
            560.0,
            glow_c,
        );
        canvas.fill_radial_glow(
            RectF::new(0.0, 0.0, sw, sh),
            0.0,
            sw * 0.78 + x_shift,
            sh * 0.62,
            520.0,
            Rgba::rgba_f(56, 189, 248, 0.10 * alpha * dim),
        );

        let content_w = 1268.0_f32;
        let content_h = 468.0_f32;
        let x0 = (sw - content_w) * 0.5 + x_shift;
        let y0 = (sh - content_h) * 0.5 - 8.0;

        // ---- Left Column: Twin Sculpted Split-Flip Clock Cards (`HH` & `MM`) ----
        let (hh, mm) = sys
            .time_hm
            .split_once(':')
            .unwrap_or((sys.time_hm.as_str(), "00"));
        let flip_w = 296.0_f32;
        let flip_h = content_h;
        let flip_gap = 20.0_f32;
        let num_col = if night_lux {
            Rgba::rgb(252, 224, 182).with_alpha(0.92 * alpha)
        } else {
            Rgba::rgb(244, 247, 252).with_alpha(0.96 * alpha)
        };

        for (ci, digits) in [hh, mm].into_iter().enumerate() {
            let fx = x0 + (ci as f32) * (flip_w + flip_gap);
            let fr = RectF::new(fx, y0, flip_w, flip_h);
            canvas.draw_soft_shadow(fr, 38.0, 18.0, 4.0, 0.42 * alpha);
            canvas.fill_v_gradient(
                fr,
                38.0,
                Rgba::rgb(29, 36, 52).with_alpha(alpha * dim),
                Rgba::rgb(18, 23, 35).with_alpha(alpha * dim),
            );
            canvas.stroke_round_rect(
                fr,
                38.0,
                1.2,
                Rgba::rgba_f(255, 255, 255, 0.10 * alpha * dim),
            );

            // Subtle upper-half sheen
            canvas.fill_v_gradient(
                RectF::new(fr.x + 2.0, fr.y + 2.0, fr.w - 4.0, fr.h * 0.5 - 3.0),
                36.0,
                Rgba::rgba_f(255, 255, 255, 0.055 * alpha * dim),
                Rgba::rgba_f(255, 255, 255, 0.01 * alpha * dim),
            );

            // Giant tabular numerals optically centered on the split-flip hinge seam
            text.draw_in_rect(
                canvas,
                digits,
                RectF::new(fr.x, fr.y + (fr.h - 216.0) * 0.5 - 22.0, fr.w, 216.0),
                186.0,
                FontWeight::Bold,
                num_col,
                Align::Center,
            );

            // Split-flip center hinge seam + side notches
            let mid_y = fr.y + fr.h * 0.5;
            canvas.fill_rect(
                RectF::new(fr.x + 1.0, mid_y - 1.5, fr.w - 2.0, 2.6),
                Rgba::rgba_f(6, 9, 15, 0.86 * alpha),
            );
            canvas.fill_rect(
                RectF::new(fr.x + 2.0, mid_y + 1.2, fr.w - 4.0, 1.0),
                Rgba::rgba_f(255, 255, 255, 0.07 * alpha * dim),
            );
            canvas.fill_round_rect(
                RectF::new(fr.x + 6.0, mid_y - 9.0, 5.0, 18.0),
                2.5,
                Rgba::rgba_f(8, 11, 18, 0.90 * alpha),
            );
            canvas.fill_round_rect(
                RectF::new(fr.x + fr.w - 11.0, mid_y - 9.0, 5.0, 18.0),
                2.5,
                Rgba::rgba_f(8, 11, 18, 0.90 * alpha),
            );
        }

        // ---- Right Column: Monthly Calendar Card + Power/Bypass/Stylus Status Card ----
        let rx0 = x0 + flip_w * 2.0 + flip_gap + 28.0;
        let rw = (x0 + content_w - rx0).max(520.0);
        let cal_h = 308.0_f32;
        let cal_r = RectF::new(rx0, y0, rw, cal_h);

        canvas.draw_soft_shadow(cal_r, 32.0, 16.0, 4.0, 0.36 * alpha);
        canvas.fill_v_gradient(
            cal_r,
            32.0,
            Rgba::rgb(26, 33, 48).with_alpha(alpha * dim),
            Rgba::rgb(19, 24, 36).with_alpha(alpha * dim),
        );
        canvas.stroke_round_rect(
            cal_r,
            32.0,
            1.1,
            Rgba::rgba_f(255, 255, 255, 0.09 * alpha * dim),
        );

        let wdays = ["日", "一", "二", "三", "四", "五", "六"];
        let cal = current_local_calendar();
        let cal_title = format!("{}年 {}月", cal.year, cal.month);
        let today_wday = ((cal.first_wday as usize) + (cal.today.saturating_sub(1) as usize)) % 7;
        let cal_sub = format!("{}月{}日 周{}", cal.month, cal.today, wdays[today_wday]);
        text.draw_in_rect(
            canvas,
            &cal_title,
            RectF::new(cal_r.x + 26.0, cal_r.y + 20.0, 240.0, 28.0),
            19.0,
            FontWeight::SemiBold,
            num_col,
            Align::Left,
        );
        text.draw_in_rect(
            canvas,
            &cal_sub,
            RectF::new(cal_r.x + cal_r.w - 226.0, cal_r.y + 22.0, 200.0, 24.0),
            14.2,
            FontWeight::Medium,
            Rgba::WHITE.with_alpha(0.58 * alpha * dim),
            Align::Right,
        );

        let grid_x = cal_r.x + 20.0;
        let grid_w = cal_r.w - 40.0;
        let col_w = grid_w / 7.0;
        let wday_y = cal_r.y + 58.0;
        for (wi, &wd) in wdays.iter().enumerate() {
            let is_wknd = wi == 0 || wi == 6;
            let wc = if is_wknd {
                Rgba::hex(0xfb923c).with_alpha(0.75 * alpha * dim)
            } else {
                Rgba::WHITE.with_alpha(0.46 * alpha * dim)
            };
            text.draw_in_rect(
                canvas,
                wd,
                RectF::new(grid_x + (wi as f32) * col_w, wday_y, col_w, 22.0),
                12.8,
                FontWeight::SemiBold,
                wc,
                Align::Center,
            );
        }

        let row_y0 = wday_y + 28.0;
        let row_h = 34.5_f32;
        let accent_today = if night_lux {
            Rgba::hex(0xd97706).with_alpha(alpha)
        } else {
            Rgba::hex(0x3b82f6).with_alpha(alpha)
        };
        for day in 1..=cal.days_in_month {
            let slot = (cal.first_wday as u32) + (day as u32 - 1);
            let col = (slot % 7) as usize;
            let row = (slot / 7) as usize;
            let cell = RectF::new(
                grid_x + (col as f32) * col_w,
                row_y0 + (row as f32) * row_h,
                col_w,
                row_h,
            );
            let is_today = day == cal.today;
            if is_today {
                let pill_sz = 30.0_f32;
                canvas.fill_round_rect(
                    RectF::new(
                        cell.x + (cell.w - pill_sz) * 0.5,
                        cell.y + (cell.h - pill_sz) * 0.5,
                        pill_sz,
                        pill_sz,
                    ),
                    15.0,
                    accent_today,
                );
            }
            let ds = day.to_string();
            let dc = if is_today {
                Rgba::WHITE.with_alpha(0.98 * alpha)
            } else if col == 0 || col == 6 {
                Rgba::WHITE.with_alpha(0.64 * alpha * dim)
            } else {
                Rgba::WHITE.with_alpha(0.86 * alpha * dim)
            };
            text.draw_in_rect(
                canvas,
                &ds,
                cell,
                14.0,
                if is_today {
                    FontWeight::Bold
                } else {
                    FontWeight::Medium
                },
                dc,
                Align::Center,
            );
        }

        // Bottom-Right Status Card: Power / `旁路供电` / Stylus / Now Playing
        let st_y = cal_r.y + cal_h + 18.0;
        let st_h = (y0 + content_h - st_y).max(120.0);
        let st_r = RectF::new(rx0, st_y, rw, st_h);
        canvas.draw_soft_shadow(st_r, 28.0, 14.0, 3.0, 0.34 * alpha);
        canvas.fill_v_gradient(
            st_r,
            28.0,
            Rgba::rgb(26, 33, 48).with_alpha(alpha * dim),
            Rgba::rgb(18, 23, 35).with_alpha(alpha * dim),
        );
        canvas.stroke_round_rect(
            st_r,
            28.0,
            1.1,
            Rgba::rgba_f(255, 255, 255, 0.09 * alpha * dim),
        );

        let bypass_on = sys.charge_limit < 100 && sys.usb_online && !sys.bat_charging;
        let p_badge = RectF::new(st_r.x + 22.0, st_r.y + (st_h - 52.0) * 0.5, 52.0, 52.0);
        let (p_ic, p_bg, p_fg) = if bypass_on {
            (
                "bypass_power",
                Rgba::rgba_f(16, 185, 129, 0.20 * alpha),
                Rgba::hex(0x34d399).with_alpha(alpha),
            )
        } else {
            (
                "bolt",
                Rgba::rgba_f(59, 130, 246, 0.20 * alpha),
                Rgba::hex(0x60a5fa).with_alpha(alpha),
            )
        };
        canvas.fill_round_rect(p_badge, 16.0, p_bg);
        icons.draw(
            canvas,
            p_ic,
            RectF::new(p_badge.x + 14.0, p_badge.y + 14.0, 24.0, 24.0),
            p_fg,
            2.1,
        );

        let ptx = p_badge.x + p_badge.w + 16.0;
        let p_headline = if bypass_on {
            format!("旁路供电中 · {}%", sys.bat_pct)
        } else if sys.bat_charging && sys.charge_limit < 100 {
            if sys.bat_watts >= 1.0 {
                format!(
                    "正在充至{}% · {}% ({:.1}W)",
                    sys.charge_limit, sys.bat_pct, sys.bat_watts
                )
            } else {
                format!("正在充至{}% · {}%", sys.charge_limit, sys.bat_pct)
            }
        } else if sys.bat_charging && sys.bat_watts >= 1.0 {
            format!("正在充电 · {}% ({:.1}W)", sys.bat_pct, sys.bat_watts)
        } else {
            format!("电源已连接 · {}%", sys.bat_pct)
        };
        let mut sub_parts: Vec<String> = Vec::new();
        if let Some(pen_pct) = sys
            .sorted_topbar_peripherals()
            .into_iter()
            .find(|d| d.official_pen)
            .and_then(|d| d.battery_pct)
            .or_else(|| (sys.pen_docked && sys.pen_pct >= 0).then_some(sys.pen_pct.min(100) as u8))
        {
            sub_parts.push(format!("手写笔 {pen_pct}%"));
        }
        if !sys.media_title.is_empty() {
            sub_parts.push(format!("♪ {}", sys.media_title));
        }

        let head_y = if sub_parts.is_empty() {
            st_r.y + 34.0
        } else {
            st_r.y + 24.0
        };
        text.draw_in_rect(
            canvas,
            &p_headline,
            RectF::new(ptx, head_y, st_r.w - 110.0, 26.0),
            16.5,
            FontWeight::SemiBold,
            num_col,
            Align::Left,
        );

        if !sub_parts.is_empty() {
            let sub_line = sub_parts.join("  ·  ");
            text.draw_in_rect(
                canvas,
                &sub_line,
                RectF::new(ptx, st_r.y + 54.0, st_r.w - 108.0, 22.0),
                13.2,
                FontWeight::Normal,
                Rgba::WHITE.with_alpha(0.62 * alpha * dim),
                Align::Left,
            );
        }

        // Progress bar at bottom of power card
        let bar_r = RectF::new(ptx, st_r.y + st_h - 26.0, st_r.w - 116.0, 6.5);
        canvas.fill_round_rect(
            bar_r,
            3.25,
            Rgba::rgba_f(255, 255, 255, 0.10 * alpha * dim),
        );
        let bfrac = (sys.bat_pct as f32 / 100.0).clamp(0.04, 1.0);
        canvas.fill_round_rect(
            RectF::new(bar_r.x, bar_r.y, bar_r.w * bfrac, bar_r.h),
            3.25,
            p_fg,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn render_standby_magazine(
        &self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        sys: &SysSnapshot,
        sw: f32,
        sh: f32,
        alpha: f32,
        dim: f32,
        x_shift: f32,
    ) {
        let full = RectF::new(x_shift, 0.0, sw, sh);
        let fade_p = self.deck.fade.pos.clamp(0.0, 1.0);
        if let Some(cur) = &self.deck.cur_img {
            if fade_p < 0.99 {
                if let Some(prev) = &self.deck.prev_img {
                    prev.draw_cover(canvas, full, alpha * dim * (1.0 - fade_p));
                }
            }
            cur.draw_cover(canvas, full, alpha * dim * fade_p);
        } else {
            canvas.draw_wallpaper_background(full, 0.88 * alpha * dim);
        }

        // Cinematic bottom-half editorial gradient
        canvas.fill_v_gradient(
            RectF::new(0.0, sh * 0.42, sw, sh * 0.58),
            0.0,
            Rgba::TRANSPARENT,
            Rgba::rgba_f(8, 11, 18, 0.82 * alpha),
        );

        // Editorial bottom-left oversized clock & date
        let lx = 64.0 + x_shift;
        text.draw_in_rect(
            canvas,
            &sys.time_hm,
            RectF::new(lx, sh - 238.0, 520.0, 122.0),
            106.0,
            FontWeight::Bold,
            Rgba::WHITE.with_alpha(0.96 * alpha * dim),
            Align::Left,
        );
        text.draw_in_rect(
            canvas,
            &sys.date_long,
            RectF::new(lx + 4.0, sh - 96.0, 520.0, 28.0),
            19.0,
            FontWeight::Medium,
            Rgba::WHITE.with_alpha(0.80 * alpha * dim),
            Align::Left,
        );

        // Bottom-right sleek status pill (dynamically measured & centered)
        let bypass_on = sys.charge_limit < 100 && sys.usb_online && !sys.bat_charging;
        let status_txt = if bypass_on {
            format!("旁路供电 · {}%", sys.bat_pct)
        } else if sys.bat_charging && sys.charge_limit < 100 {
            format!("充至{}% · {}%", sys.charge_limit, sys.bat_pct)
        } else {
            format!("电源已连接 · {}%", sys.bat_pct)
        };
        let (stw, _) = text.measure(&status_txt, 13.6, FontWeight::SemiBold, canvas.scale);
        let scontent_w = 18.0 + 8.0 + stw;
        let pill_w = (scontent_w + 34.0).clamp(154.0, 280.0);
        let pr = RectF::new(sw - pill_w - 64.0 + x_shift, sh - 108.0, pill_w, 42.0);
        canvas.fill_round_rect(pr, 21.0, Rgba::rgba_f(14, 18, 28, 0.68 * alpha));
        canvas.stroke_round_rect(
            pr,
            21.0,
            1.0,
            Rgba::rgba_f(255, 255, 255, 0.18 * alpha * dim),
        );
        let sx0 = pr.x + (pr.w - scontent_w) * 0.5;
        icons.draw(
            canvas,
            if bypass_on { "bypass_power" } else { "bolt" },
            RectF::new(sx0, pr.y + 12.0, 18.0, 18.0),
            if bypass_on {
                Rgba::hex(0x34d399).with_alpha(alpha)
            } else {
                Rgba::hex(0x60a5fa).with_alpha(alpha)
            },
            2.0,
        );
        text.draw_in_rect(
            canvas,
            &status_txt,
            RectF::new(sx0 + 26.0, pr.y, stw + 6.0, pr.h),
            13.6,
            FontWeight::SemiBold,
            Rgba::WHITE.with_alpha(0.92 * alpha * dim),
            Align::Left,
        );
    }

    pub fn on_touch_down(&mut self, slot: i32, x: f32, y: f32) -> bool {
        self.wake_monitors_if_blanked();
        self.last_activity = Instant::now();

        let in_standby = self.phase == LockPhase::StandbyCompanion;
        if !in_standby && self.osk.shown && self.auth_reveal.pos > 0.55 {
            let (sw, sh) = (
                self.last_screen_size.0 as f32,
                self.last_screen_size.1 as f32,
            );
            let (ox, oy) = crate::inline_osk_origin(&self.osk, sw, sh);
            if self.osk.on_touch_down(slot, x - ox, y - oy) {
                self.osk_active_slot = Some(slot);
                let _ = self.drain_osk_outbox();
                return true;
            }
        }

        if !in_standby {
            for (i, (r, _)) in self.hits.iter().enumerate().rev() {
                if r.contains(x, y) {
                    self.pressed_hit = Some(i);
                    break;
                }
            }
        }

        self.gesture = Some(LockGesture {
            slot,
            x0: x,
            y0: y,
            last_y: y,
            last_t: Instant::now(),
            vy: 0.0,
            start_auth: self.auth_reveal.pos,
            in_standby,
            swiping_auth: false,
        });
        true
    }

    pub fn on_touch_motion(&mut self, slot: i32, x: f32, y: f32) -> bool {
        self.last_activity = Instant::now();
        if self.osk_active_slot == Some(slot) {
            let (sw, sh) = (
                self.last_screen_size.0 as f32,
                self.last_screen_size.1 as f32,
            );
            let (ox, oy) = crate::inline_osk_origin(&self.osk, sw, sh);
            let rd = self.osk.on_touch_motion(slot, x - ox, y - oy);
            let out = self.drain_osk_outbox();
            return rd || out;
        }

        let Some(g) = &mut self.gesture else {
            return false;
        };
        if g.slot != slot {
            return false;
        }

        let now = Instant::now();
        let dt = now.duration_since(g.last_t).as_secs_f32().max(0.001);
        let dy_step = y - g.last_y;
        g.vy = g.vy * 0.45 + (dy_step / dt) * 0.55;
        g.last_y = y;
        g.last_t = now;

        let dx = x - g.x0;
        let dy = y - g.y0;
        if g.in_standby {
            // Horizontal drag previews page transition between Desk Hub (0) and Magazine Frame (1)
            if dx.abs() > 14.0 && dx.abs() > dy.abs() {
                let base = self.standby_page as f32;
                let preview = (base - dx / 420.0).clamp(0.0, 1.0);
                self.page_spring.snap(preview);
                return true;
            }
            return false;
        }

        // 1:1 vertical swipe on Lockscreen transitions continuously between Stage 1 Glance & Stage 2 Auth!
        if !g.swiping_auth && dy.abs() > 12.0 && dy.abs() > dx.abs() * 1.15 {
            g.swiping_auth = true;
            self.pressed_hit = None;
            crate::sys_state::request_gpu_boost(500);
        }
        if g.swiping_auth {
            let next_auth = (g.start_auth + (-dy / 260.0)).clamp(0.0, 1.0);
            self.auth_reveal.snap(next_auth);
            return true;
        }
        false
    }

    /// Returns `needs_redraw`. Actual unlock happens asynchronously via `tick()` once PAM/greetd succeeds!
    pub fn on_touch_up(&mut self, slot: i32, x: f32, y: f32) -> bool {
        self.last_activity = Instant::now();
        if self.osk_active_slot == Some(slot) {
            self.osk_active_slot = None;
            let rd = self.osk.on_touch_up(slot);
            let out = self.drain_osk_outbox();
            return rd || out || true;
        }

        let prev = self.pressed_hit.take();
        let Some(g) = self.gesture.take() else {
            return prev.is_some();
        };
        if g.slot != slot {
            return false;
        }

        let dx = x - g.x0;
        let dy = y - g.y0;

        if g.in_standby {
            if dx.abs() > 54.0 && dx.abs() > dy.abs() {
                let next_page = if dx < 0.0 { 1 } else { 0 };
                self.set_standby_page(next_page);
                HapticEngine::global().pulse_norm(HapticKind::Tick, 0.5);
                return true;
            }
            self.set_standby_page(self.standby_page);
            if slot == -1 {
                // Touchpad / pointer click on Standby goes directly to Stage 2 Password Entry!
                self.open_auth_stage(false);
            } else if dy < -48.0 {
                // Direct upward swipe from Standby goes straight to Stage 2 Password!
                self.open_auth_stage(true);
            } else {
                self.wake_to_prompt();
            }
            return true;
        }

        if g.swiping_auth {
            let open = if g.vy < -280.0 {
                true
            } else if g.vy > 280.0 {
                false
            } else {
                self.auth_reveal.pos >= 0.42
            };
            if open {
                self.open_auth_stage(slot != -1);
            } else {
                self.close_auth_stage();
            }
            return true;
        }

        if dx.hypot(dy) <= 18.0 {
            let mut chosen: Option<LockHit> = None;
            for (i, (r, h)) in self.hits.iter().enumerate().rev() {
                if r.contains(x, y) {
                    if prev == Some(i) || prev.is_none() {
                        chosen = Some(h.clone());
                    }
                    break;
                }
            }
            if let Some(act) = chosen {
                self.apply_hit(act, slot == -1);
                return true;
            }
            // Touchpad / pointer click anywhere on Stage 1 Glance immediately opens Stage 2
            // Password Entry (without popping up touch OSK); on Greeter touchscreen, tapping
            // bottom half also opens Stage 2 for effortless login.
            if self.auth_reveal.pos < 0.25 {
                if slot == -1 {
                    self.open_auth_stage(false);
                    return true;
                }
                if self.is_greeter() && y > self.last_screen_size.1 as f32 * 0.45 {
                    self.open_auth_stage(true);
                    return true;
                }
            }
        }
        prev.is_some()
    }

    pub fn on_pointer_activity(&mut self) {
        self.wake_monitors_if_blanked();
        self.last_activity = Instant::now();
    }

    /// Touchpad 2-finger tap or bottom-right physical click (`BTN_RIGHT` / `BTN_MIDDLE`).
    pub fn on_secondary_click(&mut self) -> bool {
        self.wake_monitors_if_blanked();
        self.last_activity = Instant::now();
        if self.phase == LockPhase::StandbyCompanion || self.auth_reveal.target < 0.5 {
            self.open_auth_stage(false);
            return true;
        }
        if !self.verifying && self.pw_buf.is_empty() {
            self.close_auth_stage();
            return true;
        }
        false
    }

    /// Touchpad 2-finger scroll (`dy > 0` = fingers swipe UP with `natural-scroll`; `dx > 0` = fingers swipe LEFT).
    pub fn on_scroll(&mut self, dx: f32, dy: f32) -> bool {
        self.wake_monitors_if_blanked();
        self.last_activity = Instant::now();
        if dx.abs() < 0.01 && dy.abs() < 0.01 {
            return false;
        }
        let now = Instant::now();
        let dt = self
            .scroll_last_at
            .map(|t| now.duration_since(t).as_secs_f32().clamp(0.001, 0.08))
            .unwrap_or(0.012);
        self.scroll_last_at = Some(now);

        if (self.phase == LockPhase::StandbyCompanion || self.scroll_standby_active)
            && !self.scroll_auth_active
        {
            if self.scroll_standby_active || (dx.abs() > dy.abs() * 1.1 && dx.abs() > 0.35) {
                self.scroll_standby_active = true;
                self.scroll_standby_dx += dx;
                let base = self.standby_page as f32;
                let preview = (base + self.scroll_standby_dx / 180.0).clamp(0.0, 1.0);
                self.page_spring.snap(preview);
                return true;
            }
            if dy > 0.35 {
                crate::sys_state::request_gpu_boost(550);
                self.phase = LockPhase::AuthPrompt;
                self.standby_reveal.set_target(0.0);
                self.scroll_auth_active = true;
                self.scroll_vy = dy / dt;
                let next = (self.auth_reveal.pos + dy / 180.0).clamp(0.02, 1.0);
                self.auth_reveal.snap(next);
                return true;
            }
            return false;
        }

        let can_start = (dy > 0.35 && self.auth_reveal.pos < 0.999)
            || (dy < -0.35
                && self.auth_reveal.pos > 0.001
                && !self.verifying
                && self.pw_buf.is_empty());
        if !self.scroll_auth_active && can_start {
            crate::sys_state::request_gpu_boost(550);
            self.scroll_auth_active = true;
            self.scroll_vy = 0.0;
        }
        if self.scroll_auth_active {
            self.scroll_vy = self.scroll_vy * 0.45 + (dy / dt) * 0.55;
            let next = (self.auth_reveal.pos + dy / 180.0).clamp(0.0, 1.0);
            self.auth_reveal.snap(next);
            return true;
        }
        false
    }

    pub fn on_scroll_stop(&mut self) -> bool {
        if self.scroll_standby_active {
            self.scroll_standby_active = false;
            self.scroll_last_at = None;
            let dx = std::mem::replace(&mut self.scroll_standby_dx, 0.0);
            let next_page = if dx > 28.0 {
                1
            } else if dx < -28.0 {
                0
            } else if self.page_spring.pos >= 0.5 {
                1
            } else {
                0
            };
            self.set_standby_page(next_page);
            HapticEngine::global().pulse_norm(HapticKind::Tick, 0.5);
            return true;
        }
        if self.scroll_auth_active {
            self.scroll_auth_active = false;
            self.scroll_last_at = None;
            let vy = std::mem::replace(&mut self.scroll_vy, 0.0);
            let open = if vy > 120.0 {
                true
            } else if vy < -120.0 {
                false
            } else {
                self.auth_reveal.pos >= 0.32
            };
            if open {
                self.open_auth_stage(false);
            } else {
                self.close_auth_stage();
            }
            return true;
        }
        false
    }

    pub fn on_touch_cancel(&mut self) {
        if let Some(slot) = self.osk_active_slot.take() {
            let _ = self.osk.on_touch_up(slot);
            let _ = self.drain_osk_outbox();
        }
        self.pressed_hit = None;
        if let Some(g) = self.gesture.take() {
            if g.in_standby {
                self.set_standby_page(self.standby_page);
            } else if g.swiping_auth {
                if self.auth_reveal.pos >= 0.5 {
                    self.open_auth_stage(true);
                } else {
                    self.close_auth_stage();
                }
            }
        }
    }

    fn apply_hit(&mut self, hit: LockHit, is_pointer: bool) {
        match hit {
            LockHit::OpenAuth => {
                let want_osk = if is_pointer {
                    self.touch_osk_open
                } else {
                    true
                };
                self.open_auth_stage(want_osk);
            }
            LockHit::DismissAuth => {
                if !self.verifying {
                    self.close_auth_stage();
                }
            }
            LockHit::Submit => {
                self.submit_password();
            }
            LockHit::ToggleShowPw => {
                self.show_pw = !self.show_pw;
            }
            LockHit::ToggleTouchOsk => {
                if self.touch_osk_open {
                    self.touch_osk_open = false;
                    self.osk.hide(false);
                } else {
                    self.open_auth_stage(true);
                }
            }
            LockHit::EnterStandby => {
                self.switch_to_standby();
            }
            LockHit::MediaCmd(cmd) => {
                media_cmd(cmd);
            }
        }
    }

    /// Physical / Pogo keyboard handler (`pressed = true` or `false` for Shift tracking!).
    /// Pressing ANY key while blanked, in Standby Companion, or in Stage 1 Glance wakes cleanly
    /// into Stage 2 Password Entry WITHOUT inserting the wakeup keystroke into `pw_buf`!
    pub fn on_evdev_key_event(&mut self, evdev_code: u32, pressed: bool) -> bool {
        match evdev_code {
            42 => {
                self.phys_shift_l = pressed;
                return false;
            }
            54 => {
                self.phys_shift_r = pressed;
                return false;
            }
            _ => {}
        }
        if !pressed {
            return false;
        }

        let was_blanked = self.dpms_blanked_by_idle || !read_panel_dpms_on();
        let was_standby = self.phase == LockPhase::StandbyCompanion;
        let was_glance = self.auth_reveal.target < 0.5;

        self.wake_monitors_if_blanked();
        self.last_activity = Instant::now();

        if evdev_code == 58 {
            self.kbd_caps = !self.kbd_caps;
            if was_blanked || was_standby || was_glance {
                self.open_auth_stage(false);
            }
            return true;
        }

        if was_blanked || was_standby || was_glance {
            if evdev_code == 1 {
                if was_standby {
                    self.wake_to_prompt();
                    return true;
                }
                return was_blanked;
            }
            self.open_auth_stage(false);
            return true;
        }

        if evdev_code == 14 {
            if !self.verifying {
                self.error_msg = None;
                self.pw_buf.pop();
            }
            return true;
        }
        if evdev_code == 1 {
            // Escape clears password buffer first; if already empty, collapses Stage 2 back to Stage 1 Glance!
            if !self.verifying {
                if !self.pw_buf.is_empty() {
                    self.pw_buf.clear();
                    self.error_msg = None;
                } else if self.auth_reveal.target > 0.0 {
                    self.close_auth_stage();
                }
            }
            return true;
        }
        if evdev_code == 28 || evdev_code == 96 {
            if !self.pw_buf.is_empty() {
                self.submit_password();
            }
            return true;
        }

        let shift = self.phys_shift_l || self.phys_shift_r;
        if let Some(ch) = evdev_to_ascii(evdev_code, shift, self.kbd_caps) {
            if self.touch_osk_open {
                self.touch_osk_open = false;
                self.osk.hide(false);
            }
            if !self.verifying && self.pw_buf.len() < 128 {
                self.error_msg = None;
                self.pw_buf.push(ch);
            }
            return true;
        }

        false
    }
}

fn evdev_to_ascii(code: u32, shift: bool, caps: bool) -> Option<char> {
    let alpha = shift ^ caps;
    let c = match code {
        2 => if shift { '!' } else { '1' },
        3 => if shift { '@' } else { '2' },
        4 => if shift { '#' } else { '3' },
        5 => if shift { '$' } else { '4' },
        6 => if shift { '%' } else { '5' },
        7 => if shift { '^' } else { '6' },
        8 => if shift { '&' } else { '7' },
        9 => if shift { '*' } else { '8' },
        10 => if shift { '(' } else { '9' },
        11 => if shift { ')' } else { '0' },
        12 => if shift { '_' } else { '-' },
        13 => if shift { '+' } else { '=' },
        16 => if alpha { 'Q' } else { 'q' },
        17 => if alpha { 'W' } else { 'w' },
        18 => if alpha { 'E' } else { 'e' },
        19 => if alpha { 'R' } else { 'r' },
        20 => if alpha { 'T' } else { 't' },
        21 => if alpha { 'Y' } else { 'y' },
        22 => if alpha { 'U' } else { 'u' },
        23 => if alpha { 'I' } else { 'i' },
        24 => if alpha { 'O' } else { 'o' },
        25 => if alpha { 'P' } else { 'p' },
        26 => if shift { '{' } else { '[' },
        27 => if shift { '}' } else { ']' },
        30 => if alpha { 'A' } else { 'a' },
        31 => if alpha { 'S' } else { 's' },
        32 => if alpha { 'D' } else { 'd' },
        33 => if alpha { 'F' } else { 'f' },
        34 => if alpha { 'G' } else { 'g' },
        35 => if alpha { 'H' } else { 'h' },
        36 => if alpha { 'J' } else { 'j' },
        37 => if alpha { 'K' } else { 'k' },
        38 => if alpha { 'L' } else { 'l' },
        39 => if shift { ':' } else { ';' },
        40 => if shift { '"' } else { '\'' },
        41 => if shift { '~' } else { '`' },
        43 => if shift { '|' } else { '\\' },
        44 => if alpha { 'Z' } else { 'z' },
        45 => if alpha { 'X' } else { 'x' },
        46 => if alpha { 'C' } else { 'c' },
        47 => if alpha { 'V' } else { 'v' },
        48 => if alpha { 'B' } else { 'b' },
        49 => if alpha { 'N' } else { 'n' },
        50 => if alpha { 'M' } else { 'm' },
        51 => if shift { '<' } else { ',' },
        52 => if shift { '>' } else { '.' },
        53 => if shift { '?' } else { '/' },
        57 => ' ',
        79 => '1',
        80 => '2',
        81 => '3',
        75 => '4',
        76 => '5',
        77 => '6',
        71 => '7',
        72 => '8',
        73 => '9',
        82 => '0',
        _ => return None,
    };
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evdev_ascii_handles_shift_and_caps() {
        assert_eq!(evdev_to_ascii(16, false, false), Some('q'));
        assert_eq!(evdev_to_ascii(16, true, false), Some('Q'));
        assert_eq!(evdev_to_ascii(16, false, true), Some('Q'));
        assert_eq!(evdev_to_ascii(16, true, true), Some('q'));
        assert_eq!(evdev_to_ascii(3, true, false), Some('@'));
        assert_eq!(evdev_to_ascii(12, true, false), Some('_'));
    }

    #[test]
    fn calendar_grid_computes_month_bounds_accurately() {
        // Sunday Nov 15, 2026 -> Nov 1, 2026 is Sunday (`first_wday = 0`), 30 days
        let g = compute_month_calendar(2026, 11, 15, 0);
        assert_eq!(g.first_wday, 0);
        assert_eq!(g.days_in_month, 30);
        assert_eq!(g.today, 15);

        // Leap year Feb 2028: Feb 29, 2028 is Tuesday (`wday = 2`) -> Feb 1, 2028 is Tuesday (`2`)
        let g2 = compute_month_calendar(2028, 2, 29, 2);
        assert_eq!(g2.first_wday, 2);
        assert_eq!(g2.days_in_month, 29);
    }

    #[test]
    fn physical_key_wakes_into_stage2_without_polluting_password_then_types() {
        let mut ls = Lockscreen::new_session_lock(None);
        ls.enter_lock(true); // Start in Standby Companion
        assert_eq!(ls.phase, LockPhase::StandbyCompanion);
        assert_eq!(ls.auth_reveal.target, 0.0);

        // First key ('a', `evdev_code = 30`) wakes from Standby into Stage 2 WITHOUT appending 'a'!
        assert!(ls.on_evdev_key_event(30, true));
        assert_eq!(ls.phase, LockPhase::AuthPrompt);
        assert_eq!(ls.auth_reveal.target, 1.0);
        assert!(!ls.touch_osk_open);
        assert!(!ls.osk.shown);
        assert_eq!(ls.pw_buf, "");

        // Subsequent key ('b', `evdev_code = 48`) now appends to password buffer!
        assert!(ls.on_evdev_key_event(48, true));
        assert_eq!(ls.pw_buf, "b");

        // Collapse back to Stage 1 Glance and verify wakeup key from Glance ALSO does not append!
        ls.pw_buf.clear();
        ls.close_auth_stage();
        assert_eq!(ls.auth_reveal.target, 0.0);
        assert!(ls.on_evdev_key_event(30, true));
        assert_eq!(ls.auth_reveal.target, 1.0);
        assert_eq!(ls.pw_buf, "");
    }

    #[test]
    fn touchpad_click_and_scroll_trigger_stage2_unlock_cleanly() {
        let mut ls = Lockscreen::new_session_lock(None);
        ls.enter_lock(true);

        // 1. Touchpad click (`slot = -1`) on Standby opens Stage 2 directly without touch OSK
        assert!(ls.on_touch_down(-1, 600.0, 400.0));
        assert!(ls.on_touch_up(-1, 600.0, 400.0));
        assert_eq!(ls.phase, LockPhase::AuthPrompt);
        assert_eq!(ls.auth_reveal.target, 1.0);
        assert!(!ls.touch_osk_open);

        // 2. Collapse to Stage 1 Glance; touchpad click anywhere on Glance opens Stage 2
        ls.close_auth_stage();
        ls.auth_reveal.snap(0.0);
        assert!(ls.on_touch_down(-1, 300.0, 300.0));
        assert!(ls.on_touch_up(-1, 300.0, 300.0));
        assert_eq!(ls.auth_reveal.target, 1.0);

        // 3. Touchpad 2-finger upward scroll (`dy > 0`) from Stage 1 Glance opens Stage 2
        ls.close_auth_stage();
        ls.auth_reveal.snap(0.0);
        assert!(ls.on_scroll(0.0, 45.0));
        assert!(ls.on_scroll(0.0, 45.0));
        assert!(ls.on_scroll_stop());
        assert_eq!(ls.auth_reveal.target, 1.0);
    }
}
