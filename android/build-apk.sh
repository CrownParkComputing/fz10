#!/usr/bin/env bash
# Builds the installable FZ-10 APK (arm64, the Retroid Pocket and every other
# aarch64 Android handheld). No Gradle, no Android Studio: cargo-ndk makes the
# .so, and the build-tools the SDK already ships do the rest.
#
#   ./build-apk.sh            # release
#   ./build-apk.sh debug      # faster, much slower to play
#
# Sets ANDROID_HOME / ANDROID_NDK_HOME itself from the usual SDK locations.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
profile=${1:-release}
case "$profile" in
release) cargo_flags=(--release) ;;
debug) cargo_flags=() ;;
*)
    echo "usage: $0 [release|debug]" >&2
    exit 2
    ;;
esac

SDK=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}
NDK=${ANDROID_NDK_HOME:-$(ls -d "$SDK"/ndk/* 2>/dev/null | sort -V | tail -1)}
BT=$(ls -d "$SDK"/build-tools/* 2>/dev/null | sort -V | tail -1)
PLATFORM=$(ls -d "$SDK"/platforms/android-* 2>/dev/null | sort -V | tail -1)
ABI=${ABI:-arm64-v8a}
OUT=$here/build
PKG=com.crownpark.fz10
PKGDIR=com/crownpark/fz10
JAVA=$here/java/$PKGDIR

for need in "$NDK" "$BT" "$PLATFORM"; do
    if [ -z "$need" ] || [ ! -d "$need" ]; then
        echo "missing Android SDK part (SDK=$SDK)" >&2
        exit 1
    fi
done
export ANDROID_HOME=$SDK
export ANDROID_NDK_HOME=$NDK

# The rustup-managed toolchain is the one that carries the android target; a
# distro cargo/rustc placed first on PATH (Arch) carries none. CARGO overrides.
export PATH="$HOME/.cargo/bin:$PATH"
if [ -z "${CARGO:-}" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO=$HOME/.cargo/bin/cargo
fi
CARGO=${CARGO:-cargo}

echo "==> core + launcher for $ABI ($profile)"
rm -rf "$OUT"
mkdir -p "$OUT/jniLibs"
( cd "$here" && "$CARGO" ndk -t "$ABI" -o "$OUT/jniLibs" build "${cargo_flags[@]}" --lib )
SO=$OUT/jniLibs/$ABI/libretro3do_gui.so
[ -f "$SO" ] || {
    echo "no $SO" >&2
    exit 1
}

echo "==> resources"
mkdir -p "$OUT/res"
"$BT/aapt2" compile --dir "$here/res" -o "$OUT/res.zip"
"$BT/aapt2" link -o "$OUT/base.apk" -I "$PLATFORM/android.jar" \
    --manifest "$here/AndroidManifest.xml" --auto-add-overlay "$OUT/res.zip"

echo "==> the pad shim"
mkdir -p "$OUT/classes" "$OUT/dex"
javac -nowarn --release 17 -classpath "$PLATFORM/android.jar" -d "$OUT/classes" \
    "$JAVA"/*.java
"$BT/d8" --lib "$PLATFORM/android.jar" --min-api 26 --output "$OUT/dex" \
    "$OUT/classes/$PKGDIR"/*.class

echo "==> package"
cp "$OUT/base.apk" "$OUT/app-unsigned.apk"
mkdir -p "$OUT/stage/lib/$ABI"
cp "$SO" "$OUT/stage/lib/$ABI/"
( cd "$OUT/stage" && zip -q -0 -X "$OUT/app-unsigned.apk" "lib/$ABI/libretro3do_gui.so" )
( cd "$OUT/dex" && zip -q -X "$OUT/app-unsigned.apk" classes.dex )
"$BT/zipalign" -p -f 4 "$OUT/app-unsigned.apk" "$OUT/app-aligned.apk"

echo "==> sign"
KS=$here/keystore/debug.jks
if [ ! -f "$KS" ]; then
    mkdir -p "$here/keystore"
    keytool -genkeypair -keystore "$KS" -storepass android -keypass android \
        -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
        -dname "CN=Android Debug,O=Android,C=US" >/dev/null 2>&1
fi
APK=$OUT/retro3do-fz10-$profile.apk
rm -f "$APK"
"$BT/apksigner" sign --ks "$KS" --ks-pass pass:android --key-pass pass:android \
    --ks-key-alias androiddebugkey --out "$APK" "$OUT/app-aligned.apk"
"$BT/apksigner" verify "$APK"

echo
echo "APK: $APK"
echo "install: adb install -r $APK"
