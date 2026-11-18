// SPDX-License-Identifier: MIT
use std::time::Instant;

use cosmic_text::Align;
use tb520fu_ui::canvas::{Canvas, RectF};
use tb520fu_ui::color::{Rgba, Skin};
use tb520fu_ui::svg_icons::IconSet;
use tb520fu_ui::text::{FontWeight, TextEngine};
use tb520fu_ui::widgets::draw_origin_pill_slider;

use crate::layouts::{hint_for, KbLayout, KbMode, KeyDef, KeyKind, Lang, EMOJI_CATS, NUM_LIST, SYM_CATS};
use crate::state::{OskState, PanelKind};

pub struct OskRenderer {
    pub icons: IconSet,
    pub text: TextEngine,
}

impl Default for OskRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl OskRenderer {
    pub fn new() -> Self {
        Self {
            icons: IconSet::new(),
            text: TextEngine::new(),
        }
    }

    /// Render the complete OSK frame onto its own transparent Wayland surface.
    pub fn render(&mut self, state: &mut OskState, canvas: &mut Canvas) {
        canvas.clear(Rgba::TRANSPARENT);
        self.render_overlay(state, canvas);
    }

    /// Composite the OSK onto an existing `canvas` without clearing it (for unified Shell embedding & headless PNGs).
    pub fn render_overlay(&mut self, state: &mut OskState, canvas: &mut Canvas) {
        canvas.set_clip_round_rect(None);

        let m = state.metrics;
        let u = m.u;
        let ox = m.fpad;
        let oy = m.fpad + state.current_slide_offset();
        if oy >= canvas.logical_height() + 4.0 {
            return;
        }

        // 1. Backdrop 300px Dual-Kawase Liquid Glass / matte surface
        self.render_backdrop(state, canvas, ox, oy);

        // 2. Top band (toolbar, composing candidate bar, or sub-panel header)
        self.render_band(state, canvas, ox, oy);

        // 3. Body: active sub-panel OR main/num key grid
        if state.panel != PanelKind::None {
            self.render_panel(state, canvas, ox, oy);
        } else {
            self.render_keys(state, canvas, ox, oy);
        }

        // 4. Floating mode bottom drag handle
        if state.prefs.mode == KbMode::Float {
            let hw = 44.0 * u;
            let hh = 4.5 * u;
            let hx = ox + (m.kb_w - hw) * 0.5;
            let hy = oy + m.kb_h - (m.bot_h + hh) * 0.5;
            canvas.fill_round_rect(RectF::new(hx, hy, hw, hh), hh * 0.5, state.pal.ink2);
        }

        // 5. Popups & overlays (key bubbles, long-press alternates, slide-up clear pill, toast)
        self.render_overlays(state, canvas, ox, oy);
    }

    fn render_backdrop(&self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = &state.pal;
        let osk_tint = match pal.skin {
            Skin::GlassDark => Rgba::rgba_f(22, 24, 30, 0.78),
            Skin::GlassLight => Rgba::rgba_f(231, 234, 240, 0.80),
            Skin::SolidDark => Rgba::hex(0x232323),
            Skin::SolidLight => Rgba::hex(0xebedf1),
        };
        let osk_hair = match pal.skin {
            Skin::GlassDark => Rgba::rgba_f(255, 255, 255, 0.11),
            Skin::GlassLight => Rgba::rgba_f(255, 255, 255, 0.58),
            _ => pal.hair,
        };

        if state.prefs.mode == KbMode::Float {
            let body = RectF::new(ox, oy, m.kb_w, m.kb_h);
            let rad = 18.0 * u;
            canvas.draw_soft_shadow(body, rad, 16.0 * u, 6.0 * u, 0.36);
            canvas.fill_glass_corners_styled(body, [rad; 4], pal, osk_tint, osk_hair, 0.0, 1.0);
        } else if state.split_glass_active() {
            let half_w = (m.kb_w * m.split_half_frac).ceil();
            let top_r = 16.0 * u;
            let band_rect = RectF::new(ox, oy, m.kb_w, m.band_h);
            canvas.fill_glass_corners_styled(
                band_rect,
                [top_r, top_r, 0.0, 0.0],
                pal,
                osk_tint,
                Rgba::TRANSPARENT,
                0.0,
                1.0,
            );
            canvas.draw_line(ox + top_r * 0.5, oy + 0.5, ox + m.kb_w - top_r * 0.5, oy + 0.5, 1.0, osk_hair);

            let pad_h = m.kb_h - m.band_h;
            let left_pad = RectF::new(ox, oy + m.band_h, half_w, pad_h);
            let right_pad = RectF::new(ox + m.kb_w - half_w, oy + m.band_h, half_w, pad_h);
            canvas.fill_glass_corners_styled(
                left_pad,
                [0.0, 0.0, 14.0 * u, 0.0],
                pal,
                osk_tint,
                Rgba::TRANSPARENT,
                0.0,
                1.0,
            );
            canvas.fill_glass_corners_styled(
                right_pad,
                [0.0, 0.0, 0.0, 14.0 * u],
                pal,
                osk_tint,
                Rgba::TRANSPARENT,
                0.0,
                1.0,
            );
        } else {
            let body = RectF::new(ox, oy, m.kb_w, m.kb_h);
            let top_r = 16.0 * u;
            canvas.fill_glass_corners_styled(
                body,
                [top_r, top_r, 0.0, 0.0],
                pal,
                osk_tint,
                Rgba::TRANSPARENT,
                0.0,
                1.0,
            );
            canvas.draw_line(ox + top_r * 0.5, oy + 0.5, ox + m.kb_w - top_r * 0.5, oy + 0.5, 1.0, osk_hair);
        }
    }

