# Packages and releases

Ferese ships as one package. It includes the compositor, shell, Settings, lock
screen, authentication agent, recording helper and desktop portal.

The release workflow builds an x86_64 RPM on Fedora 44 and an x86_64 Arch package.
COPR and AUR publication are separate steps; neither repository is set up by this
workflow.

## Install a release package

Download the package for your distribution from
[GitHub Releases](https://github.com/ferese-wm/ferese/releases), then install it:

```sh
# Fedora: select the ferese RPM, not the debuginfo or source RPM.
sudo dnf install ./ferese-<version>-1.fc44.x86_64.rpm

# Arch Linux
sudo pacman -U ./ferese-<version>-1-x86_64.pkg.tar.zst
```

Select Ferese in your login manager, or run `ferese-session` from a local TTY.
The package does not replace your login manager or modify your personal config.
To start with the supplied config:

```sh
mkdir -p ~/.config/ferese
cp -n /usr/libexec/ferese/config.example.kdl ~/.config/ferese/config.kdl
```

If you installed with `install.sh`, uninstall that copy before installing a
package. Its `/usr/local/bin` launchers take precedence over `/usr/bin`, and its
portal service can override the packaged service. From the original checkout:

```sh
sudo ./scripts/install-session.sh uninstall
```

Keep your personal config. Log out before changing the installed session, then
log back in once the package is installed.

## Build packages locally

With Python 3.11 or newer and the pinned Rust toolchain installed, prepare the
source archive and versioned recipes from a checkout:

```sh
python3 scripts/package.py source
```

This downloads the locked Rust dependencies and includes an offline Cargo cache.
Ferese uses two packages with the same name and version from different sources,
which `cargo vendor` cannot represent. The cache preserves those source identities
and includes the Git submodules needed by libcosmic. It contains no Cargo
credentials or user-level Cargo configuration.

The resulting files are in `target/dist`: a source archive, Fedora spec, Arch
PKGBUILD and source checksum. Package builds run offline. Dependency license
notices and a JSON report of their sources and SPDX licenses ship with the package.

### Fedora

```sh
sudo dnf install rpm-build dnf5-plugins
sudo dnf builddep target/dist/ferese.spec
mkdir -p ~/rpmbuild/SOURCES
cp target/dist/ferese-*.tar.gz ~/rpmbuild/SOURCES/
rpmbuild -ba target/dist/ferese.spec
```

The binary RPMs are in `~/rpmbuild/RPMS`; the source RPM is in `~/rpmbuild/SRPMS`.
Build with `--with resize_metrics` to enable resize-barrier measurements.
The source RPM can also be submitted to COPR once a project is configured.

### Arch Linux

Build as a regular user with the dependencies listed in the generated PKGBUILD:

```sh
cd target/dist
makepkg -s
```

For a development build, copy `packaging/arch/ferese-git/PKGBUILD` into an empty
build directory and run `makepkg -s`. It tracks `main` and conflicts with the
release package. Submit that recipe to AUR separately if you want AUR distribution.

## Publish a release

1. Set `workspace.package.version` in `Cargo.toml` and update `Cargo.lock`.
2. Write `docs/releases/v<version>.md`. The workflow uses this file as the release
   notes. Commit the release changes and push them.
3. Tag that commit with the same version, prefixed by `v`, then push the tag:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

A tag/version mismatch fails before packaging. Tags with a prerelease suffix,
such as `v0.2.0-rc.1`, produce a GitHub prerelease.

The workflow builds and tests both packages before publishing. Release assets
include the offline source archive, binary and source RPMs, Arch package,
PKGBUILD, `.SRCINFO`, Fedora spec and SHA-256 checksums. A failed upload leaves a
draft; rerunning the workflow can finish that draft. Published releases are not
overwritten.

Label a release preparation PR `release` to run the package builds before
merging. You can also use **Run workflow** to test a branch without publishing
or creating a tag. Fedora and Arch jobs run the workspace tests under a private
D-Bus session and validate the installed desktop files. They do not test real
DRM hardware; check login, locking, authentication, screen sharing and recording
on each distribution before announcing a release.
