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

/// The app-element notifications an observer registers. Kept as a const
/// so `Drop` can unregister exactly what `new` registered.
const APP_NOTIFICATIONS: [&str; 8] = [
    AX_WINDOW_CREATED,
    AX_UI_ELEMENT_DESTROYED,
    AX_FOCUSED_WINDOW_CHANGED,
    AX_WINDOW_MOVED,
    AX_WINDOW_RESIZED,
    AX_TITLE_CHANGED,
    AX_WINDOW_MINIATURIZED,
    AX_WINDOW_DEMINIATURIZED,
];

/// An AXObserver for a single application.
pub struct AppObserver {
    pid: i32,
    _observer: ManuallyDrop<CFRetained<AXObserver>>,
    /// The app element the notifications are registered on — retained so
    /// `Drop` can unregister them (AXObserverRemoveNotification needs it).
    app_element: CFRetained<AXUIElement>,
    _callback: *mut EventCallback,
    /// OBS-D2: notifications whose registration failed (slow-launch apps
    /// like Electron return kAXErrorCannotComplete while their AX server
    /// warms up). Retried by `retry_pending`; an app with unregistered
    /// notifications is otherwise silently poller-only forever.
    pending: std::cell::RefCell<Vec<&'static str>>,
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

        // Subscribe to all notification types on the app element,
        // collecting any that fail so they can be retried.
        let mut pending: Vec<&'static str> = Vec::new();
        for notif in APP_NOTIFICATIONS {
            if !try_add_notification(&observer, app_element, notif, callback_ptr) {
                pending.push(notif);
            }
        }
        if !pending.is_empty() {
            tracing::debug!(pid, ?pending, "some AX notifications not yet registered — will retry");
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
            app_element: unsafe { CFRetained::retain(NonNull::from(app_element)) },
            _callback: callback_ptr,
            pending: std::cell::RefCell::new(pending),
        })
    }

    /// True when every notification is registered (nothing pending).
    pub fn is_fully_registered(&self) -> bool {
        self.pending.borrow().is_empty()
    }

    /// OBS-D2: retry the notifications that failed to register. Cheap
    /// no-op once fully registered. Call opportunistically (e.g. once a
    /// second from the polling tick) so slow-launch apps stop being
    /// poller-only once their AX server is ready.
    pub fn retry_pending(&self) {
        if self.pending.borrow().is_empty() {
            return;
        }
        let mut pending = self.pending.borrow_mut();
        pending.retain(|notif| {
            !try_add_notification(&self._observer, &self.app_element, notif, self._callback)
        });
        if pending.is_empty() {
            tracing::debug!(pid = self.pid, "all AX notifications now registered");
        }
    }
}

/// Register one notification; returns true on success (or already
/// registered). -25708 = notificationAlreadyRegistered.
fn try_add_notification(
    observer: &AXObserver,
    app_element: &AXUIElement,
    notif: &'static str,
    callback_ptr: *mut EventCallback,
) -> bool {
    let notif_str = CFString::from_static_str(notif);
    let err =
        unsafe { observer.add_notification(app_element, &notif_str, callback_ptr as *mut c_void) };
    err == AXError(0) || err == AXError(-25708)
}

impl Drop for AppObserver {
    fn drop(&mut self) {
        // Order matters: detach the runloop source and unregister every
        // notification BEFORE freeing the callback box. Otherwise an
        // AXUIElementDestroyed burst already queued on the observer's
        // mach port (apps emit one right before their pid disappears)
        // can fire observer_callback against the freed box — a UAF — and
        // the retained runloop source leaks one per terminated app.
        unsafe {
            // Remove the source from whatever runloop new() added it to.
            // Observers are created and dropped on the main runloop
            // thread, so current() is the same runloop.
            let source = self._observer.run_loop_source();
            if let Some(run_loop) = CFRunLoop::current() {
                let mode: &CFRunLoopMode =
                    kCFRunLoopCommonModes.expect("kCFRunLoopCommonModes");
                run_loop.remove_source(Some(source.as_ref()), Some(mode));
            }

            for notif in APP_NOTIFICATIONS {
                let notif_str = CFString::from_static_str(notif);
                let _ = self
                    ._observer
                    .remove_notification(&self.app_element, &notif_str);
            }

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
