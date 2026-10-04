use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use zbus::Connection;
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::backend::{Cancel, Options, Request, authorize};
use crate::consent::Prompt;

const PATH: &str = "/org/freedesktop/portal/desktop";
const MAX_IMAGE_BYTES: u64 = 128 * 1024 * 1024;
type Reply = (u32, Options);

type RequestMap = HashMap<String, (String, Arc<Cancel>)>;

#[derive(Clone)]
pub(crate) struct Requests(Arc<Mutex<RequestMap>>, usize);

impl Default for Requests {
    fn default() -> Self {
        Self::with_limit(8)
    }
}

impl Requests {
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self(Default::default(), limit)
    }

    pub(crate) async fn revoke_stale(&self, owner: Option<&str>) {
        for (request_owner, cancel) in self.0.lock().await.values() {
            if Some(request_owner.as_str()) != owner {
                cancel.stop();
            }
        }
    }

    pub(crate) async fn begin(
        &self,
        connection: &Connection,
        header: &Header<'_>,
        handle: &OwnedObjectPath,
    ) -> zbus::fdo::Result<Arc<Cancel>> {
        let owner = authorize(connection, header).await?;
        if !handle.as_str().starts_with(&format!("{PATH}/request/")) {
            return Err(zbus::fdo::Error::InvalidArgs("Invalid request path".into()));
        }
        let mut pending = self.0.lock().await;
        if pending.len() >= self.1 || pending.contains_key(handle.as_str()) {
            return Err(zbus::fdo::Error::LimitsExceeded("Request limit reached".into()));
        }
        let cancel = Arc::new(Cancel::default());
        if !connection
            .object_server()
            .at(
                handle,
                Request {
                    owner: owner.clone(),
                    cancel: cancel.clone(),
                },
            )
            .await?
        {
            return Err(zbus::fdo::Error::InvalidArgs("Request already exists".into()));
        }
        pending.insert(handle.to_string(), (owner, cancel.clone()));
        Ok(cancel)
    }

    pub(crate) async fn end(&self, connection: &Connection, handle: &OwnedObjectPath) {
        self.0.lock().await.remove(handle.as_str());
        let _ = connection.object_server().remove::<Request, _>(handle).await;
    }
}

#[derive(Clone)]
pub(crate) struct Screenshot(pub(crate) Requests);

#[zbus::interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl Screenshot {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn available_targets(&self) -> u32 {
        1 | 2 | 4 | 8
    }

    async fn screenshot(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let cancel = self.0.begin(connection, &header, &handle).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.wait() => (1, Options::new()),
            result = screenshot(&app_id, &parent_window, &options) => reply(result),
        };
        self.0.end(connection, &handle).await;
        Ok(result)
    }

    async fn pick_color(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let cancel = self.0.begin(connection, &header, &handle).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.wait() => (1, Options::new()),
            result = pick_color(&app_id, &parent_window) => reply(result),
        };
        self.0.end(connection, &handle).await;
        Ok(result)
    }
}

#[derive(Clone)]
pub(crate) struct Wallpaper(pub(crate) Requests);

#[zbus::interface(name = "org.freedesktop.impl.portal.Wallpaper")]
impl Wallpaper {
    #[zbus(name = "SetWallpaperURI")]
    #[allow(
        clippy::too_many_arguments,
        reason = "Signature is defined by the portal D-Bus interface"
    )]
    async fn set_wallpaper_uri(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        uri: String,
        options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<u32> {
        let cancel = self.0.begin(connection, &header, &handle).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.wait() => 1,
            result = wallpaper(&app_id, &parent_window, &uri, &options, cancel.clone()) => reply(result).0,
        };
        self.0.end(connection, &handle).await;
        Ok(result)
    }
}

pub(crate) fn reply(result: Result<Option<Options>, String>) -> Reply {
    match result {
        Ok(Some(values)) => (0, values),
        Ok(None) => (1, Options::new()),
        Err(error) => {
            eprintln!("ferese portal: {error}");
            (2, Options::new())
        }
    }
}

