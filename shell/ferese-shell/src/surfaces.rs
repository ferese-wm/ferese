use super::{
    Anchor, Background, BarMetrics, Color, Connection, ContentFit, Dispatch, EFFECT_FRAME_PENDING, Element, Event,
    EventQueue, FereseEffectsManagerV1, FereseShell, FereseSurfaceEffectsV1, GlobalListContents, IcedMargin,
    IcedOutput, KeyboardInteractivity, Layer, Length, Limits, Message, Ordering, OutputSurfaces, PlatformSpecific,
    Proxy, QueueHandle, SctkLayerSurfaceSettings, Task, WallpaperMode, avoid_widget_overlap, clamp_note_position,
    config, container, delegate_noop, destroy_layer_surface, ferese_surface_effects_v1, image, output_bar_hidden,
    registry_queue_init, set_margin, status_ui, text, theme, wayland, window, wl_output, wl_registry, wl_surface,
};

pub(super) fn panel_placement(edge: ferese_config::panel::Edge, margin: i32, side: i32) -> (Anchor, IcedMargin) {
    let (anchor, top, bottom) = match edge {
        ferese_config::panel::Edge::Top => (Anchor::TOP, margin, 0),
        ferese_config::panel::Edge::Bottom => (Anchor::BOTTOM, 0, margin),
    };
    (
        anchor,
        IcedMargin {
            top,
            right: side,
            bottom,
            left: side,
        },
    )
}

