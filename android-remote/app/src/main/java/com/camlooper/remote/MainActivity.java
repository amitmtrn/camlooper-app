package com.camlooper.remote;

import android.Manifest;
import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Bundle;
import android.view.View;
import android.view.WindowManager;
import android.content.ActivityNotFoundException;
import android.content.ClipData;
import android.webkit.PermissionRequest;
import android.webkit.ValueCallback;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Toast;

import androidx.webkit.WebViewAssetLoader;

import java.util.ArrayList;
import java.util.List;

/**
 * CamLooper Remote (Android wrapper for remote-web/index.html).
 *
 * The page is bundled in the APK and served by WebViewAssetLoader under
 * https://camlooper.com/remote-app/ . Requests to that path never leave the phone; the page's
 * origin is therefore https://camlooper.com, which relay v2 (api.camlooper.com) already allows
 * for CORS and WebSocket upgrades. Everything else (the relay API, WebRTC media) goes to the
 * network as usual.
 */
public class MainActivity extends Activity {
    static final String HOST = "camlooper.com";
    static final String PATH = "/remote-app/";
    static final String START_URL = "https://" + HOST + PATH + "index.html";
    private static final int REQ_MEDIA = 42;
    private static final int REQ_FILE = 43;

    private WebView webView;
    private PermissionRequest pendingRequest;
    /** Callback of the page's pending <input type="file"> (one at a time). */
    private ValueCallback<Uri[]> fileCallback;

