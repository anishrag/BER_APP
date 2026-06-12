//! Runtime tuning knobs, loaded from `tuning.yaml`.
//!
//! These are the thresholds for the new-storey trace, the sub-area carve, and the
//! collinearity review — the numbers we tune while iterating on the feel of the
//! tool. They live in a YAML file so they can be adjusted without recompiling.
//!
//! Every field has a `Default` matching the value used before extraction, and
//! `#[serde(default)]` so a partial `tuning.yaml` only overrides the keys it
//! lists. A missing or unparseable file falls back to all-defaults (logged).
//!
//! Resolution order for the file path: `$BER_TUNING`, else `./tuning.yaml`.

use serde::Deserialize;

/// Thresholds for the new-storey "draw on top of the previous storey" flow.
/// Ratios are fractions of the previous storey's bounding-box diagonal unless
/// noted; metre values are absolute.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NewStoreyTuning {
    /// Snap radius for pulling a traced point onto the previous outline
    /// (`ring_diag * this`).
    pub snap_tol_ratio: f32,
    /// View zoom when a new upper storey is added (leaves room to draw past the
    /// previous footprint). `1.0` = pure auto-fit.
    pub add_storey_zoom: f32,
}

impl Default for NewStoreyTuning {
    fn default() -> Self {
        Self {
            snap_tol_ratio: 0.08,
            add_storey_zoom: 0.7,
        }
    }
}

/// Collinearity-review separation thresholds (fractions of the plan diagonal).
/// A near-collinear pair is offered for review when the perpendicular gap
/// between the two edges is between `min` and `max`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CollinearityTuning {
    pub min_separation_ratio: f32,
    pub max_separation_ratio: f32,
}

impl Default for CollinearityTuning {
    fn default() -> Self {
        Self {
            min_separation_ratio: 0.0015,
            max_separation_ratio: 0.09,
        }
    }
}

/// The full tuning tree (root of `tuning.yaml`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Tuning {
    pub new_storey: NewStoreyTuning,
    pub collinearity: CollinearityTuning,
    /// Sub-area carve knobs (living areas / construction-phase areas), in
    /// `manip_2d_plan`.
    pub subarea: manip_2d_plan::SubareaTuning,
}

/// Shipped defaults, embedded at compile time. The repo-root file IS the
/// shipped file (the test below guards it against drifting from the struct
/// defaults), so on platforms with no cwd file — Android — the app still
/// gets the tuned values.
const EMBEDDED_TUNING: &str = include_str!("../tuning.yaml");

impl Tuning {
    /// Load from `$BER_TUNING`, else `./tuning.yaml`, else the embedded copy.
    /// An invalid file → all defaults, with a logged note.
    pub fn load() -> Self {
        let path = std::env::var("BER_TUNING").unwrap_or_else(|_| "tuning.yaml".to_string());
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_yaml::from_str::<Tuning>(&text) {
                Ok(t) => {
                    log::info!("[tuning] loaded {path}");
                    t
                }
                Err(e) => {
                    log::warn!("[tuning] {path}: parse error ({e}); using defaults");
                    Self::default()
                }
            },
            Err(_) => match serde_yaml::from_str::<Tuning>(EMBEDDED_TUNING) {
                Ok(t) => t,
                Err(e) => {
                    // Unreachable in practice: the test below keeps the
                    // embedded file parseable.
                    log::error!("[tuning] embedded defaults: parse error ({e}); using defaults");
                    Self::default()
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped `tuning.yaml` must parse AND list the same values as the
    /// built-in defaults — otherwise the file is silently ignored (a typo'd key
    /// or bad value falls back to defaults), which would be invisible. This
    /// guards against the file and the structs drifting apart.
    #[test]
    fn tuning_yaml_parses_and_matches_defaults() {
        let text =
            std::fs::read_to_string("tuning.yaml").expect("tuning.yaml present at repo root");
        let t: Tuning = serde_yaml::from_str(&text).expect("tuning.yaml parses");
        let d = Tuning::default();
        let ns = &t.new_storey;
        let dn = &d.new_storey;
        assert_eq!(ns.snap_tol_ratio, dn.snap_tol_ratio);
        assert_eq!(ns.add_storey_zoom, dn.add_storey_zoom);
        assert_eq!(
            t.collinearity.min_separation_ratio,
            d.collinearity.min_separation_ratio
        );
        assert_eq!(
            t.collinearity.max_separation_ratio,
            d.collinearity.max_separation_ratio
        );
        assert_eq!(t.subarea.close_gap_m, d.subarea.close_gap_m);
        assert_eq!(t.subarea.on_ratio, d.subarea.on_ratio);
        assert_eq!(t.subarea.off_ratio, d.subarea.off_ratio);
        assert_eq!(t.subarea.on_tol_ceil_m, d.subarea.on_tol_ceil_m);
        assert_eq!(t.subarea.off_tol_ceil_m, d.subarea.off_tol_ceil_m);
        assert_eq!(t.subarea.rdp_tol_ratio, d.subarea.rdp_tol_ratio);
        assert_eq!(t.subarea.rdp_tol_floor, d.subarea.rdp_tol_floor);
        assert_eq!(t.subarea.simplify_ratio, d.subarea.simplify_ratio);
        assert_eq!(t.subarea.simplify_ceil_m, d.subarea.simplify_ceil_m);
        assert_eq!(
            t.subarea.cut_vertex_tol_ratio,
            d.subarea.cut_vertex_tol_ratio
        );
        assert_eq!(t.subarea.cut_reach_tol_ratio, d.subarea.cut_reach_tol_ratio);
        assert_eq!(t.subarea.corner_snap_m, d.subarea.corner_snap_m);
    }
}
