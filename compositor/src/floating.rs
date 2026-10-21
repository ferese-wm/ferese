//! Floating placement and snapping in logical output coordinates.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use ferese_layout::Rect;
use serde::{Deserialize, Serialize};

pub const SNAP_DISTANCE: f64 = 10.0;
const RELEASE_DISTANCE: f64 = 20.0;
pub const CASCADE_STEP: f64 = 32.0; // CSD titlebars have no protocol-supplied height.

pub fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let width = (a.x + a.width).min(b.x + b.width) - x;
    let height = (a.y + a.height).min(b.y + b.height) - y;
    (width > 0.0 && height > 0.0).then_some(Rect::new(x, y, width, height))
}

pub fn clamp_to_work(mut rect: Rect, work: Rect) -> Rect {
    // Preserve client minimum sizes even when the work area is smaller.
    rect.x = rect
        .x
        .clamp(work.x, work.x + (work.width - rect.width).max(0.0));
    rect.y = rect
        .y
        .clamp(work.y, work.y + (work.height - rect.height).max(0.0));
    rect
}

pub fn centered(size: (f64, f64), anchor: Rect) -> Rect {
    Rect::new(
        anchor.x + (anchor.width - size.0) / 2.0,
        anchor.y + (anchor.height - size.1) / 2.0,
        size.0,
        size.1,
    )
}

pub fn anchored_size(rect: Rect, size: (f64, f64), left: bool, top: bool) -> Rect {
    Rect::new(
        if left {
            rect.x + rect.width - size.0
        } else {
            rect.x
        },
        if top {
            rect.y + rect.height - size.1
        } else {
            rect.y
        },
        size.0,
        size.1,
    )
}

pub fn min_overlap(
    size: (f64, f64),
    work: Rect,
    others: &[Rect],
    anchor: (f64, f64),
) -> Option<Rect> {
    let (width, height) = size;
    if !width.is_finite()
        || !height.is_finite()
        || width > work.width
        || height > work.height
        || width <= 0.0
        || height <= 0.0
    {
        return None;
    }
    let mut xs = vec![work.x, work.x + work.width - width];
    let mut ys = vec![work.y, work.y + work.height - height];
    // Add the user's anchor to avoid corner-only placement in open space.
    xs.push((anchor.0 - width / 2.0).clamp(work.x, work.x + work.width - width));
    ys.push((anchor.1 - height / 2.0).clamp(work.y, work.y + work.height - height));
    for other in others {
        xs.extend([other.x + other.width, other.x - width]);
        ys.extend([other.y + other.height, other.y - height]);
    }
    xs.retain(|x| *x >= work.x && *x + width <= work.x + work.width);
    ys.retain(|y| *y >= work.y && *y + height <= work.y + work.height);
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    xs.dedup();
    ys.dedup();
    let mut best: Option<((f64, f64), Rect)> = None;
    for x in xs {
        for &y in &ys {
            let rect = Rect::new(x, y, width, height);
            let overlap = others
                .iter()
                .filter_map(|other| intersection(rect, *other))
                .map(|r| r.width * r.height)
                .sum::<f64>();
            let distance =
                (x + width / 2.0 - anchor.0).powi(2) + (y + height / 2.0 - anchor.1).powi(2);
            let cost = (overlap, distance);
            if best.as_ref().is_none_or(|(old, _)| {
                cost.0
                    .total_cmp(&old.0)
                    .then(cost.1.total_cmp(&old.1))
                    .is_lt()
            }) {
                best = Some((cost, rect));
            }
        }
    }
    best.map(|(_, rect)| rect)
}

#[derive(Default)]
pub struct Cascade {
    positions: HashMap<String, (f64, f64)>,
}

