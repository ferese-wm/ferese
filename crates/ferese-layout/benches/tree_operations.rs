//! Run with `cargo bench --locked -p ferese-layout --bench tree_operations`.
use ferese_layout::{Axis, LayoutTree, Rect, WindowId};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    println!("windows,shape,operation,iterations,ns_per_operation");
    for count in [32, 128, 512, 2048, 4096] {
        for shape in ["balanced", "skewed"] {
            let mut tree = LayoutTree::default();
            for id in 1..=count {
                let target = if shape == "balanced" { id / 2 } else { 1 };
                tree.insert(
                    WindowId(id),
                    (id > 1).then_some(WindowId(target)),
                    if id % 2 == 0 {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    },
                    0.5,
                )
                .unwrap();
            }
            let bounds = Rect::new(0.0, 0.0, 1920.0, 1080.0);
            let iterations = (262144 / count).max(64);
            for (case, target) in [
                ("shallow", 2),
                ("deep", if shape == "balanced" { count } else { 1 }),
            ] {
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(
                        tree.automatic_axis(Some(black_box(WindowId(target))), black_box(bounds))
                            .unwrap(),
                    );
                }
                println!(
                    "{count},{shape},automatic_axis_{case},{iterations},{}",
                    start.elapsed().as_nanos() / u128::from(iterations)
                );
            }
        }
    }
}
