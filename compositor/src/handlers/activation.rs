use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use smithay::input::Seat;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
#[cfg(test)]
use smithay::utils::SERIAL_COUNTER;
use smithay::utils::Serial;
use smithay::wayland::compositor::with_states;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};

use crate::Ferese;

const TOKEN_MAX_AGE: Duration = Duration::from_secs(10);
const MAX_INTERACTIONS: usize = 64;
const MAX_TOKENS: usize = 256;

pub(crate) struct InputHistory<C = ClientId> {
    events: VecDeque<(Serial, C, Instant)>,
}

impl<C> Default for InputHistory<C> {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
        }
    }
}

impl<C: PartialEq> InputHistory<C> {
    fn record(&mut self, serial: Serial, client: C, time: Instant) {
        while self.events.len() >= MAX_INTERACTIONS {
            self.events.pop_front();
        }

        self.events.push_back((serial, client, time));
    }

    pub(crate) fn validate(&self, serial: Serial, client: &C, now: Instant) -> Option<Instant> {
        self.events.iter().rev().find_map(|(event, owner, time)| {
            (*event == serial
                && owner == client
                && now.saturating_duration_since(*time) <= TOKEN_MAX_AGE)
                .then_some(*time)
        })
    }
}

struct ApprovedInteraction(Instant);

#[derive(Default)]
struct PendingActivation(Mutex<Option<Instant>>);

fn activation_deadline(data: &XdgActivationTokenData) -> Option<Instant> {
    data.user_data
        .get::<ApprovedInteraction>()
        .map(|proof| data.timestamp.min(proof.0) + TOKEN_MAX_AGE)
}

impl Ferese {
    fn prune_activation_tokens(&mut self, now: Instant) {
        self.xdg_activation_state.retain_tokens(|_, data| {
            activation_deadline(data).is_some_and(|deadline| now < deadline)
        });
    }

    fn arm_activation_cleanup(&mut self) -> bool {
        if self.activation_timer_pending {
            return true;
        }
        match self
            .loop_handle
            .insert_source(Timer::from_duration(TOKEN_MAX_AGE), |_, _, state| {
                state.prune_activation_tokens(Instant::now());
                if state.xdg_activation_state.tokens().next().is_some() {
                    TimeoutAction::ToDuration(TOKEN_MAX_AGE)
                } else {
                    state.activation_timer_pending = false;
                    TimeoutAction::Drop
                }
            }) {
            Ok(_) => {
                self.activation_timer_pending = true;
                true
            }
            Err(error) => {
                tracing::warn!(%error, "could not schedule activation token cleanup");
                false
            }
        }
    }

    pub(crate) fn revoke_activations(&mut self) {
        self.activation_inputs.events.clear();
        self.xdg_activation_state.retain_tokens(|_, _| false);
        for window in self.space.elements() {
            if let Some(toplevel) = window.toplevel() {
                take_pending_activation(toplevel.wl_surface());
            }
        }
    }

    pub(crate) fn activate_pending_window(&mut self, surface: &WlSurface) {
        let Some(deadline) = take_pending_activation(surface) else {
            return;
        };
        if Instant::now() < deadline
            && !self.session_lock.active()
            && !self.input_capture.active()
            && let Some(id) = self.windows.id_for_surface(surface)
        {
            self.activate_managed_window(id);
        }
    }

    pub(crate) fn record_activation_input(&mut self, serial: Serial, surface: Option<WlSurface>) {
        if self.session_lock.active() || self.input_capture.active() {
            return;
        }

        if let Some(client) = surface.and_then(|surface| surface.client()) {
            self.activation_inputs
                .record(serial, client.id(), Instant::now());
        }
    }
}

fn take_pending_activation(surface: &WlSurface) -> Option<Instant> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<PendingActivation>()
            .and_then(|pending| {
                pending
                    .0
                    .lock()
                    .expect("pending activation is not poisoned")
                    .take()
            })
    })
}

impl XdgActivationHandler for Ferese {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        if self.session_lock.active() || self.input_capture.active() {
            return false;
        }

