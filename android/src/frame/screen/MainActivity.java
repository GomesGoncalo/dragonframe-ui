package frame.screen;

import android.app.Activity;
import android.content.Intent;
import android.graphics.Color;
import android.hardware.Sensor;
import android.hardware.SensorEvent;
import android.hardware.SensorEventListener;
import android.hardware.SensorManager;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.View;
import android.view.WindowManager;
import android.webkit.RenderProcessGoneDetail;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;

/**
 * Shows the frame's screen, a page served by frameserver on the frame itself, edge to edge.
 *
 * Everything the screen does lives in that page. This only gives it the whole display,
 * keeps the display on, and keeps trying when the page cannot be loaded (after a restart
 * the server may come up a little later than this does).
 *
 * It also listens to the light sensor without doing anything with what it hears. The
 * sensor only measures while something listens, and Android stops listening when the
 * screen is off; frameserver reads the measurements to know when a dark room is lit again.
 */
public class MainActivity extends Activity {
    private static final String PAGE = "http://127.0.0.1:8080/frame";
    private static final long RETRY_MS = 5000;

    private final Handler handler = new Handler(Looper.getMainLooper());
    private final SensorEventListener lightListener = new SensorEventListener() {
        @Override
        public void onSensorChanged(SensorEvent event) {
        }

        @Override
        public void onAccuracyChanged(Sensor sensor, int accuracy) {
        }
    };
    private WebView web;
    private String page = PAGE;

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON | WindowManager.LayoutParams.FLAG_FULLSCREEN);
        SensorManager sensors = (SensorManager) getSystemService(SENSOR_SERVICE);
        Sensor light = sensors == null ? null : sensors.getDefaultSensor(Sensor.TYPE_LIGHT);
        if (light != null) {
            sensors.registerListener(lightListener, light, SensorManager.SENSOR_DELAY_NORMAL);
        }
        open(getIntent());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        open(intent);
    }

    /** Show the page the intent names, or the usual one. */
    private void open(Intent intent) {
        if (intent != null && intent.getData() != null) {
            page = intent.getDataString();
        }
        build();
    }

    /** Make a fresh WebView and load the page into it. */
    private void build() {
        handler.removeCallbacksAndMessages(null);
        if (web != null) {
            web.destroy();
        }
        web = new WebView(this);
        web.setBackgroundColor(Color.BLACK);
        WebSettings settings = web.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(true);
        // The page sounds a chime for calendar events, with nobody there to touch it first.
        settings.setMediaPlaybackRequiresUserGesture(false);
        web.setWebViewClient(new WebViewClient() {
            @Override
            public void onReceivedError(WebView view, WebResourceRequest request, WebResourceError error) {
                if (request.isForMainFrame()) {
                    retry();
                }
            }

            @Override
            public void onReceivedHttpError(WebView view, WebResourceRequest request, WebResourceResponse response) {
                if (request.isForMainFrame()) {
                    retry();
                }
            }

            @Override
            public boolean onRenderProcessGone(WebView view, RenderProcessGoneDetail detail) {
                // The page's process died (the system may take it back when memory is short): start over.
                build();
                return true;
            }
        });
        setContentView(web);
        hideBars();
        web.loadUrl(page);
    }

    private void retry() {
        handler.removeCallbacksAndMessages(null);
        handler.postDelayed(new Runnable() {
            @Override
            public void run() {
                web.loadUrl(page);
            }
        }, RETRY_MS);
    }

    private void hideBars() {
        getWindow().getDecorView().setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                        | View.SYSTEM_UI_FLAG_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                        | View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                        | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
    }

    @Override
    public void onWindowFocusChanged(boolean focused) {
        super.onWindowFocusChanged(focused);
        if (focused) {
            hideBars();
        }
    }

    @Override
    public void onBackPressed() {
        // There is nowhere to go back to; the page has its own way to the original screen.
    }

    @Override
    protected void onDestroy() {
        SensorManager sensors = (SensorManager) getSystemService(SENSOR_SERVICE);
        if (sensors != null) {
            sensors.unregisterListener(lightListener);
        }
        handler.removeCallbacksAndMessages(null);
        if (web != null) {
            web.destroy();
        }
        super.onDestroy();
    }
}
