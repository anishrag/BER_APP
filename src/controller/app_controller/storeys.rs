//! Storey management: switching/adding storeys, loading resolved geometry
//! into the editor, and the deferred derive (mark-dirty/materialize) pipeline.
use super::*;

/// The durable, geometry-independent key a Structure-phase SELECTION re-points
/// by across the id-churning re-resolve: a wall's quantized base edge, or a
/// floor/roof area's quantized plan centroid. Walls and floors mint fresh
/// region ids on EVERY resolve (`WallRegions::regenerate` / `FloorRegions`
/// partition), so a selection (or open "Edit attributes" session) keyed by raw
/// id would dangle after any re-derive — silently dropping later edits. This is
/// the selection analogue of the attribute side-tables' `SurfaceRef` /
/// `CentroidKey`. (Roof ids are stable — regions-as-truth — so re-pointing them
/// is a harmless no-op, handled uniformly.)
#[derive(Clone, Copy, PartialEq, Eq)]
enum SurfaceKey {
    Wall((i64, i64, i64, i64)),
    Area((i64, i64)),
}

/// The live region id of `kind` whose durable [`SurfaceKey`] matches `key`, in
/// `storey`'s freshly re-derived geometry — the inverse of
/// [`AppController::structure_surface_key`]. `None` when no region matches (the
/// surface the edit removed).
fn structure_id_for_key(storey: &Storey, kind: SurfaceKind, key: SurfaceKey) -> Option<ItemId> {
    let wall_key = |r: &ber_model::ber::structural::WallRegion| {
        r.base_edge().map(|(e0, e1)| {
            document::riser_key(
                domain::points::Point2f64::new(e0.x, e0.y),
                domain::points::Point2f64::new(e1.x, e1.y),
            )
        })
    };
    match (kind, key) {
        (SurfaceKind::Walls, SurfaceKey::Wall(k)) => storey
            .walls
            .regions
            .iter()
            .find(|r| wall_key(r) == Some(k))
            .map(|r| r.id),
        (SurfaceKind::Floors, SurfaceKey::Area(k)) => storey
            .floors
            .regions
            .iter()
            .find(|r| document::centroid_key(r.centroid()) == k)
            .map(|r| r.id),
        (SurfaceKind::Roofs, SurfaceKey::Area(k)) => storey
            .roofs
            .regions
            .iter()
            .find(|r| document::centroid_key(r.centroid()) == k)
            .map(|r| r.id),
        _ => None,
    }
}

impl AppController {
    /// The durable [`SurfaceKey`] of resolved region `id` of `kind` in storey
    /// `idx`'s CURRENT geometry — `None` if it isn't a live region of that kind.
    /// Captured BEFORE a re-resolve so the Structure selection can re-point to
    /// the region's fresh id afterward.
    fn structure_surface_key(
        &self,
        idx: usize,
        kind: SurfaceKind,
        id: ItemId,
    ) -> Option<SurfaceKey> {
        let storey = self.current_record.as_ref()?.data.storeys.get(idx)?;
        match kind {
            SurfaceKind::Walls => {
                let (e0, e1) = storey
                    .walls
                    .regions
                    .iter()
                    .find(|r| r.id == id)?
                    .base_edge()?;
                Some(SurfaceKey::Wall(document::riser_key(
                    domain::points::Point2f64::new(e0.x, e0.y),
                    domain::points::Point2f64::new(e1.x, e1.y),
                )))
            }
            SurfaceKind::Floors => {
                let r = storey.floors.regions.iter().find(|r| r.id == id)?;
                Some(SurfaceKey::Area(document::centroid_key(r.centroid())))
            }
            SurfaceKind::Roofs => {
                let r = storey.roofs.regions.iter().find(|r| r.id == id)?;
                Some(SurfaceKey::Area(document::centroid_key(r.centroid())))
            }
        }
    }

    /// Like [`structure_surface_key`](Self::structure_surface_key) but DETECTS
    /// the kind of `id` (the selection can mix walls / floors / roofs), returning
    /// the kind alongside its durable key. `None` if `id` isn't a live region.
    fn structure_surface_key_any(
        &self,
        idx: usize,
        id: ItemId,
    ) -> Option<(SurfaceKind, SurfaceKey)> {
        let kind = self
            .current_record
            .as_ref()?
            .data
            .storeys
            .get(idx)?
            .surface_kind_of(id)?;
        Some((kind, self.structure_surface_key(idx, kind, id)?))
    }

