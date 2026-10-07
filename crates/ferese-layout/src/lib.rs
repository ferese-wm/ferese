use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

mod scrolling;

pub use scrolling::{Column, ColumnWidth, ScrollingLayout, ViewportFocusStrategy, ViewportTarget};

const MIN_SPLIT_RATIO: f64 = 0.05;
const MAX_SPLIT_RATIO: f64 = 0.95;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WindowId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NodeId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Window(WindowId),
    Split {
        axis: Axis,
        ratio: f64,
        first: NodeId,
        second: NodeId,
    },
    Stack {
        children: Vec<NodeId>,
        active: usize,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GapConfig {
    pub inner: f64,
    pub outer: f64,
    pub smart: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeConstraints {
    pub min_width: f64,
    pub min_height: f64,
    pub max_width: Option<f64>,
    pub max_height: Option<f64>,
}

impl Default for SizeConstraints {
    fn default() -> Self {
        Self {
            min_width: 1.0,
            min_height: 1.0,
            max_width: None,
            max_height: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConstraintKind {
    MinimumWidth,
    MinimumHeight,
    MaximumWidth,
    MaximumHeight,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConstraintWarning {
    pub window: WindowId,
    pub kind: ConstraintKind,
    pub requested: f64,
    pub assigned: f64,
}

#[derive(Debug, Default, PartialEq)]
pub struct LayoutResult {
    pub geometry: HashMap<WindowId, Rect>,
    pub warnings: Vec<ConstraintWarning>,
}

impl Default for GapConfig {
    fn default() -> Self {
        Self {
            inner: 10.0,
            outer: 4.0,
            smart: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutError {
    DuplicateWindow(WindowId),
    UnknownWindow(WindowId),
    InvalidTree(&'static str),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateWindow(window) => write!(formatter, "duplicate window {window:?}"),
            Self::UnknownWindow(window) => write!(formatter, "unknown window {window:?}"),
            Self::InvalidTree(reason) => write!(formatter, "invalid layout tree: {reason}"),
        }
    }
}

impl Error for LayoutError {}

#[derive(Clone, Debug, Default)]
pub struct LayoutTree {
    root: Option<NodeId>,
    nodes: HashMap<NodeId, Node>,
    parents: HashMap<NodeId, NodeId>,
    windows: HashMap<WindowId, NodeId>,
    next_node: u64,
}

impl LayoutTree {
    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn contains(&self, window: WindowId) -> bool {
        self.windows.contains_key(&window)
    }

    pub fn window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.windows.keys().copied()
    }

    pub fn preferred_window(&self) -> Option<WindowId> {
        let leaf = self.first_window(self.root?).ok()?;
        match self.nodes.get(&leaf)? {
            Node::Window(window) => Some(*window),
            _ => None,
        }
    }

    /// Include every structural leaf, even inactive stack members. Preserve
    /// stack child order when their positions coincide.
    pub fn window_ids_in_reading_order(&self, bounds: Rect) -> Result<Vec<WindowId>, LayoutError> {
        self.validate()?;
        let mut windows = Vec::with_capacity(self.windows.len());
        if let Some(root) = self.root {
            self.collect_window_positions(root, bounds, &mut windows)?;
        }
        windows.sort_by(|(_, left), (_, right)| left.x.total_cmp(&right.x).then_with(|| left.y.total_cmp(&right.y)));
        Ok(windows.into_iter().map(|(window, _)| window).collect())
    }

    pub fn insert(
        &mut self,
        window: WindowId,
        focused: Option<WindowId>,
        axis: Axis,
        ratio: f64,
    ) -> Result<NodeId, LayoutError> {
        if self.contains(window) {
            return Err(LayoutError::DuplicateWindow(window));
        }

        let Some(root) = self.root else {
            let window_node = self.insert_node(Node::Window(window));
            self.windows.insert(window, window_node);
            self.root = Some(window_node);
            return Ok(window_node);
        };

        let target = match focused {
            Some(focused) => self
                .windows
                .get(&focused)
                .copied()
                .ok_or(LayoutError::UnknownWindow(focused))?,
            None => self.first_window(root)?,
        };
        let old_parent = self.parents.get(&target).copied();
        let window_node = self.insert_node(Node::Window(window));
        self.windows.insert(window, window_node);
        let split = self.insert_node(Node::Split {
            axis,
            ratio: normalized_ratio(ratio),
            first: target,
            second: window_node,
        });

        self.parents.insert(target, split);
        self.parents.insert(window_node, split);

        if let Some(parent) = old_parent {
            self.replace_child(parent, target, split)?;
            self.parents.insert(split, parent);
        } else {
            self.root = Some(split);
        }

        debug_assert!(self.validate().is_ok());
        Ok(window_node)
    }

    pub fn remove(&mut self, window: WindowId) -> Result<(), LayoutError> {
        let leaf = self
            .windows
            .get(&window)
            .copied()
            .ok_or(LayoutError::UnknownWindow(window))?;
        let Some(parent) = self.parents.get(&leaf).copied() else {
            self.windows.remove(&window);
            self.nodes.remove(&leaf);
            self.root = None;
            return Ok(());
        };

        match self.nodes.get(&parent).cloned() {
            Some(Node::Split { first, second, .. }) => {
                let sibling = if first == leaf {
                    second
                } else if second == leaf {
                    first
                } else {
                    return Err(LayoutError::InvalidTree("split does not contain window"));
                };

                self.collapse_parent(parent, leaf, sibling)?;
            }
            Some(Node::Stack {
                mut children,
                mut active,
            }) => {
                let position = children
                    .iter()
                    .position(|child| *child == leaf)
                    .ok_or(LayoutError::InvalidTree("stack does not contain window"))?;

                if children.len() == 2 {
                    let sibling = children[1 - position];
                    self.collapse_parent(parent, leaf, sibling)?;
                } else {
                    children.remove(position);
                    if position < active {
                        active -= 1;
                    } else if position == active {
                        active = active.min(children.len() - 1);
                    }

                    self.windows.remove(&window);
                    self.parents.remove(&leaf);
                    self.nodes.remove(&leaf);
                    self.nodes.insert(parent, Node::Stack { children, active });
                }
            }
            Some(Node::Window(_)) => {
                return Err(LayoutError::InvalidTree("window parent is a window"));
            }
            None => return Err(LayoutError::InvalidTree("window parent is missing")),
        }

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn stack_window(&mut self, window: WindowId, target: WindowId) -> Result<(), LayoutError> {
        if window == target {
            return Ok(());
        }
        if !self.contains(target) {
            return Err(LayoutError::UnknownWindow(target));
        }
        if !self.contains(window) {
            return Err(LayoutError::UnknownWindow(window));
        }

        self.remove(window)?;

        let target_node = self
            .windows
            .get(&target)
            .copied()
            .ok_or(LayoutError::UnknownWindow(target))?;
        let old_parent = self.parents.get(&target_node).copied();
        let window_node = self.insert_node(Node::Window(window));
        let stack = self.insert_node(Node::Stack {
            children: vec![target_node, window_node],
            active: 1,
        });

        self.windows.insert(window, window_node);
        self.parents.insert(target_node, stack);
        self.parents.insert(window_node, stack);

        if let Some(parent) = old_parent {
            self.replace_child(parent, target_node, stack)?;
            self.parents.insert(stack, parent);
        } else {
            self.root = Some(stack);
        }

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn activate_window(&mut self, window: WindowId) -> Result<(), LayoutError> {
        let mut child = self
            .windows
            .get(&window)
            .copied()
            .ok_or(LayoutError::UnknownWindow(window))?;

        while let Some(parent) = self.parents.get(&child).copied() {
            if let Some(Node::Stack { children, active }) = self.nodes.get_mut(&parent) {
                *active = children
                    .iter()
                    .position(|candidate| *candidate == child)
                    .ok_or(LayoutError::InvalidTree("stack does not contain child"))?;
            }

            child = parent;
        }

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn geometry(&self, bounds: Rect) -> Result<HashMap<WindowId, Rect>, LayoutError> {
        let mut geometry = HashMap::new();

        if let Some(root) = self.root {
            self.layout_node(root, bounds, &mut geometry)?;
        }

        Ok(geometry)
    }

    pub fn geometry_with_gaps(&self, bounds: Rect, gaps: GapConfig) -> Result<HashMap<WindowId, Rect>, LayoutError> {
        let mut geometry = self.geometry(bounds)?;
        apply_gaps(&mut geometry, bounds, gaps);

        Ok(geometry)
    }

    pub fn geometry_with_constraints(
        &self,
        bounds: Rect,
        gaps: GapConfig,
        constraints: &HashMap<WindowId, SizeConstraints>,
        focused: Option<WindowId>,
    ) -> Result<LayoutResult, LayoutError> {
        let mut result = LayoutResult::default();

        if let Some(root) = self.root {
            self.layout_node_constrained(root, bounds, constraints, focused, &mut result.geometry)?;
        }

        apply_gaps(&mut result.geometry, bounds, gaps);

        for (window, rect) in &mut result.geometry {
            let constraint = constraints.get(window).copied().unwrap_or_default();
            let constraint = normalized_constraints(constraint);

            record_minimum_warnings(*window, *rect, constraint, &mut result.warnings);

            if let Some(maximum) = constraint.max_width
                && rect.width > maximum
            {
                result.warnings.push(ConstraintWarning {
                    window: *window,
                    kind: ConstraintKind::MaximumWidth,
                    requested: maximum,
                    assigned: rect.width,
                });
                rect.width = maximum;
            }
            if let Some(maximum) = constraint.max_height
                && rect.height > maximum
            {
                result.warnings.push(ConstraintWarning {
                    window: *window,
                    kind: ConstraintKind::MaximumHeight,
                    requested: maximum,
                    assigned: rect.height,
                });
                rect.height = maximum;
            }
        }

        result.warnings.sort_by_key(|warning| {
            let kind = match warning.kind {
                ConstraintKind::MinimumWidth => 0,
                ConstraintKind::MinimumHeight => 1,
                ConstraintKind::MaximumWidth => 2,
                ConstraintKind::MaximumHeight => 3,
            };
            (warning.window.0, kind)
        });

        Ok(result)
    }

    pub fn automatic_axis(&self, focused: Option<WindowId>, bounds: Rect) -> Result<Axis, LayoutError> {
        let Some(root) = self.root else {
            return Ok(Axis::Horizontal);
        };
        let target = match focused {
            Some(window) => window,
            None => match self.nodes.get(&self.first_window(root)?) {
                Some(Node::Window(window)) => *window,
                _ => return Err(LayoutError::InvalidTree("leaf is not a window")),
            },
        };
        let rect = self
            .geometry(bounds)?
            .remove(&target)
            .ok_or(LayoutError::UnknownWindow(target))?;

        Ok(if rect.width > rect.height {
            Axis::Horizontal
        } else {
            Axis::Vertical
        })
    }

    pub fn directional_neighbor(
        &self,
        window: WindowId,
        direction: Direction,
        bounds: Rect,
    ) -> Result<Option<WindowId>, LayoutError> {
        let geometry = self.geometry(bounds)?;
        let target = geometry
            .get(&window)
            .copied()
            .ok_or(LayoutError::UnknownWindow(window))?;
        let (target_x, target_y) = target.center();

        Ok(geometry
            .into_iter()
            .filter(|(candidate, _)| *candidate != window)
            .filter_map(|(candidate, rect)| {
                let (candidate_x, candidate_y) = rect.center();
                let (primary, secondary, aligned) = match direction {
                    Direction::Left if candidate_x < target_x => (
                        target_x - candidate_x,
                        (target_y - candidate_y).abs(),
                        intervals_overlap(target.y, target.height, rect.y, rect.height),
                    ),
                    Direction::Right if candidate_x > target_x => (
                        candidate_x - target_x,
                        (target_y - candidate_y).abs(),
                        intervals_overlap(target.y, target.height, rect.y, rect.height),
                    ),
                    Direction::Up if candidate_y < target_y => (
                        target_y - candidate_y,
                        (target_x - candidate_x).abs(),
                        intervals_overlap(target.x, target.width, rect.x, rect.width),
                    ),
                    Direction::Down if candidate_y > target_y => (
                        candidate_y - target_y,
                        (target_x - candidate_x).abs(),
                        intervals_overlap(target.x, target.width, rect.x, rect.width),
                    ),
                    _ => return None,
                };

                Some((candidate, primary, secondary, aligned))
            })
            .min_by(|left, right| {
                right
                    .3
                    .cmp(&left.3)
                    .then_with(|| left.1.total_cmp(&right.1))
                    .then_with(|| left.2.total_cmp(&right.2))
                    .then_with(|| left.0.0.cmp(&right.0.0))
            })
            .map(|(candidate, _, _, _)| candidate))
    }

    pub fn move_window(&mut self, window: WindowId, direction: Direction, bounds: Rect) -> Result<bool, LayoutError> {
        let Some(neighbor) = self.directional_neighbor(window, direction, bounds)? else {
            return Ok(false);
        };
        let window_node = self
            .windows
            .get(&window)
            .copied()
            .ok_or(LayoutError::UnknownWindow(window))?;
        let neighbor_node = self
            .windows
            .get(&neighbor)
            .copied()
            .ok_or(LayoutError::UnknownWindow(neighbor))?;

        self.nodes.insert(window_node, Node::Window(neighbor));
        self.nodes.insert(neighbor_node, Node::Window(window));
        self.windows.insert(window, neighbor_node);
        self.windows.insert(neighbor, window_node);

        debug_assert!(self.validate().is_ok());
        Ok(true)
    }

    pub fn resize_window(&mut self, window: WindowId, direction: Direction, amount: f64) -> Result<bool, LayoutError> {
        let mut child = self
            .windows
            .get(&window)
            .copied()
            .ok_or(LayoutError::UnknownWindow(window))?;
        let amount = valid_resize_amount(amount);

        if amount == 0.0 {
            return Ok(false);
        }

        while let Some(parent) = self.parents.get(&child).copied() {
            let adjustment = match self.nodes.get(&parent) {
                Some(Node::Split {
                    axis: Axis::Horizontal,
                    first,
                    ..
                }) if direction == Direction::Right && *first == child => Some(amount),
                Some(Node::Split {
                    axis: Axis::Horizontal,
                    second,
                    ..
                }) if direction == Direction::Left && *second == child => Some(-amount),
                Some(Node::Split {
                    axis: Axis::Vertical,
                    first,
                    ..
                }) if direction == Direction::Down && *first == child => Some(amount),
                Some(Node::Split {
                    axis: Axis::Vertical,
                    second,
                    ..
                }) if direction == Direction::Up && *second == child => Some(-amount),
                Some(Node::Split { .. } | Node::Stack { .. }) => None,
                Some(Node::Window(_)) => {
                    return Err(LayoutError::InvalidTree("window cannot be a parent"));
                }
                None => return Err(LayoutError::InvalidTree("parent node is missing")),
            };

            if let Some(adjustment) = adjustment {
                let Some(Node::Split { ratio, .. }) = self.nodes.get_mut(&parent) else {
                    return Err(LayoutError::InvalidTree("resize target is not a split"));
                };
                let resized = normalized_ratio(*ratio + adjustment);
                let changed = resized != *ratio;
                *ratio = resized;

                debug_assert!(self.validate().is_ok());
                return Ok(changed);
            }

            child = parent;
        }

        Ok(false)
    }

    pub fn validate(&self) -> Result<(), LayoutError> {
        let Some(root) = self.root else {
            return if self.nodes.is_empty() && self.parents.is_empty() && self.windows.is_empty() {
                Ok(())
            } else {
                Err(LayoutError::InvalidTree("empty root retains nodes"))
            };
        };
        if self.parents.contains_key(&root) {
            return Err(LayoutError::InvalidTree("root has a parent"));
        }

        let mut visited = HashSet::new();
        self.validate_node(root, None, &mut visited)?;

        if visited.len() != self.nodes.len() {
            return Err(LayoutError::InvalidTree("tree contains unreachable nodes"));
        }
        if self.windows.len()
            != self
                .nodes
                .values()
                .filter(|node| matches!(node, Node::Window(_)))
                .count()
        {
            return Err(LayoutError::InvalidTree("window index is inconsistent"));
        }

        Ok(())
    }

    fn insert_node(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.next_node);
        self.next_node += 1;
        self.nodes.insert(id, node);
        id
    }

    fn collect_window_positions(
        &self,
        node: NodeId,
        bounds: Rect,
        windows: &mut Vec<(WindowId, Rect)>,
    ) -> Result<(), LayoutError> {
        match self.nodes.get(&node) {
            Some(Node::Window(window)) => windows.push((*window, bounds)),
            Some(Node::Split {
                axis,
                ratio,
                first,
                second,
            }) => {
                let (first_bounds, second_bounds) = split_rect(bounds, *axis, *ratio);
                self.collect_window_positions(*first, first_bounds, windows)?;
                self.collect_window_positions(*second, second_bounds, windows)?;
            }
            Some(Node::Stack { children, .. }) => {
                for child in children {
                    self.collect_window_positions(*child, bounds, windows)?;
                }
            }
            None => return Err(LayoutError::InvalidTree("node is missing")),
        }
        Ok(())
    }

    fn first_window(&self, node: NodeId) -> Result<NodeId, LayoutError> {
        match self.nodes.get(&node) {
            Some(Node::Window(_)) => Ok(node),
            Some(Node::Split { first, .. }) => self.first_window(*first),
            Some(Node::Stack { children, active, .. }) => children
                .get(*active)
                .copied()
                .ok_or(LayoutError::InvalidTree("stack active index is invalid"))
                .and_then(|child| self.first_window(child)),
            None => Err(LayoutError::InvalidTree("node is missing")),
        }
    }

    fn replace_child(&mut self, parent: NodeId, old: NodeId, new: NodeId) -> Result<(), LayoutError> {
        match self.nodes.get_mut(&parent) {
            Some(Node::Split { first, .. }) if *first == old => *first = new,
            Some(Node::Split { second, .. }) if *second == old => *second = new,
            Some(Node::Stack { children, .. }) => {
                let child = children
                    .iter_mut()
                    .find(|child| **child == old)
                    .ok_or(LayoutError::InvalidTree("parent does not contain child"))?;
                *child = new;
            }
            _ => return Err(LayoutError::InvalidTree("parent does not contain child")),
        }

        Ok(())
    }

    fn collapse_parent(&mut self, parent: NodeId, leaf: NodeId, sibling: NodeId) -> Result<(), LayoutError> {
        let window = match self.nodes.get(&leaf) {
            Some(Node::Window(window)) => *window,
            _ => return Err(LayoutError::InvalidTree("removed leaf is not a window")),
        };
        let grandparent = self.parents.get(&parent).copied();

        self.windows.remove(&window);
        self.parents.remove(&leaf);
        self.parents.remove(&parent);
        self.nodes.remove(&leaf);
        self.nodes.remove(&parent);

        if let Some(grandparent) = grandparent {
            self.replace_child(grandparent, parent, sibling)?;
            self.parents.insert(sibling, grandparent);
        } else {
            self.parents.remove(&sibling);
            self.root = Some(sibling);
        }

        Ok(())
    }

    fn layout_node(
        &self,
        node: NodeId,
        bounds: Rect,
        geometry: &mut HashMap<WindowId, Rect>,
    ) -> Result<(), LayoutError> {
        match self.nodes.get(&node) {
            Some(Node::Window(window)) => {
                geometry.insert(*window, bounds);
            }
            Some(Node::Split {
                axis,
                ratio,
                first,
                second,
            }) => {
                let (first_bounds, second_bounds) = split_rect(bounds, *axis, *ratio);
                self.layout_node(*first, first_bounds, geometry)?;
                self.layout_node(*second, second_bounds, geometry)?;
            }
            Some(Node::Stack { children, active }) => {
                let child = children
                    .get(*active)
                    .ok_or(LayoutError::InvalidTree("stack active index is invalid"))?;
                self.layout_node(*child, bounds, geometry)?;
            }
            None => return Err(LayoutError::InvalidTree("node is missing")),
        }

        Ok(())
    }

    fn layout_node_constrained(
        &self,
        node: NodeId,
        bounds: Rect,
        constraints: &HashMap<WindowId, SizeConstraints>,
        focused: Option<WindowId>,
        geometry: &mut HashMap<WindowId, Rect>,
    ) -> Result<(), LayoutError> {
        match self.nodes.get(&node) {
            Some(Node::Window(window)) => {
                geometry.insert(*window, bounds);
            }
            Some(Node::Split {
                axis,
                ratio,
                first,
                second,
            }) => {
                let first_minimum = self.minimum_size(*first, constraints)?;
                let second_minimum = self.minimum_size(*second, constraints)?;
                let available = match axis {
                    Axis::Horizontal => bounds.width,
                    Axis::Vertical => bounds.height,
                }
                .max(0.0);
                let first_required = match axis {
                    Axis::Horizontal => first_minimum.0,
                    Axis::Vertical => first_minimum.1,
                };
                let second_required = match axis {
                    Axis::Horizontal => second_minimum.0,
                    Axis::Vertical => second_minimum.1,
                };
                let desired = available * ratio;
                let first_extent = if first_required + second_required <= available {
                    desired.clamp(first_required, available - second_required)
                } else if focused.is_some_and(|window| self.contains_window(*first, window)) {
                    first_required.min(available)
                } else {
                    (available - second_required).max(0.0)
                };
                let adjusted_ratio = if available > 0.0 { first_extent / available } else { 0.5 };
                let (first_bounds, second_bounds) = split_rect(bounds, *axis, adjusted_ratio);

                self.layout_node_constrained(*first, first_bounds, constraints, focused, geometry)?;
                self.layout_node_constrained(*second, second_bounds, constraints, focused, geometry)?;
            }
            Some(Node::Stack { children, active }) => {
                let child = children
                    .get(*active)
                    .ok_or(LayoutError::InvalidTree("stack active index is invalid"))?;
                self.layout_node_constrained(*child, bounds, constraints, focused, geometry)?;
            }
            None => return Err(LayoutError::InvalidTree("node is missing")),
        }

        Ok(())
    }

    fn minimum_size(
        &self,
        node: NodeId,
        constraints: &HashMap<WindowId, SizeConstraints>,
    ) -> Result<(f64, f64), LayoutError> {
        match self.nodes.get(&node) {
            Some(Node::Window(window)) => {
                let constraint = constraints.get(window).copied().unwrap_or_default();
                let constraint = normalized_constraints(constraint);
                Ok((constraint.min_width, constraint.min_height))
            }
            Some(Node::Split {
                axis, first, second, ..
            }) => {
                let first = self.minimum_size(*first, constraints)?;
                let second = self.minimum_size(*second, constraints)?;

                Ok(match axis {
                    Axis::Horizontal => (first.0 + second.0, first.1.max(second.1)),
                    Axis::Vertical => (first.0.max(second.0), first.1 + second.1),
                })
            }
            Some(Node::Stack { children, .. }) => {
                let mut minimum = (1.0_f64, 1.0_f64);

                for child in children {
                    let child = self.minimum_size(*child, constraints)?;
                    minimum.0 = minimum.0.max(child.0);
                    minimum.1 = minimum.1.max(child.1);
                }

                Ok(minimum)
            }
            None => Err(LayoutError::InvalidTree("node is missing")),
        }
    }

    fn contains_window(&self, node: NodeId, window: WindowId) -> bool {
        match self.nodes.get(&node) {
            Some(Node::Window(candidate)) => *candidate == window,
            Some(Node::Split { first, second, .. }) => {
                self.contains_window(*first, window) || self.contains_window(*second, window)
            }
            Some(Node::Stack { children, .. }) => children.iter().any(|child| self.contains_window(*child, window)),
            None => false,
        }
    }

    fn validate_node(
        &self,
        node: NodeId,
        expected_parent: Option<NodeId>,
        visited: &mut HashSet<NodeId>,
    ) -> Result<(), LayoutError> {
        if !visited.insert(node) {
            return Err(LayoutError::InvalidTree("tree contains a cycle"));
        }
        if self.parents.get(&node).copied() != expected_parent {
            return Err(LayoutError::InvalidTree("parent index is inconsistent"));
        }

        match self.nodes.get(&node) {
            Some(Node::Window(window)) => {
                if self.windows.get(window) != Some(&node) {
                    return Err(LayoutError::InvalidTree("window index is inconsistent"));
                }
            }
            Some(Node::Split {
                ratio, first, second, ..
            }) => {
                if first == second || !ratio.is_finite() || !(MIN_SPLIT_RATIO..=MAX_SPLIT_RATIO).contains(ratio) {
                    return Err(LayoutError::InvalidTree("split is invalid"));
                }

                self.validate_node(*first, Some(node), visited)?;
                self.validate_node(*second, Some(node), visited)?;
            }
            Some(Node::Stack { children, active }) => {
                if children.len() < 2 || *active >= children.len() {
                    return Err(LayoutError::InvalidTree("stack is invalid"));
                }

                for child in children {
                    self.validate_node(*child, Some(node), visited)?;
                }
            }
            None => return Err(LayoutError::InvalidTree("node is missing")),
        }

        Ok(())
    }
}

fn normalized_ratio(ratio: f64) -> f64 {
    if ratio.is_finite() {
        ratio.clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO)
    } else {
        0.5
    }
}

fn valid_gap(gap: f64) -> f64 {
    if gap.is_finite() { gap.max(0.0) } else { 0.0 }
}

fn valid_resize_amount(amount: f64) -> f64 {
    if amount.is_finite() { amount.abs() } else { 0.0 }
}

fn normalized_constraints(constraints: SizeConstraints) -> SizeConstraints {
    let min_width = finite_positive(constraints.min_width).unwrap_or(1.0);
    let min_height = finite_positive(constraints.min_height).unwrap_or(1.0);
    let max_width = constraints
        .max_width
        .and_then(finite_positive)
        .map(|maximum| maximum.max(min_width));
    let max_height = constraints
        .max_height
        .and_then(finite_positive)
        .map(|maximum| maximum.max(min_height));

    SizeConstraints {
        min_width,
        min_height,
        max_width,
        max_height,
    }
}

fn finite_positive(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

fn record_minimum_warnings(
    window: WindowId,
    rect: Rect,
    constraints: SizeConstraints,
    warnings: &mut Vec<ConstraintWarning>,
) {
    if rect.width < constraints.min_width {
        warnings.push(ConstraintWarning {
            window,
            kind: ConstraintKind::MinimumWidth,
            requested: constraints.min_width,
            assigned: rect.width,
        });
    }
    if rect.height < constraints.min_height {
        warnings.push(ConstraintWarning {
            window,
            kind: ConstraintKind::MinimumHeight,
            requested: constraints.min_height,
            assigned: rect.height,
        });
    }
}

fn apply_gaps(geometry: &mut HashMap<WindowId, Rect>, bounds: Rect, gaps: GapConfig) {
    let outer = if gaps.smart && geometry.len() == 1 {
        0.0
    } else {
        valid_gap(gaps.outer)
    };
    let half_inner = valid_gap(gaps.inner) / 2.0;
    let right = bounds.x + bounds.width;
    let bottom = bounds.y + bounds.height;

    for rect in geometry.values_mut() {
        let left_inset = if nearly_equal(rect.x, bounds.x) {
            outer
        } else {
            half_inner
        };
        let top_inset = if nearly_equal(rect.y, bounds.y) {
            outer
        } else {
            half_inner
        };
        let right_inset = if nearly_equal(rect.x + rect.width, right) {
            outer
        } else {
            half_inner
        };
        let bottom_inset = if nearly_equal(rect.y + rect.height, bottom) {
            outer
        } else {
            half_inner
        };

        rect.x += left_inset;
        rect.y += top_inset;
        rect.width = (rect.width - left_inset - right_inset).max(1.0);
        rect.height = (rect.height - top_inset - bottom_inset).max(1.0);
    }
}

fn nearly_equal(left: f64, right: f64) -> bool {
    (left - right).abs() < f64::EPSILON * left.abs().max(right.abs()).max(1.0)
}

fn intervals_overlap(first_start: f64, first_size: f64, second_start: f64, second_size: f64) -> bool {
    first_start < second_start + second_size && second_start < first_start + first_size
}

fn split_rect(bounds: Rect, axis: Axis, ratio: f64) -> (Rect, Rect) {
    match axis {
        Axis::Horizontal => {
            let boundary = bounds.x + bounds.width * ratio;
            let first = Rect::new(bounds.x, bounds.y, boundary - bounds.x, bounds.height);
            let second = Rect::new(boundary, bounds.y, bounds.x + bounds.width - boundary, bounds.height);

            (first, second)
        }
        Axis::Vertical => {
            let boundary = bounds.y + bounds.height * ratio;
            let first = Rect::new(bounds.x, bounds.y, bounds.width, boundary - bounds.y);
            let second = Rect::new(bounds.x, boundary, bounds.width, bounds.y + bounds.height - boundary);

            (first, second)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_splits_the_focused_leaf() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.insert(WindowId(3), Some(WindowId(1)), Axis::Vertical, 0.5)
            .unwrap();

        assert!(tree.validate().is_ok());
        assert_eq!(tree.geometry(Rect::new(0.0, 0.0, 100.0, 80.0)).unwrap().len(), 3);
    }

    #[test]
    fn removal_collapses_the_parent_split() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.remove(WindowId(1)).unwrap();

        assert!(tree.validate().is_ok());
        assert_eq!(tree.node(tree.root().unwrap()), Some(&Node::Window(WindowId(2))));
    }

    #[test]
    fn shared_split_boundary_is_exact() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 1.0 / 3.0).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 1.0 / 3.0)
            .unwrap();

        let geometry = tree.geometry(Rect::new(0.0, 0.0, 101.0, 80.0)).unwrap();
        let first = geometry[&WindowId(1)];
        let second = geometry[&WindowId(2)];

        assert_eq!(first.x + first.width, second.x);
        assert_eq!(second.x + second.width, 101.0);
    }

    #[test]
    fn rejects_duplicate_and_unknown_windows() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();

        assert_eq!(
            tree.insert(WindowId(1), None, Axis::Horizontal, 0.5),
            Err(LayoutError::DuplicateWindow(WindowId(1)))
        );
        assert_eq!(
            tree.insert(WindowId(2), Some(WindowId(9)), Axis::Horizontal, 0.5,),
            Err(LayoutError::UnknownWindow(WindowId(9)))
        );
    }

    #[test]
    fn non_finite_ratio_uses_balanced_split() {
        assert_eq!(normalized_ratio(f64::NAN), 0.5);
        assert_eq!(normalized_ratio(f64::INFINITY), 0.5);
    }

    #[test]
    fn automatic_axis_follows_focused_tile_shape() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();

        let wide = Rect::new(0.0, 0.0, 1_920.0, 1_080.0);
        assert_eq!(tree.automatic_axis(Some(WindowId(1)), wide).unwrap(), Axis::Horizontal);

        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        assert_eq!(tree.automatic_axis(Some(WindowId(1)), wide).unwrap(), Axis::Vertical);
    }

    #[test]
    fn gaps_preserve_outer_and_shared_spacing() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();

        let geometry = tree
            .geometry_with_gaps(Rect::new(0.0, 0.0, 100.0, 80.0), GapConfig::default())
            .unwrap();
        let first = geometry[&WindowId(1)];
        let second = geometry[&WindowId(2)];

        assert_eq!(first.x, 4.0);
        assert_eq!(second.x + second.width, 96.0);
        assert_eq!(second.x - (first.x + first.width), 10.0);
    }

    #[test]
    fn smart_gaps_remove_outer_gap_for_one_window() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();

        let bounds = Rect::new(0.0, 0.0, 100.0, 80.0);
        let geometry = tree
            .geometry_with_gaps(
                bounds,
                GapConfig {
                    smart: true,
                    ..GapConfig::default()
                },
            )
            .unwrap();

        assert_eq!(geometry[&WindowId(1)], bounds);
    }

    #[test]
    fn default_gaps_keep_outer_spacing_for_one_window() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();

        let geometry = tree
            .geometry_with_gaps(Rect::new(0.0, 0.0, 100.0, 80.0), GapConfig::default())
            .unwrap();

        assert_eq!(geometry[&WindowId(1)], Rect::new(4.0, 4.0, 92.0, 72.0));
    }

    #[test]
    fn directional_neighbor_prefers_primary_distance() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.insert(WindowId(3), Some(WindowId(2)), Axis::Vertical, 0.5)
            .unwrap();

        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            tree.directional_neighbor(WindowId(1), Direction::Right, bounds)
                .unwrap(),
            Some(WindowId(2))
        );
        assert_eq!(
            tree.directional_neighbor(WindowId(2), Direction::Down, bounds).unwrap(),
            Some(WindowId(3))
        );
        assert_eq!(
            tree.directional_neighbor(WindowId(1), Direction::Left, bounds).unwrap(),
            None
        );
    }

    #[test]
    fn directional_move_swaps_window_leaves() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        let bounds = Rect::new(0.0, 0.0, 100.0, 80.0);

        assert!(tree.move_window(WindowId(1), Direction::Right, bounds).unwrap());

        let geometry = tree.geometry(bounds).unwrap();
        assert_eq!(geometry[&WindowId(2)].x, 0.0);
        assert_eq!(geometry[&WindowId(1)].x, 50.0);
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn directional_move_at_boundary_is_a_noop() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();

        assert!(
            !tree
                .move_window(WindowId(1), Direction::Left, Rect::new(0.0, 0.0, 100.0, 80.0),)
                .unwrap()
        );
    }

    #[test]
    fn directional_resize_changes_the_nearest_matching_split() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.insert(WindowId(3), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);

        assert!(tree.resize_window(WindowId(1), Direction::Right, 0.1).unwrap());

        let geometry = tree.geometry(bounds).unwrap();
        assert_eq!(geometry[&WindowId(1)].width, 30.0);
        assert_eq!(geometry[&WindowId(3)].x, 30.0);
        assert_eq!(geometry[&WindowId(2)].x, 50.0);
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn directional_resize_walks_to_an_ancestor_and_clamps() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.insert(WindowId(3), Some(WindowId(1)), Axis::Vertical, 0.5)
            .unwrap();
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);

        assert!(tree.resize_window(WindowId(3), Direction::Right, 1.0).unwrap());
        assert!(!tree.resize_window(WindowId(3), Direction::Right, 1.0).unwrap());

        let geometry = tree.geometry(bounds).unwrap();
        assert_eq!(geometry[&WindowId(2)].x, 95.0);
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn directional_resize_at_an_outer_edge_is_a_noop() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();

        assert!(!tree.resize_window(WindowId(1), Direction::Left, 0.05).unwrap());
        assert!(!tree.resize_window(WindowId(1), Direction::Right, f64::NAN).unwrap());
    }

    #[test]
    fn stacking_windows_shows_only_the_active_child() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.stack_window(WindowId(2), WindowId(1)).unwrap();

        let geometry = tree.geometry(Rect::new(0.0, 0.0, 100.0, 80.0)).unwrap();
        assert_eq!(geometry.len(), 1);
        assert_eq!(geometry[&WindowId(2)], Rect::new(0.0, 0.0, 100.0, 80.0));
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn structural_reading_order_includes_all_stack_members_independently_of_activation() {
        let mut tree = LayoutTree::default();
        let ids = [WindowId(30), WindowId(10), WindowId(20), WindowId(5)];
        tree.insert(ids[0], None, Axis::Horizontal, 0.5).unwrap();
        for pair in ids.windows(2) {
            tree.insert(pair[1], Some(pair[0]), Axis::Horizontal, 0.5).unwrap();
            tree.stack_window(pair[1], pair[0]).unwrap();
        }
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        for active in ids {
            tree.activate_window(active).unwrap();
            assert_eq!(
                tree.geometry(bounds).unwrap().keys().copied().collect::<Vec<_>>(),
                vec![active]
            );
            assert_eq!(tree.window_ids_in_reading_order(bounds).unwrap(), ids);
            assert_eq!(tree.preferred_window(), Some(active));
            assert!(tree.validate().is_ok());
        }
    }

    #[test]
    fn flat_stack_reading_order_preserves_every_inactive_member() {
        let mut tree = LayoutTree::default();
        let ids = [WindowId(900), WindowId(7), WindowId(400), WindowId(10)];
        let children = ids
            .iter()
            .map(|window| {
                let leaf = tree.insert_node(Node::Window(*window));
                tree.windows.insert(*window, leaf);
                leaf
            })
            .collect::<Vec<_>>();
        let root = tree.insert_node(Node::Stack {
            children: children.clone(),
            active: 2,
        });
        for child in children {
            tree.parents.insert(child, root);
        }
        tree.root = Some(root);
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        assert!(tree.validate().is_ok());
        assert_eq!(tree.geometry(bounds).unwrap().len(), 1);
        assert_eq!(tree.preferred_window(), Some(ids[2]));
        assert_eq!(tree.window_ids_in_reading_order(bounds).unwrap(), ids);
    }

    #[test]
    fn structural_reading_order_retains_xy_order_through_splits_and_stacks() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(40), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(10), Some(WindowId(40)), Axis::Vertical, 0.5)
            .unwrap();
        tree.insert(WindowId(30), Some(WindowId(40)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.insert(WindowId(20), Some(WindowId(40)), Axis::Vertical, 0.5)
            .unwrap();
        tree.stack_window(WindowId(20), WindowId(40)).unwrap();
        tree.insert(WindowId(50), Some(WindowId(10)), Axis::Horizontal, 0.5)
            .unwrap();
        let bounds = Rect::new(25.0, 40.0, 1_000.0, 800.0);
        let expected = [40, 20, 10, 30, 50].map(WindowId);
        assert_eq!(tree.window_ids_in_reading_order(bounds).unwrap(), expected);
        assert_eq!(tree.geometry(bounds).unwrap().len(), 4);
        let collapsed = tree.window_ids_in_reading_order(Rect::default()).unwrap();
        assert_eq!(collapsed.len(), expected.len());
        assert_eq!(
            collapsed.into_iter().collect::<HashSet<_>>(),
            expected.into_iter().collect()
        );
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn tree_preference_follows_first_split_branch_and_active_stack_children() {
        let mut tree = LayoutTree::default();
        assert_eq!(tree.preferred_window(), None);
        assert!(tree.window_ids_in_reading_order(Rect::default()).unwrap().is_empty());
        tree.insert(WindowId(30), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(10), Some(WindowId(30)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.insert(WindowId(20), Some(WindowId(30)), Axis::Vertical, 0.5)
            .unwrap();
        tree.stack_window(WindowId(20), WindowId(30)).unwrap();
        assert_eq!(tree.preferred_window(), Some(WindowId(20)));
        tree.activate_window(WindowId(30)).unwrap();
        assert_eq!(tree.preferred_window(), Some(WindowId(30)));
        tree.activate_window(WindowId(10)).unwrap();
        assert_eq!(tree.preferred_window(), Some(WindowId(30)));
    }

    #[test]
    fn activating_a_hidden_stack_child_changes_visible_geometry() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.stack_window(WindowId(2), WindowId(1)).unwrap();
        tree.activate_window(WindowId(1)).unwrap();

        let geometry = tree.geometry(Rect::new(0.0, 0.0, 100.0, 80.0)).unwrap();
        assert_eq!(geometry.len(), 1);
        assert_eq!(geometry[&WindowId(1)], Rect::new(0.0, 0.0, 100.0, 80.0));
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn removing_from_a_stack_collapses_or_preserves_cardinality() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.stack_window(WindowId(2), WindowId(1)).unwrap();
        tree.insert(WindowId(3), Some(WindowId(2)), Axis::Vertical, 0.5)
            .unwrap();

        tree.remove(WindowId(3)).unwrap();
        tree.remove(WindowId(2)).unwrap();

        assert_eq!(tree.node(tree.root().unwrap()), Some(&Node::Window(WindowId(1))));
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn randomized_operation_sequences_preserve_tree_invariants() {
        let mut tree = LayoutTree::default();
        let mut windows = Vec::new();
        let mut next_window = 1;
        let mut random = 0x5eed_u64;
        let bounds = Rect::new(0.0, 0.0, 1_920.0, 1_080.0);

        for _ in 0..2_000 {
            random = random.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let operation = random % 6;

            match operation {
                0 if windows.len() < 32 => {
                    let window = WindowId(next_window);
                    next_window += 1;
                    let focused = choose_window(&windows, random.rotate_left(7));
                    let axis = if random & 1 == 0 {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    };

                    tree.insert(window, focused, axis, ratio_from_random(random)).unwrap();
                    windows.push(window);
                }
                1 if !windows.is_empty() => {
                    let index = random as usize % windows.len();
                    let window = windows.swap_remove(index);
                    tree.remove(window).unwrap();
                }
                2 if windows.len() >= 2 => {
                    let first = random as usize % windows.len();
                    let mut second = random.rotate_left(13) as usize % windows.len();
                    if first == second {
                        second = (second + 1) % windows.len();
                    }

                    tree.stack_window(windows[first], windows[second]).unwrap();
                }
                3 if !windows.is_empty() => {
                    let window = windows[random as usize % windows.len()];
                    tree.activate_window(window).unwrap();
                }
                4 if !windows.is_empty() => {
                    let window = windows[random as usize % windows.len()];
                    let _ = tree.move_window(window, random_direction(random), bounds);
                }
                5 if !windows.is_empty() => {
                    let window = windows[random as usize % windows.len()];
                    tree.resize_window(window, random_direction(random), 0.03).unwrap();
                }
                _ => {}
            }

            assert!(tree.validate().is_ok());
        }
    }

    #[test]
    fn constraints_adjust_split_boundaries_when_space_is_available() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.2).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.2)
            .unwrap();
        let constraints = HashMap::from([
            (
                WindowId(1),
                SizeConstraints {
                    min_width: 40.0,
                    ..SizeConstraints::default()
                },
            ),
            (
                WindowId(2),
                SizeConstraints {
                    min_width: 30.0,
                    ..SizeConstraints::default()
                },
            ),
        ]);
        let result = tree
            .geometry_with_constraints(
                Rect::new(0.0, 0.0, 100.0, 80.0),
                GapConfig {
                    inner: 0.0,
                    outer: 0.0,
                    smart: false,
                },
                &constraints,
                Some(WindowId(1)),
            )
            .unwrap();

        assert_eq!(result.geometry[&WindowId(1)].width, 40.0);
        assert_eq!(result.geometry[&WindowId(2)].width, 60.0);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn focused_window_wins_an_impossible_minimum_constraint_conflict() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        let constraints = HashMap::from([
            (
                WindowId(1),
                SizeConstraints {
                    min_width: 80.0,
                    ..SizeConstraints::default()
                },
            ),
            (
                WindowId(2),
                SizeConstraints {
                    min_width: 80.0,
                    ..SizeConstraints::default()
                },
            ),
        ]);
        let result = tree
            .geometry_with_constraints(
                Rect::new(0.0, 0.0, 100.0, 80.0),
                GapConfig {
                    inner: 0.0,
                    outer: 0.0,
                    smart: false,
                },
                &constraints,
                Some(WindowId(1)),
            )
            .unwrap();

        assert_eq!(result.geometry[&WindowId(1)].width, 80.0);
        assert_eq!(result.geometry[&WindowId(2)].width, 20.0);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].window, WindowId(2));
        assert_eq!(result.warnings[0].kind, ConstraintKind::MinimumWidth);
    }

    #[test]
    fn maximum_constraints_clip_without_overlapping_neighbors() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        let constraints = HashMap::from([(
            WindowId(1),
            SizeConstraints {
                max_width: Some(60.0),
                max_height: Some(40.0),
                ..SizeConstraints::default()
            },
        )]);
        let result = tree
            .geometry_with_constraints(
                Rect::new(0.0, 0.0, 100.0, 80.0),
                GapConfig::default(),
                &constraints,
                Some(WindowId(1)),
            )
            .unwrap();

        assert_eq!(result.geometry[&WindowId(1)], Rect::new(4.0, 4.0, 60.0, 40.0));
        assert_eq!(result.warnings.len(), 2);
    }

    fn choose_window(windows: &[WindowId], random: u64) -> Option<WindowId> {
        (!windows.is_empty()).then(|| windows[random as usize % windows.len()])
    }

    fn ratio_from_random(random: u64) -> f64 {
        0.2 + (random.rotate_left(19) % 601) as f64 / 1_000.0
    }

    fn random_direction(random: u64) -> Direction {
        match random.rotate_left(29) % 4 {
            0 => Direction::Left,
            1 => Direction::Right,
            2 => Direction::Up,
            _ => Direction::Down,
        }
    }
}
