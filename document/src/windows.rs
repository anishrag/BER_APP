//! Dwelling-level fenestration completeness — the Mapping "Windows" checklist
//! gate. Reads the RESOLVED `Ber` (Layer 1), like the exposure rollups.

use ber_model::ber::ber_struct::Ber;
use ber_model::ber::fenestration::OpeningPlacement;

/// `true` when the dwelling's windows pass the Mapping "Windows" gate:
///
/// 1. **At least four windows house-wide** — standalone windows (wall + roof)
///    summed across every storey. Door-sidelight/transom panes (assembly
///    members) don't count: a door with three sidelights isn't "four windows".
///    There's no per-storey minimum — a single-window storey is fine as long as
///    the whole house clears four.
/// 2. **Every multi-storey window stays on the building** — its absolute top
///    sits no higher than its wall's same-type vertically-aligned run allows
///    ([`Ber::wall_run_top_z`]), sampled across the window's FULL span (a
///    window near a sloped roofline must fit at its high edge, not just its
///    centre). A multi-storey window that spans up past where its wall run ends
///    is "hanging off the building" and fails the gate.
///
/// A multi-storey window whose host wall has momentarily gone missing (mid
/// re-anchor) is skipped rather than failed — the check is about geometry that
/// actually resolved, not transient derivation gaps.
pub fn windows_complete(ber: &Ber) -> bool {
    let total: usize = ber
        .storeys
        .iter()
        .map(|s| s.windows.iter().filter(|w| w.assembly_id.is_none()).count())
        .sum();
    if total < 4 {
        return false;
    }
    let datums = ber.storey_floor_datums();
    for (idx, storey) in ber.storeys.iter().enumerate() {
        for w in &storey.windows {
            if !w.multi_storey {
                continue;
            }
            let Some(OpeningPlacement::Wall {
                wall_id,
                along_m,
                sill_m,
            }) = &w.placement
            else {
                continue; // a multi-storey roof window is nonsensical; ignore
            };
            let Some(wall) = storey.walls.regions.iter().find(|r| r.id == *wall_id) else {
                continue; // host wall transiently absent — don't fail on it
            };
            let base_z = wall.base_frame().map(|(_, _, _, bz)| bz).unwrap_or(0.0);
            let window_top = datums.get(idx).copied().unwrap_or(0.0)
                + base_z
                + *sill_m as f64
                + w.height_m as f64;
            // The run top can taper across the window — sample at both edges and
            // require the window to clear the LOWER of the two (its high corner).
            let (half_w, along) = (w.width_m as f64 * 0.5, *along_m as f64);
            let run_top = ber
                .wall_run_top_z(idx, wall, along - half_w)
                .min(ber.wall_run_top_z(idx, wall, along + half_w));
            if window_top > run_top + 1e-3 {
                return false; // hangs off the top of the building
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ber_model::ber::common::ItemId;
    use ber_model::ber::fenestration::{OpeningPlacement, Window};
    use ber_model::ber::structural::{
        Opes, Polygon3D, Storey, WallExposure, WallRegion, WallRegions,
    };
    use domain::points::Point3f64;

    /// A 4 m flat wall, top at 2.4 m, base at z = 0.
    fn flat_wall(id: ItemId) -> WallRegion {
        WallRegion {
            id,
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 2.4),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            exposure: WallExposure::Unset,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        }
    }

    fn win(multi_storey: bool, wall_id: ItemId, sill_m: f32, height_m: f32) -> Window {
        Window {
            id: ItemId::default(),
            vertices: vec![],
            width_m: 1.0,
            height_m,
            window_type_id: ItemId::default(),
            placement: Some(OpeningPlacement::Wall {
                wall_id,
                along_m: 2.0,
                sill_m,
            }),
            wall_anchor: None,
            assembly_id: None,
            multi_storey,
            shape: ber_model::ber::fenestration::WindowShape::Rectangular,
            overshading: Default::default(),
            num_opes: None,
        }
    }

    fn storey_with(windows: Vec<Window>, wall_id: ItemId) -> Storey {
        Storey {
            walls: WallRegions {
                regions: vec![flat_wall(wall_id)],
            },
            windows,
            ..Storey::default()
        }
    }

    fn ber_with(storey: Storey) -> Ber {
        Ber {
            storeys: vec![storey],
            ..Ber::default()
        }
    }

    #[test]
    fn under_four_windows_fails() {
        let wid = ItemId::default();
        let ber = ber_with(storey_with(
            vec![win(false, wid, 1.0, 1.0), win(false, wid, 1.0, 1.0)],
            wid,
        ));
        assert!(!windows_complete(&ber));
    }

    #[test]
    fn four_plain_windows_pass() {
        let wid = ItemId::default();
        let ber = ber_with(storey_with(
            (0..4).map(|_| win(false, wid, 1.0, 1.0)).collect(),
            wid,
        ));
        assert!(windows_complete(&ber));
    }

    #[test]
    fn sidelight_panes_do_not_count_toward_the_four() {
        // 1 real window + 3 sidelight panes (assembly members) = only 1 counted.
        let wid = ItemId::default();
        let mut ws = vec![win(false, wid, 1.0, 1.0)];
        for _ in 0..3 {
            let mut s = win(false, wid, 0.3, 1.0);
            s.assembly_id = Some(ItemId::default());
            ws.push(s);
        }
        assert!(!windows_complete(&ber_with(storey_with(ws, wid))));
    }

    #[test]
    fn multi_storey_within_run_passes() {
        // Single storey → the run tops out at the wall (2.4). A multi-storey
        // window topping at 1.0 + 1.0 = 2.0 stays on the building.
        let wid = ItemId::default();
        let mut ws: Vec<Window> = (0..3).map(|_| win(false, wid, 1.0, 1.0)).collect();
        ws.push(win(true, wid, 1.0, 1.0));
        assert!(windows_complete(&ber_with(storey_with(ws, wid))));
    }

    #[test]
    fn multi_storey_hanging_off_fails() {
        // Top at 2.0 + 1.0 = 3.0 > the 2.4 wall top, and no storey above to
        // extend the run → hangs off the building.
        let wid = ItemId::default();
        let mut ws: Vec<Window> = (0..3).map(|_| win(false, wid, 1.0, 1.0)).collect();
        ws.push(win(true, wid, 2.0, 1.0));
        assert!(!windows_complete(&ber_with(storey_with(ws, wid))));
    }
}
