#!/usr/bin/env bash
# Packs the release streamer (with the bundled headset APK) into
# build/KiraVR-<version>-x86_64.AppImage.
#
#   scripts/build-appimage.sh [--build]
#     --build  Build the streamer and client in release mode first.
#
# Debug info is stripped from the AppImage and kept in
# build/debug-symbols/<version>/ for addr2line.
# Env: KIRAVR_DEBUG=1, NO_COLOR=1, KIRAVR_JOBS (with --build).

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
kiravr_init build-appimage

APPIMAGETOOL_VERSION="1.9.0"
TOOLS="$HOME/.local/share/kiravr/tools"
APPIMAGETOOL="$TOOLS/appimagetool-$APPIMAGETOOL_VERSION-x86_64.AppImage"
STREAMER="$KIRAVR_REPO_DIR/build/alvr_streamer_linux"
WORK="$KIRAVR_REPO_DIR/build/appimage"
APPDIR="$WORK/KiraVR.AppDir"
PAYLOAD="$APPDIR/usr/share/kiravr/streamer"

if [[ "${1:-}" == "--build" ]]; then
    "$KIRAVR_REPO_DIR/scripts/build-client-distrobox.sh" --release
    "$KIRAVR_REPO_DIR/scripts/build-streamer-distrobox.sh" --release
fi

VERSION="$(grep -m1 '^version' "$KIRAVR_REPO_DIR/Cargo.toml" | cut -d'"' -f2)"
[[ -x "$STREAMER/bin/alvr_dashboard" ]] || { error "No streamer build in $STREAMER. Run with --build."; exit 1; }
DRIVER="$STREAMER/lib64/alvr/bin/linux64/driver_alvr_server.so"
[[ -f "$DRIVER" ]] || { error "Driver missing: $DRIVER"; exit 1; }
if [[ ! -f "$STREAMER/share/alvr/kiravr_client.apk" ]]; then
    bundle_client_apk
    [[ -f "$STREAMER/share/alvr/kiravr_client.apk" ]] \
        || warn "No headset APK: The AppImage will work, but \"Install on Headset\" stays off"
fi

if [[ ! -x "$APPIMAGETOOL" ]]; then
    info "Downloading appimagetool $APPIMAGETOOL_VERSION"
    mkdir -p "$TOOLS"
    curl --fail -L --progress-bar -o "$APPIMAGETOOL.part" \
        "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-x86_64.AppImage"
    chmod +x "$APPIMAGETOOL.part" && mv "$APPIMAGETOOL.part" "$APPIMAGETOOL"
    ok "appimagetool ready"
fi

info "Assembling the AppDir"
rm -rf "$WORK"
mkdir -p "$PAYLOAD" "$KIRAVR_REPO_DIR/build/debug-symbols/$VERSION"
cp -a "$STREAMER/." "$PAYLOAD/"
cp "$KIRAVR_REPO_DIR/packaging/appimage/AppRun" "$APPDIR/AppRun"
cp "$KIRAVR_REPO_DIR/packaging/appimage/dev.daggenthal.kiravr.desktop" "$APPDIR/"
cp "$KIRAVR_REPO_DIR/packaging/appimage/dev.daggenthal.kiravr.png" "$APPDIR/"
ln -s dev.daggenthal.kiravr.png "$APPDIR/.DirIcon"
chmod +x "$APPDIR/AppRun"

info "Stripping debug info (kept in build/debug-symbols/$VERSION)"
for binary in "$PAYLOAD/bin/alvr_dashboard" "$PAYLOAD/lib64/alvr/bin/linux64/driver_alvr_server.so"; do
    name="$(basename "$binary")"
    symbols="$KIRAVR_REPO_DIR/build/debug-symbols/$VERSION/$name.debug"
    objcopy --only-keep-debug "$binary" "$symbols"
    strip --strip-debug "$binary"
    objcopy --add-gnu-debuglink="$symbols" "$binary"
done

# Changes whenever any shipped file changes, so the AppImage knows when to
# replace the installed copy (also between two builds of the same version).
echo "$VERSION" > "$PAYLOAD/kiravr-version"
(cd "$PAYLOAD" && find . -type f ! -name 'kiravr-build-id' -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -c1-16) \
    > "$PAYLOAD/kiravr-build-id"

OUTPUT="$KIRAVR_REPO_DIR/build/KiraVR-$VERSION-x86_64.AppImage"
info "Packing $OUTPUT"
rm -f "$OUTPUT"
# Extract-and-run works without FUSE (also inside containers).
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGETOOL" --no-appstream "$APPDIR" "$OUTPUT" \
    > "$WORK/appimagetool.log" 2>&1 || { error "appimagetool failed, see $WORK/appimagetool.log"; tail -20 "$WORK/appimagetool.log" >&2; exit 1; }
ok "AppImage built: $OUTPUT ($(du -h "$OUTPUT" | cut -f1))"
