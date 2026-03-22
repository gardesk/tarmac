use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;

use tarmac::core::input::Action;
use tarmac::core::state::WmState;
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
    let config = tarmac::config::lua::load_config(&config_path);
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
                let prev_focused = state.active_workspace().focused;
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
                // Publish focus change from FFM or click-to-focus
                let new_focused = state.active_workspace().focused;
                if new_focused != prev_focused
                    && let Some(id) = new_focused
                {
                    if let Some(data) = state.window_focus_data(id) {
                        fire_lua_event_data("window_focused", &data);
                    }
                    if let Some(evt) = state.window_focused_event(id) {
                        publish_event(evt);
                    }
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
                    if let Some(w) = state.registry.get(wid) {
                        let ws_id = state
                            .workspaces
                            .find_window(wid)
                            .map(|idx| state.workspaces.get(idx).id.to_string())
                            .unwrap_or_default();
                        publish_event(tarmac::ipc::events::WmEvent::WindowCreated {
                            window_id: wid,
                            title: w.title.clone(),
                            app_name: w.app_name.clone(),
                            app_bundle: w.app_bundle_id.clone(),
                            workspace: ws_id,
                        });
                        if let Some(data) = state.window_created_data(wid) {
                            fire_lua_event_data("window_created", &data);
                        }
                        let layout_data = state.layout_changed_data();
                        publish_event(state.layout_changed_event());
                        fire_lua_event_data("layout_changed", &layout_data);
                    }
                }
            });
        }),
        Box::new(move |wid, _pid| {
            // Capture app_name before the window is removed from the registry
            let app_name = WM_STATE.with(|s| {
                s.borrow()
                    .as_ref()
                    .and_then(|state| state.registry.get(wid).map(|w| w.app_name.clone()))
                    .unwrap_or_default()
            });
            WM_STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.on_window_closed(wid);
                    let layout_data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &layout_data);
                }
            });
            let closed_data = serde_json::json!({
                "window_id": wid,
                "app_name": &app_name,
            });
            publish_event(tarmac::ipc::events::WmEvent::WindowClosed {
                window_id: wid,
                app_name,
            });
            fire_lua_event_data("window_closed", &closed_data);
        }),
        Box::new(move |pid| {
            WM_STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.on_app_terminated(pid);
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
                Action::CloseWindow => {
                    state.close_focused();
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
                Action::Focus(dir) => {
                    state.focus_direction(dir);
                    if let Some(id) = state.active_workspace().focused {
                        if let Some(data) = state.window_focus_data(id) {
                            fire_lua_event_data("window_focused", &data);
                        }
                        if let Some(evt) = state.window_focused_event(id) {
                            publish_event(evt);
                        }
                    }
                }
                Action::Swap(dir) => {
                    state.swap_direction(dir);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
                Action::Resize(dir) => {
                    state.resize_direction(dir);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
                Action::Equalize => {
                    state.equalize();
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
                Action::Workspace(num) => {
                    let old = state.active_workspace().id.to_string();
                    state.switch_workspace(num);
                    let new = state.active_workspace().id.to_string();
                    fire_lua_event("workspace_changed", &[&old, &new]);
                    publish_event(state.workspace_changed_event(old, new));
                }
                Action::MoveToWorkspace(num) => {
                    state.move_to_workspace(num);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
                Action::WorkspaceNext => {
                    let old = state.active_workspace().id.to_string();
                    state.workspace_next();
                    let new = state.active_workspace().id.to_string();
                    fire_lua_event("workspace_changed", &[&old, &new]);
                    publish_event(state.workspace_changed_event(old, new));
                }
                Action::WorkspacePrev => {
                    let old = state.active_workspace().id.to_string();
                    state.workspace_prev();
                    let new = state.active_workspace().id.to_string();
                    fire_lua_event("workspace_changed", &[&old, &new]);
                    publish_event(state.workspace_changed_event(old, new));
                }
                Action::ToggleFloat => {
                    state.toggle_float();
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
                Action::ToggleSpecial(ref name) => {
                    state.toggle_special(name);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
                Action::MoveToSpecial(ref name) => {
                    state.move_to_special(name);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
                Action::FocusMonitorNext => {
                    state.focus_monitor_next();
                    let data = state.monitor_changed_data();
                    fire_lua_event_data("monitor_changed", &data);
                    publish_event(tarmac::ipc::events::WmEvent::MonitorChanged {
                        index: state.focused_monitor,
                        monitor_count: state.monitors.len(),
                        focused_workspace: state.active_workspace().id.to_string(),
                    });
                }
                Action::FocusMonitorPrev => {
                    state.focus_monitor_prev();
                    let data = state.monitor_changed_data();
                    fire_lua_event_data("monitor_changed", &data);
                    publish_event(tarmac::ipc::events::WmEvent::MonitorChanged {
                        index: state.focused_monitor,
                        monitor_count: state.monitors.len(),
                        focused_workspace: state.active_workspace().id.to_string(),
                    });
                }
                Action::MoveToMonitorNext => {
                    state.move_to_monitor_next();
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
                Action::MoveToMonitorPrev => {
                    state.move_to_monitor_prev();
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                }
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
    std::process::Command::new("open")
        .arg("-na")
        .arg("WezTerm")
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
gar.set("border_width", "4")
gar.set("border_color_focused", "#5294e2")
gar.set("border_color_unfocused", "#59595980")
gar.set("border_radius", "10")

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

    // Process tray menu actions and refresh tray state
    poll_tray_actions();
    TRAY.with(|t| {
        if let Some(tray) = t.borrow().as_ref() {
            update_tray(tray);
        }
    });

    // Process settings window actions
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
                    if let Some(id) = state.active_workspace().focused {
                        if let Some(data) = state.window_focus_data(id) {
                            fire_lua_event_data("window_focused", &data);
                        }
                        if let Some(evt) = state.window_focused_event(id) {
                            publish_event(evt);
                        }
                    }
                    Response::ok_empty()
                } else {
                    Response::err("usage: focus left|right|up|down")
                }
            }
            "swap" => {
                if let Some(dir) = request.args.first().and_then(|a| parse_dir(a)) {
                    state.swap_direction(dir);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                    Response::ok_empty()
                } else {
                    Response::err("usage: swap left|right|up|down")
                }
            }
            "resize" => {
                if let Some(dir) = request.args.first().and_then(|a| parse_dir(a)) {
                    state.resize_direction(dir);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                    Response::ok_empty()
                } else {
                    Response::err("usage: resize left|right|up|down")
                }
            }
            "close" => {
                state.close_focused();
                let data = state.layout_changed_data();
                publish_event(state.layout_changed_event());
                fire_lua_event_data("layout_changed", &data);
                Response::ok_empty()
            }
            "equalize" => {
                state.equalize();
                let data = state.layout_changed_data();
                publish_event(state.layout_changed_event());
                fire_lua_event_data("layout_changed", &data);
                Response::ok_empty()
            }
            "workspace" => {
                if let Some(n) = request.args.first().and_then(|a| a.parse::<u8>().ok()) {
                    let old = state.active_workspace().id.to_string();
                    state.switch_workspace(n);
                    let new = state.active_workspace().id.to_string();
                    fire_lua_event("workspace_changed", &[&old, &new]);
                    publish_event(state.workspace_changed_event(old, new));
                    Response::ok_empty()
                } else {
                    Response::err("usage: workspace <1-10>")
                }
            }
            "move-to-workspace" => {
                if let Some(n) = request.args.first().and_then(|a| a.parse::<u8>().ok()) {
                    state.move_to_workspace(n);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                    Response::ok_empty()
                } else {
                    Response::err("usage: move-to-workspace <1-10>")
                }
            }
            "toggle-floating" => {
                state.toggle_float();
                let data = state.layout_changed_data();
                publish_event(state.layout_changed_event());
                fire_lua_event_data("layout_changed", &data);
                Response::ok_empty()
            }
            "toggle-special" => {
                if let Some(name) = request.args.first() {
                    state.toggle_special(name);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
                    Response::ok_empty()
                } else {
                    Response::err("usage: toggle-special <name>")
                }
            }
            "move-to-special" => {
                if let Some(name) = request.args.first() {
                    state.move_to_special(name);
                    let data = state.layout_changed_data();
                    publish_event(state.layout_changed_event());
                    fire_lua_event_data("layout_changed", &data);
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
                let geoms = ws.tree.calculate_geometries_with_gaps(
                    sr,
                    state.gap_inner,
                    state.gap_outer,
                    true,
                );
                let windows: Vec<serde_json::Value> = geoms
                    .iter()
                    .map(|(wid, rect)| {
                        let app_name = state
                            .registry
                            .get(*wid)
                            .map(|w| w.app_name.clone())
                            .unwrap_or_default();
                        serde_json::json!({
                            "id": wid,
                            "app_name": app_name,
                            "x": rect.x, "y": rect.y,
                            "width": rect.width, "height": rect.height,
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

fn reload_config() {
    let path = CONFIG_PATH.with(|p| p.borrow().clone());
    let Some(path) = path else {
        tracing::error!("no config path stored, cannot reload");
        return;
    };

    tracing::info!("reloading config...");
    let config = tarmac::config::lua::load_config(&path);

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

    tracing::info!("config reloaded successfully");
}

fn fire_lua_event(event: &str, args: &[&str]) {
    LUA_CONFIG.with(|c| {
        if let Some(config) = c.borrow().as_ref() {
            config.fire_event(event, args);
        }
    });
}

fn fire_lua_event_data(event: &str, data: &serde_json::Value) {
    LUA_CONFIG.with(|c| {
        if let Some(config) = c.borrow().as_ref() {
            config.fire_event_with_data(event, data);
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

fn update_tray(tray: &tarmac::ui::tray::TrayWidget) {
    WM_STATE.with(|s| {
        if let Some(state) = s.borrow().as_ref() {
            let active_id = state.active_workspace().id.to_string();
            let workspaces: Vec<tarmac::ui::tray::TrayWorkspace> = state
                .workspaces
                .iter()
                .map(|ws| tarmac::ui::tray::TrayWorkspace {
                    id: ws.id.to_string(),
                    active: ws.visible,
                    windows: ws.all_window_ids().len(),
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
                tarmac::ui::tray::TrayAction::SwitchWorkspace(num) => {
                    handle_action(tarmac::core::input::Action::Workspace(num));
                }
                tarmac::ui::tray::TrayAction::OpenSettings => {
                    open_settings();
                }
                tarmac::ui::tray::TrayAction::Reload => {
                    reload_config();
                }
                tarmac::ui::tray::TrayAction::Quit => {
                    tracing::info!("quit from tray");
                    cleanup_socket();
                    std::process::exit(0);
                }
            }
        }
    });
}

fn open_settings() {
    SETTINGS_WIN.with(|sw| {
        if sw.borrow().is_none() {
            let mtm = unsafe { objc2::MainThreadMarker::new_unchecked() };
            let win = tarmac::ui::settings::SettingsWindow::new(mtm);
            // Populate from current state
            WM_STATE.with(|s| {
                if let Some(state) = s.borrow().as_ref() {
                    let snap = build_settings_snapshot(state);
                    win.populate(&snap);
                }
            });
            *sw.borrow_mut() = Some(win);
        }
        if let Some(win) = sw.borrow().as_ref() {
            win.toggle();
        }
    });
}

fn build_settings_snapshot(
    state: &tarmac::core::state::WmState,
) -> tarmac::ui::settings::SettingsSnapshot {
    use tarmac::core::input::Modifiers;
    let mod_key = LUA_CONFIG.with(|c| {
        c.borrow()
            .as_ref()
            .map(|cfg| {
                if cfg.settings.mod_key == Modifiers::OPTION {
                    "option"
                } else if cfg.settings.mod_key == Modifiers::CONTROL {
                    "control"
                } else {
                    "command"
                }
            })
            .unwrap_or("command")
            .to_string()
    });
    tarmac::ui::settings::SettingsSnapshot {
        gap_inner: state.gap_inner,
        gap_outer: state.gap_outer,
        bar_height: state.bar_height,
        border_width: state.borders.border_width,
        border_radius: state.borders.radius,
        border_color_focused: state.borders.focused_color.to_hex(),
        border_color_unfocused: state.borders.unfocused_color.to_hex(),
        focus_follows_mouse: state.focus_follows_mouse,
        mouse_follows_focus: state.mouse_follows_focus,
        mod_key,
        keybinds: LUA_CONFIG.with(|c| {
            c.borrow()
                .as_ref()
                .map(|cfg| {
                    cfg.keybinds
                        .iter()
                        .map(|kb| (format!("{:?}+{:?}", kb.modifiers, kb.key), format!("{:?}", kb.action)))
                        .collect()
                })
                .unwrap_or_default()
        }),
        rules: LUA_CONFIG.with(|c| {
            c.borrow()
                .as_ref()
                .map(|cfg| {
                    cfg.rules
                        .iter()
                        .map(|r| {
                            let match_str = r.app_name.as_deref().unwrap_or("*").to_string();
                            let mut actions = Vec::new();
                            if let Some(true) = r.floating {
                                actions.push("float".to_string());
                            }
                            if let Some(ws) = &r.workspace {
                                actions.push(format!("workspace {ws}"));
                            }
                            (match_str, actions.join(", "))
                        })
                        .collect()
                })
                .unwrap_or_default()
        }),
    }
}

fn poll_settings_actions() {
    use tarmac::config::lua::{lua_number, lua_string};
    use tarmac::ui::settings::SettingsAction;

    SETTINGS_WIN.with(|sw| {
        let borrow = sw.borrow();
        let Some(win) = borrow.as_ref() else { return };
        let actions = win.poll_actions();
        if actions.is_empty() {
            return;
        }
        win.refresh_labels();
        drop(borrow);

        let config_path = CONFIG_PATH.with(|p| p.borrow().clone());

        WM_STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                for action in actions {
                    match action {
                        SettingsAction::GapInner(v) => {
                            state.gap_inner = v;
                            write_setting(&config_path, "gap_inner", &lua_number(v));
                        }
                        SettingsAction::GapOuter(v) => {
                            state.gap_outer = v;
                            write_setting(&config_path, "gap_outer", &lua_number(v));
                        }
                        SettingsAction::BarHeight(v) => {
                            state.bar_height = v;
                            write_setting(&config_path, "bar_height", &lua_number(v));
                        }
                        SettingsAction::BorderWidth(v) => {
                            state.borders.border_width = v;
                            write_setting(&config_path, "border_width", &lua_string(&lua_number(v)));
                        }
                        SettingsAction::BorderRadius(v) => {
                            state.borders.radius = v;
                            write_setting(&config_path, "border_radius", &lua_string(&lua_number(v)));
                        }
                        SettingsAction::BorderColorFocused(ref hex) => {
                            state.borders.focused_color =
                                tarmac::platform::border::BorderColor::from_hex(hex);
                            write_setting(&config_path, "border_color_focused", &lua_string(hex));
                        }
                        SettingsAction::BorderColorUnfocused(ref hex) => {
                            state.borders.unfocused_color =
                                tarmac::platform::border::BorderColor::from_hex(hex);
                            write_setting(&config_path, "border_color_unfocused", &lua_string(hex));
                        }
                        SettingsAction::FocusFollowsMouse(v) => {
                            state.focus_follows_mouse = v;
                            write_setting(
                                &config_path,
                                "focus_follows_mouse",
                                &lua_string(if v { "true" } else { "false" }),
                            );
                        }
                        SettingsAction::MouseFollowsFocus(v) => {
                            state.mouse_follows_focus = v;
                            write_setting(
                                &config_path,
                                "mouse_follows_focus",
                                &lua_string(if v { "true" } else { "false" }),
                            );
                        }
                        SettingsAction::ModKey(ref key) => {
                            write_setting(&config_path, "mod_key", &lua_string(key));
                        }
                        SettingsAction::Open => {}
                    }
                }
                state.apply_layout();
                state.update_borders();
            }
        });
    });
}

fn write_setting(config_path: &Option<std::path::PathBuf>, key: &str, value: &str) {
    if let Some(path) = config_path
        && let Err(e) = tarmac::config::lua::update_lua_setting(path, key, value)
    {
        tracing::warn!(key, err = %e, "failed to write setting to config");
    }
}

fn run_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_app_kit::NSApplicationActivationPolicy;

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Initialize system tray
    let tray = tarmac::ui::tray::TrayWidget::new(mtm);
    update_tray(&tray);
    TRAY.with(|t| *t.borrow_mut() = Some(tray));

    tracing::info!("entering main event loop");
    app.run();
}
