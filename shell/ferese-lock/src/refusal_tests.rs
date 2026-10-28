//! Drive a real protocol refusal into the locker's termination path.
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use cosmic::Application;
use cosmic::app::Core;
use cosmic::iced::Event;
use cosmic::iced::event::{PlatformSpecific, wayland};
use wayland_client::globals::{self, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_v1::{
    self, ExtSessionLockV1,
};
use wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::{
    self as server_manager, ExtSessionLockManagerV1 as ServerManager,
};
use wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::{
    self as server_lock, ExtSessionLockV1 as ServerLock,
};
use wayland_server::{Client, DataInit, Display, DisplayHandle, GlobalDispatch, New};

use crate::password::Password;
use crate::{Flags, Locker, runtime};

struct RefusingCompositor;
impl GlobalDispatch<ServerManager, ()> for RefusingCompositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        manager: New<ServerManager>,
        _: &(),
        data: &mut DataInit<'_, Self>,
    ) {
        data.init(manager, ());
    }
}
impl wayland_server::Dispatch<ServerManager, ()> for RefusingCompositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ServerManager,
        request: server_manager::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let server_manager::Request::Lock { id } = request {
            data.init(id, ()).finished();
        }
    }
}
impl wayland_server::Dispatch<ServerLock, ()> for RefusingCompositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ServerLock,
        _: server_lock::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

struct LockClient {
    locker: Locker,
    refused: bool,
}
impl Dispatch<WlRegistry, GlobalListContents> for LockClient {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<ExtSessionLockManagerV1, ()> for LockClient {
    fn event(
        _: &mut Self,
        _: &ExtSessionLockManagerV1,
        _: wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<ExtSessionLockV1, ()> for LockClient {
    fn event(
        client: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, ext_session_lock_v1::Event::Finished) {
            client.refused = true;
            let _ = client
                .locker
                .handle_event(Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::SessionLock(wayland::SessionLockEvent::Finished),
                )));
        }
    }
}

#[test]
fn advertised_protocol_refusal_does_not_report_success() {
    let (server, socket) = UnixStream::pair().expect("private Wayland sockets");
    let mut display = Display::<RefusingCompositor>::new().expect("fixture display");
    let mut handle = display.handle();
    handle.create_global::<RefusingCompositor, ServerManager, _>(1, ());
    handle
        .insert_client(server, Arc::new(()))
        .expect("fixture client");
    let running = Arc::new(AtomicBool::new(true));
    let active = running.clone();
    let worker = thread::spawn(move || {
        // Bound the fixture even if the client fails to send its request.
        for _ in 0..1000 {
            if !active.load(Ordering::Acquire) {
                break;
            }
            display
                .dispatch_clients(&mut RefusingCompositor)
                .expect("dispatch fixture");
            display.flush_clients().expect("flush fixture");
            thread::sleep(Duration::from_millis(5));
        }
    });
    let connection = Connection::from_socket(socket).expect("client connection");
    let (globals, mut queue) =
        globals::registry_queue_init::<LockClient>(&connection).expect("registry roundtrip");
    let manager: ExtSessionLockManagerV1 = globals
        .bind(&queue.handle(), 1..=1, ())
        .expect("advertised lock manager");
    let (locker, _) = Locker::init(
        Core::default(),
        Flags {
            password: Password::try_new().expect("locked test buffer"),
            preview: false,
            user: "test-locker".into(),
            config: None,
            completed: Arc::new(AtomicBool::new(false)),
        },
    );
    let mut client = LockClient {
        locker,
        refused: false,
    };
    let _lock = manager.lock(&queue.handle(), ());
    queue
        .roundtrip(&mut client)
        .expect("lock request roundtrip");
    running.store(false, Ordering::Release);
    worker.join().expect("fixture server finished");
    assert!(
        client.refused,
        "the fixture must send finished through the wire"
    );
    assert!(!client.locker.confirmation.confirmed());
    assert!(runtime::check_completion(client.locker.completed.load(Ordering::Acquire)).is_err());
}
