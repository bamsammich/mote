// Blit a single texture into a pixel-space rectangle of the render target.
//
// Used twice per frame by the compositor: once to place the focused page's
// OSR texture into the chrome-reported viewport rect, and once to lay the
// chrome's full-window texture over the top (the chrome is transparent in the
// viewport region, so the page shows through — chrome-surrounds-content).

// Pixel dimensions of the render target (surface or offscreen texture).
struct Globals {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;

// The texture to blit and its sampler.
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

// The destination rectangle in pixels: (x, y, w, h).
struct Dest { rect: vec4<f32>, };
@group(1) @binding(2) var<uniform> dest: Dest;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let c = corners[vi];
    let px = dest.rect.xy + c * dest.rect.zw;
    // Pixel -> normalized device coordinates (y points down in pixel space).
    let ndc = vec2(px.x / globals.screen.x * 2.0 - 1.0,
                   1.0 - px.y / globals.screen.y * 2.0);
    var out: VsOut;
    out.pos = vec4(ndc, 0.0, 1.0);
    out.uv = c;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
