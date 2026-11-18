// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};
use tiny_skia::Color;

/// Straight (non-premultiplied) RGBA color in sRGB space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Self = Self { r: 0, g: 0, b: 0, a: 0 };
    pub const WHITE: Self = Self { r: 255, g: 255, b: 255, a: 255 };
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0, a: 255 };

    #[inline]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    #[inline]
    pub fn rgba_f(r: u8, g: u8, b: u8, alpha: f32) -> Self {
        let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self { r, g, b, a }
    }

    #[inline]
    pub const fn hex(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xFF) as u8,
            g: ((rgb >> 8) & 0xFF) as u8,
            b: (rgb & 0xFF) as u8,
            a: 255,
        }
    }

    #[inline]
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self {
            a: ((self.a as f32) * alpha.clamp(0.0, 1.0)).round() as u8,
            ..self
        }
    }

    #[inline]
    pub fn to_vec4(self) -> [f32; 4] {
        [
            (self.r as f32) / 255.0,
            (self.g as f32) / 255.0,
            (self.b as f32) / 255.0,
            (self.a as f32) / 255.0,
        ]
    }

    #[inline]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let inv = 1.0 - t;
        Self {
            r: (self.r as f32 * inv + other.r as f32 * t).round() as u8,
            g: (self.g as f32 * inv + other.g as f32 * t).round() as u8,
            b: (self.b as f32 * inv + other.b as f32 * t).round() as u8,
            a: (self.a as f32 * inv + other.a as f32 * t).round() as u8,
        }
    }

    #[inline]
    pub fn to_skia(self) -> Color {
        Color::from_rgba8(self.r, self.g, self.b, self.a)
    }
}

/// 3-Tier Liquid Glass + OriginOS/ColorOS 4 skins (`glass-dark`, `glass-light`, `solid-dark`, `solid-light`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Skin {
    #[default]
    GlassDark,
    GlassLight,
    SolidDark,
    SolidLight,
}

impl Skin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GlassDark => "glass-dark",
            Self::GlassLight => "glass-light",
            Self::SolidDark => "solid-dark",
            Self::SolidLight => "solid-light",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s {
            "glass-light" => Self::GlassLight,
            "solid-dark" => Self::SolidDark,
            "solid-light" => Self::SolidLight,
            _ => Self::GlassDark,
        }
    }

    pub fn is_light(self) -> bool {
        matches!(self, Self::GlassLight | Self::SolidLight)
    }

    pub fn is_glass(self) -> bool {
        matches!(self, Self::GlassDark | Self::GlassLight)
    }

    pub fn toggle_night(self) -> Self {
        match self {
            Self::GlassDark => Self::GlassLight,
            Self::GlassLight => Self::GlassDark,
            Self::SolidDark => Self::SolidLight,
            Self::SolidLight => Self::SolidDark,
        }
    }
}

/// System-wide accent color tokens synchronized between `tb520fu-shell`, `tb520fu-osk`, and `tb520fu-settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AccentColor {
    #[default]
    Blue,
    Indigo,
    Teal,
    Amber,
    Rose,
    Emerald,
}

impl AccentColor {
    pub const ALL: [Self; 6] = [
        Self::Blue,
        Self::Indigo,
        Self::Teal,
        Self::Amber,
        Self::Rose,
        Self::Emerald,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Indigo => "indigo",
            Self::Teal => "teal",
            Self::Amber => "amber",
            Self::Rose => "rose",
            Self::Emerald => "emerald",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s.trim() {
            "indigo" | "purple" => Self::Indigo,
            "teal" | "cyan" => Self::Teal,
            "amber" | "orange" => Self::Amber,
            "rose" | "pink" | "red" => Self::Rose,
            "emerald" | "green" => Self::Emerald,
            _ => Self::Blue,
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Blue => 0,
            Self::Indigo => 1,
            Self::Teal => 2,
            Self::Amber => 3,
            Self::Rose => 4,
            Self::Emerald => 5,
        }
    }

