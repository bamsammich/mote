//! Mote egui UI spike — offscreen renderer.
//!
//! Renders a 1280x800 dark-themed browser-chrome mock to `out.png` and
//! writes timing/memory metrics to stdout. No window is opened.
//!
//! THROWAWAY code — lint compliance is intentionally skipped.

#![allow(clippy::all, warnings, unused)]

use std::fs;
use std::io::Read;
use std::sync::Arc;
use std::time::Instant;

use egui::{
    Color32, Context, FontDefinitions, FontFamily, FontId, Margin, Rect, Rounding,
    Stroke, Style, Vec2, ViewportId, Visuals,
};
use egui::epaint::{CornerRadius, StrokeKind};
use egui_wgpu::{Renderer, ScreenDescriptor};
use image::{ImageBuffer, Rgba};
use wgpu::util::DeviceExt;

// ─── Design tokens (dusk dark theme) ────────────────────────────────────────

const BG: Color32 = Color32::from_rgb(0x14, 0x11, 0x0F);
const SURFACE_1: Color32 = Color32::from_rgb(0x1C, 0x18, 0x15);
const SURFACE_2: Color32 = Color32::from_rgb(0x24, 0x1F, 0x1B);
const SURFACE_SUNK: Color32 = Color32::from_rgb(0x0E, 0x0C, 0x0A);
const BORDER: Color32 = Color32::from_rgb(0x2E, 0x28, 0x23);
const BORDER_STRONG: Color32 = Color32::from_rgb(0x3A, 0x33, 0x2D);
const FG: Color32 = Color32::from_rgb(0xEC, 0xE5, 0xD8);
const FG_1: Color32 = Color32::from_rgb(0xC9, 0xC0, 0xB0);
const FG_2: Color32 = Color32::from_rgb(0x8A, 0x82, 0x78);
const FG_3: Color32 = Color32::from_rgb(0x5C, 0x54, 0x4B);
const ACCENT: Color32 = Color32::from_rgb(0xE0, 0xA4, 0x58);
const ACCENT_DEEP: Color32 = Color32::from_rgb(0xB4, 0x7C, 0x36);
const SUCCESS: Color32 = Color32::from_rgb(0x6B, 0x8E, 0x4E);

const W: u32 = 1280;
const H: u32 = 800;
const TABBAR_H: f32 = 40.0;
const OMNI_H: f32 = 36.0;
const SIDEBAR_W: f32 = 316.0; // 36 activity bar + 280 panel
const ACTIVITY_W: f32 = 36.0;
const RADIUS_1: f32 = 2.0;
const RADIUS_2: f32 = 4.0;
const SPACE_2: f32 = 8.0;
const SPACE_3: f32 = 12.0;
const SPACE_4: f32 = 16.0;

// ─── Tab strip ───────────────────────────────────────────────────────────────

fn draw_tabbar(painter: &egui::Painter, full_rect: Rect) {
    // Background
    painter.rect_filled(full_rect, 0.0, SURFACE_1);
    painter.line_segment(
        [full_rect.left_bottom(), full_rect.right_bottom()],
        Stroke::new(1.0, BORDER),
    );

    let tab_w = 200.0_f32;
    let tabs = [
        ("github.com/bamsammich/mote", true),
        ("motesh.dev — themes", false),
        ("build #482 — running", false),
    ];

    let mut x = full_rect.left();
    for (title, active) in tabs {
        let tab_rect = Rect::from_min_size(
            egui::pos2(x, full_rect.top()),
            Vec2::new(tab_w, full_rect.height()),
        );

        let bg = if active { BG } else { SURFACE_1 };
        painter.rect_filled(tab_rect, 0.0, bg);

        if active {
            painter.line_segment(
                [tab_rect.left_top(), tab_rect.right_top()],
                Stroke::new(2.0, ACCENT),
            );
        }

        painter.line_segment(
            [tab_rect.right_top(), tab_rect.right_bottom()],
            Stroke::new(1.0, BORDER),
        );

        let dot_x = tab_rect.left() + 12.0;
        let dot_y = tab_rect.center().y;
        let dot_color = if active { ACCENT } else { FG_2 };
        painter.circle_filled(egui::pos2(dot_x, dot_y), 3.0, dot_color);

        let fg = if active { FG } else { FG_2 };
        painter.text(
            egui::pos2(dot_x + 10.0, dot_y),
            egui::Align2::LEFT_CENTER,
            title,
            FontId::new(11.0, FontFamily::Monospace),
            fg,
        );

        if active {
            painter.text(
                egui::pos2(tab_rect.right() - 10.0, dot_y),
                egui::Align2::CENTER_CENTER,
                "×",
                FontId::new(13.0, FontFamily::Proportional),
                FG_2,
            );
        }

        x += tab_w;
    }

    // "+" new-tab button
    let plus_rect = Rect::from_min_size(
        egui::pos2(x, full_rect.top()),
        Vec2::new(36.0, full_rect.height()),
    );
    painter.rect_filled(plus_rect, 0.0, SURFACE_1);
    painter.text(
        plus_rect.center(),
        egui::Align2::CENTER_CENTER,
        "+",
        FontId::new(14.0, FontFamily::Proportional),
        FG_2,
    );
}

