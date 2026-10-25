//! Tree and scrolling layouts share window identities, constraints and geometry.
mod layout_tree;
mod scrolling;

pub use layout_tree::*;
pub use scrolling::{
    Column, ColumnDisplay, ColumnWidth, ScrollingLayout, ViewportFocusStrategy, ViewportTarget,
};

/// The destination of an interactive tiled-window drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropPlacement {
    Left,
    Right,
    Above,
    Below,
    Tab,
}

#[cfg(test)]
mod pointer_tests {
    use super::*;

    #[test]
    fn pointer_tree_drop_preserves_windows_and_is_atomic_for_unknown_targets() {
        for placement in [
            DropPlacement::Left,
            DropPlacement::Right,
            DropPlacement::Above,
            DropPlacement::Below,
            DropPlacement::Tab,
        ] {
            let mut tree = LayoutTree::default();
            for id in 1..=4 {
                tree.insert(WindowId(id), None, Axis::Horizontal, 0.5)
                    .unwrap();
            }
            tree.place_window(WindowId(1), WindowId(3), placement)
                .unwrap();
            tree.validate().unwrap();
            assert_eq!(tree.window_ids().count(), 4);
            let before = tree.clone();
            assert!(
                tree.place_window(WindowId(1), WindowId(99), placement)
                    .is_err()
            );
            assert_eq!(tree, before);
            tree.place_window(WindowId(1), WindowId(1), placement)
                .unwrap();
            assert_eq!(tree, before);
        }
    }

    #[test]
    fn pointer_scrolling_drop_preserves_window_and_row_invariants() {
        for placement in [
            DropPlacement::Left,
            DropPlacement::Right,
            DropPlacement::Above,
            DropPlacement::Below,
            DropPlacement::Tab,
        ] {
            let mut layout = ScrollingLayout::default();
            for id in 1..=4 {
                layout.insert(WindowId(id), None).unwrap();
            }
            layout
                .place_window(WindowId(1), WindowId(3), placement)
                .unwrap();
            layout.validate().unwrap();
            assert_eq!(layout.window_ids().count(), 4);
            let column = layout
                .columns()
                .iter()
                .find(|column| column.windows.contains(&WindowId(1)))
                .unwrap();
            if placement == DropPlacement::Tab {
                assert_eq!(column.display, ColumnDisplay::Tabbed);
                assert_eq!(column.windows.len(), 2);
            }
            let before = layout.clone();
            assert!(
                layout
                    .place_window(WindowId(1), WindowId(99), placement)
                    .is_err()
            );
            assert_eq!(layout, before);
        }
    }

    #[test]
    fn pointer_tree_resize_moves_the_selected_boundary_in_both_directions() {
        let mut tree = LayoutTree::default();
        tree.insert(WindowId(1), None, Axis::Horizontal, 0.5)
            .unwrap();
        tree.insert(WindowId(2), Some(WindowId(1)), Axis::Horizontal, 0.5)
            .unwrap();
        let bounds = Rect::new(-1000.0, -500.0, 1000.0, 600.0);
        let before = tree.geometry(bounds).unwrap();
        assert!(
            tree.resize_edge(WindowId(1), Direction::Right, 100.0, bounds)
                .unwrap()
        );
        let after = tree.geometry(bounds).unwrap();
        assert_eq!(
            after[&WindowId(1)].width,
            before[&WindowId(1)].width + 100.0
        );
        assert_eq!(
            after[&WindowId(2)].width,
            before[&WindowId(2)].width - 100.0
        );
        assert!(
            tree.resize_edge(WindowId(2), Direction::Left, -100.0, bounds)
                .unwrap()
        );
        assert_eq!(tree.geometry(bounds).unwrap(), before);
        assert!(
            !tree
                .resize_edge(WindowId(1), Direction::Left, 100.0, bounds)
                .unwrap()
        );
        assert!(
            !tree
                .resize_edge(WindowId(1), Direction::Right, f64::NAN, bounds)
                .unwrap()
        );
    }
}

#[cfg(test)]
mod pointer_sequence_tests {
    use super::*;

    #[test]
    fn repeated_pointer_drops_and_resizes_keep_tabbed_layouts_valid() {
        let mut tree = LayoutTree::default();
        let mut scrolling = ScrollingLayout::default();
        for id in 1..=8 {
            tree.insert(WindowId(id), None, Axis::Horizontal, 0.5)
                .unwrap();
            scrolling.insert(WindowId(id), None).unwrap();
        }
        let bounds = Rect::new(-1200.0, -300.0, 1200.0, 800.0);
        let mut seed = 42_u64;
        for _ in 0..1000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let source = WindowId(1 + (seed >> 32) % 8);
            let target = WindowId(1 + (seed >> 40) % 8);
            let placement = [
                DropPlacement::Left,
                DropPlacement::Right,
                DropPlacement::Above,
                DropPlacement::Below,
                DropPlacement::Tab,
            ][(seed >> 48) as usize % 5];
            tree.place_window(source, target, placement).unwrap();
            scrolling.place_window(source, target, placement).unwrap();
            let edge = [
                Direction::Left,
                Direction::Right,
                Direction::Up,
                Direction::Down,
            ][(seed >> 52) as usize % 4];
            let delta = (seed >> 16) as i16 as f64;
            tree.resize_edge(source, edge, delta, bounds).unwrap();
            scrolling.resize_edge(source, edge, delta, bounds).unwrap();
            tree.validate().unwrap();
            scrolling.validate().unwrap();
            assert_eq!(tree.window_ids().count(), 8);
            assert_eq!(scrolling.window_ids().count(), 8);
            for id in 1..=8 {
                assert!(tree.contains(WindowId(id)));
                assert!(scrolling.contains(WindowId(id)));
            }
        }
    }
}
