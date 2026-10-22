use std::sync::{Arc, Mutex, mpsc};

use ferese_protocols::effects::v1::server::{
    ferese_effects_manager_v1 as manager, ferese_surface_effects_v1 as effects,
};
use wayland_server::protocol::{wl_compositor, wl_surface};
use wayland_server::{Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New};

#[derive(Default)]
struct Counts {
    managers: usize,
    manager_destroys: usize,
    surfaces: usize,
    effect_destroys: usize,
}

struct Server(Arc<Mutex<Counts>>);

impl GlobalDispatch<wl_compositor::WlCompositor, ()> for Server {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<wl_compositor::WlCompositor>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for Server {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_compositor::WlCompositor,
        request: wl_compositor::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let wl_compositor::Request::CreateSurface { id } = request {
            init.init(id, ());
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for Server {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_surface::WlSurface,
        _: wl_surface::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<manager::FereseEffectsManagerV1, ()> for Server {
    fn bind(
        state: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<manager::FereseEffectsManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        state.0.lock().unwrap().managers += 1;
        init.init(resource, ());
    }
}

impl Dispatch<manager::FereseEffectsManagerV1, ()> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &manager::FereseEffectsManagerV1,
        request: manager::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            manager::Request::GetSurfaceEffects { id, .. } => {
                state.0.lock().unwrap().surfaces += 1;
                init.init(id, ());
            }
            manager::Request::Destroy => state.0.lock().unwrap().manager_destroys += 1,
            _ => unreachable!(),
        }
    }
}

impl Dispatch<effects::FereseSurfaceEffectsV1, ()> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &effects::FereseSurfaceEffectsV1,
        request: effects::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, effects::Request::Destroy) {
            state.0.lock().unwrap().effect_destroys += 1;
        }
    }
}

wayland_client::delegate_noop!(super::EffectsState: ignore wayland_client::protocol::wl_compositor::WlCompositor);
wayland_client::delegate_noop!(super::EffectsState: ignore wayland_client::protocol::wl_surface::WlSurface);

#[test]
fn popup_effects_share_the_registry_and_destroy_every_protocol_object() {
    effects_lifecycle(true);
}

#[test]
fn missing_effects_extension_reuses_the_registry_across_failed_attachments() {
    effects_lifecycle(false);
}

fn effects_lifecycle(advertise: bool) {
    use wayland_client::protocol::wl_compositor::WlCompositor;
    let counts = Arc::new(Mutex::new(Counts::default()));
    let worker_counts = counts.clone();
    let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    let (stop, stopped) = mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        let mut display = Display::<Server>::new().unwrap();
        let mut handle = display.handle();
        handle.create_global::<Server, wl_compositor::WlCompositor, _>(1, ());
        if advertise {
            handle.create_global::<Server, manager::FereseEffectsManagerV1, _>(6, ());
        }
        handle.insert_client(server, Arc::new(())).unwrap();
        let mut state = Server(worker_counts);
        loop {
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            match stopped.recv_timeout(std::time::Duration::from_millis(1)) {
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                _ => break,
            }
        }
    });
    let connection = wayland_client::Connection::from_socket(client).unwrap();
    let (globals, queue) =
        wayland_client::globals::registry_queue_init::<super::EffectsState>(&connection).unwrap();
    let compositor = globals
        .bind::<WlCompositor, _, _>(&queue.handle(), 1..=1, ())
        .unwrap();
    let mut context = None;
    let mut first = None;
    for _ in 0..20 {
        let surface = compositor.create_surface(&queue.handle(), ());
        use wayland_client::Proxy;
        // Match window::run: each native callback lends the same display through
        // a fresh borrowed backend. Neither wrapper owns the actual display.
        // SAFETY: connection retains the live display until the borrowed backend and surface are
        // dropped.
        let borrowed = wayland_client::Connection::from_backend(unsafe {
            wayland_client::backend::Backend::from_foreign_display(
                connection.backend().display_ptr(),
            )
        });
        assert_ne!(borrowed, connection);
        // SAFETY: surface is a live wl_surface on the same display retained by connection.
        let id = unsafe {
            wayland_client::backend::ObjectId::from_ptr(
                wayland_client::protocol::wl_surface::WlSurface::interface(),
                surface.id().as_ptr(),
            )
        }
        .unwrap();
        let borrowed_surface =
            wayland_client::protocol::wl_surface::WlSurface::from_id(&borrowed, id).unwrap();
        let binding = super::EffectsBinding::attach_role(&mut context, &borrowed_surface, None, 1.);
        assert_eq!(binding.is_ok(), advertise);
        if let Some(first) = &first {
            assert!(std::rc::Rc::ptr_eq(first, context.as_ref().unwrap()));
        } else {
            first = context.clone();
        }
        // Parent surface lifetime does not release the effects protocol object.
        surface.destroy();
        drop(binding);
    }
    connection.roundtrip().unwrap();
    {
        let counts = counts.lock().unwrap();
        assert_eq!(counts.managers, usize::from(advertise));
        assert_eq!(counts.surfaces, if advertise { 20 } else { 0 });
        assert_eq!(counts.effect_destroys, counts.surfaces);
        assert_eq!(counts.manager_destroys, 0);
    }
    drop(first);
    drop(context);
    connection.roundtrip().unwrap();
    assert_eq!(
        counts.lock().unwrap().manager_destroys,
        usize::from(advertise)
    );
    drop(stop);
    worker.join().unwrap();
}
