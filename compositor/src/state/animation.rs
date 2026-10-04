use super::*;

impl Ferese {
    pub(super) fn presentation_now(&self) -> Duration {
        #[cfg(test)]
        if let Some(now) = self.animation_test_time {
            return now;
        }
        self.start_time.elapsed()
    }

    pub fn advance_animations(&mut self, now: Instant) -> bool {
        #[cfg(test)]
        if let Some(time) = self.animation_test_time {
            return self.advance_animations_at(Duration::ZERO, time);
        }
        let delta = crate::presentation::frame_delta(&mut self.last_animation_tick, now);
        // Consume wall time even while presentation is paused. A paused
        // desktop must not turn suspend/DPMS time into a spring step.
        if self.space.outputs().next().is_none()
            || self.session_lock.sleeping
            || self.direct_backend.as_ref().is_some_and(|backend| !backend.active)
        {
            #[cfg(feature = "resize-metrics")]
            self.resize_metrics.pause_clock(self.start_time.elapsed());
            return self.animations_need_tick();
        }

        self.advance_animations_by(delta)
    }

    pub fn record_drm_presentation(&mut self, node: DrmNode, crtc: crtc::Handle, time: DrmEventTime, sequence: u32) {
        if let Some(backend) = self.direct_backend.as_mut() {
            backend.record_presentation(node, crtc, time, sequence);
        }
    }

    #[cfg(feature = "resize-metrics")]
    pub(super) fn measure_resize_pauses(&mut self, now: Duration) {
        if self.session_lock.sleeping || self.direct_backend.as_ref().is_some_and(|backend| !backend.active) {
            self.resize_metrics.pause_clock(now);
            return;
        }

        for id in self.windows.resizing() {
            let Some(workspace) = self.workspaces.workspace_for_window(*id) else {
                continue;
            };
            let moving = |value: &AnimatedValue| value.current != value.target || value.velocity != 0.0;
            let viewport = self
                .presentation_dependencies
                .viewport_blocked(workspace, &self.windows)
                && self.viewport_animations.get(&workspace).is_some_and(moving)
                && self
                    .output_workspaces
                    .output_for_workspace(workspace)
                    .is_some_and(|output| self.output_workspaces.active_workspace(output) == Some(workspace))
                && self
                    .focus_swipe
                    .as_ref()
                    .is_none_or(|swipe| swipe.workspace != workspace);
            let others = self.workspaces.workspace(workspace).is_some_and(|workspace| {
                workspace
                    .layout
                    .window_ids()
                    .chain(workspace.floating.iter().copied())
                    .any(|other| {
                        other != *id
                            && self.presentation_dependencies.reflow_blocked(other, &self.windows)
                            && self
                                .windows
                                .window(other)
                                .is_some_and(|window| self.space.element_location(window).is_some())
                            && self.windows.record(other).is_some_and(|record| {
                                record.geometry.is_some_and(|mut geometry| {
                                    geometry.advance(Duration::ZERO, self.spring_config, self.animations_enabled)
                                }) || record.world_x.as_ref().is_some_and(|(_, value)| moving(value))
                                    || record.coupled_width.as_ref().is_some_and(|(_, value)| moving(value))
                            })
                    })
            });
            self.resize_metrics.observe(
                *id,
                now,
                self.animations_enabled && viewport,
                self.animations_enabled && others,
            );
        }
    }

    pub(crate) fn reset_animation_clock(&mut self) {
        self.last_animation_tick = Instant::now();
        #[cfg(feature = "resize-metrics")]
        self.resize_metrics.pause_clock(self.start_time.elapsed());
    }

