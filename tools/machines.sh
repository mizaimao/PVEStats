# The two machines the scripts here work with, from tui/deploy.conf
# (git-ignored; start from tui/deploy.conf.example), and helpers to run
# commands on them and to build there. Sourced, not run, by tui/deploy,
# tui/screenshot, tui/kiosk and each agent's build script. A script needs
# only the settings of the machine it uses: a build needs no screen machine.

# This file's own folder, whichever script sourced it: bash names it in
# BASH_SOURCE, zsh in $0.
here=$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)
repo=$(dirname "$here")
conf="$repo/tui/deploy.conf"

if [ ! -f "$conf" ]; then
    echo "$(basename "$0"): no tui/deploy.conf; copy tui/deploy.conf.example and fill it in" >&2
    exit 2
fi
# shellcheck source=../tui/deploy.conf.example
. "$conf"
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
B=$(remote "${BUILD_DIR:-}")
S=$(remote "${SCREEN_DIR:-}")

# Each machine's settings are checked when it is first used.
# Word splitting of the option strings is intended.
# shellcheck disable=SC2086
on_build() {
    : "${BUILD_HOST:?set in tui/deploy.conf}" "${BUILD_DIR:?set in tui/deploy.conf}"
    ssh -o BatchMode=yes $BUILD_SSH_OPTS "$BUILD_HOST" "$@"
}
# shellcheck disable=SC2086
on_screen() {
    : "${SCREEN_HOST:?set in tui/deploy.conf}" "${SCREEN_DIR:?set in tui/deploy.conf}"
    ssh -o BatchMode=yes $SCREEN_SSH_OPTS "$SCREEN_HOST" "$@"
}

# macOS tar would add metadata files and headers that GNU tar warns about.
tar_flags=()
if tar --version 2>/dev/null | grep -q bsdtar; then
    export COPYFILE_DISABLE=1
    tar_flags=(--no-mac-metadata --no-xattrs)
fi
# The working tree, or the paths given, as a tar stream: without build
# output and caches (any target, .cargo-home, .build or .swiftpm folder, and
# each part's bin), the apps, the app's local settings (ui.json),
# tui/deploy.conf or the kiosk's own screen-off window (tui/screen_off.local).
pack() {
    # The +-form keeps bash 3.2 (macOS) from calling an empty array unbound.
    (cd "$repo" && tar ${tar_flags[@]+"${tar_flags[@]}"} -cf - \
        --exclude target --exclude .cargo-home --exclude .build --exclude .swiftpm \
        --exclude './*/bin' --exclude '*.app' \
        --exclude ./.tools --exclude ./.venv --exclude ./captures --exclude ./ui.json --exclude ./icons \
        --exclude ./tui/deploy.conf --exclude "./tui/screen_off.local*" \
        --exclude __pycache__ --exclude .DS_Store --exclude '*.jpg' "$@")
}

# Copies the paths given (from the repo's root, e.g. ./pve) to BUILD_DIR on
# the build machine.
send_to_build() {
    on_build "mkdir -p \"$B\""
    pack "$@" | on_build "tar -xf - -C \"$B\""
}

# remote_cargo DIR [NAME=VALUE...] [ARG...]
# On the build machine, in DIR under BUILD_DIR (sent there first), with
# cargo's cache in DIR/.cargo-home: the tests, then a release build with the
# ARGs, the NAME=VALUEs set for the build alone. A failing test stops it.
# REMOTE_CARGO_TESTS=no builds without the tests.
remote_cargo() {
    local dir=$1 env="" args="" tests="cargo test --release --quiet &&"
    shift
    while [ $# -gt 0 ] && [[ $1 =~ ^[A-Z_][A-Z0-9_]*= ]]; do
        env+="$(printf '%q ' "$1")"
        shift
    done
    if [ $# -gt 0 ]; then
        args=$(printf '%q ' "$@")
    fi
    if [ "${REMOTE_CARGO_TESTS:-yes}" = no ]; then
        tests=""
    fi
    on_build "cd \"$B/$dir\" && export CARGO_HOME=\"\$PWD/.cargo-home\" PATH=\"\$HOME/.cargo/bin:\$PATH\" \
        && $tests ${env}cargo build --release --quiet $args"
}

# fetch_binary REMOTE LOCAL
# A program built on the build machine (REMOTE, under BUILD_DIR) to LOCAL in
# this repo, written beside it first and then renamed into place, so a
# failed copy leaves the old one whole.
fetch_binary() {
    local remote_path=$1 local_path="$repo/$2"
    mkdir -p "$(dirname "$local_path")"
    on_build "cat \"$B/$remote_path\"" >"$local_path.new"
    chmod 755 "$local_path.new"
    mv "$local_path.new" "$local_path"
    echo "== built $2 ($(wc -c <"$local_path" | tr -d ' ') bytes)"
}

# The newest glibc symbol version a program built on the build machine
# (under BUILD_DIR) asks for: the oldest glibc it runs with.
glibc_needed() {
    on_build "objdump -T \"$B/$1\" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -1"
}
