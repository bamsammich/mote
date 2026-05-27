//! Workspace tab picker — the `Mod+Space` fuzzy-finder over a workspace's tabs.
//!
//! DESIGN §The workspace tab picker: a first-class navigation primitive. The
//! picker lists every tab in the current workspace — active in any window plus
//! hidden in workspace — ranked by [`mote_session::Session::tab_picker_ranked`],
//! fuzzy-filtered as the user types. Selecting an **active** tab switches to it
//! (the shell's existing switch path); a **hidden** tab is **revealed**
//! (materialized into the window).
//!
//! ## Where the logic lives
//!
//! The picker is a **Rust-side state machine** ([`PickerState`]); the overlay
//! page is pure display. This is a deliberate consequence of the bridge model:
//! the host-bridge op router (`window.cefQuery`) is attached **only** to the
//! chrome page (`mote-cef` §bridge), so a separate overlay [`mote_cef::Page`]
//! cannot invoke ops back to the shell. So instead of round-tripping selection
//! through an op, the shell:
//!
//! 1. captures keystrokes in its winit loop (it already intercepts keybinds),
//! 2. routes them into [`PickerState`] while the picker is open,
//! 3. re-renders the overlay's list via `Page::eval_js` after each change,
//! 4. executes the selection directly in Rust (`select_tab` / reveal).
//!
//! The overlay HTML reuses mote-ui's `palette.css` (the command-palette surface)
//! and is composited full-window on the chrome texture exactly like the
//! integrity overlay (`Ctrl+Shift+I`).

use std::fmt::Write as _;

use mote_session::{Tab, TabState};
use mote_types::TabId;

/// The picker overlay document, served as `mote://overlay/picker.html`.
///
/// Pure display: it reuses mote-ui's `palette.css` (the command-palette surface)
/// over `tokens.css` + `base.css`, and exposes a single `window.__motePicker`
/// render function the shell drives via `Page::eval_js`. There is no op router
/// on this page (the bridge attaches it to the chrome page only); all key input
/// and selection are handled Rust-side by [`PickerState`].
///
/// The backdrop is full-window and opaque-dark because the overlay is composited
/// over the chrome texture (the live page is not visible behind it, same as the
/// integrity overlay) — so the palette reads as floating over a dimmed browser.
pub(crate) const PICKER_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<link rel="stylesheet" href="tokens.css" />
<link rel="stylesheet" href="base.css" />
<link rel="stylesheet" href="components/palette.css" />
<style>
  /* The overlay composites onto the chrome texture over the page (same as the
     integrity overlay), so the surface must be OPAQUE to fully cover the live
     page beneath. base.css forces `body`/`.mote-root` transparent (the chrome
     page lets the web view show through its slot grid) — this overlay is NOT
     the chrome slot grid, so we override with an opaque fill at high enough
     specificity to beat base.css's `.mote-root` rule. */
  html, body { height: 100%; margin: 0; }
  html, body.picker-root { background: #0e0c0a; }
  .palette-backdrop { background: transparent; }
  .palette-row .cat.is-active .dot { color: var(--success); }
  .palette-row .url { color: var(--fg-3); margin-left: 8px; min-width: 0;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
</head>
<body class="picker-root" data-theme="dusk">
  <div class="palette-backdrop" role="dialog" aria-modal="true" aria-label="workspace tab picker">
    <div class="palette">
      <div class="palette-input">
        <span class="prompt">&rsaquo;_</span>
        <input id="q" placeholder="switch to tab&hellip;" readonly />
        <span class="count" id="count"></span>
      </div>
      <div class="palette-list" id="list" role="listbox"></div>
    </div>
  </div>
<script>
(function () {
  "use strict";
  var list = document.getElementById("list");
  var q = document.getElementById("q");
  var count = document.getElementById("count");
  // The shell pushes {query, rows:[{cat,title,url,sel,active}]} here on every
  // keystroke. Rows are built with text nodes — never innerHTML of the
  // page-derived title/url strings.
  window.__motePicker = function (state) {
    q.value = state.query || "";
    var rows = Array.isArray(state.rows) ? state.rows : [];
    count.textContent = rows.length + "";
    list.textContent = "";
    if (rows.length === 0) {
      var empty = document.createElement("div");
      empty.className = "palette-empty";
      empty.textContent = "no tabs match";
      list.appendChild(empty);
      return;
    }
    rows.forEach(function (r) {
      var row = document.createElement("div");
      row.className = r.sel ? "palette-row is-sel" : "palette-row";
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", r.sel ? "true" : "false");

      var cat = document.createElement("span");
      cat.className = r.active ? "cat is-active" : "cat";
      cat.textContent = r.cat || "";
      row.appendChild(cat);

      var name = document.createElement("span");
      name.className = "name";
      name.textContent = r.title || r.url || "untitled";
      row.appendChild(name);

      var url = document.createElement("span");
      url.className = "url";
      url.textContent = r.url || "";
      row.appendChild(url);

      list.appendChild(row);
      if (r.sel) {
        row.scrollIntoView({ block: "nearest" });
      }
    });
  };
})();
</script>
</body>
</html>"#;

/// The category of a picker row, derived from the tab's session state.
///
/// Selecting an [`Active`](EntryKind::Active)/[`Idle`](EntryKind::Idle) tab
/// switches to it; selecting a [`Hidden`](EntryKind::Hidden)/[`Held`](EntryKind::Held)
/// tab reveals it. Modeled as one enum (rather than several bools) so the row
/// state has a single source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    /// Active tab with a live renderer.
    Active,
    /// Active tab whose renderer was discarded for idleness (reloads on focus).
    Idle,
    /// Hidden tab (renderer destroyed; retrievable via the picker).
    Hidden,
    /// Hidden tab with the hold flag set (exempt from TTL aging).
    Held,
}

impl EntryKind {
    /// Whether selecting this row switches to (vs reveals) the tab.
    const fn is_active(self) -> bool {
        matches!(self, Self::Active | Self::Idle)
    }

    /// The short category label shown on the left of a picker row.
    const fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Hidden => "hidden",
            Self::Held => "held",
        }
    }
}

