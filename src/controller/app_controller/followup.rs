//! Controller side of the Follow-ups tab: derive the view data, edit the
//! selection/draft, and run the one-shot "send email" (spawn the network call,
//! poll it, stamp the one-shot lock on success). The catalogue, auto-selection
//! and draft assembly are pure and live in `ber_model::ber::followup`; the
//! actual POST is engine I/O. This is the thin shell that sequences them.

use std::sync::{Arc, Mutex};

use chrono::Datelike;

use ber_model::ber::common::ItemId;
use ber_model::ber::followup::{self, CustomPoint, FollowUpPointId};

use super::AppController;

/// In-flight state of a send, shared with the worker thread.
pub(super) enum FollowUpSendState {
    Sending,
    Done,
    NoJob,
    Failed(String),
}

/// A send in flight, bound to the BER it was fired for so its completion is
/// applied to that record even if the assessor has since switched away.
pub(super) struct PendingSend {
    pub(super) job_id: ItemId,
    pub(super) state: Arc<Mutex<FollowUpSendState>>,
}

/// Identifies a tab row: a built-in catalogue point or a per-BER custom one.
#[derive(Debug, Clone, PartialEq)]
pub enum FollowUpRowKey {
    BuiltIn(FollowUpPointId),
    Custom(ItemId),
}

/// One request-point row for the tab: its key, label, tick state, and whether
/// it can be removed (custom points only).
pub struct FollowUpPointRow {
    pub key: FollowUpRowKey,
    pub label: String,
    pub checked: bool,
    pub removable: bool,
}

/// Everything the Follow-ups tab renders from.
pub struct FollowUpView {
    pub points: Vec<FollowUpPointRow>,
    /// The editable email draft.
    pub body: String,
    /// The request email has been sent — the button is spent and the form locked.
    pub sent: bool,
    /// A send is currently in flight.
    pub sending: bool,
    /// The last send failed (no cloud job / network) — message for the tab.
    pub error: Option<String>,
}

impl AppController {
    /// Point the follow-up send at the cloud-config file (same `sync.json` the
    /// workers use). Called from `bootstrap`.
    pub fn set_sync_config_file(&mut self, path: std::path::PathBuf) {
        self.sync_config_file = path;
    }

    /// Build the Follow-ups tab view. On the first visit for a BER (nothing sent
    /// yet, never initialized) it seeds the selection from the survey and
    /// assembles the draft, persisting so a reopen is stable. Returns `None`
    /// when no BER is loaded.
    pub fn followup_view(&mut self) -> Option<FollowUpView> {
        // Fold in any completed send first, so `sent`/`error` are current.
        self.poll_followup_send();

        let record = self.current_record.as_mut()?;
        let reference_year = record.date_assessed.year().max(0) as u32;
        let ber = &mut record.data;

        let mut seeded = false;
        if !ber.followup.is_sent() && !ber.followup.initialized {
            ber.followup.selected = followup::auto_selected(ber, reference_year);
            rebuild_followup_body(ber);
            ber.followup.initialized = true;
            seeded = true;
        }
        if seeded {
            let _ = self.persist();
        }

        let record = self.current_record.as_ref()?;
        let current_id = record.data.id.clone();
        let fu = &record.data.followup;
        let mut points: Vec<FollowUpPointRow> = followup::ALL_POINTS
            .into_iter()
            .map(|id| FollowUpPointRow {
                key: FollowUpRowKey::BuiltIn(id),
                label: id.label().to_string(),
                checked: fu.selected.contains(&id),
                removable: false,
            })
            .collect();
        points.extend(fu.custom.iter().map(|c| FollowUpPointRow {
            key: FollowUpRowKey::Custom(c.id.clone()),
            label: c.label.clone(),
            checked: c.checked,
            removable: true,
        }));
        // A send is "in flight" for THIS tab only when the pending send is for
        // the record on screen (and still running).
        let sending = self.followup_send.as_ref().is_some_and(|p| {
            p.job_id == current_id && matches!(*p.state.lock().unwrap(), FollowUpSendState::Sending)
        });
        // Only surface an error that belongs to the record on screen.
        let error = self
            .followup_send_error
            .as_ref()
            .filter(|(id, _)| *id == current_id)
            .map(|(_, msg)| msg.clone());
        Some(FollowUpView {
            points,
            body: fu.body.clone(),
            sent: fu.is_sent(),
            sending,
            error,
        })
    }

    /// Toggle a request row (built-in or custom) on/off and rebuild the draft.
    /// No-op once sent. Persists.
    pub fn toggle_followup_point(&mut self, key: FollowUpRowKey) {
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let ber = &mut record.data;
        if ber.followup.is_sent() {
            return;
        }
        match key {
            FollowUpRowKey::BuiltIn(id) => {
                if let Some(pos) = ber.followup.selected.iter().position(|p| *p == id) {
                    ber.followup.selected.remove(pos);
                } else {
                    ber.followup.selected.push(id);
                }
            }
            FollowUpRowKey::Custom(id) => {
                if let Some(c) = ber.followup.custom.iter_mut().find(|c| c.id == id) {
                    c.checked = !c.checked;
                }
            }
        }
        rebuild_followup_body(ber);
        self.followup_send_error = None;
        let _ = self.persist();
    }

