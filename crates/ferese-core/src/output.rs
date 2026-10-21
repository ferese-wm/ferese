use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fmt;

use crate::WorkspaceId;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OutputId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl OutputGeometry {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn center(self) -> (i64, i64) {
        (
            i64::from(self.x) * 2 + i64::from(self.width),
            i64::from(self.y) * 2 + i64::from(self.height),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputError {
    AlreadyConnected(OutputId),
    UnknownOutput(OutputId),
    InvalidGeometry(OutputId),
    WorkspaceAlreadyAssigned {
        workspace: WorkspaceId,
        output: OutputId,
    },
}

impl fmt::Display for OutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyConnected(output) => write!(formatter, "output {output:?} is connected"),
            Self::UnknownOutput(output) => write!(formatter, "unknown output {output:?}"),
            Self::InvalidGeometry(output) => {
                write!(formatter, "invalid geometry for output {output:?}")
            }
            Self::WorkspaceAlreadyAssigned { workspace, output } => {
                write!(
                    formatter,
                    "workspace {workspace:?} is assigned to {output:?}"
                )
            }
        }
    }
}

impl Error for OutputError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceSwitch {
    Activated(OutputId),
    FocusedExisting(OutputId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OutputState {
    geometry: OutputGeometry,
    active: WorkspaceId,
    previous: Option<WorkspaceId>,
    workspaces: BTreeSet<WorkspaceId>,
}

impl OutputState {
    fn activate(&mut self, workspace: WorkspaceId) {
        if self.active != workspace {
            self.previous = self
                .workspaces
                .contains(&self.active)
                .then_some(self.active);
            self.active = workspace;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EvacuatedWorkspace {
    workspace: WorkspaceId,
    target: Option<OutputId>,
    revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EvacuationRecord {
    active: WorkspaceId,
    workspaces: Vec<EvacuatedWorkspace>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputWorkspaceMap {
    outputs: BTreeMap<OutputId, OutputState>,
    assignments: HashMap<WorkspaceId, OutputId>,
    homes: HashMap<WorkspaceId, OutputId>,
    revisions: HashMap<WorkspaceId, u64>,
    evacuations: HashMap<OutputId, EvacuationRecord>,
    focused: Option<OutputId>,
    orphaned_focus: Option<WorkspaceId>,
}

/// A staged ownership decision. No windows, renderers or protocol resources are copied.
#[derive(Debug)]
pub struct DesktopPlan {
    pub outputs: OutputWorkspaceMap,
    pub created_workspaces: Vec<WorkspaceId>,
    pub affected_outputs: BTreeSet<OutputId>,
    pub focus_changed: bool,
}

impl OutputWorkspaceMap {
    /// Plan against the final usable inventory, never a per-device intermediate.
    /// An empty inventory is a valid headless desktop after physical loss.
    pub fn plan_desktop(
        &self,
        usable: &[(OutputId, OutputGeometry)],
        workspaces: &[WorkspaceId],
        mut next_workspace: WorkspaceId,
    ) -> Result<DesktopPlan, OutputError> {
        let mut desired = BTreeMap::new();
        for &(id, geometry) in usable {
            if geometry.width <= 0
                || geometry.height <= 0
                || geometry.x.checked_add(geometry.width).is_none()
                || geometry.y.checked_add(geometry.height).is_none()
            {
                return Err(OutputError::InvalidGeometry(id));
            }
            if desired.insert(id, geometry).is_some() {
                return Err(OutputError::AlreadyConnected(id));
            }
        }
        let returning = desired
            .keys()
            .filter(|id| !self.outputs.contains_key(id))
            .filter_map(|id| self.evacuations.get(id).map(|record| (*id, record.clone())))
            .collect::<Vec<_>>();
        let mut outputs = self.clone();
        let mut created_workspaces = Vec::new();
        // New destinations exist before evacuation. Temporary removals must
        // never become migration destinations, even when several GPUs disappear.
        for (&id, &geometry) in &desired {
            if outputs.outputs.contains_key(&id) {
                outputs.update_geometry(id, geometry);
                continue;
            }
            let fallback = workspaces
                .iter()
                .copied()
                .filter(|workspace| {
                    outputs.output_for_workspace(*workspace).is_none()
                        && outputs
                            .home_output(*workspace)
                            .is_none_or(|home| home == id)
                })
                .min()
                .unwrap_or_else(|| {
                    let workspace = next_workspace;
                    next_workspace.0 = next_workspace
                        .0
                        .checked_add(1)
                        .expect("workspace IDs exhausted");
                    created_workspaces.push(workspace);
                    workspace
                });
            outputs.connect(id, geometry, fallback)?;
        }
        let mut removed = self
            .connected_outputs()
            .filter(|id| !desired.contains_key(id))
            .map(|id| (id, outputs.outputs.remove(&id).unwrap()))
            .collect::<Vec<_>>();
        // Preserve the original focused workspace when several outputs vanish.
        removed.sort_by_key(|(id, _)| (Some(*id) == self.focused, *id));
        for (id, state) in removed {
            outputs.evacuate(id, state);
        }
        // A returning output may initially be unable to reclaim the only
        // workspace of a donor that is itself retiring. Retry the same policy
        // against the final inventory, where that donor no longer needs it.
        for (id, record) in returning {
            let reclaimed = outputs.reclaim(id, &record);
            let state = outputs.outputs.get_mut(&id).unwrap();
            state.workspaces.extend(reclaimed.iter().copied());
            if reclaimed.contains(&record.active) {
                state.activate(record.active);
            }
        }
        debug_assert!(outputs.validate());
        let mut affected_outputs = BTreeSet::new();
        for id in self.connected_outputs().chain(outputs.connected_outputs()) {
            if self.outputs.get(&id) != outputs.outputs.get(&id) {
                affected_outputs.insert(id);
            }
        }
        let focus_changed = self.focused != outputs.focused
            || self.focused.and_then(|id| self.active_workspace(id))
                != outputs.focused.and_then(|id| outputs.active_workspace(id));
        if focus_changed {
            affected_outputs.extend(self.focused);
            affected_outputs.extend(outputs.focused);
        }
        Ok(DesktopPlan {
            outputs,
            created_workspaces,
            affected_outputs,
            focus_changed,
        })
    }

    pub fn assigned_workspaces(
        &self,
        output: OutputId,
    ) -> impl DoubleEndedIterator<Item = WorkspaceId> + '_ {
        self.outputs
            .get(&output)
            .into_iter()
            .flat_map(|state| state.workspaces.iter().copied())
    }

    pub fn focused_output(&self) -> Option<OutputId> {
        self.focused
    }

    pub fn active_workspace(&self, output: OutputId) -> Option<WorkspaceId> {
        self.outputs.get(&output).map(|state| state.active)
    }

    pub fn previous_workspace(&self, output: OutputId) -> Option<WorkspaceId> {
        self.outputs.get(&output).and_then(|state| state.previous)
    }

    pub fn output_for_workspace(&self, workspace: WorkspaceId) -> Option<OutputId> {
        self.assignments.get(&workspace).copied()
    }

    pub fn home_output(&self, workspace: WorkspaceId) -> Option<OutputId> {
        self.homes.get(&workspace).copied()
    }

    pub fn forget_workspace(&mut self, workspace: WorkspaceId) -> bool {
        if self
            .outputs
            .values()
            .any(|output| output.active == workspace)
        {
            return false;
        }
        if let Some(owner) = self.assignments.remove(&workspace)
            && let Some(output) = self.outputs.get_mut(&owner)
        {
            output.workspaces.remove(&workspace);
            if output.previous == Some(workspace) {
                output.previous = None;
            }
        }
        self.revisions.remove(&workspace);
        self.homes.remove(&workspace);
        if self.orphaned_focus == Some(workspace) {
            self.orphaned_focus = None;
        }
        self.evacuations.retain(|_, record| {
            record
                .workspaces
                .retain(|entry| entry.workspace != workspace);
            if record.active == workspace {
                if let Some(entry) = record.workspaces.first() {
                    record.active = entry.workspace;
                } else {
                    return false;
                }
            }
            true
        });
        debug_assert!(self.validate());
        true
    }

    pub fn connected_outputs(&self) -> impl Iterator<Item = OutputId> + '_ {
        self.outputs.keys().copied()
    }

    pub fn geometry(&self, output: OutputId) -> Option<OutputGeometry> {
        self.outputs.get(&output).map(|state| state.geometry)
    }

    pub fn update_geometry(&mut self, output: OutputId, geometry: OutputGeometry) {
        if let Some(state) = self.outputs.get_mut(&output) {
            state.geometry = geometry;
        }
    }

    fn reclaim(&mut self, output: OutputId, record: &EvacuationRecord) -> Vec<WorkspaceId> {
        let mut reclaimed = Vec::new();
        for evacuated in &record.workspaces {
            let target = self
                .assignments
                .get(&evacuated.workspace)
                .copied()
                .or_else(|| {
                    evacuated
                        .target
                        .filter(|target| self.outputs.contains_key(target))
                });
            let assignment_matches = self.homes.get(&evacuated.workspace) == Some(&output);
            let revision_matches = self.revision(evacuated.workspace) == evacuated.revision;

            if assignment_matches && revision_matches {
                if target == Some(output) {
                    reclaimed.push(evacuated.workspace);
                    continue;
                }
                // Cleanup may have removed every other workspace while this
                // monitor was disconnected. Never strand the donor output;
                // the reconnecting monitor can use its fresh fallback.
                if target
                    .and_then(|target| self.outputs.get(&target))
                    .is_some_and(|state| {
                        state.workspaces.len() == 1
                            && state.workspaces.contains(&evacuated.workspace)
                    })
                {
                    continue;
                }

                if let Some(target) = target
                    && let Some(target_state) = self.outputs.get_mut(&target)
                {
                    target_state.workspaces.remove(&evacuated.workspace);
                    if target_state.previous == Some(evacuated.workspace) {
                        target_state.previous = None;
                    }
                    if target_state.active == evacuated.workspace
                        && let Some(active) = target_state
                            .workspaces
                            .iter()
                            .copied()
                            .min_by_key(|workspace| workspace.0)
                    {
                        target_state.activate(active);
                    }
                }
                self.assignments.insert(evacuated.workspace, output);
                reclaimed.push(evacuated.workspace);
            }
        }
        reclaimed
    }

    pub fn connect(
        &mut self,
        output: OutputId,
        geometry: OutputGeometry,
        fallback_workspace: WorkspaceId,
    ) -> Result<WorkspaceId, OutputError> {
        if self.outputs.contains_key(&output) {
            return Err(OutputError::AlreadyConnected(output));
        }

        let record = self.evacuations.remove(&output);
        let mut reclaimed = record
            .as_ref()
            .map(|record| self.reclaim(output, record))
            .unwrap_or_default();

        let active = record
            .as_ref()
            .and_then(|record| reclaimed.contains(&record.active).then_some(record.active))
            .or_else(|| {
                reclaimed
                    .iter()
                    .copied()
                    .min_by_key(|workspace| workspace.0)
            });
        let mut active = match active {
            Some(active) => active,
            None => {
                if let Some(owner) = self.assignments.get(&fallback_workspace).copied() {
                    return Err(OutputError::WorkspaceAlreadyAssigned {
                        workspace: fallback_workspace,
                        output: owner,
                    });
                }
                self.assignments.insert(fallback_workspace, output);
                self.homes.insert(fallback_workspace, output);
                reclaimed.push(fallback_workspace);
                fallback_workspace
            }
        };

        if self.outputs.is_empty() {
            // A replacement may have to reuse the final CRTC, or every cable
            // may be unplugged. Attach unowned evacuated workspaces without
            // changing their homes, so every window remains reachable.
            let orphaned = self
                .homes
                .keys()
                .filter(|workspace| !self.assignments.contains_key(workspace))
                .copied()
                .collect::<Vec<_>>();
            for workspace in orphaned {
                self.assignments.insert(workspace, output);
                reclaimed.push(workspace);
            }
            if let Some(focused) = self.orphaned_focus.take()
                && reclaimed.contains(&focused)
            {
                active = focused;
            }
        }

        self.outputs.insert(
            output,
            OutputState {
                geometry,
                active,
                previous: None,
                workspaces: reclaimed.into_iter().collect(),
            },
        );
        self.focused.get_or_insert(output);

        debug_assert!(self.validate());
        Ok(active)
    }

    pub fn disconnect(&mut self, output: OutputId) -> Result<Option<OutputId>, OutputError> {
        let removed = self
            .outputs
            .remove(&output)
            .ok_or(OutputError::UnknownOutput(output))?;
        let target = self.evacuate(output, removed);
        debug_assert!(self.validate());
        Ok(target)
    }

    fn evacuate(&mut self, output: OutputId, removed: OutputState) -> Option<OutputId> {
        let target = self.migration_target(output, removed.geometry);
        let removed_had_focus = self.focused == Some(output);
        let mut evacuated = removed.workspaces.into_iter().collect::<Vec<_>>();
        evacuated.sort_by_key(|workspace| workspace.0);

        for workspace in &evacuated {
            if let Some(target) = target {
                self.assignments.insert(*workspace, target);
                self.outputs
                    .get_mut(&target)
                    .expect("migration target is connected")
                    .workspaces
                    .insert(*workspace);
            } else {
                self.assignments.remove(workspace);
            }
        }

        if removed_had_focus {
            self.focused = target;
            if target.is_none() {
                self.orphaned_focus = Some(removed.active);
            }
            if let Some(target) = target {
                self.outputs
                    .get_mut(&target)
                    .expect("migration target is connected")
                    .activate(removed.active);
            }
        }

        self.evacuations.insert(
            output,
            EvacuationRecord {
                active: removed.active,
                workspaces: evacuated
                    .into_iter()
                    .map(|workspace| EvacuatedWorkspace {
                        workspace,
                        target,
                        revision: self.revision(workspace),
                    })
                    .collect(),
            },
        );

        target
    }

    pub fn focus_output(&mut self, output: OutputId) -> Result<(), OutputError> {
        if !self.outputs.contains_key(&output) {
            return Err(OutputError::UnknownOutput(output));
        }
        self.focused = Some(output);
        Ok(())
    }

    pub fn switch_workspace(
        &mut self,
        output: OutputId,
        workspace: WorkspaceId,
    ) -> Result<WorkspaceSwitch, OutputError> {
        if !self.outputs.contains_key(&output) {
            return Err(OutputError::UnknownOutput(output));
        }

        if let Some(owner) = self.assignments.get(&workspace).copied()
            && owner != output
        {
            self.focused = Some(owner);
            self.outputs
                .get_mut(&owner)
                .expect("workspace owner is connected")
                .activate(workspace);
            return Ok(WorkspaceSwitch::FocusedExisting(owner));
        }

        let reassigned = self.assignments.insert(workspace, output) != Some(output);
        let state = self
            .outputs
            .get_mut(&output)
            .expect("output was checked above");
        state.workspaces.insert(workspace);
        state.activate(workspace);
        self.focused = Some(output);
        if reassigned {
            self.homes.insert(workspace, output);
            self.bump_revision(workspace);
        }

        debug_assert!(self.validate());
        Ok(WorkspaceSwitch::Activated(output))
    }

    pub fn select_workspace(
        &mut self,
        output: OutputId,
        workspace: WorkspaceId,
        auto_back_and_forth: bool,
    ) -> Result<WorkspaceSwitch, OutputError> {
        let workspace = if auto_back_and_forth && self.active_workspace(output) == Some(workspace) {
            self.previous_workspace(output).unwrap_or(workspace)
        } else {
            workspace
        };
        self.switch_workspace(output, workspace)
    }

    pub fn assign_workspace(
        &mut self,
        output: OutputId,
        workspace: WorkspaceId,
    ) -> Result<OutputId, OutputError> {
        if !self.outputs.contains_key(&output) {
            return Err(OutputError::UnknownOutput(output));
        }
        if let Some(owner) = self.assignments.get(&workspace).copied() {
            return Ok(owner);
        }

        self.assignments.insert(workspace, output);
        self.homes.insert(workspace, output);
        self.outputs
            .get_mut(&output)
            .expect("output was checked above")
            .workspaces
            .insert(workspace);
        self.bump_revision(workspace);

        debug_assert!(self.validate());
        Ok(output)
    }

    /// Explicit reassignment changes the home, unlike merely focusing an evacuated workspace.
    /// The caller supplies a fresh workspace if moving the donor's final workspace.
    pub fn reassign_workspace(
        &mut self,
        output: OutputId,
        workspace: WorkspaceId,
        donor_fallback: WorkspaceId,
    ) -> Result<(), OutputError> {
        if !self.outputs.contains_key(&output) {
            return Err(OutputError::UnknownOutput(output));
        }
        if let Some(owner) = self.assignments.get(&workspace).copied()
            && owner != output
        {
            let donor = self
                .outputs
                .get_mut(&owner)
                .expect("assigned owner is connected");
            if donor.workspaces.len() == 1 {
                if let Some(assigned) = self.assignments.get(&donor_fallback) {
                    return Err(OutputError::WorkspaceAlreadyAssigned {
                        workspace: donor_fallback,
                        output: *assigned,
                    });
                }
                donor.workspaces.insert(donor_fallback);
                self.assignments.insert(donor_fallback, owner);
                self.homes.insert(donor_fallback, owner);
            }
            donor.workspaces.remove(&workspace);
            if donor.previous == Some(workspace) {
                donor.previous = None;
            }
            if donor.active == workspace {
                donor.active = *donor.workspaces.first().expect("donor retains a workspace");
                if donor.previous == Some(donor.active) {
                    donor.previous = None;
                }
            }
        }
        self.assignments.insert(workspace, output);
        self.homes.insert(workspace, output);
        self.outputs
            .get_mut(&output)
            .unwrap()
            .workspaces
            .insert(workspace);
        self.bump_revision(workspace);
        debug_assert!(self.validate());
        Ok(())
    }

    fn migration_target(
        &self,
        removed: OutputId,
        removed_geometry: OutputGeometry,
    ) -> Option<OutputId> {
        if let Some(focused) = self.focused
            && focused != removed
            && self.outputs.contains_key(&focused)
        {
            return Some(focused);
        }

        let removed_center = removed_geometry.center();
        self.outputs
            .iter()
            .min_by_key(|(id, state)| {
                let center = state.geometry.center();
                let x = center.0 - removed_center.0;
                let y = center.1 - removed_center.1;
                (x * x + y * y, **id)
            })
            .map(|(id, _)| *id)
    }

    fn revision(&self, workspace: WorkspaceId) -> u64 {
        self.revisions.get(&workspace).copied().unwrap_or(0)
    }

    fn bump_revision(&mut self, workspace: WorkspaceId) {
        let revision = self.revisions.entry(workspace).or_default();
        *revision = revision.saturating_add(1);
    }

    fn validate(&self) -> bool {
        if self
            .focused
            .is_some_and(|focused| !self.outputs.contains_key(&focused))
        {
            return false;
        }

        for (output, state) in &self.outputs {
            if !state.workspaces.contains(&state.active) {
                return false;
            }
            if state.previous.is_some_and(|previous| {
                previous == state.active || !state.workspaces.contains(&previous)
            }) {
                return false;
            }
            for workspace in &state.workspaces {
                if self.assignments.get(workspace) != Some(output) {
                    return false;
                }
            }
        }

        self.assignments.iter().all(|(workspace, output)| {
            self.outputs
                .get(output)
                .is_some_and(|state| state.workspaces.contains(workspace))
        })
    }
}

#[cfg(test)]
mod tests {
    fn plan(map: &OutputWorkspaceMap, ids: &[u64]) -> DesktopPlan {
        map.plan_desktop(
            &ids.iter()
                .map(|id| (OutputId(*id), geometry(*id as i32 * 1920)))
                .collect::<Vec<_>>(),
            &(1..=32).map(WorkspaceId).collect::<Vec<_>>(),
            WorkspaceId(33),
        )
        .unwrap()
    }

    #[test]
    fn staged_add_remove_and_noop_leave_the_source_unchanged() {
        let original = OutputWorkspaceMap::default();
        let first = plan(&original, &[1]);
        assert_eq!(original, OutputWorkspaceMap::default());
        let second = plan(&first.outputs, &[1, 2]);
        assert_eq!(first.outputs.connected_outputs().count(), 1);
        assert_eq!(second.outputs.connected_outputs().count(), 2);
        let unchanged = plan(&second.outputs, &[2, 1]);
        assert_eq!(unchanged.outputs, second.outputs);
        assert!(unchanged.affected_outputs.is_empty());
        assert!(!unchanged.focus_changed);
        let removed = plan(&second.outputs, &[1]);
        assert_eq!(removed.outputs.focused_output(), Some(OutputId(1)));
        assert_eq!(
            removed.outputs.active_workspace(OutputId(1)),
            Some(WorkspaceId(1))
        );
        assert_eq!(
            removed.outputs.output_for_workspace(WorkspaceId(2)),
            Some(OutputId(1))
        );
    }

    #[test]
    fn simultaneous_removals_migrate_directly_to_final_outputs_and_preserve_focus() {
        let mut before = plan(&OutputWorkspaceMap::default(), &[1, 2, 3]).outputs;
        before.focus_output(OutputId(2)).unwrap();
        let after = plan(&before, &[4]);
        assert_eq!(after.outputs.focused_output(), Some(OutputId(4)));
        assert_eq!(
            after.outputs.active_workspace(OutputId(4)),
            Some(WorkspaceId(2))
        );
        for workspace in 1..=3 {
            assert_eq!(
                after.outputs.output_for_workspace(WorkspaceId(workspace)),
                Some(OutputId(4))
            );
        }
        for evacuation in after.outputs.evacuations.values() {
            assert!(
                evacuation
                    .workspaces
                    .iter()
                    .all(|workspace| workspace.target == Some(OutputId(4)))
            );
        }
        assert_eq!(before.connected_outputs().count(), 3);
    }

    #[test]
    fn staged_reconnect_respects_explicit_reassignment() {
        let before = plan(&OutputWorkspaceMap::default(), &[1, 2]).outputs;
        let mut evacuated = plan(&before, &[1]).outputs;
        let restored = plan(&evacuated, &[1, 2]);
        assert_eq!(
            restored.outputs.output_for_workspace(WorkspaceId(2)),
            Some(OutputId(2))
        );
        evacuated
            .reassign_workspace(OutputId(1), WorkspaceId(2), WorkspaceId(9))
            .unwrap();
        let reassigned = plan(&evacuated, &[1, 2]);
        assert_eq!(
            reassigned.outputs.output_for_workspace(WorkspaceId(2)),
            Some(OutputId(1))
        );
    }

    #[test]
    fn returning_home_reclaims_from_a_donor_retired_in_the_same_plan() {
        let mut before = plan(&OutputWorkspaceMap::default(), &[1, 2, 3]).outputs;
        before.focus_output(OutputId(2)).unwrap();
        before.disconnect(OutputId(1)).unwrap();
        before
            .switch_workspace(OutputId(2), WorkspaceId(1))
            .unwrap();
        assert!(before.forget_workspace(WorkspaceId(2)));
        before.focus_output(OutputId(3)).unwrap();
        let after = plan(&before, &[1, 3]).outputs;
        assert_eq!(
            after.output_for_workspace(WorkspaceId(1)),
            Some(OutputId(1))
        );
        assert_eq!(after.active_workspace(OutputId(1)), Some(WorkspaceId(1)));
        assert_eq!(after.focused_output(), Some(OutputId(3)));
    }

    #[test]
    fn invalid_inventory_cannot_mutate_the_published_map() {
        let before = plan(&OutputWorkspaceMap::default(), &[1, 2]).outputs;
        let saved = before.clone();
        assert!(
            before
                .plan_desktop(
                    &[(OutputId(1), geometry(0)), (OutputId(1), geometry(1))],
                    &[],
                    WorkspaceId(3)
                )
                .is_err()
        );
        assert!(
            before
                .plan_desktop(
                    &[(OutputId(3), OutputGeometry::new(0, 0, 0, 800))],
                    &[],
                    WorkspaceId(3)
                )
                .is_err()
        );
        assert_eq!(before, saved);
    }

    #[test]
    fn physical_loss_can_publish_headless_and_recover_all_workspaces() {
        let mut before = plan(&OutputWorkspaceMap::default(), &[1, 2]).outputs;
        before.focus_output(OutputId(2)).unwrap();
        let headless = plan(&before, &[]).outputs;
        assert_eq!(headless.focused_output(), None);
        assert!(headless.assignments.is_empty());
        let recovered = plan(&headless, &[3]).outputs;
        assert_eq!(
            recovered.active_workspace(OutputId(3)),
            Some(WorkspaceId(2))
        );
        for id in [1, 2] {
            assert_eq!(
                recovered.output_for_workspace(WorkspaceId(id)),
                Some(OutputId(3))
            );
        }
    }

    #[test]
    fn deterministic_topology_sequence_is_coherent_and_idempotent() {
        let mut map = OutputWorkspaceMap::default();
        let mut seed = 0x5eed_u64;
        for step in 0..512 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let ids = (1..=4)
                .filter(|id| seed & (1 << id) != 0)
                .collect::<Vec<_>>();
            let next = plan(&map, &ids);
            assert!(next.outputs.validate(), "step {step}");
            assert_eq!(plan(&next.outputs, &ids).outputs, next.outputs);
            map = next.outputs;
            if let Some(&id) = ids.first() {
                map.focus_output(OutputId(id)).unwrap();
                let workspace = WorkspaceId(16 + step % 8);
                map.switch_workspace(OutputId(id), workspace).unwrap();
                if step % 3 == 0 {
                    map.reassign_workspace(OutputId(id), workspace, WorkspaceId(31))
                        .unwrap();
                }
            }
            assert!(map.validate());
        }
    }

    #[test]
    fn deterministic_desktop_sequence_preserves_every_window() {
        use crate::WorkspaceSet;
        use ferese_layout::{Axis, WindowId};
        let mut workspaces = WorkspaceSet::default();
        let mut outputs = OutputWorkspaceMap::default();
        let mut seed = 42u64;
        let mut windows = std::collections::HashSet::new();
        for step in 1..=256 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let inventory = (1..=3)
                .filter(|id| seed & (1 << id) != 0)
                .map(|id| (OutputId(id), geometry(id as i32 * 800)))
                .collect::<Vec<_>>();
            let ids = workspaces
                .iter()
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>();
            let plan = outputs
                .plan_desktop(&inventory, &ids, workspaces.next_workspace_id())
                .unwrap();
            for id in plan.created_workspaces {
                assert_eq!(workspaces.create_workspace(), id);
            }
            outputs = plan.outputs;
            outputs.reconcile_workspaces(&mut workspaces, &Default::default());
            let window = WindowId(step);
            workspaces
                .insert_window(window, Axis::Horizontal, 0.5)
                .unwrap();
            windows.insert(window);
            if let Some((output, _)) = inventory.last() {
                outputs.focus_output(*output).unwrap();
                let target = outputs.active_workspace(*output).unwrap();
                workspaces
                    .move_window_to_workspace(window, target, Axis::Horizontal, 0.5)
                    .unwrap();
                workspaces.activate(target).unwrap();
            }
            assert!(outputs.validate());
            workspaces.validate().unwrap();
            let actual = workspaces
                .iter()
                .flat_map(|workspace| {
                    workspace
                        .layout
                        .window_ids()
                        .chain(workspace.floating.iter().copied())
                })
                .collect::<Vec<_>>();
            assert_eq!(
                actual.len(),
                windows.len(),
                "duplicate or lost window at {step}"
            );
            assert_eq!(
                actual.into_iter().collect::<std::collections::HashSet<_>>(),
                windows
            );
            for workspace in workspaces.iter().filter(|workspace| !workspace.is_empty()) {
                assert_eq!(
                    outputs.output_for_workspace(workspace.id).is_some(),
                    !inventory.is_empty()
                );
            }
        }
    }

    #[test]
    fn reconnect_cannot_reclaim_the_donor_monitors_only_workspace() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1920), WorkspaceId(2))
            .unwrap();
        outputs.focus_output(OutputId(2)).unwrap();
        outputs.disconnect(OutputId(2)).unwrap();
        assert!(outputs.forget_workspace(WorkspaceId(1)));

        outputs
            .connect(OutputId(2), geometry(1920), WorkspaceId(3))
            .unwrap();

        assert_eq!(outputs.active_workspace(OutputId(1)), Some(WorkspaceId(2)));
        assert_eq!(outputs.active_workspace(OutputId(2)), Some(WorkspaceId(3)));
        assert!(outputs.validate());
    }

