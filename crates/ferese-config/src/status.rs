//! Stable status IDs shared by configuration, Settings and the shell.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum StatusItem {
    Media,
    System,
    Network,
    Bluetooth,
    Audio,
    Recording,
    Notifications,
    Battery,
}

impl StatusItem {
    /// Presentation order is independent of configuration insertion order.
    pub const ALL: [Self; 8] = [
        Self::Media,
        Self::System,
        Self::Network,
        Self::Bluetooth,
        Self::Audio,
        Self::Recording,
        Self::Notifications,
        Self::Battery,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::System => "system",
            Self::Network => "network",
            Self::Bluetooth => "bluetooth",
            Self::Audio => "audio",
            Self::Recording => "recording",
            Self::Notifications => "notifications",
            Self::Battery => "battery",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Media => "Now playing",
            Self::System => "Control Center",
            Self::Network => "Wi-Fi",
            Self::Bluetooth => "Bluetooth",
            Self::Audio => "Sound",
            Self::Recording => "Screen recording",
            Self::Notifications => "Notifications",
            Self::Battery => "Battery",
        }
    }
    pub const fn visible_by_default(self) -> bool {
        !matches!(self, Self::Bluetooth)
    }
}

/// Omitted entries retain their defaults; false moves an item into overflow.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct StatusVisibility(BTreeMap<StatusItem, bool>);

impl StatusVisibility {
    pub fn visible(&self, item: StatusItem) -> bool {
        self.0.get(&item).copied().unwrap_or(item.visible_by_default())
    }
    pub fn partition(&self, mut available: impl FnMut(StatusItem) -> bool) -> (Vec<StatusItem>, Vec<StatusItem>) {
        StatusItem::ALL
            .into_iter()
            .filter(|item| available(*item))
            .partition(|item| self.visible(*item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Deserialize)]
    struct Config {
        status: Section,
    }
    #[derive(Deserialize)]
    struct Section {
        icons: StatusVisibility,
    }
    #[test]
    fn parses_partial_overrides_and_roundtrips() {
        let doc = crate::Document::parse("status { icons { battery #false; media #false; }; }").unwrap();
        let c: Config = serde_json::from_value(doc.value().clone()).unwrap();
        assert!(!c.status.icons.visible(StatusItem::Battery));
        assert!(c.status.icons.visible(StatusItem::System));
        let again: StatusVisibility = serde_json::from_value(serde_json::to_value(&c.status.icons).unwrap()).unwrap();
        assert_eq!(again, c.status.icons);
    }
    #[test]
    fn rejects_unknown_ids_and_non_boolean_values() {
        for value in [
            serde_json::json!({"clock": false}),
            serde_json::json!({"audio": "false"}),
        ] {
            assert!(serde_json::from_value::<StatusVisibility>(value).is_err());
        }
    }
    #[test]
    fn every_visibility_combination_preserves_order_and_reachability() {
        for mask in 0..256 {
            let visibility = StatusVisibility(
                StatusItem::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(i, item)| (item, mask & (1 << i) != 0))
                    .collect(),
            );
            for missing in 0..=8 {
                let available = |item| StatusItem::ALL.get(missing) != Some(&item);
                let (visible, overflow) = visibility.partition(available);
                for item in StatusItem::ALL {
                    assert_eq!(visible.contains(&item) || overflow.contains(&item), available(item));
                    assert!(!(visible.contains(&item) && overflow.contains(&item)));
                }
                assert!(visible.windows(2).all(|w| w[0] < w[1]));
                assert!(overflow.windows(2).all(|w| w[0] < w[1]));
            }
        }
    }
}
