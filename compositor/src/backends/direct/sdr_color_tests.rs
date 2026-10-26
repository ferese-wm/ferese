use super::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::rc::Rc;

#[derive(Clone)]
struct Fake(Rc<RefCell<State>>);
#[derive(Default)]
struct State {
    properties: HashMap<connector::Handle, Vec<Observed>>,
    blobs: HashMap<u64, Vec<u8>>,
    events: Vec<Event>,
    commits: usize,
    fail_commits: HashSet<usize>,
    retains: usize,
    fail_retain: Option<usize>,
    next_blob: u64,
}
#[derive(Debug, PartialEq)]
enum Event {
    Commit {
        assignments: Vec<Assignment>,
        test: bool,
    },
    Retain(u64),
    Destroy(u64),
}

fn connector(id: u32) -> connector::Handle {
    NonZeroU32::new(id).unwrap().into()
}
fn property(id: u32) -> property::Handle {
    NonZeroU32::new(id).unwrap().into()
}
fn observed(id: u32, current: u64, sdr: u64, blob: bool) -> Observed {
    Observed {
        property: property(id),
        current,
        sdr,
        blob,
        writable: true,
    }
}

impl Hardware for Fake {
    fn properties(&self, connector: connector::Handle) -> io::Result<Vec<Observed>> {
        Ok(self
            .0
            .borrow()
            .properties
            .get(&connector)
            .into_iter()
            .flatten()
            .map(|entry| Observed {
                property: entry.property,
                current: entry.current,
                sdr: entry.sdr,
                blob: entry.blob,
                writable: entry.writable,
            })
            .collect())
    }
    fn blob(&self, id: u64) -> io::Result<Vec<u8>> {
        self.0
            .borrow()
            .blobs
            .get(&id)
            .cloned()
            .ok_or_else(|| io::Error::other("blob disappeared"))
    }
    fn retain_blob(&self, bytes: &mut [u8]) -> io::Result<u64> {
        let mut state = self.0.borrow_mut();
        state.retains += 1;
        if state.fail_retain == Some(state.retains) {
            return Err(io::Error::other("allocation failure"));
        }
        state.next_blob += 1;
        let id = state.next_blob;
        state.blobs.insert(id, bytes.to_vec());
        state.events.push(Event::Retain(id));
        Ok(id)
    }
    fn destroy_blob(&self, id: u64) {
        let mut state = self.0.borrow_mut();
        state.blobs.remove(&id);
        state.events.push(Event::Destroy(id));
    }
    fn commit(&self, assignments: &[Assignment], test: bool) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        state.commits += 1;
        state.events.push(Event::Commit {
            assignments: assignments.to_vec(),
            test,
        });
        if state.fail_commits.contains(&state.commits) {
            return Err(io::Error::other("atomic failure"));
        }
        if !test {
            for assignment in assignments {
                state
                    .properties
                    .get_mut(&assignment.connector)
                    .unwrap()
                    .iter_mut()
                    .find(|entry| entry.property == assignment.property)
                    .unwrap()
                    .current = assignment.value;
            }
        }
        Ok(())
    }
}

fn fixture() -> Fake {
    let fake = Fake(Rc::new(RefCell::new(State {
        next_blob: 1000,
        ..State::default()
    })));
    fake.0.borrow_mut().properties.insert(
        connector(1),
        vec![observed(1, 11, 0, true), observed(2, 3, 7, false)],
    );
    fake.0.borrow_mut().blobs.insert(11, vec![1, 2, 3, 4, 5]);
    fake
}
fn current(fake: &Fake, connector_id: u32, property_id: u32) -> u64 {
    fake.0.borrow().properties[&connector(connector_id)]
        .iter()
        .find(|entry| entry.property == property(property_id))
        .unwrap()
        .current
}

#[test]
fn missing_and_already_sdr_properties_need_no_commits_or_blob_allocations() {
    let fake = Fake(Rc::new(RefCell::new(State::default())));
    let plan = Plan::capture_with(&fake, [connector(1)]).unwrap();
    assert!(plan.is_empty());
    drop(plan.apply_with(fake.clone(), true).unwrap());
    fake.0.borrow_mut().properties.insert(
        connector(1),
        vec![observed(1, 0, 0, true), observed(2, 7, 7, false)],
    );
    drop(
        Plan::capture_with(&fake, [connector(1)])
            .unwrap()
            .apply_with(fake.clone(), true)
            .unwrap(),
    );
    assert!(fake.0.borrow().events.is_empty());
}

