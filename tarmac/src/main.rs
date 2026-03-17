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
    state.rules = config.rules;
    state.discover_and_observe();
    WM_STATE.with(|s| *s.borrow_mut() = Some(state));

    // Register hotkeys from config
    let mut _hotkey_mgr = HotkeyManager::new(Box::new(|action| {
        handle_action(action);
    }));
    if let Some(ref mut mgr) = _hotkey_mgr {
        for kb in &config.keybinds {
            mgr.register(kb.modifiers, kb.key, kb.action);
        }
        tracing::info!(
            registered = config.keybinds.len(),
            "hotkeys registered from config"
        );
    } else {
        tracing::error!("failed to create hotkey manager");
    }

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
    let ipc_rx = tarmac::ipc::server::start_server();
    IPC_RX.with(|r| *r.borrow_mut() = Some(ipc_rx));

    install_polling_timer();
    run_app();
}

fn handle_action(action: Action) {
    WM_STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            match action {
                Action::SpawnTerminal => spawn_terminal(),
                Action::CloseWindow => state.close_focused(),
                Action::Focus(dir) => state.focus_direction(dir),
                Action::Swap(dir) => state.swap_direction(dir),
                Action::Resize(dir) => state.resize_direction(dir),
                Action::Equalize => state.equalize(),
                Action::Workspace(num) => state.switch_workspace(num),
                Action::MoveToWorkspace(num) => state.move_to_workspace(num),
                Action::WorkspaceNext => state.workspace_next(),
                Action::WorkspacePrev => state.workspace_prev(),
                Action::ToggleFloat => state.toggle_float(),
                Action::Reload => {
                    tracing::info!("config reload requested (restart tarmac to apply)");
                    // TODO: Full hot-reload requires re-registering Carbon hotkeys
                    // which needs dropping and recreating the HotkeyManager.
                    // For now, log the request.
                }
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
    let default_config = r#"-- tarmac configuration
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

-- System
gar.bind("mod+shift+r", "reload")
-- gar.bind("mod+shift+e", "exit")

-- Window rules
-- gar.rule({ app_name = "Calculator" }, { floating = true })
-- gar.rule({ app_name = "System Settings" }, { floating = true })
-- gar.rule({ app_name = "Safari" }, { workspace = 2 })

-- Autostart (uncomment as needed)
-- gar.exec_once("sketchybar")
"#;

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
}

fn process_ipc_command(
    request: &tarmac::ipc::protocol::Request,
) -> tarmac::ipc::protocol::Response {
    use tarmac::ipc::protocol::Response;

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
            "workspace" => {
                if let Some(n) = request.args.first().and_then(|a| a.parse::<u8>().ok()) {
                    state.switch_workspace(n);
                    Response::ok_empty()
                } else {
                    Response::err("usage: workspace <1-10>")
                }
            }
            "move-to-workspace" => {
                if let Some(n) = request.args.first().and_then(|a| a.parse::<u8>().ok()) {
                    state.move_to_workspace(n);
                    Response::ok_empty()
                } else {
                    Response::err("usage: move-to-workspace <1-10>")
                }
            }
            "toggle-floating" => {
                state.toggle_float();
                Response::ok_empty()
            }
            "get-workspaces" => {
                let active_id = state.workspaces.active_id().to_string();
                let ws_count = state.workspaces.active().all_window_ids().len();
                let focused = state.workspaces.active().focused;
                Response::ok(serde_json::json!({
                    "active": active_id,
                    "windows": ws_count,
                    "focused": focused,
                }))
            }
            "get-focused" => {
                if let Some(focused_id) = state.workspaces.active().focused {
                    if let Some(w) = state.registry.get(focused_id) {
                        Response::ok(serde_json::json!({
                            "id": w.id,
                            "app_name": w.app_name,
                            "title": w.title,
                            "x": w.x, "y": w.y,
                            "width": w.width, "height": w.height,
                            "floating": w.floating,
                        }))
                    } else {
                        Response::ok(serde_json::json!({ "id": focused_id }))
                    }
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

fn cleanup_socket() {
    let path = tarmac::ipc::protocol::socket_path();
    let _ = std::fs::remove_file(&path);
}

fn run_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_app_kit::NSApplicationActivationPolicy;

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    tracing::info!("entering main event loop");
    app.run();
}
