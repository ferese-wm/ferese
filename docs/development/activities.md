# Publishing activities

Use the activity IPC to publish progress, timers, media or lists in the panel.
For shell setup and everyday use, see [Live Activities](../activities.md).
The [configuration reference](../configuration/activities.md) covers settings and rules.

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
A direct publisher and `run` use the publishing process itself.

The host verifies ownership using kernel peer credentials and a socket-bound pidfd. It checks the parent relationship, process birth and UID, and monitors
owner exit. This requires Linux 6.5 or newer; an unverifiable owner is rejected
instead of trusting a JSON PID.

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

### Limits and delivery

Each owner may hold three records, including delayed and terminal entries. The
session record, priority and compact-slot limits follow [Activities configuration](../configuration/activities.md).

New publications are limited to ten per owner per second. Replaceable updates
are limited to 30 per owner per second; rejected updates get
an explicit error.

The bar keeps only the latest snapshot in its bounded subscription; the broker
retains accepted results. Terminal and cancel requests wait for bounded
dispatcher capacity on the socket worker. A CLI disconnect does not remove an
owner's records.

### Expiry and output changes

Ferese removes running activities without a result when their owner exits or
their lease expires.
Results survive owner exit until their original deadline. Leases are positive and
at most five minutes; valid refreshes renew only the lease.

Activities stay on their creation output, initially the focused output. On output
removal they move to the first surviving output, or become unpresented until an
output is available. Host timestamps use `CLOCK_BOOTTIME`, including suspend.
Countdown wire timestamps are ignored and replaced with host sample time.

## Extensions and templates

The version-1 activity schema is carried inside the existing versioned,
length-prefixed JSON `activity-publish` command. `activity-get` and long-poll
`activity-watch` provide snapshots. See [`ferese_ipc::activities`](../../crates/ferese-ipc/src/activities.rs)
and [`ferese_activities::Data`](../../crates/ferese-activities/src/lib.rs); there is no
second socket or JSON-lines protocol.

Extensions publish typed Progress, Timer, Media, or List data. The bar controls
rendering. Values are indeterminate, finite progress in `[0,1]`, bounded text, or
countdown durations. Titles/text are limited to 256 UTF-8 bytes; lists to eight
rows. Icons are symbolic names; unknown names use the Ferese symbol, and paths
are rejected. No extension widgets, markup, colors, shaders or drawing programs
are accepted.

Use `--template` with a typed JSON template, for example:
`'{"kind":"list","data":{"rows":["Indexing files","Waiting for server"]}}'`.
Capture/call integrations use `--source capture` and describe the state they
actually observe. Process names, microphone use, or camera use do not infer calls
or downloads. Existing MPRIS state supplies Media rows and transport controls;
recording/battery reuse the shell's existing protected state and watchers.

The optional notification bridge uses existing notification state, ranks below
all other ordinary sources, and respects DND and popup privacy. When enabled, a
notice becomes an activity rather than a duplicate toast. It is off by default.
