//! Offscreen composite test (the spike's headless render-to-PNG approach).
//!
//! Builds a stand-in chrome frame (a dusk-colored window with a transparent
//! rectangular hole where the page viewport is) and a stand-in page frame (a
//! bright gradient), composites them, writes `out.png` next to the crate, and
//! asserts the page shows through the viewport region while the chrome
//! surrounds it. This proves chrome-surrounds-content (ADR-0003) with no
//! window — the same way `spikes/ui-wgpu` produced its `out.png`.

use mote_ui::{Compositor, PixelFormat, ViewportRect};

const W: u32 = 1000;
const H: u32 = 600;

// Viewport rect the chrome reports for the page (`<main data-slot>` geometry):
// a sidebar on the left and a top bar carve the page region inward.
const VP_X: u32 = 280;
const VP_Y: u32 = 76;
const VP_W: u32 = 660;
const VP_H: u32 = 480;

// The dusk chrome surface color (#1c1815 -> rgb 28,24,21).
const CHROME_R: u8 = 28;
const CHROME_G: u8 = 24;
const CHROME_B: u8 = 21;

/// Map a `u32` pixel coordinate to `f32` losslessly. All test dimensions fit
/// in `u16`, and `f32::from(u16)` is exact and lint-free.
fn fpx(v: u32) -> f32 {
    f32::from(u16::try_from(v).expect("test dimension fits in u16"))
}

/// Chrome stand-in: opaque dusk fill, fully transparent inside the viewport
/// rect so the page shows through (RGBA).
fn make_chrome() -> Vec<u8> {
    let mut buf = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let in_viewport = (VP_X..VP_X + VP_W).contains(&x) && (VP_Y..VP_Y + VP_H).contains(&y);
            if in_viewport {
                // transparent hole — the page is visible here.
                buf[i] = 0;
                buf[i + 1] = 0;
                buf[i + 2] = 0;
                buf[i + 3] = 0;
            } else {
                buf[i] = CHROME_R;
                buf[i + 1] = CHROME_G;
                buf[i + 2] = CHROME_B;
                buf[i + 3] = 255;
            }
        }
    }
    buf
}

/// Page stand-in: a bright green->blue gradient (RGBA), unmistakable against
/// the dark chrome.
fn make_page(w: u32, h: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let gx = u8::try_from(60 + (x * 180 / w.max(1))).unwrap_or(255);
            let gy = u8::try_from(60 + (y * 180 / h.max(1))).unwrap_or(255);
            buf[i] = 30;
            buf[i + 1] = gx;
            buf[i + 2] = gy;
            buf[i + 3] = 255;
        }
    }
    buf
}

fn pixel(rgba: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
    let i = ((y * W + x) * 4) as usize;
    (rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3])
}

