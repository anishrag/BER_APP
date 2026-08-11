use serde::{Deserialize, Serialize};

use super::common::{ConstructionPhase, ConstructionPhases, Evidence, ExternalSide, ItemId, Year};
use super::fenestration::{Frame, Glazing, WindowNameSpecifics, WindowType};
use super::structural::{
    CeilingApex, CeilingApexHint, CeilingConstruction, CeilingContinuation, CeilingRegions, Dormer,
    FloorConstruction, Storey, ThermalMassInfo, WallConstruction,
};
use super::systems::{HeatingSystem, SolarElements, VentilationSystem};
use domain::points::Point2f64;

/// Client-provided answers from the booking/intake form, surfaced on the Key
/// Details screen. A staging area: the assessor reads/edits these and (for now)
/// transcribes them into the DEAP model by hand — they are NOT yet wired into
/// `heating_system`, `window_types`, constructions, etc. Populated externally at
/// intake (like address/eircode). `#[serde(default)]` on the `Ber` field keeps
/// older records loading.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IntakeDetails {
    pub property_type: String,
    pub heating_system: String,
    pub window_year: Option<Year>,
    /// The client marked the window-installation year UNKNOWN (the Key-Details
    /// "unknown" checkbox) — distinguishes a deliberate "don't know" from a
    /// blank `window_year`. `#[serde(default)]` → `false` on older records.
    #[serde(default)]
    pub window_year_unknown: bool,
    pub door_year: Option<Year>,
    pub insulation_walls: bool,
    pub insulation_roof: bool,
    pub insulation_floor: bool,
    pub insulation_notes: String,
    pub mprn: String,
    pub reason: String,
    // --- On-site questions (assessor asks the client at the property, not from
    // the booking form). `None` = not answered. Asked conditionally on Key
    // Details; the answers feed the post-survey follow-up email (which evidence
    // to request). `#[serde(default)]` keeps older records loading.
    /// Insulation works present: can the builder provide a signed letter?
    #[serde(default)]
    pub builder_letter_possible: Option<bool>,
    /// Extension present: is a letter from the architect available for it?
    #[serde(default)]
    pub architect_letter_extension: Option<bool>,
    /// Recent (2010+) build: is an architect letter possible for the original build?
    #[serde(default)]
    pub architect_letter_original: Option<bool>,
    /// Works done in the house: is a letter from the BUILDER available for them?
    /// (Asked below the architect questions.) `#[serde(default)]` keeps older
    /// records loading.
    #[serde(default)]
    pub builder_letter_works: Option<bool>,
    /// Insulation works present: does the client have invoices/photos of the
    /// works being done that show the materials used and the areas insulated?
    /// (The 2-proof works-evidence fallback when no builder letter.)
    /// `#[serde(default)]` keeps older records loading.
    #[serde(default)]
    pub works_evidence_available: Option<bool>,
    /// The client's signed Letter of Engagement, linked by BERintake once
    /// signed (`file_path` holds its S3 URL; `content_type` "application/pdf").
    /// Populated from the booking, so it evidences the build years the client
    /// gave — the first proof for every booking-sourced construction phase
    /// (see [`Ber::attach_loe_to_booking_phases`]). `None` until one exists.
    #[serde(default)]
    pub letter_of_engagement: Option<Evidence>,
}

