# tarmac

A tiling window manager for macOS. BSP layout, Lua configuration, IPC control.

macOS Tahoe (15.0+), Apple Silicon.

## Install

```bash
brew install gardesk/tap/tarmac
```

Or build from source:

```bash
git clone --recurse-submodules git@github.com:gardesk/tarmac.git
cd tarmac
cargo build --release
```

Binaries land in `target/release/` — `tarmac`, `tarmacctl`, and `ers` (border renderer).

## Usage

```bash
tarmac                            # start the window manager
tarmacctl get-workspaces          # query workspace state
tarmacctl focus left              # focus window to the left
tarmacctl subscribe               # stream events (JSON)
```

Requires Accessibility permission: System Settings > Privacy & Security > Accessibility.

## Configuration

`~/.config/tarmac/init.lua` — created on first run.

```lua
gar.set("mod_key", "option")
gar.set("gap_inner", 8)
gar.set("gap_outer", 8)
gar.set("border_width", 4)

gar.bind("mod+h", "focus left")
gar.bind("mod+shift+h", "swap left")
gar.bind("mod+1", "workspace 1")

gar.rule({ app_name = "Calculator" }, { floating = true })
gar.on("workspace_changed", function(old, new)
    gar.exec("sketchybar --trigger tarmac_wkspc")
end)
```

Hot reload: `Mod+Shift+R`

## What it does

- BSP tiling with automatic split direction
- 10 numbered workspaces + scratchpads
- Multi-monitor with per-monitor workspaces
- Focus-follows-mouse, mouse-follows-focus
- Window rules (auto-float, workspace assignment)
- Window borders via ers (SkyLight overlays)
- IPC over Unix socket (JSON protocol)
- Lua event callbacks

## What it doesn't do

- No Wayland/Linux support — macOS only, uses private SkyLight framework
- No window animations
- No built-in bar — integrates with sketchybar or similar via IPC/callbacks
- Multi-monitor support has known rough edges (see docs)

## Documentation

**[tarmac.musicsian.com](https://tarmac.musicsian.com)** — configuration reference, keybindings, IPC protocol, window rules, troubleshooting.

## License

MIT