pub(crate) async fn consent(
    app: &str,
    parent: &str,
    title: &str,
    description: &str,
    accept: &str,
    image: Option<PathBuf>,
) -> Result<bool, String> {
    if app.len() > 512 {
        return Err("Application ID is too long".into());
    }
    let prompt = Prompt {
        parent: parent.into(),
        title: title.into(),
        description: if app.is_empty() {
            description.into()
        } else {
            format!("{app}\n{description}")
        },
        accept: accept.into(),
        image,
        shortcuts: Vec::new(),
    };
    let mut child = crate::backend::child_command()?
        .arg("--consent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| error.to_string())?;
    let pid = child.id().ok_or("Missing consent process ID")?;
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(&serde_json::to_vec(&prompt).map_err(|error| error.to_string())?)
        .await
        .map_err(|error| error.to_string())?;
    drop(input);
    let output = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output())
        .await
        .map_err(|_| "Consent timed out")?
        .map_err(|error| error.to_string())?;
    if output.status.success() && output.stdout == b"true\n" {
        wait_for_surface_removal(pid).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub(crate) async fn ipc(command: &'static str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    tokio::task::spawn_blocking(move || ipc_sync(command, args))
        .await
        .map_err(|error| error.to_string())?
}

pub(crate) fn ipc_sync(command: &'static str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    let path = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or("Missing runtime directory")?
        .join("ferese/control.sock");
    let mut stream = UnixStream::connect(path).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| error.to_string())?;
    let request = ferese_ipc::Request {
        version: ferese_ipc::VERSION,
        id: 1,
        kind: "command".into(),
        command: command.into(),
        args,
    };
    ferese_ipc::write_frame(&mut stream, &request).map_err(|error| error.to_string())?;
    let response: ferese_ipc::Response = ferese_ipc::read_frame(&mut stream).map_err(|error| error.to_string())?;
    if let Some(error) = response.error {
        return Err(error.message);
    }
    response.result.ok_or("Missing IPC result".into())
}

async fn select_geometry(point: bool) -> Result<Option<String>, String> {
    let mut command = crate::backend::child_command_for("slurp")?;
    if point {
        command.arg("-p");
    }
    command.stdin(Stdio::null());
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| error.to_string())?;
    let pid = child.id().ok_or("Missing selection process ID")?;
    let output = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output())
        .await
        .map_err(|_| "Selection timed out")?
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Ok(None);
    }
    let geometry = String::from_utf8(output.stdout)
        .map_err(|error| error.to_string())?
        .trim()
        .to_owned();
    if geometry.len() > 128 || geometry.is_empty() {
        return Err("Invalid selection".into());
    }
    wait_for_surface_removal(pid).await?;
    Ok(Some(geometry))
}

pub(crate) async fn wait_for_surface_removal(pid: u32) -> Result<(), String> {
    wait_for_surface_removal_using(|| ipc("has-client-surfaces", serde_json::json!({"pid": pid}))).await
}

pub(crate) async fn wait_for_surface_removal_using<
    F: std::future::Future<Output = Result<serde_json::Value, String>>,
