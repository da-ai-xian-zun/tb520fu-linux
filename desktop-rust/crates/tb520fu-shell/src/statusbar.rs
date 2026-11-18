// SPDX-License-Identifier: MIT
use std::time::{Duration, Instant};

use cosmic_text::Align;
use tb520fu_ui::canvas::{Canvas, RectF};
use tb520fu_ui::color::{Palette, Rgba};
use tb520fu_ui::haptics::{HapticEngine, HapticKind};
use tb520fu_ui::spring::Spring1D;
use tb520fu_ui::svg_icons::IconSet;
use tb520fu_ui::text::{FontWeight, TextEngine};

use crate::niri_ipc::{focus_workspace_idx, toggle_overview, NiriSnapshot};
use crate::sys_state::{ShellPrefs, SysSnapshot};

/// Sleek iPadOS / ZUI-grade Top Status Bar (`36` logical px -> `72` physical px @ `2.0x`).
pub const BAR_HEIGHT: u32 = 36;

/// Unified TopBar geometry, typography & icon tokens (bold high-contrast single scale across Left, Center, Right).
const BAR_EDGE_PAD: f32 = 16.0;
const BAR_FONT: f32 = 13.4;
const BAR_ICON: f32 = 16.8;
const BAR_STROKE: f32 = 2.45;
const BAR_CAPSULE_H: f32 = 25.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullWing {
    Left,
    #[default]
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarAction {
    None,
    OpenConsole { wing: PullWing },
    DragConsole { wing: PullWing, finger_y: f32 },
    FinishPull { wing: PullWing, vy: f32 },
    ToggleIslandPop,
    ScrubOsd { is_brightness: bool, pct: u8 },
}

#[derive(Debug, Clone, Copy)]
enum CenterHit {
    Workspace(u32),
    Overview,
    IslandPop,
}

#[derive(Debug, Clone, Copy)]
struct BarGesture {
    slot: i32,
    x0: f32,
    y0: f32,
    last_y: f32,
    last_t: Instant,
    vy: f32,
    in_center: bool,
    wing: PullWing,
    pulling: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OsdKind {
    Volume { muted: bool },
    Brightness,
    MicMute { muted: bool },
    Toast,
}

struct CapsuleOsd {
    kind: OsdKind,
    icon: &'static str,
    label: String,
    has_slider: bool,
    expires_at: Option<Instant>,
    dragging_slot: Option<i32>,
    capsule_rect: RectF,
    track_rect: RectF,
}

impl Default for CapsuleOsd {
    fn default() -> Self {
        Self {
            kind: OsdKind::Volume { muted: false },
            icon: "sound",
            label: String::new(),
            has_slider: true,
            expires_at: None,
            dragging_slot: None,
            capsule_rect: RectF::new(0.0, 0.0, 0.0, 0.0),
            track_rect: RectF::new(0.0, 0.0, 180.0, 8.0),
        }
    }
}

pub struct StatusBar {
    hits: Vec<(RectF, CenterHit)>,
    center_span: (f32, f32),
    last_width: f32,
    gesture: Option<BarGesture>,
    anim_phase: f32,
    /// `0.0` = 100% transparent over Empty Home / Overview; `1.0` = neutral frosted glass when windows are open.
    pub glass_reveal: Spring1D,
    /// `0.0` = hidden / compact Media capsule; `1.0` = expanded Volume / Brightness / Mute / Toast OSD capsule.
    pub osd_reveal: Spring1D,
    /// Smooth continuous `0.0..=1.0` slider value inside the expanded TopBar OSD capsule.
    pub osd_val: Spring1D,
    osd: CapsuleOsd,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            hits: Vec::with_capacity(12),
            center_span: (0.0, 0.0),
            last_width: 1472.0,
            gesture: None,
            anim_phase: 0.0,
            glass_reveal: Spring1D::normalized(0.0, 18.0, 1.0),
            osd_reveal: Spring1D::normalized(0.0, 26.0, 0.94),
            osd_val: Spring1D::normalized(0.5, 32.0, 1.0),
            osd: CapsuleOsd::default(),
        }
    }

    pub fn show_volume_osd(&mut self, pct: u8, muted: bool) {
        let frac = (pct.min(100) as f32) / 100.0;
        let was_open =
            self.osd_reveal.pos > 0.15 && matches!(self.osd.kind, OsdKind::Volume { .. });
        self.osd.kind = OsdKind::Volume { muted };
        self.osd.icon = if muted || pct == 0 {
            "volume_off"
        } else {
            "sound"
        };
        self.osd.label = if muted {
            "静音".into()
        } else {
            format!("{}%", pct.min(100))
        };
        self.osd.has_slider = true;
        self.osd.expires_at = Some(Instant::now() + Duration::from_millis(1550));
        if was_open {
            self.osd_val.set_target(frac);
        } else {
            self.osd_val.snap(frac);
        }
        self.osd_reveal.set_target(1.0);
    }

    pub fn show_brightness_osd(&mut self, pct: u8) {
        let p = pct.clamp(1, 100);
        let frac = (p as f32) / 100.0;
        let was_open = self.osd_reveal.pos > 0.15 && self.osd.kind == OsdKind::Brightness;
        self.osd.kind = OsdKind::Brightness;
        self.osd.icon = "sun";
        self.osd.label = format!("{p}%");
        self.osd.has_slider = true;
        self.osd.expires_at = Some(Instant::now() + Duration::from_millis(1550));
        if was_open {
            self.osd_val.set_target(frac);
        } else {
            self.osd_val.snap(frac);
        }
        self.osd_reveal.set_target(1.0);
    }

    pub fn show_mic_osd(&mut self, muted: bool) {
        self.osd.kind = OsdKind::MicMute { muted };
        self.osd.icon = if muted { "mic_off" } else { "mic" };
        self.osd.label = if muted {
            "麦克风已静音".into()
        } else {
            "麦克风已开启".into()
        };
        self.osd.has_slider = false;
        self.osd.dragging_slot = None;
        self.osd.expires_at = Some(Instant::now() + Duration::from_millis(1500));
        self.osd_reveal.set_target(1.0);
    }

    pub fn show_toast_osd(&mut self, icon: &'static str, label: impl Into<String>) {
        self.osd.kind = OsdKind::Toast;
        self.osd.icon = icon;
        self.osd.label = label.into();
        self.osd.has_slider = false;
        self.osd.dragging_slot = None;
        self.osd.expires_at = Some(Instant::now() + Duration::from_millis(1750));
        self.osd_reveal.set_target(1.0);
    }

    pub fn has_active_windows(niri: &NiriSnapshot) -> bool {
        if niri.overview {
            return false;
        }
        let out_name = niri.primary_output_name();
        niri.active_workspace_on(out_name)
            .map_or(false, |ws| !niri.columns_of(ws.id).is_empty())
    }

    pub fn sync_target(&mut self, niri: &NiriSnapshot) {
        let tgt = if Self::has_active_windows(niri) {
            1.0
        } else {
            0.0
        };
        self.glass_reveal.set_target(tgt);
    }

    pub fn is_animating(&self) -> bool {
        self.glass_reveal.is_animating()
            || self.osd_reveal.is_animating()
            || self.osd_val.is_animating()
    }

    pub fn has_pending_osd_timeout(&self) -> bool {
        self.osd.dragging_slot.is_none()
            && self.osd.expires_at.is_some()
            && self.osd_reveal.target > 0.0
    }

    pub fn step(&mut self, dt: f32, playing: bool, niri: &NiriSnapshot) -> bool {
        self.sync_target(niri);
        let mut rd = self.glass_reveal.step(dt);
        if self.osd.dragging_slot.is_none() {
            if let Some(exp) = self.osd.expires_at {
                if Instant::now() >= exp {
                    self.osd.expires_at = None;
                    self.osd_reveal.set_target(0.0);
                    rd = true;
                }
            }
        }
        if self.osd_reveal.step(dt) {
            rd = true;
        }
        if self.osd_val.step(dt) {
            rd = true;
        }
        if playing {
            self.anim_phase = (self.anim_phase + dt * 5.2) % std::f32::consts::TAU;
            rd = true;
        }
        rd
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
        unread_notifs: usize,
    ) {
        self.hits.clear();
        self.osd.capsule_rect = RectF::default();
        let w = canvas.logical_width();
        let h = canvas.logical_height();
        self.last_width = w.max(1.0);
        let out_name = niri.primary_output_name();

        // 1. Genuine Liquid Glass Backdrop (`MODE_FAKE_GLASS` over `tex_glass` Dual-Kawase wallpaper):
        //    - TopBar has `exclusive_zone = 36`, so tiled windows never sit behind it — only the static
        //      wallpaper is behind TopBar. Sampling `tex_glass` gives authentic 300px frosted refraction,
        //      top specular sheen, and crisp bottom prism highlight at `0.00 ms` compositor blur cost!
        canvas.clear(Rgba::TRANSPARENT);
        let monet = tb520fu_ui::vk_renderer::active_wallpaper_monet();
        let g = self.glass_reveal.pos.clamp(0.0, 1.0);
        if pal.glass {
            let (glass_tint, sheen, prism_hi, prism_shadow) = if pal.light {
                // Crystal milk-glass: 18%–28% veil over normal/bright wallpapers (slightly firmer over pitch-black wallpapers)
                let base_a = (0.18 + 0.08 * g + (0.45 - monet.top_luma).max(0.0) * 0.34).clamp(0.18, 0.46);
                (
                    Rgba::rgba_f(255, 255, 255, base_a),
                    0.095 + 0.025 * g,
                    Rgba::rgba_f(255, 255, 255, 0.48 + 0.14 * g),
                    Rgba::rgba_f(12, 18, 30, 0.06 + 0.05 * g),
                )
            } else {
                // Smoked crystal-glass: 20%–32% veil over dark/medium wallpapers (slightly firmer over snow-bright wallpapers)
                let base_a = (0.20 + 0.10 * g + (monet.top_luma - 0.55).max(0.0) * 0.42).clamp(0.20, 0.48);
                (
                    Rgba::rgba_f(10, 14, 24, base_a),
                    0.065 + 0.025 * g,
                    Rgba::rgba_f(255, 255, 255, 0.18 + 0.08 * g),
                    Rgba::rgba_f(4, 8, 16, 0.18 + 0.10 * g),
                )
            };
            canvas.fill_wallpaper_liquid_glass(
                RectF::new(0.0, 0.0, w, h),
                [0.0; 4],
                pal,
                glass_tint,
                Rgba::TRANSPARENT,
                sheen,
                0.88 + 0.12 * g,
            );
            // Sculpted Liquid Glass bottom refractive prism highlight + subtle separation hairline
            canvas.fill_rect(RectF::new(0.0, h - 1.5, w, 0.9), prism_hi);
            canvas.fill_rect(RectF::new(0.0, h - 0.7, w, 0.7), prism_shadow);
        } else if g > 0.005 {
            canvas.fill_rect(RectF::new(0.0, 0.0, w, h), pal.glass_tint.with_alpha(g));
            canvas.fill_rect(RectF::new(0.0, h - 1.0, w, 1.0), pal.hair.with_alpha(g));
        }

        // High-contrast solid foreground ink matched to skin & effective Liquid Glass luminance
        let dark_on_bright_wp = !pal.light && monet.top_luma > 0.72;
        let light_fg_ctx = pal.light || dark_on_bright_wp;

        let bar_ink = if light_fg_ctx {
            Rgba::rgb(10, 14, 24)
        } else {
            pal.ink
        };
        let bar_ink_sub = bar_ink;
        let bar_ink_mute = bar_ink.with_alpha(0.48);
        let emerald_ink = if light_fg_ctx {
            Rgba::hex(0x047857)
        } else {
            Rgba::hex(0x34d399)
        };
        let warn_ink = if light_fg_ctx {
            Rgba::hex(0xc2410c)
        } else {
            pal.warn
        };
        let pill_bg = if light_fg_ctx {
            Rgba::rgba_f(255, 255, 255, 0.42)
        } else {
            Rgba::rgba_f(255, 255, 255, 0.14)
        };
        let pill_rim = if light_fg_ctx {
            Rgba::rgba_f(255, 255, 255, 0.68)
        } else {
            Rgba::rgba_f(255, 255, 255, 0.24)
        };

        // 2. Center Interactive Cluster: Minimal Borderless Workspace Dots + Optional Media/OSD Capsule
        let (dots, _) = niri.workspace_dots(out_name);
        let mut dots_w = 0.0_f32;
        for (_, is_act, is_empty) in &dots {
            dots_w += if *is_act {
                18.0
            } else if *is_empty {
                6.5
            } else {
                6.0
            };
        }
        if dots.len() > 1 {
            dots_w += ((dots.len() - 1) as f32) * 6.0;
        }

        let has_media = !sys.media_title.is_empty() || sys.media_playing;
        let media_part = if has_media {
            let raw = if sys.media_title.is_empty() {
                "正在播放"
            } else {
                sys.media_title.as_str()
            };
            truncate_chars(raw, 18)
        } else {
            String::new()
        };
        let (media_tw, _) = if has_media {
            text.measure(&media_part, BAR_FONT, FontWeight::SemiBold, canvas.scale)
        } else {
            (0.0, 0.0)
        };
        let base_media_w = if has_media {
            (30.0 + media_tw + 13.0).clamp(92.0, 220.0)
        } else {
            0.0
        };

        let osd_r = self.osd_reveal.pos.clamp(0.0, 1.0);
        let show_capsule = has_media || osd_r > 0.01;
        let target_osd_w = if self.osd.has_slider {
            268.0_f32
        } else {
            let (tw, _) =
                text.measure(&self.osd.label, BAR_FONT, FontWeight::SemiBold, canvas.scale);
            (40.0 + tw + 16.0).clamp(144.0, 296.0)
        };
        let capsule_w = if show_capsule {
            base_media_w + (target_osd_w - base_media_w) * osd_r
        } else {
            0.0
        };

        let gap_c = 12.0_f32;
        let total_center_w = if show_capsule && capsule_w > 1.0 {
            dots_w + gap_c + capsule_w
        } else {
            dots_w
        };
        let cx0 = ((w - total_center_w) * 0.5).max(180.0);
        let cx1 = cx0 + total_center_w;
        self.center_span = (cx0 - 8.0, cx1 + 8.0);

        // 2a. Clean Borderless Monochrome Workspace Dots (`━━` active, `•` occupied, `○` trailing empty)
        let mut dx = cx0;
        for &(ws_idx, is_act, is_trailing_empty) in &dots {
            let dw = if is_act {
                18.0
            } else if is_trailing_empty {
                6.5
            } else {
                6.0
            };
            let dh = if is_trailing_empty { 6.5 } else { 6.0 };
            let dr = RectF::new(dx, (h - dh) * 0.5, dw, dh);
            if is_act && is_trailing_empty {
                canvas.fill_round_rect(dr, dh * 0.5, bar_ink.with_alpha(0.24));
                canvas.stroke_round_rect(dr, dh * 0.5, 1.4, bar_ink.with_alpha(0.88));
            } else if is_act {
                canvas.fill_round_rect(dr, dh * 0.5, bar_ink.with_alpha(0.90));
            } else if is_trailing_empty {
                canvas.stroke_round_rect(dr, dh * 0.5, 1.35, bar_ink_mute);
            } else {
                canvas.fill_round_rect(dr, dh * 0.5, bar_ink_mute);
            }
            let hit_r = RectF::new(dx - 4.0, 2.0, dw + 8.0, h - 4.0);
            self.hits.push((
                hit_r,
                if is_act {
                    CenterHit::Overview
                } else {
                    CenterHit::Workspace(ws_idx)
                },
            ));
            dx += dw + 6.0;
        }

        // 2b. Liquid Glass Media / OSD Capsule (strictly absent when no music & no OSD!)
        if show_capsule && capsule_w > 8.0 {
            let cap_alpha = if has_media {
                1.0
            } else {
                (osd_r * 1.4).clamp(0.0, 1.0)
            };
            let pill_h = BAR_CAPSULE_H + 1.0 * osd_r;
            let pill_rad = pill_h * 0.5;
            let isl_r = RectF::new(cx0 + dots_w + gap_c, (h - pill_h) * 0.5, capsule_w, pill_h);
            self.osd.capsule_rect = isl_r;

            if pal.glass && cap_alpha > 0.05 && isl_r.w >= 40.0 {
                canvas.draw_glass_shadow(isl_r, pill_rad, 8.0, 1.5, 0.12 * cap_alpha);
            }
            canvas.fill_round_rect(isl_r, pill_rad, pill_bg.with_alpha(cap_alpha));
            canvas.stroke_round_rect(isl_r, pill_rad, 1.0, pill_rim.with_alpha(cap_alpha));

            let norm_alpha = if has_media {
                ((0.55 - osd_r) / 0.55).clamp(0.0, 1.0)
            } else {
                0.0
            };
            if norm_alpha > 0.01 {
                let mut tx = isl_r.x + 10.5;
                let eq_cols = [
                    Rgba::hex(0xa855f7).with_alpha(norm_alpha),
                    Rgba::hex(0xec4899).with_alpha(norm_alpha),
                    Rgba::hex(0x3b82f6).with_alpha(norm_alpha),
                ];
                let eq_base_y = (h + 10.0) * 0.5;
                for (bi, &ec) in eq_cols.iter().enumerate() {
                    let bh = if sys.media_playing {
                        let ph = self.anim_phase + (bi as f32) * 1.35;
                        3.0 + (ph.sin().abs()) * 7.5
                    } else {
                        4.0
                    };
                    canvas.fill_round_rect(
                        RectF::new(tx + (bi as f32) * 4.0, eq_base_y - bh, 2.2, bh),
                        1.1,
                        ec,
                    );
                }
                tx += 15.5;
                text.draw_in_rect(
                    canvas,
                    &media_part,
                    RectF::new(tx, 0.0, media_tw + 6.0, h),
                    BAR_FONT,
                    FontWeight::SemiBold,
                    bar_ink.with_alpha(norm_alpha),
                    Align::Left,
                );
            }

            let osd_alpha = ((osd_r - 0.28) / 0.72).clamp(0.0, 1.0);
            if osd_alpha > 0.01 {
                let ic_col = match self.osd.kind {
                    OsdKind::Volume { muted: true } | OsdKind::MicMute { muted: true } => {
                        Rgba::hex(0xf97316).with_alpha(osd_alpha)
                    }
                    OsdKind::Brightness => Rgba::hex(if light_fg_ctx { 0xd97706 } else { 0xfbbf24 })
                        .with_alpha(osd_alpha),
                    OsdKind::Volume { muted: false } => bar_ink.with_alpha(osd_alpha),
                    OsdKind::MicMute { muted: false } | OsdKind::Toast => {
                        Rgba::hex(if light_fg_ctx { 0x059669 } else { 0x34d399 })
                            .with_alpha(osd_alpha)
                    }
                };
                icons.draw(
                    canvas,
                    self.osd.icon,
                    RectF::new(isl_r.x + 11.0, (h - BAR_ICON) * 0.5, BAR_ICON, BAR_ICON),
                    ic_col,
                    BAR_STROKE,
                );

                if self.osd.has_slider && isl_r.w >= 140.0 {
                    let scrubbing = self.osd.dragging_slot.is_some();
                    let th = if scrubbing { 7.5_f32 } else { 5.5_f32 };
                    let tw = (isl_r.w - 88.0).max(32.0);
                    let track = RectF::new(isl_r.x + 33.5, (h - th) * 0.5, tw, th);
                    self.osd.track_rect = track;

                    let track_bg = if light_fg_ctx {
                        Rgba::rgba_f(18, 24, 38, 0.14 * osd_alpha)
                    } else {
                        Rgba::rgba_f(255, 255, 255, 0.18 * osd_alpha)
                    };
                    canvas.fill_round_rect(track, th * 0.5, track_bg);
                    let frac = self.osd_val.pos.clamp(0.0, 1.0);
                    let fill_w = (track.w * frac).max(0.0);
                    if fill_w > 1.2 {
                        let fill_col = match self.osd.kind {
                            OsdKind::Volume { muted: true } => {
                                if light_fg_ctx {
                                    Rgba::rgba_f(18, 24, 38, 0.28 * osd_alpha)
                                } else {
                                    Rgba::rgba_f(255, 255, 255, 0.30 * osd_alpha)
                                }
                            }
                            OsdKind::Brightness => {
                                Rgba::hex(if light_fg_ctx { 0xd97706 } else { 0xfbbf24 })
                                    .with_alpha(osd_alpha)
                            }
                            _ => Rgba::hex(if light_fg_ctx { 0x2563eb } else { 0x3b82f6 })
                                .with_alpha(osd_alpha),
                        };
                        canvas.fill_round_rect(
                            RectF::new(track.x, track.y, fill_w.max(th), th),
                            th * 0.5,
                            fill_col,
                        );
                    }

                    let val_col = if matches!(self.osd.kind, OsdKind::Volume { muted: true }) {
                        Rgba::hex(0xf97316).with_alpha(osd_alpha)
                    } else {
                        bar_ink.with_alpha(osd_alpha)
                    };
                    text.draw_in_rect(
                        canvas,
                        &self.osd.label,
                        RectF::new(isl_r.x + isl_r.w - 49.0, 0.0, 39.0, h),
                        BAR_FONT,
                        FontWeight::SemiBold,
                        val_col,
                        Align::Right,
                    );
                } else if !self.osd.has_slider && isl_r.w >= 96.0 {
                    text.draw_in_rect(
                        canvas,
                        &self.osd.label,
                        RectF::new(isl_r.x + 33.0, 0.0, (isl_r.w - 44.0).max(24.0), h),
                        BAR_FONT,
                        FontWeight::SemiBold,
                        bar_ink.with_alpha(osd_alpha),
                        Align::Left,
                    );
                }
            }

            if has_media && osd_r < 0.45 {
                self.hits.push((
                    RectF::new(isl_r.x, 2.0, isl_r.w, h - 4.0),
                    CenterHit::IslandPop,
                ));
            }
        }

        // 3. Left Wing: Clean Monochrome Clock + Inline Stylus / BT Peripheral Indicators (zero chunky colored capsules!)
        let mut lx = BAR_EDGE_PAD;
        let (clk_tw, _) = text.measure(&sys.time_hm, BAR_FONT, FontWeight::SemiBold, canvas.scale);
        let clk_w = clk_tw.ceil() + 2.0;
        text.draw_in_rect(
            canvas,
            &sys.time_hm,
            RectF::new(lx, 0.0, clk_w + 4.0, h),
            BAR_FONT,
            FontWeight::SemiBold,
            bar_ink,
            Align::Left,
        );
        lx += clk_w + 14.0;

        let max_lx = (self.center_span.0 - 12.0).max(lx);
        for dev in sys.sorted_topbar_peripherals() {
            let pct_str = match dev.battery_pct {
                Some(p) => format!("{p}%"),
                None => continue,
            };
            let (ptw, _) = text.measure(&pct_str, BAR_FONT, FontWeight::SemiBold, canvas.scale);
            let item_w = BAR_ICON + 4.5 + ptw.ceil();
            if lx + item_w > max_lx {
                break;
            }
            let is_low = dev.battery_pct.is_some_and(|p| p <= 20);
            let fg = if is_low { warn_ink } else { bar_ink_sub };

            icons.draw(
                canvas,
                dev.kind.icon_name(),
                RectF::new(lx, (h - BAR_ICON) * 0.5, BAR_ICON, BAR_ICON),
                fg,
                BAR_STROKE,
            );
            text.draw_in_rect(
                canvas,
                &pct_str,
                RectF::new(lx + BAR_ICON + 4.5, 0.0, ptw.ceil() + 4.0, h),
                BAR_FONT,
                FontWeight::SemiBold,
                fg,
                Align::Left,
            );
            lx += item_w + 13.0;
        }

        // 4. Right Wing: Bold Status Icons + Live Charge/Discharge Wattage + Battery `%` + High-Contrast Battery Cell
        let mut rx = w - BAR_EDGE_PAD;
        let bypass_active = sys.usb_online && sys.charge_limit < 100 && !sys.bat_charging;

        let cell_w = 24.5_f32;
        let cell_h = 12.5_f32;
        rx -= cell_w + 3.0;
        let cell_r = RectF::new(rx, (h - cell_h) * 0.5, cell_w, cell_h);
        canvas.stroke_round_rect(cell_r, 3.6, 1.8, bar_ink.with_alpha(0.88));
        canvas.fill_round_rect(
            RectF::new(cell_r.x + cell_w + 1.2, (h - 5.0) * 0.5, 2.1, 5.0),
            1.0,
            bar_ink.with_alpha(0.88),
        );
        let fill_frac = (sys.bat_pct as f32 / 100.0).clamp(0.08, 1.0);
        let inner_cell = cell_r.inset(2.4, 2.4);
        let bat_col = if sys.bat_charging || bypass_active {
            emerald_ink
        } else if sys.bat_pct <= 20 {
            warn_ink
        } else {
            bar_ink
        };
        canvas.fill_round_rect(
            RectF::new(
                inner_cell.x,
                inner_cell.y,
                inner_cell.w * fill_frac,
                inner_cell.h,
            ),
            1.6,
            bat_col,
        );
        if sys.bat_charging {
            icons.draw(
                canvas,
                "bolt",
                RectF::new(
                    cell_r.x + (cell_w - 10.0) * 0.5,
                    (h - 10.0) * 0.5,
                    10.0,
                    10.0,
                ),
                if light_fg_ctx {
                    Rgba::WHITE
                } else {
                    Rgba::hex(0x0b1218)
                },
                2.2,
            );
        }

        let bat_lbl = format!("{}%", sys.bat_pct);
        let (bat_tw_raw, _) =
            text.measure(&bat_lbl, BAR_FONT, FontWeight::SemiBold, canvas.scale);
        let bat_tw = bat_tw_raw.ceil() + 2.0;
        rx -= bat_tw + 6.0;
        text.draw_in_rect(
            canvas,
            &bat_lbl,
            RectF::new(rx, 0.0, bat_tw, h),
            BAR_FONT,
            FontWeight::SemiBold,
            bar_ink,
            Align::Right,
        );

        let pwr_lbl = format_battery_power(sys);
        let (pwr_tw_raw, _) =
            text.measure(&pwr_lbl, BAR_FONT, FontWeight::SemiBold, canvas.scale);
        let pwr_tw = pwr_tw_raw.ceil() + 2.0;
        let pwr_col = if sys.bat_charging || bypass_active {
            emerald_ink
        } else {
            bar_ink
        };
        if rx - (pwr_tw + 8.5) >= self.center_span.1 + 12.0 {
            rx -= pwr_tw + 8.5;
            text.draw_in_rect(
                canvas,
                &pwr_lbl,
                RectF::new(rx, 0.0, pwr_tw, h),
                BAR_FONT,
                FontWeight::SemiBold,
                pwr_col,
                Align::Right,
            );
        }
        rx -= 11.5;

        let mut status_icons: Vec<(&str, Rgba)> = Vec::with_capacity(6);
        status_icons.push((
            if sys.wifi_on { "wifi" } else { "wifi_off" },
            if sys.wifi_on { bar_ink_sub } else { bar_ink_mute },
        ));
        if sys.bt_on {
            status_icons.push(("bluetooth", bar_ink_sub));
        }
        if bypass_active {
            status_icons.push(("bypass_power", emerald_ink));
        }
        if sys.muted {
            status_icons.push(("volume_off", warn_ink));
        }
        if sys.mic_muted {
            status_icons.push(("mic_off", warn_ink));
        }
        if prefs.dnd {
            status_icons.push(("moon_dnd", bar_ink_sub));
        }

        let min_rx = self.center_span.1 + 12.0;
        for (ic_name, col) in status_icons {
            if rx - BAR_ICON < min_rx {
                break;
            }
            rx -= BAR_ICON;
            icons.draw(
                canvas,
                ic_name,
                RectF::new(rx, (h - BAR_ICON) * 0.5, BAR_ICON, BAR_ICON),
                col,
                BAR_STROKE,
            );
            rx -= 10.0;
        }
        let _ = unread_notifs;
    }

    fn scrub_osd_at(&mut self, x: f32) -> BarAction {
        let tr = self.osd.track_rect;
        let is_brightness = self.osd.kind == OsdKind::Brightness;
        let raw_frac = ((x - tr.x) / tr.w.max(1.0)).clamp(0.0, 1.0);
        let pct = if is_brightness {
            ((raw_frac * 99.0).round() as u8 + 1).clamp(1, 100)
        } else {
            (raw_frac * 100.0).round().clamp(0.0, 100.0) as u8
        };
        let snapped_frac = (pct as f32) / 100.0;
        self.osd_val.snap(snapped_frac);
        if !is_brightness {
            self.osd.kind = OsdKind::Volume { muted: pct == 0 };
            self.osd.icon = if pct == 0 { "volume_off" } else { "sound" };
        }
        self.osd.label = format!("{pct}%");
        BarAction::ScrubOsd { is_brightness, pct }
    }

    pub fn no_pull_center_span(&self, bar_w: f32) -> (f32, f32) {
        let w = bar_w.max(320.0);
        let band_lo = w * 0.33;
        let band_hi = w * 0.67;
        let (c0, c1) = self.center_span;
        if c1 > c0 {
            (band_lo.min(c0), band_hi.max(c1))
        } else {
            (band_lo, band_hi)
        }
    }

    pub fn is_in_center_no_pull(&self, x: f32, bar_w: f32) -> bool {
        let (lo, hi) = self.no_pull_center_span(bar_w);
        x >= lo && x <= hi
    }

    pub fn on_touch_down(&mut self, slot: i32, x: f32, y: f32, bar_w: f32) -> BarAction {
        if self.osd_reveal.pos > 0.55
            && self.osd.has_slider
            && self.osd.capsule_rect.inset(-8.0, -8.0).contains(x, y)
        {
            self.osd.dragging_slot = Some(slot);
            self.osd.expires_at = None;
            self.gesture = None;
            HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_width);
            return self.scrub_osd_at(x);
        }

        let in_center = self.is_in_center_no_pull(x, bar_w);
        let wing = if x < bar_w * 0.5 {
            PullWing::Left
        } else {
            PullWing::Right
        };
        self.gesture = Some(BarGesture {
            slot,
            x0: x,
            y0: y,
            last_y: y,
            last_t: Instant::now(),
            vy: 0.0,
            in_center,
            wing,
            pulling: false,
        });
        BarAction::None
    }

    pub fn on_touch_motion(&mut self, slot: i32, x: f32, y: f32) -> BarAction {
        if self.osd.dragging_slot == Some(slot) {
            return self.scrub_osd_at(x);
        }

        let Some(g) = &mut self.gesture else {
            return BarAction::None;
        };
        if g.slot != slot || g.in_center {
            return BarAction::None;
        }
        let now = Instant::now();
        let dt = now.duration_since(g.last_t).as_secs_f32().max(0.001);
        let dy_step = y - g.last_y;
        g.vy = g.vy * 0.45 + (dy_step / dt) * 0.55;
        g.last_y = y;
        g.last_t = now;

        let total_dy = y - g.y0;
        if !g.pulling && total_dy > 7.0 {
            g.pulling = true;
            HapticEngine::global().pulse_norm(HapticKind::Light, g.x0 / self.last_width);
        }
        if g.pulling {
            return BarAction::DragConsole {
                wing: g.wing,
                finger_y: y.max(12.0),
            };
        }
        BarAction::None
    }

    pub fn on_touch_cancel(&mut self) -> BarAction {
        if self.osd.dragging_slot.take().is_some() {
            self.osd.expires_at = Some(Instant::now() + Duration::from_millis(1500));
        }
        if let Some(g) = self.gesture.take() {
            if g.pulling {
                return BarAction::FinishPull {
                    wing: g.wing,
                    vy: g.vy,
                };
            }
        }
        BarAction::None
    }

    pub fn on_touch_up(&mut self, slot: i32, x: f32, y: f32) -> BarAction {
        if self.osd.dragging_slot == Some(slot) {
            self.osd.dragging_slot = None;
            self.osd.expires_at = Some(Instant::now() + Duration::from_millis(1500));
            return self.scrub_osd_at(x);
        }

        let Some(g) = self.gesture.take() else {
            return BarAction::None;
        };
        if g.slot != slot {
            return BarAction::None;
        }
        if g.in_center {
            if (x - g.x0).hypot(y - g.y0) <= 18.0 {
                for &(r, hit) in self.hits.iter().rev() {
                    if r.contains(x, y) {
                        HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_width);
                        match hit {
                            CenterHit::Workspace(idx) => {
                                focus_workspace_idx(idx.min(250) as u8);
                                return BarAction::None;
                            }
                            CenterHit::Overview => {
                                toggle_overview();
                                return BarAction::None;
                            }
                            CenterHit::IslandPop => {
                                return BarAction::ToggleIslandPop;
                            }
                        }
                    }
                }
            }
            return BarAction::None;
        }

        if g.pulling {
            return BarAction::FinishPull {
                wing: g.wing,
                vy: g.vy,
            };
        }
        if (x - g.x0).hypot(y - g.y0) <= 16.0 {
            HapticEngine::global().pulse_norm(HapticKind::Tick, x / self.last_width);
            return BarAction::OpenConsole { wing: g.wing };
        }
        BarAction::None
    }
}

