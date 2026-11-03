package io.github.schotman_development.fonoteca;

import android.annotation.TargetApi;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.job.JobInfo;
import android.app.job.JobParameters;
import android.app.job.JobScheduler;
import android.app.job.JobService;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;

/**
 * Keeps the app alive while it downloads music in the background, as a
 * user-initiated data transfer job (Android 14 and later). The downloading is
 * Rust's (src/downloads.rs); this is the job and its notification, which
 * Android requires and shows in its Task Manager, where the user can stop it.
 * Before Android 14 there is no job, and the downloads run while the app is
 * open or playing.
 */
public class DownloadService extends JobService {
    static final int JOB_ID = 19;
    private static final String CHANNEL = "downloads";
    private static final int NOTIFICATION = 2;
    private static final Handler main = new Handler(Looper.getMainLooper());

    /** The job running and what its notification says. Main thread only. */
    private static DownloadService running;
    private static JobParameters parameters;
    private static String text = "";
    private static int done;
    private static int total;

    /**
     * Schedules the job for about `bytes` of downloading, over an unmetered
     * network only if `unmetered`. Android lets an app schedule one only while
     * it is in the foreground, as when the user has just asked for a download;
     * otherwise the downloads go on in the app without a job. From any thread.
     */
    @TargetApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
    static void schedule(Context context, long bytes, boolean unmetered) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            return;
        }
        main.post(() -> {
            JobScheduler scheduler = context.getSystemService(JobScheduler.class);
            if (running != null || scheduler.getPendingJob(JOB_ID) != null) {
                return;
            }
            scheduler.schedule(new JobInfo.Builder(JOB_ID, new ComponentName(context, DownloadService.class))
                    .setUserInitiated(true)
                    .setRequiredNetworkType(unmetered ? JobInfo.NETWORK_TYPE_UNMETERED : JobInfo.NETWORK_TYPE_ANY)
                    .setEstimatedNetworkBytes(bytes > 0 ? bytes : JobInfo.NETWORK_BYTES_UNKNOWN, 0)
                    .build());
        });
    }

    /** Says how far the downloads are: `text` under the title, `done` of `total` songs. From any thread. */
    static void progress(String message, int songsDone, int songsTotal) {
        main.post(() -> {
            text = message;
            done = songsDone;
            total = songsTotal;
            if (running != null) {
                running.update();
            }
        });
    }

    /** Ends the job, and with it the notification. From any thread. */
    static void finish(Context context) {
        main.post(() -> {
            if (running != null && parameters != null) {
                running.jobFinished(parameters, false);
                running = null;
                parameters = null;
            }
            context.getSystemService(JobScheduler.class).cancel(JOB_ID);
        });
    }

    // The job is only ever scheduled on Android 14 and later, where the
    // system calls this; setNotification and the policy constant need it.
    @TargetApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
    @Override
    public boolean onStartJob(JobParameters params) {
        running = this;
        parameters = params;
        getSystemService(NotificationManager.class).createNotificationChannel(
                new NotificationChannel(CHANNEL, "Downloads", NotificationManager.IMPORTANCE_LOW));
        // Due within 10 seconds of starting, or Android stops the job.
        setNotification(params, NOTIFICATION, notification(), JobService.JOB_END_NOTIFICATION_POLICY_REMOVE);
        // The work goes on in Rust, so the job is not done.
        return true;
    }

    /**
     * The job stopped. When the user stopped it, from Android's Task Manager,
     * Rust stops too and Android won't reschedule it. When the system did, as
     * when wifi is lost under the unmetered constraint, Rust goes on by its own
     * rules and waits for wifi, and the job is asked for again.
     */
    @TargetApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
    @Override
    public boolean onStopJob(JobParameters params) {
        running = null;
        parameters = null;
        boolean byUser = params.getStopReason() == JobParameters.STOP_REASON_USER;
        if (byUser) {
            MainActivity.downloadsStopped();
        }
        return !byUser;
    }

    private void update() {
        getSystemService(NotificationManager.class).notify(NOTIFICATION, notification());
    }

    private Notification notification() {
        // As the launcher opens it: the app's task comes back rather than a
        // second activity.
        Intent open = getPackageManager().getLaunchIntentForPackage(getPackageName());
        Notification.Builder builder = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_launcher_monochrome)
                .setContentTitle("Downloading music")
                .setContentText(text)
                .setOngoing(true)
                .setContentIntent(PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE));
        if (total > 0) {
            builder.setProgress(total, done, false);
        }
        return builder.build();
    }
}