/// A single picker row: a snapshot of one tab taken when the picker opens.
///
/// Decoupled from the live [`Tab`] so the picker holds no borrow on the session
/// while it is open (the shell mutates the session on selection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PickerEntry {
    /// The tab's stable id (used to resolve the selection back to a session tab).
    pub(crate) id: TabId,
    /// Display title (falls back to the URL when the page has no title yet).
    pub(crate) title: String,
    /// The tab's URL (shown dimmed; also fuzzy-matched against).
    pub(crate) url: String,
    /// The row category (active / idle / hidden / held).
    pub(crate) kind: EntryKind,
    /// Pinned tab (shown with a marker; ranked near the top by the session).
    pub(crate) is_pinned: bool,
}

impl PickerEntry {
    /// Builds a picker entry from a session tab snapshot.
    pub(crate) fn from_tab(tab: &Tab) -> Self {
        let title = tab
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| tab.url.clone());
        let kind = match tab.state {
            TabState::Active if tab.is_discarded => EntryKind::Idle,
            TabState::Active => EntryKind::Active,
            TabState::Hidden if tab.hidden_meta.as_ref().is_some_and(|m| m.hold) => EntryKind::Held,
            // Hidden-without-hold, and Closed (which the session never ranks).
            TabState::Hidden | TabState::Closed => EntryKind::Hidden,
        };
        Self {
            id: tab.id,
            title,
            url: tab.url.clone(),
            kind,
            is_pinned: tab.is_pinned,
        }
    }

    /// Whether selecting this entry switches to (vs reveals) the tab.
    pub(crate) const fn is_active(&self) -> bool {
        self.kind.is_active()
    }
}

/// The picker's open/filter/selection state.
///
/// `base` is the workspace's tabs in [`tab_picker_ranked`] order, captured at
/// open time. `query` filters them; `selected` indexes the **filtered** view.
///
/// [`tab_picker_ranked`]: mote_session::Session::tab_picker_ranked
#[derive(Debug, Default)]
pub(crate) struct PickerState {
    /// Whether the picker is currently open.
    pub(crate) open: bool,
    /// Whether the overlay page's `__motePicker` render hook is known to exist
    /// yet (it appears only after the page's first paint; an `eval_js` before
    /// then is lost). The shell flips this on the page's first paint and pushes
    /// the initial state — the same warm-up the chrome bridge page needs.
    pub(crate) ready: bool,
    /// The fuzzy query the user has typed.
    query: String,
    /// The base ranked entries (recency/active/pinned order from the session).
    base: Vec<PickerEntry>,
    /// Index into the current filtered view of the selected row.
    selected: usize,
}

