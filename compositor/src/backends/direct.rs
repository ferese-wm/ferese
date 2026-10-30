use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io;
use std::os::fd::AsFd;
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) mod capture;
mod context;
mod crtc_assignment;
mod diagnostics;
pub(crate) mod feedback;
pub(crate) mod gamma;
mod input_resume;
pub(crate) mod leasing;
mod lid;
mod lid_policy;
mod mirror;
mod output_power;
mod planes;
mod presentation_mode;
mod protocol;
mod recovery;
mod redraw;
mod scheduling;
mod sdr_color;
mod session_device;
mod topology;
mod transaction;
use diagnostics::{Diagnostics, PrimaryOutcome};
use output_power::{OutputPower, Power};
pub(crate) use protocol::configure_outputs;
use redraw::{AnimationFallback, SchedulingMetrics};
use scheduling::{
    arm_frame, cancel_output_timers, monotonic_now, record_frame_schedule, request_frame,
    watch_render_ready,
};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Buffer, Fourcc};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmEventTime, DrmNode, NodeType};
use smithay::backend::egl::EGLDisplay;
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::{RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{ImportDma, ImportMemWl, Renderer};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu};
use smithay::desktop::layer_map_for_output;
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::{
    Mode as OutputMode, Output, OutputModeSource, PhysicalProperties, Scale, Subpixel,
};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, RegistrationToken};
use smithay::reexports::drm::control::{
    Device as ControlDevice, Mode as DrmMode, ModeTypeFlags, ResourceHandles, connector, crtc,
    property,
};
use smithay::reexports::input::{Device as LibinputDevice, Libinput};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::utils::{Monotonic, Time, Transform};
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use smithay::wayland::presentation::Refresh;
use topology::{ReconcileReason, Topology, reconcile_outputs};

use crate::Ferese;
use crate::config::{OutputModeRequest, OutputProfile, OutputSettings, OutputTransform};
use crate::metrics::RenderMetrics;
use crate::monitor_identity::{Identity, IdentityRegistry};
use crate::output_policy::{DesiredOutputConfiguration, ManualOverride, Monitor};
use crate::render::{frame_effect_metrics, sampled_output_elements};

pub struct DirectBackendState {
    pub session: LibSeatSession,
    pub active: bool,
    lid: lid::LidState,
    lid_reader: lid::Reader,
    topology: Topology,
    identities: IdentityRegistry,
    pub(crate) manual_outputs: ManualOverride,
    leasing: leasing::State,
    desired_outputs: DesiredOutputConfiguration,
    monitors: Vec<Monitor>,
    output_error: Option<String>,
    applied_outputs: DesiredOutputConfiguration,
    revert_outputs: Option<DesiredOutputConfiguration>,
    confirmation: Option<(DesiredOutputConfiguration, ManualOverride)>,
    confirmation_timer: Option<RegistrationToken>,
    confirmation_lid: bool,
    pub(crate) low_power: bool,
    devices: HashMap<DrmNode, DirectDevice>,
    input_context: Libinput,
    input_open_failures: input_resume::OpenFailures,
    input_sleep_offset: Option<input_resume::SleepOffset>,
    input_devices: Vec<LibinputDevice>,
    input_retry: Option<RegistrationToken>,
    presentation: HashMap<(DrmNode, crtc::Handle), PresentationClock>,
    pub(crate) connected_outputs: Vec<ConnectedOutputInfo>,
    battery_timer: Option<RegistrationToken>,
    power_retry: Option<RegistrationToken>,
    animation_timer: Option<RegistrationToken>,
    animation_deadline: Option<Instant>,
    scheduling_metrics: SchedulingMetrics,
}

impl DirectBackendState {
    pub(crate) fn protocol_output_name(&self, info: &ConnectedOutputInfo) -> String {
        protocol::output_name(&info.connector, &info.identity, &self.monitors)
    }
    pub(crate) fn physical_output(&self, identity: &str) -> Option<&Output> {
        self.devices
            .values()
            .flat_map(|device| device.outputs.values())
            .find(|output| output.identity == identity)
            .map(|output| &output.output)
    }

    pub(crate) fn physical_outputs(&self) -> impl Iterator<Item = &Output> {
        self.devices
            .values()
            .flat_map(|device| device.outputs.values().map(|output| &output.output))
    }

    pub(crate) fn output_configuration_error(&self) -> Option<&str> {
        self.output_error.as_deref()
    }

    pub(crate) fn confirmation_pending(&self) -> bool {
        self.confirmation.is_some()
    }

    pub(crate) fn reconciling(&self) -> bool {
        self.topology.reconciling
    }

    fn can_render(&self) -> bool {
        self.active && !self.topology.reconciling && !self.lid.pending()
    }

    pub(crate) fn process_dmabuf_imports(
        &mut self,
        imports: &mut crate::dmabuf_imports::ImportManager,
    ) {
        if !self.active || self.topology.reconciling {
            return;
        }

        let device_iter = self
            .devices
            .values_mut()
            .filter(|device| device.drm.is_active());
        for device in device_iter {
            imports.process(
                device.dmabuf_global,
                &mut device.renderer,
                Some(device.render_node),
            );
        }
    }

    pub(crate) fn scene_output(&self, state: &Ferese, output: &Output) -> Output {
        let source = self
            .devices
            .values()
            .flat_map(|device| device.outputs.values())
            .find(|candidate| &candidate.output == output)
            .and_then(|candidate| candidate.mirror_source.as_ref());
        source
            .and_then(|key| self.monitors.iter().find(|monitor| &monitor.key == key))
            .and_then(|monitor| state.output_by_identity(&monitor.identity))
            .cloned()
            .unwrap_or_else(|| output.clone())
    }

    pub(crate) fn dump_scheduling_metrics(&self) {
        self.scheduling_metrics.dump();
    }

    pub(crate) fn capture_resize_snapshot(
        &mut self,
        window: &smithay::desktop::Window,
        geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
        output: &Output,
        remaining: usize,
    ) -> Result<Option<crate::render::ResizeSnapshot>, smithay::backend::renderer::gles::GlesError>
    {
        let Some(device) = self.devices.values_mut().find(|device| {
            device
                .outputs
                .values()
                .any(|candidate| &candidate.output == output)
        }) else {
            return Ok(None);
        };
        crate::render::capture_resize_snapshot(
            &mut device.renderer,
            window,
            geometry,
            output.current_scale().fractional_scale(),
            remaining,
        )
    }

    pub(crate) fn clear_stream_capture_targets(&mut self) {
        for device in self.devices.values_mut() {
            crate::render::clear_stream_capture_targets(&mut device.renderer);
        }
    }

    pub(crate) fn capture_renderer(&mut self, output: &Output) -> Option<&mut GlesRenderer> {
        self.devices
            .values_mut()
            .find(|device| {
                device
                    .outputs
                    .values()
                    .any(|candidate| &candidate.output == output)
            })
            .map(|device| &mut device.renderer)
    }

    pub(crate) fn readback_renderer(
        &mut self,
        context: &smithay::backend::renderer::ErasedContextId,
    ) -> Option<&mut GlesRenderer> {
        self.devices
            .values_mut()
            .map(|device| &mut device.renderer)
            .find(|renderer| renderer.context_id().erased() == *context)
    }

