use cosmic::iced::Color;
use cosmic::widget::image;
use ferese_config::Document;

#[derive(Clone)]
pub struct Appearance {
    pub appearance: ferese_config::theme::Appearance,
    pub high_contrast: bool,
    pub dim: f32,
    pub show_clock: bool,
    pub show_date: bool,
    pub twelve_hour: bool,
    pub panel: Color,
    pub text: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub radius: f32,
    pub font: cosmic::font::Font,
    pub wallpaper: image::Handle,
    pub wallpaper_path: std::path::PathBuf,
    pub wallpaper_blur: f32,
    pub wallpaper_revision: u64,
    wallpaper_loaded: bool,
    wallpaper_pending: Option<(std::path::PathBuf, bool)>,
    pub reduce_transparency: bool,
    pub avatar: Option<image::Handle>,
}

impl Appearance {
    pub fn load(user: &str, path: Option<&std::path::Path>) -> Self {
        let doc = path
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|source| Document::parse(&source).ok());
        let mut theme = ferese_theme_client::service::current().presented;
        if let Some(path) = path.filter(|path| ferese_config::config_path().as_deref() != Some(*path))
            && let Some(document) = &doc
            && let Ok(mut connection) = ferese_ipc::theme::Connection::connect()
        {
            let args = serde_json::json!({
                "source": document.to_string(),
                "directory": path.parent(),
            });
            if let Ok(value) = connection.call("theme-preview", args)
                && let Ok(preview) = serde_json::from_value(value["theme"].clone())
            {
                theme = preview;
            }
        }
        Self::from_document(user, doc, &theme)
    }

    fn from_document(user: &str, doc: Option<Document>, theme: &ferese_config::theme::ResolvedTheme) -> Self {
        let doc = doc.map(|doc| doc.with_theme(theme));
        let string = |key: &str, fallback: &str| {
            doc.as_ref()
                .and_then(|d| d.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or(fallback)
                .to_owned()
        };
        let palette = ferese_theme::Palette::from_resolved(theme);
        let font = string("theme.typography.font_family", "Inter");
        let background = string("theme.background.path", ferese_config::default_wallpaper());
        let path = string("theme.background.lock_path", &background);
        let path = if let Some(tail) = path.strip_prefix("~/") {
            std::env::var("HOME")
                .map(|home| format!("{home}/{tail}"))
                .unwrap_or(path)
        } else {
            path
        };
        let wallpaper_path = std::path::PathBuf::from(path);
        let blur = doc
            .as_ref()
            .and_then(|doc| doc.get("lock_screen.background_blur"))
            .and_then(|value| value.as_f64())
            .unwrap_or(18.)
            .clamp(0., 40.) as f32;
        let reduce_transparency = theme.accessibility.reduce_transparency;
        let loaded = load_wallpaper(&wallpaper_path, if reduce_transparency { 0. } else { blur });
        let wallpaper_loaded = loaded.is_ok();
        let wallpaper = loaded.unwrap_or_else(|_| {
            image::Handle::from_bytes(include_bytes!("../../../assets/wallpapers/ferese.png").as_slice())
        });
        let boolean = |key, fallback| {
            doc.as_ref()
                .and_then(|d| d.get(key))
                .and_then(|v| v.as_bool())
                .unwrap_or(fallback)
        };
        Self {
            appearance: theme.appearance,
            high_contrast: theme.accessibility.increase_contrast,
            dim: doc
                .as_ref()
                .and_then(|d| d.get("lock_screen.background_dim"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.48)
                .clamp(0., 0.9) as f32,
            show_clock: boolean("lock_screen.show_clock", true),
            show_date: boolean("lock_screen.show_date", true),
            twelve_hour: string("lock_screen.clock_format", "24h") == "12h",
            panel: palette.sidebar,
            text: palette.text,
            accent: palette.accent,
            on_accent: palette.on_accent,
            radius: palette.radius,
            font: ferese_theme::font(Some(&font)),
            wallpaper,
            wallpaper_path,
            wallpaper_blur: blur,
            wallpaper_revision: 0,
            wallpaper_loaded,
            wallpaper_pending: None,
            reduce_transparency,
            avatar: account_picture(user),
        }
    }

    pub fn request_wallpaper(
        &mut self,
        theme: &ferese_config::theme::ResolvedTheme,
    ) -> Option<(u64, std::path::PathBuf, f32)> {
        let path = theme
            .tokens
            .background
            .lock_path
            .as_ref()
            .or(theme.tokens.background.path.as_ref())
            .cloned()
            .unwrap_or_else(|| ferese_config::default_wallpaper_for(theme.appearance).into());
        let reduce = theme.accessibility.reduce_transparency;
        let desired = (path.clone(), reduce);
        if self.wallpaper_pending.as_ref() == Some(&desired) {
            return None;
        }
        if self.wallpaper_loaded && path == self.wallpaper_path && reduce == self.reduce_transparency {
            if self.wallpaper_pending.take().is_some() {
                self.wallpaper_revision = self.wallpaper_revision.wrapping_add(1);
            }
            return None;
        }
        self.wallpaper_pending = Some(desired);
        self.wallpaper_revision = self.wallpaper_revision.wrapping_add(1);
        Some((
            self.wallpaper_revision,
            path,
            if reduce { 0.0 } else { self.wallpaper_blur },
        ))
    }

    pub fn finish_wallpaper(&mut self, revision: u64, result: Result<image::Handle, String>) -> Result<(), String> {
        if revision != self.wallpaper_revision {
            return Ok(());
        }
        let Some((path, reduce)) = self.wallpaper_pending.take() else {
            return Ok(());
        };
        let wallpaper = result?;
        self.wallpaper = wallpaper;
        self.wallpaper_path = path;
        self.reduce_transparency = reduce;
        self.wallpaper_loaded = true;
        Ok(())
    }

    pub fn clock_format(&self) -> &'static str {
        if self.twelve_hour { "%I:%M %p" } else { "%H:%M" }
    }

    pub fn theme(&self) -> cosmic::Theme {
        ferese_theme::Palette {
            appearance: self.appearance,
            high_contrast: self.high_contrast,
            background: self.panel,
            sidebar: self.panel,
            card: self.panel,
            text: self.text,
            muted: self.text.scale_alpha(0.55),
            accent: self.accent,
            accent_gradient: None,
            on_accent: self.on_accent,
            radius: self.radius,
            error: Color::from_rgb8(235, 98, 98),
        }
        .native_theme()
    }
}

