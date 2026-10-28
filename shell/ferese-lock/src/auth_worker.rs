//! A stalled PAM module must not strand the interface or leave authentication threads behind.
use std::io;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::auth::{self, Outcome};
use crate::password::Password;

const TIMEOUT: Duration = Duration::from_secs(60);

/// Internal subprocess entry point. It has no Wayland connection or unlock authority.
pub(super) fn run() -> io::Result<i32> {
    // SAFETY: PR_SET_DUMPABLE accepts an integer and dereferences no caller memory.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let password = Password::read_from(&mut io::stdin().lock())?;
    let user = auth::username().map_err(io::Error::other)?;
    Ok(match auth::authenticate(&user, &password) {
        Outcome::Accepted => 10,
        Outcome::Rejected => 11,
        Outcome::AccountAttention => 12,
        Outcome::Unavailable => 13,
    })
}

pub(super) async fn authenticate(password: Password) -> Outcome {
    let result = async {
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--authenticate")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        complete(&mut child, password, TIMEOUT).await
    }
    .await;
    match result {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("ferese-lock: authentication worker failed: {error}");
            Outcome::Unavailable
        }
    }
}

async fn complete(child: &mut Child, password: Password, timeout: Duration) -> io::Result<Outcome> {
    use tokio::io::AsyncWriteExt;
    let result = tokio::time::timeout(timeout, async {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing worker pipe"))?;
        let bytes = password.as_bytes_with_nul();
        let length = bytes.len() - 1;
        stdin.write_all(&(length as u16).to_le_bytes()).await?;
        stdin.write_all(&bytes[..length]).await?;
        drop(stdin);
        drop(password);
        let status = child.wait().await?;
        Ok(match status.code() {
            Some(10) => Outcome::Accepted,
            Some(11) => Outcome::Rejected,
            Some(12) => Outcome::AccountAttention,
            _ => Outcome::Unavailable,
        })
    })
    .await;
    match result {
        Ok(Ok(outcome)) => Ok(outcome),
        failed => {
            // The unreaped child reserves this PID and owns a separate process group.
            // Stop PAM helpers too, then reap before allowing another attempt.
            if let Some(pid) = child.id() {
                // SAFETY: The positive child PID identifies its own process group. It cannot
                // be reused before wait reaps the child; the negative value targets that group.
                if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } != 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error);
                    }
                }
            }
            child.kill().await?;
            match failed {
                Ok(Err(error)) => Err(error),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "authentication timed out",
                )),
                Ok(Ok(_)) => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_reaps_worker_and_unexpected_exit_never_authenticates() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("worker test runtime");
        runtime.block_on(async {
            for (command, timeout, expected) in [
                ("exec sleep 30", Duration::from_millis(30), None),
                (
                    "cat >/dev/null; exit 0",
                    Duration::from_secs(2),
                    Some(Outcome::Unavailable),
                ),
                (
                    "cat >/dev/null; exit 11",
                    Duration::from_secs(2),
                    Some(Outcome::Rejected),
                ),
                (
                    "cat >/dev/null; exit 12",
                    Duration::from_secs(2),
                    Some(Outcome::AccountAttention),
                ),
            ] {
                let mut child = Command::new("sh")
                    .args(["-c", command])
                    .stdin(Stdio::piped())
                    .process_group(0)
                    .kill_on_drop(true)
                    .spawn()
                    .expect("fixture worker");
                let mut password = Password::try_new().expect("test password memory");
                assert!(password.push_str("test-only"));
                let result = complete(&mut child, password, timeout).await;
                if let Some(expected) = expected {
                    assert_eq!(result.expect("worker result"), expected);
                } else {
                    assert_eq!(
                        result.expect_err("deadline").kind(),
                        io::ErrorKind::TimedOut
                    );
                }
                assert!(
                    child.try_wait().expect("worker status").is_some(),
                    "worker must be reaped"
                );
            }
        });
    }
}
