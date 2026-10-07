# Native-client soak testing

The soak runner repeatedly opens and closes native Wayland clients, testing focus,
resize, column width, fullscreen, floating windows and workspace changes. Every
tenth iteration terminates a client abruptly. When a local malformed-client probe
is available, it sends malformed Wayland messages through three isolated connections.
The runner requires Ferese to disconnect each malformed client, then checks that
the compositor and authenticated IPC socket still respond. It also records
compositor RSS growth.

Build `ferese` and `feresectl` and start a disposable
[nested session](installation.md#preview-and-logs). Select its Wayland socket and
compositor PID explicitly, then run:

```sh
WAYLAND_DISPLAY=wayland-N FERESE_PID=PID ./scripts/soak-native.sh
```

Replace `wayland-N` and `PID` with the preview's values. Do not use the host socket
or rely on automatic PID selection when multiple sessions are running.

The default duration is 24 hours. A shorter validation run is:

```bash
WAYLAND_DISPLAY=wayland-N \
FERESE_PID=PID \
FERESE_SOAK_SECONDS=300 \
./scripts/soak-native.sh
```

Development probes are local tools and are not distributed in the Cargo workspace.
Set `FERESE_MALFORMED_CLIENT` to an existing executable to enable protocol checks.
If you explicitly configure a probe that does not exist, the runner reports an
error. Without a probe, it reports that protocol checks are skipped.

The local probe can also be run independently against a disposable Ferese session:

```bash
WAYLAND_DISPLAY=wayland-1 /path/to/ferese-malformed-client
target/debug/feresectl outputs
```

Do not point the malformed-client probe at the host compositor socket.

Environment controls:

- `FERESE_SOAK_SECONDS`: run duration, default `86400`;
- `FERESE_SOAK_DELAY`: delay between operations, default `0.15` seconds;
- `FERESE_SOAK_CLIENT`: native client executable, default `foot`;
- `FERESECTL`: path to `feresectl`, default `target/debug/feresectl`;
- `FERESE_MALFORMED_CLIENT`: path to the malformed protocol probe, default
  `target/debug/ferese-malformed-client`;
- `FERESE_PID`: compositor PID; automatic lookup is only a fallback;
- `FERESE_SOAK_LOG`: result log, default `/tmp/ferese-soak-<pid>.log`.
- `FERESE_SOAK_MAX_RSS_GROWTH_KIB`: maximum RSS growth, default `131072`
  (128 MiB);
- `FERESE_SOAK_MAX_FD_GROWTH`: maximum open-file-descriptor growth, default
  `32`;
- `FERESE_SOAK_MAX_THREAD_GROWTH`: maximum thread growth, default `8`.

Run the same workload once with the nested backend and once from a direct DRM
session. Samples are taken after spawned clients are reaped. A passing run
requires the compositor to remain alive, IPC queries to succeed, and RSS, file
descriptor, and thread growth to stay within the declared budgets. Tighten the
defaults for release hardware when its normal cache behavior is known. Preserve
the soak log and the `ferese::render` summaries with the release artifacts.