    /// Add a per-BER custom request point (the tab's "+"), ticked, and rebuild
    /// the draft. Ignores a blank label+content. No-op once sent. Persists.
    pub fn add_followup_custom(&mut self, label: String, content: String) {
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let ber = &mut record.data;
        if ber.followup.is_sent() {
            return;
        }
        if label.trim().is_empty() && content.trim().is_empty() {
            return;
        }
        ber.followup.custom.push(CustomPoint {
            id: ItemId::default(),
            label: if label.trim().is_empty() {
                "Custom request".to_string()
            } else {
                label
            },
            content,
            checked: true,
        });
        rebuild_followup_body(ber);
        self.followup_send_error = None;
        let _ = self.persist();
    }

    /// Remove a custom request point and rebuild the draft. No-op once sent.
    /// Persists.
    pub fn remove_followup_custom(&mut self, id: ItemId) {
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let ber = &mut record.data;
        if ber.followup.is_sent() {
            return;
        }
        ber.followup.custom.retain(|c| c.id != id);
        rebuild_followup_body(ber);
        self.followup_send_error = None;
        let _ = self.persist();
    }

    /// Store the assessor's edited email body. No-op once sent. Persists.
    pub fn set_followup_body(&mut self, body: String) {
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        if record.data.followup.is_sent() {
            return;
        }
        record.data.followup.body = body;
        let _ = self.persist();
    }

    /// Fire the one-shot send: POST the composed email to the backend on a
    /// worker thread. No-op if already sent or a send is in flight. The result
    /// is applied by [`poll_followup_send`].
    pub fn send_followup(&mut self) {
        if self.followup_send.is_some() {
            return;
        }
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let ber = &record.data;
        if ber.followup.is_sent() {
            return;
        }
        let job_id = ber.id.clone();
        let job_id_str = job_id.to_string();
        let subject = followup::subject(ber);
        let body = ber.followup.body.clone();
        let config_file = self.sync_config_file.clone();

        let state = Arc::new(Mutex::new(FollowUpSendState::Sending));
        self.followup_send = Some(PendingSend {
            job_id: job_id.clone(),
            state: Arc::clone(&state),
        });
        // A retry clears any stale error for this record.
        self.followup_send_error = None;

        std::thread::spawn(move || {
            let result =
                crate::engine::send_details_request(&config_file, &job_id_str, &subject, &body);
            let mut guard = state.lock().unwrap();
            *guard = match result {
                Ok(crate::engine::SendOutcome::Sent) => FollowUpSendState::Done,
                Ok(crate::engine::SendOutcome::NoJob) => FollowUpSendState::NoJob,
                Err(e) => FollowUpSendState::Failed(e.to_string()),
            };
        });
    }

    /// Apply a finished send. On success stamp the one-shot `sent_at` lock on
    /// the BER the send was FIRED for — via the loaded record when it's still
    /// current, otherwise straight to the DB — so navigating away mid-send can't
    /// lock the wrong record or leave the sent one re-sendable. On failure tag a
    /// message to that record. Cheap no-op while sending or idle. Called at the
    /// top of [`followup_view`] each frame.
    pub fn poll_followup_send(&mut self) {
        let Some(pending) = self.followup_send.as_ref() else {
            return;
        };
        let job_id = pending.job_id.clone();
        let outcome = {
            let guard = pending.state.lock().unwrap();
            match &*guard {
                FollowUpSendState::Sending => return, // still running
                FollowUpSendState::Done => Ok(()),
                FollowUpSendState::NoJob => Err(
                    "This BER has no cloud job yet, so the request can't be sent. \
                     It registers once the survey syncs — try again shortly."
                        .to_string(),
                ),
                FollowUpSendState::Failed(e) => Err(format!("Couldn't send the request: {e}")),
            }
        };
        // The send finished — drop the handle before mutating record state.
        self.followup_send = None;
        match outcome {
            Ok(()) => self.stamp_followup_sent(&job_id),
            Err(msg) => self.followup_send_error = Some((job_id, msg)),
        }
    }

    /// Record that the follow-up email for `job_id` has been sent (the one-shot
    /// lock). Writes through the loaded record when it's the current one;
    /// otherwise loads that BER from the DB, stamps it, and saves — so the lock
    /// always lands on the right record.
    fn stamp_followup_sent(&mut self, job_id: &ItemId) {
        let now = chrono::Utc::now().to_rfc3339();
        let id_str = job_id.to_string();
        if self
            .current_record
            .as_ref()
            .is_some_and(|r| r.data.id == *job_id)
        {
            if let Some(record) = self.current_record.as_mut() {
                record.data.followup.sent_at = Some(now);
                if let Err(e) = self.persist() {
                    log::error!("follow-up: sent but persist failed for {id_str}: {e}");
                }
            }
            return;
        }
        // The assessor switched away before the send landed — stamp the record
        // directly in the DB so its one-shot lock still takes.
        let repo = crate::engine::ber_repository::BerRepository::new(&self.conn);
        match repo.load(job_id) {
            Ok(mut record) => {
                record.data.followup.sent_at = Some(now);
                if let Err(e) = repo.upsert(&record) {
                    log::error!("follow-up: sent but DB stamp failed for {id_str}: {e}");
                }
            }
            Err(e) => {
                log::error!("follow-up: sent but couldn't load {id_str} to stamp: {e}");
            }
        }
    }
}

/// Rebuild the editable draft from the current selection + ticked custom points.
fn rebuild_followup_body(ber: &mut ber_model::ber::ber_struct::Ber) {
    let selected = ber.followup.selected.clone();
    let custom = ber.followup.custom.clone();
    ber.followup.body = followup::assemble_draft(ber, &selected, &custom);
}
