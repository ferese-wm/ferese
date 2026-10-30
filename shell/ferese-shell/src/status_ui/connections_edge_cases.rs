use super::*;
use crate::status::wireless::Security;

fn menu() -> FereseShell {
    let mut shell = crate::tests::shell_with_measured_panel();
    shell.status_service = crate::status::Service::disconnected_for_test();
    shell.status.network = Some(crate::status::Network {
        enabled: true,
        ..Default::default()
    });
    let anchor = super::super::PopoverAnchor {
        parent: shell.outputs[0].bar,
        panel: shell.config.panels[0].id.clone(),
        item: Some(crate::panel::ItemId("network".into())),
        rectangle: Default::default(),
    };
    drop(shell.open_menu(Menu::Network, anchor));
    assert!(shell.connection_menu().is_some());
    shell
}

fn failed_password_can_be_reentered(was_saved: bool) {
    let mut shell = menu();
    let mut network = WifiNetwork {
        path: "/ap/home".into(),
        device: "/device".into(),
        ssid: b"Home".to_vec(),
        name: "Home".into(),
        strength: 80,
        connected: false,
        security: Security::Personal,
        saved: was_saved.then(|| "/saved/home".into()),
    };
    // Drop unpolled tasks: this exercises UI state transitions without connecting a radio.
    drop(shell.update_connections(Event::Wifi(network.clone())));
    if !was_saved {
        assert!(shell.connections.wifi.is_some());
        drop(shell.update_connections(Event::Password(Password::new("wrong-password".into()))));
        drop(shell.update_connections(Event::Submit));
    }
    let serial = shell
        .connections
        .pending
        .as_ref()
        .expect("activation should be pending")
        .serial;
    // NetworkManager creates the profile before authentication completes.
    network.saved = Some("/saved/home".into());
    shell.status.network.as_mut().unwrap().available = Some(vec![network.clone()]);
    drop(shell.update_connections(Event::Finished(
        serial,
        Err("Could not connect. Check the network credentials and try again.".into()),
    )));
    if shell.connections.wifi.is_none() {
        drop(shell.update_connections(Event::Wifi(network)));
    }
    assert!(
        shell.connections.wifi.is_some(),
        "retry after rejected credentials must offer password entry even though a profile exists"
    );
    assert!(
        shell.connections.pending.is_none(),
        "retry must wait for replacement credentials instead of replaying the failed saved profile"
    );
}

#[test]
fn edge_case_new_wifi_wrong_password_can_be_corrected_after_profile_creation() {
    failed_password_can_be_reentered(false);
}

#[test]
fn edge_case_saved_wifi_changed_password_can_be_corrected() {
    failed_password_can_be_reentered(true);
}

#[test]
fn edge_case_dismissing_password_entry_clears_the_secret() {
    let mut shell = menu();
    shell.connections.wifi = Some(WifiNetwork {
        path: "/ap/home".into(),
        device: "/device".into(),
        ssid: b"Home".to_vec(),
        name: "Home".into(),
        strength: 80,
        connected: false,
        security: Security::Personal,
        saved: None,
    });
    drop(shell.update_connections(Event::Password(Password::new("test-password".into()))));
    drop(shell.close_menu());
    assert!(shell.connections.wifi.is_none());
    assert!(shell.connections.password.as_str().is_empty());
}
