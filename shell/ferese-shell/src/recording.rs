//! The bar only controls the recorder and consumes bounded status messages.
//! Encoding and portal negotiation run in the separate ferese-record process.
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Debug, Default)]
pub enum State {
    #[default]
    Idle,
    Selecting,
    Recording(Instant),
    Saving,
    Saved(PathBuf),
    Error(String),
}
#[derive(Clone, Debug, Deserialize)]
pub struct Update {
    state: String,
    message: Option<String>,
    path: Option<PathBuf>,
}
#[derive(Clone, Debug)]
pub enum Event {
    Update(Update),
    Exited,
}
#[derive(Clone)]
struct Events(Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Event>>>);

impl std::hash::Hash for Events {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

impl Events {
    fn stream(&self) -> impl cosmic::iced::futures::Stream<Item = Event> + use<> {
        cosmic::iced::futures::stream::unfold(self.0.clone(), |receiver| async move {
            let event = receiver.lock().await.recv().await?;
            Some((event, receiver))
        })
    }
}

#[derive(Default)]
pub struct Recorder {
    pub state: State,
    control: Option<ChildStdin>,
    updates: Option<Events>,
}
impl Recorder {
    pub fn label(&self) -> &'static str {
        match self.state {
            State::Selecting => "Cancel display selection",
            State::Recording(_) => "Stop recording",
            State::Saving => "Saving recording",
            _ => "Record a display",
        }
    }

    pub fn busy(&self) -> bool {
        self.updates.is_some()
    }

    pub fn elapsed(&self) -> Option<String> {
        let State::Recording(start) = self.state else {
            return None;
        };
        let seconds = start.elapsed().as_secs();
        Some(format!("{:02}:{:02}", seconds / 60, seconds % 60))
    }

    pub fn start(&mut self) {
        if self.busy() {
            return;
        }

        let program = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("ferese-record")))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("ferese-record"));

        match Command::new(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(mut child) => {
                self.control = child.stdin.take();
                let (send, receive) = tokio::sync::mpsc::channel(8);
                self.updates = Some(Events(Arc::new(tokio::sync::Mutex::new(receive))));
                self.state = State::Selecting;

                std::thread::spawn(move || read_events(child, send));
            }
            Err(error) => {
                let message = format!("Cannot start recorder: {error}");
                notify("Recording unavailable", message.clone());
                self.state = State::Error(message);
            }
        }
    }

    pub fn stop(&mut self) {
        if self.busy() && !matches!(self.state, State::Saving) {
            // EOF requests EOS/finalization. Never terminate the encoder abruptly.
            self.control.take();
            self.state = State::Saving;
        }
    }

    pub fn subscription(&self) -> cosmic::iced::Subscription<Event> {
        self.updates
            .as_ref()
            .map_or_else(cosmic::iced::Subscription::none, |events| {
                cosmic::iced::Subscription::run_with(events.clone(), Events::stream)
            })
    }

    pub fn elapsed_subscription(&self) -> cosmic::iced::Subscription<()> {
        fn stream(deadline: &Instant) -> impl cosmic::iced::futures::Stream<Item = ()> + use<> {
            let deadline = *deadline;
            cosmic::iced::futures::stream::once(async move {
                tokio::time::sleep_until(deadline.into()).await;
            })
        }

        self.elapsed_deadline(Instant::now())
            .map_or_else(cosmic::iced::Subscription::none, |deadline| {
                cosmic::iced::Subscription::run_with(deadline, stream)
            })
    }

    fn elapsed_deadline(&self, now: Instant) -> Option<Instant> {
        let State::Recording(start) = self.state else {
            return None;
        };
        Some(start + Duration::from_secs(now.saturating_duration_since(start).as_secs() + 1))
    }

    pub fn handle(&mut self, event: Event) {
        if self.apply(event) {
            match &self.state {
                State::Saved(path) => notify("Recording saved", path.display().to_string()),
                State::Error(message) => notify("Recording failed", message.clone()),
                _ => (),
            }
        }
    }

    fn apply(&mut self, event: Event) -> bool {
        match event {
            Event::Update(update) => match update.state.as_str() {
                "selecting" if self.control.is_some() => self.state = State::Selecting,
                "recording" if self.control.is_some() => {
                    if !matches!(self.state, State::Recording(_)) {
                        self.state = State::Recording(Instant::now());
                    }
                }
                "saving" => self.state = State::Saving,
                "saved" => {
                    self.state = update
                        .path
                        .map(State::Saved)
                        .unwrap_or_else(|| State::Error("Recorder returned no saved file".into()))
                }
                "cancelled" => self.state = State::Idle,
                "error" => self.state = State::Error(update.message.unwrap_or_else(|| "Recording failed".into())),
                _ => (),
            },
            Event::Exited => {
                if matches!(self.state, State::Selecting | State::Recording(_) | State::Saving) {
                    self.state = State::Error("The recorder stopped unexpectedly".into());
                }
                self.control.take();
                self.updates.take();
                return true;
            }
        }
        false
    }
}