        let Some((serial, seat)) = &data.serial else {
            return false;
        };
        let Some(client) = &data.client_id else {
            return false;
        };
        if Seat::<Self>::from_resource(seat).as_ref() != Some(&self.seat)
            || seat.client().is_none_or(|owner| owner.id() != *client)
            || data
                .surface
                .as_ref()
                .is_some_and(|surface| surface.client().is_none_or(|owner| owner.id() != *client))
        {
            return false;
        }

        let Some(time) = self
            .activation_inputs
            .validate(*serial, client, Instant::now())
        else {
            return false;
        };
        data.user_data
            .insert_if_missing(|| ApprovedInteraction(time));
        self.prune_activation_tokens(Instant::now());
        self.xdg_activation_state.tokens().count() < MAX_TOKENS && self.arm_activation_cleanup()
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        self.xdg_activation_state.remove_token(&token);
        if self.session_lock.active() || self.input_capture.active() {
            return;
        }

        // The recipient may be a different client: launcher-to-app token handoff.
        // Retain the original interaction time rather than renewing it at commit.
        let Some(deadline) =
            activation_deadline(&data).filter(|deadline| Instant::now() < *deadline)
        else {
            tracing::debug!("ignored stale or non-interactive activation request");
            return;
        };

        let Some(id) = self.windows.id_for_surface(&surface) else {
            if self.window_for_surface(&surface).is_some() {
                with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing(PendingActivation::default);
                    *states
                        .data_map
                        .get::<PendingActivation>()
                        .expect("pending activation exists")
                        .0
                        .lock()
                        .expect("pending activation is not poisoned") = Some(deadline);
                });
            }
            return;
        };
        self.activate_managed_window(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_creation_validates_real_seat_owner_and_input_age() {
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::sync::Arc;

        if !crate::startup_tests::private_runtime(
            "handlers::activation::tests::token_creation_validates_real_seat_owner_and_input_age",
        ) {
            return;
        }

        let mut events = EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let (server, mut wire) = UnixStream::pair().unwrap();
        let launcher = state
            .display_handle
            .insert_client(server, Arc::new(crate::state::ClientState::default()))
            .unwrap();
        let send = |wire: &mut UnixStream, object: u32, opcode: u32, args: &[u32]| {
            let bytes: Vec<_> = [object, ((args.len() as u32 + 2) * 4) << 16 | opcode]
                .into_iter()
                .chain(args.iter().copied())
                .flat_map(u32::to_ne_bytes)
                .collect();
            wire.write_all(&bytes).unwrap();
        };
        send(&mut wire, 1, 1, &[2]); // wl_display.get_registry
        events
            .dispatch(Duration::from_millis(1), &mut state)
            .unwrap();
        state.display_handle.flush_clients().unwrap();
        wire.set_nonblocking(true).unwrap();
        let mut globals = Vec::new();
        let _ = wire.read_to_end(&mut globals);
        wire.set_nonblocking(false).unwrap();
        let mut offset = 0;
        let mut seat_name = None;
        while offset < globals.len() {
            let header = u32::from_ne_bytes(globals[offset + 4..offset + 8].try_into().unwrap());
            if u32::from_ne_bytes(globals[offset..offset + 4].try_into().unwrap()) == 2
                && header & 0xffff == 0
            {
                let length =
                    u32::from_ne_bytes(globals[offset + 12..offset + 16].try_into().unwrap())
                        as usize;
                if &globals[offset + 16..offset + 16 + length] == b"wl_seat\0" {
                    seat_name = Some(u32::from_ne_bytes(
                        globals[offset + 8..offset + 12].try_into().unwrap(),
                    ));
                }
            }

            offset += (header >> 16) as usize;
        }

        let mut args = vec![seat_name.expect("seat global"), 8];
        args.extend(
            b"wl_seat\0"
                .chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
        );
        args.extend([7, 3]);
        send(&mut wire, 2, 0, &args);
        events
            .dispatch(Duration::from_millis(1), &mut state)
            .unwrap();
        let seat: WlSeat = state
            .seat
            .client_seats(&launcher)
            .into_iter()
            .next()
            .expect("bound seat");
        let (token, _) = state.xdg_activation_state.create_external_token(None);
        let token = token.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let data = || XdgActivationTokenData {
            client_id: Some(launcher.id()),
            serial: Some((serial, seat.clone())),
            ..Default::default()
        };
        assert!(
            !state.token_created(token.clone(), data()),
            "fabricated serial accepted"
        );
        state.activation_inputs.record(
            serial,
            launcher.id(),
            Instant::now() - TOKEN_MAX_AGE - Duration::from_millis(1),
        );
        assert!(
            !state.token_created(token.clone(), data()),
            "fresh token renewed stale input"
        );
        state
            .activation_inputs
            .record(serial, launcher.id(), Instant::now());
        let approved = data();
        assert!(state.token_created(token.clone(), approved.clone()));
        assert!(approved.user_data.get::<ApprovedInteraction>().is_some());
        for _ in 0..MAX_TOKENS {
            state
                .xdg_activation_state
                .create_external_token(Some(approved.clone()));
        }
        assert!(
            !state.token_created(token.clone(), data()),
            "token pool must be bounded"
        );
        state.xdg_activation_state.retain_tokens(|_, _| false);
        assert!(
            state.token_created(token.clone(), data()),
            "capacity must recover after cleanup"
        );

        let (server, mut recipient_wire) = UnixStream::pair().unwrap();
        let recipient = state
            .display_handle
            .insert_client(server, Arc::new(crate::state::ClientState::default()))
            .unwrap();
        let mut stolen = data();
        stolen.client_id = Some(recipient.id());
        assert!(
            !state.token_created(token.clone(), stolen),
            "other client claimed launcher's seat/input"
        );
        // The proof is attached to the token, not the recipient identity. This
        // is what lets a validated launcher hand it to a different application.
        assert_ne!(approved.client_id, Some(recipient.id()));
        assert!(
            approved
                .user_data
                .get::<ApprovedInteraction>()
                .unwrap()
                .0
                .elapsed()
                < TOKEN_MAX_AGE
        );

        // A valid handoff must not focus a recipient before its first buffer.
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
        use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
        use smithay::wayland::shell::xdg::XdgWmBaseUserData;
        let compositor = recipient
            .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
                &state.display_handle,
                6,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        let shell = recipient
            .create_resource::<XdgWmBase, XdgWmBaseUserData, Ferese>(
                &state.display_handle,
                6,
                Default::default(),
            )
            .unwrap();
        send(&mut recipient_wire, compositor.id().protocol_id(), 0, &[2]);
        send(&mut recipient_wire, shell.id().protocol_id(), 2, &[3, 2]);
        send(&mut recipient_wire, 3, 1, &[4]);
        events
            .dispatch(Duration::from_millis(1), &mut state)
            .unwrap();
        let target = state
            .space
            .elements()
            .find_map(|window| {
                let surface = window.toplevel()?.wl_surface();
                surface
                    .client()
                    .is_some_and(|client| client.id() == recipient.id())
                    .then(|| surface.clone())
            })
            .expect("recipient's toplevel");
        state.request_activation(token, approved, target.clone());
        assert_eq!(state.seat.get_keyboard().unwrap().current_focus(), None);
        assert!(with_states(&target, |states| {
            states
                .data_map
                .get::<PendingActivation>()
                .unwrap()
                .0
                .lock()
                .unwrap()
                .is_some()
        }));
        state.revoke_activations();
        assert_eq!(take_pending_activation(&target), None);
        assert_eq!(
            state
                .activation_inputs
                .validate(serial, &launcher.id(), Instant::now()),
            None
        );
        assert_eq!(state.xdg_activation_state.tokens().count(), 0);
    }

    #[test]
    fn validates_owner_serial_and_original_interaction_age() {
        let now = Instant::now();
        let mut history = InputHistory::default();
        let serial = Serial::from(12);
        history.record(serial, "launcher", now);
        assert_eq!(history.validate(serial, &"launcher", now), Some(now));
        assert_eq!(history.validate(Serial::from(13), &"launcher", now), None);
        assert_eq!(history.validate(serial, &"other-client", now), None);
        assert_eq!(
            history.validate(
                serial,
                &"launcher",
                now + TOKEN_MAX_AGE + Duration::from_millis(1)
            ),
            None
        );
        // An approved token retains the interaction time across client handoff.
        let approved = ApprovedInteraction(history.validate(serial, &"launcher", now).unwrap());
        assert_eq!(approved.0, now);
    }
}
