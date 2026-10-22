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
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        matchStatusBar(getResources().getConfiguration());
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
