//! BER record lifecycle and persistence orchestration: create/open,
//! checkpoints, the editing-layer snapshot, filtered persist and autosave.
use super::*;

/// Format version of the editing-layer snapshot (`Document` serde). Bump when
/// the serialized shape changes incompatibly — old snapshots are then ignored on
/// load and the editor rebuilds from the resolved BER points instead.
// v2: snapshot is now a multi-storey `Document` ({ storeys, active_storey })
// rather than a single storey's flat fields.
// v3: StoreyDoc gained `floor_exposure` / `ceiling_exposure` maps — old
// snapshots lack them; discard + rebuild from Store A (then seed/auto-derive).
pub(super) const PARAMETRIC_SNAPSHOT_VERSION: i64 = 3;

impl AppController {
    pub fn load_data_async(&self, tx: Sender<(String, f32)>) {
        let loader = self.loader_engine.clone();

        std::thread::spawn(move || {
            let _ = loader.load_general_data(tx);
        });
    }

    pub fn create_ber(&mut self, name: String, date_assessed: NaiveDate) -> Ber {
        let ber = Ber::new();
        let record = BerRecord {
            id: ber.id,
            name,
            date_assessed,
            data: ber.clone(),
        };

        let repo = BerRepository::new(&self.conn);
        repo.upsert(&record).expect("upsert failed");
        // From-scratch BER: no intake job exists for it, so queue it for
        // cloud-job registration — the push worker POSTs /surveyor/jobs when
        // connectivity allows, and the survey then uploads like any pulled job.
        if let Err(e) = repo.enqueue_job_registration(&record.id) {
            log::warn!("could not queue {} for job registration: {e}", record.id.0);
        }
        self.current_record = Some(record);
        // A freshly created BER starts on its first storey (which `commit_to_ber`
        // lazily appends on the first stroke).
        self.current_storey_idx = Some(0);
        ber
    }

    /// Deep-copy a saved BER into a BRAND-NEW, fully-persisted DB record and
    /// return its id. Used from Home's Load list — it writes a real new row (and
    /// clones the editing-layer snapshot if present) but does NOT open the copy:
    /// the assessor stays on the Load list, so `current_record` is untouched.
    ///
    /// The `Ber` data struct carries its own `id` (must equal the record id), so
    /// the clone's `data.id` is re-stamped to the new id. `date_assessed` is
    /// preserved — it's a copy of the SAME assessment, not a new one.
    pub fn duplicate_ber(&self, source: &ItemId) -> Result<ItemId, String> {
        let repo = BerRepository::new(&self.conn);
        let src = repo
            .load(source)
            .map_err(|e| format!("failed to load BER: {}", e))?;

        let new_id = ItemId::default();
        let mut data = src.data.clone();
        data.id = new_id.clone();
        let new_record = BerRecord {
            id: new_id.clone(),
            name: format!("{} (copy)", src.name),
            date_assessed: src.date_assessed,
            data,
        };
        repo.upsert(&new_record)
            .map_err(|e| format!("duplicate upsert failed: {}", e))?;

        // Clone the editing-layer snapshot (one row per ber, PK = ber_id) so the
        // copy reopens with the exact in-progress parametric state, not a rebuild.
        if let Some((version, storey_idx, json)) = repo
            .load_snapshot(source)
            .map_err(|e| format!("failed to load snapshot: {}", e))?
        {
            repo.save_snapshot(&new_id, version, storey_idx, &json)
                .map_err(|e| format!("duplicate snapshot save failed: {}", e))?;
        }

        Ok(new_id)
    }

    /// Set the BER's construction phases (house + extension years). These are the
    /// only phases the assessor can later assign to an area; captured on the
    /// Key Details form. No-op when no BER is loaded. Persists.
    pub fn set_construction_phases(&mut self, mut phases: ConstructionPhases) {
        if let Some(record) = self.current_record.as_mut() {
            // Booking provenance is set at seed time (from the booking), not on
            // the Key Details form — which rebuilds the phases from scratch. Carry
            // it across the rebuild for every phase that still exists.
            let prev = std::mem::take(&mut record.data.construction_phases.booking_sourced);
            for (phase, sourced) in prev {
                if sourced && phases.years.contains_key(&phase) {
                    phases.set_booking_sourced(phase, true);
                }
            }
            record.data.construction_phases = phases;
        }
        let _ = self.persist();
    }

    /// Rename the loaded BER. No-op when no BER is loaded. Persists (same
    /// write-through pattern as `set_construction_phases`).
    pub fn rename_ber(&mut self, name: String) {
        if let Some(record) = self.current_record.as_mut() {
            record.name = name;
        }
        let _ = self.persist();
    }

    /// Set the loaded BER's property address. No-op when no BER is loaded.
    /// Persists (same write-through pattern as `set_construction_phases`).
    /// Normally seeded from the intake booking form; this is the manual path
    /// for from-scratch BERs.
    pub fn set_address(&mut self, address: String) {
        if let Some(record) = self.current_record.as_mut() {
            record.data.address = address;
        }
        let _ = self.persist();
    }

    /// Set the loaded BER's eircode. No-op when no BER is loaded. Persists
    /// (same write-through pattern as `set_construction_phases`).
    pub fn set_eircode(&mut self, eircode: String) {
        if let Some(record) = self.current_record.as_mut() {
            record.data.eircode = eircode;
        }
        let _ = self.persist();
    }

    /// Set the loaded BER's assessment date. No-op when no BER is loaded.
    /// Persists (same write-through pattern as `set_construction_phases`).
    pub fn set_date_assessed(&mut self, date_assessed: NaiveDate) {
        if let Some(record) = self.current_record.as_mut() {
            record.date_assessed = date_assessed;
        }
        let _ = self.persist();
    }

    /// Set the loaded BER's client-intake staging fields (Key Details form). No-op
    /// when no BER is loaded. Persists (same write-through pattern as above).
    pub fn set_intake(&mut self, mut intake: IntakeDetails) {
        if let Some(record) = self.current_record.as_mut() {
            // The Letter of Engagement is linked by BERintake, not entered on the
            // Key Details form — carry it across a form rebuild rather than
            // dropping it. (Only if the incoming intake doesn't already carry one.)
            if intake.letter_of_engagement.is_none() {
                intake.letter_of_engagement = record.data.intake.letter_of_engagement.take();
            }
            record.data.intake = intake;
        }
        let _ = self.persist();
    }

