use super::*;

/// Output-local presentation values. Sampling never changes authoritative
/// geometry, configure barriers, animation clocks, or another output's view.
pub(crate) struct FrameScene {
    pub windows: HashMap<WindowId, WindowFrame>,
    pub overview: crate::overview::OverviewMotion,
    // Plane selection and scheduling reuse the activity sampled with this scene.
    pub animating: bool,
    pub delta: Duration,
}

pub(crate) struct WindowFrame {
    pub geometry: WindowGeometry,
    pub presentation: crate::presentation::WindowPresentation,
    pub dim: f64,
}

impl Ferese {
    pub(crate) fn current_window_presentation(&self, id: WindowId) -> Option<crate::presentation::WindowPresentation> {
        let geometry = *self.windows.geometry(&id)?;
        Some(self.compose_window_presentation(
            id,
            geometry,
            self.overview.motion(),
            self.workspace_slide_offset(id),
            Duration::ZERO,
        ))
    }

    fn compose_window_presentation(
        &self,
        id: WindowId,
        geometry: WindowGeometry,
        overview: &crate::overview::OverviewMotion,
        offset: (f64, f64),
        delta: Duration,
    ) -> crate::presentation::WindowPresentation {
        let record = self.windows.record(id).expect("presentation has a window record");
        let mut visual = geometry.visual;
        if self.presentation_dependencies.reflow_blocked(id, &self.windows) {
            let predicted_x_velocity = visual.velocity.x;
            visual.velocity = Default::default();
            if !geometry.is_zooming()
                && let Some((workspace, _)) = record.world_x.as_ref()
                && !self
                    .presentation_dependencies
                    .viewport_blocked(*workspace, &self.windows)
                && let Some(viewport) = self.viewport_animations.get(workspace)
            {
                visual.velocity.x = if delta.is_zero() {
                    -viewport.velocity
                } else {
                    predicted_x_velocity
                };
            }
        }
        let mut bounds = overview.presented_bounds(id, visual);
        bounds.current.x += offset.0;
        bounds.current.y += offset.1;
        if let Some(workspace) = self.workspaces.workspace_for_window(id)
            && let Some((output_id, slide)) = self
                .workspace_slides
                .iter()
                .find(|(_, slide)| slide.contains(workspace))
            && let Some(item) = slide.items.iter().find(|item| item.workspace == workspace)
            && let Some(area) = self
                .outputs_by_id
                .get(output_id)
                .and_then(|output| self.space.output_geometry(output))
        {
            let (_, velocity) = slide.sample(item, delta);
            bounds.velocity.x += velocity.x * f64::from(area.size.w);
            bounds.velocity.y += velocity.y * f64::from(area.size.h);
        }
        let focused = if self.overview.is_active() {
            self.overview_selected(id)
        } else {
            self.focused_window == Some(id)
        };
        let mut opacity = record.opening.unwrap_or_else(|| AnimatedValue::new(1.0));
        let mut emphasis = record
            .focus
            .unwrap_or_else(|| AnimatedValue::new(if focused { 1.0 } else { 0.0 }));
        let mut shadow = record.shadow.unwrap_or(emphasis);
        if !self.animations_enabled {
            opacity.snap();
            emphasis.snap();
            shadow.snap();
        } else if !delta.is_zero() {
            opacity.advance(
                delta,
                SpringConfig {
                    position_tolerance: 0.00001,
                    velocity_tolerance: 0.00001,
                    ..self.spring_config
                },
            );
            emphasis.advance(delta, crate::presentation::emphasis_spring(self.spring_config));
            shadow.advance(delta, crate::presentation::shadow_spring(self.spring_config));
        }
        let presence_scale = 0.97 + 0.03 * opacity.current;
        let native_size =
            (!overview.is_presenting() && presence_scale != 1.0).then(|| ClientSize::from_rect(bounds.current));
        bounds.velocity = crate::presentation::scaled_visual_velocity(
            bounds.current,
            bounds.velocity,
            presence_scale,
            0.03 * opacity.velocity,
        );
        bounds.current = crate::presentation::scaled_visual_rect(bounds.current, presence_scale);
        bounds.target = bounds.current;
        crate::presentation::WindowPresentation {
            id,
            bounds,
            opacity,
            emphasis,
            shadow,
            scale_content: overview.is_presenting(),
            native_size,
        }
    }

