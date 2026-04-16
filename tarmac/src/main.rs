use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;

use tarmac::config::document::{KeybindRow, ManagedConfigDocument, RuleRow, WorkspaceRow};
use tarmac::config::lua::{LuaKeybind, SpecialWorkspaceConfig, WindowRule};
use tarmac::core::input::{Action, Key, Modifiers};
use tarmac::core::state::WmState;
use tarmac::core::workspace::{WorkspaceDefinition, WorkspaceId, WorkspaceKind, WorkspaceTarget};
use tarmac::platform::event_tap::EventTap;
use tarmac::platform::hotkey::HotkeyManager;
use tarmac::platform::permissions;
use tarmac::platform::workspace_observer::WorkspacePollingObserver;
use tracing_subscriber::EnvFilter;

thread_local! {
    static WORKSPACE_POLLER: RefCell<Option<WorkspacePollingObserver>> = const { RefCell::new(None) };
    static WM_STATE: RefCell<Option<WmState>> = const { RefCell::new(None) };
    static IPC_RX: RefCell<Option<std::sync::mpsc::Receiver<tarmac::ipc::server::IpcCommand>>> = const { RefCell::new(None) };
    static EVENT_BUS: RefCell<Option<std::sync::Arc<tarmac::ipc::events::EventBus>>> = const { RefCell::new(None) };
    static LUA_CONFIG: RefCell<Option<tarmac::config::lua::LuaConfig>> = const { RefCell::new(None) };
    static HOTKEY_MGR: RefCell<Option<HotkeyManager>> = const { RefCell::new(None) };
    static CONFIG_PATH: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
    static TRAY: RefCell<Option<tarmac::ui::tray::TrayWidget>> = const { RefCell::new(None) };
    static SETTINGS_WIN: RefCell<Option<tarmac::ui::settings::SettingsWindow>> = const { RefCell::new(None) };
    static SETTINGS_SELECTED_KEYBIND_ID: RefCell<Option<String>> = const { RefCell::new(None) };
    static SETTINGS_SELECTED_RULE_ID: RefCell<Option<String>> = const { RefCell::new(None) };
    static SETTINGS_SELECTED_WORKSPACE_ID: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn main() {
    init_logging();
    tracing::info!("tarmac v{}", env!("CARGO_PKG_VERSION"));

    if !permissions::check_accessibility() {
        tracing::error!("accessibility permission not granted — exiting");
        tracing::info!("grant access in: System Settings → Privacy & Security → Accessibility");
        std::process::exit(1);
    }
    tracing::info!("accessibility permission granted");

    init_config_dir();
    install_signal_handlers();

    // Load Lua config
    // Use ~/.config/tarmac/init.lua (XDG-style, matching gar)
    let config_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".config")
        .join("tarmac")
        .join("init.lua");
    generate_default_config_if_missing(&config_path);
    let config = load_runtime_config(&config_path);
    tracing::info!(
        keybinds = config.keybinds.len(),
        mod_key = ?config.settings.mod_key,
        "config loaded"
    );

    // Initialize window manager state with config settings
    let mut state = WmState::new();
    state.focus_follows_mouse = config.settings.focus_follows_mouse;
    state.mouse_follows_focus = config.settings.mouse_follows_focus;
    state.gap_inner = config.settings.gap_inner;
    state.gap_outer = config.settings.gap_outer;
    state.bar_height = config.settings.bar_height;
    state.rules = config.rules.clone();
    state.special_configs = config.special_configs.clone();
    state.set_workspace_defs(config.workspace_defs.clone());
    state.borders.border_width = config.settings.border_width;
    state.borders.focused_color =
        tarmac::platform::border::BorderColor::from_hex(&config.settings.border_color_focused);
    state.borders.unfocused_color =
        tarmac::platform::border::BorderColor::from_hex(&config.settings.border_color_unfocused);
    state.borders.radius = config.settings.border_radius;
    state.borders.spawn();
    state.discover_and_observe();
    WM_STATE.with(|s| *s.borrow_mut() = Some(state));

    // Register hotkeys and store in thread-local for hot reload
    register_hotkeys_from_config(&config);

    // Store config path and Lua config for hot reload and event callbacks
    CONFIG_PATH.with(|p| *p.borrow_mut() = Some(config_path));
    LUA_CONFIG.with(|c| *c.borrow_mut() = Some(config));

    // CGEventTap for mouse events: click-to-focus and focus-follows-mouse
    let _event_tap = EventTap::install(Box::new(move |event| {
        use tarmac::platform::event_tap::MouseEvent;
        WM_STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                match event {
                    MouseEvent::Click { x, y } => state.click_to_focus(x, y),
                    MouseEvent::Moved { x, y } => {
                        if state.is_dragging() {
                            state.update_drag(x, y);
                        } else {
                            state.mouse_moved(x, y);
                        }
                    }
                    MouseEvent::LeftDown {
                        x,
                        y,
                        cmd_held: true,
                    } => state.begin_move_drag(x, y),
                    MouseEvent::LeftDragged { x, y } => {
                        if state.is_dragging() {
                            state.update_drag(x, y);
                        }
                    }
                    MouseEvent::LeftUp { .. } => state.end_drag(),
                    MouseEvent::RightDown {
                        x,
                        y,
                        cmd_held: true,
                    } => state.begin_resize_drag(x, y),
                    MouseEvent::RightDragged { x, y } => {
                        if state.is_dragging() {
                            state.update_drag(x, y);
                        }
                    }
                    MouseEvent::RightUp { .. } => state.end_drag(),
                    _ => {}
                }
            }
        });
    }));

    // Set up polling for new/closed windows and app terminations
    let poller = WorkspacePollingObserver::new(
        Box::new(move |pid, owner, wid| {
            WM_STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.on_new_window_detected(pid, &owner, wid);
                }
            });
        }),
        Box::new(move |wid, _pid| {
            WM_STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.on_window_closed(wid);
                }
            });
            publish_event(tarmac::ipc::events::WmEvent::WindowClosed { window_id: wid });
        }),
        Box::new(move |pid| {
            WM_STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.on_app_terminated(pid);
                }
            });
        }),
        Box::new(move |pid| {
            WM_STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.adopt_external_app_focus(pid, None);
                }
            });
        }),
        Box::new(move |wid| {
            WM_STATE.with(|s| {
                s.borrow()
                    .as_ref()
                    .is_some_and(|state| state.is_window_hidden(wid))
            })
        }),
    );
    WORKSPACE_POLLER.with(|p| *p.borrow_mut() = Some(poller));

    // Start IPC server
    let event_bus = std::sync::Arc::new(tarmac::ipc::events::EventBus::new());
    EVENT_BUS.with(|r| *r.borrow_mut() = Some(event_bus.clone()));
    let ipc_rx = tarmac::ipc::server::start_server(event_bus);
    IPC_RX.with(|r| *r.borrow_mut() = Some(ipc_rx));

    // Register display hotplug callback
    tarmac::platform::display::register_display_change_callback(Box::new(|| {
        tracing::info!("display configuration changed, refreshing monitors");
        WM_STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.refresh_monitors();
            }
        });
    }));

    install_polling_timer();
    run_app();
}