    fn render_band(&mut self, state: &mut OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = state.pal;

        // Panel title bar for full-screen sub-panels
        if !matches!(
            state.panel,
            PanelKind::None | PanelKind::Toolbox | PanelKind::LayoutPicker
        ) {
            let back_ic = RectF::new(ox + 16.0 * u, oy + (m.band_h - 22.0 * u) * 0.5, 22.0 * u, 22.0 * u);
            self.icons.draw(canvas, "back", back_ic, pal.icon, 2.1);
            self.text.draw_in_rect(
                canvas,
                "返回",
                RectF::new(ox + 40.0 * u, oy, 56.0 * u, m.band_h),
                15.0 * u,
                FontWeight::Medium,
                pal.ink3,
                Align::Left,
            );

            let title = match state.panel {
                PanelKind::Feedback => "按键音效",
                PanelKind::Clipboard => "剪贴板",
                PanelKind::TextEdit => "文本编辑与触控板",
                PanelKind::Emoji => "表情",
                PanelKind::Symbols => "符号",
                PanelKind::Candidates => "全部候选词",
                _ => "",
            };
            self.text.draw_in_rect(
                canvas,
                title,
                RectF::new(ox + 110.0 * u, oy, m.kb_w - 220.0 * u, m.band_h),
                16.0 * u,
                FontWeight::SemiBold,
                pal.ink,
                Align::Center,
            );

            if state.panel == PanelKind::Clipboard {
                let armed = state
                    .clip_clear_armed_until
                    .is_some_and(|exp| Instant::now() < exp);
                let btn_r = RectF::new(
                    ox + m.kb_w - 104.0 * u,
                    oy + (m.band_h - 34.0 * u) * 0.5,
                    90.0 * u,
                    34.0 * u,
                );
                let bg = if armed { pal.danger.with_alpha(0.20) } else { pal.chip };
                let fg = if armed { pal.danger } else { pal.ink3 };
                canvas.fill_round_rect(btn_r, 17.0 * u, bg);
                self.text.draw_in_rect(
                    canvas,
                    if armed { "再点清空" } else { "清空" },
                    btn_r,
                    13.5 * u,
                    FontWeight::Medium,
                    fg,
                    Align::Center,
                );
            }
            canvas.draw_line(ox + 12.0 * u, oy + m.band_h - 1.0, ox + m.kb_w - 12.0 * u, oy + m.band_h - 1.0, 1.0, pal.sep);
            return;
        }

        // Composing mode: preedit + horizontal candidate strip
        if state.composing() {
            state.cand_rects.clear();
            if !state.preedit.is_empty() {
                self.text.draw_in_rect(
                    canvas,
                    &state.preedit,
                    RectF::new(ox + 18.0 * u, oy + 4.0 * u, m.kb_w - 84.0 * u, 20.0 * u),
                    13.5 * u,
                    FontWeight::Medium,
                    pal.acc_ink,
                    Align::Left,
                );
            }

            let c_top = if state.preedit.is_empty() { 8.0 * u } else { 24.0 * u };
            let c_h = m.band_h - c_top - 4.0 * u;
            let viewport_x = 12.0 * u;
            let viewport_w = (m.kb_w - 76.0 * u).max(60.0 * u);

            canvas.set_clip_round_rect(Some((
                RectF::new(ox + viewport_x, oy + c_top, viewport_w, c_h),
                [8.0 * u; 4],
            )));

            let mut cur_x = 16.0 * u;
            for (idx, cand) in state.candidates.iter().enumerate() {
                let (tw, _) = self.text.measure(cand, 18.0 * u * m.fs, FontWeight::Medium, canvas.scale);
                let item_w = (tw + 26.0 * u).max(46.0 * u);
                let vis_x = cur_x - state.cand_scroll_x;
                let local_r = RectF::new(vis_x, c_top, item_w, c_h);

                if vis_x + item_w >= viewport_x - 8.0 * u && vis_x <= viewport_x + viewport_w + 8.0 * u {
                    let is_primary = idx as i32 == state.cand_cursor.max(0);
                    let draw_r = RectF::new(ox + local_r.x, oy + local_r.y + 3.0 * u, local_r.w, local_r.h - 6.0 * u);
                    if is_primary {
                        canvas.fill_round_rect(draw_r, 10.0 * u, pal.acc_soft);
                    }
                    self.text.draw_in_rect(
                        canvas,
                        cand,
                        draw_r,
                        18.0 * u * m.fs,
                        if is_primary { FontWeight::SemiBold } else { FontWeight::Normal },
                        if is_primary { pal.acc_ink } else { pal.ink },
                        Align::Center,
                    );
                    state.cand_rects.push((idx, local_r));
                }
                cur_x += item_w + 6.0 * u;
            }
            state.cand_content_w = cur_x;
            canvas.set_clip_round_rect(None);

            // Expand candidates button (`⌄`) on the right
            let sep_x = ox + m.kb_w - 60.0 * u;
            canvas.draw_line(sep_x, oy + c_top + 8.0 * u, sep_x, oy + m.band_h - 10.0 * u, 1.0, pal.sep);
            let exp_ic = RectF::new(
                ox + m.kb_w - 42.0 * u,
                oy + c_top + (c_h - 22.0 * u) * 0.5,
                22.0 * u,
                22.0 * u,
            );
            self.icons.draw(canvas, "candidate_more", exp_ic, pal.icon2, 2.0);
            return;
        }

        // Idle toolbar buttons
        let btns = state.toolbar_buttons();
        for (id, icon_name, local_r) in btns {
            let active = (id == "toolbox" && state.panel == PanelKind::Toolbox)
                || (id == "emoji" && state.panel == PanelKind::Emoji)
                || (id == "layout" && state.panel == PanelKind::LayoutPicker)
                || (id == "clip" && state.panel == PanelKind::Clipboard)
                || (id == "edit" && state.panel == PanelKind::TextEdit)
                || (id == "toggle_split" && state.prefs.mode == KbMode::Split);
            let r = RectF::new(ox + local_r.x, oy + local_r.y, local_r.w, local_r.h);
            if active {
                canvas.fill_round_rect(r.inset(4.0 * u, 4.0 * u), 12.0 * u, pal.acc_soft);
            }
            let ic_sz = 23.0 * u * m.fs.max(0.90);
            let ic_r = RectF::new(
                r.x + (r.w - ic_sz) * 0.5,
                r.y + (r.h - ic_sz) * 0.5,
                ic_sz,
                ic_sz,
            );
            let fg = if active { pal.acc_ink } else { pal.icon2 };
            self.icons.draw(canvas, icon_name, ic_r, fg, 1.9);
        }
    }

