//! Structure-phase "Divide wall" — split the selected wall region along its
//! base line into 2–3 parts by dragging two bounds (with snapping) or typing
//! exact distances, the same interaction the exposure sections use. The
//! division commits as durable structure WALL SPLITS (`Storey::wall_splits`,
//! replayed by every resolve), so the pieces are real regions the assessor
//! then attributes separately. The exposure-mode section flow already offered
//! this ergonomics for painting; the Structure phase only had the carve-ring
//! tool — this brings the light 1-D flow to construction attribution.

use ber_model::ber::structural::SurfaceKind;
use domain::points::{Point2f32, Point2f64};
use manip_plan::VertexId;

use super::exposure::SectionHandle;
use super::AppController;

/// The in-flight divide: the parent perimeter wall, the SELECTED region's
/// span on it, and the two movable bounds (all metres from the wall's start
/// vertex). Transient controller state.
#[derive(Debug, Clone, Copy)]
pub(super) struct StructureDivide {
    pub vid: VertexId,
    pub span: (f32, f32),
    pub bounds: (f32, f32),
    pub drag: Option<SectionHandle>,
}

/// UI view of the divide: wall endpoints (plan coords) + the span/bounds, so
/// the canvas draws the handles and the dock shows the piece lengths.
#[derive(Debug, Clone, Copy)]
pub struct StructureDivideView {
    pub wall_a: (f64, f64),
    pub wall_b: (f64, f64),
    pub wall_len: f32,
    pub span: (f32, f32),
    pub bounds: (f32, f32),
}

/// Bounds this close to the span edge don't cut (a handle parked at the end).
const DIVIDE_EDGE_TOL_M: f32 = 0.02;

impl AppController {
    /// Whether "Divide wall" is offered: exactly one WALL region selected in
    /// the Structure phase (same gate as the face Partition button).
    pub fn structure_can_divide(&self) -> bool {
        self.structure_partition_wall().is_some()
    }

    /// Open the divide on the selected wall region: bounds start AT the
    /// span's ends (drag either inward, or type distances, to place cuts).
    pub fn begin_structure_divide(&mut self) {
        self.materialize();
        let Some(vid) = self.structure_partition_wall() else {
            return;
        };
        let Some(span) = self.selected_wall_region_span(vid) else {
            return;
        };
        self.structure_divide = Some(StructureDivide {
            vid,
            span,
            bounds: span,
            drag: None,
        });
    }

    /// The open divide for the UI, or `None`.
    pub fn structure_divide_view(&self) -> Option<StructureDivideView> {
        let d = self.structure_divide?;
        let (a, b, len) = self.wall_geom_of(d.vid)?;
        Some(StructureDivideView {
            wall_a: (a.x as f64, a.y as f64),
            wall_b: (b.x as f64, b.y as f64),
            wall_len: len,
            span: d.span,
            bounds: d.bounds,
        })
    }

    pub fn structure_divide_active(&self) -> bool {
        self.structure_divide.is_some()
    }

    /// Primary press while the divide is open: grab the nearer bound handle
    /// if the press is within `tol` of it (world metres).
    pub fn structure_divide_pointer_down(&mut self, wx: f64, wy: f64, tol: f64) {
        let Some(d) = self.structure_divide else {
            return;
        };
        let Some((a, b, len)) = self.wall_geom_of(d.vid) else {
            return;
        };
        let p = Point2f32::new(wx as f32, wy as f32);
        let at = |m: f32| {
            let t = if len > 1e-6 { m / len } else { 0.0 };
            Point2f32::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
        };
        let dist = |q: Point2f32| ((q.x - p.x).powi(2) + (q.y - p.y).powi(2)).sqrt() as f64;
        let (d0, d1) = (dist(at(d.bounds.0)), dist(at(d.bounds.1)));
        let handle = if d0 <= d1 && d0 <= tol {
            Some(SectionHandle::Start)
        } else if d1 < d0 && d1 <= tol {
            Some(SectionHandle::End)
        } else {
            None
        };
        if let Some(dd) = self.structure_divide.as_mut() {
            dd.drag = handle;
        }
    }

    /// Drag the grabbed bound along the wall, snapping to the span's ends and
    /// to the OTHER wall boundaries on the same base line (existing splits /
    /// phase seams), at the viewport-relative carve tolerance.
    pub fn structure_divide_pointer_drag(&mut self, wx: f64, wy: f64) {
        let Some(d) = self.structure_divide else {
            return;
        };
        let Some(handle) = d.drag else {
            return;
        };
        let Some((a, b, len)) = self.wall_geom_of(d.vid) else {
            return;
        };
        let along = {
            let (ux, uy) = ((b.x - a.x) / len, (b.y - a.y) / len);
            ((wx as f32 - a.x) * ux + (wy as f32 - a.y) * uy).clamp(d.span.0, d.span.1)
        };
        let snap_tol = self.snap_tol_carve();
        let mut along = along;
        for cand in self.wall_boundary_candidates(d.vid) {
            if (cand - along).abs() <= snap_tol {
                along = cand;
                break;
            }
        }
        if let Some(dd) = self.structure_divide.as_mut() {
            match handle {
                SectionHandle::Start => dd.bounds.0 = along.min(dd.bounds.1),
                SectionHandle::End => dd.bounds.1 = along.max(dd.bounds.0),
            }
        }
    }

    pub fn structure_divide_pointer_up(&mut self) {
        if let Some(d) = self.structure_divide.as_mut() {
            d.drag = None;
        }
    }

