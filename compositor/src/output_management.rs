//! Transactional standard monitor configuration with per-manager object ownership.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::{
    zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::ZwlrOutputModeV1,
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::Transform;

use crate::Ferese;
use crate::backends::direct::{ConnectedModeInfo, ConnectedOutputInfo};
use crate::config::{OutputModeRequest, OutputSettings, OutputTransform};

const VERSION: u32 = 4;

#[derive(Clone, Debug, PartialEq)]
struct Head {
    info: ConnectedOutputInfo,
    description: String,
    name: String,
    position: [i32; 2],
}

struct HeadInstance {
    resource: ZwlrOutputHeadV1,
    modes: Vec<(ConnectedModeInfo, ZwlrOutputModeV1)>,
    snapshot: Head,
}

struct Manager {
    resource: ZwlrOutputManagerV1,
    heads: HashMap<String, HeadInstance>,
}

pub(crate) struct OutputManagement {
    serial: u32,
    inventory: Vec<Head>,
    managers: Vec<Manager>,
}

#[derive(Clone)]
pub(crate) struct HeadData {
    manager: ZwlrOutputManagerV1,
    identity: String,
}

pub(crate) struct ModeData {
    head: ZwlrOutputHeadV1,
    mode: ConnectedModeInfo,
}

#[derive(Default)]
struct PendingHead {
    mode: Option<OutputModeRequest>,
    position: Option<[i32; 2]>,
    transform: Option<OutputTransform>,
    scale: Option<f64>,
    adaptive_sync: Option<bool>,
    invalid_mode: bool,
}

struct Configuration {
    serial: u32,
    manager: ZwlrOutputManagerV1,
    used: bool,
    resource: Option<smithay::reexports::wayland_server::Weak<ZwlrOutputConfigurationV1>>,
    heads: HashMap<String, Option<PendingHead>>,
    configured_handles: HashMap<String, ZwlrOutputHeadV1>,
    children: Vec<smithay::reexports::wayland_server::Weak<ZwlrOutputConfigurationHeadV1>>,
}

pub(crate) struct ConfigurationData(Arc<Mutex<Configuration>>);

pub(crate) struct ConfigurationHeadData {
    configuration: Arc<Mutex<Configuration>>,
    head: ZwlrOutputHeadV1,
    identity: String,
}

impl OutputManagement {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        display.create_global::<Ferese, ZwlrOutputManagerV1, _>(VERSION, ());
        Self {
            serial: 1,
            inventory: Vec::new(),
            managers: Vec::new(),
        }
    }

    fn publish(&mut self, display: &DisplayHandle, inventory: Vec<Head>) {
        if self.inventory == inventory {
            return;
        }
        self.inventory = inventory;
        self.serial = self.serial.wrapping_add(1);
        for manager in &mut self.managers {
            publish_manager(display, manager, &self.inventory);
            manager.resource.done(self.serial);
        }
    }
}

pub(crate) fn sync(state: &mut Ferese) {
    if !state.output_management.managers.is_empty() {
        refresh(state);
    }
}