    pub fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Indigo,
            2 => Self::Teal,
            3 => Self::Amber,
            4 => Self::Rose,
            5 => Self::Emerald,
            _ => Self::Blue,
        }
    }

    /// Returns `(acc, acc_press_light, acc_press_dark, acc_ink_light, acc_ink_dark, drawer_dark)`.
    pub fn swatches(self) -> (Rgba, Rgba, Rgba, Rgba, Rgba, Rgba) {
        match self {
            Self::Blue => (
                Rgba::hex(0x2e6bf6),
                Rgba::hex(0x2658ca),
                Rgba::hex(0x5486f8),
                Rgba::hex(0x2e6bf6),
                Rgba::hex(0x7cb0ff),
                Rgba::hex(0x93c5fd),
            ),
            Self::Indigo => (
                Rgba::hex(0x6366f1),
                Rgba::hex(0x4f46e5),
                Rgba::hex(0x818cf8),
                Rgba::hex(0x4f46e5),
                Rgba::hex(0xa5b4fc),
                Rgba::hex(0xc7d2fe),
            ),
            Self::Teal => (
                Rgba::hex(0x0d9488),
                Rgba::hex(0x0f766e),
                Rgba::hex(0x2dd4bf),
                Rgba::hex(0x0f766e),
                Rgba::hex(0x5eead4),
                Rgba::hex(0x99f6e4),
            ),
            Self::Amber => (
                Rgba::hex(0xd97706),
                Rgba::hex(0xb45309),
                Rgba::hex(0xf59e0b),
                Rgba::hex(0xb45309),
                Rgba::hex(0xfbbf24),
                Rgba::hex(0xfde68a),
            ),
            Self::Rose => (
                Rgba::hex(0xe11d48),
                Rgba::hex(0xbe123c),
                Rgba::hex(0xfb7185),
                Rgba::hex(0xbe123c),
                Rgba::hex(0xfda4af),
                Rgba::hex(0xfecdd3),
            ),
            Self::Emerald => (
                Rgba::hex(0x059669),
                Rgba::hex(0x047857),
                Rgba::hex(0x34d399),
                Rgba::hex(0x047857),
                Rgba::hex(0x6ee7b7),
                Rgba::hex(0xa7f3d0),
            ),
        }
    }
}

/// Full palette tokens derived from `Skin`, including 300px Liquid Glass tint/rim/specular tokens.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub skin: Skin,
    pub light: bool,
    pub glass: bool,
    pub mat: Rgba,
    pub glass_tint: Rgba,
    pub glass_rim: Rgba,
    pub glass_sheen: f32,
    pub hair: Rgba,
    pub key: Rgba,
    pub key_p: Rgba,
    pub fn_key: Rgba,
    pub fn_key_p: Rgba,
    pub lip: Rgba,
    pub lip_p: Rgba,
    pub edge: Rgba,
    pub ink: Rgba,
    pub ink2: Rgba,
    pub ink3: Rgba,
    pub icon: Rgba,
    pub icon2: Rgba,
    pub acc: Rgba,
    pub acc_p: Rgba,
    pub acc_ink: Rgba,
    pub active_drawer_ink: Rgba,
    pub acc_lip: Rgba,
    pub acc_soft: Rgba,
    pub pop: Rgba,
    pub pop_line: Rgba,
    pub card: Rgba,
    pub card_p: Rgba,
    pub chip: Rgba,
    pub sep: Rgba,
    pub sheet: Rgba,
    pub pill_track: Rgba,
    pub pill_fill: Rgba,
    pub pill_fill_ink: Rgba,
    pub danger: Rgba,
    pub ok: Rgba,
    pub warn: Rgba,
    pub press_sinks: bool,
}

impl Palette {
    pub fn for_skin(skin: Skin) -> Self {
        Self::for_skin_and_accent(skin, AccentColor::Blue)
    }