fn handle_action(action: Action) {
    // Reload borrows WM_STATE internally — handle outside the borrow
    if matches!(action, Action::Reload) {
        reload_config();
        return;
    }

    WM_STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            match action {
                Action::SpawnTerminal => spawn_terminal(),
                Action::CloseWindow => state.close_focused(),
                Action::Focus(dir) => {
                    state.focus_direction(dir);
                    if let Some(id) = state.active_workspace().focused {
                        let id_str = id.to_string();
                        fire_lua_event("window_focused", &[&id_str]);
                        let app_name = state
                            .registry
                            .get(id)
                            .map(|w| w.app_name.clone())
                            .unwrap_or_default();
                        publish_event(tarmac::ipc::events::WmEvent::WindowFocused {
                            window_id: id,
                            app_name,
                        });
                    }
                }
                Action::Swap(dir) => state.swap_direction(dir),
                Action::Resize(dir) => state.resize_direction(dir),
                Action::Equalize => state.equalize(),
                Action::Workspace(ref target) => {
                    let old = state.active_workspace().id.to_string();
                    state.switch_workspace(target);
                    let new = state.active_workspace().id.to_string();
                    fire_lua_event("workspace_changed", &[&old, &new]);
                    publish_event(tarmac::ipc::events::WmEvent::WorkspaceChanged {
                        old: old.clone(),
                        new: new.clone(),
                    });
                }
                Action::MoveToWorkspace(ref target) => state.move_to_workspace(target),
                Action::WorkspaceNext => {
                    let old = state.active_workspace().id.to_string();
                    state.workspace_next();
                    let new = state.active_workspace().id.to_string();
                    fire_lua_event("workspace_changed", &[&old, &new]);
                    publish_event(tarmac::ipc::events::WmEvent::WorkspaceChanged {
                        old: old.clone(),
                        new: new.clone(),
                    });
                }
                Action::WorkspacePrev => {
                    let old = state.active_workspace().id.to_string();
                    state.workspace_prev();
                    let new = state.active_workspace().id.to_string();
                    fire_lua_event("workspace_changed", &[&old, &new]);
                    publish_event(tarmac::ipc::events::WmEvent::WorkspaceChanged {
                        old: old.clone(),
                        new: new.clone(),
                    });
                }
                Action::ToggleFloat => state.toggle_float(),
                Action::Unstack => state.unstack_focused(),
                Action::ToggleSpecial(ref name) => state.toggle_special(name),
                Action::MoveToSpecial(ref name) => state.move_to_special(name),
                Action::FocusMonitorNext => {
                    state.focus_monitor_next();
                    let mid = state.focused_monitor.to_string();
                    fire_lua_event("monitor_focused", &[&mid]);
                    publish_event(tarmac::ipc::events::WmEvent::MonitorChanged {
                        index: state.focused_monitor,
                    });
                }
                Action::FocusMonitorPrev => {
                    state.focus_monitor_prev();
                    let mid = state.focused_monitor.to_string();
                    fire_lua_event("monitor_focused", &[&mid]);
                    publish_event(tarmac::ipc::events::WmEvent::MonitorChanged {
                        index: state.focused_monitor,
                    });
                }
                Action::MoveToMonitorNext => state.move_to_monitor_next(),
                Action::MoveToMonitorPrev => state.move_to_monitor_prev(),
                Action::Reload => unreachable!("handled above"),
                Action::Exit => {
                    tracing::info!("exit requested");
                    cleanup_socket();
                    std::process::exit(0);
                }
            }
        }
    });
}

fn spawn_terminal() {
    tracing::info!("spawning terminal");
    let command = LUA_CONFIG.with(|c| {
        c.borrow()
            .as_ref()
            .map(|config| config.settings.terminal_command.clone())
            .unwrap_or_else(|| "open -na WezTerm".to_string())
    });
    std::process::Command::new("/bin/sh")
        .args(["-c", &command])
        .spawn()
        .ok();
}

fn generate_default_config_if_missing(path: &std::path::Path) {
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let default_config = r##"-- tarmac configuration
-- ~/.config/tarmac/init.lua

-- Custom Lua outside the managed block is preserved and still executed.
-- The Settings window only rewrites the marked block below.

-- BEGIN TARMAC SETTINGS
-- Modifier key: "command", "option", or "control"
gar.set("mod_key", "command")

-- Gaps (pixels between windows and at screen edges)
gar.set("gap_inner", 8)
gar.set("gap_outer", 8)

-- Behavior
gar.set("focus_follows_mouse", "true")
gar.set("mouse_follows_focus", "true")

-- Terminal command
gar.set("terminal", "open -na WezTerm")

-- Keybindings
-- Format: gar.bind("modifiers+key", "action [args]")
-- "mod" resolves to the configured mod_key

gar.bind("mod+return", "spawn_terminal")
gar.bind("mod+shift+q", "close")
gar.bind("mod+e", "equalize")
gar.bind("mod+shift+space", "toggle_float")
gar.bind("mod+shift+u", "unstack")

-- Focus
gar.bind("mod+h", "focus left")
gar.bind("mod+j", "focus down")
gar.bind("mod+k", "focus up")
gar.bind("mod+l", "focus right")
gar.bind("mod+left", "focus left")
gar.bind("mod+down", "focus down")
gar.bind("mod+up", "focus up")
gar.bind("mod+right", "focus right")

-- Swap
gar.bind("mod+shift+h", "swap left")
gar.bind("mod+shift+j", "swap down")
gar.bind("mod+shift+k", "swap up")
gar.bind("mod+shift+l", "swap right")
gar.bind("mod+shift+left", "swap left")
gar.bind("mod+shift+down", "swap down")
gar.bind("mod+shift+up", "swap up")
gar.bind("mod+shift+right", "swap right")

-- Resize
gar.bind("mod+ctrl+h", "resize left")
gar.bind("mod+ctrl+j", "resize down")
gar.bind("mod+ctrl+k", "resize up")
gar.bind("mod+ctrl+l", "resize right")

-- Workspaces
for i = 1, 9 do
    gar.bind("mod+" .. i, "workspace " .. i)
    gar.bind("mod+shift+" .. i, "move_to_workspace " .. i)
end
gar.bind("mod+0", "workspace 10")
gar.bind("mod+shift+0", "move_to_workspace 10")

-- Workspace cycling
-- gar.bind("mod+tab", "workspace_next")
-- gar.bind("mod+shift+tab", "workspace_prev")

-- Monitor navigation
gar.bind("mod+comma", "focus_monitor_prev")
gar.bind("mod+period", "focus_monitor_next")
-- gar.bind("mod+shift+comma", "move_to_monitor_prev")
-- gar.bind("mod+shift+period", "move_to_monitor_next")

-- System
gar.bind("mod+shift+r", "reload")
-- gar.bind("mod+shift+e", "exit")

-- Window rules
-- gar.rule({ app_name = "Calculator" }, { floating = true })
-- gar.rule({ app_name = "System Settings" }, { floating = true })
-- gar.rule({ app_name = "Safari" }, { workspace = 2 })

-- Borders (set border_width > 0 to enable ers)
gar.set("border_width", 4)
gar.set("border_color_focused", "#5294e2")
gar.set("border_color_unfocused", "#59595980")
gar.set("border_radius", 10)

-- END TARMAC SETTINGS

-- Autostart (uncomment as needed)
-- gar.exec_once("sketchybar")
"##;

    match std::fs::write(path, default_config) {
        Ok(()) => tracing::info!(?path, "generated default config"),
        Err(e) => tracing::warn!(?path, err = %e, "failed to write default config"),
    }
}

fn install_polling_timer() {
    unsafe {
        let timer = CFRunLoopTimerCreate(
            ptr::null(),
            CFAbsoluteTimeGetCurrent() + 0.05,
            0.05, // 50ms — responsive event processing
            0,
            0,
            Some(poll_timer_callback),
            ptr::null_mut(),
        );
        let run_loop = CFRunLoopGetCurrent();
        CFRunLoopAddTimer(run_loop, timer, kCFRunLoopCommonModes);
    }
}

unsafe extern "C" fn poll_timer_callback(_timer: *const c_void) {
    WM_STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.process_events();
        }
    });

    WORKSPACE_POLLER.with(|p| {
        if let Some(poller) = p.borrow_mut().as_mut() {
            poller.poll();
        }
    });

    // Process IPC commands
    IPC_RX.with(|r| {
        if let Some(rx) = r.borrow().as_ref() {
            while let Ok(cmd) = rx.try_recv() {
                let response = process_ipc_command(&cmd.request);
                let _ = cmd.response_tx.send(response);
            }
        }
    });

    poll_tray_actions();
    TRAY.with(|t| {
        if let Some(tray) = t.borrow().as_ref() {
            update_tray(tray);
        }
    });

    poll_settings_actions();
}

