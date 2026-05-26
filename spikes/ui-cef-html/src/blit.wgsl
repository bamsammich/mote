// Blit a CEF OSR BGRA frame into a px-rect of the 1280x800 composite.
// CEF delivers BGRA8 premultiplied; we swizzle to RGBA in the shader.

struct Push {
  rect: vec4<f32>,    // x, y, w, h in px
  screen: vec2<f32>,
  _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> push: Push;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

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
  let px = push.rect.xy + c * push.rect.zw;
  let ndc = vec2(px.x / push.screen.x * 2.0 - 1.0,
                 1.0 - px.y / push.screen.y * 2.0);
  var out: VsOut;
  out.pos = vec4(ndc, 0.0, 1.0);
  out.uv = c;
  return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  let s = textureSample(tex, samp, in.uv);
  // texture is fed as Rgba8Unorm but the bytes are BGRA -> swizzle.
  return vec4(s.b, s.g, s.r, s.a);
}
