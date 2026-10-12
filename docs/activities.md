# Live Activities

Live Activities shows running commands and other tasks in your panel. A build
can stay visible while you work in another window; click its activity to return
to the terminal when Ferese can identify that window. The panel supplies the
appearance, so tasks use your theme and animation settings.

## Start using it

After installing with `./scripts/install.sh`, open a new terminal and run a
command as usual:

```sh
cargo build
```

The installer detects Fish, Bash or Zsh, installs its shell hook, enables shell
activities and adds an Activities item to your panel. A Cargo build appears
once it has run for **1.5 seconds**. A cached build that finishes sooner stays
hidden, including its completion message.

These command rules are included:

| Command | Delay before appearing |
| --- | --- |
| `cargo build`, `check`, `test`, `run`, `clippy` | 1.5 seconds |
| `npm` or `pnpm` followed by `install`, `build`, `test`, `run` | 1.5 seconds |
| `make` | 300 ms |
| `git push` | 300 ms |
| `docker build` | 1.5 seconds |

Rules match the command text at the start of the line. Commands such as
`cargo +nightly build`, `npm ci`, or `env RUSTFLAGS=... cargo build` need a custom
rule or an explicit wrapper. Commands that don't match a rule are discarded
without being logged.

To check the display without waiting for a build, run:

```sh
feresectl run --title "Activity check" -- sleep 5
```

This publishes directly and doesn't need a shell hook, but the panel still needs
an Activities item. The check appears after 300 ms and finishes after five
seconds.

### Existing installations

From an updated Ferese checkout, rerun user setup as your normal user:

```sh
python3 scripts/installer/install.py user-setup
```

This needs the updated `feresectl` installed. Setup backs up changed config and
shell startup files. It preserves an explicit `shell-hook #false` setting and
an existing Activities item, including one you've hidden. After the first
successful setup, later installs update the hook files without restoring panel
items you've removed or changing your activity settings.

Open a new terminal to load the hook. In an existing Fish terminal, you can
load it immediately with:

```fish
source ~/.config/fish/conf.d/ferese-activities.fish
```

The paths above use the default configuration directory. If you set
`XDG_CONFIG_HOME`, use that directory in place of `~/.config`.

### Add Activities to your panel

Open **Settings → Panel & Shell → Edit panel**, select a group in Arrange, and
choose **Add item → Activities**. Keep its visibility enabled. This is also how
to restore the item after removing it.

Activities can live in Start, Center or End. Regular panels use that position
and open the activity list in a popup. Morph presents tasks inside its existing
island and adds an Activities tab. Changing a group's position doesn't create
another Morph island. See [Panel configuration](panel-configuration.md) for item
placement and overflow controls.

## Run a command explicitly

Use `feresectl run` for commands outside the built-in rules, unsupported shells,
or scripts where an interactive hook doesn't run:

```sh
feresectl run --title "Backup" -- ./backup.sh
feresectl run --title "Tests" -- npm test
```

Arguments after `--` are passed to the command. It keeps its terminal input,
output, working directory and exit status, and still runs if activity publishing
fails. Completion follows the command's exit. A background descendant holding
stdout open does not keep the wrapper alive indefinitely.

### Cargo progress

A normal `cargo build` shows that the command is running and whether it finishes
successfully. It doesn't report a percentage. To show the number of compilation
units completed, use the Cargo adapter:

```sh
feresectl run --cargo-json -- cargo build
```

The adapter reads Cargo's JSON output while keeping diagnostics readable in the
terminal. It reports completed units as text because Cargo doesn't supply a
reliable total for a percentage. Setting `progress "cargo-json"` in a rule enables
this adapter for matching `feresectl run` commands; it does not wrap plain
`cargo build` automatically.

## Use the activity list

Morph shows up to two activities in its collapsed and peek states, with a `+n`
badge for additional tasks. Higher-priority tasks appear first, with temporary
boosts for new tasks and completion results. Hovering holds the current selection
in place while its values continue updating. Recording and low battery take
priority over ordinary activities.

Click the activity strip or open the Activities tab to see the list. Arrow keys
select a row and Enter activates it. Selecting a linked task focuses its terminal
and closes the list. If the terminal has closed, or one terminal process owns
several possible windows, Ferese leaves the list open. It cannot select a
particular tmux pane.

Tasks with reported progress show a percentage inside a ring. Tasks without a
percentage show an animated arc around their icon. Reduced motion makes progress
changes immediate and uses a static track. Activities do not automatically open
the expanded panel.

