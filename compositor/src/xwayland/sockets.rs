use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::ops::Range;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::{SocketAddr, UnixListener};
use std::path::{Path, PathBuf};

use tracing::warn;

// Identity checks prevent accidental unlinking, not races with hostile same-UID code.
#[derive(Debug)]
struct CreatedPath {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl CreatedPath {
    fn record(path: PathBuf, metadata: &fs::Metadata) -> Self {
        Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn remove(&mut self) {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.dev() == self.device && metadata.ino() == self.inode => {
                if let Err(error) = fs::remove_file(&self.path) {
                    warn!(
                        path = %self.path.display(),
                        %error,
                        "could not remove X11 path this process created"
                    );
                }
            }
            Ok(_) => warn!(
                path = %self.path.display(),
                "refusing to remove an X11 path that no longer matches the one this process created"
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => warn!(
                path = %self.path.display(),
                %error,
                "could not inspect X11 path during cleanup"
            ),
        }
    }
}

impl Drop for CreatedPath {
    fn drop(&mut self) {
        self.remove();
    }
}

fn validate_shared_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.file_type().is_symlink() || (metadata.uid() != 0 && metadata.uid() != uid) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("untrusted X11 directory: {}", path.display()),
        ));
    }

    if metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("shared X11 directory is not sticky: {}", path.display()),
        ));
    }

    Ok(())
}

#[derive(Debug)]
pub(crate) struct Reservation {
    display: u32,
    listeners: Vec<UnixListener>,
    socket_path: Option<CreatedPath>,
    lock_path: Option<CreatedPath>,
    socket_directory: PathBuf,
}

impl Reservation {
    // Leave pre-existing locks and socket paths untouched.
    pub fn allocate() -> io::Result<Self> {
        #[cfg(test)]
        {
            // Owned inputs, so an ordinary test never competes for a real slot in
            // `/tmp/.X11-unix` nor depends on that directory existing.
            let (sockets, locks) = crate::xwayland::test_hooks::allocator_root_for_reservation().unwrap_or_else(|| {
                let root = crate::xwayland::test_hooks::default_allocator_root();
                (root.clone(), root)
            });
            Self::allocate_in(&sockets, &locks, 0..64)
        }

        #[cfg(not(test))]
        Self::allocate_in(Path::new("/tmp/.X11-unix"), Path::new("/tmp"), 0..64)
    }

