//! Wall-exposure marking: classifying the storey's perimeter (envelope) walls
//! by heat loss — `Exposed` (open to outside), `SemiExposed` (borders an
//! unheated space or a neighbour's dwelling), or `NonHeatLoss` (party/internal).
//!
//! A wall can carry several exposure SECTIONS along its length (each a range in
//! metres from the wall's start vertex). Each section is an independent,
//! single-exposure piece — at the BER layer a wall splits into one `WallRegion`
//! per section (never a fused multi-exposure region). Un-marked length defaults
//! to `Exposed` (the envelope assumption). Exposure is keyed by each wall's
//! stable start `VertexId`, lives on the OUTLINE only (living-area interior
//! walls aren't envelope/heat-loss elements), and is derived into BER
//! `WallRegions` on commit (see `commit_to_ber`).
//!
//! Sections are full-height for now. A future face/height split (e.g. a dormer
//! side wall cut diagonally into exposed top + semi-exposed bottom) can be added
//! by giving `ExposureSection` a face subdivision — see the plan; the BER output
//! (`WallRegion.polygon` is an arbitrary `Polygon3D`) already allows it.
use super::*;

/// One exposure piece along a wall: the range `[start_m, end_m]` (metres from
/// the wall's start vertex) and its heat-loss class. Independent and
/// single-exposure; per wall they're kept sorted and non-overlapping.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExposureSection {
    pub start_m: f32,
    pub end_m: f32,
    pub exposure: WallExposure,
}

/// Which end of the current selection a drag is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionHandle {
    Start,
    End,
}

/// One outline wall for rendering: endpoints (world coords), its length, the
/// exposure sections set on it, and whether it's selected.
pub struct WallExposureView {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub length_m: f32,
    pub sections: Vec<ExposureSection>,
    pub selected: bool,
}

/// The selected wall's current section selection, for drawing handles and the
/// toolbar readout.
pub struct ExposureSelection {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub start_m: f32,
    pub end_m: f32,
    pub length_m: f32,
}