    fn render_keys(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let fs = m.fs;
        let pal = &state.pal;
        let zh = state.lang.is_cjk();
        let rad = 7.0 * u * fs.max(0.88);

        for (idx, kd) in state.keys.iter().enumerate() {
            if kd.kind == KeyKind::List {
                self.render_num_list_key(state, canvas, kd, ox, oy);
                continue;
            }

            let down = state.is_key_down(idx);
            let sink = if down && pal.press_sinks { 1.0 * u } else { 0.0 };
            let r = RectF::new(ox + kd.rect.x, oy + kd.rect.y + sink, kd.rect.w, kd.rect.h);

            let (bg, lip_col) = if kd.kind == KeyKind::Enter && kd.send {
                (if down { pal.acc_p } else { pal.acc }, pal.acc_lip)
            } else if kd.kind == KeyKind::Shift && state.shift > 0 {
                (if down { pal.key_p } else { pal.key }, pal.lip)
            } else if kd.fn_tint {
                (if down { pal.fn_key_p } else { pal.fn_key }, if down { pal.lip_p } else { pal.lip })
            } else {
                (if down { pal.key_p } else { pal.key }, if down { pal.lip_p } else { pal.lip })
            };

            // Bottom 1px tactile lip
            if lip_col.a > 0 {
                let lip_r = RectF::new(r.x, r.y + 1.2 * u, r.w, r.h);
                canvas.fill_round_rect(lip_r, rad, lip_col);
            }
            // Main key cap
            canvas.fill_round_rect(r, rad, bg);
            if pal.edge.a > 0 {
                canvas.stroke_round_rect(r, rad, 1.0, pal.edge);
            }

            // Radial tap glow (`Theme.tap`)
            if let Some((g_idx, gx, gy, t0)) = state.glow {
                if g_idx == idx {
                    let frac = (t0.elapsed().as_secs_f32() / 0.18).clamp(0.0, 1.0);
                    let alpha = (1.0 - frac) * 0.28;
                    canvas.fill_radial_glow(
                        r,
                        rad,
                        ox + gx,
                        oy + gy,
                        r.w.max(r.h) * (0.55 + 0.45 * frac),
                        pal.acc.with_alpha(alpha),
                    );
                }
            }

            // Key face content
            match kd.kind {
                KeyKind::Char => {
                    let has_hint = if let Some(hint) = hint_for(kd.k, zh) {
                        let hr = RectF::new(r.x, r.y + 4.0 * u * fs, r.w, 14.5 * u * fs);
                        self.text.draw_in_rect(
                            canvas,
                            hint,
                            hr,
                            11.5 * u * fs,
                            FontWeight::Normal,
                            pal.ink2,
                            Align::Center,
                        );
                        true
                    } else {
                        false
                    };
                    let cap = state.display_cap_of(kd.k);
                    let dy = if has_hint { 6.0 * u * fs } else { 0.0 };
                    let cr = RectF::new(r.x, r.y + dy, r.w, r.h - dy);
                    self.text.draw_in_rect(
                        canvas,
                        &cap,
                        cr,
                        21.5 * u * fs,
                        FontWeight::Medium,
                        pal.ink,
                        Align::Center,
                    );
                }
                KeyKind::Digit => {
                    self.text.draw_in_rect(
                        canvas,
                        kd.ch,
                        r,
                        23.5 * u * fs,
                        FontWeight::Medium,
                        pal.ink,
                        Align::Center,
                    );
                }
                KeyKind::Punct => {
                    self.text.draw_in_rect(
                        canvas,
                        kd.ch,
                        r,
                        20.0 * u * fs,
                        FontWeight::Normal,
                        pal.ink,
                        Align::Center,
                    );
                }
                KeyKind::Sep => {
                    self.text.draw_in_rect(
                        canvas,
                        "分词",
                        r,
                        15.0 * u * fs,
                        FontWeight::Medium,
                        pal.ink,
                        Align::Center,
                    );
                }
                KeyKind::Sym => {
                    self.text.draw_in_rect(
                        canvas,
                        "符",
                        r,
                        16.5 * u * fs,
                        FontWeight::Medium,
                        pal.ink,
                        Align::Center,
                    );
                }
                KeyKind::Num => {
                    self.text.draw_in_rect(
                        canvas,
                        "123",
                        r,
                        15.5 * u * fs,
                        FontWeight::Medium,
                        pal.ink,
                        Align::Center,
                    );
                }
                KeyKind::Back => {
                    self.text.draw_in_rect(
                        canvas,
                        "返回",
                        r,
                        15.5 * u * fs,
                        FontWeight::Medium,
                        pal.ink,
                        Align::Center,
                    );
                }
                KeyKind::Del => {
                    let sz = 23.0 * u * fs;
                    let ir = RectF::new(r.x + (r.w - sz) * 0.5, r.y + (r.h - sz) * 0.5, sz, sz);
                    self.icons.draw(canvas, "backspace", ir, pal.icon, 1.9);
                }
                KeyKind::Enter => {
                    if kd.send {
                        let lbl = state.enter_label.unwrap_or("发送");
                        self.text.draw_in_rect(
                            canvas,
                            lbl,
                            r,
                            17.5 * u * fs,
                            FontWeight::Medium,
                            Rgba::WHITE,
                            Align::Center,
                        );
                    } else {
                        let sz = 23.0 * u * fs;
                        let ir = RectF::new(r.x + (r.w - sz) * 0.5, r.y + (r.h - sz) * 0.5, sz, sz);
                        self.icons.draw(canvas, "enter", ir, pal.icon, 2.1);
                    }
                }
                KeyKind::Shift => {
                    let sz = 22.0 * u * fs;
                    let ir = RectF::new(r.x + (r.w - sz) * 0.5, r.y + (r.h - sz) * 0.5, sz, sz);
                    let (ic, fg) = match state.shift {
                        2 => ("shift_lock", pal.acc_ink),
                        1 => ("shift_once", pal.acc_ink),
                        _ => ("shift", pal.icon),
                    };
                    self.icons.draw(canvas, ic, ir, fg, 2.0);
                }
                KeyKind::Space => {
                    if state.cursor_mode {
                        let sz = 22.0 * u * fs;
                        let ir = RectF::new(r.x + (r.w - sz) * 0.5, r.y + (r.h - sz) * 0.5, sz, sz);
                        self.icons.draw(canvas, "space_swipe", ir, pal.acc_ink, 2.0);
                    } else if state.layout == KbLayout::Num {
                        self.text.draw_in_rect(
                            canvas,
                            "空格",
                            r,
                            15.0 * u * fs,
                            FontWeight::Normal,
                            pal.ink,
                            Align::Center,
                        );
                    } else {
                        let sw = (68.0 * u * fs).min(r.w - 10.0 * u);
                        let sh = sw * 0.5;
                        let ir = RectF::new(r.x + (r.w - sw) * 0.5, r.y + (r.h - sh) * 0.5, sw, sh);
                        self.icons.draw(canvas, "__space", ir, pal.ink, 1.45);
                    }
                }
                KeyKind::Lang => {
                    let (big, sub) = match state.lang {
                        Lang::Zh => ("中", "/英"),
                        Lang::En => ("英", "/中"),
                        Lang::Ja => ("日", "/中"),
                    };
                    let cx = r.x + r.w * 0.5;
                    self.text.draw_in_rect(
                        canvas,
                        big,
                        RectF::new(cx - 22.0 * u * fs, r.y, 24.0 * u * fs, r.h),
                        16.5 * u * fs,
                        FontWeight::SemiBold,
                        pal.ink,
                        Align::Right,
                    );
                    self.text.draw_in_rect(
                        canvas,
                        sub,
                        RectF::new(cx + 2.0 * u * fs, r.y + 3.0 * u * fs, 24.0 * u * fs, r.h - 3.0 * u * fs),
                        11.5 * u * fs,
                        FontWeight::Normal,
                        pal.ink2,
                        Align::Left,
                    );
                }
                KeyKind::List => {}
            }
        }
    }

