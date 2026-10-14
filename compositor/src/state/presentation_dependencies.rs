use super::*;
use smithay::utils::Serial;

#[derive(Clone, Copy, Debug, PartialEq)]
struct ClientCommit {
    window: WindowId,
    serial: Serial,
}

#[derive(Clone, Copy, PartialEq)]
struct ViewportCommit {
    commit: ClientCommit,
    width: (f64, f64),
}

#[derive(Clone, PartialEq)]
struct ViewportStep {
    target: ferese_layout::ViewportTarget,
    inputs: Vec<ViewportCommit>,
}

#[derive(Clone, Default, PartialEq)]
pub(super) struct ViewportDependencies {
    steps: Vec<ViewportStep>,
    waits: Vec<ClientCommit>,
    // A pathological burst must not retain an unbounded history. The fallback
    // preserves the linked wait until commit/deadline or an absolute retarget.
    overflow: bool,
}

const MAX_VIEWPORT_STEPS: usize = 32;

/// Connect moving tiles where their conservative swept bounds overlap. Such
/// tiles must hand off together: advancing one into a neighbor's
/// still-presented source would break the resize/reflow boundary. A fixed
/// boundary between independent branches merely touches and creates no edge.
/// Use solved presentation rectangles, so constraints, gaps and interrupted
/// motion participate rather than assuming structural siblings are independent.
fn tree_reflow_groups(
    motions: impl IntoIterator<Item = (WindowId, Rect, Rect, ferese_animation::RectVelocity)>,
    inner_gap: f64,
) -> HashMap<WindowId, usize> {
    let margin = inner_gap.max(0.0) / 2.0;
    let moving = motions
        .into_iter()
        .filter(|(_, source, target, _)| source != target)
        .map(|(id, source, target, velocity)| {
            let left = source.x.min(target.x) - margin;
            let top = source.y.min(target.y) - margin;
            // Geometry uses one no-crossing spring for its components. With
            // proportional velocities an edge shares their progress, including
            // a fixed split boundary. Interrupted components can have different
            // progress: bound each separately rather than miss an intermediate
            // excursion of x+width or y+height beyond both endpoint rectangles.
            let edge =
                |position: f64, size: f64, target_position: f64, target_size: f64, speed: f64, size_speed: f64| {
                    if (speed * (target_size - size) - size_speed * (target_position - position)).abs() < 1e-6 {
                        (position + size).max(target_position + target_size)
                    } else {
                        position.max(target_position) + size.max(target_size)
                    }
                };
            let right = edge(
                source.x,
                source.width,
                target.x,
                target.width,
                velocity.x,
                velocity.width,
            ) + margin;
            let bottom = edge(
                source.y,
                source.height,
                target.y,
                target.height,
                velocity.y,
                velocity.height,
            ) + margin;
            (id, Rect::new(left, top, right - left, bottom - top))
        })
        .collect::<Vec<_>>();
    let mut groups = HashMap::new();
    for (id, _) in &moving {
        if groups.contains_key(id) {
            continue;
        }
        let group = groups.len();
        groups.insert(*id, group);
        let mut frontier = vec![*id];
        while let Some(id) = frontier.pop() {
            let bounds = moving.iter().find(|(window, _)| *window == id).unwrap().1;
            for (other, candidate) in &moving {
                if !groups.contains_key(other)
                    && bounds.x.max(candidate.x) + 0.001 < (bounds.x + bounds.width).min(candidate.x + candidate.width)
                    && bounds.y.max(candidate.y) + 0.001
                        < (bounds.y + bounds.height).min(candidate.y + candidate.height)
                {
                    groups.insert(*other, group);
                    frontier.push(*other);
                }
            }
        }
    }
    groups
}

/// The buffer-dependent properties in this path. Translation has its own
/// inputs; it does not inherit a tile's resize/reflow wait by membership.
#[derive(Default)]
pub(super) struct PresentationDependencies {
    reflow: HashMap<WindowId, Vec<ClientCommit>>,
    // Provenance and configure serials only; ViewportPresentation owns holds.
    viewport: HashMap<WorkspaceId, ViewportDependencies>,
    // A reflow release tick consumes waiting wall time without stepping springs.
    paused_reflow: HashSet<WindowId>,
}