// ─── Omnibox row ─────────────────────────────────────────────────────────────

fn draw_omnibar(painter: &egui::Painter, full_rect: Rect) {
    painter.rect_filled(full_rect, 0.0, BG);
    painter.line_segment(
        [full_rect.left_bottom(), full_rect.right_bottom()],
        Stroke::new(1.0, BORDER),
    );

    let content_x = SIDEBAR_W;
    let content_w = full_rect.width() - content_x;
    let omni_w = content_w * 0.62;
    let omni_x = content_x + (content_w - omni_w) / 2.0;
    let omni_y = full_rect.top() + (OMNI_H - 26.0) / 2.0;
    let omni_rect = Rect::from_min_size(egui::pos2(omni_x, omni_y), Vec2::new(omni_w, 26.0));

    painter.rect_filled(omni_rect, RADIUS_1, SURFACE_SUNK);
    painter.rect_stroke(omni_rect.shrink(0.5), RADIUS_1, Stroke::new(1.0, BORDER), StrokeKind::Outside);

    // Mode tag  [url]
    let mode_w = 48.0;
    let mode_rect = Rect::from_min_size(omni_rect.min, Vec2::new(mode_w, omni_rect.height()));
    painter.rect_filled(mode_rect, RADIUS_1, SURFACE_1);
    painter.line_segment(
        [mode_rect.right_top(), mode_rect.right_bottom()],
        Stroke::new(1.0, BORDER),
    );
    painter.text(
        egui::pos2(mode_rect.left() + 5.0, mode_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "[",
        FontId::new(12.0, FontFamily::Monospace),
        ACCENT,
    );
    painter.text(
        egui::pos2(mode_rect.left() + 12.0, mode_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "url",
        FontId::new(12.0, FontFamily::Monospace),
        FG,
    );
    painter.text(
        egui::pos2(mode_rect.right() - 8.0, mode_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "]",
        FontId::new(12.0, FontFamily::Monospace),
        ACCENT,
    );

    // URL content
    let url_x = omni_rect.left() + mode_w + 8.0;
    let center_y = omni_rect.center().y;
    painter.text(
        egui::pos2(url_x, center_y),
        egui::Align2::LEFT_CENTER,
        "github.com/",
        FontId::new(12.0, FontFamily::Monospace),
        FG_2,
    );
    painter.text(
        egui::pos2(url_x + 74.0, center_y),
        egui::Align2::LEFT_CENTER,
        "bamsammich",
        FontId::new(12.0, FontFamily::Monospace),
        FG,
    );
    painter.text(
        egui::pos2(url_x + 74.0 + 76.0, center_y),
        egui::Align2::LEFT_CENTER,
        "/mote",
        FontId::new(12.0, FontFamily::Monospace),
        FG_2,
    );

    // Right icons
    painter.text(
        egui::pos2(omni_rect.right() - 22.0, center_y),
        egui::Align2::CENTER_CENTER,
        "★",
        FontId::new(12.0, FontFamily::Proportional),
        FG_2,
    );
    painter.text(
        egui::pos2(omni_rect.right() - 8.0, center_y),
        egui::Align2::CENTER_CENTER,
        "▣",
        FontId::new(12.0, FontFamily::Proportional),
        FG_2,
    );
}

// ─── Sidebar ─────────────────────────────────────────────────────────────────

fn draw_sidebar(painter: &egui::Painter, full_rect: Rect) {
    painter.rect_filled(full_rect, 0.0, SURFACE_1);
    painter.line_segment(
        [full_rect.right_top(), full_rect.right_bottom()],
        Stroke::new(1.0, BORDER),
    );

    // Activity bar (leftmost 36px)
    let act_rect = Rect::from_min_size(full_rect.min, Vec2::new(ACTIVITY_W, full_rect.height()));
    painter.rect_filled(act_rect, 0.0, BG);
    painter.line_segment(
        [act_rect.right_top(), act_rect.right_bottom()],
        Stroke::new(1.0, BORDER),
    );

    // Activity icons (unicode stand-ins for Lucide icons)
    let icons = ["⊞", "☰", "⊙", "✦", "⊡", "{}"];
    for (i, icon) in icons.iter().enumerate() {
        let icon_y = act_rect.top() + 8.0 + i as f32 * 32.0;
        let is_active = i == 4; // integrity panel active
        let color = if is_active { ACCENT } else { FG_2 };
        if is_active {
            painter.rect_filled(
                Rect::from_min_size(
                    egui::pos2(act_rect.left(), icon_y + 2.0),
                    Vec2::new(2.0, 24.0),
                ),
                0.0,
                ACCENT,
            );
        }
        painter.text(
            egui::pos2(act_rect.center().x, icon_y + 14.0),
            egui::Align2::CENTER_CENTER,
            *icon,
            FontId::new(13.0, FontFamily::Proportional),
            color,
        );
    }

    // Panel area
    let panel_rect = Rect::from_min_size(
        egui::pos2(full_rect.left() + ACTIVITY_W, full_rect.top()),
        Vec2::new(full_rect.width() - ACTIVITY_W, full_rect.height()),
    );

    // Panel header
    let hdr_rect = Rect::from_min_size(panel_rect.min, Vec2::new(panel_rect.width(), 30.0));
    painter.rect_filled(hdr_rect, 0.0, SURFACE_1);
    painter.line_segment(
        [hdr_rect.left_bottom(), hdr_rect.right_bottom()],
        Stroke::new(1.0, BORDER),
    );

    // [browser integrity]
    let lbl_x = hdr_rect.left() + SPACE_3;
    let lbl_y = hdr_rect.center().y;
    painter.text(egui::pos2(lbl_x, lbl_y), egui::Align2::LEFT_CENTER, "[", FontId::new(11.0, FontFamily::Monospace), ACCENT);
    painter.text(egui::pos2(lbl_x + 7.0, lbl_y), egui::Align2::LEFT_CENTER, "browser integrity", FontId::new(11.0, FontFamily::Monospace), FG);
    painter.text(egui::pos2(lbl_x + 7.0 + 110.0, lbl_y), egui::Align2::LEFT_CENTER, "]", FontId::new(11.0, FontFamily::Monospace), ACCENT);

    // Plugin card
    let card_margin = SPACE_3;
    let card_rect = Rect::from_min_size(
        egui::pos2(panel_rect.left() + card_margin, hdr_rect.bottom() + card_margin),
        Vec2::new(panel_rect.width() - card_margin * 2.0, 140.0),
    );
    painter.rect_filled(card_rect, RADIUS_2, SURFACE_1);
    painter.rect_stroke(card_rect.shrink(0.5), RADIUS_2, Stroke::new(1.0, BORDER), StrokeKind::Outside);

    let cy = card_rect.top() + SPACE_4;
    let cx = card_rect.left() + SPACE_3;

    // Plugin name
    painter.text(
        egui::pos2(cx, cy),
        egui::Align2::LEFT_TOP,
        "password-manager-1password",
        FontId::new(11.0, FontFamily::Monospace),
        FG,
    );

    // Version
    painter.text(
        egui::pos2(cx, cy + 15.0),
        egui::Align2::LEFT_TOP,
        "v1.0.0",
        FontId::new(10.0, FontFamily::Monospace),
        FG_2,
    );

    // verified badge
    let badge_x = cx + 46.0;
    let badge_y = cy + 15.0;
    let badge_rect = Rect::from_min_size(egui::pos2(badge_x, badge_y), Vec2::new(62.0, 16.0));
    painter.rect_filled(badge_rect, RADIUS_1, SURFACE_1);
    painter.rect_stroke(
        badge_rect.shrink(0.5),
        RADIUS_1,
        Stroke::new(1.0, Color32::from_rgba_premultiplied(107, 142, 78, 77)),
        StrokeKind::Outside,
    );
    painter.circle_filled(egui::pos2(badge_rect.left() + 8.0, badge_rect.center().y), 3.0, SUCCESS);
    painter.text(
        egui::pos2(badge_rect.left() + 15.0, badge_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "verified",
        FontId::new(10.0, FontFamily::Monospace),
        SUCCESS,
    );

    // Permission lines
    let perms = [
        "http:fetch:https://*.1password.com/*",
        "storage:persistent",
        "page:inject_script:*",
    ];
    for (i, perm) in perms.iter().enumerate() {
        painter.text(
            egui::pos2(cx, cy + 34.0 + i as f32 * 15.0),
            egui::Align2::LEFT_TOP,
            *perm,
            FontId::new(10.0, FontFamily::Monospace),
            FG_3,
        );
    }

    // Action buttons
    let btn_y = card_rect.bottom() - 30.0;
    let btn_labels = ["revoke", "update"];
    let mut bx = cx;
    for lbl in &btn_labels {
        let bw = lbl.len() as f32 * 6.5 + 14.0;
        let btn_rect = Rect::from_min_size(egui::pos2(bx, btn_y), Vec2::new(bw, 22.0));
        painter.rect_filled(btn_rect, RADIUS_1, SURFACE_1);
        painter.rect_stroke(btn_rect.shrink(0.5), RADIUS_1, Stroke::new(1.0, BORDER_STRONG), StrokeKind::Outside);
        // Keycap bottom
        painter.line_segment(
            [btn_rect.left_bottom(), btn_rect.right_bottom()],
            Stroke::new(1.5, BORDER_STRONG),
        );
        painter.text(
            btn_rect.center(),
            egui::Align2::CENTER_CENTER,
            *lbl,
            FontId::new(11.0, FontFamily::Proportional),
            FG_1,
        );
        bx += bw + SPACE_2;
    }
}

// ─── Content area with composited page texture ────────────────────────────────

fn draw_content(painter: &egui::Painter, content_rect: Rect, page_texture_id: egui::TextureId) {
    painter.rect_filled(content_rect, 0.0, BG);
    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(page_texture_id, content_rect, uv, Color32::WHITE);
}

// ─── Style matching Mote dusk tokens ─────────────────────────────────────────

fn cr(r: f32) -> CornerRadius {
    CornerRadius::same(r as u8)
}

fn build_style() -> Style {
    let mut style = Style::default();
    let mut visuals = Visuals::dark();

    visuals.window_fill = SURFACE_1;
    visuals.panel_fill = BG;
    visuals.extreme_bg_color = SURFACE_SUNK;
    visuals.faint_bg_color = SURFACE_2;
    visuals.code_bg_color = SURFACE_SUNK;

    visuals.widgets.noninteractive.bg_fill = SURFACE_1;
    visuals.widgets.noninteractive.weak_bg_fill = SURFACE_1;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, FG_1);
    visuals.widgets.noninteractive.corner_radius = cr(RADIUS_1);

    visuals.widgets.inactive.bg_fill = SURFACE_1;
    visuals.widgets.inactive.weak_bg_fill = SURFACE_1;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, FG_2);
    visuals.widgets.inactive.corner_radius = cr(RADIUS_1);

    visuals.widgets.hovered.bg_fill = SURFACE_2;
    visuals.widgets.hovered.weak_bg_fill = SURFACE_2;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, FG);
    visuals.widgets.hovered.corner_radius = cr(RADIUS_1);

    visuals.widgets.active.bg_fill = SURFACE_2;
    visuals.widgets.active.weak_bg_fill = SURFACE_2;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, FG);
    visuals.widgets.active.corner_radius = cr(RADIUS_1);

    visuals.selection.bg_fill = Color32::from_rgba_premultiplied(224, 164, 88, 60);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    visuals.window_corner_radius = cr(RADIUS_2);
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.override_text_color = Some(FG);
    visuals.hyperlink_color = ACCENT;

    style.visuals = visuals;
    style.spacing.item_spacing = Vec2::new(4.0, 4.0);
    style.spacing.window_margin = Margin::same(0);
    style.spacing.button_padding = Vec2::new(8.0, 4.0);

    style
}

