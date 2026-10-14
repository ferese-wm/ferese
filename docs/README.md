# Documentation

These guides describe the development version on `main`.
For the packaged **0.1.0-beta**, use the
[release notes](https://github.com/ferese-wm/ferese/releases/tag/v0.1.0-beta)
and [guides at that tag](https://github.com/ferese-wm/ferese/tree/v0.1.0-beta/docs).

## Usage

- [Installation](installation.md): install dependencies, log in, preview changes, update and recover a session.
- [Why Ferese](why-ferese.md): window layouts and a shared theme for the desktop.
- [Screenshots](screenshots.md): see the themes, overview and shell controls.
- [Live Activities](activities.md): show running commands in the panel, configure detection and troubleshoot missing tasks.
- [Shortcuts and gestures](shortcuts.md): keyboard navigation, moving and resizing with Super-drag, and window grouping.
- [Tabbed windows](tabbed-windows.md): group windows, select tabs and switch scrolling columns between tabs and rows.
- [Lock screen](locking.md): change its appearance, set up automatic locking and configure display sleep.
- [Screen sharing and recording](screen-sharing.md): choose a source and stop sharing.
- [Desktop portals](portals.md): services available to applications.
- [feresectl and IPC events](ipc.md): query the desktop, target a session and follow changes.

## Configuration

- [Introduction](configuration.md): KDL syntax, loading and validation.
- [Layout](configuration/layout.md).
- [Animations](configuration/animations.md).
- [Appearance](configuration/appearance.md).
- [Workspaces](configuration/workspaces.md).
- [Input](configuration/input.md).
- [Commands and bindings](configuration/bindings.md).
- [Window rules](configuration/window-rules.md).
- [Outputs](configuration/outputs.md).
- [Panels](configuration/panel.md).
- [Panel options and examples](panel-configuration.md).
- [Activities](configuration/activities.md).
- [Status controls](configuration/status.md).
- [Startup, locking and logout](configuration/startup-and-locking.md).
- [Idle inhibition](configuration/idle-inhibition.md).
- [Session protocols](configuration/session-protocols.md).
- [Xwayland](configuration/xwayland.md).

## Development

- [Packages and releases](packaging.md): build Fedora and Arch packages and publish a tagged release.
- [Development](development.md): build Ferese, run tests and check portal contracts.
- [Publishing activities](development/activities.md): IPC schema, ownership, limits and templates.
- [Animation model](animation-model.md): geometry, spring motion, gestures and prediction.
- [Rendering diagnostics](rendering-performance.md): outcome counters, capture cost and repeatable hardware baselines.
- [Output management tests](output-management-testing.md): profiles, hotplug and DRM failures.
- [Client soak testing](native-soak-testing.md).
- [Website development](../site/README.md).
- [Theme components](../crates/ferese-theme/README.md): shared native UI styles.
