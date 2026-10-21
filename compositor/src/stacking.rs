use std::collections::HashMap;

use ferese_layout::WindowId;

pub(crate) fn layer_priority(
    floating: bool,
    zooming: bool,
    fullscreen: bool,
    above_fullscreen: bool,
) -> u8 {
    if above_fullscreen {
        4
    } else if fullscreen {
        3
    } else if floating {
        2
    } else if zooming {
        1
    } else {
        0
    }
}

/// Persistent back-to-front order. Geometry updates never change this order.
#[derive(Default)]
pub(crate) struct WindowStack {
    order: Vec<WindowId>,
    ranks: HashMap<WindowId, usize>,
    revision: u64,
}

impl WindowStack {
    pub fn insert(&mut self, id: WindowId) {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.ranks.entry(id) {
            entry.insert(self.order.len());
            self.order.push(id);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn remove(&mut self, id: WindowId) {
        if let Some(rank) = self.ranks.remove(&id) {
            self.order.remove(rank);
            self.revision = self.revision.wrapping_add(1);
            for (rank, candidate) in self.order.iter().enumerate().skip(rank) {
                self.ranks.insert(*candidate, rank);
            }
        }
    }

    pub fn raise(&mut self, id: WindowId) {
        if self.order.last() == Some(&id) {
            return;
        }

        self.remove(id);
        self.insert(id);
    }

    pub fn rank(&self, id: WindowId) -> usize {
        self.ranks.get(&id).copied().unwrap_or(usize::MAX)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// Desired order survives coordinate-only remaps, which Smithay also raises.
pub(crate) struct StackingCache<W> {
    order: Vec<W>,
    revision: Option<u64>,
    dirty: bool,
    remapped: bool,
}

impl<W> Default for StackingCache<W> {
    fn default() -> Self {
        Self {
            order: Vec::new(),
            revision: None,
            dirty: true,
            remapped: true,
        }
    }
}

impl<W> StackingCache<W> {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn remapped(&mut self) {
        self.remapped = true;
    }

    pub fn needs_rebuild(&self, revision: u64, mapped: usize) -> bool {
        self.dirty || self.revision != Some(revision) || self.order.len() != mapped
    }

    pub fn needs_restore(&self) -> bool {
        self.remapped
    }

    pub fn replace(&mut self, order: Vec<W>, revision: u64) {
        self.order = order;
        self.revision = Some(revision);
        self.dirty = false;
        self.remapped = true;
    }

    pub fn order(&self) -> &[W] {
        &self.order
    }

    pub fn restored(&mut self) {
        self.remapped = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_stacking_and_coordinate_remaps_do_not_require_resorting() {
        let mut cache = StackingCache::default();
        assert!(cache.needs_rebuild(1, 2));
        cache.replace(vec!["first", "second"], 1);
        cache.restored();
        let storage = cache.order().as_ptr();

        for _ in 0..100 {
            assert!(!cache.needs_rebuild(1, 2));
            assert!(!cache.needs_restore());
            assert_eq!(cache.order().as_ptr(), storage);
        }

        cache.remapped();
        assert!(!cache.needs_rebuild(1, 2));
        assert!(cache.needs_restore());
        assert_eq!(cache.order(), ["first", "second"]);
        cache.restored();
        assert!(cache.needs_rebuild(2, 2));
        assert!(cache.needs_rebuild(1, 1));
        cache.invalidate();
        assert!(cache.needs_rebuild(1, 2));
    }

    #[test]
    fn stack_revision_changes_only_when_membership_or_rank_changes() {
        let mut stack = WindowStack::default();
        stack.insert(WindowId(1));
        let first = stack.revision();
        stack.insert(WindowId(1));
        stack.raise(WindowId(1));
        stack.remove(WindowId(9));
        assert_eq!(stack.revision(), first);

        stack.insert(WindowId(2));
        let second = stack.revision();
        assert_ne!(second, first);
        stack.raise(WindowId(1));
        assert_ne!(stack.revision(), second);
        let raised = stack.revision();
        stack.remove(WindowId(1));
        assert_ne!(stack.revision(), raised);
    }

    #[test]
    fn cached_ranks_match_reference_order_after_mixed_operations() {
        let mut stack = WindowStack::default();
        let mut reference = Vec::new();
        let mut random = 42_u64;
        for _ in 0..2000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let id = WindowId((random >> 32) % 32);
            match random % 3 {
                0 => {
                    stack.insert(id);
                    if !reference.contains(&id) {
                        reference.push(id);
                    }
                }
                1 => {
                    stack.remove(id);
                    reference.retain(|candidate| *candidate != id);
                }
                _ => {
                    stack.raise(id);
                    reference.retain(|candidate| *candidate != id);
                    reference.push(id);
                }
            }
            for id in (0..33).map(WindowId) {
                assert_eq!(
                    stack.rank(id),
                    reference
                        .iter()
                        .position(|candidate| *candidate == id)
                        .unwrap_or(usize::MAX)
                );
            }
        }
    }

    #[test]
    fn floating_windows_stay_above_tiled_and_maximized_windows() {
        assert!(
            layer_priority(true, false, false, false) > layer_priority(false, true, false, false)
        );
        assert!(
            layer_priority(false, true, false, false) > layer_priority(false, false, false, false)
        );
        assert!(
            layer_priority(false, false, true, false) > layer_priority(true, false, false, false)
        );
        assert!(
            layer_priority(true, false, false, true) > layer_priority(false, false, true, false)
        );
    }

    #[test]
    fn remapping_neighbours_does_not_change_order() {
        let mut stack = WindowStack::default();
        stack.insert(WindowId(1));
        stack.insert(WindowId(2));
        stack.raise(WindowId(1));
        stack.insert(WindowId(2));
        assert!(stack.rank(WindowId(1)) > stack.rank(WindowId(2)));
        stack.remove(WindowId(1));
        assert_eq!(stack.rank(WindowId(2)), 0);
    }
}