    pub(crate) fn readback_contexts(&self) -> Vec<smithay::backend::renderer::ErasedContextId> {
        self.devices
            .values()
            .map(|device| device.renderer.context_id().erased())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConnectedOutputInfo {
    pub connector: String,
    pub connected: bool,
    pub identity: String,
    pub internal: bool,
    pub requested_enabled: bool,
    pub mirror_source: Option<String>,
    pub enabled: bool,
    pub profile: Option<String>,
    pub requested_profile: Option<String>,
    pub auto_refresh: bool,
    pub adaptive_sync_supported: bool,
    pub adaptive_sync_requested: bool,
    pub adaptive_sync_enabled: bool,
    pub tearing_supported: bool,
    pub allow_tearing: bool,
    pub physical_size: Option<(u32, u32)>,
    pub current_mode: Option<ConnectedModeInfo>,
    pub available_modes: Vec<ConnectedModeInfo>,
    pub scale: f64,
    pub transform: OutputTransform,
    pub configured_position: Option<[i32; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConnectedModeInfo {
    pub width: u16,
    pub height: u16,
    pub refresh_millihertz: i32,
    pub preferred: bool,
}

#[derive(Default)]
struct PresentationClock {
    last_presentation: Option<Duration>,
    presented_frames: u64,
    refresh_interval: Option<Duration>,
    missed_deadlines: u64,
}

struct FrameFeedback {
    feedback: OutputPresentationFeedback,
    tearing: bool,
    refresh: Refresh,
}

type OutputCompositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    FrameFeedback,
    DrmDeviceFd,
>;

// A successful probe replaces the complete inventory. Failed probes retain the
// previous snapshot, including disconnected heads, until discovery recovers.
struct ConnectorInventory {
    resources: ResourceHandles,
    connectors: Vec<DetectedConnector>,
}

struct DetectedConnector {
    info: connector::Info,
    desktop: bool,
    edid: Option<Vec<u8>>,
    adaptive_sync_supported: bool,
}

fn refresh_connector_inventory(device: &mut DirectDevice) -> io::Result<()> {
    let drm = &device.drm;
    let resources = drm.resource_handles()?;
    let connectors = resources
        .connectors()
        .iter()
        .map(|handle| {
            let info = drm.get_connector(*handle, true)?;
            let connected = info.state() == connector::State::Connected;
            let desktop = connected && !leasing::non_desktop(drm, &info)?;
            let edid = if connected {
                connector_edid(drm, &info)
            } else {
                None
            };
            let adaptive_sync_supported =
                desktop && presentation_mode::connector_capable(drm, &info)?;
            Ok(DetectedConnector {
                info,
                desktop,
                edid,
                adaptive_sync_supported,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    device.inventory = Some(ConnectorInventory {
        resources,
        connectors,
    });
    Ok(())
}

struct DirectDevice {
    inventory: Option<ConnectorInventory>,
    active_leases: Vec<smithay::wayland::drm_lease::DrmLease>,
    notifier: RegistrationToken,
    dmabuf_global: smithay::wayland::dmabuf::DmabufGlobal,
    connected_outputs: Vec<ConnectedOutputInfo>,
    drm: DrmDevice,
    gbm: GbmDevice<DrmDeviceFd>,
    renderer: GlesRenderer,
    render_node: DrmNode,
    outputs: HashMap<crtc::Handle, DirectOutput>,
    // Last: release libseat's original FD after renderer/DRM resources.
    _session_device: session_device::SessionDevice<LibSeatSession>,
}

struct DirectOutput {
    gamma: gamma::State,
    internal: bool,
    connector: connector::Handle,
    mode: DrmMode,
    settings: OutputSettings,
    output: Output,
    global: Option<GlobalId>,
    identity: String,
    mirror_source: Option<String>,
    mirror_texture: Option<capture::CaptureTarget>,
    mirror_materials: crate::render::MaterialResources,
    mirror_capture_texture: [Option<capture::CaptureTarget>; 2],
    mirror_canvas: Option<Output>,
    surface: OutputCompositor,
    primary_commit: Option<CommitCounter>,
    capture_texture: [Option<capture::CaptureTarget>; 2],
    render_metrics: RenderMetrics,
    diagnostics: Diagnostics,
    dmabuf_feedback: feedback::OutputFeedback,
    frame_pending: bool,
    completion: recovery::Completion,
    completion_timer: Option<RegistrationToken>,
    animation_fallback: AnimationFallback,
    cursor_visible: bool,
    scheduler: crate::frame_scheduler::FrameScheduler,
    render_timer: Option<RegistrationToken>,
    render_fence: Option<RegistrationToken>,
    power: OutputPower,
    callback_timer: Option<RegistrationToken>,
    lock_frame_pending: bool,
    tearing_supported: bool,
}

pub(crate) fn supports_tearing(state: &Ferese) -> bool {
    state.direct_backend.as_ref().is_some_and(|backend| {
        backend
            .devices
            .values()
            .flat_map(|device| device.outputs.values())
            .any(|output| output.tearing_supported && output.settings.allow_tearing)
    })
}

#[derive(Clone)]
struct OutputSelection {
    name: String,
    connector: connector::Info,
    crtc: crtc::Handle,
    mode: DrmMode,
    settings: OutputSettings,
    identity: String,
    mirror_source: Option<String>,
}

struct OutputScan {
    selections: Vec<OutputSelection>,
    connected_outputs: Vec<ConnectedOutputInfo>,
}

pub fn init(event_loop: &mut EventLoop<Ferese>, state: &mut Ferese) -> Result<(), Box<dyn Error>> {
    let (session, notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    let udev_backend = UdevBackend::new(&seat_name)?;
    let primary_path = primary_gpu(&seat_name)?
        .or_else(|| {
            udev_backend
                .device_list()
                .next()
                .map(|(_, path)| path.to_owned())
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no DRM device found"))?;
    let input_open_failures = input_resume::OpenFailures::default();
    let mut libinput_context = Libinput::new_with_udev(input_resume::Access {
        inner: LibinputSessionInterface::<LibSeatSession>::from(session.clone()),
        failures: input_open_failures.clone(),
    });
    libinput_context
        .udev_assign_seat(&seat_name)
        .map_err(|()| io::Error::other(format!("failed to assign libinput seat {seat_name}")))?;
    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());
    let session_active = session.is_active();
    if !session_active {
        libinput_context.suspend();
    }

    let lid_reader = lid::Reader::start(state)?;
    state.direct_backend = Some(DirectBackendState {
        session,
        active: session_active,
        lid: lid::LidState::default(),
        lid_reader,
        topology: Topology::default(),
        identities: IdentityRegistry::default(),
        manual_outputs: ManualOverride::default(),
        leasing: leasing::State::default(),
        desired_outputs: DesiredOutputConfiguration::default(),
        monitors: Vec::new(),
        output_error: None,
        applied_outputs: DesiredOutputConfiguration::default(),
        revert_outputs: None,
        confirmation: None,
        confirmation_timer: None,
        confirmation_lid: false,
        low_power: super::power::low_power(false, super::power::battery_percent()),
        devices: HashMap::new(),
        input_context: libinput_context,
        input_open_failures,
        input_sleep_offset: input_resume::SleepOffset::sample(),
        input_devices: Vec::new(),
        input_retry: None,
        presentation: HashMap::new(),
        connected_outputs: Vec::new(),
        battery_timer: None,
        power_retry: None,
        animation_timer: None,
        animation_deadline: None,
        scheduling_metrics: SchedulingMetrics::new(),
    });

    state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend")
        .topology
        .observe_devices(&[(
            primary_path.clone(),
            topology::device_identity(&primary_path),
        )]);
    lid::request_refresh(state, false);

    event_loop
        .handle()
        .insert_source(libinput_backend, |event, _, state| {
            process_libinput_event(state, event);
        })?;
    event_loop
        .handle()
        .insert_source(notifier, move |event, _, state| match event {
            SessionEvent::PauseSession => {
                state.reset_session_input();
                let handle = state.loop_handle.clone();
                if let Some(backend) = state.direct_backend.as_mut() {
                    backend.active = false;
                    if let Some(token) = backend.input_retry.take() {
                        handle.remove(token);
                    }
                    backend.input_context.suspend();
                    leasing::suspend(backend);
                    backend.lid.pause();
                    backend.lid_reader.set_active(false);
                    if let Some(token) = backend.lid_reader.deadline.take() {
                        handle.remove(token);
                    }
                    backend.topology.dirty = true;
                    backend.topology.coalescer.clear();
                    if let Some(token) = backend.topology.settle_timer.take() {
                        handle.remove(token);
                    }
                    if let Some(token) = backend.topology.retry_timer.take() {
                        handle.remove(token);
                    }
                    if let Some(token) = backend.battery_timer.take() {
                        handle.remove(token);
                    }
                    if let Some(token) = backend.power_retry.take() {
                        handle.remove(token);
                    }
                    for clock in backend.presentation.values_mut() {
                        clock.reset_timing();
                    }
                    for device in backend.devices.values_mut() {
                        for output in device.outputs.values_mut() {
                            output.gamma.suspend();
                            cancel_output_timers(&handle, output);
                            output.surface.reset_buffers();
                            output.dmabuf_feedback.invalidate();
                            output.primary_commit = None;
                            output.capture_texture = Default::default();
                            output.frame_pending = false;
                            output.power.suspended();
                        }
                        device.drm.pause();
                    }
                }

                state.display_presentation.clear();
                state.refresh_idle_inhibition();

                tracing::info!("direct session paused");
            }

            SessionEvent::ActivateSession => {
                let reopen_input = state
                    .direct_backend
                    .as_ref()
                    .is_some_and(|backend| !backend.active);
                if let Some(backend) = state.direct_backend.as_mut() {
                    backend.active = true;
                    backend.lid_reader.set_active(true);
                }
                if reopen_input {
                    input_resume::resume(state);
                }

                let seat = state.seat.clone();
                state.idle_notifier_state.notify_activity(&seat);
                lid::request_refresh(state, true);
                tracing::info!("direct session reacquired; refreshing external state");
            }
        })?;
    event_loop
        .handle()
        .insert_source(udev_backend, |event, _, state| {
            if let UdevEvent::Removed { device_id } = event
                && let Some(node) = direct_node_for_device(state, device_id)
                && let Some(backend) = state.direct_backend.as_mut()
            {
                // A remove/re-add may reuse the same dev_t. Retire the old fd
                // even if fresh enumeration already sees its replacement.
                backend.topology.invalidated.insert(node);
            }

            topology::hotplug(state);
        })?;

    tracing::info!(seat = %seat_name, "initialized direct session input and device discovery");
    Ok(())
}

fn process_libinput_event(state: &mut Ferese, event: InputEvent<LibinputInputBackend>) {
    match event {
        InputEvent::DeviceAdded { mut device } => {
            state.add_input_device(device.clone());
            configure_libinput_device(state, &mut device);
            if let Some(backend) = state.direct_backend.as_mut() {
                backend.input_devices.push(device);
            }
        }
        InputEvent::DeviceRemoved { device } => {
            state.remove_input_device(&device);
            if let Some(backend) = state.direct_backend.as_mut() {
                backend
                    .input_devices
                    .retain(|candidate| candidate != &device);
            }
        }
        event => {
            if state
                .direct_backend
                .as_ref()
                .is_some_and(|backend| backend.active)
            {
                state.process_input_event(event);
            }
        }
    }
}

fn configure_libinput_device(state: &Ferese, device: &mut LibinputDevice) {
    if device.has_capability(smithay::reexports::input::DeviceCapability::Keyboard)
        && let Some(keyboard) = state.seat.get_keyboard()
    {
        device.led_update(keyboard.led_state().into());
    }
    let settings = state.input_settings.touchpad;
    let touchpad = device.config_tap_finger_count() > 0 || device.config_dwt_is_available();
    let mouse = state.input_settings.mouse;
    if touchpad {
        if device.config_accel_is_available()
            && let Err(error) = device.config_accel_set_speed(settings.accel_speed)
        {
            tracing::warn!(device = %device.name(), ?error, "failed to configure touchpad acceleration");
        }
        use smithay::reexports::input::SendEventsMode;
        let mode = if settings.disable_while_mouse_connected {
            SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE
        } else {
            SendEventsMode::ENABLED
        };
        if device
            .config_send_events_modes()
            .contains(SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE)
            && let Err(error) = device.config_send_events_set_mode(mode)
        {
            tracing::warn!(device = %device.name(), ?error, "failed to configure touchpad external-mouse policy");
        }
    }
    if !touchpad && device.has_capability(smithay::reexports::input::DeviceCapability::Pointer) {
        if device.config_accel_is_available() {
            if let Err(error) = device.config_accel_set_speed(mouse.accel_speed) {
                tracing::warn!(device = %device.name(), ?error, "failed to configure mouse acceleration");
            }
            use smithay::reexports::input::AccelProfile;
            let profile = match mouse.accel_profile {
                Some(crate::config::AccelerationProfile::Adaptive) => Some(AccelProfile::Adaptive),
                Some(crate::config::AccelerationProfile::Flat) => Some(AccelProfile::Flat),
                None => device.config_accel_default_profile(),
            };
            if let Some(profile) =
                profile.filter(|profile| device.config_accel_profiles().contains(profile))
                && let Err(error) = device.config_accel_set_profile(profile)
            {
                tracing::warn!(device = %device.name(), ?error, "failed to configure mouse acceleration profile");
            }
        }
        if device.config_left_handed_is_available()
            && let Err(error) = device.config_left_handed_set(mouse.left_handed)
        {
            tracing::warn!(device = %device.name(), ?error, "failed to configure mouse handedness");
        }
        if device.config_middle_emulation_is_available()
            && let Err(error) = device.config_middle_emulation_set_enabled(mouse.middle_emulation)
        {
            tracing::warn!(device = %device.name(), ?error, "failed to configure middle button emulation");
        }
    }

    if device.config_tap_finger_count() > 0
        && let Err(error) = device.config_tap_set_enabled(settings.tap)
    {
        tracing::warn!(device = %device.name(), ?error, "failed to configure tap-to-click");
    }
    if device.config_scroll_has_natural_scroll()
        && let Err(error) = device.config_scroll_set_natural_scroll_enabled(if touchpad {
            settings.natural_scroll
        } else {
            mouse.natural_scroll
        })
    {
        tracing::warn!(device = %device.name(), ?error, "failed to configure natural scrolling");
    }
    if device.config_dwt_is_available()
        && let Err(error) = device.config_dwt_set_enabled(settings.disable_while_typing)
    {
        tracing::warn!(
            device = %device.name(),
            ?error,
            "failed to configure disable-while-typing"
        );
    }
}

pub(crate) fn update_keyboard_leds(state: &mut Ferese, leds: smithay::input::keyboard::LedState) {
    if let Some(backend) = state.direct_backend.as_mut() {
        for device in &mut backend.input_devices {
            if device.has_capability(smithay::reexports::input::DeviceCapability::Keyboard) {
                device.led_update(leds.into());
            }
        }
    }
}

pub(crate) fn reload_input_devices(state: &mut Ferese) {
    let Some(backend) = state.direct_backend.as_ref() else {
        return;
    };
    // Device clones refer to the same live libinput objects.
    let mut devices = backend.input_devices.clone();
    for device in &mut devices {
        configure_libinput_device(state, device);
    }
}

fn automatic_refresh_active(backend: &DirectBackendState) -> bool {
    backend.active
        && backend.devices.values().any(|device| {
            device
                .outputs
                .values()
                .any(|output| output.settings.auto_refresh)
        })
}

fn sync_battery_timer(state: &mut Ferese) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    if !automatic_refresh_active(backend) {
        if let Some(token) = backend.battery_timer.take() {
            state.loop_handle.remove(token);
        }
        return;
    }
    if backend.battery_timer.is_some() {
        return;
    }

    match state.loop_handle.insert_source(
        Timer::from_duration(Duration::from_secs(5)),
        |_, _, state| {
            if let Some(backend) = state.direct_backend.as_mut() {
                backend.battery_timer = None;
            }
            update_power_policy(state);
            sync_battery_timer(state);
            TimeoutAction::Drop
        },
    ) {
        Ok(token) => backend.battery_timer = Some(token),
        Err(error) => tracing::warn!(%error, "could not arm automatic refresh battery timer"),
    }
}

fn update_power_policy(state: &mut Ferese) {
    if !state
        .direct_backend
        .as_ref()
        .is_some_and(automatic_refresh_active)
    {
        return;
    }
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    // Keep low_power as the applied policy while paused. A transition sampled
    // during inactivity must still be detected and applied on activation.
    let battery = if backend.active {
        super::power::battery_percent()
    } else {
        None
    };
    if let Some(next) = super::power::policy_change(backend.active, backend.low_power, battery) {
        backend.low_power = next;
        reload_outputs(state, false);
    }
}

pub(crate) fn reload_outputs(state: &mut Ferese, layout_dirty: bool) {
    if let Some(backend) = state.direct_backend.as_mut() {
        backend.topology.layout_dirty |= layout_dirty;
    }
    reconcile_outputs(state, ReconcileReason::Configuration);
}

pub(crate) fn set_output_profile(state: &mut Ferese, name: &str) -> Result<(), String> {
    let backend = state
        .direct_backend
        .as_mut()
        .ok_or("monitor profiles require the direct DRM backend")?;
    if !backend.active || backend.lid.pending() {
        return Err("output session is inactive or refreshing lid state".into());
    }
    if name != "auto"
        && !state
            .output_profiles
            .iter()
            .any(|profile| profile.name == name)
    {
        return Err(format!("unknown output profile {name:?}"));
    }
    let previous = backend.manual_outputs.clone();
    let known_good = backend.applied_outputs.clone();
    backend.manual_outputs.profile = (name != "auto").then(|| name.to_owned());
    backend.manual_outputs.configuration = None;
    backend.manual_outputs.internal = None;
    backend.manual_outputs.layout = None;
    let desired = crate::output_policy::select_profile(
        &backend.monitors,
        backend.lid.closed(),
        &state.output_profiles,
        &backend.manual_outputs,
    );
    if name != "auto" && desired.profile.as_deref() != Some(name) {
        backend.manual_outputs = previous;
        return Err("profile does not match the connected monitors/lid".into());
    }
    if let Err(error) = validate_live_outputs(state, &state.output_profiles) {
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .manual_outputs = previous;
        return Err(error);
    }
    reconcile_outputs(state, ReconcileReason::Configuration);
    if let Some(error) = state
        .direct_backend
        .as_ref()
        .expect("DRM callbacks require an installed direct backend")
        .output_error
        .clone()
    {
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .manual_outputs = previous;
        return Err(error);
    }
    if name != "auto"
        && state
            .direct_backend
            .as_ref()
            .expect("DRM callbacks require an installed direct backend")
            .desired_outputs
            .profile
            .as_deref()
            != Some(name)
    {
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .manual_outputs = previous;
        return Err("profile does not match the connected monitors/lid".into());
    }
    arm_output_confirmation(state, known_good, previous);
    state.notify_monitor_state();
    Ok(())
}

pub(crate) fn set_output_layout(
    state: &mut Ferese,
    layout: crate::config::OutputLayout,
) -> Result<(), String> {
    let backend = state
        .direct_backend
        .as_mut()
        .ok_or("display modes require the direct DRM backend")?;
    if !backend.active || backend.lid.pending() {
        return Err("output session is inactive or refreshing lid state".into());
    }
    let usable = backend
        .monitors
        .iter()
        .filter(|monitor| monitor.usable)
        .collect::<Vec<_>>();
    let available = match layout {
        crate::config::OutputLayout::InternalOnly => usable.iter().any(|monitor| monitor.internal),
        crate::config::OutputLayout::ExternalOnly => usable.iter().any(|monitor| !monitor.internal),
        crate::config::OutputLayout::Extend | crate::config::OutputLayout::Mirror => {
            usable.len() >= 2
        }
    };
    if !available {
        return Err("this display mode requires monitors that are not connected".into());
    }
    let previous = backend.manual_outputs.clone();
    let known_good = backend.applied_outputs.clone();
    backend.manual_outputs.profile = None;
    backend.manual_outputs.configuration = None;
    backend.manual_outputs.internal = None;
    backend.manual_outputs.layout = Some(layout);
    if let Err(error) = validate_live_outputs(state, &state.output_profiles) {
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .manual_outputs = previous;
        return Err(error);
    }
    reconcile_outputs(state, ReconcileReason::Configuration);
    if let Some(error) = state
        .direct_backend
        .as_ref()
        .expect("DRM callbacks require an installed direct backend")
        .output_error
        .clone()
    {
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .manual_outputs = previous;
        return Err(error);
    }
    arm_output_confirmation(state, known_good, previous);
    state.notify_monitor_state();
    Ok(())
}

pub(crate) fn set_internal_output(state: &mut Ferese, enabled: bool) -> Result<(), String> {
    let backend = state
        .direct_backend
        .as_mut()
        .ok_or("internal panel control requires the direct DRM backend")?;
    if !backend.active || backend.lid.pending() {
        return Err("output session is inactive or refreshing lid state".into());
    }
    if !enabled
        && !backend
            .monitors
            .iter()
            .any(|monitor| !monitor.internal && monitor.usable)
    {
        return Err("cannot disable the last usable output".into());
    }
    let previous = backend.manual_outputs.clone();
    let known_good = backend.applied_outputs.clone();
    backend.manual_outputs.internal = Some(enabled);
    backend.manual_outputs.configuration = None;
    backend.manual_outputs.layout = None;
    reconcile_outputs(state, ReconcileReason::Configuration);
    if let Some(error) = state
        .direct_backend
        .as_ref()
        .expect("DRM callbacks require an installed direct backend")
        .output_error
        .clone()
    {
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .manual_outputs = previous;
        return Err(error);
    }
    arm_output_confirmation(state, known_good, previous);
    state.notify_monitor_state();
    Ok(())
}

fn confirmation_baseline<T>(pending: Option<T>, previous: T) -> T {
    pending.unwrap_or(previous)
}

fn arm_output_confirmation(
    state: &mut Ferese,
    previous: DesiredOutputConfiguration,
    manual: ManualOverride,
) {
    let backend = state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend");
    if let Some(token) = backend.confirmation_timer.take() {
        state.loop_handle.remove(token);
    }
    let timeout = backend
        .desired_outputs
        .profile
        .as_ref()
        .and_then(|name| {
            state
                .output_profiles
                .iter()
                .find(|profile| &profile.name == name)
        })
        .map_or(15, |profile| profile.confirm_timeout);
    let baseline = confirmation_baseline(backend.confirmation.take(), (previous, manual));
    if timeout == 0 || baseline.0.outputs.is_empty() {
        return;
    }
    backend.confirmation = Some(baseline);
    backend.confirmation_lid = backend.lid.closed();
    match state.loop_handle.insert_source(
        Timer::from_duration(Duration::from_secs(timeout.into())),
        |_, _, state| {
            if let Some(backend) = state.direct_backend.as_mut() {
                backend.confirmation_timer = None;
            }
            let _ = confirm_output_configuration(state, false);
            TimeoutAction::Drop
        },
    ) {
        Ok(token) => {
            state
                .direct_backend
                .as_mut()
                .expect("DRM callbacks require an installed direct backend")
                .confirmation_timer = Some(token)
        }
        Err(error) => {
            tracing::warn!(%error, "could not arm monitor confirmation; reverting");
            let _ = confirm_output_configuration(state, false);
        }
    }
}

pub(crate) fn confirm_output_configuration(
    state: &mut Ferese,
    confirm: bool,
) -> Result<(), String> {
    let backend = state
        .direct_backend
        .as_mut()
        .ok_or("direct backend unavailable")?;
    if let Some(token) = backend.confirmation_timer.take() {
        state.loop_handle.remove(token);
    }
    if let Some((previous, manual)) = backend.confirmation.take()
        && !confirm
    {
        backend.revert_outputs = Some(crate::output_policy::restore_configuration(
            &previous,
            &backend.monitors,
        ));
        backend.manual_outputs = manual;
        reconcile_outputs(state, ReconcileReason::Configuration);
        if let Some(error) = state
            .direct_backend
            .as_ref()
            .expect("DRM callbacks require an installed direct backend")
            .output_error
            .clone()
        {
            return Err(error);
        }
    }
    state.notify_monitor_state();
    Ok(())
}

pub(crate) fn validate_live_outputs(
    state: &Ferese,
    profiles: &[OutputProfile],
) -> Result<(), String> {
    let Some(backend) = state.direct_backend.as_ref() else {
        return Ok(());
    };
    if !backend.can_render() || backend.devices.is_empty() {
        // Syntax/config validation still runs; live probing waits for ownership.
        return Ok(());
    }
    for profile in profiles {
        let mut matched = HashSet::new();
        for settings in &profile.outputs {
            let matches = backend
                .monitors
                .iter()
                .filter(|monitor| monitor.matches(&settings.matcher))
                .collect::<Vec<_>>();
            if matches.len() > 1 || matches.iter().any(|monitor| !matched.insert(&monitor.key)) {
                return Err(format!(
                    "profile {:?} contains ambiguous or overlapping monitor selectors",
                    profile.name
                ));
            }
        }
    }
    let mut desired = crate::output_policy::select_profile(
        &backend.monitors,
        backend.lid.closed(),
        profiles,
        &backend.manual_outputs,
    );
    resolve_output_positions(backend, &mut desired)?;
    let usable = validate_output_configuration(backend, &desired, &[])?;
    if usable == 0 {
        return Err("display configuration would leave no usable output".into());
    }
    Ok(())
}

fn open_device(state: &mut Ferese, path: &Path) -> Result<DrmNode, Box<dyn Error>> {
    let session = state
        .direct_backend
        .as_ref()
        .expect("direct backend state is initialized before the DRM device")
        .session
        .clone();
    let node = DrmNode::from_path(path)?;
    let session_device = session_device::SessionDevice::open(session, path)?;
    let fd = DrmDeviceFd::new(session_device.device_fd()?);
    let (drm, notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd)?;
    // SAFETY: GBM owns a valid DRM descriptor for the lifetime of the EGL display.
    let egl_display = unsafe { EGLDisplay::new(gbm.clone())? };
    let egl_context = context::try_new(&egl_display)?;
    // SAFETY: the new context is not current on another thread and remains renderer-owned.
    let renderer = unsafe { GlesRenderer::new(egl_context)? };
    state.shm_state.update_formats(renderer.shm_formats());
    let dmabuf_formats = renderer.dmabuf_formats();
    let display_handle = state.display_handle.clone();
    // Mesa's Wayland EGL path needs the main device, not just a v3 format
    // list, to choose a hardware render node. Without feedback, nested EGL
    // clients can fall back to llvmpipe despite a GPU-backed DRM compositor.
    let render_node = node
        .node_with_type(NodeType::Render)
        .and_then(Result::ok)
        .unwrap_or(node);
    let feedback = DmabufFeedbackBuilder::new(render_node.dev_id(), dmabuf_formats).build()?;
    let registration = state
        .loop_handle
        .insert_source(notifier, move |event, metadata, state| match event {
            DrmEvent::VBlank(crtc) => {
                #[cfg(feature = "drm-fault-injection")]
                if state
                    .direct_backend
                    .as_ref()
                    .and_then(|backend| backend.devices.get(&node))
                    .and_then(|device| device.outputs.get(&crtc))
                    .is_some_and(|output| output.frame_pending)
                    && recovery::inject("missing-completion")
                {
                    tracing::warn!(?node, ?crtc, "injecting missing DRM completion");
                    return;
                }

                if let Some(metadata) = metadata {
                    let mut retired = false;
                    let mut retirement_failed = false;
                    let feedback = state
                        .direct_backend
                        .as_mut()
                        .and_then(|backend| backend.devices.get_mut(&node))
                        .and_then(|device| device.outputs.get_mut(&crtc))
                        .and_then(|output| {
                            #[cfg(feature = "drm-fault-injection")]
                            if output.frame_pending && recovery::inject("retirement") {
                                tracing::warn!(?node, ?crtc, "injecting DRM retirement failure");
                                retirement_failed = true;
                                output.diagnostics.retirement(false, true);
                                output.diagnostics.report_if_due();
                                return None;
                            }

                            let (feedback, completed, error) = recovery::retirement(output.surface.frame_submitted());
                            retired = completed;
                            output.diagnostics.retirement(completed, error.is_some());
                            output.diagnostics.report_if_due();
                            if let Some(error) = error {
                                tracing::error!(?node, ?crtc, %error, "failed to retire DRM frame");
                                retirement_failed = true;
                            }

                            feedback
                        });

                    let previous = state.direct_backend.as_ref()
                        .and_then(|backend| backend.presentation.get(&(node, crtc)))
                        .and_then(|clock| clock.last_presentation);
                    let valid_clock = state.direct_backend.as_ref()
                        .and_then(|backend| backend.devices.get(&node))
                        .and_then(|device| device.outputs.get(&crtc))
                        .is_some_and(|output| {
                            if let DrmEventTime::Monotonic(timestamp) = metadata.time {
                                presentation_mode::valid_timestamp(timestamp, monotonic_now(), refresh_duration_from_mode(output.mode), previous)
                            } else { false }
                        });
                    let feedback = feedback.filter(|_| valid_clock);
                    let fixed_phase = feedback.as_ref().is_some_and(|feedback| matches!(feedback.refresh, Refresh::Fixed(_)));
                    if let (Some(FrameFeedback { mut feedback, tearing, refresh }), DrmEventTime::Monotonic(time)) = (feedback, metadata.time) {
                        feedback.presented(
                            Time::<Monotonic>::from(time),
                            refresh,
                            u64::from(metadata.sequence),
                            presentation_mode::feedback_flags(tearing),
                        );
                    }

                    let lock_output = state
                        .direct_backend
                        .as_mut()
                        .and_then(|backend| backend.devices.get_mut(&node))
                        .and_then(|device| device.outputs.get_mut(&crtc))
                        .and_then(|output| {
                            if retired && std::mem::take(&mut output.lock_frame_pending) {
                                Some(output.output.clone())
                            } else {
                                None
                            }
                        });

                    if let Some(output) = lock_output {
                        state.lock_frame_presented(&output);
                    }

                    if let Some(output) = state
                        .direct_backend
                        .as_mut()
                        .and_then(|backend| backend.devices.get_mut(&node))
                        .and_then(|device| device.outputs.get_mut(&crtc))
                        && retired
                    {
                        output.completion.retired();
                        if let Some(token) = output.render_fence.take() {
                            state.loop_handle.remove(token);
                        }
                        output.frame_pending = false;
                        if output.power.needs_wake() {
                            output.gamma.retry_probe();
                            state.gamma_dirty = true;
                        }
                        output.power.presented();
                        if let DrmEventTime::Monotonic(timestamp) = metadata.time {
                            if valid_clock {
                                if let Some(plan) = output.scheduler.presented_with_phase(timestamp, fixed_phase)
                                    && std::env::var_os("FERESE_TRACE_PERFORMANCE").is_some()
                                {
                                    tracing::debug!(target: "ferese::render", output = %output.output.name(),
                                        target_us = plan.present_at.as_micros(), actual_us = timestamp.as_micros(),
                                        request_to_present_us = timestamp.saturating_sub(plan.requested_at).as_micros(),
                                        missed_deadlines = output.scheduler.missed_deadlines,
                                        "DRM presentation schedule");
                                }
                            } else {
                                output.scheduler.retire_unknown();
                            }
                        } else {
                            output.scheduler.retire_unknown();
                        }
                    }

                    if retired {
                        if let Some(output) = state
                            .direct_backend
                            .as_ref()
                            .and_then(|backend| backend.devices.get(&node))
                            .and_then(|device| device.outputs.get(&crtc))
                        {
                            state.display_presentation.presented(&output.output);
                        }
                        state.refresh_idle_inhibition();
                    }

                    if retirement_failed {
                        recovery::reconcile_device(state, node);
                        return;
                    }

                    apply_output_power(state);

                    if retired && valid_clock {
                        state.record_drm_presentation(node, crtc, metadata.time, metadata.sequence);
                    } else if !valid_clock
                        && let Some(clock) = state.direct_backend.as_mut()
                            .and_then(|backend| backend.presentation.get_mut(&(node, crtc)))
                    {
                        clock.reset_timing();
                    }
                    tracing::trace!(?node, ?crtc, sequence = metadata.sequence, "page flip");
                    let active = state
                        .direct_backend
                        .as_ref()
                        .is_some_and(|backend| backend.can_render());
                    if active && retired {
                        schedule_frame_callbacks(state, node, crtc, monotonic_now());
                        let identity = state
                            .direct_backend
                            .as_ref()
                            .and_then(|backend| backend.devices.get(&node))
                            .and_then(|device| device.outputs.get(&crtc))
                            .map(|output| output.output.clone());
                        if identity.is_some_and(|output| state.output_has_animations(&output)) {
                            request_frame(state, node, crtc);
                        } else {
                            arm_frame(state, node, crtc);
                        }

                        arm_animation_timer(state);
                    }
                }
            }

            DrmEvent::Error(error) => {
                tracing::error!(?node, %error, "DRM event error");
            }
        })?;

    let dmabuf_global = state
        .dmabuf_state
        .create_global_with_default_feedback::<Ferese>(&display_handle, &feedback);
    state.dmabuf_imports.register(dmabuf_global);
    state.dmabuf_feedback.register(dmabuf_global, feedback);
    state.register_sync_renderer(&renderer, Some(render_node));
    state
        .direct_backend
        .as_mut()
        .expect("direct backend state remains initialized")
        .devices
        .insert(
            node,
            DirectDevice {
                inventory: None,
                active_leases: Vec::new(),
                _session_device: session_device,
                drm,
                gbm,
                renderer,
                render_node,
                outputs: HashMap::new(),
                notifier: registration,
                dmabuf_global,
                connected_outputs: Vec::new(),
            },
        );
    tracing::info!(?node, ?path, "initialized DRM/GBM device");
    Ok(node)
}

pub(crate) fn sleep_locked_outputs(state: &mut Ferese) {
    if !state.session_lock.active() || !state.session_lock.sleeping {
        return;
    }

    if let Some(backend) = state.direct_backend.as_mut() {
        for device in backend.devices.values_mut() {
            for output in device.outputs.values_mut() {
                output.power.request(Power::Off);
            }
        }
    }
    apply_output_power(state);
}

// Apply runtime requests without changing output globals or workspace ownership.
fn apply_output_power(state: &mut Ferese) {
    let Some(backend) = state
        .direct_backend
        .as_mut()
        .filter(|backend| backend.can_render())
    else {
        return;
    };

    let device_iter = backend
        .devices
        .values_mut()
        .filter(|device| device.drm.is_active());
    for device in device_iter {
        for output in device.outputs.values_mut() {
            if !output.power.needs_power_off() || output.frame_pending {
                continue;
            }

            match output.surface.clear() {
                Ok(()) => {
                    cancel_output_timers(&state.loop_handle, output);
                    output.power.powered_off();
                    state.display_presentation.remove_output(&output.output);
                }

                Err(error) => {
                    tracing::warn!(%error, output = %output.output.name(), "could not apply output power-off request")
                }
            }
        }
    }
    let retry = backend.active
        && backend
            .devices
            .values()
            .filter(|device| device.drm.is_active())
            .any(|device| {
                device
                    .outputs
                    .values()
                    .any(|output| output.power.needs_power_off() && !output.frame_pending)
            });
    if !retry {
        if let Some(token) = backend.power_retry.take() {
            state.loop_handle.remove(token);
        }
    } else if backend.power_retry.is_none() {
        match state.loop_handle.insert_source(
            Timer::from_duration(Duration::from_secs(1)),
            |_, _, state| {
                if let Some(backend) = state.direct_backend.as_mut() {
                    backend.power_retry = None;
                }
                apply_output_power(state);
                TimeoutAction::Drop
            },
        ) {
            Ok(token) => backend.power_retry = Some(token),
            Err(error) => tracing::warn!(%error, "could not arm failed power-off retry"),
        }
    }
    state.refresh_idle_inhibition();
}

pub(crate) fn wake_locked_outputs(state: &mut Ferese) {
    state.reset_animation_clock();
    if let Some(backend) = state.direct_backend.as_mut() {
        for device in backend.devices.values_mut() {
            for output in device.outputs.values_mut() {
                if output.power.needs_wake() {
                    output.surface.reset_buffer_ages();
                }
                output.power.request(Power::On);
            }
        }
    }
    apply_output_power(state);
    render_all(state);
}

#[track_caller]
pub fn render_all(state: &mut Ferese) {
    state.image_capture.invalidate_workspaces();
    for output in state.space.outputs() {
        capture::changes::content_changed(output);
    }
    redraw(state, None, std::panic::Location::caller());
}

#[track_caller]
pub(crate) fn render_on(state: &mut Ferese, outputs: &[Output]) {
    for output in outputs {
        capture::changes::content_changed(output);
    }
    redraw(state, Some(outputs), std::panic::Location::caller());
}

/// Consumer demand schedules a frame without invalidating retained scene pixels.
#[track_caller]
pub(crate) fn schedule_capture(state: &mut Ferese, outputs: &[Output]) {
    redraw(state, Some(outputs), std::panic::Location::caller());
}

#[track_caller]
pub(crate) fn render_cursor(state: &mut Ferese) {
    // Include nested outputs and both sides of an output crossing. This does not
    // invalidate the content revision used by cursor-hidden captures.
    for output in state.space.outputs() {
        capture::changes::cursor_changed(output);
    }
    let outputs = cursor_affected_outputs(state);
    redraw(state, Some(&outputs), std::panic::Location::caller());
}

fn cursor_affected_outputs(state: &Ferese) -> Vec<Output> {
    state
        .direct_backend
        .as_ref()
        .into_iter()
        .flat_map(|backend| backend.devices.values())
        .flat_map(|device| device.outputs.values())
        .filter(|output| output.cursor_visible || redraw::cursor_on_output(state, &output.output))
        .map(|output| output.output.clone())
        .collect()
}

pub(crate) fn cursor_animation_visible(state: &Ferese) -> bool {
    if let Some(backend) = state.direct_backend.as_ref() {
        backend.can_render()
            && backend.devices.values().any(|device| {
                device.outputs.values().any(|output| {
                    output.power.can_render() && redraw::cursor_on_output(state, &output.output)
                })
            })
    } else {
        state
            .space
            .outputs()
            .any(|output| redraw::cursor_on_output(state, output))
    }
}

#[track_caller]
pub(crate) fn render_surface(
    state: &mut Ferese,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) {
    use smithay::input::pointer::CursorImageStatus;
    use smithay::wayland::compositor::get_parent;
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }

    if matches!(&state.cursor_status, CursorImageStatus::Surface(cursor) if cursor == &root)
        || state.is_drag_icon(&root)
    {
        render_cursor(state);
        return;
    }

    let capture_root = state
        .popups
        .find_popup(&root)
        .and_then(|popup| smithay::desktop::find_popup_root_surface(&popup).ok())
        .unwrap_or_else(|| root.clone());
    if let Some(workspace) = state
        .windows
        .id_for_surface(&capture_root)
        .and_then(|id| state.workspaces.workspace_for_window(id))
    {
        state.image_capture.workspace_changed(workspace);
    }
    let outputs = state.surface_outputs(&root);
    render_on(state, &outputs);
}

fn redraw(
    state: &mut Ferese,
    selected: Option<&[Output]>,
    source: &'static std::panic::Location<'static>,
) {
    if let Some(transition) = state.desktop_transition.as_mut() {
        transition.redraw.extend(
            selected
                .map(|outputs| outputs.to_vec())
                .unwrap_or_else(|| state.space.outputs().cloned().collect()),
        );
        return;
    }
    if state.direct_backend.is_none() {
        crate::winit::request_frame(state);
        return;
    }
    if state
        .direct_backend
        .as_ref()
        .is_none_or(|backend| !backend.can_render())
    {
        return;
    }

    let outputs = state
        .direct_backend
        .as_ref()
        .expect("DRM callbacks require an installed direct backend")
        .devices
        .iter()
        .flat_map(|(node, device)| {
            device
                .outputs
                .iter()
                .map(|(crtc, output)| (*node, *crtc, output.output.clone()))
        })
        .collect::<Vec<_>>();
    // Include animations before advancing, so their final settled frame is
    // drawn too. Advancing shared state must not leave another output stale.
    let affected = outputs
        .iter()
        .map(|(_, _, output)| {
            let scene = state
                .direct_backend
                .as_ref()
                .expect("DRM callbacks require an installed direct backend")
                .scene_output(state, output);
            selected.is_none_or(|selected| selected.contains(output) || selected.contains(&scene))
                || state.output_has_animations(&scene)
                || state.output_has_pending_visual_changes(&scene)
        })
        .collect::<Vec<_>>();
    state.advance_animations(Instant::now());
    let mut requested = 0;
    let node_iter = outputs.iter().zip(affected);
    for ((node, crtc, output), affected) in node_iter {
        if affected || state.output_has_animations(output) {
            request_frame(state, *node, *crtc);
            requested += 1;
        }
    }

    state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend")
        .scheduling_metrics
        .redraw(source, selected.is_none(), requested, outputs.len());
    arm_animation_timer(state);
}

pub fn switch_vt(state: &mut Ferese, vt: i32) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };

    if let Err(error) = backend.session.change_vt(vt) {
        tracing::error!(vt, %error, "failed to switch virtual terminal");
    }
}

pub(crate) fn set_lid_closed(state: &mut Ferese, closed: bool) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    // Even an unchanged switch observation supersedes an in-flight DBus read.
    if backend.lid.observe(closed) {
        tracing::info!(closed, "laptop lid state changed");
        reconcile_outputs(state, ReconcileReason::Configuration);
    }
}

pub(crate) fn system_resumed(state: &mut Ferese) {
    input_resume::refresh(state);
    if state
        .direct_backend
        .as_ref()
        .is_some_and(|backend| backend.active)
    {
        let seat = state.seat.clone();
        state.idle_notifier_state.notify_activity(&seat);
    }
    lid::request_refresh(state, true);
}

pub(crate) fn reconcile_after_monitor_recovery(state: &mut Ferese) {
    input_resume::reconcile(state);
    lid::request_reconcile(state);
}

fn internal_connector(interface: connector::Interface) -> bool {
    matches!(
        interface,
        connector::Interface::EmbeddedDisplayPort
            | connector::Interface::LVDS
            | connector::Interface::DSI
    )
}

#[cfg(test)]
fn lid_hides_panel(closed: bool, external_available: bool) -> bool {
    closed && external_available
}

// Return the sampled scene's activity so dispatch does not rescan the windows.
fn render_output(
    state: &mut Ferese,
    node: DrmNode,
    crtc: crtc::Handle,
    plan: crate::frame_scheduler::FramePlan,
    started: Duration,
) -> bool {
    if state.session_lock.active() && state.session_lock.sleeping {
        sleep_locked_outputs(state);
        let asleep = state
            .direct_backend
            .as_ref()
            .and_then(|backend| backend.devices.get(&node))
            .and_then(|device| device.outputs.get(&crtc))
            .is_none_or(|output| !output.power.can_render() || output.frame_pending);
        if asleep {
            return false;
        }
    }

    let missed_deadlines = state
        .direct_backend
        .as_ref()
        .and_then(|backend| backend.devices.get(&node))
        .and_then(|device| device.outputs.get(&crtc))
        .map_or(0, |output| output.scheduler.missed_deadlines);
    let scene_output = state.direct_backend.as_ref().map(|backend| {
        let physical = &backend.devices[&node].outputs[&crtc].output;
        backend.scene_output(state, physical)
    });
    let Some(mut device) = state
        .direct_backend
        .as_mut()
        .and_then(|backend| backend.devices.remove(&node))
    else {
        return false;
    };

    let Some(mut output) = device.outputs.remove(&crtc) else {
        restore_device(state, node, device);
        return false;
    };

    if !device.drm.is_active() || !output.power.can_render() || output.frame_pending {
        device.outputs.insert(crtc, output);
        restore_device(state, node, device);
        return false;
    }

    let render_started = Instant::now();
    let now = monotonic_now();
    let horizon = output
        .scheduler
        .forecast_time(now, plan)
        .map_or(Duration::ZERO, |target| target.saturating_sub(now));
    let scene_output = scene_output.unwrap_or_else(|| output.output.clone());
    let frame = state.sample_frame(&scene_output, horizon);
    let rendered = {
        (|| -> Result<bool, Box<dyn Error>> {
            let elements = if output.mirror_source.is_some() {
                mirror::elements(
                    state,
                    &mut device.renderer,
                    &scene_output,
                    &frame,
                    &mut output,
                )?
            } else {
                sampled_output_elements(state, &mut device.renderer, &output.output, true, &frame)
            };
            let effects =
                frame_effect_metrics(&elements, output.output.current_scale().fractional_scale());
            let fullscreen = state
                .output_has_fullscreen_for_frame(&output.output, frame.overview.is_presenting());
            let locked = state.session_lock.active();
            let flags = planes::frame_flags(fullscreen, frame.animating, locked);
            let tearing_candidate =
                presentation_mode::tearing_candidate(state, &output) && !frame.animating;
            output
                .diagnostics
                .attempt(fullscreen, frame.animating, locked);
            let result = output
                .surface
                .render_frame(
                    &mut device.renderer,
                    &elements,
                    if output.mirror_source.is_some() {
                        [0.0, 0.0, 0.0, 1.0]
                    } else {
                        [0.035, 0.04, 0.055, 1.0]
                    },
                    flags,
                )
                .inspect_err(|_| output.diagnostics.render_failed())?;

            let primary_format = match &result.primary_element {
                PrimaryPlaneElement::Swapchain(primary) => Some(primary.buffer().format()),
                PrimaryPlaneElement::Element(element) => {
                    match element.underlying_storage(&mut device.renderer) {
                        Some(UnderlyingStorage::Wayland(buffer)) => {
                            smithay::wayland::dmabuf::get_dmabuf(buffer)
                                .ok()
                                .map(|buffer| buffer.format())
                        }
                        _ => None,
                    }
                }
            };
            let feedback_candidates = feedback::candidates(
                &mut device.renderer,
                &elements,
                &output.output,
                flags.contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT),
            );

            if result.needs_sync()
                && let PrimaryPlaneElement::Swapchain(primary) = &result.primary_element
            {
                primary
                    .sync
                    .wait()
                    .inspect_err(|_| output.diagnostics.sync_failed())?;
            }

            if result.is_empty {
                output.diagnostics.no_damage();
                let capture_started = output.diagnostics.capture_start(|| {
                    [true, false]
                        .into_iter()
                        .any(|cursor| state.has_pending_screencopy(&output.output, cursor))
                });
                output.dmabuf_feedback.observed_frame(
                    primary_format,
                    &result.states,
                    feedback_candidates,
                );
                if output.mirror_source.is_some() {
                    capture::capture_mirror(
                        state,
                        &mut device.renderer,
                        &scene_output,
                        &frame,
                        &mut output,
                    );
                } else {
                    capture::capture_output(
                        state,
                        &mut device.renderer,
                        &mut output.capture_texture,
                        &output.output,
                        &frame,
                    );
                }

                output.diagnostics.capture_finished(capture_started);
                output
                    .render_metrics
                    .record_no_damage(render_started.elapsed(), missed_deadlines);
                return Ok(false);
            }

            // Count GPU composition damage; direct scanout and cursor-only
            // updates do not redraw the primary swapchain.
            let (damage, primary_commit) = match &result.primary_element {
                PrimaryPlaneElement::Swapchain(primary) => {
                    let damage = primary.damage.damage_since(output.primary_commit);
                    let damage = damage.map_or_else(
                        || {
                            vec![smithay::utils::Rectangle::from_size(
                                output
                                    .output
                                    .current_mode()
                                    .expect("a rendered output must have a current mode")
                                    .size,
                            )]
                        },
                        |damage| {
                            damage
                                .iter()
                                .map(|rect| {
                                    smithay::utils::Rectangle::new(
                                        (rect.loc.x, rect.loc.y).into(),
                                        (rect.size.w, rect.size.h).into(),
                                    )
                                })
                                .collect()
                        },
                    );
                    (damage, Some(primary.damage.current_commit()))
                }

                PrimaryPlaneElement::Element(_) => (Vec::new(), None),
            };
            let tearing = tearing_candidate
                && matches!(result.primary_element, PrimaryPlaneElement::Element(_))
                && crate::tearing_control::fullscreen_surface(state, &output.output).is_some_and(|surface| {
                    result.states.element_render_state(smithay::backend::renderer::element::Id::from(&surface))
                        .is_some_and(|state| state.presentation_state == smithay::backend::renderer::element::RenderElementPresentationState::ZeroCopy)
                });
            let presentation = if output.mirror_source.is_some() {
                OutputPresentationFeedback::new(&output.output)
            } else {
                crate::presentation::take_output_feedback(
                    state,
                    &output.output,
                    &result.states,
                    presentation_mode::feedback_flags(tearing),
                )
            };
            let primary_outcome = PrimaryOutcome::classify(
                matches!(result.primary_element, PrimaryPlaneElement::Element(_)),
                !damage.is_empty(),
            );
            let cursor_plane = result.cursor_element.is_some();
            let visibility = result.states.clone();
            let render_sync = match &result.primary_element {
                PrimaryPlaneElement::Swapchain(primary) => Some(primary.sync.clone()),
                PrimaryPlaneElement::Element(_) => None,
            };
            drop(result);

            let refresh = presentation_mode::feedback_refresh(
                output.surface.vrr_enabled(),
                tearing,
                refresh_duration_from_mode(output.mode),
            );
            let queued = output.surface.queue_frame_with_tearing(
                FrameFeedback {
                    feedback: presentation,
                    tearing,
                    refresh,
                },
                tearing,
            );
            output.diagnostics.queue_result(
                &queued,
                primary_outcome,
                cursor_plane,
                flags.contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT),
            );
            capture::after_queue(queued, || {
                record_frame_schedule(&mut output, plan, started, monotonic_now(), true);
                if let Some(sync) = render_sync {
                    watch_render_ready(state, node, crtc, &mut output, plan, started, sync);
                }
                output.dmabuf_feedback.observed_frame(
                    primary_format,
                    &visibility,
                    feedback_candidates,
                );
                if output.mirror_source.is_none() {
                    state.display_presentation.queued(
                        &output.output,
                        &visibility,
                        frame.input_windows(&output.output),
                    );
                }
                output.primary_commit = primary_commit;
                output.render_metrics.record_frame(
                    render_started.elapsed(),
                    &damage,
                    missed_deadlines,
                    effects,
                );
                output.frame_pending = true;
                if !recovery::arm_completion(&state.loop_handle, node, crtc, &mut output) {
                    recovery::reconcile_device(state, node);
                }

                output.lock_frame_pending = state.session_lock.active();
                let capture_started = output.diagnostics.capture_start(|| {
                    [true, false]
                        .into_iter()
                        .any(|cursor| state.has_pending_screencopy(&output.output, cursor))
                });

                if output.mirror_source.is_some() {
                    capture::capture_mirror(
                        state,
                        &mut device.renderer,
                        &scene_output,
                        &frame,
                        &mut output,
                    );
                } else {
                    capture::capture_output(
                        state,
                        &mut device.renderer,
                        &mut output.capture_texture,
                        &output.output,
                        &frame,
                    );
                }
                output.diagnostics.capture_finished(capture_started);
            })?;

            Ok(true)
        })()
    };

    output.diagnostics.report_if_due();

    // Keep the footprint of the last successful scene, including queued
    // cursor planes. Requests do not read the pointer while a grab holds it.
    if rendered.is_ok() {
        output.cursor_visible = redraw::cursor_on_output(state, &output.output);
    }

    let submitted = rendered.as_ref().is_ok_and(|submitted| *submitted);
    if !submitted {
        record_frame_schedule(&mut output, plan, started, monotonic_now(), false);
    }

    if submitted && let Some(token) = output.callback_timer.take() {
        state.loop_handle.remove(token);
    }

    match rendered {
        Ok(true) => tracing::trace!(?node, ?crtc, "queued DRM frame"),
        Ok(false) => (),
        Err(error) => tracing::error!(?node, ?crtc, %error, "failed to render DRM frame"),
    }

    device.outputs.insert(crtc, output);
    restore_device(state, node, device);
    if !submitted
        && scene_output
            == state
                .direct_backend
                .as_ref()
                .expect("DRM callbacks require an installed direct backend")
                .devices[&node]
                .outputs[&crtc]
                .output
    {
        schedule_frame_callbacks(state, node, crtc, plan.present_at);
    }

    frame.animating
}

fn restore_device(state: &mut Ferese, node: DrmNode, device: DirectDevice) {
    if let Some(backend) = state.direct_backend.as_mut() {
        backend.devices.insert(node, device);
    }
}

fn direct_node_for_device(state: &Ferese, device_id: libc::dev_t) -> Option<DrmNode> {
    state
        .direct_backend
        .as_ref()?
        .devices
        .keys()
        .copied()
        .find(|node| node.dev_id() == device_id)
}

fn rescan_device(state: &mut Ferese, node: DrmNode) -> Result<bool, ()> {
    let Some(backend) = state
        .direct_backend
        .as_mut()
        .filter(|backend| backend.active)
    else {
        return Err(());
    };
    let Some(mut device) = backend.devices.remove(&node) else {
        return Err(());
    };
    let scan = select_outputs(
        &device,
        &backend.monitors,
        &backend.desired_outputs,
        backend.low_power,
    );
    let result = scan
        .map_err(|error| error.to_string())
        .and_then(|scan| transaction::apply_device(state, node, &mut device, scan));
    if let Err(error) = &result {
        tracing::warn!(?node, %error, "output transaction failed; restored known-good outputs where possible");
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .output_error = Some(error.clone());
        transaction::update_applied_info(&mut device);
    }
    restore_device(state, node, device);
    Ok(result.is_ok())
}

fn refresh_connected_info(state: &mut Ferese, deferred: &[DrmNode]) {
    let backend = state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend");
    for (node, device) in &mut backend.devices {
        if !deferred.contains(node)
            && let Ok(scan) = select_outputs(
                device,
                &backend.monitors,
                &backend.desired_outputs,
                backend.low_power,
            )
        {
            device.connected_outputs = scan.connected_outputs;
        }
        transaction::update_applied_info(device);
        for info in &mut device.connected_outputs {
            info.profile = backend.applied_outputs.profile.clone();
        }
    }
    let mut inventory = backend
        .devices
        .values()
        .flat_map(|device| device.connected_outputs.clone())
        .collect::<Vec<_>>();
    for previous in &backend.connected_outputs {
        if !inventory
            .iter()
            .any(|output| output.identity == previous.identity)
        {
            let mut missing = previous.clone();
            missing.connected = false;
            missing.enabled = false;
            missing.requested_enabled = false;
            missing.current_mode = None;
            inventory.push(missing);
        }
    }
    let identities = inventory
        .iter()
        .map(|output| output.identity.clone())
        .collect::<Vec<_>>();
    backend.connected_outputs = inventory;
    for identity in identities {
        state.ensure_output_identity(&identity);
    }
}

fn remember_applied_configuration(state: &mut Ferese) {
    let backend = state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend");
    if backend.topology.reconciling {
        // Applied positions are read only after the coherent logical publish.
        backend.topology.remember_applied = true;
        return;
    }
    let mut applied = backend.desired_outputs.clone();
    for desired in &mut applied.outputs {
        let monitor = backend
            .monitors
            .iter()
            .find(|monitor| monitor.key == desired.key);
        let output = monitor.and_then(|monitor| {
            backend
                .devices
                .values()
                .flat_map(|device| device.outputs.values())
                .find(|output| output.identity == monitor.identity)
        });
        desired.settings.enabled = output.is_some();
        if let Some(output) = output {
            desired.settings = output.settings.clone();
            let (width, height) = output.mode.size();
            desired.settings.mode = Some(OutputModeRequest {
                width,
                height,
                refresh_millihertz: Some(OutputMode::from(output.mode).refresh as u32),
            });
            let position = output.output.current_location();
            desired.settings.position = Some([position.x, position.y]);
            desired.mirror_source = output.mirror_source.clone();
        }
    }
    backend.applied_outputs = applied;
}

fn validate_desired_outputs(state: &Ferese, deferred: &[DrmNode]) -> Result<(), String> {
    let backend = state
        .direct_backend
        .as_ref()
        .ok_or("direct backend unavailable")?;
    validate_output_configuration(backend, &backend.desired_outputs, deferred).map(|_| ())
}

/// Only background recovery may defer known devices. Interactive callers pass
/// an empty slice so every requested output and hardware test remains required.
fn validate_output_configuration(
    backend: &DirectBackendState,
    desired: &DesiredOutputConfiguration,
    deferred: &[DrmNode],
) -> Result<usize, String> {
    let deferred_keys = deferred
        .iter()
        .filter_map(|node| backend.devices.get(node))
        .map(|device| format!("{}/", drm_device_key(&device.drm)))
        .collect::<Vec<_>>();
    let scans = backend
        .devices
        .iter()
        .filter(|(node, _)| !deferred.contains(node))
        .map(|(_, device)| {
            select_outputs(device, &backend.monitors, desired, backend.low_power)
                .map(|scan| (device, scan))
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let selected = scans
        .iter()
        .flat_map(|(device, scan)| {
            let device_key = drm_device_key(&device.drm);
            scan.selections
                .iter()
                .map(move |selection| format!("{device_key}/{}", selection.connector))
        })
        .collect::<Vec<_>>();
    validate_output_plan(desired, &selected, &deferred_keys, || {
        for (device, scan) in &scans {
            transaction::validate_device(device, scan).map_err(|error| error.to_string())?;
        }
        Ok(())
    })?;
    Ok(selected.len())
}

/// Keep plan completeness separate from the hardware probe so a partial scan
/// cannot be accepted merely because the driver accepts its selected subset.
fn validate_output_plan(
    desired: &DesiredOutputConfiguration,
    selected: &[String],
    deferred: &[String],
    test_hardware: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    validate_selected_outputs(
        desired
            .outputs
            .iter()
            .filter(|output| !deferred.iter().any(|prefix| output.key.starts_with(prefix))),
        selected,
    )?;
    test_hardware()
}

fn validate_selected_outputs<'a>(
    outputs: impl IntoIterator<Item = &'a crate::output_policy::DesiredOutput>,
    selected: &[String],
) -> Result<(), String> {
    let output_iter = outputs.into_iter().filter(|output| output.settings.enabled);
    for output in output_iter {
        if !selected.contains(&output.key) {
            return Err(format!(
                "enabled output {} has no usable CRTC assignment",
                output.key
            ));
        }
    }
    Ok(())
}

fn remove_device(state: &mut Ferese, node: DrmNode) {
    let Some(mut device) = state
        .direct_backend
        .as_mut()
        .and_then(|backend| backend.devices.remove(&node))
    else {
        return;
    };

    device.active_leases.clear();
    if let Some(backend) = state.direct_backend.as_mut() {
        leasing::retire(backend, node);
    }
    state.loop_handle.remove(device.notifier);
    state.cancel_gpu_buffer_waits(device.render_node);
    state.dmabuf_imports.remove(device.dmabuf_global);
    state.dmabuf_feedback.remove(device.dmabuf_global);
    state
        .dmabuf_state
        .disable_global::<Ferese>(&state.display_handle, &device.dmabuf_global);
    let context = device.renderer.context_id().erased();
    state.wallpaper.forget_context(&context);
    state.render.forget_context(&context);
    state.capture_render.forget_context(&context);
    for (crtc, mut output) in device.outputs {
        cancel_output_timers(&state.loop_handle, &mut output);
        if let Some(global) = output.global.take() {
            state.display_handle.disable_global::<Ferese>(global);
        }
        state.unregister_output(&output.output);
        if let Some(backend) = state.direct_backend.as_mut() {
            backend.presentation.remove(&(node, crtc));
        }
    }

    tracing::info!(?node, "removed DRM device");
}

fn refresh_duration_from_mode(mode: DrmMode) -> Duration {
    Duration::from_nanos(1_000_000_000_000 / OutputMode::from(mode).refresh.max(1) as u64)
}

fn animation_fallbacks(state: &mut Ferese) -> Vec<(DrmNode, crtc::Handle, Instant)> {
    let now = Instant::now();
    let outputs = state
        .direct_backend
        .as_ref()
        .into_iter()
        .flat_map(|backend| backend.devices.iter())
        .flat_map(|(node, device)| {
            device.outputs.iter().map(move |(crtc, output)| {
                (*node, *crtc, output.output.clone(), device.drm.is_active())
            })
        })
        .collect::<Vec<_>>();
    let mut fallbacks = Vec::new();
    for (node, crtc, identity, device_active) in outputs {
        let enabled = device_active
            && !state.session_lock.sleeping
            && state
                .direct_backend
                .as_ref()
                .is_some_and(|backend| backend.can_render());
        let scene = state
            .direct_backend
            .as_ref()
            .expect("DRM callbacks require an installed direct backend")
            .scene_output(state, &identity);
        let animating = enabled && state.output_has_animations(&scene);
        let backend = state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend");
        let output = backend
            .devices
            .get_mut(&node)
            .expect("the selected DRM device remains registered during this operation")
            .outputs
            .get_mut(&crtc)
            .expect("the selected DRM device remains registered during this operation");
        // Preserve recovery of a requested final frame if another output
        // advances this animation to rest before the watchdog is dispatched.
        let active = enabled
            && (animating
                || (output.animation_fallback.waiting_for_frame()
                    && output.scheduler.has_pending_request()));
        let chained = output.render_timer.is_some() || output.frame_pending;
        if active && chained {
            backend.scheduling_metrics.chained_animation_observations += 1;
        }

        if let Some(deadline) = output.animation_fallback.deadline(
            now,
            refresh_duration_from_mode(output.mode),
            active && output.power.can_render(),
            chained,
        ) {
            fallbacks.push((node, crtc, deadline));
        }
    }

    fallbacks
}

// Pending buffers are events with deadlines, not visual animations. Recompute
// from current output identities and waits so commit, replacement and teardown
// cancel stale wakeups, including mirrored and sleeping outputs.
fn resize_deadlines(state: &Ferese) -> Vec<(Output, Instant)> {
    let Some(backend) = state
        .direct_backend
        .as_ref()
        .filter(|backend| backend.can_render())
    else {
        return Vec::new();
    };
    if state.session_lock.sleeping {
        return Vec::new();
    }
    backend
        .devices
        .values()
        .filter(|device| device.drm.is_active())
        .flat_map(|device| device.outputs.values())
        .filter(|output| output.power.can_render())
        .filter_map(|output| {
            let scene = backend.scene_output(state, &output.output);
            Some((
                output.output.clone(),
                state.start_time + state.output_resize_deadline(&scene)?,
            ))
        })
        .collect()
}

fn arm_animation_timer(state: &mut Ferese) {
    let deadline = animation_fallbacks(state)
        .iter()
        .map(|(_, _, deadline)| *deadline)
        .chain(
            resize_deadlines(state)
                .into_iter()
                .map(|(_, deadline)| deadline),
        )
        .min();
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    if backend.animation_deadline == deadline {
        return;
    }

    if let Some(token) = backend.animation_timer.take() {
        state.loop_handle.remove(token);
        backend.scheduling_metrics.fallback_cancels += 1;
    }

    backend.animation_deadline = None;
    let Some(deadline) = deadline else {
        return;
    };
    match state
        .loop_handle
        .insert_source(Timer::from_deadline(deadline), |_, _, state| {
            if let Some(backend) = state.direct_backend.as_mut() {
                backend.animation_timer = None;
                backend.animation_deadline = None;
                backend.scheduling_metrics.fallback_wakes += 1;
            }

            let now = Instant::now();
            let expired_outputs = resize_deadlines(state)
                .into_iter()
                .filter_map(|(output, deadline)| (deadline <= now).then_some(output))
                .collect::<Vec<_>>();
            if !expired_outputs.is_empty() {
                // Retire waits before rearming, even if a DRM submission is
                // in flight. redraw advances the shared clock and requests the
                // released outputs plus any final animation frames elsewhere.
                render_on(state, &expired_outputs);
            }

            // Revalidate identities, power state, activity and normal frame chains
            // at dispatch. Disconnected/replaced outputs never get a stale request.
            for (node, crtc, deadline) in animation_fallbacks(state) {
                if deadline <= Instant::now() {
                    state
                        .direct_backend
                        .as_mut()
                        .expect("DRM callbacks require an installed direct backend")
                        .scheduling_metrics
                        .fallback_recoveries += 1;
                    state
                        .direct_backend
                        .as_mut()
                        .expect("DRM callbacks require an installed direct backend")
                        .devices
                        .get_mut(&node)
                        .expect(
                            "the selected DRM device must remain registered during this operation",
                        )
                        .outputs
                        .get_mut(&crtc)
                        .expect("the selected CRTC must remain registered during this operation")
                        .animation_fallback = AnimationFallback::default();
                    request_frame(state, node, crtc);
                }
            }

            arm_animation_timer(state);
            TimeoutAction::Drop
        }) {
        Ok(token) => {
            let backend = state
                .direct_backend
                .as_mut()
                .expect("DRM callbacks require an installed direct backend");
            backend.animation_timer = Some(token);
            backend.animation_deadline = Some(deadline);
            backend.scheduling_metrics.fallback_arms += 1;
        }

        Err(error) => tracing::warn!(%error, "could not schedule DRM animation fallback"),
    }
}

fn schedule_frame_callbacks(
    state: &mut Ferese,
    node: DrmNode,
    crtc: crtc::Handle,
    target: Duration,
) {
    if state.session_lock.sleeping {
        return;
    }

    let now = Instant::now();
    let timestamp = monotonic_now();
    let Some(output) = state
        .direct_backend
        .as_ref()
        .filter(|backend| backend.can_render())
        .and_then(|backend| backend.devices.get(&node))
        .filter(|device| device.drm.is_active())
        .and_then(|device| device.outputs.get(&crtc))
    else {
        return;
    };

    if output.mirror_source.is_some() || output.callback_timer.is_some() || output.frame_pending {
        return;
    }

    let identity = output.output.clone();
    let Some(callback_at) = output.scheduler.callback_deadline(timestamp, target) else {
        return;
    };

    if callback_at <= timestamp {
        deliver_frame_callbacks(state, node, crtc, &identity);
        return;
    }

    let deadline = now + callback_at.saturating_sub(timestamp);
    match state
        .loop_handle
        .insert_source(Timer::from_deadline(deadline), move |_, _, state| {
            deliver_frame_callbacks(state, node, crtc, &identity);
            TimeoutAction::Drop
        }) {
        Ok(token) => {
            if let Some(output) = state
                .direct_backend
                .as_mut()
                .and_then(|backend| backend.devices.get_mut(&node))
                .and_then(|device| device.outputs.get_mut(&crtc))
            {
                output.callback_timer = Some(token);
            }
        }

        Err(error) => tracing::warn!(%error, "could not schedule DRM frame callbacks"),
    }
}

fn deliver_frame_callbacks(
    state: &mut Ferese,
    node: DrmNode,
    crtc: crtc::Handle,
    identity: &Output,
) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };

    let Some(device) = backend.devices.get_mut(&node) else {
        return;
    };

    let Some(output) = device
        .outputs
        .get_mut(&crtc)
        .filter(|output| &output.output == identity)
    else {
        return;
    };

    output.callback_timer = None;
    if !backend.active
        || backend.topology.reconciling
        || backend.lid.pending()
        || !device.drm.is_active()
        || !output.power.can_render()
        || state.session_lock.sleeping
        || output.frame_pending
    {
        return;
    }

    let timestamp = monotonic_now();
    let Some(next) = output.scheduler.callback_deadline(timestamp, timestamp) else {
        return;
    };

    if next > timestamp {
        schedule_frame_callbacks(state, node, crtc, next);
        return;
    }

    output.scheduler.callback_sent(timestamp);
    send_frame_callbacks(state, identity);
}

fn send_frame_callbacks(state: &mut Ferese, output: &Output) {
    state.presentation_constraint_frame(output);
    let eligible = state.callback_outputs();
    if state.session_lock.active() {
        state.lock_frame_callbacks(output);
        return;
    }
    let visible_windows = state.space.elements().filter(|window| {
        state
            .windows
            .ids()
            .get(*window)
            .is_some_and(|id| state.window_belongs_to_output(*id, output))
    });
    for window in visible_windows {
        window.send_frame(output, state.start_time.elapsed(), None, |surface, _| {
            eligible.get(&surface.into()).cloned()
        });
    }
    let layers = layer_map_for_output(output)
        .layers()
        .cloned()
        .collect::<Vec<_>>();
    for layer in &layers {
        layer.send_frame(output, state.start_time.elapsed(), None, |surface, _| {
            eligible.get(&surface.into()).cloned()
        });
    }
    state.send_cursor_frame(output);
    state.space.refresh();
    state.popups.cleanup();
    layer_map_for_output(output).cleanup();
    if let Err(error) = state.display_handle.flush_clients() {
        tracing::debug!(%error, "failed to flush clients after DRM frame");
    }
}

fn select_outputs(
    device: &DirectDevice,
    monitors: &[Monitor],
    desired: &DesiredOutputConfiguration,
    low_power: bool,
) -> io::Result<OutputScan> {
    let drm = &device.drm;
    let inventory = device
        .inventory
        .as_ref()
        .ok_or_else(|| io::Error::other("connector inventory is unavailable"))?;
    let resources = &inventory.resources;
    let device_key = drm_device_key(drm);
    let mut selections = Vec::new();
    let mut connected_outputs = Vec::new();
    let mut candidates = Vec::new();
    let mut requests = Vec::new();

    let detected_iter = inventory
        .connectors
        .iter()
        .filter(|connector| connector.desktop);
    for detected in detected_iter {
        let connector = detected.info.clone();
        let key = format!("{device_key}/{}", connector);
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.key == key)
            .ok_or_else(|| io::Error::other("connector topology changed during reconciliation"))?;
        let desired_output = desired
            .outputs
            .iter()
            .find(|output| output.key == key)
            .ok_or_else(|| io::Error::other("connector missing from desired configuration"))?;
        let identity = monitor.identity.clone();
        let settings = desired_output.settings.clone();
        let selected_mode = settings
            .enabled
            .then(|| {
                let normal = select_mode(&connector, settings.mode)?;
                if settings.auto_refresh && low_power {
                    let modes = connector
                        .modes()
                        .iter()
                        .map(|mode| {
                            let (width, height) = mode.size();
                            (width, height, OutputMode::from(*mode).refresh)
                        })
                        .collect::<Vec<_>>();
                    super::power::low_refresh_mode(normal.size(), &modes)
                        .map(|index| connector.modes()[index])
                        .or(Some(normal))
                } else {
                    Some(normal)
                }
            })
            .flatten();
        if settings.enabled && selected_mode.is_none() && settings.mode.is_some() {
            return Err(io::Error::other(format!(
                "requested mode for {} is unavailable",
                connector
            )));
        }
        if let Some(mode) = selected_mode {
            let size =
                output_transform(settings.transform).transform_size(OutputMode::from(mode).size);
            let width = (size.w as f64 / settings.scale).round();
            let height = (size.h as f64 / settings.scale).round();
            if width < 1.0 || height < 1.0 || width > i32::MAX as f64 || height > i32::MAX as f64 {
                return Err(io::Error::other(format!(
                    "scale for {} creates invalid logical geometry",
                    connector
                )));
            }
            if let Some([x, y]) = settings.position
                && (x.checked_add(width as i32).is_none() || y.checked_add(height as i32).is_none())
            {
                return Err(io::Error::other(format!(
                    "position for {} overflows logical geometry",
                    connector
                )));
            }
        }
        connected_outputs.push(ConnectedOutputInfo {
            connector: connector.to_string(),
            connected: true,
            identity: identity.clone(),
            internal: monitor.internal,
            requested_enabled: settings.enabled,
            mirror_source: desired_output.mirror_source.clone(),
            enabled: settings.enabled,
            profile: desired.profile.clone(),
            requested_profile: desired.profile.clone(),
            auto_refresh: settings.auto_refresh,
            adaptive_sync_supported: detected.adaptive_sync_supported,
            adaptive_sync_requested: settings.adaptive_sync,
            adaptive_sync_enabled: false,
            tearing_supported: presentation_mode::tearing_capable(drm),
            allow_tearing: settings.allow_tearing,
            physical_size: connector.size(),
            current_mode: selected_mode.map(connected_mode_info),
            available_modes: connector
                .modes()
                .iter()
                .copied()
                .map(connected_mode_info)
                .collect(),
            scale: settings.scale,
            transform: settings.transform,
            configured_position: settings.position,
        });
        if !settings.enabled {
            tracing::info!(output = %connector, "output disabled by active profile");
            continue;
        }
        let Some(mode) = selected_mode else {
            continue;
        };

        let applied = device
            .outputs
            .iter()
            .find(|(_, output)| output.connector == connector.handle())
            .map(|(crtc, _)| *crtc);
        let kernel = connector
            .current_encoder()
            .and_then(|handle| drm.get_encoder(handle).ok())
            .and_then(|encoder| encoder.crtc());
        let mut compatible = Vec::new();
        for handle in connector.encoders() {
            if let Ok(encoder) = drm.get_encoder(*handle) {
                for crtc in resources.filter_crtcs(encoder.possible_crtcs()) {
                    if !compatible.contains(&crtc) && !leasing::crtc_is_leased(device, crtc) {
                        compatible.push(crtc);
                    }
                }
            }
        }
        requests.push(crtc_assignment::Candidates {
            applied,
            kernel,
            compatible,
        });
        candidates.push((
            connector,
            mode,
            settings,
            identity,
            desired_output.mirror_source.clone(),
        ));
    }

    let connector_iter = candidates
        .into_iter()
        .zip(crtc_assignment::assign(&requests));
    for ((connector, mode, settings, identity, mirror_source), crtc) in connector_iter {
        if let Some(crtc) = crtc {
            let supported = presentation_mode::capable(drm, connector.handle(), crtc)?;
            let mirrored = mirror_source.is_some()
                || desired.outputs.iter().any(|output| {
                    output.settings.enabled
                        && output.mirror_source.as_deref() == Some(identity.as_str())
                });
            presentation_mode::validate(settings.adaptive_sync, supported, mirrored)
                .map_err(io::Error::other)?;
            if let Some(info) = connected_outputs
                .iter_mut()
                .find(|info| info.identity == identity)
            {
                info.adaptive_sync_supported = supported;
            }
            selections.push(OutputSelection {
                name: protocol::output_name(&connector.to_string(), &identity, monitors),
                connector,
                crtc,
                mode,
                settings,
                identity,
                mirror_source,
            });
        }
    }

    let prefix = format!("{device_key}/");
    let selected = selections
        .iter()
        .map(|selection| format!("{prefix}{}", selection.connector))
        .collect::<Vec<_>>();
    // Also guard the rescan immediately before applying a device transaction:
    // hotplug can invalidate the earlier configuration-wide validation.
    validate_selected_outputs(
        desired
            .outputs
            .iter()
            .filter(|output| output.key.starts_with(&prefix)),
        &selected,
    )
    .map_err(io::Error::other)?;

    selections.sort_by_key(|selection| selection.settings.position.is_none());
    Ok(OutputScan {
        selections,
        connected_outputs,
    })
}

fn connected_mode_info(mode: DrmMode) -> ConnectedModeInfo {
    let (width, height) = mode.size();
    ConnectedModeInfo {
        width,
        height,
        refresh_millihertz: OutputMode::from(mode).refresh,
        preferred: mode.mode_type().contains(ModeTypeFlags::PREFERRED),
    }
}

fn select_mode(
    connector: &connector::Info,
    requested: Option<OutputModeRequest>,
) -> Option<DrmMode> {
    let fallback = || {
        connector
            .modes()
            .iter()
            .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| connector.modes().first())
            .copied()
    };
    let Some(requested) = requested else {
        return fallback();
    };

