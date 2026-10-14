#!/bin/bash
set -euo pipefail
umask 077

usage() {
    cat <<'EOF'
Usage: scripts/install-macos.sh [--app ABSOLUTE_PATH] [--orbit-root ABSOLUTE_PATH] [--uninstall] [--no-build]

Install Orbit persistently into Parsec.app. The default app is
/Applications/Parsec.app and the default Orbit root is
~/Library/Application Support/ParsecOrbit.

Options:
  --app PATH         Parsec.app bundle path (absolute)
  --orbit-root PATH  user-owned Orbit state and plugin directory (absolute)
  --uninstall        restore the matching pristine app and keep Orbit data
  --no-build         use existing target/release artifacts
  -h, --help         show this help

Run as the signed-in ordinary user. The wrapper invokes only the installer
binary through /usr/bin/sudo; it never launches or stops Parsec.
EOF
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

app='/Applications/Parsec.app'
orbit_root=''
uninstall=0
no_build=0
while (($#)); do
    case "$1" in
        --app)
            (($# >= 2)) || fail '--app needs an absolute path'
            app=$2
            shift 2
            ;;
        --orbit-root)
            (($# >= 2)) || fail '--orbit-root needs an absolute path'
            orbit_root=$2
            shift 2
            ;;
        --uninstall)
            uninstall=1
            shift
            ;;
        --no-build)
            no_build=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

[[ "$(uname -s)" == Darwin ]] || fail 'this installer must run on macOS'
[[ "$(uname -m)" == arm64 ]] || fail 'this installer supports macOS arm64 only'
uid=$(id -u)
[[ "$uid" != 0 ]] || fail 'run this wrapper as your ordinary user, not root'
[[ "$app" == /* ]] || fail '--app must be an absolute path'
[[ "$app" != *$'\n'* ]] || fail '--app must not contain a newline'
[[ -n "${HOME:-}" && "$HOME" == /* ]] || fail 'HOME must be an absolute path'
if [[ -z "$orbit_root" ]]; then
    orbit_root="$HOME/Library/Application Support/ParsecOrbit"
fi
[[ "$orbit_root" == /* ]] || fail '--orbit-root must be an absolute path'
[[ "$orbit_root" != *$'\n'* ]] || fail '--orbit-root must not contain a newline'

root=$(cd "$(dirname "$0")/.." && pwd -P)
release="$root/target/release"
installer="$release/parsec-orbit-installer-macos"
loader="$release/libparsec_orbit_loader.dylib"
plugins_root="$orbit_root/plugins"
backup_root="$orbit_root/plugin-backups"
sudo_bin=/usr/bin/sudo
codesign_bin=/usr/bin/codesign
lipo_bin=/usr/bin/lipo

check_no_symlink_components() {
    local path=$1 component current
    local -a components
    [[ "$path" == /* ]] || return 1
    current=''
    IFS='/' read -r -a components <<< "$path"
    for component in "${components[@]}"; do
        [[ -z "$component" ]] && continue
        [[ "$component" != '.' && "$component" != '..' ]] || return 1
        current="$current/$component"
        [[ ! -L "$current" ]] || return 1
    done
}

check_no_symlink_components "$app" || fail "refusing a symlink or non-canonical app path: $app"
if [[ -e "$app" && ! -d "$app" ]]; then
    fail "app bundle path is not a directory: $app"
fi
# A recorded interrupted backup can leave this path temporarily absent.
# Let the Rust transaction recover it; an unrecorded missing app still refuses.
check_no_symlink_components "$orbit_root" || fail "refusing a symlink or non-canonical Orbit root: $orbit_root"
[[ -x "$sudo_bin" ]] || fail "missing required command: $sudo_bin"

mkdir -p "$orbit_root"
[[ "$(/usr/bin/stat -f %u "$orbit_root")" == "$uid" ]] || fail "Orbit root must belong to uid $uid"
chmod 700 "$orbit_root"
wrapper_lock="$orbit_root/.install-macos-lock"
mkdir "$wrapper_lock" 2>/dev/null || fail "another install/restore may be active; lock: $wrapper_lock (remove only after checking no installer is running)"
printf '%s\n' "$$" > "$wrapper_lock/pid"
release_wrapper_lock() {
    rm -f "$wrapper_lock/pid"
    rmdir "$wrapper_lock" 2>/dev/null || true
}
trap release_wrapper_lock EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

if ((uninstall)); then
    check_no_symlink_components "$installer" || fail "refusing a symlink in installer artifact path: $installer"
    [[ -x "$installer" && ! -L "$installer" ]] || fail "missing installer artifact: $installer (build once without --no-build)"
    mkdir -p "$orbit_root"
    check_no_symlink_components "$orbit_root" || fail "refusing a symlink in Orbit root: $orbit_root"
    [[ -d "$orbit_root" && -w "$orbit_root" ]] || fail "Orbit root is not writable by uid $uid: $orbit_root"
    [[ "$(/usr/bin/stat -f %u "$orbit_root")" == "$uid" ]] || fail "Orbit root must belong to uid $uid"
    chmod 700 "$orbit_root"
    log="$orbit_root/install-macos.log"
    [[ ! -L "$log" ]] || fail "refusing symlink log file: $log"
    if [[ -e "$log" ]]; then
        [[ -f "$log" && "$(/usr/bin/stat -f %u "$log")" == "$uid" && "$(/usr/bin/stat -f %l "$log")" == 1 ]] || fail "log must be an owned single-link regular file: $log"
    fi
    printf '[%s] Restoring Parsec.app at %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$app" | tee -a "$log"
    printf 'Orbit plugins and data will be kept under %s.\n' "$orbit_root" | tee -a "$log"
    if "$sudo_bin" "$installer" uninstall "$app" --orbit-root "$orbit_root" --sudo-user "$uid"; then
        printf '[%s] Restore completed. If Parsec.app was open, quit and reopen it manually.\n' "$(date '+%Y-%m-%d %H:%M:%S')" | tee -a "$log"
        exit 0
    else
        status=$?
        printf '[%s] Restore failed with status %s; see the installer output and Rust transaction journal.\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$status" | tee -a "$log" >&2
        exit "$status"
    fi
fi

plugins=(
    'parsec-orbit-plugin-config|com.parsec-orbit.config|libparsec_orbit_plugin_config.dylib'
    'parsec-orbit-plugin-hooks|com.parsec-orbit.hooks|libparsec_orbit_plugin_hooks.dylib'
    'parsec-orbit-plugin-js-runtime|com.parsec-orbit.js-runtime|libparsec_orbit_plugin_js_runtime.dylib'
)

if ((!no_build)); then
    command -v cargo >/dev/null 2>&1 || fail 'cargo is required to build the release artifacts'
    printf 'Building only the macOS installer, loader, and three official plugins.\n'
    (cd "$root" && cargo build --release --locked \
        -p parsec-orbit-installer-macos \
        -p parsec-orbit-loader-macos \
        -p parsec-orbit-plugin-config \
        -p parsec-orbit-plugin-hooks \
        -p parsec-orbit-plugin-js-runtime) || fail 'cargo build failed'
fi

for artifact in "$installer" "$loader"; do
    check_no_symlink_components "$artifact" || fail "refusing a symlink in release artifact path: $artifact"
    [[ -f "$artifact" && ! -L "$artifact" ]] || fail "missing or unsafe release artifact: $artifact"
done
[[ -x "$installer" ]] || fail "installer artifact is not executable: $installer"
[[ -x "$codesign_bin" ]] || fail "missing required command: $codesign_bin"
[[ -x "$lipo_bin" ]] || fail "missing required command: $lipo_bin"
"$lipo_bin" -verify_arch arm64 "$loader" || fail "loader has no arm64 slice: $loader"

for record in "${plugins[@]}"; do
    IFS='|' read -r crate plugin_id library <<< "$record"
    manifest="$root/crates/$crate/plugin.macos.toml"
    artifact="$release/$library"
    check_no_symlink_components "$manifest" || fail "refusing a symlink in manifest path: $manifest"
    check_no_symlink_components "$artifact" || fail "refusing a symlink in plugin artifact path: $artifact"
    [[ -f "$manifest" && ! -L "$manifest" ]] || fail "missing or unsafe macOS manifest: $manifest"
    [[ -f "$artifact" && ! -L "$artifact" ]] || fail "missing or unsafe release plugin: $artifact"
    "$lipo_bin" -verify_arch arm64 "$artifact" || fail "plugin has no arm64 slice: $artifact"
    grep -Fqx "id = \"$plugin_id\"" "$manifest" || fail "unexpected plugin id in $manifest"
    grep -Fqx "entry = \"$library\"" "$manifest" || fail "unexpected plugin entry in $manifest"
done

mkdir -p "$orbit_root"
check_no_symlink_components "$orbit_root" || fail "refusing a symlink in Orbit root: $orbit_root"
[[ -d "$orbit_root" && -w "$orbit_root" ]] || fail "Orbit root is not writable by uid $uid: $orbit_root"
[[ "$(/usr/bin/stat -f %u "$orbit_root")" == "$uid" ]] || fail "Orbit root must belong to uid $uid"
chmod 700 "$orbit_root"
log="$orbit_root/install-macos.log"
[[ ! -L "$log" ]] || fail "refusing symlink log file: $log"
if [[ -e "$log" ]]; then
    [[ -f "$log" && "$(/usr/bin/stat -f %u "$log")" == "$uid" && "$(/usr/bin/stat -f %l "$log")" == 1 ]] || fail "log must be an owned single-link regular file: $log"
fi
printf '[%s] Preparing persistent Parsec.app installation at %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$app" | tee -a "$log"

umask 077
stage=$(mktemp -d "$orbit_root/.install-macos.XXXXXX") || fail 'could not create private staging directory'
backup_dir=''
plugins_changed=0
deployed=()
rollback_plugins() {
    local i plugin_id destination backup
    for ((i=${#deployed[@]}-1; i>=0; i--)); do
        plugin_id=${deployed[$i]}
        destination="$plugins_root/$plugin_id"
        if [[ -d "$destination" && ! -L "$destination" ]]; then
            rm -rf "$destination" || printf 'error: could not remove failed deployment: %s\n' "$destination" >&2
        fi
        if [[ -n "$backup_dir" && -d "$backup_dir/$plugin_id" ]]; then
            mv "$backup_dir/$plugin_id" "$destination" || printf 'error: prior plugin remains at %s/%s\n' "$backup_dir" "$plugin_id" >&2
        fi
    done
}
cleanup() {
    local status=$?
    if ((plugins_changed)); then
        rollback_plugins
        printf '[%s] Deployment failed; attempted to restore prior plugin directories. Review %s for any recovery files.\n' \
            "$(date '+%Y-%m-%d %H:%M:%S')" "${backup_dir:-$orbit_root}" | tee -a "$log" >&2
    fi
    [[ -n "$stage" && -d "$stage" ]] && rm -rf "$stage"
    release_wrapper_lock
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

for record in "${plugins[@]}"; do
    IFS='|' read -r crate plugin_id library <<< "$record"
    source_manifest="$root/crates/$crate/plugin.macos.toml"
    source_library="$release/$library"
    staged="$stage/$plugin_id"
    mkdir "$staged"
    cp "$source_manifest" "$staged/plugin.toml"
    cp "$source_library" "$staged/$library"
    [[ -f "$staged/plugin.toml" && ! -L "$staged/plugin.toml" && -f "$staged/$library" && ! -L "$staged/$library" ]] || fail "staging failed for $plugin_id"
    "$codesign_bin" --force --sign - "$staged/$library"
    "$codesign_bin" --verify --strict --verbose=2 "$staged/$library"
    printf '[%s] Staged and strictly verified %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$plugin_id" | tee -a "$log"
done

check_no_symlink_components "$plugins_root" || fail "refusing a symlink in plugin path: $plugins_root"
if [[ -e "$plugins_root" ]]; then
    [[ -d "$plugins_root" ]] || fail "plugin path is not a directory: $plugins_root"
fi
check_no_symlink_components "$backup_root" || fail "refusing a symlink in plugin backup path: $backup_root"
mkdir -p "$plugins_root" "$backup_root"
backup_dir=$(mktemp -d "$backup_root/install-macos.XXXXXX") || fail 'could not create plugin backup directory'
plugins_changed=1

for record in "${plugins[@]}"; do
    IFS='|' read -r _ plugin_id _ <<< "$record"
    destination="$plugins_root/$plugin_id"
    [[ ! -L "$destination" ]] || fail "refusing symlink plugin directory: $destination"
    if [[ -e "$destination" ]]; then
        [[ -d "$destination" ]] || fail "plugin destination is not a directory: $destination"
        mv "$destination" "$backup_dir/$plugin_id"
    fi
    deployed+=("$plugin_id")
    mv "$stage/$plugin_id" "$destination"
    printf '[%s] Installed plugin %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$plugin_id" | tee -a "$log"
done

printf '[%s] Applying the bundle patch through the Rust installer.\n' "$(date '+%Y-%m-%d %H:%M:%S')" | tee -a "$log"
if ! "$sudo_bin" "$installer" apply "$app" --loader "$loader" --orbit-root "$orbit_root" --sudo-user "$uid"; then
    fail 'bundle installation failed; plugin directories will be rolled back'
fi
plugins_changed=0
printf '[%s] Installation completed. Previous plugin directories are retained at %s.\n' \
    "$(date '+%Y-%m-%d %H:%M:%S')" "$backup_dir" | tee -a "$log"
printf 'Quit and reopen Parsec manually to load Orbit. No app was launched or stopped.\n' | tee -a "$log"
rm -rf "$stage"
release_wrapper_lock
trap - EXIT HUP INT TERM
exit 0