#[test]
fn composites_page_into_viewport_with_chrome_around() {
    let Ok(mut compositor) = Compositor::new_offscreen(W, H) else {
        // No GPU/adapter available (e.g. some CI sandboxes). Skip rather than
        // fail — the on-surface and offscreen paths share one pipeline, and
        // the local run captures the evidence.
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let chrome = make_chrome();
    let page = make_page(VP_W, VP_H);

    compositor
        .update_chrome(&chrome, W, H, PixelFormat::Rgba8)
        .expect("chrome upload");
    compositor
        .update_page(
            &page,
            VP_W,
            VP_H,
            PixelFormat::Rgba8,
            ViewportRect::new(fpx(VP_X), fpx(VP_Y), fpx(VP_W), fpx(VP_H)),
        )
        .expect("page upload");

    let rgba = compositor.render_offscreen_rgba().expect("composite");
    assert_eq!(rgba.len(), (W * H * 4) as usize);

    // Write evidence PNG next to the crate.
    let png = compositor.render_offscreen_png().expect("png");
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/out.png");
    std::fs::write(path, &png).expect("write out.png");
    eprintln!("wrote {path} ({} bytes)", png.len());

    // --- Region assertions ---------------------------------------------------

    // 1. Non-blank: not every pixel is the clear color (black).
    let blank = rgba
        .chunks_exact(4)
        .all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0);
    assert!(!blank, "composite is entirely blank");

    // 2. Center of the viewport shows the PAGE (the green/blue gradient),
    //    not the chrome color and not black.
    let (pr, pg, pb, pa) = pixel(&rgba, VP_X + VP_W / 2, VP_Y + VP_H / 2);
    assert_eq!(pa, 255, "viewport center should be opaque");
    let is_chrome_color =
        pr.abs_diff(CHROME_R) < 8 && pg.abs_diff(CHROME_G) < 8 && pb.abs_diff(CHROME_B) < 8;
    assert!(
        !is_chrome_color,
        "viewport center shows chrome color {pr},{pg},{pb} — page not composited"
    );
    // The page gradient at center is distinctly green+blue dominant over red.
    assert!(
        pg > pr && pb > pr,
        "viewport center {pr},{pg},{pb} is not the page gradient"
    );

    // 3. Top-left corner (outside the viewport) shows the CHROME color.
    let (cr, cg, cb, ca) = pixel(&rgba, 20, 20);
    assert_eq!(ca, 255, "chrome region should be opaque");
    assert!(
        cr.abs_diff(CHROME_R) < 12 && cg.abs_diff(CHROME_G) < 12 && cb.abs_diff(CHROME_B) < 12,
        "top-left {cr},{cg},{cb} is not the chrome color (chrome not drawn)"
    );

    // 4. A point just outside the viewport edge (left of it, mid-height) is
    //    chrome, confirming the page is clipped to the viewport rect.
    let (lr, lg, lb, _) = pixel(&rgba, VP_X - 20, VP_Y + VP_H / 2);
    assert!(
        lr.abs_diff(CHROME_R) < 12 && lg.abs_diff(CHROME_G) < 12 && lb.abs_diff(CHROME_B) < 12,
        "pixel left of viewport {lr},{lg},{lb} is not chrome — page bled outside its rect"
    );
}

#[test]
fn tab_switch_is_a_texture_swap() {
    let Ok(mut compositor) = Compositor::new_offscreen(W, H) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    compositor
        .update_chrome(&make_chrome(), W, H, PixelFormat::Rgba8)
        .expect("chrome");

    let vp = ViewportRect::new(fpx(VP_X), fpx(VP_Y), fpx(VP_W), fpx(VP_H));

    // Tab A: a page that is red-dominant.
    let mut page_a = vec![0u8; (VP_W * VP_H * 4) as usize];
    for px in page_a.chunks_exact_mut(4) {
        px.copy_from_slice(&[220, 40, 40, 255]);
    }
    compositor
        .update_page(&page_a, VP_W, VP_H, PixelFormat::Rgba8, vp)
        .expect("page a");
    let a = compositor.render_offscreen_rgba().expect("composite a");
    let (ar, ..) = pixel(&a, VP_X + VP_W / 2, VP_Y + VP_H / 2);
    assert!(ar > 150, "tab A center should be red, got r={ar}");

    // Tab B: swap to a blue-dominant page (the cheap texture-swap path).
    let mut page_b = vec![0u8; (VP_W * VP_H * 4) as usize];
    for px in page_b.chunks_exact_mut(4) {
        px.copy_from_slice(&[40, 40, 220, 255]);
    }
    compositor
        .update_page(&page_b, VP_W, VP_H, PixelFormat::Rgba8, vp)
        .expect("page b");
    let b = compositor.render_offscreen_rgba().expect("composite b");
    let (_, _, bb, _) = pixel(&b, VP_X + VP_W / 2, VP_Y + VP_H / 2);
    assert!(
        bb > 150,
        "tab B center should be blue after swap, got b={bb}"
    );
}