    let matching = connector
        .modes()
        .iter()
        .filter(|mode| mode.size() == (requested.width, requested.height));
    if let Some(refresh) = requested.refresh_millihertz {
        matching
            .min_by_key(|mode| {
                OutputMode::from(**mode)
                    .refresh
                    .unsigned_abs()
                    .abs_diff(refresh)
            })
            .filter(|mode| {
                OutputMode::from(**mode)
                    .refresh
                    .unsigned_abs()
                    .abs_diff(refresh)
                    <= 1_000
            })
            .copied()
    } else {
        matching
            .max_by_key(|mode| OutputMode::from(**mode).refresh)
            .copied()
    }
}

fn create_direct_output(
    state: &mut Ferese,
    drm: &mut DrmDevice,
    gbm: &GbmDevice<DrmDeviceFd>,
    renderer: &GlesRenderer,
    selection: OutputSelection,
    publish: bool,
) -> Result<DirectOutput, Box<dyn Error>> {
    let OutputSelection {
        name,
        connector,
        crtc,
        mode,
        settings,
        identity,
        mirror_source,
    } = selection;
    let drm_surface = drm.create_surface(crtc, mode, &[connector.handle()])?;
    let tearing_supported = presentation_mode::tearing_capable(drm);
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let node = DrmNode::from_file(gbm.as_fd())?;
    let render_node = node
        .node_with_type(NodeType::Render)
        .and_then(Result::ok)
        .unwrap_or(node);
    let mut surface = DrmCompositor::new(
        OutputModeSource::Static {
            size: OutputMode::from(mode).size,
            scale: settings.scale.into(),
            transform: output_transform(settings.transform),
        },
        drm_surface,
        None,
        allocator,
        GbmFramebufferExporter::new(gbm.clone(), Some(render_node).into()),
        [Fourcc::Argb8888, Fourcc::Abgr8888],
        renderer.dmabuf_formats(),
        drm.cursor_size(),
        Some(gbm.clone()),
    )?;
    if settings.adaptive_sync
        && surface.vrr_supported(connector.handle())?
            == smithay::backend::drm::VrrSupport::NotSupported
    {
        return Err("adaptive sync became unavailable before output activation".into());
    }
    surface.use_vrr(settings.adaptive_sync)?;
    // Publish only after DRM/GBM creation succeeds; failed activation must not
    // leave a phantom output/workspace that could receive evacuated windows.
    let (output, global) = create_output(
        state,
        name,
        &connector,
        mode,
        identity.clone(),
        &settings,
        publish && mirror_source.is_none(),
    );
    surface.set_output_mode_source((&output).into());
    let render_metrics = RenderMetrics::from_environment(output.name());
    let diagnostics = Diagnostics::from_environment(output.name());
    let dmabuf_feedback = feedback::OutputFeedback::new(
        render_node.dev_id(),
        node.dev_id(),
        renderer.dmabuf_formats(),
        surface.surface().plane_info().formats.clone(),
    );

    tracing::info!(?crtc, connector = %connector, "initialized DRM output");
    let mut scheduler =
        crate::frame_scheduler::FrameScheduler::new(refresh_duration_from_mode(mode));
    scheduler.set_variable_refresh(settings.adaptive_sync);
    Ok(DirectOutput {
        gamma: Default::default(),
        internal: internal_connector(connector.interface()),
        connector: connector.handle(),
        mode,
        settings: settings.clone(),
        output,
        global,
        identity,
        mirror_source,
        mirror_texture: None,
        mirror_materials: Default::default(),
        mirror_capture_texture: Default::default(),
        mirror_canvas: None,
        surface,
        primary_commit: None,
        capture_texture: Default::default(),
        render_metrics,
        diagnostics,
        dmabuf_feedback,
        frame_pending: false,
        completion: Default::default(),
        completion_timer: None,
        animation_fallback: AnimationFallback::default(),
        cursor_visible: false,
        power: OutputPower::default(),
        scheduler,
        render_timer: None,
        render_fence: None,
        callback_timer: None,
        lock_frame_pending: false,
        tearing_supported,
    })
}

