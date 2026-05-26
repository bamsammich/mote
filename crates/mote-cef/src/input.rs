//! Input-event vocabulary and CEF event mapping for off-screen browsers.
//!
//! Mote's window layer (`mote-shell`, Wave B) hit-tests winit events and decides
//! *which* [`crate::Page`] an event targets and *where* (page-local coordinates).
//! This module defines the CEF-free input types it hands to [`crate::Page`]'s
//! `send_*` methods, plus the mapping from those types into the `cef::MouseEvent`
//! / `cef::KeyEvent` structs the browser host expects.
//!
//! The mapping functions are pure (`Mote type -> cef event struct`) so they can be
//! unit-tested without a live CEF process; the actual `host.send_*` FFI calls live
//! in [`crate::browser::Page`].

use cef::{KeyEvent, KeyEventType, MouseButtonType, MouseEvent};

/// Keyboard / mouse modifier flags, mirroring CEF's `cef_event_flags_t` bits that
/// Mote forwards. Combine with `|`. Coordinate-independent; applies to both mouse
/// and key events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers(u32);

impl Modifiers {
    /// No modifiers held.
    pub const NONE: Self = Self(0);
    // Values mirror cef_event_flags_t (stable C ABI constants).
    /// Caps Lock is on.
    pub const CAPS_LOCK: Self = Self(1 << 0);
    /// Shift held.
    pub const SHIFT: Self = Self(1 << 1);
    /// Control held.
    pub const CONTROL: Self = Self(1 << 2);
    /// Alt / Option held.
    pub const ALT: Self = Self(1 << 3);
    /// Left mouse button down.
    pub const LEFT_MOUSE_BUTTON: Self = Self(1 << 4);
    /// Middle mouse button down.
    pub const MIDDLE_MOUSE_BUTTON: Self = Self(1 << 5);
    /// Right mouse button down.
    pub const RIGHT_MOUSE_BUTTON: Self = Self(1 << 6);
    /// Command (macOS) / Meta / Super held.
    pub const COMMAND: Self = Self(1 << 7);

    /// The raw CEF event-flags bitmask.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Whether all bits of `other` are set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Left / primary button.
    Left,
    /// Middle / wheel button.
    Middle,
    /// Right / secondary button.
    Right,
}

/// Whether a button or key transition is a press or a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    /// The button/key went down.
    Down,
    /// The button/key came up.
    Up,
}

/// The kind of keyboard event being injected.
///
/// A full keystroke is typically `KeyDown` → `Char` → `KeyUp`: `KeyDown`/`KeyUp`
/// carry the virtual key code (for shortcuts / non-text keys), while `Char`
/// carries the resolved text character (what gets typed into a field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Key pressed (carries the virtual key code).
    Down,
    /// Key released.
    Up,
    /// A resolved character input (carries the typed character).
    Char,
}

/// A page-local mouse position.
///
/// Coordinates are already mapped into the target page's surface space by the
/// caller (the window→page hit-test is `mote-shell`'s job); `mote-cef` injects at
/// the given coordinates verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MousePosition {
    /// X in page-local pixels (0 = left edge of the page surface).
    pub x: i32,
    /// Y in page-local pixels (0 = top edge of the page surface).
    pub y: i32,
}

/// A keyboard event to inject. Build the virtual key code per platform upstream;
/// `mote-cef` forwards it unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput {
    /// Press / release / char.
    pub action: KeyAction,
    /// Windows virtual-key code (CEF's cross-platform `windows_key_code`). For a
    /// `Char` event this is ignored in favour of [`Self::character`].
    pub windows_key_code: i32,
    /// Native key code for the host platform, if known (0 if not).
    pub native_key_code: i32,
    /// The text character for a [`KeyAction::Char`] event, as a UTF-16 code unit
    /// (CEF takes a single `char16`). For non-`Char` events, set to 0.
    pub character: u16,
    /// Active modifiers.
    pub modifiers: Modifiers,
}

// ---------------------------------------------------------------------------
// Pure mappers: Mote input vocabulary -> cef event structs. Unit-tested below.
// ---------------------------------------------------------------------------

impl MouseButton {
    /// Map to CEF's button-type enum.
    pub(crate) const fn to_cef(self) -> MouseButtonType {
        match self {
            Self::Left => MouseButtonType::LEFT,
            Self::Middle => MouseButtonType::MIDDLE,
            Self::Right => MouseButtonType::RIGHT,
        }
    }
}

/// Build a `cef::MouseEvent` at `pos` with `modifiers`.
pub(crate) const fn mouse_event(pos: MousePosition, modifiers: Modifiers) -> MouseEvent {
    MouseEvent {
        x: pos.x,
        y: pos.y,
        modifiers: modifiers.bits(),
    }
}