/// Paint `[start, end]` of a wall with `exposure`: the new mark WINS over any
/// overlap (existing sections are trimmed to their non-overlapping remnants),
/// then the new section is inserted and the list re-sorted. No merging of
/// adjacent same-type ranges — each stays its own region.
fn paint_section(
    sections: &mut Vec<ExposureSection>,
    start: f32,
    end: f32,
    exposure: WallExposure,
) {
    let (s, e) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if e - s < 1e-4 {
        return; // zero-length selection paints nothing
    }
    let mut out: Vec<ExposureSection> = Vec::new();
    for sec in sections.iter() {
        if sec.end_m <= s || sec.start_m >= e {
            out.push(*sec); // no overlap
            continue;
        }
        if sec.start_m < s - 1e-4 {
            out.push(ExposureSection {
                start_m: sec.start_m,
                end_m: s,
                exposure: sec.exposure,
            });
        }
        if sec.end_m > e + 1e-4 {
            out.push(ExposureSection {
                start_m: e,
                end_m: sec.end_m,
                exposure: sec.exposure,
            });
        }
        // The overlapped middle is dropped — the new mark wins.
    }
    out.push(ExposureSection {
        start_m: s,
        end_m: e,
        exposure,
    });
    out.sort_by(|a, b| {
        a.start_m
            .partial_cmp(&b.start_m)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    *sections = out;
}

/// Full coverage of a wall of length `len` as `(start, end, exposure)` ranges:
/// the set sections in order, with every gap (and the whole wall when there are
/// no sections) filled with `Exposed`. Used to derive one BER `WallRegion` per
/// range.
pub(crate) fn wall_coverage(
    len: f32,
    sections: &[ExposureSection],
) -> Vec<(f32, f32, WallExposure)> {
    let mut out: Vec<(f32, f32, WallExposure)> = Vec::new();
    let mut cursor = 0.0_f32;
    for sec in sections {
        let s = sec.start_m.clamp(0.0, len);
        let e = sec.end_m.clamp(0.0, len);
        if s > cursor + 1e-4 {
            out.push((cursor, s, WallExposure::Exposed)); // gap before this section
        }
        if e > s + 1e-4 {
            out.push((s, e, sec.exposure));
            cursor = e;
        }
    }
    if cursor < len - 1e-4 {
        out.push((cursor, len, WallExposure::Exposed)); // trailing gap
    }
    if out.is_empty() {
        out.push((0.0, len, WallExposure::Exposed)); // no sections → all exposed
    }
    out
}

impl AppController {
    /// True when the editor is in wall-exposure mode (drives the UI toggle and
    /// routes a canvas tap/drag to wall selection rather than drawing).
    pub fn exposure_mode(&self) -> bool {
        self.exposure_mode
    }

    /// Enter or leave wall-exposure mode. Exposure applies to the storey
    /// envelope, so entering forces the outline active (and leaves living-area
    /// mode). All selection/drag state is cleared.
    pub fn set_exposure_mode(&mut self, on: bool) {
        self.exposure_mode = on;
        self.selected_exposure_wall = None;
        self.exposure_sel = None;
        self.exposure_drag = None;
        if on {
            // Flush the live buffer to its slot first: `activate_shape(Outline)`
            // early-returns without syncing when the outline is already active,
            // which would leave `self.outline.floor_plan` stale (the renderer
            // reads it). Then leave living-area mode and act on the outline.
            self.sync_active();
            self.set_living_area_mode(false);
        }
    }

    /// Endpoints + length of the selected wall, if any.
    fn selected_wall_geom(&self) -> Option<(Point2f32, Point2f32, f32)> {
        let vid = self.selected_exposure_wall?;
        self.outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .find(|(v, _, _)| *v == vid)
            .map(|(_, a, b)| (a, b, wall_len(a, b)))
    }

    /// Primary press in exposure mode. If a selection handle is within `tol` of
    /// the pointer, begin dragging it; otherwise select the wall under the
    /// pointer (defaulting its selection to the whole wall). Tapping empty space
    /// clears the selection.
    pub fn exposure_pointer_down(&mut self, world_x: f64, world_y: f64, tol: f64) {
        // 1. Grab a selection handle if the pointer is on one.
        if let (Some((a, b, len)), Some((s_m, e_m))) =
            (self.selected_wall_geom(), self.exposure_sel)
        {
            let (px, py) = (world_x as f32, world_y as f32);
            let (sx, sy) = lerp_along(a, b, len, s_m);
            let (ex, ey) = lerp_along(a, b, len, e_m);
            let t2 = (tol * tol) as f32;
            let ds = (px - sx).powi(2) + (py - sy).powi(2);
            let de = (px - ex).powi(2) + (py - ey).powi(2);
            if ds <= t2 || de <= t2 {
                self.exposure_drag = Some(if ds <= de {
                    SectionHandle::Start
                } else {
                    SectionHandle::End
                });
                return;
            }
        }
        // 2. Otherwise (re)select the wall under the pointer (ring 0 only),
        //    picking just the SUB-SECTION under the tap — the coverage range (a
        //    marked section or an un-marked gap) containing the tapped distance.
        //    So once a wall is split, tapping a leftover piece selects only that
        //    piece, not the whole wall.
        match self.floor_plan.hit_test_wall(world_x, world_y, tol) {
            Some((0, edge_start)) => {
                self.selected_exposure_wall = self.floor_plan.vertex_id_at(0, edge_start);
                self.exposure_drag = None;
                self.exposure_sel = if let Some((a, b, len)) = self.selected_wall_geom() {
                    let d = project_distance(a, b, len, world_x as f32, world_y as f32);
                    let range = match self
                        .selected_exposure_wall
                        .and_then(|v| self.wall_sections.get(&v))
                    {
                        Some(secs) => range_at(len, secs, d),
                        None => (0.0, len),
                    };
                    Some(range)
                } else {
                    None
                };
            }
            _ => {
                self.selected_exposure_wall = None;
                self.exposure_sel = None;
                self.exposure_drag = None;
            }
        }
    }

    /// Drag the grabbed handle: project the pointer onto the wall line and set
    /// that end's distance (clamped to the wall). No-op if no handle is held.
    pub fn exposure_pointer_drag(&mut self, world_x: f64, world_y: f64) {
        let Some(handle) = self.exposure_drag else {
            return;
        };
        let Some((a, b, len)) = self.selected_wall_geom() else {
            return;
        };
        let Some((mut s_m, mut e_m)) = self.exposure_sel else {
            return;
        };
        let d = project_distance(a, b, len, world_x as f32, world_y as f32);
        match handle {
            SectionHandle::Start => s_m = d,
            SectionHandle::End => e_m = d,
        }
        self.exposure_sel = Some((s_m, e_m));
    }

    /// End a handle drag.
    pub fn exposure_pointer_up(&mut self) {
        self.exposure_drag = None;
    }

    /// Set the selection range directly (the typed-distance path). Clamped to
    /// the wall; no-op if no wall is selected.
    pub fn set_exposure_section_range(&mut self, start_m: f32, end_m: f32) {
        let Some((_, _, len)) = self.selected_wall_geom() else {
            return;
        };
        self.exposure_sel = Some((start_m.clamp(0.0, len), end_m.clamp(0.0, len)));
    }

    /// Paint the current selection range on the selected wall with `exposure`,
    /// then materialize the WHOLE wall into explicit sections — any leftover
    /// length around the mark becomes its own `Exposed` section. So marking a
    /// range inside a wall splits it into N explicit sections (and N
    /// `WallRegion`s on commit). Undoable; persists immediately.
    pub fn set_wall_exposure(&mut self, exposure: WallExposure) {
        let Some(vid) = self.selected_exposure_wall else {
            return;
        };
        let Some((s, e)) = self.exposure_sel else {
            return;
        };
        let Some((_, _, len)) = self.selected_wall_geom() else {
            return;
        };
        let undo_snap = self.current_history_snapshot();
        let sections = self.wall_sections.entry(vid).or_default();
        paint_section(sections, s, e, exposure);
        // Fill the gaps so the wall is fully partitioned into explicit sections.
        *sections = wall_coverage(len, sections)
            .into_iter()
            .map(|(start_m, end_m, exposure)| ExposureSection {
                start_m,
                end_m,
                exposure,
            })
            .collect();
        let _ = self.commit_to_ber();
        self.push_history_snapshot(undo_snap);
    }

    /// Remove all exposure sections from the selected wall (it reverts to the
    /// default Exposed assumption). Undoable; persists immediately.
    pub fn clear_wall_exposure(&mut self) {
        let Some(vid) = self.selected_exposure_wall else {
            return;
        };
        if !self.wall_sections.contains_key(&vid) {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        self.wall_sections.remove(&vid);
        let _ = self.commit_to_ber();
        self.push_history_snapshot(undo_snap);
    }

    /// Mark every outline wall with no sections yet as fully `Exposed` (one
    /// full-length section). Walls that already carry sections are untouched.
    /// Undoable; persists immediately. No-op if nothing changes.
    pub fn set_all_remaining_exposed(&mut self) {
        self.sync_active();
        let unset: Vec<(VertexId, f32)> = self
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .filter(|(vid, _, _)| self.wall_sections.get(vid).map_or(true, |s| s.is_empty()))
            .map(|(vid, a, b)| (vid, wall_len(a, b)))
            .collect();
        if unset.is_empty() {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        for (vid, len) in unset {
            self.wall_sections.insert(
                vid,
                vec![ExposureSection {
                    start_m: 0.0,
                    end_m: len,
                    exposure: WallExposure::Exposed,
                }],
            );
        }
        let _ = self.commit_to_ber();
        self.push_history_snapshot(undo_snap);
    }

    /// The outline perimeter walls with their sections + selection state, for
    /// rendering. World coords (f64), ring order.
    ///
    /// Sections are sourced from the in-memory `Storey.walls` (the `Ber` struct
    /// in `current_record`) by geometry-matching each `WallRegion` to its
    /// perimeter edge — BER data rendered from BER. The runtime `wall_sections`
    /// cache stays the editing model (where paint/clear/`set_remaining`
    /// mutate); on every commit those changes flow into `Storey.walls`, which
    /// is what this method then reads back. Selection is interaction state and
    /// stays live (`selected_exposure_wall` keyed by `VertexId`).
    pub fn wall_exposure_views(&self) -> Vec<WallExposureView> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let sections_by_vid = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .map(|s| self.sections_from_wall_regions(&s.walls))
            .unwrap_or_default();
        self.outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .map(|(vid, a, b)| WallExposureView {
                a: (a.x as f64, a.y as f64),
                b: (b.x as f64, b.y as f64),
                length_m: wall_len(a, b),
                sections: sections_by_vid.get(&vid).cloned().unwrap_or_default(),
                selected: self.selected_exposure_wall == Some(vid),
            })
            .collect()
    }

    /// The current section selection on the selected wall (for handles + the
    /// toolbar readout), or `None` when no wall is selected.
    pub fn exposure_selection(&self) -> Option<ExposureSelection> {
        let (a, b, len) = self.selected_wall_geom()?;
        let (s, e) = self.exposure_sel?;
        Some(ExposureSelection {
            a: (a.x as f64, a.y as f64),
            b: (b.x as f64, b.y as f64),
            start_m: s,
            end_m: e,
            length_m: len,
        })
    }

    /// Rebuild the runtime section model from the persisted `WallRegions` (the
    /// source of truth) on load. Each region's base edge is projected onto the
    /// perimeter wall it lies on to recover its `(start_m, end_m, exposure)`;
    /// regions are grouped by wall and sorted. Walls with no region are absent
    /// (unset). Inverse of the `commit_to_ber` derivation.
    pub(super) fn sections_from_wall_regions(
        &self,
        walls: &WallRegions,
    ) -> HashMap<VertexId, Vec<ExposureSection>> {
        let perim = self.outline.floor_plan.perimeter_walls();
        let mut out: HashMap<VertexId, Vec<ExposureSection>> = HashMap::new();
        for region in &walls.regions {
            let ext = &region.polygon.exterior;
            if ext.len() < 2 {
                continue;
            }
            let p0 = Point2f32::new(ext[0].x as f32, ext[0].y as f32);
            let p1 = Point2f32::new(ext[1].x as f32, ext[1].y as f32);
            let mid = Point2f32::new((p0.x + p1.x) * 0.5, (p0.y + p1.y) * 0.5);
            let Some((vid, a, b)) = perim.iter().copied().find(|(_, a, b)| {
                domain::points::point_to_segment_distance(
                    mid.x as f64,
                    mid.y as f64,
                    a.x as f64,
                    a.y as f64,
                    b.x as f64,
                    b.y as f64,
                ) < 1e-2
            }) else {
                continue; // region's base edge isn't on any current wall
            };
            let len = wall_len(a, b);
            let d0 = project_distance(a, b, len, p0.x, p0.y);
            let d1 = project_distance(a, b, len, p1.x, p1.y);
            out.entry(vid).or_default().push(ExposureSection {
                start_m: d0.min(d1),
                end_m: d0.max(d1),
                exposure: region.exposure,
            });
        }
        for secs in out.values_mut() {
            secs.sort_by(|x, y| {
                x.start_m
                    .partial_cmp(&y.start_m)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        out
    }
}

/// The coverage range `(start, end)` — a marked section or an un-marked gap —
/// containing distance `d` along a wall of length `len`. Lets a tap select just
/// the piece under it rather than the whole wall.
fn range_at(len: f32, sections: &[ExposureSection], d: f32) -> (f32, f32) {
    for (s, e, _) in wall_coverage(len, sections) {
        if d >= s - 1e-4 && d <= e + 1e-4 {
            return (s, e);
        }
    }
    (0.0, len)
}

/// Euclidean wall length.
fn wall_len(a: Point2f32, b: Point2f32) -> f32 {
    ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
}

/// Point at distance `d` (metres) along the wall `a→b` of length `len`.
fn lerp_along(a: Point2f32, b: Point2f32, len: f32, d: f32) -> (f32, f32) {
    let t = if len > 1e-6 { d / len } else { 0.0 };
    (a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
}

/// Distance (metres from `a`) of the projection of `(px, py)` onto the wall
/// `a→b`, clamped to `[0, len]`.
fn project_distance(a: Point2f32, b: Point2f32, len: f32, px: f32, py: f32) -> f32 {
    if len <= 1e-6 {
        return 0.0;
    }
    let dot = (px - a.x) * (b.x - a.x) + (py - a.y) * (b.y - a.y);
    (dot / len).clamp(0.0, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(start: f32, end: f32, e: WallExposure) -> ExposureSection {
        ExposureSection {
            start_m: start,
            end_m: end,
            exposure: e,
        }
    }

    #[test]
    fn paint_mid_range_splits_into_three() {
        let mut s = vec![sec(0.0, 5.0, WallExposure::Exposed)];
        paint_section(&mut s, 2.0, 3.0, WallExposure::SemiExposed);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], sec(0.0, 2.0, WallExposure::Exposed));
        assert_eq!(s[1], sec(2.0, 3.0, WallExposure::SemiExposed));
        assert_eq!(s[2], sec(3.0, 5.0, WallExposure::Exposed));
    }

    #[test]
    fn paint_whole_wall_is_one_section() {
        let mut s = Vec::new();
        paint_section(&mut s, 0.0, 4.0, WallExposure::NonHeatLoss);
        assert_eq!(s, vec![sec(0.0, 4.0, WallExposure::NonHeatLoss)]);
    }

    #[test]
    fn overlapping_repaint_overrides_the_overlap() {
        let mut s = vec![sec(0.0, 4.0, WallExposure::Exposed)];
        paint_section(&mut s, 1.0, 2.0, WallExposure::SemiExposed);
        // now exposed[0,1], semi[1,2], exposed[2,4]
        paint_section(&mut s, 1.5, 3.0, WallExposure::NonHeatLoss);
        // semi shrinks to [1,1.5], the [1.5,2] exposed-middle... actually [1.5,3]
        // wins: exposed[0,1], semi[1,1.5], non[1.5,3], exposed[3,4]
        assert_eq!(s[0], sec(0.0, 1.0, WallExposure::Exposed));
        assert_eq!(s[1], sec(1.0, 1.5, WallExposure::SemiExposed));
        assert_eq!(s[2], sec(1.5, 3.0, WallExposure::NonHeatLoss));
        assert_eq!(s[3], sec(3.0, 4.0, WallExposure::Exposed));
    }

    #[test]
    fn coverage_fills_gaps_with_exposed() {
        let s = vec![sec(2.0, 3.0, WallExposure::SemiExposed)];
        let cov = wall_coverage(5.0, &s);
        assert_eq!(
            cov,
            vec![
                (0.0, 2.0, WallExposure::Exposed),
                (2.0, 3.0, WallExposure::SemiExposed),
                (3.0, 5.0, WallExposure::Exposed),
            ]
        );
    }

    #[test]
    fn coverage_of_no_sections_is_all_exposed() {
        assert_eq!(
            wall_coverage(4.0, &[]),
            vec![(0.0, 4.0, WallExposure::Exposed)]
        );
    }
}
