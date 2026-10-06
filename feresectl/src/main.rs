mod autostart;

use std::error::Error;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::{env, fs, io};

use ferese_ipc::{Request, Response, VERSION, read_frame, write_frame};
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn Error>> {
    if env::args().nth(1).as_deref() == Some("autostart") {
        if env::args().len() != 2 {
            return Err("usage: feresectl autostart".into());
        }
        return autostart::run();
    }
    if env::args().nth(1).as_deref() == Some("theme") {
        return theme_command(env::args().skip(2).collect());
    }
    if env::args().nth(1).as_deref() == Some("xwayland") {
        return xwayland_command(env::args().skip(2).collect());
    }
    let (command, args) = parse_args(env::args().skip(1))?;
    let request = Request {
        version: VERSION,
        id: 1,
        kind: "command".to_owned(),
        command: command.clone(),
        args,
    };
    let mut stream = UnixStream::connect(socket_path()?)?;

    write_frame(&mut stream, &request)?;
    let response: Response = read_frame(&mut stream)?;
    if let Some(error) = response.error {
        return Err(format!("{}: {}", error.code, error.message).into());
    }

    if matches!(command.as_str(), "screenshot" | "screenshot-window") {
        write_png(&response)
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&response.result.unwrap_or(Value::Null))?
        );
        Ok(())
    }
}

fn theme_command(args: Vec<String>) -> Result<(), Box<dyn Error>> {
    let mut connection = ferese_ipc::theme::Connection::connect()?;
    match args.as_slice() {
        [command] if command == "get" || command == "status" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&connection.get(ferese_config::families::builtins)?)?
            );
        }
        [command] if command == "subscribe" => {
            let mut snapshot = connection.get(ferese_config::families::builtins)?;
            loop {
                println!("{}", serde_json::to_string(&snapshot)?);
                snapshot = connection.watch(snapshot.revision, ferese_config::families::builtins)?;
            }
        }
        [command, mode] if command == "mode" && matches!(mode.as_str(), "light" | "dark" | "auto") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&connection.call("theme-set-mode", json!({"mode": mode}))?)?
            );
        }
        [command, path] if command == "preview" => {
            let source = fs::read_to_string(path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&connection.call(
                    "theme-preview",
                    json!({"source": source, "directory": std::path::Path::new(path).canonicalize()?.parent()})
                )?)?
            );
        }
        _ => {
            return Err(
                "usage: feresectl theme <get|status|subscribe|mode light|mode dark|mode auto|preview PATH>".into(),
            );
        }
    }
    Ok(())
}