impl PickerState {
    /// Opens the picker over `entries` (already in session ranking order).
    pub(crate) fn open(&mut self, entries: Vec<PickerEntry>) {
        self.open = true;
        self.ready = false;
        self.query.clear();
        self.base = entries;
        self.selected = 0;
    }

    /// Closes the picker, dropping the captured entries.
    pub(crate) fn close(&mut self) {
        self.open = false;
        self.ready = false;
        self.query.clear();
        self.base.clear();
        self.selected = 0;
    }

    /// Appends a typed character to the query, clamping the selection.
    pub(crate) fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.clamp_selection();
    }

    /// Removes the last query character (Backspace).
    pub(crate) fn backspace(&mut self) {
        self.query.pop();
        self.clamp_selection();
    }

    /// Moves the selection down one row (wrapping at the end).
    pub(crate) fn move_down(&mut self) {
        let n = self.filtered().len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected + 1) % n;
    }

    /// Moves the selection up one row (wrapping at the start).
    pub(crate) fn move_up(&mut self) {
        let n = self.filtered().len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected + n - 1) % n;
    }

    /// The currently-selected entry, or `None` if the filtered view is empty.
    pub(crate) fn selected_entry(&self) -> Option<&PickerEntry> {
        let filtered = self.filtered();
        filtered.get(self.selected).map(|m| m.entry)
    }

    /// The query string (for rendering the input).
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// The fuzzy-filtered + re-ranked rows for the current query.
    ///
    /// With an empty query this is the base ranking unchanged. With a query,
    /// only matching rows survive; they are ordered by fuzzy score (higher
    /// first), with the **base rank** breaking ties so the session's
    /// active-first/recency ordering still shows through equal-scored rows
    /// (DESIGN: "fuzzy match score weighted by recency").
    fn filtered(&self) -> Vec<Match<'_>> {
        if self.query.is_empty() {
            return self
                .base
                .iter()
                .enumerate()
                .map(|(rank, entry)| Match {
                    entry,
                    score: 0,
                    rank,
                })
                .collect();
        }
        let mut matches: Vec<Match<'_>> = self
            .base
            .iter()
            .enumerate()
            .filter_map(|(rank, entry)| {
                // Match against title first, then URL; keep the better score.
                let title_score = fuzzy_score(&entry.title, &self.query);
                let url_score = fuzzy_score(&entry.url, &self.query);
                let score = title_score.max(url_score);
                score.map(|score| Match { entry, score, rank })
            })
            .collect();
        // Higher score first; ties broken by the base rank (lower = higher).
        matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.rank.cmp(&b.rank)));
        matches
    }

    /// Clamp the selection into the current filtered range.
    fn clamp_selection(&mut self) {
        let n = self.filtered().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// Render the filtered rows to a JSON array the overlay's `renderPicker`
    /// consumes: `[{cat,title,url,sel,active}, …]`. Strings are JS-escaped by
    /// the caller's `js_string`; here we build a data structure, never markup.
    pub(crate) fn rows_json(&self, js_string: impl Fn(&str) -> String) -> String {
        let filtered = self.filtered();
        let mut out = String::from("[");
        for (i, m) in filtered.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"cat\":{},\"title\":{},\"url\":{},\"sel\":{},\"active\":{}}}",
                js_string(m.entry.kind.label()),
                js_string(&m.entry.title),
                js_string(&m.entry.url),
                i == self.selected,
                m.entry.is_active(),
            );
        }
        out.push(']');
        out
    }
}

/// A scored match in the filtered view, retaining the base rank for tiebreaks.
struct Match<'a> {
    entry: &'a PickerEntry,
    score: i32,
    rank: usize,
}

