//! Window border overlays using transparent NSWindows.
//! Each managed window can have a border overlay that follows its geometry.
//! Borders are borderless, transparent, click-through NSWindows that draw
//! a colored rectangle using Core Graphics.

use std::collections::HashMap;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, msg_send_id};

use crate::core::tree::Rect;

type WindowId = u32;

/// RGBA color for border drawing.
#[derive(Debug, Clone, Copy)]
pub struct BorderColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl BorderColor {
    /// Parse a hex color string (#RRGGBB or #RRGGBBAA).
    pub fn from_hex(hex: &str) -> Self {
        let hex = hex.trim_start_matches('#');
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f64 / 255.0;
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f64 / 255.0;
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f64 / 255.0;
        let a = if hex.len() >= 8 {
            u8::from_str_radix(&hex[6..8], 16).unwrap_or(255) as f64 / 255.0
        } else {
            1.0
        };
        Self { r, g, b, a }
    }
}

/// Manages border overlay windows for all tracked windows.
pub struct BorderManager {
    /// Map from managed window ID to its overlay CGWindowID.
    overlays: HashMap<WindowId, u32>,
    pub border_width: f64,
    pub focused_color: BorderColor,
    pub unfocused_color: BorderColor,
    pub radius: f64,
}

impl BorderManager {
    pub fn new() -> Self {
        Self {
            overlays: HashMap::new(),
            border_width: 0.0,
            focused_color: BorderColor::from_hex("#5294e2"),
            unfocused_color: BorderColor::from_hex("#2d2d2d"),
            radius: 10.0,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.border_width > 0.0
    }

    /// Create or update the border overlay for a window.
    pub fn update_border(&mut self, wid: WindowId, rect: Rect, focused: bool) {
        if !self.is_enabled() {
            return;
        }

        let color = if focused {
            self.focused_color
        } else {
            self.unfocused_color
        };

        let bw = self.border_width;
        // Border frame is slightly larger than the window
        let border_rect = Rect::new(
            rect.x - bw,
            rect.y - bw,
            rect.width + 2.0 * bw,
            rect.height + 2.0 * bw,
        );

        if let Some(&overlay_wid) = self.overlays.get(&wid) {
            // Update existing overlay position/size and redraw
            update_overlay(overlay_wid, border_rect, color, bw, self.radius);
        } else {
            // Create new overlay
            if let Some(overlay_wid) = create_overlay(border_rect, color, bw, self.radius) {
                self.overlays.insert(wid, overlay_wid);
            }
        }
    }

    /// Hide the border for a window.
    pub fn hide_border(&self, wid: WindowId) {
        if let Some(&overlay_wid) = self.overlays.get(&wid) {
            crate::platform::skylight::set_window_alpha(overlay_wid, 0.0);
        }
    }

    /// Show the border for a window.
    pub fn show_border(&self, wid: WindowId) {
        if let Some(&overlay_wid) = self.overlays.get(&wid) {
            crate::platform::skylight::set_window_alpha(overlay_wid, 1.0);
        }
    }

    /// Remove the border overlay for a window.
    pub fn remove_border(&mut self, wid: WindowId) {
        if let Some(overlay_wid) = self.overlays.remove(&wid) {
            destroy_overlay(overlay_wid);
        }
    }

    /// Remove all border overlays.
    pub fn remove_all(&mut self) {
        let wids: Vec<u32> = self.overlays.values().copied().collect();
        for overlay_wid in wids {
            destroy_overlay(overlay_wid);
        }
        self.overlays.clear();
    }

    /// Update focus: set focused border on new window, unfocused on old.
    pub fn update_focus(&mut self, old_focused: Option<WindowId>, new_focused: Option<WindowId>, get_rect: impl Fn(WindowId) -> Option<Rect>) {
        if !self.is_enabled() {
            return;
        }

        if let Some(old) = old_focused {
            if let Some(rect) = get_rect(old) {
                self.update_border(old, rect, false);
            }
        }
        if let Some(new) = new_focused {
            if let Some(rect) = get_rect(new) {
                self.update_border(new, rect, true);
            }
        }
    }
}

// --- SkyLight-based overlay implementation ---
// Uses SLSNewWindow to create a WindowServer-level overlay, avoiding
// the need for a full NSWindow + NSView hierarchy.

unsafe extern "C" {
    fn SLSMainConnectionID() -> i32;
    fn SLSNewWindow(
        cid: i32,
        window_type: i32,
        x: f64,
        y: f64,
        region: *const c_void,
        wid_out: *mut u32,
    ) -> i32;
    fn SLSReleaseWindow(cid: i32, wid: u32) -> i32;
    fn SLSSetWindowAlpha(cid: i32, wid: u32, alpha: f32) -> i32;
    fn SLSSetWindowLevel(cid: i32, wid: u32, level: i32) -> i32;
    fn SLSOrderWindow(cid: i32, wid: u32, mode: i32, relative_to: u32) -> i32;
    fn SLSSetWindowResolution(cid: i32, wid: u32, resolution: f64) -> i32;

    fn CGWindowContextCreate(cid: i32, wid: u32, options: *const c_void) -> *mut c_void;

    // Core Graphics drawing
    fn CGContextSetRGBStrokeColor(ctx: *mut c_void, r: f64, g: f64, b: f64, a: f64);
    fn CGContextSetLineWidth(ctx: *mut c_void, width: f64);
    fn CGContextAddRoundedRect(ctx: *mut c_void, rect: CGRect, rx: f64, ry: f64);
    fn CGContextClearRect(ctx: *mut c_void, rect: CGRect);
    fn CGContextStrokePath(ctx: *mut c_void);
    fn CGContextFlush(ctx: *mut c_void);
    fn CGContextRelease(ctx: *mut c_void);

    fn CGSNewRegionWithRect(rect: *const CGRect, region: *mut *const c_void) -> i32;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn create_overlay(rect: Rect, color: BorderColor, bw: f64, radius: f64) -> Option<u32> {
    let cid = unsafe { SLSMainConnectionID() };
    let cg_rect = CGRect {
        x: 0.0,
        y: 0.0,
        width: rect.width,
        height: rect.height,
    };

    let mut region: *const c_void = std::ptr::null();
    let err = unsafe { CGSNewRegionWithRect(&cg_rect, &mut region) };
    if err != 0 || region.is_null() {
        tracing::warn!("CGSNewRegionWithRect failed: {}", err);
        return None;
    }

    let mut wid: u32 = 0;
    // window_type 2 = kCGBackingStoreBuffered
    let err = unsafe { SLSNewWindow(cid, 2, rect.x, rect.y, region, &mut wid) };
    if err != 0 {
        tracing::warn!("SLSNewWindow failed: {}", err);
        return None;
    }

    // Set window above all normal windows (level 20 = above floating)
    unsafe {
        SLSSetWindowLevel(cid, wid, 20);
        SLSOrderWindow(cid, wid, 1, 0); // kCGWindowAbove
    }

    // Get Retina scale factor
    let scale = get_main_screen_scale();
    if scale > 1.0 {
        unsafe { SLSSetWindowResolution(cid, wid, scale); }
    }

    draw_border(cid, wid, rect, color, bw, radius, scale);

    Some(wid)
}

fn update_overlay(wid: u32, rect: Rect, color: BorderColor, bw: f64, radius: f64) {
    let cid = unsafe { SLSMainConnectionID() };

    // Move the overlay window
    let point = super::skylight::CGPoint {
        x: rect.x,
        y: rect.y,
    };
    unsafe {
        super::skylight::SLSMoveWindow(cid, wid, &point);
    }

    // Resize and redraw
    let scale = get_main_screen_scale();
    draw_border(cid, wid, rect, color, bw, radius, scale);

    // Ensure visible
    unsafe {
        SLSSetWindowAlpha(cid, wid, 1.0);
        SLSOrderWindow(cid, wid, 1, 0);
    }
}

fn destroy_overlay(wid: u32) {
    let cid = unsafe { SLSMainConnectionID() };
    unsafe {
        SLSReleaseWindow(cid, wid);
    }
}

fn draw_border(
    cid: i32,
    wid: u32,
    rect: Rect,
    color: BorderColor,
    bw: f64,
    radius: f64,
    scale: f64,
) {
    let ctx = unsafe { CGWindowContextCreate(cid, wid, std::ptr::null()) };
    if ctx.is_null() {
        return;
    }

    let w = rect.width * scale;
    let h = rect.height * scale;

    // Clear the context
    let full = CGRect {
        x: 0.0,
        y: 0.0,
        width: w,
        height: h,
    };
    unsafe {
        CGContextClearRect(ctx, full);
    }

    // Draw the border rectangle (inset by half the border width)
    let half_bw = bw * scale / 2.0;
    let border = CGRect {
        x: half_bw,
        y: half_bw,
        width: w - bw * scale,
        height: h - bw * scale,
    };

    unsafe {
        CGContextSetRGBStrokeColor(ctx, color.r, color.g, color.b, color.a);
        CGContextSetLineWidth(ctx, bw * scale);
        CGContextAddRoundedRect(ctx, border, radius * scale, radius * scale);
        CGContextStrokePath(ctx);
        CGContextFlush(ctx);
        CGContextRelease(ctx);
    }
}

fn get_main_screen_scale() -> f64 {
    // Use NSScreen.mainScreen.backingScaleFactor
    unsafe {
        let cls = objc2::runtime::AnyClass::get(c"NSScreen").unwrap();
        let screen: *const AnyObject = msg_send![cls, mainScreen];
        if screen.is_null() {
            return 1.0;
        }
        let scale: f64 = msg_send![screen, backingScaleFactor];
        scale
    }
}
