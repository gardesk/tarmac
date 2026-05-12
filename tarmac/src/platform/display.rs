use crate::core::tree::Rect;
use objc2::msg_send;

/// Get the usable frame of the main display (minus dock and menu bar).
/// Coordinates are in screen space with origin at top-left.
pub fn get_usable_frame() -> Rect {
    unsafe {
        let main_id = CGMainDisplayID();
        let full = CGDisplayBounds(main_id);
        let full_height = full.size.height;

        // NSScreen.mainScreen.visibleFrame gives us the usable area
        // but uses bottom-left origin. Use CGDisplay for consistency.
        // The usable area excludes dock and menu bar.
        // We can approximate by checking NSScreen.
        let visible = get_nsscreen_visible_frame();

        top_left_rect_from_visible_frame(visible, full_height)
    }
}

/// Get the full display frame (including dock and menu bar).
/// This is the physical screen bounds — used for hiding windows off-screen.
pub fn get_full_display_frame() -> Rect {
    unsafe {
        let main_id = CGMainDisplayID();
        let full = CGDisplayBounds(main_id);
        // CGDisplayBounds returns origin at top-left for the main display
        Rect::new(
            full.origin.x,
            full.origin.y,
            full.size.width,
            full.size.height,
        )
    }
}

struct NSRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn top_left_rect_from_visible_frame(visible: NSRect, main_height: f64) -> Rect {
    let top_y = main_height - visible.y - visible.height;
    Rect::new(visible.x, top_y, visible.width, visible.height)
}

/// Get NSScreen.mainScreen.visibleFrame (bottom-left origin)
fn get_nsscreen_visible_frame() -> NSRect {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    // Safety: called from main thread
    let mtm = unsafe { MainThreadMarker::new_unchecked() };

    let screen = NSScreen::mainScreen(mtm).expect("no main screen");
    let frame = screen.frame();
    let visible = screen.visibleFrame();

    tracing::trace!(
        full_w = frame.size.width,
        full_h = frame.size.height,
        vis_x = visible.origin.x,
        vis_y = visible.origin.y,
        vis_w = visible.size.width,
        vis_h = visible.size.height,
        "NSScreen frames"
    );

    NSRect {
        x: visible.origin.x,
        y: visible.origin.y,
        width: visible.size.width,
        height: visible.size.height,
    }
}

// CGDisplay FFI
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

/// Get the current mouse cursor position in CG screen coordinates (top-left origin).
pub fn get_cursor_position() -> (f64, f64) {
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return (0.0, 0.0);
        }
        let loc = CGEventGetLocation(event);
        CFRelease(event as *const std::ffi::c_void);
        (loc.x, loc.y)
    }
}

/// Warp the mouse cursor to a specific screen position.
///
/// macOS quirk: `CGWarpMouseCursorPosition` alone often leaves the cursor
/// invisible at its new location until the user nudges the mouse — the
/// system's cursor compositor doesn't redraw on a synthetic warp. The fix
/// is to (a) disassociate hardware-cursor smoothing so the warp is honored
/// crisply, (b) post a synthetic `kCGEventMouseMoved` at the new position
/// so the compositor refreshes, and (c) reassociate.
pub fn warp_mouse(x: f64, y: f64) {
    unsafe {
        let point = CGPoint { x, y };
        CGAssociateMouseAndMouseCursorPosition(false);
        CGWarpMouseCursorPosition(point);
        // CGEventPost is gated out of debug-build unit tests because it
        // adds enough wall-clock latency to push past the 400ms
        // workspace-switch silence window in
        // workspace_switch_silences_external_app_focus_callback. The
        // cursor-visibility wakeup it provides doesn't matter in tests.
        #[cfg(not(test))]
        {
            let event = CGEventCreateMouseEvent(
                std::ptr::null(),
                K_CG_EVENT_MOUSE_MOVED,
                point,
                K_CG_MOUSE_BUTTON_LEFT,
            );
            if !event.is_null() {
                CGEventPost(K_CG_HID_EVENT_TAP, event);
                CFRelease(event as *const std::ffi::c_void);
            }
        }
        CGAssociateMouseAndMouseCursorPosition(true);
    }
}

