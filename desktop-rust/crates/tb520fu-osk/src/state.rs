// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tb520fu_ui::canvas::RectF;
use tb520fu_ui::color::{AccentColor, Palette, Skin};
use tb520fu_ui::haptics::{HapticEngine, HapticKind};
use tb520fu_ui::pinyin::lookup_pinyin_candidates;
use tb520fu_ui::spring::Spring1D;
use unicode_segmentation::UnicodeSegmentation;

use crate::fcitx::{lookup_evdev_code, FcitxCmd};
use crate::layouts::{
    alternates_for, build_keys, hint_for, KbLayout, KbMetrics, KbMode, KeyDef, KeyKind, Lang,
    EMOJI_CATS, NUM_LIST, SYM_CATS,
};
use crate::sound::{ClickKind, SoundEngine, SoundStyle};

fn default_haptic_level() -> u8 {
    3
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OskPrefs {
    pub skin: Skin,
    #[serde(default)]
    pub accent: AccentColor,
    #[serde(default = "default_true")]
    pub auto_popup: bool,
    pub mode: KbMode,
    pub hadj: f32,
    pub vol: u8,
    #[serde(default = "default_haptic_level")]
    pub haptic_level: u8,
    pub sound: String,
    pub sound_on: bool,
    pub bubble: bool,
    pub flick: bool,
    pub cursor_swipe: bool,
    pub tap_glow: bool,
    pub sym_lock: bool,
    pub float_x: i32,
    pub float_y: i32,
    pub pinned: Vec<String>,
    pub recent_emoji: Vec<String>,
}

impl Default for OskPrefs {
    fn default() -> Self {
        Self {
            skin: Skin::GlassDark,
            accent: AccentColor::Blue,
            auto_popup: true,
            mode: KbMode::Dock,
            hadj: 1.0,
            vol: 5,
            haptic_level: 3,
            sound: "std".into(),
            sound_on: true,
            bubble: true,
            flick: true,
            cursor_swipe: true,
            tap_glow: true,
            sym_lock: false,
            float_x: 560,
            float_y: 360,
            pinned: Vec::new(),
            recent_emoji: Vec::new(),
        }
    }
}

impl OskPrefs {
    pub fn config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".config/tb520fu/osk.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    None,
    Toolbox,
    Feedback,
    Clipboard,
    TextEdit,
    Emoji,
    Symbols,
    Candidates,
    LayoutPicker,
}

#[derive(Debug, Clone)]
pub struct BubbleInfo {
    pub rect: RectF,
    pub main: String,
    pub sub: String,
}

#[derive(Debug, Clone)]
pub struct AltsPopup {
    pub key_rect: RectF,
    pub items: Vec<String>,
    pub sel: usize,
}

#[derive(Debug, Clone)]
pub enum OutgoingAction {
    Fcitx(FcitxCmd),
    /// Direct text insertion for in-process Shell search bar / Quick Note pad or offline Pinyin fallback!
    DirectText(String),
    DirectBackspace,
    DirectEnter,
    DirectClearAll,
    /// Inject a raw Wayland virtual-keyboard key + XKB modifier mask (`1=Shift`, `4=Ctrl`, `8=Alt`).
    VkCombo { evdev_code: u32, mods: u32 },
    /// Reconfigure layer-shell geometry (mode switch, float drag, height adjust).
    ReconfigureSurface,
    /// Hide Wayland surface once slide-out spring finishes.
    UnmapSurface,
}

#[derive(Debug, Clone)]
struct KeyTouch {
    key_idx: usize,
    x0: f32,
    y0: f32,
    px: f32,
    py: f32,
    t0: Instant,
    done: bool,
    flick: bool,
    alts_open: bool,
    cursor_mode: bool,
    cursor_last_x: f32,
    del_repeating: bool,
    del_last_rep: Instant,
    del_rep_count: u32,
    del_clear_hot: bool,
    moved_far: bool,
    num_list_idx: Option<usize>,
    num_list_scroll_y0: f32,
    num_list_scrolling: bool,
}

#[derive(Debug, Clone)]
enum ActiveGesture {
    Key(KeyTouch),
    BandPull {
        x0: f32,
        y0: f32,
        cand_scroll_x0: f32,
        mode: BandGestureMode,
        tapped_cand: Option<usize>,
        tapped_tool: Option<&'static str>,
    },
    FloatDrag {
        start_sx: f32,
        start_sy: f32,
    },
    EditTrackpad {
        lx: f32,
        ly: f32,
        t0: Instant,
        moved: bool,
        drag_select: bool,
    },
    FeedbackSlider {
        track: RectF,
    },
    PanelScroll {
        y0: f32,
        scroll0: f32,
        moved: bool,
        tap_pos: (f32, f32),
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BandGestureMode {
    Undecided,
    PullHide,
    ScrollCands,
}

pub struct OskState {
    pub prefs: OskPrefs,
    pub pal: Palette,
    pub metrics: KbMetrics,
    pub keys: Vec<KeyDef>,
    pub lang: Lang,
    /// 0 = off, 1 = once, 2 = caps lock
    pub shift: u8,
    pub shift_held: u8,
    shift_used: bool,
    shift_was_once: bool,
    last_shift_tap: Option<Instant>,

    pub layout: KbLayout,
    pub panel: PanelKind,
    pub preedit: String,
    pub candidates: Vec<String>,
    pub cand_rects: Vec<(usize, RectF)>,
    pub cand_scroll_x: f32,
    pub cand_content_w: f32,
    pub has_prev: bool,
    pub has_next: bool,
    pub cand_cursor: i32,

    pub sym_cat: usize,
    pub emo_cat: usize,
    pub clip_cat: usize,
    pub clipboard: Vec<String>,
    pub clip_clear_armed_until: Option<Instant>,
    pub panel_scroll_y: f32,
    pub num_list_scroll_y: f32,
    pub num_list_hot: Option<usize>,
    pub edit_selecting: bool,
    pub edit_pad_dot: Option<(f32, f32)>,

    pub shown: bool,
    pub forced_open: bool,
    pub slide_y: Spring1D,
    pub pull_y: f32,
    pub screen_w: u32,
    pub screen_h: u32,

    pub bubbles: HashMap<i32, BubbleInfo>,
    pub alts: Option<AltsPopup>,
    pub clear_pill: Option<(RectF, bool)>,
    pub cursor_mode: bool,
    pub glow: Option<(usize, f32, f32, Instant)>,
    pub toast: Option<(String, Instant)>,

    /// When `true` (e.g. Shell App Menu search bar or Quick Note pad is focused), uses built-in offline Pinyin
    /// and emits `DirectText` / `DirectBackspace` / `DirectEnter` in 0ms without needing `fcitx5`!
    pub direct_text_sink: bool,
    /// When `true` (e.g. Lockscreen / Greeter password entry), locks language to English ASCII and hides clipboard/emoji.
    pub lock_english: bool,
    /// Optional override for the blue action Enter key label (e.g. `"打开"` in App Search, default `"发送"`).
    pub enter_label: Option<&'static str>,
    /// Whether live `fcitx5` D-Bus service is connected (`false` automatically activates offline Pinyin!).
    pub fcitx_online: bool,
    internal_pinyin: String,

    touch_pts: HashMap<i32, (f32, f32)>,
    pinch_start_dist: Option<f32>,
    gestures: HashMap<i32, ActiveGesture>,
    pub sound: SoundEngine,
    pub haptics: HapticEngine,
    pub outbox: Vec<OutgoingAction>,
}

impl OskState {
    pub fn new(screen_w: u32, screen_h: u32) -> Self {
        let prefs = OskPrefs::load();
        let pal = Palette::for_skin_and_accent(prefs.skin, prefs.accent);
        let metrics = KbMetrics::compute(prefs.mode, prefs.hadj, screen_w, screen_h);
        let keys = build_keys(prefs.mode, KbLayout::Main, Lang::Zh, &metrics);
        let vol = if prefs.sound_on { prefs.vol } else { 0 };
        let sound = SoundEngine::new(vol, SoundStyle::from_name(&prefs.sound));
        let haptics = HapticEngine::new(true);
        if prefs.haptic_level == 0 {
            haptics.set_enabled(false);
        } else {
            haptics.set_enabled(true);
            haptics.set_level(prefs.haptic_level);
        }
        let mut slide_y = Spring1D::with_params(metrics.kb_h + 40.0, 24.0, 1.0);
        slide_y.snap(metrics.kb_h + 40.0);

        Self {
            prefs,
            pal,
            metrics,
            keys,
            lang: Lang::Zh,
            shift: 0,
            shift_held: 0,
            shift_used: false,
            shift_was_once: false,
            last_shift_tap: None,
            layout: KbLayout::Main,
            panel: PanelKind::None,
            preedit: String::new(),
            candidates: Vec::new(),
            cand_rects: Vec::new(),
            cand_scroll_x: 0.0,
            cand_content_w: 0.0,
            has_prev: false,
            has_next: false,
            cand_cursor: 0,
            sym_cat: 0,
            emo_cat: 0,
            clip_cat: 0,
            clipboard: Vec::new(),
            clip_clear_armed_until: None,
            panel_scroll_y: 0.0,
            num_list_scroll_y: 0.0,
            num_list_hot: None,
            edit_selecting: false,
            edit_pad_dot: None,
            shown: false,
            forced_open: false,
            slide_y,
            pull_y: 0.0,
            screen_w,
            screen_h,
            bubbles: HashMap::new(),
            alts: None,
            clear_pill: None,
            cursor_mode: false,
            glow: None,
            toast: None,
            direct_text_sink: false,
            lock_english: false,
            enter_label: None,
            fcitx_online: true,
            internal_pinyin: String::new(),
            touch_pts: HashMap::new(),
            pinch_start_dist: None,
            gestures: HashMap::new(),
            sound,
            haptics,
            outbox: Vec::new(),
        }
    }

    pub fn composing(&self) -> bool {
        !self.preedit.is_empty() || !self.candidates.is_empty()
    }

    pub fn reload_prefs_from_disk(&mut self) {
        let fresh = OskPrefs::load();
        let mode_changed = fresh.mode != self.prefs.mode || (fresh.hadj - self.prefs.hadj).abs() > 0.001;
        self.prefs = fresh;
        self.pal = Palette::for_skin_and_accent(self.prefs.skin, self.prefs.accent);
        let vol = if self.prefs.sound_on { self.prefs.vol } else { 0 };
        self.sound.set_volume(vol);
        self.sound.set_style(SoundStyle::from_name(&self.prefs.sound));
        if mode_changed {
            self.rebuild_geometry();
            self.outbox.push(OutgoingAction::ReconfigureSurface);
        }
    }

    /// Configure this in-process OSK instance for Lockscreen / Greeter password entry:
    /// locked to English ASCII (`Lang::En`), no Pinyin preedit, no clipboard/emoji panels, docked/split only.
    pub fn configure_password_mode(&mut self, enter_label: &'static str) {
        self.lock_english = true;
        self.direct_text_sink = true;
        self.fcitx_online = false;
        self.enter_label = Some(enter_label);
        if self.prefs.mode == KbMode::Float {
            self.prefs.mode = KbMode::Dock;
        }
        self.lang = Lang::En;
        self.layout = KbLayout::Main;
        self.panel = PanelKind::None;
        self.shift = 0;
        self.shift_held = 0;
        self.internal_pinyin.clear();
        self.preedit.clear();
        self.candidates.clear();
        self.rebuild_geometry();
        self.outbox.clear();
    }

    pub fn split_glass_active(&self) -> bool {
        self.prefs.mode == KbMode::Split && self.layout == KbLayout::Main && self.panel == PanelKind::None
    }

    pub fn current_slide_offset(&self) -> f32 {
        self.slide_y.pos + self.pull_y
    }

    pub fn rebuild_geometry(&mut self) {
        self.metrics = KbMetrics::compute(self.prefs.mode, self.prefs.hadj, self.screen_w, self.screen_h);
        self.keys = build_keys(self.prefs.mode, self.layout, self.lang, &self.metrics);
        self.gestures.clear();
        self.bubbles.clear();
        self.alts = None;
        self.clear_pill = None;
        self.cursor_mode = false;
    }

    pub fn set_screen_size(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 && (self.screen_w != w || self.screen_h != h) {
            self.screen_w = w;
            self.screen_h = h;
            self.rebuild_geometry();
            if !self.shown {
                self.slide_y.snap(self.metrics.kb_h + 40.0);
            }
        }
    }

    pub fn show(&mut self, forced: bool) {
        if forced {
            self.forced_open = true;
        }
        self.pull_y = 0.0;
        if !self.shown {
            self.shown = true;
            self.slide_y.set_target(0.0);
            self.outbox.push(OutgoingAction::Fcitx(FcitxCmd::EnterOnScreenMode));
            self.outbox.push(OutgoingAction::ReconfigureSurface);
        } else {
            self.slide_y.set_target(0.0);
        }
    }

    pub fn hide(&mut self, notify_fcitx: bool) {
        if !self.shown {
            return;
        }
        if notify_fcitx {
            if self.composing() {
                self.outbox.push(OutgoingAction::Fcitx(FcitxCmd::ClearComposition));
            }
            self.outbox.push(OutgoingAction::Fcitx(FcitxCmd::RequestHide));
        }
        self.shown = false;
        self.forced_open = false;
        self.panel = PanelKind::None;
        self.gestures.clear();
        self.bubbles.clear();
        self.alts = None;
        self.clear_pill = None;
        self.cursor_mode = false;
        self.pull_y = 0.0;
        self.slide_y.set_target(self.metrics.kb_h + 40.0);
        // Immediately drop `exclusive_zone` to `0` while still mapped so Niri restores the
        // bottom working_area right away when dismissing via the OSK collapse button or pull-down!
        self.outbox.push(OutgoingAction::ReconfigureSurface);
    }

    pub fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now() + Duration::from_millis(1250)));
    }

    pub fn push_clipboard(&mut self, item: String) {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            return;
        }
        self.clipboard.retain(|x| x != &item);
        self.clipboard.insert(0, item);
        if self.clipboard.len() > 100 {
            self.clipboard.truncate(100);
        }
    }

    /// Periodic tick (144Hz while animating or holding a key). Returns `(needs_redraw, needs_next_frame)`.
    pub fn tick(&mut self, dt: f32) -> (bool, bool) {
        let mut redraw = false;
        let was_anim = self.slide_y.is_animating();
        let still_anim = self.slide_y.step(dt);
        if was_anim || still_anim {
            redraw = true;
            if !self.shown && !still_anim {
                self.outbox.push(OutgoingAction::UnmapSurface);
            }
        }

        let now = Instant::now();
        if let Some((_, exp)) = &self.toast {
            if now >= *exp {
                self.toast = None;
                redraw = true;
            }
        }
        if let Some((_, _, _, t0)) = self.glow {
            if now.duration_since(t0) > Duration::from_millis(180) {
                self.glow = None;
                redraw = true;
            } else {
                redraw = true;
            }
        }

        // Check long-press timers (400ms alternates, 400ms delete repeat, 450ms edit trackpad select)
        let u = self.metrics.u;
        let mut pending_cmds: Vec<(usize, bool)> = Vec::new(); // (type, clear)
        let mut open_alts_for: Option<(usize, f32)> = None;
        let mut trigger_toast: Option<&'static str> = None;

        for g in self.gestures.values_mut() {
            match g {
                ActiveGesture::Key(kt) => {
                    let Some(kd) = self.keys.get(kt.key_idx) else { continue };
                    let elapsed_ms = now.duration_since(kt.t0).as_millis();
                    if kd.kind == KeyKind::Char && !kt.done && !kt.moved_far && !kt.flick && !kt.alts_open && elapsed_ms >= 400 {
                        kt.alts_open = true;
                        open_alts_for = Some((kt.key_idx, kt.px));
                        redraw = true;
                    } else if kd.kind == KeyKind::Del && !kt.del_repeating && elapsed_ms >= 400 {
                        kt.del_repeating = true;
                        kt.del_last_rep = now;
                        kt.del_rep_count = 1;
                        pending_cmds.push((1, false));
                        redraw = true;
                    } else if kd.kind == KeyKind::Del && kt.del_repeating && !kt.del_clear_hot {
                        let interval_ms = if kt.del_rep_count > 10 { 38 } else { 72 };
                        if now.duration_since(kt.del_last_rep).as_millis() >= interval_ms {
                            kt.del_last_rep = now;
                            kt.del_rep_count += 1;
                            pending_cmds.push((1, false));
                        }
                    }
                }
                ActiveGesture::EditTrackpad {
                    t0,
                    moved,
                    drag_select,
                    ..
                } => {
                    if !*moved && !*drag_select && now.duration_since(*t0).as_millis() >= 450 {
                        *drag_select = true;
                        trigger_toast = Some("拖动选取文本");
                        redraw = true;
                    }
                }
                _ => {}
            }
        }

        if let Some((kidx, px)) = open_alts_for {
            self.bubbles.clear();
            let kd = &self.keys[kidx];
            let items = alternates_for(kd.k, self.lang.is_cjk());
            if !items.is_empty() {
                self.haptics.pulse_at_x(self.screen_x_for(px), self.screen_w as f32, HapticKind::Heavy);
                let mut popup = AltsPopup {
                    key_rect: kd.rect,
                    items,
                    sel: 0,
                };
                Self::update_alts_sel(&mut popup, px, u, self.metrics.kb_w);
                self.alts = Some(popup);
            }
        }
        for (kind, _) in pending_cmds {
            if kind == 1 {
                self.haptics.pulse_at_x(self.screen_w as f32 * 0.90, self.screen_w as f32, HapticKind::Repeat);
                self.do_delete();
                let del_rect = self
                    .keys
                    .iter()
                    .find(|k| k.kind == KeyKind::Del)
                    .map(|k| k.rect)
                    .unwrap_or_default();
                let pw = 210.0 * u;
                let ph = 54.0 * u;
                let px = (del_rect.x + del_rect.w - pw).clamp(8.0 * u, self.metrics.kb_w - pw - 8.0 * u);
                let py = (del_rect.y - ph - 10.0 * u).max(4.0 * u);
                if self.clear_pill.is_none() {
                    self.clear_pill = Some((RectF::new(px, py, pw, ph), false));
                }
            }
        }
        if let Some(msg) = trigger_toast {
            self.show_toast(msg);
        }

        let any_hold = self.gestures.values().any(|g| {
            matches!(
                g,
                ActiveGesture::Key(_) | ActiveGesture::EditTrackpad { .. }
            )
        });
        let needs_frame = still_anim || any_hold || self.glow.is_some() || self.toast.is_some();
        (redraw, needs_frame)
    }

    pub fn is_animating(&self) -> bool {
        let any_hold = self.gestures.values().any(|g| {
            matches!(
                g,
                ActiveGesture::Key(_) | ActiveGesture::EditTrackpad { .. }
            )
        });
        self.slide_y.is_animating() || any_hold || self.glow.is_some() || self.toast.is_some()
    }

    pub fn is_key_down(&self, idx: usize) -> bool {
        self.gestures.values().any(|g| match g {
            ActiveGesture::Key(kt) => kt.key_idx == idx && !kt.done,
            _ => false,
        })
    }

    pub fn cap_of(&self, k: &str) -> String {
        if !self.lang.is_cjk() && (self.shift > 0 || self.shift_held > 0) {
            k.to_ascii_uppercase()
        } else {
            k.to_string()
        }
    }

    /// Keycap glyph shown on the keyboard face: uppercase in Chinese/Japanese mode (`Q W E R T`)
    /// and whenever Shift is active, matching Y700 ZUI OSK (`index.html` `up = KS.lang === 'zh' || KS.shift > 0`).
    pub fn display_cap_of(&self, k: &str) -> String {
        if self.lang.is_cjk() || self.shift > 0 || self.shift_held > 0 {
            k.to_ascii_uppercase()
        } else {
            k.to_string()
        }
    }

    fn screen_x_for(&self, bx: f32) -> f32 {
        if self.prefs.mode == KbMode::Float {
            self.prefs.float_x as f32 + self.metrics.fpad + bx
        } else {
            self.metrics.fpad + bx
        }
    }

    fn use_internal_ime(&self) -> bool {
        self.direct_text_sink || !self.fcitx_online
    }

    fn refresh_internal_pinyin(&mut self) {
        if self.internal_pinyin.is_empty() {
            self.preedit.clear();
            self.candidates.clear();
            self.cand_cursor = 0;
            self.cand_scroll_x = 0.0;
            if self.panel == PanelKind::Candidates {
                self.panel = PanelKind::None;
            }
        } else {
            let (pre, cands) = lookup_pinyin_candidates(&self.internal_pinyin);
            self.preedit = pre;
            self.candidates = cands;
            self.cand_cursor = 0;
            self.cand_scroll_x = 0.0;
        }
    }

    fn emit_committed_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.direct_text_sink {
            self.outbox.push(OutgoingAction::DirectText(s.to_string()));
        } else {
            self.outbox
                .push(OutgoingAction::Fcitx(FcitxCmd::CommitString(s.to_string())));
        }
    }

    pub fn select_candidate_idx(&mut self, idx: usize) {
        if self.use_internal_ime() && !self.internal_pinyin.is_empty() {
            if let Some(chosen) = self.candidates.get(idx).cloned() {
                self.internal_pinyin.clear();
                self.refresh_internal_pinyin();
                self.panel = PanelKind::None;
                self.emit_committed_str(&chosen);
            }
        } else {
            self.outbox
                .push(OutgoingAction::Fcitx(FcitxCmd::SelectCandidate(idx as i32)));
        }
    }

    // ---------------- Touch Input Dispatch ----------------

    pub fn on_touch_down(&mut self, slot: i32, surf_x: f32, surf_y: f32) -> bool {
        let bx = surf_x - self.metrics.fpad;
        let by = surf_y - self.metrics.fpad - self.current_slide_offset();
        let u = self.metrics.u;

        if bx < 0.0 || bx > self.metrics.kb_w || by < 0.0 || by > self.metrics.kb_h {
            return false;
        }

        // In Split mode, the transparent center gap (`half_w .. kb_w - half_w` below `band_h`)
        // must pass touches through cleanly without capturing the slot!
        if by >= self.metrics.band_h && self.split_glass_active() {
            let half_w = (self.metrics.kb_w * self.metrics.split_half_frac).ceil();
            if bx > half_w && bx < self.metrics.kb_w - half_w {
                return false;
            }
        }

        self.touch_pts.insert(slot, (surf_x, surf_y));
        if self.touch_pts.len() == 2 {
            let mut it = self.touch_pts.values();
            if let (Some(&p0), Some(&p1)) = (it.next(), it.next()) {
                let d0 = (p0.0 - p1.0).hypot(p0.1 - p1.1);
                if d0 > 70.0 * u {
                    self.pinch_start_dist = Some(d0);
                }
            }
        }

        // Floating mode bottom drag bar (`kb_h - bot_h .. kb_h`)
        if self.prefs.mode == KbMode::Float && by >= self.metrics.kb_h - self.metrics.bot_h - 6.0 * u {
            self.gestures.insert(
                slot,
                ActiveGesture::FloatDrag {
                    start_sx: surf_x,
                    start_sy: surf_y,
                },
            );
            return true;
        }

        // Sub-panel active?
        if self.panel != PanelKind::None {
            let _ = self.on_panel_touch_down(slot, bx, by);
            return true;
        }

        // Top band (`0 .. band_h`)
        if by < self.metrics.band_h {
            let _ = self.on_band_touch_down(slot, bx, by);
            return true;
        }

        // Key grid (`band_h .. band_h + key_h`): exact hit first, else snap within `7.0 * u` key gutter,
        // and always swallow touches inside the keyboard's glass body so embedded OSK never leaks taps
        // into underlying Console scrim / widgets!
        if let Some(kidx) = self.keys.iter().position(|k| k.rect.contains(bx, by)) {
            self.begin_key_touch(slot, kidx, bx, by);
            return true;
        }
        let max_gutter_d2 = (7.0 * u) * (7.0 * u);
        let mut best: Option<(usize, f32)> = None;
        for (idx, k) in self.keys.iter().enumerate() {
            let cx = bx.clamp(k.rect.x, k.rect.x + k.rect.w);
            let cy = by.clamp(k.rect.y, k.rect.y + k.rect.h);
            let d2 = (bx - cx) * (bx - cx) + (by - cy) * (by - cy);
            if d2 <= max_gutter_d2 && best.is_none_or(|(_, bd2)| d2 < bd2) {
                best = Some((idx, d2));
            }
        }
        if let Some((kidx, _)) = best {
            self.begin_key_touch(slot, kidx, bx, by);
        }
        true
    }

    pub fn on_touch_motion(&mut self, slot: i32, surf_x: f32, surf_y: f32) -> bool {
        let bx = surf_x - self.metrics.fpad;
        let by = surf_y - self.metrics.fpad - self.slide_y.pos;
        let u = self.metrics.u;

        // Two-finger pinch-in (`Dock/Split -> Float`) & spread (`Float -> Dock`) gesture!
        if self.touch_pts.contains_key(&slot) {
            self.touch_pts.insert(slot, (surf_x, surf_y));
            if self.touch_pts.len() == 2 {
                if let Some(d0) = self.pinch_start_dist {
                    let mut it = self.touch_pts.values();
                    if let (Some(&p0), Some(&p1)) = (it.next(), it.next()) {
                        let d = (p0.0 - p1.0).hypot(p0.1 - p1.1);
                        let thresh = 92.0 * u;
                        if !self.lock_english && self.prefs.mode != KbMode::Float && d < d0 - thresh {
                            self.pinch_start_dist = None;
                            self.touch_pts.clear();
                            self.haptics.pulse_at_x(self.screen_w as f32 * 0.5, self.screen_w as f32, HapticKind::Heavy);
                            self.set_mode(KbMode::Float);
                            self.show_toast("已捏合切换悬浮键盘");
                            return true;
                        } else if self.prefs.mode == KbMode::Float && d > d0 + thresh {
                            self.pinch_start_dist = None;
                            self.touch_pts.clear();
                            self.haptics.pulse_at_x(self.screen_w as f32 * 0.5, self.screen_w as f32, HapticKind::Heavy);
                            self.set_mode(KbMode::Dock);
                            self.show_toast("已张开恢复停靠键盘");
                            return true;
                        }
                    }
                }
            }
        }

        let Some(mut g) = self.gestures.remove(&slot) else {
            return false;
        };
        let mut redraw = false;
        let reinsert = true;

        match &mut g {
            ActiveGesture::FloatDrag { start_sx, start_sy } => {
                let dx = (surf_x - *start_sx).round() as i32;
                let dy = (surf_y - *start_sy).round() as i32;
                if dx.abs() >= 2 || dy.abs() >= 2 {
                    self.prefs.float_x = (self.prefs.float_x + dx)
                        .clamp(0, (self.screen_w as i32 - self.metrics.surf_w as i32).max(0));
                    self.prefs.float_y = (self.prefs.float_y + dy)
                        .clamp(0, (self.screen_h as i32 - self.metrics.surf_h as i32).max(0));
                    self.outbox.push(OutgoingAction::ReconfigureSurface);
                }
            }
            ActiveGesture::BandPull {
                x0,
                y0,
                cand_scroll_x0,
                mode,
                tapped_cand,
                tapped_tool,
            } => {
                let dx = bx - *x0;
                let dy = by - *y0;
                if *mode == BandGestureMode::Undecided && dx.hypot(dy) > 10.0 * u {
                    *tapped_cand = None;
                    *tapped_tool = None;
                    if self.composing() && dx.abs() > dy.abs() {
                        *mode = BandGestureMode::ScrollCands;
                    } else if dy > 0.0 && self.prefs.mode != KbMode::Float {
                        *mode = BandGestureMode::PullHide;
                    }
                }
                match *mode {
                    BandGestureMode::ScrollCands => {
                        let max_scroll = (self.cand_content_w - (self.metrics.kb_w - 72.0 * u)).max(0.0);
                        self.cand_scroll_x = (*cand_scroll_x0 - dx).clamp(0.0, max_scroll);
                        redraw = true;
                    }
                    BandGestureMode::PullHide => {
                        if dy >= 70.0 * u {
                            self.pull_y = 0.0;
                            self.hide(true);
                            return true;
                        }
                        self.pull_y = (dy.max(0.0)).min(140.0 * u) * 0.6;
                        redraw = true;
                    }
                    BandGestureMode::Undecided => {}
                }
            }
            ActiveGesture::EditTrackpad {
                lx,
                ly,
                moved,
                drag_select,
                ..
            } => {
                self.edit_pad_dot = Some((bx, by));
                let step = 18.0 * u;
                if (bx - *lx).abs() > 6.0 * u || (by - *ly).abs() > 6.0 * u {
                    *moved = true;
                }
                let sel_mods = if *drag_select || self.edit_selecting { 1u32 } else { 0u32 };
                while (bx - *lx).abs() >= step {
                    let right = bx > *lx;
                    if let Some(ev) = lookup_evdev_code(if right { "Right" } else { "Left" }) {
                        self.outbox.push(OutgoingAction::VkCombo {
                            evdev_code: ev,
                            mods: sel_mods,
                        });
                    }
                    *lx += if right { step } else { -step };
                }
                while (by - *ly).abs() >= step * 1.8 {
                    let down = by > *ly;
                    if let Some(ev) = lookup_evdev_code(if down { "Down" } else { "Up" }) {
                        self.outbox.push(OutgoingAction::VkCombo {
                            evdev_code: ev,
                            mods: sel_mods,
                        });
                    }
                    *ly += if down { step * 1.8 } else { -step * 1.8 };
                }
                redraw = true;
            }
            ActiveGesture::FeedbackSlider { track } => {
                let frac = ((bx - track.x) / track.w.max(1.0)).clamp(0.0, 1.0);
                let new_v = (frac * 10.0).round() as u8;
                if new_v != self.prefs.vol {
                    self.prefs.vol = new_v;
                    self.prefs.sound_on = new_v > 0;
                    self.sound.set_volume(new_v);
                    self.sound.play(ClickKind::Char);
                    if !self.lock_english {
                        self.prefs.save();
                    }
                    redraw = true;
                }
            }
            ActiveGesture::PanelScroll {
                y0,
                scroll0,
                moved,
                ..
            } => {
                let dy = by - *y0;
                if dy.abs() > 10.0 * u {
                    *moved = true;
                }
                if *moved {
                    self.panel_scroll_y = (*scroll0 - dy).max(0.0);
                    redraw = true;
                }
            }
            ActiveGesture::Key(kt) => {
                let jump_dist = 48.0 * u;
                let step_jump = (bx - kt.px).hypot(by - kt.py);
                if !kt.done && !kt.alts_open && !kt.del_repeating && !kt.cursor_mode && step_jump >= jump_dist {
                    if let Some(new_idx) = self.keys.iter().position(|k| k.rect.contains(bx, by)) {
                        if new_idx != kt.key_idx {
                            let mut finished = kt.clone();
                            finished.flick = false;
                            self.finish_key_touch(slot, &finished);
                            self.begin_key_touch(slot, new_idx, bx, by);
                            return true;
                        }
                    }
                }

                let dx = bx - kt.x0;
                let dy = by - kt.y0;
                kt.px = bx;
                kt.py = by;
                if !kt.moved_far && dx.hypot(dy) > 16.0 * u {
                    kt.moved_far = true;
                }
                let kind = self.keys[kt.key_idx].kind;
                let key_k = self.keys[kt.key_idx].k;
                let key_rect = self.keys[kt.key_idx].rect;

                match kind {
                    KeyKind::Char if !kt.done => {
                        if kt.alts_open {
                            if let Some(popup) = &mut self.alts {
                                Self::update_alts_sel(popup, bx, u, self.metrics.kb_w);
                            }
                            redraw = true;
                        } else if self.prefs.flick {
                            if let Some(hint) = hint_for(key_k, self.lang.is_cjk()) {
                                let flick_dy = 24.0 * u;
                                let half_w = key_rect.w * 0.65;
                                if !kt.flick && dy <= -flick_dy && dx.abs() <= half_w {
                                    kt.flick = true;
                                    let sub = self.display_cap_of(key_k);
                                    self.set_bubble(slot, key_rect, hint.to_string(), sub);
                                    redraw = true;
                                } else if kt.flick && (dy > -flick_dy * 0.55 || dx.abs() > half_w) {
                                    kt.flick = false;
                                    let main = self.display_cap_of(key_k);
                                    self.set_bubble(slot, key_rect, main, String::new());
                                    redraw = true;
                                }
                            }
                        }
                    }
                    KeyKind::Space => {
                        if kt.cursor_mode {
                            let step = 14.0 * u;
                            while (bx - kt.cursor_last_x).abs() >= step {
                                let right = bx > kt.cursor_last_x;
                                self.haptics.pulse_at_x(self.screen_x_for(bx), self.screen_w as f32, HapticKind::Repeat);
                                if let Some(ev) = lookup_evdev_code(if right { "Right" } else { "Left" }) {
                                    self.outbox.push(OutgoingAction::VkCombo { evdev_code: ev, mods: 0 });
                                }
                                kt.cursor_last_x += if right { step } else { -step };
                            }
                        } else if self.prefs.cursor_swipe && dx.abs() >= 26.0 * u {
                            kt.cursor_mode = true;
                            kt.cursor_last_x = bx;
                            self.cursor_mode = true;
                            self.haptics.pulse_at_x(self.screen_x_for(bx), self.screen_w as f32, HapticKind::Tool);
                            redraw = true;
                        }
                    }
                    KeyKind::Del => {
                        if dy <= -24.0 * u && !kt.del_repeating {
                            kt.del_repeating = true;
                            kt.del_last_rep = Instant::now();
                            let pw = 210.0 * u;
                            let ph = 54.0 * u;
                            let px = (key_rect.x + key_rect.w - pw).clamp(8.0 * u, self.metrics.kb_w - pw - 8.0 * u);
                            let py = (key_rect.y - ph - 10.0 * u).max(4.0 * u);
                            self.clear_pill = Some((RectF::new(px, py, pw, ph), false));
                            redraw = true;
                        }
                        if let Some((pill_r, ref mut hot)) = self.clear_pill {
                            let now_hot = pill_r.inset(-10.0 * u, -10.0 * u).contains(bx, by);
                            if now_hot != *hot {
                                *hot = now_hot;
                                kt.del_clear_hot = now_hot;
                                redraw = true;
                            }
                        }
                    }
                    KeyKind::List => {
                        if !kt.num_list_scrolling && dy.abs() > 8.0 * u {
                            kt.num_list_scrolling = true;
                            kt.num_list_idx = None;
                            self.num_list_hot = None;
                        }
                        if kt.num_list_scrolling {
                            let item_h = 52.0 * u * self.metrics.fs;
                            let max_s = ((NUM_LIST.len() as f32) * item_h - key_rect.h + 12.0 * u).max(0.0);
                            self.num_list_scroll_y = (kt.num_list_scroll_y0 - dy).clamp(0.0, max_s);
                            redraw = true;
                        }
                    }
                    _ => {}
                }
            }
        }

        if reinsert {
            self.gestures.insert(slot, g);
        }
        redraw
    }

    pub fn on_touch_up(&mut self, slot: i32) -> bool {
        self.touch_pts.remove(&slot);
        if self.touch_pts.len() < 2 {
            self.pinch_start_dist = None;
        }
        let Some(g) = self.gestures.remove(&slot) else {
            return false;
        };
        match g {
            ActiveGesture::FloatDrag { .. } => {
                self.prefs.save();
                true
            }
            ActiveGesture::BandPull {
                mode,
                tapped_cand,
                tapped_tool,
                x0,
                ..
            } => {
                self.pull_y = 0.0;
                if mode == BandGestureMode::Undecided {
                    if let Some(idx) = tapped_cand {
                        self.sound.play(ClickKind::Char);
                        self.haptics.pulse_at_x(self.screen_x_for(x0), self.screen_w as f32, HapticKind::Char);
                        self.select_candidate_idx(idx);
                    } else if let Some(tool) = tapped_tool {
                        self.haptics.pulse_at_x(self.screen_x_for(x0), self.screen_w as f32, HapticKind::Tool);
                        self.activate_toolbar_btn(tool);
                    }
                }
                true
            }
            ActiveGesture::EditTrackpad { .. } => {
                self.edit_pad_dot = None;
                true
            }
            ActiveGesture::FeedbackSlider { .. } => true,
            ActiveGesture::PanelScroll {
                moved,
                tap_pos: (tx, ty),
                ..
            } => {
                if !moved {
                    self.handle_panel_tap(tx, ty);
                }
                true
            }
            ActiveGesture::Key(kt) => {
                self.finish_key_touch(slot, &kt);
                true
            }
        }
    }

    pub fn on_touch_cancel(&mut self) -> bool {
        self.touch_pts.clear();
        self.pinch_start_dist = None;
        self.gestures.clear();
        self.bubbles.clear();
        self.alts = None;
        self.clear_pill = None;
        self.cursor_mode = false;
        self.pull_y = 0.0;
        self.shift_held = 0;
        true
    }

    // ---------------- Key Press / Release Helpers ----------------

    fn begin_key_touch(&mut self, slot: i32, kidx: usize, bx: f32, by: f32) {
        let kind = self.keys[kidx].kind;
        let key_k = self.keys[kidx].k;
        let key_rect = self.keys[kidx].rect;

        // Two-thumb rollover: commit any pending earlier letter immediately
        self.commit_pending_letters(kind == KeyKind::Shift);

        let (click, hap) = match kind {
            KeyKind::Space => (ClickKind::Space, HapticKind::Space),
            KeyKind::Del => (ClickKind::Del, HapticKind::Del),
            KeyKind::Enter => (ClickKind::Enter, HapticKind::Enter),
            _ => (ClickKind::Char, HapticKind::Char),
        };
        self.sound.play(click);
        self.haptics.pulse_at_x(self.screen_x_for(bx), self.screen_w as f32, hap);

        if self.prefs.tap_glow {
            self.glow = Some((kidx, bx, by, Instant::now()));
        }

        let mut num_list_idx = None;
        match kind {
            KeyKind::Char => {
                if self.prefs.bubble {
                    let main = self.display_cap_of(key_k);
                    self.set_bubble(slot, key_rect, main, String::new());
                }
            }
            KeyKind::Del => {
                self.do_delete();
            }
            KeyKind::Shift => {
                self.shift_held = self.shift_held.saturating_add(1);
                self.shift_was_once = self.shift == 1;
                self.shift_used = false;
            }
            KeyKind::List => {
                let u = self.metrics.u;
                let item_h = 52.0 * u * self.metrics.fs;
                let rel_y = by - key_rect.y - 6.0 * u + self.num_list_scroll_y;
                let idx = (rel_y / item_h).floor() as isize;
                if idx >= 0 && (idx as usize) < NUM_LIST.len() {
                    num_list_idx = Some(idx as usize);
                    self.num_list_hot = num_list_idx;
                }
            }
            _ => {}
        }

        self.gestures.insert(
            slot,
            ActiveGesture::Key(KeyTouch {
                key_idx: kidx,
                x0: bx,
                y0: by,
                px: bx,
                py: by,
                t0: Instant::now(),
                done: false,
                flick: false,
                alts_open: false,
                cursor_mode: false,
                cursor_last_x: bx,
                del_repeating: false,
                del_last_rep: Instant::now(),
                del_rep_count: 0,
                del_clear_hot: false,
                moved_far: false,
                num_list_idx,
                num_list_scroll_y0: self.num_list_scroll_y,
                num_list_scrolling: false,
            }),
        );
    }

    fn commit_pending_letters(&mut self, incoming_is_shift: bool) {
        let now = Instant::now();
        let mut to_commit: Vec<(i32, usize, bool)> = Vec::new();
        for (&s_id, g) in &mut self.gestures {
            if let ActiveGesture::Key(kt) = g {
                if kt.done || kt.alts_open {
                    continue;
                }
                let kd = &self.keys[kt.key_idx];
                if kd.kind != KeyKind::Char {
                    continue;
                }
                if incoming_is_shift && self.shift == 0 && now.duration_since(kt.t0).as_millis() <= 100 {
                    continue;
                }
                kt.done = true;
                to_commit.push((s_id, kt.key_idx, kt.flick));
            }
        }
        for (s_id, kidx, flick) in to_commit {
            self.bubbles.remove(&s_id);
            let k = self.keys[kidx].k;
            if flick {
                if let Some(h) = hint_for(k, self.lang.is_cjk()) {
                    self.type_text(h);
                    continue;
                }
            }
            self.type_letter(k);
        }
    }

    fn finish_key_touch(&mut self, slot: i32, kt: &KeyTouch) {
        self.bubbles.remove(&slot);
        let Some(kd) = self.keys.get(kt.key_idx).cloned() else {
            return;
        };
        match kd.kind {
            KeyKind::Char => {
                if kt.alts_open {
                    if let Some(popup) = self.alts.take() {
                        if let Some(picked) = popup.items.get(popup.sel) {
                            self.type_alternate(picked);
                        }
                    }
                } else if !kt.done {
                    if kt.flick {
                        if let Some(h) = hint_for(kd.k, self.lang.is_cjk()) {
                            self.type_text(h);
                        } else {
                            self.type_letter(kd.k);
                        }
                    } else {
                        self.type_letter(kd.k);
                    }
                }
            }
            KeyKind::Punct | KeyKind::Digit => {
                self.type_text(kd.ch);
            }
            KeyKind::List => {
                self.num_list_hot = None;
                if !kt.num_list_scrolling {
                    if let Some(i) = kt.num_list_idx {
                        if let Some(&sym) = NUM_LIST.get(i) {
                            self.type_text(sym);
                        }
                    }
                }
            }
            KeyKind::Space => {
                if kt.cursor_mode {
                    self.cursor_mode = false;
                } else if self.use_internal_ime() {
                    if !self.internal_pinyin.is_empty() {
                        self.select_candidate_idx(0);
                    } else {
                        self.emit_committed_str(" ");
                    }
                } else {
                    self.outbox
                        .push(OutgoingAction::Fcitx(FcitxCmd::TypeKey("space".into())));
                }
            }
            KeyKind::Del => {
                if kt.del_clear_hot {
                    self.clear_all();
                }
                self.clear_pill = None;
            }
            KeyKind::Enter => {
                if self.use_internal_ime() {
                    if !self.internal_pinyin.is_empty() {
                        let raw = std::mem::take(&mut self.internal_pinyin);
                        self.refresh_internal_pinyin();
                        self.emit_committed_str(&raw);
                    } else if self.direct_text_sink {
                        self.outbox.push(OutgoingAction::DirectEnter);
                    } else {
                        self.outbox
                            .push(OutgoingAction::Fcitx(FcitxCmd::TypeKey("Return".into())));
                    }
                } else {
                    self.outbox
                        .push(OutgoingAction::Fcitx(FcitxCmd::TypeKey("Return".into())));
                }
            }
            KeyKind::Sep => {
                if self.use_internal_ime() && !self.internal_pinyin.is_empty() {
                    self.internal_pinyin.push('\'');
                    self.refresh_internal_pinyin();
                } else {
                    self.outbox
                        .push(OutgoingAction::Fcitx(FcitxCmd::TypeKey("apostrophe".into())));
                }
            }
            KeyKind::Shift => {
                self.commit_pending_letters(false);
                self.shift_held = self.shift_held.saturating_sub(1);
                let now = Instant::now();
                if self.shift_used {
                    if self.shift == 1 {
                        self.shift = 0;
                    }
                } else if self.shift == 2 {
                    self.shift = 0;
                } else if self
                    .last_shift_tap
                    .is_some_and(|t| now.duration_since(t).as_millis() <= 400)
                {
                    self.shift = 2;
                    self.last_shift_tap = None;
                } else if self.shift_was_once {
                    self.shift = 0;
                    self.last_shift_tap = Some(now);
                } else {
                    self.shift = 1;
                    self.last_shift_tap = Some(now);
                }
            }
            KeyKind::Lang => {
                let next = self.lang.next();
                self.set_lang(next);
            }
            KeyKind::Sym => {
                self.sym_cat = if self.lang.is_cjk() { 0 } else { 1 };
                self.open_panel(PanelKind::Symbols);
            }
            KeyKind::Num => {
                self.layout = KbLayout::Num;
                self.rebuild_geometry();
            }
            KeyKind::Back => {
                self.layout = KbLayout::Main;
                self.rebuild_geometry();
            }
        }
    }

    fn type_letter(&mut self, k: &str) {
        let ch = self.cap_of(k);
        if self.shift_held > 0 {
            self.shift_used = true;
        } else if self.shift == 1 {
            self.shift = 0;
        }
        if self.use_internal_ime() {
            if self.lang == Lang::Zh
                && ch.len() == 1
                && ch.as_bytes()[0].is_ascii_lowercase()
                && self.shift == 0
                && self.shift_held == 0
            {
                self.internal_pinyin.push_str(&ch);
                self.refresh_internal_pinyin();
                return;
            }
            if !self.internal_pinyin.is_empty() {
                self.select_candidate_idx(0);
            }
            self.emit_committed_str(&ch);
            return;
        }
        self.outbox.push(OutgoingAction::Fcitx(FcitxCmd::TypeKey(ch)));
    }

    fn type_alternate(&mut self, s: &str) {
        if !self.use_internal_ime() && s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() {
            self.outbox
                .push(OutgoingAction::Fcitx(FcitxCmd::TypeKey(s.to_string())));
        } else {
            self.type_text(s);
        }
    }

    pub fn type_text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.use_internal_ime() {
            if !self.internal_pinyin.is_empty() {
                self.select_candidate_idx(0);
            }
            self.emit_committed_str(s);
            return;
        }
        let mapped = match s {
            " " => Some("space"),
            "," => if !self.lang.is_cjk() { Some("comma") } else { None },
            "." => if !self.lang.is_cjk() { Some("period") } else { None },
            "/" => if !self.composing() && !self.lang.is_cjk() { Some("slash") } else { None },
            "-" => if !self.composing() && !self.lang.is_cjk() { Some("minus") } else { None },
            "@" => if !self.composing() && !self.lang.is_cjk() { Some("at") } else { None },
            _ if s.len() == 1 && s.as_bytes()[0].is_ascii_alphanumeric() && !self.composing() => Some(s),
            _ => None,
        };
        if let Some(key_name) = mapped {
            self.outbox
                .push(OutgoingAction::Fcitx(FcitxCmd::TypeKey(key_name.into())));
        } else {
            self.outbox
                .push(OutgoingAction::Fcitx(FcitxCmd::CommitString(s.to_string())));
        }
    }

    pub fn do_delete(&mut self) {
        if self.use_internal_ime() {
            if !self.internal_pinyin.is_empty() {
                self.internal_pinyin.pop();
                self.refresh_internal_pinyin();
                return;
            }
            if self.direct_text_sink {
                self.outbox.push(OutgoingAction::DirectBackspace);
                return;
            }
        }
        if self.composing() {
            self.outbox
                .push(OutgoingAction::Fcitx(FcitxCmd::TypeKey("BackSpace".into())));
        } else if let Some(ev) = lookup_evdev_code("BackSpace") {
            self.outbox.push(OutgoingAction::VkCombo {
                evdev_code: ev,
                mods: 0,
            });
        }
    }

    pub fn clear_all(&mut self) {
        if !self.internal_pinyin.is_empty() {
            self.internal_pinyin.clear();
            self.refresh_internal_pinyin();
            return;
        }
        if self.direct_text_sink {
            self.outbox.push(OutgoingAction::DirectClearAll);
            return;
        }
        if self.composing() {
            self.outbox.push(OutgoingAction::Fcitx(FcitxCmd::ClearComposition));
            return;
        }
        // Ctrl+A (`evdev 30`, `Ctrl = 4`) then BackSpace (`evdev 14`) via direct Wayland virtual keyboard!
        if let (Some(a_code), Some(bs_code)) = (lookup_evdev_code("a"), lookup_evdev_code("BackSpace")) {
            self.outbox.push(OutgoingAction::VkCombo {
                evdev_code: a_code,
                mods: 4,
            });
            self.outbox.push(OutgoingAction::VkCombo {
                evdev_code: bs_code,
                mods: 0,
            });
        }
    }

    pub fn set_lang(&mut self, lang: Lang) {
        if self.lock_english && lang != Lang::En {
            self.show_toast("密码输入固定为英文键盘");
            return;
        }
        self.lang = lang;
        self.shift = 0;
        self.shift_held = 0;
        self.internal_pinyin.clear();
        if self.use_internal_ime() {
            self.refresh_internal_pinyin();
        }
        self.rebuild_geometry();
        self.outbox.push(OutgoingAction::Fcitx(FcitxCmd::SetLang(lang)));
    }

    pub fn set_mode(&mut self, mode: KbMode) {
        if self.lock_english && mode == KbMode::Float {
            self.show_toast("密码键盘支持停靠或分离形态");
            return;
        }
        if self.prefs.mode == mode {
            return;
        }
        self.prefs.mode = mode;
        if !self.lock_english {
            self.prefs.save();
        }
        self.rebuild_geometry();
        self.outbox.push(OutgoingAction::ReconfigureSurface);
    }

    pub fn open_panel(&mut self, p: PanelKind) {
        self.sound.play(ClickKind::Tool);
        self.panel = if self.panel == p { PanelKind::None } else { p };
        self.panel_scroll_y = 0.0;
    }

    fn set_bubble(&mut self, slot: i32, key_r: RectF, main: String, sub: String) {
        let u = self.metrics.u;
        let bw = key_r.w.max(66.0 * u);
        let bh = 82.0 * u;
        let bx = (key_r.x + (key_r.w - bw) * 0.5).clamp(4.0 * u, self.metrics.kb_w - bw - 4.0 * u);
        let by = (key_r.y - bh - 6.0 * u).max(2.0 * u);
        self.bubbles.insert(
            slot,
            BubbleInfo {
                rect: RectF::new(bx, by, bw, bh),
                main,
                sub,
            },
        );
    }

    fn update_alts_sel(popup: &mut AltsPopup, px: f32, u: f32, kb_w: f32) {
        let cell_w = 64.0 * u;
        let total_w = (popup.items.len() as f32) * cell_w + 12.0 * u;
        let box_x = (popup.key_rect.x + popup.key_rect.w * 0.5 - total_w * 0.5)
            .clamp(6.0 * u, (kb_w - total_w - 6.0 * u).max(6.0 * u));
        let idx = ((px - (box_x + 6.0 * u)) / cell_w).floor() as isize;
        popup.sel = idx.clamp(0, (popup.items.len() as isize) - 1) as usize;
    }

    // ---------------- Top Band & Panels Hit-Testing ----------------

    pub fn toolbar_buttons(&self) -> Vec<(&'static str, &'static str, RectF)> {
        let u = self.metrics.u;
        let fl = self.prefs.mode == KbMode::Float;
        let sp = self.prefs.mode == KbMode::Split;
        let pad = if fl { 6.0 * u } else { 20.0 * u };
        let pitch = if fl {
            48.0 * u
        } else if sp {
            64.0 * u
        } else {
            108.0 * u
        };
        let btn_sz = if fl { 44.0 * u } else { 48.0 * u };
        let y = (self.metrics.band_h - btn_sz) * 0.5;
        let right_start = self.metrics.kb_w - pad - 2.0 * 54.0 * u - 8.0 * u;

        let items: &[(&str, &str)] = &[
            ("toolbox", "expand"),
            ("emoji", "emoji"),
            ("layout", "keyboard"),
            ("voice", "mic"),
            ("clip", "clipboard"),
            ("edit", "cursor_edit"),
        ];
        let mut out = Vec::new();
        for (i, &(id, ic)) in items.iter().enumerate() {
            let x = if sp && i >= 3 {
                right_start - ((items.len() - i) as f32) * pitch
            } else {
                pad + (i as f32) * pitch
            };
            out.push((id, ic, RectF::new(x, y, btn_sz, btn_sz)));
        }
        if !fl {
            let split_ic = if sp { "dock" } else { "split" };
            out.push((
                "toggle_split",
                split_ic,
                RectF::new(self.metrics.kb_w - pad - 2.0 * 54.0 * u, y, btn_sz, btn_sz),
            ));
        }
        out.push((
            "collapse",
            "collapse",
            RectF::new(self.metrics.kb_w - pad - btn_sz - 4.0 * u, y, btn_sz, btn_sz),
        ));
        out
    }

    fn on_band_touch_down(&mut self, slot: i32, bx: f32, by: f32) -> bool {
        let u = self.metrics.u;
        let mut tapped_cand = None;
        let mut tapped_tool = None;

        if self.composing() {
            // Rightmost expand button (`60 * u`)
            if bx >= self.metrics.kb_w - 62.0 * u && by >= 22.0 * u {
                tapped_tool = Some("expand_cands");
            } else {
                for &(idx, r) in &self.cand_rects {
                    if r.contains(bx, by) {
                        tapped_cand = Some(idx);
                        break;
                    }
                }
            }
        } else {
            for (id, _, r) in self.toolbar_buttons() {
                if r.contains(bx, by) {
                    tapped_tool = Some(id);
                    break;
                }
            }
        }

        self.gestures.insert(
            slot,
            ActiveGesture::BandPull {
                x0: bx,
                y0: by,
                cand_scroll_x0: self.cand_scroll_x,
                mode: BandGestureMode::Undecided,
                tapped_cand,
                tapped_tool,
            },
        );
        true
    }

    fn activate_toolbar_btn(&mut self, id: &str) {
        if self.lock_english && matches!(id, "emoji" | "clip" | "voice") {
            self.sound.play(ClickKind::Tool);
            self.show_toast("锁屏状态已隐藏此面板");
            return;
        }
        match id {
            "toolbox" => self.open_panel(PanelKind::Toolbox),
            "emoji" => self.open_panel(PanelKind::Emoji),
            "layout" => self.open_panel(PanelKind::LayoutPicker),
            "voice" => {
                self.sound.play(ClickKind::Tool);
                self.show_toast("语音输入：请连接离线语音或麦克风服务");
            }
            "clip" => self.open_panel(PanelKind::Clipboard),
            "edit" => self.open_panel(PanelKind::TextEdit),
            "expand_cands" => {
                self.sound.play(ClickKind::Expand);
                self.panel = PanelKind::Candidates;
                self.panel_scroll_y = 0.0;
            }
            "toggle_split" => {
                self.sound.play(ClickKind::Tool);
                let next = if self.prefs.mode == KbMode::Split {
                    KbMode::Dock
                } else {
                    KbMode::Split
                };
                self.set_mode(next);
            }
            "collapse" => {
                self.sound.play(ClickKind::Tool);
                self.hide(!self.direct_text_sink);
            }
            _ => {}
        }
    }

    fn on_panel_touch_down(&mut self, slot: i32, bx: f32, by: f32) -> bool {
        let u = self.metrics.u;
        // Top band stays live while Toolbox or LayoutPicker is open!
        if matches!(self.panel, PanelKind::Toolbox | PanelKind::LayoutPicker) && by < self.metrics.band_h {
            return self.on_band_touch_down(slot, bx, by);
        }

        // Special interactive regions inside TextEdit & Feedback
        if self.panel == PanelKind::TextEdit && by >= self.metrics.band_h {
            let pad_r = self.edit_trackpad_rect();
            if pad_r.contains(bx, by) {
                self.edit_pad_dot = Some((bx, by));
                self.gestures.insert(
                    slot,
                    ActiveGesture::EditTrackpad {
                        lx: bx,
                        ly: by,
                        t0: Instant::now(),
                        moved: false,
                        drag_select: false,
                    },
                );
                return true;
            }
        } else if self.panel == PanelKind::Feedback && by >= self.metrics.band_h {
            let vol_r = self.feedback_vol_rect();
            if vol_r.inset(-4.0 * u, -6.0 * u).contains(bx, by) {
                let frac = ((bx - vol_r.x) / vol_r.w.max(1.0)).clamp(0.0, 1.0);
                let nv = (frac * 10.0).round() as u8;
                self.prefs.vol = nv;
                self.prefs.sound_on = nv > 0;
                self.sound.set_volume(nv);
                self.sound.play(ClickKind::Char);
                if !self.lock_english {
                    self.prefs.save();
                }
                self.gestures
                    .insert(slot, ActiveGesture::FeedbackSlider { track: vol_r });
                return true;
            }
        }

        self.gestures.insert(
            slot,
            ActiveGesture::PanelScroll {
                y0: by,
                scroll0: self.panel_scroll_y,
                moved: false,
                tap_pos: (bx, by),
            },
        );
        true
    }

    pub fn edit_trackpad_rect(&self) -> RectF {
        let u = self.metrics.u;
        let fl = self.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 12.0 * u;
        let row_h = 58.0 * u;
        let top_y = self.metrics.band_h + 6.0 * u;
        let area_w = self.metrics.kb_w - 2.0 * inset;
        let area_h = self.metrics.kb_h - top_y - self.metrics.bot_h;
        let cw = (area_w - 4.0 * gap) / 5.0;
        let pw = if fl { area_w } else { 3.0 * cw + 2.0 * gap };
        let ph = if fl {
            (area_h - row_h - 2.0 * gap) * 0.48
        } else {
            area_h - row_h - gap
        };
        RectF::new(inset, top_y, pw, ph)
    }

    pub fn feedback_vol_rect(&self) -> RectF {
        let u = self.metrics.u;
        let inset = 16.0 * u;
        let col_w = self.metrics.kb_w - 2.0 * inset;
        RectF::new(inset, self.metrics.band_h + 44.0 * u, col_w, 54.0 * u)
    }

    fn handle_panel_tap(&mut self, bx: f32, by: f32) {
        let u = self.metrics.u;
        // Title bar back button (`0 .. 84*u`, `0 .. band_h`) for panels with PanelTitle
        if !matches!(self.panel, PanelKind::Toolbox | PanelKind::LayoutPicker)
            && by < self.metrics.band_h
            && bx < 96.0 * u
        {
            self.sound.play(ClickKind::Tool);
            self.panel = PanelKind::None;
            return;
        }

        match self.panel {
            PanelKind::Toolbox => self.tap_toolbox(bx, by),
            PanelKind::LayoutPicker => self.tap_layout_picker(bx, by),
            PanelKind::Feedback => self.tap_feedback(bx, by),
            PanelKind::TextEdit => self.tap_text_edit(bx, by),
            PanelKind::Symbols => self.tap_symbols(bx, by),
            PanelKind::Emoji => self.tap_emoji(bx, by),
            PanelKind::Candidates => self.tap_candidates(bx, by),
            PanelKind::Clipboard => self.tap_clipboard(bx, by),
            PanelKind::None => {}
        }
    }

    pub fn toolbox_tiles() -> &'static [(&'static str, &'static str, &'static str, bool)] {
        &[
            ("clip", "clipboard", "剪贴板", false),
            ("edit", "cursor_edit", "文本编辑", false),
            ("feedback", "sound", "按键音效", false),
            ("split", "split", "分离键盘", true),
            ("float", "float", "悬浮键盘", true),
            ("night", "theme", "浅/深色", true),
            ("flick", "arrow_up", "上滑输入", true),
            ("bubble", "bubble", "按键气泡", true),
            ("swipe", "space_swipe", "滑动光标", true),
            ("glow", "glow", "点触光感", true),
            ("tall", "height_adjust", "增高键盘", false),
            ("short", "height_adjust", "调低键盘", false),
        ]
    }

    pub fn is_toolbox_tile_on(&self, id: &str) -> bool {
        match id {
            "split" => self.prefs.mode == KbMode::Split,
            "float" => self.prefs.mode == KbMode::Float,
            "night" => !self.prefs.skin.is_light(),
            "flick" => self.prefs.flick,
            "bubble" => self.prefs.bubble,
            "swipe" => self.prefs.cursor_swipe,
            "glow" => self.prefs.tap_glow,
            _ => false,
        }
    }

    fn tap_toolbox(&mut self, bx: f32, by: f32) {
        let u = self.metrics.u;
        let fl = self.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 12.0 * u;
        let top_y = self.metrics.band_h + 6.0 * u;
        let cols = if fl { 4 } else { 6 };
        let grid_w = self.metrics.kb_w - 2.0 * inset;
        let grid_h = self.metrics.kb_h - top_y - self.metrics.bot_h;
        let cw = (grid_w - ((cols - 1) as f32) * gap) / (cols as f32);
        let rows = (Self::toolbox_tiles().len() + cols - 1) / cols;
        let ch = (grid_h - ((rows - 1) as f32) * gap) / (rows as f32);

        for (i, &(id, _, _, _)) in Self::toolbox_tiles().iter().enumerate() {
            let c = i % cols;
            let r = i / cols;
            let rect = RectF::new(inset + (c as f32) * (cw + gap), top_y + (r as f32) * (ch + gap), cw, ch);
            if rect.contains(bx, by) {
                self.sound.play(ClickKind::Tool);
                if self.lock_english && id == "clip" {
                    self.show_toast("锁屏状态已隐藏剪贴板");
                    return;
                }
                match id {
                    "clip" => self.panel = PanelKind::Clipboard,
                    "edit" => self.panel = PanelKind::TextEdit,
                    "feedback" => self.panel = PanelKind::Feedback,
                    "split" => {
                        let m = if self.prefs.mode == KbMode::Split { KbMode::Dock } else { KbMode::Split };
                        self.set_mode(m);
                    }
                    "float" => {
                        let m = if self.prefs.mode == KbMode::Float { KbMode::Dock } else { KbMode::Float };
                        self.set_mode(m);
                    }
                    "night" => {
                        self.prefs.skin = self.prefs.skin.toggle_night();
                        self.pal = Palette::for_skin(self.prefs.skin);
                        self.prefs.save();
                        self.show_toast(if self.prefs.skin.is_light() { "已切换浅色主题" } else { "已切换深色主题" });
                    }
                    "flick" => {
                        self.prefs.flick = !self.prefs.flick;
                        self.prefs.save();
                    }
                    "bubble" => {
                        self.prefs.bubble = !self.prefs.bubble;
                        self.prefs.save();
                    }
                    "swipe" => {
                        self.prefs.cursor_swipe = !self.prefs.cursor_swipe;
                        self.prefs.save();
                    }
                    "glow" => {
                        self.prefs.tap_glow = !self.prefs.tap_glow;
                        self.prefs.save();
                    }
                    "tall" => {
                        self.prefs.hadj = (self.prefs.hadj + 0.08).clamp(0.80, 1.24);
                        self.prefs.save();
                        self.rebuild_geometry();
                        self.outbox.push(OutgoingAction::ReconfigureSurface);
                        self.show_toast(format!("键盘高度 {:.0}%", self.prefs.hadj * 100.0));
                    }
                    "short" => {
                        self.prefs.hadj = (self.prefs.hadj - 0.08).clamp(0.80, 1.24);
                        self.prefs.save();
                        self.rebuild_geometry();
                        self.outbox.push(OutgoingAction::ReconfigureSurface);
                        self.show_toast(format!("键盘高度 {:.0}%", self.prefs.hadj * 100.0));
                    }
                    _ => {}
                }
                return;
            }
        }
    }

    fn tap_layout_picker(&mut self, bx: f32, by: f32) {
        let u = self.metrics.u;
        let inset = 16.0 * u;
        let gap = 12.0 * u;
        let w = self.metrics.kb_w - 2.0 * inset;
        let row1_y = self.metrics.band_h + 36.0 * u;
        let row2_y = row1_y + 78.0 * u + 36.0 * u;
        let row1 = ["zh", "en", "ja", "num", "sym"];
        let cw1 = (w - 4.0 * gap) / 5.0;
        for (i, &id) in row1.iter().enumerate() {
            let r = RectF::new(inset + (i as f32) * (cw1 + gap), row1_y, cw1, 74.0 * u);
            if r.contains(bx, by) {
                self.sound.play(ClickKind::Tool);
                self.panel = PanelKind::None;
                match id {
                    "zh" => {
                        self.layout = KbLayout::Main;
                        self.set_lang(Lang::Zh);
                    }
                    "en" => {
                        self.layout = KbLayout::Main;
                        self.set_lang(Lang::En);
                    }
                    "ja" => {
                        self.layout = KbLayout::Main;
                        self.set_lang(Lang::Ja);
                    }
                    "num" => {
                        self.layout = KbLayout::Num;
                        self.rebuild_geometry();
                    }
                    "sym" => {
                        self.panel = PanelKind::Symbols;
                    }
                    _ => {}
                }
                return;
            }
        }
        let row2 = [
            ("dock", KbMode::Dock),
            ("split", KbMode::Split),
            ("float", KbMode::Float),
        ];
        let cw2 = (w - 2.0 * gap) / 3.0;
        for (i, &(_, mode)) in row2.iter().enumerate() {
            let r = RectF::new(inset + (i as f32) * (cw2 + gap), row2_y, cw2, 74.0 * u);
            if r.contains(bx, by) {
                self.sound.play(ClickKind::Tool);
                self.panel = PanelKind::None;
                self.set_mode(mode);
                return;
            }
        }
    }

    fn tap_feedback(&mut self, bx: f32, by: f32) {
        let u = self.metrics.u;
        let inset = 16.0 * u;
        let style_y = self.metrics.band_h + 132.0 * u;
        let w = self.feedback_vol_rect().w;
        let sw = (w - 16.0 * u) / 3.0;
        for (i, &(code, _)) in [("std", "标准"), ("soft", "柔和"), ("mech", "机械")].iter().enumerate() {
            let r = RectF::new(inset + 4.0 * u + (i as f32) * (sw + 4.0 * u), style_y + 4.0 * u, sw, 46.0 * u);
            if r.contains(bx, by) {
                self.prefs.sound = code.into();
                self.sound.set_style(SoundStyle::from_name(code));
                self.sound.play(ClickKind::Char);
                self.prefs.save();
                return;
            }
        }
    }

    fn tap_text_edit(&mut self, bx: f32, by: f32) {
        let u = self.metrics.u;
        let fl = self.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 12.0 * u;
        let row_h = 58.0 * u;
        let top_y = self.metrics.band_h + 6.0 * u;
        let area_w = self.metrics.kb_w - 2.0 * inset;
        let area_h = self.metrics.kb_h - top_y - self.metrics.bot_h;
        let pad_r = self.edit_trackpad_rect();

        // 3x3 cross
        let cross_x = if fl { inset } else { pad_r.x + pad_r.w + gap };
        let cross_y = if fl { pad_r.y + pad_r.h + gap } else { top_y };
        let cross_w = if fl { area_w } else { inset + area_w - cross_x };
        let cross_h = pad_r.h;
        let bw = (cross_w - 20.0 * u) / 3.0;
        let bh = (cross_h - 20.0 * u) / 3.0;
        let cells = [
            (0, 0, "Home"),
            (1, 0, "Up"),
            (2, 0, "End"),
            (0, 1, "Left"),
            (1, 1, "SELECT"),
            (2, 1, "Right"),
            (1, 2, "Down"),
        ];
        for (c, r, act) in cells {
            let rect = RectF::new(
                cross_x + (c as f32) * (bw + 10.0 * u),
                cross_y + (r as f32) * (bh + 10.0 * u),
                bw,
                bh,
            );
            if rect.contains(bx, by) {
                self.sound.play(ClickKind::Tool);
                if act == "SELECT" {
                    self.edit_selecting = !self.edit_selecting;
                } else if let Some(ev) = lookup_evdev_code(act) {
                    let mods = if self.edit_selecting { 1 } else { 0 };
                    self.outbox.push(OutgoingAction::VkCombo { evdev_code: ev, mods });
                }
                return;
            }
        }

        // Bottom row: 全选 / 剪切 / 复制 / 粘贴 / 删除
        let bot_y = top_y + area_h - row_h;
        let act_w = (area_w - 4.0 * gap) / 5.0;
        let bot_acts: &[(&str, u32, Option<&str>)] = &[
            ("a", 4, None),
            ("x", 4, Some("已剪切")),
            ("c", 4, Some("已复制")),
            ("v", 4, None),
            ("BackSpace", 0, None),
        ];
        for (i, &(kname, mods, toast)) in bot_acts.iter().enumerate() {
            let rect = RectF::new(inset + (i as f32) * (act_w + gap), bot_y, act_w, row_h);
            if rect.contains(bx, by) {
                self.sound.play(ClickKind::Char);
                if let Some(ev) = lookup_evdev_code(kname) {
                    self.outbox.push(OutgoingAction::VkCombo { evdev_code: ev, mods });
                }
                if let Some(msg) = toast {
                    self.show_toast(msg);
                }
                return;
            }
        }
    }

    fn tap_symbols(&mut self, bx: f32, by: f32) {
        let u = self.metrics.u;
        let fl = self.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 10.0 * u;
        let row_h = 56.0 * u;
        let top_y = self.metrics.band_h + 6.0 * u;
        let cat_y = self.metrics.kb_w.min(self.metrics.kb_h); // unused dummy
        let _ = cat_y;
        let bar_y = self.metrics.kb_h - self.metrics.bot_h - row_h;
        let grid_h = bar_y - top_y - 10.0 * u;
        let right_w = if fl { 74.0 * u } else { 108.0 * u };
        let grid_w = self.metrics.kb_w - 2.0 * inset - right_w - gap;

        // Bottom category bar (`返回` + 7 categories + `锁定`)
        if by >= bar_y && by <= bar_y + row_h {
            if bx < inset + 84.0 * u {
                self.sound.play(ClickKind::Tool);
                self.panel = PanelKind::None;
                return;
            }
            if bx >= self.metrics.kb_w - inset - 92.0 * u {
                self.sound.play(ClickKind::Tool);
                self.prefs.sym_lock = !self.prefs.sym_lock;
                self.prefs.save();
                return;
            }
            let mid_x = inset + 92.0 * u;
            let mid_w = self.metrics.kb_w - 2.0 * inset - 192.0 * u;
            let cw = mid_w / (SYM_CATS.len() as f32);
            let idx = ((bx - mid_x) / cw).floor() as isize;
            if idx >= 0 && (idx as usize) < SYM_CATS.len() {
                self.sound.play(ClickKind::Tool);
                self.sym_cat = idx as usize;
                self.panel_scroll_y = 0.0;
            }
            return;
        }

        // Right column (`退格 / 空格 / 回车`)
        let rx = self.metrics.kb_w - inset - right_w;
        if bx >= rx && by >= top_y && by <= top_y + grid_h {
            let rh = (grid_h - 12.0 * u) / 3.0;
            let idx = ((by - top_y) / (rh + 6.0 * u)).floor() as usize;
            self.sound.play(ClickKind::Char);
            match idx {
                0 => self.do_delete(),
                1 => self.type_text(" "),
                _ => {
                    self.panel = PanelKind::None;
                    if self.direct_text_sink {
                        self.outbox.push(OutgoingAction::DirectEnter);
                    } else {
                        self.outbox
                            .push(OutgoingAction::Fcitx(FcitxCmd::TypeKey("Return".into())));
                    }
                }
            }
            return;
        }

        // Symbol grid
        if bx >= inset && bx <= inset + grid_w && by >= top_y && by <= top_y + grid_h {
            let syms = SYM_CATS[self.sym_cat.min(SYM_CATS.len() - 1)].1;
            let cols = if fl { 5 } else if self.sym_cat == 1 { 11 } else { 10 };
            let cw = grid_w / (cols as f32);
            let ch = grid_h / 3.0;
            let c = ((bx - inset) / cw).floor() as usize;
            let r = ((by - top_y + self.panel_scroll_y) / ch).floor() as usize;
            let idx = r * cols + c;
            if c < cols && idx < syms.len() {
                self.sound.play(ClickKind::Char);
                let s = syms[idx];
                self.type_text(s);
                if !self.prefs.sym_lock {
                    self.panel = PanelKind::None;
                }
            }
        }
    }

    pub fn emoji_items(&self, cat: usize) -> Vec<String> {
        if cat == 0 && !self.prefs.recent_emoji.is_empty() {
            return self.prefs.recent_emoji.clone();
        }
        let raw = EMOJI_CATS[cat.min(EMOJI_CATS.len() - 1)].1;
        raw.graphemes(true).map(|g| g.to_string()).collect()
    }

    fn tap_emoji(&mut self, bx: f32, by: f32) {
        let u = self.metrics.u;
        let fl = self.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let row_h = 56.0 * u;
        let top_y = self.metrics.band_h + 6.0 * u;
        let bar_y = self.metrics.kb_h - self.metrics.bot_h - row_h;
        let grid_h = bar_y - top_y - 10.0 * u;
        let grid_w = self.metrics.kb_w - 2.0 * inset;

        if by >= bar_y && by <= bar_y + row_h {
            if bx < inset + 84.0 * u {
                self.sound.play(ClickKind::Tool);
                self.panel = PanelKind::None;
                return;
            }
            if bx >= self.metrics.kb_w - inset - 84.0 * u {
                self.sound.play(ClickKind::Del);
                self.do_delete();
                return;
            }
            let mid_x = inset + 92.0 * u;
            let mid_w = self.metrics.kb_w - 2.0 * inset - 184.0 * u;
            let cw = mid_w / (EMOJI_CATS.len() as f32);
            let idx = ((bx - mid_x) / cw).floor() as isize;
            if idx >= 0 && (idx as usize) < EMOJI_CATS.len() {
                self.sound.play(ClickKind::Tool);
                self.emo_cat = idx as usize;
                self.panel_scroll_y = 0.0;
            }
            return;
        }

        if bx >= inset && bx <= inset + grid_w && by >= top_y && by <= top_y + grid_h {
            let items = self.emoji_items(self.emo_cat);
            let cols = if fl { 7 } else { 13 };
            let cw = grid_w / (cols as f32);
            let ch = 56.0 * u;
            let c = ((bx - inset) / cw).floor() as usize;
            let r = ((by - top_y + self.panel_scroll_y) / ch).floor() as usize;
            let idx = r * cols + c;
            if c < cols && idx < items.len() {
                self.sound.play(ClickKind::Char);
                let em = items[idx].clone();
                self.prefs.recent_emoji.retain(|e| e != &em);
                self.prefs.recent_emoji.insert(0, em.clone());
                if self.prefs.recent_emoji.len() > 28 {
                    self.prefs.recent_emoji.truncate(28);
                }
                self.prefs.save();
                self.type_text(&em);
            }
        }
    }

    fn tap_candidates(&mut self, bx: f32, by: f32) {
        let u = self.metrics.u;
        let fl = self.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 10.0 * u;
        let top_y = self.metrics.band_h + 6.0 * u;
        let area_h = self.metrics.kb_h - top_y - self.metrics.bot_h;
        let right_w = if fl { 78.0 * u } else { 112.0 * u };
        let grid_w = self.metrics.kb_w - 2.0 * inset - right_w - gap;
        let rx = self.metrics.kb_w - inset - right_w;

        if bx >= rx && by >= top_y && by <= top_y + area_h {
            let rh = (area_h - 16.0 * u) / 3.0;
            let idx = ((by - top_y) / (rh + 8.0 * u)).floor() as usize;
            self.sound.play(ClickKind::Tool);
            match idx {
                0 => self.panel = PanelKind::None,
                1 => self.do_delete(),
                _ => {
                    self.outbox.push(OutgoingAction::Fcitx(FcitxCmd::ClearComposition));
                    self.panel = PanelKind::None;
                }
            }
            return;
        }

        if bx >= inset && bx <= inset + grid_w && by >= top_y && by <= top_y + area_h {
            let cols = if fl { 3 } else { 6 };
            let cw = grid_w / (cols as f32);
            let ch = 62.0 * u;
            let c = ((bx - inset) / cw).floor() as usize;
            let r = ((by - top_y + self.panel_scroll_y) / ch).floor() as usize;
            let idx = r * cols + c;
            if c < cols && idx < self.candidates.len() {
                self.sound.play(ClickKind::Char);
                self.haptics.pulse_at_x(self.screen_x_for(bx), self.screen_w as f32, HapticKind::Char);
                self.select_candidate_idx(idx);
            }
        }
    }

    pub fn filtered_clipboard(&self) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let mut push_unique = |s: &str, pin: bool| {
            if s.trim().is_empty() || out.iter().any(|(x, _)| x == s) {
                return;
            }
            let keep = match self.clip_cat {
                1 => s.contains("http://") || s.contains("https://") || s.contains("www."),
                2 => s.chars().filter(|c| c.is_ascii_digit()).count() >= 7,
                3 => s.contains('@') && s.contains('.'),
                _ => true,
            };
            if keep {
                out.push((s.to_string(), pin));
            }
        };
        for p in &self.prefs.pinned {
            push_unique(p, true);
        }
        for h in &self.clipboard {
            push_unique(h, false);
        }
        out
    }

    fn tap_clipboard(&mut self, bx: f32, by: f32) {
        let u = self.metrics.u;
        let fl = self.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 10.0 * u;
        let now = Instant::now();

        // Top-right Clear button
        if by < self.metrics.band_h && bx >= self.metrics.kb_w - 110.0 * u {
            self.sound.play(ClickKind::Tool);
            if self.clip_clear_armed_until.is_some_and(|t| now < t) {
                self.clip_clear_armed_until = None;
                self.clipboard.clear();
                self.show_toast("已清空未锁定剪贴板");
            } else {
                self.clip_clear_armed_until = Some(now + Duration::from_secs(5));
            }
            return;
        }

        // Category pills (`band_h + 6 .. band_h + 46`)
        let cat_y = self.metrics.band_h + 8.0 * u;
        if by >= cat_y && by <= cat_y + 38.0 * u {
            let cw = 84.0 * u;
            let idx = ((bx - inset) / (cw + 8.0 * u)).floor() as isize;
            if (0..4).contains(&idx) {
                self.sound.play(ClickKind::Tool);
                self.clip_cat = idx as usize;
                self.panel_scroll_y = 0.0;
            }
            return;
        }

        let grid_y = self.metrics.band_h + 54.0 * u;
        let grid_w = self.metrics.kb_w - 2.0 * inset;
        if by >= grid_y && bx >= inset && bx <= inset + grid_w {
            let items = self.filtered_clipboard();
            let cols = if fl { 1 } else { 3 };
            let cw = (grid_w - ((cols - 1) as f32) * gap) / (cols as f32);
            let ch = 82.0 * u;
            let c = ((bx - inset) / (cw + gap)).floor() as usize;
            let r = ((by - grid_y + self.panel_scroll_y) / (ch + gap)).floor() as usize;
            let idx = r * cols + c;
            if c < cols && idx < items.len() {
                let card_x = inset + (c as f32) * (cw + gap);
                let (txt, pinned) = &items[idx];
                // Top-right pin button on card (`42 * u` wide)
                if bx >= card_x + cw - 46.0 * u {
                    self.sound.play(ClickKind::Tool);
                    if *pinned {
                        self.prefs.pinned.retain(|x| x != txt);
                        self.show_toast("已解锁");
                    } else {
                        self.prefs.pinned.insert(0, txt.clone());
                        self.show_toast("已锁定，清空时保留");
                    }
                    self.prefs.save();
                } else {
                    self.sound.play(ClickKind::Char);
                    let t = txt.clone();
                    self.type_text(&t);
                    self.show_toast("已粘贴");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_osk_swallows_key_gutter_and_passes_center_gap() {
        let mut osk = OskState::new(1682, 1051);
        osk.set_mode(KbMode::Split);
        osk.show(false);
        osk.slide_y.snap(0.0);

        // 1. Transparent Split center gap below top band must return false and NOT capture slot
        let mid_x = osk.metrics.fpad + osk.metrics.kb_w * 0.5;
        let mid_y = osk.metrics.fpad + osk.metrics.band_h + 60.0;
        assert!(!osk.on_touch_down(1, mid_x, mid_y));
        assert!(osk.touch_pts.is_empty());

        // 2. Key gutter between Q and W inside left Split wing pad must be swallowed (returns true)
        let q = osk.keys[0].rect;
        let w = osk.keys[1].rect;
        let gutter_x = osk.metrics.fpad + (q.x + q.w + w.x) * 0.5;
        let gutter_y = osk.metrics.fpad + q.y + q.h * 0.5;
        assert!(!q.contains(gutter_x - osk.metrics.fpad, gutter_y - osk.metrics.fpad));
        assert!(osk.on_touch_down(2, gutter_x, gutter_y));
        let _ = osk.on_touch_up(2);
    }
}
