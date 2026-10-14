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
See [DMA-BUF feedback](dmabuf-feedback.md) for the current allocation hints.
Broader primary-plane and overlay policies still need hardware validation.
