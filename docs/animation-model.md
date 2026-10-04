# Continuum: Ferese’s animation model
When you resize a window, Ferese has to coordinate where the layout wants it, where it currently appears, and what size the application has actually drawn. Those can differ during a transition: the layout may already have requested a larger window while the application is preparing its next buffer and the visible frame is still expanding. Continuum manages that visible movement so the application and your next input can proceed while the animation finishes. It provides the spring implementation and motion settings used by both the compositor and the shell.

How that movement behaves depends on what you’re doing. While dragging, the window follows the pointer directly; adding a spring there would make it trail your hand. When you release a gesture, however, a spring takes over with the velocity of the movement you just made. If you change the destination before it settles, Ferese can retain the current position and velocity rather than restart the movement from rest. Large movements use zero bounce by default, so they settle without oscillating around the destination.

## Keeping layout, animation, and application buffers in step

The resize example explains why [WindowGeometry](../crates/ferese-animation/src/lib.rs) keeps three states. Its logical rectangle records the destination requested by layout or a presentation mode, such as fullscreen. Its visual rectangle records what is moving toward that destination, including the current bounds and velocity. Client geometry tracks the size Ferese requested from the application and the size the application has committed.

Keeping these separate means the compositor can request the destination size as soon as the layout changes. It does not have to wait for the animation to finish before asking the application to draw. The new buffer might arrive before or after the visible rectangle settles, so retained images cover the resize handoff while presentation continues independently.

In scrolling layout, the visible position also depends on the viewport. A window has a position in the workspace, and moving the viewport changes where that position falls on screen:

```text
screen_x = world_x - viewport_x
```

The window and viewport have separate springs because either can move without the other. Fullscreen and maximize temporarily take control of presentation geometry, which makes returning to the scrolling layout a handoff between two ways of positioning the same window. That handoff must preserve the visible rectangle; otherwise the window would jump when the layout resumes control.

The compositor carries the result in [WindowPresentation](../compositor/src/presentation.rs), alongside window identity, opacity, focus emphasis, shadow strength, and content-scaling information. Keeping the identity with the presentation allows an overview thumbnail or a retained closing image to remain associated with its window even though its bounds have changed.

Those bounds also keep the rendering layers together. Content, clip, and border use the same presented geometry, with their physical edges rounded together at the output scale. Independent springs for those edges could let the border pull away from the content, especially at fractional scales. Shadow strength and focus emphasis can have different responses because neither determines where the content ends.

## What happens when a movement is interrupted

Suppose a window is moving toward one position when another layout change sends it elsewhere. The animation needs its current velocity as well as its current position to continue without restarting from rest. The shared `AnimatedValue` and `AnimatedRect` types retain both, along with the target; a rectangle stores a velocity for each of its four components. Changing the target leaves that existing position and velocity intact.

Before retargeting, the caller samples the motion at the interruption time. That sample becomes the starting state for the new destination, which is how fullscreen reversal, overview reversal, and workspace slide retargeting continue from motion already in progress. Between those changes, the [solver](../crates/ferese-animation/src/spring.rs) evaluates the damped spring equation over the actual elapsed time:

```text
m x″ + c x′ + k(x − target) = 0
```

The solver handles underdamped and overdamped motion and uses a power-series evaluation near critical damping to avoid numerical cancellation. Because it evaluates the trajectory directly, it does not need Euler integration steps or a cap that treats a long frame as a shorter one. After a stall, the spring advances by the elapsed wall time. The missed visual samples are still lost, but the motion does not acquire an extra delay by discarding that time.

The physical solution alone does not decide when Ferese should stop drawing an animation. Both position error and velocity must be within the property’s tolerances to settle; at that point, presentation snaps exactly to the target and clears velocity. Crossing policy adds another way to stop. With `NoCrossing`, a component stops the first time it reaches its target, while `AllowOvershoot` lets it cross and settle later. Crossing detection examines the whole elapsed interval, so a long frame cannot hide a crossing merely because the component has already returned to its original side of the target.

Preserving velocity is useful when continuing the same movement, but some interactions deliberately replace it. Pointer dragging sets geometry directly, and disabled animations or reduced motion snap to the destination. A first-crossing clamp clears velocity when it stops the component. Selecting a different shell panel also starts a new entrance for the new content, as described below. Spatial transitions otherwise use springs, while small opacity changes such as hover or theme interpolation may still use timed transitions.

