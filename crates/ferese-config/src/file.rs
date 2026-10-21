use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const LIMIT: usize = 60 * 1024;

pub struct ConfigFile {
    path: PathBuf,
    source: String,
    // Keep the sidecar inode: deleting it would let another writer lock a new inode.
    _lock: File,
}

impl ConfigFile {
    pub fn open(path: &Path) -> Result<Self, String> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;

        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => parent
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join(path.file_name().ok_or("Invalid configuration path")?),
            Err(error) => return Err(error.to_string()),
        };
        let mut lock_name = path
            .file_name()
            .ok_or("Invalid configuration path")?
            .to_os_string();
        lock_name.push(".lock");

        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_file_name(lock_name))
            .map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => {
                "Configuration is being saved by another app. Please try again.".into()
            }
            fs::TryLockError::Error(error) => error.to_string(),
        })?;

        let mut file = Self {
            path,
            source: String::new(),
            _lock: lock,
        };
        file.source = read(&file.path)?;
        Ok(file)
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn replace(
        self,
        desired: &str,
        backup: Option<&Path>,
        validate: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(), String> {
        if desired.len() > LIMIT {
            return Err("Configuration exceeds the 60 KiB limit.".into());
        }

        let parent = self.path.parent().ok_or("Invalid configuration path")?;
        let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;

        if let Ok(metadata) = fs::metadata(&self.path) {
            staged
                .as_file()
                .set_permissions(metadata.permissions())
                .map_err(|e| e.to_string())?;
        }

        staged
            .write_all(desired.as_bytes())
            .map_err(|e| e.to_string())?;
        staged.as_file().sync_all().map_err(|e| e.to_string())?;
        validate(staged.path())?;

        if read(&self.path)? != self.source {
            return Err("Configuration changed while saving. Please reload and try again.".into());
        }

        if let Some(path) = backup {
            let mut previous =
                tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            previous
                .write_all(self.source.as_bytes())
                .map_err(|e| e.to_string())?;
            previous.as_file().sync_all().map_err(|e| e.to_string())?;
            previous.persist(path).map_err(|e| e.to_string())?;
        }

        staged.persist(&self.path).map_err(|e| e.to_string())?;

        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| e.to_string())?;

        Ok(())
    }
}

impl Drop for ConfigFile {
    fn drop(&mut self) {
        // A spawned child can briefly retain the open-file description after our
        // descriptor closes. Release the lock before closing the owner's handle.
        let _ = self._lock.unlock();
    }
}

fn read(path: &Path) -> Result<String, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut source = String::new();
    file.take((LIMIT + 1) as u64)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() > LIMIT {
        return Err("Configuration exceeds the 60 KiB limit.".into());
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_excludes_other_processes_and_survives_replacement() {
        const CHILD: &str = "FERESE_CONFIG_LOCK_TEST_PATH";
        if let Some(path) = std::env::var_os(CHILD) {
            assert!(
                ConfigFile::open(Path::new(&path))
                    .err()
                    .unwrap()
                    .contains("another app")
            );
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        for source in ["// first\n", "// second\n"] {
            let file = ConfigFile::open(&path).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "file::tests::lock_excludes_other_processes_and_survives_replacement",
                ])
                .env(CHILD, &path)
                .status()
                .unwrap();
            assert!(status.success());
            file.replace(source, None, |_| Ok(())).unwrap();
            assert_eq!(ConfigFile::open(&path).unwrap().source(), source);
        }
    }

    #[test]
    fn symlink_aliases_share_a_lock_and_preserve_the_link() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        let alias = directory.path().join("alias.kdl");
        fs::write(&path, "// initial").unwrap();
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let file = ConfigFile::open(&path).unwrap();
        assert!(ConfigFile::open(&alias).is_err());
        // A concurrent process spawn can briefly inherit this open-file description.
        let inherited = file._lock.try_clone().unwrap();
        drop(file);
        ConfigFile::open(&alias)
            .unwrap()
            .replace("// updated", None, |_| Ok(()))
            .unwrap();
        assert!(alias.is_symlink());
        assert_eq!(fs::read_to_string(path).unwrap(), "// updated");
        drop(inherited);
    }

    #[test]
    fn failed_validation_releases_lock_and_preserves_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        fs::write(&path, "// initial").unwrap();
        let file = ConfigFile::open(&path).unwrap();
        assert!(
            file.replace("// rejected", None, |_| Err("invalid".into()))
                .is_err()
        );
        assert_eq!(ConfigFile::open(&path).unwrap().source(), "// initial");
    }
}
