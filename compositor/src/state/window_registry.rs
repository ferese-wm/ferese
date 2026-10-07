use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use ferese_animation::{AnimatedValue, WindowGeometry};
use ferese_core::WorkspaceId;
use ferese_layout::{ColumnWidth, Rect, WindowId};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::resize_transaction::ResizeTransaction;

/// Metadata with exactly the lifetime of a managed window. Layouts and focus
/// history reference its ID; they do not own or duplicate this metadata.
#[derive(Default)]
pub(crate) struct WindowRecord {
    pub geometry: Option<WindowGeometry>,
    pub surface: Option<WlSurface>,
    pub app_id: String,
    pub title: String,
    pub metadata_revision: u64,
    pub resize: Option<ResizeTransaction>,
    pub focus: Option<AnimatedValue>,
    pub shadow: Option<AnimatedValue>,
    pub focus_transition: Option<crate::focus_effect::BoundedFade>,
    pub opening: Option<AnimatedValue>,
    pub mapped_once: bool,
    pub world_x: Option<(WorkspaceId, AnimatedValue)>,
    pub coupled_width: Option<(WorkspaceId, AnimatedValue)>,
    pub maximized: bool,
    pub maximized_column_width: Option<ColumnWidth>,
    pub natural_floating_pending: bool,
    pub floating_memory: Option<crate::floating::Remembered>,
    pub placement_anchor: Option<Rect>,
    pub resize_anchor: Option<(bool, bool, Rect)>,
    pub column_width_pending: bool,
    pub rules_applied: bool,
}

/// The handle index is read-only outside this owner. Registering and removing a
/// handle always updates its record as well. IDs are never reused on remapping.
pub(crate) struct WindowRegistry<W> {
    ids: HashMap<W, WindowId>,
    by_id: HashMap<WindowId, W>,
    surface_ids: HashMap<WlSurface, WindowId>,
    pub(super) records: HashMap<WindowId, WindowRecord>,
    // Ascending IDs; overview reverses this for front-to-back traversal.
    overview_order: Vec<WindowId>,
    next_id: u64,
}

impl<W> Default for WindowRegistry<W> {
    fn default() -> Self {
        Self {
            ids: HashMap::new(),
            by_id: HashMap::new(),
            surface_ids: HashMap::new(),
            records: HashMap::new(),
            overview_order: Vec::new(),
            next_id: 1,
        }
    }
}

impl<W: Eq + Hash> WindowRegistry<W> {
    pub fn allocate_id(&mut self) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id = self.next_id.checked_add(1).expect("window IDs exhausted");