>(
    mut check: impl FnMut() -> F,
) -> Result<(), String> {
    for _ in 0..100 {
        if check().await? == false {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err("Screenshot helper is still visible".into())
}

async fn capture(geometry: Option<String>) -> Result<Vec<u8>, String> {
    capture_command("screenshot", serde_json::json!({"geometry": geometry})).await
}

async fn capture_command(command: &'static str, args: serde_json::Value) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || {
        let result = ipc_sync(command, args)?;
        let path = result["path"].as_str().ok_or("Missing screenshot path")?;
        read_screenshot(path)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn read_screenshot(path: &str) -> Result<Vec<u8>, String> {
    struct Cleanup<'a>(&'a str);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let _cleanup = Cleanup(path);
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("Screenshot is too large".into());
    }
    Ok(bytes)
}

async fn screenshot(app: &str, parent: &str, options: &Options) -> Result<Option<Options>, String> {
    let target = options
        .get("target")
        .map(u32::try_from)
        .transpose()
        .map_err(|_| "Invalid screenshot target")?
        .unwrap_or(
            if options
                .get("interactive")
                .map(bool::try_from)
                .transpose()
                .map_err(|_| "Invalid interactive hint")?
                .unwrap_or(false)
            {
                4
            } else {
                1
            },
        );
    if !matches!(target, 1 | 2 | 4 | 8) {
        return Err("Unsupported screenshot target".into());
    }
    let active_window = if target == 8 {
        Some(
            ipc("get-focused-window", serde_json::json!({})).await?["id"]
                .as_u64()
                .ok_or("No active window")?,
        )
    } else {
        None
    };
    if !consent(
        app,
        parent,
        "Take a screenshot?",
        "Allow this app to capture the selected screen content.",
        "Continue",
        None,
    )
    .await?
    {
        return Ok(None);
    }
    let bytes = match target {
        1 => capture(None).await?,
        4 => {
            let Some(geometry) = select_geometry(false).await? else {
                return Ok(None);
            };
            capture(Some(geometry)).await?
        }
        2 | 8 => {
            let id = match active_window {
                Some(id) => id,
                None => {
                    let Some(id) = select_window(app, parent).await? else {
                        return Ok(None);
                    };
                    id
                }
            };
            capture_command("screenshot-window", serde_json::json!({"window":id})).await?
        }
        _ => unreachable!(),
    };
    let directory = private_directory("screenshots")?;
    let mut file = tempfile::Builder::new()
        .prefix("screenshot-")
        .suffix(".png")
        .tempfile_in(directory)
        .map_err(|error| error.to_string())?;
    std::io::Write::write_all(&mut file, &bytes).map_err(|error| error.to_string())?;
    let (_, path) = file.keep().map_err(|error| error.to_string())?;
    Ok(Some(HashMap::from([(
        "uri".into(),
        Value::from(file_uri(&path)?)
            .try_to_owned()
            .map_err(|error| error.to_string())?,
    )])))
}

fn window_sources(windows: &serde_json::Value) -> Result<Vec<crate::capture::Source>, String> {
    let windows = windows.as_array().ok_or("Invalid window list")?;
    if windows.len() > 128 {
        return Err("Too many windows to choose from".into());
    }
    let mut sources = Vec::new();
    for window in windows.iter().filter(|window| window["mapped"] == true) {
        let Some(id) = window["id"].as_u64() else {
            continue;
        };
        let title = window["title"].as_str().unwrap_or("Untitled window");
        let app = window["app_id"].as_str().unwrap_or("");
        let label = format!("{title} — {app}")
            .chars()
            .filter(|c| !c.is_control())
            .take(256)
            .collect();
        sources.push(crate::capture::Source {
            name: id.to_string(),
            label,
            width: window["width"]
                .as_i64()
                .and_then(|w| i32::try_from(w).ok())
                .unwrap_or(0),
            height: window["height"]
                .as_i64()
                .and_then(|h| i32::try_from(h).ok())
                .unwrap_or(0),
            x: 0,
            y: 0,
            scale: 1,
        });
    }
    Ok(sources)
}

pub(crate) fn sharing_window_sources(windows: &serde_json::Value) -> Result<Vec<crate::capture::Source>, String> {
    let windows = windows.as_array().ok_or("Invalid window list")?;
    let eligible = windows
        .iter()
        .filter(|window| {
            window["workspace"].as_u64().is_some()
                && window["capture_width"].as_i64().is_some_and(|width| width > 0)
                && window["capture_height"].as_i64().is_some_and(|height| height > 0)
                && window["app_id"].as_str() != Some("dev.ferese.ScreenShare")
        })
        .take(128)
        .map(|window| {
            let mut window = window.clone();
            window["mapped"] = true.into();
            window["width"] = window["capture_width"].clone();
            window["height"] = window["capture_height"].clone();
            window
        })
        .collect::<Vec<_>>();
    let mut sources = window_sources(&serde_json::Value::Array(eligible))?;
    for source in &mut sources {
        source.name = format!("window:{}", source.name);
    }
    Ok(sources)
}

async fn select_window(app: &str, parent: &str) -> Result<Option<u64>, String> {
    let windows = ipc("get-windows", serde_json::json!({})).await?;
    let sources = window_sources(&windows)?;
    if sources.is_empty() {
        return Err("No windows are available".into());
    }
    let prompt = crate::picker::Prompt {
        app: app.into(),
        sources,
        multiple: false,
        parent: parent.into(),
        window_capture: true,
        persist_mode: 0,
        rememberable: Vec::new(),
    };
    let mut child = crate::backend::child_command()?
        .arg("--picker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| error.to_string())?;
    let pid = child.id().ok_or("Missing window selector PID")?;
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&serde_json::to_vec(&prompt).map_err(|error| error.to_string())?)
        .await
        .map_err(|error| error.to_string())?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output())
        .await
        .map_err(|_| "Window selection timed out")?
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Ok(None);
    }
    let names: Vec<String> = serde_json::from_slice(&output.stdout).map_err(|_| "Invalid window selection")?;
    if names.len() != 1 || !prompt.sources.iter().any(|source| source.name == names[0]) {
        return Err("Unknown selected window".into());
    }
    wait_for_surface_removal(pid).await?;
    names[0]
        .parse::<u64>()
        .map(Some)
        .map_err(|_| "Invalid window ID".into())
}

