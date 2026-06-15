package ie.cannygreen.berapp;

import android.app.NativeActivity;
import android.os.Bundle;
import android.view.WindowInsets;
import android.view.WindowInsetsController;

/**
 * Thin subclass of {@link NativeActivity} whose only job is immersive mode.
 *
 * The Rust app (libber_app.so, loaded via the {@code android.app.lib_name}
 * meta-data, same as plain NativeActivity) draws a single edge-to-edge egui
 * surface. With the system navigation bar visible it floats over the app's
 * bottom panels (the prompt dock / Continue bar / input fields). We hide the
 * status + navigation bars entirely so the surface owns the whole screen; the
 * user swipes from an edge to transiently reveal Back/Home/Recents, which then
 * auto-hide.
 *
 * These calls touch the Window/decor View, so they must run on the UI thread —
 * which is exactly where Activity lifecycle callbacks already run, hence doing
 * it here in Java rather than over JNI from the native thread. Re-applying on
 * focus regain re-hides the bars after a transient swipe or returning to the
 * app. minSdk = 30, so WindowInsetsController is available unconditionally.
 */
public class MainActivity extends NativeActivity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        hideSystemBars();
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            hideSystemBars();
        }
    }

    private void hideSystemBars() {
        WindowInsetsController controller = getWindow().getDecorView().getWindowInsetsController();
        if (controller != null) {
            controller.hide(WindowInsets.Type.systemBars());
            controller.setSystemBarsBehavior(
                    WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
        }
    }
}
