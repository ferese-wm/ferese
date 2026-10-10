use cosmic::app::Task;
use cosmic::widget::{button, column, row};
use ferese_config::theme::Appearance;

use super::{Alignment, App, Element, Field, Kind, Length, Message, Page, schema, visuals, widget};
use crate::store::{Edit, Snapshot, set};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Target {
    #[default]
    Both,
    Light,
    Dark,
}

impl Target {
    const ALL: [Self; 3] = [Self::Both, Self::Light, Self::Dark];

    fn prefix(self) -> &'static str {
        match self {
            Self::Both => "theme.background",
            Self::Light => "theme.light.background",
            Self::Dark => "theme.dark.background",
        }
    }

    fn appearance(self, active: Appearance) -> Appearance {
        match self {
            Self::Both => active,
            Self::Light => Appearance::Light,
            Self::Dark => Appearance::Dark,
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Both => "Both modes",
            Self::Light => "Light mode",
            Self::Dark => "Dark mode",
        })
    }
}

#[derive(Default)]
pub(super) struct State {
    pub target: Target,
    active: Preview,
    defaults: [Preview; 2],
}

#[derive(Default)]
struct Preview {
    path: String,
    requested: Option<String>,
    handle: Option<widget::image::Handle>,
    error: Option<String>,
}

impl Preview {
    fn request(&mut self, path: &str) -> bool {
        if path.is_empty() {
            self.path.clear();
            self.requested = None;
            self.handle = None;
            self.error = None;
            return false;
        }

        if self.requested.is_some() || self.path == path {
            return false;
        }

        // Keep the last decoded image while its replacement loads. A stale
        // completion can then also return to that image without decoding again.
        self.error = None;
        self.requested = Some(path.to_owned());
        true
    }

    fn finish(&mut self, path: String, desired: &str, result: Result<widget::image::Handle, String>) {
        if self.requested.as_deref() != Some(&path) {
            return;
        }

        self.requested = None;
        if path != desired {
            return;
        }

        self.path = path;
        match result {
            Ok(handle) => {
                self.handle = Some(handle);
                self.error = None;
            }
            Err(error) => {
                self.handle = None;
                self.error = Some(error);
            }
        }
    }
}

fn index(appearance: Appearance) -> usize {
    usize::from(appearance == Appearance::Dark)
}

pub(super) fn edits(target: Target, token: &str, value: impl Into<serde_json::Value>) -> Vec<Edit> {
    let mut edits = vec![set(&format!("{}.{token}", target.prefix()), value)];
    if target == Target::Both {
        for kind in ["light", "dark"] {
            edits.push(Edit::Unset(format!("theme.{kind}.background.{token}")));
        }
    }

    edits
}

pub(super) fn restore_defaults() -> Vec<Edit> {
    [Target::Both, Target::Light, Target::Dark]
        .map(|target| Edit::Unset(format!("{}.path", target.prefix())))
        .into()
}

fn setting(snapshot: &Snapshot, target: Target, token: &str, fallback: &str) -> String {
    snapshot
        .item(&format!("{}.{token}", target.prefix()))
        .or_else(|| snapshot.item(&format!("theme.background.{token}")))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

fn image_button_style(palette: visuals::Palette, selected: bool) -> cosmic::theme::Button {
    let paint = move |hovered: bool, focused: bool| button::Style {
        shape: Some(cosmic::iced::border::Shape::Continuous),
        background: None,
        border_radius: palette.radius.min(10.).into(),
        border_width: if selected || focused {
            2.
        } else if hovered {
            1.
        } else {
            0.
        },
        border_color: palette.accent,
        ..Default::default()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |focused, _| paint(false, focused)),
        hovered: Box::new(move |focused, _| paint(true, focused)),
        pressed: Box::new(move |focused, _| paint(true, focused)),
        disabled: Box::new(move |_| paint(false, false)),
    }
}

fn thumbnail(path: String, builtin: Option<Appearance>) -> Task<Message> {
    cosmic::task::future(async move {
        let (send, receive) = cosmic::iced::futures::channel::oneshot::channel();
        let file = path.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<_, String> {
                let mut reader = image::ImageReader::open(&file)
                    .map_err(|e| e.to_string())?
                    .with_guessed_format()
                    .map_err(|e| e.to_string())?;
                let mut limits = image::Limits::default();
                limits.max_alloc = Some(256 * 1024 * 1024);
                limits.max_image_width = Some(16384);
                limits.max_image_height = Some(16384);
                reader.limits(limits);
                let (width, height) = if builtin.is_some() { (400, 225) } else { (1000, 500) };
                let pixels = reader
                    .decode()
                    .map_err(|e| e.to_string())?
                    .thumbnail(width, height)
                    .into_rgba8();
                Ok(widget::image::Handle::from_rgba(
                    pixels.width(),
                    pixels.height(),
                    pixels.into_raw(),
                ))
            })();
            let _ = send.send(result);
        });
        let result = receive
            .await
            .unwrap_or_else(|_| Err("Could not load wallpaper preview.".into()));
        match builtin {
            Some(appearance) => Message::DefaultWallpaperThumbnail(appearance, result),
            None => Message::Thumbnail(path, result),
        }
    })
}

