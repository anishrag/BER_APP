//! Post-survey client follow-up requests.
//!
//! After a survey there are usually a handful of documents/details still needed
//! from the client to resolve unknowns and use better-than-default values (DoPs,
//! build-date proof, builder letters, …). The assessor ticks the ones that
//! apply on the "Follow ups" tab and one email goes out to the client.
//!
//! This module is the pure-data heart of that feature (per the data-model
//! placement rule): the catalogue of built-in request points, the survey-driven
//! auto-selection, and the assembly of the editable email draft. Per-BER custom
//! points ([`CustomPoint`]) let the assessor add one-off requests. The catalogue
//! wording comes from mining ~600 of the assessor's past client emails (see the
//! `followup-email-points` project notes). Networking (the actual send) and the
//! one-shot lock live in the app shell; only the [`FollowUpRequest`] record that
//! persists with the BER lives here.

use serde::{Deserialize, Deserializer, Serialize};

use super::ber_struct::Ber;
use super::common::{ConstructionPhase, ItemId, Year};
use super::structural::{ConstructionBase, ConstructionMakeup};
use super::systems::HeatingSystemType;

/// "Newer than N years old" window: an element built/installed in `year` is
/// newer than 10 years old at `reference_year` when `year + 10 > reference_year`.
const RECENT_YEARS: Year = 10;

/// The four dated-extension phases, in order.
const EXTENSION_PHASES: [ConstructionPhase; 4] = [
    ConstructionPhase::Extension1,
    ConstructionPhase::Extension2,
    ConstructionPhase::Extension3,
    ConstructionPhase::Extension4,
];

/// One built-in post-survey request point. Serialized slug matches the
/// master-list id (e.g. `window_dop`) so a stored selection is stable across
/// releases; unknown/removed slugs are dropped on load (see [`lenient_points`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpPointId {
    HouseBuildDate,
    ExtensionBuildDate,
    BuilderUpgradeLetter,
    WorksEvidenceNoBuilder,
    CertifierNewbuildLetter,
    WindowDop,
    DoorDop,
    SolarDocs,
    HeatPumpData,
    /// Combined airtightness test report + ACD (thermal-bridging) certification.
    AirtightnessAndAcd,
    /// Hot water cylinder whose model couldn't be read during the survey — ask
    /// for the make/model number + proof (label photo, invoice, spec sheet).
    /// Manual tick.
    WaterCylinderModel,
    /// Hot water cylinder whose insulation couldn't be measured (factory-sealed)
    /// — ask for documentation / an invoice proving the supply date. Manual tick.
    WaterCylinderInsulation,
}

/// Base URL of the client-facing documentation hub on the website. Each point's
/// [`doc_slug`] appends to this to give the "More details" link in the email.
/// Must match the `/docs/<slug>` routes in BERwebsite (see `docPoints.js`).
pub const DOC_BASE_URL: &str = "https://cannygreen.com/docs";

/// Every built-in point, in the order the UI lists them.
pub const ALL_POINTS: [FollowUpPointId; 12] = [
    FollowUpPointId::HouseBuildDate,
    FollowUpPointId::ExtensionBuildDate,
    FollowUpPointId::BuilderUpgradeLetter,
    FollowUpPointId::WorksEvidenceNoBuilder,
    FollowUpPointId::CertifierNewbuildLetter,
    FollowUpPointId::WindowDop,
    FollowUpPointId::DoorDop,
    FollowUpPointId::SolarDocs,
    FollowUpPointId::HeatPumpData,
    FollowUpPointId::AirtightnessAndAcd,
    FollowUpPointId::WaterCylinderModel,
    FollowUpPointId::WaterCylinderInsulation,
];

