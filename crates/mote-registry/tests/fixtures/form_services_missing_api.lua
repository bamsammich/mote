-- Negative fixture: claims `password-manager-form-services` but omits the
-- required `inject_isolated` API function. Step 1 passes (all terms known);
-- step 3 must fail with a MissingApi error naming `inject_isolated`.
local M = {}

M.manifest = {
  schema = "v1",
  name = "broken-form-services-plugin",
  version = "1.0.0",

  permissions = {
    "page:read_dom",
  },

  capabilities = {
    "password-manager-form-services",
  },
}

M.api = {
  show_autofill_picker = function(items) return items[1] end,
  -- inject_isolated intentionally absent.
}

M.hooks = {
  ["page:on_load"] = function(_p) end,
}

return M
