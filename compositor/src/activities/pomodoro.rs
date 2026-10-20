use super::*;
use ferese_activities::pomodoro::{Session, Status};
use ferese_ipc::activities::{PomodoroAction, PomodoroRequest};

pub(super) struct Timer {
    pub id: Id,
    pub session: Session,
    output: Option<String>,
    created_at: u64,
}
impl Timer {
    pub fn activity(&self) -> ferese_activities::Activity {
        let now = ferese_ipc::activities::now_ms();
        let value = if let Some(deadline) = self.session.deadline {
            Value::Countdown {
                remaining_ms: deadline.saturating_sub(now),
                sampled_at: now,
            }
        } else {
            Value::PausedCountdown {
                remaining_ms: self.session.remaining_ms,
            }
        };
        ferese_activities::Activity {
            id: self.id.clone(),
            data: Data {
                source: Source::Timer,
                output: self.output.clone(),
                priority: 100,
                icon: "timer".into(),
                title: format!(
                    "Pomodoro · {}{}",
                    self.session.phase.label(),
                    match self.session.status {
                        Status::Paused => " · Paused",
                        Status::Ready => " · Ready",
                        Status::Finished => " · Finished",
                        _ => "",
                    }
                ),
                value,
                template: Template::Timer,
            },
            state: State::Running,
            created_at: self.created_at,
            eligible_at: self.created_at,
            refreshed_at: self.created_at,
            lease_deadline: None,
            result_deadline: None,
            result_message: None,
            flash_on_fail: false,
            result_flash_ms: 0,
            group: None,
        }
    }
}
impl Engine {
    pub fn pomodoro_control(
        &mut self,
        request: PomodoroRequest,
        output: Option<String>,
    ) -> Result<serde_json::Value, String> {
        if request.version != ferese_ipc::activities::VERSION {
            return Err("Unsupported activity schema".into());
        }
        if let Some(id) = &request.session
            && self.pomodoro.as_ref().is_none_or(|timer| &timer.id != id)
        {
            return Err("Pomodoro session changed; refresh its controls".into());
        }
        let now = ferese_ipc::activities::now_ms();
        if !matches!(request.action, PomodoroAction::Stop | PomodoroAction::Reset) && self.advance_pomodoro(now) {
            self.changed();
        }
        match request.action {
            PomodoroAction::Status => {}
            PomodoroAction::Start | PomodoroAction::Reset => {
                if request.action == PomodoroAction::Start
                    && self
                        .pomodoro
                        .as_ref()
                        .is_some_and(|timer| timer.session.status != Status::Finished)
                {
                    return Err("A Pomodoro session already exists; resume or reset it".into());
                }
                let session = Session::start(self.pomodoro_settings.clone(), now)?;
                // Verify the long-lived host, never the short-lived CLI, as owner.
                let (owner, _pidfd, _) =
                    crate::ipc::activity_owner::inspect(std::process::id()).map_err(|error| error.to_string())?;
                let id = Id {
                    owner,
                    local: "pomodoro".into(),
                    generation: self.revision,
                    session: self.store.session(),
                };
                self.pomodoro = Some(Timer {
                    id,
                    session,
                    output,
                    created_at: now,
                });
            }
            PomodoroAction::Stop => {
                self.pomodoro = None;
            }
            action => {
                let timer = self.pomodoro.as_mut().ok_or("No Pomodoro session; start one first")?;
                match action {
                    PomodoroAction::Pause => timer.session.pause(now)?,
                    PomodoroAction::Resume => timer.session.resume(now)?,
                    PomodoroAction::Next => timer.session.next(now)?,
                    _ => unreachable!(),
                }
            }
        }
        if request.action != PomodoroAction::Status {
            self.changed();
        }
        let snapshot = self.snapshot();
        serde_json::to_value(&snapshot.pomodoro).map_err(|error| error.to_string())
    }
    pub(super) fn advance_pomodoro(&mut self, now: u64) -> bool {
        let Some(timer) = &mut self.pomodoro else {
            return false;
        };
        let Some(completion) = timer.session.advance(now) else {
            return false;
        };
        if timer.session.settings.notifications {
            let title = format!("Pomodoro: {} complete", completion.phase.label());
            let body = completion.next.map_or_else(
                || "Your Pomodoro session is finished.".into(),
                |phase| {
                    format!(
                        "{} {}.",
                        phase.label(),
                        if timer.session.status == Status::Running {
                            "has started"
                        } else {
                            "is ready"
                        }
                    )
                },
            );
            let alerts = self.alerts.get_or_insert_with(Alerts::new);
            if alerts.sender.try_send((title, body)).is_err() {
                tracing::warn!("Pomodoro alert queue unavailable");
            }
        }
        true
    }
    pub fn pomodoro_remove_output(&mut self, output: &str, fallback: Option<&str>) {
        if let Some(timer) = &mut self.pomodoro
            && timer.output.as_deref() == Some(output)
        {
            timer.output = fallback.map(str::to_owned);
        }
    }
    pub fn pomodoro_attach_output(&mut self, output: &str) -> bool {
        if let Some(timer) = &mut self.pomodoro
            && timer.output.is_none()
        {
            timer.output = Some(output.into());
            return true;
        }
        false
    }
}

