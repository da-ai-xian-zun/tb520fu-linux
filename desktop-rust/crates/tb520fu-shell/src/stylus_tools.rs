// SPDX-License-Identifier: MIT
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use cosmic_text::Align;
use tb520fu_ui::canvas::{Canvas, RectF};
use tb520fu_ui::color::{Palette, Rgba};
use tb520fu_ui::haptics::{HapticEngine, HapticKind};
use tb520fu_ui::svg_icons::IconSet;
use tb520fu_ui::text::{FontWeight, TextEngine};
use tb520fu_ui::widgets::BitmapIcon;

use crate::niri_ipc::{
    apply_split_preset_for_window, center_window_id, focus_trailing_empty_workspace,
    move_window_to_tiling_id, NiriSnapshot,
};
use crate::statusbar::BAR_HEIGHT;
use crate::sys_state::{media_cmd, ShellPrefs, SysSnapshot};

#[derive(Debug, Clone, Copy)]
pub struct WinMenuState {
    pub win_id: u64,
    pub cx: f32,
    pub ty: f32,
    pub is_floating: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    None,
    FocusQuickNoteOsk,
    SavedToast(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct StrokePt {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

#[derive(Debug, Clone)]
pub struct InkStroke {
    pub color: Rgba,
    pub base_width: f32,
    pub pts: Vec<StrokePt>,
}

const INK_COLORS: [Rgba; 5] = [
    Rgba::hex(0xff453a),
    Rgba::hex(0xff9f0a),
    Rgba::hex(0x32d74b),
    Rgba::hex(0x3b82f6),
    Rgba::WHITE,
];

const INK_WIDTHS: [f32; 3] = [2.6, 5.0, 9.0];

#[derive(Debug, Clone, Copy)]
enum ToolHit {
    // Media IslandPop
    IslandPrev,
    IslandPlayPause,
    IslandNext,
    // Quick Note
    NoteFocusInput,
    NoteClear,
    NoteClose,
    // Pen Orb
    OrbToggleMenu,
    OrbAnnot,
    OrbNote,
    OrbNewWs,
    // Window Top-Pill Liquid Glass Capsule
    WinSplitPreset { win_id: u64, pct: u8 },
    WinMoveToTiling(u64),
    WinCenter(u64),
    // Annotation Toolbar
    AnnotColor(usize),
    AnnotWidth(usize),
    AnnotUndo,
    AnnotClear,
    AnnotSave,
    AnnotClose,
}

#[derive(Debug, Clone, Copy)]
enum GutterDrag {
    None,
    NoteMove {
        slot: i32,
        dx: f32,
        dy: f32,
    },
    CornerSwipe {
        slot: i32,
        left_corner: bool,
        x0: f32,
        y0: f32,
        fired: bool,
    },
    AnnotDraw {
        slot: i32,
        last_t: Instant,
        vx: f32,
        vy: f32,
        max_press: f32,
    },
}

pub struct CompanionTools {
    // 1. Live Media Controller Popup
    pub island_open: bool,
    island_rect: RectF,

    // 2. Quick Note Pad
    pub note_open: bool,
    pub note_focused: bool,
    pub note_text: String,
    pub note_pos: (f32, f32),

    // 3. Screenshot + Stylus Annotation
    pub annot_open: bool,
    pub annot_bg: Option<BitmapIcon>,
    pub annot_strokes: Vec<InkStroke>,
    pub annot_predicted: Vec<StrokePt>,
    pub annot_color_idx: usize,
    pub annot_width_idx: usize,
    pending_save_annot: bool,

    // 4. Optional Pen Orb
    pub orb_expanded: bool,
    pub orb_pos: (f32, f32),

    // 5. Live Captions text cache
    caption_text: String,
    last_caption_poll: Option<Instant>,

    // 6. Window Top-Pill Liquid Glass Menu Popover
    pub win_menu: Option<WinMenuState>,
    win_menu_rect: RectF,

    hits: Vec<(RectF, ToolHit)>,
    note_header_rect: RectF,
    annot_bar_rect: RectF,
    last_screen_w: f32,
    drag: GutterDrag,
}

impl Default for CompanionTools {
    fn default() -> Self {
        Self::new()
    }
}

impl CompanionTools {
    pub fn new() -> Self {
        let note_text = fs::read_to_string(Self::note_path()).unwrap_or_default();
        Self {
            island_open: false,
            island_rect: RectF::default(),
            note_open: false,
            note_focused: false,
            note_text,
            note_pos: (1060.0, 88.0),
            annot_open: false,
            annot_bg: None,
            annot_strokes: Vec::new(),
            annot_predicted: Vec::new(),
            annot_color_idx: 0,
            annot_width_idx: 1,
            pending_save_annot: false,
            orb_expanded: false,
            orb_pos: (1404.0, 460.0),
            caption_text: String::new(),
            last_caption_poll: None,
            win_menu: None,
            win_menu_rect: RectF::default(),
            hits: Vec::with_capacity(32),
            note_header_rect: RectF::default(),
            annot_bar_rect: RectF::default(),
            last_screen_w: 1472.0,
            drag: GutterDrag::None,
        }
    }

    pub fn open_window_menu(&mut self, win_id: u64, cx: f32, ty: f32, is_floating: bool) {
        if self.win_menu.is_some_and(|m| m.win_id == win_id) {
            self.win_menu = None;
            return;
        }
        HapticEngine::global().pulse_norm(HapticKind::Tick, (cx / self.last_screen_w.max(1.0)).clamp(0.0, 1.0));
        self.win_menu = Some(WinMenuState {
            win_id,
            cx,
            ty,
            is_floating,
        });
    }

    fn note_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".config/tb520fu/quick_note.txt")
    }

    pub fn save_note(&self) {
        let p = Self::note_path();
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(p, &self.note_text);
    }

    pub fn start_screenshot_annotation(&mut self) {
        let tmp_png = "/tmp/tb520fu_annot_capture.png";
        let _ = fs::remove_file(tmp_png);
        let ok = Command::new("grim")
            .arg(tmp_png)
            .status()
            .is_ok_and(|s| s.success());
        self.annot_bg = if ok {
            BitmapIcon::from_photo_file(std::path::Path::new(tmp_png), 2944)
        } else {
            None
        };
        self.annot_strokes.clear();
        self.annot_predicted.clear();
        self.annot_open = true;
        self.island_open = false;
    }

    pub fn has_visible_layer(&self, prefs: &ShellPrefs, _niri: &NiriSnapshot) -> bool {
        self.annot_open
            || self.win_menu.is_some()
            || self.island_open
            || self.note_open
            || prefs.live_captions
            || prefs.pen_orb
            || prefs.pen_corners
    }

    pub fn tick(&mut self, _dt: f32, prefs: &ShellPrefs) -> bool {
        let mut rd = false;
        if prefs.live_captions
            && self
                .last_caption_poll
                .map_or(true, |t| t.elapsed() >= Duration::from_millis(650))
        {
            self.last_caption_poll = Some(Instant::now());
            let uid = unsafe { libc::getuid() };
            let cap_file = format!("/run/user/{uid}/tb520fu-captions.txt");
            let latest = fs::read_to_string(cap_file)
                .unwrap_or_default()
                .trim()
                .to_string();
            if latest != self.caption_text {
                self.caption_text = latest;
                rd = true;
            }
        }
        rd
    }

    /// Renders active desktop overlays & returns exact `wl_region` input rectangles
    /// so clicks outside handles/popups pass straight through to Niri tiled windows!
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
    ) -> Vec<RectF> {
        self.hits.clear();
        self.win_menu_rect = RectF::default();
        self.island_rect = RectF::default();

        let sw = canvas.logical_width();
        let sh = canvas.logical_height();
        self.last_screen_w = sw.max(1.0);

        let has_media = !sys.media_title.is_empty() || sys.media_playing;
        if !has_media {
            self.island_open = false;
        }

        // Auto-close window menu if target window closed or Overview opened
        if let Some(wm) = self.win_menu {
            if niri.overview || !niri.windows.iter().any(|w| w.id == wm.win_id) {
                self.win_menu = None;
            }
        }

        // Full-screen Screenshot + Stylus Annotation mode takes exclusive full-screen input
        if self.annot_open {
            self.render_annotation_mode(canvas, icons, text, pal, sw, sh);
            if self.pending_save_annot {
                self.pending_save_annot = false;
                save_annotated_canvas(canvas);
            }
            return vec![RectF::new(0.0, 0.0, sw, sh)];
        }

        canvas.clear(Rgba::TRANSPARENT);
        let mut regions: Vec<RectF> = Vec::with_capacity(12);

        // 1. Window Top-Pill Liquid Glass Capsule (anchored below tapped `36x4.5` pill)
        if let Some(wm) = self.win_menu {
            let win_opt = niri.windows.iter().find(|w| w.id == wm.win_id);
            let is_floating = win_opt.map(|w| w.is_floating).unwrap_or(wm.is_floating);
            let presets = prefs.normalized_width_presets();
            let pad_x = 9.0_f32;
            let pill_h = 34.0_f32;
            let mh = 50.0_f32; // >= MIN_BLUR_SIDE (40.0) so Niri Dual-Kawase rounded blur engages
            let pill_w = 50.0_f32;
            let pill_gap = 5.0_f32;
            let sep_span = 17.0_f32;
            let center_w = 56.0_f32;
            let mw = if is_floating {
                pad_x * 2.0 + 78.0 + 6.0 + 58.0
            } else {
                let n = presets.len() as f32;
                pad_x * 2.0 + n * pill_w + (n - 1.0).max(0.0) * pill_gap + sep_span + center_w
            };
            let mx = (wm.cx - mw * 0.5).clamp(14.0, (sw - mw - 14.0).max(14.0));
            let my = (wm.ty + 14.0).clamp((BAR_HEIGHT as f32) + 8.0, (sh - mh - 18.0).max(60.0));
            let card = RectF::new(mx, my, mw, mh);
            self.win_menu_rect = card;

            let monet = tb520fu_ui::vk_renderer::active_wallpaper_monet();
            canvas.draw_glass_shadow(card, 25.0, 16.0, 5.0, if pal.light { 0.18 } else { 0.30 });
            if pal.glass {
                let (glass_tint, glass_rim, sheen) = if pal.light {
                    (
                        Rgba::rgb(244, 247, 255)
                            .lerp(monet.accent, 0.14)
                            .with_alpha(0.54),
                        Rgba::rgba_f(255, 255, 255, 0.82),
                        0.065,
                    )
                } else {
                    (
                        Rgba::rgb(64, 72, 92)
                            .lerp(monet.accent, 0.18)
                            .with_alpha(0.46),
                        Rgba::WHITE.lerp(monet.accent, 0.22).with_alpha(0.38),
                        0.085,
                    )
                };
                canvas.fill_glass_corners_styled(
                    card,
                    [25.0; 4],
                    pal,
                    glass_tint,
                    glass_rim,
                    sheen,
                    1.0,
                );
            } else {
                canvas.fill_round_rect(card, 25.0, pal.card);
                canvas.stroke_round_rect(card, 25.0, 1.0, pal.hair);
            }
            regions.push(card);

            let pill_well = if !pal.glass {
                pal.chip
            } else if pal.light {
                Rgba::rgba_f(255, 255, 255, 0.48)
            } else {
                Rgba::rgba_f(18, 24, 38, 0.34)
            };
            let pill_rim = if !pal.glass {
                Rgba::TRANSPARENT
            } else if pal.light {
                Rgba::rgba_f(255, 255, 255, 0.64)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.18)
            };
            let row_y = my + (mh - pill_h) * 0.5;

            if is_floating {
                let mut bx = mx + pad_x;
                for (lbl, bw, hit) in [
                    ("贴回分栏", 78.0_f32, ToolHit::WinMoveToTiling(wm.win_id)),
                    ("居中", 58.0_f32, ToolHit::WinCenter(wm.win_id)),
                ] {
                    let br = RectF::new(bx, row_y, bw, pill_h);
                    canvas.fill_round_rect(br, 17.0, pill_well);
                    if pill_rim.a > 0 {
                        canvas.stroke_round_rect(br, 17.0, 1.0, pill_rim);
                    }
                    text.draw_in_rect(
                        canvas,
                        lbl,
                        br,
                        12.8,
                        FontWeight::SemiBold,
                        pal.ink,
                        Align::Center,
                    );
                    self.hits.push((br, hit));
                    bx += bw + 6.0;
                }
            } else {
                let cur_prop = win_opt
                    .filter(|w| !w.is_floating)
                    .map(|w| ((w.tile_width as f32 + 12.0) / (sw - 12.0).max(1.0)).clamp(0.15, 1.0));
                let active_idx = cur_prop.and_then(|cp| {
                    presets
                        .iter()
                        .enumerate()
                        .map(|(i, &p)| {
                            let rf = (ShellPrefs::preset_pct_to_proportion(p) / 100.0) as f32;
                            (i, (cp - rf).abs())
                        })
                        .filter(|&(_, diff)| diff <= 0.035)
                        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                        .map(|(i, _)| i)
                });

                let mut bx = mx + pad_x;
                for (bi, &pct) in presets.iter().enumerate() {
                    let br = RectF::new(bx, row_y, pill_w, pill_h);
                    let active = active_idx == Some(bi);
                    if active {
                        canvas.fill_round_rect(br, 17.0, pal.acc.with_alpha(0.92));
                        canvas.stroke_round_rect(
                            br,
                            17.0,
                            1.0,
                            Rgba::rgba_f(255, 255, 255, 0.34),
                        );
                    } else {
                        canvas.fill_round_rect(br, 17.0, pill_well);
                        if pill_rim.a > 0 {
                            canvas.stroke_round_rect(br, 17.0, 1.0, pill_rim);
                        }
                    }
                    let lbl = format!("{pct}%");
                    text.draw_in_rect(
                        canvas,
                        &lbl,
                        br,
                        12.8,
                        FontWeight::SemiBold,
                        if active { Rgba::WHITE } else { pal.ink },
                        Align::Center,
                    );
                    self.hits.push((
                        br,
                        ToolHit::WinSplitPreset {
                            win_id: wm.win_id,
                            pct,
                        },
                    ));
                    bx += pill_w + pill_gap;
                }

                let sep_x = bx - pill_gap + (sep_span - 1.0) * 0.5;
                canvas.fill_rect(
                    RectF::new(
                        sep_x,
                        my + (mh - 20.0) * 0.5,
                        1.0,
                        20.0,
                    ),
                    if pal.light {
                        Rgba::rgba_f(0, 0, 0, 0.12)
                    } else {
                        Rgba::rgba_f(255, 255, 255, 0.22)
                    },
                );
                let center_r = RectF::new(bx - pill_gap + sep_span, row_y, center_w, pill_h);
                canvas.fill_round_rect(center_r, 17.0, pill_well);
                if pill_rim.a > 0 {
                    canvas.stroke_round_rect(center_r, 17.0, 1.0, pill_rim);
                }
                text.draw_in_rect(
                    canvas,
                    "居中",
                    center_r,
                    12.8,
                    FontWeight::SemiBold,
                    pal.ink,
                    Align::Center,
                );
                self.hits.push((center_r, ToolHit::WinCenter(wm.win_id)));
            }
        }

