mod output;
mod progress;
pub(super) mod setup;
mod transport;
use clap::{Subcommand, ValueEnum};
use ferese_activities::{Data, Id, Source, State, Template, Value};
use ferese_ipc::activities::{OwnerScope, Publish, Request, VERSION};
use serde_json::json;
use std::error::Error;
use std::io::{self, BufRead, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Enable shell activities and add the panel item if not already configured.
    Setup {
        #[arg(long)]
        config: std::path::PathBuf,
    },
    Start {
        local: String,
        #[arg(long, default_value = "ferese")]
        icon: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        progress: Option<f32>,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        source: Option<String>,
        /// Typed Progress, Timer, Media, or List payload as JSON.
        #[arg(long)]
        template: Option<String>,
        #[arg(long)]
        remaining_ms: Option<u64>,
        #[arg(long)]
        output: Option<String>,
        #[arg(long, default_value_t = 100)]
        priority: u8,
        #[arg(long)]
        lease_ms: Option<u64>,
    },
    Update {
        local: String,
        #[arg(long)]
        progress: Option<f32>,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        remaining_ms: Option<u64>,
        #[arg(long)]
        lease_ms: Option<u64>,
        #[arg(long)]
        handle: Option<String>,
    },
    End {
        local: String,
        #[arg(long)]
        result: ResultKind,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        handle: Option<String>,
    },
    Cancel {
        local: String,
        #[arg(long)]
        handle: Option<String>,
    },
    /// Report an observed command to opt-in rules without executing it.
    Hook {
        local: String,
        #[arg(long)]
        command: String,
        #[arg(long)]
        cwd: String,
    },
    /// Show all current activity records.
    List,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ResultKind {
    Ok,
    Fail,
    Cancelled,
}
fn handle(value: Option<&str>) -> Result<Option<Id>, Box<dyn Error>> {
    value
        .map(|value| serde_json::from_str(value).map_err(Into::into))
        .transpose()
}
fn value(
    progress: Option<f32>,
    message: Option<&str>,
    remaining_ms: Option<u64>,
) -> Result<Option<Value>, Box<dyn Error>> {
    if usize::from(progress.is_some())
        + usize::from(message.is_some())
        + usize::from(remaining_ms.is_some())
        > 1
    {
        return Err("choose one activity value".into());
    }
    Ok(progress
        .map(Value::Progress)
        .or_else(|| message.map(|message| Value::Text(message.to_owned())))
        .or_else(|| {
            remaining_ms.map(|remaining_ms| Value::Countdown {
                remaining_ms,
                sampled_at: 0,
            })
        }))
}
fn publish(
    socket: &Path,
    owner: OwnerScope,
    publish: Publish,
) -> Result<serde_json::Value, Box<dyn Error>> {
    publish_with_timeout(socket, owner, publish, std::time::Duration::from_secs(2))
}
fn publish_with_timeout(
    socket: &Path,
    owner: OwnerScope,
    publish: Publish,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let mut stream = transport::connect(socket, timeout)?;
    super::request(
        &mut stream,
        "activity-publish",
        json!(Request {
            version: VERSION,
            owner,
            publish
        }),
    )
}

pub fn command(action: &Action, socket: &Path, json_output: bool) -> Result<(), Box<dyn Error>> {
    let request = match action {
        Action::Setup { .. } => unreachable!("setup is an offline command"),
        Action::Start {
            local,
            icon,
            title,
            progress,
            remaining_ms,
            message,
            source,
            template,
            output,
            priority,
            lease_ms,
        } => {
            let value = value(*progress, message.as_deref(), *remaining_ms)?
                .unwrap_or(Value::Indeterminate);
            let data = Data {
                source: if let Some(source) = source {
                    serde_json::from_value(json!(source))?
                } else if remaining_ms.is_some() {
                    Source::Timer
                } else {
                    Source::Publisher
                },
                output: output.clone(),
                priority: *priority,
                icon: icon.clone(),
                title: title.clone(),
                template: if let Some(template) = template {
                    if template.len() > 4_096 {
                        return Err("template exceeds message budget".into());
                    }
                    serde_json::from_str(template)?
                } else if remaining_ms.is_some() {
                    Template::Timer
                } else {
                    Template::Progress
                },
                value,
            };
            Publish::Start {
                local: local.clone(),
                data,
                lease_ms: *lease_ms,
            }
        }
        Action::Update {
            local,
            progress,
            message,
            remaining_ms,
            lease_ms,
            handle: token,
        } => Publish::Update {
            local: local.clone(),
            handle: handle(token.as_deref())?,
            value: value(*progress, message.as_deref(), *remaining_ms)?,
            lease_ms: *lease_ms,
        },
        Action::End {
            local,
            result,
            message,
            handle: token,
        } => Publish::End {
            local: local.clone(),
            handle: handle(token.as_deref())?,
            result: match result {
                ResultKind::Ok => State::Done,
                ResultKind::Fail => State::Failed,
                ResultKind::Cancelled => State::Cancelled,
            },
            message: message.clone(),
        },
        Action::Cancel {
            local,
            handle: token,
        } => Publish::Cancel {
            local: local.clone(),
            handle: handle(token.as_deref())?,
        },
        Action::Hook {
            local,
            command,
            cwd,
        } => Publish::Hook {
            local: local.clone(),
            command: command.clone(),
            cwd: cwd.clone(),
        },
        Action::List => {
            let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
            return super::output::print(
                &super::request(&mut stream, "activity-get", json!({}))?,
                json_output,
            );
        }
    };
    let result = publish(socket, OwnerScope::Parent, request)?;
    // Handles are machine-readable even when general queries use tabular output.
    super::output::print(&result, true)
}

static CHILD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
extern "C" fn terminate(signal: i32) {
    let pid = CHILD.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: kill is async-signal-safe; the PID belongs to this wrapper's child.
    if pid > 0 {
        unsafe {
            libc::kill(pid, signal);
        }
    }
}

pub fn run(
    title: Option<&str>,
    args: &[String],
    socket: Option<&Path>,
    cargo_json: bool,
) -> Result<(), Box<dyn Error>> {
    let Some(program) = args.first() else {
        return Err("run requires a command after --".into());
    };
    let local = format!("run-{}", std::process::id());
    let mut token = None;
    let mut configured_adapter = false;
    if let Some(socket) = socket {
        let metadata = args.join(" ");
        if metadata.len() <= 4_096 {
            let result = publish(
                socket,
                OwnerScope::Publisher,
                Publish::Run {
                    local: local.clone(),
                    title: title.map(str::to_owned),
                    command: metadata,
                    cwd: std::env::current_dir()
                        .ok()
                        .map(|path| path.to_string_lossy().into_owned())
                        .filter(|cwd| cwd.len() <= 1_024)
                        .unwrap_or_default(),
                },
            )
            .ok();
            if let Some(result) = result {
                configured_adapter = result["progress"] == "cargo-json";
                token = serde_json::from_value(result["handle"].clone()).ok();
            }
        }
    }
    let adapter = (configured_adapter || cargo_json)
        && program == "cargo"
        && args.get(1).is_some_and(|operation| {
            matches!(
                operation.as_str(),
                "build" | "check" | "test" | "run" | "clippy"
            )
        });
    let mut command = Command::new(program);
    command.args(&args[1..]);
    if adapter && !args.iter().any(|arg| arg.starts_with("--message-format")) {
        // Cargo options must precede the program arguments following `--`.
        let split = args[1..]
            .iter()
            .position(|arg| arg == "--")
            .map(|index| index + 1)
            .unwrap_or(args.len());
        command = Command::new(program);
        command
            .args(&args[1..split])
            .arg("--message-format=json-render-diagnostics")
            .args(&args[split..]);
    }
    command
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(if adapter {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });
    // Terminal-generated signals reach both processes in the shell's job group.
    // The wrapper waits for the actual child result; the child resets inherited
    // dispositions before exec. SIGTSTP/SIGCONT retain normal job control.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, terminate as *const () as usize);
        command.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            if let (Some(socket), Some(handle)) = (socket, token) {
                let _ = publish(
                    socket,
                    OwnerScope::Publisher,
                    Publish::End {
                        local,
                        handle: Some(handle),
                        result: State::Failed,
                        message: Some("Could not start command".into()),
                    },
                );
            }
            return Err(error.into());
        }
    };
    CHILD.store(child.id() as i32, std::sync::atomic::Ordering::Relaxed);
    let publisher = socket
        .zip(token)
        .map(|(socket, handle)| progress::Publisher::new(socket.to_owned(), local, handle));
    let drain_deadline = output::Deadline::default();
    let reader = child.stdout.take().map(|stdout| {
        let deadline = drain_deadline.clone();
        let updates = publisher
            .as_ref()
            .map(|publisher| publisher.updates.clone());
        let (done, finished) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut input = io::BufReader::new(output::Output {
                pipe: stdout,
                deadline,
            });
            let mut stdout = io::stdout().lock();
            let mut units = 0_u64;
            let mut last = std::time::Instant::now();
            loop {
                let mut bytes = Vec::new();
                // Read bounded chunks, preserving arbitrary non-JSON output.
                let Ok(count) = input.by_ref().take(65_537).read_until(b'\n', &mut bytes) else {
                    break;
                };
                if count == 0 {
                    break;
                }
                if bytes.len() <= 65_536
                    && bytes.first() == Some(&b'{')
                    && let Ok(message) = serde_json::from_slice::<serde_json::Value>(&bytes)
                {
                    if message["reason"] == "compiler-message" {
                        if let Some(rendered) = message["message"]["rendered"].as_str() {
                            let _ = stdout.write_all(rendered.as_bytes());
                        }
                    } else if message["reason"] == "compiler-artifact" {
                        units = units.saturating_add(1);
                    } else if message["reason"] != "build-finished"
                        && message["reason"] != "build-script-executed"
                    {
                        let _ = stdout.write_all(&bytes);
                    }
                    if last.elapsed() >= std::time::Duration::from_millis(200) {
                        if let Some(updates) = &updates {
                            updates.progress(units);
                        }
                        last = std::time::Instant::now();
                    }
                    continue;
                }
                if stdout
                    .write_all(&bytes)
                    .and_then(|_| stdout.flush())
                    .is_err()
                {
                    break;
                }
            }
            let _ = stdout.flush();
            let _ = done.send(());
        });
        finished
    });
    let status = child.wait()?;
    CHILD.store(0, std::sync::atomic::Ordering::Relaxed);
    *drain_deadline.lock().unwrap() =
        Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
    if let Some(reader) = reader {
        // Descendants may inherit the pipe after the direct child exits. Drain
        // buffered output briefly, but never wait for those descendants' EOF.
        let _ = reader.recv_timeout(std::time::Duration::from_millis(350));
    }
    if let Some(publisher) = publisher {
        let result = if matches!(
            status.signal(),
            Some(libc::SIGINT | libc::SIGTERM | libc::SIGQUIT)
        ) {
            State::Cancelled
        } else if status.success() {
            State::Done
        } else {
            State::Failed
        };
        publisher.finish(result);
    }
    if let Some(signal) = status.signal() {
        // Preserve signal termination as well as the conventional shell status.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        std::process::exit(128 + signal);
    }
    std::process::exit(status.code().unwrap_or(1));
}