fn create_output(
    state: &mut Ferese,
    name: String,
    connector: &connector::Info,
    mode: DrmMode,
    identity: String,
    settings: &OutputSettings,
    publish: bool,
) -> (Output, Option<GlobalId>) {
    let physical_size = connector.size().unwrap_or((0, 0));
    let edid_identity = identity
        .strip_prefix("edid:")
        .map(|value| value.split(':').collect::<Vec<_>>());
    let output = Output::new(
        name,
        PhysicalProperties {
            serial_number: String::new(),
            size: (physical_size.0 as i32, physical_size.1 as i32).into(),
            subpixel: Subpixel::from(connector.subpixel()),
            make: edid_identity
                .as_ref()
                .and_then(|parts| parts.first())
                .copied()
                .unwrap_or("Unknown")
                .into(),
            model: edid_identity
                .as_ref()
                .and_then(|parts| parts.get(1))
                .copied()
                .unwrap_or("Unknown")
                .into(),
        },
    );
    let output_mode = OutputMode::from(mode);

    let global = publish.then(|| output.create_global::<Ferese>(&state.display_handle));
    let modes = connector.modes().iter().copied().map(OutputMode::from);
    for mode in modes {
        output.add_mode(mode);
    }
    output.set_preferred(output_mode);
    let position = settings.position.unwrap_or_else(|| {
        let x = state
            .space
            .outputs()
            .filter_map(|output| state.space.output_geometry(output))
            .map(|geometry| geometry.loc.x + geometry.size.w)
            .max()
            .unwrap_or(0);
        [x, 0]
    });
    output.change_current_state(
        Some(output_mode),
        Some(output_transform(settings.transform)),
        Some(Scale::Fractional(settings.scale)),
        Some((position[0], position[1]).into()),
    );
    if publish {
        state.space.map_output(&output, (position[0], position[1]));
        state.register_output(&output, identity);
    }
    (output, global)
}

