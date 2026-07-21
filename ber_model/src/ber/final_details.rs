//! Phase-4 ("Final") data: a small set of house-level details the assessor sets
//! at the end — a dropdown form (floor / wall-finish / heating), per-side
//! sheltering, and (read-only here) a north-up satellite image used to align the
//! plan to true north. Pure data; the values feed the DEAP model later.

use serde::{Deserialize, Serialize};

/// Ground-floor construction, at the granularity the Phase-4 form asks for
/// (sealed vs unsealed suspended floors matter for infiltration).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FloorKind {
    #[default]
    Unset,
    Solid,
    SuspendedSealed,
    SuspendedUnsealed,
}

impl FloorKind {
    pub const OPTIONS: [FloorKind; 3] = [
        FloorKind::Solid,
        FloorKind::SuspendedSealed,
        FloorKind::SuspendedUnsealed,
    ];
    pub fn label(self) -> &'static str {
        match self {
            FloorKind::Unset => "—",
            FloorKind::Solid => "Solid",
            FloorKind::SuspendedSealed => "Suspended (sealed)",
            FloorKind::SuspendedUnsealed => "Suspended (unsealed)",
        }
    }
}

/// Interior finish of a wall in the Phase-4 form. Internal walls offer the
/// base finishes ([`WallFinish::OPTIONS`]); separating and external walls —
/// the heat-loss-relevant faces — additionally offer an insulated
/// plasterboard (thermal-laminate dry-lining) finish
/// ([`WallFinish::OPTIONS_WITH_INSULATED`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WallFinish {
    #[default]
    Unset,
    PlasterboardOnTimber,
    HardPlaster,
    PlasterboardOnDotAndDab,
    /// Insulated plasterboard (thermal-laminate dry-lining) — additive
    /// 2026-07; offered for separating and external walls only. Serde is
    /// name-based, so older records (which never stored it) are unaffected.
    InsulatedPlasterboard,
}

impl WallFinish {
    /// The finishes every wall field offers (internal walls stop here).
    pub const OPTIONS: [WallFinish; 3] = [
        WallFinish::PlasterboardOnTimber,
        WallFinish::HardPlaster,
        WallFinish::PlasterboardOnDotAndDab,
    ];
    /// The finishes offered for SEPARATING and EXTERNAL walls: the base
    /// [`Self::OPTIONS`] plus insulated plasterboard (a dry-lining that only
    /// makes sense on a heat-loss face, so internal walls don't offer it).
    pub const OPTIONS_WITH_INSULATED: [WallFinish; 4] = [
        WallFinish::PlasterboardOnTimber,
        WallFinish::HardPlaster,
        WallFinish::PlasterboardOnDotAndDab,
        WallFinish::InsulatedPlasterboard,
    ];
    pub fn label(self) -> &'static str {
        match self {
            WallFinish::Unset => "—",
            WallFinish::PlasterboardOnTimber => "Plasterboard on timber",
            WallFinish::HardPlaster => "Hard plaster",
            WallFinish::PlasterboardOnDotAndDab => "Plasterboard on dot & dab",
            WallFinish::InsulatedPlasterboard => "Insulated plasterboard",
        }
    }
}

/// The dwelling's heat-emitter system, at the Phase-4 form's granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HeatingKind {
    #[default]
    Unset,
    Radiators,
    Underfloor,
    Both,
    Electric,
}

impl HeatingKind {
    pub const OPTIONS: [HeatingKind; 4] = [
        HeatingKind::Radiators,
        HeatingKind::Underfloor,
        HeatingKind::Both,
        HeatingKind::Electric,
    ];
    pub fn label(self) -> &'static str {
        match self {
            HeatingKind::Unset => "—",
            HeatingKind::Radiators => "Radiators",
            HeatingKind::Underfloor => "Underfloor",
            HeatingKind::Both => "Both",
            HeatingKind::Electric => "Electric",
        }
    }
}

