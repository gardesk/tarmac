use std::fmt;

use super::tree::Node;
use super::window::WindowId;

/// Workspace identifier — numbered (1-10), lettered (A-Z), or special (scratchpads).
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum WorkspaceId {
    Numbered(u8),
    Lettered(char),
    Special(String),
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            WorkspaceId::Numbered(n) => write!(f, "{}", n),
            WorkspaceId::Lettered(c) => write!(f, "{}", c),
            WorkspaceId::Special(name) => write!(f, "special:{}", name),
        }
    }
}

/// A floating window with its own geometry, not managed by the BSP tree.
#[derive(Debug, Clone)]
pub struct FloatingWindow {
    pub id: WindowId,
    pub geometry: super::tree::Rect,
}

/// A single workspace with its own BSP tree, floating windows, and focus state.
pub struct Workspace {
    pub id: WorkspaceId,
    pub tree: Node,
    pub floating: Vec<FloatingWindow>,
    pub focused: Option<WindowId>,
    pub focus_history: Vec<WindowId>,
    /// Which monitor index this workspace was last displayed on.
    pub last_monitor: Option<usize>,
    /// Whether this workspace is currently visible on some monitor.
    pub visible: bool,
}

impl Workspace {
    pub fn new(id: WorkspaceId) -> Self {
        Self {
            id,
            tree: Node::empty(),
            floating: Vec::new(),
            focused: None,
            focus_history: Vec::new(),
            last_monitor: None,
            visible: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tree.is_empty() && self.floating.is_empty()
    }

    pub fn all_window_ids(&self) -> Vec<WindowId> {
        let mut ids = self.tree.windows();
        ids.extend(self.floating.iter().map(|f| f.id));
        ids
    }

    pub fn is_floating(&self, id: WindowId) -> bool {
        self.floating.iter().any(|f| f.id == id)
    }

    /// Toggle a window between tiled and floating.
    pub fn toggle_float(&mut self, id: WindowId, screen_rect: super::tree::Rect) -> bool {
        if let Some(idx) = self.floating.iter().position(|f| f.id == id) {
            // Floating → tiled
            self.floating.remove(idx);
            self.tree.insert_with_rect(id, self.focused, screen_rect);
            true
        } else if self.tree.contains(id) {
            // Tiled → floating
            let geoms = self.tree.calculate_geometries(screen_rect);
            let geometry = geoms
                .iter()
                .find(|(w, _)| *w == id)
                .map(|(_, r)| *r)
                .unwrap_or(super::tree::Rect::new(100.0, 100.0, 800.0, 600.0));
            self.tree.remove(id);
            self.floating.push(FloatingWindow { id, geometry });
            true
        } else {
            false
        }
    }

    pub fn record_focus(&mut self, window_id: WindowId) {
        self.focus_history.retain(|id| *id != window_id);
        self.focus_history.push(window_id);
        self.focused = Some(window_id);
    }

    /// Pop the most recent focus and return the new focused window.
    pub fn pop_focus(&mut self) -> Option<WindowId> {
        self.focus_history.pop();
        self.focused = self.focus_history.last().copied();
        self.focused
    }
}

/// Manages all workspaces (flat vec, index 0 = workspace 1).
pub struct WorkspaceManager {
    workspaces: Vec<Workspace>,
}

impl WorkspaceManager {
    pub fn new() -> Self {
        // Pre-create 10 numbered workspaces
        let workspaces = (1..=10)
            .map(|n| Workspace::new(WorkspaceId::Numbered(n)))
            .collect();
        Self { workspaces }
    }

    pub fn get(&self, idx: usize) -> &Workspace {
        &self.workspaces[idx]
    }

    pub fn get_mut(&mut self, idx: usize) -> &mut Workspace {
        &mut self.workspaces[idx]
    }

    /// Get or create a workspace at the given index, growing the vec if needed.
    pub fn get_or_create(&mut self, idx: usize) -> &mut Workspace {
        while self.workspaces.len() <= idx {
            let n = (self.workspaces.len() + 1) as u8;
            self.workspaces.push(Workspace::new(WorkspaceId::Numbered(n)));
        }
        &mut self.workspaces[idx]
    }

