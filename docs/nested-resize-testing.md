# Nested resize checks

Build and launch a preview with protocol and render logging:

```sh
cargo build --release --locked -p ferese -p ferese-shell
WAYLAND_DEBUG=1 FERESE_TRACE_PERFORMANCE=1 \
  target/release/ferese --backend nested --grant-effects --grant-shell-control -- \
  target/release/ferese-shell 2>/tmp/ferese-resize.log
```

Set a wallpaper, then drag each edge of the host window and repeatedly maximize
and restore it. The wallpaper and bar should resize without long stalls. Check
this at fractional scaling too.

## Compare renderers

Repeat with `ICED_BACKEND=wgpu` and `ICED_BACKEND=tiny-skia`. Shell and Settings
prefer software rendering, with GPU fallback. This keeps Settings labels visible
inside scrolling content; software can be slower when resampling wallpaper.
An explicit `ICED_BACKEND` overrides this preference.

Follow the trace from host resize through layer configure, client acknowledgement,
new buffer commit and compositor rendering. A quick acknowledgement does not mean
the client has finished drawing. `FERESE_TRACE_PERFORMANCE` measures compositor
work, not shell rasterization.

## Historical check

A KDE Wayland check at 175% scale used debug binaries and protocol tracing.
Median configure acknowledgement was 1.35 ms with tiny-skia and 2.27 ms with
wgpu. Wallpaper buffer intervals were 5919 ms with tiny-skia (only two buffers)
and 9.85 ms median with wgpu. The captures had different durations, so these
observations do not establish comparative throughput. GPU intervals also had stalls
up to about 200 ms.

Repeat on the current build. A nested result does not establish direct-session
resize or frame-pacing behavior.

## Controlled slow client

Build the compositor and control tool, then run on a Wayland host:

```sh
cargo build --release --locked -p ferese -p feresectl
FERESE_TEST_RESIZE=1 FERESE_TEST_BINARY=target/release/ferese \
  FERESE_TEST_CTL=target/release/feresectl \
  python3 scripts/tests/test_resize_dependencies_isolated.py
```

The test creates a private nested desktop and two SHM clients. One acknowledges
a resize immediately but withholds its new buffer until instructed. It checks
that an independent first-column viewport target moves both windows together
before the 300 ms deadline, while the slow client's raster and presented size
stay held, then commits an actually resized raster. This is a state/coordinate
check on the selected host renderer, not a smoothness or hardware benchmark.

For deterministic commit, supersession, multiple-client, timeout, unmap,
workspace, swipe-cancel, stacked-column, prediction, hit-testing, snapshot and
capture privacy coverage without a host desktop:

```sh
cargo test --locked -p ferese resize_dependencies_protocol_and_pixels -- --ignored
```

That test requires an offscreen EGL device, including a software device. Layout
and dependency unit tests run without EGL in the ordinary workspace test suite.
Viewport dependencies retain at most 32 relative target recipes; a longer burst
falls back to a conservative linked wait until commit, deadline, or an absolute
retarget. Existing resize snapshots and their handoff lifetime are reused.