    /// The loaded record's name, for the workspace top bar. `None` when no BER
    /// is loaded.
    pub fn current_record_name(&self) -> Option<String> {
        self.current_record.as_ref().map(|r| r.name.clone())
    }

    /// The loaded record's Key Details — `(name, date_assessed, phases, intake)`
    /// — for pre-populating the form on workspace entry. `None` when no BER is
    /// loaded.
    pub fn current_key_details(
        &self,
    ) -> Option<(
        String,
        String,
        String,
        NaiveDate,
        ConstructionPhases,
        IntakeDetails,
    )> {
        self.current_record.as_ref().map(|r| {
            (
                r.name.clone(),
                r.data.address.clone(),
                r.data.eircode.clone(),
                r.date_assessed,
                r.data.construction_phases.clone(),
                r.data.intake.clone(),
            )
        })
    }

    /// All saved BERs as lightweight summaries (newest assessment date first),
    /// for the Home page's Load panel. Empty on a query failure.
    pub fn list_bers(&self) -> Vec<BerSummary> {
        BerRepository::new(&self.conn).list().unwrap_or_default()
    }

    /// Delete a saved BER (and its editing-layer snapshot). REFUSES to delete
    /// the currently-open record — in practice unreachable, since Home (the
    /// only place delete is offered) is only entered record-free via
    /// `exit_ber`, but the guard keeps the invariant explicit.
    pub fn delete_ber(&mut self, id: &ItemId) -> Result<(), String> {
        if self.current_record.as_ref().is_some_and(|r| r.id == *id) {
            return Err("cannot delete the currently-open BER".to_string());
        }
        BerRepository::new(&self.conn)
            .delete(id)
            .map_err(|e| format!("delete failed: {}", e))
    }

    /// Leave the workspace: flush any pending edits to disk, then clear ALL
    /// loaded/session state back to the fresh-app baseline (the state
    /// `AppController::new` builds), so a subsequent `create_ber`/`open_ber`
    /// starts clean. `current_storey_idx` returns to `None` (the no-record
    /// state); create/open set it to `Some(0)` on entry. Returns the flush
    /// result — the clear happens regardless.
    pub fn exit_ber(&mut self) -> Result<(), String> {
        let flushed = self.flush_if_dirty();
        // Barrier: Home (and its delete) is only reachable through here, so
        // waiting out the background writer's queue closes the race where a
        // queued autosave upsert resurrects a record deleted moments later.
        if let Some(writer) = &self.writer {
            writer.drain();
        }
        self.current_record = None;
        self.current_storey_idx = None;
        self.document = Document::default();
        self.sketch_engine = SketchEngine::new();
        self.review = None;
        self.pending_redraw = None;
        self.pending_new_storey = None;
        self.inherited_seed_pending.clear();
        self.living_area_mode = false;
        self.porch_mode = false;
        self.exposure_mode = false;
        self.selected_exposure_wall = None;
        self.exposure_sel = None;
        self.exposure_drag = None;
        self.selected_riser = None;
        self.pending_cut = None;
        self.phase_mode = false;
        self.phase_regions.clear();
        self.pending_phase_cut = None;
        self.step_mode = false;
        self.step_regions.clear();
        self.pending_step_cut = None;
        self.ceiling_step_mode = false;
        self.ceiling_step_regions.clear();
        self.pending_ceiling_step = None;
        self.ceiling_point_mode = false;
        self.pending_roof_point = None;
        self.pending_centre_between = None;
        self.continuation_tool = None;
        self.pending_ceiling_continuation = None;
        self.reset_ceiling_confirm = false;
        self.pending_roof_ridge = None;
        self.roof_ridge_notice = None;
        self.pending_roof_rect = None;
        self.dormer_mode = false;
        self.pending_rect_is_dormer = false;
        self.splice_debug.clear();
        self.last_snap_action = None;
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
        self.history.clear();
        self.redo.clear();
        self.view = ViewTransform::default();
        // Nothing loaded → nothing pending against disk or the derive pipeline.
        self.saved_revision = self.revision;
        self.derived_revision = self.revision;
        flushed
    }

