use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferese_protocols::effects::v1::server::ferese_effects_manager_v1::FereseEffectsManagerV1;
use ferese_protocols::effects::v1::server::ferese_surface_effects_v1::FereseSurfaceEffectsV1;
use ferese_protocols::effects::v1::server::{ferese_effects_manager_v1, ferese_surface_effects_v1};
use ferese_protocols::material::v1::server::ferese_material_manager_v1::FereseMaterialManagerV1;
use ferese_protocols::material::v1::server::ferese_surface_material_v1::FereseSurfaceMaterialV1;
use ferese_protocols::material::v1::server::{ferese_material_manager_v1, ferese_surface_material_v1};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum, Weak,
};
use smithay::wayland::compositor::with_states;

use crate::Ferese;
use crate::config::MaterialStyle;
use crate::private_client::ClientCapabilities;
use crate::state::ClientState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SemanticRole {
    Panel,
    PanelElevated,
    Popover,
    Menu,
    Notification,
    Hud,
    Modal,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResolvedMaterial {
    pub style: MaterialStyle,
    pub opacity: f32,
    pub shadow: [f64; 3],
}

pub(crate) fn resolve_material(role: SemanticRole, style: MaterialStyle, opacity: f32) -> ResolvedMaterial {
    let shadow = match role {
        SemanticRole::Panel => [1.0, 5.0, 0.04],
        SemanticRole::PanelElevated | SemanticRole::Popover | SemanticRole::Menu => [2.0, 8.0, 0.07],
        SemanticRole::Hud => [1.0, 4.0, 0.04],
        SemanticRole::Notification | SemanticRole::Modal => [3.0, 10.0, 0.09],
    };
    let opacity = match style {
        MaterialStyle::Solid => 1.0,
        MaterialStyle::Translucent => opacity.clamp(0.0, 1.0),
    };
    ResolvedMaterial { style, opacity, shadow }
}

#[derive(Debug)]
struct PendingPresentation {
    role: SemanticRole,
    regions: Vec<[f64; 5]>,
    opacity: u32,
    region_opacities: Vec<u32>,
}

#[derive(Debug)]
struct SurfaceEffectsState {
    attached: AtomicBool,
    generation: AtomicU64,
    role: Mutex<Option<SemanticRole>>,
    regions: Mutex<Option<Vec<[f64; 5]>>>,
    opacity: Mutex<u32>,
    pending: Mutex<Option<PendingPresentation>>,
    region_opacities: Mutex<Vec<u32>>,
    presentation_supported: AtomicBool,
    dismissing: AtomicBool,
}

impl SurfaceEffectsState {
    fn new() -> Self {
        Self {
            attached: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            role: Mutex::new(None),
            regions: Mutex::new(None),
            opacity: Mutex::new(1000),
            pending: Mutex::new(None),
            region_opacities: Mutex::new(Vec::new()),
            presentation_supported: AtomicBool::new(false),
            dismissing: AtomicBool::new(false),
        }
    }

    fn commit_presentation(&self) {
        let Some(pending) = self.pending.lock().unwrap().take() else {
            return;
        };
        *self.role.lock().unwrap() = Some(pending.role);
        *self.regions.lock().unwrap() = Some(pending.regions);
        *self.region_opacities.lock().unwrap() = pending.region_opacities;
        if !self.dismissing.load(Ordering::Acquire) {
            *self.opacity.lock().unwrap() = pending.opacity;
        }
        self.generation.fetch_add(1, Ordering::Release);
    }

    fn set_role(&self, role: Option<SemanticRole>) -> bool {
        let mut current = self.role.lock().unwrap();
        if *current != role {
            *current = role;
            self.generation.fetch_add(1, Ordering::Release);
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
pub(crate) struct SurfaceEffectsUserData(Mutex<Weak<WlSurface>>);

impl SurfaceEffectsUserData {
    fn new(surface: WlSurface) -> Self {
        Self(Mutex::new(surface.downgrade()))
    }

    fn surface(&self) -> Option<WlSurface> {
        self.0.lock().unwrap().upgrade().ok()
    }
}

pub(crate) fn init_global(display: &DisplayHandle) {
    display.create_global::<Ferese, FereseEffectsManagerV1, _>(5, ());
    display.create_global::<Ferese, FereseMaterialManagerV1, _>(1, ());
}

impl GlobalDispatch<FereseMaterialManagerV1, ()> for Ferese {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<FereseMaterialManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<FereseMaterialManagerV1, ()> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &FereseMaterialManagerV1,
        request: ferese_material_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ferese_material_manager_v1::Request::GetSurfaceMaterial { id, surface } => {
                let attached = with_states(&surface, |states| {
                    states.data_map.insert_if_missing_threadsafe(SurfaceEffectsState::new);
                    let effects = states.data_map.get::<SurfaceEffectsState>().unwrap();
                    effects.attached.swap(true, Ordering::AcqRel)
                });
                if attached {
                    manager.post_error(
                        ferese_material_manager_v1::Error::AlreadyConstructed,
                        "surface already has material or effects",
                    );
                    return;
                }
                let material = data_init.init(id, SurfaceEffectsUserData::new(surface.clone()));
                set_surface_role(&surface, Some(SemanticRole::Modal));
                material.ready();
                crate::backends::direct::render_surface(state, &surface);
            }
            ferese_material_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<FereseSurfaceMaterialV1, SurfaceEffectsUserData> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        _material: &FereseSurfaceMaterialV1,
        request: ferese_surface_material_v1::Request,
        data: &SurfaceEffectsUserData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, ferese_surface_material_v1::Request::Destroy)
            && let Some(surface) = data.surface()
            && detach(&surface)
        {
            crate::backends::direct::render_surface(state, &surface);
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _material: &FereseSurfaceMaterialV1,
        data: &SurfaceEffectsUserData,
    ) {
        if let Some(surface) = data.surface() {
            detach(&surface);
        }
    }
}

pub(crate) fn commit_presentation(surface: &WlSurface) {
    with_states(surface, |states| {
        if let Some(effects) = states.data_map.get::<SurfaceEffectsState>() {
            effects.commit_presentation();
        }
    });
}

pub(crate) fn surface_regions(surface: &WlSurface) -> Option<Vec<[f64; 5]>> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<SurfaceEffectsState>()?
            .regions
            .lock()
            .unwrap()
            .clone()
    })
}

