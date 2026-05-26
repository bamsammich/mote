//! The thin wgpu compositor (ADR-0003, `docs/plans/02-browser-shell.md` §1.1).
//!
//! Mote's chrome is an HTML/CSS document rendered off-screen by a CEF browser;
//! each web page is rendered off-screen by its own CEF browser. The compositor
//! is the thin wgpu layer that draws those off-screen frames into the window:
//!
//! 1. clear the surface,
//! 2. blit the **focused page** texture into the viewport rect the chrome
//!    reports,
//! 3. blit the **chrome** texture over the full surface — the chrome is
//!    transparent in the viewport region, so the page shows through
//!    (*chrome-surrounds-content*),
//! 4. present.
//!
//! A page/tab switch is a texture swap (step 2 binds a different page texture);
//! the chrome texture is reused. The compositor is decoupled from CEF: it
//! accepts raw frame buffers via [`Compositor::update_chrome`] /
//! [`Compositor::update_page`], so this crate carries **no** `mote-cef`
//! dependency. The shell feeds it bytes from `mote-cef` paint frames.
//!
//! Two render paths share one pipeline:
//!
//! - [`Compositor::render`] — composite into the window surface (the shell's
//!   per-frame path).
//! - [`Compositor::render_offscreen_png`] — composite into an offscreen
//!   texture and read it back as PNG bytes (headless testing, the same
//!   approach the `ui-wgpu` spike used to produce evidence without a window).
//!
//! The window itself is created by `mote-shell` (winit); the compositor only
//! receives a window handle. wgpu 29's `create_surface` is **safe** for a
//! `raw-window-handle` target, so this crate contains no `unsafe`.

use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

/// A pixel-space rectangle on the render target: the region the focused page
/// texture is blitted into (the chrome's `<main data-slot>` geometry).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportRect {
    /// Left edge, in physical pixels.
    pub x: f32,
    /// Top edge, in physical pixels.
    pub y: f32,
    /// Width, in physical pixels.
    pub width: f32,
    /// Height, in physical pixels.
    pub height: f32,
}

impl ViewportRect {
    /// A rectangle from position and size, in physical pixels.
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn as_uniform_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.x.to_ne_bytes());
        out[4..8].copy_from_slice(&self.y.to_ne_bytes());
        out[8..12].copy_from_slice(&self.width.to_ne_bytes());
        out[12..16].copy_from_slice(&self.height.to_ne_bytes());
        out
    }
}

/// The byte layout of a frame buffer handed to the compositor.
///
/// CEF's CPU off-screen path delivers `BGRA8` premultiplied bytes; loaded
/// images and procedural stand-ins are typically `RGBA8`. The compositor
/// uploads either into a wgpu texture, swizzling `BGRA` to `RGBA` in the
/// shader's color space at sample time via the texture format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// Blue, green, red, alpha — CEF `on_paint` byte order.
    Bgra8,
    /// Red, green, blue, alpha.
    Rgba8,
}

impl PixelFormat {
    const fn texture_format(self) -> wgpu::TextureFormat {
        match self {
            // `*UnormSrgb` so the blit samples in linear space and the
            // offscreen/surface targets (also sRGB) round-trip faithfully.
            Self::Bgra8 => wgpu::TextureFormat::Bgra8UnormSrgb,
            Self::Rgba8 => wgpu::TextureFormat::Rgba8UnormSrgb,
        }
    }
}

/// Errors the compositor can return.
#[derive(Debug, thiserror::Error)]
pub enum CompositorError {
    /// No wgpu adapter could be acquired for the surface/instance.
    #[error("no compatible wgpu adapter found")]
    NoAdapter,
    /// The wgpu device could not be created.
    #[error("failed to create wgpu device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    /// The surface could not be created from the window handle.
    #[error("failed to create wgpu surface: {0}")]
    CreateSurface(#[from] wgpu::CreateSurfaceError),
    /// A frame buffer's length did not match its declared dimensions.
    #[error("frame buffer is {got} bytes but {width}x{height} needs {want}")]
    BadBufferLen {
        /// Bytes supplied.
        got: usize,
        /// Bytes required (`width * height * 4`).
        want: usize,
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
    },
    /// Acquiring the next surface frame failed (timeout, lost, outdated, ...).
    #[error("failed to acquire surface frame: {0}")]
    AcquireFrame(&'static str),
    /// Reading back the offscreen target failed.
    #[error("failed to map offscreen readback buffer")]
    Readback,
}

/// An uploaded frame: its bind group (texture + sampler + dest-rect uniform)
/// and the dest-rect buffer so the destination can be rewritten on resize.
#[derive(Debug)]
struct Layer {
    bind_group: wgpu::BindGroup,
    dest_buffer: wgpu::Buffer,
}

/// Where the compositor draws: the window surface or an offscreen texture.
enum Target {
    /// The window surface plus its live configuration.
    Surface {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    /// Headless: no surface; [`Compositor::render_offscreen_png`] supplies the
    /// target each call.
    Offscreen { width: u32, height: u32 },
}

/// The thin wgpu compositor: device + queue + a blit pipeline, plus the chrome
/// and focused-page frame textures it composites each frame.
///
/// Construct with [`Compositor::new_for_window`] (on-surface path, for the
/// shell's window) or [`Compositor::new_offscreen`] (headless, for tests).
/// Feed frames with [`Compositor::update_chrome`] / [`Compositor::update_page`]
/// and draw with [`Compositor::render`] / [`Compositor::render_offscreen_png`].
pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    globals_bind_group: wgpu::BindGroup,
    globals_buffer: wgpu::Buffer,
    layer_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The format of the target the pipeline was built for.
    target_format: wgpu::TextureFormat,
    target: Target,
    width: u32,
    height: u32,
    chrome: Option<Layer>,
    page: Option<(Layer, ViewportRect)>,
}

impl std::fmt::Debug for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compositor")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("target_format", &self.target_format)
            .field("has_chrome", &self.chrome.is_some())
            .field("has_page", &self.page.is_some())
            .finish_non_exhaustive()
    }
}

