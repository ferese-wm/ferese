//! Validate a device-wide desired topology, then use Smithay for every real commit.
//! Smithay exposes TEST_ONLY per surface, but not a combined CRTC request. The
//! one raw request here is validation only; mode/plane ownership stays in Smithay.
use super::{
    ControlDevice, DesiredOutputConfiguration, DirectDevice, DirectOutput, DrmDevice, DrmDeviceFd,
    DrmMode, DrmNode, Error, Ferese, Fourcc, GbmAllocator, GbmBufferFlags, GbmFramebufferExporter,
    GlesRenderer, HashMap, HashSet, OutputCompositor, OutputMode, OutputModeSource, OutputScan,
    OutputSelection, OutputSettings, PrimaryPlaneElement, cancel_output_timers,
    connected_mode_info, connector, create_direct_output, create_output, crtc, drm_device_key,
    internal_connector, io, output_transform, property, remember_applied_configuration,
    rescan_device, validate_desired_outputs,
};
use smithay::backend::allocator::Allocator;
use smithay::backend::drm::exporter::{ExportBuffer, ExportFramebuffer};
use smithay::reexports::drm::control::atomic::AtomicModeReq;
use smithay::reexports::drm::control::{AtomicCommitFlags, ResourceHandle};

fn property<D: ControlDevice, H: ResourceHandle>(
    drm: &D,
    handle: H,
    name: &str,
) -> io::Result<property::Handle> {
    for (property, _) in drm.get_properties(handle)?.iter() {
        if drm.get_property(*property)?.name().to_bytes() == name.as_bytes() {
            return Ok(*property);
        }
    }
    Err(io::Error::other(format!("missing DRM property {name}")))
}

fn add<H: ResourceHandle>(
    drm: &DrmDevice,
    request: &mut AtomicModeReq,
    handle: H,
    name: &str,
    value: u64,
) -> io::Result<()> {
    request.add_raw_property(handle.into(), property(drm, handle, name)?, value);
    Ok(())
}

fn clear_secondary_planes(
    drm: &DrmDevice,
    request: &mut AtomicModeReq,
    crtc: crtc::Handle,
) -> Result<(), Box<dyn Error>> {
    let planes = drm.planes(&crtc)?;
    for plane in planes.cursor.iter().chain(&planes.overlay) {
        let properties = drm.get_properties(plane.handle)?;
        let owned = properties.iter().any(|(handle, value)| {
            drm.get_property(*handle).is_ok_and(|info| {
                info.name().to_bytes() == b"CRTC_ID" && *value == u32::from(crtc) as u64
            })
        });
        if owned {
            add(drm, request, plane.handle, "FB_ID", 0)?;
            add(drm, request, plane.handle, "CRTC_ID", 0)?;
        }
    }
    Ok(())
}

struct Blobs {
    fd: DrmDeviceFd,
    ids: Vec<u64>,
}

impl Drop for Blobs {
    fn drop(&mut self) {
        for id in &self.ids {
            let _ = self.fd.destroy_property_blob(*id);
        }
    }
}