    fn output_window_records<'a>(
        &'a self,
        output: &'a Output,
        block_resizes: bool,
    ) -> impl Iterator<Item = (WindowId, &'a super::window_registry::WindowRecord, bool)> + 'a {
        self.output_id(output)
            .into_iter()
            .flat_map(move |output| self.output_workspaces.assigned_workspaces(output))
            .filter_map(move |id| self.workspaces.workspace(id))
            .flat_map(move |workspace| {
                workspace
                    .layout
                    .window_ids()
                    .chain(workspace.floating.iter().copied())
                    .filter_map(move |id| {
                        Some((
                            id,
                            self.windows.record(id)?,
                            block_resizes && self.presentation_dependencies.reflow_blocked(id, &self.windows),
                        ))
                    })
            })
    }

    pub(crate) fn output_has_pending_visual_changes(&self, output: &Output) -> bool {
        let selected = if self.overview.is_active() {
            self.overview.selected()
        } else {
            self.focused_window
        };
        self.output_window_records(output, false).any(|(id, record, _)| {
            self.window_belongs_to_output(id, output)
                && (record
                    .focus
                    .as_ref()
                    .is_some_and(|focus| focus.needs_update(if selected == Some(id) { 1.0 } else { 0.0 }))
                    || record
                        .shadow
                        .as_ref()
                        .is_some_and(|shadow| shadow.needs_update(if selected == Some(id) { 1.0 } else { 0.0 }))
                    || record.dimming.as_ref().is_some_and(|dim| {
                        dim.needs_update(crate::dimming::target(
                            self.inactive_dim,
                            self.focused_window,
                            id,
                            self.overview.is_presenting(),
                        ))
                    }))
        })
    }

    pub(crate) fn output_has_animations(&self, output: &Output) -> bool {
        if self
            .render
            .closing
            .iter()
            .any(|window| Some(window.output) == self.output_id(output))
            || self.overview.is_animating(self.spring_config)
            || self
                .dismissing_popups
                .iter()
                .any(|(root, _, _)| self.surface_outputs(root).contains(output))
        {
            return true;
        }

        if self
            .output_id(output)
            .and_then(|id| self.workspace_slides.get(&id))
            .is_some_and(|slide| slide.held_progress.is_none() && slide.moving())
        {
            return true;
        }

        self.output_window_records(output, false).any(|(id, record, _)| {
            let Some(mut geometry) = record.geometry else {
                return false;
            };

            if !self.window_belongs_to_output(id, output) {
                return false;
            }

            if record.resize.is_some()
                || record.opening.is_some()
                || self.render.snapshot(&id).is_some()
                || record.focus.as_ref().is_some_and(|focus| focus.is_animating())
                || record.shadow.as_ref().is_some_and(|shadow| shadow.is_animating())
                || record.dimming.as_ref().is_some_and(|dim| dim.is_animating())
            {
                return true;
            }

            if geometry.advance(Duration::ZERO, self.spring_config, self.animations_enabled) {
                return true;
            }

            let Some(workspace) = self.workspaces.workspace_for_window(id) else {
                return false;
            };

            self.viewport_animations.get(&workspace).is_some_and(|viewport| {
                let mut viewport = *viewport;
                let held = self
                    .focus_swipe
                    .as_ref()
                    .is_some_and(|swipe| swipe.workspace == workspace);
                !held && viewport.advance(Duration::ZERO, self.viewport_spring_config)
            }) || record.coupled_width.as_ref().is_some_and(|(_, width)| {
                let mut width = *width;
                width.advance(Duration::ZERO, self.viewport_spring_config)
            })
        })
    }

    pub(crate) fn sample_frame(&self, output: &Output, horizon: Duration) -> FrameScene {
        let animating = self.output_has_animations(output);
        let delta = if self.animations_enabled && animating {
            horizon.mul_f64(self.animation_speed)
        } else {
            Duration::ZERO
        };

        let overview = self.overview.sample(delta, self.spring_config);
        let predicted_slide_offsets =
            (!delta.is_zero()).then(|| self.sample_workspace_slide_offsets(self.output_id(output), delta));
        let slide_offsets = predicted_slide_offsets
            .as_ref()
            .unwrap_or(&self.workspace_slide_offsets);
        let mut windows = HashMap::new();

        for (id, record, blocked) in self.output_window_records(output, !delta.is_zero()) {
            let Some(original) = record.geometry else { continue };

            // The workspace strip also draws inactive workspaces on this output.
            // Keep their samples through the overview exit transition; the main
            // grid still filters by window_belongs_to_output in the render path.
            if !overview.is_presenting() && !self.window_belongs_to_output(id, output) {
                continue;
            }

            let world = record.world_x.as_ref().and_then(|(workspace, world)| {
                let viewport = self.viewport_animations.get(workspace)?;
                let held = self
                    .focus_swipe
                    .as_ref()
                    .is_some_and(|swipe| swipe.workspace == *workspace);

                Some((
                    *world,
                    *viewport,
                    held || self
                        .presentation_dependencies
                        .viewport_blocked(*workspace, &self.windows),
                ))
            });
            let width = record.coupled_width.as_ref().map(|(_, width)| *width);
            let geometry = if delta.is_zero() {
                original
            } else {
                predict_geometry(
                    original,
                    delta,
                    self.spring_config,
                    self.viewport_spring_config,
                    world,
                    width,
                    blocked,
                )
            };

            let offset = self
                .workspaces
                .workspace_for_window(id)
                .and_then(|workspace| slide_offsets.get(&workspace).copied())
                .unwrap_or_default();
            let presentation = self.compose_window_presentation(id, geometry, &overview, offset, delta);
            let mut dim = record.dimming.clone();
            if let Some(dim) = &mut dim {
                dim.predict(delta, self.inactive_dim.duration_ms);
            }
            windows.insert(
                id,
                WindowFrame {
                    geometry,
                    presentation,
                    dim: dim.map_or(0.0, |dim| dim.current),
                },
            );
        }

        FrameScene {
            windows,
            overview,
            animating,
            delta,
        }
    }
}

