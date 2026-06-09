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

/// Fill a `w*h` RGBA buffer with a single solid color.
fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&rgba);
    }
    buf
}

/// BUG #1 regression: after a window grow, `resize` + `resize_page_viewport`
/// must stretch the retained page texture to the NEW viewport so there is no
/// stale band (clear/black pixels) along the grown right/bottom edge until CEF
/// re-paints.
#[test]
fn resize_page_viewport_stretches_retained_frame_no_stale_band() {
    // Start small, then grow. The page is a distinct solid color so "page vs
    // clear(black)" is unambiguous.
    const SMALL_W: u32 = 400;
    const SMALL_H: u32 = 300;
    const NEW_W: u32 = 1000;
    const NEW_H: u32 = 700;

    // Initial viewport: page fills the whole small window (no chrome inset, so
    // any black pixel after the grow is a stale band, not an inset).
    const INIT_VP_W: u32 = SMALL_W;
    const INIT_VP_H: u32 = SMALL_H;

    // Vivid magenta page — clearly not the BLACK clear color.
    const PAGE: [u8; 4] = [220, 30, 200, 255];

    let Ok(mut compositor) = Compositor::new_offscreen(SMALL_W, SMALL_H) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    // Upload a page texture at the initial (small) viewport. No chrome layer:
    // the page is the only thing drawn, so the only non-page pixels can be the
    // clear color.
    compositor
        .update_page(
            &solid(INIT_VP_W, INIT_VP_H, PAGE),
            INIT_VP_W,
            INIT_VP_H,
            PixelFormat::Rgba8,
            ViewportRect::new(0.0, 0.0, fpx(INIT_VP_W), fpx(INIT_VP_H)),
        )
        .expect("page upload");

    // Grow the window. This is the shell's `handle_resize` path: resize the
    // target, then stretch the retained page layer to the new viewport.
    compositor.resize(NEW_W, NEW_H);
    compositor.resize_page_viewport(ViewportRect::new(0.0, 0.0, fpx(NEW_W), fpx(NEW_H)));

    let rgba = compositor.render_offscreen_rgba().expect("composite");
    assert_eq!(rgba.len(), (NEW_W * NEW_H * 4) as usize);

    // pixel() indexes by W (the original const); after a grow the row stride is
    // NEW_W, so read directly here.
    let at = |x: u32, y: u32| -> (u8, u8, u8, u8) {
        let i = ((y * NEW_W + x) * 4) as usize;
        (rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3])
    };
    let is_page = |(r, g, b, a): (u8, u8, u8, u8)| {
        a == 255 && r.abs_diff(PAGE[0]) < 24 && g.abs_diff(PAGE[1]) < 24 && b.abs_diff(PAGE[2]) < 24
    };

    // The new right edge (x = NEW_W - 1) and bottom edge (y = NEW_H - 1) — the
    // region that was BEYOND the old viewport — must now be the page color, not
    // the black clear (the stale band the bug left behind).
    let right_mid = at(NEW_W - 1, NEW_H / 2);
    assert!(
        is_page(right_mid),
        "right edge {right_mid:?} is not the page color — stale band after grow"
    );
    let bottom_mid = at(NEW_W / 2, NEW_H - 1);
    assert!(
        is_page(bottom_mid),
        "bottom edge {bottom_mid:?} is not the page color — stale band after grow"
    );
    let far_corner = at(NEW_W - 1, NEW_H - 1);
    assert!(
        is_page(far_corner),
        "far corner {far_corner:?} is not the page color — stale band after grow"
    );
}

/// BUG #2 regression: activating a not-yet-painted tab must clear the retained
/// (closed-tab) page texture so its stale pixels are not shown — the viewport
/// shows the clear color until the new page paints.
#[test]
fn clear_page_drops_stale_texture_shows_clear_color() {
    const PAGE: [u8; 4] = [40, 220, 80, 255]; // vivid green prior-tab page.

    let Ok(mut compositor) = Compositor::new_offscreen(W, H) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let vp = ViewportRect::new(fpx(VP_X), fpx(VP_Y), fpx(VP_W), fpx(VP_H));
    compositor
        .update_page(&solid(VP_W, VP_H, PAGE), VP_W, VP_H, PixelFormat::Rgba8, vp)
        .expect("prior page upload");

    // Sanity: the prior page is visible at the viewport center.
    assert!(compositor.has_page(), "page should be set before clear");
    let before = compositor
        .render_offscreen_rgba()
        .expect("composite before");
    let (br, bg, bb, _) = pixel(&before, VP_X + VP_W / 2, VP_Y + VP_H / 2);
    assert!(
        bg > 150 && br < 120 && bb < 150,
        "prior page center {br},{bg},{bb} is not the green page"
    );

    // Activation of an unpainted tab: drop the closed tab's texture.
    compositor.clear_page();
    assert!(
        !compositor.has_page(),
        "page should be None after clear_page"
    );

    // With no chrome and no page, the whole target is the BLACK clear color —
    // specifically the viewport center must NOT show the prior green page.
    let after = compositor.render_offscreen_rgba().expect("composite after");
    let (ar, ag, ab, _) = pixel(&after, VP_X + VP_W / 2, VP_Y + VP_H / 2);
    assert!(
        ar == 0 && ag == 0 && ab == 0,
        "viewport center {ar},{ag},{ab} after clear_page is not the clear color — stale texture shown"
    );
}