    fn render_num_list_key(&mut self, state: &OskState, canvas: &mut Canvas, kd: &KeyDef, ox: f32, oy: f32) {
        let u = state.metrics.u;
        let fs = state.metrics.fs;
        let pal = &state.pal;
        let rad = 10.0 * u;
        let r = RectF::new(ox + kd.rect.x, oy + kd.rect.y, kd.rect.w, kd.rect.h);

        canvas.fill_round_rect(r, rad, pal.fn_key);
        if pal.edge.a > 0 {
            canvas.stroke_round_rect(r, rad, 1.0, pal.edge);
        }

        canvas.set_clip_round_rect(Some((r, [rad; 4])));
        let item_h = 52.0 * u * fs;
        for (i, &sym) in NUM_LIST.iter().enumerate() {
            let iy = r.y + 6.0 * u + (i as f32) * item_h - state.num_list_scroll_y;
            if iy + item_h < r.y || iy > r.y + r.h {
                continue;
            }
            let ir = RectF::new(r.x + 6.0 * u, iy, r.w - 12.0 * u, item_h);
            if state.num_list_hot == Some(i) {
                canvas.fill_round_rect(ir.inset(2.0 * u, 3.0 * u), 8.0 * u, pal.key_p);
            }
            self.text.draw_in_rect(
                canvas,
                sym,
                ir,
                19.0 * u * fs,
                FontWeight::Normal,
                pal.ink,
                Align::Center,
            );
            if i + 1 < NUM_LIST.len() {
                canvas.draw_line(
                    r.x + 20.0 * u,
                    iy + item_h,
                    r.x + r.w - 20.0 * u,
                    iy + item_h,
                    1.0,
                    pal.sep,
                );
            }
        }
        canvas.set_clip_round_rect(None);
    }

    // ---------------- Sub-Panels ----------------

    fn render_panel(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        match state.panel {
            PanelKind::Toolbox => self.render_toolbox(state, canvas, ox, oy),
            PanelKind::LayoutPicker => self.render_layout_picker(state, canvas, ox, oy),
            PanelKind::Feedback => self.render_feedback(state, canvas, ox, oy),
            PanelKind::TextEdit => self.render_text_edit(state, canvas, ox, oy),
            PanelKind::Symbols => self.render_symbols(state, canvas, ox, oy),
            PanelKind::Emoji => self.render_emoji(state, canvas, ox, oy),
            PanelKind::Candidates => self.render_candidates_panel(state, canvas, ox, oy),
            PanelKind::Clipboard => self.render_clipboard(state, canvas, ox, oy),
            PanelKind::None => {}
        }
    }

    fn render_toolbox(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = &state.pal;
        let fl = state.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 12.0 * u;
        let top_y = m.band_h + 6.0 * u;
        let cols = if fl { 4 } else { 6 };
        let grid_w = m.kb_w - 2.0 * inset;
        let grid_h = m.kb_h - top_y - m.bot_h;
        let cw = (grid_w - ((cols - 1) as f32) * gap) / (cols as f32);
        let tiles = OskState::toolbox_tiles();
        let rows = (tiles.len() + cols - 1) / cols;
        let ch = (grid_h - ((rows - 1) as f32) * gap) / (rows as f32);

        for (i, &(id, icon, label, is_toggle)) in tiles.iter().enumerate() {
            let c = i % cols;
            let r = i / cols;
            let rect = RectF::new(
                ox + inset + (c as f32) * (cw + gap),
                oy + top_y + (r as f32) * (ch + gap),
                cw,
                ch,
            );
            let on = is_toggle && state.is_toolbox_tile_on(id);
            let bg = if on { pal.acc_soft } else { pal.card };
            let fg = if on { pal.acc_ink } else { pal.ink };
            canvas.fill_round_rect(rect, 14.0 * u, bg);
            if on {
                canvas.stroke_round_rect(rect, 14.0 * u, 1.2, pal.acc.with_alpha(0.55));
            }
            let ic_sz = (24.0 * u).min(ch * 0.36);
            let ic_r = RectF::new(
                rect.x + (rect.w - ic_sz) * 0.5,
                rect.y + ch * 0.20,
                ic_sz,
                ic_sz,
            );
            self.icons.draw(canvas, icon, ic_r, fg, 1.9);
            self.text.draw_in_rect(
                canvas,
                label,
                RectF::new(rect.x + 4.0 * u, rect.y + ch * 0.56, rect.w - 8.0 * u, ch * 0.36),
                12.5 * u,
                if on { FontWeight::SemiBold } else { FontWeight::Normal },
                fg,
                Align::Center,
            );
        }
    }

    fn render_layout_picker(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = &state.pal;
        let inset = 16.0 * u;
        let gap = 12.0 * u;
        let w = m.kb_w - 2.0 * inset;

        self.text.draw_in_rect(
            canvas,
            "输入语言与布局",
            RectF::new(ox + inset, oy + m.band_h + 6.0 * u, w, 26.0 * u),
            13.0 * u,
            FontWeight::Medium,
            pal.ink3,
            Align::Left,
        );

        let row1_y = oy + m.band_h + 36.0 * u;
        let row1 = [
            ("zh", "中文全键", state.layout == KbLayout::Main && state.lang == Lang::Zh),
            ("en", "English", state.layout == KbLayout::Main && state.lang == Lang::En),
            ("ja", "日本語", state.layout == KbLayout::Main && state.lang == Lang::Ja),
            ("num", "数字九键", state.layout == KbLayout::Num),
            ("sym", "符号面板", false),
        ];
        let cw1 = (w - 4.0 * gap) / 5.0;
        for (i, &(_, lbl, sel)) in row1.iter().enumerate() {
            let r = RectF::new(ox + inset + (i as f32) * (cw1 + gap), row1_y, cw1, 74.0 * u);
            canvas.fill_round_rect(r, 14.0 * u, if sel { pal.acc_soft } else { pal.card });
            if sel {
                canvas.stroke_round_rect(r, 14.0 * u, 1.5, pal.acc);
            }
            self.text.draw_in_rect(
                canvas,
                lbl,
                r,
                15.0 * u,
                if sel { FontWeight::SemiBold } else { FontWeight::Medium },
                if sel { pal.acc_ink } else { pal.ink },
                Align::Center,
            );
        }

        let row2_title_y = row1_y + 78.0 * u + 8.0 * u;
        self.text.draw_in_rect(
            canvas,
            "键盘形态",
            RectF::new(ox + inset, row2_title_y, w, 24.0 * u),
            13.0 * u,
            FontWeight::Medium,
            pal.ink3,
            Align::Left,
        );

        let row2_y = row1_y + 78.0 * u + 36.0 * u;
        let row2 = [
            ("dock", "标准停靠", KbMode::Dock),
            ("split", "拇指分离", KbMode::Split),
            ("float", "自由悬浮", KbMode::Float),
        ];
        let cw2 = (w - 2.0 * gap) / 3.0;
        for (i, &(ic, lbl, mode)) in row2.iter().enumerate() {
            let sel = state.prefs.mode == mode;
            let r = RectF::new(ox + inset + (i as f32) * (cw2 + gap), row2_y, cw2, 74.0 * u);
            canvas.fill_round_rect(r, 14.0 * u, if sel { pal.acc_soft } else { pal.card });
            if sel {
                canvas.stroke_round_rect(r, 14.0 * u, 1.5, pal.acc);
            }
            let fg = if sel { pal.acc_ink } else { pal.ink };
            let ic_r = RectF::new(r.x + 20.0 * u, r.y + (r.h - 24.0 * u) * 0.5, 24.0 * u, 24.0 * u);
            self.icons.draw(canvas, ic, ic_r, fg, 2.0);
            self.text.draw_in_rect(
                canvas,
                lbl,
                RectF::new(r.x + 52.0 * u, r.y, r.w - 60.0 * u, r.h),
                15.0 * u,
                if sel { FontWeight::SemiBold } else { FontWeight::Medium },
                fg,
                Align::Left,
            );
        }
    }

