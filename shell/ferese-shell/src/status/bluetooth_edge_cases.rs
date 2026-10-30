use super::*;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str};

struct Controllers;

#[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
impl Controllers {
    fn get_managed_objects(&self) -> ManagedObjects {
        [
            ("/org/bluez/hci0", "00:00:00:00:00:01", true),
            ("/org/bluez/hci1", "00:00:00:00:00:02", false),
        ]
        .into_iter()
        .map(|(path, address, powered)| {
            (
                OwnedObjectPath::try_from(path).unwrap(),
                [(
                    "org.bluez.Adapter1".try_into().unwrap(),
                    [
                        ("Powered".into(), OwnedValue::from(powered)),
                        ("Address".into(), OwnedValue::from(Str::from(address))),
                    ]
                    .into(),
                )]
                .into(),
            )
        })
        .collect()
    }
}

fn selection_case(name: &str, script: &str, selected: bool) {
    const CHILD: &str = "FERESE_TEST_CONTROLLER_SELECTION_CHILD";
    if std::env::var(CHILD).as_deref() != Ok(name) {
        // Change PATH only for a child test process; parallel tests keep their environment.
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let fake = directory.path().join("bluetoothctl");
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let original = std::env::var_os("PATH").unwrap_or_default();
        let paths =
            std::iter::once(directory.path().to_path_buf()).chain(std::env::split_paths(&original));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("status::bluetooth::edge_cases::{name}"),
                "--nocapture",
            ])
            .env(CHILD, name)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
            "child must execute exactly one regression test: {:?}",
            output
        );
        assert!(
            output.status.success(),
            "isolated controller regression failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let bus = crate::status::tests::TestBus::new();
    let _server = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.bluez")
        .unwrap()
        .serve_at("/", Controllers)
        .unwrap()
        .build()
        .unwrap();
    let snapshot = read(&bus.connect()).unwrap();
    if selected {
        assert!(
            !snapshot
                .expect("the explicitly selected controller must be reported")
                .enabled
        );
    } else {
        assert!(
            snapshot.is_none(),
            "a failed selection must not invent the first controller's power state"
        );
    }
}

#[test]
fn edge_case_bluetooth_selection_command_failure_is_unavailable() {
    selection_case(
        "edge_case_bluetooth_selection_command_failure_is_unavailable",
        "#!/bin/sh\nexit 1\n",
        false,
    );
}

#[test]
fn edge_case_bluetooth_missing_selected_controller_is_unavailable() {
    selection_case(
        "edge_case_bluetooth_missing_selected_controller_is_unavailable",
        "#!/bin/sh\nprintf 'Controller 00:00:00:00:00:FF missing\\n'\n",
        false,
    );
}

#[test]
fn edge_case_bluetooth_explicit_second_controller_keeps_its_power_state() {
    selection_case(
        "edge_case_bluetooth_explicit_second_controller_keeps_its_power_state",
        "#!/bin/sh\nprintf 'Controller 00:00:00:00:00:02 selected\\n'\n",
        true,
    );
}