fn predict_geometry(
    original: WindowGeometry,
    delta: Duration,
    spring: SpringConfig,
    viewport_spring: SpringConfig,
    world: Option<(AnimatedValue, AnimatedValue, bool)>,
    width: Option<AnimatedValue>,
    reflow_held: bool,
) -> WindowGeometry {
    let mut predicted = original;
    let zooming = predicted.is_zooming();
    if width.is_some() {
        predicted.visual.target.width = predicted.visual.current.width;
        predicted.visual.velocity.width = 0.0;
    }

    if !reflow_held {
        predicted.advance(delta, spring, true);
    } else {
        predicted.visual.velocity = Default::default();
    }
    predicted.visual.target.width = original.visual.target.width;
    if let Some((mut world, mut viewport, held)) = world
        && !zooming
    {
        if !reflow_held {
            world.advance(delta, spring);
        } else {
            world.velocity = 0.0;
        }
        if !held {
            viewport.advance(delta, viewport_spring);
        } else {
            viewport.velocity = 0.0;
        }

        super::animation::sync_scrolling_coordinates(&mut predicted, &mut world, &viewport, false);
    }

    if let Some(mut width) = width
        && !reflow_held
    {
        width.advance(delta, viewport_spring);
        predicted.visual.current.width = width.current;
        predicted.visual.velocity.width = width.velocity;
    }

    predicted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a private XDG_RUNTIME_DIR and permission to bind test sockets"]
    fn overview_strip_switch_slides_stable_grids_and_keeps_strip_fixed() {
        let runtime = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
        assert!(runtime.starts_with(std::env::temp_dir()));
        let mut event_loop = EventLoop::try_new().unwrap();
        let mut state = Ferese::new(
            &mut event_loop,
            Display::new().unwrap(),
            crate::config::Config::default().runtime_config().unwrap(),
        )
        .unwrap();
        let output = Output::new(
            "overview-slide-test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (1200, 800).into(),
                refresh: 120_000,
            }),
            Some(smithay::utils::Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "overview-slide-test".into());
        let output_id = state.output_id(&output).unwrap();
        let from = state.output_workspaces.active_workspace(output_id).unwrap();
        let to = state.workspaces.create_workspace();
        state.output_workspaces.assign_workspace(output_id, to).unwrap();
        for (id, workspace) in [(WindowId(1), from), (WindowId(2), to)] {
            let normal = Rect::new(100., 160., 640., 480.);
            state
                .workspaces
                .insert_floating_window(id, workspace, normal, false)
                .unwrap();
            state.windows.records.insert(
                id,
                super::super::window_registry::WindowRecord {
                    geometry: Some(WindowGeometry::new(normal, None)),
                    ..Default::default()
                },
            );
        }
        state.set_overview_active(true);
        state.advance_animations_by(Duration::from_secs(10));
        let old = state.current_window_presentation(WindowId(1)).unwrap().bounds.current;
        let cards = state.overview_workspace_cards(&output);
        let card = cards.iter().find(|card| card.workspace == to).unwrap();
        assert!(
            state.click_overview_workspace(
                (
                    card.rect.x + card.rect.width * 0.5,
                    card.rect.y + card.rect.height * 0.5
                )
                    .into()
            )
        );
        assert!(state.workspace_slides.contains_key(&output_id));
        assert!(state.window_belongs_to_output(WindowId(1), &output));
        let incoming = state.current_window_presentation(WindowId(2)).unwrap().bounds.current;
        assert_eq!(incoming.width, old.width);
        assert_eq!(incoming.height, old.height);
        assert!(incoming.x > old.x + 1000.);
        let predicted = state.sample_frame(&output, Duration::from_millis(60));
        assert!(predicted.windows[&WindowId(1)].presentation.bounds.current.x < old.x);
        assert!(predicted.windows[&WindowId(2)].presentation.bounds.current.x < incoming.x);
        assert_eq!(
            predicted.windows[&WindowId(2)].presentation.bounds.current.width,
            incoming.width
        );
        // Activation can add a trailing empty workspace. Once that layout is
        // established, slide prediction must not translate the strip or previews.
        let current_cards = state.overview_workspace_cards(&output);
        let predicted_cards = state.overview_workspace_cards_for_frame(&output, Some(&predicted));
        for (before, after) in current_cards.iter().zip(predicted_cards) {
            assert_eq!(before.rect, after.rect);
            assert_eq!(before.windows, after.windows);
        }
        state.advance_animations_by(Duration::from_millis(80));
        let before = state.workspace_slides[&output_id].clone();
        state.update_workspace_slide(output_id, output_id, Some(to), from, None);
        let reversed = &state.workspace_slides[&output_id];
        assert_eq!(reversed.speed, crate::overview::OVERVIEW_MOTION_SPEED);
        for item in &before.items {
            let after = reversed
                .items
                .iter()
                .find(|other| other.workspace == item.workspace)
                .unwrap();
            assert_eq!(item.start, after.start);
            assert_eq!(item.velocity, after.velocity);
        }
        // Finish on the selected workspace and retire all outgoing previews.
        state.output_workspaces.switch_workspace(output_id, from).unwrap();
        state.retarget_overview();
        state.advance_animations_by(Duration::from_secs(10));
        assert!(state.workspace_slides.is_empty());
        assert!(!state.overview.has_window_preview(WindowId(2)));
        assert!(!state.overview.needs_tick());
        state.animations_enabled = false;
        let card = state
            .overview_workspace_cards(&output)
            .into_iter()
            .find(|card| card.workspace == to)
            .unwrap();
        state.click_overview_workspace(
            (
                card.rect.x + card.rect.width * 0.5,
                card.rect.y + card.rect.height * 0.5,
            )
                .into(),
        );
        assert!(state.workspace_slides.is_empty());
        assert!(!state.overview.needs_tick());
    }

    #[test]
    #[ignore = "requires a private XDG_RUNTIME_DIR and permission to bind test sockets"]
    fn frame_sampling_preserves_compositor_state_between_output_deadlines() {
        let runtime = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
        assert!(
            runtime.starts_with(std::env::temp_dir()),
            "use a disposable runtime directory"
        );
        let mut event_loop = EventLoop::try_new().unwrap();
        let display = Display::new().unwrap();
        let config = crate::config::Config::default().runtime_config().unwrap();
        let mut state = Ferese::new(&mut event_loop, display, config).unwrap();
        let output = Output::new(
            "forecast-test".into(),
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
                refresh: 60_000,
            }),
            Some(smithay::utils::Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "forecast-test".into());
        let id = WindowId(1);
        state.workspaces.insert_window(id, Axis::Horizontal, 0.5).unwrap();
        let mut geometry = WindowGeometry::new(Rect::new(0., 0., 400., 300.), None);
        geometry.visual.set_target(Rect::new(600., 0., 400., 300.));
        // This test isolates frame sampling from the protocol client fixture.
        state.windows.records.insert(
            id,
            super::super::window_registry::WindowRecord {
                geometry: Some(geometry),
                ..Default::default()
            },
        );
        let tick = state.last_animation_tick;
        for horizon in [
            Duration::from_millis(16),
            Duration::from_millis(4),
            Duration::from_millis(8),
        ] {
            {
                let forecast = state.sample_frame(&output, horizon);
                assert!(forecast.windows[&id].geometry.visual.current.x > geometry.visual.current.x);
                assert!(forecast.animating);
            }

            assert_eq!(*state.windows.geometry(&id).unwrap(), geometry);
            assert_eq!(state.last_animation_tick, tick);
        }

        // Sampling one output must not visit or include another output's records.
        let other = Output::new(
            "other-forecast".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        other.change_current_state(
            Some(smithay::output::Mode {
                size: (1280, 720).into(),
                refresh: 240_000,
            }),
            Some(smithay::utils::Transform::Normal),
            None,
            Some((1920, 0).into()),
        );
        state.space.map_output(&other, (1920, 0));
        state.register_output(&other, "other-forecast".into());
        let other_workspace = state
            .output_workspaces
            .active_workspace(state.output_id(&other).unwrap())
            .unwrap();
        let other_id = WindowId(2);
        state
            .workspaces
            .insert_floating_window(other_id, other_workspace, geometry.visual.current, false)
            .unwrap();
        state.windows.records.insert(
            other_id,
            super::super::window_registry::WindowRecord {
                geometry: Some(geometry),
                ..Default::default()
            },
        );

        assert_eq!(
            state
                .output_window_records(&output, false)
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>(),
            vec![id]
        );
        assert_eq!(
            state
                .output_window_records(&other, false)
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>(),
            vec![other_id]
        );
        assert!(
            !state
                .sample_frame(&output, Duration::from_millis(8))
                .windows
                .contains_key(&other_id)
        );
        assert!(
            !state
                .sample_frame(&other, Duration::from_millis(4))
                .windows
                .contains_key(&id)
        );

        let output_id = state.output_id(&output).unwrap();
        let from = state.workspaces.workspace_for_window(id).unwrap();
        let to = state.workspaces.create_workspace();
        state.output_workspaces.assign_workspace(output_id, to).unwrap();
        let mut slide = WorkspaceSlide::new(None, from, to, SwipeDirection::Left);
        slide.held_progress = Some(0.25);
        state.workspace_slides.insert(output_id, slide);
        state.refresh_workspace_slide_offsets();
        let offsets = state.workspace_slide_offsets.clone();
        assert_eq!(offsets[&from].0, -480.0);
        assert_eq!(offsets[&to].0, 1440.0);
        assert_eq!(
            state.sample_frame(&output, Duration::from_millis(8)).windows[&id]
                .presentation
                .bounds
                .current
                .x,
            state.sample_frame(&output, Duration::from_millis(8)).windows[&id]
                .geometry
                .visual
                .current
                .x
                - 480.0
        );
        assert_eq!(state.workspace_slide_offsets, offsets);
        assert!(
            state
                .sample_workspace_slide_offsets(state.output_id(&other), Duration::from_millis(8))
                .is_empty()
        );
        state.cancel_workspace_slides();
        assert!(state.workspace_slide_offsets.is_empty());

        // A prediction must neither advance nor expire a client resize barrier.
        state.windows.set_transaction(
            id,
            crate::resize_transaction::ResizeTransaction::new(9.into(), Duration::ZERO),
        );
        state.rebuild_presentation_dependencies();
        let blocked = state.sample_frame(&output, Duration::from_millis(16));
        assert_eq!(blocked.windows[&id].geometry, geometry);
        assert!(state.windows.transaction(&id).is_some());
        state.windows.clear_transaction(&id);

        state.animations_enabled = false;
        let forecast = state.sample_frame(&output, Duration::from_millis(16));
        assert_eq!(forecast.windows[&id].geometry, geometry);

        let settled = WindowGeometry::new(Rect::new(0., 0., 400., 300.), None);
        state.windows.set_geometry(id, settled);
        state.animations_enabled = true;
        assert!(!state.sample_frame(&output, Duration::ZERO).animating);
        assert!(state.sample_frame(&other, Duration::ZERO).animating);
        // Target changes with animations disabled still need their final frame
        // on both displays, even though no spring/fade remains active afterward.
        state.focused_window = Some(id);
        state
            .windows
            .update(id, |record| record.focus = Some(AnimatedValue::new(1.0)));
        state.focused_window = Some(other_id);
        assert!(state.output_has_pending_visual_changes(&output));
        state.animations_enabled = false;
        state.advance_animations(Instant::now());
        assert!(!state.output_has_pending_visual_changes(&output));
        state.animations_enabled = true;
        state.windows.set_geometry(id, geometry);
        assert!(state.output_has_animations(&output));
        state.unregister_output(&other);
        assert!(!state.output_has_animations(&other));
        assert!(
            state.output_has_animations(&output),
            "remaining output keeps its active animation"
        );

        let inactive = state.workspaces.create_workspace();
        state.output_workspaces.assign_workspace(output_id, inactive).unwrap();
        let preview_id = WindowId(3);
        state
            .workspaces
            .insert_floating_window(preview_id, inactive, settled.visual.current, false)
            .unwrap();
        state.windows.records.insert(
            preview_id,
            super::super::window_registry::WindowRecord {
                geometry: Some(settled),
                ..Default::default()
            },
        );
        assert!(
            !state
                .sample_frame(&output, Duration::ZERO)
                .windows
                .contains_key(&preview_id)
        );

        state.set_overview_active(true);
        let frame = state.sample_frame(&output, Duration::ZERO);
        assert!(
            frame.windows.contains_key(&preview_id),
            "inactive workspace needs a thumbnail sample"
        );
        assert!(
            !state.window_belongs_to_output(preview_id, &output),
            "thumbnail must stay out of the main grid"
        );
        for card in state.overview_workspace_cards_for_frame(&output, Some(&frame)) {
            for (id, _) in card.windows {
                assert!(
                    frame.windows.contains_key(&id),
                    "workspace card has no presentation for {id:?}"
                );
            }
        }

        state.set_overview_active(false);
        assert!(
            state.overview.is_presenting(),
            "exit transition keeps the strip visible"
        );
        assert!(
            state
                .sample_frame(&output, Duration::ZERO)
                .windows
                .contains_key(&preview_id)
        );
    }

    #[test]
    fn mixed_refresh_forecasts_do_not_change_authoritative_geometry_or_client_state() {
        let mut geometry = WindowGeometry::new(Rect::new(0., 0., 400., 300.), None);
        geometry.visual.set_target(Rect::new(600., 0., 500., 300.));
        let original = geometry;
        let sample = |delta| {
            predict_geometry(
                geometry,
                delta,
                SpringConfig::default(),
                SpringConfig::default(),
                None,
                None,
                false,
            )
        };

        let fast = sample(Duration::from_nanos(1_000_000_000 / 240));
        let slow = sample(Duration::from_nanos(1_000_000_000 / 60));
        assert!(fast.visual.current.x > 0.);
        assert!(slow.visual.current.x > fast.visual.current.x);
        assert_eq!(sample(Duration::from_nanos(1_000_000_000 / 240)), fast);
        assert_eq!(geometry, original);
        assert_eq!(slow.client, original.client);
        assert_eq!(slow.logical, original.logical);
    }

    #[test]
    fn scrolling_coordinates_and_coupled_width_use_the_same_forecast_time() {
        let mut geometry = WindowGeometry::new(Rect::new(500., 0., 400., 300.), None);
        geometry.set_logical_target(Rect::new(400., 0., 500., 300.), Duration::ZERO);
        let mut world = AnimatedValue::new(500.);
        world.set_target(600.);
        let mut viewport = AnimatedValue::new(0.);
        viewport.set_target(200.);
        let mut width = AnimatedValue::new(400.);
        width.set_target(500.);
        let spring = SpringConfig::default();
        let delta = Duration::from_millis(10);
        let predicted = predict_geometry(
            geometry,
            delta,
            spring,
            spring,
            Some((world, viewport, false)),
            Some(width),
            false,
        );
        world.advance_with_policy(delta, spring, CrossingPolicy::NoCrossing);
        viewport.advance_with_policy(delta, spring, CrossingPolicy::NoCrossing);
        width.advance_with_policy(delta, spring, CrossingPolicy::NoCrossing);
        assert_eq!(predicted.visual.current.x, world.current - viewport.current);
        assert_eq!(predicted.visual.current.width, width.current);
        assert_eq!(predicted.visual.target, geometry.visual.target);
    }
}
