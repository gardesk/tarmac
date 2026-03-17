use std::collections::HashMap;

use crate::platform::accessibility::CGWindowID;

pub type WindowId = CGWindowID;

/// Tracked state for a managed window.
#[derive(Debug, Clone)]
pub struct WindowState {
    pub id: WindowId,
    pub app_pid: i32,
    pub app_name: String,
    pub app_bundle_id: String,
    pub title: String,
    pub role: String,
    pub subrole: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub floating: bool,
    pub minimized: bool,
}

/// Registry of all tracked windows.
#[derive(Debug)]
pub struct WindowRegistry {
    windows: HashMap<WindowId, WindowState>,
}

impl WindowRegistry {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
        }
    }

    pub fn add(&mut self, window: WindowState) {
        tracing::debug!(
            id = window.id,
            app = %window.app_name,
            title = %window.title,
            "window added"
        );
        self.windows.insert(window.id, window);
    }

    pub fn remove(&mut self, id: WindowId) -> Option<WindowState> {
        let removed = self.windows.remove(&id);
        if let Some(ref w) = removed {
            tracing::debug!(id = w.id, app = %w.app_name, title = %w.title, "window removed");
        }
        removed
    }

    pub fn get(&self, id: WindowId) -> Option<&WindowState> {
        self.windows.get(&id)
    }

    pub fn get_mut(&mut self, id: WindowId) -> Option<&mut WindowState> {
        self.windows.get_mut(&id)
    }

    pub fn contains(&self, id: WindowId) -> bool {
        self.windows.contains_key(&id)
    }

    pub fn count(&self) -> usize {
        self.windows.len()
    }

    pub fn all(&self) -> impl Iterator<Item = &WindowState> {
        self.windows.values()
    }

    /// Remove all windows belonging to a given app pid.
    pub fn remove_by_pid(&mut self, pid: i32) -> Vec<WindowState> {
        let ids: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, w)| w.app_pid == pid)
            .map(|(id, _)| *id)
            .collect();

        let mut removed = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(w) = self.windows.remove(&id) {
                tracing::debug!(id = w.id, app = %w.app_name, "window removed (app terminated)");
                removed.push(w);
            }
        }
        removed
    }

    /// Update position and size for a window.
    pub fn update_geometry(&mut self, id: WindowId, x: f64, y: f64, width: f64, height: f64) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.x = x;
            w.y = y;
            w.width = width;
            w.height = height;
        }
    }

    /// Update title for a window.
    pub fn update_title(&mut self, id: WindowId, title: String) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.title = title;
        }
    }
}

impl Default for WindowRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_window(id: WindowId, app: &str, title: &str) -> WindowState {
        WindowState {
            id,
            app_pid: 100,
            app_name: app.to_string(),
            app_bundle_id: format!("com.test.{}", app),
            title: title.to_string(),
            role: "AXWindow".to_string(),
            subrole: "AXStandardWindow".to_string(),
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
            floating: false,
            minimized: false,
        }
    }

    #[test]
    fn add_and_get() {
        let mut reg = WindowRegistry::new();
        reg.add(make_window(1, "Terminal", "bash"));
        assert_eq!(reg.count(), 1);
        assert!(reg.contains(1));
        assert_eq!(reg.get(1).unwrap().title, "bash");
    }

    #[test]
    fn remove() {
        let mut reg = WindowRegistry::new();
        reg.add(make_window(1, "Terminal", "bash"));
        let removed = reg.remove(1);
        assert!(removed.is_some());
        assert_eq!(reg.count(), 0);
    }

    #[test]
    fn remove_by_pid() {
        let mut reg = WindowRegistry::new();
        let mut w1 = make_window(1, "Terminal", "tab1");
        w1.app_pid = 200;
        let mut w2 = make_window(2, "Terminal", "tab2");
        w2.app_pid = 200;
        reg.add(w1);
        reg.add(w2);
        reg.add(make_window(3, "Safari", "web"));

        let removed = reg.remove_by_pid(200);
        assert_eq!(removed.len(), 2);
        assert_eq!(reg.count(), 1);
    }

    #[test]
    fn update_geometry() {
        let mut reg = WindowRegistry::new();
        reg.add(make_window(1, "Terminal", "bash"));
        reg.update_geometry(1, 100.0, 200.0, 1000.0, 700.0);
        let w = reg.get(1).unwrap();
        assert_eq!(w.x, 100.0);
        assert_eq!(w.y, 200.0);
        assert_eq!(w.width, 1000.0);
        assert_eq!(w.height, 700.0);
    }

    #[test]
    fn update_title() {
        let mut reg = WindowRegistry::new();
        reg.add(make_window(1, "Terminal", "bash"));
        reg.update_title(1, "zsh".to_string());
        assert_eq!(reg.get(1).unwrap().title, "zsh");
    }
}