impl FollowUpPointId {
    /// Stable slug (matches the serde rename) — the checklist item id sent to
    /// the backend so a ticked item is identifiable across the app + add-on.
    pub fn slug(self) -> &'static str {
        use FollowUpPointId::*;
        match self {
            HouseBuildDate => "house_build_date",
            ExtensionBuildDate => "extension_build_date",
            BuilderUpgradeLetter => "builder_upgrade_letter",
            WorksEvidenceNoBuilder => "works_evidence_no_builder",
            CertifierNewbuildLetter => "certifier_newbuild_letter",
            WindowDop => "window_dop",
            DoorDop => "door_dop",
            SolarDocs => "solar_docs",
            HeatPumpData => "heat_pump_data",
            AirtightnessAndAcd => "airtightness_and_acd",
            WaterCylinderModel => "water_cylinder_model",
            WaterCylinderInsulation => "water_cylinder_insulation",
        }
    }

    /// URL slug of this point's explainer page on the website
    /// (`{DOC_BASE_URL}/{doc_slug}`). Kept in step with `docPoints.js` in
    /// BERwebsite — these are the client-facing, hyphenated slugs (distinct from
    /// [`slug`], the underscore checklist id sent to the backend).
    pub fn doc_slug(self) -> &'static str {
        use FollowUpPointId::*;
        match self {
            HouseBuildDate => "house-build-date",
            ExtensionBuildDate => "extension-build-date",
            BuilderUpgradeLetter => "builders-letter",
            WorksEvidenceNoBuilder => "evidence-of-works",
            CertifierNewbuildLetter => "certifiers-letter",
            WindowDop => "window-documentation",
            DoorDop => "door-documentation",
            SolarDocs => "solar-panel-documents",
            HeatPumpData => "heat-pump-documents",
            AirtightnessAndAcd => "airtightness-and-thermal-bridging",
            WaterCylinderModel => "hot-water-cylinder",
            WaterCylinderInsulation => "hot-water-cylinder",
        }
    }

    /// Full URL of this point's explainer page, for the email's "More details"
    /// link.
    pub fn doc_url(self) -> String {
        format!("{DOC_BASE_URL}/{}", self.doc_slug())
    }

    /// Short label for the assessor's checkbox on the tab.
    pub fn label(self) -> &'static str {
        use FollowUpPointId::*;
        match self {
            HouseBuildDate => "House build date + proof",
            ExtensionBuildDate => "Extension build date + proof",
            BuilderUpgradeLetter => "Builder letter for upgrade works",
            WorksEvidenceNoBuilder => "Works evidence (2 proofs, no builder)",
            CertifierNewbuildLetter => "Architect / certifier letter (new build)",
            WindowDop => "Window DoP + proof of model",
            DoorDop => "Door DoP + proof of model",
            SolarDocs => "Solar PV documentation",
            HeatPumpData => "Heat pump data (sign-off + datasheet)",
            AirtightnessAndAcd => "Airtightness report + ACD certification",
            WaterCylinderModel => "Cylinder model (couldn't read on unit)",
            WaterCylinderInsulation => "Cylinder insulation (unmeasurable)",
        }
    }

    /// Short, client-facing heading for the request block in the email — plain
    /// English (no jargon like "DoP"/"ACD"), 2–3 words. Rendered bold above the
    /// request paragraph. Distinct from [`label`], which is the assessor's
    /// internal checkbox text.
    pub fn heading(self) -> &'static str {
        use FollowUpPointId::*;
        match self {
            HouseBuildDate => "House build date",
            ExtensionBuildDate => "Extension build date",
            BuilderUpgradeLetter => "Builder's letter",
            WorksEvidenceNoBuilder => "Evidence of works",
            CertifierNewbuildLetter => "Certifier's letter",
            WindowDop => "Window documentation",
            DoorDop => "Door documentation",
            SolarDocs => "Solar panel documents",
            HeatPumpData => "Heat pump documents",
            AirtightnessAndAcd => "Airtightness & thermal bridging",
            WaterCylinderModel => "Cylinder model",
            WaterCylinderInsulation => "Cylinder insulation",
        }
    }

    /// The request paragraph for this point, with `[bracketed]` prompts where a
    /// value has to be filled in by hand and survey values substituted where we
    /// can (build years, extension names). Rendered into the editable draft.
    pub fn render(self, ber: &Ber, fu: &FollowUpRequest) -> String {
        use FollowUpPointId::*;
        match self {
            HouseBuildDate => "Would you have any documentary evidence for the build date of the \
                house? The build date is quite important — the age band sets the default \
                insulation values for the parts of the house that can't be verified during the \
                survey. If you have any document that evidences it, ideally a legal or \
                conveyancing document, that would be ideal."
                .to_string(),
            ExtensionBuildDate => {
                let (word, verb, pron, them) = if fu.extensions.len() == 1 {
                    ("extension", "was", "it was", "it")
                } else {
                    ("extensions", "were", "they were", "them")
                };
                let bullets = fu
                    .extensions
                    .iter()
                    .map(|p| format!("• {}", extension_name(ber, p)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "Would you have any documentary evidence for when the following {word} {verb} \
                     built? Without it I have to assume {pron} built along with the original house, \
                     which usually gives {them} worse default values. A planning document, invoice \
                     or dated photos would all be ideal.\n\n{bullets}"
                )
            }
            BuilderUpgradeLetter => {
                let bullets = builder_surface_groups(ber)
                    .into_iter()
                    .filter(|s| fu.builder_surfaces.contains(&s.id))
                    .map(|s| format!("• {}", s.name))
                    .collect::<Vec<_>>()
                    .join("\n");
                let ufh = if has_underfloor_heating(ber) {
                    "\n\nIf there's underfloor heating, it should also explicitly state which \
                     layer the underfloor-heating pipes have been installed in."
                } else {
                    ""
                };
                format!(
                    "For the upgrade works, could you get a signed letter from the \
                     builder/contractor on their headed paper, with the full property address, \
                     confirming the works? It needs to include the full property address, the \
                     signer's name and job title, and state the build-up of the upgraded elements \
                     with the areas covered. We need this to mention the build-up of the following \
                     surfaces:\n\n{bullets}{ufh}"
                )
            }
            WorksEvidenceNoBuilder => {
                let bullets = builder_surface_groups(ber)
                    .into_iter()
                    .filter(|s| fu.builder_surfaces.contains(&s.id))
                    .map(|s| format!("• {}", s.name))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "Since a letter from the builder isn't available, the SEAI require two \
                     independent proofs instead. We need this for the following works:\n\n\
                     {bullets}\n\nFor each, any two of these can work: photos taken while the work \
                     was being done, and/or invoices, receipts or emails for the materials — \
                     ideally showing the site address and the product used. If you can dig out \
                     two, that can cover the evidence requirements."
                )
            }
            CertifierNewbuildLetter => {
                // The ticked surface split (same offered list as the builder's
                // letter): named surfaces make the request concrete; none
                // ticked keeps the general wording.
                let bullets = builder_surface_groups(ber)
                    .into_iter()
                    .filter(|s| fu.certifier_surfaces.contains(&s.id))
                    .map(|s| format!("• {}", s.name))
                    .collect::<Vec<_>>()
                    .join("\n");
                let surfaces = if bullets.is_empty() {
                    String::new()
                } else {
                    format!(
                        " It needs to cover the build-up of the following surfaces:\n\n{bullets}\n"
                    )
                };
                let fallback = match latest_extension_year(ber) {
                    Some(y) => format!(
                        " If this isn't available it wouldn't make much of a difference — I'd \
                         still be able to use default values from {y} for the extension, which \
                         should be quite good."
                    ),
                    None => String::new(),
                };
                format!(
                    "For the new extension elements, a letter from the builder unfortunately \
                     can't be accepted by the SEAI — I'd need documentation from the certifying \
                     architect, engineer or assigned certifier: as-built drawings signed off by \
                     them, or a signed letter confirming the build-ups as actually built and that \
                     they comply with the building regulations of the time.{surfaces}{fallback}"
                )
            }
            WindowDop => {
                // Ticked window TYPES make the request concrete (the client
                // sees which windows are meant); no types ticked keeps the
                // general wording (records with no placed windows, or the
                // intake-year-driven tick).
                let ticked: Vec<String> = candidate_window_types(ber)
                    .into_iter()
                    .filter(|(id, _)| fu.window_types.contains(id))
                    .map(|(_, label)| label)
                    .collect();
                let intro = if ticked.is_empty() {
                    "For the windows, to use better-than-default values I'd need two \
                     things:"
                        .to_string()
                } else {
                    let bullets = ticked
                        .iter()
                        .map(|l| format!("• {l}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!(
                        "For the following window types, to use better-than-default values \
                         I'd need two things for each:\n\n{bullets}"
                    )
                };
                format!(
                    "{intro}\n\n\
                     (1) the Declaration of Performance for the windows, showing the U-value and \
                     solar transmittance — [window supplier] should be able to provide this if you \
                     contact them (I need the standard certified product document, not one generated \
                     specifically for your house; NSAI WEP or BFRC certification also works).\n\n\
                     (2) something linking that model to your house — an invoice, order confirmation \
                     or email from the supplier listing the models supplied to your address.\n\n\
                     If it's not available it's no problem — the default values for the age of the \
                     house are reasonable, so it won't hurt the rating much."
                )
            }
            DoorDop => {
                // Same idea for the ticked door TYPES. Per-type wording only
                // applies while the record genuinely divides into MORE than
                // one door type (matching the tab's sub-row gate — so ticks
                // gone stale after doors collapsed to one type fall back
                // too); otherwise the single-door wording with its bracketed
                // prompt stands.
                let candidates = candidate_door_types(ber);
                let ticked: Vec<&String> = if candidates.len() > 1 {
                    candidates
                        .iter()
                        .filter(|n| fu.door_types.contains(n))
                        .collect()
                } else {
                    Vec::new()
                };
                let (intro, dop) = if ticked.is_empty() {
                    (
                        "For the [front / back] door, to use better-than-default values I'd \
                         need two things:"
                            .to_string(),
                        "its",
                    )
                } else {
                    let bullets = ticked
                        .iter()
                        .map(|l| format!("• {l}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    (
                        format!(
                            "For the following doors, to use better-than-default values I'd \
                             need two things for each:\n\n{bullets}"
                        ),
                        "the",
                    )
                };
                format!(
                    "{intro}\n\n\
                     (1) {dop} Declaration of Performance showing the U-value — [door supplier] \
                     should be able to provide this if you don't have it.\n\n\
                     (2) proof of the model installed at your address, such as an invoice or order \
                     confirmation. If there's a sticker or marking on the door itself showing the \
                     make/model, a photo of that would also help."
                )
            }
            SolarDocs => "Could you send me any documentation you have for the solar panels? The \
                best document is the Declaration of Works from the installer, as it has \
                everything required. Otherwise, an invoice or sign-off from [installer] on \
                headed paper (or a branded email) stating the make, model and number of panels \
                installed at the house would work. Without documentation I have to assume \
                default panels, which understates the benefit."
                .to_string(),
            HeatPumpData => "For the heat pump, could you check if you have:\n\n\
                (1) the Designer / Installer sign-off sheet from [installer] — any heat pump \
                installed after 2015 (and all grant installations) should have one, and it \
                contains values like the design flow temperature that usually give a better \
                rating than the defaults.\n\n\
                (2) the Ecodesign label / technical datasheet for [heat pump make/model] — the \
                manufacturer is required to provide this, and it shows the tested efficiency \
                figures I need."
                .to_string(),
            AirtightnessAndAcd => "Some newer houses have had an airtightness (air permeability) \
                test done, or are certified as built to the Acceptable Construction Details \
                standard for thermal bridging. If either of those documents exists for the house, \
                could you let me know so I can include it in the BER? These aren't always \
                available — I just wanted to check in case you have them."
                .to_string(),
            WaterCylinderModel => "For the hot water cylinder, I couldn't make out the model \
                details on it during the survey. Would you have the make and model number — or an \
                invoice, spec sheet, or a photo of the manufacturer's label showing it? That lets \
                me identify the exact cylinder rather than fall back to a default."
                .to_string(),
            WaterCylinderInsulation => "For the hot water cylinder, I couldn't measure the \
                insulation thickness, as it's sealed inside the casing. Would you have any \
                documentation for it, or even an invoice showing when it was supplied? That lets \
                me use its actual figures instead of an age-based default, which is usually worse."
                .to_string(),
        }
    }
}

/// The shared window TYPES offered as sub-checks under the Window-documentation
/// request — `(type id, display label)`, in registry order, restricted to types
/// some placed window actually references (orphan registry entries — deleted
/// windows' types, dedup seeds — would only inflate the list; same scope lesson
/// as [`builder_surface_groups`]). The client is asked about the ticked ones.
pub fn candidate_window_types(ber: &Ber) -> Vec<(ItemId, String)> {
    use std::collections::HashSet;
    let used: HashSet<ItemId> = ber
        .storeys
        .iter()
        .flat_map(|s| s.windows.iter())
        .map(|w| w.window_type_id)
        .collect();
    ber.window_types
        .iter()
        .filter(|t| used.contains(&t.id))
        .map(|t| (t.id, t.display_label()))
        .collect()
}

/// The distinct door TYPES on the record — trimmed non-empty `type_name`s in
/// first-appearance order (doors group by their free-text label; the label IS
/// the group, as in `rename_door_type`). Offered as sub-checks under the
/// Door-documentation request only when there's MORE than one — a single door
/// type keeps the plain request row.
pub fn candidate_door_types(ber: &Ber) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in &ber.storeys {
        for d in &s.doors {
            let name = d.type_name.trim();
            if !name.is_empty() && !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// The extension phases still needing a dated proof — defined but not
/// year-proven. These are the sub-checks under the Extension build-date request;
/// the client is asked about the ticked ones.
pub fn candidate_extensions(ber: &Ber) -> Vec<ConstructionPhase> {
    EXTENSION_PHASES
        .iter()
        .filter(|p| {
            ber.construction_phases.is_defined(p) && !ber.construction_phases.has_year_proof(p)
        })
        .cloned()
        .collect()
}

/// Display name for an extension phase in the request — its nickname if given,
/// otherwise the built-in name ("Extension 1").
pub fn extension_name(ber: &Ber, phase: &ConstructionPhase) -> String {
    ber.construction_phases
        .nickname_of(phase)
        .filter(|n| !n.trim().is_empty())
        .map(|n| n.to_string())
        .unwrap_or_else(|| phase.default_name())
}

/// A surface group offered under the Builder's-letter request: its construction
/// id, display name (the assessor's Structure label, else the type name), and
/// whether to auto-tick it — original-house fabric whose build-up is specified
/// (custom layers / added insulation) or declared unknown, i.e. something a
/// builder's letter can evidence.
pub struct SurfaceGroup {
    pub id: ItemId,
    pub name: String,
    pub auto: bool,
}

fn surface_name(base: &ConstructionBase, type_label: &str) -> String {
    base.name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| type_label.to_string())
}

/// Auto-tick a surface for the builder's letter when it's original-house fabric
/// with a specified (photo-backed) build-up or a declared-unknown one.
fn builder_auto(base: &ConstructionBase) -> bool {
    base.construction_phase == ConstructionPhase::Original
        && (base.construction_makeup.requires_photo()
            || base.construction_makeup == ConstructionMakeup::Unknown)
}

/// The wall/floor/ceiling construction groups an EXISTING heat-loss surface
/// references, for the Builder's-/certifier's-letter sub-checks — walls, then
/// floors, then ceilings/roofs, each with a display name and the auto-tick
/// flag. The division and scope mirror the STRUCTURE REVIEW exactly: one
/// entry per construction group, offered only when some resolved region in
/// structure scope (`in_structure_scope` — heat-loss-relevant: not
/// party / inter-storey / excluded) carries it. Untethered constructions —
/// the find-or-create base seeds, records orphaned by re-grouping, groups
/// referenced only by buried junction surfaces — never reach the list (they
/// were what inflated a messy record's letter to 20+ rows; field feedback
/// 2026-07-24).
pub fn builder_surface_groups(ber: &Ber) -> Vec<SurfaceGroup> {
    use std::collections::HashSet;
    let (mut walls, mut floors, mut ceilings) = (HashSet::new(), HashSet::new(), HashSet::new());
    for s in &ber.storeys {
        for r in &s.walls.regions {
            if r.exposure.in_structure_scope() {
                walls.insert(r.construction_id);
            }
        }
        for r in &s.floors.regions {
            if r.exposure.in_structure_scope() {
                floors.insert(r.construction_id);
            }
        }
        for r in &s.ceilings.regions {
            if r.exposure.in_structure_scope() {
                ceilings.insert(r.construction_id);
            }
        }
    }
    let mut out = Vec::new();
    for w in ber
        .wall_constructions
        .iter()
        .filter(|w| walls.contains(&w.base.id))
    {
        out.push(SurfaceGroup {
            id: w.base.id,
            name: surface_name(&w.base, w.construction_type.label()),
            auto: builder_auto(&w.base),
        });
    }
    for f in ber
        .floor_constructions
        .iter()
        .filter(|f| floors.contains(&f.base.id))
    {
        out.push(SurfaceGroup {
            id: f.base.id,
            name: surface_name(&f.base, f.construction_type.label()),
            auto: builder_auto(&f.base),
        });
    }
    for c in ber
        .ceiling_constructions
        .iter()
        .filter(|c| ceilings.contains(&c.base.id))
    {
        out.push(SurfaceGroup {
            id: c.base.id,
            name: surface_name(&c.base, c.construction_type.label()),
            auto: builder_auto(&c.base),
        });
    }
    out
}

/// The most recent KNOWN build year among the defined extensions — the "latest
/// extension" year cited in the certifier-letter fallback. `None` when no
/// extension has a known year.
fn latest_extension_year(ber: &Ber) -> Option<Year> {
    EXTENSION_PHASES
        .iter()
        .filter(|p| ber.construction_phases.is_defined(p))
        .filter_map(|p| ber.construction_phases.year_of(p))
        .max()
}

/// Whether any storey has underfloor heating (drives the pipe-layer note in the
/// builder's-letter request).
pub fn has_underfloor_heating(ber: &Ber) -> bool {
    ber.storeys
        .iter()
        .flat_map(|s| s.rooms.iter())
        .any(|r| r.underfloor_heating)
}

/// The email subject.
pub fn subject(ber: &Ber) -> String {
    if ber.address.trim().is_empty() {
        "Your BER assessment — a few details to finish the report".to_string()
    } else {
        format!("BER assessment, {} — a few details to finish", ber.address)
    }
}

/// True when something built/installed in `year` is newer than 10 years old at
/// the assessment date.
fn newer_than_recent(year: Year, reference_year: Year) -> bool {
    year + RECENT_YEARS > reference_year
}

/// The survey-driven initial selection of built-in points, given the assessment
/// year (for the "newer than 10 years" tests). Rules:
/// - **house build date**: unless the year is marked proven;
/// - **extension build date**: any defined extension whose year isn't proven;
/// - **builder letter** vs **works evidence** (mutually exclusive): with
///   insulation works AND the builder-letter answer NO → works evidence;
///   otherwise with insulation works OR the builder-letter answer YES → builder
///   letter;
/// - **certifier new-build letter**: an architect letter was flagged available
///   (extension or original);
/// - **window DoP**: any window newer than 10 years OR a certified window type;
/// - **door DoP**: the door install year is newer than 10 years;
/// - **heat pump data**: the heating system is a heat pump;
/// - **airtightness + ACD**: the house is newer than 10 years old.
pub fn auto_selected(ber: &Ber, reference_year: Year) -> Vec<FollowUpPointId> {
    use FollowUpPointId::*;
    let phases = &ber.construction_phases;
    let intake = &ber.intake;
    let mut out = Vec::new();

    if !phases.has_year_proof(&ConstructionPhase::Original) {
        out.push(HouseBuildDate);
    }

    let unproven_extension = EXTENSION_PHASES
        .iter()
        .any(|p| phases.is_defined(p) && !phases.has_year_proof(p));
    if unproven_extension {
        out.push(ExtensionBuildDate);
    }

    // Builder letter vs works evidence: mutually exclusive. A builder letter
    // alone is sufficient proof (SEAI); the 2-proof works evidence is the
    // fallback only when the builder letter is explicitly unavailable.
    let insulation_works =
        intake.insulation_walls || intake.insulation_roof || intake.insulation_floor;
    if insulation_works && intake.builder_letter_works == Some(false) {
        out.push(WorksEvidenceNoBuilder);
    } else if insulation_works || intake.builder_letter_works == Some(true) {
        out.push(BuilderUpgradeLetter);
    }

    if intake.architect_letter_extension == Some(true)
        || intake.architect_letter_original == Some(true)
    {
        out.push(CertifierNewbuildLetter);
    }

    let recent_window = ber
        .window_types
        .iter()
        .any(|w| w.certified || newer_than_recent(w.year, reference_year))
        || intake
            .window_year
            .is_some_and(|y| newer_than_recent(y, reference_year));
    if recent_window {
        out.push(WindowDop);
    }

    if intake
        .door_year
        .is_some_and(|y| newer_than_recent(y, reference_year))
    {
        out.push(DoorDop);
    }

    if matches!(ber.heating_system.system_type, HeatingSystemType::HeatPump) {
        out.push(HeatPumpData);
    }

    if phases
        .year_of(&ConstructionPhase::Original)
        .is_some_and(|y| newer_than_recent(y, reference_year))
    {
        out.push(AirtightnessAndAcd);
    }

    out
}

/// A per-BER, assessor-authored request point (the tab's "+"). Not part of the
/// shared catalogue — it lives on this BER's [`FollowUpRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomPoint {
    /// Stable identity for toggle/remove.
    pub id: ItemId,
    /// Checkbox label.
    pub label: String,
    /// The paragraph appended to the email when ticked.
    pub content: String,
    /// Whether it's included in the email.
    #[serde(default = "crate::ber::followup::default_true")]
    pub checked: bool,
}

fn default_true() -> bool {
    true
}

/// Assemble the editable email body for the selected built-in points (catalogue
/// order) followed by the ticked custom points (their order). A greeting +
/// intro, each point as a numbered block — a bold heading then its request
/// paragraph underneath — then a sign-off. Headings are wrapped in `**…**`
/// markers, which the intake backend renders as bold in the HTML email and
/// which read fine in the plain-text fallback / editable preview.
pub fn assemble_draft(ber: &Ber, fu: &FollowUpRequest) -> String {
    let mut body = String::from(
        "Hi,\n\nThanks again for having me out to survey your home for the BER assessment.\n\n",
    );
    // Each point is (heading, request paragraph, optional "more details" URL).
    // Built-in points link to their explainer page on the website; custom
    // points have no page, so no link.
    let mut points: Vec<(String, String, Option<String>)> = ALL_POINTS
        .into_iter()
        .filter(|p| fu.selected.contains(p))
        .map(|p| {
            (
                p.heading().to_string(),
                p.render(ber, fu),
                Some(p.doc_url()),
            )
        })
        .collect();
    points.extend(
        fu.custom
            .iter()
            .filter(|c| c.checked && !c.content.trim().is_empty())
            .map(|c| (c.label.clone(), c.content.clone(), None)),
    );

    if points.is_empty() {
        body.push_str(
            "I have everything I need for now — I'll be in touch if anything else comes up.\n\n",
        );
    } else {
        body.push_str(
            "To finish the assessment as accurately as possible, there are a few details I'd be \
             grateful for your help with. If you could reply point by point it makes sure \
             nothing gets missed:\n\n",
        );
        // Number the points so the client can answer each one: bold heading on
        // the numbered line, the full request underneath, then a "More details"
        // link to the explainer page (rendered as a button in the HTML email).
        for (i, (heading, para, url)) in points.iter().enumerate() {
            match url {
                Some(u) => body.push_str(&format!(
                    "{}. **{}**\n{}\n[More details »]({})\n\n",
                    i + 1,
                    heading,
                    para,
                    u
                )),
                None => body.push_str(&format!("{}. **{}**\n{}\n\n", i + 1, heading, para)),
            }
        }
    }
    body.push_str(
        "If anything's tricky to track down, just send on whatever you can \
         find.\n\nMany thanks,\nAnish",
    );
    body
}

/// Deserialize `selected` leniently: drop any slug this build doesn't know
/// (removed/renamed points, or a newer build's additions), so an evolving
/// catalogue never breaks loading a whole BER record.
fn lenient_points<'de, D>(d: D) -> Result<Vec<FollowUpPointId>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::IntoDeserializer;
    let raw = Vec::<String>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|s| {
            let de: serde::de::value::StrDeserializer<serde::de::value::Error> =
                s.as_str().into_deserializer();
            FollowUpPointId::deserialize(de).ok()
        })
        .collect())
}

