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

use ber_model::ber::ber_struct::{Ber, IntakeDetails};
use ber_model::ber::common::{ConstructionPhase, ItemId, Photo};

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

/// The client-provided seed fields we surface on the tablet. Address/eircode/
/// storeys seed the core `Ber`; construction year + extensions seed the
/// construction phases; the rest land in `Ber::intake` (Key Details staging).
#[derive(Deserialize, Default)]
struct BerSeed {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    eircode: Option<String>,
    #[serde(rename = "numberOfStoreys", default)]
    number_of_storeys: Option<i32>,
    #[serde(rename = "constructionYear", default)]
    construction_year: Option<u32>,
    #[serde(default)]
    extensions: Vec<SeedExtension>,
    #[serde(rename = "propertyType", default)]
    property_type: Option<String>,
    #[serde(rename = "heatingSystem", default)]
    heating_system: Option<String>,
    #[serde(rename = "windowYear", default)]
    window_year: Option<u32>,
    #[serde(rename = "doorYear", default)]
    door_year: Option<u32>,
    #[serde(default)]
    insulation: Option<SeedInsulation>,
    #[serde(default)]
    mprn: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct SeedExtension {
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize, Default)]
struct SeedInsulation {
    #[serde(default)]
    walls: Option<bool>,
    #[serde(default)]
    roof: Option<bool>,
    #[serde(default)]
    floor: Option<bool>,
    #[serde(default)]
    notes: Option<String>,
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
        // No status filter: the server returns everything ready to assess
        // (booked or confirmed) — a job reaches the tablet as soon as the client
        // books, without waiting on the letter of engagement being signed.
        let resp = self
            .agent
            .get(&self.url("/surveyor/jobs"))
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

    // Construction year + extensions → construction phases (the Key Details
    // "Year of Construction" + extension rows). Original = year built; each
    // extension takes the next Extension1..4 variant, with its description as
    // the row nickname. A description with no year designates the phase with the
    // year "unknown", matching how the form itself commits such a row.
    if let Some(y) = seed.construction_year {
        ber.construction_phases
            .set_year(ConstructionPhase::Original, y);
    }
    let ext_phases = [
        ConstructionPhase::Extension1,
        ConstructionPhase::Extension2,
        ConstructionPhase::Extension3,
        ConstructionPhase::Extension4,
    ];
    for (ext, phase) in seed.extensions.iter().zip(ext_phases) {
        let has_desc = ext.description.as_deref().is_some_and(|d| !d.is_empty());
        match ext.year {
            Some(y) => ber.construction_phases.set_year(phase.clone(), y),
            None if has_desc => ber.construction_phases.set_year_unknown(phase.clone()),
            None => continue,
        }
        if let Some(desc) = &ext.description {
            if !desc.is_empty() {
                ber.construction_phases.set_nickname(phase, desc.clone());
            }
        }
    }

    // The remaining client answers → the Key Details "intake" staging fields
    // (not yet wired into the DEAP model; the assessor confirms/fills them).
    let ins = seed.insulation.unwrap_or_default();
    ber.intake = IntakeDetails {
        property_type: seed.property_type.unwrap_or_default(),
        heating_system: seed.heating_system.unwrap_or_default(),
        window_year: seed.window_year,
        door_year: seed.door_year,
        insulation_walls: ins.walls.unwrap_or(false),
        insulation_roof: ins.roof.unwrap_or(false),
        insulation_floor: ins.floor.unwrap_or(false),
        insulation_notes: ins.notes.unwrap_or_default(),
        mprn: seed.mprn.unwrap_or_default(),
        reason: seed.reason.unwrap_or_default(),
    };

