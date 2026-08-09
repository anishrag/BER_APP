//! Smoke test of the edit-dimension resize against a REAL field record.
//!
//! Point `BER_RESIZE_RECORD` at a `data.json` (a serialized `Ber`, e.g. a
//! prod backup) and this loads it through the normal open path, widens the
//! storey's southernmost full-width wall by 0.4 m moving its EAST end, and
//! checks the invariants the feature promises: the footprint grows by
//! exactly the delta, no wall loses its construction attribution, every
//! window/door keeps a valid wall placement, and undo restores the original
//! footprint. Skips (passes) when the env var isn't set, so CI without a
//! field record is unaffected.

use ber_app::controller::app_controller::AppController;
use ber_app::engine::ber_repository::{BerRecord, BerRepository};
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::fenestration::OpeningPlacement;

#[test]
fn real_record_survives_an_east_widen_and_undo() {
    let Ok(path) = std::env::var("BER_RESIZE_RECORD") else {
        eprintln!("BER_RESIZE_RECORD not set — skipping the real-record resize smoke test");
        return;
    };
    let json = std::fs::read_to_string(&path).expect("read the record json");
    let ber: Ber = serde_json::from_str(&json).expect("parse the record");
    let id = ber.id;

    let conn = ber_app::engine::open_db(":memory:").unwrap();
    {
        let repo = BerRepository::new(&conn);
        repo.init_db().unwrap();
        repo.upsert(&BerRecord {
            id,
            name: ber.address.clone(),
            date_assessed: chrono::NaiveDate::from_ymd_opt(2026, 8, 8).unwrap(),
            data: ber.clone(),
        })
        .unwrap();
    }
    let mut c = AppController::new(conn);
    c.open_ber(&id).unwrap();
    c.materialize();

    let before = c.current_ber_clone().unwrap();
    let storey0 = &before.storeys[0];
    let ring = storey0.floors.footprint_ring().expect("footprint");
    let min_y = ring.iter().map(|p| p.y).fold(f64::MAX, f64::min);
    let (min_x, max_x) = ring.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
        (lo.min(p.x), hi.max(p.x))
    });
    let width = max_x - min_x;
    let placed = |b: &Ber| -> usize {
        b.storeys[0]
            .windows
            .iter()
            .map(|w| &w.placement)
            .chain(b.storeys[0].doors.iter().map(|d| &d.placement))
            .filter(|p| matches!(p, Some(OpeningPlacement::Wall { .. })))
            .count()
    };
    let placed_before = placed(&before);
    let walls_before = storey0.walls.regions.len();
    let ceilings_before = storey0.ceilings.regions.len();
    let attributed_before = storey0
        .walls
        .regions
        .iter()
        .filter(|r| r.construction_id != Default::default())
        .count();

    // Tap the SOUTH wall (full-width, along the E-W axis) at its midpoint.
    let tap_x = (min_x + max_x) * 0.5;
    c.set_edit_dimension_mode(true);
    assert!(
        c.edit_dimension_tap(tap_x, min_y, 0.15),
        "tap picked the south wall at ({tap_x:.2}, {min_y:.2})"
    );
    let prompt = c.edit_dimension_for_ui().expect("prompt open");
    assert!(
        (prompt.current_length as f64 - width).abs() < 0.02,
        "picked the full-width wall: prompt {:.3} vs width {width:.3}",
        prompt.current_length
    );

    // Widen by 0.4 m. The default moving end may be either side; the checks
    // below are side-agnostic (width delta + attribution survival).
    let target = prompt.current_length + 0.4;
    c.apply_edit_dimension(target).unwrap();

    let after = c.current_ber_clone().unwrap();
    let ring2 = after.storeys[0].floors.footprint_ring().expect("footprint");
    let (min_x2, max_x2) = ring2.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
        (lo.min(p.x), hi.max(p.x))
    });
    assert!(
        ((max_x2 - min_x2) - (width + 0.4)).abs() < 0.02,
        "storey width grew by 0.4 (was {width:.3}, now {:.3})",
        max_x2 - min_x2
    );
    assert_eq!(
        after.storeys[0].walls.regions.len(),
        walls_before,
        "no wall appeared/disappeared"
    );
    // The field bug: the ceiling refit minted an extra flat fill region for
    // the grown strip instead of stretching the existing one.
    assert_eq!(
        after.storeys[0].ceilings.regions.len(),
        ceilings_before,
        "the existing ceiling region(s) expanded — no extra fill region"
    );
    let ceil_max_x = after.storeys[0]
        .ceilings
        .regions
        .iter()
        .flat_map(|r| r.polygon.exterior.iter())
        .map(|p| p.x)
        .fold(f64::MIN, f64::max);
    assert!(
        (ceil_max_x - max_x2).abs() < 0.02,
        "the ceiling covers the widened footprint (ceiling {ceil_max_x:.3} vs floor {max_x2:.3})"
    );
    let attributed_after = after.storeys[0]
        .walls
        .regions
        .iter()
        .filter(|r| r.construction_id != Default::default())
        .count();
    assert_eq!(
        attributed_after, attributed_before,
        "every wall kept its construction attribution"
    );
    assert_eq!(
        placed(&after),
        placed_before,
        "every opening kept a wall placement"
    );

    // Undo restores the original width.
    c.undo().unwrap();
    let restored = c.current_ber_clone().unwrap();
    let ring3 = restored.storeys[0]
        .floors
        .footprint_ring()
        .expect("footprint");
    let (min_x3, max_x3) = ring3.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
        (lo.min(p.x), hi.max(p.x))
    });
    assert!(
        ((max_x3 - min_x3) - width).abs() < 0.02,
        "undo restored the width"
    );
}
