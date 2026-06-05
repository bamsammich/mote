-- Mote bundled first-party plugin: history
--
-- Fulfills TWO exclusive capabilities:
--   • `ui:history_provider` (contract: query_history, record_visit, update_title)
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
-- Data model (chronological visit log with URL-level title cache):
--
--   URL records  — key "u:<url>"
--     { url, title, first_seen_ms, last_seen_ms, total_count }
--     One record per URL; mutable.  Title is URL-level so update_title
--     propagates to all historical rows for that URL via the join in
--     query_history(sort="recent").
--
--   Visit events — key "e:<time_ms_padded>"
--     { url, time_ms }
--     One record per visit; append-only.  Key is a 16-digit zero-padded
--     decimal timestamp so storage.list_keys() returns events in
--     lexicographic = chronological order.
--
--     Collision note: two visits at the exact same millisecond on a
--     single-machine stream are astronomically unlikely, but the key space
--     is keyed on wall-clock ms so no explicit disambiguator is added.
--     If a future use-case requires sub-ms precision, append a counter suffix.
--
-- Wall-clock time enters via shell-stamping: the shell captures
-- SystemTime::now() and passes time_ms in the record_visit payload.  The
-- plugin is time-free — it only stores what the shell gives it.  This is
-- consistent with the broader "shell stamps context for plugins" pattern and
-- sidesteps the fingerprinting concern that gates a general mote.time API.
--
-- mote.json.encode/decode is used for all record serialization — this is the
-- library-backed approach (serde_json under the hood) required by the
-- feedback-use-libraries-not-rolled project rule.
--
-- NOTE on filter case-sensitivity: Lua's string.lower is ASCII-only.  For
-- v0.1, case-insensitive substring matching uses string.lower on both the
-- filter and the record fields — this is correct for ASCII URLs and titles
-- and is an acceptable v0.1 simplification.  Unicode folding can be added
-- later via a mote.text host API if needed.
--
-- NOTE on max_entries: write-side LRU trim (max_entries cap) is intentionally
-- NOT implemented.  The data model is bounded in practice by real browsing
-- volume; a configurable trim policy belongs in a follow-up task once the
-- usage pattern is observed.
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
  -- ui:history_provider (record_visit / update_title / query_history) and
  -- ui:urlbar_provider (query), replacing the now-removed standalone urlbar
  -- plugin.
  capabilities = {
    "ui:history_provider",
    "ui:urlbar_provider",
  },

  identity_scope = "per_identity",
}

-- ---------------------------------------------------------------------------
-- Internal helpers
-- ---------------------------------------------------------------------------

--- Read and JSON-decode a URL record from storage.
--- Returns nil if the key does not exist or decoding fails.
local function read_url_record(url)
  local raw = storage.get("u:" .. url)
  if raw == nil then return nil end
  return mote.json.decode(raw)
end

--- Encode and write a URL record to storage.
local function write_url_record(rec)
  local encoded = mote.json.encode(rec)
  if encoded ~= nil then
    storage.set("u:" .. rec.url, encoded)
  end
end

--- Write a visit event to storage.
--- Key format: "e:<16-digit-zero-padded-ms>" so list_keys() is chronological.
local function write_event(url, time_ms)
  local key = "e:" .. string.format("%016d", time_ms)
  local encoded = mote.json.encode({ url = url, time_ms = time_ms })
  if encoded ~= nil then
    storage.set(key, encoded)
  end
end

-- ---------------------------------------------------------------------------
-- `M.api` — satisfies BOTH capability contracts:
--   ui:history_provider: required_api = ["query_history", "record_visit", "update_title"]
--   ui:urlbar_provider:  required_api = ["query"]
-- ---------------------------------------------------------------------------

