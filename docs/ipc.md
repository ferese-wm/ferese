# feresectl and IPC events

`feresectl` controls a running Ferese session. Run `feresectl --help` for commands
and `feresectl COMMAND --help` for arguments.

## Queries

Queries print plain text by default. Add `-j` or `--json` when passing results to
another program. The flag works before or after a subcommand.

```sh
feresectl outputs
feresectl windows -j
feresectl -j workspaces
feresectl focused-window
feresectl idle-inhibition
feresectl keybindings
feresectl session-state
feresectl output-profiles
feresectl theme get
feresectl xwayland status
feresectl media
```

The `get-*` spellings remain hidden aliases for the corresponding queries and
print a deprecation warning on stderr. They use the same output rules: scripts
that previously parsed their JSON must add `-j`. New scripts should use the
canonical names. The aliases will be removed in a later release.

Screenshots still write PNG bytes to stdout, including when `-j` is present:

```sh
feresectl screenshot > desktop.png
feresectl screenshot -g '0,0 800x600' > region.png
feresectl screenshot-window 7 > window.png
```

## Choose a session

Every feresectl IPC command, including theme commands and subscriptions, resolves
its target in this order:

1. `--socket PATH`
2. A nonempty `FERESE_SOCKET`
3. `$XDG_RUNTIME_DIR/ferese/control.sock`

```sh
feresectl --socket "$XDG_RUNTIME_DIR/ferese/instances/12345-abcdef/control.sock" outputs
FERESE_SOCKET="$XDG_RUNTIME_DIR/ferese/instances/12345-abcdef/control.sock" feresectl theme get -j
```

`--socket` selects an existing server; it does not create one or change where the
compositor listens. DRM uses `$XDG_RUNTIME_DIR/ferese/control.sock`. Nested
instances use `$XDG_RUNTIME_DIR/ferese/instances/<id>/control.sock`; copy the
actual path from the compositor's startup log. Each instance removes its own
socket and directory on normal shutdown. A forced kill can leave the directory
behind; later instances allocate a different one.

Clients launched by a nested compositor inherit its `FERESE_SOCKET`, so
`feresectl outputs` inside that session connects to that instance.
`autostart` launches desktop entries locally and does not use IPC.

## Follow changes

```sh
feresectl event-stream
feresectl --socket "$XDG_RUNTIME_DIR/ferese/instances/12345-abcdef/control.sock" event-stream
```

`event-stream` writes one JSON value per line and flushes each line. It always
uses JSON, so `-j` is optional. `theme subscribe` also keeps its newline-delimited
JSON output.

The first event is a `snapshot` containing outputs, workspaces, windows, focus,
config status, the logical theme, and lock state. Later events contain replacement
values for changed domains:

| Type | Payload field |
| --- | --- |
| `outputs_changed` | `outputs` |
| `workspaces_changed` | `workspaces` |
| `windows_changed` | `windows` |
| `focus_changed` | `focus` |
| `config_changed` | `config` |
| `theme_changed` | `theme` |
| `lock_changed` | `lock` |

For example:

```json
{"version":1,"generation":12,"last":true,"type":"focus_changed","focus":{"window":7,"output":1,"workspace":3}}
```

Event schema version 1 is defined in `ferese-ipc::events`, independently of the
command protocol version. A subscriber requests that version using the framed
`event-stream` command. The server sends a normal response acknowledging the
subscription, followed by framed events on the same connection. That connection
is then reserved for events.

Changes are sampled after an event-loop dispatch. Output reconciliation must
finish before a desktop generation is published. Events from the same dispatch
share a `generation`; only its final event has `last: true`. Consumers maintaining
a desktop model should apply the whole generation together. Intermediate changes
within a dispatch are coalesced, so this stream is not an audit log of every input
or mutation. Generation numbers may have gaps.

Window events describe identity, title, activation, workspace, floating state,
and fullscreen state. They omit visual geometry, animation frames, and client
damage. Theme events omit the presented crossfade state and its frame revisions.
Config revisions advance when a changed configuration is accepted; rejected
reloads leave that revision intact and report an error. Lock events distinguish
`unlocked`, `acquiring`, `locked`, and `orphaned`, and include display sleep state.
While locked, snapshots and replacement events redact windows, workspaces,
focus, and output workspace/focus fields. Unlocking restores those domains.

## Delivery limits

The compositor allows at most 16 event subscribers within its existing limit of
64 IPC connection workers. Each subscriber has a queue of at most 32 events.
Publishing uses non-blocking sends. A full or disconnected queue removes that
subscriber; socket writes happen on its worker with a two-second write timeout.
Idle disconnected streams are checked every 250 milliseconds.

Events are not replayed. An overflow can end a stream partway through a generation.
Discard any generation without `last: true`, reconnect, and replace local state
with the new initial snapshot. Socket authentication and the existing one-MiB
frame limit also apply to event streams.

When there are no subscribers, event publication does not build desktop
snapshots. With subscribers, it compares logical domain snapshots after each
dispatch. Ordinary damage, cursor movement, and redraw code do not publish events
directly.