/// The follow-up request state that persists with the BER. `initialized`
/// distinguishes "never opened the tab" (seed from [`auto_selected`]) from "the
/// assessor cleared everything". `sent_at` (ISO-8601, stamped by the shell)
/// being `Some` means the email has gone and the button is spent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FollowUpRequest {
    #[serde(default)]
    pub initialized: bool,
    #[serde(default, deserialize_with = "lenient_points")]
    pub selected: Vec<FollowUpPointId>,
    /// Extension phases ticked for the Extension build-date request (the
    /// sub-checks). ExtensionBuildDate is in `selected` iff this is non-empty.
    #[serde(default)]
    pub extensions: Vec<ConstructionPhase>,
    /// Construction group ids ticked for the Builder's-letter request (the
    /// surface sub-checks). BuilderUpgradeLetter is in `selected` iff this is
    /// non-empty.
    #[serde(default)]
    pub builder_surfaces: Vec<ItemId>,
    /// Construction group ids ticked for the CERTIFIER's-letter request — the
    /// SAME offered surface list as the builder's letter
    /// ([`builder_surface_groups`]), ticked independently: the assessor's
    /// split of the surfaces decides who is asked to evidence what (builder
    /// letter vs certifier letter). Unlike `builder_surfaces` this list may
    /// be empty with the point still selected — the certifier request reads
    /// fine without named surfaces (the general new-build wording).
    /// `#[serde(default)]` so older records load.
    #[serde(default)]
    pub certifier_surfaces: Vec<ItemId>,
    /// Window TYPE ids ticked for the Window-documentation request (the
    /// per-type sub-checks, offered when the record has placed windows —
    /// [`candidate_window_types`]). When non-empty, WindowDop's email block
    /// names these types and the backend checklist gets one `window_dop:<id>`
    /// row per tick instead of the single `window_dop` row.
    /// `#[serde(default)]` so older records load.
    #[serde(default)]
    pub window_types: Vec<ItemId>,
    /// Door TYPE names (trimmed) ticked for the Door-documentation request —
    /// offered only when the record has more than one door type
    /// ([`candidate_door_types`]); a single door type keeps the plain row.
    /// Keys are the free-text type names (the label IS the group);
    /// [`Ber::rename_door_type`] remaps them on a rename.
    /// `#[serde(default)]` so older records load.
    ///
    /// [`Ber::rename_door_type`]: super::ber_struct::Ber::rename_door_type
    #[serde(default)]
    pub door_types: Vec<String>,
    /// Per-BER custom request points added via the tab's "+".
    #[serde(default)]
    pub custom: Vec<CustomPoint>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub sent_at: Option<String>,
}

