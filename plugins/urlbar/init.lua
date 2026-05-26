-- Mote bundled first-party plugin: urlbar
--
-- Fulfills the `ui:urlbar_provider` capability (exclusive, critical).
-- Contract (v1.toml): required_api = ["query"], required_events = [].
--
-- POLICY FLOOR (docs/plans/02-browser-shell.md §8):
-- The shell owns the urlbar MECHANISM (element, host API, omnibox display).
-- This plugin owns the urlbar POLICY: which suggestions appear, their ranking,
-- and the urlbar:suggest collector surface that other plugins contribute to.
--
-- Phase-2 slice (W-A0): accepts typed text, returns an empty suggestion list,
-- and emits urlbar:suggest so the collector seam is real from day one.
-- History/bookmark/tab-search ranking is Phase-5 richness layered on top of
-- this already-wired path.
--
-- ADR-0001: all hooks/events/api are declarative module-level tables.
-- setup() runs only after all four load-time validation steps pass.
-- No AI surfaces (DESIGN principle #8 / ADR-0002).
local M = {}

M.manifest = {
  schema = "v1",
  name = "urlbar",
  version = "0.1.0",

  -- Permissions needed for the Phase-2 slice.
  -- workspaces:list is not needed here; urlbar is navigation-policy only.
  -- events:emit: to emit urlbar:suggest onto the collector bus.
  -- events:on: to receive urlbar:suggest contributor responses (Phase 5).
  -- storage:persistent: to persist per-session state (last query, etc.).
  permissions = {
    "events:emit",
    "events:on",
    "storage:persistent",
  },

  capabilities = {
    "ui:urlbar_provider",
  },

  identity_scope = "global",
}

-- `M.api` satisfies the `ui:urlbar_provider` contract:
--   required_api = ["query"]
--
-- query(text) → suggestion list.
--
-- Phase-2 behavior: emit urlbar:suggest onto the collector bus so contributors
-- can respond, then return an empty list. The shell treats absence of
-- suggestions as an empty dropdown — not an error (§8.4 graceful degradation).
-- Real ranking (history + bookmarks + tab-search scoring) is Phase-5 richness.
M.api = {
  query = function(text)
    -- Stub to the urlbar:suggest collector seam. The shell host API for
    -- pushing navigation (Page::load_url) arrives with the shell in a later
    -- wave; we do not invent it here. The return value is the suggestion list
    -- the omnibox renders.
    --
    -- Phase-2: no history/bookmark store yet → empty suggestions.
    -- Phase-5: query history + bookmarks + open-tab index, rank, return list.
    return {}
  end,
}

-- `M.events` is empty for the Phase-2 slice: the urlbar:suggest collector
-- pattern (where the provider emits the event and aggregates contributor
-- responses) is Phase-5 richness. We declare the table so the loader sees it.
M.events = {}

-- `M.hooks` is empty for the Phase-2 slice.
M.hooks = {}

function M.setup()
  -- Phase-2: nothing to initialize. Storage will be used in Phase 5 to
  -- persist per-session state (last query, suggestion cache TTL, etc.).
  -- The setup() body is intentionally minimal; the host API for navigation
  -- arrives with the shell and is not stubbed here.
end

return M