impl PresentationDependencies {
    pub(super) fn save_viewport(&self, workspace: WorkspaceId) -> Option<ViewportDependencies> {
        self.viewport.get(&workspace).cloned()
    }

    pub(super) fn restore_viewport(&mut self, workspace: WorkspaceId, saved: Option<ViewportDependencies>) {
        if let Some(saved) = saved {
            self.viewport.insert(workspace, saved);
        } else {
            self.viewport.remove(&workspace);
        }
    }

    #[cfg(test)]
    pub(super) fn wait_for_viewport(&mut self, workspace: WorkspaceId, id: WindowId, serial: Serial) {
        self.viewport.insert(
            workspace,
            ViewportDependencies {
                waits: vec![ClientCommit { window: id, serial }],
                ..Default::default()
            },
        );
    }

    pub(super) fn needs_tick(&self) -> bool {
        !self.paused_reflow.is_empty()
    }

    fn pending<W: std::hash::Hash + Eq + Clone>(
        wait: ClientCommit,
        windows: &window_registry::WindowRegistry<W>,
    ) -> bool {
        windows
            .transaction(&wait.window)
            .is_some_and(|transaction| transaction.serial() == wait.serial)
    }

    pub(super) fn reflow_blocked<W: std::hash::Hash + Eq + Clone>(
        &self,
        id: WindowId,
        windows: &window_registry::WindowRegistry<W>,
    ) -> bool {
        self.reflow
            .get(&id)
            .is_some_and(|waits| waits.iter().any(|wait| Self::pending(*wait, windows)))
    }

    pub(super) fn viewport_blocked<W: std::hash::Hash + Eq + Clone>(
        &self,
        workspace: WorkspaceId,
        windows: &window_registry::WindowRegistry<W>,
    ) -> bool {
        self.viewport
            .get(&workspace)
            .is_some_and(|waits| waits.waits.iter().any(|wait| Self::pending(*wait, windows)))
    }

    pub(super) fn holds<W: std::hash::Hash + Eq + Clone>(
        &mut self,
        windows: &window_registry::WindowRegistry<W>,
    ) -> HashSet<WindowId> {
        let reflow = self
            .reflow
            .keys()
            .copied()
            .filter(|id| self.reflow_blocked(*id, windows))
            .collect::<HashSet<_>>();
        let mut held_reflow = std::mem::replace(&mut self.paused_reflow, reflow.clone());
        held_reflow.extend(reflow);
        self.reflow.retain(|_, waits| {
            waits.retain(|wait| Self::pending(*wait, windows));
            !waits.is_empty()
        });
        self.viewport.retain(|_, waits| {
            waits.waits.retain(|wait| Self::pending(*wait, windows));
            for step in &mut waits.steps {
                step.inputs.retain(|input| Self::pending(input.commit, windows));
            }
            if waits.steps.iter().all(|step| step.inputs.is_empty()) {
                waits.steps.clear();
            }
            !waits.steps.is_empty() || !waits.waits.is_empty()
        });
        held_reflow
    }
}