    pub fn for_skin_and_accent(skin: Skin, accent: AccentColor) -> Self {
        let light = skin.is_light();
        let glass = skin.is_glass();
        let (acc, acc_pl, acc_pd, ink_l, ink_d, drawer_d) = accent.swatches();
        let card = match skin {
            Skin::GlassLight => Rgba::rgba_f(255, 255, 255, 0.56),
            Skin::SolidLight => Rgba::hex(0xffffff),
            Skin::SolidDark => Rgba::hex(0x2e313a),
            Skin::GlassDark => Rgba::rgba_f(255, 255, 255, 0.12),
        };
        let ink = if light {
            Rgba::hex(0x141824)
        } else if skin == Skin::SolidDark {
            Rgba::hex(0xedf0f7)
        } else {
            Rgba::rgba_f(255, 255, 255, 0.96)
        };
        Self {
            skin,
            light,
            glass,
            mat: match skin {
                Skin::GlassLight => Rgba::rgba_f(244, 246, 252, 0.78),
                Skin::SolidLight => Rgba::hex(0xebeff6),
                Skin::SolidDark => Rgba::hex(0x1c1f27),
                Skin::GlassDark => Rgba::rgba_f(18, 22, 34, 0.76),
            },
            glass_tint: match skin {
                Skin::GlassLight => Rgba::rgba_f(244, 246, 252, 0.78),
                Skin::GlassDark => Rgba::rgba_f(18, 22, 34, 0.76),
                Skin::SolidLight => Rgba::hex(0xebeff6),
                Skin::SolidDark => Rgba::hex(0x1c1f27),
            },
            glass_rim: if light {
                Rgba::rgba_f(255, 255, 255, 0.65)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.12)
            },
            glass_sheen: if light { 0.04 } else { 0.018 },
            hair: match skin {
                Skin::GlassLight => Rgba::rgba_f(255, 255, 255, 0.65),
                Skin::SolidLight => Rgba::rgba_f(0, 0, 0, 0.07),
                Skin::SolidDark => Rgba::rgba_f(255, 255, 255, 0.08),
                Skin::GlassDark => Rgba::rgba_f(255, 255, 255, 0.10),
            },
            key: match skin {
                Skin::GlassLight => Rgba::rgba_f(255, 255, 255, 0.54),
                Skin::SolidLight => Rgba::hex(0xffffff),
                Skin::SolidDark => Rgba::hex(0x4d4d4d),
                Skin::GlassDark => Rgba::rgba_f(255, 255, 255, 0.18),
            },
            key_p: match skin {
                Skin::GlassLight => Rgba::rgba_f(255, 255, 255, 0.18),
                Skin::SolidLight => Rgba::hex(0xbfc5cf),
                Skin::SolidDark => Rgba::hex(0x5c5c5c),
                Skin::GlassDark => Rgba::rgba_f(255, 255, 255, 0.28),
            },
            fn_key: match skin {
                Skin::GlassLight => Rgba::rgba_f(150, 158, 173, 0.40),
                Skin::SolidLight => Rgba::hex(0xbfc5cf),
                Skin::SolidDark => Rgba::hex(0x333333),
                Skin::GlassDark => Rgba::rgba_f(255, 255, 255, 0.09),
            },
            fn_key_p: match skin {
                Skin::GlassLight => Rgba::rgba_f(148, 156, 173, 0.14),
                Skin::SolidLight => Rgba::hex(0x909499),
                Skin::SolidDark => Rgba::hex(0x4d4d4d),
                Skin::GlassDark => Rgba::rgba_f(255, 255, 255, 0.18),
            },
            lip: match skin {
                Skin::SolidLight => Rgba::hex(0xa7a9b0),
                Skin::SolidDark => Rgba::hex(0x131313),
                Skin::GlassLight => Rgba::rgba_f(0, 0, 0, 0.26),
                Skin::GlassDark => Rgba::rgba_f(0, 0, 0, 0.30),
            },
            lip_p: match skin {
                Skin::GlassLight => Rgba::rgba_f(0, 0, 0, 0.09),
                Skin::SolidLight | Skin::SolidDark => Rgba::TRANSPARENT,
                Skin::GlassDark => Rgba::rgba_f(0, 0, 0, 0.10),
            },
            edge: if skin == Skin::SolidDark {
                Rgba::rgba_f(255, 255, 255, 0.05)
            } else if light {
                Rgba::TRANSPARENT
            } else {
                Rgba::rgba_f(255, 255, 255, 0.07)
            },
            ink,
            ink2: if light {
                Rgba::rgba_f(31, 36, 48, 0.42)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.42)
            },
            ink3: if light {
                Rgba::rgba_f(31, 36, 48, 0.64)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.66)
            },
            icon: ink,
            icon2: if light {
                Rgba::hex(0x283040)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.88)
            },
            acc,
            acc_p: if light { acc_pl } else { acc_pd },
            acc_ink: if light { ink_l } else { ink_d },
            // High-contrast active item ink inside Wi-Fi / BT drawers
            active_drawer_ink: if light { ink_l } else { drawer_d },
            acc_lip: match skin {
                Skin::SolidLight => acc.lerp(Rgba::BLACK, 0.62),
                Skin::GlassLight => acc.lerp(Rgba::BLACK, 0.58).with_alpha(0.45),
                _ => acc.lerp(Rgba::BLACK, 0.72).with_alpha(0.55),
            },
            acc_soft: if light {
                acc.with_alpha(0.16)
            } else {
                acc.with_alpha(0.28)
            },
            pop: if light {
                Rgba::hex(0xfcfefd)
            } else if skin == Skin::SolidDark {
                Rgba::hex(0x3e424f)
            } else {
                Rgba::hex(0x3b404e)
            },
            pop_line: if light {
                Rgba::hex(0xc5cbd6)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.14)
            },
            card,
            card_p: match skin {
                Skin::GlassLight => Rgba::rgba_f(255, 255, 255, 0.78),
                Skin::SolidLight => Rgba::hex(0xdfe4ed),
                Skin::SolidDark => Rgba::hex(0x414654),
                Skin::GlassDark => Rgba::rgba_f(255, 255, 255, 0.20),
            },
            chip: match skin {
                Skin::GlassLight => Rgba::rgba_f(0, 0, 0, 0.065),
                Skin::SolidLight => Rgba::hex(0xe4e8f0),
                Skin::SolidDark => Rgba::hex(0x363a46),
                Skin::GlassDark => Rgba::rgba_f(255, 255, 255, 0.13),
            },
            sep: if light {
                Rgba::rgba_f(0, 0, 0, 0.08)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.10)
            },
            sheet: match skin {
                Skin::GlassLight => Rgba::rgba_f(238, 242, 249, 0.88),
                Skin::SolidLight => Rgba::hex(0xf4f6fa),
                Skin::SolidDark => Rgba::hex(0x232731),
                Skin::GlassDark => Rgba::rgba_f(24, 28, 40, 0.86),
            },
            pill_track: if light { Rgba::rgba_f(0, 0, 0, 0.08) } else { card },
            pill_fill: if light {
                Rgba::hex(0xffffff)
            } else {
                Rgba::rgba_f(255, 255, 255, 0.94)
            },
            pill_fill_ink: Rgba::hex(0x1b1f28),
            danger: Rgba::hex(0xff453a),
            ok: Rgba::hex(0x32d74b),
            warn: Rgba::hex(0xff9f0a),
            press_sinks: light,
        }
    }
}

