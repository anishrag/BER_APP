//! Post-survey client follow-up requests.
//!
//! After a survey there are usually a handful of documents/details still needed
//! from the client to resolve unknowns and use better-than-default values (DoPs,
//! build-date proof, builder letters, …). The assessor ticks the ones that
//! apply on the "Follow ups" tab and one email goes out to the client.
//!
//! This module is the pure-data heart of that feature (per the data-model
//! placement rule): the catalogue of request points, the survey-driven
//! auto-selection, and the assembly of the editable email draft. The catalogue
//! wording comes from mining ~600 of the assessor's past client emails (see the
//! `followup-email-points` project notes). Networking (the actual send) and the
//! one-shot lock live in the app shell; only the [`FollowUpRequest`] record that
//! persists with the BER lives here.

use serde::{Deserialize, Serialize};

use super::ber_struct::Ber;
use super::common::ConstructionPhase;

/// A build year at or after this counts as a "recent build" whose original
/// fabric wants certifier (not builder) evidence — mirrors the Key-Details
/// architect-letter threshold.
const RECENT_BUILD_YEAR: super::common::Year = 2010;

/// The four dated-extension phases, in order.
const EXTENSION_PHASES: [ConstructionPhase; 4] = [
    ConstructionPhase::Extension1,
    ConstructionPhase::Extension2,
    ConstructionPhase::Extension3,
    ConstructionPhase::Extension4,
];

/// One post-survey request point. Serialized slug matches the master-list id
/// (e.g. `window_dop`) so a stored selection is stable across releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpPointId {
    SolarDocs,
    HeatPumpData,
    AirtightnessReport,
    AcdCertification,
    WindowDop,
    DoorDop,
    RoofWindowPlate,
    HouseBuildDate,
    ExtensionBuildDate,
    BuilderUpgradeLetter,
    WorksEvidenceNoBuilder,
    CertifierNewbuildLetter,
    CylinderInvoice,
    HeatingControls,
}

/// Every point, in the order the UI lists them.
pub const ALL_POINTS: [FollowUpPointId; 14] = [
    FollowUpPointId::HouseBuildDate,
    FollowUpPointId::ExtensionBuildDate,
    FollowUpPointId::BuilderUpgradeLetter,
    FollowUpPointId::WorksEvidenceNoBuilder,
    FollowUpPointId::CertifierNewbuildLetter,
    FollowUpPointId::WindowDop,
    FollowUpPointId::DoorDop,
    FollowUpPointId::RoofWindowPlate,
    FollowUpPointId::SolarDocs,
    FollowUpPointId::HeatPumpData,
    FollowUpPointId::AirtightnessReport,
    FollowUpPointId::AcdCertification,
    FollowUpPointId::CylinderInvoice,
    FollowUpPointId::HeatingControls,
];

