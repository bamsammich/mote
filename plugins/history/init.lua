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
-- mote.json.encode/decode is used for all record serialization — this is the
-- library-backed approach (serde_json under the hood) required by the
-- feedback-use-libraries-not-rolled project rule.  The bookmarks plugin uses
-- a pipe-delimited codec (written before this rule was adopted); this plugin
-- uses JSON as the forward-looking baseline for all new plugins.
--
-- NOTE: os.time is NOT available in the Mote Lua sandbox (os module is
-- excluded for security).  `last_visited` is a monotonic `_seq` counter —
-- a stand-in until a `mote.time` host API is designed.  This gives stable
-- recency ordering without wall-clock access.  Future callers upgrading to
-- real timestamps should rename the field to avoid ambiguity.
--
-- Ranking formula for query_history results:
--   score = visit_count * 1_000_000 + last_visited
-- Higher visit_count wins; last_visited breaks ties between entries with the
-- same count.  Results are capped at 20 (the natural list length if smaller).
-- This formula is documented here so the ranking contract is explicit and
-- testable.
--
-- NOTE on filter case-sensitivity: Lua's string.lower is ASCII-only.  For
-- v0.1, case-insensitive substring matching uses string.lower on both the
-- filter and the record fields — this is correct for ASCII URLs and titles
-- and is an acceptable v0.1 simplification.  Unicode folding can be added
-- later via a mote.text host API if needed.
--
-- NOTE on max_entries: write-side LRU trim (max_entries cap) is intentionally
-- NOT implemented in this commit.  The data model is bounded in practice by
-- real browsing volume; a configurable trim policy belongs in a follow-up
-- task once the usage pattern is observed.  See phase5a plan §B2 deferred.
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

--- Read and JSON-decode a visit record from storage.
--- Returns nil if the key does not exist or decoding fails.
local function read_record(url)
  local raw = storage.get("v:" .. url)
  if raw == nil then return nil end
  return mote.json.decode(raw)
end

--- Encode and write a visit record to storage.
local function write_record(rec)
  local encoded = mote.json.encode(rec)
  if encoded ~= nil then
    storage.set("v:" .. rec.url, encoded)
  end
end

-- ---------------------------------------------------------------------------
-- `M.api` — satisfies BOTH capability contracts:
--   ui:history_provider: required_api = ["query_history", "record_visit"]
--   ui:urlbar_provider:  required_api = ["query"]
-- ---------------------------------------------------------------------------

M.api = {
  --- record_visit(payload)
  ---   payload = { url = <string>, title = <string|nil> }
  ---
  --- Records a visit for the given URL.  If a record already exists for this
  --- URL, visit_count is incremented and last_visited is updated; the title is
  --- replaced only when the new title is non-nil and non-empty (preserving an
  --- existing title when no title is supplied in this visit).
  ---
  --- Returns true on success; nil/false on invalid input.
  record_visit = function(payload)
    if payload == nil or payload.url == nil or payload.url == "" then
      return nil
    end
    local url   = tostring(payload.url)
    local title = payload.title

    local seq = next_seq()
    local existing = read_record(url)

    local rec
    if existing == nil then
      -- First visit: create a new record.
      rec = {
        url         = url,
        title       = (title ~= nil and title ~= "") and tostring(title) or "",
        visit_count = 1,
        last_visited = seq,
      }
    else
      -- Subsequent visit: bump count, update seq, optionally update title.
      rec = {
        url         = url,
        -- Only replace title if the new one is non-nil and non-empty.
        title       = (title ~= nil and tostring(title) ~= "")
                        and tostring(title)
                        or (existing.title or ""),
        visit_count = (existing.visit_count or 0) + 1,
        last_visited = seq,
      }
    end

    write_record(rec)
    return true
  end,

  --- query_history(filter)
  ---   filter = optional substring string (nil or "" = no filter)
  ---
  --- Returns a 1-indexed Lua array of visit records, ranked descending by:
  ---   score = visit_count * 1_000_000 + last_visited
  --- High visit_count wins ties; last_visited (recency) breaks equal-count
  --- ties.  Results are capped at 20.
  ---
  --- Case-insensitive ASCII substring matching is used for the filter (see
  --- NOTE in file header regarding unicode).
  query_history = function(filter)
    local keys = storage.list_keys()
    local records = {}

    -- Collect all "v:" keys and decode records; skip nil/decode failures.
    for _, key in ipairs(keys) do
      if key:sub(1, 2) == "v:" then
        local raw = storage.get(key)
        if raw ~= nil then
          local rec = mote.json.decode(raw)
          if rec ~= nil then
            records[#records + 1] = rec
          end
        end
      end
    end

    -- Apply optional substring filter.
    if filter ~= nil and filter ~= "" then
      local f = tostring(filter):lower()
      local filtered = {}
      for _, rec in ipairs(records) do
        local url_lc   = (rec.url   or ""):lower()
        local title_lc = (rec.title or ""):lower()
        if url_lc:find(f, 1, true) or title_lc:find(f, 1, true) then
          filtered[#filtered + 1] = rec
        end
      end
      records = filtered
    end

    -- Sort by score = visit_count * 1_000_000 + last_visited, descending.
    table.sort(records, function(a, b)
      local sa = (a.visit_count or 0) * 1000000 + (a.last_visited or 0)
      local sb = (b.visit_count or 0) * 1000000 + (b.last_visited or 0)
      return sa > sb
    end)

    -- Cap at 20 results.
    local result = {}
    for i = 1, math.min(#records, 20) do
      result[i] = records[i]
    end
    return result
  end,

  --- query(text) — urlbar provider contract.
  ---
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
  -- Nothing to initialize for the Phase-5a slice.  Storage is written
  -- on demand by the API functions above.
end

return M
