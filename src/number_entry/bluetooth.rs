//! Bluetooth laser distance meter input — a Leica DISTO (e.g. the D2) feeding
//! measurements into the number-entry buffer over BLE.
//!
//! UI-free, like the rest of [`crate::number_entry`]: a [`LaserSource`] owns the
//! background connection and exposes a [`Status`] plus a stream of [`Reading`]s
//! the UI drains each frame. The renderer never touches BLE — it polls.
//!
//! ## Protocol
//!
//! Leica DISTO meters expose a vendor BLE GATT service. The one characteristic
//! we need streams the measured distance as a little-endian IEEE-754 `f32` in
//! **metres** via notifications; the assessor points and presses the button on
//! the device itself and the reading arrives here. (Service/characteristic
//! UUIDs are the publicly documented DISTO values.)
//!
//! ## Build
//!
//! The actual radio code is behind the `bluetooth` cargo feature (it pulls in
//! `btleplug` + `tokio`). With the feature off, [`LaserSource`] still exists but
//! reports [`Status::Unsupported`] and yields no readings, so the rest of the
//! app — and the test suite — builds and runs untouched. On Android the feature
//! additionally needs the `droidplug` companion and the BLUETOOTH_SCAN /
//! BLUETOOTH_CONNECT runtime permissions (see `android/README.md`).

/// A distance reading from the laser, in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub metres: f32,
}

/// The laser link's current state, surfaced to the UI for its status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The `bluetooth` feature isn't compiled in.
    Unsupported,
    /// Built, but not listening — the idle/resting state.
    Idle,
    /// Attaching to the already-paired DISTO + subscribing (no scan).
    Connecting,
    /// Connected and subscribed — receiving; the assessor presses the laser.
    Connected,
    /// The last attempt failed; carries a short reason for the status line.
    Error(String),
}

impl Status {
    /// Whether the link is attaching or live (drives whether the UI keeps
    /// polling/repainting, and the keypad-mode tear-down).
    pub fn is_active(&self) -> bool {
        matches!(self, Status::Connecting | Status::Connected)
    }
}

// ── Android: BLE lives on the Java side (MainActivity) ──────────────────────
// Android never uses btleplug (cross-compiling its droidplug companion is a
// thornier path — see android/README.md). Instead the Kotlin/Java activity owns
// the BluetoothGatt connection to the paired DISTO and pushes each reading +
// status code into the globals below via two JNI `native` methods, which the
// UI then drains exactly like the desktop source. Raw JNI pointers keep this
// dependency-free (no `jni` crate).
#[cfg(target_os = "android")]
mod backend {
    use super::{Reading, Status};
    use std::os::raw::c_void;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Mutex;

    /// Latest readings pushed from Java since the last UI poll.
    static READINGS: Mutex<Vec<Reading>> = Mutex::new(Vec::new());
    /// Last status code reported from Java (see [`status_from_code`]).
    static STATUS_CODE: AtomicI32 = AtomicI32::new(0);

    /// Map the Java status code to a [`Status`]. Kept in sync with the `STATUS_*`
    /// constants in `MainActivity.java`.
    fn status_from_code(code: i32) -> Status {
        match code {
            1 => Status::Connecting,
            2 => Status::Connected,
            4 => Status::Error("No paired DISTO found — pair it in Bluetooth settings".into()),
            5 => Status::Error("Bluetooth permission needed".into()),
            6 => Status::Error("Bluetooth is off".into()),
            3 => Status::Error("Bluetooth error".into()),
            _ => Status::Idle,
        }
    }

    /// On Android the connection is owned by the Java activity (it attaches to
    /// the bonded DISTO on resume), so this is a thin reader over the globals
    /// the JNI callbacks feed; `connect`/`disconnect` are no-ops.
    pub struct LaserSource;

    impl LaserSource {
        pub fn new() -> Self {
            Self
        }
        pub fn status(&self) -> Status {
            status_from_code(STATUS_CODE.load(Ordering::SeqCst))
        }
        pub fn connect(&mut self) {}
        pub fn disconnect(&mut self) {}
        pub fn poll(&mut self) -> Option<Reading> {
            let mut q = READINGS.lock().unwrap();
            // Most recent reading wins; older unconfirmed ones are superseded.
            let last = q.pop();
            q.clear();
            last
        }
    }

