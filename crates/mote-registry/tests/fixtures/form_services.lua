-- Contract-conformance fixture: a minimal valid v1 plugin that fulfills the
-- `password-manager-form-services` capability and exercises representative
-- permissions across several domains and resource shapes.
--
-- DISCIPLINES §2: this is the per-version minimal plugin that step-1 + step-3
-- must accept. It declares the required API (`show_autofill_picker`,
-- `inject_isolated`) and the required event handler (`page:on_load`).
local M = {}

M.manifest = {
  schema = "v1",
  name = "password-manager-form-services-plugin",
  version = "1.0.0",

  permissions = {
    "page:read_dom",                                  -- glob, implicit *
    "page:inject_script:*",                           -- glob, explicit wildcard
    "page:inject_script:https://*.example.com/*",     -- glob, specific origin
    "ui:sidebar",                                     -- none
    "storage:persistent",                             -- none
    "net:intercept_request:!*.banking.com",           -- glob, deny
    "secret:read:anthropic_api_key",                  -- dynamic resource
    "mcp:client:internal-tools",                      -- dynamic resource
  },

  capabilities = {
    "password-manager-form-services",
  },
}

M.api = {
  show_autofill_picker = function(items) return items[1] end,
  inject_isolated = function(_script, _world) end,
  -- An extra, vendor-specific function beyond the contract (loose contracts).
  vendor_extra = function() end,
}

M.hooks = {
  ["page:on_load"] = function(_p) end,
}

function M.setup()
  -- Never called during step-1/step-3 validation.
  error("setup must not run during conformance checks")
end

return M