/// Discover all connected displays and return Monitor structs.
pub fn discover_displays() -> Vec<crate::core::monitor::Monitor> {
    use crate::core::monitor::Monitor;
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let main_id = unsafe { CGMainDisplayID() };

    // Get all display IDs
    let mut display_ids = [0u32; 16];
    let mut count: u32 = 0;
    unsafe {
        CGGetActiveDisplayList(16, display_ids.as_mut_ptr(), &mut count);
    }

    // Get NSScreen list for visible frames
    let ns_screens = NSScreen::screens(mtm);
    let ns_count = ns_screens.len();

    let main_height = unsafe { CGDisplayBounds(main_id).size.height };

    let mut monitors = Vec::new();
    for &did in display_ids.iter().take(count as usize) {
        let cg_bounds = unsafe { CGDisplayBounds(did) };
        let frame = Rect::new(
            cg_bounds.origin.x,
            cg_bounds.origin.y,
            cg_bounds.size.width,
            cg_bounds.size.height,
        );

        // Match by the actual CGDirectDisplayID to avoid ambiguous geometry
        // when monitors share the same width or are stacked vertically.
        let usable_frame =
            find_nsscreen_for_display(&ns_screens, ns_count, did, main_height).unwrap_or(frame);

        monitors.push(Monitor {
            id: did,
            frame,
            usable_frame,
            is_primary: did == main_id,
            active_workspace: 0,
        });
    }

    // macOS reserves the menu-bar zone globally in CG y-coords, even on
    // non-primary displays whose NSScreen.visibleFrame falsely reports
    // the full frame as usable. Two failure modes we have to handle:
    //
    //   (a) Secondary's usable_frame top is ABOVE the primary's (e.g.
    //       widescreen positioned to the left of a notch'd laptop where
    //       the primary's menu-bar zone in CG-y intersects the
    //       secondary). Clamp to primary's usable_frame top.
    //   (b) NSScreen.visibleFrame on the secondary returns the FULL
    //       frame (vis == frame), even though the OS still enforces a
    //       menu-bar zone on that display — observed on macOS Tahoe
    //       after sleep/hotplug cycles on 4K externals. AX position
    //       requests get clamped to y=ns_frame.y + ~30 with no
    //       NSScreen warning. Apply a fallback 30px reservation so
    //       layout math reserves that zone explicitly.
    if let Some(primary_top) = monitors
        .iter()
        .find(|m| m.is_primary)
        .map(|m| m.usable_frame.y)
    {
        for m in monitors.iter_mut() {
            if m.is_primary {
                continue;
            }
            // Case (a): clamp to primary's usable_frame top when the
            // secondary's top is above it AND their CG regions overlap.
            let bottom = m.usable_frame.y + m.usable_frame.height;
            if m.usable_frame.y < primary_top && bottom > primary_top {
                let dy = primary_top - m.usable_frame.y;
                m.usable_frame.y = primary_top;
                m.usable_frame.height = (m.usable_frame.height - dy).max(0.0);
                tracing::debug!(
                    id = m.id,
                    reserved_top = dy,
                    new_y = m.usable_frame.y,
                    new_h = m.usable_frame.height,
                    "applied global menu-bar reservation to secondary (overlap case)"
                );
                continue;
            }
            // Case (b): NSScreen.visibleFrame reported full frame (no
            // reservation) but the OS still enforces one. Detect this
            // by usable_frame.height == frame.height and apply a 30px
            // top reservation.
            if (m.usable_frame.height - m.frame.height).abs() < 0.5
                && (m.usable_frame.y - m.frame.y).abs() < 0.5
            {
                const FALLBACK_MENUBAR_HEIGHT: f64 = 30.0;
                m.usable_frame.y += FALLBACK_MENUBAR_HEIGHT;
                m.usable_frame.height = (m.usable_frame.height - FALLBACK_MENUBAR_HEIGHT).max(0.0);
                tracing::debug!(
                    id = m.id,
                    reserved_top = FALLBACK_MENUBAR_HEIGHT,
                    new_y = m.usable_frame.y,
                    new_h = m.usable_frame.height,
                    "applied fallback menu-bar reservation to secondary (stale NSScreen)"
                );
            }
        }
    }

    tracing::info!(count = monitors.len(), "displays discovered");
    for m in &monitors {
        tracing::debug!(
            id = m.id,
            primary = m.is_primary,
            x = m.usable_frame.x,
            y = m.usable_frame.y,
            w = m.usable_frame.width,
            h = m.usable_frame.height,
            "display"
        );
    }

    monitors
}

/// Find the NSScreen matching a CGDisplay by display ID,
/// and return its visible frame converted to top-left origin.
fn find_nsscreen_for_display(
    screens: &objc2_foundation::NSArray<objc2_app_kit::NSScreen>,
    count: usize,
    display_id: u32,
    main_height: f64,
) -> Option<Rect> {
    for i in 0..count {
        let screen = screens.objectAtIndex(i);
        let frame = screen.frame();
        let visible = screen.visibleFrame();
        let screen_display_id = match nsscreen_display_id(&screen) {
            Some(id) => id,
            None => continue,
        };

        tracing::debug!(
            ns_idx = i,
            ns_display_id = screen_display_id,
            ns_frame_x = frame.origin.x,
            ns_frame_y = frame.origin.y,
            ns_frame_w = frame.size.width,
            ns_frame_h = frame.size.height,
            ns_vis_x = visible.origin.x,
            ns_vis_y = visible.origin.y,
            ns_vis_w = visible.size.width,
            ns_vis_h = visible.size.height,
            display_id,
            "NSScreen candidate"
        );

        if screen_display_id == display_id {
            let usable_frame = top_left_rect_from_visible_frame(
                NSRect {
                    x: visible.origin.x,
                    y: visible.origin.y,
                    width: visible.size.width,
                    height: visible.size.height,
                },
                main_height,
            );
            tracing::debug!(
                ns_idx = i,
                converted_x = usable_frame.x,
                converted_y = usable_frame.y,
                converted_w = usable_frame.width,
                converted_h = usable_frame.height,
                main_height,
                "NSScreen matched → usable_frame"
            );
            return Some(usable_frame);
        }
    }
    tracing::warn!(
        display_id,
        "no NSScreen match found — falling back to CG bounds"
    );
    None
}

