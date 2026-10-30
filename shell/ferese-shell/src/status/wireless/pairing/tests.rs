use super::*;

fn agent() -> (Agent, mpsc::UnboundedReceiver<Update>) {
    let (sender, receiver) = mpsc::unbounded();
    (
        Agent {
            authority: Arc::new(OnceLock::new()),
            cancelled: Arc::new(AtomicBool::new(false)),
            phase: Arc::new(AtomicU8::new(Phase::Preparing as u8)),
            device: "/device/selected".into(),
            sender,
            pending: Mutex::new(None),
            sequence: AtomicU64::new(0),
        },
        receiver,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn confirmation_requires_an_answer_and_cannot_authorize_another_device() {
    let (agent, mut events) = agent();
    let other = OwnedObjectPath::try_from("/device/other").unwrap();
    assert!(agent.ask(&other, Prompt::Pin).await.is_err());
    let selected = OwnedObjectPath::try_from("/device/selected").unwrap();
    let ui = async {
        let Update::Prompt(request) = events.next().await.unwrap() else {
            panic!("expected confirmation");
        };
        assert!(matches!(request.prompt, Prompt::Confirm(Some(123456))));
        request.answer(Some(Password::default()));
        request.answer(None);
    };
    let (result, ()) =
        cosmic::iced::futures::join!(agent.ask(&selected, Prompt::Confirm(Some(123456))), ui);
    assert!(result.is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_or_cancelling_a_question_cannot_complete_authentication() {
    for cancel in [false, true] {
        let (agent, mut events) = agent();
        let selected = OwnedObjectPath::try_from("/device/selected").unwrap();
        let ui = async {
            let Update::Prompt(request) = events.next().await.unwrap() else {
                panic!("expected a question");
            };
            if cancel {
                agent.cancel_request();
                request.answer(Some(Password::new("123456".into())));
            } else {
                drop(request);
            }
        };
        let (result, ()) = cosmic::iced::futures::join!(agent.ask(&selected, Prompt::Passkey), ui);
        assert!(result.is_err());
    }
}

#[test]
fn pairing_input_validation_and_debug_do_not_leak_codes() {
    let (request, _) = test_request(1, Prompt::Passkey);
    for code in ["", "12345", "1234567", "+12345", "１２３４５６"] {
        assert!(!request.accepts(code), "{code}");
    }
    assert!(request.accepts("000123"));
    let (pin, _) = test_request(2, Prompt::Pin);
    assert!(pin.accepts("abcd1234"));
    assert!(!pin.accepts(""));
    assert!(!pin.accepts("12345678901234567"));
    let (display, _) = test_request(3, Prompt::Display("123456".into()));
    assert!(!display.accepts(""));
    assert!(!format!("{display:?}").contains("123456"));
}

#[test]
#[ignore = "requires dbus-daemon; uses a private test bus"]
fn private_bus_agent_rejects_forged_callbacks_and_accepts_its_authority() {
    let bus = crate::status::tests::TestBus::new();
    let authority = bus.connect();
    let spoof = bus.connect();
    let (agent, mut events) = agent();
    agent
        .authority
        .set(authority.unique_name().unwrap().to_string())
        .unwrap();
    let server = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(PATH, agent)
        .unwrap()
        .build()
        .unwrap();
    let destination = server.unique_name().unwrap().to_string();
    let selected = OwnedObjectPath::try_from("/device/selected").unwrap();
    let result = spoof.call_method(
        Some(destination.as_str()),
        PATH,
        Some("org.bluez.Agent1"),
        "RequestPinCode",
        &(selected.clone(),),
    );
    assert!(
        matches!(result, Err(zbus::Error::MethodError(name, ..)) if name.as_str() == "org.bluez.Error.Rejected")
    );
    let target = destination.clone();
    let request = std::thread::spawn(move || {
        authority.call_method(
            Some(target.as_str()),
            PATH,
            Some("org.bluez.Agent1"),
            "RequestConfirmation",
            &(selected, 123456u32),
        )
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), events.next())
            .await
            .unwrap()
            .unwrap();
        let Update::Prompt(request) = event else {
            panic!("expected the authority's confirmation");
        };
        request.answer(Some(Password::default()));
    });
    request.join().unwrap().unwrap();
}

struct ConnectionState {
    mode: String,
    started: AtomicBool,
    connected: AtomicBool,
    trusted: AtomicBool,
    disconnects: AtomicU64,
    pairing_cancels: AtomicU64,
    ready: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct ConnectingDevice(Arc<ConnectionState>);
#[zbus::interface(name = "org.bluez.Device1")]
impl ConnectingDevice {
    #[zbus(property)]
    fn connected(&self) -> bool {
        self.0.connected.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn paired(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn trusted(&self) -> bool {
        self.0.trusted.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn set_trusted(&self, value: bool) {
        self.0.trusted.store(value, Ordering::SeqCst);
    }
    fn pair(&self) {}
    fn cancel_pairing(&self) {
        self.0.pairing_cancels.fetch_add(1, Ordering::SeqCst);
    }
    async fn connect(&self) -> zbus::fdo::Result<()> {
        self.0.started.store(true, Ordering::SeqCst);
        self.0.ready.notify_one();
        self.0.release.notified().await;
        if self.0.mode == "interrupt" {
            Err(zbus::fdo::Error::Failed("connection canceled".into()))
        } else {
            // Reproduce a success crossing the first Disconnect call.
            self.0.connected.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
    fn disconnect(&self) -> zbus::fdo::Result<()> {
        self.0.disconnects.fetch_add(1, Ordering::SeqCst);
        self.0.release.notify_one();
        if self.0.mode == "refuse" {
            Err(zbus::fdo::Error::Failed(
                "fixture refused disconnection".into(),
            ))
        } else {
            self.0.connected.store(false, Ordering::SeqCst);
            Ok(())
        }
    }
    async fn wait_for_connect(&self) {
        while !self.0.started.load(Ordering::SeqCst) {
            self.0.ready.notified().await;
        }
    }
}

struct AgentManager;
#[zbus::interface(name = "org.bluez.AgentManager1")]
impl AgentManager {
    fn register_agent(&self, _agent: OwnedObjectPath, _capability: String) {}
}

#[test]
fn private_bus_cancel_connect_interrupts_inflight_and_late_success_and_reports_cleanup_failure() {
    const CHILD: &str = "FERESE_BLUETOOTH_CANCEL_PROBE";
    if let Ok(mode) = std::env::var(CHILD) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (send, cancel) = oneshot::channel();
                let stream = connect(
                    BluetoothDevice {
                        path: "/device/selected".into(),
                        name: "Test".into(),
                        paired: false,
                        connected: false,
                    },
                    cancel,
                );
                cosmic::iced::futures::pin_mut!(stream);
                let dismiss = async {
                    let connection = zbus::Connection::system().await.unwrap();
                    connection
                        .call_method(
                            Some("org.bluez"),
                            "/device/selected",
                            Some("org.bluez.Device1"),
                            "WaitForConnect",
                            &(),
                        )
                        .await
                        .unwrap();
                    send.send(()).unwrap();
                };
                let outcome = async {
                    loop {
                        match stream
                            .next()
                            .await
                            .expect("connection stream ended before completion")
                        {
                            Update::Finished(result) => {
                                assert!(mode != "refuse");
                                assert!(result.is_err());
                                break;
                            }
                            Update::CancellationFailed(error) => {
                                assert_eq!(mode, "refuse");
                                assert!(error.contains("fixture refused disconnection"));
                                break;
                            }
                            _ => {}
                        }
                    }
                };
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    cosmic::iced::futures::join!(dismiss, outcome);
                })
                .await
                .unwrap();
            });
        return;
    }
    for mode in ["interrupt", "late", "refuse"] {
        let bus = crate::status::tests::TestBus::new();
        let state = Arc::new(ConnectionState {
            mode: mode.into(),
            started: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            trusted: AtomicBool::new(false),
            disconnects: AtomicU64::new(0),
            pairing_cancels: AtomicU64::new(0),
            ready: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let _server = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.bluez")
            .unwrap()
            .serve_at("/org/bluez", AgentManager)
            .unwrap()
            .serve_at("/device/selected", ConnectingDevice(state.clone()))
            .unwrap()
            .build()
            .unwrap();
        // Exercise the production system-bus path in a subprocess, without mutating
        // this test process's environment or contacting the host BlueZ service.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "status::wireless::pairing::tests::private_bus_cancel_connect_interrupts_inflight_and_late_success_and_reports_cleanup_failure", "--nocapture"])
            .env(CHILD, mode).env("DBUS_SYSTEM_BUS_ADDRESS", &bus.address)
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Bluetooth cancellation timed out: {mode}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(state.connected.load(Ordering::SeqCst), mode == "refuse");
        assert!(state.disconnects.load(Ordering::SeqCst) >= 2);
        assert_eq!(
            state.pairing_cancels.load(Ordering::SeqCst),
            0,
            "Connect must be canceled with Disconnect"
        );
        assert!(!state.trusted.load(Ordering::SeqCst));
    }
}