pub fn format_battery_power(sys: &SysSnapshot) -> String {
    let w = sys.bat_watts.max(0.0);
    if sys.bat_charging && w >= 0.05 {
        format!("+{w:.1}W")
    } else if sys.usb_online && w < 0.05 {
        "0.0W".to_string()
    } else if w < 0.05 {
        "0.0W".to_string()
    } else {
        format!("-{w:.1}W")
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let cnt = s.chars().count();
    if cnt <= max_chars {
        s.to_string()
    } else {
        let sub: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{sub}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tb520fu_ui::color::Skin;

    #[test]
    fn topbar_hides_media_capsule_when_idle_and_morphs_osd_1_to_1() {
        let mut bar = StatusBar::new();
        let mut canvas = Canvas::new(2944, 72, 2.0);
        let icons = IconSet::new();
        let mut text = TextEngine::new();
        let pal = Palette::for_skin(Skin::GlassDark);
        let sys = SysSnapshot::default();
        let niri = NiriSnapshot::default();
        let prefs = ShellPrefs::default();

        bar.render(&mut canvas, &icons, &mut text, &pal, &sys, &niri, &prefs, 0);
        assert_eq!(bar.osd.capsule_rect.w, 0.0);
        assert!(!bar.hits.iter().any(|(_, h)| matches!(h, CenterHit::IslandPop)));

        bar.show_volume_osd(42, false);
        for _ in 0..40 {
            bar.step(1.0 / 120.0, false, &niri);
        }
        assert!(bar.osd_reveal.pos > 0.90);

        bar.render(&mut canvas, &icons, &mut text, &pal, &sys, &niri, &prefs, 0);
        assert!(
            bar.osd.capsule_rect.w >= 250.0,
            "expanded OSD capsule width should be >= 250px, got {}",
            bar.osd.capsule_rect.w
        );

        let mid_y = bar.osd.capsule_rect.y + bar.osd.capsule_rect.h * 0.5;
        let act_down = bar.on_touch_down(0, bar.osd.capsule_rect.x + 20.0, mid_y, 1472.0);
        assert!(matches!(
            act_down,
            BarAction::ScrubOsd {
                is_brightness: false,
                ..
            }
        ));

        let target_x = bar.osd.track_rect.x + bar.osd.track_rect.w * 0.80;
        let act_move = bar.on_touch_motion(0, target_x, mid_y);
        match act_move {
            BarAction::ScrubOsd { is_brightness, pct } => {
                assert!(!is_brightness);
                assert!((78..=82).contains(&pct), "expected ~80%, got {pct}%");
            }
            other => panic!("expected ScrubOsd, got {other:?}"),
        }
        let _ = bar.on_touch_up(0, target_x, mid_y);
    }

    #[test]
    fn topbar_center_third_blocks_console_pull_even_without_media() {
        let mut bar = StatusBar::new();
        let mut canvas = Canvas::new(2944, 72, 2.0);
        let icons = IconSet::new();
        let mut text = TextEngine::new();
        let pal = Palette::for_skin(Skin::GlassDark);
        let sys = SysSnapshot::default();
        let niri = NiriSnapshot::default();
        let prefs = ShellPrefs::default();

        bar.render(&mut canvas, &icons, &mut text, &pal, &sys, &niri, &prefs, 0);

        for mid_x in [500.0_f32, 650.0, 736.0, 860.0, 960.0] {
            assert_eq!(bar.on_touch_down(0, mid_x, 10.0, 1472.0), BarAction::None);
            assert_eq!(bar.on_touch_motion(0, mid_x, 80.0), BarAction::None);
            assert_eq!(bar.on_touch_up(0, mid_x, 80.0), BarAction::None);
        }

        // Left wing pulls Left Console
        assert_eq!(bar.on_touch_down(0, 220.0, 10.0, 1472.0), BarAction::None);
        assert!(matches!(
            bar.on_touch_motion(0, 220.0, 60.0),
            BarAction::DragConsole {
                wing: PullWing::Left,
                ..
            }
        ));
        let _ = bar.on_touch_up(0, 220.0, 60.0);

        // Right wing pulls Right Console
        assert_eq!(bar.on_touch_down(0, 1260.0, 10.0, 1472.0), BarAction::None);
        assert!(matches!(
            bar.on_touch_motion(0, 1260.0, 60.0),
            BarAction::DragConsole {
                wing: PullWing::Right,
                ..
            }
        ));
        let _ = bar.on_touch_up(0, 1260.0, 60.0);
    }

    #[test]
    fn battery_power_formats_charging_discharging_and_bypass_zero_cleanly() {
        let mut sys = SysSnapshot::default();
        sys.usb_online = false;
        sys.bat_charging = false;
        sys.bat_watts = 3.4;
        assert_eq!(format_battery_power(&sys), "-3.4W");

        sys.usb_online = true;
        sys.bat_charging = true;
        sys.bat_watts = 22.5;
        assert_eq!(format_battery_power(&sys), "+22.5W");

        sys.usb_online = true;
        sys.charge_limit = 90;
        sys.bat_charging = false;
        sys.bat_watts = 0.0;
        assert_eq!(format_battery_power(&sys), "0.0W");
    }

    #[test]
    fn topbar_renders_translucent_wallpaper_liquid_glass_without_compositor_blur_rects() {
        let mut bar = StatusBar::new();
        bar.glass_reveal.snap(1.0);
        let mut canvas = Canvas::new(2944, 72, 2.0);
        let icons = IconSet::new();
        let mut text = TextEngine::new();
        for skin in [Skin::GlassDark, Skin::GlassLight] {
            let pal = Palette::for_skin(skin);
            bar.render(
                &mut canvas,
                &icons,
                &mut text,
                &pal,
                &SysSnapshot::default(),
                &NiriSnapshot::default(),
                &ShellPrefs::default(),
                0,
            );
            assert!(
                canvas.blur_rects.is_empty(),
                "TopBar uses pre-blurred wallpaper Liquid Glass (zero compositor blur_rects)"
            );
            let first = canvas.instances.first().expect("must emit Liquid Glass strip");
            assert!(
                (first.params[0] - tb520fu_ui::canvas::MODE_FAKE_GLASS).abs() < 1e-4,
                "expected MODE_FAKE_GLASS, got {}",
                first.params[0]
            );
            assert!(
                first.color0[3] < 0.50,
                "TopBar Liquid Glass tint must stay translucent (<0.50), got {}",
                first.color0[3]
            );
        }
    }
}
