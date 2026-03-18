//! Minimal SkyLight private framework bindings for window level control.
//! SLSSetWindowLevel sets a window's z-level in the WindowServer,
//! allowing floating windows to stay above normal windows across apps.
//! This does NOT require SIP to be disabled.

use std::ffi::c_int;

type CGSConnectionID = c_int;
type CGError = i32;

/// Normal window level (standard app windows)
pub const K_CG_NORMAL_WINDOW_LEVEL: c_int = 0;
/// Floating window level (above normal windows, below menus)
pub const K_CG_FLOATING_WINDOW_LEVEL: c_int = 3;

// SkyLight is a private framework, linked via build.rs
unsafe extern "C" {
    fn SLSMainConnectionID() -> CGSConnectionID;
    fn SLSSetWindowLevel(cid: CGSConnectionID, wid: u32, level: c_int) -> CGError;
    fn SLSSetWindowAlpha(cid: CGSConnectionID, wid: u32, alpha: f32) -> CGError;
}

/// Set a window's opacity in the WindowServer (0.0 = invisible, 1.0 = opaque).
pub fn set_window_alpha(window_id: u32, alpha: f32) -> bool {
    let cid = unsafe { SLSMainConnectionID() };
    let result = unsafe { SLSSetWindowAlpha(cid, window_id, alpha) };
    result == 0
}

/// Set a window's z-level in the WindowServer.
/// Use K_CG_FLOATING_WINDOW_LEVEL (3) to keep a window above all normal windows.
pub fn set_window_level(window_id: u32, level: c_int) -> bool {
    let cid = unsafe { SLSMainConnectionID() };
    let result = unsafe { SLSSetWindowLevel(cid, window_id, level) };
    if result != 0 {
        tracing::warn!(window_id, level, result, cid, "SLSSetWindowLevel failed");
    }
    result == 0
}
