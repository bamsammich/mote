//! Mote chrome mock — custom immediate-mode UI over wgpu, rendered offscreen.
//!
//! Pipeline: rounded-rect SDF pipeline (rect.wgsl) for all fills/borders,
//! a texture-blit pipeline (blit.wgsl) to composite an external "page" RGBA
//! texture (CEF OSR stand-in), and glyphon (cosmic-text) for real text.
//!
//! Layout is hand-rolled with a tiny `Rect` helper + cursor arithmetic — no
//! flexbox lib. The scene is rebuilt every frame (immediate mode).

mod tokens;
use tokens as t;

use bytemuck::{Pod, Zeroable};
use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight, Wrap,
};
use wgpu::util::DeviceExt;

const W: u32 = 1280;
const H: u32 = 800;

// ----------------------------------------------------------------------------
// Layout helper
// ----------------------------------------------------------------------------
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}
impl Rect {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    fn inset(self, p: f32) -> Self {
        Rect::new(self.x + p, self.y + p, self.w - 2.0 * p, self.h - 2.0 * p)
    }
    /// Cut `amount` px off the top, return (cut, remainder).
    fn cut_top(self, amount: f32) -> (Rect, Rect) {
        (
            Rect::new(self.x, self.y, self.w, amount),
            Rect::new(self.x, self.y + amount, self.w, self.h - amount),
        )
    }
    fn cut_left(self, amount: f32) -> (Rect, Rect) {
        (
            Rect::new(self.x, self.y, amount, self.h),
            Rect::new(self.x + amount, self.y, self.w - amount, self.h),
        )
    }
}

// ----------------------------------------------------------------------------
// Rect instance (matches rect.wgsl Instance)
// ----------------------------------------------------------------------------
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RectInst {
    rect: [f32; 4],
    fill: [f32; 4],
    border: [f32; 4],
    params: [f32; 4], // radius, border_w, border_bottom_w, _
}

#[derive(Default)]
struct Scene {
    rects: Vec<RectInst>,
}
impl Scene {
    fn rect(&mut self, r: Rect, fill: t::Rgba) {
        self.rects.push(RectInst {
            rect: [r.x, r.y, r.w, r.h],
            fill,
            border: t::TRANSPARENT,
            params: [0.0, 0.0, 0.0, 0.0],
        });
    }
    fn rrect(&mut self, r: Rect, fill: t::Rgba, radius: f32) {
        self.rects.push(RectInst {
            rect: [r.x, r.y, r.w, r.h],
            fill,
            border: t::TRANSPARENT,
            params: [radius, 0.0, 0.0, 0.0],
        });
    }
    fn bordered(&mut self, r: Rect, fill: t::Rgba, border: t::Rgba, radius: f32, bw: f32) {
        self.rects.push(RectInst {
            rect: [r.x, r.y, r.w, r.h],
            fill,
            border,
            params: [radius, bw, 0.0, 0.0],
        });
    }
    /// keycap: heavier bottom border (the spec's button/tab construction)
    fn keycap(&mut self, r: Rect, fill: t::Rgba, border: t::Rgba, radius: f32, bottom: f32) {
        self.rects.push(RectInst {
            rect: [r.x, r.y, r.w, r.h],
            fill,
            border,
            params: [radius, 1.0, bottom, 0.0],
        });
    }
    fn dot(&mut self, cx: f32, cy: f32, d: f32, fill: t::Rgba) {
        self.rrect(
            Rect::new(cx - d / 2.0, cy - d / 2.0, d, d),
            fill,
            t::RADIUS_DOT,
        );
    }
}

// ----------------------------------------------------------------------------
// Text: queued runs, flushed via glyphon
// ----------------------------------------------------------------------------
struct TextRun {
    text: String,
    x: f32,
    y: f32,
    size: f32,
    color: t::Rgba,
    mono: bool,
    weight: Weight,
    clip_w: f32,
}

struct TextLayer {
    runs: Vec<TextRun>,
}
impl TextLayer {
    fn new() -> Self {
        Self { runs: Vec::new() }
    }
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        color: t::Rgba,
        mono: bool,
        weight: Weight,
        clip_w: f32,
    ) {
        self.runs.push(TextRun {
            text: text.to_string(),
            x,
            y,
            size,
            color,
            mono,
            weight,
            clip_w,
        });
    }
    fn sans(&mut self, text: &str, x: f32, y: f32, size: f32, color: t::Rgba) {
        self.add(text, x, y, size, color, false, Weight::NORMAL, 1000.0);
    }
    fn mono(&mut self, text: &str, x: f32, y: f32, size: f32, color: t::Rgba) {
        self.add(text, x, y, size, color, true, Weight::NORMAL, 1000.0);
    }
}

