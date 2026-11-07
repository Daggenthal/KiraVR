# Shared helpers for KiraVR scripts. Source it, do not run it.
#
# Provides colored output (only on a TTY, off with NO_COLOR=1), a crash header
# with a stack trace on errors, and KIRAVR_DEBUG=1 debug mode, which logs every
# command to build/<name>-<time>.log and keeps going past errors.

KIRAVR_REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KIRAVR_DEBUG="${KIRAVR_DEBUG:-0}"

if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
    RED=$'\e[31m'; BRED=$'\e[1;31m'; CYAN=$'\e[36m'; YELLOW=$'\e[33m'; GREEN=$'\e[32m'; BOLD=$'\e[1m'; RESET=$'\e[0m'
else
    RED=""; BRED=""; CYAN=""; YELLOW=""; GREEN=""; BOLD=""; RESET=""
fi

info()  { echo "${BOLD}==>${RESET} $*"; }
ok()    { echo "${GREEN}OK:${RESET} $*"; }
warn()  { echo "${YELLOW}Warning:${RESET} $*" >&2; }
error() { echo "${RED}Error:${RESET} $*" >&2; }

kiravr_on_error() {
    local code=$? line=$1
    echo "${BRED}*** Step failed (exit $code) at line $line ***${RESET}" >&2
    local i=0
    while caller $i >/dev/null 2>&1; do
        echo "${CYAN}  at $(caller $i)${RESET}" >&2
        i=$((i + 1))
    done
    if [[ "$KIRAVR_DEBUG" == "1" ]]; then
        warn "Debug mode: continuing past the error"
    else
        exit "$code"
    fi
}

# kiravr_init <log name>
kiravr_init() {
    set -uo pipefail
    if [[ "$KIRAVR_DEBUG" == "1" ]]; then
        local log="$KIRAVR_REPO_DIR/build/$1-$(date +%Y%m%d-%H%M%S).log"
        mkdir -p "$(dirname "$log")"
        exec > >(tee -a "$log") 2>&1
        set -x
        warn "Debug mode on, logging to $log"
    else
        set -e
    fi
    trap 'kiravr_on_error $LINENO' ERR
}

# Parallel jobs: half the CPU threads, a quarter while SteamVR runs. A full
# parallel build can use more than 10 GB, and on a 16 GB machine with a VR game
# running that got the game killed by the OOM killer.
kiravr_jobs() {
    local threads jobs
    threads="$(nproc)"
    if pgrep -x vrserver >/dev/null 2>&1; then
        jobs=$(( threads / 4 ))
        warn "SteamVR is running, building with fewer jobs to leave memory for VR" >&2
    else
        jobs=$(( threads / 2 ))
    fi
    echo "${KIRAVR_JOBS:-$(( jobs > 0 ? jobs : 1 ))}"
}

# kiravr_ensure_box <name> <image> <apt packages...>
kiravr_ensure_box() {
    local box="$1" image="$2"
    shift 2
    command -v distrobox >/dev/null || { error "distrobox not found. On SteamOS it ships by default; elsewhere install distrobox and podman."; exit 1; }
    if ! distrobox list 2>/dev/null | awk -F'|' '{gsub(/ /,"",$2); print $2}' | grep -qx "$box"; then
        info "Creating distrobox $box from $image"
        distrobox create --yes --name "$box" --image "$image"
        ok "Created $box"
    else
        ok "Distrobox $box already exists"
    fi
    info "Installing build packages (this needs sudo inside the box, not on the host)"
    distrobox enter "$box" -- bash -lc "sudo apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $*"
    ok "Packages installed"
    if ! distrobox enter "$box" -- bash -lc "command -v rustup" >/dev/null 2>&1; then
        info "Installing rustup to \$HOME/.cargo (toolchain version comes from rust-toolchain.toml)"
        distrobox enter "$box" -- bash -lc "curl --proto '=https' --tlsv1.2 -sSf --fail https://sh.rustup.rs | sh -s -- -y --default-toolchain none --no-modify-path"
    fi
    ok "rustup available"
}

# Copies the newest headset APK into the streamer build, where the dashboard's
# "Install on Headset" looks for it. Call after either build.
bundle_client_apk() {
    local streamer="$KIRAVR_REPO_DIR/build/alvr_streamer_linux"
    local apk
    apk="$(ls -t "$KIRAVR_REPO_DIR"/build/alvr_client_android/*.apk 2>/dev/null | head -1)"
    if [[ ! -d "$streamer" ]]; then
        info "No streamer build yet, skipping the APK bundle"
        return 0
    fi
    if [[ -z "$apk" ]]; then
        warn "No headset APK built yet: \"Install on Headset\" stays off until scripts/build-client-distrobox.sh runs"
        return 0
    fi
    mkdir -p "$streamer/share/alvr"
    cp "$apk" "$streamer/share/alvr/kiravr_client.apk"
    ok "Headset app bundled with the streamer ($(basename "$apk"))"
}
