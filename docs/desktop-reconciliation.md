# Desktop reconciliation and presentation

During an output topology change, Ferese publishes one logical desktop after
the usable hardware inventory is known.
The boundary is synchronous within one event-loop dispatch;
it does not wait for client commits, animation completion or page flips.

## Previous flow

`direct/topology.rs::reconcile_outputs` already centralized hotplug, profile
changes, retry and resume. It debounced connector events, validated device-wide
KMS requests, applied devices independently and attempted compensation on failure.
Its `reconciling` guard suppressed relayout, shell broadcasts and frame scheduling.

Inside that guard, `apply_device` still changed live Wayland output metadata,
registered each output and updated output/workspace geometry. Registration could
restore evacuated workspaces and reposition floating windows. Retirement called
`unregister_output`, which combined render/capture/lock cleanup with workspace
migration, focus changes, floating movement and relayout. Thus later devices or
rollback operated on desktop policy already changed by earlier devices.

The end of reconciliation restored focus, performed global relayout (including
client configures, surface preferences, a shell snapshot and redraw), then made
another global redraw request. Profile configuration reload could relayout again.

## Current phases

1. **Observe and choose desired hardware.** Reconciliation uses the existing
   monitor identities, profile selection, position policy, hotplug coalescer and
   lid/resume handling.
2. **Validate and apply hardware.** The backend uses device-wide atomic TEST_ONLY
   validation, Smithay resource staging/commits, per-device rollback, cross-device
   compensation and last-output recovery.
   These operations update resource metadata, not live
   workspace ownership or published output geometry.
3. **Plan the publishable desktop.** After all hardware outcomes are known,
   `publish_runtime_desktop` reports usable logical outputs. Physical mirror
   scanouts do not acquire workspaces. `OutputWorkspaceMap::plan_desktop` stages a
   small copy of ownership, geometry, focus and evacuation history. It uses the
   existing connect/reclaim/evacuate policy and reserves any needed workspace IDs.
   It rejects duplicate IDs and invalid geometry without changing the source map.
4. **Publish logical state.** `Ferese::publish_desktop` computes floating moves
   from the old geometry directly to the final geometry, then installs output
   registries, Wayland output metadata, ownership, floating destinations and logical
   focus. Slides that reference evacuated workspaces are retired. Unaffected
   viewport, world-coordinate, window-animation and resize state remain intact.
5. **Configure and finalize.** `finish_desktop_transition` performs one relayout
   when needed, restores keyboard/lock focus, refreshes lock/idle policy, releases
   the shell publication guard and requests redraws for affected outputs.
   Existing layout
   remains responsible for tiled destinations, layer work areas, surface-tree
   scale/transform preferences, XDG configures and animation target ownership.
6. **Present asynchronously.** The existing resize barriers, native-content
   snapshots, animations, frame scheduling and `DisplayPresentation` handle
   client readiness, motion, frame submission and displayed-content tracking.
   Logical publication does not assert that new pixels were displayed.

The desired hardware plan precedes application; the ownership plan is calculated
from the result of application and recovery. This avoids computing
and then undoing migrations to requested outputs that never became usable.

## Types and scope

- `DesktopPlan` is pure data: the staged `OutputWorkspaceMap`, reserved workspace
  IDs, affected output IDs and focus dirtiness. It copies neither window layouts
  nor client buffers nor the whole compositor.
- `DesktopOutput` adapts actual backend resources to that planner. Its Wayland
  handles, mode, scale and transform stay in the compositor, outside the pure map.
- `DesktopTransition` holds short-lived resource retirement/redraw bookkeeping and
  guards shell publication while the synchronous transition is in progress.
- `DesktopChanges` distinguishes layout, focus, shell and redraw consequences for
  the finalizer. Resume can request fresh frames without forcing a topology change.

Registration/removal outside DRM and nested output resizing use the same logical
publication path. Profile reload carries accompanying scene dirtiness into its
finalization rather than immediately relayouting the desktop a second time.
Pointer movement, client damage, ordinary focus, title changes, cursor updates and
frame callbacks do not construct or run a desktop plan.

## Failure, retirement and security

A rejected hardware configuration leaves the desktop unchanged if the previous
hardware remains usable. After physical loss or failed restoration,
unusable scanouts are retired immediately, and the final plan excludes them.
There is no atomicity guarantee across independent DRM devices.

Low-level retirement still disables globals, cancels frame timers and removes DRM
presentation clocks. `retire_output_resources` clears records of displayed client
content, output render caches, pending screencopies and lock resources.
It does **not**
migrate workspaces, change logical focus or relayout. Repeated retirement within
the same boundary is harmless. Publication removes the output from Space and the
logical registries once the final inventory is available.

If every physical output disappears, a headless desktop is valid: windows and
workspace restoration history survive, while output ownership and keyboard focus
are cleared. A requested all-disabled configuration still follows the existing
validation/recovery policy. A powered-off output is not the same as a retired
output; runtime output power remains independent of topology.

Session-lock protection remains mandatory for new/reconfigured scanouts. Hardware
resets invalidate records of previously displayed content and lock frames.
The boundary does not
allow a saved desktop frame to substitute for a protected lock frame.

## Readiness, motion and shell visibility

`ResizeTransaction` remains the only configure readiness barrier. Its existing
300 ms deadline and workspace coupling are unchanged: neighbors sharing the same
viewport may pause together, but an unrelated workspace/output continues advancing.
Existing committed client size/content may lag a newly published logical size.
Reconciliation does not acknowledge client configures, advance springs itself,
reset unrelated velocity or introduce another snapshot/animation system.

Shell broadcasts and initial subscription snapshots are deferred while the
boundary is open. The finalizer releases that guard only after output ownership,
focus, workspace cleanup and logical window layout agree. The existing shell
snapshot protocol and deduplication remain in use. A snapshot describes the
committed logical state; it does not mean every output has displayed that state.

`DisplayPresentation` remains a record of queued/displayed client visibility,
not a topology transaction or a frame buffer store. Existing native resize
snapshots survive output-cache retirement; GPU context loss still invalidates
textures owned by that context. No new cross-GPU snapshot transport is introduced.
Hardware preparation still uses a black modeset frame where required by the
existing backend. This change prevents unintended logical intermediate desktops;
it does not guarantee uninterrupted physical scanout through a modeset or
device loss.

## Regression coverage

Pure planner tests cover addition/removal, focused and non-focused removal,
replacement of multiple outputs, reconnect/explicit reassignment, a returning
output whose donor retires in the same plan, headless recovery, failed planning,
no-op/idempotent reconciliation and deterministic 512-step ownership sequences.
A separate 256-step sequence mutates topology, focus, workspace/window membership
and validates ownership and preservation of every window after each step.

The fake hardware transaction test applies commit and rollback failures, then
plans from survivors: successful rollback produces no logical change; failed
rollback produces a coherent reduced desktop without the requested new output.

The private Wayland integration regression uses actual shell, XDG toplevel and
session-lock resources. It checks no intermediate shell generation, one complete
final snapshot, repeated retirement, floating clamping, focused-window migration,
removal during a workspace slide, pending XDG resize after publication, another
workspace's progress during the wait, deadline expiry, no-op inventories and
locked headless loss/recovery. Fractional-scale/rotation tests compare planner
geometry directly with Smithay's Space geometry.

Physical KMS commits, cross-GPU recovery and resume still require the native
checks in [output-management-testing.md](output-management-testing.md). Pure and
private-protocol tests do not validate a DRM driver's behavior.