    fn render_feedback(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = &state.pal;
        let inset = 16.0 * u;

        let local_vol = state.feedback_vol_rect();
        self.text.draw_in_rect(
            canvas,
            "按键音量",
            RectF::new(ox + local_vol.x, oy + m.band_h + 12.0 * u, local_vol.w, 26.0 * u),
            13.5 * u,
            FontWeight::Medium,
            pal.ink3,
            Align::Left,
        );

        let vol_r = RectF::new(ox + local_vol.x, oy + local_vol.y, local_vol.w, local_vol.h);
        let frac = (state.prefs.vol as f32) / 10.0;
        let right_txt = if state.prefs.vol == 0 {
            "静音".to_string()
        } else {
            format!("{} / 10", state.prefs.vol)
        };
        let ic = if state.prefs.vol == 0 { "volume_off" } else { "sound" };
        draw_origin_pill_slider(
            canvas,
            &self.icons,
            &mut self.text,
            pal,
            vol_r,
            frac,
            ic,
            "按键音",
            &right_txt,
            u,
        );

        self.text.draw_in_rect(
            canvas,
            "音效风格",
            RectF::new(ox + inset, oy + m.band_h + 104.0 * u, local_vol.w, 24.0 * u),
            13.5 * u,
            FontWeight::Medium,
            pal.ink3,
            Align::Left,
        );

        let style_y = oy + m.band_h + 132.0 * u;
        let track_r = RectF::new(ox + inset, style_y, local_vol.w, 54.0 * u);
        canvas.fill_round_rect(track_r, 14.0 * u, pal.card);

        let sw = (local_vol.w - 16.0 * u) / 3.0;
        for (i, &(code, lbl)) in [("std", "标准"), ("soft", "柔和"), ("mech", "机械")].iter().enumerate() {
            let sel = state.prefs.sound == code;
            let sr = RectF::new(
                ox + inset + 4.0 * u + (i as f32) * (sw + 4.0 * u),
                style_y + 4.0 * u,
                sw,
                46.0 * u,
            );
            if sel {
                canvas.fill_round_rect(sr, 11.0 * u, pal.acc_soft);
                canvas.stroke_round_rect(sr, 11.0 * u, 1.2, pal.acc);
            }
            self.text.draw_in_rect(
                canvas,
                lbl,
                sr,
                14.5 * u,
                if sel { FontWeight::SemiBold } else { FontWeight::Normal },
                if sel { pal.acc_ink } else { pal.ink },
                Align::Center,
            );
        }
    }

    fn render_text_edit(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = &state.pal;
        let fl = state.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 12.0 * u;
        let row_h = 58.0 * u;
        let top_y = m.band_h + 6.0 * u;
        let area_w = m.kb_w - 2.0 * inset;
        let area_h = m.kb_h - top_y - m.bot_h;

        // 1. Trackpad card
        let pad_local = state.edit_trackpad_rect();
        let pad_r = RectF::new(ox + pad_local.x, oy + pad_local.y, pad_local.w, pad_local.h);
        canvas.fill_round_rect(pad_r, 16.0 * u, pal.card);
        canvas.stroke_round_rect(pad_r, 16.0 * u, 1.0, pal.hair);
        let mark_sz = 30.0 * u;
        self.icons.draw(
            canvas,
            "cursor_edit",
            RectF::new(
                pad_r.x + (pad_r.w - mark_sz) * 0.5,
                pad_r.y + (pad_r.h - mark_sz) * 0.5,
                mark_sz,
                mark_sz,
            ),
            pal.ink2.with_alpha(0.34),
            1.8,
        );
        if let Some((dx, dy)) = state.edit_pad_dot {
            let cx = (ox + dx).clamp(pad_r.x + 14.0 * u, pad_r.x + pad_r.w - 14.0 * u);
            let cy = (oy + dy).clamp(pad_r.y + 14.0 * u, pad_r.y + pad_r.h - 14.0 * u);
            canvas.fill_round_rect(
                RectF::new(cx - 12.0 * u, cy - 12.0 * u, 24.0 * u, 24.0 * u),
                12.0 * u,
                pal.acc.with_alpha(0.45),
            );
        }

        // 2. 3x3 Navigation cross
        let cross_x = if fl { inset } else { pad_local.x + pad_local.w + gap };
        let cross_y = if fl { pad_local.y + pad_local.h + gap } else { top_y };
        let cross_w = if fl { area_w } else { inset + area_w - cross_x };
        let cross_h = pad_local.h;
        let bw = (cross_w - 20.0 * u) / 3.0;
        let bh = (cross_h - 20.0 * u) / 3.0;
        let cells: &[(usize, usize, &str, Option<&str>)] = &[
            (0, 0, "行首", Some("home")),
            (1, 0, "", Some("arrow_up")),
            (2, 0, "行尾", Some("end")),
            (0, 1, "", Some("arrow_left")),
            (1, 1, "选取", None),
            (2, 1, "", Some("arrow_right")),
            (1, 2, "", Some("arrow_down")),
        ];
        for &(c, r, lbl, ic_opt) in cells {
            let rect = RectF::new(
                ox + cross_x + (c as f32) * (bw + 10.0 * u),
                oy + cross_y + (r as f32) * (bh + 10.0 * u),
                bw,
                bh,
            );
            let is_sel = lbl == "选取" && state.edit_selecting;
            canvas.fill_round_rect(rect, 12.0 * u, if is_sel { pal.acc } else { pal.key });
            let fg = if is_sel { Rgba::WHITE } else { pal.ink };
            if let Some(ic) = ic_opt {
                if lbl.is_empty() {
                    let sz = 22.0 * u;
                    self.icons.draw(
                        canvas,
                        ic,
                        RectF::new(rect.x + (rect.w - sz) * 0.5, rect.y + (rect.h - sz) * 0.5, sz, sz),
                        fg,
                        2.0,
                    );
                } else {
                    self.text.draw_in_rect(canvas, lbl, rect, 14.0 * u, FontWeight::Medium, fg, Align::Center);
                }
            } else {
                self.text.draw_in_rect(canvas, lbl, rect, 14.5 * u, FontWeight::SemiBold, fg, Align::Center);
            }
        }

        // 3. Bottom action row (`全选 / 剪切 / 复制 / 粘贴 / 删除`)
        let bot_y = oy + top_y + area_h - row_h;
        let act_w = (area_w - 4.0 * gap) / 5.0;
        let acts = [
            ("select_all", "全选"),
            ("cut", "剪切"),
            ("copy", "复制"),
            ("paste", "粘贴"),
            ("backspace", "退格"),
        ];
        for (i, &(ic, lbl)) in acts.iter().enumerate() {
            let rect = RectF::new(ox + inset + (i as f32) * (act_w + gap), bot_y, act_w, row_h);
            canvas.fill_round_rect(rect, 12.0 * u, pal.fn_key);
            let ic_sz = 20.0 * u;
            self.icons.draw(
                canvas,
                ic,
                RectF::new(rect.x + 18.0 * u, rect.y + (rect.h - ic_sz) * 0.5, ic_sz, ic_sz),
                pal.icon,
                1.9,
            );
            self.text.draw_in_rect(
                canvas,
                lbl,
                RectF::new(rect.x + 42.0 * u, rect.y, rect.w - 48.0 * u, rect.h),
                14.5 * u,
                FontWeight::Medium,
                pal.ink,
                Align::Left,
            );
        }
    }

