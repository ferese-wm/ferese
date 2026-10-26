# Screen sharing and recording

Share a monitor, workspace, or window from a Wayland-compatible recording or conferencing app,
or save a video using the panel's Recording control. The
[installation guide](installation.md) covers the portal and PipeWire dependencies.

## Record from the panel

1. Click the Recording item in the panel, or open Controls → Recording in Morph.
2. Choose a display and an audio source: **Off**, **System sound**, **Microphone**,
   or **Both**. Audio is off by default; the other choices use
   your default output or input device from audio settings.
3. Click **Record** to capture the whole display. For part of it, turn on
   **Select a region**, click **Select region**, then drag a rectangle inside
   the chosen display. Press Escape to cancel selection.
4. Click the stop square to finish.

The recorder saves WebM files with VP8 video and, when selected, Opus audio under
`Ferese` in your XDG Videos folder, then shows the path in a notification. Region
recordings cover a rectangle within one display; they cannot span displays.
If recording fails, a notification reports the error and any unfinished
`.webm.part` file.

## Start sharing

### Panel status

Add **Capture status** in Settings → Panel → Edit panel → Add item.
You can move it between groups, hide it, or choose Full, Compact or Icon.
New panel presets include it. Add it yourself if you use a saved layout.

The item appears when an app starts capturing a source you've approved through
the desktop portal. Click it to see the application and shared sources.
OBS preview capture shows **Sharing**, even when OBS is not broadcasting.
Ferese's built-in recorder keeps its separate Recording item. In Morph, capture
status appears in the hub when the Capture status item is enabled.

OBS has a monochrome app icon. Other capture apps use the sharing icon, and both
follow the active panel theme. The red dot means an output is live or recording.
Sharing, paused recording, and reconnecting alone do not show that dot.
Choose Full for elapsed time, Compact for status text, or Icon for just the icon
and status dot. Click the item for separate stream and recording times.

To show OBS **LIVE**, **REC**, paused recording, and reconnecting states, enable
OBS's WebSocket server under Tools → WebSocket Server Settings, then create
`$XDG_CONFIG_HOME/ferese/obs.json` (normally `~/.config/ferese/obs.json`):

```json
{"enabled": true, "port": 4455, "password": "your OBS WebSocket password"}
```

Set the file's permissions to `600` and restart the shell. Ferese connects only
to `127.0.0.1`, reads output status, and never starts or stops OBS outputs.
It connects only if this configuration exists and is enabled. If OBS disconnects,
its output status clears. Any portal sharing session stays visible.

Ferese updates capture and OBS status when it receives an event. Elapsed time
uses a local one-second timer while a running output's indicator can be shown.
Sharing and paused-only recording need no timer. If configured OBS is unavailable,
connection retries back off to once per minute.

### Choose a source

Select a Wayland screen-sharing source in your application. In Ferese's picker,
choose a display, workspace, or window and click **Share**. Nothing is selected initially.
Apps requesting multiple sources can select up to eight.

A shared display includes everything visible on it, including notifications.
Window sharing captures only the selected app's content and subsurfaces, even if
the window is covered or on another workspace. It excludes separate popups and
compositor decorations. Window streams preserve backing pixel density across
the captured surface tree when it exceeds the display scale. For example, a 2×
application buffer stays at 2× in the stream on a 1.75× display, avoiding a
fractional downsample before the recording app scales it. The video size follows
window resizes and backing buffer density changes; closing the window ends sharing.
At fractional scales, clients can round the two backing dimensions separately.
Capture preserves those pixel dimensions rather than adding a row or column
by applying the larger axis density to both axes, which would soften text.

A shared workspace stays attached to its identity when you switch elsewhere. It
includes that workspace's windows, their popups and decorations, and the wallpaper.
Panels, launcher and notifications are excluded by default. Turn on **Include
shell** in the sharing picker to include them while that workspace is visible.
Switching away or opening overview hides the shell from the stream, while the
selected workspace's windows keep sharing. The option needs approval each time
and is available only with a compositor that supports it.

Overview and windows protected by capture rules stay excluded. Authentication
windows are protected by default.
Scrolling shares the workspace viewport, not the entire offscreen canvas. Capture
does not activate the workspace, move focus, or resize hidden applications.

Choose **Workspace quality** when sharing a workspace: **Native** (the default)
keeps the display's physical pixel dimensions, **1080p** fits within 1920 × 1080,
and **1440p** fits within 2560 × 1440. Portrait displays swap those bounds.
All choices preserve aspect ratio and avoid upscaling. For a 2880 × 1800 display,
the three choices produce 1728 × 1080, 2304 × 1440, and 2880 × 1800 respectively.
Use Native for detailed text and match OBS's source/output resolution to avoid
another downscale. Higher quality uses more memory and bandwidth and remains
subject to the capture resource limits below; an unsupported size fails rather
than silently reducing native quality. Older compositors offer 1080p only.
The canvas and quality are fixed for the session. Changing the
source output's scale, transform, geometry, or workspace ownership ends sharing;
select the workspace again. Removing the workspace or locking also ends sharing.
Window blur and desktop transition effects are omitted from the workspace view.