fn nsscreen_display_id(screen: &objc2_app_kit::NSScreen) -> Option<u32> {
    let description = screen.deviceDescription();
    let value = description.objectForKey(objc2_foundation::ns_string!("NSScreenNumber"))?;
    let display_id: u32 = unsafe { msg_send![&*value, unsignedIntValue] };
    Some(display_id)
}

/// Register a callback for display configuration changes (hotplug).
/// The callback receives a boolean: true = display added/changed, false = display removed.
pub fn register_display_change_callback(callback: Box<dyn Fn() + Send>) {
    // Store callback in a static to keep it alive
    use std::sync::Mutex;
    static CALLBACK: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

    *CALLBACK.lock().unwrap() = Some(callback);

    unsafe extern "C" fn display_reconfiguration_callback(
        _display: u32,
        _flags: u32,
        _user_info: *mut std::ffi::c_void,
    ) {
        use std::sync::Mutex as StdMutex;
        use std::time::Instant;
        static LAST_FIRE: StdMutex<Option<Instant>> = StdMutex::new(None);

        // Only react to "done" events (after reconfiguration is complete)
        let begin_flag = 1u32 << 0;
        if _flags & begin_flag != 0 {
            return; // Skip "begin" events
        }

        // Debounce: macOS fires multiple "done" events per reconfiguration.
        // Skip if last fire was <500ms ago.
        {
            let mut last = LAST_FIRE.lock().unwrap();
            let now = Instant::now();
            if let Some(t) = *last
                && now.duration_since(t).as_millis() < 500
            {
                return;
            }
            *last = Some(now);
        }

        if let Ok(guard) = CALLBACK.lock()
            && let Some(cb) = guard.as_ref()
        {
            cb();
        }
    }

    unsafe {
        CGDisplayRegisterReconfigurationCallback(
            Some(display_reconfiguration_callback),
            std::ptr::null_mut(),
        );
    }
    tracing::info!("display hotplug callback registered");
}

unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGWarpMouseCursorPosition(new_cursor_position: CGPoint) -> i32;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
    fn CGDisplayRegisterReconfigurationCallback(
        callback: Option<
            unsafe extern "C" fn(display: u32, flags: u32, user_info: *mut std::ffi::c_void),
        >,
        user_info: *mut std::ffi::c_void,
    ) -> i32;
    fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
    #[cfg_attr(test, allow(dead_code))]
    fn CGEventCreateMouseEvent(
        source: *const std::ffi::c_void,
        mouse_type: u32,
        mouse_cursor_position: CGPoint,
        mouse_button: u32,
    ) -> *mut std::ffi::c_void;
    #[cfg_attr(test, allow(dead_code))]
    fn CGEventPost(tap: u32, event: *mut std::ffi::c_void);
    fn CFRelease(cf: *const std::ffi::c_void);
}

#[cfg_attr(test, allow(dead_code))]
const K_CG_EVENT_MOUSE_MOVED: u32 = 5;
#[cfg_attr(test, allow(dead_code))]
const K_CG_HID_EVENT_TAP: u32 = 0;
#[cfg_attr(test, allow(dead_code))]
const K_CG_MOUSE_BUTTON_LEFT: u32 = 0;

#[cfg(test)]
mod tests {
    use super::{NSRect, top_left_rect_from_visible_frame};
    use crate::core::tree::Rect;

    #[test]
    fn converts_main_display_visible_frame_to_top_left_space() {
        let visible = NSRect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1055.0,
        };

        assert_eq!(
            top_left_rect_from_visible_frame(visible, 1080.0),
            Rect::new(0.0, 25.0, 1920.0, 1055.0)
        );
    }

    #[test]
    fn converts_display_below_main_to_top_left_space() {
        let visible = NSRect {
            x: 0.0,
            y: -1080.0,
            width: 1920.0,
            height: 1080.0,
        };

        assert_eq!(
            top_left_rect_from_visible_frame(visible, 1080.0),
            Rect::new(0.0, 1080.0, 1920.0, 1080.0)
        );
    }
}