    fn render_symbols(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = &state.pal;
        let fl = state.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 10.0 * u;
        let row_h = 56.0 * u;
        let top_y = m.band_h + 6.0 * u;
        let bar_y = m.kb_h - m.bot_h - row_h;
        let grid_h = bar_y - top_y - 10.0 * u;
        let right_w = if fl { 74.0 * u } else { 108.0 * u };
        let grid_w = m.kb_w - 2.0 * inset - right_w - gap;

        // Symbol grid
        let grid_r = RectF::new(ox + inset, oy + top_y, grid_w, grid_h);
        canvas.set_clip_round_rect(Some((grid_r, [12.0 * u; 4])));
        let syms = SYM_CATS[state.sym_cat.min(SYM_CATS.len() - 1)].1;
        let cols = if fl { 5 } else if state.sym_cat == 1 { 11 } else { 10 };
        let cw = grid_w / (cols as f32);
        let ch = grid_h / 3.0;
        for (idx, &s) in syms.iter().enumerate() {
            let c = idx % cols;
            let r = idx / cols;
            let cy = oy + top_y + (r as f32) * ch - state.panel_scroll_y;
            if cy + ch < oy + top_y || cy > oy + top_y + grid_h {
                continue;
            }
            let cell = RectF::new(ox + inset + (c as f32) * cw + 3.0 * u, cy + 3.0 * u, cw - 6.0 * u, ch - 6.0 * u);
            canvas.fill_round_rect(cell, 9.0 * u, pal.key);
            let fsz = if s.len() > 4 { 12.5 * u } else { 18.0 * u };
            self.text.draw_in_rect(canvas, s, cell, fsz, FontWeight::Normal, pal.ink, Align::Center);
        }
        canvas.set_clip_round_rect(None);

        // Right column (`退格 / 空格 / 回车`)
        let rx = ox + m.kb_w - inset - right_w;
        let rh = (grid_h - 12.0 * u) / 3.0;
        for (i, ic) in ["backspace", "__space", "enter"].iter().enumerate() {
            let rr = RectF::new(rx, oy + top_y + (i as f32) * (rh + 6.0 * u), right_w, rh);
            let is_enter = i == 2;
            canvas.fill_round_rect(rr, 10.0 * u, if is_enter { pal.acc } else { pal.fn_key });
            let sz = 22.0 * u;
            let iw = if *ic == "__space" { sz * 1.8 } else { sz };
            self.icons.draw(
                canvas,
                ic,
                RectF::new(rr.x + (rr.w - iw) * 0.5, rr.y + (rr.h - sz) * 0.5, iw, sz),
                if is_enter { Rgba::WHITE } else { pal.icon },
                2.0,
            );
        }

        // Bottom category bar (`返回` + 7 categories + `锁定`)
        let by = oy + bar_y;
        let back_r = RectF::new(ox + inset, by, 84.0 * u, row_h);
        canvas.fill_round_rect(back_r, 12.0 * u, pal.fn_key);
        self.text.draw_in_rect(canvas, "返回", back_r, 14.5 * u, FontWeight::Medium, pal.ink, Align::Center);

        let mid_x = ox + inset + 92.0 * u;
        let mid_w = m.kb_w - 2.0 * inset - 192.0 * u;
        canvas.fill_round_rect(RectF::new(mid_x, by, mid_w, row_h), 12.0 * u, pal.card);
        let cat_w = mid_w / (SYM_CATS.len() as f32);
        for (i, &(name, _)) in SYM_CATS.iter().enumerate() {
            let cr = RectF::new(mid_x + (i as f32) * cat_w, by, cat_w, row_h);
            let sel = i == state.sym_cat;
            if sel {
                canvas.fill_round_rect(cr.inset(3.0 * u, 5.0 * u), 9.0 * u, pal.acc_soft);
            }
            self.text.draw_in_rect(
                canvas,
                name,
                cr,
                13.5 * u,
                if sel { FontWeight::SemiBold } else { FontWeight::Normal },
                if sel { pal.acc_ink } else { pal.ink3 },
                Align::Center,
            );
        }

        let lock_r = RectF::new(ox + m.kb_w - inset - 92.0 * u, by, 92.0 * u, row_h);
        let locked = state.prefs.sym_lock;
        canvas.fill_round_rect(lock_r, 12.0 * u, if locked { pal.acc_soft } else { pal.fn_key });
        let l_ic = RectF::new(lock_r.x + 14.0 * u, lock_r.y + (row_h - 18.0 * u) * 0.5, 18.0 * u, 18.0 * u);
        let l_fg = if locked { pal.acc_ink } else { pal.icon2 };
        self.icons.draw(canvas, "lock", l_ic, l_fg, 1.9);
        self.text.draw_in_rect(
            canvas,
            "锁定",
            RectF::new(lock_r.x + 34.0 * u, lock_r.y, lock_r.w - 40.0 * u, row_h),
            13.5 * u,
            FontWeight::Medium,
            l_fg,
            Align::Left,
        );
    }

