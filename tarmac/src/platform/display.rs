use crate::core::tree::Rect;

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

        // NSScreen origin is bottom-left; convert to top-left
        let menu_bar_y = full_height - visible.y - visible.height;

        Rect::new(visible.x, menu_bar_y, visible.width, visible.height)
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
pub fn warp_mouse(x: f64, y: f64) {
    unsafe {
        let point = CGPoint { x, y };
        CGWarpMouseCursorPosition(point);
        // Reassociate to prevent cursor drift after warp
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

        // Find matching NSScreen for visible frame
        // NSScreen uses bottom-left origin; CG uses top-left
        let usable_frame = find_nsscreen_for_display(
            &ns_screens,
            ns_count,
            cg_bounds.origin.x,
            cg_bounds.size.width,
            main_height,
        )
        .unwrap_or(frame);

        monitors.push(Monitor {
            id: did,
            frame,
            usable_frame,
            is_primary: did == main_id,
            active_workspace: 0,
        });
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

/// Find the NSScreen matching a CGDisplay by x position and width,
/// and return its visible frame converted to top-left origin.
fn find_nsscreen_for_display(
    screens: &objc2_foundation::NSArray<objc2_app_kit::NSScreen>,
    count: usize,
    cg_x: f64,
    cg_width: f64,
    main_height: f64,
) -> Option<Rect> {
    for i in 0..count {
        let screen = screens.objectAtIndex(i);
        let frame = screen.frame();
        let visible = screen.visibleFrame();

        tracing::debug!(
            ns_idx = i,
            ns_frame_x = frame.origin.x,
            ns_frame_y = frame.origin.y,
            ns_frame_w = frame.size.width,
            ns_frame_h = frame.size.height,
            ns_vis_x = visible.origin.x,
            ns_vis_y = visible.origin.y,
            ns_vis_w = visible.size.width,
            ns_vis_h = visible.size.height,
            cg_x,
            cg_width,
            "NSScreen candidate"
        );

        // Match by x position and width (NSScreen frame origin is bottom-left)
        if (frame.origin.x - cg_x).abs() < 1.0 && (frame.size.width - cg_width).abs() < 1.0 {
            // Convert visible frame from bottom-left to top-left origin
            let top_y = main_height - visible.origin.y - visible.size.height;
            tracing::debug!(
                ns_idx = i,
                converted_x = visible.origin.x,
                converted_y = top_y,
                converted_w = visible.size.width,
                converted_h = visible.size.height,
                main_height,
                "NSScreen matched → usable_frame"
            );
            return Some(Rect::new(
                visible.origin.x,
                top_y,
                visible.size.width,
                visible.size.height,
            ));
        }
    }
    tracing::warn!(cg_x, cg_width, "no NSScreen match found — falling back to CG bounds");
    None
}

/// Register a callback for display configuration changes (hotplug).
/// The callback receives a boolean: true = display added/changed, false = display removed.
pub fn register_display_change_callback(callback: Box<dyn Fn()>) {
    // Store callback in a static to keep it alive
    use std::sync::Mutex;
    static CALLBACK: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

    // Safety: the callback is only called from the main thread's CFRunLoop.
    // We use transmute to add Send since Mutex requires it.
    let send_cb: Box<dyn Fn() + Send> = unsafe { std::mem::transmute(callback) };

    *CALLBACK.lock().unwrap() = Some(send_cb);

    unsafe extern "C" fn display_reconfiguration_callback(
        _display: u32,
        _flags: u32,
        _user_info: *mut std::ffi::c_void,
    ) {
        // Only react to "done" events (after reconfiguration is complete)
        let begin_flag = 1u32 << 0;
        if _flags & begin_flag != 0 {
            return; // Skip "begin" events
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
    fn CFRelease(cf: *const std::ffi::c_void);
}
