use std::ffi::c_void;

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

/// Dump all on-screen windows via CGWindowList for diagnostic purposes.
/// This sees ALL windows regardless of AX accessibility, including window IDs, PIDs, and names.
pub fn dump_cg_window_list() {
    unsafe {
        let info = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        );
        if info.is_null() {
            tracing::warn!("CGWindowListCopyWindowInfo returned null");
            return;
        }

        let count = CFArrayGetCount(info);
        tracing::debug!(count, "CGWindowList on-screen windows");

        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(info, i);
            if dict.is_null() {
                continue;
            }

            let pid = cg_dict_get_i32(dict, kCGWindowOwnerPID);
            let wid = cg_dict_get_i32(dict, kCGWindowNumber);
            let layer = cg_dict_get_i32(dict, kCGWindowLayer);
            let name = cg_dict_get_string(dict, kCGWindowOwnerName);
            let title = cg_dict_get_string(dict, kCGWindowName);

            // Only log normal layer (0) windows — skip menu bar, dock, etc.
            if layer == 0 {
                tracing::trace!(
                    wid,
                    pid,
                    layer,
                    owner = %name,
                    title = %title,
                    "CGWindowList entry"
                );
            }
        }

        CFRelease(info);
    }
}

/// Discover all manageable windows across all running applications.
pub fn discover_all_windows() -> Vec<WindowInfo> {
    // First, dump the CG window list so we can see ground truth
    dump_cg_window_list();

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

// CGWindowList FFI
#[allow(non_upper_case_globals, clashing_extern_declarations)]
mod cg_ffi {
    use std::ffi::c_void;

    pub const kCGWindowListOptionOnScreenOnly: u32 = 1 << 0;
    pub const kCGWindowListExcludeDesktopElements: u32 = 1 << 4;
    pub const kCGNullWindowID: u32 = 0;

    unsafe extern "C" {
        pub fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
        pub fn CFArrayGetCount(array: *const c_void) -> isize;
        pub fn CFArrayGetValueAtIndex(array: *const c_void, idx: isize) -> *const c_void;
        pub fn CFRelease(cf: *const c_void);

        pub static kCGWindowOwnerPID: *const c_void;
        pub static kCGWindowNumber: *const c_void;
        pub static kCGWindowLayer: *const c_void;
        pub static kCGWindowOwnerName: *const c_void;
        pub static kCGWindowName: *const c_void;

        pub fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
        pub fn CFNumberGetValue(number: *const c_void, r#type: i32, value_ptr: *mut c_void)
        -> bool;
        pub fn CFStringGetCStringPtr(string: *const c_void, encoding: u32) -> *const i8;
    }
}
use cg_ffi::*;

fn cg_dict_get_i32(dict: *const c_void, key: *const c_void) -> i32 {
    unsafe {
        let val = CFDictionaryGetValue(dict, key);
        if val.is_null() {
            return 0;
        }
        let mut result: i32 = 0;
        CFNumberGetValue(
            val,
            3, // kCFNumberSInt32Type
            (&mut result as *mut i32).cast::<c_void>(),
        );
        result
    }
}

fn cg_dict_get_string(dict: *const c_void, key: *const c_void) -> String {
    unsafe {
        let val = CFDictionaryGetValue(dict, key);
        if val.is_null() {
            return String::new();
        }
        let cstr = CFStringGetCStringPtr(val, 0x08000100); // kCFStringEncodingUTF8
        if cstr.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(cstr).to_string_lossy().to_string()
    }
}
