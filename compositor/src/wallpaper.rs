//! One decoded image and one texture per GPU context, not double-buffered
//! full-screen UI surfaces per monitor. Resize changes sampling only.
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

mod pixels;
use pixels::Pixels;

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
}
pub(crate) struct WallpaperState {
    config: WallpaperConfig,
    pending: Option<WallpaperConfig>,
    commit: CommitCounter,
    owned: bool,
    mode: WallpaperMode,
    receiver: Option<mpsc::Receiver<Result<Pixels, String>>>,
    pixels: Option<Pixels>,
    textures: HashMap<ErasedContextId, WallpaperTexture>,
    upload_retries: HashMap<ErasedContextId, Instant>,
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
        let retained = config.clone();
        let (sender, receiver) = mpsc::channel();
        let owned = config.path.as_ref().is_some_and(|path| path.is_file());
        if owned {
            let path = config.path.unwrap();
            let wakeup = wakeup.clone();
            std::thread::spawn(move || {
                let _ = sender.send(pixels::load(&path));
                if let Some(wakeup) = wakeup {
                    wakeup.wakeup();
                }
            });
        }
        Self {
            config: retained,
            pending: None,
            commit: CommitCounter::default(),
            owned,
            mode: config.mode,
            receiver: owned.then_some(receiver),
            pixels: None,
            textures: HashMap::new(),
            upload_retries: HashMap::new(),
            wakeup,
            retry_timer_pending: Rc::default(),
            retry_wakeup: Rc::default(),
        }
    }

    pub(crate) fn configuration(&self) -> &WallpaperConfig {
        &self.config
    }

    pub(crate) fn failed(&self) -> bool {
        self.config.path.is_some() && self.receiver.is_none() && self.pixels.is_none()
    }

    #[cfg(test)]
    pub fn owns_background(&self) -> bool {
        self.owned
    }

    pub fn reload(&mut self, config: WallpaperConfig) {
        if self.receiver.is_some() {
            self.pending = (self.config != config).then_some(config);
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
        if config.path.as_ref().is_some_and(|path| !path.is_file()) {
            tracing::warn!("new wallpaper is unavailable; retaining previous image");
            return;
        }
        let mut replacement = Self::with_wakeup(config, self.wakeup.clone());
        self.config = replacement.config;
        self.mode = replacement.mode;
        self.owned = replacement.owned;
        self.receiver = replacement.receiver.take();
        if self.config.path.is_none() {
            self.pixels = None;
            self.textures.clear();
            self.upload_retries.clear();
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
                if let Some(pending) = self.pending.take() {
                    if pending.path != self.config.path {
                        self.reload(pending);
                        return true;
                    }
                    self.config = pending;
                    self.mode = self.config.mode;
                }
                match result {
                    Ok(pixels) => {
                        tracing::info!(
                            width = pixels.width(),
                            height = pixels.height(),
                            bytes = pixels.as_raw().len(),
                            "decoded compositor wallpaper once"
                        );
                        self.pixels = Some(pixels);
                        self.textures.clear();
                        self.upload_retries.clear();
                        self.commit.increment();
                    }
                    Err(error) => {
                        tracing::warn!(%error, "wallpaper unavailable; using output clear color")
                    }
                }
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.receiver = None;
                false
            }
            Err(mpsc::TryRecvError::Empty) => false,
        }
    }

    pub fn element(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Option<NativeTextureElement> {
        let pixels = self.pixels.as_ref()?;
        let context = renderer.context_id().erased();
        if !self.textures.contains_key(&context) {
            if !self.upload_ready(&context, Instant::now()) {
                return None;
            }
            let size = Size::from((pixels.width() as i32, pixels.height() as i32));
            match renderer.import_memory(pixels.as_raw(), Fourcc::Abgr8888, size, false) {
                Ok(texture) => {
                    self.upload_retries.remove(&context);
                    tracing::debug!(bytes = pixels.as_raw().len(), "uploaded shared wallpaper texture");
                    self.textures
                        .insert(context.clone(), WallpaperTexture { texture, id: Id::new() });
                }
                Err(error) => {
                    // Avoid retrying a large failed allocation every frame, but
                    // allow recovery from temporary GPU memory pressure.
                    self.upload_retries.insert(context, Instant::now() + UPLOAD_RETRY_DELAY);
                    tracing::debug!(%error, "wallpaper texture import failed");
                    return None;
                }
            }
        }
        let cached = self.textures.get(&context)?;
        let output_size = output.current_transform().transform_size(output.current_mode()?.size);
        let (geometry, source) = image_geometry(
            (pixels.width() as i32, pixels.height() as i32).into(),
            output_size,
            self.mode,
        );
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
        let mut event_loop = EventLoop::<WallpaperState>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(Timer::from_duration(Duration::from_secs(5)), |_, _, _| {
                panic!("wallpaper decoder failed to wake the idle event loop");
            })
            .unwrap();
        let signal = event_loop.get_signal();
        let mut state = WallpaperState::with_wakeup(WallpaperConfig::default(), Some(signal.clone()));
        event_loop
            .run(None, &mut state, |state| {
                if state.poll() {
                    assert!(state.pixels.is_some());
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
}
