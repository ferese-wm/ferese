//! One persistent connection observes sink, hardware-route and default-device changes.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use pipewire as pw;
use pw::proxy::{Listener, ProxyT};
use pw::spa::param::ParamType;
use pw::types::ObjectType;

use super::super::PollState;

pub(super) struct State {
    revision: AtomicU64,
    alive: AtomicBool,
    wake: Arc<PollState>,
}

impl State {
    pub(super) fn changed(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        // Synchronize with the poller's predicate check and condvar wait.
        super::super::wake_status(&self.wake);
    }

    pub(super) fn set_alive(&self, alive: bool) {
        if self.alive.swap(alive, Ordering::AcqRel) != alive {
            self.changed();
        }
    }
}

#[derive(Default)]
struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
    sender: Mutex<Option<pw::channel::Sender<()>>>,
}

pub(super) struct Signals {
    pub(super) state: Arc<State>,
    stop: Arc<Stop>,
}

impl Signals {
    pub(super) fn start(wake: Arc<PollState>) -> Self {
        Self::start_remote(wake, None)
    }

    fn start_remote(wake: Arc<PollState>, remote: Option<String>) -> Self {
        let signals = Self::disconnected(wake);
        let state = signals.state.clone();
        let stop = signals.stop.clone();
        let _ = std::thread::Builder::new()
            .name("ferese-audio-events".into())
            .spawn(move || {
                pw::init();

                loop {
                    if *stop.stopped.lock().unwrap() {
                        break;
                    }

                    let _ = monitor(state.clone(), &stop, remote.as_deref());
                    state.set_alive(false);
                    *stop.sender.lock().unwrap() = None;
                    let stopped = stop.stopped.lock().unwrap();
                    let (stopped, _) = stop
                        .wake
                        .wait_timeout_while(stopped, Duration::from_secs(5), |value| !*value)
                        .unwrap();

                    if *stopped {
                        break;
                    }
                }
            });
        signals
    }

    fn disconnected(wake: Arc<PollState>) -> Self {
        Self {
            state: Arc::new(State {
                revision: AtomicU64::new(0),
                alive: AtomicBool::new(false),
                wake,
            }),
            stop: Arc::new(Stop::default()),
        }
    }

    pub(super) fn revision(&self) -> u64 {
        self.state.revision.load(Ordering::Acquire)
    }

