//! Render-node timeline tests never create KMS surfaces or change a live session.
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::renderer::ContextId;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::utils::{Buffer, with_renderer_surface_state};
use smithay::reexports::drm::control::{Device, from_u32, syncobj};
use smithay::reexports::wayland_protocols::wp::linux_drm_syncobj::v1::server::wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::DeviceFd;
use smithay::wayland::GlobalData;
use smithay::wayland::drm_syncobj::{DrmSyncobjState, DrmTimeline};
use smithay::wayland::compositor::with_states;
use smithay::wayland::drm_syncobj::DrmSyncobjCachedState;

use crate::Ferese;
use crate::state::test_client_request as request;

fn render_node() -> DrmDeviceFd {
    let path =
        std::env::var("FERESE_TEST_RENDER_NODE").unwrap_or_else(|_| "/dev/dri/renderD128".into());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)))
}

fn handles(device: &DrmDeviceFd) -> Vec<u32> {
    // This independently opened file has only this test's syncobj handles.
    // Probe a bounded range containing the source plus the 512 permitted imports.
    (1..=1024)
        .filter(|value| {
            let handle = from_u32::<syncobj::Handle>(*value).unwrap();
            device
                .syncobj_timeline_query(&[handle], &mut [0], false)
                .is_ok()
        })
        .collect()
}

fn manager(
    state: &mut Ferese,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    device: &DrmDeviceFd,
) -> WpLinuxDrmSyncobjManagerV1 {
    state.buffer_sync.explicit.state = Some(DrmSyncobjState::new::<Ferese>(
        &state.display_handle,
        device.clone(),
    ));
    surface
        .client()
        .unwrap()
        .create_resource::<WpLinuxDrmSyncobjManagerV1, GlobalData, Ferese>(
            &state.display_handle,
            1,
            GlobalData,
        )
        .unwrap()
}

fn messages(wire: &mut UnixStream) -> Vec<(u32, u16, Vec<u32>)> {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    let mut offset = 0;
    let mut result = Vec::new();
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let size = (header >> 16) as usize;
        let args = bytes[offset + 8..offset + size]
            .chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
            .collect();
        result.push((object, header as u16, args));
        offset += size;
    }
    result
}

#[test]
#[ignore = "requires a DRM render node; only test-owned timeline ioctls are issued"]
fn imported_handle_is_released_only_after_the_last_committed_point_reference() {
    let device = render_node();
    let source = device.create_syncobj(false).unwrap();
    let before = handles(&device);
    assert_eq!(before, [u32::from(source)]);
    let timeline = DrmTimeline::new(&device, device.syncobj_to_fd(source, false).unwrap()).unwrap();
    assert_eq!(handles(&device).len(), before.len() + 1);
    let point = timeline.point(3);
    drop(timeline);
    assert_eq!(handles(&device).len(), before.len() + 1);
    point.signal().unwrap();
    drop(point);
    assert_eq!(handles(&device), before);
    device.destroy_syncobj(source).unwrap();
}

#[test]
#[ignore = "requires a DRM render node; isolated native socket and test-owned timeline handles"]
fn native_import_destroy_loops_reclaim_imported_handles() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::syncobj_lifetime_tests::native_import_destroy_loops_reclaim_imported_handles",
    ) {
        return;
    }
    let device = render_node();
    let source = device.create_syncobj(false).unwrap();
    let exported = device.syncobj_to_fd(source, false).unwrap();
    let before = handles(&device);
    let (mut state, mut events, surface, mut wire) = super::tests::fixture();
    let manager = manager(&mut state, &surface, &device);
    for index in 0..64 {
        let id = 7 + index;
        request(
            &mut wire,
            manager.id().protocol_id(),
            2,
            &[id],
            Some(exported.as_raw_fd()),
        );
        request(&mut wire, id, 0, &[], None);
        if index % 8 == 7 {
            super::tests::dispatch(&mut state, &mut events);
            assert_eq!(handles(&device), before);
        }
    }
    assert!(surface.is_alive());
    assert_eq!(handles(&device), before);
    device.destroy_syncobj(source).unwrap();
}

