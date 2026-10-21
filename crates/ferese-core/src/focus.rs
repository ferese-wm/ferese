use std::collections::HashSet;

use ferese_layout::WindowId;

/// Most recent focus first. Layer surfaces and temporary selections do not enter this list.
#[derive(Debug, Default)]
pub struct FocusHistory {
    windows: Vec<WindowId>,
}

impl FocusHistory {
    pub fn record(&mut self, window: WindowId) {
        if self.windows.first() == Some(&window) {
            return;
        }
        self.remove(window);
        self.windows.insert(0, window);
    }

    pub fn remove(&mut self, window: WindowId) {
        self.windows.retain(|id| *id != window);
    }

    pub fn candidates(
        &self,
        current: Option<WindowId>,
        available: impl AsRef<[WindowId]>,
    ) -> Vec<WindowId> {
        let mut remaining = available.as_ref().iter().copied().collect::<HashSet<_>>();
        let mut ordered = Vec::with_capacity(remaining.len());

        for id in current.into_iter().chain(self.windows.iter().copied()) {
            if remaining.remove(&id) {
                ordered.push(id);
            }
        }

        let mut tail = remaining.into_iter().collect::<Vec<_>>();
        tail.sort_unstable_by_key(|id| id.0);
        ordered.extend(tail);
        ordered
    }
}

/// A frozen traversal order: previewing does not reorder focus history.
#[derive(Debug)]
pub struct FocusCycle {
    windows: Vec<WindowId>,
    selected: Option<WindowId>,
}

impl FocusCycle {
    pub fn new(windows: Vec<WindowId>, current: Option<WindowId>) -> Self {
        let selected = current.filter(|id| windows.contains(id));
        Self { windows, selected }
    }

    pub fn windows(&self) -> &[WindowId] {
        &self.windows
    }

    pub fn reconcile(&mut self, available: &[WindowId]) -> Option<WindowId> {
        if self.selected.is_some_and(|id| available.contains(&id)) {
            self.selected
        } else {
            self.advance(false, available)
        }
    }

    pub fn advance(&mut self, reverse: bool, available: &[WindowId]) -> Option<WindowId> {
        let available = available.iter().copied().collect::<HashSet<_>>();

        // Keep the old index when the selected window disappears so forward
        // traversal selects its successor rather than skipping over it.
        let old = self
            .selected
            .and_then(|id| self.windows.iter().position(|candidate| *candidate == id));
        let before = old.map(|index| {
            self.windows[..index]
                .iter()
                .filter(|id| available.contains(id))
                .count()
        });
        let survives = self.selected.is_some_and(|id| available.contains(&id));
        self.windows.retain(|id| available.contains(id));
        if self.windows.is_empty() {
            self.selected = None;
            return None;
        }
        let len = self.windows.len();
        let index = match (before, reverse, survives) {
            (Some(index), true, _) => (index + len - 1) % len,
            (Some(index), false, true) => (index + 1) % len,
            (Some(index), false, false) => index % len,
            (None, true, _) => len - 1,
            (None, false, _) => 0,
        };
        self.selected = Some(self.windows[index]);
        self.selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_candidates_preserve_linear_order_with_duplicates_and_missing_windows() {
        let mut history = FocusHistory::default();

        for id in [7, 2, 5, 9, 2] {
            history.record(WindowId(id));
        }

        for mask in 0..1024 {
            let available = (0..10)
                .filter(|id| mask & (1 << id) != 0)
                .flat_map(|id| [WindowId(id), WindowId(id)])
                .collect::<Vec<_>>();

            for current in [None, Some(WindowId(2)), Some(WindowId(99))] {
                let mut sorted = available.clone();
                sorted.sort_unstable_by_key(|id| id.0);
                sorted.dedup();
                let mut expected = Vec::new();

                for id in current
                    .into_iter()
                    .chain(history.windows.iter().copied())
                    .chain(sorted.iter().copied())
                {
                    if sorted.contains(&id) && !expected.contains(&id) {
                        expected.push(id);
                    }
                }

                assert_eq!(history.candidates(current, &available), expected);
            }
        }
    }

    #[test]
    fn history_is_unique_filters_unavailable_and_last_focus_toggles() {
        let mut history = FocusHistory::default();
        for id in [1, 2, 3, 3] {
            history.record(WindowId(id));
        }
        let available = vec![WindowId(1), WindowId(2), WindowId(3), WindowId(4)];
        assert_eq!(
            history.candidates(Some(WindowId(3)), available.clone()),
            [3, 2, 1, 4].map(WindowId)
        );
        history.record(WindowId(2));
        assert_eq!(
            history.candidates(Some(WindowId(2)), available)[1],
            WindowId(3)
        );
        history.remove(WindowId(3));
        assert_eq!(
            history.candidates(None, vec![WindowId(1), WindowId(4)]),
            [1, 4].map(WindowId)
        );
    }

    #[test]
    fn cycle_freezes_order_wraps_reverses_and_skips_closed_windows() {
        let available = [3, 2, 1].map(WindowId);
        let mut cycle = FocusCycle::new(available.to_vec(), Some(WindowId(3)));
        assert_eq!(cycle.advance(false, &available), Some(WindowId(2)));
        assert_eq!(cycle.advance(false, &available), Some(WindowId(1)));
        assert_eq!(cycle.advance(false, &available), Some(WindowId(3)));
        assert_eq!(cycle.advance(true, &available), Some(WindowId(1)));
        assert_eq!(
            cycle.advance(false, &[WindowId(3), WindowId(2)]),
            Some(WindowId(3))
        );
        assert_eq!(cycle.advance(false, &[]), None);
    }

    #[test]
    fn missing_current_focus_starts_at_most_recent_and_new_windows_do_not_join_cycle() {
        let mut cycle = FocusCycle::new(vec![WindowId(2), WindowId(1)], None);
        assert_eq!(
            cycle.advance(false, &[WindowId(3), WindowId(2), WindowId(1)]),
            Some(WindowId(2))
        );
        assert_eq!(cycle.advance(true, &[WindowId(2)]), Some(WindowId(2)));
    }

    #[test]
    fn closing_the_preview_selects_its_successor_before_commit() {
        let available = [3, 2, 1].map(WindowId);
        let mut cycle = FocusCycle::new(available.to_vec(), Some(WindowId(3)));
        assert_eq!(cycle.advance(false, &available), Some(WindowId(2)));
        assert_eq!(
            cycle.reconcile(&[WindowId(3), WindowId(1)]),
            Some(WindowId(1))
        );
        assert_eq!(
            cycle.reconcile(&[WindowId(3), WindowId(1)]),
            Some(WindowId(1))
        );
    }
}