impl App {
    pub(super) fn apply_wallpaper_edits(&mut self, edits: Vec<Edit>) -> Task<Message> {
        for edit in &edits {
            if let Edit::Set(path, _) | Edit::Unset(path) = edit {
                self.inputs.remove(path);
            }
        }

        self.edit_many(edits)
    }

    pub(super) fn active_wallpaper_path(&self) -> String {
        if self.path == crate::store::config_path() {
            return self
                .resolved
                .presented
                .tokens
                .background
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
        }

        // A separate config preview must not borrow the live desktop's path.
        let appearance = match self.current.string("theme.mode", "auto").as_str() {
            "light" => Appearance::Light,
            "dark" => Appearance::Dark,
            _ => self.resolved.theme.appearance,
        };
        let target = if appearance == Appearance::Light {
            Target::Light
        } else {
            Target::Dark
        };
        let path = setting(
            &self.current,
            target,
            "path",
            ferese_config::default_wallpaper_for(appearance),
        );
        if path.is_empty() {
            return path;
        }

        ferese_config::theme::theme_path(self.path.parent().unwrap_or(std::path::Path::new(".")), path.as_ref())
            .to_string_lossy()
            .into_owned()
    }

    pub(super) fn load_thumbnail(&mut self) -> Task<Message> {
        if self.page != Page::Wallpaper {
            return Task::none();
        }

        let mut tasks = Vec::with_capacity(3);
        let path = self.active_wallpaper_path();
        if self.wallpaper.active.request(&path) {
            tasks.push(thumbnail(path, None));
        }

        for appearance in [Appearance::Light, Appearance::Dark] {
            let path = ferese_config::default_wallpaper_for(appearance);
            if self.wallpaper.defaults[index(appearance)].request(path) {
                tasks.push(thumbnail(path.into(), Some(appearance)));
            }
        }

        Task::batch(tasks)
    }

    pub(super) fn wallpaper_thumbnail(&mut self, path: String, result: Result<widget::image::Handle, String>) {
        let desired = self.active_wallpaper_path();
        self.wallpaper.active.finish(path, &desired, result);
    }

    pub(super) fn default_wallpaper_thumbnail(
        &mut self,
        appearance: Appearance,
        result: Result<widget::image::Handle, String>,
    ) {
        let path = ferese_config::default_wallpaper_for(appearance);
        self.wallpaper.defaults[index(appearance)].finish(path.into(), path, result);
    }

    pub(super) fn wallpaper_fields(&self) -> Vec<Field> {
        let active = if self.path == crate::store::config_path() {
            self.resolved.presented.appearance
        } else {
            match self.current.string("theme.mode", "auto").as_str() {
                "light" => Appearance::Light,
                "dark" => Appearance::Dark,
                _ => self.resolved.theme.appearance,
            }
        };
        schema::fields(Page::Wallpaper)
            .into_iter()
            .map(|mut field| {
                field.path = field.path.replace("theme.background", self.wallpaper.target.prefix());
                if let Kind::Text { default, .. } = &mut field.kind {
                    *default = ferese_config::default_wallpaper_for(self.wallpaper.target.appearance(active));
                }

                field
            })
            .collect()
    }

    pub(super) fn wallpaper_field_value(&self, path: &str, fallback: &str) -> String {
        if path.starts_with(self.wallpaper.target.prefix()) {
            return setting(
                &self.draft,
                self.wallpaper.target,
                path.rsplit('.').next().unwrap(),
                fallback,
            );
        }

        self.draft.string(path, fallback)
    }