impl Compositor {
    /// Create a compositor that renders into a window.
    ///
    /// `window` is any `raw-window-handle` target (e.g. a winit `Window`);
    /// this crate does not depend on winit. `width`/`height` are the window's
    /// physical pixel size. wgpu 29 makes `create_surface` safe for a raw
    /// handle, so no `unsafe` is involved.
    ///
    /// # Errors
    /// Fails if a surface, adapter, or device cannot be acquired.
    pub fn new_for_window<W>(window: W, width: u32, height: u32) -> Result<Self, CompositorError>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        // Safe in wgpu 29: the target is a `raw-window-handle` window, boxed
        // into a `SurfaceTarget::Window` — no `unsafe` block required.
        let surface = instance.create_surface(wgpu::SurfaceTarget::Window(Box::new(window)))?;

        let (adapter, device, queue) =
            pollster_block_on(Self::request_device(&instance, Some(&surface)))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: caps.present_modes[0],
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        Ok(Self::assemble(
            device,
            queue,
            format,
            Target::Surface { surface, config },
            width.max(1),
            height.max(1),
        ))
    }

    /// Create a headless compositor that renders into an offscreen texture.
    ///
    /// No window or surface is involved; use [`Compositor::render_offscreen_png`]
    /// to composite and read back PNG bytes. This is the spike's offscreen
    /// path, suitable for tests and CI without a display.
    ///
    /// # Errors
    /// Fails if no adapter or device can be acquired.
    pub fn new_offscreen(width: u32, height: u32) -> Result<Self, CompositorError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let (_adapter, device, queue) = pollster_block_on(Self::request_device(&instance, None))?;
        // sRGB so readback matches the surface path's color treatment.
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        Ok(Self::assemble(
            device,
            queue,
            format,
            Target::Offscreen {
                width: width.max(1),
                height: height.max(1),
            },
            width.max(1),
            height.max(1),
        ))
    }

    async fn request_device(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'static>>,
    ) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), CompositorError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: surface,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| CompositorError::NoAdapter)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("mote-ui compositor device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await?;
        Ok((adapter, device, queue))
    }

    fn assemble(
        device: wgpu::Device,
        queue: wgpu::Queue,
        target_format: wgpu::TextureFormat,
        target: Target,
        width: u32,
        height: u32,
    ) -> Self {
        // Globals: the target's pixel dimensions (vec2 + pad = 16 bytes).
        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compositor globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        write_globals(&queue, &globals_buffer, width, height);

        let globals_bgl = make_globals_bgl(&device);
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compositor globals bg"),
            layout: &globals_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let layer_bgl = make_layer_bgl(&device);
        let pipeline = make_pipeline(&device, target_format, &globals_bgl, &layer_bgl);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("compositor sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            device,
            queue,
            pipeline,
            globals_bind_group,
            globals_buffer,
            layer_bgl,
            sampler,
            target_format,
            target,
            width,
            height,
            chrome: None,
            page: None,
        }
    }

    /// The current target size in physical pixels (`(width, height)`).
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Upload a new chrome frame (the full-window chrome texture).
    ///
    /// `bytes` is `width * height * 4` bytes in `format`. The chrome is drawn
    /// over the full surface each frame; it is transparent in the viewport
    /// region so the page shows through.
    ///
    /// # Errors
    /// Returns [`CompositorError::BadBufferLen`] if `bytes` does not match
    /// `width * height * 4`.
    pub fn update_chrome(
        &mut self,
        bytes: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(), CompositorError> {
        let full = ViewportRect::new(0.0, 0.0, px_to_f32(self.width), px_to_f32(self.height));
        let layer = self.upload_layer("chrome", bytes, width, height, format, full)?;
        self.chrome = Some(layer);
        Ok(())
    }

    /// Upload a new focused-page frame and the viewport rect it is drawn into.
    ///
    /// `bytes` is `width * height * 4` bytes in `format`; `viewport` is the
    /// destination rectangle in physical pixels (the chrome-reported
    /// `<main data-slot>` geometry). A tab switch is simply another call here
    /// with a different buffer — a texture swap.
    ///
    /// # Errors
    /// Returns [`CompositorError::BadBufferLen`] if `bytes` does not match
    /// `width * height * 4`.
    pub fn update_page(
        &mut self,
        bytes: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
        viewport: ViewportRect,
    ) -> Result<(), CompositorError> {
        let layer = self.upload_layer("page", bytes, width, height, format, viewport)?;
        self.page = Some((layer, viewport));
        Ok(())
    }

    /// Drop the focused-page texture (e.g. when no tab is focused).
    pub fn clear_page(&mut self) {
        self.page = None;
    }

    fn upload_layer(
        &self,
        label: &str,
        bytes: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
        dest: ViewportRect,
    ) -> Result<Layer, CompositorError> {
        let want = width as usize * height as usize * 4;
        if bytes.len() != want {
            return Err(CompositorError::BadBufferLen {
                got: bytes.len(),
                want,
                width,
                height,
            });
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: format.texture_format(),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Each layer carries its own destination-rect uniform: the page uses
        // its viewport rect, the chrome the full target.
        let dest_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compositor layer dest"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&dest_buffer, 0, &dest.as_uniform_bytes());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.layer_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: dest_buffer.as_entire_binding(),
                },
            ],
        });
        Ok(Layer {
            bind_group,
            dest_buffer,
        })
    }

    /// Composite into the window surface and present.
    ///
    /// Order: clear, blit the focused page into its viewport rect, blit the
    /// chrome over the full surface, present. Returns without error and draws
    /// nothing extra if a layer is missing.
    ///
    /// # Errors
    /// Fails if the surface frame cannot be acquired (e.g. lost surface) or
    /// the compositor is in offscreen mode.
    pub fn render(&mut self) -> Result<(), CompositorError> {
        let Target::Surface { surface, .. } = &self.target else {
            return Ok(());
        };
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout => {
                return Err(CompositorError::AcquireFrame("timeout"));
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return Err(CompositorError::AcquireFrame("occluded"));
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                return Err(CompositorError::AcquireFrame("outdated"));
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return Err(CompositorError::AcquireFrame("lost"));
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(CompositorError::AcquireFrame("validation"));
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.encode_composite(&view);
        frame.present();
        Ok(())
    }

    /// Composite into an offscreen texture and return PNG-encoded bytes.
    ///
    /// The same composite order as [`Compositor::render`], drawn into a fresh
    /// offscreen target sized to the compositor, then read back and PNG
    /// encoded. Headless — no surface required.
    ///
    /// # Errors
    /// Fails if the readback buffer cannot be mapped.
    pub fn render_offscreen_png(&mut self) -> Result<Vec<u8>, CompositorError> {
        let (w, h) = (self.width, self.height);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("compositor offscreen target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.encode_composite(&view);
        let rgba = self.read_back_rgba(&texture, w, h)?;
        Ok(encode_png(&rgba, w, h))
    }

    /// Composite into an offscreen texture and return the raw RGBA8 pixels.
    ///
    /// Like [`Compositor::render_offscreen_png`] but skips PNG encoding —
    /// useful for region/pixel assertions in tests.
    ///
    /// # Errors
    /// Fails if the readback buffer cannot be mapped.
    pub fn render_offscreen_rgba(&mut self) -> Result<Vec<u8>, CompositorError> {
        let (w, h) = (self.width, self.height);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("compositor offscreen target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.encode_composite(&view);
        self.read_back_rgba(&texture, w, h)
    }

    /// Record and submit the composite pass into `view`.
    fn encode_composite(&self, view: &wgpu::TextureView) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compositor encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("compositor composite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.globals_bind_group, &[]);

            // 1. focused page into its viewport rect.
            if let Some((layer, _)) = &self.page {
                pass.set_bind_group(1, &layer.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
            // 2. chrome over the full window (transparent viewport region lets
            //    the page show through).
            if let Some(layer) = &self.chrome {
                pass.set_bind_group(1, &layer.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
    }

    fn read_back_rgba(
        &self,
        texture: &wgpu::Texture,
        w: u32,
        h: u32,
    ) -> Result<Vec<u8>, CompositorError> {
        // bytes_per_row must be 256-aligned for copy_texture_to_buffer.
        let unpadded = w * 4;
        let padded = unpadded.div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compositor readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compositor readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|_| CompositorError::Readback)?;
        rx.recv()
            .map_err(|_| CompositorError::Readback)?
            .map_err(|_| CompositorError::Readback)?;

        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        buffer.unmap();
        Ok(pixels)
    }

    /// Resize the target and (for the surface path) reconfigure the surface.
    ///
    /// No-op for zero dimensions. The chrome should be re-uploaded at the new
    /// size and the page viewport recomputed by the caller after a resize.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.width = width;
        self.height = height;
        write_globals(&self.queue, &self.globals_buffer, width, height);
        match &mut self.target {
            Target::Surface { surface, config } => {
                config.width = width;
                config.height = height;
                surface.configure(&self.device, config);
            }
            Target::Offscreen {
                width: w,
                height: h,
            } => {
                *w = width;
                *h = height;
            }
        }
        // The chrome covers the full window; keep its dest rect in sync so a
        // retained chrome texture still maps over the resized target until the
        // caller re-uploads a correctly sized chrome frame.
        if let Some(chrome) = &self.chrome {
            let full = ViewportRect::new(0.0, 0.0, px_to_f32(width), px_to_f32(height));
            self.queue
                .write_buffer(&chrome.dest_buffer, 0, &full.as_uniform_bytes());
        }
    }
}

/// Convert a pixel dimension to `f32` for shader uniforms.
///
/// Window/texture dimensions are far below `2^24`, the largest integer `f32`
/// represents exactly, so no precision is lost in practice.
#[allow(clippy::cast_precision_loss)]
const fn px_to_f32(v: u32) -> f32 {
    v as f32
}

fn write_globals(queue: &wgpu::Queue, buffer: &wgpu::Buffer, width: u32, height: u32) {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&px_to_f32(width).to_ne_bytes());
    bytes[4..8].copy_from_slice(&px_to_f32(height).to_ne_bytes());
    queue.write_buffer(buffer, 0, &bytes);
}

/// Bind-group layout for the globals uniform (target pixel dimensions).
fn make_globals_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("compositor globals bgl"),
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
    })
}

