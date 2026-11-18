struct Globals {
    target_size: vec2<f32>,
    wallpaper_size: vec2<f32>,
    surf_origin: vec2<f32>,
    extra: vec2<f32>,
};

struct SdfInstance {
    rect: vec4<f32>,
    clip_rect: vec4<f32>,
    radii: vec4<f32>,
    clip_radii: vec4<f32>,
    color0: vec4<f32>,
    color1: vec4<f32>,
    uv_rect: vec4<f32>,
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<storage, read> instances: array<SdfInstance>;
@group(0) @binding(2) var samp_linear: sampler;
@group(0) @binding(3) var tex_r8: texture_2d<f32>;
@group(0) @binding(4) var tex_rgba: texture_2d<f32>;
@group(0) @binding(5) var tex_glass: texture_2d<f32>;
@group(0) @binding(6) var tex_photo: texture_2d<f32>;
@group(0) @binding(7) var tex_cache: texture_2d<f32>;

// The instance is read from the storage buffer once per vertex and handed to the fragment stage as
// flat varyings. Reading the 128-byte struct per fragment instead costs eight buffer loads on every
// shaded pixel, which on Adreno dominated the frame time.
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) frag_xy: vec2<f32>,
    @location(1) @interpolate(flat) rect: vec4<f32>,
    @location(2) @interpolate(flat) clip_rect: vec4<f32>,
    @location(3) @interpolate(flat) radii: vec4<f32>,
    @location(4) @interpolate(flat) clip_radii: vec4<f32>,
    @location(5) @interpolate(flat) color0: vec4<f32>,
    @location(6) @interpolate(flat) color1: vec4<f32>,
    @location(7) @interpolate(flat) uv_rect: vec4<f32>,
    @location(8) @interpolate(flat) params: vec4<f32>,
};

fn sd_round_box(frag_xy: vec2<f32>, box_rect: vec4<f32>, radii: vec4<f32>) -> f32 {
    let half_sz = box_rect.zw * 0.5;
    let center = box_rect.xy + half_sz;
    let p = frag_xy - center;

    var r = radii.x;
    if p.x > 0.0 {
        if p.y > 0.0 {
            r = radii.z;
        } else {
            r = radii.y;
        }
    } else {
        if p.y > 0.0 {
            r = radii.w;
        } else {
            r = radii.x;
        }
    }
    let max_r = max(min(half_sz.x, half_sz.y), 0.0);
    let rc = clamp(r, 0.0, max_r);
    let q = abs(p) - half_sz + vec2<f32>(rc, rc);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - rc;
}

fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let denom = max(dot(ba, ba), 0.0001);
    let h = clamp(dot(pa, ba) / denom, 0.0, 1.0);
    return length(pa - ba * h);
}

