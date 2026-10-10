//! Canonical SVG assets, sizing and semantic tint rules for native Ferese UIs.
use cosmic::iced::{Color, ContentFit, Length};
use cosmic::theme;
use cosmic::widget::icon;

pub const ARROW: &[u8] = include_bytes!("../assets/icons/arrow.svg");
pub const BATTERY: &[u8] = include_bytes!("../assets/icons/battery.svg");
pub const CHECK: &[u8] = include_bytes!("../assets/icons/check.svg");
pub const DISPLAY_INTERNAL: &[u8] = include_bytes!("../assets/icons/display-internal.svg");
pub const DISPLAY_EXTERNAL: &[u8] = include_bytes!("../assets/icons/display-external.svg");
pub const DISPLAY_EXTEND: &[u8] = include_bytes!("../assets/icons/display-extend.svg");
pub const DISPLAY_MIRROR: &[u8] = include_bytes!("../assets/icons/display-mirror.svg");
pub const DISPLAY: &[u8] = include_bytes!("../assets/icons/display.svg");
pub const FERESE: &[u8] = include_bytes!("../../../assets/branding/ferese-symbolic.svg");
pub const OVERVIEW: &[u8] = include_bytes!("../assets/icons/overview.svg");
pub const AIRPLANE: &[u8] = include_bytes!("../assets/icons/status/airplane.svg");
pub const BATTERY_25: &[u8] = include_bytes!("../assets/icons/status/battery-25.svg");
pub const BATTERY_50: &[u8] = include_bytes!("../assets/icons/status/battery-50.svg");
pub const BATTERY_75: &[u8] = include_bytes!("../assets/icons/status/battery-75.svg");
pub const BATTERY_CHARGING: &[u8] = include_bytes!("../assets/icons/status/battery-charging.svg");
pub const BATTERY_EMPTY: &[u8] = include_bytes!("../assets/icons/status/battery-empty.svg");
pub const BATTERY_FULL: &[u8] = include_bytes!("../assets/icons/status/battery-full.svg");
pub const BLUETOOTH_CONNECTED: &[u8] = include_bytes!("../assets/icons/status/bluetooth-connected.svg");
pub const BLUETOOTH_OFF: &[u8] = include_bytes!("../assets/icons/status/bluetooth-off.svg");
pub const BLUETOOTH_ON: &[u8] = include_bytes!("../assets/icons/status/bluetooth-on.svg");
pub const BRIGHTNESS: &[u8] = include_bytes!("../assets/icons/status/brightness.svg");
pub const CALENDAR: &[u8] = include_bytes!("../assets/icons/status/calendar.svg");
pub const CHEVRON_DOWN: &[u8] = include_bytes!("../assets/icons/status/chevron-down.svg");
pub const CHEVRON_UP: &[u8] = include_bytes!("../assets/icons/status/chevron-up.svg");
pub const CLOSE: &[u8] = include_bytes!("../assets/icons/status/close.svg");
pub const CONTROL_CENTER: &[u8] = include_bytes!("../assets/icons/status/control-center.svg");
pub const DND: &[u8] = include_bytes!("../assets/icons/status/dnd.svg");
pub const ETHERNET: &[u8] = include_bytes!("../assets/icons/status/ethernet.svg");
pub const LOCK: &[u8] = include_bytes!("../assets/icons/status/lock.svg");
pub const MEDIA: &[u8] = include_bytes!("../assets/icons/status/media.svg");
pub const MEDIA_PLAY: &[u8] = include_bytes!("../assets/icons/status/media-play.svg");
pub const MEDIA_PAUSE: &[u8] = include_bytes!("../assets/icons/status/media-pause.svg");
pub const MEDIA_NEXT: &[u8] = include_bytes!("../assets/icons/status/media-next.svg");
pub const MEDIA_PREVIOUS: &[u8] = include_bytes!("../assets/icons/status/media-previous.svg");
pub const MIC_MUTE: &[u8] = include_bytes!("../assets/icons/status/mic-mute.svg");
pub const MIC: &[u8] = include_bytes!("../assets/icons/status/mic.svg");
pub const MOON: &[u8] = include_bytes!("../assets/icons/status/moon.svg");
pub const NOTIFICATIONS_OFF: &[u8] = include_bytes!("../assets/icons/status/notifications-off.svg");
pub const NOTIFICATIONS: &[u8] = include_bytes!("../assets/icons/status/notifications.svg");
pub const POWER_BALANCED: &[u8] = include_bytes!("../assets/icons/status/power-balanced.svg");
pub const POWER_OFF: &[u8] = include_bytes!("../assets/icons/status/power-off.svg");
pub const POWER_PERFORMANCE: &[u8] = include_bytes!("../assets/icons/status/power-performance.svg");
pub const POWER_SAVER: &[u8] = include_bytes!("../assets/icons/status/power-saver.svg");
pub const POWER: &[u8] = include_bytes!("../assets/icons/status/power.svg");
pub const RECORD_CANCEL: &[u8] = include_bytes!("../assets/icons/status/record-cancel.svg");
pub const RECORD_SAVING: &[u8] = include_bytes!("../assets/icons/status/record-saving.svg");
pub const RECORD_STOP: &[u8] = include_bytes!("../assets/icons/status/record-stop.svg");
pub const RECORD: &[u8] = include_bytes!("../assets/icons/status/record.svg");
pub const RESTART: &[u8] = include_bytes!("../assets/icons/status/restart.svg");
pub const SETTINGS: &[u8] = include_bytes!("../assets/icons/status/settings.svg");
pub const SUSPEND: &[u8] = include_bytes!("../assets/icons/status/suspend.svg");
pub const TRASH: &[u8] = include_bytes!("../assets/icons/status/trash.svg");
pub const VOLUME_HIGH: &[u8] = include_bytes!("../assets/icons/status/volume-high.svg");
pub const VOLUME_LOW: &[u8] = include_bytes!("../assets/icons/status/volume-low.svg");
pub const VOLUME_MEDIUM: &[u8] = include_bytes!("../assets/icons/status/volume-medium.svg");
pub const VOLUME_MUTE: &[u8] = include_bytes!("../assets/icons/status/volume-mute.svg");
pub const WIFI_FULL: &[u8] = include_bytes!("../assets/icons/status/wifi-full.svg");
pub const WIFI_LOW: &[u8] = include_bytes!("../assets/icons/status/wifi-low.svg");
pub const WIFI_MEDIUM: &[u8] = include_bytes!("../assets/icons/status/wifi-medium.svg");
pub const WIFI_OFF: &[u8] = include_bytes!("../assets/icons/status/wifi-off.svg");
pub const VOLUME: &[u8] = include_bytes!("../assets/icons/volume.svg");
pub const WIFI: &[u8] = include_bytes!("../assets/icons/wifi.svg");