fn output_transform(transform: OutputTransform) -> Transform {
    match transform {
        OutputTransform::Normal => Transform::Normal,
        OutputTransform::Rotate90 => Transform::_90,
        OutputTransform::Rotate180 => Transform::_180,
        OutputTransform::Rotate270 => Transform::_270,
        OutputTransform::Flipped => Transform::Flipped,
        OutputTransform::Flipped90 => Transform::Flipped90,
        OutputTransform::Flipped180 => Transform::Flipped180,
        OutputTransform::Flipped270 => Transform::Flipped270,
    }
}

fn connector_edid(drm: &DrmDevice, connector: &connector::Info) -> Option<Vec<u8>> {
    drm.get_properties(connector.handle())
        .ok()
        .and_then(|properties| {
            properties.iter().find_map(|(handle, raw)| {
                let info = drm.get_property(*handle).ok()?;
                if info.name().to_bytes() != b"EDID" {
                    return None;
                }
                match info.value_type().convert_value(*raw) {
                    property::Value::Blob(blob) if blob != 0 => drm.get_property_blob(blob).ok(),
                    _ => None,
                }
            })
        })
}

fn drm_device_key(drm: &DrmDevice) -> String {
    let node = DrmNode::from_file(drm.as_fd()).expect("DRM descriptor has a device identity");
    let path = node
        .dev_path()
        .and_then(|path| {
            path.file_name()
                .map(|name| Path::new("/sys/class/drm").join(name).join("device"))
        })
        .and_then(|path| std::fs::canonicalize(path).ok());
    path.map_or_else(
        || format!("gpu-{}", node.dev_id()),
        |path| path.to_string_lossy().into_owned(),
    )
}

