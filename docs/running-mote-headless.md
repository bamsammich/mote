# Running Mote Headless (debugging browser + test harness)

Mote can run **without a visible window** — rendering into a virtual display and
exposing a [Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/)
(CDP) endpoint you can attach to. Two use cases:

1. **Headless debugging browser** — launch Mote on a virtual display, attach
   Chrome DevTools (or a script) over CDP to inspect/drive the chrome UI and
   content pages without taking over your screen.
2. **Headless test harness** — the UI regression suite's end-to-end lane drives
   the real app over CDP under a virtual display (in CI and, optionally,
   git hooks). See ADR-0021 and the testing-architecture ADR.

The CDP surface is **off by default**, **loopback-only**, and **dev/test-only**
— governed by [ADR-0021](adr/0021-test-mode-cef-devtools-protocol-surface.md). It
must never ship enabled. Read that ADR before relying on this.

---

## Prerequisites

- A build of the app: `mise exec -- cargo build -p mote-app`.
- **`Xvfb`** for a virtual X display (Arch: `sudo pacman -S xorg-server-xvfb`).
- `LD_LIBRARY_PATH` must point at the dir holding `libcef.so` — `mote-app`'s
  binary currently lacks the `$ORIGIN` rpath, so set
  `LD_LIBRARY_PATH=$PWD/target/debug` (or `target/release`). (Packaging follow-up
  will remove this need.)

---

## Quick start

```sh
# 1. Start a virtual display.
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp &

# 2. Launch Mote into it, with the CDP port enabled.
#    NOTE the `env -u WAYLAND_DISPLAY` and WINIT_UNIX_BACKEND=x11 — see the gotcha below.
env -u WAYLAND_DISPLAY WINIT_UNIX_BACKEND=x11 DISPLAY=:99 \
    MOTE_REMOTE_DEBUG_PORT=9222 \
    LD_LIBRARY_PATH="$PWD/target/debug" \
    ./target/debug/mote --ozone-platform=x11 &

# 3. Confirm the CDP endpoint is live (loopback only).
curl -s http://127.0.0.1:9222/json/version | jq .
```

A healthy launch logs `chrome + N tab(s) live`, `N/N plugins loaded`, and
`first chrome+content frames painted` to stderr.

### ⚠️ The `WAYLAND_DISPLAY` gotcha (read this)

On a Wayland desktop (Hyprland, etc.), **you must scrub `WAYLAND_DISPLAY`**.
`winit` (which creates the window) prefers Wayland when `WAYLAND_DISPLAY` is set,
so it attaches to your **real compositor** — the window appears on your screen —
*even with* `DISPLAY=:99` and `--ozone-platform=x11`. That flag only steers CEF,
not winit. `env -u WAYLAND_DISPLAY WINIT_UNIX_BACKEND=x11` forces winit onto the
X11 (`:99`) backend. Verify with:

```sh
tr '\0' '\n' < /proc/"$(pgrep -x mote | head -1)"/environ | grep -E 'DISPLAY|WAYLAND'
# want: DISPLAY=:99  and NO WAYLAND_DISPLAY
```

---

## Driving it over CDP

### Enumerate targets

```sh
curl -s http://127.0.0.1:9222/json | jq -r '.[] | "\(.type)  \(.url)"'
# page  https://example.com/...            ← content tab
# page  mote://chrome/index.html           ← the chrome UI
```

Each target carries a `webSocketDebuggerUrl` on `127.0.0.1`. The `mote://chrome`
target is the chrome UI (omnibox, tabs, panels, settings); the `http(s)` targets
are content pages.

### Attach Chrome DevTools (interactive debugging)

From a real Chromium/Chrome on your machine, open `chrome://inspect`, add
`127.0.0.1:9222` under "Configure… → Discover network targets", and click
**inspect** on a target — full DevTools (Elements, Console, Network) against the
running Mote chrome or a content page. (Or open the `devtools://` front-end URL
from `/json` directly.)

### Drive it with Playwright

```js
import { chromium } from "@playwright/test";
const browser = await chromium.connectOverCDP("http://127.0.0.1:9222");
const ctx = browser.contexts()[0];
const chrome = ctx.pages().find((p) => p.url().startsWith("mote://chrome"));
console.log(await chrome.evaluate(() => location.href));
```

### Evaluate JS with no dependencies (Node ≥ 21 has a built-in `WebSocket`)

```sh
WS=$(curl -s http://127.0.0.1:9222/json | jq -r '.[] | select(.url|startswith("mote://chrome")) | .webSocketDebuggerUrl')
node -e '
const ws = new WebSocket(process.argv[1]);
ws.onopen = () => ws.send(JSON.stringify({id:1, method:"Runtime.evaluate",
  params:{expression:"location.href", returnByValue:true}}));
ws.onmessage = (e) => { const m = JSON.parse(e.data);
  if (m.id===1){ console.log(m.result.result.value); ws.close(); process.exit(0); } };
' "$WS"
```

---

## Environment variables

| Variable | Effect |
|---|---|
| `MOTE_REMOTE_DEBUG_PORT` | Enables the CDP endpoint on `127.0.0.1:<port>`. **Unset / `0` = off (default).** Governed by ADR-0021. |
| `MOTE_WINDOW_SIZE` | *(planned)* `WxH` initial window size, for deterministic test geometry. Defaults to `1280x800`. |
| `DISPLAY` | The X display to attach to (`:99` for Xvfb). |
| `WINIT_UNIX_BACKEND=x11` | Forces winit onto X11 (paired with scrubbing `WAYLAND_DISPLAY`). |
| `LD_LIBRARY_PATH` | Must contain `libcef.so` (`$PWD/target/debug`). |

---

## Cleanup

```sh
pkill -x mote      # then `pkill -9 -x mote` if CEF children linger
pkill -x Xvfb
```

A debug-build CEF instance is heavy (CPU + GPU paint); don't leave it running.

---

## Security model (why this is safe to ship)

The CDP endpoint is a process-global, out-of-band channel that can attach to any
renderer and evaluate arbitrary JS — exactly the capability the host-bridge
isolation (ADR-0005) makes unreachable. It is therefore confined, by tested
invariants ([ADR-0021](adr/0021-test-mode-cef-devtools-protocol-surface.md)):

- **Off by default** — `MOTE_REMOTE_DEBUG_PORT` unset ⇒ no listener. A default
  run opens nothing.
- **Loopback-only** — bound to `127.0.0.1`; there is no public-bind option.
- **Orthogonal to the sandbox** — enabling CDP does not relax `no_sandbox`.
- **Grants no plugin-reachable capability** — it is a test harness, not a
  plugin-facing API; it does not alter the `introspect:` / dev-mode model.

Never enable `MOTE_REMOTE_DEBUG_PORT` on a machine where the loopback interface
is reachable by untrusted parties.
