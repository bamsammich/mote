-- Mote bundled first-party plugin: workspace-manager
--
-- Fulfills the `workspace:provider` capability (exclusive, critical).
-- Contract (v1.toml):
--   required_api    = ["list_workspaces", "switch_workspace"]
--   required_events = ["workspaces:on_change"]
--
-- POLICY FLOOR (docs/plans/02-browser-shell.md §8):
-- The shell owns the workspace MECHANISM (tab-list model, session persistence,
-- the workspace picker widget seam). This plugin owns the workspace POLICY:
-- workspace definition, switching decisions, per-workspace state management,
-- and the Lua-driven `mote.workspace.define` surface.
--
-- Phase 5a slice (E2): built-in workspace set with real persistence and
-- switch validation.  `switch_workspace` validates the id against the
-- built-in set, persists the active selection via storage, and emits
-- `workspaces:on_change` so the shell (E3) can swap the visible tab strip.
-- `list_workspaces` returns the set with the persisted active entry flagged.
-- Survives runtime reload.
--
-- Built-in workspace set (v0.1 — defined here for Phase 5a demonstrability):
--   { id = "default", name = "Default" }
--   { id = "work",    name = "Work"    }
-- The Phase-6+ `mote.workspace.define` config-Lua surface will let users
-- extend and customize this set; that surface is out of scope for E2.
--
-- identity_scope = "global": workspace definitions are cross-identity per
-- DESIGN §Workspace — a deliberate divergence from the per_identity default
-- used by bookmarks and history.
--
-- Storage convention: bare `storage.*` (not `mote.storage.*`) — this matches
-- the canonical form used by the working bookmarks and history plugins.
--
-- ADR-0001: all hooks/events/api are declarative module-level tables.
-- setup() runs only after all four load-time validation steps pass.
-- No AI surfaces (DESIGN principle #8 / ADR-0002).
local M = {}

M.manifest = {
  schema = "v1",
  name = "workspace-manager",
  version = "0.1.0",

  -- workspaces:list / workspaces:switch: enumerate and change workspaces.
  -- storage:persistent: persist active-workspace state across restarts.
  -- events:emit: notify the shell/chrome of workspace changes (on_change).
  -- events:on:   receive workspace-related events from the shell (Phase 5+).
  permissions = {
    "workspaces:list",
    "workspaces:switch",
    "storage:persistent",
    "events:emit",
    "events:on",
  },

  capabilities = {
    "workspace:provider",
  },

  identity_scope = "global",
}

-- ---------------------------------------------------------------------------
-- Built-in workspace set (v0.1)
--
-- Indexed array of records { id, name }.  Order is significant: the first
-- entry is the default active workspace when no persisted value exists.
-- The Phase-6+ `mote.workspace.define` surface will extend this set;
-- that is out of scope for Phase 5a.
-- ---------------------------------------------------------------------------
local BUILTIN_WORKSPACES = {
  { id = "default", name = "Default" },
  { id = "work",    name = "Work"    },
}

-- ---------------------------------------------------------------------------
-- Internal helpers
-- ---------------------------------------------------------------------------

--- Return true if `id` is in the built-in workspace set.
local function is_valid_id(id)
  for _, ws in ipairs(BUILTIN_WORKSPACES) do
    if ws.id == id then return true end
  end
  return false
end

--- Return the persisted active workspace id, defaulting to the first built-in
--- workspace if the stored value is absent, empty, or not in the built-in set.
--- If the stored value is not in the set (e.g. left over from an older version
--- of the built-in list), we fall back silently to the first entry rather than
--- treating it as a hard error — the caller can log if needed.
local function active_id()
  local stored = storage.get("active_workspace")
  if stored ~= nil and stored ~= "" and is_valid_id(stored) then
    return stored
  end
  -- Fall back to the first entry in the built-in list.
  return BUILTIN_WORKSPACES[1].id
end

-- ---------------------------------------------------------------------------
-- `M.api` satisfies the `workspace:provider` contract:
--   required_api = ["list_workspaces", "switch_workspace"]
-- ---------------------------------------------------------------------------

M.api = {
  --- list_workspaces() → array of workspace descriptors.
  ---
  --- Returns a 1-indexed array of { id, name, active } records.  Exactly one
  --- record has `active = true` (the persisted active workspace, or the first
  --- built-in workspace as a fallback if no valid persisted value exists).
  list_workspaces = function()
    local cur = active_id()
    local result = {}
    for i, ws in ipairs(BUILTIN_WORKSPACES) do
      result[i] = {
        id     = ws.id,
        name   = ws.name,
        active = (ws.id == cur),
      }
    end
    return result
  end,

  --- switch_workspace(payload) → boolean (success).
  ---
  --- `payload` is a Map `{ id = <string> }` — matching the bookmarks-style
  --- convention (the shell will produce a HostValue::Map when calling this via
  --- Rust's invoke_capability).  A bare string is also accepted for robustness
  --- (L2: the host→Lua arg is whatever the caller passes as its first arg).
  ---
  --- Validates that `id` is in the built-in workspace set.  If the id is not
  --- valid, returns `false` without persisting or emitting.
  ---
  --- On success:
  ---   1. Persists the new active id via storage.
  ---   2. Emits `workspaces:on_change` with `{ active = id }` so the shell
  ---      (E3) can swap the visible tab strip.
  ---   3. Returns `true`.
  switch_workspace = function(payload)
    -- Normalize: accept Map { id = ... } OR bare string (robustness).
    local id
    if type(payload) == "table" then
      id = payload.id
    elseif type(payload) == "string" then
      id = payload
    end

    if id == nil or type(id) ~= "string" or id == "" then
      return false
    end

    -- Validate against the built-in set.
    if not is_valid_id(id) then
      return false
    end

    -- Persist the new active workspace.
    storage.set("active_workspace", id)

    -- Emit the change event so the shell can react (workspaces:on_change is
    -- declared broadcast in the registry).
    mote.events.emit("workspaces:on_change", { active = id })

    return true
  end,
}

-- ---------------------------------------------------------------------------
-- `M.events` satisfies the `workspace:provider` contract:
--   required_events = ["workspaces:on_change"]
--
-- The provider declares a handler for `workspaces:on_change` as required by
-- the capability contract.  The provider itself does not need to react to the
-- event it emits — the shell (E3) is the mechanism consumer.  The handler is a
-- no-op here; it exists to satisfy the registry conformance check (step 3).
-- ---------------------------------------------------------------------------
M.events = {
  ["workspaces:on_change"] = function(_change)
    -- No-op: the provider emits this event; it does not need to act on it.
    -- The shell (E3) observes the event and swaps the visible tab strip.
    -- If per-workspace state beyond active_workspace is added in Phase 6+,
    -- reconciliation logic belongs here.
  end,
}

-- `M.hooks` is empty.
M.hooks = {}

function M.setup()
  -- Seed `active_workspace` ONLY when no value has been persisted yet.
  -- This avoids overwriting the user's active selection on reload.
  local stored = storage.get("active_workspace")
  if stored == nil or stored == "" then
    storage.set("active_workspace", BUILTIN_WORKSPACES[1].id)
  end
  -- Phase 6+: load mote.workspace.define declarations from config here.
end

return M