    /// Load an existing BER into the editor: sets it as the current record and
    /// reconstructs the parametric sketch + dimensioning state from the stored
    /// snapshot, so editing resumes with every collinearity equivalence and
    /// entered dimension intact. If the snapshot is missing or from an older,
    /// incompatible version, falls back to rebuilding a fresh parametric model
    /// from the resolved `floors` geometry (equivalences/dimensions are then
    /// lost — unavoidable for pre-snapshot records).
    pub fn open_ber(&mut self, id: &ItemId) -> Result<(), String> {
        // Boundary flush: switching records persists any pending edits on the
        // current one first, so the idle-deferred writes aren't lost when the
        // assessor opens a different (or the same) BER.
        let _ = self.flush_if_dirty();
        let record = BerRepository::new(&self.conn)
            .load(id)
            .map_err(|e| format!("failed to load BER: {}", e))?;
        self.current_record = Some(record);
        // Rebuild ONE StoreyDoc per storey from its resolved points — the fallback
        // when there's no compatible snapshot. Grow the container to N storeys,
        // then rebuild each in place (so multi-storey records stay fully editable
        // even pre-snapshot). A reopened BER lands on storey 0; a snapshot may move
        // it to the storey the assessor was last editing (see `try_restore_snapshot`).
        let n = self
            .current_record
            .as_ref()
            .unwrap()
            .data
            .storeys
            .len()
            .max(1);
        self.document = Document::default();
        for _ in 1..n {
            self.document.push_storey();
        }
        for i in 0..n {
            self.document.set_active_storey(i);
            let storey = self
                .current_record
                .as_ref()
                .unwrap()
                .data
                .storeys
                .get(i)
                .cloned();
            self.load_storey_into_editor(storey.as_ref());
        }
        self.document.set_active_storey(0);
        self.current_storey_idx = Some(0);
        // Restore the exact in-progress editing state (every storey's shared
        // params / collinearity / curves + un-finalised work the resolved record
        // can't hold) from the snapshot; falls back silently to the rebuild above
        // if it's missing or from an incompatible version.
        self.try_restore_snapshot(id);
        // RESTART-SAFE drawn-path seeding: re-derive which upper storeys still
        // await their inherited floor levels ({idx > 0, outline un-finalised}
        // — the snapshot above decides which outlines those are). The
        // new-storey FLOW itself is not restored (`load_storey_into_editor`
        // cleared it); only the seed transition survives.
        self.rederive_inherited_seed_pending();
        // Rebuild ALL session + durable-side-table editing state from the
        // resolved record (Layer 1), for every storey — the shared sequence used
        // by both load paths. Runs AFTER `try_restore_snapshot` (which replaced
        // the whole Document) and BEFORE `refresh_all_storeys` re-resolves.
        self.reconstruct_session_state_from_record();
        // An in-flight ceiling-step line belongs to the PREVIOUS record — its
        // prompt holds VertexIds into that record's outline plan. A pending
        // roof point's / ridge's prompt does too.
        self.pending_ceiling_step = None;
        self.ceiling_step_mode = false;
        self.pending_roof_point = None;
        self.pending_centre_between = None;
        self.continuation_tool = None;
        self.pending_ceiling_continuation = None;
        self.reset_ceiling_confirm = false;
        self.pending_roof_ridge = None;
        self.roof_ridge_notice = None;
        self.pending_roof_rect = None;
        self.dormer_mode = false;
        self.pending_rect_is_dormer = false;
        self.ceiling_point_mode = false;
        self.view = ViewTransform::default();
        self.document.edit_state = EditState::default();
        self.review = None;
        // Restart-safe upper-outline placement: a not-yet-finalised upper
        // outline (its wall lengths pinned but its free axis un-positioned)
        // needs the storey-below footprint re-supplied so the vertical-placement
        // prompt re-appears (the override is `#[serde(skip)]`).
        self.reestablish_position_reference_override();
        self.compute_next_dimension_prompt();
        // Heal opening anchors against the (possibly re-minted) outline VertexIds
        // BEFORE the re-resolve re-mints the walls — otherwise a snapshot-less
        // load re-homes every opening onto the wrong wall.
        self.reseed_all_opening_anchors();
        // Re-resolve every storey so the recomposed ceilings (above) get the clean,
        // per-phase carve re-applied — this is what drops the stale split while
        // preserving the deliberate structure splits + steps + apexes that
        // `heal_phase_induced_roof_splits` replayed. Runs AFTER the exposure /
        // construction seeds so the resolve's fresh-id stamp reads the seeded
        // durable maps rather than resetting them to Unset. Resolve keeps ceilings
        // verbatim when the outline is unchanged, so an untagged storey (not
        // recomposed) stays byte-identical.
        self.refresh_all_storeys();
        self.history.clear();
        self.redo.clear();
        // Freshly loaded → nothing pending against disk, and the geometry was
        // just resolved above, so the next no-edit `materialize` is a no-op.
        self.saved_revision = self.revision;
        self.derived_revision = self.revision;
        // LOAD-TIME PURGE (legacy migration): drop the floor/ceiling exposure
        // map entries whose keyed region now resolves InterStorey — the
        // pre-feature auto-seeded Party/NonHeatLoss entries, shadowed by the
        // derivation that just ran in the refresh above. Anything dropped
        // leaves the record dirty so the migrated classification persists.
        if self.purge_shadowed_exposure_entries_all() {
            let _ = self.mark_dirty();
        }
        Ok(())
    }

    /// Run [`document::purge_shadowed_exposure_entries`] over every storey —
    /// the shared load-path tail (`open_ber` / `load_checkpoint`), called after
    /// the first `refresh_all_storeys` so the regions carry the derivation's
    /// fresh `InterStorey` stamps. Returns whether any entry was dropped.
    fn purge_shadowed_exposure_entries_all(&mut self) -> bool {
        let Some(record) = self.current_record.as_ref() else {
            return false;
        };
        let n = record.data.storeys.len().min(self.document.storeys.len());
        let mut purged = false;
        for idx in 0..n {
            purged |= document::purge_shadowed_exposure_entries(
                &mut self.document.storeys[idx],
                &record.data.storeys[idx],
            );
        }
        purged
    }

    /// Rebuild ALL session + durable-side-table editing state from the resolved
    /// record (Layer 1), for EVERY storey. The single shared sequence behind both
    /// load paths — `open_ber` calls it after `try_restore_snapshot`, and
    /// `load_checkpoint` calls it as the ONLY source (no snapshot). Keeping it in
    /// one place is deliberate: the previous duplicated, drifted versions are what
    /// left the checkpoint path missing the exposure/construction seeds (the
    /// "floor goes red after adding a window" bug) and reconstructing only
    /// storey 0.
    ///
    /// Order matters: ceiling-step regions must be reconstructed before
    /// `heal_phase_induced_roof_splits` (it reads them); the seeds backfill the
    /// durable exposure/construction maps from Layer 1 BEFORE the caller's
    /// `refresh_all_storeys` re-resolves (so the fresh-id stamps read the seeded
    /// maps rather than resetting them to `Unset`).
    fn reconstruct_session_state_from_record(&mut self) {
        self.phase_regions.clear();
        self.step_regions.clear();
        self.ceiling_step_regions.clear();
        let n = self
            .current_record
            .as_ref()
            .map(|r| r.data.storeys.len())
            .unwrap_or(0)
            .min(self.document.storeys.len());
        for i in 0..n {
            self.reconstruct_phase_regions_for(i);
            self.reconstruct_step_regions_for(i);
            self.load_ceiling_step_regions_for(i); // restore the durable step list
            self.reconstruct_wall_face_partitions_for(i); // Layer-1 recipe → working map
            self.reconstruct_wall_sections_for(i); // resolved walls → 1-D sections
        }
        // Backfill the durable exposure + construction maps from the resolved
        // regions — now the SOLE persisted source (these are `skip_serializing`,
        // so the snapshot no longer carries them). Idempotent (`or_insert`): a
        // legacy snapshot that still deserialized them is preserved.
        //
        // MUST run BEFORE `heal_phase_induced_roof_splits`: heal recomposes a
        // phase-tagged storey's ceilings via `default_for_footprint` (DEFAULT
        // exposure), so seeding after it would read the wiped ceilings and lose the
        // ceiling exposure on the snapshot-less (checkpoint) path. Capturing here
        // takes the saved Layer-1 exposures; the caller's re-resolve then
        // re-stamps them onto the (deterministically recomposed) ceilings.
        self.seed_riser_exposure_from_record();
        self.seed_horizontal_exposure_from_record();
        self.seed_construction_stores_from_record();
        // Recompose phase-tagged ceilings from their durable inputs (drops a stale
        // pre-fix phase-induced split); the caller's re-resolve re-applies the
        // clean carve and re-stamps the exposures seeded just above.
        self.heal_phase_induced_roof_splits();
        // Recompose ceilings that carry OUTLINE-EDIT SCARS (field report
        // eddc2f3f): records saved before the in-flux-outline resolve gate baked
        // a superseded cut line into their regions-as-truth ceilings.
        self.heal_outline_edit_roof_scars();
    }

