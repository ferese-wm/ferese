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

Arguments after `--` are passed to the command. It keeps its terminal input,
output, working directory, environment, terminal process group and exit/signal
status, and still runs if activity publishing fails. Completion follows the command's exit. A background descendant holding
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
see [Command rules](#command-rules).

### Turn off automatic command detection

Set `shell-hook #false` inside `activities.sources`. Leave `process-watch` off
as well if you don't want process detection. You can still use `feresectl run`
and direct publishers.

To skip shell and Activities setup during installation, pass
`./scripts/install.sh --no-activities`. This skips setup for that installation;
it doesn't remove an existing hook or disable an already configured source.

### Command rules

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
    rule "cargo" {
        match #"^cargo (build|check|test|run|clippy)\b"#
        title "cargo {1}"
        icon "hammer"
        priority 100
        progress "indeterminate"
        min-duration-ms 1500
        ignore-parents "rust-analyzer"
        dedupe-by "cwd"
        flash-on-fail #true
        result ok="Finished" fail="Command failed"
    }
}
```

User rules run in declaration order, then unreplaced built-ins in Cargo,
npm/pnpm, make, git-push, docker-build order. A user rule replaces the entire
built-in with the same name. One command selects one rule, pinned for its lifetime.
`ignore-parents` accepts multiple names. Regex input/pattern/program sizes are
bounded. Command and cwd metadata are used only during matching; nonmatches are
discarded and never logged. Grouping hashes cwd within owner/rule. Invocations
retain independent lifetimes and progress is never averaged.

The global/rule visibility delay is their maximum. Completion before that delay,
including failure, never flashes. Eligible results last 1500 ms by default;
`flash-on-fail #false` removes failures directly. Configuration is validated
before application; an invalid reload preserves the last valid configuration.
Diagnostics include source offsets for activity configuration errors.

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
Ownership verification needs Linux 6.5 or newer. See
[Publisher ownership and lifecycle](#publisher-ownership-and-lifecycle)
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
supplies real progress and completion results.

## Publisher ownership and lifecycle

Use the same socket selection as every other control command: `--socket`, then
`FERESE_SOCKET`, then the session runtime default. Separate nested sessions keep
separate brokers. One-shot commands use their verified immediate parent (normally
the shell), not the CLI connection, as lifetime owner. IDs are local to that owner.
A direct publisher and `run` use the publishing process itself. The host obtains
kernel peer credentials and a socket-bound pidfd, checks the parent relationship,
process birth and UID, and monitors owner exit. This requires Linux 6.5 or newer;
an unverifiable owner is rejected instead of trusting a JSON PID.

```sh
feresectl activity start build --icon hammer --title "Build"
feresectl activity update build --progress 0.6
feresectl activity end build --result ok --message "Built"
feresectl activity start tea --icon timer --title "Tea" --remaining-ms 180000
feresectl run --title "cargo test" -- cargo test
```

`start` prints a JSON handle containing owner, session and generation. The
`--handle` option on update/end/cancel checks that exact invocation. Omitting it
resolves the local ID only within the same verified owner. Duplicate starts are
rejected. Repeated identical accepted results keep the original result deadline;
a removed or superseded generation is stale. Register again after broker restart.

The compositor assigns a terminal window at activity creation using the
publisher's process ancestry. List selection follows the activity ID across
reordering. Recording and low battery keep their protected priority, including
the combined recording/low-battery presentation.

Each owner may hold three records, including delayed and terminal entries. The
session defaults to 128 records, a priority ceiling of 160 and two compact slots.
New publications are limited to ten per owner per second. Replaceable updates
are limited to 30 per owner per second; rejected updates get
an explicit error. The bar coalesces snapshots through a bounded latest-value
subscription while the broker retains accepted results. Terminal/cancel dispatch waits for bounded dispatcher capacity
on the socket worker. A CLI disconnect does not remove an owner's records.
Running owner death and expired leases remove entries without assigning a result.
Results survive owner exit until their original deadline. Leases are positive and
at most five minutes; valid refreshes renew only the lease.

Activities stay on their creation output, initially the focused output. On output
removal they move to the first surviving output, or become unpresented until an
output is available. Host timestamps use `CLOCK_BOOTTIME`, including suspend.
Countdown wire timestamps are ignored and replaced with host sample time.

## Extensions and templates

The version-1 activity schema is carried inside the existing versioned,
length-prefixed JSON `activity-publish` command. `activity-get` and long-poll
`activity-watch` provide snapshots. See `ferese_ipc::activities` and
`ferese_activities::Data`; there is no second socket or JSON-lines protocol.

Extensions publish typed Progress, Timer, Media, or List data. The bar controls
rendering. Values are indeterminate, finite progress in `[0,1]`, bounded text, or
countdown durations. Titles/text are limited to 256 UTF-8 bytes; lists to eight
rows. Icons are symbolic names; unknown names use the Ferese symbol, and paths
are rejected. No extension widgets, markup, colors, shaders or drawing programs
are accepted.

Use `--template` with a typed JSON template, e.g.
`'{"kind":"list","data":{"rows":["Indexing files","Waiting for server"]}}'`.
Capture/call integrations use `--source capture` and describe the state they
actually observe. Process names, microphone use, or camera use do not infer calls
or downloads. Existing MPRIS state supplies Media rows and transport controls;
recording/battery reuse the shell's existing protected state and watchers.

The optional notification bridge uses existing notification state, ranks below
all other ordinary sources, and respects DND and popup privacy. When enabled, a
notice becomes an activity rather than a duplicate toast. It is off by default.
