//! Bug-report capture (thin shell): sequence audio recording + a full BER state
//! dump into the local queue and kick the uploader. The engine
//! (`crate::engine::bug_report`) owns file I/O + networking; `crate::platform::
//! audio` owns recording; this just orchestrates them and gathers context.
use super::*;

use std::collections::BTreeMap;

use crate::engine::bug_report::{self, BugReportMeta};
use crate::engine::BugUploadStatus;

impl AppController {
    /// Where captured bug reports are staged/queued (set from `Paths`).
    pub fn set_bug_reports_dir(&mut self, dir: std::path::PathBuf) {
        self.bug_reports_dir = dir;
    }

    /// Attach the background uploader (real app only; needs sync config).
    pub fn attach_bug_uploader(&mut self, uploader: crate::engine::BugReportUploader) {
        self.bug_uploader = Some(uploader);
    }

    /// Whether spoken (audio) bug descriptions are available on this platform.
    pub fn bug_audio_supported(&self) -> bool {
        crate::platform::audio::is_supported()
    }

    /// Whether an audio recording is currently in progress.
    pub fn bug_recording_active(&self) -> bool {
        self.bug_recording_path.is_some()
    }

    /// Begin recording a spoken bug description into a staging file. Errors on
    /// platforms without audio. Idempotent while already recording.
    pub fn start_bug_recording(&mut self) -> Result<(), String> {
        if self.bug_recording_path.is_some() {
            return Ok(());
        }
        if !crate::platform::audio::is_supported() {
            return Err("Audio recording is only available on the tablet.".into());
        }
        let staging = self.bug_reports_dir.join(".recording");
        std::fs::create_dir_all(&staging).map_err(|e| format!("staging dir: {e}"))?;
        let path = staging.join(format!(
            "{}.m4a",
            ber_model::ber::common::ItemId::default().0
        ));
        crate::platform::audio::start(&path)?;
        self.bug_recording_path = Some(path);
        Ok(())
    }

    /// Stop recording but KEEP the finalized file so `finish_bug_report` can
    /// attach it (the "Stop" button — distinct from Cancel, which discards).
    /// Idempotent: safe if already stopped (the Android recorder no-ops).
    pub fn stop_bug_recording(&mut self) {
        if self.bug_recording_path.is_some() {
            let _ = crate::platform::audio::stop();
        }
    }

    /// Stop and discard an in-flight recording (Cancel).
    pub fn cancel_bug_recording(&mut self) {
        if let Some(path) = self.bug_recording_path.take() {
            let _ = crate::platform::audio::stop();
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Finish a bug report: stop any recording, capture the current BER state
    /// dump, write both to the local queue (offline-first), and trigger upload.
    /// `context` is UI-supplied (e.g. the active page); app version + platform
    /// are added here. Returns a short user-facing confirmation.
    pub fn finish_bug_report(
        &mut self,
        note: String,
        mut context: BTreeMap<String, String>,
    ) -> Result<String, String> {
        // 1. Finalize audio (synchronous on Android — file ready on return).
        let audio_path = match self.bug_recording_path.take() {
            Some(path) => {
                crate::platform::audio::stop()?;
                let has_bytes = std::fs::metadata(&path)
                    .map(|m| m.len() > 0)
                    .unwrap_or(false);
                has_bytes.then_some(path)
            }
            None => None,
        };

        // 2. State dump — byte-identical to a dev checkpoint (Ber + Document snapshot).
        let (state_json, ber_id, address) = match self.current_record.as_ref() {
            Some(rec) => {
                let snap = self.build_snapshot();
                let snap_ref = snap.as_ref().map(|s| (s.version, s.json.as_str()));
                let json = crate::engine::checkpoint::to_json(&rec.data, snap_ref)?;
                let addr = rec.data.address.trim();
                (
                    json,
                    Some(rec.id.0.to_string()),
                    (!addr.is_empty()).then(|| addr.to_string()),
                )
            }
            None => (
                serde_json::json!({ "berOpen": false }).to_string(),
                None,
                None,
            ),
        };

        // 3. Context: app version + platform, plus whatever the UI supplied.
        context.insert("appVersion".into(), env!("CARGO_PKG_VERSION").into());
        context.insert("platform".into(), std::env::consts::OS.into());

        let meta = BugReportMeta {
            note,
            ber_id,
            address,
            context,
            has_audio: audio_path.is_some(),
        };

        // 4. Queue locally, then poke the uploader.
        bug_report::queue_report(
            &self.bug_reports_dir,
            &meta,
            audio_path.as_deref(),
            &state_json,
        )?;
        if let Some(u) = &self.bug_uploader {
            u.trigger();
        }

        Ok(if self.bug_uploader.is_some() {
            "Bug report saved — it will upload when online.".into()
        } else {
            "Bug report saved to this device.".into()
        })
    }

    /// Trigger an immediate upload attempt (e.g. WiFi came up). No-op if off.
    pub fn bug_upload_now(&self) {
        if let Some(u) = &self.bug_uploader {
            u.trigger();
        }
    }

    /// Latest uploader status for the UI, or `None` when upload is unconfigured.
    pub fn bug_upload_status(&self) -> Option<BugUploadStatus> {
        self.bug_uploader.as_ref().map(|u| u.status())
    }
}

#[cfg(test)]
mod tests {
    use super::AppController;
    use std::collections::BTreeMap;

    fn tmp_queue(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ber_ctrl_bug_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// Finishing a report with no BER open still writes a queued report (state
    /// dump degrades to a `berOpen:false` marker; no audio on desktop).
    #[test]
    fn finish_without_ber_queues_a_report() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let queue = tmp_queue("no_ber");
        c.set_bug_reports_dir(queue.clone());

        let msg = c
            .finish_bug_report("mic test".into(), BTreeMap::new())
            .expect("queues");
        assert!(msg.contains("saved"));

        // Exactly one report dir with state + meta, no audio.
        let dirs: Vec<_> = std::fs::read_dir(&queue).unwrap().flatten().collect();
        assert_eq!(dirs.len(), 1, "one queued report");
        let report = dirs[0].path();
        let state = std::fs::read_to_string(report.join("state.json")).unwrap();
        assert!(state.contains("berOpen"), "no-BER state marker");
        assert!(!report.join("audio.m4a").exists(), "no audio on desktop");

        let meta = std::fs::read_to_string(report.join("meta.json")).unwrap();
        assert!(meta.contains("mic test"));
        assert!(meta.contains("appVersion"));
        let _ = std::fs::remove_dir_all(&queue);
    }

    /// With a BER open, the state dump is a real checkpoint (carries the `Ber`)
    /// and the meta records the BER id.
    #[test]
    fn finish_with_ber_dumps_checkpoint() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let queue = tmp_queue("with_ber");
        c.set_bug_reports_dir(queue.clone());
        let ber = c.create_ber(
            "Test".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 7, 4).unwrap(),
        );

        c.finish_bug_report("geometry glitch".into(), BTreeMap::new())
            .expect("queues");

        let report = std::fs::read_dir(&queue)
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path();
        let state = std::fs::read_to_string(report.join("state.json")).unwrap();
        assert!(state.contains("\"ber\""), "checkpoint carries the Ber");
        let meta = std::fs::read_to_string(report.join("meta.json")).unwrap();
        assert!(
            meta.contains(&ber.id.0.to_string()),
            "meta records the BER id"
        );
        let _ = std::fs::remove_dir_all(&queue);
    }
}