    // Name the BER by its address, minus the county and any eircode (the eircode
    // is sometimes folded into the address field). Fall back to the eircode if
    // that leaves nothing.
    let name = match ber_name_from_address(&ber.address) {
        n if !n.is_empty() => n,
        _ => ber.eircode.clone(),
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

/// Build the BER's display name from the seed address, dropping any county
/// component and embedded eircode. Seeds arrive comma-separated, e.g.
/// "12 Main St, Ballincollig, Co. Cork, T12 X70P" (and the eircode is sometimes
/// appended into the address field itself), which we want shown as
/// "12 Main St, Ballincollig".
fn ber_name_from_address(address: &str) -> String {
    address
        .split(',')
        .map(|part| strip_eircode(part.trim()))
        .filter(|part| !part.is_empty() && !is_county(part))
        .collect::<Vec<_>>()
        .join(", ")
}

/// True for an address component that is the county, e.g. "Co. Cork",
/// "Co Cork", "County Cork". Deliberately narrow so town names that merely start
/// with "Co" (Cobh, Coachford) are kept.
fn is_county(part: &str) -> bool {
    let lower = part.to_ascii_lowercase();
    lower.starts_with("co. ")
        || lower.starts_with("co ")
        || lower.starts_with("co.")
        || lower.starts_with("county ")
}

/// Remove an eircode from one address component, whether spaced ("T12 X70P") or
/// solid ("T12X70P"). An eircode is a 3-char routing key (letter, digit,
/// alphanumeric) plus a 4-char alphanumeric identifier. Case-insensitive.
fn strip_eircode(part: &str) -> String {
    let words: Vec<&str> = part.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        if is_eircode_solid(words[i]) {
            i += 1;
        } else if is_routing_key(words[i])
            && i + 1 < words.len()
            && is_eircode_identifier(words[i + 1])
        {
            i += 2;
        } else {
            out.push(words[i]);
            i += 1;
        }
    }
    out.join(" ")
}

fn is_routing_key(w: &str) -> bool {
    let b = w.as_bytes();
    w.len() == 3
        && b[0].is_ascii_alphabetic()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_alphanumeric()
}

fn is_eircode_identifier(w: &str) -> bool {
    w.len() == 4 && w.bytes().all(|c| c.is_ascii_alphanumeric())
}

fn is_eircode_solid(w: &str) -> bool {
    let b = w.as_bytes();
    w.len() == 7
        && b[0].is_ascii_alphabetic()
        && b[1].is_ascii_digit()
        && w.bytes().all(|c| c.is_ascii_alphanumeric())
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

#[cfg(test)]
mod name_tests {
    use super::ber_name_from_address;

    #[test]
    fn drops_county_and_spaced_eircode() {
        assert_eq!(
            ber_name_from_address("12 Main St, Ballincollig, Co. Cork, T12 X70P"),
            "12 Main St, Ballincollig"
        );
    }

    #[test]
    fn drops_eircode_folded_into_a_part() {
        assert_eq!(
            ber_name_from_address("12 Main St, Ballincollig T12 X70P"),
            "12 Main St, Ballincollig"
        );
    }

    #[test]
    fn drops_solid_eircode_and_county_word() {
        assert_eq!(
            ber_name_from_address("5 Oak Rd, Douglas, County Cork, T12X70P"),
            "5 Oak Rd, Douglas"
        );
    }

    #[test]
    fn keeps_towns_that_start_with_co() {
        assert_eq!(ber_name_from_address("Cobh, Co. Cork"), "Cobh");
        assert_eq!(ber_name_from_address("Coachford, Co Cork"), "Coachford");
    }

    #[test]
    fn plain_address_untouched() {
        assert_eq!(
            ber_name_from_address("42 Elm Grove, Bishopstown"),
            "42 Elm Grove, Bishopstown"
        );
    }

    #[test]
    fn empty_when_only_county_and_eircode() {
        assert_eq!(ber_name_from_address("Co. Cork, T12 X70P"), "");
    }
}

#[cfg(test)]
mod seed_tests {
    use super::BerSeed;

    // The wire contract: a seed JSON (camelCase keys) must deserialize onto the
    // fields we map to the tablet. Guards against serde-rename typos.
    #[test]
    fn deserializes_full_seed() {
        let json = r#"{
            "address": "12 Main St, Cork",
            "eircode": "T12 X70P",
            "numberOfStoreys": 2,
            "constructionYear": 1985,
            "extensions": [{"year": 2008, "description": "Back extension"}],
            "propertyType": "semi-detached",
            "heatingSystem": "gas boiler",
            "windowYear": 2010,
            "doorYear": 2011,
            "insulation": {"walls": true, "roof": false, "notes": "pumped"},
            "mprn": "10001234567",
            "reason": "sale"
        }"#;
        let s: BerSeed = serde_json::from_str(json).unwrap();
        assert_eq!(s.construction_year, Some(1985));
        assert_eq!(s.extensions.len(), 1);
        assert_eq!(s.extensions[0].year, Some(2008));
        assert_eq!(
            s.extensions[0].description.as_deref(),
            Some("Back extension")
        );
        assert_eq!(s.property_type.as_deref(), Some("semi-detached"));
        assert_eq!(s.heating_system.as_deref(), Some("gas boiler"));
        assert_eq!(s.window_year, Some(2010));
        assert_eq!(s.door_year, Some(2011));
        let ins = s.insulation.unwrap();
        assert_eq!(ins.walls, Some(true));
        assert_eq!(ins.roof, Some(false));
        assert_eq!(ins.floor, None);
        assert_eq!(s.mprn.as_deref(), Some("10001234567"));
        assert_eq!(s.reason.as_deref(), Some("sale"));
    }

    // A sparse seed (older/telegram job) must still deserialize, everything else
    // defaulting — the fields are all optional.
    #[test]
    fn deserializes_sparse_seed() {
        let s: BerSeed = serde_json::from_str(r#"{"eircode":"D06 K2X3"}"#).unwrap();
        assert_eq!(s.eircode.as_deref(), Some("D06 K2X3"));
        assert_eq!(s.construction_year, None);
        assert!(s.extensions.is_empty());
        assert!(s.insulation.is_none());
    }
}