    fn animations_need_tick(&self) -> bool {
        if self.focus_swipe.is_some()
            || self.presentation_dependencies.needs_tick()
            || !self.workspace_slides.is_empty()
            || !self.render.closing.is_empty()
            || !self.dismissing_popups.is_empty()
            || self.render.snapshots().next().is_some()
            || self.overview.needs_tick()
        {
            return true;
        }

        let moving = |value: &AnimatedValue| value.current != value.target || value.velocity != 0.0;

        if self.viewport_animations.iter().any(|(workspace, viewport)| {
            self.output_workspaces
                .output_for_workspace(*workspace)
                .is_some_and(|output| self.output_workspaces.active_workspace(output) == Some(*workspace))
                && moving(viewport)
        }) {
            return true;
        }

        let selected = if self.overview.is_active() {
            self.overview.selected()
        } else {
            self.focused_window
        };

        self.windows.records().any(|(&id, record)| {
            let focus = if selected == Some(id) { 1.0 } else { 0.0 };
            let dim = crate::dimming::target(
                self.inactive_dim,
                self.focused_window,
                id,
                self.overview.is_presenting(),
            );

            record_needs_tick(record, focus, dim, || {
                self.windows
                    .window(id)
                    .is_some_and(|window| self.space.element_location(window).is_some())
            })
        })
    }

    pub(super) fn advance_animations_by(&mut self, delta: std::time::Duration) -> bool {
        self.advance_animations_at(delta, self.presentation_now())
    }