    pub(super) fn wallpaper_controls(&self) -> Element<'_, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        let preview = &self.wallpaper.active;
        let mut body = column([]).spacing(8).push(self.label("Current wallpaper", 13.));
        let current: Element<'_, Message> = if let Some(handle) = &preview.handle {
            widget::image(handle.clone())
                .border_radius(palette.radius.min(10.))
                .shape(cosmic::iced::border::Shape::Continuous)
                .width(Length::Shrink)
                .height(180)
                .content_fit(cosmic::iced::ContentFit::Contain)
                .into()
        } else {
            self.wallpaper_placeholder(
                if preview.requested.is_some() {
                    "Loading preview…"
                } else {
                    preview.error.as_deref().unwrap_or("No wallpaper image selected.")
                },
                palette,
                180.,
            )
        };
        body = body.push(widget::container(current).height(180).center_x(Length::Fill));

        body = body.push(
            row([])
                .spacing(8)
                .align_y(Alignment::Center)
                .push(self.label("Apply to", 12.))
                .push(ferese_theme::controls::select(
                    cosmic::iced::widget::pick_list(Target::ALL, Some(self.wallpaper.target), Message::WallpaperTarget)
                        .font(self.font)
                        .text_size(12),
                    palette,
                ))
                .push(widget::Space::new().width(Length::Fill))
                .push(self.settings_button(
                    "Choose image…",
                    "M3 4h18v16H3z M3 15l5-5 5 5 3-3 5 5 M15 8h.01",
                    Some(Message::PickWallpaper),
                    false,
                )),
        );
        let mut defaults = row([]).spacing(8);
        let active = self.active_wallpaper_path();
        for appearance in [Appearance::Light, Appearance::Dark] {
            let name = if appearance == Appearance::Light {
                "Light"
            } else {
                "Dark"
            };
            let preview = &self.wallpaper.defaults[index(appearance)];
            let image: Element<'_, Message> = if let Some(handle) = &preview.handle {
                widget::image(handle.clone())
                    .border_radius(palette.radius.min(10.))
                    .shape(cosmic::iced::border::Shape::Continuous)
                    .width(Length::Fill)
                    .height(110)
                    .content_fit(cosmic::iced::ContentFit::Cover)
                    .into()
            } else {
                self.wallpaper_placeholder(preview.error.as_deref().unwrap_or("Loading preview…"), palette, 110.)
            };
            let label = widget::container(self.label(name, 12.).class(cosmic::theme::Text::Color(palette.text)))
                .padding([7, 10])
                .width(Length::Fill);
            let tile = column([]).push(image).push(label);

            defaults = defaults.push(
                button::custom(tile)
                    .name(format!("Use default {name} wallpaper"))
                    .padding(0)
                    .width(Length::Fill)
                    .class(image_button_style(
                        palette,
                        active == ferese_config::default_wallpaper_for(appearance),
                    ))
                    .on_press(Message::UseDefaultWallpaper(appearance)),
            );
        }

        body.push(
            row([])
                .spacing(8)
                .align_y(Alignment::Center)
                .push(self.label("Default wallpapers", 13.))
                .push(widget::Space::new().width(Length::Fill))
                .push(self.settings_button(
                    "Match appearance",
                    "M20 7v5h-5 M4 17v-5h5 M6 7a7 7 0 0 1 12-2l2 2 M18 17a7 7 0 0 1-12 2l-2-2",
                    Some(Message::RestoreDefaultWallpapers),
                    false,
                )),
        )
        .push(defaults)
        .into()
    }

    fn wallpaper_placeholder<'a>(
        &'a self,
        message: &'a str,
        palette: visuals::Palette,
        height: f32,
    ) -> Element<'a, Message> {
        widget::container(
            column([])
                .spacing(8)
                .align_x(Alignment::Center)
                .push(visuals::action_icon(
                    "M3 4h18v16H3z M3 15l5-5 5 5 3-3 5 5 M15 8h.01",
                    palette.muted,
                ))
                .push(self.note(message)),
        )
        .padding(16)
        .width(Length::Fill)
        .height(height)
        .center_x(Length::Fill)
        .center_y(height)
        .class(visuals::surface(palette.card, palette.radius.min(10.)))
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_slots_keep_their_size_while_loading_and_on_failure() {
        use cosmic::Application;
        use cosmic::iced::advanced::renderer::Headless;
        use cosmic::iced::advanced::{layout, widget::Tree};
        use cosmic::iced::{Font, Pixels, Size};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.),
                Some("tiny-skia"),
            ))
            .unwrap();

        for width in [420., 760.] {
            let mut app = App::init(
                cosmic::app::Core::default(),
                ("/preview/config.kdl".into(), Snapshot::parse(String::new()), None),
            )
            .0;
            let dimensions = |app: &App| {
                let mut view = app.wallpaper_controls();
                let mut tree = Tree::new(view.as_widget());
                view.as_widget_mut()
                    .layout(
                        &mut tree,
                        &renderer,
                        &layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY)),
                    )
                    .size()
            };
            let initial = dimensions(&app);
            assert!(app.wallpaper.active.request("/active.png"));

            for preview in &mut app.wallpaper.defaults {
                assert!(preview.request("/default.png"));
            }

            assert_eq!(dimensions(&app), initial);
            let handle = widget::image::Handle::from_rgba(2, 1, vec![255; 8]);
            app.wallpaper
                .active
                .finish("/active.png".into(), "/active.png", Ok(handle.clone()));

            for index in 0..app.wallpaper.defaults.len() {
                app.wallpaper.defaults[index].finish("/default.png".into(), "/default.png", Ok(handle.clone()));
                assert_eq!(dimensions(&app), initial);
            }

            assert!(app.wallpaper.active.request("/missing.png"));
            assert_eq!(dimensions(&app), initial);
            app.wallpaper.active.finish(
                "/missing.png".into(),
                "/missing.png",
                Err("Could not load this image.".into()),
            );
            assert_eq!(dimensions(&app), initial);
            assert!(!app.wallpaper.active.request(""));
            assert_eq!(dimensions(&app), initial);
        }
    }

    #[test]
    #[ignore = "release UI preview; requires headless renderer backends"]
    fn render_preview_pages_on_both_backends() {
        use cosmic::Application;
        use cosmic::iced::advanced::renderer::{Headless, Renderer as _};
        use cosmic::iced::advanced::{Layout, layout, mouse, renderer, widget::Tree};
        use cosmic::iced::{Color, Font, Pixels, Rectangle, Size};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut app = App::init(
            cosmic::app::Core::default(),
            ("/preview/config.kdl".into(), Snapshot::parse(String::new()), None),
        )
        .0;
        for appearance in [Appearance::Light, Appearance::Dark] {
            let path = ferese_config::default_wallpaper_for(appearance);
            let pixels = image::open(path).unwrap().thumbnail(400, 225).into_rgba8();
            let handle = widget::image::Handle::from_rgba(pixels.width(), pixels.height(), pixels.into_raw());
            assert!(app.wallpaper.defaults[index(appearance)].request(path));
            app.default_wallpaper_thumbnail(appearance, Ok(handle));
        }

        for backend in ["wgpu", "tiny-skia"] {
            let mut renderer = runtime
                .block_on(<cosmic::Renderer as Headless>::new(
                    Font::default(),
                    Pixels(14.),
                    Some(backend),
                ))
                .expect("requested renderer");
            for appearance in [Appearance::Light, Appearance::Dark] {
                let name = if appearance == Appearance::Light {
                    "light"
                } else {
                    "dark"
                };
                app.current = Snapshot::parse(format!("theme {{ mode \"{name}\"; }}")).unwrap();
                app.draft = app.current.clone();
                app.visible_rows = (0..app.resolved.families.len().div_ceil(3))
                    .map(|row| ("theme", row))
                    .collect();
                let pixels = image::open(ferese_config::default_wallpaper_for(appearance))
                    .unwrap()
                    .thumbnail(1000, 500)
                    .into_rgba8();
                app.wallpaper.active.handle = Some(widget::image::Handle::from_rgba(
                    pixels.width(),
                    pixels.height(),
                    pixels.into_raw(),
                ));
                for page in [Page::Wallpaper, Page::Appearance] {
                    app.page = page;
                    let bounds = Rectangle::with_size(Size::new(1000., 780.));
                    let theme = app.native_palette.native_theme();
                    let mut view = app.page_view();
                    let mut tree = Tree::new(view.as_widget());
                    let node = view.as_widget_mut().layout(
                        &mut tree,
                        &renderer,
                        &layout::Limits::new(Size::ZERO, bounds.size()),
                    );
                    renderer.reset(bounds);
                    view.as_widget().draw(
                        &tree,
                        &mut renderer,
                        &theme,
                        &renderer::Style {
                            text_color: Color::WHITE,
                            icon_color: Color::WHITE,
                            scale_factor: 1.25,
                        },
                        Layout::new(&node),
                        mouse::Cursor::Unavailable,
                        &bounds,
                    );
                    let pixels = Headless::screenshot(
                        &mut renderer,
                        Size::new(1250, 975),
                        1.25,
                        theme.cosmic().bg_color().into(),
                    );
                    let preview = if page == Page::Wallpaper { "wallpaper" } else { "theme" };
                    let path = format!("/tmp/ferese-{preview}-{backend}-{name}.png");
                    image::save_buffer(&path, &pixels, 1250, 975, image::ColorType::Rgba8).unwrap();
                    eprintln!("Preview page: {path}");
                }
            }
        }
    }

    #[test]
    fn selecting_both_removes_mode_overrides_without_changing_lock_wallpaper() {
        let mut snapshot = Snapshot::parse(
            "theme { background { lock-path \"/lock.png\"; }; light { background { path \"/light.png\"; }; }; dark { background { path \"/dark.png\"; }; }; }".into(),
        ).unwrap();
        for edit in edits(Target::Both, "path", "/shared.png") {
            snapshot.edit(&edit).unwrap();
        }

        assert_eq!(snapshot.string("theme.background.path", ""), "/shared.png");
        assert!(snapshot.item("theme.light.background.path").is_none());
        assert!(snapshot.item("theme.dark.background.path").is_none());
        assert_eq!(snapshot.string("theme.background.lock_path", ""), "/lock.png");
    }

    #[test]
    fn selecting_one_mode_preserves_the_other_and_inherits_shared_settings() {
        let mut snapshot =
            Snapshot::parse("theme { background { path \"/shared.png\"; mode \"fit\"; }; }".into()).unwrap();
        for edit in edits(Target::Light, "path", "/light.png") {
            snapshot.edit(&edit).unwrap();
        }

        assert_eq!(setting(&snapshot, Target::Light, "path", ""), "/light.png");
        assert_eq!(setting(&snapshot, Target::Dark, "path", ""), "/shared.png");
        assert_eq!(setting(&snapshot, Target::Light, "mode", "fill"), "fit");
    }

    #[test]
    fn restoring_defaults_keeps_placement_and_lock_image() {
        let mut snapshot = Snapshot::parse("theme { background { path \"/custom.png\"; lock-path \"/lock.png\"; mode \"fit\"; }; light { background { path \"/light.png\"; }; }; }".into()).unwrap();
        for edit in restore_defaults() {
            snapshot.edit(&edit).unwrap();
        }

        for target in Target::ALL {
            assert!(snapshot.item(&format!("{}.path", target.prefix())).is_none());
        }

        assert_eq!(snapshot.string("theme.background.mode", ""), "fit");
        assert_eq!(snapshot.string("theme.background.lock_path", ""), "/lock.png");
    }

    #[test]
    fn returning_to_a_previous_path_during_a_load_reuses_its_preview() {
        let mut preview = Preview::default();
        let image = widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]);
        assert!(preview.request("/first.png"));
        preview.finish("/first.png".into(), "/first.png", Ok(image.clone()));
        assert!(preview.request("/second.png"));
        preview.finish("/second.png".into(), "/first.png", Ok(image));
        assert!(!preview.request("/first.png"));
        assert!(preview.handle.is_some());
        assert_eq!(preview.path, "/first.png");
    }

    #[test]
    fn replacement_keeps_the_previous_image_until_it_is_ready() {
        let mut preview = Preview::default();
        let first = widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]);
        let second = widget::image::Handle::from_rgba(1, 1, vec![255, 255, 255, 255]);
        assert!(preview.request("/first.png"));
        preview.finish("/first.png".into(), "/first.png", Ok(first.clone()));
        assert!(preview.request("/second.png"));
        assert_eq!(preview.handle.as_ref().unwrap().id(), first.id());
        assert_eq!(preview.path, "/first.png");
        assert!(!preview.request("/second.png"));
        preview.finish("/second.png".into(), "/second.png", Ok(second.clone()));
        assert_eq!(preview.handle.as_ref().unwrap().id(), second.id());
        assert_eq!(preview.path, "/second.png");
    }

    #[test]
    fn clearing_wallpaper_during_a_load_does_not_restore_the_old_image() {
        let mut preview = Preview::default();
        let image = widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]);
        assert!(preview.request("/first.png"));
        preview.finish("/first.png".into(), "/first.png", Ok(image.clone()));
        assert!(preview.request("/second.png"));
        assert!(!preview.request(""));
        assert!(preview.handle.is_none());
        assert!(preview.requested.is_none());
        preview.finish("/second.png".into(), "", Ok(image));
        assert!(preview.handle.is_none());
        assert!(preview.path.is_empty());
    }

    #[test]
    fn stale_preview_completion_does_not_replace_the_active_wallpaper() {
        let mut preview = Preview::default();
        assert!(preview.request("/old.png"));
        let image = widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]);
        preview.finish("/old.png".into(), "/new.png", Ok(image.clone()));
        assert!(preview.handle.is_none());
        assert!(preview.request("/new.png"));
        preview.finish("/old.png".into(), "/new.png", Ok(image.clone()));
        assert_eq!(preview.requested.as_deref(), Some("/new.png"));
        preview.finish("/new.png".into(), "/new.png", Ok(image));
        assert!(preview.handle.is_some());
        assert!(!preview.request("/new.png"));
    }
}
