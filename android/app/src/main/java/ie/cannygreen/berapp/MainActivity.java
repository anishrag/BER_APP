package ie.cannygreen.berapp;

import android.Manifest;
import android.annotation.SuppressLint;
import android.app.NativeActivity;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCallback;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattDescriptor;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothManager;
import android.bluetooth.BluetoothProfile;
import android.content.ContentValues;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.os.Build;
import android.os.Bundle;
import android.provider.MediaStore;
import android.util.Log;
import android.view.WindowInsets;
import android.view.WindowInsetsController;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.UUID;

/**
 * Thin subclass of {@link NativeActivity}. Two jobs:
 *
 * <ol>
 *   <li><b>Immersive mode</b> — hide the system bars so the egui surface owns
 *       the whole screen (re-applied on focus regain).</li>
 *   <li><b>Bluetooth laser bridge</b> — attach to the paired Leica DISTO over
 *       BLE and stream its distance readings into the Rust app. We do the BLE on
 *       the Java side (Android's own {@code BluetoothGatt}) because it's the
 *       well-trodden, debuggable path; the Rust side just receives via two
 *       {@code native} methods. No scanning — we connect to the already-bonded
 *       device, so only {@code BLUETOOTH_CONNECT} is needed (no location).</li>
 * </ol>
 *
 * Readings/state flow Java → Rust only; the Rust {@code LaserSource} drains them
 * exactly like the desktop btleplug source. The activity owns the connection
 * lifecycle (connect on resume, close on pause) and auto-reconnects (the Disto
 * sleeps between measurements), so the app is simply "receiving" while open.
 */
public class MainActivity extends NativeActivity {
    private static final String TAG = "ber_app";

    // Leica DISTO vendor GATT service + its distance characteristic (notifies a
    // little-endian float of metres), and the standard CCCD descriptor.
    private static final UUID SERVICE_UUID =
            UUID.fromString("3ab10100-f831-4395-b29d-570977d5bf94");
    private static final UUID DISTANCE_UUID =
            UUID.fromString("3ab10101-f831-4395-b29d-570977d5bf94");
    private static final UUID CCCD_UUID =
            UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");

    // Status codes mirrored in number_entry::bluetooth (Android backend).
    private static final int STATUS_IDLE = 0;
    private static final int STATUS_CONNECTING = 1;
    private static final int STATUS_CONNECTED = 2;
    private static final int STATUS_ERROR = 3;
    private static final int STATUS_NO_DEVICE = 4;
    private static final int STATUS_NO_PERMISSION = 5;
    private static final int STATUS_BT_OFF = 6;

    private static final int REQ_BLUETOOTH = 1001;

    private BluetoothGatt gatt;

    static {
        // NativeActivity dlopen's libber_app.so from native code, which does
        // NOT register it with the Java classloader's native-library list — so
        // the `native` methods below would fail to resolve (UnsatisfiedLinkError).
        // Loading it here via the JVM registers its JNI symbols for lookup;
        // dlopen is idempotent, so this is the same already-loaded library.
        System.loadLibrary("ber_app");
    }

    private static final int REQ_PHOTO = 1002;

    // The MediaStore row the camera writes the full-size capture into, kept
    // between launching the camera and its result. We copy it into a private
    // cache file, hand that path to Rust, then delete the gallery row so the
    // assessor's photo library isn't cluttered (scoped storage lets us delete
    // our own insert).
    private Uri pendingPhotoUri;

    // Implemented in Rust (libber_app.so) — see number_entry::bluetooth's
    // `Java_..._nativeOn*` exports (laser) and ui::widgets::photo's
    // `Java_..._nativeOnPhoto*` exports (camera).
    private static native void nativeOnDistance(double metres);

    private static native void nativeOnStatus(int code);

    private static native void nativeOnPhotoCaptured(String path);

