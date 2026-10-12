# Live Activities

Add **Activities** from Settings → Panels → Arrange to any group in Start,
Center, or End. It is a normal item with visibility, representation, priority,
gaps, and overflow settings. Existing panel definitions keep their item order.
Flat, Floating, and Adaptive panels use the configured position and an anchored
list popup. Morph uses the configured item inside its existing central island,
with compact/split content and the same list. Morph's zones remain its control
catalog; they do not create separate islands.

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

The item has no content when no ordinary task is eligible. Known progress draws
a thin ring around its bundled icon. Unknown work uses a restrained rotating arc,
without a percentage. Hidden items and measurement widgets do not animate.
Reduced motion snaps progress and uses a static track. Morph preserves recording
and low-battery priority, including their combined presentation. DND limits ordinary
running activities to one, hides the count, and suppresses terminal flashes.
Click opens the list; arrow keys select rows and their identity survives reorder.
Workspace buttons retain their own actions. Automatic expansion is disabled.

## Publishing

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
feresectl activity cancel build
feresectl activity start tea --icon timer --title "Tea" --remaining-ms 180000
feresectl run --title "cargo test" -- cargo test
```

`start` prints a JSON handle containing owner, session and generation. The
`--handle` option on update/end/cancel checks that exact invocation. Omitting it
resolves the local ID only within the same verified owner. Duplicate starts are
rejected. Repeated identical accepted results keep the original result deadline;
a removed or superseded generation is stale. Register again after broker restart.

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

## Rules and opt-in detection

```kdl
activities {
    max-visible 2
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

Source the matching file in `contrib/activities/` explicitly. zsh uses
`add-zsh-hook`, fish uses named pre/postexec events, and Bash composes with
[bash-preexec](https://github.com/rcaloras/bash-preexec)'s hook arrays. Load
bash-preexec before the Bash hook; the hook does not replace an existing DEBUG
trap. Hook files never execute captured command text and preserve command status.
Hooks and process watching are off by default. The optional process watcher is
an alternative when hooks are unavailable; it performs bounded sampling on one
worker, and disappearance means expiry, never inferred success/failure.

`feresectl run` preserves argv, cwd, environment, terminal process group,
stdout/stderr, and child exit/signal status. Publishing failure leaves execution
available. It does not automatically wrap shell commands. `progress "cargo-json"`
opts a matched rule into the supported `run` adapter; `run --cargo-json` explicitly
opts in one Cargo invocation. The adapter adds Cargo JSON diagnostics, preserves
rendered diagnostics and non-JSON output, and reports completed units as text.
`build-finished` never completes the activity; the actual child exit does.
Cargo does not provide a reliable total-work denominator. See the
[Cargo JSON contract](https://doc.rust-lang.org/cargo/reference/external-tools.html).

## Validation

```sh
cargo test --locked -p ferese-activities -p ferese-config -p ferese-ipc -p feresectl
cargo test --locked -p ferese-shell
python3 scripts/tests/test_activity_hooks.py -v
FERESE_TEST_ACTIVITIES=1 python3 scripts/tests/test_activities_isolated.py -v
FERESE_REVIEW_BACKEND=tiny-skia cargo test --locked -p ferese-shell \
    render_hub_review_states -- --ignored --nocapture
```

The isolated check starts a private Weston host and broker. Render captures live
in `/tmp/ferese-hub-review`; they include single/split/list content, long titles,
translucency and 1×/1.25×/1.5× scales. Direct DRM, physical multi-monitor hotplug,
suspend and live capture hardware still need hardware validation.