fn process_ipc_command(
    request: &tarmac::ipc::protocol::Request,
) -> tarmac::ipc::protocol::Response {
    use tarmac::ipc::protocol::Response;

    // reload borrows WM_STATE internally — handle it outside the borrow
    if request.command == "reload" {
        reload_config();
        return Response::ok_empty();
    }

    WM_STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let Some(state) = state_ref.as_mut() else {
            return Response::err("no state");
        };

        match request.command.as_str() {
            "focus" => {
                if let Some(dir) = request.args.first().and_then(|a| parse_dir(a)) {
                    state.focus_direction(dir);
                    Response::ok_empty()
                } else {
                    Response::err("usage: focus left|right|up|down")
                }
            }
            "swap" => {
                if let Some(dir) = request.args.first().and_then(|a| parse_dir(a)) {
                    state.swap_direction(dir);
                    Response::ok_empty()
                } else {
                    Response::err("usage: swap left|right|up|down")
                }
            }
            "resize" => {
                if let Some(dir) = request.args.first().and_then(|a| parse_dir(a)) {
                    state.resize_direction(dir);
                    Response::ok_empty()
                } else {
                    Response::err("usage: resize left|right|up|down")
                }
            }
            "close" => {
                state.close_focused();
                Response::ok_empty()
            }
            "equalize" => {
                state.equalize();
                Response::ok_empty()
            }
            "unstack" => {
                state.unstack_focused();
                Response::ok_empty()
            }
            "workspace" => {
                if let Some(target) = request
                    .args
                    .first()
                    .and_then(|arg| WorkspaceTarget::parse(arg))
                {
                    state.switch_workspace(&target);
                    Response::ok_empty()
                } else {
                    Response::err("usage: workspace <1-10|A-Z>")
                }
            }
            "move-to-workspace" => {
                if let Some(target) = request
                    .args
                    .first()
                    .and_then(|arg| WorkspaceTarget::parse(arg))
                {
                    state.move_to_workspace(&target);
                    Response::ok_empty()
                } else {
                    Response::err("usage: move-to-workspace <1-10|A-Z>")
                }
            }
            "toggle-floating" => {
                state.toggle_float();
                Response::ok_empty()
            }
            "toggle-special" => {
                if let Some(name) = request.args.first() {
                    state.toggle_special(name);
                    Response::ok_empty()
                } else {
                    Response::err("usage: toggle-special <name>")
                }
            }
            "move-to-special" => {
                if let Some(name) = request.args.first() {
                    state.move_to_special(name);
                    Response::ok_empty()
                } else {
                    Response::err("usage: move-to-special <name>")
                }
            }
            "get-workspaces" => {
                let active_id = state.active_workspace().id.to_string();
                let mut ws_list: Vec<serde_json::Value> = state
                    .workspaces
                    .iter()
                    .map(|ws| {
                        serde_json::json!({
                            "id": ws.id.to_string(),
                            "active": ws.id.to_string() == active_id,
                            "windows": ws.all_window_ids().len(),
                            "focused": ws.focused,
                        })
                    })
                    .collect();
                ws_list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
                Response::ok(serde_json::json!({
                    "active": active_id,
                    "workspaces": ws_list,
                }))
            }
            "get-focused" => {
                use tarmac::platform::accessibility::{
                    ax_get_position, ax_get_size, ax_get_string,
                };
                if let Some(focused_id) = state.active_workspace().focused {
                    let app_name = state
                        .registry
                        .get(focused_id)
                        .map(|w| w.app_name.clone())
                        .unwrap_or_default();
                    let floating = state.active_workspace().is_floating(focused_id);
                    let stack = state.active_workspace().tree.stack_info(focused_id).map(
                        |(members, active)| {
                            serde_json::json!({
                                "members": members,
                                "active_index": active,
                                "active": members.get(active),
                            })
                        },
                    );

                    // Query live AX data for current title and geometry
                    let (title, x, y, width, height) =
                        if let Some(ax_ref) = state.get_ax_ref(focused_id) {
                            let title = ax_get_string(ax_ref, "AXTitle").unwrap_or_default();
                            let (x, y) = ax_get_position(ax_ref).unwrap_or((0.0, 0.0));
                            let (w, h) = ax_get_size(ax_ref).unwrap_or((0.0, 0.0));
                            (title, x, y, w, h)
                        } else {
                            (String::new(), 0.0, 0.0, 0.0, 0.0)
                        };

                    Response::ok(serde_json::json!({
                        "id": focused_id,
                        "app_name": app_name,
                        "title": title,
                        "x": x, "y": y,
                        "width": width, "height": height,
                        "floating": floating,
                        "stack": stack,
                        "workspace": state.active_workspace().id.to_string(),
                    }))
                } else {
                    Response::ok(serde_json::json!({ "focused": null }))
                }
            }
            "get-windows" => {
                let windows: Vec<serde_json::Value> = state
                    .registry
                    .all()
                    .map(|w| {
                        serde_json::json!({
                            "id": w.id,
                            "app_name": w.app_name,
                            "title": w.title,
                            "floating": w.floating,
                        })
                    })
                    .collect();
                Response::ok(serde_json::json!({ "windows": windows }))
            }
            "exec" => {
                if let Some(cmd) = request.args.first() {
                    std::process::Command::new("/bin/sh")
                        .args(["-c", cmd])
                        .spawn()
                        .ok();
                    Response::ok_empty()
                } else {
                    Response::err("usage: exec <command>")
                }
            }
            "get-monitors" => {
                let monitors: Vec<serde_json::Value> = state
                    .monitors
                    .iter()
                    .enumerate()
                    .map(|(i, m)| {
                        serde_json::json!({
                            "id": m.id,
                            "index": i,
                            "frame": {
                                "x": m.frame.x, "y": m.frame.y,
                                "width": m.frame.width, "height": m.frame.height,
                            },
                            "usable_frame": {
                                "x": m.usable_frame.x, "y": m.usable_frame.y,
                                "width": m.usable_frame.width, "height": m.usable_frame.height,
                            },
                            "is_primary": m.is_primary,
                            "active_workspace": m.active_workspace + 1,
                            "focused": i == state.focused_monitor,
                        })
                    })
                    .collect();
                Response::ok(serde_json::json!({ "monitors": monitors }))
            }
            "get-tree" => {
                let ws_idx =
                    if let Some(n) = request.args.first().and_then(|a| a.parse::<u8>().ok()) {
                        (n as usize).saturating_sub(1)
                    } else {
                        state.active_ws_idx()
                    };
                let ws = state.workspaces.get(ws_idx);
                let sr = state
                    .monitor_showing_workspace(ws_idx)
                    .map(|mi| state.monitor_rect(mi))
                    .unwrap_or_else(|| state.focused_rect());
                let (gap_inner, gap_outer) = state.workspace_gaps(ws_idx);
                let geoms = ws
                    .tree
                    .calculate_geometries_with_gaps(sr, gap_inner, gap_outer, true);
                let windows: Vec<serde_json::Value> = geoms
                    .iter()
                    .map(|(wid, rect)| {
                        let app_name = state
                            .registry
                            .get(*wid)
                            .map(|w| w.app_name.clone())
                            .unwrap_or_default();
                        let stack = ws.tree.stack_info(*wid);
                        let stack_index = stack
                            .as_ref()
                            .and_then(|(members, _)| members.iter().position(|id| *id == *wid));
                        let stack_active = stack
                            .as_ref()
                            .map(|(members, active)| members.get(*active) == Some(wid))
                            .unwrap_or(false);
                        serde_json::json!({
                            "id": wid,
                            "app_name": app_name,
                            "x": rect.x, "y": rect.y,
                            "width": rect.width, "height": rect.height,
                            "stacked": stack.is_some(),
                            "stack_index": stack_index,
                            "stack_active": stack_active,
                        })
                    })
                    .collect();
                let floating: Vec<serde_json::Value> = ws
                    .floating
                    .iter()
                    .map(|fw| {
                        let app_name = state
                            .registry
                            .get(fw.id)
                            .map(|w| w.app_name.clone())
                            .unwrap_or_default();
                        serde_json::json!({
                            "id": fw.id,
                            "app_name": app_name,
                            "x": fw.geometry.x, "y": fw.geometry.y,
                            "width": fw.geometry.width, "height": fw.geometry.height,
                            "floating": true,
                        })
                    })
                    .collect();
                Response::ok(serde_json::json!({
                    "workspace": ws.id.to_string(),
                    "tiled": windows,
                    "floating": floating,
                }))
            }
            other => Response::err(format!("unknown command: {}", other)),
        }
    })
}

fn parse_dir(s: &str) -> Option<tarmac::core::tree::Direction> {
    match s {
        "left" => Some(tarmac::core::tree::Direction::Left),
        "right" => Some(tarmac::core::tree::Direction::Right),
        "up" => Some(tarmac::core::tree::Direction::Up),
        "down" => Some(tarmac::core::tree::Direction::Down),
        _ => None,
    }
}

