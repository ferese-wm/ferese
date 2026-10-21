use super::{Target, apps, sources};
use crate::compositor_ipc;

pub(super) fn run(target: Target, terminal: &[String]) -> Result<(), String> {
    use sources::Action;
    let mut command = match target {
        Target::App { id, desktop } => {
            let entry =
                freedesktop_desktop_entry::DesktopEntry::from_path(&desktop, None::<&[&str]>)
                    .map_err(|e| e.to_string())?;
            let result = if entry.terminal() {
                let arguments = entry.parse_exec().map_err(|e| e.to_string())?;
                let mut command = terminal_command(terminal)?;
                command.args(arguments);
                if let Some(path) = entry.path() {
                    command.current_dir(path);
                }
                spawn(&mut command)
            } else {
                open(
                    "gio",
                    &[std::ffi::OsStr::new("launch"), desktop.as_os_str()],
                )
            };
            if result.is_ok()
                && let Err(error) = apps::record(&id)
            {
                eprintln!("Could not save launcher history: {error}");
            }
            return result;
        }
        Target::File(path) => {
            return open("gio", &[std::ffi::OsStr::new("open"), path.as_os_str()]);
        }
        Target::Action(action @ (Action::Light | Action::Dark | Action::Auto)) => {
            return compositor_ipc::request("theme-set-mode",serde_json::json!({"mode": match action { Action::Light => "light", Action::Dark => "dark", _ => "auto" }})).map(|_|());
        }
        Target::Action(Action::Logout) => {
            return compositor_ipc::request("request-logout", serde_json::json!({})).map(|_| ());
        }
        Target::Action(Action::Lock) => std::process::Command::new("ferese-lock"),
        Target::Action(Action::Screenshot) => return screenshot(),
        _ => return Err("Action unavailable".into()),
    };
    spawn(&mut command)
}
fn terminal_command(terminal: &[String]) -> Result<std::process::Command, String> {
    let (program, options) = terminal
        .split_first()
        .filter(|(program, _)| !program.is_empty())
        .ok_or("No terminal command configured")?;
    let mut command = std::process::Command::new(program);
    command.args(options);
    let name = std::path::Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    match name {
        "foot" | "footclient" | "kitty" | "gnome-terminal" | "kgx" => {
            command.arg("--");
        }
        "wezterm" if !options.iter().any(|option| option == "start") => {
            command.args(["start", "--"]);
        }
        "wezterm" => {
            command.arg("--");
        }
        _ => {
            command.arg("-e");
        }
    }
    Ok(command)
}

fn open(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    public_environment(&mut command);
    use std::io::{Read, Seek};
    let mut stderr = tempfile::tempfile().map_err(|e| e.to_string())?;
    command
        .stdout(std::process::Stdio::null())
        .stderr(stderr.try_clone().map_err(|e| e.to_string())?);
    // Waiting for gio's status must not wait for an application's inherited pipes.
    let status = command.status().map_err(|e| e.to_string())?;
    if status.success() {
        return Ok(());
    }
    stderr.rewind().map_err(|e| e.to_string())?;
    let mut error = String::new();
    let _ = stderr.take(1024).read_to_string(&mut error);
    if error.trim().is_empty() {
        Err(format!("{program} exited with {status}"))
    } else {
        Err(error.trim().chars().take(240).collect())
    }
}
fn public_environment(command: &mut std::process::Command) {
    command
        .stdin(std::process::Stdio::null())
        .env_remove("WAYLAND_SOCKET")
        .env_remove("FERESE_SHELL_CONTROL_SOCKET");
    if let Some(display) = std::env::var_os("FERESE_PUBLIC_WAYLAND_DISPLAY") {
        command.env("WAYLAND_DISPLAY", display);
    }
    command.env_remove("FERESE_PUBLIC_WAYLAND_DISPLAY");
    crate::renderer::configure_app(command);
}
fn spawn(command: &mut std::process::Command) -> Result<(), String> {
    public_environment(command);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn screenshot() -> Result<(), String> {
    let home = std::env::var_os("HOME").ok_or("No home directory")?;
    let directory = std::path::PathBuf::from(home).join("Pictures/Screenshots");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let file = tempfile::Builder::new()
        .prefix("Ferese-")
        .suffix(".png")
        .tempfile_in(&directory)
        .map_err(|e| e.to_string())?;
    let mut command = std::process::Command::new("feresectl");
    command
        .arg("screenshot")
        .stdout(file.reopen().map_err(|e| e.to_string())?)
        .stderr(std::process::Stdio::piped());
    public_environment(&mut command);
    let output = command.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(240)
            .collect());
    }
    file.keep().map_err(|e| e.to_string())?;
    Ok(())
}