    /// Type a bound exactly: `first` picks which cut, `value_m` is the
    /// distance from the SELECTED PIECE's left end (how the assessor measures
    /// on site). Clamped into the span.
    pub fn set_structure_divide_bound(&mut self, first: bool, value_m: f32) -> Result<(), String> {
        let Some(d) = self.structure_divide.as_mut() else {
            return Ok(());
        };
        if !value_m.is_finite() || value_m < 0.0 {
            return Err("distance must be a non-negative number".into());
        }
        let along = (d.span.0 + value_m).clamp(d.span.0, d.span.1);
        if first {
            d.bounds.0 = along.min(d.bounds.1);
        } else {
            d.bounds.1 = along.max(d.bounds.0);
        }
        Ok(())
    }

    /// Commit the divide: each bound STRICTLY inside the span becomes a
    /// durable wall split (a cut segment crossing the base edge there); the
    /// resolve replays it into separate regions. One undoable action; the
    /// piece between the bounds is selected afterwards, ready to attribute.
    pub fn apply_structure_divide(&mut self) -> Result<(), String> {
        let Some(d) = self.structure_divide else {
            return Ok(());
        };
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        let Some((a, b, len)) = self.wall_geom_of(d.vid) else {
            return Ok(());
        };
        let interior: Vec<f32> = [d.bounds.0, d.bounds.1]
            .into_iter()
            .filter(|m| *m > d.span.0 + DIVIDE_EDGE_TOL_M && *m < d.span.1 - DIVIDE_EDGE_TOL_M)
            .collect();
        if interior.is_empty() {
            return Err("Drag a handle inward (or type a distance) to place the cut first.".into());
        }
        let undo_snap = self.current_history_snapshot();
        let (ux, uy) = ((b.x - a.x) / len, (b.y - a.y) / len);
        let (nx, ny) = (-uy, ux);
        const CROSS_M: f32 = 0.15;
        let cuts: Vec<[Point2f64; 2]> = interior
            .iter()
            .map(|m| {
                let px = a.x + ux * m;
                let py = a.y + uy * m;
                [
                    Point2f64::new((px + nx * CROSS_M) as f64, (py + ny * CROSS_M) as f64),
                    Point2f64::new((px - nx * CROSS_M) as f64, (py - ny * CROSS_M) as f64),
                ]
            })
            .collect();
        {
            let Some(storey) = self
                .current_record
                .as_mut()
                .and_then(|r| r.data.storeys.get_mut(idx))
            else {
                return Ok(());
            };
            storey.push_structure_split(SurfaceKind::Walls, &[], cuts);
        }
        let mid = (d.bounds.0.max(d.span.0) + d.bounds.1.min(d.span.1)) * 0.5;
        self.structure_divide = None;
        let _ = self.mark_dirty();
        self.materialize();
        // Select the middle piece so the next tap is "Edit attributes".
        self.structure_selected.clear();
        if let Some(id) = self.wall_region_at(d.vid, mid) {
            self.structure_selected.insert(id);
        }
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// Close the divide without cutting anything.
    pub fn cancel_structure_divide(&mut self) {
        self.structure_divide = None;
    }

    // ── shared lookups ─────────────────────────────────────────────────────

    /// The perimeter wall's endpoints + length, by stable vid.
    fn wall_geom_of(&self, vid: VertexId) -> Option<(Point2f32, Point2f32, f32)> {
        self.document
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .find(|(v, _, _)| *v == vid)
            .map(|(_, a, b)| {
                let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                (a, b, len)
            })
    }

    /// The STRUCTURE-selected wall region's along-span on wall `vid`.
    fn selected_wall_region_span(&self, vid: VertexId) -> Option<(f32, f32)> {
        let idx = self.current_storey_idx?;
        let storey = self.current_record.as_ref()?.data.storeys.get(idx)?;
        let sd = self.document.storeys.get(idx)?;
        storey
            .walls
            .regions
            .iter()
            .filter(|r| self.structure_selected.contains(&r.id))
            .find_map(|r| {
                let (v, s, e, _) = sd.recover_wall_region_span(r)?;
                (v == vid).then_some((s, e))
            })
    }

    /// Along-positions of every existing region boundary on wall `vid` (span
    /// starts/ends of all its resolved pieces) — the drag's snap candidates.
    fn wall_boundary_candidates(&self, vid: VertexId) -> Vec<f32> {
        let Some(idx) = self.current_storey_idx else {
            return Vec::new();
        };
        let (Some(storey), Some(sd)) = (
            self.current_record
                .as_ref()
                .and_then(|r| r.data.storeys.get(idx)),
            self.document.storeys.get(idx),
        ) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for r in &storey.walls.regions {
            if let Some((v, s, e, _)) = sd.recover_wall_region_span(r) {
                if v == vid {
                    out.push(s);
                    out.push(e);
                }
            }
        }
        out
    }

    /// The resolved wall region on `vid` whose span contains `along`.
    fn wall_region_at(&self, vid: VertexId, along: f32) -> Option<ber_model::ber::common::ItemId> {
        let idx = self.current_storey_idx?;
        let storey = self.current_record.as_ref()?.data.storeys.get(idx)?;
        let sd = self.document.storeys.get(idx)?;
        storey.walls.regions.iter().find_map(|r| {
            let (v, s, e, _) = sd.recover_wall_region_span(r)?;
            (v == vid && s - 1e-3 <= along && along <= e + 1e-3).then_some(r.id)
        })
    }
}
