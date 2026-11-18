// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use svgtypes::{PathParser, PathSegment};
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

use crate::canvas::{
    build_round_rect_path, AtlasBank, Canvas, RectF, SdfInstance, MODE_R8_ATLAS,
};
use crate::color::Rgba;

#[derive(Debug, Clone)]
enum Prim {
    Path { path: Path, filled: bool },
}

#[derive(Debug, Clone)]
pub struct VectorIcon {
    pub view_w: f32,
    pub view_h: f32,
    prims: Vec<Prim>,
}

#[allow(clippy::too_many_arguments)]
fn arc_to_cubics(
    pb: &mut PathBuilder,
    x1: f64,
    y1: f64,
    mut rx: f64,
    mut ry: f64,
    x_axis_rotation_deg: f64,
    large_arc: bool,
    sweep: bool,
    x2: f64,
    y2: f64,
) {
    if (x1 - x2).abs() < 1e-7 && (y1 - y2).abs() < 1e-7 {
        return;
    }
    rx = rx.abs();
    ry = ry.abs();
    if rx < 1e-7 || ry < 1e-7 {
        pb.line_to(x2 as f32, y2 as f32);
        return;
    }
    let phi = x_axis_rotation_deg.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();

    let dx2 = (x1 - x2) * 0.5;
    let dy2 = (y1 - y2) * 0.5;
    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;

    let rx_sq = rx * rx;
    let ry_sq = ry * ry;
    let x1p_sq = x1p * x1p;
    let y1p_sq = y1p * y1p;

    let lambda = x1p_sq / rx_sq + y1p_sq / ry_sq;
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let rx_sq = rx * rx;
    let ry_sq = ry * ry;

    let num = (rx_sq * ry_sq - rx_sq * y1p_sq - ry_sq * x1p_sq).max(0.0);
    let den = rx_sq * y1p_sq + ry_sq * x1p_sq;
    let mut coef = if den > 1e-12 { (num / den).sqrt() } else { 0.0 };
    if large_arc == sweep {
        coef = -coef;
    }
    let cxp = coef * (rx * y1p / ry);
    let cyp = coef * (-ry * x1p / rx);

    let cx = cos_phi * cxp - sin_phi * cyp + (x1 + x2) * 0.5;
    let cy = sin_phi * cxp + cos_phi * cyp + (y1 + y2) * 0.5;

    fn vec_angle(ux: f64, uy: f64, vx: f64, vy: f64) -> f64 {
        let dot = ux * vx + uy * vy;
        let len = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt().max(1e-12);
        let mut ang = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            ang = -ang;
        }
        ang
    }

    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;

    let theta1 = vec_angle(1.0, 0.0, ux, uy);
    let mut dtheta = vec_angle(ux, uy, vx, vy);
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f64::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f64::consts::TAU;
    }

    let segs = ((dtheta.abs() / (std::f64::consts::FRAC_PI_2 + 1e-4)).ceil() as usize).max(1);
    let delta = dtheta / (segs as f64);
    let alpha = (4.0 / 3.0) * (delta * 0.25).tan();

    for i in 0..segs {
        let t1 = theta1 + (i as f64) * delta;
        let t2 = t1 + delta;
        let (s1, c1) = t1.sin_cos();
        let (s2, c2) = t2.sin_cos();

        let p1x = c1 - alpha * s1;
        let p1y = s1 + alpha * c1;
        let p2x = c2 + alpha * s2;
        let p2y = s2 - alpha * c2;
        let p3x = c2;
        let p3y = s2;

        let map = |px: f64, py: f64| -> (f32, f32) {
            let sx = px * rx;
            let sy = py * ry;
            (
                (cos_phi * sx - sin_phi * sy + cx) as f32,
                (sin_phi * sx + cos_phi * sy + cy) as f32,
            )
        };
        let (c1x, c1y) = map(p1x, p1y);
        let (c2x, c2y) = map(p2x, p2y);
        let (ex, ey) = map(p3x, p3y);
        pb.cubic_to(c1x, c1y, c2x, c2y, ex, ey);
    }
}

