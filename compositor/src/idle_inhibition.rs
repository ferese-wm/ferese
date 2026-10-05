//! Automatic policy feeds the same idle notifier as Wayland and portal requests.
pub(crate) mod media;

use std::collections::HashSet;

use ferese_layout::WindowId;
use serde::Deserialize;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::Ferese;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub fullscreen_playback: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fullscreen_playback: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    None,
    Visible,
    Fullscreen,
    Playing,
    FullscreenPlaying,
}

impl Mode {
    fn inhibits(self, visible: bool, fullscreen: bool, playing: bool) -> bool {
        visible
            && match self {
                Self::None => false,
                Self::Visible => true,
                Self::Fullscreen => fullscreen,
                Self::Playing => playing,
                Self::FullscreenPlaying => fullscreen && playing,
            }
    }
}

#[derive(Clone, Copy)]
struct AppWindow<'a> {
    id: WindowId,
    pid: Option<u32>,
    app_id: &'a str,
}

/// MPRIS reports application playback, not which of its windows owns a video.
/// Prefer process credentials; use DesktopEntry only when no process matches.
/// Multiple matching windows are ambiguous, including hidden windows.
fn player_window(player: &media::Player, windows: &[AppWindow<'_>]) -> Option<WindowId> {
    let mut matches = windows
        .iter()
        .filter(|window| player.pid.is_some() && window.pid == player.pid);
    if let Some(window) = matches.next() {
        return matches.next().is_none().then_some(window.id);
    }

    let desktop = player.desktop_entry.as_deref()?;
    let mut matches = windows
        .iter()
        .filter(|window| crate::window_rules::normalize_app_id(window.app_id) == desktop);
    let window = matches.next()?;
    matches.next().is_none().then_some(window.id)
}

impl Ferese {
    pub(crate) fn refresh_idle_inhibition(&mut self) {
        let playing = self.media_players.iter().any(|player| player.playing);
        let has_rules = self.window_rules.iter().any(|rule| rule.idle_policy().is_some());
        let selected = self
            .window_rules
            .iter()
            .any(|rule| rule.idle_policy().is_some_and(|mode| mode != Mode::None));
        let playback_policy = self.idle_inhibit.fullscreen_playback
            || self
                .window_rules
                .iter()
                .any(|rule| matches!(rule.idle_policy(), Some(Mode::Playing | Mode::FullscreenPlaying)));
        let automatic = (playback_policy && playing) || selected;
        let eligible = if self.idle_inhibitors.is_empty() && !automatic {
            Default::default()
        } else {
            self.callback_outputs()
        };

        let application = if playback_policy && playing {
            self.windows
                .records()
                .map(|(&id, record)| {
                    let pid = record
                        .surface
                        .as_ref()
                        .and_then(|surface| surface.client())
                        .and_then(|client| client.get_credentials(&self.display_handle).ok())
                        .and_then(|credentials| u32::try_from(credentials.pid).ok());
                    AppWindow {
                        id,
                        pid,
                        app_id: &record.app_id,
                    }
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let playing_windows = self
            .media_players
            .iter()
            .filter(|player| player.playing)
            .filter_map(|player| player_window(player, &application))
            .collect::<HashSet<_>>();

        let automatic = automatic
            && !self.session_lock.active()
            && self.windows.records().any(|(&id, record)| {
                let Some(surface) = &record.surface else {
                    return false;
                };
                if !eligible.contains_key(&surface.into()) {
                    return false;
                }

                let explicit = has_rules
                    .then(|| {
                        let transient = with_states(surface, |states| {
                            states
                                .data_map
                                .get::<XdgToplevelSurfaceData>()
                                .is_some_and(|data| data.lock().unwrap().parent.is_some())
                        });
                        crate::window_rules::resolve(
                            &self.window_rules,
                            Some(&record.app_id),
                            Some(&record.title),
                            transient,
                        )
                        .idle_inhibit
                    })
                    .flatten();
                let mode = explicit.unwrap_or(if self.idle_inhibit.fullscreen_playback {
                    Mode::FullscreenPlaying
                } else {
                    Mode::None
                });
                let fullscreen = self
                    .workspaces
                    .workspace_for_window(id)
                    .and_then(|workspace| self.workspaces.workspace(workspace))
                    .is_some_and(|workspace| workspace.fullscreen == Some(id));

                mode.inhibits(true, fullscreen, playing_windows.contains(&id))
            });
        let requested = self
            .idle_inhibitors
            .keys()
            .any(|surface| eligible.contains_key(&surface.into()));
        self.automatic_idle_inhibited = automatic;
        self.idle_notifier_state
            .set_is_inhibited(automatic || requested || self.portal_session.idle_inhibited());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_releases_on_pause_hidden_window_and_fullscreen_exit() {
        assert!(Mode::FullscreenPlaying.inhibits(true, true, true));
        assert!(!Mode::FullscreenPlaying.inhibits(true, true, false));
        assert!(!Mode::FullscreenPlaying.inhibits(false, true, true));
        assert!(!Mode::FullscreenPlaying.inhibits(true, false, true));
        assert!(Mode::Visible.inhibits(true, false, false));
        assert!(!Mode::Visible.inhibits(false, true, true));
        assert!(Mode::Playing.inhibits(true, false, true));
        assert!(!Mode::Playing.inhibits(true, false, false));
        assert!(Mode::Fullscreen.inhibits(true, true, false));
        assert!(!Mode::None.inhibits(true, true, true));
    }

    #[test]
    fn configuration_validates_modes_and_keeps_idle_rules_out_of_placement() {
        let config = crate::config::Config::parse_source(
            "idle-inhibit { fullscreen-playback #false; }\nwindow-rule app-id=\"player\" idle-inhibit=\"visible\"\nwindow-rule app-id=\"player\" title=\"Private\" idle-inhibit=\"none\"\n",
        ).unwrap().runtime_config().unwrap();
        assert!(!config.idle_inhibit.fullscreen_playback);
        let visible = crate::window_rules::resolve(&config.window_rules, Some("player"), None, false);
        assert_eq!(visible.idle_inhibit, Some(Mode::Visible));
        let none = crate::window_rules::resolve(&config.window_rules, Some("player"), Some("Private"), false);
        assert_eq!(none.idle_inhibit, Some(Mode::None));
        assert_eq!(crate::window_rules::live_result(visible, none, false), None);
        for mode in ["playing", "fullscreen", "fullscreen-playing", "none"] {
            assert!(
                crate::config::Config::parse_source(&format!(
                    "window-rule app-id=\"player\" idle-inhibit=\"{mode}\"\n"
                ))
                .unwrap()
                .runtime_config()
                .is_ok()
            );
        }
        assert!(
            crate::config::Config::parse_source("window-rule app-id=\"player\" idle-inhibit=\"alwayss\"\n").is_err()
        );
        assert!(crate::config::Config::parse_source("idle-inhibit { fullscreen-playback \"yes\"; }\n").is_err());
    }

    #[test]
    fn player_identity_prefers_pid_and_rejects_ambiguous_or_closed_windows() {
        let player = media::Player {
            name: "org.mpris.MediaPlayer2.test".into(),
            owner: ":1.5".into(),
            pid: Some(42),
            desktop_entry: Some("player".into()),
            playing: true,
            ..Default::default()
        };
        let window = AppWindow {
            id: WindowId(1),
            pid: Some(42),
            app_id: "other",
        };
        let unrelated = AppWindow {
            id: WindowId(2),
            pid: Some(99),
            app_id: "Player.desktop",
        };
        assert_eq!(player_window(&player, &[window, unrelated]), Some(window.id));
        assert_eq!(player_window(&player, &[unrelated]), Some(unrelated.id));
        assert_eq!(player_window(&player, &[]), None);
        assert_eq!(
            player_window(
                &player,
                &[
                    window,
                    AppWindow {
                        id: WindowId(3),
                        ..window
                    }
                ]
            ),
            None
        );
        assert_eq!(
            player_window(
                &player,
                &[
                    unrelated,
                    AppWindow {
                        id: WindowId(4),
                        ..unrelated
                    }
                ]
            ),
            None
        );
    }
}
