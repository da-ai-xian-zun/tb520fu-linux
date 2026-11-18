struct BlurParams {
    texel: vec2<f32>,
    offset: f32,
    sat_boost: f32,
};

@group(0) @binding(0) var<uniform> params: BlurParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vid: u32) -> VsOut {
    var uv = vec2<f32>(0.0, 0.0);
    if vid == 1u {
        uv = vec2<f32>(2.0, 0.0);
    } else if vid == 2u {
        uv = vec2<f32>(0.0, 2.0);
    }
    var out: VsOut;
    out.pos = vec4<f32>(uv * 2.0 - vec2<f32>(1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_down(in: VsOut) -> @location(0) vec4<f32> {
    let hp = params.texel * params.offset;
    var sum = textureSampleLevel(src_tex, samp, in.uv, 0.0) * 4.0;
    sum += textureSampleLevel(src_tex, samp, in.uv - hp, 0.0);
    sum += textureSampleLevel(src_tex, samp, in.uv + hp, 0.0);
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(hp.x, -hp.y), 0.0);
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(-hp.x, hp.y), 0.0);
    return sum / 8.0;
}

@fragment
fn fs_up(in: VsOut) -> @location(0) vec4<f32> {
    let hp = params.texel * params.offset;
    var sum = vec4<f32>(0.0);
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(-hp.x * 2.0, 0.0), 0.0);
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(-hp.x, hp.y), 0.0) * 2.0;
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(0.0, hp.y * 2.0), 0.0);
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(hp.x, hp.y), 0.0) * 2.0;
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(hp.x * 2.0, 0.0), 0.0);
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(hp.x, -hp.y), 0.0) * 2.0;
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(0.0, -hp.y * 2.0), 0.0);
    sum += textureSampleLevel(src_tex, samp, in.uv + vec2<f32>(-hp.x, -hp.y), 0.0) * 2.0;

    var rgb = (sum / 12.0).rgb;
    if abs(params.sat_boost - 1.0) > 0.01 {
        let luma = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        rgb = clamp(mix(vec3<f32>(luma, luma, luma), rgb, params.sat_boost), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return vec4<f32>(rgb, 1.0);
}
