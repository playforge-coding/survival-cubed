// Instanced textured quads for the tile world and players.

struct Camera {
    offset: vec2<f32>,   // world-space pixel at the top-left of the screen
    viewport: vec2<f32>, // screen size in physical pixels
    zoom: f32,           // screen pixels per world pixel
    _pad0: f32,
    _pad1: vec2<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) corner: vec2<f32>,
    @location(1) pos: vec2<f32>,
    @location(2) size: vec2<f32>,
    @location(3) uv_min: vec2<f32>,
    @location(4) uv_max: vec2<f32>,
    @location(5) color: vec4<f32>,
) -> VsOut {
    let world = pos + corner * size;
    let screen = (world - camera.offset) * camera.zoom;
    let ndc = vec2<f32>(
        screen.x / (camera.viewport.x * 0.5) - 1.0,
        1.0 - screen.y / (camera.viewport.y * 0.5),
    );
    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = mix(uv_min, uv_max, corner);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(atlas_tex, atlas_samp, in.uv) * in.color;
    if (c.a < 0.01) {
        discard;
    }
    return c;
}
