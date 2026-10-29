//! Resolved strip geometry for direct panning and release snapping.
use super::{ScrollingLayout, ViewportFocusStrategy, ViewportRequest};
use crate::WindowId;

#[derive(Clone, Debug, PartialEq)]
struct ScrollColumn {
    window: WindowId,
    position: f64,
    width: f64,
    page: usize,
}

struct ScrollTarget {
    column: usize,
    position: f64,
    request: ViewportRequest,
}

/// A gesture retains this snapshot so layout changes cannot redirect its release.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrollGeometry {
    viewport_width: f64,
    gap: f64,
    strategy: ViewportFocusStrategy,
    columns: Vec<ScrollColumn>,
}

impl ScrollGeometry {
    #[must_use]
    pub fn viewport_width(&self) -> f64 {
        self.viewport_width
    }

    #[must_use]
    pub fn limits(&self) -> (f64, f64) {
        let first = &self.columns[0];
        let last = self.columns.last().expect("a resolved strip has columns");
        match self.strategy {
            ViewportFocusStrategy::Center => (
                (first.width / 2.0 - self.viewport_width / 2.0).min(0.0),
                (last.position + last.width / 2.0 - self.viewport_width / 2.0)
                    .max(last.position + last.width - self.viewport_width),
            ),
            ViewportFocusStrategy::Minimal => (
                0.0,
                (last.position + last.width - self.viewport_width).max(0.0),
            ),
            ViewportFocusStrategy::Paged => (
                0.0,
                self.columns[last.page].position + (last.width - self.viewport_width).max(0.0),
            ),
        }
    }

    /// Compare only horizontal geometry and the active identities of the columns.
    #[must_use]
    pub fn matches(&self, layout: &ScrollingLayout) -> bool {
        layout.last_sizing.is_some_and(|sizing| {
            sizing.viewport_width == self.viewport_width
                && sizing.inner_gap == self.gap
                && layout.focus_strategy == self.strategy
                && layout.columns.len() == self.columns.len()
                && layout
                    .columns
                    .iter()
                    .zip(&self.columns)
                    .all(|(live, saved)| {
                        live.windows.get(live.active) == Some(&saved.window)
                            && layout
                                .allocated_widths
                                .get(&saved.window)
                                .copied()
                                .unwrap_or_else(|| sizing.resolve(live.width))
                                == saved.width
                    })
        })
    }

    #[must_use]
    pub fn snap_window(&self, projected: f64, preferred: Option<WindowId>) -> Option<WindowId> {
        self.target(projected, preferred)
            .map(|target| self.columns[target.column].window)
    }

    fn target(&self, projected: f64, preferred: Option<WindowId>) -> Option<ScrollTarget> {
        if !projected.is_finite() {
            return None;
        }
        let limits = self.limits();
        let score = |target: &ScrollTarget| {
            let column = &self.columns[target.column];
            (
                (target.position - projected).abs(),
                (column.position + column.width / 2.0 - projected - self.viewport_width / 2.0)
                    .abs(),
                Some(column.window) != preferred,
            )
        };
        let mut best: Option<ScrollTarget> = None;
        let index_iter = self.columns.iter().enumerate();
        for (index, column) in index_iter {
            for right in [false, true] {
                let request = match self.strategy {
                    ViewportFocusStrategy::Center => ViewportRequest::Center(index, None),
                    ViewportFocusStrategy::Paged if column.width <= self.viewport_width => {
                        ViewportRequest::Scroll {
                            column: column.page,
                            right: false,
                        }
                    }
                    _ => ViewportRequest::Scroll {
                        column: index,
                        right,
                    },
                };
                let position = match request {
                    ViewportRequest::Center(..) => {
                        column.position + column.width / 2.0 - self.viewport_width / 2.0
                    }
                    ViewportRequest::Scroll { column, right } => {
                        let column = &self.columns[column];
                        column.position
                            + if right {
                                column.width - self.viewport_width
                            } else {
                                0.0
                            }
                    }
                    _ => unreachable!(),
                }
                .clamp(limits.0, limits.1);
                let candidate = ScrollTarget {
                    column: index,
                    position,
                    request,
                };
                let next = score(&candidate);
                let better = best.as_ref().is_none_or(|old| {
                    let old = score(old);
                    next.0
                        .total_cmp(&old.0)
                        .then_with(|| next.1.total_cmp(&old.1))
                        .then_with(|| next.2.cmp(&old.2))
                        .is_lt()
                });
                if better {
                    best = Some(candidate);
                }
            }
        }
        best
    }
}

