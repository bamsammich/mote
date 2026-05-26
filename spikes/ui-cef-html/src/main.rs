//! Mote UI spike #3 — chrome as an HTML/CSS document rendered by CEF (off-screen),
//! composited with a thin wgpu blit layer.
//!
//! Pipeline (headless, deterministic):
//!   1. process split via execute_process (browser process continues; subprocess
//!      re-execs the helper bin and exits).
//!   2. initialize CEF with windowless_rendering_enabled + external_message_pump.
//!   3. create TWO OSR browsers: the chrome doc (transparent bg) and a page doc.
//!   4. pump do_message_loop_work() until both deliver a CPU on_paint BGRA frame.
//!   5. composite: page into the [viewport] rect, chrome over the whole 1280x800.
//!   6. read back -> out.png. Report RSS + timings.
use cef::rc::Rc as _;
use cef::{
    api_hash, args::Args, browser_host_create_browser_sync, do_message_loop_work, execute_process,
    initialize, shutdown, sys, wrap_client, wrap_render_handler, App, Browser, BrowserSettings,
    CefString, Client, ImplBrowser, ImplBrowserHost, ImplClient, ImplCommandLine,
    ImplRenderHandler, PaintElementType, Rect, RenderHandler, Settings, WindowInfo,
    WrapClient, WrapRenderHandler,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

const W: u32 = 1280;
const H: u32 = 800;

// Viewport rect inside the chrome (must match the grid in chrome.html):
// sidebar 280px wide, top chrome = tabbar(40) + omnibox(36) = 76px.
const VP_X: u32 = 280;
const VP_Y: u32 = 76;
const VP_W: u32 = W - VP_X; // 1000
const VP_H: u32 = H - VP_Y; // 724

/// A shared BGRA framebuffer that a RenderHandler writes into on every on_paint.
#[derive(Clone)]
struct Frame {
    buf: Rc<RefCell<Vec<u8>>>,
    w: Rc<RefCell<u32>>,
    h: Rc<RefCell<u32>>,
    paints: Rc<RefCell<u32>>,
}
impl Frame {
    fn new() -> Self {
        Self {
            buf: Rc::new(RefCell::new(Vec::new())),
            w: Rc::new(RefCell::new(0)),
            h: Rc::new(RefCell::new(0)),
            paints: Rc::new(RefCell::new(0)),
        }
    }
}

// ---------- RenderHandler (CPU on_paint path) ----------
// The state the handler closes over. The wrap_render_handler! macro generates
// the RcImpl refcounting boilerplate; we only write the behavior.
#[derive(Clone)]
struct OsrHandler {
    frame: Frame,
    w: i32,
    h: i32,
}

wrap_render_handler! {
    struct RenderHandlerBuilder {
        inner: OsrHandler,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(r) = rect {
                r.x = 0;
                r.y = 0;
                r.width = self.inner.w;
                r.height = self.inner.h;
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if type_ != PaintElementType::default() || buffer.is_null() || width <= 0 || height <= 0
            {
                return;
            }
            let n = (width * height * 4) as usize;
            let src = unsafe { std::slice::from_raw_parts(buffer, n) };
            *self.inner.frame.buf.borrow_mut() = src.to_vec();
            *self.inner.frame.w.borrow_mut() = width as u32;
            *self.inner.frame.h.borrow_mut() = height as u32;
            *self.inner.frame.paints.borrow_mut() += 1;
        }
    }
}

// ---------- Client (returns our RenderHandler) ----------
wrap_client! {
    struct ClientBuilder {
        render: RenderHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }
    }
}

fn make_browser(url: &str, w: i32, h: i32) -> (Browser, Frame) {
    let frame = Frame::new();
    let render = RenderHandlerBuilder::new(OsrHandler {
        frame: frame.clone(),
        w,
        h,
    });
    let mut client = ClientBuilder::new(render);

    let window_info = WindowInfo {
        windowless_rendering_enabled: 1,
        ..Default::default()
    };
    let settings = BrowserSettings {
        windowless_frame_rate: 60,
        ..Default::default()
    };
    let browser = browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(url)),
        Some(&settings),
        None,
        None,
    )
    .expect("create OSR browser");
    (browser, frame)
}

fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: f64 = rest.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0.0);
            return kb / 1024.0;
        }
    }
    0.0
}

/// Sum RSS (MB) of all CEF subprocesses, broken down by --type=. The chrome
/// renderer is the `renderer` process hosting our chrome document.
fn child_rss_breakdown() -> Vec<(String, f64)> {
    let mut out = Vec::new();
    let me = std::process::id();
    let proc = std::path::Path::new("/proc");
    let Ok(entries) = std::fs::read_dir(proc) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid_s) = name.to_str() else { continue };
        let Ok(pid) = pid_s.parse::<u32>() else { continue };
        if pid == me {
            continue;
        }
        let cmd = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if !cmd.contains("ui-cef-html-spike") {
            continue;
        }
        let typ = cmd
            .split('\0')
            .find_map(|a| a.strip_prefix("--type=").map(|s| s.to_string()))
            .unwrap_or_else(|| "browser-child".into());
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let rss = status
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|r| r.trim().trim_end_matches(" kB").trim().parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1024.0;
        out.push((typ, rss));
    }
    out
}

fn main() -> std::process::ExitCode {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();
    let cmd = args.as_cmd_line().expect("cmd line");
    let switch = CefString::from("type");
    let is_browser_process = cmd.has_switch(Some(&switch)) != 1;

    // process split — in a subprocess this runs the CEF loop and never returns.
    let ret = execute_process(
        Some(args.as_main_args()),
        None::<&mut App>,
        std::ptr::null_mut(),
    );
    if !is_browser_process {
        return 0.into();
    }
    assert_eq!(ret, -1, "browser process: execute_process must return -1");

    let cache = std::env::current_dir().unwrap().join(".cef-cache");
    let _ = std::fs::create_dir_all(&cache);
    let settings = Settings {
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        no_sandbox: 1,
        root_cache_path: CefString::from(&*cache.to_string_lossy()),
        ..Default::default()
    };
    assert_eq!(
        initialize(
            Some(args.as_main_args()),
            Some(&settings),
            None::<&mut App>,
            std::ptr::null_mut(),
        ),
        1,
        "cef initialize failed"
    );

    // file:// URLs to our local chrome + page documents.
    let dir = std::env::current_dir().unwrap();
    let chrome_url = format!("file://{}/chrome/chrome.html", dir.display());
    let page_url = format!("file://{}/chrome/page.html", dir.display());

    let (chrome_b, chrome_frame) = make_browser(&chrome_url, W as i32, H as i32);
    let (page_b, page_frame) = make_browser(&page_url, VP_W as i32, VP_H as i32);

    // pump the loop until both browsers have painted at least once (+ a few extra
    // to let fonts/layout settle), or a timeout.
    let start = Instant::now();
    let mut first_both: Option<Instant> = None;
    loop {
        do_message_loop_work();
        std::thread::sleep(std::time::Duration::from_millis(4));
        let cp = *chrome_frame.paints.borrow();
        let pp = *page_frame.paints.borrow();
        // Both have painted at least once -> we have a frame. Give a short settle
        // window (250ms) to absorb any font/layout follow-up repaint, then stop.
        if cp >= 1 && pp >= 1 {
            let t = first_both.get_or_insert_with(Instant::now);
            if t.elapsed().as_millis() > 250 {
                break;
            }
        }
        if start.elapsed().as_secs() > 15 {
            eprintln!("TIMEOUT waiting for first paint: chrome={cp} page={pp}");
            break;
        }
    }
    let paint_ms = start.elapsed().as_millis();
    let rss = rss_mb();
    eprintln!(
        "PAINTED chrome={} page={} in {paint_ms}ms  browser-process RSS={rss:.0}MB (pre-wgpu)",
        *chrome_frame.paints.borrow(),
        *page_frame.paints.borrow()
    );
    let mut total_child = 0.0;
    for (typ, mb) in child_rss_breakdown() {
        eprintln!("  CEF subprocess --type={typ:<12} RSS={mb:.0}MB");
        total_child += mb;
    }
    eprintln!("  CEF subprocesses total RSS={total_child:.0}MB");
    eprintln!("  => full shell (browser proc + all CEF subprocs) = {:.0}MB", rss + total_child);

    // ---- composite via wgpu blit (page first, chrome over) ----
    let comp_start = Instant::now();
    let out = pollster::block_on(composite(&chrome_frame, &page_frame));
    let comp_ms = comp_start.elapsed().as_micros() as f64 / 1000.0;
    eprintln!("composite {comp_ms:.2}ms");

    image::save_buffer(
        dir.join("out.png"),
        &out,
        W,
        H,
        image::ColorType::Rgba8,
    )
    .expect("save png");
    eprintln!("wrote out.png  final RSS={:.0}MB", rss_mb());

    // tidy: close browsers + shutdown
    if let Some(host) = chrome_b.host() {
        host.close_browser(1);
    }
    if let Some(host) = page_b.host() {
        host.close_browser(1);
    }
    for _ in 0..20 {
        do_message_loop_work();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    shutdown();
    std::process::ExitCode::SUCCESS
}

/// wgpu compositor: blit page BGRA into the viewport rect, then chrome BGRA
/// over the full frame (chrome has transparent viewport area -> page shows).
async fn composite(chrome: &Frame, page: &Frame) -> Vec<u8> {
    use wgpu::util::DeviceExt;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .unwrap();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .unwrap();

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("composite"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&Default::default());

    // upload a BGRA frame as an Rgba8Unorm texture (we swizzle in the shader).
    let make_tex = |buf: &[u8], w: u32, h: u32| -> wgpu::TextureView {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("src"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
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
            buf,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        tex.create_view(&Default::default())
    };

    let cw = *chrome.w.borrow();
    let ch = *chrome.h.borrow();
    let pw = *page.w.borrow();
    let ph = *page.h.borrow();
    let chrome_view = make_tex(&chrome.buf.borrow(), cw, ch);
    let page_view = make_tex(&page.buf.borrow(), pw, ph);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blit"),
        source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
    });

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Push {
        rect: [f32; 4],
        screen: [f32; 2],
        _pad: [f32; 2],
    }

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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

    let draw = |rect: [f32; 4], view: &wgpu::TextureView| -> (wgpu::Buffer, wgpu::BindGroup) {
        let push = Push { rect, screen: [W as f32, H as f32], _pad: [0.0; 2] };
        let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&push),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        (ubuf, bg)
    };

    let (_pb, page_bg) = draw([VP_X as f32, VP_Y as f32, VP_W as f32, VP_H as f32], &page_view);
    let (_cb, chrome_bg) = draw([0.0, 0.0, W as f32, H as f32], &chrome_view);

    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.05, g: 0.04, b: 0.03, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        });
        rp.set_pipeline(&pipeline);
        // page first
        rp.set_bind_group(0, &page_bg, &[]);
        rp.draw(0..6, 0..1);
        // chrome over (transparent viewport region lets page show through)
        rp.set_bind_group(0, &chrome_bg, &[]);
        rp.draw(0..6, 0..1);
    }

    // readback
    let bpr = 4 * W;
    let padded = (bpr + 255) / 256 * 256;
    let rbuf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (padded * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &rbuf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    queue.submit(Some(enc.finish()));

    let slice = rbuf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    let data = slice.get_mapped_range();
    let mut out = vec![0u8; (4 * W * H) as usize];
    for y in 0..H {
        let s = (y * padded) as usize;
        let d = (y * 4 * W) as usize;
        out[d..d + (4 * W) as usize].copy_from_slice(&data[s..s + (4 * W) as usize]);
    }
    out
}