    /// `MainActivity.nativeOnDistance(double)` — a fresh measurement (metres).
    /// Raw pointers for the JNIEnv*/jclass we don't use; ABI matches the JNI
    /// `void (JNIEnv*, jclass, jdouble)` signature.
    #[no_mangle]
    pub extern "C" fn Java_ie_cannygreen_berapp_MainActivity_nativeOnDistance(
        _env: *mut c_void,
        _class: *mut c_void,
        metres: f64,
    ) {
        if let Ok(mut q) = READINGS.lock() {
            q.push(Reading {
                metres: metres as f32,
            });
        }
    }

    /// `MainActivity.nativeOnStatus(int)` — a connection-state change.
    #[no_mangle]
    pub extern "C" fn Java_ie_cannygreen_berapp_MainActivity_nativeOnStatus(
        _env: *mut c_void,
        _class: *mut c_void,
        code: i32,
    ) {
        STATUS_CODE.store(code, Ordering::SeqCst);
    }
}

// ── Desktop: the real btleplug-backed source ────────────────────────────────
#[cfg(all(not(target_os = "android"), feature = "bluetooth"))]
mod backend {
    use super::{Reading, Status};
    use btleplug::api::{Central, Manager as _, Peripheral as _};
    use btleplug::platform::{Adapter, Manager, Peripheral};
    use futures::StreamExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;
    use uuid::Uuid;

    /// Leica DISTO vendor GATT service — used to recognise an already-paired
    /// device among the adapter's known peripherals (no active scan).
    const SERVICE_UUID: Uuid = Uuid::from_u128(0x3ab10100_f831_4395_b29d_570977d5bf94);
    /// Leica DISTO distance characteristic (under [`SERVICE_UUID`]): notifies a
    /// little-endian `f32` of metres.
    const DISTANCE_UUID: Uuid = Uuid::from_u128(0x3ab10101_f831_4395_b29d_570977d5bf94);

    /// Background BLE connection to a Leica DISTO laser meter.
    pub struct LaserSource {
        status: Arc<Mutex<Status>>,
        readings: Receiver<Reading>,
        tx: Sender<Reading>,
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl LaserSource {
        pub fn new() -> Self {
            let (tx, readings) = mpsc::channel();
            Self {
                status: Arc::new(Mutex::new(Status::Idle)),
                readings,
                tx,
                stop: Arc::new(AtomicBool::new(false)),
                worker: None,
            }
        }

        pub fn status(&self) -> Status {
            self.status.lock().unwrap().clone()
        }

        /// Begin listening: attach to the already-paired DISTO and subscribe,
        /// in the background. Idempotent — a no-op while a worker is running.
        pub fn connect(&mut self) {
            if self.worker.as_ref().is_some_and(|h| !h.is_finished()) {
                return;
            }
            self.stop.store(false, Ordering::SeqCst);
            *self.status.lock().unwrap() = Status::Connecting;
            let tx = self.tx.clone();
            let status = self.status.clone();
            let stop = self.stop.clone();
            self.worker = Some(std::thread::spawn(move || {
                // A small current-thread runtime dedicated to this connection.
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        *status.lock().unwrap() = Status::Error(e.to_string());
                        return;
                    }
                };
                let result = rt.block_on(run(tx, status.clone(), stop));
                let mut s = status.lock().unwrap();
                // Don't clobber a more specific Error already set inside run().
                if !matches!(*s, Status::Error(_)) {
                    *s = match result {
                        Ok(()) => Status::Idle,
                        Err(e) => Status::Error(e.to_string()),
                    };
                }
            }));
        }

        /// Ask the worker to drop the connection and stop. Returns to `Idle`.
        pub fn disconnect(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            *self.status.lock().unwrap() = Status::Idle;
        }

