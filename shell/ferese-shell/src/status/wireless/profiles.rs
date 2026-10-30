//! NetworkManager profile selection uses protocol capabilities, not UI labels.
use std::collections::HashMap;

use zbus::zvariant::OwnedValue;

use super::{Security, Settings};

const PRIVACY: u32 = 0x1;
const KEY_PSK: u32 = 0x100;
const KEY_EAP: u32 = 0x200;
const KEY_SAE: u32 = 0x400;
const KEY_OWE: u32 = 0x800;
const KEY_OWE_TRANSITION: u32 = 0x1000;
const KEY_SUITE_B: u32 = 0x2000;
const PAIR_WEP40: u32 = 0x1;
const PAIR_WEP104: u32 = 0x2;
const PAIR_TKIP: u32 = 0x4;
const PAIR_CCMP: u32 = 0x8;
const GROUP_WEP40: u32 = 0x10;
const GROUP_WEP104: u32 = 0x20;
const GROUP_TKIP: u32 = 0x40;
const GROUP_CCMP: u32 = 0x80;

type Properties = HashMap<String, OwnedValue>;

#[derive(Clone, Copy)]
pub(super) struct WifiSecurity {
    pub flags: u32,
    pub wpa: u32,
    pub rsn: u32,
}

impl WifiSecurity {
    fn from_ap(ap: &Properties) -> Self {
        Self {
            flags: number(ap, "Flags"),
            wpa: number(ap, "WpaFlags"),
            rsn: number(ap, "RsnFlags"),
        }
    }

    pub fn kind(self) -> Security {
        let keys = self.wpa | self.rsn;
        if keys & (KEY_PSK | KEY_SAE) == KEY_PSK | KEY_SAE {
            Security::Transition
        } else if keys & KEY_SAE != 0 {
            Security::Sae
        } else if keys & KEY_PSK != 0 {
            Security::Personal
        } else if keys & (KEY_EAP | KEY_SUITE_B) != 0 {
            Security::Enterprise
        } else if keys & (KEY_OWE | KEY_OWE_TRANSITION) != 0 {
            Security::Owe
        } else if self.flags & PRIVACY != 0 {
            Security::Unsupported
        } else {
            Security::Open
        }
    }

    fn compatible(self, settings: Option<&Properties>) -> bool {
        let Some(settings) = settings else {
            return self.kind() == Security::Open;
        };
        let key = string(settings, "key-mgmt").unwrap_or_default();
        let protocols = strings(settings, "proto");
        let pmf_disabled = settings
            .get("pmf")
            .and_then(|value| i32::try_from(value).ok())
            == Some(1);
        [("wpa", self.wpa), ("rsn", self.rsn)]
            .into_iter()
            .any(|(protocol, flags)| {
                if !protocols.is_empty() && !protocols.iter().any(|allowed| allowed == protocol) {
                    return false;
                }
                let rsn = protocol == "rsn" && !pmf_disabled;
                let key_matches = match key {
                    // NetworkManager negotiates WPA2 or WPA3 for wpa-psk.
                    "wpa-psk" => flags & KEY_PSK != 0 || (rsn && flags & KEY_SAE != 0),
                    "sae" => rsn && flags & KEY_SAE != 0,
                    "wpa-eap" => flags & KEY_EAP != 0,
                    "wpa-eap-suite-b-192" => rsn && flags & KEY_SUITE_B != 0,
                    "owe" => rsn && flags & (KEY_OWE | KEY_OWE_TRANSITION) != 0,
                    _ => false,
                };
                // Suite-B's mandatory GCMP-256 cipher has no flag in NM's AP API.
                key_matches
                    && (key == "wpa-eap-suite-b-192"
                        || (ciphers_match(&strings(settings, "pairwise"), flags, false)
                            && ciphers_match(&strings(settings, "group"), flags, true)))
            })
    }
}

pub(super) struct SavedProfile {
    pub path: String,
    pub settings: Settings,
}

impl SavedProfile {
    pub fn matches(&self, ssid: &[u8], ap: &Properties) -> bool {
        let Some(wireless) = self.settings.get("802-11-wireless") else {
            return false;
        };
        if bytes(wireless, "ssid").as_deref() != Some(ssid) {
            return false;
        }
        if let Some(bssid) = bytes(wireless, "bssid")
            && !bssid.is_empty()
            && string(ap, "HwAddress").and_then(mac_address).as_deref() != Some(bssid.as_slice())
        {
            return false;
        }
        let mode = match string(wireless, "mode").unwrap_or("infrastructure") {
            "infrastructure" => 2,
            "adhoc" => 1,
            "mesh" => 4,
            _ => return false,
        };
        if number(ap, "Mode") != mode {
            return false;
        }
        let frequency = number(ap, "Frequency");
        if let Some(band) = string(wireless, "band") {
            let in_band = match band {
                "bg" => (2400..=2500).contains(&frequency),
                "a" => (4900..5925).contains(&frequency),
                "6GHz" => (5925..=7125).contains(&frequency),
                "" => true,
                _ => false,
            };
            if !in_band {
                return false;
            }
        }
        let channel = number(wireless, "channel");
        if channel != 0 && frequency_channel(frequency) != Some(channel) {
            return false;
        }
        WifiSecurity::from_ap(ap).compatible(self.settings.get("802-11-wireless-security"))
    }
}

fn number(properties: &Properties, name: &str) -> u32 {
    properties
        .get(name)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default()
}

fn string<'a>(properties: &'a Properties, name: &str) -> Option<&'a str> {
    properties
        .get(name)
        .and_then(|value| <&str>::try_from(value).ok())
}

fn strings(properties: &Properties, name: &str) -> Vec<String> {
    properties
        .get(name)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<String>::try_from(value).ok())
        .unwrap_or_default()
}

fn bytes(properties: &Properties, name: &str) -> Option<Vec<u8>> {
    properties
        .get(name)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<u8>::try_from(value).ok())
}

fn mac_address(value: &str) -> Option<Vec<u8>> {
    let bytes: Vec<_> = value
        .split(':')
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<_, _>>()
        .ok()?;
    (bytes.len() == 6).then_some(bytes)
}

fn frequency_channel(frequency: u32) -> Option<u32> {
    let base = match frequency {
        2484 => return Some(14),
        5935 => return Some(2),
        2412..=2472 => 2407,
        4910..=4980 => 4000,
        5000..5925 => 5000,
        5955..=7115 => 5950,
        _ => return None,
    };
    ((frequency - base).is_multiple_of(5)).then_some((frequency - base) / 5)
}

fn ciphers_match(ciphers: &[String], flags: u32, group: bool) -> bool {
    ciphers.is_empty()
        || ciphers.iter().any(|cipher| {
            let mask = match (cipher.as_str(), group) {
                ("wep40", false) => PAIR_WEP40,
                ("wep104", false) => PAIR_WEP104,
                ("tkip", false) => PAIR_TKIP,
                ("ccmp", false) => PAIR_CCMP,
                ("wep40", true) => GROUP_WEP40,
                ("wep104", true) => GROUP_WEP104,
                ("tkip", true) => GROUP_TKIP,
                ("ccmp", true) => GROUP_CCMP,
                _ => return false,
            };
            flags & mask != 0
        })
}

#[cfg(test)]
mod tests;
