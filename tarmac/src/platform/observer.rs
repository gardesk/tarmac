use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr::{self, NonNull};

use objc2_application_services::{AXError, AXObserver, AXUIElement};
use objc2_core_foundation::{
    CFRetained, CFRunLoop, CFRunLoopMode, CFString, kCFRunLoopCommonModes,
};

use super::accessibility::{AxError, AxResult};

/// Notification types we subscribe to.
pub const AX_WINDOW_CREATED: &str = "AXWindowCreated";
pub const AX_UI_ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";
pub const AX_FOCUSED_WINDOW_CHANGED: &str = "AXFocusedWindowChanged";
pub const AX_WINDOW_MOVED: &str = "AXWindowMoved";
pub const AX_WINDOW_RESIZED: &str = "AXWindowResized";
pub const AX_TITLE_CHANGED: &str = "AXTitleChanged";
pub const AX_WINDOW_MINIATURIZED: &str = "AXWindowMiniaturized";
pub const AX_WINDOW_DEMINIATURIZED: &str = "AXWindowDeminiaturized";

/// Events produced by the observer.
#[derive(Debug)]
pub enum WindowEvent {
    Created {
        pid: i32,
        element: CFRetained<AXUIElement>,
    },
    Destroyed {
        pid: i32,
        element: CFRetained<AXUIElement>,
    },
    FocusChanged {
        pid: i32,
        element: CFRetained<AXUIElement>,
    },
    Moved {
        pid: i32,
        element: CFRetained<AXUIElement>,
    },
    Resized {
        pid: i32,
        element: CFRetained<AXUIElement>,
    },
    TitleChanged {
        pid: i32,
        element: CFRetained<AXUIElement>,
    },
    Minimized {
        pid: i32,
        element: CFRetained<AXUIElement>,
    },
    Unminimized {
        pid: i32,
        element: CFRetained<AXUIElement>,
    },
}

/// Type-erased callback storage.
type EventCallback = Box<dyn Fn(WindowEvent)>;

/// An AXObserver for a single application.
pub struct AppObserver {
    pid: i32,
    _observer: ManuallyDrop<CFRetained<AXObserver>>,
    _callback: *mut EventCallback,
}

impl AppObserver {
    /// Create an observer for the given app pid and subscribe to all window events.
    pub fn new(pid: i32, app_element: &AXUIElement, callback: EventCallback) -> AxResult<Self> {
        let callback_ptr = Box::into_raw(Box::new(callback));

        let mut observer_ptr: *mut AXObserver = ptr::null_mut();
        let status = unsafe {
            AXObserver::create(
                pid,
                Some(observer_callback),
                NonNull::new(&mut observer_ptr as *mut *mut AXObserver).unwrap(),
            )
        };
        if status != AXError(0) {
            // Clean up callback on failure
            unsafe {
                let _ = Box::from_raw(callback_ptr);
            }
            return Err(AxError::Ax(status));
        }

        let observer =
            unsafe { CFRetained::from_raw(NonNull::new(observer_ptr).expect("non-null observer")) };

        // Subscribe to all notification types on the app element
        let notifications = [
            AX_WINDOW_CREATED,
            AX_FOCUSED_WINDOW_CHANGED,
            AX_WINDOW_MOVED,
            AX_WINDOW_RESIZED,
            AX_TITLE_CHANGED,
            AX_WINDOW_MINIATURIZED,
            AX_WINDOW_DEMINIATURIZED,
        ];

        for notif in notifications {
            let notif_str = CFString::from_static_str(notif);
            let err = unsafe {
                observer.add_notification(app_element, &notif_str, callback_ptr as *mut c_void)
            };
            if err != AXError(0) && err != AXError(-25708) {
                // -25708 = notificationAlreadyRegistered, ignore
                tracing::trace!(pid, notif, err = ?err, "failed to add notification");
            }
        }

        // Add to current run loop
        let source = unsafe { observer.run_loop_source() };
        if let Some(run_loop) = CFRunLoop::current() {
            let mode: &CFRunLoopMode =
                unsafe { kCFRunLoopCommonModes.expect("kCFRunLoopCommonModes") };
            run_loop.add_source(Some(source.as_ref()), Some(mode));
        }

        Ok(Self {
            pid,
            _observer: ManuallyDrop::new(observer),
            _callback: callback_ptr,
        })
    }
}

impl Drop for AppObserver {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self._observer);
            let _ = Box::from_raw(self._callback);
        }
        tracing::trace!(pid = self.pid, "observer dropped");
    }
}

/// C callback invoked by the AX framework on the main thread.
unsafe extern "C-unwind" fn observer_callback(
    _observer: NonNull<AXObserver>,
    element: NonNull<AXUIElement>,
    notification: NonNull<CFString>,
    user_data: *mut c_void,
) {
    if user_data.is_null() {
        return;
    }

    let callback = unsafe { &*(user_data as *const EventCallback) };
    let element = unsafe { CFRetained::retain(element) };
    let notification = unsafe { CFRetained::retain(notification) };
    let notif_str = notification.to_string();

    // Derive pid from the element (best effort)
    let mut pid: i32 = 0;
    let _ = unsafe { AXUIElementGetPid(&element, NonNull::new(&mut pid as *mut i32).unwrap()) };

    let event = match notif_str.as_str() {
        "AXWindowCreated" => WindowEvent::Created {
            pid,
            element: element.clone(),
        },
        "AXUIElementDestroyed" => WindowEvent::Destroyed {
            pid,
            element: element.clone(),
        },
        "AXFocusedWindowChanged" => WindowEvent::FocusChanged {
            pid,
            element: element.clone(),
        },
        "AXWindowMoved" => WindowEvent::Moved {
            pid,
            element: element.clone(),
        },
        "AXWindowResized" => WindowEvent::Resized {
            pid,
            element: element.clone(),
        },
        "AXTitleChanged" => WindowEvent::TitleChanged {
            pid,
            element: element.clone(),
        },
        "AXWindowMiniaturized" => WindowEvent::Minimized {
            pid,
            element: element.clone(),
        },
        "AXWindowDeminiaturized" => WindowEvent::Unminimized {
            pid,
            element: element.clone(),
        },
        other => {
            tracing::trace!(notification = other, "unhandled AX notification");
            return;
        }
    };

    callback(event);
}

unsafe extern "C" {
    fn AXUIElementGetPid(element: &AXUIElement, pid: NonNull<i32>) -> AXError;
}