impl FereseShell {
    pub(super) fn output_event(&mut self, event: wayland::OutputEvent, output: wl_output::WlOutput) -> Task<Message> {
        if matches!(event, wayland::OutputEvent::Removed) {
            if let Some(index) = self.outputs.iter().position(|entry| entry.output == output) {
                let entry = self.outputs.remove(index);
                self.refresh_media_art(false);
                let menu = if self.menu.as_ref().is_some_and(|menu| menu.anchor.parent == entry.bar) {
                    self.destroy_menu()
                } else {
                    Task::none()
                };
                let mut tasks = vec![
                    self.dismiss_workspace_tooltip(),
                    menu.chain(destroy_layer_surface(entry.bar)),
                    self.rebuild_system_modal(),
                ];
                if self.note_drag.as_ref().is_some_and(|drag| {
                    entry.clock == Some(drag.source) || entry.notes.iter().any(|(_, id)| *id == drag.source)
                }) {
                    tasks.push(self.finish_note_drag(false));
                }

                if let Some(wallpaper) = entry.wallpaper {
                    tasks.push(destroy_layer_surface(wallpaper));
                }

                if let Some(clock) = entry.clock {
                    tasks.push(destroy_layer_surface(clock));
                }

                for (_, id) in entry.notes {
                    self.note_pointer.remove(&id);
                    tasks.push(destroy_layer_surface(id));
                }

                return Task::batch(tasks);
            }

            return Task::none();
        }

        let size = match &event {
            wayland::OutputEvent::Created(info) => info.as_ref().and_then(|info| info.logical_size),
            wayland::OutputEvent::InfoUpdate(info) => info.logical_size,
            _ => None,
        };
        let name = match event {
            wayland::OutputEvent::Created(info) => info.and_then(|info| info.name),
            wayland::OutputEvent::InfoUpdate(info) => info.name,
            _ => None,
        };

        if let Some(entry) = self.outputs.iter_mut().find(|entry| entry.output == output) {
            if size.is_some() {
                entry.size = size;
            }

            let changed = name.is_some() && entry.name != name;
            if name.is_some() {
                entry.name = name;
            }

            let bar = entry.bar;
            let margin = self.update_panel_margin(bar, false);
            return if changed {
                Task::batch([margin, self.rebuild_clocks(true), self.rebuild_notes(true)])
            } else {
                margin
            };
        }

        let bar_surface_id = window::Id::unique();
        let wallpaper_surface_id = window::Id::unique();
        let geometry = self.config.panels[0].geometry;
        let bar_layout = self.config.panels[0].background;
        let (edge_anchor, margin) = panel_placement(self.config.panels[0].edge, geometry.edge_margin, 0);
        let bar = BarMetrics::from(geometry);
        let wallpaper_output = output.clone();
        let bar_output = output.clone();
        let hidden = output_bar_hidden(
            &self.snapshot,
            self.snapshot
                .outputs
                .iter()
                .find(|output| Some(output.name.as_str()) == name.as_deref())
                .map(|output| output.id),
        );
        self.outputs.push(OutputSurfaces {
            output,
            name,
            bar: bar_surface_id,
            wallpaper: std::env::var_os("FERESE_COMPOSITOR_WALLPAPER")
                .is_none()
                .then_some(wallpaper_surface_id),
            effects: None,
            bar_regions: Vec::new(),
            panel_resolution: Default::default(),
            bar_margin_horizontal: 0,
            clock: None,
            notes: Vec::new(),
            size,
            hidden,
        });
        self.refresh_media_art(false);
        let wallpaper_action = cosmic::surface::action::app_layer_shell::<Self>(
            |_| Default::default(),
            move |_| SctkLayerSurfaceSettings {
                id: wallpaper_surface_id,
                layer: Layer::Background,
                keyboard_interactivity: KeyboardInteractivity::None,
                input_zone: Some(Vec::new()),
                anchor: Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM | Anchor::LEFT,
                output: IcedOutput::Output(wallpaper_output.clone()),
                namespace: "ferese-shell-wallpaper".to_owned(),
                // Wallpaper covers the full output, including beneath the bar
                // and its floating margins. Zero would use the remaining workspace.
                exclusive_zone: -1,
                size: Some((None, None)),
                size_limits: Limits::NONE,
                ..Default::default()
            },
            Some(Box::new(Self::view_wallpaper)),
        );
        let bar_action = cosmic::surface::action::app_layer_shell::<Self>(
            |_| Default::default(),
            move |_| SctkLayerSurfaceSettings {
                id: bar_surface_id,
                input_zone: super::bar::input_region(bar_layout, hidden, &[]),
                layer: Layer::Top,
                keyboard_interactivity: KeyboardInteractivity::None,
                anchor: edge_anchor | Anchor::LEFT | Anchor::RIGHT,
                output: IcedOutput::Output(bar_output.clone()),
                namespace: "ferese-shell-top-bar".to_owned(),
                // Measure at full width before applying the requested side inset.
                margin,
                size: Some((None, Some(bar.height.round() as u32))),
                size_limits: Limits::NONE,
                // Reserve space toward the desktop; layer-shell accounts for the edge margin.
                exclusive_zone: (bar.height.round() as i32).saturating_add(geometry.window_clearance),
            },
            Some(Box::new(move |app| app.view_layer(bar_surface_id))),
        );

        // Submit the small interactive surface before the full-screen image.
        let mut surfaces = vec![bar_action];
        if std::env::var_os("FERESE_COMPOSITOR_WALLPAPER").is_none() {
            surfaces.push(wallpaper_action);
        }

        let tasks = surfaces
            .into_iter()
            .map(cosmic::Action::Surface)
            .map(cosmic::task::message);

        Task::batch([
            Task::batch(tasks),
            self.rebuild_clocks(false),
            self.rebuild_notes(false),
            self.rebuild_system_modal(),
            cosmic::task::message(cosmic::Action::App(Message::ShowGuide)),
        ])
    }