impl FollowUpPointId {
    /// Short label for the checkbox.
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
            RoofWindowPlate => "Roof-window model plate photo",
            SolarDocs => "Solar PV documentation",
            HeatPumpData => "Heat pump data (sign-off + datasheet)",
            AirtightnessReport => "Airtightness report",
            AcdCertification => "ACD / thermal-bridging certification",
            CylinderInvoice => "Hot-water cylinder invoice",
            HeatingControls => "Heating zones / thermostats",
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
                     similar would all help — even the serial plate on a roof window from that \
                     period can serve as evidence. Without proof I have to assume it was \
                     constructed along with the original house, and the default values would be \
                     considerably worse."
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
            RoofWindowPlate => "Could you send me a photo of the model plate of the roof window \
                in [location]? If you open the window, there's a plate visible on the edge \
                showing the model number. This lets me use the actual specs instead of the \
                defaults — and it can also help evidence the installation date."
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
            AirtightnessReport => "Was an airtightness test (air permeability test) ever carried \
                out on the house? These are usually done for new builds and sometimes after deep \
                retrofits. If you have the test report, could you send it on — a measured result \
                almost always improves the BER compared to the default assumption I'd otherwise \
                have to use."
                .to_string(),
            AcdCertification => "To use the improved thermal-bridging factor in the calculation, \
                the SEAI require evidence that the house was built to the Acceptable \
                Construction Details. This means construction drawings showing all the junctions \
                in the dwelling, a schedule of junctions, and a signed confirmation from a \
                registered architect, chartered engineer or building surveyor of the standard \
                the house was built to. Drawings alone unfortunately aren't sufficient. Without \
                this documentation I have to use the default factor — it's worth checking with \
                [architect / engineer / certifier] whether these exist for your house."
                .to_string(),
            CylinderInvoice => "The hot water cylinder looked quite new — if you have the \
                invoice or spec sheet for it, could you send it on? The details I need are the \
                volume and the manufacturer's declared standing heat loss (or the insulation \
                thickness). A photo of the label on the cylinder itself would also work. Without \
                this I have to use an age-based default, which is usually worse than the actual \
                figures."
                .to_string(),
            HeatingControls => "Could you confirm how many heating zones there are in the house \
                and whether there are room thermostats controlling them? If there's more than \
                one zone, a photo of the controls (or a screenshot if it's controlled through an \
                app — the app name is useful too) would let me include this as evidence. Proper \
                zone control makes a real difference to the rating."
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

/// The survey-driven initial selection. Only points we can infer from data the
/// survey captures today are ticked; the rest start unticked for the assessor
/// to add by hand (see the tab). Rules:
/// - house build date: unless the year is marked proven;
/// - extension build date: any defined extension whose year isn't proven;
/// - builder letter: insulation works done AND a builder letter is available;
/// - works evidence (2 proofs): insulation works done AND no builder letter;
/// - certifier new-build letter: an architect/certifier letter was flagged
///   available, or the build is recent.
pub fn auto_selected(ber: &Ber) -> Vec<FollowUpPointId> {
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

    let insulation_works =
        intake.insulation_walls || intake.insulation_roof || intake.insulation_floor;
    // "Available" if either builder-letter answer is a yes; "unavailable" only
    // when an answer is an explicit no. An unanswered question stays silent.
    let builder_available =
        intake.builder_letter_works == Some(true) || intake.builder_letter_possible == Some(true);
    let builder_unavailable =
        intake.builder_letter_works == Some(false) || intake.builder_letter_possible == Some(false);
    if insulation_works && builder_available {
        out.push(BuilderUpgradeLetter);
    } else if insulation_works && builder_unavailable {
        out.push(WorksEvidenceNoBuilder);
    }

    let recent_build = phases
        .year_of(&ConstructionPhase::Original)
        .is_some_and(|y| y >= RECENT_BUILD_YEAR);
    if intake.architect_letter_original == Some(true)
        || intake.architect_letter_extension == Some(true)
        || recent_build
    {
        out.push(CertifierNewbuildLetter);
    }

    out
}

/// Assemble the editable email body for the selected points, in catalogue
/// order. A greeting + intro, each point as its own paragraph, then a sign-off.
pub fn assemble_draft(ber: &Ber, selected: &[FollowUpPointId]) -> String {
    let mut body = String::from(
        "Hi,\n\nThanks again for having me out to survey your home for the BER assessment.\n\n",
    );
    let chosen: Vec<FollowUpPointId> = ALL_POINTS
        .into_iter()
        .filter(|p| selected.contains(p))
        .collect();
    if chosen.is_empty() {
        body.push_str(
            "I have everything I need for now — I'll be in touch if anything else comes up.\n\n",
        );
    } else {
        body.push_str(
            "To finish the assessment as accurately as possible, there are a few details I'd be \
             grateful for your help with:\n\n",
        );
        for p in chosen {
            body.push_str(&p.render(ber));
            body.push_str("\n\n");
        }
    }
    body.push_str(
        "If anything's tricky to track down, just let me know and we can work through it \
         together.\n\nMany thanks,\nAnish",
    );
    body
}

/// The follow-up request state that persists with the BER. `initialized`
/// distinguishes "never opened the tab" (seed from [`auto_selected`]) from "the
/// assessor cleared everything". `sent_at` (ISO-8601, stamped by the shell)
/// being `Some` means the email has gone and the button is spent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FollowUpRequest {
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub selected: Vec<FollowUpPointId>,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::common::ConstructionPhase;

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
        assert!(
            auto_selected(&ber_with_year(1990, false)).contains(&FollowUpPointId::HouseBuildDate)
        );
        assert!(
            !auto_selected(&ber_with_year(1990, true)).contains(&FollowUpPointId::HouseBuildDate)
        );
    }

    #[test]
    fn extension_ticked_when_unproven_and_named_in_draft() {
        let mut ber = ber_with_year(1990, true);
        ber.construction_phases
            .set_year(ConstructionPhase::Extension1, 2005);
        ber.construction_phases
            .set_nickname(ConstructionPhase::Extension1, "Back extension".to_string());
        let sel = auto_selected(&ber);
        assert!(sel.contains(&FollowUpPointId::ExtensionBuildDate));
        let draft = assemble_draft(&ber, &sel);
        assert!(draft.contains("the Back extension"), "draft: {draft}");
    }

    #[test]
    fn builder_letter_vs_works_evidence_are_exclusive() {
        let mut ber = ber_with_year(1990, true);
        ber.intake.insulation_walls = true;

        ber.intake.builder_letter_works = Some(true);
        let sel = auto_selected(&ber);
        assert!(sel.contains(&FollowUpPointId::BuilderUpgradeLetter));
        assert!(!sel.contains(&FollowUpPointId::WorksEvidenceNoBuilder));

        ber.intake.builder_letter_works = Some(false);
        ber.intake.builder_letter_possible = Some(false);
        let sel = auto_selected(&ber);
        assert!(sel.contains(&FollowUpPointId::WorksEvidenceNoBuilder));
        assert!(!sel.contains(&FollowUpPointId::BuilderUpgradeLetter));
    }

    #[test]
    fn draft_orders_points_and_round_trips_selection() {
        let ber = ber_with_year(2015, false); // recent build → certifier too
        let sel = auto_selected(&ber);
        let draft = assemble_draft(&ber, &sel);
        assert!(draft.starts_with("Hi,"));
        assert!(draft.contains("Many thanks,\nAnish"));
        // Serialized ids stay stable snake_case slugs.
        let json = serde_json::to_string(&FollowUpPointId::WindowDop).unwrap();
        assert_eq!(json, "\"window_dop\"");
    }

    #[test]
    fn empty_selection_reads_as_all_good() {
        let ber = ber_with_year(1990, true);
        let draft = assemble_draft(&ber, &[]);
        assert!(draft.contains("everything I need for now"));
    }
}