Workspaces appear when an application requests display sharing. The portal
exposes them as restricted views of an existing monitor using the standard
`MONITOR` source type; it does not advertise a new workspace type or create a
virtual display. Applications requesting only windows still see only windows.
The panel recorder continues to select displays.

The picker follows your shell colors, font, corners, opacity, and blur.

## Stop sharing

Click **Stop sharing** or close the sharing window. For the built-in recorder,
use the panel's stop button, which Morph also shows while recording. Sharing
also ends when the app closes or Ferese locks. If you disconnect a shared monitor
or change its capture size, its stream ends and you need to start sharing again.

## Remember permissions

When an app requests saved display permissions, the picker offers **Allow without
asking**, off by default. Approval lasts for the duration requested by the app.
The portal manages stored permissions and revocation.

A different app, changed display, or changed cursor option needs approval again.
Nested previews and displays without a reliable identity always ask. Window and workspace
permissions are not saved. Stopping a stream does not erase a saved permission.

## Requirements and limits

Install PipeWire, WirePlumber, `xdg-desktop-portal`, and `xdg-desktop-portal-gtk`.
The built-in recorder also needs GStreamer's PipeWire, base, and good plugins.
Region selection needs `slurp`; audio recording needs PipeWire-Pulse or PulseAudio
and the GStreamer PulseAudio and Opus plugins. With audio set to Off, the recorder
does not open an audio device. For distribution package names,
see the [package lists](installation.md#requirements).

The installer supplies Ferese's portal backend and selects it for screen sharing.
Log into a new Ferese session after installation.

Streams negotiate their frame rate with the recording app, with a default of
30 fps and a ceiling of 60 fps. To set a different ceiling, give the portal
process `FERESE_CAPTURE_MAX_FPS` with an integer from 1 to 240. This controls
capture independently of the display refresh rate; for workspace capture, set
the same ceiling in the compositor environment. It does not make an idle
desktop generate frames.

On supported GPUs, Ferese renders directly into DMA-BUF buffers shared with
PipeWire and waits for GPU completion before handing them to the recording app.
Capture negotiates renderer-supported RGB8 buffer modifiers with the recording
app, probing their allocation and plane layout before offering them. Each
plane keeps its own file descriptor, offset and stride. Linear buffers remain
available when supported by the renderer and allocator. Modifiers affect memory
layout and interoperability; choosing a tiled buffer does not add image detail.
If GPU buffers are unavailable, capture uses shared memory without the portal's
intermediate frame copies. Both paths wait for source damage, while a resumed
consumer receives an initial image even if the desktop has stayed unchanged.
For troubleshooting, `FERESE_CAPTURE_FORCE_SHM=1` selects shared memory; large
or rapidly changing captures on this path still incur GPU readback costs.

## Workspace capture resources

Workspace sources reuse the existing image-copy-capture and PipeWire transport.
Each combination of workspace, quality, cursor and shell options has its own retained GPU
image with independent damage history and render resources. At most one changed
workspace is composed per event-loop dispatch, yielding to input and display
deadlines between views. Matching consumers reuse its composition and keep
separate output buffers. Window commits invalidate only their owning workspace.
Views that include the visible shell also track output changes; layout samples
are reused until geometry changes. Consumer demand alone does not invalidate the
image.

Hidden applications receive paced frame callbacks only while their content
contributes to an active capture. An unchanged source waits for damage. A paused
consumer stops capture work without prematurely returning a buffer still owned by
the compositor; its retained capture target is released after two idle seconds.
Closing the last session releases its view immediately on event-loop cleanup.
No workspace capture targets or recurring capture timers exist without sessions.

Persistent capture admission reserves up to 512 MiB of pixel storage across all
sessions, accounting conservatively for eight consumer buffers, a composition
target, a fallback transfer texture, and stride padding. Matching views can use
less than the reservation. This is not a cap on total process RSS, driver
bookkeeping, wallpaper storage, or the recording application's encoder. The
existing per-image and session-count limits also apply. GPU fences remain
asynchronous; slow consumers do not accumulate a queue of rendered frames.

## Troubleshooting

- Confirm PipeWire and WirePlumber are running.
- Confirm `XDG_CURRENT_DESKTOP` includes `Ferese`.
- Check that `FERESE_ENABLE_SCREENCOPY` was not set to `0` before session startup.
- Check for portal overrides in `~/.config/xdg-desktop-portal/`.

The installed preference is `/usr/share/xdg-desktop-portal/ferese-portals.conf`.
Its ScreenCast entry should select Ferese:

```ini
[preferred]
default=gtk;
org.freedesktop.impl.portal.ScreenCast=ferese;
```

`xdg-desktop-portal-ferese --sources` lists displays on the current
`WAYLAND_DISPLAY` without starting a stream. Nested previews do not replace the
host desktop's portal environment. See [Development](development.md#portal-checks)
for isolated sharing tests.