        id
    }

    pub fn register(&mut self, window: W, id: WindowId)
    where
        W: Clone,
    {
        assert!(!self.ids.contains_key(&window), "window already registered");
        assert!(!self.records.contains_key(&id), "window ID already registered");

        let index = self.overview_order.partition_point(|existing| existing.0 < id.0);
        self.overview_order.insert(index, id);
        self.by_id.insert(id, window.clone());
        self.ids.insert(window, id);
        self.records.insert(id, WindowRecord::default());
    }

    pub fn ids(&self) -> &HashMap<W, WindowId> {
        &self.ids
    }

    /// Overview render and hit order, with the newest ID in front. Normal
    /// stacking changes do not affect this independent presentation order.
    pub fn overview_windows(&self) -> impl DoubleEndedIterator<Item = &W> {
        self.overview_order.iter().rev().filter_map(|id| self.by_id.get(id))
    }

    pub fn window(&self, id: WindowId) -> Option<&W> {
        self.by_id.get(&id)
    }

    pub fn id_for_surface(&self, surface: &WlSurface) -> Option<WindowId> {
        self.surface_ids.get(surface).copied()
    }

    pub fn ordered_ids(&self) -> impl DoubleEndedIterator<Item = WindowId> + '_ {
        self.overview_order.iter().copied()
    }

    pub fn record(&self, id: WindowId) -> Option<&WindowRecord> {
        self.records.get(&id)
    }

    pub fn record_mut(&mut self, id: WindowId) -> Option<&mut WindowRecord> {
        self.records.get_mut(&id)
    }

    pub fn records(&self) -> impl Iterator<Item = (&WindowId, &WindowRecord)> {
        self.records.iter()
    }

    pub fn records_mut(&mut self) -> impl Iterator<Item = (&WindowId, &mut WindowRecord)> {
        self.records.iter_mut()
    }

    pub fn take_world_positions(&mut self) -> HashMap<WindowId, (WorkspaceId, AnimatedValue)> {
        self.records
            .iter_mut()
            .filter_map(|(id, record)| Some((*id, record.world_x.take()?)))
            .collect()
    }

    pub fn geometry(&self, id: &WindowId) -> Option<&WindowGeometry> {
        self.records.get(id)?.geometry.as_ref()
    }

    pub fn geometry_mut(&mut self, id: &WindowId) -> Option<&mut WindowGeometry> {
        self.records.get_mut(id)?.geometry.as_mut()
    }

    pub fn geometries(&self) -> impl Iterator<Item = (&WindowId, &WindowGeometry)> {
        self.records
            .iter()
            .filter_map(|(id, record)| Some((id, record.geometry.as_ref()?)))
    }

    pub fn set_geometry(&mut self, id: WindowId, geometry: WindowGeometry) {
        self.update(id, |record| record.geometry = Some(geometry));
    }

    pub fn transaction(&self, id: &WindowId) -> Option<&ResizeTransaction> {
        self.records.get(id)?.resize.as_ref()
    }

    pub fn set_transaction(&mut self, id: WindowId, transaction: ResizeTransaction) {
        self.update(id, |record| record.resize = Some(transaction));
    }

    pub fn clear_transaction(&mut self, id: &WindowId) {
        self.update(*id, |record| record.resize = None);
    }

    pub fn resizing(&self) -> impl Iterator<Item = &WindowId> {
        self.records
            .iter()
            .filter_map(|(id, record)| record.resize.is_some().then_some(id))
    }

    pub fn expire_transactions(&mut self, now: std::time::Duration) {
        for (id, record) in &mut self.records {
            if record.resize.is_some_and(|transaction| transaction.expired(now)) {
                tracing::warn!(?id, "resize presentation deadline reached");
                record.resize = None;
            }
        }
    }

    pub fn update<R>(&mut self, id: WindowId, update: impl FnOnce(&mut WindowRecord) -> R) -> Option<R> {
        self.records.get_mut(&id).map(update)
    }

    pub fn remove(&mut self, window: &W) -> Option<WindowId> {
        let id = self.ids.remove(window)?;
        self.by_id.remove(&id);
        if let Some(record) = self.records.remove(&id)
            && let Some(surface) = record.surface
        {
            self.surface_ids.remove(&surface);
        }
        let index = self
            .overview_order
            .binary_search_by_key(&id.0, |id| id.0)
            .expect("registered windows have an overview entry");
        self.overview_order.remove(index);

        Some(id)
    }

    pub fn take_column_width_requests(&mut self) -> HashSet<WindowId> {
        self.records
            .iter_mut()
            .filter_map(|(id, record)| std::mem::take(&mut record.column_width_pending).then_some(*id))
            .collect()
    }
}

impl WindowRegistry<Window> {
    pub fn register_window(&mut self, window: Window, id: WindowId) {
        let surface = window.toplevel().map(|toplevel| toplevel.wl_surface().clone());
        self.register(window, id);

        if let Some(surface) = surface {
            self.surface_ids.insert(surface.clone(), id);
            self.records.get_mut(&id).unwrap().surface = Some(surface.clone());
            self.refresh_metadata(&surface);
        }
    }

