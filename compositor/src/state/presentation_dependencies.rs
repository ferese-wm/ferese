use super::*;
use smithay::utils::Serial;

#[derive(Clone, Copy, Debug, PartialEq)]
struct ClientCommit {
    window: WindowId,
    serial: Serial,
}

#[derive(Clone, Copy)]
struct ViewportCommit {
    commit: ClientCommit,
    width: (f64, f64),
}

#[derive(Clone)]
struct ViewportStep {
    target: ferese_layout::ViewportTarget,
    inputs: Vec<ViewportCommit>,
}

#[derive(Clone, Default)]
pub(super) struct ViewportDependencies {
    steps: Vec<ViewportStep>,
    waits: Vec<ClientCommit>,
    // A pathological burst must not retain an unbounded history. The fallback
    // preserves the linked wait until commit/deadline or an absolute retarget.
    overflow: bool,
}

const MAX_VIEWPORT_STEPS: usize = 32;

/// The buffer-dependent properties in this path. Translation has its own
/// inputs; it does not inherit a tile's resize/reflow wait by membership.
#[derive(Default)]
pub(super) struct PresentationDependencies {
    reflow: HashMap<WindowId, Vec<ClientCommit>>,
    viewport: HashMap<WorkspaceId, ViewportDependencies>,
    // A release tick consumes waiting wall time without stepping held springs.
    paused_reflow: HashSet<WindowId>,
    paused_viewport: HashSet<WorkspaceId>,
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
        !self.paused_reflow.is_empty() || !self.paused_viewport.is_empty()
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
    ) -> (HashSet<WindowId>, HashSet<WorkspaceId>) {
        let reflow = self
            .reflow
            .keys()
            .copied()
            .filter(|id| self.reflow_blocked(*id, windows))
            .collect::<HashSet<_>>();
        let viewport = self
            .viewport
            .keys()
            .copied()
            .filter(|workspace| self.viewport_blocked(*workspace, windows))
            .collect::<HashSet<_>>();
        let mut held_reflow = std::mem::replace(&mut self.paused_reflow, reflow.clone());
        let mut held_viewport = std::mem::replace(&mut self.paused_viewport, viewport.clone());
        held_reflow.extend(reflow);
        held_viewport.extend(viewport);
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
        (held_reflow, held_viewport)
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
        let paused_viewport = self.presentation_dependencies.paused_viewport.clone();
        self.rebuild_presentation_dependencies();
        self.presentation_dependencies.paused_reflow.extend(paused_reflow);
        self.presentation_dependencies.paused_viewport.extend(paused_viewport);
    }

    pub(super) fn rebuild_presentation_dependencies(&mut self) {
        let mut reflow = HashMap::new();
        let mut viewport = HashMap::new();
        let mut previous_viewport = std::mem::take(&mut self.presentation_dependencies.viewport);
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
                            WorkspaceLayout::Tree(_) => {
                                workspace.layout.contains(id) && workspace.layout.contains(*slow)
                            }
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
                .focus_swipe
                .as_ref()
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
        self.presentation_dependencies
            .paused_viewport
            .retain(|id| viewport.get(id).is_some_and(|waits| !waits.waits.is_empty()));
        self.presentation_dependencies.reflow = reflow;
        self.presentation_dependencies.viewport = viewport;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resize_transaction::ResizeTransaction;

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
            .geometry_with_constraints(bounds, gaps, &constraints, Some(b))
            .unwrap();
        let pending = [(
            a,
            ResizeTransaction::new(10.into(), Duration::ZERO).with_column_width(Some(100.0), Some(150.0)),
        )];
        let first = viewport_dependencies(&layout, &pending, Default::default());
        assert_eq!(first.waits.len(), 1);
        layout.slide_focus_from(b, a).unwrap();
        layout
            .geometry_with_constraints(bounds, gaps, &constraints, Some(a))
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
            .geometry_with_constraints(bounds, gaps, &constraints, Some(c))
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
            .geometry_with_constraints(bounds, gaps, &constraints, Some(b))
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
                .geometry_with_constraints(bounds, gaps, &constraints, Some(to))
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
        let (held, _) = dependencies.holds(&windows);
        assert!(held.contains(&a), "release tick must not charge waiting time");
        let (held, _) = dependencies.holds(&windows);
        assert!(!held.contains(&a));
        // Unmap removes the serial owner; both properties retire on the tick.
        windows.remove(&"b");
        assert!(!dependencies.viewport_blocked(workspace, &windows));
        let (_, held) = dependencies.holds(&windows);
        assert!(held.contains(&workspace));
        assert!(dependencies.holds(&windows).1.is_empty());
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
            .geometry_with_constraints(bounds, gaps, &HashMap::new(), Some(ids[4]))
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
        }
    }
}
