# Screen sharing and recording

Share a monitor or window from a Wayland-compatible recording or conferencing app,
or save a video using the top bar. [Installation](installation.md) includes the
portal and PipeWire dependencies.

## Record from the top bar

1. Click the recording icon.
2. Choose a display and an audio source: **Off**, **System sound**, **Microphone**,
   or **System + microphone**. Audio is off by default; the other choices use
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

Select a Wayland screen-sharing source in your application. In Ferese's picker,
choose a display or window and click **Share**. Nothing is selected initially.
Apps requesting multiple sources can select up to eight.

A shared display includes everything visible on it, including notifications.
Window sharing captures only the selected app's content and subsurfaces, even if
the window is covered or on another workspace. It excludes separate popups and
compositor decorations. The video size follows window resizes; closing the window
ends sharing.

The picker follows your shell colors, font, corners, opacity, and blur.

## Stop sharing

Click **Stop sharing** or close the sharing window. For the built-in recorder,
use the stop button in the top bar. Sharing also ends when the app closes or
Ferese locks. If you disconnect a shared monitor or change its capture size,
its stream ends and you need to start sharing again.

## Remember permissions

When an app requests saved display permissions, the picker offers **Allow without
asking**, off by default. Approval lasts for the duration requested by the app.
The portal manages stored permissions and revocation.

A different app, changed display, or changed cursor option needs approval again.
Nested previews and displays without a reliable identity always ask. Window
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

Streams run at up to 30 fps with hidden or embedded cursors. Frames are copied
through CPU-accessible buffers. Large streams or several simultaneous streams
can therefore use substantial CPU and memory bandwidth.

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