impl FollowUpRequest {
    /// Whether the request email has already been sent (button spent).
    pub fn is_sent(&self) -> bool {
        self.sent_at.is_some()
    }

    /// The requested items as `(item_id, label)`, in draft order — selected
    /// built-in points (by slug) then ticked custom points (by their id). Sent
    /// to the backend so the office checklist mirrors exactly what was asked.
    ///
    /// PER-TYPE expansion (additive wire shapes; same `{itemId, label}` rows):
    /// when window TYPES are ticked, the single `window_dop` row is replaced by
    /// one `window_dop:<type_id>` row per ticked type (candidate order, labels
    /// from [`candidate_window_types`]); ticked door TYPES likewise become
    /// `door_dop:<type name>` rows. With no per-type ticks — or when every
    /// ticked type has gone stale (its window type / door deleted since), or
    /// the record no longer has MORE than one door type (the same gate the
    /// tab and the draft use) — the classic `window_dop` / `door_dop` rows
    /// are sent unchanged, so old records and records without placed openings
    /// keep today's payload exactly and a selected point always yields at
    /// least one row.
    pub fn checklist_items(&self, ber: &Ber) -> Vec<(String, String)> {
        let mut items: Vec<(String, String)> = Vec::new();
        for p in ALL_POINTS.into_iter().filter(|p| self.selected.contains(p)) {
            let per_type: Vec<(String, String)> = match p {
                FollowUpPointId::WindowDop if !self.window_types.is_empty() => {
                    candidate_window_types(ber)
                        .into_iter()
                        .filter(|(id, _)| self.window_types.contains(id))
                        .map(|(id, label)| {
                            (
                                format!("{}:{}", p.slug(), id.0),
                                format!("{} — {label}", p.label()),
                            )
                        })
                        .collect()
                }
                FollowUpPointId::DoorDop if !self.door_types.is_empty() => {
                    // Same >1-types gate as the draft and the tab's sub-rows:
                    // a record whose doors (now) share one type sends the
                    // classic row even if a stale per-type tick lingers.
                    let candidates = candidate_door_types(ber);
                    if candidates.len() > 1 {
                        candidates
                            .into_iter()
                            .filter(|n| self.door_types.contains(n))
                            .map(|name| {
                                (
                                    format!("{}:{name}", p.slug()),
                                    format!("{} — {name}", p.label()),
                                )
                            })
                            .collect()
                    } else {
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            };
            if per_type.is_empty() {
                items.push((p.slug().to_string(), p.label().to_string()));
            } else {
                items.extend(per_type);
            }
        }
        items.extend(
            self.custom
                .iter()
                .filter(|c| c.checked)
                .map(|c| (c.id.to_string(), c.label.clone())),
        );
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::common::ConstructionPhase;

    const REF_YEAR: Year = 2026;

    fn ber_with_year(y: u32, proven: bool) -> Ber {
        let mut ber = Ber::default();
        ber.address = "8 Ashgrove Hill".to_string();
        ber.construction_phases
            .set_year(ConstructionPhase::Original, y);
        ber.construction_phases
            .set_year_proof(ConstructionPhase::Original, proven);
        ber
    }

    /// A minimal placed window referencing shared type `type_id` (per-type
    /// candidates only offer types some window actually uses).
    fn window_of(type_id: ItemId) -> crate::ber::fenestration::Window {
        crate::ber::fenestration::Window {
            id: ItemId::default(),
            vertices: vec![],
            width_m: 1.2,
            height_m: 1.1,
            window_type_id: type_id,
            placement: None,
            wall_anchor: None,
            assembly_id: None,
            multi_storey: false,
            shape: crate::ber::fenestration::WindowShape::Rectangular,
            overshading: Default::default(),
            num_opes: None,
        }
    }

    /// A minimal door with free-text type `name` (the label IS the group).
    fn door_named(name: &str) -> crate::ber::fenestration::Door {
        crate::ber::fenestration::Door {
            id: ItemId::default(),
            type_name: name.to_string(),
            vertices: vec![],
            photos: vec![],
            width_m: 0.9,
            height_m: 2.0,
            kind: None,
            placement: None,
            wall_anchor: None,
            assembly_id: None,
            draught_stripped: None,
        }
    }

    /// A ber with two USED window types (plus an orphan) and doors of two
    /// free-text types, for the per-type window/door row tests.
    fn ber_with_openings() -> (Ber, ItemId, ItemId) {
        use crate::ber::fenestration::{Frame, Glazing};
        let mut ber = ber_with_year(1990, true);
        let front = ber.ensure_window_type(
            Glazing::Double,
            Frame::WoodUPVC,
            2020,
            false,
            true,
            vec![16],
            Some("Rationel front".to_string()),
        );
        let back = ber.ensure_window_type(
            Glazing::Triple,
            Frame::Metal,
            2021,
            false,
            true,
            vec![],
            Some("Velfac back".to_string()),
        );
        // An orphan registry type no window references — must never be offered.
        let orphan = ber.ensure_window_type(
            Glazing::Single,
            Frame::Metal,
            0,
            false,
            true,
            vec![],
            Some("Orphan".to_string()),
        );
        assert_ne!(orphan, front);
        let mut s = crate::ber::structural::Storey::default();
        s.windows.push(window_of(front));
        s.windows.push(window_of(back));
        s.windows.push(window_of(front));
        s.doors.push(door_named("Front door"));
        s.doors.push(door_named("Back door"));
        s.doors.push(door_named("Front door")); // same type twice
        ber.storeys.push(s);
        (ber, front, back)
    }

    /// Per-type candidates: window types restricted to USED ones (registry
    /// order, display labels); door types deduped trimmed names in
    /// first-appearance order.
    #[test]
    fn candidate_window_and_door_types_scope_and_labels() {
        let (ber, front, back) = ber_with_openings();
        let wt = candidate_window_types(&ber);
        assert_eq!(
            wt.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![front, back],
            "used types only — the orphan registry entry is not offered"
        );
        assert!(
            wt[0].1.starts_with("Rationel front — Double · Wood/UPVC"),
            "display label: {}",
            wt[0].1
        );
        assert_eq!(
            candidate_door_types(&ber),
            vec!["Front door".to_string(), "Back door".to_string()],
            "deduped, first-appearance order"
        );
    }

    /// Ticked window/door types are named (bulleted) in their request blocks;
    /// with no per-type ticks the classic general wording stands.
    #[test]
    fn per_type_ticks_render_into_the_draft() {
        let (ber, front, _back) = ber_with_openings();
        let mut fu = FollowUpRequest {
            selected: vec![FollowUpPointId::WindowDop, FollowUpPointId::DoorDop],
            ..Default::default()
        };
        let plain = assemble_draft(&ber, &fu);
        assert!(
            plain.contains("For the windows,") && plain.contains("For the [front / back] door,"),
            "no per-type ticks → classic wording: {plain}"
        );

        fu.window_types = vec![front];
        fu.door_types = vec!["Back door".to_string()];
        let per_type = assemble_draft(&ber, &fu);
        assert!(
            per_type.contains("• Rationel front — Double"),
            "ticked window type named: {per_type}"
        );
        assert!(
            !per_type.contains("Velfac back"),
            "unticked window type NOT named: {per_type}"
        );
        assert!(
            per_type.contains("• Back door") && !per_type.contains("• Front door"),
            "only the ticked door type named: {per_type}"
        );
    }

    /// The backend checklist wire rows: per-type ticks replace the single
    /// `window_dop` / `door_dop` rows with `window_dop:<type_id>` /
    /// `door_dop:<type name>` rows (candidate order); without per-type ticks
    /// the classic single rows are sent byte-identically (wire compat).
    #[test]
    fn checklist_items_expand_per_type_and_stay_compatible_without() {
        let (ber, front, back) = ber_with_openings();
        let mut fu = FollowUpRequest {
            selected: vec![FollowUpPointId::WindowDop, FollowUpPointId::DoorDop],
            ..Default::default()
        };
        let classic = fu.checklist_items(&ber);
        assert_eq!(
            classic
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["window_dop", "door_dop"],
            "no per-type ticks → today's payload exactly"
        );

        fu.window_types = vec![front, back];
        fu.door_types = vec!["Front door".to_string()];
        let items = fu.checklist_items(&ber);
        let ids: Vec<&str> = items.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                format!("window_dop:{}", front.0),
                format!("window_dop:{}", back.0),
                "door_dop:Front door".to_string(),
            ],
            "per-type rows replace the parents"
        );
        assert!(
            items[0]
                .1
                .starts_with("Window DoP + proof of model — Rationel front"),
            "per-type label carries the type: {}",
            items[0].1
        );
    }

    /// Stale per-type ticks (their openings deleted since seeding) fall back
    /// to the CLASSIC wire rows and wording rather than silently dropping the
    /// request: all windows gone → bare `window_dop`; doors collapsed to a
    /// single type → bare `door_dop` (the same >1-types gate the tab uses).
    #[test]
    fn stale_per_type_ticks_fall_back_to_classic_rows() {
        let (mut ber, front, back) = ber_with_openings();
        // Doors collapse to one type; the seeded ticks linger.
        ber.storeys[0].doors.retain(|d| d.type_name != "Back door");
        let fu = FollowUpRequest {
            selected: vec![FollowUpPointId::DoorDop],
            door_types: vec!["Front door".to_string(), "Back door".to_string()],
            ..Default::default()
        };
        assert_eq!(
            fu.checklist_items(&ber)
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["door_dop"],
            "one door type → classic row despite stale ticks"
        );
        assert!(
            assemble_draft(&ber, &fu).contains("For the [front / back] door,"),
            "classic wording too"
        );

        // Every window deleted; the seeded type ticks linger.
        ber.storeys[0].windows.clear();
        let fu = FollowUpRequest {
            selected: vec![FollowUpPointId::WindowDop],
            window_types: vec![front, back],
            ..Default::default()
        };
        assert_eq!(
            fu.checklist_items(&ber)
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["window_dop"],
            "no candidates left → the selected point still yields its classic row"
        );
        assert!(assemble_draft(&ber, &fu).contains("For the windows,"));
    }

    /// A door-type rename re-points the follow-up tick keyed by the old name
    /// (from either the Structure review or the Follow-ups tab — same model
    /// write), so a ticked request never orphans.
    #[test]
    fn door_rename_remaps_followup_door_type_ticks() {
        let (mut ber, _, _) = ber_with_openings();
        ber.followup.door_types = vec!["Front door".to_string(), "Back door".to_string()];
        assert_eq!(ber.rename_door_type("Front door", "Hall door"), 2);
        assert_eq!(
            ber.followup.door_types,
            vec!["Back door".to_string(), "Hall door".to_string()],
            "tick re-keyed to the new name"
        );
        // Renaming INTO an already-ticked name doesn't duplicate the tick.
        assert_eq!(ber.rename_door_type("Hall door", "Back door"), 2);
        assert_eq!(ber.followup.door_types, vec!["Back door".to_string()]);
    }

    #[test]
    fn house_build_date_ticked_unless_proven() {
        assert!(auto_selected(&ber_with_year(1990, false), REF_YEAR)
            .contains(&FollowUpPointId::HouseBuildDate));
        assert!(!auto_selected(&ber_with_year(1990, true), REF_YEAR)
            .contains(&FollowUpPointId::HouseBuildDate));
    }

    #[test]
    fn extension_ticked_when_unproven_and_named_in_draft() {
        let mut ber = ber_with_year(1990, true);
        ber.construction_phases
            .set_year(ConstructionPhase::Extension1, 2005);
        ber.construction_phases
            .set_nickname(ConstructionPhase::Extension1, "Back extension".to_string());
        let sel = auto_selected(&ber, REF_YEAR);
        assert!(sel.contains(&FollowUpPointId::ExtensionBuildDate));
        let fu = FollowUpRequest {
            selected: sel,
            extensions: candidate_extensions(&ber),
            ..Default::default()
        };
        let draft = assemble_draft(&ber, &fu);
        assert!(draft.contains("• Back extension"), "draft: {draft}");
        // Numbered blocks with a bold heading, request paragraph underneath.
        // (House year is proven here, so the extension is the only point.)
        assert!(
            draft.contains("1. **Extension build date**\n"),
            "draft: {draft}"
        );
    }

    #[test]
    fn heating_system_synced_from_boiler_objects() {
        use crate::ber::objects::{BoilerFuel, BuildingObject, ObjectKind, ObjectRoom};
        use crate::ber::systems::HeatingSystemType;
        use domain::points::Point2f32;

        // Heat pump object → HeatPump type → heat-pump follow-up.
        let mut ber = ber_with_year(1990, true);
        let mut room = ObjectRoom::new(Point2f32::new(0.0, 0.0));
        room.objects.push(BuildingObject::new(
            ObjectKind::Boiler(BoilerFuel::HeatPump),
            1,
        ));
        ber.attic_room = Some(room);
        ber.sync_heating_system_from_objects();
        assert!(matches!(
            ber.heating_system.system_type,
            HeatingSystemType::HeatPump
        ));
        assert!(auto_selected(&ber, REF_YEAR).contains(&FollowUpPointId::HeatPumpData));

        // Gas boiler object → GasBoiler type, and NO heat-pump follow-up.
        let mut ber = ber_with_year(1990, true);
        let mut room = ObjectRoom::new(Point2f32::new(0.0, 0.0));
        room.objects
            .push(BuildingObject::new(ObjectKind::Boiler(BoilerFuel::Gas), 1));
        ber.attic_room = Some(room);
        ber.sync_heating_system_from_objects();
        assert!(matches!(
            ber.heating_system.system_type,
            HeatingSystemType::GasBoiler
        ));
        assert!(!auto_selected(&ber, REF_YEAR).contains(&FollowUpPointId::HeatPumpData));

        // Both present → heat pump wins.
        let mut ber = ber_with_year(1990, true);
        let mut room = ObjectRoom::new(Point2f32::new(0.0, 0.0));
        room.objects
            .push(BuildingObject::new(ObjectKind::Boiler(BoilerFuel::Gas), 1));
        room.objects.push(BuildingObject::new(
            ObjectKind::Boiler(BoilerFuel::HeatPump),
            1,
        ));
        ber.attic_room = Some(room);
        ber.sync_heating_system_from_objects();
        assert!(matches!(
            ber.heating_system.system_type,
            HeatingSystemType::HeatPump
        ));
    }

    #[test]
    fn builder_letter_vs_works_evidence_are_exclusive() {
        let mut ber = ber_with_year(1990, true);
        ber.intake.insulation_walls = true;

        // Insulation works, builder-letter answer unset → builder letter.
        let sel = auto_selected(&ber, REF_YEAR);
        assert!(sel.contains(&FollowUpPointId::BuilderUpgradeLetter));
        assert!(!sel.contains(&FollowUpPointId::WorksEvidenceNoBuilder));

        // Builder-letter answer YES (even without insulation) → builder letter.
        let mut ber2 = ber_with_year(1990, true);
        ber2.intake.builder_letter_works = Some(true);
        let sel = auto_selected(&ber2, REF_YEAR);
        assert!(sel.contains(&FollowUpPointId::BuilderUpgradeLetter));

        // Insulation works + builder-letter answer NO → works evidence only.
        ber.intake.builder_letter_works = Some(false);
        let sel = auto_selected(&ber, REF_YEAR);
        assert!(sel.contains(&FollowUpPointId::WorksEvidenceNoBuilder));
        assert!(!sel.contains(&FollowUpPointId::BuilderUpgradeLetter));
    }

    #[test]
    fn window_and_door_and_heatpump_and_recent_house() {
        let mut ber = ber_with_year(2018, true); // proven, but recent → airtightness+acd
        ber.intake.window_year = Some(2020); // window newer than 10 years
        ber.intake.door_year = Some(2020);
        ber.heating_system.system_type = HeatingSystemType::HeatPump;

        let sel = auto_selected(&ber, REF_YEAR);
        assert!(sel.contains(&FollowUpPointId::WindowDop), "recent window");
        assert!(sel.contains(&FollowUpPointId::DoorDop), "recent door");
        assert!(sel.contains(&FollowUpPointId::HeatPumpData));
        assert!(
            sel.contains(&FollowUpPointId::AirtightnessAndAcd),
            "recent house"
        );
    }

    #[test]
    fn old_house_skips_recent_only_points() {
        let ber = ber_with_year(1980, true);
        let sel = auto_selected(&ber, REF_YEAR);
        assert!(!sel.contains(&FollowUpPointId::AirtightnessAndAcd));
        assert!(!sel.contains(&FollowUpPointId::WindowDop));
        assert!(!sel.contains(&FollowUpPointId::DoorDop));
        assert!(!sel.contains(&FollowUpPointId::HeatPumpData));
    }

    #[test]
    fn custom_points_render_into_draft_when_checked() {
        let ber = ber_with_year(1990, true);
        let custom = vec![
            CustomPoint {
                id: ItemId::default(),
                label: "Septic tank cert".to_string(),
                content: "Could you send the septic tank certificate?".to_string(),
                checked: true,
            },
            CustomPoint {
                id: ItemId::default(),
                label: "Unchecked one".to_string(),
                content: "Should not appear.".to_string(),
                checked: false,
            },
        ];
        let fu = FollowUpRequest {
            custom,
            ..Default::default()
        };
        let draft = assemble_draft(&ber, &fu);
        assert!(draft.contains("septic tank certificate"));
        assert!(!draft.contains("Should not appear"));
    }

    #[test]
    fn lenient_deserialize_drops_unknown_slugs() {
        // A record written by a future/older build with slugs this build removed.
        let json = r#"{"initialized":true,"selected":["house_build_date","roof_window_plate","cylinder_invoice","window_dop"],"body":"x","sent_at":null}"#;
        let fu: FollowUpRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            fu.selected,
            vec![FollowUpPointId::HouseBuildDate, FollowUpPointId::WindowDop]
        );
    }