async fn pick_color(app: &str, parent: &str) -> Result<Option<Options>, String> {
    if !consent(
        app,
        parent,
        "Pick a screen color?",
        "Choose a pixel to share its color with this app.",
        "Choose color",
        None,
    )
    .await?
    {
        return Ok(None);
    }
    let Some(geometry) = select_geometry(true).await? else {
        return Ok(None);
    };
    let bytes = capture(Some(geometry)).await?;
    let color = png_color(&bytes)?;
    Ok(Some(HashMap::from([(
        "color".into(),
        Value::from(color).try_to_owned().map_err(|error| error.to_string())?,
    )])))
}

fn png_color(bytes: &[u8]) -> Result<(f64, f64, f64), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let size = reader
        .output_buffer_size()
        .filter(|size| *size <= 16 * 1024)
        .ok_or("Unexpected pixel dimensions")?;
    let mut pixels = vec![0; size];
    let info = reader.next_frame(&mut pixels).map_err(|error| error.to_string())?;
    if info.width == 0 || info.height == 0 {
        return Err("Empty image".into());
    }
    match info.color_type {
        png::ColorType::Rgb | png::ColorType::Rgba => Ok((
            pixels[0] as f64 / 255.,
            pixels[1] as f64 / 255.,
            pixels[2] as f64 / 255.,
        )),
        _ => Err("Unexpected screenshot pixel format".into()),
    }
}

fn private_directory(name: &str) -> Result<PathBuf, String> {
    let directory = dirs::data_local_dir()
        .ok_or("Missing data directory")?
        .join("ferese/portal")
        .join(name);
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
    Ok(directory)
}

fn file_uri(path: &Path) -> Result<String, String> {
    url::Url::from_file_path(path)
        .map(String::from)
        .map_err(|_| "Invalid file path".into())
}

fn local_path(uri: &str) -> Result<PathBuf, String> {
    url::Url::parse(uri)
        .map_err(|error| error.to_string())?
        .to_file_path()
        .map_err(|_| "Wallpaper must be a local file URI".into())
}

async fn wallpaper(
    app: &str,
    parent: &str,
    uri: &str,
    options: &Options,
    cancel: Arc<Cancel>,
) -> Result<Option<Options>, String> {
    let target = options
        .get("set-on")
        .map(<&str>::try_from)
        .transpose()
        .map_err(|_| "Invalid wallpaper target")?
        .unwrap_or("background");
    if !matches!(target, "background" | "lockscreen" | "both") {
        return Err("Unsupported wallpaper target".into());
    }
    let input = local_path(uri)?;
    let preparing = cancel.clone();
    let file = tokio::task::spawn_blocking(move || prepare_wallpaper(&input, &preparing))
        .await
        .map_err(|error| error.to_string())??;
    if !consent(
        app,
        parent,
        "Change your wallpaper?",
        "Use this picture as your wallpaper.",
        "Set wallpaper",
        Some(file.path().to_path_buf()),
    )
    .await?
    {
        return Ok(None);
    }
    if cancel.stopped.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let (_, path) = file.keep().map_err(|error| error.to_string())?;
    if let Err(error) = update_wallpaper(&path, target).await {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(Some(Options::new()))
}

fn prepare_wallpaper(input: &Path, cancel: &Cancel) -> Result<tempfile::NamedTempFile, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let input = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(input)
        .map_err(|error| error.to_string())?;
    if !input.metadata().map_err(|error| error.to_string())?.is_file() {
        return Err("Wallpaper must be a regular file".into());
    }
    let directory = private_directory("wallpapers")?;
    let mut file = tempfile::Builder::new()
        .prefix("wallpaper-")
        .tempfile_in(directory)
        .map_err(|error| error.to_string())?;
    let mut input = input.take(MAX_IMAGE_BYTES + 1);
    let mut size = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        if cancel.stopped.load(Ordering::SeqCst) {
            return Err("Wallpaper request cancelled".into());
        }
        let bytes = input.read(&mut buffer).map_err(|error| error.to_string())?;
        if bytes == 0 {
            break;
        }
        size += bytes as u64;
        if size > MAX_IMAGE_BYTES {
            return Err("Wallpaper is too large".into());
        }
        std::io::Write::write_all(&mut file, &buffer[..bytes]).map_err(|error| error.to_string())?;
    }
    if size == 0 {
        return Err("Wallpaper is empty".into());
    }
    let reader = image::ImageReader::open(file.path())
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let (width, height) = reader.into_dimensions().map_err(|error| error.to_string())?;
    if width == 0
        || height == 0
        || width > 16384
        || height > 16384
        || u64::from(width) * u64::from(height) > MAX_IMAGE_BYTES / 4
    {
        return Err("Wallpaper dimensions exceed the image limit".into());
    }
    let mut reader = image::ImageReader::open(file.path())
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_IMAGE_BYTES);
    reader.limits(limits);
    reader.decode().map_err(|error| error.to_string())?;
    Ok(file)
}

