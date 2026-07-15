//! Cloud push — the other half of [`super::sync`]: continuously uploads local
//! BERs to the BERintake backend, so the office-side tooling (BER_deap) always
//! has the latest survey data. "Whenever it can", not just on completion: every
//! save triggers a (debounced) pass, and a retry tick covers offline stretches.
//!
//! Per BER the pass PUTs the stored `data_json` to `bers/{jobId}/data.json` via
//! the presigned URLs `POST /surveyor/jobs/{jobId}/ber/presign` mints, plus any
//! photos not yet uploaded (photos are immutable once attached — each uploads
//! once). Dirtiness is a hash of the stored `data_json` against the
//! `push_state` table (see [`BerRepository::push_state`]); pushing is
//! idempotent, so lost bookkeeping just re-uploads.
//!
//! BERs created from scratch on the tablet have no matching cloud job — the
//! backend answers 404 and the BER is set aside until its content next changes
//! (`/surveyor/jobs/{jobId}/ber/complete` and scratch-job creation are later
//! increments).
//!
//! Reuses [`SyncConfig`] (same `sync.json` / `$BER_SYNC_*`). Boundary: per
//! CLAUDE.md rule 4 all networking + DB access lives here in `src/engine/`.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;

use super::ber_repository::BerRepository;
use super::sync::SyncConfig;

/// How often the idle worker re-checks when something is still unpushed —
/// covers "WiFi came back" without an explicit trigger.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// How long a triggered pass waits for the save flurry (and the background
/// [`super::PersistenceWriter`]) to settle before reading the DB. Also
/// coalesces a burst of triggers into one pass.
const DEBOUNCE: Duration = Duration::from_secs(2);

// ---- Wire types ------------------------------------------------------------

#[derive(Serialize)]
struct PresignRequest {
    photos: Vec<PresignPhoto>,
}

#[derive(Serialize)]
struct PresignPhoto {
    id: String,
    #[serde(rename = "contentType")]
    content_type: &'static str,
}

#[derive(Serialize)]
struct CreateJobRequest<'a> {
    #[serde(rename = "jobId")]
    job_id: &'a str,
    name: &'a str,
    address: &'a str,
    eircode: &'a str,
}

#[derive(Deserialize)]
struct PresignResponse {
    #[serde(rename = "dataUrl")]
    data_url: String,
    #[serde(rename = "photoUrls", default)]
    photo_urls: HashMap<String, String>,
}

// ---- HTTP client -----------------------------------------------------------

struct PushClient {
    cfg: SyncConfig,
    agent: ureq::Agent,
}

