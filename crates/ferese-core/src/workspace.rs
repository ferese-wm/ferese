use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use ferese_layout::{
    Axis, ColumnWidth, Direction, GapConfig, LayoutError, LayoutResult, LayoutTree, Rect, ScrollingLayout,
    SizeConstraints, ViewportFocusStrategy, WindowId,
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceId(pub u64);

#[derive(Debug)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub layout: WorkspaceLayout,
    pub floating: Vec<WindowId>,
    pub last_focused: Option<WindowId>,
    pub fullscreen: Option<WindowId>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LayoutMode {
    #[default]
    Scrolling,
    Tree,
}

#[derive(Clone, Debug)]
pub enum WorkspaceLayout {
    Scrolling(ScrollingLayout),
    Tree(LayoutTree),
}

impl Default for WorkspaceLayout {
    fn default() -> Self {
        Self::Scrolling(ScrollingLayout::default())
    }
}

impl WorkspaceLayout {
    pub fn new(mode: LayoutMode, default_column_width: ColumnWidth, focus_strategy: ViewportFocusStrategy) -> Self {
        match mode {
            LayoutMode::Scrolling => {
                let mut layout = ScrollingLayout::with_default_width(default_column_width);
                layout.set_focus_strategy(focus_strategy);
                Self::Scrolling(layout)
            }
            LayoutMode::Tree => Self::Tree(LayoutTree::default()),
        }
    }

    pub fn mode(&self) -> LayoutMode {
        match self {
            Self::Scrolling(_) => LayoutMode::Scrolling,
            Self::Tree(_) => LayoutMode::Tree,
        }
    }

    pub fn viewport_x(&self) -> Option<f64> {
        match self {
            Self::Scrolling(layout) => Some(layout.viewport_x()),
            Self::Tree(_) => None,
        }
    }

    pub fn preferred_window(&self) -> Option<WindowId> {
        match self {
            Self::Scrolling(layout) => layout.active_window(),
            Self::Tree(layout) => layout.preferred_window(),
        }
    }

    pub fn contains(&self, window: WindowId) -> bool {
        match self {
            Self::Scrolling(layout) => layout.contains(window),
            Self::Tree(layout) => layout.contains(window),
        }
    }

    pub fn window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        let scrolling = match self {
            Self::Scrolling(layout) => Some(layout.window_ids()),
            Self::Tree(_) => None,
        };
        let tree = match self {
            Self::Tree(layout) => Some(layout.window_ids()),
            Self::Scrolling(_) => None,
        };

        scrolling.into_iter().flatten().chain(tree.into_iter().flatten())
    }

    pub fn insert(
        &mut self,
        window: WindowId,
        focused: Option<WindowId>,
        axis: Axis,
        ratio: f64,
    ) -> Result<(), LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.insert(window, focused),
            Self::Tree(layout) => layout.insert(window, focused, axis, ratio).map(drop),
        }
    }

    pub fn remove(&mut self, window: WindowId) -> Result<(), LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.remove(window),
            Self::Tree(layout) => layout.remove(window),
        }
    }

    pub fn stack_window(&mut self, window: WindowId, target: WindowId) -> Result<(), LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.move_into_column(window, target),
            Self::Tree(layout) => layout.stack_window(window, target),
        }
    }

    pub fn activate_window(&mut self, window: WindowId) -> Result<(), LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.focus_and_reveal(window),
            Self::Tree(layout) => layout.activate_window(window),
        }
    }

    pub fn activate_window_without_reveal(&mut self, window: WindowId) -> Result<(), LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.focus_without_reveal(window),
            Self::Tree(layout) => layout.activate_window(window),
        }
    }

    pub fn geometry(&self, bounds: Rect) -> Result<HashMap<WindowId, Rect>, LayoutError> {
        match self {
            Self::Scrolling(layout) => {
                let mut layout = layout.clone();
                layout
                    .resolve_geometry_with_constraints(bounds, GapConfig::default(), &HashMap::new(), None)
                    .map(|result| result.geometry)
            }
            Self::Tree(layout) => layout.geometry(bounds),
        }
    }

    pub fn resolve_geometry_with_constraints(
        &mut self,
        bounds: Rect,
        gaps: GapConfig,
        constraints: &HashMap<WindowId, SizeConstraints>,
        focused: Option<WindowId>,
    ) -> Result<LayoutResult, LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.resolve_geometry_with_constraints(
                bounds,
                gaps,
                constraints,
                focused.filter(|window| layout.contains(*window)),
            ),
            Self::Tree(layout) => layout.geometry_with_constraints(bounds, gaps, constraints, focused),
        }
    }

    pub fn automatic_axis(&self, focused: Option<WindowId>, bounds: Rect) -> Result<Axis, LayoutError> {
        match self {
            Self::Scrolling(_) => Ok(Axis::Horizontal),
            Self::Tree(layout) => layout.automatic_axis(focused, bounds),
        }
    }

    pub fn directional_neighbor(
        &self,
        window: WindowId,
        direction: Direction,
        bounds: Rect,
    ) -> Result<Option<WindowId>, LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.directional_neighbor(window, direction),
            Self::Tree(layout) => layout.directional_neighbor(window, direction, bounds),
        }
    }

    pub fn move_window(&mut self, window: WindowId, direction: Direction, bounds: Rect) -> Result<bool, LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.move_window(window, direction),
            Self::Tree(layout) => layout.move_window(window, direction, bounds),
        }
    }

    pub fn resize_window(&mut self, window: WindowId, direction: Direction, amount: f64) -> Result<bool, LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.resize_window(window, direction, amount),
            Self::Tree(layout) => layout.resize_window(window, direction, amount),
        }
    }

    pub fn extract_window(&mut self, window: WindowId) -> Result<bool, LayoutError> {
        match self {
            Self::Scrolling(layout) => {
                let grouped = layout
                    .columns()
                    .iter()
                    .find(|column| column.windows.contains(&window))
                    .is_some_and(|column| column.windows.len() > 1);
                layout.extract_to_column(window)?;
                Ok(grouped)
            }
            Self::Tree(_) => Ok(false),
        }
    }

    pub fn cycle_column_width(&mut self, window: WindowId, presets: &[ColumnWidth]) -> Result<bool, LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.cycle_column_width(window, presets),
            Self::Tree(_) => Ok(false),
        }
    }

    pub fn center_window(
        &mut self,
        window: WindowId,
        bounds: Rect,
        gaps: GapConfig,
        constraints: &HashMap<WindowId, SizeConstraints>,
    ) -> Result<bool, LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.center_window(window, bounds, gaps, constraints),
            Self::Tree(_) => Ok(false),
        }
    }

    pub fn validate(&self) -> Result<(), LayoutError> {
        match self {
            Self::Scrolling(layout) => layout.validate(),
            Self::Tree(layout) => layout.validate(),
        }
    }

    pub fn set_mode(
        &mut self,
        mode: LayoutMode,
        bounds: Rect,
        focused: Option<WindowId>,
        default_column_width: ColumnWidth,
        focus_strategy: ViewportFocusStrategy,
    ) -> Result<bool, LayoutError> {
        if self.mode() == mode {
            return Ok(false);
        }

        let replacement = match self {
            Self::Scrolling(layout) => {
                let columns = layout.columns().to_vec();
                let mut tree = LayoutTree::default();
                let mut previous = None;

                for column in columns {
                    for (row, window) in column.windows.into_iter().enumerate() {
                        let axis = if row == 0 { Axis::Horizontal } else { Axis::Vertical };
                        tree.insert(window, previous, axis, 0.5)?;
                        previous = Some(window);
                    }
                }
                if let Some(focused) = focused.filter(|window| tree.contains(*window)) {
                    tree.activate_window(focused)?;
                }
                Self::Tree(tree)
            }
            Self::Tree(layout) => {
                let windows = layout.window_ids_in_reading_order(bounds)?;
                let mut scrolling = ScrollingLayout::with_default_width(default_column_width);
                scrolling.set_focus_strategy(focus_strategy);
                let mut previous = None;

                for window in windows {
                    scrolling.insert(window, previous)?;
                    previous = Some(window);
                }
                if let Some(focused) = focused.filter(|window| scrolling.contains(*window)) {
                    scrolling.focus(focused)?;
                }
                Self::Scrolling(scrolling)
            }
        };

        *self = replacement;
        debug_assert!(self.validate().is_ok());
        Ok(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowPlacement {
    Tiled,
    Floating { rect: Rect },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceError {
    InvalidNumericName(u32),
    UnknownWorkspace(WorkspaceId),
    DuplicateWindow(WindowId),
    InvalidState(&'static str),
    Layout(LayoutError),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNumericName(index) => {
                write!(formatter, "invalid workspace number {index}")
            }
            Self::UnknownWorkspace(workspace) => {
                write!(formatter, "unknown workspace {workspace:?}")
            }
            Self::DuplicateWindow(window) => write!(formatter, "duplicate window {window:?}"),
            Self::InvalidState(reason) => write!(formatter, "invalid workspace state: {reason}"),
            Self::Layout(error) => error.fmt(formatter),
        }
    }
}

impl Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Layout(error) => Some(error),
            _ => None,
        }
    }
}

