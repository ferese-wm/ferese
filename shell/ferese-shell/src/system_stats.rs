//! Text-bar measurements, sampled off the UI thread only while the bar is visible.
use std::fs;
use std::time::{Duration, Instant};

use cosmic::iced::{Subscription, futures};

const INTERVAL: Duration = Duration::from_secs(2);
const DISK_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub cpu: Option<u8>,
    pub disk: Option<u8>,
    pub network: Option<NetworkRate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NetworkRate {
    pub interface: String,
    pub received: u64,
    pub transmitted: u64,
}

impl NetworkRate {
    pub fn label(&self) -> String {
        format!("rx {} tx {}", rate(self.received), rate(self.transmitted))
    }
}

fn rate(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1}GiB/s", bytes as f64 / (1024. * 1024. * 1024.))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1}MiB/s", bytes as f64 / (1024. * 1024.))
    } else if bytes >= 1024 {
        format!("{:.0}KiB/s", bytes as f64 / 1024.)
    } else {
        format!("{bytes}B/s")
    }
}

pub(crate) fn subscription(visible: bool) -> Subscription<Snapshot> {
    if visible {
        Subscription::run(stream)
    } else {
        Subscription::none()
    }
}

fn stream() -> impl futures::Stream<Item = Snapshot> {
    futures::stream::unfold(
        (Sampler::default(), None),
        |(mut sampler, mut previous)| async move {
            loop {
                if sampler.sampled {
                    tokio::time::sleep(INTERVAL).await;
                }
                // Cancellation drops the stream. At most one outstanding sample can
                // finish; no worker or timer keeps polling after the bar disappears.
                let (next, snapshot) = tokio::task::spawn_blocking(move || {
                    let snapshot = sampler.sample(Instant::now());
                    (sampler, snapshot)
                })
                .await
                .ok()?;
                sampler = next;
                if previous.as_ref() != Some(&snapshot) {
                    previous = Some(snapshot.clone());
                    return Some((snapshot, (sampler, previous)));
                }
            }
        },
    )
}

#[derive(Default)]
struct Sampler {
    sampled: bool,
    cpu: Option<Cpu>,
    network: Option<NetworkSample>,
    disk: Option<(Instant, Option<u8>)>,
}

impl Sampler {
    fn sample(&mut self, now: Instant) -> Snapshot {
        self.sampled = true;
        let cpu = fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|text| Cpu::parse(&text));
        let cpu_percent = cpu.zip(self.cpu).and_then(|(new, old)| new.percent(old));
        self.cpu = cpu;
        let network = read_network(now);
        let network_rate = network
            .as_ref()
            .zip(self.network.as_ref())
            .and_then(|(new, old)| new.rate(old));
        self.network = network;
        if self
            .disk
            .is_none_or(|(at, _)| now.saturating_duration_since(at) >= DISK_INTERVAL)
        {
            self.disk = Some((now, disk_usage()));
        }
        Snapshot {
            cpu: cpu_percent,
            disk: self.disk.and_then(|(_, usage)| usage),
            network: network_rate,
        }
    }
}

#[derive(Clone, Copy)]
struct Cpu {
    total: u64,
    idle: u64,
}

impl Cpu {
    fn parse(text: &str) -> Option<Self> {
        let mut fields = text.lines().next()?.split_whitespace();
        if fields.next()? != "cpu" {
            return None;
        }
        // guest and guest_nice already belong to user and nice; do not add them twice.
        let values: Vec<u64> = fields
            .take(8)
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        if values.len() < 4 {
            return None;
        }
        Some(Self {
            total: values
                .iter()
                .try_fold(0u64, |total, value| total.checked_add(*value))?,
            idle: values[3].checked_add(values.get(4).copied().unwrap_or(0))?,
        })
    }

    fn percent(self, previous: Self) -> Option<u8> {
        let total = self.total.checked_sub(previous.total)?;
        let idle = self.idle.checked_sub(previous.idle)?;
        let busy = total.checked_sub(idle)?;
        (total > 0)
            .then(|| ((u128::from(busy) * 100 + u128::from(total) / 2) / u128::from(total)) as u8)
    }
}

struct NetworkSample {
    interface: String,
    index: u32,
    received: u64,
    transmitted: u64,
    at: Instant,
}

impl NetworkSample {
    fn rate(&self, previous: &Self) -> Option<NetworkRate> {
        if self.interface != previous.interface || self.index != previous.index {
            return None;
        }
        let seconds = self.at.checked_duration_since(previous.at)?.as_secs_f64();
        if seconds <= 0. {
            return None;
        }
        Some(NetworkRate {
            interface: self.interface.clone(),
            received: (self.received.checked_sub(previous.received)? as f64 / seconds).round()
                as u64,
            transmitted: (self.transmitted.checked_sub(previous.transmitted)? as f64 / seconds)
                .round() as u64,
        })
    }
}

