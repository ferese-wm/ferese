use super::*;

impl Ferese {
    pub(super) fn visible_workspace_ids(&self) -> HashSet<WorkspaceId> {
        let mut visible = self
            .output_workspaces
            .connected_outputs()
            .filter_map(|output| self.output_workspaces.active_workspace(output))
            .collect::<HashSet<_>>();
        visible.extend(
            self.workspace_slides
                .values()
                .flat_map(|slide| slide.items.iter().map(|item| item.workspace)),
        );
        visible
    }

    pub(super) fn reconcile_workspaces(&mut self, visible: &HashSet<WorkspaceId>) {
        for workspace in self
            .output_workspaces
            .reconcile_workspaces(&mut self.workspaces, visible)
        {
            self.viewports.remove(&workspace);
        }
    }

    pub(super) fn unmap_invisible_windows(&mut self, visible: &HashSet<WindowId>) -> bool {
        let mut changed = false;
        let windows = self
            .windows
            .ids()
            .iter()
            .map(|(window, id)| (window.clone(), *id))
            .collect::<Vec<_>>();

        for (window, id) in windows {
            if visible.contains(&id) {
                continue;
            }

            let was_mapped = self.space.element_location(&window).is_some();
            changed |= was_mapped;
            if was_mapped && let Some(geometry) = self.windows.geometry_mut(&id) {
                geometry.settle_presentation();
            }

            self.space.unmap_elem(&window);
        }
        self.refresh_idle_inhibition();

        changed
    }

    pub fn relayout(&mut self) {
        self.relayout_outputs(None, true);
    }

    pub(crate) fn relayout_window(&mut self, id: WindowId) {
        let outputs = self
            .space
            .outputs()
            .filter(|output| self.window_belongs_to_output(id, output))
            .cloned()
            .collect::<Vec<_>>();
        self.relayout_on(&outputs);
    }

    pub(crate) fn relayout_on(&mut self, outputs: &[Output]) {
        self.relayout_outputs(Some(outputs.to_vec()), true);
    }

    pub(super) fn relayout_desktop(&mut self) {
        self.relayout_outputs(None, false);
    }