/// Monet palette + top-strip luminance extracted once when the wallpaper is loaded.
#[derive(Debug, Clone, Copy)]
pub struct WallpaperMonet {
    pub accent: Rgba,
    pub bar_glass_dark: Rgba,
    pub bar_glass_light: Rgba,
    pub bar_rim_dark: Rgba,
    pub top_luma: f32,
}

impl Default for WallpaperMonet {
    fn default() -> Self {
        Self::from_accent_and_luma(Rgba::hex(0xe88a48), 0.66)
    }
}

impl WallpaperMonet {
    pub fn from_accent_and_luma(accent: Rgba, top_luma: f32) -> Self {
        let tl = top_luma.clamp(0.0, 1.0);
        // Adapt dark-glass alpha to top-strip brightness so white statusbar ink stays crisp even over bright silver skies
        let dark_alpha = (0.36 + 0.32 * tl).clamp(0.36, 0.68);
        let light_alpha = (0.58 - 0.16 * tl).clamp(0.40, 0.58);
        let bar_glass_dark = Rgba::rgb(12, 16, 26).lerp(accent, 0.20).with_alpha(dark_alpha);
        let bar_glass_light = Rgba::rgb(245, 247, 252).lerp(accent, 0.18).with_alpha(light_alpha);
        let bar_rim_dark = Rgba::WHITE.lerp(accent, 0.35).with_alpha(0.18);
        Self {
            accent,
            bar_glass_dark,
            bar_glass_light,
            bar_rim_dark,
            top_luma: tl,
        }
    }

