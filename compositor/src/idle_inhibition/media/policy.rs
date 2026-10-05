use std::collections::{BTreeMap, HashSet};

use ferese_ipc::media::{Choice, Playback, Snapshot};

use super::{Player, Update};

#[derive(Default)]
pub(super) struct PlayersState {
    pub players: BTreeMap<String, Player>,
    pub pinned: Option<String>,
    pub ignored: HashSet<String>,
    activity: u64,
}

impl PlayersState {
    pub fn update(&mut self, mut player: Player) {
        if let Some(old) = self.players.get(&player.name) {
            player.started = old.started;
            player.active = old.active;
            if player.view.status != old.view.status
                || player.view.track_id != old.view.track_id
                || player.view.title != old.view.title
                || player.view.artist != old.view.artist
            {
                self.activity += 1;
                player.active = self.activity;
                if player.playing && !old.playing {
                    player.started = self.activity;
                }
            }
        } else {
            self.activity += 1;
            player.active = self.activity;
            if player.playing {
                player.started = self.activity;
            }
        }
        self.players.insert(player.name.clone(), player);
    }

    pub fn remove(&mut self, name: &str) {
        self.players.remove(name);
        self.ignored.remove(name);
        if self.pinned.as_deref() == Some(name) {
            self.pinned = None;
        }
    }

    pub fn selected(&self) -> Option<&Player> {
        let eligible =
            |player: &&Player| player.view.status != Playback::Stopped && !self.ignored.contains(&player.name);
        if let Some(player) = self
            .pinned
            .as_ref()
            .and_then(|name| self.players.get(name))
            .filter(eligible)
        {
            return Some(player);
        }

        self.players.values().filter(eligible).max_by(|a, b| {
            (a.playing, if a.playing { a.started } else { a.active })
                .cmp(&(b.playing, if b.playing { b.started } else { b.active }))
                .then_with(|| b.name.cmp(&a.name))
        })
    }

    pub fn update_snapshot(&self) -> Update {
        Update {
            players: self.players.values().cloned().collect(),
            snapshot: Snapshot {
                revision: 0,
                selected: self.selected().map(|player| player.view.clone()),
                pinned: self.pinned.clone(),
                players: self
                    .players
                    .values()
                    .map(|player| Choice {
                        name: player.name.clone(),
                        owner: player.owner.clone(),
                        identity: player.view.identity.clone(),
                        status: player.view.status,
                        ignored: self.ignored.contains(&player.name),
                    })
                    .collect(),
            },
        }
    }
}
