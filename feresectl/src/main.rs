mod autostart;
mod cli;
mod output;

use std::error::Error;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::{env, fs, io};

use clap::{Parser, ValueEnum};
use cli::{Cli, Command, ThemeAction};
use ferese_ipc::{Request, Response, VERSION, read_frame, write_frame};
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Autostart) {
        return autostart::run();
    }
    for alias in env::args().skip(1).filter(|arg| {
        matches!(
            arg.as_str(),
            "get-outputs"
                | "get-windows"
                | "get-workspaces"
                | "get-focused-window"
                | "get-idle-inhibition"
                | "get-session-state"
                | "get-keybindings"
        )
    }) {
        eprintln!(
            "feresectl: {alias} is deprecated; use {}",
            alias.trim_start_matches("get-")
        );
    }
    let socket = ferese_ipc::socket::resolve(cli.socket.as_deref())?;
    match &cli.command {
        Command::Theme { action } => theme_command(action, &socket, cli.json),
        Command::EventStream => event_stream(&socket),
        command => {
            let (command, args) = command.ipc().expect("IPC command");
            let mut stream = UnixStream::connect(socket)?;
            let result = request(&mut stream, command, args)?;
            if matches!(command, "screenshot" | "screenshot-window") {
                write_png(&Response::success(1, result))
            } else {
                output::print(&result, cli.json)
            }
        }
    }
}

