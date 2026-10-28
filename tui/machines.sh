# The two machines tui/deploy and tui/screenshot (and router/build) work
# with, from tui/deploy.conf (git-ignored; start from deploy.conf.example),
# and helpers to run commands on them. Sourced, not run.

# This file's own folder, whichever script sourced it: bash names it in
# BASH_SOURCE, zsh in $0.
here=$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)
repo=$(dirname "$here")
conf="$here/deploy.conf"

if [ ! -f "$conf" ]; then
    echo "$(basename "$0"): no tui/deploy.conf; copy tui/deploy.conf.example and fill it in" >&2
    exit 2
fi
# shellcheck source=deploy.conf.example
. "$conf"
: "${BUILD_HOST:?set in tui/deploy.conf}" "${BUILD_DIR:?set in tui/deploy.conf}"
: "${SCREEN_HOST:?set in tui/deploy.conf}" "${SCREEN_DIR:?set in tui/deploy.conf}"
BUILD_SSH_OPTS=${BUILD_SSH_OPTS:-}
SCREEN_SSH_OPTS=${SCREEN_SSH_OPTS:-}

# A folder as the remote shell should see it inside double quotes: a leading
# ~/ becomes $HOME/, which expands there.
remote() {
    case "$1" in
        "~/"*) printf '$HOME/%s' "${1#"~/"}" ;;
        *) printf '%s' "$1" ;;
    esac
}
B=$(remote "$BUILD_DIR")
S=$(remote "$SCREEN_DIR")

# Word splitting of the option strings is intended.
# shellcheck disable=SC2086
on_build() { ssh -o BatchMode=yes $BUILD_SSH_OPTS "$BUILD_HOST" "$@"; }
# shellcheck disable=SC2086
on_screen() { ssh -o BatchMode=yes $SCREEN_SSH_OPTS "$SCREEN_HOST" "$@"; }

# macOS tar would add metadata files and headers that GNU tar warns about.
tar_flags=()
if tar --version 2>/dev/null | grep -q bsdtar; then
    export COPYFILE_DISABLE=1
    tar_flags=(--no-mac-metadata --no-xattrs)
fi
# The working tree, or the paths given, as a tar stream: without build
# output, caches, the app's local settings (ui.json), tui/deploy.conf or the
# kiosk's own screen-off window (tui/screen_off.local).
pack() {
    # The +-form keeps bash 3.2 (macOS) from calling an empty array unbound.
    (cd "$repo" && tar ${tar_flags[@]+"${tar_flags[@]}"} -cf - \
        --exclude ./.tools --exclude ./.venv --exclude ./PVEStats.app --exclude "./PVEStats Terminal.app" \
        --exclude ./captures --exclude ./ui.json --exclude ./icons \
        --exclude ./tui/target --exclude ./tui/.cargo-home --exclude ./tui/bin \
        --exclude ./tui/mac/.build --exclude ./tui/mac/.swiftpm \
        --exclude ./tui/shot/target --exclude ./tui/deploy.conf --exclude "./tui/screen_off.local*" \
        --exclude ./router/agent/target --exclude ./router/agent/.cargo-home --exclude ./router/bin \
        --exclude ./pve/agent/target --exclude ./pve/agent/.cargo-home --exclude ./pve/bin \
        --exclude ./nas/agent/target --exclude ./nas/agent/.cargo-home --exclude ./nas/bin \
        --exclude ./agent/rust/target --exclude ./agent/rust/.cargo-home --exclude ./agent/bin \
        --exclude ./server/target --exclude ./server/.cargo-home --exclude ./server/bin \
        --exclude __pycache__ --exclude .DS_Store --exclude '*.jpg' "$@")
}