    #[test]
    fn forgetting_an_inactive_workspace_prevents_hotplug_resurrection() {
        let mut outputs = super::OutputWorkspaceMap::default();
        let display = super::OutputId(1);
        let geometry = super::OutputGeometry::new(0, 0, 1920, 1080);
        let first = super::WorkspaceId(1);
        let second = super::WorkspaceId(2);
        outputs.connect(display, geometry, first).unwrap();
        assert!(!outputs.forget_workspace(first));
        outputs.switch_workspace(display, second).unwrap();
        assert!(outputs.forget_workspace(first));
        assert_eq!(outputs.output_for_workspace(first), None);
        outputs.disconnect(display).unwrap();
        outputs
            .connect(display, geometry, super::WorkspaceId(3))
            .unwrap();
        assert_eq!(outputs.active_workspace(display), Some(second));
        assert_eq!(outputs.output_for_workspace(first), None);
        assert!(outputs.validate());
    }

    #[test]
    fn changing_output_geometry_preserves_workspace_ownership_and_focus() {
        let mut outputs = super::OutputWorkspaceMap::default();
        let id = super::OutputId(1);
        let workspace = crate::WorkspaceId(1);
        outputs
            .connect(id, super::OutputGeometry::new(0, 0, 1920, 1080), workspace)
            .unwrap();
        outputs.update_geometry(id, super::OutputGeometry::new(2000, 0, 1280, 720));
        assert_eq!(outputs.output_for_workspace(workspace), Some(id));
        assert_eq!(outputs.active_workspace(id), Some(workspace));
        assert_eq!(outputs.focused_output(), Some(id));
    }
    use super::*;