    pub(super) fn handle_event(&mut self, event: Event, id: window::Id) -> Task<Message> {
        if let Event::Mouse(cosmic::iced::mouse::Event::ButtonPressed(_)) = &event
            && let Some(modal) = &mut self.system_modal
            && modal.contains(id)
        {
            modal.focus_visible = false;
        }

        if let Event::Mouse(mouse) = &event {
            if !self
                .outputs
                .iter()
                .any(|entry| entry.clock == Some(id) || entry.notes.iter().any(|(_, surface)| *surface == id))
            {
                return Task::none();
            }

            match mouse {
                cosmic::iced::mouse::Event::CursorMoved { position } => {
                    self.note_pointer.insert(id, *position);

                    if let Some(drag) = &mut self.note_drag
                        && drag.source == id
                    {
                        let delta = *position - drag.start;
                        let dimensions = if let Some(id) = &drag.id {
                            self.config
                                .desktop_widgets
                                .notes
                                .iter()
                                .find(|note| &note.id == id)
                                .map(|note| (note.width, note.height))
                        } else {
                            let clock = &self.config.desktop_widgets.clock;
                            Some((clock.width, clock.height))
                        };

                        if let Some(dimensions) = dimensions {
                            let requested = clamp_note_position(drag.origin + delta, drag.output_size, dimensions);
                            drag.position = avoid_widget_overlap(drag.position, requested, dimensions, &drag.obstacles);
                            // Move a compact, cached buffer in the compositor instead of
                            // repainting an output-sized buffer for every pointer event.
                            return set_margin(
                                drag.overlay,
                                drag.position.y.round() as i32,
                                0,
                                0,
                                drag.position.x.round() as i32,
                            );
                        }
                    }
                }

                cosmic::iced::mouse::Event::ButtonReleased(cosmic::iced::mouse::Button::Left)
                    if self.note_drag.as_ref().is_some_and(|drag| drag.source == id) =>
                {
                    return self.finish_note_drag(true);
                }
                _ => {}
            }
        }
        if let Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
            key: cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Enter),
            ..
        }) = &event
            && self
                .system_modal
                .as_ref()
                .is_some_and(|modal| modal.is_display_mode() && modal.contains(id))
        {
            return if self.display_mode.pending() {
                self.confirm_display_mode(true)
            } else {
                self.apply_display_mode()
            };
        }

        if let Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed { key, .. }) = &event
            && self
                .system_modal
                .as_ref()
                .is_some_and(|modal| modal.is_display_mode() && modal.contains(id))
            && matches!(
                key,
                cosmic::iced::keyboard::Key::Character(_)
                    | cosmic::iced::keyboard::Key::Named(
                        cosmic::iced::keyboard::key::Named::ArrowLeft
                            | cosmic::iced::keyboard::key::Named::ArrowRight
                            | cosmic::iced::keyboard::key::Named::ArrowUp
                            | cosmic::iced::keyboard::key::Named::ArrowDown
                    )
            )
        {
            return self.display_mode_key(key);
        }
        if let Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
            key: cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Tab),
            modifiers,
            ..
        }) = &event
            && !modifiers.control()
            && self.system_modal.as_ref().is_some_and(|modal| modal.contains(id))
        {
            return self.navigate_system_modal(modifiers.shift());
        }
        if matches!(
            &event,
            Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                key: cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Escape),
                ..
            })
        ) {
            if self.system_modal.is_some() {
                return self.close_system_modal();
            }
            if self.note_drag.is_some() {
                return self.finish_note_drag(false);
            }

            if self.note_editor.is_some() {
                return self.finish_note_edit();
            }

            if self.notifications.history_open {
                return self.close_notification_history();
            }
        }

        match event {
            Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Output(event, output))) => {
                self.output_event(event, output)
            }
            Event::Window(window::Event::Opened { .. })
                if self.outputs.iter().any(|entry| entry.bar == id)
                    || self.menu.as_ref().is_some_and(|menu| menu.id == id)
                    || self.system_modal.as_ref().is_some_and(|modal| modal.contains(id))
                    || self
                        .notification_surface
                        .as_ref()
                        .is_some_and(|surface| surface.id == id) =>
            {
                Task::batch([
                    self.focus_system_modal(id),
                    window::run(id, native_wayland_surface)
                        .map(move |surface| cosmic::Action::App(Message::NativeSurface(id, surface))),
                ])
            }
            Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                key: cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Escape),
                ..
            }) if self.menu.is_some() => self.close_menu(),
            Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Popup(event, surface, popup_id))) => {
                if self.workspace_ui.tooltip == Some(popup_id) && matches!(event, wayland::PopupEvent::Done) {
                    self.workspace_ui.tooltip = None;
                    return self.dismiss_workspace_tooltip();
                }
                if let Some(menu) = &mut self.menu
                    && menu.id == popup_id
                {
                    match event {
                        wayland::PopupEvent::Done => {
                            if menu.kind == status_ui::Menu::Notifications {
                                self.notifications.history_open = false;
                                self.notifications.hovered = None;
                            }
                            self.menu = None;
                            EFFECT_FRAME_PENDING.store(false, Ordering::Relaxed);
                            return self.sync_notification_surface();
                        }
                        wayland::PopupEvent::Configured { .. } => {
                            menu.prepare_surface(&surface);
                        }
                        _ => {}
                    }
                }
                Task::none()
            }
            Event::Window(window::Event::Closed) if self.menu.as_ref().is_some_and(|menu| menu.id == id) => {
                self.menu = None;
                EFFECT_FRAME_PENDING.store(false, Ordering::Relaxed);
                Task::none()
            }
            Event::Window(window::Event::Closed) if self.workspace_ui.tooltip == Some(id) => {
                self.workspace_ui.tooltip = None;
                self.dismiss_workspace_tooltip()
            }
            Event::Window(window::Event::Closed)
                if self.system_modal.as_ref().is_some_and(|modal| modal.contains(id)) =>
            {
                self.close_system_modal()
            }
            Event::Window(window::Event::Closed) => Task::none(),
            Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Frame(_, surface, frame_id)))
                if self.menu.as_ref().is_some_and(|menu| menu.id == frame_id) =>
            {
                if let Some(menu) = &mut self.menu
                    && menu.effects.is_none()
                {
                    menu.prepare_surface(&surface);
                }
                Task::none()
            }
            Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Frame(_, surface, frame_id)))
                if self.outputs.iter().any(|entry| entry.bar == frame_id) =>
            {
                self.attach_effects(frame_id, &surface);
                Task::none()
            }
            Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Layer(_, surface, layer_id)))
                if self
                    .notification_surface
                    .as_ref()
                    .is_some_and(|entry| entry.id == layer_id) =>
            {
                let entry = self.notification_surface.as_mut().unwrap();

                if entry.effects.is_none() {
                    match EffectsBinding::attach_role(&surface, None, 1.0) {
                        Ok(binding) => entry.effects = Some(binding),
                        Err(error) => {
                            eprintln!("ferese-shell: notification effects unavailable: {error}")
                        }
                    }
                }
                Task::none()
            }
            Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Layer(_, surface, layer_id)))
                if self.outputs.iter().any(|entry| entry.bar == layer_id) =>
            {
                self.attach_effects(layer_id, &surface);
                self.attach_power_material(layer_id, &surface);
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub(super) fn attach_effects(&mut self, id: window::Id, surface: &wl_surface::WlSurface) {
        let hidden = self.bar_hidden(id);
        let islands = self.config.panels[0].background == ferese_config::BarLayout::Islands;
        let Some(entry) = self.outputs.iter_mut().find(|entry| entry.bar == id) else {
            return;
        };

        if entry.effects.is_some() {
            return;
        }

        EFFECT_FRAME_PENDING.store(false, Ordering::Relaxed);

        let binding = if islands {
            // Wait for measured island bounds rather than briefly painting the full bar.
            EffectsBinding::attach_role(surface, None, 1.0)
        } else {
            EffectsBinding::attach(surface, !hidden)
        };

        match binding {
            Ok(binding) => entry.effects = Some(binding),
            Err(error) => eprintln!("ferese-shell: panel material unavailable: {error}"),
        }
    }

    pub(super) fn view_wallpaper(&self) -> Element<'_, cosmic::Action<Message>> {
        let Some(handle) = self.wallpaper.as_ref() else {
            return container(text(""))
                .width(Length::Fill)
                .height(Length::Fill)
                .class(theme::Container::custom(wallpaper_fallback_style))
                .into();
        };
        let content_fit = match self.config.wallpaper.mode {
            WallpaperMode::Fill => ContentFit::Cover,
            WallpaperMode::Fit => ContentFit::Contain,
        };

        container(
            image(handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(content_fit),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .class(theme::Container::custom(wallpaper_fallback_style))
        .into()
    }
}

// Resolve a surface while iced guarantees the native handles are alive. Keep
// the borrowed-display connection alive until the effects binding takes over.
pub(super) fn native_wayland_surface(
    window: &dyn cosmic::iced::window::Window,
) -> Result<(Connection, wl_surface::WlSurface), String> {
    use cosmic::iced::window::raw_window_handle::{RawDisplayHandle, RawWindowHandle};
    let display = window.display_handle().map_err(|error| error.to_string())?;
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(handle)) = (display.as_raw(), handle.as_raw())
    else {
        return Err("shell material requires a Wayland surface".to_owned());
    };
    // SAFETY: iced lends matching, live Wayland handles for this callback.
    // This backend borrows the display; it never disconnects iced's connection.
    // Both handles belong to the runtime, which outlives the shell bindings.
    let backend = unsafe { wayland_client::backend::Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let connection = Connection::from_backend(backend);
    // SAFETY: raw-window-handle identifies this pointer as a live wl_surface.
    let id = unsafe {
        wayland_client::backend::ObjectId::from_ptr(wl_surface::WlSurface::interface(), handle.surface.as_ptr().cast())
    }
    .map_err(|error| error.to_string())?;
    let surface = wl_surface::WlSurface::from_id(&connection, id).map_err(|error| error.to_string())?;
    Ok((connection, surface))
}

pub(super) fn wallpaper_fallback_style(_theme: &cosmic::Theme) -> container::Style {
    let [red, green, blue] = config::default_background();

    container::Style {
        background: Some(Background::Color(Color::from_rgb8(red, green, blue))),
        ..container::Style::default()
    }
}

fn encode_regions(regions: &[[f32; 5]]) -> Vec<u8> {
    regions
        .iter()
        .flat_map(|region| region.iter().flat_map(|value| value.to_ne_bytes()))
        .collect()
}

fn validate_region_counts(regions: usize, opacities: usize) -> Result<(), std::io::Error> {
    let limit = ferese_protocols::effects::v1::MAX_REGIONS;
    if regions > limit || opacities > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("effects presentation exceeds the {limit}-region limit"),
        ));
    }
    Ok(())
}