pub(super) fn validate_device(
    device: &DirectDevice,
    scan: &OutputScan,
) -> Result<(), Box<dyn Error>> {
    let color = super::sdr_color::Plan::capture(
        &device.drm,
        scan.selections
            .iter()
            .map(|selection| selection.connector.handle()),
    )?;
    if scan.selections.iter().all(|selection| {
        device.outputs.get(&selection.crtc).is_some_and(|output| {
            output.connector == selection.connector.handle()
                && output.mode == selection.mode
                && output.settings.adaptive_sync == selection.settings.adaptive_sync
        })
    }) && scan.selections.len() == device.outputs.len()
        && color.is_empty()
    {
        return Ok(());
    }
    // Legacy KMS has no atomic test. Smithay validates its surface/mode as far as
    // the API permits; commit failures still go through the restoration path.
    if !device.drm.is_atomic() {
        return Ok(());
    }
    let drm = &device.drm;
    let mut request = AtomicModeReq::new();
    color.extend_test(&mut request);
    let mut blobs = Blobs {
        fd: drm.device_fd().clone(),
        ids: Vec::new(),
    };
    let mut buffers = Vec::new();
    let mut framebuffers = Vec::new();
    let mut allocator = GbmAllocator::new(
        device.gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(device.gbm.clone(), Some(device.render_node).into());
    let mut used_planes = HashSet::new();
    let desired_crtcs = scan
        .selections
        .iter()
        .map(|selection| selection.crtc)
        .collect::<HashSet<_>>();
    let desired_connectors = scan
        .selections
        .iter()
        .map(|selection| selection.connector.handle())
        .collect::<HashSet<_>>();
    for output in device.outputs.values() {
        if !desired_connectors.contains(&output.connector) {
            add(drm, &mut request, output.connector, "CRTC_ID", 0)?;
        }
        if !desired_crtcs.contains(&output.surface.crtc()) {
            clear_secondary_planes(drm, &mut request, output.surface.crtc())?;
            add(drm, &mut request, output.surface.crtc(), "ACTIVE", 0)?;
            add(drm, &mut request, output.surface.crtc(), "MODE_ID", 0)?;
            add(drm, &mut request, output.surface.plane(), "FB_ID", 0)?;
            add(drm, &mut request, output.surface.plane(), "CRTC_ID", 0)?;
        }
    }
    for selection in &scan.selections {
        clear_secondary_planes(drm, &mut request, selection.crtc)?;
        let planes = drm.planes(&selection.crtc)?;
        let primary = device
            .outputs
            .get(&selection.crtc)
            .map(|output| output.surface.plane())
            .or_else(|| {
                planes
                    .primary
                    .iter()
                    .find(|plane| !used_planes.contains(&plane.handle))
                    .map(|plane| plane.handle)
            })
            .ok_or("no usable primary plane")?;
        used_planes.insert(primary);
        if let Ok(rotation) = property(drm, primary, "rotation") {
            request.add_raw_property(primary.into(), rotation, 1);
        }
        let blob: u64 = drm.create_property_blob(&selection.mode)?.into();
        blobs.ids.push(blob);
        add(drm, &mut request, selection.crtc, "MODE_ID", blob)?;
        add(drm, &mut request, selection.crtc, "ACTIVE", 1)?;
        if let Ok(vrr) = property(drm, selection.crtc, "VRR_ENABLED") {
            request.add_raw_property(
                selection.crtc.into(),
                vrr,
                u64::from(selection.settings.adaptive_sync),
            );
        } else if selection.settings.adaptive_sync {
            return Err("adaptive sync is unavailable on the selected CRTC".into());
        }
        add(
            drm,
            &mut request,
            selection.connector.handle(),
            "CRTC_ID",
            u32::from(selection.crtc) as u64,
        )?;
        let (width, height) = selection.mode.size();
        let formats = &planes
            .primary
            .iter()
            .find(|plane| plane.handle == primary)
            .ok_or("primary plane disappeared")?
            .formats;
        let mut allocated = None;
        for format in formats.iter().filter(|format| {
            matches!(
                format.code,
                Fourcc::Argb8888 | Fourcc::Abgr8888 | Fourcc::Xrgb8888 | Fourcc::Xbgr8888
            )
        }) {
            if let Ok(buffer) = allocator.create_buffer(
                width.into(),
                height.into(),
                format.code,
                &[format.modifier],
            ) && let Ok(Some(fb)) =
                exporter.add_framebuffer(drm.device_fd(), ExportBuffer::Allocator(&buffer), false)
            {
                allocated = Some((buffer, fb));
                break;
            }
        }
        let (buffer, fb) = allocated.ok_or("unable to allocate a DRM validation buffer")?;
        add(
            drm,
            &mut request,
            primary,
            "FB_ID",
            u32::from(*fb.as_ref()) as u64,
        )?;
        add(
            drm,
            &mut request,
            primary,
            "CRTC_ID",
            u32::from(selection.crtc) as u64,
        )?;
        for (name, value) in [
            ("SRC_X", 0),
            ("SRC_Y", 0),
            ("SRC_W", (width as u64) << 16),
            ("SRC_H", (height as u64) << 16),
            ("CRTC_X", 0),
            ("CRTC_Y", 0),
            ("CRTC_W", width as u64),
            ("CRTC_H", height as u64),
        ] {
            add(drm, &mut request, primary, name, value)?;
        }
        buffers.push(buffer);
        framebuffers.push(fb);
    }
    drm.atomic_commit(
        AtomicCommitFlags::TEST_ONLY | AtomicCommitFlags::ALLOW_MODESET,
        request,
    )?;
    Ok(())
}

fn prepare(
    surface: &mut OutputCompositor,
    renderer: &mut GlesRenderer,
    mode: DrmMode,
    settings: &OutputSettings,
) -> Result<(), Box<dyn Error>> {
    surface.use_mode(mode)?;
    surface.use_vrr(settings.adaptive_sync)?;
    surface.set_output_mode_source(OutputModeSource::Static {
        size: OutputMode::from(mode).size,
        scale: settings.scale.into(),
        transform: output_transform(settings.transform),
    });
    surface.reset_buffer_ages();
    let elements: Vec<crate::render::AnimatedWindowRenderElement> = Vec::new();
    let result = surface.render_frame(
        renderer,
        &elements,
        [0.0, 0.0, 0.0, 1.0],
        smithay::backend::drm::compositor::FrameFlags::empty(),
    )?;
    if let PrimaryPlaneElement::Swapchain(primary) = &result.primary_element {
        primary.sync.wait()?;
    }
    Ok(())
}

/// Retire a logical output only after its hardware is unavailable or confirmed disabled.
fn retire(state: &mut Ferese, output: &mut DirectOutput, node: DrmNode, crtc: crtc::Handle) {
    output.gamma = Default::default();
    cancel_output_timers(&state.loop_handle, output);
    if let Some(global) = output.global.take() {
        state.display_handle.disable_global::<Ferese>(global);
    }
    state.session_lock.output_removed(&output.output);
    state.unregister_output(&output.output);
    state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend")
        .presentation
        .remove(&(node, crtc));
}

struct DeviceTransaction<'a> {
    state: &'a mut Ferese,
    device: &'a mut DirectDevice,
    selections: &'a HashMap<crtc::Handle, OutputSelection>,
    staged: HashMap<crtc::Handle, DirectOutput>,
    replaced: HashSet<crtc::Handle>,
    color_plan: Option<super::sdr_color::Plan>,
    color_applied: Option<super::sdr_color::Applied>,
}

