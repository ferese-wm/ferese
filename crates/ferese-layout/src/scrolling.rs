use std::collections::{HashMap, HashSet};

use crate::{
    ConstraintKind, ConstraintWarning, Direction, GapConfig, LayoutError, LayoutResult, Rect, SizeConstraints,
    WindowId, normalized_constraints, record_minimum_warnings,
};

const DEFAULT_WIDTH: f64 = 0.5;
const MIN_COLUMN_WIDTH: f64 = 0.1;
const MAX_COLUMN_WIDTH: f64 = 2.0;
const MIN_ROW_HEIGHT: f64 = 0.05;
const REVEAL_EPSILON: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColumnWidth {
    Proportion(f64),
    Fixed(f64),
    Full,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ViewportFocusStrategy {
    #[default]
    Minimal,
    Center,
    Paged,
}

impl Default for ColumnWidth {
    fn default() -> Self {
        Self::Proportion(DEFAULT_WIDTH)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub windows: Vec<WindowId>,
    pub active: usize,
    pub width: ColumnWidth,
    pub heights: Vec<f64>,
}

impl Column {
    fn new(window: WindowId, width: ColumnWidth) -> Self {
        Self {
            windows: vec![window],
            active: 0,
            width: normalized_width(width),
            heights: vec![1.0],
        }
    }

    fn normalize_heights(&mut self) {
        if self.windows.is_empty() {
            self.heights.clear();
            return;
        }
        if self.heights.len() != self.windows.len() {
            self.heights.resize(self.windows.len(), 1.0);
        }
        self.heights.iter_mut().for_each(|height| {
            if !height.is_finite() || *height <= 0.0 {
                *height = 1.0;
            }
        });
        let total = self.heights.iter().sum::<f64>();
        self.heights.iter_mut().for_each(|height| *height /= total);
        self.active = self.active.min(self.windows.len() - 1);
    }
}

// The last calculation that selected the viewport target. Keep it separate
// from pending focus requests so an unrelated relayout cannot lose provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportTarget {
    columns: Vec<(Vec<WindowId>, f64)>,
    inner: f64,
    viewport_width: f64,
    strategy: ViewportFocusStrategy,
    start: f64,
    request: ViewportRequest,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ViewportRequest {
    Reveal(usize),
    WidthCycle(usize),
    Slide(usize, usize),
    Center(usize, Option<f64>),
}

impl ViewportTarget {
    fn target(&self, replacements: &HashMap<usize, f64>, start: Option<f64>) -> f64 {
        let start = start.unwrap_or(self.start);
        let mut x = 0.0;
        let positions = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, (_, width))| {
                let width = replacements.get(&index).copied().unwrap_or(*width);
                let position = (x, width);
                x += width + self.inner;
                position
            })
            .collect::<Vec<_>>();
        let mut layout = ScrollingLayout {
            viewport_x: start,
            focus_strategy: self.strategy,
            ..Default::default()
        };
        match self.request {
            ViewportRequest::Reveal(active) => layout.reveal_column(&positions, self.viewport_width, active),
            ViewportRequest::WidthCycle(active) => {
                layout.active_column = Some(active);
                layout.retarget_after_width_cycle(&positions, self.viewport_width);
            }
            ViewportRequest::Slide(from, to) => {
                let (last, width) = positions.last().copied().unwrap_or_default();
                layout.viewport_x = (start + positions[to].0 - positions[from].0)
                    .clamp(0.0, (last + width - self.viewport_width).max(0.0));
            }
            ViewportRequest::Center(active, maximum) => {
                let width = maximum.map_or(positions[active].1, |maximum| positions[active].1.min(maximum));
                layout.viewport_x = positions[active].0 + width / 2.0 - self.viewport_width / 2.0
            }
        }
        layout.viewport_x
    }

    /// Re-evaluate the same target with pending allocated widths held back.
    /// A start-relative request can also use the previous held target.
    pub fn with_held_widths(&self, widths: &[(WindowId, f64, f64)], start: Option<f64>) -> f64 {
        let replacements = widths
            .iter()
            .filter_map(|(window, source, target)| {
                self.columns
                    .iter()
                    .position(|(windows, width)| windows.contains(window) && (*width - target).abs() < 0.001)
                    .map(|index| (index, *source))
            })
            .collect::<HashMap<_, _>>();
        self.target(&replacements, start)
    }

    pub fn uses_previous_target(&self) -> bool {
        matches!(self.request, ViewportRequest::Slide(..))
            || (matches!(self.request, ViewportRequest::Reveal(..)) && self.strategy == ViewportFocusStrategy::Minimal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PendingFocusRequest {
    Reveal(WindowId),
    Slide { from: WindowId, to: WindowId },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PendingViewportRequest {
    Focus(PendingFocusRequest),
    // Width cycling takes precedence. Keep the focus request only in case a
    // subsequent no-op cycle cancels the width request before layout resolves.
    WidthCycle(Option<PendingFocusRequest>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScrollingLayout {
    columns: Vec<Column>,
    active_column: Option<usize>,
    viewport_x: f64,
    default_width: ColumnWidth,
    focus_strategy: ViewportFocusStrategy,
    pending_viewport: Option<PendingViewportRequest>,
    last_viewport_width: Option<f64>,
    allocated_widths: HashMap<WindowId, f64>,
    viewport_basis: Option<ViewportTarget>,
}

impl Default for ScrollingLayout {
    fn default() -> Self {
        Self {
            columns: Vec::new(),
            active_column: None,
            viewport_x: 0.0,
            default_width: ColumnWidth::default(),
            focus_strategy: ViewportFocusStrategy::Minimal,
            pending_viewport: None,
            last_viewport_width: None,
            allocated_widths: HashMap::new(),
            viewport_basis: None,
        }
    }
}

impl ScrollingLayout {
    fn pending_focus(&self) -> Option<PendingFocusRequest> {
        match self.pending_viewport {
            Some(PendingViewportRequest::Focus(request)) => Some(request),
            Some(PendingViewportRequest::WidthCycle(request)) => request,
            None => None,
        }
    }

    fn request_focus(&mut self, request: Option<PendingFocusRequest>) {
        self.pending_viewport = if matches!(self.pending_viewport, Some(PendingViewportRequest::WidthCycle(_))) {
            Some(PendingViewportRequest::WidthCycle(request))
        } else {
            request.map(PendingViewportRequest::Focus)
        };
    }

    fn request_reveal(&mut self, window: Option<WindowId>) {
        if !matches!(self.pending_focus(), Some(PendingFocusRequest::Slide { .. })) {
            self.request_focus(window.map(PendingFocusRequest::Reveal));
        }
    }

    pub fn set_default_width(&mut self, width: ColumnWidth) {
        self.default_width = normalized_width(width);
    }

    pub fn with_default_width(default_width: ColumnWidth) -> Self {
        Self {
            default_width: normalized_width(default_width),
            ..Self::default()
        }
    }

    pub fn set_focus_strategy(&mut self, strategy: ViewportFocusStrategy) {
        self.focus_strategy = strategy;
        self.request_focus(self.active_window().map(PendingFocusRequest::Reveal));
    }

    pub fn default_width(&self) -> ColumnWidth {
        self.default_width
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn active_column(&self) -> Option<usize> {
        self.active_column
    }

    pub fn active_window(&self) -> Option<WindowId> {
        let column = self.columns.get(self.active_column?)?;
        column.windows.get(column.active).copied()
    }

    pub fn viewport_x(&self) -> f64 {
        self.viewport_x
    }

    pub fn allocated_column_width(&self, window: WindowId) -> Option<f64> {
        self.allocated_widths.get(&window).copied()
    }

    /// Whether this target changes when the pending column width is held back.
    /// Allocated widths, rather than cell-grid/native buffer sizes, own the strip.
    pub fn viewport_depends_on_width(&self, window: WindowId, source: f64, target: f64) -> bool {
        (source - target).abs() > 0.001
            && self
                .viewport_basis
                .as_ref()
                .map(|basis| basis.with_held_widths(&[(window, source, target)], None))
                .is_some_and(|held| (held - self.viewport_x).abs() > 0.001)
    }

    pub fn viewport_target(&self) -> Option<&ViewportTarget> {
        self.viewport_basis.as_ref()
    }

    fn record_viewport_basis(
        &mut self,
        positions: &[(f64, f64)],
        inner: f64,
        viewport_width: f64,
        request: ViewportRequest,
    ) {
        let next = ViewportTarget {
            columns: self
                .columns
                .iter()
                .zip(positions)
                .map(|(column, (_, width))| (column.windows.clone(), *width))
                .collect(),
            inner,
            viewport_width,
            strategy: self.focus_strategy,
            start: self.viewport_x,
            request,
        };
        self.viewport_basis = Some(next);
    }

    pub fn contains(&self, window: WindowId) -> bool {
        self.window_location(window).is_some()
    }

    pub fn window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.columns.iter().flat_map(|column| column.windows.iter().copied())
    }

    pub fn insert(&mut self, window: WindowId, focused: Option<WindowId>) -> Result<(), LayoutError> {
        if self.contains(window) {
            return Err(LayoutError::DuplicateWindow(window));
        }

        let index = focused
            .and_then(|focused| self.window_location(focused))
            .map(|(column, _)| column + 1)
            .unwrap_or(self.columns.len());
        self.columns.insert(index, Column::new(window, self.default_width));
        self.active_column = Some(index);
        self.request_reveal((self.columns.len() > 1).then_some(window));

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn remove(&mut self, window: WindowId) -> Result<(), LayoutError> {
        let (column_index, window_index) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;
        let column = &mut self.columns[column_index];
        let removed_active_column = self.active_column == Some(column_index) && column.windows.len() == 1;
        column.windows.remove(window_index);
        column.heights.remove(window_index);

        if column.windows.is_empty() {
            self.columns.remove(column_index);
            self.active_column = match self.active_column {
                None => None,
                Some(_) if self.columns.is_empty() => None,
                Some(active) if active > column_index => Some(active - 1),
                Some(active) => Some(active.min(self.columns.len() - 1)),
            };
        } else {
            column.active = column.active.min(column.windows.len() - 1);
            column.normalize_heights();
        }
        if self.pending_focus() == Some(PendingFocusRequest::Reveal(window))
            || removed_active_column
            || self.focus_strategy == ViewportFocusStrategy::Paged
        {
            self.request_reveal(self.active_window());
        }

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn focus(&mut self, window: WindowId) -> Result<(), LayoutError> {
        let (column, index) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;
        let changed_column = self.active_column != Some(column);
        self.active_column = Some(column);
        self.columns[column].active = index;
        if changed_column {
            self.request_reveal(Some(window));
        }
        Ok(())
    }

    /// Hover changes the active window without requesting viewport movement.
    /// Keep any earlier explicit reveal attached to its original window.
    pub fn focus_without_reveal(&mut self, window: WindowId) -> Result<(), LayoutError> {
        let (column, index) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;
        self.active_column = Some(column);
        self.columns[column].active = index;
        Ok(())
    }

    /// An explicit selection must reveal even a window already focused by hover.
    pub fn focus_and_reveal(&mut self, window: WindowId) -> Result<(), LayoutError> {
        self.focus_without_reveal(window)?;
        self.request_focus(Some(PendingFocusRequest::Reveal(window)));
        Ok(())
    }

    pub fn slide_focus_from(&mut self, previous: WindowId, focused: WindowId) -> Result<(), LayoutError> {
        self.window_location(previous)
            .ok_or(LayoutError::UnknownWindow(previous))?;
        if self.focus_strategy == ViewportFocusStrategy::Minimal {
            return self.focus_and_reveal(focused);
        }
        self.focus_without_reveal(focused)?;
        self.request_focus(Some(PendingFocusRequest::Slide {
            from: previous,
            to: focused,
        }));
        Ok(())
    }

    pub fn move_into_column(&mut self, window: WindowId, target: WindowId) -> Result<(), LayoutError> {
        if window == target {
            return Ok(());
        }
        self.window_location(target).ok_or(LayoutError::UnknownWindow(target))?;
        self.remove(window)?;
        let (target_column, target_index) = self
            .window_location(target)
            .expect("removing another window preserves the target");
        let column = &mut self.columns[target_column];
        let insertion = (target_index + 1).min(column.windows.len());
        column.windows.insert(insertion, window);
        column.heights.insert(insertion, 1.0);
        column.active = insertion;
        column.normalize_heights();
        self.active_column = Some(target_column);
        self.request_reveal(self.active_window());

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn extract_to_column(&mut self, window: WindowId) -> Result<(), LayoutError> {
        let (source, _) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;
        if self.columns[source].windows.len() == 1 {
            self.active_column = Some(source);
            return Ok(());
        }

        self.remove(window)?;
        let insertion = (source + 1).min(self.columns.len());
        self.columns.insert(insertion, Column::new(window, self.default_width));
        self.active_column = Some(insertion);
        self.request_reveal(self.active_window());

        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub fn set_column_width(&mut self, window: WindowId, width: ColumnWidth) -> Result<(), LayoutError> {
        let (column, _) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;
        self.columns[column].width = normalized_width(width);
        self.request_reveal(self.active_window());
        Ok(())
    }

    pub fn cycle_column_width(&mut self, window: WindowId, presets: &[ColumnWidth]) -> Result<bool, LayoutError> {
        let (column, _) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;
        let presets = presets.iter().copied().map(normalized_width).collect::<Vec<_>>();
        if presets.is_empty() {
            return Ok(false);
        }

        let current = normalized_width(self.columns[column].width);
        let next = presets
            .iter()
            .position(|preset| *preset == current)
            .map(|index| presets[(index + 1) % presets.len()])
            .unwrap_or(presets[0]);
        self.columns[column].width = next;
        let focus = self.pending_focus();
        self.pending_viewport = if next != current {
            Some(PendingViewportRequest::WidthCycle(focus))
        } else {
            focus.map(PendingViewportRequest::Focus)
        };
        Ok(next != current)
    }

    pub fn center_window(
        &mut self,
        window: WindowId,
        bounds: Rect,
        gaps: GapConfig,
        constraints: &HashMap<WindowId, SizeConstraints>,
    ) -> Result<bool, LayoutError> {
        let before = self.viewport_x;
        let result = self.resolve_geometry_with_constraints(bounds, gaps, constraints, Some(window))?;
        let rect = result
            .geometry
            .get(&window)
            .copied()
            .ok_or(LayoutError::UnknownWindow(window))?;
        let viewport_center = bounds.x + bounds.width / 2.0;
        let window_center = rect.x + rect.width / 2.0;
        self.viewport_x += window_center - viewport_center;
        let active = self.window_location(window).unwrap().0;
        let inner = finite_nonnegative(gaps.inner);
        let positions = self
            .columns
            .iter()
            .scan(0.0, |x, column| {
                let width = self.allocated_widths[&column.windows[0]];
                let position = (*x, width);
                *x += width + inner;
                Some(position)
            })
            .collect::<Vec<_>>();
        let viewport_width = self.last_viewport_width.unwrap();
        self.record_viewport_basis(
            &positions,
            inner,
            viewport_width,
            ViewportRequest::Center(
                active,
                constraints
                    .get(&window)
                    .copied()
                    .map(normalized_constraints)
                    .unwrap_or_default()
                    .max_width
                    .map(|maximum| {
                        maximum.max(
                            constraints
                                .get(&window)
                                .copied()
                                .map(normalized_constraints)
                                .unwrap_or_default()
                                .min_width,
                        )
                    }),
            ),
        );
        self.resolve_geometry_with_constraints(bounds, gaps, constraints, Some(window))?;

        Ok(self.viewport_x != before)
    }

    pub fn directional_neighbor(
        &self,
        window: WindowId,
        direction: Direction,
    ) -> Result<Option<WindowId>, LayoutError> {
        let (column, row) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;
        let neighbor = match direction {
            Direction::Up => row.checked_sub(1).map(|row| self.columns[column].windows[row]),
            Direction::Down => self.columns[column].windows.get(row + 1).copied(),
            Direction::Left => column.checked_sub(1).map(|column| {
                let column = &self.columns[column];
                column.windows[column.active]
            }),
            Direction::Right => self.columns.get(column + 1).map(|column| column.windows[column.active]),
        };

        Ok(neighbor)
    }

    pub fn move_window(&mut self, window: WindowId, direction: Direction) -> Result<bool, LayoutError> {
        let (column, row) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;

        match direction {
            Direction::Up if row > 0 => {
                self.columns[column].windows.swap(row, row - 1);
                self.columns[column].heights.swap(row, row - 1);
                self.columns[column].active = row - 1;
            }
            Direction::Down if row + 1 < self.columns[column].windows.len() => {
                self.columns[column].windows.swap(row, row + 1);
                self.columns[column].heights.swap(row, row + 1);
                self.columns[column].active = row + 1;
            }
            Direction::Left if column > 0 => self.move_horizontally(window, column - 1)?,
            Direction::Right if column + 1 < self.columns.len() => self.move_horizontally(window, column + 1)?,
            _ => return Ok(false),
        }

        debug_assert!(self.validate().is_ok());
        Ok(true)
    }

    pub fn resize_window(&mut self, window: WindowId, direction: Direction, amount: f64) -> Result<bool, LayoutError> {
        let (column, row) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;
        let amount = if amount.is_finite() { amount.abs() } else { 0.0 };
        if amount == 0.0 {
            return Ok(false);
        }

        let changed = match direction {
            Direction::Left | Direction::Right => {
                let current = match normalized_width(self.columns[column].width) {
                    ColumnWidth::Proportion(value) => value,
                    ColumnWidth::Fixed(_) | ColumnWidth::Full => DEFAULT_WIDTH,
                };
                let adjustment = if direction == Direction::Right { amount } else { -amount };
                let resized = (current + adjustment).clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH);
                self.columns[column].width = ColumnWidth::Proportion(resized);
                resized != current
            }
            Direction::Up if row > 0 => resize_rows(&mut self.columns[column], row, row - 1, amount),
            Direction::Down if row + 1 < self.columns[column].windows.len() => {
                resize_rows(&mut self.columns[column], row, row + 1, amount)
            }
            Direction::Up | Direction::Down => false,
        };

        debug_assert!(self.validate().is_ok());
        Ok(changed)
    }

    /// Consume pending viewport policy requests and solve the logical layout.
    pub fn resolve_geometry_with_constraints(
        &mut self,
        bounds: Rect,
        gaps: GapConfig,
        constraints: &HashMap<WindowId, SizeConstraints>,
        focused: Option<WindowId>,
    ) -> Result<LayoutResult, LayoutError> {
        if let Some(focused) = focused {
            self.focus(focused)?;
        }

        self.validate_scalars()?;
        debug_assert!(self.validate().is_ok());

        if self.columns.is_empty() {
            self.viewport_x = 0.0;
            return Ok(LayoutResult::default());
        }

        let window_count = self.window_ids().count();
        let inner = finite_nonnegative(gaps.inner);
        let outer = if gaps.smart && window_count == 1 {
            0.0
        } else {
            finite_nonnegative(gaps.outer)
        };
        let viewport_width = (bounds.width - outer * 2.0).max(1.0);
        let viewport_height = (bounds.height - outer * 2.0).max(1.0);
        // Strip coordinates start at the viewport's inset left edge. Inner
        // gaps belong to column positions; outer margins inset the viewport
        // and are added back exactly once when placing windows on the output.
        let mut column_positions = Vec::with_capacity(self.columns.len());
        let mut next_column_x = 0.0;

        for column in &self.columns {
            let requested = match normalized_width(column.width) {
                ColumnWidth::Proportion(proportion) => ((viewport_width + inner) * proportion - inner).max(1.0),
                ColumnWidth::Fixed(width) => width,
                ColumnWidth::Full => viewport_width,
            };
            let minimum = column
                .windows
                .iter()
                .map(|window| {
                    constraints
                        .get(window)
                        .copied()
                        .map(normalized_constraints)
                        .unwrap_or_default()
                        .min_width
                })
                .fold(1.0_f64, f64::max);
            let width = requested.max(minimum).max(1.0);
            column_positions.push((next_column_x, width));
            next_column_x += width + inner;
        }
        self.allocated_widths = self
            .columns
            .iter()
            .zip(&column_positions)
            .flat_map(|(column, (_, width))| column.windows.iter().map(move |id| (*id, *width)))
            .collect();
        let viewport_resized = self.last_viewport_width != Some(viewport_width);
        self.last_viewport_width = Some(viewport_width);
        match self.pending_viewport.take() {
            Some(PendingViewportRequest::WidthCycle(_)) => {
                if let Some(active) = self.active_column {
                    self.record_viewport_basis(
                        &column_positions,
                        inner,
                        viewport_width,
                        ViewportRequest::WidthCycle(active),
                    );
                }
                self.retarget_after_width_cycle(&column_positions, viewport_width);
            }
            Some(PendingViewportRequest::Focus(PendingFocusRequest::Slide {
                from: previous,
                to: focused,
            })) => {
                if let (Some((from, _)), Some((to, _))) =
                    (self.window_location(previous), self.window_location(focused))
                {
                    self.record_viewport_basis(
                        &column_positions,
                        inner,
                        viewport_width,
                        ViewportRequest::Slide(from, to),
                    );
                    let last_end = column_positions
                        .last()
                        .map(|(start, width)| start + width)
                        .unwrap_or(0.0);
                    let max_offset = (last_end - viewport_width).max(0.0);
                    self.viewport_x =
                        (self.viewport_x + column_positions[to].0 - column_positions[from].0).clamp(0.0, max_offset);
                }
            }
            request => {
                let reveal = match request {
                    Some(PendingViewportRequest::Focus(PendingFocusRequest::Reveal(window))) => Some(window),
                    None => None,
                    _ => unreachable!(),
                }
                .or_else(|| {
                    (viewport_resized && self.focus_strategy == ViewportFocusStrategy::Paged)
                        .then(|| self.active_window())
                        .flatten()
                });
                if let Some((column, _)) = reveal.and_then(|window| self.window_location(window)) {
                    self.record_viewport_basis(
                        &column_positions,
                        inner,
                        viewport_width,
                        ViewportRequest::Reveal(column),
                    );
                    self.reveal_column(&column_positions, viewport_width, column);
                }
            }
        }

        let mut result = LayoutResult::default();
        for (column_index, column) in self.columns.iter().enumerate() {
            let (column_x, column_width) = column_positions[column_index];
            let available_height = (viewport_height - inner * (column.windows.len().saturating_sub(1) as f64)).max(1.0);
            let mut y = bounds.y + outer;

            for (window_index, window) in column.windows.iter().enumerate() {
                let mut rect = Rect::new(
                    bounds.x + outer + column_x - self.viewport_x,
                    y,
                    column_width,
                    available_height * column.heights[window_index],
                );
                let constraint = constraints
                    .get(window)
                    .copied()
                    .map(normalized_constraints)
                    .unwrap_or_default();
                record_minimum_warnings(*window, rect, constraint, &mut result.warnings);
                apply_maximums(*window, &mut rect, constraint, &mut result.warnings);
                y += available_height * column.heights[window_index] + inner;
                result.geometry.insert(*window, rect);
            }
        }

        Ok(result)
    }

    fn reveal_column(&mut self, positions: &[(f64, f64)], viewport_width: f64, active: usize) {
        if let Some(start) = self.paged_viewport(positions, viewport_width, Some(active)) {
            self.viewport_x = start;
            return;
        }
        let (start, width) = positions[active];
        let end = start + width;
        if self.focus_strategy == ViewportFocusStrategy::Center {
            self.viewport_x = start + width / 2.0 - viewport_width / 2.0;
            return;
        }

        if start >= self.viewport_x - REVEAL_EPSILON && end <= self.viewport_x + viewport_width + REVEAL_EPSILON {
            return;
        }

        let target = if width >= viewport_width {
            // Approaching from the left reveals the left edge; approaching
            // from the right reveals the right edge. Keep an existing view
            // inside an oversized column stable on repeated focus.
            self.viewport_x.clamp(start, end - viewport_width)
        } else if start < self.viewport_x {
            start
        } else if end > self.viewport_x + viewport_width {
            end - viewport_width
        } else {
            self.viewport_x
        };
        if (target - self.viewport_x).abs() > REVEAL_EPSILON {
            self.viewport_x = target;
        }
    }

    fn paged_viewport(&self, positions: &[(f64, f64)], viewport_width: f64, active: Option<usize>) -> Option<f64> {
        if self.focus_strategy != ViewportFocusStrategy::Paged {
            return None;
        }
        let active = active?;
        // Pack actual allocated widths, including gaps and client constraints.
        // Page boundaries are independent of focus direction. An oversized
        // column occupies its own page rather than overlapping its neighbors.
        let mut first = 0;
        while first < positions.len() {
            let (start, _) = positions[first];
            let mut last = first;
            while let Some(&(next_start, next_width)) = positions.get(last + 1) {
                if next_start + next_width - start > viewport_width + 1e-6 {
                    break;
                }
                last += 1;
            }
            if active <= last {
                return Some(start);
            }
            first = last + 1;
        }
        None
    }

    fn retarget_after_width_cycle(&mut self, positions: &[(f64, f64)], viewport_width: f64) {
        if let Some(start) = self.paged_viewport(positions, viewport_width, self.active_column) {
            self.viewport_x = start;
            return;
        }
        let Some(active) = self.active_column else {
            return;
        };
        let Some(&(start, width)) = positions.get(active) else {
            return;
        };
        let Some(&(last_start, last_width)) = positions.last() else {
            return;
        };
        let strip_width = last_start + last_width;

        if strip_width <= viewport_width {
            self.viewport_x = 0.0;
        } else if width >= viewport_width {
            self.viewport_x = start;
        } else {
            self.viewport_x = (start + width - viewport_width).max(0.0);
        }
    }

    fn move_horizontally(&mut self, window: WindowId, destination: usize) -> Result<(), LayoutError> {
        let (source, _) = self.window_location(window).ok_or(LayoutError::UnknownWindow(window))?;
        if self.columns[source].windows.len() == 1 {
            self.columns.swap(source, destination);
            self.active_column = Some(destination);
            self.request_reveal(self.active_window());
            return Ok(());
        }

        let insertion = if destination < source { source } else { source + 1 };
        self.remove(window)?;
        let insertion = insertion.min(self.columns.len());
        self.columns.insert(insertion, Column::new(window, self.default_width));
        self.active_column = Some(insertion);
        self.request_reveal(self.active_window());
        Ok(())
    }

    fn window_location(&self, window: WindowId) -> Option<(usize, usize)> {
        self.columns.iter().enumerate().find_map(|(column, data)| {
            data.windows
                .iter()
                .position(|candidate| *candidate == window)
                .map(|index| (column, index))
        })
    }

    fn validate_scalars(&self) -> Result<(), LayoutError> {
        if !self.viewport_x.is_finite() {
            return Err(LayoutError::InvalidTree("scrolling viewport offset is invalid"));
        }
        if normalized_width(self.default_width) != self.default_width {
            return Err(LayoutError::InvalidTree("scrolling default column width is invalid"));
        }
        if self.columns.is_empty() {
            return if self.active_column.is_none() {
                Ok(())
            } else {
                Err(LayoutError::InvalidTree("empty scrolling layout has an active column"))
            };
        }
        if self.active_column.is_none_or(|active| active >= self.columns.len()) {
            return Err(LayoutError::InvalidTree("scrolling active column is invalid"));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), LayoutError> {
        self.validate_scalars()?;

        let mut windows = HashSet::new();
        for column in &self.columns {
            if normalized_width(column.width) != column.width {
                return Err(LayoutError::InvalidTree("scrolling column width is invalid"));
            }
            if column.windows.is_empty() {
                return Err(LayoutError::InvalidTree("scrolling column is empty"));
            }
            if column.active >= column.windows.len() || column.heights.len() != column.windows.len() {
                return Err(LayoutError::InvalidTree("scrolling column metadata is invalid"));
            }
            if !column.heights.iter().all(|height| height.is_finite() && *height > 0.0) {
                return Err(LayoutError::InvalidTree("scrolling height is invalid"));
            }
            for window in &column.windows {
                if !windows.insert(*window) {
                    return Err(LayoutError::DuplicateWindow(*window));
                }
            }
        }
        Ok(())
    }
}

fn resize_rows(column: &mut Column, row: usize, neighbor: usize, amount: f64) -> bool {
    let transferable = (column.heights[neighbor] - MIN_ROW_HEIGHT).max(0.0);
    let adjustment = amount.min(transferable);
    if adjustment == 0.0 {
        return false;
    }

    column.heights[row] += adjustment;
    column.heights[neighbor] -= adjustment;
    true
}

fn normalized_width(width: ColumnWidth) -> ColumnWidth {
    match width {
        ColumnWidth::Proportion(value) if value.is_finite() && value > 0.0 => ColumnWidth::Proportion(value),
        ColumnWidth::Fixed(value) if value.is_finite() && value > 0.0 => ColumnWidth::Fixed(value),
        ColumnWidth::Full => ColumnWidth::Full,
        _ => ColumnWidth::default(),
    }
}

fn finite_nonnegative(value: f64) -> f64 {
    if value.is_finite() { value.max(0.0) } else { 0.0 }
}

fn apply_maximums(
    window: WindowId,
    rect: &mut Rect,
    constraints: SizeConstraints,
    warnings: &mut Vec<ConstraintWarning>,
) {
    if let Some(maximum) = constraints.max_width
        && rect.width > maximum
    {
        warnings.push(ConstraintWarning {
            window,
            kind: ConstraintKind::MaximumWidth,
            requested: maximum,
            assigned: rect.width,
        });
        rect.width = maximum;
    }
    if let Some(maximum) = constraints.max_height
        && rect.height > maximum
    {
        warnings.push(ConstraintWarning {
            window,
            kind: ConstraintKind::MaximumHeight,
            requested: maximum,
            assigned: rect.height,
        });
        rect.height = maximum;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u64) -> WindowId {
        WindowId(id)
    }

    #[test]
    fn pending_request_preserves_slide_and_width_cycle_precedence() {
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Fixed(150.0));
        let a = window(1);
        let b = window(2);
        layout.insert(a, None).unwrap();
        layout.insert(b, Some(a)).unwrap();
        layout.set_focus_strategy(ViewportFocusStrategy::Center);
        layout
            .resolve_geometry_with_constraints(bounds, GapConfig::default(), &HashMap::new(), Some(b))
            .unwrap();
        layout.slide_focus_from(b, a).unwrap();
        // Ordinary reveal requests must not replace a pending slide.
        layout.focus(b).unwrap();
        assert_eq!(
            layout.pending_focus(),
            Some(PendingFocusRequest::Slide { from: b, to: a })
        );
        layout
            .cycle_column_width(b, &[ColumnWidth::Fixed(150.0), ColumnWidth::Fixed(100.0)])
            .unwrap();
        assert!(matches!(
            layout.pending_viewport,
            Some(PendingViewportRequest::WidthCycle(_))
        ));
        layout.focus_and_reveal(a).unwrap();
        assert_eq!(
            layout.pending_viewport,
            Some(PendingViewportRequest::WidthCycle(Some(PendingFocusRequest::Reveal(a))))
        );
        layout
            .resolve_geometry_with_constraints(bounds, GapConfig::default(), &HashMap::new(), Some(a))
            .unwrap();
        assert!(matches!(
            layout.viewport_target().unwrap().request,
            ViewportRequest::WidthCycle(_)
        ));
        assert!(layout.pending_viewport.is_none());
    }

    #[test]
    fn canceled_width_cycle_restores_focus_request_and_solver_consumes_it_once() {
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Fixed(150.0));
        let a = window(1);
        let b = window(2);
        layout.insert(a, None).unwrap();
        layout.insert(b, Some(a)).unwrap();
        layout.set_focus_strategy(ViewportFocusStrategy::Center);
        layout
            .resolve_geometry_with_constraints(bounds, GapConfig::default(), &HashMap::new(), Some(b))
            .unwrap();
        layout.slide_focus_from(b, a).unwrap();
        assert!(layout.cycle_column_width(a, &[ColumnWidth::Fixed(100.0)]).unwrap());
        assert!(!layout.cycle_column_width(a, &[ColumnWidth::Fixed(100.0)]).unwrap());
        assert_eq!(
            layout.pending_viewport,
            Some(PendingViewportRequest::Focus(PendingFocusRequest::Slide {
                from: b,
                to: a
            }))
        );
        layout
            .resolve_geometry_with_constraints(bounds, GapConfig::default(), &HashMap::new(), Some(a))
            .unwrap();
        assert!(matches!(
            layout.viewport_target().unwrap().request,
            ViewportRequest::Slide(_, _)
        ));
        let viewport = layout.viewport_x();
        let provenance = layout.viewport_target().cloned();
        layout
            .resolve_geometry_with_constraints(bounds, GapConfig::default(), &HashMap::new(), Some(a))
            .unwrap();
        assert_eq!(layout.viewport_x(), viewport);
        assert_eq!(layout.viewport_target(), provenance.as_ref());
        assert!(layout.pending_viewport.is_none());
    }

    #[test]
    fn explicit_center_replays_maximum_even_when_only_held_width_crosses_it() {
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };
        let id = WindowId(1);
        let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Fixed(100.0));
        layout.insert(id, None).unwrap();
        let constraints = HashMap::from([(
            id,
            SizeConstraints {
                max_width: Some(200.0),
                ..Default::default()
            },
        )]);
        layout.center_window(id, bounds, gaps, &constraints).unwrap();
        assert_eq!(layout.viewport_x(), -50.0);
        assert_eq!(
            layout
                .viewport_target()
                .unwrap()
                .with_held_widths(&[(id, 300.0, 100.0)], None),
            0.0
        );
        let constraints = HashMap::from([(
            id,
            SizeConstraints {
                max_width: Some(40.0),
                ..Default::default()
            },
        )]);
        layout.set_column_width(id, ColumnWidth::Fixed(150.0)).unwrap();
        layout.center_window(id, bounds, gaps, &constraints).unwrap();
        assert!(!layout.viewport_depends_on_width(id, 100.0, 150.0));
    }

    #[test]
    fn viewport_dependencies_follow_allocated_widths_and_target_derivation() {
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };
        let a = WindowId(1);
        let b = WindowId(2);
        let c = WindowId(3);
        for strategy in [
            ViewportFocusStrategy::Minimal,
            ViewportFocusStrategy::Center,
            ViewportFocusStrategy::Paged,
        ] {
            let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Fixed(100.0));
            layout.set_focus_strategy(strategy);
            layout.insert(a, None).unwrap();
            layout.insert(b, Some(a)).unwrap();
            layout.insert(c, Some(b)).unwrap();
            layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(c))
                .unwrap();
            layout.set_column_width(b, ColumnWidth::Fixed(150.0)).unwrap();
            layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(c))
                .unwrap();
            assert!(layout.viewport_depends_on_width(b, 100.0, 150.0), "{strategy:?}");
            assert!(
                !layout.viewport_depends_on_width(b, 150.0, 150.0),
                "height-only size change"
            );
            // An unrelated relayout does not erase the last calculation.
            layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(c))
                .unwrap();
            assert!(layout.viewport_depends_on_width(b, 100.0, 150.0));
            layout.focus_and_reveal(a).unwrap();
            layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(a))
                .unwrap();
            assert!(
                !layout.viewport_depends_on_width(b, 100.0, 150.0),
                "independent reversal {strategy:?}"
            );
            layout.center_window(b, bounds, gaps, &HashMap::new()).unwrap();
            assert!(
                layout.viewport_depends_on_width(b, 100.0, 150.0),
                "explicit centering {strategy:?}"
            );
            assert!(
                !layout.viewport_depends_on_width(b, 150.0, 100.0),
                "obsolete width generation"
            );
        }
        let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Fixed(100.0));
        layout.insert(a, None).unwrap();
        layout.insert(b, Some(a)).unwrap();
        layout.insert(c, Some(b)).unwrap();
        let constraints = HashMap::from([(
            b,
            SizeConstraints {
                min_width: 150.0,
                max_width: Some(40.0),
                ..Default::default()
            },
        )]);
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &constraints, Some(c))
            .unwrap();
        assert_eq!(layout.allocated_column_width(b), Some(150.0));
        assert_eq!(result.geometry[&b].width, 150.0, "minimum wins inconsistent maximum");
        assert!(layout.viewport_depends_on_width(b, 100.0, 150.0));
    }

    #[test]
    fn inserts_new_columns_after_focus() {
        let mut layout = ScrollingLayout::default();
        layout.insert(window(1), None).unwrap();
        layout.insert(window(2), Some(window(1))).unwrap();
        layout.insert(window(3), Some(window(1))).unwrap();

        assert_eq!(
            layout.window_ids().collect::<Vec<_>>(),
            vec![window(1), window(3), window(2)]
        );
        assert_eq!(layout.active_column(), Some(1));
    }

    #[test]
    fn configured_default_width_is_applied_to_new_columns() {
        let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Full);
        layout.insert(window(1), None).unwrap();
        layout.insert(window(2), Some(window(1))).unwrap();

        assert_eq!(layout.default_width(), ColumnWidth::Full);
        assert!(layout.columns().iter().all(|column| column.width == ColumnWidth::Full));
    }

    #[test]
    fn hover_does_not_replace_a_pending_explicit_reveal() {
        for strategy in [
            ViewportFocusStrategy::Minimal,
            ViewportFocusStrategy::Center,
            ViewportFocusStrategy::Paged,
        ] {
            let mut layout = ScrollingLayout::default();
            layout.set_focus_strategy(strategy);
            for id in 1..=3 {
                layout.insert(window(id), None).unwrap();
            }
            let bounds = Rect::new(0.0, 0.0, 1000.0, 800.0);
            let gaps = GapConfig {
                inner: 0.0,
                outer: 0.0,
                smart: false,
            };
            for target in [3, 1] {
                layout.focus_and_reveal(window(target)).unwrap();
                let mut expected = layout.clone();
                expected
                    .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(target)))
                    .unwrap();
                layout.focus_without_reveal(window(2)).unwrap();
                layout
                    .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
                    .unwrap();
                assert_eq!(layout.active_window(), Some(window(2)));
                assert_eq!(layout.viewport_x(), expected.viewport_x(), "{strategy:?}");
            }
            layout.focus_and_reveal(window(3)).unwrap();
            layout.focus_without_reveal(window(2)).unwrap();
            layout.remove(window(3)).unwrap();
            assert!(
                layout
                    .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
                    .is_ok()
            );
        }
    }

    #[test]
    fn focus_reveal_scrolls_only_as_far_as_needed() {
        let mut layout = ScrollingLayout::default();
        for id in 1..=4 {
            layout.insert(window(id), Some(window(id.saturating_sub(1)))).unwrap();
        }
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };

        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(4)))
            .unwrap();

        assert_eq!(layout.viewport_x(), 1_000.0);
        assert_eq!(result.geometry[&window(4)].x, 500.0);
    }

    #[test]
    fn minimal_reveal_uses_the_nearest_edge_and_keeps_visible_columns_still() {
        for (viewport, start, width, expected) in [
            (500.0, 500.0, 500.0, 500.0),
            (500.0, 1000.0, 500.0, 500.0),
            (500.0, 400.0, 500.0, 400.0),
            (500.0, 1100.0, 500.0, 600.0),
            (500.0, 400.0, 1000.0, 400.0),
            (500.0, 1100.0, 1000.0, 1100.0),
            (500.0, 400.0, 1500.0, 500.0),
            (500.0, -1000.0, 1500.0, -500.0),
            (500.0, 1100.0, 1500.0, 1100.0),
        ] {
            let mut layout = ScrollingLayout {
                viewport_x: viewport,
                ..ScrollingLayout::default()
            };
            layout.reveal_column(&[(start, width)], 1000.0, 0);
            assert_eq!(
                layout.viewport_x(),
                expected,
                "viewport={viewport}, start={start}, width={width}"
            );
            layout.reveal_column(&[(start, width)], 1000.0, 0);
            assert_eq!(layout.viewport_x(), expected, "repeated reveal must stay still");
        }
    }

    #[test]
    fn minimal_reveal_ignores_subpixel_edge_drift() {
        for (start, width, expected) in [
            (499.5, 500.0, 500.0),
            (1000.5, 500.0, 500.0),
            (499.4, 500.0, 499.4),
            (1000.6, 500.0, 500.6),
            (499.5, 1000.0, 500.0),
            (500.5, 1000.0, 500.0),
            (500.5, 1500.0, 500.0),
            (-0.5, 1500.0, 500.0),
        ] {
            let mut layout = ScrollingLayout {
                viewport_x: 500.0,
                ..ScrollingLayout::default()
            };
            layout.reveal_column(&[(start, width)], 1000.0, 0);
            assert!(
                (layout.viewport_x() - expected).abs() < 1e-9,
                "start={start}, width={width}"
            );
        }
    }

    #[test]
    fn minimal_reveal_enters_oversized_columns_from_either_side() {
        let mut layout = ScrollingLayout::default();
        for id in 1..=3 {
            layout.insert(window(id), Some(window(id.saturating_sub(1)))).unwrap();
        }
        layout.columns[1].width = ColumnWidth::Fixed(1500.0);
        let bounds = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };
        for (focused, expected) in [(1, 0.0), (2, 500.0), (3, 1500.0), (2, 1000.0), (2, 1000.0)] {
            layout.focus_and_reveal(window(focused)).unwrap();
            layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(focused)))
                .unwrap();
            assert_eq!(layout.viewport_x(), expected);
        }
    }

    #[test]
    fn minimal_reveal_aligns_full_width_columns_without_neighbor_context() {
        let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Full);
        layout.insert(window(1), None).unwrap();
        layout.insert(window(2), Some(window(1))).unwrap();
        let bounds = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let gaps = GapConfig {
            inner: 10.0,
            outer: 0.0,
            smart: false,
        };
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();
        assert_eq!(layout.viewport_x(), 1010.0);
        assert_eq!(result.geometry[&window(2)].x, 0.0);
        let previous = result.geometry[&window(1)];
        assert!(previous.x + previous.width <= 0.0);
        layout.focus_and_reveal(window(1)).unwrap();
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap();
        assert_eq!(layout.viewport_x(), 0.0);
        assert_eq!(result.geometry[&window(1)].x, 0.0);
        assert!(result.geometry[&window(2)].x >= bounds.width);
    }

    #[test]
    fn minimal_reveal_accounts_for_fractional_gaps_and_outer_margins() {
        let bounds = Rect::new(100.25, 30.5, 1000.5, 800.0);
        let gaps = GapConfig {
            inner: 9.5,
            outer: 13.25,
            smart: false,
        };
        for (width, offsets) in [
            (ColumnWidth::Fixed(400.0), [0.0, 245.0, 245.0, 0.0]),
            (ColumnWidth::Full, [0.0, 1967.0, 983.5, 0.0]),
        ] {
            let mut layout = ScrollingLayout::with_default_width(width);
            for id in 1..=3 {
                layout.insert(window(id), Some(window(id.saturating_sub(1)))).unwrap();
            }
            for (focused, offset) in [1, 3, 2, 1].into_iter().zip(offsets) {
                layout.focus_and_reveal(window(focused)).unwrap();
                let result = layout
                    .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(focused)))
                    .unwrap();
                let rect = result.geometry[&window(focused)];
                assert_eq!(layout.viewport_x(), offset, "width={width:?}, focused={focused}");
                if focused == 1 || width == ColumnWidth::Full {
                    assert_eq!(rect.x, 113.5, "left edge includes the outer margin");
                }
                if focused == 3 || width == ColumnWidth::Full {
                    assert_eq!(rect.x + rect.width, 1087.5, "right edge includes the outer margin");
                }
                let next = result.geometry[&window(2)];
                let first = result.geometry[&window(1)];
                assert_eq!(next.x - (first.x + first.width), 9.5, "inner gap stays intact");
            }
        }
    }

    #[test]
    fn two_half_width_columns_fit_without_focus_scrolling() {
        let mut layout = ScrollingLayout::default();
        layout.insert(window(1), None).unwrap();
        layout.insert(window(2), Some(window(1))).unwrap();
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let gaps = GapConfig {
            inner: 10.0,
            outer: 10.0,
            smart: false,
        };

        let first = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap();
        assert_eq!(layout.viewport_x(), 0.0);
        let second = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();

        assert_eq!(layout.viewport_x(), 0.0);
        assert_eq!(first.geometry, second.geometry);
        assert_eq!(second.geometry[&window(1)], Rect::new(10.0, 10.0, 485.0, 780.0));
        assert_eq!(second.geometry[&window(2)], Rect::new(505.0, 10.0, 485.0, 780.0));
    }

    #[test]
    fn full_width_column_stays_in_strip_when_new_half_window_opens() {
        let (mut layout, bounds, gaps) = two_half_width_layout();
        layout.set_focus_strategy(ViewportFocusStrategy::Paged);
        layout
            .set_column_width(window(2), ColumnWidth::Proportion(1.0))
            .unwrap();
        let zoomed = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();
        assert_eq!(zoomed.geometry[&window(2)].width, 980.0);
        layout.insert(window(3), Some(window(2))).unwrap();
        let opened = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(3)))
            .unwrap();
        let previous = opened.geometry[&window(2)];
        let new = opened.geometry[&window(3)];
        assert_eq!(previous.width, 980.0);
        assert_eq!(new, Rect::new(10.0, 10.0, 485.0, 780.0));
        assert_eq!(previous.x + previous.width + gaps.inner, new.x);
        assert_eq!(layout.active_window(), Some(window(3)));
        layout
            .set_column_width(window(2), ColumnWidth::Proportion(0.5))
            .unwrap();
        let restored = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();
        assert_eq!(restored.geometry[&window(2)].width, 485.0);
    }

    fn paged_layout(count: u64) -> (ScrollingLayout, Rect, GapConfig) {
        let (mut layout, bounds, gaps) = two_half_width_layout();
        layout.set_focus_strategy(ViewportFocusStrategy::Paged);
        for id in 3..=count {
            layout.insert(window(id), Some(window(id - 1))).unwrap();
        }
        (layout, bounds, gaps)
    }

    #[test]
    fn paged_focus_snaps_six_half_columns_in_both_directions() {
        let (mut layout, bounds, gaps) = paged_layout(6);
        for focused in [1, 2, 3, 4, 5, 6, 5, 4, 3, 2, 1] {
            let geometry = layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(focused)))
                .unwrap()
                .geometry;
            let first = (focused - 1) / 2 * 2 + 1;
            assert_eq!(layout.viewport_x(), ((first - 1) / 2) as f64 * 990.0);
            assert_eq!(geometry[&window(first)].x, 10.0);
            assert_eq!(geometry[&window(first + 1)].x, 505.0);
            assert_eq!(
                geometry[&window(first + 1)].x + geometry[&window(first + 1)].width,
                990.0
            );
        }
    }

    #[test]
    fn minimal_swipe_focus_reveals_only_hidden_edges() {
        let mut layout = ScrollingLayout::default();
        for id in 1..=3 {
            layout.insert(window(id), Some(window(id.saturating_sub(1)))).unwrap();
        }
        let bounds = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };
        layout.focus_and_reveal(window(1)).unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap();
        for (previous, focused, expected) in [(1, 2, 0.0), (2, 3, 500.0), (3, 2, 500.0), (2, 1, 0.0)] {
            layout.slide_focus_from(window(previous), window(focused)).unwrap();
            layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(focused)))
                .unwrap();
            assert_eq!(layout.viewport_x(), expected);
        }
    }

    #[test]
    fn swipe_focus_slides_one_column_at_a_time_in_paged_layout() {
        let (mut layout, bounds, gaps) = paged_layout(5);
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap();
        assert_eq!(layout.viewport_x(), 0.0);

        for (previous, focused, expected) in [(1, 2, 495.0), (2, 3, 990.0), (3, 2, 495.0), (2, 1, 0.0)] {
            layout.focus_and_reveal(window(focused)).unwrap();
            layout.slide_focus_from(window(previous), window(focused)).unwrap();
            let result = layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(focused)))
                .unwrap();

            assert_eq!(layout.viewport_x(), expected);
            assert_eq!(result.geometry[&window(focused)].x, 10.0);
        }
    }

    #[test]
    fn paged_focus_realigns_after_output_resize() {
        let (mut layout, bounds, gaps) = paged_layout(6);
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(5)))
            .unwrap();
        let bounds = Rect::new(20.0, 48.0, 1400.0, 800.0);
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(5)))
            .unwrap();
        assert_eq!(layout.viewport_x(), 2780.0);
        assert_eq!(result.geometry[&window(5)].x, 30.0);
        assert_eq!(result.geometry[&window(6)].x, 725.0);
    }

    #[test]
    fn paged_focus_preserves_explicit_center_until_focus_changes() {
        let (mut layout, bounds, gaps) = paged_layout(4);
        layout.center_window(window(2), bounds, gaps, &HashMap::new()).unwrap();
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();
        let rect = result.geometry[&window(2)];
        assert_eq!(rect.x + rect.width / 2.0, 500.0);
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap();
        assert_eq!(result.geometry[&window(1)].x, 10.0);
    }

    #[test]
    fn full_width_column_occupies_its_own_page() {
        let (mut layout, bounds, gaps) = paged_layout(4);
        layout.columns[0].width = ColumnWidth::Full;
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(3)))
            .unwrap();
        assert_eq!(result.geometry[&window(2)].x, 10.0);
        assert_eq!(result.geometry[&window(3)].x, 505.0);
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(4)))
            .unwrap();
        assert_eq!(result.geometry[&window(4)].x, 10.0);
    }

    #[test]
    fn client_minimums_split_columns_across_pages() {
        let (mut layout, bounds, gaps) = paged_layout(2);
        let constraints = HashMap::from([(
            window(1),
            SizeConstraints {
                min_width: 700.0,
                ..Default::default()
            },
        )]);
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &constraints, Some(window(2)))
            .unwrap();
        assert!(layout.viewport_x() > 0.0);
        let rect = result.geometry[&window(2)];
        assert!(rect.x >= 10.0 && rect.x + rect.width <= 990.0);
    }

    #[test]
    fn paged_focus_groups_thirds_in_both_directions_at_fractional_sizes() {
        let (mut layout, _, gaps) = paged_layout(6);
        for column in &mut layout.columns {
            column.width = ColumnWidth::Proportion(1.0 / 3.0);
        }
        let bounds = Rect::new(13.25, 48.5, 1000.5, 800.25);
        for focused in [1, 2, 3, 4, 5, 6, 5, 4, 3, 2, 1] {
            let result = layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(focused)))
                .unwrap();
            let first = (focused - 1) / 3 * 3 + 1;
            assert!((result.geometry[&window(first)].x - 23.25).abs() < 1e-6);
            let last = result.geometry[&window(first + 2)];
            assert!((last.x + last.width - 1003.75).abs() < 1e-6);
        }
    }

    #[test]
    fn paged_focus_packs_mixed_widths_by_actual_fit() {
        let (mut layout, bounds, gaps) = paged_layout(5);
        let widths = [0.6, 0.4, 0.25, 0.5, 0.25];
        for (column, width) in layout.columns.iter_mut().zip(widths) {
            column.width = ColumnWidth::Proportion(width);
        }
        for (focused, first) in [(1, 1), (2, 1), (3, 3), (4, 3), (5, 3), (2, 1)] {
            let result = layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(focused)))
                .unwrap();
            assert_eq!(result.geometry[&window(first)].x, 10.0);
            let rect = result.geometry[&window(focused)];
            assert!(rect.x >= 10.0 && rect.x + rect.width <= 990.0 + 1e-6);
        }
    }

    #[test]
    fn oversized_page_does_not_absorb_neighbor_and_removal_repacks() {
        let (mut layout, bounds, gaps) = paged_layout(4);
        let constraints = HashMap::from([(
            window(1),
            SizeConstraints {
                min_width: 1200.0,
                ..Default::default()
            },
        )]);
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &constraints, Some(window(2)))
            .unwrap();
        assert_eq!(result.geometry[&window(2)].x, 10.0);
        assert_eq!(result.geometry[&window(3)].x, 505.0);
        layout.remove(window(1)).unwrap();
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();
        assert_eq!(layout.viewport_x(), 0.0);
        assert_eq!(result.geometry[&window(2)].x, 10.0);
        assert_eq!(result.geometry[&window(3)].x, 505.0);
    }

    #[test]
    fn manual_resize_does_not_retarget_the_viewport() {
        let mut layout = ScrollingLayout::default();
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };

        for id in 1..=3 {
            layout.insert(window(id), (id > 1).then(|| window(id - 1))).unwrap();
            layout
                .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(id)))
                .unwrap();
        }
        let before = layout.viewport_x();

        layout.resize_window(window(3), Direction::Right, 0.5).unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(3)))
            .unwrap();

        assert_eq!(layout.viewport_x(), before);
    }

    #[test]
    fn minimal_reveal_of_the_first_column_aligns_to_the_strip_start() {
        let mut layout = ScrollingLayout::default();
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };

        for id in 1..=3 {
            layout.insert(window(id), None).unwrap();
        }
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(3)))
            .unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap();

        assert_eq!(layout.viewport_x(), 0.0);
    }

    #[test]
    fn closing_non_focused_column_does_not_move_viewport() {
        let mut layout = ScrollingLayout::default();
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };

        for id in 1..=4 {
            layout.insert(window(id), None).unwrap();
        }
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(4)))
            .unwrap();
        let before = layout.viewport_x();

        layout.remove(window(1)).unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(4)))
            .unwrap();

        assert_eq!(layout.viewport_x(), before);
    }

    #[test]
    fn grouping_and_extracting_preserve_membership() {
        let mut layout = ScrollingLayout::default();
        layout.insert(window(1), None).unwrap();
        layout.insert(window(2), Some(window(1))).unwrap();
        layout.move_into_column(window(2), window(1)).unwrap();

        assert_eq!(layout.columns().len(), 1);
        assert_eq!(layout.columns()[0].windows, vec![window(1), window(2)]);

        layout.extract_to_column(window(2)).unwrap();
        assert_eq!(layout.columns().len(), 2);
        assert_eq!(
            layout.window_ids().collect::<HashSet<_>>(),
            HashSet::from([window(1), window(2)])
        );
    }

    #[test]
    fn later_column_changes_do_not_move_the_first_column() {
        let mut layout = ScrollingLayout::default();
        layout.insert(window(1), None).unwrap();
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let gaps = GapConfig {
            smart: false,
            ..GapConfig::default()
        };
        let first = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap()
            .geometry[&window(1)];

        layout.insert(window(2), Some(window(1))).unwrap();
        let second = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap()
            .geometry[&window(1)];

        assert_eq!(first.x, second.x);
        assert_eq!(first.width, second.width);
    }

    #[test]
    fn directional_navigation_follows_columns_and_rows() {
        let mut layout = ScrollingLayout::default();
        layout.insert(window(1), None).unwrap();
        layout.insert(window(2), Some(window(1))).unwrap();
        layout.insert(window(3), Some(window(2))).unwrap();
        layout.move_into_column(window(3), window(2)).unwrap();

        assert_eq!(
            layout.directional_neighbor(window(1), Direction::Right),
            Ok(Some(window(3)))
        );
        assert_eq!(
            layout.directional_neighbor(window(3), Direction::Up),
            Ok(Some(window(2)))
        );
        assert_eq!(
            layout.directional_neighbor(window(2), Direction::Down),
            Ok(Some(window(3)))
        );
    }

    #[test]
    fn vertical_moves_reorder_rows_and_horizontal_moves_extract_them() {
        let mut layout = ScrollingLayout::default();
        layout.insert(window(1), None).unwrap();
        layout.insert(window(2), Some(window(1))).unwrap();
        layout.insert(window(3), Some(window(2))).unwrap();
        layout.move_into_column(window(3), window(2)).unwrap();

        assert!(layout.move_window(window(3), Direction::Up).unwrap());
        assert_eq!(layout.columns()[1].windows, vec![window(3), window(2)]);
        assert!(layout.move_window(window(3), Direction::Left).unwrap());
        assert_eq!(layout.columns().len(), 3);
        assert_eq!(
            layout.window_ids().collect::<Vec<_>>(),
            vec![window(1), window(3), window(2)]
        );
    }

    #[test]
    fn resize_changes_column_width_and_neighboring_row_weights() {
        let mut layout = ScrollingLayout::default();
        layout.insert(window(1), None).unwrap();
        layout.insert(window(2), Some(window(1))).unwrap();
        layout.move_into_column(window(2), window(1)).unwrap();

        assert!(layout.resize_window(window(1), Direction::Right, 0.1).unwrap());
        assert_eq!(layout.columns()[0].width, ColumnWidth::Proportion(0.6));

        assert!(layout.resize_window(window(1), Direction::Down, 0.1).unwrap());
        assert_eq!(layout.columns()[0].heights, vec![0.6, 0.4]);
    }

    #[test]
    fn width_cycle_wraps_through_configured_presets() {
        let mut layout = ScrollingLayout::default();
        layout.insert(window(1), None).unwrap();
        let presets = [
            ColumnWidth::Proportion(0.5),
            ColumnWidth::Proportion(2.0 / 3.0),
            ColumnWidth::Full,
        ];

        assert!(layout.cycle_column_width(window(1), &presets).unwrap());
        assert_eq!(layout.columns()[0].width, ColumnWidth::Proportion(2.0 / 3.0));
        assert!(layout.cycle_column_width(window(1), &presets).unwrap());
        assert_eq!(layout.columns()[0].width, ColumnWidth::Full);
        assert!(layout.cycle_column_width(window(1), &presets).unwrap());
        assert_eq!(layout.columns()[0].width, ColumnWidth::Proportion(0.5));
    }

    #[test]
    fn left_column_zoom_pushes_and_restores_the_right_column() {
        let (mut layout, bounds, gaps) = two_half_width_layout();
        let presets = [ColumnWidth::Proportion(0.5), ColumnWidth::Full];

        layout.focus(window(1)).unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap();
        layout.cycle_column_width(window(1), &presets).unwrap();
        let zoomed = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap();

        assert_eq!(layout.viewport_x(), 0.0);
        assert_eq!(zoomed.geometry[&window(1)].x, 10.0);
        assert_eq!(zoomed.geometry[&window(1)].width, 980.0);
        assert_eq!(zoomed.geometry[&window(2)].x, 1_000.0);

        layout.cycle_column_width(window(1), &presets).unwrap();
        let restored = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(1)))
            .unwrap();

        assert_eq!(layout.viewport_x(), 0.0);
        assert_eq!(restored.geometry[&window(1)].x, 10.0);
        assert_eq!(restored.geometry[&window(2)].x, 505.0);
    }

    #[test]
    fn right_column_zoom_moves_left_neighbor_out_and_back() {
        let (mut layout, bounds, gaps) = two_half_width_layout();
        let presets = [ColumnWidth::Proportion(0.5), ColumnWidth::Full];

        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();
        layout.cycle_column_width(window(2), &presets).unwrap();
        let zoomed = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();

        assert_eq!(layout.viewport_x(), 495.0);
        assert_eq!(zoomed.geometry[&window(1)].x, -485.0);
        assert_eq!(zoomed.geometry[&window(2)].x, 10.0);
        assert_eq!(zoomed.geometry[&window(2)].width, 980.0);

        layout.cycle_column_width(window(2), &presets).unwrap();
        let restored = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();

        assert_eq!(layout.viewport_x(), 0.0);
        assert_eq!(restored.geometry[&window(1)].x, 10.0);
        assert_eq!(restored.geometry[&window(2)].x, 505.0);
    }

    fn two_half_width_layout() -> (ScrollingLayout, Rect, GapConfig) {
        let mut layout = ScrollingLayout::default();
        layout.insert(window(1), None).unwrap();
        layout.insert(window(2), Some(window(1))).unwrap();

        (
            layout,
            Rect::new(0.0, 0.0, 1_000.0, 800.0),
            GapConfig {
                inner: 10.0,
                outer: 10.0,
                smart: false,
            },
        )
    }

    #[test]
    fn explicit_center_places_a_middle_column_in_the_viewport_center() {
        let mut layout = ScrollingLayout::default();
        for id in 1_u64..=4 {
            layout
                .insert(window(id), id.checked_sub(1).filter(|id| *id > 0).map(window))
                .unwrap();
        }
        let bounds = Rect::new(0.0, 0.0, 1_000.0, 800.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(4)))
            .unwrap();

        assert!(layout.center_window(window(2), bounds, gaps, &HashMap::new()).unwrap());
        let result = layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(window(2)))
            .unwrap();
        let rect = result.geometry[&window(2)];

        assert_eq!(rect.x + rect.width / 2.0, 500.0);
    }

    #[test]
    fn randomized_scrolling_operations_preserve_invariants() {
        let mut layout = ScrollingLayout::default();
        let mut windows = Vec::new();
        let mut next_window = 1_u64;
        let mut random = 0x5c40_11ab_u64;

        for _ in 0..2_000 {
            random = random
                .wrapping_mul(2_862_933_555_777_941_757)
                .wrapping_add(3_037_000_493);

            match random % 7 {
                0 if windows.len() < 48 => {
                    let new_window = window(next_window);
                    next_window += 1;
                    let focused = windows.get(random as usize % windows.len().max(1)).copied();
                    layout.insert(new_window, focused).unwrap();
                    windows.push(new_window);
                }
                1 if !windows.is_empty() => {
                    let index = random as usize % windows.len();
                    layout.remove(windows.swap_remove(index)).unwrap();
                }
                2 if windows.len() > 1 => {
                    let first = random as usize % windows.len();
                    let mut second = random.rotate_left(17) as usize % windows.len();
                    if first == second {
                        second = (second + 1) % windows.len();
                    }
                    layout.move_into_column(windows[first], windows[second]).unwrap();
                }
                3 if !windows.is_empty() => {
                    let index = random as usize % windows.len();
                    layout.extract_to_column(windows[index]).unwrap();
                }
                4 if !windows.is_empty() => {
                    let index = random as usize % windows.len();
                    let direction = random_direction(random.rotate_left(9));
                    layout.move_window(windows[index], direction).unwrap();
                }
                5 if !windows.is_empty() => {
                    let index = random as usize % windows.len();
                    let direction = random_direction(random.rotate_left(23));
                    layout.resize_window(windows[index], direction, 0.05).unwrap();
                }
                6 if !windows.is_empty() => {
                    let index = random as usize % windows.len();
                    layout.focus(windows[index]).unwrap();
                }
                _ => {}
            }

            layout.validate().unwrap();
            assert_eq!(layout.window_ids().collect::<HashSet<_>>().len(), windows.len());
        }
    }

    fn random_direction(random: u64) -> Direction {
        match random % 4 {
            0 => Direction::Left,
            1 => Direction::Right,
            2 => Direction::Up,
            _ => Direction::Down,
        }
    }
}