    private static native void nativeOnPhotoCancelled();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        hideSystemBars();
    }

    @Override
    protected void onResume() {
        super.onResume();
        startLaser();
    }

    @Override
    protected void onPause() {
        super.onPause();
        stopLaser();
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            hideSystemBars();
        }
    }

    private void hideSystemBars() {
        WindowInsetsController controller = getWindow().getDecorView().getWindowInsetsController();
        if (controller != null) {
            controller.hide(WindowInsets.Type.systemBars());
            controller.setSystemBarsBehavior(
                    WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
        }
    }

    // ── Bluetooth laser ─────────────────────────────────────────────────────

    private boolean hasBtPermission() {
        // BLUETOOTH_CONNECT is a runtime permission on API 31+. On API 30 the
        // legacy BLUETOOTH permission is install-time (declared in the manifest).
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            return checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT)
                    == PackageManager.PERMISSION_GRANTED;
        }
        return true;
    }

    @SuppressLint("MissingPermission")
    private void startLaser() {
        // onResume can fire more than once; don't open a second GATT connection
        // to the same device (two clients can drop notifications). autoConnect
        // keeps the existing one alive/reconnecting.
        if (gatt != null) {
            return;
        }
        if (!hasBtPermission()) {
            nativeOnStatus(STATUS_NO_PERMISSION);
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                requestPermissions(
                        new String[] {Manifest.permission.BLUETOOTH_CONNECT}, REQ_BLUETOOTH);
            }
            return;
        }
        BluetoothManager manager = (BluetoothManager) getSystemService(Context.BLUETOOTH_SERVICE);
        BluetoothAdapter adapter = manager != null ? manager.getAdapter() : null;
        if (adapter == null || !adapter.isEnabled()) {
            nativeOnStatus(STATUS_BT_OFF);
            return;
        }
        // No scan: pick the bonded device whose name looks like a DISTO.
        BluetoothDevice target = null;
        for (BluetoothDevice d : adapter.getBondedDevices()) {
            String name = d.getName();
            if (name != null && name.toUpperCase().contains("DISTO")) {
                target = d;
                break;
            }
        }
        if (target == null) {
            nativeOnStatus(STATUS_NO_DEVICE);
            return;
        }
        nativeOnStatus(STATUS_CONNECTING);
        Log.i(TAG, "laser: connecting to bonded DISTO " + target.getName());
        // autoConnect = true: reconnect whenever the (battery-saving) Disto wakes.
        gatt = target.connectGatt(this, true, gattCallback);
    }

    @SuppressLint("MissingPermission")
    private void stopLaser() {
        if (gatt != null) {
            gatt.close();
            gatt = null;
        }
        nativeOnStatus(STATUS_IDLE);
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == REQ_BLUETOOTH) {
            if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
                startLaser();
            } else {
                nativeOnStatus(STATUS_NO_PERMISSION);
            }
        }
    }

    // ── Camera ──────────────────────────────────────────────────────────────

    /**
     * Called FROM Rust (ui::widgets::photo's JNI launch) when the assessor taps
     * an "add photo" button. Launches the system camera; the captured photo's
     * path returns asynchronously via {@link #nativeOnPhotoCaptured} (or
     * {@link #nativeOnPhotoCancelled}). {@code quality} is the per-site default
     * (0=Low … 3=Full) — the system camera owns flash/quality, so it's only
     * logged for now (a future CameraX capture screen would honour it).
     *
     * We route the full-size image through a MediaStore output URI (no
     * FileProvider/androidx needed): the camera writes there, we copy it into
     * private cache, then delete the gallery row. Runs the launch on the UI
     * thread — the call arrives on Rust's render thread.
     */
    public void requestPhoto(int quality) {
        Log.i(TAG, "camera: requestPhoto(quality=" + quality + ")");
        runOnUiThread(
                () -> {
                    try {
                        ContentValues values = new ContentValues();
                        values.put(
                                MediaStore.Images.Media.DISPLAY_NAME,
                                "berapp_" + System.currentTimeMillis() + ".jpg");
                        values.put(MediaStore.Images.Media.MIME_TYPE, "image/jpeg");
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                            values.put(
                                    MediaStore.Images.Media.RELATIVE_PATH, "Pictures/BERApp");
                        }
                        pendingPhotoUri =
                                getContentResolver()
                                        .insert(
                                                MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
                                                values);
                        if (pendingPhotoUri == null) {
                            Log.e(TAG, "camera: could not create MediaStore output");
                            nativeOnPhotoCancelled();
                            return;
                        }
                        Intent intent = new Intent(MediaStore.ACTION_IMAGE_CAPTURE);
                        intent.putExtra(MediaStore.EXTRA_OUTPUT, pendingPhotoUri);
                        intent.addFlags(
                                Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                                        | Intent.FLAG_GRANT_READ_URI_PERMISSION);
                        startActivityForResult(intent, REQ_PHOTO);
                    } catch (Exception e) {
                        Log.e(TAG, "camera: launch failed", e);
                        nativeOnPhotoCancelled();
                    }
                });
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQ_PHOTO) {
            return;
        }
        Uri uri = pendingPhotoUri;
        pendingPhotoUri = null;
        if (resultCode != RESULT_OK || uri == null) {
            Log.i(TAG, "camera: cancelled (result " + resultCode + ")");
            if (uri != null) {
                getContentResolver().delete(uri, null, null);
            }
            nativeOnPhotoCancelled();
            return;
        }
        // Copy the captured image into private cache, then drop the gallery row.
        try {
            File dir = new File(getCacheDir(), "camera");
            dir.mkdirs();
            File out = new File(dir, "capture_" + System.currentTimeMillis() + ".jpg");
            try (InputStream in = getContentResolver().openInputStream(uri);
                    OutputStream os = new FileOutputStream(out)) {
                byte[] buf = new byte[64 * 1024];
                int n;
                while ((n = in.read(buf)) > 0) {
                    os.write(buf, 0, n);
                }
            }
            getContentResolver().delete(uri, null, null);
            Log.i(TAG, "camera: saved to " + out.getAbsolutePath());
            // Write a fast subsampled thumbnail next to the capture, at the
            // "<path>.thumb.jpg" convention the Rust side probes for (ingest_photo).
            // Best-effort: a failure just means the interim preview is the full
            // image until the background job's own thumbnail lands.
            writeFastThumbnail(out);
            nativeOnPhotoCaptured(out.getAbsolutePath());
        } catch (Exception e) {
            Log.e(TAG, "camera: failed to copy capture", e);
            getContentResolver().delete(uri, null, null);
            nativeOnPhotoCancelled();
        }
    }

    /**
     * Write a small JPEG thumbnail of {@code src} to {@code src + ".thumb.jpg"}
     * using a SUBSAMPLED decode ({@link BitmapFactory.Options#inSampleSize}) — it
     * skips most pixels at the native level, so it's far faster than decoding the
     * full ~12 MP image. The Rust photo ingest probes for this sibling file and
     * uses it as the interim light preview while the full image is processed in
     * the background. Best-effort: any failure is logged and ignored.
     */
    private void writeFastThumbnail(File src) {
        final int target = 512; // long-edge px for the preview
        try {
            BitmapFactory.Options bounds = new BitmapFactory.Options();
            bounds.inJustDecodeBounds = true;
            BitmapFactory.decodeFile(src.getAbsolutePath(), bounds);
            int longEdge = Math.max(bounds.outWidth, bounds.outHeight);
            if (longEdge <= 0) {
                return;
            }
            int sample = 1;
            while (longEdge / (sample * 2) >= target) {
                sample *= 2;
            }
            BitmapFactory.Options opts = new BitmapFactory.Options();
            opts.inSampleSize = sample;
            Bitmap bmp = BitmapFactory.decodeFile(src.getAbsolutePath(), opts);
            if (bmp == null) {
                return;
            }
            File thumb = new File(src.getAbsolutePath() + ".thumb.jpg");
            try (OutputStream os = new FileOutputStream(thumb)) {
                bmp.compress(Bitmap.CompressFormat.JPEG, 75, os);
            }
            bmp.recycle();
            Log.i(TAG, "camera: wrote thumbnail " + thumb.getAbsolutePath());
        } catch (Exception e) {
            Log.w(TAG, "camera: thumbnail failed (ignored)", e);
        }
    }

    private final BluetoothGattCallback gattCallback =
            new BluetoothGattCallback() {
                @Override
                @SuppressLint("MissingPermission")
                public void onConnectionStateChange(BluetoothGatt g, int status, int newState) {
                    if (newState == BluetoothProfile.STATE_CONNECTED) {
                        nativeOnStatus(STATUS_CONNECTING);
                        Log.i(TAG, "laser: connected, discovering services");
                        g.discoverServices();
                    } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                        // autoConnect keeps the gatt alive and will reconnect.
                        nativeOnStatus(STATUS_IDLE);
                        Log.i(TAG, "laser: disconnected (status " + status + ")");
                    }
                }

                @Override
                @SuppressLint("MissingPermission")
                public void onServicesDiscovered(BluetoothGatt g, int status) {
                    if (status != BluetoothGatt.GATT_SUCCESS) {
                        nativeOnStatus(STATUS_ERROR);
                        return;
                    }
                    BluetoothGattService service = g.getService(SERVICE_UUID);
                    BluetoothGattCharacteristic ch =
                            service != null ? service.getCharacteristic(DISTANCE_UUID) : null;
                    if (ch == null) {
                        Log.w(TAG, "laser: DISTO distance characteristic not found");
                        nativeOnStatus(STATUS_ERROR);
                        return;
                    }
                    g.setCharacteristicNotification(ch, true);
                    enableNotifications(g, ch);
                    nativeOnStatus(STATUS_CONNECTED);
                    Log.i(TAG, "laser: subscribed; listening for readings");
                }

                // API 33+ delivers the value directly.
                @Override
                public void onCharacteristicChanged(
                        BluetoothGatt g, BluetoothGattCharacteristic ch, byte[] value) {
                    Log.d(TAG, "laser: notify (v33) " + ch.getUuid());
                    if (DISTANCE_UUID.equals(ch.getUuid())) {
                        handleReading(value);
                    }
                }

                // API ≤ 32 delivers it via getValue().
                @Override
                @SuppressWarnings("deprecation")
                public void onCharacteristicChanged(
                        BluetoothGatt g, BluetoothGattCharacteristic ch) {
                    Log.d(TAG, "laser: notify (legacy) " + ch.getUuid());
                    if (DISTANCE_UUID.equals(ch.getUuid())) {
                        handleReading(ch.getValue());
                    }
                }

                @Override
                public void onDescriptorWrite(
                        BluetoothGatt g, BluetoothGattDescriptor descriptor, int status) {
                    Log.i(TAG, "laser: CCCD write status " + status + " (0 = success)");
                }
            };

    @SuppressWarnings("deprecation")
    @SuppressLint("MissingPermission")
    private void enableNotifications(BluetoothGatt g, BluetoothGattCharacteristic ch) {
        int props = ch.getProperties();
        boolean indicate = (props & BluetoothGattCharacteristic.PROPERTY_INDICATE) != 0;
        Log.i(
                TAG,
                "laser: char properties=0x" + Integer.toHexString(props) + " indicate=" + indicate);
        BluetoothGattDescriptor cccd = ch.getDescriptor(CCCD_UUID);
        if (cccd == null) {
            Log.w(TAG, "laser: no CCCD descriptor on the distance characteristic");
            return;
        }
        // The DISTO delivers via INDICATE, not NOTIFY, on some characteristics —
        // pick the CCCD value from the characteristic's properties (this is what
        // btleplug does on desktop). Deprecated setValue+writeDescriptor still
        // works on minSdk 30.
        cccd.setValue(
                indicate
                        ? BluetoothGattDescriptor.ENABLE_INDICATION_VALUE
                        : BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE);
        boolean ok = g.writeDescriptor(cccd);
        Log.i(TAG, "laser: writeDescriptor(CCCD) indicate=" + indicate + " requested=" + ok);
    }

    private void handleReading(byte[] value) {
        if (value != null && value.length >= 4) {
            float metres = ByteBuffer.wrap(value).order(ByteOrder.LITTLE_ENDIAN).getFloat();
            Log.i(TAG, "laser: reading " + metres + " m raw=" + java.util.Arrays.toString(value));
            if (!Float.isNaN(metres) && !Float.isInfinite(metres)) {
                nativeOnDistance(metres);
            }
        } else {
            Log.w(TAG, "laser: short/empty reading payload");
        }
    }
}
