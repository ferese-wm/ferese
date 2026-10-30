use zbus::zvariant::Value;

use super::*;

fn profile(key: &str) -> SavedProfile {
    SavedProfile {
        path: "/saved".into(),
        settings: HashMap::from([
            (
                "802-11-wireless".into(),
                HashMap::from([(
                    "ssid".into(),
                    Value::from(b"Home".as_slice()).try_to_owned().unwrap(),
                )]),
            ),
            (
                "802-11-wireless-security".into(),
                HashMap::from([("key-mgmt".into(), Value::from(key).try_to_owned().unwrap())]),
            ),
        ]),
    }
}

fn ap(rsn: u32) -> Properties {
    HashMap::from([
        ("Flags".into(), 1_u32.into()),
        ("RsnFlags".into(), rsn.into()),
        ("Mode".into(), 2_u32.into()),
        ("Frequency".into(), 5180_u32.into()),
        (
            "HwAddress".into(),
            Value::from("12:34:56:78:9a:bc").try_to_owned().unwrap(),
        ),
    ])
}

#[test]
fn personal_profiles_preserve_negotiation_and_explicit_wpa3_requirements() {
    for (key, advertised, expected) in [
        ("wpa-psk", KEY_PSK, true),
        ("wpa-psk", KEY_SAE, true),
        ("wpa-psk", KEY_PSK | KEY_SAE, true),
        ("sae", KEY_PSK, false),
        ("sae", KEY_SAE, true),
        ("sae", KEY_PSK | KEY_SAE, true),
    ] {
        assert_eq!(
            profile(key).matches(b"Home", &ap(advertised)),
            expected,
            "{key} with {advertised:x}"
        );
    }
    let mut restricted = profile("wpa-psk");
    restricted
        .settings
        .get_mut("802-11-wireless-security")
        .unwrap()
        .insert("pmf".into(), 1_i32.into());
    assert!(!restricted.matches(b"Home", &ap(KEY_SAE)));
    assert!(restricted.matches(b"Home", &ap(KEY_PSK | KEY_SAE)));
}

#[test]
fn enterprise_profiles_match_the_advertised_key_management() {
    assert!(!profile("wpa-eap-suite-b-192").matches(b"Home", &ap(KEY_EAP)));
    assert!(profile("wpa-eap-suite-b-192").matches(b"Home", &ap(KEY_SUITE_B)));
    assert!(profile("wpa-eap").matches(b"Home", &ap(KEY_EAP)));
    assert!(!profile("wpa-eap").matches(b"Home", &ap(KEY_SUITE_B)));
}

#[test]
fn ap_restrictions_exclude_only_incompatible_profiles() {
    let access_point = ap(KEY_PSK);
    for (name, rejected, accepted) in [
        (
            "bssid",
            Value::from(&[0_u8; 6][..]),
            Value::from(&[0x12_u8, 0x34, 0x56, 0x78, 0x9a, 0xbc][..]),
        ),
        ("band", Value::from("bg"), Value::from("a")),
        ("channel", Value::from(40_u32), Value::from(36_u32)),
        ("mode", Value::from("ap"), Value::from("infrastructure")),
    ] {
        let mut restricted = profile("wpa-psk");
        restricted
            .settings
            .get_mut("802-11-wireless")
            .unwrap()
            .insert(name.into(), rejected.try_to_owned().unwrap());
        assert!(
            !restricted.matches(b"Home", &access_point),
            "{name} restriction was ignored"
        );
        restricted
            .settings
            .get_mut("802-11-wireless")
            .unwrap()
            .insert(name.into(), accepted.try_to_owned().unwrap());
        assert!(
            restricted.matches(b"Home", &access_point),
            "compatible {name} restriction was rejected"
        );
    }
}

#[test]
fn protocol_and_cipher_restrictions_apply_to_the_same_advertisement() {
    let mut restricted = profile("wpa-psk");
    let settings = restricted
        .settings
        .get_mut("802-11-wireless-security")
        .unwrap();
    settings.insert(
        "proto".into(),
        Value::from(vec!["wpa"]).try_to_owned().unwrap(),
    );
    assert!(!restricted.matches(b"Home", &ap(KEY_SAE)));
    let settings = restricted
        .settings
        .get_mut("802-11-wireless-security")
        .unwrap();
    settings.remove("proto");
    settings.insert(
        "pairwise".into(),
        Value::from(vec!["ccmp"]).try_to_owned().unwrap(),
    );
    settings.insert(
        "group".into(),
        Value::from(vec!["ccmp"]).try_to_owned().unwrap(),
    );
    assert!(!restricted.matches(b"Home", &ap(KEY_PSK | PAIR_TKIP | GROUP_TKIP)));
    assert!(restricted.matches(b"Home", &ap(KEY_PSK | PAIR_CCMP | GROUP_CCMP)));
    let mut mixed = ap(KEY_PSK | GROUP_CCMP);
    mixed.insert("WpaFlags".into(), (KEY_PSK | PAIR_CCMP).into());
    assert!(
        !restricted.matches(b"Home", &mixed),
        "ciphers from different protocol versions cannot be combined"
    );
}