pub const APPLICATION: &[u8] = include_bytes!("../../../assets/branding/ferese.svg");

pub fn tinted(source: &'static [u8], size: u16, foreground: Color) -> icon::Icon {
    accented(source, size, foreground, foreground)
}

pub fn symbolic(source: &'static [u8], size: u16) -> icon::Icon {
    icon::from_svg_bytes(source).symbolic(true).icon().size(size)
}

const TINT_CACHE_ENTRIES: usize = 64;
const TINT_CACHE_BYTES: usize = 128 * 1024;

struct TintedIcon {
    source: &'static [u8],
    colors: [u32; 8],
    handle: icon::Handle,
    bytes: usize,
}

#[derive(Default)]
struct TintCache {
    entries: std::collections::VecDeque<TintedIcon>,
    bytes: usize,
}

impl TintCache {
    fn get(&mut self, source: &'static [u8], foreground: Color, accent: Color) -> icon::Handle {
        let colors = [
            foreground.r,
            foreground.g,
            foreground.b,
            foreground.a,
            accent.r,
            accent.g,
            accent.b,
            accent.a,
        ]
        .map(f32::to_bits);
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| std::ptr::eq(entry.source, source) && entry.colors == colors)
        {
            let entry = self.entries.remove(index).unwrap();
            let handle = entry.handle.clone();
            self.entries.push_back(entry);
            return handle;
        }
        let svg = tint_svg(source, foreground, accent);
        let bytes = svg.capacity();
        let handle = icon::from_svg_bytes(svg).symbolic(false);
        if bytes <= TINT_CACHE_BYTES {
            while self.entries.len() >= TINT_CACHE_ENTRIES || self.bytes + bytes > TINT_CACHE_BYTES {
                self.bytes -= self.entries.pop_front().unwrap().bytes;
            }
            self.bytes += bytes;
            self.entries.push_back(TintedIcon {
                source,
                colors,
                handle: handle.clone(),
                bytes,
            });
        }
        handle
    }
}

thread_local! {
    // Retain shared artwork across view rebuilds, with bounded storage as colors change.
    static TINT_CACHE: std::cell::RefCell<TintCache> = std::cell::RefCell::new(TintCache::default());
}

