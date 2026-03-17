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

/// Active drag operation on a floating window.
#[derive(Debug, Clone, Copy)]
enum DragMode {
    Move,
    Resize,
}

#[derive(Debug, Clone, Copy)]
struct DragState {
    window_id: WindowId,
    mode: DragMode,
    start_mouse: (f64, f64),
    start_geometry: Rect,
}

/// Central state for the window manager.
pub struct WmState {
    pub registry: WindowRegistry,
    pub workspaces: WorkspaceManager,
    pub monitors: super::monitor::MonitorManager,
    ax_refs: HashMap<WindowId, CFRetained<AXUIElement>>,
    observers: HashMap<i32, AppObserver>,
    screen_rect: Rect,
    event_queue: Rc<RefCell<Vec<QueuedEvent>>>,
    ffm_cooldown_until: Option<std::time::Instant>,
    ffm_last_window: Option<WindowId>,
    drag: Option<DragState>,
    pub focus_follows_mouse: bool,
    pub mouse_follows_focus: bool,
    pub gap_inner: f64,
    pub gap_outer: f64,
    pub rules: Vec<crate::config::lua::WindowRule>,
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
            monitors: super::monitor::MonitorManager::new(),
            ax_refs: HashMap::new(),
            observers: HashMap::new(),
            screen_rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            event_queue: Rc::new(RefCell::new(Vec::new())),
            ffm_cooldown_until: None,
            ffm_last_window: None,
            drag: None,
            focus_follows_mouse: true,
            mouse_follows_focus: true,
            gap_inner: 0.0,
            gap_outer: 0.0,
            rules: Vec::new(),
        }
    }

    pub fn discover_and_observe(&mut self) {
        // Discover all displays
        let displays = crate::platform::display::discover_displays();
        self.monitors.set_monitors(displays);

        // Detect which monitor the cursor is on — that becomes the focused monitor
        let (cx, cy) = crate::platform::display::get_cursor_position();
        tracing::info!(cursor_x = cx, cursor_y = cy, "cursor position at startup");
        if let Some(cursor_monitor) = self.monitors.monitor_at_point(cx, cy) {
            self.monitors.focused = cursor_monitor;
            tracing::info!(monitor = cursor_monitor, "focused monitor set to cursor location");
        }

        // Use focused monitor's usable frame for layout
        self.screen_rect = self
            .monitors
            .focused_monitor()
            .map(|m| m.usable_frame)
            .unwrap_or_else(get_usable_frame);
        tracing::info!(
            x = self.screen_rect.x,
            y = self.screen_rect.y,
            w = self.screen_rect.width,
            h = self.screen_rect.height,
            monitors = self.monitors.count(),
            "usable screen frame"
        );

        // Assign workspace 1 to the focused (cursor) monitor, then remaining
        // monitors get workspace 2, 3, etc. in left-to-right order.
        let focused_id = self.monitors.focused;
        let mut ordered_ids: Vec<u32> = Vec::new();
        ordered_ids.push(focused_id);
        for m in self.monitors.sorted_by_position() {
            if m.id != focused_id {
                ordered_ids.push(m.id);
            }
        }
        self.workspaces.assign_monitors(&ordered_ids);

        let windows = discover_all_windows();
        for w in &windows {
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
        }

        tracing::info!(
            windows = self.registry.count(),
            "initial window registry populated"
        );

        self.apply_layout();
        self.install_observers_for_all();

        tracing::info!(observers = self.observers.len(), "observers installed");
    }

    /// Get the AX element reference for a window (for IPC queries).
    pub fn get_ax_ref(&self, id: WindowId) -> Option<&CFRetained<AXUIElement>> {
        self.ax_refs.get(&id)
    }

    pub fn process_events(&mut self) {
        let events: Vec<QueuedEvent> = self.event_queue.borrow_mut().drain(..).collect();
        for queued in events {
            self.handle_event(&queued.event, &queued.app_name, &queued.app_bundle);
        }
    }

    // --- Layout ---

    /// Apply layout for ALL visible workspaces on their respective monitors.
    pub fn apply_layout(&self) {
        for (monitor_id, ws_id) in self.workspaces.monitor_assignments() {
            let screen_rect = self
                .monitors
                .get(*monitor_id)
                .map(|m| m.usable_frame)
                .unwrap_or(self.screen_rect);

            if let Some(ws) = self.workspaces.get_workspace(ws_id) {
                let geometries = ws.tree.calculate_geometries_with_gaps(
                    screen_rect,
                    self.gap_inner,
                    self.gap_outer,
                    true,
                );
                tracing::debug!(
                    monitor = monitor_id,
                    workspace = %ws_id,
                    windows = geometries.len(),
                    sr_x = screen_rect.x,
                    sr_y = screen_rect.y,
                    sr_w = screen_rect.width,
                    sr_h = screen_rect.height,
                    "apply_layout"
                );
                for (wid, rect) in &geometries {
                    tracing::debug!(
                        wid,
                        x = rect.x,
                        y = rect.y,
                        w = rect.width,
                        h = rect.height,
                        "layout position"
                    );
                    if let Some(ax_ref) = self.ax_refs.get(wid) {
                        if let Err(e) = ax_set_position(ax_ref, rect.x, rect.y) {
                            tracing::warn!(wid, ?e, "ax_set_position failed");
                        }
                        if let Err(e) = ax_set_size(ax_ref, rect.width, rect.height) {
                            tracing::warn!(wid, ?e, "ax_set_size failed");
                        }
                    }
                }
            }
        }
    }

    /// Get the screen rect for the focused monitor.
    pub fn focused_screen_rect(&self) -> Rect {
        self.monitors
            .focused_monitor()
            .map(|m| m.usable_frame)
            .unwrap_or(self.screen_rect)
    }

    /// Get the screen rect for a specific workspace based on its monitor assignment.
    fn screen_rect_for_workspace(&self, ws_id: &super::workspace::WorkspaceId) -> Rect {
        for (mid, wid) in self.workspaces.monitor_assignments() {
            if wid == ws_id
                && let Some(m) = self.monitors.get(*mid)
            {
                return m.usable_frame;
            }
        }
        self.focused_screen_rect()
    }

    // --- Window operations ---

    pub fn focus_direction(&mut self, direction: super::tree::Direction) {
        let ws = self.workspaces.active();
        let focused = match ws.focused {
            Some(f) => f,
            None => return,
        };
        let sr = self.focused_screen_rect();
        let geoms = ws.tree.calculate_geometries(sr);
        if let Some(target) = Node::find_adjacent(&geoms, focused, direction) {
            self.focus_window(target);
            if self.mouse_follows_focus
                && let Some((_, rect)) = geoms.iter().find(|(id, _)| *id == target)
            {
                warp_mouse_to_center(rect);
                self.ffm_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
                self.ffm_last_window = Some(target);
            }
        } else if self.monitors.count() > 1 {
            // No window found in direction on current monitor —
            // try crossing to adjacent monitor
            use super::tree::Direction;
            let next_monitor = match direction {
                Direction::Right => self.monitors.next_monitor(self.monitors.focused),
                Direction::Left => self.monitors.prev_monitor(self.monitors.focused),
                _ => None, // Up/Down stays on current monitor
            };
            if let Some(mid) = next_monitor
                && mid != self.monitors.focused
            {
                self.monitors.focused = mid;
                self.workspaces.set_focused_monitor(mid);
                if let Some(m) = self.monitors.get(mid) {
                    self.screen_rect = m.usable_frame;
                }
                if let Some(wid) = self.workspaces.active().focused {
                    self.focus_window(wid);
                    if self.mouse_follows_focus {
                        let geoms = self
                            .workspaces
                            .active()
                            .tree
                            .calculate_geometries(self.focused_screen_rect());
                        if let Some((_, rect)) = geoms.iter().find(|(id, _)| *id == wid) {
                            warp_mouse_to_center(rect);
                            self.ffm_cooldown_until = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(200),
                            );
                            self.ffm_last_window = Some(wid);
                        }
                    }
                }
                tracing::debug!(monitor = mid, "crossed to adjacent monitor");
            }
        }
    }

    pub fn swap_direction(&mut self, direction: super::tree::Direction) {
        let ws = self.workspaces.active();
        let focused = match ws.focused {
            Some(f) => f,
            None => return,
        };
        let geoms = ws.tree.calculate_geometries(self.focused_screen_rect());
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

    /// Focus a window with full app activation. Used for keybinds, clicks, workspace switch.
    pub fn focus_window(&mut self, id: WindowId) {
        self.focus_window_impl(id, true);
    }

    /// Focus a window without app activation. Used for FFM to avoid z-order disruption.
    pub fn focus_window_soft(&mut self, id: WindowId) {
        self.focus_window_impl(id, false);
    }

    fn focus_window_impl(&mut self, id: WindowId, activate_app: bool) {
        if let Some(ax_ref) = self.ax_refs.get(&id) {
            if activate_app {
                // Full activation — raise window and bring app forward
                let _ = ax_perform_action(ax_ref, "AXRaise");
                if let Some(w) = self.registry.get(id) {
                    let ax_app = unsafe { AXUIElement::new_application(w.app_pid) };
                    let key = objc2_core_foundation::CFString::from_static_str("AXFrontmost");
                    let _ = crate::platform::accessibility::ax_set_bool(&ax_app, &key, true);
                }
            } else {
                // Soft focus — update internal tracking only, don't touch macOS.
                // Any AX attribute change (AXRaise, AXMain, AXFocused) on a same-app
                // window reorders it above the floating window. So we do nothing
                // to macOS and rely on our internal focus tracking for keybind dispatch.
            }
        }
        self.workspaces.active_mut().record_focus(id);
        self.enforce_floating_levels();
        tracing::debug!(id, activate_app, "focused window");
    }

    // --- Drag operations for floating windows ---

    /// Start a move drag on a floating window (Cmd+LeftClick).
    pub fn begin_move_drag(&mut self, x: f64, y: f64) {
        let ws = self.workspaces.active();
        // Find which floating window is under the cursor
        if let Some(fw) = ws
            .floating
            .iter()
            .rev()
            .find(|fw| fw.geometry.contains_point(x, y))
        {
            self.drag = Some(DragState {
                window_id: fw.id,
                mode: DragMode::Move,
                start_mouse: (x, y),
                start_geometry: fw.geometry,
            });
            tracing::debug!(id = fw.id, "move drag started");
        }
    }

    /// Start a resize drag on a floating window (Cmd+RightClick).
    pub fn begin_resize_drag(&mut self, x: f64, y: f64) {
        let ws = self.workspaces.active();
        if let Some(fw) = ws
            .floating
            .iter()
            .rev()
            .find(|fw| fw.geometry.contains_point(x, y))
        {
            self.drag = Some(DragState {
                window_id: fw.id,
                mode: DragMode::Resize,
                start_mouse: (x, y),
                start_geometry: fw.geometry,
            });
            tracing::debug!(id = fw.id, "resize drag started");
        }
    }

    /// Update an active drag operation.
    pub fn update_drag(&mut self, x: f64, y: f64) {
        let Some(drag) = self.drag else { return };

        let dx = x - drag.start_mouse.0;
        let dy = y - drag.start_mouse.1;

        match drag.mode {
            DragMode::Move => {
                let new_x = drag.start_geometry.x + dx;
                let new_y = drag.start_geometry.y + dy;

                // Update floating geometry
                if let Some(fw) = self
                    .workspaces
                    .active_mut()
                    .floating
                    .iter_mut()
                    .find(|f| f.id == drag.window_id)
                {
                    fw.geometry.x = new_x;
                    fw.geometry.y = new_y;
                }

                // Apply via AX
                if let Some(ax_ref) = self.ax_refs.get(&drag.window_id) {
                    let _ = ax_set_position(ax_ref, new_x, new_y);
                }
            }
            DragMode::Resize => {
                let new_w = (drag.start_geometry.width + dx).max(200.0);
                let new_h = (drag.start_geometry.height + dy).max(100.0);

                if let Some(fw) = self
                    .workspaces
                    .active_mut()
                    .floating
                    .iter_mut()
                    .find(|f| f.id == drag.window_id)
                {
                    fw.geometry.width = new_w;
                    fw.geometry.height = new_h;
                }

                if let Some(ax_ref) = self.ax_refs.get(&drag.window_id) {
                    let _ = ax_set_size(ax_ref, new_w, new_h);
                }
            }
        }
    }

    /// End an active drag operation.
    pub fn end_drag(&mut self) {
        if let Some(drag) = self.drag.take() {
            tracing::debug!(id = drag.window_id, ?drag.mode, "drag ended");
        }
    }

    /// Check if a drag is currently active.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Re-apply SkyLight window levels for all floating windows.
    /// Called after every focus change since app activation can reset ordering.
    fn enforce_floating_levels(&self) {
        use crate::platform::skylight::{K_CG_FLOATING_WINDOW_LEVEL, set_window_level};
        for fw in &self.workspaces.active().floating {
            set_window_level(fw.id, K_CG_FLOATING_WINDOW_LEVEL);
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
        if !self.focus_follows_mouse {
            return;
        }
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
            // Check tiled windows using gap-aware geometry matching actual layout
            let geoms = ws.tree.calculate_geometries_with_gaps(
                self.focused_screen_rect(),
                self.gap_inner,
                self.gap_outer,
                true,
            );
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
                // Use soft focus only when floating windows exist to preserve z-order.
                // Otherwise use full activation for proper title bar highlighting.
                if self.workspaces.active().floating.is_empty() {
                    self.focus_window(id);
                } else {
                    self.focus_window_soft(id);
                }
            }
        }
    }

    pub fn toggle_float(&mut self) {
        let focused = match self.workspaces.active().focused {
            Some(f) => f,
            None => return,
        };
        let sr = self.focused_screen_rect();
        if self.workspaces.active_mut().toggle_float(focused, sr) {
            self.apply_layout();
            if self.workspaces.active().is_floating(focused) {
                // Position at stored geometry
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
                // Set window level to floating so it stays above all normal windows
                use crate::platform::skylight::{K_CG_FLOATING_WINDOW_LEVEL, set_window_level};
                set_window_level(focused, K_CG_FLOATING_WINDOW_LEVEL);
                self.enforce_floating_levels();
                tracing::info!(id = focused, "window floated (level=floating)");
            } else {
                // Restore to normal window level
                use crate::platform::skylight::{K_CG_NORMAL_WINDOW_LEVEL, set_window_level};
                set_window_level(focused, K_CG_NORMAL_WINDOW_LEVEL);
                tracing::info!(id = focused, "window tiled (level=normal)");
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
            let geoms = ws.tree.calculate_geometries_with_gaps(
                self.focused_screen_rect(),
                self.gap_inner,
                self.gap_outer,
                true,
            );
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
        let transition = self
            .workspaces
            .switch_to(target, self.focused_screen_rect());

        tracing::debug!(
            hide = transition.hide.len(),
            show = transition.show.len(),
            "workspace transition"
        );

        // Hide windows off ALL monitors
        for wid in &transition.hide {
            if let Some(ax_ref) = self.ax_refs.get(wid) {
                let (w, _h) = ax_get_size(ax_ref).unwrap_or((2048.0, 1400.0));
                let (hide_x, hide_y) = compute_hide_position(&self.monitors, w);
                let _ = ax_set_position(ax_ref, hide_x, hide_y);
                tracing::debug!(wid, hide_x, hide_y, "hidden");
            }
        }

        // Apply layout on all visible workspaces (uses per-monitor rects)
        self.apply_layout();

        // Focus
        if let Some(focus_id) = transition.focus {
            self.focus_window(focus_id);
        }

        tracing::info!(ws = %self.workspaces.active_id(), "switched workspace");
    }

    pub fn workspace_next(&mut self) {
        let current = match self.workspaces.active_id() {
            super::workspace::WorkspaceId::Numbered(n) => *n,
            _ => return,
        };
        let next = if current >= 10 { 1 } else { current + 1 };
        self.switch_workspace(next);
    }

    pub fn workspace_prev(&mut self) {
        let current = match self.workspaces.active_id() {
            super::workspace::WorkspaceId::Numbered(n) => *n,
            _ => return,
        };
        let prev = if current <= 1 { 10 } else { current - 1 };
        self.switch_workspace(prev);
    }

    pub fn focus_monitor_next(&mut self) {
        let current = self.monitors.focused;
        if let Some(next) = self.monitors.next_monitor(current) {
            self.monitors.focused = next;
            self.workspaces.set_focused_monitor(next);
            if let Some(m) = self.monitors.get(next) {
                self.screen_rect = m.usable_frame;
            }
            // Reapply layout for the new monitor's workspace
            self.apply_layout();
            if let Some(wid) = self.workspaces.active().focused {
                self.focus_window(wid);
            }
            tracing::info!(monitor = next, ws = %self.workspaces.active_id(), "focused monitor");
        }
    }

    pub fn focus_monitor_prev(&mut self) {
        let current = self.monitors.focused;
        if let Some(prev) = self.monitors.prev_monitor(current) {
            self.monitors.focused = prev;
            self.workspaces.set_focused_monitor(prev);
            if let Some(m) = self.monitors.get(prev) {
                self.screen_rect = m.usable_frame;
            }
            self.apply_layout();
            if let Some(wid) = self.workspaces.active().focused {
                self.focus_window(wid);
            }
            tracing::info!(monitor = prev, ws = %self.workspaces.active_id(), "focused monitor");
        }
    }

    pub fn move_to_monitor_next(&mut self) {
        let current = self.monitors.focused;
        if let Some(next) = self.monitors.next_monitor(current)
            && next != current
        {
            self.move_window_to_monitor(next);
        }
    }

    pub fn move_to_monitor_prev(&mut self) {
        let current = self.monitors.focused;
        if let Some(prev) = self.monitors.prev_monitor(current)
            && prev != current
        {
            self.move_window_to_monitor(prev);
        }
    }

    /// Move the focused window from the current monitor's workspace to the target monitor's workspace.
    fn move_window_to_monitor(&mut self, target_monitor: super::monitor::MonitorId) {
        let focused = match self.workspaces.active().focused {
            Some(f) => f,
            None => return,
        };

        // Get the target monitor's active workspace
        let target_ws = match self.workspaces.active_id_for_monitor(target_monitor) {
            Some(ws) => ws.clone(),
            None => return,
        };

        // Get target monitor's screen rect for layout
        let target_rect = match self.monitors.get(target_monitor) {
            Some(m) => m.usable_frame,
            None => return,
        };

        // Remove from current workspace
        let current_ws = self.workspaces.active_mut();
        let was_floating = current_ws.is_floating(focused);
        if was_floating {
            current_ws.floating.retain(|f| f.id != focused);
        } else {
            current_ws.tree.remove(focused);
        }
        current_ws.focus_history.retain(|id| *id != focused);
        if current_ws.focused == Some(focused) {
            current_ws.focused = current_ws
                .focus_history
                .last()
                .copied()
                .or(current_ws.tree.first_window());
        }

        // Relayout current monitor
        self.apply_layout();

        // Insert into target workspace
        let target = self.workspaces.get_or_create(target_ws);
        if was_floating {
            target.floating.push(super::workspace::FloatingWindow {
                id: focused,
                geometry: super::tree::Rect::new(
                    target_rect.x + 50.0,
                    target_rect.y + 50.0,
                    800.0,
                    600.0,
                ),
            });
        } else {
            target
                .tree
                .insert_with_rect(focused, target.focused, target_rect);
        }
        target.record_focus(focused);

        // Apply layout on target monitor using its screen rect
        let geoms = target.tree.calculate_geometries_with_gaps(
            target_rect,
            self.gap_inner,
            self.gap_outer,
            true,
        );
        for (wid, rect) in &geoms {
            if let Some(ax_ref) = self.ax_refs.get(wid) {
                // Multi-display AX move: size first, then position, then size again
                let _ = ax_set_size(ax_ref, rect.width, rect.height);
                let _ = ax_set_position(ax_ref, rect.x, rect.y);
                let _ = ax_set_size(ax_ref, rect.width, rect.height);
            }
        }

        // Focus the target monitor
        self.monitors.focused = target_monitor;
        self.workspaces.set_focused_monitor(target_monitor);
        self.screen_rect = target_rect;
        self.focus_window(focused);

        tracing::info!(
            id = focused,
            monitor = target_monitor,
            "moved window to monitor"
        );
    }

    /// Re-discover displays and reassign workspaces. Called on hotplug.
    pub fn refresh_monitors(&mut self) {
        let displays = crate::platform::display::discover_displays();
        let old_count = self.monitors.count();
        self.monitors.set_monitors(displays);
        let new_count = self.monitors.count();

        let sorted_ids: Vec<u32> = self
            .monitors
            .sorted_by_position()
            .iter()
            .map(|m| m.id)
            .collect();
        self.workspaces.assign_monitors(&sorted_ids);

        // Update screen rect for focused monitor
        if let Some(m) = self.monitors.focused_monitor() {
            self.screen_rect = m.usable_frame;
        }

        // Reapply layout on all visible workspaces
        self.apply_layout();

        tracing::info!(old_count, new_count, "monitors refreshed");
    }

    pub fn move_to_workspace(&mut self, num: u8) {
        let focused = match self.workspaces.active().focused {
            Some(f) => f,
            None => return,
        };

        let target = WorkspaceId::Numbered(num);
        if self
            .workspaces
            .move_window_to(focused, target.clone(), self.focused_screen_rect())
        {
            // Hide the moved window off all monitors
            if let Some(ax_ref) = self.ax_refs.get(&focused) {
                let (w, _h) = ax_get_size(ax_ref).unwrap_or((2048.0, 1400.0));
                let (hx, hy) = compute_hide_position(&self.monitors, w);
                let _ = ax_set_position(ax_ref, hx, hy);
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

        // Resolve actual app name from NSRunningApplication (CGWindowList owner is often empty)
        let resolved_name = resolve_app_name(pid).unwrap_or_else(|| owner.to_string());
        tracing::info!(pid, app = %resolved_name, "new window detected, enumerating");
        let ax_app = unsafe { AXUIElement::new_application(pid) };
        let app_info = crate::platform::application::AppInfo {
            pid,
            bundle_id: String::new(),
            name: resolved_name,
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
        // Phantom window guard: skip windows with zero size
        if width <= 0.0 || height <= 0.0 {
            tracing::trace!(id, app_name, "skipping zero-size phantom window");
            return;
        }

        tracing::debug!(
            id,
            app_name,
            title,
            subrole,
            rules_count = self.rules.len(),
            "add_window_to_active"
        );

        // Check window rules for matching (case-insensitive, supports regex via /pattern/)
        let mut rule_float: Option<bool> = None;
        let mut rule_workspace: Option<u8> = None;
        let mut rule_geometry: Option<(f64, f64, f64, f64)> = None;
        let app_lower = app_name.to_lowercase();
        let title_lower = title.to_lowercase();
        for rule in &self.rules {
            let name_matches = rule
                .app_name
                .as_ref()
                .is_none_or(|n| match_string_or_regex(n, &app_lower));
            let bundle_matches = rule
                .app_bundle
                .as_ref()
                .is_none_or(|b| app_bundle_id.to_lowercase().contains(&b.to_lowercase()));
            let title_matches = rule
                .title
                .as_ref()
                .is_none_or(|t| match_string_or_regex(t, &title_lower));
            if name_matches && bundle_matches && title_matches {
                tracing::debug!(app_name, title, ?rule, "window rule matched");
                if let Some(f) = rule.floating {
                    rule_float = Some(f);
                }
                if let Some(w) = rule.workspace {
                    rule_workspace = Some(w);
                }
                if let Some(g) = rule.geometry {
                    rule_geometry = Some(g);
                }
            }
        }

        let should_float = rule_float.unwrap_or_else(|| should_auto_float(subrole, width, height));

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

        // Determine target workspace (rule override or active)
        if let Some(ws_num) = rule_workspace {
            let target = super::workspace::WorkspaceId::Numbered(ws_num);
            let is_active = *self.workspaces.active_id() == target;
            let target_rect = self.screen_rect_for_workspace(&target);

            let ws = self.workspaces.get_or_create(target);
            if should_float {
                let geom = rule_geometry
                    .map(|(gx, gy, gw, gh)| Rect::new(gx, gy, gw, gh))
                    .unwrap_or_else(|| Rect::new(x, y, width, height));
                ws.floating.push(super::workspace::FloatingWindow {
                    id: *id,
                    geometry: geom,
                });
            } else {
                ws.tree.insert_with_rect(*id, ws.focused, target_rect);
            }
            ws.record_focus(*id);
            tracing::info!(id, app_name, ws_num, "window assigned to workspace by rule");

            if !is_active {
                // Hide the window off all monitors
                if let Some(ax_ref) = self.ax_refs.get(id) {
                    let (w, _h) = ax_get_size(ax_ref).unwrap_or((2048.0, 1400.0));
                    let (hx, hy) = compute_hide_position(&self.monitors, w);
                    let _ = ax_set_position(ax_ref, hx, hy);
                }
                // Switch to the target workspace to follow the window
                self.switch_workspace(ws_num);
            }
        } else {
            let sr = self.focused_screen_rect();
            let ws = self.workspaces.active_mut();
            if should_float {
                let geom = rule_geometry
                    .map(|(gx, gy, gw, gh)| Rect::new(gx, gy, gw, gh))
                    .unwrap_or_else(|| Rect::new(x, y, width, height));
                ws.floating.push(super::workspace::FloatingWindow {
                    id: *id,
                    geometry: geom,
                });
                tracing::info!(id, subrole, "auto-floated window");
            } else {
                ws.tree.insert_with_rect(*id, ws.focused, sr);
            }
            ws.record_focus(*id);
        }
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
                }
            }
            WindowEvent::Moved { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && let Ok((x, y)) = ax_get_position(element)
                    && let Some(w) = self.registry.get(id)
                {
                    let (width, height) = (w.width, w.height);
                    self.registry.update_geometry(id, x, y, width, height);
                    // Update floating window geometry so FFM uses correct bounds
                    if let Some(fw) = self
                        .workspaces
                        .active_mut()
                        .floating
                        .iter_mut()
                        .find(|f| f.id == id)
                    {
                        fw.geometry.x = x;
                        fw.geometry.y = y;
                    }
                }
            }
            WindowEvent::Resized { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && let (Ok((x, y)), Ok((w, h))) =
                        (ax_get_position(element), ax_get_size(element))
                {
                    self.registry.update_geometry(id, x, y, w, h);
                    if let Some(fw) = self
                        .workspaces
                        .active_mut()
                        .floating
                        .iter_mut()
                        .find(|f| f.id == id)
                    {
                        fw.geometry = Rect::new(x, y, w, h);
                    }
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
fn should_auto_float(subrole: &str, _width: f64, _height: f64) -> bool {
    matches!(
        subrole,
        "AXDialog" | "AXSheet" | "AXFloatingWindow" | "AXSystemFloatingWindow"
    )
}

/// Compute a hide position for a window that's off-screen on ALL monitors.
/// Uses the bottom-left corner of the leftmost monitor minus the window width,
/// plus the full height of all monitors below the lowest monitor.
fn compute_hide_position(
    monitors: &super::monitor::MonitorManager,
    window_width: f64,
) -> (f64, f64) {
    if monitors.count() == 0 {
        return (1.0 - window_width, 10000.0);
    }
    // Find the leftmost x and the bottommost y across all monitors
    let min_x = monitors
        .all()
        .iter()
        .map(|m| m.frame.x)
        .fold(f64::INFINITY, f64::min);
    let max_y = monitors
        .all()
        .iter()
        .map(|m| m.frame.y + m.frame.height)
        .fold(f64::NEG_INFINITY, f64::max);
    // Position: far left of leftmost monitor and below all monitors
    (min_x - window_width - 100.0, max_y + 100.0)
}

/// Warp the mouse cursor to the center of a rect.
fn warp_mouse_to_center(rect: &Rect) {
    let (cx, cy) = rect.center();
    warp_mouse(cx, cy);
}

/// Resolve app name from PID using NSRunningApplication.
fn resolve_app_name(pid: i32) -> Option<String> {
    use objc2_app_kit::NSRunningApplication;
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    app.localizedName().map(|n| n.to_string())
}

/// Match a string against a pattern. If pattern starts and ends with /,
/// treat it as a regex. Otherwise, case-insensitive substring match.
fn match_string_or_regex(pattern: &str, haystack: &str) -> bool {
    if pattern.starts_with('/') && pattern.ends_with('/') && pattern.len() > 2 {
        // Regex pattern: /pattern/
        let regex_str = &pattern[1..pattern.len() - 1];
        match regex::Regex::new(regex_str) {
            Ok(re) => re.is_match(haystack),
            Err(e) => {
                tracing::warn!(pattern, err = %e, "invalid regex in window rule");
                false
            }
        }
    } else {
        // Simple case-insensitive substring
        haystack.contains(&pattern.to_lowercase())
    }
}
