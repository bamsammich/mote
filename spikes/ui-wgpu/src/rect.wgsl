// Rounded-rect SDF pipeline. One instance per rect.
// Renders fill + optional border, antialiased edges. Supports asymmetric
// bottom border via `border_bottom` (the keycap construction in the spec).

struct Globals {
    screen: vec2<f32>, // viewport size in px
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;

struct Instance {
    @location(0) rect: vec4<f32>,      // x, y, w, h in px (top-left origin)
    @location(1) fill: vec4<f32>,      // rgba premult-not (straight alpha)
    @location(2) border: vec4<f32>,    // border rgba
    @location(3) params: vec4<f32>,    // radius, border_w, border_bottom_w, _unused
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,   // px coords relative to rect top-left
    @location(1) size: vec2<f32>,
    @location(2) fill: vec4<f32>,
    @location(3) border: vec4<f32>,
    @location(4) params: vec4<f32>,
};

// Full-screen-ish quad per instance via 6 verts.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let c = corners[vi];
    let px = inst.rect.xy + c * inst.rect.zw;
    // px -> NDC (y down to y up)
    let ndc = vec2(px.x / globals.screen.x * 2.0 - 1.0,
                   1.0 - px.y / globals.screen.y * 2.0);
    var out: VsOut;
    out.pos = vec4(ndc, 0.0, 1.0);
    out.local = c * inst.rect.zw;
    out.size = inst.rect.zw;
    out.fill = inst.fill;
    out.border = inst.border;
    out.params = inst.params;
    return out;
}

// SDF of rounded box centered at origin, half-size b, radius r.
fn sd_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let half = in.size * 0.5;
    let p = in.local - half;
    let r = min(in.params.x, min(half.x, half.y));
    let bw = in.params.y;
    let bottom = in.params.z; // extra bottom border thickness

    let d = sd_round_box(p, half, r);
    let aa = 1.0;

    // outer alpha (inside the rounded shape)
    let outer = 1.0 - smoothstep(-aa, 0.0, d);
    if (outer <= 0.0) { discard; }

    // Border region: distance from edge less than effective border width.
    // Bottom edge uses the larger of bw / bottom (keycap depth).
    var local_bw = bw;
    // p.y > 0 is the lower half (y-down local space, but we centered so +y = down)
    if (p.y > half.y - max(bottom, bw) && bottom > 0.0) {
        local_bw = max(bw, bottom);
    }

    var col = in.fill;
    if (local_bw > 0.0 && in.border.a > 0.0) {
        let border_mix = smoothstep(-local_bw - aa, -local_bw, d);
        col = mix(in.fill, in.border, border_mix);
    }
    col.a = col.a * outer;
    return col;
}
