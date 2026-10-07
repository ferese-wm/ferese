# Installation

Ferese runs on Linux as its own Wayland session alongside your existing desktop.
Launch it from a login manager, a command-based greeter or a local TTY.

## Requirements

For a full session, you’ll need a Wayland-capable graphics driver and a local user
session with seat access through logind or seatd. A nested preview runs inside your
existing Wayland desktop.

To build Ferese, install **Rust 1.95.0**, a C/C++ toolchain, CMake, pkg-config,
Python 3.9 or newer, and the libraries below. Once those packages are installed,
the installer builds the Ferese components.

### Fedora

```sh
sudo dnf install python3 git curl gcc gcc-c++ make cmake pkgconf-pkg-config \
  wayland-devel libxkbcommon-devel libinput-devel systemd-devel libseat-devel \
  mesa-libgbm-devel mesa-libEGL-devel libdrm-devel fontconfig-devel \
  freetype-devel expat-devel dbus-daemon dbus-tools desktop-file-utils foot pam \
  clang clang-devel pipewire-devel pipewire wireplumber polkit polkit-libs \
  xdg-desktop-portal xdg-desktop-portal-gtk \
  gstreamer1-devel gstreamer1-plugins-base gstreamer1-plugins-good pipewire-gstreamer
```

### Arch Linux

```sh
sudo pacman -S --needed python base-devel git curl cmake pkgconf wayland libxkbcommon \
  libinput systemd seatd mesa libdrm fontconfig freetype2 expat dbus \
  desktop-file-utils foot pam clang pipewire libpipewire wireplumber polkit \
  xdg-desktop-portal xdg-desktop-portal-gtk \
  gstreamer gst-plugins-base gst-plugins-good gst-plugin-pipewire
```

### Debian and Ubuntu

```sh
sudo apt update
sudo apt install python3 build-essential git curl cmake pkg-config libwayland-dev \
  libxkbcommon-dev libinput-dev libudev-dev libseat-dev libgbm-dev libegl-dev \
  libdrm-dev libfontconfig1-dev libfreetype-dev libexpat1-dev dbus-bin \
  dbus-user-session desktop-file-utils foot libpam0g clang libclang-dev \
  libpipewire-0.3-dev libspa-0.2-dev pipewire wireplumber polkitd libpolkit-agent-1-0 \
  xdg-desktop-portal xdg-desktop-portal-gtk \
  libgstreamer1.0-dev gstreamer1.0-pipewire gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good
```

