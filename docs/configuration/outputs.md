# Displays

## Color temperature

Night light warms the display using its hardware gamma ramp. It is off by default.
Set its state and temperature in `config.kdl`:

```kdl
night-light {
    enabled #false
    temperature 4500
}
```

The temperature range is 1000–6500 K. Lower values are warmer. At 6500 K, the
display keeps its neutral color.
The panel's **Night light** item opens an on/off switch and temperature slider.
In Morph, the button stays beside the tabs when you expand the panel, even if
you remove the Night light item from your panel layout. Quick Settings has the
same controls. The toggle saves immediately. Releasing the slider saves its
temperature in the config.

Night light applies to all supported connected displays, including mirror targets.
It requires a DRM session. A nested session cannot change the host display's colors.
There is no automatic schedule yet.

### External gamma-control clients

Ferese supports `wlr-gamma-control-unstable-v1` on DRM outputs with hardware
gamma ramps. A gamma-control client can adjust the display's color temperature.
Only one client can control each output at a time. While that client remains
connected, Ferese reapplies its gamma table after display sleep or a session
switch. Closing the client restores the previous table.

Nested outputs and hardware without gamma-ramp support report that gamma control
is unavailable. This protocol does not provide ICC profiles or HDR color management.

When you turn on night light, Ferese takes control of the gamma tables and sends
external controllers `failed`. These clients must request gamma control again
after you turn it off. New requests are rejected while night light is on.

## Output profiles

Each `output-profile` needs a unique, nonempty `name` and at least one `output`
entry. A profile matches when every required monitor is connected and its
optional `lid-closed` condition matches. The profile with the most required
monitors wins; ties use config order. If none matches, Ferese extends across all
usable connected monitors. Find selectors with `feresectl outputs`.

| `output-profile` key | Type | Default |
| --- | --- | --- |
| `name` | unique nonempty string | required |
| `layout` | `internal-only`, `external-only`, `extend`, `mirror` | `extend` |
| `lid-closed` | boolean | either lid state |
| `mirror-source` | selector of an enabled output in this profile | first enabled hardware key |
| `lid-policy` | `dock-or-suspend`, `ignore` | `dock-or-suspend` |
| `confirm-timeout` | seconds; `0` disables confirmation | `15` |

| `output-profile → output` key | Type | Default |
| --- | --- | --- |
| `match` | nonempty connector or persistent identity string | required; unique within profile |
| `required` | boolean | `true`; set `false` for an optional monitor |
| `enabled` | boolean | `true` |
| `mode` | `"WIDTHxHEIGHT"` or `"WIDTHxHEIGHT@HZ"` | Preferred mode |
| `auto-refresh` | boolean | `false` |
| `adaptive-sync` | boolean; requires connector and atomic KMS support | `false` |
| `allow-tearing` | boolean; permits supported clients to request asynchronous flips | `false` |
| `scale` | number > 0 | `1` |
| `transform` | enum below | `"normal"` |
| `position` | `[integer, integer]` | Automatic horizontal placement |

Transforms: `normal`, `rotate-90`, `rotate-180`, `rotate-270`, `flipped`,
`flipped-90`, `flipped-180`, `flipped-270`. In extend and mirror profiles,
unlisted monitors stay enabled with defaults. Internal-only and external-only
filter by connector type. A layout that would disable the final usable display
falls back to a working display. Profiles that explicitly disable all their
listed outputs are invalid.

`adaptive-sync=#true` explicitly enables variable refresh on a supported output.
The request is validated and restored with the rest of the output transaction.
Mirror sources and targets reject adaptive sync because their presentation clocks
are coupled. IPC exposes support, requested state, and applied state separately;
an unsupported request fails without silently enabling a different policy.

`allow-tearing=#true` permits asynchronous flips when the atomic DRM driver advertises
support and a fullscreen client commits an asynchronous presentation hint.
Ferese requires a settled, unlocked fullscreen surface using primary direct
scanout. Mirroring, pending capture, variable refresh, and FIFO-constrained
updates keep synchronized presentation. Unsupported asynchronous driver requests
retry ordinary presentation. IPC reports `tearing_supported` and `allow_tearing`;
permission alone does not imply every frame tears.

`required` controls whether a profile matches when a monitor is absent. Every
connected monitor enabled by the resolved policy, including optional monitors
and mirror targets, needs its own compatible CRTC assignment. Ferese rejects an
incomplete assignment before applying the configuration. A complete assignment
still needs to pass driver validation; connector compatibility alone does not
establish that the hardware accepts the modes and planes.