fn parse_svg_d(d: &str) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let (mut cx, mut cy) = (0.0_f64, 0.0_f64);
    let (mut sx, mut sy) = (0.0_f64, 0.0_f64);
    let mut prev_ctrl: Option<(f64, f64)> = None;

    for seg in PathParser::from(d) {
        let Ok(seg) = seg else { break };
        match seg {
            PathSegment::MoveTo { abs, x, y } => {
                if abs {
                    cx = x;
                    cy = y;
                } else {
                    cx += x;
                    cy += y;
                }
                sx = cx;
                sy = cy;
                pb.move_to(cx as f32, cy as f32);
                prev_ctrl = None;
            }
            PathSegment::LineTo { abs, x, y } => {
                if abs {
                    cx = x;
                    cy = y;
                } else {
                    cx += x;
                    cy += y;
                }
                pb.line_to(cx as f32, cy as f32);
                prev_ctrl = None;
            }
            PathSegment::HorizontalLineTo { abs, x } => {
                cx = if abs { x } else { cx + x };
                pb.line_to(cx as f32, cy as f32);
                prev_ctrl = None;
            }
            PathSegment::VerticalLineTo { abs, y } => {
                cy = if abs { y } else { cy + y };
                pb.line_to(cx as f32, cy as f32);
                prev_ctrl = None;
            }
            PathSegment::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let (ax1, ay1, ax2, ay2, ax, ay) = if abs {
                    (x1, y1, x2, y2, x, y)
                } else {
                    (cx + x1, cy + y1, cx + x2, cy + y2, cx + x, cy + y)
                };
                pb.cubic_to(
                    ax1 as f32, ay1 as f32, ax2 as f32, ay2 as f32, ax as f32, ay as f32,
                );
                prev_ctrl = Some((ax2, ay2));
                cx = ax;
                cy = ay;
            }
            PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                let (ax1, ay1) = match prev_ctrl {
                    Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                    None => (cx, cy),
                };
                let (ax2, ay2, ax, ay) = if abs {
                    (x2, y2, x, y)
                } else {
                    (cx + x2, cy + y2, cx + x, cy + y)
                };
                pb.cubic_to(
                    ax1 as f32, ay1 as f32, ax2 as f32, ay2 as f32, ax as f32, ay as f32,
                );
                prev_ctrl = Some((ax2, ay2));
                cx = ax;
                cy = ay;
            }
            PathSegment::Quadratic { abs, x1, y1, x, y } => {
                let (ax1, ay1, ax, ay) = if abs {
                    (x1, y1, x, y)
                } else {
                    (cx + x1, cy + y1, cx + x, cy + y)
                };
                pb.quad_to(ax1 as f32, ay1 as f32, ax as f32, ay as f32);
                prev_ctrl = Some((ax1, ay1));
                cx = ax;
                cy = ay;
            }
            PathSegment::SmoothQuadratic { abs, x, y } => {
                let (ax1, ay1) = match prev_ctrl {
                    Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                    None => (cx, cy),
                };
                let (ax, ay) = if abs { (x, y) } else { (cx + x, cy + y) };
                pb.quad_to(ax1 as f32, ay1 as f32, ax as f32, ay as f32);
                prev_ctrl = Some((ax1, ay1));
                cx = ax;
                cy = ay;
            }
            PathSegment::EllipticalArc {
                abs,
                rx,
                ry,
                x_axis_rotation,
                large_arc,
                sweep,
                x,
                y,
            } => {
                let (ax, ay) = if abs { (x, y) } else { (cx + x, cy + y) };
                arc_to_cubics(
                    &mut pb,
                    cx,
                    cy,
                    rx,
                    ry,
                    x_axis_rotation,
                    large_arc,
                    sweep,
                    ax,
                    ay,
                );
                cx = ax;
                cy = ay;
                prev_ctrl = None;
            }
            PathSegment::ClosePath { .. } => {
                cx = sx;
                cy = sy;
                pb.close();
                prev_ctrl = None;
            }
        }
    }
    pb.finish()
}

fn attr_val<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{name}=\"");
    let idx = tag.find(&pat)?;
    let rest = &tag[idx + pat.len()..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn attr_f32(tag: &str, name: &str, default: f32) -> f32 {
    attr_val(tag, name)
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(default)
}

fn parse_icon_snippet(name: &str, snippet: &str) -> VectorIcon {
    let (view_w, view_h) = if name == "__space" {
        (48.0, 24.0)
    } else {
        (24.0, 24.0)
    };
    let mut prims = Vec::new();
    let mut rem = snippet;
    while let Some(lt) = rem.find('<') {
        rem = &rem[lt + 1..];
        let Some(gt) = rem.find('>') else { break };
        let tag = &rem[..gt];
        rem = &rem[gt + 1..];
        let filled = attr_val(tag, "fill") == Some("currentColor");
        if tag.starts_with("path ") {
            if let Some(d) = attr_val(tag, "d") {
                if let Some(path) = parse_svg_d(d) {
                    prims.push(Prim::Path { path, filled });
                }
            }
        } else if tag.starts_with("rect ") {
            let x = attr_f32(tag, "x", 0.0);
            let y = attr_f32(tag, "y", 0.0);
            let w = attr_f32(tag, "width", 0.0);
            let h = attr_f32(tag, "height", 0.0);
            let rx = attr_f32(tag, "rx", 0.0);
            if let Some(path) = build_round_rect_path(RectF::new(x, y, w, h), [rx; 4]) {
                prims.push(Prim::Path { path, filled });
            }
        } else if tag.starts_with("circle ") {
            let cx = attr_f32(tag, "cx", 0.0);
            let cy = attr_f32(tag, "cy", 0.0);
            let r = attr_f32(tag, "r", 0.0);
            if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
                prims.push(Prim::Path { path, filled });
            }
        }
    }
    VectorIcon {
        view_w,
        view_h,
        prims,
    }
}

/// GPU-Atlas Vector Icon Set (`icons.json` + Shell & Quick-Toggle icons).
/// Each icon size is rasterized **once** into `AtlasBank`'s `R8_UNORM` GPU texture and drawn as 1 `SdfInstance`!
pub struct IconSet {
    icons: HashMap<&'static str, (&'static str, VectorIcon)>,
}

impl Default for IconSet {
    fn default() -> Self {
        Self::new()
    }
}

