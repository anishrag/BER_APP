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

/// Interior finish of a wall — the same options apply to internal, separating and
/// external walls in the Phase-4 form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WallFinish {
    #[default]
    Unset,
    PlasterboardOnTimber,
    HardPlaster,
    PlasterboardOnDotAndDab,
}

impl WallFinish {
    pub const OPTIONS: [WallFinish; 3] = [
        WallFinish::PlasterboardOnTimber,
        WallFinish::HardPlaster,
        WallFinish::PlasterboardOnDotAndDab,
    ];
    pub fn label(self) -> &'static str {
        match self {
            WallFinish::Unset => "—",
            WallFinish::PlasterboardOnTimber => "Plasterboard on timber",
            WallFinish::HardPlaster => "Hard plaster",
            WallFinish::PlasterboardOnDotAndDab => "Plasterboard on dot & dab",
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

/// The Phase-4 dropdown form values (house-level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FinalDetails {
    pub floor: FloorKind,
    pub internal_wall: WallFinish,
    pub separating_wall: WallFinish,
    pub external_wall: WallFinish,
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
    /// Every side has been classified (none left `Unset`).
    pub fn all_set(&self) -> bool {
        super::common::ExternalSide::ALL
            .iter()
            .all(|&s| self.get(s) != ShelterCategory::Unset)
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
