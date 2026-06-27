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

        BerRepository::new(&self.conn)
            .upsert(&record)
            .expect("upsert failed");
        self.current_record = Some(record);
        // A freshly created BER starts on its first storey (which `commit_to_ber`
        // lazily appends on the first stroke).
        self.current_storey_idx = Some(0);
        ber
    }

    /// Set the BER's construction phases (house + extension years). These are the
    /// only phases the assessor can later assign to an area; captured on the
    /// Key Details form. No-op when no BER is loaded. Persists.
    pub fn set_construction_phases(&mut self, phases: ConstructionPhases) {
        if let Some(record) = self.current_record.as_mut() {
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

    /// Set the loaded BER's assessment date. No-op when no BER is loaded.
    /// Persists (same write-through pattern as `set_construction_phases`).
    pub fn set_date_assessed(&mut self, date_assessed: NaiveDate) {
        if let Some(record) = self.current_record.as_mut() {
            record.date_assessed = date_assessed;
        }
        let _ = self.persist();
    }

    /// The loaded record's name, for the workspace top bar. `None` when no BER
    /// is loaded.
    pub fn current_record_name(&self) -> Option<String> {
        self.current_record.as_ref().map(|r| r.name.clone())
    }

    /// The loaded record's Key Details — `(name, date_assessed, phases)` — for
    /// pre-populating the form on workspace entry. `None` when no BER is loaded.
    pub fn current_key_details(&self) -> Option<(String, NaiveDate, ConstructionPhases)> {
        self.current_record.as_ref().map(|r| {
            (
                r.name.clone(),
                r.date_assessed,
                r.data.construction_phases.clone(),
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
        self.roof_point_mode = false;
        self.pending_roof_point = None;
        self.pending_roof_ridge = None;
        self.pending_roof_rect = None;
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
        self.pending_roof_ridge = None;
        self.pending_roof_rect = None;
        self.roof_point_mode = false;
        self.view = ViewTransform::default();
        self.document.edit_state = EditState::default();
        self.review = None;
        self.compute_next_dimension_prompt();
        // Re-resolve every storey so the recomposed roofs (above) get the clean,
        // per-phase carve re-applied — this is what drops the stale split while
        // preserving the deliberate structure splits + steps + apexes that
        // `heal_phase_induced_roof_splits` replayed. Runs AFTER the exposure /
        // construction seeds so the resolve's fresh-id stamp reads the seeded
        // durable maps rather than resetting them to Unset. Resolve keeps roofs
        // verbatim when the outline is unchanged, so an untagged storey (not
        // recomposed) stays byte-identical.
        self.refresh_all_storeys();
        self.history.clear();
        self.redo.clear();
        // Freshly loaded → nothing pending against disk, and the geometry was
        // just resolved above, so the next no-edit `materialize` is a no-op.
        self.saved_revision = self.revision;
        self.derived_revision = self.revision;
        Ok(())
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
        // phase-tagged storey's roofs via `default_for_footprint` (DEFAULT
        // exposure), so seeding after it would read the wiped roofs and lose the
        // ceiling exposure on the snapshot-less (checkpoint) path. Capturing here
        // takes the saved Layer-1 exposures; the caller's re-resolve then
        // re-stamps them onto the (deterministically recomposed) roofs.
        self.seed_riser_exposure_from_record();
        self.seed_horizontal_exposure_from_record();
        self.seed_construction_stores_from_record();
        // Recompose phase-tagged roofs from their durable inputs (drops a stale
        // pre-fix phase-induced split); the caller's re-resolve re-applies the
        // clean carve and re-stamps the exposures seeded just above.
        self.heal_phase_induced_roof_splits();
    }

    /// MIGRATION self-heal for the per-phase-zone roof fix: records saved before
    /// the fix baked a spurious phase-induced split into their stored roofs
    /// (a uniform-phase floor fragmented by a step below seeded a redundant
    /// coplanar same-construction boundary onto the storey above, splitting its
    /// roof). Roofs are regions-as-truth — `resolve_storey` keeps them verbatim
    /// and the phase carve only ever SPLITS — so that stale boundary survives
    /// every reload.
    ///
    /// Rebuild each PHASE-TAGGED storey's roofs from its durable declarative
    /// inputs via [`recompose_roofs`](Self::recompose_roofs) (default footprint →
    /// ceiling steps → apexes → structure roof-splits). Recompose does NOT
    /// reproduce the phase carve, so the stale split is dropped; the re-resolve
    /// at the end of `open_ber` then re-applies the now-clean per-phase carve.
    /// Deliberate structure roof-splits, ceiling steps, and apexes are replayed
    /// from their durable fields, so they all survive.
    ///
    /// GUARD: gated on the storey being phase-tagged (its phase regions were
    /// reconstructed above). A spurious phase-induced roof split can only exist
    /// where there were phase boundaries, so an untagged storey needs no heal —
    /// and `recompose_roofs` is skipped there, keeping its stored roofs
    /// byte-identical (the regions-as-truth reload fixpoint). Recompose is
    /// deterministic, so re-running it on an already-clean phase-tagged storey
    /// reproduces identical geometry — the fixpoint holds there too.
    ///
    /// Requires the per-storey ceiling-step regions to be reconstructed first
    /// (`recompose_roofs` reads `ceiling_step_regions[idx]`).
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
                self.recompose_roofs(idx);
            }
        }
    }

    /// Seed the durable `riser_exposure` table from the persisted resolved
    /// risers — legacy records (and any whose snapshot predates the table) carry
    /// a riser's exposure only on its resolved `WallRegion`, which the first
    /// fresh-id re-resolve would otherwise reset to `Unset`. Idempotent: fills
    /// only gaps, keyed by the riser's base edge.
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
                    .filter(|rw| rw.exposure != ber_model::ber::structural::WallExposure::Unset)
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
    /// preserve — those re-derive from the stack).
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
                    .filter(|r| r.exposure != FloorExposure::Unset)
                    .map(|r| (document::centroid_key(r.centroid()), r.exposure))
                    .collect();
                let ceilings = storey
                    .roofs
                    .regions
                    .iter()
                    .filter(|r| r.exposure != CeilingExposure::Unset)
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
            type WallEntry = (WallConstructionType, ConstructionMakeup, ConstructionPhase);
            #[allow(clippy::type_complexity)]
            let (walls, risers, floors, ceilings): (
                Vec<(manip_plan::SurfaceRef, WallEntry)>,
                Vec<(document::RiserKey, WallEntry)>,
                Vec<(
                    document::CentroidKey,
                    (FloorConstructionType, ConstructionMakeup, ConstructionPhase),
                )>,
                Vec<(
                    document::CentroidKey,
                    (
                        CeilingConstructionType,
                        ConstructionMakeup,
                        ConstructionPhase,
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
                for r in &storey.walls.regions {
                    let Some(c) = record.data.wall_construction(r.construction_id) else {
                        continue;
                    };
                    let ty = c.construction_type;
                    let mk = c.base.construction_makeup.clone();
                    let ph = c.base.construction_phase.clone();
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
                        walls.push((sref, (ty, mk, ph)));
                    } else {
                        let key = document::riser_key(
                            domain::points::Point2f64::new(e0.x, e0.y),
                            domain::points::Point2f64::new(e1.x, e1.y),
                        );
                        risers.push((key, (ty, mk, ph)));
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
                        (ty != FloorConstructionType::default()
                            || mk != ConstructionMakeup::default())
                        .then(|| (document::centroid_key(r.centroid()), (ty, mk, ph)))
                    })
                    .collect();
                let ceilings = storey
                    .roofs
                    .regions
                    .iter()
                    .filter_map(|r| {
                        let c = record.data.ceiling_construction(r.construction_id)?;
                        let ty = c.construction_type;
                        let mk = c.base.construction_makeup.clone();
                        let ph = c.base.construction_phase.clone();
                        (ty != CeilingConstructionType::default()
                            || mk != ConstructionMakeup::default())
                        .then(|| (document::centroid_key(r.centroid()), (ty, mk, ph)))
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
        if version != PARAMETRIC_SNAPSHOT_VERSION {
            return;
        }
        let Ok(mut doc) = serde_json::from_str::<Document>(&json) else {
            return;
        };
        // The snapshot is a whole multi-storey Document, written alongside the
        // resolved record — so it must describe the SAME storey count and have at
        // least one storey. On any mismatch keep the rebuild, which is always
        // consistent with `current_record`. `storey_idx` is now vestigial — the
        // active storey lives in the Document.
        let _ = storey_idx;
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
        let record = self.current_record.as_ref().ok_or("no BER loaded")?;
        crate::engine::checkpoint::save(&record.data, crate::engine::checkpoint::DEFAULT_PATH)
    }

    /// DEV: load the checkpoint file (if present) into the editor and return
    /// whether it succeeded. Mirrors `open_ber`'s reconstruction. Called on
    /// startup when `BER_CHECKPOINT` is set.
    pub fn load_checkpoint(&mut self) -> bool {
        let Some(ber) = crate::engine::checkpoint::load(crate::engine::checkpoint::DEFAULT_PATH)
        else {
            return false;
        };
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
        // Same restart-safe re-derivation as open_ber.
        self.rederive_inherited_seed_pending();
        // Rebuild ALL editing state from Layer 1 (every storey) — the SAME shared
        // sequence as `open_ber`. The checkpoint has no Document snapshot, so this
        // is the only source: it reconstructs phase/step/ceiling/partition/section
        // state AND seeds the durable exposure + construction maps. Seeding here is
        // what fixes the checkpoint "floor goes red after adding a window" bug
        // (§6) — previously the seeds ran only on the DB path, so the first
        // re-resolve reset exposures to `Unset`. It also reconstructs ALL storeys,
        // not just storey 0.
        self.reconstruct_session_state_from_record();
        // Same as open_ber: drop any line/point state from the previous session.
        self.pending_ceiling_step = None;
        self.ceiling_step_mode = false;
        self.pending_roof_point = None;
        self.pending_roof_ridge = None;
        self.pending_roof_rect = None;
        self.roof_point_mode = false;
        self.view = ViewTransform::default();
        self.document.edit_state = EditState::default();
        self.review = None;
        self.compute_next_dimension_prompt();
        // Re-resolve every storey so the seeded maps are stamped onto the geometry
        // now (not on the first edit) — mirrors `open_ber`, and sets the revision
        // bookkeeping so the next no-edit `materialize` is a no-op.
        self.refresh_all_storeys();
        self.history.clear();
        self.redo.clear();
        self.saved_revision = self.revision;
        self.derived_revision = self.revision;
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
        let idx = self.current_storey_idx.unwrap_or(0);
        if self.outline_finalised() {
            // Outline is metric + fully dimensioned → keep its floors/walls,
            // and persist only the living areas that are themselves done (the
            // model owns the finalised-projection; `ensure_*` borrows
            // `out.data`, so resolve the id before the storey borrow below).
            let lists = self.document.finalised_living_area_lists();
            let floor_cid = out.data.ensure_floor_construction(
                Default::default(),
                Default::default(),
                Default::default(),
            );
            if let Some(storey) = out.data.storeys.get_mut(idx) {
                storey.living_areas = FloorRegions::from_vertex_lists(lists, floor_cid);
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
