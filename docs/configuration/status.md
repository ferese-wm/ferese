# Status controls

| `status` key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `keybinding-guide` | boolean | `true` | Show the active shortcut guide at login until disabled in Settings → Keyboard & mouse → Shortcuts. |
| `low-battery-threshold` | integer 0–100 | `20` | Warning-color threshold |
| `settings-command` | argument array | `["ferese-settings"]` | Settings launcher; `[]` hides the action |

Configure the panel in **Panel** or its KDL block, as described in the
[panel guide](../panel-configuration.md). The panel's appearance applies to every
group, while popup and notification backgrounds follow the shared shell theme.
Transparent gaps between islands pass clicks through to the windows beneath.

## Now Playing

The Media item can show artwork, the track title and a play/pause button wherever
you place it in the panel. Click the artwork or title to open playback controls
for MPRIS players; Morph presents playback in its Music view.
Scroll over the control to change the player's volume, or
middle-click to raise its window when supported. The menu has previous/next
buttons, a seek slider when supported, and a player chooser. It uses the same
colors, transparency and animations as the other bar menus.

Automatic selection prefers the player that most recently started playing.
When none is playing, it selects the most recently active paused player. Players
already running at startup have no known activity order; ties use their bus names.
Stopped players hide the control. Pin a player through the chooser to keep it
selected, or ignore it to exclude it. These choices last until that player leaves
the bus or the media service reconnects. `Choose automatically` clears the pin.

The bar and media keys share one MPRIS monitor. Playback updates come from D-Bus
signals. Progress updates only while the menu is open and playback is running:
once a second, or every three seconds on battery. Album art loads while the
bar or menu is visible, on a worker with download, decode and cache limits. A placeholder
keeps its space while loading or when art is unavailable.

The default Play/Pause, Next and Previous keys use `media-play-pause`,
`media-next` and `media-previous`. These actions take no arguments. Custom
bindings can use them too:

```kdl
binding "Super+P" "media-play-pause"
```

Inspect players or control the selected one from the terminal:

```sh
feresectl media
feresectl media play-pause
feresectl media next
feresectl media previous
feresectl media pin org.mpris.MediaPlayer2.example
feresectl media ignore org.mpris.MediaPlayer2.example
feresectl media unignore org.mpris.MediaPlayer2.example
feresectl media auto
```

`unignore` restores an ignored player even when the bar control is hidden. Apps
without MPRIS support do not appear here.
