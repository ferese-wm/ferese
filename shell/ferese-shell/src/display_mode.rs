use super::*;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Mode {
    Internal,
    External,
    #[default]
    Extend,
    Mirror,
}

impl Mode {
    pub const ALL: [Self; 4] = [Self::Internal, Self::External, Self::Extend, Self::Mirror];

    pub fn name(self) -> &'static str {
        match self {
            Self::Internal => "internal-only",
            Self::External => "external-only",
            Self::Extend => "extend",
            Self::Mirror => "mirror",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Internal => "Internal only",
            Self::External => "External only",
            Self::Extend => "Extend",
            Self::Mirror => "Mirror",
        }
    }

    fn icon(self) -> &'static [u8] {
        match self {
            Self::Internal => ferese_theme::icons::DISPLAY_INTERNAL,
            Self::External => ferese_theme::icons::DISPLAY_EXTERNAL,
            Self::Extend => ferese_theme::icons::DISPLAY_EXTEND,
            Self::Mirror => ferese_theme::icons::DISPLAY_MIRROR,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Display {
    label: String,
    internal: bool,
    physical: bool,
    enabled: bool,
    usable: bool,
    mirrored: bool,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Inventory {
    displays: Vec<Display>,
    profile: Option<String>,
    pub pending: bool,
    timeout: u64,
}

impl Inventory {
    pub fn external_connected(&self) -> bool {
        self.displays
            .iter()
            .any(|display| display.physical && !display.internal)
    }

    fn available(&self, mode: Mode) -> bool {
        let usable = self.displays.iter().filter(|display| display.usable);
        match mode {
            Mode::Internal => usable.clone().any(|display| display.internal),
            Mode::External => usable.clone().any(|display| !display.internal),
            Mode::Extend | Mode::Mirror => usable.count() >= 2,
        }
    }

    fn current(&self) -> Mode {
        let enabled = self.displays.iter().filter(|display| display.enabled);
        if enabled.clone().any(|display| display.mirrored) {
            Mode::Mirror
        } else if enabled.clone().any(|display| display.internal) {
            if enabled.clone().any(|display| !display.internal) {
                Mode::Extend
            } else {
                Mode::Internal
            }
        } else {
            Mode::External
        }
    }

    fn parse(outputs: Value, profiles: Value) -> Result<Self, String> {
        let outputs = outputs.as_array().ok_or("Missing monitor inventory")?;
        let mut profile = None;
        let mut displays = Vec::new();
        for output in outputs
            .iter()
            .filter(|output| output["connected"].as_bool().unwrap_or(true))
        {
            if let Some(name) = output["profile"].as_str() {
                profile = Some(name.to_owned());
            }

            let internal = output["internal"].as_bool().unwrap_or(false);
            let name = if internal {
                "Built-in display"
            } else {
                output["connector"]
                    .as_str()
                    .or_else(|| output["name"].as_str())
                    .unwrap_or("External display")
            };
            let mode = if output["current_mode"].is_object() {
                &output["current_mode"]
            } else {
                output["available_modes"]
                    .as_array()
                    .and_then(|modes| {
                        modes
                            .iter()
                            .find(|mode| mode["preferred"] == true)
                            .or_else(|| modes.first())
                    })
                    .unwrap_or(&Value::Null)
            };
            let label = match (mode["width"].as_u64(), mode["height"].as_u64()) {
                (Some(width), Some(height)) => format!("{name}, {width}×{height}"),
                _ => name.to_owned(),
            };
            displays.push(Display {
                label,
                internal,
                physical: output["internal"].is_boolean(),
                enabled: output["applied_enabled"]
                    .as_bool()
                    .or_else(|| output["enabled"].as_bool())
                    .unwrap_or(false),
                usable: output["available_modes"]
                    .as_array()
                    .is_some_and(|modes| !modes.is_empty()),
                mirrored: output["mirror_source"].is_string(),
            });
        }

        let timeout = profiles["profiles"]
            .as_array()
            .and_then(|profiles| {
                profiles
                    .iter()
                    .find(|entry| entry["name"].as_str() == profile.as_deref())
            })
            .and_then(|entry| entry["confirm_timeout"].as_u64())
            .unwrap_or(15);
        Ok(Self {
            displays,
            profile,
            pending: profiles["confirmation_pending"].as_bool().unwrap_or(false),
            timeout,
        })
    }
}

fn load() -> Result<Inventory, String> {
    Inventory::parse(
        compositor_ipc::call("outputs")?,
        compositor_ipc::call("output-profiles")?,
    )
}

#[derive(Default)]
pub(super) struct Model {
    pub open: bool,
    pub inventory: Option<Inventory>,
    selected: Mode,
    pub busy: bool,
    error: Option<String>,
    generation: u64,
    operation: u64,
    confirmation_since: Option<Instant>,
}

impl Model {
    pub fn begin(&mut self) {
        self.open = true;
        self.error = None;
        if let Some(inventory) = &self.inventory {
            self.selected = inventory.current();
        }
    }

    pub fn external_connected(&self) -> bool {
        self.inventory.as_ref().is_some_and(Inventory::external_connected)
    }

    pub fn pending(&self) -> bool {
        self.inventory.as_ref().is_some_and(|inventory| inventory.pending)
    }

    fn update(&mut self, inventory: Inventory) {
        if inventory.pending && !self.pending() {
            self.confirmation_since = Some(Instant::now());
        }

        if !inventory.pending {
            self.confirmation_since = None;
        }

        self.selected = inventory.current();
        self.inventory = Some(inventory);
    }
}

impl FereseShell {
    pub(super) fn load_display_inventory(&mut self) -> Task<Message> {
        if self.display_mode.busy {
            return Task::none();
        }

        self.display_mode.generation = self.display_mode.generation.wrapping_add(1);
        let generation = self.display_mode.generation;
        Task::perform(
            async {
                tokio::task::spawn_blocking(load)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result)
            },
            move |result| cosmic::Action::App(Message::DisplaysLoaded(generation, result)),
        )
    }

    pub(super) fn finish_display_inventory(
        &mut self,
        generation: u64,
        result: Result<Inventory, String>,
    ) -> Task<Message> {
        if generation != self.display_mode.generation || self.display_mode.busy {
            return Task::none();
        }

        match result {
            Ok(inventory) => {
                self.display_mode.update(inventory);
            }
            Err(error) => self.display_mode.error = Some(error),
        }

        Task::none()
    }

    pub(super) fn select_display_mode(&mut self, mode: Mode, preserve_focus: bool) -> Task<Message> {
        if !self.display_mode.busy
            && !self.display_mode.pending()
            && self
                .display_mode
                .inventory
                .as_ref()
                .is_some_and(|inventory| inventory.available(mode))
        {
            self.display_mode.selected = mode;
            self.display_mode.error = None;
            // Native buttons clear focus on mouse clicks and retain it for
            // keyboard activation. Do not add focus back after a click.
            if preserve_focus {
                return Task::none();
            }

            // Arrow/number selection leaves Enter for the default action,
            // rather than activating a previously clicked or tabbed mode.
            return cosmic::iced::advanced::widget::operate(cosmic::iced::advanced::widget::operation::scoped(
                cosmic::iced::advanced::widget::Id::new("ferese-system-modal-controls"),
                cosmic::iced::advanced::widget::operation::focusable::unfocus(),
            ));
        }

        Task::none()
    }

    pub(super) fn display_mode_key(&mut self, key: &cosmic::iced::keyboard::Key) -> Task<Message> {
        use cosmic::iced::keyboard::{Key, key::Named};
        let current = Mode::ALL
            .iter()
            .position(|mode| *mode == self.display_mode.selected)
            .unwrap();
        let mode = match key {
            Key::Character(character) => character
                .parse::<usize>()
                .ok()
                .filter(|number| (1..=4).contains(number))
                .map(|number| Mode::ALL[number - 1]),
            Key::Named(Named::ArrowLeft | Named::ArrowUp | Named::ArrowRight | Named::ArrowDown) => {
                let backwards = matches!(key, Key::Named(Named::ArrowLeft | Named::ArrowUp));
                (1..=4)
                    .map(|step| Mode::ALL[(current + if backwards { 4 - step } else { step }) % 4])
                    .find(|mode| {
                        self.display_mode
                            .inventory
                            .as_ref()
                            .is_some_and(|inventory| inventory.available(*mode))
                    })
            }
            _ => None,
        };
        mode.map_or_else(Task::none, |mode| self.select_display_mode(mode, false))
    }

    pub(super) fn apply_display_mode(&mut self) -> Task<Message> {
        if self.display_mode.busy
            || self.display_mode.pending()
            || !self
                .display_mode
                .inventory
                .as_ref()
                .is_some_and(|inventory| inventory.available(self.display_mode.selected))
        {
            return Task::none();
        }

        let mode = self.display_mode.selected;
        self.display_operation(false, move || {
            compositor_ipc::request("output-layout", json!({"layout": mode.name()})).map(|_| ())
        })
    }

    pub(super) fn confirm_display_mode(&mut self, keep: bool) -> Task<Message> {
        if self.display_mode.busy || !self.display_mode.pending() {
            return Task::none();
        }

        self.display_operation(true, move || {
            compositor_ipc::call(if keep { "output-confirm" } else { "output-revert" }).map(|_| ())
        })
    }

    fn display_operation(
        &mut self,
        close: bool,
        operation: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Task<Message> {
        self.display_mode.busy = true;
        self.display_mode.generation = self.display_mode.generation.wrapping_add(1);
        self.display_mode.operation = self.display_mode.operation.wrapping_add(1);
        let serial = self.display_mode.operation;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    operation()?;
                    load()
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result)
            },
            move |result| cosmic::Action::App(Message::DisplayModeCompleted(serial, close, result)),
        )
    }

    pub(super) fn finish_display_mode(
        &mut self,
        serial: u64,
        close: bool,
        result: Result<Inventory, String>,
    ) -> Task<Message> {
        if serial != self.display_mode.operation {
            return Task::none();
        }

        self.display_mode.busy = false;
        match result {
            Ok(inventory) => {
                self.display_mode.update(inventory);
                self.display_mode.error = None;
                if close || !self.display_mode.pending() {
                    return self.close_system_modal();
                }
                Task::none()
            }
            Err(error) => {
                self.display_mode.error = Some(error);
                self.load_display_inventory()
            }
        }
    }

    pub(super) fn display_mode_rows(&self) -> cosmic::widget::Column<'_, cosmic::Action<Message>, cosmic::Theme> {
        let palette = self.config.theme.palette();
        let material_opacity = self.config.theme.material_opacity();
        let model = &self.display_mode;
        let focus_visible = self.system_modal.as_ref().is_some_and(|modal| modal.focus_visible);
        let pending = model.pending();
        let mut header = row![
            text("Display mode").size(17),
            cosmic::iced::widget::Space::new().width(Length::Fill)
        ]
        .align_y(cosmic::iced::Alignment::Center)
        .spacing(12);
        if let Some(profile) = model
            .inventory
            .as_ref()
            .and_then(|inventory| inventory.profile.as_deref())
        {
            header = header.push(
                text(format!("{profile} profile"))
                    .size(12)
                    .class(theme::Text::Color(palette.muted)),
            );
        }

        let mut cards = row::with_capacity(4).spacing(8);
        for (index, mode) in Mode::ALL.into_iter().enumerate() {
            let available = !model.busy
                && !pending
                && model
                    .inventory
                    .as_ref()
                    .is_some_and(|inventory| inventory.available(mode));
            let card = cosmic::widget::column![
                ferese_theme::icons::tinted(mode.icon(), 40, palette.text),
                text(mode.label())
                    .size(13)
                    .wrapping(cosmic::iced::widget::text::Wrapping::None),
                text((index + 1).to_string())
                    .size(11)
                    .class(theme::Text::Color(palette.muted)),
            ]
            .align_x(cosmic::iced::Alignment::Center)
            .spacing(8)
            .width(Length::Fill);
            cards = cards.push(
                button::custom(card)
                    .id(format!("ferese-display-mode-{}", mode.name()).into())
                    .width(Length::Fill)
                    .padding([14, 6])
                    .class(ferese_theme::controls::material_button_style_with_focus(
                        palette,
                        model.selected == mode,
                        material_opacity,
                        focus_visible,
                    ))
                    .on_press_maybe(available.then_some(cosmic::Action::App(Message::SelectDisplayMode(mode)))),
            );
        }

        let mut rows = cosmic::widget::column![header, cards].spacing(14);
        if let Some(inventory) = &model.inventory {
            let mut displays = cosmic::widget::column::with_capacity(inventory.displays.len()).spacing(6);
            for display in &inventory.displays {
                displays = displays.push(
                    row![
                        ferese_theme::icons::tinted(ferese_theme::icons::DISPLAY, 16, palette.muted),
                        text(&display.label).size(12).class(theme::Text::Color(palette.muted))
                    ]
                    .spacing(8)
                    .align_y(cosmic::iced::Alignment::Center),
                );
            }

            rows = rows.push(displays);
        }

        if let Some(error) = &model.error {
            rows = rows.push(text(error).size(12));
        }

        let hint = if model.busy {
            "Applying display configuration…".to_owned()
        } else if pending {
            let remaining = model
                .inventory
                .as_ref()
                .map_or(15, |inventory| inventory.timeout)
                .saturating_sub(model.confirmation_since.map_or(0, |since| since.elapsed().as_secs()));
            format!("Keep this configuration? Reverts in {remaining}s.")
        } else {
            "Arrow keys or 1–4 to choose. Enter to apply.".to_owned()
        };
        let apply = if pending {
            Message::KeepDisplayMode
        } else {
            Message::ApplyDisplayMode
        };
        let cancel = if pending {
            Message::RevertDisplayMode
        } else {
            Message::CancelPower
        };
        let can_apply = !model.busy
            && (pending
                || model
                    .inventory
                    .as_ref()
                    .is_some_and(|inventory| inventory.available(model.selected)));
        rows.push(text(hint).size(12).class(theme::Text::Color(palette.muted)))
            .push(
                container(
                    row![
                        cosmic::iced::widget::Space::new().width(Length::Fill),
                        ferese_theme::controls::text_button(
                            if pending { "Revert" } else { "Cancel" },
                            shell_font(),
                            palette,
                            false
                        )
                        .class(ferese_theme::controls::material_button_style_with_focus(
                            palette,
                            false,
                            material_opacity,
                            focus_visible
                        ))
                        .id("ferese-modal-cancel".into())
                        .on_press_maybe((!model.busy).then_some(cosmic::Action::App(cancel))),
                        ferese_theme::controls::text_button(
                            if pending { "Keep" } else { "Apply" },
                            shell_font(),
                            palette,
                            true
                        )
                        .class(ferese_theme::controls::button_style_with_focus(
                            palette,
                            true,
                            focus_visible
                        ))
                        .id("ferese-display-apply".into())
                        .on_press_maybe(can_apply.then_some(cosmic::Action::App(apply))),
                    ]
                    .spacing(10),
                )
                .id("ferese-display-modal-actions"),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(internal: bool, enabled: bool, connected: bool, mirror: bool) -> Value {
        json!({
            "connector": if internal { "eDP-1" } else { "DP-1" },
            "connected": connected, "internal": internal, "applied_enabled": enabled,
            "mirror_source": mirror.then_some("source"),
            "available_modes": [{"width": 1920, "height": 1080, "preferred": true}],
            "profile": "desk"
        })
    }

    #[test]
    fn disabled_and_mirrored_external_monitors_keep_the_bar_trigger() {
        for mirror in [false, true] {
            let inventory = Inventory::parse(
                json!([output(true, true, true, false), output(false, mirror, true, mirror)]),
                json!({}),
            )
            .unwrap();
            assert!(inventory.external_connected());
            assert_eq!(inventory.current(), if mirror { Mode::Mirror } else { Mode::Internal });
            assert!(Mode::ALL.into_iter().all(|mode| inventory.available(mode)));
        }

        let removed = Inventory::parse(
            json!([output(true, true, true, false), output(false, false, false, false)]),
            json!({}),
        )
        .unwrap();
        assert!(!removed.external_connected());
        assert!(removed.available(Mode::Internal));
        assert!(!removed.available(Mode::External));
        assert!(!removed.available(Mode::Extend));
        assert!(!removed.available(Mode::Mirror));
    }

    #[test]
    fn confirmation_tracks_backend_state_and_profile_timeout() {
        let mut model = Model::default();
        let outputs = json!([output(true, true, true, false), output(false, true, true, false)]);
        model.update(
            Inventory::parse(
                outputs.clone(),
                json!({"confirmation_pending": true, "profiles": [{"name": "desk", "confirm_timeout": 7}]}),
            )
            .unwrap(),
        );
        assert!(model.pending());
        assert_eq!(model.selected, Mode::Extend);
        assert_eq!(model.inventory.as_ref().unwrap().timeout, 7);
        let since = model.confirmation_since;
        model.update(Inventory::parse(outputs.clone(), json!({"confirmation_pending": true})).unwrap());
        assert_eq!(
            model.confirmation_since, since,
            "inventory refresh must not restart the countdown"
        );
        model.update(Inventory::parse(outputs, json!({"confirmation_pending": false})).unwrap());
        assert!(!model.pending());
        assert_eq!(model.confirmation_since, None);
    }

    #[test]
    fn unusable_monitor_does_not_enable_an_unavailable_mode() {
        let inventory = Inventory::parse(
            json!([{"connector":"DP-1", "connected":true, "internal":false, "available_modes":[]}]),
            json!({}),
        )
        .unwrap();
        assert!(inventory.external_connected());
        assert!(Mode::ALL.into_iter().all(|mode| !inventory.available(mode)));
    }
}
