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

const STACK_REVEAL_OFFSET_X: f64 = 4.0;
const STACK_REVEAL_OFFSET_Y: f64 = 4.0;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Internal {
        split: SplitDirection,
        ratio: f32,
        left: Box<Node>,
        right: Box<Node>,
    },
    Stack {
        windows: Vec<WindowId>,
        active: usize,
        previous: Box<Node>,
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
    fn stack_reveal_direction(windows_len: usize, active: usize) -> f64 {
        let left_count = active;
        let right_count = windows_len.saturating_sub(active + 1);
        if left_count > right_count { -1.0 } else { 1.0 }
    }

    pub fn empty() -> Self {
        Node::Leaf { window: None }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Node::Leaf { window: None })
    }

    pub fn slot_count(&self) -> usize {
        match self {
            Node::Leaf { window: Some(_) } => 1,
            Node::Leaf { window: None } => 0,
            Node::Stack { windows, .. } => usize::from(!windows.is_empty()),
            Node::Internal { left, right, .. } => left.slot_count() + right.slot_count(),
        }
    }

    pub fn window_count(&self) -> usize {
        match self {
            Node::Leaf { window: Some(_) } => 1,
            Node::Leaf { window: None } => 0,
            Node::Stack { windows, .. } => windows.len(),
            Node::Internal { left, right, .. } => left.window_count() + right.window_count(),
        }
    }

    pub fn windows(&self) -> Vec<WindowId> {
        match self {
            Node::Leaf { window: Some(w) } => vec![*w],
            Node::Leaf { window: None } => vec![],
            Node::Stack { windows, .. } => windows.clone(),
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
            Node::Stack { windows, .. } => windows.contains(&window),
            Node::Internal { left, right, .. } => left.contains(window) || right.contains(window),
        }
    }

    pub fn first_window(&self) -> Option<WindowId> {
        match self {
            Node::Leaf { window } => *window,
            Node::Stack {
                windows, active, ..
            } => windows
                .get(*active)
                .copied()
                .or_else(|| windows.first().copied()),
            Node::Internal { left, right, .. } => {
                left.first_window().or_else(|| right.first_window())
            }
        }
    }

    /// Determine split direction based on container aspect ratio.
    /// Wider than tall → vertical (side-by-side). Taller → horizontal (stacked).
    pub fn smart_split_direction(rect: &Rect) -> SplitDirection {
        if rect.width > rect.height {
            SplitDirection::Vertical
        } else {
            SplitDirection::Horizontal
        }
    }

    /// Insert a window next to `target` using smart split.
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
                let direction = Self::smart_split_direction(&rect);
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
            Node::Stack {
                windows,
                active,
                previous,
            } => {
                let insert_at = target
                    .and_then(|target_window| windows.iter().position(|wid| *wid == target_window))
                    .map(|idx| idx + 1)
                    .unwrap_or_else(|| (*active + 1).min(windows.len()));
                windows.insert(insert_at, new_window);
                *active = insert_at;
                previous.insert_with_rect(new_window, target, rect);
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
            Node::Stack {
                windows,
                active,
                previous,
            } => {
                let Some(idx) = windows.iter().position(|wid| *wid == window) else {
                    return false;
                };
                windows.remove(idx);
                previous.remove(window);

                if windows.is_empty() {
                    *self = Node::empty();
                } else if windows.len() == 1 {
                    *self = Node::Leaf {
                        window: windows.first().copied(),
                    };
                } else if *active >= windows.len() {
                    *active = windows.len() - 1;
                } else if idx < *active {
                    *active -= 1;
                }
                true
            }
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
        self.calculate_geometries_with_gaps(rect, 0.0, 0.0, true)
    }

    pub fn calculate_focus_geometries(&self, rect: Rect) -> Vec<(WindowId, Rect)> {
        self.calculate_focus_geometries_with_gaps(rect, 0.0, 0.0, true)
    }

    /// Calculate geometries with inner and outer gaps.
    /// `gap_outer` is applied to the root rect edges.
    /// `gap_inner` is the space between adjacent windows (half applied to each side).
    pub fn calculate_geometries_with_gaps(
        &self,
        rect: Rect,
        gap_inner: f64,
        gap_outer: f64,
        is_root: bool,
    ) -> Vec<(WindowId, Rect)> {
        // Apply outer gap to root rect
        let padded = if is_root && gap_outer > 0.0 {
            Rect::new(
                rect.x + gap_outer,
                rect.y + gap_outer,
                (rect.width - 2.0 * gap_outer).max(0.0),
                (rect.height - 2.0 * gap_outer).max(0.0),
            )
        } else {
            rect
        };

        match self {
            Node::Leaf { window: Some(w) } => vec![(*w, padded)],
            Node::Leaf { window: None } => vec![],
            Node::Stack {
                windows, active, ..
            } => {
                let active_window = windows.get(*active).copied();
                let x_direction = Self::stack_reveal_direction(windows.len(), *active);
                let mut geoms = Vec::with_capacity(windows.len());
                let mut depth = 0usize;
                for (idx, wid) in windows.iter().enumerate() {
                    if Some(*wid) == active_window {
                        continue;
                    }
                    depth += 1;
                    geoms.push((
                        *wid,
                        Rect::new(
                            padded.x + x_direction * STACK_REVEAL_OFFSET_X * depth as f64,
                            padded.y + STACK_REVEAL_OFFSET_Y * depth as f64,
                            padded.width,
                            padded.height,
                        ),
                    ));
                    if idx == *active {
                        depth = depth.saturating_sub(1);
                    }
                }
                if let Some(wid) = active_window {
                    geoms.push((wid, padded));
                }
                geoms
            }
            Node::Internal {
                split,
                ratio,
                left,
                right,
            } => {
                let half_gap = gap_inner / 2.0;
                let (mut left_rect, mut right_rect) = padded.split(*split, *ratio);

                // Apply inner gap between the two halves
                if gap_inner > 0.0 {
                    match split {
                        SplitDirection::Vertical => {
                            left_rect.width = (left_rect.width - half_gap).max(0.0);
                            right_rect.x += half_gap;
                            right_rect.width = (right_rect.width - half_gap).max(0.0);
                        }
                        SplitDirection::Horizontal => {
                            left_rect.height = (left_rect.height - half_gap).max(0.0);
                            right_rect.y += half_gap;
                            right_rect.height = (right_rect.height - half_gap).max(0.0);
                        }
                    }
                }

                let mut geoms =
                    left.calculate_geometries_with_gaps(left_rect, gap_inner, gap_outer, false);
                geoms.extend(
                    right.calculate_geometries_with_gaps(right_rect, gap_inner, gap_outer, false),
                );
                geoms
            }
        }
    }

    pub fn calculate_focus_geometries_with_gaps(
        &self,
        rect: Rect,
        gap_inner: f64,
        gap_outer: f64,
        is_root: bool,
    ) -> Vec<(WindowId, Rect)> {
        let padded = if is_root && gap_outer > 0.0 {
            Rect::new(
                rect.x + gap_outer,
                rect.y + gap_outer,
                (rect.width - 2.0 * gap_outer).max(0.0),
                (rect.height - 2.0 * gap_outer).max(0.0),
            )
        } else {
            rect
        };

        match self {
            Node::Leaf { window: Some(w) } => vec![(*w, padded)],
            Node::Leaf { window: None } => vec![],
            Node::Stack {
                windows, active, ..
            } => windows
                .get(*active)
                .copied()
                .map(|wid| vec![(wid, padded)])
                .unwrap_or_default(),
            Node::Internal {
                split,
                ratio,
                left,
                right,
            } => {
                let half_gap = gap_inner / 2.0;
                let (mut left_rect, mut right_rect) = padded.split(*split, *ratio);

                if gap_inner > 0.0 {
                    match split {
                        SplitDirection::Vertical => {
                            left_rect.width = (left_rect.width - half_gap).max(0.0);
                            right_rect.x += half_gap;
                            right_rect.width = (right_rect.width - half_gap).max(0.0);
                        }
                        SplitDirection::Horizontal => {
                            left_rect.height = (left_rect.height - half_gap).max(0.0);
                            right_rect.y += half_gap;
                            right_rect.height = (right_rect.height - half_gap).max(0.0);
                        }
                    }
                }

                let mut geoms = left
                    .calculate_focus_geometries_with_gaps(left_rect, gap_inner, gap_outer, false);
                geoms.extend(
                    right.calculate_focus_geometries_with_gaps(
                        right_rect, gap_inner, gap_outer, false,
                    ),
                );
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
            Node::Stack {
                windows,
                active,
                previous,
            } => {
                if let Some(w) = windows.iter_mut().find(|w| **w == a) {
                    *w = b;
                    *found_a = true;
                } else if let Some(w) = windows.iter_mut().find(|w| **w == b) {
                    *w = a;
                    *found_b = true;
                }
                if let Some(current) = windows.get(*active).copied() {
                    if current == a {
                        *active = windows.iter().position(|wid| *wid == b).unwrap_or(*active);
                    } else if current == b {
                        *active = windows.iter().position(|wid| *wid == a).unwrap_or(*active);
                    }
                }
                previous.swap_impl(a, b, found_a, found_b);
            }
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
        Self::adjacent_candidates(geometries, from, direction)
            .into_iter()
            .next()
    }

    /// Find all windows in a direction from a source window, ordered from
    /// best focus candidate to worst using the same scoring as find_adjacent.
    pub fn adjacent_candidates(
        geometries: &[(WindowId, Rect)],
        from: WindowId,
        direction: Direction,
    ) -> Vec<WindowId> {
        let Some((_, from_rect)) = geometries.iter().find(|(w, _)| *w == from) else {
            return Vec::new();
        };
        let (from_cx, from_cy) = from_rect.center();

        let mut candidates: Vec<_> = geometries
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
            .collect();

        candidates.sort_by(|(_, a), (_, b)| {
            let dist_a = Self::adjacent_distance(&from_rect, a, direction);
            let dist_b = Self::adjacent_distance(&from_rect, b, direction);
            dist_a
                .partial_cmp(&dist_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        candidates.into_iter().map(|(w, _)| *w).collect()
    }

    /// Distance metric for focus navigation that uses edge distances and
    /// vertical/horizontal overlap instead of center-to-center distance.
    ///
    /// For Left/Right: prefer the closest window whose vertical extent
    /// overlaps with the source. Ties broken by horizontal gap.
    /// For Up/Down: prefer the closest window whose horizontal extent
    /// overlaps with the source. Ties broken by vertical gap.
    fn adjacent_distance(from: &Rect, to: &Rect, direction: Direction) -> f64 {
        match direction {
            Direction::Left | Direction::Right => {
                // Horizontal gap: edge-to-edge distance
                let h_gap = if matches!(direction, Direction::Right) {
                    (to.x - (from.x + from.width)).abs()
                } else {
                    (from.x - (to.x + to.width)).abs()
                };

                // Vertical overlap: how much the two windows share vertically.
                // Positive = overlapping, negative = gap between them.
                let overlap_top = from.y.max(to.y);
                let overlap_bot = (from.y + from.height).min(to.y + to.height);
                let v_overlap = overlap_bot - overlap_top;

                if v_overlap > 0.0 {
                    // Windows share vertical space — prefer closer horizontally.
                    // Subtract overlap as a bonus (more overlap = lower distance).
                    // Tiebreaker: prefer center closer to source center vertically.
                    let (_, from_cy) = from.center();
                    let (_, to_cy) = to.center();
                    let center_dist = (to_cy - from_cy).abs() * 0.001;
                    h_gap - v_overlap * 0.01 + center_dist
                } else {
                    // No vertical overlap — penalize the vertical gap heavily.
                    let v_gap = -v_overlap;
                    h_gap + v_gap * 100.0
                }
            }
            Direction::Up | Direction::Down => {
                let v_gap = if matches!(direction, Direction::Down) {
                    (to.y - (from.y + from.height)).abs()
                } else {
                    (from.y - (to.y + to.height)).abs()
                };

                let overlap_left = from.x.max(to.x);
                let overlap_right = (from.x + from.width).min(to.x + to.width);
                let h_overlap = overlap_right - overlap_left;

                if h_overlap > 0.0 {
                    let (from_cx, _) = from.center();
                    let (to_cx, _) = to.center();
                    let center_dist = (to_cx - from_cx).abs() * 0.001;
                    v_gap - h_overlap * 0.01 + center_dist
                } else {
                    let h_gap = -h_overlap;
                    v_gap + h_gap * 100.0
                }
            }
        }
    }

    /// Find the window nearest to the entry edge when crossing monitors.
    /// When entering from the Left (pressing Right), picks the window with smallest center-x.
    /// When entering from the Right (pressing Left), picks the window with largest center-x.
    pub fn nearest_to_edge(
        geometries: &[(WindowId, Rect)],
        direction: Direction,
    ) -> Option<WindowId> {
        if geometries.is_empty() {
            return None;
        }
        geometries
            .iter()
            .min_by(|(_, a), (_, b)| {
                let (acx, _) = a.center();
                let (bcx, _) = b.center();
                match direction {
                    // Pressing Right → entering target from left → want leftmost (smallest x)
                    Direction::Right => acx.partial_cmp(&bcx).unwrap_or(std::cmp::Ordering::Equal),
                    // Pressing Left → entering target from right → want rightmost (largest x)
                    Direction::Left => bcx.partial_cmp(&acx).unwrap_or(std::cmp::Ordering::Equal),
                    // Up/Down: use y instead
                    Direction::Down => {
                        let (_, acy) = a.center();
                        let (_, bcy) = b.center();
                        acy.partial_cmp(&bcy).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    Direction::Up => {
                        let (_, acy) = a.center();
                        let (_, bcy) = b.center();
                        bcy.partial_cmp(&acy).unwrap_or(std::cmp::Ordering::Equal)
                    }
                }
            })
            .map(|(w, _)| *w)
    }

    /// Resize the split affecting a window in the given direction.
    pub fn resize(&mut self, window: WindowId, direction: Direction, delta: f32) -> bool {
        match self {
            Node::Leaf { .. } | Node::Stack { .. } => false,
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

    pub fn set_stack_active(&mut self, window: WindowId) -> bool {
        match self {
            Node::Stack {
                windows,
                active,
                previous: _,
            } => {
                if let Some(idx) = windows.iter().position(|wid| *wid == window) {
                    *active = idx;
                    true
                } else {
                    false
                }
            }
            Node::Internal { left, right, .. } => {
                left.set_stack_active(window) || right.set_stack_active(window)
            }
            Node::Leaf { .. } => false,
        }
    }

    pub fn stack_info(&self, window: WindowId) -> Option<(Vec<WindowId>, usize)> {
        match self {
            Node::Stack {
                windows, active, ..
            } if windows.contains(&window) => Some((windows.clone(), *active)),
            Node::Internal { left, right, .. } => {
                left.stack_info(window).or_else(|| right.stack_info(window))
            }
            _ => None,
        }
    }

    pub fn cycle_stack(&mut self, window: WindowId, forward: bool) -> Option<WindowId> {
        match self {
            Node::Stack {
                windows, active, ..
            } if windows.contains(&window) => {
                if windows.is_empty() {
                    return None;
                }
                let next = if forward {
                    if *active + 1 < windows.len() {
                        Some(*active + 1)
                    } else {
                        None
                    }
                } else if *active > 0 {
                    Some(*active - 1)
                } else {
                    None
                }?;
                *active = next;
                windows.get(*active).copied()
            }
            Node::Internal { left, right, .. } => left
                .cycle_stack(window, forward)
                .or_else(|| right.cycle_stack(window, forward)),
            _ => None,
        }
    }

    pub fn reorder_stack(&mut self, window: WindowId, forward: bool) -> Option<WindowId> {
        match self {
            Node::Stack {
                windows,
                active,
                previous,
            } if windows.contains(&window) => {
                let idx = windows.iter().position(|wid| *wid == window)?;
                let swap_idx = if forward {
                    if idx + 1 < windows.len() {
                        Some(idx + 1)
                    } else {
                        None
                    }
                } else if idx > 0 {
                    Some(idx - 1)
                } else {
                    None
                }?;
                windows.swap(idx, swap_idx);
                *active = swap_idx;
                previous.swap(window, windows[idx]);
                windows.get(*active).copied()
            }
            Node::Internal { left, right, .. } => left
                .reorder_stack(window, forward)
                .or_else(|| right.reorder_stack(window, forward)),
            _ => None,
        }
    }

    pub fn unstack(&mut self, window: WindowId) -> bool {
        match self {
            Node::Stack {
                windows, previous, ..
            } if windows.contains(&window) => {
                *self = (**previous).clone();
                true
            }
            Node::Internal { left, right, .. } => left.unstack(window) || right.unstack(window),
            _ => false,
        }
    }

    pub fn make_stack_for_window(&mut self, window: WindowId) -> bool {
        self.make_stack_for_window_impl(window)
    }

    fn make_stack_for_window_impl(&mut self, window: WindowId) -> bool {
        match self {
            Node::Internal { left, right, .. } => {
                let left_contains = left.contains(window);
                let right_contains = right.contains(window);
                if !left_contains && !right_contains {
                    return false;
                }

                let child_contains = if left_contains { left } else { right };
                if !matches!(child_contains.as_ref(), Node::Stack { .. })
                    && child_contains.slot_count() > 1
                    && child_contains.make_stack_for_window_impl(window)
                {
                    return true;
                }

                if self.slot_count() > 1 {
                    let previous = self.clone();
                    let windows = self.windows();
                    let active = windows.iter().position(|wid| *wid == window).unwrap_or(0);
                    *self = Node::Stack {
                        windows,
                        active,
                        previous: Box::new(previous),
                    };
                    true
                } else {
                    false
                }
            }
            _ => false,
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
    fn geometry_three_windows_smart_split() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        let geoms = tree.calculate_geometries(SCREEN);
        assert_eq!(geoms.len(), 3);

        let g1 = geoms.iter().find(|(w, _)| *w == 1).unwrap();
        let g2 = geoms.iter().find(|(w, _)| *w == 2).unwrap();
        let g3 = geoms.iter().find(|(w, _)| *w == 3).unwrap();

        // Window 1: left half (1920 > 1080, so first split is vertical)
        assert_rect_approx(&g1.1, 0.0, 0.0, 960.0, 1080.0);
        // Window 2: right half is 960x1080 (taller than wide), so split horizontal
        // Window 2: top-right
        assert_rect_approx(&g2.1, 960.0, 0.0, 960.0, 540.0);
        // Window 3: bottom-right
        assert_rect_approx(&g3.1, 960.0, 540.0, 960.0, 540.0);
    }

    #[test]
    fn smart_split_four_windows() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        tree.insert_with_rect(4, Some(3), SCREEN);
        let geoms = tree.calculate_geometries(SCREEN);
        assert_eq!(geoms.len(), 4);

        let g1 = geoms.iter().find(|(w, _)| *w == 1).unwrap();
        let g2 = geoms.iter().find(|(w, _)| *w == 2).unwrap();
        let g3 = geoms.iter().find(|(w, _)| *w == 3).unwrap();
        let g4 = geoms.iter().find(|(w, _)| *w == 4).unwrap();

        // ┌──────────────┬──────────────┐
        // │              │    Win 2     │
        // │   Win 1      ├──────┬───────┤
        // │              │ W 3  │  W 4  │
        // └──────────────┴──────┴───────┘
        assert_rect_approx(&g1.1, 0.0, 0.0, 960.0, 1080.0);
        assert_rect_approx(&g2.1, 960.0, 0.0, 960.0, 540.0);
        // Bottom-right 960x540 is wider than tall → vertical split
        assert_rect_approx(&g3.1, 960.0, 540.0, 480.0, 540.0);
        assert_rect_approx(&g4.1, 1440.0, 540.0, 480.0, 540.0);
    }

    #[test]
    fn smart_split_direction_wide() {
        assert_eq!(
            Node::smart_split_direction(&Rect::new(0.0, 0.0, 1920.0, 1080.0)),
            SplitDirection::Vertical
        );
    }

    #[test]
    fn smart_split_direction_tall() {
        assert_eq!(
            Node::smart_split_direction(&Rect::new(0.0, 0.0, 960.0, 1080.0)),
            SplitDirection::Horizontal
        );
    }

    #[test]
    fn smart_split_direction_square() {
        // Equal → horizontal (height >= width)
        assert_eq!(
            Node::smart_split_direction(&Rect::new(0.0, 0.0, 500.0, 500.0)),
            SplitDirection::Horizontal
        );
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

    // --- Edge cases ---

    #[test]
    fn rect_zero_size() {
        let r = Rect::new(100.0, 200.0, 0.0, 0.0);
        let (left, right) = r.split(SplitDirection::Vertical, 0.5);
        assert_eq!(left.width, 0.0);
        assert_eq!(right.width, 0.0);
    }

    #[test]
    fn insert_remove_all_then_reinsert() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        tree.remove(1);
        tree.remove(2);
        assert!(tree.is_empty());
        tree.insert(3, None);
        assert_eq!(tree.window_count(), 1);
        assert!(tree.contains(3));
    }

    #[test]
    fn remove_same_window_twice() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        assert!(tree.remove(1));
        assert!(!tree.remove(1)); // second remove returns false
    }

    #[test]
    fn swap_nonexistent_windows() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        tree.insert(2, Some(1));
        assert!(!tree.swap(1, 99)); // 99 doesn't exist
    }

    #[test]
    fn resize_single_window_noop() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        assert!(!tree.resize(1, Direction::Right, 0.1));
    }

    #[test]
    fn find_adjacent_single_window() {
        let mut tree = Node::empty();
        tree.insert(1, None);
        let geoms = tree.calculate_geometries(SCREEN);
        assert_eq!(Node::find_adjacent(&geoms, 1, Direction::Right), None);
        assert_eq!(Node::find_adjacent(&geoms, 1, Direction::Left), None);
    }

    #[test]
    fn geometry_preserves_total_area() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        tree.insert_with_rect(4, Some(3), SCREEN);

        let geoms = tree.calculate_geometries(SCREEN);
        let total_area: f64 = geoms.iter().map(|(_, r)| r.width * r.height).sum();
        let screen_area = SCREEN.width * SCREEN.height;
        assert!(
            (total_area - screen_area).abs() < 0.01,
            "total area {} != screen area {}",
            total_area,
            screen_area
        );
    }

    #[test]
    fn find_adjacent_four_windows() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        tree.insert_with_rect(4, Some(3), SCREEN);
        let geoms = tree.calculate_geometries(SCREEN);

        // In the 4-window smart-split layout:
        // ┌──────┬──────┐
        // │      │  2   │
        // │  1   ├──┬───┤
        // │      │ 3│ 4 │
        // └──────┴──┴───┘
        // Win1 is full-height left column. Win2 and Win3 both share an edge
        // and equal vertical overlap. Tiebreaker: center closest to source
        // center (y=540). Win2 center y=270 (dist=270) < Win3 center y=810 (dist=270).
        // Equal tiebreaker, but Win2 comes first → picks 2.
        assert_eq!(Node::find_adjacent(&geoms, 1, Direction::Right), Some(2));
        assert_eq!(Node::find_adjacent(&geoms, 2, Direction::Left), Some(1));
        assert_eq!(Node::find_adjacent(&geoms, 2, Direction::Down), Some(3));
        assert_eq!(Node::find_adjacent(&geoms, 3, Direction::Up), Some(2));
        assert_eq!(Node::find_adjacent(&geoms, 3, Direction::Right), Some(4));
        assert_eq!(Node::find_adjacent(&geoms, 4, Direction::Left), Some(3));
    }

    #[test]
    fn adjacent_candidates_preserve_focus_priority_order() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        let geoms = tree.calculate_geometries(SCREEN);

        assert_eq!(
            Node::adjacent_candidates(&geoms, 1, Direction::Right),
            vec![2, 3]
        );
        assert_eq!(
            Node::adjacent_candidates(&geoms, 2, Direction::Left),
            vec![1]
        );
    }

    // --- Nearest to edge tests ---

    #[test]
    fn nearest_to_edge_picks_leftmost_on_right_cross() {
        // Simulate 4-window layout
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        tree.insert_with_rect(4, Some(3), SCREEN);
        let geoms = tree.calculate_geometries(SCREEN);
        // Pressing Right to enter this monitor → want leftmost window
        // Win1 is at x=0 (leftmost)
        assert_eq!(Node::nearest_to_edge(&geoms, Direction::Right), Some(1));
    }

    #[test]
    fn nearest_to_edge_picks_rightmost_on_left_cross() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        tree.insert_with_rect(4, Some(3), SCREEN);
        let geoms = tree.calculate_geometries(SCREEN);
        // Pressing Left to enter this monitor → want rightmost window
        // Win4 center=(1680,810) is rightmost
        assert_eq!(Node::nearest_to_edge(&geoms, Direction::Left), Some(4));
    }

    #[test]
    fn nearest_to_edge_empty() {
        let geoms: Vec<(WindowId, Rect)> = vec![];
        assert_eq!(Node::nearest_to_edge(&geoms, Direction::Right), None);
    }

    #[test]
    fn nearest_to_edge_single_window() {
        let geoms = vec![(1, Rect::new(0.0, 0.0, 1920.0, 1080.0))];
        assert_eq!(Node::nearest_to_edge(&geoms, Direction::Right), Some(1));
        assert_eq!(Node::nearest_to_edge(&geoms, Direction::Left), Some(1));
    }

    // --- Gap tests ---

    #[test]
    fn gaps_reduce_window_sizes() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);

        let geoms_no_gap = tree.calculate_geometries(SCREEN);
        let geoms_with_gap = tree.calculate_geometries_with_gaps(SCREEN, 10.0, 20.0, true);

        // With outer gap, windows should be smaller
        let g1_no = geoms_no_gap.iter().find(|(w, _)| *w == 1).unwrap();
        let g1_gap = geoms_with_gap.iter().find(|(w, _)| *w == 1).unwrap();
        assert!(g1_gap.1.width < g1_no.1.width);
        assert!(g1_gap.1.x > g1_no.1.x); // shifted by outer gap
    }

    #[test]
    fn gaps_preserve_no_overlap() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);

        let geoms = tree.calculate_geometries_with_gaps(SCREEN, 10.0, 20.0, true);
        let g1 = geoms.iter().find(|(w, _)| *w == 1).unwrap();
        let g2 = geoms.iter().find(|(w, _)| *w == 2).unwrap();

        // Windows should not overlap — g1's right edge should be left of g2's left edge
        let g1_right = g1.1.x + g1.1.width;
        assert!(
            g1_right <= g2.1.x,
            "windows overlap: g1 right {} > g2 left {}",
            g1_right,
            g2.1.x
        );
    }

    #[test]
    fn zero_gaps_same_as_no_gaps() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);

        let geoms_default = tree.calculate_geometries(SCREEN);
        let geoms_zero = tree.calculate_geometries_with_gaps(SCREEN, 0.0, 0.0, true);

        assert_eq!(geoms_default.len(), geoms_zero.len());
        for i in 0..geoms_default.len() {
            assert_eq!(geoms_default[i].0, geoms_zero[i].0);
            assert_rect_approx(
                &geoms_zero[i].1,
                geoms_default[i].1.x,
                geoms_default[i].1.y,
                geoms_default[i].1.width,
                geoms_default[i].1.height,
            );
        }
    }

    #[test]
    fn make_stack_for_window_replaces_smallest_conflicting_subtree() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);

        assert!(tree.make_stack_for_window(3));
        let stack = tree.stack_info(3).expect("window should be stacked");
        assert_eq!(stack.0, vec![2, 3]);
        assert_eq!(stack.1, 1);
        assert!(tree.contains(1));
    }

    #[test]
    fn stacked_render_geometries_reveal_background_windows_to_the_right_when_right_biased() {
        let tree = Node::Stack {
            windows: vec![1, 2, 3],
            active: 0,
            previous: Box::new(Node::Leaf { window: Some(1) }),
        };
        let geoms = tree.calculate_geometries(SCREEN);
        let g1 = geoms.iter().find(|(wid, _)| *wid == 1).unwrap().1;
        let g2 = geoms.iter().find(|(wid, _)| *wid == 2).unwrap().1;

        assert!(g2.x > g1.x);
        assert!(g2.y > g1.y);
        assert_eq!(g2.width, g1.width);
        assert_eq!(g2.height, g1.height);
    }

    #[test]
    fn stacked_render_geometries_reveal_background_windows_to_the_left_when_left_biased() {
        let tree = Node::Stack {
            windows: vec![1, 2, 3],
            active: 2,
            previous: Box::new(Node::Leaf { window: Some(3) }),
        };
        let geoms = tree.calculate_geometries(SCREEN);
        let g2 = geoms.iter().find(|(wid, _)| *wid == 2).unwrap().1;
        let g3 = geoms.iter().find(|(wid, _)| *wid == 3).unwrap().1;

        assert!(g2.x < g3.x);
        assert!(g2.y > g3.y);
        assert_eq!(g2.width, g3.width);
        assert_eq!(g2.height, g3.height);
    }

    #[test]
    fn cycle_stack_stops_at_directional_boundary() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        assert!(tree.make_stack_for_window(3));

        assert_eq!(tree.cycle_stack(3, false), Some(2));
        assert_eq!(tree.cycle_stack(2, false), None);
        assert_eq!(tree.cycle_stack(2, true), Some(3));
        assert_eq!(tree.cycle_stack(3, true), None);
    }

    #[test]
    fn unstack_restores_previous_subtree() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        let original = tree.clone();

        assert!(tree.make_stack_for_window(3));
        assert!(tree.unstack(3));
        assert_eq!(tree, original);
    }

    #[test]
    fn removing_from_stack_collapses_to_leaf() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);

        assert!(tree.make_stack_for_window(2));
        assert!(tree.remove(2));

        assert!(matches!(tree, Node::Leaf { window: Some(1) }));
    }
}
