-- e2e/config/init.lua — deterministic tarmac config for the e2e harness.
-- Kept minimal and fixed so assertions don't depend on a developer's personal
-- setup. Copied to ~/.config/tarmac/init.lua inside the VM (or used via
-- TARMAC_CONFIG in local mode) before the daemon starts.

gar.set("gap_inner", 8)
gar.set("gap_outer", 8)
gar.set("mod_key", "alt")
gar.set("focus_follows_mouse", false)
gar.set("mouse_follows_focus", false)

-- A predictable scratchpad for the AX-clamp / special-workspace checks.
gar.special_workspace("scratch", { width = 0.6, height = 0.6 })
