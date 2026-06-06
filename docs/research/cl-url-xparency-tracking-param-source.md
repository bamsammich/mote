# Tracking-param source for CL-URL-XPARENCY (A8/A9/B3)

- **Date:** 2026-06-06
- **Project:** Mote (Apache-2.0) — UX-elevation survey cluster CL-URL-XPARENCY.
- **Question:** what source backs tracking-param detection (A9 count + copy-clean, B3 hover strip)? Must be Rust (logic lives shell-side; the chrome renders a structured URL pushed from the shell, reusing the `psl` crate from ADR-0018 for the eTLD+1 boundary).

## Findings

- **`url` crate (v2)** is already in the tree (`mote-permissions`). Query-param parsing is free regardless of choice.
- **`clearurls`** (docs.rs/clearurls, github jendrikw/clearurls): wraps the crowd-sourced ClearURLs ruleset, **embedded/offline**, ~442k downloads/mo, used in production via `lemmy_utils`. API: `UrlCleaner::from_embedded_rules()?` → `clear_single_url_str(&str)`. **Returns only the cleaned URL** (no count of removed params — derive the count by diffing query params before/after). Deps: regex, serde, serde_json, url, percent-encoding (+ optional linkify/markdown-it). **License: LGPL-3.0-only.**
- **`url-cleaner`** (lib.rs): **AGPL-3.0-or-later** — more restrictive than LGPL; ruled out for a permissive project.
- **Redirect-unwrapping** (B3's `click.e.host/CL0/<encoded-real-url>` → destination) needs ClearURLs' per-provider "redirections" rules. The `clearurls` crate *may* apply these inside `clear_single_url_str` (unconfirmed from docs); a curated global param-name list does **not** do unwrapping.

## License tension (the decision)

Mote is **Apache-2.0**. `clearurls` is **LGPL-3.0-only**. LGPL↔Apache is legally permissible, but statically linking LGPL into a permissive binary imposes **LGPL §4 relinking obligations** on Mote's *distribution* (ship object files / allow relinking). Maintainer call, not an implementation default.

## Options

- **(A) `clearurls` crate (LGPL-3.0)** — fullest coverage: crowd-sourced rules, per-provider rules, likely redirect-unwrapping (the B3 unwrap shown in the mockup). Cost: Mote's distribution takes on LGPL obligations for this component; pulls regex+serde_json.
- **(B) Curated global param list, Apache-clean (recommended)** — embed ~40 well-known global tracking params (`utm_*`, `gclid`, `fbclid`, `msclkid`, `mc_eid`, `mc_cid`, `_hsenc`, `_hsmi`, `igshid`, `vero_id`, `mtm_campaign`, `wickedid`, `yclid`, `_openstat`, `mkt_tok`, …). Param **names are facts** (not the copyrightable ClearURLs data file). Covers A8 (emphasis), A9 (count + copy-clean for query trackers). B3 = strip-query-params + truncate + count, **without** redirect-unwrapping (defer that as a future enhancement regardless of license). No license entanglement; stays pure Apache-2.0. Lightest dep footprint (just `url`, already present).

## Decision

**Maintainer chose (A) — `clearurls` crate (LGPL-3.0), 2026-06-06.** Accepted the
LGPL relinking obligation in exchange for the crowd-sourced ruleset + redirect
unwrapping (which powers the B3 hover destination preview shown in the mockup).
Pinned `clearurls = "0.0.4"`; confirmed it unwraps redirects (`/url?q=` →
destination) and ships rules embedded/offline. The obligation is recorded in
`/THIRD-PARTY-LICENSES.md`; the dependency is confined to `mote-shell::analyze_url`
to keep a future relinking/dynamic-boundary tractable. (Recommendation had been
(B); maintainer overrode for coverage.)

Sources: https://docs.rs/clearurls/ · https://github.com/jendrikw/clearurls · https://lib.rs/crates/url-cleaner
