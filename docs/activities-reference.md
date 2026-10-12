# Live Activities reference

For setup, everyday commands and troubleshooting, start with the
[Live Activities guide](activities.md). This page covers rules and integrations
that publish their own status.

## Command rules

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

## Manual shell setup

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
[Cargo adapter](activities.md#cargo-progress) requires `feresectl run`.

`feresectl run` preserves argv, cwd, environment, terminal process group,
stdout/stderr, and child exit/signal status. It still runs the command if
publishing fails, and a `build-finished` message never completes its activity:
completion follows the actual child exit.
The adapter preserves rendered diagnostics and non-JSON output. See the
[Cargo JSON contract](https://doc.rust-lang.org/cargo/reference/external-tools.html).

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

For example, an actual download integration can publish its own reliable fraction:

```sh
feresectl activity start download --source download --icon download \
    --title "Downloading update" --progress 0.2
feresectl activity update download --progress 0.7
```

Use `--template` with a typed JSON template, e.g.
`'{"kind":"list","data":{"rows":["Indexing files","Waiting for server"]}}'`.
Capture/call integrations use `--source capture` and describe the state they
actually observe. Process names, microphone use, or camera use do not infer calls
or downloads. Existing MPRIS state supplies Media rows and transport controls;
recording/battery reuse the shell's existing protected state and watchers.

The optional notification bridge uses existing notification state, ranks below
all other ordinary sources, and respects DND and popup privacy. When enabled, a
notice becomes an activity rather than a duplicate toast. It is off by default.

## Validation

Hidden items and measurement widgets do not animate. Reduced motion snaps
progress and uses a static track. Workspace buttons retain their own actions;
activity selection does not replace workspace navigation.

```sh
cargo test --locked -p ferese-activities -p ferese-config -p ferese-ipc -p feresectl
cargo test --locked -p ferese-shell
python3 scripts/tests/test_activity_hooks.py -v
python3 -m unittest discover -s scripts/tests -p 'test_portal_install.py' -v
FERESE_TEST_ACTIVITIES=1 python3 scripts/tests/test_activities_isolated.py -v
FERESE_REVIEW_BACKEND=tiny-skia cargo test --locked -p ferese-shell \
    render_hub_review_states -- --ignored --nocapture
```

The isolated check starts a private Weston host and broker. Render captures live
in `/tmp/ferese-hub-review`; they include single/split/list content, long titles,
translucency and 1×/1.25×/1.5× scales. Direct DRM, physical multi-monitor hotplug,
suspend and live capture hardware still need hardware validation.
