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
use crate::platform::display::warp_mouse;
use crate::platform::observer::{AppObserver, WindowEvent};

use super::tree::{Node, Rect};
use super::window::{WindowId, WindowRegistry, WindowState};
use super::workspace::WorkspaceManager;

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
    pub monitors: Vec<super::monitor::Monitor>,
    ax_refs: HashMap<WindowId, CFRetained<AXUIElement>>,
    observers: HashMap<i32, AppObserver>,
    pub focused_monitor: usize,
    pub bar_height: f64,
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
            monitors: Vec::new(),
            ax_refs: HashMap::new(),
            observers: HashMap::new(),
            focused_monitor: 0,
            bar_height: 0.0,
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

    /// Active workspace index for focused monitor.
    pub fn active_ws_idx(&self) -> usize {
        self.monitors[self.focused_monitor].active_workspace
    }

    /// Active workspace (read-only).
    pub fn active_workspace(&self) -> &super::workspace::Workspace {
        self.workspaces.get(self.active_ws_idx())
    }

    /// Active workspace (mutable).
    pub fn active_workspace_mut(&mut self) -> &mut super::workspace::Workspace {
        let idx = self.active_ws_idx();
        self.workspaces.get_mut(idx)
    }

    /// Usable frame for a monitor, adjusted for bar_height.
    fn monitor_rect(&self, mi: usize) -> Rect {
        let r = self.monitors[mi].usable_frame;
        if self.bar_height > 0.0 {
            Rect::new(r.x, r.y + self.bar_height, r.width, r.height - self.bar_height)
        } else {
            r
        }
    }

    /// Usable frame for focused monitor.
    fn focused_rect(&self) -> Rect {
        self.monitor_rect(self.focused_monitor)
    }

    /// Find which monitor index has a workspace visible, if any.
    fn monitor_showing_workspace(&self, ws_idx: usize) -> Option<usize> {
        self.monitors.iter().position(|m| m.active_workspace == ws_idx)
    }

    pub fn discover_and_observe(&mut self) {
        // Discover all displays
        let mut displays = crate::platform::display::discover_displays();

        // Sort monitors by x position (left-to-right)
        displays.sort_by(|a, b| {
            a.usable_frame
                .x
                .partial_cmp(&b.usable_frame.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.monitors = displays;

        // Detect which monitor the cursor is on
        let (cx, cy) = crate::platform::display::get_cursor_position();
        tracing::info!(cursor_x = cx, cursor_y = cy, "cursor position at startup");

        let cursor_mi = super::monitor::index_at_point(&self.monitors, cx, cy)
            .unwrap_or(0);
        self.focused_monitor = cursor_mi;
        tracing::info!(monitor = cursor_mi, "focused monitor set to cursor location");

        // Assign workspaces: cursor's monitor gets workspace 0 (ws1),
        // then remaining monitors get 1, 2, etc. in left-to-right order.
        let mut ws_idx = 0usize;
        // Cursor monitor gets workspace 0
        self.monitors[cursor_mi].active_workspace = ws_idx;
        self.workspaces.get_mut(ws_idx).visible = true;
        self.workspaces.get_mut(ws_idx).last_monitor = Some(cursor_mi);
        ws_idx += 1;

        // Remaining monitors in left-to-right order
        for mi in 0..self.monitors.len() {
            if mi == cursor_mi {
                continue;
            }
            self.monitors[mi].active_workspace = ws_idx;
            self.workspaces.get_mut(ws_idx).visible = true;
            self.workspaces.get_mut(ws_idx).last_monitor = Some(mi);
            ws_idx += 1;
        }

        tracing::info!(
            monitors = self.monitors.len(),
            focused = self.focused_monitor,
            "displays configured"
        );

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

        // Apply layout twice: first pass moves windows to the correct
        // monitor, second pass resizes correctly since they're already there.
        // Needed because macOS AX constrains resize to the window's current
        // display, and the position change may not have taken effect yet.
        self.apply_layout();
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.apply_layout();
        self.fix_oversized_windows();
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
    /// Uses the yabai/AeroSpace pattern: disable AXEnhancedUserInterface,
    /// then size → position → size per window.
    pub fn apply_layout(&self) {
        use crate::platform::accessibility::ax_set_bool;
        let eui_key =
            objc2_core_foundation::CFString::from_static_str("AXEnhancedUserInterface");

        for (mi, monitor) in self.monitors.iter().enumerate() {
            let ws = self.workspaces.get(monitor.active_workspace);
            let screen_rect = self.monitor_rect(mi);
            let geometries = ws.tree.calculate_geometries_with_gaps(
                screen_rect, self.gap_inner, self.gap_outer, true,
            );
            tracing::debug!(monitor = mi, workspace = %ws.id, windows = geometries.len(),
                sr_x = screen_rect.x, sr_y = screen_rect.y, sr_w = screen_rect.width,
                sr_h = screen_rect.height, "apply_layout");

            for (wid, rect) in &geometries {
                tracing::debug!(wid, x = rect.x, y = rect.y, w = rect.width, h = rect.height, "tile");
                if let Some(ax_ref) = self.ax_refs.get(wid) {
                    // Get the app-level AX element to toggle AXEnhancedUserInterface
                    let app_ref = if let Some(w) = self.registry.get(*wid) {
                        Some(unsafe { AXUIElement::new_application(w.app_pid) })
                    } else {
                        None
                    };

                    // Disable AXEnhancedUserInterface (yabai/AeroSpace workaround).
                    // This makes macOS more compliant with resize requests.
                    let was_eui = app_ref.as_ref().and_then(|app| {
                        crate::platform::accessibility::ax_get_bool(app, &eui_key).ok()
                    });
                    if was_eui == Some(true) {
                        if let Some(app) = &app_ref {
                            let _ = ax_set_bool(app, &eui_key, false);
                        }
                    }

                    // Size → Position → Size (yabai/AeroSpace order)
                    let _ = ax_set_size(ax_ref, rect.width, rect.height);
                    let _ = ax_set_position(ax_ref, rect.x, rect.y);
                    let _ = ax_set_size(ax_ref, rect.width, rect.height);

                    // Restore AXEnhancedUserInterface
                    if was_eui == Some(true) {
                        if let Some(app) = &app_ref {
                            let _ = ax_set_bool(app, &eui_key, true);
                        }
                    }
                }
            }
        }
    }

    /// After layout, check for windows that overflow their tiles.
    /// Try swapping the oversized window into the largest available tile.
    /// If it still doesn't fit, auto-float it.
    pub fn fix_oversized_windows(&mut self) {
        for mi in 0..self.monitors.len() {
            let ws_idx = self.monitors[mi].active_workspace;
            let screen_rect = self.monitor_rect(mi);
            let geometries = self.workspaces.get(ws_idx).tree.calculate_geometries_with_gaps(
                screen_rect, self.gap_inner, self.gap_outer, true,
            );
            if geometries.is_empty() { continue; }

            // Find windows that overflow their tiles
            let mut oversized: Vec<(super::window::WindowId, f64, f64)> = Vec::new();
            for (wid, rect) in &geometries {
                if let Some(ax_ref) = self.ax_refs.get(wid) {
                    if let Ok((aw, ah)) = ax_get_size(ax_ref) {
                        if aw > rect.width + 1.0 || ah > rect.height + 1.0 {
                            oversized.push((*wid, aw, ah));
                        }
                    }
                }
            }

            for (oversized_wid, min_w, min_h) in oversized {
                // Find the largest tile that could fit this window
                let best_swap = geometries.iter()
                    .filter(|(wid, _)| *wid != oversized_wid)
                    .filter(|(_, rect)| rect.width >= min_w - 1.0 && rect.height >= min_h - 1.0)
                    .max_by(|(_, a), (_, b)| {
                        (a.width * a.height).partial_cmp(&(b.width * b.height))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(wid, _)| *wid);

                if let Some(swap_target) = best_swap {
                    // Swap in the BSP tree and re-apply layout
                    tracing::info!(
                        oversized = oversized_wid, target = swap_target,
                        "swapping oversized window into larger tile"
                    );
                    self.workspaces.get_mut(ws_idx).tree.swap(oversized_wid, swap_target);
                    self.apply_layout();
                } else {
                    // No tile large enough — auto-float
                    tracing::info!(
                        id = oversized_wid, min_w, min_h,
                        "auto-floating window that exceeds all tiles"
                    );
                    let sr = self.monitor_rect(mi);
                    self.workspaces.get_mut(ws_idx).toggle_float(oversized_wid, sr);
                    if let Some(ax_ref) = self.ax_refs.get(&oversized_wid) {
                        // Center the floated window
                        let fx = sr.x + (sr.width - min_w) / 2.0;
                        let fy = sr.y + (sr.height - min_h) / 2.0;
                        let _ = ax_set_position(ax_ref, fx, fy);
                        let _ = ax_set_size(ax_ref, min_w, min_h);
                    }
                    use crate::platform::skylight::{K_CG_FLOATING_WINDOW_LEVEL, set_window_level};
                    set_window_level(oversized_wid, K_CG_FLOATING_WINDOW_LEVEL);
                    self.apply_layout();
                }
                // Only fix one window per pass to avoid cascading swaps
                break;
            }
        }
    }

    // --- Window operations ---

    pub fn focus_direction(&mut self, direction: super::tree::Direction) {
        let ws = self.active_workspace();
        let focused = match ws.focused {
            Some(f) => f,
            None => return,
        };
        let sr = self.focused_rect();
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
        } else if self.monitors.len() > 1 {
            // No window found in direction on current monitor --
            // try crossing to adjacent monitor
            use super::tree::Direction;
            let new_mi = match direction {
                Direction::Right => {
                    let next = super::monitor::next_index(&self.monitors, self.focused_monitor);
                    if next != self.focused_monitor { Some(next) } else { None }
                }
                Direction::Left => {
                    let prev = super::monitor::prev_index(&self.monitors, self.focused_monitor);
                    if prev != self.focused_monitor { Some(prev) } else { None }
                }
                _ => None, // Up/Down stays on current monitor
            };
            if let Some(new_mi) = new_mi {
                self.focused_monitor = new_mi;
                if let Some(wid) = self.active_workspace().focused {
                    self.focus_window(wid);
                    if self.mouse_follows_focus {
                        let geoms = self
                            .active_workspace()
                            .tree
                            .calculate_geometries(self.focused_rect());
                        if let Some((_, rect)) = geoms.iter().find(|(id, _)| *id == wid) {
                            warp_mouse_to_center(rect);
                            self.ffm_cooldown_until = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(200),
                            );
                            self.ffm_last_window = Some(wid);
                        }
                    }
                }
                tracing::debug!(monitor = new_mi, "crossed to adjacent monitor");
            }
        }
    }

    pub fn swap_direction(&mut self, direction: super::tree::Direction) {
        let ws = self.active_workspace();
        let focused = match ws.focused {
            Some(f) => f,
            None => return,
        };
        let sr = self.focused_rect();
        let geoms = ws.tree.calculate_geometries(sr);
        if let Some(target) = Node::find_adjacent(&geoms, focused, direction) {
            tracing::debug!(
                focused,
                target,
                ?direction,
                "swap_direction"
            );
            if self.active_workspace_mut().tree.swap(focused, target) {
                self.apply_layout();
            }
        } else {
            tracing::debug!(focused, ?direction, "swap: no adjacent window");
        }
    }

    pub fn resize_direction(&mut self, direction: super::tree::Direction) {
        let focused = match self.active_workspace().focused {
            Some(f) => f,
            None => return,
        };
        if self
            .active_workspace_mut()
            .tree
            .resize(focused, direction, 0.05)
        {
            self.apply_layout();
        }
    }

    pub fn equalize(&mut self) {
        self.active_workspace_mut().tree.equalize();
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
                // Full activation -- raise window and bring app forward
                let _ = ax_perform_action(ax_ref, "AXRaise");
                if let Some(w) = self.registry.get(id) {
                    let ax_app = unsafe { AXUIElement::new_application(w.app_pid) };
                    let key = objc2_core_foundation::CFString::from_static_str("AXFrontmost");
                    let _ = crate::platform::accessibility::ax_set_bool(&ax_app, &key, true);
                }
            } else {
                // Soft focus -- update internal tracking only, don't touch macOS.
            }
        }
        self.active_workspace_mut().record_focus(id);
        self.enforce_floating_levels();
        tracing::debug!(id, activate_app, "focused window");
    }

    // --- Drag operations for floating windows ---

    /// Start a move drag on a floating window (Cmd+LeftClick).
    pub fn begin_move_drag(&mut self, x: f64, y: f64) {
        // Find which floating window is under the cursor
        let hit = self.active_workspace()
            .floating
            .iter()
            .rev()
            .find(|fw| fw.geometry.contains_point(x, y))
            .map(|fw| (fw.id, fw.geometry));
        if let Some((id, geometry)) = hit {
            self.drag = Some(DragState {
                window_id: id,
                mode: DragMode::Move,
                start_mouse: (x, y),
                start_geometry: geometry,
            });
            tracing::debug!(id, "move drag started");
        }
    }

    /// Start a resize drag on a floating window (Cmd+RightClick).
    pub fn begin_resize_drag(&mut self, x: f64, y: f64) {
        let hit = self.active_workspace()
            .floating
            .iter()
            .rev()
            .find(|fw| fw.geometry.contains_point(x, y))
            .map(|fw| (fw.id, fw.geometry));
        if let Some((id, geometry)) = hit {
            self.drag = Some(DragState {
                window_id: id,
                mode: DragMode::Resize,
                start_mouse: (x, y),
                start_geometry: geometry,
            });
            tracing::debug!(id, "resize drag started");
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
                    .active_workspace_mut()
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
                    .active_workspace_mut()
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
        for fw in &self.active_workspace().floating {
            set_window_level(fw.id, K_CG_FLOATING_WINDOW_LEVEL);
        }
    }

    pub fn close_focused(&mut self) {
        let focused = match self.active_workspace().focused {
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

        let ws = self.active_workspace();

        // Check floating windows first -- they're visually on top
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
                self.focused_rect(),
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
                && self.active_workspace().focused != Some(id)
            {
                // Use soft focus only when floating windows exist to preserve z-order.
                // Otherwise use full activation for proper title bar highlighting.
                if self.active_workspace().floating.is_empty() {
                    self.focus_window(id);
                } else {
                    self.focus_window_soft(id);
                }
            }
        }
    }

    pub fn toggle_float(&mut self) {
        let focused = match self.active_workspace().focused {
            Some(f) => f,
            None => return,
        };
        let sr = self.focused_rect();
        if self.active_workspace_mut().toggle_float(focused, sr) {
            self.apply_layout();
            if self.active_workspace().is_floating(focused) {
                // Position at stored geometry
                if let Some(fw) = self
                    .active_workspace()
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
        let ws = self.active_workspace();

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
                self.focused_rect(),
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
            && self.active_workspace().focused != Some(id)
        {
            self.focus_window(id);
        }
    }

    // --- Workspace operations ---

    pub fn switch_workspace(&mut self, num: u8) {
        let target_idx = (num as usize).saturating_sub(1);
        let current_idx = self.monitors[self.focused_monitor].active_workspace;

        if target_idx == current_idx { return; }

        tracing::info!(from = current_idx + 1, to = num, "switching workspace");

        // Is target workspace already visible on some monitor?
        if let Some(other_mi) = self.monitor_showing_workspace(target_idx) {
            // Just move focus to that monitor (gar-style: no swap)
            self.focused_monitor = other_mi;
            if let Some(wid) = self.workspaces.get(target_idx).focused {
                self.focus_window(wid);
            }
            if self.mouse_follows_focus {
                let rect = self.focused_rect();
                warp_mouse_to_center(&rect);
                self.ffm_cooldown_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
            }
        } else {
            // Hide current workspace windows
            let current_ws = self.workspaces.get(current_idx);
            for wid in current_ws.all_window_ids() {
                if let Some(ax_ref) = self.ax_refs.get(&wid) {
                    let (w, _) = ax_get_size(ax_ref).unwrap_or((2048.0, 1400.0));
                    let (hx, hy) = compute_hide_position(&self.monitors, w);
                    let _ = ax_set_position(ax_ref, hx, hy);
                }
            }
            self.workspaces.get_mut(current_idx).visible = false;

            // Show target workspace
            self.monitors[self.focused_monitor].active_workspace = target_idx;
            self.workspaces.get_mut(target_idx).visible = true;
            self.workspaces.get_mut(target_idx).last_monitor = Some(self.focused_monitor);

            // Double-apply: first pass positions windows on the monitor,
            // second pass resizes correctly after macOS processes the moves.
            self.apply_layout();
            std::thread::sleep(std::time::Duration::from_millis(50));
            self.apply_layout();
            self.fix_oversized_windows();
            if let Some(wid) = self.workspaces.get(target_idx).focused {
                self.focus_window(wid);
            }
        }
        tracing::info!(ws = num, "switched workspace");
    }

    pub fn workspace_next(&mut self) {
        let current_ws = self.active_workspace();
        let current = match &current_ws.id {
            super::workspace::WorkspaceId::Numbered(n) => *n,
            _ => return,
        };
        let next = if current >= 10 { 1 } else { current + 1 };
        self.switch_workspace(next);
    }

    pub fn workspace_prev(&mut self) {
        let current_ws = self.active_workspace();
        let current = match &current_ws.id {
            super::workspace::WorkspaceId::Numbered(n) => *n,
            _ => return,
        };
        let prev = if current <= 1 { 10 } else { current - 1 };
        self.switch_workspace(prev);
    }

    pub fn focus_monitor_next(&mut self) {
        if self.monitors.len() <= 1 { return; }
        let next = super::monitor::next_index(&self.monitors, self.focused_monitor);
        self.focused_monitor = next;
        self.apply_layout();
        if let Some(wid) = self.active_workspace().focused {
            self.focus_window(wid);
        }
        tracing::info!(monitor = next, ws = %self.active_workspace().id, "focused monitor");
    }

    pub fn focus_monitor_prev(&mut self) {
        if self.monitors.len() <= 1 { return; }
        let prev = super::monitor::prev_index(&self.monitors, self.focused_monitor);
        self.focused_monitor = prev;
        self.apply_layout();
        if let Some(wid) = self.active_workspace().focused {
            self.focus_window(wid);
        }
        tracing::info!(monitor = prev, ws = %self.active_workspace().id, "focused monitor");
    }

    pub fn move_to_monitor_next(&mut self) {
        if self.monitors.len() <= 1 { return; }
        let next = super::monitor::next_index(&self.monitors, self.focused_monitor);
        if next != self.focused_monitor {
            self.move_window_to_monitor(next);
        }
    }

    pub fn move_to_monitor_prev(&mut self) {
        if self.monitors.len() <= 1 { return; }
        let prev = super::monitor::prev_index(&self.monitors, self.focused_monitor);
        if prev != self.focused_monitor {
            self.move_window_to_monitor(prev);
        }
    }

    /// Move the focused window from the current monitor's workspace to the target monitor's workspace.
    fn move_window_to_monitor(&mut self, target_mi: usize) {
        let focused = match self.active_workspace().focused {
            Some(f) => f,
            None => return,
        };

        // Get the target monitor's active workspace
        let target_ws_idx = self.monitors[target_mi].active_workspace;

        // Get target monitor's screen rect for layout
        let target_rect = self.monitor_rect(target_mi);

        // Remove from current workspace
        let current_ws = self.active_workspace_mut();
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
        let target = self.workspaces.get_or_create(target_ws_idx);
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
        self.focused_monitor = target_mi;
        self.focus_window(focused);

        tracing::info!(
            id = focused,
            monitor = target_mi,
            "moved window to monitor"
        );
    }

    /// Re-discover displays and reassign workspaces. Called on hotplug.
    pub fn refresh_monitors(&mut self) {
        let mut new_displays = crate::platform::display::discover_displays();
        new_displays.sort_by(|a, b| {
            a.usable_frame
                .x
                .partial_cmp(&b.usable_frame.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let old_count = self.monitors.len();

        // Match new displays to old by CGDirectDisplayID to preserve workspace assignments
        let mut new_monitors: Vec<super::monitor::Monitor> = Vec::with_capacity(new_displays.len());
        let mut used_ws: Vec<bool> = vec![false; self.workspaces.count()];

        for nd in &new_displays {
            if let Some(old) = self.monitors.iter().find(|om| om.id == nd.id) {
                let mut m = nd.clone();
                m.active_workspace = old.active_workspace;
                used_ws.resize(used_ws.len().max(m.active_workspace + 1), false);
                used_ws[m.active_workspace] = true;
                new_monitors.push(m);
            } else {
                // New monitor -- assign first non-visible workspace
                let ws_idx = used_ws.iter().position(|&u| !u).unwrap_or(used_ws.len());
                used_ws.resize(used_ws.len().max(ws_idx + 1), false);
                used_ws[ws_idx] = true;
                let mut m = nd.clone();
                m.active_workspace = ws_idx;
                self.workspaces.get_or_create(ws_idx).visible = true;
                self.workspaces.get_or_create(ws_idx).last_monitor = Some(new_monitors.len());
                new_monitors.push(m);
            }
        }

        self.monitors = new_monitors;

        // Clamp focused_monitor
        if self.focused_monitor >= self.monitors.len() {
            self.focused_monitor = 0;
        }

        // Reapply layout on all visible workspaces
        self.apply_layout();

        tracing::info!(old_count, new_count = self.monitors.len(), "monitors refreshed");
    }

    pub fn move_to_workspace(&mut self, num: u8) {
        let target_idx = (num as usize).saturating_sub(1);
        let current_idx = self.active_ws_idx();
        if target_idx == current_idx { return; }

        let focused = match self.active_workspace().focused {
            Some(f) => f,
            None => return,
        };

        // Remove from current workspace
        let current_ws = self.workspaces.get_mut(current_idx);
        let was_floating = current_ws.is_floating(focused);
        if was_floating {
            current_ws.floating.retain(|f| f.id != focused);
        } else {
            current_ws.tree.remove(focused);
        }
        current_ws.focus_history.retain(|id| *id != focused);
        if current_ws.focused == Some(focused) {
            current_ws.focused = current_ws.focus_history.last().copied()
                .or(current_ws.tree.first_window());
        }

        // Insert into target workspace
        let target_rect = self.monitor_showing_workspace(target_idx)
            .map(|mi| self.monitor_rect(mi))
            .unwrap_or(self.focused_rect());
        let target_ws = self.workspaces.get_or_create(target_idx);
        if was_floating {
            target_ws.floating.push(super::workspace::FloatingWindow {
                id: focused,
                geometry: Rect::new(target_rect.x + 50.0, target_rect.y + 50.0, 800.0, 600.0),
            });
        } else {
            target_ws.tree.insert_with_rect(focused, target_ws.focused, target_rect);
        }
        target_ws.record_focus(focused);

        // If target is not visible, hide the window
        if !self.workspaces.get(target_idx).visible {
            if let Some(ax_ref) = self.ax_refs.get(&focused) {
                let (w, _) = ax_get_size(ax_ref).unwrap_or((2048.0, 1400.0));
                let (hx, hy) = compute_hide_position(&self.monitors, w);
                let _ = ax_set_position(ax_ref, hx, hy);
            }
        }

        self.apply_layout();
        if let Some(next) = self.active_workspace().focused {
            self.focus_window(next);
        }
        tracing::info!(id = focused, target = num, "moved window to workspace");
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
        tracing::info!(id, "window closed -> retiling");
        self.registry.remove(id);
        self.ax_refs.remove(&id);

        // Find the workspace containing this window and remove from it
        if let Some(ws_idx) = self.workspaces.find_window(id) {
            let ws = self.workspaces.get_mut(ws_idx);
            ws.floating.retain(|f| f.id != id);
            ws.tree.remove(id);
            if ws.focused == Some(id) {
                ws.pop_focus();
            }
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
            // Find the workspace containing this window and remove from it
            if let Some(ws_idx) = self.workspaces.find_window(w.id) {
                let ws = self.workspaces.get_mut(ws_idx);
                ws.floating.retain(|f| f.id != w.id);
                ws.tree.remove(w.id);
            }
        }
        if !removed.is_empty() {
            // Fix focus on any affected workspace
            for ws_idx in 0..self.workspaces.count() {
                let ws = self.workspaces.get_mut(ws_idx);
                if ws
                    .focused
                    .is_some_and(|f| removed.iter().any(|w| w.id == f))
                {
                    ws.pop_focus();
                }
            }
            self.apply_layout();
            tracing::info!(pid, removed = removed.len(), "app terminated -> retiled");
        }
    }

    /// Check if a window is on an inactive workspace (intentionally hidden).
    pub fn is_window_hidden(&self, wid: u32) -> bool {
        let id = wid as WindowId;
        if let Some(ws_idx) = self.workspaces.find_window(id) {
            !self.workspaces.get(ws_idx).visible
        } else {
            false
        }
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
            let target_idx = (ws_num as usize).saturating_sub(1);
            let is_active = target_idx == self.active_ws_idx();
            let target_rect = self.monitor_showing_workspace(target_idx)
                .map(|mi| self.monitor_rect(mi))
                .unwrap_or(self.focused_rect());

            let ws = self.workspaces.get_or_create(target_idx);
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
            let sr = self.focused_rect();
            let ws = self.active_workspace_mut();
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
                tracing::info!(id, app = app_name, title = %title, "window created -> tiling");
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
                self.fix_oversized_windows();
            }
            WindowEvent::Destroyed { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && self.registry.contains(id)
                {
                    tracing::info!(id, app = app_name, "window destroyed -> retiling");
                    self.registry.remove(id);
                    self.ax_refs.remove(&id);
                    // Find the workspace containing this window and remove from it
                    if let Some(ws_idx) = self.workspaces.find_window(id) {
                        let ws = self.workspaces.get_mut(ws_idx);
                        ws.floating.retain(|f| f.id != id);
                        ws.tree.remove(id);
                        if ws.focused == Some(id) {
                            ws.pop_focus();
                        }
                    }
                    self.apply_layout();
                }
            }
            WindowEvent::FocusChanged { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && self.registry.contains(id)
                {
                    self.active_workspace_mut().record_focus(id);
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
                        .active_workspace_mut()
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
                        .active_workspace_mut()
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
    monitors: &[super::monitor::Monitor],
    window_width: f64,
) -> (f64, f64) {
    if monitors.is_empty() {
        return (1.0 - window_width, 10000.0);
    }
    // Find the leftmost x and the bottommost y across all monitors
    let min_x = monitors
        .iter()
        .map(|m| m.frame.x)
        .fold(f64::INFINITY, f64::min);
    let max_y = monitors
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
