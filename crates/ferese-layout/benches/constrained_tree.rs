//! Run with `cargo bench --locked -p ferese-layout --bench constrained_tree`.
//! Report solver cost separately from insertion and debug tree validation.
use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;

use ferese_layout::{Axis, GapConfig, LayoutTree, Rect, SizeConstraints, WindowId};

fn main() {
    println!("windows,case,iterations,ns_per_solve");
    for count in [32, 128, 512, 2048] {
        let mut tree = LayoutTree::default();
        for id in 1..=count {
            tree.insert(
                WindowId(id),
                (id > 1).then_some(WindowId(1)),
                Axis::Horizontal,
                0.5,
            )
            .unwrap();
        }
        for (case, width) in [("feasible", count as f64 * 20.0), ("impossible", 100.0)] {
            let constraints = (1..=count)
                .map(|id| {
                    (
                        WindowId(id),
                        SizeConstraints {
                            min_width: 10.0,
                            ..SizeConstraints::default()
                        },
                    )
                })
                .collect::<HashMap<_, _>>();
            let iterations = (8192 / count).max(16);
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(
                    tree.geometry_with_constraints(
                        black_box(Rect::new(0.0, 0.0, width, 1080.0)),
                        GapConfig {
                            inner: 0.0,
                            outer: 0.0,
                            smart: false,
                        },
                        black_box(&constraints),
                        Some(WindowId(1)),
                    )
                    .unwrap(),
                );
            }
            println!(
                "{count},{case},{iterations},{}",
                start.elapsed().as_nanos() / iterations as u128
            );
        }
    }
}
