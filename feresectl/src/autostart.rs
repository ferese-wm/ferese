use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs};

pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    for key in ["WAYLAND_SOCKET", "FERESE_SHELL_CONTROL_SOCKET"] {
        if let Some(fd) = env::var(key)
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|fd| *fd >= 3)
        {
            // These session-helper descriptors must never reach ordinary startup apps.
            // SAFETY: fcntl receives integer descriptor flags, without pointer arguments; invalid
            // descriptors return an error.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
        }
    }
    let home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or("Missing user config directory")?;
    let system = env::var_os("XDG_CONFIG_DIRS").unwrap_or_default();
    let mut system_directories = env::split_paths(&system)
        .filter(|path| path.is_absolute())
        .collect::<Vec<_>>();
    if system_directories.is_empty() {
        system_directories.push(PathBuf::from("/etc/xdg"));
    }
    let directories = std::iter::once(home)
        .chain(system_directories)
        .collect::<Vec<_>>();
    let mut entries = BTreeMap::new();
    for directory in directories {
        let Ok(files) = fs::read_dir(directory.join("autostart")) else {
            continue;
        };
        let file_iter = files.flatten();
        for file in file_iter {
            if file
                .path()
                .extension()
                .is_some_and(|extension| extension == "desktop")
            {
                entries.entry(file.file_name()).or_insert(file.path());
            }
        }
    }
    let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "Ferese".into());
    for path in entries.into_values() {
        let Some(entry) = read_entry(&path) else {
            continue;
        };
        if !enabled(&entry, &desktop) {
            continue;
        }
        if let Some(program) = entry.get("TryExec").filter(|program| !program.is_empty())
            && !available(program)
        {
            continue;
        }
        let parent = std::process::id() as libc::pid_t;
        let mut command = Command::new("timeout");
        command
            .args(["--kill-after=1s", "15s", "gio", "launch"])
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .env_remove("WAYLAND_SOCKET")
            .env_remove("FERESE_SHELL_CONTROL_SOCKET")
            .env_remove("FERESE_PUBLIC_WAYLAND_DISPLAY");
        // Only signal-safe calls run in the child before exec. A pending launcher
        // dies with this helper, while apps already launched by GIO remain independent.
        // SAFETY: The child hook uses only async-signal-safe prctl, getppid and _exit before
        // exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let result = command.status();
        if !matches!(result, Ok(status) if status.success()) {
            eprintln!("ferese: could not autostart {}", path.display());
        }
    }
    Ok(())
}

fn read_entry(path: &Path) -> Option<BTreeMap<String, String>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut source = String::new();
    file.take(1024 * 1024 + 1)
        .read_to_string(&mut source)
        .ok()?;
    if source.len() > 1024 * 1024 {
        return None;
    }
    let mut in_entry = false;
    let mut fields = BTreeMap::new();
    let line_iter = source.lines().map(str::trim);
    for line in line_iter {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim().to_owned(), value.to_owned());
        }
    }
    Some(fields)
}

fn enabled(entry: &BTreeMap<String, String>, desktop: &str) -> bool {
    if entry.get("Type").is_none_or(|value| value != "Application")
        || entry.get("Hidden").is_some_and(|value| value == "true")
        || entry
            .get("X-GNOME-Autostart-enabled")
            .is_some_and(|value| value == "false")
    {
        return false;
    }
    let matching = |list: &str| {
        list.split(';')
            .filter(|name| !name.is_empty())
            .any(|name| desktop.split(':').any(|current| current == name))
    };
    entry.get("OnlyShowIn").is_none_or(|list| matching(list))
        && entry.get("NotShowIn").is_none_or(|list| !matching(list))
}

fn available(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &Path| {
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        executable(Path::new(program))
    } else {
        env::var_os("PATH").is_some_and(|paths| {
            env::split_paths(&paths).any(|path| executable(&path.join(program)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_hidden_and_desktop_filters() {
        let mut entry = BTreeMap::from([("Type".into(), "Application".into())]);
        assert!(enabled(&entry, "Ferese"));
        entry.insert("OnlyShowIn".into(), "GNOME;".into());
        assert!(!enabled(&entry, "Ferese"));
        entry.insert("OnlyShowIn".into(), "Ferese;".into());
        assert!(enabled(&entry, "Ferese:Other"));
        entry.insert("NotShowIn".into(), "Ferese;".into());
        assert!(!enabled(&entry, "Ferese"));
        entry.remove("NotShowIn");
        entry.insert("Hidden".into(), "true".into());
        assert!(!enabled(&entry, "Ferese"));
    }
}
