use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::SyncSender;

use ferese_ipc::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat;

const MAX_SESSIONS: usize = 8;
const MAX_BARRIERS: usize = 64;
const MAX_EVENTS: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Zone {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

impl Zone {
    fn right(self) -> f64 {
        f64::from(self.x) + f64::from(self.width)
    }

    fn bottom(self) -> f64 {
        f64::from(self.y) + f64::from(self.height)
    }

    fn contains(self, point: (f64, f64)) -> bool {
        point.0 >= f64::from(self.x)
            && point.0 < self.right()
            && point.1 >= f64::from(self.y)
            && point.1 < self.bottom()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Barrier {
    pub id: u32,
    pub position: [i32; 4],
}

#[derive(Clone, Copy, Debug)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

fn edge(barrier: &Barrier, zones: &[Zone]) -> Option<Edge> {
    if barrier.id == 0 {
        return None;
    }
    let [x1, y1, x2, y2] = barrier.position.map(f64::from);
    let horizontal = y1 == y2 && x1 <= x2;
    let vertical = x1 == x2 && y1 <= y2;
    if !horizontal && !vertical {
        return None;
    }
    for zone in zones {
        let found = if horizontal && x1 >= f64::from(zone.x) && x2 < zone.right() {
            if y1 == f64::from(zone.y) {
                Some(Edge::Top)
            } else if y1 == zone.bottom() {
                Some(Edge::Bottom)
            } else {
                None
            }
        } else if vertical && y1 >= f64::from(zone.y) && y2 < zone.bottom() {
            if x1 == f64::from(zone.x) {
                Some(Edge::Left)
            } else if x1 == zone.right() {
                Some(Edge::Right)
            } else {
                None
            }
        } else {
            None
        };
        let Some(found) = found else {
            continue;
        };
        let intersects_neighbor = zones.iter().any(|other| {
            if other == zone {
                return false;
            }
            match found {
                Edge::Top | Edge::Bottom => {
                    let outside = y1
                        + if matches!(found, Edge::Top) {
                            -0.5
                        } else {
                            0.5
                        };
                    outside >= f64::from(other.y)
                        && outside < other.bottom()
                        && x1 < other.right()
                        && x2 + 1.0 > f64::from(other.x)
                }
                Edge::Left | Edge::Right => {
                    let outside = x1
                        + if matches!(found, Edge::Left) {
                            -0.5
                        } else {
                            0.5
                        };
                    outside >= f64::from(other.x)
                        && outside < other.right()
                        && y1 < other.bottom()
                        && y2 + 1.0 > f64::from(other.y)
                }
            }
        });
        if !intersects_neighbor {
            return Some(found);
        }
    }
    None
}

fn crossed(barrier: &Barrier, edge: Edge, from: (f64, f64), to: (f64, f64)) -> Option<f64> {
    let [x1, y1, x2, y2] = barrier.position.map(f64::from);
    let (distance, movement, start, end) = match edge {
        Edge::Left if from.0 >= x1 && to.0 < x1 => (x1 - from.0, to.0 - from.0, y1, y2 + 1.0),
        Edge::Right if from.0 < x1 && to.0 >= x1 => (x1 - from.0, to.0 - from.0, y1, y2 + 1.0),
        Edge::Top if from.1 >= y1 && to.1 < y1 => (y1 - from.1, to.1 - from.1, x1, x2 + 1.0),
        Edge::Bottom if from.1 < y1 && to.1 >= y1 => (y1 - from.1, to.1 - from.1, x1, x2 + 1.0),
        _ => return None,
    };
    let fraction = distance / movement;
    let along = if matches!(edge, Edge::Left | Edge::Right) {
        from.1 + (to.1 - from.1) * fraction
    } else {
        from.0 + (to.0 - from.0) * fraction
    };
    ((0.0..=1.0).contains(&fraction) && along >= start && along < end).then_some(fraction)
}

struct Session {
    owner: u64,
    capabilities: u32,
    barriers: Vec<(Barrier, Edge)>,
    enabled: bool,
    closed: bool,
    activation: Option<u32>,
    events: VecDeque<Value>,
}

#[derive(Default)]
pub(crate) struct InputCapture {
    sessions: HashMap<u64, Session>,
    zones: Vec<Zone>,
    zone_set: u32,
    next_session: u64,
    next_activation: u32,
    active: Option<u64>,
    last_absolute: Option<(f64, f64)>,
    pub(crate) restore_focus: bool,
    waiters: HashMap<u64, (u64, u64, SyncSender<Response>)>,
}

impl InputCapture {
    pub(crate) fn update_zones(&mut self, mut zones: Vec<Zone>) {
        zones.sort_by_key(|zone| (zone.x, zone.y, zone.width, zone.height));
        zones.dedup();
        if self.zones == zones {
            return;
        }
        self.zones = zones;
        self.zone_set = self.zone_set.wrapping_add(1);
        self.disable_all();
        let ids = self.sessions.keys().copied().collect::<Vec<_>>();
        for id in ids {
            self.sessions.get_mut(&id).unwrap().barriers.clear();
            self.enqueue(id, json!({"type":"zones", "zone_set":self.zone_set}));
        }
        self.flush();
    }

    pub(crate) fn register(&mut self, owner: u64, capabilities: u32) -> Result<u64, String> {
        if self.sessions.len() >= MAX_SESSIONS || capabilities == 0 || capabilities & !3 != 0 {
            return Err("Unsupported capabilities or input capture session limit reached".into());
        }
        self.next_session = self
            .next_session
            .checked_add(1)
            .ok_or("Input capture IDs exhausted")?;
        let id = self.next_session;
        self.sessions.insert(
            id,
            Session {
                owner,
                capabilities,
                barriers: Vec::new(),
                enabled: false,
                closed: false,
                activation: None,
                events: VecDeque::new(),
            },
        );
        Ok(id)
    }

    pub(crate) fn zones(&self, id: u64) -> Result<Value, String> {
        self.session(id)?;
        Ok(
            json!({"zone_set":self.zone_set, "zones":self.zones.iter().map(|zone|
            (zone.width, zone.height, zone.x, zone.y)).collect::<Vec<_>>()}),
        )
    }

    fn session(&self, id: u64) -> Result<&Session, String> {
        self.sessions
            .get(&id)
            .ok_or("Unknown input capture session".into())
    }

    pub(crate) fn set_barriers(
        &mut self,
        id: u64,
        zone_set: u32,
        barriers: Vec<Barrier>,
    ) -> Result<Vec<u32>, String> {
        self.session(id)?;
        if barriers.len() > MAX_BARRIERS {
            return Err("Too many pointer barriers".into());
        }
        self.disable(id)?;
        let mut accepted = Vec::new();
        let mut failed = Vec::new();
        let mut counts = HashMap::new();
        for barrier in &barriers {
            *counts.entry(barrier.id).or_insert(0) += 1;
        }
        for barrier in barriers {
            let kind = edge(&barrier, &self.zones);
            if zone_set == self.zone_set
                && counts[&barrier.id] == 1
                && let Some(kind) = kind
            {
                accepted.push((barrier, kind));
            } else {
                failed.push(barrier.id);
            }
        }
        self.sessions.get_mut(&id).unwrap().barriers = accepted;
        Ok(failed)
    }

    pub(crate) fn enable(&mut self, id: u64) -> Result<(), String> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or("Unknown input capture session")?;
        if session.closed {
            return Err("Input capture session is closed".into());
        }
        session.enabled = true;
        Ok(())
    }

    pub(crate) fn has_sessions(&self) -> bool {
        !self.sessions.is_empty()
    }

    pub(crate) fn active(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn captures(&self, capability: u32) -> bool {
        self.active
            .and_then(|id| self.sessions.get(&id))
            .is_some_and(|session| session.capabilities & capability != 0)
    }

    pub(crate) fn motion(&mut self, from: (f64, f64), to: (f64, f64), allowed: bool) -> bool {
        if self.active() {
            return true;
        }
        if !allowed
            || ![from.0, from.1, to.0, to.1]
                .iter()
                .all(|value| value.is_finite())
        {
            return false;
        }
        let hit = self
            .sessions
            .iter()
            .filter(|(_, session)| session.enabled && !session.closed)
            .flat_map(|(id, session)| {
                session.barriers.iter().filter_map(move |(barrier, kind)| {
                    crossed(barrier, *kind, from, to).map(|fraction| (*id, barrier.id, fraction))
                })
            })
            .min_by(|left, right| left.2.total_cmp(&right.2).then(left.0.cmp(&right.0)));
        let Some((id, barrier, _)) = hit else {
            return false;
        };
        self.next_activation = self.next_activation.wrapping_add(1);
        let activation = self.next_activation;
        self.active = Some(id);
        let session = self.sessions.get_mut(&id).unwrap();
        session.activation = Some(activation);
        self.enqueue(
            id,
            json!({"type":"activated", "activation_id":activation,
            "barrier_id":barrier, "cursor_position":to}),
        );
        self.flush();
        true
    }

    fn enqueue(&mut self, id: u64, event: Value) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        if session.closed {
            return;
        }
        if session.events.len() >= MAX_EVENTS {
            session.events.clear();
            session.events.push_back(json!({"type":"closed"}));
            session.closed = true;
            session.enabled = false;
            session.activation = None;
            if self.active == Some(id) {
                self.active = None;
                self.last_absolute = None;
                self.restore_focus = true;
            }
            return;
        }
        session.events.push_back(event);
    }

    pub(crate) fn push(&mut self, event: Value) {
        if let Some(id) = self.active {
            self.enqueue(id, event);
            self.flush();
        }
    }

    pub(crate) fn absolute_delta(&mut self, point: (f64, f64)) -> (f64, f64) {
        let previous = self.last_absolute.replace(point).unwrap_or(point);
        (point.0 - previous.0, point.1 - previous.1)
    }

    pub(crate) fn close_all(&mut self) {
        if self.active.take().is_some() {
            self.restore_focus = true;
        }
        self.last_absolute = None;
        self.sessions.clear();
        self.flush();
    }

    pub(crate) fn release(&mut self, id: u64, activation: Option<u32>) -> Result<(), String> {
        let session = self.session(id)?;
        if activation.is_some() && session.activation != activation {
            return Err("Stale input capture activation".into());
        }
        if self.active == Some(id) {
            self.active = None;
            self.last_absolute = None;
            self.restore_focus = true;
            let session = self.sessions.get_mut(&id).unwrap();
            let activation = session.activation.take();
            self.enqueue(
                id,
                json!({"type":"deactivated", "activation_id":activation}),
            );
            self.flush();
        }
        Ok(())
    }

    pub(crate) fn disable(&mut self, id: u64) -> Result<(), String> {
        self.release(id, None)?;
        let session = self.sessions.get_mut(&id).unwrap();
        if session.enabled {
            session.enabled = false;
            self.enqueue(id, json!({"type":"disabled"}));
        }
        self.flush();
        Ok(())
    }

    pub(crate) fn disable_all(&mut self) {
        let ids = self.sessions.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let _ = self.disable(id);
        }
    }

    pub(crate) fn valid_position(&self, point: (f64, f64)) -> bool {
        point.0.is_finite()
            && point.1.is_finite()
            && self.zones.iter().any(|zone| zone.contains(point))
    }

    pub(crate) fn watch(
        &mut self,
        owner: u64,
        request: u64,
        id: u64,
        response: SyncSender<Response>,
    ) {
        if !self.sessions.contains_key(&id)
            || self.waiters.len() >= MAX_SESSIONS
            || self.waiters.values().any(|(_, token, _)| *token == id)
        {
            let _ = response.try_send(Response::error(
                request,
                "invalid_capture",
                "Unknown or already watched input capture session",
            ));
            return;
        }
        self.waiters.insert(owner, (request, id, response));
        self.flush();
    }

    fn flush(&mut self) {
        let ready = self
            .waiters
            .iter()
            .filter_map(|(owner, (_, id, _))| {
                self.sessions
                    .get(id)
                    .is_none_or(|session| !session.events.is_empty())
                    .then_some(*owner)
            })
            .collect::<Vec<_>>();
        for owner in ready {
            let (request, id, response) = self.waiters.remove(&owner).unwrap();
            let result = if let Some(session) = self.sessions.get_mut(&id) {
                json!({"events":session.events.drain(..).collect::<Vec<_>>()})
            } else {
                json!({"events":[{"type":"closed"}]})
            };
            let _ = response.try_send(Response::success(request, result));
        }
    }

    pub(crate) fn remove_owner(&mut self, owner: u64) {
        self.waiters.remove(&owner);
        let ids = self
            .sessions
            .iter()
            .filter_map(|(id, session)| (session.owner == owner).then_some(*id))
            .collect::<Vec<_>>();
        for id in ids {
            let _ = self.release(id, None);
            self.sessions.remove(&id);
        }
        self.flush();
    }
}

impl crate::Ferese {
    pub(crate) fn refresh_input_capture_zones(&mut self) {
        if !self.input_capture.has_sessions() {
            return;
        }
        let active = self.input_capture.active();
        let zones = self
            .space
            .outputs()
            .filter_map(|output| {
                let rect = self.space.output_geometry(output)?;
                Some(Zone {
                    width: u32::try_from(rect.size.w).ok()?,
                    height: u32::try_from(rect.size.h).ok()?,
                    x: rect.loc.x,
                    y: rect.loc.y,
                })
            })
            .collect();
        self.input_capture.update_zones(zones);
        if active && !self.input_capture.active() {
            self.restore_input_capture_focus();
        }
    }

    pub(crate) fn restore_input_capture_focus(&mut self) {
        self.input_capture.restore_focus = false;
        self.cursor_redraw_pending = true;
        if !self.session_lock.active() {
            self.restore_keyboard_focus();
            if let Some(pointer) = self.seat.get_pointer() {
                let location = pointer.current_location();
                pointer.motion(
                    self,
                    self.surface_under(location),
                    &smithay::input::pointer::MotionEvent {
                        location,
                        serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                        time: smithay::backend::input::InputTime::from_millis(0),
                    },
                );
                pointer.frame(self);
            }
        }
    }

    pub(crate) fn capture_pointer_motion(
        &mut self,
        from: smithay::utils::Point<f64, smithay::utils::Logical>,
        to: smithay::utils::Point<f64, smithay::utils::Logical>,
        absolute: bool,
    ) -> bool {
        let pointer = self.seat.get_pointer().expect("seat has pointer");
        let active = self.input_capture.active();
        let allowed = !self.session_lock.active()
            && !self.overview.is_presenting()
            && !pointer.is_grabbed()
            && !self.swipe.active()
            && self
                .seat
                .get_keyboard()
                .is_none_or(|keyboard| !keyboard.is_grabbed())
            && self
                .seat
                .get_touch()
                .is_none_or(|touch| !touch.is_grabbed())
            && !self.seat.keyboard_shortcuts_inhibited();
        if !self
            .input_capture
            .motion((from.x, from.y), (to.x, to.y), allowed)
        {
            return false;
        }
        let delta = if absolute {
            self.input_capture.absolute_delta((to.x, to.y))
        } else {
            (to.x - from.x, to.y - from.y)
        };
        if !active {
            self.set_binding_mode("default");
            self.cursor_redraw_pending = true;
            pointer.motion(
                self,
                None,
                &smithay::input::pointer::MotionEvent {
                    location: from,
                    serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                    time: smithay::backend::input::InputTime::from_millis(0),
                },
            );
            pointer.frame(self);
            if self.input_capture.captures(1) {
                let keyboard = self.seat.get_keyboard().expect("seat has keyboard");
                let keys = keyboard
                    .pressed_keys()
                    .iter()
                    .filter(|key| !self.intercepted_keys.contains(key))
                    .map(|key| key.raw().saturating_sub(8))
                    .collect::<Vec<_>>();
                keyboard.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial());
                self.input_capture.push(json!({"type":"keys", "keys":keys}));
            }
        } else if self.input_capture.captures(2) {
            self.input_capture
                .push(json!({"type":"motion", "x":delta.0, "y":delta.1}));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zones() -> Vec<Zone> {
        vec![
            Zone {
                width: 1920,
                height: 1080,
                x: 0,
                y: 0,
            },
            Zone {
                width: 1920,
                height: 1080,
                x: 1920,
                y: 0,
            },
        ]
    }

    #[test]
    fn barriers_use_inclusive_pixel_span_and_only_outer_union_edges() {
        let zones = zones();
        for position in [
            [0, 0, 1919, 0],
            [0, 1080, 1919, 1080],
            [1920, 0, 3839, 0],
            [3840, 0, 3840, 1079],
            [0, 0, 0, 1079],
        ] {
            assert!(
                edge(&Barrier { id: 1, position }, &zones).is_some(),
                "{position:?}"
            );
        }
        for position in [
            [1920, 0, 1920, 1079],
            [0, 0, 1920, 0],
            [5, 10, 8, 13],
            [50, 50, 50, 200],
            [0, 5, 0, 4],
        ] {
            assert!(
                edge(&Barrier { id: 1, position }, &zones).is_none(),
                "{position:?}"
            );
        }
        let barrier = Barrier {
            id: 1,
            position: [3840, 0, 3840, 1079],
        };
        assert!(crossed(&barrier, Edge::Right, (3830.0, 1079.5), (3900.0, 1079.5)).is_some());
        assert!(crossed(&barrier, Edge::Right, (3830.0, 1080.0), (3900.0, 1080.0)).is_none());
    }

    #[test]
    fn absolute_samples_report_incremental_motion_and_reset_on_release() {
        let mut capture = InputCapture::default();
        assert_eq!(capture.absolute_delta((100.0, 50.0)), (0.0, 0.0));
        assert_eq!(capture.absolute_delta((110.0, 55.0)), (10.0, 5.0));
        assert_eq!(capture.absolute_delta((120.0, 60.0)), (10.0, 5.0));
        capture.close_all();
        assert_eq!(capture.absolute_delta((600.0, 700.0)), (0.0, 0.0));
    }

    #[test]
    fn duplicate_ids_are_rejected_without_installing_a_barrier() {
        let mut capture = InputCapture::default();
        capture.update_zones(zones());
        let id = capture.register(1, 3).unwrap();
        let barrier = Barrier {
            id: 9,
            position: [3840, 0, 3840, 1079],
        };
        let failed = capture
            .set_barriers(id, capture.zone_set, vec![barrier.clone(), barrier])
            .unwrap();
        assert_eq!(failed, vec![9, 9]);
        assert!(capture.sessions[&id].barriers.is_empty());
    }

    #[test]
    fn stalled_control_and_input_queues_close_and_restore_local_focus() {
        let mut capture = InputCapture::default();
        capture.update_zones(zones());
        let id = capture.register(1, 3).unwrap();
        capture
            .set_barriers(
                id,
                capture.zone_set,
                vec![Barrier {
                    id: 1,
                    position: [3840, 0, 3840, 1079],
                }],
            )
            .unwrap();
        capture.enable(id).unwrap();
        assert!(capture.motion((3839.0, 50.0), (3900.0, 50.0), true));
        for _ in 0..MAX_EVENTS + 10 {
            capture.push(json!({"type":"motion", "x":1.0, "y":0.0}));
        }
        assert!(!capture.active());
        assert!(capture.restore_focus);
        assert!(capture.enable(id).is_err());
        assert_eq!(capture.sessions[&id].events.len(), 1);
        assert_eq!(capture.sessions[&id].events[0]["type"], "closed");

        let id = capture.register(2, 2).unwrap();
        for _ in 0..MAX_EVENTS + 10 {
            capture.enable(id).ok();
            capture.disable(id).unwrap();
        }
        assert_eq!(capture.sessions[&id].events.len(), 1);
        assert!(capture.sessions[&id].closed);
    }

    #[test]
    fn release_preserves_enable_but_topology_changes_disable_and_invalidate() {
        let mut capture = InputCapture::default();
        capture.update_zones(zones());
        let id = capture.register(9, 3).unwrap();
        let barrier = Barrier {
            id: 1,
            position: [3840, 0, 3840, 1079],
        };
        assert!(
            capture
                .set_barriers(id, capture.zone_set, vec![barrier])
                .unwrap()
                .is_empty()
        );
        capture.enable(id).unwrap();
        assert!(capture.motion((3839.0, 50.0), (3900.0, 50.0), true));
        let activation = capture.sessions[&id].activation.unwrap();
        assert!(
            capture
                .release(id, Some(activation.wrapping_add(1)))
                .is_err()
        );
        capture.release(id, Some(activation)).unwrap();
        assert!(capture.sessions[&id].enabled);
        capture.update_zones(vec![zones()[0]]);
        assert!(!capture.active());
        assert!(!capture.sessions[&id].enabled);
        assert!(capture.sessions[&id].barriers.is_empty());
    }
}
