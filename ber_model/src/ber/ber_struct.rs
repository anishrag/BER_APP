use serde::{Deserialize, Serialize};

use super::common::{ConstructionPhases, ItemId, Photo, Year};
use super::fenestration::{Frame, Glazing, WindowNameSpecifics, WindowType};
use super::structural::{
    CeilingConstruction, FloorConstruction, RoofApex, RoofApexHint, Storey, ThermalMassInfo,
    WallConstruction,
};
use super::systems::{HeatingSystem, SolarElements, VentilationSystem};
use domain::points::Point2f64;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Ber {
    pub id: ItemId,
    pub address: String,
    pub eircode: String,
    pub construction_phases: ConstructionPhases,
    pub heating_system: HeatingSystem,
    pub ventilation_system: VentilationSystem,
    pub sheltered_sides: f32,
    pub number_of_storeys: i32,
    pub storeys: Vec<Storey>,
    /// Shared wall/floor/ceiling constructions ("a few per house"). Storey
    /// regions reference these by `construction_id`; grouping for DEAP export
    /// collapses regions onto their shared construction. `#[serde(default)]` so
    /// records written before the split still load.
    #[serde(default)]
    pub wall_constructions: Vec<WallConstruction>,
    #[serde(default)]
    pub floor_constructions: Vec<FloorConstruction>,
    #[serde(default)]
    pub ceiling_constructions: Vec<CeilingConstruction>,
    /// Shared window types (glazing/frame/year) — windows reference these by
    /// `window_type_id`, like wall regions reference constructions.
    /// `#[serde(default)]` so older records load.
    #[serde(default)]
    pub window_types: Vec<WindowType>,
    pub thermal_mass_info: ThermalMassInfo,
    pub orientation_deg: f32,
    pub solar: Vec<SolarElements>,
    pub photos: Vec<Photo>,
    /// Whether the assessor has completed the Stage-1 (Mapping) REVIEW — stepped
    /// through each external side's elevation against its photo and confirmed
    /// "all's good". This (not raw checklist completeness) is what unlocks the
    /// Structure phase. `#[serde(default)]` so older records load as un-reviewed.
    #[serde(default)]
    pub mapping_reviewed: bool,
    /// Whether the assessor has completed the Stage-2 (Structure) REVIEW — cycled
    /// through each construction group / window type / door, checking the details
    /// against the highlighted 3D surfaces. Recorded on confirm. `#[serde(default)]`
    /// so older records load as un-reviewed.
    #[serde(default)]
    pub structure_reviewed: bool,
    /// Phase-3 (Objects) review: the object categories the assessor has confirmed
    /// have NO items in the dwelling (e.g. "no cylinder", "no thermostat") — the
    /// review prompts for each empty category one at a time. `#[serde(default)]`
    /// so older records load with nothing confirmed.
    #[serde(default)]
    pub objects_no_items_confirmed: Vec<crate::ber::objects::ObjectCategory>,
}

// Pure data helpers, optional
impl Ber {
    pub fn new() -> Self {
        Self {
            number_of_storeys: 1,
            ..Default::default()
        }
    }

