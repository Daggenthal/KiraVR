#!/usr/bin/env bash
# Build the KiraVR streamer inside a distrobox, for hosts with a read-only root
# (SteamOS) or without a native toolchain. Everything lives under $HOME.
#
# Usage: scripts/build-streamer-distrobox.sh [--nvidia] [--debug-build] [--skip-deps]
#
# Environment:
#   KIRAVR_BOX=name     Distrobox name (default: kiravr-build)
#   KIRAVR_JOBS=n       Parallel compile jobs (default: half the CPU threads,
#                       a quarter while SteamVR runs, so VR is not OOM-killed)
#   KIRAVR_DEBUG=1      Log every command, keep going past errors
#   NO_COLOR=1          Disable colored output (also off when not on a TTY)

set -uo pipefail

BOX="${KIRAVR_BOX:-kiravr-build}"
IMAGE="docker.io/library/ubuntu:24.04"
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEBUG="${KIRAVR_DEBUG:-0}"
NVIDIA=0
PROFILE="--release"
SKIP_DEPS=0

for arg in "$@"; do
    case "$arg" in
        --nvidia) NVIDIA=1 ;;
        --debug-build) PROFILE="" ;;
        --skip-deps) SKIP_DEPS=1 ;;
        -h|--help) sed -n 2,13p "$0"; exit 0 ;;
        *) echo "Unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# Colors only on a TTY and only when NO_COLOR is unset
if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
    RED=$'\e[31m'; BRED=$'\e[1;31m'; CYAN=$'\e[36m'; YELLOW=$'\e[33m'; GREEN=$'\e[32m'; BOLD=$'\e[1m'; RESET=$'\e[0m'
else
    RED=""; BRED=""; CYAN=""; YELLOW=""; GREEN=""; BOLD=""; RESET=""
fi

info()  { echo "${BOLD}==>${RESET} $*"; }
ok()    { echo "${GREEN}OK:${RESET} $*"; }
warn()  { echo "${YELLOW}Warning:${RESET} $*" >&2; }
error() { echo "${RED}Error:${RESET} $*" >&2; }

on_error() {
    local code=$? line=$1
    echo "${BRED}*** Build step failed (exit $code) at line $line ***${RESET}" >&2
    local i=0
    while caller $i >/dev/null 2>&1; do
        echo "${CYAN}  at $(caller $i)${RESET}" >&2
        i=$((i + 1))
    done
    if [[ "$DEBUG" == "1" ]]; then
        warn "Debug mode: continuing past the error"
    else
        exit "$code"
    fi
}

if [[ "$DEBUG" == "1" ]]; then
    LOG="$REPO_DIR/build/distrobox-build-$(date +%Y%m%d-%H%M%S).log"
    mkdir -p "$(dirname "$LOG")"
    exec > >(tee -a "$LOG") 2>&1
    set -x
    warn "Debug mode on, logging to $LOG"
else
    set -e
fi
trap 'on_error $LINENO' ERR

APT_PACKAGES=(
    build-essential pkg-config nasm cmake curl git ca-certificates
    clang libclang-dev
    libva-dev libdrm-dev libvulkan-dev libx264-dev libasound2-dev libxrandr-dev
    libunwind-dev libgtk-3-dev libpipewire-0.3-dev libspa-0.2-dev libssl-dev
)

in_box() { distrobox enter "$BOX" -- bash -lc "$*"; }

command -v distrobox >/dev/null || { error "distrobox not found. On SteamOS it ships by default; elsewhere install distrobox and podman."; exit 1; }

if ! distrobox list 2>/dev/null | awk -F'|' '{gsub(/ /,"",$2); print $2}' | grep -qx "$BOX"; then
    info "Creating distrobox $BOX from $IMAGE"
    distrobox create --yes --name "$BOX" --image "$IMAGE"
    ok "Created $BOX"
else
    ok "Distrobox $BOX already exists"
fi

info "Installing build packages (this needs sudo inside the box, not on the host)"
in_box "sudo apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ${APT_PACKAGES[*]}"
ok "Packages installed"

if ! in_box "command -v rustup" >/dev/null 2>&1; then
    info "Installing rustup to \$HOME/.cargo (toolchain version comes from rust-toolchain.toml)"
    in_box "curl --proto '=https' --tlsv1.2 -sSf --fail https://sh.rustup.rs | sh -s -- -y --default-toolchain none --no-modify-path"
fi
ok "rustup available"

# A full parallel build of FFmpeg plus the Rust workspace can use more than
# 10 GB. On a 16 GB Steam Machine with a VR game running, that got the game
# killed by the OOM killer, so default to a modest job count.
THREADS="$(nproc)"
if pgrep -x vrserver >/dev/null 2>&1; then
    DEFAULT_JOBS=$(( THREADS / 4 ))
    warn "SteamVR is running, building with fewer jobs to leave memory for VR"
else
    DEFAULT_JOBS=$(( THREADS / 2 ))
fi
JOBS="${KIRAVR_JOBS:-$(( DEFAULT_JOBS > 0 ? DEFAULT_JOBS : 1 ))}"
info "Using $JOBS compile jobs (set KIRAVR_JOBS to change)"

CARGO='source "$HOME/.cargo/env" && export CARGO_BUILD_JOBS='"$JOBS"' && cd "'"$REPO_DIR"'"'

if [[ "$SKIP_DEPS" == "1" ]]; then
    warn "Skipping prepare-deps as requested"
elif [[ -d "$REPO_DIR/deps/linux/ffmpeg" ]]; then
    ok "deps/linux already prepared (delete it to rebuild FFmpeg and x264)"
else
    info "Building FFmpeg and x264 (takes a while the first time)"
    if [[ "$NVIDIA" == "1" ]]; then
        in_box "$CARGO && cargo xtask prepare-deps --platform linux"
    else
        in_box "$CARGO && cargo xtask prepare-deps --platform linux --no-nvidia"
    fi
    ok "Dependencies built"
fi

info "Building the streamer ${PROFILE:-(debug profile)}"
in_box "$CARGO && cargo xtask build-streamer $PROFILE"
ok "Streamer built: $REPO_DIR/build/alvr_streamer_linux"