    fn geometry(x: i32) -> OutputGeometry {
        OutputGeometry::new(x, 0, 1_920, 1_080)
    }

    #[test]
    fn history_tracks_actual_switches_and_toggles_without_replacing_it_on_a_noop() {
        let mut outputs = OutputWorkspaceMap::default();
        let display = OutputId(1);
        outputs
            .connect(display, geometry(0), WorkspaceId(1))
            .unwrap();
        assert_eq!(outputs.previous_workspace(display), None);
        outputs
            .select_workspace(display, WorkspaceId(1), true)
            .unwrap();
        assert_eq!(outputs.previous_workspace(display), None);
        outputs.switch_workspace(display, WorkspaceId(2)).unwrap();
        outputs.switch_workspace(display, WorkspaceId(2)).unwrap();
        assert_eq!(outputs.previous_workspace(display), Some(WorkspaceId(1)));
        outputs
            .select_workspace(display, WorkspaceId(2), false)
            .unwrap();
        assert_eq!(outputs.active_workspace(display), Some(WorkspaceId(2)));
        for expected in [WorkspaceId(1), WorkspaceId(2), WorkspaceId(1)] {
            let current = outputs.active_workspace(display).unwrap();
            outputs.select_workspace(display, current, true).unwrap();
            assert_eq!(outputs.active_workspace(display), Some(expected));
            assert_eq!(outputs.previous_workspace(display), Some(current));
        }
        assert!(outputs.validate());
    }