    // The production caller supplies monotonic wall time; tests supply a fake clock.
    pub(super) fn advance_animations_at(&mut self, delta: Duration, now: Duration) -> bool {
        #[cfg(test)]
        if self.animation_test_time.is_some() {
            self.animation_test_time = Some(now);
        }
        if !self.animations_need_tick() {
            self.sync_window_stacking();
            return false;
        }

        let delta = delta.mul_f64(self.animation_speed);
        if self
            .focus_swipe
            .as_ref()
            .is_some_and(|swipe| !self.focus_swipe_is_current(swipe))
        {
            if let Some(swipe) = self.focus_swipe.take() {
                self.presentation_dependencies
                    .restore_viewport(swipe.workspace, swipe.dependencies);
                self.release_presentation_dependencies();
            }
        }

        let mut active_animation = false;
        let mut completed_slides = Vec::new();
        for (output, slide) in &mut self.workspace_slides {
            if slide.held_progress.is_some() {
                continue;
            }

            if slide.advance(delta) {
                active_animation = true;
            } else {
                completed_slides.push(*output);
            }
        }

        self.dismissing_popups.retain_mut(|(root, popup, motion)| {
            if !smithay::utils::IsAlive::alive(popup.wl_surface()) {
                return false;
            }

            let duration = if self.animations_enabled { 140.0 } else { 0.0 };
            let active = motion.advance_visual(0.0, delta, duration);
            crate::effects::fade_dismissed_surface(popup.wl_surface(), motion.current);
            active_animation |= active;
            if !active {
                let _ = PopupManager::dismiss_popup(root, popup);
            }
            active
        });

        let dim_settings = self.inactive_dim;
        let duration = if self.animations_enabled {
            dim_settings.duration_ms
        } else {
            0.0
        };

        let mut dim_changed = false;
        // Include hidden workspace windows: overview can present them too.
        // Read selection once so these immutable fields can be borrowed alongside
        // each record without allocating a temporary window-ID vector.
        let selected_window = if self.overview.is_active() {
            self.overview.selected()
        } else {
            self.focused_window
        };

        for (&id, record) in self.windows.records_mut() {
            let selected = selected_window == Some(id);
            let target = if selected { 1.0 } else { 0.0 };
            let focus = record.focus.get_or_insert_with(|| AnimatedValue::new(target));
            let previous = focus.current;
            let changed = focus.target != target;
            focus.set_target(target);
            if self.animations_enabled {
                active_animation |= focus.advance(
                    if changed { Duration::ZERO } else { delta },
                    crate::presentation::emphasis_spring(self.spring_config),
                );
            } else {
                focus.snap();
            }
            dim_changed |= previous != focus.current;
            let shadow = record.shadow.get_or_insert_with(|| AnimatedValue::new(target));
            let previous = shadow.current;
            let changed = shadow.target != target;
            shadow.set_target(target);
            if self.animations_enabled {
                active_animation |= shadow.advance(
                    if changed { Duration::ZERO } else { delta },
                    crate::presentation::shadow_spring(self.spring_config),
                );
            } else {
                shadow.snap();
            }
            dim_changed |= previous != shadow.current;
        }

        for window in self.space.elements() {
            let Some(id) = self.windows.ids().get(window).copied() else {
                continue;
            };
            let target = crate::dimming::target(dim_settings, self.focused_window, id, self.overview.is_presenting());
            let Some(record) = self.windows.record_mut(id) else {
                continue;
            };

            let dim = record
                .dimming
                .get_or_insert_with(|| crate::dimming::DimAnimation::new(target));
            let previous = dim.current;
            active_animation |= dim.advance_visual(target, delta, duration);
            dim_changed |= previous != dim.current;
        }

        for (_, record) in self.windows.records_mut() {
            if let Some(opening) = &mut record.opening {
                let active = if self.animations_enabled {
                    opening.advance(
                        delta,
                        SpringConfig {
                            position_tolerance: 0.00001,
                            velocity_tolerance: 0.00001,
                            ..self.spring_config
                        },
                    )
                } else {
                    opening.snap();
                    false
                };
                active_animation |= active;
                if !active {
                    record.opening = None;
                }
            }
        }
        let mut finished = Vec::new();
        self.render.closing.retain_mut(|window| {
            let active =
                self.animations_enabled && !self.session_lock.active() && window.advance(delta, self.spring_config);
            if !active {
                finished.push(window.presentation.id);
            }
            active_animation |= active;
            active
        });
        for id in finished {
            tracing::debug!(?id, "released close presentation");
            self.render.remove_window(id);
        }

        // Capture holds before expiry so waiting wall time never steps reflow.
        let (mut held_reflow, mut held_viewport) = self.presentation_dependencies.holds(&self.windows);
        #[cfg(feature = "resize-metrics")]
        self.measure_resize_pauses(now);
        #[cfg(feature = "resize-metrics")]
        {
            for (&id, record) in self.windows.records() {
                if record.resize.is_some_and(|transaction| transaction.expired(now)) {
                    self.resize_metrics.end(id, now, crate::resize_metrics::End::Deadline);
                }
            }
        }
        let expired = self
            .windows
            .resizing()
            .any(|id| self.windows.transaction(id).unwrap().expired(now));
        self.windows.expire_transactions(now);
        if expired {
            self.release_presentation_dependencies();
        }

        let (released_reflow, released_viewport) = self.presentation_dependencies.holds(&self.windows);
        held_reflow.extend(released_reflow);
        held_viewport.extend(released_viewport);
        let animations_enabled = self.animations_enabled;
        let animation_speed = self.animation_speed;
        #[cfg(feature = "resize-metrics")]
        let resize_metrics = &mut self.resize_metrics;
        self.render.retain_snapshots(|id, snapshot| {
            if !animations_enabled {
                return false;
            }

            let waiting_for_client = self.presentation_dependencies.reflow_blocked(*id, &self.windows);
            // A shrinking client's destination buffer arrives before the
            // animated bounds reach it. Keep the old native pixels covering
            // that strip rather than fading them into the neutral resize fill.
            let uncovered = self.windows.geometry(id).is_some_and(|geometry| {
                geometry.client.committed_size.is_some_and(|size| {
                    crate::presentation::resize_needs_old_frame(
                        geometry.visual.current,
                        geometry.logical,
                        size.width,
                        size.height,
                    )
                })
            });
            let blocked = waiting_for_client || uncovered;
            #[cfg(feature = "resize-metrics")]
            resize_metrics.handoff_tick(*id, now, blocked);
            let active = crate::presentation::advance_handoff(
                &mut snapshot.elapsed,
                &mut snapshot.last_tick,
                now,
                blocked,
                animation_speed,
            );
            if !blocked {
                snapshot.commit.increment();
            }

            if !active {
                #[cfg(feature = "resize-metrics")]
                resize_metrics.handoff_end(*id, now, snapshot.elapsed);
                tracing::debug!(?id, bytes = snapshot.bytes(), "released resize handoff snapshot");
            }
            active
        });
        active_animation |= self.render.snapshots().next().is_some();
        // Keep scheduling frames while waiting, so the deadline cannot stall.
        active_animation |=
            self.windows.resizing().next().is_some() || !held_reflow.is_empty() || !held_viewport.is_empty();
        for (workspace, viewport) in &mut self.viewport_animations {
            if self
                .output_workspaces
                .output_for_workspace(*workspace)
                .is_none_or(|output| self.output_workspaces.active_workspace(output) != Some(*workspace))
            {
                continue;
            }

            if held_viewport.contains(workspace) {
                continue;
            }

            if let Some(swipe) = self.focus_swipe.as_ref().filter(|swipe| swipe.workspace == *workspace) {
                viewport.current = swipe.position();
                viewport.velocity = 0.0;
            } else if self.animations_enabled {
                active_animation |= viewport.advance(delta, self.viewport_spring_config);
            } else {
                viewport.snap();
            }
        }

        let mut remaps = Vec::new();

        for window in self.space.elements() {
            let Some(id) = self.windows.ids().get(window).copied() else {
                continue;
            };

            let held = held_reflow.contains(&id);
            let Some(record) = self.windows.record_mut(id) else {
                continue;
            };

            let natural_pending = record.natural_floating_pending;
            let Some(geometry) = record.geometry.as_mut() else {
                continue;
            };

            let previous = geometry.visual.current;
            geometry.presentation_changed = false;
            let zooming = geometry.is_zooming();

            let coupled_target = record.coupled_width.as_ref().map(|(_, width)| width.target);
            if coupled_target.is_some() {
                geometry.visual.target.width = geometry.visual.current.width;
                geometry.visual.velocity.width = 0.0;
            }
            if !held {
                active_animation |= geometry.advance(delta, self.spring_config, self.animations_enabled);
            }

            if zooming != geometry.is_zooming() {
                self.stacking_cache.invalidate();
            }

            if let Some(target) = coupled_target {
                geometry.visual.target.width = target;
            }

            if let Some((workspace, world_x)) = record.world_x.as_mut()
                && let Some(viewport) = self.viewport_animations.get(workspace)
            {
                if zooming {
                    sync_scrolling_coordinates(geometry, world_x, viewport, true);
                } else if !held && self.animations_enabled {
                    active_animation |= world_x.advance(delta, self.spring_config);
                } else if !held {
                    world_x.snap();
                }

                if !zooming {
                    let mut presented_world = *world_x;
                    if held {
                        presented_world.velocity = 0.0;
                    }
                    let mut presented_viewport = *viewport;
                    if held_viewport.contains(workspace) {
                        presented_viewport.velocity = 0.0;
                    }
                    sync_scrolling_coordinates(geometry, &mut presented_world, &presented_viewport, false);
                }
            }

            if let Some((_, width)) = record.coupled_width.as_mut()
                && !held
            {
                let width_active = if self.animations_enabled {
                    width.advance(delta, self.viewport_spring_config)
                } else {
                    width.snap();
                    false
                };
                active_animation |= width_active;
                geometry.visual.current.width = width.current;
                geometry.visual.velocity.width = width.velocity;
                if !width_active {
                    record.coupled_width = None;
                }
            }

            if let Some(size) = geometry.client.expire_wait(now) {
                tracing::warn!(
                    ?id,
                    configured_width = size.width,
                    configured_height = size.height,
                    committed = ?geometry.client.committed_size,
                    "client did not commit the final configured size within 500 ms"
                );
            }

            geometry.presentation_changed |= previous != geometry.visual.current;
            if !held
                && let Some(size) = geometry.presentation_size_request(now)
                && !natural_pending
                && let Some(toplevel) = window.toplevel()
            {
                toplevel.with_pending_state(|state| {
                    state.size = Some((size.width, size.height).into());
                });
                toplevel.send_pending_configure();
            }

            let visual = geometry.visual.current;
            let location = (visual.x.round() as i32, visual.y.round() as i32).into();

            if self.space.element_location(window) != Some(location) {
                remaps.push((id, location));
            }
        }

        for (id, location) in remaps {
            if let Some(window) = self.windows.window(id) {
                self.space.map_element(window.clone(), location, false);
                self.stacking_cache.remapped();
            }
        }

        if !completed_slides.is_empty() {
            for output in completed_slides {
                self.workspace_slides.remove(&output);
            }

            let visible = self.visible_workspace_ids();
            let visible_windows = self
                .windows
                .ids()
                .values()
                .copied()
                .filter(|id| {
                    self.workspaces
                        .workspace_for_window(*id)
                        .is_none_or(|workspace| visible.contains(&workspace))
                })
                .collect::<HashSet<_>>();
            self.unmap_invisible_windows(&visible_windows);
            self.reconcile_workspaces(&visible);
            self.send_shell_snapshots();
            self.retarget_overview();
            active_animation = true;
        }
        if !self.overview.is_active() {
            self.retarget_overview();
        }
        active_animation |= self
            .overview
            .advance(delta, self.spring_config, self.animations_enabled);
        self.refresh_workspace_slide_offsets();
        self.sync_window_stacking();

        active_animation |= dim_changed;
        active_animation
    }

