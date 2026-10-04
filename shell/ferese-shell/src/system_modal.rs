use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::widget::Space;
use cosmic::widget::column;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PowerAction {
    Poweroff,
    Reboot,
    Suspend,
    Logout(u32),
}

impl PowerAction {
    pub(super) fn from_status(action: status::Action) -> Option<Self> {
        match action {
            status::Action::Poweroff => Some(Self::Poweroff),
            status::Action::Reboot => Some(Self::Reboot),
            status::Action::Suspend => Some(Self::Suspend),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Poweroff => "Power off",
            Self::Reboot => "Restart",
            Self::Suspend => "Suspend",
            Self::Logout(_) => "Log out",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Poweroff => "Power off this computer?",
            Self::Reboot => "Restart this computer?",
            Self::Suspend => "Suspend this computer?",
            Self::Logout(_) => "Log out of Ferese?",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Suspend => "Your apps will stay open. The session will not be locked.",
            Self::Logout(_) => "Save your work. Your apps will close when you log out.",
            _ => "Save your work before continuing.",
        }
    }
}

struct ModalSurface {
    id: window::Id,
    primary: bool,
    effects: Option<EffectsBinding>,
    regions: motion::Regions,
}

#[derive(Clone, Debug, PartialEq)]
enum Content {
    Power(PowerAction),
    Guide(Vec<keybinding_guide::Entry>),
    Displays,
}

pub(super) struct SystemModal {
    content: Content,
    surfaces: Vec<ModalSurface>,
    pub(super) motion: motion::PopupMotion,
    error: Option<String>,
    inhibitors: Option<compositor_ipc::Approval>,
    keyboard_nav: bool,
    pub(super) focus_visible: bool,
}

impl SystemModal {
    pub(super) fn is_guide(&self) -> bool {
        matches!(self.content, Content::Guide(_))
    }

    pub(super) fn is_display_mode(&self) -> bool {
        matches!(self.content, Content::Displays)
    }

    pub(super) fn contains(&self, id: window::Id) -> bool {
        self.surfaces.iter().any(|surface| surface.id == id)
    }
}

impl FereseShell {
    pub(super) fn open_system_modal(&mut self, action: PowerAction, output_name: Option<&str>) -> Task<Message> {
        self.open_modal(Content::Power(action), output_name)
    }

    pub(super) fn open_display_modal(&mut self, output: Option<&str>) -> Task<Message> {
        if self
            .system_modal
            .as_ref()
            .is_some_and(|modal| modal.is_display_mode() && !modal.motion.closing())
        {
            return self.close_system_modal();
        }
        self.display_mode.begin();
        let open = self.open_modal(Content::Displays, output);
        let load = self.load_display_inventory();
        Task::batch([open, load])
    }

    pub(super) fn rebuild_system_modal(&mut self) -> Task<Message> {
        let guide = self.system_modal.as_ref().and_then(|modal| match &modal.content {
            Content::Guide(entries)
                if !modal.motion.closing() && (self.guide_load.manual || self.config.status.keybinding_guide) =>
            {
                Some(entries.clone())
            }
            _ => None,
        });
        let destroy = self.destroy_system_modal(true);
        if self.display_mode.open {
            let open = self.open_modal(Content::Displays, None);
            return Task::batch([destroy, open]);
        }
        if let Some(entries) = guide {
            if self.outputs.is_empty() {
                self.guide_shown = false;
                self.guide_attempts = 0;
                return destroy;
            }
            let open = self.open_guide(entries);
            Task::batch([destroy, open])
        } else {
            destroy
        }
    }

    pub(super) fn open_guide(&mut self, entries: Vec<keybinding_guide::Entry>) -> Task<Message> {
        self.open_guide_on(entries, None)
    }

    pub(super) fn open_guide_on(
        &mut self,
        entries: Vec<keybinding_guide::Entry>,
        output: Option<&str>,
    ) -> Task<Message> {
        self.open_modal(Content::Guide(entries), output)
    }