    /// Which storey the editor is currently acting on. `Some(i)` = storey `i`
    /// is being edited; `None` = the "All storeys" overview (read-only, no
    /// drawing). Default `Some(0)` on create/open.
    pub fn current_storey_idx(&self) -> Option<usize> {
        self.current_storey_idx
    }

    /// Switch which storey the editor acts on. Commits the live editor state
    /// to its current storey, then loads the target (`None` for the read-only
    /// All-storeys overview). No-op when already on `target`.
    pub fn set_current_storey(&mut self, target: Option<usize>) -> Result<(), String> {
        if self.current_storey_idx == target {
            return Ok(());
        }
        // An in-flight ceiling-step CARVE is an active `StepArea` shape on the
        // OUTGOING storey — abandon it BEFORE the commit syncs the buffer (and
        // before the mode flag resets below, which is what identifies it), so
        // the half-dimensioned area neither persists nor resurfaces as a
        // floor-step pick when switching back.
        self.cancel_pending_ceiling_step();
        // Derive the OUTGOING storey into `current_record` (eager — we're leaving
        // it; `commit_to_ber` syncs its buffer first).
        self.commit_to_ber()?;
        // LOSSLESS switch: the target storey's `StoreyDoc` is kept in place, so we
        // just move the active index — NO rebuild-from-resolved-points (which
        // would drop shared params / collinearity / curves). Its outline, living
        // areas, dimensioning and exposure are exactly as the assessor left them.
        if let Some(i) = target {
            self.document.set_active_storey(i);
        }
        self.current_storey_idx = target;
        // Re-derive the now-active storey on the next render/persist.
        self.mark_dirty()?;
        // Clear only TRANSIENT state — none of it carries across a switch. The
        // storey's SHAPES + dimensioning (above) are preserved.
        self.view = ViewTransform::default();
        self.document.edit_state = EditState::default();
        self.document.pending_arc = None;
        self.review = None;
        self.pending_cut = None;
        self.pending_redraw = None;
        // The new-storey flow (thickness/method/browse) belongs to the storey
        // it was opened on — never carried across a switch. (The inherited-
        // level SEED set stays: it's consumed at each storey's finalisation.)
        self.pending_new_storey = None;
        // Pending phase/step cuts hold the OUTGOING storey's geometry — a pick
        // after the switch would graft it onto the new storey.
        self.pending_phase_cut = None;
        self.pending_step_cut = None;
        // Same for a half-finished ceiling-step line (its coordinate is in the
        // outgoing storey's space); the mode resets like the other area tools.
        self.pending_ceiling_step = None;
        self.ceiling_step_mode = false;
        // And a half-finished roof point / ridge (same coordinate space
        // argument).
        self.pending_roof_point = None;
        self.pending_roof_ridge = None;
        self.pending_roof_rect = None;
        self.roof_point_mode = false;
        self.exit_openings_mode();
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
        self.living_area_mode = false;
        self.exposure_mode = false;
        self.surface_exposure_mode = false;
        self.phase_mode = false;
        self.step_mode = false;
        self.selected_exposure_wall = None;
        self.exposure_sel = None;
        self.exposure_drag = None;
        self.selected_riser = None;
        // Structure-phase selection holds the OUTGOING storey's region ids —
        // drop it (and any pending carve) so nothing carries onto the new storey.
        self.structure_selected.clear();
        self.pending_structure_carves.clear();
        self.pending_structure_cut = None;
        self.structure_split_kind = None;
        match target {
            Some(i) => {
                // Resume on the outline (canonical entry); load its slot into the
                // buffer. The storey's shapes were preserved in place above.
                self.document.active = ActiveShape::Outline;
                self.document.load_active_into_buffer();
                // Rebuild the target storey's phase areas from its tagged floor
                // regions (unless this session already has them).
                if !self.phase_regions.contains_key(&i) {
                    self.reconstruct_phase_regions_for(i);
                }
                // Same for its step areas (from the levelled floor regions).
                if !self.step_regions.contains_key(&i) {
                    self.reconstruct_step_regions_for(i);
                }
                // And its ceiling-step areas (restore the durable list, else
                // migrate an old record by re-deriving from the stepped roofs).
                if !self.ceiling_step_regions.contains_key(&i) {
                    self.load_ceiling_step_regions_for(i);
                }
            }
            None => {
                // "All storeys" overview — no editable shape. The active storey's
                // slots stay intact (synced by `commit_to_ber`); empty only the
                // live buffer so the canvas has nothing to edit on.
                self.document.floor_plan = FloorPlan::default();
                self.document.dimensioning = DimensionState::default();
            }
        }
        self.compute_next_dimension_prompt();
        // Undo/redo is per-editing-session of one storey — don't carry across.
        self.history.clear();
        self.redo.clear();
        Ok(())
    }