        // 2. Luminous Monet Liquid Glass Media Controller Card (only when media is active — zero timer!)
        if self.island_open && has_media {
            let iw = 372.0_f32;
            let ih = 76.0_f32;
            let card = RectF::new((sw - iw) * 0.5, (BAR_HEIGHT as f32) + 8.0, iw, ih);
            self.island_rect = card;

            let monet = tb520fu_ui::vk_renderer::active_wallpaper_monet();
            canvas.draw_glass_shadow(card, 26.0, 18.0, 5.0, if pal.light { 0.18 } else { 0.30 });
            if pal.glass {
                let (glass_tint, glass_rim, sheen) = if pal.light {
                    (
                        Rgba::rgb(244, 247, 255)
                            .lerp(monet.accent, 0.14)
                            .with_alpha(0.56),
                        Rgba::rgba_f(255, 255, 255, 0.82),
                        0.065,
                    )
                } else {
                    (
                        Rgba::rgb(64, 72, 92)
                            .lerp(monet.accent, 0.18)
                            .with_alpha(0.46),
                        Rgba::WHITE.lerp(monet.accent, 0.22).with_alpha(0.38),
                        0.085,
                    )
                };
                canvas.fill_glass_corners_styled(
                    card,
                    [26.0; 4],
                    pal,
                    glass_tint,
                    glass_rim,
                    sheen,
                    1.0,
                );
            } else {
                canvas.fill_round_rect(card, 26.0, pal.card);
                canvas.stroke_round_rect(card, 26.0, 1.0, pal.hair);
            }
            regions.push(RectF::new(0.0, 0.0, sw, sh));

            let art_r = RectF::new(card.x + 14.0, card.y + (ih - 48.0) * 0.5, 48.0, 48.0);
            canvas.fill_v_gradient(
                art_r,
                15.0,
                Rgba::hex(0x6366f1),
                Rgba::hex(0xec4899),
            );
            canvas.fill_radial_glow(
                art_r,
                15.0,
                art_r.x + art_r.w * 0.85,
                art_r.y + art_r.h * 0.88,
                art_r.w * 0.85,
                Rgba::hex(0xfb923c).with_alpha(0.70),
            );
            icons.draw(
                canvas,
                "music",
                art_r.inset(13.5, 13.5),
                Rgba::WHITE.with_alpha(0.95),
                1.95,
            );

            let m_title = if sys.media_title.is_empty() {
                "正在播放"
            } else {
                sys.media_title.as_str()
            };
            let m_sub = if sys.media_artist.is_empty() {
                if sys.media_playing {
                    "正在播放"
                } else {
                    "已暂停"
                }
            } else {
                sys.media_artist.as_str()
            };
            let text_x = art_r.x + art_r.w + 12.0;
            let text_w = (card.w - (text_x - card.x) - 134.0).max(60.0);
            text.draw_in_rect(
                canvas,
                m_title,
                RectF::new(text_x, card.y + 17.0, text_w, 22.0),
                14.0,
                FontWeight::SemiBold,
                pal.ink,
                Align::Left,
            );
            text.draw_in_rect(
                canvas,
                m_sub,
                RectF::new(text_x, card.y + 40.0, text_w, 18.0),
                12.2,
                FontWeight::Normal,
                pal.ink2,
                Align::Left,
            );

            let pill_well = if !pal.glass {
                pal.chip
            } else if pal.light {
                Rgba::rgba_f(255, 255, 255, 0.48)
            } else {
                Rgba::rgba_f(18, 24, 38, 0.34)
            };
            let pill_rim = if !pal.glass {
                Rgba::TRANSPARENT
            } else if pal.light {
                Rgba::rgba_f(255, 255, 255, 0.64)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.18)
            };

