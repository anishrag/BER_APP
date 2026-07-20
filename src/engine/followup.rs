//! One-shot "request details" send — the network half of the Follow-ups tab.
//!
//! The tablet composes the client email locally (editable preview); this posts
//! the finished subject + body to the intake backend, which emails the client
//! and moves the job to `details_requested`. Unlike [`super::push`] this isn't a
//! background worker — it's a single user-initiated call the controller runs on
//! a short-lived thread. Boundary: per CLAUDE.md rule 4 the networking lives
//! here in `src/engine/`.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::sync::SyncConfig;

/// Outcome of a send attempt. `NoJob` (the backend 404) is the expected state
/// for a from-scratch BER that was never registered as a cloud job — surfaced
/// to the user rather than treated as an error.
pub enum SendOutcome {
    Sent,
    NoJob,
}

#[derive(Serialize)]
struct RequestBody<'a> {
    subject: &'a str,
    body: &'a str,
}

/// POST the composed follow-up email to
/// `/surveyor/jobs/{job_id}/ber/request-details`. Loads the same cloud config
/// (`sync.json` / `$BER_SYNC_*`) as the sync/push workers.
pub fn send_details_request(
    config_file: &Path,
    job_id: &str,
    subject: &str,
    body: &str,
) -> anyhow::Result<SendOutcome> {
    let cfg = SyncConfig::load(config_file)
        .ok_or_else(|| anyhow::anyhow!("cloud sync not configured"))?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build();
    let url = format!(
        "{}/surveyor/jobs/{}/ber/request-details",
        cfg.base_url.trim_end_matches('/'),
        job_id
    );
    match agent
        .post(&url)
        .set("x-surveyor-key", &cfg.api_key)
        .send_json(&RequestBody { subject, body })
    {
        Ok(_) => Ok(SendOutcome::Sent),
        Err(ureq::Error::Status(404, _)) => Ok(SendOutcome::NoJob),
        Err(e) => Err(e.into()),
    }
}