    /// Append a fresh empty `Storey` to the current BER and switch the editor
    /// onto it. The previous storey is committed first; the new one starts
    /// with an empty outline, no living areas, no wall sections, and a fresh
    /// dimensioning state (scale calibrates again on its first dimension).
    /// Errors if no BER is loaded.
    pub fn add_storey(&mut self) -> Result<(), String> {
        if self.current_record.is_none() {
            return Err("no BER loaded".into());
        }
        // Save whatever storey is live before leaving it.
        self.commit_to_ber()?;
        let new_idx = {
            let record = self.current_record.as_mut().unwrap();
            let n = record.data.storeys.len();
            record.data.storeys.push(Storey {
                storey_number: n as i32,
                name: format!("Storey {}", n + 1),
                ..Default::default()
            });
            n
        };
        log::debug!(
            "[new-storey] add_storey: appended storey {} (now editing it, zoomed out for drawing room)",
            new_idx
        );
        // Grow the editing container in lock-step: a fresh empty StoreyDoc, made
        // active. (Invariant: document.storeys.len() == current_record storeys.)
        self.document.push_storey();
        self.current_storey_idx = Some(new_idx);
        // Start zoomed out so the previous-storey watermark doesn't fill the
        // canvas — the assessor needs room to draw a footprint that extends
        // BEYOND it (e.g. over a ground-floor garage). `Fit` resets to full-fit.
        self.view = ViewTransform {
            zoom: self.tuning.new_storey.add_storey_zoom,
            ..ViewTransform::default()
        };
        self.document.edit_state = EditState::default();
        self.review = None;
        self.document.pending_arc = None;
        self.pending_cut = None;
        self.pending_redraw = None;
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
        // Fresh, empty editor — scale_set false so the first wall dimensioned on
        // the new storey calibrates its scale (independent of any prior storey).
        self.load_storey_into_editor(None);
        // Open the NEW-STOREY flow (AFTER load_storey_into_editor — it clears
        // the pending flow): the slab thickness is asked FIRST (never on
        // storey 0), and the inherited floor levels seed when this storey's
        // outline finalises (accept or drawn finalisation).
        if new_idx > 0 {
            self.pending_new_storey = Some(PendingNewStorey {
                stage: NewStoreyStage::Thickness,
            });
            self.inherited_seed_pending.insert(new_idx);
        }
        self.history.clear();
        self.redo.clear();
        Ok(())
    }

    /// Rebuild the editor state from a storey's RESOLVED geometry. There is no
    /// parametric snapshot — the DB holds only finalised, metre-scale geometry,
    /// so this is unambiguous. The outline is rebuilt from its floor polygon
    /// (exact points, marked complete + metric so it isn't re-prompted); living
    /// areas are rebuilt non-editable; wall exposure is recovered from the wall
    /// regions. Re-edit fidelity (shared params, collinearity equivalences,
    /// curve arcs) is intentionally not preserved — drawing tools work from the
    /// loaded points.
    pub(super) fn load_storey_into_editor(&mut self, storey: Option<&Storey>) {
        // Shell session state resets here; the model rebuild itself is
        // `StoreyDoc::from_storey` (document) — replacing the whole StoreyDoc
        // covers the buffer, slots, active shape, pending arc and wall sections.
        // Any in-flight phase/step area belongs to the OUTGOING storey — abandon it.
        self.pending_phase_cut = None;
        self.pending_step_cut = None;
        // So does an in-flight new-storey flow (`add_storey` re-opens it AFTER
        // this call when it's the one loading).
        self.pending_new_storey = None;
        // An in-flight ceiling-step line too (its coordinate is that storey's),
        // and an in-flight roof point / ridge for the same reason.
        self.pending_ceiling_step = None;
        self.ceiling_step_mode = false;
        self.pending_roof_point = None;
        self.pending_roof_ridge = None;
        self.pending_roof_rect = None;
        self.roof_point_mode = false;
        self.exit_openings_mode();
        self.living_area_mode = false;
        self.exposure_mode = false;
        // Phase mode is a sub-area tool like the two above; leaving it on across a
        // storey switch routed the new storey's OUTLINE stroke into the phase
        // carve (against an empty footprint). Reset it so the new storey starts in
        // outline mode. Step mode is the same kind of tool — same reset.
        self.phase_mode = false;
        self.step_mode = false;
        self.surface_exposure_mode = false;
        self.selected_exposure_wall = None;
        self.exposure_sel = None;
        self.exposure_drag = None;
        self.selected_riser = None;
        let idx = self.document.active_storey;
        self.document.storeys[idx] = storey.map(StoreyDoc::from_storey).unwrap_or_default();
    }