/// Build a `cef::KeyEvent` from a Mote [`KeyInput`].
pub(crate) fn key_event(input: KeyInput) -> KeyEvent {
    let type_ = match input.action {
        KeyAction::Down => KeyEventType::KEYDOWN,
        KeyAction::Up => KeyEventType::KEYUP,
        KeyAction::Char => KeyEventType::CHAR,
    };
    KeyEvent {
        type_,
        modifiers: input.modifiers.bits(),
        windows_key_code: input.windows_key_code,
        native_key_code: input.native_key_code,
        // CEF's `character` field is a UTF-16 code unit (char16_t == u16).
        character: input.character,
        unmodified_character: input.character,
        focus_on_editable_field: 0,
        is_system_key: 0,
        ..Default::default()
    }
}

/// `mouse_up` flag CEF's `send_mouse_click_event` expects for a [`ButtonAction`].
pub(crate) const fn click_is_up(action: ButtonAction) -> i32 {
    match action {
        ButtonAction::Down => 0,
        ButtonAction::Up => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_combine_and_query() {
        let m = Modifiers::SHIFT | Modifiers::CONTROL;
        assert!(m.contains(Modifiers::SHIFT));
        assert!(m.contains(Modifiers::CONTROL));
        assert!(!m.contains(Modifiers::ALT));
        assert_eq!(m.bits(), (1 << 1) | (1 << 2));
        assert_eq!(Modifiers::NONE.bits(), 0);
    }

    #[test]
    fn modifiers_bitor_assign() {
        let mut m = Modifiers::SHIFT;
        m |= Modifiers::ALT;
        assert!(m.contains(Modifiers::SHIFT | Modifiers::ALT));
    }

    #[test]
    fn mouse_event_carries_coords_and_modifiers() {
        let e = mouse_event(MousePosition { x: 42, y: 99 }, Modifiers::LEFT_MOUSE_BUTTON);
        assert_eq!(e.x, 42);
        assert_eq!(e.y, 99);
        assert_eq!(e.modifiers, Modifiers::LEFT_MOUSE_BUTTON.bits());
    }

    #[test]
    fn mouse_button_maps_to_cef() {
        assert_eq!(
            MouseButton::Left.to_cef().get_raw(),
            MouseButtonType::LEFT.get_raw()
        );
        assert_eq!(
            MouseButton::Middle.to_cef().get_raw(),
            MouseButtonType::MIDDLE.get_raw()
        );
        assert_eq!(
            MouseButton::Right.to_cef().get_raw(),
            MouseButtonType::RIGHT.get_raw()
        );
    }

    #[test]
    fn click_up_flag() {
        assert_eq!(click_is_up(ButtonAction::Down), 0);
        assert_eq!(click_is_up(ButtonAction::Up), 1);
    }

    #[test]
    fn key_event_down_maps_keycode() {
        let e = key_event(KeyInput {
            action: KeyAction::Down,
            windows_key_code: 0x41, // 'A'
            native_key_code: 38,
            character: 0,
            modifiers: Modifiers::SHIFT,
        });
        assert_eq!(e.type_.get_raw(), KeyEventType::KEYDOWN.get_raw());
        assert_eq!(e.windows_key_code, 0x41);
        assert_eq!(e.native_key_code, 38);
        assert_eq!(e.modifiers, Modifiers::SHIFT.bits());
        assert_eq!(e.is_system_key, 0);
    }

    #[test]
    fn key_event_char_carries_character() {
        let e = key_event(KeyInput {
            action: KeyAction::Char,
            windows_key_code: 0,
            native_key_code: 0,
            character: u16::try_from('é' as u32).unwrap(),
            modifiers: Modifiers::NONE,
        });
        assert_eq!(e.type_.get_raw(), KeyEventType::CHAR.get_raw());
        assert_eq!(e.character, u16::try_from('é' as u32).unwrap());
        assert_eq!(e.unmodified_character, e.character);
    }

    #[test]
    fn key_event_up_maps_type() {
        let e = key_event(KeyInput {
            action: KeyAction::Up,
            windows_key_code: 0x41,
            native_key_code: 38,
            character: 0,
            modifiers: Modifiers::NONE,
        });
        assert_eq!(e.type_.get_raw(), KeyEventType::KEYUP.get_raw());
    }

    #[test]
    fn key_event_default_size_is_set() {
        // KeyEvent has a `size` field CEF validates; Default fills it.
        let e = key_event(KeyInput {
            action: KeyAction::Down,
            windows_key_code: 0,
            native_key_code: 0,
            character: 0,
            modifiers: Modifiers::NONE,
        });
        assert!(e.size > 0, "KeyEvent.size must be populated for the C ABI");
    }
}
