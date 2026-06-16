use serde::{Deserialize, Serialize};

use super::common::{ConstructionPhases, ItemId, Photo, Year};
use super::fenestration::{Frame, Glazing, WindowNameSpecifics, WindowType};
use super::structural::{
    CeilingConstruction, FloorConstruction, RoofApex, Storey, ThermalMassInfo, WallConstruction,
};
use super::systems::{HeatingSystem, SolarElements, VentilationSystem};

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
}

// Pure data helpers, optional
impl Ber {
    pub fn new() -> Self {
        Self {
            number_of_storeys: 1,
            ..Default::default()
        }
    }

    /// Find-or-create a shared `WindowType` for `(glazing, frame, year)` and
    /// return its id — the window-type analogue of `ensure_*_construction`, so
    /// identical windows share one type.
    pub fn ensure_window_type(&mut self, glazing: Glazing, frame: Frame, year: Year) -> ItemId {
        if let Some(t) = self.window_types.iter().find(|t| {
            t.glazing == glazing
                && t.frame == frame
                && t.year == year
                && t.specifics == WindowNameSpecifics::Standard
        }) {
            return t.id;
        }
        let id = ItemId::default();
        self.window_types.push(WindowType {
            id,
            year,
            glazing,
            gap: vec![],
            frame,
            specifics: WindowNameSpecifics::Standard,
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