#[allow(non_upper_case_globals)]
unsafe extern "C" {
    fn CFAbsoluteTimeGetCurrent() -> f64;
    fn CFRunLoopGetCurrent() -> *const c_void;
    fn CFRunLoopTimerCreate(
        allocator: *const c_void,
        fire_date: f64,
        interval: f64,
        flags: u64,
        order: i64,
        callout: Option<unsafe extern "C" fn(*const c_void)>,
        context: *mut c_void,
    ) -> *const c_void;
    fn CFRunLoopAddTimer(run_loop: *const c_void, timer: *const c_void, mode: *const c_void);
    static kCFRunLoopCommonModes: *const c_void;
}

fn init_logging() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("tarmac=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn init_config_dir() {
    if let Some(home) = dirs::home_dir() {
        let tarmac_config = home.join(".config").join("tarmac");
        if !tarmac_config.exists() {
            if let Err(e) = std::fs::create_dir_all(&tarmac_config) {
                tracing::warn!("failed to create config dir: {}", e);
            } else {
                tracing::info!("created config directory at {:?}", tarmac_config);
            }
        }
    }
}

fn install_signal_handlers() {
    if let Err(e) = ctrlc::set_handler(|| {
        cleanup_socket();
        std::process::exit(0);
    }) {
        tracing::warn!("failed to set ctrl-c handler: {}", e);
    }
}

fn register_hotkeys_from_config(config: &tarmac::config::lua::LuaConfig) {
    // Drop old hotkey manager (unregisters all Carbon hotkeys)
    HOTKEY_MGR.with(|h| *h.borrow_mut() = None);

    let mut mgr = match HotkeyManager::new(Box::new(|action| {
        handle_action(action);
    })) {
        Some(m) => m,
        None => {
            tracing::error!("failed to create hotkey manager");
            return;
        }
    };

    for kb in &config.keybinds {
        mgr.register(kb.modifiers, kb.key, kb.action.clone());
    }
    tracing::info!(registered = config.keybinds.len(), "hotkeys registered");

    HOTKEY_MGR.with(|h| *h.borrow_mut() = Some(mgr));
}

fn load_runtime_config(path: &std::path::Path) -> tarmac::config::lua::LuaConfig {
    match ManagedConfigDocument::load(path) {
        Ok(doc) => {
            let mod_key = doc.effective.settings.mod_key;
            let resolved_keybinds = doc.resolved_keybinds(mod_key);
            let (resolved_workspace_defs, resolved_special_configs) =
                doc.resolved_workspace_defs_and_specials();
            let mut config = doc.effective;
            config.keybinds = resolved_keybinds;
            config.workspace_defs = resolved_workspace_defs;
            config.special_configs = resolved_special_configs;
            config
        }
        Err(err) => {
            tracing::warn!(
                ?path,
                err,
                "failed to load managed config document, falling back"
            );
            tarmac::config::lua::load_config(path)
        }
    }
}

fn reload_config() {
    let path = CONFIG_PATH.with(|p| p.borrow().clone());
    let Some(path) = path else {
        tracing::error!("no config path stored, cannot reload");
        return;
    };

    tracing::info!("reloading config...");
    let config = load_runtime_config(&path);

    // Update WmState settings
    WM_STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.focus_follows_mouse = config.settings.focus_follows_mouse;
            state.mouse_follows_focus = config.settings.mouse_follows_focus;
            state.gap_inner = config.settings.gap_inner;
            state.gap_outer = config.settings.gap_outer;
            state.bar_height = config.settings.bar_height;
            state.rules = config.rules.clone();
            state.special_configs = config.special_configs.clone();
            state.set_workspace_defs(config.workspace_defs.clone());
            state.borders.border_width = config.settings.border_width;
            state.borders.focused_color = tarmac::platform::border::BorderColor::from_hex(
                &config.settings.border_color_focused,
            );
            state.borders.unfocused_color = tarmac::platform::border::BorderColor::from_hex(
                &config.settings.border_color_unfocused,
            );
            state.borders.radius = config.settings.border_radius;
            state.borders.restart();
            // Reapply layout with potentially new gap/bar values
            state.apply_layout();
            state.update_borders();
        }
    });

    // Re-register hotkeys (drops old ones, registers new)
    register_hotkeys_from_config(&config);

    // Update Lua config for event callbacks.
    // Drop old config first to avoid RefCell borrow conflict during Lua state cleanup.
    LUA_CONFIG.with(|c| {
        let old = c.borrow_mut().take();
        drop(old);
        *c.borrow_mut() = Some(config);
    });

    refresh_settings_window();
    tracing::info!("config reloaded successfully");
}

fn fire_lua_event(event: &str, args: &[&str]) {
    LUA_CONFIG.with(|c| {
        if let Some(config) = c.borrow().as_ref() {
            config.fire_event(event, args);
        }
    });
}

fn publish_event(event: tarmac::ipc::events::WmEvent) {
    EVENT_BUS.with(|r| {
        if let Some(bus) = r.borrow().as_ref() {
            bus.publish(event);
        }
    });
}

fn cleanup_socket() {
    let path = tarmac::ipc::protocol::socket_path();
    let _ = std::fs::remove_file(&path);
}

fn workspace_last_active_title(
    workspace: &tarmac::core::workspace::Workspace,
    registry: &tarmac::core::window::WindowRegistry,
) -> Option<String> {
    workspace
        .focused
        .or_else(|| workspace.focus_history.last().copied())
        .and_then(|window_id| registry.get(window_id))
        .and_then(|window| {
            let title = window.title.trim();
            if !title.is_empty() {
                Some(title.to_string())
            } else {
                let app_name = window.app_name.trim();
                (!app_name.is_empty()).then(|| app_name.to_string())
            }
        })
}

fn update_tray(tray: &tarmac::ui::tray::TrayWidget) {
    WM_STATE.with(|s| {
        if let Some(state) = s.borrow().as_ref() {
            let active_id = state.active_workspace().id.to_string();
            let workspaces: Vec<tarmac::ui::tray::TrayWorkspace> = state
                .workspaces
                .iter()
                .filter_map(|ws| {
                    let switch_target = ws.id.as_regular_target();
                    if switch_target.is_none() && ws.all_window_ids().is_empty() {
                        return None;
                    }
                    Some(tarmac::ui::tray::TrayWorkspace {
                        id: ws.id.to_string(),
                        active: ws.visible,
                        windows: ws.all_window_ids().len(),
                        last_active_title: workspace_last_active_title(ws, &state.registry),
                        switch_target,
                    })
                })
                .collect();
            tray.update(&workspaces, &active_id);
        }
    });
}

fn poll_tray_actions() {
    TRAY.with(|t| {
        let borrow = t.borrow();
        let Some(tray) = borrow.as_ref() else { return };
        let actions = tray.poll_actions();
        drop(borrow);

        for action in actions {
            match action {
                tarmac::ui::tray::TrayAction::SwitchWorkspace(target) => {
                    handle_action(Action::Workspace(target));
                }
                tarmac::ui::tray::TrayAction::OpenSettings => open_settings(),
                tarmac::ui::tray::TrayAction::Reload => reload_config(),
                tarmac::ui::tray::TrayAction::Quit => {
                    cleanup_socket();
                    std::process::exit(0);
                }
            }
        }
    });
}

fn open_settings() {
    SETTINGS_WIN.with(|slot| {
        if slot.borrow().is_none() {
            let mtm = unsafe { objc2::MainThreadMarker::new_unchecked() };
            let win = tarmac::ui::settings::SettingsWindow::new(mtm);
            *slot.borrow_mut() = Some(win);
        }
        refresh_settings_window();
        if let Some(win) = slot.borrow().as_ref() {
            win.open_or_focus();
        }
    });
}

fn refresh_settings_window() {
    let Some(snapshot) = build_settings_snapshot() else {
        return;
    };
    SETTINGS_WIN.with(|slot| {
        if let Some(win) = slot.borrow().as_ref() {
            win.populate(&snapshot);
        }
    });
}

