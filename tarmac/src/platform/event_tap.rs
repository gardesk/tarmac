use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopMode, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

/// Callback for mouse click events. Args: (x, y).
pub type ClickHandler = Box<dyn Fn(f64, f64)>;

/// Holds the event tap resources. Drop to disable.
pub struct EventTap {
    _port: CFRetained<CFMachPort>,
    _source: CFRetained<objc2_core_foundation::CFRunLoopSource>,
    _handler: *mut ClickHandler,
}

impl EventTap {
    /// Create an event tap that intercepts mouse clicks for click-to-focus.
    /// Keyboard hotkeys are handled by Carbon RegisterEventHotKey instead.
    pub fn install(handler: ClickHandler) -> Option<Self> {
        let handler_ptr = Box::into_raw(Box::new(handler));

        let mask: CGEventMask = 1 << CGEventType::LeftMouseDown.0 as u64;

        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::HIDEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions(0),
                mask,
                Some(event_tap_callback),
                handler_ptr as *mut c_void,
            )?
        };

        let source = CFMachPort::new_run_loop_source(None, Some(&port), 0)?;
        if let Some(rl) = CFRunLoop::current() {
            let mode: &CFRunLoopMode =
                unsafe { kCFRunLoopCommonModes.expect("kCFRunLoopCommonModes") };
            rl.add_source(Some(&source), Some(mode));
        }
        CGEvent::tap_enable(&port, true);

        tracing::info!("mouse event tap installed");

        Some(Self {
            _port: port,
            _source: source,
            _handler: handler_ptr,
        })
    }
}

impl Drop for EventTap {
    fn drop(&mut self) {
        CGEvent::tap_enable(&self._port, false);
        unsafe {
            let _ = Box::from_raw(self._handler);
        }
    }
}

unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // Handle tap disabled events
    let ety = event_type.0 as i32;
    if ety == -1 || ety == -2 {
        tracing::warn!("mouse event tap disabled, re-enabling");
        return event.as_ptr();
    }

    if user_info.is_null() || event_type != CGEventType::LeftMouseDown {
        return event.as_ptr();
    }

    let event_ref = unsafe { event.as_ref() };
    let location = CGEvent::location(Some(event_ref));
    let handler = unsafe { &*(user_info as *const ClickHandler) };
    handler(location.x, location.y);

    // Always pass click through — never suppress
    event.as_ptr()
}