Installation has been tested on Fedora. Package names or versions may differ on
other distributions; check the [Smithay
dependencies](https://github.com/Smithay/smithay#system-dependencies) for equivalent
libraries.

### X11 support (optional)

To run X11-only applications, Ferese launches
[xwayland-satellite](https://github.com/Supreeeme/xwayland-satellite),
which in turn needs an `Xwayland` binary. Both are runtime dependencies, not
build dependencies, and neither is needed to build Ferese.

- `Xwayland` ships with the X.Org server packages: `xorg-x11-server-Xwayland`
  on Fedora and Debian/Ubuntu, `xorg-xwayland` on Arch.
- `xwayland-satellite` is not packaged by most distributions. Build or install
  it from its upstream releases **with its `systemd` feature enabled**
  (`cargo build --release --features systemd`). That feature is what sends the
  `READY=1` readiness notification Ferese waits for. Upstream's default feature
  set is empty, so a default build never sends it, Ferese's fixed 10-second
  startup budget expires, and the service reports a startup failure instead of
  a display. A spawned process that is still alive is not readiness.

Both the compositor's default and the packaged configuration ship with X11
enabled, so a session with the two pieces above needs no configuration change.
If you maintain your own `config.kdl`, the block to keep is

```kdl
xwayland {
    enabled #true
    startup  on-demand
    path     "xwayland-satellite"
}
```

in `~/.config/ferese/config.kdl` and starting a new session, then check
`feresectl xwayland status`. See
[X11 support](configuration.md#x11-support-xwayland-satellite) for the full key
list, diagnostics, and known limitations.

#### Sandboxed applications

Ferese keeps the X11 cookie in a private file under `$XDG_RUNTIME_DIR`, and
applications inherit it through `XAUTHORITY`. Sandboxed packaging formats do not
all make that path visible, so an X11-only application may fail to start even
though `feresectl xwayland status` reports a running service.

Flatpak masks `/run/user/1000` with the application's own runtime directory, and
a `fallback-x11` permission is not granted inside a Wayland session. Both effects
deny the sandbox access to the display, so the application exits without a
window. Grant the socket explicitly:

```sh
flatpak override --user --socket=x11 com.spotify.Client
```

Snaps under strict confinement usually expose the real `/run/user/1000`, so a
snap that speaks X11 typically connects without extra configuration. That is a
confinement detail rather than a guarantee: if a snap application still cannot
reach the display, check whether its sandbox can read the `XAUTHORITY` path
reported by `feresectl xwayland status`.

### Rust toolchain

Install Rust through [rustup.rs](https://rustup.rs/). The checkout's
`rust-toolchain.toml` selects the required compiler and rustfmt automatically.
All workspace crates inherit their minimum Rust version from the root `Cargo.toml`.

From inside the checkout, verify the selected toolchain:

```sh
rustup show active-toolchain
cargo --version
```

## Build and install

Run these commands as your normal user:

```sh
git clone https://github.com/ferese-wm/ferese.git
cd ferese
./scripts/install.sh
```

The script builds locked release binaries as your normal user, then asks for
administrator access through `sudo` or `pkexec` to install them. Leave the build
itself unprivileged. The first build needs network access to fetch Rust dependencies.

The installer adds the session launcher, desktop tools, portal backend, login
entry, icons, default wallpaper and example config. Releases live under
`/usr/local/lib/ferese/releases/` and commands under `/usr/local/bin/`, with `current`
and `previous` links for switching between releases during updates and rollback.

The installer preserves your configuration, other desktop sessions and existing PAM
policy. Ferese starts its authentication agent and portal services with the session.
The authentication agent registers for the login session, so requests from desktop
apps use the Ferese dialog. Remove other polkit agents from your Ferese autostart
configuration to avoid competing session registrations.

### Initial configuration

You can start with Ferese’s built-in defaults, or copy the example config if you’d like
a file to customize:

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/ferese"
if [ ! -e "${XDG_CONFIG_HOME:-$HOME/.config}/ferese/config.kdl" ]; then
  cp /usr/local/lib/ferese/current/config.example.kdl \
    "${XDG_CONFIG_HOME:-$HOME/.config}/ferese/config.kdl"
fi
```

After login, open **Control Center → Settings** or run `ferese-settings` to configure
your desktop. See [Configuration](configuration.md) for themes, displays, shortcuts and
startup apps. The default terminal is `foot`, which you can replace with your preferred
terminal command in Settings.

## Start a session

### Graphical login managers

Save your work and log out, then open your login screen’s session selector, choose
**Ferese** and sign in. Login managers that support Wayland sessions can discover the
installed entry in `/usr/share/wayland-sessions/`.

If Ferese is missing, confirm that the session entry exists and its launcher is
executable:

```sh
ls -l /usr/share/wayland-sessions/ferese.desktop
test -x /usr/local/bin/ferese-session && echo "Session launcher is ready"
desktop-file-validate /usr/share/wayland-sessions/ferese.desktop
```

If the entry still doesn’t appear, check your login manager’s documentation for Wayland
session discovery, or use a Wayland-capable greeter or the TTY method below if your
manager only launches X11 sessions.

### Command-based greeters

If your greeter asks for a session command instead of reading desktop files, use:

```sh
/usr/local/bin/ferese-session
```

For greetd, set this as the session command after authentication; see the [greetd
documentation](https://sr.ht/~kennylevinsen/greetd/). The launcher starts the
compositor, shell and session services together.

### From a TTY

Log out of your graphical session and switch to a local console with **Ctrl+Alt+F2**, or
another available function key, then sign in as your normal user and run:

```sh
/usr/local/bin/ferese-session
```

Your login needs to provide `XDG_RUNTIME_DIR` and access to the active seat through
logind or seatd; if either is missing, fix your distribution’s login and session setup
rather than running Ferese as root or making device nodes writable. On non-systemd
systems, follow the distribution’s seatd and user-session instructions, and use a nested
preview whenever you’re already inside a graphical session.

## Optional desktop tools

Install these through your distribution when you want the corresponding features:

| Feature | Tools or services |
| --- | --- |
| Wallpaper file picker | `zenity` |
| Screenshots and editing | `slurp`, `satty`, `wl-clipboard`, Python 3 |
| Idle locking | `swayidle` with the included `ferese-lock` |
| Network controls | NetworkManager and its running service |
| Audio controls | PipeWire, WirePlumber, and `wpctl` |
| Bluetooth controls | BlueZ, `bluetoothctl`, and its running service |

Package names and availability vary by distribution. Use Satty’s [upstream
instructions](https://github.com/Satty-org/Satty#install) if it isn’t in your
repositories. Ferese leaves service setup to you and preserves your existing service
configuration during installation.

Use **Print Screen** to capture the active monitor, **Super+Shift+S** to select an area,
or `ferese-screenshot --all` to capture all monitors in one image.

The native locker follows your shell theme. Customize it in **Settings → Lock
Screen**, and run `ferese-lock --preview` to open an ordinary preview window. Before
enabling automatic locking, test real unlocking and read [Native locker](locking.md) for
authentication, idle locking and limits.

## Preview and logs

`ferese` selects a nested window when `WAYLAND_DISPLAY` or `DISPLAY` is set,
and DRM otherwise. Nested instances have separate control sockets. Clients
launched by Ferese inherit that instance’s `FERESE_SOCKET`. Use
`--backend=nested` or `--backend=drm` to override detection.

Nested previews leave the host activation environment and session target alone.
They skip XDG autostart and run configured autostart entries only with `nested true`.

To preview a source build before installing, run these commands from the checkout
inside an existing Wayland desktop:

```sh
cargo build --release --locked -p ferese -p ferese-shell -p ferese-settings -p feresectl -p ferese-lock -p xdg-desktop-portal-ferese
FERESE_ENABLE_SCREENCOPY=1 target/release/ferese \
  --grant-effects --grant-shell-control -- \
  target/release/ferese-shell
```

After installation, the equivalent launcher is:

```sh
ferese-session --nested
```

The session launcher saves logs in `${XDG_STATE_HOME:-$HOME/.local/state}/ferese/`,
while a source build launched directly writes to its terminal. Keep another desktop
available when testing scaling, monitor hotplug, suspend or locking so you have a way
back if something goes wrong.

The launcher enables screencopy for screenshot tools, allowing Wayland clients to
capture the unlocked desktop and making `feresectl screenshot` available. Set
`FERESE_ENABLE_SCREENCOPY=0` in the session environment to disable every screen-reading
path, including the built-in screenshot command, at your next login; capture is always
blocked while the session is locked.

When you launch the compositor directly, set `FERESE_ENABLE_SCREENCOPY=1` to enable
screenshots, since that variable is otherwise unset and screen capture stays disabled.

## Updating

From your existing checkout, with any local edits saved:

```sh
git pull --ff-only
./scripts/install.sh
```

Log out and back in to use the new release. A running session keeps its existing
processes and release paths until it restarts.

The installer prepares a complete release bundle before asking for administrator
access. Its manifest records the source commit, dirty-tree status, build features,
file modes and SHA-256 checksums. The installer verifies the bundle again after
copying it into a root-owned staging directory. These checks detect changed or incomplete
bundles; they are not a publisher signature.

The installer reports system installation and user setup separately. If user setup
fails after installation, retry it as your normal user:

```sh
python3 scripts/installer/install.py user-setup
```

This backs up the exact old development portal override when present and reloads
user systemd configuration. It preserves custom overrides and does not restart the
running desktop or portal.

Installer options:

```sh
./scripts/install.sh --dry-run
./scripts/install.sh --offline --release-id my-build
./scripts/install.sh --resize-metrics
./scripts/install.sh --bundle-only ./target/bundles/my-build --release-id my-build
./scripts/install.sh --bundle ./target/bundles/my-build
```

`--bundle` replaces `--skip-build` and installs a complete bundle instead of loose
files in `target/release/`. Build options cannot be combined with `--bundle`.
The front-end dry run prints commands. To inspect the filesystem changes planned
for an existing bundle, run:

```sh
./scripts/install-session.sh install ./target/bundles/my-build --dry-run
```

Modified administrator files stop installation before activation. To explicitly
replace Ferese's portal definition and preferences, use
`./scripts/install.sh --bundle ./target/bundles/my-build --replace-portal-config`.
Transaction backups live under `/usr/local/lib/ferese/transactions/`; each
`journal.json` maps the numbered backups to their original paths. Existing PAM
policies are preserved.

A lock prevents concurrent installations. The installer records recovery data
before changing system files, then updates integration files and switches `current`
last. If an operation fails, it restores the previous files. After an interruption,
recovery runs on the next install, or you can run it yourself:

```sh
sudo ./scripts/install-session.sh recover
```

Recovery stops if a recorded file was subsequently changed by an administrator.
Inspect the conflict and the retained `.transaction/journal.json` before restoring
the recorded old or new contents and retrying. Multi-file installation is
recoverable, not a single atomic filesystem operation.

## Logout and recovery

Save your work before leaving: **Super+Shift+E** and `feresectl request-logout` ask for
confirmation, while `feresectl exit` ends the session immediately. If Ferese freezes,
switch to another TTY with **Ctrl+Alt+F1–F12**, sign in and find the affected compositor
process:

```sh
pgrep -a -u "$USER" -x ferese
```

Replace `PID` with the affected session’s process ID and use `kill PID` to end it.
Its applications will close and unsaved work may be lost. You can
then choose another desktop at the next login.

### Roll back a release

Stop the Ferese session, then run from the checkout:

```sh
sudo ./scripts/install-session.sh rollback
# Or select a retained bundle by its release ID:
sudo ./scripts/install-session.sh rollback my-build
```

Rollback restores both the binaries and that release's session, systemd, portal
and icon files. It refuses to overwrite modified administrator files and preserves
existing PAM policy. Log in again to use the selected release. After rollback,
`previous` points to the release you just left.

Releases installed before bundle manifests can be upgraded using their retained
ownership records, but cannot be selected by this rollback command. Keep the old
release directory until you have validated the first bundled installation.

To remove managed commands and desktop integration while retaining release bundles,
transaction backups, PAM policy and user configuration:

```sh
sudo ./scripts/install-session.sh uninstall
```

### Remove the login option

To hide Ferese from login managers while keeping its releases and your config:

```sh
sudo mv /usr/share/wayland-sessions/ferese.desktop \
  /usr/local/lib/ferese/ferese.desktop.disabled
```

For a command-based greeter, remove Ferese from its session choices in the greeter’s
configuration; reinstalling Ferese restores the desktop session entry.

## Screen sharing and recording

The installer includes Ferese’s native ScreenCast portal and backs up the old user
service override pointing to `~/.local/libexec/ferese`, so new sessions use the
installed release. Custom overrides are preserved and reported before building.
See [Screen sharing](screen-sharing.md) for consent, PipeWire setup, supported sources
and recording limits.
