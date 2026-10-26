# Optional session protocols

These are launch-time environment switches, not KDL keys. Enable only for
trusted clients: `FERESE_ENABLE_INPUT_METHOD=1`,
`FERESE_ENABLE_SHORTCUT_INHIBIT=1`, `FERESE_ENABLE_SCREENCOPY=1`.
The installed session launcher enables screencopy for screenshots unless
`FERESE_ENABLE_SCREENCOPY=0` is explicitly set in its environment. Setting it
to `0` disables every path that can read the screen, including the built-in
screenshot command, not just the Wayland protocol. A session launched by hand
rather than through `ferese-session` needs `FERESE_ENABLE_SCREENCOPY=1` in its
environment for screenshots to work.
Text input is available by default; Ctrl+Alt+Escape releases an active shortcut
inhibitor. Use `feresectl --help` for runtime control commands.

Shell rounding is controlled in Settings → Appearance → Shell corner radius.
The shell and compositor use the same squircle profile for fills, borders, shadows
and blur masks. Small controls cap the radius to fit their size; circles and pills
keep circular outlines. The measured profile has small tangent and curvature
discontinuities at its internal segment joins. Window rounding remains under
Settings → Appearance → Windows. Shell rounding comes from `theme.geometry.shell-radius`,
which defaults to 14 px. Panel corners follow their selected style and height,
with optional overrides in the [panel configuration](../panel-configuration.md#background-outline-and-corners).
