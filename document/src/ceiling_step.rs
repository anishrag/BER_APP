//! Ceiling-step logic: session-only areas that raise or lower part of the
//! storey ceiling at commit, entered as an AXIS-ALIGNED LINE + side + delta.
//!
//! A ceiling step is a DURABLE input: its canonical `(ring, delta)` is stored on
//! `Storey::ceiling_steps`, and the resolve composes the ceilings from it (the
//! regions-as-truth ceilings are the geometric truth, the step list is the editable
//! input behind them). The logic over the step regions — fitting/validating the
//! input line, the position prompt for its coordinate, building the half-rect
//! area, and clipping it to the outline — lives here.
//!
//! The line (axis + coordinate + side) is input UX only. The session record is
//! the REGION: after each assign the shell CLIPS each drawn step to the outline
//! ([`canonicalize_ceiling_steps`]) — no geometry inference — and mirrors the
//! result into the durable field, so a reload simply RESTORES it.

use crate::position::reference_walls;
use crate::StoreyDoc;
use ber_model::ber::structural::{CeilingRegions, FloorRegions};
use domain::points::{Point2f32, Point2f64};
use manip_plan::{ParameterAxis, VertexId};

/// Coordinate tolerance (metres) for the "is this wall on the region's extent
/// on `axis`?" test — a wall within a millimetre of the ring's axis-extent
/// min or max bounds the region there. Matches `position::COORD_TOL`.
const REGION_EXTENT_TOL: f32 = 1e-3;

/// Does `coord` (a reference wall's coordinate on `axis`) lie ON the priority
/// `ring`'s extent on `axis` — i.e. equal the ring's bbox min or max on that
/// axis within [`REGION_EXTENT_TOL`]? The region is a rectangle-ish piece
/// bounded by outline walls, so its axis-extent endpoints ARE the coordinates
/// of the two walls that bound it perpendicular to `axis`. Cheap bbox test,
/// no edge walk. `false` for an empty ring.
fn wall_bounds_region_on_axis(coord: f32, ring: &[Point2f64], axis: ParameterAxis) -> bool {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for v in ring {
        let c = match axis {
            ParameterAxis::X => v.x,
            ParameterAxis::Y => v.y,
        };
        lo = lo.min(c);
        hi = hi.max(c);
    }
    if !lo.is_finite() || !hi.is_finite() {
        return false;
    }
    let c = coord as f64;
    (c - lo).abs() < REGION_EXTENT_TOL as f64 || (c - hi).abs() < REGION_EXTENT_TOL as f64
}

/// Edge-coordinate tolerance for [`region_line_coords`] — matches
/// `LINE_CROSS_TOL` (an axis-aligned edge / a dedupe hit).
const LINE_COORD_TOL: f64 = 1e-3;

/// Collect, into `out`, the CONSTANT coordinate on `axis` of every edge of the
/// plan `ring` (point pairs, world metres) PARALLEL to a step line on `axis` —
/// an axis-aligned edge (a vertical edge → its `x` for axis X; a horizontal edge
/// → its `y` for axis Y). The ring is treated as CLOSED (the last→first edge is
/// included, so an unclosed ring still contributes every edge); a geo ring whose
/// last point repeats the first yields a degenerate wrap edge that the run test
/// skips. Deduped within tolerance.
fn push_axis_parallel_coords(ring: &[(f64, f64)], axis: ParameterAxis, out: &mut Vec<f32>) {
    let n = ring.len();
    if n < 2 {
        return;
    }
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let coord = match axis {
            ParameterAxis::X => ((a.0 - b.0).abs() < LINE_COORD_TOL
                && (a.1 - b.1).abs() > LINE_COORD_TOL)
                .then_some(a.0),
            ParameterAxis::Y => ((a.1 - b.1).abs() < LINE_COORD_TOL
                && (a.0 - b.0).abs() > LINE_COORD_TOL)
                .then_some(a.1),
        };
        if let Some(c) = coord {
            let c = c as f32;
            if !out.iter().any(|rc| (rc - c).abs() < LINE_COORD_TOL as f32) {
                out.push(c);
            }
        }
    }
}

/// Reference coordinates (on `axis`) of every FLOOR-region edge parallel to a
/// step line on `axis` ([`push_axis_parallel_coords`]). Internal step boundaries
/// AND the region outline alike, deduped. The shell gathers these from THIS
/// storey's resolved floors and the storey BELOW's, then feeds them to
/// [`StoreyDoc::snap_ceiling_step_line`] so a drawn step line also snaps to
/// existing region lines — not only the current outline walls.
pub fn region_line_coords(regions: &FloorRegions, axis: ParameterAxis) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    for region in &regions.regions {
        let ring: Vec<(f64, f64)> = region
            .polygon
            .exterior()
            .coords()
            .map(|c| (c.x, c.y))
            .collect();
        push_axis_parallel_coords(&ring, axis, &mut out);
    }
    out
}

/// Reference coordinates (on `axis`) of every CEILING-region edge parallel to a
/// step line on `axis` — the analogue of [`region_line_coords`] for the roof
/// (ceiling-step) regions, so a drawn step line also snaps to existing ceiling
/// steps in THIS storey and the one below. Ceiling rings are 3D and may be
/// unclosed; only the plan `(x, y)` matters and the wrap edge is included.
pub fn ceiling_line_coords(regions: &CeilingRegions, axis: ParameterAxis) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    for region in &regions.regions {
        let ring: Vec<(f64, f64)> = region.polygon.exterior.iter().map(|v| (v.x, v.y)).collect();
        push_axis_parallel_coords(&ring, axis, &mut out);
    }
    out
}

/// Which side of the step line the delta applies to: coordinates on the
/// line's axis LESS or GREATER than the line's coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineSide {
    Less,
    Greater,
}

/// A session-only ceiling-step area: a world-space (metre) ring + the z DELTA
/// inside it, RELATIVE to the storey's default ceiling height (+ raised,
/// − lowered). Overlapping areas COMPOSE (deltas sum) when applied in order
/// by `CeilingRegions::with_z_deltas`; after canonicalization the areas are
/// the exact derived pieces, which never overlap.
#[derive(Debug, Clone)]
pub struct CeilingStepRegion {
    pub polygon: Vec<Point2f32>,
    pub delta_m: f64,
}

impl CeilingStepRegion {
    /// True when this region is a SAME-HEIGHT DIVIDER — a flat area at the
    /// default ceiling height (no z delta), drawn only to bound a later ridge to
    /// its own area. The `1e-3` band is the shared "is this delta zero?" test.
    pub fn is_divider(&self) -> bool {
        self.delta_m.abs() < 1e-3
    }
}