    /// MIGRATION self-heal for the in-flux-outline resolve gate (field report
    /// eddc2f3f): records saved before the fix re-fit their regions-as-truth
    /// ceilings against a MID-DIMENSIONING (or undone) outline, baking the
    /// provisional cut line in as a permanent scar — an unjustified flat seam
    /// with `Unset` sliver fills beside it, and wall tops tracing the stale
    /// edges. Ceilings are regions-as-truth (`resolve_storey` keeps them
    /// verbatim while the footprint is unchanged), so the scar survives every
    /// reload on its own.
    ///
    /// Rebuild each SCARRED storey's ceilings from its durable declarative
    /// inputs via [`recompose_ceilings`](Self::recompose_ceilings) — the same
    /// repair shape as [`heal_phase_induced_roof_splits`]
    /// (Self::heal_phase_induced_roof_splits); deliberate structure splits,
    /// steps/dividers, apexes and dormers replay from their durable fields, the
    /// scar (which no durable input produces) is dropped, and the caller's
    /// re-resolve re-stamps the exposures seeded before this ran (the
    /// containment fallback re-applies a painted exposure whose region ring the
    /// recompose changed). The cross-storey derivation then re-splits and
    /// re-stamps the `InterStorey` pieces.
    ///
    /// GUARDS: detection-gated (`Storey::ceilings_carry_outline_edit_scars`) so
    /// a clean storey keeps its stored ceilings byte-identical (the
    /// regions-as-truth reload fixpoint); phase-tagged storeys are skipped
    /// (already recomposed by the phase heal just above); a storey whose
    /// outline is still mid-dimensioning defers (its footprint is about to
    /// move — the resolve gate holds its record steady until the loop
    /// completes).
    fn heal_outline_edit_roof_scars(&mut self) {
        let n = self
            .current_record
            .as_ref()
            .map(|r| r.data.storeys.len())
            .unwrap_or(0)
            .min(self.document.storeys.len());
        for idx in 0..n {
            if self.phase_regions.get(&idx).is_some_and(|v| !v.is_empty()) {
                continue; // already recomposed by the phase heal
            }
            if !self
                .document
                .storeys
                .get(idx)
                .is_some_and(|sd| sd.measurements_complete())
            {
                continue; // outline in flux — defer to the resolve gate
            }
            let scarred = self
                .current_record
                .as_ref()
                .and_then(|r| r.data.storeys.get(idx))
                .is_some_and(|s| {
                    s.ceiling_height_m.is_some()
                        && !s.ceilings.regions.is_empty()
                        && s.ceilings_carry_outline_edit_scars()
                });
            if scarred {
                log::info!(
                    "storey {idx}: outline-edit scar detected in stored ceilings — \
                     recomposing from durable inputs"
                );
                self.recompose_ceilings(idx);
            }
        }
    }

    /// MIGRATION self-heal for the per-phase-zone roof fix: records saved before
    /// the fix baked a spurious phase-induced split into their stored ceilings
    /// (a uniform-phase floor fragmented by a step below seeded a redundant
    /// coplanar same-construction boundary onto the storey above, splitting its
    /// roof). Ceilings are regions-as-truth — `resolve_storey` keeps them verbatim
    /// and the phase carve only ever SPLITS — so that stale boundary survives
    /// every reload.
    ///
    /// Rebuild each PHASE-TAGGED storey's ceilings from its durable declarative
    /// inputs via [`recompose_ceilings`](Self::recompose_ceilings) (default footprint →
    /// ceiling steps → apexes → structure roof-splits). Recompose does NOT
    /// reproduce the phase carve, so the stale split is dropped; the re-resolve
    /// at the end of `open_ber` then re-applies the now-clean per-phase carve.
    /// Deliberate structure roof-splits, ceiling steps, and apexes are replayed
    /// from their durable fields, so they all survive.
    ///
    /// GUARD: gated on the storey being phase-tagged (its phase regions were
    /// reconstructed above). A spurious phase-induced roof split can only exist
    /// where there were phase boundaries, so an untagged storey needs no heal —
    /// and `recompose_ceilings` is skipped there, keeping its stored ceilings
    /// byte-identical (the regions-as-truth reload fixpoint). Recompose is
    /// deterministic, so re-running it on an already-clean phase-tagged storey
    /// reproduces identical geometry — the fixpoint holds there too.
    ///
    /// Requires the per-storey ceiling-step regions to be reconstructed first
    /// (`recompose_ceilings` reads `ceiling_step_regions[idx]`).
    fn heal_phase_induced_roof_splits(&mut self) {
        let n = self
            .current_record
            .as_ref()
            .map(|r| r.data.storeys.len())
            .unwrap_or(0)
            .min(self.document.storeys.len());
        for idx in 0..n {
            // Phase-tagged ⇔ reconstructed phase regions exist for this storey.
            if self.phase_regions.get(&idx).is_some_and(|v| !v.is_empty()) {
                self.recompose_ceilings(idx);
            }
        }
    }

