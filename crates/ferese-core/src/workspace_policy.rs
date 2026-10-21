use std::collections::HashSet;

use crate::{OutputId, OutputWorkspaceMap, WorkspaceId, WorkspaceSet};

/// Shared presentation model. Identity never changes when positions compact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceView {
    pub id: WorkspaceId,
    pub output: OutputId,
    pub index: u32,
    pub window_count: usize,
    pub visible: bool,
    pub focused: bool,
}

impl OutputWorkspaceMap {
    pub fn workspace_views(&self, workspaces: &WorkspaceSet) -> Vec<WorkspaceView> {
        self.workspace_views_iter(workspaces).collect()
    }

    pub fn workspace_views_iter<'a>(
        &'a self,
        workspaces: &'a WorkspaceSet,
    ) -> impl Iterator<Item = WorkspaceView> + 'a {
        self.connected_outputs()
            .flat_map(move |output| self.workspace_views_for_output(workspaces, output))
    }

    pub fn workspace_views_for_output<'a>(
        &'a self,
        workspaces: &'a WorkspaceSet,
        output: OutputId,
    ) -> impl Iterator<Item = WorkspaceView> + 'a {
        self.ordered_workspace_ids(workspaces, output)
            .enumerate()
            .map(move |(index, id)| {
                let visible = self.active_workspace(output) == Some(id);

                WorkspaceView {
                    id,
                    output,
                    index: index as u32 + 1,
                    window_count: workspaces.workspace(id).unwrap().window_count(),
                    visible,
                    focused: visible && self.focused_output() == Some(output),
                }
            })
    }

    pub fn ordered_workspace_ids<'a>(
        &'a self,
        workspaces: &'a WorkspaceSet,
        output: OutputId,
    ) -> impl DoubleEndedIterator<Item = WorkspaceId> + 'a {
        self.assigned_workspaces(output)
            .filter(|id| workspaces.workspace(*id).is_some())
    }

    pub fn ordered_workspaces(
        &self,
        workspaces: &WorkspaceSet,
        output: OutputId,
    ) -> Vec<WorkspaceId> {
        self.ordered_workspace_ids(workspaces, output).collect()
    }

    /// Out-of-range numeric selection resolves to the trailing spare. Looking
    /// up a gesture target never creates workspaces or changes history.
    pub fn workspace_at(
        &self,
        workspaces: &WorkspaceSet,
        output: OutputId,
        index: u32,
    ) -> Option<WorkspaceId> {
        let index = index.checked_sub(1)? as usize;
        self.ordered_workspace_ids(workspaces, output)
            .nth(index)
            .or_else(|| self.ordered_workspace_ids(workspaces, output).next_back())
    }

    /// Reconcile after membership, selection, output, or transition changes.
    /// History is a reference, not a reason to keep an empty workspace alive.
    pub fn reconcile_workspaces(
        &mut self,
        workspaces: &mut WorkspaceSet,
        transitions: &HashSet<WorkspaceId>,
    ) -> Vec<WorkspaceId> {
        if let Some(active) = self
            .focused_output()
            .and_then(|output| self.active_workspace(output))
        {
            workspaces
                .activate(active)
                .expect("output's active workspace exists");
        }

        let mut protected = HashSet::new();
        let outputs = self.connected_outputs().collect::<Vec<_>>();
        for output in outputs {
            let last = self.ordered_workspace_ids(workspaces, output).next_back();
            if let Some(active) = self.active_workspace(output) {
                protected.insert(active);
            }

            let spare = last
                .filter(|id| workspaces.workspace(*id).is_some_and(|w| w.is_empty()))
                .unwrap_or_else(|| {
                    let id = workspaces.create_workspace();
                    self.assign_workspace(output, id).expect("connected output");
                    id
                });
            protected.insert(spare);
        }

        let removed = workspaces
            .iter()
            .filter(|workspace| {
                workspace.is_empty()
                    && !protected.contains(&workspace.id)
                    && !transitions.contains(&workspace.id)
                    && workspace.id != workspaces.active_id()
            })
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        for id in &removed {
            self.forget_workspace(*id);
            assert!(workspaces.remove_empty(*id));
        }

        removed
    }
}

#[cfg(test)]
mod tests {
    use ferese_layout::{Axis, Rect, WindowId};

    use super::*;
    use crate::OutputGeometry;

    fn setup() -> (WorkspaceSet, OutputWorkspaceMap, OutputId) {
        let set = WorkspaceSet::default();
        let mut outputs = OutputWorkspaceMap::default();
        let output = OutputId(1);
        outputs
            .connect(
                output,
                OutputGeometry::new(0, 0, 1000, 800),
                set.active_id(),
            )
            .unwrap();

        (set, outputs, output)
    }