fn build_settings_snapshot() -> Option<tarmac::ui::settings::SettingsSnapshot> {
    let path = CONFIG_PATH.with(|p| p.borrow().clone())?;
    let doc = ManagedConfigDocument::load(&path).ok()?;
    let mod_key = doc.effective.settings.mod_key;
    let keybinds = doc.resolved_keybind_rows(mod_key);
    let previous_selected_keybind_id =
        SETTINGS_SELECTED_KEYBIND_ID.with(|slot| slot.borrow().clone());
    let selected_keybind_id =
        resolve_selected_keybind_id(&keybinds, previous_selected_keybind_id.as_deref());
    SETTINGS_SELECTED_KEYBIND_ID.with(|slot| *slot.borrow_mut() = selected_keybind_id.clone());

    let rules = doc.rule_rows();
    let previous_selected_rule_id = SETTINGS_SELECTED_RULE_ID.with(|slot| slot.borrow().clone());
    let selected_rule_id = resolve_selected_rule_id(&rules, previous_selected_rule_id.as_deref());
    SETTINGS_SELECTED_RULE_ID.with(|slot| *slot.borrow_mut() = selected_rule_id.clone());

    let workspaces = doc.workspace_rows();
    let previous_selected_workspace_id =
        SETTINGS_SELECTED_WORKSPACE_ID.with(|slot| slot.borrow().clone());
    let selected_workspace_id =
        resolve_selected_workspace_id(&workspaces, previous_selected_workspace_id.as_deref());
    SETTINGS_SELECTED_WORKSPACE_ID.with(|slot| *slot.borrow_mut() = selected_workspace_id.clone());
    let displays = WM_STATE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|state| {
                state
                    .monitors
                    .iter()
                    .enumerate()
                    .map(
                        |(index, monitor)| tarmac::ui::settings::WorkspaceDisplayOption {
                            display_id: monitor.id,
                            label: format!("Display {} ({})", index + 1, monitor.id),
                        },
                    )
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });

    Some(tarmac::ui::settings::SettingsSnapshot {
        gap_inner: doc.managed.settings.gap_inner,
        gap_outer: doc.managed.settings.gap_outer,
        bar_height: doc.managed.settings.bar_height,
        border_width: doc.managed.settings.border_width,
        border_radius: doc.managed.settings.border_radius,
        border_color_focused: doc.managed.settings.border_color_focused.clone(),
        border_color_unfocused: doc.managed.settings.border_color_unfocused.clone(),
        focus_follows_mouse: doc.managed.settings.focus_follows_mouse,
        mouse_follows_focus: doc.managed.settings.mouse_follows_focus,
        mod_key: match doc.managed.settings.mod_key {
            mods if mods == tarmac::core::input::Modifiers::OPTION => "option".to_string(),
            mods if mods == tarmac::core::input::Modifiers::CONTROL => "control".to_string(),
            _ => "command".to_string(),
        },
        keybinds,
        selected_keybind_id,
        rules,
        selected_rule_id,
        workspaces,
        selected_workspace_id,
        displays,
    })
}

fn resolve_selected_keybind_id(rows: &[KeybindRow], preferred_id: Option<&str>) -> Option<String> {
    if let Some(preferred_id) = preferred_id
        && rows.iter().any(|row| row.id == preferred_id)
    {
        return Some(preferred_id.to_string());
    }

    rows.iter()
        .find(|row| row.editable)
        .or_else(|| rows.first())
        .map(|row| row.id.clone())
}

fn fallback_selected_keybind_after_delete(rows: &[KeybindRow], deleted_id: &str) -> Option<String> {
    let deleted_index = rows.iter().position(|row| row.id == deleted_id)?;
    rows.get(deleted_index + 1)
        .or_else(|| {
            deleted_index
                .checked_sub(1)
                .and_then(|index| rows.get(index))
        })
        .map(|row| row.id.clone())
}

fn resolve_selected_rule_id(rows: &[RuleRow], preferred_id: Option<&str>) -> Option<String> {
    if let Some(preferred_id) = preferred_id
        && rows.iter().any(|row| row.id == preferred_id)
    {
        return Some(preferred_id.to_string());
    }

    rows.iter()
        .find(|row| row.editable)
        .or_else(|| rows.first())
        .map(|row| row.id.clone())
}

fn fallback_selected_rule_after_delete(rows: &[RuleRow], deleted_id: &str) -> Option<String> {
    let deleted_index = rows.iter().position(|row| row.id == deleted_id)?;
    rows.get(deleted_index + 1)
        .or_else(|| {
            deleted_index
                .checked_sub(1)
                .and_then(|index| rows.get(index))
        })
        .map(|row| row.id.clone())
}

fn resolve_selected_workspace_id(
    rows: &[WorkspaceRow],
    preferred_id: Option<&str>,
) -> Option<String> {
    if let Some(preferred_id) = preferred_id
        && rows.iter().any(|row| row.id == preferred_id)
    {
        return Some(preferred_id.to_string());
    }

    rows.iter()
        .find(|row| row.editable)
        .or_else(|| rows.first())
        .map(|row| row.id.clone())
}

fn fallback_selected_workspace_after_delete(
    rows: &[WorkspaceRow],
    deleted_id: &str,
) -> Option<String> {
    let deleted_row = rows.iter().find(|row| row.id == deleted_id)?;
    if deleted_row.kind == WorkspaceKind::Numbered {
        return Some(deleted_id.to_string());
    }

    let deleted_index = rows.iter().position(|row| row.id == deleted_id)?;
    rows.get(deleted_index + 1)
        .or_else(|| {
            deleted_index
                .checked_sub(1)
                .and_then(|index| rows.get(index))
        })
        .map(|row| row.id.clone())
}

fn managed_rule_index(rules: &[WindowRule], target_id: &str) -> Option<usize> {
    rules
        .iter()
        .enumerate()
        .find(|(index, rule)| rule.effective_id(*index) == target_id)
        .map(|(index, _)| index)
}

fn managed_keybind_index(keybinds: &[LuaKeybind], target_id: &str) -> Option<usize> {
    let index = target_id.strip_prefix("managed:")?.parse::<usize>().ok()?;
    (index < keybinds.len()).then_some(index)
}

fn keybind_shortcut_conflicts(keybinds: &[LuaKeybind], modifiers: Modifiers, key: Key) -> bool {
    keybinds
        .iter()
        .any(|keybind| keybind.modifiers == modifiers && keybind.key == key)
}

fn default_managed_keybind(keybinds: &[LuaKeybind], mod_key: Modifiers) -> LuaKeybind {
    for key in [
        Key::Period,
        Key::Comma,
        Key::Equal,
        Key::Minus,
        Key::Grave,
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::F,
        Key::G,
        Key::I,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ] {
        let modifiers = mod_key | Modifiers::SHIFT;
        if !keybind_shortcut_conflicts(keybinds, modifiers, key) {
            return LuaKeybind {
                modifiers,
                key,
                action: Action::Reload,
            };
        }
    }

    LuaKeybind {
        modifiers: mod_key | Modifiers::SHIFT,
        key: Key::Period,
        action: Action::Reload,
    }
}

fn add_managed_keybind(keybinds: &mut Vec<LuaKeybind>, mod_key: Modifiers) -> String {
    let keybind = default_managed_keybind(keybinds, mod_key);
    keybinds.push(keybind);
    format!("managed:{}", keybinds.len() - 1)
}

fn copy_keybind_to_managed(
    rows: &[KeybindRow],
    keybinds: &mut Vec<LuaKeybind>,
    target_id: &str,
) -> Option<String> {
    let row = rows
        .iter()
        .find(|row| row.id == target_id && !row.editable)?;
    keybinds.push(row.keybind.clone());
    Some(format!("managed:{}", keybinds.len() - 1))
}

fn delete_managed_keybind(keybinds: &mut Vec<LuaKeybind>, target_id: &str) -> bool {
    let Some(index) = managed_keybind_index(keybinds, target_id) else {
        return false;
    };
    keybinds.remove(index);
    true
}

fn replace_managed_keybind(
    keybinds: &mut [LuaKeybind],
    target_id: &str,
    replacement: LuaKeybind,
) -> bool {
    let Some(index) = managed_keybind_index(keybinds, target_id) else {
        return false;
    };
    keybinds[index] = replacement;
    true
}

fn next_managed_rule_id(rules: &[WindowRule]) -> String {
    let used = rules
        .iter()
        .enumerate()
        .map(|(index, rule)| rule.effective_id(index))
        .collect::<std::collections::BTreeSet<_>>();

    for candidate in 1..10_000 {
        let id = format!("rule_{candidate:03}");
        if !used.contains(&id) {
            return id;
        }
    }

    format!("rule_{}", used.len() + 1)
}

