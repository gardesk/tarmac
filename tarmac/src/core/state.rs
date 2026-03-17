use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFRetained;

use crate::platform::accessibility::{
    ax_copy_attribute, ax_get_position, ax_get_size, ax_get_string, ax_get_window_id,
    ax_perform_action, ax_set_position, ax_set_size, is_manageable_window,
};
use crate::platform::application::{
    discover_all_windows, discover_applications, enumerate_windows,
};
use crate::platform::display::{get_usable_frame, warp_mouse};
use crate::platform::observer::{AppObserver, WindowEvent};

use super::tree::{Node, Rect};
use super::window::{WindowId, WindowRegistry, WindowState};
use super::workspace::{WorkspaceId, WorkspaceManager};

struct QueuedEvent {
    event: WindowEvent,
    app_name: String,
    app_bundle: String,
}

/// Central state for the window manager.
pub struct WmState {
    pub registry: WindowRegistry,
    pub workspaces: WorkspaceManager,
    ax_refs: HashMap<WindowId, CFRetained<AXUIElement>>,
    observers: HashMap<i32, AppObserver>,
    screen_rect: Rect,
    event_queue: Rc<RefCell<Vec<QueuedEvent>>>,
    /// Suppress focus-follows-mouse briefly after mouse warp to prevent feedback loops
    ffm_cooldown_until: Option<std::time::Instant>,
    /// Last window that had focus-follows-mouse focus (to avoid redundant focus calls)
    ffm_last_window: Option<WindowId>,
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
            workspaces: WorkspaceManager::new(),
            ax_refs: HashMap::new(),
            observers: HashMap::new(),
            screen_rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            event_queue: Rc::new(RefCell::new(Vec::new())),
            ffm_cooldown_until: None,
            ffm_last_window: None,
        }
    }

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
            let ws = self.workspaces.active_mut();
            ws.tree.insert_with_rect(w.id, ws.focused, self.screen_rect);
            ws.record_focus(w.id);
        }

        tracing::info!(
            windows = self.registry.count(),
            "initial window registry populated"
        );

        self.apply_layout();
        self.install_observers_for_all();

        tracing::info!(observers = self.observers.len(), "observers installed");
    }

    pub fn process_events(&mut self) {
        let events: Vec<QueuedEvent> = self.event_queue.borrow_mut().drain(..).collect();
        for queued in events {
            self.handle_event(&queued.event, &queued.app_name, &queued.app_bundle);
        }
    }

    // --- Layout ---

    pub fn apply_layout(&self) {
        let geometries = self
            .workspaces
            .active()
            .tree
            .calculate_geometries(self.screen_rect);
        for (wid, rect) in &geometries {
            if let Some(ax_ref) = self.ax_refs.get(wid) {
                let _ = ax_set_position(ax_ref, rect.x, rect.y);
                let _ = ax_set_size(ax_ref, rect.width, rect.height);
            }
        }
    }

    // --- Window operations ---

    pub fn focus_direction(&mut self, direction: super::tree::Direction) {
        let ws = self.workspaces.active();
        let focused = match ws.focused {
            Some(f) => f,
            None => return,
        };
        let geoms = ws.tree.calculate_geometries(self.screen_rect);
        if let Some(target) = Node::find_adjacent(&geoms, focused, direction) {
            self.focus_window(target);
            // Mouse follows focus: warp cursor to the center of the newly focused window
            if let Some((_, rect)) = geoms.iter().find(|(id, _)| *id == target) {
                warp_mouse_to_center(rect);
                // Suppress focus-follows-mouse for 200ms to prevent feedback loop
                self.ffm_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
                self.ffm_last_window = Some(target);
            }
        }
    }

    pub fn swap_direction(&mut self, direction: super::tree::Direction) {
        let ws = self.workspaces.active();
        let focused = match ws.focused {
            Some(f) => f,
            None => return,
        };
        let geoms = ws.tree.calculate_geometries(self.screen_rect);
        if let Some(target) = Node::find_adjacent(&geoms, focused, direction)
            && self.workspaces.active_mut().tree.swap(focused, target)
        {
            self.apply_layout();
        }
    }

    pub fn resize_direction(&mut self, direction: super::tree::Direction) {
        let focused = match self.workspaces.active().focused {
            Some(f) => f,
            None => return,
        };
        if self
            .workspaces
            .active_mut()
            .tree
            .resize(focused, direction, 0.05)
        {
            self.apply_layout();
        }
    }

    pub fn equalize(&mut self) {
        self.workspaces.active_mut().tree.equalize();
        self.apply_layout();
    }

    pub fn focus_window(&mut self, id: WindowId) {
        if let Some(ax_ref) = self.ax_refs.get(&id) {
            let _ = ax_perform_action(ax_ref, "AXRaise");
            if let Some(w) = self.registry.get(id) {
                let ax_app = unsafe { AXUIElement::new_application(w.app_pid) };
                let key = objc2_core_foundation::CFString::from_static_str("AXFrontmost");
                let _ = crate::platform::accessibility::ax_set_bool(&ax_app, &key, true);
            }
            self.workspaces.active_mut().record_focus(id);
            // Ensure all floating windows stay above tiled windows
            self.raise_floating_windows();
            tracing::debug!(id, "focused window");
        }
    }

    /// Raise all floating windows on the active workspace so they stay above tiled.
    /// Uses AXRaise which works within an app's window stack. For cross-app raising,
    /// we briefly set the floating window's app as frontmost then restore.
    fn raise_floating_windows(&self) {
        let ws = self.workspaces.active();
        if ws.floating.is_empty() {
            return;
        }

        let focused_id = ws.focused;
        let focused_is_floating = focused_id.is_some_and(|f| ws.is_floating(f));

        // Only do cross-app raising if the focused window is tiled
        // (meaning a tiled window might be covering a floating window from another app)
        if !focused_is_floating {
            for fw in &ws.floating {
                if let Some(ax_ref) = self.ax_refs.get(&fw.id) {
                    if let Some(w) = self.registry.get(fw.id) {
                        // Activate floating window's app and raise
                        let ax_app = unsafe { AXUIElement::new_application(w.app_pid) };
                        let key = objc2_core_foundation::CFString::from_static_str("AXFrontmost");
                        let _ = crate::platform::accessibility::ax_set_bool(&ax_app, &key, true);
                    }
                    let _ = ax_perform_action(ax_ref, "AXRaise");
                }
            }
        } else {
            // Focused is floating — just AXRaise each floater (same-app raise)
            for fw in &ws.floating {
                if let Some(ax_ref) = self.ax_refs.get(&fw.id) {
                    let _ = ax_perform_action(ax_ref, "AXRaise");
                }
            }
        }
    }

    pub fn close_focused(&mut self) {
        let focused = match self.workspaces.active().focused {
            Some(f) => f,
            None => return,
        };
        if let Some(ax_ref) = self.ax_refs.get(&focused) {
            let close_attr = objc2_core_foundation::CFString::from_static_str("AXCloseButton");
            if let Ok(close_btn) = ax_copy_attribute(ax_ref, &close_attr) {
                let btn_ptr =
                    &*close_btn as *const objc2_core_foundation::CFType as *const AXUIElement;
                let btn_ref = unsafe { &*btn_ptr };
                let _ = ax_perform_action(btn_ref, "AXPress");
                tracing::info!(id = focused, "close button pressed");
            }
        }
    }

    /// Focus-follows-mouse: focus the window under the cursor.
    /// Checks floating windows first (they're visually on top).
    pub fn mouse_moved(&mut self, x: f64, y: f64) {
        // Check cooldown (suppress after mouse warp to prevent feedback loops)
        if let Some(until) = self.ffm_cooldown_until {
            if std::time::Instant::now() < until {
                return;
            }
            self.ffm_cooldown_until = None;
        }

        let ws = self.workspaces.active();

        // Check floating windows first — they're visually on top
        let floating_under = ws
            .floating
            .iter()
            .rev() // Check most recently focused first
            .find(|fw| fw.geometry.contains_point(x, y))
            .map(|fw| fw.id);

        let window_under = if floating_under.is_some() {
            floating_under
        } else {
            // Check tiled windows
            let geoms = ws.tree.calculate_geometries(self.screen_rect);
            geoms
                .iter()
                .find(|(_, rect)| rect.contains_point(x, y))
                .map(|(id, _)| *id)
        };

        // Only refocus if the window changed
        if window_under != self.ffm_last_window {
            self.ffm_last_window = window_under;
            if let Some(id) = window_under
                && self.workspaces.active().focused != Some(id)
            {
                self.focus_window(id);
            }
        }
    }

    pub fn toggle_float(&mut self) {
        let focused = match self.workspaces.active().focused {
            Some(f) => f,
            None => return,
        };
        if self
            .workspaces
            .active_mut()
            .toggle_float(focused, self.screen_rect)
        {
            self.apply_layout();
            // If now floating, position at its stored geometry
            if self.workspaces.active().is_floating(focused) {
                if let Some(fw) = self
                    .workspaces
                    .active()
                    .floating
                    .iter()
                    .find(|f| f.id == focused)
                    && let Some(ax_ref) = self.ax_refs.get(&focused)
                {
                    let _ = ax_set_position(ax_ref, fw.geometry.x, fw.geometry.y);
                    let _ = ax_set_size(ax_ref, fw.geometry.width, fw.geometry.height);
                }
                tracing::info!(id = focused, "window floated");
            } else {
                tracing::info!(id = focused, "window tiled");
            }
        }
    }

    pub fn click_to_focus(&mut self, x: f64, y: f64) {
        let ws = self.workspaces.active();

        // Check floating first
        let floating_hit = ws
            .floating
            .iter()
            .rev()
            .find(|fw| fw.geometry.contains_point(x, y))
            .map(|fw| fw.id);

        let id = if let Some(fid) = floating_hit {
            Some(fid)
        } else {
            let geoms = ws.tree.calculate_geometries(self.screen_rect);
            geoms
                .iter()
                .find(|(_, rect)| rect.contains_point(x, y))
                .map(|(id, _)| *id)
        };

        if let Some(id) = id
            && self.workspaces.active().focused != Some(id)
        {
            self.focus_window(id);
        }
    }

    // --- Workspace operations ---

    pub fn switch_workspace(&mut self, num: u8) {
        let target = WorkspaceId::Numbered(num);
        tracing::info!(from = %self.workspaces.active_id(), to = %target, "switching workspace");
        let transition = self.workspaces.switch_to(target, self.screen_rect);

        tracing::debug!(
            hide = transition.hide.len(),
            show = transition.show.len(),
            "workspace transition"
        );

        // Hide using AeroSpace's exact approach:
        // Position at (1 - width, visibleRect.maxY - 1)
        // Right edge at x=1, top edge 1px above screen bottom.
        let vis_max_y = self.screen_rect.y + self.screen_rect.height;
        for wid in &transition.hide {
            if let Some(ax_ref) = self.ax_refs.get(wid) {
                let (w, _h) = ax_get_size(ax_ref).unwrap_or((2048.0, 1400.0));
                let hide_x = 1.0 - w;
                let hide_y = vis_max_y - 1.0;
                let _ = ax_set_position(ax_ref, hide_x, hide_y);
                if let Ok((ax, ay)) = ax_get_position(ax_ref) {
                    tracing::debug!(wid, req_x = hide_x, req_y = hide_y, ax, ay, "hidden");
                }
            }
        }

        // Show windows: position and size
        for (wid, rect) in &transition.show {
            if let Some(ax_ref) = self.ax_refs.get(wid) {
                let _ = ax_set_position(ax_ref, rect.x, rect.y);
                let _ = ax_set_size(ax_ref, rect.width, rect.height);
            }
        }

        // Focus
        if let Some(focus_id) = transition.focus {
            self.focus_window(focus_id);
        }

        tracing::info!(ws = %self.workspaces.active_id(), "switched workspace");
    }

    pub fn move_to_workspace(&mut self, num: u8) {
        let focused = match self.workspaces.active().focused {
            Some(f) => f,
            None => return,
        };

        let target = WorkspaceId::Numbered(num);
        if self
            .workspaces
            .move_window_to(focused, target.clone(), self.screen_rect)
        {
            // Hide the moved window
            if let Some(ax_ref) = self.ax_refs.get(&focused) {
                let vis_max_y = self.screen_rect.y + self.screen_rect.height;
                let (w, _h) = ax_get_size(ax_ref).unwrap_or((2048.0, 1400.0));
                let _ = ax_set_position(ax_ref, 1.0 - w, vis_max_y - 1.0);
            }

            // Retile current workspace
            self.apply_layout();

            // Focus next window on current workspace
            if let Some(next) = self.workspaces.active().focused {
                self.focus_window(next);
            }

            tracing::info!(id = focused, target = %target, "moved window to workspace");
        }
    }

    // --- App lifecycle ---

    pub fn on_new_window_detected(&mut self, pid: i32, owner: &str, _wid: u32) {
        if self.registry.all().any(|w| w.app_pid == pid) && self.observers.contains_key(&pid) {
            self.process_events();
            return;
        }

        tracing::info!(pid, owner, "new window detected, enumerating");
        let ax_app = unsafe { AXUIElement::new_application(pid) };
        let app_info = crate::platform::application::AppInfo {
            pid,
            bundle_id: String::new(),
            name: owner.to_string(),
            ax_ref: ax_app.clone(),
        };
        let windows = enumerate_windows(&app_info);

        let mut added = false;
        for w in &windows {
            if !self.registry.contains(w.id) {
                self.add_window_to_active(
                    &w.id,
                    w.app_pid,
                    &w.app_name,
                    &w.app_bundle_id,
                    &w.title,
                    &w.role,
                    &w.subrole,
                    w.x,
                    w.y,
                    w.width,
                    w.height,
                    w.ax_ref.clone(),
                );
                added = true;
            }
        }
        if added {
            self.apply_layout();
        }
        self.install_observer_for_app(pid, &ax_app, owner, "");
    }

    pub fn on_window_closed(&mut self, wid: u32) {
        let id = wid as WindowId;
        if !self.registry.contains(id) {
            return;
        }
        tracing::info!(id, "window closed → retiling");
        self.registry.remove(id);
        self.ax_refs.remove(&id);
        self.workspaces.active_mut().tree.remove(id);
        let ws = self.workspaces.active_mut();
        if ws.focused == Some(id) {
            ws.pop_focus();
        }
        self.apply_layout();
    }

    pub fn on_app_launched(&mut self, pid: i32, name: &str, bundle_id: &str) {
        tracing::info!(pid, app = name, "on_app_launched");
        let ax_app = unsafe { AXUIElement::new_application(pid) };
        let app_info = crate::platform::application::AppInfo {
            pid,
            bundle_id: bundle_id.to_string(),
            name: name.to_string(),
            ax_ref: ax_app.clone(),
        };
        let windows = enumerate_windows(&app_info);

        let mut added = false;
        for w in &windows {
            if !self.registry.contains(w.id) {
                self.add_window_to_active(
                    &w.id,
                    w.app_pid,
                    &w.app_name,
                    &w.app_bundle_id,
                    &w.title,
                    &w.role,
                    &w.subrole,
                    w.x,
                    w.y,
                    w.width,
                    w.height,
                    w.ax_ref.clone(),
                );
                added = true;
            }
        }
        if added {
            self.apply_layout();
        }
        self.install_observer_for_app(pid, &ax_app, name, bundle_id);
    }

    pub fn on_app_terminated(&mut self, pid: i32) {
        let removed = self.registry.remove_by_pid(pid);
        self.observers.remove(&pid);
        for w in &removed {
            self.ax_refs.remove(&w.id);
            self.workspaces.active_mut().tree.remove(w.id);
        }
        if !removed.is_empty() {
            let ws = self.workspaces.active_mut();
            if ws
                .focused
                .is_some_and(|f| removed.iter().any(|w| w.id == f))
            {
                ws.pop_focus();
            }
            self.apply_layout();
            tracing::info!(pid, removed = removed.len(), "app terminated → retiled");
        }
    }

    /// Check if a window is on an inactive workspace (intentionally hidden).
    pub fn is_window_hidden(&self, wid: u32) -> bool {
        let id = wid as WindowId;
        if !self.registry.contains(id) {
            return false;
        }
        let ws = self.workspaces.active();
        // Window is hidden if it's not in the active workspace's tree OR floating list
        !ws.tree.contains(id) && !ws.is_floating(id)
    }

    // --- Helpers ---

    #[allow(clippy::too_many_arguments)]
    fn add_window_to_active(
        &mut self,
        id: &WindowId,
        app_pid: i32,
        app_name: &str,
        app_bundle_id: &str,
        title: &str,
        role: &str,
        subrole: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        ax_ref: CFRetained<AXUIElement>,
    ) {
        let should_float = should_auto_float(subrole, width, height);
        self.registry.add(WindowState {
            id: *id,
            app_pid,
            app_name: app_name.to_string(),
            app_bundle_id: app_bundle_id.to_string(),
            title: title.to_string(),
            role: role.to_string(),
            subrole: subrole.to_string(),
            x,
            y,
            width,
            height,
            floating: should_float,
            minimized: false,
        });
        self.ax_refs.insert(*id, ax_ref);
        let ws = self.workspaces.active_mut();
        if should_float {
            ws.floating.push(super::workspace::FloatingWindow {
                id: *id,
                geometry: Rect::new(x, y, width, height),
            });
            tracing::info!(id, subrole, "auto-floated window");
        } else {
            ws.tree.insert_with_rect(*id, ws.focused, self.screen_rect);
        }
        ws.record_focus(*id);
    }

    fn handle_event(&mut self, event: &WindowEvent, app_name: &str, app_bundle: &str) {
        match event {
            WindowEvent::Created { pid, element } => {
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
                self.add_window_to_active(
                    &id,
                    *pid,
                    app_name,
                    app_bundle,
                    &title,
                    "AXWindow",
                    "AXStandardWindow",
                    x,
                    y,
                    w,
                    h,
                    element.clone(),
                );
                self.apply_layout();
            }
            WindowEvent::Destroyed { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && self.registry.contains(id)
                {
                    tracing::info!(id, app = app_name, "window destroyed → retiling");
                    self.registry.remove(id);
                    self.ax_refs.remove(&id);
                    self.workspaces.active_mut().tree.remove(id);
                    let ws = self.workspaces.active_mut();
                    if ws.focused == Some(id) {
                        ws.pop_focus();
                    }
                    self.apply_layout();
                }
            }
            WindowEvent::FocusChanged { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && self.registry.contains(id)
                {
                    self.workspaces.active_mut().record_focus(id);
                    // Re-raise floating windows when focus changes to a tiled window
                    // This handles external focus changes (user clicking a tiled window)
                    if !self.workspaces.active().is_floating(id)
                        && !self.workspaces.active().floating.is_empty()
                    {
                        self.raise_floating_windows();
                    }
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

    fn install_observers_for_all(&mut self) {
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

        let queue = Rc::clone(&self.event_queue);
        let cb_name = name.to_string();
        let cb_bundle = bundle_id.to_string();
        let callback = Box::new(move |event: WindowEvent| {
            queue.borrow_mut().push(QueuedEvent {
                event,
                app_name: cb_name.clone(),
                app_bundle: cb_bundle.clone(),
            });
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

/// Check if a window should automatically float based on its subrole and size.
fn should_auto_float(subrole: &str, width: f64, height: f64) -> bool {
    matches!(
        subrole,
        "AXDialog" | "AXSheet" | "AXFloatingWindow" | "AXSystemFloatingWindow"
    ) || (width > 0.0 && height > 0.0 && width < 400.0 && height < 300.0)
}

/// Warp the mouse cursor to the center of a rect.
fn warp_mouse_to_center(rect: &Rect) {
    let (cx, cy) = rect.center();
    warp_mouse(cx, cy);
}