    fn reconcile(set: &mut WorkspaceSet, outputs: &mut OutputWorkspaceMap) {
        outputs.reconcile_workspaces(set, &HashSet::new());
        assert!(set.validate().is_ok());

        for output in outputs.connected_outputs() {
            let ids = outputs.ordered_workspaces(set, output);
            assert!(set.workspace(*ids.last().unwrap()).unwrap().is_empty());
            let empties = ids
                .iter()
                .filter(|id| set.workspace(**id).unwrap().is_empty())
                .count();
            let active_middle_empty = outputs
                .active_workspace(output)
                .is_some_and(|id| Some(&id) != ids.last() && set.workspace(id).unwrap().is_empty());
            assert_eq!(empties, 1 + usize::from(active_middle_empty));
        }
    }

    #[test]
    fn occupied_workspaces_get_one_spare_and_positions_compact_without_changing_ids() {
        let (mut set, mut outputs, output) = setup();
        let first = set.active_id();
        reconcile(&mut set, &mut outputs);
        assert_eq!(set.iter().count(), 1);
        set.insert_window(WindowId(1), Axis::Horizontal, 0.5)
            .unwrap();
        reconcile(&mut set, &mut outputs);
        let second = outputs.workspace_at(&set, output, 2).unwrap();
        outputs.switch_workspace(output, second).unwrap();
        reconcile(&mut set, &mut outputs);
        set.insert_window(WindowId(2), Axis::Horizontal, 0.5)
            .unwrap();
        reconcile(&mut set, &mut outputs);
        let spare = outputs.workspace_at(&set, output, 3).unwrap();

        set.remove_window(WindowId(1)).unwrap();
        reconcile(&mut set, &mut outputs);

        assert!(set.workspace(first).is_none());
        assert_eq!(outputs.previous_workspace(output), None);
        assert_eq!(outputs.workspace_at(&set, output, 1), Some(second));
        assert_eq!(outputs.workspace_at(&set, output, 99), Some(spare));
        assert_eq!(outputs.workspace_at(&set, output, 0), None);
        assert_eq!(
            outputs
                .workspace_views(&set)
                .iter()
                .map(|view| view.index)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn selected_empty_survives_until_leaving_but_history_does_not_pin_it() {
        let (mut set, mut outputs, output) = setup();
        let first = set.active_id();
        set.insert_window(WindowId(1), Axis::Horizontal, 0.5)
            .unwrap();
        reconcile(&mut set, &mut outputs);
        let spare = outputs.workspace_at(&set, output, 2).unwrap();
        set.remove_window(WindowId(1)).unwrap();
        reconcile(&mut set, &mut outputs);
        assert!(set.workspace(first).is_some());

        outputs.switch_workspace(output, spare).unwrap();
        reconcile(&mut set, &mut outputs);

        assert_eq!(outputs.ordered_workspaces(&set, output), vec![spare]);
        assert_eq!(outputs.previous_workspace(output), None);
    }

    #[test]
    fn transitions_temporarily_protect_empty_workspaces_and_cancelled_lookup_has_no_effect() {
        let (mut set, mut outputs, output) = setup();
        let first = set.active_id();
        set.insert_window(WindowId(1), Axis::Horizontal, 0.5)
            .unwrap();
        reconcile(&mut set, &mut outputs);
        let spare = outputs.workspace_at(&set, output, 2).unwrap();
        let before = outputs.workspace_views(&set);
        for _ in 0..10 {
            assert_eq!(outputs.workspace_at(&set, output, 2), Some(spare));
        }
        assert_eq!(outputs.workspace_views(&set), before);
        assert_eq!(outputs.previous_workspace(output), None);

        outputs.switch_workspace(output, spare).unwrap();
        set.remove_window(WindowId(1)).unwrap();
        outputs.reconcile_workspaces(&mut set, &HashSet::from([first]));
        assert!(set.workspace(first).is_some());
        reconcile(&mut set, &mut outputs);
        assert!(set.workspace(first).is_none());
    }

    #[test]
    fn floating_windows_count_as_occupied_and_last_close_preserves_active_identity() {
        let (mut set, mut outputs, output) = setup();
        let first = set.active_id();
        set.insert_floating_window(WindowId(1), first, Rect::new(0., 0., 100., 100.), true)
            .unwrap();
        reconcile(&mut set, &mut outputs);
        assert_eq!(outputs.workspace_views(&set)[0].window_count, 1);
        assert_eq!(set.iter().count(), 2);

        set.remove_window(WindowId(1)).unwrap();
        reconcile(&mut set, &mut outputs);
        assert_eq!(outputs.active_workspace(output), Some(first));
        let spare = outputs.workspace_at(&set, output, 2).unwrap();
        outputs.switch_workspace(output, spare).unwrap();
        reconcile(&mut set, &mut outputs);
        assert_eq!(set.iter().count(), 1);
    }

    #[test]
    fn monitors_have_independent_positions_and_reconnect_restores_occupied_workspaces() {
        let (mut set, mut outputs, left) = setup();
        let first = set.active_id();
        set.insert_window(WindowId(1), Axis::Horizontal, 0.5)
            .unwrap();
        reconcile(&mut set, &mut outputs);
        let right = OutputId(2);
        let second = set.create_workspace();
        outputs
            .connect(right, OutputGeometry::new(1000, 0, 1000, 800), second)
            .unwrap();
        outputs.focus_output(right).unwrap();
        reconcile(&mut set, &mut outputs);
        set.insert_window(WindowId(2), Axis::Horizontal, 0.5)
            .unwrap();
        reconcile(&mut set, &mut outputs);
        assert_eq!(outputs.workspace_at(&set, left, 1), Some(first));
        assert_eq!(outputs.workspace_at(&set, right, 1), Some(second));
        assert_eq!(
            outputs
                .workspace_views(&set)
                .iter()
                .filter(|view| view.visible)
                .count(),
            2
        );
        assert_eq!(
            outputs
                .workspace_views(&set)
                .iter()
                .filter(|view| view.focused)
                .count(),
            1
        );

        outputs.disconnect(right).unwrap();
        reconcile(&mut set, &mut outputs);
        let fallback = set.create_workspace();
        outputs
            .connect(right, OutputGeometry::new(1000, 0, 1000, 800), fallback)
            .unwrap();
        reconcile(&mut set, &mut outputs);

        assert_eq!(outputs.output_for_workspace(first), Some(left));
        assert_eq!(outputs.output_for_workspace(second), Some(right));
        assert!(set.workspace(fallback).is_none());
        assert_eq!(outputs.workspace_at(&set, right, 1), Some(second));
    }
}

#[cfg(test)]
mod sequences {
    use ferese_layout::{Axis, WindowId};

    use super::*;
    use crate::OutputGeometry;

    #[test]
    fn repeated_switch_move_close_and_hotplug_keep_workspace_invariants() {
        let mut set = WorkspaceSet::default();
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(
                OutputId(1),
                OutputGeometry::new(0, 0, 1000, 800),
                set.active_id(),
            )
            .unwrap();
        let mut next_window = 1;
        let mut seed = 0x1234_5678_u64;

        for step in 0..500 {
            outputs.reconcile_workspaces(&mut set, &HashSet::new());
            let output = outputs.focused_output().unwrap();
            let windows = set
                .iter()
                .flat_map(|workspace| workspace.layout.window_ids())
                .collect::<Vec<_>>();
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;

            match seed % 5 {
                0 => {
                    set.insert_window(WindowId(next_window), Axis::Horizontal, 0.5)
                        .unwrap();
                    next_window += 1;
                }
                1 if !windows.is_empty() => {
                    set.remove_window(windows[seed as usize % windows.len()])
                        .unwrap();
                }
                2 => {
                    let index = (seed % 9 + 1) as u32;
                    let target = outputs.workspace_at(&set, output, index).unwrap();
                    outputs.switch_workspace(output, target).unwrap();
                }
                3 if !windows.is_empty() => {
                    let target = outputs.workspace_at(&set, output, u32::MAX).unwrap();
                    set.move_window_to_workspace(
                        windows[seed as usize % windows.len()],
                        target,
                        Axis::Horizontal,
                        0.5,
                    )
                    .unwrap();
                }
                _ => {
                    if outputs.active_workspace(OutputId(2)).is_some() {
                        outputs.disconnect(OutputId(2)).unwrap();
                    } else {
                        let fallback = set.create_workspace();
                        outputs
                            .connect(
                                OutputId(2),
                                OutputGeometry::new(1000, 0, 1000, 800),
                                fallback,
                            )
                            .unwrap();
                        outputs.focus_output(OutputId(2)).unwrap();
                    }
                }
            }

            outputs.reconcile_workspaces(&mut set, &HashSet::new());
            assert!(set.validate().is_ok(), "step {step}");
            let views = outputs.workspace_views(&set);
            assert_eq!(views.iter().filter(|view| view.focused).count(), 1);
            assert_eq!(views.len(), set.iter().count());
            for output in outputs.connected_outputs() {
                let rows = views
                    .iter()
                    .filter(|view| view.output == output)
                    .collect::<Vec<_>>();
                assert_eq!(rows.last().unwrap().window_count, 0);
                for (index, row) in rows.iter().enumerate() {
                    assert_eq!(row.index, index as u32 + 1);
                    if row.window_count == 0 && index + 1 != rows.len() {
                        assert!(row.visible, "abandoned empty workspace at step {step}");
                    }
                }
                if let Some(previous) = outputs.previous_workspace(output) {
                    assert!(rows.iter().any(|row| row.id == previous));
                }
            }
        }
    }
}
