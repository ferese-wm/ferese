# Rendering diagnostics and hardware baselines

Set `FERESE_TRACE_PERFORMANCE=1` in the environment of a fresh compositor process
and enable the `ferese::render` info log target. For a manual DRM session, for
example:

```sh
FERESE_TRACE_PERFORMANCE=1 RUST_LOG=ferese::render=info ferese
```

Use your normal supported session launch procedure. Do not start a second DRM
compositor inside a running desktop. Setting this variable on `feresectl` does
not enable counters in an already running compositor. To disable accounting,
remove the variable and restart; any value, including `0`, enables it.

Existing `render performance` logs describe timings and composition damage.
The direct backend additionally emits `DRM frame outcomes` per physical output,
roughly every five seconds when render or retirement activity reaches a reporting
point. Idle outputs need not emit a report. Counters reset after each report;
retirement of a submission can occur in the following interval. Output recreation
starts a new series. Mirror targets have their own series. Nested sessions do not
provide DRM outcome counters.

## GPU context priority

The direct backend requests `ContextPriority::High` through Smithay for each GPU's
EGL context, including device recreation. Smithay omits the priority attribute when
`EGL_IMG_context_priority` is unavailable. If context creation with the hint fails,
Ferese retries once without it and propagates any remaining initialization error.
The extension defines priority as a hint: driver capabilities, privileges and
system policy determine the granted priority. CPU scheduling boosts do not set it.

Enable `RUST_LOG=smithay::backend::egl=info,ferese=info` to see Smithay's
`EGL context created` log with the queried priority. `None` means the priority is
unknown, including when the extension or query is unavailable; it does not mean
High was granted. Smithay also reports a requested/granted mismatch. Ferese logs
when it retries without the hint.

The nested backend still uses Smithay's `winit` context setup, which exposes no
priority option in the pinned dependency. There are no separate production shared
renderer contexts. This request alone establishes no FPS, presentation-latency or
capture CPU improvement; those need repeated measurements under GPU contention
on physical hardware.

The fallback policy has ordinary unit tests. The opt-in initialization test uses
a real EGL device, including software Mesa, without exercising DRM or GPU contention:

```sh
LIBGL_ALWAYS_SOFTWARE=1 EGL_PLATFORM=surfaceless cargo test --locked -p ferese backends::direct::context::tests -- --include-ignored
```

## Counter meanings

| Fields | Meaning |
| --- | --- |
| `render_attempts` | Calls to Smithay `render_frame`, after scene construction; permission to attempt scanout does not prove that a client buffer was tested. |
| `primary_allowed`, `blocked_locked`, `blocked_animation`, `blocked_not_fullscreen` | Partition of attempts under the existing primary-plane policy. When multiple blockers apply, diagnostic precedence is lock, animation, then absence of fullscreen. |
| `render_failures`, `sync_failures`, `queue_failures` | Errors returned by rendering, explicit synchronization, or queueing respectively. Failed queues never increment submission outcomes. |
| `no_damage_results` | Smithay returned `is_empty`; no display frame was queued. Capture may still run. This is independent of primary selection. |
| `submissions` | Successful `queue_frame` calls, before completion; these are not presented-frame counts. |
| `scanout_submissions` | Queued result uses `PrimaryPlaneElement::Element`, Smithay's direct primary-plane selection. |
| `composed_submissions` | Queued result uses the swapchain with composition damage since the last successfully queued primary commit. |
| `reused_swapchain_submissions` | Queued swapchain result has no new composition damage. This can accompany cursor updates or forced commits; it does not prove a cursor-only update. |
| `allowed_composition_submissions` | Primary scanout permission was set, but the queued result uses a swapchain, including reuse. The public result does not expose exact candidate/driver rejection reasons. |
| `cursor_plane_submissions`, `no_cursor_plane_submissions` | Partition of successful queues by Smithay's cursor-plane assignment. No assignment can mean a hidden, absent, or composed cursor; it is not a hardware-cursor failure count. |
| `retired_frames`, `retirement_failures` | Successful retirement with feedback, or a retirement error. Only successful retirement counts as completed; no feedback and errors do not. A completion can lack a usable monotonic timestamp. These totals do not classify presented primary/cursor paths. |
| `capture_batches`, `capture_us` | Batches with pending monitor capture requests, and their separate CPU wall time through capture scene construction, recomposition and readback processing. A batch may process cursor-included and cursor-excluded requests. Failures remain included in this cost; asynchronous encoding is excluded. |