fn col(c: t::Rgba) -> Color {
    Color::rgba(
        (c[0] * 255.0) as u8,
        (c[1] * 255.0) as u8,
        (c[2] * 255.0) as u8,
        (c[3] * 255.0) as u8,
    )
}

// ----------------------------------------------------------------------------
// Build the scene (the actual chrome mock). Pure layout — no GPU here.
// ----------------------------------------------------------------------------
struct Built {
    scene: Scene,
    text: TextLayer,
    viewport: Rect, // where the page texture composites
}

fn build_chrome() -> Built {
    let mut s = Scene::default();
    let mut tx = TextLayer::new();
    let full = Rect::new(0.0, 0.0, W as f32, H as f32);

    // page background
    s.rect(full, t::BG);

    // ---- top: tab bar (40px) ----
    let (tabbar, below) = full.cut_top(t::CHROME_TABBAR);
    s.rect(tabbar, t::SURFACE_1);
    // bottom hairline border
    s.rect(
        Rect::new(tabbar.x, tabbar.y + tabbar.h - 1.0, tabbar.w, 1.0),
        t::BORDER,
    );

    let tabs = [
        ("motesh.dev — themes", true),
        ("build #482 — running", false),
        ("1Password", false),
    ];
    let tab_w = 220.0;
    let mut tx_cursor = tabbar;
    for (i, (title, active)) in tabs.iter().enumerate() {
        let (tab, rest) = tx_cursor.cut_left(tab_w);
        tx_cursor = rest;
        if *active {
            s.rect(tab, t::BG);
            // accent top border (2px)
            s.rect(Rect::new(tab.x, tab.y, tab.w, 2.0), t::ACCENT);
        }
        // right hairline divider
        s.rect(
            Rect::new(tab.x + tab.w - 1.0, tab.y + 6.0, 1.0, tab.h - 12.0),
            t::BORDER,
        );
        // favicon dot
        let cy = tab.y + tab.h / 2.0;
        let dot_color = if i == 1 { t::SUCCESS } else { t::ACCENT };
        s.dot(tab.x + 14.0, cy, 8.0, dot_color);
        // title (mono-sm)
        let title_color = if *active { t::FG } else { t::FG_2 };
        tx.add(
            title,
            tab.x + 26.0,
            cy - t::TEXT_MONO_SM / 2.0 - 1.0,
            t::TEXT_MONO_SM,
            title_color,
            true,
            Weight::NORMAL,
            tab_w - 60.0,
        );
        // close x (revealed on active/hover; show on active)
        if *active {
            tx.sans("×", tab.x + tab.w - 22.0, cy - 8.0, 15.0, t::FG_2);
        }
    }
    // "+" new-tab button
    let plus_x = tx_cursor.x + 4.0;
    tx.sans("+", plus_x + 8.0, tabbar.y + 10.0, 18.0, t::FG_2);

    // ---- omnibox row (36px) ----
    let (omnirow, body) = below.cut_top(t::CHROME_OMNIBOX);
    s.rect(omnirow, t::SURFACE_1);
    s.rect(
        Rect::new(omnirow.x, omnirow.y + omnirow.h - 1.0, omnirow.w, 1.0),
        t::BORDER,
    );
    // the omnibox field (sunk well, radius-1, accent border = focused)
    let field = Rect::new(
        omnirow.x + t::SPACE_3,
        omnirow.y + 6.0,
        omnirow.w - t::SPACE_3 * 2.0 - 80.0,
        24.0,
    );
    s.bordered(field, t::SURFACE_SUNK, t::ACCENT, t::RADIUS_1, 1.0);
    // mode tag [url]
    let (mode_tag, _) = field.cut_left(54.0);
    s.rect(
        Rect::new(mode_tag.x + 1.0, mode_tag.y + 1.0, mode_tag.w, mode_tag.h - 2.0),
        t::SURFACE_1,
    );
    let mode_y = field.y + field.h / 2.0 - t::TEXT_MONO / 2.0 - 1.0;
    tx.mono("[", field.x + 10.0, mode_y, t::TEXT_MONO, t::ACCENT);
    tx.mono("url", field.x + 17.0, mode_y, t::TEXT_MONO, t::FG);
    tx.mono("]", field.x + 38.0, mode_y, t::TEXT_MONO, t::ACCENT);
    // secure glyph + URL with host-dim / host / path coloring
    let mut ux = field.x + 64.0;
    tx.mono("\u{2388}", ux, mode_y, t::TEXT_MONO, t::SUCCESS); // ⎈
    ux += 16.0;
    let segs: [(&str, t::Rgba); 3] = [
        ("github.com/motesh/", t::FG_2),
        ("mote", t::FG),
        ("/blob/main/init.lua", t::FG_2),
    ];
    for (seg, c) in segs {
        tx.mono(seg, ux, mode_y, t::TEXT_MONO, c);
        ux += seg.chars().count() as f32 * 7.8; // JetBrains Mono ~0.6em advance @13px
    }
    // two icon buttons on the right (star, panel)
    let icon1 = Rect::new(omnirow.x + omnirow.w - 64.0, omnirow.y + 6.0, 24.0, 24.0);
    let icon2 = Rect::new(omnirow.x + omnirow.w - 34.0, omnirow.y + 6.0, 24.0, 24.0);
    s.bordered(icon1, t::SURFACE_1, t::BORDER, t::RADIUS_1, 1.0);
    s.bordered(icon2, t::SURFACE_1, t::BORDER, t::RADIUS_1, 1.0);
    tx.sans("\u{2605}", icon1.x + 6.0, icon1.y + 4.0, 13.0, t::FG_2); // ★
    tx.sans("\u{25A3}", icon2.x + 6.0, icon2.y + 4.0, 13.0, t::FG_2); // ▣

    // ---- left sidebar (280px) ----
    let (sidebar, viewport) = body.cut_left(t::SIDEBAR_W);
    s.rect(sidebar, t::SURFACE_1);
    s.rect(
        Rect::new(sidebar.x + sidebar.w - 1.0, sidebar.y, 1.0, sidebar.h),
        t::BORDER,
    );

    let pad = t::SPACE_4;
    let mut cy = sidebar.y + pad;
    // header
    tx.add(
        "Browser Integrity",
        sidebar.x + pad,
        cy,
        t::TEXT_H3,
        t::FG,
        false,
        Weight::SEMIBOLD,
        sidebar.w,
    );
    cy += 30.0;

    // plugin card
    let card = Rect::new(
        sidebar.x + pad,
        cy,
        sidebar.w - pad * 2.0,
        152.0,
    );
    s.bordered(card, t::SURFACE_1, t::BORDER, t::RADIUS_2, 1.0);
    let cpad = t::SPACE_3;
    let mut yy = card.y + cpad;
    // title (mono, since it's a plugin id / dev tooling context)
    tx.mono(
        "password-manager-1password",
        card.x + cpad,
        yy,
        t::TEXT_MONO,
        t::FG,
    );
    yy += 18.0;
    // second row: version + verified badge side by side
    tx.mono("v1.0.0", card.x + cpad, yy, t::TEXT_MONO_SM, t::FG_2);
    let badge = Rect::new(card.x + cpad + 52.0, yy - 3.0, 70.0, 18.0);
    s.bordered(
        badge,
        t::TRANSPARENT,
        t::with_a(t::SUCCESS, 0.5),
        t::RADIUS_1,
        1.0,
    );
    s.dot(badge.x + 9.0, badge.y + badge.h / 2.0, 6.0, t::SUCCESS);
    tx.mono("verified", badge.x + 16.0, badge.y + 4.0, 10.0, t::SUCCESS);
    yy += 22.0;

    // 3 permission lines (bullet + mono text). The 1Password glob is long;
    // mono-sm at a slightly reduced size keeps it on one line inside 248px.
    let perms = [
        "http:fetch:https://*.1password.com/*",
        "storage:persistent",
        "crypto:seal_to_plugin",
    ];
    for p in perms {
        s.dot(card.x + cpad + 3.0, yy + 6.0, 4.0, t::FG_2);
        tx.add(
            p,
            card.x + cpad + 12.0,
            yy,
            9.5,
            t::FG_1,
            true,
            Weight::NORMAL,
            card.w - cpad * 2.0 - 12.0,
        );
        yy += 16.0;
    }
    yy += 6.0;
    // action buttons row: Revoke (danger), Update (secondary)
    let bh = 26.0;
    let revoke = Rect::new(card.x + cpad, yy, 70.0, bh);
    let update = Rect::new(card.x + cpad + 78.0, yy, 70.0, bh);
    // danger button: transparent fill, ember-tinted border
    s.keycap(
        revoke,
        t::TRANSPARENT,
        t::with_a(t::hex("#C84A2C"), 0.45),
        t::RADIUS_1,
        2.0,
    );
    tx.add(
        "Revoke",
        revoke.x + 14.0,
        revoke.y + 6.0,
        t::TEXT_SMALL,
        t::hex("#C84A2C"),
        false,
        Weight::MEDIUM,
        70.0,
    );
    // secondary keycap button
    s.keycap(update, t::SURFACE_1, t::BORDER_STRONG, t::RADIUS_1, 2.0);
    tx.add(
        "Update",
        update.x + 14.0,
        update.y + 6.0,
        t::TEXT_SMALL,
        t::FG,
        false,
        Weight::MEDIUM,
        70.0,
    );

    Built {
        scene: s,
        text: tx,
        viewport,
    }
}