/// A subsequence fuzzy score: returns `None` if `query` is not a (case-insensitive)
/// subsequence of `haystack`, else a positive score that rewards
/// **consecutive** matches and matches at word boundaries / the very start.
///
/// This is intentionally small and dependency-free; it is the same family of
/// scorer as fzf's, sufficient for ranking a workspace's tabs. An empty query
/// scores 0 (everything matches) — callers special-case the empty query.
fn fuzzy_score(haystack: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().flat_map(char::to_lowercase).collect();
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();

    let mut score = 0i32;
    let mut hi = 0usize;
    let mut prev_match: Option<usize> = None;
    for &nc in &needle {
        // Advance the haystack cursor to the next occurrence of `nc`.
        let mut found = None;
        while hi < hay.len() {
            if hay[hi] == nc {
                found = Some(hi);
                hi += 1;
                break;
            }
            hi += 1;
        }
        let pos = found?; // not a subsequence → no match
        score += 1; // base point per matched char
        if pos == 0 {
            score += 5; // matches at the very start are strong
        } else if !hay[pos - 1].is_alphanumeric() {
            score += 3; // word-boundary match
        }
        if prev_match == Some(pos.wrapping_sub(1)) {
            score += 4; // consecutive run
        }
        prev_match = Some(pos);
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use mote_types::{TabId, WorkspaceId};

    use super::*;

    fn entry(id: u64, title: &str, url: &str, active: bool) -> PickerEntry {
        PickerEntry {
            id: TabId::new(id),
            title: title.to_owned(),
            url: url.to_owned(),
            kind: if active {
                EntryKind::Active
            } else {
                EntryKind::Hidden
            },
            is_pinned: false,
        }
    }

    // ── Fuzzy scorer ──────────────────────────────────────────────────────

    #[test]
    fn fuzzy_non_subsequence_is_none() {
        assert!(fuzzy_score("github", "xyz").is_none());
        assert!(fuzzy_score("github", "ghz").is_none());
    }

    #[test]
    fn fuzzy_subsequence_matches() {
        assert!(fuzzy_score("github.com", "ghb").is_some());
        assert!(fuzzy_score("GitHub", "git").is_some());
    }

    #[test]
    fn fuzzy_consecutive_outscores_scattered() {
        let consec = fuzzy_score("github", "git").unwrap();
        let scattered = fuzzy_score("graphite", "git").unwrap();
        assert!(
            consec > scattered,
            "consecutive {consec} should beat scattered {scattered}"
        );
    }

    #[test]
    fn fuzzy_start_match_bonus() {
        let at_start = fuzzy_score("rust-lang", "rust").unwrap();
        let mid = fuzzy_score("trust-rust", "rust").unwrap();
        assert!(at_start > mid, "start {at_start} should beat mid {mid}");
    }

    // ── Filter + ranking integration ──────────────────────────────────────

    #[test]
    fn empty_query_preserves_base_ranking() {
        let mut p = PickerState::default();
        p.open(vec![
            entry(1, "Active", "https://a.com", true),
            entry(2, "Hidden", "https://b.com", false),
        ]);
        let titles: Vec<_> = p.filtered().iter().map(|m| m.entry.title.clone()).collect();
        assert_eq!(titles, vec!["Active", "Hidden"]);
    }

    #[test]
    fn query_filters_to_matches_only() {
        let mut p = PickerState::default();
        p.open(vec![
            entry(1, "GitHub", "https://github.com", true),
            entry(2, "GitLab", "https://gitlab.com", false),
            entry(3, "Reddit", "https://reddit.com", false),
        ]);
        for c in "git".chars() {
            p.push_char(c);
        }
        let ids: Vec<u64> = p.filtered().iter().map(|m| m.entry.id.get()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert!(!ids.contains(&3));
    }

    #[test]
    fn query_matches_url_not_just_title() {
        let mut p = PickerState::default();
        p.open(vec![entry(1, "Untitled", "https://rust-lang.org", true)]);
        for c in "rust".chars() {
            p.push_char(c);
        }
        assert_eq!(p.filtered().len(), 1);
    }

    #[test]
    fn ranking_breaks_score_ties_by_base_rank() {
        // Two equally-good matches: the one earlier in the base ranking (the
        // active tab the session ranked first) wins the tiebreak.
        let mut p = PickerState::default();
        p.open(vec![
            entry(1, "docs", "https://a.com", true),  // base rank 0
            entry(2, "docs", "https://b.com", false), // base rank 1
        ]);
        for c in "docs".chars() {
            p.push_char(c);
        }
        let first = p.filtered();
        assert_eq!(first[0].entry.id.get(), 1);
    }

    #[test]
    fn selection_moves_and_wraps() {
        let mut p = PickerState::default();
        p.open(vec![
            entry(1, "a", "https://a.com", true),
            entry(2, "b", "https://b.com", true),
        ]);
        assert_eq!(p.selected_entry().unwrap().id.get(), 1);
        p.move_down();
        assert_eq!(p.selected_entry().unwrap().id.get(), 2);
        p.move_down(); // wraps
        assert_eq!(p.selected_entry().unwrap().id.get(), 1);
        p.move_up(); // wraps back to end
        assert_eq!(p.selected_entry().unwrap().id.get(), 2);
    }

    #[test]
    fn selection_clamps_when_filter_shrinks() {
        let mut p = PickerState::default();
        p.open(vec![
            entry(1, "alpha", "https://a.com", true),
            entry(2, "beta", "https://b.com", true),
        ]);
        p.move_down(); // select index 1 (beta)
        assert_eq!(p.selected_entry().unwrap().id.get(), 2);
        // Typing "alp" narrows to alpha only; selection must clamp to 0.
        for c in "alp".chars() {
            p.push_char(c);
        }
        assert_eq!(p.filtered().len(), 1);
        assert_eq!(p.selected_entry().unwrap().id.get(), 1);
    }

    #[test]
    fn backspace_widens_filter() {
        let mut p = PickerState::default();
        p.open(vec![
            entry(1, "alpha", "https://a.com", true),
            entry(2, "beta", "https://b.com", true),
        ]);
        for c in "alp".chars() {
            p.push_char(c);
        }
        assert_eq!(p.filtered().len(), 1);
        p.backspace();
        p.backspace();
        p.backspace();
        assert_eq!(p.query(), "");
        assert_eq!(p.filtered().len(), 2);
    }

    #[test]
    fn close_resets_state() {
        let mut p = PickerState::default();
        p.open(vec![entry(1, "a", "https://a.com", true)]);
        for c in "a".chars() {
            p.push_char(c);
        }
        p.close();
        assert!(!p.open);
        assert_eq!(p.query(), "");
        assert!(p.selected_entry().is_none());
    }

    #[test]
    fn no_match_yields_empty_and_no_selection() {
        let mut p = PickerState::default();
        p.open(vec![entry(1, "github", "https://github.com", true)]);
        for c in "zzz".chars() {
            p.push_char(c);
        }
        assert_eq!(p.filtered().len(), 0);
        assert!(p.selected_entry().is_none());
    }

    #[test]
    fn rows_json_marks_selection_and_active() {
        let mut p = PickerState::default();
        p.open(vec![
            entry(1, "Alpha", "https://a.com", true),
            entry(2, "Beta", "https://b.com", false),
        ]);
        let json = p.rows_json(|s| format!("\"{s}\""));
        assert!(json.contains("\"title\":\"Alpha\""));
        assert!(json.contains("\"sel\":true"));
        assert!(json.contains("\"active\":false")); // the hidden tab
        assert!(json.contains("\"cat\":\"active\""));
        assert!(json.contains("\"cat\":\"hidden\""));
    }

    #[test]
    fn from_tab_classifies_states() {
        use std::time::SystemTime;
        let mut active = Tab::new(TabId::new(1), "https://a.com".into(), WorkspaceId::new(1));
        active.title = Some("A".into());
        let a = PickerEntry::from_tab(&active);
        assert_eq!(a.kind, EntryKind::Active);
        assert!(a.is_active());
        assert_eq!(a.title, "A");

        let mut hidden = Tab::new(TabId::new(2), "https://b.com".into(), WorkspaceId::new(1));
        hidden.hide(SystemTime::now());
        hidden.set_hold(true);
        let h = PickerEntry::from_tab(&hidden);
        assert!(!h.is_active());
        assert_eq!(h.kind, EntryKind::Held);
        // No title → falls back to URL.
        assert_eq!(h.title, "https://b.com");
    }
}