    /// Snapshot of the BER currently being edited, for the UI to render.
    /// Returns None if no BER has been created/loaded yet.
    pub fn current_ber_clone(&self) -> Option<Ber> {
        self.current_record.as_ref().map(|r| r.data.clone())
    }

    /// True when there's a completed orthogonal shape to act on. The UI uses
    /// this to gate downstream edit tools (Select, Move, Extend, Trim, …)
    /// behind the prerequisite "draw the orthogonal shape first."
    pub fn has_floor_plan(&self) -> bool {
        self.document.floor_plan.get_points().len() >= 3
    }

    /// Project current in-memory state into `self.current_record` and persist.
    /// No-op when no BER is loaded. Called by handler methods that complete a
    /// logical user action (stroke end, wall drag end, etc.) — not from
    /// incremental handlers like `sketch_add_point`.
    /// Derive the active storey's geometry into `current_record` (the
    /// [`document::resolve_storey`] projection + phase stamping).
    /// No revision bookkeeping — the callers (`commit_to_ber` / `materialize`)
    /// own that.
    pub(super) fn refresh_storey(&mut self) {
        if self.current_record.is_none() {
            return;
        }
        // No active storey (the "All storeys" overview tab) → nothing to derive;
        // `current_record` already holds each storey's last-derived geometry.
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        // Capture the selected riser's durable base-edge key BEFORE the
        // re-resolve mints fresh region ids, so the selection can be re-pointed
        // to the riser's new id afterward (riser selection survives the
        // re-derive — its id is otherwise unstable without the amend).
        let wall_key = |this: &Self, id| -> Option<(i64, i64, i64, i64)> {
            let storey = this.current_record.as_ref()?.data.storeys.get(idx)?;
            let r = storey.walls.regions.iter().find(|r| r.id == id)?;
            let (e0, e1) = r.base_edge()?;
            Some(document::riser_key(
                domain::points::Point2f64::new(e0.x, e0.y),
                domain::points::Point2f64::new(e1.x, e1.y),
            ))
        };
        let sel_riser_key = self.selected_riser.and_then(|id| wall_key(self, id));
        // Same for the open elevation's wall — its id is just as unstable.
        let sel_elev_key = self.elevation_wall.and_then(|id| wall_key(self, id));
        // Capture the Structure-phase selection + open "Edit attributes" session
        // by durable key BEFORE the re-resolve mints fresh wall/floor ids, so
        // both re-point to the new ids afterward (same survival mechanism as the
        // riser/elevation selection — across an attribute edit the geometry is
        // identical and only the ids churn, so the selection stays meaningful).
        // Capture each selected id WITH its kind (the selection can span kinds),
        // so each re-points by the right durable key after the re-resolve.
        let sel_struct_keys: Vec<(SurfaceKind, SurfaceKey)> = self
            .structure_selected
            .iter()
            .filter_map(|id| self.structure_surface_key_any(idx, *id))
            .collect();
        // Each edit group already knows its kind (from its construction type).
        let edit_struct_keys: Option<Vec<(SurfaceKind, Vec<SurfaceKey>)>> =
            self.structure_edit.as_ref().map(|e| {
                e.groups
                    .iter()
                    .map(|g| {
                        let kind = g.ty.kind();
                        let keys = g
                            .ids
                            .iter()
                            .filter_map(|id| self.structure_surface_key(idx, kind, *id))
                            .collect();
                        (kind, keys)
                    })
                    .collect()
            });
        // Flush the active buffer into its slot, then ensure the storey slot + its
        // constructions exist — the two `&mut` prerequisites of the otherwise
        // read-only projection (`document::resolve_storey`).
        self.sync_active();
        let regions: &[PhaseRegion] = self
            .phase_regions
            .get(&idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let steps: &[StepRegion] = self
            .step_regions
            .get(&idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let ceiling_steps: &[CeilingStepRegion] = self
            .ceiling_step_regions
            .get(&idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let record = self.current_record.as_mut().unwrap();
        document::ensure_storey_constructions(&mut record.data, idx, regions);
        record.data.storeys[idx] = document::resolve_storey(
            &self.document.storeys[idx],
            &record.data,
            idx,
            regions,
            steps,
            ceiling_steps,
        );
        // Floors are already phase-correct from the partition; this stamps the
        // phase onto the just-rebuilt WALLS by reading those partitioned floors.
        document::stamp_wall_phases(&mut record.data, idx);
        // Compose each wall's ASSIGNED construction (type + make-up, from the
        // parametric store) onto the just-derived phase.
        document::stamp_wall_constructions(&mut record.data, idx, &self.document.storeys[idx]);
        // The phase areas carve the ROOFS too (in resolve, like the floors);
        // stamp the phase onto those carved regions from the partitioned floors.
        document::stamp_ceiling_phases(&mut record.data, idx);
        // Same for FLOOR + CEILING/ROOF area constructions (keyed by centroid).
        document::stamp_floor_constructions(&mut record.data, idx, &self.document.storeys[idx]);
        document::stamp_ceiling_constructions(&mut record.data, idx, &self.document.storeys[idx]);
        // Re-point the riser selection to the (re-derived) riser with the same
        // base edge — keeps the selection valid across the fresh ids.
        if let Some(key) = sel_riser_key {
            let new_id = record.data.storeys.get(idx).and_then(|s| {
                s.walls
                    .regions
                    .iter()
                    .find(|r| {
                        r.base_edge().is_some_and(|(e0, e1)| {
                            document::riser_key(
                                domain::points::Point2f64::new(e0.x, e0.y),
                                domain::points::Point2f64::new(e1.x, e1.y),
                            ) == key
                        })
                    })
                    .map(|r| r.id)
            });
            self.selected_riser = new_id;
        }
        // Re-point the open elevation's wall the same way.
        if let Some(key) = sel_elev_key {
            let new_id = record.data.storeys.get(idx).and_then(|s| {
                s.walls
                    .regions
                    .iter()
                    .find(|r| {
                        r.base_edge().is_some_and(|(e0, e1)| {
                            document::riser_key(
                                domain::points::Point2f64::new(e0.x, e0.y),
                                domain::points::Point2f64::new(e1.x, e1.y),
                            ) == key
                        })
                    })
                    .map(|r| r.id)
            });
            self.elevation_wall = new_id;
        }
        // Re-point the Structure selection + open edit session onto the
        // re-derived regions by durable key. Without this, the stored ids go
        // stale on the very next resolve (walls/floors re-mint every time), so a
        // multi-group "Edit attributes" session would silently drop every edit
        // after the first (the apply itself triggers the re-resolve) and the
        // canvas shading would blank out. Group ids that no longer match (the
        // surface was carved away) drop out.
        let new_sel: std::collections::HashSet<ItemId> = record
            .data
            .storeys
            .get(idx)
            .map(|storey| {
                sel_struct_keys
                    .iter()
                    .filter_map(|(kind, k)| structure_id_for_key(storey, *kind, *k))
                    .collect()
            })
            .unwrap_or_default();
        self.structure_selected = new_sel;
        let new_group_ids: Option<Vec<Vec<ItemId>>> =
            edit_struct_keys.as_ref().and_then(|groups| {
                record.data.storeys.get(idx).map(|storey| {
                    groups
                        .iter()
                        .map(|(kind, group_keys)| {
                            group_keys
                                .iter()
                                .filter_map(|k| structure_id_for_key(storey, *kind, *k))
                                .collect()
                        })
                        .collect()
                })
            });
        if let Some(new_ids) = new_group_ids {
            if let Some(edit) = self.structure_edit.as_mut() {
                for (g, ids) in edit.groups.iter_mut().zip(new_ids) {
                    g.ids = ids;
                }
            }
        }
        // Floor/ceiling exposure auto-derivation, as a FINAL pass over every
        // storey (it reads the neighbours' resolved footprints). `or_insert`
        // keeps any manual override; the pass also STAMPS the derived values onto
        // the resolved regions this same frame (derive-all then stamp-all).
        self.derive_horizontal_exposure_all();
    }

    /// Recompose storey `idx`'s truth roofs from its ceiling EDIT inputs — the
    /// session ±Δ steps, durable dividers, durable apexes, and structure carves
    /// — in the canonical order (default → steps/dividers → apexes → carves).
    /// `resolve_storey` no longer rebuilds the roofs from these; it trusts the
    /// stored `roofs` and only re-fits to the outline. So a ceiling edit updates
    /// its input (session list / durable field), calls this ONCE, then
    /// `materialize` re-fits + re-derives the walls and `reconstruct` refreshes
    /// the session display cache. No-op without a ceiling height.
    pub(super) fn recompose_roofs(&mut self, idx: usize) {
        use ber_model::ber::structural::CeilingRegions;
        let footprint: Vec<Vec<domain::points::Point2f64>> = self
            .document
            .storeys
            .get(idx)
            .map(|s| {
                s.outline
                    .floor_plan
                    .polygons()
                    .into_iter()
                    .map(|ring| ring.into_iter().map(domain::points::to_f64).collect())
                    .collect()
            })
            .unwrap_or_default();
        if !footprint.iter().any(|r| r.len() >= 3) {
            return;
        }
        let session_steps = self
            .ceiling_step_regions
            .get(&idx)
            .cloned()
            .unwrap_or_default();
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let Some(storey) = record.data.storeys.get(idx) else {
            return;
        };
        let Some(height) = storey.ceiling_height_m else {
            return;
        };
        let apexes = storey.roof_apexes.clone();
        let hints = storey.roof_apex_hints.clone();
        let splits = storey.roof_splits.clone();
        let Some(cid) = record.data.ceiling_construction_id(
            Default::default(),
            Default::default(),
            Default::default(),
        ) else {
            return;
        };
        // `session_steps` carries delta-0 dividers too (restored from the durable
        // `ceiling_steps`), so they compose here without a separate pass.
        let areas: Vec<(Vec<domain::points::Point2f64>, f64)> = session_steps
            .iter()
            .map(|r| {
                (
                    r.polygon
                        .iter()
                        .map(|p| domain::points::to_f64(*p))
                        .collect(),
                    r.delta_m,
                )
            })
            .collect();
        let mut roofs = CeilingRegions::default_for_footprint(footprint, height, cid);
        if !areas.is_empty() {
            roofs = roofs.with_z_deltas(&areas);
        }
        if !apexes.is_empty() {
            roofs = roofs.with_apexes_hinted(&apexes, &hints);
        }
        if !splits.is_empty() {
            roofs = roofs.split_faces_coplanar(&splits);
        }
        record.data.storeys[idx].roofs = roofs;
    }

    /// Derive EVERY storey's geometry into `current_record` — the all-storeys
    /// analogue of [`refresh_storey`](Self::refresh_storey), for changes that
    /// touch all storeys at once (e.g. the All-tab default ceiling height).
    /// Each storey resolves from its retained `StoreyDoc`. The live buffer is
    /// deliberately NOT synced: on the All tab it's empty by design (the slots
    /// were committed when the tab was entered), and syncing would clobber the
    /// active storey's outline with that empty buffer.
    pub(super) fn refresh_all_storeys(&mut self) {
        if self.current_record.is_none() {
            return;
        }
        let n = self.document.storeys.len();
        for idx in 0..n {
            let regions: &[PhaseRegion] = self
                .phase_regions
                .get(&idx)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let steps: &[StepRegion] = self
                .step_regions
                .get(&idx)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let ceiling_steps: &[CeilingStepRegion] = self
                .ceiling_step_regions
                .get(&idx)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let record = self.current_record.as_mut().unwrap();
            document::ensure_storey_constructions(&mut record.data, idx, regions);
            record.data.storeys[idx] = document::resolve_storey(
                &self.document.storeys[idx],
                &record.data,
                idx,
                regions,
                steps,
                ceiling_steps,
            );
            document::stamp_wall_phases(&mut record.data, idx);
            // Compose each wall's ASSIGNED construction (type + make-up, from the
            // parametric store) onto the just-derived phase.
            document::stamp_wall_constructions(&mut record.data, idx, &self.document.storeys[idx]);
            // The phase areas carve the ROOFS too (in resolve, like the floors);
            // stamp the phase onto those carved regions from the partitioned floors.
            document::stamp_ceiling_phases(&mut record.data, idx);
            // Same for FLOOR + CEILING/ROOF area constructions (keyed by centroid).
            document::stamp_floor_constructions(&mut record.data, idx, &self.document.storeys[idx]);
            document::stamp_ceiling_constructions(
                &mut record.data,
                idx,
                &self.document.storeys[idx],
            );
        }
        // Floor/ceiling exposure auto-derivation runs as a FINAL pass, after
        // every storey resolved — it reads the neighbours' resolved footprints (a
        // ground floor → Ground, a top ceiling → ExposedRoof, fully-stacked
        // surfaces → party). `or_insert` keeps any manual override, and the pass
        // STAMPS the result straight onto the resolved regions this same frame
        // (derive-all then stamp-all), so the gate settles in one commit.
        self.derive_horizontal_exposure_all();
    }

    /// Auto-derive every storey's floor/ceiling exposure into its `StoreyDoc`
    /// maps, THEN stamp the result onto the resolved regions in the SAME frame.
    /// Each storey reads its neighbours' resolved footprints (ground floor →
    /// Ground, top ceiling → ExposedRoof, fully stacked → party); `or_insert`
    /// never clobbers a manual override. The two-pass shape (derive-all, then
    /// stamp-all) is what makes a lower storey's Party ceiling re-stamp even
    /// though `refresh_storey` only re-resolves the active storey.
    fn derive_horizontal_exposure_all(&mut self) {
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let n = record.data.storeys.len().min(self.document.storeys.len());
        // Pass 1 — DERIVE every storey's doc maps. Reads the neighbours' resolved
        // footprints from `current_record` (immutable) while writing each
        // `StoreyDoc`'s exposure (mutable) — disjoint fields, so split the borrow.
        {
            let ber = &record.data;
            let storeys = &mut self.document.storeys;
            for idx in 0..n {
                document::auto_derive_horizontal_exposure(&mut storeys[idx], ber, idx, n);
            }
        }
        // Pass 2 — STAMP the just-derived maps onto every storey's RESOLVED
        // regions this frame. The resolve's own stamp ran from the PRE-derive
        // maps, so without this the Store-A regions would lag a cycle (the
        // "Surfaces" gate would stay red until an unrelated edit), and a
        // non-active/lower storey would never re-stamp at all.
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        for idx in 0..n {
            document::stamp_horizontal_exposure(
                &self.document.storeys[idx],
                &mut record.data.storeys[idx],
            );
        }
    }

    /// Mark the active storey edited: the in-memory `current_record` geometry is
    /// now stale, and there's an un-saved change. The re-derive is DEFERRED to
    /// [`materialize`](Self::materialize) (before render / persist), so editing —
    /// including mid-drag — never re-derives the whole BER per edit.
    pub(super) fn mark_dirty(&mut self) -> Result<(), String> {
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Bring `current_record`'s geometry up to date with the editing model, if an
    /// edit happened since the last derive. Called before anything READS the
    /// resolved geometry (render, persist). No-op when already current.
    pub fn materialize(&mut self) {
        if self.revision != self.derived_revision {
            self.refresh_storey();
            self.derived_revision = self.revision;
        }
    }

    /// Eagerly derive AND mark dirty (and record that we just derived). Kept for
    /// the few boundary callers that need `current_record` current immediately —
    /// e.g. saving the outgoing storey before `add_storey` switches away.
    pub(super) fn commit_to_ber(&mut self) -> Result<(), String> {
        self.refresh_storey();
        self.revision = self.revision.wrapping_add(1);
        self.derived_revision = self.revision;
        Ok(())
    }

    /// Rebuild the session phase areas for storey `idx` from its persisted floor
    /// regions (each non-`Original` floor region → a phase area). Phase areas are
    /// session-only, so on reload they'd otherwise be empty and the NEXT commit
    /// would re-partition with no areas — wiping the split. Reconstructing here
    /// keeps the partition reproducible and lets a new selection still snap to the
    /// existing zones. No-op when nothing is tagged.
    pub(super) fn reconstruct_phase_regions_for(&mut self, idx: usize) {
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let regions = document::phase_regions_from_floors(&record.data, idx);
        if regions.is_empty() {
            self.phase_regions.remove(&idx);
        } else {
            self.phase_regions.insert(idx, regions);
        }
    }

    /// Rebuild the session step areas for storey `idx` from its persisted floor
    /// regions (each region with non-zero `level_m` → a step area). Step areas
    /// are session-only, so on reload they'd otherwise be empty and the NEXT
    /// commit would re-level with no areas — flattening the floor.
    /// Reconstructing here keeps the split reproducible. No-op when nothing is
    /// stepped.
    pub(super) fn reconstruct_step_regions_for(&mut self, idx: usize) {
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let regions = document::step_regions_from_floors(&record.data, idx);
        if regions.is_empty() {
            self.step_regions.remove(&idx);
        } else {
            self.step_regions.insert(idx, regions);
        }
    }

    /// CANONICALIZE the session ceiling-step list for storey `idx`: clip each
    /// drawn step input to the outline ([`document::canonicalize_ceiling_steps`])
    /// — no geometry inference — and mirror the result into the storey's DURABLE
    /// `ceiling_steps`, so a reload RESTORES it. Called after each ceiling edit.
    /// No-op (entry removed) when nothing is stepped.
    pub(super) fn reconstruct_ceiling_step_regions_for(&mut self, idx: usize) {
        let footprint: Vec<Vec<domain::points::Point2f64>> = self
            .document
            .storeys
            .get(idx)
            .map(|s| {
                s.outline
                    .floor_plan
                    .polygons()
                    .into_iter()
                    .map(|ring| ring.into_iter().map(domain::points::to_f64).collect())
                    .collect()
            })
            .unwrap_or_default();
        let steps = self
            .ceiling_step_regions
            .get(&idx)
            .cloned()
            .unwrap_or_default();
        let canon = document::canonicalize_ceiling_steps(&footprint, &steps);
        if let Some(storey) = self
            .current_record
            .as_mut()
            .and_then(|r| r.data.storeys.get_mut(idx))
        {
            storey.ceiling_steps = canon
                .iter()
                .map(|r| {
                    (
                        r.polygon
                            .iter()
                            .map(|p| domain::points::to_f64(*p))
                            .collect(),
                        r.delta_m,
                    )
                })
                .collect();
        }
        if canon.is_empty() {
            self.ceiling_step_regions.remove(&idx);
        } else {
            self.ceiling_step_regions.insert(idx, canon);
        }
    }

    /// Restore storey `idx`'s session ceiling-step list from its DURABLE
    /// `ceiling_steps` — the reload/switch path, no geometry inference. Returns
    /// `false` (and clears the entry) when the durable field is empty.
    pub(super) fn restore_ceiling_step_regions_for(&mut self, idx: usize) -> bool {
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return false;
        };
        if storey.ceiling_steps.is_empty() {
            self.ceiling_step_regions.remove(&idx);
            return false;
        }
        let regions: Vec<CeilingStepRegion> = storey
            .ceiling_steps
            .iter()
            .map(|(ring, delta)| CeilingStepRegion {
                polygon: ring
                    .iter()
                    .map(|p| Point2f32::new(p.x as f32, p.y as f32))
                    .collect(),
                delta_m: *delta,
            })
            .collect();
        self.ceiling_step_regions.insert(idx, regions);
        true
    }

    /// Reload/switch entry for the session ceiling-step list: RESTORE it from the
    /// durable `ceiling_steps`. Records saved before that field existed have no
    /// steps to restore (accepted — there is no geometry-inference migration).
    pub(super) fn load_ceiling_step_regions_for(&mut self, idx: usize) {
        self.restore_ceiling_step_regions_for(idx);
    }

    /// The previous storey's outline ring as `Point2f32` (world/metre coords),
    /// or `None` when the editor is on storey 0 / there's no previous geometry.
    /// Used both to snap a new storey's stroke onto it and to build a reference
    /// `FloorPlan` for pinning shared walls.
    pub(super) fn previous_storey_ring(&self) -> Option<Vec<Point2f32>> {
        let idx = self.current_storey_idx?;
        if idx == 0 {
            return None;
        }
        let prev = self.current_record.as_ref()?.data.storeys.get(idx - 1)?;
        let ring = prev.floors.footprint_ring()?;
        Some(ring.into_iter().map(to_f32).collect())
    }
}
