//! Debug dump of a resolved storey's roof/wall derivation — pure formatting,
//! no I/O. The shell prints/copies it (`AppController::debug_dump_current`);
//! the assessor pastes it for diagnosis. Stable, greppable layout: every line
//! starts with a fixed keyword, rings render as `(x,y[,z])` tuples to 4 dp.

use datatypes::ber::common::ItemId;
use datatypes::ber::structural::Storey;
use std::fmt::Write as _;

/// Short form of an `ItemId` for the dump: the first 8 hex chars of the UUID.
fn short(id: &ItemId) -> String {
    id.0.simple().to_string()[..8].to_string()
}

/// Multi-line report of one storey's resolved geometry: ceiling height, roof
/// apexes, floors, roofs (with flat/fan classification), walls (with their
/// full top-profile vertex lists), and the apex-group summary. `label` tags
/// the header so multi-storey dumps stay distinguishable.
pub fn debug_dump_storey(storey: &Storey, label: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "=== STOREY DUMP[{label}] ===");
    let _ = writeln!(s, "ceiling_height_m: {:?}", storey.ceiling_height_m);

    let _ = writeln!(s, "roof_apexes ({}):", storey.roof_apexes.len());
    for (i, a) in storey.roof_apexes.iter().enumerate() {
        let _ = writeln!(
            s,
            "  apex[{i}] ({:.4},{:.4}) z={:.4}",
            a.point.x, a.point.y, a.z_m
        );
    }

    let _ = writeln!(s, "floors ({}):", storey.floors.regions.len());
    for (i, r) in storey.floors.regions.iter().enumerate() {
        let ring: Vec<String> = r
            .polygon
            .exterior()
            .coords()
            .map(|c| format!("({:.4},{:.4})", c.x, c.y))
            .collect();
        let _ = writeln!(
            s,
            "  floor[{i}] cid={} level_m={:.4} ring({}): {}",
            short(&r.construction_id),
            r.level_m,
            ring.len(),
            ring.join(" ")
        );
    }

    let _ = writeln!(s, "roofs ({}):", storey.roofs.regions.len());
    for (i, r) in storey.roofs.regions.iter().enumerate() {
        let class = match (r.flat_z(), r.fan_base_z(&storey.roof_apexes)) {
            (Some(z), _) => format!("flat_z={z:.4}"),
            (None, Some(b)) => format!("sloped fan_base_z={b:.4}"),
            (None, None) => "sloped fan_base_z=None".to_string(),
        };
        let ring: Vec<String> = r
            .polygon
            .exterior
            .iter()
            .map(|v| format!("({:.4},{:.4},{:.4})", v.x, v.y, v.z))
            .collect();
        let _ = writeln!(
            s,
            "  roof[{i}] cid={} {class} ring({}): {}",
            short(&r.construction_id),
            ring.len(),
            ring.join(" ")
        );
    }

    let _ = writeln!(s, "walls ({}):", storey.walls.regions.len());
    for (i, r) in storey.walls.regions.iter().enumerate() {
        let e = &r.polygon.exterior;
        let base = if e.len() >= 2 {
            format!(
                "({:.4},{:.4})->({:.4},{:.4})",
                e[0].x, e[0].y, e[1].x, e[1].y
            )
        } else {
            "degenerate".to_string()
        };
        let ring: Vec<String> = e
            .iter()
            .map(|v| format!("({:.4},{:.4},{:.4})", v.x, v.y, v.z))
            .collect();
        let _ = writeln!(
            s,
            "  wall[{i}] id={} exposure={:?} opes={} base={base} ring({}): {}",
            short(&r.id),
            r.exposure,
            r.opes.doors.len() + r.opes.windows.len(),
            ring.len(),
            ring.join(" ")
        );
    }

    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
    let _ = writeln!(s, "apex_groups ({}):", groups.len());
    for (i, g) in groups.iter().enumerate() {
        let rings: Vec<String> = g
            .merged_rings
            .iter()
            .map(|ring| {
                let pts: Vec<String> = ring
                    .iter()
                    .map(|p| format!("({:.4},{:.4})", p.x, p.y))
                    .collect();
                format!("ring({}): {}", pts.len(), pts.join(" "))
            })
            .collect();
        let _ = writeln!(
            s,
            "  group[{i}] apexes={:?} regions={:?} base_z={:.4} merged {}",
            g.apex_idxs,
            g.region_idxs,
            g.base_z,
            rings.join(" | ")
        );
    }
    let _ = writeln!(s, "=== END STOREY DUMP[{label}] ===");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use datatypes::ber::structural::{CeilingRegions, FloorRegions, RoofApex};
    use domain::points::Point2f64;

    /// The dump is stable + greppable: header/footer with the label, one
    /// keyword-prefixed line per region, classification + counts present.
    #[test]
    fn dump_covers_every_section_with_the_label() {
        let square = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        let mut storey = Storey {
            floors: FloorRegions::from_vertex_lists(vec![square.clone()], Default::default()),
            ceiling_height_m: Some(2.4),
            roof_apexes: vec![RoofApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.0,
            }],
            ..Storey::default()
        };
        storey.roofs = CeilingRegions::default_for_footprint(vec![square], 2.4, Default::default())
            .with_apexes(&storey.roof_apexes);

        let dump = debug_dump_storey(&storey, "GF");
        assert!(dump.starts_with("=== STOREY DUMP[GF] ===\n"));
        assert!(dump.ends_with("=== END STOREY DUMP[GF] ===\n"));
        assert!(dump.contains("ceiling_height_m: Some(2.4)"));
        assert!(dump.contains("apex[0] (2.0000,2.0000) z=4.0000"));
        assert!(dump.contains("floor[0]"));
        assert!(dump.contains("roofs (4):"), "{dump}");
        assert!(dump.contains("sloped fan_base_z=2.4000"));
        assert!(dump.contains("walls (0):"));
        assert!(dump.contains("group[0]"));
    }
}