    pub(crate) fn animations_enabled(&self) -> bool {
        self.animations_enabled
    }

    pub(crate) fn animation_duration(&self, duration: Duration) -> Duration {
        scaled_animation_duration(duration, self.animations_enabled, self.animation_speed)
    }

    pub(crate) fn workspace_slide_offset(&self, window: WindowId) -> (f64, f64) {
        self.workspaces
            .workspace_for_window(window)
            .and_then(|workspace| self.workspace_slide_offsets.get(&workspace).copied())
            .unwrap_or_default()
    }

    pub(super) fn sample_workspace_slide_offsets(
        &self,
        output: Option<OutputId>,
        delta: Duration,
    ) -> HashMap<WorkspaceId, (f64, f64)> {
        let mut offsets = HashMap::new();
        self.fill_workspace_slide_offsets(output, delta, &mut offsets);
        offsets
    }

    pub(super) fn refresh_workspace_slide_offsets(&mut self) {
        // Reuse the input snapshot's allocation. Predicted render snapshots
        // remain separate and never change hit-testing or gesture state.
        let mut offsets = std::mem::take(&mut self.workspace_slide_offsets);
        offsets.clear();
        self.fill_workspace_slide_offsets(None, Duration::ZERO, &mut offsets);
        self.workspace_slide_offsets = offsets;
    }

