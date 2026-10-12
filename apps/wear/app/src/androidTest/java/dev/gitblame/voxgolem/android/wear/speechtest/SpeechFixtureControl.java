package dev.gitblame.voxgolem.android.wear.speechtest;

import android.content.ComponentName;
import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Binder;
import android.os.Bundle;

public final class SpeechFixtureControl extends ContentProvider {
    @Override public boolean onCreate() { return true; }
    @Override public Bundle call(String method, String argument, Bundle extras) {
        try {
            int targetUid = getContext().getPackageManager().getApplicationInfo("dev.gitblame.voxgolem.android", 0).uid;
            if (Binder.getCallingUid() != targetUid && Binder.getCallingUid() != getContext().getApplicationInfo().uid)
                throw new SecurityException("Only the instrumented application can control this test fixture");
            if ("stats".equals(method)) {
                Bundle result = new Bundle();
                result.putInt("creations", SyntheticTtsService.creations.get());
                result.putInt("voiceQueries", SyntheticTtsService.voiceQueries.get());
                result.putInt("activeBindings", SyntheticTtsService.activeBindings.get());
                return result;
            }
            if (!"enable".equals(method) && !"enable-delayed".equals(method) && !"disable".equals(method))
                throw new IllegalArgumentException("Unknown fixture operation");
            if (!"disable".equals(method)) {
                SyntheticTtsService.startupDelay = "enable-delayed".equals(method) ? 3_000 : 0;
                SyntheticTtsService.creations.set(0); SyntheticTtsService.voiceQueries.set(0);
            }
            long identity = Binder.clearCallingIdentity();
            try {
                getContext().getPackageManager().setComponentEnabledSetting(new ComponentName(getContext(), SyntheticTtsService.class),
                    !"disable".equals(method) ? PackageManager.COMPONENT_ENABLED_STATE_ENABLED : PackageManager.COMPONENT_ENABLED_STATE_DISABLED,
                    PackageManager.DONT_KILL_APP);
            } finally { Binder.restoreCallingIdentity(identity); }
            return new Bundle();
        } catch (PackageManager.NameNotFoundException e) { throw new IllegalStateException(e); }
    }
    @Override public Cursor query(Uri uri, String[] projection, String selection, String[] arguments, String order) { return null; }
    @Override public String getType(Uri uri) { return null; }
    @Override public Uri insert(Uri uri, ContentValues values) { throw new UnsupportedOperationException(); }
    @Override public int delete(Uri uri, String selection, String[] arguments) { throw new UnsupportedOperationException(); }
    @Override public int update(Uri uri, ContentValues values, String selection, String[] arguments) { throw new UnsupportedOperationException(); }
}
