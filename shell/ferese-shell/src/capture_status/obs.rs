use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use cosmic::iced::futures::{SinkExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::Event;

#[derive(Clone, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default)]
    password: String,
}
fn default_port() -> u16 {
    4455
}

impl Config {
    pub fn load() -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
            })?;
        let path = base.join("ferese/obs.json");
        let metadata = std::fs::metadata(&path).ok()?;
        // The file may contain the OBS password. Never log its contents.
        if metadata.len() > 16 * 1024 || metadata.mode() & 0o077 != 0 {
            eprintln!("ferese-shell: obs.json must be private (mode 600) and smaller than 16 KiB");
            return None;
        }
        let config: Self = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        (config.enabled && config.port != 0).then_some(config)
    }
}

#[derive(Clone, Debug)]
struct Output {
    active: bool,
    paused: bool,
    reconnecting: bool,
    duration: Duration,
    sampled: Instant,
}
impl Default for Output {
    fn default() -> Self {
        Self {
            active: false,
            paused: false,
            reconnecting: false,
            duration: Duration::ZERO,
            sampled: Instant::now(),
        }
    }
}
impl Output {
    fn elapsed(&self) -> Duration {
        self.duration
            + if self.active && !self.paused {
                self.sampled.elapsed()
            } else {
                Duration::ZERO
            }
    }
    fn label(&self, record: bool, compact: bool) -> Option<String> {
        if !self.active {
            return None;
        }
        let state = if self.reconnecting {
            "Reconnecting"
        } else if self.paused {
            "REC paused"
        } else if record {
            "REC"
        } else {
            "LIVE"
        };
        if compact {
            return Some(state.into());
        }
        let seconds = self.elapsed().as_secs();
        Some(format!("{state} {:02}:{:02}", seconds / 60, seconds % 60))
    }
    fn status(&mut self, data: &Value) -> Result<(), String> {
        self.active = data["outputActive"]
            .as_bool()
            .ok_or("Missing OBS output status")?;
        self.paused = data["outputPaused"].as_bool().unwrap_or(false);
        self.reconnecting = data["outputReconnecting"].as_bool().unwrap_or(false);
        self.duration = Duration::from_millis(data["outputDuration"].as_u64().unwrap_or(0));
        self.sampled = Instant::now();
        Ok(())
    }
    fn event(&mut self, data: &Value) -> Result<(), String> {
        let state = data["outputState"].as_str().ok_or("Missing OBS state")?;
        self.duration = self.elapsed();
        self.sampled = Instant::now();
        match state {
            "OBS_WEBSOCKET_OUTPUT_STARTED" => {
                self.active = true;
                self.duration = Duration::ZERO;
                self.paused = false;
                self.reconnecting = false;
            }
            "OBS_WEBSOCKET_OUTPUT_PAUSED" => self.paused = true,
            "OBS_WEBSOCKET_OUTPUT_RESUMED" => self.paused = false,
            "OBS_WEBSOCKET_OUTPUT_RECONNECTING" => self.reconnecting = true,
            "OBS_WEBSOCKET_OUTPUT_RECONNECTED" => self.reconnecting = false,
            "OBS_WEBSOCKET_OUTPUT_STOPPED" | "OBS_WEBSOCKET_OUTPUT_STARTING" => {
                *self = Self::default()
            }
            "OBS_WEBSOCKET_OUTPUT_STOPPING" => (),
            _ => return Err("Unknown OBS output state".into()),
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    stream: Output,
    record: Output,
}
impl Snapshot {
    pub fn active(&self) -> bool {
        self.stream.active || self.record.active
    }
    pub fn ticking(&self) -> bool {
        self.stream.active || (self.record.active && !self.record.paused)
    }
    pub fn label(&self) -> Option<String> {
        self.bar_label(false).map(|label| format!("OBS · {label}"))
    }
    pub fn on_air(&self) -> bool {
        (self.stream.active && !self.stream.reconnecting)
            || (self.record.active && !self.record.paused && !self.record.reconnecting)
    }
    pub fn bar_label(&self, compact: bool) -> Option<String> {
        let stream = self.stream.label(false, compact);
        let record = self.record.label(true, compact);
        match (stream, record) {
            (Some(stream), Some(_)) => Some(format!(
                "{stream} · {}",
                if self.record.paused {
                    "REC paused"
                } else {
                    "REC"
                }
            )),
            (Some(label), None) | (None, Some(label)) => Some(label),
            _ => None,
        }
    }
    pub fn details(&self) -> Vec<String> {
        [
            self.stream.label(false, false),
            self.record.label(true, false),
        ]
        .into_iter()
        .flatten()
        .map(|label| format!("OBS · {label}"))
        .collect()
    }
}

fn identify(hello: &Value, password: &str) -> Result<Value, String> {
    use base64::Engine;
    use sha2::Digest;

    if hello["op"] != 0 || hello["d"]["rpcVersion"].as_u64().unwrap_or(0) < 1 {
        return Err("Unsupported OBS handshake".into());
    }
    // Outputs only: no meters, previews, scene changes, or per-frame events.
    let mut reply = json!({"op":1,"d":{"rpcVersion":1,"eventSubscriptions":64}});
    if let Some(auth) = hello["d"].get("authentication") {
        let salt = auth["salt"].as_str().ok_or("Missing OBS salt")?;
        let challenge = auth["challenge"].as_str().ok_or("Missing OBS challenge")?;
        let secret = STANDARD.encode(Sha256::digest(format!("{password}{salt}")));
        reply["d"]["authentication"] = STANDARD
            .encode(Sha256::digest(format!("{secret}{challenge}")))
            .into();
    }
    Ok(reply)
}

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn receive(socket: &mut Socket) -> Result<Value, String> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).map_err(|_| "Invalid OBS message".into());
            }
            Some(Ok(Message::Ping(_))) => socket.flush().await.map_err(|_| "OBS disconnected")?,
            Some(Ok(Message::Pong(_))) => (),
            _ => return Err("OBS disconnected".into()),
        }
    }
}

