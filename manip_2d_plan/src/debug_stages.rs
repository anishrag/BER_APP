//! Splice-pipeline debug capture — a cross-cutting utility shared by the edit,
//! cut, and orthogonalize modules. Each pipeline stage records a labelled
//! path/ring here; the app shell drains them for its coloured stage overlay.
//!
//! A LEAF module (depends only on `domain`), so the lower-level modules
//! (`orthogonal`, `carve`) can record their stages without depending UP on the
//! high-level `path_edits` module — which is where this used to live, creating a
//! `carve ⇄ path_edits` cycle and an `orthogonal → path_edits` inversion.

use domain::points::Point2f32;

/// Toggle capture of the splice-pipeline phases for the app's debug overlay.
const DEBUG_DRAW: bool = true;

/// One captured phase of the splice pipeline — a labelled path/ring the app
/// draws as a coloured debug overlay. `closed` joins the last point to the first.
#[derive(Clone, Debug)]
pub struct SplicePhase {
    pub label: String,
    pub points: Vec<Point2f32>,
    pub closed: bool,
}

thread_local! {
    static SPLICE_PHASES: std::cell::RefCell<Vec<SplicePhase>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain the splice-pipeline phases recorded since the last drain. The shell
/// drains BEFORE a splice op (to clear stale phases) and again AFTER to capture
/// that op's phases for the overlay.
pub fn take_splice_debug() -> Vec<SplicePhase> {
    SPLICE_PHASES.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// Record one splice phase for the debug overlay, in CALL order (the UI numbers
/// them). Only the path/ring is captured. No-op unless `DEBUG_DRAW`. The shell
/// drains these with [`take_splice_debug`]. `pub(crate)` so the orthogonalizer
/// and cut pipeline can record their own internal sub-stages.
pub(crate) fn record_phase(label: &str, shape: &[Point2f32], closed: bool) {
    if !DEBUG_DRAW || shape.is_empty() {
        return;
    }
    // Also dump the coordinates to the console so a stage can be inspected /
    // shared without the UI. Numbered to match the overlay's legend order is the
    // UI's job; here we print label + every point.
    log::info!(
        "[splice stage] {label} ({} pts, closed={closed}): {shape:?}",
        shape.len()
    );
    SPLICE_PHASES.with(|p| {
        p.borrow_mut().push(SplicePhase {
            label: label.to_string(),
            points: shape.to_vec(),
            closed,
        });
    });
}