fn refresh(state: &mut Ferese) {
    let inventory = if let Some(backend) = state.direct_backend.as_ref() {
        let connected = backend
            .connected_outputs
            .iter()
            .filter(|info| info.connected);
        if connected.clone().count() == state.output_management.inventory.len()
            && connected
                .clone()
                .zip(&state.output_management.inventory)
                .all(|(info, head)| {
                    info == &head.info
                        && backend.physical_output(&info.identity).map_or(
                            info.configured_position.unwrap_or([0, 0]),
                            |output| {
                                let location = output.current_location();
                                [location.x, location.y]
                            },
                        ) == head.position
                })
        {
            return;
        }
        connected
            .map(|info| {
                let output = backend.physical_output(&info.identity);
                Head {
                    name: output.map_or_else(|| backend.protocol_output_name(info), Output::name),
                    info: info.clone(),
                    description: output.map_or_else(
                        || {
                            let mut identity = info
                                .identity
                                .strip_prefix("edid:")
                                .into_iter()
                                .flat_map(|identity| identity.split(':'));
                            let make = identity.next().unwrap_or("Unknown");
                            let model = identity.next().unwrap_or("Unknown");
                            format!("{make} - {model} - {}", backend.protocol_output_name(info))
                        },
                        Output::description,
                    ),
                    position: output.map_or(info.configured_position.unwrap_or([0, 0]), |output| {
                        let location = output.current_location();
                        [location.x, location.y]
                    }),
                }
            })
            .collect()
    } else {
        state
            .space
            .outputs()
            .map(|output| {
                let mode = |mode: smithay::output::Mode| ConnectedModeInfo {
                    width: mode.size.w.try_into().unwrap_or(u16::MAX),
                    height: mode.size.h.try_into().unwrap_or(u16::MAX),
                    refresh_millihertz: mode.refresh,
                    preferred: output.preferred_mode() == Some(mode),
                };
                let mut modes = output.modes().into_iter().map(mode).collect::<Vec<_>>();
                let current_mode = output.current_mode().map(mode);
                if let Some(current) = current_mode
                    && !modes.contains(&current)
                {
                    modes.push(current);
                }
                let location = output.current_location();
                Head {
                    info: ConnectedOutputInfo {
                        connector: output.name(),
                        connected: true,
                        identity: format!("nested:{}", output.name()),
                        internal: false,
                        requested_enabled: true,
                        mirror_source: None,
                        enabled: current_mode.is_some(),
                        profile: None,
                        requested_profile: None,
                        auto_refresh: false,
                        adaptive_sync_supported: false,
                        tearing_supported: false,
                        allow_tearing: false,
                        adaptive_sync_requested: false,
                        adaptive_sync_enabled: false,
                        physical_size: None,
                        current_mode,
                        available_modes: modes,
                        scale: output.current_scale().fractional_scale(),
                        transform: from_transform(output.current_transform()),
                        configured_position: Some([location.x, location.y]),
                    },
                    name: output.name(),
                    description: output.description(),
                    position: [location.x, location.y],
                }
            })
            .collect()
    };
    state
        .output_management
        .publish(&state.display_handle, inventory);
}

fn finish_head(head: &HeadInstance) {
    for (_, mode) in &head.modes {
        mode.finished();
    }
    head.resource.finished();
}

fn publish_manager(display: &DisplayHandle, manager: &mut Manager, inventory: &[Head]) {
    manager.heads.retain(|identity, head| {
        let live = inventory.iter().any(|next| {
            &next.info.identity == identity
                && next.name == head.snapshot.name
                && next.info.physical_size == head.snapshot.info.physical_size
                && next.description == head.snapshot.description
        });
        if !live {
            finish_head(head);
        }
        live
    });
    let Some(client) = manager.resource.client() else {
        return;
    };
    for next in inventory {
        let identity = &next.info.identity;
        let introduced = !manager.heads.contains_key(identity);
        if introduced {
            let Ok(resource) = client.create_resource::<ZwlrOutputHeadV1, _, Ferese>(
                display,
                manager.resource.version(),
                HeadData {
                    manager: manager.resource.clone(),
                    identity: identity.clone(),
                },
            ) else {
                continue;
            };
            manager.resource.head(&resource);
            resource.name(next.name.clone());
            resource.description(next.description.clone());
            if let Some((width, height)) = next.info.physical_size {
                resource.physical_size(
                    width.min(i32::MAX as u32) as i32,
                    height.min(i32::MAX as u32) as i32,
                );
            }
            manager.heads.insert(
                identity.clone(),
                HeadInstance {
                    resource,
                    modes: Vec::new(),
                    snapshot: next.clone(),
                },
            );
        }
        let head = manager.heads.get_mut(identity).expect("head was populated");
        if !head.resource.is_alive() {
            continue;
        }
        head.modes.retain(|(mode, resource)| {
            let live =
                next.info.available_modes.contains(mode) || next.info.current_mode == Some(*mode);
            if !live {
                resource.finished();
            }
            live
        });
        for mode in next
            .info
            .available_modes
            .iter()
            .chain(next.info.current_mode.iter())
        {
            if head.modes.iter().any(|(existing, _)| existing == mode) {
                continue;
            }
            let Ok(resource) = client.create_resource::<ZwlrOutputModeV1, _, Ferese>(
                display,
                manager.resource.version().min(3),
                ModeData {
                    head: head.resource.clone(),
                    mode: *mode,
                },
            ) else {
                continue;
            };
            head.resource.mode(&resource);
            resource.size(i32::from(mode.width), i32::from(mode.height));
            resource.refresh(mode.refresh_millihertz);
            if mode.preferred {
                resource.preferred();
            }
            head.modes.push((*mode, resource));
        }
        if introduced || head.snapshot.info.enabled != next.info.enabled {
            head.resource.enabled(i32::from(next.info.enabled));
        }
        if head.resource.version() >= 4
            && (introduced
                || head.snapshot.info.adaptive_sync_enabled != next.info.adaptive_sync_enabled)
        {
            head.resource
                .adaptive_sync(if next.info.adaptive_sync_enabled {
                    zwlr_output_head_v1::AdaptiveSyncState::Enabled
                } else {
                    zwlr_output_head_v1::AdaptiveSyncState::Disabled
                });
        }
        if next.info.enabled {
            let activated = introduced || !head.snapshot.info.enabled;
            if (activated || head.snapshot.info.current_mode != next.info.current_mode)
                && let Some((_, mode)) = head.modes.iter().find(|(mode, resource)| {
                    Some(*mode) == next.info.current_mode && resource.is_alive()
                })
            {
                head.resource.current_mode(mode);
            }
            if activated || head.snapshot.position != next.position {
                head.resource.position(next.position[0], next.position[1]);
            }
            if activated || head.snapshot.info.transform != next.info.transform {
                head.resource.transform(to_transform(next.info.transform));
            }
            if activated || head.snapshot.info.scale != next.info.scale {
                head.resource.scale(next.info.scale);
            }
        }
        head.snapshot = next.clone();
    }
}

