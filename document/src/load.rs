//! Rebuilding the editing model from resolved BER geometry — the load path.
//! When no editing snapshot is available, a storey is reconstructed from its
//! persisted (finalised, metric) points: the inverse of the shell's resolve
//! projection. The shell owns *when* to rebuild (open/checkpoint/add-storey);
//! the reconstruction itself is pure model state.
use crate::{DimensionState, ShapeSlot, StoreyDoc};
use datatypes::ber::structural::{FloorRegions, Storey};
use domain::points::{to_f32, Point2f32};
use manip_2d_plan::FloorPlan;

impl StoreyDoc {
    /// Rebuild one storey's editing model from its resolved BER geometry.
    ///
    /// Outline: rebuilt from the resolved floor footprint
    /// ([`FloorRegions::footprint_ring`]) with exact stored points, so it
    /// round-trips. Loaded geometry is finalised: metre scale set, nothing to
    /// dimension. Living areas and wall-exposure sections are reconstructed
    /// from their resolved regions. Everything else (active shape, edit
    /// cursor, pending arc) starts fresh.
    pub fn from_storey(storey: &Storey) -> StoreyDoc {
        let mut doc = StoreyDoc::default();
        if let Some(ring) = storey.floors.footprint_ring() {
            let primary: Vec<Point2f32> = ring.into_iter().map(to_f32).collect();
            doc.floor_plan = FloorPlan::from_clean_ring(&primary).unwrap_or_default();
            doc.dimensioning = DimensionState {
                known_params: doc.floor_plan.all_active_params(),
                scale_set: true,
                ..Default::default()
            };
        }
        doc.outline = ShapeSlot {
            floor_plan: doc.floor_plan.clone(),
            dimensioning: doc.dimensioning.clone(),
        };
        doc.living_areas = reconstruct_living_areas(&storey.living_areas);
        doc.wall_sections = doc.sections_from_wall_regions(&storey.walls);
        doc
    }
}

/// Rebuild the runtime living-area slots from their resolved polygons on load.
/// Living areas are non-editable, so each is reconstructed from exact points
/// (`from_clean_ring`) with a COMPLETE dimensioning state (metre scale already
/// set, all params known) — they render and can be deleted, but aren't
/// re-dimensioned. Degenerate (<3-vertex) polygons are skipped.
pub fn reconstruct_living_areas(areas: &FloorRegions) -> Vec<ShapeSlot> {
    areas
        .to_vertex_lists()
        .into_iter()
        .filter(|verts| verts.len() >= 3)
        .filter_map(|verts| {
            let pts: Vec<Point2f32> = verts
                .iter()
                .map(|p| Point2f32::new(p.x as f32, p.y as f32))
                .collect();
            let plan = FloorPlan::from_clean_ring(&pts).ok()?;
            let known_params = plan.all_active_params();
            Some(ShapeSlot {
                floor_plan: plan,
                dimensioning: DimensionState {
                    known_params,
                    scale_set: true,
                    ..Default::default()
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use datatypes::ber::common::ItemId;
    use domain::points::Point2f64;

    fn square_m(side: f64) -> Vec<Point2f64> {
        vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(side, 0.0),
            Point2f64::new(side, side),
            Point2f64::new(0.0, side),
        ]
    }

    #[test]
    fn reconstruct_living_areas_skips_degenerate_polygons() {
        let areas = FloorRegions::from_vertex_lists(
            vec![square_m(4.0), vec![Point2f64::new(0.0, 0.0)]],
            ItemId::default(),
        );
        let slots = reconstruct_living_areas(&areas);
        assert_eq!(slots.len(), 1);
        let slot = &slots[0];
        assert_eq!(slot.floor_plan.get_points().len(), 4);
        // Complete dimensioning state: renders, but never re-prompted.
        assert!(slot.dimensioning.scale_set);
        assert!(slot.dimensioning.current_prompt.is_none());
        assert_eq!(
            slot.dimensioning.known_params,
            slot.floor_plan.all_active_params()
        );
    }

    #[test]
    fn from_storey_round_trips_a_square_outline() {
        let storey = Storey {
            floors: FloorRegions::from_vertex_lists(vec![square_m(5.0)], ItemId::default()),
            ..Storey::default()
        };
        let doc = StoreyDoc::from_storey(&storey);
        let pts = doc.floor_plan.get_points();
        assert_eq!(pts.len(), 4);
        assert!(pts
            .iter()
            .any(|p| (p.x - 5.0).abs() < 1e-6 && (p.y - 5.0).abs() < 1e-6));
        // Loaded geometry is finalised; the outline slot mirrors the buffer.
        assert!(doc.dimensioning.scale_set);
        assert_eq!(doc.outline.floor_plan.get_points(), pts);
    }

    #[test]
    fn from_storey_with_no_floor_is_empty() {
        let doc = StoreyDoc::from_storey(&Storey::default());
        assert!(doc.floor_plan.get_points().is_empty());
        assert!(!doc.dimensioning.scale_set);
        assert!(doc.living_areas.is_empty());
    }
}