#[test]
fn common_and_special_channels_keep_their_band() {
    for (frequency, channel) in [
        (2412, 1),
        (2484, 14),
        (4910, 182),
        (5180, 36),
        (5935, 2),
        (5955, 1),
        (7115, 233),
    ] {
        assert_eq!(frequency_channel(frequency), Some(channel));
    }
    assert_eq!(frequency_channel(5181), None);
}

struct RestrictedProfile {
    bssid: [u8; 6],
    band: &'static str,
    mode: &'static str,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl RestrictedProfile {
    fn get_settings(&self) -> Settings {
        let mut settings = profile("wpa-psk").settings;
        let wireless = settings.get_mut("802-11-wireless").unwrap();
        wireless.insert(
            "bssid".into(),
            Value::from(self.bssid.as_slice()).try_to_owned().unwrap(),
        );
        wireless.insert(
            "band".into(),
            Value::from(self.band).try_to_owned().unwrap(),
        );
        wireless.insert(
            "mode".into(),
            Value::from(self.mode).try_to_owned().unwrap(),
        );
        settings
    }
}

struct Device(std::sync::Arc<std::sync::Mutex<Vec<zbus::zvariant::OwnedObjectPath>>>);
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    fn available_connections(&self) -> Vec<zbus::zvariant::OwnedObjectPath> {
        self.0.lock().unwrap().clone()
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        30
    }
}

struct Radio;
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Radio {
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::zvariant::OwnedObjectPath {
        "/".try_into().unwrap()
    }
    fn get_all_access_points(&self) -> Vec<zbus::zvariant::OwnedObjectPath> {
        vec!["/ap".try_into().unwrap()]
    }
}

struct AccessPoint(std::sync::Arc<std::sync::atomic::AtomicU32>);
#[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        b"Home".to_vec()
    }
    #[zbus(property)]
    fn flags(&self) -> u32 {
        PRIVACY
    }
    #[zbus(property)]
    fn rsn_flags(&self) -> u32 {
        KEY_PSK
    }
    #[zbus(property)]
    fn mode(&self) -> u32 {
        2
    }
    #[zbus(property)]
    fn frequency(&self) -> u32 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[zbus(property)]
    fn hw_address(&self) -> &str {
        "12:34:56:78:9a:bc"
    }
}

struct Manager(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    fn activate_connection(
        &self,
        profile: zbus::zvariant::OwnedObjectPath,
        _device: zbus::zvariant::OwnedObjectPath,
        _ap: zbus::zvariant::OwnedObjectPath,
    ) -> zbus::zvariant::OwnedObjectPath {
        self.0.lock().unwrap().push(profile.to_string());
        "/active".try_into().unwrap()
    }
}

struct Active;
#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl Active {
    #[zbus(property)]
    fn state(&self) -> u32 {
        2
    }
}

#[test]
fn private_bus_profile_selection_respects_device_and_ap_restrictions_at_activation() {
    use super::super::{NM, NM_PATH, Password, connect_wifi_on, wifi_networks};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    };
    let bus = crate::status::tests::TestBus::new();
    // /saved/a would otherwise sort first, but NM has excluded it for this device.
    let available = Arc::new(Mutex::new(
        ["/saved/b", "/saved/c", "/saved/d", "/saved/z"]
            .map(|path| path.try_into().unwrap())
            .to_vec(),
    ));
    let frequency = Arc::new(AtomicU32::new(5180));
    let activations = Arc::new(Mutex::new(Vec::new()));
    let mut builder = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(NM)
        .unwrap()
        .serve_at("/device", Device(available.clone()))
        .unwrap()
        .serve_at("/device", Radio)
        .unwrap()
        .serve_at("/ap", AccessPoint(frequency.clone()))
        .unwrap()
        .serve_at(NM_PATH, Manager(activations.clone()))
        .unwrap()
        .serve_at("/active", Active)
        .unwrap();
    for (path, bssid, band, mode) in [
        (
            "/saved/a",
            [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc],
            "a",
            "infrastructure",
        ),
        ("/saved/b", [0; 6], "a", "infrastructure"),
        (
            "/saved/c",
            [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc],
            "bg",
            "infrastructure",
        ),
        ("/saved/d", [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc], "a", "ap"),
        (
            "/saved/z",
            [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc],
            "a",
            "infrastructure",
        ),
    ] {
        builder = builder
            .serve_at(path, RestrictedProfile { bssid, band, mode })
            .unwrap();
    }
    let _server = builder.build().unwrap();
    let client = bus.connect();
    let network = wifi_networks(&client, &["/device".try_into().unwrap()])
        .unwrap()
        .remove(0);
    assert_eq!(network.saved.as_deref(), Some("/saved/z"));
    connect_wifi_on(&client, &network, &Password::default()).unwrap();
    // AP properties can change after the list was drawn: select the now usable profile.
    frequency.store(2412, Ordering::Relaxed);
    connect_wifi_on(&client, &network, &Password::default()).unwrap();
    assert_eq!(*activations.lock().unwrap(), ["/saved/z", "/saved/c"]);
    *available.lock().unwrap() = vec!["/saved/b".try_into().unwrap()];
    let error = connect_wifi_on(&client, &network, &Password::default()).unwrap_err();
    assert!(!error.retry_password);
    assert_eq!(
        activations.lock().unwrap().len(),
        2,
        "a stale profile must not be activated"
    );
}