fn to_transform(transform: OutputTransform) -> wl_output::Transform {
    match transform {
        OutputTransform::Normal => wl_output::Transform::Normal,
        OutputTransform::Rotate90 => wl_output::Transform::_90,
        OutputTransform::Rotate180 => wl_output::Transform::_180,
        OutputTransform::Rotate270 => wl_output::Transform::_270,
        OutputTransform::Flipped => wl_output::Transform::Flipped,
        OutputTransform::Flipped90 => wl_output::Transform::Flipped90,
        OutputTransform::Flipped180 => wl_output::Transform::Flipped180,
        OutputTransform::Flipped270 => wl_output::Transform::Flipped270,
    }
}

fn from_transform(transform: Transform) -> OutputTransform {
    match transform {
        Transform::Normal => OutputTransform::Normal,
        Transform::_90 => OutputTransform::Rotate90,
        Transform::_180 => OutputTransform::Rotate180,
        Transform::_270 => OutputTransform::Rotate270,
        Transform::Flipped => OutputTransform::Flipped,
        Transform::Flipped90 => OutputTransform::Flipped90,
        Transform::Flipped180 => OutputTransform::Flipped180,
        Transform::Flipped270 => OutputTransform::Flipped270,
    }
}

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for Ferese {
    fn bind(
        state: &mut Self,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        refresh(state);
        let resource = init.init(resource, ());
        let mut manager = Manager {
            resource,
            heads: HashMap::new(),
        };
        publish_manager(display, &mut manager, &state.output_management.inventory);
        manager.resource.done(state.output_management.serial);
        state.output_management.managers.push(manager);
    }

    fn can_view(client: Client, _data: &()) -> bool {
        crate::handlers::security_context::unrestricted_client(&client)
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                let configuration = Arc::new(Mutex::new(Configuration {
                    serial,
                    manager: resource.clone(),
                    used: false,
                    resource: None,
                    heads: HashMap::new(),
                    configured_handles: HashMap::new(),
                    children: Vec::new(),
                }));
                let instance = init.init(id, ConfigurationData(configuration.clone()));
                configuration
                    .lock()
                    .expect("output configuration mutex poisoned")
                    .resource = Some(instance.downgrade());
            }
            zwlr_output_manager_v1::Request::Stop => {
                state
                    .output_management
                    .managers
                    .retain(|manager| &manager.resource != resource);
                resource.finished();
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &ZwlrOutputManagerV1, _data: &()) {
        state
            .output_management
            .managers
            .retain(|manager| &manager.resource != resource);
    }
}

impl Dispatch<ZwlrOutputHeadV1, HeadData> for Ferese {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwlrOutputHeadV1,
        _request: zwlr_output_head_v1::Request,
        _data: &HeadData,
        _display: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputModeV1, ModeData> for Ferese {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwlrOutputModeV1,
        _request: <ZwlrOutputModeV1 as Resource>::Request,
        _data: &ModeData,
        _display: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
    }
}

