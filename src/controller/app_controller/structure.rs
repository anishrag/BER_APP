//! Structure-phase surface SELECTION: the app-shell interaction layer over the
//! current storey's resolved regions (walls / roofs / floors). The selected set
//! holds region [`ItemId`]s of the active [`SurfaceKind`]; it's seeded by the
//! Select subtools (`Select all` / `Select remaining` fill it, `Select` starts
//! empty) and refined by tapping or by carving a region to pick.
//!
//! The hit-testing/geometry itself lives one crate down: which surface a tap
//! lands on and which surfaces sit inside a carved region are pure read-only
//! queries over the BER `Storey` data model — see [`Storey::surface_at`],
//! [`Storey::surfaces_in_ring`], [`Storey::surface_ids`] in `datatypes`. This
//! module only sequences those queries, drives the carve through `document`,
//! and holds the transient selected set (app-shell interaction state — the
//! regions live on the BER record, not the editing `Document`).
use super::*;
use datatypes::ber::structural::Storey;
use domain::points::Point2f32;

impl AppController {
    /// The Structure-phase selected region ids — the canvas shades exactly these.
    pub fn structure_selected_ids(&self) -> &HashSet<ItemId> {
        &self.structure_selected
    }

    /// Toggle the surface under a tapped point (world metres) in/out of the
    /// selection. The active [`StructureSelection::kind`] decides which regions
    /// are hit-tested; no-op when no Select subtool is active.
    pub fn toggle_structure_surface_at(&mut self, world_x: f64, world_y: f64, tol: f64) {
        let Some(sel) = self.structure_selection else {
            return;
        };
        let hit = self
            .current_storey()
            .and_then(|s| s.surface_at(sel.kind, world_x, world_y, tol));
        if let Some(id) = hit {
            if !self.structure_selected.insert(id) {
                self.structure_selected.remove(&id);
            }
        }
    }

    /// Clear every selected surface (the "Deselect all" subtool). Keeps the
    /// active Select subtool, so the assessor can keep picking.
    pub fn clear_structure_surface_selection(&mut self) {
        self.structure_selected.clear();
        self.pending_select_cut = None;
    }

    /// A Structure-phase CARVE stroke (world coords): carve the storey the SAME
    /// way a living-area cut does. A straight cut splits it into two candidate
    /// regions to pick from (see [`structure_cut_regions`]); any other stroke
    /// carves a single region whose surfaces are selected immediately.
    pub fn apply_structure_carve_stroke(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        if self.structure_selection.is_none() {
            return;
        }
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        // Carve against the whole outline (no holes) — we only want the region
        // boundary to test surface membership against, not a tiled sub-area.
        self.sync_active();
        match self
            .document
            .carve_or_cut_in_available(&pts, &[], &self.tuning.subarea)
        {
            Ok((_boundary, SubareaOutcome::Cut(a, b))) => {
                self.pending_select_cut = Some([a, b]);
            }
            Ok((_boundary, SubareaOutcome::Area(plan))) => {
                self.select_surfaces_in_region(&plan_ring_f64(&plan));
            }
            Err(e) => log::error!("structure carve_or_cut: {}", e),
        }
    }

    /// The two candidate regions of a pending carve (world coords), for the UI
    /// to highlight while the assessor picks one. Empty when none is pending.
    pub fn structure_cut_regions(&self) -> Vec<Vec<(f64, f64)>> {
        self.pending_select_cut
            .as_ref()
            .map(|regions| {
                regions
                    .iter()
                    .map(|p| {
                        p.get_points()
                            .iter()
                            .map(|pt| (pt.x as f64, pt.y as f64))
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pick which carved region to select from: every surface of the active
    /// kind inside the tapped region joins the selection. A tap outside both
    /// cancels.
    pub fn choose_structure_cut_region(&mut self, world_x: f32, world_y: f32) {
        let Some([a, b]) = self.pending_select_cut.take() else {
            return;
        };
        let chosen = if a.contains_point(world_x, world_y) {
            Some(a)
        } else if b.contains_point(world_x, world_y) {
            Some(b)
        } else {
            None
        };
        if let Some(plan) = chosen {
            self.select_surfaces_in_region(&plan_ring_f64(&plan));
        }
    }

    /// True while a carve is awaiting the assessor's region pick.
    pub fn structure_cut_pending(&self) -> bool {
        self.pending_select_cut.is_some()
    }

    /// Add every surface of the active kind inside `region_ring` (world coords)
    /// to the selection — the carve-pick query, delegated to the storey.
    fn select_surfaces_in_region(&mut self, region_ring: &[Point2f64]) {
        let Some(sel) = self.structure_selection else {
            return;
        };
        let hits = match self.current_storey() {
            Some(storey) => storey.surfaces_in_ring(sel.kind, region_ring),
            None => return,
        };
        self.structure_selected.extend(hits);
    }

    /// Replace the selection with every surface id of `kind` (drives `Select
    /// all` / — as a placeholder until the criteria land — `Select remaining`).
    pub(super) fn select_all_structure_surfaces(&mut self, kind: SurfaceKind) {
        let ids = self
            .current_storey()
            .map(|s| s.surface_ids(kind))
            .unwrap_or_default();
        self.structure_selected = ids.into_iter().collect();
    }

    /// The current storey's resolved regions, if a storey is in scope.
    fn current_storey(&self) -> Option<&Storey> {
        self.current_record
            .as_ref()?
            .data
            .storeys
            .get(self.current_storey_idx?)
    }
}

/// A carved region plan's ring widened to the BER/datatypes `f64` space — the
/// input the `Storey` surface queries expect.
fn plan_ring_f64(plan: &FloorPlan) -> Vec<Point2f64> {
    plan.get_points()
        .iter()
        .map(|p| domain::points::to_f64(*p))
        .collect()
}
