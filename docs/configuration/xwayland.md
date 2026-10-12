# X11 support (xwayland-satellite)

Ferese can run legacy X11-only applications through
[xwayland-satellite](https://github.com/Supreeeme/xwayland-satellite).
```kdl
xwayland {
    enabled #true
    startup  on-demand
    path     "xwayland-satellite"
}
```

| Key | Values | Default | Meaning |
| --- | --- | --- | --- |
| `enabled` | `true` / `false` | `true` | Run the bridge at all. `false` is a complete opt-out. |
| `startup` | `on-demand` / `eager` | `on-demand` | `on-demand` starts Satellite the first time an application connects to the X11 socket. `eager` starts it during session startup. |
| `path` | path to an executable | `xwayland-satellite` | Which binary to run. |

These keys require a new session. Reloading configuration does not restart or
reallocate the X11 service; `feresectl xwayland status` reports
`restart_required` when the running configuration no longer matches the file.

## Requirements and failure behavior

`xwayland-satellite` and an `Xwayland` binary it can find must be installed.
Satellite must be built with its `systemd` feature enabled
(`cargo build --release --features systemd`); that feature sends the `READY=1`
notification Ferese waits for. Upstream's default feature set is empty, so a
default build never sends it and every start ends in a startup failure after
Ferese's fixed 10-second budget, even though the process is running. Being
spawned is not readiness. If any of these are missing, the session still starts
as a normal Wayland desktop and `feresectl xwayland status` explains why X11 is
unavailable. Ferese never publishes a `DISPLAY` value that does not work, so
applications fail cleanly instead of hanging against an endpoint that accepts
nothing.

In a nested session, Ferese's own environment is preserved for backend
initialization, but applications it launches receive the new Ferese endpoints.
The host's `DISPLAY` is never exported to the activation environment of a nested
session, so it cannot leak into services started by your login session.

## Diagnostics and recovery

```text
feresectl xwayland status
feresectl xwayland retry
```

`status` reports whether the service is idle, starting, running, backing off, or
failed, plus the display, the Satellite PID, and whether readiness was verified.
A failed service is not restarted automatically: Ferese retries a small bounded
number of times, then stops and waits, so a broken binary cannot produce a
restart loop. Run `feresectl xwayland retry` once the cause is fixed. The
snapshot never contains the X11 cookie, so it is safe to paste into a bug
report.

`retry` can also refuse to run. If the previous service group cannot be proven
gone — for example it survived the stop sequence — Ferese keeps its cleanup
record instead of starting a second generation that would compete with
survivors for the same display, and the retry fails with a message naming what
still holds the X11 display. The endpoint is not republished in the meantime.
Run the retry again once that group has finished exiting.

Each start uses a fresh, private notification socket and only a `READY=1`
message from the Satellite process that was actually spawned is accepted, so a
stale or unrelated notification cannot be mistaken for readiness.

## Limitations

- X11 applications are ordinary Wayland windows as far as layout, focus, and
  window rules are concerned. Rules match the application identity Satellite
  reports, so they are configured the same way as for native clients.
- Native Wayland preferences are kept as they are. Ferese does not force GTK,
  Qt, Firefox, or Electron onto X11 to demonstrate support.
- Portal dialogs requested with an `x11:` parent are shown **unparented**. The
  chooser or consent prompt still opens and authorization is unchanged; Ferese
  does not yet translate an X11 window ID into a Wayland export, and inventing a
  parent would be a security problem.
- Per-monitor fractional scaling of X11 applications depends on the selected
  Satellite build and its Xsettings policy. Text sharpness, pointer coordinates,
  and popup placement should be checked on each output profile rather than
  assumed.
- Drag and drop between native and X11 applications depends on the Satellite
  build and is not part of the initial support claim.
- Sandboxed applications may need explicit permission to reach the display. A
  flatpak with only `fallback-x11` gets no X11 socket inside a Wayland session and
  cannot read the private `XAUTHORITY` file under `$XDG_RUNTIME_DIR`, so it exits
  without a window while the service itself reports healthy. See
  [Sandboxed applications](../installation.md#sandboxed-applications).
