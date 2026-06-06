# Mote — UX Elevation Survey

**Date:** 2026-06-04 · **Scope:** the polish-phase + Phase-5a user-facing surface (omnibox, status line, find, context menu, tabs/sidebar, new-tab page, workspaces, settings, security/integrity UI, bookmarks/history plugins).

**Goal:** a full, deduplicated, non-overlapping list of opportunities to elevate Mote's browser UX to *contend with mainstream browsers* (Chrome/Firefox/Safari/Arc) on polish and table-stakes — **without losing the original design vision or any differentiator**. This is a review backlog for prioritization, not an implementation plan. Nothing here is committed work.

---

## How this was produced

Nine read-only component analysts each audited one disjoint surface (code + the live running instance + the committed P1–P6 screenshots), with explicit out-of-scope boundaries so findings don't overlap. Each was briefed with Mote's vision/differentiators and the `mote-design` hard rules and told to flag any idea that has *tension* with the vision rather than silently propose a Chrome clone. The orchestrator then deduplicated across agents, merged parallel findings, and verified the highest-impact defect claims directly against the code (marked ✓ below).

**Reading the entries.** Each item keeps its source ID (e.g. `A2`, `H1`) for traceability. Tags: **type** (defect = current bug/rough edge; gap = missing table-stakes; enh = elevation idea), **pri** (P0 must-fix/table-stakes → P2 nice-to-have), **effort** (S/M/L). `★` = amplifies a Mote-unique differentiator. `⚠ TENSION` = conflicts with the vision and needs an explicit decision. `✓` = orchestrator-verified against code.

---

## The through-line (executive summary)

The chrome is **visually mature and on-brand** — the bracket lockups, keycap construction, vertical tabs, dusk/vellum theming, and structured-DOM (no-innerHTML) discipline are all well-executed. The gap is almost everywhere the same shape: **polished surfaces wired to placeholder data or half-finished pipelines.** Find-in-page renders a `[find]` bar but discards the query before it reaches CEF. The status line shows a vim-style mode chip frozen at `NORMAL`. The security popover and the integrity panel — *the* differentiator — display literal `(placeholder)` / `(details pending)` copy and hardcode every audit row to "allowed." The settings panel accepts input and writes nothing. The search box can't search.

This is good news for prioritization: **closing the placeholder→real-data gap is the single highest-leverage theme, and it doubly serves the transparency differentiator.** The same work that makes Mote feel finished also makes "the browser that shows you what others hide" actually true. The biggest risk to the vision is the opposite — shipping surfaces that *assert* things they haven't verified ("certificate: verified" from the scheme alone; "last 24h" over a ring buffer that isn't time-windowed). For a product whose pitch is honesty, a confidently-wrong placeholder is worse than an honest "not yet available."

**Counts:** ~100 deduplicated findings · ~26 P0 (mostly genuine defects) · the differentiator surfaces (security/transparency, config-as-truth, keyboard-first, plugin-extensibility) account for most of the highest-value `★` items.

---

## P0 — defects & table-stakes, split by roadmap readiness

