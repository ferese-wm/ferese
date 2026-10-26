use super::*;

fn skewed_tree(windows: u64) -> LayoutTree {
    let mut tree = LayoutTree::default();
    let first = tree.insert_node(Node::Window(WindowId(1)));
    tree.windows.insert(WindowId(1), first);
    tree.root = Some(first);
    for id in 2..=windows {
        let previous = tree.root.unwrap();
        let leaf = tree.insert_node(Node::Window(WindowId(id)));
        tree.windows.insert(WindowId(id), leaf);
        let parent = tree.insert_node(Node::Split {
            axis: Axis::Horizontal,
            ratio: 0.5,
            first: previous,
            second: leaf,
        });
        tree.parents.insert(previous, parent);
        tree.parents.insert(leaf, parent);
        tree.root = Some(parent);
    }
    tree
}

#[test]
fn deep_tree_public_operations_use_bounded_call_stack() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let mut tree = skewed_tree(4096);
            let bounds = Rect::new(-100.0, 40.0, 8192.0, 1080.0);
            tree.validate().unwrap();
            assert_eq!(tree.visible_window_count(), 4096);
            assert_eq!(tree.preferred_window(), Some(WindowId(1)));
            assert_eq!(
                tree.window_ids_in_reading_order(bounds).unwrap().len(),
                4096
            );
            assert_eq!(tree.geometry(bounds).unwrap().len(), 4096);
            tree.automatic_axis(None, bounds).unwrap();
            tree.directional_neighbor(WindowId(4096), Direction::Left, bounds)
                .unwrap();
            tree.insert(WindowId(4097), None, Axis::Vertical, 0.5)
                .unwrap();
            tree.resize_window(WindowId(1), Direction::Down, 0.1)
                .unwrap();
            tree.resize_edge(WindowId(1), Direction::Down, 1.0, bounds)
                .unwrap();
            tree.move_window(WindowId(4096), Direction::Left, bounds)
                .unwrap();
            tree.stack_window(WindowId(4097), WindowId(1)).unwrap();
            assert_eq!(
                tree.automatic_axis(Some(WindowId(1)), bounds),
                Err(LayoutError::UnknownWindow(WindowId(1)))
            );
            tree.activate_window(WindowId(1)).unwrap();
            tree.automatic_axis(Some(WindowId(1)), bounds).unwrap();
            assert!(tree.leave_tab_group(WindowId(4097)).unwrap());
            tree.place_window(WindowId(4097), WindowId(4096), crate::DropPlacement::Left)
                .unwrap();
            tree.remove(WindowId(4097)).unwrap();
            tree.remove(WindowId(1)).unwrap();
            tree.validate().unwrap();
            assert_eq!(tree.visible_window_count(), 4095);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn indexed_window_bounds_match_geometry_across_tree_edits_and_hidden_tabs() {
    let mut tree = LayoutTree::default();
    let bounds = Rect::new(-171.5, 28.0, 1920.5, 1080.25);
    let mut random = 0x4219_fadd_1713_9201_u64;
    for id in 1..=48 {
        tree.insert(
            WindowId(id),
            (id > 1).then_some(WindowId(id / 2)),
            if id % 2 == 0 {
                Axis::Horizontal
            } else {
                Axis::Vertical
            },
            0.3,
        )
        .unwrap();
    }
    for step in 0..300 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let a = WindowId(random % 48 + 1);
        let b = WindowId((random >> 16) % 48 + 1);
        match step % 4 {
            0 => tree.stack_window(a, b).unwrap(),
            1 => {
                tree.leave_tab_group(a).unwrap();
            }
            2 => tree.activate_window(a).unwrap(),
            _ => {
                tree.resize_window(a, Direction::Right, 0.1).unwrap();
            }
        }
        tree.validate().unwrap();
        let visible = tree.geometry(bounds).unwrap();
        assert_eq!(tree.visible_window_count(), visible.len());
        for window in tree.window_ids() {
            if let Some(rect) = visible.get(&window) {
                assert_eq!(tree.window_bounds(window, bounds).unwrap(), *rect);
                assert_eq!(
                    tree.automatic_axis(Some(window), bounds).unwrap(),
                    if rect.width > rect.height {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    }
                );
            } else {
                assert_eq!(
                    tree.window_bounds(window, bounds),
                    Err(LayoutError::UnknownWindow(window))
                );
            }
        }
    }
}

#[test]
fn validation_rejects_extra_parent_entries_and_cycles_without_recursion() {
    let mut tree = skewed_tree(8);
    tree.parents.insert(NodeId(1000), tree.root.unwrap());
    assert_eq!(
        tree.validate(),
        Err(LayoutError::InvalidTree("parent index is inconsistent"))
    );
    tree.parents.remove(&NodeId(1000));
    let root = tree.root.unwrap();
    if let Some(Node::Split { first, .. }) = tree.nodes.get_mut(&root) {
        *first = root;
    }
    assert_eq!(
        tree.validate(),
        Err(LayoutError::InvalidTree("tree contains a cycle"))
    );
    assert_eq!(
        tree.geometry(Rect::default()),
        Err(LayoutError::InvalidTree("tree contains a cycle"))
    );
    assert_eq!(
        tree.first_window(root),
        Err(LayoutError::InvalidTree("tree contains a cycle"))
    );
}