A visible task's result stays for 1.5 seconds by default. Do not disturb limits
ordinary running activities to one, hides the badge and suppresses result
flashes. When there are no eligible tasks, the regular Activities item has no
content and Morph returns to its other content.

## Configure Activities

In **Settings → Panel & Shell → Appearance**, **Visible activities** chooses one
or two slots in Morph's collapsed and peek states. **Show activity count** controls
the `+n` badge; hiding it still lets you open the full list.

For source and timing settings, edit the top-level `activities` block in
`~/.config/ferese/config.kdl`. Merge these fields into an existing block rather
than adding a second one:

```kdl
activities {
    max-visible 2
    show-badge #true
    start-delay-ms 300
    result-flash-ms 1500
    sources {
        shell-hook #true
        process-watch #false
        notification-bridge #false
    }
}
```

| Setting | Effect |
| --- | --- |
| `max-visible` | One or two compact activity slots. |
| `show-badge` | Show `+n` for tasks beyond those slots. |
| `start-delay-ms` | Minimum delay for a new task; a rule can require a longer delay. |
| `result-flash-ms` | How long an eligible completion result remains visible. |
| `shell-hook` | Accept command start/end events from an installed shell hook. |
| `process-watch` | Opt-in process sampling when hooks aren't available; process disappearance removes the task without inferring success or failure. |
| `notification-bridge` | Present notifications as activities instead of duplicate toasts, respecting Do not disturb and notification popup settings. |

The process watcher and notification bridge stay off unless you enable them.
Media comes from MPRIS; downloads and call statuses need an integration that
publishes them. Microphone or camera use alone does not create a call activity.

Changes reload live. Invalid edits leave the last valid configuration in use;
check an edit with `ferese --check-config ~/.config/ferese/config.kdl`.
For command patterns, custom titles, per-rule delays and completion messages,
see [Command rules](activities-reference.md#command-rules).

### Turn off automatic command detection

Set `shell-hook #false` inside `activities.sources`. Leave `process-watch` off
as well if you don't want process detection. You can still use `feresectl run`
and direct publishers.

To skip shell and Activities setup during installation, pass
`./scripts/install.sh --no-activities`. This skips setup for that installation;
it doesn't remove an existing hook or disable an already configured source.

## If nothing appears

1. **Try the five-second check above.** If it appears, the display and broker
   work; check shell setup and command matching next.
2. **Check the panel item.** In Arrange, confirm that Activities exists and is
   visible. On a regular panel, also check its overflow and available space.
3. **Open a new terminal.** Installing a hook doesn't load it into terminals
   that are already open. Confirm `shell-hook #true` in your config too.
4. **Check the command and duration.** Cargo and npm tasks must last at least
   1.5 seconds under the built-in rules. A custom rule can increase that delay.
   Prefixes and different subcommands may not match the built-in patterns.
5. **Inspect the broker while the task runs:**

   ```sh
   feresectl -j activity list
   ```

   An empty `activities` array means no task is registered in that session.
   A registered task can still be waiting for its delay, ranked below another
   task, or hidden by panel settings. In Morph, also check for recording or a
   low-battery state taking priority.
6. **Check the session and build.** A nested preview has its own broker; use the
   same [session socket](ipc.md#choose-a-session) for publishing and listing.
   If `activity` or `run` is an unknown command, update the installed binaries.
   Log out and back in after upgrading the compositor and shell.

Shell hooks suppress publishing errors so they don't interrupt your commands.
For an ownership or permission error, use a direct publication from the same
terminal to see the response:

```sh
feresectl activity start diagnostic --title "Activity check" --lease-ms 10000
feresectl activity cancel diagnostic
```

The lease removes this diagnostic after ten seconds if you omit the cancel.
Ownership verification needs Linux 6.5 or newer. See the
[publisher reference](activities-reference.md#publisher-ownership-and-lifecycle)
for ownership, limits and expiry behavior.

## Publish your own status

Run these commands from the same shell. The ID `download` belongs to that shell,
so another terminal cannot update it:

```sh
feresectl activity start download --source download --icon download \
    --title "Downloading update" --progress 0.2
sleep 2
feresectl activity update download --progress 0.7
feresectl activity end download --result ok --message "Downloaded"
```

This example publishes status only; it doesn't perform a download. Your program
supplies real progress and completion results. For custom integrations, see the
[Live Activities reference](activities-reference.md), which covers IDs, leases,
fixed view templates and the local IPC protocol.