pub(crate) fn surface_region_opacities(surface: &WlSurface) -> Vec<u32> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<SurfaceEffectsState>()
            .map(|effects| effects.region_opacities.lock().unwrap().clone())
            .unwrap_or_default()
    })
}

fn decode_region_opacities(bytes: &[u8]) -> Option<Vec<u32>> {
    if !bytes.len().is_multiple_of(4) || bytes.len() > 32 * 4 {
        return None;
    }
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| {
            let value = u32::from_ne_bytes(*v);
            (value <= 1000).then_some(value)
        })
        .collect()
}

fn decode_regions(bytes: &[u8]) -> Option<Vec<[f64; 5]>> {
    if !bytes.len().is_multiple_of(20) || bytes.len() > 32 * 20 {
        return None;
    }
    bytes
        .as_chunks::<20>()
        .0
        .iter()
        .map(|tuple| {
            let values: [f64; 5] = std::array::from_fn(|i| f64::from(f32::from_ne_bytes(tuple.as_chunks::<4>().0[i])));
            (values[2] > 0.0
                && values[3] > 0.0
                && values[4] >= 0.0
                && values.iter().all(|v| v.is_finite() && v.abs() <= 32768.0))
            .then_some(values)
        })
        .collect()
}

// Consumed by the material renderer in the next M7 slice.
#[allow(dead_code)]
pub(crate) fn surface_role(surface: &WlSurface) -> Option<(SemanticRole, u64)> {
    with_states(surface, |states| {
        let effects = states.data_map.get::<SurfaceEffectsState>()?;
        let role = *effects.role.lock().unwrap();
        role.map(|role| (role, effects.generation.load(Ordering::Acquire)))
    })
}