impl ScrollingLayout {
    #[must_use]
    pub fn scroll_geometry(&self) -> Option<ScrollGeometry> {
        let sizing = self.last_sizing?;
        if self.columns.is_empty() {
            return None;
        }
        let mut position = 0.0;
        let mut page = 0;
        let mut page_start = 0.0;
        let mut columns = Vec::with_capacity(self.columns.len());
        let index_iter = self.columns.iter().enumerate();
        for (index, column) in index_iter {
            let window = *column.windows.get(column.active)?;
            let width = self
                .allocated_widths
                .get(&window)
                .copied()
                .unwrap_or_else(|| sizing.resolve(column.width));
            if position + width - page_start > sizing.viewport_width + 1e-6 {
                page = index;
                page_start = position;
            }
            columns.push(ScrollColumn {
                window,
                position,
                width,
                page,
            });
            position += width + sizing.inner_gap;
        }
        Some(ScrollGeometry {
            viewport_width: sizing.viewport_width,
            gap: sizing.inner_gap,
            strategy: self.focus_strategy,
            columns,
        })
    }

    /// Snap a gesture release while retaining width provenance for client waits.
    pub fn finish_scroll(&mut self, projected: f64) -> Option<WindowId> {
        let geometry = self.scroll_geometry()?;
        let target = geometry.target(projected, self.active_window())?;
        let window = geometry.columns[target.column].window;
        self.focus(window).ok()?;
        self.viewport_x = target.position;
        self.pending_viewport = None;
        let positions = geometry
            .columns
            .iter()
            .map(|column| (column.position, column.width))
            .collect::<Vec<_>>();
        self.record_viewport_basis(
            &positions,
            geometry.gap,
            geometry.viewport_width,
            target.request,
        );
        Some(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColumnWidth, GapConfig, Rect, SizeConstraints};
    use std::collections::HashMap;

    fn layout(widths: &[f64], strategy: ViewportFocusStrategy) -> ScrollingLayout {
        let mut layout = ScrollingLayout::default();
        let width_iter = widths.iter().enumerate();
        for (index, width) in width_iter {
            let id = WindowId(index as u64 + 1);
            layout.insert(id, None).unwrap();
            layout
                .set_column_width(id, ColumnWidth::Fixed(*width))
                .unwrap();
        }
        layout.set_focus_strategy(strategy);
        solve(&mut layout, &HashMap::new());
        layout
    }

    fn solve(layout: &mut ScrollingLayout, constraints: &HashMap<WindowId, SizeConstraints>) {
        layout
            .resolve_geometry_with_constraints(
                Rect::new(0., 0., 1000., 600.),
                GapConfig {
                    inner: 0.,
                    outer: 0.,
                    smart: false,
                },
                constraints,
                Some(WindowId(1)),
            )
            .unwrap();
    }

    #[test]
    fn releases_cross_columns_and_preserve_each_focus_policy() {
        for (strategy, projected, expected, position, limits) in [
            (
                ViewportFocusStrategy::Minimal,
                850.,
                WindowId(3),
                800.,
                (0., 1000.),
            ),
            (
                ViewportFocusStrategy::Center,
                950.,
                WindowId(4),
                900.,
                (-300., 1300.),
            ),
            (
                ViewportFocusStrategy::Paged,
                1450.,
                WindowId(5),
                1600.,
                (0., 1600.),
            ),
        ] {
            let mut layout = layout(&[400.; 5], strategy);
            let geometry = layout.scroll_geometry().unwrap();
            assert_eq!(geometry.limits(), limits);
            assert_eq!(
                geometry.snap_window(projected, Some(WindowId(1))),
                Some(expected)
            );
            assert_eq!(layout.active_window(), Some(WindowId(1)));
            assert_eq!(layout.finish_scroll(projected), Some(expected));
            assert_eq!(layout.viewport_x(), position);
            assert_eq!(layout.active_window(), Some(expected));
            assert_eq!(
                layout
                    .viewport_target()
                    .unwrap()
                    .with_held_widths(&[], None),
                position
            );
        }
    }

    #[test]
    fn release_retains_the_selected_edge_through_relayout_and_width_holds() {
        for strategy in [ViewportFocusStrategy::Minimal, ViewportFocusStrategy::Paged] {
            let mut layout = layout(&[400., 1400., 200., 200.], strategy);
            assert_eq!(layout.finish_scroll(800.), Some(WindowId(2)));
            assert_eq!(layout.viewport_x(), 800.);
            let basis = layout.viewport_target().unwrap();
            assert_eq!(
                basis.with_held_widths(&[(WindowId(1), 200., 400.)], None),
                600.
            );
            assert_eq!(
                basis.with_held_widths(&[(WindowId(2), 1200., 1400.)], None),
                600.
            );
            layout
                .resolve_geometry_with_constraints(
                    Rect::new(0., 0., 1000., 600.),
                    GapConfig {
                        inner: 0.,
                        outer: 0.,
                        smart: false,
                    },
                    &HashMap::new(),
                    Some(WindowId(2)),
                )
                .unwrap();
            assert_eq!(
                layout.viewport_x(),
                800.,
                "an unrelated solve must retain the right edge"
            );
        }
    }

    #[test]
    fn minimal_scrolling_can_snap_a_small_columns_right_edge() {
        let mut layout = layout(&[400.; 4], ViewportFocusStrategy::Minimal);
        assert_eq!(layout.finish_scroll(205.), Some(WindowId(3)));
        assert_eq!(layout.viewport_x(), 200.);
        assert_eq!(
            layout
                .viewport_target()
                .unwrap()
                .with_held_widths(&[(WindowId(2), 300., 400.)], None),
            100.
        );
    }

    #[test]
    fn centered_oversized_columns_can_be_panned_to_both_edges() {
        let mut layout = layout(&[1400.], ViewportFocusStrategy::Center);
        assert_eq!(layout.scroll_geometry().unwrap().limits(), (0., 400.));
        assert_eq!(layout.finish_scroll(0.), Some(WindowId(1)));
        assert_eq!(
            layout.viewport_x(),
            200.,
            "release retains the center policy"
        );
    }

    #[test]
    fn snapshots_ignore_panning_and_detect_width_policy_and_identity_changes() {
        let mut layout = layout(&[400.; 5], ViewportFocusStrategy::Minimal);
        let geometry = layout.scroll_geometry().unwrap();
        assert!(layout.scroll_viewport(100.));
        assert!(geometry.matches(&layout));
        layout.set_focus_strategy(ViewportFocusStrategy::Center);
        assert!(!geometry.matches(&layout));
        layout.set_focus_strategy(ViewportFocusStrategy::Minimal);
        let mut constraints = HashMap::new();
        constraints.insert(
            WindowId(1),
            SizeConstraints {
                min_width: 1400.,
                ..Default::default()
            },
        );
        solve(&mut layout, &constraints);
        assert!(!geometry.matches(&layout));
        assert_eq!(layout.scroll_geometry().unwrap().limits(), (0., 2000.));
        let updated = layout.scroll_geometry().unwrap();
        layout.remove(WindowId(3)).unwrap();
        assert!(!updated.matches(&layout));
    }

    #[test]
    fn shared_snap_points_keep_focus_stable_until_the_view_moves() {
        let mut layout = layout(&[500., 500.], ViewportFocusStrategy::Minimal);
        let geometry = layout.scroll_geometry().unwrap();
        assert_eq!(
            geometry.snap_window(0., Some(WindowId(1))),
            Some(WindowId(1))
        );
        assert_eq!(
            geometry.snap_window(0., Some(WindowId(2))),
            Some(WindowId(2))
        );
        assert_eq!(
            geometry.snap_window(20., Some(WindowId(1))),
            Some(WindowId(2))
        );
        assert_eq!(layout.finish_scroll(20.), Some(WindowId(2)));
        assert_eq!(layout.viewport_x(), 0.);
    }

    #[test]
    fn invalid_releases_do_not_mutate_the_layout() {
        let mut layout = layout(&[400.; 5], ViewportFocusStrategy::Minimal);
        let original = layout.clone();
        for projected in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(layout.finish_scroll(projected).is_none());
            assert_eq!(layout, original);
        }
        assert!(ScrollingLayout::default().scroll_geometry().is_none());
    }
}