fn default_managed_rule(rules: &[WindowRule]) -> WindowRule {
    WindowRule {
        id: Some(next_managed_rule_id(rules)),
        name: Some("New Rule".to_string()),
        enabled: true,
        app_name: None,
        app_bundle: None,
        title: None,
        floating: None,
        workspace: None,
        geometry: None,
    }
}

fn add_managed_rule(rules: &mut Vec<WindowRule>) -> String {
    let rule = default_managed_rule(rules);
    let id = rule.id.clone().expect("default managed rule id missing");
    rules.push(rule);
    id
}

fn duplicate_managed_rule(rules: &mut Vec<WindowRule>, target_id: &str) -> Option<String> {
    let index = managed_rule_index(rules, target_id)?;
    let mut duplicate = rules.get(index)?.clone();
    let duplicate_id = next_managed_rule_id(rules);
    duplicate.id = Some(duplicate_id.clone());
    rules.insert(index + 1, duplicate);
    Some(duplicate_id)
}

fn delete_managed_rule(rules: &mut Vec<WindowRule>, target_id: &str) -> bool {
    let Some(index) = managed_rule_index(rules, target_id) else {
        return false;
    };
    rules.remove(index);
    true
}

fn move_managed_rule_up(rules: &mut [WindowRule], target_id: &str) -> bool {
    let Some(index) = managed_rule_index(rules, target_id) else {
        return false;
    };
    if index == 0 {
        return false;
    }
    rules.swap(index, index - 1);
    true
}

fn move_managed_rule_down(rules: &mut [WindowRule], target_id: &str) -> bool {
    let Some(index) = managed_rule_index(rules, target_id) else {
        return false;
    };
    if index + 1 >= rules.len() {
        return false;
    }
    rules.swap(index, index + 1);
    true
}

fn replace_managed_rule(
    rules: &mut [WindowRule],
    target_id: &str,
    mut replacement: WindowRule,
) -> bool {
    let Some(index) = managed_rule_index(rules, target_id) else {
        return false;
    };
    replacement.id = Some(target_id.to_string());
    rules[index] = replacement;
    true
}

fn toggle_managed_rule_enabled(rules: &mut [WindowRule], target_id: &str, enabled: bool) -> bool {
    let Some(index) = managed_rule_index(rules, target_id) else {
        return false;
    };
    if rules[index].id.is_none() {
        rules[index].id = Some(target_id.to_string());
    }
    rules[index].enabled = enabled;
    true
}

fn managed_workspace_def_index(defs: &[WorkspaceDefinition], target_id: &str) -> Option<usize> {
    let target = WorkspaceId::parse(target_id)?;
    defs.iter().position(|def| def.id == target)
}

fn managed_special_index(specials: &[SpecialWorkspaceConfig], target_id: &str) -> Option<usize> {
    let name = target_id.strip_prefix("special:")?;
    specials.iter().position(|special| special.name == name)
}

fn upsert_managed_workspace_def(
    defs: &mut Vec<WorkspaceDefinition>,
    definition: WorkspaceDefinition,
) {
    if let Some(index) = defs.iter().position(|def| def.id == definition.id) {
        defs[index] = definition;
    } else {
        defs.push(definition);
    }
}

fn upsert_managed_special_config(
    specials: &mut Vec<SpecialWorkspaceConfig>,
    special: SpecialWorkspaceConfig,
) {
    if let Some(index) = specials
        .iter()
        .position(|existing| existing.name == special.name)
    {
        specials[index] = special;
    } else {
        specials.push(special);
    }
}

fn add_managed_lettered_workspace(
    defs: &mut Vec<WorkspaceDefinition>,
    letter: &str,
) -> Option<String> {
    let id = WorkspaceId::parse(letter)?;
    if !matches!(id, WorkspaceId::Lettered(_)) {
        return None;
    }
    let definition = WorkspaceDefinition::new(id.clone());
    upsert_managed_workspace_def(defs, definition);
    Some(id.to_string())
}

fn add_managed_special_workspace(
    defs: &mut Vec<WorkspaceDefinition>,
    specials: &mut Vec<SpecialWorkspaceConfig>,
    name: &str,
) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let id = WorkspaceId::Special(name.to_string());
    upsert_managed_workspace_def(defs, WorkspaceDefinition::new(id.clone()));
    upsert_managed_special_config(specials, SpecialWorkspaceConfig::default_for(name));
    Some(id.to_string())
}

fn copy_workspace_to_managed(
    rows: &[WorkspaceRow],
    defs: &mut Vec<WorkspaceDefinition>,
    specials: &mut Vec<SpecialWorkspaceConfig>,
    target_id: &str,
) -> Option<String> {
    let row = rows
        .iter()
        .find(|row| row.id == target_id && !row.editable)?;
    upsert_managed_workspace_def(defs, row.definition.clone());
    if let Some(special) = row.special.clone() {
        upsert_managed_special_config(specials, special);
    }
    Some(row.id.clone())
}

fn delete_managed_workspace(
    rows: &[WorkspaceRow],
    defs: &mut Vec<WorkspaceDefinition>,
    specials: &mut Vec<SpecialWorkspaceConfig>,
    target_id: &str,
) -> bool {
    let Some(row) = rows.iter().find(|row| row.id == target_id && row.editable) else {
        return false;
    };

    let mut changed = false;
    if let Some(index) = managed_workspace_def_index(defs, target_id) {
        defs.remove(index);
        changed = true;
    }
    if row.kind == WorkspaceKind::Special
        && let Some(index) = managed_special_index(specials, target_id)
    {
        specials.remove(index);
        changed = true;
    }
    changed
}

fn replace_managed_workspace(
    defs: &mut Vec<WorkspaceDefinition>,
    specials: &mut Vec<SpecialWorkspaceConfig>,
    target_id: &str,
    draft: tarmac::ui::settings::WorkspaceDraft,
) -> bool {
    if target_id != draft.id {
        return false;
    }

    upsert_managed_workspace_def(defs, draft.to_definition());
    match draft.special_config() {
        Some(special) => upsert_managed_special_config(specials, special),
        None => {
            if let Some(index) = managed_special_index(specials, target_id) {
                specials.remove(index);
            }
        }
    }
    true
}

