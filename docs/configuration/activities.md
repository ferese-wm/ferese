# Configure Activities

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

## Turn off automatic command detection

Set `shell-hook #false` inside `activities.sources`. Leave `process-watch` off
as well if you don't want process detection. You can still use `feresectl run`
and direct publishers.

To skip shell and Activities setup during installation, pass
`./scripts/install.sh --no-activities`. This skips setup for that installation;
it doesn't remove an existing hook or disable an already configured source.

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
