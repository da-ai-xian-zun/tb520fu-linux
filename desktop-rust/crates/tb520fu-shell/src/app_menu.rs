// SPDX-License-Identifier: MIT
use std::time::{Duration, Instant};

use cosmic_text::Align;
use tb520fu_ui::canvas::{Canvas, RectF};
use tb520fu_ui::color::{Palette, Rgba};
use tb520fu_ui::haptics::{HapticEngine, HapticKind};
use tb520fu_ui::pinyin::matches_app_query;
use tb520fu_ui::svg_icons::IconSet;
use tb520fu_ui::text::{FontWeight, TextEngine};

use crate::niri_ipc::{spawn_app_with_mode, NiriSnapshot};
use crate::sys_state::{AppCategory, DesktopApp, LaunchMode, ShellPrefs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMenuEvent {
    None,
    FocusSearchBar,
    ClearSearch,
    ModeChanged,
    AppLaunched,
}

#[derive(Debug, Clone)]
enum MenuHit {
    SearchBox,
    ClearQuery,
    Mode(LaunchMode),
    Category(AppCategory),
    AppIdx(usize),
}

#[derive(Debug, Clone, Copy)]
struct PressTrack {
    slot: i32,
    x0: f32,
    y0: f32,
    scroll0: f32,
    t0: Instant,
    scrolling: bool,
    long_fired: bool,
    app_idx: Option<usize>,
}

pub struct AppMenuColumn {
    pub apps: Vec<DesktopApp>,
    pub query: String,
    pub search_focused: bool,
    pub category: AppCategory,
    pub scroll_y: f32,
    max_scroll: f32,
    last_screen_w: f32,
    hits: Vec<(RectF, MenuHit)>,
    grid_clip: RectF,
    press: Option<PressTrack>,
}

impl AppMenuColumn {
    pub fn new(mut apps: Vec<DesktopApp>) -> Self {
        if apps.is_empty() {
            apps = fallback_demo_apps();
        }
        Self {
            apps,
            query: String::new(),
            search_focused: false,
            category: AppCategory::All,
            scroll_y: 0.0,
            max_scroll: 0.0,
            last_screen_w: 1472.0,
            hits: Vec::with_capacity(64),
            grid_clip: RectF::default(),
            press: None,
        }
    }

    pub fn filtered_indices(&self, prefs: &ShellPrefs) -> Vec<usize> {
        let q = self.query.trim();
        self.apps
            .iter()
            .enumerate()
            .filter(|(_, app)| {
                let cat_ok = if !q.is_empty() {
                    true
                } else {
                    app.matches_category(self.category, &prefs.favorites)
                };
                if !cat_ok {
                    return false;
                }
                if q.is_empty() {
                    true
                } else {
                    matches_app_query(&app.name, &app.id, q)
                }
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Check `320ms` long-press on an app icon -> launch directly in a new workspace!
    pub fn tick_long_press(&mut self, niri: &NiriSnapshot) -> bool {
        let Some(p) = &mut self.press else {
            return false;
        };
        if !p.scrolling && !p.long_fired {
            if let Some(app_idx) = p.app_idx {
                if p.t0.elapsed() >= Duration::from_millis(320) {
                    p.long_fired = true;
                    HapticEngine::global().pulse_norm(HapticKind::Medium, p.x0 / self.last_screen_w);
                    if let Some(app) = self.apps.get(app_idx) {
                        spawn_app_with_mode(app.exec_argv.clone(), LaunchMode::NewWs, niri);
                        return true;
                    }
                }
            }
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        pal: &Palette,
        niri: &NiriSnapshot,
        prefs: &ShellPrefs,
        panel: RectF,
        alpha: f32,
    ) {
        self.hits.clear();
        self.last_screen_w = canvas.logical_width().max(1.0);

        // Borderless directly on the `.pull` Liquid Glass sheet (aligned with outer columns)
        let pad = 8.0;
        let ix = panel.x + pad;
        let iw = (panel.w - 2.0 * pad).max(120.0);
        let mut cy = panel.y + 4.0;

        // 1. Top Row: Dark Inset Search Bar + 3-Way Launch Mode Segmented Pill (`[新列] [并列] [新工作区]`)
        let mode_seg_w = 204.0_f32.min(iw * 0.44);
        let search_w = (iw - mode_seg_w - 10.0).max(120.0);
        let top_h = 40.0;

        let sr = RectF::new(ix, cy, search_w, top_h);
        let inset_well = if pal.light {
            pal.chip.with_alpha(alpha)
        } else {
            Rgba::rgba_f(0, 0, 0, 0.24 * alpha)
        };
        canvas.fill_round_rect(sr, 20.0, inset_well);
        canvas.stroke_round_rect(
            sr,
            20.0,
            if self.search_focused { 1.5 } else { 1.0 },
            if self.search_focused {
                pal.acc.with_alpha(alpha)
            } else {
                pal.sep.with_alpha(alpha)
            },
        );
        icons.draw(
            canvas,
            "search",
            RectF::new(sr.x + 14.0, sr.y + 11.5, 17.0, 17.0),
            if self.search_focused {
                pal.active_drawer_ink.with_alpha(alpha)
            } else {
                pal.ink3.with_alpha(alpha)
            },
            1.95,
        );
        let q_empty = self.query.is_empty();
        let q_disp = if q_empty {
            "搜索应用".to_string()
        } else if self.search_focused {
            format!("{}|", self.query)
        } else {
            self.query.clone()
        };
        text.draw_in_rect(
            canvas,
            &q_disp,
            RectF::new(sr.x + 37.0, sr.y, sr.w - 70.0, sr.h),
            13.8,
            if q_empty {
                FontWeight::Normal
            } else {
                FontWeight::Medium
            },
            if q_empty {
                pal.ink3.with_alpha(0.85 * alpha)
            } else {
                pal.ink.with_alpha(alpha)
            },
            Align::Left,
        );
        if !q_empty {
            let clr_r = RectF::new(sr.x + sr.w - 34.0, sr.y + 6.0, 28.0, 28.0);
            icons.draw(
                canvas,
                "close",
                clr_r.inset(7.0, 7.0),
                pal.ink3.with_alpha(alpha),
                2.0,
            );
            self.hits.push((clr_r, MenuHit::ClearQuery));
            self.hits.push((
                RectF::new(sr.x, sr.y, sr.w - 36.0, sr.h),
                MenuHit::SearchBox,
            ));
        } else {
            self.hits.push((sr, MenuHit::SearchBox));
        }

        // 3-Way Launch Mode Segmented Pill (`[新列] [并列] [新工作区]`)
        let mr_h = 36.0;
        let mr = RectF::new(
            sr.x + sr.w + 10.0,
            cy + (top_h - mr_h) * 0.5,
            mode_seg_w,
            mr_h,
        );
        canvas.fill_round_rect(mr, 18.0, inset_well);
        canvas.stroke_round_rect(mr, 18.0, 1.0, pal.sep.with_alpha(alpha));
        let seg_inner = mr.inset(3.0, 3.0);
        let seg_weights = [0.29_f32, 0.29, 0.42];
        let mut sx_cur = seg_inner.x;
        for (mi, m) in LaunchMode::ALL.into_iter().enumerate() {
            let each_w = seg_inner.w * seg_weights[mi];
            let item_r = RectF::new(sx_cur, seg_inner.y, each_w, seg_inner.h);
            sx_cur += each_w;
            let active = prefs.launch_mode == m;
            if active {
                canvas.fill_round_rect(item_r, 15.0, pal.acc.with_alpha(alpha));
            }
            text.draw_in_rect(
                canvas,
                m.label(),
                item_r,
                12.0,
                FontWeight::SemiBold,
                if active {
                    Rgba::WHITE.with_alpha(alpha)
                } else {
                    pal.ink.with_alpha(0.78 * alpha)
                },
                Align::Center,
            );
            self.hits.push((item_r, MenuHit::Mode(m)));
        }
        cy += top_h + 13.0;

        // 2. Compact Left-Aligned Category Pills (`全部 / 常用 / 开发与终端 / 办公与笔记 / 影音与创作 / 系统工具`)
        let cat_h = 31.0;
        let cat_gap = 7.0;
        let mut cat_x = ix;
        for cat in AppCategory::ALL {
            let (tw, _) = text.measure(cat.label(), 12.5, FontWeight::SemiBold, canvas.scale);
            let cat_w = (tw + 26.0).max(52.0);
            if cat_x + cat_w > ix + iw + 2.0 {
                break;
            }
            let cr = RectF::new(cat_x, cy, cat_w, cat_h);
            let sel = self.category == cat && self.query.trim().is_empty();
            if sel {
                canvas.fill_round_rect(cr, 15.5, pal.pill_fill.with_alpha(alpha));
            } else {
                canvas.fill_round_rect(cr, 15.5, pal.chip.with_alpha(alpha));
                canvas.stroke_round_rect(cr, 15.5, 1.0, pal.sep.with_alpha(alpha));
            }
            text.draw_in_rect(
                canvas,
                cat.label(),
                cr,
                12.5,
                if sel {
                    FontWeight::SemiBold
                } else {
                    FontWeight::Medium
                },
                if sel {
                    pal.pill_fill_ink.with_alpha(alpha)
                } else {
                    pal.ink.with_alpha(0.84 * alpha)
                },
                Align::Center,
            );
            self.hits.push((cr, MenuHit::Category(cat)));
            cat_x += cat_w + cat_gap;
        }
        cy += cat_h + 16.0;

        // 3. Scrollable 5-Column Crafted Squircle App Grid (stretching all the way to the bottom of `panel`)
        let grid_h = (panel.y + panel.h - 8.0 - cy).max(80.0);
        let grid_r = RectF::new(ix, cy, iw, grid_h);
        self.grid_clip = grid_r;

        let filtered = self.filtered_indices(prefs);
        let cols = if iw >= 520.0 { 5usize } else { 4usize };
        let cell_w = iw / (cols as f32);
        let cell_h = 108.0_f32;
        let rows = (filtered.len() + cols - 1) / cols;
        let content_h = (rows as f32) * cell_h;
        self.max_scroll = (content_h - grid_h).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll);

        canvas.set_clip_round_rect(Some((grid_r, [16.0; 4])));

        if filtered.is_empty() {
            text.draw_in_rect(
                canvas,
                "未找到匹配应用",
                RectF::new(grid_r.x, grid_r.y + grid_h * 0.34, grid_r.w, 32.0),
                14.5,
                FontWeight::Medium,
                pal.ink2.with_alpha(alpha),
                Align::Center,
            );
        } else {
            for (pos, &app_idx) in filtered.iter().enumerate() {
                let row = pos / cols;
                let col = pos % cols;
                let cx = grid_r.x + (col as f32) * cell_w;
                let ry = grid_r.y + (row as f32) * cell_h - self.scroll_y;
                if ry + cell_h < grid_r.y - 4.0 || ry > grid_r.y + grid_r.h + 4.0 {
                    continue;
                }
                let app = &self.apps[app_idx];
                let pressed = self
                    .press
                    .is_some_and(|p| !p.scrolling && p.app_idx == Some(app_idx));
                let sq_sz = if pressed { 56.0 } else { 60.0 };
                let sq_rad = sq_sz * 0.29;
                let sq_r = RectF::new(
                    cx + (cell_w - sq_sz) * 0.5,
                    ry + 6.0 + (60.0 - sq_sz) * 0.5,
                    sq_sz,
                    sq_sz,
                );

                // Always prefer the application's own icon (`SVG`/`PNG`/`XPM`); only fall back to
                // curated gradient squircle + vector glyph when no app icon exists on disk.
                if let Some(bmp) = &app.icon {
                    if bmp.has_opaque_corners() {
                        bmp.draw_rounded(canvas, sq_r, sq_rad, alpha, true);
                        canvas.stroke_round_rect(
                            sq_r,
                            sq_rad,
                            1.0,
                            Rgba::WHITE.with_alpha(0.16 * alpha),
                        );
                    } else {
                        bmp.draw_rounded(canvas, sq_r, 0.0, alpha, false);
                    }
                } else {
                    let (crafted_ic, g0, g1) = crafted_app_style(&app.id, &app.name);
                    canvas.fill_v_gradient(sq_r, sq_rad, g0.with_alpha(alpha), g1.with_alpha(alpha));
                    canvas.stroke_round_rect(
                        sq_r,
                        sq_rad,
                        1.0,
                        Rgba::WHITE.with_alpha(0.24 * alpha),
                    );
                    icons.draw(
                        canvas,
                        crafted_ic.unwrap_or("grid"),
                        sq_r.inset(sq_sz * 0.24, sq_sz * 0.24),
                        Rgba::WHITE.with_alpha(alpha),
                        2.05,
                    );
                }

                let lbl = truncate_app_name(&app.name, 8);
                let lbl_y = ry + 6.0 + 60.0 + 6.0;
                text.draw_in_rect(
                    canvas,
                    &lbl,
                    RectF::new(cx + 2.0, lbl_y, cell_w - 4.0, 20.0),
                    12.8,
                    FontWeight::Medium,
                    pal.ink.with_alpha(0.94 * alpha),
                    Align::Center,
                );

                // Running indicator dot (`run-dot`, `#2e6bf6`) directly below label!
                let is_running = niri
                    .windows
                    .iter()
                    .any(|w| app_matches_window(app, &w.app_id));
                if is_running {
                    canvas.fill_round_rect(
                        RectF::new(cx + (cell_w - 5.0) * 0.5, lbl_y + 22.0, 5.0, 5.0),
                        2.5,
                        pal.acc.with_alpha(alpha),
                    );
                }

                let hit_cell = RectF::new(cx, ry, cell_w, cell_h);
                if let Some(clipped) = hit_cell.intersect(self.grid_clip) {
                    if clipped.h >= 12.0 {
                        self.hits.push((clipped, MenuHit::AppIdx(app_idx)));
                    }
                }
            }
        }

        canvas.set_clip_round_rect(None);
    }

    pub fn on_touch_down(&mut self, slot: i32, x: f32, y: f32) -> bool {
        let mut app_idx = None;
        for (r, hit) in self.hits.iter().rev() {
            if r.contains(x, y) {
                if let MenuHit::AppIdx(idx) = *hit {
                    if self.grid_clip.contains(x, y) {
                        app_idx = Some(idx);
                    }
                }
                break;
            }
        }
        self.press = Some(PressTrack {
            slot,
            x0: x,
            y0: y,
            scroll0: self.scroll_y,
            t0: Instant::now(),
            scrolling: false,
            long_fired: false,
            app_idx,
        });
        app_idx.is_some()
    }

    pub fn can_scroll_down_further(&self) -> bool {
        self.scroll_y < self.max_scroll - 0.5
    }

    pub fn press_origin(&self) -> Option<(f32, f32)> {
        self.press.as_ref().map(|p| (p.x0, p.y0))
    }

    pub fn is_actively_scrolling(&self) -> bool {
        self.press.as_ref().is_some_and(|p| p.scrolling)
    }

    pub fn cancel_press(&mut self) {
        self.press = None;
    }

    pub fn on_touch_motion(&mut self, slot: i32, _x: f32, y: f32) -> bool {
        let Some(p) = &mut self.press else {
            return false;
        };
        if p.slot != slot {
            return false;
        }
        let dy = y - p.y0;
        let can_scroll_dir = (dy < 0.0 && self.scroll_y < self.max_scroll - 0.5)
            || (dy > 0.0 && self.scroll_y > 0.5);
        if !p.scrolling && dy.abs() > 10.0 && can_scroll_dir && self.grid_clip.contains(p.x0, p.y0) {
            p.scrolling = true;
            p.app_idx = None;
        }
        if p.scrolling {
            self.scroll_y = (p.scroll0 - dy).clamp(0.0, self.max_scroll);
            return true;
        }
        false
    }

    pub fn on_scroll(&mut self, dy: f32) -> bool {
        if self.max_scroll <= 0.0 {
            return false;
        }
        let prev = self.scroll_y;
        self.scroll_y = (self.scroll_y + dy * 1.35).clamp(0.0, self.max_scroll);
        (self.scroll_y - prev).abs() > 0.01
    }

    pub fn on_touch_up(
        &mut self,
        slot: i32,
        x: f32,
        y: f32,
        prefs: &mut ShellPrefs,
        niri: &NiriSnapshot,
    ) -> AppMenuEvent {
        let Some(p) = self.press.take() else {
            return AppMenuEvent::None;
        };
        if p.slot != slot || p.scrolling {
            return AppMenuEvent::None;
        }
        if p.long_fired {
            return AppMenuEvent::AppLaunched;
        }
        if (x - p.x0).hypot(y - p.y0) > 18.0 {
            return AppMenuEvent::None;
        }

        for (r, hit) in self.hits.iter().rev() {
            if r.contains(x, y) {
                match *hit {
                    MenuHit::SearchBox => {
                        self.search_focused = true;
                        HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);
                        return AppMenuEvent::FocusSearchBar;
                    }
                    MenuHit::ClearQuery => {
                        self.query.clear();
                        self.scroll_y = 0.0;
                        HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);
                        return AppMenuEvent::ClearSearch;
                    }
                    MenuHit::Mode(m) => {
                        prefs.launch_mode = m;
                        prefs.save();
                        HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);
                        return AppMenuEvent::ModeChanged;
                    }
                    MenuHit::Category(cat) => {
                        self.category = cat;
                        self.query.clear();
                        self.scroll_y = 0.0;
                        HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);
                        return AppMenuEvent::ModeChanged;
                    }
                    MenuHit::AppIdx(idx) => {
                        if !self.grid_clip.contains(x, y) {
                            continue;
                        }
                        if let Some(app) = self.apps.get(idx) {
                            HapticEngine::global().pulse_norm(HapticKind::Light, x / self.last_screen_w);
                            spawn_app_with_mode(app.exec_argv.clone(), prefs.launch_mode, niri);
                            return AppMenuEvent::AppLaunched;
                        }
                    }
                }
            }
        }
        AppMenuEvent::None
    }
}