    pub(super) fn alive(&self) -> bool {
        self.state.alive.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn test(wake: Arc<PollState>) -> Self {
        let signals = Self::disconnected(wake);
        signals.state.set_alive(true);
        signals
    }

    #[cfg(test)]
    pub(super) fn changed(&self) {
        self.state.changed();
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        *self.stop.stopped.lock().unwrap() = true;
        self.stop.wake.notify_one();

        if let Some(sender) = self.stop.sender.lock().unwrap().as_ref() {
            let _ = sender.send(());
        }
    }
}

struct Bound {
    _listener: Box<dyn Listener>,
    _proxy: Box<dyn ProxyT>,
}

fn parameter_events(
    state: Arc<State>,
) -> impl Fn(i32, ParamType, u32, u32, Option<&pw::spa::pod::Pod>) {
    let values = RefCell::new(HashMap::<u32, Vec<u8>>::new());
    move |_, _, index, _, param| {
        let bytes = param.map_or(&[][..], pw::spa::pod::Pod::as_bytes);
        let mut values = values.borrow_mut();

        if values
            .get(&index)
            .is_none_or(|previous| previous.as_slice() != bytes)
        {
            values.insert(index, bytes.to_vec());
            state.changed();
        }
    }
}

fn bind(
    registry: &pw::registry::Registry,
    object: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    state: Arc<State>,
) -> Result<Option<Bound>, pw::Error> {
    let class = object.props.and_then(|props| props.get("media.class"));
    let bound = match object.type_ {
        ObjectType::Node if class == Some("Audio/Sink") => {
            let node: pw::node::Node = registry.bind(object)?;
            let info_state = state.clone();
            let listener = node
                .add_listener_local()
                .info(move |_| info_state.changed())
                .param(parameter_events(state))
                .register();
            node.subscribe_params(&[ParamType::Props]);
            Bound {
                _listener: Box::new(listener),
                _proxy: Box::new(node),
            }
        }
        ObjectType::Device if class == Some("Audio/Device") => {
            let device: pw::device::Device = registry.bind(object)?;
            let listener = device
                .add_listener_local()
                .param(parameter_events(state))
                .register();
            device.subscribe_params(&[ParamType::Route]);
            Bound {
                _listener: Box::new(listener),
                _proxy: Box::new(device),
            }
        }
        ObjectType::Metadata
            if object.props.and_then(|props| props.get("metadata.name")) == Some("default") =>
        {
            let metadata: pw::metadata::Metadata = registry.bind(object)?;
            let listener = metadata
                .add_listener_local()
                .property(move |subject, key, _, _| {
                    if subject == 0
                        && matches!(
                            key,
                            None | Some("default.audio.sink" | "default.configured.audio.sink")
                        )
                    {
                        state.changed();
                    }

                    0
                })
                .register();
            Bound {
                _listener: Box::new(listener),
                _proxy: Box::new(metadata),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(bound))
}

fn monitor(state: Arc<State>, stop: &Stop, remote: Option<&str>) -> Result<(), pw::Error> {
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let properties =
        remote.map(|remote| pw::properties::properties! { *pw::keys::REMOTE_NAME => remote });
    let core = context.connect_rc(properties)?;
    let registry = core.get_registry_rc()?;
    let objects = Rc::new(RefCell::new(HashMap::new()));
    let weak_registry = registry.downgrade();
    let weak_loop = mainloop.downgrade();
    let add_objects = objects.clone();
    let add_state = state.clone();
    let remove_state = state.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |object| {
            if let Some(registry) = weak_registry.upgrade() {
                match bind(&registry, object, add_state.clone()) {
                    Ok(Some(bound)) => {
                        add_objects.borrow_mut().insert(object.id, bound);
                        add_state.changed();
                    }
                    Ok(None) => {}
                    Err(_) => {
                        if let Some(mainloop) = weak_loop.upgrade() {
                            mainloop.quit();
                        }
                    }
                }
            }
        })
        .global_remove(move |id| {
            if objects.borrow_mut().remove(&id).is_some() {
                remove_state.changed();
            }
        })
        .register();
    let pending = core.sync(0)?;
    let ready_state = state.clone();
    let weak_loop = mainloop.downgrade();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                ready_state.set_alive(true);
            }
        })
        .error(move |_, _, _, _| {
            // A rejected subscription is also a loss of coverage. Fall back to
            // bounded reads rather than trusting a partially observed graph.
            state.set_alive(false);

            if let Some(mainloop) = weak_loop.upgrade() {
                mainloop.quit();
            }
        })
        .register();
    let (sender, receiver) = pw::channel::channel();
    let weak_loop = mainloop.downgrade();
    let _receiver = receiver.attach(mainloop.loop_(), move |_| {
        if let Some(mainloop) = weak_loop.upgrade() {
            mainloop.quit();
        }
    });
    *stop.sender.lock().unwrap() = Some(sender);

    if !*stop.stopped.lock().unwrap() {
        mainloop.run();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    use super::*;

    struct Server(Child);

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_for(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);

        while !predicate() {
            assert!(Instant::now() < deadline, "audio subscription timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[ignore = "requires pipewire, pw-cli and pw-metadata; starts a private audio server"]
    fn native_subscription_handles_idle_changes_disconnect_reconnect_and_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("pipewire/pipewire.conf.d");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("test.conf"),
            r#"
context.objects = [
    { factory = metadata args = { metadata.name = default } }
    { factory = adapter args = {
        factory.name = support.null-audio-sink
        node.name = test-sink
        media.class = Audio/Sink
        audio.position = [ FL FR ]
    } }
]
"#,
        )
        .unwrap();
        let mut command = Command::new("pipewire");
        command
            .env("XDG_RUNTIME_DIR", root.path())
            .env("PIPEWIRE_RUNTIME_DIR", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut server = Server(command.spawn().unwrap());
        wait_for(|| root.path().join("pipewire-0").exists());
        let wake = Arc::new((Mutex::new((0, false, None, false, 0)), Condvar::new()));
        let signals = Signals::start_remote(
            wake,
            Some(root.path().join("pipewire-0").to_str().unwrap().into()),
        );
        wait_for(|| signals.alive());
        std::thread::sleep(Duration::from_millis(500));
        let revision = signals.revision();
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(
            signals.revision(),
            revision,
            "idle audio emitted repeated updates"
        );
        let run = |program: &str, args: &[&str]| {
            let output = Command::new(program)
                .args(args)
                .env("PIPEWIRE_RUNTIME_DIR", root.path())
                .env("XDG_RUNTIME_DIR", root.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        run(
            "pw-metadata",
            &[
                "-n",
                "default",
                "0",
                "default.audio.sink",
                r#"{"name":"test-sink"}"#,
            ],
        );
        wait_for(|| signals.revision() > revision);
        let graph: serde_json::Value = serde_json::from_slice(&run("pw-dump", &[])).unwrap();
        let id = graph
            .as_array()
            .unwrap()
            .iter()
            .find(|object| object["info"]["props"]["node.name"] == "test-sink")
            .unwrap()["id"]
            .to_string();
        let revision = signals.revision();
        run(
            "pw-metadata",
            &[
                "-n",
                "default",
                "0",
                "default.audio.source",
                r#"{"name":"irrelevant-source"}"#,
            ],
        );
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            signals.revision(),
            revision,
            "source changes must not refresh sink status"
        );
        run("pw-cli", &["set-param", &id, "Props", "{ mute = true }"]);
        wait_for(|| signals.revision() > revision);
        let revision = signals.revision();
        run(
            "pw-cli",
            &[
                "set-param",
                &id,
                "Props",
                "{ channelVolumes = [ 0.125 0.125 ] }",
            ],
        );
        wait_for(|| signals.revision() > revision);
        let revision = signals.revision();
        run("pw-cli", &["destroy", &id]);
        wait_for(|| signals.revision() > revision);
        server.0.kill().unwrap();
        server.0.wait().unwrap();
        wait_for(|| !signals.alive());
        server = Server(command.spawn().unwrap());
        wait_for(|| signals.alive());
        let state = signals.state.clone();
        drop(signals);
        wait_for(|| !state.alive.load(Ordering::Acquire));
        drop(server);
    }
}
