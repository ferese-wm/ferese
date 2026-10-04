use super::*;

impl Ferese {
    pub(crate) fn restore_initial_floating_size(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else { return };
        let (app_id, title, transient) = with_states(toplevel.wl_surface(), |states| {
            let attributes = states.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap();
            (
                attributes.app_id.clone(),
                attributes.title.clone(),
                attributes.parent.is_some(),
            )
        });
        let special_mode = toplevel.with_pending_state(|pending| {
            pending.states.contains(xdg_toplevel::State::Fullscreen)
                || pending.states.contains(xdg_toplevel::State::Maximized)
        });
        let rule = resolve_window_rules(&self.window_rules, app_id.as_deref(), title.as_deref(), transient);
        if crate::window_rules::is_native_dialog(app_id.as_deref())
            || transient
            || special_mode
            || rule.fullscreen == Some(true)
            || !rule.floating.unwrap_or(rule.width.is_some() || rule.height.is_some())
        {
            return;
        }

        let outputs = self.floating_outputs();
        let Some((remembered, work)) = app_id
            .as_deref()
            .and_then(|app| self.floating_memory.get(app))
            .and_then(|entry| {
                let rect = entry.restore(&outputs)?;
                let work = outputs.iter().find(|(name, _)| *name == entry.output)?.1;
                Some((rect, work))
            })
        else {
            return;
        };
        // Metadata is available on the initial bufferless commit. Restore the
        // size before the client creates its first buffer, without mapping or
        // focusing the window. Use the same rule/constraint precedence as map.
        let rect = self.place_floating(
            window,
            None,
            work,
            (remembered.width, remembered.height),
            Some((rule.width, rule.height)),
        );
        let size = ClientSize::from_rect(rect);
        toplevel.with_pending_state(|pending| pending.size = Some((size.width, size.height).into()));
    }

    pub(crate) fn add_rule_placed_window(
        &mut self,
        window: Window,
        app_id: Option<&str>,
        title: Option<&str>,
        parent: Option<WindowId>,
    ) {
        let rule = resolve_window_rules(&self.window_rules, app_id, title, parent.is_some());
        let floating = rule
            .floating
            .unwrap_or(parent.is_some() || rule.width.is_some() || rule.height.is_some());
        if !floating {
            self.add_tiled_window(window);
            return;
        }

        if let Some(parent) = parent {
            self.add_transient_window(window, parent);
            return;
        }

        if let Some(pointer) = self.seat.get_pointer() {
            self.focus_output_at(pointer.current_location());
        }

        let remembered_work = app_id
            .filter(|app| !crate::window_rules::is_native_dialog(Some(app)))
            .and_then(|app| self.floating_memory.get(app))
            .and_then(|entry| {
                let outputs = self.floating_outputs();
                entry.restore(&outputs)?;
                outputs
                    .into_iter()
                    .find(|(name, _)| *name == entry.output)
                    .map(|(_, work)| work)
            });
        if let Some(work) = remembered_work {
            self.focus_output_at((work.x + work.width / 2.0, work.y + work.height / 2.0).into());
        }

        let Some(bounds) = self.output_bounds() else {
            self.add_tiled_window(window);
            return;
        };

        let id = self.windows.allocate_id();
        let size_rect = client_size(&window)
            .map(|size| natural_floating_rect(bounds, size))
            .unwrap_or_else(|| centered_floating_rect(bounds));
        let rect = self.place_floating(&window, None, bounds, (size_rect.width, size_rect.height), None);
        let anchor = self
            .focused_window
            .and_then(|focused| self.presented_window_rect(focused))
            .unwrap_or(bounds);
        let fullscreen = self.workspaces.active().fullscreen;
        let focus = true;
        if let Err(error) = self
            .workspaces
            .insert_floating_window(id, self.workspaces.active_id(), rect, focus)
        {
            tracing::error!(%error, ?id, "failed to insert rule-placed floating window");
            return;
        }

        self.windows.register_window(window.clone(), id);
        self.refresh_capture_privacy();
        self.windows.update(id, |w| w.placement_anchor = Some(anchor));

        if rule.width.is_none() && rule.height.is_none() && client_size(&window).is_none() && remembered_work.is_none()
        {
            self.windows.update(id, |w| w.natural_floating_pending = true);
        }

        self.window_stack.insert(id);
        if let Some(fullscreen) = fullscreen {
            self.floating_above_fullscreen.insert(id, fullscreen);
        }

        if focus {
            self.focused_window = Some(id);
        }

        self.space.map_element(window, (0, 0), focus);
        // apply_initial_window_rules performs the first relayout/configure.
    }

