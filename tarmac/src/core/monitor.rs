use super::tree::{Direction, Node, Rect};

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

/// Next monitor index in position order (no wrapping — returns None at rightmost edge).
pub fn next_index_nowrap(monitors: &[Monitor], from: usize) -> Option<usize> {
    let sorted = sorted_indices(monitors);
    let pos = sorted.iter().position(|&i| i == from).unwrap_or(0);
    if pos + 1 < sorted.len() {
        Some(sorted[pos + 1])
    } else {
        None
    }
}

/// Previous monitor index in position order (no wrapping — returns None at leftmost edge).
pub fn prev_index_nowrap(monitors: &[Monitor], from: usize) -> Option<usize> {
    let sorted = sorted_indices(monitors);
    let pos = sorted.iter().position(|&i| i == from).unwrap_or(0);
    if pos > 0 { Some(sorted[pos - 1]) } else { None }
}

/// Find which monitor index contains a screen point.
pub fn index_at_point(monitors: &[Monitor], x: f64, y: f64) -> Option<usize> {
    monitors.iter().position(|m| m.frame.contains_point(x, y))
}

/// Find the nearest monitor in a geographic direction from `from`.
/// Returns None when no monitor lies in that direction.
///
/// Left/Right are equivalent to `prev_index_nowrap`/`next_index_nowrap`
/// for typical horizontal layouts, but using true geographic adjacency
/// (rather than pure x-sort) means a vertical monitor stack works: Up
/// from a laptop yields the monitor above it instead of None.
///
/// Reuses `Node::adjacent_distance` for the scoring so monitor-level
/// adjacency picks the same way window-level focus does (rect overlap
/// is preferred over center-to-center distance, vertical/horizontal gap
/// is the secondary key).
pub fn next_in_direction(
    monitors: &[Monitor],
    from: usize,
    direction: Direction,
) -> Option<usize> {
    // Strict edge test rather than center comparison: a candidate
    // counts as "in direction X" only when its leading edge in X has
    // passed `from`'s trailing edge in X (modulo a small tolerance for
    // floating-point alignment between adjacent display rects).
    // Center comparison fails for side-by-side monitors with different
    // heights — the rightmost's center y can be below the leftmost's
    // center y purely from height difference, giving a false "Down" hit.
    const TOL: f64 = 1.0;
    let from_rect = monitors.get(from)?.frame;

    monitors
        .iter()
        .enumerate()
        .filter(|(i, m)| {
            if *i == from {
                return false;
            }
            let r = &m.frame;
            match direction {
                Direction::Left => r.x + r.width <= from_rect.x + TOL,
                Direction::Right => r.x >= from_rect.x + from_rect.width - TOL,
                Direction::Up => r.y + r.height <= from_rect.y + TOL,
                Direction::Down => r.y >= from_rect.y + from_rect.height - TOL,
            }
        })
        .min_by(|(_, a), (_, b)| {
            let da = Node::adjacent_distance(&from_rect, &a.frame, direction);
            let db = Node::adjacent_distance(&from_rect, &b.frame, direction);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
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
    fn next_prev_nowrap() {
        let monitors = make_monitors();
        assert_eq!(next_index_nowrap(&monitors, 0), Some(1));
        assert_eq!(next_index_nowrap(&monitors, 1), None); // rightmost edge
        assert_eq!(prev_index_nowrap(&monitors, 0), None); // leftmost edge
        assert_eq!(prev_index_nowrap(&monitors, 1), Some(0));
    }

    #[test]
    fn single_monitor_nowrap() {
        let monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 2048.0, 1326.0),
            usable_frame: Rect::new(0.0, 40.0, 2048.0, 1286.0),
            is_primary: true,
            active_workspace: 0,
        }];
        assert_eq!(next_index_nowrap(&monitors, 0), None);
        assert_eq!(prev_index_nowrap(&monitors, 0), None);
    }

    #[test]
    fn next_in_direction_horizontal() {
        let monitors = make_monitors();
        assert_eq!(
            next_in_direction(&monitors, 0, Direction::Right),
            Some(1)
        );
        assert_eq!(next_in_direction(&monitors, 1, Direction::Left), Some(0));
        // No monitor to the left of the leftmost.
        assert_eq!(next_in_direction(&monitors, 0, Direction::Left), None);
        // No monitor to the right of the rightmost.
        assert_eq!(next_in_direction(&monitors, 1, Direction::Right), None);
        // No monitor above or below either in a horizontal stack.
        assert_eq!(next_in_direction(&monitors, 0, Direction::Up), None);
        assert_eq!(next_in_direction(&monitors, 0, Direction::Down), None);
    }

    #[test]
    fn next_in_direction_vertical_stack() {
        // Laptop monitor below, external monitor above.
        let monitors = vec![
            Monitor {
                id: 1,
                frame: Rect::new(0.0, 1080.0, 1920.0, 1080.0),
                usable_frame: Rect::new(0.0, 1105.0, 1920.0, 1055.0),
                is_primary: true,
                active_workspace: 0,
            },
            Monitor {
                id: 2,
                frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                usable_frame: Rect::new(0.0, 25.0, 1920.0, 1055.0),
                is_primary: false,
                active_workspace: 1,
            },
        ];
        assert_eq!(next_in_direction(&monitors, 0, Direction::Up), Some(1));
        assert_eq!(next_in_direction(&monitors, 1, Direction::Down), Some(0));
        assert_eq!(next_in_direction(&monitors, 0, Direction::Down), None);
        assert_eq!(next_in_direction(&monitors, 1, Direction::Up), None);
        // No monitor left/right of either in a pure vertical stack.
        assert_eq!(next_in_direction(&monitors, 0, Direction::Left), None);
    }

    #[test]
    fn next_in_direction_single_monitor_all_none() {
        let monitors = vec![Monitor {
            id: 42,
            frame: Rect::new(0.0, 0.0, 2048.0, 1326.0),
            usable_frame: Rect::new(0.0, 40.0, 2048.0, 1286.0),
            is_primary: true,
            active_workspace: 0,
        }];
        for dir in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            assert_eq!(next_in_direction(&monitors, 0, dir), None);
        }
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
