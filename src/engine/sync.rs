//! Cloud sync with the BERintake backend (the "surveyor" API).
//!
//! Offline-first: the tablet's SQLite stays the working store. This module adds
//! a background worker that, when connectivity allows, PULLS the jobs the office
//! has marked ready (`confirmed`) and imports each as a local `ber` row keyed by
//! the SAME `jobId` — so one field assessment maps 1:1 to one intake record.
//!
//! Boundary: per CLAUDE.md rule 4 all DB access is here via `BerRepository`. The
//! HTTP client is blocking `ureq` (the app is threads-not-async), and lives in
//! `src/engine/` as the I/O boundary — nothing above it touches the network.
//!
//! The push-back path (upload the finished BER to S3, mark `assessed`) lands in
//! a follow-up; this increment is the pull/import half.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;

use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::{ItemId, Photo};

use super::ber_repository::{BerRecord, BerRepository};

// ---- Config ---------------------------------------------------------------

/// Where to reach the surveyor API and the shared key to authenticate with.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub base_url: String,
    pub api_key: String,
}

#[derive(Deserialize)]
struct RawConfig {
    #[serde(rename = "baseUrl", default)]
    base_url: String,
    #[serde(rename = "apiKey", default)]
    api_key: String,
}

impl SyncConfig {
    /// Load from `$BER_SYNC_URL` / `$BER_SYNC_KEY` if both are set, else from a
    /// `sync.json` file (`{ "baseUrl": "...", "apiKey": "..." }`). Returns `None`
    /// when unconfigured — sync then stays off and the app is fully usable
    /// offline. Mirrors the `$BER_TUNING`-else-file convention in `config.rs`.
    pub fn load(config_file: &Path) -> Option<Self> {
        if let (Ok(base_url), Ok(api_key)) =
            (std::env::var("BER_SYNC_URL"), std::env::var("BER_SYNC_KEY"))
        {
            if !base_url.is_empty() && !api_key.is_empty() {
                return Some(Self { base_url, api_key });
            }
        }
        let text = std::fs::read_to_string(config_file).ok()?;
        let raw: RawConfig = serde_json::from_str(&text).ok()?;
        if raw.base_url.is_empty() || raw.api_key.is_empty() {
            return None;
        }
        Some(Self {
            base_url: raw.base_url,
            api_key: raw.api_key,
        })
    }
}

// ---- Wire types (subset of the cloud responses we consume) ----------------

#[derive(Deserialize)]
struct ListResponse {
    jobs: Vec<JobSummary>,
}

#[derive(Deserialize)]
struct JobSummary {
    #[serde(rename = "jobId")]
    job_id: String,
}

#[derive(Deserialize)]
struct JobDetail {
    #[serde(rename = "berSeed")]
    ber_seed: Option<BerSeed>,
    #[serde(rename = "satelliteImageUrl")]
    satellite_image_url: Option<String>,
}

/// Only the seed fields we can map into `Ber` today. Unmapped fields
/// (propertyType, heatingSystem, extensions, …) are ignored by serde for now —
/// they stay in the cloud record for the assessor to confirm on-site.
#[derive(Deserialize, Default)]
struct BerSeed {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    eircode: Option<String>,
    #[serde(rename = "numberOfStoreys", default)]
    number_of_storeys: Option<i32>,
}

// ---- HTTP client ----------------------------------------------------------

struct SyncClient {
    cfg: SyncConfig,
    agent: ureq::Agent,
}

impl SyncClient {
    fn new(cfg: SyncConfig) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build();
        Self { cfg, agent }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    fn list_confirmed(&self) -> anyhow::Result<Vec<JobSummary>> {
        let resp = self
            .agent
            .get(&self.url("/surveyor/jobs?status=confirmed"))
            .set("x-surveyor-key", &self.cfg.api_key)
            .call()?;
        let parsed: ListResponse = resp.into_json()?;
        Ok(parsed.jobs)
    }

    fn get_job(&self, job_id: &str) -> anyhow::Result<JobDetail> {
        let resp = self
            .agent
            .get(&self.url(&format!("/surveyor/jobs/{job_id}")))
            .set("x-surveyor-key", &self.cfg.api_key)
            .call()?;
        Ok(resp.into_json()?)
    }

    /// GET arbitrary bytes (used for the presigned satellite-image URL, which is
    /// already authenticated in the query string — no key header).
    fn download(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        let resp = self.agent.get(url).call()?;
        let mut buf = Vec::new();
        resp.into_reader().read_to_end(&mut buf)?;
        Ok(buf)
    }
}

// ---- Import (pull) --------------------------------------------------------

fn import_confirmed(
    client: &SyncClient,
    conn: &super::Db,
    photos_dir: &Path,
) -> anyhow::Result<usize> {
    let repo = BerRepository::new(conn);
    // What we already hold locally (by jobId == ber.id) — don't re-import.
    let existing: std::collections::HashSet<String> =
        repo.list()?.into_iter().map(|s| s.id.to_string()).collect();

    let mut imported = 0;
    for summary in client.list_confirmed()? {
        if existing.contains(&summary.job_id) {
            continue;
        }
        match import_one(client, &repo, photos_dir, &summary.job_id) {
            Ok(()) => imported += 1,
            Err(e) => log::warn!("sync: import of {} failed: {e}", summary.job_id),
        }
    }
    Ok(imported)
}

