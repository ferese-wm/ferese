#!/usr/bin/env bash
# Build and prepare a complete release as the user; elevate only installation.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/install.sh [options]

Build, bundle and install Ferese. Run as your normal user.

  --release-id ID          Release name (default: UTC timestamp + PID)
  --bundle PATH            Install an existing verified bundle; do not build
  --bundle-only PATH       Build a bundle at PATH without installing
  --replace-portal-config  Back up and replace modified Ferese portal config
  --offline               Build using cached Cargo dependencies
  --resize-metrics        Build with resize-barrier measurements
  --no-activities         Skip automatic shell hook and Activities setup
  --dry-run               Show commands without building or installing
  -h, --help              Show this help

Recovery and rollback: scripts/install-session.sh --help
Repeat user setup: python3 scripts/installer/install.py user-setup
EOF
}

fail() { echo "ferese installer: $*" >&2; exit 1; }
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
helper=$repo_dir/scripts/installer/install.py
release_id="$(date -u +%Y%m%d-%H%M%S)-$$"
release_set=false
bundle_path=
bundle_only=
replace_portal_config=false
offline=false
resize_metrics=false
dry_run=false
no_activities=false
while (($#)); do
    case $1 in
        --release-id|--bundle|--bundle-only)
            (($# >= 2)) && [[ -n $2 && $2 != --* ]] || fail "$1 requires a value"
            case $1 in
                --release-id) release_id=$2; release_set=true ;;
                --bundle) bundle_path=$2 ;;
                --bundle-only) bundle_only=$2 ;;
            esac
            shift 2 ;;
        --replace-portal-config) replace_portal_config=true; shift ;;
        --offline) offline=true; shift ;;
        --resize-metrics) resize_metrics=true; shift ;;
        --no-activities) no_activities=true; shift ;;
        --dry-run) dry_run=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown option: $1 (see --help)" ;;
    esac
done
[[ $release_id =~ ^[a-zA-Z0-9][a-zA-Z0-9._-]*$ ]] || fail 'invalid release ID'
[[ $(uname -s) == Linux ]] || fail 'Ferese requires Linux'
command -v python3 >/dev/null || fail 'install Python 3 first'
if [[ -n $bundle_path ]]; then
    [[ -z $bundle_only ]] && ! $offline && ! $resize_metrics && ! $release_set ||
        fail '--bundle cannot be combined with build options'
else
    ((EUID != 0)) || fail 'build as your normal user, without sudo'
fi
if [[ -n $bundle_only ]] && $replace_portal_config; then
    fail '--replace-portal-config requires installation'
fi

build=(cargo build --manifest-path "$repo_dir/Cargo.toml" --target-dir "$repo_dir/target" --release --locked)
mapfile -t packages < <(python3 "$helper" packages)
((${#packages[@]})) || fail 'could not read package inventory'
for package in "${packages[@]}"; do build+=(-p "$package"); done
if $offline; then build+=(--offline); fi
features=default
if $resize_metrics; then
    build+=(--features ferese/resize-metrics)
    features=ferese/resize-metrics
fi

if [[ -z $bundle_path ]]; then
    bundle_path=${bundle_only:-$repo_dir/target/bundles/$release_id}
    prepare=(python3 "$helper" bundle --output "$bundle_path" --release-id "$release_id" --features "$features")
fi
installer=(/usr/bin/bash "$repo_dir/scripts/install-session.sh" install "$bundle_path")
if $replace_portal_config; then installer+=(--replace-portal-config); fi
if [[ -z $bundle_only ]] && ((EUID != 0)); then
    if command -v sudo >/dev/null; then
        installer=(sudo "${installer[@]}")
    elif command -v pkexec >/dev/null; then
        installer=(pkexec "${installer[@]}")
    else
        fail 'system installation requires sudo or pkexec'
    fi
fi

if $dry_run; then
    if [[ ${prepare+x} ]]; then
        printf 'Build: '; printf '%q ' "${build[@]}"; printf '\n'
        printf 'Bundle: '; printf '%q ' "${prepare[@]}"; printf '\n'
    fi
    if [[ -z $bundle_only ]]; then printf 'Install: '; printf '%q ' "${installer[@]}"; printf '\n'; fi
    exit 0
fi

user_setup=(python3 "$helper" user-setup)
if $no_activities; then user_setup+=(--no-activities); fi
if [[ -z $bundle_only ]] && ((EUID != 0)); then
    "${user_setup[@]}" --check
fi
if [[ ${prepare+x} ]]; then
    for tool in cargo desktop-file-validate dbus-run-session; do
        command -v "$tool" >/dev/null || fail "required command not found: $tool"
    done
    desktop-file-validate "$repo_dir/packaging/ferese.desktop"
    desktop-file-validate "$repo_dir/packaging/dev.ferese.Settings.desktop"
    (cd -- "$repo_dir"; "${build[@]}")
    "$repo_dir/target/release/ferese-polkit-agent" --check
    "${prepare[@]}"
fi
[[ -z $bundle_only ]] || exit 0

python3 "$helper" verify "$bundle_path"
"${installer[@]}"
if ((EUID != 0)); then
    if ! "${user_setup[@]}"; then
        echo 'System installation succeeded; user setup is incomplete.' >&2
        echo "Retry as your normal user: python3 '$helper' user-setup" >&2
        exit 1
    fi
else
    echo "System installation succeeded. As your normal user, run: python3 '$helper' user-setup"
fi
