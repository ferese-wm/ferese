# Lock screen

Ferese Lock follows your desktop wallpaper, colors, font, and shell corners.
Open **Settings → Lock screen** to change its clock, date, wallpaper softness,
and inactivity delays.

## Preview and lock

```sh
ferese-lock --preview      # ordinary window; does not lock or check passwords
ferese-lock                # lock; returns after compositor confirmation
ferese-lock --foreground   # stay attached until unlock
```

Do not enter your real password in a preview. The account picture comes from
AccountsService or `~/.face`; a silhouette is used when neither is available.

Press Escape or Ctrl+U to clear the password, or Backspace to remove the last character.
Partial input clears after ten seconds without typing. The authentication status also shows whether
Caps Lock is on. A failed attempt displays an error and delays the next retry.
Authentication uses the system's PAM policy; both password and account validation must succeed.

The screen brightness keys work while locked, including when brightness is zero.
They require `brightnessctl` and adjust brightness in 2% steps. While locked,
these keys use the compositor's brightness control instead of custom bindings.

## Automatic locking

Install `swayidle`, then add this to `~/.config/ferese/config.kdl` to lock after
five minutes and before system sleep:

```kdl
autostart {
    command "swayidle" "-w" "timeout" "300" "ferese-lock" "before-sleep" "ferese-lock" "lock" "ferese-lock"
}
```

Test password unlocking before enabling automatic locking. The installer provides
`/etc/pam.d/ferese-lock` and preserves an existing administrator policy.

## Inactivity and display sleep

Locked displays dim after 30 seconds without input and sleep after 120 seconds.
Keyboard, pointer, or touch activity wakes them without unlocking. Set either
delay to zero to disable it, in Settings or KDL:

```kdl
lock-screen {
    dim-after-seconds 30
    sleep-after-seconds 120
}
```

Both delays start when the session locks, separately from the automatic-lock timer.
Display sleep does not suspend the computer.

## Recovery and limits

The lock covers all displays, including displays connected after locking. If the
lock UI crashes, the compositor keeps the session protected and allows a replacement
locker to take over. A running locker cannot be replaced. To recover, launch
`ferese-lock` with the affected session's Wayland environment. You can also end the
session from another TTY; see [Recovery](installation.md#logout-and-recovery).

The native DRM backend reports readiness after every connected output has presented
protected content, or immediately when there are no outputs. The locker accepts the
runtime's real `Locked` event; sending a lock request is not confirmation. It starts
with an opaque fallback and loads the wallpaper and account picture after confirmation,
so a slow image cannot delay the lock request. A refused or interrupted foreground
lock exits with an error.

Nested Ferese sessions do not advertise session locking because their backend cannot
verify when the host presents a submitted frame. Use `ferese-lock --preview` to inspect
the UI, and lock the host session for protection.

Restricted Wayland clients cannot discover or bind `ext_session_lock_manager_v1`,
including after a locker crash. Recovery requires an unrestricted desktop client.

The password buffer occupies a fixed, locked memory mapping that is excluded from
core dumps and wiped before release. The authentication worker takes ownership of that
mapping, while the text widget receives only bullets. Short-lived toolkit keyboard
events and PAM-owned responses still sit outside this buffer's memory guarantees;
this does not provide end-to-end protection for every copy made by those dependencies.
Keyboard traces omit keycodes, keysyms and modifier contents, even at TRACE level.

Avatar files are limited to 8 MiB, 4096 pixels per dimension and a 32 MiB decoding
budget. Wallpapers must be regular files no larger than 32 MiB and are decoded on a
worker before they reach the UI. The clock updates at minute boundaries, independently
of authentication and password-clearing deadlines.

The password interface does not support password changes or interactive
multi-factor prompts. Update expired credentials outside the locker.

Developers should validate locking and readiness against
[ext-session-lock-v1](https://wayland.app/protocols/ext-session-lock-v1).
See [Development](development.md) for tests.
