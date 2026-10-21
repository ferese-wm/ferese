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
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn help_exits_successfully_without_a_session_and_has_hidden_legacy_aliases() {
    for args in [
        &["--help"][..],
        &["theme", "--help"],
        &["launcher", "--help"],
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
        vec![
            "theme",
            "mode",
            "dark",
            "--socket",
            nested.to_str().unwrap(),
            "--json",
        ],
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
        let worker = serve(
            &socket,
            "outputs",
            json!([{"name": "nested", "enabled": true}]),
        );
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
        write_frame(
            &mut stream,
            &Response::success(request.id, json!({"version": VERSION})),
        )
        .unwrap();
        write_frame(&mut stream, &event).unwrap();
        stopped
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
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

#[test]
fn activity_start_uses_versioned_framing_and_parent_scope() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("activity.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request: Request = read_frame(&mut stream).unwrap();
        assert_eq!(request.version, ferese_ipc::VERSION);
        assert_eq!(request.command, "activity-publish");
        let wire: ferese_ipc::activities::Request = serde_json::from_value(request.args).unwrap();
        assert!(matches!(
            wire.owner,
            ferese_ipc::activities::OwnerScope::Parent
        ));
        match wire.publish {
            ferese_ipc::activities::Publish::Start { local, data, .. } => {
                assert_eq!(local, "build");
                assert_eq!(data.value, ferese_activities::Value::Indeterminate);
            }
            _ => panic!("wrong activity action"),
        }
        write_frame(
            &mut stream,
            &Response::success(request.id, json!({"handle":{"generation":1}})),
        )
        .unwrap();
    });
    success(
        ctl(&[
            "--socket",
            socket.to_str().unwrap(),
            "activity",
            "start",
            "build",
            "--title",
            "Build",
        ])
        .output()
        .unwrap(),
    );
    server.join().unwrap();
}

#[test]
fn run_preserves_arguments_cwd_environment_output_and_exit_when_publishing_fails() {
    let dir = tempfile::tempdir().unwrap();
    let output = ctl(&[
        "--socket",
        "/missing/ferese.sock",
        "run",
        "--title",
        "Test",
        "--",
        "sh",
        "-c",
        "printf '%s|%s|%s' \"$1\" \"$ACTIVITY_TEST_VALUE\" \"$PWD\"; printf stderr >&2; exit 17",
        "sh",
        "argument with spaces;$literal",
    ])
    .env("ACTIVITY_TEST_VALUE", "preserved")
    .current_dir(dir.path())
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(17));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "argument with spaces;$literal|preserved|{}",
            dir.path().display()
        )
    );
    assert_eq!(output.stderr, b"stderr");
}

#[test]
fn run_preserves_child_exit_and_signal_status() {
    use std::os::unix::process::ExitStatusExt;
    let output = ctl(&[
        "run",
        "--",
        "sh",
        "-c",
        "printf '{\"reason\":\"build-finished\",\"success\":true}\\n'; exit 19",
    ])
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(19));
    let output = ctl(&["run", "--", "sh", "-c", "kill -TERM $$"])
        .output()
        .unwrap();
    assert_eq!(output.status.signal(), Some(libc::SIGTERM));
}

#[test]
fn wrapper_forwards_termination_and_preserves_job_control_group() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let mut command = ctl(&["run", "--", "sh", "-c", "kill -STOP $$; sleep 30"]);
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let pid = child.id() as i32;
    assert_eq!(unsafe { libc::kill(-pid, libc::SIGCONT) }, 0);
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
}

#[test]
fn terminal_ctrl_c_reaches_child_and_wrapper_returns_sigint() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let mut command = ctl(&["run", "--", "sleep", "30"]);
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(unsafe { libc::kill(-(child.id() as i32), libc::SIGINT) }, 0);
    assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGINT));
}

#[test]
fn a_silent_broker_cannot_prevent_or_change_command_execution() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("blocked.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let worker = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
    });
    let began = std::time::Instant::now();
    let result = ctl(&[
        "--socket",
        socket.to_str().unwrap(),
        "run",
        "--",
        "sh",
        "-c",
        "printf executed; exit 23",
    ])
    .output()
    .unwrap();
    assert_eq!(result.stdout, b"executed");
    assert_eq!(result.status.code(), Some(23));
    assert!(began.elapsed() < std::time::Duration::from_secs(3));
    worker.join().unwrap();
}