    #[test]
    fn another_monitors_workspace_updates_only_its_owners_history() {
        let mut outputs = OutputWorkspaceMap::default();
        let left = OutputId(1);
        let right = OutputId(2);
        outputs.connect(left, geometry(0), WorkspaceId(1)).unwrap();
        outputs
            .connect(right, geometry(1920), WorkspaceId(2))
            .unwrap();
        outputs.switch_workspace(left, WorkspaceId(3)).unwrap();
        outputs.switch_workspace(right, WorkspaceId(4)).unwrap();
        assert_eq!(
            outputs.select_workspace(left, WorkspaceId(2), true),
            Ok(WorkspaceSwitch::FocusedExisting(right))
        );
        assert_eq!(outputs.active_workspace(left), Some(WorkspaceId(3)));
        assert_eq!(outputs.previous_workspace(left), Some(WorkspaceId(1)));
        assert_eq!(outputs.previous_workspace(right), Some(WorkspaceId(4)));
        let previous = outputs.previous_workspace(right).unwrap();
        outputs.switch_workspace(right, previous).unwrap();
        assert_eq!(outputs.active_workspace(right), Some(WorkspaceId(4)));
        assert_eq!(outputs.previous_workspace(right), Some(WorkspaceId(2)));
        assert!(outputs.validate());
    }

