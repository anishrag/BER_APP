use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::Deref;

use uuid::Uuid;

pub type Year = u32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ItemId(pub Uuid);

impl Default for ItemId {
    fn default() -> Self {
        ItemId(Uuid::new_v4())
    }
}

impl ItemId {
    /// The stable NIL id — "references nothing": what the inter-storey
    /// derivation clears an out-of-scope region's `construction_id` to
    /// (stable across derives, unlike the random `default()`).
    pub fn nil() -> Self {
        ItemId(Uuid::nil())
    }
}

impl Deref for ItemId {
    type Target = Uuid;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

use std::str::FromStr;

impl FromStr for ItemId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(ItemId)
    }
}

/// Which elevation an external photo documents. An external photo is a
/// [`Evidence`] whose `side` is `Some(_)`; the four sides together make the
/// Mapping "External photos" checklist row complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExternalSide {
    Front,
    Right,
    Left,
    Back,
}

impl ExternalSide {
    /// Every side, in canonical display order (Front, Right, Left, Back).
    pub const ALL: [ExternalSide; 4] = [
        ExternalSide::Front,
        ExternalSide::Right,
        ExternalSide::Left,
        ExternalSide::Back,
    ];

    /// Human label for the side ("Front", "Right", …).
    pub fn label(&self) -> &'static str {
        match self {
            ExternalSide::Front => "Front",
            ExternalSide::Right => "Right",
            ExternalSide::Left => "Left",
            ExternalSide::Back => "Back",
        }
    }

    /// Whether a photo of this side is REQUIRED for the External-photos gate.
    /// Front + Back are mandatory; Left + Right are optional (often not
    /// accessible on a mid-terrace or a tight boundary). Single source of truth
    /// for both the completeness gate and the UI's required/optional labelling.
    pub fn photo_required(self) -> bool {
        matches!(self, ExternalSide::Front | ExternalSide::Back)
    }

    /// Whether the assessor may DEFER this side's photo to the end of the
    /// assessment (the per-side "Defer" button on the external-photos panel).
    /// A deferred side counts as SET for the completeness gates; the Final
    /// phase then re-prompts for the deferred shots and blocks "BER complete"
    /// until they're captured (defer = postpone, never skip).
    ///
    /// The FRONT is deliberately NOT deferrable — this asymmetry is domain
    /// logic, don't "simplify" it away: deferring exists for apartments where
    /// you can't tell which unit is which from the outside until you've been
    /// inside — but you always know the front, because you walk in through
    /// it, so the front photo is always takeable up-front.
    pub fn photo_deferrable(self) -> bool {
        !matches!(self, ExternalSide::Front)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Evidence {
    pub id: ItemId,
    pub file_path: String, // local path to the full-resolution evidence file
    /// Local path to a small downscaled preview, generated at attach time.
    /// Images only; `None` for documents (e.g. a PDF) and for older photos
    /// (fall back to `file_path`) — rendering the full-res original as a
    /// thumbnail is slow and memory-heavy on a tablet.
    #[serde(default)]
    pub thumbnail_path: Option<String>,
    pub description: Option<String>,
    /// Which external elevation this photo documents, if it's an EXTERNAL
    /// picture. `None` for ordinary item photos. `serde(default)` so records
    /// written before this field load with `None`.
    #[serde(default)]
    pub side: Option<ExternalSide>,
    /// MIME type of the evidence file. `None` on legacy records and on all
    /// tablet-captured photos, which are images — the effective type is then
    /// inferred from the file extension (see [`Evidence::content_type`]). Set
    /// explicitly (e.g. "application/pdf") for non-image evidence such as a
    /// Declaration of Performance threaded in as a document.
    #[serde(default)]
    pub content_type: Option<String>,
}

impl Evidence {
    /// The effective MIME type: the explicit `content_type` if set, otherwise
    /// inferred from the `file_path` extension (the legacy / tablet path),
    /// defaulting to JPEG. Mirrors the png/webp/svg/jpeg mapping used by the
    /// uploader and the report renderer.
    pub fn content_type(&self) -> &str {
        if let Some(ct) = self.content_type.as_deref() {
            if !ct.is_empty() {
                return ct;
            }
        }
        let ext = self
            .file_path
            .rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match ext.as_str() {
            "png" => "image/png",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "pdf" => "application/pdf",
            _ => "image/jpeg",
        }
    }

    /// Whether this evidence is a raster image (embeddable inline) as opposed to
    /// a document (which callers should link rather than embed). Non-image
    /// evidence has no `thumbnail_path`.
    pub fn is_image(&self) -> bool {
        self.content_type().starts_with("image/")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum ConstructionPhase {
    /// Untagged base fabric: skipped implicit default, never year-dated, never
    /// offered in the phase picker. The starting state of a region until the
    /// assessor assigns it a dated phase.
    #[default]
    Unassigned,
    Original,
    Extension1,
    Extension2,
    Extension3,
    Extension4,
    Other(String),
}

impl ConstructionPhase {
    /// The phase's built-in display name ("Original", "Extension 1", …) —
    /// what the UI shows when no nickname has been given.
    pub fn default_name(&self) -> String {
        match self {
            ConstructionPhase::Unassigned => "Unassigned".to_string(),
            ConstructionPhase::Original => "Original".to_string(),
            ConstructionPhase::Extension1 => "Extension 1".to_string(),
            ConstructionPhase::Extension2 => "Extension 2".to_string(),
            ConstructionPhase::Extension3 => "Extension 3".to_string(),
            ConstructionPhase::Extension4 => "Extension 4".to_string(),
            ConstructionPhase::Other(s) => s.clone(),
        }
    }
}

/// BER-level construction dates mapping
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConstructionPhases {
    /// The DEFINED phases and their build years. A key's presence means the
    /// phase exists (it's offered in the phase picker); the value is
    /// `Some(year)` when the year is known and `None` when the assessor has
    /// designated the phase but marked its year UNKNOWN (to be filled in
    /// later). Old records — whose values were plain years — deserialize as
    /// `Some(year)` (serde reads a bare number into `Option` as `Some`).
    pub years: BTreeMap<ConstructionPhase, Option<Year>>,
    /// User-given nicknames per phase (e.g. "Back extension", "over garage") —
    /// what the UI shows instead of "Extension 1". Optional per phase.
    #[serde(default)]
    pub nicknames: BTreeMap<ConstructionPhase, String>,
    /// Per phase: the assessor found SUFFICIENT ONLINE PROOF of this phase's
    /// build year (the Key-Details "got proof" checkbox), so the client needn't
    /// be asked to evidence the date. A phase absent from the map (or `false`)
    /// means no proof recorded. `#[serde(default)]` so older records load with
    /// no proof flags.
    #[serde(default)]
    pub year_proof: BTreeMap<ConstructionPhase, bool>,
    /// Per phase: evidence artifacts backing the chosen build year — the
    /// screenshots the office captures while confirming the age (historical
    /// maps, aerial imagery, planning records, TABULA reference sheets), each
    /// with a description. Distinct from [`year_proof`], which is merely a
    /// "proof exists" flag; this holds the actual documents the report later
    /// reproduces. A phase absent from the map has no evidence.
    /// `#[serde(default)]` so older records load with none.
    #[serde(default)]
    pub year_evidence: BTreeMap<ConstructionPhase, Vec<Evidence>>,
}

impl ConstructionPhases {
    pub fn new(years: BTreeMap<ConstructionPhase, Option<Year>>) -> Self {
        Self {
            years,
            nicknames: BTreeMap::new(),
            year_proof: BTreeMap::new(),
            year_evidence: BTreeMap::new(),
        }
    }

    /// Evidence backing `phase`'s build year, in attach order (empty if none).
    pub fn year_evidence_of(&self, phase: &ConstructionPhase) -> &[Evidence] {
        self.year_evidence
            .get(phase)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Attach one evidence item to `phase`'s build year.
    pub fn add_year_evidence(&mut self, phase: ConstructionPhase, evidence: Evidence) {
        self.year_evidence.entry(phase).or_default().push(evidence);
    }

    /// Remove the evidence item with `id` from `phase` (no-op if absent);
    /// drops the phase's entry once its last item is gone.
    pub fn remove_year_evidence(&mut self, phase: &ConstructionPhase, id: ItemId) {
        if let Some(v) = self.year_evidence.get_mut(phase) {
            v.retain(|e| e.id != id);
            if v.is_empty() {
                self.year_evidence.remove(phase);
            }
        }
    }

    /// Record whether the assessor has online proof of `phase`'s build year.
    /// `false` removes the entry (keeps the map to only the proven phases).
    pub fn set_year_proof(&mut self, phase: ConstructionPhase, proof: bool) {
        if proof {
            self.year_proof.insert(phase, true);
        } else {
            self.year_proof.remove(&phase);
        }
    }

    /// True when the assessor has recorded online proof of `phase`'s year.
    pub fn has_year_proof(&self, phase: &ConstructionPhase) -> bool {
        self.year_proof.get(phase).copied().unwrap_or(false)
    }

    /// Set (or clear, if empty) the nickname for `phase`.
    pub fn set_nickname(&mut self, phase: ConstructionPhase, nickname: String) {
        if nickname.trim().is_empty() {
            self.nicknames.remove(&phase);
        } else {
            self.nicknames.insert(phase, nickname);
        }
    }

    /// The nickname for `phase`, if one was given.
    pub fn nickname_of(&self, phase: &ConstructionPhase) -> Option<&str> {
        self.nicknames.get(phase).map(|s| s.as_str())
    }

    /// Non-panicking lookup of a phase's KNOWN year. `None` both when the
    /// phase isn't defined and when it's defined with an unknown year — use
    /// [`is_defined`](Self::is_defined) / [`year_is_unknown`](Self::year_is_unknown)
    /// to tell those apart.
    pub fn year_of(&self, phase: &ConstructionPhase) -> Option<Year> {
        self.years.get(phase).copied().flatten()
    }

    /// True when `phase` has been designated at all — with a known year OR an
    /// unknown one. This (not a known year) is what makes a phase assignable,
    /// so "unknown" counts as answered for every year-requiring gate.
    pub fn is_defined(&self, phase: &ConstructionPhase) -> bool {
        self.years.contains_key(phase)
    }

    /// True when `phase` is defined but its year was marked UNKNOWN (the
    /// Key-Details "unknown" checkbox) — displayed as "unk", fillable later.
    pub fn year_is_unknown(&self, phase: &ConstructionPhase) -> bool {
        matches!(self.years.get(phase), Some(None))
    }

    /// Set (or replace) the year for `phase`. Filling in a real year over a
    /// previously-unknown one goes through here too.
    pub fn set_year(&mut self, phase: ConstructionPhase, year: Year) {
        self.years.insert(phase, Some(year));
    }

    /// Define `phase` with an UNKNOWN year: it becomes assignable now and the
    /// real year can be set later via [`set_year`](Self::set_year).
    pub fn set_year_unknown(&mut self, phase: ConstructionPhase) {
        self.years.insert(phase, None);
    }

    /// A human label for `phase`: the user's nickname if given (e.g. "Back
    /// extension"), else the default name ("Original", "Extension 1"), with
    /// the year appended when the phase is defined — the known year, or "unk"
    /// while it's still unknown. E.g. "Back extension (2008)",
    /// "Original (1985)", "Extension 1 (unk)".
    pub fn display_label(&self, phase: &ConstructionPhase) -> String {
        let name = self
            .nickname_of(phase)
            .map(|s| s.to_string())
            .unwrap_or_else(|| phase.default_name());
        match self.years.get(phase) {
            Some(Some(year)) => format!("{name} ({year})"),
            Some(None) => format!("{name} (unk)"),
            None => name,
        }
    }

    /// The phases that have a year set, in canonical order (Original, then
    /// Extension1..4, then any `Other`). This is the list the assessor may
    /// assign to an area — only construction dates entered up front are
    /// selectable.
    pub fn defined_phases(&self) -> Vec<ConstructionPhase> {
        let mut out: Vec<ConstructionPhase> = [
            ConstructionPhase::Original,
            ConstructionPhase::Extension1,
            ConstructionPhase::Extension2,
            ConstructionPhase::Extension3,
            ConstructionPhase::Extension4,
        ]
        .into_iter()
        .filter(|p| self.years.contains_key(p))
        .collect();
        for k in self.years.keys() {
            if matches!(k, ConstructionPhase::Other(_)) {
                out.push(k.clone());
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Radiator {
    pub with_trv: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChimneyFuel {
    Gas,
    SolidFuel,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chimney {
    pub open: bool,
    pub fuel_type: ChimneyFuel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeLight {
    Incandescent,
    Halogen,
    HalogenLV,
    LinearFlourescent,
    LEDCFL,
    Other(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Light {
    pub count: u32,
    pub light_type: TypeLight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thermostat {
    pub delayed_start: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vent {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FanType {
    Kitchen,
    Bathroom,
    Other(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fan {
    pub fan_type: FanType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ItemType {
    Thermostat(Thermostat),
    Vent(Vent),
    Fan(Fan),
    Radiator(Radiator),
    Chimney(Chimney),
    Light(Light),
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub name: String,
    pub item_type: ItemType,
    pub photos: Vec<Evidence>,
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_side_all_has_four() {
        assert_eq!(ExternalSide::ALL.len(), 4);
    }

    #[test]
    fn photo_side_round_trips() {
        let photo = Evidence {
            file_path: "front.jpg".to_string(),
            side: Some(ExternalSide::Right),
            ..Default::default()
        };
        let json = serde_json::to_string(&photo).unwrap();
        let back: Evidence = serde_json::from_str(&json).unwrap();
        assert_eq!(back.side, Some(ExternalSide::Right));
        assert_eq!(back.file_path, "front.jpg");
    }

    #[test]
    fn photo_content_type_infers_and_overrides() {
        // No explicit type, .jpg extension -> inferred JPEG, treated as image.
        let jpg = Evidence {
            file_path: "x.jpg".to_string(),
            ..Default::default()
        };
        assert_eq!(jpg.content_type(), "image/jpeg");
        assert!(jpg.is_image());

        // .png extension -> inferred PNG.
        let png = Evidence {
            file_path: "x.png".to_string(),
            ..Default::default()
        };
        assert_eq!(png.content_type(), "image/png");
        assert!(png.is_image());

        // Explicit content_type wins over the extension and marks a document.
        let pdf = Evidence {
            file_path: "dop.jpg".to_string(),
            content_type: Some("application/pdf".to_string()),
            ..Default::default()
        };
        assert_eq!(pdf.content_type(), "application/pdf");
        assert!(!pdf.is_image());
    }

    /// A record written BEFORE unknown years existed stored plain numbers —
    /// they must load as known years (`Some`), not as unknown.
    #[test]
    fn legacy_plain_year_loads_as_known() {
        let legacy = r#"{"years":{"Original":1985,"Extension1":2008}}"#;
        let phases: ConstructionPhases = serde_json::from_str(legacy).unwrap();
        assert_eq!(phases.year_of(&ConstructionPhase::Original), Some(1985));
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), Some(2008));
        assert!(!phases.year_is_unknown(&ConstructionPhase::Extension1));
        assert!(phases.is_defined(&ConstructionPhase::Extension1));
    }

    /// An unknown-year phase survives a serialize → deserialize round-trip
    /// (it's durable — the assessor fills the real year in later).
    #[test]
    fn unknown_year_round_trips() {
        let mut phases = ConstructionPhases::default();
        phases.set_year(ConstructionPhase::Original, 1985);
        phases.set_year_unknown(ConstructionPhase::Extension1);
        let json = serde_json::to_string(&phases).unwrap();
        let back: ConstructionPhases = serde_json::from_str(&json).unwrap();
        assert!(back.is_defined(&ConstructionPhase::Extension1));
        assert!(back.year_is_unknown(&ConstructionPhase::Extension1));
        assert_eq!(back.year_of(&ConstructionPhase::Extension1), None);
        assert_eq!(back.year_of(&ConstructionPhase::Original), Some(1985));
    }

    /// An unknown-year phase counts as ANSWERED: it's defined (so it appears
    /// in the assignable list), labels as "unk", and a later `set_year`
    /// replaces the unknown with the real year.
    #[test]
    fn unknown_year_is_defined_labels_unk_and_fills_in_later() {
        let mut phases = ConstructionPhases::default();
        phases.set_year_unknown(ConstructionPhase::Extension1);
        assert_eq!(
            phases.defined_phases(),
            vec![ConstructionPhase::Extension1],
            "unknown-year phase is still assignable"
        );
        assert_eq!(
            phases.display_label(&ConstructionPhase::Extension1),
            "Extension 1 (unk)"
        );

        // The assessor learns the year later and fills it in.
        phases.set_year(ConstructionPhase::Extension1, 2008);
        assert!(!phases.year_is_unknown(&ConstructionPhase::Extension1));
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), Some(2008));
        assert_eq!(
            phases.display_label(&ConstructionPhase::Extension1),
            "Extension 1 (2008)"
        );
    }

    #[test]
    fn photo_side_defaults_to_none_for_old_records() {
        // A record serialized before `side` existed has no such key.
        let legacy = r#"{"id":"00000000-0000-0000-0000-000000000000","file_path":"x.jpg","description":null}"#;
        let photo: Evidence = serde_json::from_str(legacy).unwrap();
        assert_eq!(photo.side, None);
    }
}