fn settings(head: &Head, pending: Option<&PendingHead>) -> Result<OutputSettings, &'static str> {
    let mut settings = crate::output_policy::defaults(head.info.identity.clone());
    settings.auto_refresh = false;
    settings.allow_tearing = head.info.allow_tearing;
    settings.enabled = pending.is_some();
    let Some(pending) = pending else {
        return Ok(settings);
    };
    if pending.invalid_mode {
        return Err("retired mode requested");
    }
    settings.mode = pending.mode.or_else(|| {
        head.info
            .current_mode
            .or_else(|| head.info.available_modes.first().copied())
            .map(|mode| OutputModeRequest {
                width: mode.width,
                height: mode.height,
                refresh_millihertz: Some(mode.refresh_millihertz.unsigned_abs()),
            })
    });
    settings.scale = pending.scale.unwrap_or(head.info.scale);
    settings.adaptive_sync = pending
        .adaptive_sync
        .unwrap_or(head.info.adaptive_sync_enabled);
    if settings.adaptive_sync && !head.info.adaptive_sync_supported {
        return Err("adaptive sync is not supported by this head");
    }
    settings.transform = pending.transform.unwrap_or(head.info.transform);
    settings.position = Some(pending.position.unwrap_or(head.position));
    let mode = settings.mode.ok_or("enabled head has no mode")?;
    let rotated = matches!(
        settings.transform,
        OutputTransform::Rotate90
            | OutputTransform::Rotate270
            | OutputTransform::Flipped90
            | OutputTransform::Flipped270
    );
    let dimensions = if rotated {
        [mode.height, mode.width]
    } else {
        [mode.width, mode.height]
    };
    let position = settings.position.expect("position was populated");
    for axis in 0..2 {
        let extent = (f64::from(dimensions[axis]) / settings.scale).round();
        if !extent.is_finite()
            || !(1.0..=f64::from(i32::MAX)).contains(&extent)
            || position[axis].checked_add(extent as i32).is_none()
        {
            return Err("logical output dimensions exceed supported coordinates");
        }
    }
    Ok(settings)
}