    pub fn add_tiled_window(&mut self, window: Window) {
        let id = self.windows.allocate_id();
        let focus_new_window = self.workspaces.active().fullscreen.is_none();

        let axis = self
            .output_bounds()
            .and_then(|bounds| {
                self.workspaces
                    .active()
                    .layout
                    .automatic_axis(self.focused_window, bounds)
                    .ok()
            })
            .unwrap_or(Axis::Horizontal);

        if let Err(error) = self.workspaces.insert_window(id, axis, 0.5) {
            tracing::error!(%error, ?id, "failed to insert window into layout");
            return;
        }

        self.windows.register_window(window.clone(), id);
        self.refresh_capture_privacy();
        self.window_stack.insert(id);
        if focus_new_window {
            self.focused_window = Some(id);
        }

        self.space.map_element(window, (0, 0), focus_new_window);
        self.relayout();

        if focus_new_window {
            self.restore_keyboard_focus();
        }
    }

    pub fn add_transient_window(&mut self, window: Window, parent: WindowId) {
        let Some(workspace) = self.workspaces.workspace_for_window(parent) else {
            self.add_tiled_window(window);
            return;
        };

        let Some(bounds) = self.floating_bounds_for_window(parent) else {
            self.add_tiled_window(window);
            return;
        };

        let parent_rect = self.presented_window_rect(parent).unwrap_or(bounds);
        let visible_parent = crate::floating::intersection(parent_rect, bounds).unwrap_or(bounds);
        let size_rect = client_size(&window)
            .map(|s| natural_floating_rect(bounds, s))
            .unwrap_or_else(|| centered_transient_rect(visible_parent));
        let rect = self.place_floating(&window, None, bounds, (size_rect.width, size_rect.height), None);
        let id = self.windows.allocate_id();

        let focus = workspace == self.workspaces.active_id()
            && (self.focused_window == Some(parent) || self.workspaces.active().fullscreen == Some(parent));
        if let Err(error) = self.workspaces.insert_floating_window(id, workspace, rect, focus) {
            tracing::error!(%error, ?id, ?parent, "failed to insert transient window");
            return;
        }

        self.windows.register_window(window.clone(), id);
        self.refresh_capture_privacy();
        self.window_stack.insert(id);
        if client_size(&window).is_none() {
            self.windows.update(id, |w| w.natural_floating_pending = true);
        }

        if self
            .workspaces
            .workspace(workspace)
            .is_some_and(|workspace| workspace.fullscreen == Some(parent))
        {
            self.floating_above_fullscreen.insert(id, parent);
        }

        if focus {
            self.focused_window = Some(id);
        }

        self.space.map_element(window, (0, 0), focus);
        self.relayout();
        if focus {
            self.restore_keyboard_focus();
        }
    }

    pub(crate) fn apply_initial_window_rules(
        &mut self,
        window: &Window,
        app_id: Option<&str>,
        title: Option<&str>,
        transient: bool,
    ) {
        let Some(id) = self.windows.ids().get(window).copied() else {
            return;
        };

        if !self
            .windows
            .update(id, |w| !std::mem::replace(&mut w.rules_applied, true))
            .unwrap_or(false)
        {
            return;
        }

        let rule = resolve_window_rules(&self.window_rules, app_id, title, transient);
        self.apply_window_rule_result(window, rule);
    }