    /// Seed the durable `riser_exposure` table from the persisted resolved
    /// risers — legacy records (and any whose snapshot predates the table) carry
    /// a riser's exposure only on its resolved `WallRegion`, which the first
    /// fresh-id re-resolve would otherwise reset to `Unset`. Idempotent: fills
    /// only gaps, keyed by the riser's base edge. `InterStorey` risers are
    /// SKIPPED (like `Unset`): that stamp is `derive_inter_storey`'s, re-derived
    /// on every resolve — seeding it would freeze a derived mark into the
    /// assessor override table.
    fn seed_riser_exposure_from_record(&mut self) {
        let n = self
            .current_record
            .as_ref()
            .map(|r| r.data.storeys.len())
            .unwrap_or(0)
            .min(self.document.storeys.len());
        for idx in 0..n {
            let entries: Vec<(
                (i64, i64, i64, i64),
                ber_model::ber::structural::WallExposure,
            )> = {
                let Some(record) = self.current_record.as_ref() else {
                    break;
                };
                let Some(storey) = record.data.storeys.get(idx) else {
                    continue;
                };
                self.document.storeys[idx]
                    .riser_walls(storey)
                    .into_iter()
                    .filter(|rw| {
                        use ber_model::ber::structural::WallExposure;
                        rw.exposure != WallExposure::Unset
                            && rw.exposure != WallExposure::InterStorey
                    })
                    .map(|rw| (document::riser_key(rw.a, rw.b), rw.exposure))
                    .collect()
            };
            let sd = &mut self.document.storeys[idx];
            for (k, e) in entries {
                sd.riser_exposure.entry(k).or_insert(e);
            }
        }
    }

    /// Seed the durable `floor_exposure` / `ceiling_exposure` maps from the
    /// persisted resolved regions — legacy records (and any whose snapshot
    /// predates the maps) carry a floor/ceiling boundary condition only on its
    /// resolved `FloorRegion` / `CeilingRegion`, which the first fresh-id
    /// re-resolve would otherwise reset to `Unset`. Idempotent: fills only gaps,
    /// keyed by the region's quantized centroid (the same key the resolve stamp
    /// and the manual-marking ops use). Skips `Unset` regions (no value to
    /// preserve — those re-derive from the stack) and `InterStorey` ones (that
    /// stamp is `derive_inter_storey`'s, re-derived on every resolve — seeding
    /// it would freeze a derived mark into the assessor map).
    pub(super) fn seed_horizontal_exposure_from_record(&mut self) {
        use ber_model::ber::structural::{CeilingExposure, FloorExposure};
        let n = self
            .current_record
            .as_ref()
            .map(|r| r.data.storeys.len())
            .unwrap_or(0)
            .min(self.document.storeys.len());
        for idx in 0..n {
            #[allow(clippy::type_complexity)]
            let (floors, ceilings): (
                Vec<(document::CentroidKey, FloorExposure)>,
                Vec<(document::CentroidKey, CeilingExposure)>,
            ) = {
                let Some(record) = self.current_record.as_ref() else {
                    break;
                };
                let Some(storey) = record.data.storeys.get(idx) else {
                    continue;
                };
                let floors = storey
                    .floors
                    .regions
                    .iter()
                    .filter(|r| {
                        !matches!(
                            r.exposure,
                            FloorExposure::Unset
                                | FloorExposure::InterStorey
                                | FloorExposure::Irrelevant
                        )
                    })
                    .map(|r| (document::centroid_key(r.centroid()), r.exposure))
                    .collect();
                let ceilings = storey
                    .ceilings
                    .regions
                    .iter()
                    .filter(|r| {
                        !matches!(
                            r.exposure,
                            CeilingExposure::Unset
                                | CeilingExposure::InterStorey
                                | CeilingExposure::Irrelevant
                        )
                    })
                    .map(|r| (document::centroid_key(r.centroid()), r.exposure))
                    .collect();
                (floors, ceilings)
            };
            let sd = &mut self.document.storeys[idx];
            for (k, e) in floors {
                sd.floor_exposure.entry(k).or_insert(e);
            }
            for (k, e) in ceilings {
                sd.ceiling_exposure.entry(k).or_insert(e);
            }
        }
    }

