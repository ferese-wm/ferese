use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::OnceLock;

static APP_BACKEND: OnceLock<Option<OsString>> = OnceLock::new();

/// Called at process entry, before the toolkit or workers start threads.
pub(crate) fn configure_shell() {
    let backend = std::env::var_os("ICED_BACKEND");
    let use_shell_default = backend.is_none();
    APP_BACKEND.set(backend).expect("renderer configured once");

    if use_shell_default {
        // Prefer GPU rendering; retain software rendering for unsupported devices.
        // SAFETY: main calls this before starting any threads.
        unsafe { std::env::set_var("ICED_BACKEND", "wgpu,tiny-skia") };
    }
}

pub(crate) fn configure_app(command: &mut Command) {
    // Applications survive cleanup of a crashed desktop helper generation.
    command.process_group(0);
    if let Some(backend) = APP_BACKEND.get() {
        apply_app_backend(command, backend.as_deref());
    }
}

fn apply_app_backend(command: &mut Command, backend: Option<&OsStr>) {
    if let Some(backend) = backend {
        command.env("ICED_BACKEND", backend);
    } else {
        // Applications choose their own default rendering backend.
        command.env_remove("ICED_BACKEND");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_prefers_wgpu_and_preserves_explicit_renderer_choices() {
        const CHILD: &str = "FERESE_TEST_RENDERER_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let original = std::env::var_os("ICED_BACKEND");
            configure_shell();
            assert_eq!(
                std::env::var_os("ICED_BACKEND"),
                Some(original.clone().unwrap_or_else(|| "wgpu,tiny-skia".into()))
            );
            let mut app = Command::new("ferese-settings");
            configure_app(&mut app);
            assert_eq!(
                app.get_envs()
                    .find(|(key, _)| *key == "ICED_BACKEND")
                    .unwrap()
                    .1,
                original.as_deref()
            );
            return;
        }
        // Each child configures its environment before the toolkit starts.
        for backend in [
            None,
            Some("tiny-skia"),
            Some("wgpu"),
            Some("tiny-skia,wgpu"),
        ] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "renderer::tests::shell_prefers_wgpu_and_preserves_explicit_renderer_choices",
                    "--test-threads=1",
                ])
                .env(CHILD, "1");
            if let Some(backend) = backend {
                child.env("ICED_BACKEND", backend);
            } else {
                child.env_remove("ICED_BACKEND");
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "backend {backend:?}:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn settings_does_not_inherit_the_shells_automatic_renderer_preference() {
        let mut command = Command::new("ferese-settings");
        command.env("ICED_BACKEND", "wgpu,tiny-skia");
        apply_app_backend(&mut command, None);
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == "ICED_BACKEND")
                .unwrap()
                .1,
            None
        );
    }

    #[test]
    fn settings_preserves_explicit_renderer_preferences() {
        for backend in ["tiny-skia", "wgpu", "wgpu,tiny-skia"] {
            let mut command = Command::new("ferese-settings");
            apply_app_backend(&mut command, Some(OsStr::new(backend)));
            assert_eq!(
                command
                    .get_envs()
                    .find(|(key, _)| *key == "ICED_BACKEND")
                    .unwrap()
                    .1,
                Some(OsStr::new(backend))
            );
        }
    }
}
