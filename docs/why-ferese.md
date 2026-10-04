# Why Ferese

When you use a window manager, arranging windows is only part of using the desktop. You still need to connect to Wi-Fi, change an audio device, adjust a display, read notifications and lock the screen. Those tasks involve different components, but you encounter them as parts of the same environment. When their settings and behaviour are maintained separately, making them fit together becomes another job.

Ferese brings window management and desktop controls into one project. It includes a Wayland compositor, a desktop shell, Settings, Control Center and a lock screen, with configuration and theming shared across its components. Building them together makes it possible to decide how a desktop change should work across those components, rather than leaving each one to interpret it independently.

## Settings that apply beyond one component

Changing an accent colour should affect the places where Ferese uses that colour. Changing motion speed should affect both window transitions and shell transitions. These are desktop preferences, even though the compositor and shell run in separate processes. Ferese’s shared theme and motion settings give those processes a common definition to work from.

Settings and `~/.config/ferese/config.kdl` are two ways to edit the same configuration. You can use the graphical interface for a display change, then open the file to adjust a binding or window rule. Settings preserves comments and custom fields when saving. File changes reload automatically, and invalid edits leave the last working configuration active. This lets the graphical controls remain useful without making the configuration file a secondary or unsupported way to manage the desktop.

The shared settings have a defined scope. Ferese can coordinate the surfaces it draws and the transitions it manages; it does not control every animation an application draws inside its own window. That distinction matters when describing a consistent desktop: consistency between Ferese’s components should not be confused with taking over application behaviour.

[![Rosé Pine dark desktop with Settings, Control Center and a file manager.](images/screenshots/rose-pine-dark.webp)](images/screenshots/rose-pine-dark.webp)

## Different work needs different arrangements

A scrolling layout is useful when several windows need more space than can comfortably fit on one screen. Instead of shrinking everything to remain visible, the desktop can show part of a wider arrangement and move the viewport as focus changes. Tree tiling serves a different situation: the available screen is divided between windows so they can remain visible together.

Ferese supports both, with the layout selected per workspace. Floating windows are also available and remember their size and placement. A workspace can therefore use scrolling columns for a sequence of wide windows, while another uses tree tiling for a smaller set that needs to be viewed together.

Even within scrolling, moving focus does not always need to move the desktop. Ferese’s minimal focus strategy leaves a fully visible column where it is and scrolls only far enough to reveal a hidden edge. Other strategies centre the focused column or arrange columns into viewport-sized pages. These choices affect how much the desktop moves while you work, so they belong in the layout’s behaviour rather than being treated as visual decoration.

[![Ferese Blue overview with a terminal, Settings and three workspace previews.](images/screenshots/ferese-blue-overview.webp)](images/screenshots/ferese-blue-overview.webp)

## Movement that responds to what you do next

An animation often starts before you have finished deciding what to do. You might focus another window while the viewport is moving, reverse a fullscreen transition, or reopen a panel before it has finished closing. The desktop needs to handle that next input using the state already on screen.

Continuum, Ferese’s shared motion system, keeps a moving value’s current position, velocity and destination. Changing the destination can preserve the existing movement instead of restarting from rest. Direct manipulation follows a different rule: a dragged window follows the pointer directly, while a released gesture hands its velocity to a spring. The distinction keeps the animation from adding a trailing movement between your hand and the window.

This coordination extends beyond position. A window’s content, clipping and border need to follow the same presented bounds. When the application closes, its live surface may disappear before the visual exit finishes, so Ferese retains an image for that transition. When a shell panel closes, its content and compositor-drawn material need to leave together. Otherwise, the text can disappear while its background or blur remains. These are practical reasons for designing the motion system alongside the compositor and shell.

## Taking responsibility for the whole interaction

Including more of the desktop also means taking responsibility for more of its behaviour. A common theme does not resolve a late application buffer, and a correct spring equation does not make a missed frame arrive on time. Resize coordination, output timing and interrupted transitions still need testing, including measurements on the hardware where the desktop runs.

Ferese’s reason for existing is to work on those interactions together while keeping window management configurable. Choosing separate desktop components remains useful when that independence is what you want. Ferese takes responsibility for the components it includes, so changing the layout, adjusting the appearance or using an everyday desktop control does not also require maintaining the connections between them.
