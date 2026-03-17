use super::tree::Rect;

pub type MonitorId = u32; // CGDirectDisplayID

/// A tracked display with its geometry and active workspace.
#[derive(Debug, Clone)]
pub struct Monitor {
    pub id: MonitorId,
    /// Full physical display bounds (includes dock + menu bar area)
    pub frame: Rect,
    /// Usable area (excludes dock + menu bar)
    pub usable_frame: Rect,
    pub is_primary: bool,
    /// Index of the workspace currently displayed on this monitor.
    pub active_workspace: usize,
}

/// Sort monitor indices left-to-right by x position.
pub fn sorted_indices(monitors: &[Monitor]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..monitors.len()).collect();
    indices.sort_by(|&a, &b| {
        monitors[a]
            .usable_frame
            .x
            .partial_cmp(&monitors[b].usable_frame.x)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    indices
}

/// Next monitor index in position order (wrapping).
pub fn next_index(monitors: &[Monitor], from: usize) -> usize {
    let sorted = sorted_indices(monitors);
    let pos = sorted.iter().position(|&i| i == from).unwrap_or(0);
    sorted[(pos + 1) % sorted.len()]
}

/// Previous monitor index in position order (wrapping).
pub fn prev_index(monitors: &[Monitor], from: usize) -> usize {
    let sorted = sorted_indices(monitors);
    let pos = sorted.iter().position(|&i| i == from).unwrap_or(0);
    sorted[if pos == 0 { sorted.len() - 1 } else { pos - 1 }]
}

/// Find which monitor index contains a screen point.
pub fn index_at_point(monitors: &[Monitor], x: f64, y: f64) -> Option<usize> {
    monitors
        .iter()
        .position(|m| m.frame.contains_point(x, y))
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
                active_workspace: 0,
            },
            Monitor {
                id: 2,
                frame: Rect::new(1920.0, 0.0, 2560.0, 1440.0),
                usable_frame: Rect::new(1920.0, 25.0, 2560.0, 1415.0),
                is_primary: false,
                active_workspace: 1,
            },
        ]
    }

    #[test]
    fn sorted_left_to_right() {
        let monitors = make_monitors();
        assert_eq!(sorted_indices(&monitors), vec![0, 1]);
    }

    #[test]
    fn next_prev_cycle() {
        let monitors = make_monitors();
        assert_eq!(next_index(&monitors, 0), 1);
        assert_eq!(next_index(&monitors, 1), 0); // wraps
        assert_eq!(prev_index(&monitors, 0), 1); // wraps
        assert_eq!(prev_index(&monitors, 1), 0);
    }

    #[test]
    fn point_lookup() {
        let monitors = make_monitors();
        assert_eq!(index_at_point(&monitors, 500.0, 500.0), Some(0));
        assert_eq!(index_at_point(&monitors, 2500.0, 500.0), Some(1));
        assert_eq!(index_at_point(&monitors, -100.0, 500.0), None);
    }

    #[test]
    fn single_monitor() {
        let monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 2048.0, 1326.0),
            usable_frame: Rect::new(0.0, 40.0, 2048.0, 1286.0),
            is_primary: true,
            active_workspace: 0,
        }];
        assert_eq!(next_index(&monitors, 0), 0); // wraps to self
        assert_eq!(prev_index(&monitors, 0), 0);
    }
}