fn read_events(mut child: Child, send: tokio::sync::mpsc::Sender<Event>) {
    let mut output = BufReader::new(child.stdout.take().expect("piped recorder output"));

    loop {
        let mut line = String::new();
        match output.by_ref().take(65537).read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line.len() > 65536 => break,
            Ok(_) => {
                if let Ok(update) = serde_json::from_str::<Update>(&line)
                    && send.blocking_send(Event::Update(update)).is_err()
                {
                    break;
                }
            }
        }
    }

    // A rejected status line may leave the child blocked writing into stdout.
    // Close our reader before waiting so that writer can observe the disconnect.
    drop(output);
    let _ = child.wait();
    let _ = send.blocking_send(Event::Exited);
}

// Use the normal notification center for results, without an extra recording menu.
fn notify(title: &'static str, body: String) {
    std::thread::spawn(move || {
        let result = (|| -> zbus::Result<()> {
            let connection = zbus::blocking::connection::Builder::session()?
                .method_timeout(std::time::Duration::from_secs(3))
                .build()?;
            let proxy = zbus::blocking::Proxy::new(
                &connection,
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
            )?;
            let _: u32 = proxy.call(
                "Notify",
                &(
                    "Ferese",
                    0u32,
                    "ferese",
                    title,
                    body,
                    Vec::<String>::new(),
                    std::collections::HashMap::<String, zbus::zvariant::OwnedValue>::new(),
                    -1i32,
                ),
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("ferese-shell: recording notification: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::futures::{FutureExt, StreamExt};

    #[test]
    fn oversized_status_closes_stdout_before_waiting_for_exit() {
        let child = Command::new("head")
            .args(["-c", "16777216", "/dev/zero"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let (send, mut receive) = tokio::sync::mpsc::channel(8);
        let (done_send, done_receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            read_events(child, send);
            done_send.send(()).unwrap();
        });
        let done = done_receive.recv_timeout(Duration::from_secs(2));

        if done.is_err() {
            // Reap a stuck test child before reporting a failure. The reader
            // still owns Child, so this PID cannot have been reused.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }

        worker.join().unwrap();
        assert!(
            done.is_ok(),
            "reader kept the output pipe open while waiting for a blocked writer"
        );
        assert!(matches!(receive.try_recv(), Ok(Event::Exited)));
    }

    fn update(state: &str, path: Option<PathBuf>) -> Event {
        Event::Update(Update {
            state: state.into(),
            message: None,
            path,
        })
    }

    fn session() -> (Recorder, std::process::Child, tokio::sync::mpsc::Sender<Event>) {
        // A real control pipe lets stop() exercise EOF without a recorder,
        // portal, encoder, or D-Bus service.
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let (send, receive) = tokio::sync::mpsc::channel(8);
        let recorder = Recorder {
            state: State::Selecting,
            control: child.stdin.take(),
            updates: Some(Events(Arc::new(tokio::sync::Mutex::new(receive)))),
        };
        (recorder, child, send)
    }

    #[test]
    fn active_recording_events_stop_by_eof_and_keep_terminal_result() {
        let (mut recorder, mut child, send) = session();
        assert_eq!(recorder.subscription().units(), 1);
        assert_eq!(recorder.elapsed_subscription().units(), 0);
        recorder.apply(update("selecting", None));
        assert!(matches!(recorder.state, State::Selecting));
        recorder.apply(update("recording", None));
        let State::Recording(start) = recorder.state else {
            panic!("recording event was lost")
        };
        assert_eq!(recorder.elapsed_subscription().units(), 1);
        recorder.apply(update("recording", None));
        assert!(matches!(recorder.state, State::Recording(same) if same == start));
        recorder.stop();
        assert!(matches!(recorder.state, State::Saving));
        assert!(recorder.control.is_none());
        assert!(recorder.busy());
        assert_eq!(recorder.elapsed_subscription().units(), 0);
        assert!(child.wait().unwrap().success());
        recorder.apply(update("selecting", None));
        recorder.apply(update("recording", None));
        assert!(matches!(recorder.state, State::Saving));
        recorder.apply(update("saving", None));
        recorder.apply(update("saved", Some("display.webm".into())));
        assert!(recorder.apply(Event::Exited));
        assert!(matches!(recorder.state, State::Saved(ref path) if path == &PathBuf::from("display.webm")));
        assert!(!recorder.busy());
        assert_eq!(recorder.subscription().units(), 0);
        assert!(send.try_send(Event::Exited).is_err());
    }

    #[test]
    fn cancelling_selection_uses_eof_and_leaves_no_elapsed_or_event_timer() {
        let (mut recorder, mut child, send) = session();
        recorder.stop();
        assert!(matches!(recorder.state, State::Saving));
        assert!(child.wait().unwrap().success());
        recorder.apply(update("cancelled", None));
        assert!(recorder.apply(Event::Exited));
        assert!(matches!(recorder.state, State::Idle));
        assert_eq!(recorder.subscription().units(), 0);
        assert_eq!(recorder.elapsed_subscription().units(), 0);
        assert!(send.try_send(Event::Exited).is_err());
    }

    #[test]
    fn elapsed_deadlines_only_run_while_recording_and_align_to_start() {
        let start = Instant::now();
        let mut recorder = Recorder::default();
        for state in [
            State::Idle,
            State::Selecting,
            State::Saving,
            State::Saved("video.webm".into()),
            State::Error("failed".into()),
        ] {
            recorder.state = state;
            assert_eq!(recorder.elapsed_deadline(start), None);
        }
        recorder.state = State::Recording(start);
        assert_eq!(
            recorder.elapsed_deadline(start + Duration::from_millis(999)),
            Some(start + Duration::from_secs(1))
        );
        assert_eq!(
            recorder.elapsed_deadline(start + Duration::from_secs(1)),
            Some(start + Duration::from_secs(2))
        );
        assert_eq!(
            recorder.elapsed_deadline(start + Duration::from_millis(5200)),
            Some(start + Duration::from_secs(6))
        );
    }

    #[test]
    fn final_recording_results_survive_exit_and_release_session() {
        for terminal in ["saved", "cancelled", "error"] {
            let mut recorder = Recorder {
                state: State::Saving,
                ..Recorder::default()
            };
            let (send, receive) = tokio::sync::mpsc::channel(8);
            recorder.updates = Some(Events(Arc::new(tokio::sync::Mutex::new(receive))));
            recorder.apply(update(terminal, Some("video.webm".into())));
            assert!(recorder.busy());
            assert!(recorder.apply(Event::Exited));
            assert!(!recorder.busy());
            match terminal {
                "saved" => {
                    assert!(matches!(recorder.state, State::Saved(ref path) if path == &PathBuf::from("video.webm")))
                }
                "cancelled" => assert!(matches!(recorder.state, State::Idle)),
                _ => assert!(matches!(recorder.state, State::Error(ref message) if message == "Recording failed")),
            }
            assert!(send.try_send(Event::Exited).is_err());
        }
    }

    #[test]
    fn missing_path_and_unexpected_exit_remain_errors() {
        let mut recorder = Recorder::default();
        recorder.apply(update("saved", None));
        assert!(matches!(recorder.state, State::Error(ref message) if message == "Recorder returned no saved file"));
        for state in [State::Selecting, State::Recording(Instant::now()), State::Saving] {
            recorder.state = state;
            recorder.apply(Event::Exited);
            assert!(
                matches!(recorder.state, State::Error(ref message) if message == "The recorder stopped unexpectedly")
            );
        }
    }

    #[test]
    fn late_start_events_cannot_restart_stopped_recorder() {
        let mut recorder = Recorder {
            state: State::Saving,
            ..Recorder::default()
        };
        recorder.apply(update("selecting", None));
        recorder.apply(update("recording", None));
        assert!(matches!(recorder.state, State::Saving));
    }

    #[test]
    fn event_stream_wakes_without_polling_and_keeps_bounded_order() {
        let (send, receive) = tokio::sync::mpsc::channel(8);
        let events = Events(Arc::new(tokio::sync::Mutex::new(receive)));
        let stream = events.stream();
        cosmic::iced::futures::pin_mut!(stream);
        assert!(stream.next().now_or_never().is_none());
        for _ in 0..8 {
            send.try_send(update("saving", None)).unwrap();
        }
        assert!(send.try_send(Event::Exited).is_err());
        for _ in 0..8 {
            assert!(
                matches!(stream.next().now_or_never(), Some(Some(Event::Update(Update { state, .. }))) if state == "saving")
            );
        }
        send.try_send(Event::Exited).unwrap();
        assert!(matches!(stream.next().now_or_never(), Some(Some(Event::Exited))));
        drop(send);
        assert!(matches!(stream.next().now_or_never(), Some(None)));
    }
}
