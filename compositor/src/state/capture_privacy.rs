use smithay::desktop::Window;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::Ferese;

impl Ferese {
    pub(crate) fn has_capture_exclusions(&self) -> bool {
        // Detached close snapshots outlive the registry's privacy metadata.
        // Keep them on the display and use the filtered scene for captures.
        !self.capture_protected_windows.is_empty()
            || self.capture_protected_cursor.is_some()
            || !self.render.closing.is_empty()
    }

    /// Resolve at capture time: metadata changes must not wait for placement rules.
    /// Transient children inherit protection, including children on other outputs.
    pub(crate) fn capture_protected(&self, window: &Window) -> bool {
        let Some(toplevel) = window.toplevel() else {
            return true;
        };
        let mut surface = Some(toplevel.wl_surface().clone());
        let mut visited = Vec::new();
        while let Some(current) = surface {
            if visited.contains(&current) {
                return true;
            }
            visited.push(current.clone());
            let (blocked, parent) = with_states(&current, |states| {
                let Some(data) = states.data_map.get::<XdgToplevelSurfaceData>() else {
                    return (true, None);
                };
                let data = data.lock().unwrap();
                let rule = crate::window_rules::resolve(
                    &self.window_rules,
                    data.app_id.as_deref(),
                    data.title.as_deref(),
                    data.parent.is_some(),
                );
                (rule.block_out_from_screencasts == Some(true), data.parent.clone())
            });
            if blocked {
                return true;
            }
            surface = parent;
        }
        false
    }

    // Called from cursor_image while the pointer mutex is held: do not query
    // pointer focus here. A cursor surface can outlive its protected window.
    pub(crate) fn update_capture_cursor_privacy(&mut self) {
        use smithay::input::pointer::CursorImageStatus;
        use smithay::reexports::wayland_server::Resource;
        let CursorImageStatus::Surface(cursor) = &self.cursor_status else {
            self.capture_protected_cursor = None;
            return;
        };
        if self.capture_protected_cursor.as_ref() == Some(cursor) {
            return;
        }
        let protected = self.windows.ids().iter().any(|(window, id)| {
            window
                .toplevel()
                .is_some_and(|top| top.wl_surface().id().same_client_as(&cursor.id()))
                && (self.capture_protected_windows.contains(id) || self.capture_protected(window))
        });
        self.capture_protected_cursor = protected.then(|| cursor.clone());
    }

    pub(crate) fn capture_cursor_protected(&self) -> bool {
        use smithay::input::pointer::CursorImageStatus;
        use smithay::reexports::wayland_server::Resource;
        use smithay::wayland::compositor::get_parent;
        // A cursor image is supplied by a client and can outlive pointer focus.
        // Conservatively conceal that client's custom cursor while any of its
        // windows is protected, including popup grabs outside the parent rect.
        if let CursorImageStatus::Surface(cursor) = &self.cursor_status {
            return self.capture_protected_cursor.as_ref() == Some(cursor)
                || self.windows.ids().keys().any(|window| {
                    window
                        .toplevel()
                        .is_some_and(|top| top.wl_surface().id().same_client_as(&cursor.id()))
                        && self.capture_protected(window)
                });
        }
        let Some(mut focus) = self.seat.get_pointer().and_then(|pointer| pointer.current_focus()) else {
            return false;
        };
        while let Some(parent) = get_parent(&focus) {
            focus = parent;
        }
        if let Some(popup) = self.popups.find_popup(&focus)
            && let Ok(root) = smithay::desktop::find_popup_root_surface(&popup)
        {
            focus = root;
        }
        self.windows
            .id_for_surface(&focus)
            .and_then(|id| self.windows.window(id))
            .is_some_and(|window| self.capture_protected(window))
    }

    pub(crate) fn capture_window_allowed(&self, id: ferese_layout::WindowId, epoch: u64) -> bool {
        epoch == self.capture_epoch
            && self
                .windows
                .window(id)
                .is_some_and(|window| !self.capture_protected(window))
    }

    /// Deferred readbacks and encoders must not publish across a policy change,
    /// even if a subsequent change restores the original policy.
    pub(crate) fn refresh_capture_privacy(&mut self) {
        self.update_capture_cursor_privacy();
        let protected = self
            .windows
            .ids()
            .iter()
            .filter(|(window, _)| self.capture_protected(window))
            .map(|(_, id)| *id)
            .collect();
        if self.capture_protected_windows == protected {
            return;
        }
        self.capture_protected_windows = protected;
        self.update_capture_cursor_privacy();
        self.capture_epoch = self.capture_epoch.wrapping_add(1);
        self.screenshot
            .terminate_all_with_reason("Screenshot cancelled: capture privacy changed");
        // Drop cached blur pixels as well as their damage history.
        self.capture_render = Default::default();
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            self.defer_output_redraw(output);
        }
    }
}

#[cfg(test)]
#[path = "capture_privacy_tests.rs"]
pub(super) mod tests;
