mod art;
mod subscription;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use cosmic::app::Task;
use cosmic::iced::Subscription;
use ferese_ipc::media::{Playback, Snapshot, now_us};
use serde_json::{Value, json};

use crate::{FereseShell, Message};

pub(crate) use art::Artwork;

pub(crate) struct Model {
    pub snapshot: Arc<Snapshot>,
    pub error: Option<String>,
    pub choosing: bool,
    pub seeking: Option<u64>,
    pub now_us: u64,
    pub art: art::Service,
    pub artwork: Option<Artwork>,
    busy: bool,
    pending: VecDeque<Value>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            snapshot: Arc::new(Snapshot::default()),
            error: None,
            choosing: false,
            seeking: None,
            now_us: now_us(),
            art: art::Service::new(),
            artwork: None,
            busy: false,
            pending: VecDeque::new(),
        }
    }
}

impl Model {
    pub fn action(&self, action: &str) -> Value {
        match &self.snapshot.selected {
            Some(player) => json!({"action": action, "player":player.name, "owner":player.owner}),
            None => json!({"action": action}),
        }
    }

    pub fn subscription(&self) -> Subscription<Arc<Snapshot>> {
        subscription::subscription()
    }

    pub fn tick_subscription(&self, visible: bool, on_battery: bool) -> Subscription<()> {
        if visible && needs_progress(&self.snapshot) {
            cosmic::iced::time::every(Duration::from_secs(if on_battery { 3 } else { 1 })).map(|_| ())
        } else {
            Subscription::none()
        }
    }

    pub fn receive(&mut self, snapshot: Arc<Snapshot>) {
        let previous = self.snapshot.selected.as_ref();
        let next = snapshot.selected.as_ref();
        if previous.map(|p| (&p.owner, &p.track_id)) != next.map(|p| (&p.owner, &p.track_id)) {
            self.seeking = None;
            self.error = None;
        }

        self.snapshot = snapshot;
        self.now_us = now_us();
    }

    pub fn refresh_art(&mut self, visible: bool) {
        let player = if visible { self.snapshot.selected.as_ref() } else { None };
        self.art.request(player);
        if visible && self.artwork.as_ref().is_some_and(|art| !self.art.accepts(art)) {
            self.artwork = None;
        }
    }

    pub fn command(&mut self, args: Value) -> Task<Message> {
        if self.busy {
            // Coalesce adjacent wheel events without dropping transport clicks.
            if args["action"] == "volume"
                && let Some(pending) = self.pending.back_mut()
                && pending["action"] == "volume"
                && pending["owner"] == args["owner"]
            {
                pending["delta"] = json!(
                    (pending["delta"].as_f64().unwrap_or(0.0) + args["delta"].as_f64().unwrap_or(0.0)).clamp(-1.0, 1.0)
                );
            } else if self.pending.len() < 8 {
                self.pending.push_back(args);
            } else {
                self.error = Some("Media controls are busy".into());
            }

            return Task::none();
        }

        self.busy = true;
        self.error = None;
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                let mut connection = ferese_ipc::theme::Connection::connect().map_err(|e| e.to_string())?;
                connection.call("media-action", args).map(|_| ())
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
            cosmic::Action::App(Message::MediaCompleted(result))
        })
    }

    pub fn completed(&mut self, result: Result<(), String>) -> Task<Message> {
        self.busy = false;
        self.error = result.err();
        match self.pending.pop_front() {
            Some(args) => self.command(args),
            None => Task::none(),
        }
    }
}

fn needs_progress(snapshot: &Snapshot) -> bool {
    snapshot.selected.as_ref().is_some_and(|player| {
        player.status == Playback::Playing && player.position_us.is_some() && player.length_us.is_some()
    })
}

impl FereseShell {
    pub fn refresh_media_art(&mut self, menu_visible: bool) {
        self.media
            .refresh_art(menu_visible || self.outputs.iter().any(|output| !output.hidden));
    }

    pub fn media_tick_subscription(&self) -> Subscription<()> {
        let visible = self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.kind == crate::status_ui::Menu::Media && !menu.motion.closing())
            && self.outputs.iter().any(|output| !output.hidden);
        let on_battery = self
            .status
            .battery
            .as_ref()
            .is_some_and(|battery| battery.status == "Discharging");
        self.media.tick_subscription(visible, on_battery)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_updates_coalesce_and_transport_commands_remain_ordered_and_bounded() {
        let mut model = Model {
            busy: true,
            ..Default::default()
        };
        for _ in 0..100 {
            let _ = model.command(json!({"action":"volume", "owner":":1.1", "delta":0.01}));
        }

        assert_eq!(model.pending.len(), 1);
        assert!((model.pending[0]["delta"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        let _ = model.command(json!({"action":"next"}));
        let _ = model.command(json!({"action":"previous"}));
        assert_eq!(model.pending[1]["action"], "next");
        assert_eq!(model.pending[2]["action"], "previous");
        for _ in 0..100 {
            let _ = model.command(json!({"action":"next"}));
        }
        assert_eq!(model.pending.len(), 8);
        assert!(model.error.is_some());
    }

    #[test]
    fn progress_only_runs_for_playing_media_with_known_position_and_duration() {
        let mut snapshot = Snapshot::default();
        assert!(!needs_progress(&snapshot));
        snapshot.selected = Some(ferese_ipc::media::Player {
            status: Playback::Playing,
            position_us: Some(0),
            length_us: Some(100),
            ..Default::default()
        });
        assert!(needs_progress(&snapshot));
        snapshot.selected.as_mut().unwrap().status = Playback::Paused;
        assert!(!needs_progress(&snapshot));
        snapshot.selected.as_mut().unwrap().status = Playback::Playing;
        snapshot.selected.as_mut().unwrap().length_us = None;
        assert!(!needs_progress(&snapshot));
    }
}
