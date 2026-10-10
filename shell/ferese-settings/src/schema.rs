#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Appearance,
    Accessibility,
    Wallpaper,
    Desktop,
    Bar,
    Notifications,
    LockScreen,
    Windows,
    Motion,
    Keyboard,
    Shortcuts,
    Startup,
    Displays,
    Connections,
}

impl Page {
    pub const NAVIGATION: [(&'static str, &'static [Self]); 3] = [
        (
            "Desktop",
            &[Self::Appearance, Self::Wallpaper, Self::Desktop, Self::Bar],
        ),
        ("Interaction", &[Self::Windows, Self::Keyboard, Self::Accessibility]),
        (
            "System",
            &[
                Self::Displays,
                Self::Connections,
                Self::Notifications,
                Self::LockScreen,
                Self::Startup,
            ],
        ),
    ];

    /// Subpages share one entry in navigation and search.
    pub fn navigation_page(self) -> Self {
        match self {
            Self::Motion => Self::Windows,
            Self::Shortcuts => Self::Keyboard,
            _ => self,
        }
    }

    pub fn subpages(self) -> &'static [(Self, &'static str)] {
        match self.navigation_page() {
            Self::Windows => &[(Self::Windows, "Windows"), (Self::Motion, "Motion")],
            Self::Keyboard => &[(Self::Keyboard, "Keyboard & mouse"), (Self::Shortcuts, "Shortcuts")],
            _ => &[],
        }
    }

    pub fn search_destination(self, query: &str) -> Self {
        if !self.matches_own(query) {
            for &(page, _) in self.subpages() {
                if page.matches_own(query) {
                    return page;
                }
            }
        }
        self
    }

    #[cfg(test)]
    pub const ALL: [Self; 14] = [
        Self::Appearance,
        Self::Accessibility,
        Self::Wallpaper,
        Self::Desktop,
        Self::Bar,
        Self::Notifications,
        Self::LockScreen,
        Self::Windows,
        Self::Motion,
        Self::Keyboard,
        Self::Shortcuts,
        Self::Startup,
        Self::Displays,
        Self::Connections,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Accessibility => "Accessibility",
            Self::Wallpaper => "Wallpaper",
            Self::Desktop => "Desktop widgets",
            Self::Bar => "Panel & Shell",
            Self::Notifications => "Notifications",
            Self::LockScreen => "Lock screen",
            Self::Windows => "Windows",
            Self::Motion => "Motion",
            Self::Keyboard => "Keyboard & mouse",
            Self::Shortcuts => "Shortcuts",
            Self::Startup => "Login items",
            Self::Displays => "Displays",
            Self::Connections => "Connections",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Appearance => "A desktop that feels like yours.",
            Self::Accessibility => "Make the desktop easier to see and use.",
            Self::Wallpaper => "Set the scene for your workspace.",
            Self::Desktop => "Clock and sticky notes on your desktop.",
            Self::Bar => "Arrange panel items and groups.",
            Self::Notifications => "Stay informed on your terms.",
            Self::LockScreen => "Your desktop, safely put away.",
            Self::Windows => "Window layouts, spacing and focus effects.",
            Self::Motion => "Animations and transition speed.",
            Self::Keyboard => "Keyboard layouts, pointer behavior and touchpad gestures.",
            Self::Shortcuts => "Keyboard shortcuts and swipe actions.",
            Self::Startup => "Ready when you sign in.",
            Self::Displays => "A place for every screen.",
            Self::Connections => "Wi-Fi networks and Bluetooth devices.",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Accessibility => "M12 3a2 2 0 1 0 0 4a2 2 0 1 0 0-4 M4 9h16 M12 9v6 M12 15l-5 6 M12 15l5 6",
            Self::Appearance => {
                "M12 3a9 9 0 1 0 0 18h1a2 2 0 0 0 0-4h-1a1 1 0 0 1 0-2h3a6 6 0 0 0 0-12z M7 9h.01 M10 6h.01 M15 6h.01 M18 10h.01"
            }
            Self::Wallpaper => "M4 4h16v16H4z M4 16l5-5 4 4 3-3 4 4 M15 8h.01",
            Self::Desktop => "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0 M12 7v5l3 2",
            Self::Bar => "M3 5h18v14H3z M3 9h18 M6 7h.01 M18 7h.01",
            Self::Notifications => "M6 8a6 6 0 0 1 12 0v6l2 3H4l2-3z M10 21h4",
            Self::LockScreen => "M7 10V7a5 5 0 0 1 10 0v3 M5 10h14v11H5z M12 14v3",
            Self::Windows => "M3 4h12v12H3z M8 9h13v12H8z",
            Self::Motion => "M3 8h9a3 3 0 1 0-3-3 M3 12h15a3 3 0 1 1-3 3 M3 16h5",
            Self::Keyboard => {
                "M3 6h18v12H3z M6 9h.01 M10 9h.01 M14 9h.01 M18 9h.01 M6 12h.01 M10 12h.01 M14 12h.01 M18 12h.01 M8 15h8"
            }
            Self::Shortcuts => {
                "M8 8h8v8H8z M8 8H5a3 3 0 1 1 3-3v3z M16 8V5a3 3 0 1 1 3 3h-3z M16 16h3a3 3 0 1 1-3 3v-3z M8 16v3a3 3 0 1 1-3-3h3z"
            }
            Self::Startup => "M12 3v9 M7 5a9 9 0 1 0 10 0",
            Self::Displays => "M3 4h18v13H3z M12 17v4 M8 21h8",
            Self::Connections => "M2 8a16 16 0 0 1 20 0 M5 12a11 11 0 0 1 14 0 M8 16a6 6 0 0 1 8 0 M12 20h.01",
        }
    }

    pub fn matches(self, query: &str) -> bool {
        self.matches_own(query) || self.subpages().iter().any(|(page, _)| page.matches_own(query))
    }

    fn matches_own(self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.title().to_lowercase().contains(&query)
            || self.subtitle().to_lowercase().contains(&query)
            || fields(self).iter().any(|f| {
                format!("{} {} {}", f.label, f.description, f.path)
                    .to_lowercase()
                    .contains(&query)
            })
    }
}