    #[test]
    fn reconnect_clears_history_for_workspaces_reclaimed_by_another_output() {
        let mut outputs = OutputWorkspaceMap::default();
        let left = OutputId(1);
        let right = OutputId(2);
        outputs.connect(left, geometry(0), WorkspaceId(1)).unwrap();
        outputs
            .connect(right, geometry(1920), WorkspaceId(2))
            .unwrap();
        outputs.focus_output(right).unwrap();
        outputs.disconnect(right).unwrap();
        assert_eq!(outputs.previous_workspace(left), Some(WorkspaceId(1)));
        outputs.switch_workspace(left, WorkspaceId(1)).unwrap();
        assert_eq!(outputs.previous_workspace(left), Some(WorkspaceId(2)));
        outputs
            .connect(right, geometry(1920), WorkspaceId(3))
            .unwrap();
        assert_eq!(outputs.previous_workspace(left), None);
        assert_eq!(outputs.previous_workspace(right), None);
        assert!(outputs.validate());
    }

    #[test]
    fn forgetting_a_previous_workspace_clears_its_history() {
        let mut outputs = OutputWorkspaceMap::default();
        let display = OutputId(1);
        outputs
            .connect(display, geometry(0), WorkspaceId(1))
            .unwrap();
        outputs.switch_workspace(display, WorkspaceId(2)).unwrap();
        assert!(outputs.forget_workspace(WorkspaceId(1)));
        assert_eq!(outputs.previous_workspace(display), None);
        outputs
            .select_workspace(display, WorkspaceId(2), true)
            .unwrap();
        assert_eq!(outputs.active_workspace(display), Some(WorkspaceId(2)));
        assert_eq!(
            outputs.select_workspace(OutputId(99), WorkspaceId(2), true),
            Err(OutputError::UnknownOutput(OutputId(99)))
        );
    }

