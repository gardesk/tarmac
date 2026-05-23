use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
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
use super::workspace::{WorkspaceDefinition, WorkspaceManager, WorkspaceTarget};

struct QueuedEvent {
    event: WindowEvent,
    app_name: String,
    app_bundle: String,
}

/// Active drag operation on a floating window.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DragMode {
    Move,
    Resize,
    /// HX-09: drag started on a tiled window; the window was floated
    /// when the cursor crossed `drag_threshold` and now follows the
    /// cursor. On release, `end_drag` runs the drop-disposition
    /// cascade to retile at the drop point (or stay floating).
    TiledRelocate,
}

#[derive(Debug, Clone, Copy)]
struct DragState {
    window_id: WindowId,
    mode: DragMode,
    start_mouse: (f64, f64),
    start_geometry: Rect,
    /// HX-09: source workspace at drag start, used by end_drag to
    /// remove the floated window from the right floating list before
    /// retiling.
    original_workspace: usize,
    /// HX-09: the tile rect the window occupied at drag start. Used to
    /// pick the float-on-drag size (a scaled-down copy).
    original_tile_rect: Rect,
}

/// Sliding-window size for AX focus storm detection.
const EXT_FOCUS_STORM_WINDOW: std::time::Duration = std::time::Duration::from_millis(200);
/// Adoption count inside the window above which we declare a storm and
/// enter quiet mode. Pathological apps (Messages, Safari dialogs) have been
/// observed at >2000 events/sec; normal user navigation peaks at ~30/sec.
const EXT_FOCUS_STORM_THRESHOLD: usize = 20;
/// Duration of quiet mode once a storm is detected. After it expires, the
/// most recent target observed during quiet is applied as a single focus
/// adoption via the polling tick.
const EXT_FOCUS_QUIET_DURATION: std::time::Duration = std::time::Duration::from_millis(500);

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
    focus_return_memory: HashMap<(WindowId, super::tree::Direction), WindowId>,
    /// Recent intentional focus activations (window id, app pid, expiry).
    /// Acts as a small ring so a burst of internal activations (e.g. a
    /// workspace switch followed by a follow-up FFM focus) can all be
    /// matched against the lagging `NSWorkspace.frontmostApplication` poll
    /// without the most recent entry clobbering older ones.
    recent_internal_focus: Vec<(WindowId, i32, std::time::Instant)>,
    /// While set, drop incoming external-focus callbacks unconditionally.
    /// Armed on workspace switch to absorb the storm of frontmost-app
    /// transitions macOS reports while AX activations propagate.
    workspace_switch_silence_until: Option<std::time::Instant>,
    /// Sliding window of recent adopt_external_focus timestamps used to
    /// detect AX focus storms (misbehaving apps emitting hundreds of
    /// AXFocusedWindowChanged events per second). When the rate breaches
    /// EXT_FOCUS_STORM_THRESHOLD events inside EXT_FOCUS_STORM_WINDOW,
    /// adoption enters quiet mode until `ext_focus_quiet_until`.
    ext_focus_history: VecDeque<std::time::Instant>,
    /// While set, drop incoming external-focus adoptions; the latest target
    /// observed during quiet is stashed in `ext_focus_pending_target` and
    /// applied once via the polling tick after quiet expires.
    ext_focus_quiet_until: Option<std::time::Instant>,
    ext_focus_pending_target: Option<WindowId>,
    drag: Option<DragState>,
    pub focus_follows_mouse: bool,
    pub mouse_follows_focus: bool,
    pub gap_inner: f64,
    pub gap_outer: f64,
    /// HX-02: when true, `Action::Resize` adjusts the deepest matching
    /// ancestor split (climb-past-non-matching behavior). When false,
    /// adjusts the shallowest matching ancestor (pre-HX-02 behavior).
    pub smart_resizing: bool,
    /// HX-03: when true, `Action::Swap(Direction)` at a workspace edge
    /// crosses to the geographically adjacent monitor in any of the
    /// four directions. When false it no-ops at the edge.
    pub window_direction_monitor_fallback: bool,
    /// HX-05 split-axis policy. See `Settings::split_axis`.
    pub split_axis: String,
    /// HX-05 child-ordering policy. See `Settings::force_split`.
    pub force_split: String,
    /// HX-05 aspect threshold used when `split_axis = "smart"`.
    pub split_width_multiplier: f64,
    /// HX-05: preserve parent split direction on existing-Internal
    /// inserts (reserved for future use; current insert path doesn't
    /// re-derive direction on existing Internals).
    pub preserve_split: bool,
    /// HX-05: when true, `Action::Preselect` hints persist across
    /// multiple window opens rather than clearing after one use.
    pub permanent_direction_override: bool,
    /// HX-05 focal-point source policy. See `Settings::focal_point_source`.
    pub focal_point_source: String,
    /// HX-05 cursor cache, updated on every `mouse_moved` so AX-driven
    /// new-window callbacks (which fire without a corresponding mouse
    /// event) can still read the last-known cursor location.
    pub last_cursor: Option<(f64, f64)>,
    /// HX-06 pseudo-tile set. Members occupy a slot in the BSP tree but
    /// `show_window` skips the AX size write — they render at their
    /// preferred size, centered within the assigned tile rect. In-memory
    /// only; restart resets all windows to stretched.
    pseudo_windows: std::collections::HashSet<WindowId>,
    /// HX-08 minimum pixel delta a Mod+drag must travel before crossing
    /// the threshold. Mirrors `Settings::drag_threshold`.
    pub drag_threshold: f64,
    /// HX-08: while Some(now < deadline), `adopt_external_focus` drops
    /// incoming focus events. Parallels `workspace_switch_silence_until`.
    /// Set by `begin_move_drag` on a tiled hit, refreshed by
    /// `update_drag`, cleared by `end_drag`.
    drag_suppress_focus_until: Option<std::time::Instant>,
    /// HX-08: true between `begin_move_drag` (tiled hit) and the moment
    /// the cursor crosses `drag_threshold`. Below threshold the drag
    /// stays dormant — no AX writes, no float transition. HX-09 will
    /// fill in what happens when this flag transitions to false.
    drag_threshold_active: bool,
    /// HX-08: per-window timestamp of the last AX position/size write.
    /// `show_window` consults this to throttle rapid resize loops and
    /// drag-cursor-follow writes to ~60 Hz per window. Interior
    /// mutability so the existing `&self`-only call sites (apply_layout,
    /// show_window) can update it without rippling `&mut self` through
    /// dozens of paths.
    ax_write_last: std::cell::RefCell<HashMap<WindowId, std::time::Instant>>,
    /// HX-08: writes throttled by `should_throttle_ax_write` go here
    /// and are retried at the top of the next `apply_layout`. Same
    /// interior-mutability rationale as `ax_write_last`.
    pending_show_window: std::cell::RefCell<HashMap<WindowId, Rect>>,
    pub rules: Vec<crate::config::lua::WindowRule>,
    /// Active special workspace overlay per monitor (None = no overlay).
    active_specials: Vec<Option<usize>>,
    /// Overlay sizing config per special workspace name.
    pub special_configs: Vec<crate::config::lua::SpecialWorkspaceConfig>,
    /// User-configured workspace metadata and definitions.
    pub workspace_defs: Vec<WorkspaceDefinition>,
    /// Border overlay manager for focused/unfocused window borders.
    pub borders: crate::platform::border::BorderManager,
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
            focus_return_memory: HashMap::new(),
            recent_internal_focus: Vec::new(),
            workspace_switch_silence_until: None,
            ext_focus_history: VecDeque::new(),
            ext_focus_quiet_until: None,
            ext_focus_pending_target: None,
            drag: None,
            focus_follows_mouse: true,
            mouse_follows_focus: true,
            gap_inner: 0.0,
            gap_outer: 0.0,
            smart_resizing: true,
            window_direction_monitor_fallback: true,
            split_axis: "smart".to_string(),
            force_split: "follow_mouse".to_string(),
            split_width_multiplier: 1.0,
            preserve_split: false,
            permanent_direction_override: false,
            focal_point_source: "cursor_or_active".to_string(),
            last_cursor: None,
            pseudo_windows: std::collections::HashSet::new(),
            drag_threshold: 5.0,
            drag_suppress_focus_until: None,
            drag_threshold_active: false,
            ax_write_last: std::cell::RefCell::new(HashMap::new()),
            pending_show_window: std::cell::RefCell::new(HashMap::new()),
            rules: Vec::new(),
            active_specials: Vec::new(),
            special_configs: Vec::new(),
            workspace_defs: crate::config::lua::default_workspace_definitions(),
            borders: crate::platform::border::BorderManager::new(),
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

    /// The effective focused window, checking the active special workspace first.
    fn effective_focused(&self) -> Option<WindowId> {
        let mi = self.focused_monitor;
        if mi < self.active_specials.len()
            && let Some(special_idx) = self.active_specials[mi]
            && let Some(wid) = self.workspaces.get(special_idx).focused
        {
            return Some(wid);
        }
        self.active_workspace().focused
    }

    /// Active workspace (mutable).
    pub fn active_workspace_mut(&mut self) -> &mut super::workspace::Workspace {
        let idx = self.active_ws_idx();
        self.workspaces.get_mut(idx)
    }

    pub fn set_workspace_defs(&mut self, defs: Vec<WorkspaceDefinition>) {
        self.workspace_defs = defs;
        self.workspace_defs.sort_by_key(|def| def.id.to_string());
        self.workspace_defs.dedup_by(|a, b| a.id == b.id);
        self.ensure_defined_workspaces();
    }

    fn ensure_defined_workspaces(&mut self) {
        for def in &self.workspace_defs {
            self.workspaces.get_or_create_by_id(def.id.clone());
        }
    }

    pub fn workspace_prefs(
        &self,
        ws_idx: usize,
    ) -> Option<&crate::core::workspace::WorkspacePrefs> {
        let id = &self.workspaces.get(ws_idx).id;
        self.workspace_defs
            .iter()
            .find(|def| &def.id == id)
            .map(|def| &def.prefs)
    }

    pub fn workspace_gaps(&self, ws_idx: usize) -> (f64, f64) {
        if let Some(prefs) = self.workspace_prefs(ws_idx) {
            (
                prefs.gap_inner.unwrap_or(self.gap_inner),
                prefs.gap_outer.unwrap_or(self.gap_outer),
            )
        } else {
            (self.gap_inner, self.gap_outer)
        }
    }

    fn workspace_index_for_target(&mut self, target: &WorkspaceTarget) -> usize {
        self.workspaces.get_or_create_target(target)
    }

    fn preferred_monitor_for_workspace(&self, ws_idx: usize) -> Option<usize> {
        let prefs = self.workspace_prefs(ws_idx)?;
        let display_id = prefs.monitor.as_ref()?.display_id;
        self.monitors
            .iter()
            .position(|monitor| monitor.id == display_id)
    }

    /// Usable frame for a monitor, adjusted for bar_height.
    /// Only applies bar_height on monitors where macOS didn't already
    /// deduct a system menu bar (i.e., non-primary / external monitors).
    pub fn monitor_rect(&self, mi: usize) -> Rect {
        let m = &self.monitors[mi];
        let r = m.usable_frame;
        // If usable_frame.y == frame.y, the system didn't deduct a menu bar
        // for this monitor — apply bar_height for sketchybar/etc.
        // If usable_frame.y > frame.y, the system already reserved space.
        let needs_bar = self.bar_height > 0.0 && (r.y - m.frame.y).abs() < 1.0;
        if needs_bar {
            Rect::new(
                r.x,
                r.y + self.bar_height,
                r.width,
                r.height - self.bar_height,
            )
        } else {
            r
        }
    }

    /// Usable frame for focused monitor.
    pub fn focused_rect(&self) -> Rect {
        self.monitor_rect(self.focused_monitor)
    }

    /// Find which monitor index has a workspace visible, if any.
    pub fn monitor_showing_workspace(&self, ws_idx: usize) -> Option<usize> {
        self.monitors
            .iter()
            .position(|m| m.active_workspace == ws_idx)
    }

    fn special_showing_workspace(&self, ws_idx: usize) -> bool {
        self.active_specials
            .iter()
            .flatten()
            .any(|&idx| idx == ws_idx)
    }

    fn workspace_is_effectively_visible(&self, ws_idx: usize) -> bool {
        self.monitor_showing_workspace(ws_idx).is_some() || self.special_showing_workspace(ws_idx)
    }

    fn sync_workspace_visibility(&mut self) {
        for ws_idx in 0..self.workspaces.count() {
            let visible = self.workspace_is_effectively_visible(ws_idx);
            self.workspaces.get_mut(ws_idx).visible = visible;
        }
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

        let cursor_mi = super::monitor::index_at_point(&self.monitors, cx, cy).unwrap_or(0);
        self.focused_monitor = cursor_mi;
        tracing::info!(
            monitor = cursor_mi,
            "focused monitor set to cursor location"
        );

        self.ensure_defined_workspaces();

        let mut assigned = vec![false; self.workspaces.count()];
        let mut monitor_assignments = vec![None; self.monitors.len()];

        for (ws_idx, is_assigned) in assigned
            .iter_mut()
            .enumerate()
            .take(self.workspaces.count())
        {
            if let Some(mi) = self.preferred_monitor_for_workspace(ws_idx)
                && monitor_assignments[mi].is_none()
            {
                monitor_assignments[mi] = Some(ws_idx);
                *is_assigned = true;
            }
        }

        if monitor_assignments[cursor_mi].is_none() {
            monitor_assignments[cursor_mi] = Some(0);
            assigned[0] = true;
        }

        let mut next_ws_idx = 0usize;
        for assignment in monitor_assignments.iter_mut().take(self.monitors.len()) {
            if assignment.is_none() {
                while next_ws_idx < assigned.len() && assigned[next_ws_idx] {
                    next_ws_idx += 1;
                }
                if next_ws_idx >= assigned.len() {
                    self.workspaces.get_or_create(next_ws_idx);
                    assigned.push(false);
                }
                *assignment = Some(next_ws_idx);
                assigned[next_ws_idx] = true;
            }
        }

        for (mi, ws_idx) in monitor_assignments.into_iter().enumerate() {
            let ws_idx = ws_idx.unwrap_or(0);
            self.monitors[mi].active_workspace = ws_idx;
            self.workspaces.get_mut(ws_idx).last_monitor = Some(mi);
            self.workspaces.get_mut(ws_idx).last_display_id = Some(self.monitors[mi].id);
        }
        self.sync_workspace_visibility();

        tracing::info!(
            monitors = self.monitors.len(),
            focused = self.focused_monitor,
            "displays configured"
        );

        let windows = discover_all_windows();
        // Route each discovered window to the workspace active on the
        // monitor it physically lives on, not always the cursor's
        // monitor's workspace. add_window_to_active resolves the target
        // workspace via active_workspace_mut() which reads
        // focused_monitor — temporarily point that at each window's
        // monitor for the duration of the insert, then restore.
        // Without this, windows on non-cursor monitors get inserted
        // into the cursor monitor's BSP tree and apply_layout asks AX
        // to move them across displays; some apps (Ghostty in
        // particular) fail or race that move and end up logically in
        // the wrong workspace's tree while remaining physically on
        // their original monitor — visible to the user as windows
        // that are reachable via stack-cycle but never join the tile.
        let original_focused = self.focused_monitor;
        for w in &windows {
            let cx = w.x + w.width / 2.0;
            let cy = w.y + w.height / 2.0;
            let mi =
                super::monitor::index_at_point(&self.monitors, cx, cy).unwrap_or(original_focused);
            self.focused_monitor = mi;
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
        self.focused_monitor = original_focused;

        tracing::info!(
            windows = self.registry.count(),
            "initial window registry populated"
        );

        // Catch BSP overflow from tree geometry before writing AX. Without
        // this, the first apply_layout writes sub-pixel tile rects for the
        // overflow case (observed 2026-05-19: 17 windows in workspace 1
        // produced req_w=0, req_h=0 tiles macOS clamped to 105×33 on top of
        // each other), leaving the user looking at overlapping windows
        // until fix_oversized_windows_aggressive kicks in a second later.
        self.proactive_overflow_normalize();

        // Apply layout twice: first pass moves windows to the correct
        // monitor, second pass resizes correctly since they're already there.
        // Needed because macOS AX constrains resize to the window's current
        // display, and the position change may not have taken effect yet.
        self.apply_layout();
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.apply_layout();
        self.fix_oversized_windows_aggressive();
        self.update_borders();
        self.install_observers_for_all();

        tracing::info!(observers = self.observers.len(), "observers installed");
    }

    /// Get the AX element reference for a window (for IPC queries).
    pub fn get_ax_ref(&self, id: WindowId) -> Option<&CFRetained<AXUIElement>> {
        self.ax_refs.get(&id)
    }

    /// Reverse-lookup a window id from its AXUIElement. Used by the
    /// AXUIElementDestroyed handler when the element is already invalidated
    /// by macOS at notification time, so `_AXUIElementGetWindow` returns
    /// `kAXErrorInvalidUIElement` and we cannot extract the wid directly.
    /// CFEqual on AXUIElement compares element identity, not the underlying
    /// window's liveness, so this still works after destruction.
    fn find_window_by_ax_element(&self, target: &AXUIElement) -> Option<WindowId> {
        use objc2_core_foundation::CFType;
        let target_cf: &CFType = target.as_ref();
        self.ax_refs
            .iter()
            .find(|(_, elem)| (elem.as_ref() as &CFType) == target_cf)
            .map(|(id, _)| *id)
    }

    /// Reap any wid in the registry that is no longer present in CGWindowList
    /// (all layers, on- and off-screen) and is not currently staged-hidden by
    /// tarmac itself. This is the authoritative safety net for close detection
    /// — the AX `Destroyed` notification can fire with an already-invalidated
    /// element, and the workspace polling path can race with workspace
    /// switches. A 1Hz reconcile guarantees the tree never holds onto a wid
    /// that the WindowServer has actually destroyed.
    pub fn reconcile_registry_against_window_server(&mut self) {
        use crate::platform::application::get_cg_window_list_all_layers;
        let live_wids: std::collections::HashSet<u32> = get_cg_window_list_all_layers()
            .into_iter()
            .map(|info| info.wid)
            .collect();
        let mut stale: Vec<u32> = Vec::new();
        for w in self.registry.all() {
            if !live_wids.contains(&w.id) && !self.is_window_hidden(w.id) {
                stale.push(w.id);
            }
        }
        for wid in stale {
            tracing::info!(wid, "reconcile: window vanished from WindowServer, reaping");
            self.on_window_closed(wid);
        }
    }

    /// Inflate the user-configured gaps by ers's border width so the
    /// configured value is the *visible* empty space between tiles, not
    /// the gross window-rect-to-window-rect distance. Ers draws border
    /// overlays 4px (or whatever `border_width` is) outside each window's
    /// frame; with a raw 8px tile gap, two adjacent windows' 4px borders
    /// fill the entire gap and visually touch. Adding 2 * border_width
    /// to inner gaps and 1 * border_width to outer gaps reserves space
    /// for the borders so the configured gap survives as visible empty
    /// space.
    pub fn effective_gaps(&self, ws_idx: usize) -> (f64, f64) {
        let (gap_inner, gap_outer) = self.workspace_gaps(ws_idx);
        let bw = self.borders.border_width.max(0.0);
        (gap_inner + 2.0 * bw, gap_outer + bw)
    }

    fn workspace_render_geometries(&self, ws_idx: usize, rect: Rect) -> Vec<(WindowId, Rect)> {
        let (gap_inner, gap_outer) = self.effective_gaps(ws_idx);
        self.workspaces
            .get(ws_idx)
            .tree
            .calculate_geometries_with_gaps(rect, gap_inner, gap_outer, true)
    }

    fn workspace_focus_geometries(&self, ws_idx: usize, rect: Rect) -> Vec<(WindowId, Rect)> {
        let (gap_inner, gap_outer) = self.effective_gaps(ws_idx);
        self.workspaces
            .get(ws_idx)
            .tree
            .calculate_focus_geometries_with_gaps(rect, gap_inner, gap_outer, true)
    }

    pub fn process_events(&mut self) {
        let events: Vec<QueuedEvent> = self.event_queue.borrow_mut().drain(..).collect();
        for queued in events {
            self.handle_event(&queued.event, &queued.app_name, &queued.app_bundle);
        }
        self.enforce_hidden_workspaces();
    }

    // --- Layout ---

    /// Apply layout for ALL visible workspaces on their respective monitors.
    /// Uses the yabai/AeroSpace pattern: disable AXEnhancedUserInterface,
    /// then size → position → size per window.
    pub fn apply_layout(&self) {
        // HX-08: catch up any AX writes the per-window rate limiter
        // deferred from earlier frames before issuing fresh ones.
        self.drain_pending_show_window();
        for (mi, monitor) in self.monitors.iter().enumerate() {
            let ws_idx = monitor.active_workspace;
            let ws = self.workspaces.get(ws_idx);
            let screen_rect = self.monitor_rect(mi);
            let geometries = self.workspace_render_geometries(ws_idx, screen_rect);
            // Only the active member of each stack lives at the tile rect.
            // Non-active members get staged off-screen via hide_window so
            // macOS's per-app global z-order cannot surface a non-active
            // member above the active one (which happened when stacks
            // contained windows from different apps and the user focused
            // a sibling of one of those apps elsewhere — e.g. a Ghostty
            // member of a Messages-active stack rising to the top of the
            // tile when the user focused a different Ghostty on another
            // monitor). focus_geometries returns one entry per visible
            // tile (active for stacks, the leaf window for BSP leaves), so
            // the difference vs render geometries is exactly the set of
            // non-active stack members.
            let actives = self.workspace_focus_geometries(ws_idx, screen_rect);
            let active_ids: std::collections::HashSet<super::window::WindowId> =
                actives.iter().map(|(w, _)| *w).collect();
            tracing::debug!(monitor = mi, workspace = %ws.id, windows = geometries.len(),
                sr_x = screen_rect.x, sr_y = screen_rect.y, sr_w = screen_rect.width,
                sr_h = screen_rect.height, "apply_layout");

            for (wid, rect) in &actives {
                tracing::trace!(
                    wid,
                    x = rect.x,
                    y = rect.y,
                    w = rect.width,
                    h = rect.height,
                    "tile"
                );
                self.show_window(*wid, *rect);
            }
            for (wid, _) in &geometries {
                if !active_ids.contains(wid) {
                    tracing::trace!(wid, "stack non-active hidden off-screen");
                    self.hide_window(*wid);
                }
            }

            for fw in &ws.floating {
                tracing::debug!(
                    wid = fw.id,
                    x = fw.geometry.x,
                    y = fw.geometry.y,
                    w = fw.geometry.width,
                    h = fw.geometry.height,
                    "show floating"
                );
                self.show_window(fw.id, fw.geometry);
            }
            self.restack_floating_windows(ws_idx);
        }
    }

    /// Update border overlays for all visible windows.
    pub fn update_borders(&mut self) {
        if !self.borders.is_enabled() {
            return;
        }

        for (mi, monitor) in self.monitors.iter().enumerate() {
            let ws_idx = monitor.active_workspace;
            let ws = self.workspaces.get(ws_idx);
            let sr = self.monitor_rect(mi);
            let geoms = self.workspace_focus_geometries(ws_idx, sr);
            let focused = ws.focused;

            for (wid, rect) in &geoms {
                self.borders
                    .update_border(*wid, *rect, focused == Some(*wid));
            }
            for fw in &ws.floating {
                self.borders
                    .update_border(fw.id, fw.geometry, focused == Some(fw.id));
            }
        }
    }

    fn show_window(&self, wid: WindowId, rect: Rect) {
        use crate::platform::accessibility::ax_set_bool;

        let Some(ax_ref) = self.ax_refs.get(&wid) else {
            return;
        };

        // HX-08: throttle per-window to ~60 Hz. Rapid back-to-back
        // writes (drag cursor-follow, resize oscillation) get deferred
        // to the next apply_layout, which fires often enough that
        // throttled writes catch up within ~16-50ms.
        if self.should_throttle_ax_write(wid) {
            self.pending_show_window.borrow_mut().insert(wid, rect);
            tracing::trace!(wid, "ax write throttled — deferred");
            return;
        }

        // HX-06 pseudo-tile: position the window centered in `rect` at
        // its own preferred (current AX) size. No size writes, no EUI
        // twiddle — preserving the natural app dimensions is the whole
        // point. The window still occupies a slot in the tree; only
        // the visible footprint differs from a normal tile.
        if self.pseudo_windows.contains(&wid) {
            let (aw, ah) = ax_get_size(ax_ref).unwrap_or((rect.width, rect.height));
            let px = rect.x + ((rect.width - aw) / 2.0).max(0.0);
            let py = rect.y + ((rect.height - ah) / 2.0).max(0.0);
            let _ = crate::platform::skylight::set_window_group_system_alpha(wid, 1.0);
            let _ = crate::platform::skylight::set_window_group_alpha(wid, 1.0);
            let _ = ax_set_position(ax_ref, px, py);
            let _ = crate::platform::skylight::move_window(wid, px, py);
            return;
        }

        let eui_key = objc2_core_foundation::CFString::from_static_str("AXEnhancedUserInterface");
        let app_ref = self
            .registry
            .get(wid)
            .map(|w| unsafe { AXUIElement::new_application(w.app_pid) });

        // Disable AXEnhancedUserInterface (yabai/AeroSpace workaround).
        // This makes macOS more compliant with resize requests.
        let was_eui = app_ref
            .as_ref()
            .and_then(|app| crate::platform::accessibility::ax_get_bool(app, &eui_key).ok());
        if was_eui == Some(true)
            && let Some(app) = &app_ref
        {
            let _ = ax_set_bool(app, &eui_key, false);
        }

        // Reassert visibility before and after geometry changes in case the
        // app recreates backing surfaces while being shown again.
        let _ = crate::platform::skylight::set_window_group_system_alpha(wid, 1.0);
        let _ = crate::platform::skylight::set_window_group_alpha(wid, 1.0);

        // Size → Position → Size (yabai/AeroSpace order)
        let _ = ax_set_size(ax_ref, rect.width, rect.height);
        let _ = ax_set_position(ax_ref, rect.x, rect.y);
        let _ = ax_set_size(ax_ref, rect.width, rect.height);
        // Some apps silently ignore AX-set-position (observed: windows that
        // were previously fullscreen/zoomed, or processes whose AX state has
        // diverged from the WindowServer). Backstop the AX move with a direct
        // SkyLight move so the visible position matches what we asked for.
        // Idempotent for windows already at (rect.x, rect.y).
        let _ = crate::platform::skylight::move_window(wid, rect.x, rect.y);
        let _ = crate::platform::skylight::set_window_group_system_alpha(wid, 1.0);
        let _ = crate::platform::skylight::set_window_group_alpha(wid, 1.0);

        if let (Ok((ax, ay)), Ok((aw, ah))) = (ax_get_position(ax_ref), ax_get_size(ax_ref)) {
            let dx = (ax - rect.x).abs();
            let dy = (ay - rect.y).abs();
            let dw = (aw - rect.width).abs();
            let dh = (ah - rect.height).abs();
            if dx > 1.0 || dy > 1.0 || dw > 1.0 || dh > 1.0 {
                tracing::debug!(
                    wid,
                    req_x = rect.x,
                    req_y = rect.y,
                    req_w = rect.width,
                    req_h = rect.height,
                    got_x = ax,
                    got_y = ay,
                    got_w = aw,
                    got_h = ah,
                    "show_window mismatch"
                );
            }
        }

        // Restore AXEnhancedUserInterface
        if was_eui == Some(true)
            && let Some(app) = &app_ref
        {
            let _ = ax_set_bool(app, &eui_key, true);
        }
    }

    /// After layout, check for windows that overflow their tiles.
    /// Try swapping the oversized window into the largest available tile.
    /// If it still doesn't fit, auto-float it.
    ///
    /// `aggressive` is set during initial discovery: if the BSP layout would
    /// leave any window unable to fit its tile, collapse the entire workspace
    /// into a single root-level stack instead of producing the hybrid
    /// "few-tiles-plus-corner-stack" shape that arises from the staircase BSP
    /// shape on cold start with many windows. Normal runtime overflow keeps
    /// the original Phase 2 behavior — small subtree stacks are fine when the
    /// user has already arranged the layout.
    /// Pre-flight check: scan each visible workspace's BSP-computed tile
    /// geometry and, if any tile would be smaller than the OS will accept
    /// for a normal window, collapse that workspace into a root stack
    /// before apply_layout writes the broken layout to AX.
    ///
    /// This is the proactive counterpart to fix_oversized_windows: the
    /// latter runs *after* apply_layout has already produced visible 0×0
    /// tiles and the OS has clamped them to its minimum (105×33 on
    /// Tahoe), leaving the user staring at overlapping windows for a
    /// full half-second before the safety net kicks in. By catching
    /// overflow purely from tree geometry — no AX calls — we can fix it
    /// before it ever reaches the screen.
    ///
    /// Threshold rationale: 150×100. Most apps' minimum window size is
    /// at least 200×150 and macOS hard-clamps small windows to ~100×33;
    /// 150×100 is conservatively below any legitimately small tile in a
    /// normal BSP layout (a 5-way split of a 1728-px-wide laptop is still
    /// ~340 px) while large enough to catch every overflow pattern seen
    /// in the wild.
    pub fn proactive_overflow_normalize(&mut self) {
        const MIN_TILE_W: f64 = 150.0;
        const MIN_TILE_H: f64 = 100.0;

        let monitor_count = self.monitors.len();
        for mi in 0..monitor_count {
            let ws_idx = self.monitors[mi].active_workspace;
            let screen_rect = self.monitor_rect(mi);
            let geometries = self.workspace_focus_geometries(ws_idx, screen_rect);
            if geometries.len() <= 1 {
                continue;
            }

            // Pick the worst-offending tile and use its window as the active
            // for the collapsed stack so focus lands on the most recently
            // problematic window. Falls back to the focused window if any.
            let worst = geometries
                .iter()
                .filter(|(_, r)| r.width < MIN_TILE_W || r.height < MIN_TILE_H)
                .min_by(|(_, a), (_, b)| {
                    let area_a = a.width * a.height;
                    let area_b = b.width * b.height;
                    area_a
                        .partial_cmp(&area_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

            let Some((overflow_wid, overflow_rect)) = worst else {
                continue;
            };
            let active = self
                .workspaces
                .get(ws_idx)
                .focused
                .unwrap_or(*overflow_wid);
            let min_w = overflow_rect.width;
            let min_h = overflow_rect.height;

            if self
                .workspaces
                .get_mut(ws_idx)
                .tree
                .collapse_to_root_stack(active)
            {
                tracing::info!(
                    ws = ws_idx + 1,
                    smallest_tile_w = min_w,
                    smallest_tile_h = min_h,
                    threshold_w = MIN_TILE_W,
                    threshold_h = MIN_TILE_H,
                    "proactive collapse: workspace overflow detected, collapsing to root stack"
                );
            }
        }
    }

    pub fn fix_oversized_windows(&mut self) {
        self.fix_oversized_windows_with(false);
    }

    pub fn fix_oversized_windows_aggressive(&mut self) {
        self.fix_oversized_windows_with(true);
    }

    fn fix_oversized_windows_with(&mut self, aggressive: bool) {
        // Give apps time to settle to their actual minimum size after apply_layout.
        // Some apps (e.g. Messages) handle AX resize asynchronously — they ack the
        // request but snap to their minimum size later.
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Collect the initial set of workspaces to check (visible on monitors).
        let mut pending: Vec<usize> = (0..self.monitors.len())
            .map(|mi| self.monitors[mi].active_workspace)
            .collect();
        // Track workspaces we've already fully processed to avoid infinite loops.
        let mut processed: Vec<usize> = Vec::new();
        while let Some(ws_idx) = pending.pop() {
            if processed.contains(&ws_idx) {
                continue;
            }

            // Determine the screen rect for this workspace's layout.
            let screen_rect = self
                .monitor_showing_workspace(ws_idx)
                .map(|mi| self.monitor_rect(mi))
                .unwrap_or_else(|| self.focused_rect());
            // Phase 1: Swap oversized windows into larger tiles.
            // Batch all swaps using BSP geometry (no AX calls), then apply layout
            // once and settle. This is both faster (one layout instead of N) and
            // more correct (displaced windows settle before re-checking).
            let mut settled: Vec<super::window::WindowId> = Vec::new();
            let mut did_any_swap = true;
            while did_any_swap {
                did_any_swap = false;
                let mut no_swap: Vec<super::window::WindowId> = Vec::new();

                loop {
                    // Geometries are computed from the BSP tree (cheap, no AX).
                    // After batched swaps, tree geometry reflects the new layout
                    // even before apply_layout sends AX commands.
                    let geometries = self.workspace_focus_geometries(ws_idx, screen_rect);
                    if geometries.is_empty() {
                        break;
                    }

                    // Find the first oversized window (skip settled and no-swap).
                    // Use the pre-swap ax_get_size (actual minimum) against the
                    // BSP-computed tile rect to decide if a swap is needed.
                    let oversized = geometries.iter().find_map(|(wid, rect)| {
                        if settled.contains(wid) || no_swap.contains(wid) {
                            return None;
                        }
                        // HX-06: pseudo windows render at their preferred
                        // size intentionally; their "overflow" is by design.
                        if self.pseudo_windows.contains(wid) {
                            return None;
                        }
                        let ax_ref = self.ax_refs.get(wid)?;
                        let (aw, ah) = ax_get_size(ax_ref).ok()?;
                        if aw > rect.width + 1.0 || ah > rect.height + 1.0 {
                            Some((*wid, aw, ah))
                        } else {
                            None
                        }
                    });

                    let (ow, min_w, min_h) = match oversized {
                        Some(v) => v,
                        None => break,
                    };

                    let best_swap = geometries
                        .iter()
                        .filter(|(wid, _)| *wid != ow && !settled.contains(wid))
                        .filter(|(_, rect)| rect.width >= min_w - 1.0 && rect.height >= min_h - 1.0)
                        .max_by(|(_, a), (_, b)| {
                            (a.width * a.height)
                                .partial_cmp(&(b.width * b.height))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .map(|(wid, _)| *wid);

                    if let Some(swap_target) = best_swap {
                        tracing::info!(
                            oversized = ow,
                            target = swap_target,
                            ws = ws_idx + 1,
                            "swapping oversized window into larger tile"
                        );
                        // Swap in BSP tree only — no apply_layout yet
                        self.workspaces.get_mut(ws_idx).tree.swap(ow, swap_target);
                        settled.push(ow);
                        did_any_swap = true;
                        // Don't break — continue scanning for more swaps in the
                        // same pass using updated BSP geometry
                    } else {
                        no_swap.push(ow);
                    }
                }

                if did_any_swap {
                    // Single apply_layout for all swaps in this pass, then settle
                    self.apply_layout();
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                // Outer loop re-checks: displaced windows may now overflow after
                // settling. The settled set prevents ping-pong.
            }

            // Phase 2: Replace the smallest conflicting subtree with a stack.
            // In aggressive mode (cold start), collapse the entire workspace
            // tree into one root-level stack on the first oversized window,
            // instead of leaving a hybrid layout where some windows got their
            // own tile and the rest piled into a corner subtree.
            loop {
                let geometries = self.workspace_focus_geometries(ws_idx, screen_rect);
                if geometries.is_empty() {
                    break;
                }

                // A single window alone on a workspace can never truly overflow —
                // it gets the full screen. ax_get_size may return a stale size from
                // a previous tile, not the actual minimum. Skip overflow detection
                // for solo windows.
                if geometries.len() <= 1 {
                    break;
                }

                let oversized = geometries.iter().find_map(|(wid, rect)| {
                    // HX-06: pseudo windows are intentionally smaller
                    // than their tile rect; don't treat them as oversized.
                    if self.pseudo_windows.contains(wid) {
                        return None;
                    }
                    let ax_ref = self.ax_refs.get(wid)?;
                    let (aw, ah) = ax_get_size(ax_ref).ok()?;
                    if aw > rect.width + 1.0 || ah > rect.height + 1.0 {
                        Some((*wid, aw, ah))
                    } else {
                        None
                    }
                });

                let (oversized_wid, min_w, min_h) = match oversized {
                    Some(v) => v,
                    None => break,
                };

                if aggressive {
                    if self
                        .workspaces
                        .get_mut(ws_idx)
                        .tree
                        .collapse_to_root_stack(oversized_wid)
                    {
                        tracing::info!(
                            id = oversized_wid,
                            min_w,
                            min_h,
                            ws = ws_idx + 1,
                            "cold-start overflow: collapsing workspace into root stack"
                        );
                        self.apply_layout();
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    break;
                }

                if self
                    .workspaces
                    .get_mut(ws_idx)
                    .tree
                    .make_stack_for_window(oversized_wid)
                {
                    tracing::info!(
                        id = oversized_wid,
                        min_w,
                        min_h,
                        ws = ws_idx + 1,
                        "stacking oversized subtree locally"
                    );
                    self.workspaces
                        .get_mut(ws_idx)
                        .tree
                        .set_stack_active(oversized_wid);
                    self.apply_layout();
                    std::thread::sleep(std::time::Duration::from_millis(50));
                } else {
                    break;
                }
            }

            processed.push(ws_idx);
        }

        // Safety net: ensure ALL windows on non-visible workspaces are hidden.
        // Uses the same hide_window as switch_workspace (alpha=0 + AX position
        // off-screen). This is the LAST thing that runs, so the AX position
        // command overrides any prior positioning from apply_layout.
        for ws_idx in 0..self.workspaces.count() {
            if self.monitor_showing_workspace(ws_idx).is_some() {
                continue; // Visible workspace — windows should be shown
            }
            let wids = self.workspaces.get(ws_idx).all_window_ids();
            if !wids.is_empty() {
                tracing::info!(
                    ws = ws_idx + 1,
                    count = wids.len(),
                    "safety net hiding non-visible workspace"
                );
            }
            for wid in wids {
                self.hide_window(wid);
                self.log_hidden_window_diagnostics(wid, "post-safety-hide");
            }
        }
    }

    // --- Window operations ---

    pub fn focus_direction(&mut self, direction: super::tree::Direction) {
        use super::tree::Direction;

        let ws = self.active_workspace();
        let focused = ws.focused;
        let sr = self.focused_rect();

        if let Some(from) = focused
            && matches!(direction, Direction::Left | Direction::Right)
            && let Some(next) = self
                .active_workspace_mut()
                .tree
                .cycle_stack(from, matches!(direction, Direction::Right))
        {
            self.focus_window(next);
            return;
        }

        // Try intra-workspace navigation first (only if we have a focused window)
        if let Some(from) = focused {
            let geoms = self.workspace_focus_geometries(self.active_ws_idx(), sr);

            // Check if focused window is at the monitor edge in the requested
            // direction. If so, cross monitors instead of spiraling into the BSP tree.
            let at_edge = if let Some((_, rect)) = geoms.iter().find(|(w, _)| *w == from) {
                let tolerance = self.gap_outer + 2.0;
                match direction {
                    Direction::Right => (rect.x + rect.width) >= (sr.x + sr.width - tolerance),
                    Direction::Left => rect.x <= (sr.x + tolerance),
                    Direction::Down => (rect.y + rect.height) >= (sr.y + sr.height - tolerance),
                    Direction::Up => rect.y <= (sr.y + tolerance),
                }
            } else {
                false
            };

            if !at_edge && let Some(default_target) = Node::find_adjacent(&geoms, from, direction) {
                let candidates = Node::adjacent_candidates(&geoms, from, direction);
                let from_rect = geoms.iter().find(|(w, _)| *w == from).map(|(_, r)| *r);
                let default_rect = geoms
                    .iter()
                    .find(|(w, _)| *w == default_target)
                    .map(|(_, r)| *r);
                let target = self
                    .focus_return_memory
                    .get(&(from, direction))
                    .copied()
                    .filter(|remembered| {
                        if !candidates.contains(remembered) {
                            return false;
                        }
                        // Only honor remembered target when its distance is
                        // close to the default's. Otherwise topology has
                        // changed (e.g., a new window appeared between
                        // source and remembered target) and the memory is
                        // stale.
                        let Some(remembered_rect) = geoms
                            .iter()
                            .find(|(w, _)| *w == *remembered)
                            .map(|(_, r)| *r)
                        else {
                            return false;
                        };
                        let (Some(fr), Some(dr)) = (from_rect, default_rect) else {
                            return true;
                        };
                        let d_default = Node::adjacent_distance(&fr, &dr, direction);
                        let d_remembered =
                            Node::adjacent_distance(&fr, &remembered_rect, direction);
                        // Memory wins when within 32px (a typical gap +
                        // border) of the default's distance — that's the
                        // tie-breaker regime memory is meant for.
                        d_remembered - d_default <= 32.0
                    })
                    .unwrap_or(default_target);
                self.focus_window(target);
                self.remember_focus_transition(from, direction, target, &geoms);
                if self.mouse_follows_focus
                    && let Some((_, rect)) = geoms.iter().find(|(id, _)| *id == target)
                {
                    warp_mouse_to_center(rect);
                    self.ffm_cooldown_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
                    self.ffm_last_window = Some(target);
                }
                return;
            }
        }

        // At monitor edge or no adjacent window — try crossing monitors
        if self.monitors.len() <= 1 {
            return;
        }
        let new_mi = match direction {
            Direction::Right => {
                super::monitor::next_index_nowrap(&self.monitors, self.focused_monitor)
            }
            Direction::Left => {
                super::monitor::prev_index_nowrap(&self.monitors, self.focused_monitor)
            }
            _ => None,
        };
        if let Some(new_mi) = new_mi {
            self.focused_monitor = new_mi;
            let target_sr = self.focused_rect();
            let target_geoms = self.workspace_focus_geometries(self.active_ws_idx(), target_sr);

            if let Some(wid) =
                Node::nearest_to_edge(&target_geoms, direction).or(self.active_workspace().focused)
            {
                // Target has windows — focus the nearest one
                self.focus_window(wid);
                if self.mouse_follows_focus {
                    if let Some((_, rect)) = target_geoms.iter().find(|(id, _)| *id == wid) {
                        warp_mouse_to_center(rect);
                    }
                    self.ffm_cooldown_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
                    self.ffm_last_window = Some(wid);
                }
            } else {
                // Target workspace is empty — deactivate all title bars
                // and warp mouse to monitor center
                crate::platform::application::deactivate_all_windows();
                self.ffm_last_window = None;
                if self.mouse_follows_focus {
                    warp_mouse_to_center(&target_sr);
                    self.ffm_cooldown_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
                }
            }
            tracing::debug!(monitor = new_mi, "crossed to adjacent monitor");
        }
    }

    /// HX-04: remove the focused window from its current tile and
    /// re-insert it at a focal point computed one pixel past the
    /// focused tile's edge in `direction`. Unlike `swap_direction`,
    /// this *relocates* the window — the layout reshapes around the
    /// move rather than trading two positions.
    ///
    /// When the focal point lands outside the workspace rect (focused
    /// window already touches the edge in `direction`), falls through
    /// to `move_window_to_monitor` if `window_direction_monitor_fallback`
    /// is enabled. Otherwise no-ops.
    ///
    /// No-op when the focal point lands inside the focused window's
    /// own tile — happens when the workspace has only one window or
    /// when the focused tile fills the workspace in the relevant axis.
    pub fn move_direction(&mut self, direction: super::tree::Direction) {
        let ws = self.active_workspace();
        let Some(focused) = ws.focused else {
            return;
        };
        let sr = self.focused_rect();
        let ws_idx = self.active_ws_idx();
        let geoms = self.workspace_focus_geometries(ws_idx, sr);
        let Some(src_rect) = geoms
            .iter()
            .find(|(w, _)| *w == focused)
            .map(|(_, r)| *r)
        else {
            return;
        };
        let focal = src_rect.focal_point_for_direction(direction);

        if !sr.contains_point(focal.0, focal.1) {
            // Focal landed off the workspace rect — focused window is
            // at the workspace edge in `direction`. Cross to the
            // adjacent monitor when fallback is enabled.
            if !self.window_direction_monitor_fallback || self.monitors.len() <= 1 {
                return;
            }
            if let Some(target_mi) =
                super::monitor::next_in_direction(&self.monitors, self.focused_monitor, direction)
            {
                tracing::debug!(
                    focused,
                    monitor = target_mi,
                    ?direction,
                    "move: relocating to adjacent monitor"
                );
                self.move_window_to_monitor(target_mi);
            }
            return;
        }

        // Focal landed inside the workspace — find the target leaf and
        // relocate via remove+reinsert.
        let target = self
            .active_workspace()
            .tree
            .find_leaf_at_point(sr, focal.0, focal.1);
        if target == Some(focused) {
            return;
        }
        tracing::debug!(focused, ?target, ?direction, "move: relocating in workspace");
        self.active_workspace_mut().tree.remove(focused);
        self.active_workspace_mut()
            .tree
            .insert_with_rect(focused, target, sr);
        self.prune_focus_memory_for_window(focused);
        self.apply_layout();
        self.fix_oversized_windows();
        self.sync_mouse_after_focus(ws_idx, focused);
    }

    pub fn swap_direction(&mut self, direction: super::tree::Direction) {
        use super::tree::Direction;

        let ws = self.active_workspace();
        let focused = match ws.focused {
            Some(f) => f,
            None => return,
        };
        let sr = self.focused_rect();

        if matches!(direction, Direction::Left | Direction::Right)
            && let Some(next) = self
                .active_workspace_mut()
                .tree
                .reorder_stack(focused, matches!(direction, Direction::Right))
        {
            self.apply_layout();
            self.focus_window(next);
            return;
        }

        let geoms = self.workspace_focus_geometries(self.active_ws_idx(), sr);

        // Check if the focused window touches the monitor edge in the
        // requested direction. If so, skip intra-workspace swap and move
        // to the adjacent monitor. This prevents the BSP spiral from
        // trapping windows at the edge.
        let at_edge = if let Some((_, rect)) = geoms.iter().find(|(w, _)| *w == focused) {
            let tolerance = self.gap_outer + 2.0;
            match direction {
                Direction::Right => (rect.x + rect.width) >= (sr.x + sr.width - tolerance),
                Direction::Left => rect.x <= (sr.x + tolerance),
                Direction::Down => (rect.y + rect.height) >= (sr.y + sr.height - tolerance),
                Direction::Up => rect.y <= (sr.y + tolerance),
            }
        } else {
            false
        };

        if !at_edge && let Some(target) = Node::find_adjacent(&geoms, focused, direction) {
            tracing::debug!(focused, target, ?direction, "swap_direction");
            if self.active_workspace_mut().tree.swap(focused, target) {
                self.apply_layout();
                self.fix_oversized_windows();
                let ws_idx = self.active_ws_idx();
                self.sync_mouse_after_focus(ws_idx, focused);
            }
            return;
        }

        // At monitor edge or no adjacent window — move to the
        // geographically adjacent monitor in `direction`, when the
        // user has enabled cross-monitor fallback (HX-03; default on).
        if !self.window_direction_monitor_fallback || self.monitors.len() <= 1 {
            return;
        }
        let new_mi =
            super::monitor::next_in_direction(&self.monitors, self.focused_monitor, direction);
        if let Some(target_mi) = new_mi {
            tracing::debug!(
                focused,
                monitor = target_mi,
                ?direction,
                "swap: moving to adjacent monitor"
            );
            self.move_window_to_monitor(target_mi);
        }
    }

    pub fn resize_direction(&mut self, direction: super::tree::Direction) {
        let focused = match self.active_workspace().focused {
            Some(f) => f,
            None => return,
        };
        let smart = self.smart_resizing;
        if self
            .active_workspace_mut()
            .tree
            .resize(focused, direction, 0.05, smart)
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

    fn dismiss_special_on_monitor(&mut self, monitor_idx: usize) {
        if monitor_idx >= self.active_specials.len() {
            return;
        }
        let Some(special_idx) = self.active_specials[monitor_idx] else {
            return;
        };
        let wids = self.workspaces.get(special_idx).all_window_ids();
        self.active_specials[monitor_idx] = None;
        self.sync_workspace_visibility();
        for wid in wids {
            self.hide_window(wid);
        }
    }

    fn sync_mouse_after_focus(&mut self, ws_idx: usize, id: WindowId) {
        self.ffm_last_window = Some(id);
        if !self.mouse_follows_focus {
            return;
        }

        let rect = self
            .monitor_showing_workspace(ws_idx)
            .map_or_else(|| self.focused_rect(), |mi| self.monitor_rect(mi));
        let geoms = self.workspace_focus_geometries(ws_idx, rect);
        let target_rect = geoms
            .iter()
            .find(|(wid, _)| *wid == id)
            .map(|(_, r)| *r)
            .unwrap_or(rect);

        // Skip the warp when the cursor is already inside the target
        // rect. This is the user's typical state (mouse-follows-focus
        // already keeps the cursor over the focused window) and warping
        // again only matters when adoption moves focus elsewhere. It
        // also kills any feedback loop where a rapid stream of
        // adopt_external_focus calls would otherwise spam warps and
        // freeze the cursor at the rect's center.
        let (cx, cy) = crate::platform::display::get_cursor_position();
        if target_rect.contains_point(cx, cy) {
            return;
        }

        // Set cooldown BEFORE the warp so the synthetic mouseMoved event
        // posted by warp_mouse arrives at the FFM handler with the
        // cooldown already armed. Otherwise the order is racy: the tap
        // callback can fire on the same runloop spin and slip past an
        // unarmed cooldown.
        self.ffm_cooldown_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
        warp_mouse_to_center(&target_rect);
    }

    fn opposite_direction(direction: super::tree::Direction) -> super::tree::Direction {
        use super::tree::Direction;
        match direction {
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        }
    }

    fn remember_focus_transition(
        &mut self,
        from: WindowId,
        direction: super::tree::Direction,
        to: WindowId,
        geoms: &[(WindowId, super::tree::Rect)],
    ) {
        self.focus_return_memory.insert((from, direction), to);

        // Only memoize the reverse traversal when `from` and `to` are
        // axis-aligned along the navigation direction. A diagonal jump
        // (e.g. Up from a tall left tile that lands on the upper-right
        // tile because nothing is directly above) must not poison the
        // opposite axis's memory, otherwise pressing Down from the
        // upper-right tile would route back to the left column instead
        // of dropping to the lower-right tile.
        let from_rect = geoms.iter().find(|(w, _)| *w == from).map(|(_, r)| r);
        let to_rect = geoms.iter().find(|(w, _)| *w == to).map(|(_, r)| r);
        if let (Some(a), Some(b)) = (from_rect, to_rect)
            && Self::has_perpendicular_overlap(a, b, direction)
        {
            self.focus_return_memory
                .insert((to, Self::opposite_direction(direction)), from);
        }
    }

    fn has_perpendicular_overlap(
        a: &super::tree::Rect,
        b: &super::tree::Rect,
        direction: super::tree::Direction,
    ) -> bool {
        use super::tree::Direction;
        match direction {
            Direction::Left | Direction::Right => {
                a.y.max(b.y) < (a.y + a.height).min(b.y + b.height)
            }
            Direction::Up | Direction::Down => a.x.max(b.x) < (a.x + a.width).min(b.x + b.width),
        }
    }

    fn prune_focus_memory_for_window(&mut self, window: WindowId) {
        self.focus_return_memory
            .retain(|(from, _), to| *from != window && *to != window);
    }

    /// Cap on `recent_internal_focus` ring size. Bounds memory and bounds the
    /// linear scan in `should_ignore_external_focus`. A workspace switch
    /// produces ~1 internal focus per visible window, so 16 is plenty.
    const RECENT_FOCUS_CAPACITY: usize = 16;
    /// How long an entry in `recent_internal_focus` shadows incoming external
    /// focus events. Long enough to cover apply_layout's 50ms sleep plus AX
    /// activation propagation and at least one frontmost-app poll cycle.
    const RECENT_FOCUS_TTL: std::time::Duration = std::time::Duration::from_millis(600);

    fn prune_recent_internal_focus(&mut self) {
        let now = std::time::Instant::now();
        self.recent_internal_focus
            .retain(|(_, _, until)| now < *until);
    }

    fn mark_internal_focus(&mut self, id: WindowId) {
        let Some(window) = self.registry.get(id) else {
            return;
        };
        let pid = window.app_pid;
        self.prune_recent_internal_focus();
        // Coalesce: if the same window is already pending, just refresh its
        // expiry rather than appending a duplicate.
        if let Some(entry) = self
            .recent_internal_focus
            .iter_mut()
            .find(|(eid, epid, _)| *eid == id && *epid == pid)
        {
            entry.2 = std::time::Instant::now() + Self::RECENT_FOCUS_TTL;
            return;
        }
        if self.recent_internal_focus.len() >= Self::RECENT_FOCUS_CAPACITY {
            self.recent_internal_focus.remove(0);
        }
        self.recent_internal_focus.push((
            id,
            pid,
            std::time::Instant::now() + Self::RECENT_FOCUS_TTL,
        ));
    }

    /// How long after a workspace switch to drop external-focus callbacks.
    /// Covers the apply_layout 50ms sleep, AX activation propagation, and
    /// several frontmost-app poll cycles before the OS state settles.
    const WORKSPACE_SWITCH_SILENCE: std::time::Duration = std::time::Duration::from_millis(400);

    fn arm_workspace_switch_silence(&mut self) {
        self.workspace_switch_silence_until =
            Some(std::time::Instant::now() + Self::WORKSPACE_SWITCH_SILENCE);
    }

    fn is_in_workspace_switch_silence(&mut self) -> bool {
        let Some(until) = self.workspace_switch_silence_until else {
            return false;
        };
        if std::time::Instant::now() >= until {
            self.workspace_switch_silence_until = None;
            return false;
        }
        true
    }

    /// HX-08: 500ms rolling window past the last drag event, during
    /// which `adopt_external_focus` drops incoming focus events. Set
    /// by `begin_move_drag` on a tiled hit, refreshed by `update_drag`,
    /// cleared by `end_drag`.
    const DRAG_FOCUS_SUPPRESSION: std::time::Duration = std::time::Duration::from_millis(500);

    fn arm_drag_focus_suppression(&mut self) {
        self.drag_suppress_focus_until =
            Some(std::time::Instant::now() + Self::DRAG_FOCUS_SUPPRESSION);
    }

    fn is_in_drag_focus_suppression(&mut self) -> bool {
        let Some(until) = self.drag_suppress_focus_until else {
            return false;
        };
        if std::time::Instant::now() >= until {
            self.drag_suppress_focus_until = None;
            return false;
        }
        true
    }

    fn should_ignore_external_focus(&mut self, pid: i32, requested: Option<WindowId>) -> bool {
        self.prune_recent_internal_focus();
        if self.recent_internal_focus.is_empty() {
            return false;
        }
        // Ignore the event if either (a) the requested window is one we
        // just internally focused — re-adopting it is at best a no-op — or
        // (b) the event's pid matches an app we just activated. Side-effect
        // AXFocusedWindowChanged notifications fire for *other* windows of
        // the activated app (e.g. activating Ghostty window A on monitor B
        // causes Ghostty to also emit FocusChanged for the previous key
        // window B in a stack on monitor A), and adopting those events
        // perpetually drags focus back into the stack we were trying to
        // mod+arrow out of. The 600ms TTL is short enough that genuine
        // user clicks on a sibling window of the same app within that
        // window are rare; if one does happen we miss it, which is
        // strictly less disruptive than the cross-monitor focus loop.
        if let Some(id) = requested
            && self
                .recent_internal_focus
                .iter()
                .any(|(eid, _, _)| *eid == id)
        {
            return true;
        }
        self.recent_internal_focus
            .iter()
            .any(|(_, epid, _)| *epid == pid)
    }

    fn is_external_focus_candidate(&self, id: WindowId) -> bool {
        self.registry
            .get(id)
            .is_some_and(|window| !window.minimized && self.workspaces.find_window(id).is_some())
    }

    fn resolve_external_focus_target(
        &self,
        pid: i32,
        requested: Option<WindowId>,
    ) -> Option<WindowId> {
        if let Some(id) = requested.filter(|id| self.is_external_focus_candidate(*id)) {
            return Some(id);
        }

        // Prefer windows on currently visible workspaces over hidden ones, and
        // the focused monitor's workspace over other visible ones. Without this
        // bias the resolver iterates by workspace index, so workspace 1 (index
        // 0) acts as a magnet for any cross-workspace external focus event and
        // can yank the WM back across workspaces it just left.
        let active_ws = self
            .monitors
            .get(self.focused_monitor)
            .map(|m| m.active_workspace);

        if let Some(ws_idx) = active_ws
            && let Some(wid) = self.find_pid_target_in_workspace(ws_idx, pid)
        {
            return Some(wid);
        }

        for (ws_idx, _) in self.workspaces.iter().enumerate() {
            if Some(ws_idx) == active_ws {
                continue;
            }
            if self.monitor_showing_workspace(ws_idx).is_none() {
                continue;
            }
            if let Some(wid) = self.find_pid_target_in_workspace(ws_idx, pid) {
                return Some(wid);
            }
        }

        for (ws_idx, _) in self.workspaces.iter().enumerate() {
            if self.monitor_showing_workspace(ws_idx).is_some() {
                continue;
            }
            if let Some(wid) = self.find_pid_target_in_workspace(ws_idx, pid) {
                return Some(wid);
            }
        }

        self.registry
            .all()
            .filter(|window| window.app_pid == pid)
            .map(|window| window.id)
            .find(|id| self.is_external_focus_candidate(*id))
    }

    fn find_pid_target_in_workspace(&self, ws_idx: usize, pid: i32) -> Option<WindowId> {
        self.workspaces
            .get(ws_idx)
            .focus_history
            .iter()
            .rev()
            .find(|&&wid| {
                self.registry
                    .get(wid)
                    .is_some_and(|window| window.app_pid == pid)
                    && self.is_external_focus_candidate(wid)
            })
            .copied()
    }

    fn adopt_external_focus(&mut self, id: WindowId) {
        if self.is_in_drag_focus_suppression() {
            return;
        }
        if self.rate_limit_external_focus(id) {
            return;
        }
        let Some(ws_idx) = self.workspaces.find_window(id) else {
            return;
        };

        let prev_monitor = self.focused_monitor;
        self.focus_window_impl(id, false);

        if let Some(mi) = self.monitor_showing_workspace(ws_idx) {
            if mi < self.active_specials.len() && self.active_specials[mi] != Some(ws_idx) {
                self.dismiss_special_on_monitor(mi);
            }
            self.focused_monitor = mi;
            // Only warp on cross-monitor adoption. Misbehaving apps (e.g.
            // Messages) emit AXFocusedWindowChanged hundreds of times per
            // second under bad geometry; warping on every one freezes the
            // cursor at the focused window's center because each warp
            // races the next AX event. The user didn't request these
            // re-focus events — leave the cursor where they put it.
            if mi != prev_monitor {
                self.sync_mouse_after_focus(ws_idx, id);
            } else {
                self.ffm_last_window = Some(id);
            }
            return;
        }

        let ws_id = self.workspaces.get(ws_idx).id.clone();
        match ws_id {
            super::workspace::WorkspaceId::Special(name) => {
                let current_overlay = self
                    .active_specials
                    .iter()
                    .position(|special| *special == Some(ws_idx));
                if let Some(mi) = current_overlay {
                    self.focused_monitor = mi;
                } else {
                    self.toggle_special(&name);
                }
                self.focus_window_impl(id, false);
            }
            _ => {
                if let Some(target) = ws_id.as_regular_target() {
                    if self.focused_monitor < self.active_specials.len()
                        && self.active_specials[self.focused_monitor].is_some()
                    {
                        self.dismiss_special_on_monitor(self.focused_monitor);
                    }
                    self.switch_workspace(&target);
                    self.sync_mouse_after_focus(ws_idx, id);
                }
            }
        }
    }

    /// Rate-limit external focus adoptions to break self-perpetuating AX
    /// focus storms between misbehaving apps (the disaster of 2026-05-19:
    /// four AXDialog windows ping-ponged focus at 2 kHz, warping the mouse
    /// thousands of times per second across two monitors). Returns true if
    /// the call should be dropped — caller should bail immediately.
    ///
    /// During quiet mode the most recent target is stashed in
    /// `ext_focus_pending_target` and applied later via
    /// `drain_pending_ext_focus` from the polling tick, so the user still
    /// ends up at the correct final focus once the storm subsides.
    fn rate_limit_external_focus(&mut self, id: WindowId) -> bool {
        let now = std::time::Instant::now();

        // Drain history entries that have fallen out of the sliding window.
        while let Some(t) = self.ext_focus_history.front() {
            if now.duration_since(*t) > EXT_FOCUS_STORM_WINDOW {
                self.ext_focus_history.pop_front();
            } else {
                break;
            }
        }

        // Active quiet mode — stash latest target and skip processing.
        if let Some(until) = self.ext_focus_quiet_until {
            if now < until {
                self.ext_focus_pending_target = Some(id);
                return true;
            }
            // Quiet expired between ticks; fall through and let the current
            // event proceed normally. drain_pending_ext_focus may have
            // already applied the pending target — that's fine, this is
            // just the next real event.
            self.ext_focus_quiet_until = None;
            self.ext_focus_pending_target = None;
            self.ext_focus_history.clear();
        }

        self.ext_focus_history.push_back(now);

        if self.ext_focus_history.len() > EXT_FOCUS_STORM_THRESHOLD {
            let count = self.ext_focus_history.len();
            self.ext_focus_quiet_until = Some(now + EXT_FOCUS_QUIET_DURATION);
            self.ext_focus_pending_target = Some(id);
            self.ext_focus_history.clear();
            tracing::warn!(
                count,
                window_ms = EXT_FOCUS_STORM_WINDOW.as_millis() as u64,
                quiet_ms = EXT_FOCUS_QUIET_DURATION.as_millis() as u64,
                target = id,
                "external focus storm detected — entering quiet mode"
            );
            return true;
        }

        false
    }

    /// Called from the polling tick. If quiet mode has expired and we
    /// stashed a target during the storm, apply it now so focus settles on
    /// the latest AX-reported window.
    pub fn drain_pending_ext_focus(&mut self) {
        let Some(until) = self.ext_focus_quiet_until else {
            return;
        };
        if std::time::Instant::now() < until {
            return;
        }
        self.ext_focus_quiet_until = None;
        self.ext_focus_history.clear();
        if let Some(target) = self.ext_focus_pending_target.take() {
            tracing::debug!(target, "ext focus quiet expired — applying deferred target");
            // Re-enter adoption; rate_limit_external_focus will see a fresh
            // history and proceed normally. If the storm is ongoing it'll
            // re-arm quiet on the very next AX event.
            self.adopt_external_focus(target);
        }
    }

    pub fn adopt_external_app_focus(&mut self, pid: i32, requested: Option<WindowId>) {
        if self.is_in_workspace_switch_silence() {
            return;
        }
        if self.should_ignore_external_focus(pid, requested) {
            return;
        }
        let Some(target) = self.resolve_external_focus_target(pid, requested) else {
            return;
        };
        self.adopt_external_focus(target);
    }

    fn focus_window_impl(&mut self, id: WindowId, activate_app: bool) {
        if activate_app {
            self.mark_internal_focus(id);
        }
        if let Some(ax_ref) = self.ax_refs.get(&id) {
            if activate_app {
                // Full activation sequence for reliable cross-monitor focus:
                // 1. Set AXFrontmost on the app-level AX element
                // 2. NSRunningApplication.activate (brings app to foreground)
                // 3. AXRaise (brings window to front of app's stack)
                // 4. Set AXMain on window (makes it the key window)
                // 5. Set AXFocused on window (tells AX this is focused)
                //
                // Steps 1+2 are skipped when the target's app is already
                // frontmost: NSRunningApplication.activate reshuffles every
                // window of the app globally (a documented macOS quirk),
                // which jarringly reorders stack siblings on monitors the
                // user isn't even interacting with — e.g. focusing one
                // Ghostty on monitor B flips which Ghostty is on top of a
                // stack on monitor A. AXRaise on the specific target is
                // per-window and doesn't shuffle siblings, so it's safe to
                // keep.
                let target_pid = self.registry.get(id).map(|w| w.app_pid);
                let app_already_frontmost = target_pid.is_some_and(|pid| {
                    crate::platform::workspace_observer::frontmost_regular_application_pid()
                        == Some(pid)
                });
                if !app_already_frontmost && let Some(pid) = target_pid {
                    let app_ref =
                        unsafe { objc2_application_services::AXUIElement::new_application(pid) };
                    let frontmost_key =
                        objc2_core_foundation::CFString::from_static_str("AXFrontmost");
                    let _ =
                        crate::platform::accessibility::ax_set_bool(&app_ref, &frontmost_key, true);
                    crate::platform::application::activate_app(pid);
                }
                let _ = ax_perform_action(ax_ref, "AXRaise");
                let main_key = objc2_core_foundation::CFString::from_static_str("AXMain");
                let _ = crate::platform::accessibility::ax_set_bool(ax_ref, &main_key, true);
                let focused_key = objc2_core_foundation::CFString::from_static_str("AXFocused");
                let _ = crate::platform::accessibility::ax_set_bool(ax_ref, &focused_key, true);
            } else {
                // Soft focus -- update internal tracking only, don't touch macOS.
            }
        }
        // Record focus on the workspace that CONTAINS this window,
        // not the active workspace — during cross-monitor FFM the active
        // workspace might be different from the window's workspace.
        let (old_focused, stack_focus_changed) =
            if let Some(ws_idx) = self.workspaces.find_window(id) {
                let old = self.workspaces.get(ws_idx).focused;
                self.workspaces.get_mut(ws_idx).raise_floating(id);
                let stack_changed = self.workspaces.get_mut(ws_idx).tree.set_stack_active(id);
                self.workspaces.get_mut(ws_idx).record_focus(id);
                (old, stack_changed)
            } else {
                let old = self.active_workspace().focused;
                self.active_workspace_mut().raise_floating(id);
                let stack_changed = self.active_workspace_mut().tree.set_stack_active(id);
                self.active_workspace_mut().record_focus(id);
                (old, stack_changed)
            };
        if stack_focus_changed {
            self.apply_layout();
        }
        self.enforce_floating_levels(id);

        // Update border colors on focus change
        if self.borders.is_enabled() && old_focused != Some(id) {
            // Get rects for old and new focused windows from registry
            let old_rect = old_focused.and_then(|oid| {
                self.registry
                    .get(oid)
                    .map(|w| Rect::new(w.x, w.y, w.width, w.height))
            });
            let new_rect = self
                .registry
                .get(id)
                .map(|w| Rect::new(w.x, w.y, w.width, w.height));
            if let Some(rect) = old_rect
                && let Some(oid) = old_focused
            {
                self.borders.update_border(oid, rect, false);
            }
            if let Some(rect) = new_rect {
                self.borders.update_border(id, rect, true);
            }
        }

        tracing::debug!(id, activate_app, "focused window");
    }

    // --- Drag operations for floating windows ---

    /// Start a move drag on a floating window (Cmd+LeftClick).
    pub fn begin_move_drag(&mut self, x: f64, y: f64) {
        // Floating-window drag (existing behavior): cursor hits a
        // floating tile → start an immediate-AX drag.
        let floating_hit = self
            .active_workspace()
            .floating
            .iter()
            .rev()
            .find(|fw| fw.geometry.contains_point(x, y))
            .map(|fw| (fw.id, fw.geometry));
        if let Some((id, geometry)) = floating_hit {
            let ws_idx = self.active_ws_idx();
            self.drag = Some(DragState {
                window_id: id,
                mode: DragMode::Move,
                start_mouse: (x, y),
                start_geometry: geometry,
                original_workspace: ws_idx,
                original_tile_rect: geometry,
            });
            self.drag_threshold_active = false;
            tracing::debug!(id, "move drag started (floating)");
            return;
        }
        // HX-08: tiled-window drag scaffolding. Check the tiled
        // geometries at the cursor; if found, stage drag state with
        // `drag_threshold_active = true` and arm focus suppression.
        // HX-09 wires the threshold-cross transition to float and the
        // drop disposition on end_drag.
        let sr = self.focused_rect();
        let ws_idx = self.active_ws_idx();
        let geoms = self.workspace_focus_geometries(ws_idx, sr);
        let tiled_hit = geoms
            .into_iter()
            .find(|(_, rect)| rect.contains_point(x, y));
        if let Some((id, geometry)) = tiled_hit {
            self.drag = Some(DragState {
                window_id: id,
                mode: DragMode::Move,
                start_mouse: (x, y),
                start_geometry: geometry,
                original_workspace: ws_idx,
                original_tile_rect: geometry,
            });
            self.drag_threshold_active = true;
            self.arm_drag_focus_suppression();
            tracing::debug!(
                id,
                threshold = self.drag_threshold,
                "move drag staged (tiled pending threshold)"
            );
        }
    }

    /// Start a resize drag on a floating window (Cmd+RightClick).
    pub fn begin_resize_drag(&mut self, x: f64, y: f64) {
        let hit = self
            .active_workspace()
            .floating
            .iter()
            .rev()
            .find(|fw| fw.geometry.contains_point(x, y))
            .map(|fw| (fw.id, fw.geometry));
        if let Some((id, geometry)) = hit {
            let ws_idx = self.active_ws_idx();
            self.drag = Some(DragState {
                window_id: id,
                mode: DragMode::Resize,
                start_mouse: (x, y),
                start_geometry: geometry,
                original_workspace: ws_idx,
                original_tile_rect: geometry,
            });
            tracing::debug!(id, "resize drag started");
        }
    }

    /// HX-09: convert a tiled window into a floating one for the drag
    /// follow-through. The window leaves the source workspace's BSP
    /// tree, gets added to the same workspace's floating list at a
    /// cursor-centered position scaled to 84.89% of its source tile
    /// (Hyprland's magic number), and `apply_layout` re-stretches the
    /// siblings that took over the freed tile.
    ///
    /// Called from `update_drag` the moment the cursor crosses
    /// `drag_threshold`. Refreshes `drag_suppress_focus_until` so any
    /// AX focus storms from the source app during the float don't
    /// interrupt the drag.
    fn float_window_for_drag(&mut self, drag: &mut DragState, cursor: (f64, f64)) {
        let wid = drag.window_id;
        let original_w = drag.original_tile_rect.width;
        let original_h = drag.original_tile_rect.height;
        // 0.8489 mirrors Hyprland's `position().size() * 0.8489` — small
        // enough to indicate "now floating," large enough to remain
        // usable. Clamped to a minimum so tiny tiles don't shrink past
        // usability.
        let new_w = (original_w * 0.8489).max(200.0);
        let new_h = (original_h * 0.8489).max(150.0);
        let new_x = cursor.0 - new_w / 2.0;
        let new_y = cursor.1 - new_h / 2.0;
        let new_geom = super::tree::Rect::new(new_x, new_y, new_w, new_h);

        // Pull the window out of the source workspace's tree and add
        // to its floating list.
        let source_ws = drag.original_workspace;
        let ws = self.workspaces.get_mut(source_ws);
        ws.tree.remove(wid);
        ws.floating.push(super::workspace::FloatingWindow {
            id: wid,
            geometry: new_geom,
        });

        // The DragState's geometry is referenced by update_drag to
        // derive new positions from cursor deltas. Anchor it to the
        // new float position so subsequent updates compute against
        // post-float coordinates.
        drag.start_mouse = cursor;
        drag.start_geometry = new_geom;
        drag.mode = DragMode::TiledRelocate;

        self.arm_drag_focus_suppression();
        self.apply_layout();
        tracing::info!(
            wid,
            source_ws,
            new_w,
            new_h,
            "drag: floated tiled window for relocate"
        );
    }

    /// Update an active drag operation.
    pub fn update_drag(&mut self, x: f64, y: f64) {
        let Some(drag) = self.drag else { return };

        let dx = x - drag.start_mouse.0;
        let dy = y - drag.start_mouse.1;

        // HX-08: tiled-pending drag — dormant until the cursor crosses
        // `drag_threshold`. Below threshold = no AX writes, no float.
        // Above = clear the flag and (in HX-09) call the float-on-drag
        // transition. For HX-08 we just log so the infrastructure is
        // verifiable in tracing.
        if self.drag_threshold_active {
            let delta = (dx * dx + dy * dy).sqrt();
            if delta < self.drag_threshold {
                self.arm_drag_focus_suppression();
                return;
            }
            self.drag_threshold_active = false;
            tracing::debug!(
                id = drag.window_id,
                delta,
                threshold = self.drag_threshold,
                "drag threshold crossed → floating for relocate"
            );
            // HX-09: float the tiled window, anchor the drag at the
            // current cursor, switch mode to TiledRelocate. The
            // helper writes back the mutated DragState below.
            let mut new_drag = drag;
            self.float_window_for_drag(&mut new_drag, (x, y));
            self.drag = Some(new_drag);
            return;
        }

        // Refresh focus suppression while a tiled drag remains active
        // (HX-09 will trigger this branch once the threshold transition
        // is wired). For a floating drag this is harmless.
        if self.drag_suppress_focus_until.is_some() {
            self.arm_drag_focus_suppression();
        }

        match drag.mode {
            // HX-09 TiledRelocate shares the move loop with regular
            // floating-window drag: the window was floated by
            // float_window_for_drag when the threshold crossed and now
            // lives in `floating`, so the position-update logic is the
            // same. end_drag is where the disposition diverges.
            DragMode::Move | DragMode::TiledRelocate => {
                let new_x = drag.start_geometry.x + dx;
                let new_y = drag.start_geometry.y + dy;

                // Update floating geometry across all workspaces — for
                // TiledRelocate the window might be on a workspace
                // that's not the active one if the user dragged it
                // across monitors.
                let ws_count = self.workspaces.count();
                for idx in 0..ws_count {
                    let ws = self.workspaces.get_mut(idx);
                    if let Some(fw) = ws.floating.iter_mut().find(|f| f.id == drag.window_id) {
                        fw.geometry.x = new_x;
                        fw.geometry.y = new_y;
                    }
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

    /// End an active drag operation. `x`/`y` is the cursor at release —
    /// used by HX-09's TiledRelocate disposition to choose between
    /// retile, keep-floating, and cross-monitor relocate.
    pub fn end_drag(&mut self, x: f64, y: f64) {
        let Some(drag) = self.drag.take() else { return };

        // HX-08: clear the threshold flag and focus suppression on
        // release whether or not the threshold was ever crossed.
        self.drag_threshold_active = false;
        self.drag_suppress_focus_until = None;

        if drag.mode != DragMode::TiledRelocate {
            tracing::debug!(id = drag.window_id, ?drag.mode, "drag ended");
            return;
        }

        // HX-09 disposition cascade. The dragged window currently lives
        // in `original_workspace.floating`. Find the topmost visible
        // tile under the drop coordinate (excluding the dragged window
        // itself) and retile there. Otherwise keep it floating.
        let drop = (x, y);
        let mut target: Option<(usize, super::tree::Rect)> = None;
        for mi in 0..self.monitors.len() {
            let ws_idx = self.monitors[mi].active_workspace;
            let sr = self.monitor_rect(mi);
            if !sr.contains_point(drop.0, drop.1) {
                continue;
            }
            let geoms = self.workspace_focus_geometries(ws_idx, sr);
            let hit = geoms
                .into_iter()
                .find(|(wid, rect)| *wid != drag.window_id && rect.contains_point(drop.0, drop.1));
            if hit.is_some() {
                target = Some((ws_idx, sr));
                break;
            }
            // Drop was over the monitor but no tile (e.g. menubar gap or
            // workspace background). Don't break — let other monitors
            // also get a hit-test; but if no other matches, we'll fall
            // through to keep-floating.
            if target.is_none() {
                target = Some((ws_idx, sr));
            }
        }

        let wid = drag.window_id;
        let source_ws = drag.original_workspace;

        // Pull from the source floating list. Drop disposition decides
        // what happens next.
        let removed = {
            let ws = self.workspaces.get_mut(source_ws);
            let idx = ws.floating.iter().position(|f| f.id == wid);
            idx.map(|i| ws.floating.remove(i))
        };
        if removed.is_none() {
            tracing::warn!(
                wid,
                source_ws,
                "drag end: floated window missing from source floating list"
            );
            return;
        }

        match target {
            Some((target_ws, target_rect))
                if self
                    .workspace_focus_geometries(target_ws, target_rect)
                    .iter()
                    .any(|(other, rect)| *other != wid && rect.contains_point(drop.0, drop.1)) =>
            {
                // Tile hit — retile at the drop focal point.
                let split_override = self.split_axis_override();
                let multiplier = self.split_width_multiplier;
                let force_split_left = self.force_split == "left";
                let ws = self.workspaces.get_mut(target_ws);
                ws.tree.insert_at_focal_point_options(
                    wid,
                    drop,
                    target_rect,
                    split_override,
                    multiplier,
                );
                if force_split_left {
                    ws.tree.swap_parent_children(wid);
                }
                ws.record_focus(wid);
                self.prune_focus_memory_for_window(wid);
                self.apply_layout();
                self.fix_oversized_windows();
                self.focus_window(wid);
                self.sync_mouse_after_focus(target_ws, wid);
                tracing::info!(
                    wid,
                    target_ws,
                    drop_x = drop.0,
                    drop_y = drop.1,
                    "drag drop: retiled at focal point"
                );
            }
            _ => {
                // Keep floating: put it back on the source workspace's
                // floating list at its drop position.
                let fw = removed.unwrap();
                let kept = super::workspace::FloatingWindow {
                    id: wid,
                    geometry: super::tree::Rect::new(
                        fw.geometry.x,
                        fw.geometry.y,
                        fw.geometry.width,
                        fw.geometry.height,
                    ),
                };
                self.workspaces.get_mut(source_ws).floating.push(kept);
                tracing::info!(
                    wid,
                    drop_x = drop.0,
                    drop_y = drop.1,
                    "drag drop: no tile under cursor, keeping floating"
                );
            }
        }
    }

    /// Check if a drag is currently active.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Re-apply SkyLight window levels for all floating windows.
    /// Called after every focus change since app activation can reset ordering.
    fn enforce_floating_levels(&self, focused_id: WindowId) {
        if let Some(ws_idx) = self.workspaces.find_window(focused_id) {
            self.restack_floating_windows(ws_idx);
        } else {
            self.restack_floating_windows(self.active_ws_idx());
        }
    }

    fn restack_floating_windows(&self, ws_idx: usize) {
        use crate::platform::skylight::{K_CG_FLOATING_WINDOW_LEVEL, set_window_level};

        let ws = self.workspaces.get(ws_idx);
        for fw in &ws.floating {
            set_window_level(fw.id, K_CG_FLOATING_WINDOW_LEVEL);
            if let Some(ax_ref) = self.ax_refs.get(&fw.id) {
                let _ = ax_perform_action(ax_ref, "AXRaise");
            }
        }
    }

    pub fn close_focused(&mut self) {
        let focused = match self.effective_focused() {
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
        // HX-05: cache last cursor for new-window focal-point insertion,
        // unconditionally — the focal-point machinery uses this even
        // when FFM is disabled.
        self.last_cursor = Some((x, y));

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

        // Detect monitor change — update focused_monitor if cursor moved
        // to a different display so FFM checks the correct workspace.
        // Use exact hit test first, fall back to nearest monitor by x distance
        // (handles cases where cursor is at a Y position outside a shorter monitor).
        let detected_mi = super::monitor::index_at_point(&self.monitors, x, y).or_else(|| {
            // Nearest monitor by x distance
            self.monitors
                .iter()
                .enumerate()
                .min_by_key(|(_, m)| {
                    let cx = m.frame.x + m.frame.width / 2.0;
                    ((x - cx).abs() * 1000.0) as i64
                })
                .map(|(i, _)| i)
        });
        if let Some(mi) = detected_mi
            && mi != self.focused_monitor
        {
            // Deactivate the previous monitor's focused window so its title
            // bar goes inactive. Without this, macOS keeps the old window's
            // title bar highlighted even after we activate a window on the
            // new monitor (especially when both are the same app, e.g. WezTerm).
            let old_ws_idx = self.monitors[self.focused_monitor].active_workspace;
            if let Some(old_wid) = self.workspaces.get(old_ws_idx).focused
                && let Some(ax_ref) = self.ax_refs.get(&old_wid)
            {
                let main_key = objc2_core_foundation::CFString::from_static_str("AXMain");
                let _ = crate::platform::accessibility::ax_set_bool(ax_ref, &main_key, false);
            }

            self.focused_monitor = mi;
            self.ffm_last_window = None;
            tracing::debug!(monitor = mi, "FFM detected monitor change");

            // If the new monitor's workspace is empty, deactivate all
            // title bars so no window appears focused.
            if self.active_workspace().tree.window_count() == 0
                && self.active_workspace().floating.is_empty()
            {
                crate::platform::application::deactivate_all_windows();
                return;
            }
        }

        // If a special workspace is active on this monitor, FFM only
        // interacts with special workspace windows — the underlying
        // workspace is visually present but should not capture focus.
        let mi = self.focused_monitor;
        let special_ws_idx = if mi < self.active_specials.len() {
            self.active_specials[mi]
        } else {
            None
        };

        let window_under = if let Some(special_idx) = special_ws_idx {
            let sr = self.monitor_rect(mi);
            let ws = self.workspaces.get(special_idx);
            let (gap_inner, gap_outer) = self.effective_gaps(special_idx);

            // Look up config for this special workspace's overlay rect
            let special_name = match &ws.id {
                super::workspace::WorkspaceId::Special(name) => name.clone(),
                _ => String::new(),
            };
            let cfg = self
                .special_configs
                .iter()
                .find(|c| c.name == special_name)
                .cloned()
                .unwrap_or_else(|| {
                    crate::config::lua::SpecialWorkspaceConfig::default_for(&special_name)
                });

            let overlay_w = sr.width * cfg.width;
            let overlay_h = sr.height * cfg.height;
            let (overlay_x, overlay_y) = match cfg.position.as_str() {
                "top" => (sr.x + (sr.width - overlay_w) / 2.0, sr.y),
                "bottom" => (
                    sr.x + (sr.width - overlay_w) / 2.0,
                    sr.y + sr.height - overlay_h,
                ),
                _ => (
                    sr.x + (sr.width - overlay_w) / 2.0,
                    sr.y + (sr.height - overlay_h) / 2.0,
                ),
            };
            let overlay_rect = Rect::new(overlay_x, overlay_y, overlay_w, overlay_h);

            // Check floating windows on special workspace
            let floating_hit = ws
                .floating
                .iter()
                .rev()
                .find(|fw| fw.geometry.contains_point(x, y))
                .map(|fw| fw.id);

            if floating_hit.is_some() {
                floating_hit
            } else {
                // Use focus geometry here so inactive stack slivers don't
                // steal focus on hover.
                let geoms = ws.tree.calculate_focus_geometries_with_gaps(
                    overlay_rect,
                    gap_inner,
                    gap_outer,
                    true,
                );
                geoms
                    .iter()
                    .rev()
                    .find(|(_, rect)| rect.contains_point(x, y))
                    .map(|(id, _)| *id)
            }
        } else {
            let ws = self.active_workspace();

            // Check floating windows first -- they're visually on top
            let floating_under = ws
                .floating
                .iter()
                .rev()
                .find(|fw| fw.geometry.contains_point(x, y))
                .map(|fw| fw.id);

            if floating_under.is_some() {
                floating_under
            } else {
                // Use focus geometry here so inactive stack slivers don't
                // steal focus on hover.
                let geoms =
                    self.workspace_focus_geometries(self.active_ws_idx(), self.focused_rect());
                geoms
                    .iter()
                    .rev()
                    .find(|(_, rect)| rect.contains_point(x, y))
                    .map(|(id, _)| *id)
            }
        };

        // Refocus when window under cursor changes.
        if window_under != self.ffm_last_window {
            self.ffm_last_window = window_under;
            if let Some(id) = window_under {
                self.focus_window(id);
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
                self.enforce_floating_levels(focused);
                tracing::info!(id = focused, "window floated (level=floating)");
            } else {
                // Restore to normal window level
                use crate::platform::skylight::{K_CG_NORMAL_WINDOW_LEVEL, set_window_level};
                set_window_level(focused, K_CG_NORMAL_WINDOW_LEVEL);
                tracing::info!(id = focused, "window tiled (level=normal)");
            }
        }
    }

    pub fn unstack_focused(&mut self) {
        let focused = match self.effective_focused() {
            Some(f) => f,
            None => return,
        };

        let Some(ws_idx) = self.workspaces.find_window(focused) else {
            return;
        };

        if self.workspaces.get_mut(ws_idx).tree.unstack(focused) {
            self.apply_layout();
            self.focus_window(focused);
            tracing::info!(id = focused, "restored stacked subtree");
        }
    }

    pub fn promote_stack_focused(&mut self) {
        let focused = match self.effective_focused() {
            Some(f) => f,
            None => return,
        };

        let Some(ws_idx) = self.workspaces.find_window(focused) else {
            return;
        };

        if self
            .workspaces
            .get_mut(ws_idx)
            .tree
            .promote_stack_window(focused)
        {
            self.apply_layout();
            self.focus_window(focused);
            tracing::info!(id = focused, "promoted focused stack window to top");
        }
    }

    pub fn toggle_split(&mut self) {
        let focused = match self.effective_focused() {
            Some(f) => f,
            None => return,
        };
        let Some(ws_idx) = self.workspaces.find_window(focused) else {
            return;
        };
        if self
            .workspaces
            .get_mut(ws_idx)
            .tree
            .toggle_parent_split(focused)
        {
            self.apply_layout();
            self.focus_window(focused);
            tracing::info!(id = focused, "toggled parent split direction");
        }
    }

    pub fn swap_split(&mut self) {
        let focused = match self.effective_focused() {
            Some(f) => f,
            None => return,
        };
        let Some(ws_idx) = self.workspaces.find_window(focused) else {
            return;
        };
        if self
            .workspaces
            .get_mut(ws_idx)
            .tree
            .swap_parent_children(focused)
        {
            self.apply_layout();
            self.focus_window(focused);
            tracing::info!(id = focused, "swapped parent split children");
        }
    }

    pub fn promote_to_root(&mut self) {
        let focused = match self.effective_focused() {
            Some(f) => f,
            None => return,
        };
        let Some(ws_idx) = self.workspaces.find_window(focused) else {
            return;
        };
        if self
            .workspaces
            .get_mut(ws_idx)
            .tree
            .promote_to_root(focused)
        {
            // Tree shape changed; existing focus_return_memory entries that
            // referenced the promoted window's old neighborhood are now
            // stale.
            self.prune_focus_memory_for_window(focused);
            self.apply_layout();
            self.focus_window(focused);
            let ws_idx = self.active_ws_idx();
            self.sync_mouse_after_focus(ws_idx, focused);
            tracing::info!(id = focused, "promoted focused subtree to workspace root");
        }
    }

    /// HX-07: merge the focused window into the adjacent leaf in
    /// `direction`, creating a Stack at the adjacent position. The
    /// focused window becomes the active stack member. No-op if no
    /// adjacent window exists in that direction.
    ///
    /// If the adjacent slot already holds a Stack, the focused window
    /// is appended to that stack rather than nesting a new stack.
    pub fn add_to_stack(&mut self, direction: super::tree::Direction) {
        let Some(focused) = self.effective_focused() else {
            return;
        };
        let sr = self.focused_rect();
        let ws_idx = self.active_ws_idx();
        let geoms = self.workspace_focus_geometries(ws_idx, sr);
        let Some(adjacent) = Node::find_adjacent(&geoms, focused, direction) else {
            return;
        };
        if !self
            .workspaces
            .get_mut(ws_idx)
            .tree
            .merge_into_neighbor(focused, adjacent)
        {
            return;
        }
        tracing::info!(
            focused,
            adjacent,
            ?direction,
            "add_to_stack: merged focused into adjacent"
        );
        self.apply_layout();
        self.focus_window(focused);
        self.sync_mouse_after_focus(ws_idx, focused);
    }

    /// HX-07: pull the focused window out of its containing Stack and
    /// re-graft it as a side-by-side sibling of the smaller stack.
    /// Surgical contrast to `Unstack`, which restores the entire
    /// pre-stack tree. No-op when the focused window is not in a Stack.
    pub fn split_from_stack(&mut self) {
        let Some(focused) = self.effective_focused() else {
            return;
        };
        let Some(ws_idx) = self.workspaces.find_window(focused) else {
            return;
        };
        if self
            .workspaces
            .get(ws_idx)
            .tree
            .stack_info(focused)
            .is_none()
        {
            return;
        }
        if !self
            .workspaces
            .get_mut(ws_idx)
            .tree
            .split_active_from_stack(focused)
        {
            return;
        }
        tracing::info!(focused, "split_from_stack: pulled out of stack");
        self.apply_layout();
        self.focus_window(focused);
        self.sync_mouse_after_focus(ws_idx, focused);
    }

    /// HX-08 AX write rate limiter. Returns true if the call site
    /// should *throttle* (skip the AX write) because this window was
    /// written to within the last 16ms (~60 Hz). When it returns false
    /// the timestamp is updated, so a subsequent rapid call within the
    /// same 16ms window throttles. Uses interior mutability so
    /// `show_window` can call this with `&self`.
    ///
    /// Per-window granularity (not per-app): the bug pattern is rapid
    /// updates to a single window (drag cursor-follow, resize loop).
    /// Multi-window operations (e.g., a workspace switch firing writes
    /// to N distinct windows) sail through because each window has its
    /// own bucket.
    fn should_throttle_ax_write(&self, wid: WindowId) -> bool {
        const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);
        let now = std::time::Instant::now();
        let mut last = self.ax_write_last.borrow_mut();
        if let Some(&t) = last.get(&wid)
            && now.duration_since(t) < MIN_INTERVAL
        {
            return true;
        }
        last.insert(wid, now);
        false
    }

    /// HX-08: drain writes that were throttled in earlier frames. Run
    /// at the top of `apply_layout` so deferred writes catch up before
    /// the main layout pass issues fresh ones. A still-throttled retry
    /// just re-defers itself — eventual consistency, bounded by the
    /// per-window 16ms timer.
    fn drain_pending_show_window(&self) {
        let pending: Vec<(WindowId, Rect)> =
            self.pending_show_window.borrow_mut().drain().collect();
        for (wid, rect) in pending {
            self.show_window(wid, rect);
        }
    }

    /// HX-06: query whether a window is in pseudo-tile mode.
    pub fn is_pseudo(&self, wid: WindowId) -> bool {
        self.pseudo_windows.contains(&wid)
    }

    /// HX-06: toggle pseudo-tile mode on the focused window. Pseudo
    /// windows occupy a slot in the BSP tree but `show_window` skips
    /// the AX size write, leaving them at their preferred size centered
    /// in the assigned tile rect.
    ///
    /// v1 limitation: toggling pseudo on a stack member logs a warning
    /// and no-ops. Combined pseudo + stack rendering is deferred.
    pub fn toggle_pseudo(&mut self) {
        let Some(focused) = self.effective_focused() else {
            return;
        };
        // If focused is in a stack, warn and skip — stack rendering is
        // a separate code path and combined pseudo + stack semantics
        // aren't defined in v1.
        if let Some(ws_idx) = self.workspaces.find_window(focused)
            && self
                .workspaces
                .get(ws_idx)
                .tree
                .stack_info(focused)
                .is_some()
        {
            tracing::warn!(
                id = focused,
                "toggle_pseudo no-op for stack member (v1 limitation)"
            );
            return;
        }
        let now_pseudo = if self.pseudo_windows.contains(&focused) {
            self.pseudo_windows.remove(&focused);
            false
        } else {
            self.pseudo_windows.insert(focused);
            true
        };
        tracing::info!(id = focused, pseudo = now_pseudo, "toggle_pseudo");
        self.apply_layout();
    }

    /// HX-05: record a directional preselect hint for the active
    /// workspace. The next window opened in this workspace will use
    /// the hint to derive its focal point. Hint persistence matches
    /// `permanent_direction_override` — when true the hint survives
    /// multiple inserts; when false it clears after one use.
    pub fn preselect(&mut self, direction: super::tree::Direction) {
        let persistent = self.permanent_direction_override;
        self.active_workspace_mut().preselect = Some((direction, persistent));
        tracing::debug!(?direction, persistent, "preselect set");
    }

    pub fn set_ratio(&mut self, value: f64) {
        let focused = match self.effective_focused() {
            Some(f) => f,
            None => return,
        };
        let Some(ws_idx) = self.workspaces.find_window(focused) else {
            return;
        };
        if self
            .workspaces
            .get_mut(ws_idx)
            .tree
            .set_parent_ratio(focused, value as f32)
        {
            self.apply_layout();
            tracing::info!(id = focused, ratio = value, "set parent split ratio");
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
            let geoms = self.workspace_render_geometries(self.active_ws_idx(), self.focused_rect());
            geoms
                .iter()
                .rev()
                .find(|(_, rect)| rect.contains_point(x, y))
                .map(|(id, _)| *id)
        };

        if let Some(id) = id {
            // Sync FFM tracker so mouse_moved doesn't re-trigger on the same window
            self.ffm_last_window = Some(id);
            if self.active_workspace().focused != Some(id) {
                self.focus_window(id);
            }
        }
    }

    // --- Workspace operations ---

    pub fn switch_workspace(&mut self, target: &WorkspaceTarget) {
        let target_idx = self.workspace_index_for_target(target);
        let current_idx = self.monitors[self.focused_monitor].active_workspace;

        if target_idx == current_idx {
            return;
        }

        tracing::info!(from = current_idx + 1, to = %target, "switching workspace");

        // Drop external-focus callbacks for a beat. While AX activations
        // propagate and apply_layout settles, NSWorkspace.frontmostApplication
        // can flip back through the previously-focused app and the 50ms poll
        // would otherwise interpret it as an external focus event and yank us
        // back across workspaces.
        self.arm_workspace_switch_silence();

        // Case 1: Target workspace is already visible on some monitor → jump focus
        if let Some(other_mi) = self.monitor_showing_workspace(target_idx) {
            self.focused_monitor = other_mi;
            self.ffm_last_window = None; // Reset FFM state for new workspace
            if let Some(wid) = self.workspaces.get(target_idx).focused {
                self.focus_window(wid);
                // Warp to the focused WINDOW center (not monitor center)
                if self.mouse_follows_focus {
                    let sr = self.focused_rect();
                    let geoms = self.workspace_focus_geometries(target_idx, sr);
                    if let Some((_, rect)) = geoms.iter().find(|(id, _)| *id == wid) {
                        warp_mouse_to_center(rect);
                    } else {
                        warp_mouse_to_center(&sr);
                    }
                    self.ffm_cooldown_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
                    self.ffm_last_window = Some(wid);
                }
            } else if self.mouse_follows_focus {
                let rect = self.focused_rect();
                warp_mouse_to_center(&rect);
                self.ffm_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
            }
            tracing::info!(workspace = %target, monitor = other_mi, "jumped to visible workspace");
            return;
        }

        // Case 2: Target has windows and remembers a different monitor → jump there
        let target_has_windows = !self.workspaces.get(target_idx).is_empty();
        let target_last_monitor = self.workspaces.get(target_idx).last_monitor;
        let show_on_monitor =
            if let Some(preferred) = self.preferred_monitor_for_workspace(target_idx) {
                preferred
            } else if target_has_windows
                && let Some(mi) = target_last_monitor
                && mi < self.monitors.len()
                && mi != self.focused_monitor
            {
                mi // Jump to the remembered monitor
            } else {
                self.focused_monitor // Show on current monitor
            };

        // Hide the workspace currently on the target monitor
        let displaced_idx = self.monitors[show_on_monitor].active_workspace;
        self.hide_workspace_windows(displaced_idx, show_on_monitor);
        // If the displaced workspace is empty, unassign it from the monitor
        if self.workspaces.get(displaced_idx).is_empty() {
            self.workspaces.get_mut(displaced_idx).last_monitor = None;
            tracing::debug!(ws = displaced_idx + 1, "empty workspace unassigned");
        }

        // Show target workspace on the chosen monitor
        self.monitors[show_on_monitor].active_workspace = target_idx;
        self.workspaces.get_mut(target_idx).last_monitor = Some(show_on_monitor);
        self.workspaces.get_mut(target_idx).last_display_id =
            Some(self.monitors[show_on_monitor].id);
        self.focused_monitor = show_on_monitor;
        self.sync_workspace_visibility();

        // Apply layout with double-apply for cross-monitor moves
        self.apply_layout();
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.apply_layout();
        self.fix_oversized_windows();
        self.ffm_last_window = None; // Reset FFM state for new workspace
        if let Some(wid) = self.workspaces.get(target_idx).focused {
            self.focus_window(wid);
            if self.mouse_follows_focus {
                let sr = self.focused_rect();
                let geoms = self.workspace_focus_geometries(target_idx, sr);
                if let Some((_, rect)) = geoms.iter().find(|(id, _)| *id == wid) {
                    warp_mouse_to_center(rect);
                } else {
                    warp_mouse_to_center(&sr);
                }
                self.ffm_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
                self.ffm_last_window = Some(wid);
            }
        } else if self.mouse_follows_focus {
            let rect = self.focused_rect();
            warp_mouse_to_center(&rect);
            self.ffm_cooldown_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
        }
        tracing::info!(workspace = %target, monitor = show_on_monitor, "switched workspace");
    }

    /// Hide all windows on a workspace by positioning them below the monitor
    /// that is currently showing that workspace. macOS clamps AX moves to the
    /// window's current monitor; keeping the full-size window anchored to that
    /// monitor avoids the large visible strip that appears when a resize is
    /// rejected and only the y-position is clamped.
    fn hide_workspace_windows(&self, ws_idx: usize, monitor_idx: usize) {
        let hide_frame = self.monitors[monitor_idx].frame;
        let ws = self.workspaces.get(ws_idx);
        for wid in ws.all_window_ids() {
            self.hide_window_on_frame(wid, hide_frame, Some(monitor_idx));
        }
    }

    /// Hide all windows on a workspace without relying on a monitor-specific
    /// AX hide position. Works even after monitor detach.
    fn hide_workspace_windows_immediate(&self, ws_idx: usize) {
        let ws = self.workspaces.get(ws_idx);
        for wid in ws.all_window_ids() {
            let _ = crate::platform::skylight::set_window_group_system_alpha(wid, 0.0);
            let _ = crate::platform::skylight::set_window_group_alpha(wid, 0.0);
        }
    }

    /// Hide a single window using its workspace's monitor affinity.
    /// Keep the window full-size and use the same per-monitor coordinates that
    /// previously worked for workspace switching: right edge at monitor.x + 1,
    /// y well below the monitor bottom so macOS clamps it just off-screen.
    fn hide_window(&self, wid: WindowId) {
        let (hide_frame, monitor_idx) = self.hide_anchor_frame(wid);
        self.hide_window_on_frame(wid, hide_frame, monitor_idx);
    }

    fn hide_window_on_frame(&self, wid: WindowId, hide_frame: Rect, monitor_idx: Option<usize>) {
        let _ = crate::platform::skylight::set_window_group_system_alpha(wid, 0.0);
        let _ = crate::platform::skylight::set_window_group_alpha(wid, 0.0);

        let ax_ref = self.ax_refs.get(&wid);
        let width = ax_ref
            .and_then(|ax_ref| ax_get_size(ax_ref).ok().map(|(w, _)| w))
            .or_else(|| self.registry.get(wid).map(|window| window.width))
            .unwrap_or(2048.0);
        let (hide_x, hide_y) =
            hide_target_for_frame(hide_frame, width, monitor_idx, &self.monitors);

        if let Some(window) = self.registry.get(wid)
            && window.app_name == "Music"
        {
            tracing::info!(
                wid,
                anchor_x = hide_frame.x,
                anchor_y = hide_frame.y,
                anchor_w = hide_frame.width,
                anchor_h = hide_frame.height,
                hide_x,
                hide_y,
                hide_monitor = monitor_idx,
                "hidden window target"
            );
        }

        if let Some(ax_ref) = ax_ref {
            let _ = ax_set_position(ax_ref, hide_x, hide_y);
        } else {
            let _ = crate::platform::skylight::move_window(wid, hide_x, hide_y);
        }

        // Reassert opacity after the move in case the app redraws itself.
        let _ = crate::platform::skylight::set_window_group_alpha(wid, 0.0);
        let _ = crate::platform::skylight::set_window_group_system_alpha(wid, 0.0);
    }

    fn hide_anchor_frame(&self, wid: WindowId) -> (Rect, Option<usize>) {
        if self.monitors.is_empty() {
            return (Rect::new(-32_000.0, -32_000.0, 1.0, 1.0), None);
        }

        if let Some(ws_idx) = self.workspaces.find_window(wid) {
            if let Some(mi) = self.monitor_showing_workspace(ws_idx) {
                return (self.monitors[mi].frame, Some(mi));
            }

            let ws = self.workspaces.get(ws_idx);
            if let Some(mi) = ws.last_monitor.filter(|&mi| mi < self.monitors.len()) {
                return (self.monitors[mi].frame, Some(mi));
            }
            if let Some(display_id) = ws.last_display_id
                && let Some((mi, monitor)) = self
                    .monitors
                    .iter()
                    .enumerate()
                    .find(|(_, monitor)| monitor.id == display_id)
            {
                return (monitor.frame, Some(mi));
            }
        }

        let fallback_monitor = self.focused_monitor.min(self.monitors.len() - 1);
        (
            self.monitors[fallback_monitor].frame,
            Some(fallback_monitor),
        )
    }

    fn enforce_hidden_workspaces(&self) {
        for ws_idx in 0..self.workspaces.count() {
            if self.workspace_is_effectively_visible(ws_idx) {
                continue;
            }
            for wid in self.workspaces.get(ws_idx).all_window_ids() {
                self.hide_window(wid);
            }
        }
    }

    fn log_hidden_window_diagnostics(&self, wid: WindowId, phase: &'static str) {
        let Some(window) = self.registry.get(wid) else {
            return;
        };
        if window.app_name != "Music" {
            return;
        }

        let ws_idx = self.workspaces.find_window(wid);
        let group_ids = crate::platform::skylight::window_group_ids(wid);
        let on_screen: Vec<_> = crate::platform::application::get_cg_window_list_all_layers()
            .into_iter()
            .filter(|cg| cg.pid == window.app_pid || group_ids.contains(&cg.wid))
            .collect();

        tracing::info!(
            phase,
            wid,
            pid = window.app_pid,
            app = %window.app_name,
            workspace = ws_idx.map(|idx| idx + 1),
            workspace_visible = ws_idx.map(|idx| self.workspace_is_effectively_visible(idx)),
            group_ids = ?group_ids,
            on_screen_count = on_screen.len(),
            "hidden window diagnostics"
        );

        for cg in on_screen {
            tracing::info!(
                phase,
                root_wid = wid,
                cg_wid = cg.wid,
                pid = cg.pid,
                owner = %cg.owner,
                title = %cg.title,
                layer = cg.layer,
                alpha = cg.alpha,
                x = cg.x,
                y = cg.y,
                w = cg.width,
                h = cg.height,
                "hidden window cg state"
            );
        }
    }

    pub fn workspace_next(&mut self) {
        let current = self.active_workspace().id.as_regular_target();
        let mut regular_targets: Vec<_> = self
            .workspaces
            .iter()
            .filter_map(|ws| ws.id.as_regular_target())
            .collect();
        regular_targets.sort_by_key(|target| match target {
            WorkspaceTarget::Numbered(num) => (0, format!("{num:03}")),
            WorkspaceTarget::Lettered(ch) => (1, ch.to_string()),
        });
        let Some(current) = current else { return };
        let Some(pos) = regular_targets.iter().position(|target| *target == current) else {
            return;
        };
        let next = regular_targets[(pos + 1) % regular_targets.len()].clone();
        self.switch_workspace(&next);
    }

    pub fn workspace_prev(&mut self) {
        let current = self.active_workspace().id.as_regular_target();
        let mut regular_targets: Vec<_> = self
            .workspaces
            .iter()
            .filter_map(|ws| ws.id.as_regular_target())
            .collect();
        regular_targets.sort_by_key(|target| match target {
            WorkspaceTarget::Numbered(num) => (0, format!("{num:03}")),
            WorkspaceTarget::Lettered(ch) => (1, ch.to_string()),
        });
        let Some(current) = current else { return };
        let Some(pos) = regular_targets.iter().position(|target| *target == current) else {
            return;
        };
        let prev = regular_targets[if pos == 0 {
            regular_targets.len() - 1
        } else {
            pos - 1
        }]
        .clone();
        self.switch_workspace(&prev);
    }

    // --- Special workspaces (scratchpads) ---

    /// Toggle a special workspace overlay on the focused monitor.
    /// If hidden → show as centered overlays on top of the current workspace.
    /// If shown → hide all windows and dismiss the overlay.
    pub fn toggle_special(&mut self, name: &str) {
        let special_idx = self.workspaces.special_index(name);

        // Ensure active_specials vec covers all monitors
        while self.active_specials.len() < self.monitors.len() {
            self.active_specials.push(None);
        }

        let mi = self.focused_monitor;
        if self.active_specials[mi] == Some(special_idx) {
            // Currently shown → hide
            self.active_specials[mi] = None;
            let wids = self.workspaces.get(special_idx).all_window_ids();
            self.sync_workspace_visibility();
            for wid in wids {
                self.hide_window(wid);
            }
            // Refocus the underlying workspace
            if let Some(wid) = self.active_workspace().focused {
                self.focus_window(wid);
            }
            tracing::info!(name, ws = special_idx + 1, "special workspace hidden");
        } else {
            // Dismiss any other active special on this monitor first
            if let Some(old_idx) = self.active_specials[mi] {
                let old_wids = self.workspaces.get(old_idx).all_window_ids();
                self.active_specials[mi] = None;
                self.sync_workspace_visibility();
                for wid in old_wids {
                    self.hide_window(wid);
                }
            }

            // Mark the special workspace as visible
            self.active_specials[mi] = Some(special_idx);
            {
                let ws = self.workspaces.get_mut(special_idx);
                ws.last_monitor = Some(mi);
            }
            self.sync_workspace_visibility();

            let sr = self.monitor_rect(mi);
            let ws = self.workspaces.get(special_idx);

            if ws.tree.window_count() == 0 && ws.floating.is_empty() {
                tracing::info!(name, "special workspace is empty");
                return;
            }

            // Look up overlay config (or use defaults)
            let cfg = self
                .special_configs
                .iter()
                .find(|c| c.name == name)
                .cloned()
                .unwrap_or_else(|| crate::config::lua::SpecialWorkspaceConfig::default_for(name));

            let overlay_w = sr.width * cfg.width;
            let overlay_h = sr.height * cfg.height;
            let (overlay_x, overlay_y) = match cfg.position.as_str() {
                "top" => (sr.x + (sr.width - overlay_w) / 2.0, sr.y),
                "bottom" => (
                    sr.x + (sr.width - overlay_w) / 2.0,
                    sr.y + sr.height - overlay_h,
                ),
                _ => (
                    // "center" (default)
                    sr.x + (sr.width - overlay_w) / 2.0,
                    sr.y + (sr.height - overlay_h) / 2.0,
                ),
            };
            let overlay_rect = Rect::new(overlay_x, overlay_y, overlay_w, overlay_h);

            // Compute geometries and collect data before calling self methods
            let (gap_inner, gap_outer) = self.effective_gaps(special_idx);
            let geoms =
                ws.tree
                    .calculate_geometries_with_gaps(overlay_rect, gap_inner, gap_outer, true);
            let floating_data: Vec<(WindowId, Rect)> =
                ws.floating.iter().map(|fw| (fw.id, fw.geometry)).collect();
            let focus_target = ws.focused.or_else(|| geoms.first().map(|(id, _)| *id));

            // Now show windows (no workspace borrow held)
            for (wid, rect) in &geoms {
                self.show_window(*wid, *rect);
                crate::platform::skylight::set_window_level(
                    *wid,
                    crate::platform::skylight::K_CG_FLOATING_WINDOW_LEVEL,
                );
            }
            for (wid, rect) in &floating_data {
                self.show_window(*wid, *rect);
                crate::platform::skylight::set_window_level(
                    *wid,
                    crate::platform::skylight::K_CG_FLOATING_WINDOW_LEVEL,
                );
            }
            if let Some(wid) = focus_target {
                self.focus_window(wid);
            }
            tracing::info!(
                name,
                ws = special_idx + 1,
                windows = geoms.len(),
                "special workspace shown"
            );
        }
    }

    /// Move the focused window to a special workspace.
    /// If the special workspace is currently visible, the window appears there.
    /// If hidden, the window disappears.
    pub fn move_to_special(&mut self, name: &str) {
        let focused = match self.active_workspace().focused {
            Some(f) => f,
            None => return,
        };

        let current_idx = self.active_ws_idx();
        let special_idx = self.workspaces.special_index(name);
        if current_idx == special_idx {
            return; // Already on this special workspace
        }

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
            current_ws.focused = current_ws
                .focus_history
                .last()
                .copied()
                .or(current_ws.tree.first_window());
        }

        // Insert into special workspace
        let sr = self.focused_rect();
        let special_ws = self.workspaces.get_mut(special_idx);
        if was_floating {
            special_ws.floating.push(super::workspace::FloatingWindow {
                id: focused,
                geometry: Rect::new(sr.x + 50.0, sr.y + 50.0, 800.0, 600.0),
            });
        } else {
            special_ws
                .tree
                .insert_with_rect(focused, special_ws.focused, sr);
        }
        special_ws.record_focus(focused);

        // If the special workspace isn't visible, hide the window
        while self.active_specials.len() < self.monitors.len() {
            self.active_specials.push(None);
        }
        let is_visible = self.active_specials[self.focused_monitor] == Some(special_idx);
        if !is_visible {
            self.hide_window(focused);
        } else {
            self.restack_floating_windows(special_idx);
        }

        // Retile the source workspace
        self.apply_layout();

        // Focus next window on source workspace
        if let Some(next) = self.active_workspace().focused {
            self.focus_window(next);
        }

        tracing::info!(
            id = focused,
            special = name,
            "moved window to special workspace"
        );
    }

    pub fn focus_monitor_next(&mut self) {
        if self.monitors.len() <= 1 {
            return;
        }
        let next = super::monitor::next_index(&self.monitors, self.focused_monitor);
        self.focused_monitor = next;
        self.apply_layout();
        if let Some(wid) = self.active_workspace().focused {
            self.focus_window(wid);
            if self.mouse_follows_focus {
                let sr = self.focused_rect();
                let geoms = self.workspace_focus_geometries(self.active_ws_idx(), sr);
                if let Some((_, rect)) = geoms.iter().find(|(id, _)| *id == wid) {
                    warp_mouse_to_center(rect);
                } else {
                    warp_mouse_to_center(&sr);
                }
                self.ffm_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
                self.ffm_last_window = Some(wid);
            }
        } else if self.mouse_follows_focus {
            let sr = self.focused_rect();
            warp_mouse_to_center(&sr);
            self.ffm_cooldown_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
        }
        tracing::info!(monitor = next, ws = %self.active_workspace().id, "focused monitor");
    }

    pub fn focus_monitor_prev(&mut self) {
        if self.monitors.len() <= 1 {
            return;
        }
        let prev = super::monitor::prev_index(&self.monitors, self.focused_monitor);
        self.focused_monitor = prev;
        self.apply_layout();
        if let Some(wid) = self.active_workspace().focused {
            self.focus_window(wid);
            if self.mouse_follows_focus {
                let sr = self.focused_rect();
                let geoms = self.workspace_focus_geometries(self.active_ws_idx(), sr);
                if let Some((_, rect)) = geoms.iter().find(|(id, _)| *id == wid) {
                    warp_mouse_to_center(rect);
                } else {
                    warp_mouse_to_center(&sr);
                }
                self.ffm_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
                self.ffm_last_window = Some(wid);
            }
        } else if self.mouse_follows_focus {
            let sr = self.focused_rect();
            warp_mouse_to_center(&sr);
            self.ffm_cooldown_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
        }
        tracing::info!(monitor = prev, ws = %self.active_workspace().id, "focused monitor");
    }

    pub fn move_to_monitor_next(&mut self) {
        if self.monitors.len() <= 1 {
            return;
        }
        let next = super::monitor::next_index(&self.monitors, self.focused_monitor);
        if next != self.focused_monitor {
            self.move_window_to_monitor(next);
        }
    }

    pub fn move_to_monitor_prev(&mut self) {
        if self.monitors.len() <= 1 {
            return;
        }
        let prev = super::monitor::prev_index(&self.monitors, self.focused_monitor);
        if prev != self.focused_monitor {
            self.move_window_to_monitor(prev);
        }
    }

    /// Move the focused window from the current monitor's workspace to the target monitor's workspace.
    fn move_window_to_monitor(&mut self, target_mi: usize) {
        let focused = match self.effective_focused() {
            Some(f) => f,
            None => return,
        };

        // Get the target monitor's active workspace
        let target_ws_idx = self.monitors[target_mi].active_workspace;

        // Get target monitor's screen rect for layout
        let target_rect = self.monitor_rect(target_mi);
        let (target_gap_inner, target_gap_outer) = self.effective_gaps(target_ws_idx);

        // Find which workspace the window is actually on
        let current_idx = match self.workspaces.find_window(focused) {
            Some(idx) => idx,
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
            target_gap_inner,
            target_gap_outer,
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
        self.sync_mouse_after_focus(target_ws_idx, focused);

        tracing::info!(id = focused, monitor = target_mi, "moved window to monitor");
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
        let old_active_specials = self.active_specials.clone();
        let old_focused_display_id = self.monitors.get(self.focused_monitor).map(|m| m.id);

        // Collect old display IDs for orphan detection
        let old_display_ids: Vec<u32> = self.monitors.iter().map(|m| m.id).collect();
        let new_display_ids: Vec<u32> = new_displays.iter().map(|d| d.id).collect();

        // --- Phase 1: Match new displays to old by CGDirectDisplayID ---
        let mut new_monitors: Vec<super::monitor::Monitor> = Vec::with_capacity(new_displays.len());
        let mut used_ws: Vec<bool> = vec![false; self.workspaces.count()];

        for nd in &new_displays {
            if let Some(old) = self.monitors.iter().find(|om| om.id == nd.id) {
                // Surviving monitor — preserve workspace assignment
                let mut m = nd.clone();
                m.active_workspace = old.active_workspace;
                let new_idx = new_monitors.len();
                // Update workspace's last_monitor to the NEW index
                self.workspaces.get_mut(m.active_workspace).last_monitor = Some(new_idx);
                self.workspaces.get_mut(m.active_workspace).last_display_id = Some(nd.id);
                used_ws.resize(used_ws.len().max(m.active_workspace + 1), false);
                used_ws[m.active_workspace] = true;
                new_monitors.push(m);
            }
        }

        // --- Phase 2: Hide orphaned workspaces ---
        for &old_did in &old_display_ids {
            if new_display_ids.contains(&old_did) {
                continue; // Display survived
            }
            // Find the old monitor with this display ID
            if let Some(old_m) = self.monitors.iter().find(|m| m.id == old_did) {
                let ws_idx = old_m.active_workspace;
                tracing::info!(
                    display_id = old_did,
                    workspace = ws_idx + 1,
                    "hiding orphaned workspace from detached monitor"
                );
                self.hide_workspace_windows_immediate(ws_idx);
                let ws = self.workspaces.get_mut(ws_idx);
                ws.last_monitor = None;
                // Keep last_display_id intact for reconnection
            }
        }

        // --- Phase 3: Assign workspaces to new (unmatched) monitors ---
        for nd in &new_displays {
            if self.monitors.iter().any(|om| om.id == nd.id) {
                // Already matched in phase 1 — skip if already in new_monitors
                if new_monitors.iter().any(|nm| nm.id == nd.id) {
                    continue;
                }
            }
            if new_monitors.iter().any(|nm| nm.id == nd.id) {
                continue; // Already handled
            }

            // New monitor — prefer an explicitly assigned workspace first,
            // then reconnect by last_display_id, then any hidden workspace.
            let ws_idx = (0..self.workspaces.count())
                .find(|&i| {
                    !used_ws.get(i).copied().unwrap_or(false)
                        && !self.workspace_is_effectively_visible(i)
                        && self.workspace_prefs(i).and_then(|prefs| {
                            prefs.monitor.as_ref().map(|monitor| monitor.display_id)
                        }) == Some(nd.id)
                })
                .or_else(|| {
                    (0..self.workspaces.count()).find(|&i| {
                        !used_ws.get(i).copied().unwrap_or(false)
                            && !self.workspace_is_effectively_visible(i)
                            && self.workspaces.get(i).last_display_id == Some(nd.id)
                    })
                })
                .or_else(|| {
                    // Next preference: first hidden workspace assigned to no specific monitor.
                    (0..self.workspaces.count()).find(|&i| {
                        !used_ws.get(i).copied().unwrap_or(false)
                            && !self.workspace_is_effectively_visible(i)
                            && self
                                .workspace_prefs(i)
                                .and_then(|prefs| {
                                    prefs.monitor.as_ref().map(|monitor| monitor.display_id)
                                })
                                .is_none()
                    })
                })
                .or_else(|| {
                    (0..self.workspaces.count()).find(|&i| {
                        !used_ws.get(i).copied().unwrap_or(false)
                            && !self.workspace_is_effectively_visible(i)
                    })
                })
                .unwrap_or_else(|| {
                    // Last resort: grow
                    let idx = self.workspaces.count();
                    self.workspaces.get_or_create(idx);
                    idx
                });

            used_ws.resize(used_ws.len().max(ws_idx + 1), false);
            used_ws[ws_idx] = true;

            let new_idx = new_monitors.len();
            let mut m = nd.clone();
            m.active_workspace = ws_idx;

            let ws = self.workspaces.get_mut(ws_idx);
            ws.last_monitor = Some(new_idx);
            ws.last_display_id = Some(nd.id);

            tracing::info!(
                display_id = nd.id,
                workspace = ws_idx + 1,
                "assigned workspace to new monitor"
            );
            new_monitors.push(m);
        }

        // --- Phase 4: Replace monitors, recover focus ---
        let remapped_specials: Vec<Option<usize>> = new_monitors
            .iter()
            .map(|monitor| {
                self.monitors
                    .iter()
                    .position(|old_monitor| old_monitor.id == monitor.id)
                    .and_then(|old_idx| old_active_specials.get(old_idx).copied().flatten())
            })
            .collect();

        self.monitors = new_monitors;
        self.active_specials = remapped_specials;
        self.sync_workspace_visibility();

        // Map old focused_monitor through display ID → new index
        if let Some(old_did) = old_focused_display_id {
            if let Some(new_idx) = self.monitors.iter().position(|m| m.id == old_did) {
                self.focused_monitor = new_idx;
            } else {
                // Focused monitor was detached
                self.focused_monitor = 0;
                self.ffm_last_window = None;
                self.ffm_cooldown_until = None;
                tracing::info!("focused monitor detached, recovering focus to monitor 0");
                if let Some(wid) = self
                    .workspaces
                    .get(self.monitors[0].active_workspace)
                    .focused
                {
                    self.focus_window(wid);
                }
                let sr = self.monitor_rect(0);
                warp_mouse(sr.x + sr.width / 2.0, sr.y + sr.height / 2.0);
            }
        } else if self.focused_monitor >= self.monitors.len() {
            self.focused_monitor = 0;
        }

        // --- Phase 5: Double-apply layout ---
        self.apply_layout();
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.apply_layout();
        self.fix_oversized_windows();

        tracing::info!(
            old_count,
            new_count = self.monitors.len(),
            "monitors refreshed"
        );
    }

    pub fn move_to_workspace(&mut self, target: &WorkspaceTarget) {
        let target_idx = self.workspace_index_for_target(target);

        let focused = match self.effective_focused() {
            Some(f) => f,
            None => return,
        };

        // Find which workspace the window is actually on
        let current_idx = match self.workspaces.find_window(focused) {
            Some(idx) => idx,
            None => return,
        };
        if target_idx == current_idx {
            return;
        }

        // If removing from a special workspace, dismiss the overlay
        let mi = self.focused_monitor;
        if mi < self.active_specials.len()
            && let Some(special_idx) = self.active_specials[mi]
            && current_idx == special_idx
        {
            // Dismiss the special workspace since we're taking its window
            self.active_specials[mi] = None;
            self.sync_workspace_visibility();
            // Hide any remaining windows on the special workspace
            let remaining = self.workspaces.get(special_idx).all_window_ids();
            for wid in remaining {
                if wid != focused {
                    self.hide_window(wid);
                }
            }
        }

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
            current_ws.focused = current_ws
                .focus_history
                .last()
                .copied()
                .or(current_ws.tree.first_window());
        }

        // Insert into target workspace
        let target_rect = self
            .monitor_showing_workspace(target_idx)
            .or_else(|| self.preferred_monitor_for_workspace(target_idx))
            .map(|mi| self.monitor_rect(mi))
            .unwrap_or(self.focused_rect());
        let target_monitor = self
            .monitor_showing_workspace(target_idx)
            .or_else(|| self.preferred_monitor_for_workspace(target_idx))
            .unwrap_or(self.focused_monitor);
        let target_ws = self.workspaces.get_or_create(target_idx);
        target_ws.last_monitor = Some(target_monitor);
        target_ws.last_display_id = Some(self.monitors[target_monitor].id);
        if was_floating {
            target_ws.floating.push(super::workspace::FloatingWindow {
                id: focused,
                geometry: Rect::new(target_rect.x + 50.0, target_rect.y + 50.0, 800.0, 600.0),
            });
        } else {
            target_ws
                .tree
                .insert_with_rect(focused, target_ws.focused, target_rect);
        }
        target_ws.record_focus(focused);

        // If target is not visible, hide the window
        if !self.workspace_is_effectively_visible(target_idx) {
            self.hide_window(focused);
        }

        // Double-apply for cross-monitor moves
        self.apply_layout();
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.apply_layout();

        // User explicitly chose this workspace — try swaps, but if it still
        // overflows, float it centered here instead of evicting elsewhere.
        if !was_floating {
            self.fix_oversized_on_target(target_idx, focused);
        }

        if let Some(next) = self.active_workspace().focused {
            self.focus_window(next);
        }
        tracing::info!(id = focused, target = %target, "moved window to workspace");
    }

    /// After moving a window to a specific workspace, try swaps to fix overflow.
    /// If no swap works, replace the smallest conflicting subtree with a stack.
    /// Only runs when the target workspace is visible — hidden workspaces have
    /// stale window sizes and will be checked when switched to.
    fn fix_oversized_on_target(&mut self, ws_idx: usize, moved_wid: super::window::WindowId) {
        // Only check visible workspaces — hidden windows have stale sizes
        let screen_rect = match self.monitor_showing_workspace(ws_idx) {
            Some(mi) => self.monitor_rect(mi),
            None => return, // Will be checked on switch_workspace
        };

        std::thread::sleep(std::time::Duration::from_millis(100));

        // Check if the moved window actually overflows
        let geometries = self.workspace_focus_geometries(ws_idx, screen_rect);

        let (_min_w, _min_h) = match geometries.iter().find(|(wid, _)| *wid == moved_wid) {
            Some((_, rect)) => {
                let (aw, ah) = self
                    .ax_refs
                    .get(&moved_wid)
                    .and_then(|ax| ax_get_size(ax).ok())
                    .unwrap_or((0.0, 0.0));
                if aw <= rect.width + 1.0 && ah <= rect.height + 1.0 {
                    return; // Fits fine
                }
                (aw, ah)
            }
            None => return, // Not in tree (floating or missing)
        };

        // Try swaps with settled-set logic (same as fix_oversized_windows phase 1)
        let mut settled: Vec<super::window::WindowId> = Vec::new();
        loop {
            let geoms = self.workspace_focus_geometries(ws_idx, screen_rect);
            if geoms.is_empty() {
                break;
            }

            // Find first unsettled oversized window
            let oversized = geoms.iter().find_map(|(wid, rect)| {
                if settled.contains(wid) {
                    return None;
                }
                let ax_ref = self.ax_refs.get(wid)?;
                let (aw, ah) = ax_get_size(ax_ref).ok()?;
                if aw > rect.width + 1.0 || ah > rect.height + 1.0 {
                    Some((*wid, aw, ah))
                } else {
                    None
                }
            });

            let (ow, ow_min_w, ow_min_h) = match oversized {
                Some(v) => v,
                None => return, // All fixed by swaps
            };

            let best_swap = geoms
                .iter()
                .filter(|(wid, _)| *wid != ow && !settled.contains(wid))
                .filter(|(_, rect)| rect.width >= ow_min_w - 1.0 && rect.height >= ow_min_h - 1.0)
                .max_by(|(_, a), (_, b)| {
                    (a.width * a.height)
                        .partial_cmp(&(b.width * b.height))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(wid, _)| *wid);

            if let Some(swap_target) = best_swap {
                tracing::info!(
                    oversized = ow,
                    target = swap_target,
                    ws = ws_idx + 1,
                    "swapping oversized window into larger tile"
                );
                self.workspaces.get_mut(ws_idx).tree.swap(ow, swap_target);
                self.apply_layout();
                settled.push(ow);
            } else {
                if self
                    .workspaces
                    .get_mut(ws_idx)
                    .tree
                    .make_stack_for_window(ow)
                {
                    tracing::info!(
                        id = ow,
                        min_w = ow_min_w,
                        min_h = ow_min_h,
                        ws = ws_idx + 1,
                        "stacking oversized subtree on target workspace"
                    );
                    self.workspaces.get_mut(ws_idx).tree.set_stack_active(ow);
                    self.apply_layout();
                    std::thread::sleep(std::time::Duration::from_millis(50));
                } else {
                    break;
                }
            }
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
            std::thread::sleep(std::time::Duration::from_millis(50));
            self.apply_layout();
            self.fix_oversized_windows();
        }
        self.install_observer_for_app(pid, &ax_app, owner, "");
    }

    /// Returns true if the window was actually reaped, false if the call was
    /// a no-op (window not in registry, or suppressed because parent app is
    /// hidden). Callers should use the return value to gate side effects
    /// like IPC WindowClosed publishes.
    pub fn on_window_closed(&mut self, wid: u32) -> bool {
        let id = wid as WindowId;
        if !self.registry.contains(id) {
            return false;
        }
        // Hide Others (cmd+opt+H) and single-app Hide (cmd+H) drop the app's
        // windows from CGWindowList even though they still exist. Both the
        // polling observer and the 1Hz reconcile interpret that absence as
        // closure and call here. If the parent app is hidden, the windows
        // are alive — leave them in the registry/tree so they snap back to
        // their original tiles when the user un-hides.
        if let Some(w) = self.registry.get(id)
            && crate::platform::application::app_is_hidden(w.app_pid)
        {
            tracing::info!(
                id,
                pid = w.app_pid,
                "suppressing reap: parent app is hidden, window still alive"
            );
            return false;
        }
        tracing::info!(id, "window closed -> retiling");
        self.borders.remove_border(id);
        self.registry.remove(id);
        self.ax_refs.remove(&id);
        self.pseudo_windows.remove(&id);
        self.ax_write_last.borrow_mut().remove(&id);
        self.pending_show_window.borrow_mut().remove(&id);
        self.prune_focus_memory_for_window(id);

        // Find the workspace containing this window and remove from it
        let mut new_focus: Option<(usize, WindowId)> = None;
        if let Some(ws_idx) = self.workspaces.find_window(id) {
            let ws = self.workspaces.get_mut(ws_idx);
            ws.floating.retain(|f| f.id != id);
            ws.tree.remove(id);
            if ws.focused == Some(id) {
                ws.pop_focus();
                if let Some(next) = ws.focused {
                    new_focus = Some((ws_idx, next));
                }
            }
        }
        self.apply_layout();

        // pop_focus only updates WM state — without an AX activation the
        // receiving window has no key focus and the user gets the system
        // bell on the next keystroke. Drive the full focus path.
        if let Some((ws_idx, next)) = new_focus {
            self.focus_window(next);
            self.sync_mouse_after_focus(ws_idx, next);
        }
        true
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
            self.pseudo_windows.remove(&w.id);
            self.ax_write_last.borrow_mut().remove(&w.id);
            self.pending_show_window.borrow_mut().remove(&w.id);
            self.prune_focus_memory_for_window(w.id);
            // Find the workspace containing this window and remove from it
            if let Some(ws_idx) = self.workspaces.find_window(w.id) {
                let ws = self.workspaces.get_mut(ws_idx);
                ws.floating.retain(|f| f.id != w.id);
                ws.tree.remove(w.id);
            }
        }
        if !removed.is_empty() {
            // Fix focus on any affected workspace
            let active_ws_idx = self.active_ws_idx();
            let mut active_ws_new_focus: Option<WindowId> = None;
            for ws_idx in 0..self.workspaces.count() {
                let ws = self.workspaces.get_mut(ws_idx);
                if ws
                    .focused
                    .is_some_and(|f| removed.iter().any(|w| w.id == f))
                {
                    ws.pop_focus();
                    if ws_idx == active_ws_idx {
                        active_ws_new_focus = ws.focused;
                    }
                }
            }
            self.apply_layout();
            // The active workspace's new focused window needs an AX
            // activation, otherwise the next keystroke rings the system
            // bell. Background workspaces don't need this since the user
            // can't type into them until they're switched to.
            if let Some(next) = active_ws_new_focus {
                self.focus_window(next);
                self.sync_mouse_after_focus(active_ws_idx, next);
            }
            tracing::info!(pid, removed = removed.len(), "app terminated -> retiled");
        }
    }

    /// HX-05: derive a focal point for inserting a new window into the
    /// active workspace. Order of precedence:
    ///   1. Consume an active-workspace preselect hint if set (using
    ///      the focused window's edge in the hint direction). When
    ///      `permanent_direction_override` is true the hint is
    ///      re-stored after consumption so it survives multiple opens.
    ///   2. Otherwise route by `focal_point_source`:
    ///      - "cursor"           — last_cursor if available, else active.
    ///      - "active"           — focused window's center.
    ///      - "cursor_or_active" — cursor if inside workspace, else active.
    ///   3. Final fallback when there is no focused window: workspace
    ///      rect center.
    fn compute_focal_point_for_new_window(&mut self) -> super::tree::FocalPoint {
        let sr = self.focused_rect();
        let ws_idx = self.active_ws_idx();
        let focused_wid = self.active_workspace().focused;
        let focused_rect = focused_wid.and_then(|w| {
            self.workspace_focus_geometries(ws_idx, sr)
                .into_iter()
                .find(|(wid, _)| *wid == w)
                .map(|(_, r)| r)
        });

        // Preselect short-circuits everything else.
        if let Some((dir, persistent)) = self.active_workspace_mut().take_preselect() {
            let base = focused_rect.unwrap_or(sr);
            let focal = base.focal_point_for_direction(dir);
            if persistent {
                self.active_workspace_mut().preselect = Some((dir, true));
            }
            return focal;
        }

        let cursor_in_workspace = self.last_cursor.filter(|&(x, y)| sr.contains_point(x, y));
        match self.focal_point_source.as_str() {
            "cursor" => self
                .last_cursor
                .or_else(|| focused_rect.map(|r| r.center()))
                .unwrap_or_else(|| sr.center()),
            "active" => focused_rect.map(|r| r.center()).unwrap_or_else(|| sr.center()),
            _ => cursor_in_workspace
                .or_else(|| focused_rect.map(|r| r.center()))
                .unwrap_or_else(|| sr.center()),
        }
    }

    /// Map `Settings::split_axis` string onto a tree-level split
    /// override. "smart" → None (aspect-driven via multiplier).
    fn split_axis_override(&self) -> Option<super::tree::SplitDirection> {
        match self.split_axis.as_str() {
            "horizontal" => Some(super::tree::SplitDirection::Horizontal),
            "vertical" => Some(super::tree::SplitDirection::Vertical),
            _ => None,
        }
    }

    /// Check if a window is on an inactive workspace (intentionally hidden).
    pub fn is_window_hidden(&self, wid: u32) -> bool {
        let id = wid as WindowId;
        if let Some(ws_idx) = self.workspaces.find_window(id) {
            !self.workspace_is_effectively_visible(ws_idx)
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

        // Check window rules for matching
        let mut rule_float: Option<bool> = None;
        let mut rule_workspace: Option<String> = None;
        let mut rule_geometry: Option<(f64, f64, f64, f64)> = None;
        for rule in &self.rules {
            if rule.matches_window(app_name, app_bundle_id, title) {
                tracing::debug!(app_name, title, ?rule, "window rule matched");
                if let Some(f) = rule.floating {
                    rule_float = Some(f);
                }
                if let Some(ref w) = rule.workspace {
                    rule_workspace = Some(w.clone());
                }
                if let Some(g) = rule.geometry {
                    rule_geometry = Some(g);
                }
            }
        }

        let should_float = rule_float.unwrap_or_else(|| should_auto_float(subrole, &ax_ref));

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
        if let Some(ref ws_str) = rule_workspace {
            if let Some(special_name) = ws_str.strip_prefix("special:") {
                // Special workspace rule — send directly to scratchpad
                let special_idx = self.workspaces.special_index(special_name);
                let sr = self.focused_rect();
                let ws = self.workspaces.get_mut(special_idx);
                if should_float {
                    let geom = rule_geometry
                        .map(|(gx, gy, gw, gh)| Rect::new(gx, gy, gw, gh))
                        .unwrap_or_else(|| Rect::new(x, y, width, height));
                    ws.floating.push(super::workspace::FloatingWindow {
                        id: *id,
                        geometry: geom,
                    });
                } else {
                    ws.tree.insert_with_rect(*id, ws.focused, sr);
                }
                ws.record_focus(*id);
                self.hide_window(*id);
                tracing::info!(
                    id,
                    app_name,
                    special_name,
                    "window assigned to special workspace by rule"
                );
            } else if let Some(target) = WorkspaceTarget::parse(ws_str) {
                // Regular workspace rule
                let target_idx = self.workspace_index_for_target(&target);
                let is_active = target_idx == self.active_ws_idx();
                let target_rect = self
                    .monitor_showing_workspace(target_idx)
                    .or_else(|| self.preferred_monitor_for_workspace(target_idx))
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
                tracing::info!(id, app_name, workspace = %target, "window assigned to workspace by rule");

                if !is_active {
                    self.hide_window(*id);
                    self.switch_workspace(&target);
                }
            }
        } else {
            let sr = self.focused_rect();
            if should_float {
                let geom = rule_geometry
                    .map(|(gx, gy, gw, gh)| Rect::new(gx, gy, gw, gh))
                    .unwrap_or_else(|| Rect::new(x, y, width, height));
                let ws = self.active_workspace_mut();
                ws.floating.push(super::workspace::FloatingWindow {
                    id: *id,
                    geometry: geom,
                });
                tracing::info!(id, subrole, "auto-floated window");
                ws.record_focus(*id);
            } else {
                // HX-05 focal-point insertion. Compute focal *before* the
                // mutable workspace borrow because the focal helper itself
                // mutates (consumes preselect).
                let focal = self.compute_focal_point_for_new_window();
                let split_override = self.split_axis_override();
                let multiplier = self.split_width_multiplier;
                let force_split_left = self.force_split == "left";
                let new_id = *id;
                let ws = self.active_workspace_mut();
                ws.tree.insert_at_focal_point_options(
                    new_id,
                    focal,
                    sr,
                    split_override,
                    multiplier,
                );
                if force_split_left {
                    // The default Leaf→Internal conversion puts the new
                    // window on the right (second child). force_split=left
                    // inverts that by swapping the freshly-created
                    // parent's children.
                    ws.tree.swap_parent_children(new_id);
                }
                ws.record_focus(new_id);
            }
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
                // Reject phantom windows: zero/tiny size or empty-titled
                // duplicate from an already-tracked app.
                if w < 50.0 || h < 50.0 {
                    tracing::debug!(
                        id,
                        app = app_name,
                        w,
                        h,
                        "skipping phantom window (too small)"
                    );
                    return;
                }
                // Apps like Messages, Codex, WezTerm create invisible helper
                // windows with AXStandardWindow subrole and valid sizes but
                // empty titles. If the app already has a window in the registry,
                // reject empty-titled new windows as phantoms.
                if title.is_empty() && self.registry.all().any(|w| w.app_pid == *pid) {
                    tracing::debug!(
                        id,
                        app = app_name,
                        "skipping phantom window (empty title, app already tracked)"
                    );
                    return;
                }
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
                std::thread::sleep(std::time::Duration::from_millis(50));
                self.apply_layout();
                self.fix_oversized_windows();
            }
            WindowEvent::Destroyed { element, .. } => {
                // _AXUIElementGetWindow frequently returns kAXErrorInvalidUIElement
                // by the time AXUIElementDestroyed fires (the underlying window is
                // already gone). Fall back to identifying the wid by AXUIElement
                // identity in our own ax_refs map — that survives destruction
                // because we hold a CFRetained.
                let id = ax_get_window_id(element)
                    .ok()
                    .or_else(|| self.find_window_by_ax_element(element));
                if let Some(id) = id
                    && self.registry.contains(id)
                {
                    tracing::info!(id, app = app_name, "window destroyed -> retiling");
                    self.on_window_closed(id);
                } else if id.is_none() {
                    tracing::debug!(
                        app = app_name,
                        "Destroyed event with unidentifiable element \
                         — polling fallback will catch it"
                    );
                }
            }
            WindowEvent::FocusChanged { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && self.registry.contains(id)
                {
                    let pid = self
                        .registry
                        .get(id)
                        .map(|window| window.app_pid)
                        .unwrap_or(0);
                    self.adopt_external_app_focus(pid, Some(id));
                }
            }
            WindowEvent::Moved { element, .. } => {
                if let Ok(id) = ax_get_window_id(element)
                    && let Ok((x, y)) = ax_get_position(element)
                    && let Some(w) = self.registry.get(id)
                {
                    if self.is_window_hidden(id) {
                        return;
                    }
                    let (width, height) = (w.width, w.height);
                    self.registry.update_geometry(id, x, y, width, height);
                    // Update floating window geometry on its actual workspace
                    if let Some(ws_idx) = self.workspaces.find_window(id)
                        && let Some(fw) = self
                            .workspaces
                            .get_mut(ws_idx)
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
                    if self.is_window_hidden(id) {
                        return;
                    }
                    self.registry.update_geometry(id, x, y, w, h);
                    if let Some(ws_idx) = self.workspaces.find_window(id)
                        && let Some(fw) = self
                            .workspaces
                            .get_mut(ws_idx)
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
fn should_auto_float(subrole: &str, ax_ref: &AXUIElement) -> bool {
    use crate::platform::accessibility::ax_get_bool;
    use objc2_core_foundation::CFString;

    if matches!(
        subrole,
        "AXDialog" | "AXSystemDialog" | "AXSheet" | "AXFloatingWindow" | "AXSystemFloatingWindow"
    ) {
        return true;
    }

    // Many "modal-feeling" NSWindows ship with subrole=AXStandardWindow but
    // mark AXModal=true. Treat those as floats so save/confirm dialogs and
    // app-defined modals don't get tiled alongside their parent window.
    let modal_attr = CFString::from_static_str("AXModal");
    if ax_get_bool(ax_ref, &modal_attr).unwrap_or(false) {
        return true;
    }

    false
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

fn hide_target_for_frame(
    frame: Rect,
    width: f64,
    monitor_idx: Option<usize>,
    monitors: &[super::monitor::Monitor],
) -> (f64, f64) {
    let hide_y = frame.y + frame.height + 5000.0;

    let Some(monitor_idx) = monitor_idx.filter(|&idx| idx < monitors.len()) else {
        return (frame.x + 1.0 - width, hide_y);
    };

    let global_min_x = monitors
        .iter()
        .map(|monitor| monitor.frame.x)
        .fold(f64::INFINITY, f64::min);
    let global_max_x = monitors
        .iter()
        .map(|monitor| monitor.frame.x + monitor.frame.width)
        .fold(f64::NEG_INFINITY, f64::max);

    let frame_max_x = frame.x + frame.width;
    let left_distance = frame.x - global_min_x;
    let right_distance = global_max_x - frame_max_x;

    let hide_x = if left_distance <= right_distance {
        frame.x + 1.0 - width
    } else {
        frame_max_x - 1.0
    };

    let _ = monitor_idx;
    (hide_x, hide_y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::monitor::Monitor;
    use crate::core::tree::Direction;
    use crate::core::window::WindowState;

    fn tracked_window(id: u32, pid: i32, name: &str) -> WindowState {
        WindowState {
            id,
            app_pid: pid,
            app_name: name.to_string(),
            app_bundle_id: format!("com.test.{name}"),
            title: format!("{name} {id}"),
            role: "AXWindow".to_string(),
            subrole: "AXStandardWindow".to_string(),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            floating: false,
            minimized: false,
        }
    }

    #[test]
    fn hide_anchor_frame_defaults_without_monitors() {
        let state = WmState::new();
        let (hide_frame, monitor_idx) = state.hide_anchor_frame(42);
        assert_eq!((hide_frame.x, hide_frame.y), (-32_000.0, -32_000.0));
        assert_eq!(monitor_idx, None);
    }

    #[test]
    fn hide_anchor_frame_prefers_visible_workspace_monitor() {
        let mut state = WmState::new();
        state.monitors = vec![
            Monitor {
                id: 10,
                frame: Rect::new(-1920.0, 0.0, 1920.0, 1080.0),
                usable_frame: Rect::new(-1920.0, 33.0, 1920.0, 1047.0),
                is_primary: false,
                active_workspace: 1,
            },
            Monitor {
                id: 20,
                frame: Rect::new(0.0, 0.0, 1728.0, 1117.0),
                usable_frame: Rect::new(0.0, 33.0, 1728.0, 1084.0),
                is_primary: true,
                active_workspace: 0,
            },
        ];
        state.workspaces.get_mut(1).visible = true;
        state.workspaces.get_mut(1).last_monitor = Some(0);
        state.monitors[0].active_workspace = 1;
        state.registry.add(WindowState {
            id: 99,
            app_pid: 1,
            app_name: "Test".to_string(),
            app_bundle_id: "test.bundle".to_string(),
            title: String::new(),
            role: "AXWindow".to_string(),
            subrole: "AXStandardWindow".to_string(),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            floating: false,
            minimized: false,
        });
        state
            .workspaces
            .get_mut(1)
            .tree
            .insert_with_rect(99, None, state.monitors[0].usable_frame);

        let (hide_frame, monitor_idx) = state.hide_anchor_frame(99);
        assert_eq!(monitor_idx, Some(0));
        assert_eq!(hide_frame, state.monitors[0].frame);
    }

    #[test]
    fn hide_anchor_frame_uses_last_display_id_for_hidden_workspace() {
        let mut state = WmState::new();
        state.monitors = vec![
            Monitor {
                id: 10,
                frame: Rect::new(-1920.0, 0.0, 1920.0, 1080.0),
                usable_frame: Rect::new(-1920.0, 33.0, 1920.0, 1047.0),
                is_primary: false,
                active_workspace: 1,
            },
            Monitor {
                id: 20,
                frame: Rect::new(0.0, 0.0, 1728.0, 1117.0),
                usable_frame: Rect::new(0.0, 33.0, 1728.0, 1084.0),
                is_primary: true,
                active_workspace: 0,
            },
        ];
        state.focused_monitor = 0;
        state.workspaces.get_mut(2).last_display_id = Some(20);
        state.registry.add(WindowState {
            id: 77,
            app_pid: 1,
            app_name: "Test".to_string(),
            app_bundle_id: "test.bundle".to_string(),
            title: String::new(),
            role: "AXWindow".to_string(),
            subrole: "AXStandardWindow".to_string(),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            floating: false,
            minimized: false,
        });
        state
            .workspaces
            .get_mut(2)
            .tree
            .insert_with_rect(77, None, state.monitors[1].usable_frame);

        let (hide_frame, monitor_idx) = state.hide_anchor_frame(77);
        assert_eq!(monitor_idx, Some(1));
        assert_eq!(hide_frame, state.monitors[1].frame);
    }

    #[test]
    fn hide_target_for_frame_keeps_right_edge_one_point_inside_monitor() {
        let monitors = vec![
            Monitor {
                id: 10,
                frame: Rect::new(-1920.0, 0.0, 1920.0, 1080.0),
                usable_frame: Rect::new(-1920.0, 33.0, 1920.0, 1047.0),
                is_primary: false,
                active_workspace: 1,
            },
            Monitor {
                id: 20,
                frame: Rect::new(0.0, 0.0, 1710.0, 1107.0),
                usable_frame: Rect::new(0.0, 33.0, 1710.0, 1074.0),
                is_primary: true,
                active_workspace: 0,
            },
        ];
        let frame = monitors[0].frame;
        let (hide_x, hide_y) = hide_target_for_frame(frame, 980.0, Some(0), &monitors);
        assert_eq!(hide_x, -2899.0);
        assert_eq!(hide_y, 6080.0);
    }

    #[test]
    fn hide_target_for_frame_uses_monitor_origin_for_positive_x_displays() {
        let monitors = vec![
            Monitor {
                id: 10,
                frame: Rect::new(-1920.0, 0.0, 1920.0, 1080.0),
                usable_frame: Rect::new(-1920.0, 33.0, 1920.0, 1047.0),
                is_primary: false,
                active_workspace: 1,
            },
            Monitor {
                id: 20,
                frame: Rect::new(0.0, 0.0, 1710.0, 1107.0),
                usable_frame: Rect::new(0.0, 33.0, 1710.0, 1074.0),
                is_primary: true,
                active_workspace: 0,
            },
        ];
        let frame = monitors[1].frame;
        let (hide_x, hide_y) = hide_target_for_frame(frame, 980.0, Some(1), &monitors);
        assert_eq!(hide_x, 1709.0);
        assert_eq!(hide_y, 6107.0);
    }

    #[test]
    fn hide_target_for_frame_falls_back_without_monitor_affinity() {
        let frame = Rect::new(0.0, 0.0, 1710.0, 1107.0);
        let (hide_x, hide_y) = hide_target_for_frame(frame, 980.0, None, &[]);
        assert_eq!(hide_x, -979.0);
        assert_eq!(hide_y, 6107.0);
    }

    #[test]
    fn special_workspace_counts_as_effectively_visible() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1710.0, 1107.0),
            usable_frame: Rect::new(0.0, 33.0, 1710.0, 1074.0),
            is_primary: true,
            active_workspace: 0,
        }];
        let special_idx = state.workspaces.special_index("scratch");
        state.active_specials = vec![Some(special_idx)];
        state.sync_workspace_visibility();

        assert!(state.workspace_is_effectively_visible(special_idx));
        assert!(state.workspaces.get(special_idx).visible);
    }

    #[test]
    fn is_window_hidden_uses_effective_visibility_not_cached_flag() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1710.0, 1107.0),
            usable_frame: Rect::new(0.0, 33.0, 1710.0, 1074.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.registry.add(WindowState {
            id: 123,
            app_pid: 1,
            app_name: "Test".to_string(),
            app_bundle_id: "test.bundle".to_string(),
            title: String::new(),
            role: "AXWindow".to_string(),
            subrole: "AXStandardWindow".to_string(),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            floating: false,
            minimized: false,
        });
        state.workspaces.get_mut(1).tree.insert_with_rect(
            123,
            None,
            state.monitors[0].usable_frame,
        );
        state.workspaces.get_mut(1).visible = true; // Stale cached state.

        assert!(state.is_window_hidden(123));

        state.monitors[0].active_workspace = 1;
        state.sync_workspace_visibility();
        assert!(!state.is_window_hidden(123));
    }

    #[test]
    fn focus_direction_cycles_stacked_windows_left_right() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        {
            let ws = state.workspaces.get_mut(0);
            ws.tree.insert_with_rect(1, None, screen);
            ws.tree.insert_with_rect(2, Some(1), screen);
            ws.tree.insert_with_rect(3, Some(2), screen);
            assert!(ws.tree.make_stack_for_window(3));
            ws.tree.set_stack_active(3);
            ws.record_focus(3);
        }

        state.focus_direction(Direction::Left);
        assert_eq!(state.active_workspace().focused, Some(2));

        state.focus_direction(Direction::Left);
        assert_eq!(state.active_workspace().focused, Some(1));

        state.focus_direction(Direction::Right);
        assert_eq!(state.active_workspace().focused, Some(2));

        state.focus_direction(Direction::Right);
        assert_eq!(state.active_workspace().focused, Some(3));
    }

    #[test]
    fn focus_direction_returns_to_last_ambiguous_tile() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        {
            let ws = state.workspaces.get_mut(0);
            ws.tree.insert_with_rect(1, None, screen);
            ws.tree.insert_with_rect(2, Some(1), screen);
            ws.tree.insert_with_rect(3, Some(2), screen);
            ws.record_focus(3);
        }

        state.focus_direction(Direction::Left);
        assert_eq!(state.active_workspace().focused, Some(1));

        state.focus_direction(Direction::Right);
        assert_eq!(state.active_workspace().focused, Some(3));

        state.focus_direction(Direction::Up);
        assert_eq!(state.active_workspace().focused, Some(2));

        state.focus_direction(Direction::Left);
        assert_eq!(state.active_workspace().focused, Some(1));

        state.focus_direction(Direction::Right);
        assert_eq!(state.active_workspace().focused, Some(2));
    }

    /// Regression: when a directional traversal jumps diagonally (the
    /// only candidate in `direction` has no perpendicular-axis overlap
    /// with the source), the bidirectional focus memory must NOT
    /// memoize the inverse — otherwise a later press in the opposite
    /// direction routes to the perpendicular tile instead of the
    /// natural axis-aligned target. Concretely: with a tall left tile
    /// (1), upper-right tile (2), and lower-right tile (3), an Up
    /// transition from 1 → 2 is diagonal (no horizontal overlap). Down
    /// from 2 must drop to 3, not jump back to 1.
    #[test]
    fn diagonal_transition_does_not_poison_opposite_axis_memory() {
        let mut state = WmState::new();
        // Geoms emulate: 1 tall on the left, 2 upper-right, 3 lower-right.
        let geoms = vec![
            (1u32, Rect::new(0.0, 0.0, 500.0, 1000.0)),
            (2u32, Rect::new(500.0, 0.0, 500.0, 500.0)),
            (3u32, Rect::new(500.0, 500.0, 500.0, 500.0)),
        ];

        // Simulate the diagonal Up transition 1 → 2.
        state.remember_focus_transition(1, Direction::Up, 2, &geoms);

        // Forward memory is recorded.
        assert_eq!(
            state.focus_return_memory.get(&(1, Direction::Up)).copied(),
            Some(2)
        );
        // Inverse memory MUST NOT be recorded — 1 and 2 have no
        // horizontal overlap, so Down from 2 should follow the natural
        // axis-aligned default (3), not jump to 1.
        assert_eq!(state.focus_return_memory.get(&(2, Direction::Down)), None);
    }

    /// Sanity check the inverse-memory write that the above test
    /// suppresses: an axis-aligned Left transition from 2 → 1 must
    /// still record the (1, Right) → 2 return path so that pressing
    /// Right after Left round-trips back to 2.
    #[test]
    fn axis_aligned_transition_records_inverse_memory() {
        let mut state = WmState::new();
        let geoms = vec![
            (1u32, Rect::new(0.0, 0.0, 500.0, 500.0)),
            (2u32, Rect::new(500.0, 0.0, 500.0, 500.0)),
        ];

        state.remember_focus_transition(2, Direction::Left, 1, &geoms);

        assert_eq!(
            state
                .focus_return_memory
                .get(&(2, Direction::Left))
                .copied(),
            Some(1)
        );
        assert_eq!(
            state
                .focus_return_memory
                .get(&(1, Direction::Right))
                .copied(),
            Some(2)
        );
    }

    /// Regression: a stale focus_return_memory entry from a prior layout
    /// (when only the far-left window existed as a Left candidate from
    /// the source) must not override the obvious nearest-neighbor
    /// default after a new window appears between source and the
    /// remembered target. Reproduces the widescreen layout where
    /// pressing Left from the bottom-right tile would skip the new
    /// middle-tall window and land on the far-left tile.
    #[test]
    fn focus_memory_yields_to_closer_default_when_topology_grows() {
        let mut state = WmState::new();
        // Original 3-window layout: far-left tall (1), upper-right (2),
        // lower-right (3). From 3, Left has only 1 as a candidate.
        let geoms_before = vec![
            (1u32, Rect::new(0.0, 0.0, 500.0, 1000.0)),
            (2u32, Rect::new(500.0, 0.0, 500.0, 500.0)),
            (3u32, Rect::new(500.0, 500.0, 500.0, 500.0)),
        ];
        state.remember_focus_transition(3, Direction::Left, 1, &geoms_before);
        assert_eq!(
            state
                .focus_return_memory
                .get(&(3, Direction::Left))
                .copied(),
            Some(1)
        );

        // New 4-window layout: a middle-tall (4) appears between the
        // left column and the right column. Now Left from 3 should
        // pick 4, not the stale memorized 1.
        let geoms_after = vec![
            (1u32, Rect::new(0.0, 0.0, 250.0, 1000.0)),
            (4u32, Rect::new(250.0, 0.0, 250.0, 1000.0)),
            (2u32, Rect::new(500.0, 0.0, 500.0, 500.0)),
            (3u32, Rect::new(500.0, 500.0, 500.0, 500.0)),
        ];
        let from_rect = geoms_after[3].1;
        let default_target = Node::find_adjacent(&geoms_after, 3, Direction::Left).unwrap();
        assert_eq!(default_target, 4);
        let candidates = Node::adjacent_candidates(&geoms_after, 3, Direction::Left);
        let default_rect = geoms_after
            .iter()
            .find(|(w, _)| *w == default_target)
            .map(|(_, r)| *r)
            .unwrap();
        let target = state
            .focus_return_memory
            .get(&(3, Direction::Left))
            .copied()
            .filter(|remembered| {
                if !candidates.contains(remembered) {
                    return false;
                }
                let Some(remembered_rect) = geoms_after
                    .iter()
                    .find(|(w, _)| *w == *remembered)
                    .map(|(_, r)| *r)
                else {
                    return false;
                };
                let d_default = Node::adjacent_distance(&from_rect, &default_rect, Direction::Left);
                let d_remembered =
                    Node::adjacent_distance(&from_rect, &remembered_rect, Direction::Left);
                d_remembered - d_default <= 32.0
            })
            .unwrap_or(default_target);
        assert_eq!(target, 4, "stale memory should yield to closer default");
    }

    #[test]
    fn mouse_hover_on_inactive_stack_sliver_does_not_change_focus() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        {
            let ws = state.workspaces.get_mut(0);
            ws.tree.insert_with_rect(70, None, screen);
            ws.tree.insert_with_rect(71, Some(70), screen);
            assert!(ws.tree.make_stack_for_window(71));
            ws.tree.set_stack_active(70);
            ws.record_focus(70);
        }

        let geoms = state.workspace_render_geometries(0, screen);
        let inactive = geoms.iter().find(|(wid, _)| *wid == 71).unwrap().1;
        let hover_x = inactive.x + inactive.width - 1.0;
        let hover_y = inactive.y + 1.0;

        state.mouse_moved(hover_x, hover_y);

        assert_eq!(state.active_workspace().focused, Some(70));
        assert_eq!(
            state.active_workspace().tree.stack_info(70),
            Some((vec![70, 71], 0))
        );
    }

    #[test]
    fn external_focus_reveals_hidden_workspace_and_tracks_target_window() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        state
            .workspaces
            .get_or_create_target(&WorkspaceTarget::Numbered(2));

        {
            let ws = state.workspaces.get_mut(1);
            ws.tree.insert_with_rect(10, None, screen);
            ws.tree.insert_with_rect(11, Some(10), screen);
            assert!(ws.tree.make_stack_for_window(11));
            ws.tree.set_stack_active(10);
            ws.record_focus(10);
        }

        state.adopt_external_focus(11);

        assert_eq!(state.monitors[0].active_workspace, 1);
        assert_eq!(state.focused_monitor, 0);
        assert_eq!(state.active_workspace().focused, Some(11));
        assert_eq!(
            state.active_workspace().tree.stack_info(11),
            Some((vec![10, 11], 1))
        );
    }

    #[test]
    fn external_focus_jumps_to_monitor_showing_window_workspace() {
        let mut state = WmState::new();
        state.monitors = vec![
            Monitor {
                id: 42,
                frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
                is_primary: true,
                active_workspace: 0,
            },
            Monitor {
                id: 43,
                frame: Rect::new(1920.0, 0.0, 1920.0, 1080.0),
                usable_frame: Rect::new(1920.0, 0.0, 1920.0, 1080.0),
                is_primary: false,
                active_workspace: 1,
            },
        ];
        state
            .workspaces
            .get_or_create_target(&WorkspaceTarget::Numbered(2));
        state.sync_workspace_visibility();
        let screen = state.monitors[1].usable_frame;

        {
            let ws = state.workspaces.get_mut(1);
            ws.tree.insert_with_rect(21, None, screen);
            ws.tree.insert_with_rect(22, Some(21), screen);
            assert!(ws.tree.make_stack_for_window(22));
            ws.tree.set_stack_active(21);
            ws.record_focus(21);
        }

        state.focused_monitor = 0;
        state.adopt_external_focus(22);

        assert_eq!(state.focused_monitor, 1);
        assert_eq!(state.monitors[1].active_workspace, 1);
        assert_eq!(state.workspaces.get(1).focused, Some(22));
        assert_eq!(
            state.workspaces.get(1).tree.stack_info(22),
            Some((vec![21, 22], 1))
        );
    }

    #[test]
    fn external_app_focus_falls_back_to_last_focused_window() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        state.registry.add(tracked_window(30, 7, "Messages"));
        state.registry.add(tracked_window(31, 7, "Messages"));
        state
            .workspaces
            .get_or_create_target(&WorkspaceTarget::Numbered(2));

        {
            let ws = state.workspaces.get_mut(1);
            ws.tree.insert_with_rect(30, None, screen);
            ws.tree.insert_with_rect(31, Some(30), screen);
            assert!(ws.tree.make_stack_for_window(31));
            ws.tree.set_stack_active(30);
            ws.record_focus(30);
            ws.record_focus(31);
        }

        state.adopt_external_app_focus(7, None);

        assert_eq!(state.monitors[0].active_workspace, 1);
        assert_eq!(state.active_workspace().focused, Some(31));
        assert_eq!(
            state.active_workspace().tree.stack_info(31),
            Some((vec![30, 31], 1))
        );
    }

    #[test]
    fn external_focus_arms_mouse_follow_for_promoted_stack_window() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        state.registry.add(tracked_window(50, 11, "Firefox"));
        state.registry.add(tracked_window(51, 11, "Messages"));
        state
            .workspaces
            .get_or_create_target(&WorkspaceTarget::Numbered(2));

        {
            let ws = state.workspaces.get_mut(1);
            ws.tree.insert_with_rect(50, None, screen);
            ws.tree.insert_with_rect(51, Some(50), screen);
            assert!(ws.tree.make_stack_for_window(51));
            ws.tree.set_stack_active(50);
            ws.record_focus(50);
        }

        state.adopt_external_app_focus(11, Some(51));

        assert_eq!(state.active_workspace().focused, Some(51));
        assert_eq!(state.ffm_last_window, Some(51));
        assert!(state.ffm_cooldown_until.is_some());
        assert_eq!(
            state.active_workspace().tree.stack_info(51),
            Some((vec![50, 51], 1))
        );
    }

    #[test]
    fn workspace_switch_silences_external_app_focus_callback() {
        // Regression: a frontmost-app poll that arrives mid workspace switch
        // (with a stale pid that doesn't match the ring) used to switch us
        // back across workspaces, kicking off the rapid-cycling loop.
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        let screen = state.monitors[0].usable_frame;

        state.registry.add(tracked_window(90, 21, "AppA"));
        state.registry.add(tracked_window(91, 22, "AppB"));
        state
            .workspaces
            .get_or_create_target(&WorkspaceTarget::Numbered(2));
        {
            let ws = state.workspaces.get_mut(0);
            ws.tree.insert_with_rect(90, None, screen);
            ws.record_focus(90);
        }
        {
            let ws = state.workspaces.get_mut(1);
            ws.tree.insert_with_rect(91, None, screen);
            ws.record_focus(91);
        }
        state.sync_workspace_visibility();

        state.switch_workspace(&WorkspaceTarget::Numbered(2));
        assert_eq!(state.monitors[0].active_workspace, 1);

        // Lagging poll for the previous workspace's frontmost app — must not
        // yank us back to ws1 while the silence latch is armed.
        state.adopt_external_app_focus(21, None);
        assert_eq!(state.monitors[0].active_workspace, 1);
    }

    #[test]
    fn recent_internal_focus_ignores_lagging_poll_for_prior_activation() {
        // Regression: the single-slot guard let the *previous* activation's
        // pid leak through once a newer activation overwrote the slot. With
        // a ring, a frontmost-app poll arriving for the older pid should
        // still be recognized as self-initiated.
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        state.registry.add(tracked_window(80, 14, "AppA"));
        state.registry.add(tracked_window(81, 15, "AppB"));
        {
            let ws = state.workspaces.get_mut(0);
            ws.tree.insert_with_rect(80, None, screen);
            ws.tree.insert_with_rect(81, Some(80), screen);
            ws.record_focus(80);
        }

        // Two intentional activations in quick succession — single-slot would
        // forget the first one.
        state.focus_window(80);
        state.focus_window(81);

        // A lagging poll arrives for the older activation's pid.
        assert!(state.should_ignore_external_focus(14, None));
        // And for the newer one.
        assert!(state.should_ignore_external_focus(15, None));
        // Unrelated pid still passes through.
        assert!(!state.should_ignore_external_focus(99, None));
    }

    #[test]
    fn internal_focus_echo_does_not_trigger_external_mouse_warp() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        state.registry.add(tracked_window(60, 12, "Firefox"));
        state.registry.add(tracked_window(61, 12, "Firefox"));
        {
            let ws = state.workspaces.get_mut(0);
            ws.tree.insert_with_rect(60, None, screen);
            ws.tree.insert_with_rect(61, Some(60), screen);
            ws.record_focus(60);
        }

        state.focus_window(61);
        assert_eq!(state.active_workspace().focused, Some(61));
        assert!(state.ffm_cooldown_until.is_none());

        state.adopt_external_app_focus(12, Some(61));

        assert_eq!(state.active_workspace().focused, Some(61));
        assert!(state.ffm_cooldown_until.is_none());
    }

    #[test]
    fn external_app_focus_prefers_visible_workspace_over_workspace_one() {
        // Regression: a stale focus_history entry for `pid` on workspace 1
        // (index 0) used to win over a candidate on the currently visible
        // workspace, yanking the WM back to ws1 in a feedback loop with the
        // 50ms frontmost-app poll.
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        let screen = state.monitors[0].usable_frame;

        state.registry.add(tracked_window(70, 13, "WezTerm"));
        state.registry.add(tracked_window(71, 13, "WezTerm"));
        state
            .workspaces
            .get_or_create_target(&WorkspaceTarget::Numbered(2));

        // Stale history on ws1 for pid 13.
        {
            let ws = state.workspaces.get_mut(0);
            ws.tree.insert_with_rect(70, None, screen);
            ws.record_focus(70);
        }
        // Currently visible ws2 also has a window for pid 13.
        {
            let ws = state.workspaces.get_mut(1);
            ws.tree.insert_with_rect(71, None, screen);
            ws.record_focus(71);
        }

        // Make ws2 the visible workspace.
        state.monitors[0].active_workspace = 1;
        state.sync_workspace_visibility();

        state.adopt_external_app_focus(13, None);

        // Resolver must prefer the ws2 window over the stale ws1 entry.
        assert_eq!(state.monitors[0].active_workspace, 1);
        assert_eq!(state.active_workspace().focused, Some(71));
    }

    #[test]
    fn external_app_focus_dismisses_overlay_before_switching() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        let special_idx = state.workspaces.special_index("messages");
        state.active_specials = vec![Some(special_idx)];
        state.sync_workspace_visibility();

        state.registry.add(tracked_window(40, 9, "Messages"));
        state
            .workspaces
            .get_or_create_target(&WorkspaceTarget::Numbered(2));
        {
            let ws = state.workspaces.get_mut(1);
            ws.tree.insert_with_rect(40, None, screen);
            ws.record_focus(40);
        }

        state.adopt_external_app_focus(9, None);

        assert_eq!(state.active_specials, vec![None]);
        assert_eq!(state.monitors[0].active_workspace, 1);
        assert_eq!(state.active_workspace().focused, Some(40));
    }

    #[test]
    fn promote_stack_focused_moves_active_window_to_top() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        {
            let ws = state.workspaces.get_mut(0);
            ws.tree.insert_with_rect(1, None, screen);
            ws.tree.insert_with_rect(2, Some(1), screen);
            ws.tree.insert_with_rect(3, Some(2), screen);
            assert!(ws.tree.make_stack_for_window(3));
            ws.tree.set_stack_active(3);
            ws.record_focus(3);
        }

        state.focus_direction(Direction::Left);
        assert_eq!(state.active_workspace().focused, Some(2));

        state.promote_stack_focused();

        assert_eq!(state.active_workspace().focused, Some(2));
        assert_eq!(
            state.active_workspace().tree.stack_info(2),
            Some((vec![2, 3], 0))
        );
    }

    #[test]
    fn proactive_overflow_normalize_collapses_workspace_with_sub_threshold_tiles() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 1,
            frame: Rect::new(0.0, 0.0, 1728.0, 1084.0),
            usable_frame: Rect::new(0.0, 33.0, 1728.0, 1051.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        // Insert 17 windows into ws0's tree, mirroring the disaster log
        // (workspace 1 received all discovered windows after restart).
        for wid in 1u32..=17 {
            state.registry.add(tracked_window(wid, 100, "Ghost"));
            state
                .workspaces
                .get_mut(0)
                .tree
                .insert_with_rect(wid, None, screen);
        }
        // Sanity: with 17 windows in BSP, at least one tile is sub-threshold.
        let geoms = state.workspaces.get(0).tree.calculate_geometries(screen);
        assert!(geoms.len() > 1, "tree must produce multiple tiles");
        let smallest_w = geoms
            .iter()
            .map(|(_, r)| r.width)
            .fold(f64::INFINITY, f64::min);
        let smallest_h = geoms
            .iter()
            .map(|(_, r)| r.height)
            .fold(f64::INFINITY, f64::min);
        assert!(
            smallest_w < 150.0 || smallest_h < 100.0,
            "test setup failed: smallest tile {smallest_w}x{smallest_h} should be sub-threshold"
        );

        state.proactive_overflow_normalize();

        // After normalization the tree should be a single root stack — only
        // one entry in focus_geometries.
        let after = state.workspace_focus_geometries(0, screen);
        assert_eq!(
            after.len(),
            1,
            "workspace should collapse to a single tile, got {after:?}"
        );
    }

    #[test]
    fn proactive_overflow_normalize_leaves_small_workspaces_alone() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 1,
            frame: Rect::new(0.0, 0.0, 1728.0, 1084.0),
            usable_frame: Rect::new(0.0, 33.0, 1728.0, 1051.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        // 4 windows on a normal-size monitor → roughly quarter tiles
        // (~864×525 each), well above the threshold. Must not collapse.
        for wid in 1u32..=4 {
            state.registry.add(tracked_window(wid, 100, "Ghost"));
            state
                .workspaces
                .get_mut(0)
                .tree
                .insert_with_rect(wid, None, screen);
        }
        let before_geoms = state.workspaces.get(0).tree.calculate_geometries(screen);
        let before_count = before_geoms.len();

        state.proactive_overflow_normalize();

        let after_count = state.workspace_focus_geometries(0, screen).len();
        assert_eq!(
            after_count, before_count,
            "small workspace should not collapse: had {before_count} tiles, now {after_count}"
        );
    }

    #[test]
    fn rate_limit_external_focus_enters_quiet_after_threshold_burst() {
        let mut state = WmState::new();
        // First THRESHOLD calls inside the sliding window must all pass
        // through (return false = "do not drop").
        for i in 0..EXT_FOCUS_STORM_THRESHOLD as u32 {
            assert!(
                !state.rate_limit_external_focus(i),
                "call {i} should not be rate-limited"
            );
        }
        // The next call exceeds threshold and trips quiet mode.
        assert!(state.rate_limit_external_focus(999));
        assert!(state.ext_focus_quiet_until.is_some());
        assert_eq!(state.ext_focus_pending_target, Some(999));
        // Storm is cleared so the next ticks won't trip immediately on stale
        // history once quiet expires.
        assert!(state.ext_focus_history.is_empty());

        // While in quiet, subsequent adoptions should be dropped and update
        // the pending target to whichever id arrived most recently.
        assert!(state.rate_limit_external_focus(1234));
        assert_eq!(state.ext_focus_pending_target, Some(1234));
    }

    #[test]
    fn rate_limit_external_focus_resumes_after_quiet_expires() {
        let mut state = WmState::new();
        // Trip the storm.
        for i in 0..=EXT_FOCUS_STORM_THRESHOLD as u32 {
            state.rate_limit_external_focus(i);
        }
        assert!(state.ext_focus_quiet_until.is_some());

        // Force quiet to "already expired" without sleeping.
        state.ext_focus_quiet_until =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(10));

        // Next call should fall through, clear quiet, and proceed.
        assert!(!state.rate_limit_external_focus(42));
        assert!(state.ext_focus_quiet_until.is_none());
        assert!(state.ext_focus_pending_target.is_none());
    }

    #[test]
    fn drain_pending_ext_focus_noop_when_quiet_not_set() {
        let mut state = WmState::new();
        state.ext_focus_pending_target = Some(7); // stale (shouldn't be applied)
        state.drain_pending_ext_focus();
        // No quiet armed → nothing drained, pending preserved untouched.
        assert_eq!(state.ext_focus_pending_target, Some(7));
    }

    #[test]
    fn drain_pending_ext_focus_holds_until_quiet_expires() {
        let mut state = WmState::new();
        state.ext_focus_pending_target = Some(7);
        state.ext_focus_quiet_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
        state.drain_pending_ext_focus();
        // Quiet still active → no drain.
        assert_eq!(state.ext_focus_pending_target, Some(7));
        assert!(state.ext_focus_quiet_until.is_some());
    }

    #[test]
    fn drain_pending_ext_focus_consumes_target_after_expiry() {
        let mut state = WmState::new();
        // Pending target points at a window that won't resolve in workspaces
        // (registry is empty), so adopt_external_focus early-returns; we
        // just care that drain consumes pending and clears quiet state.
        state.ext_focus_pending_target = Some(7);
        state.ext_focus_quiet_until =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
        state.drain_pending_ext_focus();
        assert_eq!(state.ext_focus_pending_target, None);
        assert!(state.ext_focus_quiet_until.is_none());
    }

    #[test]
    fn on_window_closed_returns_false_when_wid_not_in_registry() {
        // Establishes the bool contract used by main.rs to gate IPC publishes:
        // an unknown wid must report "did not reap" so the WindowClosed event
        // is not emitted to subscribers for a window we never tracked.
        let mut state = WmState::new();
        assert!(!state.on_window_closed(12345));
    }

    #[test]
    fn on_window_closed_reaps_when_app_lookup_fails() {
        // app_is_hidden returns false for pids that don't resolve to a running
        // NSRunningApplication, so a window owned by a non-existent pid (e.g.
        // app already terminated) must be reaped normally — not suppressed.
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        // PID -1 cannot exist as a real process; NSRunningApplication lookup
        // returns nil, so app_is_hidden falls through to false.
        state.registry.add(tracked_window(7777, -1, "Ghost"));
        state
            .workspaces
            .get_mut(0)
            .tree
            .insert_with_rect(7777, None, state.monitors[0].usable_frame);

        assert!(state.on_window_closed(7777));
        assert!(!state.registry.contains(7777));
    }

    /// Shared HX-05 fixture: single 1920x1080 monitor with 2 windows
    /// (Vertical split, 1 on left, 2 on right) and focus on 1.
    fn make_focal_test_state() -> WmState {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 1,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.mouse_follows_focus = false;
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;
        for wid in 1u32..=2 {
            state.registry.add(tracked_window(wid, 100, "Ghost"));
        }
        let ws = state.workspaces.get_mut(0);
        ws.tree.insert_with_rect(1, None, screen);
        ws.tree.insert_with_rect(2, Some(1), screen);
        ws.focused = Some(1);
        state
    }

    #[test]
    fn compute_focal_point_cursor_inside_uses_cursor() {
        let mut state = make_focal_test_state();
        state.focal_point_source = "cursor".to_string();
        state.last_cursor = Some((1500.0, 540.0));
        let focal = state.compute_focal_point_for_new_window();
        assert_eq!(focal, (1500.0, 540.0));
    }

    #[test]
    fn compute_focal_point_cursor_outside_workspace_falls_back_to_active() {
        let mut state = make_focal_test_state();
        state.focal_point_source = "cursor_or_active".to_string();
        // Cursor on a phantom second monitor.
        state.last_cursor = Some((9999.0, 9999.0));
        let focal = state.compute_focal_point_for_new_window();
        // Active focused = 1; its rect is (0,0,960,1080) → center (480, 540).
        assert_eq!(focal, (480.0, 540.0));
    }

    #[test]
    fn compute_focal_point_active_mode_ignores_cursor() {
        let mut state = make_focal_test_state();
        state.focal_point_source = "active".to_string();
        state.last_cursor = Some((1500.0, 540.0)); // would be used by cursor mode
        let focal = state.compute_focal_point_for_new_window();
        // "active" must use focused window's center (480, 540), not cursor.
        assert_eq!(focal, (480.0, 540.0));
    }

    #[test]
    fn compute_focal_point_consumes_preselect_when_set() {
        let mut state = make_focal_test_state();
        state.permanent_direction_override = false;
        // Preselect Right: focal should be 1px past the right edge of 1's tile.
        state.workspaces.get_mut(0).preselect = Some((Direction::Right, false));
        let focal = state.compute_focal_point_for_new_window();
        // 1's rect is (0,0,960,1080); right edge focal = (961, 540).
        assert_eq!(focal, (961.0, 540.0));
        // Single-use preselect is cleared after consumption.
        assert!(state.workspaces.get(0).preselect.is_none());
    }

    #[test]
    fn compute_focal_point_persistent_preselect_is_restored() {
        let mut state = make_focal_test_state();
        state.workspaces.get_mut(0).preselect = Some((Direction::Down, true));
        let _ = state.compute_focal_point_for_new_window();
        assert_eq!(
            state.workspaces.get(0).preselect,
            Some((Direction::Down, true))
        );
        // And calling again still applies it.
        let focal2 = state.compute_focal_point_for_new_window();
        // 1's rect is (0,0,960,1080); down edge focal = (480, 1081).
        assert_eq!(focal2, (480.0, 1081.0));
    }

    // --- HX-08 rate-limiter + drag-prep tests ---

    #[test]
    fn ax_write_rate_limiter_throttles_within_budget() {
        let state = WmState::new();
        // First call wins (no prior timestamp); records "now".
        assert!(!state.should_throttle_ax_write(42));
        // Immediate second call is within the 16ms budget → throttle.
        assert!(state.should_throttle_ax_write(42));
    }

    #[test]
    fn ax_write_rate_limiter_allows_after_budget() {
        let state = WmState::new();
        // Plant a stale timestamp >16ms in the past.
        state.ax_write_last.borrow_mut().insert(
            42,
            std::time::Instant::now() - std::time::Duration::from_millis(50),
        );
        // Budget has elapsed → call passes through.
        assert!(!state.should_throttle_ax_write(42));
    }

    #[test]
    fn ax_write_rate_limiter_is_per_window() {
        let state = WmState::new();
        // Window 42 burns its slot.
        assert!(!state.should_throttle_ax_write(42));
        assert!(state.should_throttle_ax_write(42));
        // Window 7 has its own slot — independent.
        assert!(!state.should_throttle_ax_write(7));
    }

    #[test]
    fn drag_focus_suppression_active_blocks_then_clears() {
        let mut state = WmState::new();
        assert!(!state.is_in_drag_focus_suppression());
        state.arm_drag_focus_suppression();
        assert!(state.is_in_drag_focus_suppression());
        // Force the deadline into the past — next call clears it.
        state.drag_suppress_focus_until =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
        assert!(!state.is_in_drag_focus_suppression());
        assert!(state.drag_suppress_focus_until.is_none());
    }

    #[test]
    fn begin_move_drag_on_tiled_hit_sets_threshold_active() {
        let mut state = make_focal_test_state();
        // Window 1 lives in the left half (x=0..960). Hit it at (100, 100).
        state.begin_move_drag(100.0, 100.0);
        assert!(
            state.is_dragging(),
            "begin_move_drag on tiled hit must stage a DragState"
        );
        assert!(state.drag_threshold_active);
        assert!(state.drag_suppress_focus_until.is_some());
    }

    #[test]
    fn update_drag_below_threshold_is_dormant() {
        let mut state = make_focal_test_state();
        state.begin_move_drag(100.0, 100.0);
        assert!(state.drag_threshold_active);
        // Move 3 px — below the default 5 px threshold.
        state.update_drag(103.0, 100.0);
        assert!(
            state.drag_threshold_active,
            "below-threshold delta must keep the gate active"
        );
    }

    #[test]
    fn update_drag_above_threshold_clears_active() {
        let mut state = make_focal_test_state();
        state.begin_move_drag(100.0, 100.0);
        // Move 10 px — past the default 5 px threshold.
        state.update_drag(110.0, 100.0);
        assert!(
            !state.drag_threshold_active,
            "above-threshold delta must clear the gate"
        );
    }

    #[test]
    fn end_drag_clears_threshold_and_suppression() {
        let mut state = make_focal_test_state();
        state.begin_move_drag(100.0, 100.0);
        assert!(state.drag_threshold_active);
        assert!(state.drag_suppress_focus_until.is_some());
        // Drop coord doesn't matter for the below-threshold drag — the
        // window never floated. End just clears the gates.
        state.end_drag(100.0, 100.0);
        assert!(!state.drag_threshold_active);
        assert!(state.drag_suppress_focus_until.is_none());
        assert!(!state.is_dragging());
    }

    // --- HX-09 drag-to-tile tests ---

    #[test]
    fn drag_threshold_cross_floats_tiled_window() {
        let mut state = make_focal_test_state();
        // Window 1 lives at (0,0,960,1080). Drag from inside its tile.
        state.begin_move_drag(100.0, 100.0);
        assert!(state.drag_threshold_active);
        assert!(state.workspaces.get(0).tree.contains(1));
        assert!(state.workspaces.get(0).floating.is_empty());

        // Move past threshold.
        state.update_drag(200.0, 100.0);
        assert!(!state.drag_threshold_active);
        assert_eq!(
            state.drag.map(|d| d.mode),
            Some(DragMode::TiledRelocate),
            "mode should be TiledRelocate"
        );
        // Tree no longer has window 1.
        assert!(!state.workspaces.get(0).tree.contains(1));
        // Floating list has it.
        assert!(state.workspaces.get(0).floating.iter().any(|f| f.id == 1));
    }

    #[test]
    fn drag_drop_on_tile_retiles_at_focal() {
        let mut state = make_focal_test_state();
        state.begin_move_drag(100.0, 100.0);
        state.update_drag(200.0, 100.0);
        // After removal of 1, the tree collapses to Leaf{2} covering
        // the whole workspace. Drop at (1500, 540) hits leaf 2.
        state.end_drag(1500.0, 540.0);
        // Window 1 is back in the tree.
        assert!(state.workspaces.get(0).tree.contains(1));
        // No longer in floating.
        assert!(state.workspaces.get(0).floating.is_empty());
        // Drag state cleared.
        assert!(!state.is_dragging());
    }

    #[test]
    fn drag_drop_off_monitor_keeps_floating() {
        let mut state = make_focal_test_state();
        state.begin_move_drag(100.0, 100.0);
        state.update_drag(200.0, 100.0);
        // Drop way outside the workspace rect.
        state.end_drag(-1000.0, -1000.0);
        // Window 1 stays floating.
        assert!(state.workspaces.get(0).floating.iter().any(|f| f.id == 1));
        assert!(!state.workspaces.get(0).tree.contains(1));
        assert!(!state.is_dragging());
    }

    // --- HX-06 pseudo-tile tests ---

    #[test]
    fn toggle_pseudo_inserts_then_removes() {
        let mut state = make_focal_test_state();
        state.workspaces.get_mut(0).focused = Some(1);
        assert!(!state.is_pseudo(1));
        state.toggle_pseudo();
        assert!(state.is_pseudo(1));
        state.toggle_pseudo();
        assert!(!state.is_pseudo(1));
    }

    #[test]
    fn toggle_pseudo_no_op_for_stack_member() {
        let mut state = make_focal_test_state();
        let screen = state.monitors[0].usable_frame;
        // Collapse the 2-window tree to a Stack so window 1 lives inside it.
        let ws = state.workspaces.get_mut(0);
        assert!(ws.tree.collapse_to_root_stack(1));
        ws.focused = Some(1);
        let _ = screen;
        state.toggle_pseudo();
        // Stack members must not be added to the pseudo set in v1.
        assert!(!state.is_pseudo(1));
    }

    #[test]
    fn pseudo_pruned_on_window_close() {
        let mut state = make_focal_test_state();
        state.workspaces.get_mut(0).focused = Some(1);
        state.toggle_pseudo();
        assert!(state.is_pseudo(1));
        let reaped = state.on_window_closed(1);
        assert!(reaped, "window-close should reap a tracked window");
        assert!(!state.is_pseudo(1), "pseudo entry must be cleaned up");
    }

    /// Shared HX-04 fixture: single 1920x1080 monitor with 3 windows in a
    /// Vertical-then-nested-Horizontal layout. Tree shape:
    ///   Internal{V, Leaf{1}, Internal{H, Leaf{2}, Leaf{3}}}
    /// Geometries: 1 = left half, 2 = top-right quad, 3 = bottom-right quad.
    fn make_three_window_state() -> WmState {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 1,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            is_primary: true,
            active_workspace: 0,
        }];
        // Disable mouse_follows_focus so move_direction skips the AX warp.
        state.mouse_follows_focus = false;
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;
        for wid in 1u32..=3 {
            state.registry.add(tracked_window(wid, 100, "Ghost"));
        }
        let ws = state.workspaces.get_mut(0);
        ws.tree.insert_with_rect(1, None, screen);
        ws.tree.insert_with_rect(2, Some(1), screen);
        ws.tree.insert_with_rect(3, Some(2), screen);
        state
    }

    #[test]
    fn move_direction_relocates_to_target_leaf() {
        // Focus window 1 (left half). Move Right computes focal at
        // (961, 540), which falls in window 3's tile (bottom-right
        // quad). The relocate path removes 1 and re-inserts next to 3.
        let mut state = make_three_window_state();
        state.workspaces.get_mut(0).focused = Some(1);
        let screen = state.monitors[0].usable_frame;

        let pre = state.workspaces.get(0).tree.calculate_geometries(screen);
        let pre_one = pre.iter().find(|(w, _)| *w == 1).unwrap().1;
        assert_eq!(pre_one.x, 0.0, "test setup: 1 starts on the left");

        state.move_direction(Direction::Right);

        let post = state.workspaces.get(0).tree.calculate_geometries(screen);
        let post_one = post.iter().find(|(w, _)| *w == 1).unwrap().1;
        // After Move Right, 1's tile lives in the right half (x >= 960).
        assert!(
            post_one.x >= 960.0,
            "window 1 should have relocated to the right half, got x={}",
            post_one.x
        );
        // All 3 windows remain in the tree.
        assert_eq!(post.len(), 3);
    }

    #[test]
    fn move_direction_no_op_with_single_window() {
        // Single window fills the workspace; any focal point lands
        // outside sr. With monitors.len()==1 the cross-monitor branch
        // returns early. Tree shape must be unchanged.
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 1,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.mouse_follows_focus = false;
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;
        state.registry.add(tracked_window(7, 100, "Solo"));
        state
            .workspaces
            .get_mut(0)
            .tree
            .insert_with_rect(7, None, screen);
        state.workspaces.get_mut(0).focused = Some(7);

        let pre = state.workspaces.get(0).tree.clone();
        for dir in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            state.move_direction(dir);
        }
        let post = state.workspaces.get(0).tree.clone();
        assert_eq!(pre, post, "single-window tree must be unchanged");
    }

    #[test]
    fn move_direction_no_op_at_edge_when_fallback_disabled() {
        // Window 1 already occupies the left half; pressing Move Left
        // pushes the focal to (-1, 540), outside the workspace. With
        // window_direction_monitor_fallback disabled the action no-ops.
        let mut state = make_three_window_state();
        state.workspaces.get_mut(0).focused = Some(1);
        state.window_direction_monitor_fallback = false;
        let screen = state.monitors[0].usable_frame;

        let pre = state.workspaces.get(0).tree.clone();
        state.move_direction(Direction::Left);
        let post = state.workspaces.get(0).tree.clone();
        assert_eq!(pre, post, "tree must be unchanged at edge w/o fallback");

        // Sanity: with a SINGLE monitor the fallback also no-ops even
        // when the setting is enabled, but tree must still be unchanged.
        state.window_direction_monitor_fallback = true;
        state.move_direction(Direction::Left);
        let post2 = state.workspaces.get(0).tree.clone();
        assert_eq!(pre, post2);

        // And verify the workspace rect was used correctly (no crash).
        let _ = state.workspaces.get(0).tree.calculate_geometries(screen);
    }

    #[test]
    fn unstack_focused_restores_original_tree() {
        let mut state = WmState::new();
        state.monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            usable_frame: Rect::new(0.0, 33.0, 1920.0, 1047.0),
            is_primary: true,
            active_workspace: 0,
        }];
        state.sync_workspace_visibility();
        let screen = state.monitors[0].usable_frame;

        let original = {
            let ws = state.workspaces.get_mut(0);
            ws.tree.insert_with_rect(1, None, screen);
            ws.tree.insert_with_rect(2, Some(1), screen);
            ws.tree.insert_with_rect(3, Some(2), screen);
            let original = ws.tree.clone();
            assert!(ws.tree.make_stack_for_window(3));
            ws.tree.set_stack_active(3);
            ws.record_focus(3);
            original
        };

        state.unstack_focused();
        assert_eq!(state.active_workspace().tree, original);
    }
}
