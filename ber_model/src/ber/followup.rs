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
}

/// Every built-in point, in the order the UI lists them.
pub const ALL_POINTS: [FollowUpPointId; 10] = [
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
        }
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
        }
    }

    /// The request paragraph for this point, with `[bracketed]` prompts where a
    /// value has to be filled in by hand and survey values substituted where we
    /// can (build years, extension names). Rendered into the editable draft.
    pub fn render(self, ber: &Ber) -> String {
        use FollowUpPointId::*;
        match self {
            HouseBuildDate => {
                let believed = match ber
                    .construction_phases
                    .year_of(&ConstructionPhase::Original)
                {
                    Some(y) => format!(" (I believe around {y})"),
                    None => String::new(),
                };
                format!(
                    "Could you confirm the year the house was built{believed}? This date is \
                     quite important — the age band determines the default insulation values \
                     for parts of the house that can't be verified during the survey. If you \
                     have any proof, ideally a legal or conveyancing document, that would be \
                     best. Another option is a confirmation from ESB of the date they first \
                     began supplying the house."
                )
            }
            ExtensionBuildDate => {
                let which = extension_phrase(ber);
                format!(
                    "Could you also share the year {which} was built, along with any proof of \
                     the date if you can find it? An invoice, planning document, dated photos or \
                     similar would all help. Without proof I have to assume it was constructed \
                     along with the original house, and the default values would be considerably \
                     worse."
                )
            }
            BuilderUpgradeLetter => "For the upgrade works, could you get a signed letter from \
                [builder / contractor] on their headed paper confirming the works? It needs to \
                include the full property address, the signer's name and job title, and state \
                the build-up of the upgraded elements with the areas covered (e.g. [100mm PIR \
                on all exterior walls]). To make it easier I can draft the letter with the \
                details filled in — they'd just need to put it on their letterhead, check it \
                against what was done, complete the highlighted parts and sign."
                .to_string(),
            WorksEvidenceNoBuilder => "For the [works], since a letter from the builder isn't \
                available, the SEAI require two independent proofs of the works instead. These \
                can be: photos taken while the work was being done, and/or invoices, receipts or \
                emails for the materials — ideally showing the site address and the product \
                used. If you can dig out two of these, that will cover the evidence requirements."
                .to_string(),
            CertifierNewbuildLetter => "For the [new-build elements], letters from builders \
                unfortunately can't be accepted by the SEAI — I would need documentation from \
                the certifying architect, engineer or assigned certifier: as-built drawings \
                signed off by them, or a signed letter confirming the build-ups as actually \
                built and that they comply with the building regulations of the time. I can \
                prepare a draft letter with the construction details already filled in — they'd \
                just need to put it on their letterhead, verify it against what was built, \
                complete the highlighted fields and sign."
                .to_string(),
            WindowDop => "For the windows, to use better-than-default values I would need two \
                things: (1) the Declaration of Performance for the windows, showing the U-value \
                and solar transmittance — [window supplier] should be able to provide this if \
                you contact them (I need the standard certified product document, not one \
                generated specifically for your house; NSAI WEP or BFRC certification also \
                works); and (2) something linking that model to your house — an invoice, order \
                confirmation or email from the supplier listing the models supplied to your \
                address."
                .to_string(),
            DoorDop => "For the [front / back] door, similarly, I would need: (1) its \
                Declaration of Performance showing the U-value — available from [door supplier] \
                if you don't have it; and (2) proof of the model installed at your address, such \
                as an invoice or order confirmation. If there's a sticker or marking on the door \
                itself showing the make/model, a photo of that would also help."
                .to_string(),
            SolarDocs => "Could you send me any documentation you have for the solar panels? The \
                best document is the Declaration of Works from the installer, as it has \
                everything required. Otherwise, an invoice or sign-off from [installer] on \
                headed paper (or a branded email) stating the make, model and number of panels \
                installed at the house would work. Without documentation I have to assume \
                default panels, which understates the benefit."
                .to_string(),
            HeatPumpData => "For the heat pump, could you check if you have: (1) the Designer / \
                Installer sign-off sheet from [installer] — any heat pump installed after 2015 \
                (and all grant installations) should have one, and it contains values like the \
                design flow temperature that usually give a better rating than the defaults; and \
                (2) the Ecodesign label / technical datasheet for [heat pump make/model] — the \
                manufacturer is required to provide this, and it shows the tested efficiency \
                figures I need. If you can't find them, I'm happy to contact the installer \
                directly on your behalf."
                .to_string(),
            AirtightnessAndAcd => "As the house is relatively new, two pieces of documentation \
                would let me improve the rating if they exist: (1) an airtightness (air \
                permeability) test report — new builds usually have one, and a measured result \
                almost always beats the default; and (2) evidence the house was built to the \
                Acceptable Construction Details for thermal bridging — construction drawings \
                showing the junctions, a schedule of junctions, and a signed confirmation from a \
                registered architect, chartered engineer or building surveyor (drawings alone \
                aren't enough). It's worth checking with [architect / engineer / certifier] \
                whether these exist for your house."
                .to_string(),
        }
    }
}

/// Names the extension(s) still needing a dated proof, for the request line —
/// e.g. "the Back extension", "the Back extension and the garage conversion",
/// or a plain "the extension" when none are nicknamed.
fn extension_phrase(ber: &Ber) -> String {
    let names: Vec<String> = EXTENSION_PHASES
        .iter()
        .filter(|p| ber.construction_phases.is_defined(p))
        .filter(|p| !ber.construction_phases.has_year_proof(p))
        .filter_map(|p| {
            ber.construction_phases
                .nickname_of(p)
                .filter(|n| !n.trim().is_empty())
                .map(|n| format!("the {n}"))
        })
        .collect();
    match names.len() {
        0 => "the extension".to_string(),
        1 => names[0].clone(),
        _ => {
            let (last, rest) = names.split_last().unwrap();
            format!("{} and {}", rest.join(", "), last)
        }
    }
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
pub fn assemble_draft(ber: &Ber, selected: &[FollowUpPointId], custom: &[CustomPoint]) -> String {
    let mut body = String::from(
        "Hi,\n\nThanks again for having me out to survey your home for the BER assessment.\n\n",
    );
    // Each point is (heading, request paragraph).
    let mut points: Vec<(String, String)> = ALL_POINTS
        .into_iter()
        .filter(|p| selected.contains(p))
        .map(|p| (p.heading().to_string(), p.render(ber)))
        .collect();
    points.extend(
        custom
            .iter()
            .filter(|c| c.checked && !c.content.trim().is_empty())
            .map(|c| (c.label.clone(), c.content.clone())),
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
        // the numbered line, then the full request underneath.
        for (i, (heading, para)) in points.iter().enumerate() {
            body.push_str(&format!("{}. **{}**\n{}\n\n", i + 1, heading, para));
        }
    }
    body.push_str(
        "If anything's tricky to track down, just let me know and we can work through it \
         together.\n\nMany thanks,\nAnish",
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
    pub fn checklist_items(&self) -> Vec<(String, String)> {
        let mut items: Vec<(String, String)> = ALL_POINTS
            .into_iter()
            .filter(|p| self.selected.contains(p))
            .map(|p| (p.slug().to_string(), p.label().to_string()))
            .collect();
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
        let draft = assemble_draft(&ber, &sel, &[]);
        assert!(draft.contains("the Back extension"), "draft: {draft}");
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
        let draft = assemble_draft(&ber, &[], &custom);
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
}