## Choosing the spring response

Users configure duration, bounce, and a per-spring overshoot policy. Mass, stiffness, and damping remain implementation parameters, but they are no longer accepted animation configuration fields. The [settings resolver](../crates/ferese-animation/src/settings.rs) converts the public settings to those physical parameters as follows:

```text
T = duration_ms / 1000
m = 1
ω = 2π / T
ζ = 1 − bounce          when bounce ≥ 0
ζ = 1 / (1 + bounce)    when bounce < 0
k = mω²
c = 2mωζ
```

A `duration-ms` value therefore changes the spring’s response rather than setting a completion deadline. Two movements using that value can take different times to settle because they begin at different distances or velocities, or use different crossing policies and tolerances. These are Ferese’s conversion rules; they do not establish equivalence with Apple’s perceptual spring APIs.

The main spring responds more quickly than the viewport spring, allowing a window’s geometry to settle sooner than a scrolling movement. Overview applies an additional speed factor to give its larger changes more time. Focus emphasis and shadow strength derive their responses from the main spring, with emphasis responding sooner and the shadow following more slowly:

| Setting or response | Current value |
| --- | --- |
| Main spring | 240 ms, bounce 0, overshoot false |
| Viewport spring | 350 ms, bounce 0, overshoot false |
| Built-in animation speed | 1.0 |
| Packaged configuration speed | 0.9 |
| Overview geometry and overview workspace slides | 0.6 × configured speed |
| Focus emphasis response | 0.8 × main spring response time |
| Shadow response | 1.2 × main spring response time |

Bounce must be strictly between −1 and 1. Negative values produce overdamping; positive values require `overshoot #true` so the crossing policy permits the requested bounce. Duration and speed must be finite and positive, and the resolved stiffness and damping must remain finite and positive too. If the configuration fails these checks, Ferese keeps the last accepted settings.

The global speed setting scales elapsed spring time, so 0.5 takes twice as long and 2.0 takes half as long. It applies to both compositor and Ferese shell motion. Setting `reduced-motion #true` takes precedence over enabled animations and applies the destination directly. Neither setting controls animations that applications draw inside their own windows.

## Passing a gesture’s velocity into the release

A release needs to account for how the fingers were moving just before they lifted. The shared [velocity tracker](../crates/ferese-animation/src/gesture.rs) keeps up to 32 samples from the last 100 ms and estimates velocity at the newest sample. With three or more samples it fits a quadratic; with two it fits a line. If the newest sample is older than 40 ms, it returns zero release velocity so an old movement does not supply momentum after a pause. Duplicate timestamps replace the preceding sample, while out-of-order and non-finite samples are rejected.

That estimate helps select a destination before the spring takes over. For a position `p`, velocity `v` in position units per second, and per-millisecond deceleration factor `r`, the projected destination is:

```text
projected = p − v / (1000 × ln(r))
```

The compositor’s [momentum swipe release](../compositor/src/gestures.rs) uses `r = 0.997` and commits when projected normalized progress reaches 0.5. This lets release velocity influence the decision instead of deciding from distance alone. Cancellation does not commit, and a bounded preview suppresses velocity directed beyond its endpoint. Once projection has selected the target, the spring performs the release motion.

At a blocked workspace edge, the visible displacement is resisted, so the velocity passed to the spring must account for that resistance as well. For an extent `L`, displacement `d`, and resistance constant `a = 0.55`, Ferese uses:

```text
q = 1 + a × |d| / L
displayed = sign(d) × L × (1 − 1/q)
derivative = a / q²
```

The [workspace gesture state](../compositor/src/state.rs) uses normalized extent 1. As the fingers push farther beyond the boundary, this function allows progressively less visible movement. Its derivative converts finger velocity to the velocity of that resisted presentation before the spring returns it to the boundary. Without that conversion, the release would start at a speed that did not match the movement visible just before it.

## Moving into overview and between its workspaces

Entering [overview](../compositor/src/overview.rs) begins with the windows’ sampled desktop presentations and moves them toward thumbnail rectangles. The slower overview response gives those large geometry changes more time. If you reverse the transition before it finishes, the current rectangles and velocities are available for the return movement.

