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

For manual panel configuration, add an `activities` item to an existing group.
This complete panel example places it at the end of a floating panel; don't add
a second `panel` block if your config already has one:

```kdl
panel "main" {
    style "floating"
    end {
        group "tasks" {
            item "activities" kind="activities" priority=20
        }
    }
}
```

### Manual shell setup

Without installer setup, hooks and process watching are off by default. Add an
Activities item to the panel and enable `activities.sources.shell-hook` before
loading a hook. From a checkout, source `contrib/activities/ferese.fish` in Fish,
`contrib/activities/ferese.zsh` in Zsh, or the following in Bash:

```sh
source contrib/activities/bash-preexec/bash-preexec.sh
source contrib/activities/ferese.bash
```

Use absolute paths when adding these lines to shell startup files. Zsh uses
`add-zsh-hook`, Fish uses named pre/postexec events, and Bash composes with
[bash-preexec](https://github.com/rcaloras/bash-preexec)'s hook arrays. Load
bash-preexec before the Bash hook; the hook does not replace an existing DEBUG
trap. Hook files never execute captured command text and preserve command status.

The installer copies hooks into `$XDG_CONFIG_HOME/ferese/activities`, defaulting
to `~/.config/ferese/activities`. Fish loads through its `conf.d` directory; Bash
and Zsh get a marked block in `.bashrc` or `.zshrc`. Zsh setup honors an exported
`ZDOTDIR`. Bash shells started in login mode must also load `.bashrc` from their
login profile for the installed hook to run.

Process watching is an opt-in alternative when hooks are unavailable. It performs
bounded sampling on one worker, and disappearance means expiry, never inferred
success or failure. Shell hooks observe interactive commands; they do not
automatically wrap them or parse their output. The
[Cargo adapter](#cargo-progress) requires `feresectl run`.

## Run a command explicitly

Use `feresectl run` for commands outside the built-in rules, unsupported shells,
or scripts where an interactive hook doesn't run:

```sh
feresectl run --title "Backup" -- ./backup.sh
feresectl run --title "Tests" -- npm test
```

Arguments after `--` are passed to the command. The command keeps its terminal
input, output, working directory, environment, terminal process group and
exit/signal status, and still runs if activity publishing fails.

Completion follows the command's exit. A background descendant holding stdout
open does not keep the wrapper alive indefinitely.

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
`cargo build` automatically. A `build-finished` message never completes the
activity; completion follows the actual child exit. The adapter also preserves
non-JSON output. See the
[Cargo JSON contract](https://doc.rust-lang.org/cargo/reference/external-tools.html).

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

A visible task's result stays for the configured [result display time](configuration/activities.md#settings).
Do not disturb limits ordinary running activities to one, hides the badge and
suppresses result flashes. When there are no eligible tasks, the regular Activities item has no
content and Morph returns to its other content.

## Configure Activities

[Configure Activities](configuration/activities.md).

### Turn off automatic command detection

[Turn off automatic command detection](configuration/activities.md#turn-off-automatic-command-detection).

### Command rules

[Command rules](configuration/activities.md#command-rules).

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
See [Publisher ownership and lifecycle](development/activities.md#publisher-ownership-and-lifecycle)
for the Linux requirement, ownership checks, limits and expiry behavior.

## Publish your own status

See [Publishing activities](development/activities.md#publish-your-own-status) for the CLI examples.

## Publisher ownership and lifecycle

See [Ownership and lifecycle](development/activities.md#publisher-ownership-and-lifecycle).

## Extensions and templates

See [Templates and the IPC schema](development/activities.md#extensions-and-templates).

## Pomodoro

Start a work session with:

```sh
feresectl pomodoro start
```

The default session has four 25-minute work intervals, with 5-minute short
breaks and a 15-minute long break after the fourth work interval. It finishes
at the end of the long break. Each completed interval sends a silent
notification, and the next phase waits for you to start it unless you've
enabled automatic advancement in [Pomodoro settings](configuration/activities.md#pomodoro).

Open the activity list to pause, resume, skip, reset or stop the timer. In Morph,
the controls are in the Activities tab; other panel styles use the Activities
popup. You can reach the buttons with Tab and activate them with Enter.
The paused timer stays visible with its remaining time frozen.

The same controls are available from a terminal or a configured keybinding:

```sh
feresectl pomodoro pause
feresectl pomodoro resume
feresectl pomodoro next
feresectl pomodoro status
feresectl pomodoro reset
feresectl pomodoro stop
```

`next` skips a running or paused interval without counting it as completed work.
When the next phase is already ready, `next` starts it. `reset` cancels the
session and starts again with your current settings, while `stop` removes it.
Only one session can run at a time, and it survives closing the terminal that
started it. See the settings reference for suspend and restart behavior.