/// Clip each ceiling-step input ring to the storey `footprint` — the canonical
/// session list, with NO geometry inference. Ceiling steps are durable INPUTS
/// now (`Storey::ceiling_steps`): a step is the user's drawn area bounded to the
/// outline, and the apexes compose on top in the resolve, so a step's canonical
/// area is just its own clipped ring (delta-0 dividers included — they survive
/// here, no separate durable field). Empties / sub-mm² slivers dropped; deltas
/// preserved. Replaces the old reconstruct-from-ceilings canonicalization.
pub fn canonicalize_ceiling_steps(
    footprint: &[Vec<Point2f64>],
    steps: &[CeilingStepRegion],
) -> Vec<CeilingStepRegion> {
    use geo::bool_ops::BooleanOps;
    use geo::{Area, LineString, MultiPolygon, Polygon};
    let to_poly = |ring: Vec<(f64, f64)>| Polygon::new(LineString::from(ring), vec![]);
    let fp_polys: Vec<Polygon<f64>> = footprint
        .iter()
        .filter(|r| r.len() >= 3)
        .map(|r| to_poly(r.iter().map(|p| (p.x, p.y)).collect()))
        .collect();
    if fp_polys.is_empty() {
        return Vec::new();
    }
    let fp = MultiPolygon(fp_polys);
    let mut out = Vec::new();
    for step in steps {
        if step.polygon.len() < 3 {
            continue;
        }
        let sp = to_poly(
            step.polygon
                .iter()
                .map(|p| (p.x as f64, p.y as f64))
                .collect(),
        );
        for piece in sp.intersection(&fp).0.iter() {
            if piece.unsigned_area() < 1e-6 {
                continue;
            }
            let mut ring: Vec<Point2f32> = piece
                .exterior()
                .coords()
                .map(|c| Point2f32::new(c.x as f32, c.y as f32))
                .collect();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            if ring.len() >= 3 {
                out.push(CeilingStepRegion {
                    polygon: ring,
                    delta_m: step.delta_m,
                });
            }
        }
    }
    out
}

/// The "how far is the step line from an outline wall?" question — the line
/// analogue of [`PositionPrompt`](crate::PositionPrompt), shell-resolved
/// (there's no shape or parameter behind a line, so it does NOT enter the
/// `recompute_position_prompt` loop). The shell holds it in its pending-step
/// state and resolves it via [`StoreyDoc::resolve_ceiling_line_position`].
#[derive(Debug, Clone)]
pub struct LinePositionPrompt {
    /// The axis the line's POSITION varies on (X for a vertical line).
    pub axis: ParameterAxis,
    /// Cyclable outline reference walls, nearest first. Never empty
    /// (a line with no parallel reference auto-resolves instead).
    pub options: Vec<LinePositionOption>,
    pub current_idx: usize,
}

/// One candidate measurement for the line's coordinate. The reference wall is
/// in the storey OUTLINE plan (`StoreyDoc::outline.floor_plan`) — there is no
/// area plan; the measured object is the line itself.
#[derive(Debug, Clone, Copy)]
pub struct LinePositionOption {
    /// Start vertex of the reference wall, in the storey OUTLINE plan.
    pub outline_wall_vid: VertexId,
    /// The outline wall's coordinate on the prompt's `axis`.
    pub reference_value: f32,
    /// `signum(line − reference)` at build time — which side of the reference
    /// the line was drawn on, preserved when the entered distance is applied.
    pub sign: f32,
    /// `|line − reference|` — the approximate drawn distance (prefill value).
    pub current_distance: f32,
}

impl LinePositionPrompt {
    /// Advance to the next reference option (wraps). No-op when empty.
    pub fn cycle(&mut self) {
        if !self.options.is_empty() {
            self.current_idx = (self.current_idx + 1) % self.options.len();
        }
    }
}

/// User-facing rejection for a line that doesn't cross the storey footprint.
/// Kept stable — tests (and the shell) match on it.
const LINE_OFF_FOOTPRINT_MSG: &str =
    "The step line must cross the storey footprint — pick a position inside the floor plan.";

/// Coordinate tolerance for the crosses-the-footprint test, in metres: a line
/// within a millimetre of the bounding box edge steps nothing.
const LINE_CROSS_TOL: f32 = 1e-3;

/// Fixed FIT slop (metres) for the roof-mode stroke fits: how far a fitted
/// rectangle side ([`StoreyDoc::fit_roof_rect`]) or ridge endpoint
/// ([`StoreyDoc::fit_roof_ridge_line`]) may sit past a footprint wall and still
/// be ACCEPTED (a stroke ending on/just past the wall survives to its end snap).
/// This is an accept/reject slop, NOT a snap-to-reference radius — the drawn
/// LINE/POINT SNAPS ([`snap_ceiling_step_line`](StoreyDoc::snap_ceiling_step_line),
/// [`snap_roof_ridge_coord`](StoreyDoc::snap_roof_ridge_coord),
/// [`snap_roof_point_coord`](StoreyDoc::snap_roof_point_coord)) take a
/// viewport-relative `tol` argument from the shell instead. Kept fixed and
/// crate-visible for the fit tests and `ceiling_point.rs`.
pub(crate) const CEILING_LINE_SNAP_TOL_M: f32 = 0.2;

// ── Ceiling-step operations ─────────────────────────────────────────────────
// Pure queries over the storey outline. The shell (`AppController`) owns the
// pending-step state machine, the region storage, and canonicalization.
impl StoreyDoc {
    /// Fit a near-straight `stroke` (world/metre space) to an axis-aligned
    /// step line: [`manip_plan::fit_axis_line`] + the crosses-the-footprint
    /// validation. Returns the axis the line's position varies on (X for a
    /// vertical line) and its coordinate. `Err` is user-facing.
    pub fn fit_ceiling_step_line(
        &self,
        stroke: &[Point2f32],
    ) -> Result<(ParameterAxis, f32), String> {
        let (axis, coord) = manip_plan::fit_axis_line(stroke).ok_or_else(|| {
            "That stroke isn't a straight line — draw one straight stroke across the ceiling."
                .to_string()
        })?;
        self.validate_ceiling_step_line(axis, coord)?;
        Ok((axis, coord))
    }

    /// The crosses-the-footprint validation: the line's coordinate must lie
    /// strictly inside the OUTLINE's bounding box on its axis (a line on or
    /// beyond the edge steps nothing). Bbox-level on purpose — the half-rect
    /// area always intersects the footprint when the coordinate is inside the
    /// bbox, and the resolve clip handles the rest. `Err` is user-facing.
    pub fn validate_ceiling_step_line(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
    ) -> Result<(), String> {
        let Some(((min_x, min_y), (max_x, max_y))) = self.outline.floor_plan.bounding_box() else {
            return Err(LINE_OFF_FOOTPRINT_MSG.to_string());
        };
        let (lo, hi) = match axis {
            ParameterAxis::X => (min_x, max_x),
            ParameterAxis::Y => (min_y, max_y),
        };
        if coord_m <= lo + LINE_CROSS_TOL || coord_m >= hi - LINE_CROSS_TOL {
            return Err(LINE_OFF_FOOTPRINT_MSG.to_string());
        }
        Ok(())
    }