type EffectPresentation = (Vec<[f32; 5]>, u32, Vec<u32>, ferese_surface_effects_v1::Role);

pub(super) struct EffectsBinding {
    connection: Connection,
    _manager: FereseEffectsManagerV1,
    pub(super) surface: FereseSurfaceEffectsV1,
    _queue: EventQueue<EffectsState>,
    regions: std::cell::RefCell<Option<Vec<[f32; 5]>>>,
    opacity: std::cell::Cell<Option<u32>>,
    presentation: std::cell::RefCell<Option<EffectPresentation>>,
}

impl EffectsBinding {
    pub(super) fn attach(surface: &wl_surface::WlSurface, visible: bool) -> Result<Self, Box<dyn std::error::Error>> {
        Self::attach_role(
            surface,
            visible.then_some(ferese_surface_effects_v1::Role::Panel),
            if visible { 1.0 } else { 0.0 },
        )
    }

    pub(super) fn attach_role(
        surface: &wl_surface::WlSurface,
        role: Option<ferese_surface_effects_v1::Role>,
        opacity: f32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let backend = surface
            .backend()
            .upgrade()
            .ok_or("libcosmic Wayland connection is no longer alive")?;
        let connection = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<EffectsState>(&connection)?;
        let qh = queue.handle();
        let manager = globals.bind::<FereseEffectsManagerV1, _, _>(&qh, 5..=5, ())?;
        let effects = manager.get_surface_effects(surface, &qh, ());

        let opacity = (opacity.clamp(0.0, 1.0) * 1000.0).round() as u32;
        effects.set_opacity(opacity);
        if let Some(role) = role {
            effects.set_role(role);
        } else {
            effects.clear_role();
        }
        connection.flush()?;

        Ok(Self {
            connection,
            _manager: manager,
            surface: effects,
            _queue: queue,
            regions: Default::default(),
            opacity: std::cell::Cell::new(Some(opacity)),
            presentation: Default::default(),
        })
    }