    pub fn count(&self) -> usize {
        self.workspaces.len()
    }

    /// Find which workspace index contains a window (tiled or floating).
    pub fn find_window(&self, window_id: WindowId) -> Option<usize> {
        self.workspaces.iter().position(|ws| {
            ws.tree.contains(window_id) || ws.is_floating(window_id)
        })
    }

    /// Iterate all workspaces.
    pub fn iter(&self) -> impl Iterator<Item = &Workspace> {
        self.workspaces.iter()
    }
}

impl Default for WorkspaceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tree::Rect;

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };

    #[test]
    fn creates_10_workspaces() {
        let mgr = WorkspaceManager::new();
        assert_eq!(mgr.count(), 10);
        assert_eq!(mgr.get(0).id, WorkspaceId::Numbered(1));
        assert_eq!(mgr.get(9).id, WorkspaceId::Numbered(10));
    }

    #[test]
    fn get_or_create_grows() {
        let mut mgr = WorkspaceManager::new();
        let ws = mgr.get_or_create(15);
        assert_eq!(ws.id, WorkspaceId::Numbered(16));
        assert_eq!(mgr.count(), 16);
    }

    #[test]
    fn find_window() {
        let mut mgr = WorkspaceManager::new();
        mgr.get_mut(0).tree.insert(1, None);
        mgr.get_mut(2).tree.insert(2, None);
        assert_eq!(mgr.find_window(1), Some(0));
        assert_eq!(mgr.find_window(2), Some(2));
        assert_eq!(mgr.find_window(99), None);
    }

    #[test]
    fn focus_history() {
        let mut ws = Workspace::new(WorkspaceId::Numbered(1));
        ws.record_focus(1);
        ws.record_focus(2);
        ws.record_focus(3);
        assert_eq!(ws.focused, Some(3));

        ws.pop_focus();
        assert_eq!(ws.focused, Some(2));

        ws.pop_focus();
        assert_eq!(ws.focused, Some(1));
    }

    #[test]
    fn toggle_float_tiled_to_floating() {
        let mut ws = Workspace::new(WorkspaceId::Numbered(1));
        ws.tree.insert_with_rect(1, None, SCREEN);
        ws.tree.insert_with_rect(2, Some(1), SCREEN);
        ws.focused = Some(2);

        assert!(ws.toggle_float(2, SCREEN));
        assert!(!ws.tree.contains(2));
        assert!(ws.is_floating(2));
    }

    #[test]
    fn toggle_float_floating_to_tiled() {
        let mut ws = Workspace::new(WorkspaceId::Numbered(1));
        ws.tree.insert_with_rect(1, None, SCREEN);
        ws.tree.insert_with_rect(2, Some(1), SCREEN);
        ws.toggle_float(2, SCREEN);
        ws.toggle_float(2, SCREEN);
        assert!(!ws.is_floating(2));
        assert!(ws.tree.contains(2));
    }

    #[test]
    fn all_window_ids_includes_floating() {
        let mut ws = Workspace::new(WorkspaceId::Numbered(1));
        ws.tree.insert_with_rect(1, None, SCREEN);
        ws.tree.insert_with_rect(2, Some(1), SCREEN);
        ws.toggle_float(2, SCREEN);
        let mut ids = ws.all_window_ids();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn workspace_id_display() {
        assert_eq!(WorkspaceId::Numbered(3).to_string(), "3");
        assert_eq!(WorkspaceId::Lettered('W').to_string(), "W");
        assert_eq!(
            WorkspaceId::Special("scratch".to_string()).to_string(),
            "special:scratch"
        );
    }

    #[test]
    fn visible_and_last_monitor() {
        let mut ws = Workspace::new(WorkspaceId::Numbered(1));
        assert!(!ws.visible);
        assert!(ws.last_monitor.is_none());

        ws.visible = true;
        ws.last_monitor = Some(0);
        assert!(ws.visible);
        assert_eq!(ws.last_monitor, Some(0));
    }
}
