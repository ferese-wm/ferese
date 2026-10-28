//! Registration belongs to one authority process; its replacement cannot inherit live requests.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::fdo::DBusProxy;
use zbus::names::UniqueName;
use zbus::zvariant::Value;
use zbus::{Connection, Proxy};

use crate::agent::{AGENT_PATH, AUTHORITY, AUTHORITY_INTERFACE, AUTHORITY_PATH};

type Requests = Mutex<HashMap<String, Arc<AtomicBool>>>;

fn cancel_requests(active: &Requests) {
    for cancelled in active
        .lock()
        .expect("authentication ownership lock poisoned")
        .values()
    {
        cancelled.store(true, Ordering::Release);
    }
}

pub(super) async fn serve(
    connection: &Connection,
    subject: (String, HashMap<String, Value<'static>>),
    active: Arc<Requests>,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_util::StreamExt;

    let authority = Proxy::new(connection, AUTHORITY, AUTHORITY_PATH, AUTHORITY_INTERFACE).await?;
    // Subscribe before querying the owner so a restart during setup cannot be missed.
    let mut changes = authority.receive_owner_changed().await?;
    let bus = DBusProxy::new(connection).await?;
    let mut owner = match bus.get_name_owner(AUTHORITY.try_into()?).await {
        Ok(owner) => Some(UniqueName::from(owner)),
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => None,
        Err(error) => return Err(error.into()),
    };
    if owner.is_none() {
        // Registering against a unique owner must preserve D-Bus service activation.
        tokio::time::timeout(
            Duration::from_secs(10),
            bus.start_service_by_name(AUTHORITY.try_into()?, 0),
        )
        .await??;
    }
    let locale = std::env::var("LANG").unwrap_or_default();
    let mut registered = false;
    loop {
        let registration = async {
            if registered {
                return std::future::pending().await;
            }
            let Some(owner) = &owner else {
                return std::future::pending().await;
            };
            let proxy = Proxy::new(
                connection,
                owner.clone(),
                AUTHORITY_PATH,
                AUTHORITY_INTERFACE,
            )
            .await?;
            proxy
                .call::<_, _, ()>(
                    "RegisterAuthenticationAgent",
                    &(&subject, &locale, AGENT_PATH),
                )
                .await
        };
        tokio::select! {
            biased;
            change = changes.next() => {
                let Some(next) = change else {
                    cancel_requests(&active);
                    return Err("System bus disconnected while serving authentication".into());
                };
                if next != owner {
                    cancel_requests(&active);
                    owner = next;
                    registered = false;
                }
            }
            result = registration => {
                match result {
                    Ok(()) => registered = true,
                    Err(error) => {
                        // Exit rather than leave a live, unregistered agent. The session
                        // supervisor retries after transient bus/authority failures.
                        cancel_requests(&active);
                        return Err(error.into());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::StreamExt;
    use tokio::sync::mpsc::UnboundedSender;
    use zbus::MessageStream;
    use zbus::connection::Builder;
    use zbus::zvariant::OwnedValue;

    use super::*;
    use crate::agent;
    use crate::agent::tests::PrivateBus;

    struct Authority(UnboundedSender<()>);
    #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
    impl Authority {
        fn register_authentication_agent(
            &self,
            _subject: (String, HashMap<String, OwnedValue>),
            _locale: String,
            _path: String,
        ) {
            self.0.send(()).unwrap();
        }
    }

    // Launched by the private bus's service activation, never by the system bus.
    #[tokio::test]
    async fn activation_fixture() {
        let Ok(marker) = std::env::var("FERESE_TEST_ACTIVATION_MARKER") else {
            return;
        };
        let address = std::env::var("DBUS_STARTER_ADDRESS").unwrap();
        let (tx, mut registrations) = tokio::sync::mpsc::unbounded_channel();
        let connection = Builder::address(address.as_str())
            .unwrap()
            .name(AUTHORITY)
            .unwrap()
            .serve_at(AUTHORITY_PATH, Authority(tx))
            .unwrap()
            .build()
            .await
            .unwrap();
        registrations.recv().await.unwrap();
        std::fs::write(marker, "registered").unwrap();
        let mut messages = MessageStream::from(&connection);
        while matches!(messages.next().await, Some(Ok(_))) {}
    }

    #[tokio::test]
    async fn absent_authority_is_activated_before_the_first_request() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("registered");
        let service = directory.path().join(format!("{AUTHORITY}.service"));
        let executable = std::env::current_exe().expect("locate the activation fixture executable");
        let service_contents = format!(
            "[D-BUS Service]\n\
             Name={AUTHORITY}\n\
             Exec={} --exact authority::tests::activation_fixture --nocapture\n",
            executable.display(),
        );
        std::fs::write(service, service_contents).expect("write the private activation service");
        let config = directory.path().join("bus.conf");
        let config_contents = format!(
            "<busconfig>\
               <type>session</type>\
               <listen>unix:tmpdir=/tmp</listen>\
               <servicedir>{}</servicedir>\
               <policy context=\"default\">\
                 <allow send_destination=\"*\"/>\
                 <allow receive_sender=\"*\"/>\
                 <allow own=\"*\"/>\
               </policy>\
             </busconfig>",
            directory.path().display(),
        );
        std::fs::write(&config, config_contents).expect("write the private bus configuration");
        let mut command = std::process::Command::new("dbus-daemon");
        command
            .args(["--nofork", "--print-address=1", "--config-file"])
            .arg(&config)
            .env("FERESE_TEST_ACTIVATION_MARKER", &marker);
        let (mut bus, address) = PrivateBus::start_command(command);
        let connection = Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let active = Arc::new(Mutex::new(HashMap::new()));
        let worker = tokio::spawn(async move {
            serve(
                &connection,
                agent::login_session_subject("test-session".into()),
                active,
            )
            .await
            .map_err(|error| error.to_string())
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        bus.stop();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), worker)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn replacement_authority_registers_again_and_invalidates_old_requests() {
        let (mut bus, address) = PrivateBus::start();
        let (tx, mut registrations) = tokio::sync::mpsc::unbounded_channel();
        let authority = Builder::address(address.as_str())
            .unwrap()
            .name(AUTHORITY)
            .unwrap()
            .serve_at(AUTHORITY_PATH, Authority(tx.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let connection = Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let active = Arc::new(Mutex::new(HashMap::from([(
            "old-cookie".into(),
            cancelled.clone(),
        )])));
        let worker = tokio::spawn(async move {
            serve(
                &connection,
                agent::login_session_subject("test-session".into()),
                active,
            )
            .await
            .map_err(|error| error.to_string())
        });
        tokio::time::timeout(Duration::from_secs(3), registrations.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!cancelled.load(Ordering::Acquire));
        authority.close().await.unwrap();
        let replacement = Builder::address(address.as_str())
            .unwrap()
            .name(AUTHORITY)
            .unwrap()
            .serve_at(AUTHORITY_PATH, Authority(tx))
            .unwrap()
            .build()
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), registrations.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        bus.stop();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), worker)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        drop(replacement);
    }
}