fn account_picture(user: &str) -> Option<image::Handle> {
    let mut candidates = vec![std::path::PathBuf::from("/var/lib/AccountsService/icons").join(user)];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(std::path::PathBuf::from(home).join(".face"));
    }
    candidates.into_iter().find_map(|path| {
        let pixels = decode_avatar(&path).ok()?;
        let side = pixels.width().min(pixels.height());
        if side == 0 {
            return None;
        }
        let square = ::image::imageops::crop_imm(
            &pixels,
            (pixels.width() - side) / 2,
            (pixels.height() - side) / 2,
            side,
            side,
        )
        .to_image();
        let mut pixels = ::image::imageops::resize(&square, 256, 256, ::image::imageops::FilterType::Lanczos3);
        // Clip the decoded thumbnail, so the avatar stays circular even on
        // renderers that do not support rounded image clipping.
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            let distance = ((x as f32 + 0.5 - 128.).powi(2) + (y as f32 + 0.5 - 128.).powi(2)).sqrt();
            let coverage = (128. - distance).clamp(0., 1.);
            pixel[3] = (pixel[3] as f32 * coverage).round() as u8;
        }
        Some(image::Handle::from_rgba(
            pixels.width(),
            pixels.height(),
            pixels.into_raw(),
        ))
    })
}

