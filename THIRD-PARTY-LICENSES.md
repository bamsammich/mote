# Third-party licenses

Mote is licensed under **Apache-2.0** (see `LICENSE`). Most dependencies are
permissively licensed (MIT / Apache-2.0 / BSD). This file tracks dependencies
whose license imposes obligations beyond permissive attribution, so the
obligation is visible and honored in distribution.

> This list is currently hand-maintained. A future pass may automate it with
> [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) (generate a full
> dependency-license report in CI).

## Copyleft / weak-copyleft dependencies

| Crate | Version | License | Why it's here | Obligation |
|---|---|---|---|---|
| [`clearurls`](https://crates.io/crates/clearurls) | 0.0.4 | **LGPL-3.0-only** | Tracking-parameter detection + redirect-unwrapping for the URL-transparency feature (CL-URL-XPARENCY: omnibox tracker count, "copy clean url", hover-URL preview). Wraps the crowd-sourced [ClearURLs](https://clearurls.xyz/) ruleset. | LGPL-3.0 §4 (Combined Works): a distributed Mote binary that statically links `clearurls` must allow the end user to **relink** against a modified version of the library — e.g. by shipping the corresponding object files / build inputs, or by isolating `clearurls` behind a dynamically-replaceable boundary. The crate's own source is available under LGPL-3.0; Mote's source remains Apache-2.0. |

### Notes on the `clearurls` adoption

- **Decision of record (2026-06-06):** the maintainer accepted the LGPL-3.0
  obligation in exchange for the crowd-sourced ClearURLs ruleset and its
  redirect-unwrapping (which powers the hover-URL destination preview). The
  trade-off versus a license-clean curated param list is recorded in
  `docs/research/cl-url-xparency-tracking-param-source.md`.
- **Embedded rules:** `clearurls` ships the ClearURLs rules data embedded; no
  network access at runtime.
- **Containment:** the dependency is confined to `mote-shell` (URL analysis in
  `analyze_url`). Keeping it behind a single seam makes a future dynamic /
  replaceable boundary (the cleanest way to satisfy §4 relinking) tractable if
  Mote's packaging needs it.
- **Follow-up:** when Mote reaches a distribution/packaging milestone (ROADMAP
  Phase 9), revisit how the relinking obligation is satisfied for the shipped
  binary (object files vs. dynamic linking vs. dropping to the curated list).
