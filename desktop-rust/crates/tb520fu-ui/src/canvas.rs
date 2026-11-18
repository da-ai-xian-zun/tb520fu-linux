// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::io::BufWriter;
use std::sync::{Mutex, OnceLock};

use tiny_skia::{Path, PathBuilder, Rect};

use crate::color::{Palette, Rgba};
use crate::vk_renderer::{ VkRenderer, ATLAS_DIM };

/// Logical/physical rectangle `[x, y, w, h]`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    #[inline]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[inline]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    #[inline]
    pub fn inset(&self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            w: (self.w - 2.0 * dx).max(0.0),
            h: (self.h - 2.0 * dy).max(0.0),
        }
    }

    #[inline]
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    #[inline]
    pub fn intersect(&self, clip: Self) -> Option<Self> {
        let x0 = self.x.max(clip.x);
        let y0 = self.y.max(clip.y);
        let x1 = (self.x + self.w).min(clip.x + clip.w);
        let y1 = (self.y + self.h).min(clip.y + clip.h);
        if x1 > x0 && y1 > y0 {
            Some(Self {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            })
        } else {
            None
        }
    }
}

/// Build a rounded rectangle `tiny_skia::Path` (used when rasterizing vector `<rect rx=...>` primitives into the GPU icon atlas).
pub fn build_round_rect_path(r: RectF, radii: [f32; 4]) -> Option<Path> {
    if r.w <= 0.0 || r.h <= 0.0 {
        return None;
    }
    let max_r = (r.w.min(r.h) * 0.5).max(0.0);
    let [tl, tr, br, bl] = [
        radii[0].clamp(0.0, max_r),
        radii[1].clamp(0.0, max_r),
        radii[2].clamp(0.0, max_r),
        radii[3].clamp(0.0, max_r),
    ];
    if tl == 0.0 && tr == 0.0 && br == 0.0 && bl == 0.0 {
        let rect = Rect::from_xywh(r.x, r.y, r.w, r.h)?;
        return Some(PathBuilder::from_rect(rect));
    }
    const K: f32 = 0.552_284_8;
    let (l, t, ri, b) = (r.x, r.y, r.x + r.w, r.y + r.h);
    let mut pb = PathBuilder::new();
    pb.move_to(l + tl, t);
    pb.line_to(ri - tr, t);
    if tr > 0.0 {
        pb.cubic_to(ri - tr * (1.0 - K), t, ri, t + tr * (1.0 - K), ri, t + tr);
    }
    pb.line_to(ri, b - br);
    if br > 0.0 {
        pb.cubic_to(ri, b - br * (1.0 - K), ri - br * (1.0 - K), b, ri - br, b);
    }
    pb.line_to(l + bl, b);
    if bl > 0.0 {
        pb.cubic_to(l + bl * (1.0 - K), b, l, b - bl * (1.0 - K), l, b - bl);
    }
    pb.line_to(l, t + tl);
    if tl > 0.0 {
        pb.cubic_to(l, t + tl * (1.0 - K), l + tl * (1.0 - K), t, l + tl, t);
    }
    pb.close();
    pb.finish()
}

// ---------------- 128-byte Vulkan Uber-SDF Instance ----------------

pub const MODE_SOLID_FILL: f32 = 0.0;
pub const MODE_H_GRADIENT: f32 = 1.0;
pub const MODE_V_GRADIENT: f32 = 2.0;
pub const MODE_STROKE: f32 = 3.0;
pub const MODE_SOFT_SHADOW: f32 = 4.0;
pub const MODE_RADIAL_GLOW: f32 = 5.0;
pub const MODE_CAPSULE_LINE: f32 = 6.0;
pub const MODE_R8_ATLAS: f32 = 7.0;
pub const MODE_RGBA_ATLAS: f32 = 8.0;
pub const MODE_FAKE_GLASS: f32 = 9.0;
pub const MODE_PHOTO_BLIT: f32 = 10.0;
pub const MODE_TINT_GLASS: f32 = 11.0;
pub const MODE_CACHE_BLIT: f32 = 12.0;

/// A glass shape the compositor should blur behind, in logical surface coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlurRect {
    pub rect: RectF,
    pub radii: [f32; 4],
}