/// Why a push attempt failed — 404 (no cloud job for this id) is handled
/// specially: it's the normal state for a scratch-created BER, not an error.
enum PushError {
    NoJob,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for PushError {
    fn from(e: anyhow::Error) -> Self {
        PushError::Other(e)
    }
}

impl PushClient {
    fn new(cfg: SyncConfig) -> Self {
        // Longer read timeout than the pull sync: photo PUTs on a weak field
        // connection (same reasoning as the bug-report uploader).
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build();
        Self { cfg, agent }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    fn presign(
        &self,
        job_id: &str,
        photos: Vec<PresignPhoto>,
    ) -> Result<PresignResponse, PushError> {
        let resp = self
            .agent
            .post(&self.url(&format!("/surveyor/jobs/{job_id}/ber/presign")))
            .set("x-surveyor-key", &self.cfg.api_key)
            .send_json(&PresignRequest { photos });
        match resp {
            Ok(r) => Ok(r.into_json().map_err(anyhow::Error::from)?),
            Err(ureq::Error::Status(404, _)) => Err(PushError::NoJob),
            Err(e) => Err(PushError::Other(e.into())),
        }
    }

    /// Register a cloud job for a from-scratch BER (idempotent server-side —
    /// re-registering an existing job is a 200).
    fn create_job(&self, req: &CreateJobRequest<'_>) -> anyhow::Result<()> {
        self.agent
            .post(&self.url("/surveyor/jobs"))
            .set("x-surveyor-key", &self.cfg.api_key)
            .send_json(req)?;
        Ok(())
    }

    /// PUT bytes to a presigned S3 URL. No key header (auth is in the query
    /// string), but Content-Type MUST match what the backend signed.
    fn put_blob(&self, url: &str, content_type: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.agent
            .put(url)
            .set("Content-Type", content_type)
            .send_bytes(bytes)?;
        Ok(())
    }
}

// ---- One pass ---------------------------------------------------------------

/// Hash of the stored `data_json` used for dirty detection. `DefaultHasher` is
/// only stable within a build — after an app upgrade everything may look dirty
/// once and re-push, which is idempotent and harmless.
pub(crate) fn content_hash(data_json: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data_json.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// The photos of `ber` not yet uploaded, as (id, file_path, content_type).
/// Only photos whose file actually exists on disk — a missing file stays
/// pending (it may be mid-recompress) and is retried next pass.
fn pending_photos(ber: &Ber, already: &HashSet<String>) -> Vec<(String, String, &'static str)> {
    ber.all_photos()
        .into_iter()
        .filter(|p| !already.contains(&p.id.to_string()))
        .filter(|p| std::path::Path::new(&p.file_path).exists())
        .map(|p| {
            let ct = if p.file_path.to_ascii_lowercase().ends_with(".png") {
                "image/png"
            } else {
                "image/jpeg"
            };
            (p.id.to_string(), p.file_path.clone(), ct)
        })
        .collect()
}

/// Push one BER: presign, PUT data.json, PUT pending photos, record the state.
fn push_one(
    client: &PushClient,
    repo: &BerRepository<'_>,
    id: &ItemId,
    data_json: &str,
    hash: &str,
    mut pushed_photos: HashSet<String>,
) -> Result<(), PushError> {
    let ber: Ber = serde_json::from_str(data_json)
        .map_err(|e| PushError::Other(anyhow::anyhow!("unparseable data_json: {e}")))?;
    let pending = pending_photos(&ber, &pushed_photos);

    let presign_photos = pending
        .iter()
        .map(|(id, _, ct)| PresignPhoto {
            id: id.clone(),
            content_type: ct,
        })
        .collect();
    let resp = client.presign(&id.to_string(), presign_photos)?;

    client.put_blob(&resp.data_url, "application/json", data_json.as_bytes())?;

    // Photos are best-effort per file: one unreadable/failed photo shouldn't
    // hold the data.json push (already done) or the other photos hostage. A
    // failed one stays out of `pushed_photos`, so it retries next pass — but
    // only surfaces as an error if it keeps the state permanently incomplete.
    let mut photo_err = None;
    for (photo_id, file_path, ct) in &pending {
        let Some(url) = resp.photo_urls.get(photo_id) else {
            continue; // backend declined the id; nothing to retry against
        };
        let upload = std::fs::read(file_path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| client.put_blob(url, ct, &bytes));
        match upload {
            Ok(()) => {
                pushed_photos.insert(photo_id.clone());
            }
            Err(e) => {
                log::warn!("push: photo {photo_id} of {} failed: {e}", id.0);
                photo_err = Some(e);
            }
        }
    }

    let photos_vec: Vec<String> = pushed_photos.into_iter().collect();
    repo.set_push_state(id, hash, &photos_vec)
        .map_err(|e| PushError::Other(e.into()))?;
    match photo_err {
        // data.json landed and is recorded; the failed photo retries via the
        // hash-independent photo check next pass.
        Some(e) => Err(PushError::Other(e)),
        None => Ok(()),
    }
}

/// Register a cloud job for a queued from-scratch BER, labeled with what the
/// office can recognise it by (the tablet-side name + whatever address/eircode
/// the assessor entered on Key Details).
fn register_job(
    client: &PushClient,
    id: &ItemId,
    name: &str,
    data_json: &str,
) -> Result<(), PushError> {
    let ber: Ber = serde_json::from_str(data_json)
        .map_err(|e| PushError::Other(anyhow::anyhow!("unparseable data_json: {e}")))?;
    client
        .create_job(&CreateJobRequest {
            job_id: &id.to_string(),
            name,
            address: &ber.address,
            eircode: &ber.eircode,
        })
        .map_err(PushError::Other)
}

/// Observable outcome of the most recent push pass (read by the UI).
#[derive(Debug, Clone, Default)]
pub struct PushStatus {
    pub in_progress: bool,
    /// BERs with cloud jobs still awaiting a (re-)push after the last pass.
    pub pending: usize,
    /// Cumulative successful BER pushes this session.
    pub pushed: usize,
    pub last_error: Option<String>,
}

fn run_pass(
    client: &PushClient,
    conn: &super::Db,
    status: &Arc<Mutex<PushStatus>>,
    no_job: &mut HashMap<String, String>,
) {
    status.lock().unwrap().in_progress = true;
    let repo = BerRepository::new(conn);
    let mut pushed_now = 0;
    let mut still_pending = 0;
    let mut last_error = None;

    let rows = match repo.list_raw() {
        Ok(rows) => rows,
        Err(e) => {
            log::error!("push: cannot list BERs: {e}");
            let mut s = status.lock().unwrap();
            s.in_progress = false;
            s.last_error = Some(e.to_string());
            return;
        }
    };

    for (id, name, data_json) in rows {
        let hash = content_hash(&data_json);
        let (pushed_hash, photo_ids) = match repo.push_state(&id) {
            Ok(state) => state.unwrap_or_default(),
            Err(e) => {
                log::warn!("push: push_state for {} unreadable: {e}", id.0);
                Default::default()
            }
        };
        let pushed_set: HashSet<String> = photo_ids.into_iter().collect();

        let dirty = pushed_hash != hash;
        let photos_due = !dirty
            && serde_json::from_str::<Ber>(&data_json)
                .map(|b| !pending_photos(&b, &pushed_set).is_empty())
                .unwrap_or(false);
        if !dirty && !photos_due {
            continue;
        }
        // A scratch BER with no cloud job: don't re-ask the backend until its
        // content changes again (the map remembers the hash that 404'd).
        if no_job.get(&id.to_string()) == Some(&hash) {
            continue;
        }

        match push_one(client, &repo, &id, &data_json, &hash, pushed_set) {
            Ok(()) => {
                pushed_now += 1;
                no_job.remove(&id.to_string());
            }
            Err(PushError::NoJob) => {
                // A from-scratch BER queued for registration gets its cloud
                // job created on the spot, then pushes in the same pass.
                // Everything else (pre-registration scratch BERs) is set
                // aside until its content changes again.
                let queued = repo.job_registration_queued(&id).unwrap_or(false);
                if !queued {
                    log::info!("push: {} has no cloud job (scratch BER?) — set aside", id.0);
                    no_job.insert(id.to_string(), hash);
                    continue;
                }
                match register_job(client, &id, &name, &data_json)
                    .and_then(|()| push_one(client, &repo, &id, &data_json, &hash, HashSet::new()))
                {
                    Ok(()) => {
                        let _ = repo.dequeue_job_registration(&id);
                        pushed_now += 1;
                        no_job.remove(&id.to_string());
                        log::info!("push: registered cloud job for {} and pushed", id.0);
                    }
                    Err(PushError::NoJob) => {
                        // Registration claimed success but presign still 404s —
                        // shouldn't happen; leave queued and surface it.
                        log::warn!("push: {} still has no job after registration", id.0);
                        last_error = Some("registration did not take".into());
                        still_pending += 1;
                    }
                    Err(PushError::Other(e)) => {
                        log::warn!("push: registering {} failed: {e}", id.0);
                        last_error = Some(e.to_string());
                        still_pending += 1;
                    }
                }
            }
            Err(PushError::Other(e)) => {
                log::warn!("push: {} failed: {e}", id.0);
                last_error = Some(e.to_string());
                still_pending += 1;
            }
        }
    }

    let mut s = status.lock().unwrap();
    s.in_progress = false;
    s.pushed += pushed_now;
    s.pending = still_pending;
    if last_error.is_some() {
        s.last_error = last_error;
    } else if pushed_now > 0 {
        s.last_error = None;
    }
}

// ---- Background worker (modeled on sync::SyncWorker) ------------------------

enum Msg {
    PushNow,
}

/// Owns a dedicated thread with its own DB connection + HTTP client. Passes
/// once on spawn, on each (debounced) [`trigger`](Self::trigger), and on a
/// [`RETRY_INTERVAL`] tick while anything is still pending. Dropping it closes
/// the channel; the thread exits after any in-flight pass.
pub struct PushWorker {
    tx: Sender<Msg>,
    _handle: JoinHandle<()>,
    status: Arc<Mutex<PushStatus>>,
}

impl PushWorker {
    /// Spawn the worker if sync is configured; `None` otherwise (fully offline —
    /// BERs stay local and push once configured on a later run).
    pub fn spawn(db_path: PathBuf, config_file: PathBuf) -> Option<Self> {
        let cfg = SyncConfig::load(&config_file)?;
        let conn = match super::open_db(&db_path) {
            Ok(c) => c,
            Err(e) => {
                log::error!("push: cannot open db, push disabled: {e}");
                return None;
            }
        };
        let client = PushClient::new(cfg);
        let status = Arc::new(Mutex::new(PushStatus::default()));
        let status_thread = Arc::clone(&status);
        let (tx, rx) = mpsc::channel::<Msg>();

        let handle = std::thread::Builder::new()
            .name("ber-push".into())
            .spawn(move || {
                let mut no_job: HashMap<String, String> = HashMap::new();
                run_pass(&client, &conn, &status_thread, &mut no_job);
                loop {
                    match rx.recv_timeout(RETRY_INTERVAL) {
                        Ok(Msg::PushNow) => {
                            // Debounce: swallow the trigger burst (and give the
                            // background writer time to land the save), then
                            // push once.
                            while let Ok(Msg::PushNow) = rx.recv_timeout(DEBOUNCE) {}
                            run_pass(&client, &conn, &status_thread, &mut no_job);
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            // Idle wake: only bother if something's still due.
                            if status_thread.lock().unwrap().pending > 0 {
                                run_pass(&client, &conn, &status_thread, &mut no_job);
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("spawn push thread");

        Some(Self {
            tx,
            _handle: handle,
            status,
        })
    }

    /// Ask the worker to push soon (call after any save). Debounced; no-op
    /// after shutdown.
    pub fn trigger(&self) {
        let _ = self.tx.send(Msg::PushNow);
    }

    /// Snapshot of the latest pass's outcome.
    pub fn status(&self) -> PushStatus {
        self.status.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic_and_content_sensitive() {
        let a = content_hash("{\"x\":1}");
        assert_eq!(a, content_hash("{\"x\":1}"));
        assert_ne!(a, content_hash("{\"x\":2}"));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn pending_photos_skips_pushed_and_missing_files() {
        use ber_model::ber::common::Photo;
        let existing = std::env::temp_dir().join(format!("ber_push_{}.jpg", std::process::id()));
        std::fs::write(&existing, b"jpg").unwrap();

        let mut ber = Ber::new();
        let on_disk = Photo {
            id: Default::default(),
            file_path: existing.to_string_lossy().into_owned(),
            thumbnail_path: None,
            description: None,
            side: None,
        };
        let missing = Photo {
            id: Default::default(),
            file_path: "/nonexistent/gone.jpg".into(),
            thumbnail_path: None,
            description: None,
            side: None,
        };
        let already = Photo {
            id: Default::default(),
            file_path: existing.to_string_lossy().into_owned(),
            thumbnail_path: None,
            description: None,
            side: None,
        };
        let already_id = already.id.to_string();
        ber.photos = vec![on_disk.clone(), missing, already];

        let pushed: HashSet<String> = [already_id].into();
        let pending = pending_photos(&ber, &pushed);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, on_disk.id.to_string());
        assert_eq!(pending[0].2, "image/jpeg");

        let _ = std::fs::remove_file(&existing);
    }
}
