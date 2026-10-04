use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, get_role, is_sync_subsurface,
};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::shm::{ShmHandler, ShmState};

use super::{layer_shell, xdg_shell};
use crate::Ferese;
use crate::state::ClientState;

impl CompositorHandler for Ferese {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("all clients have Ferese client state")
            .compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        self.update_surface_preferences(surface);
    }

    fn new_subsurface(&mut self, surface: &WlSurface, _parent: &WlSurface) {
        self.update_surface_preferences(surface);
    }

    fn commit(&mut self, surface: &WlSurface) {
        if get_role(surface) == Some(smithay::wayland::shell::xdg::XDG_TOPLEVEL_ROLE)
            && let Some(window) = self.window_for_surface(surface)
            && let Some(toplevel) = window.toplevel()
            && !xdg_shell::validate_size_constraints(toplevel)
        {
            return;
        }

        self.update_surface_preferences(surface);
        self.capture_resize_before_commit(surface);
        self.capture_close_before_commit(surface);
        on_commit_buffer_handler::<Self>(surface);
        crate::effects::commit_presentation(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            let window = self.window_for_surface(&root);

            if let Some(window) = window {
                window.on_commit();
                if surface == &root {
                    if crate::state::window_has_buffer(&window) {
                        xdg_shell::apply_initial_window_rules(self, &window);
                    } else if self.windows.ids().contains_key(&window) {
                        self.remove_tiled_window(&window);
                        self.space.map_element(window.clone(), (0, 0), false);
                        self.invalidate_window_stacking();
                        self.restore_keyboard_focus();
                    }
                    if window
                        .toplevel()
                        .is_some_and(|toplevel| !xdg_shell::initial_configure_sent(toplevel))
                    {
                        self.restore_initial_floating_size(&window);
                    }
                }
                self.record_client_commit(&window);
            }
        }
        self.refresh_idle_inhibition();

        layer_shell::handle_commit(self, surface);
        xdg_shell::handle_commit(self, surface);
        crate::backends::direct::render_surface(self, surface);
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        if self.session_lock.active() {
            self.session_lock
                .surfaces
                .retain(|_, lock| lock.wl_surface() != surface);
            self.focus_lock_surface();
        }

        crate::backends::direct::render_surface(self, surface);
        if self.idle_inhibitors.remove(surface).is_some() {
            self.refresh_idle_inhibition();
        }
        if matches!(&self.cursor_status, smithay::input::pointer::CursorImageStatus::Surface(cursor) if cursor == surface)
        {
            self.cursor_status = smithay::input::pointer::CursorImageStatus::default_named();
            crate::backends::direct::render_cursor(self);
        }
    }
}

impl BufferHandler for Ferese {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl DmabufHandler for Ferese {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        self.queue_dmabuf_import(dmabuf, notifier);
        crate::backends::direct::render_all(self);
    }
}

impl ShmHandler for Ferese {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