// ─── Procedural page texture ──────────────────────────────────────────────────

fn create_page_texture_data(w: u32, h: u32) -> Vec<u8> {
    let mut data = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let t = y as f32 / h as f32;
            let r = (20.0_f32 + 8.0_f32 * t) as u8;   // 0x14 → 0x1C
            let g = (17.0_f32 + 7.0_f32 * t) as u8;   // 0x11 → 0x18
            let b = (15.0_f32 + 6.0_f32 * t) as u8;   // 0x0F → 0x15
            let idx = ((y * w + x) * 4) as usize;
            data[idx] = r;
            data[idx + 1] = g;
            data[idx + 2] = b;
            data[idx + 3] = 255;
        }
    }
    // Grid lines to make "rendered page" obvious
    for y in 0..h {
        for x in 0..w {
            if x % 40 == 0 || y % 40 == 0 {
                let idx = ((y * w + x) * 4) as usize;
                data[idx] = data[idx].saturating_add(10);
                data[idx + 1] = data[idx + 1].saturating_add(8);
                data[idx + 2] = data[idx + 2].saturating_add(6);
            }
        }
    }
    data
}

// ─── Memory measurement ───────────────────────────────────────────────────────

fn read_rss_kb() -> u64 {
    let mut buf = String::new();
    if let Ok(mut f) = fs::File::open("/proc/self/status") {
        let _ = f.read_to_string(&mut buf);
    }
    for line in buf.lines() {
        if line.starts_with("VmRSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(v) = parts.get(1) {
                return v.parse().unwrap_or(0);
            }
        }
    }
    0
}

