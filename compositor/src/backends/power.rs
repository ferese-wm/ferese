//! Power state for automatic display refresh rates. Device batteries are excluded.
use std::path::Path;

pub(super) fn battery_percent() -> Option<u8> {
    read_battery_percent(Path::new("/sys/class/power_supply"))
}

fn read_battery_percent(root: &Path) -> Option<u8> {
    let mut entries = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    let read = |path: &Path, name: &str| {
        std::fs::read_to_string(path.join(name))
            .ok()
            .map(|text| text.trim().to_owned())
    };
    if entries.iter().any(|path| {
        read(path, "online").as_deref() == Some("1")
            && read(path, "type").as_deref() != Some("Battery")
    }) {
        return None;
    }
    entries.iter().find_map(|path| {
        if read(path, "type").as_deref() != Some("Battery")
            || read(path, "scope").as_deref() == Some("Device")
            || read(path, "status").as_deref() != Some("Discharging")
        {
            return None;
        }
        read(path, "capacity")?
            .parse::<u8>()
            .ok()
            .filter(|percent| *percent <= 100)
    })
}

pub(super) fn low_power(previous: bool, battery: Option<u8>) -> bool {
    battery.is_some_and(|percent| if previous { percent < 35 } else { percent < 30 })
}

/// Return a new policy only when an active session needs a mode transition.
/// Inactive samples leave the applied policy intact until activation.
pub(super) fn policy_change(active: bool, applied: bool, battery: Option<u8>) -> Option<bool> {
    if !active {
        return None;
    }
    let next = low_power(applied, battery);
    (next != applied).then_some(next)
}

/// Keep resolution unchanged and use a supported mode within 1 Hz of 60 Hz.
/// Panels without such a mode retain their normal refresh rate.
pub(super) fn low_refresh_mode(size: (u16, u16), modes: &[(u16, u16, i32)]) -> Option<usize> {
    modes
        .iter()
        .enumerate()
        .filter(|(_, (width, height, refresh))| {
            (*width, *height) == size && (59_000..=61_000).contains(refresh)
        })
        .min_by_key(|(_, (_, _, refresh))| refresh.abs_diff(60_000))
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_rescans_only_for_a_changed_policy() {
        assert_eq!(policy_change(true, false, Some(50)), None);
        assert_eq!(policy_change(true, true, Some(32)), None);
        assert_eq!(policy_change(true, false, Some(29)), Some(true));
        assert_eq!(policy_change(true, true, None), Some(false));
    }

    #[test]
    fn paused_samples_defer_changes_until_activation() {
        assert_eq!(policy_change(false, false, Some(20)), None);
        assert_eq!(policy_change(true, false, Some(20)), Some(true));
        assert_eq!(policy_change(false, true, None), None);
        assert_eq!(policy_change(true, true, None), Some(false));
        // A round trip across the threshold while paused needs no rescan.
        assert_eq!(policy_change(false, false, Some(20)), None);
        assert_eq!(policy_change(true, false, Some(50)), None);
    }

    #[test]
    fn low_refresh_selection_rejects_unsupported_rates_and_preserves_resolution() {
        let modes = [
            (2880, 1800, 120_000),
            (1920, 1080, 60_000),
            (2880, 1800, 59_940),
            (2880, 1800, 60_000),
        ];
        assert_eq!(low_refresh_mode((2880, 1800), &modes), Some(3));
        assert_eq!(low_refresh_mode((2880, 1800), &modes[..3]), Some(2));
        assert_eq!(low_refresh_mode((2880, 1800), &modes[..2]), None);
        assert_eq!(
            low_refresh_mode((2880, 1800), &[(2880, 1800, -60_000)]),
            None
        );
    }

    #[test]
    fn thresholds_have_hysteresis_and_ac_or_unknown_restore_normal() {
        assert!(low_power(false, Some(29)));
        assert!(!low_power(false, Some(30)));
        assert!(low_power(true, Some(34)));
        assert!(!low_power(true, Some(35)));
        assert!(!low_power(true, None));
    }

    #[test]
    fn only_discharging_system_batteries_count_and_ac_has_priority() {
        let root = tempfile::tempdir().unwrap();
        let battery = root.path().join("BAT0");
        std::fs::create_dir(&battery).unwrap();
        for (name, value) in [
            ("type", "Battery"),
            ("scope", "System"),
            ("status", "Discharging"),
            ("capacity", "20"),
        ] {
            std::fs::write(battery.join(name), value).unwrap();
        }
        assert_eq!(read_battery_percent(root.path()), Some(20));
        std::fs::write(battery.join("scope"), "Device").unwrap();
        assert_eq!(read_battery_percent(root.path()), None);
        std::fs::write(battery.join("scope"), "System").unwrap();
        std::fs::write(battery.join("capacity"), "bad").unwrap();
        assert_eq!(read_battery_percent(root.path()), None);
        std::fs::write(battery.join("capacity"), "20").unwrap();
        let ac = root.path().join("AC");
        std::fs::create_dir(&ac).unwrap();
        std::fs::write(ac.join("type"), "Mains").unwrap();
        std::fs::write(ac.join("online"), "1").unwrap();
        assert_eq!(read_battery_percent(root.path()), None);
    }
}
