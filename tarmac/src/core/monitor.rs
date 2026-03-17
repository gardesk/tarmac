use super::tree::Rect;

pub type MonitorId = u32; // CGDirectDisplayID

/// A tracked display with its geometry.
#[derive(Debug, Clone)]
pub struct Monitor {
    pub id: MonitorId,
    /// Full physical display bounds (includes dock + menu bar area)
    pub frame: Rect,
    /// Usable area (excludes dock + menu bar)
    pub usable_frame: Rect,
    pub is_primary: bool,
}

/// Manages all connected displays.
#[derive(Debug)]
pub struct MonitorManager {
    monitors: Vec<Monitor>,
    pub focused: MonitorId,
}

impl MonitorManager {
    pub fn new() -> Self {
        Self {
            monitors: Vec::new(),
            focused: 0,
        }
    }

    pub fn set_monitors(&mut self, monitors: Vec<Monitor>) {
        if let Some(primary) = monitors.iter().find(|m| m.is_primary) {
            self.focused = primary.id;
        } else if let Some(first) = monitors.first() {
            self.focused = first.id;
        }
        self.monitors = monitors;
    }

    pub fn count(&self) -> usize {
        self.monitors.len()
    }

    pub fn get(&self, id: MonitorId) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.id == id)
    }

    pub fn focused_monitor(&self) -> Option<&Monitor> {
        self.get(self.focused)
    }

    pub fn all(&self) -> &[Monitor] {
        &self.monitors
    }

    /// Get monitors sorted left-to-right by x position.
    pub fn sorted_by_position(&self) -> Vec<&Monitor> {
        let mut sorted: Vec<&Monitor> = self.monitors.iter().collect();
        sorted.sort_by(|a, b| {
            a.usable_frame
                .x
                .partial_cmp(&b.usable_frame.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sorted
    }

    /// Get the next monitor in position order.
    pub fn next_monitor(&self, from: MonitorId) -> Option<MonitorId> {
        let sorted = self.sorted_by_position();
        let idx = sorted.iter().position(|m| m.id == from)?;
        let next_idx = (idx + 1) % sorted.len();
        Some(sorted[next_idx].id)
    }

    /// Get the previous monitor in position order.
    pub fn prev_monitor(&self, from: MonitorId) -> Option<MonitorId> {
        let sorted = self.sorted_by_position();
        let idx = sorted.iter().position(|m| m.id == from)?;
        let prev_idx = if idx == 0 { sorted.len() - 1 } else { idx - 1 };
        Some(sorted[prev_idx].id)
    }

    /// Find which monitor contains a screen point.
    pub fn monitor_at_point(&self, x: f64, y: f64) -> Option<MonitorId> {
        self.monitors
            .iter()
            .find(|m| m.frame.contains_point(x, y))
            .map(|m| m.id)
    }
}

impl Default for MonitorManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_monitors() -> Vec<Monitor> {
        vec![
            Monitor {
                id: 1,
                frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                usable_frame: Rect::new(0.0, 25.0, 1920.0, 1055.0),
                is_primary: true,
            },
            Monitor {
                id: 2,
                frame: Rect::new(1920.0, 0.0, 2560.0, 1440.0),
                usable_frame: Rect::new(1920.0, 25.0, 2560.0, 1415.0),
                is_primary: false,
            },
        ]
    }

    #[test]
    fn primary_is_focused_by_default() {
        let mut mgr = MonitorManager::new();
        mgr.set_monitors(make_monitors());
        assert_eq!(mgr.focused, 1);
    }

    #[test]
    fn next_prev_cycle() {
        let mut mgr = MonitorManager::new();
        mgr.set_monitors(make_monitors());
        assert_eq!(mgr.next_monitor(1), Some(2));
        assert_eq!(mgr.next_monitor(2), Some(1)); // wraps
        assert_eq!(mgr.prev_monitor(1), Some(2)); // wraps
        assert_eq!(mgr.prev_monitor(2), Some(1));
    }

    #[test]
    fn monitor_at_point() {
        let mut mgr = MonitorManager::new();
        mgr.set_monitors(make_monitors());
        assert_eq!(mgr.monitor_at_point(500.0, 500.0), Some(1));
        assert_eq!(mgr.monitor_at_point(2500.0, 500.0), Some(2));
        assert_eq!(mgr.monitor_at_point(-100.0, 500.0), None);
    }

    #[test]
    fn single_monitor() {
        let mut mgr = MonitorManager::new();
        mgr.set_monitors(vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 2048.0, 1326.0),
            usable_frame: Rect::new(0.0, 40.0, 2048.0, 1286.0),
            is_primary: true,
        }]);
        assert_eq!(mgr.count(), 1);
        assert_eq!(mgr.focused, 42);
        assert_eq!(mgr.next_monitor(42), Some(42)); // wraps to self
    }
}
