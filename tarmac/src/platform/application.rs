use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFRetained;

use super::accessibility::{
    CGWindowID, ax_get_position, ax_get_size, ax_get_string, ax_get_window_id, ax_get_windows,
    is_manageable_window,
};

/// Metadata about a running application.
#[derive(Debug)]
pub struct AppInfo {
    pub pid: i32,
    pub bundle_id: String,
    pub name: String,
    pub ax_ref: CFRetained<AXUIElement>,
}

/// Metadata about a discovered window.
#[derive(Debug)]
pub struct WindowInfo {
    pub id: CGWindowID,
    pub app_pid: i32,
    pub app_name: String,
    pub app_bundle_id: String,
    pub title: String,
    pub role: String,
    pub subrole: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub ax_ref: CFRetained<AXUIElement>,
}

/// Discover all running regular applications.
pub fn discover_applications() -> Vec<AppInfo> {
    let workspace = NSWorkspace::sharedWorkspace();
    let apps: Retained<objc2_foundation::NSArray<NSRunningApplication>> =
        workspace.runningApplications();

    let mut result = Vec::new();
    let count = apps.count();
    for i in 0..count {
        let app: Retained<NSRunningApplication> = apps.objectAtIndex(i);
        let policy = app.activationPolicy();
        if policy != NSApplicationActivationPolicy::Regular {
            continue;
        }

        let pid = app.processIdentifier();
        let bundle_id: String = match app.bundleIdentifier() {
            Some(b) => b.to_string(),
            None => continue,
        };
        let name: String = match app.localizedName() {
            Some(n) => n.to_string(),
            None => continue,
        };

        let ax_ref = unsafe { AXUIElement::new_application(pid) };

        result.push(AppInfo {
            pid,
            bundle_id,
            name,
            ax_ref,
        });
    }

    result
}

/// Enumerate all manageable windows for an application.
pub fn enumerate_windows(app: &AppInfo) -> Vec<WindowInfo> {
    let ax_windows = match ax_get_windows(&app.ax_ref) {
        Ok(w) => w,
        Err(e) => {
            tracing::trace!(app = %app.name, err = %e, "failed to get windows");
            return Vec::new();
        }
    };

    tracing::trace!(app = %app.name, ax_window_count = ax_windows.len(), "raw AX windows");

    let mut result = Vec::new();
    for ax_win in ax_windows {
        if !is_manageable_window(&ax_win) {
            continue;
        }

        let id = match ax_get_window_id(&ax_win) {
            Ok(id) => id,
            Err(e) => {
                tracing::trace!(app = %app.name, err = %e, "failed to get window id");
                continue;
            }
        };

        let title = ax_get_string(&ax_win, "AXTitle").unwrap_or_default();
        let role = ax_get_string(&ax_win, "AXRole").unwrap_or_default();
        let subrole = ax_get_string(&ax_win, "AXSubrole").unwrap_or_default();
        let (x, y) = ax_get_position(&ax_win).unwrap_or((0.0, 0.0));
        let (width, height) = ax_get_size(&ax_win).unwrap_or((0.0, 0.0));

        result.push(WindowInfo {
            id,
            app_pid: app.pid,
            app_name: app.name.clone(),
            app_bundle_id: app.bundle_id.clone(),
            title,
            role,
            subrole,
            x,
            y,
            width,
            height,
            ax_ref: ax_win,
        });
    }

    result
}

/// Discover all manageable windows across all running applications.
pub fn discover_all_windows() -> Vec<WindowInfo> {
    let apps = discover_applications();
    let mut all_windows = Vec::new();

    for app in &apps {
        let windows = enumerate_windows(app);
        tracing::debug!(
            app = %app.name,
            count = windows.len(),
            "discovered windows"
        );
        all_windows.extend(windows);
    }

    tracing::info!(
        total = all_windows.len(),
        apps = apps.len(),
        "window discovery complete"
    );
    all_windows
}