@vertex
fn vs_main(
    @builtin(vertex_index) vert_idx: u32,
    @builtin(instance_index) inst_idx: u32,
) -> VsOut {
    let inst = instances[inst_idx];
    let mode = i32(inst.params.x + 0.5);

    var corner = vec2<f32>(0.0, 0.0);
    switch vert_idx % 6u {
        case 0u: { corner = vec2<f32>(0.0, 0.0); }
        case 1u: { corner = vec2<f32>(1.0, 0.0); }
        case 2u: { corner = vec2<f32>(0.0, 1.0); }
        case 3u: { corner = vec2<f32>(0.0, 1.0); }
        case 4u: { corner = vec2<f32>(1.0, 0.0); }
        default: { corner = vec2<f32>(1.0, 1.0); }
    }

    var min_p = inst.rect.xy - vec2<f32>(1.5, 1.5);
    var max_p = inst.rect.xy + inst.rect.zw + vec2<f32>(1.5, 1.5);

    if mode == 4 {
        let pad = max(inst.params.y, 1.0) + abs(inst.params.z) + 4.0;
        min_p = inst.rect.xy - vec2<f32>(pad, pad);
        max_p = inst.rect.xy + inst.rect.zw + vec2<f32>(pad, pad);
    } else if mode == 6 {
        let p0 = inst.rect.xy;
        let p1 = inst.rect.zw;
        let pad = inst.params.y * 0.5 + 2.0;
        min_p = min(p0, p1) - vec2<f32>(pad, pad);
        max_p = max(p0, p1) + vec2<f32>(pad, pad);
    } else if mode == 7 || mode == 8 || mode == 10 || mode == 12 {
        min_p = inst.rect.xy;
        max_p = inst.rect.xy + inst.rect.zw;
    }

    let px = mix(min_p, max_p, corner);
    let ndc = (px / max(globals.target_size, vec2<f32>(1.0, 1.0))) * 2.0 - vec2<f32>(1.0, 1.0);

    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, ndc.y, 0.0, 1.0);
    out.frag_xy = px;
    out.rect = inst.rect;
    out.clip_rect = inst.clip_rect;
    out.radii = inst.radii;
    out.clip_radii = inst.clip_radii;
    out.color0 = inst.color0;
    out.color1 = inst.color1;
    out.uv_rect = inst.uv_rect;
    out.params = inst.params;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let mode = i32(in.params.x + 0.5);
    let p = in.frag_xy;

    var clip_cov = 1.0;
    if in.clip_rect.z > 0.0 && in.clip_rect.w > 0.0 {
        let dc = sd_round_box(p, in.clip_rect, in.clip_radii);
        clip_cov = clamp(0.5 - dc, 0.0, 1.0);
        if clip_cov <= 0.001 {
            discard;
        }
    }

    // 0 = SOLID_FILL
    if mode == 0 {
        let d = sd_round_box(p, in.rect, in.radii);
        let cov = clamp(0.5 - d, 0.0, 1.0) * clip_cov;
        let a = in.color0.a * cov;
        return vec4<f32>(in.color0.rgb * a, a);
    }

    // 1 = H_GRADIENT, 2 = V_GRADIENT
    if mode == 1 || mode == 2 {
        let d = sd_round_box(p, in.rect, in.radii);
        let cov = clamp(0.5 - d, 0.0, 1.0) * clip_cov;
        var t = 0.0;
        if mode == 1 {
            t = clamp((p.x - in.rect.x) / max(in.rect.z, 1.0), 0.0, 1.0);
        } else {
            t = clamp((p.y - in.rect.y) / max(in.rect.w, 1.0), 0.0, 1.0);
        }
        let col = mix(in.color0, in.color1, t);
        let a = col.a * cov;
        return vec4<f32>(col.rgb * a, a);
    }

    // 3 = STROKE (hollow analytical rounded border)
    if mode == 3 {
        let sw = max(in.params.y, 1.0);
        let hw = sw * 0.5;
        let d_outer = sd_round_box(p, in.rect, in.radii);
        let d_ring = abs(d_outer + hw) - hw;
        let cov = clamp(0.5 - d_ring, 0.0, 1.0) * clip_cov;
        let a = in.color0.a * cov;
        return vec4<f32>(in.color0.rgb * a, a);
    }

    // 4 = SOFT_SHADOW (`params.w > 0.5` hollows out `in.rect` so translucent Liquid Glass stays pure)
    if mode == 4 {
        let spread = max(in.params.y, 1.0);
        let dy = in.params.z;
        let sh_rect = vec4<f32>(in.rect.x, in.rect.y + dy, in.rect.z, in.rect.w);
        let d = sd_round_box(p, sh_rect, in.radii);
        let falloff = 1.0 - smoothstep(-spread * 0.25, spread, d);
        var a = in.color0.a * falloff * falloff * clip_cov;
        if in.params.w > 0.5 {
            let d_card = sd_round_box(p, in.rect, in.radii);
            let inside_cov = clamp(0.5 - d_card, 0.0, 1.0);
            a = a * (1.0 - inside_cov);
        }
        return vec4<f32>(in.color0.rgb * a, a);
    }

    // 5 = RADIAL_GLOW
    if mode == 5 {
        let d = sd_round_box(p, in.rect, in.radii);
        let cov = clamp(0.5 - d, 0.0, 1.0) * clip_cov;
        let center = in.uv_rect.xy;
        let glow_r = max(in.uv_rect.z, 1.0);
        let dist = clamp(length(p - center) / glow_r, 0.0, 1.0);
        let fall = (1.0 - dist) * (1.0 - dist);
        let a = in.color0.a * fall * cov;
        return vec4<f32>(in.color0.rgb * a, a);
    }

    // 6 = CAPSULE_LINE (lines & 80-pt predicted stylus ink segments)
    if mode == 6 {
        let half_w = max(in.params.y * 0.5, 0.5);
        let d = sd_segment(p, in.rect.xy, in.rect.zw) - half_w;
        let cov = clamp(0.5 - d, 0.0, 1.0) * clip_cov;
        let a = in.color0.a * cov;
        return vec4<f32>(in.color0.rgb * a, a);
    }

    // 7 = R8_ATLAS (cosmic-text glyphs + vector icons)
    if mode == 7 {
        let local = clamp((p - in.rect.xy) / max(in.rect.zw, vec2<f32>(1.0, 1.0)), vec2<f32>(0.0), vec2<f32>(1.0));
        let uv = mix(in.uv_rect.xy, in.uv_rect.zw, local);
        let mask_cov = textureSampleLevel(tex_r8, samp_linear, uv, 0.0).r;
        let a = in.color0.a * mask_cov * clip_cov;
        return vec4<f32>(in.color0.rgb * a, a);
    }

    // 8 = RGBA_ATLAS (color emoji + app icons, premultiplied RGBA in atlas)
    if mode == 8 {
        let local = clamp((p - in.rect.xy) / max(in.rect.zw, vec2<f32>(1.0, 1.0)), vec2<f32>(0.0), vec2<f32>(1.0));
        let uv = mix(in.uv_rect.xy, in.uv_rect.zw, local);
        var box_cov = 1.0;
        if in.radii.x > 0.0 || in.radii.y > 0.0 || in.radii.z > 0.0 || in.radii.w > 0.0 {
            let d = sd_round_box(p, in.rect, in.radii);
            box_cov = clamp(0.5 - d, 0.0, 1.0);
        }
        let texel = textureSampleLevel(tex_rgba, samp_linear, uv, 0.0);
        let mul = in.color0.a * box_cov * clip_cov;
        return texel * mul;
    }

    // 9 = FAKE_GLASS (300px Dual-Kawase blurred wallpaper + Liquid Glass tint + specular rim)
    if mode == 9 {
        let d = sd_round_box(p, in.rect, in.radii);
        let cov = clamp(0.5 - d, 0.0, 1.0) * clip_cov;
        if cov <= 0.001 {
            discard;
        }
        let wp_uv = clamp(
            (p + globals.surf_origin) / max(globals.wallpaper_size, vec2<f32>(1.0, 1.0)),
            vec2<f32>(0.002, 0.002),
            vec2<f32>(0.998, 0.998),
        );
        let blurred = textureSampleLevel(tex_glass, samp_linear, wp_uv, 0.0).rgb;
        let local_v = clamp((p.y - in.rect.y) / max(in.rect.w, 1.0), 0.0, 1.0);

        // Blend frosted saturated wallpaper with Liquid Glass surface tint
        var rgb = mix(blurred, in.color0.rgb, in.color0.a);
        // Top Liquid Glass specular sheen
        let sheen = (1.0 - local_v) * (1.0 - local_v) * in.params.w;
        rgb = clamp(rgb + vec3<f32>(sheen, sheen, sheen), vec3<f32>(0.0), vec3<f32>(1.0));

        // Crisp 1.1px inner refraction rim highlight from color1
        let d_rim = abs(d + 0.65) - 0.65;
        let rim_cov = clamp(0.5 - d_rim, 0.0, 1.0) * in.color1.a;
        rgb = mix(rgb, in.color1.rgb, rim_cov);

        let panel_alpha = clamp(in.params.z, 0.0, 1.0) * cov;
        return vec4<f32>(rgb * panel_alpha, panel_alpha);
    }

    // 11 = TINT_GLASS: translucent tint + sheen + rim. What shows through is the compositor's blur
    // of whatever is behind this surface (requested with ext-background-effect-v1 for the same shape).
    if mode == 11 {
        let d = sd_round_box(p, in.rect, in.radii);
        let cov = clamp(0.5 - d, 0.0, 1.0) * clip_cov;
        if cov <= 0.001 {
            discard;
        }
        let local_v = clamp((p.y - in.rect.y) / max(in.rect.w, 1.0), 0.0, 1.0);
        let sheen = (1.0 - local_v) * (1.0 - local_v) * in.params.w;
        // premultiplied tint, lightened a little towards the top
        var a = in.color0.a;
        var rgb = in.color0.rgb * a + vec3<f32>(sheen, sheen, sheen);
        a = clamp(a + sheen, 0.0, 1.0);
        // 1.1px inner rim highlight
        let d_rim = abs(d + 0.65) - 0.65;
        let rim_cov = clamp(0.5 - d_rim, 0.0, 1.0) * in.color1.a;
        rgb = mix(rgb, in.color1.rgb, rim_cov);
        a = mix(a, 1.0, rim_cov);
        let k = clamp(in.params.z, 0.0, 1.0) * cov;
        return vec4<f32>(rgb * k, a * k);
    }

    // 12 = CACHE_BLIT (premultiplied RGBA offscreen UI snapshot for 120Hz zero-overdraw sliding)
    if mode == 12 {
        let local = clamp((p - in.rect.xy) / max(in.rect.zw, vec2<f32>(1.0, 1.0)), vec2<f32>(0.0), vec2<f32>(1.0));
        let uv = mix(in.uv_rect.xy, in.uv_rect.zw, local);
        let cached = textureSampleLevel(tex_cache, samp_linear, uv, 0.0);
        let mul = in.color0.a * clip_cov;
        return cached * mul;
    }

    // 10 = PHOTO_BLIT (full-res wallpaper / Standby photo / Screenshot canvas)
    let local = clamp((p - in.rect.xy) / max(in.rect.zw, vec2<f32>(1.0, 1.0)), vec2<f32>(0.0), vec2<f32>(1.0));
    let uv = mix(in.uv_rect.xy, in.uv_rect.zw, local);
    var box_cov = 1.0;
    if in.radii.x > 0.0 || in.radii.y > 0.0 || in.radii.z > 0.0 || in.radii.w > 0.0 {
        let d = sd_round_box(p, in.rect, in.radii);
        box_cov = clamp(0.5 - d, 0.0, 1.0);
    }
    let photo = textureSampleLevel(tex_photo, samp_linear, uv, 0.0);
    let a = photo.a * in.color0.a * box_cov * clip_cov;
    return vec4<f32>(photo.rgb * a, a);
}
