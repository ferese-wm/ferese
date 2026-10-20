use clap::Subcommand;
use ferese_ipc::activities::{PomodoroAction, PomodoroRequest, VERSION};
use std::{error::Error, os::unix::net::UnixStream, path::Path, time::Duration};

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Start a session using activities.pomodoro settings.
    Start,
    Pause,
    Resume,
    /// Skip the current interval, or start a phase that is ready.
    Next,
    /// Cancel and remove the session without a completion alert.
    Stop,
    /// Cancel and start a new session with current settings.
    Reset,
    Status,
}
pub fn command(action: &Action, socket: &Path, json: bool) -> Result<(), Box<dyn Error>> {
    let action = match action {
        Action::Start => PomodoroAction::Start,
        Action::Pause => PomodoroAction::Pause,
        Action::Resume => PomodoroAction::Resume,
        Action::Next => PomodoroAction::Next,
        Action::Stop => PomodoroAction::Stop,
        Action::Reset => PomodoroAction::Reset,
        Action::Status => PomodoroAction::Status,
    };
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let result = super::request(
        &mut stream,
        "pomodoro",
        serde_json::to_value(PomodoroRequest {
            version: VERSION,
            action,
            session: None,
        })?,
    )?;
    super::output::print(&result, json)
}
