//! Mote design tokens (dusk dark theme) transcribed from spec/03_tokens.md.
//! This is the "token vocabulary" a Lua theme would set; here it's Rust consts.

pub type Rgba = [f32; 4];

/// Parse `#RRGGBB` -> linear-ish sRGB stored straight. We render into an
/// Rgba8UnormSrgb target, so we hand the GPU sRGB values directly.
const fn nib(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

pub const fn hex(s: &str) -> Rgba {
    let b = s.as_bytes();
    let r = nib(b[1]) * 16 + nib(b[2]);
    let g = nib(b[3]) * 16 + nib(b[4]);
    let bl = nib(b[5]) * 16 + nib(b[6]);
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        bl as f32 / 255.0,
        1.0,
    ]
}

pub const fn with_a(c: Rgba, a: f32) -> Rgba {
    [c[0], c[1], c[2], a]
}

// --- semantic colors (dusk) ---
pub const BG: Rgba = hex("#14110F"); // ink-800
pub const SURFACE_1: Rgba = hex("#1C1815"); // ink-700
pub const SURFACE_2: Rgba = hex("#241F1B"); // ink-600
pub const SURFACE_SUNK: Rgba = hex("#0E0C0A"); // ink-900
pub const BORDER: Rgba = hex("#2E2823"); // ink-500
pub const BORDER_STRONG: Rgba = hex("#3A332D"); // ink-400
pub const BORDER_SUBTLE: Rgba = hex("#241F1B"); // ink-600
pub const FG: Rgba = hex("#ECE5D8");
pub const FG_1: Rgba = hex("#C9C0B0");
pub const FG_2: Rgba = hex("#8A8278"); // ink-200
pub const FG_3: Rgba = hex("#5C544B"); // ink-300

pub const ACCENT: Rgba = hex("#E0A458"); // amber
pub const ACCENT_DEEP: Rgba = hex("#B47C36"); // amber-deep
pub const ACCENT_ON: Rgba = hex("#0E0C0A"); // ink-900
pub const SUCCESS: Rgba = hex("#6B8E4E"); // moss
pub const SPECIAL: Rgba = hex("#8E6FA0"); // plum
pub const TRANSPARENT: Rgba = [0.0, 0.0, 0.0, 0.0];

// --- spacing (4px grid) ---
pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;

// --- radius ---
pub const RADIUS_1: f32 = 2.0; // buttons, fields, tabs, chips
pub const RADIUS_2: f32 = 4.0; // cards
pub const RADIUS_DOT: f32 = 9999.0;

// --- layout (Mote-specific) ---
pub const CHROME_TABBAR: f32 = 40.0;
pub const CHROME_OMNIBOX: f32 = 36.0;
pub const SIDEBAR_W: f32 = 280.0;

// --- type sizes (px) from spec/04_typography.md ---
pub const TEXT_BODY: f32 = 14.0;
pub const TEXT_SMALL: f32 = 12.0;
pub const TEXT_MICRO: f32 = 11.0;
pub const TEXT_MONO: f32 = 13.0;
pub const TEXT_MONO_SM: f32 = 11.0;
pub const TEXT_H3: f32 = 18.0;
