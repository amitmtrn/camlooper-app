package com.camlooper.remote;

import android.content.Context;
import android.content.pm.ApplicationInfo;

final class BuildConfigHelper {
    private BuildConfigHelper() {}

    static boolean isDebuggable(Context c) {
        return (c.getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;
    }
}