fn viewport_dependencies(
    layout: &ferese_layout::ScrollingLayout,
    pending: &[(WindowId, crate::resize_transaction::ResizeTransaction)],
    mut previous: ViewportDependencies,
) -> ViewportDependencies {
    let Some(target) = layout.viewport_target() else {
        return ViewportDependencies::default();
    };
    let live = |input: &ViewportCommit| {
        pending
            .iter()
            .any(|(window, transaction)| *window == input.commit.window && transaction.serial() == input.commit.serial)
    };
    for step in &mut previous.steps {
        for input in &mut step.inputs {
            if let Some((_, transaction)) = pending.iter().find(|(window, transaction)| {
                *window == input.commit.window && transaction.column_width() == Some(input.width)
            }) {
                input.commit.serial = transaction.serial();
            }
        }
        step.inputs.retain(live);
    }
    // Fold any prefix with no current configure inputs to the numeric start
    // already stored in the next request. No obsolete serial owns a recipe.
    if let Some(first) = previous.steps.iter().position(|step| !step.inputs.is_empty()) {
        previous.steps.drain(..first);
    } else {
        previous.steps.clear();
        previous.overflow = false;
    }
    let inputs = pending
        .iter()
        .filter_map(|(window, transaction)| {
            let width = transaction.column_width()?;
            transaction.width_changes().then_some(ViewportCommit {
                commit: ClientCommit {
                    window: *window,
                    serial: transaction.serial(),
                },
                width,
            })
        })
        .collect::<Vec<_>>();
    if previous.steps.last().is_none_or(|step| step.target != *target) {
        if !target.uses_previous_target() {
            previous.steps.clear();
            previous.overflow = false;
        }
        if previous.steps.len() < MAX_VIEWPORT_STEPS {
            previous.steps.push(ViewportStep {
                target: target.clone(),
                inputs: inputs.clone(),
            });
        } else {
            previous.overflow = true;
        }
    } else if let Some(step) = previous.steps.last_mut() {
        // A replacement binds this request to its latest configure, including
        // same-width height changes. Older recipe inputs retire above.
        step.inputs = inputs.clone();
    }
    let mut owners = previous
        .steps
        .iter()
        .flat_map(|step| step.inputs.iter().map(|input| input.commit))
        .collect::<Vec<_>>();
    owners.extend(inputs.iter().map(|input| input.commit));
    owners.sort_by_key(|owner| owner.window.0);
    owners.dedup();
    let evaluate = |only: Option<ClientCommit>, exclude: Option<ClientCommit>, retained: Option<&HashSet<WindowId>>| {
        let mut start = None;
        for step in &previous.steps {
            let widths = step
                .inputs
                .iter()
                .filter(|input| {
                    only.is_none_or(|only| input.commit == only)
                        && exclude != Some(input.commit)
                        && retained.is_none_or(|owners| owners.contains(&input.commit.window))
                })
                .map(|input| (input.commit.window, input.width.0, input.width.1))
                .collect::<Vec<_>>();
            start = Some(step.target.with_held_widths(&widths, start));
        }
        start.unwrap_or(layout.viewport_x())
    };
    let held = evaluate(None, None, None);
    previous.waits = owners
        .iter()
        .copied()
        .filter(|owner| {
            previous.overflow
                || (evaluate(Some(*owner), None, None) - layout.viewport_x()).abs() > 0.001
                || (held - evaluate(None, Some(*owner), None)).abs() > 0.001
        })
        .collect();

    // Page packing is nonlinear: all held widths can change the page even
    // when neither a single input nor removing one input exposes the change.
    // Retain a collective wait in that case, removing inputs that are not
    // needed to keep the held target different from the requested target.
    if previous.waits.is_empty() && (held - layout.viewport_x()).abs() > 0.001 {
        let mut collective = owners.iter().map(|owner| owner.window).collect::<HashSet<_>>();
        for owner in &owners {
            collective.remove(&owner.window);
            if (evaluate(None, None, Some(&collective)) - layout.viewport_x()).abs() <= 0.001 {
                collective.insert(owner.window);
            }
        }

        previous.waits = owners
            .into_iter()
            .filter(|owner| collective.contains(&owner.window))
            .collect();
    }

    // Keep a bounded recipe while any of its inputs is pending: a partial
    // commit can expose a page-packing dependency previously masked by others.
    if previous.steps.iter().all(|step| step.inputs.is_empty()) {
        return ViewportDependencies::default();
    }
    previous
}

impl Ferese {
    pub(super) fn release_presentation_dependencies(&mut self) {
        let paused_reflow = self.presentation_dependencies.paused_reflow.clone();
        self.rebuild_presentation_dependencies_with_release(true);
        self.presentation_dependencies.paused_reflow.extend(paused_reflow);
    }

    pub(super) fn rebuild_presentation_dependencies(&mut self) {
        self.rebuild_presentation_dependencies_with_release(false);
    }

