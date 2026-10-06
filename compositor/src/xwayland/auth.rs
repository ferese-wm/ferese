use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use tempfile::NamedTempFile;
use tracing::warn;

/// A private X11 authority file holding one `MIT-MAGIC-COOKIE-1` record.
///
/// This authenticates access to the X server. It does not isolate mutually
/// untrusted applications once they share that X server.
#[derive(Debug)]
pub(crate) struct AuthorityFile {
    file: NamedTempFile,
}

fn random_cookie() -> io::Result<[u8; 16]> {
    let mut cookie = [0_u8; 16];
    let mut filled = 0;
    while filled < cookie.len() {
        let count = unsafe { libc::getrandom(cookie[filled..].as_mut_ptr().cast(), cookie.len() - filled, 0) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "getrandom returned zero"));
        }
        filled += count as usize;
    }

    Ok(cookie)
}

fn hostname() -> io::Result<Vec<u8>> {
    let mut buffer = [0_u8; 256];
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "hostname is not terminated"))?;

    Ok(buffer[..end].to_vec())
}

/// Write one length-prefixed libXau binary field.
fn field(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let length = u16::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Xauthority field is too long"))?;
    output.write_all(&length.to_be_bytes())?;
    output.write_all(bytes)
}

impl AuthorityFile {
    /// Create the authority file under the session's validated
    /// `XDG_RUNTIME_DIR`, for the given display number.
    ///
    /// `FamilyLocal` with the current hostname is used deliberately rather than
    /// a wildcard family. A container with a different hostname may need the
    /// platform's normal Xauthority forwarding instead.
    pub fn create(runtime_directory: &Path, display: u32) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(runtime_directory)?;
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "XDG_RUNTIME_DIR must be a private directory owned by this user",
            ));
        }

        let mut file = tempfile::Builder::new()
            .prefix("ferese-xauth-")
            .tempfile_in(runtime_directory)?;
        file.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;

        let cookie = random_cookie()?;
        let host = hostname()?;
        let number = display.to_string();
        let output = file.as_file_mut();
        output.write_all(&256_u16.to_be_bytes())?; // FamilyLocal
        field(output, &host)?;
        field(output, number.as_bytes())?;
        field(output, b"MIT-MAGIC-COOKIE-1")?;
        field(output, &cookie)?;
        output.flush()?;

        Ok(Self { file })
    }

    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// Release the authority file. The cookie is never logged, returned over
    /// IPC, or included in a diagnostic bundle.
    fn release(&mut self) {
        if let Err(error) = fs::remove_file(self.file.path()) {
            warn!(path = %self.file.path().display(), %error, "could not remove Xauthority file");
        }
    }
}

impl Drop for AuthorityFile {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_runtime_dir(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("ferese-xauth-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    fn read_record(path: &Path) -> (u16, String, String, String, Vec<u8>) {
        let bytes = fs::read(path).unwrap();
        let mut offset = 0;
        let family = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
        offset += 2;

        let take = &|offset: &mut usize| -> Vec<u8> {
            let length = u16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]) as usize;
            *offset += 2;
            let value = bytes[*offset..*offset + length].to_vec();
            *offset += length;
            value
        };

        let host = String::from_utf8(take(&mut offset)).unwrap();
        let display = String::from_utf8(take(&mut offset)).unwrap();
        let name = String::from_utf8(take(&mut offset)).unwrap();
        let cookie = take(&mut offset);
        assert_eq!(offset, bytes.len(), "trailing bytes in authority file");

        (family, host, display, name, cookie)
    }

    #[test]
    fn writes_a_family_local_mit_cookie_record() {
        let runtime = private_runtime_dir("record");
        let file = AuthorityFile::create(&runtime, 7).unwrap();

        let (family, host, display, name, cookie) = read_record(file.path());

        assert_eq!(family, 256, "FamilyLocal");
        assert!(!host.is_empty(), "hostname must be recorded");
        assert_eq!(display, "7");
        assert_eq!(name, "MIT-MAGIC-COOKIE-1");
        assert_eq!(cookie.len(), 16);
    }

    #[test]
    fn the_cookie_is_fresh_for_each_display() {
        let runtime = private_runtime_dir("fresh");
        let first = AuthorityFile::create(&runtime, 1).unwrap();
        let second = AuthorityFile::create(&runtime, 2).unwrap();

        assert_ne!(first.path(), second.path());
        assert_ne!(read_record(first.path()).4, read_record(second.path()).4);
    }

    #[test]
    fn the_file_is_private_and_removed_on_drop() {
        let runtime = private_runtime_dir("private");
        let path = {
            let file = AuthorityFile::create(&runtime, 3).unwrap();
            let path = file.path().to_owned();
            let mode = fs::metadata(&path).unwrap().mode() & 0o777;
            assert_eq!(mode, 0o600, "authority file must not be group/world readable");
            path
        };

        assert!(!path.exists(), "authority file must be removed on drop");
    }

    #[test]
    fn a_group_readable_runtime_directory_is_rejected() {
        let runtime = std::env::temp_dir().join(format!("ferese-xauth-loose-{}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime);
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();

        let error = AuthorityFile::create(&runtime, 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_missing_runtime_directory_is_rejected() {
        let error = AuthorityFile::create(Path::new("/nonexistent/ferese-runtime"), 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    /// Verify the record is actually readable by libXau, not just by our own
    /// parser: `xauth -f PATH list` must decode the file and report the
    /// display, cookie type, and a 16-byte cookie.
    #[test]
    fn the_record_is_decodable_by_libxau_via_xauth() {
        let runtime = private_runtime_dir("libxau");
        let file = AuthorityFile::create(&runtime, 91).unwrap();

        let output = std::process::Command::new("xauth")
            .args(["-f", file.path().to_str().unwrap(), "list"])
            .output()
            .expect("xauth must be available to verify the authority file");

        assert!(
            output.status.success(),
            "xauth rejected the authority file: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let listing = String::from_utf8_lossy(&output.stdout);
        let entry = listing
            .lines()
            .find(|line| line.contains("MIT-MAGIC-COOKIE-1"))
            .unwrap_or_else(|| panic!("xauth did not list the cookie record: {listing:?}"));
        assert!(entry.contains(":91"), "entry must reference the display: {entry:?}");

        let cookie_hex = entry.split_whitespace().last().expect("cookie field");
        assert_eq!(cookie_hex.len(), 32, "16-byte cookie as hex: {cookie_hex}");
        assert!(cookie_hex.chars().any(|c| c != '0'), "cookie must not be zero");
    }

    #[test]
    fn another_user_cannot_read_the_authority_file() {
        let runtime = private_runtime_dir("cross-user");
        let file = AuthorityFile::create(&runtime, 92).unwrap();
        let mode = fs::metadata(file.path()).unwrap().mode() & 0o777;

        // The mode alone is weak evidence; assert the containing directory is
        // private too, since a world-readable runtime dir would defeat it.
        assert_eq!(mode, 0o600);
        assert_eq!(fs::metadata(&runtime).unwrap().mode() & 0o777, 0o700);
    }
}