The lens: **was the enabling engine for this feature supposed to exist yet?** If the backend belongs to a later phase (or isn't built), the polished-but-dead UI is *premature scaffolding* and the placeholder was inevitable. If the infra already exists and the feature is table-stakes for the shell we've already shipped (Phase 2/5), it's a *legitimate feature shipped buggy*. (✓ = orchestrator-verified in code.)

### A. Premature — chrome shipped ahead of its engine

UI surfaces for capabilities whose backend is scheduled later (or undecided). The placeholder/dead state was *inevitable*. **Right response: descope/hide the affordance until its phase, or consciously pull that engine forward — do not polish the placeholder.** Tracked, not implemented in this pass.

- **B1 — vim mode chip frozen at `NORMAL`.** Presupposes modal editing; **vim-mode is Phase 6**, so there is no mode to report. → hide the chip until vim-mode. `statusline.rs:229`.
- **A2 — `[cmd]` omnibox mode dispatch.** Presupposes a command-execution engine (rides with vim-mode, **Phase 6**); submit just navigates (`host.js:441`). → suppress the `[cmd]` tag until dispatch exists.
- **G10 / G4 — settings *writes* + live-state read.** Depend on the config-mutation API (code comment at `lib.rs:1762`: "wired once the config-mutation API is complete") and the unresolved config-truth doctrine. A write-capable GUI ahead of its backend. → make settings a read-only config *mirror* first. (The read-only keybinds section is appropriately staged.)
- **G1 / G3 — settings viewport-fill + toggle a11y.** Frontend bugs that only live inside the premature write panel; defer until a read-only surface is legitimately in scope.
- **H2 — security popover cert/TLS/cookies depth.** Needs CEF SSL-status/cookie callbacks (unwired); ships literal `(placeholder)` / `verified (details pending)`. The lock indicator is fine; the rich content is ahead of its data source. ⚠ asserting "verified" from the scheme alone violates the no-deception principle. → slim to honestly-available data (scheme + per-site plugin permissions). ★
- **H1 (network-request portion) — blocked counts + "browser's own outbound" row.** Depend on net interception (**adblock, Phase 6**); correctly future. → label honestly, do not fake. ★

### B. Legitimate shell table-stakes — fix now

Shell features for the browser already shipped (Phase 2/5). The enabling infrastructure exists today; each defect is a wiring/quality bug. **These are the implementation targets** — checked off as landed (`[x]` = implemented + tested + verified).

**Status (2026-06-04): all 14 landed on `main`** — `64b3b22` find · `144743f` context-menu · `c0b7197` omnibox · `71faecb` workspace · `49715d9` audit. 1067 workspace tests green (incl. new black-box tests per defect); clean chrome boot + **E1** (newtab omnibox blanking) live-verified via screenshot; the remaining items are unit-verified (GUI input injection is unavailable on this host, and the bundled plugins didn't unpack in the throwaway scratch profile). Bucket A remains intentionally untouched.

**I1 follow-up (2026-06-05):** the omnibox search resolver was subsequently refined from "any dot → navigate, always https" to a researched, public-suffix-based heuristic matching Chrome/Firefox (navigate known-TLD hosts + IPs + localhost; search dotless/unknown-suffix words like `node.js`; https-default with a loopback→http exception). Recorded in **ADR-0018** (`b15d6ce`) and implemented in `28881c2` (adds the `psl` crate).

- [x] **✓ C2 — find-in-page never searches.** `find_in_page` op does `let _ = text;` (`mote-shell/src/lib.rs:1437`); CEF's `Page::find` is never called with the query. Find mode is cosmetic.
- [x] **✓ C3 — Enter in find does nothing.** `find_next`/`find_prev` ops aren't registered; only the shell-side Ctrl+G keybind works.
- [x] **C4 — match count never populates.** `OnFindResult` is unwired, so the styled "N / M" counter is always blank.
- [x] **C1 — `[find]` placeholder reads "enter a url."** The shared input's placeholder/aria-label aren't updated on mode entry.
- [x] **D1 — no editable-field context menu.** CEF's `is_editable`/`edit_state_flags` are never extracted; cut/copy/paste/select-all missing in inputs/textareas.
- [x] **D10 — context-menu back/forward never appear.** `can_go_back`/`can_go_forward` are hardcoded `false` in the CEF callback, never patched from live nav state.
- [x] **E1 — new-tab exposes its internal URL.** The omnibox shows `mote://chrome/newtab.html` instead of empty + focused (`isNewtabUrl()` helper exists, unused in `set_url`).
- [x] **✓ I1 — the omnibox can't search.** `toUrl` prepends `https://` to every bare term (`host.js:58/60`); the configured engine's `url_template` is never read at navigate time; context-menu search hardcodes Google (`host.js:2041`).
- [x] **I3 — no cross-source suggestion dedup.** A URL both visited and bookmarked appears twice.
- [x] **I5 — bookmark match case-sensitive, history case-insensitive.** `google` matches the visit but not the `Google` bookmark.
- [x] **F1 / F10 — workspaces hardcoded in two places.** Lua `BUILTIN_WORKSPACES` + a shadow Rust `workspace_id_for_slug()` table desync; index-switch silently fails for any 3rd workspace.
- [x] **F3 — keybind collision.** `⌘⇧W` is advertised for *both* "switch workspace" and "close window."
- [x] **A1 — nav tooltips read "(available in p2)"** in static HTML (verify: likely overwritten at boot by `wireNavButtons`).
- [x] **H1 (now-part) — audit log hardcodes "allowed."** The integrity panel (Phase 2) has real Allow/Deny data for capability calls; reflect it instead of hardcoding `AuditDecision::Allowed` (network-request rows stay future per A).

---

## High-leverage clusters (one fix → many surfaces)

These group findings that share a root cause. Fixing the root lands every listed surface at once — prioritize the cluster, not the individual items.

| Cluster | Root cause | Lands these | Net |
|---|---|---|---|
| **CL-LOADING** | CEF `is_loading`/`OnLoadingStateChange` never surfaced to the shell | tab `···` ticker (`E2`), status-line "loading 64%" (`B7`), reload↔stop glyph (`A12`) | a complete, spinner-free load-state story |
| **CL-SEARCH** | configured search engine never consumed; everything coerced to a URL | omnibox search/navigate detection (`I1`), "search with \<engine\>" row (`I2`), de-hardcode Google in context menu (`D3`), engine sourced from Lua config | the omnibox actually searches, config-driven ★ |
| **CL-KBNAV** | the omnibox dropdown's roving-focus pattern isn't shared | context menu (`D2`), workspace popover (`F4`), integrity panel (`H13`) all mouse-only | extract one shared helper → keyboard-first everywhere ★ |
| **CL-KEYMAP** | keybind table is incomplete/contended | `⌘K` focus (`A5`), Ctrl+Shift+Tab prev-tab (`E9`), tab-by-index vs workspace-index collision (`E4`/`F10`), `⌘⇧W` collision (`F3`), `⌘T` from content focus (backlog) | a coherent, documented, conflict-free keymap |
| **CL-XPARENCY-DATA** | flagship transparency surfaces render fixtures, not live data | real audit decisions (`H1`), real popover cert/cookies/permissions (`H2`), explain/block actions (`H3`), per-plugin call timeline (`H4`), live settings-integrity table (`H14`+`G11`), checksum/verify-time detail (`H16`), plugins-section provenance (`G7`) | makes "the browser that shows you what others hide" literally true ★★ |
| **CL-CONFIG-TRUTH** | settings GUI has no honest relationship to the Lua config stack | real managed.lua writes (`G10`), live state read (`G4`), per-row config provenance badge (`G5`), per-capability revoke (`G6`) | the settings panel becomes a config *mirror*, not a shadow store ★ |
| **CL-URL-XPARENCY** | the URL/trackers are shown raw with no signal or emphasis | domain-emphasis formatting (`A8`), surface+copy-clean tracking params (`A9`), hover-url strip/truncate (`B3`) | honest, readable URLs that *show* trackers instead of hiding or ignoring them ★ |
| **CL-MARKDOWN** | page/anchor title never fetched | fix `[url](url)`→`[title](url)` in omnibox + link menu (`A14`/`D4`), add copy-selection-as-markdown (`D11`) | a coherent "copy as markdown everywhere" dev-workflow win ★ |
| **CL-SPECDRIFT** | spec and implementation have diverged | cmd prefix `:` vs `>` (`A3`), Esc/Backspace mode rules (`A6`), status-line icon rule (`B2`), newtab dot-grid contradiction (`E12`), height comments (`B10`) | one spec-reconciliation pass (specs are immutable from code — decide canonical side) |

---

## Cluster implementation log

Progress as the clusters are worked one-by-one (order = the row order above).
`✓` landed + gated · `◐` partially landed · `☐` not started.

- **◐ CL-LOADING** — 1a landed (`4aab43e`): **E2** active-tab `···` ticker +
  **A12** functional reload↔stop (new `stop` op → `Page::stop_load`). Gate
  green (fmt/clippy `-D`/tests), 3 new black-box tests, boot-clean
  smoke-verified (zero errors/panics with the new chrome). Review caught +
  fixed a `chrome_ready`-ordering bug that would have swallowed the boot
  tab's first ticker. The transient ticker/glyph *visual* is pending capture
  (the dev screen was locked at verify time). **B7** ("loading 64%") is
  **deferred to 1b** — a real percentage needs CEF `OnLoadingProgressChange`;
  an indeterminate number would be fabricated, which the through-line forbids.
- **✓ CL-KBNAV** — landed in three phases, all **live-verified** on a real
  Mote (ydotool-driven, screenshot-confirmed): **p1** (`4c4dc51`) extracted a
  shared `roving.js` helper (pure nav-math + dual-mode attach factory) and
  refactored the omnibox completion dropdown onto it with 31 node regression
  assertions wired into the lefthook gate; **p2a** (`60eca12`) the context
  menu, security popover, and workspace popover (**D2**, **F4**); **p2b**
  (`0c9036a`) the integrity panel cards (**H13**). j/k + arrows everywhere,
  Enter/Space activate, Esc closes + returns focus. Live-testing caught four
  real seams unit tests can't see: `roving.js` not embedded/served (would've
  broken omnibox arrows); chrome-focus-capture routing (`focus_changed` claim
  — content-triggered surfaces got no keys); a pre-existing P1 `.ws-chip`
  selector regression that self-closed the workspace popover; and a
  document-vs-element keydown-listener subtlety for keyboard-opened overlays.
- **✓ CL-URL-XPARENCY** — landed (`d3bbcce` shell · `2b75774` chrome),
  **live-verified** on real Mote. Shell `analyze_url` (psl eTLD+1 + the
  `clearurls` crate for de-tracking + redirect-unwrapping) pushes a structured
  `set_url` + a stripped hover-URL; chrome renders the unfocused emphasis
  display layer (A8), the tracker chip + per-param `--danger` underline +
  "copy clean url" menu (A9), and the shell renders the hover destination
  preview (B3). Decisions: reuse shell `psl`; **clearurls (LGPL-3.0)** accepted
  by the maintainer (recorded in `/THIRD-PARTY-LICENSES.md` +
  `docs/research/cl-url-xparency-tracking-param-source.md`); surface-don't-strip
  (copy-clean is opt-in, the address bar is never auto-cleaned). Verified live:
  `example.com` bright vs dimmed scheme/path when blurred, raw editable URL on
  focus, "2 trackers" chip + utm/gclid underlined, hover shows the de-tracked
  destination. (copy-clean's *clipboard content* not independently captured —
  no clipboard CLI on the box; the row is wired to the proven
  `navigator.clipboard` path with a unit-verified clean value.)
- **◐ CL-MARKDOWN** — partial. **A14** landed (`37eaecc`): the omnibox
  "copy as markdown link" now emits `[title](url)` (the document title is
  available via `on_title_change`; pushed on `set_url`). **D4** (link →
  `[linktext](url)`) and **D11** (copy *selection* as markdown) are **deferred**
  — both need structured DOM data (anchor `innerText`; selection HTML) out of
  an untrusted content page, which Mote's isolation model doesn't surface (CEF
  context-menu params give only `target_url` + plain `selected_text`; the host
  bridge is gated to `mote://chrome` only; `eval_js` on content is one-way).
  They require a dedicated **render-process DOM-extraction channel** (a
  `ProcessMessage` carrying link-text / selection-HTML), which crosses the
  untrusted-content isolation boundary (DISCIPLINES §1) and warrants its own
  design/security review. Per the maintainer's "rock-solid or defer" call, D11
  in particular can't be done well without the selection's HTML, so it waits.
  (Clipboard *content* not capturable on this box — XWayland clipboard
  unreadable by available tools; `mote://` is a verified secure context so the
  `navigator.clipboard` write path is the real one.)
- ☐ CL-SEARCH ·
  ☐ CL-KEYMAP · ☐ CL-XPARENCY-DATA · ☐ CL-CONFIG-TRUTH · ☐ CL-SPECDRIFT

## Findings by category

### Navigation & Omnibox

- **A4 — security dot leaks into find/cmd modes** · defect · P0 · S · The `.secure` dot stays visible in `[find]`/`[cmd]`, implying a connection-trust claim about a search string. Hide `.secure` in non-url modes. ★ (honesty)
- **✓ A8 — domain-emphasis URL formatting** · enh · P1 · M · Render eTLD+1 in `--fg`, scheme/path/query in `--fg-2` when unfocused; full raw string on focus. The spec's `.host`/`.path` spans exist but are never emitted. Differs from Safari by never *eliding* — emphasis via token color, full URL always present. ★ [CL-URL-XPARENCY] — ✓ landed (`2b75774`): unfocused display-layer overlay; live-verified emphasis + raw-on-focus swap.
- **✓ A9 — surface tracking params + "copy clean url"** · enh · P1 · M · Inline count of tracking params + a context-menu "copy clean url"; never auto-strip the displayed/navigated URL. ★ [CL-URL-XPARENCY] — ✓ landed (`2b75774`): "N trackers" chip + per-param `--danger` underline + copy-clean menu row (opt-in, never auto-strips). Live-verified.
- **A10/I1/I2 — wire the config search engine; search-vs-navigate; "search with \<engine\>" row** · defect/gap · P0 · M · The single most-felt omnibox gap. Engine from Lua config (not a GUI dropdown). ★ [CL-SEARCH]
- **A11 — completions: no empty/no-match state, no source grouping, no default action row** · enh · P1 · M · Dropdown vanishes on zero results; add a persistent top "search for 'X'" / "open \<url\>" row and a no-match affordance. Stay in the mono/bracket idiom (dim `[source]` separators, not colored icons).
- **I6 — inline autocomplete (prefix completion)** · gap · P1 · M · Typing `git` should auto-fill `github.com` with the tail selected; ranking has no prefix-beats-substring notion. Table-stakes speed.
- **I4 — bookmark suggestions unranked, starved by history flood** · defect · P1 · M · "All history first, then bookmarks, cap 10" means an exact-title bookmark loses to 10 weak history hits. Needs a unified cross-source score.
- **I7 — fuzzy matching + Firefox-style source sigils** (`*`bookmarks `^`history `%`tabs `#`title `$`url) · enh · P2 · L · A query DSL over inspectable local data — a strong dotfiles-native differentiator the qutebrowser crowd expects. ★
- **I14 — zero-input top-sites/recent on omnibox focus** · gap · P2 · S · Reuses the existing relevance ranking; config-gate it to respect the minimal aesthetic.
- **A7/E1 — newtab omnibox shows internal URL** · defect · P0 · S · Blank + placeholder + focus on the newtab page instead of `mote://chrome/newtab.html`.
- **✓ A12 — reload has no stop state** · gap · P2 · M · Static reload→stop *glyph swap* (no spinner) driven by load state. [CL-LOADING] — ✓ landed (1a, `4aab43e`): functional stop (`stop` op → `Page::stop_load`).
- **A13 — long-press history popover is a permanent "coming later" stub** · enh · P2 · M · The 500ms timer + anchored popover are built; wire CEF's back/forward entry list. 90% done.

### Find & In-Page

- **C2/C3/C4/C1 — see P0 list** (query discarded, Enter dead, count blank, wrong placeholder). Together these mean find mode currently does not work end-to-end.
- **C5 — Shift+Enter for previous match** · gap · P1 · S · Standard everywhere; only Ctrl+Shift+G (shell path) exists.
- **C6 — blur drops active find silently** · gap · P1 · M · Highlights stay but no signal. ★ Surface "find active: 3/12" in the status line (the `mote.findcount` slot already exists) — a transparent, Mote-native pattern.
- **C7 — smartcase / case toggle** · gap · P2 · M · CEF's `match_case` is hardcoded `false`. ★ A `mote.find.smartcase` config (vim semantics: insensitive unless the query has an uppercase) is zero-UI and reads as "made for me" to the Neovim persona.
- **C8 — find state not persisted per tab** · gap · P2 · M · Switching tabs loses query/count; switching back shows nothing.
- **C9 — keycap hints for ↑↓ navigation** · enh · P2 · S · `[Ctrl+Shift+G][Ctrl+G]` keycaps in the find bar teach the bindings without adding browser chrome. ★ (uses the existing tooltip-kbd pattern)
- **C10 — aria-label not updated for find mode** · defect · P1 · S · Screen reader announces "address combobox" in find mode.
- **Decision to record:** should find stay an omnibox mode or become a dedicated find-bar element? (analyst leans: keep the mode, but it must reach feature-complete before the `[find]` tag is credible.)

### Context Menus & Actions

- **D1 — editable-field menu missing** (P0, above).
- **D10 — back/forward never show** (P0, above).
- **✓ D2 — no keyboard nav in the menu** · defect · P0 · M · ⚠ undermines the keyboard-first claim. [CL-KBNAV] ★ — ✓ landed (p2a, `60eca12`): arrows+j/k over actionable rows, Enter/Space activate, Esc close+return-focus; needed a `focus_changed:"chrome"` claim so content-triggered menus receive keys. Live-verified.
- **D3 — hardcoded Google search on selection** · defect · P1 · S · ⚠ violates both config-is-code and no-data-without-consent. [CL-SEARCH]
- **D5 — image menu: no "copy image"/"save image"** · gap · P2 · M · (save ties to the deferred `downloads:*` domain.)
- **D6 — no "open link in background tab"** · gap · P1 · S · `new_tab` op lacks a `background` param (the popup path already has it).
- **D7 — no plugin-contributed context-menu items (`ui:context_menu` capability)** · gap · P1 · L · ★★ The single strongest extensibility item: vim-mode ("hint mode"), adblock ("block element"), reader-mode, password-manager all want this. Surfaces in the integrity panel transparently. Needs an ADR. (CLAUDE.md explicitly names this a differentiator.)
- **D8 — link+selection menus don't merge** · defect · P2 · S · Exclusive priority drops selection items when right-clicking a link inside a selection; both flag bits are already present.
- **D9 — no separators/grouping in the popover** · gap · P2 · S · Flat list; needs a `separator` row type — prerequisite for distinguishing built-in vs plugin rows (D7).
- **✓ A14 / ◐ D4 — copy-as-markdown uses URL as anchor text** · defect · P1 · M · Produces `[url](url)`. [CL-MARKDOWN] ★ — ✓ **A14** landed (`37eaecc`): omnibox copy-as-markdown uses the document title → `[title](url)`. ◐ **D4** (the *link* case via `closest('a').innerText`) deferred — needs the render-process DOM-extraction channel (untrusted-content isolation boundary); CEF context-menu params don't carry anchor text.
- **◐ D11 — copy selection as markdown** (blockquote/inline-code) · enh · P2 · S · ★ no mainstream browser does this; on-brand for the dev/LLM-prompt workflow. [CL-MARKDOWN] — ◐ **deferred** (maintainer's "rock-solid or defer" call): needs the selection's *HTML* (same render-process DOM-extraction channel) + a battle-tested HTML→markdown lib. Plain `selected_text` has no structure to preserve, so it can't be done well yet.

### Tabs & Session

- **✓ E2 — tab loading ticker** · gap · P1 · M · The spec'd `.load` `···` accent ticker is dead CSS; `is_loading` is tracked in CEF but never serialized. ★ (terminal-aesthetic, no spinner). [CL-LOADING] — ✓ landed (1a, `4aab43e`): active-tab ticker, survives `renderTabs` rebuilds.
- **E3 — audio/mute indicator** · gap · P1 · M · Spec'd `.audio` `♪` glyph + click-to-mute; CEF audio callback + `tabs_json` field both missing. A tab playing audio is invisible.
- **E7 — drag-reorder tabs** · gap · P2 · L · Spec'd ("drag: reorder; off-strip = new window") but entirely absent; matters more in a vertical list than a horizontal strip.
- **E9 — Ctrl+Shift+Tab (reverse cycle) + rebindable prev/next** · gap · P1 · S · Only forward `Ctrl+Tab` exists. [CL-KEYMAP] ★ (rebindable = dotfiles-native)
- **E4 — direct tab-by-index switching** · gap · P1 · M · ⚠ Ctrl+1–9 is taken by workspaces; resolve the allocation (e.g. Ctrl+Alt+N for tabs) explicitly. [CL-KEYMAP]
- **E11 — recently-closed-tab surface** · gap · P2 · M · `ClosedTabStack` exists + Ctrl+Shift+T pops it, but no way to reopen the *2nd*-last or see the stack. ★ A collapsed "N closed ›" row at the bottom of the `[tabs]` panel (same mono row style) — distinct from the chronological `[history]` panel.
- **E10 — tab-count chip shows while non-tabs panel active** · defect · P2 · S · The `[tabs]` count persists under the `[bookmarks]`/`[history]` header, implying "3 bookmarks." Gate it to the tabs panel.
- **E8 — rail active-stripe clips outside the activitybar** · defect · P2 · S · `left:-4px` stripe can collide with the structural border under subpixel rounding; `overflow:hidden` or inset.
- **I8 — "switch to open tab" suggestion source** · gap · P1 · M · ★ A tabs plugin subscribing to `urlbar:suggest` (source="tab") proves the collector API is honest and stops duplicate-tab churn (DESIGN names tab-search as the canonical contributor).
- **F6/F11 — tab picker is single-workspace, but named/keybound like it's global** · gap · P1 · M · Ctrl+Space only searches the active workspace; add a scope label + a `⌘⇧Space` cross-workspace mode. ★ (Mote's keyboard-first answer to Arc's universal search, no cloud)

### New-Tab & Start

- **E5 — newtab is inert** · enh · P1 · L · No quick links, no recent, no omnibox auto-focus. ★ Build a *Neovim-dashboard*-style start page (recent history rows, pinned shortcuts from `managed.lua`, no images, no external fetch) via the existing `newtab.center` slot — config-driven, themable, replaceable. NOT a Chrome visual-tile grid.
- **E6 — newtab hint hardcodes `⌘L`** · defect · P1 · S · macOS glyph on a Linux-first app (actual bind is Ctrl+L). Push the platform-correct accelerator from the shell.
- **E12 — newtab dot-grid doubles the slot dot-grid** · defect · P2 · S · ⚠ spec both forbids and mandates a dot-grid background; decide and drop `slot-empty` from `newtab.center`. [CL-SPECDRIFT]
- **I11 — config-declared bookmarks (bookmarks-as-code)** · enh · P2 · M · ★ Declare a curated, version-controlled bookmark set in dotfiles, merged on load — distinguish from runtime-toggled stars per the managed.lua rule. (Feeds the newtab pins.)

### Workspaces

- **F1/F10/F3 — see P0 list** (hardcoded slug table, silent index-switch failure, `⌘⇧W` collision).
- **F2 — `mote.workspace.define` config surface absent** · gap · P0 · L · ⚠ The DESIGN-spec'd dotfile mechanism to declare workspaces (name/icon/accent/identity/pinned) does nothing — Mote is currently *less* configurable than mainstream here, inverting its top differentiator. ★
- **F7 — per-workspace personalization (accent/icon/identity/pinned tabs)** · gap · P1 · L · ★ "Workspaces as full contexts, not tab groups" is what separates Mote from Firefox containers / Chrome groups; landing per-workspace **accent** alone is high-impact and config-declarable (which no mainstream browser supports).
- **✓ F4 — workspace popover not keyboard-navigable** · defect · P1 · S · [CL-KBNAV] — ✓ landed (p2a, `60eca12`): listbox roving (arrows+j/k), current option focused on open, Enter/Space switches workspace. Also fixed a pre-existing P1 regression where the popover self-closed on its opening click (stale `.workspace-strip` guard vs the renamed `.ws-chip`). Live-verified Default↔Work.
- **F5 — multi-dot indicator means nothing to the user** · defect · P1 · S · A lone accent dot ("more than one workspace exists") with no label/tooltip; add a tooltip or replace with a count/icon.
- **F8 — no GUI create/rename/delete (and none via command mode)** · gap · P2 · M · ★ The right model: GUI writes a managed.lua entry → Lua reload picks it up (no conflict with config-truth). Also expose `:workspace` command-mode verbs.
- **F9 — switch is fire-and-forget** · gap · P1 · S · Chip name updates only on the next shell push; add an instant optimistic name update (no animation needed).
- **F12 — `workspaces:on_change` payload too thin** · gap · P2 · S · ★ Carries only `{active=slug}`; enrich with name/accent/icon/identity so plugins can react to switches without secondary calls.

### Status & Feedback

- **B1 — mode chip frozen at NORMAL** (P0, above). ★
- **◐ B7 — no loading indicator** · gap · P1 · M · Spec'd "connection segment → inline progress + `loading 64%`"; `is_loading` exists in CEF, unsurfaced. ★ [CL-LOADING] — ◐ deferred to **1b**: the binary state is wired (1a); a real percentage needs CEF `OnLoadingProgressChange` (no fabricated number).
- **B4 — zoom indicator is passive + transient** · gap · P1 · S · After 1500ms nothing shows current non-100% zoom; remembered-zoom pages give no confirmation. Add a persistent non-100% indicator + tooltip hint "Ctrl+0 to reset" (click-to-reset is blocked by the v2 click API, B5).
- **✓ B3 — hover-url not stripped/truncated** · gap · P2 · M · ★ Shows full tracked URL verbatim; show origin+path + a tracking-param count. [CL-URL-XPARENCY] — ✓ landed (`d3bbcce`): hover shows the `clearurls`-cleaned + redirect-unwrapped destination's full host+path (subdomain kept for anti-phishing), query stripped, tracker count appended. Live-verified.
- **B5 — status elements can't be clickable** · gap · P1 · L · ★ `action`/`disabled` are reserved-v2 stubs; `statusline.publish-clickable` is empty. Activating this makes the status bar as composable as Neovim lualine.
- **B11 — no inter-plugin priority contract** · gap · P1 · M · ★ A plugin can collide with `mote.mode` at priority 100; add reserved ranges or named anchor slots (more composable than lualine's fixed sections).
- **B8 — spec'd `theme` + `resources`(RAM) elements unimplemented** · gap · P2 · M/L · ★ RAM-per-tab in the status bar is shown by no mainstream browser — a "no hidden cost" transparency signal.
- **H6 — live plugin-activity indicator** · enh · P1 · M · ★ An ambient, restrained glyph when a plugin is doing network I/O, attributed to the named plugin+permission (data the audit producer already has) — the always-visible counterpart to the on-demand panel. (This is the DATA the status-line security element should expose.)

### Settings & Config

- **G10/G4/G3/G1 — see P0 list** (write stubs, no live read, toggle wiring, viewport fill).
- **G5 — per-row config provenance badge** · gap · P1 · M · ★★ "set in plugins.lua:12" / "overridden by managed.lua" + `[clear override]`. The `.managed-badge` CSS hook already exists. This is the highest-leverage feature for the config-truth vision — categorically beyond any browser (closest analog: VS Code's "set in workspace settings").
- **G6 — per-capability revoke from the plugins section** · gap · P1 · M · ★ ADR-0017 spec'd `[revoke <cap>]`; HTML omits it. Mote's capability model is finer than Chrome's optional-permission toggle.
- **G7 — plugins section shows no provenance** · gap · P1 · M · ★ No source/commit/origin-glyph; with them, the section becomes a dotfiles audit surface ("is my adblock the exact commit I pinned?"). [CL-XPARENCY-DATA]
- **G8 — keybinds section → which-key discovery** · gap · P2 · M · ★ Add prefix drill-down (type `Ctrl+` → see all chords), the Neovim-native discoverability the persona expects; no mainstream analog.
- **G9 — no cross-section settings search** · gap · P2 · M · Chrome/Firefox search-all-settings is table-stakes for a search-biased audience; deep-link results, "open in config" as a result action. ★
- **G2 — stale broken-load screenshots (mojibake "mote â€" composited")** · defect · P1 · S · The bug is fixed in code (`b68e1f0`/`3b99c7a`); the `docs/screenshots/p6/*` artifacts still show the broken state and misrepresent P6. Re-capture.

### Security & Transparency (the differentiator — go deep here)

- **H1 — audit log hardcodes "allowed"; no browser-own-outbound row** (P0, above). ★★ [CL-XPARENCY-DATA]
- **H2 — security popover all placeholder** (P0, above). ★ Beyond cert/cookies parity, list *which plugins hold permissions on this origin* — impossible in Chrome/Firefox (no per-plugin model). [CL-XPARENCY-DATA]
- **H3 — explain/block audit actions non-functional** · defect · P1 · M · ★ `href="#"` / not rendered. Tie "explain" to the *capability call* (plugin, permission, latency — already captured), and "block" as one-click consent.
- **H4 — per-plugin "what did this do recently" timeline** · enh · P1 · M · ★ The ring already stores operation/decision/latency/timestamp and `recent_for_plugin` returns it; the panel collapses it to one `last_used` string. Show the real call sequence ("vim-mode: keys:bind ×3, page:inject_script(github.com) 4s ago").
- **H5 — `page:inject_unsafe_script` not visually red-flagged** · gap · P1 · S · ★ DESIGN §597 requires the one world-isolation escape to be conspicuously distinct from ordinary high-risk; it gets the same generic badge.
- **H14/G11 — settings-integrity table is a static 3-row fixture** · defect · P1 · M · ★ The everyday integrity view (the non-modal counterpart to Ctrl+Shift+I) shows fiction; a real checksum *mismatch* — the one event that most needs to be visible — is structurally invisible. Wire to live `IntegrityStatus`. [CL-XPARENCY-DATA]
- **H16 — `[verified]` badge has no inspectable backing** · gap · P2 · M · ★ Show pinned checksum + verify time on drill-down (`IntegrityPluginDetail` only `eprintln!`s). "Verified" should be evidence, not an assertion.
- **H15 — no update-diff preview** · gap · P2 · M · ★ The permission-delta path exists; add the source-commit/checksum diff DESIGN promises ("update diffs shown before applying").
- **H7 — export the audit log** · enh · P2 · S · ★ `history()` already reads the durable sink; add a user-initiated JSON/CSV export (never auto-upload). A portable per-call, plugin-attributed ledger.
- **H8 — one-click revoke from the security popover** · enh · P2 · M · ★ Revoke a plugin's origin-scoped capability at the point of trust evaluation (revoke plumbing already exists). [depends on H2]
- **H9 — relative timestamps go stale, no exact time** · defect · P2 · S · "2 minutes ago" is frozen at render; attach the ISO timestamp on hover. A forensic surface must be precise.
- **H10 — "last 24h"/"last 7d" labels don't match the data** · defect · P1 · S · ⚠ Sourced from a ring buffer (capped at 20 denials), not a time-windowed query. A false temporal claim on the flagship surface; query the durable sink or relabel.
- **H11 — storage audit lost its "what" label** · gap · P2 · S · Live `label: None` drops "2.3 MB *of filter lists*" → just an opaque number.
- **H12 — dangerous-combination warnings aren't actionable** · enh · P2 · M · ★ Make each warning's named permissions click-to-scroll to the offending rows ("deny one of these"). Combination-aware approval is already Mote-only.
- **✓ H13 — integrity panel has no in-panel keyboard model** · gap · P2 · M · j/k between cards, Enter to expand. [CL-KBNAV] — ✓ landed (p2b, `0c9036a`): roving over `.plugin-card` (j/k+arrows), Enter/Space into the card's first action; Esc shell-owned. Live-verified bookmarks→history→workspace-manager (the keydown driver must be document-level — cards lose DOM focus after the shell's async send_focus).
- **I10 — delete-from-history / clear-range** · gap · P1 · M · ★ `ui:history_provider` is append-only; `history:delete` is reserved but unimplemented. Local data you can't delete undercuts the privacy story.

### Plugins & Providers (extensibility + bookmarks/history quality)

- **D7 — `ui:context_menu` plugin capability** (P1, L) ★★ — see Context Menus.
- **B5/B11 — status-line clickability + ordering contract** ★ — see Status & Feedback.
- **F12 — richer `workspaces:on_change` payload** ★ — see Workspaces.
- **I9 — history store is unbounded** · gap · P1 · M · ⚠ `record_visit` appends forever; `query` does a full `list_keys()` + per-event join *per keystroke* → O(total visits) per keypress, breaking the sub-ms perf claim. Resolve with a config-declared retention window (on-brand) + scan only deduped `u:` records.
- **I13 — export + scriptable query surface** · enh · P2 · M · ★ `:hist github` / `:bmark add`, export to a dotfile-friendly format. Turns these plugins into the showcase for "queryable/scriptable first-party data." Stays local (no cloud sync).
- **I12 — bookmark folders/tags + title editing** · gap · P2 · M · ★ Prefer flat-with-tags (queryable via the I7 sigils) over a nested folder tree, to stay keyboard-first.

### Keyboard & Modality

- **CL-KBNAV** (D2/F4/H13) — extract the omnibox dropdown's roving-focus into a shared helper for every floating surface. ★ P0-ish: the keyboard-first claim currently fails on menus/popovers/panels.
- **CL-KEYMAP** (A5 `⌘K`, E9 prev-tab, E4/F10 index-switch collision, F3 `⌘⇧W`, `⌘T`-from-content) — one coherent, conflict-free, documented (and ideally rebindable) keymap pass. ★
- **A6 — Esc/Backspace mode rules diverge from spec** · defect · P1 · S · [CL-SPECDRIFT]
- **G8 — which-key keybinds discovery** ★ — see Settings.
- **C7 — smartcase find** ★ — see Find.

### Visual Polish, Theming & Accessibility

- **B2 — status-line icons vs spec** · defect · P1 · S · ⚠ The spec forbids icons in segments but the API/CSS/`mote.security` element use them (correctly, as Lucide stroke — not emoji). The spec rule is likely stale; decide. [CL-SPECDRIFT]
- **B6 — tabcount uses an inline `style="color:var(--fg-2)"`** · defect · P1 · S · Bypasses the token API; themes can't restyle it. Add an `Fg2`/`Dim` token.
- **B10 — status-line height comments wrong (24px vs 22px token)** · defect · P2 · S · [CL-SPECDRIFT]
- **B9 — status-line `aria-live="polite"` on the whole bar** · defect · P1 · S · Floods screen readers on every hover-url tick; scope `aria-live` to meaningful changes only.
- **C10 / G3 — aria correctness** (find label; settings toggle role) — see Find / Settings.
- **E8 — rail stripe clip** — see Tabs.
- **F5 — multi-dot semantics** — see Workspaces.
- **General accessibility theme:** keyboard nav (CL-KBNAV), aria-live scoping (B9), role/aria-checked on the right element (G3), mode-aware aria-labels (C10) recur across surfaces — worth one a11y pass alongside the keyboard-map work.

---

## Vision-tension register (decisions the team owns)

Items that conflict with a stated principle and need an explicit call before building — surfaced here so they aren't resolved silently:

- **Honest placeholders.** H2/A4/A14/H10 — anywhere the chrome *asserts* trust/time it hasn't verified ("certificate: verified", "last 24h") contradicts the transparency pitch. Rule of thumb: render honest "not yet available" over confident-but-wrong.
- **Config-is-truth vs a write-capable GUI.** G5/G6/G10/F8 — the resolution the analysts converge on: the GUI writes to the **managed.lua** layer and shows per-value provenance; it never becomes an opaque shadow store. Worth ratifying as the settings doctrine.
- **Keybind allocation.** E4 vs F10 — Ctrl+1–9 can't be both tabs and workspaces; F3 — `⌘⇧W` can't be both. Decide the map.
- **Spec drift.** A3 (cmd `:` vs `>`), B2 (icons), E12 (dot-grid), B10 — specs are immutable from the code side; pick the canonical side per item in one reconciliation pass.
- **History retention vs local-first.** I9 — unbounded local history (privacy/transparency-positive) vs per-keystroke full-scan perf. A user-declared retention window squares both and is on-brand.
- **AI boundary holds.** Note that *none* of these ~100 ideas introduce built-in AI UI — the `[ask]` mode stays a plugin hook, suggestions stay non-AI, and the differentiator work (MCP/introspection) is untouched. Good.

---

## Appendix — methodology & sources

**Component scopes (disjoint, by analyst):** A omnibox/address-bar · B status-line · C find-in-page · D context-menus · E tabs/sidebar/new-tab · F workspaces · G settings · H security/integrity/transparency · I bookmarks+history plugins.

**Evidence:** code under `crates/mote-ui/chrome/`, `crates/mote-shell/`, `crates/mote-cef/`, `crates/mote-runtime/`, `plugins/{bookmarks,history,workspace-manager}/`; the spec contracts under `spec/components/`; the committed screenshots under `docs/screenshots/{p1..p6}/`; and the live running instance (`/tmp/mote-interactive`, captured 2026-06-04). Orchestrator-verified defects (✓): C2, C3, B1, A2, G10, I1.

**Not covered (out of scope for this survey):** identity/profile isolation internals, the MCP/AI-native path (Phase 8), packaging, and the password-manager stack (Phase 5b) — all pre-feature or deferred.