impl GlobalDispatch<FereseEffectsManagerV1, ()> for Ferese {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<FereseEffectsManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        client
            .get_data::<ClientState>()
            .is_some_and(|state| state.capabilities.contains(ClientCapabilities::EFFECTS))
    }
}

impl Dispatch<FereseEffectsManagerV1, ()> for Ferese {
    fn request(
        _state: &mut Self,
        _client: &Client,
        manager: &FereseEffectsManagerV1,
        request: ferese_effects_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ferese_effects_manager_v1::Request::GetSurfaceEffects { id, surface } => {
                let already_attached = with_states(&surface, |states| {
                    states.data_map.insert_if_missing_threadsafe(SurfaceEffectsState::new);
                    states
                        .data_map
                        .get::<SurfaceEffectsState>()
                        .unwrap()
                        .attached
                        .swap(true, Ordering::AcqRel)
                });

                if already_attached {
                    manager.post_error(
                        ferese_effects_manager_v1::Error::AlreadyConstructed,
                        "wl_surface already has a Ferese effects object",
                    );
                } else {
                    let effects = data_init.init(id, SurfaceEffectsUserData::new(surface.clone()));
                    with_states(&surface, |states| {
                        states
                            .data_map
                            .get::<SurfaceEffectsState>()
                            .unwrap()
                            .presentation_supported
                            .store(effects.version() >= 3, Ordering::Release)
                    });
                }
            }
            ferese_effects_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<FereseSurfaceEffectsV1, SurfaceEffectsUserData> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &FereseSurfaceEffectsV1,
        request: ferese_surface_effects_v1::Request,
        data: &SurfaceEffectsUserData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Some(surface) = data.surface() else {
            return;
        };

        match request {
            ferese_surface_effects_v1::Request::SetPresentation {
                role,
                regions,
                opacity,
                region_opacities,
            } => {
                let (Some(role), Some(regions), Some(region_opacities)) = (
                    decode_role(role),
                    decode_regions(&regions),
                    decode_region_opacities(&region_opacities),
                ) else {
                    return;
                };
                with_states(&surface, |states| {
                    if let Some(effects) = states.data_map.get::<SurfaceEffectsState>() {
                        *effects.pending.lock().unwrap() = Some(PendingPresentation {
                            role,
                            regions,
                            opacity: opacity.min(1000),
                            region_opacities,
                        });
                    }
                });
            }
            ferese_surface_effects_v1::Request::SetOpacity { opacity } => {
                let changed = with_states(&surface, |states| {
                    let Some(effects) = states.data_map.get::<SurfaceEffectsState>() else {
                        return false;
                    };
                    if effects.dismissing.load(Ordering::Acquire) {
                        return false;
                    }
                    let mut current = effects.opacity.lock().unwrap();
                    let opacity = opacity.min(1000);
                    if *current == opacity {
                        return false;
                    }
                    *current = opacity;
                    effects.generation.fetch_add(1, Ordering::Release);
                    true
                });
                if changed {
                    crate::backends::direct::render_surface(state, &surface);
                }
            }
            ferese_surface_effects_v1::Request::SetRole { role } => {
                let Some(role) = decode_role(role) else {
                    return;
                };
                if set_surface_role(&surface, Some(role)) {
                    crate::backends::direct::render_surface(state, &surface);
                }
            }
            ferese_surface_effects_v1::Request::SetRegionOpacities { opacities } => {
                let Some(opacities) = decode_region_opacities(&opacities) else {
                    return;
                };
                let changed = with_states(&surface, |states| {
                    let Some(effects) = states.data_map.get::<SurfaceEffectsState>() else {
                        return false;
                    };
                    let mut old = effects.region_opacities.lock().unwrap();
                    if *old == opacities {
                        return false;
                    }
                    *old = opacities;
                    effects.generation.fetch_add(1, Ordering::Release);
                    true
                });
                if changed {
                    crate::backends::direct::render_surface(state, &surface);
                }
            }
            ferese_surface_effects_v1::Request::SetRegions { regions } => {
                let Some(regions) = decode_regions(&regions) else {
                    return;
                };
                let changed = with_states(&surface, |states| {
                    let Some(effects) = states.data_map.get::<SurfaceEffectsState>() else {
                        return false;
                    };
                    let mut old = effects.regions.lock().unwrap();
                    if old.as_ref() == Some(&regions) {
                        return false;
                    }
                    *old = Some(regions);
                    effects.generation.fetch_add(1, Ordering::Release);
                    true
                });
                if changed {
                    crate::backends::direct::render_surface(state, &surface);
                }
            }
            ferese_surface_effects_v1::Request::ClearRole => {
                if set_surface_role(&surface, None) {
                    crate::backends::direct::render_surface(state, &surface);
                }
            }
            ferese_surface_effects_v1::Request::Destroy => {
                if detach(&surface) {
                    crate::backends::direct::render_surface(state, &surface);
                }
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _resource: &FereseSurfaceEffectsV1,
        data: &SurfaceEffectsUserData,
    ) {
        if let Some(surface) = data.surface() {
            detach(&surface);
        }
    }
}

fn decode_role(role: WEnum<ferese_surface_effects_v1::Role>) -> Option<SemanticRole> {
    Some(match role {
        WEnum::Value(ferese_surface_effects_v1::Role::Panel) => SemanticRole::Panel,
        WEnum::Value(ferese_surface_effects_v1::Role::PanelElevated) => SemanticRole::PanelElevated,
        WEnum::Value(ferese_surface_effects_v1::Role::Popover) => SemanticRole::Popover,
        WEnum::Value(ferese_surface_effects_v1::Role::Menu) => SemanticRole::Menu,
        WEnum::Value(ferese_surface_effects_v1::Role::Notification) => SemanticRole::Notification,
        WEnum::Value(ferese_surface_effects_v1::Role::Hud) => SemanticRole::Hud,
        WEnum::Value(ferese_surface_effects_v1::Role::Modal) => SemanticRole::Modal,
        WEnum::Unknown(_) | WEnum::Value(_) => return None,
    })
}

fn set_surface_role(surface: &WlSurface, role: Option<SemanticRole>) -> bool {
    with_states(surface, |states| {
        states
            .data_map
            .get::<SurfaceEffectsState>()
            .is_some_and(|effects| effects.set_role(role))
    })
}

fn detach(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        let Some(effects) = states.data_map.get::<SurfaceEffectsState>() else {
            return false;
        };
        let role_changed = effects.set_role(None);
        effects.region_opacities.lock().unwrap().clear();
        effects.pending.lock().unwrap().take();
        effects.dismissing.store(false, Ordering::Release);
        let mut opacity = effects.opacity.lock().unwrap();
        let opacity_changed = *opacity != 1000;
        *opacity = 1000;
        let was_attached = effects.attached.swap(false, Ordering::AcqRel);
        role_changed || was_attached || opacity_changed
    })
}