fn resolve_output_positions(
    backend: &DirectBackendState,
    desired: &mut DesiredOutputConfiguration,
) -> Result<(), String> {
    let mut placements = Vec::new();
    for device in backend.devices.values() {
        let scan = select_outputs(device, &backend.monitors, desired, backend.low_power)
            .map_err(|error| error.to_string())?;
        for selection in scan.selections {
            let size = output_transform(selection.settings.transform)
                .transform_size(OutputMode::from(selection.mode).size);
            let key = format!("{}/{}", drm_device_key(&device.drm), selection.connector);
            let previous = device
                .outputs
                .values()
                .find(|output| output.identity == selection.identity)
                .filter(|output| output.mirror_source.is_none())
                .map(|output| {
                    let position = output.output.current_location();
                    [position.x, position.y]
                });
            placements.push(crate::output_policy::Placement {
                key,
                size: (
                    (size.w as f64 / selection.settings.scale).round() as i32,
                    (size.h as f64 / selection.settings.scale).round() as i32,
                ),
                previous,
            });
        }
    }
    crate::output_policy::arrange_positions(desired, &placements)
}

fn refresh_monitor_policy(state: &mut Ferese) -> Result<(), String> {
    let backend = state
        .direct_backend
        .as_mut()
        .ok_or("direct backend unavailable")?;
    let mut discovered = Vec::new();
    for device in backend.devices.values() {
        let drm = &device.drm;
        let key = drm_device_key(drm);
        if let Some(inventory) = &device.inventory {
            let detected_iter = inventory
                .connectors
                .iter()
                .filter(|connector| connector.desktop);
            for detected in detected_iter {
                discovered.push((
                    format!("{key}/{}", detected.info),
                    detected.info.clone(),
                    detected.edid.clone(),
                ));
            }
        }
    }
    discovered.sort_by(|a, b| a.0.cmp(&b.0));
    let identities = backend.identities.resolve(
        &discovered
            .iter()
            .map(|(key, _, edid)| (key.clone(), edid.as_deref().and_then(Identity::from_edid)))
            .collect::<Vec<_>>(),
    );
    backend.monitors = discovered
        .into_iter()
        .zip(identities)
        .map(|((key, connector, edid), identity)| {
            let mut aliases = vec![format!("drm:{connector}")];
            if let Some(edid) = edid {
                aliases.push(format!("drm-edid:{:016x}", stable_hash(&edid)));
            }
            Monitor {
                key,
                identity,
                connector: connector.to_string(),
                aliases,
                internal: internal_connector(connector.interface()),
                usable: !connector.modes().is_empty(),
            }
        })
        .collect();
    if backend.confirmation.is_some() {
        let previous_keys = backend
            .applied_outputs
            .outputs
            .iter()
            .map(|output| &output.key)
            .collect::<HashSet<_>>();
        let current_keys = backend
            .monitors
            .iter()
            .map(|monitor| &monitor.key)
            .collect::<HashSet<_>>();
        if previous_keys != current_keys || backend.confirmation_lid != backend.lid.closed() {
            backend.confirmation = None;
            if let Some(token) = backend.confirmation_timer.take() {
                state.loop_handle.remove(token);
            }
        }
    }
    let previous_override = (
        backend.manual_outputs.profile.clone(),
        backend.manual_outputs.internal,
        backend.manual_outputs.layout,
    );
    backend.manual_outputs.observe(
        &backend.monitors,
        backend.lid.closed(),
        &state.output_profiles,
    );
    if previous_override
        != (
            backend.manual_outputs.profile.clone(),
            backend.manual_outputs.internal,
            backend.manual_outputs.layout,
        )
    {
        backend.confirmation = None;
        if let Some(token) = backend.confirmation_timer.take() {
            state.loop_handle.remove(token);
        }
    }
    let mut desired = crate::output_policy::select_profile(
        &backend.monitors,
        backend.lid.closed(),
        &state.output_profiles,
        &backend.manual_outputs,
    );
    resolve_output_positions(backend, &mut desired)?;
    tracing::debug!(profile = ?desired.profile, changed = ?crate::output_policy::changed_outputs(&backend.desired_outputs, &desired), "selected monitor configuration");
    if let Some(previous) = backend.revert_outputs.take() {
        desired = crate::output_policy::restore_configuration(&previous, &backend.monitors);
        resolve_output_positions(backend, &mut desired)?;
    }
    backend.desired_outputs = desired;
    Ok(())
}

