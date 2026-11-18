// SPDX-License-Identifier: MIT
use std::collections::HashMap;

pub use cosmic_text::Align;
use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent,
    Weight, Wrap,
};

use crate::canvas::{
    AtlasBank, AtlasEntry, Canvas, RectF, SdfInstance, MODE_R8_ATLAS, MODE_RGBA_ATLAS,
};
use crate::color::Rgba;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontWeight {
    Normal,
    Medium,
    SemiBold,
    Bold,
}

impl FontWeight {
    fn to_cosmic(self) -> Weight {
        match self {
            Self::Normal => Weight::NORMAL,
            Self::Medium => Weight::MEDIUM,
            Self::SemiBold => Weight::SEMIBOLD,
            Self::Bold => Weight::BOLD,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ShapeKey {
    text: String,
    phys_px_q: u16, // quantized to 1/16th px
    weight: FontWeight,
    wrap_w_q: u32, // 0 = unwrapped single-line
    max_lines: u16,
}

#[derive(Clone, Copy)]
struct PositionedGlyph {
    cache_key: CacheKey,
    rel_x: i32,
    rel_y: i32,
}

#[derive(Clone)]
struct ShapedRun {
    max_line_w: f32,
    total_h: f32,
    eff_px: f32,
    baseline_y: f32,
    use_emoji: bool,
    glyphs: Vec<PositionedGlyph>,
}

/// GPU-Atlas HarfBuzz + Swash text shaper with shaped-run layout cache (0 HarfBuzz calls per animated frame!).
pub struct TextEngine {
    pub font_system: FontSystem,
    emoji_font_system: Option<FontSystem>,
    ui_sans: &'static str,
    has_mid_weights: bool,
    pub swash_cache: SwashCache,
    emoji_swash_cache: SwashCache,
    scratch: Buffer,
    shape_cache: HashMap<ShapeKey, ShapedRun>,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn pick_ui_sans(db: &cosmic_text::fontdb::Database) -> &'static str {
    let preferred = [
        "Inter",
        "Noto Sans CJK SC",
        "Noto Sans",
        "Cantarell",
        "DejaVu Sans",
        "Liberation Sans",
    ];
    preferred
        .iter()
        .copied()
        .find(|fam| db.faces().any(|f| f.families.iter().any(|(n, _)| n == *fam)))
        .unwrap_or("Noto Sans")
}

fn contains_true_emoji(text: &str) -> bool {
    if text.is_ascii() {
        return false;
    }
    text.chars().any(|c| {
        matches!(
            c as u32,
            0x20E3 | 0xFE0F | 0x203C..=0x2B55 | 0x1F000..=0x1FAFF
        )
    })
}

fn is_unwanted_ui_face(f: &cosmic_text::fontdb::FaceInfo, strip_emoji: bool) -> bool {
    if f.monospaced {
        return true;
    }
    f.families.iter().any(|(name, _)| {
        let low = name.to_ascii_lowercase();
        (strip_emoji && low.contains("emoji"))
            || low.contains("mono")
            || low.contains("serif")
            || low.contains("roman")
            || low.contains("bookman")
            || low.contains("latin modern")
            || low.starts_with("lm ")
            || low.contains("c059")
            || low.contains("p052")
            || low.contains("z003")
            || low.contains("d050000l")
            || low.contains("urw ")
            || low.contains("narrow")
            || low.contains("math")
    })
}

fn family_has_mid_weights(db: &cosmic_text::fontdb::Database, fam: &str) -> bool {
    db.faces().any(|f| {
        f.families.iter().any(|(n, _)| n == fam) && (500..=650).contains(&f.weight.0)
    })
}

impl TextEngine {
    pub fn new() -> Self {
        let mut font_system = FontSystem::new();
        let strip_ids: Vec<_> = font_system
            .db()
            .faces()
            .filter(|f| is_unwanted_ui_face(f, true))
            .map(|f| f.id)
            .collect();
        for id in strip_ids {
            font_system.db_mut().remove_face(id);
        }
        let chosen = pick_ui_sans(font_system.db());
        let has_mid_weights = family_has_mid_weights(font_system.db(), chosen);
        font_system.db_mut().set_sans_serif_family(chosen);
        let scratch = Buffer::new(&mut font_system, Metrics::new(16.0, 22.0));
        Self {
            font_system,
            emoji_font_system: None,
            ui_sans: chosen,
            has_mid_weights,
            swash_cache: SwashCache::new(),
            emoji_swash_cache: SwashCache::new(),
            scratch,
            shape_cache: HashMap::with_capacity(1024),
        }
    }

    fn ensure_emoji_fs(&mut self) {
        if self.emoji_font_system.is_none() {
            let mut efs = FontSystem::new();
            let strip_ids: Vec<_> = efs
                .db()
                .faces()
                .filter(|f| is_unwanted_ui_face(f, false))
                .map(|f| f.id)
                .collect();
            for id in strip_ids {
                efs.db_mut().remove_face(id);
            }
            let chosen = pick_ui_sans(efs.db());
            efs.db_mut().set_sans_serif_family(chosen);
            self.emoji_font_system = Some(efs);
        }
    }

    fn shape_cached(
        &mut self,
        text: &str,
        phys_px: f32,
        line_mult: f32,
        weight: FontWeight,
        wrap_phys_w: Option<f32>,
        max_lines: usize,
    ) -> ShapedRun {
        let phys_px_q = (phys_px * 16.0).round().clamp(16.0, 65000.0) as u16;
        let wrap_w_q = wrap_phys_w.map_or(0, |w| (w * 4.0).round().max(1.0) as u32);
        let max_lines_u16 = max_lines.min(4096) as u16;
        let key = ShapeKey {
            text: text.to_string(),
            phys_px_q,
            weight,
            wrap_w_q,
            max_lines: max_lines_u16,
        };
        if let Some(hit) = self.shape_cache.get(&key) {
            return hit.clone();
        }
        if self.shape_cache.len() >= 1536 {
            self.shape_cache.clear();
        }

        let eff_px = (phys_px_q as f32) / 16.0;
        let line_h = eff_px * line_mult;
        let metrics = Metrics::new(eff_px, line_h);
        let use_emoji = contains_true_emoji(text);
        if use_emoji {
            self.ensure_emoji_fs();
        }
        {
            let fs = if use_emoji {
                self.emoji_font_system.as_mut().unwrap()
            } else {
                &mut self.font_system
            };
            self.scratch.set_metrics(fs, metrics);
            if let Some(ww) = wrap_phys_w {
                self.scratch.set_wrap(fs, Wrap::WordOrGlyph);
                self.scratch.set_size(fs, Some(ww), None);
            } else {
                self.scratch.set_wrap(fs, Wrap::None);
                self.scratch.set_size(fs, None, None);
            }
            let resolved_weight = if self.has_mid_weights {
                weight.to_cosmic()
            } else {
                match weight {
                    FontWeight::Normal | FontWeight::Medium => Weight::NORMAL,
                    FontWeight::SemiBold | FontWeight::Bold => Weight::BOLD,
                }
            };
            let attrs = Attrs::new()
                .family(Family::Name(self.ui_sans))
                .weight(resolved_weight);
            self.scratch.set_text(fs, text, attrs, Shaping::Advanced);
            self.scratch.shape_until_scroll(fs, false);
        }

        let mut max_line_w = 0.0_f32;
        let mut total_h = 0.0_f32;
        let mut baseline_y = eff_px * 0.82;
        let mut glyphs = Vec::new();
        for (line_idx, run) in self.scratch.layout_runs().enumerate() {
            if line_idx >= max_lines {
                break;
            }
            if line_idx == 0 {
                baseline_y = run.line_y;
            }
            max_line_w = max_line_w.max(run.line_w);
            total_h = total_h.max(run.line_top + run.line_height);
            for g in run.glyphs.iter() {
                let phys = g.physical((0.0, run.line_y), 1.0);
                glyphs.push(PositionedGlyph {
                    cache_key: phys.cache_key,
                    rel_x: phys.x,
                    rel_y: phys.y,
                });
            }
        }

        let shaped = ShapedRun {
            max_line_w,
            total_h,
            eff_px,
            baseline_y,
            use_emoji,
            glyphs,
        };
        self.shape_cache.insert(key, shaped.clone());
        shaped
    }

    pub fn measure(&mut self, text: &str, px_size: f32, weight: FontWeight, scale: f32) -> (f32, f32) {
        if text.is_empty() || px_size <= 0.0 {
            return (0.0, px_size.max(0.0));
        }
        let scale = scale.max(0.5);
        let run = self.shape_cached(text, (px_size * scale).max(1.0), 1.28, weight, None, usize::MAX);
        (run.max_line_w / scale, (run.total_h / scale).max(px_size))
    }

    pub fn draw_in_rect(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        bounds: RectF,
        px_size: f32,
        weight: FontWeight,
        color: Rgba,
        h_align: Align,
    ) {
        if text.is_empty() || color.a == 0 || bounds.w <= 0.0 || bounds.h <= 0.0 || px_size <= 0.0 {
            return;
        }
        let scale = canvas.scale;
        let run = self.shape_cached(text, (px_size * scale).max(1.0), 1.25, weight, None, 1);

        let avail_w = bounds.w * scale;
        let ox = (bounds.x * scale
            + match h_align {
                Align::Center => (avail_w - run.max_line_w).max(0.0) * 0.5,
                Align::Right | Align::End => (avail_w - run.max_line_w).max(0.0),
                _ => 0.0,
            })
        .round() as i32;
        let oy = ((bounds.y + bounds.h * 0.5) * scale - run.baseline_y + 0.365 * run.eff_px)
            .round() as i32;
        let clip_min_x = (bounds.x * scale).floor();
        let clip_max_x = ((bounds.x + bounds.w) * scale).ceil();
        self.emit_glyph_instances(
            canvas,
            &run,
            ox,
            oy,
            color,
            Some((clip_min_x, clip_max_x)),
            f32::MAX,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_wrapped(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        bounds: RectF,
        px_size: f32,
        weight: FontWeight,
        color: Rgba,
        max_lines: usize,
    ) {
        if text.is_empty() || color.a == 0 || bounds.w <= 0.0 || bounds.h <= 0.0 || px_size <= 0.0 {
            return;
        }
        let scale = canvas.scale;
        let run = self.shape_cached(
            text,
            (px_size * scale).max(1.0),
            1.32,
            weight,
            Some(bounds.w * scale),
            max_lines,
        );
        let ox = (bounds.x * scale).round() as i32;
        let oy = (bounds.y * scale).round() as i32;
        let max_y = ((bounds.y + bounds.h) * scale).ceil();
        self.emit_glyph_instances(canvas, &run, ox, oy, color, None, max_y);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_glyph_instances(
        &mut self,
        canvas: &mut Canvas,
        run: &ShapedRun,
        ox: i32,
        oy: i32,
        color: Rgba,
        x_bounds: Option<(f32, f32)>,
        max_y: f32,
    ) {
        let mut bank = AtlasBank::global().lock().unwrap();
        let col_v4 = color.to_vec4();

        for g in &run.glyphs {
            let entry = if let Some(&cached) = bank.glyphs.get(&g.cache_key) {
                cached
            } else {
                let (fs, swash) = if run.use_emoji {
                    (
                        self.emoji_font_system.as_mut().unwrap(),
                        &mut self.emoji_swash_cache,
                    )
                } else {
                    (&mut self.font_system, &mut self.swash_cache)
                };
                let Some(img) = swash.get_image(fs, g.cache_key) else {
                    continue;
                };
                let gw = img.placement.width;
                let gh = img.placement.height;
                if gw == 0 || gh == 0 {
                    let empty = AtlasEntry {
                        uv: [0.0; 4],
                        is_rgba: false,
                        left: 0,
                        top: 0,
                        width: 0,
                        height: 0,
                    };
                    bank.glyphs.insert(g.cache_key, empty);
                    continue;
                }
                let built = match img.content {
                    SwashContent::Mask => {
                        let uv = bank.insert_r8(gw, gh, img.data.clone());
                        AtlasEntry {
                            uv,
                            is_rgba: false,
                            left: img.placement.left,
                            top: img.placement.top,
                            width: gw,
                            height: gh,
                        }
                    }
                    SwashContent::SubpixelMask => {
                        let mut mono = Vec::with_capacity((gw * gh) as usize);
                        for px in img.data.chunks_exact(4) {
                            let avg = ((px[0] as u32 + px[1] as u32 + px[2] as u32) / 3).min(255) as u8;
                            mono.push(avg);
                        }
                        let uv = bank.insert_r8(gw, gh, mono);
                        AtlasEntry {
                            uv,
                            is_rgba: false,
                            left: img.placement.left,
                            top: img.placement.top,
                            width: gw,
                            height: gh,
                        }
                    }
                    SwashContent::Color => {
                        // Premultiply color emoji RGBA for our premultiplied Uber-SDF blend pipeline
                        let mut premul = vec![0u8; img.data.len()];
                        for (s, d) in img.data.chunks_exact(4).zip(premul.chunks_exact_mut(4)) {
                            let a = s[3] as u32;
                            d[0] = ((s[0] as u32 * a + 127) / 255) as u8;
                            d[1] = ((s[1] as u32 * a + 127) / 255) as u8;
                            d[2] = ((s[2] as u32 * a + 127) / 255) as u8;
                            d[3] = s[3];
                        }
                        let uv = bank.insert_rgba(gw, gh, premul);
                        AtlasEntry {
                            uv,
                            is_rgba: true,
                            left: img.placement.left,
                            top: img.placement.top,
                            width: gw,
                            height: gh,
                        }
                    }
                };
                bank.glyphs.insert(g.cache_key, built);
                built
            };

            if entry.width == 0 || entry.height == 0 {
                continue;
            }
            let gx = (ox + g.rel_x + entry.left) as f32;
            let gy = (oy + g.rel_y - entry.top) as f32;
            let gw = entry.width as f32;
            let gh = entry.height as f32;
            if gy + gh > max_y + 2.0 {
                continue;
            }
            if let Some((xmin, xmax)) = x_bounds {
                if gx + gw < xmin || gx > xmax {
                    continue;
                }
            }

            canvas.push_instance(SdfInstance {
                rect: [gx, gy, gw, gh],
                color0: col_v4,
                uv_rect: entry.uv,
                params: [
                    if entry.is_rgba {
                        MODE_RGBA_ATLAS
                    } else {
                        MODE_R8_ATLAS
                    },
                    0.0,
                    0.0,
                    0.0,
                ],
                ..Default::default()
            });
        }
    }
}
