package ie.cannygreen.berapp;

import android.Manifest;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.os.Bundle;
import android.util.Log;
import android.view.Gravity;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.LinearLayout;

import androidx.activity.ComponentActivity;
import androidx.camera.core.CameraSelector;
import androidx.camera.core.ImageCapture;
import androidx.camera.core.ImageCaptureException;
import androidx.camera.core.Preview;
import androidx.camera.lifecycle.ProcessCameraProvider;
import androidx.camera.view.PreviewView;
import androidx.core.content.ContextCompat;

import com.google.common.util.concurrent.ListenableFuture;

import java.io.File;

/**
 * In-app CameraX capture screen, launched for-result by
 * {@link MainActivity#requestPhoto(int)}. Replaces the previous hand-off to the
 * system camera app (whose flash we couldn't control) with our own preview +
 * shutter + <b>flash toggle</b> (Off / Auto / On).
 *
 * <p>On a successful capture it writes the JPEG into the app's private cache and
 * returns its absolute path via the result intent's {@code "path"} extra with
 * {@link #RESULT_OK}; a cancel/back or any error returns {@link #RESULT_CANCELED}.
 * {@link MainActivity#onActivityResult} turns that into the Rust
 * {@code nativeOnPhotoCaptured} / {@code nativeOnPhotoCancelled} callbacks.
 *
 * <p>Extends {@link ComponentActivity} because CameraX binds its use-cases to a
 * {@link androidx.lifecycle.LifecycleOwner}, which {@code ComponentActivity} is.
 */
public class CameraActivity extends ComponentActivity {
    private static final String TAG = "ber_app";
    public static final String EXTRA_QUALITY = "quality";
    public static final String EXTRA_PATH = "path";
    private static final int REQ_CAMERA_PERM = 2001;

    private PreviewView previewView;
    private Button flashButton;
    private ImageCapture imageCapture;
    // Cycles Off → Auto → On; the label + ImageCapture flash mode follow it.
    private int flashMode = ImageCapture.FLASH_MODE_OFF;
    private boolean capturing = false;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // Default to cancelled: a back-press / early finish reports no capture.
        setResult(RESULT_CANCELED);
        setContentView(buildUi());

