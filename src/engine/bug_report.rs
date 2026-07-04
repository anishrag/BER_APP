//! Local-first bug-report capture + upload to the intake backend.
//!
//! The assessor taps a Bug button, records a spoken description (audio), and the
//! app writes a full BER state dump. Both land in a per-report subdir under
//! [`crate::paths::Paths::bug_reports_dir`] IMMEDIATELY — offline-first, so a
//! report is never lost when the tablet has no signal. A background worker
//! (modeled on [`super::sync::SyncWorker`]) drains the queue to the surveyor API
//! whenever connectivity allows, PUTting the blobs to the presigned S3 URLs the
//! backend mints, then marking the intake row uploaded.
//!
//! Reuses [`SyncConfig`](super::sync::SyncConfig) — the same `baseUrl` + `apiKey`
//! (`sync.json` / `$BER_SYNC_*`) as the pull sync — so the cloud is configured in
//! ONE place. When unconfigured, capture still works locally; only upload waits.
//!
//! Boundary: per CLAUDE.md rule 4 all networking lives here in `src/engine/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use ber_model::ber::common::ItemId;

use super::sync::SyncConfig;

/// Marker file written into a report's dir once the backend has confirmed the
/// upload. Its presence excludes the report from future drain passes.
const UPLOADED_MARKER: &str = ".uploaded";

/// How often the idle worker re-checks the queue when there's nothing to react
/// to — so a report queued while offline uploads within this window once WiFi
/// returns, even without an explicit trigger.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

// ---- Report metadata (persisted as meta.json, sent on upload) -------------

/// Captured at the moment the Bug button is used and stored next to the blobs.
/// The upload sends these fields to `POST /surveyor/bug-reports`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BugReportMeta {
    /// The assessor's typed note (may be empty when the description is audio-only).
    pub note: String,
    /// The open BER's id, when one is open (else the report is app-level).
    #[serde(default)]
    pub ber_id: Option<String>,
    /// The open BER's address, for at-a-glance triage at home.
    #[serde(default)]
    pub address: Option<String>,
    /// Free-form app context (page, app version, device model, …).
    #[serde(default)]
    pub context: BTreeMap<String, String>,
    /// Whether an `audio.m4a` was captured for this report.
    #[serde(default)]
    pub has_audio: bool,
}

/// Write a captured report into the local queue: a fresh `{uuid}/` subdir holding
/// `state.json` (the BER dump), `meta.json`, and — if `audio_path` points at an
/// existing file — a copy as `audio.m4a`. Returns the report's dir.
///
/// Local-first and network-free: always succeeds offline; the background
/// [`BugReportUploader`] picks the report up later. Callable even when upload is
/// unconfigured.
pub fn queue_report(
    bug_reports_dir: &Path,
    meta: &BugReportMeta,
    audio_path: Option<&Path>,
    state_json: &str,
) -> Result<PathBuf, String> {
    let id = ItemId::default().0.to_string();
    let dir = bug_reports_dir.join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    std::fs::write(dir.join("state.json"), state_json)
        .map_err(|e| format!("write state.json: {e}"))?;
    let meta_json =
        serde_json::to_string_pretty(meta).map_err(|e| format!("serialize meta: {e}"))?;
    std::fs::write(dir.join("meta.json"), meta_json)
        .map_err(|e| format!("write meta.json: {e}"))?;

    if let Some(src) = audio_path {
        if src.exists() {
            std::fs::copy(src, dir.join("audio.m4a")).map_err(|e| format!("copy audio: {e}"))?;
        }
    }
    log::info!("bug-report: queued {id}");
    Ok(dir)
}

// ---- Wire types -----------------------------------------------------------

#[derive(Serialize)]
struct CreateRequest<'a> {
    note: &'a str,
    #[serde(rename = "berId", skip_serializing_if = "Option::is_none")]
    ber_id: Option<&'a str>,
    #[serde(rename = "address", skip_serializing_if = "Option::is_none")]
    address: Option<&'a str>,
    #[serde(rename = "appContext")]
    app_context: &'a BTreeMap<String, String>,
    #[serde(rename = "hasAudio")]
    has_audio: bool,
}