Once overview is open, clicking another workspace in the strip changes which grid is visible. The outgoing and incoming grids slide horizontally according to workspace order, with incoming windows already at thumbnail size. This keeps the workspace change within overview rather than replaying the full entrance from desktop-sized windows. Both grids remain available during the slide; outgoing presentations are removed after it settles.

The strip sits outside the grid’s slide transform, so selecting a workspace does not carry the strip along with the windows. Its layout can still change when, for example, activation creates a trailing empty workspace. Repeated clicks retarget the existing slide from its sampled position and velocity, allowing a new selection to take effect before the previous movement finishes.

These slides use the viewport spring through [workspace navigation](../compositor/src/state/navigation.rs). Desktop workspace switches move vertically, while switches inside overview move horizontally and use the overview speed factor. With reduced motion enabled, the selected workspace appears without the slide.

## Letting exits finish after content closes

An application’s surface lifetime does not always match the duration of its visible exit. Window entrance starts when content is available, but on unmap the live surface can disappear before a closing animation would finish. Ferese retains content for that exit, moving toward 98% of the current bounds while opacity, emphasis, and shadow fade to zero.

Closing during resize also retains the resize snapshots and their handoffs. Their progress must continue during close so the old buffer does not remain frozen over the exiting image. Retaining a picture is only part of the work; the presentation state that determines how it blends with the newer content has to continue advancing too.

Shell popups keep their surface until the closing motion settles, then destroy it. If you reopen the same panel during close, Ferese retargets that existing motion. Switching from Battery to Wi-Fi is different: the surface stays, but the new panel gets its own entrance. This behavior lives in [panel lifecycle handling](../shell/ferese-shell/src/status_ui/lifecycle.rs) and avoids a destroy-then-create gap between panels.

Because the shell draws content while the compositor draws its material, both must use matching presentation state during an exit. Otherwise the text can disappear while the background, blur, or rounded clip remains visible. The surface’s continued existence gives the exit time to finish, but its content and material still have to finish together.

## Sampling motion for the display

The spring can provide a position for a given time, but the renderer still has to choose which time to sample. On the direct backend, the compositor’s [per-output scheduler](../compositor/src/frame_scheduler.rs) estimates the next presentation time using output timing and render cost. The [prediction path](../compositor/src/state/prediction.rs) samples for that time, composing the window, viewport, overview, and workspace offsets into the presentation that will be drawn.

The shell runs in a separate process with its own clock and frame lifecycle, even though it shares the spring implementation and settings. Its [frame-driven wrapper](../shell/ferese-shell/src/motion/frames.rs) samples actual elapsed time on redraw and requests another frame while motion remains active. Iced’s Wayland backend gates those requests through the surface frame callback, so animation sampling follows the surface’s opportunity to draw.

After settlement, hover redraws reuse animation content when the motion sample has not changed. Notification expiry and clock updates still need their own timers, but those timers serve different purposes from advancing an animation. The shared spring math therefore gives shell and compositor motion the same response model without guaranteeing identical presentation timestamps: frame callbacks, buffer commits, renderer cost, and compositor deadlines still affect what reaches the display.

## Checking the model against what is drawn

The [animation trace tests](../crates/ferese-animation/src/traces.rs) follow motion through stalls, changing frame cadence, retargeting, resize interruption, and gesture release. Solver tests compare the results with an independent characteristic-root reference, while gesture tests check projection and the rubber-band derivative. Compositor tests then check how that motion is used for presentation ownership, predicted geometry, workspace switching, and reduced motion.

Rendering checks need to follow the interruptions people can make while using the desktop: reversing a transition, clicking several overview workspaces, closing during resize, or selecting a shell panel before the previous motion settles. At fractional output scales, those checks must also look for separation between content, clip, and border, and for background or blur remaining after shell content exits. Frame traces should confirm that animation requests stop after settlement.

Those checks establish state and rendering behavior, but input-to-presentation latency and smoothness at 60, 120, 144, and 240 Hz still need measurements on the target hardware. The solver’s correctness cannot tell us whether a particular frame arrived in time to be displayed.

For user settings and KDL examples, see [Animation configuration](configuration.md#animations).