fn stable_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

impl DirectBackendState {
    pub fn record_presentation(
        &mut self,
        node: DrmNode,
        crtc: crtc::Handle,
        time: DrmEventTime,
        sequence: u32,
    ) -> Option<Duration> {
        let missed = self
            .devices
            .get(&node)
            .and_then(|device| device.outputs.get(&crtc))
            .map_or(0, |output| output.scheduler.missed_deadlines);
        self.presentation.get_mut(&(node, crtc)).and_then(|clock| {
            clock.missed_deadlines = missed;
            clock.record(time, sequence)
        })
    }
}

impl PresentationClock {
    fn reset_timing(&mut self) {
        self.last_presentation = None;
    }

    fn set_refresh(&mut self, refresh_millihertz: i32) {
        if refresh_millihertz > 0 {
            self.refresh_interval = Some(Duration::from_nanos(
                1_000_000_000_000_u64 / refresh_millihertz as u64,
            ));
        }
    }

    fn record(&mut self, time: DrmEventTime, sequence: u32) -> Option<Duration> {
        let DrmEventTime::Monotonic(time) = time else {
            tracing::warn!(
                sequence,
                "DRM driver reported a realtime page-flip timestamp"
            );
            return None;
        };
        let delta = self
            .last_presentation
            .map(|previous| time.saturating_sub(previous));

        self.last_presentation = Some(time);
        self.presented_frames = self.presented_frames.saturating_add(1);
        delta
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    fn three_monitor_plan(layout: crate::config::OutputLayout) -> DesiredOutputConfiguration {
        let monitors = ["A", "B", "C"].map(|key| Monitor {
            key: key.into(),
            connector: key.into(),
            identity: key.into(),
            aliases: vec![],
            internal: key == "A",
            usable: true,
        });
        let profile = OutputProfile {
            name: "three".into(),
            outputs: monitors
                .iter()
                .map(|monitor| crate::output_policy::defaults(monitor.key.clone()))
                .collect(),
            layout,
            confirm_timeout: 15,
            lid_policy: crate::config::LidPolicy::Ignore,
            lid_closed: None,
            mirror_source: None,
        };
        crate::output_policy::select_profile(
            &monitors,
            false,
            &[profile],
            &ManualOverride::default(),
        )
    }

    #[test]
    fn deferred_gpu_does_not_block_due_retries_or_hide_other_missing_outputs() {
        let mut desired = three_monitor_plan(crate::config::OutputLayout::Extend);
        desired.outputs[0].key = "gpu-one/A".into();
        desired.outputs[1].key = "gpu-two/B".into();
        desired.outputs[2].key = "gpu-two/C".into();
        let selected = ["gpu-one/A".into()];
        let deferred = ["gpu-two/".into()];
        assert!(validate_output_plan(&desired, &selected, &deferred, || Ok(())).is_ok());
        assert_eq!(
            validate_output_plan(&desired, &selected, &deferred, || Err(
                "KMS test failed".into()
            )),
            Err("KMS test failed".into())
        );
        assert!(validate_output_plan(&desired, &[], &deferred, || Ok(())).is_err());
        assert!(validate_output_plan(&desired, &selected, &[], || Ok(())).is_err());
        desired.outputs[2].key = "gpu-other/C".into();
        assert!(
            validate_output_plan(&desired, &selected, &deferred, || Ok(()))
                .unwrap_err()
                .contains("gpu-other/C")
        );
    }

    #[test]
    fn complete_live_plan_still_requires_hardware_success() {
        let desired = three_monitor_plan(crate::config::OutputLayout::Extend);
        let selected = vec!["A".into(), "B".into(), "C".into()];
        assert!(validate_output_plan(&desired, &selected, &[], || Ok(())).is_ok());
        assert_eq!(
            validate_output_plan(&desired, &selected, &[], || Err(
                "atomic TEST_ONLY failed".into()
            )),
            Err("atomic TEST_ONLY failed".into())
        );
    }

    #[test]
    fn requiredness_does_not_allow_a_connected_enabled_output_to_be_omitted() {
        let mut desired = three_monitor_plan(crate::config::OutputLayout::Extend);
        desired.outputs[2].settings.required = false;
        let error =
            validate_output_plan(&desired, &["A".into(), "B".into()], &[], || Ok(())).unwrap_err();
        assert!(error.contains("C"));
        desired.outputs[2].settings.enabled = false;
        assert!(validate_output_plan(&desired, &["A".into(), "B".into()], &[], || Ok(())).is_ok());
    }

    #[test]
    fn disconnected_monitors_affect_profile_matching_and_do_not_need_assignments() {
        let connected = ["A", "B"].map(|key| Monitor {
            key: key.into(),
            connector: key.into(),
            identity: key.into(),
            aliases: vec![],
            internal: key == "A",
            usable: true,
        });
        let mut profile = OutputProfile {
            name: "dock".into(),
            outputs: ["A", "B", "C"]
                .map(|key| crate::output_policy::defaults(key.into()))
                .into(),
            layout: crate::config::OutputLayout::Extend,
            confirm_timeout: 15,
            lid_policy: crate::config::LidPolicy::Ignore,
            lid_closed: None,
            mirror_source: None,
        };
        for required in [true, false] {
            profile.outputs[2].required = required;
            let desired = crate::output_policy::select_profile(
                &connected,
                false,
                &[profile.clone()],
                &ManualOverride::default(),
            );
            assert_eq!(
                desired.profile.as_deref(),
                if required { None } else { Some("dock") }
            );
            assert_eq!(desired.outputs.len(), 2);
            assert!(
                validate_output_plan(&desired, &["A".into(), "B".into()], &[], || Ok(())).is_ok()
            );
        }
    }

    #[test]
    fn device_scans_cannot_satisfy_another_gpus_missing_output() {
        let mut desired = three_monitor_plan(crate::config::OutputLayout::Extend);
        desired.outputs[0].key = "gpu-one/A".into();
        desired.outputs[1].key = "gpu-two/B".into();
        desired.outputs[2].key = "gpu-two/C".into();
        let selected = vec!["gpu-one/A".into(), "gpu-two/B".into(), "gpu-one/C".into()];
        assert!(validate_output_plan(&desired, &selected, &[], || Ok(())).is_err());
        // A device-local rescan checks only its own enabled outputs.
        assert!(
            validate_selected_outputs(
                desired
                    .outputs
                    .iter()
                    .filter(|output| output.key.starts_with("gpu-one/")),
                &selected
            )
            .is_ok()
        );
    }

    #[test]
    fn impossible_assignment_reaches_live_validation_as_an_error() {
        let desired = three_monitor_plan(crate::config::OutputLayout::Extend);
        let crtc = std::num::NonZeroU32::new(1).unwrap().into();
        let requests = (0..3)
            .map(|_| crtc_assignment::Candidates {
                applied: None,
                kernel: None,
                compatible: vec![crtc],
            })
            .collect::<Vec<_>>();
        let selected = desired
            .outputs
            .iter()
            .zip(crtc_assignment::assign(&requests))
            .filter(|(_, assignment)| assignment.is_some())
            .map(|(output, _)| output.key.clone())
            .collect::<Vec<_>>();
        assert!(validate_output_plan(&desired, &selected, &[], || Ok(())).is_err());
    }

    #[test]
    fn feasible_three_monitor_assignment_passes_live_plan_validation() {
        let desired = three_monitor_plan(crate::config::OutputLayout::Extend);
        let requests =
            [&[1, 2][..], &[1, 3][..], &[1, 3][..]].map(|compatible| crtc_assignment::Candidates {
                applied: None,
                kernel: None,
                compatible: compatible
                    .iter()
                    .map(|value| std::num::NonZeroU32::new(*value).unwrap().into())
                    .collect(),
            });
        let selected = desired
            .outputs
            .iter()
            .zip(crtc_assignment::assign(&requests))
            .filter(|(_, assignment)| assignment.is_some())
            .map(|(output, _)| output.key.clone())
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 3);
        assert!(validate_output_plan(&desired, &selected, &[], || Ok(())).is_ok());
    }

