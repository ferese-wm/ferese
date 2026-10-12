# Configure Activities

Select Morph in **Settings → Panel & Shell** to find **Visible activities** and
**Show activity count** in its appearance controls. These set the compact slots
and `+n` badge; hiding the badge still lets you open the full list.

Set sources, timing and command rules in the top-level `activities` block in
`~/.config/ferese/config.kdl`. Merge fields into an existing block rather than
adding a second one. This example enables the shell hook:

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

For hook installation and everyday use, see [Live Activities](../activities.md).
For the IPC schema and publisher lifetimes, see [Publishing activities](../development/activities.md).

## Settings

Omitted fields use these built-in defaults. Integer ranges include both endpoints.

| Key | Type / accepted values | Default | Meaning |
| --- | --- | --- | --- |
| `max-visible` | Integer 1–2 | `2` | Compact activity slots |
| `show-badge` | Boolean | `#true` | Show `+n` for tasks beyond those slots |
| `max-total` | Integer 1–128 | `128` | Session record limit, including delayed and completed entries |
| `owner-ceiling` | Integer 0–255 | `160` | Highest base priority used to rank ordinary activities |
| `start-delay-ms` | Integer 0–60000 | `300` | Minimum delay before a task appears; a rule can require a longer delay |
| `result-flash-ms` | Integer 0–10000 | `1500` | How long an eligible completion result remains visible |

## Sources

Set these booleans inside `activities.sources`:

| Key | Built-in default | Meaning |
| --- | --- | --- |
| `shell-hook` | `#false` | Accept command start/end events from an installed shell hook |
| `process-watch` | `#false` | Sample processes when hooks are unavailable; disappearance removes the task without inferring success or failure |
| `notification-bridge` | `#false` | Show notifications as activities instead of duplicate toasts, respecting Do not disturb and notification popup settings |

All three sources are off in the built-in defaults. First-time installer setup
enables `shell-hook` unless you explicitly set it to `#false`. The process watcher
and notification bridge stay off unless you enable them.

Media comes from MPRIS. Downloads and call statuses need an integration that
publishes them; microphone or camera use alone does not create a call activity.

Changes reload live. Invalid edits leave the last valid configuration in use.
Check an edit with `ferese --check-config ~/.config/ferese/config.kdl`.

## Turn off automatic command detection

Set `shell-hook #false` inside `activities.sources`. Leave `process-watch` off
as well if you don't want process detection. You can still use `feresectl run`
and direct publishers.

To skip shell and Activities setup during installation, pass
`./scripts/install.sh --no-activities`. This skips setup for that installation;
it doesn't remove an existing hook or disable an already configured source.

## Command rules

A named `rule` matches command text and supplies its title, icon, priority and
completion messages. Rules are matched in declaration order, then unreplaced
built-ins in Cargo, npm/pnpm, make, git-push, docker-build order. A user rule
replaces the entire built-in with the same name. One command selects one rule,
which stays attached to that invocation until it ends.

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

These defaults apply to a new custom rule. Built-in rules have their own values,
shown in the [source catalog](../../crates/ferese-config/activities.kdl).

| Key | Type / accepted values | Default for a custom rule |
| --- | --- | --- |
| Rule name | String, up to 256 UTF-8 bytes, no control characters; unique | Required positional argument |
| `match` | Valid regular expression, up to 2048 UTF-8 bytes | Required |
| `title` | String, up to 256 UTF-8 bytes, no control characters | Required |
| `icon` | Symbolic name, up to 64 ASCII letters, digits or hyphens | `"ferese"` |
| `priority` | Integer 0–255; ranking caps it at `owner-ceiling` | `100` |
| `progress` | `"indeterminate"` or `"cargo-json"` | `"indeterminate"` |
| `min-duration-ms` | Integer 0–60000 | `0` |
| `ignore-parents` | Up to 16 names, each up to 128 UTF-8 bytes | No ignored parents |
| `dedupe-by` | `"cwd"`, or omit for no grouping | Unset |
| `flash-on-fail` | Boolean | `#true` |
| `result.ok` | String, up to 256 UTF-8 bytes, no control characters | `"Finished"` |
| `result.fail` | String, up to 256 UTF-8 bytes, no control characters | `"Command failed"` |

Use `result ok="Finished" fail="Command failed"` inside the rule to change both
messages. If you provide `result`, both fields are required. Title placeholders
refer to regex captures: `{0}` is the whole match, `{1}` the first group, and so
on. Invalid capture references reject the configuration. An expanded title over
256 bytes or containing control characters is discarded during matching.

`ignore-parents` accepts multiple names in one node, such as
`ignore-parents "rust-analyzer" "editor"`. `dedupe-by "cwd"` groups tasks by a
hash of the working directory within the same owner and rule. Invocations retain
independent lifetimes, and progress is never averaged.

`progress "cargo-json"` enables the adapter for matching `feresectl run`
commands. Shell hooks do not wrap commands or parse their output automatically;
see [Cargo progress](../activities.md#cargo-progress).

## Visibility and matching limits

The visibility delay is the larger of `start-delay-ms` and `min-duration-ms`.
Completion before that delay, including failure, never flashes. Eligible results
last 1500 ms by default; `flash-on-fail #false` removes failures directly.

At most 64 rules may remain after unreplaced built-ins are added. Command input
is limited to 4096 UTF-8 bytes, working directories to 1024 bytes and parent lists
to 16 entries. The matcher sets a 256 KiB compiled-size limit and a 256 KiB DFA-size limit.
Command and cwd metadata are used only during matching; nonmatches are discarded
and never logged.

Configuration is validated before application; an invalid reload preserves the
last valid configuration. Diagnostics include source offsets for activity
configuration errors.