    /// Seed the durable WALL + FLOOR + CEILING/ROOF construction stores from the
    /// persisted resolved regions — legacy records (and any whose snapshot
    /// predates the stores) carry an assigned construction only on its resolved
    /// region, which the first fresh-id re-resolve would otherwise drop back to
    /// the derived default. Idempotent: fills only gaps; floors/ceilings keyed by
    /// the region's quantized centroid, walls by their parametric `SurfaceRef`
    /// (base-edge midpoint → outline vertex, the same key
    /// `stamp_wall_constructions` reads). Skips regions still at the default
    /// (type + make-up), which carry no assignment to preserve, and walls with no
    /// perimeter `SurfaceRef` (risers — their construction isn't `SurfaceRef`-keyed).
    pub(super) fn seed_construction_stores_from_record(&mut self) {
        use ber_model::ber::structural::{
            CeilingConstructionType, ConstructionMakeup, FloorConstructionType,
            WallConstructionType,
        };
        let n = self
            .current_record
            .as_ref()
            .map(|r| r.data.storeys.len())
            .unwrap_or(0)
            .min(self.document.storeys.len());
        for idx in 0..n {
            type WallEntry = (
                WallConstructionType,
                ConstructionMakeup,
                ConstructionPhase,
                Option<String>,
            );
            #[allow(clippy::type_complexity)]
            let (walls, risers, floors, ceilings): (
                Vec<(manip_plan::SurfaceRef, WallEntry)>,
                Vec<(document::RiserKey, WallEntry)>,
                Vec<(
                    document::CentroidKey,
                    (
                        FloorConstructionType,
                        ConstructionMakeup,
                        ConstructionPhase,
                        Option<String>,
                    ),
                )>,
                Vec<(
                    document::CentroidKey,
                    (
                        CeilingConstructionType,
                        ConstructionMakeup,
                        ConstructionPhase,
                        Option<String>,
                    ),
                )>,
            ) = {
                let Some(record) = self.current_record.as_ref() else {
                    break;
                };
                let Some(storey) = record.data.storeys.get(idx) else {
                    continue;
                };
                // Map each resolved wall's base midpoint back to its durable key —
                // a perimeter SurfaceRef, or (for a riser) its quantized base edge.
                let plan = &self.document.storeys[idx].outline.floor_plan;
                let mut walls = Vec::new();
                let mut risers = Vec::new();
                // Perimeter AND porch inner walls: a porch wall's base midpoint
                // has no outline SurfaceRef, so it seeds into the riser store —
                // the same base-edge key its attribution writes and
                // `stamp_wall_constructions` reads. Without this, a saved
                // porch-wall attribution dies on the first re-resolve after an
                // app restart (regions re-mint; the session store is empty).
                for r in storey
                    .walls
                    .regions
                    .iter()
                    .chain(storey.porch_walls.regions.iter())
                {
                    let Some(c) = record.data.wall_construction(r.construction_id) else {
                        continue;
                    };
                    let ty = c.construction_type;
                    let mk = c.base.construction_makeup.clone();
                    let ph = c.base.construction_phase.clone();
                    let nm = c.base.name.clone();
                    if ty == WallConstructionType::default() && mk == ConstructionMakeup::default()
                    {
                        continue;
                    }
                    let Some((e0, e1)) = r.base_edge() else {
                        continue;
                    };
                    let mid = domain::points::Point2f32::new(
                        ((e0.x + e1.x) * 0.5) as f32,
                        ((e0.y + e1.y) * 0.5) as f32,
                    );
                    if let Some(sref) = plan.surface_ref_at(mid, 0.05) {
                        walls.push((sref, (ty, mk, ph, nm)));
                    } else {
                        let key = document::riser_key(
                            domain::points::Point2f64::new(e0.x, e0.y),
                            domain::points::Point2f64::new(e1.x, e1.y),
                        );
                        risers.push((key, (ty, mk, ph, nm)));
                    }
                }
                let floors = storey
                    .floors
                    .regions
                    .iter()
                    .filter_map(|r| {
                        let c = record.data.floor_construction(r.construction_id)?;
                        let ty = c.construction_type;
                        let mk = c.base.construction_makeup.clone();
                        let ph = c.base.construction_phase.clone();
                        let nm = c.base.name.clone();
                        (ty != FloorConstructionType::default()
                            || mk != ConstructionMakeup::default())
                        .then(|| (document::centroid_key(r.centroid()), (ty, mk, ph, nm)))
                    })
                    .collect();
                let ceilings = storey
                    .ceilings
                    .regions
                    .iter()
                    .filter_map(|r| {
                        let c = record.data.ceiling_construction(r.construction_id)?;
                        let ty = c.construction_type;
                        let mk = c.base.construction_makeup.clone();
                        let ph = c.base.construction_phase.clone();
                        let nm = c.base.name.clone();
                        (ty != CeilingConstructionType::default()
                            || mk != ConstructionMakeup::default())
                        .then(|| (document::centroid_key(r.centroid()), (ty, mk, ph, nm)))
                    })
                    .collect();
                (walls, risers, floors, ceilings)
            };
            let sd = &mut self.document.storeys[idx];
            for (k, v) in walls {
                sd.wall_constructions.entry(k).or_insert(v);
            }
            for (k, v) in risers {
                sd.riser_constructions.entry(k).or_insert(v);
            }
            for (k, v) in floors {
                sd.floor_constructions.entry(k).or_insert(v);
            }
            for (k, v) in ceilings {
                sd.ceiling_constructions.entry(k).or_insert(v);
            }
        }
    }

    /// Replace the rebuilt editor state with the editing-layer snapshot for `id`,
    /// if one exists and matches the current format version. Silently keeps the
    /// rebuild on a missing / stale / unparseable snapshot (the resolved record
    /// stays the durable source of truth).
    pub(super) fn try_restore_snapshot(&mut self, id: &ItemId) {
        let Ok(Some((version, storey_idx, json))) =
            BerRepository::new(&self.conn).load_snapshot(id)
        else {
            return;
        };
        // `storey_idx` is now vestigial — the active storey lives in the Document.
        let _ = storey_idx;
        self.restore_document_snapshot(version, &json);
    }

    /// Replace the rebuilt editor `Document` with a serialized editing-layer
    /// snapshot `json` of format `version`, when it's compatible. Shared by the
    /// DB path ([`try_restore_snapshot`](Self::try_restore_snapshot)) and the
    /// checkpoint path ([`load_checkpoint`](Self::load_checkpoint)). Silently
    /// keeps the rebuild on a stale/incompatible version, an unparseable
    /// snapshot, or a storey-count mismatch — the resolved record stays the
    /// durable source of truth.
    pub(super) fn restore_document_snapshot(&mut self, version: i64, json: &str) {
        if version != PARAMETRIC_SNAPSHOT_VERSION {
            return;
        }
        let Ok(mut doc) = serde_json::from_str::<Document>(json) else {
            return;
        };
        // The snapshot is a whole multi-storey Document, written alongside the
        // resolved record — so it must describe the SAME storey count and have at
        // least one storey. On any mismatch keep the rebuild, which is always
        // consistent with `current_record`.
        let n = self
            .current_record
            .as_ref()
            .map(|r| r.data.storeys.len())
            .unwrap_or(0);
        if doc.storeys.is_empty() || doc.storeys.len() != n {
            return;
        }
        if doc.active_storey >= n {
            doc.active_storey = 0;
        }
        let idx = doc.active_storey;
        self.document = doc;
        self.current_storey_idx = Some(idx);
        // Resume on the OUTLINE (the canonical entry point, same as a fresh open).
        // The in-progress shapes are all restored in their slots; we just point
        // the live buffer back at the outline.
        self.document.active = ActiveShape::Outline;
        self.load_active_into_buffer();
    }

    /// DEV: write the current in-memory BER to the checkpoint file so the next
    /// `BER_CHECKPOINT=1 cargo run` reloads this exact plan instead of you
    /// redrawing it. No-op (error) when no BER is loaded.
    pub fn save_checkpoint(&mut self) -> Result<(), String> {
        // Flush the live editor buffer into `current_record` first, so the
        // checkpoint captures what's on screen right now.
        self.commit_to_ber()?;
        // Serialize the editing-layer Document snapshot alongside the Ber, so the
        // reload restores the exact parametric plans (and their VertexIds) instead
        // of rebuilding a fresh outline — mirrors the DB save's two halves.
        let doc_json = serde_json::to_string(&self.document).ok();
        let snapshot = doc_json
            .as_deref()
            .map(|j| (PARAMETRIC_SNAPSHOT_VERSION, j));
        let record = self.current_record.as_ref().ok_or("no BER loaded")?;
        crate::engine::checkpoint::save(
            &record.data,
            snapshot,
            crate::engine::checkpoint::DEFAULT_PATH,
        )
    }

