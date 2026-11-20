package com.crownpark.fz10;

import android.app.NativeActivity;
import android.content.Intent;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.provider.Settings;
import android.view.InputDevice;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.View;
import android.view.WindowManager;

/**
 * The activity native-activity needs: {@link NativeActivity} does the window,
 * the input queue and the library load; this subclass only adds the two things
 * a 3DO pad wants and winit cannot see — the controller's key codes and its
 * axes — and hands them to the library through JNI.
 */
public class Fz10Activity extends NativeActivity {
    private static native void nativeKey(int code, boolean down);

    private static native void nativeAxis(int axis, float value);

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        askForSharedStorage();
    }

    /**
     * /sdcard/fz10 is where the launcher wants its games, BIOS and art: one
     * folder a file manager, an SD card reader or `adb push` can fill. That
     * needs "All files access", so the first run opens the switch for it. The
     * launcher works without the grant on its own external files dir, so
     * declining or backing out is safe.
     */
    private void askForSharedStorage() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            return;
        }
        if (Environment.isExternalStorageManager()) {
            return;
        }
        try {
            startActivity(
                    new Intent(
                            Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                            Uri.parse("package:" + getPackageName())));
        } catch (Exception noSettingsPage) {
            // Some builds have no per-app page; the fallback folder still works.
        }
    }

    @Override
    public boolean onKeyDown(int code, KeyEvent event) {
        nativeKey(code, true);
        return super.onKeyDown(code, event);
    }

    @Override
    public boolean onKeyUp(int code, KeyEvent event) {
        nativeKey(code, false);
        return super.onKeyUp(code, event);
    }

    @Override
    public boolean onGenericMotionEvent(MotionEvent event) {
        if ((event.getSource() & InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK) {
            nativeAxis(MotionEvent.AXIS_X, event.getAxisValue(MotionEvent.AXIS_X));
            nativeAxis(MotionEvent.AXIS_Y, event.getAxisValue(MotionEvent.AXIS_Y));
            nativeAxis(MotionEvent.AXIS_HAT_X, event.getAxisValue(MotionEvent.AXIS_HAT_X));
            nativeAxis(MotionEvent.AXIS_HAT_Y, event.getAxisValue(MotionEvent.AXIS_HAT_Y));
            nativeAxis(MotionEvent.AXIS_LTRIGGER, event.getAxisValue(MotionEvent.AXIS_LTRIGGER));
            nativeAxis(MotionEvent.AXIS_RTRIGGER, event.getAxisValue(MotionEvent.AXIS_RTRIGGER));
            return true;
        }
        return super.onGenericMotionEvent(event);
    }

    /** A handheld is a handheld: no bars, no sleep while a game is up. */
    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (!hasFocus) {
            return;
        }
        getWindow()
                .getDecorView()
                .setSystemUiVisibility(
                        View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                                | View.SYSTEM_UI_FLAG_FULLSCREEN
                                | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                                | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                                | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                                | View.SYSTEM_UI_FLAG_LAYOUT_STABLE);
    }
}