    /// Extract dominant vibrant hue via a 12-bin chroma-weighted hue histogram (so complementary
    /// hues like warm amber + cool slate never cancel into muddy brown) plus top-strip luminance.
    /// Accepts either RGBA (`stride = 4`, `(r,g,b)` at `[0,1,2]`) or BGRA (`bgra = true`, `(b,g,r)` at `[0,1,2]`).
    pub fn extract(pixels: &[u8], w: usize, h: usize, bgra: bool) -> Self {
        if w == 0 || h == 0 || pixels.len() < w * h * 4 {
            return Self::default();
        }

        // 1. Top ~4.5% statusbar strip luminance
        let top_h = (h / 22).clamp(1, h);
        let sx_top = (w / 96).max(1);
        let sy_top = (top_h / 6).max(1);
        let mut luma_sum = 0.0_f64;
        let mut luma_cnt = 0usize;
        for y in (0..top_h).step_by(sy_top) {
            for x in (0..w).step_by(sx_top) {
                let i = (y * w + x) * 4;
                let (r, g, b) = if bgra {
                    (pixels[i + 2] as f64, pixels[i + 1] as f64, pixels[i] as f64)
                } else {
                    (pixels[i] as f64, pixels[i + 1] as f64, pixels[i + 2] as f64)
                };
                luma_sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0;
                luma_cnt += 1;
            }
        }
        let top_luma = if luma_cnt > 0 {
            (luma_sum / (luma_cnt as f64)) as f32
        } else {
            0.5
        };

        // 2. 12-bin hue histogram across entire wallpaper
        let mut bin_w = [0.0_f64; 12];
        let mut bin_rgb = [[0.0_f64; 3]; 12];
        let sx = (w / 64).max(1);
        let sy = (h / 40).max(1);
        for y in (0..h).step_by(sy) {
            for x in (0..w).step_by(sx) {
                let i = (y * w + x) * 4;
                let (r, g, b) = if bgra {
                    (pixels[i + 2] as f32, pixels[i + 1] as f32, pixels[i] as f32)
                } else {
                    (pixels[i] as f32, pixels[i + 1] as f32, pixels[i + 2] as f32)
                };
                let max_c = r.max(g).max(b);
                let min_c = r.min(g).min(b);
                let delta = max_c - min_c;
                let chroma = delta / 255.0;
                let val = max_c / 255.0;
                if chroma < 0.12 || val < 0.12 || val > 0.97 {
                    continue;
                }
                let mut hue = if (max_c - r).abs() < 1e-3 {
                    ((g - b) / delta).rem_euclid(6.0)
                } else if (max_c - g).abs() < 1e-3 {
                    ((b - r) / delta) + 2.0
                } else {
                    ((r - g) / delta) + 4.0
                } * 60.0;
                if hue < 0.0 {
                    hue += 360.0;
                }
                let bin = (((hue / 30.0).floor() as usize) % 12).min(11);
                let wgt = (chroma * chroma * (0.35 + 0.65 * val)) as f64;
                bin_w[bin] += wgt;
                bin_rgb[bin][0] += (r as f64) * wgt;
                bin_rgb[bin][1] += (g as f64) * wgt;
                bin_rgb[bin][2] += (b as f64) * wgt;
            }
        }

        let mut best_bin = 0usize;
        let mut best_score = 0.0_f64;
        for b in 0..12 {
            if bin_w[b] > best_score {
                best_score = bin_w[b];
                best_bin = b;
            }
        }

        let accent = if best_score > 1e-5 {
            let prev = (best_bin + 11) % 12;
            let next = (best_bin + 1) % 12;
            let tw = bin_w[best_bin] + 0.35 * (bin_w[prev] + bin_w[next]);
            let mr = (bin_rgb[best_bin][0] + 0.35 * (bin_rgb[prev][0] + bin_rgb[next][0])) / tw;
            let mg = (bin_rgb[best_bin][1] + 0.35 * (bin_rgb[prev][1] + bin_rgb[next][1])) / tw;
            let mb = (bin_rgb[best_bin][2] + 0.35 * (bin_rgb[prev][2] + bin_rgb[next][2])) / tw;
            Rgba::rgb(
                mr.round().clamp(0.0, 255.0) as u8,
                mg.round().clamp(0.0, 255.0) as u8,
                mb.round().clamp(0.0, 255.0) as u8,
            )
        } else {
            Rgba::hex(0x4b83f2)
        };

        Self::from_accent_and_luma(accent, top_luma)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monet_histogram_preserves_vibrant_hue_without_muddy_cancellation() {
        let (w, h) = (64usize, 40usize);
        let mut img = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if y < 4 {
                    // Silver top edge (#a8aeb8)
                    img[i..i + 4].copy_from_slice(&[168, 174, 184, 255]);
                } else if x < 38 {
                    // Warm coral-amber feather (#f07838)
                    img[i..i + 4].copy_from_slice(&[240, 120, 56, 255]);
                } else {
                    // Complementary cool slate-blue (#3b6898)
                    img[i..i + 4].copy_from_slice(&[59, 104, 152, 255]);
                }
            }
        }
        let m = WallpaperMonet::extract(&img, w, h, false);
        assert!(m.top_luma > 0.60, "silver top edge must report bright top_luma");
        assert!(
            m.accent.r > 200 && m.accent.r > m.accent.b + 100,
            "dominant warm coral bin must win cleanly instead of cancelling with blue: {:?}",
            m.accent
        );
    }
}