fn request(stream: &mut UnixStream, command: &str, args: Value) -> Result<Value, Box<dyn Error>> {
    write_frame(
        stream,
        &Request {
            version: VERSION,
            id: 1,
            kind: "command".into(),
            command: command.into(),
            args,
        },
    )?;
    let response: Response = read_frame(stream)?;
    if response.version != VERSION || response.id != 1 {
        return Err("Unexpected IPC response".into());
    }
    if let Some(error) = response.error {
        return Err(format!("{}: {}", error.code, error.message).into());
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn theme_command(action: &ThemeAction, socket: &Path, json_output: bool) -> Result<(), Box<dyn Error>> {
    let mut connection = ferese_ipc::theme::Connection::connect_to(socket)?;
    let result = match action {
        ThemeAction::Get => serde_json::to_value(connection.get(ferese_config::families::builtins)?)?,
        ThemeAction::Subscribe => {
            let mut snapshot = connection.get(ferese_config::families::builtins)?;
            let mut stdout = io::stdout().lock();
            loop {
                if !output::write_json_line(&mut stdout, &snapshot)? {
                    return Ok(());
                }
                snapshot = connection.watch(snapshot.revision, ferese_config::families::builtins)?;
            }
        }
        ThemeAction::Mode { mode } => connection.call(
            "theme-set-mode",
            json!({"mode": mode.to_possible_value().unwrap().get_name()}),
        )?,
        ThemeAction::Preview { path } => connection.call(
            "theme-preview",
            json!({
                "source": fs::read_to_string(path)?, "directory": path.canonicalize()?.parent(),
            }),
        )?,
    };
    output::print(&result, json_output)
}

fn event_stream(socket: &Path) -> Result<(), Box<dyn Error>> {
    let mut stream = UnixStream::connect(socket)?;
    request(
        &mut stream,
        "event-stream",
        json!({"version": ferese_ipc::events::VERSION}),
    )?;
    let mut stdout = io::stdout().lock();
    loop {
        let event: ferese_ipc::events::Event = read_frame(&mut stream)?;
        event.validate()?;
        if !output::write_json_line(&mut stdout, &event)? {
            return Ok(());
        }
    }
}

#[cfg(test)]
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<(String, Value), String> {
    let cli = Cli::try_parse_from(std::iter::once("feresectl".to_owned()).chain(args)).map_err(|e| e.to_string())?;
    cli.command
        .ipc()
        .map(|(command, args)| (command.into(), args))
        .ok_or_else(|| "not an IPC command".into())
}

// The compositor stages the PNG in a private directory and replies with its
// path. Open it before unlinking so a failure never destroys the only copy, and
// stream the bytes with explicit writes: println! would corrupt binary output.
fn write_png(response: &Response) -> Result<(), Box<dyn Error>> {
    let path = response
        .result
        .as_ref()
        .and_then(|result| result.get("path"))
        .and_then(Value::as_str)
        .ok_or("the compositor did not return a screenshot path")?;
    let mut file = fs::File::open(path)?;
    if let Err(error) = fs::remove_file(path) {
        // The compositor sweeps anything left behind, so a failure here is not
        // worth losing the capture over.
        eprintln!("feresectl: could not remove {path}: {error}");
    }
    let mut stdout = io::stdout().lock();
    io::copy(&mut file, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xwayland_commands_map_to_the_shared_ipc_command_names() {
        assert_eq!(ferese_ipc::xwayland::STATUS_COMMAND, "xwayland-status");
        assert_eq!(ferese_ipc::xwayland::RETRY_COMMAND, "xwayland-retry");

        assert!(Cli::try_parse_from(["feresectl", "xwayland", "status"]).is_ok());
    }

    #[test]
    fn the_xwayland_status_snapshot_round_trips_over_the_ipc_framing() {
        let status = ferese_ipc::xwayland::Status {
            enabled: true,
            effective_startup: "on-demand".to_owned(),
            state: ferese_ipc::xwayland::State::Running,
            display: Some(":7".to_owned()),
            satellite_pid: Some(24017),
            generation: Some(2),
            readiness: ferese_ipc::xwayland::Readiness::Verified,
            recent_failures: 1,
            restart_required: false,
            last_error: None,
        };
        let request = Request {
            version: VERSION,
            id: 1,
            kind: "command".to_owned(),
            command: ferese_ipc::xwayland::STATUS_COMMAND.to_owned(),
            args: json!({}),
        };

        let mut bytes = Vec::new();
        write_frame(&mut bytes, &request).expect("encode the request");
        let decoded: Request = read_frame(&mut bytes.as_slice()).expect("decode the request");
        assert_eq!(decoded.command, "xwayland-status");

        let mut reply = Vec::new();
        write_frame(
            &mut reply,
            &Response::success(1, ferese_ipc::xwayland::Status::to_value(&status)),
        )
        .expect("encode the response");
        let response: Response = read_frame(&mut reply.as_slice()).expect("decode the response");
        assert!(response.error.is_none());

        let decoded = ferese_ipc::xwayland::Status::from_value(response.result.expect("a result"))
            .expect("decode the status snapshot");
        assert_eq!(decoded, status);
        assert_eq!(decoded.state, ferese_ipc::xwayland::State::Running);
    }

    #[test]
    fn a_failed_x11_retry_surfaces_the_compositor_error_code() {
        let response = Response::error(1, "x11_unavailable", "X11 is disabled in the configuration");
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &response).expect("encode");
        let decoded: Response = read_frame(&mut bytes.as_slice()).expect("decode");

        let error = decoded.error.expect("an error response");
        assert_eq!(error.code, "x11_unavailable");
        assert!(error.message.contains("disabled"));
    }

    #[test]
    fn media_commands_route_transport_and_player_preferences() {
        assert_eq!(parse_args(["media".into()]).unwrap().0, "media-get");
        for action in ["play-pause", "next", "previous", "raise", "auto"] {
            let (command, args) = parse_args(["media".into(), action.into()]).unwrap();
            assert_eq!(command, "media-action");
            assert_eq!(args["action"], action);
        }

        for (action, ignored) in [("ignore", true), ("unignore", false)] {
            let (_, args) = parse_args(["media".into(), action.into(), "player".into()]).unwrap();
            assert_eq!(args["action"], "ignore");
            assert_eq!(args["ignored"], ignored);
            assert_eq!(args["player"], "player");
        }

        assert!(parse_args(["media".into(), "pin".into()]).is_err());
        assert!(parse_args(["media".into(), "next".into(), "extra".into()]).is_err());
    }

    #[test]
    fn display_mode_commands_validate_layouts() {
        for layout in ["internal-only", "external-only", "extend", "mirror"] {
            assert_eq!(
                parse_args(["output-layout".into(), layout.into()]).unwrap().1,
                json!({"layout":layout})
            );
        }
        assert!(parse_args(["output-layout".into(), "docked".into()]).is_err());
        assert!(parse_args(["output-layout".into()]).is_err());
        assert!(parse_args(["toggle-display-mode".into()]).is_ok());
    }

    #[test]
    fn shortcut_hint_command_rejects_extra_arguments() {
        assert_eq!(
            parse_args(["toggle-keybinding-guide".into()]).unwrap(),
            ("toggle-keybinding-guide".into(), json!({}))
        );
        assert!(parse_args(["toggle-keybinding-guide".into(), "extra".into()]).is_err());
    }
    #[test]
    fn overview_toggle_accepts_no_arguments() {
        assert_eq!(
            parse_args(["toggle-overview".into()]).unwrap(),
            ("toggle-overview".into(), json!({}))
        );
        assert!(parse_args(["toggle-overview".into(), "extra".into()]).is_err());
    }

    #[test]
    fn parses_direction_and_workspace_commands() {
        assert_eq!(
            parse_args(["focus".to_owned(), "left".to_owned()]).unwrap().1,
            json!({ "direction": "left" })
        );
        assert_eq!(
            parse_args(["workspace".to_owned(), "7".to_owned()]).unwrap().1,
            json!({ "index": 7 })
        );
        assert_eq!(
            parse_args(["cycle-column-width".to_owned()]).unwrap(),
            ("cycle-column-width".to_owned(), json!({}))
        );
        assert_eq!(
            parse_args(["workspace-back-and-forth".into()]).unwrap(),
            ("workspace-back-and-forth".into(), json!({}))
        );
        assert!(parse_args(["workspace-back-and-forth".into(), "2".into()]).is_err());
        for command in ["focus-last-window", "focus-mru-next", "focus-mru-previous"] {
            assert_eq!(parse_args([command.into()]).unwrap(), (command.into(), json!({})));
            assert!(parse_args([command.into(), "2".into()]).is_err());
        }
    }

    #[test]
    fn screenshot_takes_an_optional_geometry() {
        assert_eq!(
            parse_args(["screenshot".into()]).unwrap(),
            ("screenshot".into(), json!({})),
            "no geometry captures every enabled output"
        );
        assert_eq!(
            parse_args(["screenshot".into(), "-g".into(), "10,-20 300x200".into()])
                .unwrap()
                .1,
            json!({ "geometry": "10,-20 300x200" }),
            "negative origins survive argument parsing"
        );
        assert_eq!(
            parse_args(["screenshot".into(), "--geometry".into(), "0,0 8x8".into()])
                .unwrap()
                .1,
            json!({ "geometry": "0,0 8x8" })
        );
    }

    #[test]
    fn screenshot_rejects_stray_arguments() {
        assert!(parse_args(["screenshot".into(), "extra".into()]).is_err());
        assert!(parse_args(["screenshot".into(), "-g".into()]).is_err());
        assert!(
            parse_args(["screenshot".into(), "0,0 8x8".into()]).is_err(),
            "a bare geometry is not accepted without the flag"
        );
    }

    #[test]
    fn output_commands_have_consistent_arguments() {
        for command in ["outputs", "output-profiles", "output-confirm", "output-revert"] {
            assert_eq!(parse_args([command.into()]).unwrap(), (command.into(), json!({})));
            assert!(parse_args([command.into(), "extra".into()]).is_err());
        }
        assert_eq!(
            parse_args(["output-profile".into(), "auto".into()]).unwrap().1,
            json!({"name": "auto"})
        );
        assert_eq!(
            parse_args(["output-profile".into(), "desk".into()]).unwrap().1,
            json!({"name": "desk"})
        );
        assert_eq!(
            parse_args(["output-internal".into(), "off".into()]).unwrap().1,
            json!({"enabled": false})
        );
        assert_eq!(
            parse_args(["output-internal".into(), "on".into()]).unwrap().1,
            json!({"enabled": true})
        );
        assert!(parse_args(["output-internal".into(), "maybe".into()]).is_err());
        assert!(parse_args(["output-profile".into()]).is_err());
    }

    #[test]
    fn rejects_unknown_or_missing_arguments() {
        assert_eq!(parse_args(["exit".to_owned()]).unwrap(), ("exit".to_owned(), json!({})));
        assert!(parse_args(["exit".to_owned(), "extra".to_owned()]).is_err());
        assert!(parse_args(["focus".to_owned()]).is_err());
        assert!(parse_args(["reload-config".to_owned()]).is_ok());
        assert!(parse_args(["reload-config".to_owned(), "extra".to_owned()]).is_err());
        assert!(parse_args(["get-idle-inhibition".to_owned()]).is_ok());
        assert!(parse_args(["get-idle-inhibition".to_owned(), "extra".to_owned()]).is_err());
    }
}