        if (checkSelfPermission(Manifest.permission.CAMERA)
                != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {Manifest.permission.CAMERA}, REQ_CAMERA_PERM);
        } else {
            startCamera();
        }
    }

    /** Programmatic UI: full-bleed preview with a flash toggle (top) and a
     * cancel + shutter row (bottom) — no XML layout/theme dependency. */
    private FrameLayout buildUi() {
        FrameLayout root = new FrameLayout(this);
        root.setLayoutParams(
                new ViewGroup.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT));
        root.setBackgroundColor(Color.BLACK);

        previewView = new PreviewView(this);
        previewView.setLayoutParams(
                new FrameLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT));
        root.addView(previewView);

        flashButton = new Button(this);
        flashButton.setText(flashLabel());
        FrameLayout.LayoutParams flashLp =
                new FrameLayout.LayoutParams(
                        ViewGroup.LayoutParams.WRAP_CONTENT,
                        ViewGroup.LayoutParams.WRAP_CONTENT);
        flashLp.gravity = Gravity.TOP | Gravity.END;
        flashLp.setMargins(32, 32, 32, 32);
        flashButton.setLayoutParams(flashLp);
        flashButton.setOnClickListener(v -> cycleFlash());
        root.addView(flashButton);

        LinearLayout bottom = new LinearLayout(this);
        bottom.setOrientation(LinearLayout.HORIZONTAL);
        bottom.setGravity(Gravity.CENTER_VERTICAL);
        FrameLayout.LayoutParams bottomLp =
                new FrameLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.WRAP_CONTENT);
        bottomLp.gravity = Gravity.BOTTOM;
        bottomLp.setMargins(32, 32, 32, 48);
        bottom.setLayoutParams(bottomLp);

        Button cancel = new Button(this);
        cancel.setText("Cancel");
        cancel.setOnClickListener(v -> finish());
        LinearLayout.LayoutParams cancelLp =
                new LinearLayout.LayoutParams(
                        ViewGroup.LayoutParams.WRAP_CONTENT,
                        ViewGroup.LayoutParams.WRAP_CONTENT);
        cancel.setLayoutParams(cancelLp);
        bottom.addView(cancel);

        Button shutter = new Button(this);
        shutter.setText("● Capture");
        LinearLayout.LayoutParams shutterLp =
                new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f);
        shutter.setLayoutParams(shutterLp);
        shutter.setOnClickListener(v -> takePhoto());
        bottom.addView(shutter);

        root.addView(bottom);
        return root;
    }

    private String flashLabel() {
        switch (flashMode) {
            case ImageCapture.FLASH_MODE_ON:
                return "Flash: On";
            case ImageCapture.FLASH_MODE_AUTO:
                return "Flash: Auto";
            default:
                return "Flash: Off";
        }
    }

    /** Off → Auto → On → Off. Applies to the live ImageCapture immediately. */
    private void cycleFlash() {
        switch (flashMode) {
            case ImageCapture.FLASH_MODE_OFF:
                flashMode = ImageCapture.FLASH_MODE_AUTO;
                break;
            case ImageCapture.FLASH_MODE_AUTO:
                flashMode = ImageCapture.FLASH_MODE_ON;
                break;
            default:
                flashMode = ImageCapture.FLASH_MODE_OFF;
                break;
        }
        flashButton.setText(flashLabel());
        if (imageCapture != null) {
            imageCapture.setFlashMode(flashMode);
        }
    }

    private void startCamera() {
        ListenableFuture<ProcessCameraProvider> future = ProcessCameraProvider.getInstance(this);
        future.addListener(
                () -> {
                    try {
                        ProcessCameraProvider provider = future.get();
                        Preview preview = new Preview.Builder().build();
                        preview.setSurfaceProvider(previewView.getSurfaceProvider());
                        imageCapture =
                                new ImageCapture.Builder()
                                        .setCaptureMode(ImageCapture.CAPTURE_MODE_MINIMIZE_LATENCY)
                                        .setFlashMode(flashMode)
                                        .build();
                        provider.unbindAll();
                        provider.bindToLifecycle(
                                this,
                                CameraSelector.DEFAULT_BACK_CAMERA,
                                preview,
                                imageCapture);
                    } catch (Exception e) {
                        Log.e(TAG, "camera: failed to start CameraX", e);
                        finish();
                    }
                },
                ContextCompat.getMainExecutor(this));
    }

    private void takePhoto() {
        if (imageCapture == null || capturing) {
            return;
        }
        capturing = true;
        File dir = new File(getCacheDir(), "camera");
        dir.mkdirs();
        File out = new File(dir, "capture_" + System.currentTimeMillis() + ".jpg");
        ImageCapture.OutputFileOptions opts =
                new ImageCapture.OutputFileOptions.Builder(out).build();
        imageCapture.setFlashMode(flashMode);
        imageCapture.takePicture(
                opts,
                ContextCompat.getMainExecutor(this),
                new ImageCapture.OnImageSavedCallback() {
                    @Override
                    public void onImageSaved(ImageCapture.OutputFileResults results) {
                        Log.i(TAG, "camera: captured " + out.getAbsolutePath());
                        Intent data = new Intent();
                        data.putExtra(EXTRA_PATH, out.getAbsolutePath());
                        setResult(RESULT_OK, data);
                        finish();
                    }

                    @Override
                    public void onError(ImageCaptureException exc) {
                        Log.e(TAG, "camera: capture failed", exc);
                        capturing = false;
                        finish();
                    }
                });
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != REQ_CAMERA_PERM) {
            return;
        }
        if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            startCamera();
        } else {
            Log.i(TAG, "camera: CAMERA permission denied");
            finish();
        }
    }
}