async fn send(socket: &mut Socket, value: Value) -> Result<(), String> {
    tokio::time::timeout(
        Duration::from_secs(3),
        socket.send(Message::Text(value.to_string().into())),
    )
    .await
    .map_err(|_| "OBS write timed out")?
    .map_err(|_| "OBS disconnected".into())
}

async fn request(socket: &mut Socket, record: bool, generation: u64) -> Result<(), String> {
    send(
        socket,
        json!({"op":6,"d":{
            "requestType": if record { "GetRecordStatus" } else { "GetStreamStatus" },
            "requestId": format!("{}-{generation}", if record { "record" } else { "stream" })
        }}),
    )
    .await
}

async fn connect(config: &Config) -> Result<Socket, String> {
    let limits = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(64 * 1024)
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));
    // Credentials are sent only to the user's local OBS instance.
    let (mut socket, _) = tokio_tungstenite::connect_async_with_config(
        format!("ws://127.0.0.1:{}", config.port),
        Some(limits),
        true,
    )
    .await
    .map_err(|_| "OBS unavailable")?;
    let hello = receive(&mut socket).await?;
    send(&mut socket, identify(&hello, &config.password)?).await?;
    if receive(&mut socket).await?["op"] != 2 {
        return Err("OBS identification failed".into());
    }
    Ok(socket)
}