impl crate::output_transaction::HardwareTransaction for DeviceTransaction<'_> {
    type Key = crtc::Handle;

    fn prepare_commit(&mut self) -> Result<(), String> {
        if let Some(plan) = self.color_plan.take() {
            self.color_applied = Some(
                plan.apply(&self.device.drm)
                    .map_err(|error| format!("could not apply SDR output state: {error}"))?,
            );
        }
        Ok(())
    }

    fn stage(&mut self, crtc: crtc::Handle) -> Result<(), String> {
        let selection = &self.selections[&crtc];
        if self
            .device
            .outputs
            .get(&crtc)
            .is_none_or(|output| output.connector != selection.connector.handle())
        {
            let output = create_direct_output(
                self.state,
                &mut self.device.drm,
                &self.device.gbm,
                &self.device.renderer,
                selection.clone(),
                false,
            )
            .map_err(|error| error.to_string())?;
            self.staged.insert(crtc, output);
        }
        let output = self
            .staged
            .get_mut(&crtc)
            .or_else(|| self.device.outputs.get_mut(&crtc))
            .expect("the selected CRTC has a live or staged output in this transaction");
        output
            .surface
            .set_connectors(&[selection.connector.handle()])
            .map_err(|error| error.to_string())?;
        output.dmabuf_feedback.invalidate();
        prepare(
            &mut output.surface,
            &mut self.device.renderer,
            selection.mode,
            &selection.settings,
        )
        .map_err(|error| error.to_string())
    }

    fn commit(&mut self, crtc: crtc::Handle) -> Result<(), String> {
        if self.staged.contains_key(&crtc) && self.device.outputs.contains_key(&crtc) {
            // Connector and mode cannot be changed together through use_mode /
            // set_connectors: each tests against the other's old value. Stage a
            // replacement surface, retire the old one *before* its first commit
            // (Smithay clears the CRTC on surface drop), then publish metadata.
            let replacement = self
                .staged
                .remove(&crtc)
                .expect("the selected CRTC has a live or staged output in this transaction");
            let previous = self
                .device
                .outputs
                .get_mut(&crtc)
                .expect("the selected CRTC has a live or staged output in this transaction");
            previous.surface = replacement.surface;
            previous.dmabuf_feedback = replacement.dmabuf_feedback;
            self.replaced.insert(crtc);
        }
        self.staged
            .get_mut(&crtc)
            .or_else(|| self.device.outputs.get_mut(&crtc))
            .expect("the selected CRTC has a live or staged output in this transaction")
            .surface
            .commit_frame()
            .map_err(|error| error.to_string())
    }

    fn disable(&mut self, crtc: crtc::Handle) -> Result<(), String> {
        self.device
            .outputs
            .get_mut(&crtc)
            .expect("the selected CRTC has a live or staged output in this transaction")
            .surface
            .clear()
            .map_err(|error| error.to_string())
    }

    fn abort_staged(&mut self) {
        for output in self.staged.values_mut() {
            let _ = output.surface.clear();
        }
        self.staged.clear();
    }

    fn restore(&mut self, crtc: crtc::Handle) -> Result<(), String> {
        if self.replaced.contains(&crtc) {
            let output = &self.device.outputs[&crtc];
            let connector = self
                .device
                .drm
                .get_connector(output.connector, true)
                .map_err(|error| error.to_string())?;
            if connector.state() != connector::State::Connected {
                return Err("previous connector is no longer available".into());
            }
            let selection = OutputSelection {
                name: output.output.name(),
                connector,
                crtc,
                mode: output.mode,
                settings: output.settings.clone(),
                identity: output.identity.clone(),
                mirror_source: output.mirror_source.clone(),
            };
            let mut replacement = create_direct_output(
                self.state,
                &mut self.device.drm,
                &self.device.gbm,
                &self.device.renderer,
                selection,
                false,
            )
            .map_err(|error| error.to_string())?;
            prepare(
                &mut replacement.surface,
                &mut self.device.renderer,
                replacement.mode,
                &replacement.settings,
            )
            .map_err(|error| error.to_string())?;
            let previous = self
                .device
                .outputs
                .get_mut(&crtc)
                .expect("the selected CRTC has a live or staged output in this transaction");
            previous.surface = replacement.surface;
            previous.dmabuf_feedback = replacement.dmabuf_feedback;
            previous
                .surface
                .commit_frame()
                .map_err(|error| error.to_string())?;
        }
        let Some(output) = self.device.outputs.get_mut(&crtc) else {
            return Ok(());
        };
        output.dmabuf_feedback.invalidate();
        output
            .surface
            .reset_state()
            .map_err(|error| error.to_string())?;
        let restored = output
            .surface
            .set_connectors(&[output.connector])
            .map_err(|error| error.to_string())
            .and_then(|()| {
                prepare(
                    &mut output.surface,
                    &mut self.device.renderer,
                    output.mode,
                    &output.settings,
                )
                .map_err(|error| error.to_string())
            })
            .and_then(|()| {
                output
                    .surface
                    .commit_frame()
                    .map_err(|error| error.to_string())
            });
        self.state.session_lock.output_added(&output.output);
        self.state
            .display_presentation
            .remove_output(&output.output);
        self.state
            .desktop_transition
            .as_mut()
            .expect("desktop transition must be started before publication or retirement")
            .redraw
            .insert(output.output.clone());
        output.lock_frame_pending = false;
        output
            .surface
            .set_output_mode_source((&output.output).into());
        cancel_output_timers(&self.state.loop_handle, output);
        output.frame_pending = false;
        restored
    }
}

