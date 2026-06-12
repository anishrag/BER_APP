//! Wall-exposure model operations: painting / clearing per-wall heat-loss
//! sections on the storey outline, and deriving full coverage. Pure model
//! mutation — the shell owns pointer selection, undo, and persistence.
use crate::{ExposureSection, StoreyDoc};
use datatypes::ber::structural::{WallExposure, WallRegions};
use domain::points::{Point2f32, Point2f64};
use manip_2d_plan::{project_distance_along, segment_len, VertexId};
use std::collections::HashMap;

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
/// the painted sections in order, with every gap (and the whole wall when
/// nothing is painted) filled with `Unset` — unmarked length is genuinely
/// unset, never assumed `Exposed`. `Unset` is DERIVED coverage only: it never
/// enters the stored sections (single-representation rule). Used to derive one
/// BER `WallRegion` per range (via [`StoreyDoc::wall_exposure_segments`]).
pub fn wall_coverage(len: f32, sections: &[ExposureSection]) -> Vec<(f32, f32, WallExposure)> {
    let mut out: Vec<(f32, f32, WallExposure)> = Vec::new();
    let mut cursor = 0.0_f32;
    for sec in sections {
        let s = sec.start_m.clamp(0.0, len);
        let e = sec.end_m.clamp(0.0, len);
        if s > cursor + 1e-4 {
            out.push((cursor, s, WallExposure::Unset)); // gap before this section
        }
        if e > s + 1e-4 {
            out.push((s, e, sec.exposure));
            cursor = e;
        }
    }
    if cursor < len - 1e-4 {
        out.push((cursor, len, WallExposure::Unset)); // trailing gap
    }
    if out.is_empty() {
        out.push((0.0, len, WallExposure::Unset)); // nothing painted → all unset
    }
    out
}

/// The coverage range `(start, end)` — a marked section or an un-marked gap —
/// containing distance `d` along a wall of length `len`. Lets a tap select just
/// the piece under it rather than the whole wall.
pub fn range_at(len: f32, sections: &[ExposureSection], d: f32) -> (f32, f32) {
    for (s, e, _) in wall_coverage(len, sections) {
        if d >= s - 1e-4 && d <= e + 1e-4 {
            return (s, e);
        }
    }
    (0.0, len)
}

impl StoreyDoc {
    /// Paint a section `[start, end]` (clamped to the wall's length `len`) with
    /// `exposure` onto outline wall `vid`. ONLY painted sections are stored —
    /// unmarked length never materializes into sections; it shows up as `Unset`
    /// in the derived coverage instead (single-representation rule: the
    /// in-session model equals the post-reload one with no canonicalize pass).
    pub fn set_wall_section(
        &mut self,
        vid: VertexId,
        start: f32,
        end: f32,
        len: f32,
        exposure: WallExposure,
    ) {
        let sections = self.wall_sections.entry(vid).or_default();
        paint_section(
            sections,
            start.clamp(0.0, len),
            end.clamp(0.0, len),
            exposure,
        );
    }

    /// Remove all exposure sections from outline wall `vid`.
    pub fn clear_wall_section(&mut self, vid: VertexId) {
        self.wall_sections.remove(&vid);
    }

