package io.github.schotman_development.fonoteca;

import android.annotation.TargetApi;
import android.content.Context;
import android.media.AudioAttributes;
import android.media.AudioDeviceInfo;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioMixerAttributes;
import android.os.Build;
import android.util.Log;

import java.util.ArrayList;
import java.util.List;

/**
 * Android 14's bit-perfect mode for a USB audio output: while the app's
 * mixer attributes are set on the device, what the app plays in exactly
 * that format reaches the output untouched. Without it, or below Android 14,
 * the system's mixer resamples everything to the rate it runs the output at.
 */
final class BitPerfectOutput {
    private static final String TAG = "BitPerfectOutput";
    /** The encodings the player can produce (cpal's AAudio plays 16-bit and float). */
    private static final int PCM_16 = 16;
    private static final int PCM_FLOAT = 32;

    private final AudioManager audio;
    private final AudioAttributes media = new AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_MEDIA).build();
    /** The output the preferred attributes are set on, until cleared. */
    private AudioDeviceInfo engaged;

    BitPerfectOutput(Context context) {
        audio = context.getSystemService(AudioManager.class);
    }

    /** The formats the USB output passes on untouched, as rate, channels, bits (32 is float) triples. */
    @TargetApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE) // usbOutput() finds nothing before it, so `engaged` stays null
    synchronized int[] formats() {
        List<Integer> triples = new ArrayList<>();
        AudioDeviceInfo usb = usbOutput();
        if (usb == null) {
            return new int[0];
        }
        for (AudioMixerAttributes attributes : audio.getSupportedMixerAttributes(usb)) {
            AudioFormat format = attributes.getFormat();
            int bits = bitsOf(format);
            if (attributes.getMixerBehavior() != AudioMixerAttributes.MIXER_BEHAVIOR_BIT_PERFECT || bits == 0) {
                continue;
            }
            triples.add(format.getSampleRate());
            triples.add(channelsOf(format));
            triples.add(bits);
        }
        int[] flat = new int[triples.size()];
        for (int i = 0; i < flat.length; i++) {
            flat[i] = triples.get(i);
        }
        return flat;
    }

    /** Sets the format the USB output passes on untouched; false if none of its attributes match. */
    @TargetApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE) // usbOutput() finds nothing before it, so `engaged` stays null
    synchronized boolean engage(int rate, int channels, int bits) {
        AudioDeviceInfo usb = usbOutput();
        if (usb == null) {
            return false;
        }
        for (AudioMixerAttributes attributes : audio.getSupportedMixerAttributes(usb)) {
            AudioFormat format = attributes.getFormat();
            if (attributes.getMixerBehavior() != AudioMixerAttributes.MIXER_BEHAVIOR_BIT_PERFECT
                    || format.getSampleRate() != rate
                    || channelsOf(format) != channels
                    || bitsOf(format) != bits) {
                continue;
            }
            if (audio.setPreferredMixerAttributes(media, usb, attributes)) {
                engaged = usb;
                return true;
            }
            Log.w(TAG, "Android refused " + attributes);
            return false;
        }
        return false;
    }

    /** Gives the output back to the system's mixer; harmless if it never was taken. */
    @TargetApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE) // usbOutput() finds nothing before it, so `engaged` stays null
    synchronized void release() {
        if (engaged == null) {
            return;
        }
        // False when the device is gone, which took the setting with it.
        if (!audio.clearPreferredMixerAttributes(media, engaged)) {
            Log.w(TAG, "Android didn't clear the preferred mixer attributes");
        }
        engaged = null;
    }

    /** Only Android 14 and later has bit-perfect mode, and only for USB outputs. */
    private AudioDeviceInfo usbOutput() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            return null;
        }
        for (AudioDeviceInfo device : audio.getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
            int type = device.getType();
            if (type == AudioDeviceInfo.TYPE_USB_DEVICE || type == AudioDeviceInfo.TYPE_USB_HEADSET) {
                return device;
            }
        }
        return null;
    }

    private static int channelsOf(AudioFormat format) {
        return Integer.bitCount(format.getChannelMask() != 0 ? format.getChannelMask() : format.getChannelIndexMask());
    }

    private static int bitsOf(AudioFormat format) {
        switch (format.getEncoding()) {
            case AudioFormat.ENCODING_PCM_16BIT:
                return PCM_16;
            case AudioFormat.ENCODING_PCM_FLOAT:
                return PCM_FLOAT;
            default:
                return 0;
        }
    }
}