        /// The most recent reading since the last poll (intermediate readings
        /// are superseded — a fresh measurement replaces an unconfirmed one).
        pub fn poll(&mut self) -> Option<Reading> {
            let mut last = None;
            while let Ok(r) = self.readings.try_recv() {
                last = Some(r);
            }
            last
        }
    }

    impl Drop for LaserSource {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    /// Locate the DISTO among the adapter's ALREADY-KNOWN peripherals — no
    /// active scan. Relies on the meter being paired/connected at the OS level
    /// (BlueZ then exposes it via the object manager). Matched by its vendor
    /// service UUID, with the advertised name as a fallback.
    async fn find_disto(central: &Adapter) -> btleplug::Result<Option<Peripheral>> {
        for p in central.peripherals().await? {
            if let Ok(Some(props)) = p.properties().await {
                let name = props.local_name.clone().unwrap_or_default();
                log::debug!(
                    "known peripheral: name={name:?} services={:?}",
                    props.services
                );
                let by_service = props.services.contains(&SERVICE_UUID);
                let by_name = name.to_uppercase().contains("DISTO");
                if by_service || by_name {
                    log::info!(
                        "matched DISTO {name:?} (by_service={by_service}, by_name={by_name})"
                    );
                    return Ok(Some(p));
                }
            }
        }
        Ok(None)
    }

    /// Attach to the paired DISTO → subscribe → forward notifications until told
    /// to stop or the device drops. No scanning: the app is purely in receiving
    /// mode and the laser pushes each reading.
    async fn run(
        tx: Sender<Reading>,
        status: Arc<Mutex<Status>>,
        stop: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let set = |s: Status| *status.lock().unwrap() = s;

        let manager = Manager::new().await?;
        let central = manager
            .adapters()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no Bluetooth adapter"))?;
        log::info!("BLE adapter ready; looking for a known DISTO (no scan)");

        // The device should already be known to the OS; allow a few ticks for
        // it to surface, then give up with a clear message.
        let mut device = None;
        for _ in 0..5 {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if let Some(p) = find_disto(&central).await? {
                device = Some(p);
                break;
            }
            log::debug!("no DISTO among known peripherals yet; retrying");
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        let device = device.ok_or_else(|| {
            anyhow::anyhow!("No paired DISTO found — pair it in your Bluetooth settings first")
        })?;

        // Open the GATT link only if the OS hasn't already (e.g. a paired but
        // idle device); a connected one we just subscribe to.
        let already = device.is_connected().await?;
        log::info!("attaching to DISTO (already connected: {already})");
        if !already {
            device.connect().await?;
        }
        device.discover_services().await?;
        let distance = device
            .characteristics()
            .into_iter()
            .find(|c| c.uuid == DISTANCE_UUID)
            .ok_or_else(|| anyhow::anyhow!("DISTO distance characteristic not found"))?;
        device.subscribe(&distance).await?;

        set(Status::Connected);
        log::info!("subscribed to DISTO distance; listening for readings");
        let mut notifications = device.notifications().await?;
        loop {
            tokio::select! {
                item = notifications.next() => match item {
                    Some(n) if n.uuid == DISTANCE_UUID && n.value.len() >= 4 => {
                        let metres = f32::from_le_bytes([n.value[0], n.value[1], n.value[2], n.value[3]]);
                        log::debug!("reading: {metres} m (raw {:02x?})", n.value);
                        if metres.is_finite() {
                            let _ = tx.send(Reading { metres });
                        }
                    }
                    Some(n) => log::debug!("notification on other char {} ({} bytes)", n.uuid, n.value.len()),
                    None => break, // notification stream ended → device gone
                },
                _ = tokio::time::sleep(Duration::from_millis(300)) => {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }
        // Leave the OS-level pairing/connection intact (the assessor manages the
        // device); just stop receiving. Re-entering Laser mode re-subscribes.
        let _ = device.unsubscribe(&distance).await;
        Ok(())
    }
}

// ── Desktop without the feature: an inert stub with the same surface ────────
#[cfg(all(not(target_os = "android"), not(feature = "bluetooth")))]
mod backend {
    use super::{Reading, Status};

    /// Stand-in when the `bluetooth` feature is off: always [`Status::Unsupported`],
    /// never yields a reading. Lets the UI compile and run the laser mode (it
    /// just shows "not built in") without the radio dependencies.
    pub struct LaserSource;

    impl LaserSource {
        pub fn new() -> Self {
            Self
        }
        pub fn status(&self) -> Status {
            Status::Unsupported
        }
        pub fn connect(&mut self) {}
        pub fn disconnect(&mut self) {}
        pub fn poll(&mut self) -> Option<Reading> {
            None
        }
    }
}

pub use backend::LaserSource;

impl Default for LaserSource {
    fn default() -> Self {
        LaserSource::new()
    }
}