    fn open_modal(&mut self, content: Content, output_name: Option<&str>) -> Task<Message> {
        if self.pending_power.is_some() || self.outputs.is_empty() {
            if let Content::Power(PowerAction::Logout(serial)) = content
                && let Some(control) = &self.control
            {
                control.cancel_logout(serial);
            }
            return Task::none();
        }
        if self
            .system_modal
            .as_ref()
            .is_some_and(|modal| modal.content == content && !modal.motion.closing())
        {
            return Task::none();
        }
        if content != Content::Displays {
            self.display_mode.open = false;
        }
        let primary = output_name
            .and_then(|name| {
                self.outputs
                    .iter()
                    .position(|output| output.name.as_deref() == Some(name))
            })
            .or_else(|| self.outputs.iter().position(|output| output.bar == self.bar_surface_id))
            .unwrap_or(0);
        let mut tasks = vec![
            self.destroy_system_modal(true),
            self.destroy_menu(),
            self.close_notification_history(),
        ];
        let mut surfaces = Vec::new();
        let mut outputs = (0..self.outputs.len()).collect::<Vec<_>>();
        outputs.sort_by_key(|index| *index == primary);
        for index in outputs {
            let id = window::Id::unique();
            let output = self.outputs[index].output.clone();
            let primary = index == primary;
            surfaces.push(ModalSurface {
                id,
                primary,
                effects: None,
                regions: Default::default(),
            });
            let action = cosmic::surface::action::app_layer_shell::<Self>(
                |_| Default::default(),
                move |_| SctkLayerSurfaceSettings {
                    id,
                    layer: Layer::Overlay,
                    keyboard_interactivity: if primary {
                        KeyboardInteractivity::Exclusive
                    } else {
                        KeyboardInteractivity::None
                    },
                    anchor: Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM | Anchor::LEFT,
                    output: IcedOutput::Output(output.clone()),
                    namespace: "ferese-system-modal".to_owned(),
                    exclusive_zone: -1,
                    size: Some((None, None)),
                    ..Default::default()
                },
                Some(Box::new(move |app| app.view_system_modal(id))),
            );
            tasks.push(cosmic::task::message(cosmic::Action::Surface(action)));
        }
        let mode = match content {
            Content::Power(PowerAction::Suspend) => Some(compositor_ipc::Mode::Suspend),
            Content::Power(PowerAction::Logout(_)) => Some(compositor_ipc::Mode::Logout),
            Content::Power(_) => Some(compositor_ipc::Mode::Shutdown),
            Content::Guide(_) | Content::Displays => None,
        };
        if let Some(mode) = mode {
            let id = surfaces[0].id;
            tasks.push(Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || compositor_ipc::inhibitors(mode))
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result)
                },
                move |result| cosmic::Action::App(Message::SystemInhibitors(id, result)),
            ));
        }
        let keyboard_nav = self.core.keyboard_nav();
        self.core.set_keyboard_nav(false);
        self.system_modal = Some(SystemModal {
            content,
            surfaces,
            motion: motion::PopupMotion::new(self.config.animations),
            error: None,
            inhibitors: mode.is_none().then(compositor_ipc::Approval::default),
            keyboard_nav,
            focus_visible: false,
        });
        Task::batch(tasks)
    }

    pub(super) fn set_system_inhibitors(
        &mut self,
        id: window::Id,
        result: Result<compositor_ipc::Approval, String>,
    ) -> Task<Message> {
        if let Some(modal) = &mut self.system_modal
            && modal.contains(id)
            && matches!(modal.content, Content::Power(_))
            && !modal.motion.closing()
        {
            match result {
                Ok(inhibitors) => modal.inhibitors = Some(inhibitors),
                Err(error) => {
                    modal.error = Some(error);
                }
            }
        } else if let Ok(approval) = result {
            return Self::cancel_query(approval);
        }
        Task::none()
    }

    fn cancel_query(approval: compositor_ipc::Approval) -> Task<Message> {
        Task::perform(
            async move {
                let _ = tokio::task::spawn_blocking(move || approval.cancel()).await;
            },
            |_| cosmic::Action::App(Message::AnimatePower),
        )
    }

    pub(super) fn attach_power_material(&mut self, id: window::Id, surface: &wl_surface::WlSurface) {
        let Some(modal) = &mut self.system_modal else {
            return;
        };
        let Some(entry) = modal.surfaces.iter_mut().find(|entry| entry.id == id) else {
            return;
        };
        if entry.primary && entry.effects.is_none() {
            match EffectsBinding::attach_role(surface, None, 0.0) {
                Ok(effects) => entry.effects = Some(effects),
                Err(error) => eprintln!("ferese-shell: system modal material unavailable: {error}"),
            }
        }
        if entry.primary {
            modal.motion.begin(Instant::now());
        }
    }

    pub(super) fn close_system_modal(&mut self) -> Task<Message> {
        if self.system_modal.as_ref().is_some_and(SystemModal::is_display_mode) {
            if self.display_mode.busy {
                return Task::none();
            }
            if self.display_mode.pending() {
                return self.confirm_display_mode(false);
            }
            self.display_mode.open = false;
        }

        if let Some(modal) = &mut self.system_modal {
            if let Content::Power(PowerAction::Logout(serial)) = modal.content
                && let Some(control) = &self.control
            {
                control.cancel_logout(serial);
            }
            let cancel = modal
                .inhibitors
                .take()
                .map(Self::cancel_query)
                .unwrap_or_else(Task::none);
            modal.motion.retarget(0.0, Instant::now());
            return Task::batch([cancel, self.animate_system_modal()]);
        }
        self.animate_system_modal()
    }

    pub(super) fn animate_system_modal(&mut self) -> Task<Message> {
        if self
            .system_modal
            .as_ref()
            .is_some_and(|modal| modal.motion.closing() && !modal.motion.animating())
        {
            return self.destroy_system_modal(false);
        }
        Task::none()
    }

    pub(super) fn destroy_system_modal(&mut self, cancel: bool) -> Task<Message> {
        let Some(modal) = self.system_modal.take() else {
            return Task::none();
        };
        self.core.set_keyboard_nav(modal.keyboard_nav);
        if cancel
            && let Content::Power(PowerAction::Logout(serial)) = modal.content
            && let Some(control) = &self.control
        {
            control.cancel_logout(serial);
        }
        let mut tasks = modal
            .surfaces
            .into_iter()
            .map(|surface| destroy_layer_surface(surface.id))
            .collect::<Vec<_>>();
        if cancel && let Some(approval) = modal.inhibitors {
            tasks.push(Self::cancel_query(approval));
        }
        Task::batch(tasks)
    }

    pub(super) fn cancel_logout_modal(&mut self, serial: u32) -> Task<Message> {
        if self
            .system_modal
            .as_ref()
            .is_some_and(|modal| modal.content == Content::Power(PowerAction::Logout(serial)))
        {
            self.destroy_system_modal(false)
        } else {
            Task::none()
        }
    }

    pub(super) fn focus_system_modal(&self, id: window::Id) -> Task<Message> {
        if self.system_modal.as_ref().is_some_and(|modal| {
            !modal.is_display_mode()
                && !modal.motion.closing()
                && modal.surfaces.iter().any(|surface| surface.id == id && surface.primary)
        }) {
            button::focus("ferese-modal-cancel".into())
        } else {
            Task::none()
        }
    }

    pub(super) fn navigate_system_modal(&mut self, backwards: bool) -> Task<Message> {
        let Some(modal) = &mut self.system_modal else {
            return Task::none();
        };

        if modal.motion.closing() {
            return Task::none();
        }

        modal.focus_visible = true;

        cosmic::iced::advanced::widget::operate(modal_focus_operation(backwards))
    }

    pub(super) fn execute_system_modal(&mut self) -> Task<Message> {
        let Some(modal) = &mut self.system_modal else {
            return Task::none();
        };
        if modal.motion.closing() || self.pending_power.is_some() || modal.inhibitors.is_none() {
            return Task::none();
        }
        let Content::Power(power) = modal.content else {
            return self.close_system_modal();
        };
        let approval = modal.inhibitors.as_ref().unwrap().clone();
        let action = match power {
            PowerAction::Logout(serial) => {
                if let Some(control) = &self.control {
                    control.confirm_logout(serial, approval.revision, approval.token, approval.force());
                    return self.destroy_system_modal(false);
                }
                modal.error = Some("Ferese shell control is unavailable".to_owned());
                return Task::none();
            }
            PowerAction::Poweroff => status::Action::Poweroff,
            PowerAction::Reboot => status::Action::Reboot,
            PowerAction::Suspend => status::Action::Suspend,
        };
        let id = modal.surfaces[0].id;
        self.pending_power = Some((id, power));
        let destroy = self.destroy_system_modal(false);
        let execute = Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let result = approval
                        .prepare()
                        .and_then(|()| status::execute_power(action, approval.force()));
                    if result.is_err() {
                        approval.cancel();
                    }
                    result
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result)
            },
            move |result| cosmic::Action::App(Message::PowerCompleted(id, result)),
        );
        Task::batch([destroy, execute])
    }

    pub(super) fn finish_power_action(&mut self, id: window::Id, result: Result<(), String>) -> Task<Message> {
        let Some((pending_id, action)) = self.pending_power else {
            return Task::none();
        };
        if id != pending_id {
            return Task::none();
        }
        self.pending_power = None;
        match result {
            Ok(()) => Task::none(),
            Err(error) => {
                let task = self.open_system_modal(action, None);
                if let Some(modal) = &mut self.system_modal {
                    modal.error = Some(error);
                }
                task
            }
        }
    }

    fn view_system_modal(&self, id: window::Id) -> Element<'_, cosmic::Action<Message>> {
        let Some(modal) = &self.system_modal else {
            return text("").into();
        };
        let Some(surface) = modal.surfaces.iter().find(|surface| surface.id == id) else {
            return text("").into();
        };
        motion::frame_driven(
            modal.motion.revision(),
            move |now| self.view_system_modal_at(id, now),
            |now| modal.motion.frame_active(now),
            |now| {
                if let Some(effects) = &surface.effects {
                    let _ = effects.set_presentation(
                        &surface.regions.lock().unwrap(),
                        modal.motion.progress_at(now),
                        [],
                        ferese_surface_effects_v1::Role::Modal,
                    );
                }
            },
            |now| {
                (surface.primary && modal.motion.closing() && !modal.motion.animating_at(now))
                    .then_some(cosmic::Action::App(Message::AnimatePower))
            },
        )
    }

    fn view_system_modal_at(&self, id: window::Id, now: Instant) -> Element<'_, cosmic::Action<Message>> {
        let Some(modal) = &self.system_modal else {
            return text("").into();
        };
        let Some(surface) = modal.surfaces.iter().find(|surface| surface.id == id) else {
            return text("").into();
        };
        let progress = modal.motion.progress_at(now);
        let content: Element<'_, cosmic::Action<Message>> = if surface.primary {
            let theme = self.config.theme;
            let material = surface.effects.is_some();
            // Opacity belongs to the presented surface, never to semantic colors.
            let palette = theme.palette();
            let rows = match &modal.content {
                Content::Power(action) => {
                    let mut rows = column![
                        text(action.title()).size(17),
                        text(action.description())
                            .size(13)
                            .class(theme::Text::Color(palette.muted)),
                    ]
                    .spacing(10);
                    if let Some(inhibitors) = &modal.inhibitors
                        && !inhibitors.reasons.is_empty()
                    {
                        rows = rows.push(
                            text("Applications requested that this action be prevented:")
                                .size(13)
                                .class(theme::Text::Color(palette.muted)),
                        );
                        let items = inhibitors.reasons.iter().fold(
                            column::with_capacity(inhibitors.reasons.len()).spacing(6),
                            |rows, reason| rows.push(text(reason).size(13)),
                        );
                        rows = rows
                            .push(container(cosmic::widget::scrollable(items).height(Length::Shrink)).max_height(160));
                    }
                    let label = if modal.inhibitors.as_ref().is_some_and(|approval| approval.force()) {
                        format!("{} anyway", action.label())
                    } else {
                        action.label().to_owned()
                    };
                    if let Some(error) = &modal.error {
                        rows = rows.push(text(error).size(13));
                    }
                    rows.push(
                        container(
                            row![
                                Space::new().width(Length::Fill),
                                ferese_theme::controls::text_button("Cancel", shell_font(), palette, false)
                                    .class(ferese_theme::controls::button_style_with_focus(
                                        palette,
                                        false,
                                        modal.focus_visible,
                                    ))
                                    .id("ferese-modal-cancel".into())
                                    .on_press(cosmic::Action::App(Message::CancelPower)),
                                ferese_theme::controls::text_button(label, shell_font(), palette, true)
                                    .class(ferese_theme::controls::button_style_with_focus(
                                        palette,
                                        true,
                                        modal.focus_visible,
                                    ))
                                    .on_press_maybe(
                                        modal
                                            .inhibitors
                                            .is_some()
                                            .then_some(cosmic::Action::App(Message::ExecutePower))
                                    ),
                            ]
                            .spacing(10),
                        )
                        .id("ferese-system-modal-controls"),
                    )
                }
                Content::Displays => column![container(self.display_mode_rows()).id("ferese-system-modal-controls")],
                Content::Guide(entries) => {
                    let mut bindings = column::with_capacity(entries.len()).spacing(12);
                    for entry in entries {
                        bindings = bindings.push(
                            row![
                                text(&entry.keys).size(13).width(Length::FillPortion(1)),
                                text(&entry.description).size(13).width(Length::FillPortion(1)),
                            ]
                            .spacing(16),
                        );
                    }
                    let height = self
                        .outputs
                        .iter()
                        .filter_map(|output| output.size)
                        .map(|(_, height)| height)
                        .min()
                        .unwrap_or(720)
                        .saturating_sub(360)
                        .clamp(40, 400) as f32;
                    column![
                        text("Welcome to Ferese").size(23),
                        ferese_theme::menus::section_label("Your active shortcuts", shell_font(), palette.muted,)
                            .size(14),
                        container(cosmic::widget::scrollable(bindings).height(Length::Shrink)).max_height(height),
                        text("Disable this guide in Settings → Shortcuts. It appears at each login until disabled.")
                            .size(12)
                            .class(theme::Text::Color(palette.muted)),
                        container(row![
                            Space::new().width(Length::Fill),
                            ferese_theme::controls::text_button("Got it", shell_font(), palette, true,)
                                .class(ferese_theme::controls::button_style_with_focus(
                                    palette,
                                    true,
                                    modal.focus_visible,
                                ))
                                .id("ferese-modal-cancel".into())
                                .on_press(cosmic::Action::App(Message::CancelPower)),
                        ])
                        .id("ferese-system-modal-controls"),
                    ]
                    .spacing(14)
                }
            };
            container(rows)
                .id("ferese-blur-card")
                .width(Length::Fill)
                .max_width(if modal.is_display_mode() || modal.is_guide() {
                    600
                } else {
                    360
                })
                .padding(if modal.is_guide() { 24 } else { 18 })
                .class(theme::Container::custom(move |_| container::Style {
                    background: (!material)
                        .then_some(Background::Color(color_with_opacity(theme.surface_popover, 1.0))),
                    text_color: Some(palette.text),
                    icon_color: Some(palette.text),
                    border: Border {
                        shape: BorderShape::Continuous,
                        color: palette.muted.scale_alpha(0.16),
                        width: 1.0,
                        radius: theme.material_radius.into(),
                        ..Default::default()
                    },
                    snap: true,
                    ..Default::default()
                }))
                .into()
        } else {
            text("").into()
        };
        let content = if surface.primary {
            motion::animated(
                content,
                progress,
                surface.regions.clone(),
                self.config.theme.material_radius,
            )
        } else {
            content
        };
        let scrim_alpha = if surface.effects.is_some() { 1.0 } else { progress };
        let centered = container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(24)
            .align_x(cosmic::iced::Alignment::Center)
            .align_y(cosmic::iced::Alignment::Center)
            .class(theme::Container::custom(move |_| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.34 * scrim_alpha))),
                ..Default::default()
            }));
        centered.into()
    }
}