/// All allocations/tests precede commits. Logical publication waits for the
/// final usable inventory; devices are independently applied and restored.
pub(super) fn apply_device(
    state: &mut Ferese,
    node: DrmNode,
    device: &mut DirectDevice,
    scan: OutputScan,
) -> Result<(), String> {
    let selections = scan
        .selections
        .into_iter()
        .map(|selection| (selection.crtc, selection))
        .collect::<HashMap<_, _>>();
    let color = super::sdr_color::Plan::capture(
        &device.drm,
        selections
            .values()
            .map(|selection| selection.connector.handle()),
    )
    .map_err(|error| format!("could not prepare SDR output state: {error}"))?;
    let mut changed = selections
        .iter()
        .filter_map(|(crtc, selection)| {
            let same = device.outputs.get(crtc).is_some_and(|output| {
                output.connector == selection.connector.handle()
                    && output.output.name() == selection.name
                    && output.identity == selection.identity
                    && output.mode == selection.mode
                    && output.settings.same_configuration(&selection.settings)
                    && output.mirror_source == selection.mirror_source
            });
            (!same).then_some(*crtc)
        })
        .collect::<Vec<_>>();
    changed.sort_by_key(|crtc| (selections[crtc].mirror_source.is_some(), u32::from(*crtc)));
    let removals = device
        .outputs
        .keys()
        .filter(|crtc| !selections.contains_key(crtc))
        .copied()
        .collect::<Vec<_>>();
    let hardware_changes = changed
        .iter()
        .filter(|crtc| {
            !device.outputs.get(crtc).is_some_and(|output| {
                let selection = &selections[crtc];
                output.connector == selection.connector.handle()
                    && output.mode == selection.mode
                    && output.settings.adaptive_sync == selection.settings.adaptive_sync
            })
        })
        .copied()
        .collect::<Vec<_>>();
    let mut driver = DeviceTransaction {
        state,
        device,
        selections: &selections,
        staged: HashMap::new(),
        replaced: HashSet::new(),
        color_plan: Some(color),
        color_applied: None,
    };
    let result = crate::output_transaction::apply(&mut driver, &hardware_changes, &removals);
    let mut staged = std::mem::take(&mut driver.staged);
    let color = driver.color_applied.take();
    if let Err(mut failure) = result {
        if let Some(color) = &color
            && let Err(error) = color.restore(&device.drm)
        {
            failure
                .error
                .push_str(&format!("; color rollback failed: {error}"));
        }
        for crtc in failure.unavailable {
            if let Some(mut output) = device.outputs.remove(&crtc) {
                retire(state, &mut output, node, crtc);
            }
        }
        return Err(failure.error);
    }
    for crtc in &changed {
        let selection = &selections[crtc];
        let mut output = staged
            .remove(crtc)
            .or_else(|| device.outputs.remove(crtc))
            .expect("the selected CRTC has a live or staged output in this transaction");
        if output.connector != selection.connector.handle()
            || output.identity != selection.identity
            || output.output.name() != selection.name
        {
            retire(state, &mut output, node, *crtc);
            let (handle, _) = create_output(
                state,
                selection.name.clone(),
                &selection.connector,
                selection.mode,
                selection.identity.clone(),
                &selection.settings,
                false,
            );
            output.output = handle;
        }
        if selection.mirror_source.is_some() && output.global.is_some() {
            retire(state, &mut output, node, *crtc);
        }
        output.connector = selection.connector.handle();
        output.internal = internal_connector(selection.connector.interface());
        output.identity = selection.identity.clone();
        output.settings = selection.settings.clone();
        output.mode = selection.mode;
        output.mirror_source = selection.mirror_source.clone();
        // Resource metadata describes applied hardware. Wayland output state,
        // workspace ownership and floating positions publish once after all GPUs.
        state
            .desktop_transition
            .as_mut()
            .expect("output reconciliation boundary")
            .redraw
            .insert(output.output.clone());
        state.session_lock.output_added(&output.output);
        if hardware_changes.contains(crtc) {
            state.display_presentation.remove_output(&output.output);
        }
        output.lock_frame_pending = false;
        output
            .surface
            .set_output_mode_source((&output.output).into());
        cancel_output_timers(&state.loop_handle, &mut output);
        output
            .scheduler
            .set_variable_refresh(selection.settings.adaptive_sync);
        output.frame_pending = false;
        output.primary_commit = None;
        output.capture_texture = Default::default();
        output.power.suspended();
        let clock = state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .presentation
            .entry((node, *crtc))
            .or_default();
        clock.set_refresh(OutputMode::from(output.mode).refresh);
        clock.reset_timing();
        device.outputs.insert(*crtc, output);
    }
    // Retire resources now; the final inventory determines evacuation once.
    for crtc in removals {
        let mut output = device
            .outputs
            .remove(&crtc)
            .expect("the selected CRTC has a live or staged output in this transaction");
        retire(state, &mut output, node, crtc);
    }
    device.connected_outputs = scan.connected_outputs;
    update_applied_info(device);
    Ok(())
}