    #[test]
    fn new_outputs_receive_distinct_workspaces() {
        let mut outputs = OutputWorkspaceMap::default();

        assert_eq!(
            outputs.connect(OutputId(1), geometry(0), WorkspaceId(1)),
            Ok(WorkspaceId(1))
        );
        assert_eq!(
            outputs.connect(OutputId(2), geometry(1_920), WorkspaceId(2)),
            Ok(WorkspaceId(2))
        );
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(1)),
            Some(OutputId(1))
        );
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(2)),
            Some(OutputId(2))
        );
    }

    #[test]
    fn removal_prefers_a_remaining_focused_output_without_replacing_its_workspace() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1_920), WorkspaceId(2))
            .unwrap();
        outputs.focus_output(OutputId(1)).unwrap();

        assert_eq!(outputs.disconnect(OutputId(2)), Ok(Some(OutputId(1))));
        assert_eq!(outputs.active_workspace(OutputId(1)), Some(WorkspaceId(1)));
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(2)),
            Some(OutputId(1))
        );
    }

    #[test]
    fn removing_the_focused_output_activates_its_workspace_on_the_nearest_output() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1_920), WorkspaceId(2))
            .unwrap();
        outputs
            .connect(OutputId(3), geometry(3_840), WorkspaceId(3))
            .unwrap();
        outputs.focus_output(OutputId(2)).unwrap();

        assert_eq!(outputs.disconnect(OutputId(2)), Ok(Some(OutputId(1))));
        assert_eq!(outputs.focused_output(), Some(OutputId(1)));
        assert_eq!(outputs.active_workspace(OutputId(1)), Some(WorkspaceId(2)));
    }

    #[test]
    fn reconnect_reclaims_automatically_evacuated_workspaces() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1_920), WorkspaceId(2))
            .unwrap();
        outputs.focus_output(OutputId(1)).unwrap();
        outputs.disconnect(OutputId(2)).unwrap();

        assert_eq!(
            outputs.connect(OutputId(2), geometry(1_920), WorkspaceId(3)),
            Ok(WorkspaceId(2))
        );
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(2)),
            Some(OutputId(2))
        );
        assert_eq!(outputs.active_workspace(OutputId(2)), Some(WorkspaceId(2)));
    }

    #[test]
    fn reconnect_restores_the_migration_targets_remaining_workspace() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1_920), WorkspaceId(2))
            .unwrap();
        outputs.focus_output(OutputId(2)).unwrap();
        outputs.disconnect(OutputId(2)).unwrap();
        assert_eq!(outputs.active_workspace(OutputId(1)), Some(WorkspaceId(2)));

        outputs
            .connect(OutputId(2), geometry(1_920), WorkspaceId(3))
            .unwrap();

        assert_eq!(outputs.active_workspace(OutputId(1)), Some(WorkspaceId(1)));
        assert_eq!(outputs.active_workspace(OutputId(2)), Some(WorkspaceId(2)));
    }

    #[test]
    fn using_an_evacuated_workspace_does_not_prevent_automatic_return() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1_920), WorkspaceId(2))
            .unwrap();
        outputs.focus_output(OutputId(1)).unwrap();
        outputs.disconnect(OutputId(2)).unwrap();
        outputs
            .switch_workspace(OutputId(1), WorkspaceId(2))
            .unwrap();

        assert_eq!(
            outputs.connect(OutputId(2), geometry(1_920), WorkspaceId(3)),
            Ok(WorkspaceId(2))
        );
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(2)),
            Some(OutputId(2))
        );
    }

    #[test]
    fn repeated_lid_and_monitor_handoffs_restore_both_workspace_owners() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1920), WorkspaceId(2))
            .unwrap();
        for removed in [OutputId(1), OutputId(2), OutputId(1), OutputId(2)] {
            let remaining = if removed == OutputId(1) {
                OutputId(2)
            } else {
                OutputId(1)
            };
            outputs.focus_output(removed).unwrap();
            outputs.disconnect(removed).unwrap();
            assert_eq!(
                outputs.output_for_workspace(WorkspaceId(1)),
                Some(remaining)
            );
            assert_eq!(
                outputs.output_for_workspace(WorkspaceId(2)),
                Some(remaining)
            );
            // Switching between handed-off workspaces is normal use, not an
            // explicit change of their home output.
            outputs.switch_workspace(remaining, WorkspaceId(1)).unwrap();
            outputs.switch_workspace(remaining, WorkspaceId(2)).unwrap();
            outputs
                .connect(
                    removed,
                    geometry(if removed == OutputId(1) { 0 } else { 1920 }),
                    WorkspaceId(3),
                )
                .unwrap();
            assert_eq!(
                outputs.output_for_workspace(WorkspaceId(1)),
                Some(OutputId(1))
            );
            assert_eq!(
                outputs.output_for_workspace(WorkspaceId(2)),
                Some(OutputId(2))
            );
            assert_eq!(outputs.active_workspace(OutputId(1)), Some(WorkspaceId(1)));
            assert_eq!(outputs.active_workspace(OutputId(2)), Some(WorkspaceId(2)));
        }
    }

    #[test]
    fn selecting_a_workspace_owned_by_another_output_focuses_that_output() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1_920), WorkspaceId(2))
            .unwrap();

        assert_eq!(
            outputs.switch_workspace(OutputId(1), WorkspaceId(2)),
            Ok(WorkspaceSwitch::FocusedExisting(OutputId(2)))
        );
        assert_eq!(outputs.focused_output(), Some(OutputId(2)));
    }

    #[test]
    fn assigning_an_unused_workspace_does_not_change_the_active_workspace() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();

        assert_eq!(
            outputs.assign_workspace(OutputId(1), WorkspaceId(2)),
            Ok(OutputId(1))
        );
        assert_eq!(outputs.active_workspace(OutputId(1)), Some(WorkspaceId(1)));
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(2)),
            Some(OutputId(1))
        );
    }
    #[test]
    fn two_hop_evacuation_restores_original_home() {
        let mut outputs = OutputWorkspaceMap::default();
        for id in 1..=3 {
            outputs
                .connect(OutputId(id), geometry(id as i32 * 1920), WorkspaceId(id))
                .unwrap();
        }
        outputs.focus_output(OutputId(2)).unwrap();
        outputs.disconnect(OutputId(1)).unwrap();
        outputs.disconnect(OutputId(2)).unwrap();
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(1)),
            Some(OutputId(3))
        );
        assert_eq!(outputs.home_output(WorkspaceId(1)), Some(OutputId(1)));
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(4))
            .unwrap();
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(1)),
            Some(OutputId(1))
        );
        assert!(outputs.active_workspace(OutputId(3)).is_some());
    }

    #[test]
    fn explicit_reassignment_after_evacuation_prevents_home_steal_back() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1920), WorkspaceId(2))
            .unwrap();
        outputs.disconnect(OutputId(1)).unwrap();
        outputs
            .reassign_workspace(OutputId(2), WorkspaceId(1), WorkspaceId(9))
            .unwrap();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(3))
            .unwrap();
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(1)),
            Some(OutputId(2))
        );
        assert_eq!(outputs.home_output(WorkspaceId(1)), Some(OutputId(2)));
    }

    #[test]
    fn reassigning_last_workspace_supplies_donor_fallback() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .connect(OutputId(2), geometry(1920), WorkspaceId(2))
            .unwrap();
        outputs
            .reassign_workspace(OutputId(2), WorkspaceId(1), WorkspaceId(3))
            .unwrap();
        assert_eq!(outputs.active_workspace(OutputId(1)), Some(WorkspaceId(3)));
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(1)),
            Some(OutputId(2))
        );
    }
    #[test]
    fn final_output_replacement_keeps_all_workspaces_reachable_without_changing_homes() {
        let mut outputs = OutputWorkspaceMap::default();
        outputs
            .connect(OutputId(1), geometry(0), WorkspaceId(1))
            .unwrap();
        outputs
            .assign_workspace(OutputId(1), WorkspaceId(2))
            .unwrap();
        outputs
            .switch_workspace(OutputId(1), WorkspaceId(2))
            .unwrap();
        outputs.disconnect(OutputId(1)).unwrap();
        outputs
            .connect(OutputId(3), geometry(0), WorkspaceId(3))
            .unwrap();
        assert_eq!(outputs.active_workspace(OutputId(3)), Some(WorkspaceId(2)));
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(1)),
            Some(OutputId(3))
        );
        assert_eq!(outputs.home_output(WorkspaceId(1)), Some(OutputId(1)));
        outputs
            .connect(OutputId(1), geometry(1920), WorkspaceId(4))
            .unwrap();
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(1)),
            Some(OutputId(1))
        );
        assert_eq!(
            outputs.output_for_workspace(WorkspaceId(2)),
            Some(OutputId(1))
        );
    }
}
