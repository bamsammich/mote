//! The BLAKE3 content checksum, rendered as `blake3:<hex>`.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// The algorithm prefix every rendered [`Checksum`] carries.
const PREFIX: &str = "blake3:";

/// Number of hex characters in a 32-byte BLAKE3 digest.
const HEX_LEN: usize = blake3::OUT_LEN * 2;

/// A BLAKE3 content checksum.
///
/// Mote uses this for **integrity** verification (the file matches what the
/// user approved), not trust (DESIGN §Integrity verification). It is rendered
/// and parsed as `blake3:<64 lowercase hex chars>`.
///
/// The digest is BLAKE3, per the dependency stack and integrity-verification
/// spec — **not** sha256, despite a stale `sha256:` string in one DESIGN
/// manifest example.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Checksum([u8; blake3::OUT_LEN]);

impl Checksum {
    /// Computes the BLAKE3 checksum of `bytes`.
    #[must_use]
    pub fn hash(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Returns the raw 32-byte digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; blake3::OUT_LEN] {
        &self.0
    }

    /// Renders the digest as 64 lowercase hex characters, without the prefix.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(HEX_LEN);
        for byte in self.0 {
            // Two lowercase hex digits per byte; `write!` to a String is infallible.
            use fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", self.to_hex())
    }
}

impl fmt::Debug for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Checksum({self})")
    }
}

/// Error returned when a string is not a valid `blake3:<hex>` [`Checksum`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ChecksumParseError {
    /// The string did not start with the `blake3:` prefix.
    #[error("checksum must start with the {PREFIX:?} prefix")]
    MissingPrefix,
    /// The hex body was not exactly 64 characters.
    #[error("checksum hex body must be {HEX_LEN} characters, got {0}")]
    WrongLength(usize),
    /// The hex body contained a non-hex-digit character.
    #[error("checksum hex body contains a non-hex character")]
    InvalidHex,
}

impl FromStr for Checksum {
    type Err = ChecksumParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix(PREFIX)
            .ok_or(ChecksumParseError::MissingPrefix)?;
        if hex.len() != HEX_LEN {
            return Err(ChecksumParseError::WrongLength(hex.len()));
        }

        let mut bytes = [0u8; blake3::OUT_LEN];
        for (i, byte) in bytes.iter_mut().enumerate() {
            let pair = &hex[i * 2..i * 2 + 2];
            *byte = u8::from_str_radix(pair, 16).map_err(|_| ChecksumParseError::InvalidHex)?;
        }
        Ok(Self(bytes))
    }
}