// ─── Core render helper ───────────────────────────────────────────────────────

fn one_frame(
    ctx: &Context,
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    render_view: &wgpu::TextureView,
    screen: &ScreenDescriptor,
    page_texture_id: egui::TextureId,
) {
    let mut raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            Vec2::new(W as f32, H as f32),
        )),
        ..Default::default()
    };
    // Set pixels_per_point via viewport info
    raw.viewports
        .entry(ViewportId::ROOT)
        .and_modify(|v| v.native_pixels_per_point = Some(1.0))
        .or_insert_with(|| {
            let mut vi = egui::ViewportInfo::default();
            vi.native_pixels_per_point = Some(1.0);
            vi
        });

    let full_output = ctx.run(raw, |ctx| {
        // Background fill
        egui::Area::new(egui::Id::new("bg"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Background)
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(W as f32, H as f32)),
                    0.0,
                    BG,
                );
            });

        // Tab bar
        egui::Area::new(egui::Id::new("tabbar"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(W as f32, TABBAR_H));
                draw_tabbar(ui.painter(), rect);
            });

        // Omnibar
        egui::Area::new(egui::Id::new("omnibar"))
            .fixed_pos(egui::pos2(0.0, TABBAR_H))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let rect = Rect::from_min_size(egui::pos2(0.0, TABBAR_H), Vec2::new(W as f32, OMNI_H));
                draw_omnibar(ui.painter(), rect);
            });

        // Sidebar
        let chrome_h = TABBAR_H + OMNI_H;
        egui::Area::new(egui::Id::new("sidebar"))
            .fixed_pos(egui::pos2(0.0, chrome_h))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let rect = Rect::from_min_size(
                    egui::pos2(0.0, chrome_h),
                    Vec2::new(SIDEBAR_W, H as f32 - chrome_h),
                );
                draw_sidebar(ui.painter(), rect);
            });

        // Content area with composited page texture
        egui::Area::new(egui::Id::new("content"))
            .fixed_pos(egui::pos2(SIDEBAR_W, chrome_h))
            .order(egui::Order::Background)
            .show(ctx, |ui| {
                let rect = Rect::from_min_size(
                    egui::pos2(SIDEBAR_W, chrome_h),
                    Vec2::new(W as f32 - SIDEBAR_W, H as f32 - chrome_h),
                );
                draw_content(ui.painter(), rect, page_texture_id);
            });
    });

    // Apply font/texture deltas from egui
    for (id, delta) in &full_output.textures_delta.set {
        renderer.update_texture(device, queue, *id, delta);
    }
    for id in &full_output.textures_delta.free {
        renderer.free_texture(id);
    }

    let primitives = ctx.tessellate(full_output.shapes, 1.0);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("egui"),
    });

    renderer.update_buffers(device, queue, &mut encoder, &primitives, screen);

    let mut pass = encoder
        .begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: render_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0x14 as f64 / 255.0,
                        g: 0x11 as f64 / 255.0,
                        b: 0x0F as f64 / 255.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        })
        .forget_lifetime();

    renderer.render(&mut pass, &primitives, screen);
    drop(pass); // end the render pass before finishing encoder

    queue.submit(std::iter::once(encoder.finish()));
    device.poll(wgpu::Maintain::Wait);
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    pollster::block_on(run());
}