fn xwayland_command(args: Vec<String>) -> Result<(), Box<dyn Error>> {
    use ferese_ipc::xwayland::{RETRY_COMMAND, STATUS_COMMAND, Status};

    let command = match args.as_slice() {
        [command] if command == "status" => STATUS_COMMAND,
        [command] if command == "retry" => RETRY_COMMAND,
        _ => return Err("usage: feresectl xwayland <status|retry>".into()),
    };

    let request = Request {
        version: VERSION,
        id: 1,
        kind: "command".to_owned(),
        command: command.to_owned(),
        args: json!({}),
    };
    let mut stream = UnixStream::connect(socket_path()?)?;
    write_frame(&mut stream, &request)?;
    let response: Response = read_frame(&mut stream)?;
    if let Some(error) = response.error {
        return Err(format!("{}: {}", error.code, error.message).into());
    }

    let status = Status::from_value(response.result.unwrap_or(Value::Null))?;
    if command == RETRY_COMMAND {
        println!(
            "X11 retry requested; current state: {}",
            serde_json::to_string(&status.state)?
        );
    }
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
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

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<(String, Value), String> {
    let mut args = args.into_iter();
    let command = args.next().ok_or_else(usage)?;
    let positional = args.collect::<Vec<_>>();
    let payload = match command.as_str() {
        "output-layout" => {
            exactly_one(&command, &positional, "internal-only, external-only, extend or mirror")?;
            if !["internal-only", "external-only", "extend", "mirror"].contains(&positional[0].as_str()) {
                return Err("unknown display layout".into());
            }
            json!({ "layout": positional[0] })
        }
        "output-profile" => {
            exactly_one(&command, &positional, "profile name or auto")?;
            json!({ "name": positional[0] })
        }
        "output-internal" => {
            exactly_one(&command, &positional, "on or off")?;
            let enabled = match positional[0].as_str() {
                "on" => true,
                "off" => false,
                _ => return Err("output-internal requires on or off".into()),
            };
            json!({ "enabled": enabled })
        }
        "screenshot-window" => {
            exactly_one(&command, &positional, "window ID")?;
            let window = positional[0]
                .parse::<u64>()
                .map_err(|_| "Expected an unsigned window ID")?;
            json!({"window":window})
        }
        "focus" | "move" | "resize" => {
            exactly_one(&command, &positional, "direction")?;
            json!({ "direction": positional[0] })
        }
        "workspace" | "move-to-workspace" => {
            exactly_one(&command, &positional, "index")?;
            let index = positional[0]
                .parse::<u32>()
                .map_err(|_| format!("{} requires a positive workspace index", command))?;
            json!({ "index": index })
        }
        "media" => {
            return match positional.as_slice() {
                [] => Ok(("media-get".into(), json!({}))),
                [mode] if mode == "get" => Ok(("media-get".into(), json!({}))),
                [action] if ["play-pause", "next", "previous", "raise", "auto"].contains(&action.as_str()) => {
                    Ok(("media-action".into(), json!({"action": action})))
                }
                [action, player] if ["pin", "ignore", "unignore"].contains(&action.as_str()) => {
                    Ok(("media-action".into(), json!({"action":if action == "pin" { "pin" } else { "ignore" },"player":player,"ignored":action == "ignore"})))
                }
                _ => Err("usage: feresectl media [get|play-pause|next|previous|raise|auto|pin PLAYER|ignore PLAYER|unignore PLAYER]".into()),
            };
        }
        "screenshot" => match positional.as_slice() {
            [] => json!({}),
            [flag, geometry] if flag == "--geometry" || flag == "-g" => {
                json!({ "geometry": geometry })
            }
            _ => {
                return Err(format!(
                    "{command} accepts an optional --geometry \"x,y WxH\" ({})",
                    usage()
                ));
            }
        },
        "toggle-floating"
        | "toggle-fullscreen"
        | "toggle-maximized"
        | "toggle-layout"
        | "toggle-overview"
        | "toggle-keybinding-guide"
        | "cycle-column-width"
        | "center-column"
        | "consume"
        | "expel"
        | "close"
        | "get-focused-window"
        | "get-windows"
        | "get-idle-inhibition"
        | "get-workspaces"
        | "get-outputs"
        | "outputs"
        | "toggle-display-mode"
        | "output-profiles"
        | "output-confirm"
        | "output-revert"
        | "reload-config"
        | "exit"
        | "request-logout"
        | "workspace-back-and-forth"
        | "focus-last-window"
        | "focus-mru-next"
        | "focus-mru-previous" => {
            if !positional.is_empty() {
                return Err(format!("{command} does not accept arguments"));
            }
            json!({})
        }
        _ => return Err(format!("unknown command {command:?}\n{}", usage())),
    };

    Ok((command, payload))
}

fn exactly_one(command: &str, args: &[String], name: &str) -> Result<(), String> {
    if args.len() == 1 {
        Ok(())
    } else {
        Err(format!("{command} requires exactly one {name}"))
    }
}

fn socket_path() -> Result<PathBuf, io::Error> {
    env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|directory| directory.join("ferese/control.sock"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))
}

fn usage() -> String {
    "usage: feresectl media [get|play-pause|next|previous|raise|auto|pin PLAYER|ignore PLAYER|unignore PLAYER]\n       feresectl outputs\n       feresectl output-profiles\n       feresectl <output-confirm|output-revert>\n       feresectl output-layout <internal-only|external-only|extend|mirror>\n       feresectl toggle-display-mode\n       feresectl output-profile <name|auto>\n       feresectl output-internal <on|off>\n       feresectl autostart\n       feresectl xwayland <status|retry>\n       feresectl screenshot [--geometry \"x,y WxH\"]\n       feresectl screenshot-window <window-id>\n       feresectl <focus|move|resize> <direction>\n       feresectl <workspace|move-to-workspace> <index>\n       feresectl workspace-back-and-forth\n       feresectl <focus-last-window|focus-mru-next|focus-mru-previous>\n       feresectl <toggle-floating|toggle-maximized|toggle-fullscreen|toggle-layout|toggle-overview|toggle-keybinding-guide>\n       feresectl <cycle-column-width|center-column|consume|expel|close|exit|request-logout>\n       feresectl <get-focused-window|get-windows|get-workspaces|get-outputs|get-idle-inhibition|reload-config>".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xwayland_commands_map_to_the_shared_ipc_command_names() {
        assert_eq!(ferese_ipc::xwayland::STATUS_COMMAND, "xwayland-status");
        assert_eq!(ferese_ipc::xwayland::RETRY_COMMAND, "xwayland-retry");

        assert!(usage().contains("feresectl xwayland <status|retry>"));
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