impl Cascade {
    pub fn place(&mut self, output: &str, work: Rect, size: (f64, f64)) -> Rect {
        let position = self
            .positions
            .entry(output.to_owned())
            .or_insert((work.x, work.y));
        if position.0 < work.x
            || position.1 < work.y
            || position.0 + size.0 > work.x + work.width
            || position.1 + size.1 > work.y + work.height
        {
            *position = (work.x, work.y);
        }
        let rect = Rect::new(position.0, position.1, size.0, size.1);
        *position = (position.0 + CASCADE_STEP, position.1 + CASCADE_STEP);
        rect
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Remembered {
    pub output: String,
    fractions: [f64; 4],
}

impl Remembered {
    pub fn new(output: String, rect: Rect, work: Rect) -> Self {
        Self {
            output,
            fractions: [
                (rect.x - work.x) / work.width,
                (rect.y - work.y) / work.height,
                rect.width / work.width,
                rect.height / work.height,
            ],
        }
    }

    pub fn restore(&self, outputs: &[(String, Rect)]) -> Option<Rect> {
        let work = outputs.iter().find(|(name, _)| *name == self.output)?.1;
        if !self.fractions.iter().all(|v| v.is_finite())
            || self.fractions[2] <= 0.0
            || self.fractions[3] <= 0.0
        {
            return None;
        }
        let [x, y, w, h] = self.fractions;
        let rect = Rect::new(
            work.x + x * work.width,
            work.y + y * work.height,
            w * work.width,
            h * work.height,
        );
        if ![rect.x, rect.y, rect.width, rect.height]
            .iter()
            .all(|v| v.is_finite())
        {
            return None;
        }
        let visible = outputs.iter().any(|(_, area)| {
            intersection(rect, *area)
                .is_some_and(|r| r.width * r.height >= rect.width * rect.height * 0.25)
        });
        visible.then_some(rect)
    }
}

#[derive(Clone, Default)]
pub struct Memory {
    entries: BTreeMap<String, Remembered>,
}

#[derive(Default)]
struct SaveState {
    pending: Option<(PathBuf, Memory)>,
    closed: bool,
}

struct SaveThread {
    shared: std::sync::Arc<(std::sync::Mutex<SaveState>, std::sync::Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SaveThread {
    fn start(
        mut persist: impl FnMut(&Path, &Memory) -> std::io::Result<()> + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = std::sync::Arc::new((
            std::sync::Mutex::new(SaveState::default()),
            std::sync::Condvar::new(),
        ));
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("floating-geometry-save".into())
            .spawn(move || {
                loop {
                    let job = {
                        let (mutex, ready) = &*worker;
                        let mut state = mutex.lock().unwrap();
                        while state.pending.is_none() && !state.closed {
                            state = ready.wait(state).unwrap();
                        }
                        match state.pending.take() {
                            Some(job) => job,
                            None => break,
                        }
                    };
                    // Serialization, directory creation, writes and fsync all run
                    // outside the queue lock and outside the compositor thread.
                    if let Err(error) = persist(&job.0, &job.1) {
                        tracing::warn!(%error, "could not save floating window placement");
                    }
                }
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    fn queue(&self, path: PathBuf, memory: Memory) {
        let (mutex, ready) = &*self.shared;
        mutex.lock().unwrap().pending = Some((path, memory));
        ready.notify_one();
    }
}

impl Drop for SaveThread {
    fn drop(&mut self) {
        let (mutex, ready) = &*self.shared;
        mutex.lock().unwrap().closed = true;
        ready.notify_one();
        // Flush the latest snapshot only during compositor shutdown, after
        // the event loop has stopped. Drag completion never joins this thread.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub struct SaveWorker {
    worker: Option<SaveThread>,
}

impl SaveWorker {
    pub fn queue(&mut self, path: PathBuf, memory: &Memory) -> std::io::Result<()> {
        if self.worker.is_none() {
            self.worker = Some(SaveThread::start(|path, memory| memory.persist(path))?);
        }
        self.worker.as_ref().unwrap().queue(path, memory.clone());
        Ok(())
    }
}

impl Memory {
    pub fn get(&self, app_id: &str) -> Option<&Remembered> {
        self.entries.get(app_id)
    }
    pub fn save(&mut self, app_id: String, entry: Remembered) {
        if !app_id.is_empty() && app_id.len() <= 4096 {
            self.entries.insert(app_id, entry);
        }
    }
    pub fn load(path: &Path) -> Self {
        let entries = std::fs::metadata(path)
            .ok()
            .filter(|m| m.len() <= 1024 * 1024)
            .and_then(|_| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { entries }
    }
    pub fn persist(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("memory path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(&self.entries)?)?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
    pub fn path() -> Option<PathBuf> {
        let root = std::env::var_os("XDG_STATE_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))?;
        Some(root.join("ferese/floating.json"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapLine {
    pub target: u64,
    pub position: f64,
}

#[derive(Default, Debug)]
pub struct AxisSnap {
    stuck: Option<(SnapLine, bool)>,
}

impl AxisSnap {
    pub fn clear(&mut self) {
        self.stuck = None;
    }
    pub fn apply(&mut self, raw: f64, length: f64, lines: &[SnapLine]) -> f64 {
        if let Some((line, high)) = self.stuck {
            if let Some(current) = lines
                .iter()
                .find(|l| l.target == line.target && l.position == line.position)
            {
                let correction = current.position - raw - if high { length } else { 0.0 };
                if correction.abs() < RELEASE_DISTANCE {
                    return raw + correction;
                }
            }
            self.stuck = None;
        }
        let nearest = lines
            .iter()
            .flat_map(|line| {
                [
                    (line.position - raw, *line, false),
                    (line.position - raw - length, *line, true),
                ]
            })
            .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()));
        if let Some((correction, line, high)) = nearest
            && correction.abs() < SNAP_DISTANCE
        {
            self.stuck = Some((line, high));
            raw + correction
        } else {
            raw
        }
    }
}

pub fn snap_lines(raw: Rect, work: Rect, others: &[(u64, Rect)]) -> (Vec<SnapLine>, Vec<SnapLine>) {
    let mut xs = vec![
        SnapLine {
            target: 0,
            position: work.x,
        },
        SnapLine {
            target: 0,
            position: work.x + work.width,
        },
    ];
    let mut ys = vec![
        SnapLine {
            target: 0,
            position: work.y,
        },
        SnapLine {
            target: 0,
            position: work.y + work.height,
        },
    ];
    for &(target, rect) in others {
        if raw.y - SNAP_DISTANCE <= rect.y + rect.height
            && raw.y + raw.height + SNAP_DISTANCE >= rect.y
        {
            xs.extend([
                SnapLine {
                    target,
                    position: rect.x,
                },
                SnapLine {
                    target,
                    position: rect.x + rect.width,
                },
            ]);
        }
        if raw.x - SNAP_DISTANCE <= rect.x + rect.width
            && raw.x + raw.width + SNAP_DISTANCE >= rect.x
        {
            ys.extend([
                SnapLine {
                    target,
                    position: rect.y,
                },
                SnapLine {
                    target,
                    position: rect.y + rect.height,
                },
            ]);
        }
    }
    (xs, ys)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saves_do_not_wait_for_storage_and_coalesce_to_the_latest_snapshot() {
        use std::sync::{Arc, Mutex, mpsc};
        let written = Arc::new(Mutex::new(Vec::new()));
        let recorded = written.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut first = true;
        let worker = SaveThread::start(move |_, memory| {
            recorded
                .lock()
                .unwrap()
                .push(memory.entries.keys().next().unwrap().clone());
            if first {
                first = false;
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap(); // simulate arbitrarily slow storage
            }
            Ok(())
        })
        .unwrap();
        let snapshot = |name: &str| {
            let mut memory = Memory::default();
            memory.save(
                name.into(),
                Remembered::new(
                    "panel".into(),
                    Rect::new(0., 0., 10., 10.),
                    Rect::new(0., 0., 100., 100.),
                ),
            );
            memory
        };
        worker.queue(PathBuf::from("unused"), snapshot("first"));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        worker.queue(PathBuf::from("unused"), snapshot("superseded"));
        worker.queue(PathBuf::from("unused"), snapshot("last"));
        assert_eq!(*written.lock().unwrap(), vec!["first"]);
        release_tx.send(()).unwrap();
        drop(worker); // shutdown drains the latest queued snapshot
        assert_eq!(*written.lock().unwrap(), vec!["first", "last"]);
    }
    #[test]
    fn transient_centers_on_visible_parent_and_clamps_without_shrinking() {
        let work = Rect::new(100., 50., 1000., 800.);
        let visible = intersection(Rect::new(-300., -100., 800., 600.), work).unwrap();
        assert_eq!(visible, Rect::new(100., 50., 400., 450.));
        assert_eq!(
            clamp_to_work(centered((200., 100.), visible), work),
            Rect::new(200., 225., 200., 100.)
        );
        assert!(intersection(Rect::new(-300., 50., 200., 100.), work).is_none());
        assert_eq!(
            clamp_to_work(Rect::new(-100., -100., 1200., 900.), work),
            Rect::new(100., 50., 1200., 900.)
        );
    }
    #[test]
    fn client_rounded_resize_keeps_the_opposite_edges_anchored() {
        let rect = Rect::new(100., 80., 800., 600.);
        assert_eq!(
            anchored_size(rect, (693., 547.), true, true),
            Rect::new(207., 133., 693., 547.)
        );
        assert_eq!(
            anchored_size(rect, (693., 600.), true, false),
            Rect::new(207., 80., 693., 600.)
        );
        assert_eq!(
            anchored_size(rect, (800., 547.), false, true),
            Rect::new(100., 133., 800., 547.)
        );
        assert_eq!(
            anchored_size(rect, (693., 547.), false, false),
            Rect::new(100., 80., 693., 547.)
        );
    }
    #[test]
    fn placement_avoids_overlap_and_centers_empty_space() {
        let work = Rect::new(10., 20., 100., 100.);
        assert_eq!(
            min_overlap((20., 20.), work, &[], (60., 70.)),
            Some(Rect::new(50., 60., 20., 20.))
        );
        let obstacle = Rect::new(10., 20., 60., 100.);
        let placed = min_overlap((20., 20.), work, &[obstacle], (60., 70.)).unwrap();
        assert!(intersection(placed, obstacle).is_none());
        // A tiny overlap must never win merely because it is nearer.
        let placed = min_overlap(
            (10., 10.),
            Rect::new(0., 0., 1000., 10.),
            &[Rect::new(0., 0., 10., 0.0001)],
            (5., 5.),
        )
        .unwrap();
        assert_eq!(placed.x, 10.);
    }
    #[test]
    fn candidate_search_matches_exhaustive_positions() {
        let mut seed = 719_u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as u32
        };
        for _ in 0..200 {
            let size = ((next() % 8 + 1) as f64, (next() % 8 + 1) as f64);
            let others = (0..5)
                .map(|_| {
                    Rect::new(
                        (next() % 12) as f64,
                        (next() % 12) as f64,
                        (next() % 8 + 1) as f64,
                        (next() % 8 + 1) as f64,
                    )
                })
                .collect::<Vec<_>>();
            let overlap = |r| {
                others
                    .iter()
                    .filter_map(|o| intersection(r, *o))
                    .map(|r| r.width * r.height)
                    .sum::<f64>()
            };
            let placed = min_overlap(size, Rect::new(0., 0., 16., 16.), &others, (8., 8.)).unwrap();
            let brute = (0..=16 - size.0 as i32)
                .flat_map(|x| {
                    (0..=16 - size.1 as i32)
                        .map(move |y| Rect::new(x as f64, y as f64, size.0, size.1))
                })
                .map(overlap)
                .min_by(f64::total_cmp)
                .unwrap();
            assert_eq!(overlap(placed), brute);
        }
    }
    #[test]
    fn memory_scales_and_rejects_missing_or_invisible_outputs() {
        let entry = Remembered::new(
            "panel".into(),
            Rect::new(25., 50., 50., 25.),
            Rect::new(0., 0., 100., 100.),
        );
        assert_eq!(
            entry.restore(&[("panel".into(), Rect::new(100., 0., 200., 200.))]),
            Some(Rect::new(150., 100., 100., 50.))
        );
        assert_eq!(entry.restore(&[]), None);
        let offscreen = Remembered::new(
            "panel".into(),
            Rect::new(-90., 0., 100., 100.),
            Rect::new(0., 0., 100., 100.),
        );
        assert_eq!(
            offscreen.restore(&[("panel".into(), Rect::new(0., 0., 100., 100.))]),
            None
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("floating.json");
        let mut memory = Memory::default();
        memory.save("app".into(), entry);
        memory.persist(&path).unwrap();
        assert!(Memory::load(&path).get("app").is_some());
    }
    #[test]
    fn cascade_is_per_output_and_wraps() {
        let mut cascade = Cascade::default();
        let work = Rect::new(100., 50., 100., 100.);
        assert_eq!(cascade.place("one", work, (50., 50.)).x, 100.);
        assert_eq!(cascade.place("one", work, (50., 50.)).x, 132.);
        assert_eq!(cascade.place("two", work, (50., 50.)).x, 100.);
        assert_eq!(cascade.place("one", work, (50., 50.)).x, 100.);
        assert!(min_overlap((150., 50.), work, &[], (100., 50.)).is_none());
    }
    #[test]
    fn snap_hysteresis_keeps_the_same_edge_and_releases_from_raw_motion() {
        let mut snap = AxisSnap::default();
        let lines = [SnapLine {
            target: 1,
            position: 100.,
        }];
        assert_eq!(snap.apply(95., 50., &lines), 100.);
        assert_eq!(snap.apply(85., 50., &lines), 100.);
        assert_eq!(snap.apply(79., 50., &lines), 79.);
        assert_eq!(snap.apply(45., 50., &lines), 50.);
        assert_eq!(snap.apply(35., 50., &lines), 50.);
        assert_eq!(snap.apply(35., 50., &[]), 35.);
        snap.clear();
        assert_eq!(snap.apply(85., 50., &lines), 85.);
        assert_eq!(snap.apply(90., 50., &lines), 90., "10px does not acquire");
        assert_eq!(snap.apply(91., 50., &lines), 100.);
        assert_eq!(snap.apply(80., 50., &lines), 80., "20px releases");
        assert_eq!(snap.apply(95., 50., &lines), 100.);
        // A narrow window must not switch edges while stuck to the same line.
        assert_eq!(snap.apply(86., 8., &lines), 100.);
        let mut other_axis = AxisSnap::default();
        assert_eq!(other_axis.apply(85., 50., &lines), 85.);
    }
    #[test]
    fn snapping_excludes_windows_across_the_screen() {
        let (xs, ys) = snap_lines(
            Rect::new(0., 0., 50., 50.),
            Rect::new(0., 0., 1000., 1000.),
            &[
                (1, Rect::new(100., 500., 50., 50.)),
                (2, Rect::new(100., 20., 50., 50.)),
            ],
        );
        assert!(!xs.iter().any(|l| l.target == 1));
        assert!(xs.iter().any(|l| l.target == 2));
        assert!(!ys.iter().any(|l| l.target == 1 || l.target == 2));
    }
}
