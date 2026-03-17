use std::cell::RefCell;
use std::rc::Rc;

use tarmac::core::window::{WindowRegistry, WindowState};
use tarmac::platform::accessibility::{
    ax_get_position, ax_get_size, ax_get_string, ax_get_window_id, is_manageable_window,
};
use tarmac::platform::application::{discover_all_windows, discover_applications};
use tarmac::platform::observer::{AppObserver, WindowEvent};
use tarmac::platform::permissions;
use tracing_subscriber::EnvFilter;

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

    // Shared window registry
    let registry = Rc::new(RefCell::new(WindowRegistry::new()));

    // Discover existing windows
    let windows = discover_all_windows();
    for w in &windows {
        registry.borrow_mut().add(WindowState {
            id: w.id,
            app_pid: w.app_pid,
            app_name: w.app_name.clone(),
            app_bundle_id: w.app_bundle_id.clone(),
            title: w.title.clone(),
            role: w.role.clone(),
            subrole: w.subrole.clone(),
            x: w.x,
            y: w.y,
            width: w.width,
            height: w.height,
            floating: false,
            minimized: false,
        });
    }

    tracing::info!(
        windows = registry.borrow().count(),
        "initial window registry populated"
    );

    // Set up observers for each running app
    let apps = discover_applications();
    // Keep observers alive for the lifetime of the app
    let mut _observers: Vec<AppObserver> = Vec::new();

    for app in &apps {
        let reg = Rc::clone(&registry);
        let app_name = app.name.clone();
        let app_bundle = app.bundle_id.clone();
        let app_pid = app.pid;

        let callback = Box::new(move |event: WindowEvent| {
            handle_window_event(event, &reg, &app_name, &app_bundle, app_pid);
        });

        match AppObserver::new(app.pid, &app.ax_ref, callback) {
            Ok(observer) => {
                tracing::trace!(app = %app.name, pid = app.pid, "observer installed");
                _observers.push(observer);
            }
            Err(e) => {
                tracing::trace!(app = %app.name, err = %e, "failed to create observer");
            }
        }
    }

    tracing::info!(observers = _observers.len(), "observers installed");

    run_app();
}

fn handle_window_event(
    event: WindowEvent,
    registry: &Rc<RefCell<WindowRegistry>>,
    app_name: &str,
    app_bundle: &str,
    app_pid: i32,
) {
    match event {
        WindowEvent::Created { element, .. } => {
            if !is_manageable_window(&element) {
                return;
            }
            let id = match ax_get_window_id(&element) {
                Ok(id) => id,
                Err(_) => return,
            };
            if registry.borrow().contains(id) {
                return; // deduplicate
            }
            let title = ax_get_string(&element, "AXTitle").unwrap_or_default();
            let (x, y) = ax_get_position(&element).unwrap_or((0.0, 0.0));
            let (w, h) = ax_get_size(&element).unwrap_or((0.0, 0.0));
            tracing::info!(id, app = app_name, title = %title, "window created");
            registry.borrow_mut().add(WindowState {
                id,
                app_pid,
                app_name: app_name.to_string(),
                app_bundle_id: app_bundle.to_string(),
                title,
                role: "AXWindow".to_string(),
                subrole: "AXStandardWindow".to_string(),
                x,
                y,
                width: w,
                height: h,
                floating: false,
                minimized: false,
            });
        }
        WindowEvent::Destroyed { element, .. } => {
            // Try to get the window ID; may fail if element is already invalid
            if let Ok(id) = ax_get_window_id(&element)
                && registry.borrow().contains(id)
            {
                tracing::info!(id, app = app_name, "window destroyed");
                registry.borrow_mut().remove(id);
            }
        }
        WindowEvent::FocusChanged { element, .. } => {
            if let Ok(id) = ax_get_window_id(&element) {
                let title = ax_get_string(&element, "AXTitle").unwrap_or_default();
                tracing::debug!(id, app = app_name, title = %title, "focus changed");
            }
        }
        WindowEvent::Moved { element, .. } => {
            if let Ok(id) = ax_get_window_id(&element)
                && let Ok((x, y)) = ax_get_position(&element)
            {
                tracing::debug!(id, x, y, "window moved");
                let mut reg = registry.borrow_mut();
                if let Some(w) = reg.get(id) {
                    let (width, height) = (w.width, w.height);
                    reg.update_geometry(id, x, y, width, height);
                }
            }
        }
        WindowEvent::Resized { element, .. } => {
            if let Ok(id) = ax_get_window_id(&element)
                && let (Ok((x, y)), Ok((w, h))) = (ax_get_position(&element), ax_get_size(&element))
            {
                tracing::debug!(id, w, h, "window resized");
                registry.borrow_mut().update_geometry(id, x, y, w, h);
            }
        }
        WindowEvent::TitleChanged { element, .. } => {
            if let Ok(id) = ax_get_window_id(&element)
                && let Ok(title) = ax_get_string(&element, "AXTitle")
            {
                tracing::debug!(id, title = %title, "title changed");
                registry.borrow_mut().update_title(id, title);
            }
        }
        WindowEvent::Minimized { element, .. } => {
            if let Ok(id) = ax_get_window_id(&element) {
                tracing::debug!(id, app = app_name, "window minimized");
                if let Some(w) = registry.borrow_mut().get_mut(id) {
                    w.minimized = true;
                }
            }
        }
        WindowEvent::Unminimized { element, .. } => {
            if let Ok(id) = ax_get_window_id(&element) {
                tracing::debug!(id, app = app_name, "window unminimized");
                if let Some(w) = registry.borrow_mut().get_mut(id) {
                    w.minimized = false;
                }
            }
        }
    }
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