/// Legacy pre-refactor shape: floor/wall finishes + heating used to live here.
/// Kept only so old records still parse; migrated into ThermalMassInfo +
/// HeatingSystem on load (see Ber::migrate_legacy), then dropped on next save.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct LegacyFinalDetails {
    #[serde(default)]
    pub floor: FloorKind,
    #[serde(default)]
    pub internal_wall: WallFinish,
    #[serde(default)]
    pub separating_wall: WallFinish,
    #[serde(default)]
    pub external_wall: WallFinish,
    #[serde(default)]
    pub heating: HeatingKind,
}

/// How sheltered one side of the dwelling is (neighbouring buildings / terrain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ShelterCategory {
    #[default]
    Unset,
    Sheltered,
    HalfSheltered,
    Unsheltered,
}

impl ShelterCategory {
    /// Cycle to the next category on a tap (Unset → Sheltered → Half →
    /// Unsheltered → Sheltered…). Tapping a side advances it.
    pub fn next(self) -> ShelterCategory {
        match self {
            ShelterCategory::Unset | ShelterCategory::Unsheltered => ShelterCategory::Sheltered,
            ShelterCategory::Sheltered => ShelterCategory::HalfSheltered,
            ShelterCategory::HalfSheltered => ShelterCategory::Unsheltered,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ShelterCategory::Unset => "—",
            ShelterCategory::Sheltered => "Sheltered",
            ShelterCategory::HalfSheltered => "Half sheltered",
            ShelterCategory::Unsheltered => "Unsheltered",
        }
    }
}

/// Per-side sheltering (the four [`super::common::ExternalSide`]s).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ShelteringBySide {
    pub front: ShelterCategory,
    pub right: ShelterCategory,
    pub left: ShelterCategory,
    pub back: ShelterCategory,
}

impl ShelteringBySide {
    pub fn get(&self, side: super::common::ExternalSide) -> ShelterCategory {
        use super::common::ExternalSide::*;
        match side {
            Front => self.front,
            Right => self.right,
            Left => self.left,
            Back => self.back,
        }
    }
    pub fn set(&mut self, side: super::common::ExternalSide, c: ShelterCategory) {
        use super::common::ExternalSide::*;
        match side {
            Front => self.front = c,
            Right => self.right = c,
            Left => self.left = c,
            Back => self.back = c,
        }
    }
    /// The sides still awaiting a sheltering classification (left
    /// [`ShelterCategory::Unset`]), in [`super::common::ExternalSide::ALL`]
    /// order — the detailed core behind [`all_set`](Self::all_set), naming
    /// WHICH sides block the checklist row.
    pub fn unset_sides(&self) -> Vec<super::common::ExternalSide> {
        super::common::ExternalSide::ALL
            .iter()
            .copied()
            .filter(|&s| self.get(s) == ShelterCategory::Unset)
            .collect()
    }

