//! Human-readable EDID identities. Serial-less displays remain tied to a qualified connector.
//! Duplicate serials are qualified too; a remembered collision stays qualified after unplug.
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub make: String,
    pub model: String,
    pub serial: Option<String>,
}

fn escaped(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

impl Identity {
    pub fn from_edid(edid: &[u8]) -> Option<Self> {
        if edid.len() < 128
            || edid[..8] != [0, 255, 255, 255, 255, 255, 255, 0]
            || edid[..128]
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
                != 0
        {
            return None;
        }
        let manufacturer = u16::from_be_bytes([edid[8], edid[9]]);
        let mut make = String::new();
        for shift in [10, 5, 0] {
            let letter = ((manufacturer >> shift) & 31) as u8;
            if !(1..=26).contains(&letter) {
                return None;
            }
            make.push(char::from(b'A' + letter - 1));
        }
        let product = u16::from_le_bytes([edid[10], edid[11]]);
        let numeric_serial = u32::from_le_bytes(edid[12..16].try_into().ok()?);
        let mut model = format!("{product:04X}");
        let mut serial = None;
        for descriptor in edid[54..126].chunks_exact(18) {
            if descriptor[..3] != [0, 0, 0] {
                continue;
            }
            let text = String::from_utf8_lossy(&descriptor[5..18])
                .trim_matches(|c: char| c.is_whitespace() || c == '\0')
                .to_owned();
            if text.is_empty() {
                continue;
            }
            match descriptor[3] {
                0xfc => model = format!("{product:04X}-{}", escaped(&text)),
                0xff if text != "0" => serial = Some(escaped(&text)),
                _ => {}
            }
        }
        if serial.is_none() && numeric_serial != 0 && numeric_serial != u32::MAX {
            serial = Some(format!("{numeric_serial:08X}"));
        }
        Some(Self {
            make,
            model,
            serial,
        })
    }

    pub fn base(&self) -> String {
        format!(
            "edid:{}:{}:{}",
            self.make,
            self.model,
            self.serial.as_deref().unwrap_or("no-serial")
        )
    }
}

#[derive(Default)]
pub(crate) struct IdentityRegistry {
    ambiguous: HashSet<String>,
    /// Retains the first monitor's identity if a later collision is discovered.
    known: HashMap<String, String>,
}

impl IdentityRegistry {
    pub fn resolve(&mut self, monitors: &[(String, Option<Identity>)]) -> Vec<String> {
        let mut counts = HashMap::new();
        for (_, identity) in monitors {
            if let Some(identity) = identity {
                *counts.entry(identity.base()).or_insert(0) += 1;
            }
        }
        self.ambiguous.extend(
            counts
                .into_iter()
                .filter(|(_, count)| *count > 1)
                .map(|(base, _)| base),
        );
        // Prefer already-known identities, but never permit duplicate runtime IDs.
        let mut used = HashSet::new();
        let mut ordered = monitors.iter().enumerate().collect::<Vec<_>>();
        ordered.sort_by_key(|(_, (key, _))| (!self.known.contains_key(key), key));
        let mut result = vec![String::new(); monitors.len()];
        for (index, (key, identity)) in ordered {
            let base = identity.as_ref().map(Identity::base);
            let qualified = match &base {
                Some(base) => format!("{base}@{}", escaped(key)),
                None => format!("drm:{}", escaped(key)),
            };
            let preferred = self
                .known
                .get(key)
                .filter(|known| {
                    base.as_ref()
                        .is_some_and(|base| *known == base || *known == &qualified)
                        || (base.is_none() && *known == &qualified)
                })
                .cloned()
                .unwrap_or_else(|| {
                    if identity
                        .as_ref()
                        .is_some_and(|identity| identity.serial.is_some())
                        && !self.ambiguous.contains(base.as_ref().unwrap())
                    {
                        base.clone().unwrap()
                    } else {
                        qualified.clone()
                    }
                });
            let resolved = if used.insert(preferred.clone()) {
                preferred
            } else {
                qualified
            };
            assert!(
                result.iter().all(|other| other != &resolved),
                "qualified hardware connector must be unique"
            );
            self.known.insert(key.clone(), resolved.clone());
            result[index] = resolved;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(serial: Option<&str>) -> Identity {
        Identity {
            make: "DEL".into(),
            model: "1234-monitor".into(),
            serial: serial.map(String::from),
        }
    }

    #[test]
    fn replacement_on_same_connector_invalidates_cached_identity() {
        let mut registry = IdentityRegistry::default();
        let key = "gpu/DP-1".to_owned();
        let dell = identity(Some("123"));
        let mut lg = identity(Some("456"));
        lg.make = "GSM".into();
        let old = registry.resolve(&[(key.clone(), Some(dell.clone()))]);
        let new = registry.resolve(&[(key.clone(), Some(lg))]);
        assert_ne!(old, new);
        assert!(new[0].starts_with("edid:GSM:"));
        assert_eq!(registry.resolve(&[(key.clone(), Some(dell))]), old);
        assert!(registry.resolve(&[(key, None)])[0].starts_with("drm:"));
    }

    #[test]
    fn serial_survives_connector_change() {
        let mut registry = IdentityRegistry::default();
        let first = registry.resolve(&[("gpu/DP-1".into(), Some(identity(Some("123"))))]);
        let next = registry.resolve(&[("gpu/DP-2".into(), Some(identity(Some("123"))))]);
        assert_eq!(first, next);
        assert_eq!(first[0], "edid:DEL:1234-monitor:123");
    }

    #[test]
    fn identical_without_serial_are_distinct_and_reconnect_stably() {
        for serial in [None, Some("duplicated")] {
            let mut registry = IdentityRegistry::default();
            let inputs = [
                ("gpu/DP-1".into(), Some(identity(serial))),
                ("gpu/DP-2".into(), Some(identity(serial))),
            ];
            let resolved = registry.resolve(&inputs);
            assert_ne!(resolved[0], resolved[1]);
            assert_eq!(registry.resolve(&inputs[..1])[0], resolved[0]);
            assert_eq!(registry.resolve(&inputs), resolved);
        }
    }

    #[test]
    fn collision_after_first_connection_does_not_change_existing_identity() {
        let mut registry = IdentityRegistry::default();
        let a = ("gpu/DP-1".into(), Some(identity(Some("duplicate"))));
        let b = ("gpu/DP-2".into(), Some(identity(Some("duplicate"))));
        let first = registry.resolve(std::slice::from_ref(&a));
        let both = registry.resolve(&[a.clone(), b]);
        assert_eq!(both[0], first[0]);
        assert_ne!(both[0], both[1]);
        assert_eq!(registry.resolve(&[a]), first);
    }

    #[test]
    fn edid_decodes_make_product_and_numeric_serial_and_checks_checksum() {
        let mut edid = [0u8; 128];
        edid[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
        let del = (4u16 << 10) | (5 << 5) | 12;
        edid[8..10].copy_from_slice(&del.to_be_bytes());
        edid[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        edid[12..16].copy_from_slice(&42u32.to_le_bytes());
        edid[127] = 0u8.wrapping_sub(edid.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
        assert_eq!(
            Identity::from_edid(&edid).unwrap().base(),
            "edid:DEL:1234:0000002A"
        );
        edid[127] ^= 1;
        assert!(Identity::from_edid(&edid).is_none());
    }
}
