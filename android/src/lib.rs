//! What Android loads: the launcher (`retro3do-gui`) as a shared library, with
//! `android_main` as winit's entry point and two JNI entry points the activity
//! calls for pad input it can see but winit cannot name.
//!
//! The whole front end is reused as-is: `gui` is the same source file the
//! desktop binary `retro3do-gui` builds, compiled here with the Android cfg.

#[path = "../../app/src/bin/retro3do-gui.rs"]
pub mod gui;

// `android-activity` resolves this symbol in the loaded library and calls it
// with the activity. It has to be exported here: the gui source forbids unsafe
// attributes, and `no_mangle` counts as one. The by-value activity handle is
// the signature android-activity looks for, layout included.
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn android_main(app: winit::platform::android::activity::AndroidApp) {
    gui::android_main(app);
}

use core::ffi::c_void;

// JNI: `Java_com_crownpark_fz10_Fz10Activity_<method>`. Only plain integers and
// floats cross, so nothing here has to touch the JVM. The first two parameters
// are the JNIEnv and the class/instance, unused.

#[no_mangle]
pub extern "system" fn Java_com_crownpark_fz10_Fz10Activity_nativeKey(
    _env: *mut c_void,
    _this: *mut c_void,
    code: i32,
    down: u8,
) {
    gui::android_pad_key(code, down != 0);
}

#[no_mangle]
pub extern "system" fn Java_com_crownpark_fz10_Fz10Activity_nativeAxis(
    _env: *mut c_void,
    _this: *mut c_void,
    axis: i32,
    value: f32,
) {
    gui::android_pad_axis(axis, value);
}