#[derive(Clone, Debug)]
pub enum Kind {
    Font,
    Toggle(bool),
    Range {
        default: f64,
        min: f64,
        max: f64,
        step: f64,
        suffix: &'static str,
        integer: bool,
    },
    Choice {
        default: &'static str,
        choices: &'static [(&'static str, &'static str)],
    },
    Text {
        default: &'static str,
        argv: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Field {
    pub path: String,
    pub label: String,
    pub description: String,
    pub kind: Kind,
}

impl Field {
    pub fn new(path: impl Into<String>, label: impl Into<String>, description: impl Into<String>, kind: Kind) -> Self {
        Self {
            path: path.into(),
            label: label.into(),
            description: description.into(),
            kind,
        }
    }
}

fn toggle(path: &str, label: &str, description: &str, default: bool) -> Field {
    Field::new(path, label, description, Kind::Toggle(default))
}

#[derive(Clone, Copy, Debug)]
pub struct RangeSpec {
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub suffix: &'static str,
    pub integer: bool,
}

pub fn range(
    path: impl Into<String>,
    label: impl Into<String>,
    description: impl Into<String>,
    spec: RangeSpec,
) -> Field {
    let RangeSpec {
        default,
        min,
        max,
        step,
        suffix,
        integer,
    } = spec;
    Field::new(
        path,
        label,
        description,
        Kind::Range {
            default,
            min,
            max,
            step,
            suffix,
            integer,
        },
    )
}

pub fn text(
    path: impl Into<String>,
    label: impl Into<String>,
    description: impl Into<String>,
    default: &'static str,
) -> Field {
    Field::new(path, label, description, Kind::Text { default, argv: false })
}

pub fn choice(
    path: &str,
    label: &str,
    description: &str,
    default: &'static str,
    choices: &'static [(&'static str, &'static str)],
) -> Field {
    Field::new(path, label, description, Kind::Choice { default, choices })
}

fn clock_fields() -> Vec<Field> {
    vec![
        toggle(
            "desktop_widgets.clock.enabled",
            "Desktop clock",
            "Behind windows and click-through.",
            false,
        ),
        choice(
            "desktop_widgets.clock.style",
            "Clock style",
            "Pixel stacks heavy, alternating-color digits; Minimal uses plain text.",
            "pixel",
            &[("pixel", "Pixel"), ("minimal", "Minimal")],
        ),
        toggle(
            "desktop_widgets.clock.bold",
            "Bold text",
            "Use the font's bold weight.",
            false,
        ),
        toggle(
            "desktop_widgets.clock.show_date",
            "Show date",
            "A small date above the digits in Pixel; beneath the time in Minimal.",
            true,
        ),
        toggle(
            "desktop_widgets.clock.lowercase",
            "Lowercase",
            "Lowercase month, weekday and am/pm.",
            true,
        ),
        text(
            "desktop_widgets.clock.anchor",
            "Position",
            "top_left, top_center, top_right, center_left, center, center_right, bottom_left, bottom_center, bottom_right",
            "top_left",
        ),
        Field::new(
            "desktop_widgets.clock.font_family",
            "Font",
            "Choose an installed family or enter a name. Empty uses the style default.",
            Kind::Font,
        ),
        text(
            "desktop_widgets.clock.time_format",
            "Time format",
            "%H:%M for 24-hour; %-I:%M %p for 12-hour; add :%S for seconds.",
            "%-I:%M %p",
        ),
        text(
            "desktop_widgets.clock.date_format",
            "Date format",
            "%A, %-d %B for weekday and date; %-d %b for a short date.",
            "%a, %b %-d",
        ),
        text(
            "desktop_widgets.clock.time_zone",
            "Time zone",
            "Empty follows system time; otherwise an IANA name such as Africa/Lagos.",
            "",
        ),
        text(
            "desktop_widgets.clock.color",
            "Time color",
            "#RRGGBB or #RRGGBBAA; empty follows the theme.",
            "",
        ),
        text(
            "desktop_widgets.clock.date_color",
            "Date color",
            "Empty follows muted theme text.",
            "",
        ),
        text(
            "desktop_widgets.clock.background",
            "Background",
            "Empty is transparent; optionally #RRGGBB or #RRGGBBAA.",
            "",
        ),
        range(
            "desktop_widgets.clock.margin_x",
            "Horizontal margin",
            "Distance from left/right edge; ignored when horizontally centered.",
            RangeSpec {
                default: 64.0,
                min: 0.0,
                max: 1000.0,
                step: 1.0,
                suffix: " px",
                integer: true,
            },
        ),
        range(
            "desktop_widgets.clock.margin_y",
            "Vertical margin",
            "Distance from top/bottom edge; ignored when vertically centered.",
            RangeSpec {
                default: 80.0,
                min: 0.0,
                max: 1000.0,
                step: 1.0,
                suffix: " px",
                integer: true,
            },
        ),
        range(
            "desktop_widgets.clock.width",
            "Widget width",
            "Logical pixels; increase for longer formats.",
            RangeSpec {
                default: 440.0,
                min: 64.0,
                max: 1600.0,
                step: 1.0,
                suffix: " px",
                integer: true,
            },
        ),
        range(
            "desktop_widgets.clock.height",
            "Widget height",
            "Leave room for both text lines.",
            RangeSpec {
                default: 320.0,
                min: 32.0,
                max: 800.0,
                step: 1.0,
                suffix: " px",
                integer: true,
            },
        ),
        range(
            "desktop_widgets.clock.time_size",
            "Time size",
            "Logical pixels; scales with your output.",
            RangeSpec {
                default: 128.0,
                min: 8.0,
                max: 240.0,
                step: 1.0,
                suffix: " px",
                integer: false,
            },
        ),
        range(
            "desktop_widgets.clock.date_size",
            "Date size",
            "Separate size for the date line.",
            RangeSpec {
                default: 18.0,
                min: 8.0,
                max: 96.0,
                step: 1.0,
                suffix: " px",
                integer: false,
            },
        ),
        range(
            "desktop_widgets.clock.opacity",
            "Opacity",
            "Applies to text and the optional background.",
            RangeSpec {
                default: 0.9,
                min: 0.0,
                max: 1.0,
                step: 0.05,
                suffix: "",
                integer: false,
            },
        ),
        range(
            "desktop_widgets.clock.gap",
            "Line spacing",
            "Space between time and date.",
            RangeSpec {
                default: 4.0,
                min: 0.0,
                max: 64.0,
                step: 1.0,
                suffix: " px",
                integer: false,
            },
        ),
        range(
            "desktop_widgets.clock.padding",
            "Inner padding",
            "Space around the labels.",
            RangeSpec {
                default: 12.0,
                min: 0.0,
                max: 64.0,
                step: 1.0,
                suffix: " px",
                integer: false,
            },
        ),
        choice(
            "desktop_widgets.clock.alignment",
            "Text alignment",
            "Alignment within the widget.",
            "center",
            &[("left", "Left"), ("center", "Center"), ("right", "Right")],
        ),
    ]
}

pub fn note_fields(index: usize) -> Vec<Field> {
    let prefix = format!("desktop_widgets.notes.{index}");
    vec![
        Field::new(
            format!("{prefix}.interactive"),
            "Desktop editing",
            "Drag the title and edit directly. Off makes the card click-through.",
            Kind::Toggle(true),
        ),
        Field::new(format!("{prefix}.enabled"), "Show note", "", Kind::Toggle(true)),
        text(format!("{prefix}.title"), "Title", "Empty hides the title.", "Note"),
        text(
            format!("{prefix}.anchor"),
            "Position",
            "Nine positions, e.g. top_right or center.",
            "top_right",
        ),
        Field::new(
            format!("{prefix}.font_family"),
            "Font",
            "Choose an installed family or enter a name. Empty follows the desktop font.",
            Kind::Font,
        ),
        text(
            format!("{prefix}.color"),
            "Text color",
            "Empty or theme follows the theme; otherwise #RRGGBB / #RRGGBBAA.",
            "",
        ),
        text(
            format!("{prefix}.background"),
            "Background",
            "theme follows the palette; empty is transparent; otherwise hex color.",
            "theme",
        ),
        range(
            format!("{prefix}.width"),
            "Width",
            "Logical pixels; centered axes ignore margins.",
            RangeSpec {
                default: 320.0,
                min: 120.0,
                max: 1200.0,
                step: 1.0,
                suffix: " px",
                integer: true,
            },
        ),
        range(
            format!("{prefix}.height"),
            "Height",
            "Logical pixels; centered axes ignore margins.",
            RangeSpec {
                default: 240.0,
                min: 80.0,
                max: 1200.0,
                step: 1.0,
                suffix: " px",
                integer: true,
            },
        ),
        range(
            format!("{prefix}.margin_x"),
            "Horizontal margin",
            "Logical pixels; centered axes ignore margins.",
            RangeSpec {
                default: 48.0,
                min: 0.0,
                max: 1000.0,
                step: 1.0,
                suffix: " px",
                integer: true,
            },
        ),
        range(
            format!("{prefix}.margin_y"),
            "Vertical margin",
            "Logical pixels; centered axes ignore margins.",
            RangeSpec {
                default: 80.0,
                min: 0.0,
                max: 1000.0,
                step: 1.0,
                suffix: " px",
                integer: true,
            },
        ),
        range(
            format!("{prefix}.text_size"),
            "Text size",
            "Logical pixels; centered axes ignore margins.",
            RangeSpec {
                default: 16.0,
                min: 8.0,
                max: 96.0,
                step: 1.0,
                suffix: " px",
                integer: false,
            },
        ),
        range(
            format!("{prefix}.title_size"),
            "Title size",
            "Logical pixels; centered axes ignore margins.",
            RangeSpec {
                default: 18.0,
                min: 8.0,
                max: 96.0,
                step: 1.0,
                suffix: " px",
                integer: false,
            },
        ),
        range(
            format!("{prefix}.opacity"),
            "Opacity",
            "Logical pixels; centered axes ignore margins.",
            RangeSpec {
                default: 0.9,
                min: 0.0,
                max: 1.0,
                step: 0.05,
                suffix: "",
                integer: false,
            },
        ),
        range(
            format!("{prefix}.padding"),
            "Padding",
            "Logical pixels; centered axes ignore margins.",
            RangeSpec {
                default: 20.0,
                min: 0.0,
                max: 64.0,
                step: 1.0,
                suffix: " px",
                integer: false,
            },
        ),
        range(
            format!("{prefix}.gap"),
            "Title spacing",
            "Logical pixels; centered axes ignore margins.",
            RangeSpec {
                default: 8.0,
                min: 0.0,
                max: 64.0,
                step: 1.0,
                suffix: " px",
                integer: false,
            },
        ),
        Field::new(
            format!("{prefix}.alignment"),
            "Text alignment",
            "",
            Kind::Choice {
                default: "left",
                choices: &[("left", "Left"), ("center", "Center"), ("right", "Right")],
            },
        ),
    ]
}

pub fn fields(page: Page) -> Vec<Field> {
    match page {
        Page::LockScreen => vec![
            range(
                "lock_screen.dim_after_seconds",
                "Dim after",
                "Dim the locked display after inactivity. Zero disables dimming.",
                RangeSpec {
                    default: 30.0,
                    min: 0.0,
                    max: 600.0,
                    step: 5.0,
                    suffix: "s",
                    integer: true,
                },
            ),
            range(
                "lock_screen.sleep_after_seconds",
                "Display sleep after",
                "Turn off locked displays after inactivity. Input wakes them; zero disables display sleep.",
                RangeSpec {
                    default: 120.0,
                    min: 0.0,
                    max: 1800.0,
                    step: 15.0,
                    suffix: "s",
                    integer: true,
                },
            ),
            toggle(
                "lock_screen.show_clock",
                "Show clock",
                "Display the time above the sign-in card.",
                true,
            ),
            toggle(
                "lock_screen.show_date",
                "Show date",
                "Display the weekday and date.",
                true,
            ),
            choice(
                "lock_screen.clock_format",
                "Clock format",
                "Use a 12-hour or 24-hour clock.",
                "24h",
                &[("24h", "24-hour"), ("12h", "12-hour")],
            ),
            range(
                "lock_screen.background_blur",
                "Wallpaper softness",
                "Blur the lock screen wallpaper. Zero keeps the original image sharp.",
                RangeSpec {
                    default: 18.0,
                    min: 0.0,
                    max: 40.0,
                    step: 1.0,
                    suffix: "",
                    integer: false,
                },
            ),
            range(
                "lock_screen.background_dim",
                "Wallpaper dimming",
                "Darken the wallpaper behind the clock and sign-in card.",
                RangeSpec {
                    default: 0.48,
                    min: 0.0,
                    max: 0.9,
                    step: 0.02,
                    suffix: "",
                    integer: false,
                },
            ),
        ],
        Page::Accessibility => vec![
            toggle(
                "theme.accessibility.increase_contrast",
                "Increase contrast",
                "Stronger text and borders.",
                false,
            ),
            toggle(
                "theme.accessibility.reduce_transparency",
                "Reduce transparency",
                "Solid surfaces; background blur is disabled.",
                false,
            ),
            toggle(
                "animations.reduced_motion",
                "Reduce motion",
                "Use immediate changes in place of animations.",
                false,
            ),
        ],
        Page::Appearance => vec![
            choice(
                "theme.mode",
                "Appearance",
                "Choose a fixed appearance or let Auto follow a source.",
                "dark",
                &[("light", "Light"), ("dark", "Dark"), ("auto", "Auto")],
            ),
            text(
                "theme.file",
                "Shared theme file",
                "Optional KDL overrides. Relative paths start in your config folder.",
                "",
            ),
            text(
                "theme.light.file",
                "Light theme file",
                "Optional overrides for Light appearance.",
                "",
            ),
            text(
                "theme.dark.file",
                "Dark theme file",
                "Optional overrides for Dark appearance.",
                "",
            ),
            text(
                "theme.schedule.light_at",
                "Light at",
                "Local time in HH:MM format.",
                "07:00",
            ),
            text(
                "theme.schedule.dark_at",
                "Dark at",
                "Local time in HH:MM format.",
                "19:00",
            ),
            text(
                "theme.schedule.timezone",
                "Timezone",
                "system follows your desktop timezone; or enter an IANA timezone.",
                "system",
            ),
            range(
                "theme.geometry.shell_radius",
                "Shell corner radius",
                "Rounds the bar, menus, notifications, desktop widgets and hover backgrounds. Window corners are separate.",
                RangeSpec {
                    default: 14.0,
                    min: 0.0,
                    max: 32.0,
                    step: 1.0,
                    suffix: " px",
                    integer: false,
                },
            ),
            text(
                "theme.accent",
                "Accent color",
                "Focus rings and selected controls. Use a hex color.",
                "#3D7BE6",
            ),
            choice(
                "theme.focus_ring.style",
                "Accent style",
                "Window focus borders and selected controls. Gradients use the theme colors unless overridden.",
                "auto",
                &[("auto", "Gradient"), ("solid", "Solid")],
            ),
            choice(
                "theme.border.style",
                "Inactive border style",
                "Gradients retain the border color's transparency.",
                "auto",
                &[("auto", "Gradient"), ("solid", "Solid")],
            ),
            choice(
                "theme.material.style",
                "Surface style",
                "Choose the finish for the bar and menus.",
                "solid",
                &[("solid", "Solid"), ("translucent", "Translucent")],
            ),
            range(
                "theme.material.opacity",
                "Shell opacity",
                "Background opacity for the bar, popovers, menus, notifications and sharing dialog. Text and icons stay opaque.",
                RangeSpec {
                    default: ferese_config::DEFAULT_MATERIAL_OPACITY,
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    suffix: "",
                    integer: false,
                },
            ),
            range(
                "theme.material.blur_radius",
                "Background blur",
                "Softness behind translucent surfaces.",
                RangeSpec {
                    default: 12.0,
                    min: 0.0,
                    max: 32.0,
                    step: 1.0,
                    suffix: " px",
                    integer: false,
                },
            ),
            text(
                "theme.typography.font_family",
                "Interface font",
                "Use the name of an installed font family.",
                "Inter",
            ),
            range(
                "theme.geometry.focus_ring_width",
                "Focus border",
                "A subtle outline around the active window.",
                RangeSpec {
                    default: 2.0,
                    min: 0.0,
                    max: 4.0,
                    step: 0.5,
                    suffix: " px",
                    integer: false,
                },
            ),
        ],
        Page::Wallpaper => vec![
            text(
                "theme.background.path",
                "Image location",
                "Choose an image above, or enter its full path.",
                ferese_config::default_wallpaper(),
            ),
            choice(
                "theme.background.mode",
                "Image placement",
                "Fill crops to the screen. Fit keeps the whole image.",
                "fill",
                &[("fill", "Fill"), ("fit", "Fit")],
            ),
        ],
        Page::Desktop => clock_fields(),
        Page::Notifications => vec![
            toggle(
                "notifications.show_popups",
                "Show popup notifications",
                "Keep notifications in history when popups are hidden.",
                true,
            ),
            toggle(
                "notifications.do_not_disturb",
                "Do Not Disturb",
                "Silence normal popups; critical notifications can still appear.",
                false,
            ),
            range(
                "notifications.timeout_ms",
                "Popup timeout",
                "Default duration; apps can request another timeout. Pauses while hovered.",
                RangeSpec {
                    default: 6000.0,
                    min: 1000.0,
                    max: 30000.0,
                    step: 1000.0,
                    suffix: " ms",
                    integer: true,
                },
            ),
        ],
        Page::Bar => vec![
            choice(
                "status.bar_layout",
                "Layout",
                "Use one continuous background or separate islands around the controls.",
                "continuous",
                &[("continuous", "Continuous"), ("islands", "Islands")],
            ),
            range(
                "status.bar_island_padding",
                "Island side padding",
                "Space on each side between the island background and its bordered controls.",
                RangeSpec {
                    default: f64::from(ferese_config::default_bar_island_padding()),
                    min: 0.0,
                    max: 32.0,
                    step: 1.0,
                    suffix: " px",
                    integer: false,
                },
            ),
            toggle(
                "status.window_title",
                "Focused window title",
                "Show the focused window title in the center of the bar when space allows.",
                true,
            ),
            range(
                "theme.geometry.top_bar_height",
                "Height",
                "Keeps icon and text sizes unchanged.",
                RangeSpec {
                    default: 28.0,
                    min: 24.0,
                    max: 48.0,
                    step: 1.0,
                    suffix: " px",
                    integer: false,
                },
            ),
            range(
                "theme.geometry.top_bar_margin_top",
                "Edge margin",
                "Space from the selected screen edge. Set to zero for a flush panel.",
                RangeSpec {
                    default: 0.0,
                    min: 0.0,
                    max: 24.0,
                    step: 1.0,
                    suffix: " px",
                    integer: true,
                },
            ),
            range(
                "theme.geometry.top_bar_margin_horizontal",
                "Side margins",
                "Inset from both edges; stops before extra controls overflow.",
                RangeSpec {
                    default: 0.0,
                    min: 0.0,
                    max: 4096.0,
                    step: 1.0,
                    suffix: " px",
                    integer: true,
                },
            ),
            range(
                "theme.geometry.top_bar_window_gap",
                "Window clearance",
                "Space between the panel and application windows.",
                RangeSpec {
                    default: 0.0,
                    min: 0.0,
                    max: 24.0,
                    step: 1.0,
                    suffix: " px",
                    integer: true,
                },
            ),
            toggle(
                "status.battery_percentage",
                "Battery percentage",
                "Show the remaining charge beside the battery icon.",
                true,
            ),
        ],
        Page::Windows => vec![
            toggle(
                "workspaces.auto_back_and_forth",
                "Toggle back with the same workspace shortcut",
                "Press the current workspace's shortcut again to return to the previous workspace on this monitor.",
                false,
            ),
            choice(
                "layout.mode",
                "Layout for new workspaces",
                "Existing workspaces keep their current layout.",
                "scrolling",
                &[("scrolling", "Scrolling"), ("tree", "Tiling")],
            ),
            choice(
                "scrolling.focus_strategy",
                "Scroll behavior",
                "How the viewport follows the focused column.",
                "minimal",
                &[
                    ("minimal", "Minimal"),
                    ("center_on_focus", "Centered"),
                    ("paged", "Paged"),
                ],
            ),
            range(
                "scrolling.default_column_width",
                "New window width",
                "Fraction of the available workspace.",
                RangeSpec {
                    default: 0.5,
                    min: 0.25,
                    max: 1.0,
                    step: 0.05,
                    suffix: "",
                    integer: false,
                },
            ),
            range(
                "layout.inner_gap",
                "Between windows",
                "Space between neighboring windows.",
                RangeSpec {
                    default: 10.0,
                    min: 0.0,
                    max: 32.0,
                    step: 1.0,
                    suffix: " px",
                    integer: false,
                },
            ),
            range(
                "layout.outer_gap",
                "Workspace edges",
                "Space around the outside of the layout.",
                RangeSpec {
                    default: 4.0,
                    min: 0.0,
                    max: 32.0,
                    step: 1.0,
                    suffix: " px",
                    integer: false,
                },
            ),
            range(
                "theme.geometry.window_radius",
                "Corner radius",
                "Rounded corners for application windows.",
                RangeSpec {
                    default: 14.0,
                    min: 0.0,
                    max: 28.0,
                    step: 1.0,
                    suffix: " px",
                    integer: false,
                },
            ),
            toggle(
                "appearance.focus_effect.enabled",
                "Window focus effects",
                "Apply window opacity and inactive dimming. Turning this off keeps your settings.",
                true,
            ),
            range(
                "appearance.focus_effect.active_opacity",
                "Active window opacity",
                "Opacity of the active application, including fullscreen windows.",
                RangeSpec {
                    default: 1.0,
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    suffix: "",
                    integer: false,
                },
            ),
            range(
                "appearance.focus_effect.inactive_opacity",
                "Inactive window opacity",
                "Opacity of other application windows.",
                RangeSpec {
                    default: 1.0,
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    suffix: "",
                    integer: false,
                },
            ),
            range(
                "appearance.focus_effect.inactive_dim",
                "Inactive window dimming",
                "Darken inactive windows. Zero disables dimming.",
                RangeSpec {
                    default: 0.0,
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    suffix: "",
                    integer: false,
                },
            ),
        ],
        Page::Motion => vec![
            toggle(
                "animations.enabled",
                "Animations",
                "Animate window changes and shell transitions.",
                true,
            ),
            range(
                "animations.speed",
                "Animation speed",
                "Lower values give transitions more time.",
                RangeSpec {
                    default: 1.0,
                    min: 0.25,
                    max: 2.0,
                    step: 0.05,
                    suffix: "×",
                    integer: false,
                },
            ),
            range(
                "appearance.focus_effect.duration_ms",
                "Focus transition",
                "Duration of opacity and dimming changes when the active window changes.",
                RangeSpec {
                    default: 150.0,
                    min: 0.0,
                    max: 600.0,
                    step: 10.0,
                    suffix: " ms",
                    integer: false,
                },
            ),
        ],
        Page::Keyboard => vec![
            text(
                "input.xkb_layout",
                "Keyboard layouts",
                "Layout codes, separated by commas. For example: us,ru.",
                "us",
            ),
            text(
                "input.xkb_variant",
                "Layout variant",
                "Leave empty to use the standard variant.",
                "",
            ),
            range(
                "input.repeat_rate",
                "Key repeat",
                "Characters repeated each second while a key is held.",
                RangeSpec {
                    default: 25.0,
                    min: 1.0,
                    max: 70.0,
                    step: 1.0,
                    suffix: " /s",
                    integer: true,
                },
            ),
            range(
                "input.repeat_delay_ms",
                "Delay before repeat",
                "How long to hold a key before it starts repeating.",
                RangeSpec {
                    default: 600.0,
                    min: 150.0,
                    max: 1000.0,
                    step: 25.0,
                    suffix: " ms",
                    integer: true,
                },
            ),
            toggle(
                "input.focus_follows_mouse",
                "Focus follows pointer",
                "Focus visible windows without raising or scrolling. Click to bring them into view.",
                false,
            ),
            toggle(
                "input.touchpad.tap",
                "Tap to click",
                "Tap the touchpad to click. Updates connected devices immediately.",
                true,
            ),
            toggle(
                "input.touchpad.natural_scroll",
                "Natural scrolling",
                "Move content in the direction of your fingers. Updates immediately.",
                true,
            ),
            toggle(
                "input.touchpad.disable_while_typing",
                "Ignore touchpad while typing",
                "Avoid accidental pointer movement.",
                true,
            ),
            range(
                "input.touchpad.swipe_threshold",
                "Swipe distance",
                "Distance needed to navigate. Larger values reduce accidental swipes.",
                RangeSpec {
                    default: 80.0,
                    min: 16.0,
                    max: 1000.0,
                    step: 8.0,
                    suffix: " px",
                    integer: true,
                },
            ),
        ],
        Page::Shortcuts => vec![toggle(
            "status.keybinding_guide",
            "Show keybinding guide at login",
            "Keep the shortcut guide until you are comfortable with Ferese.",
            true,
        )],
        Page::Startup | Page::Displays | Page::Connections => vec![],
    }
}