async fn run() {
    // ── wgpu device setup ────────────────────────────────────────────────────
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::None,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .expect("no wgpu adapter");

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("mote-egui-spike"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: Default::default(),
            },
            None,
        )
        .await
        .expect("device creation failed");

    let format = wgpu::TextureFormat::Rgba8Unorm;

    // ── Offscreen render target ───────────────────────────────────────────────
    let render_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("egui_offscreen"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let render_view = render_texture.create_view(&Default::default());

    // ── egui context & renderer ───────────────────────────────────────────────
    let ctx = Context::default();
    ctx.set_style(build_style());

    let mut renderer = Renderer::new(&device, format, None, 1, false);

    let screen = ScreenDescriptor {
        size_in_pixels: [W, H],
        pixels_per_point: 1.0,
    };

    // ── Upload procedural page texture ────────────────────────────────────────
    let page_w = (W as f32 - SIDEBAR_W) as u32;
    let page_h = (H as f32 - TABBAR_H - OMNI_H) as u32;
    let page_data = create_page_texture_data(page_w, page_h);

    let page_wgpu_tex = device.create_texture_with_data(
        &queue,
        &wgpu::TextureDescriptor {
            label: Some("page_tex"),
            size: wgpu::Extent3d { width: page_w, height: page_h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &page_data,
    );
    let page_view = page_wgpu_tex.create_view(&Default::default());
    let page_texture_id = renderer.register_native_texture(&device, &page_view, wgpu::FilterMode::Linear);

    // ── Warm-up pass (font atlas build, shader compile) ───────────────────────
    one_frame(&ctx, &mut renderer, &device, &queue, &render_view, &screen, page_texture_id);

    // ── Time 100 offscreen renders ────────────────────────────────────────────
    let rss_before = read_rss_kb();
    let t0 = Instant::now();
    const N: u32 = 100;
    for _ in 0..N {
        one_frame(&ctx, &mut renderer, &device, &queue, &render_view, &screen, page_texture_id);
    }
    let elapsed = t0.elapsed();
    let avg_ms = elapsed.as_secs_f64() * 1000.0 / N as f64;
    let rss_after = read_rss_kb();

    // ── Read back PNG ─────────────────────────────────────────────────────────
    let bytes_per_pixel = 4u32;
    let unpadded = W * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = (unpadded + align - 1) / align * align;

    let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        render_texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback_buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback_buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().expect("buffer map failed");

    let mapped = slice.get_mapped_range();
    let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(W, H);
    for row in 0..H {
        let src = &mapped[(row * padded) as usize..(row * padded + unpadded) as usize];
        for col in 0..W {
            let base = (col * 4) as usize;
            img.put_pixel(col, row, Rgba([src[base], src[base + 1], src[base + 2], src[base + 3]]));
        }
    }
    drop(mapped);
    readback_buf.unmap();

    let out_path = "out.png";
    img.save(out_path).expect("failed to save PNG");

    // ── Metrics ───────────────────────────────────────────────────────────────
    println!("=== egui spike metrics ===");
    println!("Output: {out_path}  ({W}x{H})");
    println!("Avg frame time ({N} frames): {avg_ms:.3} ms");
    println!("VmRSS before: {} kB", rss_before);
    println!("VmRSS after:  {} kB", rss_after);
    println!("Delta RSS:    {} kB", rss_after.saturating_sub(rss_before));
}