Primary outcomes partition submissions; cursor outcomes independently partition
submissions. A scanout/submission ratio describes queued outcomes, not the fraction
of wall time an output spent scanning out an unchanged client. Zero composition damage does not establish direct scanout. A
successful submission establishes neither kernel presentation nor a performance
improvement. Existing presentation schedule logs provide timestamps when usable; enable
`RUST_LOG=ferese::render=debug` to include them.
Capture timing is separate from submitted render timing. Existing no-damage
render timing includes capture work; do not add those overlapping totals.
These diagnostics do not change scanout flags, feedback, clipping, blur, capture
privacy, locking, gamma processing or recovery.

Counters are opt-in and use constant-size per-output storage. With diagnostics
disabled, capture accounting does not inspect pending requests or read a clock.
The implementation does not inspect additional surfaces or perform GPU timing.

## Repeatable hardware baseline

1. Record the exact commit/build, GPU and driver, kernel, output modes, refresh
   rates, scale, transforms, mirror layout, power state, application versions and
   relevant appearance settings. Save configuration and the full diagnostic log.
2. Use a real DRM session. Warm up each workload for 10 seconds, measure a fixed
   60-second window, and repeat it three times. Use matching time windows and
   sum counters across intervals; record the actual workload duration separately.
   Keep idle cases separate because they may not produce periodic reports.
3. Start with an opaque settled fullscreen client, then toggle fullscreen and
   exercise animations and popups. Compare it with a non-fullscreen and a rounded
   or translucent scene. Move/show/hide the cursor in each case. Expect permission
   and observed outcomes to differ; record both without assigning unreported
   hardware rejection reasons.
4. Repeat with monitor screenshots/sharing, cursor included and excluded,
   protected windows, overlapping blurs and moving content beneath blur. Check
   captured pixels and privacy, not just timings. Test lock/unlock with capture
   active and verify no desktop capture survives lock acquisition.
5. Repeat on mixed outputs, mirrors, fractional scaling and rotation; exercise
   hotplug, gamma/night light and the recovery checks in
   [Output management tests](output-management-testing.md). Check visual and
   capture correctness after fallback and after returning to scanout.
6. Compare successful queue outcomes, composition damage, CPU render timings,
   capture timings and presentation schedule timestamps against the same baseline.
   Measure GPU work or power with an appropriate external tool and record its
   method. Report variation across repetitions. Software tests and nested sessions
   cannot establish physical scanout, GPU savings or presentation latency.

Follow-on instrumentation should measure blur invalidation, clean-cache reuse,
actual framebuffer recapture counts/cost, asynchronous capture encoding and
window-capture readback separately. Current effect metrics are placeholders;
`capture_us` covers monitor capture batches only. Exact cursor-only KMS updates
and detailed scanout rejection causes require additional upstream observability.
Broader primary-plane and overlay policies still need hardware validation.

## Capture scene reuse

With `FERESE_TRACE_PERFORMANCE=1`, both backends emit `capture scene outcomes`
through the `ferese::render` log target. Each report covers one retained output
view, with separate counts for cursor-hidden and cursor-embedded capture, and
resets its counters after roughly five seconds of capture activity.

`preparations` counts successful filtered-scene preparations, `scene_reuses`
counts checks that skipped rebuilding that scene, and `compositions` counts
renders with nonempty capture damage. A pending damage-waiting frame may be
checked repeatedly without a delivery. A preparation can also find unchanged
pixels, so it need not increase the composition count. These counters describe the
capture source; they do not count PipeWire deliveries, measure GPU time or
describe window-only capture. For a mirror, they describe its filtered source
before fitting it to the target output.

