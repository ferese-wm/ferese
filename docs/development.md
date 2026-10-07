# Development

Install the [build dependencies](installation.md#requirements), then run:

```sh
cargo build --release --locked --workspace
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
python3 -m unittest discover -s scripts/tests -v
```

Use release builds for performance and animation checks. Preview changes in a
[nested session](installation.md#preview-and-logs) before testing hardware behavior.
Keep another desktop available for direct-session recovery.

CI checks formatting, lints and tests across the whole workspace, including the
shell, settings, portal and CLI. The Fedora workspace job also builds desktop
binaries and runs installer, session-recovery and private portal-contract checks. The
compositor job runs offscreen capture regressions with software Mesa. Opt-in
nested and hardware checks remain separate.

## Test guides

- [Desktop reconciliation](desktop-reconciliation.md): topology publication, hardware failure and presentation ownership.

- [Output management tests](output-management-testing.md): profiles, hotplug, mirroring, lid handling and failed DRM commits.
- [Nested resize checks](nested-resize-testing.md): wallpaper and bar responsiveness.
- [Client soak testing](native-soak-testing.md): repeated client lifecycle and resource growth.
- [Performance measurements](performance-baseline.md): sampling and historical results.

## Portal checks

Run backend unit tests and compare D-Bus contracts against installed portal XML:

```sh
cargo test --locked -p xdg-desktop-portal-ferese
python3 scripts/tests/test_portal_contracts.py
```

The contract test runs on a private bus and checks signatures, appearance changes,
caller authorization and retention of the last working config after invalid edits.
It does not open consent dialogs or prove complete compatibility with every app.

With release binaries built, run isolated integration checks:

```sh
FERESE_TEST_SHORTCUTS=1 python3 scripts/tests/test_shortcuts_isolated.py
FERESE_TEST_WINDOW_CAPTURE=1 python3 scripts/tests/test_window_capture_isolated.py
FERESE_TEST_INHIBIT=1 python3 scripts/tests/test_inhibit_isolated.py
FERESE_TEST_RESTORE=1 python3 scripts/tests/test_restore_isolated.py
FERESE_TEST_WINDOW_STREAM=1 python3 scripts/tests/test_window_stream_isolated.py
```

These checks use disposable nested sessions or private buses to test shortcut
cleanup, window capture, inhibitors, restored permissions and live window streams.
Check each script's dependencies before running it.

For monitor streaming and lock revocation, install Python GI/GStreamer with
`pipewiresrc`, `dbus-daemon`, `xdg-desktop-portal`, and `bwrap`, then run:

```sh
FERESE_TEST_PORTAL=1 \
FERESE_TEST_PORTAL_BINARY=target/release/xdg-desktop-portal-ferese \
python3 scripts/tests/test_portal_isolated.py
```

Add `FERESE_TEST_PORTAL_CONSENT=1` to exercise the picker. Choose the temporary
display and click **Share**. Tests must target the nested preview, not the host desktop.

Check real encoding and the private InputCapture transport:

```sh
cargo test --locked -p xdg-desktop-portal-ferese --bin ferese-record encoder_finishes_a_real_webm -- --ignored
cargo test --locked -p ferese input_capture::tests
cargo test --locked -p xdg-desktop-portal-ferese eis::tests -- --ignored
```

Encoding needs the recorder's GStreamer plugins. The transport test needs libei
and uses synthetic input over a private socket pair. Hardware pointer barriers,
multiple monitors, display sleep, and live authentication still need session tests.

## Capture privacy checks

The capture privacy regression uses disposable Wayland clients and offscreen EGL
(including software Mesa); it does not capture the host desktop or validate DRM:

```sh
cargo test --locked -p ferese capture_privacy_pixels_and_policy_transitions -- --ignored
cargo test --locked -p ferese backends::direct::capture::tests -- --ignored
cargo test --locked -p ferese mirrored_pixels_fit -- --ignored
```

## Session lock checks

```sh
cargo test --release --locked -p ferese session_lock::tests -- --nocapture
```

These tests create private runtime directories and Wayland clients. They run in
normal test suites and in the compositor lifecycle CI job, without a host desktop or DRM.
They cover confirmation, rejected unlocks, owner death and replacement, output
removal, keyboard focus and idle timers. Physical display protection still needs
hardware testing.

## Resume listener checks

```sh
cargo test --release --locked -p ferese resume::tests -- --nocapture
```

These tests need `dbus-daemon`. They use a private bus to test missed resumes,
logind replacement and listener shutdown during connection setup, signal waits
and retries. They run in the compositor lifecycle CI job and never contact system logind.
Recovery requests fresh hardware and theme state without resetting idle activity.
Hardware suspend/resume still needs testing on a direct session.

## Idle inhibition checks

The playback test runs a fake MPRIS player on a private bus and opens a client in
a temporary nested desktop. It checks pause/stop, workspace switching, window closure,
player replacement and live rule changes. It needs a Wayland session,
PyGObject, `dbus-run-session`, `cc` and `wayland-scanner`.

```sh
cargo build --release --locked -p ferese -p feresectl
FERESE_TEST_IDLE_INHIBITION=1 FERESE_TEST_BINARY=target/release/ferese FERESE_TEST_CTL=target/release/feresectl python3 scripts/tests/test_idle_inhibition_isolated.py
```

## Report a bug

Include the commit or installed release, reproduction steps, backend (nested or
direct), monitor scales/transforms, and relevant logs. Avoid publishing passwords,
access tokens, or private window content.

## Resume recovery

The direct backend keeps track of known GPUs when their DRM resources are
unavailable. It handles hotplug, configuration and lid changes through the same
recovery path, deferring the work while the session is inactive. On activation,
it reads the lid state asynchronously and re-enumerates devices and connectors
before allowing presentation. A logind system-wake signal triggers the same work,
even if seat ownership has not changed. New libinput observations take precedence
over pending lid reads. If a read fails, Ferese keeps the last observation. Reads
have a three-second deadline so a stalled bus cannot block recovery indefinitely.

When a device fails, Ferese removes its output globals and retries with exponential
backoff capped at 32 seconds. Working devices remain usable. Known hardware can return
under a different `cardN` path. Reconciliation also discovers newly connected
GPUs. Connected-output reports mark a mode active only when
output creation succeeded. Lock ownership is retained throughout recovery.

Run the ordering and private D-Bus checks with:

```sh
cargo test --locked -p ferese backends::direct::topology
cargo test --locked -p ferese backends::direct::lid -- --include-ignored
```

The D-Bus test requires `dbus-daemon` and permission to create private sockets.
Hardware validation still needs a direct session: change monitors while on
another VT, suspend/resume with a dock disconnected, change the lid while
suspended, and unplug/reconnect the managed GPU where supported. Verify current
output geometry, idle notifications, recovery after failures and continued lock
protection. Nested sessions do not validate DRM reacquisition.

## Capture and DRM failure checks

The compositor tests cover screenshot admission across requests, cancellation
while buffers remain owned, staging-file cleanup, activation serials, child
reaping and wallpaper decode coalescing:

```sh
cargo test --release --locked -p ferese
```

Screenshot requests share a 256 MiB readback reservation. The reservation follows
pending readbacks, queued results and encoder jobs, including after cancellation.
It does not include GPU targets, the encoder's composition canvas or encoded PNG
bytes; it is not a cap on total process memory.

The offscreen pixel tests need EGL. CI runs them with software Mesa and private
Wayland sockets:

```sh
LIBGL_ALWAYS_SOFTWARE=1 EGL_PLATFORM=surfaceless cargo test --release --locked -p ferese state::capture_privacy::tests -- --ignored
LIBGL_ALWAYS_SOFTWARE=1 EGL_PLATFORM=surfaceless cargo test --release --locked -p ferese backends::direct::capture::tests::capture_recomposes_cursor_without_touching_the_display_target -- --ignored
```

A retirement error or a frame pending for two seconds triggers DRM device
reconciliation. Ferese keeps the frame marked pending until device teardown;
clearing that flag alone would leave Smithay's buffer ownership uncertain.
The watchdog keeps one timer per output while frames are flowing. Session pause
and output removal cancel it.

To exercise both failures on hardware, build with the test feature and start
Ferese from an unused VT. These commands each inject one failure. Device
recreation may briefly blank the displays; do not run them inside your active
desktop.

```sh
cargo build --release --locked -p ferese --features drm-fault-injection
FERESE_TEST_DRM_FAILURE=retirement target/release/ferese --backend drm
FERESE_TEST_DRM_FAILURE=missing-completion target/release/ferese --backend drm
```

Run them separately. Check that redraws resume, connected outputs retain their
identities, windows remain reachable, and lock protection survives the recovery.
The unit tests check retirement-result handling and pending-frame ownership;
they cannot prove hardware recovery succeeds. The installer leaves fault
injection disabled.
