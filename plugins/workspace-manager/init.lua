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
-- Phase-2 slice (W-A0): registers a single default workspace, fulfills the
-- full provider contract (list_workspaces + switch_workspace + on_change
-- handler), and drives the picker. The full multi-workspace management Lua
-- surface (mote.workspace.define declarations, workspace lifecycle hooks,
-- per-workspace pinned-tab management) comes in Phase 5.
--
-- ADR-0001: all hooks/events/api are declarative module-level tables.
-- setup() runs only after all four load-time validation steps pass.
-- No AI surfaces (DESIGN principle #8 / ADR-0002).
local M = {}

M.manifest = {
  schema = "v1",
  name = "workspace-manager",
  version = "0.1.0",

  -- Permissions needed for the Phase-2 slice.
  -- workspaces:list / workspaces:switch: enumerate and change workspaces.
  -- storage:persistent: persist active-workspace state across restarts.
  -- events:emit: notify the shell/chrome of workspace changes.
  -- events:on: receive workspace-related events from the shell (Phase 5).
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

-- `M.api` satisfies the `workspace:provider` contract:
--   required_api = ["list_workspaces", "switch_workspace"]
--
-- Phase-2 behavior: a single built-in "default" workspace. The Phase-5 slice
-- layers mote.workspace.define declarations on top of this foundation.
M.api = {
  -- list_workspaces() → array of workspace descriptors.
  --
  -- Phase-2: one default workspace. Phase-5: union of the built-in default
  -- and all `mote.workspace.define` declarations from init.lua.
  list_workspaces = function()
    return {
      { id = "default", name = "Default", active = true },
    }
  end,

  -- switch_workspace(id) → boolean (success).
  --
  -- Phase-2: stubs to the shell navigation seam. The host API for driving
  -- workspace switches (the actual tab-list swap) arrives with the shell in a
  -- later wave; we do not invent it here. Returns true to signal the intent
  -- was accepted; the shell mechanism carries out the actual switch.
  switch_workspace = function(_id)
    -- Phase-5: validate id against the registered workspace set, drive the
    -- tab-strip swap via the host workspace API, persist active-workspace to
    -- storage.
    return true
  end,
}

-- `M.events` satisfies the `workspace:provider` contract:
--   required_events = ["workspaces:on_change"]
--
-- workspaces:on_change fires when the active workspace or workspace set
-- changes (shell → plugin broadcast). The provider must declare the handler
-- so it can update its internal model when the shell advances state.
M.events = {
  ["workspaces:on_change"] = function(_change)
    -- Phase-2: no internal workspace state to update yet.
    -- Phase-5: reconcile the change event against the registered workspace
    -- set, update per-workspace active-tab pointers, emit any downstream
    -- events (e.g. picker refresh) via events.emit.
  end,
}

-- `M.hooks` is empty for the Phase-2 slice.
M.hooks = {}

function M.setup()
  -- Phase-2: persist the initial default workspace so the shell can recover
  -- active-workspace state across restarts. Storage write is under our granted
  -- storage:persistent permission.
  storage.set("active_workspace", "default")
  -- Phase-5: load mote.workspace.define declarations from config, register
  -- them in the workspace set, seed the picker.
end

return M
