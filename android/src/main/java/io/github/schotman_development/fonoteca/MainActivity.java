package io.github.schotman_development.fonoteca;

import android.app.NativeActivity;
import android.content.res.Configuration;
import android.os.Build;
import android.os.Bundle;
import android.view.View;
import android.view.WindowInsetsController;

/**
 * The NativeActivity the app runs in, with the status bar's icons dark while
 * the phone is in light mode and light in dark mode. The app draws under the
 * status bar in the phone's colours, and the theme alone would keep the
 * icons light.
 */
public class MainActivity extends NativeActivity {
    /** A media button pressed: Rust's (src/media.rs), registered as the app starts. */
    static native void command(int button, double seconds);

    /** What plays, from Rust's thread, for the notification and lock screen. */
    public void showPlaying(String title, String artists, boolean playing, long duration, long position) {
        PlaybackService.show(this, new PlaybackService.Playing(title, artists, playing, duration, position));
    }

    public void hidePlaying() {
        PlaybackService.hide();
    }

    private BitPerfectOutput bitPerfect;

    /** What the USB output takes untouched, for Rust's player (src/bit_perfect.rs). */
    public int[] bitPerfectFormats() {
        return bitPerfect.formats();
    }

    public boolean engageBitPerfect(int rate, int channels, int bits) {
        return bitPerfect.engage(rate, channels, bits);
    }

    public void releaseBitPerfect() {
        bitPerfect.release();
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        bitPerfect = new BitPerfectOutput(this);
        matchStatusBar(getResources().getConfiguration());
    }

    // Leaving the app ends its player, so its controls go too: after the
    // Rust side has stopped, so nothing it shows lands after this.
    @Override
    protected void onDestroy() {
        super.onDestroy();
        PlaybackService.hide();
        bitPerfect.release();
    }

    // Dark mode turned on or off doesn't restart the activity (see
    // android:configChanges); it comes here.
    @Override
    public void onConfigurationChanged(Configuration config) {
        super.onConfigurationChanged(config);
        matchStatusBar(config);
    }

    @SuppressWarnings("deprecation")
    private void matchStatusBar(Configuration config) {
        boolean light = (config.uiMode & Configuration.UI_MODE_NIGHT_MASK) != Configuration.UI_MODE_NIGHT_YES;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            WindowInsetsController bars = getWindow().getInsetsController();
            if (bars != null) {
                int dark = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS;
                bars.setSystemBarsAppearance(light ? dark : 0, dark);
            }
        } else {
            View decor = getWindow().getDecorView();
            int flags = decor.getSystemUiVisibility();
            int dark = View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR;
            decor.setSystemUiVisibility(light ? flags | dark : flags & ~dark);
        }
    }
}