/// Bind-group layout for a layer: texture + sampler + dest-rect uniform.
fn make_layer_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("compositor layer bgl"),
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
    })
}

/// The blit render pipeline: a full-screen-triangle-pair vertex stage that maps
/// a layer's dest rect to NDC, alpha-blended so transparent chrome regions let
/// the underlying page show through.
fn make_pipeline(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    globals_bgl: &wgpu::BindGroupLayout,
    layer_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("compositor blit shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("compositor/blit.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("compositor pipeline layout"),
        bind_group_layouts: &[Some(globals_bgl), Some(layer_bgl)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("compositor blit pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

// ----------------------------------------------------------------------------
// Minimal async block_on (no pollster dependency).
// ----------------------------------------------------------------------------

/// Drive a future to completion on the current thread.
///
/// wgpu's native `request_adapter`/`request_device` futures resolve after the
/// instance is polled; a trivial busy-poll executor suffices and avoids a
/// `pollster` dependency (keeping the crate's dep surface to wgpu +
/// raw-window-handle).
fn pollster_block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = Box::pin(fut);
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

/// A `Waker` that does nothing — the busy-poll executor re-polls each loop
/// iteration, so wake notifications are unnecessary. Built via the safe
/// [`Wake`] trait, avoiding the raw-vtable `unsafe` path.
struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
    fn wake_by_ref(self: &Arc<Self>) {}
}

// ----------------------------------------------------------------------------
// Minimal PNG encoder (no `image`/`png` dependency).
// ----------------------------------------------------------------------------

/// Encode RGBA8 pixels as a PNG (zlib stored blocks; no compression).
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);

    // IHDR
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type: RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_chunk(&mut out, *b"IHDR", &ihdr);

    // Raw image data: each row prefixed with filter byte 0.
    let row_len = width as usize * 4;
    let mut raw = Vec::with_capacity((row_len + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * row_len..(y + 1) * row_len]);
    }

    let zlib = zlib_stored(&raw);
    write_chunk(&mut out, *b"IDAT", &zlib);
    write_chunk(&mut out, *b"IEND", &[]);
    out
}

fn write_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(&kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Wrap `data` in a zlib stream using uncompressed (stored) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x78); // CMF: deflate, 32K window
    out.push(0x01); // FLG
    // Deflate stored blocks, max 65535 bytes each.
    let mut i = 0;
    while i < data.len() {
        let chunk = (data.len() - i).min(0xFFFF);
        let last = i + chunk >= data.len();
        out.push(u8::from(last)); // BFINAL, BTYPE=00 (stored)
        // `chunk` is `.min(0xFFFF)`, so this conversion never truncates.
        let len = u16::try_from(chunk).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&data[i..i + chunk]);
        i += chunk;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
