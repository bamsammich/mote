# ADR-0018 — Omnibox URL-vs-Search Determination: Public-Suffix-Based, HTTPS-Default with Loopback Exception

- **Status:** Accepted (approved by the maintainer 2026-06-05)
- **Date:** 2026-06-05

---

## Context and Problem Statement

When the user submits free text in the omnibox (the `omnibox_submit` op) or
picks "search for selection" from the context menu, the shell must decide
whether that text is a **URL to navigate to** or a **search query** for the
configured engine. (Suggestion-row clicks bypass this — they already carry a
real URL straight to `navigate`.) The determination lives shell-side in
`resolve_omnibox_input` (`mote-shell`) on purpose: the configured search
engine is config truth, so the chrome sends raw text and the shell decides.

The first implementation (shipped in `c0b7197`, Bucket-B item I1 of the UX
elevation survey) used "no whitespace + contains a dot → prepend `https://`;
otherwise search." That rule (a) **over-navigates** dotted non-domains —
`node.js`, `foo.internal` become dead `https://` loads where mainstream
browsers search them — and (b) **forces `https://` on loopback**, so
`localhost:3000` resolves to `https://localhost:3000` when local dev servers
are almost always HTTP. We researched Chrome and Firefox to ground a
correct rule.

**Research.** Chrome's `AutocompleteInput` consults a hardcoded ICANN
TLD / public-suffix list: `host.<known-suffix>` navigates, `host.<unknown
-suffix>` searches (so `grep.geek`, `foo.internal`, `node.js` all search).
Chrome defaults schemeless typed navigations to `https://` (HTTPS-by-default,
2023) with HTTP fallback, exempting loopback. Firefox's `URIFixup` treats
input as a search when there is a space or quote before the first `.`/`:`/`?`
(or it starts with `?`); a single dotless word searches and then runs a DNS
probe to offer *"did you mean to go to http://host/?"*. Sources:
[Firefox URL Bar Algorithm](https://wiki.mozilla.org/Firefox/URL_Bar_Algorithm),
[Chromium — Towards HTTPS by default](https://blog.chromium.org/2023/08/towards-https-by-default.html),
[Chromium searches unknown TLDs (HN)](https://news.ycombinator.com/item?id=41930967),
[OpenNIC — alternative TLDs in Chrome](https://wiki.opennic.org/chrome_alternative_tlds),
[Firefox bug 1578856 — `dns_first_for_single_words`](https://bugzilla.mozilla.org/show_bug.cgi?id=1578856).

## Decision Drivers

- Match mainstream muscle memory: `google.com` must navigate; `node.js` /
  `foo.internal` must search, exactly as Chrome does.
- Be honest about scheme: HTTPS-first like modern Chrome/Firefox, but
  loopback is HTTP (forcing HTTPS on `localhost` is a daily-driver papercut).
- Keep the resolver shell-side — the engine is config truth (cross-ref
  ADR-0017's `set_search_engine` → `managed.lua` write target).
- Use a maintained public-suffix-list crate, not a hand-rolled TLD table
  (the project's *use-libraries-not-rolled* discipline).

## Considered Options

1. **Keep the naive "any dot → navigate, always https"** — rejected: dead
   navigations for non-domains, wrong scheme for loopback, diverges from both
   browsers.
2. **Public-suffix-based determination + HTTPS-default with a loopback HTTP
   exception** (chosen) — mirrors Chrome's TLD-list behavior via a PSL crate.
3. **Full parity including the async-DNS "did you mean to navigate" single-word
   path** — deferred: needs async DNS plumbing out of v0.1 scope; recorded as
   the intended future path for dotless intranet hosts.

## Decision Outcome

Adopt **option 2**. `resolve_omnibox_input` applies the following, first match
wins:

1. **Explicit scheme** (`http`/`https`/`ftp`/`data`/`mote`/`about`/…) →
   navigate as-is.
2. **Whitespace or quote before the first `.`/`:`/`?`** (or a leading `?`) →
   **search** (Firefox's rule).
3. **Host-shaped → navigate**, when the input is an IP literal (v4/v6),
   `localhost` / `*.localhost`, a `host:port` with a numeric port, **or** a
   dotted host whose suffix is a valid **public suffix** (PSL/ICANN — Chrome's
   rule). Anything else (a dotless word, or a dotted host with an unknown
   suffix such as `node.js` / `foo.internal`) → **search**.
4. **Scheme for schemeless navigations** defaults to `https://`, **except
   loopback** (`localhost`, `127.0.0.1`, `[::1]`) → `http://`.
5. **Search** substitutes the configured engine template's `{q}` with the
   RFC-3986 percent-encoded query (space → `%20`). The template must contain
   `{q}`; a template lacking it is rejected and the resolver falls back to the
   built-in default.
6. **Deferred (future):** a dotless single word that DNS-resolves offers
   *"did you mean to go to `<host>`?"* (requires async DNS).

**Worked examples**

| Input | Result |
|---|---|
| `google.com` | navigate `https://google.com` |
| `wikipedia.org` | navigate `https://wikipedia.org` |
| `node.js` / `foo.internal` | **search** (unknown public suffix) |
| `rust async traits` | **search** (space before any delimiter) |
| `localhost:3000` | navigate `http://localhost:3000` |
| `127.0.0.1:8080` | navigate `http://127.0.0.1:8080` |
| `https://x.test/p` | navigate as-is |
| `mote://chrome/settings` | navigate as-is |

## Consequences

- Adds a maintained public-suffix-list dependency (e.g. `psl` / `addr` /
  `publicsuffix`) to `mote-shell`. The resolver shipped in `c0b7197` is
  revised, with tests covering the worked-examples matrix above
  (fail-first per the project's always-test rule).
- The `{q}` template is the stable contract between this resolver and the
  settings `set_search_engine` write path (ADR-0017); both sides agree on
  literal `{q}` substitution.
- Dotless **intranet hostnames** cannot be reached by bare name until the
  deferred async-DNS path lands — acceptable for v0.1 and identical to
  Chrome's default-search behavior. Until then, an explicit scheme
  (`http://wiki`) or a trailing form that trips rule 3 navigates.
- Cross-references: ADR-0017 (settings / search-engine write target);
  `docs/ux-survey/2026-06-04-elevation-opportunities.md` item I1.