            let mut bx = card.x + card.w - 126.0;
            for (ic, hit, is_main) in [
                ("skip_prev", ToolHit::IslandPrev, false),
                (
                    if sys.media_playing { "pause" } else { "play" },
                    ToolHit::IslandPlayPause,
                    true,
                ),
                ("skip_next", ToolHit::IslandNext, false),
            ] {
                let bsz = if is_main { 38.0_f32 } else { 34.0_f32 };
                let br = RectF::new(bx, card.y + (ih - bsz) * 0.5, bsz, bsz);
                if is_main {
                    canvas.fill_round_rect(br, bsz * 0.5, pal.acc.with_alpha(0.92));
                    canvas.stroke_round_rect(
                        br,
                        bsz * 0.5,
                        1.0,
                        Rgba::rgba_f(255, 255, 255, 0.32),
                    );
                } else {
                    canvas.fill_round_rect(br, bsz * 0.5, pill_well);
                    if pill_rim.a > 0 {
                        canvas.stroke_round_rect(br, bsz * 0.5, 1.0, pill_rim);
                    }
                }
                icons.draw(
                    canvas,
                    ic,
                    br.inset(if is_main { 10.0 } else { 8.5 }, if is_main { 10.0 } else { 8.5 }),
                    if is_main { Rgba::WHITE } else { pal.ink },
                    1.95,
                );
                self.hits.push((br, hit));
                bx += bsz + 5.0;
            }
        }

        // 3. Quick Note Floating Liquid Glass Pad
        if self.note_open {
            let nw = 368.0_f32;
            let nh = 264.0_f32;
            let nx = self.note_pos.0.clamp(16.0, (sw - nw - 16.0).max(16.0));
            let ny = self.note_pos.1.clamp((BAR_HEIGHT as f32) + 12.0, (sh - nh - 16.0).max(60.0));
            let card = RectF::new(nx, ny, nw, nh);
            self.note_header_rect = RectF::new(nx, ny, nw - 84.0, 44.0);

            let monet = tb520fu_ui::vk_renderer::active_wallpaper_monet();
            canvas.draw_glass_shadow(card, 24.0, 18.0, 6.0, if pal.light { 0.18 } else { 0.32 });
            if pal.glass {
                let (glass_tint, glass_rim, sheen) = if pal.light {
                    (
                        Rgba::rgb(244, 247, 255)
                            .lerp(monet.accent, 0.14)
                            .with_alpha(0.58),
                        Rgba::rgba_f(255, 255, 255, 0.82),
                        0.065,
                    )
                } else {
                    (
                        Rgba::rgb(64, 72, 92)
                            .lerp(monet.accent, 0.18)
                            .with_alpha(0.48),
                        Rgba::WHITE.lerp(monet.accent, 0.22).with_alpha(0.38),
                        0.085,
                    )
                };
                canvas.fill_glass_corners_styled(
                    card,
                    [24.0; 4],
                    pal,
                    glass_tint,
                    glass_rim,
                    sheen,
                    1.0,
                );
            } else {
                canvas.fill_round_rect(card, 24.0, pal.card);
                canvas.stroke_round_rect(card, 24.0, 1.0, pal.hair);
            }
            regions.push(card);

            icons.draw(
                canvas,
                "note",
                RectF::new(nx + 16.0, ny + 13.0, 18.0, 18.0),
                pal.active_drawer_ink,
                2.0,
            );
            text.draw_in_rect(
                canvas,
                "便签",
                RectF::new(nx + 40.0, ny + 10.0, 120.0, 24.0),
                14.5,
                FontWeight::SemiBold,
                pal.ink,
                Align::Left,
            );

            let clr_r = RectF::new(nx + nw - 74.0, ny + 9.0, 28.0, 28.0);
            icons.draw(canvas, "trash", clr_r.inset(6.0, 6.0), pal.ink3, 1.9);
            self.hits.push((clr_r, ToolHit::NoteClear));

            let cls_r = RectF::new(nx + nw - 38.0, ny + 9.0, 28.0, 28.0);
            icons.draw(canvas, "close", cls_r.inset(6.0, 6.0), pal.ink3, 1.9);
            self.hits.push((cls_r, ToolHit::NoteClose));

            let body_r = RectF::new(nx + 14.0, ny + 46.0, nw - 28.0, nh - 60.0);
            canvas.fill_round_rect(body_r, 14.0, pal.card);
            canvas.stroke_round_rect(
                body_r,
                14.0,
                if self.note_focused { 1.4 } else { 1.0 },
                if self.note_focused { pal.acc } else { pal.hair },
            );
            let disp = if self.note_text.is_empty() && !self.note_focused {
                "轻触记录便签…".to_string()
            } else if self.note_focused {
                format!("{}|", self.note_text)
            } else {
                self.note_text.clone()
            };
            let note_ink = if self.note_text.is_empty() && !self.note_focused {
                pal.ink2
            } else {
                pal.ink
            };
            let inner_note = body_r.inset(12.0, 10.0);
            for (li, line) in disp.split('\n').take(7).enumerate() {
                let ly = inner_note.y + (li as f32) * 21.0;
                if ly + 18.0 > inner_note.y + inner_note.h + 4.0 {
                    break;
                }
                text.draw_in_rect(
                    canvas,
                    line,
                    RectF::new(inner_note.x, ly, inner_note.w, 20.0),
                    13.8,
                    FontWeight::Normal,
                    note_ink,
                    Align::Left,
                );
            }
            self.hits.push((body_r, ToolHit::NoteFocusInput));
        }

        // 4. Live Captions Bar (bottom centered Liquid Glass capsule)
        if prefs.live_captions {
            let cw = 640.0_f32.min(sw - 64.0);
            let ch = 48.0_f32;
            let cr = RectF::new((sw - cw) * 0.5, sh - ch - 18.0, cw, ch);
            canvas.draw_glass_shadow(cr, 24.0, 12.0, 4.0, 0.30);
            canvas.fill_glass_panel(cr, 24.0, pal, 0.96);
            icons.draw(
                canvas,
                "captions",
                RectF::new(cr.x + 16.0, cr.y + 14.0, 20.0, 20.0),
                pal.active_drawer_ink,
                2.0,
            );
            let sub_txt = if self.caption_text.is_empty() {
                "实时字幕已开启"
            } else {
                self.caption_text.as_str()
            };
            text.draw_in_rect(
                canvas,
                sub_txt,
                RectF::new(cr.x + 44.0, cr.y, cr.w - 60.0, cr.h),
                14.5,
                FontWeight::Medium,
                pal.ink,
                Align::Left,
            );
        }

        // 5. Optional Stylus Floating Orb (`prefs.pen_orb` — OFF by default)
        if prefs.pen_orb {
            let ox = self.orb_pos.0.clamp(14.0, sw - 58.0);
            let oy = self.orb_pos.1.clamp(72.0, sh - 80.0);
            let orb = RectF::new(ox, oy, 44.0, 44.0);
            canvas.draw_glass_shadow(orb, 22.0, 12.0, 4.0, 0.34);
            canvas.fill_glass_panel(orb, 22.0, pal, 1.0);
            icons.draw(
                canvas,
                "stylus",
                orb.inset(11.0, 11.0),
                pal.active_drawer_ink,
                2.0,
            );
            self.hits.push((orb, ToolHit::OrbToggleMenu));
            regions.push(orb);

            if self.orb_expanded {
                let menu = RectF::new((ox - 162.0).max(16.0), oy - 4.0, 154.0, 52.0);
                canvas.fill_glass_panel(menu, 26.0, pal, 1.0);
                regions.push(menu);
                for (bi, (ic, hit)) in [
                    ("screenshot", ToolHit::OrbAnnot),
                    ("note", ToolHit::OrbNote),
                    ("plus_ws", ToolHit::OrbNewWs),
                ]
                .into_iter()
                .enumerate()
                {
                    let br = RectF::new(menu.x + 9.0 + (bi as f32) * 47.0, menu.y + 6.0, 40.0, 40.0);
                    canvas.fill_round_rect(br, 20.0, pal.chip);
                    icons.draw(canvas, ic, br.inset(10.0, 10.0), pal.icon2, 2.0);
                    self.hits.push((br, hit));
                }
            }
        }

        // 6. Optional Bottom Corner Gestures (`prefs.pen_corners` — OFF by default)
        if prefs.pen_corners {
            let l_zone = RectF::new(0.0, sh - 34.0, 56.0, 34.0);
            let r_zone = RectF::new(sw - 56.0, sh - 34.0, 56.0, 34.0);
            regions.push(l_zone);
            regions.push(r_zone);
        }

        if self.win_menu.is_some() {
            return vec![RectF::new(0.0, 0.0, sw, sh)];
        }

        regions
    }

    fn render_annotation_mode(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        pal: &Palette,
        sw: f32,
        sh: f32,
    ) {
        let full = RectF::new(0.0, 0.0, sw, sh);
        if let Some(bg) = &self.annot_bg {
            canvas.clear(Rgba::BLACK);
            bg.draw_cover(canvas, full, 1.0);
        } else {
            canvas.clear(Rgba::hex(0x0f1626));
            canvas.draw_wallpaper_background(full, 1.0);
        }

        // Render all committed + active strokes + motion-predicted tip!
        for stroke in &self.annot_strokes {
            draw_ink_polyline(canvas, &stroke.pts, stroke.color, stroke.base_width);
        }
        if let Some(cur) = self.annot_strokes.last() {
            if !self.annot_predicted.is_empty() && !cur.pts.is_empty() {
                let mut chain = Vec::with_capacity(self.annot_predicted.len() + 1);
                chain.push(*cur.pts.last().unwrap());
                chain.extend_from_slice(&self.annot_predicted);
                draw_ink_polyline(canvas, &chain, cur.color, cur.base_width);
            }
        }

        // Floating Liquid Glass Annotation Toolbar (skipped on the single frame when saving PNG!)
        if self.pending_save_annot {
            return;
        }
        let bw = 544.0_f32.min(sw - 32.0);
        let bh = 54.0_f32;
        let bar = RectF::new((sw - bw) * 0.5, sh - bh - 22.0, bw, bh);
        self.annot_bar_rect = bar;

        canvas.draw_glass_shadow(bar, 27.0, 18.0, 6.0, 0.42);
        canvas.fill_glass_panel(bar, 27.0, pal, 1.0);

        let mut x = bar.x + 16.0;
        let cy = bar.y + bh * 0.5;
        for (ci, &col) in INK_COLORS.iter().enumerate() {
            let cr = RectF::new(x, cy - 13.0, 26.0, 26.0);
            canvas.fill_round_rect(cr, 13.0, col);
            if self.annot_color_idx == ci {
                canvas.stroke_round_rect(cr.inset(-3.0, -3.0), 16.0, 2.0, Rgba::WHITE);
            }
            self.hits.push((cr.inset(-4.0, -4.0), ToolHit::AnnotColor(ci)));
            x += 34.0;
        }

        x += 8.0;
        for (wi, &w_px) in INK_WIDTHS.iter().enumerate() {
            let wr = RectF::new(x, cy - 15.0, 30.0, 30.0);
            if self.annot_width_idx == wi {
                canvas.fill_round_rect(wr, 15.0, pal.acc_soft);
            }
            let dot = (w_px * 1.6).clamp(5.0, 15.0);
            canvas.fill_round_rect(
                RectF::new(wr.x + (30.0 - dot) * 0.5, wr.y + (30.0 - dot) * 0.5, dot, dot),
                dot * 0.5,
                pal.ink,
            );
            self.hits.push((wr, ToolHit::AnnotWidth(wi)));
            x += 34.0;
        }

        x += 10.0;
        for (ic, lbl, hit, accent) in [
            (Some("undo"), "", ToolHit::AnnotUndo, false),
            (Some("trash"), "", ToolHit::AnnotClear, false),
            (None, "保存", ToolHit::AnnotSave, true),
            (Some("close"), "", ToolHit::AnnotClose, false),
        ] {
            let kw = if accent { 64.0 } else { 36.0 };
            let kr = RectF::new(x, cy - 18.0, kw, 36.0);
            canvas.fill_round_rect(kr, 18.0, if accent { pal.acc } else { pal.chip });
            if let Some(icon_name) = ic {
                icons.draw(canvas, icon_name, kr.inset(9.0, 9.0), pal.icon2, 2.0);
            } else {
                text.draw_in_rect(
                    canvas,
                    lbl,
                    kr,
                    13.2,
                    FontWeight::SemiBold,
                    Rgba::WHITE,
                    Align::Center,
                );
            }
            self.hits.push((kr, hit));
            x += kw + 8.0;
        }
    }

    pub fn on_touch_down(
        &mut self,
        slot: i32,
        x: f32,
        y: f32,
        sw: f32,
        sh: f32,
        prefs: &ShellPrefs,
    ) -> bool {
        if self.annot_open {
            if self.annot_bar_rect.contains(x, y) {
                return true;
            }
            let col = INK_COLORS[self.annot_color_idx];
            let bw = INK_WIDTHS[self.annot_width_idx];
            self.annot_strokes.push(InkStroke {
                color: col,
                base_width: bw,
                pts: vec![StrokePt {
                    x,
                    y,
                    pressure: 0.75,
                }],
            });
            self.annot_predicted.clear();
            self.drag = GutterDrag::AnnotDraw {
                slot,
                last_t: Instant::now(),
                vx: 0.0,
                vy: 0.0,
                max_press: 0.75,
            };
            return true;
        }

        // Dismiss Media IslandPop on outside tap, or capture inside tap for button release
        if self.island_open {
            if self.island_rect.contains(x, y) {
                return true;
            }
            self.island_open = false;
            if !(self.win_menu.is_some() && self.win_menu_rect.contains(x, y))
                && !(self.note_open && self.note_header_rect.contains(x, y))
                && !self.hits.iter().any(|(r, _)| r.contains(x, y))
            {
                return true;
            }
        }

        // Dismiss Window Pill Menu on outside tap, or capture inside tap for button release
        if self.win_menu.is_some() {
            if self.win_menu_rect.contains(x, y) {
                return true;
            }
            self.win_menu = None;
            if !(self.note_open && self.note_header_rect.contains(x, y))
                && !self.hits.iter().any(|(r, _)| r.contains(x, y))
            {
                return true;
            }
        }

        // Check Quick Note drag header
        if self.note_open && self.note_header_rect.contains(x, y) {
            self.drag = GutterDrag::NoteMove {
                slot,
                dx: x - self.note_pos.0,
                dy: y - self.note_pos.1,
            };
            return true;
        }

        // Check Optional Bottom Corner Gestures
        if prefs.pen_corners && y >= sh - 40.0 && (x <= 60.0 || x >= sw - 60.0) {
            self.drag = GutterDrag::CornerSwipe {
                slot,
                left_corner: x <= 60.0,
                x0: x,
                y0: y,
                fired: false,
            };
            return true;
        }

        self.hits.iter().any(|(r, _)| r.contains(x, y))
    }

    pub fn on_touch_motion(&mut self, slot: i32, x: f32, y: f32) -> bool {
        match &mut self.drag {
            GutterDrag::AnnotDraw {
                slot: s,
                last_t,
                vx,
                vy,
                max_press,
            } if *s == slot => {
                let now = Instant::now();
                let dt = now.duration_since(*last_t).as_secs_f32().clamp(0.001, 0.05);
                *last_t = now;
                if let Some(stroke) = self.annot_strokes.last_mut() {
                    let prev = *stroke.pts.last().unwrap();
                    let dx = x - prev.x;
                    let dy = y - prev.y;
                    if dx.hypot(dy) < 0.8 {
                        return false;
                    }
                    *vx = *vx * 0.45 + (dx / dt) * 0.55;
                    *vy = *vy * 0.45 + (dy / dt) * 0.55;
                    let speed = vx.hypot(*vy);
                    let press = (0.92 - (speed / 3600.0) * 0.35).clamp(0.35, 1.0);
                    *max_press = max_press.max(press);
                    stroke.pts.push(StrokePt {
                        x,
                        y,
                        pressure: press,
                    });

                    // 2-step velocity motion prediction (`8.3ms` lookahead @ 120Hz), guarded against lift-tail hooks!
                    self.annot_predicted.clear();
                    if press * 3.0 >= *max_press * 2.0 && speed > 45.0 {
                        for step in 1..=2 {
                            let look_t = (step as f32) * 0.0042;
                            self.annot_predicted.push(StrokePt {
                                x: x + *vx * look_t,
                                y: y + *vy * look_t,
                                pressure: press * 0.9,
                            });
                        }
                    }
                }
                return true;
            }
            GutterDrag::NoteMove { slot: s, dx, dy } if *s == slot => {
                self.note_pos = (x - *dx, y - *dy);
                return true;
            }
            GutterDrag::CornerSwipe {
                slot: s,
                left_corner,
                x0,
                y0,
                fired,
            } if *s == slot && !*fired => {
                let dy = *y0 - y;
                let dx = if *left_corner { x - *x0 } else { *x0 - x };
                if dy > 36.0 && dx > 24.0 {
                    *fired = true;
                    HapticEngine::global().pulse_norm(HapticKind::Medium, if *left_corner { 0.1 } else { 0.9 });
                    if *left_corner {
                        self.note_open = true;
                        self.note_focused = true;
                    } else {
                        self.start_screenshot_annotation();
                    }
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    pub fn on_touch_up(
        &mut self,
        slot: i32,
        x: f32,
        y: f32,
        sys: &mut SysSnapshot,
        niri: &NiriSnapshot,
    ) -> ToolEffect {
        let g = std::mem::replace(&mut self.drag, GutterDrag::None);
        match g {
            GutterDrag::AnnotDraw { slot: s, .. } if s == slot => {
                self.annot_predicted.clear();
                return ToolEffect::None;
            }
            GutterDrag::NoteMove { slot: s, .. } if s == slot => {
                return ToolEffect::None;
            }
            GutterDrag::CornerSwipe { slot: s, fired, left_corner, .. } if s == slot => {
                if fired && left_corner {
                    return ToolEffect::FocusQuickNoteOsk;
                }
                return ToolEffect::None;
            }
            _ => {}
        }

        let mut matched: Option<ToolHit> = None;
        for &(r, hit) in self.hits.iter().rev() {
            if r.contains(x, y) {
                matched = Some(hit);
                break;
            }
        }
        let Some(hit) = matched else {
            return ToolEffect::None;
        };
        HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_screen_w);

        match hit {
            ToolHit::IslandPrev => media_cmd("previous"),
            ToolHit::IslandPlayPause => {
                sys.media_playing = !sys.media_playing;
                media_cmd("play-pause");
            }
            ToolHit::IslandNext => media_cmd("next"),
            ToolHit::NoteFocusInput => {
                self.note_focused = true;
                return ToolEffect::FocusQuickNoteOsk;
            }
            ToolHit::NoteClear => {
                self.note_text.clear();
                self.save_note();
            }
            ToolHit::NoteClose => {
                self.note_open = false;
                self.note_focused = false;
                self.save_note();
            }
            ToolHit::OrbToggleMenu => {
                self.orb_expanded = !self.orb_expanded;
            }
            ToolHit::OrbAnnot => {
                self.orb_expanded = false;
                self.start_screenshot_annotation();
            }
            ToolHit::OrbNote => {
                self.orb_expanded = false;
                self.note_open = true;
                self.note_focused = true;
                return ToolEffect::FocusQuickNoteOsk;
            }
            ToolHit::OrbNewWs => {
                self.orb_expanded = false;
                focus_trailing_empty_workspace(niri);
            }
            ToolHit::WinSplitPreset { win_id, pct } => {
                self.win_menu = None;
                let prop = ShellPrefs::preset_pct_to_proportion(pct);
                apply_split_preset_for_window(niri, win_id, prop);
            }
            ToolHit::WinMoveToTiling(win_id) => {
                self.win_menu = None;
                move_window_to_tiling_id(win_id);
            }
            ToolHit::WinCenter(win_id) => {
                self.win_menu = None;
                center_window_id(win_id);
            }
            ToolHit::AnnotColor(ci) => {
                self.annot_color_idx = ci;
            }
            ToolHit::AnnotWidth(wi) => {
                self.annot_width_idx = wi;
            }
            ToolHit::AnnotUndo => {
                self.annot_strokes.pop();
            }
            ToolHit::AnnotClear => {
                self.annot_strokes.clear();
            }
            ToolHit::AnnotSave => {
                self.pending_save_annot = true;
                return ToolEffect::SavedToast("批注已保存至 ~/Pictures");
            }
            ToolHit::AnnotClose => {
                self.annot_open = false;
                self.annot_strokes.clear();
                self.annot_bg = None;
            }
        }
        ToolEffect::None
    }
}

fn draw_ink_polyline(canvas: &mut Canvas, pts: &[StrokePt], color: Rgba, base_w: f32) {
    if pts.is_empty() {
        return;
    }
    if pts.len() == 1 {
        let p = pts[0];
        let r = (base_w * p.pressure).max(1.5);
        canvas.fill_round_rect(RectF::new(p.x - r * 0.5, p.y - r * 0.5, r, r), r * 0.5, color);
        return;
    }
    for pair in pts.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let w = (base_w * (a.pressure + b.pressure) * 0.5).max(1.4);
        canvas.draw_line(a.x, a.y, b.x, b.y, w, color);
    }
}

fn save_annotated_canvas(canvas: &Canvas) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = format!("{home}/Pictures");
    let _ = fs::create_dir_all(&dir);
    let mut now: libc::time_t = 0;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::time(&mut now);
        libc::localtime_r(&now, &mut tm);
    }
    let fname = format!(
        "{dir}/Annot_{:04}{:02}{:02}_{:02}{:02}{:02}.png",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    );
    let _ = canvas.save_png(&fname);
    // Close annotation overlay after saving
}
