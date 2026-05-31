-- Mote bundled first-party plugin: history
--
-- Fulfills TWO exclusive capabilities:
--   • `ui:history_provider` (contract: query_history, record_visit)
--   • `ui:urlbar_provider`  (contract: query)
--
-- History owns the urlbar:suggest collector surface (DESIGN.md:349/862) — it
-- is the authoritative merge point for omnibox suggestions.  The standalone
-- urlbar plugin was removed in Phase 5a; this plugin is its replacement plus
-- the full history store.
--
-- POLICY FLOOR (docs/plans/02-browser-shell.md §8):
-- The shell owns the urlbar MECHANISM (element, host API, omnibox display).
-- This plugin owns the urlbar and history POLICY: what a visit is, ranking,
-- and the urlbar:suggest collector surface other plugins contribute to.
--
-- Data model (docs/plans/2026-05-30-phase5a-core-providers.md §Unknown-2):
-- One KV entry per URL, key = "v:" .. url, value = JSON-encoded record:
--   { url, title, visit_count, last_visited }
-- where `last_visited` is a monotonic integer counter (not wall-clock time).
-- A separate key "_seq" holds the latest sequence value.
--
-- `mote.json.encode/decode` is used for all record serialization — this is the
-- library-backed approach (serde_json under the hood) required by the
-- feedback-use-libraries-not-rolled project rule.
--
-- NOTE: os.time is NOT available in the Mote Lua sandbox (os module is
-- excluded for security).  `last_visited` is a monotonic `_seq` counter —
-- a stand-in until a `mote.time` host API is designed.  This gives stable
-- recency ordering without wall-clock access.  Future callers upgrading to
-- real timestamps should rename the field to avoid ambiguity.
--
-- ADR-0001: all hooks/events/api are declarative module-level tables.
-- setup() runs only after all four load-time validation steps pass.
-- No AI surfaces (DESIGN principle #8 / ADR-0002).
local M = {}

M.manifest = {
  schema = "v1",
  name = "history",
  version = "0.1.0",

  -- storage:persistent: to store and retrieve visit records.
  -- history:read:  enumerate and read visit history.
  -- history:write: create or update visit records.
  -- events:emit:  to emit/collect urlbar:suggest (B3 collector path).
  -- events:on:    to subscribe to events (B3 urlbar:suggest contributors
  --               will declare this; history declares it now so the manifest
  --               stays stable when B3 lands).
  permissions = {
    "storage:persistent",
    "history:read",
    "history:write",
    "events:emit",
    "events:on",
  },

  -- History owns both capabilities: it is the exclusive fulfiller of both
  -- ui:history_provider (record_visit / query_history) and ui:urlbar_provider
  -- (query), replacing the now-removed standalone urlbar plugin.
  capabilities = {
    "ui:history_provider",
    "ui:urlbar_provider",
  },

  identity_scope = "per_identity",
}

-- ---------------------------------------------------------------------------
-- Internal helpers
-- ---------------------------------------------------------------------------

--- Read the next monotonic sequence number from storage and advance it.
-- This is a stand-in for wall-clock time (os.time is not available in the
-- sandbox).  The counter is scoped to this plugin + identity by the storage
-- namespace.  The value is only meaningful for relative ordering: a larger
-- `last_visited` means "visited more recently in this session/store".
local function next_seq()
  local raw = storage.get("_seq")
  local n = tonumber(raw) or 0
  n = n + 1
  storage.set("_seq", tostring(n))
  return n
end

-- ---------------------------------------------------------------------------
-- `M.api` — satisfies BOTH capability contracts:
--   ui:history_provider: required_api = ["query_history", "record_visit"]
--   ui:urlbar_provider:  required_api = ["query"]
-- ---------------------------------------------------------------------------

M.api = {
  --- record_visit(payload) — payload = { url = <string>, title = <string|nil> }
  --- Records (or updates) a visit for the given URL.
  --- Stubs only in this commit; real implementation lands in B2.
  record_visit = function(_payload)
    -- B2: will read existing "v:<url>", bump visit_count, update last_visited,
    -- and write back with mote.json.encode.
    return true
  end,

  --- query_history(filter) — filter is an optional substring string.
  --- Returns an array of visit records ranked by visit_count + recency.
  --- Stub only in this commit; real implementation lands in B2.
  query_history = function(_filter)
    -- B2: will call storage.list_keys(), filter "v:" keys, decode with
    -- mote.json.decode, filter by substring, and rank.
    return {}
  end,

  --- query(text) — urlbar provider contract.
  --- Returns an array of suggestion records for display in the omnibox.
  --- Stub only in this commit; real B3 implementation collects contributor
  --- suggestions via mote.events.collect("urlbar:suggest", {text=text})
  --- and merges them with history's own visit-log matches.
  --
  -- TODO(B3): implement urlbar query via
  --   mote.events.collect("urlbar:suggest", {text=text})
  -- merged with own history matches; history owns the merge policy
  -- (DESIGN.md:862).
  query = function(_text)
    return {}
  end,
}

-- `M.events` — the urlbar:suggest subscriber (for B3) will be added here.
-- Declared empty now so the loader sees the table.
M.events = {}

-- `M.hooks` is empty.
M.hooks = {}

function M.setup()
  -- Nothing to initialize for the Phase-5a skeleton slice.  Storage is
  -- written on demand by the API functions above.
end

return M
