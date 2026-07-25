//! Controller side of the Follow-ups tab: derive the view data, edit the
//! selection/draft, and run the one-shot "send email" (spawn the network call,
//! poll it, stamp the one-shot lock on success). The catalogue, auto-selection
//! and draft assembly are pure and live in `ber_model::ber::followup`; the
//! actual POST is engine I/O. This is the thin shell that sequences them.

use std::sync::{Arc, Mutex};

use chrono::Datelike;

use ber_model::ber::common::{ConstructionPhase, ItemId};
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
    /// A per-extension sub-check under the Extension build-date point.
    ExtensionSub(ConstructionPhase),
    /// A per-surface (construction group) sub-check under the Builder's-letter point.
    SurfaceSub(ItemId),
    /// The SAME surface list under the CERTIFIER's-letter point, ticked
    /// independently — the assessor's split decides who evidences what.
    CertifierSurfaceSub(ItemId),
    /// A per-window-TYPE sub-check under the Window-documentation point.
    WindowTypeSub(ItemId),
    /// A per-door-TYPE sub-check under the Door-documentation point (door
    /// types key by their free-text name — the label IS the group).
    DoorTypeSub(String),
    Custom(ItemId),
}

/// What a sub-row's inline ✏ Rename writes to — the DURABLE model name the
/// whole app shows, via the SAME rename methods the Structure review uses
/// ("the letter uses THEIR name", so the assessor can clarify a name for the
/// client without leaving the tab).
#[derive(Debug, Clone, PartialEq)]
pub enum FollowUpRenameKey {
    /// A shared window type's model name → [`AppController::rename_window_type`].
    WindowType(ItemId),
    /// A door type's free-text name (renames every door sharing it) →
    /// [`AppController::rename_door_type`].
    DoorType(String),
    /// A construction group's display name →
    /// [`AppController::rename_structure_group`].
    Surface(ItemId),
}

/// One request-point row for the tab: its key, label, tick state, and whether
/// it can be removed (custom points only).
pub struct FollowUpPointRow {
    pub key: FollowUpRowKey,
    pub label: String,
    pub checked: bool,
    pub removable: bool,
    /// Per-thing sub-checks — extensions under the Extension build-date row,
    /// surface groups under the letter rows, window/door types under the
    /// documentation rows; empty for every other row.
    pub sub_rows: Vec<FollowUpSubRow>,
}