    @SuppressLint("SetJavaScriptEnabled")
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);

        final WebViewAssetLoader assetLoader = new WebViewAssetLoader.Builder()
                .setDomain(HOST)
                .addPathHandler(PATH, new WebViewAssetLoader.AssetsPathHandler(this))
                .build();

        if (BuildConfigHelper.isDebuggable(this)) {
            WebView.setWebContentsDebuggingEnabled(true); // chrome://inspect on the XPS
        }

        webView = new WebView(this);
        webView.setBackgroundColor(0xFF0F0F14);
        WebSettings s = webView.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setMediaPlaybackRequiresUserGesture(false);
        s.setAllowFileAccess(false);
        s.setAllowContentAccess(false);
        s.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);

        webView.setWebViewClient(new WebViewClient() {
            @Override
            public WebResourceResponse shouldInterceptRequest(WebView view, WebResourceRequest request) {
                Uri u = request.getUrl();
                WebResourceResponse r = assetLoader.shouldInterceptRequest(u);
                if (r != null) return r;
                // Never let the WebView fetch the real camlooper.com site under our origin.
                if (HOST.equals(u.getHost()) && "https".equals(u.getScheme())) {
                    return new WebResourceResponse("text/plain", "utf-8", 404, "Not Found", null, null);
                }
                return null;
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
                Uri u = request.getUrl();
                if (HOST.equals(u.getHost()) && u.getPath() != null && u.getPath().startsWith(PATH)) {
                    return false;
                }
                // Any other link (e.g. help links) opens in the browser, never inside the app.
                try {
                    startActivity(new Intent(Intent.ACTION_VIEW, u));
                } catch (Exception ignored) {
                }
                return true;
            }
        });

        webView.setWebChromeClient(new WebChromeClient() {
            @Override
            public void onPermissionRequest(final PermissionRequest request) {
                runOnUiThread(() -> handlePermissionRequest(request));
            }

            @Override
            public void onPermissionRequestCanceled(PermissionRequest request) {
                if (pendingRequest == request) pendingRequest = null;
            }

            /**
             * <input type="file" accept="video/*"> ("Loop a video"). Opens the system picker
             * (Photos/Gallery/Files) for one video; the WebView reads the returned content:// URI
             * itself through the File/Blob API, nothing is copied by the app.
             */
            @Override
            public boolean onShowFileChooser(WebView view, ValueCallback<Uri[]> callback,
                                             FileChooserParams params) {
                if (fileCallback != null) fileCallback.onReceiveValue(null);
                fileCallback = callback;
                Intent pick = new Intent(Intent.ACTION_GET_CONTENT);
                pick.addCategory(Intent.CATEGORY_OPENABLE);
                pick.setType(pickerMimeType(params));
                pick.putExtra(Intent.EXTRA_ALLOW_MULTIPLE,
                        params.getMode() == FileChooserParams.MODE_OPEN_MULTIPLE);
                try {
                    startActivityForResult(Intent.createChooser(pick, getString(R.string.pick_video)), REQ_FILE);
                } catch (ActivityNotFoundException e) {
                    fileCallback = null;
                    callback.onReceiveValue(null);
                    Toast.makeText(MainActivity.this, R.string.no_picker, Toast.LENGTH_LONG).show();
                }
                return true;
            }
        });

        webView.setSystemUiVisibility(View.SYSTEM_UI_FLAG_LAYOUT_STABLE);
        setContentView(webView);

        if (savedInstanceState != null) {
            webView.restoreState(savedInstanceState);
        } else {
            webView.loadUrl(START_URL);
        }
    }

    private void handlePermissionRequest(PermissionRequest request) {
        Uri origin = request.getOrigin();
        if (origin == null || !HOST.equals(origin.getHost())) {
            request.deny();
            return;
        }
        List<String> needed = new ArrayList<>();
        for (String res : request.getResources()) {
            if (PermissionRequest.RESOURCE_VIDEO_CAPTURE.equals(res)
                    && checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
                needed.add(Manifest.permission.CAMERA);
            }
            if (PermissionRequest.RESOURCE_AUDIO_CAPTURE.equals(res)
                    && checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                needed.add(Manifest.permission.RECORD_AUDIO);
            }
        }
        if (needed.isEmpty()) {
            grantAllowed(request);
        } else {
            if (pendingRequest != null) pendingRequest.deny();
            pendingRequest = request;
            requestPermissions(needed.toArray(new String[0]), REQ_MEDIA);
        }
    }

    /** Grants the media resources whose Android permission is held (camera-only if the mic was refused). */
    private void grantAllowed(PermissionRequest request) {
        List<String> ok = new ArrayList<>();
        for (String res : request.getResources()) {
            if (PermissionRequest.RESOURCE_VIDEO_CAPTURE.equals(res)
                    && checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
                ok.add(res);
            } else if (PermissionRequest.RESOURCE_AUDIO_CAPTURE.equals(res)
                    && checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
                ok.add(res);
            }
        }
        if (ok.isEmpty()) {
            request.deny();
            Toast.makeText(this, R.string.perm_denied, Toast.LENGTH_LONG).show();
        } else {
            request.grant(ok.toArray(new String[0]));
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == REQ_MEDIA && pendingRequest != null) {
            PermissionRequest r = pendingRequest;
            pendingRequest = null;
            grantAllowed(r);
        }
    }

    /** The page asks for video/*; fall back to video/* for anything odd (never open-ended). */
    static String pickerMimeType(WebChromeClient.FileChooserParams params) {
        String[] types = params.getAcceptTypes();
        if (types != null) {
            for (String t : types) {
                if (t != null && t.trim().startsWith("video/")) return t.trim();
            }
        }
        return "video/*";
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode != REQ_FILE) {
            super.onActivityResult(requestCode, resultCode, data);
            return;
        }
        ValueCallback<Uri[]> cb = fileCallback;
        fileCallback = null;
        if (cb == null) return;
        Uri[] result = null;
        if (resultCode == RESULT_OK && data != null) {
            ClipData clip = data.getClipData();
            if (clip != null && clip.getItemCount() > 0) {
                result = new Uri[clip.getItemCount()];
                for (int i = 0; i < clip.getItemCount(); i++) result[i] = clip.getItemAt(i).getUri();
            } else if (data.getData() != null) {
                result = new Uri[]{data.getData()};
            }
        }
        cb.onReceiveValue(result); // null = cancelled; the page must handle "no file picked"
    }

    @Override
    public void onBackPressed() {
        if (webView != null && webView.canGoBack()) webView.goBack();
        else super.onBackPressed();
    }

    @Override
    protected void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        if (webView != null) webView.saveState(outState);
    }

    @Override
    protected void onDestroy() {
        if (fileCallback != null) {
            fileCallback.onReceiveValue(null);
            fileCallback = null;
        }
        if (webView != null) {
            webView.destroy();
            webView = null;
        }
        super.onDestroy();
    }
}