impl IconSet {
    pub fn new() -> Self {
        let mut icons = HashMap::with_capacity(RAW_ICONS.len());
        for &(k, v) in RAW_ICONS {
            icons.insert(k, (k, parse_icon_snippet(k, v)));
        }
        Self { icons }
    }

    pub fn draw(&self, canvas: &mut Canvas, name: &str, bounds: RectF, color: Rgba, stroke_width: f32) {
        if color.a == 0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
            return;
        }
        let Some(&(static_name, ref icon)) = self.icons.get(name) else {
            return;
        };
        let pw = (bounds.w * canvas.scale).round().clamp(4.0, 256.0) as u32;
        let ph = (bounds.h * canvas.scale).round().clamp(4.0, 256.0) as u32;
        let stroke_q = (stroke_width.clamp(1.0, 6.0) * 10.0).round() as u16;
        let key = (static_name, pw as u16, ph as u16, stroke_q);

        let uv = {
            let mut bank = AtlasBank::global().lock().unwrap();
            if let Some(&cached_uv) = bank.icons.get(&key) {
                cached_uv
            } else {
                // Rasterize at 2x supersampled resolution and box-filter to crisp R8 coverage!
                let ss = 2u32;
                let rw = pw * ss;
                let rh = ph * ss;
                let mut pix = Pixmap::new(rw, rh).expect("icon pixmap");
                let sx = (rw as f32) / icon.view_w;
                let sy = (rh as f32) / icon.view_h;
                let s = sx.min(sy);
                let ox = ((rw as f32) - icon.view_w * s) * 0.5;
                let oy = ((rh as f32) - icon.view_h * s) * 0.5;
                let ts = Transform::from_row(s, 0.0, 0.0, s, ox, oy);

                let mut paint = Paint::default();
                paint.set_color_rgba8(255, 255, 255, 255);
                paint.anti_alias = true;
                let stroke = Stroke {
                    width: stroke_width.max(1.2),
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    ..Stroke::default()
                };
                for prim in &icon.prims {
                    match prim {
                        Prim::Path { path, filled } => {
                            if *filled {
                                pix.fill_path(path, &paint, FillRule::Winding, ts, None);
                            } else {
                                pix.stroke_path(path, &paint, &stroke, ts, None);
                            }
                        }
                    }
                }
                let src = pix.data();
                let mut r8 = vec![0u8; (pw * ph) as usize];
                for y in 0..ph {
                    for x in 0..pw {
                        let mut sum = 0u32;
                        for dy in 0..ss {
                            for dx in 0..ss {
                                let idx = (((y * ss + dy) * rw + (x * ss + dx)) * 4 + 3) as usize;
                                sum += src[idx] as u32;
                            }
                        }
                        r8[(y * pw + x) as usize] = (sum / (ss * ss)) as u8;
                    }
                }
                let uv = bank.insert_r8(pw, ph, r8);
                bank.icons.insert(key, uv);
                uv
            }
        };

        canvas.push_instance(SdfInstance {
            rect: [
                (bounds.x * canvas.scale).round(),
                (bounds.y * canvas.scale).round(),
                pw as f32,
                ph as f32,
            ],
            color0: color.to_vec4(),
            uv_rect: uv,
            params: [MODE_R8_ATLAS, 0.0, 0.0, 0.0],
            ..Default::default()
        });
    }
}

