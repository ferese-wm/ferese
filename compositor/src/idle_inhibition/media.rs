mod bus;
mod policy;

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Instant;

use calloop::EventLoop;
use calloop::channel::{Event, sync_channel};
use ferese_ipc::{Response, media::Snapshot};
use serde_json::Value;

use crate::Ferese;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Player {
    pub name: String,
    pub owner: String,
    pub pid: Option<u32>,
    pub desktop_entry: Option<String>,
    pub playing: bool,
    pub view: ferese_ipc::media::Player,
    pub(super) started: u64,
    pub(super) active: u64,
}

pub(crate) struct Command {
    args: Value,
    sent: Instant,
    reply: Option<(u64, mpsc::SyncSender<Response>)>,
}

struct Update {
    players: Vec<Player>,
    snapshot: Snapshot,
}

fn inhibition_changed(before: &[Player], after: &[Player]) -> bool {
    before.len() != after.len()
        || before.iter().zip(after).any(|(a, b)| {
            (&a.name, &a.owner, a.pid, &a.desktop_entry, a.playing)
                != (&b.name, &b.owner, b.pid, &b.desktop_entry, b.playing)
        })
}

#[derive(Default)]
pub(crate) struct Engine {
    snapshot: Snapshot,
    value: Value,
    waiters: HashMap<u64, (u64, mpsc::SyncSender<Response>)>,
    commands: Option<async_channel::Sender<Command>>,
}

impl Engine {
    pub(crate) fn value(&self) -> Value {
        if self.value.is_null() {
            serde_json::to_value(&self.snapshot).expect("serializable media snapshot")
        } else {
            self.value.clone()
        }
    }

    fn update(&mut self, mut snapshot: Snapshot) {
        snapshot.revision = self.snapshot.revision;
        if snapshot == self.snapshot {
            return;
        }

        snapshot.revision = snapshot.revision.wrapping_add(1);
        self.value = serde_json::to_value(&snapshot).expect("serializable media snapshot");
        self.snapshot = snapshot;
        for (_, (id, response)) in self.waiters.drain() {
            let _ = response.try_send(Response::success(id, self.value.clone()));
        }
    }

    pub(crate) fn watch(&mut self, owner: u64, id: u64, since: u64, response: mpsc::SyncSender<Response>) {
        if since != self.snapshot.revision {
            let _ = response.try_send(Response::success(id, self.value()));
        } else {
            self.waiters.insert(owner, (id, response));
        }
    }

    pub(crate) fn remove(&mut self, owner: u64) {
        self.waiters.remove(&owner);
    }

    pub(crate) fn action(&self, args: Value, reply: Option<(u64, mpsc::SyncSender<Response>)>) -> Result<(), String> {
        let commands = self.commands.as_ref().ok_or("Media service is unavailable")?;
        commands
            .try_send(Command {
                args,
                sent: Instant::now(),
                reply,
            })
            .map_err(|_| "Media command queue is unavailable or full".into())
    }
}

pub(crate) fn init(event_loop: &mut EventLoop<Ferese>) -> Result<(), Box<dyn std::error::Error>> {
    let (sender, receiver) = sync_channel::<Update>(1);
    let (commands, receive_commands) = async_channel::bounded(16);
    event_loop.handle().insert_source(receiver, move |event, _, state| {
        if let Event::Msg(update) = event {
            let changed = inhibition_changed(&state.media_players, &update.players);
            state.media_players = update.players;
            state.media_engine.update(update.snapshot);
            if changed {
                state.refresh_idle_inhibition();
            }
        }
    })?;
    event_loop
        .handle()
        .insert_idle(move |state| state.media_engine.commands = Some(commands));
    std::thread::Builder::new()
        .name("ferese-playback".into())
        .spawn(move || bus::run(sender, receive_commands))?;
    Ok(())
}

#[cfg(test)]
mod tests;