    /// DEV: load the checkpoint file (if present) into the editor and return
    /// whether it succeeded. Mirrors `open_ber`'s reconstruction. Called on
    /// startup when `BER_CHECKPOINT` is set.
    pub fn load_checkpoint(&mut self) -> bool {
        self.load_checkpoint_from(crate::engine::checkpoint::DEFAULT_PATH)
    }

    /// DEV: [`load_checkpoint`](Self::load_checkpoint) from an explicit `path` —
    /// lets tests replay a captured checkpoint / bug-report `state.json` (same
    /// format) through the full open-BER reconstruction.
    pub fn load_checkpoint_from(&mut self, path: &str) -> bool {
        let Some(loaded) = crate::engine::checkpoint::load(path) else {
            return false;
        };
        let crate::engine::checkpoint::Loaded { ber, snapshot } = loaded;
        let record = BerRecord {
            id: ber.id,
            name: "checkpoint".to_string(),
            date_assessed: chrono::Local::now().naive_local().date(),
            data: ber,
        };
        self.current_record = Some(record);
        // Size the editor to EVERY storey and load each into its slot — mirrors
        // `open_ber`. A multi-storey checkpoint otherwise leaves `document.storeys`
        // at one entry while the data has more, panicking the moment a storey > 0
        // is resolved (`resolve_storey` indexes `self.document.storeys[idx]`).
        let n = self
            .current_record
            .as_ref()
            .unwrap()
            .data
            .storeys
            .len()
            .max(1);
        self.document = Document::default();
        for _ in 1..n {
            self.document.push_storey();
        }
        for i in 0..n {
            self.document.set_active_storey(i);
            let storey = self
                .current_record
                .as_ref()
                .unwrap()
                .data
                .storeys
                .get(i)
                .cloned();
            self.load_storey_into_editor(storey.as_ref());
        }
        self.document.set_active_storey(0);
        self.current_storey_idx = Some(0);
        // Restore the editing-layer Document snapshot the checkpoint carried, so
        // the exact parametric plans (and their VertexIds) resume — mirrors
        // `open_ber`'s `try_restore_snapshot`. On a missing / stale-version
        // snapshot (or a legacy Ber-only checkpoint) this no-ops and the rebuild
        // above stands; the anchor re-seed below then heals opening homing.
        if let Some((version, json)) = snapshot {
            self.restore_document_snapshot(version, &json);
        }
        // Same restart-safe re-derivation as open_ber.
        self.rederive_inherited_seed_pending();
        // Rebuild ALL editing state from Layer 1 (every storey) — the SAME shared
        // sequence as `open_ber`. Reconstructs phase/step/ceiling/partition/section
        // state AND seeds the durable exposure + construction maps (idempotent over
        // a restored snapshot). Seeding here is
        // what fixes the checkpoint "floor goes red after adding a window" bug
        // (§6) — previously the seeds ran only on the DB path, so the first
        // re-resolve reset exposures to `Unset`. It also reconstructs ALL storeys,
        // not just storey 0.
        self.reconstruct_session_state_from_record();
        // Same as open_ber: drop any line/point state from the previous session.
        self.pending_ceiling_step = None;
        self.ceiling_step_mode = false;
        self.pending_roof_point = None;
        self.pending_centre_between = None;
        self.continuation_tool = None;
        self.pending_ceiling_continuation = None;
        self.reset_ceiling_confirm = false;
        self.pending_roof_ridge = None;
        self.roof_ridge_notice = None;
        self.pending_roof_rect = None;
        self.dormer_mode = false;
        self.pending_rect_is_dormer = false;
        self.ceiling_point_mode = false;
        self.view = ViewTransform::default();
        self.document.edit_state = EditState::default();
        self.review = None;
        // Restart-safe upper-outline placement: a not-yet-finalised upper
        // outline (its wall lengths pinned but its free axis un-positioned)
        // needs the storey-below footprint re-supplied so the vertical-placement
        // prompt re-appears (the override is `#[serde(skip)]`).
        self.reestablish_position_reference_override();
        self.compute_next_dimension_prompt();
        // Heal opening anchors against the re-minted outline VertexIds BEFORE the
        // re-resolve re-mints the walls — the checkpoint has no Document snapshot,
        // so its outline is rebuilt via `from_clean_ring` and its perimeter
        // VertexIds are re-indexed; without this every opening re-homes wrong.
        self.reseed_all_opening_anchors();
        // Re-resolve every storey so the seeded maps are stamped onto the geometry
        // now (not on the first edit) — mirrors `open_ber`, and sets the revision
        // bookkeeping so the next no-edit `materialize` is a no-op.
        self.refresh_all_storeys();
        self.history.clear();
        self.redo.clear();
        self.saved_revision = self.revision;
        self.derived_revision = self.revision;
        // Same load-time purge as `open_ber` — drop pre-feature auto-seeded
        // map entries now shadowed by the InterStorey derivation.
        if self.purge_shadowed_exposure_entries_all() {
            let _ = self.mark_dirty();
        }
        true
    }

    /// Persist to SQLite only if there are un-saved edits since the last write.
    /// Called on the idle autosave tick and at session boundaries — NOT per edit.
    pub fn flush_if_dirty(&mut self) -> Result<(), String> {
        if self.revision != self.saved_revision {
            self.materialize(); // bring current_record geometry current before writing
            self.persist()?;
            self.saved_revision = self.revision;
        }
        Ok(())
    }

