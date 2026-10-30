# Text-only bar

Enable **Settings → Panel → Text-only bar**, or set the following on the existing panel:

```kdl
panel main {
    style "flat"
    presentation "text"
}
```

The presentation uses a 24px opaque bar, 28px numbered workspace cells, the active shell font at 13px, square corners and a one-pixel bottom rule. The bar background, primary and muted text, accent and bottom rule follow the shared resolved theme, including live changes between families in the same appearance mode; no theme-mode label is shown on the bar. Urgency retains its semantic red, and text on filled cells uses a readable foreground. Insufficient contrast is corrected gradually from the theme’s chosen color, preserving its tint instead of jumping directly to black or white. The bar remains opaque even when the theme uses translucent materials. The bar does not request compositor material effects, blur or shadows. Its text controls open the existing sound, battery, Control Center and calendar popups; `settings` opens Control Center.

Workspace cells follow the monitor’s dynamic workspace list, including its real spare workspace. No unused numbered placeholders are drawn. Cells display the current workspace indices and activate their stable IDs; they appear and disappear as workspaces are created or removed. `scroll` and `[]=` reflect the active workspace's scrolling and tree layouts; an older compositor without layout metadata displays `?`.

Occupied cells have a corner marker. Urgent flags received through the existing shell protocol produce red cells; this presentation does not change the compositor's application activation or attention-request policy. Muted sound reads `vol muted`; unavailable sound and battery services are omitted. Active sharing, OBS outputs and Ferese recording appear as text controls, independent of the saved standard panel items.

CPU usage, Wi-Fi signal, network transfer rates and root filesystem usage are shown as text. Wi-Fi opens the connection controls; CPU, transfer rates and disk are plain, non-interactive readings. Readings that do not fit are omitted from the bar. CPU and network counters are sampled off the UI thread every two seconds only while a text bar is visible. Disk usage is refreshed every thirty seconds and uses the same reserved-space convention as `df`. Transfer rates are bytes per second for the lowest-metric IPv4 default-route interface, falling back to IPv6 when needed. They measure that interface’s traffic, not a bandwidth test or a sum across interfaces; policy routing and split-tunnel traffic may use other devices. Device changes or reset counters discard the previous baseline. Missing readings are omitted.

On narrow outputs, the title yields space first, the date becomes time-only, then optional status labels disappear. Control Center still provides the existing sound, battery and appearance controls. At smaller widths, a `workspaces` menu replaces the full strip and lists this monitor's real workspaces. On exceptionally narrow surfaces, the remaining controls can scroll horizontally with no visible scrollbar. No pictorial icons are introduced into the bar.

Turning the presentation off restores the standard Flat panel with its saved items and geometry. Choosing another shape preset also exits text mode. Existing groups and geometry pins are preserved while text mode is enabled; the text presentation keeps its 24px height and zero inner padding, and honors the saved window clearance. Adjust **Window clearance** in Settings → Panel to change the gap between the bar and application windows. The reserved area is 24px plus that clearance, and fullscreen releases the entire reservation. Popup content retains Ferese's existing UI.

## Verification

The shell tests cover output ownership, dynamic workspace removal and sparse indices, muted/missing services, binding modes, pointer clicks across redraws, workspace renumbering during a held press, 24px/28px geometry, opaque rendering and dark/light palettes. Render checks include narrow widths and fractional scaling. Settings tests cover enable/disable, stored composition and geometry, and shape changes.

The opt-in nested test requires a Wayland host, D-Bus and Python Pillow. It checks versioned layout updates, workspace count, appearance changes, native material suppression and switching back and forth between standard and text presentations:

```sh
FERESE_TEST_TEXT_BAR=1 python3 scripts/tests/test_text_bar_isolated.py
```

Use `FERESE_TEST_BINARY`, `FERESE_TEST_SHELL` and `FERESE_TEST_CTL` for custom executable paths. `FERESE_TEST_TEXT_BAR_PREVIEWS` saves screenshots, and `FERESE_TEST_LOG` saves the nested protocol log. The test uses a private configuration, runtime directory and session bus.