    pub(super) fn set_presentation(
        &self,
        regions: &[[f32; 5]],
        opacity: f32,
        region_opacities: impl IntoIterator<Item = f32>,
        role: ferese_surface_effects_v1::Role,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let opacity = (opacity.clamp(0.0, 1.0) * 1000.0).round() as u32;
        let values: Vec<u32> = region_opacities
            .into_iter()
            .map(|value| (value.clamp(0.0, 1.0) * 1000.0).round() as u32)
            .collect();
        validate_region_counts(regions.len(), values.len())?;
        let next = (regions.to_vec(), opacity, values, role);
        if self.presentation.borrow().as_ref() != Some(&next) {
            self.surface.set_presentation(
                role,
                encode_regions(regions),
                opacity,
                next.2.iter().flat_map(|value| value.to_ne_bytes()).collect(),
            );
            self.connection.flush()?;
            *self.presentation.borrow_mut() = Some(next);
        }
        Ok(())
    }

    pub(super) fn set_regions(&self, regions: &[[f32; 5]]) -> Result<(), Box<dyn std::error::Error>> {
        self.set_material_regions(regions, ferese_surface_effects_v1::Role::Popover)
    }

    pub(super) fn set_material_regions(
        &self,
        regions: &[[f32; 5]],
        role: ferese_surface_effects_v1::Role,
    ) -> Result<(), Box<dyn std::error::Error>> {
        validate_region_counts(regions.len(), 0)?;
        if self.regions.borrow().as_deref() == Some(regions) {
            return Ok(());
        }
        self.surface.set_regions(encode_regions(regions));
        if regions.is_empty() {
            self.surface.clear_role();
        } else {
            self.surface.set_role(role);
        }
        self.connection.flush()?;
        *self.regions.borrow_mut() = Some(regions.to_vec());
        Ok(())
    }

