use serde::{Deserialize, Serialize};

pub fn now_us() -> u64 {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // CLOCK_MONOTONIC shares one epoch between the compositor and shell.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return 0;
    }

    (time.tv_sec.max(0) as u64)
        .saturating_mul(1_000_000)
        .saturating_add(time.tv_nsec.max(0) as u64 / 1_000)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Playback {
    Playing,
    Paused,
    #[default]
    Stopped,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Player {
    pub name: String,
    pub owner: String,
    pub identity: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    pub track_id: Option<String>,
    pub status: Playback,
    pub position_us: Option<u64>,
    pub sampled_at_us: u64,
    pub length_us: Option<u64>,
    pub rate: f64,
    pub volume: Option<f64>,
    pub can_control: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    pub can_raise: bool,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            name: String::new(),
            owner: String::new(),
            identity: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            art_url: None,
            track_id: None,
            status: Playback::Stopped,
            position_us: None,
            sampled_at_us: 0,
            length_us: None,
            rate: 1.0,
            volume: None,
            can_control: false,
            can_play: false,
            can_pause: false,
            can_next: false,
            can_previous: false,
            can_seek: false,
            can_raise: false,
        }
    }
}

impl Player {
    pub fn label(&self) -> &str {
        if self.title.is_empty() {
            &self.identity
        } else {
            &self.title
        }
    }

    pub fn can_toggle(&self) -> bool {
        self.can_control
            && if self.status == Playback::Playing {
                self.can_pause
            } else {
                self.can_play
            }
    }

    pub fn position_at(&self, now_us: u64) -> Option<u64> {
        let mut position = self.position_us? as f64;
        if self.status == Playback::Playing && self.rate.is_finite() {
            position += now_us.saturating_sub(self.sampled_at_us) as f64 * self.rate;
        }

        let position = position.max(0.0) as u64;
        Some(self.length_us.map_or(position, |length| position.min(length)))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Choice {
    pub name: String,
    pub owner: String,
    pub identity: String,
    pub status: Playback,
    pub ignored: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Snapshot {
    pub revision: u64,
    pub selected: Option<Player>,
    pub players: Vec<Choice>,
    pub pinned: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_uses_rate_and_monotonic_sample_without_polling() {
        let mut player = Player {
            status: Playback::Playing,
            position_us: Some(2_000_000),
            sampled_at_us: 10_000_000,
            length_us: Some(8_000_000),
            rate: 1.5,
            ..Player::default()
        };
        assert_eq!(player.position_at(12_000_000), Some(5_000_000));
        assert_eq!(player.position_at(20_000_000), Some(8_000_000));
        assert_eq!(player.position_at(9_000_000), Some(2_000_000));
        player.status = Playback::Paused;
        assert_eq!(player.position_at(20_000_000), Some(2_000_000));
        player.position_us = None;
        assert_eq!(player.position_at(20_000_000), None);
    }
}
