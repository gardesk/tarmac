use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopMode, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

/// Mouse events delivered by the event tap.
#[derive(Debug, Clone, Copy)]
pub enum MouseEvent {
    Click { x: f64, y: f64 },
    Moved { x: f64, y: f64 },
    LeftDown { x: f64, y: f64, cmd_held: bool },
    LeftDragged { x: f64, y: f64 },
    LeftUp { x: f64, y: f64 },
    RightDown { x: f64, y: f64, cmd_held: bool },
    RightDragged { x: f64, y: f64 },
    RightUp { x: f64, y: f64 },
}

/// Callback for mouse events.
pub type MouseHandler = Box<dyn Fn(MouseEvent)>;

/// Holds the event tap resources. Drop to disable.
pub struct EventTap {
    _port: CFRetained<CFMachPort>,
    _source: CFRetained<objc2_core_foundation::CFRunLoopSource>,
    _handler: *mut MouseHandler,
}

impl EventTap {
    pub fn install(handler: MouseHandler) -> Option<Self> {
        let handler_ptr = Box::into_raw(Box::new(handler));

        let mask: CGEventMask = (1 << CGEventType::LeftMouseDown.0 as u64)
            | (1 << CGEventType::LeftMouseUp.0 as u64)
            | (1 << CGEventType::LeftMouseDragged.0 as u64)
            | (1 << CGEventType::RightMouseDown.0 as u64)
            | (1 << CGEventType::RightMouseUp.0 as u64)
            | (1 << CGEventType::RightMouseDragged.0 as u64)
            | (1 << CGEventType::MouseMoved.0 as u64);

        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::HIDEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions(1), // ListenOnly
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

fn has_cmd(flags: CGEventFlags) -> bool {
    flags.0 & (1 << 20) != 0
}

unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    let ety = event_type.0 as i32;
    if ety == -1 || ety == -2 {
        return event.as_ptr();
    }

    if user_info.is_null() {
        return event.as_ptr();
    }

    let event_ref = unsafe { event.as_ref() };
    let handler = unsafe { &*(user_info as *const MouseHandler) };
    let loc = CGEvent::location(Some(event_ref));

    let mouse_event = match event_type {
        CGEventType::LeftMouseDown => {
            let flags = CGEvent::flags(Some(event_ref));
            Some(MouseEvent::LeftDown {
                x: loc.x,
                y: loc.y,
                cmd_held: has_cmd(flags),
            })
        }
        CGEventType::LeftMouseUp => Some(MouseEvent::LeftUp { x: loc.x, y: loc.y }),
        CGEventType::LeftMouseDragged => Some(MouseEvent::LeftDragged { x: loc.x, y: loc.y }),
        CGEventType::RightMouseDown => {
            let flags = CGEvent::flags(Some(event_ref));
            Some(MouseEvent::RightDown {
                x: loc.x,
                y: loc.y,
                cmd_held: has_cmd(flags),
            })
        }
        CGEventType::RightMouseUp => Some(MouseEvent::RightUp { x: loc.x, y: loc.y }),
        CGEventType::RightMouseDragged => Some(MouseEvent::RightDragged { x: loc.x, y: loc.y }),
        CGEventType::MouseMoved => Some(MouseEvent::Moved { x: loc.x, y: loc.y }),
        _ => None,
    };

    // Also send Click for click-to-focus
    if event_type == CGEventType::LeftMouseDown {
        handler(MouseEvent::Click { x: loc.x, y: loc.y });
    }

    if let Some(evt) = mouse_event {
        handler(evt);
    }

    event.as_ptr()
}
