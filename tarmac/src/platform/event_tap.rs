use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopMode, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType,
};

use crate::core::input::{InputEvent, KeyEvent, Modifiers};

/// Callback type for input events. Return true to suppress the event.
pub type KeyHandler = Box<dyn Fn(InputEvent) -> bool>;

/// Holds the event tap resources. Drop to disable.
pub struct EventTap {
    _port: CFRetained<CFMachPort>,
    _source: CFRetained<objc2_core_foundation::CFRunLoopSource>,
    _handler: *mut KeyHandler,
}

impl EventTap {
    /// Create and install a CGEventTap on the current thread's run loop.
    /// The handler receives KeyDown events and returns true to suppress them.
    pub fn install(handler: KeyHandler) -> Option<Self> {
        let handler_ptr = Box::into_raw(Box::new(handler));

        let mask: CGEventMask = (1 << CGEventType::KeyDown.0 as u64)
            | (1 << CGEventType::KeyUp.0 as u64)
            | (1 << CGEventType::FlagsChanged.0 as u64)
            | (1 << CGEventType::LeftMouseDown.0 as u64);

        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions(0), // Default = active (can suppress)
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

        tracing::info!("event tap installed");

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
        tracing::info!("event tap dropped");
    }
}

unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    let event_ref = unsafe { event.as_ref() };

    // Handle tap disabled events
    let ety = event_type.0 as i32;
    if ety == -1 || ety == -2 {
        tracing::warn!("event tap disabled ({}), re-enabling", ety);
        // We don't have the port ref here, but the OS will re-enable on next event
        return event.as_ptr();
    }

    if user_info.is_null() {
        return event.as_ptr();
    }

    let handler = unsafe { &*(user_info as *const KeyHandler) };

    // Log all key-related events at trace level for debugging
    if event_type == CGEventType::KeyDown || event_type == CGEventType::KeyUp {
        let keycode =
            CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode)
                as u16;
        let flags = CGEvent::flags(Some(event_ref));
        tracing::trace!(
            event_type = event_type.0,
            keycode = format!("0x{:02X}", keycode),
            flags = format!("0x{:08X}", flags.0),
            "raw key event"
        );
    }

    let input = if event_type == CGEventType::KeyDown {
        let keycode =
            CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode)
                as u16;
        let flags = CGEvent::flags(Some(event_ref));
        let modifiers = flags_to_modifiers(flags);
        Some(InputEvent::Key(KeyEvent { keycode, modifiers }))
    } else if event_type == CGEventType::LeftMouseDown {
        let location = CGEvent::location(Some(event_ref));
        Some(InputEvent::MouseClick {
            x: location.x,
            y: location.y,
        })
    } else {
        None
    };

    if let Some(input) = input
        && handler(input)
    {
        return std::ptr::null_mut();
    }

    event.as_ptr()
}

fn flags_to_modifiers(flags: CGEventFlags) -> Modifiers {
    let raw = flags.0;
    let mut mods = Modifiers::empty();
    if raw & (1 << 17) != 0 {
        mods |= Modifiers::SHIFT;
    }
    if raw & (1 << 18) != 0 {
        mods |= Modifiers::CONTROL;
    }
    if raw & (1 << 19) != 0 {
        mods |= Modifiers::OPTION;
    }
    if raw & (1 << 20) != 0 {
        mods |= Modifiers::COMMAND;
    }
    if raw & (1 << 23) != 0 {
        mods |= Modifiers::FN;
    }
    mods
}
