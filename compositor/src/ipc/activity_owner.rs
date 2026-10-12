//! Authenticate the socket peer and its immediate parent before accepting any
//! activity request. pidfds bind monitoring to a process, not a reused PID.
use super::IpcEvent;
use ferese_activities::Owner;
use smithay::reexports::calloop::channel;
use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex};

/// Carries the kernel identity through the IPC queue. Monitoring begins only
/// when the compositor admits the request, so pruning cannot race a pending start.
#[derive(Debug)]
pub(crate) struct VerifiedOwner {
    pub owner: Owner,
    fd: OwnedFd,
}

#[derive(Clone)]
pub(crate) struct Monitor(Arc<(Mutex<BTreeMap<Owner, OwnedFd>>, Condvar)>);
impl Monitor {
    pub fn new(sender: channel::SyncSender<IpcEvent>) -> Self {
        let shared = Arc::new((Mutex::new(BTreeMap::<Owner, OwnedFd>::new()), Condvar::new()));
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("ferese-activity-owners".into())
            .spawn(move || {
                loop {
                    let (lock, ready) = &*worker;
                    let mut owners = lock.lock().expect("activity owners");
                    while owners.is_empty() {
                        owners = ready.wait(owners).expect("activity owners");
                    }
                    let dead: Vec<_> = owners
                        .iter()
                        .filter_map(|(owner, fd)| {
                            let mut poll = libc::pollfd {
                                fd: fd.as_raw_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            };
                            // SAFETY: poll addresses one live, initialized descriptor.
                            (unsafe { libc::poll(&raw mut poll, 1, 0) } > 0).then_some(*owner)
                        })
                        .collect();
                    for owner in &dead {
                        owners.remove(owner);
                    }
                    drop(owners);
                    for owner in dead {
                        if sender.send(IpcEvent::ActivityOwnerExited(owner)).is_err() {
                            return;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            })
            .expect("could not start activity owner monitor");
        Self(shared)
    }
    pub fn retain(&self, owners: &std::collections::BTreeSet<Owner>) {
        self.0
            .0
            .lock()
            .expect("activity owners")
            .retain(|owner, _| owners.contains(owner));
    }
    pub fn verify(&self, stream: &UnixStream, parent: bool) -> io::Result<(VerifiedOwner, Vec<String>)> {
        let peer = credentials(stream)?;
        let peer_pid = u32::try_from(peer.pid).map_err(|_| io::Error::other("invalid peer PID"))?;
        let peer_identity = identity(peer_pid)?;
        let peer_fd = peer_pidfd(stream)?;
        if identity(peer_pid)?.0 != peer_identity.0 || exited(&peer_fd) {
            return Err(io::Error::other("peer identity changed"));
        }
        let pid = if parent { peer_identity.1 } else { peer_pid };
        let (birth, _) = identity(pid)?;
        let fd = pidfd(pid)?;
        if identity(pid)?.0 != birth || exited(&fd) || (parent && identity(peer_pid)? != peer_identity) {
            return Err(io::Error::other("owner identity changed"));
        }
        let metadata = std::fs::metadata(format!("/proc/{pid}"))?;
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != peer.uid {
            return Err(io::Error::other("owner user differs from peer"));
        }
        let owner = Owner { pid, birth };
        let mut parents = Vec::new();
        let mut next = pid;
        for _ in 0..16 {
            let Ok((_, parent)) = identity(next) else {
                break;
            };
            if let Ok(name) = std::fs::read_to_string(format!("/proc/{next}/comm")) {
                parents.push(name.trim().chars().take(128).collect());
            }
            if parent == 0 || parent == next {
                break;
            }
            next = parent;
        }
        Ok((VerifiedOwner { owner, fd }, parents))
    }
    pub fn register(&self, verified: VerifiedOwner) -> io::Result<Owner> {
        let VerifiedOwner { owner, fd } = verified;
        if exited(&fd) {
            return Err(io::Error::other("activity owner exited before admission"));
        }
        let (lock, ready) = &*self.0;
        let mut owners = lock.lock().expect("activity owners");
        if owners.len() >= 128 && !owners.contains_key(&owner) {
            return Err(io::Error::other("owner monitor quota"));
        }
        owners.entry(owner).or_insert(fd);
        ready.notify_one();
        drop(owners);
        Ok(owner)
    }
}
pub(crate) fn exited(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll addresses one initialized descriptor retained by fd.
    unsafe { libc::poll(&raw mut poll, 1, 0) != 0 }
}
fn peer_pidfd(stream: &UnixStream) -> io::Result<OwnedFd> {
    let mut fd = -1_i32;
    let mut len = std::mem::size_of::<i32>() as libc::socklen_t;
    // SO_PEERPIDFD (Linux 6.5+) refers to the process captured when this socket
    // connected, closing the credential-PID reuse race before authentication.
    // SAFETY: both output buffers have the sizes expected by getsockopt.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            77,
            (&raw mut fd).cast(),
            &raw mut len,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if fd < 0 {
        return Err(io::Error::other("missing peer pidfd"));
    }
    // SAFETY: SO_PEERPIDFD returned a new descriptor owned by this caller.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
fn pidfd(pid: u32) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open accepts numeric arguments and returns an owned fd.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
    }
}
fn identity(pid: u32) -> io::Result<(u64, u32)> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat"))?)
}
fn parse_stat(stat: &str) -> io::Result<(u64, u32)> {
    let end = stat
        .rfind(')')
        .ok_or_else(|| io::Error::other("invalid process stat"))?;
    let fields: Vec<_> = stat[end + 1..].split_whitespace().collect();
    let parent = fields
        .get(1)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::other("missing parent identity"))?;
    let birth = fields
        .get(19)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::other("missing process birth"))?;
    Ok((birth, parent))
}
fn credentials(stream: &UnixStream) -> io::Result<libc::ucred> {
    let mut credentials = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: both output buffers have the sizes expected by SO_PEERCRED.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut len,
        )
    } == 0
    {
        Ok(credentials)
    } else {
        Err(io::Error::last_os_error())
    }
}
pub(crate) fn inspect(pid: u32) -> io::Result<(Owner, OwnedFd, Vec<String>)> {
    use std::os::unix::fs::MetadataExt;
    if std::fs::metadata(format!("/proc/{pid}"))?.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other("process belongs to another user"));
    }
    let (birth, _) = identity(pid)?;
    let fd = pidfd(pid)?;
    if identity(pid)?.0 != birth || exited(&fd) {
        return Err(io::Error::other("process disappeared"));
    }
    let mut next = pid;
    let mut parents = Vec::new();
    for _ in 0..16 {
        if let Ok(name) = std::fs::read_to_string(format!("/proc/{next}/comm")) {
            parents.push(name.trim().chars().take(128).collect());
        }
        let Ok((_, parent)) = identity(next) else {
            break;
        };
        if parent == 0 || parent == next {
            break;
        }
        next = parent;
    }
    Ok((Owner { pid, birth }, fd, parents))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_start_survives_monitor_pruning() {
        use ferese_activities::{Data, Source, Template, Value};
        let (sender, _receiver) = channel::sync_channel(16);
        let monitor = Monitor::new(sender);
        let mut engine = crate::activities::Engine::default();
        engine.monitor = Some(monitor.clone());
        let (peer, _other) = UnixStream::pair().unwrap();
        let (verified, _) = monitor.verify(&peer, false).unwrap();
        let owner = verified.owner;
        // A policy timer or a different queued publication runs after verification,
        // before this worker's Start reaches the compositor dispatcher.
        engine.schedule();
        engine
            .publish_verified(
                verified,
                ferese_ipc::activities::Publish::Start {
                    local: "pending-start".into(),
                    data: Data {
                        source: Source::Publisher,
                        output: None,
                        priority: 100,
                        icon: "hammer".into(),
                        title: "Build".into(),
                        value: Value::Indeterminate,
                        template: Template::Progress,
                    },
                    lease_ms: None,
                },
                None,
                Some("test".into()),
            )
            .unwrap();
        assert_eq!(engine.store.entries().count(), 1);
        assert!(
            monitor.0.0.lock().unwrap().contains_key(&owner),
            "accepted lease-free activity has lost its owner monitor"
        );
    }

    #[test]
    fn owner_that_exits_in_the_dispatch_queue_is_rejected() {
        let (sender, _receiver) = channel::sync_channel(16);
        let monitor = Monitor::new(sender);
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let (owner, fd, _) = inspect(child.id()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(monitor.register(VerifiedOwner { owner, fd }).is_err());
        assert!(monitor.0.0.lock().unwrap().is_empty());
    }

    #[test]
    fn process_names_with_spaces_or_parentheses_do_not_shift_birth_field() {
        let fields = (0..20)
            .map(|i| if i == 0 { "S".into() } else { i.to_string() })
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            parse_stat(&format!("42 (name ) with spaces) {fields}")).unwrap(),
            (19, 1)
        );
    }
    #[test]
    fn verifies_kernel_peer_parent_and_monitors_actual_exit() {
        let (sender, _receiver) = channel::sync_channel(16);
        let monitor = Monitor::new(sender);
        let (left, _) = UnixStream::pair().unwrap();
        let (peer, _) = monitor.verify(&left, false).unwrap();
        assert_eq!(peer.owner.pid, std::process::id());
        let (parent, _) = monitor.verify(&left, true).unwrap();
        assert_eq!(parent.owner.pid, unsafe { libc::getppid() } as u32);
        let mut child = std::process::Command::new("sleep").arg("0.05").spawn().unwrap();
        let fd = pidfd(child.id()).unwrap();
        assert!(!exited(&fd));
        child.wait().unwrap();
        assert!(exited(&fd));
    }
}