fn read_network(now: Instant) -> Option<NetworkSample> {
    let ipv4 = fs::read_to_string("/proc/net/route").unwrap_or_default();
    let ipv6 = fs::read_to_string("/proc/net/ipv6_route").unwrap_or_default();
    let interface = default_interface(&ipv4, &ipv6)?;
    let root = std::path::Path::new("/sys/class/net").join(&interface);
    Some(NetworkSample {
        index: fs::read_to_string(root.join("ifindex"))
            .ok()?
            .trim()
            .parse()
            .ok()?,
        received: fs::read_to_string(root.join("statistics/rx_bytes"))
            .ok()?
            .trim()
            .parse()
            .ok()?,
        transmitted: fs::read_to_string(root.join("statistics/tx_bytes"))
            .ok()?
            .trim()
            .parse()
            .ok()?,
        interface,
        at: now,
    })
}

fn default_interface(ipv4: &str, ipv6: &str) -> Option<String> {
    let v4 = ipv4
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 8 || fields[1] != "00000000" || fields[7] != "00000000" {
                return None;
            }
            let flags = u32::from_str_radix(fields[3], 16).ok()?;
            (flags & 1 != 0 && flags & 0x200 == 0)
                .then_some((fields[6].parse::<u32>().ok()?, fields[0]))
        })
        .min();
    let selected = v4.or_else(|| {
        ipv6.lines()
            .filter_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.len() != 10
                    || fields[0] != "00000000000000000000000000000000"
                    || fields[1] != "00"
                    || fields[3] != "00"
                {
                    return None;
                }
                let flags = u32::from_str_radix(fields[8], 16).ok()?;
                (flags & 1 != 0 && flags & 0x200 == 0)
                    .then_some((u32::from_str_radix(fields[5], 16).ok()?, fields[9]))
            })
            .min()
    });
    let interface = selected?.1;
    // A kernel interface name is a single path component, never an arbitrary path.
    (!interface.is_empty() && interface != "." && interface != ".." && !interface.contains('/'))
        .then(|| interface.to_owned())
}

fn disk_usage() -> Option<u8> {
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the path is NUL terminated and stats points to writable storage.
    if unsafe { libc::statvfs(c"/".as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: a successful statvfs initialized the structure.
    let stats = unsafe { stats.assume_init() };
    disk_percent(stats.f_blocks, stats.f_bfree, stats.f_bavail)
}

fn disk_percent(total: u64, free: u64, available: u64) -> Option<u8> {
    let used = total.checked_sub(free)?;
    if available > free {
        return None;
    }
    // Match df's user-visible capacity: reserved blocks are not usable free space.
    let capacity = u128::from(used) + u128::from(available);
    (capacity > 0).then(|| (u128::from(used) * 100).div_ceil(capacity) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_excludes_guest_and_rejects_resets_or_missing_samples() {
        let old = Cpu::parse("cpu 100 0 20 800 30 0 0 50 90 0").unwrap();
        let new = Cpu::parse("cpu 150 0 30 830 40 0 0 50 140 0").unwrap();
        assert_eq!(new.percent(old), Some(60));
        assert_eq!(old.percent(new), None);
        assert_eq!(old.percent(old), None);
        assert!(Cpu::parse("cpu0 1 2 3 4").is_none());
        assert!(Cpu::parse("cpu 1 2").is_none());
        assert!(Cpu::parse("cpu 1 bad 3 4").is_none());
        assert_eq!(
            Cpu {
                total: 1010,
                idle: 820
            }
            .percent(old),
            None
        );
    }

    #[test]
    fn network_uses_elapsed_time_and_resets_on_device_changes() {
        let now = Instant::now();
        let old = NetworkSample {
            interface: "wlan0".into(),
            index: 1,
            received: 10,
            transmitted: 20,
            at: now,
        };
        let mut new = NetworkSample {
            interface: "wlan0".into(),
            index: 1,
            received: 4010,
            transmitted: 2020,
            at: now + Duration::from_secs(4),
        };
        assert_eq!(new.rate(&old).unwrap().received, 1000);
        assert_eq!(new.rate(&old).unwrap().transmitted, 500);
        new.index = 2;
        assert!(new.rate(&old).is_none());
        new.index = 1;
        new.received = 0;
        assert!(new.rate(&old).is_none());
        new.received = 4010;
        new.at = now;
        assert!(new.rate(&old).is_none());
    }

    #[test]
    fn routes_ignore_down_rejected_and_non_default_entries() {
        let v4 = "eth0 00000000 01010101 0003 0 0 100 00000000\nwlan0 00000000 01010101 0003 0 0 50 00000000\nvpn0 00000000 00000000 0201 0 0 1 00000000\ndown0 00000000 00000000 0000 0 0 0 00000000\n";
        assert_eq!(default_interface(v4, ""), Some("wlan0".into()));
        let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 00000064 00000000 00000000 00000001 eth1";
        assert_eq!(default_interface("", v6), Some("eth1".into()));
        assert_eq!(default_interface("bad", "bad"), None);
    }

    #[test]
    fn disk_handles_reserved_blocks_full_and_invalid_filesystems() {
        assert_eq!(disk_percent(100, 20, 10), Some(89));
        assert_eq!(disk_percent(100, 0, 0), Some(100));
        assert_eq!(disk_percent(100, 100, 95), Some(0));
        assert_eq!(disk_percent(0, 0, 0), None);
        assert_eq!(disk_percent(10, 20, 0), None);
        assert_eq!(disk_percent(10, 5, 6), None);
        assert_eq!(disk_percent(u64::MAX, 0, 0), Some(100));
        assert_eq!(subscription(false).units(), 0);
        assert_eq!(subscription(true).units(), 1);
    }
}
