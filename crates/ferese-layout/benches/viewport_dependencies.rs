//! Run with `cargo bench --locked -p ferese-layout --bench viewport_dependencies`.
use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;

use ferese_layout::{
    ColumnWidth, GapConfig, Rect, ScrollingLayout, ViewportFocusStrategy, WindowId,
};

fn main() {
    println!("windows,pending,strategy,iterations,ns_per_replay");
    for count in [8, 32, 96, 256] {
        for strategy in [ViewportFocusStrategy::Center, ViewportFocusStrategy::Paged] {
            let mut layout = ScrollingLayout::with_default_width(ColumnWidth::Fixed(240.));
            layout.set_focus_strategy(strategy);
            for id in 1..=count {
                layout
                    .insert(WindowId(id), (id > 1).then_some(WindowId(id - 1)))
                    .unwrap();
            }
            layout
                .resolve_geometry_with_constraints(
                    Rect::new(0., 0., 1600., 1000.),
                    GapConfig {
                        inner: 8.,
                        outer: 0.,
                        smart: false,
                    },
                    &HashMap::new(),
                    Some(WindowId(count)),
                )
                .unwrap();
            let target = layout.viewport_target().unwrap();
            for pending in [1, count / 2, count] {
                let widths = (1..=pending)
                    .map(|id| (WindowId(id), 180., 240.))
                    .collect::<Vec<_>>();
                let iterations = (200_000 / count).max(100);
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(target.with_held_widths(black_box(&widths), None));
                }
                println!(
                    "{count},{pending},{strategy:?},{iterations},{}",
                    start.elapsed().as_nanos() / u128::from(iterations)
                );
            }
        }
    }
}
