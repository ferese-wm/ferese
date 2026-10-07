use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// Resolve an IPC target: explicit path, FERESE_SOCKET, then the runtime default.
pub fn resolve(explicit: Option<&Path>) -> io::Result<PathBuf> {
    resolve_with(
        explicit,
        std::env::var_os("FERESE_SOCKET").as_deref(),
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
    )
}

fn resolve_with(explicit: Option<&Path>, socket: Option<&OsStr>, runtime: Option<&OsStr>) -> io::Result<PathBuf> {
    if let Some(path) = explicit {
        if path.as_os_str().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "IPC socket path is empty"));
        }
        return Ok(path.to_owned());
    }
    if let Some(socket) = socket.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(socket));
    }
    runtime
        .filter(|value| !value.is_empty())
        .map(|directory| PathBuf::from(directory).join("ferese/control.sock"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_nested_target_wins_over_the_host_environment() {
        let runtime = Some(OsStr::new("/run/user/1000"));
        let host = OsStr::new("/run/user/1000/ferese/control.sock");
        let nested = Path::new("/tmp/nested/ferese/control.sock");
        assert_eq!(resolve_with(Some(nested), Some(host), runtime).unwrap(), nested);
        assert_eq!(resolve_with(None, Some(nested.as_os_str()), runtime).unwrap(), nested);
        assert_eq!(resolve_with(None, None, runtime).unwrap(), Path::new(host));
        assert_eq!(
            resolve_with(None, Some(OsStr::new("")), runtime).unwrap(),
            Path::new(host)
        );
        assert!(resolve_with(None, None, None).is_err());
        assert!(resolve_with(Some(Path::new("")), Some(host), runtime).is_err());
    }
}
