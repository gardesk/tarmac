use std::collections::HashMap;

use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFRetained;

use crate::platform::accessibility::{
    ax_get_position, ax_get_size, ax_get_string, ax_get_window_id, ax_set_position, ax_set_size,
    is_manageable_window,
};
use crate::platform::application::{
    discover_all_windows, discover_applications, enumerate_windows,
};
use crate::platform::display::get_usable_frame;
use crate::platform::observer::{AppObserver, WindowEvent};

use super::tree::{Node, Rect};
use super::window::{WindowId, WindowRegistry, WindowState};

/// Central state for the window manager.
/// Lives on the main thread (not Send/Sync).
pub struct WmState {
    pub registry: WindowRegistry,
    pub tree: Node,
    pub focused: Option<WindowId>,
    ax_refs: HashMap<WindowId, CFRetained<AXUIElement>>,
    observers: HashMap<i32, AppObserver>,
    screen_rect: Rect,
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
            tree: Node::empty(),
            focused: None,
            ax_refs: HashMap::new(),
            observers: HashMap::new(),
            screen_rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
        }
    }

    /// Discover all existing windows, build the BSP tree, and tile.
    pub fn discover_and_observe(&mut self) {
        self.screen_rect = get_usable_frame();
        tracing::info!(
            x = self.screen_rect.x,
            y = self.screen_rect.y,
            w = self.screen_rect.width,
            h = self.screen_rect.height,
            "usable screen frame"
        );

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
            self.ax_refs.insert(w.id, w.ax_ref.clone());
            self.tree
                .insert_with_rect(w.id, self.focused, self.screen_rect);
            self.focused = Some(w.id);
        }

        tracing::info!(
            windows = self.registry.count(),
            tree_nodes = self.tree.window_count(),
            "initial window registry populated"
        );

        // Apply initial layout
        self.apply_layout();

        // Install observers
        let pids: Vec<(i32, String, String)> = self
            .registry
            .all()
            .map(|w| (w.app_pid, w.app_name.clone(), w.app_bundle_id.clone()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        for (pid, name, bundle) in &pids {
            let ax_app = unsafe { AXUIElement::new_application(*pid) };
            self.install_observer_for_app(*pid, &ax_app, name, bundle);
        }

        let apps = discover_applications();
        for app in &apps {
            self.install_observer_for_app(app.pid, &app.ax_ref, &app.name, &app.bundle_id);
        }

        tracing::info!(observers = self.observers.len(), "observers installed");
    }

    /// Recalculate geometries from the BSP tree and apply to all windows.
    pub fn apply_layout(&self) {
        let geometries = self.tree.calculate_geometries(self.screen_rect);
        tracing::debug!(count = geometries.len(), "applying layout");

        for (wid, rect) in &geometries {
            if let Some(ax_ref) = self.ax_refs.get(wid) {
                if let Err(e) = ax_set_position(ax_ref, rect.x, rect.y) {
                    tracing::trace!(wid, err = %e, "failed to set position");
                }
                if let Err(e) = ax_set_size(ax_ref, rect.width, rect.height) {
                    tracing::trace!(wid, err = %e, "failed to set size");
                }
            }
        }
    }

    /// Handle a window event from an AX observer.
    pub fn handle_event(&mut self, event: &WindowEvent, app_name: &str, app_bundle: &str) {
        match event {
            WindowEvent::Created {
                pid: app_pid,
                element,
            } => {
                if !is_manageable_window(element) {
                    return;
                }
                let id = match ax_get_window_id(element) {
                    Ok(id) => id,
                    Err(_) => return,
                };
                if self.registry.contains(id) {
                    return;
                }

                let title = ax_get_string(element, "AXTitle").unwrap_or_default();
                let (x, y) = ax_get_position(element).unwrap_or((0.0, 0.0));
                let (w, h) = ax_get_size(element).unwrap_or((0.0, 0.0));

                tracing::info!(id, app = app_name, title = %title, "window created → tiling");

                self.registry.add(WindowState {
                    id,
                    app_pid: *app_pid,
                    app_name: app_name.to_string(),
                    app_bundle_id: app_bundle.to_string(),
                    title,
                    role: "AXWindow".to_string(),
                    subrole: "AXStandardWindow".to_string(),
                    x,
                    y,
                    width: w,
                    height: h,
                    floating: false,
                    minimized: false,
                });
                self.ax_refs.insert(id, element.clone());
                self.tree
                    .insert_with_rect(id, self.focused, self.screen_rect);
                self.focused = Some(id);
                self.apply_layout();
            }
            WindowEvent::Destroyed { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && self.registry.contains(id)
                {
                    tracing::info!(id, app = app_name, "window destroyed → retiling");
                    self.registry.remove(id);
                    self.ax_refs.remove(&id);
                    self.tree.remove(id);
                    if self.focused == Some(id) {
                        self.focused = self.tree.first_window();
                    }
                    self.apply_layout();
                }
            }
            WindowEvent::FocusChanged { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && self.registry.contains(id)
                {
                    self.focused = Some(id);
                    let title = ax_get_string(element, "AXTitle").unwrap_or_default();
                    tracing::debug!(id, app = app_name, title = %title, "focus changed");
                }
            }
            WindowEvent::Moved { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && let Ok((x, y)) = ax_get_position(element)
                    && let Some(w) = self.registry.get(id)
                {
                    let (width, height) = (w.width, w.height);
                    self.registry.update_geometry(id, x, y, width, height);
                }
            }
            WindowEvent::Resized { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && let (Ok((x, y)), Ok((w, h))) =
                        (ax_get_position(element), ax_get_size(element))
                {
                    self.registry.update_geometry(id, x, y, w, h);
                }
            }
            WindowEvent::TitleChanged { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && let Ok(title) = ax_get_string(element, "AXTitle")
                {
                    self.registry.update_title(id, title);
                }
            }
            WindowEvent::Minimized { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && let Some(w) = self.registry.get_mut(id)
                {
                    w.minimized = true;
                }
            }
            WindowEvent::Unminimized { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && let Some(w) = self.registry.get_mut(id)
                {
                    w.minimized = false;
                }
            }
        }
    }

    /// Install an observer for a newly launched app.
    pub fn on_app_launched(&mut self, pid: i32, name: &str, bundle_id: &str) {
        let ax_app = unsafe { AXUIElement::new_application(pid) };

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
                self.ax_refs.insert(w.id, w.ax_ref.clone());
                self.tree
                    .insert_with_rect(w.id, self.focused, self.screen_rect);
                self.focused = Some(w.id);
            }
        }
        if !windows.is_empty() {
            self.apply_layout();
        }

        self.install_observer_for_app(pid, &ax_app, name, bundle_id);
    }

    /// Remove all windows and observer for a terminated app.
    pub fn on_app_terminated(&mut self, pid: i32) {
        let removed = self.registry.remove_by_pid(pid);
        self.observers.remove(&pid);
        for w in &removed {
            self.ax_refs.remove(&w.id);
            self.tree.remove(w.id);
        }
        if !removed.is_empty() {
            if self
                .focused
                .is_some_and(|f| removed.iter().any(|w| w.id == f))
            {
                self.focused = self.tree.first_window();
            }
            self.apply_layout();
            tracing::info!(pid, removed = removed.len(), "app terminated → retiled");
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

        let cb_name = name.to_string();
        let cb_bundle = bundle_id.to_string();
        let callback = Box::new(move |event: WindowEvent| {
            // Log-only for now. Real event handling requires main-thread state access.
            // Sprint 2 wires this via the event loop. For now, log.
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