Monitor identities use EDID manufacturer, product/model and serial, rather than
a hash of the entire EDID. Without a usable serial, the identity includes the
GPU's sysfs hardware path and connector. For duplicate serials, Ferese adds the
same path and connector to distinguish the monitors. If both arrive together,
both identities include those details. If the second arrives later, the first
keeps its identity and only the second gets the path and connector. Ferese
remembers that choice for the compositor session. Without serials, Ferese
identifies monitors by port and cannot tell them apart if you swap their ports.
Connector names and old `drm-edid:` selectors still work. Ambiguous or
overlapping selectors are rejected during live reload. A disconnected identity
can remain in config; it prevents matching only when its entry has
`required=#true`.

```kdl
output-profile "desk" layout="external-only" {
    output "DP-1" mode="2560x1440@60" scale=1.25
    output "eDP-1" required=#false enabled=#false
}

output-profile "mobile" layout="internal-only" {
    output "eDP-1" scale=1.75
}

output-profile "presentation" layout="mirror" mirror-source="eDP-1" {
    output "eDP-1" scale=1.75
    output "HDMI-A-1" mode="1920x1080@60"
}
```

Mirror uses the source's logical size, workspace, layers and pointer. Targets
own no workspaces or separate pointer region. Ferese fits the complete source
into each target's transformed physical mode, preserving aspect ratio and
letterboxing instead of cropping. Target scale does not resize the source
layout. Each target has its own hardware cadence; only the source releases
client frame callbacks and presentation feedback. Mirrored monitors are listed
by `outputs`, but expose one logical output to clients and the shell. If a required mirror target disconnects, Ferese selects another matching
profile or extends across all usable connected monitors. Leaving mirror
restores evacuated workspaces.

```sh
feresectl outputs
feresectl output-profiles
feresectl toggle-display-mode
feresectl output-layout extend
feresectl output-profile presentation
feresectl output-confirm
feresectl output-profile auto
feresectl output-internal off
feresectl output-revert
```

The display Fn key (`XF86Display`) and the bar’s display icon open the same
display-mode chooser. The icon appears while an external monitor is connected.
`output-layout` selects a layout without requiring a named profile and keeps
the matching profile’s mode, scale and transform settings.

A manual choice overrides automatic selection until the monitor set or lid
state changes. Reload keeps it only if its profile still exists and matches.
Manual changes revert after `confirm-timeout` seconds unless confirmed;
`output-revert` restores immediately. Reverting restores the last confirmed modes,
scales, transforms and positions, even if you make several changes before
confirming. The timeout still applies if the client that made the change exits. Automatic hotplug/lid changes never wait for confirmation.
Disabling the last usable internal panel is rejected.

```kdl
output-profile name="docked" {
    output match="HDMI-A-1" scale=1.5 {
        position 0 0
    }
}
```

Settings → Displays offers the connected display's supported refresh rates at
its configured resolution. Choosing a rate saves it and disables Auto. Choosing
Auto saves the highest supported rate as the normal mode and enables automatic
switching. These choices apply immediately for the active profile and persist
across restarts.

With `auto-refresh=#true`, a discharging system battery below 30% switches the
display to a supported mode within 1 Hz of 60 Hz at the same resolution. The
configured normal mode returns on external power or when the battery reaches
35%. Power is checked every five seconds; mode changes only run when the policy
changes. Unavailable battery data restores normal refresh. If no matching 60 Hz
mode exists, normal refresh is retained. Rejected DRM mode changes retain the
current output. A refresh-rate change can briefly blank the display (about one
second on some panels), including automatic changes below 30%, recovery at 35%,
and restoration on AC. Session activation refreshes lid and connector state
before rendering; unchanged outputs retain their handles and workspaces.
Hotplug bursts settle for 150 ms before one reconciliation. Ferese records changes that arrive while the session is inactive and applies
them on activation.

```kdl
output-profile laptop {
    output eDP-1 mode="2880x1800@120" scale=1.75 auto-refresh=#true
}
```

With a usable external display, closing the lid disables internal panels after
external activation succeeds. Without one, Ferese asks logind to suspend,
respecting sleep inhibitors. `lid-policy="ignore"` disables this behavior for
the selected profile. Resume reads lid state from logind, with ACPI fallback,
before reconciling fresh hardware. Ferese releases its logind lid inhibitor on
session pause, so logind handles the lid while another session or TTY is active.
Workspaces keep their home output and return when it reconnects, unless you
reassigned or deleted them after evacuation. Floating
windows are remapped and clamped to the surviving display.

On atomic DRM, Ferese tests the complete configuration for each GPU before
changing it.
Smithay performs the real commits; logical changes follow successful hardware
application. Failed commits attempt restoration, and outputs that cannot be
restored are removed from logical state. Each GPU commits separately. If a later GPU fails, Ferese attempts to restore
the earlier GPUs. Legacy KMS lacks atomic
test-only validation. Hardware failures can still leave the session without a working monitor; Ferese tries preferred modes to recover a display instead of
intentionally turning off the last working one.
