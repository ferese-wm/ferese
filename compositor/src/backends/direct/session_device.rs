use smithay::backend::session::Session;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::DeviceFd;
use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;

pub(super) struct SessionDevice<S: Session> {
    session: S,
    fd: Option<OwnedFd>,
}

impl<S: Session> SessionDevice<S> {
    pub fn open(mut session: S, path: &Path) -> Result<Self, S::Error> {
        let fd = session.open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )?;
        Ok(Self {
            session,
            fd: Some(fd),
        })
    }

    pub fn device_fd(&self) -> io::Result<DeviceFd> {
        self.fd
            .as_ref()
            .expect("the session device retains its descriptor until Drop")
            .try_clone()
            .map(DeviceFd::from)
    }
}

impl<S: Session> Drop for SessionDevice<S> {
    fn drop(&mut self) {
        if let Err(error) = self.session.close(
            self.fd
                .take()
                .expect("the session device retains its descriptor until Drop"),
        ) {
            tracing::warn!(
                ?error,
                "could not release DRM device through the session manager"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct TestSession(Rc<Cell<Option<i32>>>);

    impl Session for TestSession {
        type Error = ();
        fn open(&mut self, _: &Path, _: OFlags) -> Result<OwnedFd, ()> {
            if self.0.get().is_some() {
                return Err(());
            }
            let fd: OwnedFd = File::open("/dev/null").unwrap().into();
            self.0.set(Some(fd.as_raw_fd()));
            Ok(fd)
        }
        fn close(&mut self, fd: OwnedFd) -> Result<(), ()> {
            assert_eq!(self.0.replace(None), Some(fd.as_raw_fd()));
            Ok(())
        }
        fn change_vt(&mut self, _: i32) -> Result<(), ()> {
            Ok(())
        }
        fn is_active(&self) -> bool {
            true
        }
        fn seat(&self) -> String {
            "seat0".into()
        }
    }

    #[test]
    fn teardown_releases_original_session_lease_before_reopening() {
        let session = TestSession::default();
        for _ in 0..3 {
            let lease = SessionDevice::open(session.clone(), Path::new("/dev/null")).unwrap();
            let rendering = lease.device_fd().unwrap();
            assert_ne!(Some(rendering.as_raw_fd()), session.0.get());
            drop(rendering);
            assert!(session.0.get().is_some());
            drop(lease);
            assert!(session.0.get().is_none());
        }
    }

    #[test]
    fn failed_renderer_initialization_releases_lease() {
        fn initialize(session: TestSession) -> Result<(), ()> {
            let lease = SessionDevice::open(session, Path::new("/dev/null"))?;
            let _rendering = lease.device_fd().unwrap();
            Err(())
        }
        let session = TestSession::default();
        assert!(initialize(session.clone()).is_err());
        assert!(session.0.get().is_none());
        assert!(SessionDevice::open(session, Path::new("/dev/null")).is_ok());
    }
}