M.api = {
  --- record_visit(payload)
  ---   payload = { url = <string>, time = <number, unix ms> }
  ---
  --- Records a visit for the given URL.
  ---
  --- URL record (u:<url>):
  ---   - First visit: create with {url, title="", first_seen_ms=time,
  ---     last_seen_ms=time, total_count=1}.
  ---   - Subsequent visit: title untouched, last_seen_ms = time,
  ---     total_count += 1.
  ---
  --- Visit event (e:<padded_time>):
  ---   - Append {url, time_ms=time}.
  ---
  --- Returns true on success; false if url missing/empty or time invalid.
  record_visit = function(payload)
    if payload == nil or payload.url == nil or payload.url == "" then
      return false
    end
    local url  = tostring(payload.url)
    local time = tonumber(payload.time)
    if time == nil or time ~= time then  -- nil or NaN
      return false
    end
    local time_ms = math.floor(time)

    -- Upsert the URL record.
    local existing = read_url_record(url)
    local rec
    if existing == nil then
      rec = {
        url           = url,
        title         = "",
        first_seen_ms = time_ms,
        last_seen_ms  = time_ms,
        total_count   = 1,
      }
    else
      rec = {
        url           = url,
        title         = existing.title or "",
        first_seen_ms = existing.first_seen_ms or time_ms,
        last_seen_ms  = time_ms,
        total_count   = (existing.total_count or 0) + 1,
      }
    end
    write_url_record(rec)

    -- Append the visit event.
    write_event(url, time_ms)

    return true
  end,

  --- update_title(payload)
  ---   payload = { url = <string>, title = <string> }
  ---
  --- Resolves the asynchronous page title for an existing URL record WITHOUT
  --- counting a re-visit.  This is the title-on-load seam: `record_visit` is
  --- called at navigate time (counting the user navigation), then `update_title`
  --- is called when the CEF `on_title_change` callback fires with the resolved
  --- title.  The two responsibilities are intentionally separated so that
  --- `total_count` reflects real user navigations, not internal load events.
  ---
  --- Because title is URL-level, this update propagates to ALL historical
  --- visit rows for that URL via the join in query_history(sort="recent").
  ---
  --- Semantics:
  ---   • url missing/empty              → return false (no-op).
  ---   • no existing URL record for url → return false (no phantom record).
  ---   • title nil/empty                → return false (nothing to update).
  ---   • otherwise: overwrite title, leave total_count, first/last_seen_ms.
  ---   • Returns true on success.
  update_title = function(payload)
    if payload == nil or payload.url == nil or payload.url == "" then
      return false
    end
    local title = payload.title
    if title == nil or tostring(title) == "" then
      return false
    end
    local url = tostring(payload.url)
    local existing = read_url_record(url)
    if existing == nil then
      -- No prior visit for this URL — do not create a phantom record.
      return false
    end
    existing.title = tostring(title)
    -- total_count, first_seen_ms, last_seen_ms are intentionally unchanged.
    write_url_record(existing)
    return true
  end,

  --- query_history(payload)
  ---   payload = optional Lua table with fields:
  ---     filter  = string (nil or "" = no filter; case-insensitive ASCII match)
  ---     limit   = number (positive integer cap; default 20; ≤0 returns {})
  ---     sort    = "recent" | "relevance"  (unknown values → "relevance")
  ---
  --- sort="recent" (default for the sidebar):
  ---   Iterate e:<...> keys in lexicographic (= chronological) order, reversed.
  ---   For each event look up u:<url> for the current title.  Returns records:
  ---     { url, title, time_ms }
  ---   Each visit is a SEPARATE row — duplicates of the same URL appear multiple
  ---   times at different timestamps (matches real browser history behavior).
  ---
  --- sort="relevance" (default for the omnibox):
  ---   Iterate u:<url> keys.  Sort by (total_count DESC, last_seen_ms DESC).
  ---   Returns records: { url, title, total_count, last_seen_ms }
  ---   Deduped per URL — sane for suggestion behavior.
  ---
  --- filter: case-insensitive ASCII substring on url+title; applied before limit.
  --- default limit = 20 (preserves omnibox behavior).
  --- Unknown `sort` falls back to "relevance" silently (closed enum, default-deny).
  query_history = function(payload)
    payload = (type(payload) == "table") and payload or {}
    local filter = (type(payload.filter) == "string") and payload.filter or ""
    local limit  = (type(payload.limit)  == "number") and math.floor(payload.limit) or 20
    if limit <= 0 then return {} end
    local sort   = (payload.sort == "recent") and "recent" or "relevance"

    local keys = storage.list_keys()

    if sort == "recent" then
      -- Collect all "e:" keys; list_keys returns them in lexicographic order
      -- (= chronological).  We reverse to get newest-first.
      local event_keys = {}
      for _, key in ipairs(keys) do
        if key:sub(1, 2) == "e:" then
          event_keys[#event_keys + 1] = key
        end
      end

      -- Reverse for newest-first traversal.
      local n = #event_keys
      local reversed = {}
      for i = 1, n do
        reversed[i] = event_keys[n - i + 1]
      end

      -- Apply filter and limit: for each event, join to URL record for title.
      local f = (filter ~= "") and filter:lower() or nil
      local result = {}
      for _, key in ipairs(reversed) do
        if #result >= limit then break end
        local raw = storage.get(key)
        if raw ~= nil then
          local ev = mote.json.decode(raw)
          if ev ~= nil and ev.url ~= nil then
            -- Join to URL record for current title.
            local url_rec = read_url_record(ev.url)
            local title = (url_rec ~= nil) and (url_rec.title or "") or ""
            -- Apply filter on url + title.
            if f == nil then
              result[#result + 1] = {
                url     = ev.url,
                title   = title,
                time_ms = ev.time_ms or 0,
              }
            else
              local url_lc   = ev.url:lower()
              local title_lc = title:lower()
              if url_lc:find(f, 1, true) or title_lc:find(f, 1, true) then
                result[#result + 1] = {
                  url     = ev.url,
                  title   = title,
                  time_ms = ev.time_ms or 0,
                }
              end
            end
          end
        end
      end
      return result

    else
      -- Relevance: iterate u:<url> keys, sort by (total_count DESC, last_seen_ms DESC).
      local records = {}
      for _, key in ipairs(keys) do
        if key:sub(1, 2) == "u:" then
          local raw = storage.get(key)
          if raw ~= nil then
            local rec = mote.json.decode(raw)
            if rec ~= nil then
              records[#records + 1] = rec
            end
          end
        end
      end

      -- Apply optional filter.
      if filter ~= "" then
        local f = filter:lower()
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

      -- Sort: total_count DESC, then last_seen_ms DESC as tiebreaker.
      table.sort(records, function(a, b)
        local ca = a.total_count or 0
        local cb = b.total_count or 0
        if ca ~= cb then return ca > cb end
        return (a.last_seen_ms or 0) > (b.last_seen_ms or 0)
      end)

      -- Cap at limit and return relevance-shaped records.
      local result = {}
      for i = 1, math.min(#records, limit) do
        local rec = records[i]
        result[i] = {
          url          = rec.url,
          title        = rec.title or "",
          total_count  = rec.total_count or 0,
          last_seen_ms = rec.last_seen_ms or 0,
        }
      end
      return result
    end
  end,

  --- query(text) — urlbar provider contract.
  ---
  --- Returns a 1-indexed Lua array of suggestion records for display in the
  --- omnibox.  Each record is { url, title, source } where source identifies
  --- the contributor ("history" for own visit-log matches, or the source tag
  --- returned by a contributing plugin such as "bookmark").
  ---
  --- v0.1 merge policy (history owns this — DESIGN.md:862):
  ---   1. Gather own visit-log matches: scan u:<url> keys, case-insensitive
  ---      ASCII substring match on url+title, rank by
  ---      total_count DESC, last_seen_ms DESC.
  ---      Tag every record with source="history".
  ---   2. Collect contributions: call mote.events.collect("urlbar:suggest",
  ---      {text=text}); each element is one subscriber's contribution array.
  ---   3. Merge: history matches first (already ranked), then flatten all
  ---      subscriber contributions in collector order.  Cap total at 10.
  ---
  --- Empty text (nil or "") → return {} immediately (cheap path; no storage
  --- scan, no collect call).
  query = function(text)
    -- Early-exit: empty text produces no suggestions.
    if text == nil or text == "" then
      return {}
    end

    local filter = tostring(text):lower()

    -- -----------------------------------------------------------------------
    -- Step 1: own history matches from URL records (deduped, ranked by
    -- total_count * 1e6 + last_seen_ms for scoring stability)
    -- -----------------------------------------------------------------------
    local keys = storage.list_keys()
    local history_records = {}
    for _, key in ipairs(keys) do
      if key:sub(1, 2) == "u:" then
        local raw = storage.get(key)
        if raw ~= nil then
          local rec = mote.json.decode(raw)
          if rec ~= nil then
            local url_lc   = (rec.url   or ""):lower()
            local title_lc = (rec.title or ""):lower()
            if url_lc:find(filter, 1, true) or title_lc:find(filter, 1, true) then
              history_records[#history_records + 1] = rec
            end
          end
        end
      end
    end

    -- Sort by score = total_count * 1_000_000 + last_seen_ms, descending.
    table.sort(history_records, function(a, b)
      local sa = (a.total_count or 0) * 1000000 + (a.last_seen_ms or 0)
      local sb = (b.total_count or 0) * 1000000 + (b.last_seen_ms or 0)
      return sa > sb
    end)

    -- Build the suggestion list from history matches, tagged source="history".
    local suggestions = {}
    for _, rec in ipairs(history_records) do
      suggestions[#suggestions + 1] = {
        url    = rec.url,
        title  = rec.title or "",
        source = "history",
      }
    end

    -- -----------------------------------------------------------------------
    -- Step 2: collect contributions from subscribers (e.g. bookmarks).
    -- -----------------------------------------------------------------------
    local extras = mote.events.collect("urlbar:suggest", { text = text }) or {}
    for _, contrib in ipairs(extras) do
      if type(contrib) == "table" then
        for _, rec in ipairs(contrib) do
          suggestions[#suggestions + 1] = rec
        end
      end
    end

    -- -----------------------------------------------------------------------
    -- Step 3: dedup by URL across the merged set (I3).
    --
    -- Policy: when a URL appears in multiple sources, keep the LAST occurrence
    -- in traversal order.  Because contributor rows (e.g. bookmarks) are
    -- appended AFTER history rows, a bookmark entry overwrites a history entry
    -- for the same URL — the bookmark is considered higher-signal (explicitly
    -- saved by the user).
    --
    -- Implementation: two-pass — build a URL→index map (last-wins), then
    -- collect the surviving rows in insertion order (stable output).
    -- -----------------------------------------------------------------------
    local seen   = {}  -- url → index of surviving entry (last occurrence)
    local order  = {}  -- insertion-order list of unique URLs
    for i, rec in ipairs(suggestions) do
      local url = rec and rec.url
      if type(url) == "string" and url ~= "" then
        if seen[url] == nil then
          order[#order + 1] = url
        end
        seen[url] = i  -- last-wins: bookmark overwrites history
      end
    end

    local deduped = {}
    for _, url in ipairs(order) do
      deduped[#deduped + 1] = suggestions[seen[url]]
    end

    -- -----------------------------------------------------------------------
    -- Step 4: cap at 10 results (applied after dedup so the cap is on unique URLs).
    -- -----------------------------------------------------------------------
    local result = {}
    for i = 1, math.min(#deduped, 10) do
      result[i] = deduped[i]
    end
    return result
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
