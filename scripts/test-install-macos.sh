#!/bin/bash
set -euo pipefail

source_root=$(cd "$(dirname "$0")/.." && pwd -P)
tmp=$(mktemp -d "/private/tmp/parsec-orbit-install-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

assert_file_contains() {
    local file=$1 text=$2
    grep -Fq -- "$text" "$file" || fail "expected '$text' in $file"
}

assert_file_not_contains() {
    local file=$1 text=$2
    if grep -Fq -- "$text" "$file"; then
        fail "did not expect '$text' in $file"
    fi
}

fixture="$tmp/repository with spaces"
mock_bin="$tmp/mock-bin"
mkdir -p "$fixture/scripts" "$fixture/crates" "$fixture/target/release" "$mock_bin"
cp "$source_root/scripts/install-macos.sh" "$fixture/scripts/install-macos.sh"
for crate in parsec-orbit-plugin-config parsec-orbit-plugin-hooks parsec-orbit-plugin-js-runtime; do
    mkdir -p "$fixture/crates/$crate"
    cp "$source_root/crates/$crate/plugin.macos.toml" "$fixture/crates/$crate/plugin.macos.toml"
done
touch "$fixture/target/release/libparsec_orbit_loader.dylib"
cat > "$fixture/target/release/parsec-orbit-installer-macos" <<'EOF'
#!/bin/bash
printf '%s\n' "$*" >> "$INSTALLER_LOG"
exit "${INSTALLER_EXIT:-0}"
EOF
chmod +x "$fixture/target/release/parsec-orbit-installer-macos"
for library in \
    libparsec_orbit_plugin_config.dylib \
    libparsec_orbit_plugin_hooks.dylib \
    libparsec_orbit_plugin_js_runtime.dylib; do
    touch "$fixture/target/release/$library"
done

cat > "$mock_bin/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) printf 'Darwin\n';; -m) printf 'arm64\n';; esac
EOF
cat > "$mock_bin/codesign" <<'EOF'
#!/bin/bash
printf '%s\n' "$*" >> "$CODESIGN_LOG"
if [[ "$1" == --force ]]; then
    touch "$4.signed-by-mock"
elif [[ "$1" == --verify ]]; then
    [[ -f "$4.signed-by-mock" ]] || exit 9
else
    exit 10
fi
EOF
cat > "$mock_bin/sudo" <<'EOF'
#!/bin/bash
printf '%s\n' "$*" >> "$SUDO_LOG"
exec "$@"
EOF
cat > "$mock_bin/lipo" <<'EOF'
#!/bin/sh
# Xcode's lipo consumes every argument after -verify_arch as an architecture.
[ "$#" -eq 3 ] && [ -f "$1" ] && [ "$2" = -verify_arch ] && [ "$3" = arm64 ] || exit 70
exit "${LIPO_EXIT:-0}"
EOF
chmod +x "$mock_bin/lipo" "$mock_bin/uname" "$mock_bin/codesign" "$mock_bin/sudo"

sed -i.bak \
    -e "s|sudo_bin=/usr/bin/sudo|sudo_bin=$mock_bin/sudo|" \
    -e "s|codesign_bin=/usr/bin/codesign|codesign_bin=$mock_bin/codesign|" \
    -e "s|lipo_bin=/usr/bin/lipo|lipo_bin=$mock_bin/lipo|" \
    "$fixture/scripts/install-macos.sh"
rm -f "$fixture/scripts/install-macos.sh.bak"

app="$tmp/Parsec App.app"
mkdir -p "$app"
export PATH="$mock_bin:$PATH"
export INSTALLER_LOG="$tmp/installer.log"
export CODESIGN_LOG="$tmp/codesign.log"
export SUDO_LOG="$tmp/sudo.log"
install_test_home="$tmp/user home"
mkdir -p "$install_test_home"
uid=$(id -u)

success_root="$install_test_home/Library/Application Support/ParsecOrbit success"
mkdir -p "$success_root/plugins/com.parsec-orbit.config" "$success_root/plugins/third-party.keep"
printf 'old config payload\n' > "$success_root/plugins/com.parsec-orbit.config/old.txt"
printf 'unmanaged payload\n' > "$success_root/plugins/third-party.keep/keep.txt"
"$fixture/scripts/install-macos.sh" --app "$app" --orbit-root "$success_root" --no-build
[[ $(wc -l < "$CODESIGN_LOG" | tr -d ' ') == 6 ]] || fail 'expected sign and strict-verify calls for three plugins'
assert_file_contains "$CODESIGN_LOG" '--force --sign -'
assert_file_contains "$CODESIGN_LOG" '--verify --strict --verbose=2'
assert_file_contains "$INSTALLER_LOG" "apply $app --loader $fixture/target/release/libparsec_orbit_loader.dylib --orbit-root $success_root --sudo-user $uid"
[[ -f "$success_root/plugins/com.parsec-orbit.config/plugin.toml" ]] || fail 'config manifest was not deployed'
[[ -f "$success_root/plugins/com.parsec-orbit.hooks/libparsec_orbit_plugin_hooks.dylib" ]] || fail 'hooks library was not deployed'
[[ -f "$success_root/plugins/com.parsec-orbit.js-runtime/plugin.toml" ]] || fail 'JS runtime manifest was not deployed'
[[ -f "$success_root/plugins/third-party.keep/keep.txt" ]] || fail 'unmanaged plugin was changed'
backup=$(find "$success_root/plugin-backups" -type f -name old.txt -print -quit)
[[ -n "$backup" ]] || fail 'replaced plugin directory was not retained in backup'

rollback_root="$install_test_home/Library/Application Support/ParsecOrbit rollback"
mkdir -p "$rollback_root/plugins/com.parsec-orbit.config" "$rollback_root/plugins/com.parsec-orbit.hooks"
printf 'prior config\n' > "$rollback_root/plugins/com.parsec-orbit.config/prior.txt"
printf 'prior hooks\n' > "$rollback_root/plugins/com.parsec-orbit.hooks/prior.txt"
export INSTALLER_EXIT=23
if "$fixture/scripts/install-macos.sh" --app "$app" --orbit-root "$rollback_root" --no-build >"$tmp/rollback.out" 2>&1; then
    fail 'expected mocked installer failure'
fi
[[ -f "$rollback_root/plugins/com.parsec-orbit.config/prior.txt" ]] || fail 'config plugin was not restored after failure'
[[ -f "$rollback_root/plugins/com.parsec-orbit.hooks/prior.txt" ]] || fail 'hooks plugin was not restored after failure'
[[ ! -e "$rollback_root/plugins/com.parsec-orbit.js-runtime" ]] || fail 'new JS runtime plugin remained after failure'
assert_file_contains "$tmp/rollback.out" 'plugin directories will be rolled back'
unset INSTALLER_EXIT

data_file="$success_root/data/keep.json"
mkdir -p "$(dirname "$data_file")"
printf '{"retain":true}\n' > "$data_file"
"$fixture/scripts/install-macos.sh" --app "$app" --orbit-root "$success_root" --uninstall --no-build
assert_file_contains "$INSTALLER_LOG" "uninstall $app --orbit-root $success_root --sudo-user $uid"
[[ -f "$data_file" ]] || fail 'uninstall removed Orbit user data'

before_sudo=$(wc -l < "$SUDO_LOG" | tr -d ' ')
if "$fixture/scripts/install-macos.sh" --app "$app" --orbit-root "$success_root" --no-build --unexpected >"$tmp/unknown.out" 2>&1; then
    fail 'unknown option was accepted'
fi
after_sudo=$(wc -l < "$SUDO_LOG" | tr -d ' ')
[[ "$before_sudo" == "$after_sudo" ]] || fail 'unknown option reached sudo'

ln -s "$app" "$tmp/linked Parsec.app"
if "$fixture/scripts/install-macos.sh" --app "$tmp/linked Parsec.app" --orbit-root "$success_root" --no-build >"$tmp/symlink.out" 2>&1; then
    fail 'symlink app path was accepted'
fi
assert_file_contains "$tmp/symlink.out" 'refusing a symlink or non-canonical app path'
after_sudo=$(wc -l < "$SUDO_LOG" | tr -d ' ')
[[ "$before_sudo" == "$after_sudo" ]] || fail 'symlink app path reached sudo'

mkdir "$success_root/.install-macos-lock"
if "$fixture/scripts/install-macos.sh" --app "$app" --orbit-root "$success_root" --no-build >"$tmp/lock.out" 2>&1; then
    fail 'concurrent wrapper lock was ignored'
fi
[[ $(wc -l < "$SUDO_LOG" | tr -d ' ') == "$before_sudo" ]] || fail 'concurrent wrapper reached sudo'
rmdir "$success_root/.install-macos-lock"

export LIPO_EXIT=1
if "$fixture/scripts/install-macos.sh" --app "$app" --orbit-root "$success_root" --no-build >"$tmp/arch.out" 2>&1; then
    fail 'a loader without arm64 was accepted'
fi
[[ $(wc -l < "$SUDO_LOG" | tr -d ' ') == "$before_sudo" ]] || fail 'invalid architecture reached sudo'
unset LIPO_EXIT

printf 'PASS: macOS installer shell protocol\n'
