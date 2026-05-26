//! Off-screen paint frames surfaced to Rust callers.
//!
//! CEF's `on_paint` hands us a CPU pixel buffer in **BGRA** byte order (the
//! Linux v0.1 path; accelerated shared-texture OSR is a future risk noted in
//! docs/research/ui-spike-cef-html.md §1). These types carry that buffer across
//! the wrapper boundary without exposing any `cef::` type.

/// The byte order of a [`PaintFrame`] buffer.
///
/// CEF's CPU `on_paint` path delivers `BGRA`. The variant is carried explicitly
/// so a future accelerated path (or a swizzle done elsewhere) can describe its
/// own layout rather than silently assuming BGRA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PixelFormat {
    /// Blue, green, red, alpha — one byte each. CEF's CPU `on_paint` default.
    Bgra8,
    /// Red, green, blue, alpha — one byte each.
    Rgba8,
}

/// A single painted off-screen frame: dimensions plus a tightly-packed pixel
/// buffer (`width * height * 4` bytes, no row padding — CEF packs rows).
///
/// Ownership is transferred to the caller (the buffer is a `Vec<u8>` copy of the
/// CEF-owned memory, which is only valid for the duration of the `on_paint`
/// callback). This makes the frame safe to hold, send, and composite after the
/// callback returns.
#[derive(Debug, Clone)]
pub struct PaintFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel byte order of [`Self::pixels`].
    pub format: PixelFormat,
    /// Tightly-packed pixels, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

impl PaintFrame {
    /// Returns `true` if the frame carries no pixels (zero-sized or empty).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pixels.is_empty() || self.width == 0 || self.height == 0
    }

    /// Returns a copy of the frame with pixels converted to [`PixelFormat::Rgba8`].
    ///
    /// A no-op clone when already RGBA. Useful for callers (e.g. PNG encoders)
    /// that expect RGBA byte order.
    #[must_use]
    pub fn to_rgba8(&self) -> Self {
        match self.format {
            PixelFormat::Rgba8 => self.clone(),
            PixelFormat::Bgra8 => {
                let mut pixels = self.pixels.clone();
                for px in pixels.chunks_exact_mut(4) {
                    px.swap(0, 2); // B<->R
                }
                Self {
                    width: self.width,
                    height: self.height,
                    format: PixelFormat::Rgba8,
                    pixels,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_to_rgba_swaps_blue_and_red() {
        let frame = PaintFrame {
            width: 1,
            height: 1,
            format: PixelFormat::Bgra8,
            // B=1 G=2 R=3 A=4
            pixels: vec![1, 2, 3, 4],
        };
        let rgba = frame.to_rgba8();
        assert_eq!(rgba.format, PixelFormat::Rgba8);
        // R=3 G=2 B=1 A=4
        assert_eq!(rgba.pixels, vec![3, 2, 1, 4]);
    }

    #[test]
    fn rgba_to_rgba_is_noop() {
        let frame = PaintFrame {
            width: 1,
            height: 1,
            format: PixelFormat::Rgba8,
            pixels: vec![9, 8, 7, 6],
        };
        assert_eq!(frame.to_rgba8().pixels, vec![9, 8, 7, 6]);
    }

    #[test]
    fn empty_detection() {
        let f = PaintFrame {
            width: 0,
            height: 0,
            format: PixelFormat::Bgra8,
            pixels: Vec::new(),
        };
        assert!(f.is_empty());
    }
}