    fn fill_workspace_slide_offsets(
        &self,
        output: Option<OutputId>,
        delta: Duration,
        offsets: &mut HashMap<WorkspaceId, (f64, f64)>,
    ) {
        for (output_id, slide) in &self.workspace_slides {
            if output.is_some_and(|output| output != *output_id) {
                continue;
            }

            let Some(size) = self
                .outputs_by_id
                .get(output_id)
                .and_then(|output| self.space.output_geometry(output))
                .map(|geometry| geometry.size)
            else {
                continue;
            };

            for item in &slide.items {
                let position = slide.position(item, delta);
                offsets.insert(
                    item.workspace,
                    (position.x * f64::from(size.w), position.y * f64::from(size.h)),
                );
            }
        }
    }

    pub(crate) fn cancel_workspace_slides(&mut self) -> bool {
        let active = !self.workspace_slides.is_empty();
        self.workspace_slides.clear();
        self.workspace_slide_offsets.clear();
        active
    }
}

// Inspect current targets, not the previous frame's activity. Target changes
// and client waits must wake a desktop that was settled on its last tick.
/// Zoom owns the visual rectangle until its final handoff tick. Otherwise the
/// strip coordinates own x. Keep this ownership decision explicit at callers.
pub(super) fn sync_scrolling_coordinates(
    geometry: &mut WindowGeometry,
    world: &mut AnimatedValue,
    viewport: &AnimatedValue,
    zoom_owns: bool,
) {
    if zoom_owns {
        world.current = geometry.visual.current.x + viewport.current;
        world.velocity = geometry.visual.velocity.x + viewport.velocity;
        debug_assert!((world.current - geometry.visual.current.x - viewport.current).abs() < 1e-6);
    } else {
        geometry.visual.current.x = world.current - viewport.current;
        geometry.visual.velocity.x = world.velocity - viewport.velocity;
        debug_assert!((geometry.logical.x - world.target + viewport.target).abs() < 1e-6);
    }
}

