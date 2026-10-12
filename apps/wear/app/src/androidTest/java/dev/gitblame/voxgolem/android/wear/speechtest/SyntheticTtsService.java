package dev.gitblame.voxgolem.android.wear.speechtest;

import android.media.AudioFormat;
import android.content.Intent;
import android.os.IBinder;
import android.os.SystemClock;
import android.speech.tts.SynthesisCallback;
import android.speech.tts.SynthesisRequest;
import android.speech.tts.TextToSpeech;
import android.speech.tts.TextToSpeechService;
import android.speech.tts.Voice;
import java.util.Collections;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;

public final class SyntheticTtsService extends TextToSpeechService {
    public static final String VOICE = "synthetic-test-iso3-local";
    public static volatile int startupDelay;
    public static final AtomicInteger creations = new AtomicInteger();
    public static final AtomicInteger voiceQueries = new AtomicInteger();
    public static final AtomicInteger activeBindings = new AtomicInteger();
    private final AtomicBoolean stopped = new AtomicBoolean();

    @Override public void onCreate() {
        creations.incrementAndGet(); super.onCreate(); SystemClock.sleep(startupDelay);
    }
    @Override public IBinder onBind(Intent intent) { activeBindings.set(1); return super.onBind(intent); }
    @Override public boolean onUnbind(Intent intent) { activeBindings.set(0); return super.onUnbind(intent); }
    @Override public void onDestroy() { activeBindings.set(0); super.onDestroy(); }

    @Override protected String[] onGetLanguage() { return new String[] {"eng", "GBR", "default"}; }
    @Override protected int onIsLanguageAvailable(String language, String country, String variant) {
        return "eng".equals(language) || "en".equals(language)
            ? TextToSpeech.LANG_COUNTRY_AVAILABLE : TextToSpeech.LANG_NOT_SUPPORTED;
    }
    @Override protected int onLoadLanguage(String language, String country, String variant) {
        return onIsLanguageAvailable(language, country, variant);
    }
    @Override public List<Voice> onGetVoices() {
        return Collections.singletonList(new Voice(VOICE, new Locale("eng", "GBR", "default"), Voice.QUALITY_NORMAL,
            Voice.LATENCY_NORMAL, false, Collections.singleton("synthetic-test")));
    }
    @Override public String onGetDefaultVoiceNameFor(String language, String country, String variant) {
        voiceQueries.incrementAndGet(); return VOICE;
    }
    @Override public int onIsValidVoiceName(String name) {
        return VOICE.equals(name) ? TextToSpeech.SUCCESS : TextToSpeech.ERROR;
    }
    @Override public int onLoadVoice(String name) { return onIsValidVoiceName(name); }
    @Override protected void onStop() { stopped.set(true); }
    @Override protected void onSynthesizeText(SynthesisRequest request, SynthesisCallback callback) {
        stopped.set(false);
        if (callback.start(16_000, AudioFormat.ENCODING_PCM_16BIT, 1) != TextToSpeech.SUCCESS) return;
        byte[] silentPcm = new byte[3_200];
        if (!stopped.get() && callback.audioAvailable(silentPcm, 0, silentPcm.length) == TextToSpeech.SUCCESS) callback.done();
        else callback.error(TextToSpeech.ERROR_SYNTHESIS);
    }
}
