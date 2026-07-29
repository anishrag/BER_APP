//! Stable region identity across a re-resolve.
//!
//! The resolve projection mints every wall/floor/ceiling region FRESH
//! (`ItemId::default()` — a random uuid) on each pass, and the InterStorey
//! derivation re-splits and re-mints again after it. So a region id was only
//! ever valid until the next materialize, and anything holding one (the
//! Structure selection, an open "Edit attributes" session, a riser selection,
//! an open elevation) had to capture a durable key beforehand and re-point
//! afterwards — per caller, and it kept being forgotten.
//!
//! This restores identity ONCE, at the end of the resolve pipeline: snapshot
//! every region's durable geometric key → id BEFORE the resolve, and after it
//! give each re-derived region back the id of the region that occupied the same
//! surface. A surface that is genuinely new (a fresh split, a carve) keeps its
//! freshly minted id.
//!
//! The key is the same one the callers used to re-point by: a wall's quantized
//! base edge plus its quantized base z (the discriminator for vertically
//! stacked pieces of one wall), a floor/ceiling area's quantized plan centroid.
//! Walls and porch walls share one pool — a wall can move between the two
//! collections as the porch classification changes, and it is the same surface.

use std::collections::{HashMap, HashSet};

use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;
use ber_model::ber::structural::{Storey, WallRegion};
use domain::points::Point2f64;

use crate::exposure::riser_key;
use crate::phase::centroid_key;

/// The durable, geometry-derived identity of a resolved region — stable across
/// a re-resolve that leaves the surface where it was.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RegionKey {
    /// Quantized base edge + quantized base z + quantized TOP z (mm).
    ///
    /// The top z is load-bearing, not decoration. The cross-storey derivation
    /// stamps a buried junction BAND over the bottom of a wall, and that band
    /// shares the wall's base edge AND its base z — the two regions differ only
    /// in how far up they reach (56 Elmwood Road, storey 1: an `InterStorey`
    /// band reaching 0.20 m and the `Exposed` wall reaching 2.41 m, both based
    /// at -0.25 m on the same edge). Without the top, the pair is one key: the
    /// identity restore sees a duplicate and gives BOTH fresh ids, so a
    /// selection holding the exposed wall lost it on the next materialize and
    /// the wall dropped out of its "Edit attributes" group.
    Wall((i64, i64, i64, i64), i64, i64),
    /// Quantized plan centroid.
    Area((i64, i64)),
}

/// Which pool a [`RegionKey`] lives in. Walls and porch walls share `Walls`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pool {
    Walls,
    Floors,
    Ceilings,
    Porches,
    LivingAreas,
}

/// Every region's durable key → the id it currently holds, per storey. Opaque:
/// produced by [`region_identity_snapshot`], consumed by
/// [`restore_region_identity`].
#[derive(Default)]
pub struct IdentitySnapshot(HashMap<(usize, Pool, RegionKey), ItemId>);

fn wall_key(r: &WallRegion) -> Option<RegionKey> {
    let (e0, e1) = r.base_edge()?;
    let top = r
        .polygon
        .exterior
        .iter()
        .map(|p| p.z)
        .fold(f64::NEG_INFINITY, f64::max);
    if !top.is_finite() {
        return None;
    }
    Some(RegionKey::Wall(
        riser_key(Point2f64::new(e0.x, e0.y), Point2f64::new(e1.x, e1.y)),
        (e0.z * 1000.0).round() as i64,
        (top * 1000.0).round() as i64,
    ))
}

/// Walk a storey's identity-bearing regions: `(pool, key, id)`.
fn regions_of(s: &Storey) -> Vec<(Pool, RegionKey, ItemId)> {
    let mut out = Vec::new();
    for r in s.walls.regions.iter().chain(s.porch_walls.regions.iter()) {
        if let Some(k) = wall_key(r) {
            out.push((Pool::Walls, k, r.id));
        }
    }
    for (pool, regions) in [
        (Pool::Floors, &s.floors),
        (Pool::Porches, &s.porches),
        (Pool::LivingAreas, &s.living_areas),
    ] {
        for r in regions.regions.iter() {
            out.push((pool, RegionKey::Area(centroid_key(r.centroid())), r.id));
        }
    }
    for r in s.ceilings.regions.iter() {
        out.push((
            Pool::Ceilings,
            RegionKey::Area(centroid_key(r.centroid())),
            r.id,
        ));
    }
    out
}

