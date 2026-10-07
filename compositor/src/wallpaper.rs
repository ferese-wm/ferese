//! One wallpaper texture per GPU context. Decoded pixels are staging data,
//! released after upload and decoded again when a new context needs them.
//! Resize changes sampling only.
use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{ErasedContextId, ImportMem, Renderer};
use smithay::output::Output;
use smithay::utils::{Buffer, Physical, Rectangle, Size};

use crate::presentation::NativeTextureElement;

mod decoder;
mod pixels;
use decoder::Decoder;
use pixels::Pixels;
use std::sync::Arc;

const UPLOAD_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct WallpaperConfig {
    #[serde(default = "default_wallpaper_path")]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub mode: WallpaperMode,
}

fn default_wallpaper_path() -> Option<PathBuf> {
    Some(PathBuf::from(ferese_config::default_wallpaper()))
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            path: default_wallpaper_path(),
            mode: WallpaperMode::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WallpaperMode {
    #[default]
    Fill,
    Fit,
}

struct WallpaperTexture {
    texture: GlesTexture,
    id: Id,
    size: Size<i32, Buffer>,
}

pub(crate) struct WallpaperState {
    config: WallpaperConfig,
    pending: Option<WallpaperConfig>,
    loading: Option<WallpaperConfig>,
    decoder: Arc<Decoder>,
    commit: CommitCounter,
    owned: bool,
    mode: WallpaperMode,

    receiver: Option<mpsc::Receiver<Result<Pixels, String>>>,

    pixels: Option<Pixels>,

    textures: HashMap<ErasedContextId, WallpaperTexture>,
    upload_retries: HashMap<ErasedContextId, Instant>,

    // Prevent a missing/corrupt source from being decoded on every frame when
    // a new renderer context needs the wallpaper.
    decode_failed: bool,

    wakeup: Option<smithay::reexports::calloop::LoopSignal>,
    retry_timer_pending: Rc<Cell<bool>>,
    retry_wakeup: Rc<Cell<bool>>,
}

impl WallpaperState {
    #[cfg(test)]
    pub fn new(config: WallpaperConfig) -> Self {
        Self::with_wakeup(config, None)
    }

    pub fn with_wakeup(config: WallpaperConfig, wakeup: Option<smithay::reexports::calloop::LoopSignal>) -> Self {
        Self::with_decoder(config, wakeup, Decoder::new(pixels::load))
    }

    pub(crate) fn replacement(&self, config: WallpaperConfig) -> Self {
        Self::with_decoder(config, self.wakeup.clone(), self.decoder.clone())
    }

    fn with_decoder(
        config: WallpaperConfig,
        wakeup: Option<smithay::reexports::calloop::LoopSignal>,
        decoder: Arc<Decoder>,
    ) -> Self {
        let retained = config.clone();
        let owned = config.path.as_ref().is_some_and(|path| path.is_file());
        let decode_failed = config.path.is_some() && !owned;

        let receiver = if owned {
            Some(decoder.submit(config.path.clone().unwrap(), wakeup.clone()))
        } else {
            decoder.cancel_pending();
            None
        };

        Self {
            config: retained,
            pending: None,
            loading: None,
            decoder,
            commit: CommitCounter::default(),
            owned,
            mode: config.mode,
            receiver,
            pixels: None,
            textures: HashMap::new(),
            upload_retries: HashMap::new(),
            decode_failed,
            wakeup,
            retry_timer_pending: Rc::default(),
            retry_wakeup: Rc::default(),
        }
    }

    fn ensure_pixels(&mut self) {
        if self.pixels.is_some() || self.receiver.is_some() || self.decode_failed {
            return;
        }

        let Some(path) = self.config.path.clone() else {
            return;
        };

        self.receiver = Some(self.decoder.submit(path, self.wakeup.clone()))
    }

    pub(crate) fn configuration(&self) -> &WallpaperConfig {
        &self.config
    }

    pub(crate) fn failed(&self) -> bool {
        self.config.path.is_some()
            && self.receiver.is_none()
            && self.pixels.is_none()
            && self.textures.is_empty()
            && self.decode_failed
    }
    #[cfg(test)]
    pub fn owns_background(&self) -> bool {
        self.owned
    }

    pub fn reload(&mut self, config: WallpaperConfig) {
        if self.config.path != config.path && config.path.as_ref().is_some_and(|path| !path.is_file()) {
            tracing::warn!("new wallpaper is unavailable; retaining previous image");
            return;
        }

        if self.receiver.is_some() {
            if self.loading.is_some() && self.config == config {
                self.receiver = None;
                self.loading = None;
                self.pending = None;
                self.decoder.cancel_pending();
                return;
            }

            let requested = self.loading.as_ref().unwrap_or(&self.config);
            self.pending = (requested != &config).then_some(config);
            return;
        }
        if self.config == config {
            return;
        }
        if self.config.path == config.path {
            self.mode = config.mode;
            self.config = config;
            self.commit.increment();
            return;
        }
        if let Some(path) = config.path.clone() {
            self.receiver = Some(self.decoder.submit(path, self.wakeup.clone()));
            self.loading = Some(config);
        } else {
            self.decoder.cancel_pending();
            self.config = config;
            self.mode = self.config.mode;
            self.owned = false;
            self.pixels = None;
            self.textures.clear();
            self.upload_retries.clear();
            self.decode_failed = false;
            self.commit.increment();
        }
    }

    pub fn forget_context(&mut self, context: &ErasedContextId) {
        self.textures.remove(context);
        self.upload_retries.remove(context);
    }

    fn upload_ready(&self, context: &ErasedContextId, now: Instant) -> bool {
        self.upload_retries.get(context).is_none_or(|retry| now >= *retry)
    }

    pub(crate) fn take_retry_wakeup(&self) -> bool {
        self.retry_wakeup.replace(false) && !self.upload_retries.is_empty()
    }

    fn next_retry_deadline(&self, now: Instant) -> Option<Instant> {
        self.upload_retries.values().copied().min().map(|deadline| {
            // A suspended/hidden output may not have attempted an upload when
            // the timer fired. Avoid immediately rescheduling an expired timer.
            if deadline <= now {
                now + UPLOAD_RETRY_DELAY
            } else {
                deadline
            }
        })
    }

    pub(crate) fn arm_retry_timer<Data: 'static>(
        &self,
        handle: &smithay::reexports::calloop::LoopHandle<'static, Data>,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
        if self.retry_timer_pending.get() {
            return Ok(());
        }
        let Some(deadline) = self.next_retry_deadline(now) else {
            return Ok(());
        };
        let pending = self.retry_timer_pending.clone();
        let wakeup = self.retry_wakeup.clone();
        handle.insert_source(Timer::from_deadline(deadline), move |_, _, _| {
            pending.set(false);
            wakeup.set(true);
            TimeoutAction::Drop
        })?;
        self.retry_timer_pending.set(true);
        Ok(())
    }

    pub fn poll(&mut self) -> bool {
        let Some(receiver) = &self.receiver else {
            return false;
        };

        match receiver.try_recv() {
            Ok(result) => {
                self.receiver = None;

                let loading = self.loading.take();
                let replacing_wallpaper = loading.is_some();

                match result {
                    Ok(pixels) => {
                        if let Some(config) = loading {
                            self.config = config;
                            self.mode = self.config.mode;
                            self.owned = true;
                        }

                        tracing::info!(
                            width = pixels.width(),
                            height = pixels.height(),
                            bytes = pixels.as_raw().len(),
                            "decoded compositor wallpaper for texture upload"
                        );

                        self.pixels = Some(pixels);
                        self.decode_failed = false;

                        // A decode for a new wallpaper invalidates textures from
                        // the old image. A lazy re-decode for another context does
                        // not: those existing textures are still valid.
                        if replacing_wallpaper {
                            self.textures.clear();
                        }

                        self.upload_retries.clear();
                        self.commit.increment();
                    }
                    Err(error) => {
                        // A failed replacement does not poison the currently
                        // accepted wallpaper. Only failure to decode the accepted
                        // source itself should suppress repeated lazy re-decodes.
                        if !replacing_wallpaper {
                            self.decode_failed = true;
                        }

                        tracing::warn!(%error, "wallpaper decode failed; retaining previous image")
                    }
                }

                if let Some(pending) = self.pending.take() {
                    self.reload(pending);
                }

                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.receiver = None;
                self.loading = None;
                false
            }
            Err(mpsc::TryRecvError::Empty) => false,
        }
    }

    pub fn element(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Option<NativeTextureElement> {
        let context = renderer.context_id().erased();

        if !self.textures.contains_key(&context) {
            if !self.upload_ready(&context, Instant::now()) {
                return None;
            }

            if self.pixels.is_none() {
                self.ensure_pixels();
                return None;
            }

            let (size, bytes, texture) = {
                let pixels = self.pixels.as_ref()?;
                let size = Size::from((pixels.width() as i32, pixels.height() as i32));
                let bytes = pixels.as_raw().len();

                let texture = renderer.import_memory(pixels.as_raw(), Fourcc::Abgr8888, size, false);

                (size, bytes, texture)
            };

            match texture {
                Ok(texture) => {
                    self.upload_retries.remove(&context);

                    self.textures.insert(
                        context.clone(),
                        WallpaperTexture {
                            texture,
                            id: Id::new(),
                            size,
                        },
                    );

                    // import_memory() has produced a renderer-owned texture.
                    // The decoded RGBA mapping is only staging and no longer
                    // needs to stay resident.
                    self.pixels = None;

                    tracing::debug!(
                        bytes,
                        "uploaded shared wallpaper texture and released CPU staging pixels"
                    );
                }
                Err(error) => {
                    // Keep pixels alive on failure so the retry does not require
                    // another decode.
                    self.upload_retries.insert(context, Instant::now() + UPLOAD_RETRY_DELAY);

                    tracing::debug!(%error, "wallpaper texture import failed");

                    return None;
                }
            }
        }

        let cached = self.textures.get(&context)?;
        let output_size = output.current_transform().transform_size(output.current_mode()?.size);

        let (geometry, source) = image_geometry(cached.size, output_size, self.mode);

        Some(NativeTextureElement {
            id: cached.id.clone(),
            commit: self.commit,
            texture: cached.texture.clone(),
            geometry,
            source,
            alpha: 1.0,
            program: None,
            uniforms: vec![],
        })
    }
}

fn image_geometry(
    image: Size<i32, Buffer>,
    output: Size<i32, Physical>,
    mode: WallpaperMode,
) -> (Rectangle<i32, Physical>, Rectangle<f64, Buffer>) {
    let sx = f64::from(output.w) / f64::from(image.w);
    let sy = f64::from(output.h) / f64::from(image.h);
    match mode {
        WallpaperMode::Fill => {
            let scale = sx.max(sy);
            let width = f64::from(output.w) / scale;
            let height = f64::from(output.h) / scale;
            (
                Rectangle::from_size(output),
                Rectangle::new(
                    ((f64::from(image.w) - width) / 2.0, (f64::from(image.h) - height) / 2.0).into(),
                    (width, height).into(),
                ),
            )
        }
        WallpaperMode::Fit => {
            let scale = sx.min(sy);
            let size: Size<i32, Physical> = (
                (f64::from(image.w) * scale).round() as i32,
                (f64::from(image.h) * scale).round() as i32,
            )
                .into();
            (
                Rectangle::new(((output.w - size.w) / 2, (output.h - size.h) / 2).into(), size),
                Rectangle::from_size(image.to_f64()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_reload_preserves_accepted_config_and_unavailable_pending_keeps_success() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = WallpaperState::new(WallpaperConfig {
            path: None,
            mode: WallpaperMode::Fill,
        });
        let accepted = state.config.clone();
        state.pixels = Some(pixels::from_rgba(image::RgbaImage::new(2, 2)).unwrap());
        let bad = directory.path().join("bad-image");
        std::fs::write(&bad, b"not an image").unwrap();
        state.reload(WallpaperConfig {
            path: Some(bad),
            mode: WallpaperMode::Fit,
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !state.poll() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(state.config, accepted);
        assert_eq!(state.pixels.as_ref().unwrap().dimensions(), (2, 2));
        let (sender, receiver) = mpsc::channel();
        state.receiver = Some(receiver);
        state.reload(WallpaperConfig {
            path: Some(directory.path().join("missing")),
            mode: WallpaperMode::Fit,
        });
        sender
            .send(Ok(pixels::from_rgba(image::RgbaImage::new(3, 4)).unwrap()))
            .unwrap();
        assert!(state.poll());
        assert_eq!(state.pixels.as_ref().unwrap().dimensions(), (3, 4));
        assert_eq!(state.config, accepted);

        let replacement = state.replacement(accepted);
        assert!(Arc::ptr_eq(&state.decoder, &replacement.decoder));
    }

    #[test]
    fn portal_wallpaper_decodes_without_a_filename_extension() {
        let directory = tempfile::tempdir().unwrap();
        let expected = image::RgbaImage::from_pixel(3, 2, image::Rgba([32, 80, 160, 255]));
        for name in ["wallpaper-portal", "wallpaper.jpg"] {
            let path = directory.path().join(name);
            expected.save_with_format(&path, image::ImageFormat::Png).unwrap();
            let mut state = WallpaperState::new(WallpaperConfig {
                path: Some(path),
                mode: WallpaperMode::Fill,
            });
            let decoded = state
                .receiver
                .take()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(decoded.dimensions(), expected.dimensions());
            assert_eq!(&decoded.as_raw()[..], expected.as_raw());
        }
    }

    #[test]
    fn failed_upload_retry_wakes_idle_loop_and_does_not_spin_when_output_is_inactive() {
        use smithay::backend::renderer::ContextId;
        use smithay::reexports::calloop::EventLoop;
        let mut event_loop = EventLoop::<()>::try_new().unwrap();
        let mut state = WallpaperState::new(WallpaperConfig {
            path: None,
            mode: WallpaperMode::Fill,
        });
        let context = ContextId::<GlesTexture>::new().erased();
        let now = Instant::now();
        state
            .upload_retries
            .insert(context.clone(), now + Duration::from_millis(10));
        state.arm_retry_timer(&event_loop.handle(), now).unwrap();
        state.arm_retry_timer(&event_loop.handle(), now).unwrap();
        assert!(state.retry_timer_pending.get());
        assert!(!state.take_retry_wakeup());
        event_loop.dispatch(Some(Duration::from_secs(1)), &mut ()).unwrap();
        assert!(!state.retry_timer_pending.get());
        assert!(
            state.take_retry_wakeup(),
            "retry must request redraw without input or client activity"
        );
        assert!(!state.take_retry_wakeup());
        let after = Instant::now();
        assert_eq!(state.next_retry_deadline(after), Some(after + UPLOAD_RETRY_DELAY));
        state.forget_context(&context);
        assert_eq!(state.next_retry_deadline(after), None);
        state.retry_wakeup.set(true);
        assert!(
            !state.take_retry_wakeup(),
            "cleared failures must not trigger stale redraws"
        );
    }

    #[test]
    fn decoder_wakes_an_idle_event_loop() {
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::calloop::timer::Timer;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallpaper.png");
        let expected = image::RgbaImage::from_pixel(2, 2, image::Rgba([32, 80, 160, 255]));
        expected.save(&path).unwrap();
        let mut event_loop = EventLoop::<WallpaperState>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(Timer::from_duration(Duration::from_secs(5)), |_, _, _| {
                panic!("wallpaper decoder failed to wake the idle event loop");
            })
            .unwrap();
        let signal = event_loop.get_signal();
        let mut state = WallpaperState::with_wakeup(
            WallpaperConfig {
                path: Some(path),
                mode: WallpaperMode::Fill,
            },
            Some(signal.clone()),
        );
        event_loop
            .run(None, &mut state, |state| {
                if state.poll() {
                    let decoded = state.pixels.as_ref().unwrap();
                    assert_eq!(decoded.dimensions(), expected.dimensions());
                    assert_eq!(&decoded.as_raw()[..], expected.as_raw());
                    signal.stop();
                }
            })
            .unwrap();
    }

    #[test]
    fn failed_uploads_back_off_per_context_and_reset_for_new_pixels() {
        use smithay::backend::renderer::ContextId;
        let context = ContextId::<GlesTexture>::new().erased();
        let other = ContextId::<GlesTexture>::new().erased();
        let mut state = WallpaperState::new(WallpaperConfig {
            path: None,
            mode: WallpaperMode::Fill,
        });
        let now = Instant::now();
        state.upload_retries.insert(context.clone(), now + UPLOAD_RETRY_DELAY);
        assert!(!state.upload_ready(&context, now));
        assert!(state.upload_ready(&other, now));
        assert!(state.upload_ready(&context, now + UPLOAD_RETRY_DELAY));
        state.forget_context(&context);
        assert!(state.upload_ready(&context, now));

        state.upload_retries.insert(context.clone(), now + UPLOAD_RETRY_DELAY);
        let (sender, receiver) = mpsc::channel();
        state.receiver = Some(receiver);
        sender
            .send(Ok(pixels::from_rgba(image::RgbaImage::new(2, 2)).unwrap()))
            .unwrap();
        assert!(state.poll());
        assert!(state.upload_ready(&context, now));
    }
    #[test]
    fn live_reload_coalesces_decoders_and_mode_changes_invalidate_damage() {
        let mut state = WallpaperState::new(WallpaperConfig::default());
        let (sender, receiver) = mpsc::channel();
        state.receiver = Some(receiver);
        state.reload(WallpaperConfig {
            path: state.config.path.clone(),
            mode: WallpaperMode::Fit,
        });
        assert!(state.receiver.is_some());
        let before = state.commit;
        sender
            .send(Ok(pixels::from_rgba(image::RgbaImage::new(2, 2)).unwrap()))
            .unwrap();
        assert!(state.poll());
        assert_eq!(state.mode, WallpaperMode::Fit);
        assert!(state.pixels.is_some());
        assert_ne!(state.commit, before);
        assert!(state.receiver.is_none());
        let before = state.commit;
        state.reload(state.config.clone());
        assert_eq!(state.commit, before);
    }

    #[test]
    fn reverting_a_pending_reload_keeps_latest_request_and_failed_decode_keeps_pixels() {
        let mut state = WallpaperState::new(WallpaperConfig::default());
        state.pixels = Some(pixels::from_rgba(image::RgbaImage::new(2, 2)).unwrap());
        let (sender, receiver) = mpsc::channel();
        state.receiver = Some(receiver);
        state.reload(WallpaperConfig {
            path: state.config.path.clone(),
            mode: WallpaperMode::Fit,
        });
        state.reload(WallpaperConfig::default());
        assert!(state.pending.is_none());
        sender.send(Err("bad image".into())).unwrap();
        assert!(state.poll());
        assert!(state.pixels.is_some());
        assert_eq!(state.mode, WallpaperMode::Fill);
    }

    #[test]
    fn fill_crops_and_fit_letterboxes_without_reallocating_image() {
        let (geometry, source) = image_geometry((3840, 2160).into(), (1600, 1000).into(), WallpaperMode::Fill);
        assert_eq!(geometry.size, (1600, 1000).into());
        assert_eq!(source.size.h, 2160.0);
        assert!(source.loc.x > 0.0);
        let (geometry, source) = image_geometry((3840, 2160).into(), (1600, 1000).into(), WallpaperMode::Fit);
        assert_eq!(geometry.size, (1600, 900).into());
        assert_eq!(geometry.loc, (0, 50).into());
        assert_eq!(source.size, (3840.0, 2160.0).into());
    }

    #[test]
    fn missing_wallpaper_preserves_external_shell_fallback() {
        let state = WallpaperState::new(WallpaperConfig {
            path: None,
            mode: WallpaperMode::default(),
        });
        assert!(!state.owns_background());
    }

    #[test]
    fn default_wallpaper_is_bundled_and_decodable() {
        let config: WallpaperConfig = ferese_config::from_str("").unwrap();
        let path = config.path.unwrap();
        assert_eq!(image::image_dimensions(&path).unwrap(), (3840, 2160));
        assert!(WallpaperState::new(WallpaperConfig::default()).owns_background());
    }

    #[test]

    fn released_staging_is_not_a_failed_wallpaper() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallpaper.png");

        image::RgbaImage::from_pixel(2, 2, image::Rgba([32, 80, 160, 255]))
            .save(&path)
            .unwrap();

        let mut state = WallpaperState::new(WallpaperConfig {
            path: Some(path),
            mode: WallpaperMode::Fill,
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while !state.poll() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(state.pixels.is_some());
        assert!(!state.failed());

        // Model successful upload releasing CPU staging.
        state.pixels = None;

        // Missing staging is normal after upload, not a decode failure.
        assert!(!state.failed());
    }

    #[test]

    fn missing_staging_is_lazily_redecoded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallpaper.png");

        image::RgbaImage::from_pixel(3, 2, image::Rgba([32, 80, 160, 255]))
            .save(&path)
            .unwrap();

        let mut state = WallpaperState::new(WallpaperConfig {
            path: Some(path),
            mode: WallpaperMode::Fill,
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while !state.poll() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }

        state.pixels = None;
        assert!(state.receiver.is_none());

        state.ensure_pixels();

        assert!(state.receiver.is_some());

        let deadline = Instant::now() + Duration::from_secs(2);
        while !state.poll() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(state.pixels.as_ref().unwrap().dimensions(), (3, 2));
    }

    #[test]
    fn failed_lazy_decode_is_not_restarted_every_frame() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallpaper");
        std::fs::write(&path, b"not an image").unwrap();

        let mut state = WallpaperState::new(WallpaperConfig {
            path: Some(path),
            mode: WallpaperMode::Fill,
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while !state.poll() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(state.decode_failed);
        assert!(state.failed());

        state.ensure_pixels();

        assert!(
            state.receiver.is_none(),
            "failed source must not restart a decode every frame"
        );
    }
}
