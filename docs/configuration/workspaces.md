# Workspaces

Each monitor has an independent list of workspaces, numbered from 1. Its bar,
overview strip, and numeric shortcuts use that same order. Numbers change when
empty workspaces are removed; internal workspace IDs stay stable.

One empty workspace is always available at the end of each monitor's list.
Opening or moving a window there creates another spare. An empty workspace stays
while displayed or participating in a swipe, then disappears after you leave it.
Selecting a number beyond the current list selects the trailing empty workspace.
Window-rule workspace numbers also refer to positions on the window's monitor.

`Super+Escape` returns to the previous surviving workspace on the focused monitor.
History does not keep abandoned empty workspaces alive. With no previous workspace,
the action does nothing. Disconnecting a monitor clears its history; occupied
workspaces move to another monitor and can return when it reconnects.

Enable **Settings → Windows → Toggle back with the same workspace shortcut**, or:

```kdl
workspaces {
    auto-back-and-forth #true
}
```

This defaults to `#false`. When enabled, pressing a `workspace` binding for the
current workspace returns to the previous one. Explicit `feresectl workspace N`
commands and overview selection still select the requested workspace directly.
Use `feresectl workspace-back-and-forth` for an explicit toggle, or bind
`workspace-back-and-forth` to another key or gesture. The action takes no argument.

`Super+BackSpace` focuses the last focused window, including windows on other
workspaces or monitors. Repeating it toggles between the two most recently
focused windows. Layer surfaces such as launchers do not replace this history.

`Alt+Tab` previews windows in focus order using overview. Keep Alt held and press
Tab to advance, or Shift+Tab to go backward. Release Alt to focus the selection;
Escape cancels without changing focus. The order stays fixed until selection is
committed. Closed windows are skipped and newly opened windows join the next
cycle. Preview transitions follow the configured animation speed and reduced
motion settings.

Bind `focus-last-window`, `focus-mru-next`, or `focus-mru-previous` to customize
these shortcuts. MRU bindings with Alt held preview until Alt is released;
bindings without Alt, gestures, and the corresponding `feresectl` commands
switch immediately. All three actions take no arguments.