#[derive(Deserialize)]
struct CreateResponse {
    #[serde(rename = "bugReportId")]
    bug_report_id: String,
    #[serde(rename = "statePutUrl")]
    state_put_url: String,
    #[serde(rename = "audioPutUrl", default)]
    audio_put_url: Option<String>,
}

// ---- HTTP client ----------------------------------------------------------

struct UploadClient {
    cfg: SyncConfig,
    agent: ureq::Agent,
}

impl UploadClient {
    fn new(cfg: SyncConfig) -> Self {
        // Longer read timeout than the pull sync: an audio blob PUT can take a
        // while on a weak field connection.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build();
        Self { cfg, agent }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    fn create(&self, req: &CreateRequest) -> anyhow::Result<CreateResponse> {
        let resp = self
            .agent
            .post(&self.url("/surveyor/bug-reports"))
            .set("x-surveyor-key", &self.cfg.api_key)
            .send_json(req)?;
        Ok(resp.into_json()?)
    }

    /// PUT bytes to a presigned S3 URL. The URL already carries its auth in the
    /// query string, so NO key header — but the `Content-Type` MUST match what
    /// the backend signed the URL with, or S3 rejects the signature.
    fn put_blob(&self, url: &str, content_type: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.agent
            .put(url)
            .set("Content-Type", content_type)
            .send_bytes(bytes)?;
        Ok(())
    }

    fn complete(&self, bug_report_id: &str) -> anyhow::Result<()> {
        self.agent
            .post(&self.url(&format!("/surveyor/bug-reports/{bug_report_id}/complete")))
            .set("x-surveyor-key", &self.cfg.api_key)
            .call()?;
        Ok(())
    }
}

// ---- Queue scan + upload --------------------------------------------------

/// Report dirs still awaiting upload: subdirs that hold a `meta.json` and lack
/// the [`UPLOADED_MARKER`]. Sorted for deterministic order.
fn list_pending(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out; // dir absent = nothing queued yet
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("meta.json").exists() && !path.join(UPLOADED_MARKER).exists()
        {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn upload_one(client: &UploadClient, report_dir: &Path) -> anyhow::Result<()> {
    let meta_json = std::fs::read_to_string(report_dir.join("meta.json"))?;
    let meta: BugReportMeta = serde_json::from_str(&meta_json)?;
    let state = std::fs::read(report_dir.join("state.json"))?;
    let audio_file = report_dir.join("audio.m4a");
    let has_audio = meta.has_audio && audio_file.exists();

    let req = CreateRequest {
        note: &meta.note,
        ber_id: meta.ber_id.as_deref(),
        address: meta.address.as_deref(),
        app_context: &meta.context,
        has_audio,
    };
    let resp = client.create(&req)?;

    client.put_blob(&resp.state_put_url, "application/json", &state)?;
    if has_audio {
        let audio = std::fs::read(&audio_file)?;
        let url = resp
            .audio_put_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("backend gave no audio URL for a report with audio"))?;
        client.put_blob(url, "audio/mp4", &audio)?;
    }
    client.complete(&resp.bug_report_id)?;

    std::fs::write(report_dir.join(UPLOADED_MARKER), &resp.bug_report_id)?;
    log::info!(
        "bug-report: uploaded {} as {}",
        report_dir.display(),
        resp.bug_report_id
    );
    Ok(())
}

// ---- Background worker (modeled on sync::SyncWorker) -----------------------

/// Observable outcome of the most recent drain pass (read by the UI).
#[derive(Debug, Clone, Default)]
pub struct BugUploadStatus {
    pub in_progress: bool,
    /// Reports still queued locally (not yet confirmed uploaded).
    pub pending: usize,
    /// Cumulative reports uploaded this session.
    pub uploaded: usize,
    pub last_error: Option<String>,
}

enum Msg {
    Drain,
}

/// Owns a dedicated thread with its own HTTP client. Drains once on spawn, on
/// each [`trigger`](Self::trigger), and on a [`RETRY_INTERVAL`] tick when reports
/// are still pending. Dropping it closes the channel; the thread exits after any
/// in-flight pass.
pub struct BugReportUploader {
    tx: Sender<Msg>,
    _handle: JoinHandle<()>,
    status: Arc<Mutex<BugUploadStatus>>,
}

impl BugReportUploader {
    /// Spawn the uploader if sync is configured; `None` otherwise (reports still
    /// queue locally and upload once configured on a later run).
    pub fn spawn(bug_reports_dir: PathBuf, config_file: PathBuf) -> Option<Self> {
        let cfg = SyncConfig::load(&config_file)?;
        let client = UploadClient::new(cfg);
        let status = Arc::new(Mutex::new(BugUploadStatus::default()));
        let status_thread = Arc::clone(&status);
        let (tx, rx) = mpsc::channel::<Msg>();

        let handle = std::thread::Builder::new()
            .name("ber-bug-upload".into())
            .spawn(move || {
                drain(&client, &bug_reports_dir, &status_thread);
                loop {
                    match rx.recv_timeout(RETRY_INTERVAL) {
                        Ok(Msg::Drain) => drain(&client, &bug_reports_dir, &status_thread),
                        Err(RecvTimeoutError::Timeout) => {
                            // Idle wake: only bother if something's still waiting.
                            if !list_pending(&bug_reports_dir).is_empty() {
                                drain(&client, &bug_reports_dir, &status_thread);
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("spawn bug-upload thread");

        Some(Self {
            tx,
            _handle: handle,
            status,
        })
    }

    /// Ask the worker to drain now (e.g. just-queued a report, or WiFi came up).
    /// No-op after shutdown.
    pub fn trigger(&self) {
        let _ = self.tx.send(Msg::Drain);
    }

    /// Snapshot of the latest pass's outcome.
    pub fn status(&self) -> BugUploadStatus {
        self.status.lock().unwrap().clone()
    }
}

fn drain(client: &UploadClient, dir: &Path, status: &Arc<Mutex<BugUploadStatus>>) {
    status.lock().unwrap().in_progress = true;
    let pending = list_pending(dir);
    let mut uploaded_now = 0;
    let mut last_error = None;
    for report in &pending {
        match upload_one(client, report) {
            Ok(()) => uploaded_now += 1,
            Err(e) => {
                log::warn!("bug-report: upload of {} failed: {e}", report.display());
                last_error = Some(e.to_string());
            }
        }
    }
    let mut s = status.lock().unwrap();
    s.in_progress = false;
    s.uploaded += uploaded_now;
    s.pending = list_pending(dir).len();
    if last_error.is_some() {
        s.last_error = last_error;
    } else if uploaded_now > 0 {
        s.last_error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ber_bugreport_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// A queued report lands as a scannable pending dir with state + meta, and an
    /// audio copy when supplied.
    #[test]
    fn queue_then_list_pending() {
        let root = tmp_dir("queue");
        // Fake an audio source file.
        std::fs::create_dir_all(&root).unwrap();
        let audio_src = root.join("rec.m4a");
        std::fs::write(&audio_src, b"fake-audio").unwrap();

        let meta = BugReportMeta {
            note: "wall snap glitched".into(),
            ber_id: Some("abc".into()),
            has_audio: true,
            ..Default::default()
        };
        let queue = root.join("bug-reports");
        let dir = queue_report(&queue, &meta, Some(&audio_src), "{\"ber\":true}").unwrap();

        assert!(dir.join("state.json").exists());
        assert!(dir.join("meta.json").exists());
        assert!(dir.join("audio.m4a").exists(), "audio was copied in");

        let pending = list_pending(&queue);
        assert_eq!(pending.len(), 1, "the report is pending");

        // The uploaded marker excludes it from later passes.
        std::fs::write(dir.join(UPLOADED_MARKER), "id").unwrap();
        assert!(list_pending(&queue).is_empty(), "marker excludes it");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// meta.json round-trips (the upload reads it back).
    #[test]
    fn meta_round_trips() {
        let meta = BugReportMeta {
            note: "n".into(),
            address: Some("1 Main St".into()),
            context: BTreeMap::from([("page".into(), "sketch".into())]),
            has_audio: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: BugReportMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.address.as_deref(), Some("1 Main St"));
        assert_eq!(back.context.get("page").map(String::as_str), Some("sketch"));
    }
}