    fn rebuild_presentation_dependencies_with_release(&mut self, release_tick: bool) {
        let mut reflow = HashMap::new();
        let mut viewport = HashMap::new();
        let mut previous_viewport = std::mem::take(&mut self.presentation_dependencies.viewport);
        let tree_groups = self
            .workspaces
            .iter()
            .filter_map(|workspace| {
                let WorkspaceLayout::Tree(tree) = &workspace.layout else {
                    return None;
                };
                let motions = tree.window_ids().filter_map(|id| {
                    let record = self.windows.record(id)?;
                    // Output-owned geometry does not participate in tiled reflow.
                    if workspace.fullscreen == Some(id) || record.maximized {
                        return None;
                    }
                    let geometry = record.geometry?;
                    Some((
                        id,
                        geometry.visual.current,
                        geometry.visual.target,
                        geometry.visual.velocity,
                    ))
                });
                Some((workspace.id, tree_reflow_groups(motions, self.gap_config.inner)))
            })
            .collect::<HashMap<_, _>>();
        for (&id, _) in self.windows.records() {
            let Some(workspace_id) = self.workspaces.workspace_for_window(id) else {
                continue;
            };
            let Some(workspace) = self.workspaces.workspace(workspace_id) else {
                continue;
            };
            let own_column = match &workspace.layout {
                WorkspaceLayout::Scrolling(layout) => {
                    layout.columns().iter().position(|column| column.windows.contains(&id))
                }
                WorkspaceLayout::Tree(_) => None,
            };
            let mut waits = Vec::new();
            for slow in self.windows.resizing() {
                if self.workspaces.workspace_for_window(*slow) != Some(workspace_id) {
                    continue;
                }
                let transaction = *self.windows.transaction(slow).unwrap();
                let commit = ClientCommit {
                    window: *slow,
                    serial: transaction.serial(),
                };
                // Fullscreen bounds come from the output, not a column. Its
                // own configure still gates presentation; tiled reflow waits
                // apply again when the window returns to normal geometry.
                let linked = id == *slow
                    || (workspace.fullscreen != Some(id)
                        && match &workspace.layout {
                            WorkspaceLayout::Scrolling(layout) => own_column.is_some_and(|own| {
                                layout
                                    .columns()
                                    .iter()
                                    .position(|column| column.windows.contains(slow))
                                    .is_some_and(|source| {
                                        source == own || (source < own && transaction.width_changes())
                                    })
                            }),
                            WorkspaceLayout::Tree(_) => tree_groups.get(&workspace_id).is_some_and(|groups| {
                                groups.get(&id).is_some_and(|group| groups.get(slow) == Some(group))
                            }),
                        });
                if linked {
                    waits.push(commit);
                }
            }
            if !waits.is_empty() {
                reflow.insert(id, waits);
            }
        }
        for workspace in self.workspaces.iter() {
            let layout = self
                .focus_swipe()
                .filter(|swipe| swipe.workspace == workspace.id)
                .and_then(|swipe| swipe.layout.as_ref())
                .or(match &workspace.layout {
                    WorkspaceLayout::Scrolling(layout) => Some(layout),
                    _ => None,
                });
            let Some(layout) = layout else { continue };
            let waits = viewport_dependencies(
                layout,
                &self
                    .windows
                    .resizing()
                    .filter(|id| {
                        self.workspaces.workspace_for_window(**id) == Some(workspace.id) && layout.contains(**id)
                    })
                    .map(|id| (*id, *self.windows.transaction(id).unwrap()))
                    .collect::<Vec<_>>(),
                previous_viewport.remove(&workspace.id).unwrap_or_default(),
            );
            if !waits.steps.is_empty() || !waits.waits.is_empty() {
                viewport.insert(workspace.id, waits);
            }
        }
        // A retarget uses the new dependency set immediately. A dependency that
        // disappeared must not freeze a newly independent target for one frame.
        self.presentation_dependencies
            .paused_reflow
            .retain(|id| reflow.contains_key(id));
        self.presentation_dependencies.reflow = reflow;
        self.presentation_dependencies.viewport = viewport;
        for (workspace, presentation) in &mut self.viewports {
            presentation.reconcile_hold(
                self.presentation_dependencies
                    .viewport_blocked(*workspace, &self.windows),
                release_tick,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resize_transaction::ResizeTransaction;

    fn tree_reflow_groups(
        motions: impl IntoIterator<Item = (WindowId, Rect, Rect)>,
        inner_gap: f64,
    ) -> HashMap<WindowId, usize> {
        super::tree_reflow_groups(
            motions
                .into_iter()
                .map(|(id, source, target)| (id, source, target, Default::default())),
            inner_gap,
        )
    }

    #[test]
    fn interrupted_components_use_conservative_edge_bounds() {
        let source = Rect::new(0.0, 0.0, 100.0, 100.0);
        let target = Rect::new(40.0, 0.0, 60.0, 100.0);
        let neighbor = Rect::new(110.0, 0.0, 100.0, 100.0);
        let moving_neighbor = Rect::new(110.0, 0.0, 110.0, 100.0);
        let groups = super::tree_reflow_groups(
            [
                (
                    WindowId(1),
                    source,
                    target,
                    ferese_animation::RectVelocity {
                        x: 100.0,
                        ..Default::default()
                    },
                ),
                (WindowId(2), neighbor, moving_neighbor, Default::default()),
            ],
            0.0,
        );
        assert_eq!(groups[&WindowId(1)], groups[&WindowId(2)]);
        let groups = tree_reflow_groups(
            [(WindowId(1), source, target), (WindowId(2), neighbor, moving_neighbor)],
            0.0,
        );
        assert_ne!(
            groups[&WindowId(1)],
            groups[&WindowId(2)],
            "coherent fixed edge remains independent"
        );
    }

    fn nested_tree() -> ferese_layout::LayoutTree {
        let mut tree = ferese_layout::LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5).unwrap();
        tree.insert(WindowId(3), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Vertical, 0.5)
            .unwrap();
        tree.insert(WindowId(4), Some(WindowId(3)), Axis::Vertical, 0.5)
            .unwrap();
        tree
    }

    #[test]
    fn solved_tree_motion_links_shared_boundaries_but_not_independent_branches() {
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        for gap in [0.0, 10.0, 80.0] {
            let gaps = GapConfig {
                inner: gap,
                outer: 10.0,
                smart: false,
            };
            let mut tree = nested_tree();
            let solve = |tree: &ferese_layout::LayoutTree, constraints: &HashMap<WindowId, SizeConstraints>| {
                tree.geometry_with_constraints(bounds, gaps, constraints, Some(WindowId(1)))
                    .unwrap()
                    .geometry
            };
            let constraints = HashMap::new();
            let source = solve(&tree, &constraints);
            tree.resize_window(WindowId(1), Direction::Down, 0.1).unwrap();
            tree.resize_window(WindowId(3), Direction::Down, 0.1).unwrap();
            let target = solve(&tree, &constraints);
            let group = |source: &HashMap<WindowId, Rect>, target: &HashMap<WindowId, Rect>| {
                tree_reflow_groups(
                    source
                        .iter()
                        .filter_map(|(&id, &rect)| Some((id, rect, *target.get(&id)?))),
                    gap,
                )
            };
            let groups = group(&source, &target);
            assert_eq!(groups[&WindowId(1)], groups[&WindowId(2)]);
            assert_eq!(groups[&WindowId(3)], groups[&WindowId(4)]);
            assert_ne!(groups[&WindowId(1)], groups[&WindowId(3)]);

            // A horizontal request walks through the vertical child to the
            // root split: both branches' actual x/width now participate.
            tree.resize_window(WindowId(1), Direction::Right, 0.1).unwrap();
            let ancestor = solve(&tree, &constraints);
            let groups = group(&source, &ancestor);
            assert!(groups.values().all(|group| *group == groups[&WindowId(1)]));

            // A minimum in a descendant can move an ancestor boundary even
            // though no ancestor ratio was explicitly changed.
            let tree = nested_tree();
            let constrained = solve(
                &tree,
                &HashMap::from([(
                    WindowId(1),
                    SizeConstraints {
                        min_width: 500.0,
                        ..Default::default()
                    },
                )]),
            );
            let groups = group(&source, &constrained);
            assert_eq!(groups[&WindowId(1)], groups[&WindowId(3)]);
            assert_eq!(groups[&WindowId(2)], groups[&WindowId(4)]);
            assert_ne!(
                groups[&WindowId(1)],
                groups[&WindowId(2)],
                "fixed row boundary remains independent"
            );

            // Maximum constraints clip actual presented bounds. A clipped
            // leaf that does not move cannot hold the changing allocation.
            let maximum = HashMap::from([(
                WindowId(1),
                SizeConstraints {
                    max_width: Some(100.0),
                    ..Default::default()
                },
            )]);
            let clipped_source = solve(&tree, &maximum);
            let mut wider = tree.clone();
            wider.resize_window(WindowId(1), Direction::Right, 0.1).unwrap();
            let clipped_target = solve(&wider, &maximum);
            assert_eq!(clipped_source[&WindowId(1)], clipped_target[&WindowId(1)]);
            assert!(!group(&clipped_source, &clipped_target).contains_key(&WindowId(1)));

            // Retarget from the held presentation, not the previous logical
            // destination. Reversing one branch leaves the other independent.
            let mut reversed = target.clone();
            for id in [WindowId(1), WindowId(2)] {
                reversed.insert(id, source[&id]);
            }
            let groups = group(&source, &reversed);
            assert!(!groups.contains_key(&WindowId(1)));
            assert_eq!(groups[&WindowId(3)], groups[&WindowId(4)]);
        }
    }

    #[test]
    fn tree_motion_ignores_settled_and_hidden_tiles_and_links_transitive_reflow() {
        let ids = [WindowId(1), WindowId(2), WindowId(3), WindowId(4)];
        let source = ids.map(|id| Rect::new((id.0 - 1) as f64 * 100.0, 0.0, 100.0, 100.0));
        let target = [
            Rect::new(0.0, 0.0, 140.0, 100.0),
            Rect::new(140.0, 0.0, 100.0, 100.0),
            Rect::new(240.0, 0.0, 60.0, 100.0),
            source[3],
        ];
        let groups = tree_reflow_groups(
            ids.into_iter().enumerate().map(|(i, id)| (id, source[i], target[i])),
            0.0,
        );
        assert_eq!(
            groups[&ids[0]], groups[&ids[2]],
            "shared boundaries connect transitively"
        );
        assert!(!groups.contains_key(&ids[3]), "settled tile owns no reflow");

        let mut tree = nested_tree();
        tree.stack_window(WindowId(2), WindowId(1)).unwrap();
        let before = tree.geometry(Rect::new(0.0, 0.0, 800.0, 600.0)).unwrap();
        tree.resize_window(WindowId(2), Direction::Right, 0.1).unwrap();
        let after = tree.geometry(Rect::new(0.0, 0.0, 800.0, 600.0)).unwrap();
        let groups = tree_reflow_groups(
            before
                .iter()
                .filter_map(|(&id, &rect)| Some((id, rect, *after.get(&id)?))),
            0.0,
        );
        assert!(
            !groups.contains_key(&WindowId(1)),
            "inactive tab has no presented geometry"
        );
        assert_eq!(groups[&WindowId(2)], groups[&WindowId(3)]);
    }

    #[test]
    fn viewport_recipe_handles_cancellation_joint_pages_and_bounded_bursts() {
        use ferese_layout::{ColumnWidth, ScrollingLayout};
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };
        let constraints = HashMap::new();
        let a = WindowId(1);
        let b = WindowId(2);
        let c = WindowId(3);
        let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Fixed(100.0));
        layout.set_focus_strategy(ViewportFocusStrategy::Center);
        layout.insert(a, None).unwrap();
        layout.insert(b, Some(a)).unwrap();
        layout.set_column_width(a, ColumnWidth::Fixed(150.0)).unwrap();
        layout.set_column_width(b, ColumnWidth::Fixed(300.0)).unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &constraints, Some(b))
            .unwrap();
        let pending = [(
            a,
            ResizeTransaction::new(10.into(), Duration::ZERO).with_column_width(Some(100.0), Some(150.0)),
        )];
        let first = viewport_dependencies(&layout, &pending, Default::default());
        assert_eq!(first.waits.len(), 1);
        layout.slide_focus_from(b, a).unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &constraints, Some(a))
            .unwrap();
        assert_eq!(layout.viewport_x(), 50.0);
        let canceled = viewport_dependencies(&layout, &pending, first);
        assert!(
            canceled.waits.is_empty(),
            "relative target cancels pending prefix width"
        );
        let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Fixed(40.0));
        layout.set_focus_strategy(ViewportFocusStrategy::Paged);
        layout.insert(a, None).unwrap();
        layout.insert(b, Some(a)).unwrap();
        layout.insert(c, Some(b)).unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &constraints, Some(c))
            .unwrap();
        let pending = [
            (
                a,
                ResizeTransaction::new(10.into(), Duration::ZERO).with_column_width(Some(100.0), Some(40.0)),
            ),
            (
                b,
                ResizeTransaction::new(20.into(), Duration::ZERO).with_column_width(Some(100.0), Some(40.0)),
            ),
        ];
        assert!(!layout.viewport_depends_on_width(a, 100.0, 40.0));
        assert!(!layout.viewport_depends_on_width(b, 100.0, 40.0));
        let both = viewport_dependencies(&layout, &pending, Default::default());
        assert_eq!(both.waits.len(), 2, "joint held widths select another page");
        let partial = viewport_dependencies(&layout, &pending[1..], both);
        assert!(partial.waits.is_empty(), "one remaining held width fits this page");
        assert!(viewport_dependencies(&layout, &[], partial).steps.is_empty());
        // Independent absolute requests reset history. Relative request storage
        // is capped even if a client keeps replacing its configure.
        layout.set_focus_strategy(ViewportFocusStrategy::Center);
        layout.set_column_width(a, ColumnWidth::Fixed(150.0)).unwrap();
        layout.set_column_width(b, ColumnWidth::Fixed(300.0)).unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &constraints, Some(b))
            .unwrap();
        let pending = [(
            a,
            ResizeTransaction::new(30.into(), Duration::ZERO).with_column_width(Some(100.0), Some(150.0)),
        )];
        let mut graph = viewport_dependencies(&layout, &pending, Default::default());
        for index in 0..80 {
            let (from, to) = if index % 2 == 0 { (b, a) } else { (a, b) };
            layout.slide_focus_from(from, to).unwrap();
            layout
                .resolve_geometry_with_constraints(bounds, gaps, &constraints, Some(to))
                .unwrap();
            graph = viewport_dependencies(&layout, &pending, graph);
            assert!(graph.steps.len() <= MAX_VIEWPORT_STEPS);
        }
        assert!(graph.overflow);
        assert!(!graph.waits.is_empty(), "bounded fallback must not free linked input");
        layout.center_window(a, bounds, gaps, &constraints).unwrap();
        graph = viewport_dependencies(&layout, &pending, graph);
        assert_eq!(graph.steps.len(), 1);
        assert!(!graph.overflow);
    }

    #[test]
    fn properties_wait_for_only_current_configures_and_consume_release_time() {
        let mut windows = window_registry::WindowRegistry::default();
        let a = WindowId(1);
        let b = WindowId(2);
        let neighbor = WindowId(3);
        windows.register("a", a);
        windows.register("b", b);
        windows.register("neighbor", neighbor);
        windows.set_transaction(a, ResizeTransaction::new(10.into(), Duration::ZERO));
        windows.set_transaction(b, ResizeTransaction::new(20.into(), Duration::from_millis(100)));
        let commit = |window, serial| ClientCommit {
            window,
            serial: Serial::from(serial),
        };
        let workspace = WorkspaceId(1);
        let mut dependencies = PresentationDependencies::default();
        dependencies.reflow.insert(a, vec![commit(a, 10)]);
        dependencies.reflow.insert(neighbor, vec![commit(a, 10), commit(b, 20)]);
        dependencies.wait_for_viewport(workspace, b, 20.into());
        assert!(dependencies.reflow_blocked(neighbor, &windows));
        assert!(!dependencies.reflow_blocked(b, &windows));
        assert!(dependencies.viewport_blocked(workspace, &windows));
        dependencies.holds(&windows);
        // A replacement cannot be released by the older committed serial.
        windows.set_transaction(b, ResizeTransaction::new(21.into(), Duration::from_millis(150)));
        assert!(!windows.transaction(&b).unwrap().accepts(Some(20.into())));
        assert!(!dependencies.viewport_blocked(workspace, &windows));
        dependencies.wait_for_viewport(workspace, b, 21.into());
        dependencies.reflow.insert(neighbor, vec![commit(a, 10), commit(b, 21)]);
        windows.expire_transactions(Duration::from_millis(300));
        assert!(windows.transaction(&a).is_none());
        assert!(windows.transaction(&b).is_some());
        assert!(dependencies.reflow_blocked(neighbor, &windows));
        let held = dependencies.holds(&windows);
        assert!(held.contains(&a), "release tick must not charge waiting time");
        let held = dependencies.holds(&windows);
        assert!(!held.contains(&a));
        // Unmap removes the serial owner; both properties retire on the tick.
        windows.remove(&"b");
        assert!(!dependencies.viewport_blocked(workspace, &windows));
        dependencies.holds(&windows);
        dependencies.holds(&windows);
        assert!(!dependencies.needs_tick());
        assert!(dependencies.reflow.is_empty() && dependencies.viewport.is_empty());
    }

    #[test]
    fn collective_page_waits_survive_redundant_pending_widths() {
        use ferese_layout::{ColumnWidth, ScrollingLayout};
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        let gaps = GapConfig {
            inner: 0.0,
            outer: 0.0,
            smart: false,
        };
        let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Fixed(40.0));
        layout.set_focus_strategy(ViewportFocusStrategy::Paged);
        let ids = [
            WindowId(1),
            WindowId(2),
            WindowId(3),
            WindowId(4),
            WindowId(5),
            WindowId(6),
        ];
        for (i, id) in ids.iter().enumerate() {
            layout.insert(*id, i.checked_sub(1).map(|j| ids[j])).unwrap();
        }

        for (id, width) in ids.into_iter().zip([40.0, 40.0, 80.0, 80.0, 40.0, 40.0]) {
            layout.set_column_width(id, ColumnWidth::Fixed(width)).unwrap();
        }

        layout.focus(ids[4]).unwrap();
        layout
            .resolve_geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(ids[4]))
            .unwrap();
        assert_eq!(layout.viewport_x(), 160.0);
        let widths = [
            (ids[0], 120.0, 40.0),
            (ids[1], 120.0, 40.0),
            (ids[4], 80.0, 40.0),
            (ids[5], 160.0, 40.0),
        ];
        let pending = widths
            .into_iter()
            .enumerate()
            .map(|(i, (id, source, target))| {
                (
                    id,
                    ResizeTransaction::new((i as u32 + 10).into(), Duration::ZERO)
                        .with_column_width(Some(source), Some(target)),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(layout.viewport_target().unwrap().with_held_widths(&widths, None), 320.0);
        let graph = viewport_dependencies(&layout, &pending, Default::default());
        assert!(
            !graph.waits.is_empty(),
            "the collectively different page needs a commit wait"
        );
        assert!(
            !graph.waits.iter().any(|wait| wait.window == ids[5]),
            "a column after focus cannot own this page wait"
        );
        let reversed = pending.iter().copied().rev().collect::<Vec<_>>();
        assert_eq!(
            viewport_dependencies(&layout, &reversed, Default::default()).waits,
            graph.waits
        );

        // Exercise every partial-commit order, including a redundant owner
        // committing before an owner in the retained collective group.
        for mask in 0..1 << pending.len() {
            let remaining = pending
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, input)| *input)
                .collect::<Vec<_>>();
            let held_widths = remaining
                .iter()
                .map(|(id, transaction)| {
                    let (source, target) = transaction.column_width().unwrap();
                    (*id, source, target)
                })
                .collect::<Vec<_>>();
            let held = layout.viewport_target().unwrap().with_held_widths(&held_widths, None);
            let partial = viewport_dependencies(&layout, &remaining, graph.clone());
            if (held - layout.viewport_x()).abs() > 0.001 {
                assert!(
                    !partial.waits.is_empty(),
                    "collective wait lost after partial commits: {mask}"
                );
            }

            assert!(
                !partial.waits.iter().any(|wait| wait.window == ids[5]),
                "unrelated column blocked mask {mask}"
            );
            if remaining.is_empty() {
                assert!(partial.steps.is_empty() && partial.waits.is_empty());
            }
            // Feed each precise partial-commit result through the presentation
            // owner. Remaining unrelated configures must not keep it held.
            let mut windows = window_registry::WindowRegistry::default();
            for id in ids {
                windows.register(id, id);
            }
            for (id, transaction) in &remaining {
                windows.set_transaction(*id, *transaction);
            }
            let workspace = WorkspaceId(1);
            let mut dependencies = PresentationDependencies::default();
            dependencies.viewport.insert(workspace, partial);
            let blocked = dependencies.viewport_blocked(workspace, &windows);
            let mut viewport = ViewportPresentation::new(0.0);
            viewport.retarget(layout.viewport_x(), true);
            viewport.reconcile_hold(true, false);
            viewport.reconcile_hold(blocked, true);
            viewport.advance(Duration::from_secs(3), SpringConfig::default(), true, blocked);
            assert_eq!(viewport.motion().current, 0.0, "release consumed waiting time: {mask}");
            viewport.advance(Duration::from_millis(16), SpringConfig::default(), true, blocked);
            assert_eq!(
                viewport.motion().current == 0.0,
                blocked,
                "incorrect held owner: {mask}"
            );
        }
    }
}