    fn render_emoji(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = &state.pal;
        let fl = state.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let row_h = 56.0 * u;
        let top_y = m.band_h + 6.0 * u;
        let bar_y = m.kb_h - m.bot_h - row_h;
        let grid_h = bar_y - top_y - 10.0 * u;
        let grid_w = m.kb_w - 2.0 * inset;

        let grid_r = RectF::new(ox + inset, oy + top_y, grid_w, grid_h);
        canvas.set_clip_round_rect(Some((grid_r, [12.0 * u; 4])));
        let items = state.emoji_items(state.emo_cat);
        let cols = if fl { 7 } else { 13 };
        let cw = grid_w / (cols as f32);
        let ch = 56.0 * u;
        for (idx, em) in items.iter().enumerate() {
            let c = idx % cols;
            let r = idx / cols;
            let cy = oy + top_y + (r as f32) * ch - state.panel_scroll_y;
            if cy + ch < oy + top_y || cy > oy + top_y + grid_h {
                continue;
            }
            let cell = RectF::new(ox + inset + (c as f32) * cw, cy, cw, ch);
            self.text.draw_in_rect(canvas, em, cell, 28.0 * u, FontWeight::Normal, Rgba::WHITE, Align::Center);
        }
        canvas.set_clip_round_rect(None);

        let by = oy + bar_y;
        let back_r = RectF::new(ox + inset, by, 84.0 * u, row_h);
        canvas.fill_round_rect(back_r, 12.0 * u, pal.fn_key);
        self.text.draw_in_rect(canvas, "ABC", back_r, 14.5 * u, FontWeight::SemiBold, pal.ink, Align::Center);

        let mid_x = ox + inset + 92.0 * u;
        let mid_w = m.kb_w - 2.0 * inset - 184.0 * u;
        canvas.fill_round_rect(RectF::new(mid_x, by, mid_w, row_h), 12.0 * u, pal.card);
        let cat_w = mid_w / (EMOJI_CATS.len() as f32);
        for (i, &(name, _)) in EMOJI_CATS.iter().enumerate() {
            let cr = RectF::new(mid_x + (i as f32) * cat_w, by, cat_w, row_h);
            let sel = i == state.emo_cat;
            if sel {
                canvas.fill_round_rect(cr.inset(3.0 * u, 5.0 * u), 9.0 * u, pal.acc_soft);
            }
            self.text.draw_in_rect(
                canvas,
                name,
                cr,
                13.5 * u,
                if sel { FontWeight::SemiBold } else { FontWeight::Normal },
                if sel { pal.acc_ink } else { pal.ink3 },
                Align::Center,
            );
        }

        let del_r = RectF::new(ox + m.kb_w - inset - 84.0 * u, by, 84.0 * u, row_h);
        canvas.fill_round_rect(del_r, 12.0 * u, pal.fn_key);
        let sz = 22.0 * u;
        self.icons.draw(
            canvas,
            "backspace",
            RectF::new(del_r.x + (del_r.w - sz) * 0.5, del_r.y + (del_r.h - sz) * 0.5, sz, sz),
            pal.icon,
            1.9,
        );
    }

    fn render_candidates_panel(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = &state.pal;
        let fl = state.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 10.0 * u;
        let top_y = m.band_h + 6.0 * u;
        let area_h = m.kb_h - top_y - m.bot_h;
        let right_w = if fl { 78.0 * u } else { 112.0 * u };
        let grid_w = m.kb_w - 2.0 * inset - right_w - gap;

        let grid_r = RectF::new(ox + inset, oy + top_y, grid_w, area_h);
        canvas.set_clip_round_rect(Some((grid_r, [12.0 * u; 4])));
        let cols = if fl { 3 } else { 6 };
        let cw = grid_w / (cols as f32);
        let ch = 62.0 * u;
        for (idx, cand) in state.candidates.iter().enumerate() {
            let c = idx % cols;
            let r = idx / cols;
            let cy = oy + top_y + (r as f32) * ch - state.panel_scroll_y;
            if cy + ch < oy + top_y || cy > oy + top_y + area_h {
                continue;
            }
            let cell = RectF::new(ox + inset + (c as f32) * cw + 3.0 * u, cy + 3.0 * u, cw - 6.0 * u, ch - 6.0 * u);
            let sel = idx as i32 == state.cand_cursor.max(0);
            canvas.fill_round_rect(cell, 10.0 * u, if sel { pal.acc_soft } else { pal.card });
            self.text.draw_in_rect(
                canvas,
                cand,
                cell,
                17.5 * u,
                if sel { FontWeight::SemiBold } else { FontWeight::Normal },
                if sel { pal.acc_ink } else { pal.ink },
                Align::Center,
            );
        }
        canvas.set_clip_round_rect(None);

        let rx = ox + m.kb_w - inset - right_w;
        let rh = (area_h - 16.0 * u) / 3.0;
        for (i, lbl) in ["收起", "退格", "重输"].iter().enumerate() {
            let rr = RectF::new(rx, oy + top_y + (i as f32) * (rh + 8.0 * u), right_w, rh);
            canvas.fill_round_rect(rr, 10.0 * u, pal.fn_key);
            self.text.draw_in_rect(canvas, lbl, rr, 15.0 * u, FontWeight::Medium, pal.ink, Align::Center);
        }
    }