    /// The whole-building plan footprint: the UNION of every storey's floor
    /// footprint, returned as one ring (the largest connected component, open —
    /// no closing duplicate). `None` when no storey has floors. Used to clip a
    /// tool that spans the whole stack (the floor-exposure carve) to the ENTIRE
    /// building outline, not just one storey's — so a carve drawn over area that
    /// belongs to a different storey isn't clipped away.
    pub fn floors_footprint_union(&self) -> Option<Vec<Point2f64>> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        let to_poly = |ring: &[Point2f64]| {
            geo::Polygon::new(
                geo::LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                vec![],
            )
        };
        let mut acc: Option<geo::MultiPolygon<f64>> = None;
        for s in &self.storeys {
            let Some(ring) = s.floors.footprint_ring() else {
                continue;
            };
            if ring.len() < 3 {
                continue;
            }
            let poly = to_poly(&ring);
            acc = Some(match acc {
                None => geo::MultiPolygon::new(vec![poly]),
                Some(mp) => mp.union(&poly),
            });
        }
        let best = acc?.0.into_iter().max_by(|a, b| {
            a.unsigned_area()
                .partial_cmp(&b.unsigned_area())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let mut ring: Vec<Point2f64> = best
            .exterior()
            .coords()
            .map(|c| Point2f64::new(c.x, c.y))
            .collect();
        if ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        (ring.len() >= 3).then_some(ring)
    }

    /// Find-or-create a shared `WindowType` for the full identity
    /// `(glazing, frame, year, certified, gap, name)` and return its id — the
    /// window-type analogue of `ensure_*_construction`, so identical windows share
    /// one type. Every component is part of the identity: in particular a `name`
    /// distinguishes two otherwise-identical models, and `certified` separates a
    /// certified from an uncertified type.
    pub fn ensure_window_type(
        &mut self,
        glazing: Glazing,
        frame: Frame,
        year: Year,
        certified: bool,
        gap: Vec<u32>,
        name: Option<String>,
    ) -> ItemId {
        let specifics = WindowNameSpecifics::from_name(name);
        if let Some(t) = self.window_types.iter().find(|t| {
            t.glazing == glazing
                && t.frame == frame
                && t.year == year
                && t.certified == certified
                && t.gap == gap
                && t.specifics == specifics
        }) {
            return t.id;
        }
        let id = ItemId::default();
        self.window_types.push(WindowType {
            id,
            year,
            glazing,
            gap,
            frame,
            certified,
            specifics,
            photos: vec![],
            notes: None,
        });
        id
    }

    /// Stamp the default ceiling height: on storey `storey_idx` when given, else
    /// on every storey that doesn't already have one (the all-storeys 3D prompt —
    /// one typed value fills the gaps, never overwrites a set storey).
    /// Caller has validated `height_m > 0`.
    pub fn set_default_ceiling_height(&mut self, storey_idx: Option<usize>, height_m: f64) {
        match storey_idx {
            Some(idx) => {
                if let Some(storey) = self.storeys.get_mut(idx) {
                    storey.ceiling_height_m = Some(height_m);
                }
            }
            None => {
                for storey in &mut self.storeys {
                    if storey.ceiling_height_m.is_none() {
                        storey.ceiling_height_m = Some(height_m);
                    }
                }
            }
        }
    }

    /// Stamp the floor-slab thickness on storey `storey_idx` (the add-storey
    /// prompt and the Heights re-edit). Validation — finite, ≥ 0, never
    /// storey 0 — happens upstream in the controller; this just stores the
    /// declarative input [`storey_floor_datums`] consumes. No-op when the
    /// storey doesn't exist.
    ///
    /// [`storey_floor_datums`]: Ber::storey_floor_datums
    pub fn set_floor_thickness(&mut self, storey_idx: usize, thickness_m: f64) {
        if let Some(storey) = self.storeys.get_mut(storey_idx) {
            storey.floor_thickness_m = Some(thickness_m);
        }
    }

    /// Absolute floor datum (z, metres) per storey, index-aligned with
    /// `storeys`. THE datum rule:
    ///
    /// - `z_0 = 0`;
    /// - `z_N = z_{N−1} + step`, where the step depends on storey N's
    ///   `floor_thickness_m`:
    ///   - `None` (legacy records, storey 0-era storeys): the storey
    ///     BELOW's `ceiling_height_m` — byte-identical to the original
    ///     full-house ceiling-height stacking;
    ///   - `Some(t)`: `anchor_z + t`, where `anchor_z` is the LOWEST flat
    ///     ceiling height of the storey below over THIS storey's footprint
    ///     ([`CeilingRegions::lowest_flat_z_over`] — overlap above sliver
    ///     area), falling back to the storey below's `ceiling_height_m`
    ///     when this storey has no footprint yet or no flat ceiling covers
    ///     it.
    ///
    /// Unset ceiling heights count 0 in both branches (`unwrap_or_default`,
    /// exactly like the stacking this replaces) — callers gate on heights
    /// being set (the 3D gate) before trusting the stack.
    ///
    /// [`CeilingRegions::lowest_flat_z_over`]:
    /// super::structural::CeilingRegions::lowest_flat_z_over
    pub fn storey_floor_datums(&self) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.storeys.len());
        let mut z = 0.0_f64;
        for (i, storey) in self.storeys.iter().enumerate() {
            if i > 0 {
                let below = &self.storeys[i - 1];
                let below_height = below.ceiling_height_m.unwrap_or_default();
                z += match storey.floor_thickness_m {
                    None => below_height,
                    Some(t) => {
                        let anchor = storey
                            .floors
                            .footprint_ring()
                            .and_then(|fp| below.roofs.lowest_flat_z_over(&fp))
                            .unwrap_or(below_height);
                        anchor + t
                    }
                };
            }
            out.push(z);
        }
        out
    }

    /// Absolute Z of the highest ceiling reachable straight UP from the wall at
    /// (storey `idx`, plan position `along_m`) through the CONTIGUOUS run of
    /// vertically-aligned, SAME-TYPE walls — same `construction_id` AND
    /// `exposure`, and the same plan base-edge on the storey above. A multi-storey
    /// window on that wall may rise no higher than this. The run stops at the
    /// first storey above with no plan-aligned wall (a setback) or a different
    /// type — which is exactly "the assessor must draw two separate windows".
    pub fn wall_run_top_z(
        &self,
        idx: usize,
        wall: &crate::ber::structural::WallRegion,
        along_m: f64,
    ) -> f64 {
        use domain::points::Point3f64;
        let datums = self.storey_floor_datums();
        let datum = |i: usize| datums.get(i).copied().unwrap_or(0.0);
        let mut top = datum(idx) + wall.top_z_at_along(along_m);
        // The window's plan position as a WORLD point on the host wall's base
        // line. Each wall above is sampled by re-projecting THIS point onto its
        // own frame — so a wall stored in the opposite base-edge orientation
        // (the upper storey traces its footprint independently) is sampled at the
        // same physical place, not its mirror image.
        let Some((origin, (ux, uy), _len, _bz)) = wall.base_frame() else {
            return top;
        };
        let (wpx, wpy) = (origin.x + ux * along_m, origin.y + uy * along_m);
        let Some((mut ea, mut eb)) = wall.base_edge() else {
            return top;
        };
        let (want_cid, want_exp) = (wall.construction_id, wall.exposure);
        let same_edge = |w: &crate::ber::structural::WallRegion, a: Point3f64, b: Point3f64| {
            let Some((wa, wb)) = w.base_edge() else {
                return false;
            };
            let close = |p: Point3f64, q: Point3f64| (p.x - q.x).hypot(p.y - q.y) < 1e-2;
            (close(wa, a) && close(wb, b)) || (close(wa, b) && close(wb, a))
        };
        for s in (idx + 1)..self.storeys.len() {
            let Some(up) = self.storeys[s]
                .walls
                .regions
                .iter()
                .find(|w| same_edge(w, ea, eb))
            else {
                break; // setback / no wall above → run ends here
            };
            if up.construction_id != want_cid || up.exposure != want_exp {
                break; // different wall type → can't span further
            }
            // Project the world plan point onto THIS wall's own base frame.
            if let Some((uo, (vx, vy), _, _)) = up.base_frame() {
                let d = (wpx - uo.x) * vx + (wpy - uo.y) * vy;
                top = top.max(datum(s) + up.top_z_at_along(d));
            }
            if let Some((na, nb)) = up.base_edge() {
                (ea, eb) = (na, nb);
            }
        }
        top
    }

    /// Append a roof apex to storey `storey_idx`'s durable apex list.
    /// Validation (position strictly inside a ceiling region, sane height)
    /// happens upstream in the document layer at assign time — this just
    /// stores the declarative input that resolve re-applies. No-op when the
    /// storey doesn't exist.
    ///
    /// The position is QUANTIZED to the f32 grid: the reload reconstruction
    /// round-trips roof rings through f32, and `with_apexes`' exact vertex
    /// matching only holds when the stored apex survives that round-trip.
    /// Shell-originated coordinates already do; this makes the fixpoint hold
    /// for ANY caller (imports, fixtures) by construction.
    pub fn add_roof_apex(&mut self, storey_idx: usize, mut apex: RoofApex) {
        apex.point = RoofApex::quantize_point(apex.point);
        if let Some(storey) = self.storeys.get_mut(storey_idx) {
            storey.roof_apexes.push(apex);
        }
    }

    /// Record a side hint for a boundary apex (a mono-pitch ridge along an
    /// internal junction): `apex_point` is matched to a stored apex; `region` is
    /// a point inside the area it should slope. The apex point is quantized to
    /// match `add_roof_apex`'s storage so the hint keys correctly.
    pub fn push_roof_apex_hint(
        &mut self,
        storey_idx: usize,
        apex_point: Point2f64,
        region: (f32, f32),
    ) {
        let q = RoofApex::quantize_point(apex_point);
        if let Some(storey) = self.storeys.get_mut(storey_idx) {
            storey.roof_apex_hints.push(RoofApexHint {
                apex: (q.x as f32, q.y as f32),
                region,
            });
        }
    }

    /// Remove the roof apex at `apex_idx` on storey `storey_idx` (the apex
    /// list in the toolbar deletes by index). No-op when either index is out
    /// of range.
    pub fn remove_roof_apex(&mut self, storey_idx: usize, apex_idx: usize) {
        if let Some(storey) = self.storeys.get_mut(storey_idx) {
            if apex_idx < storey.roof_apexes.len() {
                storey.roof_apexes.remove(apex_idx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::structural::{Opes, Polygon3D, Storey, WallExposure, WallRegion, WallRegions};
    use domain::points::Point3f64;

    fn p3(x: f64, y: f64, z: f64) -> Point3f64 {
        Point3f64::new(x, y, z)
    }

    fn wall(cid: ItemId, exposure: WallExposure, ext: Vec<Point3f64>) -> WallRegion {
        WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(ext),
            exposure,
            opes: Opes::default(),
            construction_id: cid,
        }
    }

    /// The run bound samples each wall above at the SAME physical plan point even
    /// when that wall is stored in the opposite base-edge orientation (the upper
    /// storey traces its own footprint, so the direction isn't guaranteed). With
    /// a sloped upper wall, sampling the raw `along_m` would read the mirror end.
    #[test]
    fn wall_run_top_z_handles_a_reversed_upper_wall() {
        let cid = ItemId::default();
        let exp = WallExposure::Unset;
        // Lower: flat wall, base (0,0)→(4,0), top 2.4. Window at along 0.5.
        let lower = wall(
            cid,
            exp,
            vec![
                p3(0.0, 0.0, 0.0),
                p3(4.0, 0.0, 0.0),
                p3(4.0, 0.0, 2.4),
                p3(0.0, 0.0, 2.4),
            ],
        );
        // Upper: SAME edge but stored REVERSED — base (4,0)→(0,0) — and sloped so
        // the top is 3.0 above world x=0 and 1.0 above world x=4. At world x=0.5
        // the top is 3.0 − 2.0·(0.5/4) = 2.75; with datum 2.4 that's 5.15.
        let upper = wall(
            cid,
            exp,
            vec![
                p3(4.0, 0.0, 0.0),
                p3(0.0, 0.0, 0.0),
                p3(0.0, 0.0, 3.0),
                p3(4.0, 0.0, 1.0),
            ],
        );
        let storey = |w: WallRegion, h: f64| Storey {
            walls: WallRegions { regions: vec![w] },
            ceiling_height_m: Some(h),
            ..Storey::default()
        };
        let ber = Ber {
            storeys: vec![storey(lower, 2.4), storey(upper, 2.4)],
            ..Ber::default()
        };
        let wall0 = &ber.storeys[0].walls.regions[0];
        let top = ber.wall_run_top_z(0, wall0, 0.5);
        // Correct (orientation-aware) answer is 5.15; the raw-`along_m` bug gave 3.65.
        assert!((top - 5.15).abs() < 1e-3, "run top = {top}, expected 5.15");
    }
}