pub(crate) fn surface_opacity(surface: &WlSurface) -> f32 {
    with_states(surface, |states| {
        states
            .data_map
            .get::<SurfaceEffectsState>()
            .map_or(1.0, |effects| *effects.opacity.lock().unwrap() as f32 / 1000.0)
    })
}

pub(crate) fn begin_surface_dismiss(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states.data_map.get::<SurfaceEffectsState>().is_some_and(|effects| {
            effects.presentation_supported.load(Ordering::Acquire)
                && effects.attached.load(Ordering::Acquire)
                && !effects.dismissing.swap(true, Ordering::AcqRel)
        })
    })
}

pub(crate) fn fade_dismissed_surface(surface: &WlSurface, opacity: f64) {
    with_states(surface, |states| {
        if let Some(effects) = states.data_map.get::<SurfaceEffectsState>() {
            *effects.opacity.lock().unwrap() = (opacity.clamp(0.0, 1.0) * 1000.0).round() as u32;
            effects.generation.fetch_add(1, Ordering::Release);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_waits_for_buffer_commit_and_keeps_dismissal_ownership() {
        let state = SurfaceEffectsState::new();
        let queue = |opacity| PendingPresentation {
            role: SemanticRole::Popover,
            regions: vec![[1.25, 2.5, 100.0, 80.0, 12.0]],
            opacity,
            region_opacities: vec![500],
        };
        *state.pending.lock().unwrap() = Some(queue(900));
        assert_eq!(*state.opacity.lock().unwrap(), 1000);
        assert!(state.regions.lock().unwrap().is_none());
        assert_eq!(state.generation.load(Ordering::Acquire), 0);
        // Several requests before one buffer commit present only the last.
        *state.pending.lock().unwrap() = Some(queue(800));
        state.commit_presentation();
        assert_eq!(*state.opacity.lock().unwrap(), 800);
        assert_eq!(state.regions.lock().unwrap().as_ref().unwrap()[0][0], 1.25);
        assert_eq!(*state.region_opacities.lock().unwrap(), [500]);
        assert_eq!(state.generation.load(Ordering::Acquire), 1);
        state.commit_presentation();
        assert_eq!(state.generation.load(Ordering::Acquire), 1);
        state.dismissing.store(true, Ordering::Release);
        *state.pending.lock().unwrap() = Some(queue(1000));
        state.commit_presentation();
        assert_eq!(
            *state.opacity.lock().unwrap(),
            800,
            "client cannot restart compositor fade"
        );
    }

    #[test]
    fn region_opacities_validate_independent_fades() {
        let bytes: Vec<_> = [0u32, 500, 1000].into_iter().flat_map(u32::to_ne_bytes).collect();
        assert_eq!(decode_region_opacities(&bytes), Some(vec![0, 500, 1000]));
        assert_eq!(decode_region_opacities(&[]), Some(vec![]));
        assert!(decode_region_opacities(&bytes[..11]).is_none());
        assert!(decode_region_opacities(&1001u32.to_ne_bytes()).is_none());
        assert!(decode_region_opacities(&[0; 33 * 4]).is_none());
    }

    #[test]
    fn fractional_regions_validate_all_wire_values() {
        let region = [0.25f32, -0.75, 100.5, 40.25, 13.5];
        let encode = |r: [f32; 5]| r.into_iter().flat_map(f32::to_ne_bytes).collect::<Vec<_>>();
        let bytes = encode(region);
        assert_eq!(decode_regions(&bytes), Some(vec![region.map(f64::from)]));
        assert_eq!(decode_regions(&[]), Some(vec![]));
        assert!(decode_regions(&bytes[..19]).is_none());
        assert!(decode_regions(&bytes.repeat(33)).is_none());
        for index in 0..5 {
            for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 32769.0] {
                let mut bad = region;
                bad[index] = invalid;
                assert!(decode_regions(&encode(bad)).is_none());
            }
        }
        for (index, value) in [(2, 0.0), (3, -1.0), (4, -0.25)] {
            let mut bad = region;
            bad[index] = value;
            assert!(decode_regions(&encode(bad)).is_none());
        }
    }

    #[test]
    fn role_changes_advance_generation_only_when_material_changes() {
        let effects = SurfaceEffectsState::new();
        assert!(effects.set_role(Some(SemanticRole::Panel)));
        assert_eq!(effects.generation.load(Ordering::Acquire), 1);

        assert!(!effects.set_role(Some(SemanticRole::Panel)));
        assert_eq!(effects.generation.load(Ordering::Acquire), 1);

        assert!(effects.set_role(Some(SemanticRole::Popover)));
        assert_eq!(effects.generation.load(Ordering::Acquire), 2);
    }

    #[test]
    fn solid_materials_are_opaque_and_keep_role_specific_elevation() {
        for role in [
            SemanticRole::Panel,
            SemanticRole::Popover,
            SemanticRole::Menu,
            SemanticRole::Hud,
            SemanticRole::Notification,
            SemanticRole::Modal,
        ] {
            let solid = resolve_material(role, MaterialStyle::Solid, 0.6);
            let translucent = resolve_material(role, MaterialStyle::Translucent, 0.6);
            assert_eq!(solid.opacity, 1.0);
            assert_eq!(translucent.opacity, 0.6);
            assert_eq!(solid.shadow, translucent.shadow);
        }
    }
}