    #[test]
    fn slug_stability() {
        assert_eq!(
            serde_json::to_string(&FollowUpPointId::AirtightnessAndAcd).unwrap(),
            "\"airtightness_and_acd\""
        );
    }

    #[test]
    fn every_point_has_a_hyphenated_doc_slug_and_matching_url() {
        for p in ALL_POINTS {
            let slug = p.doc_slug();
            assert!(!slug.is_empty(), "{p:?} has an empty doc_slug");
            // Client-facing slugs are hyphenated, not underscored (unlike slug()).
            assert!(
                !slug.contains('_') && !slug.contains(' '),
                "{p:?} doc_slug not hyphenated: {slug}"
            );
            assert_eq!(p.doc_url(), format!("https://cannygreen.com/docs/{slug}"));
        }
    }

    #[test]
    fn draft_includes_more_details_link_for_builtin_points() {
        let ber = ber_with_year(1990, false); // unproven → HouseBuildDate selected
        let sel = auto_selected(&ber, REF_YEAR);
        assert!(sel.contains(&FollowUpPointId::HouseBuildDate));
        let fu = FollowUpRequest {
            selected: sel,
            ..Default::default()
        };
        let draft = assemble_draft(&ber, &fu);
        assert!(
            draft.contains("[More details »](https://cannygreen.com/docs/house-build-date)"),
            "draft: {draft}"
        );
    }