    /// Snap a drawn/tapped step line onto the nearest parallel outline wall
    /// within `tol` (metres, viewport-relative). The candidates are the SAME deduped
    /// reference walls the position prompt offers ([`reference_walls`]) — so a
    /// snapped line IS at a reference wall's coordinate, exact by
    /// construction, which is exactly why the shell SKIPS the position prompt
    /// for it: there is no "how far from the wall?" question left to ask.
    /// Walls at the outline bbox EXTREMES on the axis are EXCLUDED — a line
    /// snapped onto the boundary leaves one side of it empty (a step that
    /// steps nothing), and
    /// [`validate_ceiling_step_line`](Self::validate_ceiling_step_line) would
    /// reject the coordinate anyway.
    ///
    /// Candidates are the non-extreme parallel OUTLINE walls (each carrying its
    /// vertex id) PLUS `extra_coords` — additional reference coordinates on
    /// `axis` the shell gathered from existing region lines (this storey's floor
    /// boundaries and the storey below's; see [`region_line_coords`]). Nearest
    /// within `tol` wins.
    ///
    /// Returns `(snapped coordinate, Some(outline wall vid))` when the winner is
    /// an outline wall, or `(coordinate, None)` when it is one of the extra
    /// reference lines (no outline vertex). `None` when nothing is within `tol`.
    /// Pure — the shell decides when to call, supplies the extra
    /// references, computes the viewport-relative `tol` (metres), and remembers
    /// the drawn coordinate for un-snapping.
    pub fn snap_ceiling_step_line(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
        extra_coords: &[f32],
        tol: f32,
    ) -> Option<(f32, Option<VertexId>)> {
        let ((min_x, min_y), (max_x, max_y)) = self.outline.floor_plan.bounding_box()?;
        let (lo, hi) = match axis {
            ParameterAxis::X => (min_x, max_x),
            ParameterAxis::Y => (min_y, max_y),
        };
        // A line snapped onto the outline bbox EXTREME steps nothing, and
        // `validate_ceiling_step_line` would reject it — exclude those, whether
        // they come from an outline wall or an extra reference line.
        let not_extreme =
            |c: f32| (c - lo).abs() > LINE_CROSS_TOL && (c - hi).abs() > LINE_CROSS_TOL;
        let outline = reference_walls(&self.outline.floor_plan, axis)
            .into_iter()
            .filter(|(_, c)| not_extreme(*c))
            .map(|(vid, c)| (c, Some(vid)));
        let extras = extra_coords
            .iter()
            .copied()
            .filter(|c| not_extreme(*c))
            .map(|c| (c, None));
        outline
            .chain(extras)
            .map(|(c, vid)| (c, vid, (c - coord_m).abs()))
            .filter(|(_, _, d)| *d <= tol)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(c, vid, _)| (c, vid))
    }

    /// The same-height DIVIDER cut segments of `steps` (the session
    /// ceiling-step regions the shell owns) as world-metre snap segments —
    /// each `is_divider()` region's interior split edge against the storey
    /// outline's bounding box ([`manip_plan::interior_split_edge`], the same
    /// geometry the canvas draws as the faint dashed divider). These are the
    /// line references a sub-area stroke (the porch carve) pre-snaps onto so
    /// a drawn edge lands exactly ON the "Divide ceiling" line. Real
    /// height-step (non-divider) regions contribute nothing here — their
    /// RISER base edges are already the shell's step-line references. Empty
    /// when the outline is empty or no divider yields an interior edge.
    pub fn ceiling_divider_segments(&self, steps: &[CeilingStepRegion]) -> Vec<[Point2f32; 2]> {
        let Some(((ox0, oy0), (ox1, oy1))) = self.outline.floor_plan.bounding_box() else {
            return Vec::new();
        };
        let bbox = ((ox0 as f64, oy0 as f64), (ox1 as f64, oy1 as f64));
        steps
            .iter()
            .filter(|r| r.is_divider())
            .filter_map(|r| {
                let ring: Vec<(f64, f64)> =
                    r.polygon.iter().map(|p| (p.x as f64, p.y as f64)).collect();
                manip_plan::interior_split_edge(&ring, bbox)
            })
            .map(|((ax, ay), (bx, by))| {
                [
                    Point2f32::new(ax as f32, ay as f32),
                    Point2f32::new(bx as f32, by as f32),
                ]
            })
            .collect()
    }

    /// EVERY edge of EVERY ceiling-step region in `steps`, as world-metre
    /// reference segments — the snap/pin references for a stroke drawn while
    /// ceiling sections already exist.
    ///
    /// Ceiling sections are NOT mutually exclusive: a raised area may sit
    /// wholly inside an existing section (a divider, or another raised area) —
    /// the deltas compose in `CeilingRegions::with_z_deltas`, and
    /// [`canonicalize_ceiling_steps`] clips each ring only to the footprint,
    /// never against its siblings. So the carve runs against the WHOLE outline
    /// and the existing sections bound the stroke by SNAPPING it, not by being
    /// subtracted out of the drawable region (which would make the inside of a
    /// section undrawable — field report: a raised rectangle inside an
    /// already-divided area silently did nothing).
    ///
    /// This is the pairing that keeps that safe. The carve levels a drawn wall
    /// onto the nearest parallel, overlapping reference within tolerance, and
    /// the same set drives [`StoreyDoc::pinned_slot_for`] — so a wall laid on
    /// an existing section's edge is pinned pre-known, and any wall that did
    /// NOT land on one stays free and raises its dimension prompt. Without
    /// these references a stroke inside a section would meet no boundary at
    /// all and every edge would fall to the drawn approximation.
    ///
    /// Contrast [`ceiling_divider_segments`](Self::ceiling_divider_segments),
    /// which returns ONE bbox-spanning line per same-height divider (the
    /// dashed line the canvas draws, for the porch carve to pre-snap onto).
    /// This returns the regions' own edges, dividers and height-steps alike.
    /// Degenerate rings (< 3 points) and zero-length edges contribute nothing.
    pub fn ceiling_section_edges(&self, steps: &[CeilingStepRegion]) -> Vec<[Point2f32; 2]> {
        let mut out = Vec::new();
        for step in steps {
            let ring = &step.polygon;
            let n = ring.len();
            if n < 3 {
                continue;
            }
            for i in 0..n {
                let (a, b) = (ring[i], ring[(i + 1) % n]);
                // A repeated point carries no direction — it would level a drawn
                // wall onto a coordinate with no extent to overlap.
                if (a.x - b.x).abs() > f32::EPSILON || (a.y - b.y).abs() > f32::EPSILON {
                    out.push([a, b]);
                }
            }
        }
        out
    }

    /// Snap a ridge coordinate — the CROSS coordinate (on `axis`) or an EXTENT
    /// END (on the ALONG axis `axis.other()`, callers pass that axis directly) —
    /// onto the nearest parallel reference within `tol` (metres, viewport-relative),
    /// INCLUDING walls at the outline bbox extremes: a ridge along an outer wall
    /// is a valid mono-pitch and a ridge end on the boundary is a gable (the one
    /// deliberate difference from
    /// [`snap_ceiling_step_line`](Self::snap_ceiling_step_line)). Candidates are
    /// the outline walls PLUS `extra_coords` — internal ceiling/floor region
    /// boundaries the shell gathers (see the shell's `region_snap_references`), so
    /// a ridge drawn near an area EDGE snaps onto it. Returns `(snapped
    /// coordinate, Some(outline wall vid))` for an outline hit or `(coordinate,
    /// None)` for an internal reference line; nearest wins, `None` beyond
    /// tolerance.
    pub fn snap_roof_ridge_coord(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
        extra_coords: &[f32],
        tol: f32,
    ) -> Option<(f32, Option<VertexId>)> {
        self.snap_line_to_reference_wall(axis, coord_m, true, extra_coords, tol)
    }

    /// Snap a roof-POINT coordinate on `axis` to the nearest parallel outline
    /// wall, INCLUDING the bbox-extreme (outermost) walls — so a point tapped
    /// near a CORNER snaps to it on BOTH axes (the ceiling-step snap excludes the
    /// extremes, which is why a corner tap previously snapped only one axis).
    /// Outline references only (no extra internal lines), so a hit always carries
    /// its vertex id.
    pub fn snap_roof_point_coord(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
        tol: f32,
    ) -> Option<(f32, VertexId)> {
        self.snap_line_to_reference_wall(axis, coord_m, true, &[], tol)
            .map(|(c, vid)| (c, vid.expect("outline-only snap yields a vertex id")))
    }

    /// The shared body of the line snaps: the deduped parallel outline reference
    /// walls on `axis` (each `Some(vid)`), optionally excluding the outline bbox
    /// extremes, PLUS `extra_coords` — internal reference lines (ceiling/floor
    /// region boundaries) that carry no outline vertex (`None`) and are never at
    /// a bbox extreme, so always eligible. Nearest within `tol` wins.
    fn snap_line_to_reference_wall(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
        include_extremes: bool,
        extra_coords: &[f32],
        tol: f32,
    ) -> Option<(f32, Option<VertexId>)> {
        let ((min_x, min_y), (max_x, max_y)) = self.outline.floor_plan.bounding_box()?;
        let (lo, hi) = match axis {
            ParameterAxis::X => (min_x, max_x),
            ParameterAxis::Y => (min_y, max_y),
        };
        let outline = reference_walls(&self.outline.floor_plan, axis)
            .into_iter()
            .filter(|(_, c)| {
                include_extremes
                    || ((c - lo).abs() > LINE_CROSS_TOL && (c - hi).abs() > LINE_CROSS_TOL)
            })
            .map(|(vid, c)| (c, Some(vid)));
        let extras = extra_coords.iter().copied().map(|c| (c, None));
        outline
            .chain(extras)
            .map(|(c, vid)| (c, vid, (c - coord_m).abs()))
            .filter(|(_, _, d)| *d <= tol)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(c, vid, _)| (c, vid))
    }

    /// Build the position prompt for a drawn step line: the OUTLINE walls
    /// PARALLEL to it (the walls whose perpendicular axis is `axis`, deduped
    /// by coordinate), nearest first. `None` when there is no parallel
    /// reference — the approximate-stroke principle's LOGGED auto-resolve:
    /// the caller accepts the drawn coordinate.
    ///
    /// When `prefer_region` is `Some(ring)` (a resolved ceiling region's plan
    /// ring, world metres), reference walls whose coordinate lies ON that
    /// region's extent on `axis` (the ring's bbox min/max within `COORD_TOL`)
    /// sort FIRST — nearest among them is the default (`current_idx = 0`) —
    /// then the rest by `current_distance`, so a ridge's default reference
    /// wall bounds its OWN area rather than the globally nearest wall in
    /// another arm. The option SET is UNCHANGED (cycle-button semantics
    /// preserved); only the sort + `current_idx` change. `None`, or a ring no
    /// option lies on, falls back to pure global-nearest — the original
    /// behaviour, unchanged for callers that pass `None`.
    pub fn ceiling_line_position_prompt(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
        prefer_region: Option<&[Point2f64]>,
    ) -> Option<LinePositionPrompt> {
        let refs = reference_walls(&self.outline.floor_plan, axis);
        if refs.is_empty() {
            log::info!(
                "ceiling-step line position on {axis:?} auto-resolved: no parallel outline \
                 wall to measure from — drawn coordinate {coord_m:.3} accepted"
            );
            return None;
        }
        let mut options: Vec<LinePositionOption> = refs
            .into_iter()
            .map(|(outline_wall_vid, reference_value)| LinePositionOption {
                outline_wall_vid,
                reference_value,
                sign: (coord_m - reference_value).signum(),
                current_distance: (coord_m - reference_value).abs(),
            })
            .collect();
        // Sort by distance; when a priority ring is given, walls bounding that
        // region on `axis` sort ahead of all others (their relative order is
        // still by distance). `sort_by` is stable, so within each tier the
        // distance order is preserved.
        options.sort_by(|a, b| {
            let a_bounds = prefer_region
                .is_some_and(|ring| wall_bounds_region_on_axis(a.reference_value, ring, axis));
            let b_bounds = prefer_region
                .is_some_and(|ring| wall_bounds_region_on_axis(b.reference_value, ring, axis));
            // Bounding walls first (false < true, so reverse the bool),
            // then nearest first.
            b_bounds
                .cmp(&a_bounds)
                .then_with(|| a.current_distance.total_cmp(&b.current_distance))
        });
        Some(LinePositionPrompt {
            axis,
            options,
            current_idx: 0,
        })
    }

    /// Resolve the prompt with an entered distance: the line's exact
    /// coordinate is `reference + sign · value` (the side the line was drawn
    /// on is preserved). Rejects a non-finite or negative distance and a
    /// result that no longer crosses the footprint. `Err` is user-facing;
    /// nothing is mutated — the shell carries the returned coordinate forward.
    pub fn resolve_ceiling_line_position(
        &self,
        prompt: &LinePositionPrompt,
        value: f32,
    ) -> Result<f32, String> {
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "distance must be a non-negative number (got {})",
                value
            ));
        }
        let opt = prompt
            .options
            .get(prompt.current_idx)
            .ok_or_else(|| "position prompt has no selected option".to_string())?;
        let coord = opt.reference_value + opt.sign * value;
        self.validate_ceiling_step_line(prompt.axis, coord)?;
        Ok(coord)
    }

    /// The step region for a resolved line: a cheap bounding-box HALF-RECT on
    /// the chosen side, grown by a margin so it safely overhangs the footprint
    /// — the resolve projection clips it (`with_z_deltas`), and the shell
    /// canonicalizes the session list to the exact derived pieces right after.
    /// `None` when the outline is empty or the half-rect is degenerate (the
    /// coordinate is outside the bbox on that side — `validate` rejects that
    /// earlier).
    pub fn ceiling_step_region(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
        side: LineSide,
        delta_m: f64,
    ) -> Option<CeilingStepRegion> {
        let ((min_x, min_y), (max_x, max_y)) = self.outline.floor_plan.bounding_box()?;
        let margin = ((max_x - min_x).max(max_y - min_y) * 0.1).max(1.0);
        let (x0, x1, y0, y1) = match (axis, side) {
            (ParameterAxis::X, LineSide::Less) => {
                (min_x - margin, coord_m, min_y - margin, max_y + margin)
            }
            (ParameterAxis::X, LineSide::Greater) => {
                (coord_m, max_x + margin, min_y - margin, max_y + margin)
            }
            (ParameterAxis::Y, LineSide::Less) => {
                (min_x - margin, max_x + margin, min_y - margin, coord_m)
            }
            (ParameterAxis::Y, LineSide::Greater) => {
                (min_x - margin, max_x + margin, coord_m, max_y + margin)
            }
        };
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(CeilingStepRegion {
            polygon: vec![
                Point2f32::new(x0, y0),
                Point2f32::new(x1, y0),
                Point2f32::new(x1, y1),
                Point2f32::new(x0, y1),
            ],
            delta_m,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DimensionState, ShapeSlot};
    use ber_model::ber::structural::Storey;
    use manip_plan::FloorPlan;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A world-metre rectangle ring x ∈ [x0, x1], y ∈ [0, 6] — the ceiling test
    /// footprint / step half-rect helper.
    fn square64(x0: f64, x1: f64) -> Vec<Point2f64> {
        vec![
            Point2f64::new(x0, 0.0),
            Point2f64::new(x1, 0.0),
            Point2f64::new(x1, 6.0),
            Point2f64::new(x0, 6.0),
        ]
    }

    /// A StoreyDoc with a finalised 10×6 metre outline in its slot.
    fn storey_10x6() -> StoreyDoc {
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 6.), p(0., 6.)]).unwrap();
        let known = outline.all_active_params();
        let mut doc = StoreyDoc::default();
        doc.outline = ShapeSlot {
            floor_plan: outline,
            dimensioning: DimensionState {
                scale_set: true,
                known_params: known,
                ..Default::default()
            },
        };
        doc.load_active_into_buffer();
        doc
    }

    /// A near-vertical stroke fits to a line whose position varies on X; one
    /// outside the footprint is rejected with the stable user-facing message.
    #[test]
    fn fit_accepts_a_crossing_stroke_and_rejects_one_outside() {
        let doc = storey_10x6();
        let stroke = vec![p(4.02, 0.5), p(3.97, 2.0), p(4.03, 4.0), p(3.99, 5.5)];
        let (axis, coord) = doc.fit_ceiling_step_line(&stroke).expect("crossing line");
        assert_eq!(axis, ParameterAxis::X);
        assert!((coord - 4.0).abs() < 0.05);

        // Outside the footprint (x ≈ 14) → rejected, stable message.
        let outside = vec![p(14.0, 0.5), p(14.0, 5.5)];
        let err = doc.fit_ceiling_step_line(&outside).unwrap_err();
        assert_eq!(err, LINE_OFF_FOOTPRINT_MSG);
        // Not line-like at all → the fit message, not the footprint one.
        let blob = vec![p(2., 2.), p(4., 2.), p(4., 4.), p(2., 4.), p(2., 2.1)];
        assert!(doc
            .fit_ceiling_step_line(&blob)
            .unwrap_err()
            .contains("straight"));
    }

    /// validate: strictly inside the bbox on the line's axis; the edges (and
    /// an empty outline) are out.
    #[test]
    fn validate_requires_the_line_inside_the_bbox() {
        let doc = storey_10x6();
        assert!(doc
            .validate_ceiling_step_line(ParameterAxis::X, 5.0)
            .is_ok());
        assert!(doc
            .validate_ceiling_step_line(ParameterAxis::Y, 3.0)
            .is_ok());
        for (axis, coord) in [
            (ParameterAxis::X, 0.0),
            (ParameterAxis::X, 10.0),
            (ParameterAxis::X, -2.0),
            (ParameterAxis::Y, 6.0),
            (ParameterAxis::Y, 9.0),
        ] {
            assert_eq!(
                doc.validate_ceiling_step_line(axis, coord).unwrap_err(),
                LINE_OFF_FOOTPRINT_MSG,
                "{axis:?} {coord} must be rejected"
            );
        }
        let empty = StoreyDoc::default();
        assert!(empty
            .validate_ceiling_step_line(ParameterAxis::X, 5.0)
            .is_err());
    }

    /// The position prompt lists the PARALLEL outline walls nearest first; an
    /// outline with no parallel reference yields `None` (logged auto-resolve).
    #[test]
    fn position_prompt_lists_parallel_walls_nearest_first() {
        let doc = storey_10x6();
        // Vertical line at x = 4: references are the vertical outline walls
        // x = 0 (4 m away) and x = 10 (6 m away).
        let prompt = doc
            .ceiling_line_position_prompt(ParameterAxis::X, 4.0, None)
            .expect("two parallel walls");
        assert_eq!(prompt.axis, ParameterAxis::X);
        assert_eq!(prompt.options.len(), 2);
        assert!((prompt.options[0].reference_value - 0.0).abs() < 1e-4);
        assert!((prompt.options[0].current_distance - 4.0).abs() < 1e-4);
        assert!(prompt.options[0].sign > 0.0);
        assert!((prompt.options[1].reference_value - 10.0).abs() < 1e-4);
        assert!((prompt.options[1].current_distance - 6.0).abs() < 1e-4);
        assert!(prompt.options[1].sign < 0.0);

        // Cycle wraps through the options and back.
        let mut prompt = prompt;
        prompt.cycle();
        assert_eq!(prompt.current_idx, 1);
        prompt.cycle();
        assert_eq!(prompt.current_idx, 0);

        // No outline at all → no reference → None (auto-resolve).
        let empty = StoreyDoc::default();
        assert!(empty
            .ceiling_line_position_prompt(ParameterAxis::X, 4.0, None)
            .is_none());
    }

    /// resolve: `reference + sign · value`, preserving the drawn side; rejects
    /// negative, non-finite, and off-footprint results.
    #[test]
    fn resolve_position_respects_sign_and_rejects_bad_values() {
        let doc = storey_10x6();
        let mut prompt = doc
            .ceiling_line_position_prompt(ParameterAxis::X, 4.0, None)
            .unwrap();
        // Nearest ref x = 0, drawn side +: 3 m → x = 3.
        assert!((doc.resolve_ceiling_line_position(&prompt, 3.0).unwrap() - 3.0).abs() < 1e-4);
        // Cycled to x = 10, drawn side −: 2.5 m → x = 7.5.
        prompt.cycle();
        assert!((doc.resolve_ceiling_line_position(&prompt, 2.5).unwrap() - 7.5).abs() < 1e-4);

        assert!(doc
            .resolve_ceiling_line_position(&prompt, -1.0)
            .unwrap_err()
            .contains("non-negative"));
        assert!(doc
            .resolve_ceiling_line_position(&prompt, f32::NAN)
            .unwrap_err()
            .contains("non-negative"));
        // 15 m from x = 10 going − lands at x = −5: off the footprint.
        assert_eq!(
            doc.resolve_ceiling_line_position(&prompt, 15.0)
                .unwrap_err(),
            LINE_OFF_FOOTPRINT_MSG
        );
    }

    /// Every section edge becomes a reference segment — dividers AND height
    /// steps alike, which is the difference from `ceiling_divider_segments`
    /// (dividers only, one bbox-spanning line each). These are what a stroke
    /// drawn INSIDE a section snaps and pins against now the section is no
    /// longer subtracted out of the drawable region.
    #[test]
    fn ceiling_section_edges_returns_every_edge_of_every_section() {
        let doc = storey_10x6();
        let steps = vec![
            CeilingStepRegion {
                polygon: vec![p(4., 0.), p(10., 0.), p(10., 6.), p(4., 6.)],
                delta_m: 0.0, // a divider
            },
            CeilingStepRegion {
                polygon: vec![p(0., 0.), p(2., 0.), p(2., 3.), p(0., 3.)],
                delta_m: 0.3, // a height step
            },
        ];
        let edges = doc.ceiling_section_edges(&steps);
        assert_eq!(edges.len(), 8, "4 edges each, both kinds: {edges:?}");
        // The divider's interior edge (x = 4) is present as a real segment —
        // this is the one a stroke drawn inside the right-hand section levels
        // onto, and the pin that keeps its dimension prompt from re-asking.
        assert!(
            edges
                .iter()
                .any(|[a, b]| (a.x - 4.0).abs() < 1e-6 && (b.x - 4.0).abs() < 1e-6),
            "{edges:?}"
        );
        // The height step contributes too (dividers aren't the only bound).
        assert!(
            edges
                .iter()
                .any(|[a, b]| (a.x - 2.0).abs() < 1e-6 && (b.x - 2.0).abs() < 1e-6),
            "{edges:?}"
        );
        // Degenerate rings and repeated points contribute nothing.
        let junk = vec![
            CeilingStepRegion {
                polygon: vec![p(1., 1.), p(2., 2.)],
                delta_m: 0.5,
            },
            CeilingStepRegion {
                polygon: vec![p(1., 1.), p(1., 1.), p(1., 1.)],
                delta_m: 0.5,
            },
        ];
        assert!(doc.ceiling_section_edges(&junk).is_empty());
    }

    /// The half-rect covers exactly the chosen side of the line, overhanging
    /// the footprint by the margin on the other three sides.
    #[test]
    fn ceiling_step_region_builds_the_half_rect_on_the_chosen_side() {
        let doc = storey_10x6();
        let region = doc
            .ceiling_step_region(ParameterAxis::X, 4.0, LineSide::Greater, 0.3)
            .expect("half-rect");
        assert!((region.delta_m - 0.3).abs() < 1e-12);
        assert_eq!(region.polygon.len(), 4);
        let min_x = region
            .polygon
            .iter()
            .map(|p| p.x)
            .fold(f32::INFINITY, f32::min);
        let max_x = region
            .polygon
            .iter()
            .map(|p| p.x)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (min_x - 4.0).abs() < 1e-4,
            "Greater side starts at the line"
        );
        assert!(max_x > 10.0, "overhangs the footprint (resolve clips)");
        let min_y = region
            .polygon
            .iter()
            .map(|p| p.y)
            .fold(f32::INFINITY, f32::min);
        let max_y = region
            .polygon
            .iter()
            .map(|p| p.y)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(min_y < 0.0 && max_y > 6.0, "full extent across the line");

        // Less side of a horizontal line: y ∈ [overhang, 2].
        let region = doc
            .ceiling_step_region(ParameterAxis::Y, 2.0, LineSide::Less, -0.2)
            .unwrap();
        let max_y = region
            .polygon
            .iter()
            .map(|p| p.y)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!((max_y - 2.0).abs() < 1e-4, "Less side ends at the line");

        // Empty outline → None.
        assert!(StoreyDoc::default()
            .ceiling_step_region(ParameterAxis::X, 4.0, LineSide::Less, 0.3)
            .is_none());
    }

    /// A StoreyDoc with a finalised staircase outline whose vertical walls sit
    /// at x = 0, 6.75, 7, 10 and horizontal walls at y = 0, 2, 4, 6 — i.e.
    /// NON-extreme parallel references at x ∈ {6.75, 7} and y ∈ {2, 4}.
    fn storey_staircase() -> StoreyDoc {
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(10., 0.),
            p(10., 2.),
            p(7., 2.),
            p(7., 4.),
            p(6.75, 4.),
            p(6.75, 6.),
            p(0., 6.),
        ])
        .unwrap();
        let known = outline.all_active_params();
        let mut doc = StoreyDoc::default();
        doc.outline = ShapeSlot {
            floor_plan: outline,
            dimensioning: DimensionState {
                scale_set: true,
                known_params: known,
                ..Default::default()
            },
        };
        doc.load_active_into_buffer();
        doc
    }

    /// Within tolerance of a non-extreme parallel wall the line snaps, and
    /// when TWO qualify the NEAREST wins; beyond tolerance there's no snap.
    #[test]
    fn snap_picks_the_nearest_non_extreme_wall_within_tolerance() {
        let doc = storey_staircase();
        // x = 6.9 is within 0.2 of BOTH x = 7 (0.1 away) and x = 6.75
        // (0.15 away) — the nearer wall, x = 7, wins.
        let (coord, _) = doc
            .snap_ceiling_step_line(ParameterAxis::X, 6.9, &[], 0.2)
            .expect("within tolerance");
        assert!(
            (coord - 7.0).abs() < 1e-6,
            "nearest wall wins (got {coord})"
        );
        // x = 6.82 flips the nearest to 6.75.
        let (coord, _) = doc
            .snap_ceiling_step_line(ParameterAxis::X, 6.82, &[], 0.2)
            .unwrap();
        assert!((coord - 6.75).abs() < 1e-6);
        // The Y axis snaps to its non-extreme walls the same way.
        let (coord, _) = doc
            .snap_ceiling_step_line(ParameterAxis::Y, 2.1, &[], 0.2)
            .unwrap();
        assert!((coord - 2.0).abs() < 1e-6);
        // Beyond tolerance: nearest non-extreme wall is 1.75 m away.
        assert!(doc
            .snap_ceiling_step_line(ParameterAxis::X, 5.0, &[], 0.2)
            .is_none());
    }

    /// Walls at the outline bbox extremes never snap (a line on the boundary
    /// steps nothing) — even when they're the only parallel walls at all.
    #[test]
    fn snap_excludes_bbox_extreme_walls() {
        let doc = storey_staircase();
        // Near the x = 0 / x = 10 extremes — inside the footprint, within
        // tolerance of the boundary walls, but those never qualify.
        assert!(doc
            .snap_ceiling_step_line(ParameterAxis::X, 0.1, &[], 0.2)
            .is_none());
        assert!(doc
            .snap_ceiling_step_line(ParameterAxis::X, 9.9, &[], 0.2)
            .is_none());
        // A plain rectangle has ONLY extreme walls: nothing ever snaps.
        let rect = storey_10x6();
        assert!(rect
            .snap_ceiling_step_line(ParameterAxis::X, 0.15, &[], 0.2)
            .is_none());
        assert!(rect
            .snap_ceiling_step_line(ParameterAxis::Y, 5.9, &[], 0.2)
            .is_none());
        // No outline at all → nothing to snap to.
        assert!(StoreyDoc::default()
            .snap_ceiling_step_line(ParameterAxis::X, 1.0, &[], 0.2)
            .is_none());
    }

    /// EXTRA reference coordinates (existing region lines this storey / the one
    /// below) snap too — even on a plain rectangle whose only OUTLINE walls are
    /// the excluded extremes. The winner carries NO outline vid, and an extra on
    /// the bbox extreme is still excluded (a line there steps nothing).
    #[test]
    fn snap_uses_extra_reference_lines_with_no_outline_vid() {
        let rect = storey_10x6(); // only extreme outline walls
                                  // Drawn x = 4.1; an extra reference line at x = 4.0 is within tolerance.
        let (coord, vid) = rect
            .snap_ceiling_step_line(ParameterAxis::X, 4.1, &[4.0], 0.2)
            .expect("snaps to the extra reference line");
        assert!((coord - 4.0).abs() < 1e-6);
        assert!(vid.is_none(), "an extra line carries no outline vertex");
        // An extra coord ON the bbox extreme (x = 0) is excluded.
        assert!(rect
            .snap_ceiling_step_line(ParameterAxis::X, 0.1, &[0.0], 0.2)
            .is_none());
        // Beyond tolerance: no snap.
        assert!(rect
            .snap_ceiling_step_line(ParameterAxis::X, 4.5, &[4.0], 0.2)
            .is_none());
    }

    /// `ceiling_divider_segments`: a same-height divider region yields its
    /// interior split edge (the dashed-divider cut); a real height step and a
    /// full-footprint (degenerate) region yield nothing; an empty outline
    /// yields nothing at all.
    #[test]
    fn ceiling_divider_segments_returns_only_divider_cut_edges() {
        let doc = storey_10x6();
        // The left half of the 10×6 footprint as a delta-0 divider: its
        // interior edge is the vertical cut at x = 4.
        let divider = CeilingStepRegion {
            polygon: vec![p(0., 0.), p(4., 0.), p(4., 6.), p(0., 6.)],
            delta_m: 0.0,
        };
        // A REAL step over the right half: excluded (its riser is already a
        // step-line reference elsewhere).
        let step = CeilingStepRegion {
            polygon: vec![p(4., 0.), p(10., 0.), p(10., 6.), p(4., 6.)],
            delta_m: 0.3,
        };
        // A divider covering the whole footprint: degenerate, no interior edge.
        let full = CeilingStepRegion {
            polygon: vec![p(0., 0.), p(10., 0.), p(10., 6.), p(0., 6.)],
            delta_m: 0.0,
        };
        let segs = doc.ceiling_divider_segments(&[divider.clone(), step, full]);
        assert_eq!(segs.len(), 1, "only the divider's cut edge is returned");
        let [a, b] = segs[0];
        assert!((a.x - 4.0).abs() < 1e-4 && (b.x - 4.0).abs() < 1e-4);
        let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
        assert!(lo.abs() < 1e-4 && (hi - 6.0).abs() < 1e-4);

        // No outline → nothing to bound the cut against.
        assert!(StoreyDoc::default()
            .ceiling_divider_segments(&[divider])
            .is_empty());
        // No steps at all → empty.
        assert!(doc.ceiling_divider_segments(&[]).is_empty());
    }

    /// `region_line_coords` returns the CONSTANT coordinate of each axis-aligned
    /// region edge parallel to a step line on the axis: a vertical edge's `x`
    /// for axis X, a horizontal edge's `y` for axis Y — deduped.
    #[test]
    fn region_line_coords_picks_axis_parallel_edges() {
        use ber_model::ber::common::ItemId;
        // Two side-by-side rects: x∈[0,4] and x∈[4,7], both y∈[0,3]. The shared
        // internal boundary is x = 4.
        let cid = ItemId::default();
        let mut regions = FloorRegions::new(
            vec![
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                Point2f64::new(4.0, 3.0),
                Point2f64::new(0.0, 3.0),
            ],
            cid,
        );
        regions.regions.extend(
            FloorRegions::new(
                vec![
                    Point2f64::new(4.0, 0.0),
                    Point2f64::new(7.0, 0.0),
                    Point2f64::new(7.0, 3.0),
                    Point2f64::new(4.0, 3.0),
                ],
                cid,
            )
            .regions,
        );
        let mut xs = region_line_coords(&regions, ParameterAxis::X);
        xs.sort_by(f32::total_cmp);
        assert_eq!(xs, vec![0.0, 4.0, 7.0], "vertical edges' x (deduped)");
        let mut ys = region_line_coords(&regions, ParameterAxis::Y);
        ys.sort_by(f32::total_cmp);
        assert_eq!(ys, vec![0.0, 3.0], "horizontal edges' y (deduped)");
    }

    /// `ceiling_line_coords` works the same over ceiling regions, whose rings
    /// are 3D and UNCLOSED — the wrap (last→first) edge is still counted.
    #[test]
    fn ceiling_line_coords_counts_the_wrap_edge_of_unclosed_rings() {
        use ber_model::ber::common::ItemId;
        let ceilings = CeilingRegions::default_for_footprint(
            vec![vec![
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                Point2f64::new(4.0, 3.0),
                Point2f64::new(0.0, 3.0),
            ]],
            2.4,
            ItemId::default(),
        );
        let mut xs = ceiling_line_coords(&ceilings, ParameterAxis::X);
        xs.sort_by(f32::total_cmp);
        assert_eq!(xs, vec![0.0, 4.0], "both vertical edges incl. the wrap");
        let mut ys = ceiling_line_coords(&ceilings, ParameterAxis::Y);
        ys.sort_by(f32::total_cmp);
        assert_eq!(ys, vec![0.0, 3.0]);
    }

    /// `snap_roof_ridge_coord`: the ceiling-line snap WITH the bbox extremes —
    /// a ridge end on the boundary is a gable. Extreme and interior walls
    /// both qualify, nearest wins, beyond tolerance is no snap.
    #[test]
    fn ridge_end_snap_includes_bbox_extreme_walls() {
        let doc = storey_staircase();
        // Near the x = 0 extreme: the step-line snap refuses, the ridge-end
        // snap lands ON the boundary.
        assert!(doc
            .snap_ceiling_step_line(ParameterAxis::X, 0.1, &[], 0.2)
            .is_none());
        let (coord, _) = doc
            .snap_roof_ridge_coord(ParameterAxis::X, 0.1, &[], 0.2)
            .expect("extreme wall snaps for a ridge end");
        assert!(coord.abs() < 1e-6);
        // Near the x = 10 extreme.
        let (coord, _) = doc
            .snap_roof_ridge_coord(ParameterAxis::X, 9.9, &[], 0.2)
            .unwrap();
        assert!((coord - 10.0).abs() < 1e-6);
        // Interior walls still snap identically (x = 6.9 → nearest is 7).
        let (coord, _) = doc
            .snap_roof_ridge_coord(ParameterAxis::X, 6.9, &[], 0.2)
            .unwrap();
        assert!((coord - 7.0).abs() < 1e-6);
        // Beyond tolerance: nothing.
        assert!(doc
            .snap_roof_ridge_coord(ParameterAxis::X, 5.0, &[], 0.2)
            .is_none());
        // A plain rectangle's ONLY walls are extremes: the ridge end snaps
        // where the step line never could.
        let rect = storey_10x6();
        let (coord, _) = rect
            .snap_roof_ridge_coord(ParameterAxis::Y, 5.9, &[], 0.2)
            .unwrap();
        assert!((coord - 6.0).abs() < 1e-6);
        assert!(rect
            .snap_ceiling_step_line(ParameterAxis::Y, 5.9, &[], 0.2)
            .is_none());
        // No outline at all → nothing to snap to.
        assert!(StoreyDoc::default()
            .snap_roof_ridge_coord(ParameterAxis::X, 1.0, &[], 0.2)
            .is_none());
    }

    /// The ridge snap also considers `extra_coords` — internal area boundaries
    /// the shell gathers (e.g. a raised ceiling area's edge). A ridge drawn near
    /// an area edge that ISN'T an outline wall still locks onto it, returning no
    /// vertex id (there's no outline vertex behind an internal line).
    #[test]
    fn ridge_coord_snap_locks_onto_an_internal_area_boundary() {
        let rect = storey_10x6(); // outline walls on X at x = 0 and x = 10 only
                                  // Nothing near x = 4.05 without extras (both outline walls are far off).
        assert!(rect
            .snap_roof_ridge_coord(ParameterAxis::X, 4.05, &[], 0.2)
            .is_none());
        // With an internal boundary at x = 4 supplied, the ridge snaps onto it.
        let (coord, vid) = rect
            .snap_roof_ridge_coord(ParameterAxis::X, 4.05, &[4.0], 0.2)
            .expect("snaps to the internal boundary");
        assert!((coord - 4.0).abs() < 1e-6);
        assert!(
            vid.is_none(),
            "an internal boundary has no outline vertex id"
        );
    }

    // ── Smarter default reference wall (Slice B1) ───────────────────────────

    /// An L-shaped outline: a wide bottom arm (y ∈ 0..3, x ∈ 0..12) plus a
    /// tall left arm (x ∈ 0..4, y ∈ 0..10). Vertical outline walls at
    /// x ∈ {0, 4, 12}; the right arm's bounding wall is x = 12.
    fn storey_l_shape() -> StoreyDoc {
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(12., 0.),
            p(12., 3.),
            p(4., 3.),
            p(4., 10.),
            p(0., 10.),
        ])
        .unwrap();
        let known = outline.all_active_params();
        let mut doc = StoreyDoc::default();
        doc.outline = ShapeSlot {
            floor_plan: outline,
            dimensioning: DimensionState {
                scale_set: true,
                known_params: known,
                ..Default::default()
            },
        };
        doc.load_active_into_buffer();
        doc
    }

    /// B-T1: with `Some(own_arm_ring)`, the default option (`current_idx 0`)
    /// is a wall bounding the ring, even when another wall is globally nearer;
    /// the full option set is still present and cyclable.
    #[test]
    fn prompt_prefers_a_wall_bounding_the_region() {
        let doc = storey_l_shape();
        // A vertical ridge line at x = 10, in the bottom-right arm. Reference
        // walls on X are x ∈ {0, 4, 12}. Globally nearest to 10 is x = 12
        // (2 away) then x = 4 (6 away) then x = 0 (10 away). The own arm's
        // ring spans x ∈ 4..12, so x = 4 and x = 12 BOTH bound it — x = 12 is
        // already nearest, so to prove the sort we put the ridge at x = 5,
        // where x = 4 (1 away, bounds the region) beats x = 0? No — pick a
        // region whose bounding wall is NOT the global nearest:
        // ring = the LEFT arm (x ∈ 0..4), ridge at x = 3.5 → global nearest is
        // x = 4 (0.5) which DOES bound it; so use the bottom arm ring with the
        // line near x = 4 boundary but make x=4 not nearest. Simplest robust
        // case: line at x = 3, own ring = bottom arm (x ∈ 4..12). Global
        // nearest is x = 4 (1 away) which bounds the bottom arm; next x = 0
        // (3 away) which does NOT. Default must be x = 4.
        let bottom_arm: Vec<Point2f64> = vec![
            Point2f64::new(4.0, 0.0),
            Point2f64::new(12.0, 0.0),
            Point2f64::new(12.0, 3.0),
            Point2f64::new(4.0, 3.0),
        ];
        // Put the line where the GLOBAL nearest wall does NOT bound the arm:
        // line at x = 1 → nearest is x = 0 (1 away, NOT on the bottom arm),
        // next x = 4 (3 away, BOUNDS the arm). With the ring the default flips
        // to x = 4.
        let prompt = doc
            .ceiling_line_position_prompt(ParameterAxis::X, 1.0, Some(&bottom_arm))
            .expect("parallel walls");
        assert_eq!(
            prompt.options.len(),
            3,
            "full option set preserved (x ∈ 0,4,12)"
        );
        assert!(
            (prompt.options[prompt.current_idx].reference_value - 4.0).abs() < 1e-4,
            "default is the arm-bounding wall x=4, not the global-nearest x=0"
        );

        // Without the ring, the global nearest (x = 0) is the default.
        let plain = doc
            .ceiling_line_position_prompt(ParameterAxis::X, 1.0, None)
            .unwrap();
        assert!((plain.options[0].reference_value - 0.0).abs() < 1e-4);
    }

    /// B-T2: `None` is identical to the current global-nearest ordering.
    #[test]
    fn prompt_none_is_global_nearest() {
        let doc = storey_10x6();
        let prompt = doc
            .ceiling_line_position_prompt(ParameterAxis::X, 4.0, None)
            .unwrap();
        assert_eq!(prompt.options.len(), 2);
        assert!((prompt.options[0].reference_value - 0.0).abs() < 1e-4);
        assert!((prompt.options[1].reference_value - 10.0).abs() < 1e-4);
        assert_eq!(prompt.current_idx, 0);
    }

    /// B-T3: a ring no parallel wall lies on (degenerate) falls back to
    /// global-nearest — the bounding test never matches, so the sort is the
    /// plain distance order.
    #[test]
    fn prompt_degenerate_ring_falls_back() {
        let doc = storey_10x6();
        // A ring whose X-extent (20..30) matches NO reference wall (x ∈ 0,10).
        let off_ring = vec![
            Point2f64::new(20.0, 0.0),
            Point2f64::new(30.0, 0.0),
            Point2f64::new(30.0, 6.0),
            Point2f64::new(20.0, 6.0),
        ];
        let prompt = doc
            .ceiling_line_position_prompt(ParameterAxis::X, 4.0, Some(&off_ring))
            .unwrap();
        assert!(
            (prompt.options[0].reference_value - 0.0).abs() < 1e-4,
            "no wall bounds the ring → plain global-nearest default"
        );
    }

    /// B-T4: `ridge_region_ring` returns the midpoint's region ring, and
    /// `None` when the midpoint is in no region.
    #[test]
    fn ridge_region_ring_returns_the_midpoints_region() {
        let cid = Default::default();
        let default_z = 2.4;
        // Two same-height halves at x ∈ 0..4 and x ∈ 4..10.
        let base = CeilingRegions::default_for_footprint(vec![square64(0.0, 10.0)], default_z, cid)
            .with_z_deltas(&[(square64(0.0, 4.0), 0.0), (square64(4.0, 10.0), 0.0)]);
        let storey = Storey {
            ceilings: base,
            ceiling_height_m: Some(default_z),
            ..Storey::default()
        };
        // A ridge along Y in the RIGHT half: cross axis X at x = 7, extent
        // y ∈ 1..5 → midpoint (7, 3), inside the x ∈ 4..10 region.
        let ring = crate::ridge_region_ring(&storey, ParameterAxis::X, 7.0, 1.0, 4.0)
            .expect("midpoint is in the right region");
        let min_x = ring.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let max_x = ring.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
        assert!((min_x - 4.0).abs() < 1e-6 && (max_x - 10.0).abs() < 1e-6);

        // A midpoint outside any region → None.
        assert!(
            crate::ridge_region_ring(&storey, ParameterAxis::X, 50.0, 1.0, 4.0).is_none(),
            "midpoint in no region"
        );
    }
}
