use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output};
use std::thread;

use ferese_ipc::{Request, Response, read_frame, write_frame};
use serde_json::json;

fn ctl(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_feresectl"));
    command
        .args(args)
        .env_remove("FERESE_SOCKET")
        .env_remove("XDG_RUNTIME_DIR");
    command
}

fn serve(path: &Path, expected: &'static str, value: serde_json::Value) -> thread::JoinHandle<()> {
    let listener = UnixListener::bind(path).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let request: Request = read_frame(&mut stream).unwrap();
        assert_eq!(request.command, expected);
        write_frame(&mut stream, &Response::success(request.id, value)).unwrap();
    })
}

fn success(output: Output) -> String {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn help_exits_successfully_without_a_session_and_has_hidden_legacy_aliases() {
    for args in [
        &["--help"][..],
        &["theme", "--help"],
        &["media", "--help"],
        &["xwayland", "--help"],
        &["screenshot", "--help"],
        &["focus-floating", "--help"],
    ] {
        let text = success(ctl(args).output().unwrap());
        assert!(text.contains("Usage:"));
        assert!(!text.contains("get-outputs"));
    }
    let output = ctl(&["widows"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("windows"), "{error}");
}

#[test]
fn focus_floating_sends_a_parameterless_action_and_rejects_cycle_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let request: Request = read_frame(&mut stream).unwrap();
        assert_eq!(request.command, "focus-floating");
        assert_eq!(request.args, json!({}));
        write_frame(&mut stream, &Response::success(request.id, json!({}))).unwrap();
    });
    success(
        ctl(&["--socket", socket.to_str().unwrap(), "focus-floating"])
            .output()
            .unwrap(),
    );
    worker.join().unwrap();
    let output = ctl(&["focus-floating", "next"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn explicit_socket_targets_the_nested_instance_for_queries_and_theme_commands() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("nested.sock");
    let host = directory.path().join("host.sock");
    let host_listener = UnixListener::bind(&host).unwrap();
    host_listener.set_nonblocking(true).unwrap();
    for args in [
        vec!["--socket", nested.to_str().unwrap(), "-j", "windows"],
        vec!["theme", "mode", "dark", "--socket", nested.to_str().unwrap(), "--json"],
    ] {
        let worker = serve(
            &nested,
            if args[0] == "theme" {
                "theme-set-mode"
            } else {
                "get-windows"
            },
            json!({"instance": "nested"}),
        );
        let output = success(ctl(&args).env("FERESE_SOCKET", &host).output().unwrap());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&output).unwrap(),
            json!({"instance": "nested"})
        );
        worker.join().unwrap();
        std::fs::remove_file(&nested).unwrap();
        assert_eq!(
            host_listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn socket_environment_and_runtime_fallback_use_the_same_query_path() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("ferese")).unwrap();
    let socket = directory.path().join("ferese/control.sock");
    for environment in ["FERESE_SOCKET", "XDG_RUNTIME_DIR"] {
        let worker = serve(&socket, "outputs", json!([{"name": "nested", "enabled": true}]));
        let target = if environment == "FERESE_SOCKET" {
            socket.as_path()
        } else {
            directory.path()
        };
        let output = success(ctl(&["outputs"]).env(environment, target).output().unwrap());
        assert!(output.contains("name: nested"));
        assert!(!output.contains('"'));
        worker.join().unwrap();
        std::fs::remove_file(&socket).unwrap();
    }
}

#[test]
fn legacy_queries_are_hidden_aliases_with_an_explicit_json_flag() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("control.sock");
    let worker = serve(&socket, "get-workspaces", json!([]));
    let output = ctl(&["--socket", socket.to_str().unwrap(), "get-workspaces", "-j"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("deprecated; use workspaces"));
    assert_eq!(success(output).trim(), "[]");
    worker.join().unwrap();
}

#[test]
fn event_stream_uses_the_selected_socket_and_prints_complete_json_lines() {
    use ferese_ipc::events::{Change, Event, VERSION};
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("events.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let event = Event {
        version: VERSION,
        generation: 7,
        last: true,
        change: Change::LockChanged {
            lock: ferese_ipc::events::LockState {
                phase: "locked".into(),
                sleeping: false,
            },
        },
    };
    let expected = event.clone();
    let (stop, stopped) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request: Request = read_frame(&mut stream).unwrap();
        assert_eq!(request.command, "event-stream");
        assert_eq!(request.args["version"], VERSION);
        write_frame(&mut stream, &Response::success(request.id, json!({"version": VERSION}))).unwrap();
        write_frame(&mut stream, &event).unwrap();
        stopped.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    });
    let mut child = ctl(&["event-stream", "--socket", socket.to_str().unwrap()])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), expected);
    child.kill().unwrap();
    child.wait().unwrap();
    stop.send(()).unwrap();
    worker.join().unwrap();
}