fn poll_settings_actions() {
    use tarmac::ui::settings::SettingsAction;

    SETTINGS_WIN.with(|slot| {
        let borrow = slot.borrow();
        let Some(win) = borrow.as_ref() else { return };
        let actions = win.poll_actions();
        if actions.is_empty() {
            return;
        }
        win.refresh_labels();
        drop(borrow);

        let Some(path) = CONFIG_PATH.with(|p| p.borrow().clone()) else {
            return;
        };
        let Ok(mut doc) = ManagedConfigDocument::load(&path) else {
            return;
        };

        let mut should_write = false;
        let should_refresh = false;
        let mut pending_keybind_draft = None;
        let mut pending_rule_draft = None;
        let mut pending_workspace_draft = None;
        for action in actions {
            match action {
                SettingsAction::GapInner(value) => {
                    doc.managed.settings.gap_inner = value;
                    should_write = true;
                }
                SettingsAction::GapOuter(value) => {
                    doc.managed.settings.gap_outer = value;
                    should_write = true;
                }
                SettingsAction::BarHeight(value) => {
                    doc.managed.settings.bar_height = value;
                    should_write = true;
                }
                SettingsAction::BorderWidth(value) => {
                    doc.managed.settings.border_width = value;
                    should_write = true;
                }
                SettingsAction::BorderRadius(value) => {
                    doc.managed.settings.border_radius = value;
                    should_write = true;
                }
                SettingsAction::BorderColorFocused(value) => {
                    doc.managed.settings.border_color_focused = value;
                    should_write = true;
                }
                SettingsAction::BorderColorUnfocused(value) => {
                    doc.managed.settings.border_color_unfocused = value;
                    should_write = true;
                }
                SettingsAction::FocusFollowsMouse(value) => {
                    doc.managed.settings.focus_follows_mouse = value;
                    should_write = true;
                }
                SettingsAction::MouseFollowsFocus(value) => {
                    doc.managed.settings.mouse_follows_focus = value;
                    should_write = true;
                }
                SettingsAction::ModKey(value) => {
                    doc.managed.settings.mod_key = match value.as_str() {
                        "option" => tarmac::core::input::Modifiers::OPTION,
                        "control" => tarmac::core::input::Modifiers::CONTROL,
                        _ => tarmac::core::input::Modifiers::COMMAND,
                    };
                    should_write = true;
                }
                SettingsAction::SelectKeybind(id) => {
                    SETTINGS_SELECTED_KEYBIND_ID.with(|slot| *slot.borrow_mut() = Some(id));
                }
                SettingsAction::AddKeybind => {
                    let id = add_managed_keybind(
                        &mut doc.managed.keybinds,
                        doc.managed.settings.mod_key,
                    );
                    SETTINGS_SELECTED_KEYBIND_ID.with(|slot| *slot.borrow_mut() = Some(id));
                    should_write = true;
                }
                SettingsAction::DeleteKeybind(id) => {
                    let fallback_id = fallback_selected_keybind_after_delete(
                        &doc.resolved_keybind_rows(doc.effective.settings.mod_key),
                        &id,
                    );
                    if delete_managed_keybind(&mut doc.managed.keybinds, &id) {
                        SETTINGS_SELECTED_KEYBIND_ID.with(|slot| *slot.borrow_mut() = fallback_id);
                        should_write = true;
                    }
                }
                SettingsAction::CopyKeybindToManaged(id) => {
                    let rows = doc.resolved_keybind_rows(doc.effective.settings.mod_key);
                    if let Some(new_id) =
                        copy_keybind_to_managed(&rows, &mut doc.managed.keybinds, &id)
                    {
                        SETTINGS_SELECTED_KEYBIND_ID.with(|slot| *slot.borrow_mut() = Some(new_id));
                        should_write = true;
                    }
                }
                SettingsAction::UpdateKeybindDraft(draft) => {
                    pending_keybind_draft = Some(draft);
                }
                SettingsAction::ApplyKeybind(id) => {
                    if let Some(draft) = pending_keybind_draft.take() {
                        match tarmac::config::lua::parse_keybind(
                            &draft.shortcut,
                            &draft.action,
                            doc.managed.settings.mod_key,
                        ) {
                            Ok(keybind) => {
                                if replace_managed_keybind(&mut doc.managed.keybinds, &id, keybind)
                                {
                                    SETTINGS_SELECTED_KEYBIND_ID
                                        .with(|slot| *slot.borrow_mut() = Some(id));
                                    should_write = true;
                                }
                            }
                            Err(err) => {
                                tracing::warn!(
                                    shortcut = draft.shortcut,
                                    action = draft.action,
                                    err,
                                    "failed to parse managed keybind"
                                );
                            }
                        }
                    }
                }
                SettingsAction::ResetManagedKeybinds => {
                    doc.managed.keybinds =
                        tarmac::config::lua::default_keybinds(&doc.managed.settings);
                    should_write = true;
                }
                SettingsAction::SelectRule(id) => {
                    SETTINGS_SELECTED_RULE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                }
                SettingsAction::AddRule => {
                    let id = add_managed_rule(&mut doc.managed.rules);
                    SETTINGS_SELECTED_RULE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                    should_write = true;
                }
                SettingsAction::DuplicateRule(id) => {
                    if let Some(new_id) = duplicate_managed_rule(&mut doc.managed.rules, &id) {
                        SETTINGS_SELECTED_RULE_ID.with(|slot| *slot.borrow_mut() = Some(new_id));
                        should_write = true;
                    }
                }
                SettingsAction::DeleteRule(id) => {
                    let fallback_id = fallback_selected_rule_after_delete(&doc.rule_rows(), &id);
                    if delete_managed_rule(&mut doc.managed.rules, &id) {
                        SETTINGS_SELECTED_RULE_ID.with(|slot| *slot.borrow_mut() = fallback_id);
                        should_write = true;
                    }
                }
                SettingsAction::MoveRuleUp(id) => {
                    if move_managed_rule_up(&mut doc.managed.rules, &id) {
                        SETTINGS_SELECTED_RULE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                        should_write = true;
                    }
                }
                SettingsAction::MoveRuleDown(id) => {
                    if move_managed_rule_down(&mut doc.managed.rules, &id) {
                        SETTINGS_SELECTED_RULE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                        should_write = true;
                    }
                }
                SettingsAction::UpdateRuleDraft(rule) => {
                    pending_rule_draft = Some(rule);
                }
                SettingsAction::ApplyRule(id) => {
                    if let Some(rule) = pending_rule_draft.take()
                        && replace_managed_rule(&mut doc.managed.rules, &id, rule)
                    {
                        SETTINGS_SELECTED_RULE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                        should_write = true;
                    }
                }
                SettingsAction::ToggleRuleEnabled(id, enabled) => {
                    if toggle_managed_rule_enabled(&mut doc.managed.rules, &id, enabled) {
                        SETTINGS_SELECTED_RULE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                        should_write = true;
                    }
                }
                SettingsAction::SelectWorkspace(id) => {
                    SETTINGS_SELECTED_WORKSPACE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                }
                SettingsAction::AddLetteredWorkspace(letter) => {
                    if let Some(id) =
                        add_managed_lettered_workspace(&mut doc.managed.workspace_defs, &letter)
                    {
                        SETTINGS_SELECTED_WORKSPACE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                        should_write = true;
                    }
                }
                SettingsAction::AddSpecialWorkspace(name) => {
                    if let Some(id) = add_managed_special_workspace(
                        &mut doc.managed.workspace_defs,
                        &mut doc.managed.special_configs,
                        &name,
                    ) {
                        SETTINGS_SELECTED_WORKSPACE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                        should_write = true;
                    }
                }
                SettingsAction::CopyWorkspaceToManaged(id) => {
                    let rows = doc.workspace_rows();
                    if let Some(new_id) = copy_workspace_to_managed(
                        &rows,
                        &mut doc.managed.workspace_defs,
                        &mut doc.managed.special_configs,
                        &id,
                    ) {
                        SETTINGS_SELECTED_WORKSPACE_ID
                            .with(|slot| *slot.borrow_mut() = Some(new_id));
                        should_write = true;
                    }
                }
                SettingsAction::DeleteWorkspace(id) => {
                    let rows = doc.workspace_rows();
                    let fallback_id = fallback_selected_workspace_after_delete(&rows, &id);
                    if delete_managed_workspace(
                        &rows,
                        &mut doc.managed.workspace_defs,
                        &mut doc.managed.special_configs,
                        &id,
                    ) {
                        SETTINGS_SELECTED_WORKSPACE_ID
                            .with(|slot| *slot.borrow_mut() = fallback_id);
                        should_write = true;
                    }
                }
                SettingsAction::UpdateWorkspaceDraft(draft) => {
                    pending_workspace_draft = Some(draft);
                }
                SettingsAction::ApplyWorkspace(id) => {
                    if let Some(draft) = pending_workspace_draft.take()
                        && replace_managed_workspace(
                            &mut doc.managed.workspace_defs,
                            &mut doc.managed.special_configs,
                            &id,
                            draft,
                        )
                    {
                        SETTINGS_SELECTED_WORKSPACE_ID.with(|slot| *slot.borrow_mut() = Some(id));
                        should_write = true;
                    }
                }
            }
        }

        if should_write && doc.write().is_ok() {
            reload_config();
            refresh_settings_window();
        } else if should_refresh {
            refresh_settings_window();
        }
    });
}