async fn update_wallpaper(image: &Path, target: &str) -> Result<(), String> {
    let value = crate::bridge::Bridge::connect()?
        .call("theme-get", serde_json::json!({}))
        .await?;
    let snapshot = ferese_ipc::theme::Snapshot::decode(value, ferese_config::families::builtins)?;
    let image = image.to_owned();
    let target = target.to_owned();
    tokio::task::spawn_blocking(move || {
        let _transaction = crate::backend::CONFIG_TRANSACTION
            .lock()
            .map_err(|_| "Configuration transaction failed")?;
        let path = ferese_config::config_path().ok_or("Missing config path")?;
        edit_wallpaper(&path, &image, &target, snapshot.theme.appearance)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn wallpaper_theme(
    document: &ferese_config::Document,
    directory: &Path,
    appearance: ferese_config::theme::Appearance,
) -> Result<ferese_config::theme::ResolvedTheme, String> {
    let mut document = document.clone();
    let mode = match appearance {
        ferese_config::theme::Appearance::Light => "light",
        ferese_config::theme::Appearance::Dark => "dark",
    };
    // The compositor knows the active appearance, including system overrides.
    // Resolve that variant without changing the user's automatic policy.
    document
        .set("theme.mode", mode.into())
        .map_err(|error| error.to_string())?;
    ferese_config::theme::resolve(&document, directory, jiff::Timestamp::now(), |file| {
        std::fs::read_to_string(file).map_err(|error| error.to_string())
    })
    .map(|candidate| candidate.theme)
}

fn edit_wallpaper(
    path: &Path,
    image: &Path,
    target: &str,
    appearance: ferese_config::theme::Appearance,
) -> Result<(), String> {
    use ferese_config::theme::Appearance;

    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.to_string()),
    };
    let mut document = ferese_config::Document::parse(&source).map_err(|error| error.to_string())?;
    let directory = path.parent().ok_or("Invalid config path")?;
    let before = wallpaper_theme(&document, directory, appearance)?.tokens.background;
    let (variant, other) = match appearance {
        Appearance::Light => ("light", Appearance::Dark),
        Appearance::Dark => ("dark", Appearance::Light),
    };
    let other_before = wallpaper_theme(&document, directory, other)?.tokens.background;
    let base = format!("theme.{variant}.background");
    if target == "background" && before.lock_path.is_none() {
        let old = before
            .path
            .unwrap_or_else(|| ferese_config::default_wallpaper_for(appearance).into());
        document
            .set(&format!("{base}.lock_path"), old.to_string_lossy().into_owned().into())
            .map_err(|error| error.to_string())?;
    }

    if target != "lockscreen" {
        document
            .set(&format!("{base}.path"), image.to_string_lossy().into_owned().into())
            .map_err(|error| error.to_string())?;
    }

    if target != "background" {
        document
            .set(
                &format!("{base}.lock_path"),
                image.to_string_lossy().into_owned().into(),
            )
            .map_err(|error| error.to_string())?;
    }

    let after = wallpaper_theme(&document, directory, appearance)?.tokens.background;
    if (target != "lockscreen" && after.path.as_deref() != Some(image))
        || (target != "background" && after.lock_path.as_deref() != Some(image))
        || wallpaper_theme(&document, directory, other)?.tokens.background != other_before
    {
        return Err("Wallpaper change did not resolve to the requested image".into());
    }

    let parent = path.parent().ok_or("Invalid config path")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    std::io::Write::write_all(&mut temp, document.to_string().as_bytes()).map_err(|error| error.to_string())?;
    temp.as_file().sync_all().map_err(|error| error.to_string())?;
    if std::fs::read_to_string(path).unwrap_or_default() != source {
        return Err("Configuration changed; please try again".into());
    }
    temp.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn live_window_choices_include_inactive_workspaces_and_exclude_picker() {
        let windows = serde_json::json!([
            {"id": 1, "workspace": 2, "mapped": false, "app_id": "org.Editor", "title": "Notes", "capture_width": 640, "capture_height": 480},
            {"id": 2, "workspace": 1, "mapped": true, "app_id": "dev.ferese.ScreenShare", "capture_width": 400, "capture_height": 300},
            {"id": 3, "workspace": 1, "capture_width": 0, "capture_height": 0}
        ]);
        let sources = super::sharing_window_sources(&windows).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].window_id(), Some(1));
        assert_eq!((sources[0].width, sources[0].height), (640, 480));
        assert!(sources[0].label.contains("Notes"));
    }

    use super::*;

    #[test]
    fn local_uris_preserve_special_characters_and_reject_network_paths() {
        let path = Path::new("/tmp/wallpaper #1_日本.png");
        assert_eq!(local_path(&file_uri(path).unwrap()).unwrap(), path);
        for uri in ["https://example.com/a.png", "file://example.com/a.png", "not a URI"] {
            assert!(local_path(uri).is_err());
        }
    }

    #[test]
    fn screenshot_reads_remove_staging_files_on_success_and_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.png");
        std::fs::write(&path, b"capture").unwrap();
        assert_eq!(read_screenshot(path.to_str().unwrap()).unwrap(), b"capture");
        assert!(!path.exists());

        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        assert!(read_screenshot(path.to_str().unwrap()).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn window_choices_preserve_identity_and_bound_untrusted_titles() {
        let windows = serde_json::json!([
            {"id": 1, "mapped": true, "title": "Editor\n", "app_id": "zed"},
            {"id": 2, "mapped": false, "title": "Hidden", "app_id": "foot"},
            {"id": 3, "mapped": true, "title": "x".repeat(1024)}
        ]);
        let sources = window_sources(&windows).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].name, "1");
        assert_eq!(sources[0].label, "Editor — zed");
        assert_eq!(sources[1].label.chars().count(), 256);
        assert!(window_sources(&serde_json::json!({})).is_err());
    }

    #[test]
    fn pixel_color_is_normalized_rgb() {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[255, 128, 0, 255])
            .unwrap();
        assert_eq!(png_color(&bytes).unwrap(), (1., 128. / 255., 0.));
    }

    #[test]
    fn wallpaper_targets_preserve_independent_paths_and_other_settings() {
        use ferese_config::theme::Appearance::{Dark, Light};

        for appearance in [Light, Dark] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.kdl");
            std::fs::write(
                &path,
                "// Keep my settings\ntheme { background { path \"/old.png\"; }; }; commands { terminal \"foot\"; }\n",
            )
            .unwrap();
            let resolve = |mode| {
                let document = ferese_config::Document::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
                wallpaper_theme(&document, dir.path(), mode).unwrap().tokens.background
            };
            let other = if appearance == Light { Dark } else { Light };
            let untouched = resolve(other);
            edit_wallpaper(&path, Path::new("/desktop.png"), "background", appearance).unwrap();
            assert_eq!(resolve(appearance).path.as_deref(), Some(Path::new("/desktop.png")));
            assert_eq!(resolve(appearance).lock_path.as_deref(), Some(Path::new("/old.png")));
            assert_eq!(resolve(other), untouched);
            assert!(std::fs::read_to_string(&path).unwrap().contains("Keep my settings"));

            edit_wallpaper(&path, Path::new("/lock.png"), "lockscreen", appearance).unwrap();
            assert_eq!(resolve(appearance).path.as_deref(), Some(Path::new("/desktop.png")));
            assert_eq!(resolve(appearance).lock_path.as_deref(), Some(Path::new("/lock.png")));
            assert_eq!(resolve(other), untouched);

            edit_wallpaper(&path, Path::new("/both.png"), "both", appearance).unwrap();
            assert_eq!(resolve(appearance).path.as_deref(), Some(Path::new("/both.png")));
            assert_eq!(resolve(appearance).lock_path.as_deref(), Some(Path::new("/both.png")));
            assert_eq!(resolve(other), untouched);
            let document = ferese_config::Document::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(document.get("theme.background.path").unwrap(), "/old.png");
            assert_eq!(document.get("commands.terminal").unwrap(), &serde_json::json!(["foot"]));
        }
    }

    #[test]
    fn wallpaper_replaces_active_overrides_without_changing_auto_policy() {
        use ferese_config::theme::Appearance::{Dark, Light};

        for appearance in [Light, Dark] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.kdl");
            std::fs::write(
                &path,
                r#"theme {
                mode "auto"
                schedule { source "system"; }
                background { path "/shared.png"; lock-path "/shared-lock.png"; }
                light { background { path "light.png"; lock-path "light-lock.png"; }; }
                dark { background { path "dark.png"; lock-path "dark-lock.png"; }; }
            }
            "#,
            )
            .unwrap();
            let before = ferese_config::Document::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let other = if appearance == Light { Dark } else { Light };
            let old_active = wallpaper_theme(&before, dir.path(), appearance)
                .unwrap()
                .tokens
                .background;
            let old_other = wallpaper_theme(&before, dir.path(), other).unwrap().tokens.background;
            edit_wallpaper(&path, Path::new("/new.png"), "background", appearance).unwrap();
            let after = ferese_config::Document::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let current = wallpaper_theme(&after, dir.path(), appearance)
                .unwrap()
                .tokens
                .background;
            assert_eq!(current.path.as_deref(), Some(Path::new("/new.png")));
            assert_eq!(current.lock_path, old_active.lock_path);
            assert_eq!(
                wallpaper_theme(&after, dir.path(), other).unwrap().tokens.background,
                old_other
            );
            assert_eq!(after.get("theme.mode").unwrap(), "auto");
            assert_eq!(after.get("theme.background"), before.get("theme.background"));

            edit_wallpaper(&path, Path::new("/new-lock.png"), "lockscreen", appearance).unwrap();
            let after = ferese_config::Document::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let current = wallpaper_theme(&after, dir.path(), appearance)
                .unwrap()
                .tokens
                .background;
            assert_eq!(current.path.as_deref(), Some(Path::new("/new.png")));
            assert_eq!(current.lock_path.as_deref(), Some(Path::new("/new-lock.png")));
            assert_eq!(
                wallpaper_theme(&after, dir.path(), other).unwrap().tokens.background,
                old_other
            );
        }
    }

    #[test]
    fn desktop_only_change_pins_the_effective_variant_lock_wallpaper() {
        use ferese_config::theme::Appearance::{Dark, Light};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.kdl");
        std::fs::write(
            &path,
            r#"theme {
            background { path "shared.png"; }
            light { background { path "light.png"; }; }
            dark { background { path "dark.png"; }; }
        }
        "#,
        )
        .unwrap();
        let before = ferese_config::Document::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let previous = wallpaper_theme(&before, dir.path(), Dark).unwrap().tokens.background;
        assert_eq!(previous.lock_path, None);
        edit_wallpaper(&path, Path::new("/new.png"), "background", Dark).unwrap();
        let after = ferese_config::Document::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let current = wallpaper_theme(&after, dir.path(), Dark).unwrap().tokens.background;
        assert_eq!(current.lock_path, previous.path);
        assert_eq!(
            wallpaper_theme(&after, dir.path(), Light).unwrap().tokens.background,
            wallpaper_theme(&before, dir.path(), Light).unwrap().tokens.background
        );
    }

    #[test]
    fn wallpaper_validation_failure_preserves_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.kdl");
        let source = "theme { dark { file \"missing.kdl\"; }; }\n";
        std::fs::write(&path, source).unwrap();
        assert!(
            edit_wallpaper(
                &path,
                Path::new("/new.png"),
                "both",
                ferese_config::theme::Appearance::Dark
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }

    #[test]
    fn wallpaper_rejects_nonregular_files_and_cancellation_before_copy() {
        let dir = tempfile::tempdir().unwrap();
        assert!(prepare_wallpaper(dir.path(), &Cancel::default()).is_err());
        let path = dir.path().join("image");
        std::fs::write(&path, [0u8; 32]).unwrap();
        let cancel = Cancel::default();
        cancel.stop();
        assert!(prepare_wallpaper(&path, &cancel).unwrap_err().contains("cancelled"));
    }
}