    pub(super) fn set_visible(&self, visible: bool) -> Result<(), Box<dyn std::error::Error>> {
        self.set_opacity(1.0)?;
        if visible {
            self.surface.set_role(ferese_surface_effects_v1::Role::Panel);
        } else {
            self.surface.clear_role();
        }

        self.connection.flush()?;
        Ok(())
    }

    pub(super) fn set_opacity(&self, opacity: f32) -> Result<(), Box<dyn std::error::Error>> {
        let opacity = (opacity.clamp(0.0, 1.0) * 1000.0).round() as u32;
        if self.opacity.get() != Some(opacity) {
            self.surface.set_opacity(opacity);
            self.connection.flush()?;
            self.opacity.set(Some(opacity));
        }
        Ok(())
    }
}

struct EffectsState;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for EffectsState {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(EffectsState: ignore FereseEffectsManagerV1);
delegate_noop!(EffectsState: ignore FereseSurfaceEffectsV1);

#[cfg(test)]
mod region_encoding_tests {
    #[test]
    fn oversized_presentations_are_rejected_without_truncation() {
        let limit = ferese_protocols::effects::v1::MAX_REGIONS;
        assert!(super::validate_region_counts(0, 0).is_ok());
        assert!(super::validate_region_counts(limit, limit).is_ok());
        assert!(super::validate_region_counts(limit + 1, limit).is_err());
        assert!(super::validate_region_counts(limit, limit + 1).is_err());
    }

    #[test]
    fn fractional_region_encoding_preserves_values() {
        let regions = [[0.25, -0.75, 100.5, 40.25, 14.0]];
        let bytes = super::encode_regions(&regions);
        let decoded: Vec<_> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&v| f32::from_ne_bytes(v))
            .collect();
        assert_eq!(decoded, regions[0]);
    }
}