An unchanged view reuses its retained GPU image before constructing capture
render elements. Surface commits, layout changes, animation ticks and privacy
changes invalidate it, including the final animation frame. Cursor updates
invalidate only views that embed the cursor. Output identity, scale, transform
and geometry are checked as well, so a remapped or reconfigured output cannot
reuse the previous view by mistake. Capture requests alone do not invalidate
the image, and multiple mutations before a request uses it are coalesced.

This is conservative invalidation: an unrelated redraw may still cause a
preparation, after which damage tracking can find unchanged pixels. It does not
reuse the display swapchain or partially update consumer buffers. DMA-BUF and
shared-memory delivery still write complete frames, which keeps a recycled
buffer from exposing pixels left by a previous use.

## IPC snapshot CPU benchmark

The ignored release test below measures construction of unchanged desktop event
snapshots with the default theme. It warms up the state, then reports three runs
of 2,000 snapshots. Keep the runtime and state directories private so the test
cannot connect to your desktop or write its configuration.

```sh
ipc_benchmark_runtime=$(mktemp -d)
env -u FERESE_SOCKET \
    XDG_RUNTIME_DIR="$ipc_benchmark_runtime" \
    XDG_CONFIG_HOME="$ipc_benchmark_runtime" \
    XDG_STATE_HOME="$ipc_benchmark_runtime" \
    XDG_CACHE_HOME="$ipc_benchmark_runtime" \
    FERESE_STARTUP_TEST_CHILD=ipc_events::tests::profile_unchanged_event_snapshots \
    cargo test --release --locked -p ferese profile_unchanged_event_snapshots -- --ignored --nocapture
rm -rf -- "$ipc_benchmark_runtime"
```

Compare the reported `elapsed_us` values on the same machine and build profile.
These timings cover CPU snapshot construction, while the DRM procedure above
measures rendering and presentation under real desktop workloads.

For populated desktops, replace `profile_unchanged_event_snapshots` with
`profile_populated_event_publication` in both the test filter and
`FERESE_STARTUP_TEST_CHILD`. This test creates one output and private Wayland
clients, then measures three runs of 2,000 unchanged publication checks with
0, 1, 20 and 100 windows. Setup and the initial subscription event happen before
timing. Window records stay typed through comparison and serialize only when
an event is sent, avoiding JSON objects for checks that produce no update.
The benchmark includes construction, comparison and disposal of each snapshot,
while socket serialization and subscriber processing are excluded. Keep CPU
affinity identical when comparing binaries on machines with mixed core types.

## Shell snapshot handoff benchmark

The shell's ignored release test measures delivery of a completed desktop
snapshot from the control thread to the UI. It uses a private socket pair and
tests snapshots with 1, 20 and 100 windows, reporting three runs of 2,000
handoffs for each size.

```sh
cargo test --release --locked -p ferese-shell profile_snapshot_handoff -- --ignored --nocapture
```

Snapshot preparation happens before timing; delivery, polling and destruction
are included. Compare runs on the same machine without concurrent builds.
The benchmark excludes protocol decoding, UI layout and drawing, so its results
describe this handoff's CPU cost rather than overall shell responsiveness.

## Monitor capture benchmark

Monitor captures keep their own damage history alongside the capture texture.
The first frame fills the texture, and later frames redraw only damaged regions
before readback. Cursor inclusion, disappearing surfaces and output changes are
resolved against the capture scene, independently of the displayed image.
Cursor-included and cursor-hidden streams retain separate targets, so alternating
between them does not invalidate otherwise unchanged captures. Mirrored captures
also track damage in their filtered source composition before fitting it to the
mirror output.

Run the ignored release benchmark on a hardware EGL device:

```sh
cargo test --release --locked -p ferese profile_output_capture -- --ignored --nocapture
```

