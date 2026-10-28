use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Debug, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};
use zeroize::Zeroizing;

use crate::{authority, session};

pub(super) const AGENT_PATH: &str = "/dev/ferese/PolkitAgent";
pub(super) const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
pub(super) const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
pub(super) const AUTHORITY_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum PromptEvent {
    Start(AuthenticationRequest),
    Request {
        generation: Generation,
        prompt: String,
        echo: bool,
    },
    Info {
        generation: Generation,
        text: String,
    },
    Error {
        generation: Generation,
        text: String,
    },
    Finished {
        text: String,
    },
    Response {
        generation: Generation,
        uid: u32,
        value: Zeroizing<String>,
    },
    IdentityChanged {
        generation: Generation,
        uid: u32,
    },
    SelectIdentity {
        generation: Generation,
        uid: u32,
    },
    Cancel,
}

impl Debug for PromptEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("AuthenticationProtocolEvent")
    }
}

type Identity = (String, HashMap<String, OwnedValue>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Generation {
    pub(super) selection: u64,
    pub(super) attempt: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct IdentityChoice {
    pub(super) uid: u32,
    pub(super) name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AuthenticationRequest {
    pub(super) generation: Generation,
    pub(super) action_id: String,
    pub(super) message: String,
    pub(super) icon_name: String,
    pub(super) details: BTreeMap<String, String>,
    pub(super) vendor_name: String,
    pub(super) vendor_url: String,
    pub(super) identities: Vec<IdentityChoice>,
    pub(super) selected_uid: u32,
}

#[derive(Serialize, Deserialize, zbus::zvariant::Type)]
struct ActionDescription {
    action_id: String,
    description: String,
    message: String,
    vendor_name: String,
    vendor_url: String,
    icon_name: String,
    implicit_any: u32,
    implicit_inactive: u32,
    implicit_active: u32,
    annotations: HashMap<String, String>,
}

struct Agent {
    connection: Connection,
    active: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum AgentError {
    Cancelled(String),
    Failed(String),
}

struct RequestOwnership {
    cookie: String,
    active: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl Drop for RequestOwnership {
    fn drop(&mut self) {
        self.active
            .lock()
            .expect("authentication ownership lock poisoned")
            .remove(&self.cookie);
    }
}

async fn wait_cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

impl Agent {
    async fn vendor(&self, action_id: &str) -> (String, String) {
        let lookup = async {
            let authority = Proxy::new(
                &self.connection,
                AUTHORITY,
                AUTHORITY_PATH,
                AUTHORITY_INTERFACE,
            )
            .await?;
            let locale = std::env::var("LANG").unwrap_or_default();
            let actions: Vec<ActionDescription> =
                authority.call("EnumerateActions", &(locale,)).await?;
            Ok::<_, zbus::Error>(
                actions
                    .into_iter()
                    .find(|action| action.action_id == action_id),
            )
        };
        // Missing metadata must never prevent authentication or cancellation.
        match tokio::time::timeout(Duration::from_secs(2), lookup).await {
            Ok(Ok(Some(action))) => (action.vendor_name, action.vendor_url),
            _ => (String::new(), String::new()),
        }
    }
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        details: HashMap<String, String>,
        cookie: String,
        identities: Vec<Identity>,
    ) -> Result<(), AgentError> {
        let allowed = available_identities(&identities);
        let uid = choose_identity(&allowed)
            .ok_or_else(|| AgentError::Failed("No supported authentication identity".into()))?;
        let cancelled = Arc::new(AtomicBool::new(false));

        {
            let mut active = self
                .active
                .lock()
                .expect("authentication session ownership must remain consistent across callbacks");
            if !active.is_empty() {
                return Err(AgentError::Failed(
                    "Authentication is already in progress".into(),
                ));
            }
            active.insert(cookie.clone(), cancelled.clone());
        }

        let ownership = RequestOwnership {
            cookie: cookie.clone(),
            active: self.active.clone(),
        };
        let preparation = async {
            let (vendor_name, vendor_url) = self.vendor(&action_id).await;
            let identities = session::identities(&allowed).await;
            (vendor_name, vendor_url, identities)
        };
        let (vendor_name, vendor_url, identities) = tokio::select! {
            result = preparation => result,
            () = wait_cancelled(&cancelled) => return Err(AgentError::Cancelled("Authentication cancelled".into())),
        };
        let request = AuthenticationRequest {
            generation: Generation::default(),
            action_id,
            message,
            icon_name,
            details: details.into_iter().collect(),
            vendor_name,
            vendor_url,
            identities,
            selected_uid: uid,
        };
        let result = tokio::task::spawn_blocking({
            let cookie = cookie.clone();
            let cancelled = cancelled.clone();
            move || {
                // Keep admission closed until the helper and prompt have both been reaped,
                // even if the D-Bus caller disappears while this task is running.
                let _ownership = ownership;
                session::authenticate(request, cookie, cancelled)
            }
        })
        .await;

        if cancelled.load(Ordering::Acquire) {
            return Err(AgentError::Cancelled("Authentication cancelled".into()));
        }
        match result.map_err(|error| AgentError::Failed(error.to_string()))? {
            Ok(true) => Ok(()),
            Ok(false) => Err(AgentError::Cancelled("Authentication cancelled".into())),
            Err(error) => Err(AgentError::Failed(error)),
        }
    }

    fn cancel_authentication(&self, cookie: String) {
        if let Some(cancelled) = self
            .active
            .lock()
            .expect("authentication session ownership must remain consistent across callbacks")
            .get(&cookie)
        {
            cancelled.store(true, Ordering::Release);
        }
    }
}

const MAX_IDENTITIES: usize = 16;

#[must_use]
fn available_identities(identities: &[Identity]) -> Vec<u32> {
    let mut available = Vec::new();
    let supported_uids = identities
        .iter()
        .filter(|(kind, _)| kind == "unix-user")
        .filter_map(|(_, details)| {
            details
                .get("uid")
                .and_then(|uid| u32::try_from(uid.clone()).ok())
        })
        .filter(|uid| *uid <= i32::MAX as u32);
    for uid in supported_uids {
        if !available.contains(&uid) {
            available.push(uid);
            if available.len() == MAX_IDENTITIES {
                break;
            }
        }
    }
    available
}

#[must_use]
fn choose_identity(available: &[u32]) -> Option<u32> {
    // SAFETY: geteuid has no pointer arguments or caller-side preconditions.
    let current = unsafe { libc::geteuid() };
    available
        .iter()
        .copied()
        .find(|uid| *uid == current)
        .or_else(|| available.first().copied())
}

fn process_subject() -> Result<(String, HashMap<String, Value<'static>>), String> {
    let stat = std::fs::read_to_string("/proc/self/stat").map_err(|error| error.to_string())?;
    let suffix = stat
        .rsplit_once(") ")
        .map(|(_, suffix)| suffix)
        .ok_or("Invalid /proc/self/stat")?;
    let started = suffix
        .split_whitespace()
        .nth(19)
        .ok_or("Missing process start time")?
        .parse::<u64>()
        .map_err(|error| error.to_string())?;
    let mut details = HashMap::new();
    details.insert("pid".to_owned(), Value::from(std::process::id()));
    details.insert(
        "uid".to_owned(),
        // SAFETY: geteuid has no pointer arguments or caller-side preconditions.
        Value::from(unsafe { libc::geteuid() } as i32),
    );
    details.insert("start-time".to_owned(), Value::from(started));
    Ok(("unix-process".into(), details))
}

async fn session_subject(
    connection: &Connection,
) -> Result<(String, HashMap<String, Value<'static>>), Box<dyn std::error::Error>> {
    // Nested previews must not take over authentication for the host desktop.
    if std::env::var("FERESE_SESSION_MODE").as_deref() == Ok("embedded")
        || std::env::var("FERESE_SESSION_IMPORT_ENV").as_deref() == Ok("0")
    {
        return process_subject().map_err(Into::into);
    }
    let manager = Proxy::new(
        connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let path: OwnedObjectPath = match manager
        .call("GetSessionByPID", &(std::process::id(),))
        .await
    {
        Ok(path) => path,
        Err(error) => {
            // A launcher in the user manager can be outside the session cgroup
            // while retaining the login session's environment.
            let id = std::env::var("XDG_SESSION_ID").map_err(|_| error)?;
            manager.call("GetSession", &(id,)).await?
        }
    };
    let session = Proxy::new(
        connection,
        "org.freedesktop.login1",
        path,
        "org.freedesktop.login1.Session",
    )
    .await?;
    let id: String = session.get_property("Id").await?;
    Ok(login_session_subject(id))
}

#[must_use]
pub(super) fn login_session_subject(id: String) -> (String, HashMap<String, Value<'static>>) {
    (
        "unix-session".into(),
        HashMap::from([("session-id".into(), Value::from(id))]),
    )
}

pub(super) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::system().await?;
    let active = Arc::new(Mutex::new(HashMap::new()));
    connection
        .object_server()
        .at(
            AGENT_PATH,
            Agent {
                connection: connection.clone(),
                active: active.clone(),
            },
        )
        .await?;
    let subject = session_subject(&connection).await?;
    authority::serve(&connection, subject, active).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn request_fixture() -> AuthenticationRequest {
        AuthenticationRequest {
            generation: Generation::default(),
            action_id: "dev.ferese.test".into(),
            message: "Authorize this complete request ".repeat(30),
            icon_name: "system-software-install".into(),
            details: BTreeMap::from([
                ("program".into(), "/usr/bin/example".into()),
                ("command_line".into(), "example --long-detail".into()),
            ]),
            vendor_name: "Example Vendor".into(),
            vendor_url: "https://example.test/vendor".into(),
            identities: vec![
                IdentityChoice {
                    uid: 1000,
                    name: "current".into(),
                },
                IdentityChoice {
                    uid: 1001,
                    name: "other".into(),
                },
            ],
            selected_uid: 1000,
        }
    }

    #[test]
    fn complete_request_survives_the_prompt_protocol() {
        let request = request_fixture();
        let encoded = serde_json::to_string(&PromptEvent::Start(request.clone())).unwrap();
        let PromptEvent::Start(decoded) = serde_json::from_str(&encoded).unwrap() else {
            panic!("Missing Start")
        };
        assert_eq!(decoded, request);
        assert!(decoded.message.len() > 240);
        assert!(encoded.contains("\"type\":\"start\""));
    }

    #[test]
    fn prompt_protocol_keeps_selection_response_and_echo_fields() {
        let selection: PromptEvent = serde_json::from_str(
            r#"{"type":"select_identity","generation":{"selection":2,"attempt":0},"uid":1001}"#,
        )
        .unwrap();
        assert!(matches!(
            selection,
            PromptEvent::SelectIdentity {
                uid: 1001,
                generation: Generation {
                    selection: 2,
                    attempt: 0
                }
            }
        ));
        let response = PromptEvent::Response {
            generation: Generation {
                selection: 2,
                attempt: 1,
            },
            uid: 1001,
            value: "test-response".to_owned().into(),
        };
        let decoded = serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert!(
            matches!(decoded, PromptEvent::Response { uid: 1001, generation: Generation { selection: 2, attempt: 1 }, value } if value.as_str() == "test-response")
        );
        for echo in [true, false] {
            let event = PromptEvent::Request {
                generation: Generation {
                    selection: 2,
                    attempt: 1,
                },
                prompt: "Password:".into(),
                echo,
            };
            let decoded = serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
            assert!(
                matches!(decoded, PromptEvent::Request { generation: Generation { selection: 2, attempt: 1 }, prompt, echo: actual } if prompt == "Password:" && actual == echo)
            );
        }
    }

    #[tokio::test]
    async fn busy_requests_are_rejected_on_the_bus_and_cancellation_has_polkit_error_name() {
        let (_bus, address) = PrivateBus::start();
        let connection = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let active = Arc::new(Mutex::new(HashMap::from([(
            "first".into(),
            cancelled.clone(),
        )])));
        connection
            .object_server()
            .at(
                AGENT_PATH,
                Agent {
                    connection: connection.clone(),
                    active: active.clone(),
                },
            )
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let proxy = Proxy::new(
            &client,
            connection.unique_name().unwrap().clone(),
            AGENT_PATH,
            "org.freedesktop.PolicyKit1.AuthenticationAgent",
        )
        .await
        .unwrap();
        let result = proxy
            .call::<_, _, ()>(
                "BeginAuthentication",
                &(
                    "test.action",
                    "Test",
                    "",
                    HashMap::<String, String>::new(),
                    "second",
                    vec![("unix-user", HashMap::from([("uid", Value::from(1000u32))]))],
                ),
            )
            .await;
        assert!(
            matches!(result, Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == "org.freedesktop.PolicyKit1.Error.Failed")
        );
        assert_eq!(active.lock().unwrap().len(), 1);
        proxy
            .call::<_, _, ()>("CancelAuthentication", &("first",))
            .await
            .unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        // Hold metadata lookup open so cancellation goes through the real agent
        // handler before it could launch an authentication helper.
        connection.request_name(AUTHORITY).await.unwrap();
        connection
            .object_server()
            .at(AUTHORITY_PATH, WaitingAuthority)
            .await
            .unwrap();
        active.lock().unwrap().clear();
        let body = (
            "test.action",
            "Test",
            "",
            HashMap::<String, String>::new(),
            "pending",
            vec![("unix-user", HashMap::from([("uid", Value::from(1000u32))]))],
        );
        let request = proxy.call::<_, _, ()>("BeginAuthentication", &body);
        let cancel = async {
            while !active.lock().unwrap().contains_key("pending") {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            proxy
                .call::<_, _, ()>("CancelAuthentication", &("pending",))
                .await
                .unwrap();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(request, cancel)
        })
        .await
        .unwrap();
        assert!(
            matches!(result, Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == "org.freedesktop.PolicyKit1.Error.Cancelled")
        );
        assert!(active.lock().unwrap().is_empty());
    }

    struct WaitingAuthority;
    #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
    impl WaitingAuthority {
        async fn enumerate_actions(&self, _locale: String) -> Vec<ActionDescription> {
            std::future::pending().await
        }
    }

    #[test]
    fn only_supported_unique_user_identities_are_offered() {
        let identities = vec![
            (
                "unix-user".into(),
                HashMap::from([("uid".into(), OwnedValue::from(1000u32))]),
            ),
            (
                "unix-user".into(),
                HashMap::from([("uid".into(), OwnedValue::from(1000u32))]),
            ),
            (
                "unix-group".into(),
                HashMap::from([("gid".into(), OwnedValue::from(1001u32))]),
            ),
            (
                "unix-user".into(),
                HashMap::from([("uid".into(), OwnedValue::from(u32::MAX))]),
            ),
            ("unix-user".into(), HashMap::new()),
        ];
        assert_eq!(available_identities(&identities), vec![1000]);
        assert_eq!(choose_identity(&[]), None);
        assert_eq!(choose_identity(&[1001]), Some(1001));
    }

    #[test]
    fn offered_identities_are_capped_before_username_lookups() {
        let identities: Vec<_> = (0..4096u32)
            .map(|uid| {
                (
                    "unix-user".into(),
                    HashMap::from([("uid".into(), OwnedValue::from(uid))]),
                )
            })
            .collect();
        assert_eq!(
            available_identities(&identities),
            (0..MAX_IDENTITIES as u32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn metadata_signature_matches_polkit_action_descriptions() {
        use zbus::zvariant::Type;
        assert_eq!(ActionDescription::SIGNATURE.to_string(), "(ssssssuuua{ss})");
    }

    pub(crate) struct PrivateBus(std::process::Child);

    impl PrivateBus {
        pub(crate) fn start() -> (Self, String) {
            let mut command = std::process::Command::new("dbus-daemon");
            command.args(["--session", "--nofork", "--print-address=1"]);
            Self::start_command(command)
        }
        pub(crate) fn start_command(mut command: std::process::Command) -> (Self, String) {
            use std::io::{BufRead, BufReader};
            let mut child = command
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut address = String::new();
            let mut output = BufReader::new(child.stdout.take().unwrap());
            output.read_line(&mut address).unwrap();
            // Activated fixture processes inherit this pipe; keep draining it.
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut output, &mut std::io::sink());
            });
            (Self(child), address.trim().into())
        }
        pub(crate) fn stop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct MockAuthority;

    #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
    impl MockAuthority {
        fn enumerate_actions(&self, _locale: String) -> Vec<ActionDescription> {
            vec![ActionDescription {
                action_id: "test.action".into(),
                description: "Test action".into(),
                message: "Authorize test".into(),
                vendor_name: "Test vendor".into(),
                vendor_url: "https://example.test/vendor".into(),
                icon_name: "system-software-install".into(),
                implicit_any: 0,
                implicit_inactive: 0,
                implicit_active: 1,
                annotations: HashMap::new(),
            }]
        }
    }

    #[tokio::test]
    async fn vendor_metadata_uses_real_dbus_encoding_and_is_optional() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let mut bus = PrivateBus(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let server = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .name(AUTHORITY)
            .unwrap()
            .serve_at(AUTHORITY_PATH, MockAuthority)
            .unwrap()
            .build()
            .await
            .unwrap();
        let connection = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        let agent = Agent {
            connection,
            active: Arc::new(Mutex::new(HashMap::new())),
        };
        assert_eq!(
            agent.vendor("test.action").await,
            ("Test vendor".into(), "https://example.test/vendor".into())
        );
        assert_eq!(
            agent.vendor("missing.action").await,
            (String::new(), String::new())
        );
        server.close().await.unwrap();
        assert_eq!(
            agent.vendor("test.action").await,
            (String::new(), String::new())
        );
    }

    #[test]
    fn desktop_agent_registers_for_the_login_session() {
        let (kind, details) = login_session_subject("test-session".into());
        assert_eq!(kind, "unix-session");
        assert_eq!(details.len(), 1);
        assert_eq!(details["session-id"], Value::from("test-session"));
    }

    #[test]
    fn process_subject_has_required_identity_fields() {
        let (kind, details) = process_subject().unwrap();
        assert_eq!(kind, "unix-process");
        assert_eq!(details["pid"], Value::from(std::process::id()));
        assert_eq!(
            details["uid"],
            // SAFETY: geteuid has no pointer arguments or caller-side preconditions.
            Value::from(unsafe { libc::geteuid() } as i32)
        );
        assert!(u64::try_from(details["start-time"].clone()).unwrap() > 0);
    }

    #[test]
    fn chooses_current_identity_when_available() {
        // SAFETY: geteuid has no pointer arguments or caller-side preconditions.
        let current = unsafe { libc::geteuid() };
        let identity = |uid: u32| {
            (
                "unix-user".to_owned(),
                HashMap::from([("uid".to_owned(), OwnedValue::from(uid))]),
            )
        };
        assert_eq!(
            choose_identity(&available_identities(&[identity(0), identity(current)])),
            Some(current)
        );
    }
}