fn run_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_app_kit::NSApplicationActivationPolicy;

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let tray = tarmac::ui::tray::TrayWidget::new(mtm);
    update_tray(&tray);
    TRAY.with(|t| *t.borrow_mut() = Some(tray));

    tracing::info!("entering main event loop");
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tarmac::config::document::ConfigSource;
    use tarmac::core::window::{WindowRegistry, WindowState};

    fn sample_keybind(key: Key) -> LuaKeybind {
        LuaKeybind {
            modifiers: Modifiers::COMMAND | Modifiers::SHIFT,
            key,
            action: Action::Reload,
        }
    }

    fn sample_keybind_row(id: &str, editable: bool) -> KeybindRow {
        KeybindRow {
            id: id.to_string(),
            shortcut: "mod+shift+period".to_string(),
            action: "reload".to_string(),
            source: if editable {
                ConfigSource::Managed
            } else {
                ConfigSource::Lua
            },
            editable,
            keybind: sample_keybind(Key::Period),
        }
    }

    fn sample_rule(id: &str, name: &str) -> WindowRule {
        WindowRule {
            id: Some(id.to_string()),
            name: Some(name.to_string()),
            enabled: true,
            app_name: None,
            app_bundle: None,
            title: None,
            floating: None,
            workspace: None,
            geometry: None,
        }
    }

    fn sample_rule_row(id: &str, editable: bool) -> RuleRow {
        RuleRow {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            source: if editable {
                ConfigSource::Managed
            } else {
                ConfigSource::Lua
            },
            editable,
            rule: sample_rule(id, id),
        }
    }

    fn sample_workspace_row(id: &str, source: ConfigSource, kind: WorkspaceKind) -> WorkspaceRow {
        WorkspaceRow {
            id: id.to_string(),
            kind,
            source,
            editable: source == ConfigSource::Managed,
            definition: WorkspaceDefinition::new(WorkspaceId::parse(id).expect("invalid id")),
            special: (kind == WorkspaceKind::Special).then(|| SpecialWorkspaceConfig {
                name: id.trim_start_matches("special:").to_string(),
                position: "center".to_string(),
                width: 0.7,
                height: 0.7,
            }),
        }
    }

    fn sample_window(id: u32, app: &str, title: &str) -> WindowState {
        WindowState {
            id,
            app_pid: 100,
            app_name: app.to_string(),
            app_bundle_id: format!("com.test.{app}"),
            title: title.to_string(),
            role: "AXWindow".to_string(),
            subrole: "AXStandardWindow".to_string(),
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
            floating: false,
            minimized: false,
        }
    }

    #[test]
    fn add_keybind_uses_unique_shortcut() {
        let mut keybinds = vec![sample_keybind(Key::Period)];
        let id = add_managed_keybind(&mut keybinds, Modifiers::COMMAND);
        assert_eq!(id, "managed:1");
        assert_eq!(keybinds.len(), 2);
        assert_ne!(keybinds[0].key, keybinds[1].key);
        assert_eq!(keybinds[1].action, Action::Reload);
    }

    #[test]
    fn copy_keybind_to_managed_promotes_external_binding() {
        let rows = vec![sample_keybind_row("lua:0", false)];
        let mut keybinds = Vec::new();
        let id = copy_keybind_to_managed(&rows, &mut keybinds, "lua:0").expect("copy failed");
        assert_eq!(id, "managed:0");
        assert_eq!(keybinds.len(), 1);
        assert_eq!(keybinds[0], rows[0].keybind);
    }

    #[test]
    fn delete_keybind_selection_falls_forward_then_backward() {
        let rows = vec![
            sample_keybind_row("managed:0", true),
            sample_keybind_row("managed:1", true),
            sample_keybind_row("lua:0", false),
        ];
        assert_eq!(
            fallback_selected_keybind_after_delete(&rows, "managed:0").as_deref(),
            Some("managed:1")
        );
        assert_eq!(
            fallback_selected_keybind_after_delete(&rows, "lua:0").as_deref(),
            Some("managed:1")
        );
    }

    #[test]
    fn resolve_selected_keybind_prefers_existing_then_first_managed() {
        let rows = vec![
            sample_keybind_row("lua:0", false),
            sample_keybind_row("managed:0", true),
            sample_keybind_row("managed:1", true),
        ];
        assert_eq!(
            resolve_selected_keybind_id(&rows, Some("managed:1")).as_deref(),
            Some("managed:1")
        );
        assert_eq!(
            resolve_selected_keybind_id(&rows, Some("missing")).as_deref(),
            Some("managed:0")
        );
    }

    #[test]
    fn add_rule_uses_default_contents() {
        let mut rules = vec![sample_rule("rule_001", "Existing")];
        let id = add_managed_rule(&mut rules);
        let added = rules.last().expect("missing added rule");
        assert_eq!(id, "rule_002");
        assert_eq!(added.id.as_deref(), Some("rule_002"));
        assert_eq!(added.name.as_deref(), Some("New Rule"));
        assert!(added.enabled);
    }

    #[test]
    fn duplicate_rule_preserves_fields_but_reassigns_id() {
        let mut rules = vec![sample_rule("rule_001", "Browser")];
        let duplicated_id =
            duplicate_managed_rule(&mut rules, "rule_001").expect("duplicate failed");
        assert_eq!(duplicated_id, "rule_002");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1].id.as_deref(), Some("rule_002"));
        assert_eq!(rules[1].name, rules[0].name);
    }

    #[test]
    fn move_rule_up_and_down_keep_order_stable() {
        let mut rules = vec![
            sample_rule("rule_001", "One"),
            sample_rule("rule_002", "Two"),
            sample_rule("rule_003", "Three"),
        ];
        assert!(move_managed_rule_up(&mut rules, "rule_003"));
        assert_eq!(rules[1].id.as_deref(), Some("rule_003"));
        assert!(move_managed_rule_down(&mut rules, "rule_003"));
        assert_eq!(rules[2].id.as_deref(), Some("rule_003"));
    }

    #[test]
    fn delete_rule_selection_falls_forward_then_backward() {
        let rows = vec![
            sample_rule_row("rule_001", true),
            sample_rule_row("rule_002", true),
            sample_rule_row("lua_rule", false),
        ];
        assert_eq!(
            fallback_selected_rule_after_delete(&rows, "rule_001").as_deref(),
            Some("rule_002")
        );
        assert_eq!(
            fallback_selected_rule_after_delete(&rows, "lua_rule").as_deref(),
            Some("rule_002")
        );
    }

    #[test]
    fn resolve_selected_rule_prefers_existing_then_first_managed() {
        let rows = vec![
            sample_rule_row("lua_rule", false),
            sample_rule_row("rule_001", true),
            sample_rule_row("rule_002", true),
        ];
        assert_eq!(
            resolve_selected_rule_id(&rows, Some("rule_002")).as_deref(),
            Some("rule_002")
        );
        assert_eq!(
            resolve_selected_rule_id(&rows, Some("missing")).as_deref(),
            Some("rule_001")
        );
    }

    #[test]
    fn add_lettered_workspace_normalizes_to_uppercase() {
        let mut defs = Vec::new();
        let id = add_managed_lettered_workspace(&mut defs, "w").expect("workspace add failed");
        assert_eq!(id, "W");
        assert_eq!(defs[0].id, WorkspaceId::Lettered('W'));
    }

    #[test]
    fn add_special_workspace_rejects_empty_name() {
        let mut defs = Vec::new();
        let mut specials = Vec::new();
        assert!(add_managed_special_workspace(&mut defs, &mut specials, "   ").is_none());
        assert!(defs.is_empty());
        assert!(specials.is_empty());
    }

    #[test]
    fn delete_workspace_selection_keeps_numbered_row_selected() {
        let rows = vec![
            sample_workspace_row("1", ConfigSource::Managed, WorkspaceKind::Numbered),
            sample_workspace_row("A", ConfigSource::Managed, WorkspaceKind::Lettered),
        ];
        assert_eq!(
            fallback_selected_workspace_after_delete(&rows, "1").as_deref(),
            Some("1")
        );
        assert_eq!(
            fallback_selected_workspace_after_delete(&rows, "A").as_deref(),
            Some("1")
        );
    }

    #[test]
    fn copy_workspace_to_managed_promotes_inherited_row() {
        let rows = vec![sample_workspace_row(
            "special:term",
            ConfigSource::Lua,
            WorkspaceKind::Special,
        )];
        let mut defs = Vec::new();
        let mut specials = Vec::new();
        let id = copy_workspace_to_managed(&rows, &mut defs, &mut specials, "special:term")
            .expect("copy failed");
        assert_eq!(id, "special:term");
        assert_eq!(defs[0].id, WorkspaceId::Special("term".to_string()));
        assert_eq!(specials[0].name, "term");
    }

    #[test]
    fn workspace_last_active_title_prefers_window_title_then_app_name() {
        let mut registry = WindowRegistry::new();
        registry.add(sample_window(1, "Firefox", "New Tab"));
        registry.add(sample_window(2, "Firefox", ""));

        let mut workspace = tarmac::core::workspace::Workspace::new(WorkspaceId::Numbered(1));
        workspace.focused = Some(1);
        assert_eq!(
            workspace_last_active_title(&workspace, &registry).as_deref(),
            Some("New Tab")
        );

        workspace.focused = Some(2);
        assert_eq!(
            workspace_last_active_title(&workspace, &registry).as_deref(),
            Some("Firefox")
        );

        workspace.focused = Some(99);
        workspace.focus_history.clear();
        assert_eq!(workspace_last_active_title(&workspace, &registry), None);
    }
}