    #[test]
    fn water_cylinder_is_manual_only_but_renders_with_link() {
        // Even a recent house + heat pump doesn't auto-tick the cylinder.
        let mut ber = ber_with_year(2018, true);
        ber.heating_system.system_type = HeatingSystemType::HeatPump;
        assert!(!auto_selected(&ber, REF_YEAR).contains(&FollowUpPointId::WaterCylinderModel));
        assert!(!auto_selected(&ber, REF_YEAR).contains(&FollowUpPointId::WaterCylinderInsulation));
        // When manually ticked both render with their heading + explainer link.
        let fu = FollowUpRequest {
            selected: vec![
                FollowUpPointId::WaterCylinderModel,
                FollowUpPointId::WaterCylinderInsulation,
            ],
            ..Default::default()
        };
        let draft = assemble_draft(&ber, &fu);
        assert!(draft.contains("**Cylinder model**"), "draft: {draft}");
        assert!(draft.contains("**Cylinder insulation**"), "draft: {draft}");
        assert!(
            draft.contains("https://cannygreen.com/docs/hot-water-cylinder"),
            "draft: {draft}"
        );
    }

    #[test]
    fn custom_points_have_no_more_details_link() {
        let ber = ber_with_year(1990, true);
        let custom = vec![CustomPoint {
            id: ItemId::default(),
            label: "Septic tank cert".to_string(),
            content: "Could you send the septic tank certificate?".to_string(),
            checked: true,
        }];
        let fu = FollowUpRequest {
            custom,
            ..Default::default()
        };
        let draft = assemble_draft(&ber, &fu);
        assert!(draft.contains("septic tank certificate"));
        assert!(
            !draft.contains("More details"),
            "custom points have no explainer page: {draft}"
        );
    }
    /// The letter sub-checks mirror the STRUCTURE REVIEW: one row per
    /// construction group, offered only when an EXISTING in-scope (heat-loss)
    /// region references it. Seeds/orphans and groups referenced only by
    /// buried (inter-storey) surfaces never show — the 20-row messy-record
    /// inflation of field feedback 2026-07-24.
    #[test]
    fn builder_surfaces_mirror_the_structure_review_scope() {
        use crate::ber::structural::{
            CeilingConstructionType, CeilingExposure, CeilingRegion, Opes, Polygon3D, Storey,
            WallConstructionType, WallExposure, WallRegion,
        };
        use domain::points::Point3f64;

        let mut ber = Ber::default();
        let used_wall = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Unknown,
            ConstructionPhase::Original,
        );
        // A second group of a DIFFERENT type that no region references — the
        // find-or-create seed / re-grouping orphan case.
        let orphan_wall = ber.ensure_wall_construction(
            WallConstructionType::TimberConstruction,
            ConstructionMakeup::Unknown,
            ConstructionPhase::Original,
        );
        // A ceiling group referenced ONLY by a buried inter-storey region.
        let buried_ceiling = ber.ensure_ceiling_construction(
            CeilingConstructionType::FlatRoof,
            ConstructionMakeup::Unknown,
            ConstructionPhase::Original,
        );
        let quad = |z: f64| Polygon3D {
            exterior: vec![
                Point3f64::new(0.0, 0.0, z),
                Point3f64::new(4.0, 0.0, z),
                Point3f64::new(4.0, 0.0, z + 2.4),
                Point3f64::new(0.0, 0.0, z + 2.4),
            ],
        };
        let mut s = Storey::default();
        s.walls.regions.push(WallRegion {
            id: ItemId::default(),
            polygon: quad(0.0),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: used_wall,
        });
        s.ceilings.regions.push(CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D {
                exterior: vec![
                    Point3f64::new(0.0, 0.0, 2.4),
                    Point3f64::new(4.0, 0.0, 2.4),
                    Point3f64::new(4.0, 4.0, 2.4),
                    Point3f64::new(0.0, 4.0, 2.4),
                ],
            },
            opes: Opes::default(),
            construction_id: buried_ceiling,
            exposure: CeilingExposure::InterStorey,
        });
        ber.storeys.push(s);

        let ids: Vec<ItemId> = builder_surface_groups(&ber).iter().map(|g| g.id).collect();
        assert_eq!(
            ids,
            vec![used_wall],
            "only the group an existing in-scope surface references is offered \
             (orphan {orphan_wall:?} and buried {buried_ceiling:?} are not)"
        );
    }
    /// The certifier's letter offers the SAME surface list as the builder's
    /// letter, ticked independently — the assessor's split decides who
    /// evidences what. Ticked certifier surfaces are named in the draft; with
    /// none ticked the general wording stands.
    #[test]
    fn certifier_letter_names_its_own_ticked_surfaces() {
        use crate::ber::structural::{
            Opes, Polygon3D, WallConstructionType, WallExposure, WallRegion,
        };
        use domain::points::Point3f64;

        let mut ber = ber_with_year(1990, true);
        let wall = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Unknown,
            ConstructionPhase::Extension1,
        );
        let mut s = crate::ber::structural::Storey::default();
        s.walls.regions.push(WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D {
                exterior: vec![
                    Point3f64::new(0.0, 0.0, 0.0),
                    Point3f64::new(4.0, 0.0, 0.0),
                    Point3f64::new(4.0, 0.0, 2.4),
                    Point3f64::new(0.0, 0.0, 2.4),
                ],
            },
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: wall,
        });
        ber.storeys.push(s);

        let mut fu = FollowUpRequest {
            selected: vec![FollowUpPointId::CertifierNewbuildLetter],
            ..FollowUpRequest::default()
        };
        let plain = assemble_draft(&ber, &fu);
        assert!(
            !plain.contains("needs to cover the build-up"),
            "no surfaces ticked → general wording"
        );
        fu.certifier_surfaces = vec![wall];
        let named = assemble_draft(&ber, &fu);
        assert!(
            named.contains("needs to cover the build-up") && named.contains("• Cavity"),
            "ticked surface named in the certifier request: {named}"
        );
    }
}