    #[test]
    fn live_plan_rejects_partial_extend_and_mirror_before_hardware_validation() {
        for layout in [
            crate::config::OutputLayout::Extend,
            crate::config::OutputLayout::Mirror,
        ] {
            let desired = three_monitor_plan(layout);
            for omitted in ["A", "B", "C"] {
                let selected = ["A", "B", "C"]
                    .into_iter()
                    .filter(|key| *key != omitted)
                    .map(String::from)
                    .collect::<Vec<_>>();
                let probed = std::cell::Cell::new(false);
                let result = validate_output_plan(&desired, &selected, &[], || {
                    probed.set(true);
                    Ok(()) // A driver can accept the incomplete two-output request.
                });
                assert!(
                    result.is_err(),
                    "accepted a partial {layout:?} configuration without {omitted}"
                );
                assert!(!probed.get());
            }
        }
    }

    #[test]
    fn repeated_unconfirmed_changes_keep_original_rollback_and_manual_state() {
        let a = ("confirmed A", "automatic");
        let b = confirmation_baseline(None, a);
        let c = confirmation_baseline(Some(b), ("unconfirmed B", "profile B"));
        assert_eq!(c, a);
        let d = confirmation_baseline(Some(c), ("unconfirmed C", "profile C"));
        assert_eq!(d, a);
        assert_eq!(
            confirmation_baseline(None, ("confirmed C", "profile C")),
            ("confirmed C", "profile C")
        );
    }

    #[test]
    fn lid_only_hides_panels_with_a_usable_external_output() {
        assert!(lid_hides_panel(true, true));
        assert!(!lid_hides_panel(false, true));
        assert!(!lid_hides_panel(true, false));
        assert!(!lid_hides_panel(false, false));
    }

    #[test]
    fn panel_detection_uses_connector_type_not_monitor_name() {
        assert!(internal_connector(
            connector::Interface::EmbeddedDisplayPort
        ));
        assert!(internal_connector(connector::Interface::LVDS));
        assert!(internal_connector(connector::Interface::DSI));
        assert!(!internal_connector(connector::Interface::HDMIA));
        assert!(!internal_connector(connector::Interface::DisplayPort));
    }

    #[test]
    fn connector_identity_hash_is_stable() {
        assert_eq!(stable_hash(b"hello"), 0xa430_d846_80aa_bd0b);
    }

    #[test]
    fn presentation_clock_uses_measured_monotonic_deltas() {
        let mut clock = PresentationClock::default();
        clock.set_refresh(60_000);

        assert_eq!(
            clock.record(DrmEventTime::Monotonic(Duration::from_millis(100)), 1),
            None
        );
        assert_eq!(
            clock.record(DrmEventTime::Monotonic(Duration::from_millis(116)), 2),
            Some(Duration::from_millis(16))
        );
        assert_eq!(clock.presented_frames, 2);
        assert_eq!(clock.missed_deadlines, 0);
    }

    #[test]
    fn presentation_clock_rejects_realtime_timestamps() {
        let mut clock = PresentationClock::default();

        assert_eq!(
            clock.record(DrmEventTime::Realtime(SystemTime::now()), 1),
            None
        );
        assert_eq!(clock.presented_frames, 0);
    }

    #[test]
    fn presentation_clock_does_not_count_idle_gaps_as_missed_deadlines() {
        let mut clock = PresentationClock::default();
        clock.set_refresh(60_000);

        clock.record(DrmEventTime::Monotonic(Duration::ZERO), 1);
        clock.record(DrmEventTime::Monotonic(Duration::from_millis(50)), 2);

        assert_eq!(clock.missed_deadlines, 0);
    }

    #[test]
    fn presentation_clock_excludes_inactive_session_time() {
        let mut clock = PresentationClock::default();
        clock.set_refresh(60_000);
        clock.record(DrmEventTime::Monotonic(Duration::from_millis(100)), 1);

        clock.reset_timing();

        assert_eq!(
            clock.record(DrmEventTime::Monotonic(Duration::from_secs(10)), 2),
            None
        );
        assert_eq!(clock.presented_frames, 2);
        assert_eq!(clock.missed_deadlines, 0);
    }
}