pub(super) fn stream(config: &Arc<Config>) -> impl Stream<Item = Event> + use<> {
    let config = config.clone();
    cosmic::iced::stream::channel(1, async move |mut output| {
        let mut retry = 1;
        loop {
            let connection = tokio::time::timeout(Duration::from_secs(5), connect(&config)).await;
            if let Ok(Ok(mut socket)) = connection {
                let mut snapshot = Snapshot::default();
                let mut generations = [0u64; 2];
                let mut pending = [true; 2];
                let mut deadline = Instant::now() + Duration::from_secs(5);
                let connected = Instant::now();
                let result: Result<(), String> = async {
                    request(&mut socket, false, 0).await?;
                    request(&mut socket, true, 0).await?;
                    loop {
                        let message = if pending.iter().any(|pending| *pending) {
                            tokio::time::timeout_at(deadline.into(), receive(&mut socket))
                                .await
                                .map_err(|_| "OBS status timed out")??
                        } else {
                            receive(&mut socket).await?
                        };
                        let data = &message["d"];
                        match message["op"].as_u64() {
                            Some(5) => {
                                let index = match data["eventType"].as_str() {
                                    Some("StreamStateChanged") => 0,
                                    Some("RecordStateChanged") => 1,
                                    _ => continue,
                                };
                                let state = if index == 0 {
                                    &mut snapshot.stream
                                } else {
                                    &mut snapshot.record
                                };
                                state.event(&data["eventData"])?;
                                generations[index] += 1;
                                pending[index] = true;
                                deadline = Instant::now() + Duration::from_secs(5);
                                request(&mut socket, index == 1, generations[index]).await?;
                            }
                            Some(7) => {
                                let id = data["requestId"].as_str().unwrap_or_default();
                                let index = if id == format!("stream-{}", generations[0]) {
                                    0
                                } else if id == format!("record-{}", generations[1]) {
                                    1
                                } else {
                                    continue;
                                };
                                if !pending[index] {
                                    continue;
                                }
                                if data["requestStatus"]["result"] != true {
                                    return Err("OBS status unavailable".into());
                                }
                                let state = if index == 0 {
                                    &mut snapshot.stream
                                } else {
                                    &mut snapshot.record
                                };
                                state.status(&data["responseData"])?;
                                pending[index] = false;
                            }
                            _ => continue,
                        }
                        if output.send(Event::Obs(snapshot.clone())).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                .await;
                let _ = result;
                if connected.elapsed() >= Duration::from_secs(30) {
                    retry = 1;
                }
            }
            if output.send(Event::Obs(Snapshot::default())).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(retry)).await;
            retry = (retry * 2).min(60);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn websocket_ignores_stale_replies_waits_for_events_and_clears_disconnects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = Arc::new(Config {
            enabled: true,
            port: listener.local_addr().unwrap().port(),
            password: String::new(),
        });
        let (close, closed) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            socket
                .send(Message::Text(
                    json!({"op":0,"d":{"rpcVersion":1}}).to_string().into(),
                ))
                .await
                .unwrap();
            let identify: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(identify["d"]["eventSubscriptions"], 64);
            socket
                .send(Message::Text(
                    json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            for id in ["stream-0", "record-0"] {
                let request: Value =
                    serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                        .unwrap();
                assert_eq!(request["d"]["requestId"], id);
            }
            socket.send(Message::Text(json!({"op":5,"d":{"eventType":"StreamStateChanged","eventData":{"outputState":"OBS_WEBSOCKET_OUTPUT_STARTED"}}}).to_string().into())).await.unwrap();
            let request: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(request["d"]["requestId"], "stream-1");
            for (id, data) in [
                ("stream-0", json!({"outputActive":false,"outputDuration":0})),
                (
                    "record-0",
                    json!({"outputActive":true,"outputPaused":true,"outputDuration":12000}),
                ),
                (
                    "stream-1",
                    json!({"outputActive":true,"outputDuration":70000}),
                ),
            ] {
                socket.send(Message::Text(json!({"op":7,"d":{"requestId":id,"requestStatus":{"result":true},"responseData":data}}).to_string().into())).await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(100), socket.next())
                    .await
                    .is_err(),
                "idle subscriber sent another request"
            );
            closed.await.unwrap();
            socket.close(None).await.unwrap();
        });
        let events = stream(&config);
        tokio::pin!(events);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let Some(Event::Obs(snapshot)) = events.next().await else {
                    panic!("missing OBS state");
                };
                assert!(
                    snapshot.stream.active,
                    "stale initial response hid the live stream"
                );
                if snapshot.label().as_deref() == Some("OBS · LIVE 01:10 · REC paused") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(150), events.next())
                .await
                .is_err(),
            "idle subscriber produced another update"
        );
        close.send(()).unwrap();
        let Some(Event::Obs(snapshot)) =
            tokio::time::timeout(Duration::from_secs(1), events.next())
                .await
                .unwrap()
        else {
            panic!("missing disconnect");
        };
        assert!(!snapshot.active());
        assert!(!snapshot.ticking());
        server.await.unwrap();
    }

    #[test]
    fn idle_capture_pause_reconnect_and_simultaneous_outputs() {
        let mut state = Snapshot::default();
        assert!(!state.active());
        assert!(!state.ticking());
        assert!(!state.on_air());
        state
            .stream
            .event(&json!({"outputState":"OBS_WEBSOCKET_OUTPUT_STARTING"}))
            .unwrap();
        assert!(!state.active());
        state
            .stream
            .status(&json!({"outputActive":true,"outputDuration":74000}))
            .unwrap();
        assert_eq!(state.label().as_deref(), Some("OBS · LIVE 01:14"));
        assert_eq!(state.bar_label(true).as_deref(), Some("LIVE"));
        assert!(state.on_air());
        state
            .record
            .status(&json!({"outputActive":true,"outputDuration":12000}))
            .unwrap();
        assert_eq!(state.details().len(), 2);
        assert!(state.label().unwrap().ends_with(" · REC"));
        state
            .stream
            .event(&json!({"outputState":"OBS_WEBSOCKET_OUTPUT_RECONNECTING"}))
            .unwrap();
        assert!(!state.label().unwrap().contains("LIVE"));
        assert!(
            state.on_air(),
            "recording is still active while the stream reconnects"
        );
        state
            .record
            .event(&json!({"outputState":"OBS_WEBSOCKET_OUTPUT_PAUSED"}))
            .unwrap();
        assert!(
            !state.on_air(),
            "reconnecting and paused outputs are not on air"
        );
        state
            .stream
            .event(&json!({"outputState":"OBS_WEBSOCKET_OUTPUT_STOPPED"}))
            .unwrap();
        state
            .record
            .event(&json!({"outputState":"OBS_WEBSOCKET_OUTPUT_PAUSED"}))
            .unwrap();
        assert!(state.active());
        assert!(!state.ticking());
        assert!(state.label().unwrap().contains("paused"));
        assert_eq!(state.bar_label(true).as_deref(), Some("REC paused"));
        assert!(!state.on_air());
        assert_eq!(state.record.elapsed(), state.record.duration);
    }

    #[test]
    fn authentication_subscribes_only_to_output_events() {
        use base64::Engine;
        use sha2::Digest;

        let hello = json!({"op":0,"d":{"rpcVersion":1,"authentication":{"salt":"salt","challenge":"challenge"}}});
        let reply = identify(&hello, "password").unwrap();
        let secret = STANDARD.encode(Sha256::digest(b"passwordsalt"));
        let expected = STANDARD.encode(Sha256::digest(format!("{secret}challenge")));
        assert_eq!(reply["d"]["authentication"], expected);
        assert_eq!(reply["d"]["eventSubscriptions"], 64);
        assert!(identify(&json!({"op":2,"d":{}}), "password").is_err());
    }
}