    /// Every side has been classified (none left `Unset`). Thin wrapper over
    /// [`unset_sides`](Self::unset_sides) — one source of truth, so the bool
    /// and the named sides can never disagree.
    pub fn all_set(&self) -> bool {
        self.unset_sides().is_empty()
    }
    /// DEAP's sheltered-sides count: sheltered = 1, half = 0.5, else 0.
    pub fn sheltered_count(&self) -> f32 {
        super::common::ExternalSide::ALL
            .iter()
            .map(|&s| match self.get(s) {
                ShelterCategory::Sheltered => 1.0,
                ShelterCategory::HalfSheltered => 0.5,
                _ => 0.0,
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `WallFinish` (incl. the additive `InsulatedPlasterboard`)
    /// round-trips through serde, and — additive evolution — JSON produced
    /// by the OLD enum shape (which had no `InsulatedPlasterboard` variant)
    /// still deserializes to the same values.
    #[test]
    fn wall_finish_serde_roundtrip_and_old_shape() {
        for f in [
            WallFinish::Unset,
            WallFinish::PlasterboardOnTimber,
            WallFinish::HardPlaster,
            WallFinish::PlasterboardOnDotAndDab,
            WallFinish::InsulatedPlasterboard,
        ] {
            let back: WallFinish =
                serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
            assert_eq!(back, f, "round-trip of {f:?}");
        }
        // Unit variants serialize as plain name strings.
        assert_eq!(
            serde_json::to_string(&WallFinish::InsulatedPlasterboard).unwrap(),
            r#""InsulatedPlasterboard""#
        );
        // JSON literals exactly as the OLD shape serialized them keep loading.
        for (old_json, expect) in [
            (r#""Unset""#, WallFinish::Unset),
            (
                r#""PlasterboardOnTimber""#,
                WallFinish::PlasterboardOnTimber,
            ),
            (r#""HardPlaster""#, WallFinish::HardPlaster),
            (
                r#""PlasterboardOnDotAndDab""#,
                WallFinish::PlasterboardOnDotAndDab,
            ),
        ] {
            let got: WallFinish = serde_json::from_str(old_json).unwrap();
            assert_eq!(got, expect, "old-shape {old_json} loads unchanged");
        }
    }

    /// A legacy `final_details` blob exactly as an OLD record stored it (the
    /// pre-refactor shape) still parses into the `LegacyFinalDetails` carrier —
    /// the raw material `Ber::migrate_legacy` adopts.
    #[test]
    fn legacy_final_details_old_record_loads() {
        let old = r#"{"floor":"Solid","internal_wall":"HardPlaster","separating_wall":"PlasterboardOnTimber","external_wall":"PlasterboardOnDotAndDab","heating":"Radiators"}"#;
        let d: LegacyFinalDetails = serde_json::from_str(old).unwrap();
        assert_eq!(d.floor, FloorKind::Solid);
        assert_eq!(d.internal_wall, WallFinish::HardPlaster);
        assert_eq!(d.separating_wall, WallFinish::PlasterboardOnTimber);
        assert_eq!(d.external_wall, WallFinish::PlasterboardOnDotAndDab);
        assert_eq!(d.heating, HeatingKind::Radiators);
    }

    /// The offered lists: internal walls keep the base finishes; separating /
    /// external walls get exactly the base list plus insulated plasterboard.
    /// No list offers `Unset`, and every offered finish has a real label.
    #[test]
    fn wall_finish_option_lists() {
        assert_eq!(
            WallFinish::OPTIONS_WITH_INSULATED[..WallFinish::OPTIONS.len()],
            WallFinish::OPTIONS,
            "the insulated list extends the base list in order"
        );
        assert_eq!(
            WallFinish::OPTIONS_WITH_INSULATED.last(),
            Some(&WallFinish::InsulatedPlasterboard)
        );
        assert!(
            !WallFinish::OPTIONS.contains(&WallFinish::InsulatedPlasterboard),
            "internal walls don't offer the dry-lining finish"
        );
        for o in WallFinish::OPTIONS_WITH_INSULATED {
            assert_ne!(o, WallFinish::Unset);
            assert_ne!(o.label(), "—", "{o:?} needs a real label");
        }
        assert_eq!(
            WallFinish::InsulatedPlasterboard.label(),
            "Insulated plasterboard"
        );
    }

    /// `unset_sides` is the detailed core behind `all_set`: all four sides in
    /// ALL order on a fresh record, shrinking (in order) as sides are
    /// classified, empty exactly when `all_set` flips true.
    #[test]
    fn unset_sides_is_the_core_behind_all_set() {
        use crate::ber::common::ExternalSide;

        let mut s = ShelteringBySide::default();
        assert_eq!(s.unset_sides(), ExternalSide::ALL.to_vec());
        assert!(!s.all_set());

        s.set(ExternalSide::Right, ShelterCategory::Sheltered);
        s.set(ExternalSide::Back, ShelterCategory::HalfSheltered);
        assert_eq!(
            s.unset_sides(),
            vec![ExternalSide::Front, ExternalSide::Left],
            "remaining sides, in ALL order"
        );
        assert!(!s.all_set());

        s.set(ExternalSide::Front, ShelterCategory::Unsheltered);
        s.set(ExternalSide::Left, ShelterCategory::Sheltered);
        assert!(s.unset_sides().is_empty());
        assert!(s.all_set(), "bool wrapper agrees with the empty core");
    }
}
