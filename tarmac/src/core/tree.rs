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
                // All stack members share the tile's full rect. Non-active
                // windows sit fully behind the active in z-order. The
                // previous "Slack-style reveal" offset (origin += depth *
                // STACK_REVEAL_OFFSET) leaked: the depth counter never
                // reset past the active member, so windows iterated after
                // the active in insertion order accumulated progressively
                // larger offsets and visibly drifted away from the tile
                // (user-reported as "the windows are not sized and
                // positioned as if they are members of the stack").
                //
                // Order matters here even though every member shares the
                // same rect: apply_layout walks this vec and issues AX
                // position writes per element, and macOS z-orders whichever
                // window was positioned last to the top of its level.
                // Iterating in insertion order would make the last-inserted
                // member visible regardless of `active`. Putting the active
                // member last keeps it on top — including when focus leaves
                // the stack to another monitor and apply_layout re-runs on
                // the source workspace (visible artifact: the "wrong" stack
                // member jumps to the top after mod+arrow off the stack).
                let mut ordered: Vec<(WindowId, Rect)> = Vec::with_capacity(windows.len());
                for (i, wid) in windows.iter().enumerate() {
                    if i != *active {
                        ordered.push((*wid, padded));
                    }
                }
                if let Some(active_wid) = windows.get(*active) {
                    ordered.push((*active_wid, padded));
                }
                ordered
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
            let dist_a = Self::adjacent_distance(from_rect, a, direction);
            let dist_b = Self::adjacent_distance(from_rect, b, direction);
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
    pub fn adjacent_distance(from: &Rect, to: &Rect, direction: Direction) -> f64 {
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
    ///
    /// Climbs from the focused leaf up the tree to find the *deepest*
    /// ancestor split whose direction matches `direction`, and adjusts
    /// that split's ratio. The pre-HX-02 implementation only triggered
    /// at the shallowest matching ancestor (the root-most), which made
    /// resizing "feel global": pressing Left would shift the outermost
    /// vertical boundary regardless of how deeply the focused window
    /// was nested. Climbing-up-then-pick-deepest keeps the adjustment
    /// local — the boundary nearest the focused window moves first.
    ///
    /// Returns false when no ancestor split matches the requested
    /// direction (e.g. all splits in the tree are Horizontal but
    /// direction is Left/Right). In that case the focused window
    /// already spans the relevant axis fully and there is nothing to
    /// resize.
    pub fn resize(&mut self, window: WindowId, direction: Direction, delta: f32) -> bool {
        match self {
            Node::Leaf { .. } | Node::Stack { .. } => false,
            Node::Internal {
                split,
                ratio,
                left,
                right,
            } => {
                // Recurse first: a deeper Internal matching the direction
                // takes precedence over this level. This yields the
                // deepest matching ancestor when the recursion returns
                // true.
                if left.contains(window) && left.resize(window, direction, delta) {
                    return true;
                }
                if right.contains(window) && right.resize(window, direction, delta) {
                    return true;
                }

                let axis_matches = matches!(
                    (split, direction),
                    (SplitDirection::Vertical, Direction::Left | Direction::Right)
                        | (SplitDirection::Horizontal, Direction::Up | Direction::Down)
                );

                if axis_matches && (left.contains(window) || right.contains(window)) {
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
                    true
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

    pub fn promote_stack_window(&mut self, window: WindowId) -> bool {
        match self {
            Node::Stack {
                windows,
                active,
                previous,
            } if windows.contains(&window) => {
                let Some(idx) = windows.iter().position(|wid| *wid == window) else {
                    return false;
                };
                if idx == 0 {
                    *active = 0;
                    return true;
                }
                let displaced = windows[0];
                windows.remove(idx);
                windows.insert(0, window);
                *active = 0;
                previous.swap(window, displaced);
                true
            }
            Node::Internal { left, right, .. } => {
                left.promote_stack_window(window) || right.promote_stack_window(window)
            }
            _ => false,
        }
    }

    /// Lift the focused window's leaf-or-stack subtree to be one direct
    /// child of the workspace root. The rest of the tree becomes the
    /// other child of a new root Internal that inherits the previous
    /// root's split direction. The focused subtree is placed on the
    /// side it currently occupies relative to root (stable mode):
    /// preserves visual screen-half of the focused window.
    ///
    /// Returns false when the focused window is already a direct child
    /// of root (nothing to promote), when the root is a Leaf or Stack
    /// rather than Internal (no parent split to manipulate), or when
    /// the window is not in the tree.
    ///
    /// Stacks containing the focused window are promoted as a single
    /// unit; the Stack node is preserved intact rather than unstacked.
    pub fn promote_to_root(&mut self, window: WindowId) -> bool {
        // Must be Internal at root, with the window somewhere inside.
        if !matches!(self, Node::Internal { .. }) {
            return false;
        }
        if !self.contains(window) {
            return false;
        }

        // Capture root's split direction + which side the focused leaf
        // currently lives in, before any mutation.
        let (original_split, focused_in_left) = match self {
            Node::Internal { split, left, .. } => (*split, left.contains(window)),
            _ => return false,
        };

        // Trivial case: the focused leaf-or-stack is already a direct
        // child of root. Nothing to promote.
        let direct_child_holds_window = match self {
            Node::Internal { left, right, .. } => {
                let direct = |n: &Node| match n {
                    Node::Leaf { window: Some(w) } => *w == window,
                    Node::Stack { windows, .. } => windows.contains(&window),
                    _ => false,
                };
                direct(left.as_ref()) || direct(right.as_ref())
            }
            _ => false,
        };
        if direct_child_holds_window {
            return false;
        }

        // Extract the leaf-or-stack containing the window. The tree
        // flattens around the removed slot (parent Internals collapse
        // when one child becomes Empty), giving us the "remainder."
        let Some(focused_node) = self.extract_leaf_or_stack(window) else {
            return false;
        };

        let remainder = std::mem::take(self);
        let (left_child, right_child) = if focused_in_left {
            (Box::new(focused_node), Box::new(remainder))
        } else {
            (Box::new(remainder), Box::new(focused_node))
        };
        *self = Node::Internal {
            split: original_split,
            ratio: 0.5,
            left: left_child,
            right: right_child,
        };
        true
    }

    /// Walk the tree and remove the Leaf-or-Stack that holds `window`,
    /// returning the extracted node. Parent Internals collapse when one
    /// child becomes Empty so the remainder stays well-formed.
    fn extract_leaf_or_stack(&mut self, window: WindowId) -> Option<Node> {
        match self {
            Node::Leaf { window: Some(w) } if *w == window => Some(std::mem::take(self)),
            Node::Stack { windows, .. } if windows.contains(&window) => {
                Some(std::mem::take(self))
            }
            Node::Internal { left, right, .. } => {
                if let Some(extracted) = left.extract_leaf_or_stack(window) {
                    if left.is_empty() {
                        *self = std::mem::take(right.as_mut());
                    }
                    return Some(extracted);
                }
                if let Some(extracted) = right.extract_leaf_or_stack(window) {
                    if right.is_empty() {
                        *self = std::mem::take(left.as_mut());
                    }
                    return Some(extracted);
                }
                None
            }
            _ => None,
        }
    }

    /// Swap the left/right children of the focused window's immediate
    /// Internal ancestor. Effectively moves the focused window's subtree
    /// to the opposite side of its parent split. Returns false if the
    /// window is at the root (no parent) or not in the tree.
    pub fn swap_parent_children(&mut self, window: WindowId) -> bool {
        match self {
            Node::Internal { left, right, .. } => {
                if matches!(left.as_ref(), Node::Internal { .. }) && left.contains(window) {
                    return left.swap_parent_children(window);
                }
                if matches!(right.as_ref(), Node::Internal { .. }) && right.contains(window) {
                    return right.swap_parent_children(window);
                }
                if left.contains(window) || right.contains(window) {
                    std::mem::swap(left, right);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Flip the split direction of the focused window's immediate Internal
    /// ancestor (the deepest Internal containing the window). Returns
    /// false if the window is at the root (no parent) or not in the tree.
    /// A window inside a Stack treats the Stack's parent Internal as the
    /// target.
    pub fn toggle_parent_split(&mut self, window: WindowId) -> bool {
        match self {
            Node::Internal {
                split, left, right, ..
            } => {
                if matches!(left.as_ref(), Node::Internal { .. }) && left.contains(window) {
                    return left.toggle_parent_split(window);
                }
                if matches!(right.as_ref(), Node::Internal { .. }) && right.contains(window) {
                    return right.toggle_parent_split(window);
                }
                if left.contains(window) || right.contains(window) {
                    *split = match split {
                        SplitDirection::Vertical => SplitDirection::Horizontal,
                        SplitDirection::Horizontal => SplitDirection::Vertical,
                    };
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Locate the leaf whose computed sub-rect contains the given point,
    /// returning the window in that leaf. Recurses through Internal splits
    /// using the same geometry math as `calculate_geometries` (no gaps).
    /// For a Stack tile, returns the active member's wid so callers can
    /// treat the stack as a single occupied leaf.
    ///
    /// Returns None when the point falls outside `rect`, into an empty
    /// leaf, or into a stack with no active member.
    pub fn find_leaf_at_point(&self, rect: Rect, x: f64, y: f64) -> Option<WindowId> {
        if !rect.contains_point(x, y) {
            return None;
        }
        match self {
            Node::Leaf { window } => *window,
            Node::Stack {
                windows, active, ..
            } => windows.get(*active).copied(),
            Node::Internal {
                split,
                ratio,
                left,
                right,
            } => {
                let (left_rect, right_rect) = rect.split(*split, *ratio);
                if left_rect.contains_point(x, y) {
                    left.find_leaf_at_point(left_rect, x, y)
                } else {
                    right.find_leaf_at_point(right_rect, x, y)
                }
            }
        }
    }

    pub fn make_stack_for_window(&mut self, window: WindowId) -> bool {
        self.make_stack_for_window_impl(window)
    }

    /// Replace the entire tree with a single root-level Stack containing
    /// every window currently in the tree. Used as overflow remediation
    /// during initial discovery: if even one window can't fit its tile in
    /// the BSP layout we'd otherwise produce, just stack everything into
    /// one tile so the user gets a clean, navigable layout instead of a
    /// staircase with a partial stack at the bottom.
    pub fn collapse_to_root_stack(&mut self, active_window: WindowId) -> bool {
        match self {
            Node::Internal { .. } => {
                let previous = self.clone();
                let windows = self.windows();
                if windows.is_empty() {
                    return false;
                }
                let active = windows
                    .iter()
                    .position(|wid| *wid == active_window)
                    .unwrap_or(0);
                *self = Node::Stack {
                    windows,
                    active,
                    previous: Box::new(previous),
                };
                true
            }
            _ => false,
        }
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
    fn stacked_render_geometries_share_the_tile_rect() {
        // All stack members occupy the full tile rect, regardless of
        // which is active. Non-active windows sit fully behind the
        // active in z-order — no Slack-style peek offset (removed:
        // its depth counter accumulated past the active position
        // and produced visibly drifting non-active windows).
        for active in 0..3 {
            let tree = Node::Stack {
                windows: vec![1, 2, 3],
                active,
                previous: Box::new(Node::Leaf { window: Some(1) }),
            };
            let geoms = tree.calculate_geometries(SCREEN);
            assert_eq!(geoms.len(), 3);
            for (_, rect) in &geoms {
                assert_eq!(rect.x, SCREEN.x);
                assert_eq!(rect.y, SCREEN.y);
                assert_eq!(rect.width, SCREEN.width);
                assert_eq!(rect.height, SCREEN.height);
            }
        }
    }

    #[test]
    fn promote_stack_window_moves_target_to_top() {
        let mut tree = Node::Stack {
            windows: vec![1, 2, 3],
            active: 2,
            previous: Box::new(Node::Leaf { window: Some(3) }),
        };

        assert!(tree.promote_stack_window(3));
        assert_eq!(tree.stack_info(3), Some((vec![3, 1, 2], 0)));
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

    // --- find_leaf_at_point tests ---

    #[test]
    fn find_leaf_at_point_returns_window_when_inside() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        assert_eq!(tree.find_leaf_at_point(SCREEN, 100.0, 100.0), Some(1));
    }

    #[test]
    fn find_leaf_at_point_returns_none_when_outside() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        assert_eq!(tree.find_leaf_at_point(SCREEN, 2000.0, 100.0), None);
        assert_eq!(tree.find_leaf_at_point(SCREEN, 100.0, 2000.0), None);
        assert_eq!(tree.find_leaf_at_point(SCREEN, -1.0, 100.0), None);
    }

    #[test]
    fn find_leaf_at_point_returns_none_for_empty_leaf() {
        let tree = Node::empty();
        assert_eq!(tree.find_leaf_at_point(SCREEN, 100.0, 100.0), None);
    }

    #[test]
    fn find_leaf_at_point_handles_stack_returns_active() {
        let stack = Node::Stack {
            windows: vec![1, 2, 3],
            active: 1, // window 2 is active
            previous: Box::new(Node::empty()),
        };
        assert_eq!(stack.find_leaf_at_point(SCREEN, 100.0, 100.0), Some(2));
    }

    #[test]
    fn find_leaf_at_point_descends_vertical_split_left() {
        // SCREEN is wider than tall → smart split = Vertical.
        // Two windows: 1 ends up left, 2 ends up right.
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        // Left half is x in [0, 960).
        assert_eq!(tree.find_leaf_at_point(SCREEN, 100.0, 500.0), Some(1));
    }

    #[test]
    fn find_leaf_at_point_descends_vertical_split_right() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        // Right half is x in [960, 1920).
        assert_eq!(tree.find_leaf_at_point(SCREEN, 1500.0, 500.0), Some(2));
    }

    // --- toggle_parent_split tests ---

    #[test]
    fn toggle_parent_split_flips_vertical_to_horizontal() {
        // SCREEN insertion produces Internal { Vertical, Leaf{1}, Leaf{2} }.
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        assert!(matches!(
            tree,
            Node::Internal {
                split: SplitDirection::Vertical,
                ..
            }
        ));
        assert!(tree.toggle_parent_split(1));
        assert!(matches!(
            tree,
            Node::Internal {
                split: SplitDirection::Horizontal,
                ..
            }
        ));
        // Idempotent toggle returns to original.
        assert!(tree.toggle_parent_split(1));
        assert!(matches!(
            tree,
            Node::Internal {
                split: SplitDirection::Vertical,
                ..
            }
        ));
    }

    #[test]
    fn toggle_parent_split_returns_false_for_root_window() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        // Single-window tree: window 1 is a Leaf at root with no parent split.
        assert!(!tree.toggle_parent_split(1));
    }

    #[test]
    fn toggle_parent_split_returns_false_for_missing_window() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        assert!(!tree.toggle_parent_split(99));
    }

    // --- swap_parent_children tests ---

    #[test]
    fn swap_parent_children_swaps_two_window_tree() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        // Pre-swap: 1 on the left half, 2 on the right.
        let pre = tree.calculate_geometries(SCREEN);
        let pre_left = pre.iter().find(|(w, _)| *w == 1).unwrap().1;
        let pre_right = pre.iter().find(|(w, _)| *w == 2).unwrap().1;
        assert!(pre_left.x < pre_right.x);

        assert!(tree.swap_parent_children(1));
        // Post-swap: 1 on the right, 2 on the left.
        let post = tree.calculate_geometries(SCREEN);
        let post_one = post.iter().find(|(w, _)| *w == 1).unwrap().1;
        let post_two = post.iter().find(|(w, _)| *w == 2).unwrap().1;
        assert!(post_one.x > post_two.x);
    }

    #[test]
    fn swap_parent_children_preserves_subtree_shapes() {
        // Tree: Internal { Vertical, Leaf{1}, Internal { Horizontal, Leaf{2}, Leaf{3} } }
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);

        let pre = tree.calculate_geometries(SCREEN);
        let pre_two = pre.iter().find(|(w, _)| *w == 2).unwrap().1;
        let pre_three = pre.iter().find(|(w, _)| *w == 3).unwrap().1;
        // Pre-swap: 2 and 3 share a Horizontal split on the right half.
        assert!(approx_eq(pre_two.x, pre_three.x));
        assert!(pre_two.y < pre_three.y);

        // Swap at the outer split via window 1.
        assert!(tree.swap_parent_children(1));

        let post = tree.calculate_geometries(SCREEN);
        let post_one = post.iter().find(|(w, _)| *w == 1).unwrap().1;
        let post_two = post.iter().find(|(w, _)| *w == 2).unwrap().1;
        let post_three = post.iter().find(|(w, _)| *w == 3).unwrap().1;
        // 1 moved to the right; 2-3 subtree to the left but its Horizontal
        // split is still intact (same x, 2 above 3).
        assert!(post_one.x > post_two.x);
        assert!(approx_eq(post_two.x, post_three.x));
        assert!(post_two.y < post_three.y);
    }

    #[test]
    fn swap_parent_children_returns_false_for_root() {
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        assert!(!tree.swap_parent_children(1));
    }

    // --- promote_to_root tests ---

    #[test]
    fn promote_to_root_lifts_deeply_nested_window() {
        // Build: Internal{V, Leaf{1}, Internal{H, Leaf{2}, Internal{V, Leaf{3}, Leaf{4}}}}.
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        tree.insert_with_rect(4, Some(3), SCREEN);

        assert!(tree.promote_to_root(4));
        // After promote: root Internal with Leaf{4} as one direct child.
        match &tree {
            Node::Internal { left, right, .. } => {
                let leaf4_is_direct = matches!(left.as_ref(), Node::Leaf { window: Some(4) })
                    || matches!(right.as_ref(), Node::Leaf { window: Some(4) });
                assert!(leaf4_is_direct, "window 4 should be a direct root child");
            }
            other => panic!("expected Internal root, got {other:?}"),
        }
        // Remainder still contains 1, 2, 3 somewhere in the tree.
        assert!(tree.contains(1));
        assert!(tree.contains(2));
        assert!(tree.contains(3));
        assert!(tree.contains(4));
    }

    #[test]
    fn promote_to_root_stable_preserves_visual_side_right() {
        // Window 4 lives deep in root.right's subtree → visually on right.
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(2), SCREEN);
        tree.insert_with_rect(4, Some(3), SCREEN);

        let pre = tree.calculate_geometries(SCREEN);
        let pre_four = pre.iter().find(|(w, _)| *w == 4).unwrap().1;
        let (pre_four_cx, _) = pre_four.center();
        assert!(pre_four_cx > SCREEN.center().0, "test setup: 4 must start on right");

        assert!(tree.promote_to_root(4));
        let post = tree.calculate_geometries(SCREEN);
        let post_four = post.iter().find(|(w, _)| *w == 4).unwrap().1;
        let (post_four_cx, _) = post_four.center();
        assert!(post_four_cx > SCREEN.center().0, "4 should remain visually on the right");
    }

    #[test]
    fn promote_to_root_stable_preserves_visual_side_left() {
        // Build a tree where window 4 lives deep in root.left's subtree.
        // Insert sequence: 1, then 2 with target 1 (creates first split),
        // then 3 with target 1 (recurses into left to extend the left
        // subtree), then 4 with target 3 (extends the left subtree
        // further).
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        tree.insert_with_rect(3, Some(1), SCREEN);
        tree.insert_with_rect(4, Some(3), SCREEN);

        let pre = tree.calculate_geometries(SCREEN);
        let pre_four = pre.iter().find(|(w, _)| *w == 4).unwrap().1;
        let (pre_four_cx, _) = pre_four.center();
        assert!(pre_four_cx < SCREEN.center().0, "test setup: 4 must start on left");

        assert!(tree.promote_to_root(4));
        let post = tree.calculate_geometries(SCREEN);
        let post_four = post.iter().find(|(w, _)| *w == 4).unwrap().1;
        let (post_four_cx, _) = post_four.center();
        assert!(post_four_cx < SCREEN.center().0, "4 should remain visually on the left");
    }

    #[test]
    fn promote_to_root_returns_false_if_already_root() {
        // 2-window tree: both are direct children of root Internal.
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, SCREEN);
        tree.insert_with_rect(2, Some(1), SCREEN);
        assert!(!tree.promote_to_root(1));
        assert!(!tree.promote_to_root(2));
    }

    #[test]
    fn find_leaf_at_point_descends_horizontal_split() {
        // Tall rect → smart split = Horizontal.
        let tall = Rect::new(0.0, 0.0, 500.0, 1000.0);
        let mut tree = Node::empty();
        tree.insert_with_rect(1, None, tall);
        tree.insert_with_rect(2, Some(1), tall);
        // Top half is y in [0, 500), bottom in [500, 1000).
        assert_eq!(tree.find_leaf_at_point(tall, 250.0, 100.0), Some(1));
        assert_eq!(tree.find_leaf_at_point(tall, 250.0, 800.0), Some(2));
    }
}