// Restrict Tab traversal to the modal actions, excluding the bar and desktop widgets.
fn modal_focus_operation<T: Send + 'static>(backwards: bool) -> Box<dyn cosmic::iced::advanced::widget::Operation<T>> {
    use cosmic::iced::advanced::widget::{Id, operation};
    let target = Id::new("ferese-system-modal-controls");
    if backwards {
        Box::new(operation::scoped(target, operation::focusable::focus_previous()))
    } else {
        Box::new(operation::scoped(target, operation::focusable::focus_next()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_stays_within_modal_actions_in_both_directions() {
        use cosmic::iced::Rectangle;
        use cosmic::iced::advanced::widget::{Id, Operation, operation};

        #[derive(Default)]
        struct Focus(bool);
        impl operation::Focusable for Focus {
            fn is_focused(&self) -> bool {
                self.0
            }
            fn focus(&mut self) {
                self.0 = true;
            }
            fn unfocus(&mut self) {
                self.0 = false;
            }
        }

        let mut outside = Focus(true);
        let mut cancel = Focus(false);
        let mut confirm = Focus(false);
        assert!(
            !cancel.0 && !confirm.0,
            "opening the chooser leaves its buttons unfocused"
        );
        for (backwards, expected_cancel) in [
            (false, true),
            (false, false),
            (false, true),
            (true, false),
            (true, true),
        ] {
            let mut current: Box<dyn Operation<()>> = modal_focus_operation(backwards);
            loop {
                current.focusable(Some(&Id::new("bar")), Rectangle::default(), &mut outside);
                current.container(Some(&Id::new("ferese-system-modal-controls")), Rectangle::default());
                current.traverse(&mut |operation| {
                    operation.focusable(Some(&Id::new("cancel")), Rectangle::default(), &mut cancel);
                    operation.focusable(Some(&Id::new("confirm")), Rectangle::default(), &mut confirm);
                });
                match current.finish() {
                    operation::Outcome::Chain(next) => current = next,
                    _ => break,
                }
            }
            assert!(outside.0, "modal navigation changed focus outside the modal");
            assert_ne!(cancel.0, confirm.0);
            assert_eq!(cancel.0, expected_cancel);
        }
        assert!(cancel.0);
        assert!(!confirm.0);
    }

    #[test]
    fn only_power_actions_require_confirmation() {
        assert_eq!(
            PowerAction::from_status(status::Action::Poweroff),
            Some(PowerAction::Poweroff)
        );
        assert_eq!(
            PowerAction::from_status(status::Action::Reboot),
            Some(PowerAction::Reboot)
        );
        assert_eq!(
            PowerAction::from_status(status::Action::Suspend),
            Some(PowerAction::Suspend)
        );
        assert_eq!(PowerAction::from_status(status::Action::Brightness(50)), None);
        assert_eq!(PowerAction::Logout(1).label(), "Log out");
    }
}
