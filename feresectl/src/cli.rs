use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

/// Control a running Ferese session.
#[derive(Debug, Parser)]
#[command(version)]
pub struct Cli {
    /// Print query results as JSON.
    #[arg(short = 'j', long, global = true)]
    pub json: bool,
    /// Connect to this IPC socket instead of FERESE_SOCKET or the runtime default.
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the active window.
    #[command(alias = "get-focused-window")]
    FocusedWindow,
    /// List windows.
    #[command(alias = "get-windows")]
    Windows,
    /// Show idle inhibitors.
    #[command(alias = "get-idle-inhibition")]
    IdleInhibition,
    /// List workspaces and their output ownership.
    #[command(alias = "get-workspaces")]
    Workspaces,
    /// List connected outputs and their applied configuration.
    #[command(alias = "get-outputs")]
    Outputs,
    /// List configured keybindings.
    #[command(alias = "get-keybindings")]
    Keybindings,
    /// Show session and inhibition state.
    #[command(alias = "get-session-state")]
    SessionState,
    /// Toggle floating for the focused window.
    ToggleFloating,
    /// Toggle fullscreen for the focused window.
    ToggleFullscreen,
    /// Toggle maximization for the focused window.
    ToggleMaximized,
    /// Switch between scrolling and tree layout.
    ToggleLayout,
    /// Show or hide the workspace overview.
    ToggleOverview,
    /// Show or hide the keybinding guide.
    ToggleKeybindingGuide,
    /// Cycle the focused column width.
    CycleColumnWidth,
    /// Center the focused column.
    CenterColumn,
    /// Pull a window into the focused column.
    Consume,
    /// Move a window out of the focused column.
    Expel,
    /// Close the focused window.
    Close,
    /// Cycle the display layout.
    ToggleDisplayMode,
    /// List available output profiles.
    OutputProfiles,
    /// Confirm the pending output configuration.
    OutputConfirm,
    /// Revert the pending output configuration.
    OutputRevert,
    /// Reload the compositor configuration.
    ReloadConfig,
    /// Exit the compositor.
    Exit,
    /// Open the logout confirmation.
    RequestLogout,
    /// Return to the previous workspace.
    WorkspaceBackAndForth,
    /// Focus the previously focused window.
    FocusLastWindow,
    /// Focus the most recently focused window in the opposite tiled/floating layer on this workspace.
    FocusFloating,
    /// Focus the next recently used window.
    FocusMruNext,
    /// Focus the previous recently used window.
    FocusMruPrevious,
    /// Focus a window in a direction.
    Focus { direction: Direction },
    /// Move the focused window in a direction.
    Move { direction: Direction },
    /// Resize the focused window in a direction.
    Resize { direction: Direction },
    /// Activate a workspace by its one-based index.
    Workspace {
        #[arg(value_parser = clap::value_parser!(u32).range(1..))]
        index: u32,
    },
    /// Move the focused window to a workspace.
    MoveToWorkspace {
        #[arg(value_parser = clap::value_parser!(u32).range(1..))]
        index: u32,
    },
    /// Apply a display layout.
    OutputLayout { layout: DisplayLayout },
    /// Select an output profile, or auto.
    OutputProfile { name: String },
    /// Enable or disable the internal display.
    OutputInternal { state: Switch },
    /// Write a screenshot PNG to stdout.
    Screenshot {
        #[arg(short = 'g', long)]
        geometry: Option<String>,
    },
    /// Write a window screenshot PNG to stdout.
    ScreenshotWindow { window: u64 },
    /// Launch desktop autostart entries.
    Autostart,
    /// Inspect or change the theme.
    Theme {
        #[command(subcommand)]
        action: ThemeAction,
    },
    /// Inspect or retry the XWayland server.
    Xwayland {
        #[command(subcommand)]
        action: XwaylandAction,
    },
    /// Inspect media players or control playback.
    Media {
        #[command(subcommand)]
        action: Option<MediaAction>,
    },
    /// Follow compositor changes as newline-delimited JSON.
    EventStream,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DisplayLayout {
    InternalOnly,
    ExternalOnly,
    Extend,
    Mirror,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Switch {
    On,
    Off,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ThemeMode {
    Light,
    Dark,
    Auto,
}

#[derive(Debug, Subcommand)]
pub enum ThemeAction {
    /// Show the current theme.
    #[command(alias = "status")]
    Get,
    /// Follow theme snapshots as newline-delimited JSON.
    Subscribe,
    /// Set the appearance mode.
    Mode { mode: ThemeMode },
    /// Resolve a configuration file without applying it.
    Preview { path: PathBuf },
}
#[derive(Debug, Subcommand)]
pub enum XwaylandAction {
    /// Show XWayland status.
    Status,
    /// Retry starting XWayland after a failure.
    Retry,
}
#[derive(Debug, Subcommand)]
pub enum MediaAction {
    /// Show players and the active player.
    Get,
    /// Toggle playback.
    PlayPause,
    /// Skip to the next track.
    Next,
    /// Return to the previous track.
    Previous,
    /// Raise the active player's window.
    Raise,
    /// Select the active player automatically.
    Auto,
    /// Prefer a particular player.
    Pin { player: String },
    /// Exclude a player from automatic selection.
    Ignore { player: String },
    /// Include a previously ignored player.
    Unignore { player: String },
}

fn value_name(value: &impl ValueEnum) -> String {
    value
        .to_possible_value()
        .expect("visible enum value")
        .get_name()
        .to_owned()
}

impl Command {
    pub fn ipc(&self) -> Option<(&'static str, Value)> {
        let (command, args) = match self {
            Self::FocusedWindow => ("get-focused-window", json!({})),
            Self::Windows => ("get-windows", json!({})),
            Self::IdleInhibition => ("get-idle-inhibition", json!({})),
            Self::Workspaces => ("get-workspaces", json!({})),
            Self::Outputs => ("outputs", json!({})),
            Self::Keybindings => ("get-keybindings", json!({})),
            Self::SessionState => ("get-session-state", json!({})),
            Self::ToggleFloating => ("toggle-floating", json!({})),
            Self::ToggleFullscreen => ("toggle-fullscreen", json!({})),
            Self::ToggleMaximized => ("toggle-maximized", json!({})),
            Self::ToggleLayout => ("toggle-layout", json!({})),
            Self::ToggleOverview => ("toggle-overview", json!({})),
            Self::ToggleKeybindingGuide => ("toggle-keybinding-guide", json!({})),
            Self::CycleColumnWidth => ("cycle-column-width", json!({})),
            Self::CenterColumn => ("center-column", json!({})),
            Self::Consume => ("consume", json!({})),
            Self::Expel => ("expel", json!({})),
            Self::Close => ("close", json!({})),
            Self::ToggleDisplayMode => ("toggle-display-mode", json!({})),
            Self::OutputProfiles => ("output-profiles", json!({})),
            Self::OutputConfirm => ("output-confirm", json!({})),
            Self::OutputRevert => ("output-revert", json!({})),
            Self::ReloadConfig => ("reload-config", json!({})),
            Self::Exit => ("exit", json!({})),
            Self::RequestLogout => ("request-logout", json!({})),
            Self::WorkspaceBackAndForth => ("workspace-back-and-forth", json!({})),
            Self::FocusLastWindow => ("focus-last-window", json!({})),
            Self::FocusFloating => ("focus-floating", json!({})),
            Self::FocusMruNext => ("focus-mru-next", json!({})),
            Self::FocusMruPrevious => ("focus-mru-previous", json!({})),
            Self::Focus { direction } => ("focus", json!({"direction": value_name(direction)})),
            Self::Move { direction } => ("move", json!({"direction": value_name(direction)})),
            Self::Resize { direction } => ("resize", json!({"direction": value_name(direction)})),
            Self::Workspace { index } => ("workspace", json!({"index": index})),
            Self::MoveToWorkspace { index } => ("move-to-workspace", json!({"index": index})),
            Self::OutputLayout { layout } => ("output-layout", json!({"layout": value_name(layout)})),
            Self::OutputProfile { name } => ("output-profile", json!({"name": name})),
            Self::OutputInternal { state } => ("output-internal", json!({"enabled": matches!(state, Switch::On)})),
            Self::Screenshot {
                geometry: Some(geometry),
            } => ("screenshot", json!({"geometry": geometry})),
            Self::Screenshot { geometry: None } => ("screenshot", json!({})),
            Self::ScreenshotWindow { window } => ("screenshot-window", json!({"window": window})),
            Self::Xwayland { action } => (
                match action {
                    XwaylandAction::Status => ferese_ipc::xwayland::STATUS_COMMAND,
                    XwaylandAction::Retry => ferese_ipc::xwayland::RETRY_COMMAND,
                },
                json!({}),
            ),
            Self::Media {
                action: None | Some(MediaAction::Get),
            } => ("media-get", json!({})),
            Self::Media { action: Some(action) } => {
                let args = match action {
                    MediaAction::Pin { player } => json!({"action": "pin", "player": player}),
                    MediaAction::Ignore { player } => json!({"action": "ignore", "player": player, "ignored": true}),
                    MediaAction::Unignore { player } => json!({"action": "ignore", "player": player, "ignored": false}),
                    MediaAction::PlayPause => json!({"action": "play-pause"}),
                    MediaAction::Next => json!({"action": "next"}),
                    MediaAction::Previous => json!({"action": "previous"}),
                    MediaAction::Raise => json!({"action": "raise"}),
                    MediaAction::Auto => json!({"action": "auto"}),
                    MediaAction::Get => unreachable!(),
                };
                ("media-action", args)
            }
            Self::Autostart | Self::Theme { .. } | Self::EventStream => return None,
        };
        Some((command, args))
    }
}