    pub fn refresh_metadata(&mut self, surface: &WlSurface) -> bool {
        let Some(id) = self.id_for_surface(surface) else {
            return false;
        };
        let record = self.records.get_mut(&id).unwrap();
        with_states(surface, |states| {
            let attributes = states.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap();
            let app_id = attributes.app_id.as_deref().unwrap_or_default();
            let title = attributes.title.as_deref().unwrap_or_default();
            let changed = record.app_id != app_id || record.title != title;

            if changed {
                record.app_id.clear();
                record.app_id.push_str(app_id);
                record.title.clear();
                record.title.push_str(title);
                record.metadata_revision = record.metadata_revision.wrapping_add(1);
            }

            changed
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_order_tracks_registration_removal_and_remapping() {
        let mut windows = WindowRegistry::default();
        let first = windows.allocate_id();
        let second = windows.allocate_id();
        let third = windows.allocate_id();
        // Registration need not happen in allocation order.
        windows.register("second", second);
        windows.register("first", first);
        windows.register("third", third);
        assert_eq!(
            windows.overview_windows().copied().collect::<Vec<_>>(),
            ["third", "second", "first"]
        );

        windows.remove(&"second");
        assert_eq!(
            windows.overview_windows().copied().collect::<Vec<_>>(),
            ["third", "first"]
        );
        windows.remove(&"first");
        let remapped = windows.allocate_id();
        windows.register("first", remapped);
        assert_eq!(
            windows.overview_windows().copied().collect::<Vec<_>>(),
            ["first", "third"]
        );

        windows.remove(&"first");
        windows.remove(&"third");
        assert_eq!(windows.overview_windows().count(), 0);
    }

    #[test]
    fn overview_iteration_borrows_handles_and_removal_releases_cached_handles() {
        let handle = std::rc::Rc::new("surface");
        let mut windows = WindowRegistry::default();
        let id = windows.allocate_id();
        windows.register(handle.clone(), id);
        assert_eq!(std::rc::Rc::strong_count(&handle), 3);

        for _ in 0..100 {
            assert!(std::rc::Rc::ptr_eq(windows.overview_windows().next().unwrap(), &handle));
            assert_eq!(std::rc::Rc::strong_count(&handle), 3);
        }

        windows.remove(&handle);
        assert_eq!(std::rc::Rc::strong_count(&handle), 1);
    }

    #[test]
    fn unmap_drops_metadata_and_late_updates_cannot_resurrect_it() {
        let mut windows = WindowRegistry::default();
        let id = windows.allocate_id();
        windows.register("surface", id);
        windows.update(id, |record| {
            record.maximized = true;
            record.resize_anchor = Some((true, false, Rect::new(0., 0., 800., 600.)));
            record.column_width_pending = true;
            record.rules_applied = true;
            record.focus = Some(AnimatedValue::new(1.0));
            record.opening = Some(AnimatedValue::new(0.0));
            record.world_x = Some((WorkspaceId(1), AnimatedValue::new(50.0)));
            record.coupled_width = Some((WorkspaceId(1), AnimatedValue::new(800.0)));
        });
        let geometry = WindowGeometry::new(Rect::new(0., 0., 800., 600.), None);
        let resize = ResizeTransaction::new(12.into(), std::time::Duration::ZERO);
        windows.set_geometry(id, geometry);
        windows.set_transaction(id, resize);

        assert_eq!(windows.remove(&"surface"), Some(id));
        assert_eq!(windows.remove(&"surface"), None);
        assert!(windows.record(id).is_none());
        assert!(windows.update(id, |record| record.maximized = true).is_none());

        windows.set_geometry(id, geometry);
        windows.set_transaction(id, resize);

        assert!(windows.geometry(&id).is_none());
        assert!(windows.transaction(&id).is_none());
        assert_eq!(windows.resizing().count(), 0);
        assert!(windows.ids().is_empty());
        assert!(windows.take_column_width_requests().is_empty());
    }

    #[test]
    fn remap_has_new_identity_and_does_not_inherit_old_requests() {
        let mut windows = WindowRegistry::default();
        let old = windows.allocate_id();
        windows.register("surface", old);
        windows.update(old, |record| record.rules_applied = true);
        windows.remove(&"surface");

        let new = windows.allocate_id();
        windows.register("surface", new);
        assert_ne!(old, new);
        assert!(
            windows
                .update(old, |record| record.column_width_pending = true)
                .is_none()
        );
        assert!(!windows.record(new).unwrap().rules_applied);
        assert!(windows.take_column_width_requests().is_empty());
    }

    #[test]
    fn pending_width_changes_are_consumed_once_and_survive_other_window_removal() {
        let mut windows = WindowRegistry::default();
        let first = windows.allocate_id();
        let second = windows.allocate_id();
        windows.register("first", first);
        windows.register("second", second);
        windows.update(first, |record| record.column_width_pending = true);
        windows.update(second, |record| record.column_width_pending = true);

        windows.remove(&"first");

        assert_eq!(windows.take_column_width_requests(), HashSet::from([second]));
        assert!(windows.take_column_width_requests().is_empty());
        assert!(windows.record(second).is_some());
    }

    #[test]
    fn expired_resize_releases_only_its_barrier_and_preserves_geometry() {
        use std::time::Duration;

        let mut windows = WindowRegistry::default();
        let first = windows.allocate_id();
        let second = windows.allocate_id();
        windows.register("first", first);
        windows.register("second", second);
        let geometry = WindowGeometry::new(Rect::new(0., 0., 800., 600.), None);
        windows.set_geometry(first, geometry);
        windows.set_transaction(first, ResizeTransaction::new(1.into(), Duration::ZERO));
        windows.set_transaction(second, ResizeTransaction::new(2.into(), Duration::from_millis(100)));

        windows.expire_transactions(Duration::from_millis(300));

        assert_eq!(windows.geometry(&first), Some(&geometry));
        assert!(windows.transaction(&first).is_none());
        assert!(windows.transaction(&second).unwrap().accepts(Some(2.into())));
        assert_eq!(windows.resizing().copied().collect::<Vec<_>>(), vec![second]);
    }
}
