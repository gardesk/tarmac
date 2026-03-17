use std::collections::HashMap;

use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFRetained;

use crate::platform::accessibility::{
    ax_get_position, ax_get_size, ax_get_string, ax_get_window_id, is_manageable_window,
};
use crate::platform::application::{
    discover_all_windows, discover_applications, enumerate_windows,
};
use crate::platform::observer::{AppObserver, WindowEvent};

use super::window::{WindowRegistry, WindowState};

/// Central state for the window manager.
/// Lives on the main thread (not Send/Sync).
pub struct WmState {
    pub registry: WindowRegistry,
    observers: HashMap<i32, AppObserver>, // pid -> observer
}

impl Default for WmState {
    fn default() -> Self {
        Self::new()
    }
}

impl WmState {
    pub fn new() -> Self {
        Self {
            registry: WindowRegistry::new(),
            observers: HashMap::new(),
        }
    }

    /// Discover all existing windows and install observers for all running apps.
    pub fn discover_and_observe(&mut self) {
        let windows = discover_all_windows();
        for w in &windows {
            self.registry.add(WindowState {
                id: w.id,
                app_pid: w.app_pid,
                app_name: w.app_name.clone(),
                app_bundle_id: w.app_bundle_id.clone(),
                title: w.title.clone(),
                role: w.role.clone(),
                subrole: w.subrole.clone(),
                x: w.x,
                y: w.y,
                width: w.width,
                height: w.height,
                floating: false,
                minimized: false,
            });
        }

        tracing::info!(
            windows = self.registry.count(),
            "initial window registry populated"
        );

        let apps = discover_applications();
        for app in &apps {
            self.install_observer_for_app(app.pid, &app.ax_ref, &app.name, &app.bundle_id);
        }

        tracing::info!(observers = self.observers.len(), "observers installed");
    }

    /// Install an observer for a newly launched app.
    pub fn on_app_launched(&mut self, pid: i32, name: &str, bundle_id: &str) {
        let ax_app = unsafe { AXUIElement::new_application(pid) };

        // Enumerate its windows
        let app_info = crate::platform::application::AppInfo {
            pid,
            bundle_id: bundle_id.to_string(),
            name: name.to_string(),
            ax_ref: ax_app.clone(),
        };
        let windows = enumerate_windows(&app_info);
        for w in &windows {
            if !self.registry.contains(w.id) {
                self.registry.add(WindowState {
                    id: w.id,
                    app_pid: w.app_pid,
                    app_name: w.app_name.clone(),
                    app_bundle_id: w.app_bundle_id.clone(),
                    title: w.title.clone(),
                    role: w.role.clone(),
                    subrole: w.subrole.clone(),
                    x: w.x,
                    y: w.y,
                    width: w.width,
                    height: w.height,
                    floating: false,
                    minimized: false,
                });
            }
        }

        self.install_observer_for_app(pid, &ax_app, name, bundle_id);
    }

    /// Remove all windows and observer for a terminated app.
    pub fn on_app_terminated(&mut self, pid: i32) {
        let removed = self.registry.remove_by_pid(pid);
        self.observers.remove(&pid);
        if !removed.is_empty() {
            tracing::info!(
                pid,
                removed = removed.len(),
                "app terminated, windows removed"
            );
        }
    }

    fn install_observer_for_app(
        &mut self,
        pid: i32,
        ax_ref: &CFRetained<AXUIElement>,
        name: &str,
        bundle_id: &str,
    ) {
        if self.observers.contains_key(&pid) {
            return;
        }

        // We can't capture &mut self in the callback since the callback
        // will be called re-entrantly from the run loop. Instead, just log.
        // The main loop will poll/process events in Sprint 2+.
        let cb_name = name.to_string();
        let cb_bundle = bundle_id.to_string();
        let callback = Box::new(move |event: WindowEvent| {
            handle_event_log(&event, &cb_name, &cb_bundle);
        });

        match AppObserver::new(pid, ax_ref, callback) {
            Ok(observer) => {
                tracing::trace!(app = name, pid, "observer installed");
                self.observers.insert(pid, observer);
            }
            Err(e) => {
                tracing::trace!(app = name, pid, err = %e, "failed to create observer");
            }
        }
    }
}

/// Log window events. In future sprints this will update the registry and trigger relayout.
fn handle_event_log(event: &WindowEvent, app_name: &str, _app_bundle: &str) {
    match event {
        WindowEvent::Created { pid: _, element } => {
            if !is_manageable_window(element) {
                return;
            }
            let title = ax_get_string(element, "AXTitle").unwrap_or_default();
            let id = ax_get_window_id(element).unwrap_or(0);
            tracing::info!(id, app = app_name, title = %title, "window created");
        }
        WindowEvent::Destroyed { element, .. } => {
            let id = ax_get_window_id(element).unwrap_or(0);
            tracing::info!(id, app = app_name, "window destroyed");
        }
        WindowEvent::FocusChanged { element, .. } => {
            let id = ax_get_window_id(element).unwrap_or(0);
            let title = ax_get_string(element, "AXTitle").unwrap_or_default();
            tracing::debug!(id, app = app_name, title = %title, "focus changed");
        }
        WindowEvent::Moved { element, .. } => {
            let id = ax_get_window_id(element).unwrap_or(0);
            if let Ok((x, y)) = ax_get_position(element) {
                tracing::debug!(id, x, y, "window moved");
            }
        }
        WindowEvent::Resized { element, .. } => {
            let id = ax_get_window_id(element).unwrap_or(0);
            if let Ok((w, h)) = ax_get_size(element) {
                tracing::debug!(id, w, h, "window resized");
            }
        }
        WindowEvent::TitleChanged { element, .. } => {
            let id = ax_get_window_id(element).unwrap_or(0);
            let title = ax_get_string(element, "AXTitle").unwrap_or_default();
            tracing::debug!(id, title = %title, "title changed");
        }
        WindowEvent::Minimized { .. } => {
            tracing::debug!(app = app_name, "window minimized");
        }
        WindowEvent::Unminimized { .. } => {
            tracing::debug!(app = app_name, "window unminimized");
        }
    }
}