// ----------------------------------------------------------------------------
// GPU
// ----------------------------------------------------------------------------
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    screen: [f32; 2],
    _pad: [f32; 2],
}

fn vmrss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// Procedural gradient "page" texture — stand-in for a CEF OSR frame.
fn make_page_texture(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32) -> wgpu::Texture {
    let mut data = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            // warm dusk-ish diagonal gradient with a subtle grid
            let grid = if x % 64 == 0 || y % 64 == 0 { 12 } else { 0 };
            data[i] = (20.0 + fx * 40.0) as u8 + grid;
            data[i + 1] = (18.0 + fy * 30.0) as u8 + grid;
            data[i + 2] = (15.0 + (fx + fy) * 25.0) as u8 + grid;
            data[i + 3] = 255;
        }
    }
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("page"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    tex
}

fn main() {
    pollster::block_on(run());
}

async fn run() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })
        .await
        .expect("no adapter");
    eprintln!("adapter: {:?}", adapter.get_info());
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        })
        .await
        .expect("no device");

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    // offscreen render target
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&Default::default());

    // globals uniform
    let globals = Globals {
        screen: [W as f32, H as f32],
        _pad: [0.0, 0.0],
    };
    let globals_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("globals"),
        contents: bytemuck::bytes_of(&globals),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("globals-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("globals-bg"),
        layout: &globals_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: globals_buf.as_entire_binding(),
        }],
    });

    // ---- rect pipeline ----
    let rect_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("rect"),
        source: wgpu::ShaderSource::Wgsl(include_str!("rect.wgsl").into()),
    });
    let rect_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rect-pl"),
        bind_group_layouts: &[Some(&globals_bgl)],
        immediate_size: 0,
    });
    let inst_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<RectInst>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 48,
                shader_location: 3,
                format: wgpu::VertexFormat::Float32x4,
            },
        ],
    };
    let blend = Some(wgpu::BlendState::ALPHA_BLENDING);
    let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("rect-pipeline"),
        layout: Some(&rect_pl),
        vertex: wgpu::VertexState {
            module: &rect_shader,
            entry_point: Some("vs_main"),
            buffers: &[inst_layout],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &rect_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    // ---- blit pipeline ----
    let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blit"),
        source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
    });
    let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("blit-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let blit_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("blit-pl"),
        bind_group_layouts: &[Some(&globals_bgl), Some(&blit_bgl)],
        immediate_size: 0,
    });
    let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blit-pipeline"),
        layout: Some(&blit_pl),
        vertex: wgpu::VertexState {
            module: &blit_shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &blit_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    // page texture + bind group
    let page_tex = make_page_texture(&device, &queue, 1024, 640);
    let page_view = page_tex.create_view(&Default::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    // ---- glyphon text ----
    let mut font_system = FontSystem::new();
    let mut swash = SwashCache::new();
    let cache = Cache::new(&device);
    let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
    let mut text_renderer =
        TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
    let mut text_viewport = Viewport::new(&device, &cache);
    text_viewport.update(&queue, Resolution { width: W, height: H });

    // ---- render closure ----
    let mut render_once = |save: bool| {
        let built = build_chrome();

        // viewport rect uniform for the blit
        let vp = built.viewport;
        let push_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("blit-push"),
            contents: bytemuck::cast_slice(&[vp.x, vp.y, vp.w, vp.h]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let blit_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit-bg"),
            layout: &blit_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&page_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: push_buf.as_entire_binding(),
                },
            ],
        });

        let inst_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("inst"),
            contents: bytemuck::cast_slice(&built.scene.rects),
            usage: wgpu::BufferUsages::VERTEX,
        });

        // build glyphon text buffers
        let mut text_buffers: Vec<(TextBuffer, &TextRun)> = Vec::new();
        for run in &built.text.runs {
            let mut buf = TextBuffer::new(&mut font_system, Metrics::new(run.size, run.size * 1.3));
            buf.set_size(&mut font_system, Some(run.clip_w), Some(run.size * 1.4));
            buf.set_wrap(&mut font_system, Wrap::None);
            let family = if run.mono {
                Family::Name("JetBrainsMono Nerd Font")
            } else {
                Family::SansSerif
            };
            let attrs = Attrs::new().family(family).weight(run.weight).color(col(run.color));
            buf.set_text(&mut font_system, &run.text, &attrs, Shaping::Advanced, None);
            buf.shape_until_scroll(&mut font_system, false);
            text_buffers.push((buf, run));
        }
        let text_areas: Vec<TextArea> = text_buffers
            .iter()
            .map(|(buf, run)| TextArea {
                buffer: buf,
                left: run.x,
                top: run.y,
                scale: 1.0,
                bounds: TextBounds {
                    left: run.x as i32,
                    top: run.y as i32,
                    right: (run.x + run.clip_w) as i32,
                    bottom: (run.y + run.size * 1.6) as i32,
                },
                default_color: col(run.color),
                custom_glyphs: &[],
            })
            .collect();
        text_renderer
            .prepare(
                &device,
                &queue,
                &mut font_system,
                &mut atlas,
                &text_viewport,
                text_areas,
                &mut swash,
            )
            .unwrap();

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: t::BG[0] as f64,
                            g: t::BG[1] as f64,
                            b: t::BG[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // 1. rects (bg, chrome panels, cards, buttons)
            pass.set_pipeline(&rect_pipeline);
            pass.set_bind_group(0, &globals_bg, &[]);
            pass.set_vertex_buffer(0, inst_buf.slice(..));
            pass.draw(0..6, 0..built.scene.rects.len() as u32);
            // 2. composite page texture into viewport slot
            pass.set_pipeline(&blit_pipeline);
            pass.set_bind_group(0, &globals_bg, &[]);
            pass.set_bind_group(1, &blit_bg, &[]);
            pass.draw(0..6, 0..1);
            // 3. text on top
            text_renderer
                .render(&atlas, &text_viewport, &mut pass)
                .unwrap();
        }
        queue.submit([encoder.finish()]);

        if save {
            save_png(&device, &queue, &target);
        }
        atlas.trim();
    };

    // warmup + save
    render_once(true);

    // benchmark: 100 offscreen renders
    let n = 100;
    let start = std::time::Instant::now();
    for _ in 0..n {
        render_once(false);
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
    }
    let elapsed = start.elapsed();
    let avg_ms = elapsed.as_secs_f64() * 1000.0 / n as f64;

    let rss = vmrss_kb();
    eprintln!("=== METRICS ===");
    eprintln!("rects per frame: {}", build_chrome().scene.rects.len());
    eprintln!("text runs per frame: {}", build_chrome().text.runs.len());
    eprintln!("avg frame time over {n}: {avg_ms:.3} ms");
    eprintln!("VmRSS: {} KB ({:.1} MB)", rss, rss as f64 / 1024.0);
    println!("AVG_MS={avg_ms:.3}");
    println!("RSS_KB={rss}");
}

fn save_png(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture) {
    // bytes_per_row must be 256-aligned
    let unpadded = W * 4;
    let align = 256;
    let padded = unpadded.div_ceil(align) * align;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &out_buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = out_buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).ok();
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    rx.recv().unwrap().unwrap();

    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((unpadded * H) as usize);
    for row in 0..H {
        let start = (row * padded) as usize;
        pixels.extend_from_slice(&data[start..start + unpadded as usize]);
    }
    drop(data);
    out_buf.unmap();

    let img: image::RgbaImage = image::ImageBuffer::from_raw(W, H, pixels).unwrap();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/out.png");
    img.save(path).unwrap();
    eprintln!("saved {path}");
}
