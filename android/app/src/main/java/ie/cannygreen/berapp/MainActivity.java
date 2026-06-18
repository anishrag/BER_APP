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
import android.content.Context;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;
import android.util.Log;
import android.view.WindowInsets;
import android.view.WindowInsetsController;

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

    // Implemented in Rust (libber_app.so), loaded by NativeActivity before
    // onResume — see number_entry::bluetooth's `Java_..._nativeOn*` exports.
    private static native void nativeOnDistance(double metres);

    private static native void nativeOnStatus(int code);

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
                    if (DISTANCE_UUID.equals(ch.getUuid())) {
                        handleReading(value);
                    }
                }

                // API ≤ 32 delivers it via getValue().
                @Override
                @SuppressWarnings("deprecation")
                public void onCharacteristicChanged(
                        BluetoothGatt g, BluetoothGattCharacteristic ch) {
                    if (DISTANCE_UUID.equals(ch.getUuid())) {
                        handleReading(ch.getValue());
                    }
                }
            };

    @SuppressWarnings("deprecation")
    @SuppressLint("MissingPermission")
    private void enableNotifications(BluetoothGatt g, BluetoothGattCharacteristic ch) {
        BluetoothGattDescriptor cccd = ch.getDescriptor(CCCD_UUID);
        if (cccd != null) {
            // Deprecated setValue+writeDescriptor still works on minSdk 30.
            cccd.setValue(BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE);
            g.writeDescriptor(cccd);
        }
    }

    private void handleReading(byte[] value) {
        if (value != null && value.length >= 4) {
            float metres = ByteBuffer.wrap(value).order(ByteOrder.LITTLE_ENDIAN).getFloat();
            if (!Float.isNaN(metres) && !Float.isInfinite(metres)) {
                nativeOnDistance(metres);
            }
        }
    }
}