fn import_one(
    client: &SyncClient,
    repo: &BerRepository<'_>,
    photos_dir: &Path,
    job_id: &str,
) -> anyhow::Result<()> {
    let detail = client.get_job(job_id)?;
    let id: ItemId = job_id.parse()?; // jobId is a v4 UUID — becomes ber.id
    let seed = detail.ber_seed.unwrap_or_default();

    // Best-effort: pull the satellite image into the photos dir as a Photo.
    let site_satellite_image = match &detail.satellite_image_url {
        Some(url) => match download_satellite(client, photos_dir, url) {
            Ok(photo) => Some(photo),
            Err(e) => {
                log::warn!("sync: satellite download for {job_id} failed: {e}");
                None
            }
        },
        None => None,
    };

    // Seed the externally-populated fields; everything else defaults and is
    // filled by the assessor on-site. `Ber::new()` gives number_of_storeys = 1.
    let mut ber = Ber {
        id,
        address: seed.address.unwrap_or_default(),
        eircode: seed.eircode.unwrap_or_default(),
        ..Ber::new()
    };
    if let Some(n) = seed.number_of_storeys {
        if n > 0 {
            ber.number_of_storeys = n;
        }
    }
    ber.site_satellite_image = site_satellite_image;

    let name = if !ber.address.is_empty() {
        ber.address.clone()
    } else {
        ber.eircode.clone()
    };
    let record = BerRecord {
        id,
        name,
        date_assessed: Utc::now().date_naive(),
        data: ber,
    };
    repo.upsert(&record)?;
    log::info!("sync: imported job {job_id}");
    Ok(())
}

fn download_satellite(client: &SyncClient, photos_dir: &Path, url: &str) -> anyhow::Result<Photo> {
    let bytes = client.download(url)?;
    std::fs::create_dir_all(photos_dir)?;
    // Canonicalize so the stored path loads as a `file://` image regardless of
    // cwd (same reason `ingest_photo` does it).
    let dir = std::fs::canonicalize(photos_dir)?;
    let photo_id = ItemId::default();
    let file_path = dir.join(format!("{}_satellite.jpg", photo_id.0));
    std::fs::write(&file_path, &bytes)?;
    Ok(Photo {
        id: photo_id,
        file_path: file_path.to_string_lossy().into_owned(),
        thumbnail_path: None,
        description: Some("Site satellite image (from intake)".into()),
        side: None,
    })
}

// ---- Background worker (modeled on PersistenceWriter) ---------------------

/// Observable outcome of the most recent sync pass (read by the UI).
#[derive(Debug, Clone, Default)]
pub struct SyncStatus {
    pub in_progress: bool,
    pub last_import_ok: bool,
    pub imported: usize,
    pub last_error: Option<String>,
}

enum SyncMsg {
    ImportNow,
}

/// Owns a dedicated thread with its own DB connection + HTTP client. Pulls once
/// on spawn, then on each [`trigger_import`](Self::trigger_import). Dropping it
/// closes the channel; the thread finishes any in-flight pull and exits.
pub struct SyncWorker {
    tx: Sender<SyncMsg>,
    _handle: JoinHandle<()>,
    status: Arc<Mutex<SyncStatus>>,
}

impl SyncWorker {
    /// Spawn the worker if sync is configured; `None` otherwise (offline only).
    pub fn spawn(db_path: PathBuf, photos_dir: PathBuf, config_file: PathBuf) -> Option<Self> {
        let cfg = SyncConfig::load(&config_file)?;
        // Open on this thread so a failure surfaces here (like PersistenceWriter),
        // then move the connection into the worker.
        let conn = match super::open_db(&db_path) {
            Ok(c) => c,
            Err(e) => {
                log::error!("sync: cannot open db, sync disabled: {e}");
                return None;
            }
        };
        let client = SyncClient::new(cfg);
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let status_thread = Arc::clone(&status);
        let (tx, rx) = mpsc::channel::<SyncMsg>();

        let handle = std::thread::Builder::new()
            .name("ber-sync".into())
            .spawn(move || {
                // Initial pull on startup, then wait for triggers.
                run_pass(&client, &conn, &photos_dir, &status_thread);
                while rx.recv().is_ok() {
                    run_pass(&client, &conn, &photos_dir, &status_thread);
                }
            })
            .expect("spawn sync thread");

        Some(Self {
            tx,
            _handle: handle,
            status,
        })
    }

    /// Ask the worker to pull again (e.g. a "Sync now" button). No-op after
    /// shutdown.
    pub fn trigger_import(&self) {
        let _ = self.tx.send(SyncMsg::ImportNow);
    }

    /// Snapshot of the latest pass's outcome.
    pub fn status(&self) -> SyncStatus {
        self.status.lock().unwrap().clone()
    }
}

fn run_pass(
    client: &SyncClient,
    conn: &super::Db,
    photos_dir: &Path,
    status: &Arc<Mutex<SyncStatus>>,
) {
    status.lock().unwrap().in_progress = true;
    let result = import_confirmed(client, conn, photos_dir);
    let mut s = status.lock().unwrap();
    s.in_progress = false;
    match result {
        Ok(n) => {
            s.last_import_ok = true;
            s.imported = n;
            s.last_error = None;
            if n > 0 {
                log::info!("sync: imported {n} new BER(s)");
            }
        }
        Err(e) => {
            s.last_import_ok = false;
            s.last_error = Some(e.to_string());
            log::warn!("sync: pull failed: {e}");
        }
    }
}
