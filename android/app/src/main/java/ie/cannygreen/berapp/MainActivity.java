package ie.cannygreen.berapp;

import android.Manifest;
import android.annotation.SuppressLint;
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
import android.media.MediaRecorder;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.os.Build;
import android.os.Bundle;
import android.provider.MediaStore;
import android.util.Log;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;

import com.google.androidgamesdk.GameActivity;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.UUID;

/**
 * Thin subclass of {@link GameActivity} (migrated from NativeActivity so the
 * native soft keyboard works — GameActivity's GameTextInput has a real
 * InputConnection, which NativeActivity lacks). Two jobs:
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
public class MainActivity extends GameActivity {
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
    private static final int REQ_AUDIO = 1003;

    private BluetoothGatt gatt;

    // MediaRecorder for voice-note capture, driven synchronously FROM Rust over
    // JNI (start/stop below). Held between start and stop so stop() can finalize
    // the .m4a on the same thread the Rust caller invoked start on.
    private MediaRecorder audioRecorder;

    // WiFi-connectivity watcher: fires nativeOnNetworkUp() when the internet
    // becomes reachable so queued bug reports / cloud sync flush promptly.
    private ConnectivityManager connectivityManager;
    private ConnectivityManager.NetworkCallback networkCallback;

    static {
        // GameActivity loads the library named by the android.app.lib_name
        // meta-data itself (in onCreate, via System.loadLibrary) — but only
        // after this class is initialized. Loading it here too keeps the
        // `native` methods below resolvable regardless of that ordering;
        // loadLibrary is idempotent, so it's the same library either way.
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

    // Implemented in Rust (libber_app.so) — fired when WiFi/internet becomes
    // reachable so the bug-report uploader / cloud sync can flush its queue.
    private static native void nativeOnNetworkUp();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        hideSystemBars();
        registerNetworkWatcher();
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        if (connectivityManager != null && networkCallback != null) {
            try {
                connectivityManager.unregisterNetworkCallback(networkCallback);
            } catch (Exception e) {
                Log.w(TAG, "net: unregister callback failed (ignored)", e);
            }
        }
        connectivityManager = null;
        networkCallback = null;
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
        } else if (requestCode == REQ_AUDIO) {
            // No automatic retry: the assessor re-taps record once granted.
            boolean granted =
                    grantResults.length > 0
                            && grantResults[0] == PackageManager.PERMISSION_GRANTED;
            Log.i(TAG, "audio: RECORD_AUDIO permission granted=" + granted);
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
    /**
     * Toggle the keep-screen-on window flag from native code. The app sets it
     * while in use and clears it after ~10 min idle (see src/keep_awake.rs), so
     * the device's own screen timeout takes back over. App-scoped: a window flag,
     * never a global system setting.
     */
    public void setKeepAwake(boolean on) {
        runOnUiThread(
                () -> {
                    if (on) {
                        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
                    } else {
                        getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
                    }
                });
    }

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

    // ── Audio recording ─────────────────────────────────────────────────────

    /**
     * Called FROM Rust over JNI when the assessor taps "record voice note".
     * Starts a {@link MediaRecorder} writing an AAC/MPEG-4 (.m4a) file at
     * {@code path} and returns {@code true} if recording began. Returns
     * {@code false} without recording if the {@code RECORD_AUDIO} permission is
     * missing (we request it and the assessor re-taps record once granted —
     * mirroring how {@link #startLaser} returns after requesting Bluetooth), or
     * if the recorder failed to start.
     *
     * <p>Runs on the CALLING thread (no {@code runOnUiThread}): the Rust caller
     * relies on {@link #stopAudioRecording} finalizing the file synchronously on
     * the same thread. Unlike the camera, this doesn't wait on an Activity
     * result.
     */
    public boolean startAudioRecording(String path) {
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO)
                != PackageManager.PERMISSION_GRANTED) {
            Log.i(TAG, "audio: RECORD_AUDIO not granted, requesting");
            requestPermissions(new String[] {Manifest.permission.RECORD_AUDIO}, REQ_AUDIO);
            return false;
        }
        try {
            MediaRecorder recorder;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                recorder = new MediaRecorder(this);
            } else {
                recorder = new MediaRecorder();
            }
            recorder.setAudioSource(MediaRecorder.AudioSource.MIC);
            recorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4);
            recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);
            recorder.setAudioEncodingBitRate(96000);
            recorder.setAudioSamplingRate(44100);
            recorder.setOutputFile(path);
            recorder.prepare();
            recorder.start();
            audioRecorder = recorder;
            Log.i(TAG, "audio: recording to " + path);
            return true;
        } catch (Exception e) {
            Log.e(TAG, "audio: failed to start recording", e);
            if (audioRecorder != null) {
                try {
                    audioRecorder.release();
                } catch (Exception ignored) {
                }
                audioRecorder = null;
            }
            return false;
        }
    }

    /**
     * Called FROM Rust over JNI to stop the in-progress recording. Finalizes the
     * .m4a file synchronously so it's ready when this method returns. A
     * stop-too-soon exception (recording shorter than a frame) is logged; the
     * recorder is always released and the field cleared.
     */
    public void stopAudioRecording() {
        if (audioRecorder == null) {
            return;
        }
        try {
            audioRecorder.stop();
        } catch (Exception e) {
            Log.e(TAG, "audio: stop failed", e);
        }
        audioRecorder.release();
        audioRecorder = null;
        Log.i(TAG, "audio: recording stopped");
    }

    // ── Connectivity ────────────────────────────────────────────────────────

    /**
     * Register a {@link ConnectivityManager.NetworkCallback} that fires
     * {@link #nativeOnNetworkUp} when a WiFi network with internet becomes
     * available, so queued bug reports / cloud sync flush promptly once WiFi
     * returns. Unregistered in {@link #onDestroy}.
     */
    private void registerNetworkWatcher() {
        connectivityManager =
                (ConnectivityManager) getSystemService(Context.CONNECTIVITY_SERVICE);
        if (connectivityManager == null) {
            Log.w(TAG, "net: no ConnectivityManager");
            return;
        }
        NetworkRequest request =
                new NetworkRequest.Builder()
                        .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                        .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                        .build();
        networkCallback =
                new ConnectivityManager.NetworkCallback() {
                    @Override
                    public void onAvailable(Network network) {
                        Log.i(TAG, "net: WiFi internet available");
                        nativeOnNetworkUp();
                    }
                };
        try {
            connectivityManager.registerNetworkCallback(request, networkCallback);
        } catch (Exception e) {
            Log.w(TAG, "net: register callback failed", e);
            networkCallback = null;
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