    /// Edit revision — bumped on every in-memory BER refresh. The UI watches this
    /// to detect activity and time the idle autosave.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether there are edits not yet handed to persistence (revision ahead
    /// of the last save). Drives the State category's autosave status line.
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Write a FINALISED copy of the current record to the DB. Components that
    /// aren't fully dimensioned — and thus may still be in pixel units — are
    /// dropped from what's SAVED (never from memory). So the database only ever
    /// holds complete, metric geometry, and reloading it "as points" is
    /// unambiguous. Triggered on every commit; the DB simply gains each part as
    /// it becomes finalised.
    pub(super) fn persist(&self) -> Result<(), String> {
        let Some(out) = self.build_persist_record() else {
            return Ok(());
        };
        let repo = BerRepository::new(&self.conn);
        repo.upsert(&out)
            .map_err(|e| format!("upsert failed: {}", e))?;
        if let Some(s) = self.build_snapshot() {
            repo.save_snapshot(&s.ber_id, s.version, s.storey_idx, &s.json)
                .map_err(|e| format!("snapshot save failed: {}", e))?;
        }
        self.trigger_push();
        Ok(())
    }

    /// Serialize the editing layer (the live [`Document`]: parametric plans +
    /// dimensioning + slots) for crash-recovery / exact reopen of IN-PROGRESS
    /// work. `None` when no BER is open. Transient fields are `#[serde(skip)]`ped.
    pub(super) fn build_snapshot(&self) -> Option<crate::engine::DocSnapshot> {
        let ber_id = self.current_record.as_ref()?.id.clone();
        let json = serde_json::to_string(&self.document).ok()?;
        Some(crate::engine::DocSnapshot {
            ber_id,
            version: PARAMETRIC_SNAPSHOT_VERSION,
            storey_idx: self.current_storey_idx.unwrap_or(0) as i64,
            json,
        })
    }

    /// Build the FILTERED record to persist (the DB never holds pixel/incomplete
    /// geometry). This is the UI-thread half of a save — the half that needs
    /// `self.document`; the resulting owned record can then be upserted inline
    /// (`persist`) or handed to the background writer (`autosave`).
    pub(super) fn build_persist_record(&self) -> Option<BerRecord> {
        let record = self.current_record.as_ref()?;
        let mut out = record.clone();
        // The living-area / porch overwrite below reads the ACTIVE editing
        // buffer, which belongs to `current_storey_idx`. On the All-storeys
        // view there IS no active storey — the buffer still holds whichever
        // storey was edited last, and the old `unwrap_or(0)` wrote that
        // buffer's slots onto storey 0 (a save from the All view after
        // editing a porch-less upper storey silently DELETED storey 0's
        // porches — field report: Ashgrove). No active storey → persist the
        // record as-is.
        let Some(idx) = self.current_storey_idx else {
            return Some(out);
        };
        if self.outline_finalised() {
            // Outline is metric + fully dimensioned → keep its floors/walls,
            // and persist only the living areas / porches that are themselves
            // done (the model owns the finalised-projection; `ensure_*` borrows
            // `out.data`, so resolve the id before the storey borrow below).
            let lists = self.document.finalised_living_area_lists();
            let porch_lists = self.document.finalised_porch_lists();
            let floor_cid = out.data.ensure_floor_construction(
                Default::default(),
                Default::default(),
                Default::default(),
            );
            if let Some(storey) = out.data.storeys.get_mut(idx) {
                storey.living_areas = FloorRegions::from_vertex_lists(lists, floor_cid);
                storey.porches = FloorRegions::from_vertex_lists(porch_lists, floor_cid);
            }
        } else if let Some(storey) = out.data.storeys.get_mut(idx) {
            // Outline not finalised → persist no derived geometry yet (it's
            // likely still in pixels; the model owns what "derived" means).
            storey.clear_derived_geometry();
        }
        Some(out)
    }

    /// Attach a background persistence writer (the real app does this; tests
    /// leave it unset and fall back to synchronous writes).
    pub fn attach_writer(&mut self, writer: crate::engine::PersistenceWriter) {
        self.writer = Some(writer);
    }

    /// Attach the background cloud-sync worker (real app only; needs config).
    pub fn attach_sync(&mut self, sync: crate::engine::SyncWorker) {
        self.sync = Some(sync);
    }

    /// Trigger an on-demand cloud pull (e.g. a "Sync now" action). No-op when
    /// sync isn't configured.
    pub fn sync_now(&self) {
        if let Some(sync) = &self.sync {
            sync.trigger_import();
        }
    }

    /// Attach the background cloud-push worker (real app only; needs config).
    pub fn attach_push(&mut self, push: crate::engine::PushWorker) {
        self.push = Some(push);
    }

    /// Poke the push worker after a save so the change reaches the cloud
    /// "whenever it can" (debounced worker-side). No-op when unconfigured.
    fn trigger_push(&self) {
        if let Some(push) = &self.push {
            push.trigger();
        }
    }

    /// Latest cloud-sync outcome for the UI, or `None` when sync is off.
    pub fn sync_status(&self) -> Option<crate::engine::SyncStatus> {
        self.sync.as_ref().map(|s| s.status())
    }

    /// Where attached/captured photo files are stored (set from `Paths` in
    /// `bootstrap`).
    pub fn set_photos_dir(&mut self, dir: std::path::PathBuf) {
        self.photos_dir = dir;
    }

    /// Save pending edits WITHOUT blocking the UI on disk I/O: build the filtered
    /// record on this thread, then hand it to the background writer. Falls back
    /// to a synchronous `flush_if_dirty` when no writer is attached (tests).
    /// Used by the idle autosave tick.
    pub fn autosave(&mut self) -> Result<(), String> {
        if self.revision == self.saved_revision {
            return Ok(());
        }
        if self.writer.is_none() {
            return self.flush_if_dirty();
        }
        self.materialize(); // bring current_record geometry current before snapshotting it
        if let Some(out) = self.build_persist_record() {
            let bundle = crate::engine::SaveBundle {
                record: out,
                snapshot: self.build_snapshot(),
            };
            self.writer.as_ref().unwrap().save(bundle);
            // The push worker's debounce outlasts the writer's coalescing
            // window, so by the time it reads the DB this save has landed.
            self.trigger_push();
        }
        // Optimistic: the latest is queued. A boundary `flush_if_dirty` (open/exit)
        // re-checks against `revision`, so within-window edits aren't lost.
        self.saved_revision = self.revision;
        Ok(())
    }

    /// The outline is finalised when its metre scale is set AND no wall is left
    /// to dimension — i.e. its resolved geometry is complete and in metres.
    pub(super) fn outline_finalised(&self) -> bool {
        self.document.outline.dimensioning.scale_set && self.document.outline.finalised()
    }
}
