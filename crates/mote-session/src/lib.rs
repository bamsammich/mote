//! Identity, workspace, and session state for Mote.
//!
//! The three-axis state model separating three orthogonal concerns:
//!
//! - [`Identity`] — who the user is being (Chromium profile reference +
//!   metadata). Cookies, `localStorage`, history, and cache are isolated per
//!   identity.
//! - [`WorkspaceConfig`] — what the user is doing (dotfile-config-derived:
//!   name, icon, accent, default identity, default new-tab, pinned tabs).
//! - [`Session`] — what's currently open (runtime: open tabs with
//!   [`TabState`], scroll positions, back/forward history stacks,
//!   last-visited timestamps, hidden-tab metadata, form drafts).
//!
//! # Persistence
//!
//! Session state is persisted per identity through `mote-storage` with a
//! dedicated plugin namespace (`"mote-session"`). The continuous flush model
//! (batched ~5 s, driven by the shell) means crash recovery == clean exit —
//! a hard crash loses at most ~5 s of activity with no recovery prompt.
//!
//! The shell drives timing: call [`Session::flush`] on a ~5 s interval and
//! on clean shutdown; call [`Session::restore`] on launch. This crate exposes
//! the API; the shell owns the timer.
//!
//! # Restoration
//!
//! On launch, the active workspace's tabs are restored eagerly as placeholders
//! (title/favicon without a live renderer). Other workspaces are restored
//! lazily when the user switches to them. This keeps startup fast regardless
//! of total tab count.

mod discarder;
mod error;
mod form_draft;
mod identity;
mod reaper;
mod serde_helpers;
mod session;
mod tab;
mod workspace;

pub use discarder::{DiscardConfig, Discarder};
pub use error::SessionError;
pub use form_draft::{FormDraftConfig, FormDraftEntry, FormDraftStore};
pub use identity::Identity;
pub use reaper::{HiddenTabConfig, HiddenTabReaper};
pub use session::{RestorationMode, Session};
pub use tab::{HiddenTabMeta, Tab, TabHistory, TabState};
pub use workspace::{PinnedTab, WorkspaceConfig, WorkspaceState};