    pub(super) fn reapply_window_rules(&mut self, old_rules: &[WindowRule]) {
        let windows = self.windows.ids().keys().cloned().collect::<Vec<_>>();
        for window in windows {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };

            let (app_id, title, transient) = with_states(toplevel.wl_surface(), |states| {
                let attributes = states.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap();
                (
                    attributes.app_id.clone(),
                    attributes.title.clone(),
                    attributes.parent.is_some(),
                )
            });
            let old = resolve_window_rules(old_rules, app_id.as_deref(), title.as_deref(), transient);
            let new = resolve_window_rules(&self.window_rules, app_id.as_deref(), title.as_deref(), transient);
            if let Some(new) = crate::window_rules::live_result(old, new, transient) {
                self.apply_window_rule_result(&window, new);
            }
        }
    }

    pub(super) fn apply_window_rule_result(&mut self, window: &Window, rule: crate::window_rules::WindowRuleResult) {
        let Some(id) = self.windows.ids().get(window).copied() else {
            return;
        };

        let Some(bounds) = self.floating_bounds_for_window(id) else {
            return;
        };

        if rule == Default::default() {
            return;
        }

        let axis = self
            .workspaces
            .active()
            .layout
            .automatic_axis(self.focused_window, bounds)
            .unwrap_or(Axis::Horizontal);

        if let Some(index) = rule.workspace
            && let Some(output) = self
                .workspaces
                .workspace_for_window(id)
                .and_then(|workspace| self.output_workspaces.output_for_workspace(workspace))
            && let Some(workspace) = self.output_workspaces.workspace_at(&self.workspaces, output, index)
            && let Err(error) = self.workspaces.move_window_to_workspace(id, workspace, axis, 0.5)
        {
            tracing::warn!(%error, ?id, ?workspace, "failed to apply window workspace rule");
        }

        let bounds = self.floating_bounds_for_window(id).unwrap_or(bounds);

        let should_float = rule
            .floating
            .or((rule.width.is_some() || rule.height.is_some()).then_some(true));
        if let Some(should_float) = should_float {
            let is_floating = matches!(self.workspaces.placement(id), Some(WindowPlacement::Floating { .. }));
            let mut rect = centered_floating_rect(bounds);
            if should_float && rule.width.is_none() && rule.height.is_none() {
                if let Some(size) = client_size(window) {
                    rect = natural_floating_rect(bounds, size);
                } else if Self::floating_metadata(window)
                    .0
                    .as_deref()
                    .and_then(|app| self.floating_memory.get(app))
                    .and_then(|entry| entry.restore(&self.floating_outputs()))
                    .is_none()
                {
                    self.windows.update(id, |w| w.natural_floating_pending = true);
                }
            }
            rect.width = rule.width.unwrap_or(rect.width).min(bounds.width);
            rect.height = rule.height.unwrap_or(rect.height).min(bounds.height);
            if should_float {
                rect = self.place_floating(
                    window,
                    Some(id),
                    bounds,
                    (rect.width, rect.height),
                    Some((rule.width, rule.height)),
                );
            }

            let result = if should_float != is_floating {
                self.workspaces.toggle_floating(id, rect, axis, 0.5).map(|_| ())
            } else if should_float && (rule.width.is_some() || rule.height.is_some()) {
                self.workspaces.set_floating_rect(id, rect)
            } else {
                Ok(())
            };

            if let Err(error) = result {
                tracing::warn!(%error, ?id, "failed to apply floating window rule");
            }
        }

        if let Some(fullscreen) = rule.fullscreen
            && let Err(error) = self.workspaces.set_fullscreen(id, fullscreen)
        {
            tracing::warn!(%error, ?id, fullscreen, "failed to apply fullscreen window rule");
        }

        if self.workspaces.workspace_for_window(id) != Some(self.workspaces.active_id())
            && self.focused_window == Some(id)
        {
            self.focused_window = self.workspaces.active().last_focused;
        }

        self.relayout();
        self.restore_keyboard_focus();
    }

    pub fn make_window_transient(&mut self, window: &Window, parent: WindowId) {
        let Some(id) = self.windows.ids().get(window).copied() else {
            return;
        };

        if self.workspaces.workspace_for_window(id) != self.workspaces.workspace_for_window(parent) {
            tracing::warn!(?id, ?parent, "ignored transient parent on another workspace");
            return;
        }

        let Some(bounds) = self.floating_bounds_for_window(parent) else {
            return;
        };

        let parent_rect = self.presented_window_rect(parent).unwrap_or(bounds);
        let visible_parent = crate::floating::intersection(parent_rect, bounds).unwrap_or(bounds);
        let size_rect = client_size(window)
            .map(|s| natural_floating_rect(bounds, s))
            .unwrap_or_else(|| centered_transient_rect(visible_parent));
        let rect = self.place_floating(window, Some(id), bounds, (size_rect.width, size_rect.height), None);
        let result = match self.workspaces.placement(id) {
            Some(WindowPlacement::Tiled) => self
                .workspaces
                .toggle_floating(id, rect, Axis::Horizontal, 0.5)
                .map(|_| ()),
            Some(WindowPlacement::Floating { .. }) => self.workspaces.set_floating_rect(id, rect),
            None => return,
        };

        if let Err(error) = result {
            tracing::warn!(%error, ?id, ?parent, "failed to apply transient placement");
            return;
        }

        if self
            .workspaces
            .workspace_for_window(parent)
            .and_then(|workspace| self.workspaces.workspace(workspace))
            .is_some_and(|workspace| workspace.fullscreen == Some(parent))
        {
            self.floating_above_fullscreen.insert(id, parent);
        }

        if self.focused_window == Some(parent) {
            if let Err(error) = self.workspaces.focus_window(id) {
                tracing::warn!(%error, ?id, "failed to focus transient window");
            } else {
                self.focused_window = Some(id);
                self.window_stack.raise(id);
            }
        }

        self.relayout();
        self.restore_keyboard_focus();
    }

    pub fn remove_tiled_window(&mut self, window: &Window) {
        self.update_capture_cursor_privacy();
        self.retain_closed_window(window);
        // Uncommitted toplevels have not entered a workspace yet.
        self.space.unmap_elem(window);
        let Some(id) = self.windows.remove(window) else {
            return;
        };

        self.refresh_capture_privacy();
        self.focus_history.remove(id);
        #[cfg(feature = "resize-metrics")]
        self.resize_metrics
            .end(id, self.presentation_now(), crate::resize_metrics::End::Cancelled);
        self.render.remove_window(id);
        self.capture_render.remove_window(id);
        self.window_stack.remove(id);
        self.floating_above_fullscreen.remove(&id);
        if let Err(error) = self.workspaces.remove_window(id) {
            tracing::error!(%error, ?id, "failed to remove window from layout");
        }

        if self.focused_window == Some(id) {
            self.focused_window = self.workspaces.active().last_focused;
        }

        self.relayout();
    }

    pub(crate) fn capture_resize_before_commit(&mut self, surface: &WlSurface) {
        let Some((window, id)) = self
            .windows
            .ids()
            .iter()
            .find(|(window, _)| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            })
            .map(|(window, id)| (window.clone(), *id))
        else {
            return;
        };

        let Some(transaction) = self.windows.transaction(&id) else {
            return;
        };

        let serial = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|data| data.current_serial))
        });
        if !transaction.accepts(serial) {
            return;
        }

        let Some(output) = self
            .space
            .outputs()
            .find(|output| self.window_belongs_to_output(id, output))
            .cloned()
        else {
            return;
        };

        let source_geometry = transaction.source_geometry().unwrap_or_else(|| window.geometry());
        let scale = output.current_scale().fractional_scale();
        let source_size = source_geometry.size.to_physical_precise_round(scale);
        if self.render.snapshot(&id).is_some_and(|snapshot| {
            crate::presentation::snapshot_covers_source(
                smithay::backend::renderer::Texture::size(&snapshot.texture),
                source_size,
                snapshot.scale,
                scale,
            )
        }) {
            return;
        }

        let used: usize = self.render.snapshots().map(|snapshot| snapshot.bytes()).sum::<usize>()
            + self.render.closing.iter().map(|window| window.bytes()).sum::<usize>();
        let remaining = crate::presentation::SNAPSHOT_BUDGET.saturating_sub(used);
        let result = if let Some(backend) = &self.nested_backend {
            // Commit dispatch does not run inside the Winit event callback;
            // still never risk reentrant renderer access or a compositor panic.
            match backend.try_borrow_mut() {
                Ok(mut backend) => crate::render::capture_resize_snapshot(
                    backend.renderer(),
                    &window,
                    source_geometry,
                    output.current_scale().fractional_scale(),
                    remaining,
                ),
                Err(_) => return,
            }
        } else if let Some(backend) = &mut self.direct_backend {
            backend.capture_resize_snapshot(&window, source_geometry, &output, remaining)
        } else {
            return;
        };
        match result {
            Ok(Some(snapshot)) => {
                tracing::debug!(?id, bytes = snapshot.bytes(), "captured native resize handoff");
                self.render.set_snapshot(id, snapshot);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, ?id, "resize snapshot unavailable; using native crop")
            }
        }
    }

    pub fn record_client_commit(&mut self, window: &Window) {
        let Some(&id) = self.windows.ids().get(window) else {
            return;
        };
        // current_serial is promoted by Smithay on commit, unlike
        // configure_serial, which only records an acknowledgement.
        let committed_serial = window.toplevel().and_then(|toplevel| {
            with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok().and_then(|data| data.current_serial))
            })
        });
        if self
            .windows
            .transaction(&id)
            .is_some_and(|transaction| transaction.accepts(committed_serial))
        {
            #[cfg(feature = "resize-metrics")]
            {
                self.measure_resize_pauses(self.presentation_now());
                self.resize_metrics
                    .end(id, self.presentation_now(), crate::resize_metrics::End::Commit);
            }
            self.windows.clear_transaction(&id);
            self.release_presentation_dependencies();
        }

        let Some(size) = client_size(window) else {
            return;
        };

        if let Some(record) = self.windows.record_mut(id)
            && !record.mapped_once
        {
            record.mapped_once = true;
            if self.animations_enabled {
                record.opening = Some(AnimatedValue {
                    current: 0.0,
                    target: 1.0,
                    velocity: 0.0,
                });
                self.last_animation_tick = Instant::now();
            }
        }

        let fullscreen = self
            .workspaces
            .workspace_for_window(id)
            .and_then(|workspace| self.workspaces.workspace(workspace))
            .is_some_and(|workspace| workspace.fullscreen == Some(id));
        let normal_geometry = !fullscreen
            && !self.windows.record(id).is_some_and(|w| w.maximized)
            && self.windows.geometry(&id).is_none_or(|geometry| !geometry.is_zooming());
        let initial_floating = self
            .windows
            .update(id, |w| std::mem::take(&mut w.natural_floating_pending))
            .unwrap_or(false)
            || self.windows.record(id).is_some_and(|w| w.placement_anchor.is_some());
        if initial_floating
            && normal_geometry
            && matches!(self.workspaces.placement(id), Some(WindowPlacement::Floating { .. }))
            && let Some(bounds) = self.floating_bounds_for_window(id)
        {
            let rect = self.place_floating(
                window,
                Some(id),
                bounds,
                (f64::from(size.width), f64::from(size.height)),
                Some((Some(f64::from(size.width)), Some(f64::from(size.height)))),
            );
            self.windows.update(id, |w| w.placement_anchor = None);
            if self.workspaces.set_floating_rect(id, rect).is_ok() {
                // This is the first mapped client buffer, not a user resize.
                // Do not animate from the temporary placement box.
                self.windows.set_geometry(id, WindowGeometry::new(rect, Some(size)));
                #[cfg(feature = "resize-metrics")]
                self.resize_metrics
                    .end(id, self.presentation_now(), crate::resize_metrics::End::Cancelled);
                self.windows.clear_transaction(&id);
                self.relayout();
            }
        }
        // A normal floating client may choose a different size (minimum sizes,
        // terminal cell grids, or a dialog changing its contents). Its committed
        // window geometry is authoritative once the latest configure is committed.
        // Never let an old buffer undo a newer resize or a fullscreen transition.
        self.windows.update(id, |w| w.placement_anchor = None);
        let settled_configure = window.toplevel().is_some_and(|toplevel| {
            with_states(toplevel.wl_surface(), |states| {
                states.data_map.get::<XdgToplevelSurfaceData>().is_some_and(|data| {
                    let Ok(data) = data.lock() else { return false };
                    floating_commit_is_current(
                        data.pending_configures().is_empty(),
                        data.current_serial,
                        data.configure_serial,
                        data.current.states.contains(xdg_toplevel::State::Resizing),
                    )
                })
            })
        });
        if settled_configure
            && normal_geometry
            && let Some(WindowPlacement::Floating { rect }) = self.workspaces.placement(id)
            && ClientSize::from_rect(rect) != size
        {
            let mut rect = Rect::new(rect.x, rect.y, f64::from(size.width), f64::from(size.height));
            if let Some((left, top, anchor)) = self.windows.record(id).and_then(|w| w.resize_anchor.as_ref()) {
                rect = crate::floating::anchored_size(*anchor, (rect.width, rect.height), *left, *top);
            } else if initial_floating && let Some(work) = self.floating_bounds_for_window(id) {
                rect = crate::floating::clamp_to_work(rect, work);
            }

            if self.workspaces.set_floating_rect(id, rect).is_ok() {
                self.windows.set_geometry(id, WindowGeometry::new(rect, Some(size)));
                #[cfg(feature = "resize-metrics")]
                self.resize_metrics
                    .end(id, self.presentation_now(), crate::resize_metrics::End::Cancelled);
                self.windows.clear_transaction(&id);
                self.render.clear_snapshot(&id);
                self.relayout();
            }
        }

        if settled_configure && self.windows.update(id, |w| w.resize_anchor.take()).flatten().is_some() {
            self.remember_floating(window);
        }

        let Some(geometry) = self.windows.geometry_mut(&id) else {
            return;
        };

        let matches_target = geometry.client.commit(size);
        tracing::debug!(?id, ?size, matches_target, "recorded client geometry commit");
    }

    pub fn close_focused_window(&mut self) {
        if let Some(focused) = self.focused_window {
            self.send_window_close(focused);
        }
    }

    pub(crate) fn close_managed_window(&mut self, id: WindowId) -> bool {
        if self.windows.window(id).is_none() {
            return false;
        }
        self.send_window_close(id);
        true
    }

    pub(super) fn send_window_close(&self, id: WindowId) {
        let Some(toplevel) = self.windows.window(id).and_then(|window| window.toplevel()) else {
            return;
        };

        toplevel.send_close();
    }
}