impl From<LayoutError> for WorkspaceError {
    fn from(error: LayoutError) -> Self {
        Self::Layout(error)
    }
}

#[derive(Debug)]
pub struct WorkspaceSet {
    active: WorkspaceId,
    workspaces: HashMap<WorkspaceId, Workspace>,
    window_workspaces: HashMap<WindowId, WorkspaceId>,
    placements: HashMap<WindowId, WindowPlacement>,
    default_layout_mode: LayoutMode,
    default_column_width: ColumnWidth,
    scrolling_focus_strategy: ViewportFocusStrategy,
    next_id: u64,
}

impl Default for WorkspaceSet {
    fn default() -> Self {
        Self::new(
            LayoutMode::default(),
            ColumnWidth::default(),
            ViewportFocusStrategy::default(),
        )
    }
}

impl WorkspaceSet {
    pub fn new(
        default_layout_mode: LayoutMode,
        default_column_width: ColumnWidth,
        scrolling_focus_strategy: ViewportFocusStrategy,
    ) -> Self {
        let active = WorkspaceId(1);
        let workspace = Workspace {
            id: active,
            layout: WorkspaceLayout::new(default_layout_mode, default_column_width, scrolling_focus_strategy),
            floating: Vec::new(),
            last_focused: None,
            fullscreen: None,
        };
        let workspaces = HashMap::from([(active, workspace)]);

        Self {
            active,
            workspaces,
            window_workspaces: HashMap::new(),
            placements: HashMap::new(),
            default_layout_mode,
            default_column_width,
            scrolling_focus_strategy,
            next_id: 2,
        }
    }

    pub fn reconfigure_live(
        &mut self,
        mode: LayoutMode,
        width: ColumnWidth,
        strategy: ViewportFocusStrategy,
        bounds: &HashMap<WorkspaceId, Rect>,
    ) -> Result<(), LayoutError> {
        if mode == self.default_layout_mode
            && width == self.default_column_width
            && strategy == self.scrolling_focus_strategy
        {
            return Ok(());
        }
        let mut replacements = Vec::new();
        for workspace in self.workspaces.values() {
            let mut layout = workspace.layout.clone();
            if mode != self.default_layout_mode {
                layout.set_mode(
                    mode,
                    bounds
                        .get(&workspace.id)
                        .copied()
                        .unwrap_or(Rect::new(0., 0., 1920., 1080.)),
                    workspace.last_focused,
                    width,
                    strategy,
                )?;
            }
            if let WorkspaceLayout::Scrolling(scrolling) = &mut layout {
                if width != self.default_column_width {
                    let columns = scrolling
                        .columns()
                        .iter()
                        .filter(|c| c.width == self.default_column_width)
                        .filter_map(|c| c.windows.first().copied())
                        .collect::<Vec<_>>();
                    for window in columns {
                        scrolling.set_column_width(window, width)?;
                    }
                }
                scrolling.set_default_width(width);
                if strategy != self.scrolling_focus_strategy {
                    scrolling.set_focus_strategy(strategy);
                }
            }
            replacements.push((workspace.id, layout));
        }
        for (id, layout) in replacements {
            self.workspaces.get_mut(&id).unwrap().layout = layout;
        }
        self.default_layout_mode = mode;
        self.default_column_width = width;
        self.scrolling_focus_strategy = strategy;
        Ok(())
    }

    /// Update policy without moving windows or replacing existing column widths.
    pub fn reconfigure_defaults(&mut self, mode: LayoutMode, width: ColumnWidth, strategy: ViewportFocusStrategy) {
        self.default_layout_mode = mode;
        self.default_column_width = width;
        self.scrolling_focus_strategy = strategy;
        for workspace in self.workspaces.values_mut() {
            if let WorkspaceLayout::Scrolling(layout) = &mut workspace.layout {
                layout.set_default_width(width);
                layout.set_focus_strategy(strategy);
            }
        }
    }

    pub fn active_id(&self) -> WorkspaceId {
        self.active
    }

    pub fn active(&self) -> &Workspace {
        self.workspaces
            .get(&self.active)
            .expect("active workspace always exists")
    }

    pub fn active_mut(&mut self) -> &mut Workspace {
        self.workspaces
            .get_mut(&self.active)
            .expect("active workspace always exists")
    }