pub(super) fn update_applied_info(device: &mut DirectDevice) {
    for info in &mut device.connected_outputs {
        info.enabled = false;
        info.current_mode = None;
        info.adaptive_sync_enabled = false;
        if let Some(output) = device
            .outputs
            .values()
            .find(|output| output.identity == info.identity)
        {
            info.enabled = true;
            info.current_mode = Some(connected_mode_info(output.mode));
            info.adaptive_sync_enabled = output.surface.vrr_enabled();
            info.scale = output.settings.scale;
            info.transform = output.settings.transform;
            info.configured_position = output.settings.position;
        }
    }
}

/// Disconnected hardware cannot remain logically live, even if a new policy fails validation.
pub(super) fn retire_disconnected(state: &mut Ferese) {
    let missing = state
        .direct_backend
        .as_ref()
        .expect("DRM callbacks require an installed direct backend")
        .devices
        .iter()
        .flat_map(|(node, device)| {
            device.outputs.iter().filter_map(|(crtc, output)| {
                device
                    .drm
                    .get_connector(output.connector, true)
                    .ok()
                    .filter(|connector| connector.state() != connector::State::Connected)
                    .map(|_| (*node, *crtc))
            })
        })
        .collect::<Vec<_>>();
    for (node, crtc) in missing {
        if let Some(mut output) = state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .devices
            .get_mut(&node)
            .expect("the selected DRM device must remain registered during this operation")
            .outputs
            .remove(&crtc)
        {
            let _ = output.surface.clear();
            retire(state, &mut output, node, crtc);
        }
    }
}

