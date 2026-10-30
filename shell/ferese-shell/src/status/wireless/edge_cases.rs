//! Regression coverage for real NetworkManager capability/profile combinations.
use super::super::*;

struct Profiles(Vec<(&'static str, &'static str)>);

#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings")]
impl Profiles {
    fn list_connections(&self) -> Vec<OwnedObjectPath> {
        self.0
            .iter()
            .map(|(path, _)| OwnedObjectPath::try_from(*path).unwrap())
            .collect()
    }
}

struct Profile(&'static str);

#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl Profile {
    fn get_settings(&self) -> Settings {
        HashMap::from([
            (
                "802-11-wireless".into(),
                HashMap::from([(
                    "ssid".into(),
                    Value::from(b"Home".as_slice()).try_to_owned().unwrap(),
                )]),
            ),
            (
                "802-11-wireless-security".into(),
                HashMap::from([(
                    "key-mgmt".into(),
                    Value::from(self.0).try_to_owned().unwrap(),
                )]),
            ),
        ])
    }
}

struct Device(Vec<(&'static str, &'static str)>);
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    fn available_connections(&self) -> Vec<OwnedObjectPath> {
        self.0
            .iter()
            .map(|(path, _)| OwnedObjectPath::try_from(*path).unwrap())
            .collect()
    }
}

struct Radio;

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Radio {
    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/").unwrap()
    }
    fn get_all_access_points(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from("/ap/home").unwrap()]
    }
}

struct AdvertisedAp(u32);

#[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl AdvertisedAp {
    #[zbus(property)]
    fn mode(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        b"Home".to_vec()
    }
    #[zbus(property)]
    fn flags(&self) -> u32 {
        1
    }
    #[zbus(property)]
    fn wpa_flags(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn rsn_flags(&self) -> u32 {
        self.0
    }
    #[zbus(property)]
    fn strength(&self) -> u8 {
        80
    }
}

fn discover(rsn: u32, profiles: Vec<(&'static str, &'static str)>) -> WifiNetwork {
    // No host NetworkManager connection: every production query uses this private bus.
    let bus = crate::status::tests::TestBus::new();
    let mut builder = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(NM)
        .unwrap()
        .serve_at(
            "/org/freedesktop/NetworkManager/Settings",
            Profiles(profiles.clone()),
        )
        .unwrap()
        .serve_at("/device", Device(profiles.clone()))
        .unwrap()
        .serve_at("/device", Radio)
        .unwrap()
        .serve_at("/ap/home", AdvertisedAp(rsn))
        .unwrap();
    for (path, key) in profiles {
        builder = builder.serve_at(path, Profile(key)).unwrap();
    }
    let _server = builder.build().unwrap();
    let networks = wifi_networks(
        &bus.connect(),
        &[OwnedObjectPath::try_from("/device").unwrap()],
    )
    .unwrap();
    assert_eq!(networks.len(), 1);
    networks.into_iter().next().unwrap()
}

#[test]
fn edge_case_wpa2_rejects_sae_only_saved_profile() {
    let network = discover(0x100, vec![("/saved/sae", "sae")]);
    assert_eq!(network.security, Security::Personal);
    assert!(
        network.saved.is_none(),
        "an SAE-only profile cannot authenticate to this WPA2-only AP"
    );
}

#[test]
fn edge_case_wpa2_selects_compatible_profile_when_both_are_saved() {
    let network = discover(
        0x100,
        vec![("/saved/sae", "sae"), ("/saved/psk", "wpa-psk")],
    );
    assert_eq!(
        network.saved.as_deref(),
        Some("/saved/psk"),
        "profile sorting must not select incompatible WPA3-only credentials"
    );
}

#[test]
fn edge_case_suite_b_enterprise_reuses_existing_configuration() {
    // NM_802_11_AP_SEC_KEY_MGMT_EAP_SUITE_B_192 is independent of 0x200.
    let network = discover(0x2000, vec![("/saved/enterprise", "wpa-eap-suite-b-192")]);
    assert_eq!(network.security, Security::Enterprise);
    assert_eq!(
        network.saved.as_deref(),
        Some("/saved/enterprise"),
        "already configured enterprise networks must remain connectable"
    );
}

#[test]
fn edge_case_compatible_personal_and_enterprise_profiles_remain_available() {
    for (rsn, key, expected) in [
        (0x100, "wpa-psk", Security::Personal),
        (0x400, "sae", Security::Sae),
        (0x200, "wpa-eap", Security::Enterprise),
    ] {
        let network = discover(rsn, vec![("/saved/compatible", key)]);
        assert_eq!(network.security, expected);
        assert_eq!(network.saved.as_deref(), Some("/saved/compatible"));
    }
}

#[test]
fn edge_case_sae_reuses_negotiating_personal_credentials() {
    let network = discover(0x400, vec![("/saved/personal", "wpa-psk")]);
    assert_eq!(network.saved.as_deref(), Some("/saved/personal"));
}

#[test]
fn edge_case_ordinary_enterprise_rejects_suite_b_only_credentials() {
    let network = discover(0x200, vec![("/saved/suite_b", "wpa-eap-suite-b-192")]);
    assert!(network.saved.is_none());
}
