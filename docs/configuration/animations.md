# Animations

Continuum is Ferese's shared compositor and shell motion system. Set animation
speed in Settings → Windows → Motion or edit the `animations` block in
`~/.config/ferese/config.kdl`. Changes apply live. Start with `speed` if you only
want faster or slower transitions; you do not need to change the springs.

```kdl
animations {
    enabled #true
    reduced-motion #false
    speed 1.0

    spring {
        duration-ms 240.0
        bounce 0.0
        overshoot #false
    }

    viewport-spring {
        duration-ms 350.0
        bounce 0.0
        overshoot #false
    }
}
```

This example uses the built-in defaults. The packaged config sets `speed 0.9`.
You can omit either spring block to keep its defaults.

| Section / key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `animations.enabled` | boolean | `true` | Enable motion |
| `animations.reduced-motion` | boolean | `false` | Disable animated motion |
| `animations.speed` | number > 0 | `1` | Higher is faster; `0.75` is slower |
| `animations.spring.duration-ms` | number > 0 | `240` | Window and shell spring response |
| `animations.viewport-spring.duration-ms` | number > 0 | `350` | Scrolling and workspace spring response |

Both `spring` and `viewport-spring` accept these keys:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `duration-ms` | number > 0 | `240` or `350`, as above | Larger values give a slower response; not a completion deadline |
| `bounce` | number strictly between −1 and 1 | `0` | Zero is critical damping; positive values require overshoot |
| `overshoot` | boolean | `false` | Allow this spring to cross its target |

Use duration and bounce to configure spring physics. `mass`, `stiffness`,
`damping` and `damping-ratio` are internal solver parameters, not configuration
keys. Removed or misspelled animation keys are rejected. Invalid edits leave
the last accepted configuration active.

## Speed and reduced motion

`speed` changes window motion, scrolling, workspace transitions, overview,
theme transitions and shell popup, notification and hover transitions. `0.5`
gives an animation twice as much time; `2.0` gives it half as much. Overview
uses 60% of this speed when opening or closing, including when selecting a
window. There is no separate overview-speed setting.

Set `reduced-motion #true` or `enabled #false` to make these transitions
immediate. Reduced motion takes precedence over `enabled #true`; setting
`speed 0` is invalid. These settings control Ferese's animations, not animations
inside other applications.

Morph animates its shape with its own spring and fades between views, while
following the shared speed and reduced-motion policy; see the
[panel guide](../panel-configuration.md#morph) for its states and size controls.

Shell animations sample motion on each surface's redraw, paced by its Wayland
frame callbacks. They request animation frames only while moving.
Menus, modals, notifications and hover fades use this path instead of a fixed
16 ms animation timer. Clock, notification-expiry and service updates keep their
own schedules.

Overview can reverse while moving, and workspace changes slide in their navigation
direction. Workspace and scrolling gestures carry their release velocity into
the settling spring. Scrolling gestures resist at viewport limits and spring back.
Window opening starts when content is available; closing waits for the application
to unmap, then animates its retained image. Input and application close requests
remain responsive while the animations finish.

Live windows, overview thumbnails and retained images share a window identity
and presentation state. Entering overview inherits the window's visible position
and velocity, including motion from a workspace slide. Resize images use the same
content mapping in overview and remain visible if the window closes during a resize.
Content, clipping and borders use the same squircle outline at the output's scale.

## Spring tuning

`spring` controls window position and size motion, fullscreen/maximize zoom and
overview entrance and dismissal, opening/closing windows, focus emphasis, shell
popups and notifications. `viewport-spring` controls the scrolling viewport,
workspace slides and the column-width animation that runs with scrolling.
Continuum shares the analytic spring solver and motion settings between the
compositor and shell. Small opacity changes (hover, dimming, theme fades) may
still use timed transitions.

Focus emphasis and shadows have separate spring responses. Emphasis uses 80% of
the main spring's response time; shadows use 120%. These ratios preserve the
configured damping ratio and overshoot policy, and follow `speed` and reduced
motion. They are built-in tuning values, not additional configuration keys.
Focused windows use the theme's full shadow. Unfocused shadows use 75% of its
vertical offset, 90% of its blur radius and 80% of its opacity, with spring motion
between those values. Shadow motion does not change the content or border geometry.

Spring settling time depends on distance, velocity and tolerances. `duration-ms`
controls the response, not a fixed completion deadline. Both default springs are
critically damped; the viewport has a slower response than windows and shell popups.

To opt into bounce for one spring:

```kdl
animations {
    spring {
        duration-ms 300.0
        bounce 0.2
        overshoot #true
    }
}
```

Bounce defaults to zero and must be strictly between -1 and 1. Positive bounce
requires `overshoot #true` in that spring block. Negative bounce adds overdamping.
Without overshoot, Ferese stops a spring at its first target crossing. Opacity
remains bounded even when geometry can overshoot. Overshoot is never enabled globally.

For the solver, mass is fixed at `1`. With `T = duration-ms / 1000`, Continuum uses
`omega = 2*pi/T`, `stiffness = omega^2` and `damping = 2*omega*zeta`.
`zeta = 1-bounce` for nonnegative bounce, otherwise `zeta = 1/(1+bounce)`.
This conversion does not guarantee the same perceptual duration or settling time
as Apple's springs.

Duration and speed must be finite and greater than zero. Bounce must be finite.
Values that overflow or underflow the derived coefficients are rejected. The
allowed bounce range keeps damping positive, so undamped oscillation cannot
prevent popup cleanup.
