# Login items and locking

| `autostart` key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `command` | nonempty argument array | required | Foreground process, not a self-daemonizing command |
| `enabled` | boolean | `true` | Start service |
| `restart` | boolean | `true` | Restart after exit, with a five-second retry delay |
| `nested` | boolean | `false` | Also run in nested previews |

Unchanged services keep running on reload; removed/disabled items stop. To lock
after five minutes and before sleep:

```kdl
autostart {
    command "swayidle" "-w" "timeout" "300" "ferese-lock" "before-sleep" "ferese-lock" "lock" "ferese-lock"
}
```

The locked display dims after 30 seconds and sleeps after 120 seconds of
inactivity. Settings → Lock screen changes `lock-screen.dim-after-seconds` and
`lock-screen.sleep-after-seconds`; zero disables each action. Input wakes the
display without unlocking. Display sleep does not suspend the computer.

`ferese-lock` is the native PAM-authenticated locker. It follows your wallpaper,
colors, font and shell corner radius. Its command returns successfully only after
the compositor confirms the lock; the UI keeps running until authentication
succeeds. `--foreground` keeps the command attached to its UI process.

Use `ferese-lock --preview` for an ordinary window that never locks or checks a
password. See [Native locker](../locking.md) for setup and verification. Test real
unlock in a nested compositor before enabling automatic locking. A crashed
locker leaves the session locked; recovery requires ending that session from
another TTY.

## Power and logout

Power actions use a centered confirmation. **Super+Shift+E** and
`feresectl request-logout` ask to log out. `feresectl exit` ends the session
immediately.