fn record_needs_tick(
    record: &super::window_registry::WindowRecord,
    focus: f64,
    dim: f64,
    mapped: impl FnOnce() -> bool,
) -> bool {
    if record.resize.is_some()
        || record.opening.is_some()
        || record.focus.as_ref().is_none_or(|motion| motion.needs_update(focus))
        || record.shadow.as_ref().is_none_or(|motion| motion.needs_update(focus))
    {
        return true;
    }

    let moving = |value: &AnimatedValue| value.current != value.target || value.velocity != 0.0;
    let pending = record.dimming.as_ref().is_none_or(|motion| motion.needs_update(dim))
        || record.world_x.as_ref().is_some_and(|(_, world)| moving(world))
        || record.coupled_width.is_some()
        || record.geometry.is_some_and(|geometry| {
            geometry.is_zooming()
                || geometry.presentation_changed
                || geometry.client.waiting_for_commit()
                || geometry.visual.current != geometry.visual.target
                || geometry.visual.velocity != ferese_animation::RectVelocity::default()
                || (!record.natural_floating_pending
                    && geometry.client.last_configured_size != Some(ClientSize::from_rect(geometry.logical)))
        });

    // Hidden geometry and dimming do not advance in the full path.
    // A stale hidden target must not keep waking the whole desktop.
    pending && mapped()
}

#[cfg(test)]
mod tests {
    use super::super::window_registry::WindowRecord;
    use super::*;

    fn settled() -> WindowRecord {
        let rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut geometry = WindowGeometry::new(rect, Some(ClientSize::from_rect(rect)));
        geometry.presentation_changed = false;

        WindowRecord {
            geometry: Some(geometry),
            focus: Some(AnimatedValue::new(1.0)),
            shadow: Some(AnimatedValue::new(1.0)),
            dimming: Some(DimAnimation::new(0.0)),
            ..Default::default()
        }
    }

    #[test]
    fn idle_check_skips_mapping_lookup_but_new_focus_and_dim_targets_wake_it() {
        let record = settled();

        for _ in 0..100 {
            assert!(!record_needs_tick(&record, 1.0, 0.0, || panic!(
                "settled record needs no mapping lookup"
            )));
        }

        assert!(record_needs_tick(&record, 0.0, 0.0, || true));
        assert!(record_needs_tick(&record, 1.0, 0.15, || true));
        assert!(!record_needs_tick(&record, 1.0, 0.15, || false));
    }

    #[test]
    fn shadow_keeps_ticks_until_it_settles_after_emphasis() {
        let mut record = settled();
        let shadow = record.shadow.as_mut().unwrap();
        shadow.current = 0.9;
        shadow.velocity = 0.2;
        assert!(record_needs_tick(&record, 1.0, 0.0, || false));
        record.shadow.as_mut().unwrap().advance(
            Duration::from_secs(3),
            crate::presentation::shadow_spring(SpringConfig::default()),
        );
        assert!(!record_needs_tick(&record, 1.0, 0.0, || false));
    }

    #[test]
    fn geometry_motion_client_waits_and_resize_barriers_cannot_be_skipped() {
        let mut record = settled();
        record
            .geometry
            .as_mut()
            .unwrap()
            .visual
            .set_target(Rect::new(10.0, 0.0, 400.0, 300.0));
        assert!(record_needs_tick(&record, 1.0, 0.0, || true));
        assert!(!record_needs_tick(&record, 1.0, 0.0, || false));

        let mut record = settled();
        record.geometry.as_mut().unwrap().client.request_size(
            ClientSize {
                width: 500,
                height: 300,
            },
            Duration::ZERO,
        );
        assert!(record_needs_tick(&record, 1.0, 0.0, || true));
        record.resize = Some(crate::resize_transaction::ResizeTransaction::new(
            1.into(),
            Duration::ZERO,
        ));
        assert!(record_needs_tick(&record, 1.0, 0.0, || false));
    }

    #[test]
    fn opening_and_coupled_width_cleanup_still_get_their_final_tick() {
        let mut record = settled();
        record.opening = Some(AnimatedValue::new(0.0));
        assert!(record_needs_tick(&record, 1.0, 0.0, || false));
        record.opening = None;
        assert!(!record_needs_tick(&record, 1.0, 0.0, || true));
        record.coupled_width = Some((WorkspaceId(1), AnimatedValue::new(400.0)));
        assert!(record_needs_tick(&record, 1.0, 0.0, || true));
    }

