# Retro 3DO on Android

The launcher and the emulator as one APK for aarch64 Android handhelds — built
for the Retroid Pocket (Dimensity, 1080p landscape, physical gamepad, Android
13), and no different on any other arm64 device.

Everything in this file is one script: `android/build-apk.sh`.

## Build

```bash
cd ~/retro3do-rs/android
./build-apk.sh                     # release
./build-apk.sh debug               # fast crate, slow emulator
```

Produces `android/build/retro3do-fz10-release.apk` (or `-debug.apk`) and signs
it with a generated debug key. Install over wireless adb:

```bash
adb connect 192.168.1.186:5555
adb install -r android/build/retro3do-fz10-release.apk
```

What the script does, and why there is no Gradle: `cargo-ndk` cross-compiles the
launcher to `arm64-v8a/libretro3do_gui.so`, then the SDK's own build-tools do
the packaging — `aapt2` compiles `res/` and links `AndroidManifest.xml`,
`javac`+`d8` turn the pad shim into `classes.dex`, `zip` adds the `.so` and the
dex, `zipalign -p` page-aligns the library and `apksigner` signs. A Gradle
project would add an Android Studio install and a dependency download for the
same four commands.

Requirements it expects to find (and sets `ANDROID_HOME`/`ANDROID_NDK_HOME`
for): a Rust toolchain with the `aarch64-linux-android` target
(`rustup target add aarch64-linux-android`), `cargo-ndk`
(`cargo install cargo-ndk`), a JDK, and the Android SDK at `~/Android/Sdk` with
an NDK, `platforms/android-34` and any `build-tools`. The script uses the
newest of each that is installed.

## How it is put together

| Piece | Desktop | Android |
| --- | --- | --- |
| Window, input, painting | eframe/winit (X11/Wayland) | eframe/winit, `android-native-activity` backend |
| GPU | wgpu (Vulkan/GL) | wgpu (Vulkan, then GLES) |
| Audio | SDL3 audio stream | none yet — the ring is drained, the game is silent |
| Gamepads | SDL3 gamepads | Android key/motion events into the shim (`android_pad.rs`) |
| Storage | `~/.config/retro3do/launcher.conf` | `/sdcard/fz10/launcher.conf` |

`android/src/lib.rs` compiles `app/src/bin/retro3do-gui.rs` itself — the same
file the desktop binary builds, with `#[cfg(target_os = "android")]` in place —
as a `cdylib`. There is no second copy of the front end to keep in step.

`android_main` is the entry point `android-activity` resolves in the loaded
library; winit hands the activity to eframe through `NativeOptions::android_app`.

## Storage

First run points the walkthrough at, and creates:

```
/sdcard/fz10/games      discs (.iso, .bin/.cue, .chd, .m3u)
/sdcard/fz10/bios       3DO BIOS ROMs (.bin, .rom)
/sdcard/fz10/media      box art, disc art (RetroMedia layout)
/sdcard/fz10/saves      save files
/sdcard/fz10/docs       pre-rendered documentation pages
/sdcard/fz10/launcher.conf
```

That folder is user-visible: a file manager, an SD card reader or `adb push
game.chd /sdcard/fz10/games/` all work. Writing there needs **All files
access** (Settings → Apps → Retro 3DO → Special app access → All files
access); the manifest asks for `MANAGE_EXTERNAL_STORAGE`. Without the grant the
app silently falls back to
`/storage/emulated/0/Android/data/com.crownpark.fz10/files/fz10`, which needs no
permission at all. `launcher.conf` stays a plain `key = value` file in either
place, and the walkthrough can still be pointed anywhere by hand.

## Input

The physical gamepad becomes console pad 0, beside the keyboard mapping that
already exists:

| Controller | 3DO pad |
| --- | --- |
| d-pad / left stick / hat | Up, Down, Left, Right |
| A (south) | A |
| B (east) | B |
| X (west), Y (north), C, Z | C |
| L1, L2 | Left Shift |
| R1, R2 | Right Shift |
| Start | P (Play) |
| Select | X (Stop) |

Android delivers those as key events winit has no name for, so
`Fz10Activity` (the only Java in the project, ~90 lines) subclasses
`NativeActivity` and forwards `onKeyDown`/`onKeyUp`/`onGenericMotionEvent` to
two JNI entry points in `android/src/lib.rs`; the axes are read there too, so
the left stick and the hat both steer the d-pad. Touch works everywhere the
launcher accepts a click, including dragging the FZ-1 to turn it.

The activity is `sensorLandscape`, keeps the screen awake, and hides the system
bars.

## Known limits (v1)

- **No audio on Android yet.** The core's audio ring is drained and dropped
  (`SoundAndPads::top_up` in the Android block). The hook is there; an AAudio
  or Oboe stream is the next step.
- Analog sticks only drive the d-pad, as the desktop mapping does; there is no
  on-screen pad for touch-only play (the launcher itself is fully touch).
- Release builds are signed with a generated debug key, so updates install over
  each other but the APK is not store-ready.