    fn render_clipboard(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let m = state.metrics;
        let u = m.u;
        let pal = &state.pal;
        let fl = state.prefs.mode == KbMode::Float;
        let inset = 12.5 * u;
        let gap = 10.0 * u;

        let cat_y = oy + m.band_h + 8.0 * u;
        let cats = ["全部", "链接", "号码", "邮箱"];
        let cw_cat = 84.0 * u;
        for (i, &c_lbl) in cats.iter().enumerate() {
            let r = RectF::new(ox + inset + (i as f32) * (cw_cat + 8.0 * u), cat_y, cw_cat, 36.0 * u);
            let sel = i == state.clip_cat;
            canvas.fill_round_rect(r, 18.0 * u, if sel { pal.acc_soft } else { pal.chip });
            self.text.draw_in_rect(
                canvas,
                c_lbl,
                r,
                13.5 * u,
                if sel { FontWeight::SemiBold } else { FontWeight::Normal },
                if sel { pal.acc_ink } else { pal.ink3 },
                Align::Center,
            );
        }

        let grid_y = oy + m.band_h + 54.0 * u;
        let grid_h = m.kb_h - m.band_h - 54.0 * u - m.bot_h;
        let grid_w = m.kb_w - 2.0 * inset;
        let items = state.filtered_clipboard();
        if items.is_empty() {
            self.text.draw_in_rect(
                canvas,
                "暂无内容",
                RectF::new(ox + inset, grid_y, grid_w, grid_h),
                14.0 * u,
                FontWeight::Normal,
                pal.ink2,
                Align::Center,
            );
            return;
        }

        canvas.set_clip_round_rect(Some((RectF::new(ox + inset, grid_y, grid_w, grid_h), [12.0 * u; 4])));
        let cols = if fl { 1 } else { 3 };
        let cw = (grid_w - ((cols - 1) as f32) * gap) / (cols as f32);
        let ch = 82.0 * u;
        for (idx, (txt, pinned)) in items.iter().enumerate() {
            let c = idx % cols;
            let r = idx / cols;
            let cy = grid_y + (r as f32) * (ch + gap) - state.panel_scroll_y;
            if cy + ch < grid_y || cy > grid_y + grid_h {
                continue;
            }
            let card = RectF::new(ox + inset + (c as f32) * (cw + gap), cy, cw, ch);
            canvas.fill_round_rect(card, 12.0 * u, pal.card);
            if *pinned {
                canvas.stroke_round_rect(card, 12.0 * u, 1.2, pal.acc.with_alpha(0.55));
            }
            self.text.draw_wrapped(
                canvas,
                txt,
                RectF::new(card.x + 12.0 * u, card.y + 10.0 * u, card.w - 52.0 * u, card.h - 20.0 * u),
                13.5 * u,
                FontWeight::Normal,
                pal.ink,
                3,
            );
            let pin_ic = RectF::new(card.x + card.w - 32.0 * u, card.y + 12.0 * u, 18.0 * u, 18.0 * u);
            self.icons.draw(
                canvas,
                "pin",
                pin_ic,
                if *pinned { pal.acc_ink } else { pal.ink2 },
                1.9,
            );
        }
        canvas.set_clip_round_rect(None);
    }

    // ---------------- Popups & Overlays ----------------

    fn render_overlays(&mut self, state: &OskState, canvas: &mut Canvas, ox: f32, oy: f32) {
        let u = state.metrics.u;
        let pal = &state.pal;

        // 1. Key press bubbles
        for b in state.bubbles.values() {
            let br = RectF::new(ox + b.rect.x, oy + b.rect.y, b.rect.w, b.rect.h);
            canvas.draw_soft_shadow(br, 14.0 * u, 8.0 * u, 3.0 * u, 0.28);
            canvas.fill_round_rect(br, 14.0 * u, pal.pop);
            canvas.stroke_round_rect(br, 14.0 * u, 1.0, pal.pop_line);
            if b.sub.is_empty() {
                self.text.draw_in_rect(
                    canvas,
                    &b.main,
                    br,
                    28.0 * u,
                    FontWeight::SemiBold,
                    pal.ink,
                    Align::Center,
                );
            } else {
                self.text.draw_in_rect(
                    canvas,
                    &b.main,
                    RectF::new(br.x, br.y + 6.0 * u, br.w, br.h * 0.58),
                    25.0 * u,
                    FontWeight::SemiBold,
                    pal.acc_ink,
                    Align::Center,
                );
                self.text.draw_in_rect(
                    canvas,
                    &b.sub,
                    RectF::new(br.x, br.y + br.h * 0.58, br.w, br.h * 0.36),
                    12.5 * u,
                    FontWeight::Normal,
                    pal.ink3,
                    Align::Center,
                );
            }
        }

        // 2. Long-press alternates strip
        if let Some(popup) = &state.alts {
            let cell_w = 64.0 * u;
            let box_w = (popup.items.len() as f32) * cell_w + 12.0 * u;
            let box_h = 68.0 * u;
            let bx = (popup.key_rect.x + popup.key_rect.w * 0.5 - box_w * 0.5)
                .clamp(6.0 * u, (state.metrics.kb_w - box_w - 6.0 * u).max(6.0 * u));
            let by = (popup.key_rect.y - box_h - 8.0 * u).max(4.0 * u);
            let ar = RectF::new(ox + bx, oy + by, box_w, box_h);
            canvas.draw_soft_shadow(ar, 14.0 * u, 10.0 * u, 3.0 * u, 0.30);
            canvas.fill_round_rect(ar, 14.0 * u, pal.pop);
            canvas.stroke_round_rect(ar, 14.0 * u, 1.0, pal.pop_line);
            for (i, item) in popup.items.iter().enumerate() {
                let cr = RectF::new(ar.x + 6.0 * u + (i as f32) * cell_w, ar.y + 6.0 * u, cell_w, box_h - 12.0 * u);
                let sel = i == popup.sel;
                if sel {
                    canvas.fill_round_rect(cr, 10.0 * u, pal.acc);
                }
                self.text.draw_in_rect(
                    canvas,
                    item,
                    cr,
                    20.0 * u,
                    if sel { FontWeight::SemiBold } else { FontWeight::Normal },
                    if sel { Rgba::WHITE } else { pal.ink },
                    Align::Center,
                );
            }
        }

        // 3. Delete slide-up Clear-All pill
        if let Some((pill_local, hot)) = state.clear_pill {
            let pr = RectF::new(ox + pill_local.x, oy + pill_local.y, pill_local.w, pill_local.h);
            canvas.draw_soft_shadow(pr, pr.h * 0.5, 8.0 * u, 3.0 * u, 0.28);
            let bg = if hot { pal.danger } else { pal.pop };
            let fg = if hot { Rgba::WHITE } else { pal.ink };
            canvas.fill_round_rect(pr, pr.h * 0.5, bg);
            canvas.stroke_round_rect(pr, pr.h * 0.5, 1.0, pal.pop_line);
            let lbl = if hot { "松手清空全部" } else { "↑ 上滑至此清空" };
            self.text.draw_in_rect(canvas, lbl, pr, 14.0 * u, FontWeight::SemiBold, fg, Align::Center);
        }

        // 4. Toast banner
        if let Some((msg, _)) = &state.toast {
            let (tw, _) = self.text.measure(msg, 14.0 * u, FontWeight::Medium, canvas.scale);
            let pw = (tw + 36.0 * u).max(110.0 * u);
            let ph = 38.0 * u;
            let tr = RectF::new(
                ox + (state.metrics.kb_w - pw) * 0.5,
                oy + state.metrics.band_h + 14.0 * u,
                pw,
                ph,
            );
            canvas.draw_soft_shadow(tr, ph * 0.5, 8.0 * u, 2.0 * u, 0.25);
            canvas.fill_round_rect(tr, ph * 0.5, pal.pop);
            canvas.stroke_round_rect(tr, ph * 0.5, 1.0, pal.pop_line);
            self.text.draw_in_rect(canvas, msg, tr, 14.0 * u, FontWeight::Medium, pal.ink, Align::Center);
        }
    }
}