    /// This storey's heat-loss wall SEGMENTS over the FULL perimeter: every
    /// outline wall split into its coverage ranges — painted sections plus
    /// `Unset` gaps, an unmarked wall yielding one full-length `Unset` segment
    /// — as `(start, end, exposure)` in world coords (endpoints interpolated
    /// along the wall by distance). The shell projects these into BER
    /// `WallRegion`s, so every wall exists in the resolved geometry whether
    /// it's been marked or not.
    pub fn wall_exposure_segments(&self) -> Vec<(Point2f64, Point2f64, WallExposure)> {
        self.outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .flat_map(|(vid, a, b)| {
                let sections = self
                    .wall_sections
                    .get(&vid)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                let lerp = move |d: f32| {
                    let t = if len > 1e-6 { d / len } else { 0.0 };
                    Point2f64::new(
                        (a.x + t * (b.x - a.x)) as f64,
                        (a.y + t * (b.y - a.y)) as f64,
                    )
                };
                wall_coverage(len, sections)
                    .into_iter()
                    .map(move |(s, e, exposure)| (lerp(s), lerp(e), exposure))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Rebuild the runtime section model from the persisted `WallRegions` (the
    /// source of truth) on load. Each region's base edge is projected onto the
    /// perimeter wall it lies on to recover its `(start_m, end_m, exposure)`;
    /// regions are grouped by wall and sorted. `Unset` and `Ground` regions are
    /// SKIPPED: `Unset` is derived coverage (recovering it would mark every
    /// wall after one save/load) and `Ground` belongs to derived riser walls
    /// (recovering it would turn risers into phantom painted sections) —
    /// neither was painted, so neither comes back as a section. Inverse of the
    /// commit-time derivation.
    pub fn sections_from_wall_regions(
        &self,
        walls: &WallRegions,
    ) -> HashMap<VertexId, Vec<ExposureSection>> {
        let perim = self.outline.floor_plan.perimeter_walls();
        let mut out: HashMap<VertexId, Vec<ExposureSection>> = HashMap::new();
        for region in &walls.regions {
            if matches!(region.exposure, WallExposure::Unset | WallExposure::Ground) {
                continue; // derived, not painted — never recovers to sections
            }
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
            let len = segment_len(a, b);
            let d0 = project_distance_along(a, b, len, p0.x, p0.y);
            let d1 = project_distance_along(a, b, len, p1.x, p1.y);
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

    /// Paint every `Unset` coverage RANGE on the outline `Exposed` — whole
    /// unmarked walls AND the gaps around painted sections (the gap case is
    /// what makes this work after a reload, where gaps are the only `Unset`
    /// representation). Returns whether anything changed (so the shell can
    /// skip undo/persist).
    pub fn set_all_remaining_exposed(&mut self) -> bool {
        let mut changed = false;
        for (vid, a, b) in self.outline.floor_plan.perimeter_walls() {
            let len = segment_len(a, b);
            let unset: Vec<(f32, f32)> = wall_coverage(
                len,
                self.wall_sections
                    .get(&vid)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
            .into_iter()
            .filter(|(s, e, exp)| *exp == WallExposure::Unset && e - s > 1e-4)
            .map(|(s, e, _)| (s, e))
            .collect();
            for (s, e) in unset {
                self.set_wall_section(vid, s, e, len, WallExposure::Exposed);
                changed = true;
            }
        }
        changed
    }
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
    fn coverage_fills_gaps_with_unset() {
        let s = vec![sec(2.0, 3.0, WallExposure::SemiExposed)];
        let cov = wall_coverage(5.0, &s);
        assert_eq!(
            cov,
            vec![
                (0.0, 2.0, WallExposure::Unset),
                (2.0, 3.0, WallExposure::SemiExposed),
                (3.0, 5.0, WallExposure::Unset),
            ]
        );
    }

    #[test]
    fn coverage_of_no_sections_is_all_unset() {
        assert_eq!(
            wall_coverage(4.0, &[]),
            vec![(0.0, 4.0, WallExposure::Unset)]
        );
    }

    /// A finalised 4×4 m square StoreyDoc for the model-level tests below.
    fn square_doc() -> crate::StoreyDoc {
        use domain::points::Point2f32;
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ];
        let plan = manip_2d_plan::FloorPlan::from_clean_ring(&pts).unwrap();
        let mut doc = crate::StoreyDoc::default();
        doc.dimensioning.scale_set = true;
        doc.floor_plan = plan.clone();
        doc.outline = crate::ShapeSlot {
            floor_plan: plan,
            dimensioning: doc.dimensioning.clone(),
        };
        doc
    }

    /// SINGLE-REPRESENTATION RULE: painting part of a wall stores ONLY the
    /// painted section — the gaps are never materialized into sections; they
    /// exist only as `Unset` ranges in the derived coverage.
    #[test]
    fn set_wall_section_stores_only_painted_sections() {
        let mut doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        doc.set_wall_section(vid, 1.0, 3.0, len, WallExposure::SemiExposed);

        let secs = &doc.wall_sections[&vid];
        assert_eq!(secs, &vec![sec(1.0, 3.0, WallExposure::SemiExposed)]);
        assert_eq!(
            wall_coverage(len, secs),
            vec![
                (0.0, 1.0, WallExposure::Unset),
                (1.0, 3.0, WallExposure::SemiExposed),
                (3.0, len, WallExposure::Unset),
            ]
        );
    }

    /// `wall_exposure_segments` covers the FULL perimeter: unmarked walls
    /// yield one full-length `Unset` segment; a partially marked wall yields
    /// its painted section plus `Unset` gaps.
    #[test]
    fn wall_exposure_segments_cover_the_full_perimeter() {
        let mut doc = square_doc();
        // Nothing painted → one Unset segment per wall, tiling the perimeter.
        let segs = doc.wall_exposure_segments();
        assert_eq!(segs.len(), 4);
        assert!(segs.iter().all(|(_, _, e)| *e == WallExposure::Unset));
        let total: f64 = segs
            .iter()
            .map(|(a, b, _)| ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt())
            .sum();
        assert!((total - 16.0).abs() < 1e-6, "tiles the 4×4 perimeter");

        // Paint the middle of wall0 → its segment splits into gap/mark/gap.
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        doc.set_wall_section(vid, 1.0, 3.0, len, WallExposure::SemiExposed);
        let segs = doc.wall_exposure_segments();
        assert_eq!(segs.len(), 6, "3 ranges on wall0 + 1 per other wall");
        assert_eq!(
            segs.iter()
                .filter(|(_, _, e)| *e == WallExposure::SemiExposed)
                .count(),
            1
        );
        assert_eq!(
            segs.iter()
                .filter(|(_, _, e)| *e == WallExposure::Unset)
                .count(),
            5
        );
    }

    /// "Set remaining → Exposed" paints every Unset coverage range: whole
    /// unmarked walls AND the gaps around painted sections. Idempotent.
    #[test]
    fn set_all_remaining_exposed_paints_unset_walls_and_gaps() {
        let mut doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        doc.set_wall_section(vid, 1.0, 3.0, len, WallExposure::SemiExposed);

        assert!(doc.set_all_remaining_exposed(), "something to paint");
        // Wall0: the mark survives, its two gaps are now explicit Exposed.
        let secs = &doc.wall_sections[&vid];
        assert_eq!(secs.len(), 3);
        assert_eq!(secs[0], sec(0.0, 1.0, WallExposure::Exposed));
        assert_eq!(secs[1], sec(1.0, 3.0, WallExposure::SemiExposed));
        assert_eq!(secs[2], sec(3.0, len, WallExposure::Exposed));
        // Other walls: one full-length Exposed section each.
        for (v, a, b) in doc.outline.floor_plan.perimeter_walls() {
            if v == vid {
                continue;
            }
            let secs = &doc.wall_sections[&v];
            assert_eq!(secs.len(), 1);
            assert_eq!(secs[0], sec(0.0, segment_len(a, b), WallExposure::Exposed));
        }
        // Nothing Unset remains → a second pass changes nothing.
        assert!(!doc.set_all_remaining_exposed(), "idempotent");
    }
}