    #[test]
    fn scrolling_target_changes_and_zoom_ownership_switches_are_continuous() {
        let config = SpringConfig::default();
        let mut viewport = AnimatedValue::new(100.0);
        viewport.set_target(400.0);
        viewport.advance_with_policy(Duration::from_millis(40), config, CrossingPolicy::NoCrossing);
        let mut world = AnimatedValue::new(500.0);
        world.set_target(900.0);
        world.advance_with_policy(Duration::from_millis(40), config, CrossingPolicy::NoCrossing);
        let mut width = AnimatedValue::new(400.0);
        width.set_target(700.0);
        width.advance_with_policy(Duration::from_millis(40), config, CrossingPolicy::NoCrossing);
        let mut geometry = WindowGeometry::new(
            Rect::new(world.current - viewport.current, 0.0, width.current, 300.0),
            None,
        );
        let target = Rect::new(world.target - viewport.target, 0.0, width.target, 300.0);
        geometry.set_logical_target(target, Duration::from_millis(40));
        sync_scrolling_coordinates(&mut geometry, &mut world, &viewport, false);
        geometry.visual.velocity.width = width.velocity;
        let before = geometry.visual.current;
        viewport.retarget_preserving_motion(300.0);
        world.retarget_preserving_motion(1000.0);
        width.retarget_preserving_motion(600.0);
        geometry.set_logical_target(
            Rect::new(world.target - viewport.target, 0.0, width.target, 300.0),
            Duration::from_millis(40),
        );
        sync_scrolling_coordinates(&mut geometry, &mut world, &viewport, false);
        assert_eq!(geometry.visual.current, before);
        assert_eq!(geometry.visual.velocity.x, world.velocity - viewport.velocity);
        assert_eq!(geometry.logical.x, world.target - viewport.target);

        for mode in [
            PresentationMode::Maximized,
            PresentationMode::Fullscreen,
            PresentationMode::Normal,
        ] {
            let before = geometry.visual.current;
            geometry.set_presentation_mode(Rect::new(0.0, 0.0, 1920.0, 1080.0), mode, Duration::from_millis(40));
            sync_scrolling_coordinates(&mut geometry, &mut world, &viewport, true);
            assert_eq!(geometry.visual.current, before);
            assert_eq!(world.current, geometry.visual.current.x + viewport.current);
            geometry.advance(Duration::ZERO, config, true);
            assert_eq!(geometry.visual.current, before);
            geometry.advance(Duration::from_millis(16), config, true);
        }

        // Hand back to scrolling after a completed zoom. world_x was mirrored
        // on the final zoom tick; changing its destination must not change x.
        for _ in 0..200 {
            let zoom_owned = geometry.is_zooming();
            geometry.advance(Duration::from_millis(16), config, true);
            if zoom_owned {
                sync_scrolling_coordinates(&mut geometry, &mut world, &viewport, true);
            }
            if !geometry.is_zooming() {
                break;
            }
        }
        assert!(!geometry.is_zooming());
        let before = geometry.visual.current;
        world.set_target(geometry.logical.x + viewport.target);
        sync_scrolling_coordinates(&mut geometry, &mut world, &viewport, false);
        assert_eq!(geometry.visual.current, before);
    }

