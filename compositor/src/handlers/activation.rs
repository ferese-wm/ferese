use std::collections::VecDeque;
use std::time::{Duration, Instant};

use smithay::input::Seat;
use smithay::reexports::wayland_server::{Resource, backend::ClientId};
use smithay::utils::Serial;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};

use crate::Ferese;

const TOKEN_MAX_AGE: Duration = Duration::from_secs(10);
const MAX_INTERACTIONS: usize = 64;

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

    fn validate(&self, serial: Serial, client: &C, now: Instant) -> Option<Instant> {
        self.events.iter().rev().find_map(|(event, owner, time)| {
            (*event == serial && owner == client && now.saturating_duration_since(*time) <= TOKEN_MAX_AGE)
                .then_some(*time)
        })
    }
}

struct ApprovedInteraction(Instant);

impl Ferese {
    pub(crate) fn record_activation_input(&mut self, serial: Serial, surface: Option<WlSurface>) {
        if self.session_lock.active() || self.input_capture.active() {
            return;
        }

        if let Some(client) = surface.and_then(|surface| surface.client()) {
            self.activation_inputs.record(serial, client.id(), Instant::now());
        }
    }
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

        let Some(time) = self.activation_inputs.validate(*serial, client, Instant::now()) else {
            return false;
        };
        data.user_data.insert_if_missing(|| ApprovedInteraction(time));
        true
    }

    fn request_activation(&mut self, token: XdgActivationToken, data: XdgActivationTokenData, surface: WlSurface) {
        self.xdg_activation_state.remove_token(&token);
        if self.session_lock.active() || self.input_capture.captures(1) {
            return;
        }

        // The recipient may be a different client: launcher-to-app token handoff.
        // Retain the original interaction time rather than renewing it at commit.
        let valid = data.timestamp.elapsed() <= TOKEN_MAX_AGE
            && data
                .user_data
                .get::<ApprovedInteraction>()
                .is_some_and(|proof| proof.0.elapsed() <= TOKEN_MAX_AGE);
        if !valid {
            tracing::debug!("ignored stale or non-interactive activation request");
            return;
        }

        let Some(window) = self
            .space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == &surface)
            })
            .cloned()
        else {
            return;
        };

        self.focused_window = self.windows.ids().get(&window).copied();
        if let Some(focused) = self.focused_window
            && let Err(error) = self.workspaces.focus_window(focused)
        {
            tracing::error!(%error, ?focused, "failed to update workspace focus");
            return;
        }
        self.raise_window(&window, true);
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
        self.space.elements().for_each(|window| {
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_pending_configure();
            }
        });
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
        events.dispatch(Duration::from_millis(1), &mut state).unwrap();
        state.display_handle.flush_clients().unwrap();
        wire.set_nonblocking(true).unwrap();
        let mut globals = Vec::new();
        let _ = wire.read_to_end(&mut globals);
        wire.set_nonblocking(false).unwrap();
        let mut offset = 0;
        let mut seat_name = None;
        while offset < globals.len() {
            let header = u32::from_ne_bytes(globals[offset + 4..offset + 8].try_into().unwrap());
            if u32::from_ne_bytes(globals[offset..offset + 4].try_into().unwrap()) == 2 && header & 0xffff == 0 {
                let length = u32::from_ne_bytes(globals[offset + 12..offset + 16].try_into().unwrap()) as usize;
                if &globals[offset + 16..offset + 16 + length] == b"wl_seat\0" {
                    seat_name = Some(u32::from_ne_bytes(globals[offset + 8..offset + 12].try_into().unwrap()));
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
        events.dispatch(Duration::from_millis(1), &mut state).unwrap();
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
        state.activation_inputs.record(serial, launcher.id(), Instant::now());
        let approved = data();
        assert!(state.token_created(token.clone(), approved.clone()));
        assert!(approved.user_data.get::<ApprovedInteraction>().is_some());

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
        assert!(approved.user_data.get::<ApprovedInteraction>().unwrap().0.elapsed() < TOKEN_MAX_AGE);

        // Exercise the production handler with a real recipient toplevel.
        // Creating the window needs no GPU or client buffer.
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
        use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
        use smithay::wayland::shell::xdg::XdgWmBaseUserData;
        let compositor = recipient
            .create_resource::<WlCompositor, (), Ferese>(&state.display_handle, 6, ())
            .unwrap();
        let shell = recipient
            .create_resource::<XdgWmBase, XdgWmBaseUserData, Ferese>(&state.display_handle, 6, Default::default())
            .unwrap();
        send(&mut recipient_wire, compositor.id().protocol_id(), 0, &[2]);
        send(&mut recipient_wire, shell.id().protocol_id(), 2, &[3, 2]);
        send(&mut recipient_wire, 3, 1, &[4]);
        events.dispatch(Duration::from_millis(1), &mut state).unwrap();
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
        assert_eq!(state.seat.get_keyboard().unwrap().current_focus(), Some(target));
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
            history.validate(serial, &"launcher", now + TOKEN_MAX_AGE + Duration::from_millis(1)),
            None
        );
        // An approved token retains the interaction time across client handoff.
        let approved = ApprovedInteraction(history.validate(serial, &"launcher", now).unwrap());
        assert_eq!(approved.0, now);
    }
}