/// One construction-PHASE zone of a storey, projected for the UI to tint and
/// label ([`Ber::storey_phase_zone_polygons`]). A connected same-phase floor
/// component: its `exterior` ring, its `holes` (the footprints of NEWER
/// overlapping phases the latest-wins partition carved out — a donut), and a
/// `label_anchor` guaranteed inside the material and OUTSIDE every hole. All
/// world-metre; rings are open (no repeated closing vertex).
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseZoneDisplay {
    pub phase: ConstructionPhase,
    pub exterior: Vec<Point2f64>,
    pub holes: Vec<Vec<Point2f64>>,
    pub label_anchor: Point2f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Ber {
    pub id: ItemId,
    pub address: String,
    pub eircode: String,
    /// The client's name from the booking (intake sync seeds it) — personalizes
    /// the follow-up letter's greeting. Empty on records imported before the
    /// field existed (the greeting then falls back to a bare "Hi,").
    #[serde(default)]
    pub client_name: String,
    /// Client booking-form answers not yet mapped into the DEAP model, shown on
    /// the Key Details screen for the assessor to confirm/fill. `#[serde(default)]`
    /// so older records load.
    #[serde(default)]
    pub intake: IntakeDetails,
    pub construction_phases: ConstructionPhases,
    pub heating_system: HeatingSystem,
    pub ventilation_system: VentilationSystem,
    /// Office-resolved solar PV (rating + documents) — see
    /// [`SolarPvResolution`](crate::ber::systems::SolarPvResolution).
    #[serde(default)]
    pub solar_pv: crate::ber::systems::SolarPvResolution,
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
    pub photos: Vec<Evidence>,
    /// External-photo sides whose shot the assessor DEFERRED to the end of
    /// the assessment (apartments: which unit is which is often only clear
    /// once you've been inside). A deferred side counts as SET for the
    /// Mapping completeness gates; the Final phase re-prompts for the
    /// deferred shots and blocks "BER complete" until they're captured. Only
    /// [`ExternalSide::photo_deferrable`] sides belong here — Front never
    /// does (see that method for the domain rationale) — and the accessors
    /// normalize a rogue `Front` entry away rather than honouring it.
    /// `#[serde(default)]` so records written before this field load with
    /// nothing deferred.
    #[serde(default)]
    pub deferred_photo_sides: Vec<ExternalSide>,
    /// The dwelling ROOF — the external roof structure(s) over the top storey,
    /// SEPARATE from each storey's ceiling (`Storey::ceilings` / the roof-point
    /// apexes). Drawn area-by-area in the Phase-1 roof step; only modelled when it
    /// matters (dual attic insulation, solar on the roof). `#[serde(default)]` so
    /// older records load with no roof.
    #[serde(default)]
    pub roof: crate::ber::roof::Roof,
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
    /// The house-level ATTIC object bucket (Phase 3): objects recorded in the
    /// attic space (e.g. a heat-recovery unit). The attic isn't a storey, so
    /// the bucket lives here, not on a `Storey::object_rooms` — it has no plan
    /// pin and never renders on a storey plan. Created on demand by the
    /// Objects-stage "Attic" button; dropped again if dismissed empty.
    /// `#[serde(default)]` so older records load without one.
    #[serde(default)]
    pub attic_room: Option<crate::ber::objects::ObjectRoom>,
    /// Phase-3 review: the assessor confirmed there is NOTHING in the attic
    /// ("Anything in the attic?" → Yes, none) — the attic counterpart of
    /// [`objects_no_items_confirmed`](Self::objects_no_items_confirmed).
    /// `#[serde(default)]` so older records load unconfirmed.
    #[serde(default)]
    pub attic_no_items_confirmed: bool,
    /// Whether the assessor has confirmed the Phase-3 (Objects) review — walked
    /// the counts + confirmed the empty categories and pressed "all reviewed".
    /// Recorded on confirm. `#[serde(default)]` so older records load as un-reviewed.
    #[serde(default)]
    pub objects_reviewed: bool,
    /// LEGACY (deserialize-only) pre-refactor Phase-4 blob: floor / wall-finish
    /// / heating used to live here. Now migrated on load into
    /// [`thermal_mass_info`](Self::thermal_mass_info) (floor + wall finishes)
    /// and [`heating_system.distribution`](crate::ber::systems::HeatingSystem)
    /// (the heat emitter) by [`migrate_legacy`](Self::migrate_legacy), then
    /// dropped. `skip_serializing` so new saves never emit it; `default` so new
    /// records without the old key load fine.
    #[serde(default, rename = "final_details", skip_serializing)]
    pub legacy_final_details: Option<crate::ber::final_details::LegacyFinalDetails>,
    /// Phase-4 per-side sheltering (the [`crate::ber::common::ExternalSide`]s).
    #[serde(default)]
    pub sheltering: crate::ber::final_details::ShelteringBySide,
    /// North-up satellite/aerial image of the site, used in Phase 4 to align the
    /// plan to true north. POPULATED EXTERNALLY (alongside address/eircode), not
    /// by this app — read-only here. `None` until provided.
    #[serde(default)]
    pub site_satellite_image: Option<crate::ber::common::Evidence>,
    /// Whether the assessor has aligned the plan to the satellite image and
    /// confirmed (Phase-4 north step "OK"). `#[serde(default)]`.
    #[serde(default)]
    pub north_aligned: bool,
    /// Whether the assessor has completed the Phase-4 (Final) review.
    /// `#[serde(default)]` so older records load as un-reviewed.
    #[serde(default)]
    pub details_reviewed: bool,
    /// Post-survey client follow-up: which document/detail requests are ticked,
    /// the editable email draft, and whether it's already been sent (one-shot).
    /// Driven by the "Follow ups" tab. `#[serde(default)]` so older records load
    /// with nothing requested.
    #[serde(default)]
    pub followup: crate::ber::followup::FollowUpRequest,
}

// Pure data helpers, optional
impl Ber {
    /// Attach the client's Letter of Engagement as build-year proof on every
    /// booking-sourced construction phase (idempotent). The LoE is populated
    /// from the booking, so it evidences the build years the client gave — it
    /// counts as the first of the two proofs each such phase needs. Phases the
    /// assessor added during the survey aren't covered by the LoE and are left
    /// untouched. No-op when there is no LoE. Keyed by the LoE's `file_path`
    /// (its URL) so re-running never double-attaches.
    pub fn attach_loe_to_booking_phases(&mut self) {
        let Some(loe) = self.intake.letter_of_engagement.clone() else {
            return;
        };
        let phases: Vec<ConstructionPhase> = self
            .construction_phases
            .booking_sourced
            .iter()
            .filter(|(_, &sourced)| sourced)
            .map(|(p, _)| p.clone())
            .collect();
        for phase in phases {
            let already = self
                .construction_phases
                .year_evidence_of(&phase)
                .iter()
                .any(|e| e.file_path == loe.file_path);
            if !already {
                self.construction_phases
                    .add_year_evidence(phase, loe.clone());
            }
        }
    }

    /// Update a stored photo's file paths by `id`, wherever it lives in the
    /// model (external photos, window types, wall/floor/ceiling constructions,
    /// doors, and Objects-phase building objects). Returns `true` if a photo
    /// with that id was found. The controller uses this to apply a background
    /// recompress/thumbnail job to the provisional photo it attached up front —
    /// `false` means the photo was meanwhile removed (e.g. a failed opening
    /// placement), so the caller can clean up the produced files.
    pub fn set_photo_paths(
        &mut self,
        id: ItemId,
        file_path: String,
        thumbnail_path: Option<String>,
    ) -> bool {
        let mut found = false;
        let mut visit = |p: &mut Evidence| {
            if p.id == id {
                p.file_path = file_path.clone();
                p.thumbnail_path = thumbnail_path.clone();
                found = true;
            }
        };
        for p in &mut self.photos {
            visit(p);
        }
        for t in &mut self.window_types {
            for p in &mut t.photos {
                visit(p);
            }
        }
        for c in &mut self.wall_constructions {
            for p in &mut c.base.photos {
                visit(p);
            }
        }
        for c in &mut self.floor_constructions {
            for p in &mut c.base.photos {
                visit(p);
            }
        }
        for c in &mut self.ceiling_constructions {
            for p in &mut c.base.photos {
                visit(p);
            }
        }
        for s in &mut self.storeys {
            for d in &mut s.doors {
                for p in &mut d.photos {
                    visit(p);
                }
            }
            for room in &mut s.object_rooms {
                for o in &mut room.objects {
                    for p in &mut o.photos {
                        visit(p);
                    }
                }
            }
        }
        if let Some(room) = &mut self.attic_room {
            for o in &mut room.objects {
                for p in &mut o.photos {
                    visit(p);
                }
            }
        }
        found
    }

    /// EVERY photo the assessor attached, wherever it lives in the model — the
    /// same sites [`set_photo_paths`](Self::set_photo_paths) visits (external
    /// photos, window types, wall/floor/ceiling constructions, doors, and
    /// Objects-phase building objects). NOT the site satellite image: that's
    /// populated externally and already lives in the cloud. The cloud push uses
    /// this to know what to upload.
    pub fn all_photos(&self) -> Vec<&Evidence> {
        let mut out: Vec<&Evidence> = Vec::new();
        out.extend(self.photos.iter());
        out.extend(self.window_types.iter().flat_map(|t| t.photos.iter()));
        out.extend(
            self.wall_constructions
                .iter()
                .flat_map(|c| c.base.photos.iter()),
        );
        out.extend(
            self.floor_constructions
                .iter()
                .flat_map(|c| c.base.photos.iter()),
        );
        out.extend(
            self.ceiling_constructions
                .iter()
                .flat_map(|c| c.base.photos.iter()),
        );
        out.extend(
            self.storeys
                .iter()
                .flat_map(|s| s.doors.iter())
                .flat_map(|d| d.photos.iter()),
        );
        out.extend(
            self.object_rooms_iter()
                .flat_map(|room| room.objects.iter())
                .flat_map(|o| o.photos.iter()),
        );
        out
    }

    /// EVERY object room in the dwelling: each storey's plan/surface pins plus
    /// the house-level attic bucket. The house-wide object queries (recorded
    /// kinds, review totals, category presence) walk this so the attic counts
    /// like any room.
    pub fn object_rooms_iter(&self) -> impl Iterator<Item = &crate::ber::objects::ObjectRoom> {
        self.storeys
            .iter()
            .flat_map(|s| s.object_rooms.iter())
            .chain(self.attic_room.iter())
    }

    /// Whether object pin `room_id` sits OUTSIDE the dwelling — derived from
    /// its owning storey's footprint (see [`Storey::external_object_pin_ids`]).
    /// `false` for the attic bucket (no plan position) and unknown ids.
    pub fn object_pin_is_external(&self, room_id: crate::ber::common::ItemId) -> bool {
        self.storeys.iter().any(|s| {
            s.object_rooms.iter().any(|r| r.id == room_id)
                && s.external_object_pin_ids().contains(&room_id)
        })
    }

    /// Mutable [`object_rooms_iter`](Self::object_rooms_iter).
    pub fn object_rooms_iter_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut crate::ber::objects::ObjectRoom> {
        self.storeys
            .iter_mut()
            .flat_map(|s| s.object_rooms.iter_mut())
            .chain(self.attic_room.iter_mut())
    }

    /// The DISTINCT primary heat-source types implied by the placed OBJECTS
    /// (across every room + the attic), in fixed PRECEDENCE order: heat pump,
    /// gas boiler, oil boiler, electric (panel) heater. Each `Boiler(fuel)` maps
    /// to its boiler type and each `PanelHeater` to `ElectricHeater`; the set is
    /// deduplicated so re-syncs are deterministic. Chimneys are secondary-only
    /// and are NOT included here. Empty when no primary heat-source object
    /// exists.
    pub fn heating_source_candidates(&self) -> Vec<crate::ber::systems::HeatingSystemType> {
        use crate::ber::objects::{BoilerFuel, ObjectKind};
        use crate::ber::systems::HeatingSystemType;
        let (mut heat_pump, mut gas, mut oil, mut electric) = (false, false, false, false);
        for obj in self.object_rooms_iter().flat_map(|r| r.objects.iter()) {
            match obj.kind {
                ObjectKind::Boiler(BoilerFuel::HeatPump) => heat_pump = true,
                ObjectKind::Boiler(BoilerFuel::Gas) => gas = true,
                ObjectKind::Boiler(BoilerFuel::Oil) => oil = true,
                ObjectKind::PanelHeater => electric = true,
                _ => {}
            }
        }
        // Fixed precedence order → deterministic, deduplicated.
        [
            (heat_pump, HeatingSystemType::HeatPump),
            (gas, HeatingSystemType::GasBoiler),
            (oil, HeatingSystemType::OilBoiler),
            (electric, HeatingSystemType::ElectricHeater),
        ]
        .into_iter()
        .filter_map(|(present, ty)| present.then_some(ty))
        .collect()
    }

    /// The primary heating type implied by the placed heat-source OBJECTS — the
    /// highest-precedence candidate present (heat pump > gas > oil > electric).
    /// `None` when no heat-source object exists.
    pub fn primary_boiler_heating(&self) -> Option<crate::ber::systems::HeatingSystemType> {
        self.heating_source_candidates().into_iter().next()
    }

    /// The heat sources eligible to be the PRIMARY heating system. Boilers
    /// always qualify; an electric PANEL heater qualifies only once AFFIRMED —
    /// i.e. `system_type` is already `ElectricHeater`, set by the shell's
    /// "is the house heated by panel heaters?" answer. So a lone unaffirmed
    /// panel yields no primary candidate (the heat-source gate then blocks),
    /// and a panel alongside a boiler is left to `secondary_heating`. Panels
    /// still appear in [`heating_source_candidates`](Self::heating_source_candidates),
    /// which drives the secondary list.
    pub fn primary_heating_candidates(&self) -> Vec<crate::ber::systems::HeatingSystemType> {
        use crate::ber::systems::HeatingSystemType;
        let affirmed = self.heating_system.system_type == HeatingSystemType::ElectricHeater;
        self.heating_source_candidates()
            .into_iter()
            .filter(|c| *c != HeatingSystemType::ElectricHeater || affirmed)
            .collect()
    }

    /// Reflect the placed heat-source objects and heating controls into
    /// [`heating_system`](crate::ber::systems::HeatingSystem). Called by the
    /// shell after an object add/remove. IDEMPOTENT: running it twice yields the
    /// same result.
    ///
    /// - `system_type`: KEEP the current value if it's still one of the present
    ///   candidates (preserves the assessor's "which is primary?" / electric
    ///   choice across later object edits); else the precedence pick of the
    ///   candidate set; else `Invalid`.
    /// - `secondary_heating`: every distinct candidate type other than the
    ///   chosen primary (fixed order), plus a single `Other("Chimney")` when any
    ///   chimney object is present.
    /// - `controls.programmer`: any `Programmer` object present.
    /// - `controls.room_thermostat`: summed `count` over all `Thermostat`
    ///   objects.
    /// - `separate_hot_water`: any `Boiler` or `Programmer` object with
    ///   `separate_hw_control` set.
    ///
    /// Does NOT touch `controls.zones` or `year_installed` (owned by the
    /// controller's heating prompts, a later layer).
    pub fn sync_heating_system_from_objects(&mut self) {
        use crate::ber::objects::ObjectKind;
        use crate::ber::systems::HeatingSystemType;

        let all_sources = self.heating_source_candidates();
        let primary_candidates = self.primary_heating_candidates();

        // Keep the assessor's chosen primary when it's still a candidate;
        // otherwise the precedence pick; otherwise Invalid. Preservation is what
        // makes the multi-source case idempotent. A panel heater counts here
        // only once affirmed (see `primary_heating_candidates`), so a lone
        // unaffirmed panel resolves to Invalid and the shell's gate blocks.
        let primary = if primary_candidates.contains(&self.heating_system.system_type) {
            self.heating_system.system_type.clone()
        } else {
            primary_candidates
                .first()
                .cloned()
                .unwrap_or(HeatingSystemType::Invalid)
        };

        // Scan the controls / chimney / separate-HW facts in one pass.
        let (mut programmer, mut room_thermostat, mut chimney, mut separate_hw) =
            (false, 0u32, false, false);
        for obj in self.object_rooms_iter().flat_map(|r| r.objects.iter()) {
            match &obj.kind {
                ObjectKind::Programmer => {
                    programmer = true;
                    separate_hw |= obj.separate_hw_control;
                }
                ObjectKind::Boiler(_) => separate_hw |= obj.separate_hw_control,
                ObjectKind::Thermostat => room_thermostat += obj.count,
                ObjectKind::Chimney(_) => chimney = true,
                _ => {}
            }
        }

        // Secondary = distinct candidates minus the primary (fixed order), then
        // a single chimney entry if any chimney is present.
        let mut secondary: Vec<HeatingSystemType> =
            all_sources.into_iter().filter(|c| *c != primary).collect();
        if chimney {
            secondary.push(HeatingSystemType::Other("Chimney".to_string()));
        }

        self.heating_system.system_type = primary;
        self.heating_system.secondary_heating = secondary;
        self.heating_system.controls.programmer = programmer;
        self.heating_system.controls.room_thermostat = room_thermostat;
        self.heating_system.separate_hot_water = separate_hw;
    }

    /// Reflect placed centralised ventilation units into `ventilation_system`.
    /// Deterministic and idempotent: a unit present sets the mechanical type
    /// (MVHR wins over MEV if both are placed); no unit → Natural. Fired by the
    /// shell on object add/remove.
    pub fn sync_ventilation_system_from_objects(&mut self) {
        use crate::ber::objects::{FanType, ObjectKind};
        use crate::ber::systems::VentilationSystemType;

        // A CENTRAL UNIT fan pin means the ventilation is NOT natural;
        // no pin means it is. (Legacy VentilationUnit records — the retired
        // MEV/MVHR picker options — count the same way.)
        let central = self
            .object_rooms_iter()
            .flat_map(|r| r.objects.iter())
            .any(|obj| {
                matches!(
                    obj.kind,
                    ObjectKind::Fan(FanType::Central) | ObjectKind::VentilationUnit(_)
                )
            });
        self.ventilation_system.system_type = if central {
            VentilationSystemType::Mechanical
        } else {
            VentilationSystemType::Natural
        };
    }

    /// Whether `side`'s external photo is DEFERRED to the end of the
    /// assessment. `Front` is never deferred
    /// ([`ExternalSide::photo_deferrable`]) — a rogue stored `Front` entry is
    /// normalized to "not deferred" here rather than honoured.
    pub fn photo_deferred(&self, side: ExternalSide) -> bool {
        side.photo_deferrable() && self.deferred_photo_sides.contains(&side)
    }

    /// Set / clear `side`'s photo deferral (deduplicating). No-op for a
    /// non-deferrable side — Front, see [`ExternalSide::photo_deferrable`] for
    /// why the front is always takeable up-front.
    pub fn set_photo_deferred(&mut self, side: ExternalSide, deferred: bool) {
        if !side.photo_deferrable() {
            return;
        }
        self.deferred_photo_sides.retain(|s| *s != side);
        if deferred {
            self.deferred_photo_sides.push(side);
        }
    }

    /// The deferred sides still LACKING an external photo, in
    /// [`ExternalSide::ALL`] order — what the end-of-assessment prompt lists,
    /// and what blocks "BER complete" until captured (defer = postpone to the
    /// end, never skip).
    pub fn deferred_photos_pending(&self) -> Vec<ExternalSide> {
        ExternalSide::ALL
            .iter()
            .copied()
            .filter(|&s| self.photo_deferred(s))
            .filter(|&s| !self.photos.iter().any(|p| p.side == Some(s)))
            .collect()
    }

    /// The REQUIRED external sides ([`ExternalSide::photo_required`] — Front +
    /// Back) that still have neither a photo tagged with the side in
    /// [`photos`](Self::photos) nor a deferral
    /// ([`photo_deferred`](Self::photo_deferred)), in [`ExternalSide::ALL`]
    /// order — the detailed core behind the "External photos" Mapping gate
    /// (the gate is green exactly when this is empty), naming WHICH sides
    /// block it for the tappable checklist row.
    pub fn missing_external_photo_sides(&self) -> Vec<ExternalSide> {
        ExternalSide::ALL
            .iter()
            .copied()
            .filter(|s| s.photo_required())
            .filter(|&side| {
                !(self.photos.iter().any(|p| p.side == Some(side)) || self.photo_deferred(side))
            })
            .collect()
    }

    pub fn new() -> Self {
        Self {
            number_of_storeys: 1,
            ..Default::default()
        }
    }

    /// Migrate pre-refactor records: the floor/wall finishes and heating used to
    /// live in `final_details`; adopt them into their new homes if those are still
    /// empty, then drop the legacy blob (not re-serialized).
    pub fn migrate_legacy(&mut self) {
        use crate::ber::final_details::HeatingKind;
        if let Some(l) = self.legacy_final_details.take() {
            if self.thermal_mass_info.is_unset() {
                self.thermal_mass_info.floor = l.floor;
                self.thermal_mass_info.internal_wall = l.internal_wall;
                self.thermal_mass_info.external_wall = l.external_wall;
                self.thermal_mass_info.separating_wall = l.separating_wall;
            }
            if self.heating_system.distribution == HeatingKind::Unset {
                self.heating_system.distribution = l.heating;
            }
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
    /// certified from an uncertified type. `year_proof` is NOT identity — a
    /// "got proof" tick ORs onto whichever type the placement lands on (proof
    /// status never forks a group).
    #[allow(clippy::too_many_arguments)]
    pub fn ensure_window_type(
        &mut self,
        glazing: Glazing,
        frame: Frame,
        year: Year,
        certified: bool,
        draught_stripped: bool,
        year_proof: bool,
        gap: Vec<u32>,
        name: Option<String>,
    ) -> ItemId {
        let specifics = WindowNameSpecifics::from_name(name);
        if let Some(t) = self.window_types.iter_mut().find(|t| {
            t.glazing == glazing
                && t.frame == frame
                && t.year == year
                && t.certified == certified
                && t.draught_stripped == Some(draught_stripped)
                && t.gap == gap
                && t.specifics == specifics
        }) {
            t.year_proof |= year_proof;
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
            draught_stripped: Some(draught_stripped),
            year_proof,
            specifics,
            photos: vec![],
            notes: None,
            assessed_u_value: None,
            assessed_g_value: None,
            cert_skipped: false,
        });
        id
    }

    /// Rename the shared window TYPE `id` — the openings analogue of a
    /// structure-group rename (`set_construction_name`). NAME-INTEGRITY: the
    /// model name disambiguates otherwise-identical types, so an empty name is
    /// rejected (a type REQUIRES one — the placement box enforces the same on
    /// creation), and a name already held by ANOTHER type is rejected with a
    /// user-facing hint (renames stream per keystroke; the durable name keeps
    /// its last valid value while the assessor keeps typing). A missing `id`
    /// is a successful no-op.
    pub fn rename_window_type(&mut self, id: ItemId, name: &str) -> Result<(), String> {
        let n = name.trim();
        if n.is_empty() {
            return Err("a window type needs a model name".to_string());
        }
        if self
            .window_types
            .iter()
            .any(|t| t.id != id && t.specifics.name().map(str::trim) == Some(n))
        {
            return Err(format!(
                "\"{n}\" is already another window type's name — pick a different one"
            ));
        }
        if let Some(t) = self.window_types.iter_mut().find(|t| t.id == id) {
            t.specifics = WindowNameSpecifics::Custom(n.to_string());
        }
        Ok(())
    }

    /// Set the shared window TYPE `id`'s survey fields in one commit — the
    /// Structure-review GROUP edit: one write re-attributes every window of
    /// the type, since windows point at the type by id. If the edit makes the
    /// type identical to another (the full `ensure_window_type` identity,
    /// name included), the twin is ABSORBED into `id`: its windows and
    /// follow-up tick re-point here, its photos/notes/cert values carry over,
    /// and the twin is removed — the "identical attributes means the same
    /// group" rule types already follow on creation. Returns whether anything
    /// changed; a missing `id` is a no-op.
    #[allow(clippy::too_many_arguments)]
    pub fn update_window_type_fields(
        &mut self,
        id: ItemId,
        glazing: Glazing,
        frame: Frame,
        year: Year,
        certified: bool,
        draught_stripped: bool,
        year_proof: bool,
        gap: Vec<u32>,
    ) -> bool {
        let Some(t) = self.window_types.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        if t.glazing == glazing
            && t.frame == frame
            && t.year == year
            && t.certified == certified
            && t.draught_stripped == Some(draught_stripped)
            && t.year_proof == year_proof
            && t.gap == gap
        {
            return false;
        }
        t.glazing = glazing;
        t.frame = frame;
        t.year = year;
        t.certified = certified;
        t.draught_stripped = Some(draught_stripped);
        t.year_proof = year_proof;
        t.gap = gap;
        self.absorb_identical_window_type(id);
        true
    }

    /// Merge any OTHER window type whose full identity now equals `id`'s into
    /// `id` (see [`update_window_type_fields`](Self::update_window_type_fields)).
    /// At most one twin can exist: `ensure_window_type` never creates a second.
    fn absorb_identical_window_type(&mut self, id: ItemId) {
        let Some(t) = self.window_types.iter().find(|t| t.id == id) else {
            return;
        };
        let Some(pos) = self.window_types.iter().position(|o| {
            o.id != id
                && o.glazing == t.glazing
                && o.frame == t.frame
                && o.year == t.year
                && o.certified == t.certified
                && o.draught_stripped == t.draught_stripped
                && o.gap == t.gap
                && o.specifics == t.specifics
        }) else {
            return;
        };
        let twin = self.window_types.remove(pos);
        for s in &mut self.storeys {
            for w in &mut s.windows {
                if w.window_type_id == twin.id {
                    w.window_type_id = id;
                }
            }
        }
        // A follow-up tick on the twin moves over (without doubling up).
        let ticks = &mut self.followup.window_types;
        if ticks.contains(&twin.id) {
            ticks.retain(|x| *x != twin.id);
            if !ticks.contains(&id) {
                ticks.push(id);
            }
        }
        let survivor = self
            .window_types
            .iter_mut()
            .find(|t| t.id == id)
            .expect("survivor was found above");
        survivor.photos.extend(twin.photos);
        if survivor.notes.is_none() {
            survivor.notes = twin.notes;
        }
        if survivor.assessed_u_value.is_none() {
            survivor.assessed_u_value = twin.assessed_u_value;
        }
        if survivor.assessed_g_value.is_none() {
            survivor.assessed_g_value = twin.assessed_g_value;
        }
        // Proof status is NOT identity — either side having year proof means
        // the merged group has it.
        survivor.year_proof |= twin.year_proof;
    }

    /// Rename every DOOR whose `type_name` matches `old` (trimmed) to `new`,
    /// across all storeys — the door analogue of a group rename (doors group
    /// by their free-text type label, not a shared registry). An empty or
    /// unchanged `new` is a no-op. Returns how many doors were renamed.
    ///
    /// The follow-up tab's per-door-type ticks key by the SAME free-text name
    /// (`followup.door_types`), so a rename re-points any matching tick — a
    /// rename from the Structure review or the Follow-ups tab never orphans a
    /// ticked request.
    pub fn rename_door_type(&mut self, old: &str, new: &str) -> usize {
        let (old_t, new_t) = (old.trim(), new.trim());
        if new_t.is_empty() || old_t == new_t {
            return 0;
        }
        let mut renamed = 0;
        for s in &mut self.storeys {
            for d in &mut s.doors {
                if d.type_name.trim() == old_t {
                    d.type_name = new_t.to_string();
                    renamed += 1;
                }
            }
        }
        if renamed > 0 {
            let ticks = &mut self.followup.door_types;
            if ticks.iter().any(|n| n == old_t) {
                ticks.retain(|n| n != old_t);
                if !ticks.iter().any(|n| n == new_t) {
                    ticks.push(new_t.to_string());
                }
            }
        }
        renamed
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
    ///   - `Some(t)`: `datum_z + t`, where `datum_z` is the below-ceiling
    ///     level this storey's floor rests on: its own
    ///     [`Storey::floor_datum_z_m`] when the seed asked and got an
    ///     answer, else the LARGEST-AREA flat ceiling of the storey below
    ///     over THIS storey's footprint
    ///     ([`CeilingRegions::datum_flat_z_over`] — overlap above sliver
    ///     area), falling back to the storey below's `ceiling_height_m`
    ///     when this storey has no footprint yet or no flat ceiling covers
    ///     it.
    ///
    /// The datum MUST agree with whatever
    /// [`CeilingRegions::inherited_floor_pieces`] measured the floor
    /// regions' `level_m` against — the two are halves of one convention
    /// (`walking surface = datum + level_m`), so a storey whose levels were
    /// seeded against a chosen datum stores that choice in
    /// [`Storey::floor_datum_z_m`] and this reads it back.
    ///
    /// Unset ceiling heights count 0 in both branches (`unwrap_or_default`,
    /// exactly like the stacking this replaces) — callers gate on heights
    /// being set (the 3D gate) before trusting the stack.
    ///
    /// [`resolve_floor_levels`]: super::structural::resolve_floor_levels
    /// [`CeilingRegions::inherited_floor_pieces`]:
    /// super::structural::CeilingRegions::inherited_floor_pieces
    /// [`Storey::floor_datum_z_m`]: super::structural::Storey::floor_datum_z_m
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
                        let datum = storey.floor_datum_z_m.or_else(|| {
                            storey.floors.footprint_ring().and_then(|fp| {
                                super::structural::resolve_floor_levels(
                                    &below.ceilings.inherited_floor_pieces(&fp),
                                    &[],
                                )
                                .0
                            })
                        });
                        datum.unwrap_or(below_height) + t
                    }
                };
            }
            out.push(z);
        }
        out
    }

    /// Storey `idx`'s construction-PHASE zones: its floor regions grouped by
    /// the phase their `construction_id` resolves to
    /// ([`FloorRegions::phase_zone_rings`] with this `Ber`'s floor-construction
    /// registry as the resolver), one open ring per connected component per
    /// phase. `Unassigned` zones (the implicit untagged remainder, and any
    /// region whose id doesn't resolve) are skipped; empty when `idx` is out of
    /// range or nothing is tagged. Pure projection — no mutation.
    ///
    /// Parallels `document::phase::phase_regions_from_floors`, which does the
    /// same derivation but projects to the session's `f32` `PhaseRegion`s;
    /// this stays at `f64` for data-model consumers (e.g. the upper-storey
    /// outline suggestions).
    ///
    /// [`FloorRegions::phase_zone_rings`]:
    /// super::structural::FloorRegions::phase_zone_rings
    pub fn storey_phase_zones(&self, idx: usize) -> Vec<(ConstructionPhase, Vec<Point2f64>)> {
        let Some(storey) = self.storeys.get(idx) else {
            return Vec::new();
        };
        storey
            .floors
            .phase_zone_rings(|cid| {
                self.floor_construction(cid)
                    .map(|c| c.base.construction_phase.clone())
                    .unwrap_or(ConstructionPhase::Unassigned)
            })
            .into_iter()
            .filter(|(phase, _)| *phase != ConstructionPhase::Unassigned)
            .collect()
    }

    /// Storey `below_idx`'s phase zones EXTENDED to cover `footprint` — the
    /// zones a storey ABOVE should inherit.
    ///
    /// A storey may reach past the one below (an OVERHANG — classically a
    /// bridged front-door recess), and the zones, being the below storey's own
    /// floor area, cannot reach there. That leftover would be phase-UNASSIGNED,
    /// which carves it out of the upper storey's floors AND ceilings as a
    /// region of its own with its own construction — a spurious division on a
    /// surface that is one continuous build (field: 56 Elmwood Road, a 1.57 m²
    /// slice of ceiling split off over the bridged recess).
    ///
    /// So each leftover piece is absorbed into the zone it shares the most
    /// plan BOUNDARY with — the same "copy the neighbour you mostly run along"
    /// rule the floor levels use for the very same piece. A piece adjoining no
    /// zone is left out (genuinely unassigned). Rings come back open, one per
    /// connected component per phase, as [`storey_phase_zones`] returns them.
    ///
    /// [`storey_phase_zones`]: Self::storey_phase_zones
    pub fn storey_phase_zones_over(
        &self,
        below_idx: usize,
        footprint: &[Point2f64],
    ) -> Vec<(ConstructionPhase, Vec<Point2f64>)> {
        use super::structural::{dominant_bordering, open_ring_of, ring_to_poly, split_to_simple};
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        let zones = self.storey_phase_zones(below_idx);
        if zones.is_empty() || footprint.len() < 3 {
            return zones;
        }
        // Accumulate per PHASE — several components of one phase are one zone
        // for the purpose of absorbing a neighbour.
        let mut acc: Vec<(ConstructionPhase, geo::MultiPolygon<f64>)> = Vec::new();
        for (phase, ring) in &zones {
            let poly = geo::MultiPolygon::new(vec![ring_to_poly(ring)]);
            match acc.iter_mut().find(|(p, _)| p == phase) {
                Some((_, mp)) => *mp = mp.union(&poly),
                None => acc.push((phase.clone(), poly)),
            }
        }
        let covered = acc
            .iter()
            .fold(geo::MultiPolygon::new(Vec::new()), |a, (_, mp)| a.union(mp));
        let fp = geo::MultiPolygon::new(vec![ring_to_poly(footprint)]);
        for piece in fp.difference(&covered).0.iter().flat_map(split_to_simple) {
            if piece.unsigned_area() < super::structural::SLIVER_AREA_M2 {
                continue;
            }
            let borders: Vec<&geo::MultiPolygon<f64>> = acc.iter().map(|(_, mp)| mp).collect();
            let Some(k) = dominant_bordering(&piece, &borders) else {
                continue; // adjoins no zone — genuinely unassigned
            };
            let mp = &mut acc[k].1;
            *mp = mp.union(&geo::MultiPolygon::new(vec![piece]));
        }
        acc.into_iter()
            .flat_map(|(phase, mp)| {
                mp.0.iter()
                    .flat_map(split_to_simple)
                    .filter(|p| p.unsigned_area() >= super::structural::SLIVER_AREA_M2)
                    .map(|p| (phase.clone(), open_ring_of(&p)))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Storey `idx`'s construction-PHASE zones projected for DISPLAY: like
    /// [`storey_phase_zones`](Self::storey_phase_zones) but HOLE-PRESERVING and
    /// with a guaranteed-interior label anchor, so the phase tint / label respect
    /// a donut zone (an older phase a newer overlapping one carved the middle out
    /// of). One [`PhaseZoneDisplay`] per connected same-phase component; each
    /// carries its exterior ring, its hole rings, and a `label_anchor` that is
    /// inside the material and OUTSIDE every hole. `Unassigned` zones (the
    /// untagged remainder / unresolved ids) are skipped; empty when `idx` is out
    /// of range or nothing is tagged. Pure projection — no mutation.
    pub fn storey_phase_zone_polygons(&self, idx: usize) -> Vec<PhaseZoneDisplay> {
        use geo::{Centroid, InteriorPoint, LineString};
        let Some(storey) = self.storeys.get(idx) else {
            return Vec::new();
        };
        // A geo ring → open world-metre ring (drop the repeated closing vertex).
        let ring_of = |ls: &LineString<f64>| -> Vec<Point2f64> {
            let mut pts: Vec<Point2f64> = ls.coords().map(|c| Point2f64::new(c.x, c.y)).collect();
            if pts.len() > 1 && pts.first() == pts.last() {
                pts.pop();
            }
            pts
        };
        storey
            .floors
            .phase_zone_polygons(|cid| {
                self.floor_construction(cid)
                    .map(|c| c.base.construction_phase.clone())
                    .unwrap_or(ConstructionPhase::Unassigned)
            })
            .into_iter()
            .filter(|(phase, _)| *phase != ConstructionPhase::Unassigned)
            .map(|(phase, poly)| {
                // A guaranteed-interior label point: geo's InteriorPoint lands
                // inside the polygon's material (respecting holes); on the rare
                // None fall back to the centroid, then to the origin.
                let anchor = poly
                    .interior_point()
                    .or_else(|| poly.centroid())
                    .map(|p| Point2f64::new(p.x(), p.y()))
                    .unwrap_or_else(|| Point2f64::new(0.0, 0.0));
                PhaseZoneDisplay {
                    phase,
                    exterior: ring_of(poly.exterior()),
                    holes: poly.interiors().iter().map(ring_of).collect(),
                    label_anchor: anchor,
                }
            })
            .collect()
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
    pub fn add_ceiling_apex(&mut self, storey_idx: usize, mut apex: CeilingApex) {
        apex.point = CeilingApex::quantize_point(apex.point);
        if let Some(storey) = self.storeys.get_mut(storey_idx) {
            storey.ceiling_apexes.push(apex);
        }
    }

    /// Record a side hint for a boundary apex (a mono-pitch ridge along an
    /// internal junction): `apex_point` is matched to a stored apex; `region` is
    /// a point inside the area it should slope. The apex point is quantized to
    /// match `add_ceiling_apex`'s storage so the hint keys correctly.
    pub fn push_ceiling_apex_hint(
        &mut self,
        storey_idx: usize,
        apex_point: Point2f64,
        region: (f32, f32),
    ) {
        let q = CeilingApex::quantize_point(apex_point);
        if let Some(storey) = self.storeys.get_mut(storey_idx) {
            storey.ceiling_apex_hints.push(CeilingApexHint {
                apex: (q.x as f32, q.y as f32),
                region,
            });
        }
    }

    /// Toggle plan point `p` in/out of storey `storey_idx`'s UNFANNED ceiling
    /// vertex set (`Storey::ceiling_unfanned` — tap-to-unfan). The point is
    /// QUANTIZED to the f32 grid like an apex (`CeilingApex::quantize_point`),
    /// and matched against the stored set within `UNFAN_MATCH_EPS_M` so a
    /// re-tap of a marker re-fans it even across the f32 round-trip. Returns
    /// `Some(true)` when the vertex is now unfanned, `Some(false)` when it was
    /// re-fanned, `None` when the storey doesn't exist. Validation (the point
    /// being a real fan-marker vertex) happens upstream in the document layer.
    pub fn toggle_ceiling_unfanned(&mut self, storey_idx: usize, p: Point2f64) -> Option<bool> {
        use super::structural::UNFAN_MATCH_EPS_M;
        let q = CeilingApex::quantize_point(p);
        let storey = self.storeys.get_mut(storey_idx)?;
        if let Some(i) = storey.ceiling_unfanned.iter().position(|&(x, y)| {
            ((x as f64) - q.x).abs() <= UNFAN_MATCH_EPS_M
                && ((y as f64) - q.y).abs() <= UNFAN_MATCH_EPS_M
        }) {
            storey.ceiling_unfanned.remove(i);
            Some(false)
        } else {
            storey.ceiling_unfanned.push((q.x as f32, q.y as f32));
            Some(true)
        }
    }

    /// Update storey `storey_idx`'s gable plane-anchor OVERRIDE set
    /// (`Storey::ceiling_plane_anchors`): drop every stored point matching
    /// one of `remove` (within `UNFAN_MATCH_EPS_M`), then push `add`
    /// (quantized like an apex) if given — the one-call form of "this corner
    /// now sets the pitch" (the removed points are the side's previous
    /// override) and "back to auto" (`add = None`). Validation (the point
    /// being a real eligible eave vertex) happens upstream in the document
    /// layer. `None` when the storey doesn't exist; `Some(changed)`
    /// otherwise.
    pub fn update_ceiling_plane_anchors(
        &mut self,
        storey_idx: usize,
        remove: &[Point2f64],
        add: Option<Point2f64>,
    ) -> Option<bool> {
        use super::structural::UNFAN_MATCH_EPS_M;
        let storey = self.storeys.get_mut(storey_idx)?;
        let before = storey.ceiling_plane_anchors.len();
        storey.ceiling_plane_anchors.retain(|&(x, y)| {
            !remove.iter().any(|r| {
                ((x as f64) - r.x).abs() <= UNFAN_MATCH_EPS_M
                    && ((y as f64) - r.y).abs() <= UNFAN_MATCH_EPS_M
            })
        });
        let mut changed = storey.ceiling_plane_anchors.len() != before;
        if let Some(p) = add {
            let q = CeilingApex::quantize_point(p);
            storey.ceiling_plane_anchors.push((q.x as f32, q.y as f32));
            changed = true;
        }
        Some(changed)
    }

    /// Append a CONTINUE-from ceiling surface link to storey `storey_idx`
    /// (see [`CeilingContinuation`]): the region containing `target` will be
    /// re-emitted at every recompose as the plane of the region containing
    /// `source` (on this storey for `offset` 0, the storey above/below for
    /// ±1 — cross-storey links store the neighbour storey's nearest LATERAL
    /// region anchor, never a point in a region that sits directly
    /// over/under the target: a plane continues sideways, not straight
    /// through the slab onto its own footprint —
    /// `CeilingRegions::nearest_lateral_anchor`, enforced by the shell's
    /// pick flow). Both points are
    /// QUANTIZED to the f32 grid like an apex. Validation (regions existing,
    /// distinct) happens upstream in the shell's pick flow; a link that goes
    /// stale is skipped at apply. No-op when the storey doesn't exist.
    pub fn add_ceiling_continuation(
        &mut self,
        storey_idx: usize,
        target: Point2f64,
        source: Point2f64,
        offset: i8,
    ) {
        let qt = CeilingApex::quantize_point(target);
        let qs = CeilingApex::quantize_point(source);
        if let Some(storey) = self.storeys.get_mut(storey_idx) {
            storey.ceiling_continuations.push(CeilingContinuation {
                target: (qt.x as f32, qt.y as f32),
                source: (qs.x as f32, qs.y as f32),
                source_storey_offset: offset,
            });
        }
    }

    /// Remove every continuation of storey `storey_idx` whose TARGET point is
    /// claimed by the same resolved ceiling region as `p` — the "tap an
    /// already-continued region to clear it" affordance. Returns how many
    /// entries were removed (0 when none matched / no storey).
    pub fn clear_ceiling_continuations_at(&mut self, storey_idx: usize, p: Point2f64) -> usize {
        let Some(storey) = self.storeys.get_mut(storey_idx) else {
            return 0;
        };
        let Some(&region) = storey.ceilings.claim_regions(p).first() else {
            return 0;
        };
        let ceilings = &storey.ceilings;
        let before = storey.ceiling_continuations.len();
        storey.ceiling_continuations.retain(|c| {
            let tp = Point2f64::new(c.target.0 as f64, c.target.1 as f64);
            ceilings.claim_regions(tp).first() != Some(&region)
        });
        before - storey.ceiling_continuations.len()
    }

    /// Apply storey `storey_idx`'s stored ceiling CONTINUATIONS onto
    /// `ceilings` — the shell's recompose calls this AFTER the base solve
    /// (default → steps → apexes → dormers) and BEFORE the structure
    /// face-splits, so a split carves the continued surface rather than
    /// re-deriving it. Per entry, in STORED order (so chains work — a later
    /// entry sourcing from an earlier target reads the already-continued
    /// plane):
    ///
    /// - the TARGET region is the one claiming `target` in the in-progress
    ///   `ceilings`;
    /// - the SOURCE plane is the region claiming `source` — in `ceilings`
    ///   for a same-storey link, or on the ±1 storey's STORED resolved
    ///   ceilings for a cross-storey one, converted through the storeys'
    ///   floor datums (world z → target-local z) so a slope continues
    ///   physically straight through;
    /// - the target is re-emitted as that plane over its own ring (fresh
    ///   id, default opes — the split-parity convention; construction +
    ///   exposure kept).
    ///
    /// A CYCLE (walking same-storey source→target provenance from this
    /// entry's source lands back inside this entry's own target) is warned
    /// and skipped. Entries whose points resolve to no region, to the SAME
    /// region, to a missing storey, or to a plan-degenerate source are
    /// skipped (stale-safe — the assessor may have reshaped everything
    /// since).
    pub fn apply_ceiling_continuations(
        &self,
        storey_idx: usize,
        mut ceilings: CeilingRegions,
    ) -> CeilingRegions {
        use super::structural::{region_plane, ring_to_poly, Polygon3D};
        use domain::points::Point3f64;
        use geo::Contains;
        let Some(storey) = self.storeys.get(storey_idx) else {
            return ceilings;
        };
        if storey.ceiling_continuations.is_empty() {
            return ceilings;
        }
        let datums = self.storey_floor_datums();
        // Same-storey provenance for the cycle walk: (target plan ring,
        // source point) per applied entry.
        let mut applied: Vec<(Vec<Point2f64>, Point2f64)> = Vec::new();
        for c in &storey.ceiling_continuations {
            let tp = Point2f64::new(c.target.0 as f64, c.target.1 as f64);
            let sp = Point2f64::new(c.source.0 as f64, c.source.1 as f64);
            let Some(&ti) = ceilings.claim_regions(tp).first() else {
                log::debug!("ceiling continuation: target {tp:?} resolves to no region — skipped");
                continue;
            };
            let target_ring: Vec<Point2f64> = ceilings.regions[ti]
                .polygon
                .exterior
                .iter()
                .map(|v| Point2f64::new(v.x, v.y))
                .collect();
            // The source plane, in TARGET-storey-local z.
            let plane = if c.source_storey_offset == 0 {
                match ceilings.claim_regions(sp).first() {
                    Some(&si) if si != ti => region_plane(&ceilings.regions[si]),
                    Some(_) => {
                        log::debug!(
                            "ceiling continuation: source {sp:?} is the target's own region — skipped"
                        );
                        continue;
                    }
                    None => None,
                }
            } else {
                let src_idx = storey_idx as i64 + c.source_storey_offset as i64;
                let src = usize::try_from(src_idx)
                    .ok()
                    .and_then(|i| self.storeys.get(i));
                src.and_then(|s| {
                    let &si = s.ceilings.claim_regions(sp).first()?;
                    let (c0, cx, cy) = region_plane(&s.ceilings.regions[si])?;
                    let shift = datums.get(src_idx as usize).copied().unwrap_or(0.0)
                        - datums.get(storey_idx).copied().unwrap_or(0.0);
                    Some((c0 + shift, cx, cy))
                })
            };
            let Some((c0, cx, cy)) = plane else {
                log::debug!("ceiling continuation: source {sp:?} resolves to no plane — skipped");
                continue;
            };
            // CYCLE GUARD: walk same-storey provenance from the source point;
            // reaching this entry's own target ring means the chain loops.
            let target_poly = ring_to_poly(&target_ring);
            let mut probe = sp;
            let mut cyclic = c.source_storey_offset == 0
                && target_poly.contains(&geo::Point::new(probe.x, probe.y));
            let mut hops = 0;
            while !cyclic && hops <= applied.len() {
                let Some((_, next)) = applied.iter().find(|(ring, _)| {
                    ring_to_poly(ring).contains(&geo::Point::new(probe.x, probe.y))
                }) else {
                    break;
                };
                probe = *next;
                cyclic = target_poly.contains(&geo::Point::new(probe.x, probe.y));
                hops += 1;
            }
            if cyclic {
                log::warn!("ceiling continuation {tp:?} ← {sp:?} closes a cycle — skipped");
                continue;
            }
            // Re-emit the target as the source plane over its own ring.
            let old = &ceilings.regions[ti];
            let verts: Vec<Point3f64> = target_ring
                .iter()
                .map(|p| Point3f64::new(p.x, p.y, c0 + cx * p.x + cy * p.y))
                .collect();
            ceilings.regions[ti] = super::structural::CeilingRegion {
                id: ItemId::default(),
                polygon: Polygon3D::new(verts),
                opes: Default::default(),
                construction_id: old.construction_id,
                exposure: old.exposure,
            };
            if c.source_storey_offset == 0 {
                applied.push((target_ring, sp));
            }
        }
        ceilings
    }

    /// Append a flat-topped DORMER to storey `storey_idx`'s durable dormer
    /// list (replayed at resolve by [`CeilingRegions::with_flat_patches`]).
    /// Each footprint vertex is QUANTIZED to the f32 grid, exactly as
    /// [`add_ceiling_apex`](Self::add_ceiling_apex) quantizes an apex point:
    /// the reload reconstruction round-trips roof rings through f32, and the
    /// derived dormer seams only line up when the stored footprint survives
    /// that round-trip. No-op when the storey doesn't exist.
    ///
    /// [`CeilingRegions::with_flat_patches`]: super::structural::CeilingRegions::with_flat_patches
    pub fn add_dormer(&mut self, storey_idx: usize, mut dormer: Dormer) {
        for p in &mut dormer.footprint {
            *p = CeilingApex::quantize_point(*p);
        }
        if let Some(storey) = self.storeys.get_mut(storey_idx) {
            storey.dormers.push(dormer);
        }
    }

    /// Remove the roof apex at `apex_idx` on storey `storey_idx` (the apex
    /// list in the toolbar deletes by index). No-op when either index is out
    /// of range.
    pub fn remove_roof_apex(&mut self, storey_idx: usize, apex_idx: usize) {
        if let Some(storey) = self.storeys.get_mut(storey_idx) {
            if apex_idx < storey.ceiling_apexes.len() {
                storey.ceiling_apexes.remove(apex_idx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::structural::{
        CeilingExposure, CeilingRegion, Opes, Polygon3D, Storey, WallExposure, WallRegion,
        WallRegions,
    };
    use domain::points::Point3f64;

    fn p3(x: f64, y: f64, z: f64) -> Point3f64 {
        Point3f64::new(x, y, z)
    }

    /// A bare window pointing at shared type `tid` — enough for the type-edit
    /// re-pointing tests.
    fn window_of_type(tid: ItemId) -> crate::ber::fenestration::Window {
        crate::ber::fenestration::Window {
            id: ItemId::default(),
            vertices: vec![],
            width_m: 1.0,
            height_m: 1.0,
            window_type_id: tid,
            placement: None,
            wall_anchor: None,
            assembly_id: None,
            multi_storey: false,
            shape: Default::default(),
            overshading: Default::default(),
            num_opes: None,
        }
    }

    /// The Structure-review GROUP edit: one `update_window_type_fields` call
    /// re-attributes the shared type (every window of it), and an unchanged
    /// call reports no change.
    #[test]
    fn update_window_type_fields_edits_the_shared_type() {
        let mut ber = Ber::default();
        let tid = ber.ensure_window_type(
            Glazing::Double,
            Frame::WoodUPVC,
            2013,
            false,
            true,
            false,
            vec![16],
            Some("nordan".to_string()),
        );
        // Tick certified + year proof + widen the gap, as one review commit.
        assert!(ber.update_window_type_fields(
            tid,
            Glazing::Double,
            Frame::WoodUPVC,
            2013,
            true,
            true,
            true,
            vec![20],
        ));
        let t = ber.window_types.iter().find(|t| t.id == tid).unwrap();
        assert!(t.certified);
        assert!(t.year_proof);
        assert_eq!(t.gap, vec![20]);
        assert_eq!(t.specifics.name(), Some("nordan"), "name untouched");
        // The identical commit again is a no-op.
        assert!(!ber.update_window_type_fields(
            tid,
            Glazing::Double,
            Frame::WoodUPVC,
            2013,
            true,
            true,
            true,
            vec![20],
        ));
    }

    /// An edit that makes a type IDENTICAL to another absorbs the twin —
    /// "identical attributes means the same group": the twin's windows and
    /// follow-up tick re-point to the edited type, its photos carry over, and
    /// the twin is gone.
    #[test]
    fn update_window_type_fields_absorbs_a_now_identical_twin() {
        let mut ber = Ber::default();
        // Same name is what `rename_window_type` forbids, so a real twin can
        // only be two UNNAMED (Standard) types differing on a survey field.
        let certified = ber.ensure_window_type(
            Glazing::Double,
            Frame::WoodUPVC,
            2013,
            true,
            true,
            false,
            vec![16],
            None,
        );
        let uncertified = ber.ensure_window_type(
            Glazing::Double,
            Frame::WoodUPVC,
            2013,
            false,
            true,
            false,
            vec![16],
            None,
        );
        assert_ne!(certified, uncertified, "distinct until the edit");
        ber.window_types
            .iter_mut()
            .find(|t| t.id == certified)
            .unwrap()
            .photos
            .push(crate::ber::common::Evidence::default());
        ber.storeys.push(Storey::default());
        ber.storeys[0].windows.push(window_of_type(certified));
        ber.storeys[0].windows.push(window_of_type(uncertified));
        ber.followup.window_types.push(certified);
        // The twin (already-certified type) has year proof; the edited one
        // doesn't — the merged group must keep it.
        ber.window_types
            .iter_mut()
            .find(|t| t.id == certified)
            .unwrap()
            .year_proof = true;
        // The review ticks certified on the uncertified type → identical twin.
        assert!(ber.update_window_type_fields(
            uncertified,
            Glazing::Double,
            Frame::WoodUPVC,
            2013,
            true,
            true,
            false,
            vec![16],
        ));
        assert_eq!(ber.window_types.len(), 1, "twin absorbed");
        let survivor = &ber.window_types[0];
        assert_eq!(survivor.id, uncertified, "the EDITED id survives");
        assert_eq!(survivor.photos.len(), 1, "twin's photos carried over");
        assert!(
            ber.storeys[0]
                .windows
                .iter()
                .all(|w| w.window_type_id == uncertified),
            "every window re-points to the survivor"
        );
        assert_eq!(
            ber.followup.window_types,
            vec![uncertified],
            "the follow-up tick moved over without doubling"
        );
        assert!(
            survivor.year_proof,
            "either side having year proof survives the merge"
        );
    }

    /// An OLD-shape record (floor/wall finishes + heating in `final_details`,
    /// default `thermal_mass_info`) migrates those values into their new homes
    /// on load, and the legacy blob is NOT re-serialized afterwards.
    #[test]
    fn migrate_legacy_adopts_final_details_and_drops_the_blob() {
        use crate::ber::final_details::{FloorKind, HeatingKind, WallFinish};
        // Build an old-shape blob: a default Ber's JSON (no `final_details`,
        // since it's skip_serializing) with the legacy key injected.
        let mut json = serde_json::to_value(Ber::default()).unwrap();
        json.as_object_mut().unwrap().insert(
            "final_details".to_string(),
            serde_json::json!({
                "floor": "Solid",
                "internal_wall": "HardPlaster",
                "separating_wall": "PlasterboardOnDotAndDab",
                "external_wall": "PlasterboardOnDotAndDab",
                "heating": "Radiators",
            }),
        );
        let mut ber: Ber = serde_json::from_value(json).expect("old record loads");
        assert!(ber.thermal_mass_info.is_unset(), "raw capture starts empty");
        ber.migrate_legacy();
        assert_eq!(
            ber.thermal_mass_info.external_wall,
            WallFinish::PlasterboardOnDotAndDab
        );
        assert_eq!(ber.thermal_mass_info.floor, FloorKind::Solid);
        assert_eq!(ber.thermal_mass_info.internal_wall, WallFinish::HardPlaster);
        assert_eq!(
            ber.thermal_mass_info.separating_wall,
            WallFinish::PlasterboardOnDotAndDab
        );
        assert_eq!(ber.heating_system.distribution, HeatingKind::Radiators);
        // Re-serializing must NOT emit a `final_details` key (dropped on save).
        let out = serde_json::to_value(&ber).unwrap();
        assert!(
            out.as_object().unwrap().get("final_details").is_none(),
            "legacy blob is not re-serialized"
        );
    }

    /// The unfan toggle quantizes, matches within the epsilon (so a
    /// re-tap re-fans across the f32 round-trip), and reports the new state.
    #[test]
    fn toggle_ceiling_unfanned_adds_then_removes() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());
        let p = Point2f64::new(6.0, 2.0);
        assert_eq!(ber.toggle_ceiling_unfanned(0, p), Some(true), "unfanned");
        assert_eq!(ber.storeys[0].ceiling_unfanned, vec![(6.0, 2.0)]);
        // A hair off (within the match epsilon) still toggles the SAME entry.
        let near = Point2f64::new(6.0 + 5e-5, 2.0);
        assert_eq!(
            ber.toggle_ceiling_unfanned(0, near),
            Some(false),
            "re-fanned"
        );
        assert!(ber.storeys[0].ceiling_unfanned.is_empty());
        assert_eq!(
            ber.toggle_ceiling_unfanned(7, p),
            None,
            "missing storey is a no-op"
        );
    }

    /// A ceiling region over the axis-aligned rect `x[x0, x1] × y[y0, y1]`
    /// with z linear in x: `z = z_at_x0 + (z_at_x1 − z_at_x0)·(x − x0)/(x1 − x0)`
    /// (flat when equal).
    fn slab(x0: f64, x1: f64, z_at_x0: f64, z_at_x1: f64) -> CeilingRegion {
        let z = |x: f64| z_at_x0 + (z_at_x1 - z_at_x0) * (x - x0) / (x1 - x0);
        CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(x0, 0.0, z(x0)),
                Point3f64::new(x1, 0.0, z(x1)),
                Point3f64::new(x1, 2.0, z(x1)),
                Point3f64::new(x0, 2.0, z(x0)),
            ]),
            opes: Opes::default(),
            construction_id: ItemId::default(),
            exposure: CeilingExposure::default(),
        }
    }

    /// The z of the vertex at `(x, y)` in the region containing `p`.
    fn z_at(regions: &CeilingRegions, p: Point2f64, x: f64, y: f64) -> f64 {
        regions.regions[*regions.claim_regions(p).first().expect("region")]
            .polygon
            .exterior
            .iter()
            .find(|v| (v.x - x).abs() <= 1e-9 && (v.y - y).abs() <= 1e-9)
            .unwrap_or_else(|| panic!("no vertex at ({x}, {y})"))
            .z
    }

    /// Continuations apply in STORED order (chains work), re-emitting each
    /// target as its source's plane; stale entries are skipped.
    #[test]
    fn ceiling_continuations_chain_and_skip_stale() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());
        // A x[0,2] SLOPED (z = 2 + 0.5x), B x[2,4] flat 2.4, C x[4,6] flat 2.6.
        let ceilings = CeilingRegions {
            regions: vec![
                slab(0.0, 2.0, 2.0, 3.0),
                slab(2.0, 4.0, 2.4, 2.4),
                slab(4.0, 6.0, 2.6, 2.6),
            ],
        };
        ber.add_ceiling_continuation(0, Point2f64::new(3.0, 1.0), Point2f64::new(1.0, 1.0), 0);
        ber.add_ceiling_continuation(0, Point2f64::new(5.0, 1.0), Point2f64::new(3.0, 1.0), 0);
        // Stale: resolves to no region — ignored.
        ber.add_ceiling_continuation(0, Point2f64::new(99.0, 99.0), Point2f64::new(1.0, 1.0), 0);
        let out = ber.apply_ceiling_continuations(0, ceilings);
        assert_eq!(out.regions.len(), 3, "re-emit, not re-split");
        // B continues A's plane (z = 2 + 0.5x): 3.0 at x=2, 4.0 at x=4.
        assert!((z_at(&out, Point2f64::new(3.0, 1.0), 2.0, 0.0) - 3.0).abs() <= 1e-9);
        assert!((z_at(&out, Point2f64::new(3.0, 1.0), 4.0, 0.0) - 4.0).abs() <= 1e-9);
        // C chains through B onto the same plane: 5.0 at x=6.
        assert!((z_at(&out, Point2f64::new(5.0, 1.0), 6.0, 0.0) - 5.0).abs() <= 1e-9);
    }

    /// A continuation CYCLE (A←B, B←C, C←A) is detected by the provenance
    /// walk and the closing entry skipped — C keeps its own plane instead of
    /// inheriting the chain.
    #[test]
    fn ceiling_continuation_cycle_is_skipped() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());
        let ceilings = CeilingRegions {
            regions: vec![
                slab(0.0, 2.0, 2.0, 2.0), // A flat 2.0
                slab(2.0, 4.0, 3.0, 3.0), // B flat 3.0
                slab(4.0, 6.0, 4.0, 4.0), // C flat 4.0
            ],
        };
        let (pa, pb, pc) = (
            Point2f64::new(1.0, 1.0),
            Point2f64::new(3.0, 1.0),
            Point2f64::new(5.0, 1.0),
        );
        ber.add_ceiling_continuation(0, pa, pb, 0); // A ← B
        ber.add_ceiling_continuation(0, pb, pc, 0); // B ← C
        ber.add_ceiling_continuation(0, pc, pa, 0); // C ← A: closes the loop
        let out = ber.apply_ceiling_continuations(0, ceilings);
        assert!(
            (z_at(&out, pa, 0.0, 0.0) - 3.0).abs() <= 1e-9,
            "A took B's plane"
        );
        assert!(
            (z_at(&out, pb, 2.0, 0.0) - 4.0).abs() <= 1e-9,
            "B took C's plane"
        );
        assert!(
            (z_at(&out, pc, 4.0, 0.0) - 4.0).abs() <= 1e-9,
            "the cycle-closing entry is skipped — C keeps its own plane"
        );
    }

    /// A cross-storey continuation converts the source plane through the
    /// floor datums: the storey below's sloped ceiling continues physically
    /// straight into the storey above's local frame.
    #[test]
    fn ceiling_continuation_from_storey_below_converts_datums() {
        let mut ber = Ber::default();
        let mut below = Storey::default();
        below.ceiling_height_m = Some(2.5); // legacy stacking → datum₁ = 2.5
        below.ceilings = CeilingRegions {
            regions: vec![slab(0.0, 4.0, 2.0, 4.0)], // z = 2 + 0.5x (below-local)
        };
        let mut above = Storey::default();
        above.ceilings = CeilingRegions {
            regions: vec![slab(0.0, 4.0, 2.4, 2.4)],
        };
        ber.storeys.push(below);
        ber.storeys.push(above);
        let p = Point2f64::new(1.0, 1.0);
        ber.add_ceiling_continuation(1, p, p, -1);
        let ceilings = ber.storeys[1].ceilings.clone();
        let out = ber.apply_ceiling_continuations(1, ceilings);
        // Above-local plane: (2 + 0.5x) + datum₀(0) − datum₁(2.5) = −0.5 + 0.5x.
        assert!((z_at(&out, p, 0.0, 0.0) - (-0.5)).abs() <= 1e-9);
        assert!((z_at(&out, p, 4.0, 0.0) - 1.5).abs() <= 1e-9);
    }

    /// `clear_ceiling_continuations_at` removes exactly the entries whose
    /// target shares the tapped region.
    #[test]
    fn clear_ceiling_continuations_at_removes_the_regions_entries() {
        let mut ber = Ber::default();
        let mut storey = Storey::default();
        storey.ceilings = CeilingRegions {
            regions: vec![slab(0.0, 2.0, 2.0, 2.0), slab(2.0, 4.0, 2.4, 2.4)],
        };
        ber.storeys.push(storey);
        let (in_a1, in_a2, in_b) = (
            Point2f64::new(0.5, 1.0),
            Point2f64::new(1.5, 1.0),
            Point2f64::new(3.0, 1.0),
        );
        ber.add_ceiling_continuation(0, in_a1, in_b, 0);
        ber.add_ceiling_continuation(0, in_a2, in_b, 0);
        ber.add_ceiling_continuation(0, in_b, in_a1, 0);
        assert_eq!(
            ber.clear_ceiling_continuations_at(0, Point2f64::new(1.0, 0.5)),
            2,
            "both region-A entries removed"
        );
        assert_eq!(ber.storeys[0].ceiling_continuations.len(), 1);
        assert_eq!(
            ber.clear_ceiling_continuations_at(0, Point2f64::new(9.0, 9.0)),
            0
        );
    }

    /// `Storey::reset_ceiling_shape` clears every ceiling-SHAPE input and
    /// nothing else: the default height, the dormers and the floor-side
    /// fields survive.
    #[test]
    fn reset_ceiling_shape_clears_inputs_keeps_height_and_dormers() {
        use crate::ber::structural::{CeilingContinuation, Dormer};
        let mut storey = Storey::default();
        storey.ceiling_height_m = Some(2.6);
        storey.ceiling_steps = vec![(vec![Point2f64::new(0.0, 0.0)], 0.3)];
        storey.ceiling_splits = vec![vec![Point2f64::new(0.0, 0.0)]];
        storey.ceiling_apexes = vec![CeilingApex {
            point: Point2f64::new(1.0, 1.0),
            z_m: 4.0,
        }];
        storey.ceiling_apex_hints = vec![CeilingApexHint {
            apex: (1.0, 1.0),
            region: (0.5, 0.5),
        }];
        storey.ceiling_unfanned = vec![(2.0, 2.0)];
        storey.ceiling_plane_anchors = vec![(3.0, 3.0)];
        storey.ceiling_continuations = vec![CeilingContinuation {
            target: (1.0, 1.0),
            source: (3.0, 1.0),
            source_storey_offset: 0,
        }];
        storey.dormers = vec![Dormer {
            footprint: vec![
                Point2f64::new(0.0, 0.0),
                Point2f64::new(1.0, 0.0),
                Point2f64::new(1.0, 1.0),
            ],
            top_z_m: 2.9,
        }];
        storey.floor_splits = vec![vec![Point2f64::new(0.0, 0.0)]];
        storey.reset_ceiling_shape();
        assert!(storey.ceiling_steps.is_empty());
        assert!(storey.ceiling_splits.is_empty());
        assert!(storey.ceiling_apexes.is_empty());
        assert!(storey.ceiling_apex_hints.is_empty());
        assert!(storey.ceiling_unfanned.is_empty());
        assert!(storey.ceiling_plane_anchors.is_empty());
        assert!(storey.ceiling_continuations.is_empty());
        assert_eq!(storey.ceiling_height_m, Some(2.6), "default height kept");
        assert_eq!(storey.dormers.len(), 1, "dormers kept");
        assert_eq!(storey.floor_splits.len(), 1, "floor-side fields untouched");
    }

    /// The plane-anchor setter swaps overrides in one call: remove the
    /// side's previous point(s) within the epsilon, push the new quantized
    /// one; `add = None` clears back to auto.
    #[test]
    fn update_ceiling_plane_anchors_swaps_and_clears() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());
        let a = Point2f64::new(5.52, 8.31);
        let b = Point2f64::new(1.52, 7.31);
        assert_eq!(
            ber.update_ceiling_plane_anchors(0, &[], Some(a)),
            Some(true)
        );
        assert_eq!(ber.storeys[0].ceiling_plane_anchors.len(), 1);
        // Swap: drop `a` (matched within the epsilon), store `b`.
        let a_near = Point2f64::new(5.52 + 5e-5, 8.31);
        assert_eq!(
            ber.update_ceiling_plane_anchors(0, &[a_near], Some(b)),
            Some(true)
        );
        let stored = &ber.storeys[0].ceiling_plane_anchors;
        assert_eq!(stored.len(), 1, "previous override dropped: {stored:?}");
        assert!(
            (stored[0].0 as f64 - b.x).abs() <= 1e-4 && (stored[0].1 as f64 - b.y).abs() <= 1e-4
        );
        // Clear back to auto.
        assert_eq!(ber.update_ceiling_plane_anchors(0, &[b], None), Some(true));
        assert!(ber.storeys[0].ceiling_plane_anchors.is_empty());
        assert_eq!(
            ber.update_ceiling_plane_anchors(9, &[], Some(a)),
            None,
            "missing storey is a no-op"
        );
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

    /// `storey_phase_zones` resolves each floor region's phase through the
    /// house-level construction registry, SKIPS `Unassigned` (the untagged
    /// remainder), and returns empty out of range.
    #[test]
    fn storey_phase_zones_skips_unassigned_and_handles_out_of_range() {
        use crate::ber::structural::FloorRegions;
        let mut ber = Ber::default();
        let cid_orig = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Original,
        );
        let cid_unassigned = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Unassigned,
        );
        let sq = |x0: f64, x1: f64| {
            vec![
                Point2f64::new(x0, 0.0),
                Point2f64::new(x1, 0.0),
                Point2f64::new(x1, 4.0),
                Point2f64::new(x0, 4.0),
            ]
        };
        // Left half tagged Original; the remainder stays Unassigned.
        let floors =
            FloorRegions::partition(&sq(0.0, 8.0), &[(sq(0.0, 4.0), cid_orig)], cid_unassigned);
        ber.storeys.push(Storey {
            floors,
            ..Storey::default()
        });
        let zones = ber.storey_phase_zones(0);
        assert_eq!(zones.len(), 1, "{zones:?}");
        assert_eq!(zones[0].0, ConstructionPhase::Original);
        assert!(zones[0].1.len() >= 3);
        // Out of range → empty, no panic.
        assert!(ber.storey_phase_zones(1).is_empty());
        assert!(ber.storey_phase_zones(99).is_empty());
    }

    /// Report 1cc38dc8 (Fix B): a phase zone shrunk to a DONUT (an older phase
    /// with a newer overlapping phase carved out of its middle) projects through
    /// `storey_phase_zone_polygons` carrying the HOLE ring, and its label anchor
    /// lands inside the material and OUTSIDE the hole — geo's interior point
    /// respects holes, so a tint/label placed there never sits in the void.
    #[test]
    fn storey_phase_zone_polygons_donut_carries_hole_and_labels_inside() {
        use crate::ber::structural::FloorRegions;
        use geo::{Contains, LineString, Polygon};
        let mut ber = Ber::default();
        let cid_orig = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Original,
        );
        let cid_ext = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Extension1,
        );
        let cid_rem = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Unassigned,
        );
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![
                Point2f64::new(x0, y0),
                Point2f64::new(x1, y0),
                Point2f64::new(x1, y1),
                Point2f64::new(x0, y1),
            ]
        };
        // The Extension island (2..6) claims the middle FIRST; Original then gets
        // the 8×8 outline MINUS the island → a donut.
        let floors = FloorRegions::partition(
            &rect(0.0, 0.0, 8.0, 8.0),
            &[
                (rect(2.0, 2.0, 6.0, 6.0), cid_ext),
                (rect(0.0, 0.0, 8.0, 8.0), cid_orig),
            ],
            cid_rem,
        );
        ber.storeys.push(Storey {
            floors,
            ..Storey::default()
        });

        let zones = ber.storey_phase_zone_polygons(0);
        let orig = zones
            .iter()
            .find(|z| z.phase == ConstructionPhase::Original)
            .expect("Original donut zone present");
        assert_eq!(
            orig.holes.len(),
            1,
            "the Original donut carries the island hole: {orig:?}"
        );
        // Rebuild the geo polygon (exterior + hole) and confirm the label anchor
        // is inside its material — inside the exterior AND outside the hole.
        let ls = |pts: &[Point2f64]| {
            LineString::from(pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>())
        };
        let poly = Polygon::new(
            ls(&orig.exterior),
            orig.holes.iter().map(|h| ls(h)).collect(),
        );
        let anchor = geo::Point::new(orig.label_anchor.x, orig.label_anchor.y);
        assert!(
            poly.contains(&anchor),
            "label anchor {:?} sits inside the donut and outside the hole",
            orig.label_anchor
        );
        // The Extension island zone is solid — no hole.
        let ext = zones
            .iter()
            .find(|z| z.phase == ConstructionPhase::Extension1)
            .expect("Extension island zone present");
        assert!(
            ext.holes.is_empty(),
            "the island is solid (no hole): {ext:?}"
        );
    }

    /// Photo deferral semantics: Front is never deferrable (`set` is a no-op
    /// and a rogue stored `Front` entry reads back as NOT deferred — the
    /// accessors normalize it away), toggling dedups, and
    /// `deferred_photos_pending` lists only deferred sides still lacking an
    /// external photo.
    #[test]
    fn photo_deferral_front_normalized_and_pending() {
        let mut ber = Ber::default();

        // Front: set is a no-op…
        ber.set_photo_deferred(ExternalSide::Front, true);
        assert!(ber.deferred_photo_sides.is_empty(), "Front never stored");
        // …and a rogue stored entry is normalized away on read.
        ber.deferred_photo_sides.push(ExternalSide::Front);
        assert!(!ber.photo_deferred(ExternalSide::Front));
        assert!(ber.deferred_photos_pending().is_empty());
        ber.deferred_photo_sides.clear();

        // Deferrable sides toggle, dedup, and clear.
        ber.set_photo_deferred(ExternalSide::Back, true);
        ber.set_photo_deferred(ExternalSide::Back, true);
        assert_eq!(ber.deferred_photo_sides, vec![ExternalSide::Back]);
        assert!(ber.photo_deferred(ExternalSide::Back));
        ber.set_photo_deferred(ExternalSide::Left, true);
        assert_eq!(
            ber.deferred_photos_pending(),
            vec![ExternalSide::Left, ExternalSide::Back],
            "pending in ALL order"
        );
        ber.set_photo_deferred(ExternalSide::Back, false);
        assert!(!ber.photo_deferred(ExternalSide::Back));

        // A deferred side WITH a photo is no longer pending.
        ber.photos.push(Evidence {
            side: Some(ExternalSide::Left),
            ..Default::default()
        });
        assert!(ber.deferred_photos_pending().is_empty());
        assert!(
            ber.photo_deferred(ExternalSide::Left),
            "the flag itself is untouched by pending-ness (capture clears it \
             at the controller mutation, not here)"
        );
    }

    /// The deferred list survives a serde round-trip, and a record written
    /// BEFORE the field existed (no `deferred_photo_sides` key) loads with
    /// nothing deferred.
    #[test]
    fn deferred_photo_sides_serde_roundtrip_and_old_record_default() {
        let mut ber = Ber::default();
        ber.set_photo_deferred(ExternalSide::Back, true);
        ber.set_photo_deferred(ExternalSide::Right, true);
        let json = serde_json::to_string(&ber).unwrap();
        let back: Ber = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.deferred_photo_sides,
            vec![ExternalSide::Back, ExternalSide::Right]
        );

        // Old record: strip the key entirely and reload.
        let mut value: serde_json::Value = serde_json::to_value(&ber).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("deferred_photo_sides")
            .expect("field present before the strip");
        let old: Ber = serde_json::from_value(value).unwrap();
        assert!(
            old.deferred_photo_sides.is_empty(),
            "pre-field records load with nothing deferred"
        );
    }

    /// `missing_external_photo_sides` names exactly the REQUIRED sides the
    /// External-photos gate still waits on: both required sides on an empty
    /// record, minus a side once a photo tagged with it lands, minus a
    /// deferrable side once it's deferred.
    #[test]
    fn missing_external_photo_sides_tracks_photos_and_deferrals() {
        let mut ber = Ber::default();
        assert_eq!(
            ber.missing_external_photo_sides(),
            vec![ExternalSide::Front, ExternalSide::Back],
            "the required sides, in ALL order"
        );

        // A Front photo removes Front.
        ber.photos.push(Evidence {
            side: Some(ExternalSide::Front),
            ..Default::default()
        });
        assert_eq!(ber.missing_external_photo_sides(), vec![ExternalSide::Back]);

        // Deferring Back (deferrable) counts as set for the gate.
        ber.set_photo_deferred(ExternalSide::Back, true);
        assert!(ber.missing_external_photo_sides().is_empty());
    }

    // --- Heating-system sync from placed objects -------------------------

    /// A `Ber` whose attic bucket holds the given objects — the simplest place
    /// to hang heat-source objects for the sync tests.
    fn ber_with_objects(objects: Vec<crate::ber::objects::BuildingObject>) -> Ber {
        use crate::ber::objects::ObjectRoom;
        let mut ber = Ber::default();
        let mut room = ObjectRoom::new(domain::points::Point2f32::new(0.0, 0.0));
        room.objects = objects;
        ber.attic_room = Some(room);
        ber
    }

    fn obj(kind: crate::ber::objects::ObjectKind) -> crate::ber::objects::BuildingObject {
        crate::ber::objects::BuildingObject::new(kind, 1)
    }

    /// OLD-shape records (no `separate_hw_control` on a `BuildingObject`, no
    /// `room_thermostat` on `HeatingControls`, no `secondary_heating` on
    /// `HeatingSystem`) still deserialize — the new fields default.
    #[test]
    fn legacy_heating_json_loads_with_defaulted_new_fields() {
        use crate::ber::objects::BuildingObject;
        use crate::ber::systems::{HeatingControls, HeatingSystem};

        let obj: BuildingObject =
            serde_json::from_str(r#"{"id":"00000000-0000-0000-0000-000000000000","kind":{"Boiler":"Gas"},"count":1,"note":null,"photos":[]}"#)
                .expect("legacy BuildingObject loads");
        assert!(!obj.separate_hw_control);

        let controls: HeatingControls =
            serde_json::from_str(r#"{"programmer":true,"zones":2,"notes":null}"#)
                .expect("legacy HeatingControls loads");
        assert_eq!(controls.room_thermostat, 0);

        let sys: HeatingSystem = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000000","system_type":"GasBoiler","description":"","year_installed":null,"controls":{"programmer":false,"zones":0,"notes":null},"separate_hot_water":false}"#,
        )
        .expect("legacy HeatingSystem loads");
        assert!(sys.secondary_heating.is_empty());
    }

    #[test]
    fn sync_single_gas_boiler() {
        use crate::ber::objects::{BoilerFuel, ObjectKind};
        use crate::ber::systems::HeatingSystemType;
        let mut ber = ber_with_objects(vec![obj(ObjectKind::Boiler(BoilerFuel::Gas))]);
        ber.sync_heating_system_from_objects();
        assert_eq!(ber.heating_system.system_type, HeatingSystemType::GasBoiler);
        assert!(ber.heating_system.secondary_heating.is_empty());
    }

    #[test]
    fn sync_gas_and_heat_pump_precedence_and_secondary() {
        use crate::ber::objects::{BoilerFuel, ObjectKind};
        use crate::ber::systems::HeatingSystemType;
        let mut ber = ber_with_objects(vec![
            obj(ObjectKind::Boiler(BoilerFuel::Gas)),
            obj(ObjectKind::Boiler(BoilerFuel::HeatPump)),
        ]);
        ber.sync_heating_system_from_objects();
        assert_eq!(ber.heating_system.system_type, HeatingSystemType::HeatPump);
        assert_eq!(
            ber.heating_system.secondary_heating,
            vec![HeatingSystemType::GasBoiler]
        );
    }

    #[test]
    fn sync_preserves_assessor_chosen_primary() {
        use crate::ber::objects::{BoilerFuel, ObjectKind};
        use crate::ber::systems::HeatingSystemType;
        let mut ber = ber_with_objects(vec![
            obj(ObjectKind::Boiler(BoilerFuel::Gas)),
            obj(ObjectKind::Boiler(BoilerFuel::HeatPump)),
        ]);
        // Assessor already picked the gas boiler as primary.
        ber.heating_system.system_type = HeatingSystemType::GasBoiler;
        ber.sync_heating_system_from_objects();
        assert_eq!(ber.heating_system.system_type, HeatingSystemType::GasBoiler);
        assert_eq!(
            ber.heating_system.secondary_heating,
            vec![HeatingSystemType::HeatPump]
        );
    }

    #[test]
    fn sync_recomputes_after_boiler_removed() {
        use crate::ber::objects::{BoilerFuel, ObjectKind};
        use crate::ber::systems::HeatingSystemType;
        // Gas chosen primary, then the gas boiler is removed leaving a heat pump.
        let mut ber = ber_with_objects(vec![obj(ObjectKind::Boiler(BoilerFuel::HeatPump))]);
        ber.heating_system.system_type = HeatingSystemType::GasBoiler;
        ber.sync_heating_system_from_objects();
        assert_eq!(ber.heating_system.system_type, HeatingSystemType::HeatPump);
        assert!(ber.heating_system.secondary_heating.is_empty());

        // Now remove every heat source → Invalid.
        ber.attic_room.as_mut().unwrap().objects.clear();
        ber.sync_heating_system_from_objects();
        assert_eq!(ber.heating_system.system_type, HeatingSystemType::Invalid);
        assert!(ber.heating_system.secondary_heating.is_empty());
    }

    #[test]
    fn sync_lone_panel_needs_affirmation() {
        use crate::ber::objects::ObjectKind;
        use crate::ber::systems::HeatingSystemType;
        // A lone panel heater is NOT auto-promoted to primary — it stays
        // unresolved (so the shell's heat-source gate blocks) and is listed as a
        // secondary source until the assessor affirms it.
        let mut ber = ber_with_objects(vec![obj(ObjectKind::PanelHeater)]);
        ber.sync_heating_system_from_objects();
        assert_eq!(ber.heating_system.system_type, HeatingSystemType::Invalid);
        assert_eq!(
            ber.heating_system.secondary_heating,
            vec![HeatingSystemType::ElectricHeater]
        );
        // Once affirmed (system_type set to ElectricHeater by the "yes" answer),
        // the sync preserves it and it is no longer secondary.
        ber.heating_system.system_type = HeatingSystemType::ElectricHeater;
        ber.sync_heating_system_from_objects();
        assert_eq!(
            ber.heating_system.system_type,
            HeatingSystemType::ElectricHeater
        );
        assert!(ber.heating_system.secondary_heating.is_empty());
    }

    #[test]
    fn sync_chimney_becomes_secondary() {
        use crate::ber::objects::{BoilerFuel, ChimneyState, ObjectKind};
        use crate::ber::systems::HeatingSystemType;
        let mut ber = ber_with_objects(vec![
            obj(ObjectKind::Boiler(BoilerFuel::Gas)),
            obj(ObjectKind::Chimney(ChimneyState::Open)),
            // A second chimney must NOT double the secondary entry.
            obj(ObjectKind::Chimney(ChimneyState::Closed)),
        ]);
        ber.sync_heating_system_from_objects();
        assert_eq!(ber.heating_system.system_type, HeatingSystemType::GasBoiler);
        assert_eq!(
            ber.heating_system.secondary_heating,
            vec![HeatingSystemType::Other("Chimney".to_string())]
        );
    }

    #[test]
    fn sync_programmer_and_thermostat_controls() {
        use crate::ber::objects::{BoilerFuel, BuildingObject, ObjectKind};
        let mut ber = ber_with_objects(vec![
            obj(ObjectKind::Boiler(BoilerFuel::Gas)),
            obj(ObjectKind::Programmer),
            // Thermostat counts sum to 3 (2 + 1).
            BuildingObject::new(ObjectKind::Thermostat, 2),
            BuildingObject::new(ObjectKind::Thermostat, 1),
        ]);
        ber.sync_heating_system_from_objects();
        assert!(ber.heating_system.controls.programmer);
        assert_eq!(ber.heating_system.controls.room_thermostat, 3);
    }

    #[test]
    fn sync_separate_hot_water_from_boiler_or_programmer() {
        use crate::ber::objects::{BoilerFuel, ObjectKind};
        // Boiler with the checkbox set → separate_hot_water true.
        let mut boiler = obj(ObjectKind::Boiler(BoilerFuel::Gas));
        boiler.separate_hw_control = true;
        let mut ber = ber_with_objects(vec![boiler]);
        ber.sync_heating_system_from_objects();
        assert!(ber.heating_system.separate_hot_water);

        // Programmer with the checkbox set → also true.
        let mut prog = obj(ObjectKind::Programmer);
        prog.separate_hw_control = true;
        let mut ber = ber_with_objects(vec![obj(ObjectKind::Boiler(BoilerFuel::Gas)), prog]);
        ber.sync_heating_system_from_objects();
        assert!(ber.heating_system.separate_hot_water);

        // Neither set → false.
        let mut ber = ber_with_objects(vec![
            obj(ObjectKind::Boiler(BoilerFuel::Gas)),
            obj(ObjectKind::Programmer),
        ]);
        ber.sync_heating_system_from_objects();
        assert!(!ber.heating_system.separate_hot_water);
    }

    #[test]
    fn sync_is_idempotent() {
        use crate::ber::objects::{BoilerFuel, BuildingObject, ChimneyState, ObjectKind};
        let mut ber = ber_with_objects(vec![
            obj(ObjectKind::Boiler(BoilerFuel::Gas)),
            obj(ObjectKind::Boiler(BoilerFuel::HeatPump)),
            obj(ObjectKind::PanelHeater),
            obj(ObjectKind::Chimney(ChimneyState::Open)),
            obj(ObjectKind::Programmer),
            BuildingObject::new(ObjectKind::Thermostat, 2),
        ]);
        ber.sync_heating_system_from_objects();
        let first = ber.heating_system.clone();
        ber.sync_heating_system_from_objects();
        assert_eq!(first.system_type, ber.heating_system.system_type);
        assert_eq!(
            first.secondary_heating,
            ber.heating_system.secondary_heating
        );
        assert_eq!(
            first.controls.programmer,
            ber.heating_system.controls.programmer
        );
        assert_eq!(
            first.controls.room_thermostat,
            ber.heating_system.controls.room_thermostat
        );
        assert_eq!(
            first.separate_hot_water,
            ber.heating_system.separate_hot_water
        );
    }

    // ---- Ventilation-system sync from placed objects ----

    #[test]
    fn vent_sync_no_unit_is_natural() {
        use crate::ber::objects::{BoilerFuel, ObjectKind};
        use crate::ber::systems::VentilationSystemType;
        // A dwelling with heat-source objects but no ventilation unit → Natural.
        let mut ber = ber_with_objects(vec![obj(ObjectKind::Boiler(BoilerFuel::Gas))]);
        ber.sync_ventilation_system_from_objects();
        assert!(matches!(
            ber.ventilation_system.system_type,
            VentilationSystemType::Natural
        ));
    }

    #[test]
    fn vent_sync_mechanical_unit() {
        use crate::ber::objects::{ObjectKind, VentUnitKind};
        use crate::ber::systems::VentilationSystemType;
        let mut ber = ber_with_objects(vec![obj(ObjectKind::VentilationUnit(
            VentUnitKind::Mechanical,
        ))]);
        ber.sync_ventilation_system_from_objects();
        assert!(matches!(
            ber.ventilation_system.system_type,
            VentilationSystemType::Mechanical
        ));
    }

    #[test]
    fn vent_sync_heat_recovery_unit() {
        // LEGACY records with a VentilationUnit pin: counts like a Central
        // fan — Mechanical, no MEV/MVHR distinction (retired picker options).
        use crate::ber::objects::{ObjectKind, VentUnitKind};
        use crate::ber::systems::VentilationSystemType;
        let mut ber = ber_with_objects(vec![obj(ObjectKind::VentilationUnit(
            VentUnitKind::HeatRecovery,
        ))]);
        ber.sync_ventilation_system_from_objects();
        assert!(matches!(
            ber.ventilation_system.system_type,
            VentilationSystemType::Mechanical
        ));
    }

    #[test]
    fn vent_sync_central_fan_is_mechanical() {
        use crate::ber::objects::{FanType, ObjectKind};
        use crate::ber::systems::VentilationSystemType;
        let mut ber = ber_with_objects(vec![obj(ObjectKind::Fan(FanType::Central))]);
        ber.sync_ventilation_system_from_objects();
        assert!(matches!(
            ber.ventilation_system.system_type,
            VentilationSystemType::Mechanical
        ));
    }

    #[test]
    fn vent_sync_reverts_to_natural_when_unit_removed() {
        use crate::ber::objects::{FanType, ObjectKind};
        use crate::ber::systems::VentilationSystemType;
        let mut ber = ber_with_objects(vec![obj(ObjectKind::Fan(FanType::Central))]);
        ber.sync_ventilation_system_from_objects();
        assert!(matches!(
            ber.ventilation_system.system_type,
            VentilationSystemType::Mechanical
        ));
        // Remove the unit → re-derivation lands back on Natural.
        ber.attic_room.as_mut().unwrap().objects.clear();
        ber.sync_ventilation_system_from_objects();
        assert!(matches!(
            ber.ventilation_system.system_type,
            VentilationSystemType::Natural
        ));
    }

    #[test]
    fn vent_sync_is_idempotent() {
        use crate::ber::objects::{ObjectKind, VentUnitKind};
        use crate::ber::systems::VentilationSystemType;
        let mut ber = ber_with_objects(vec![obj(ObjectKind::VentilationUnit(
            VentUnitKind::Mechanical,
        ))]);
        ber.sync_ventilation_system_from_objects();
        ber.sync_ventilation_system_from_objects();
        assert!(matches!(
            ber.ventilation_system.system_type,
            VentilationSystemType::Mechanical
        ));
    }

    /// Back-compat: a `ventilation_system` blob saved before the object-driven
    /// wiring (no unit placed) still loads, and a sync yields Natural.
    #[test]
    fn legacy_ventilation_json_loads_and_syncs_natural() {
        use crate::ber::systems::{VentilationSystem, VentilationSystemType};
        let vs: VentilationSystem = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000000","system_type":"Natural","description":null}"#,
        )
        .expect("legacy VentilationSystem loads");
        assert!(matches!(vs.system_type, VentilationSystemType::Natural));

        // No unit objects → sync resolves to Natural.
        let mut ber = ber_with_objects(vec![]);
        ber.ventilation_system = vs;
        ber.sync_ventilation_system_from_objects();
        assert!(matches!(
            ber.ventilation_system.system_type,
            VentilationSystemType::Natural
        ));
    }
}