/// Notification delivery runs off the compositor thread and is attempted once.
/// Reconnection never replays an alert; the notification server applies DND and
/// lock-screen privacy, just as it does for other applications.
pub(super) struct Alerts {
    sender: std::sync::mpsc::SyncSender<(String, String)>,
}
impl Alerts {
    fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<(String, String)>(16);
        std::thread::spawn(move || {
            while let Ok((title, body)) = receiver.recv() {
                let result = (|| -> zbus::Result<()> {
                    let connection = zbus::blocking::connection::Builder::session()?
                        .method_timeout(Duration::from_secs(2))
                        .build()?;
                    let mut hints = std::collections::HashMap::<&str, zbus::zvariant::Value<'_>>::new();
                    hints.insert("suppress-sound", zbus::zvariant::Value::Bool(true));
                    let reply = connection.call_method(
                        Some("org.freedesktop.Notifications"),
                        "/org/freedesktop/Notifications",
                        Some("org.freedesktop.Notifications"),
                        "Notify",
                        &("Ferese", 0_u32, "", title, body, Vec::<String>::new(), hints, -1_i32),
                    )?;
                    let _: u32 = reply.body().deserialize()?;
                    Ok(())
                })();
                if let Err(error) = result {
                    tracing::warn!(%error,"Pomodoro completion alert could not be delivered");
                }
            }
        });
        Self { sender }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn control(engine: &mut Engine, action: PomodoroAction) -> Result<serde_json::Value, String> {
        engine.pomodoro_control(
            PomodoroRequest {
                version: ferese_ipc::activities::VERSION,
                action,
                session: None,
            },
            Some("test".into()),
        )
    }
    #[test]
    fn completion_uses_the_standard_notification_service_without_sound() {
        const TEST: &str =
            "activities::pomodoro::tests::completion_uses_the_standard_notification_service_without_sound";
        if !crate::startup_tests::private_runtime(TEST) {
            return;
        }
        use std::io::BufRead;
        struct Bus(std::process::Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut bus = Bus(std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap());
        let mut address = String::new();
        std::io::BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        // SAFETY: private_runtime runs only this test in a separate process;
        // set the bus before starting the notification service or alert worker.
        unsafe {
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address.trim());
        }
        struct Notifications(std::sync::mpsc::Sender<(String, String, bool)>);
        #[zbus::interface(name = "org.freedesktop.Notifications")]
        impl Notifications {
            #[allow(clippy::too_many_arguments)]
            fn notify(
                &self,
                _app_name: &str,
                _replaces_id: u32,
                _app_icon: &str,
                summary: &str,
                body: &str,
                _actions: Vec<String>,
                hints: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
                _expire_timeout: i32,
            ) -> u32 {
                let silent = hints
                    .get("suppress-sound")
                    .and_then(|value| bool::try_from(value).ok())
                    .unwrap_or(false);
                self.0.send((summary.into(), body.into(), silent)).unwrap();
                1
            }
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let _service = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .serve_at("/org/freedesktop/Notifications", Notifications(sender))
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .build()
            .unwrap();
        let mut engine = Engine::default();
        control(&mut engine, PomodoroAction::Start).unwrap();
        let deadline = engine.pomodoro.as_ref().unwrap().session.deadline.unwrap();
        engine.advance_pomodoro(deadline);
        let (title, body, silent) = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(title, "Pomodoro: Work complete");
        assert_eq!(body, "Short break is ready.");
        assert!(silent);
        engine.advance_pomodoro(deadline + 1);
        assert!(receiver.try_recv().is_err());
    }
    #[test]
    fn host_owns_the_single_session_and_stale_popup_controls_are_rejected() {
        let mut engine = Engine::default();
        let first = control(&mut engine, PomodoroAction::Start).unwrap();
        let first: ferese_ipc::activities::Pomodoro = serde_json::from_value(first).unwrap();
        assert_eq!(first.id.owner.pid, std::process::id());
        assert!(control(&mut engine, PomodoroAction::Start).is_err());
        engine.remove_connection(10);
        engine.owner_exit(Owner { pid: 999_999, birth: 0 });
        assert_eq!(engine.snapshot().pomodoro.as_ref().unwrap().id, first.id);
        assert_eq!(engine.snapshot().activities.len(), 1);
        control(&mut engine, PomodoroAction::Reset).unwrap();
        assert_ne!(engine.snapshot().pomodoro.as_ref().unwrap().id, first.id);
        assert!(
            engine
                .pomodoro_control(
                    PomodoroRequest {
                        version: ferese_ipc::activities::VERSION,
                        action: PomodoroAction::Stop,
                        session: Some(first.id)
                    },
                    None
                )
                .is_err()
        );
        control(&mut engine, PomodoroAction::Stop).unwrap();
        assert!(engine.snapshot().pomodoro.is_none());
        assert!(engine.snapshot().activities.is_empty());
        assert!(Engine::default().snapshot().pomodoro.is_none());
    }
    #[test]
    fn completions_are_delivered_once_and_cancellation_does_not_alert() {
        let mut engine = Engine::default();
        let (sender, receiver) = std::sync::mpsc::sync_channel(16);
        engine.alerts = Some(Alerts { sender });
        control(&mut engine, PomodoroAction::Start).unwrap();
        let deadline = engine.pomodoro.as_ref().unwrap().session.deadline.unwrap();
        assert!(engine.advance_pomodoro(deadline));
        assert!(!engine.advance_pomodoro(deadline + 1));
        let (title, body) = receiver.try_recv().unwrap();
        assert_eq!(title, "Pomodoro: Work complete");
        assert_eq!(body, "Short break is ready.");
        assert!(receiver.try_recv().is_err());
        control(&mut engine, PomodoroAction::Reset).unwrap();
        engine.pomodoro.as_mut().unwrap().session.deadline = Some(1);
        control(&mut engine, PomodoroAction::Stop).unwrap();
        assert!(receiver.try_recv().is_err());
        control(&mut engine, PomodoroAction::Start).unwrap();
        engine.pomodoro.as_mut().unwrap().session.deadline = Some(1);
        control(&mut engine, PomodoroAction::Reset).unwrap();
        assert!(receiver.try_recv().is_err());
    }
    #[test]
    fn paused_snapshot_is_constant_and_output_loss_routes_the_timer() {
        let mut engine = Engine::default();
        control(&mut engine, PomodoroAction::Start).unwrap();
        control(&mut engine, PomodoroAction::Pause).unwrap();
        let snapshot = engine.snapshot();
        let activity = &snapshot.activities[0];
        assert!(activity.data.title.contains("Paused"));
        assert_eq!(
            activity.data.value.remaining_ms(0),
            activity.data.value.remaining_ms(u64::MAX)
        );
        engine.pomodoro_remove_output("test", None);
        assert!(engine.pomodoro_attach_output("new"));
        engine.changed();
        assert_eq!(engine.snapshot().activities[0].data.output.as_deref(), Some("new"));
    }
    #[test]
    fn disabled_alerts_and_schema_errors_have_no_side_effects() {
        let mut engine = Engine::default();
        engine.pomodoro_settings.notifications = false;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        engine.alerts = Some(Alerts { sender });
        control(&mut engine, PomodoroAction::Start).unwrap();
        assert!(
            engine
                .pomodoro_control(
                    PomodoroRequest {
                        version: 99,
                        action: PomodoroAction::Stop,
                        session: None
                    },
                    None
                )
                .is_err()
        );
        let deadline = engine.pomodoro.as_ref().unwrap().session.deadline.unwrap();
        engine.advance_pomodoro(deadline);
        assert!(receiver.try_recv().is_err());
    }
}