    #[test]
    #[ignore = "requires a private XDG_RUNTIME_DIR and permission to bind test sockets"]
    fn resize_pause_and_resume_use_fake_wall_time_without_charging_the_pause() {
        if std::env::var_os("FERESE_ANIMATION_PAUSE_TEST_CHILD").is_none() {
            let runtime = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "state::animation::tests::resize_pause_and_resume_use_fake_wall_time_without_charging_the_pause",
                    "--ignored",
                    "--nocapture",
                ])
                .env("FERESE_ANIMATION_PAUSE_TEST_CHILD", "1")
                .env("XDG_RUNTIME_DIR", runtime.path())
                .env_remove("FERESE_SOCKET")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let runtime = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
        assert!(runtime.starts_with(std::env::temp_dir()));
        let mut event_loop = EventLoop::try_new().unwrap();
        let display = Display::new().unwrap();
        let config = crate::config::Config::default().runtime_config().unwrap();
        let mut state = Ferese::new(&mut event_loop, display, config).unwrap();
        let output = Output::new(
            "pause-test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (1920, 1080).into(),
                refresh: 60000,
            }),
            None,
            None,
            None,
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "pause-test".into());
        let workspace = state.workspaces.active_id();
        let id = WindowId(1);
        state.workspaces.insert_window(id, Axis::Horizontal, 0.5).unwrap();
        state.windows.records.insert(id, settled());
        let mut viewport = AnimatedValue::new(0.0);
        viewport.set_target(500.0);
        state.viewport_animations.insert(workspace, viewport);
        state.advance_animations_at(Duration::from_millis(40), Duration::from_millis(40));
        let frozen = state.viewport_animations[&workspace];
        assert!(frozen.current > 0.0 && frozen.current < 500.0);

        // No intervening dispatch at all: expiration happens on the 300ms tick.
        state.windows.set_transaction(
            id,
            crate::resize_transaction::ResizeTransaction::new(1.into(), Duration::from_millis(40)),
        );
        state.rebuild_presentation_dependencies();
        state
            .presentation_dependencies
            .wait_for_viewport(workspace, id, 1.into());
        state.advance_animations_at(Duration::from_millis(300), Duration::from_millis(340));
        assert!(state.windows.transaction(&id).is_none());
        assert_eq!(state.viewport_animations[&workspace], frozen);
        state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(356));
        let mut expected = frozen;
        expected.advance_with_policy(
            Duration::from_millis(16),
            state.viewport_spring_config,
            CrossingPolicy::NoCrossing,
        );
        assert_eq!(state.viewport_animations[&workspace], expected);

        // A commit releases the barrier between ticks. Drop that release tick's
        // elapsed pause too, then resume at the next normal 16ms interval.
        state.windows.set_transaction(
            id,
            crate::resize_transaction::ResizeTransaction::new(2.into(), Duration::from_millis(356)),
        );
        state.rebuild_presentation_dependencies();
        state
            .presentation_dependencies
            .wait_for_viewport(workspace, id, 2.into());
        state.advance_animations_at(Duration::from_millis(100), Duration::from_millis(456));
        let frozen = state.viewport_animations[&workspace];
        state.windows.clear_transaction(&id);
        state.advance_animations_at(Duration::from_millis(200), Duration::from_millis(656));
        assert_eq!(state.viewport_animations[&workspace], frozen);
        state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(672));
        let mut expected = frozen;
        expected.advance_with_policy(
            Duration::from_millis(16),
            state.viewport_spring_config,
            CrossingPolicy::NoCrossing,
        );
        assert_eq!(state.viewport_animations[&workspace], expected);

        let frozen = state.viewport_animations[&workspace];
        let instant = Instant::now();
        state.last_animation_tick = instant;
        state.session_lock.sleeping = true;
        state.advance_animations(instant + Duration::from_millis(300));
        assert_eq!(state.viewport_animations[&workspace], frozen);
        state.session_lock.sleeping = false;
        state.advance_animations(instant + Duration::from_millis(316));
        let mut expected = frozen;
        expected.advance_with_policy(
            Duration::from_millis(16),
            state.viewport_spring_config,
            CrossingPolicy::NoCrossing,
        );
        assert_eq!(state.viewport_animations[&workspace], expected);

        // Wake paths reset the global clock even when nothing dispatched while
        // asleep. This excludes a whole undelivered pause interval as well.
        let frozen = state.viewport_animations[&workspace];
        state.last_animation_tick = Instant::now() - Duration::from_secs(5);
        state.reset_animation_clock();
        let resumed = state.last_animation_tick;
        state.advance_animations(resumed + Duration::from_millis(16));
        let mut expected = frozen;
        expected.advance_with_policy(
            Duration::from_millis(16),
            state.viewport_spring_config,
            CrossingPolicy::NoCrossing,
        );
        assert_eq!(state.viewport_animations[&workspace], expected);

        // A settled transaction still has to flush its resume guard; otherwise
        // the next unrelated animation would inherit an old paused workspace.
        state.viewport_animations.get_mut(&workspace).unwrap().snap();
        state.windows.record_mut(id).unwrap().focus = Some(AnimatedValue::new(0.0));
        state.windows.set_transaction(
            id,
            crate::resize_transaction::ResizeTransaction::new(3.into(), Duration::from_millis(800)),
        );
        state.rebuild_presentation_dependencies();
        state
            .presentation_dependencies
            .wait_for_viewport(workspace, id, 3.into());
        state.advance_animations_at(Duration::from_millis(100), Duration::from_millis(900));
        assert!(state.presentation_dependencies.needs_tick());
        state.windows.clear_transaction(&id);
        state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(916));
        assert!(!state.presentation_dependencies.needs_tick());
    }
}