/// Capture every storey's `key → id` map BEFORE a re-resolve. Cheap: keys and
/// ids only, no geometry copied.
///
/// A key that occurs twice in one storey's pool (two regions quantizing to the
/// same base edge + z, or the same centroid) is AMBIGUOUS — it cannot identify
/// either region, so it is dropped from the snapshot and both regions take
/// fresh ids on the far side. Identity is best-effort by construction; the
/// resolve stays correct without it.
pub fn region_identity_snapshot(ber: &Ber) -> IdentitySnapshot {
    let mut snap: HashMap<(usize, Pool, RegionKey), ItemId> = HashMap::new();
    let mut ambiguous: HashSet<(usize, Pool, RegionKey)> = HashSet::new();
    for (idx, s) in ber.storeys.iter().enumerate() {
        for (pool, key, id) in regions_of(s) {
            let k = (idx, pool, key);
            if snap.insert(k, id).is_some() {
                ambiguous.insert(k);
            }
        }
    }
    for k in ambiguous {
        snap.remove(&k);
    }
    IdentitySnapshot(snap)
}

/// Give each re-derived region back the id the same surface held in `snap`.
///
/// Regions whose key isn't in the snapshot (a genuinely new surface) keep the
/// id the resolve minted. An id is handed out at most once per pass — if two
/// re-derived regions somehow claim the same one, the first keeps it and the
/// second stays fresh, so ids remain unique.
pub fn restore_region_identity(ber: &mut Ber, snap: &IdentitySnapshot) {
    let mut used: HashSet<ItemId> = HashSet::new();
    for (idx, s) in ber.storeys.iter_mut().enumerate() {
        let mut take = |pool: Pool, key: RegionKey| -> Option<ItemId> {
            let old = *snap.0.get(&(idx, pool, key))?;
            used.insert(old).then_some(old)
        };
        for r in s
            .walls
            .regions
            .iter_mut()
            .chain(s.porch_walls.regions.iter_mut())
        {
            if let Some(id) = wall_key(r).and_then(|k| take(Pool::Walls, k)) {
                r.id = id;
            }
        }
        for (pool, regions) in [
            (Pool::Floors, &mut s.floors),
            (Pool::Porches, &mut s.porches),
            (Pool::LivingAreas, &mut s.living_areas),
        ] {
            for r in regions.regions.iter_mut() {
                if let Some(id) = take(pool, RegionKey::Area(centroid_key(r.centroid()))) {
                    r.id = id;
                }
            }
        }
        for r in s.ceilings.regions.iter_mut() {
            if let Some(id) = take(Pool::Ceilings, RegionKey::Area(centroid_key(r.centroid()))) {
                r.id = id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ber_model::ber::structural::{Storey, WallRegions};
    use domain::points::Point3f64;

    /// A wall and the buried junction BAND the cross-storey derivation stamps
    /// over its foot share a base edge AND a base z — they differ only in how
    /// far up they reach. The durable key must tell them apart, or the identity
    /// restore sees one ambiguous key, refuses to answer for either, and hands
    /// BOTH fresh ids.
    ///
    /// FIELD REGRESSION (56 Elmwood Road, storey 1): an `InterStorey` band
    /// reaching 0.20 m sat on the same edge and base as the `Exposed` wall
    /// reaching 2.41 m. A selection holding the exposed wall lost it on the next
    /// materialize, so the wall dropped out of its "Edit attributes" group and
    /// only the other two members of a three-wall selection were ever set.
    #[test]
    fn a_junction_band_and_its_wall_keep_separate_identities() {
        let quad = |top: f64| {
            ber_model::ber::structural::Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, top),
                Point3f64::new(0.0, 0.0, top),
            ])
        };
        let wall = |top: f64| WallRegion {
            id: ItemId::default(),
            polygon: quad(top),
            exposure: ber_model::ber::structural::WallExposure::Unset,
            opes: Default::default(),
            construction_id: ItemId::default(),
        };

        let mut ber = Ber::default();
        let band = wall(0.20); // the buried junction band
        let full = wall(2.41); // the real wall over it
        let (band_id, full_id) = (band.id, full.id);
        assert_ne!(band_id, full_id);
        ber.storeys.push(Storey {
            walls: WallRegions {
                regions: vec![band, full],
            },
            ..Default::default()
        });

        // Same base edge, same base z — only the top separates them.
        let keys: Vec<RegionKey> = ber.storeys[0]
            .walls
            .regions
            .iter()
            .filter_map(wall_key)
            .collect();
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1], "the two surfaces have DISTINCT keys");

        // Snapshot, then re-mint every id as a resolve would, then restore.
        let snap = region_identity_snapshot(&ber);
        for r in ber.storeys[0].walls.regions.iter_mut() {
            r.id = ItemId::default();
        }
        restore_region_identity(&mut ber, &snap);

        let got: Vec<ItemId> = ber.storeys[0].walls.regions.iter().map(|r| r.id).collect();
        assert_eq!(
            got,
            vec![band_id, full_id],
            "each surface got ITS OWN id back — not dropped for ambiguity, and \
             not swapped onto its twin"
        );
    }
}