/// If initial activation or rollback leaves no usable desktop, try a preferred
/// mode on each physical monitor independently. Never disable a surviving output.
pub(super) fn recover_last_output(state: &mut Ferese) {
    if state
        .direct_backend
        .as_ref()
        .expect("DRM callbacks require an installed direct backend")
        .devices
        .values()
        .any(|device| {
            device
                .outputs
                .values()
                .any(|output| output.mirror_source.is_none())
        })
    {
        return;
    }
    let backend = state
        .direct_backend
        .as_ref()
        .expect("DRM callbacks require an installed direct backend");
    let requested = backend.desired_outputs.clone();
    let original_error = backend.output_error.clone();
    let mut candidates = backend
        .monitors
        .iter()
        .filter(|monitor| monitor.usable)
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by_key(|monitor| (!monitor.internal, monitor.key.clone()));
    for candidate in candidates {
        let backend = state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend");
        backend.desired_outputs = DesiredOutputConfiguration {
            profile: None,
            suspend: requested.suspend,
            outputs: backend
                .monitors
                .iter()
                .map(|monitor| {
                    let mut settings = crate::output_policy::defaults(monitor.identity.clone());
                    settings.enabled = monitor.key == candidate.key;
                    crate::output_policy::DesiredOutput {
                        key: monitor.key.clone(),
                        settings,
                        mirror_source: None,
                    }
                })
                .collect(),
        };
        let nodes = backend
            .devices
            .iter()
            .filter(|(_, device)| {
                candidate
                    .key
                    .starts_with(&format!("{}/", drm_device_key(&device.drm)))
            })
            .map(|(node, _)| *node)
            .collect::<Vec<_>>();
        if validate_desired_outputs(state).is_ok() {
            for node in nodes {
                let _ = rescan_device(state, node);
            }
            if state
                .direct_backend
                .as_ref()
                .expect("DRM callbacks require an installed direct backend")
                .devices
                .values()
                .any(|device| {
                    device
                        .outputs
                        .values()
                        .any(|output| output.mirror_source.is_none())
                })
            {
                remember_applied_configuration(state);
                state
                    .direct_backend
                    .as_mut()
                    .expect("DRM callbacks require an installed direct backend")
                    .output_error = original_error;
                tracing::warn!(identity = %candidate.identity, "recovered last usable output with preferred mode");
                return;
            }
        }
    }
    let backend = state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend");
    backend.desired_outputs = requested;
    backend.output_error = original_error;
}