#[test]
#[ignore = "requires a DRM render node; tests native timeline admission without modesetting"]
fn native_live_import_cap_reclaims_the_abusive_client_and_preserves_another_owner() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::syncobj_lifetime_tests::native_live_import_cap_reclaims_the_abusive_client_and_preserves_another_owner",
    ) {
        return;
    }
    let device = render_node();
    let source = device.create_syncobj(false).unwrap();
    let exported = device.syncobj_to_fd(source, false).unwrap();
    let before = handles(&device);
    let (mut state, mut events, surface, mut wire) = super::tests::fixture();
    let first_manager = manager(&mut state, &surface, &device);
    for index in 0..544 {
        request(
            &mut wire,
            first_manager.id().protocol_id(),
            2,
            &[7 + index],
            Some(exported.as_raw_fd()),
        );
        if index % 8 == 7 {
            super::tests::dispatch(&mut state, &mut events);
            if !surface.is_alive() {
                break;
            }
        }
    }
    assert!(!surface.is_alive());
    assert!(
        messages(&mut wire)
            .iter()
            .any(|(object, opcode, args)| *object == 1
                && *opcode == 0
                && args.get(..2) == Some(&[first_manager.id().protocol_id(), 1]))
    );
    assert_eq!(handles(&device), before);
    let (window, mut healthy) = crate::state::test_window(&mut state, &mut events, 0xff445566);
    let healthy_surface = window.toplevel().unwrap().wl_surface();
    // Keep the same manager state and its global/client accounting.
    let healthy_manager = healthy_surface
        .client()
        .unwrap()
        .create_resource::<WpLinuxDrmSyncobjManagerV1, GlobalData, Ferese>(
            &state.display_handle,
            1,
            GlobalData,
        )
        .unwrap();
    request(
        &mut healthy,
        healthy_manager.id().protocol_id(),
        2,
        &[7],
        Some(exported.as_raw_fd()),
    );
    request(&mut healthy, 7, 0, &[], None);
    request(&mut healthy, 1, 0, &[8], None);
    super::tests::dispatch(&mut state, &mut events);
    assert!(
        messages(&mut healthy)
            .iter()
            .any(|(object, opcode, _)| *object == 8 && *opcode == 0)
    );
    assert!(healthy_surface.is_alive());
    assert_eq!(handles(&device), before);
    device.destroy_syncobj(source).unwrap();
}

#[test]
#[ignore = "requires a DRM render node; native retained-point/global retirement without modesetting"]
fn destroyed_timeline_protocol_objects_remain_tracked_until_retained_points_retire() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::syncobj_lifetime_tests::destroyed_timeline_protocol_objects_remain_tracked_until_retained_points_retire",
    ) {
        return;
    }
    let device = render_node();
    let source = device.create_syncobj(false).unwrap();
    let exported = device.syncobj_to_fd(source, false).unwrap();
    let before = handles(&device);
    let (mut state, mut events, surface, mut wire) = super::tests::fixture();
    let manager = manager(&mut state, &surface, &device);
    request(&mut wire, manager.id().protocol_id(), 1, &[7, 2], None);
    request(
        &mut wire,
        manager.id().protocol_id(),
        2,
        &[8],
        Some(exported.as_raw_fd()),
    );
    request(&mut wire, 7, 1, &[8, 0, 1], None);
    request(&mut wire, 7, 2, &[8, 0, 2], None);
    request(&mut wire, 8, 0, &[], None);
    super::tests::dispatch(&mut state, &mut events);
    let point = with_states(&surface, |states| {
        states
            .cached_state
            .get::<DrmSyncobjCachedState>()
            .pending()
            .acquire_point
            .clone()
            .unwrap()
    });
    point.signal().unwrap(); // Destroying the protocol object preserves its points.
    assert_eq!(handles(&device).len(), before.len() + 1);
    let retired = state.buffer_sync.explicit.state.take().unwrap();
    state
        .display_handle
        .disable_global::<Ferese>(retired.into_global());
    assert!(
        point.signal().is_err(),
        "global/device retirement must still visit retained points"
    );
    assert_eq!(handles(&device), before);
    drop(point);
    device.destroy_syncobj(source).unwrap();
}

#[test]
#[ignore = "requires a DRM render node for explicit points; no GPU drawing or modesetting"]
fn sampled_contexts_follow_each_explicit_attachment_when_a_wl_buffer_is_reused() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::syncobj_lifetime_tests::sampled_contexts_follow_each_explicit_attachment_when_a_wl_buffer_is_reused",
    ) {
        return;
    }
    let device = render_node();
    let source = device.create_syncobj(false).unwrap();
    let timeline = DrmTimeline::new(&device, device.syncobj_to_fd(source, false).unwrap()).unwrap();
    let (mut state, mut events, surface, _wire) = super::tests::fixture();
    super::tests::dispatch(&mut state, &mut events);
    let ordinary = with_renderer_surface_state(&surface, |data| data.buffer().cloned())
        .flatten()
        .unwrap();
    let first_context = ContextId::<GlesTexture>::new().erased();
    let second_context = ContextId::<GlesTexture>::new().erased();
    ordinary.mark_sampled(first_context.clone());
    assert!(!ordinary.was_sampled_by(&first_context));
    let first = Buffer::with_explicit((*ordinary).clone(), timeline.point(1), timeline.point(2));
    let same = first.clone();
    let reused = Buffer::with_explicit((*ordinary).clone(), timeline.point(3), timeline.point(4));
    first.mark_sampled(first_context.clone());
    assert!(same.same_attachment(&first));
    assert!(same.was_sampled_by(&first_context));
    assert!(!same.was_sampled_by(&second_context));
    assert!(!reused.same_attachment(&first));
    assert!(!reused.was_sampled_by(&first_context));
    reused.mark_sampled(second_context.clone());
    assert!(reused.was_sampled_by(&second_context));
    assert!(!first.was_sampled_by(&second_context));
    device.destroy_syncobj(source).unwrap();
}
