# Tabbed windows

Tabs keep several windows in the same space, with one visible at a time.

## Create a group

In scrolling layout, press **Super + [** to put the focused window in the column
to its left, or the one to its right if there is no left neighbour. Press
**Super + Shift + T** to turn that column into tabs. Press it again to return to
stacked rows with their previous sizes.

In tree layout, **Super + [** joins the focused window to a neighbour as a tab.
Ferese looks left, right, up, then down for a window to join.

If you want to choose the direction, bind `join-tab` to `left`, `right`, `up`, or
`down`; see [Commands and bindings](configuration/bindings.md). In scrolling
layout, this also turns the destination column into tabs, even when the two
windows are already in the same column. The ordinary `consume` action keeps the
destination column's stacked or tabbed choice.

## Switch tabs

| Shortcut | Action |
| --- | --- |
| Super + Ctrl + [ | Previous tab |
| Super + Ctrl + ] | Next tab |
| Super + J / K | Next / previous row or tab in a scrolling column |
| Super + H / L | Previous / next scrolling column |

The previous/next tab shortcuts wrap at either end of the group. Switching tabs
in a scrolling column keeps its width and scroll position. Vertical resizing
has no effect while the column is tabbed.

In tree layout, click a title in the bar above the window to select it.
Scrolling columns have a segmented rail on the left: click a segment, or hover
over the rail to see a list of titles. You can click a title in that list too.
The list stays open as you move between it and the rail. Scroll through it when
there are too many titles to fit on screen.

Keyboard tab switching briefly shows the selected tab's number and title near
the top of the window, as `n/N · title`.

## Take a window out

Press **Super + ]** to move the focused tab into its own column or split, to the
right of the group. You can also use the `leave-tab` action. Closing a window,
making it floating, or moving it to another workspace removes it from the group.
New windows open beside the group.

Tab controls disappear when only one window remains, or when the selected window
is maximized or fullscreen. Leave fullscreen before grouping or switching tabs;
fullscreen belongs to the individual window. Switching between tree and scrolling
layouts keeps all windows but separates the groups.

## Indicator and appearance

The scrolling rail and title list sit over the desktop without taking space from
the window. By default, the rail sits in the left gap if there are at least 8
logical pixels available; otherwise it sits just inside the window. Its click
area is wider than the visible rail. On short windows, the segments shrink to fit.

The rail uses the theme's accent for the selected tab. The title list follows the
shell's colors, corner radius and opacity. To change its size, colors or keyboard
feedback duration, see [Layout configuration](configuration/layout.md#tabbed-columns).
That page also shows how to open new columns as tabs by default.

## feresectl

These commands work on the focused window:

```sh
feresectl toggle-column-tabbed
feresectl join-tab left
feresectl next-tab
feresectl previous-tab
feresectl leave-tab
```
