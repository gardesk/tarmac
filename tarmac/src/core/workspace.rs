use std::collections::HashMap;
use std::fmt;

use super::tree::{Node, Rect};
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

/// A single workspace with its own BSP tree and focus state.
pub struct Workspace {
    pub id: WorkspaceId,
    pub tree: Node,
    pub focused: Option<WindowId>,
    pub focus_history: Vec<WindowId>,
}

impl Workspace {
    pub fn new(id: WorkspaceId) -> Self {
        Self {
            id,
            tree: Node::empty(),
            focused: None,
            focus_history: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    pub fn all_window_ids(&self) -> Vec<WindowId> {
        self.tree.windows()
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

/// Describes what changes when switching workspaces.
pub struct WorkspaceTransition {
    pub hide: Vec<WindowId>,
    pub show: Vec<(WindowId, Rect)>,
    pub focus: Option<WindowId>,
}

/// Manages all workspaces and tracks which is active.
pub struct WorkspaceManager {
    workspaces: HashMap<WorkspaceId, Workspace>,
    active: WorkspaceId,
}

impl WorkspaceManager {
    pub fn new() -> Self {
        let default_ws = WorkspaceId::Numbered(1);
        let mut workspaces = HashMap::new();
        workspaces.insert(default_ws.clone(), Workspace::new(default_ws.clone()));

        Self {
            workspaces,
            active: default_ws,
        }
    }

    pub fn active_id(&self) -> &WorkspaceId {
        &self.active
    }

    pub fn active(&self) -> &Workspace {
        &self.workspaces[&self.active]
    }

    pub fn active_mut(&mut self) -> &mut Workspace {
        self.workspaces.get_mut(&self.active).unwrap()
    }

    pub fn get_or_create(&mut self, id: WorkspaceId) -> &mut Workspace {
        self.workspaces
            .entry(id.clone())
            .or_insert_with(|| Workspace::new(id))
    }

    /// Switch to a different workspace. Returns the transition to apply.
    pub fn switch_to(&mut self, target_id: WorkspaceId, screen_rect: Rect) -> WorkspaceTransition {
        if target_id == self.active {
            return WorkspaceTransition {
                hide: vec![],
                show: vec![],
                focus: self.active().focused,
            };
        }

        // Collect windows to hide from current workspace
        let hide = self.active().all_window_ids();

        // Activate target workspace
        let target = self.get_or_create(target_id.clone());
        let show = target.tree.calculate_geometries(screen_rect);
        let focus = target.focused.or_else(|| target.tree.first_window());

        self.active = target_id;

        WorkspaceTransition { hide, show, focus }
    }

    /// Move a window from the active workspace to a target workspace.
    /// Returns true if the window was moved.
    pub fn move_window_to(
        &mut self,
        window_id: WindowId,
        target_id: WorkspaceId,
        screen_rect: Rect,
    ) -> bool {
        if target_id == self.active {
            return false;
        }

        // Remove from active workspace
        let active = self.active_mut();
        if !active.tree.remove(window_id) {
            return false;
        }
        active.focus_history.retain(|id| *id != window_id);
        if active.focused == Some(window_id) {
            active.focused = active
                .focus_history
                .last()
                .copied()
                .or(active.tree.first_window());
        }

        // Insert into target workspace
        let target = self.get_or_create(target_id);
        target
            .tree
            .insert_with_rect(window_id, target.focused, screen_rect);

        true
    }

    /// Find which workspace contains a window.
    pub fn workspace_for_window(&self, window_id: WindowId) -> Option<&WorkspaceId> {
        self.workspaces
            .iter()
            .find(|(_, ws)| ws.tree.contains(window_id))
            .map(|(id, _)| id)
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

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };

    #[test]
    fn starts_on_workspace_1() {
        let mgr = WorkspaceManager::new();
        assert_eq!(*mgr.active_id(), WorkspaceId::Numbered(1));
    }

    #[test]
    fn switch_to_same_is_noop() {
        let mut mgr = WorkspaceManager::new();
        let t = mgr.switch_to(WorkspaceId::Numbered(1), SCREEN);
        assert!(t.hide.is_empty());
        assert!(t.show.is_empty());
    }

    #[test]
    fn switch_to_empty_workspace() {
        let mut mgr = WorkspaceManager::new();
        mgr.active_mut().tree.insert(1, None);
        let t = mgr.switch_to(WorkspaceId::Numbered(2), SCREEN);
        assert_eq!(t.hide, vec![1]);
        assert!(t.show.is_empty());
        assert_eq!(*mgr.active_id(), WorkspaceId::Numbered(2));
    }

    #[test]
    fn switch_back_restores() {
        let mut mgr = WorkspaceManager::new();
        mgr.active_mut().tree.insert(1, None);
        mgr.active_mut().focused = Some(1);

        mgr.switch_to(WorkspaceId::Numbered(2), SCREEN);

        let t = mgr.switch_to(WorkspaceId::Numbered(1), SCREEN);
        assert_eq!(t.show.len(), 1);
        assert_eq!(t.show[0].0, 1);
        assert_eq!(t.focus, Some(1));
    }

    #[test]
    fn move_window_to_other_workspace() {
        let mut mgr = WorkspaceManager::new();
        mgr.active_mut().tree.insert_with_rect(1, None, SCREEN);
        mgr.active_mut().tree.insert_with_rect(2, Some(1), SCREEN);
        mgr.active_mut().focused = Some(2);

        let moved = mgr.move_window_to(2, WorkspaceId::Numbered(3), SCREEN);
        assert!(moved);

        // Window 2 should be gone from workspace 1
        assert!(!mgr.active().tree.contains(2));
        assert_eq!(mgr.active().tree.window_count(), 1);

        // Window 2 should be on workspace 3
        let ws3 = &mgr.workspaces[&WorkspaceId::Numbered(3)];
        assert!(ws3.tree.contains(2));
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
    fn workspace_for_window() {
        let mut mgr = WorkspaceManager::new();
        mgr.active_mut().tree.insert(1, None);
        assert_eq!(mgr.workspace_for_window(1), Some(&WorkspaceId::Numbered(1)));
        assert_eq!(mgr.workspace_for_window(99), None);
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
}
