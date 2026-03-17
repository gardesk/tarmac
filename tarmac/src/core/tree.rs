use crate::core::window::WindowId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal, // Top/Bottom
    Vertical,   // Left/Right
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn split(&self, direction: SplitDirection, ratio: f32) -> (Rect, Rect) {
        let r = ratio as f64;
        match direction {
            SplitDirection::Vertical => {
                let split_x = self.width * r;
                (
                    Rect::new(self.x, self.y, split_x, self.height),
                    Rect::new(self.x + split_x, self.y, self.width - split_x, self.height),
                )
            }
            SplitDirection::Horizontal => {
                let split_y = self.height * r;
                (
                    Rect::new(self.x, self.y, self.width, split_y),
                    Rect::new(self.x, self.y + split_y, self.width, self.height - split_y),
                )
            }
        }
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn contains_point(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Internal {
        split: SplitDirection,
        ratio: f32,
        left: Box<Node>,
        right: Box<Node>,
    },
    Leaf {
        window: Option<WindowId>,
    },
}

impl Default for Node {
    fn default() -> Self {
        Self::empty()
    }
}

impl Node {
    pub fn empty() -> Self {
        Node::Leaf { window: None }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Node::Leaf { window: None })
    }

    pub fn window_count(&self) -> usize {
        match self {
            Node::Leaf { window: Some(_) } => 1,
            Node::Leaf { window: None } => 0,
            Node::Internal { left, right, .. } => left.window_count() + right.window_count(),
        }
    }

    pub fn windows(&self) -> Vec<WindowId> {
        match self {
            Node::Leaf { window: Some(w) } => vec![*w],
            Node::Leaf { window: None } => vec![],
            Node::Internal { left, right, .. } => {
                let mut ws = left.windows();
                ws.extend(right.windows());
                ws
            }
        }
    }

    pub fn contains(&self, window: WindowId) -> bool {
        match self {
            Node::Leaf { window: Some(w) } => *w == window,
            Node::Leaf { window: None } => false,
            Node::Internal { left, right, .. } => left.contains(window) || right.contains(window),
        }
    }

    pub fn first_window(&self) -> Option<WindowId> {
        match self {
            Node::Leaf { window } => *window,
            Node::Internal { left, right, .. } => {
                left.first_window().or_else(|| right.first_window())
            }
        }
    }

    /// Insert a window next to `target` (or at the end if target is None).
    /// Uses vertical split for now; Sprint 3 adds smart split.
    pub fn insert(&mut self, new_window: WindowId, target: Option<WindowId>) {
        self.insert_with_rect(new_window, target, Rect::new(0.0, 0.0, 1920.0, 1080.0));
    }

    /// Insert with known container rect for smart split calculation.
    pub fn insert_with_rect(&mut self, new_window: WindowId, target: Option<WindowId>, rect: Rect) {
        match self {
            Node::Leaf { window: None } => {
                *self = Node::Leaf {
                    window: Some(new_window),
                };
            }
            Node::Leaf {
                window: Some(existing),
            } => {
                // Default to vertical split; Sprint 3 will use smart_split_direction
                let direction = SplitDirection::Vertical;
                let existing_window = *existing;
                *self = Node::Internal {
                    split: direction,
                    ratio: 0.5,
                    left: Box::new(Node::Leaf {
                        window: Some(existing_window),
                    }),
                    right: Box::new(Node::Leaf {
                        window: Some(new_window),
                    }),
                };
            }
            Node::Internal {
                split,
                ratio,
                left,
                right,
            } => {
                let (left_rect, right_rect) = rect.split(*split, *ratio);

                if let Some(target) = target {
                    if left.contains(target) {
                        left.insert_with_rect(new_window, Some(target), left_rect);
                    } else if right.contains(target) {
                        right.insert_with_rect(new_window, Some(target), right_rect);
                    } else {
                        right.insert_with_rect(new_window, None, right_rect);
                    }
                } else {
                    right.insert_with_rect(new_window, None, right_rect);
                }
            }
        }
    }

    /// Remove a window from the tree, collapsing empty internal nodes.
    pub fn remove(&mut self, window: WindowId) -> bool {
        match self {
            Node::Leaf { window: Some(w) } if *w == window => {
                *self = Node::empty();
                true
            }
            Node::Leaf { .. } => false,
            Node::Internal { left, right, .. } => {
                if left.remove(window) {
                    if left.is_empty() {
                        *self = std::mem::take(right.as_mut());
                    }
                    true
                } else if right.remove(window) {
                    if right.is_empty() {
                        *self = std::mem::take(left.as_mut());
                    }
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Calculate geometries for all windows given a root rect.
    pub fn calculate_geometries(&self, rect: Rect) -> Vec<(WindowId, Rect)> {
        match self {
            Node::Leaf { window: Some(w) } => vec![(*w, rect)],
            Node::Leaf { window: None } => vec![],
            Node::Internal {
                split,
                ratio,
                left,
                right,
            } => {
                let (left_rect, right_rect) = rect.split(*split, *ratio);
                let mut geoms = left.calculate_geometries(left_rect);
                geoms.extend(right.calculate_geometries(right_rect));
                geoms
            }
        }
    }

    /// Equalize all split ratios to 0.5.
    pub fn equalize(&mut self) {
        if let Node::Internal {
            ratio, left, right, ..
        } = self
        {
            *ratio = 0.5;
            left.equalize();
            right.equalize();
        }
    }

    /// Swap two windows in the tree.
    pub fn swap(&mut self, a: WindowId, b: WindowId) -> bool {
        let mut found_a = false;
        let mut found_b = false;
        self.swap_impl(a, b, &mut found_a, &mut found_b);
        found_a && found_b
    }

    fn swap_impl(&mut self, a: WindowId, b: WindowId, found_a: &mut bool, found_b: &mut bool) {
        match self {
            Node::Leaf { window: Some(w) } => {
                if *w == a {
                    *w = b;
                    *found_a = true;
                } else if *w == b {
                    *w = a;
                    *found_b = true;
                }
            }
            Node::Leaf { window: None } => {}
            Node::Internal { left, right, .. } => {
                left.swap_impl(a, b, found_a, found_b);
                right.swap_impl(a, b, found_a, found_b);
            }
        }
    }

    /// Find the nearest window in a direction from a source window.
    pub fn find_adjacent(
        geometries: &[(WindowId, Rect)],
        from: WindowId,
        direction: Direction,
    ) -> Option<WindowId> {
        let from_rect = geometries.iter().find(|(w, _)| *w == from)?.1;
        let (from_cx, from_cy) = from_rect.center();

        geometries
            .iter()
            .filter(|(w, rect)| {
                if *w == from {
                    return false;
                }
                let (cx, cy) = rect.center();
                match direction {
                    Direction::Left => cx < from_cx,
                    Direction::Right => cx > from_cx,
                    Direction::Up => cy < from_cy,
                    Direction::Down => cy > from_cy,
                }
            })
            .min_by(|(_, a), (_, b)| {
                let (acx, acy) = a.center();
                let (bcx, bcy) = b.center();
                let dist_a = match direction {
                    Direction::Left | Direction::Right => {
                        (acy - from_cy).abs() * 100.0 + (acx - from_cx).abs()
                    }
                    Direction::Up | Direction::Down => {
                        (acx - from_cx).abs() * 100.0 + (acy - from_cy).abs()
                    }
                };
                let dist_b = match direction {
                    Direction::Left | Direction::Right => {
                        (bcy - from_cy).abs() * 100.0 + (bcx - from_cx).abs()
                    }
                    Direction::Up | Direction::Down => {
                        (bcx - from_cx).abs() * 100.0 + (bcy - from_cy).abs()
                    }
                };
                dist_a.partial_cmp(&dist_b).unwrap()
            })
            .map(|(w, _)| *w)
    }

    /// Resize the split affecting a window in the given direction.
    pub fn resize(&mut self, window: WindowId, direction: Direction, delta: f32) -> bool {
        match self {
            Node::Leaf { .. } => false,
            Node::Internal {
                split,
                ratio,
                left,
                right,
            } => {
                let matches = matches!(
                    (split, direction),
                    (SplitDirection::Vertical, Direction::Left | Direction::Right)
                        | (SplitDirection::Horizontal, Direction::Up | Direction::Down)
                );

                if matches && (left.contains(window) || right.contains(window)) {
                    let in_left = left.contains(window);
                    let adjustment = match direction {
                        Direction::Right | Direction::Down => {
                            if in_left {
                                delta
                            } else {
                                -delta
                            }
                        }
                        Direction::Left | Direction::Up => {
                            if in_left {
                                -delta
                            } else {
                                delta
                            }
                        }
                    };
                    *ratio = (*ratio + adjustment).clamp(0.1, 0.9);
                    return true;
                }

                if left.contains(window) {
                    left.resize(window, direction, delta)
                } else if right.contains(window) {
                    right.resize(window, direction, delta)
                } else {
                    false
                }
            }
        }
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

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    fn assert_rect_approx(r: &Rect, x: f64, y: f64, w: f64, h: f64) {
        assert!(
            approx_eq(r.x, x)
                && approx_eq(r.y, y)
                && approx_eq(r.width, w)
                && approx_eq(r.height, h),
            "expected ({x}, {y}, {w}, {h}), got ({}, {}, {}, {})",
            r.x,
            r.y,
            r.width,
            r.height
        );
    }

    // --- Rect tests ---

    #[test]
    fn rect_split_vertical() {
        let r = Rect::new(0.0, 0.0, 1000.0, 500.0);
        let (left, right) = r.split(SplitDirection::Vertical, 0.5);
        assert_rect_approx(&left, 0.0, 0.0, 500.0, 500.0);
        assert_rect_approx(&right, 500.0, 0.0, 500.0, 500.0);
    }

    #[test]
    fn rect_split_horizontal() {
        let r = Rect::new(0.0, 0.0, 1000.0, 500.0);
        let (top, bottom) = r.split(SplitDirection::Horizontal, 0.5);
        assert_rect_approx(&top, 0.0, 0.0, 1000.0, 250.0);
        assert_rect_approx(&bottom, 0.0, 250.0, 1000.0, 250.0);
    }

    #[test]
    fn rect_split_uneven() {
        let r = Rect::new(100.0, 200.0, 800.0, 600.0);
        let (left, right) = r.split(SplitDirection::Vertical, 0.3);
        assert_rect_approx(&left, 100.0, 200.0, 240.0, 600.0);
        assert_rect_approx(&right, 340.0, 200.0, 560.0, 600.0);
    }

    #[test]
    fn rect_center() {
        let r = Rect::new(100.0, 200.0, 400.0, 300.0);
        assert_eq!(r.center(), (300.0, 350.0));
    }

    #[test]
    fn rect_contains_point() {
        let r = Rect::new(100.0, 100.0, 200.0, 200.0);
        assert!(r.contains_point(150.0, 150.0));
        assert!(r.contains_point(100.0, 100.0));
        assert!(!r.contains_point(300.0, 300.0));
        assert!(!r.contains_point(50.0, 150.0));
    }

    // --- Node basic tests ---

    #[test]
    fn empty_tree() {
        let tree = Node::empty();
        assert!(tree.is_empty());
        assert_eq!(tree.window_count(), 0);
        assert!(tree.windows().is_empty());
    }

    #[test]
    fn insert_into_empty() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        assert!(!tree.is_empty());
        assert_eq!(tree.window_count(), 1);
        assert!(tree.contains(1));
    }

    #[test]
    fn insert_two_windows() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        assert_eq!(tree.window_count(), 2);
        assert!(tree.contains(1));
        assert!(tree.contains(2));
    }

    #[test]
    fn insert_three_windows() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        tree.insert(3, Some(2));
        assert_eq!(tree.window_count(), 3);
        assert!(tree.contains(1));
        assert!(tree.contains(2));
        assert!(tree.contains(3));
    }

    #[test]
    fn remove_single_window() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        assert!(tree.remove(1));
        assert!(tree.is_empty());
    }

    #[test]
    fn remove_from_two_collapses() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        assert!(tree.remove(1));
        assert_eq!(tree.window_count(), 1);
        assert!(tree.contains(2));
        // Should have collapsed back to a leaf
        assert!(matches!(tree, Node::Leaf { window: Some(2) }));
    }

    #[test]
    fn remove_nonexistent() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        assert!(!tree.remove(99));
        assert_eq!(tree.window_count(), 1);
    }

    #[test]
    fn remove_from_three() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        tree.insert(3, Some(2));
        assert!(tree.remove(2));
        assert_eq!(tree.window_count(), 2);
        assert!(tree.contains(1));
        assert!(tree.contains(3));
    }

    // --- Geometry tests ---

    #[test]
    fn geometry_single_window() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        let geoms = tree.calculate_geometries(SCREEN);
        assert_eq!(geoms.len(), 1);
        assert_eq!(geoms[0].0, 1);
        assert_rect_approx(&geoms[0].1, 0.0, 0.0, 1920.0, 1080.0);
    }

    #[test]
    fn geometry_two_windows_vertical_split() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        let geoms = tree.calculate_geometries(SCREEN);
        assert_eq!(geoms.len(), 2);

        let g1 = geoms.iter().find(|(w, _)| *w == 1).unwrap();
        let g2 = geoms.iter().find(|(w, _)| *w == 2).unwrap();

        // Window 1: left half
        assert_rect_approx(&g1.1, 0.0, 0.0, 960.0, 1080.0);
        // Window 2: right half
        assert_rect_approx(&g2.1, 960.0, 0.0, 960.0, 1080.0);
    }

    #[test]
    fn geometry_three_windows() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        tree.insert(3, Some(2));
        let geoms = tree.calculate_geometries(SCREEN);
        assert_eq!(geoms.len(), 3);

        let g1 = geoms.iter().find(|(w, _)| *w == 1).unwrap();
        let g2 = geoms.iter().find(|(w, _)| *w == 2).unwrap();
        let g3 = geoms.iter().find(|(w, _)| *w == 3).unwrap();

        // Window 1: left half
        assert_rect_approx(&g1.1, 0.0, 0.0, 960.0, 1080.0);
        // Window 2: top-right quarter
        assert_rect_approx(&g2.1, 960.0, 0.0, 480.0, 1080.0);
        // Window 3: bottom-right quarter
        assert_rect_approx(&g3.1, 1440.0, 0.0, 480.0, 1080.0);
    }

    #[test]
    fn geometry_empty_tree() {
        let tree = Node::empty();
        let geoms = tree.calculate_geometries(SCREEN);
        assert!(geoms.is_empty());
    }

    #[test]
    fn geometry_after_remove() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        tree.remove(1);
        let geoms = tree.calculate_geometries(SCREEN);
        assert_eq!(geoms.len(), 1);
        assert_rect_approx(&geoms[0].1, 0.0, 0.0, 1920.0, 1080.0);
    }

    // --- Equalize test ---

    #[test]
    fn equalize_resets_ratios() {
        let mut tree = Node::Internal {
            split: SplitDirection::Vertical,
            ratio: 0.3,
            left: Box::new(Node::Leaf { window: Some(1) }),
            right: Box::new(Node::Internal {
                split: SplitDirection::Horizontal,
                ratio: 0.7,
                left: Box::new(Node::Leaf { window: Some(2) }),
                right: Box::new(Node::Leaf { window: Some(3) }),
            }),
        };
        tree.equalize();
        let geoms = tree.calculate_geometries(SCREEN);
        let g1 = geoms.iter().find(|(w, _)| *w == 1).unwrap();
        assert_rect_approx(&g1.1, 0.0, 0.0, 960.0, 1080.0);
    }

    // --- Swap test ---

    #[test]
    fn swap_two_windows() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        assert!(tree.swap(1, 2));

        let geoms = tree.calculate_geometries(SCREEN);
        let g1 = geoms.iter().find(|(w, _)| *w == 1).unwrap();
        let g2 = geoms.iter().find(|(w, _)| *w == 2).unwrap();
        // After swap: window 1 should be on the right, window 2 on the left
        assert_rect_approx(&g2.1, 0.0, 0.0, 960.0, 1080.0);
        assert_rect_approx(&g1.1, 960.0, 0.0, 960.0, 1080.0);
    }

    // --- Find adjacent test ---

    #[test]
    fn find_adjacent_left_right() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        let geoms = tree.calculate_geometries(SCREEN);

        assert_eq!(Node::find_adjacent(&geoms, 1, Direction::Right), Some(2));
        assert_eq!(Node::find_adjacent(&geoms, 2, Direction::Left), Some(1));
        assert_eq!(Node::find_adjacent(&geoms, 1, Direction::Left), None);
        assert_eq!(Node::find_adjacent(&geoms, 2, Direction::Right), None);
    }

    // --- Resize test ---

    #[test]
    fn resize_grows_left_window() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        assert!(tree.resize(1, Direction::Right, 0.1));

        let geoms = tree.calculate_geometries(SCREEN);
        let g1 = geoms.iter().find(|(w, _)| *w == 1).unwrap();
        // Ratio should now be 0.6, so window 1 is 60% width
        assert_rect_approx(&g1.1, 0.0, 0.0, 1152.0, 1080.0);
    }

    #[test]
    fn resize_clamps_at_bounds() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        // Try to resize way past the limit
        for _ in 0..20 {
            tree.resize(1, Direction::Right, 0.1);
        }
        let geoms = tree.calculate_geometries(SCREEN);
        let g1 = geoms.iter().find(|(w, _)| *w == 1).unwrap();
        // Should clamp at 0.9 = 1728px
        assert_rect_approx(&g1.1, 0.0, 0.0, 1728.0, 1080.0);
    }

    // --- Windows / first_window ---

    #[test]
    fn windows_returns_all() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        tree.insert(3, Some(2));
        let mut ws = tree.windows();
        ws.sort();
        assert_eq!(ws, vec![1, 2, 3]);
    }

    #[test]
    fn first_window_returns_leftmost() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        assert_eq!(tree.first_window(), Some(1));
    }
}
