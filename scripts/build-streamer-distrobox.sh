#!/usr/bin/env bash
# Build the KiraVR streamer inside a distrobox, for hosts with a read-only root
# (SteamOS) or without a native toolchain. Everything lives under $HOME.
#
# Usage: scripts/build-streamer-distrobox.sh [--nvidia] [--debug-build] [--skip-deps]
#
# Environment:
#   KIRAVR_BOX=name     Distrobox name (default: kiravr-build)
#   KIRAVR_IMAGE=image  Box image (default: Ubuntu 24.04; 22.04 gives an older glibc)
#   KIRAVR_JOBS=n       Parallel compile jobs (default: half the CPU threads,
#                       a quarter while SteamVR runs, so VR is not OOM-killed)
#   KIRAVR_DEBUG=1      Log every command, keep going past errors
#   NO_COLOR=1          Disable colored output (also off when not on a TTY)

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

BOX="${KIRAVR_BOX:-kiravr-build}"
IMAGE="${KIRAVR_IMAGE:-docker.io/library/ubuntu:24.04}"
NVIDIA=0
PROFILE="--release"
SKIP_DEPS=0

for arg in "$@"; do
    case "$arg" in
        --nvidia) NVIDIA=1 ;;
        --debug-build) PROFILE="" ;;
        --skip-deps) SKIP_DEPS=1 ;;
        -h|--help) sed -n 2,12p "$0"; exit 0 ;;
        *) echo "Unknown argument: $arg" >&2; exit 2 ;;
    esac
done

kiravr_init distrobox-build-streamer

APT_PACKAGES=(
    build-essential pkg-config nasm cmake curl git ca-certificates
    clang libclang-dev
    libva-dev libdrm-dev libvulkan-dev libx264-dev libasound2-dev libxrandr-dev
    libunwind-dev libgtk-3-dev libpipewire-0.3-dev libspa-0.2-dev libssl-dev
)

kiravr_ensure_box "$BOX" "$IMAGE" "${APT_PACKAGES[@]}"

JOBS="$(kiravr_jobs)"
info "Using $JOBS compile jobs (set KIRAVR_JOBS to change)"
in_box() { distrobox enter "$BOX" -- bash -lc "source \"\$HOME/.cargo/env\" && export CARGO_BUILD_JOBS=$JOBS && cd \"$KIRAVR_REPO_DIR\" && $*"; }

if [[ "$SKIP_DEPS" == "1" ]]; then
    warn "Skipping prepare-deps as requested"
elif [[ -d "$KIRAVR_REPO_DIR/deps/linux/ffmpeg" ]]; then
    ok "deps/linux already prepared (delete it to rebuild FFmpeg and x264)"
else
    info "Building FFmpeg and x264 (takes a while the first time)"
    if [[ "$NVIDIA" == "1" ]]; then
        in_box "cargo xtask prepare-deps --platform linux"
    else
        in_box "cargo xtask prepare-deps --platform linux --no-nvidia"
    fi
    ok "Dependencies built"
fi

info "Building the streamer ${PROFILE:-(debug profile)}"
in_box "cargo xtask build-streamer $PROFILE"
ok "Streamer built: $KIRAVR_REPO_DIR/build/alvr_streamer_linux"
bundle_client_apk
