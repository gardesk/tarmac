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

/// Warp the mouse cursor to a specific screen position.
pub fn warp_mouse(x: f64, y: f64) {
    unsafe {
        let point = CGPoint { x, y };
        CGWarpMouseCursorPosition(point);
        // Reassociate to prevent cursor drift after warp
        CGAssociateMouseAndMouseCursorPosition(true);
    }
}

unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGWarpMouseCursorPosition(new_cursor_position: CGPoint) -> i32;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
}
