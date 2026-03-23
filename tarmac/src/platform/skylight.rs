//! Minimal SkyLight private framework bindings for window level control.
//! SLSSetWindowLevel sets a window's z-level in the WindowServer,
//! allowing floating windows to stay above normal windows across apps.
//! This does NOT require SIP to be disabled.

use std::ffi::{c_int, c_void};
use std::ptr::NonNull;

use objc2_core_foundation::{CFArray, CFNumber, CFNumberType, CFRetained, kCFTypeArrayCallBacks};

type CGSConnectionID = c_int;
type CGError = i32;

/// Normal window level (standard app windows)
pub const K_CG_NORMAL_WINDOW_LEVEL: c_int = 0;
/// Floating window level (above normal windows, below menus)
pub const K_CG_FLOATING_WINDOW_LEVEL: c_int = 3;
/// Modal panel level — above all normal and floating windows.
/// Used for tarmac floating windows to prevent any app activation
/// from pushing them behind tiled windows.
pub const K_CG_MODAL_WINDOW_LEVEL: c_int = 8;

// SkyLight is a private framework, linked via build.rs
unsafe extern "C" {
    fn SLSMainConnectionID() -> CGSConnectionID;
    fn SLSSetWindowLevel(cid: CGSConnectionID, wid: u32, level: c_int) -> CGError;
    fn SLSSetWindowAlpha(cid: CGSConnectionID, wid: u32, alpha: f32) -> CGError;
    fn SLSMoveWindowWithGroup(cid: CGSConnectionID, wid: u32, point: *const CGPoint) -> CGError;
    pub fn SLSMoveWindow(cid: CGSConnectionID, wid: u32, point: *const CGPoint) -> CGError;
    fn SLSCopyAssociatedWindows(cid: CGSConnectionID, wid: u32) -> *const CFArray<CFNumber>;
    fn SLSReassociateWindowsSpacesByGeometry(
        cid: CGSConnectionID,
        window_list: &CFArray,
    ) -> CGError;
    fn SLSOrderWindow(cid: CGSConnectionID, wid: u32, mode: c_int, relative_to: u32) -> CGError;
    fn SLSTransactionCreate(cid: CGSConnectionID) -> *const c_void;
    fn SLSTransactionSetWindowSystemAlpha(
        transaction: *const c_void,
        wid: u32,
        alpha: f32,
    ) -> CGError;
    fn SLSTransactionCommit(transaction: *const c_void, synchronous: c_int) -> CGError;
    fn CFRelease(cf: *const c_void);
}

#[repr(C)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

/// Set a window's opacity in the WindowServer (0.0 = invisible, 1.0 = opaque).
pub fn set_window_alpha(window_id: u32, alpha: f32) -> bool {
    let cid = unsafe { SLSMainConnectionID() };
    let result = unsafe { SLSSetWindowAlpha(cid, window_id, alpha) };
    result == 0
}

/// Set opacity for a window and any WindowServer-associated child surfaces.
pub fn set_window_group_alpha(window_id: u32, alpha: f32) -> bool {
    let cid = unsafe { SLSMainConnectionID() };
    let mut ok = set_window_alpha(window_id, alpha);

    for child_id in associated_window_ids(cid, window_id) {
        let result = unsafe { SLSSetWindowAlpha(cid, child_id, alpha) };
        ok &= result == 0;
    }

    ok
}

/// Set system alpha for a window and associated child surfaces.
/// This affects system-drawn chrome that regular window alpha may not hide.
pub fn set_window_group_system_alpha(window_id: u32, alpha: f32) -> bool {
    let cid = unsafe { SLSMainConnectionID() };
    let transaction = unsafe { SLSTransactionCreate(cid) };
    if transaction.is_null() {
        return false;
    }

    let mut ok = true;
    ok &= unsafe { SLSTransactionSetWindowSystemAlpha(transaction, window_id, alpha) } == 0;
    for child_id in associated_window_ids(cid, window_id) {
        ok &= unsafe { SLSTransactionSetWindowSystemAlpha(transaction, child_id, alpha) } == 0;
    }
    ok &= unsafe { SLSTransactionCommit(transaction, 0) } == 0;
    unsafe { CFRelease(transaction) };
    ok
}

/// Return the root window id plus any WindowServer-associated child surfaces.
pub fn window_group_ids(window_id: u32) -> Vec<u32> {
    let cid = unsafe { SLSMainConnectionID() };
    let mut ids = vec![window_id];
    ids.extend(associated_window_ids(cid, window_id));
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Move a window in the WindowServer, bypassing AX position clamping.
/// macOS AX API clamps positions to keep windows partially on-screen.
/// Prefer moving the whole window group so app chrome and grouped surfaces
/// do not get left behind.
pub fn move_window(window_id: u32, x: f64, y: f64) -> bool {
    let cid = unsafe { SLSMainConnectionID() };
    let point = CGPoint { x, y };

    let result = unsafe { SLSMoveWindowWithGroup(cid, window_id, &point) };
    if result == 0 {
        let _ = reassociate_window_geometry(cid, window_id);
        return true;
    }

    let result = unsafe { SLSMoveWindow(cid, window_id, &point) };
    if result == 0 {
        let _ = reassociate_window_geometry(cid, window_id);
    }
    result == 0
}

fn reassociate_window_geometry(cid: CGSConnectionID, window_id: u32) -> bool {
    let window_id = window_id as i32;
    let number = unsafe {
        CFNumber::new(
            None,
            CFNumberType::IntType,
            (&window_id as *const i32).cast(),
        )
    };
    let Some(number) = number else {
        return false;
    };

    let mut values = [number.as_ref() as *const CFNumber as *const std::ffi::c_void];
    let windows: Option<CFRetained<CFArray>> =
        unsafe { CFArray::new(None, values.as_mut_ptr(), 1, &kCFTypeArrayCallBacks) };
    let Some(windows) = windows else {
        return false;
    };

    let result = unsafe { SLSReassociateWindowsSpacesByGeometry(cid, windows.as_ref()) };
    result == 0
}

fn associated_window_ids(cid: CGSConnectionID, window_id: u32) -> Vec<u32> {
    let window_list = unsafe { SLSCopyAssociatedWindows(cid, window_id) };
    let Some(window_list) = NonNull::new(window_list as *mut CFArray<CFNumber>) else {
        return Vec::new();
    };

    let window_list: CFRetained<CFArray<CFNumber>> = unsafe { CFRetained::from_raw(window_list) };
    let mut result = Vec::with_capacity(window_list.len());

    for i in 0..window_list.len() {
        let Some(number) = window_list.get(i) else {
            continue;
        };

        let mut child_id: i32 = 0;
        let ok =
            unsafe { number.value(CFNumberType::SInt32Type, (&mut child_id as *mut i32).cast()) };
        if ok && child_id > 0 && child_id as u32 != window_id {
            result.push(child_id as u32);
        }
    }

    result
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

/// Order a window above all other windows at its level.
/// mode 1 = above (kCGSOrderAbove).
pub fn order_window_front(window_id: u32) {
    let cid = unsafe { SLSMainConnectionID() };
    unsafe { SLSOrderWindow(cid, window_id, 1, 0) };
}