impl Dispatch<ZwlrOutputConfigurationV1, ConfigurationData> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &ConfigurationData,
        display: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_v1::{Error, Request};
        let mut configuration = data.0.lock().expect("output configuration mutex poisoned");
        if matches!(request, Request::Destroy) {
            configuration.used = true;
            let children = std::mem::take(&mut configuration.children);
            drop(configuration);
            for child in children {
                if let Ok(child) = child.upgrade() {
                    let _ = display.backend_handle().destroy_object::<Self>(&child.id());
                }
            }
            return;
        }
        if configuration.used {
            resource.post_error(
                Error::AlreadyUsed,
                "configuration has already been submitted",
            );
            return;
        }
        match request {
            Request::EnableHead { id, head } => {
                let Some(head_data) = head.data::<HeadData>() else {
                    return;
                };
                // Cross-manager and retired handles never identify this snapshot.
                let identity = head_data.identity.clone();
                if configuration.heads.contains_key(&identity) {
                    resource.post_error(Error::AlreadyConfiguredHead, "head configured twice");
                } else if head_data.manager != configuration.manager {
                    configuration
                        .heads
                        .insert(format!("invalid:{identity}"), None);
                } else {
                    configuration
                        .heads
                        .insert(identity.clone(), Some(PendingHead::default()));
                }
                configuration
                    .configured_handles
                    .insert(identity.clone(), head.clone());
                let child = init.init(
                    id,
                    ConfigurationHeadData {
                        configuration: data.0.clone(),
                        head,
                        identity,
                    },
                );
                configuration.children.push(child.downgrade());
            }
            Request::DisableHead { head } => {
                let Some(head_data) = head.data::<HeadData>() else {
                    return;
                };
                let identity = if head_data.manager == configuration.manager {
                    head_data.identity.clone()
                } else {
                    format!("invalid:{}", head_data.identity)
                };
                if configuration.heads.contains_key(&identity) {
                    resource.post_error(Error::AlreadyConfiguredHead, "head configured twice");
                } else {
                    configuration.heads.insert(identity.clone(), None);
                }
                configuration.configured_handles.insert(identity, head);
            }
            Request::Apply | Request::Test => {
                let test_only = matches!(request, Request::Test);
                configuration.used = true;
                // Refresh before checking the serial: an earlier callback in this
                // event batch may already have changed the actual inventory.
                sync(state);
                if configuration.serial != state.output_management.serial {
                    resource.cancelled();
                    return;
                }
                let manager = state
                    .output_management
                    .managers
                    .iter()
                    .find(|manager| manager.resource == configuration.manager);
                let Some(manager) = manager else {
                    resource.cancelled();
                    return;
                };
                if state
                    .output_management
                    .inventory
                    .iter()
                    .any(|head| !configuration.heads.contains_key(&head.info.identity))
                {
                    resource.post_error(
                        Error::UnconfiguredHead,
                        "configuration omitted a connected head",
                    );
                    return;
                }
                if configuration
                    .heads
                    .keys()
                    .any(|identity| !manager.heads.contains_key(identity))
                    || configuration
                        .configured_handles
                        .iter()
                        .any(|(identity, resource)| {
                            manager
                                .heads
                                .get(identity)
                                .is_none_or(|head| &head.resource != resource)
                        })
                {
                    resource.cancelled();
                    return;
                }
                let requested = state
                    .output_management
                    .inventory
                    .iter()
                    .map(|head| {
                        settings(
                            head,
                            configuration
                                .heads
                                .get(&head.info.identity)
                                .and_then(Option::as_ref),
                        )
                        .map(|settings| (head.info.identity.clone(), settings))
                    })
                    .collect::<Result<Vec<_>, _>>();
                drop(configuration);
                match requested.and_then(|requested| {
                    crate::backends::direct::configure_outputs(state, &requested, test_only)
                        .map_err(|_| "hardware rejected configuration")
                }) {
                    Ok(()) => resource.succeeded(),
                    Err(error) => {
                        tracing::debug!(%error, "standard output configuration rejected");
                        resource.failed();
                    }
                }
                sync(state);
            }
            Request::Destroy => unreachable!(),
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ConfigurationHeadData> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &ConfigurationHeadData,
        _display: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_head_v1::{Error, Request};
        let mut configuration = data
            .configuration
            .lock()
            .expect("output configuration mutex poisoned");
        if configuration.used {
            if let Some(parent) = configuration
                .resource
                .as_ref()
                .and_then(|parent| parent.upgrade().ok())
            {
                parent.post_error(
                    zwlr_output_configuration_v1::Error::AlreadyUsed,
                    "configuration has already been submitted",
                );
            }
            return;
        }
        let Some(Some(pending)) = configuration.heads.get_mut(&data.identity) else {
            return;
        };
        let repeated = match request {
            Request::SetMode { mode } => {
                let mode_resource = mode;
                let Some(mode) = mode_resource.data::<ModeData>() else {
                    return;
                };
                if mode.head != data.head {
                    resource.post_error(Error::InvalidMode, "mode belongs to another head");
                    return;
                }
                pending.invalid_mode = !state.output_management.managers.iter().any(|manager| {
                    manager.heads.values().any(|head| {
                        head.resource == data.head
                            && head.modes.iter().any(|(_, resource)| {
                                resource == &mode_resource && resource.is_alive()
                            })
                    })
                });
                pending
                    .mode
                    .replace(OutputModeRequest {
                        width: mode.mode.width,
                        height: mode.mode.height,
                        refresh_millihertz: Some(mode.mode.refresh_millihertz.unsigned_abs()),
                    })
                    .is_some()
            }
            Request::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
                    resource.post_error(Error::InvalidCustomMode, "invalid mode size");
                    return;
                };
                if width == 0 || height == 0 || refresh < 0 {
                    resource.post_error(Error::InvalidCustomMode, "invalid mode size or refresh");
                    return;
                }
                pending
                    .mode
                    .replace(OutputModeRequest {
                        width,
                        height,
                        refresh_millihertz: (refresh > 0).then_some(refresh as u32),
                    })
                    .is_some()
            }
            Request::SetPosition { x, y } => pending.position.replace([x, y]).is_some(),
            Request::SetTransform { transform } => {
                let Ok(transform) = wl_output::Transform::try_from(u32::from(transform)) else {
                    resource.post_error(Error::InvalidTransform, "unknown transform");
                    return;
                };
                let transform = match transform {
                    wl_output::Transform::Normal => OutputTransform::Normal,
                    wl_output::Transform::_90 => OutputTransform::Rotate90,
                    wl_output::Transform::_180 => OutputTransform::Rotate180,
                    wl_output::Transform::_270 => OutputTransform::Rotate270,
                    wl_output::Transform::Flipped => OutputTransform::Flipped,
                    wl_output::Transform::Flipped90 => OutputTransform::Flipped90,
                    wl_output::Transform::Flipped180 => OutputTransform::Flipped180,
                    wl_output::Transform::Flipped270 => OutputTransform::Flipped270,
                    _ => {
                        resource.post_error(Error::InvalidTransform, "unknown transform");
                        return;
                    }
                };
                pending.transform.replace(transform).is_some()
            }
            Request::SetAdaptiveSync { state } => {
                let state = match u32::from(state) {
                    0 => false,
                    1 => true,
                    _ => {
                        resource.post_error(
                            Error::InvalidAdaptiveSyncState,
                            "unknown adaptive sync state",
                        );
                        return;
                    }
                };
                pending.adaptive_sync.replace(state).is_some()
            }
            Request::SetScale { scale } => {
                if !scale.is_finite() || scale <= 0.0 {
                    resource.post_error(Error::InvalidScale, "scale must be positive");
                    return;
                }
                pending.scale.replace(scale).is_some()
            }
            _ => unreachable!(),
        };
        if repeated {
            resource.post_error(Error::AlreadySet, "property set twice");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ClientState, dispatch_test_client, test_client_request as request};
    use smithay::output::{Mode, PhysicalProperties, Subpixel};
    use smithay::reexports::calloop::EventLoop;
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    type Message = (u32, u16, Vec<u32>);

    fn read(wire: &mut UnixStream) -> Vec<Message> {
        wire.set_nonblocking(true).unwrap();
        let mut bytes = Vec::new();
        let _ = wire.read_to_end(&mut bytes);
        wire.set_nonblocking(false).unwrap();
        let mut messages = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
            let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
            let end = offset + (header >> 16) as usize;
            messages.push((
                object,
                header as u16,
                bytes[offset + 8..end]
                    .chunks_exact(4)
                    .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
                    .collect(),
            ));
            offset = end;
        }
        messages
    }

    fn fixture() -> (EventLoop<'static, Ferese>, Ferese, Output, UnixStream) {
        let mut events = EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = Output::new(
            "TEST-1".into(),
            PhysicalProperties {
                serial_number: String::new(),
                size: (300, 200).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "test".into());
        refresh(&mut state);
        let (server, wire) = UnixStream::pair().unwrap();
        let client = state
            .display_handle
            .insert_client(server, Arc::new(ClientState::default()))
            .unwrap();
        let resource = client
            .create_resource::<ZwlrOutputManagerV1, _, Ferese>(&state.display_handle, VERSION, ())
            .unwrap();
        let mut manager = Manager {
            resource,
            heads: HashMap::new(),
        };
        publish_manager(
            &state.display_handle,
            &mut manager,
            &state.output_management.inventory,
        );
        manager.resource.done(state.output_management.serial);
        state.output_management.managers.push(manager);
        (events, state, output, wire)
    }

    #[test]
    fn serial_changes_cancel_pending_requests_and_mode_inventory_is_atomic() {
        if !crate::startup_tests::private_runtime(
            "output_management::tests::serial_changes_cancel_pending_requests_and_mode_inventory_is_atomic",
        ) {
            return;
        }
        let (mut events, mut state, output, mut wire) = fixture();
        let manager = state.output_management.managers[0]
            .resource
            .id()
            .protocol_id();
        let serial = state.output_management.serial;
        request(&mut wire, manager, 0, &[2, serial], None);
        dispatch_test_client(&mut events, &mut state);
        output.change_current_state(
            None,
            None,
            Some(smithay::output::Scale::Fractional(1.25)),
            None,
        );
        sync(&mut state);
        request(&mut wire, 2, 3, &[], None);
        dispatch_test_client(&mut events, &mut state);
        let messages = read(&mut wire);
        assert!(
            messages
                .iter()
                .any(|(object, opcode, _)| *object == 2 && *opcode == 2)
        );
        assert!(
            messages
                .iter()
                .any(|(object, opcode, args)| *object == manager
                    && *opcode == 1
                    && args == &[state.output_management.serial])
        );
        let serial = state.output_management.serial;
        sync(&mut state);
        assert_eq!(state.output_management.serial, serial);
    }

    #[test]
    fn valid_test_is_not_accepted_without_a_configurable_backend_and_does_not_mutate() {
        if !crate::startup_tests::private_runtime(
            "output_management::tests::valid_test_is_not_accepted_without_a_configurable_backend_and_does_not_mutate",
        ) {
            return;
        }
        let (mut events, mut state, output, mut wire) = fixture();
        let manager = &state.output_management.managers[0];
        let manager_id = manager.resource.id().protocol_id();
        let head = manager
            .heads
            .values()
            .next()
            .unwrap()
            .resource
            .id()
            .protocol_id();
        let serial = state.output_management.serial;
        request(&mut wire, manager_id, 0, &[2, serial], None);
        request(&mut wire, 2, 0, &[3, head], None);
        request(&mut wire, 3, 4, &[384], None); // 1.5 in Wayland fixed point.
        request(&mut wire, 2, 3, &[], None);
        dispatch_test_client(&mut events, &mut state);
        assert!(
            read(&mut wire)
                .iter()
                .any(|(object, opcode, _)| *object == 2 && *opcode == 1)
        );
        assert_eq!(output.current_scale().fractional_scale(), 1.0);
        assert_eq!(state.output_management.serial, serial);
    }

    #[test]
    fn repeated_disable_is_a_protocol_error() {
        if !crate::startup_tests::private_runtime(
            "output_management::tests::repeated_disable_is_a_protocol_error",
        ) {
            return;
        }
        let (mut events, mut state, _output, mut wire) = fixture();
        let manager = &state.output_management.managers[0];
        let manager_id = manager.resource.id().protocol_id();
        let head = manager
            .heads
            .values()
            .next()
            .unwrap()
            .resource
            .id()
            .protocol_id();
        request(
            &mut wire,
            manager_id,
            0,
            &[2, state.output_management.serial],
            None,
        );
        request(&mut wire, 2, 1, &[head], None);
        request(&mut wire, 2, 1, &[head], None);
        dispatch_test_client(&mut events, &mut state);
        assert!(
            read(&mut wire)
                .iter()
                .any(|(object, opcode, args)| *object == 1
                    && *opcode == 0
                    && args.get(1)
                        == Some(
                            &(zwlr_output_configuration_v1::Error::AlreadyConfiguredHead as u32)
                        ))
        );
    }

    #[test]
    fn omissions_are_protocol_errors_and_disabled_heads_are_not_omissions() {
        if !crate::startup_tests::private_runtime(
            "output_management::tests::omissions_are_protocol_errors_and_disabled_heads_are_not_omissions",
        ) {
            return;
        }
        let (mut events, mut state, _output, mut wire) = fixture();
        let manager = state.output_management.managers[0]
            .resource
            .id()
            .protocol_id();
        request(
            &mut wire,
            manager,
            0,
            &[2, state.output_management.serial],
            None,
        );
        request(&mut wire, 2, 3, &[], None);
        dispatch_test_client(&mut events, &mut state);
        assert!(
            read(&mut wire)
                .iter()
                .any(|(object, opcode, args)| *object == 1
                    && *opcode == 0
                    && args.get(1)
                        == Some(&(zwlr_output_configuration_v1::Error::UnconfiguredHead as u32)))
        );
    }

    #[test]
    fn destroy_configuration_retires_all_child_objects() {
        if !crate::startup_tests::private_runtime(
            "output_management::tests::destroy_configuration_retires_all_child_objects",
        ) {
            return;
        }
        let (mut events, mut state, _output, mut wire) = fixture();
        let manager = &state.output_management.managers[0];
        let manager_id = manager.resource.id().protocol_id();
        let head = manager
            .heads
            .values()
            .next()
            .unwrap()
            .resource
            .id()
            .protocol_id();
        request(
            &mut wire,
            manager_id,
            0,
            &[2, state.output_management.serial],
            None,
        );
        request(&mut wire, 2, 0, &[3, head], None);
        request(&mut wire, 2, 4, &[], None);
        dispatch_test_client(&mut events, &mut state);
        let messages = read(&mut wire);
        assert!(
            messages
                .iter()
                .any(|(object, opcode, args)| *object == 1 && *opcode == 1 && args == &[3])
        );
    }

    #[test]
    fn logical_geometry_rejects_extreme_scale_and_position_but_accepts_rotation() {
        let head = Head {
            info: ConnectedOutputInfo {
                connector: "TEST-1".into(),
                connected: true,
                identity: "test".into(),
                internal: false,
                requested_enabled: true,
                mirror_source: None,
                enabled: true,
                profile: None,
                requested_profile: None,
                auto_refresh: false,
                adaptive_sync_supported: false,
                tearing_supported: false,
                allow_tearing: false,
                adaptive_sync_requested: false,
                adaptive_sync_enabled: false,
                physical_size: None,
                current_mode: Some(ConnectedModeInfo {
                    width: 800,
                    height: 600,
                    refresh_millihertz: 60_000,
                    preferred: true,
                }),
                available_modes: Vec::new(),
                scale: 1.0,
                transform: OutputTransform::Normal,
                configured_position: Some([0, 0]),
            },
            description: "test".into(),
            name: "TEST-1".into(),
            position: [0, 0],
        };
        let mut pending = PendingHead {
            position: Some([i32::MAX - 100, 0]),
            ..Default::default()
        };
        assert!(settings(&head, Some(&pending)).is_err());
        pending.position = None;
        pending.scale = Some(f64::MIN_POSITIVE);
        assert!(settings(&head, Some(&pending)).is_err());
        pending.scale = Some(1e30);
        assert!(settings(&head, Some(&pending)).is_err());
        pending.scale = Some(1.25);
        pending.transform = Some(OutputTransform::Rotate90);
        assert_eq!(
            settings(&head, Some(&pending)).unwrap().transform,
            OutputTransform::Rotate90
        );
        assert!(!settings(&head, None).unwrap().enabled);
    }

    #[test]
    fn malformed_properties_and_duplicate_setters_disconnect_with_precise_errors() {
        if !crate::startup_tests::private_runtime(
            "output_management::tests::malformed_properties_and_duplicate_setters_disconnect_with_precise_errors",
        ) {
            return;
        }
        use zwlr_output_configuration_head_v1::Error;
        for (opcode, args, repeated, expected) in [
            (4, vec![0], false, Error::InvalidScale),
            (4, vec![256], true, Error::AlreadySet),
            (3, vec![8], false, Error::InvalidTransform),
            (1, vec![0, 600, 60_000], false, Error::InvalidCustomMode),
            (5, vec![2], false, Error::InvalidAdaptiveSyncState),
        ] {
            let (mut events, mut state, _output, mut wire) = fixture();
            let manager = &state.output_management.managers[0];
            let manager_id = manager.resource.id().protocol_id();
            let head = manager
                .heads
                .values()
                .next()
                .unwrap()
                .resource
                .id()
                .protocol_id();
            request(
                &mut wire,
                manager_id,
                0,
                &[2, state.output_management.serial],
                None,
            );
            request(&mut wire, 2, 0, &[3, head], None);
            request(&mut wire, 3, opcode, &args, None);
            if repeated {
                request(&mut wire, 3, opcode, &args, None);
            }
            dispatch_test_client(&mut events, &mut state);
            assert!(
                read(&mut wire)
                    .iter()
                    .any(|(object, opcode, args)| *object == 1
                        && *opcode == 0
                        && args.get(1) == Some(&(expected as u32))),
                "expected {expected:?}"
            );
        }
    }

    #[test]
    fn requests_on_a_submitted_child_report_already_used_on_the_parent() {
        if !crate::startup_tests::private_runtime(
            "output_management::tests::requests_on_a_submitted_child_report_already_used_on_the_parent",
        ) {
            return;
        }
        let (mut events, mut state, _output, mut wire) = fixture();
        let manager = &state.output_management.managers[0];
        let manager_id = manager.resource.id().protocol_id();
        let head = manager
            .heads
            .values()
            .next()
            .unwrap()
            .resource
            .id()
            .protocol_id();
        request(
            &mut wire,
            manager_id,
            0,
            &[2, state.output_management.serial],
            None,
        );
        request(&mut wire, 2, 0, &[3, head], None);
        request(&mut wire, 2, 3, &[], None);
        request(&mut wire, 3, 4, &[256], None);
        dispatch_test_client(&mut events, &mut state);
        assert!(
            read(&mut wire)
                .iter()
                .any(|(object, opcode, args)| *object == 1
                    && *opcode == 0
                    && args.first() == Some(&2)
                    && args.get(1)
                        == Some(&(zwlr_output_configuration_v1::Error::AlreadyUsed as u32)))
        );
    }

    #[test]
    fn a_retired_head_cannot_be_reused_with_a_new_serial() {
        if !crate::startup_tests::private_runtime(
            "output_management::tests::a_retired_head_cannot_be_reused_with_a_new_serial",
        ) {
            return;
        }
        let (mut events, mut state, old_output, mut wire) = fixture();
        let manager = &state.output_management.managers[0];
        let manager_id = manager.resource.id().protocol_id();
        let old_head = manager
            .heads
            .values()
            .next()
            .unwrap()
            .resource
            .id()
            .protocol_id();
        state.unregister_output(&old_output);
        let replacement = Output::new(
            "TEST-1".into(),
            PhysicalProperties {
                serial_number: String::new(),
                size: (300, 200).into(),
                subpixel: Subpixel::Unknown,
                make: "replacement".into(),
                model: "test".into(),
            },
        );
        replacement.change_current_state(
            Some(Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        state.space.map_output(&replacement, (0, 0));
        state.register_output(&replacement, "test".into());
        sync(&mut state);
        request(
            &mut wire,
            manager_id,
            0,
            &[2, state.output_management.serial],
            None,
        );
        request(&mut wire, 2, 0, &[3, old_head], None);
        request(&mut wire, 2, 3, &[], None);
        dispatch_test_client(&mut events, &mut state);
        assert!(
            read(&mut wire)
                .iter()
                .any(|(object, opcode, _)| *object == 2 && *opcode == 2)
        );
    }
}