/// Glass smaller than this (logical px, either side) is drawn as plain tint without asking for blur:
/// keys, chips and the like sit on a panel that is already blurred.
const MIN_BLUR_SIDE: f32 = 40.0;

static OWN_BACKDROP_DEFAULT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Make every `Canvas` created from now on draw glass over its own wallpaper (see
/// `Canvas::own_backdrop`). For the headless `--render-png` scenes, which have no compositor.
pub fn set_own_backdrop_default(on: bool) {
    OWN_BACKDROP_DEFAULT.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SdfInstance {
    pub rect: [f32; 4],
    pub clip_rect: [f32; 4],
    pub radii: [f32; 4],
    pub clip_radii: [f32; 4],
    pub color0: [f32; 4],
    pub color1: [f32; 4],
    pub uv_rect: [f32; 4],
    pub params: [f32; 4],
}

// ---------------- Shelf-Packed GPU Glyph & Icon Atlas Bank ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasTarget {
    R8,
    Rgba,
}

pub struct AtlasUpload {
    pub target: AtlasTarget,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

struct ShelfPacker {
    cur_x: u32,
    cur_y: u32,
    row_h: u32,
}

impl ShelfPacker {
    const fn new() -> Self {
        Self {
            cur_x: 2,
            cur_y: 2,
            row_h: 0,
        }
    }

    fn reset(&mut self) {
        self.cur_x = 2;
        self.cur_y = 2;
        self.row_h = 0;
    }

    /// Allocate `w x h` with 2px gutter so bilinear filtering never bleeds across neighbors!
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w == 0 || h == 0 || w + 4 > ATLAS_DIM || h + 4 > ATLAS_DIM {
            return None;
        }
        if self.cur_x + w + 2 > ATLAS_DIM {
            self.cur_x = 2;
            self.cur_y += self.row_h + 2;
            self.row_h = 0;
        }
        if self.cur_y + h + 2 > ATLAS_DIM {
            return None;
        }
        let pos = (self.cur_x, self.cur_y);
        self.cur_x += w + 2;
        self.row_h = self.row_h.max(h);
        Some(pos)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AtlasEntry {
    pub uv: [f32; 4],
    pub is_rgba: bool,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

pub struct AtlasBank {
    pub epoch: u64,
    r8_shelf: ShelfPacker,
    rgba_shelf: ShelfPacker,
    pub glyphs: HashMap<cosmic_text::CacheKey, AtlasEntry>,
    pub icons: HashMap<(&'static str, u16, u16, u16), [f32; 4]>,
    pub bitmaps: HashMap<u64, [f32; 4]>,
    uploads: Vec<AtlasUpload>,
    photo_slot: Option<(u64, u32, u32, Vec<u8>)>,
    current_photo_id: u64,
}

static GLOBAL_ATLAS: OnceLock<Mutex<AtlasBank>> = OnceLock::new();

impl AtlasBank {
    pub fn global() -> &'static Mutex<Self> {
        GLOBAL_ATLAS.get_or_init(|| {
            Mutex::new(Self {
                epoch: 1,
                r8_shelf: ShelfPacker::new(),
                rgba_shelf: ShelfPacker::new(),
                glyphs: HashMap::with_capacity(1024),
                icons: HashMap::with_capacity(256),
                bitmaps: HashMap::with_capacity(128),
                uploads: Vec::with_capacity(128),
                photo_slot: None,
                current_photo_id: 1, // 1 = wallpaper
            })
        })
    }

    fn reset_all(&mut self) {
        self.epoch += 1;
        self.r8_shelf.reset();
        self.rgba_shelf.reset();
        self.glyphs.clear();
        self.icons.clear();
        self.bitmaps.clear();
        self.uploads.clear();
    }

    pub fn insert_r8(&mut self, w: u32, h: u32, data: Vec<u8>) -> [f32; 4] {
        let pos = match self.r8_shelf.alloc(w, h) {
            Some(p) => p,
            None => {
                self.reset_all();
                self.r8_shelf.alloc(w, h).unwrap_or((0, 0))
            }
        };
        let inv = 1.0 / (ATLAS_DIM as f32);
        let uv = [
            (pos.0 as f32) * inv,
            (pos.1 as f32) * inv,
            ((pos.0 + w) as f32) * inv,
            ((pos.1 + h) as f32) * inv,
        ];
        self.uploads.push(AtlasUpload {
            target: AtlasTarget::R8,
            x: pos.0,
            y: pos.1,
            w,
            h,
            data,
        });
        uv
    }

    pub fn insert_rgba(&mut self, w: u32, h: u32, data: Vec<u8>) -> [f32; 4] {
        let pos = match self.rgba_shelf.alloc(w, h) {
            Some(p) => p,
            None => {
                self.reset_all();
                self.rgba_shelf.alloc(w, h).unwrap_or((0, 0))
            }
        };
        let inv = 1.0 / (ATLAS_DIM as f32);
        let uv = [
            (pos.0 as f32) * inv,
            (pos.1 as f32) * inv,
            ((pos.0 + w) as f32) * inv,
            ((pos.1 + h) as f32) * inv,
        ];
        self.uploads.push(AtlasUpload {
            target: AtlasTarget::Rgba,
            x: pos.0,
            y: pos.1,
            w,
            h,
            data,
        });
        uv
    }

    pub fn set_photo(&mut self, id: u64, w: u32, h: u32, rgba: &[u8]) {
        if self.current_photo_id != id {
            self.current_photo_id = id;
            self.photo_slot = Some((id, w, h, rgba.to_vec()));
        }
    }

    pub fn request_wallpaper_in_photo_slot(&mut self) {
        self.current_photo_id = 1;
        self.photo_slot = None;
    }

    pub fn take_pending(&mut self) -> (u64, Vec<AtlasUpload>, Option<(u64, u32, u32, Vec<u8>)>) {
        (
            self.epoch,
            std::mem::take(&mut self.uploads),
            self.photo_slot.take(),
        )
    }
}

/// High-DPI GPU Instanced Uber-SDF Display-List Recorder.
///
/// Coordinates passed to drawing methods are in **logical pixels**; `scale` (e.g. `1.75` on TB520FU)
/// maps every primitive to crisp subpixel-anti-aliased physical pixels on Vulkan!
pub struct Canvas {
    pub phys_w: u32,
    pub phys_h: u32,
    pub scale: f32,
    /// Physical screen `(x, y)` origin of this Wayland surface for pixel-exact Fake-Glass UV alignment!
    pub surf_origin: [f32; 2],
    /// Physical full-screen dimensions `(w, h)` of the output (default `2944x1840`).
    pub screen_phys_size: [f32; 2],
    pub clear_color: Rgba,
    pub instances: Vec<SdfInstance>,
    /// Glass shapes drawn this frame; `WlConn::present` turns them into the surface's blur region.
    pub blur_rects: Vec<BlurRect>,
    /// False (default): glass is a translucent tint and the compositor blurs what is behind the
    /// surface. True: the canvas paints its own wallpaper (lock screen, greeter, headless scenes),
    /// where the compositor has nothing to blur, so glass samples the pre-blurred wallpaper instead.
    pub own_backdrop: bool,
    clip_phys: Option<([f32; 4], [f32; 4])>,
}

impl Canvas {
    pub fn new(phys_w: u32, phys_h: u32, scale: f32) -> Self {
        let pw = phys_w.max(1);
        let ph = phys_h.max(1);
        Self {
            phys_w: pw,
            phys_h: ph,
            scale: scale.max(0.5),
            surf_origin: [0.0, 0.0],
            screen_phys_size: [pw.max(2944) as f32, ph.max(1840) as f32],
            clear_color: Rgba::TRANSPARENT,
            instances: Vec::with_capacity(1024),
            blur_rects: Vec::new(),
            own_backdrop: OWN_BACKDROP_DEFAULT.load(std::sync::atomic::Ordering::Relaxed),
            clip_phys: None,
        }
    }

    pub fn resize(&mut self, phys_w: u32, phys_h: u32, scale: f32) {
        self.phys_w = phys_w.max(1);
        self.phys_h = phys_h.max(1);
        self.scale = scale.max(0.5);
        self.clip_phys = None;
    }

    pub fn set_screen_context(&mut self, origin_logical_x: f32, origin_logical_y: f32, screen_logical_w: f32, screen_logical_h: f32) {
        self.surf_origin = [origin_logical_x * self.scale, origin_logical_y * self.scale];
        self.screen_phys_size = [
            (screen_logical_w * self.scale).max(1.0),
            (screen_logical_h * self.scale).max(1.0),
        ];
    }

    #[inline]
    pub fn width(&self) -> u32 {
        self.phys_w
    }

    #[inline]
    pub fn height(&self) -> u32 {
        self.phys_h
    }

    #[inline]
    pub fn logical_width(&self) -> f32 {
        (self.phys_w as f32) / self.scale
    }

    #[inline]
    pub fn logical_height(&self) -> f32 {
        (self.phys_h as f32) / self.scale
    }

    #[inline]
    pub fn to_phys_rect(&self, r: RectF) -> [f32; 4] {
        [
            r.x * self.scale,
            r.y * self.scale,
            r.w * self.scale,
            r.h * self.scale,
        ]
    }

    pub fn clear(&mut self, color: Rgba) {
        self.clear_color = color;
        self.instances.clear();
        self.blur_rects.clear();
        self.clip_phys = None;
    }

    pub fn set_clip_round_rect(&mut self, clip: Option<(RectF, [f32; 4])>) {
        self.clip_phys = clip.map(|(r, rad)| {
            (
                self.to_phys_rect(r),
                rad.map(|v| (v * self.scale).max(0.0)),
            )
        });
    }

    pub fn save_clip(&self) -> Option<([f32; 4], [f32; 4])> {
        self.clip_phys
    }

    pub fn restore_clip(&mut self, saved: Option<([f32; 4], [f32; 4])>) {
        self.clip_phys = saved;
    }

    #[inline]
    pub fn push_instance(&mut self, mut inst: SdfInstance) {
        if let Some((cr, crad)) = self.clip_phys {
            inst.clip_rect = cr;
            inst.clip_radii = crad;
        }
        self.instances.push(inst);
    }

    pub fn fill_rect(&mut self, r: RectF, color: Rgba) {
        self.fill_round_rect_corners(r, [0.0; 4], color);
    }

    pub fn fill_round_rect(&mut self, r: RectF, radius: f32, color: Rgba) {
        self.fill_round_rect_corners(r, [radius; 4], color);
    }

    pub fn fill_round_rect_corners(&mut self, r: RectF, radii: [f32; 4], color: Rgba) {
        if color.a == 0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.push_instance(SdfInstance {
            rect: self.to_phys_rect(r),
            radii: radii.map(|v| (v * self.scale).max(0.0)),
            color0: color.to_vec4(),
            params: [MODE_SOLID_FILL, 0.0, 0.0, 0.0],
            ..Default::default()
        });
    }

    pub fn stroke_round_rect(&mut self, r: RectF, radius: f32, width: f32, color: Rgba) {
        self.stroke_round_rect_corners(r, [radius; 4], width, color);
    }

    pub fn stroke_round_rect_corners(&mut self, r: RectF, radii: [f32; 4], width: f32, color: Rgba) {
        if color.a == 0 || width <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.push_instance(SdfInstance {
            rect: self.to_phys_rect(r),
            radii: radii.map(|v| (v * self.scale).max(0.0)),
            color0: color.to_vec4(),
            params: [MODE_STROKE, (width * self.scale).max(1.0), 0.0, 0.0],
            ..Default::default()
        });
    }

    /// Authentic 300px Dual-Kawase Frosted Liquid Glass card (`mode = 9 FAKE_GLASS`) when `pal.glass` is true,
    /// or crisp Solid matte card when `pal.glass` is false (`SolidDark` / `SolidLight`)!
    pub fn fill_glass_panel(&mut self, r: RectF, radius: f32, pal: &Palette, opacity: f32) {
        self.fill_glass_corners(r, [radius; 4], pal, opacity);
    }

    pub fn fill_glass_corners(&mut self, r: RectF, radii: [f32; 4], pal: &Palette, opacity: f32) {
        self.fill_glass_corners_styled(
            r,
            radii,
            pal,
            pal.glass_tint,
            pal.glass_rim,
            pal.glass_sheen,
            opacity,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fill_glass_corners_styled(
        &mut self,
        r: RectF,
        radii: [f32; 4],
        pal: &Palette,
        tint: Rgba,
        rim: Rgba,
        sheen: f32,
        opacity: f32,
    ) {
        if opacity <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        if pal.glass {
            let mode = if self.own_backdrop { MODE_FAKE_GLASS } else { MODE_TINT_GLASS };
            self.push_instance(SdfInstance {
                rect: self.to_phys_rect(r),
                radii: radii.map(|v| (v * self.scale).max(0.0)),
                color0: tint.to_vec4(),
                color1: rim.to_vec4(),
                params: [mode, 0.0, opacity.clamp(0.0, 1.0), sheen],
                ..Default::default()
            });
            if !self.own_backdrop && opacity > 0.02 && r.w >= MIN_BLUR_SIDE && r.h >= MIN_BLUR_SIDE {
                self.push_blur_rect(r, radii);
            }
        } else {
            self.fill_round_rect_corners(r, radii, pal.sheet.with_alpha(opacity));
            if rim.a > 0 {
                self.stroke_round_rect_corners(r, radii, 1.0, pal.hair.with_alpha(opacity));
            }
        }
    }

    /// Authentic Liquid Glass directly sampling the 8-pass Dual-Kawase blurred wallpaper (`tex_glass`,
    /// `MODE_FAKE_GLASS`) regardless of `own_backdrop`, with zero compositor `blur_rects` cost!
    /// Ideal for `tb520fu-bar` (`exclusive_zone = 36`), where tiled windows never sit behind the bar.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_wallpaper_liquid_glass(
        &mut self,
        r: RectF,
        radii: [f32; 4],
        pal: &Palette,
        tint: Rgba,
        rim: Rgba,
        sheen: f32,
        opacity: f32,
    ) {
        if opacity <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        if pal.glass {
            self.push_instance(SdfInstance {
                rect: self.to_phys_rect(r),
                radii: radii.map(|v| (v * self.scale).max(0.0)),
                color0: tint.to_vec4(),
                color1: rim.to_vec4(),
                params: [MODE_FAKE_GLASS, 0.0, opacity.clamp(0.0, 1.0), sheen],
                ..Default::default()
            });
        } else {
            self.fill_round_rect_corners(r, radii, pal.sheet.with_alpha(opacity));
            if rim.a > 0 {
                self.stroke_round_rect_corners(r, radii, 1.0, pal.hair.with_alpha(opacity));
            }
        }
    }

    /// Ask for compositor blur behind `r` (logical). Clipped to the active clip rect and the surface.
    pub fn push_blur_rect(&mut self, r: RectF, radii: [f32; 4]) {
        let mut x0 = r.x.max(0.0);
        let mut y0 = r.y.max(0.0);
        let mut x1 = (r.x + r.w).min(self.logical_width());
        let mut y1 = (r.y + r.h).min(self.logical_height());
        let mut radii = radii;
        if let Some((cr, _)) = self.clip_phys {
            let s = self.scale;
            x0 = x0.max(cr[0] / s);
            y0 = y0.max(cr[1] / s);
            x1 = x1.min((cr[0] + cr[2]) / s);
            y1 = y1.min((cr[1] + cr[3]) / s);
        }
        if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
            return;
        }
        // a side that was cut off no longer has rounded corners
        if x0 > r.x + 0.5 { radii[0] = 0.0; radii[3] = 0.0; }
        if x1 < r.x + r.w - 0.5 { radii[1] = 0.0; radii[2] = 0.0; }
        if y0 > r.y + 0.5 { radii[0] = 0.0; radii[1] = 0.0; }
        if y1 < r.y + r.h - 0.5 { radii[2] = 0.0; radii[3] = 0.0; }
        self.blur_rects.push(BlurRect { rect: RectF::new(x0, y0, x1 - x0, y1 - y0), radii });
    }

    pub fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Rgba) {
        if color.a == 0 || width <= 0.0 {
            return;
        }
        self.push_instance(SdfInstance {
            rect: [
                x0 * self.scale,
                y0 * self.scale,
                x1 * self.scale,
                y1 * self.scale,
            ],
            color0: color.to_vec4(),
            params: [MODE_CAPSULE_LINE, (width * self.scale).max(1.0), 0.0, 0.0],
            ..Default::default()
        });
    }

    /// Single-quad analytical GPU soft drop shadow (`0.02ms` on Adreno 750 vs 5-pass CPU rasterization!).
    pub fn draw_soft_shadow(&mut self, r: RectF, radius: f32, spread: f32, dy: f32, alpha: f32) {
        if alpha <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.push_instance(SdfInstance {
            rect: self.to_phys_rect(r),
            radii: [(radius * self.scale).max(0.0); 4],
            color0: [0.0, 0.0, 0.0, alpha.clamp(0.0, 1.0)],
            params: [
                MODE_SOFT_SHADOW,
                (spread * self.scale).max(2.0),
                dy * self.scale,
                0.0,
            ],
            ..Default::default()
        });
    }

    /// Hollow outer-only GPU drop shadow for translucent Liquid Glass cards/capsules (zero black inside `r`).
    pub fn draw_glass_shadow(&mut self, r: RectF, radius: f32, spread: f32, dy: f32, alpha: f32) {
        if alpha <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.push_instance(SdfInstance {
            rect: self.to_phys_rect(r),
            radii: [(radius * self.scale).max(0.0); 4],
            color0: [0.0, 0.0, 0.0, alpha.clamp(0.0, 1.0)],
            params: [
                MODE_SOFT_SHADOW,
                (spread * self.scale).max(2.0),
                dy * self.scale,
                1.0,
            ],
            ..Default::default()
        });
    }

    pub fn fill_radial_glow(&mut self, bounds: RectF, radius: f32, cx: f32, cy: f32, glow_r: f32, color: Rgba) {
        if color.a == 0 || glow_r <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
            return;
        }
        self.push_instance(SdfInstance {
            rect: self.to_phys_rect(bounds),
            radii: [(radius * self.scale).max(0.0); 4],
            color0: color.to_vec4(),
            uv_rect: [cx * self.scale, cy * self.scale, glow_r * self.scale, 0.0],
            params: [MODE_RADIAL_GLOW, 0.0, 0.0, 0.0],
            ..Default::default()
        });
    }

    pub fn fill_h_gradient(&mut self, r: RectF, radius: f32, left: Rgba, right: Rgba) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.push_instance(SdfInstance {
            rect: self.to_phys_rect(r),
            radii: [(radius * self.scale).max(0.0); 4],
            color0: left.to_vec4(),
            color1: right.to_vec4(),
            params: [MODE_H_GRADIENT, 0.0, 0.0, 0.0],
            ..Default::default()
        });
    }

    pub fn fill_v_gradient(&mut self, r: RectF, radius: f32, top: Rgba, bottom: Rgba) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.push_instance(SdfInstance {
            rect: self.to_phys_rect(r),
            radii: [(radius * self.scale).max(0.0); 4],
            color0: top.to_vec4(),
            color1: bottom.to_vec4(),
            params: [MODE_V_GRADIENT, 0.0, 0.0, 0.0],
            ..Default::default()
        });
    }

    /// Blit the full-screen TB520FU plume/user wallpaper into `r` (for Desktop Background, Lockscreen & `--render-png` previews).
    pub fn draw_wallpaper_background(&mut self, r: RectF, opacity: f32) {
        if let Some(vk_mtx) = VkRenderer::global() {
            vk_mtx.lock().unwrap().bind_wallpaper_to_photo_slot();
        }
        AtlasBank::global().lock().unwrap().request_wallpaper_in_photo_slot();
        self.push_instance(SdfInstance {
            rect: self.to_phys_rect(r),
            color0: [1.0, 1.0, 1.0, opacity.clamp(0.0, 1.0)],
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            params: [MODE_PHOTO_BLIT, 0.0, 0.0, 0.0],
            ..Default::default()
        });
    }

    /// Check whether `VkRenderer` already holds a valid premultiplied RGBA snapshot for `(key, phys_w, phys_h)`.
    pub fn has_cached_snapshot(&self, key: u64) -> bool {
        let Some(vk_mtx) = VkRenderer::global() else {
            return false;
        };
        vk_mtx
            .lock()
            .unwrap()
            .has_valid_cache(key, self.phys_w, self.phys_h)
    }

    /// Render the current display list (`self.instances`) into the GPU snapshot texture (`tex_cache`) under `key`.
    pub fn store_cached_snapshot(&self, key: u64) -> bool {
        let Some(vk_mtx) = VkRenderer::global() else {
            return false;
        };
        let mut vk = vk_mtx.lock().unwrap();
        vk.render_to_cache(
            key,
            self.phys_w,
            self.phys_h,
            self.clear_color,
            self.surf_origin,
            self.screen_phys_size,
            &self.instances,
        )
        .is_ok()
    }

    pub fn invalidate_cached_snapshot() {
        if let Some(vk_mtx) = VkRenderer::global() {
            vk_mtx.lock().unwrap().invalidate_cache();
        }
    }

    /// Blit the cached GPU UI snapshot (`tex_cache`) into `dst` with UV sub-rect `uv = [u0, v0, u1, v1]`.
    pub fn draw_cached_snapshot(&mut self, dst: RectF, uv: [f32; 4], opacity: f32) {
        if opacity <= 0.0 || dst.w <= 0.0 || dst.h <= 0.0 {
            return;
        }
        self.push_instance(SdfInstance {
            rect: self.to_phys_rect(dst),
            color0: [1.0, 1.0, 1.0, opacity.clamp(0.0, 1.0)],
            uv_rect: uv,
            params: [MODE_CACHE_BLIT, 0.0, 0.0, 0.0],
            ..Default::default()
        });
    }

    /// Render `self` via Vulkan offscreen target into Wayland `wl_shm::Format::Argb8888` (`BGRA` premultiplied).
    pub fn render_to_wayland_argb8888(&self, dst: &mut [u8]) -> bool {
        let Some(vk_mtx) = VkRenderer::global() else {
            return false;
        };
        let mut vk = vk_mtx.lock().unwrap();
        let Ok(rgba) = vk.render_offscreen_rgba(
            self.phys_w,
            self.phys_h,
            self.clear_color,
            self.surf_origin,
            self.screen_phys_size,
            &self.instances,
        ) else {
            return false;
        };
        let len = rgba.len().min(dst.len()) & !3;
        for (s, d) in rgba[..len].chunks_exact(4).zip(dst[..len].chunks_exact_mut(4)) {
            d[0] = s[2]; // B
            d[1] = s[1]; // G
            d[2] = s[0]; // R
            d[3] = s[3]; // A
        }
        true
    }

    /// Render `self` on the Vulkan GPU (`VkRenderer`) and write straight-alpha PNG to `out_path`!
    pub fn save_png(&self, out_path: &str) -> Result<(), String> {
        let vk_mtx = VkRenderer::global().ok_or("Vulkan renderer unavailable")?;
        let mut vk = vk_mtx.lock().unwrap();
        let premul = vk.render_offscreen_rgba(
            self.phys_w,
            self.phys_h,
            self.clear_color,
            self.surf_origin,
            self.screen_phys_size,
            &self.instances,
        )?;
        let mut straight = vec![0u8; premul.len()];
        for (s, d) in premul.chunks_exact(4).zip(straight.chunks_exact_mut(4)) {
            let a = s[3] as u32;
            if a == 255 {
                d.copy_from_slice(s);
            } else if a == 0 {
                d.copy_from_slice(&[0, 0, 0, 0]);
            } else {
                d[0] = ((s[0] as u32 * 255 + a / 2) / a).min(255) as u8;
                d[1] = ((s[1] as u32 * 255 + a / 2) / a).min(255) as u8;
                d[2] = ((s[2] as u32 * 255 + a / 2) / a).min(255) as u8;
                d[3] = s[3];
            }
        }
        drop(vk);

        if let Some(parent) = std::path::Path::new(out_path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let file = std::fs::File::create(out_path).map_err(|e| format!("create {out_path}: {e}"))?;
        let mut enc = png::Encoder::new(BufWriter::new(file), self.phys_w, self.phys_h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&straight).map_err(|e| e.to_string())
    }
}