const RAW_ICONS: &[(&str, &str)] = &[
    // --- Full Y700 / OriginOS OSK icon set ---
    ("__space", "<rect x=\"20.6\" y=\"1.5\" width=\"6.8\" height=\"10.6\" rx=\"3.4\" fill=\"currentColor\" stroke=\"none\"/><path d=\"M17.4 8.6a6.6 6.6 0 0 0 13.2 0M24 15.2v1.6\"/><path d=\"M8 15.5v3.2a2.3 2.3 0 0 0 2.3 2.3h27.4a2.3 2.3 0 0 0 2.3-2.3v-3.2\"/>"),
    ("arrow_down", "<path d=\"M12 5V19\"/><path d=\"M6.5 13.5L12 19L17.5 13.5\"/>"),
    ("arrow_left", "<path d=\"M19 12H5\"/><path d=\"M10.5 6.5L5 12L10.5 17.5\"/>"),
    ("arrow_right", "<path d=\"M5 12H19\"/><path d=\"M13.5 6.5L19 12L13.5 17.5\"/>"),
    ("arrow_up", "<path d=\"M12 19V5\"/><path d=\"M6.5 10.5L12 5L17.5 10.5\"/>"),
    ("back", "<path d=\"M15.5 5L8.5 12L15.5 19\"/>"),
    ("backspace", "<path d=\"M9.5 5H19A2 2 0 0 1 21 7V17A2 2 0 0 1 19 19H9.5L3 12Z\"/><path d=\"M12.25 9.5L17.25 14.5M17.25 9.5L12.25 14.5\"/>"),
    ("bubble", "<path d=\"M7 3.5H17A2 2 0 0 1 19 5.5V11.5A2 2 0 0 1 17 13.5H14L12 16L10 13.5H7A2 2 0 0 1 5 11.5V5.5A2 2 0 0 1 7 3.5Z\"/><path d=\"M10.25 10.5L12 6.5L13.75 10.5M10.9 9H13.1\"/><path d=\"M6 20H18\"/>"),
    ("candidate_more", "<path d=\"M7 9.5L12 14.5L17 9.5\"/>"),
    ("check", "<path d=\"M4.5 12L9.5 17L19.5 6.5\"/>"),
    ("clipboard", "<path d=\"M9 4.5H7.5A2.5 2.5 0 0 0 5 7V18.5A2.5 2.5 0 0 0 7.5 21H16.5A2.5 2.5 0 0 0 19 18.5V7A2.5 2.5 0 0 0 16.5 4.5H15\"/><rect x=\"9\" y=\"3\" width=\"6\" height=\"3.5\" rx=\"1.25\"/><path d=\"M9 12H15M9 15.5H13\"/>"),
    ("close", "<path d=\"M6 6L18 18M18 6L6 18\"/>"),
    ("collapse", "<path d=\"M5 9.5L12 14.5L19 9.5\"/>"),
    ("chevron_up", "<path d=\"M5 14.5L12 9.5L19 14.5\"/>"),
    ("copy", "<rect x=\"4\" y=\"8\" width=\"12\" height=\"12\" rx=\"2.5\"/><path d=\"M8 8V6.5A2.5 2.5 0 0 1 10.5 4H17.5A2.5 2.5 0 0 1 20 6.5V13.5A2.5 2.5 0 0 1 17.5 16H16\"/>"),
    ("cursor_edit", "<path d=\"M12 5V19M9.5 4.5H14.5M9.5 19.5H14.5\"/><path d=\"M6.5 9L3.5 12L6.5 15M17.5 9L20.5 12L17.5 15\"/>"),
    ("cut", "<circle cx=\"7\" cy=\"17\" r=\"2.5\"/><circle cx=\"17\" cy=\"17\" r=\"2.5\"/><path d=\"M8.45 14.96L16.5 3.5M15.55 14.96L7.5 3.5\"/>"),
    ("dock", "<rect x=\"3\" y=\"4\" width=\"18\" height=\"11.5\" rx=\"2.5\"/><circle cx=\"6\" cy=\"8\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"9\" cy=\"8\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"12\" cy=\"8\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"15\" cy=\"8\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"18\" cy=\"8\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/><path d=\"M8.5 11.5H15.5\"/><path d=\"M3 19.5H21\"/>"),
    ("drag_handle", "<path d=\"M7 10.25H17M7 13.75H17\"/>"),
    ("emoji", "<circle cx=\"12\" cy=\"12\" r=\"9\"/><circle cx=\"9\" cy=\"9.75\" r=\"1.1\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"15\" cy=\"9.75\" r=\"1.1\" fill=\"currentColor\" stroke=\"none\"/><path d=\"M8.5 14A4 4 0 0 0 15.5 14\"/>"),
    ("end", "<path d=\"M20 6V18\"/><path d=\"M4 12H16\"/><path d=\"M11.5 7.5L16 12L11.5 16.5\"/>"),
    ("enter", "<path d=\"M19.5 4.5V10.5A3 3 0 0 1 16.5 13.5H4.5\"/><path d=\"M9 9L4.5 13.5L9 18\"/>"),
    ("expand", "<rect x=\"4\" y=\"4\" width=\"6.5\" height=\"6.5\" rx=\"1.75\"/><rect x=\"13.5\" y=\"4\" width=\"6.5\" height=\"6.5\" rx=\"1.75\"/><rect x=\"4\" y=\"13.5\" width=\"6.5\" height=\"6.5\" rx=\"1.75\"/><rect x=\"13.5\" y=\"13.5\" width=\"6.5\" height=\"6.5\" rx=\"1.75\"/>"),
    ("float", "<path d=\"M9 4H15\"/><rect x=\"4\" y=\"7.5\" width=\"16\" height=\"11.5\" rx=\"2.5\"/><circle cx=\"7.5\" cy=\"11.4\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"10.5\" cy=\"11.4\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"13.5\" cy=\"11.4\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"16.5\" cy=\"11.4\" r=\"0.9\" fill=\"currentColor\" stroke=\"none\"/><path d=\"M9 14.9H15\"/>"),
    ("globe", "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M12 3A4 9 0 0 0 12 21A4 9 0 0 0 12 3Z\"/><path d=\"M3 12H21\"/>"),
    ("glow", "<circle cx=\"12\" cy=\"12\" r=\"3.2\"/><path d=\"M12 3V5M12 19V21M3 12H5M19 12H21M5.6 5.6L7 7M17 17L18.4 18.4M5.6 18.4L7 17M17 7L18.4 5.6\"/>"),
    ("haptic", "<rect x=\"9\" y=\"4.5\" width=\"6\" height=\"15\" rx=\"2\"/><path d=\"M6 8.5V15.5M3 10.5V13.5M18 8.5V15.5M21 10.5V13.5\"/>"),
    ("height_adjust", "<path d=\"M6 4H18M6 20H18\"/><path d=\"M12 7.5V16.5\"/><path d=\"M9 10.5L12 7.5L15 10.5M9 13.5L12 16.5L15 13.5\"/>"),
    ("home", "<path d=\"M4 6V18\"/><path d=\"M20 12H8\"/><path d=\"M12.5 7.5L8 12L12.5 16.5\"/>"),
    ("keyboard", "<rect x=\"3\" y=\"5.5\" width=\"18\" height=\"13\" rx=\"2.5\"/><circle cx=\"6\" cy=\"9\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"9\" cy=\"9\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"12\" cy=\"9\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"15\" cy=\"9\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"18\" cy=\"9\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"6\" cy=\"12\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"9\" cy=\"12\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"12\" cy=\"12\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"15\" cy=\"12\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"18\" cy=\"12\" r=\"0.95\" fill=\"currentColor\" stroke=\"none\"/><path d=\"M8.5 15.25H15.5\"/>"),
    ("lock", "<rect x=\"5\" y=\"10.5\" width=\"14\" height=\"10\" rx=\"2.5\"/><path d=\"M8 10.5V7.5A4 4 0 0 1 16 7.5V10.5\"/><path d=\"M12 14.5V16.5\"/>"),
    ("mic", "<rect x=\"8.5\" y=\"3\" width=\"7\" height=\"11\" rx=\"3.5\"/><path d=\"M5 11A7 7 0 0 0 19 11\"/><path d=\"M12 18V21\"/>"),
    ("numpad", "<circle cx=\"6\" cy=\"6\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"12\" cy=\"6\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"18\" cy=\"6\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"6\" cy=\"12\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"12\" cy=\"12\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"18\" cy=\"12\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"6\" cy=\"18\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"12\" cy=\"18\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"18\" cy=\"18\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("paste", "<path d=\"M9 4.5H7.5A2.5 2.5 0 0 0 5 7V18.5A2.5 2.5 0 0 0 7.5 21H16.5A2.5 2.5 0 0 0 19 18.5V7A2.5 2.5 0 0 0 16.5 4.5H15\"/><rect x=\"9\" y=\"3\" width=\"6\" height=\"3.5\" rx=\"1.25\"/><path d=\"M12 10V16.5M9.25 13.75L12 16.5L14.75 13.75\"/>"),
    ("phrase", "<rect x=\"4\" y=\"5\" width=\"16\" height=\"14\" rx=\"2.5\"/><path d=\"M8 10H16M8 14H13\"/>"),
    ("pin", "<path d=\"M8.5 4H15.5\"/><path d=\"M9.5 4V10L6.5 14H17.5L14.5 10V4\"/><path d=\"M12 14V21\"/>"),
    ("redo", "<path d=\"M15 4L20 9L15 14\"/><path d=\"M20 9H9.5A5.5 5.5 0 0 0 9.5 20H13\"/>"),
    ("search", "<circle cx=\"10.5\" cy=\"10.5\" r=\"6.5\"/><path d=\"M15.3 15.3L20 20\"/>"),
    ("select_all", "<path d=\"M4 8V6.5A2.5 2.5 0 0 1 6.5 4H8M16 4H17.5A2.5 2.5 0 0 1 20 6.5V8M20 16V17.5A2.5 2.5 0 0 1 17.5 20H16M8 20H6.5A2.5 2.5 0 0 1 4 17.5V16\"/><path d=\"M8.5 10H15.5M8.5 14H13\"/>"),
    ("settings", "<circle cx=\"12\" cy=\"12\" r=\"3.2\"/><path d=\"M12 3.2V5.4M12 18.6V20.8M3.2 12H5.4M18.6 12H20.8M5.8 5.8L7.4 7.4M16.6 16.6L18.2 18.2M18.2 5.8L16.6 7.4M7.4 16.6L5.8 18.2\"/>"),
    ("shift", "<path d=\"M12 3.5L20 11.5H15.5V20H8.5V11.5H4Z\"/>"),
    ("shift_lock", "<path d=\"M12 3L20 11H15.5V16H8.5V11H4Z\" fill=\"currentColor\" stroke=\"none\"/><path d=\"M8.5 20H15.5\"/>"),
    ("shift_once", "<path d=\"M12 3.5L20 11.5H15.5V20H8.5V11.5H4Z\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("sound", "<path d=\"M3.5 9.5H7L11.5 5.5V18.5L7 14.5H3.5Z\"/><path d=\"M15 9A4.5 4.5 0 0 1 15 15\"/><path d=\"M17.5 6A8.5 8.5 0 0 1 17.5 18\"/>"),
    ("space_swipe", "<path d=\"M4 13.5V16.5A1.5 1.5 0 0 0 5.5 18H18.5A1.5 1.5 0 0 0 20 16.5V13.5\"/><path d=\"M5 9H19M8 6L5 9L8 12M16 6L19 9L16 12\"/>"),
    ("split", "<path d=\"M10.5 6H5.5A2.5 2.5 0 0 0 3 8.5V15.5A2.5 2.5 0 0 0 5.5 18H10.5M13.5 6H18.5A2.5 2.5 0 0 1 21 8.5V15.5A2.5 2.5 0 0 1 18.5 18H13.5\"/><circle cx=\"6\" cy=\"9.5\" r=\"0.85\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"9\" cy=\"9.5\" r=\"0.85\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"15\" cy=\"9.5\" r=\"0.85\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"18\" cy=\"9.5\" r=\"0.85\" fill=\"currentColor\" stroke=\"none\"/><path d=\"M7 15H10.5M13.5 15H17\"/>"),
    ("symbols", "<path d=\"M6 3V12M9.5 3V12M3.25 5.5H12.25M3.25 9.5H12.25\"/><circle cx=\"13.75\" cy=\"13.75\" r=\"1.6\"/><circle cx=\"19.5\" cy=\"19.5\" r=\"1.6\"/><path d=\"M20.75 12.5L12.5 20.75\"/>"),
    ("theme", "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M12 6.5A5.5 5.5 0 0 1 12 17.5Z\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("translate", "<path d=\"M7.75 3V4.5M3 5.5H12.5M10.75 5.5C10 9.25 7.25 11.75 3 13M5 5.5C5.75 9 8.5 11.5 12.5 13\"/><path d=\"M12.5 21L16.75 10.5L21 21M14 17.5H19.5\"/>"),
    ("trash", "<path d=\"M4 6.5H20\"/><path d=\"M9.5 6.5V5A1.5 1.5 0 0 1 11 3.5H13A1.5 1.5 0 0 1 14.5 5V6.5\"/><path d=\"M6 6.5L6.9 18.6A2 2 0 0 0 8.9 20.5H15.1A2 2 0 0 0 17.1 18.6L18 6.5\"/><path d=\"M10 10.5V16.5M14 10.5V16.5\"/>"),
    ("undo", "<path d=\"M9 4L4 9L9 14\"/><path d=\"M4 9H14.5A5.5 5.5 0 0 1 14.5 20H11\"/>"),
    ("volume_off", "<path d=\"M3.5 9.5H7L11.5 5.5V18.5L7 14.5H3.5Z\"/><path d=\"M15.5 9.5L20.5 14.5M20.5 9.5L15.5 14.5\"/>"),
    // --- Shell StatusBar / 20 Quick Toggles / Peripheral / Stylus / Media icons ---
    ("wifi", "<path d=\"M2.5 9A14.5 14.5 0 0 1 21.5 9\"/><path d=\"M5.8 12.5A10 10 0 0 1 18.2 12.5\"/><path d=\"M9.2 16A5.2 5.2 0 0 1 14.8 16\"/><circle cx=\"12\" cy=\"19.2\" r=\"1.3\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("wifi_off", "<path d=\"M2.5 9A14.5 14.5 0 0 1 21.5 9\"/><path d=\"M5.8 12.5A10 10 0 0 1 18.2 12.5\"/><path d=\"M4 4L20 20\"/>"),
    ("bluetooth", "<path d=\"M7 7L17 17L12 21V3L17 7L7 17\"/>"),
    ("bluetooth_off", "<path d=\"M7 7L17 17L12 21V13M12 8V3L17 7L14 10\"/><path d=\"M4 4L20 20\"/>"),
    ("sun", "<circle cx=\"12\" cy=\"12\" r=\"4.2\"/><path d=\"M12 2.5V5M12 19V21.5M2.5 12H5M19 12H21.5M5.3 5.3L7.1 7.1M16.9 16.9L18.7 18.7M18.7 5.3L16.9 7.1M7.1 16.9L5.3 18.7\"/>"),
    ("auto_brightness", "<path d=\"M12 2.3L14.8 5.1H18.9V9.2L21.7 12L18.9 14.8V18.9H14.8L12 21.7L9.2 18.9H5.1V14.8L2.3 12L5.1 9.2V5.1H9.2Z\"/><path d=\"M9.5 15.2L12 8.5L14.5 15.2M10.3 13.3H13.7\"/>"),
    ("rotate", "<path d=\"M19.5 10A7.8 7.8 0 1 0 17.5 17.2\"/><path d=\"M19.8 5.2V10.2H14.8\"/>"),
    ("torch", "<path d=\"M8 3H16L14.5 8.5V19A2 2 0 0 1 12.5 21H11.5A2 2 0 0 1 9.5 19V8.5Z\"/><path d=\"M8.5 6.5H15.5\"/>"),
    ("bolt", "<path d=\"M13 2.5L5.5 13H12L11 21.5L18.5 11H12Z\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("stylus", "<path d=\"M16.5 3.5L20.5 7.5L8 20H4V16Z\"/><path d=\"M14 6L18 10\"/>"),
    ("shield_80", "<path d=\"M12 3L19 6V11.5C19 16 15.8 19.5 12 21C8.2 19.5 5 16 5 11.5V6Z\"/><path d=\"M9.5 12L11.5 14L15 10\"/>"),
    ("monitor_dp", "<rect x=\"2.5\" y=\"4\" width=\"19\" height=\"13\" rx=\"2\"/><path d=\"M8 20.5H16M12 17V20.5\"/>"),
    ("moon_dnd", "<path d=\"M18.5 14.5A7.5 7.5 0 0 1 9.5 5.5A7.5 7.5 0 1 0 18.5 14.5Z\"/>"),
    ("screenshot", "<path d=\"M4 8V6A2 2 0 0 1 6 4H8M16 4H18A2 2 0 0 1 20 6V8M20 16V18A2 2 0 0 1 18 20H16M8 20H6A2 2 0 0 1 4 18V16\"/><circle cx=\"12\" cy=\"12\" r=\"3.2\"/>"),
    ("power", "<path d=\"M12 3V11\"/><path d=\"M6.8 6.2A8 8 0 1 0 17.2 6.2\"/>"),
    ("restart", "<path d=\"M4.5 12A7.5 7.5 0 1 1 7 17.6\"/><path d=\"M4.2 7.2V12.2H9.2\"/>"),
    ("play", "<path d=\"M7 5L19 12L7 19Z\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("pause", "<rect x=\"6.5\" y=\"5\" width=\"3.8\" height=\"14\" rx=\"1.2\" fill=\"currentColor\" stroke=\"none\"/><rect x=\"13.7\" y=\"5\" width=\"3.8\" height=\"14\" rx=\"1.2\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("skip_prev", "<path d=\"M18 5.5L9.5 12L18 18.5Z\" fill=\"currentColor\" stroke=\"none\"/><path d=\"M6.5 6V18\"/>"),
    ("skip_next", "<path d=\"M6 5.5L14.5 12L6 18.5Z\" fill=\"currentColor\" stroke=\"none\"/><path d=\"M17.5 6V18\"/>"),
    ("chevron_right", "<path d=\"M9 6L15 12L9 18\"/>"),
    ("bell", "<path d=\"M6 16.5H18L16.5 14V10A4.5 4.5 0 0 0 7.5 10V14Z\"/><path d=\"M10 19A2 2 0 0 0 14 19\"/>"),
    ("gauge", "<path d=\"M4.5 17A8.5 8.5 0 1 1 19.5 17\"/><path d=\"M12 13L16 8.5\"/><circle cx=\"12\" cy=\"13\" r=\"1.5\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("photo_frame", "<rect x=\"3\" y=\"4.5\" width=\"18\" height=\"15\" rx=\"2.5\"/><circle cx=\"8.5\" cy=\"9.5\" r=\"1.6\"/><path d=\"M4 17.5L9.5 12.5L13.5 16L16.5 13.5L20 17.5\"/>"),
    ("eye", "<path d=\"M2.5 12C4.5 7.5 8 5.5 12 5.5C16 5.5 19.5 7.5 21.5 12C19.5 16.5 16 18.5 12 18.5C8 18.5 4.5 16.5 2.5 12Z\"/><circle cx=\"12\" cy=\"12\" r=\"2.8\"/>"),
    ("eye_off", "<path d=\"M2.5 12C4.5 7.5 8 5.5 12 5.5C16 5.5 19.5 7.5 21.5 12C19.5 16.5 16 18.5 12 18.5C8 18.5 4.5 16.5 2.5 12Z\"/><path d=\"M4 4L20 20\"/>"),
    ("user", "<circle cx=\"12\" cy=\"8.5\" r=\"3.8\"/><path d=\"M5 19.5C5.5 15.8 8.4 14 12 14C15.6 14 18.5 15.8 19 19.5\"/>"),
    // New v9 icons: Reading Mode, Captions, New Workspace, Hot Corners, Pen Orb, Quick Note, Mouse, Headphones, Gamepad, Music, Timer
    ("book", "<path d=\"M4 5.5A2.5 2.5 0 0 1 6.5 3H12V19.5H6.5A2.5 2.5 0 0 0 4 22Z\"/><path d=\"M20 5.5A2.5 2.5 0 0 0 17.5 3H12V19.5H17.5A2.5 2.5 0 0 1 20 22Z\"/>"),
    ("captions", "<rect x=\"3\" y=\"5\" width=\"18\" height=\"14\" rx=\"3\"/><path d=\"M10 10A2.4 2.4 0 1 0 10 14\"/><path d=\"M16.5 10A2.4 2.4 0 1 0 16.5 14\"/>"),
    ("plus_ws", "<rect x=\"3.5\" y=\"4.5\" width=\"17\" height=\"15\" rx=\"3\"/><path d=\"M12 8.5V15.5M8.5 12H15.5\"/>"),
    ("corner_swipe", "<rect x=\"3.5\" y=\"4.5\" width=\"17\" height=\"15\" rx=\"3\"/><path d=\"M5.5 17.5L10.5 12.5M7 12.5H10.5V16\"/>"),
    ("stylus_orb", "<circle cx=\"12\" cy=\"12\" r=\"8\"/><path d=\"M14.5 7.5L16.5 9.5L10.5 15.5H8.5V13.5Z\"/>"),
    ("note", "<rect x=\"4.5\" y=\"3.5\" width=\"15\" height=\"17\" rx=\"2.5\"/><path d=\"M8.5 8.5H15.5M8.5 12.5H15.5M8.5 16.5H12.5\"/>"),
    ("headphones", "<path d=\"M4.5 13V11A7.5 7.5 0 0 1 19.5 11V13\"/><rect x=\"3.5\" y=\"12.5\" width=\"3.8\" height=\"6.5\" rx=\"1.8\"/><rect x=\"16.7\" y=\"12.5\" width=\"3.8\" height=\"6.5\" rx=\"1.8\"/>"),
    ("mouse", "<rect x=\"6.5\" y=\"3.5\" width=\"11\" height=\"17\" rx=\"5.5\"/><path d=\"M12 7V10.5\"/>"),
    ("gamepad", "<rect x=\"2.5\" y=\"7\" width=\"19\" height=\"10.5\" rx=\"4.5\"/><path d=\"M7.5 10.2V14.2M5.5 12.2H9.5\"/><circle cx=\"15.5\" cy=\"11\" r=\"1\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"17.5\" cy=\"13.2\" r=\"1\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("music", "<path d=\"M9 17.5V6L19 4V15.5\"/><circle cx=\"6.8\" cy=\"17.5\" r=\"2.4\"/><circle cx=\"16.8\" cy=\"15.5\" r=\"2.4\"/>"),
    ("timer", "<circle cx=\"12\" cy=\"13\" r=\"7.5\"/><path d=\"M10 3H14M12 9V13L14.8 14.8\"/>"),
    ("terminal", "<rect x=\"3\" y=\"4.5\" width=\"18\" height=\"15\" rx=\"2.5\"/><path d=\"M7 9.5L10.5 12.5L7 15.5M12.5 15.5H16.5\"/>"),
    ("folder", "<path d=\"M3.5 7A2 2 0 0 1 5.5 5H9.5L11.5 7.2H18.5A2 2 0 0 1 20.5 9.2V17.5A2 2 0 0 1 18.5 19.5H5.5A2 2 0 0 1 3.5 17.5Z\"/>"),
    ("code", "<path d=\"M8.5 8L4 12L8.5 16M15.5 8L20 12L15.5 16M13.5 6.5L10.5 17.5\"/>"),
    ("browser", "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M3.5 9H20.5M3.5 15H20.5M12 3A13 13 0 0 1 12 21M12 3A13 13 0 0 0 12 21\"/>"),
    ("chat", "<path d=\"M20 11.5A7.5 7.5 0 0 1 8.8 18L4 19L5.2 14.8A7.5 7.5 0 1 1 20 11.5Z\"/><circle cx=\"9.5\" cy=\"11.5\" r=\"1\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"12.5\" cy=\"11.5\" r=\"1\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"15.5\" cy=\"11.5\" r=\"1\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("video", "<rect x=\"3\" y=\"5\" width=\"18\" height=\"14\" rx=\"3\"/><path d=\"M10 9L16 12L10 15Z\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("mail", "<rect x=\"3\" y=\"5\" width=\"18\" height=\"14\" rx=\"2.5\"/><path d=\"M3.5 7L12 13L20.5 7\"/>"),
    ("cal", "<rect x=\"3.5\" y=\"5\" width=\"17\" height=\"15.5\" rx=\"2.5\"/><path d=\"M8 3V7M16 3V7M3.5 10H20.5\"/>"),
    ("calc", "<rect x=\"4\" y=\"3\" width=\"16\" height=\"18\" rx=\"2.5\"/><path d=\"M8 7.5H16M8 12.5H10M14 12.5H16M8 16.5H10M14 16.5H16\"/>"),
    ("draw", "<path d=\"M12 3A9 9 0 1 0 12 21A2 2 0 0 0 14 19V18A2 2 0 0 1 16 16H17.5A3.5 3.5 0 0 0 21 12.5A9.5 9.5 0 0 0 12 3Z\"/><circle cx=\"8\" cy=\"10\" r=\"1.2\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"12\" cy=\"7.5\" r=\"1.2\" fill=\"currentColor\" stroke=\"none\"/><circle cx=\"16\" cy=\"10\" r=\"1.2\" fill=\"currentColor\" stroke=\"none\"/>"),
    ("grid", "<rect x=\"4\" y=\"4\" width=\"6.5\" height=\"6.5\" rx=\"1.6\"/><rect x=\"13.5\" y=\"4\" width=\"6.5\" height=\"6.5\" rx=\"1.6\"/><rect x=\"4\" y=\"13.5\" width=\"6.5\" height=\"6.5\" rx=\"1.6\"/><rect x=\"13.5\" y=\"13.5\" width=\"6.5\" height=\"6.5\" rx=\"1.6\"/>"),
    ("mic_off", "<rect x=\"8.5\" y=\"3\" width=\"7\" height=\"11\" rx=\"3.5\"/><path d=\"M5 11A7 7 0 0 0 19 11\"/><path d=\"M12 18V21\"/><path d=\"M4 4L20 20\"/>"),
    ("refresh_hz", "<path d=\"M4.5 11A7.5 7.5 0 0 1 18.2 7.5\"/><path d=\"M18.5 3.8V8H14.3\"/><path d=\"M19.5 13A7.5 7.5 0 0 1 5.8 16.5\"/><path d=\"M5.5 20.2V16H9.7\"/>"),
    ("bypass_power", "<rect x=\"3\" y=\"8\" width=\"12.5\" height=\"9.5\" rx=\"2.3\"/><path d=\"M15.5 11.2V14.3\"/><path d=\"M6.5 4.8H16.2A3.8 3.8 0 0 1 20 8.6V14\"/><path d=\"M17.6 11.6L20 14.2L22.2 11.6\"/>"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_icons_parse_without_empty_paths() {
        let set = IconSet::new();
        for &(name, _) in RAW_ICONS {
            let (_, ic) = set.icons.get(name).expect(name);
            assert!(!ic.prims.is_empty(), "icon {name} parsed to 0 primitives");
        }
    }
}