It creates a synthetic 3840×2160 scene with overlapping translucent panes and
tests unchanged frames, cursor movement and full-background changes. Each case
warms up for 60 frames and reports three runs of 60 frames. With `readback=true`,
timing includes a full-frame GPU readback and a copy into CPU memory. With
`readback=false`, a one-pixel readback waits for GPU completion while avoiding
the full-frame transfer cost. These are elapsed times through the renderer,
rather than GPU timestamp measurements.

Compare old and new release binaries in alternating runs on the same GPU, and
record other GPU workloads. The test captures no desktop content and excludes
PipeWire delivery, encoding and network transport, so it cannot establish an
end-to-end OBS or call-frame latency improvement.

## Window capture conversion benchmark

The one-shot window capture path converts premultiplied readback pixels to straight
ARGB. Streaming performs this conversion in its GPU transfer shader. Opaque pixels need only a channel swap, while partial
alpha uses a 64 KiB table compiled into read-only storage. The table preserves
the original rounded and saturated conversion without divisions in the frame
loop or runtime allocation for lookup data.

```sh
cargo test --release --locked -p ferese profile_window_conversion -- --ignored --nocapture
```

The test measures 1080p and 4K frames with opaque content, translucent edges,
uniform translucency and varying alpha. Each case warms up for three frames,
then reports three runs of three frames. Input colours vary across the frame,
and the benchmark restores the premultiplied input before each conversion,
outside the timed interval. Compare release builds with matching CPU affinity
and record competing CPU workloads.

These timings isolate pixel conversion. GPU readback, copying into client
buffers, PipeWire delivery and encoding require separate measurements; use
live thread CPU samples to identify how much conversion contributes to a
running sharing session.


## Native stream validation

Streaming uses `ext-image-copy-capture-v1` with persistent capture sessions and a
four-buffer PipeWire pool. The compositor writes into those buffers directly,
using DMA-BUF when allocation and format negotiation permit it, or anonymous
shared memory otherwise. A pending capture waits for source damage, and each
completed buffer is delivered once. DMA-BUF frames become available only after
the exported GPU fence signals; cancellation, privacy changes and source removal
also invalidate pending delivery.

The isolated window test captures synthetic clients through a real PipeWire
consumer, checks silence while content is static, then requests distinct updates.
It starts a callback-driven animation on an inactive workspace, reconnects
without changing the source, resizes while paused, verifies the replacement pool
and revokes the window's capture permission. A monitor stream
then exercises damage waiting and real updates through the filtered output path.
Set `FERESE_TEST_CAPTURE_TRANSPORT` to `shm` or `dma` to require that memory type
in the native consumer, rather than merely accepting an available transport.
DMA-BUF checks require a hardware-backed nested compositor and access to its
render node.

Set `FERESE_TEST_CAPTURE_OBS_BUFFERS=1` with either transport to exercise OBS's
memory-type bitmask and mapped-buffer requirements. This catches negotiation
failures that a consumer offering just one memory type can miss.

```sh
FERESE_TEST_WINDOW_CAPTURE=1 FERESE_TEST_CAPTURE_TRANSPORT=shm \
  python3 scripts/tests/test_window_capture_isolated.py
FERESE_TEST_WINDOW_CAPTURE=1 FERESE_TEST_CAPTURE_TRANSPORT=dma \
  python3 scripts/tests/test_window_capture_isolated.py
cargo test --locked -p ferese render::capture::stream_gpu_tests -- --ignored
```

The GPU tests compare all 65,536 alpha/channel combinations against the CPU
conversion and check all eight output transforms with both texture orientations.
Native streaming tests establish buffer transport and pixel correctness; measure
CPU, GPU and encoding workloads separately before attributing a change in total
streaming performance to capture alone.

The recorder retains the last owned image and timestamps one final reference when
recording stops, so a static desktop keeps its full duration without requesting
or encoding duplicate frames throughout the idle interval. The regression test
covers an explicit stop, EOS from the capture source and continuous audio while
the video stays static:

```sh
cargo test --locked -p xdg-desktop-portal-ferese --bin ferese-record \
  recorder::tests::encoder_preserves_the_duration_of_a_static_capture -- --ignored
```