    fn relayout_outputs(&mut self, mut redraw_outputs: Option<Vec<Output>>, finalize: bool) {
        if self
            .desktop_transition
            .as_ref()
            .is_some_and(|transition| !transition.publishing)
        {
            return;
        }
        if self
            .direct_backend
            .as_ref()
            .is_some_and(|backend| backend.reconciling())
        {
            return;
        }

        if let Some(outputs) = redraw_outputs.as_mut() {
            outputs.extend(
                self.space
                    .outputs()
                    .filter(|output| {
                        self.output_has_animations(output) || self.output_has_pending_visual_changes(output)
                    })
                    .cloned(),
            );
        }

        self.refresh_input_capture_zones();
        let visible_workspaces = self.visible_workspace_ids();
        self.reconcile_workspaces(&visible_workspaces);
        if self.session_lock.active() {
            self.configure_lock_surfaces();
        }
        // Consume idle time before setting new targets; it must not become a
        // large first animation step after a keypress on an idle desktop.
        self.advance_animations(Instant::now());
        self.stacking_cache.invalidate();
        self.arrange_layers();
        let source_column_widths = self
            .windows
            .records()
            .filter_map(|(&id, record)| {
                let layout = &self
                    .workspaces
                    .workspace(self.workspaces.workspace_for_window(id)?)?
                    .layout;
                let WorkspaceLayout::Scrolling(layout) = layout else {
                    return None;
                };
                Some((
                    id,
                    record
                        .resize
                        .and_then(|transaction| transaction.column_width().map(|(source, _)| source))
                        .or_else(|| {
                            // A sibling may have committed while this column is still
                            // held. Replacement configures must share that cohort's
                            // presented source, rather than its latest allocated width.
                            layout
                                .columns()
                                .iter()
                                .find(|column| column.windows.contains(&id))
                                .and_then(|column| {
                                    column.windows.iter().find_map(|sibling| {
                                        self.windows.transaction(sibling).and_then(|transaction| {
                                            transaction.column_width().map(|(source, _)| source)
                                        })
                                    })
                                })
                        })
                        .or_else(|| layout.allocated_column_width(id))?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let mut previous_scrolling_world_x = self.windows.take_world_positions();
        let pending_column_width_cycles = self.windows.take_column_width_requests();
        let outputs = self.space.outputs().cloned().collect::<Vec<_>>();
        let constraints = self.window_constraints();
        let mut visible = HashSet::new();
        let mut placements = Vec::new();
        let mut visible_workspace_pairs = Vec::new();

        for output in outputs {
            let Some(output_id) = self.output_ids.get(&output).copied() else {
                continue;
            };

            let Some(active_workspace) = self.output_workspaces.active_workspace(output_id) else {
                continue;
            };
            visible_workspace_pairs.push((output.clone(), active_workspace));
            if let Some(slide) = self.workspace_slides.get(&output_id) {
                for item in &slide.items {
                    if item.workspace != active_workspace {
                        visible_workspace_pairs.push((output.clone(), item.workspace));
                    }
                }
            }
        }

        for (output, workspace_id) in visible_workspace_pairs {
            let Some(bounds) = self.output_bounds_for(&output) else {
                continue;
            };

            let fullscreen_bounds = self.full_output_bounds_for(&output).unwrap_or(bounds);
            for layer in layer_map_for_output(&output).layers() {
                crate::handlers::set_surface_tree_output(layer.wl_surface(), &output);
            }

            let Some(workspace) = self.workspaces.workspace(workspace_id) else {
                continue;
            };

            let workspace_fullscreen = workspace.fullscreen;
            let is_scrolling_layout = matches!(workspace.layout, WorkspaceLayout::Scrolling(_));
            let workspace_focus = workspace.last_focused;
            let focused = self
                .focused_window
                .filter(|window| self.workspaces.workspace_for_window(*window) == Some(workspace_id))
                .or(workspace_focus);
            let Some(workspace) = self.workspaces.workspace_mut(workspace_id) else {
                continue;
            };

            let layout =
                match workspace
                    .layout
                    .resolve_geometry_with_constraints(bounds, self.gap_config, &constraints, focused)
                {
                    Ok(layout) => layout,
                    Err(error) => {
                        tracing::error!(%error, ?workspace_id, "failed to compute tiled geometry");
                        continue;
                    }
                };
            let viewport_target = workspace.layout.viewport_x();
            let viewport_motion = viewport_target.map(|target| {
                let viewport = self
                    .viewports
                    .entry(workspace_id)
                    .or_insert_with(|| ViewportPresentation::new(target));
                let target_changed = viewport.retarget(target, self.animations_enabled);
                (viewport.motion().current, target_changed)
            });
            let viewport_current = viewport_motion.map(|(current, _)| current);
            let viewport_target_changed = viewport_motion.is_some_and(|(_, changed)| changed);

            for warning in layout.warnings {
                tracing::warn!(
                    window = ?warning.window,
                    kind = ?warning.kind,
                    requested = warning.requested,
                    assigned = warning.assigned,
                    "window size constraint could not be satisfied exactly"
                );
            }

            for (window, id) in self.windows.ids() {
                if self.workspaces.workspace_for_window(*id) != Some(workspace_id) {
                    continue;
                }

                if let Some(toplevel) = window.toplevel() {
                    crate::handlers::set_surface_tree_output(toplevel.wl_surface(), &output);
                }

                let is_maximized =
                    self.windows.record(*id).is_some_and(|w| w.maximized) && workspace_fullscreen != Some(*id);
                let placement = self.workspaces.placement(*id);
                let is_floating = matches!(placement, Some(WindowPlacement::Floating { .. }));
                let rect = if workspace_fullscreen == Some(*id) {
                    fullscreen_bounds
                } else if is_maximized && (!is_scrolling_layout || is_floating) {
                    maximized_rect(bounds, self.gap_config.outer)
                } else {
                    match placement {
                        Some(WindowPlacement::Tiled) => {
                            let Some(rect) = layout.geometry.get(id).copied() else {
                                continue;
                            };
                            rect
                        }
                        Some(WindowPlacement::Floating { rect }) => {
                            let constrained =
                                constrained_floating_rect(rect, constraints.get(id).copied().unwrap_or_default());
                            if constrained != rect {
                                let _ = self.workspaces.set_floating_rect(*id, constrained);
                            }
                            constrained
                        }
                        None => continue,
                    }
                };

                let is_fullscreen = workspace_fullscreen == Some(*id);

                visible.insert(*id);
                let scrolling = if !is_fullscreen && !is_floating && (!is_maximized || is_scrolling_layout) {
                    viewport_target
                        .zip(viewport_current)
                        .map(|(target, current)| (workspace_id, rect.x + target, current))
                } else {
                    None
                };

                placements.push((
                    window.clone(),
                    *id,
                    rect,
                    is_fullscreen,
                    is_maximized,
                    is_floating,
                    scrolling,
                    pending_column_width_cycles.contains(id) && viewport_target_changed,
                ));
            }
        }

        self.unmap_invisible_windows(&visible);

        let now = self.presentation_now();

        placements.sort_by_key(|(_, id, ..)| self.window_stack.rank(*id));

        for (window, id, rect, is_fullscreen, is_maximized, is_floating, scrolling, couple_width) in placements {
            let was_mapped = self.space.element_location(&window).is_some();
            let Some(record) = self.windows.record_mut(id) else {
                continue;
            };

            // A viewport-coupled width must never override fullscreen/floating geometry.
            if scrolling.is_none() {
                record.coupled_width = None;
            }

            let natural_pending = record.natural_floating_pending && is_floating && !is_fullscreen && !is_maximized;
            let had_geometry = record.geometry.is_some();
            let geometry = record
                .geometry
                .get_or_insert_with(|| WindowGeometry::new(rect, client_size(&window)));

            let mode = if is_fullscreen {
                PresentationMode::Fullscreen
            } else if is_maximized {
                PresentationMode::Maximized
            } else {
                PresentationMode::Normal
            };
            let mut requested_size = geometry.set_presentation_mode(rect, mode, now);
            if !was_mapped && (had_geometry || is_fullscreen) {
                geometry.settle_presentation();
            }

            if !self.animations_enabled {
                geometry.advance(Duration::ZERO, self.spring_config, false);
                requested_size = geometry.presentation_size_request(now).or(requested_size);
            }

            if geometry.is_zooming() {
                record.coupled_width = None;
            }

            if let Some((workspace, world_x, viewport_x)) = scrolling {
                let restored_world_x =
                    restored_scrolling_world_x(had_geometry, geometry.visual.current.x, viewport_x, world_x);
                let mut animated_world_x = previous_scrolling_world_x
                    .remove(&id)
                    .filter(|(previous_workspace, _)| *previous_workspace == workspace)
                    .map(|(_, world_x)| world_x)
                    .unwrap_or_else(|| {
                        let mut world = AnimatedValue::new(restored_world_x);
                        world.velocity = geometry.visual.velocity.x
                            + self
                                .viewports
                                .get(&workspace)
                                .map_or(0.0, |viewport| viewport.motion().velocity);
                        world
                    });
                animated_world_x.set_target(world_x);
                if !self.animations_enabled {
                    animated_world_x.snap();
                }

                if let Some(viewport) = self.viewports.get(&workspace) {
                    super::animation::sync_scrolling_coordinates(
                        geometry,
                        &mut animated_world_x,
                        viewport.motion(),
                        geometry.is_zooming(),
                    );
                }
                record.world_x = Some((workspace, animated_world_x));

                let coupled = !geometry.is_zooming()
                    && (couple_width
                        || record
                            .coupled_width
                            .as_ref()
                            .is_some_and(|(previous_workspace, _)| *previous_workspace == workspace));
                if coupled {
                    let width = record
                        .coupled_width
                        .get_or_insert_with(|| (workspace, AnimatedValue::new(geometry.visual.current.width)));
                    if width.0 != workspace {
                        *width = (workspace, AnimatedValue::new(geometry.visual.current.width));
                    }
                    width.1.retarget_preserving_motion(rect.width);
                    if !self.animations_enabled {
                        width.1.snap();
                    }
                    geometry.visual.current.width = width.1.current;
                    geometry.visual.velocity.width = width.1.velocity;
                } else if pending_column_width_cycles.contains(&id) {
                    record.coupled_width = None;
                }
            }

            let visual = geometry.visual.current;
            if natural_pending {
                requested_size = None;
            }

            let location = (visual.x.round() as i32, visual.y.round() as i32);

            self.map_window_geometry(window.clone(), location.into());
            if let Some(toplevel) = window.toplevel() {
                let state_changed = toplevel.with_pending_state(|state| {
                    if natural_pending {
                        state.size = None;
                    } else if let Some(size) = requested_size {
                        state.size = Some((size.width, size.height).into());
                    }

                    let fullscreen_changed = if is_fullscreen {
                        state.states.set(xdg_toplevel::State::Fullscreen)
                    } else {
                        state.states.unset(xdg_toplevel::State::Fullscreen)
                    };

                    let maximized_changed = if is_maximized {
                        state.states.set(xdg_toplevel::State::Maximized)
                    } else {
                        state.states.unset(xdg_toplevel::State::Maximized)
                    };

                    let tiled = !is_floating && !is_fullscreen;
                    let tiled_changed = if tiled {
                        state.states.set(xdg_toplevel::State::TiledLeft)
                            | state.states.set(xdg_toplevel::State::TiledRight)
                            | state.states.set(xdg_toplevel::State::TiledTop)
                            | state.states.set(xdg_toplevel::State::TiledBottom)
                    } else {
                        state.states.unset(xdg_toplevel::State::TiledLeft)
                            | state.states.unset(xdg_toplevel::State::TiledRight)
                            | state.states.unset(xdg_toplevel::State::TiledTop)
                            | state.states.unset(xdg_toplevel::State::TiledBottom)
                    };

                    fullscreen_changed || maximized_changed || tiled_changed
                });

                if (natural_pending || requested_size.is_some() || state_changed)
                    && let Some(serial) = toplevel.send_pending_configure()
                    && requested_size.is_some()
                    && had_geometry
                    && self.animations_enabled
                {
                    #[cfg(feature = "resize-metrics")]
                    self.resize_metrics.begin(
                        id,
                        self.workspaces.workspace_for_window(id),
                        &self.windows.record(id).unwrap().app_id,
                        now,
                    );
                    self.windows.set_transaction(
                        id,
                        crate::resize_transaction::ResizeTransaction::new(serial, now)
                            .with_source_geometry(window.geometry())
                            .with_column_width(
                                source_column_widths.get(&id).copied(),
                                self.workspaces
                                    .workspace_for_window(id)
                                    .and_then(|workspace| self.workspaces.workspace(workspace))
                                    .and_then(|workspace| match &workspace.layout {
                                        WorkspaceLayout::Scrolling(layout) => layout.allocated_column_width(id),
                                        _ => None,
                                    }),
                            ),
                    );
                }
            }
        }

        self.rebuild_presentation_dependencies();
        self.sync_window_stacking();
        self.retarget_overview();
        if !finalize {
            return;
        }
        self.send_shell_snapshots();

        if let Some(outputs) = redraw_outputs {
            crate::backends::direct::render_on(self, &outputs);
        } else {
            crate::backends::direct::render_all(self);
        }
    }

    pub(crate) fn raise_window(&mut self, window: &Window, activate: bool) {
        if let Some(id) = self.windows.ids().get(window) {
            self.window_stack.raise(*id);
        }

        self.space.raise_element(window, activate);
        self.stacking_cache.remapped();
        self.sync_window_stacking();
    }

    pub(crate) fn invalidate_window_stacking(&mut self) {
        self.stacking_cache.invalidate();
    }

    pub(crate) fn sync_window_stacking(&mut self) {
        let revision = self.window_stack.revision();
        let rebuild = self.stacking_cache.needs_rebuild(revision, self.space.elements().len());

        if !rebuild && !self.stacking_cache.needs_restore() {
            return;
        }

        if rebuild {
            let mut windows = self.space.elements().cloned().collect::<Vec<_>>();
            windows.sort_by_key(|window| {
                let id = self.windows.ids().get(window).copied();
                let geometry = id.and_then(|id| self.windows.geometry(&id));
                let floating = id
                    .is_some_and(|id| matches!(self.workspaces.placement(id), Some(WindowPlacement::Floating { .. })));
                let priority = crate::stacking::layer_priority(
                    floating,
                    geometry.is_some_and(|geometry| geometry.is_zooming()),
                    geometry.is_some_and(|geometry| geometry.is_fullscreen()),
                    floating
                        && id.is_some_and(|id| {
                            self.floating_above_fullscreen.get(&id).is_some_and(|parent| {
                                self.workspaces
                                    .workspace_for_window(id)
                                    .and_then(|workspace| self.workspaces.workspace(workspace))
                                    .is_some_and(|workspace| workspace.fullscreen == Some(*parent))
                            })
                        }),
                );
                (priority, id.map_or(usize::MAX, |id| self.window_stack.rank(id)))
            });
            self.stacking_cache.replace(windows, revision);
        }

        if !stacking_order_settled(self.space.elements(), self.stacking_cache.order(), |window| {
            window.z_index()
        }) {
            for window in self.stacking_cache.order() {
                self.space.raise_element(window, false);
            }
        }

        self.stacking_cache.restored();
    }

    pub(crate) fn map_window_geometry(&mut self, window: Window, location: Point<i32, Logical>) {
        if self.space.element_location(&window) == Some(location) {
            return;
        }

        self.space.map_element(window, location, false);
        self.stacking_cache.remapped();
    }

    pub(super) fn tiled_layout(&mut self, bounds: Rect) -> Result<LayoutResult, ferese_layout::LayoutError> {
        let constraints = self.window_constraints();
        let focused = self.focused_window;
        self.workspaces.active_mut().layout.resolve_geometry_with_constraints(
            bounds,
            self.gap_config,
            &constraints,
            focused,
        )
    }

    pub(super) fn window_constraints(&self) -> HashMap<WindowId, SizeConstraints> {
        self.windows
            .ids()
            .iter()
            .filter_map(|(window, id)| {
                let toplevel = window.toplevel()?;
                let (minimum, maximum) = with_states(toplevel.wl_surface(), |states| {
                    let mut cached = states.cached_state.get::<SurfaceCachedState>();
                    let state = cached.current();
                    (state.min_size, state.max_size)
                });
                let (minimum, maximum) = self.effective_size_constraints(
                    window,
                    (minimum.w, minimum.h),
                    (maximum.w, maximum.h),
                );

                Some((
                    *id,
                    SizeConstraints {
                        min_width: minimum.0.max(1) as f64,
                        min_height: minimum.1.max(1) as f64,
                        max_width: (maximum.0 > 0).then_some(maximum.0 as f64),
                        max_height: (maximum.1 > 0).then_some(maximum.1 as f64),
                    },
                ))
            })
            .collect()
    }

    pub(super) fn arrange_layers(&self) {
        let outputs = self.space.outputs().cloned().collect::<Vec<_>>();

        for output in outputs {
            layer_map_for_output(&output).arrange();
        }
    }
}
