#!/usr/bin/env bash
# Build the KiraVR Android client (APK) inside a distrobox. Installs a JDK in the
# box and the Android SDK and NDK under $HOME, matching what CI uses.
#
# Usage: scripts/build-client-distrobox.sh [--release] [--install]
#   --release   Optimized build (default is a debug build)
#   --install   adb install the APK on a connected headset afterwards
#
# Environment:
#   KIRAVR_BOX=name           Distrobox name (default: kiravr-build)
#   KIRAVR_ANDROID_SDK=path   SDK location (default: ~/.local/share/kiravr/android-sdk)
#   KIRAVR_JOBS=n             Parallel compile jobs (see build-streamer-distrobox.sh)
#   KIRAVR_DEBUG=1            Log every command, keep going past errors
#   NO_COLOR=1                Disable colored output (also off when not on a TTY)

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

BOX="${KIRAVR_BOX:-kiravr-build}"
IMAGE="docker.io/library/ubuntu:24.04"
SDK="${KIRAVR_ANDROID_SDK:-$HOME/.local/share/kiravr/android-sdk}"
# Pinned to what CI uses (.github/workflows/build.yml)
NDK_VERSION="26.1.10909125"   # r26b
PLATFORM="platforms;android-32"
BUILD_TOOLS="build-tools;34.0.0"
CMDLINE_TOOLS_URL="https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip"
PROFILE=""
INSTALL=0

for arg in "$@"; do
    case "$arg" in
        --release) PROFILE="--release" ;;
        --install) INSTALL=1 ;;
        -h|--help) sed -n 2,15p "$0"; exit 0 ;;
        *) echo "Unknown argument: $arg" >&2; exit 2 ;;
    esac
done

kiravr_init distrobox-build-client

kiravr_ensure_box "$BOX" "$IMAGE" build-essential pkg-config curl git ca-certificates unzip clang libclang-dev openjdk-17-jdk-headless

JAVA_HOME_IN_BOX="/usr/lib/jvm/java-17-openjdk-amd64"
ENV="export JAVA_HOME=$JAVA_HOME_IN_BOX ANDROID_HOME=\"$SDK\" ANDROID_NDK_HOME=\"$SDK/ndk/$NDK_VERSION\""
JOBS="$(kiravr_jobs)"
info "Using $JOBS compile jobs (set KIRAVR_JOBS to change)"

# Release builds sign with the KiraVR key when it is here, the same key the
# GitHub releases use, so local and released APKs update each other in place.
# Without it xtask makes a throwaway key, and switching between the two needs
# an uninstall on the headset (which also clears the app's settings).
SIGNING_DIR="${KIRAVR_SIGNING_DIR:-$HOME/.local/share/kiravr/signing}"
if [[ "$PROFILE" == "--release" && -f "$SIGNING_DIR/kiravr.keystore" && -f "$SIGNING_DIR/password" ]]; then
    ENV="$ENV CARGO_APK_RELEASE_KEYSTORE=\"$SIGNING_DIR/kiravr.keystore\" CARGO_APK_RELEASE_KEYSTORE_PASSWORD=\"\$(cat \"$SIGNING_DIR/password\")\""
    ok "Signing with the KiraVR key from $SIGNING_DIR"
elif [[ "$PROFILE" == "--release" ]]; then
    warn "No KiraVR signing key in $SIGNING_DIR, using a local throwaway key"
fi
in_box() { distrobox enter "$BOX" -- bash -lc "source \"\$HOME/.cargo/env\" && $ENV && export CARGO_BUILD_JOBS=$JOBS && cd \"$KIRAVR_REPO_DIR\" && $*"; }

SDKMANAGER="$SDK/cmdline-tools/latest/bin/sdkmanager"
if [[ ! -x "$SDKMANAGER" ]]; then
    info "Downloading Android command line tools to $SDK"
    tmp="$(mktemp -d)"
    curl -L --fail -o "$tmp/cmdline-tools.zip" "$CMDLINE_TOOLS_URL"
    mkdir -p "$SDK/cmdline-tools"
    unzip -q "$tmp/cmdline-tools.zip" -d "$tmp"
    rm -rf "$SDK/cmdline-tools/latest"
    mv "$tmp/cmdline-tools" "$SDK/cmdline-tools/latest"
    rm -rf "$tmp"
    ok "Command line tools installed"
fi

if [[ ! -d "$SDK/ndk/$NDK_VERSION" || ! -d "$SDK/platforms/android-32" || ! -d "$SDK/build-tools/34.0.0" ]]; then
    info "Installing $PLATFORM, $BUILD_TOOLS and NDK $NDK_VERSION (about 2 GB)"
    warn "This accepts the Android SDK license on your behalf"
    in_box "yes | \"$SDKMANAGER\" --sdk_root=\"$SDK\" --licenses >/dev/null && \"$SDKMANAGER\" --sdk_root=\"$SDK\" \"$PLATFORM\" \"$BUILD_TOOLS\" \"ndk;$NDK_VERSION\""
    ok "Android SDK ready"
else
    ok "Android SDK and NDK already installed"
fi

if [[ ! -d "$KIRAVR_REPO_DIR/deps/android_openxr" ]] || ! in_box "command -v cargo-apk && command -v cargo-ndk" >/dev/null 2>&1; then
    info "Preparing Android deps (Rust targets, cargo-ndk, cargo-apk, OpenXR loaders)"
    in_box "cargo xtask prepare-deps --platform android"
    ok "Android deps ready"
else
    ok "Android deps already prepared"
fi

info "Building the client ${PROFILE:-(debug profile)}"
in_box "cargo xtask build-client $PROFILE"
APK="$(ls -t "$KIRAVR_REPO_DIR"/build/alvr_client_android/*.apk 2>/dev/null | head -1)"
[[ -n "$APK" ]] || { error "Build finished but no APK found in build/alvr_client_android"; exit 1; }
ok "Client built: $APK"
bundle_client_apk

if [[ "$INSTALL" == "1" ]]; then
    command -v adb >/dev/null || { error "adb not found on the host"; exit 1; }
    info "Installing on the headset"
    adb install -r "$APK"
    ok "Installed. It shows up as \"KiraVR\" in Unknown Sources."
fi