fn truncate_app_name(s: &str, max_cjk_chars: usize) -> String {
    let max_units = max_cjk_chars * 2;
    let total_units: usize = s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
    if total_units <= max_units {
        return s.to_string();
    }
    let mut used = 0usize;
    let mut sub = String::new();
    let limit = max_units.saturating_sub(2);
    for c in s.chars() {
        let w = if c.is_ascii() { 1 } else { 2 };
        if used + w > limit {
            break;
        }
        sub.push(c);
        used += w;
    }
    format!("{sub}…")
}

fn app_matches_window(app: &DesktopApp, win_app_id: &str) -> bool {
    if win_app_id.is_empty() {
        return false;
    }
    let a = app.id.to_ascii_lowercase();
    let w = win_app_id.to_ascii_lowercase();
    if a == w {
        return true;
    }
    let bin = app
        .exec_argv
        .first()
        .and_then(|s| std::path::Path::new(s).file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !bin.is_empty() && (w == bin || w.ends_with(&format!(".{bin}"))) {
        return true;
    }
    let a_tail = a.rsplit('.').next().unwrap_or(&a);
    let w_tail = w.rsplit('.').next().unwrap_or(&w);
    !a_tail.is_empty() && a_tail.len() >= 3 && a_tail == w_tail
}

/// Curated vibrant 2-stop vertical squircle gradients + crisp vector glyphs matching `index.html` `APPS`.
fn crafted_app_style(id: &str, name: &str) -> (Option<&'static str>, Rgba, Rgba) {
    let low = id.to_ascii_lowercase();
    let nlow = name.to_ascii_lowercase();
    if low.contains("foot") || low.contains("term") || low.contains("alacritty") || low.contains("kitty") {
        (Some("terminal"), Rgba::hex(0x334155), Rgba::hex(0x0f172a))
    } else if low.contains("firefox") || low.contains("chrom") || low.contains("epiphany") || low.contains("browser") {
        (Some("browser"), Rgba::hex(0x3b82f6), Rgba::hex(0x1d4ed8))
    } else if low.contains("nautilus") || low.contains("thunar") || low.contains("dolphin") || low.contains("files") {
        (Some("folder"), Rgba::hex(0x60a5fa), Rgba::hex(0x2563eb))
    } else if low.contains("waydroid") {
        (Some("grid"), Rgba::hex(0x2dd4bf), Rgba::hex(0x0f766e))
    } else if low.contains("code") || low.contains("zed") || low.contains("builder") {
        (Some("code"), Rgba::hex(0x818cf8), Rgba::hex(0x4f46e5))
    } else if low.contains("rnote") || low.contains("xournal") || low.contains("obsidian") || low.contains("note") {
        (Some("stylus"), Rgba::hex(0xfbbf24), Rgba::hex(0xd97706))
    } else if low.contains("mpv") || low.contains("vlc") || low.contains("totem") || low.contains("video") {
        (Some("video"), Rgba::hex(0xf87171), Rgba::hex(0xdc2626))
    } else if low.contains("music") || low.contains("amberol") || low.contains("rhythmbox") {
        (Some("music"), Rgba::hex(0xf472b6), Rgba::hex(0xdb2777))
    } else if low.contains("gimp") || low.contains("krita") || low.contains("inkscape") || low.contains("draw") {
        (Some("draw"), Rgba::hex(0xf43f5e), Rgba::hex(0xbe123c))
    } else if low.contains("evince") || low.contains("zathura") || low.contains("foliate") || low.contains("libreoffice") || low.contains("reader") {
        (Some("book"), Rgba::hex(0xd97706), Rgba::hex(0x92400e))
    } else if low.contains("btop") || low.contains("htop") || low.contains("monitor") {
        (Some("gauge"), Rgba::hex(0x2dd4bf), Rgba::hex(0x0f766e))
    } else if low.contains("calc") {
        (Some("calc"), Rgba::hex(0xfb923c), Rgba::hex(0xea580c))
    } else if low.contains("mail") || low.contains("thunderbird") || low.contains("geary") {
        (Some("mail"), Rgba::hex(0x38bdf8), Rgba::hex(0x0284c7))
    } else if low.contains("cal") {
        (Some("cal"), Rgba::hex(0xfb7185), Rgba::hex(0xe11d48))
    } else if low.contains("chat") || low.contains("telegram") || low.contains("wechat") || nlow.contains("聊天") {
        (Some("chat"), Rgba::hex(0x34d399), Rgba::hex(0x059669))
    } else if low.contains("photo") || low.contains("loupe") || low.contains("shotwell") || low.contains("eog") {
        (Some("image"), Rgba::hex(0xc084fc), Rgba::hex(0x7e22ce))
    } else if low.contains("fcitx") || low.contains("setting") || low.contains("control") || low.contains("nm-connection") || low.contains("pavucontrol") {
        (Some("settings"), Rgba::hex(0x94a3b8), Rgba::hex(0x475569))
    } else {
        const PALETTES: &[(u32, u32)] = &[
            (0x3b82f6, 0x1d4ed8),
            (0x34d399, 0x059669),
            (0x818cf8, 0x4f46e5),
            (0xfb923c, 0xea580c),
            (0x38bdf8, 0x0284c7),
            (0xc084fc, 0x7e22ce),
        ];
        let hash = low.bytes().fold(0usize, |acc, b| acc.wrapping_mul(31).wrapping_add(b as usize));
        let (c0, c1) = PALETTES[hash % PALETTES.len()];
        (None, Rgba::hex(c0), Rgba::hex(c1))
    }
}

fn fallback_demo_apps() -> Vec<DesktopApp> {
    let specs = [
        ("foot", "终端 Foot", "Development;TerminalEmulator;"),
        ("firefox-esr", "Firefox 浏览器", "Network;WebBrowser;"),
        ("chromium", "Chromium", "Network;WebBrowser;"),
        ("org.gnome.Nautilus", "文件管理", "System;FileManager;"),
        ("code", "VS Code", "Development;IDE;"),
        ("rnote", "Rnote 手写笔记", "Office;Graphics;"),
        ("mpv", "MPV 播放器", "AudioVideo;Video;"),
        ("obsidian", "Obsidian", "Office;TextEditor;"),
        ("gimp", "GIMP 图像创作", "Graphics;Photography;"),
        ("krita", "Krita 绘画", "Graphics;"),
        ("libreoffice-writer", "LibreOffice 文档", "Office;"),
        ("evince", "文档阅读器", "Office;"),
        ("btop", "系统监控", "System;Utility;"),
        ("pavucontrol", "音频路由", "AudioVideo;Settings;"),
        ("nm-connection-editor", "网络配置", "System;Settings;"),
    ];
    specs
        .into_iter()
        .map(|(id, name, cats)| DesktopApp {
            id: id.into(),
            name: name.into(),
            exec_argv: vec![id.into()],
            icon_name: id.into(),
            categories: cats.split(';').filter(|s| !s.is_empty()).map(String::from).collect(),
            icon: crate::sys_state::resolve_desktop_icon(id, id),
        })
        .collect()
}
