# Multi-Monitor Test Plan

Connect an external monitor, delete `~/.config/tarmac/init.lua` to regenerate defaults, then run `RUST_LOG=tarmac=info cargo run -p tarmac`.

## Startup Verification

- [ ] Log shows `displays discovered count=2`
- [ ] Log shows workspace 1 assigned to primary, workspace 2 to secondary
- [ ] Primary monitor tiles existing windows on workspace 1
- [ ] Secondary monitor shows workspace 2 (empty or with any windows that were there)

## Monitor Focus (Cmd+Comma / Cmd+Period)

- [ ] `Cmd+Period` focuses the next monitor (cursor warps, keyboard focus changes)
- [ ] `Cmd+Comma` focuses the previous monitor
- [ ] Focus cycling wraps around (2 monitors: next from second goes back to first)
- [ ] Status in `tarmacctl get-workspaces` reflects correct active workspace per monitor

## Workspace Switching Per-Monitor

- [ ] On primary: `Cmd+3` switches primary to workspace 3 (secondary stays on ws2)
- [ ] On secondary: `Cmd+4` switches secondary to workspace 4 (primary stays on ws1)
- [ ] **Swap test**: Primary on ws1, secondary on ws2. Focus primary, press `Cmd+2` → workspaces swap (primary gets ws2, secondary gets ws1)

## Move Window Between Monitors

- [ ] `Cmd+Shift+Period` moves focused window to next monitor's workspace
- [ ] `Cmd+Shift+Comma` moves focused window to previous monitor's workspace
- [ ] Layout recalculates on both monitors after move
- [ ] Focus follows the moved window to the target monitor

## Cross-Monitor Directional Navigation

- [ ] With windows on both monitors: `Cmd+L` at the right edge of left monitor → focus crosses to right monitor
- [ ] `Cmd+H` at the left edge of right monitor → focus crosses to left monitor
- [ ] Up/Down navigation stays within the current monitor

## Window Rules with Monitors

- [ ] `gar.rule({ app_name = "Safari" }, { workspace = 2 })` sends Safari to monitor 2's workspace

## Spawn Terminal

- [ ] `Cmd+Return` spawns terminal on the currently focused monitor
- [ ] New window tiles on the focused monitor's workspace, not the other monitor

## Display Hotplug

- [ ] Disconnect external monitor → windows from secondary workspace move to primary
- [ ] Reconnect external monitor → workspace 2 assigned to it, windows restored
- [ ] No crash on connect/disconnect

## Gaps and Layout

- [ ] Each monitor respects its own usable frame (dock/menu bar position)
- [ ] Gaps apply correctly on both monitors independently
- [ ] Smart splits work correctly on both monitors (respecting each monitor's aspect ratio)

## Edge Cases

- [ ] Single monitor (no external): everything works as before, no regression
- [ ] Three monitors: workspaces 1-3 assigned, all navigation works
- [ ] Rapidly switching monitors with `Cmd+Comma/Period` → no crash
- [ ] Floating window on one monitor, tiled on another → floating stays on its monitor
