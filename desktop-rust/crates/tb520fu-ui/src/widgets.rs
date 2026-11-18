// SPDX-License-Identifier: MIT
use std::sync::atomic::{AtomicU64, Ordering};

use cosmic_text::Align;
use tiny_skia::Pixmap;

use crate::canvas::{
    AtlasBank, Canvas, RectF, SdfInstance, MODE_PHOTO_BLIT, MODE_RGBA_ATLAS,
};
use crate::color::{Palette, Rgba};
use crate::svg_icons::IconSet;
use crate::text::{FontWeight, TextEngine};

static NEXT_BITMAP_ID: AtomicU64 = AtomicU64::new(100);

/// OriginOS / ColorOS dual-tone clipped capsule slider (`Pill.qml`):
/// - Background track in `pal.pill_track`
/// - Filled portion (`frac` in `0.0..=1.0`) in `pal.pill_fill`
/// - Icon + label + value text drawn in `pal.ink` over unfilled area and crisp-SDF-clipped in `pal.pill_fill_ink` over filled area!
#[allow(clippy::too_many_arguments)]
pub fn draw_origin_pill_slider(
    canvas: &mut Canvas,
    icons: &IconSet,
    text: &mut TextEngine,
    pal: &Palette,
    rect: RectF,
    frac: f32,
    icon_name: &str,
    title: &str,
    right_label: &str,
    u: f32,
) {
    let frac = frac.clamp(0.0, 1.0);
    let rad = (20.0 * u).min(rect.h * 0.5);

    // 1. Track background
    canvas.fill_round_rect(rect, rad, pal.pill_track);

    // 2. Unclipped base foreground
    draw_pill_foreground(
        canvas,
        icons,
        text,
        rect,
        icon_name,
        title,
        right_label,
        pal.ink,
        pal.ink3,
        u,
    );

    // 3. Analytical SDF-clipped active fill + inverted foreground.
    // Ensure non-zero percentages (`> 0.5%`) start cleanly past the left icon (`min_w >= 2 * rad`)
    // so low values like `10%` never slice the sun/speaker icon vertically in half or clamp SDF corner radius!
    let ic_sz = (21.0 * u).min(rect.h * 0.48);
    let min_w = (15.0 * u + ic_sz + 11.0 * u).max(rad * 2.05).min(rect.w);
    let fill_w = if frac <= 0.005 {
        0.0
    } else {
        (min_w + (rect.w - min_w) * frac).min(rect.w)
    };
    if fill_w > 0.5 {
        let saved_clip = canvas.save_clip();
        let fill_rect = RectF::new(rect.x, rect.y, fill_w, rect.h);
        let r_right = if fill_w >= rect.w - 1.5 {
            rad
        } else {
            (9.0 * u).min(rad)
        };
        let fill_radii = [rad, r_right, r_right, rad];
        canvas.set_clip_round_rect(Some((rect, [rad; 4])));
        canvas.fill_round_rect_corners(fill_rect, fill_radii, pal.pill_fill);

        canvas.set_clip_round_rect(Some((fill_rect, fill_radii)));
        let sub_ink = pal.pill_fill_ink.with_alpha(0.68);
        draw_pill_foreground(
            canvas,
            icons,
            text,
            rect,
            icon_name,
            title,
            right_label,
            pal.pill_fill_ink,
            sub_ink,
            u,
        );
        canvas.restore_clip(saved_clip);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_pill_foreground(
    canvas: &mut Canvas,
    icons: &IconSet,
    text: &mut TextEngine,
    rect: RectF,
    icon_name: &str,
    title: &str,
    right_label: &str,
    fg: Rgba,
    fg_sub: Rgba,
    u: f32,
) {
    let ic_sz = (21.0 * u).min(rect.h * 0.48);
    let ic_x = rect.x + 15.0 * u;
    let ic_y = rect.y + (rect.h - ic_sz) * 0.5;
    icons.draw(canvas, icon_name, RectF::new(ic_x, ic_y, ic_sz, ic_sz), fg, 2.0);

    if !title.is_empty() {
        let tx = ic_x + ic_sz + 9.0 * u;
        let tw = (rect.w - (tx - rect.x) - 56.0 * u).max(0.0);
        text.draw_in_rect(
            canvas,
            title,
            RectF::new(tx, rect.y, tw, rect.h),
            14.0 * u,
            FontWeight::SemiBold,
            fg,
            Align::Left,
        );
    }
    if !right_label.is_empty() {
        let rw = 58.0 * u;
        text.draw_in_rect(
            canvas,
            right_label,
            RectF::new(rect.x + rect.w - rw - 14.0 * u, rect.y, rw, rect.h),
            13.5 * u,
            FontWeight::SemiBold,
            fg_sub,
            Align::Right,
        );
    }
}

/// Decoded RGBA icon/photo uploaded on-demand into GPU VRAM (`tex_rgba` atlas for <=512px icons, `tex_photo` for full-res photos).
#[derive(Clone)]
pub struct BitmapIcon {
    pub id: u64,
    pub pixmap: Pixmap,
}

impl BitmapIcon {
    pub fn from_pixmap(pixmap: Pixmap) -> Self {
        Self {
            id: NEXT_BITMAP_ID.fetch_add(1, Ordering::Relaxed),
            pixmap,
        }
    }

    pub fn from_png_file(path: &std::path::Path) -> Option<Self> {
        Self::from_png_file_max(path, 512)
    }

    pub fn from_png_file_max(path: &std::path::Path, max_dim: u32) -> Option<Self> {
        let file = std::fs::File::open(path).ok()?;
        let mut decoder = png::Decoder::new(file);
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).ok()?;
        let w = info.width;
        let h = info.height;
        if w == 0 || h == 0 || w > 8192 || h > 8192 {
            return None;
        }
        let step = ((w.max(h) + max_dim - 1) / max_dim).max(1);
        let dw = (w / step).max(1);
        let dh = (h / step).max(1);
        let mut pixmap = Pixmap::new(dw, dh)?;
        let dst = pixmap.data_mut();
        let bytes = &buf[..info.buffer_size()];
        for dy in 0..dh {
            let sy = (dy * step).min(h - 1);
            for dx in 0..dw {
                let sx = (dx * step).min(w - 1);
                let si = (sy * w + sx) as usize;
                let di = ((dy * dw + dx) * 4) as usize;
                match info.color_type {
                    png::ColorType::Rgba => {
                        let s = &bytes[si * 4..si * 4 + 4];
                        let a = s[3] as u32;
                        dst[di] = (((s[0] as u32) * a + 127) / 255) as u8;
                        dst[di + 1] = (((s[1] as u32) * a + 127) / 255) as u8;
                        dst[di + 2] = (((s[2] as u32) * a + 127) / 255) as u8;
                        dst[di + 3] = s[3];
                    }
                    png::ColorType::Rgb => {
                        let s = &bytes[si * 3..si * 3 + 3];
                        dst[di] = s[0];
                        dst[di + 1] = s[1];
                        dst[di + 2] = s[2];
                        dst[di + 3] = 255;
                    }
                    png::ColorType::GrayscaleAlpha => {
                        let s = &bytes[si * 2..si * 2 + 2];
                        let a = s[1] as u32;
                        let g = (((s[0] as u32) * a + 127) / 255) as u8;
                        dst[di] = g;
                        dst[di + 1] = g;
                        dst[di + 2] = g;
                        dst[di + 3] = s[1];
                    }
                    png::ColorType::Grayscale => {
                        let g = bytes[si];
                        dst[di] = g;
                        dst[di + 1] = g;
                        dst[di + 2] = g;
                        dst[di + 3] = 255;
                    }
                    _ => return None,
                }
            }
        }
        Some(Self::from_pixmap(pixmap))
    }

    pub fn from_photo_file(path: &std::path::Path, max_dim: u32) -> Option<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "png" {
            return Self::from_png_file_max(path, max_dim);
        }
        let data = std::fs::read(path).ok()?;
        let mut dec = zune_jpeg::JpegDecoder::new(&data);
        let pixels = dec.decode().ok()?;
        let (iw, ih) = dec.dimensions()?;
        let w = iw as u32;
        let h = ih as u32;
        if w == 0 || h == 0 {
            return None;
        }
        let bpp = pixels.len() / ((w as usize) * (h as usize));
        if bpp != 3 && bpp != 4 && bpp != 1 {
            return None;
        }
        let step = ((w.max(h) + max_dim - 1) / max_dim).max(1);
        let dw = (w / step).max(1);
        let dh = (h / step).max(1);
        let mut pixmap = Pixmap::new(dw, dh)?;
        let dst = pixmap.data_mut();
        for dy in 0..dh {
            let sy = (dy * step).min(h - 1);
            for dx in 0..dw {
                let sx = (dx * step).min(w - 1);
                let si = ((sy * w + sx) as usize) * bpp;
                let di = ((dy * dw + dx) * 4) as usize;
                if bpp >= 3 {
                    dst[di] = pixels[si];
                    dst[di + 1] = pixels[si + 1];
                    dst[di + 2] = pixels[si + 2];
                    dst[di + 3] = 255;
                } else {
                    let g = pixels[si];
                    dst[di] = g;
                    dst[di + 1] = g;
                    dst[di + 2] = g;
                    dst[di + 3] = 255;
                }
            }
        }
        Some(Self::from_pixmap(pixmap))
    }

    pub fn has_opaque_corners(&self) -> bool {
        let w = self.pixmap.width();
        let h = self.pixmap.height();
        if w < 4 || h < 4 {
            return false;
        }
        let data = self.pixmap.data();
        let alpha_at = |x: u32, y: u32| -> u8 { data[((y * w + x) * 4 + 3) as usize] };
        let corners = [
            alpha_at(1, 1),
            alpha_at(w - 2, 1),
            alpha_at(1, h - 2),
            alpha_at(w - 2, h - 2),
        ];
        corners.into_iter().filter(|&a| a >= 200).count() >= 3
    }

    pub fn draw(&self, canvas: &mut Canvas, bounds: RectF, opacity: f32) {
        self.draw_rounded(canvas, bounds, 0.0, opacity, false);
    }

    pub fn draw_cover(&self, canvas: &mut Canvas, bounds: RectF, opacity: f32) {
        self.draw_rounded(canvas, bounds, 0.0, opacity, true);
    }

    pub fn draw_rounded(&self, canvas: &mut Canvas, bounds: RectF, radius: f32, opacity: f32, cover: bool) {
        if opacity <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
            return;
        }
        let iw = self.pixmap.width();
        let ih = self.pixmap.height();
        if iw == 0 || ih == 0 {
            return;
        }

        let pw = bounds.w * canvas.scale;
        let ph = bounds.h * canvas.scale;
        let img_aspect = (iw as f32) / (ih as f32);
        let box_aspect = pw / ph.max(1.0);

        // Compute draw rect + UV sub-rect for contain or cover
        let (draw_rect, uv_sub) = if cover {
            if img_aspect > box_aspect {
                let vis_u = box_aspect / img_aspect;
                let u0 = (1.0 - vis_u) * 0.5;
                (canvas.to_phys_rect(bounds), [u0, 0.0, u0 + vis_u, 1.0])
            } else {
                let vis_v = img_aspect / box_aspect;
                let v0 = (1.0 - vis_v) * 0.5;
                (canvas.to_phys_rect(bounds), [0.0, v0, 1.0, v0 + vis_v])
            }
        } else {
            let sx = pw / (iw as f32);
            let sy = ph / (ih as f32);
            let s = sx.min(sy);
            let dw = (iw as f32) * s;
            let dh = (ih as f32) * s;
            let dx = bounds.x * canvas.scale + (pw - dw) * 0.5;
            let dy = bounds.y * canvas.scale + (ph - dh) * 0.5;
            ([dx, dy, dw, dh], [0.0, 0.0, 1.0, 1.0])
        };

        let rad_phys = [(radius * canvas.scale).max(0.0); 4];

        if iw <= 512 && ih <= 512 {
            let base_uv = {
                let mut bank = AtlasBank::global().lock().unwrap();
                if let Some(&uv) = bank.bitmaps.get(&self.id) {
                    uv
                } else {
                    let uv = bank.insert_rgba(iw, ih, self.pixmap.data().to_vec());
                    bank.bitmaps.insert(self.id, uv);
                    uv
                }
            };
            let du = base_uv[2] - base_uv[0];
            let dv = base_uv[3] - base_uv[1];
            let uv = [
                base_uv[0] + du * uv_sub[0],
                base_uv[1] + dv * uv_sub[1],
                base_uv[0] + du * uv_sub[2],
                base_uv[1] + dv * uv_sub[3],
            ];
            canvas.push_instance(SdfInstance {
                rect: draw_rect,
                radii: rad_phys,
                color0: [1.0, 1.0, 1.0, opacity.clamp(0.0, 1.0)],
                uv_rect: uv,
                params: [MODE_RGBA_ATLAS, 0.0, 0.0, 0.0],
                ..Default::default()
            });
        } else {
            AtlasBank::global()
                .lock()
                .unwrap()
                .set_photo(self.id, iw, ih, self.pixmap.data());
            canvas.push_instance(SdfInstance {
                rect: draw_rect,
                radii: rad_phys,
                color0: [1.0, 1.0, 1.0, opacity.clamp(0.0, 1.0)],
                uv_rect: uv_sub,
                params: [MODE_PHOTO_BLIT, 0.0, 0.0, 0.0],
                ..Default::default()
            });
        }
    }
}
