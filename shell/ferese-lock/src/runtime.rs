use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};
struct Registry;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Registry {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
pub fn check_protocol() -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|e| format!("Wayland connection: {e}"))?;
    let (globals, _) = registry_queue_init::<Registry>(&connection).map_err(|e| format!("Wayland registry: {e}"))?;
    if globals
        .contents()
        .clone_list()
        .iter()
        .any(|g| g.interface == "ext_session_lock_manager_v1")
    {
        Ok(())
    } else {
        Err("the compositor does not support secure session locking".into())
    }
}
pub fn daemonize() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| format!("Locker executable: {e}"))?;
    let mut child = Command::new(&executable)
        .arg("--foreground")
        .env("FERESE_LOCK_READY", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("Starting locker {}: {e}", executable.display()))?;
    confirm(&mut child)
}

fn confirm(child: &mut Child) -> Result<(), String> {
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let ready = reader.read_line(&mut line).is_ok() && line.trim_end() == "FERESE_LOCKED";
        let _ = tx.send(ready);
    });
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(true) => Ok(()),
        Ok(false) => match child.try_wait() {
            Ok(Some(_)) => Err("locker exited without confirming protection".into()),
            _ => Err("locker did not confirm protection; the locker was left running".into()),
        },
        Err(_) => Err("lock confirmation timed out; the locker was left running".into()),
    }
}

/// Readiness follows the compositor's protocol confirmation, never the request.
#[derive(Default)]
pub struct Confirmation {
    confirmed: bool,
}
impl Confirmation {
    pub fn observe(&mut self, event: &cosmic::iced::event::wayland::SessionLockEvent) -> bool {
        if !matches!(event, cosmic::iced::event::wayland::SessionLockEvent::Locked) || self.confirmed {
            return false;
        }
        self.confirmed = true;
        true
    }
    pub fn confirmed(&self) -> bool {
        self.confirmed
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_confirmation_returns_without_waiting_for_or_killing_the_locker() {
        for message in ["unexpected", "FERESE_LOCKED"] {
            let mut child = Command::new("sh")
                .args(["-c", "printf '%s\\n' \"$1\"; exec sleep 3", "lock-fixture", message])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let result = confirm(&mut child);
            let running = child.try_wait().unwrap().is_none();
            if running {
                child.kill().unwrap();
            }
            child.wait().unwrap();
            assert_eq!(result.is_ok(), message == "FERESE_LOCKED");
            assert!(running, "confirmation must leave the locker running");
        }
    }

    #[test]
    fn only_compositor_confirmation_marks_the_lock_ready_once() {
        use cosmic::iced::event::wayland::SessionLockEvent::{Finished, Locked, NotSupported, Requested};
        let mut c = Confirmation::default();
        for event in [Requested, Requested, Finished, NotSupported] {
            assert!(!c.observe(&event));
            assert!(!c.confirmed());
        }
        assert!(c.observe(&Locked));
        assert!(c.confirmed());
        assert!(!c.observe(&Locked));
    }
}

fn until_next_minute(since_epoch: Duration) -> Duration {
    Duration::from_secs(60) - Duration::new(since_epoch.as_secs() % 60, since_epoch.subsec_nanos())
}

pub fn minute_ticks() -> impl cosmic::iced::futures::Stream<Item = ()> {
    cosmic::iced::futures::stream::unfold((), |()| async {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        tokio::time::sleep(until_next_minute(now)).await;
        Some(((), ()))
    })
}

pub fn retry_deadline(at: &std::time::Instant) -> impl cosmic::iced::futures::Stream<Item = ()> + use<> {
    let at = *at;
    cosmic::iced::futures::stream::once(async move {
        tokio::time::sleep(Duration::from_secs(2).saturating_sub(at.elapsed())).await;
    })
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    #[test]
    fn minute_deadline_uses_the_wall_clock_boundary() {
        assert_eq!(until_next_minute(Duration::ZERO), Duration::from_secs(60));
        assert_eq!(
            until_next_minute(Duration::from_millis(119_999)),
            Duration::from_millis(1)
        );
        assert_eq!(until_next_minute(Duration::from_secs(125)), Duration::from_secs(55));
    }
}
