-- Mote bundled first-party plugin: bookmarks
--
-- Fulfills the `ui:bookmarks_provider` capability (exclusive, critical).
-- Contract (v1.toml): required_api = ["list_bookmarks", "add_bookmark",
--   "remove_bookmark"], required_events = [].
--
-- POLICY FLOOR (docs/plans/02-browser-shell.md §8):
-- The shell owns the bookmark MECHANISM (toolbar star, sidebar panel).
-- This plugin owns the bookmark POLICY: what a bookmark is, keyed storage,
-- and the urlbar:suggest collector contribution (Phase 5b/B3).
--
-- Data model (docs/plans/2026-05-30-phase5a-core-providers.md §Unknown-2):
-- One KV entry per bookmark, key = "b:" .. url, value = a pipe-delimited
-- record "url|title|added_counter".  Keying by URL makes add_bookmark
-- idempotent (bookmarking the same page twice = one entry).
--
-- NOTE: os.time is NOT available in the Mote Lua sandbox (os module is
-- excluded for security).  The "added" field is a monotonic integer counter
-- maintained in storage under the key "_seq".
--
-- NOTE: No JSON encoder is exposed to the sandbox.  Records are encoded as a
-- pipe-delimited string: "url|title|counter".  Pipes in url or title are
-- percent-encoded (%7C) to avoid ambiguity.
--
-- ADR-0001: all hooks/events/api are declarative module-level tables.
-- setup() runs only after all four load-time validation steps pass.
-- No AI surfaces (DESIGN principle #8 / ADR-0002).
local M = {}

M.manifest = {
  schema = "v1",
  name = "bookmarks",
  version = "0.1.0",

  -- storage:persistent: to store and retrieve bookmark records.
  -- bookmarks:read: enumerate and read bookmarks.
  -- bookmarks:write: create or remove bookmarks.
  -- events:on: to subscribe to urlbar:suggest (added in B3); declared now.
  -- events:emit: to emit events (e.g. notify of bookmark changes).
  permissions = {
    "storage:persistent",
    "bookmarks:read",
    "bookmarks:write",
    "events:on",
    "events:emit",
  },

  capabilities = {
    "ui:bookmarks_provider",
  },

  identity_scope = "per_identity",
}

-- ---------------------------------------------------------------------------
-- Internal helpers
-- ---------------------------------------------------------------------------

--- Percent-encode pipe characters in a string so they never collide with the
--- field separator used by the record codec.
local function encode_field(s)
  if s == nil then return "" end
  return tostring(s):gsub("|", "%%7C")
end

--- Percent-decode a field read from storage.
local function decode_field(s)
  if s == nil then return "" end
  return s:gsub("%%7C", "|")
end

--- Encode a bookmark record to the storage string format.
--- Format: "url|title|added_counter"
local function encode_record(url, title, added)
  return encode_field(url) .. "|" .. encode_field(title) .. "|" .. tostring(added)
end

--- Decode a storage string back to a record table.
--- Returns nil if the string is malformed.
local function decode_record(s)
  if s == nil or s == "" then return nil end
  -- Split on the FIRST two pipes only (url and title may be empty but not nil)
  local url_enc, rest = s:match("^([^|]*)|(.*)$")
  if url_enc == nil then return nil end
  local title_enc, added_str = rest:match("^([^|]*)|(.*)$")
  if title_enc == nil then return nil end
  local added = tonumber(added_str) or 0
  return {
    url   = decode_field(url_enc),
    title = decode_field(title_enc),
    added = added,
  }
end

--- Read the next monotonic sequence number from storage and advance it.
local function next_seq()
  local raw = storage.get("_seq")
  local n = tonumber(raw) or 0
  n = n + 1
  storage.set("_seq", tostring(n))
  return n
end

-- ---------------------------------------------------------------------------
-- `M.api` — satisfies the `ui:bookmarks_provider` contract:
--   required_api = ["list_bookmarks", "add_bookmark", "remove_bookmark"]
-- ---------------------------------------------------------------------------

M.api = {
  --- add_bookmark(arg) — arg = { url = <string>, title = <string|nil> }
  --- Idempotent: bookmarking the same URL twice updates the title (the URL
  --- is the identity key).  Returns the stored record on success.
  add_bookmark = function(arg)
    if arg == nil or arg.url == nil or arg.url == "" then
      return nil
    end
    local url   = tostring(arg.url)
    local title = arg.title and tostring(arg.title) or ""
    local key   = "b:" .. url

    -- Preserve the original added counter if the entry already exists.
    local existing = decode_record(storage.get(key))
    local added = existing and existing.added or next_seq()

    local encoded = encode_record(url, title, added)
    storage.set(key, encoded)
    return { url = url, title = title, added = added }
  end,

  --- remove_bookmark(arg) — arg = { url = <string> }
  --- Removes the bookmark keyed by url.  No-op if the bookmark does not exist.
  remove_bookmark = function(arg)
    if arg == nil or arg.url == nil then return false end
    storage.delete("b:" .. tostring(arg.url))
    return true
  end,

  --- list_bookmarks(filter) — filter is an optional substring query string.
  --- Returns an array of record tables { url, title, added }.
  --- If filter is a non-empty string, only records whose url or title
  --- contains the filter (case-sensitive) are returned.
  list_bookmarks = function(filter)
    local results = {}
    local keys = storage.list_keys()
    for _, key in ipairs(keys) do
      -- Only process bookmark keys.
      if key:sub(1, 2) == "b:" then
        local raw = storage.get(key)
        local rec = decode_record(raw)
        if rec ~= nil then
          local include = true
          if filter ~= nil and filter ~= "" then
            local f = tostring(filter)
            include = rec.url:find(f, 1, true) ~= nil
              or rec.title:find(f, 1, true) ~= nil
          end
          if include then
            results[#results + 1] = rec
          end
        end
      end
    end
    return results
  end,
}

-- ---------------------------------------------------------------------------
-- `M.events` — collector contributor for the urlbar:suggest surface.
-- ---------------------------------------------------------------------------

M.events = {
  --- urlbar:suggest contributor (ADR-0010, Task B3).
  ---
  --- Called by the `collect` path when an exclusive urlbar provider (history)
  --- gathers contributions.  Returns a 1-indexed Lua array of suggestion
  --- records, each tagged `source="bookmark"`, or an empty table when the
  --- text is empty or nothing matches.
  ---
  --- Matching inherits `list_bookmarks`'s case-sensitive substring search
  --- (both url and title).  Case-sensitivity is a known v0.1 limitation —
  --- a future mote.text host API can add Unicode folding; changing it here
  --- requires no history change (the collector pattern is open by design).
  ["urlbar:suggest"] = function(payload)
    local text = payload and payload.text or ""
    if text == "" then
      return {}
    end
    -- Delegate to the plugin's own list_bookmarks for filtering — avoids
    -- duplicating the key-prefix and codec logic.
    local matches = M.api.list_bookmarks(text)
    local result = {}
    for _, rec in ipairs(matches) do
      result[#result + 1] = {
        url    = rec.url,
        title  = rec.title,
        source = "bookmark",
      }
    end
    return result
  end,
}

-- `M.hooks` is empty.
M.hooks = {}

function M.setup()
  -- Nothing to initialize for the Phase-5a slice.  Storage is written
  -- on demand by the API functions above.
end

return M
