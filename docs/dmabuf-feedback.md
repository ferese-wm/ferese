# DMA-BUF allocation feedback

DRM sessions publish renderer formats through a Linux DMA-BUF v4 global for each
render device. Its default feedback identifies that renderer's render node (or
its primary node when no render node is available). Per-surface feedback starts
with this same render fallback; requesting feedback does not require a mapped
window or a scanout-capable buffer.

After a successful display queue, or a successful render returning no display
changes, Ferese can add an output-specific `scanout` preference. It uses the
primary plane selected by that output's DRM surface, not a union of all planes.
Only an exact FourCC/modifier pair shared by that plane and the renderer is
preferred. The pair must have been observed on the primary result. This narrow
choice follows the current policy's full-format comparison with the swapchain;
it does not advertise broader primary formats, cursor formats or overlay formats.
Implicit modifiers are not equated with explicit linear modifiers, and alpha
formats are not equated with their opaque counterparts.

The preference's target device is the output's primary DRM node; its main device
and renderer fallback remain the originating global's render device. Every
preferred pair is also renderer-importable. Failed preference construction retains
render feedback rather than failing an already queued display frame.

## Surface selection and invalidation

Only the current mapped fullscreen toplevel can receive the preference. Its last
successful frame must expose underlying buffer storage, opaque content covering
the output and full opacity. Rounded shader clipping continues to hide storage.
Current opacity or dimming also withdraws the preference immediately.
The surface must be visible on exactly one registered output, and that output
must belong to the originating import global. Animation, overview, locking,
paused/inactive outputs and mirror targets receive render fallback. A failed
scanout reported by Smithay withdraws the preference; a format-unsupported result
can still receive a compatible allocation hint.

Feedback selection refreshes after event dispatch, before flushing protocol
messages, even when no frame is drawn. Migration, fullscreen exit, unmapping and
changes in eligibility restore render-only feedback. Multiple visible outputs do
not race to replace one another's feedback. Subsurfaces, popups, layer surfaces
and cursors currently retain render-only feedback; eligibility expansion is a
separate hardware-gated phase.

Mode changes, surface replacement, rollback and session pause/reactivation
invalidate output observations. A fresh successful frame is needed before a
preference returns. Retiring a GPU withdraws scanout preferences and retires its
global. Version 4
requires a main-device tranche, so existing bindings retain last-known renderer
capabilities rather than receiving empty feedback or being retargeted to another
GPU. These retained capabilities do not promise device availability: imports
through that retired global are rejected. A reconnected device publishes a new
global. Destroyed surface requests are pruned.
Smithay owns the subscriptions, format-table transport, tranche encoding and
suppression of unchanged updates. Its feedback state is shared by all subscriptions
for one surface; the initial live request determines the originating global.
Destroy all feedback objects before requesting the surface through a different
global to change that association.

Cross-GPU migration keeps the originating renderer's fallback instead of
advertising unverified formats or retargeting imports to another GPU. This does
not establish cross-GPU composition compatibility: imports are still checked
through the originating global's renderer, and the existing destination-renderer
behavior needs physical validation. Version 3 clients have no per-surface feedback
updates; their existing import checks remain authoritative.

Allocation feedback is an optimization hint, not proof that a particular scene
or client buffer will scan out. The compositor still owns plane assignment,
clipping, blur, capture privacy, lock protection, gamma and composition fallback.
No plane eligibility flags change here.

## Checks and hardware gate

```sh
cargo test --locked -p ferese backends::direct::feedback -- --nocapture
cargo test --locked -p ferese dmabuf_feedback -- --nocapture
```

Deterministic tests cover exact intersections, device identities, observed format
changes, candidate geometry, queue failure, migration, multiple outputs, policy
withdrawal, scanout failure and reconfiguration. A private Wayland v4 client checks
initial render feedback, subscriptions, preference/fallback changes, unchanged
update suppression, feedback renewal and retired-device preference withdrawal. It does not
create a real DMA-BUF, use DRM or establish hardware acceptance.

Before expanding eligibility, record the build, GPU/driver/kernel, output modes,
scale/transforms, application and its actual allocated FourCC/modifier. Decode
v4 feedback device IDs and format tables and verify preferences against the live
renderer and selected primary plane. Test fullscreen entry/exit, animations,
opacity/clipping, popups, sharing and lock/unlock; compare captured pixels and
allocation outcomes. Exercise mixed outputs, mirrors, migration, hotplug,
reconfiguration/rollback, pause/resume, gamma and failed scanout. Include v3 clients
and applications that ignore or follow feedback, and explicitly validate
cross-GPU imports. Use the [output-management hardware checks](output-management-testing.md).
Physical scanout rate, GPU work, power and presentation latency remain unmeasured
until those hardware baselines are collected.
