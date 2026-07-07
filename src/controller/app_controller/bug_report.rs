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
        // Read-only geometry self-check — runs ONLY at capture, so no UX cost.
        // Flags outline edges that are nearly (but not exactly) axis-aligned and
        // corners that aren't square, so a "corner isn't 90°" report ships the
        // diagnosis already computed (see `outline_geometry_warnings`).
        if let Some(warnings) = self.outline_geometry_warnings() {
            context.insert("geometryWarnings".into(), warnings);
        }

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

    /// A read-only self-check of every storey's OUTLINE geometry, attached to a
    /// bug report so a "corner isn't 90°" case ships with the diagnosis already
    /// done. Returns a compact JSON string of anomalies, or `None` when the
    /// outlines are clean. Purely diagnostic — never mutates, and runs only when
    /// a report is captured, so it has zero effect on normal use.
    ///
    /// Flags two things per outline:
    /// - **near-axis edges**: an edge within 15° of horizontal/vertical but off
    ///   by more than 0.3° — i.e. it's meant to be square but its two endpoints
    ///   don't share the axis coordinate (the exact n5 symptom: a long, nearly-
    ///   horizontal wall with a few-cm rise);
    /// - **corners** whose interior angle isn't ~90° (and isn't a straight run).
    pub(super) fn outline_geometry_warnings(&self) -> Option<String> {
        use domain::points::Point2f32;
        let axis_dev = |dx: f64, dy: f64| -> f64 {
            // Distance (deg) of the edge direction to the nearest axis (0..45).
            let m = dy.atan2(dx).to_degrees().rem_euclid(90.0);
            m.min(90.0 - m)
        };
        let mut edges: Vec<serde_json::Value> = Vec::new();
        let mut corners: Vec<serde_json::Value> = Vec::new();
        for (si, sd) in self.document.storeys.iter().enumerate() {
            let pts: Vec<Point2f32> = sd.outline.floor_plan.get_points();
            let n = pts.len();
            if n < 3 {
                continue;
            }
            for i in 0..n {
                let a = pts[i];
                let b = pts[(i + 1) % n];
                let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
                let len = (dx * dx + dy * dy).sqrt();
                if len < 0.2 {
                    continue; // ignore slivers / duplicate points
                }
                let dev = axis_dev(dx, dy);
                if dev > 0.3 && dev < 15.0 {
                    let horizontal = dx.abs() > dy.abs();
                    edges.push(serde_json::json!({
                        "storey": si,
                        "edge": i,
                        "from": [a.x, a.y],
                        "to": [b.x, b.y],
                        "lengthM": (len * 1000.0).round() / 1000.0,
                        "deviationDeg": (dev * 100.0).round() / 100.0,
                        "coordGapM": (((if horizontal { dy } else { dx }).abs()) * 1000.0).round() / 1000.0,
                        "axis": if horizontal { "horizontal" } else { "vertical" },
                    }));
                }
            }
            for i in 0..n {
                let a = pts[(i + n - 1) % n];
                let b = pts[i];
                let c = pts[(i + 1) % n];
                let (v1x, v1y) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
                let (v2x, v2y) = ((c.x - b.x) as f64, (c.y - b.y) as f64);
                let dot = v1x * v2x + v1y * v2y;
                let cross = v1x * v2y - v1y * v2x;
                let interior = cross.abs().atan2(dot).to_degrees();
                // Off 90° but not a (near-)straight run through the vertex.
                if (interior - 90.0).abs() > 1.0 && interior > 5.0 && interior < 175.0 {
                    corners.push(serde_json::json!({
                        "storey": si,
                        "vertex": i,
                        "at": [b.x, b.y],
                        "interiorDeg": (interior * 100.0).round() / 100.0,
                    }));
                }
            }
        }
        if edges.is_empty() && corners.is_empty() {
            return None;
        }
        Some(
            serde_json::json!({
                "nonOrthogonalEdges": edges,
                "cornersOffFrom90": corners,
            })
            .to_string(),
        )
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