pub fn accented(source: &'static [u8], size: u16, foreground: Color, accent: Color) -> icon::Icon {
    let svg = std::str::from_utf8(source).expect("embedded SVG must be UTF-8");
    // Battery canvases are wider; reserve that space instead of stretching
    // or clipping them into the square slot used by the other status icons.
    let aspect = if svg.contains("viewBox=\"0 0 32 24\"") {
        4.0 / 3.0
    } else {
        1.0
    };
    TINT_CACHE
        .with(|cache| cache.borrow_mut().get(source, foreground, accent))
        .icon()
        .size(size)
        .width(Length::Fixed(f32::from(size) * aspect))
        .content_fit(ContentFit::Contain)
        .class(theme::Svg::custom(move |_| cosmic::iced::widget::svg::Style {
            color: None,
        }))
}

pub fn tint_svg(source: &[u8], foreground: Color, accent: Color) -> Vec<u8> {
    let rgba = |foreground: Color| {
        format!(
            "rgba({},{},{},{})",
            (foreground.r * 255.0).round() as u8,
            (foreground.g * 255.0).round() as u8,
            (foreground.b * 255.0).round() as u8,
            foreground.a,
        )
    };
    std::str::from_utf8(source)
        .expect("embedded icons must be UTF-8 SVG")
        .replace("currentColor", &rgba(foreground))
        // The source artwork's blue swatch is the semantic theme accent.
        // Battery warning/success swatches remain status colors.
        .replace("#3d7be6", &rgba(accent))
        .into_bytes()
}

pub fn outline(path: &str, tint: Color, size: u16) -> icon::Icon {
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
        <path d="{path}" fill="none" stroke="currentColor" stroke-width="1.6"
            stroke-linecap="round" stroke-linejoin="round"/>
        </svg>"##,
    );
    icon::from_svg_bytes(tint_svg(svg.as_bytes(), tint, tint))
        .symbolic(false)
        .icon()
        .size(size)
}

#[cfg(test)]
mod tests {
    use cosmic::iced::Color;
    #[test]
    fn tinted_icons_share_artwork_and_distinguish_accent_and_alpha() {
        let mut cache = super::TintCache::default();
        let foreground = Color::from_rgb8(205, 214, 244);
        let accent = Color::from_rgb8(203, 166, 247);
        let bytes = |handle: &cosmic::widget::icon::Handle| {
            let cosmic::widget::icon::Data::Svg(svg) = &handle.data else {
                panic!("expected SVG");
            };
            let cosmic::iced::advanced::svg::Data::Bytes(bytes) = svg.data() else {
                panic!("expected SVG bytes");
            };
            bytes.as_ptr()
        };
        let first = cache.get(super::BATTERY_50, foreground, accent);
        let second = cache.get(super::BATTERY_50, foreground, accent);
        assert_eq!(bytes(&first), bytes(&second), "rebuilds must reuse the same allocation");
        let changed_accent = cache.get(super::BATTERY_50, foreground, Color::from_rgb8(42, 90, 180));
        assert_ne!(bytes(&first), bytes(&changed_accent));
        let translucent = cache.get(super::BATTERY_50, Color { a: 0.5, ..foreground }, accent);
        assert_ne!(bytes(&first), bytes(&translucent));
        assert_eq!(cache.entries.len(), 3);
    }

    #[test]
    fn icon_tint_cache_evicts_old_colors_without_growing() {
        let mut cache = super::TintCache::default();
        for value in 0..512 {
            let _ = cache.get(
                super::FERESE,
                Color::from_rgba(0.5, 0.5, 0.5, value as f32 / 512.),
                Color::BLACK,
            );
            assert!(cache.entries.len() <= super::TINT_CACHE_ENTRIES);
            assert!(cache.bytes <= super::TINT_CACHE_BYTES);
        }
        assert_eq!(cache.entries.back().unwrap().colors[3], (511_f32 / 512.).to_bits());
    }

    #[test]
    fn icons_use_theme_accent_and_preserve_battery_status_colors() {
        for (source, accent) in [
            (super::BATTERY_FULL, "#63c168"),
            (super::BATTERY_50, "#3d7be6"),
            (super::BATTERY_25, "#e0654f"),
            (super::WIFI_FULL, "#3d7be6"),
            (super::CONTROL_CENTER, "#3d7be6"),
        ] {
            let svg = String::from_utf8(super::tint_svg(
                source,
                Color::from_rgb8(205, 214, 244),
                Color::from_rgb8(203, 166, 247),
            ))
            .unwrap();
            assert!(!svg.contains("currentColor"));
            assert!(svg.contains("rgba(205,214,244,1)"));
            assert!(!svg.contains("#3d7be6"));
            assert!(svg.contains(if accent == "#3d7be6" {
                "rgba(203,166,247,1)"
            } else {
                accent
            }));
        }
    }
}