fn fake_cargo(dir: &Path, script: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("cargo");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[test]
fn cargo_adapter_completion_does_not_wait_for_descendant_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_cargo(
        dir.path(),
        "#!/bin/sh\nsleep 5 &\necho $! > descendant\nprintf captured\nexit 23\n",
    );
    let output = dir.path().join("output");
    let mut child = ctl(&[
        "--socket",
        "/missing/activity.sock",
        "run",
        "--cargo-json",
        "--",
        "cargo",
        "build",
    ])
    .env("PATH", path)
    .current_dir(dir.path())
    .stdout(std::fs::File::create(&output).unwrap())
    .stderr(std::process::Stdio::null())
    .spawn()
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        thread::sleep(std::time::Duration::from_millis(10));
    };
    let descendant: i32 = std::fs::read_to_string(dir.path().join("descendant"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe {
        libc::kill(descendant, libc::SIGTERM);
    }
    assert_eq!(
        status.and_then(|status| status.code()),
        Some(23),
        "waited for descendant EOF"
    );
    assert_eq!(std::fs::read(&output).unwrap(), b"captured");
}

#[test]
fn stalled_progress_acknowledgements_do_not_block_cargo_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_cargo(
        dir.path(),
        r##"#!/usr/bin/env python3
import json, time, sys
start = time.monotonic()
time.sleep(.3)
for _ in range(20000):
    print('{"reason":"compiler-artifact"}')
sys.stdout.flush()
with open('elapsed', 'w') as f:
    f.write(str(time.monotonic() - start))
print('{"reason":"build-finished","success":true}')
print('child output')
print(json.dumps({'reason': 'compiler-message', 'message': {'rendered': 'diagnostic\n'}}))
sys.exit(19)
"##,
    );
    let socket = dir.path().join("broker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let mut stalled = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(std::time::Duration::from_millis(5));
                continue;
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            let request: Request = read_frame(&mut stream).unwrap();
            match request.args["publish"]["action"].as_str().unwrap() {
                "run" => {
                    let handle = ferese_activities::Id {
                        owner: ferese_activities::Owner { pid: 1, birth: 1 },
                        local: request.args["publish"]["local"].as_str().unwrap().into(),
                        session: 1,
                        generation: 1,
                    };
                    write_frame(
                        &mut stream,
                        &Response::success(
                            request.id,
                            json!({"handle":handle,"progress":"cargo-json"}),
                        ),
                    )
                    .unwrap();
                }
                "update" => stalled.push(stream), // Accept the update but never acknowledge it.
                "end" => {
                    assert_eq!(request.args["publish"]["result"], "failed");
                    write_frame(&mut stream, &Response::success(request.id, json!({}))).unwrap();
                    return stalled.len();
                }
                _ => panic!("unexpected publication"),
            }
        }
        panic!("completion was not published");
    });
    let output = ctl(&[
        "--socket",
        socket.to_str().unwrap(),
        "run",
        "--",
        "cargo",
        "build",
    ])
    .env("PATH", path)
    .current_dir(dir.path())
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(19));
    assert_eq!(output.stdout, b"child output\ndiagnostic\n");
    assert!(
        server.join().unwrap() > 0,
        "probe must exercise progress publishing"
    );
    let elapsed: f64 = std::fs::read_to_string(dir.path().join("elapsed"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        elapsed < 1.5,
        "stdout reader stalled the child for {elapsed}s"
    );
}

#[test]
fn launcher_toggle_uses_the_shell_command() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ipc.sock");
    let server = serve(&path, "toggle-launcher", json!({}));
    success(
        ctl(&["launcher", "toggle"])
            .env("FERESE_SOCKET", &path)
            .output()
            .unwrap(),
    );
    server.join().unwrap();
}

#[test]
fn pomodoro_commands_use_the_versioned_session_api() {
    for action in [
        "start", "pause", "resume", "next", "stop", "reset", "status",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Request = read_frame(&mut stream).unwrap();
            assert_eq!(request.command, "pomodoro");
            assert_eq!(
                request.args,
                json!({"version":1,"action":action,"session":null})
            );
            write_frame(
                &mut stream,
                &Response::success(request.id, json!({"phase":"work"})),
            )
            .unwrap();
        });
        success(
            ctl(&["--socket", socket.to_str().unwrap(), "pomodoro", action])
                .output()
                .unwrap(),
        );
        worker.join().unwrap();
    }
    success(ctl(&["pomodoro", "--help"]).output().unwrap());
}
