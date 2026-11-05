package io.github.schotman_development.fonoteca;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.graphics.drawable.Icon;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;

/**
 * Keeps the app playing with the screen off, since Android stops a
 * background app that has no foreground service, and shows what plays in a
 * notification and on the lock screen. Its media session takes their
 * buttons, a headset's and a watch's, and hands them to Rust
 * (src/media.rs) through MainActivity.command. It pauses too when Android
 * is about to move the sound to the speaker.
 */
public class PlaybackService extends Service {
    // The buttons, as src/media.rs numbers them.
    static final int PLAY = 0;
    static final int PAUSE = 1;
    static final int NEXT = 2;
    static final int PREVIOUS = 3;
    static final int SEEK = 4;

    private static final String CHANNEL = "playback";
    private static final int NOTIFICATION = 1;
    private static final String BUTTON = "button";
    private static final Handler main = new Handler(Looper.getMainLooper());

    /** What plays, or null once it's to be hidden. Main thread only, as is running. */
    private static Playing shown;
    private static PlaybackService running;

    private MediaSession session;

    /**
     * Android's word, before it moves the sound, that headphones or a
     * Bluetooth or USB output went: it pauses as the pause button does.
     */
    private final BroadcastReceiver becomingNoisy = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            MainActivity.command(PAUSE, 0);
        }
    };

    /** A track as the notification shows it, its times in milliseconds. */
    static final class Playing {
        final String title;
        final String artists;
        final boolean playing;
        final long duration;
        final long position;

        Playing(String title, String artists, boolean playing, long duration, long position) {
            this.title = title;
            this.artists = artists;
            this.playing = playing;
            this.duration = duration;
            this.position = position;
        }
    }

    /** Shows `playing`, starting the service the first time. From any thread. */
    static void show(Context context, Playing playing) {
        main.post(() -> {
            shown = playing;
            if (running != null) {
                running.update();
                return;
            }
            try {
                context.startForegroundService(new Intent(context, PlaybackService.class));
            } catch (IllegalStateException backgrounded) {
                // Android 12 and later don't let a background app start one;
                // playing goes on without the notification.
            }
        });
    }

    static void hide() {
        main.post(() -> {
            shown = null;
            if (running != null) {
                running.stopSelf();
            }
        });
    }

    @Override
    public void onCreate() {
        super.onCreate();
        running = this;
        getSystemService(NotificationManager.class).createNotificationChannel(
                new NotificationChannel(CHANNEL, "Playing", NotificationManager.IMPORTANCE_LOW));
        session = new MediaSession(this, "Fonoteca");
        session.setCallback(new MediaSession.Callback() {
            @Override
            public void onPlay() {
                MainActivity.command(PLAY, 0);
            }

            @Override
            public void onPause() {
                MainActivity.command(PAUSE, 0);
            }

            @Override
            public void onSkipToNext() {
                MainActivity.command(NEXT, 0);
            }

            @Override
            public void onSkipToPrevious() {
                MainActivity.command(PREVIOUS, 0);
            }

            @Override
            public void onSeekTo(long position) {
                MainActivity.command(SEEK, position / 1000.0);
            }
        });
        session.setActive(true);
        registerReceiver(becomingNoisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // The notification's own buttons, on Androids before 13, which don't
        // draw the session's.
        int button = intent == null ? -1 : intent.getIntExtra(BUTTON, -1);
        if (button >= 0) {
            MainActivity.command(button, 0);
        }
        // Started with startForegroundService, it must go to the foreground
        // even when hidden meanwhile, or Android ends the app.
        Playing playing = shown != null ? shown : new Playing("", "", false, 0, 0);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION, notification(playing), ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        } else {
            startForeground(NOTIFICATION, notification(playing));
        }
        if (shown == null) {
            stopSelf();
        }
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        running = null;
        unregisterReceiver(becomingNoisy);
        session.release();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private void update() {
        getSystemService(NotificationManager.class).notify(NOTIFICATION, notification(shown));
    }

    /** The session's state and the notification for `playing`. */
    private Notification notification(Playing playing) {
        session.setMetadata(new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, playing.title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, playing.artists)
                .putLong(MediaMetadata.METADATA_KEY_DURATION, playing.duration)
                .build());
        session.setPlaybackState(new PlaybackState.Builder()
                .setActions(PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE
                        | PlaybackState.ACTION_SKIP_TO_NEXT | PlaybackState.ACTION_SKIP_TO_PREVIOUS
                        | PlaybackState.ACTION_SEEK_TO)
                .setState(playing.playing ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED,
                        playing.position, playing.playing ? 1 : 0)
                .build());
        // As the launcher opens it: the app's task comes back rather than a
        // second activity.
        Intent open = getPackageManager().getLaunchIntentForPackage(getPackageName());
        return new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_launcher_monochrome)
                .setContentTitle(playing.title)
                .setContentText(playing.artists)
                .setContentIntent(PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE))
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .addAction(action(android.R.drawable.ic_media_previous, "Previous", PREVIOUS))
                .addAction(playing.playing
                        ? action(android.R.drawable.ic_media_pause, "Pause", PAUSE)
                        : action(android.R.drawable.ic_media_play, "Play", PLAY))
                .addAction(action(android.R.drawable.ic_media_next, "Next", NEXT))
                .setStyle(new Notification.MediaStyle()
                        .setMediaSession(session.getSessionToken())
                        .setShowActionsInCompactView(0, 1, 2))
                .build();
    }

    private Notification.Action action(int icon, String title, int button) {
        Intent press = new Intent(this, PlaybackService.class).putExtra(BUTTON, button);
        // Each button its own request code, or they'd share one intent.
        PendingIntent pending = PendingIntent.getService(this, button, press, PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Action.Builder(Icon.createWithResource(this, icon), title, pending).build();
    }
}