    pub fn activate(&mut self, workspace: WorkspaceId) -> Result<Option<WindowId>, WorkspaceError> {
        if !self.workspaces.contains_key(&workspace) {
            return Err(WorkspaceError::UnknownWorkspace(workspace));
        }

        self.active = workspace;

        debug_assert!(self.validate().is_ok());
        let focused = self.active().last_focused;
        let visible_floating =
            focused.filter(|window| matches!(self.placement(*window), Some(WindowPlacement::Floating { .. })));
        Ok(visible_floating.or(self.active().fullscreen).or(focused))
    }

    pub fn workspace(&self, id: WorkspaceId) -> Option<&Workspace> {
        self.workspaces.get(&id)
    }

    pub fn workspace_mut(&mut self, id: WorkspaceId) -> Option<&mut Workspace> {
        self.workspaces.get_mut(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Workspace> {
        self.workspaces.values()
    }

    pub fn workspace_for_window(&self, window: WindowId) -> Option<WorkspaceId> {
        self.window_workspaces.get(&window).copied()
    }

    pub fn placement(&self, window: WindowId) -> Option<WindowPlacement> {
        self.placements.get(&window).copied()
    }

    pub fn set_active_layout_mode(&mut self, mode: LayoutMode, bounds: Rect) -> Result<bool, WorkspaceError> {
        let default_column_width = self.default_column_width;
        let focus_strategy = self.scrolling_focus_strategy;
        let workspace = self.active_mut();
        let focused = tiled_focus(workspace);
        let changed = workspace
            .layout
            .set_mode(mode, bounds, focused, default_column_width, focus_strategy)?;

        debug_assert!(self.validate().is_ok());
        Ok(changed)
    }

    pub fn set_floating_rect(&mut self, window: WindowId, rect: Rect) -> Result<(), WorkspaceError> {
        let placement = self
            .placements
            .get_mut(&window)
            .ok_or(LayoutError::UnknownWindow(window))?;

        if !matches!(placement, WindowPlacement::Floating { .. }) {
            return Err(WorkspaceError::InvalidState("window is not floating"));
        }

        *placement = WindowPlacement::Floating {
            rect: normalized_floating_rect(rect),
        };

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn focus_window(&mut self, window: WindowId) -> Result<(), WorkspaceError> {
        self.focus_window_with_reveal(window, true)
    }

    pub fn focus_window_without_reveal(&mut self, window: WindowId) -> Result<(), WorkspaceError> {
        self.focus_window_with_reveal(window, false)
    }

    fn focus_window_with_reveal(&mut self, window: WindowId, reveal: bool) -> Result<(), WorkspaceError> {
        if self.workspace_for_window(window) != Some(self.active) {
            return Err(WorkspaceError::InvalidState(
                "focused window is not on the active workspace",
            ));
        }
        if self.active().fullscreen.is_some_and(|fullscreen| {
            fullscreen != window && !matches!(self.placement(window), Some(WindowPlacement::Floating { .. }))
        }) {
            return Err(WorkspaceError::InvalidState("focused window is hidden by fullscreen"));
        }

        if self.placement(window) == Some(WindowPlacement::Tiled) {
            if reveal {
                self.active_mut().layout.activate_window(window)?;
            } else {
                self.active_mut().layout.activate_window_without_reveal(window)?;
            }
        }
        self.active_mut().last_focused = Some(window);

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn stack_window(&mut self, window: WindowId, target: WindowId) -> Result<(), WorkspaceError> {
        let workspace = self
            .workspace_for_window(window)
            .ok_or(LayoutError::UnknownWindow(window))?;

        if self.workspace_for_window(target) != Some(workspace) {
            return Err(WorkspaceError::InvalidState(
                "stack windows belong to different workspaces",
            ));
        }
        if self.placement(window) != Some(WindowPlacement::Tiled)
            || self.placement(target) != Some(WindowPlacement::Tiled)
        {
            return Err(WorkspaceError::InvalidState("stack window is not tiled"));
        }

        let workspace = self
            .workspaces
            .get_mut(&workspace)
            .ok_or(WorkspaceError::InvalidState("window workspace is missing"))?;
        workspace.layout.stack_window(window, target)?;
        workspace.last_focused = Some(window);

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn extract_window(&mut self, window: WindowId) -> Result<bool, WorkspaceError> {
        if self.workspace_for_window(window) != Some(self.active)
            || self.placement(window) != Some(WindowPlacement::Tiled)
        {
            return Err(WorkspaceError::InvalidState(
                "extracted window is not tiled on the active workspace",
            ));
        }

        let changed = self.active_mut().layout.extract_window(window)?;
        debug_assert!(self.validate().is_ok());
        Ok(changed)
    }

    pub fn cycle_column_width(&mut self, window: WindowId, presets: &[ColumnWidth]) -> Result<bool, WorkspaceError> {
        if self.workspace_for_window(window) != Some(self.active)
            || self.placement(window) != Some(WindowPlacement::Tiled)
        {
            return Err(WorkspaceError::InvalidState(
                "resized window is not tiled on the active workspace",
            ));
        }

        let changed = self.active_mut().layout.cycle_column_width(window, presets)?;
        debug_assert!(self.validate().is_ok());
        Ok(changed)
    }

    pub fn center_window(
        &mut self,
        window: WindowId,
        bounds: Rect,
        gaps: GapConfig,
        constraints: &HashMap<WindowId, SizeConstraints>,
    ) -> Result<bool, WorkspaceError> {
        if self.workspace_for_window(window) != Some(self.active)
            || self.placement(window) != Some(WindowPlacement::Tiled)
        {
            return Err(WorkspaceError::InvalidState(
                "centered window is not tiled on the active workspace",
            ));
        }

        let changed = self
            .active_mut()
            .layout
            .center_window(window, bounds, gaps, constraints)?;
        debug_assert!(self.validate().is_ok());
        Ok(changed)
    }

    pub fn remove_empty(&mut self, id: WorkspaceId) -> bool {
        if id == self.active || !self.workspace(id).is_some_and(Workspace::is_empty) {
            return false;
        }

        self.workspaces.remove(&id);
        true
    }

    #[cfg(test)]
    pub(crate) fn fixture_workspace(&mut self, index: u32) -> Result<WorkspaceId, WorkspaceError> {
        if index == 0 {
            return Err(WorkspaceError::InvalidNumericName(index));
        }

        while self.next_id <= u64::from(index) {
            self.create_workspace();
        }

        let id = WorkspaceId(u64::from(index));
        self.workspace(id)
            .map(|_| id)
            .ok_or(WorkspaceError::UnknownWorkspace(id))
    }

    /// Reserve workspace identities in a pure topology plan without creating them.
    pub fn next_workspace_id(&self) -> WorkspaceId {
        WorkspaceId(self.next_id)
    }

    /// Creates an identity; display positions are assigned by the output owner.
    pub fn create_workspace(&mut self) -> WorkspaceId {
        let id = WorkspaceId(self.next_id);
        self.next_id = self.next_id.checked_add(1).expect("workspace IDs exhausted");
        self.workspaces.insert(
            id,
            Workspace {
                id,
                layout: WorkspaceLayout::new(
                    self.default_layout_mode,
                    self.default_column_width,
                    self.scrolling_focus_strategy,
                ),
                floating: Vec::new(),
                last_focused: None,
                fullscreen: None,
            },
        );

        debug_assert!(self.validate().is_ok());
        id
    }

    #[cfg(test)]
    fn switch_to_fixture(&mut self, index: u32) -> Result<Option<WindowId>, WorkspaceError> {
        let workspace = self.fixture_workspace(index)?;
        self.activate(workspace)
    }

    pub fn insert_window(&mut self, window: WindowId, axis: Axis, ratio: f64) -> Result<(), WorkspaceError> {
        if self.window_workspaces.contains_key(&window) {
            return Err(WorkspaceError::DuplicateWindow(window));
        }

        let workspace = self.active_mut();
        let focused = tiled_focus(workspace);
        workspace.layout.insert(window, focused, axis, ratio)?;
        if workspace.fullscreen.is_none() {
            workspace.last_focused = Some(window);
        }
        self.window_workspaces.insert(window, self.active);
        self.placements.insert(window, WindowPlacement::Tiled);

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn insert_floating_window(
        &mut self,
        window: WindowId,
        workspace_id: WorkspaceId,
        rect: Rect,
        focus: bool,
    ) -> Result<(), WorkspaceError> {
        if self.window_workspaces.contains_key(&window) {
            return Err(WorkspaceError::DuplicateWindow(window));
        }

        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or(WorkspaceError::UnknownWorkspace(workspace_id))?;
        workspace.floating.push(window);
        if focus {
            workspace.last_focused = Some(window);
        }
        self.window_workspaces.insert(window, workspace_id);
        self.placements.insert(
            window,
            WindowPlacement::Floating {
                rect: normalized_floating_rect(rect),
            },
        );

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn remove_window(&mut self, window: WindowId) -> Result<(), WorkspaceError> {
        let workspace_id = self
            .window_workspaces
            .remove(&window)
            .ok_or(LayoutError::UnknownWindow(window))?;
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or(WorkspaceError::UnknownWorkspace(workspace_id))?;

        match self.placements.remove(&window) {
            Some(WindowPlacement::Tiled) => workspace.layout.remove(window)?,
            Some(WindowPlacement::Floating { .. }) => {
                workspace.floating.retain(|candidate| *candidate != window);
            }
            None => return Err(LayoutError::UnknownWindow(window).into()),
        }

        if workspace.fullscreen == Some(window) {
            workspace.fullscreen = None;
        }

        if workspace.last_focused == Some(window) {
            workspace.last_focused = first_window(workspace);
        }

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    #[cfg(test)]
    fn move_window_to_fixture(
        &mut self,
        window: WindowId,
        index: u32,
        axis: Axis,
        ratio: f64,
    ) -> Result<WorkspaceId, WorkspaceError> {
        let destination = self.fixture_workspace(index)?;
        self.move_window_to_workspace(window, destination, axis, ratio)
    }

    pub fn move_window_to_workspace(
        &mut self,
        window: WindowId,
        destination_id: WorkspaceId,
        axis: Axis,
        ratio: f64,
    ) -> Result<WorkspaceId, WorkspaceError> {
        if self.workspace(destination_id).is_none() {
            return Err(WorkspaceError::UnknownWorkspace(destination_id));
        }

        let source_id = self
            .workspace_for_window(window)
            .ok_or(LayoutError::UnknownWindow(window))?;

        if source_id == destination_id {
            return Ok(destination_id);
        }

        let placement = self
            .placements
            .get(&window)
            .copied()
            .ok_or(LayoutError::UnknownWindow(window))?;
        let source = self
            .workspaces
            .get_mut(&source_id)
            .ok_or(WorkspaceError::UnknownWorkspace(source_id))?;

        match placement {
            WindowPlacement::Tiled => source.layout.remove(window)?,
            WindowPlacement::Floating { .. } => {
                source.floating.retain(|candidate| *candidate != window);
            }
        }

        let was_fullscreen = source.fullscreen == Some(window);
        if was_fullscreen {
            source.fullscreen = None;
        }

        if source.last_focused == Some(window) {
            source.last_focused = first_window(source);
        }

        let destination = self
            .workspaces
            .get_mut(&destination_id)
            .ok_or(WorkspaceError::UnknownWorkspace(destination_id))?;
        match placement {
            WindowPlacement::Tiled => {
                let focused = tiled_focus(destination);
                destination.layout.insert(window, focused, axis, ratio)?;
            }
            WindowPlacement::Floating { .. } => destination.floating.push(window),
        }
        if was_fullscreen {
            destination.fullscreen = Some(window);
        }
        if destination.fullscreen.is_none() || was_fullscreen {
            destination.last_focused = Some(window);
        }
        self.window_workspaces.insert(window, destination_id);

        debug_assert!(self.validate().is_ok());
        Ok(destination_id)
    }

    pub fn toggle_floating(
        &mut self,
        window: WindowId,
        floating_rect: Rect,
        axis: Axis,
        ratio: f64,
    ) -> Result<WindowPlacement, WorkspaceError> {
        let workspace_id = self
            .workspace_for_window(window)
            .ok_or(LayoutError::UnknownWindow(window))?;
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or(WorkspaceError::UnknownWorkspace(workspace_id))?;
        let placement = self
            .placements
            .get_mut(&window)
            .ok_or(LayoutError::UnknownWindow(window))?;

        *placement = match *placement {
            WindowPlacement::Tiled => {
                workspace.layout.remove(window)?;
                workspace.floating.push(window);
                WindowPlacement::Floating {
                    rect: normalized_floating_rect(floating_rect),
                }
            }
            WindowPlacement::Floating { .. } => {
                workspace.floating.retain(|candidate| *candidate != window);
                let focused = tiled_focus(workspace);
                workspace.layout.insert(window, focused, axis, ratio)?;
                WindowPlacement::Tiled
            }
        };

        let result = *placement;

        debug_assert!(self.validate().is_ok());
        Ok(result)
    }

    pub fn toggle_fullscreen(&mut self, window: WindowId) -> Result<bool, WorkspaceError> {
        let enabled = self
            .workspace_for_window(window)
            .and_then(|workspace| self.workspaces.get(&workspace))
            .is_none_or(|workspace| workspace.fullscreen != Some(window));

        self.set_fullscreen(window, enabled)?;
        Ok(enabled)
    }

    pub fn set_fullscreen(&mut self, window: WindowId, enabled: bool) -> Result<bool, WorkspaceError> {
        let workspace_id = self
            .workspace_for_window(window)
            .ok_or(LayoutError::UnknownWindow(window))?;
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or(WorkspaceError::UnknownWorkspace(workspace_id))?;

        let changed = if enabled {
            let changed = workspace.fullscreen != Some(window);
            workspace.fullscreen = Some(window);
            workspace.last_focused = Some(window);
            changed
        } else if workspace.fullscreen == Some(window) {
            workspace.fullscreen = None;
            true
        } else {
            false
        };

        debug_assert!(self.validate().is_ok());
        Ok(changed)
    }

    pub fn validate(&self) -> Result<(), WorkspaceError> {
        if !self.workspaces.contains_key(&self.active) {
            return Err(WorkspaceError::InvalidState("active workspace is missing"));
        }

        let mut seen_windows = HashSet::new();

        for (id, workspace) in &self.workspaces {
            if workspace.id != *id {
                return Err(WorkspaceError::InvalidState("workspace index is inconsistent"));
            }

            workspace.layout.validate()?;

            for window in workspace.layout.window_ids() {
                if !seen_windows.insert(window)
                    || self.window_workspaces.get(&window) != Some(id)
                    || self.placements.get(&window) != Some(&WindowPlacement::Tiled)
                {
                    return Err(WorkspaceError::InvalidState("window ownership is inconsistent"));
                }
            }

            let mut seen_floating = HashSet::new();

            for window in &workspace.floating {
                if !seen_floating.insert(*window)
                    || !seen_windows.insert(*window)
                    || self.window_workspaces.get(window) != Some(id)
                    || !matches!(self.placements.get(window), Some(WindowPlacement::Floating { .. }))
                {
                    return Err(WorkspaceError::InvalidState(
                        "floating window ownership is inconsistent",
                    ));
                }
            }

            if workspace
                .last_focused
                .is_some_and(|window| self.window_workspaces.get(&window) != Some(id))
            {
                return Err(WorkspaceError::InvalidState("workspace focus is inconsistent"));
            }

            if workspace
                .fullscreen
                .is_some_and(|window| self.window_workspaces.get(&window) != Some(id))
            {
                return Err(WorkspaceError::InvalidState(
                    "fullscreen window ownership is inconsistent",
                ));
            }
        }

        if seen_windows.len() != self.window_workspaces.len() || self.placements.len() != self.window_workspaces.len() {
            return Err(WorkspaceError::InvalidState("window index retains stale entries"));
        }

        Ok(())
    }
}

fn first_window(workspace: &Workspace) -> Option<WindowId> {
    workspace
        .layout
        .preferred_window()
        .or_else(|| workspace.floating.iter().copied().min_by_key(|window| window.0))
}

fn tiled_focus(workspace: &Workspace) -> Option<WindowId> {
    workspace
        .last_focused
        .filter(|window| workspace.layout.contains(*window))
        .or_else(|| workspace.layout.window_ids().min_by_key(|window| window.0))
}

fn normalized_floating_rect(rect: Rect) -> Rect {
    Rect::new(
        finite_or_zero(rect.x),
        finite_or_zero(rect.y),
        finite_or_zero(rect.width).max(1.0),
        finite_or_zero(rect.height).max(1.0),
    )
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

impl Workspace {
    pub fn window_count(&self) -> usize {
        self.layout.window_ids().count() + self.floating.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layout.window_ids().next().is_none() && self.floating.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn live_policy_updates_existing_layouts_and_default_widths_not_custom_widths() {
        let mut workspaces = WorkspaceSet::default();
        for id in 1..=3 {
            workspaces.insert_window(WindowId(id), Axis::Horizontal, 0.5).unwrap();
        }
        let WorkspaceLayout::Scrolling(layout) = &mut workspaces.active_mut().layout else {
            panic!()
        };
        layout
            .set_column_width(WindowId(2), ColumnWidth::Proportion(0.75))
            .unwrap();
        let bounds = HashMap::from([(workspaces.active_id(), Rect::new(0., 0., 1000., 800.))]);
        workspaces
            .reconfigure_live(
                LayoutMode::Scrolling,
                ColumnWidth::Proportion(0.4),
                ViewportFocusStrategy::Paged,
                &bounds,
            )
            .unwrap();
        let WorkspaceLayout::Scrolling(layout) = &workspaces.active().layout else {
            panic!()
        };
        assert_eq!(layout.columns()[0].width, ColumnWidth::Proportion(0.4));
        assert_eq!(layout.columns()[1].width, ColumnWidth::Proportion(0.75));
        workspaces
            .reconfigure_live(
                LayoutMode::Tree,
                ColumnWidth::Proportion(0.4),
                ViewportFocusStrategy::Paged,
                &bounds,
            )
            .unwrap();
        assert_eq!(workspaces.active().layout.mode(), LayoutMode::Tree);
        assert_eq!(workspaces.active().layout.window_ids().count(), 3);
        assert_eq!(workspaces.active().last_focused, Some(WindowId(3)));
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn reconfiguration_preserves_windows_and_current_widths_but_updates_defaults() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        let workspace = workspaces.active_id();
        workspaces.reconfigure_defaults(LayoutMode::Tree, ColumnWidth::Full, ViewportFocusStrategy::Minimal);
        let WorkspaceLayout::Scrolling(layout) = &workspaces.active().layout else {
            panic!("existing layout must survive");
        };
        assert_eq!(layout.columns()[0].width, ColumnWidth::Proportion(0.5));
        assert_eq!(layout.default_width(), ColumnWidth::Full);
        assert_eq!(workspaces.workspace_for_window(WindowId(1)), Some(workspace));
        workspaces.switch_to_fixture(2).unwrap();
        assert_eq!(workspaces.active().layout.mode(), LayoutMode::Tree);
        assert_eq!(workspaces.workspace_for_window(WindowId(1)), Some(workspace));
    }
    use super::*;

    #[test]
    fn workspace_creation_uses_fresh_ids() {
        let mut workspaces = WorkspaceSet::default();
        let first = workspaces.active_id();
        let second = workspaces.create_workspace();
        assert!(workspaces.remove_empty(second));

        let third = workspaces.create_workspace();
        assert_ne!(first, third);
        assert_ne!(second, third);
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn configured_layout_defaults_apply_to_new_workspaces() {
        let mut workspaces =
            WorkspaceSet::new(LayoutMode::Scrolling, ColumnWidth::Full, ViewportFocusStrategy::Minimal);
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        let first = workspaces
            .active_mut()
            .layout
            .resolve_geometry_with_constraints(
                Rect::new(0.0, 0.0, 1_000.0, 800.0),
                GapConfig::default(),
                &HashMap::new(),
                Some(WindowId(1)),
            )
            .unwrap();

        assert_eq!(first.geometry[&WindowId(1)], Rect::new(4.0, 4.0, 992.0, 792.0));
        workspaces.switch_to_fixture(2).unwrap();
        assert_eq!(workspaces.active().layout.mode(), LayoutMode::Scrolling);
    }

    #[test]
    fn switching_restores_workspace_focus() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();

        assert_eq!(workspaces.switch_to_fixture(2).unwrap(), None);
        workspaces.insert_window(WindowId(2), Axis::Horizontal, 0.5).unwrap();

        assert_eq!(workspaces.switch_to_fixture(1).unwrap(), Some(WindowId(1)));
        assert_eq!(workspaces.switch_to_fixture(2).unwrap(), Some(WindowId(2)));
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn moving_a_window_updates_membership_without_switching() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        workspaces.insert_window(WindowId(2), Axis::Horizontal, 0.5).unwrap();
        let first = workspaces.active_id();
        let second = workspaces
            .move_window_to_fixture(WindowId(2), 2, Axis::Horizontal, 0.5)
            .unwrap();

        assert_eq!(workspaces.active_id(), first);
        assert_eq!(workspaces.workspace_for_window(WindowId(2)), Some(second));
        assert!(!workspaces.workspace(first).unwrap().layout.contains(WindowId(2)));
        assert!(workspaces.workspace(second).unwrap().layout.contains(WindowId(2)));
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn removing_focus_chooses_a_deterministic_remaining_window() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(2), Axis::Horizontal, 0.5).unwrap();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        workspaces.remove_window(WindowId(1)).unwrap();

        assert_eq!(workspaces.active().last_focused, Some(WindowId(2)));
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn closing_a_scrolling_column_focuses_its_adjacent_column() {
        let mut workspaces = WorkspaceSet::default();
        for id in 1..=4 {
            workspaces.insert_window(WindowId(id), Axis::Horizontal, 0.5).unwrap();
        }
        workspaces.focus_window(WindowId(3)).unwrap();

        workspaces.remove_window(WindowId(3)).unwrap();

        assert_eq!(workspaces.active().last_focused, Some(WindowId(4)));
    }

    #[test]
    fn focus_rejects_a_window_on_an_inactive_workspace() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        workspaces
            .move_window_to_fixture(WindowId(1), 2, Axis::Horizontal, 0.5)
            .unwrap();

        assert_eq!(
            workspaces.focus_window(WindowId(1)),
            Err(WorkspaceError::InvalidState(
                "focused window is not on the active workspace"
            ))
        );
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn floating_toggle_removes_and_restores_tiled_membership() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        let rect = Rect::new(10.0, 20.0, 640.0, 480.0);

        assert_eq!(
            workspaces
                .toggle_floating(WindowId(1), rect, Axis::Horizontal, 0.5)
                .unwrap(),
            WindowPlacement::Floating { rect }
        );
        assert!(!workspaces.active().layout.contains(WindowId(1)));
        assert_eq!(workspaces.active().floating, vec![WindowId(1)]);

        assert_eq!(
            workspaces
                .toggle_floating(WindowId(1), rect, Axis::Horizontal, 0.5)
                .unwrap(),
            WindowPlacement::Tiled
        );
        assert!(workspaces.active().layout.contains(WindowId(1)));
        assert!(workspaces.active().floating.is_empty());
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn floating_geometry_updates_are_authoritative_and_validated() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        assert_eq!(
            workspaces.set_floating_rect(WindowId(1), Rect::default()),
            Err(WorkspaceError::InvalidState("window is not floating"))
        );

        workspaces
            .toggle_floating(WindowId(1), Rect::new(10.0, 20.0, 640.0, 480.0), Axis::Horizontal, 0.5)
            .unwrap();
        workspaces
            .set_floating_rect(WindowId(1), Rect::new(30.0, 40.0, f64::NAN, -2.0))
            .unwrap();

        assert_eq!(
            workspaces.placement(WindowId(1)),
            Some(WindowPlacement::Floating {
                rect: Rect::new(30.0, 40.0, 1.0, 1.0)
            })
        );
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn hover_focus_preserves_geometry_and_click_reveals_the_hovered_window() {
        for strategy in [
            ViewportFocusStrategy::Minimal,
            ViewportFocusStrategy::Center,
            ViewportFocusStrategy::Paged,
        ] {
            let mut workspaces = WorkspaceSet::new(LayoutMode::Scrolling, ColumnWidth::Proportion(0.5), strategy);
            for id in 1..=3 {
                workspaces.insert_window(WindowId(id), Axis::Horizontal, 0.5).unwrap();
            }
            let bounds = Rect::new(0.0, 0.0, 1000.0, 800.0);
            let gaps = GapConfig {
                inner: 0.0,
                outer: 0.0,
                smart: false,
            };
            workspaces.focus_window(WindowId(2)).unwrap();
            let before = workspaces
                .active_mut()
                .layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(WindowId(2)))
                .unwrap();
            let scroll = workspaces.active().layout.viewport_x();
            // Rapid right/middle/left hover must never scroll or skip a focus target,
            // even if a later client commit causes another layout calculation.
            for id in [3, 2, 1, 2, 3] {
                let id = WindowId(id);
                workspaces.focus_window_without_reveal(id).unwrap();
                assert_eq!(workspaces.active().last_focused, Some(id));
                let after = workspaces
                    .active_mut()
                    .layout
                    .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(id))
                    .unwrap();
                assert_eq!(before.geometry, after.geometry, "{strategy:?}");
                assert_eq!(scroll, workspaces.active().layout.viewport_x());
            }
            // Clicking the already-hovered window still requests a reveal.
            workspaces.focus_window(WindowId(3)).unwrap();
            let clicked = workspaces
                .active_mut()
                .layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(WindowId(3)))
                .unwrap();
            let rect = clicked.geometry[&WindowId(3)];
            assert!(rect.x >= 0.0 && rect.x + rect.width <= bounds.width);
            assert_ne!(scroll, workspaces.active().layout.viewport_x());
        }
    }

    #[test]
    fn opening_a_focused_float_preserves_tiled_geometry_and_scroll() {
        let mut workspaces = WorkspaceSet::new(
            LayoutMode::Scrolling,
            ColumnWidth::Proportion(0.5),
            ViewportFocusStrategy::Paged,
        );
        for id in 1..=4 {
            workspaces.insert_window(WindowId(id), Axis::Horizontal, 0.5).unwrap();
        }
        let bounds = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let before = workspaces
            .active_mut()
            .layout
            .resolve_geometry_with_constraints(bounds, GapConfig::default(), &HashMap::new(), Some(WindowId(4)))
            .unwrap();
        let scroll = workspaces.active().layout.viewport_x();
        workspaces
            .insert_floating_window(
                WindowId(5),
                workspaces.active_id(),
                Rect::new(100.0, 100.0, 600.0, 500.0),
                true,
            )
            .unwrap();
        let after = workspaces
            .active_mut()
            .layout
            .resolve_geometry_with_constraints(bounds, GapConfig::default(), &HashMap::new(), Some(WindowId(5)))
            .unwrap();
        assert_eq!(before.geometry, after.geometry);
        assert_eq!(scroll, workspaces.active().layout.viewport_x());
        assert_eq!(workspaces.active().last_focused, Some(WindowId(5)));
        assert!(!workspaces.active().layout.contains(WindowId(5)));
    }

    #[test]
    fn floating_window_can_follow_its_parent_workspace_without_focus() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        let parent_workspace = workspaces.active_id();
        workspaces.switch_to_fixture(2).unwrap();
        workspaces
            .insert_floating_window(
                WindowId(2),
                parent_workspace,
                Rect::new(20.0, 30.0, 400.0, 300.0),
                false,
            )
            .unwrap();

        assert_eq!(workspaces.workspace_for_window(WindowId(2)), Some(parent_workspace));
        assert_eq!(
            workspaces.workspace(parent_workspace).unwrap().last_focused,
            Some(WindowId(1))
        );
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn fullscreen_preserves_underlying_placement() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        let before = workspaces.placement(WindowId(1));

        assert!(workspaces.toggle_fullscreen(WindowId(1)).unwrap());
        assert_eq!(workspaces.active().fullscreen, Some(WindowId(1)));
        assert_eq!(workspaces.placement(WindowId(1)), before);
        assert!(!workspaces.set_fullscreen(WindowId(1), true).unwrap());

        assert!(!workspaces.toggle_fullscreen(WindowId(1)).unwrap());
        assert_eq!(workspaces.active().fullscreen, None);
        assert_eq!(workspaces.placement(WindowId(1)), before);
        assert!(!workspaces.set_fullscreen(WindowId(1), false).unwrap());
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn mapping_under_fullscreen_does_not_change_focus() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        workspaces.toggle_fullscreen(WindowId(1)).unwrap();
        workspaces.insert_window(WindowId(2), Axis::Horizontal, 0.5).unwrap();

        assert_eq!(workspaces.active().last_focused, Some(WindowId(1)));
        assert_eq!(workspaces.active().fullscreen, Some(WindowId(1)));
        assert!(workspaces.active().layout.contains(WindowId(2)));
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn floating_dialog_can_focus_above_fullscreen() {
        let mut workspaces = WorkspaceSet::default();
        let workspace = workspaces.active_id();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        workspaces.toggle_fullscreen(WindowId(1)).unwrap();
        workspaces
            .insert_floating_window(WindowId(2), workspace, Rect::new(100.0, 100.0, 500.0, 400.0), true)
            .unwrap();

        assert_eq!(workspaces.active().last_focused, Some(WindowId(2)));
        assert!(workspaces.focus_window(WindowId(2)).is_ok());
        workspaces.switch_to_fixture(2).unwrap();
        assert_eq!(workspaces.activate(workspace).unwrap(), Some(WindowId(2)));
        assert_eq!(workspaces.active().fullscreen, Some(WindowId(1)));
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn switching_to_fullscreen_restores_the_visible_window() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        workspaces.toggle_fullscreen(WindowId(1)).unwrap();
        workspaces.switch_to_fixture(2).unwrap();
        workspaces.insert_window(WindowId(2), Axis::Horizontal, 0.5).unwrap();
        workspaces
            .move_window_to_fixture(WindowId(2), 1, Axis::Horizontal, 0.5)
            .unwrap();

        assert_eq!(workspaces.switch_to_fixture(1).unwrap(), Some(WindowId(1)));
        assert_eq!(workspaces.active().last_focused, Some(WindowId(1)));
        assert_eq!(
            workspaces.focus_window(WindowId(2)),
            Err(WorkspaceError::InvalidState("focused window is hidden by fullscreen"))
        );
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn grouping_tiled_windows_preserves_workspace_membership() {
        let mut workspaces = WorkspaceSet::default();
        workspaces.insert_window(WindowId(1), Axis::Horizontal, 0.5).unwrap();
        workspaces.insert_window(WindowId(2), Axis::Horizontal, 0.5).unwrap();
        workspaces.stack_window(WindowId(2), WindowId(1)).unwrap();

        assert_eq!(workspaces.active().last_focused, Some(WindowId(2)));
        assert_eq!(workspaces.active().layout.mode(), LayoutMode::Scrolling);
        assert_eq!(
            workspaces
                .active()
                .layout
                .geometry(Rect::new(0.0, 0.0, 100.0, 80.0))
                .unwrap()
                .len(),
            2
        );
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn layout_mode_conversion_preserves_membership_and_focus() {
        let mut workspaces = WorkspaceSet::default();
        for id in 1..=4 {
            workspaces.insert_window(WindowId(id), Axis::Horizontal, 0.5).unwrap();
        }
        workspaces.focus_window(WindowId(2)).unwrap();
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);

        assert!(workspaces.set_active_layout_mode(LayoutMode::Tree, bounds).unwrap());
        assert_eq!(workspaces.active().layout.mode(), LayoutMode::Tree);
        assert_eq!(workspaces.active().last_focused, Some(WindowId(2)));
        assert_eq!(
            workspaces.active().layout.window_ids().collect::<HashSet<_>>(),
            HashSet::from([WindowId(1), WindowId(2), WindowId(3), WindowId(4)])
        );

        assert!(
            workspaces
                .set_active_layout_mode(LayoutMode::Scrolling, bounds)
                .unwrap()
        );
        assert_eq!(workspaces.active().layout.mode(), LayoutMode::Scrolling);
        assert_eq!(workspaces.active().last_focused, Some(WindowId(2)));
        assert!(workspaces.validate().is_ok());
    }

    fn tree_stack_workspaces(ids: &[WindowId]) -> WorkspaceSet {
        let mut workspaces = WorkspaceSet::new(
            LayoutMode::Tree,
            ColumnWidth::Proportion(0.5),
            ViewportFocusStrategy::Minimal,
        );
        workspaces.insert_window(ids[0], Axis::Horizontal, 0.5).unwrap();
        for pair in ids.windows(2) {
            workspaces.insert_window(pair[1], Axis::Horizontal, 0.5).unwrap();
            workspaces.stack_window(pair[1], pair[0]).unwrap();
        }
        workspaces
    }

    #[test]
    fn stacked_tree_mode_conversions_preserve_membership_focus_and_order() {
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let all_ids = [WindowId(30), WindowId(10), WindowId(20), WindowId(5), WindowId(40)];
        for count in [2, 3, 5] {
            let ids = &all_ids[..count];
            let expected = ids.iter().copied().collect::<HashSet<_>>();
            for focused in ids {
                // Fresh maps have independent hash seeds; structural ordering
                // must give the same result for each conversion.
                for _ in 0..8 {
                    let mut workspaces = tree_stack_workspaces(ids);
                    workspaces.focus_window(*focused).unwrap();
                    // A saved valid focus may refer to an inactive stack member.
                    let WorkspaceLayout::Tree(tree) = &mut workspaces.active_mut().layout else {
                        panic!()
                    };
                    tree.activate_window(ids[1]).unwrap();
                    assert_eq!(tree.geometry(bounds).unwrap().len(), 1);
                    assert_eq!(workspaces.active().last_focused, Some(*focused));
                    assert!(workspaces.validate().is_ok());

                    for mode in [LayoutMode::Scrolling, LayoutMode::Tree, LayoutMode::Scrolling] {
                        assert!(workspaces.set_active_layout_mode(mode, bounds).unwrap());
                        let windows = workspaces.active().layout.window_ids().collect::<Vec<_>>();
                        assert_eq!(windows.len(), ids.len());
                        assert_eq!(windows.iter().copied().collect::<HashSet<_>>(), expected);
                        assert_eq!(workspaces.active().last_focused, Some(*focused));
                        assert!(workspaces.validate().is_ok());
                        if let WorkspaceLayout::Scrolling(layout) = &workspaces.active().layout {
                            assert_eq!(windows, ids);
                            assert_eq!(layout.active_window(), Some(*focused));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn nested_tree_splits_and_stacks_survive_round_trip_conversion() {
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let mut workspaces = tree_stack_workspaces(&[WindowId(30), WindowId(10), WindowId(20)]);
        workspaces.focus_window(WindowId(10)).unwrap();
        workspaces.insert_window(WindowId(5), Axis::Vertical, 0.5).unwrap();
        workspaces.insert_window(WindowId(40), Axis::Horizontal, 0.5).unwrap();
        workspaces.insert_window(WindowId(50), Axis::Vertical, 0.5).unwrap();
        workspaces.stack_window(WindowId(50), WindowId(40)).unwrap();
        workspaces.focus_window(WindowId(40)).unwrap();
        let before = workspaces.active().layout.window_ids().collect::<HashSet<_>>();
        assert_eq!(before.len(), 6);
        assert!(workspaces.active().layout.geometry(bounds).unwrap().len() < before.len());
        for mode in [LayoutMode::Scrolling, LayoutMode::Tree, LayoutMode::Scrolling] {
            assert!(workspaces.set_active_layout_mode(mode, bounds).unwrap());
            let windows = workspaces.active().layout.window_ids().collect::<Vec<_>>();
            assert_eq!(windows.len(), before.len());
            assert_eq!(windows.into_iter().collect::<HashSet<_>>(), before);
            assert_eq!(workspaces.active().last_focused, Some(WindowId(40)));
            assert!(workspaces.validate().is_ok());
            if let WorkspaceLayout::Scrolling(layout) = &workspaces.active().layout {
                assert_eq!(layout.active_window(), Some(WindowId(40)));
            }
        }
    }

    #[test]
    fn tree_preferred_window_is_deterministic_without_a_saved_focus() {
        for _ in 0..32 {
            let mut workspaces = tree_stack_workspaces(&[WindowId(30), WindowId(20), WindowId(40)]);
            workspaces.active_mut().last_focused = None;
            assert_eq!(workspaces.active().layout.preferred_window(), Some(WindowId(40)));
            workspaces.insert_window(WindowId(10), Axis::Horizontal, 0.5).unwrap();
            workspaces.remove_window(WindowId(10)).unwrap();
            assert_eq!(workspaces.active().last_focused, Some(WindowId(40)));
            assert_eq!(workspaces.active().layout.preferred_window(), Some(WindowId(40)));
            assert!(workspaces.validate().is_ok());
        }
    }

    #[test]
    fn scrolling_column_commands_preserve_workspace_state() {
        let mut workspaces = WorkspaceSet::default();
        for id in 1..=3 {
            workspaces.insert_window(WindowId(id), Axis::Horizontal, 0.5).unwrap();
        }

        workspaces.stack_window(WindowId(3), WindowId(2)).unwrap();
        assert!(workspaces.extract_window(WindowId(3)).unwrap());
        assert!(
            workspaces
                .cycle_column_width(WindowId(3), &[ColumnWidth::Proportion(0.5), ColumnWidth::Full],)
                .unwrap()
        );
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let _ = workspaces
            .center_window(WindowId(3), bounds, GapConfig::default(), &HashMap::new())
            .unwrap();

        assert_eq!(workspaces.active().last_focused, Some(WindowId(3)));
        assert!(workspaces.validate().is_ok());
    }

    #[test]
    fn randomized_workspace_sequences_preserve_all_invariants() {
        let mut workspaces = WorkspaceSet::default();
        let mut windows = Vec::new();
        let mut next_window = 1;
        let mut random = 0xc0de_cafe_u64;

        for _ in 0..2_000 {
            random = random
                .wrapping_mul(2_862_933_555_777_941_757)
                .wrapping_add(3_037_000_493);

            match random % 7 {
                0 if windows.len() < 48 => {
                    let window = WindowId(next_window);
                    next_window += 1;
                    workspaces.insert_window(window, random_axis(random), 0.5).unwrap();
                    windows.push(window);
                }
                1 if !windows.is_empty() => {
                    let index = random as usize % windows.len();
                    let window = windows.swap_remove(index);
                    workspaces.remove_window(window).unwrap();
                }
                2 => {
                    workspaces
                        .switch_to_fixture((random.rotate_left(11) % 4 + 1) as u32)
                        .unwrap();
                }
                3 if !windows.is_empty() => {
                    let window = windows[random as usize % windows.len()];
                    workspaces
                        .move_window_to_fixture(
                            window,
                            (random.rotate_left(17) % 4 + 1) as u32,
                            random_axis(random),
                            0.5,
                        )
                        .unwrap();
                }
                4 if !windows.is_empty() => {
                    let window = windows[random as usize % windows.len()];
                    workspaces
                        .toggle_floating(window, random_rect(random), random_axis(random), 0.5)
                        .unwrap();
                }
                5 if !windows.is_empty() => {
                    let window = windows[random as usize % windows.len()];
                    workspaces.toggle_fullscreen(window).unwrap();
                }
                6 if !windows.is_empty() => {
                    let candidates = windows
                        .iter()
                        .copied()
                        .filter(|window| {
                            workspaces.workspace_for_window(*window) == Some(workspaces.active_id())
                                && workspaces
                                    .active()
                                    .fullscreen
                                    .is_none_or(|fullscreen| fullscreen == *window)
                        })
                        .collect::<Vec<_>>();

                    if let Some(window) = candidates.get(random as usize % candidates.len().max(1)) {
                        workspaces.focus_window(*window).unwrap();
                    }
                }
                _ => {}
            }

            assert!(workspaces.validate().is_ok());
        }
    }

    fn random_axis(random: u64) -> Axis {
        if random & 1 == 0 {
            Axis::Horizontal
        } else {
            Axis::Vertical
        }
    }

    fn random_rect(random: u64) -> Rect {
        Rect::new(
            (random % 400) as f64,
            (random.rotate_left(9) % 300) as f64,
            (random.rotate_left(21) % 900 + 1) as f64,
            (random.rotate_left(33) % 700 + 1) as f64,
        )
    }
}