fn decode_avatar(path: &std::path::Path) -> Result<::image::RgbaImage, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    use ::image::ImageDecoder;

    const ENCODED_LIMIT: u64 = 8 * 1024 * 1024;
    const DECODED_LIMIT: u64 = 32 * 1024 * 1024;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| error.to_string())?;
    if !file.metadata().map_err(|error| error.to_string())?.is_file() {
        return Err("Avatar is not a regular file".into());
    }

    let mut bytes = Vec::new();
    file.take(ENCODED_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > ENCODED_LIMIT {
        return Err("Avatar exceeds the file limit".into());
    }

    let mut reader = ::image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(DECODED_LIMIT);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) * 4 > DECODED_LIMIT {
        return Err("Avatar exceeds the decode limit".into());
    }

    ::image::DynamicImage::from_decoder(decoder)
        .map(|image| image.into_rgba8())
        .map_err(|error| error.to_string())
}

pub fn load_wallpaper(path: &std::path::Path, blur: f32) -> Result<image::Handle, String> {
    let result = prepare_wallpaper(path, blur);
    // Decode and resize scratch buffers have been dropped. Return their freed
    // pages even when glibc would otherwise retain them in an arena.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
    result
}

fn prepare_wallpaper(path: &std::path::Path, blur: f32) -> Result<image::Handle, String> {
    use ::image::ImageDecoder;
    let mut reader = ::image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = ::image::Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > (256 * 1024 * 1024) / 4 {
        return Err("Lock wallpaper exceeds the decode limit".into());
    }
    if blur <= 0. {
        // Iced decodes the path and releases its host buffer after GPU upload.
        // An RGBA handle would retain those pixels for the handle's lifetime.
        return Ok(image::Handle::from_path(path));
    }
    let pixels = {
        let decoded = ::image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
        let thumbnail = decoded.thumbnail(640, 640).into_rgba8();
        ::image::imageops::blur(&thumbnail, blur)
    };
    Ok(image::Handle::from_rgba(
        pixels.width(),
        pixels.height(),
        pixels.into_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wallpaper_handles_keep_only_paths_or_blurred_thumbnails() {
        let file = tempfile::NamedTempFile::new().unwrap();
        ::image::RgbaImage::from_pixel(1280, 720, ::image::Rgba([32, 80, 160, 255]))
            .save_with_format(file.path(), ::image::ImageFormat::Png)
            .unwrap();
        match load_wallpaper(file.path(), 0.).unwrap() {
            image::Handle::Path(_, path) => assert_eq!(path, file.path()),
            _ => panic!("unblurred wallpaper must not retain RGBA pixels"),
        }
        match load_wallpaper(file.path(), 2.).unwrap() {
            image::Handle::Rgba {
                width, height, pixels, ..
            } => {
                assert_eq!((width, height), (640, 360));
                assert_eq!(pixels.len(), 640 * 360 * 4);
            }
            _ => panic!("blurred wallpaper must retain only the processed thumbnail"),
        }
    }

    #[test]
    fn wallpaper_path_handles_require_a_valid_image_header() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(load_wallpaper(file.path(), 0.).is_err());
        std::fs::write(file.path(), b"not an image").unwrap();
        assert!(load_wallpaper(file.path(), 0.).is_err());
        assert!(load_wallpaper(&file.path().with_extension("missing"), 0.).is_err());
    }

    #[test]
    fn wallpaper_requests_retry_failures_and_ignore_superseded_results() {
        let directory = tempfile::tempdir().unwrap();
        let initial = directory.path().join("initial.png");
        ::image::RgbaImage::new(2, 2).save(&initial).unwrap();
        let mut theme = ferese_config::theme::default_theme();
        theme.tokens.background.path = Some(initial.clone());
        theme.tokens.background.lock_path = None;
        let document = Document::parse("").unwrap();
        let mut appearance = Appearance::from_document("no-such-test-user", Some(document), &theme);
        assert!(appearance.request_wallpaper(&theme).is_none());
        let next = directory.path().join("next.png");
        theme.tokens.background.path = Some(next.clone());
        let (failed, _, _) = appearance.request_wallpaper(&theme).unwrap();
        assert!(appearance.request_wallpaper(&theme).is_none());
        assert!(
            appearance
                .finish_wallpaper(failed, Err("decode failed".into()))
                .is_err()
        );
        assert_eq!(appearance.wallpaper_path, initial);
        let (retry, path, _) = appearance.request_wallpaper(&theme).unwrap();
        assert_eq!(path, next);
        let last = directory.path().join("last.png");
        theme.tokens.background.path = Some(last.clone());
        let (latest, _, _) = appearance.request_wallpaper(&theme).unwrap();
        appearance
            .finish_wallpaper(retry, Ok(image::Handle::from_rgba(1, 1, vec![255; 4])))
            .unwrap();
        assert_eq!(appearance.wallpaper_path, initial);
        appearance
            .finish_wallpaper(latest, Ok(image::Handle::from_rgba(1, 1, vec![255; 4])))
            .unwrap();
        assert_eq!(appearance.wallpaper_path, last);
        assert!(appearance.request_wallpaper(&theme).is_none());
        theme.tokens.background.lock_path = Some(initial.clone());
        let (_, path, _) = appearance.request_wallpaper(&theme).unwrap();
        assert_eq!(path, initial, "an explicit lock image overrides the desktop wallpaper");
    }

    #[test]
    fn avatar_decoding_bounds_dimensions_and_encoded_size() {
        let file = tempfile::NamedTempFile::new().unwrap();
        ::image::RgbaImage::new(3, 2)
            .save_with_format(file.path(), ::image::ImageFormat::Png)
            .unwrap();
        assert_eq!(decode_avatar(file.path()).unwrap().dimensions(), (3, 2));

        ::image::RgbaImage::new(4097, 1)
            .save_with_format(file.path(), ::image::ImageFormat::Png)
            .unwrap();
        assert!(decode_avatar(file.path()).is_err());

        file.as_file().set_len(8 * 1024 * 1024 + 1).unwrap();
        assert!(decode_avatar(file.path()).unwrap_err().contains("file limit"));
    }

    #[test]
    fn portal_wallpaper_loads_without_a_filename_extension() {
        let file = tempfile::NamedTempFile::new().unwrap();
        ::image::RgbaImage::from_pixel(3, 2, ::image::Rgba([32, 80, 160, 255]))
            .save_with_format(file.path(), ::image::ImageFormat::Png)
            .unwrap();
        assert!(load_wallpaper(file.path(), 0.).is_ok());
        assert!(load_wallpaper(file.path(), 2.).is_ok());
    }

    #[test]
    fn lock_options_share_shell_style_without_changing_security() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.kdl");
        std::fs::write(
            &config,
            r##"
            theme {
                geometry { shell-radius 0; }
                colors { accent "#FF5500"; }
            }
            lock-screen {
                show-clock #false
                show-date #false
                clock-format "12h"
                background-dim 0.7
            }
        "##,
        )
        .unwrap();
        let document = Document::parse(&std::fs::read_to_string(&config).unwrap()).unwrap();
        let theme = ferese_config::theme::resolve(&document, directory.path(), jiff::Timestamp::now(), |_| {
            Err("unexpected file".into())
        })
        .unwrap()
        .theme;
        let appearance = Appearance::from_document("no-such-test-user", Some(document), &theme);
        assert_eq!(appearance.radius, 0.);
        assert_eq!(theme.requested_accent, "#FF5500");
        assert_eq!(
            appearance.accent,
            ferese_theme::parse_color(&theme.tokens.colors.accent).unwrap()
        );
        assert!(ferese_theme::contrast(appearance.accent, appearance.on_accent) >= 4.5);
        assert!(!appearance.show_clock && !appearance.show_date);
        assert_eq!(appearance.clock_format(), "%I:%M %p");
        assert_eq!(appearance.dim, 0.7);
        assert_eq!(appearance.theme().cosmic().corner_radii.radius_m, [0.; 4]);
    }
}