    fn allocate_in(socket_directory: &Path, lock_directory: &Path, candidates: Range<u32>) -> io::Result<Self> {
        validate_shared_directory(socket_directory)?;
        validate_shared_directory(lock_directory)?;

        for display in candidates {
            let path = lock_directory.join(format!(".X{display}-lock"));
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&path)
            {
                Ok(file) => file,

                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };

            let lock_path = CreatedPath::record(path, &file.metadata()?);
            if let Err(error) = writeln!(file, "{:>10}", std::process::id()).and_then(|()| file.flush()) {
                drop(file);
                drop(lock_path);
                return Err(error);
            }
            drop(file);

            let mut reservation = Self {
                display,
                listeners: Vec::new(),
                socket_path: None,
                lock_path: Some(lock_path),
                socket_directory: socket_directory.to_owned(),
            };

            match reservation.bind_same_display() {
                Ok(()) => return Ok(reservation),
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                    drop(reservation);
                }
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "no unused X11 display in the configured internal candidate range",
        ))
    }

    pub fn display_number(&self) -> u32 {
        self.display
    }

    pub fn display_name(&self) -> String {
        format!(":{}", self.display)
    }

    pub fn listeners(&self) -> &[UnixListener] {
        &self.listeners
    }

    // Remove source clones and stop the server before closing the reservation.
    pub fn close_listeners(&mut self) {
        self.listeners.clear();
        self.socket_path = None;
    }

    pub fn bind_same_display(&mut self) -> io::Result<()> {
        #[cfg(test)]
        if crate::xwayland::test_hooks::fire(&crate::xwayland::test_hooks::FAIL_REBIND_AFTER) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "injected X11 display rebind failure",
            ));
        }

        if !self.listeners.is_empty() || self.socket_path.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "X11 listeners are already bound",
            ));
        }

        validate_shared_directory(&self.socket_directory)?;
        let path = self.socket_directory.join(format!("X{}", self.display));
        let filesystem = UnixListener::bind(&path)?;
        let owned_path = CreatedPath::record(path.clone(), &fs::symlink_metadata(&path)?);
        let abstract_address = SocketAddr::from_abstract_name(path.as_os_str().as_bytes())?;
        let abstract_socket = match UnixListener::bind_addr(&abstract_address) {
            Ok(listener) => listener,
            Err(error) => {
                // Drop the recorded path first so cleanup happens in order, once.
                drop(owned_path);
                drop(filesystem);
                return Err(error);
            }
        };

        self.listeners = vec![filesystem, abstract_socket];
        self.socket_path = Some(owned_path);

        Ok(())
    }

    fn release_display(&mut self) {
        self.close_listeners();
        self.lock_path = None;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.release_display();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn shared_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ferese-xwayland-alloc-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for directory in ["sockets", "locks"] {
            fs::create_dir(root.join(directory)).unwrap();
            fs::set_permissions(root.join(directory), fs::Permissions::from_mode(0o1777)).unwrap();
        }
        root
    }

    #[test]
    fn allocates_a_display_with_two_listener_transports() {
        let root = shared_root("two-transports");
        let reservation = Reservation::allocate_in(&root.join("sockets"), &root.join("locks"), 0..64).unwrap();

        assert!(reservation.listeners().len() >= 2);
        let socket = root.join("sockets").join(format!("X{}", reservation.display_number()));
        assert!(socket.exists(), "conventional socket path must exist");
        assert!(
            root.join("locks")
                .join(format!(".X{}-lock", reservation.display_number()))
                .exists()
        );
    }

    #[test]
    fn skips_an_occupied_display_without_reclaiming_it() {
        let root = shared_root("occupied");
        let sockets = root.join("sockets");
        let locks = root.join("locks");
        let first = Reservation::allocate_in(&sockets, &locks, 0..64).unwrap();
        let taken = first.display_number();

        let second = Reservation::allocate_in(&sockets, &locks, 0..64).unwrap();
        assert_ne!(second.display_number(), taken);
        assert!(second.display_number() > taken);

        assert!(locks.join(format!(".X{taken}-lock")).exists());
        assert!(sockets.join(format!("X{taken}")).exists());
    }

    #[test]
    fn an_existing_lock_file_is_left_untouched() {
        let root = shared_root("stale-lock");
        let locks = root.join("locks");
        let stale = locks.join(".X3-lock");
        fs::write(&stale, b"9999\n").unwrap();

        let reservation = Reservation::allocate_in(&root.join("sockets"), &locks, 0..64).unwrap();
        assert_ne!(reservation.display_number(), 3, "occupied number must be skipped");
        assert_eq!(fs::read(&stale).unwrap(), b"9999\n", "stale lock must not be reclaimed");
    }

    #[test]
    fn cleanup_removes_paths_this_process_created() {
        let root = shared_root("cleanup");
        let reservation = Reservation::allocate_in(&root.join("sockets"), &root.join("locks"), 0..64).unwrap();
        let display = reservation.display_number();
        let socket = root.join("sockets").join(format!("X{display}"));
        let lock = root.join("locks").join(format!(".X{display}-lock"));

        drop(reservation);

        assert!(!socket.exists(), "owned socket path must be removed");
        assert!(!lock.exists(), "owned lock path must be removed");
    }

    #[test]
    fn close_listeners_keeps_the_display_lock_owned() {
        let root = shared_root("close-listeners");
        let mut reservation = Reservation::allocate_in(&root.join("sockets"), &root.join("locks"), 0..64).unwrap();
        let display = reservation.display_number();

        reservation.close_listeners();

        assert!(reservation.listeners().is_empty());
        assert!(root.join("locks").join(format!(".X{display}-lock")).exists());
        assert!(!root.join("sockets").join(format!("X{display}")).exists());
    }

    #[test]
    fn retry_rebinds_the_same_display_number() {
        let root = shared_root("retry");
        let mut reservation = Reservation::allocate_in(&root.join("sockets"), &root.join("locks"), 0..64).unwrap();
        let display = reservation.display_number();
        reservation.close_listeners();

        reservation.bind_same_display().unwrap();

        assert_eq!(reservation.display_number(), display);
        assert!(!reservation.listeners().is_empty());
        assert!(root.join("sockets").join(format!("X{display}")).exists());
    }

    #[test]
    fn binding_twice_is_rejected() {
        let root = shared_root("double-bind");
        let mut reservation = Reservation::allocate_in(&root.join("sockets"), &root.join("locks"), 0..64).unwrap();
        reservation.close_listeners();
        reservation.bind_same_display().unwrap();

        let error = reservation.bind_same_display().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn an_untrusted_socket_directory_is_rejected() {
        let root = std::env::temp_dir().join(format!("ferese-xwayland-untrusted-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();

        let locks = root.join("locks");
        fs::create_dir(&locks).unwrap();
        fs::set_permissions(&locks, fs::Permissions::from_mode(0o1777)).unwrap();

        let error = Reservation::allocate_in(&root, &locks, 0..64).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("not sticky"));
    }

    #[test]
    fn a_missing_socket_directory_is_an_actionable_error() {
        let root = shared_root("missing-dir");
        let error = Reservation::allocate_in(&root.join("absent"), &root.join("locks"), 0..64).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn exhausting_the_candidate_range_reports_no_free_display() {
        let root = shared_root("exhausted");
        let locks = root.join("locks");
        for display in 10..13 {
            fs::write(locks.join(format!(".X{display}-lock")), b"1\n").unwrap();
        }

        let error = Reservation::allocate_in(&root.join("sockets"), &locks, 10..13).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert!(error.to_string().contains("no unused X11 display"));
    }
}
