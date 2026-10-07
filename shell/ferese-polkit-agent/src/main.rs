mod prompt;
mod session;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zbus::zvariant::{OwnedValue, Value};

const AGENT_PATH: &str = "/dev/ferese/PolkitAgent";
const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PromptEvent {
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
    Response {
        generation: Generation,
        uid: u32,
        value: String,
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

type Identity = (String, HashMap<String, OwnedValue>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Generation {
    selection: u64,
    attempt: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IdentityChoice {
    uid: u32,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AuthenticationRequest {
    generation: Generation,
    message: String,
    icon_name: String,
    details: BTreeMap<String, String>,
    vendor_name: String,
    vendor_url: String,
    identities: Vec<IdentityChoice>,
    selected_uid: u32,
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
    connection: zbus::Connection,
    active: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl Agent {
    async fn vendor(&self, action_id: &str) -> (String, String) {
        let lookup = async {
            let authority = zbus::Proxy::new(&self.connection, AUTHORITY, AUTHORITY_PATH, AUTHORITY_INTERFACE).await?;
            let locale = std::env::var("LANG").unwrap_or_default();
            let actions: Vec<ActionDescription> = authority.call("EnumerateActions", &(locale,)).await?;
            Ok::<_, zbus::Error>(actions.into_iter().find(|action| action.action_id == action_id))
        };
        // Missing metadata must never prevent authentication or cancellation.
        match tokio::time::timeout(std::time::Duration::from_secs(2), lookup).await {
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
    ) -> zbus::fdo::Result<()> {
        let allowed = available_identities(&identities);
        let uid = choose_identity(&allowed)
            .ok_or_else(|| zbus::fdo::Error::Failed("No supported authentication identity".into()))?;
        let cancelled = Arc::new(AtomicBool::new(false));

        {
            let mut active = self.active.lock().unwrap();
            if active.contains_key(&cookie) {
                return Err(zbus::fdo::Error::Failed("Duplicate request".into()));
            }
            active.insert(cookie.clone(), cancelled.clone());
        }

        let (vendor_name, vendor_url) = self.vendor(&action_id).await;
        let request = AuthenticationRequest {
            generation: Generation::default(),
            message,
            icon_name,
            details: details.into_iter().collect(),
            vendor_name,
            vendor_url,
            identities: allowed
                .into_iter()
                .map(|uid| IdentityChoice {
                    uid,
                    name: session::username(uid),
                })
                .collect(),
            selected_uid: uid,
        };
        let result = tokio::task::spawn_blocking({
            let cookie = cookie.clone();
            move || session::authenticate(request, cookie, cancelled)
        })
        .await;

        self.active.lock().unwrap().remove(&cookie);
        match result.map_err(|error| zbus::fdo::Error::Failed(error.to_string()))? {
            Ok(true) => Ok(()),
            Ok(false) => Err(zbus::fdo::Error::Failed("Authentication cancelled".into())),
            Err(error) => Err(zbus::fdo::Error::Failed(error)),
        }
    }

    fn cancel_authentication(&self, cookie: String) {
        if let Some(cancelled) = self.active.lock().unwrap().get(&cookie) {
            cancelled.store(true, Ordering::Release);
        }
    }
}

const MAX_IDENTITIES: usize = 16;

fn available_identities(identities: &[Identity]) -> Vec<u32> {
    let mut available = Vec::new();
    for uid in identities
        .iter()
        .filter(|(kind, _)| kind == "unix-user")
        .filter_map(|(_, details)| details.get("uid").and_then(|uid| u32::try_from(uid.clone()).ok()))
        .filter(|uid| *uid <= i32::MAX as u32)
    {
        if !available.contains(&uid) {
            available.push(uid);
            if available.len() == MAX_IDENTITIES {
                break;
            }
        }
    }
    available
}

fn choose_identity(available: &[u32]) -> Option<u32> {
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
    details.insert("uid".to_owned(), Value::from(unsafe { libc::geteuid() } as i32));
    details.insert("start-time".to_owned(), Value::from(started));
    Ok(("unix-process".into(), details))
}

async fn session_subject(
    connection: &zbus::Connection,
) -> Result<(String, HashMap<String, Value<'static>>), Box<dyn std::error::Error>> {
    // Nested previews must not take over authentication for the host desktop.
    if std::env::var("FERESE_SESSION_MODE").as_deref() == Ok("embedded")
        || std::env::var("FERESE_SESSION_IMPORT_ENV").as_deref() == Ok("0")
    {
        return process_subject().map_err(Into::into);
    }
    let manager = zbus::Proxy::new(
        connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let path: zbus::zvariant::OwnedObjectPath = match manager.call("GetSessionByPID", &(std::process::id(),)).await {
        Ok(path) => path,
        Err(error) => {
            // A launcher in the user manager can be outside the session cgroup
            // while retaining the login session's environment.
            let id = std::env::var("XDG_SESSION_ID").map_err(|_| error)?;
            manager.call("GetSession", &(id,)).await?
        }
    };
    let session = zbus::Proxy::new(
        connection,
        "org.freedesktop.login1",
        path,
        "org.freedesktop.login1.Session",
    )
    .await?;
    let id: String = session.get_property("Id").await?;
    Ok(login_session_subject(id))
}

fn login_session_subject(id: String) -> (String, HashMap<String, Value<'static>>) {
    (
        "unix-session".into(),
        HashMap::from([("session-id".into(), Value::from(id))]),
    )
}

async fn run_agent() -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::system().await?;
    connection
        .object_server()
        .at(
            AGENT_PATH,
            Agent {
                connection: connection.clone(),
                active: Mutex::new(HashMap::new()),
            },
        )
        .await?;
    let subject = session_subject(&connection).await?;
    let authority = zbus::Proxy::new(&connection, AUTHORITY, AUTHORITY_PATH, AUTHORITY_INTERFACE).await?;
    let locale = std::env::var("LANG").unwrap_or_default();

    authority
        .call::<_, _, ()>("RegisterAuthenticationAgent", &(subject, locale, AGENT_PATH))
        .await?;
    std::future::pending::<()>().await;

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0);
    }

    match std::env::args().nth(1).as_deref() {
        None => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(run_agent()),
        Some("--prompt") => prompt::run(),
        Some("--check") => session::check().map_err(Into::into),
        _ => Err("Usage: ferese-polkit-agent [--check]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn request_fixture() -> AuthenticationRequest {
        AuthenticationRequest {
            generation: Generation::default(),
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
        let selection: PromptEvent =
            serde_json::from_str(r#"{"type":"select_identity","generation":{"selection":2,"attempt":0},"uid":1001}"#)
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
            value: "test-response".into(),
        };
        let decoded = serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert!(
            matches!(decoded, PromptEvent::Response { uid: 1001, generation: Generation { selection: 2, attempt: 1 }, value } if value == "test-response")
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

    struct PrivateBus(std::process::Child);

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
            active: Mutex::new(HashMap::new()),
        };
        assert_eq!(
            agent.vendor("test.action").await,
            ("Test vendor".into(), "https://example.test/vendor".into())
        );
        assert_eq!(agent.vendor("missing.action").await, (String::new(), String::new()));
        server.close().await.unwrap();
        assert_eq!(agent.vendor("test.action").await, (String::new(), String::new()));
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
        assert_eq!(details["uid"], Value::from(unsafe { libc::geteuid() } as i32));
        assert!(u64::try_from(details["start-time"].clone()).unwrap() > 0);
    }

    #[test]
    fn chooses_current_identity_when_available() {
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
