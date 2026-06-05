//! Wall-exposure model operations: painting / clearing per-wall heat-loss
//! sections on the storey outline, and deriving full coverage. Pure model
//! mutation — the shell owns pointer selection, undo, and persistence.
use crate::{Document, ExposureSection};
use datatypes::ber::structural::WallExposure;
use manip_2d_plan::VertexId;

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
/// range (called by the shell's `commit_to_ber`).
pub fn wall_coverage(len: f32, sections: &[ExposureSection]) -> Vec<(f32, f32, WallExposure)> {
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

impl Document {
    /// Paint a section `[start, end]` (the wall's length is `len`) with `exposure`
    /// onto outline wall `vid`, then fill the gaps so the wall is fully
    /// partitioned into explicit sections.
    pub fn set_wall_section(
        &mut self,
        vid: VertexId,
        start: f32,
        end: f32,
        len: f32,
        exposure: WallExposure,
    ) {
        let sections = self.wall_sections.entry(vid).or_default();
        paint_section(sections, start, end, exposure);
        *sections = wall_coverage(len, sections)
            .into_iter()
            .map(|(start_m, end_m, exposure)| ExposureSection {
                start_m,
                end_m,
                exposure,
            })
            .collect();
    }

    /// Remove all exposure sections from outline wall `vid`.
    pub fn clear_wall_section(&mut self, vid: VertexId) {
        self.wall_sections.remove(&vid);
    }

    /// Mark every outline wall that has no sections set as fully `Exposed`.
    /// Returns whether anything changed (so the shell can skip undo/persist).
    pub fn set_all_remaining_exposed(&mut self) -> bool {
        let unset: Vec<(VertexId, f32)> = self
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .filter(|(vid, _, _)| self.wall_sections.get(vid).map_or(true, |s| s.is_empty()))
            .map(|(vid, a, b)| (vid, ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()))
            .collect();
        if unset.is_empty() {
            return false;
        }
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
        true
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