/// A sub-check row: one extension / surface group / window type / door type
/// under its parent request, so the assessor can ask about some and not others.
pub struct FollowUpSubRow {
    pub key: FollowUpRowKey,
    pub label: String,
    pub checked: bool,
    /// The inline-rename affordance for rows that show a durable model name
    /// (window/door types, construction groups); `None` for extensions.
    pub rename_key: Option<FollowUpRenameKey>,
    /// What the rename field should prefill with — the bare current name
    /// (`label` may be a composite like "name — Double · Wood/UPVC…").
    pub rename_prefill: String,
    /// A parked rename rejection to show under this row (window types only —
    /// their names carry integrity rules).
    pub rename_error: Option<String>,
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
            // Make sure the heating type reflects the placed objects before the
            // survey-driven selection reads it (covers records surveyed before
            // the object→heating sync existed).
            ber.sync_heating_system_from_objects();
            ber.followup.selected = followup::auto_selected(ber, reference_year);
            ber.followup.extensions = followup::candidate_extensions(ber);
            ber.followup.builder_surfaces = followup::builder_surface_groups(ber)
                .into_iter()
                .filter(|s| s.auto)
                .map(|s| s.id)
                .collect();
            // Per-type sub-checks seed FULL when their parent point seeded:
            // every window type / door type is asked about until unticked
            // (door types only when there's more than one — a single type
            // keeps the plain row).
            if ber.followup.selected.contains(&FollowUpPointId::WindowDop) {
                ber.followup.window_types = followup::candidate_window_types(ber)
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect();
            }
            if ber.followup.selected.contains(&FollowUpPointId::DoorDop) {
                let doors = followup::candidate_door_types(ber);
                if doors.len() > 1 {
                    ber.followup.door_types = doors;
                }
            }
            rebuild_followup_body(ber);
            ber.followup.initialized = true;
            seeded = true;
        }
        if seeded {
            let _ = self.persist();
        }

        let record = self.current_record.as_ref()?;
        let ber = &record.data;
        let current_id = ber.id.clone();
        let fu = &ber.followup;
        let candidates = followup::candidate_extensions(ber);
        let surfaces = followup::builder_surface_groups(ber);
        let window_types = followup::candidate_window_types(ber);
        let door_types = followup::candidate_door_types(ber);
        // Surface sub-rows show the group's durable display name, renamable
        // inline (same write as the Structure review's rename).
        let surface_sub =
            |s: &followup::SurfaceGroup, key: FollowUpRowKey, checked: bool| FollowUpSubRow {
                key,
                label: s.name.clone(),
                checked,
                rename_key: Some(FollowUpRenameKey::Surface(s.id)),
                rename_prefill: s.name.clone(),
                rename_error: None,
            };
        let mut points: Vec<FollowUpPointRow> = followup::ALL_POINTS
            .into_iter()
            .map(|id| {
                // The Extension build-date, letter and window/door
                // documentation points each show sub-checks (extensions /
                // surface groups / opening types) so some can be asked about
                // and others not.
                let checked = fu.selected.contains(&id);
                // Sub-checks show only under a ticked parent (so the shared
                // surface sub-checks appear under whichever works route is active).
                let sub_rows = if !checked {
                    Vec::new()
                } else if id == FollowUpPointId::ExtensionBuildDate {
                    candidates
                        .iter()
                        .map(|ph| FollowUpSubRow {
                            key: FollowUpRowKey::ExtensionSub(ph.clone()),
                            label: followup::extension_name(ber, ph),
                            checked: fu.extensions.contains(ph),
                            rename_key: None,
                            rename_prefill: String::new(),
                            rename_error: None,
                        })
                        .collect()
                } else if id == FollowUpPointId::BuilderUpgradeLetter
                    || id == FollowUpPointId::WorksEvidenceNoBuilder
                {
                    surfaces
                        .iter()
                        .map(|s| {
                            surface_sub(
                                s,
                                FollowUpRowKey::SurfaceSub(s.id),
                                fu.builder_surfaces.contains(&s.id),
                            )
                        })
                        .collect()
                } else if id == FollowUpPointId::CertifierNewbuildLetter {
                    // The same offered surfaces, ticked independently: the
                    // builder/certifier split of who evidences what.
                    surfaces
                        .iter()
                        .map(|s| {
                            surface_sub(
                                s,
                                FollowUpRowKey::CertifierSurfaceSub(s.id),
                                fu.certifier_surfaces.contains(&s.id),
                            )
                        })
                        .collect()
                } else if id == FollowUpPointId::WindowDop {
                    // One sub-check per shared window TYPE, so the assessor
                    // picks exactly which types the client is asked about.
                    window_types
                        .iter()
                        .map(|(tid, label)| FollowUpSubRow {
                            key: FollowUpRowKey::WindowTypeSub(*tid),
                            label: label.clone(),
                            checked: fu.window_types.contains(tid),
                            rename_key: Some(FollowUpRenameKey::WindowType(*tid)),
                            rename_prefill: ber
                                .window_types
                                .iter()
                                .find(|t| t.id == *tid)
                                .and_then(|t| t.specifics.name())
                                .unwrap_or_default()
                                .to_string(),
                            rename_error: self
                                .window_type_rename_error
                                .as_ref()
                                .filter(|(eid, _)| eid == tid)
                                .map(|(_, hint)| hint.clone()),
                        })
                        .collect()
                } else if id == FollowUpPointId::DoorDop && door_types.len() > 1 {
                    // Per-type sub-checks only when doors actually divide into
                    // more than one type; a single type keeps the plain row.
                    door_types
                        .iter()
                        .map(|name| FollowUpSubRow {
                            key: FollowUpRowKey::DoorTypeSub(name.clone()),
                            label: name.clone(),
                            checked: fu.door_types.contains(name),
                            rename_key: Some(FollowUpRenameKey::DoorType(name.clone())),
                            rename_prefill: name.clone(),
                            rename_error: None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                FollowUpPointRow {
                    key: FollowUpRowKey::BuiltIn(id),
                    label: id.label().to_string(),
                    checked,
                    removable: false,
                    sub_rows,
                }
            })
            .collect();
        points.extend(fu.custom.iter().map(|c| FollowUpPointRow {
            key: FollowUpRowKey::Custom(c.id.clone()),
            label: c.label.clone(),
            checked: c.checked,
            removable: true,
            sub_rows: Vec::new(),
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
        let candidates = followup::candidate_extensions(ber);
        let auto_surfaces: Vec<ItemId> = followup::builder_surface_groups(ber)
            .into_iter()
            .filter(|s| s.auto)
            .map(|s| s.id)
            .collect();
        let all_window_types: Vec<ItemId> = followup::candidate_window_types(ber)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let all_door_types = followup::candidate_door_types(ber);
        match key {
            FollowUpRowKey::BuiltIn(id) => {
                if id == FollowUpPointId::ExtensionBuildDate {
                    // Parent: ticking selects all extensions still needing a date;
                    // unticking clears them.
                    if ber.followup.selected.contains(&id) {
                        ber.followup.selected.retain(|p| *p != id);
                        ber.followup.extensions.clear();
                    } else {
                        ber.followup.extensions = candidates;
                        ber.followup.selected.push(id);
                    }
                } else if id == FollowUpPointId::BuilderUpgradeLetter
                    || id == FollowUpPointId::WorksEvidenceNoBuilder
                {
                    // Parent (either works route): ticking selects the
                    // auto-detected surfaces; unticking clears them.
                    if ber.followup.selected.contains(&id) {
                        ber.followup.selected.retain(|p| *p != id);
                        ber.followup.builder_surfaces.clear();
                    } else {
                        ber.followup.builder_surfaces = auto_surfaces;
                        ber.followup.selected.push(id);
                    }
                } else if id == FollowUpPointId::CertifierNewbuildLetter {
                    // Parent: plain toggle (the certifier request reads fine
                    // with no named surfaces); unticking clears its split.
                    if ber.followup.selected.contains(&id) {
                        ber.followup.selected.retain(|p| *p != id);
                        ber.followup.certifier_surfaces.clear();
                    } else {
                        ber.followup.selected.push(id);
                    }
                } else if id == FollowUpPointId::WindowDop && !all_window_types.is_empty() {
                    // Parent: ticking selects every window type; unticking
                    // clears them (mirrors the extension parent).
                    if ber.followup.selected.contains(&id) {
                        ber.followup.selected.retain(|p| *p != id);
                        ber.followup.window_types.clear();
                    } else {
                        ber.followup.window_types = all_window_types;
                        ber.followup.selected.push(id);
                    }
                } else if id == FollowUpPointId::DoorDop && all_door_types.len() > 1 {
                    // Parent over the per-door-type sub-checks (only offered
                    // with more than one door type).
                    if ber.followup.selected.contains(&id) {
                        ber.followup.selected.retain(|p| *p != id);
                        ber.followup.door_types.clear();
                    } else {
                        ber.followup.door_types = all_door_types;
                        ber.followup.selected.push(id);
                    }
                } else if let Some(pos) = ber.followup.selected.iter().position(|p| *p == id) {
                    ber.followup.selected.remove(pos);
                } else {
                    ber.followup.selected.push(id);
                }
            }
            FollowUpRowKey::ExtensionSub(phase) => {
                if let Some(pos) = ber.followup.extensions.iter().position(|p| *p == phase) {
                    ber.followup.extensions.remove(pos);
                } else {
                    ber.followup.extensions.push(phase);
                }
                // Keep the parent point present iff at least one extension is ticked.
                let has = !ber.followup.extensions.is_empty();
                let sel = &mut ber.followup.selected;
                let present = sel.contains(&FollowUpPointId::ExtensionBuildDate);
                if has && !present {
                    sel.push(FollowUpPointId::ExtensionBuildDate);
                } else if !has && present {
                    sel.retain(|p| *p != FollowUpPointId::ExtensionBuildDate);
                }
            }
            FollowUpRowKey::SurfaceSub(id) => {
                if let Some(pos) = ber.followup.builder_surfaces.iter().position(|s| *s == id) {
                    ber.followup.builder_surfaces.remove(pos);
                } else {
                    ber.followup.builder_surfaces.push(id);
                }
                // Sub-checks only show under a ticked parent, so when the last
                // surface is unticked, drop whichever works route is present.
                if ber.followup.builder_surfaces.is_empty() {
                    ber.followup.selected.retain(|p| {
                        *p != FollowUpPointId::BuilderUpgradeLetter
                            && *p != FollowUpPointId::WorksEvidenceNoBuilder
                    });
                }
            }
            FollowUpRowKey::CertifierSurfaceSub(id) => {
                // Independent tick of the same offered list — no parent drop
                // when empty (the certifier request stands without surfaces).
                if let Some(pos) = ber
                    .followup
                    .certifier_surfaces
                    .iter()
                    .position(|s| *s == id)
                {
                    ber.followup.certifier_surfaces.remove(pos);
                } else {
                    ber.followup.certifier_surfaces.push(id);
                }
            }
            FollowUpRowKey::WindowTypeSub(id) => {
                if let Some(pos) = ber.followup.window_types.iter().position(|t| *t == id) {
                    ber.followup.window_types.remove(pos);
                } else {
                    ber.followup.window_types.push(id);
                }
                // Keep the parent point present iff at least one type is
                // ticked (mirrors the extension sub-checks).
                let has = !ber.followup.window_types.is_empty();
                let sel = &mut ber.followup.selected;
                let present = sel.contains(&FollowUpPointId::WindowDop);
                if has && !present {
                    sel.push(FollowUpPointId::WindowDop);
                } else if !has && present {
                    sel.retain(|p| *p != FollowUpPointId::WindowDop);
                }
            }
            FollowUpRowKey::DoorTypeSub(name) => {
                if let Some(pos) = ber.followup.door_types.iter().position(|n| *n == name) {
                    ber.followup.door_types.remove(pos);
                } else {
                    ber.followup.door_types.push(name);
                }
                let has = !ber.followup.door_types.is_empty();
                let sel = &mut ber.followup.selected;
                let present = sel.contains(&FollowUpPointId::DoorDop);
                if has && !present {
                    sel.push(FollowUpPointId::DoorDop);
                } else if !has && present {
                    sel.retain(|p| *p != FollowUpPointId::DoorDop);
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

    /// Rename a named thing from a follow-up sub-row's inline ✏ — the SAME
    /// durable model writes the Structure review uses
    /// ([`rename_window_type`](Self::rename_window_type) /
    /// [`rename_door_type`](Self::rename_door_type) /
    /// [`rename_structure_group`](Self::rename_structure_group); a rejected
    /// window-type rename parks its hint for the sub-row to show). The letter
    /// then uses the new name: a rename that changes what the LETTER would say
    /// REGENERATES the draft (like a tick change — an unsent draft only; the
    /// one-shot lock keeps a sent email's text) and persists. A rejected or
    /// no-op rename — including renaming a name the letter doesn't mention
    /// (an unticked sub-row) — leaves the draft and any hand edits alone.
    pub fn rename_followup(&mut self, key: FollowUpRenameKey, name: String) {
        // The exact "would the letter change?" probe: assemble the fresh
        // composition before and after the model write and compare.
        let fresh_draft = |c: &Self| -> Option<String> {
            let ber = &c.current_record.as_ref()?.data;
            Some(followup::assemble_draft(ber, &ber.followup))
        };
        let before = fresh_draft(self);
        match key {
            FollowUpRenameKey::WindowType(id) => {
                self.rename_window_type(id, name);
                if self.window_type_rename_error.is_some() {
                    return; // rejected — nothing changed, keep the draft
                }
            }
            FollowUpRenameKey::DoorType(old) => self.rename_door_type(old, name),
            FollowUpRenameKey::Surface(id) => self.rename_structure_group(id, name),
        }
        if fresh_draft(self) == before {
            return; // the letter wouldn't change — don't clobber hand edits
        }
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        if !record.data.followup.is_sent() {
            rebuild_followup_body(&mut record.data);
            let _ = self.persist();
        }
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
        let (subject, body, items) = send_payload(ber);
        let config_file = self.sync_config_file.clone();

        let state = Arc::new(Mutex::new(FollowUpSendState::Sending));
        self.followup_send = Some(PendingSend {
            job_id: job_id.clone(),
            state: Arc::clone(&state),
        });
        // A retry clears any stale error for this record.
        self.followup_send_error = None;

        std::thread::spawn(move || {
            let result = crate::engine::send_details_request(
                &config_file,
                &job_id_str,
                &subject,
                &body,
                &items,
            );
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
    let fu = ber.followup.clone();
    ber.followup.body = followup::assemble_draft(ber, &fu);
}

/// Exactly what a send posts to the backend: `(subject, body, checklist
/// items)`. The body is the assessor's EDITED buffer (`followup.body` — every
/// edit lands there via `set_followup_body` before Send can fire), never a
/// re-assembled draft. Split out of [`AppController::send_followup`] so tests
/// can assert the payload at the engine seam without a network.
pub(super) fn send_payload(
    ber: &ber_model::ber::ber_struct::Ber,
) -> (String, String, Vec<(String, String)>) {
    (
        followup::subject(ber),
        ber.followup.body.clone(),
        ber.followup.checklist_items(ber),
    )
}