#[test]
fn metadata_and_named_default_values_change_together_after_test_and_blob_retention() {
    let fake = fixture();
    fake.0.borrow_mut().properties.insert(
        connector(2),
        vec![observed(3, 12, 0, true), observed(4, 5, 9, false)],
    );
    fake.0.borrow_mut().blobs.insert(12, vec![8, 9]);
    let applied = Plan::capture_with(&fake, [connector(1), connector(2), connector(1)])
        .unwrap()
        .apply_with(fake.clone(), true)
        .unwrap();
    assert_eq!(current(&fake, 1, 1), 0);
    assert_eq!(current(&fake, 1, 2), 7);
    assert_eq!(current(&fake, 2, 3), 0);
    assert_eq!(current(&fake, 2, 4), 9);
    let state = fake.0.borrow();
    assert!(
        matches!(&state.events[0], Event::Commit { assignments, test: true } if assignments.len() == 4)
    );
    assert!(matches!(&state.events[1], Event::Retain(_)));
    assert!(matches!(&state.events[2], Event::Retain(_)));
    assert!(
        matches!(&state.events[3], Event::Commit { assignments, test: false } if assignments.len() == 4)
    );
    drop(state);
    drop(applied);
    assert_eq!(fake.0.borrow().blobs.len(), 2);
}

#[test]
fn rollback_restores_blob_contents_after_the_original_kernel_id_has_disappeared() {
    let fake = fixture();
    let applied = Plan::capture_with(&fake, [connector(1)])
        .unwrap()
        .apply_with(fake.clone(), true)
        .unwrap();
    fake.0.borrow_mut().blobs.remove(&11);
    applied.restore(true).unwrap();
    let restored = current(&fake, 1, 1);
    assert_ne!(restored, 11);
    assert_eq!(fake.0.borrow().blobs[&restored], [1, 2, 3, 4, 5]);
    assert_eq!(current(&fake, 1, 2), 3);
    drop(applied);
    assert!(!fake.0.borrow().blobs.contains_key(&restored));
}

#[test]
fn failed_tests_and_failed_real_commits_preserve_the_previous_properties() {
    for failing_call in [1, 2] {
        let fake = fixture();
        fake.0.borrow_mut().fail_commits.insert(failing_call);
        assert!(
            Plan::capture_with(&fake, [connector(1)])
                .unwrap()
                .apply_with(fake.clone(), true)
                .is_err()
        );
        assert_eq!(current(&fake, 1, 1), 11);
        assert_eq!(current(&fake, 1, 2), 3);
        assert_eq!(fake.0.borrow().blobs.len(), 1);
        if failing_call == 1 {
            assert_eq!(fake.0.borrow().retains, 0);
        }
    }
}

#[test]
fn failure_to_retain_any_backup_releases_earlier_blobs_before_real_changes() {
    let fake = fixture();
    fake.0
        .borrow_mut()
        .properties
        .insert(connector(2), vec![observed(3, 12, 0, true)]);
    fake.0.borrow_mut().blobs.insert(12, vec![8, 9]);
    fake.0.borrow_mut().fail_retain = Some(2);
    assert!(
        Plan::capture_with(&fake, [connector(1), connector(2)])
            .unwrap()
            .apply_with(fake.clone(), true)
            .is_err()
    );
    assert_eq!(fake.0.borrow().commits, 1);
    assert_eq!(fake.0.borrow().blobs.len(), 2);
    assert_eq!(current(&fake, 1, 1), 11);
    assert_eq!(current(&fake, 2, 3), 12);
}

#[test]
fn rollback_failures_are_reported_and_keep_resource_cleanup_bounded() {
    for failing_call in [3, 4] {
        let fake = fixture();
        fake.0.borrow_mut().fail_commits.insert(failing_call);
        let applied = Plan::capture_with(&fake, [connector(1)])
            .unwrap()
            .apply_with(fake.clone(), true)
            .unwrap();
        assert!(applied.restore(true).is_err());
        assert_eq!(current(&fake, 1, 1), 0);
        drop(applied);
        assert_eq!(fake.0.borrow().blobs.len(), 1);
    }
}

#[test]
fn inactive_sessions_issue_no_color_commits_and_readonly_inherited_state_is_rejected() {
    let fake = fixture();
    assert!(
        Plan::capture_with(&fake, [connector(1)])
            .unwrap()
            .apply_with(fake.clone(), false)
            .is_err()
    );
    assert!(fake.0.borrow().events.is_empty());
    let applied = Plan::capture_with(&fake, [connector(1)])
        .unwrap()
        .apply_with(fake.clone(), true)
        .unwrap();
    assert!(applied.restore(false).is_err());
    assert_eq!(fake.0.borrow().commits, 2);
    drop(applied);
    let fake = fixture();
    fake.0
        .borrow_mut()
        .properties
        .get_mut(&connector(1))
        .unwrap()[0]
        .writable = false;
    assert!(Plan::capture_with(&fake, [connector(1)]).is_err());
    assert!(fake.0.borrow().events.is_empty());
}
