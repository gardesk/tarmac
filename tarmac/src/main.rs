use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;

use tarmac::core::input::{Action, KeybindManager};
use tarmac::core::state::WmState;
use tarmac::platform::event_tap::EventTap;
use tarmac::platform::permissions;
use tarmac::platform::workspace_observer::WorkspacePollingObserver;
use tracing_subscriber::EnvFilter;

thread_local! {
    static WORKSPACE_POLLER: RefCell<Option<WorkspacePollingObserver>> = const { RefCell::new(None) };
    static WM_STATE: RefCell<Option<WmState>> = const { RefCell::new(None) };
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

    // Initialize window manager state
    let mut state = WmState::new();
    state.discover_and_observe();
    WM_STATE.with(|s| *s.borrow_mut() = Some(state));

    // Set up keybinds and event tap
    let keybinds = KeybindManager::with_defaults();
    let _event_tap = EventTap::install(Box::new(move |event| {
        use tarmac::core::input::InputEvent;
        match event {
            InputEvent::Key(key_event) => {
                if let Some(action) = keybinds.dispatch(&key_event) {
                    tracing::debug!(?action, "keybind matched");
                    handle_action(action);
                    true // suppress
                } else {
                    false // pass through
                }
            }
            InputEvent::MouseClick { x, y } => {
                // Click-to-focus: find which window was clicked, focus it
                WM_STATE.with(|s| {
                    if let Some(state) = s.borrow_mut().as_mut() {
                        state.click_to_focus(x, y);
                    }
                });
                false // always pass click through
            }
        }
    }));

    if _event_tap.is_none() {
        tracing::error!("failed to create event tap — check accessibility permissions");
    }

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
    );
    WORKSPACE_POLLER.with(|p| *p.borrow_mut() = Some(poller));

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
    // Drain queued AX observer events and process them
    WM_STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.process_events();
        }
    });

    // Poll for app launches/terminations
    WORKSPACE_POLLER.with(|p| {
        if let Some(poller) = p.borrow_mut().as_mut() {
            poller.poll();
        }
    });
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
    if let Some(config) = dirs::config_dir() {
        let tarmac_config = config.join("tarmac");
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
        std::process::exit(0);
    }) {
        tracing::warn!("failed to set ctrl-c handler: {}", e);
    }
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
