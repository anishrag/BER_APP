//! Wall-face partition model operations: dividing an outline wall's internal
//! elevation (a flat wall-local rectangle, x = along_m / y = height_m) into
//! regions by drawn cut lines, and attributing each region a [`WallExposure`].
//!
//! This is the 2-D counterpart to the 1-D [`wall_sections`](StoreyDoc::wall_sections)
//! along-wall ranges. A wall is EITHER 1-D sectioned OR face-partitioned —
//! committing a cut supersedes (clears) that wall's `wall_sections`. Pure model
//! mutation; the shell owns pointer input, undo, and persistence. The geometry
//! (subdivide / point-in-polygon) lives in `manip_plan` — this module only
//! stores the cuts/exposures and drives the shared region derivation. `len` and
//! `height` are supplied by the caller (wall length + ceiling height);
//! `document` does not compute wall geometry here.
use crate::{PartitionCut, StoreyDoc, WallFacePartition, WallSpanEntry, WallSpanValue};
use ber_model::ber::structural::{Storey, WallExposure};
use domain::points::Point2f32;
use manip_plan::{point_in_polygon, subdivide_face, VertexId};
use std::collections::HashMap;

/// Two wall-local points are the same to within this tolerance (metres) — a cut
/// shorter than this is rejected as degenerate.
const SAME_PT_EPS: f32 = 1e-4;

fn near(a: Point2f32, b: Point2f32) -> bool {
    (a.x - b.x).abs() < SAME_PT_EPS && (a.y - b.y).abs() < SAME_PT_EPS
}

impl StoreyDoc {
    /// Append `cut` to outline wall `vid`'s face partition (creating the entry),
    /// and CLEAR that wall's 1-D `wall_sections` — face partitioning supersedes
    /// along-wall sectioning. A zero-length cut (`a ≈ b`) is ignored.
    ///
    /// The FIRST cut carries the wall's current exposure onto the partition as
    /// its [`base_exposure`], so every region starts out with the answer the
    /// wall already had. Without that, clearing the sections lost it: dividing
    /// a wall to give its halves different CONSTRUCTIONS silently un-answered
    /// its exposure. Where the wall was painted in several ranges, the one
    /// covering the greatest LENGTH carries over.
    ///
    /// [`base_exposure`]: ber_model::ber::structural::WallFacePartition::base_exposure
    pub fn commit_partition_cut(&mut self, vid: VertexId, cut: PartitionCut) {
        if near(cut.a, cut.b) {
            return; // degenerate cut paints nothing
        }
        let dominant = self.wall_sections.get(&vid).and_then(|sections| {
            let mut by_exposure: Vec<(WallExposure, f32)> = Vec::new();
            for s in sections {
                let run = (s.end_m - s.start_m).abs();
                match by_exposure.iter_mut().find(|(e, _)| *e == s.exposure) {
                    Some((_, acc)) => *acc += run,
                    None => by_exposure.push((s.exposure, run)),
                }
            }
            by_exposure
                .into_iter()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(e, _)| e)
        });
        self.wall_sections.remove(&vid);
        let part = self.wall_face_partitions.entry(vid).or_default();
        if part.cuts.is_empty() {
            if let Some(e) = dominant {
                part.base_exposure = e;
            }
        }
        part.cuts.push(cut);
    }

    /// Attribute `exposure` to the face region of wall `vid` that CONTAINS
    /// `sample` (wall-local). Upsert: if an existing stored sample point falls in
    /// the SAME region as `sample`, its exposure is replaced; otherwise the new
    /// `(sample, exposure)` pair is pushed. The goal is one exposure per region.
    /// No-op when the wall isn't partitioned, and when `exposure` is
    /// [`WallExposure::InterStorey`] (DERIVED-only — `derive_inter_storey`
    /// stamps and clears it; never assessor-attributed).
    pub fn set_partition_region_exposure(
        &mut self,
        vid: VertexId,
        sample: Point2f32,
        exposure: WallExposure,
        len: f32,
        height: f32,
    ) {
        if exposure == WallExposure::InterStorey {
            return; // derived-only classification — never assessor-painted
        }
        let Some(part) = self.wall_face_partitions.get(&vid) else {
            return; // not partitioned — nothing to attribute
        };
        // Find the region polygon that contains `sample`; an existing stored
        // sample in that same polygon is updated in place.
        let cuts = cut_pairs(part);
        let regions = subdivide_face(len, height, &cuts);
        let target = regions.iter().find(|poly| point_in_polygon(poly, sample));
        let part = self.wall_face_partitions.get_mut(&vid).unwrap();
        if let Some(poly) = target {
            if let Some(slot) = part
                .regions
                .iter_mut()
                .find(|(s, _)| point_in_polygon(poly, *s))
            {
                *slot = (sample, exposure);
                return;
            }
        }
        part.regions.push((sample, exposure));
    }

    /// Drop wall `vid`'s face partition — the wall returns to 1-D sectioning.
    pub fn clear_wall_partition(&mut self, vid: VertexId) {
        self.wall_face_partitions.remove(&vid);
    }

    /// Whether outline wall `vid` is face-partitioned.
    pub fn is_wall_partitioned(&self, vid: VertexId) -> bool {
        self.wall_face_partitions.contains_key(&vid)
    }

    /// A resolved wall region's parametric wall plus the BOX it occupies on that
    /// wall's face, in wall-local `(along_m, height_m)` — the single durable
    /// identity of a wall surface.
    ///
    /// One plan wall resolves into as many regions as the derivation needs: a
    /// face partition the assessor cut, a phase boundary crossing it, an
    /// inter-storey band, a roof void. Every one of them maps to the same
    /// `SurfaceRef`, so a `SurfaceRef` alone names the WALL, never the SURFACE.
    /// The box is what distinguishes them, and — matched by OVERLAP rather than
    /// equality ([`wall_span_lookup`](Self::wall_span_lookup)) — it also
    /// survives a geometry edit that moves the split, which a point-like
    /// geometric key does not.
    ///
    /// `None` for a riser or porch wall: those are genuinely not on the outline,
    /// and keep their own base-edge key.
    pub fn wall_face_box(
        &self,
        region: &ber_model::ber::structural::WallRegion,
    ) -> Option<(manip_plan::SurfaceRef, (f32, f32), (f32, f32))> {
        const TOL_M: f32 = 0.05;
        let (e0, e1) = region.base_edge()?;
        let mid = Point2f32::new(((e0.x + e1.x) * 0.5) as f32, ((e0.y + e1.y) * 0.5) as f32);
        let sref = self.outline.floor_plan.surface_ref_at(mid, TOL_M)?;
        let (_, ring) = self.wall_local_face_ring(region)?;
        let (mut a0, mut a1) = (f32::MAX, f32::MIN);
        let (mut h0, mut h1) = (f32::MAX, f32::MIN);
        for p in &ring {
            a0 = a0.min(p.x);
            a1 = a1.max(p.x);
            h0 = h0.min(p.y);
            h1 = h1.max(p.y);
        }
        Some((sref, (a0, a1), (h0, h1)))
    }

    /// The stored attribution that best fits `region`: among the entries on its
    /// parametric wall, the one whose face box OVERLAPS the region's by the
    /// greatest area, ties broken by insertion order.
    ///
    /// Overlap — not equality — is what makes one key scheme serve both jobs a
    /// wall attribution has to do. A whole wall stores the full span and keeps
    /// matching after an edit moves its ends. A piece stores its sub-span and is
    /// still the best fit for the piece that replaces it when a split shifts. And
    /// when a split DISAPPEARS (a legacy record unifying its walls on open) the
    /// surviving whole-wall region overlaps the old piece entries and inherits
    /// from the largest, instead of losing its fabric.
    pub fn wall_span_lookup(
        &self,
        region: &ber_model::ber::structural::WallRegion,
    ) -> Option<&WallSpanEntry> {
        let (sref, along, height) = self.wall_face_box(region)?;
        let entries = self.wall_constructions.get(&sref)?;
        entries
            .iter()
            .map(|e| (e, span_overlap_area(along, height, e.along_m, e.height_m)))
            .filter(|(_, area)| *area > 0.0)
            .max_by(|(_, x), (_, y)| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(e, _)| e)
    }

    /// Store `value` against the face box `region` occupies, REPLACING the entry
    /// that box dominates (≥ [`SAME_SURFACE_FRAC`] of both boxes — the assessor
    /// re-attributing the same surface) and otherwise adding one.
    pub fn wall_span_insert(
        &mut self,
        region: &ber_model::ber::structural::WallRegion,
        value: WallSpanValue,
    ) {
        let Some((sref, along_m, height_m)) = self.wall_face_box(region) else {
            return;
        };
        let entries = self.wall_constructions.entry(sref).or_default();
        let area = span_area(along_m, height_m);
        let replace = entries.iter().position(|e| {
            let o = span_overlap_area(along_m, height_m, e.along_m, e.height_m);
            let other = span_area(e.along_m, e.height_m);
            area > 0.0
                && other > 0.0
                && o >= area * SAME_SURFACE_FRAC
                && o >= other * SAME_SURFACE_FRAC
        });
        let entry = WallSpanEntry {
            along_m,
            height_m,
            value,
        };
        match replace {
            Some(i) => entries[i] = entry,
            None => entries.push(entry),
        }
    }

    /// A RESOLVED wall region's ring in WALL-LOCAL `(along_m, height_m)` coords,
    /// with the partitioned perimeter wall it belongs to — `None` for a riser, a
    /// plain wall, or a piece of an un-partitioned wall.
    ///
    /// The projection [`split_partitioned_wall`](crate::resolve) attributes
    /// exposure with, factored out: match the region's base midpoint to a
    /// perimeter wall, then project its ring onto that wall's `a`→`b` axis. It
    /// uses the SAME endpoints `stamp_wall_face_partitions` keys the recipe by,
    /// so the local frame agrees with the partition's own cuts and samples — a
    /// flipped `a`/`b` would mirror every piece's `along`.
    ///
    /// Shared by [`wall_face_box`](Self::wall_face_box) (which bounds the ring
    /// into the surface's durable face box) and the Structure face view's region
    /// tap (which point-in-polygons a tapped sample against it to find the piece
    /// the assessor means).
    pub fn wall_local_face_ring(
        &self,
        region: &ber_model::ber::structural::WallRegion,
    ) -> Option<(VertexId, Vec<Point2f32>)> {
        const TOL_M: f64 = 1e-2;
        let (origin, (ux, uy), wlen, _) = region.base_frame()?;
        let mid =
            domain::points::Point2f64::new(origin.x + ux * wlen * 0.5, origin.y + uy * wlen * 0.5);
        let plan = &self.outline.floor_plan;
        let (vid, _, _) = plan.perimeter_walls().into_iter().find(|(_, a, b)| {
            domain::points::point_to_segment_distance(
                mid.x, mid.y, a.x as f64, a.y as f64, b.x as f64, b.y as f64,
            ) < TOL_M
        })?;
        let (a, b) = plan.wall_endpoints(vid)?;
        let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
        let plen = (dx * dx + dy * dy).sqrt();
        if plen < 1e-9 {
            return None;
        }
        let (dirx, diry) = (dx / plen, dy / plen);
        let ring = &region.polygon.exterior;
        if ring.is_empty() {
            return None;
        }
        Some((
            vid,
            ring.iter()
                .map(|v| {
                    let along = (v.x - a.x as f64) * dirx + (v.y - a.y as f64) * diry;
                    Point2f32::new(along as f32, v.z as f32)
                })
                .collect(),
        ))
    }

    /// The face regions of wall `vid` with their attributed exposures — the
    /// SHARED derivation used by the view-model and (later) resolve. Builds the
    /// cut-line array from the stored [`PartitionCut`]s, subdivides the
    /// `len × height` face rectangle via `manip_plan::subdivide_face`, then
    /// attributes each region the exposure of the stored sample point it contains
    /// (default [`WallExposure::Unset`]). Returns `(region_polygon, exposure)`
    /// pairs in wall-local coords. An un-partitioned wall yields a single
    /// full-face rectangle region with `Unset`.
    pub fn partition_face_regions(
        &self,
        vid: VertexId,
        len: f32,
        height: f32,
    ) -> Vec<(Vec<Point2f32>, WallExposure)> {
        let Some(part) = self.wall_face_partitions.get(&vid) else {
            return vec![(full_face_rect(len, height), WallExposure::Unset)];
        };
        let cuts = cut_pairs(part);
        subdivide_face(len, height, &cuts)
            .into_iter()
            .map(|poly| {
                let exposure = part
                    .regions
                    .iter()
                    .find(|(s, _)| point_in_polygon(&poly, *s))
                    .map(|(_, e)| *e)
                    .unwrap_or(WallExposure::Unset);
                (poly, exposure)
            })
            .collect()
    }

    /// Reconstruct the `VertexId`-keyed working `wall_face_partitions` map from a
    /// resolved storey's durable Layer-1 recipe ([`Storey::wall_face_partitions`]),
    /// matching each geometric [`WallFaceKey`](ber_model::ber::structural::WallFaceKey)
    /// back to a perimeter wall via `surface_ref_at` on this doc's outline plan.
    /// The Layer-1 inverse of the resolve-time stamp — the partition analogue of
    /// [`sections_from_wall_regions`](StoreyDoc::sections_from_wall_regions). An
    /// entry whose key no longer lands on a wall (outline changed) is dropped.
    pub fn wall_face_partitions_from_storey(
        &self,
        storey: &Storey,
    ) -> HashMap<VertexId, WallFacePartition> {
        const TOL_M: f32 = 0.05;
        let plan = &self.outline.floor_plan;
        let mut out = HashMap::new();
        for (key, part) in &storey.wall_face_partitions {
            // Edge midpoint (mm → m), then geometry → VertexId.
            let mid = Point2f32::new(
                (key.0 + key.2) as f32 / 2000.0,
                (key.1 + key.3) as f32 / 2000.0,
            );
            if let Some(vid) = plan.surface_ref_at(mid, TOL_M).and_then(|s| s.wall_start()) {
                out.insert(vid, part.clone());
            }
        }
        out
    }
}

/// The stored cuts as the `[[a, b]; ..]` array `subdivide_face` expects.
fn cut_pairs(part: &WallFacePartition) -> Vec<[Point2f32; 2]> {
    part.cuts.iter().map(|c| [c.a, c.b]).collect()
}

/// The full wall-face rectangle (CCW) for a `len × height` elevation.
fn full_face_rect(len: f32, height: f32) -> Vec<Point2f32> {
    vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(len, 0.0),
        Point2f32::new(len, height),
        Point2f32::new(0.0, height),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShapeSlot;
    use manip_plan::FloorPlan;

    /// A finalised 4×4 m square StoreyDoc (mirrors `exposure.rs::square_doc`).
    fn square_doc() -> StoreyDoc {
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ];
        let plan = FloorPlan::from_clean_ring(&pts).unwrap();
        let mut doc = StoreyDoc::default();
        doc.dimensioning.scale_set = true;
        doc.floor_plan = plan.clone();
        doc.outline = ShapeSlot {
            floor_plan: plan,
            dimensioning: doc.dimensioning.clone(),
        };
        doc
    }

    fn first_wall(doc: &StoreyDoc) -> VertexId {
        doc.outline.floor_plan.perimeter_walls()[0].0
    }

    /// The partition working map is SINGLE-HOMED on Layer 1: it is excluded from
    /// the `StoreyDoc` (snapshot) serialization (`#[serde(skip_serializing)]`), so
    /// a snapshot does not carry it — the durable home is `Storey::wall_face_partitions`.
    #[test]
    fn partition_single_homed_not_in_storeydoc_snapshot() {
        let mut doc = square_doc();
        let vid = first_wall(&doc);
        doc.commit_partition_cut(
            vid,
            PartitionCut {
                a: Point2f32::new(0.0, 1.5),
                b: Point2f32::new(4.0, 1.5),
            },
        );
        assert!(doc.is_wall_partitioned(vid));

        let json = serde_json::to_string(&doc).unwrap();
        assert!(
            !json.contains("wall_face_partitions"),
            "skip_serializing keeps the working map out of the snapshot"
        );
        let back: StoreyDoc = serde_json::from_str(&json).unwrap();
        assert!(
            back.wall_face_partitions.is_empty(),
            "not restored from the snapshot — comes from Layer 1 instead"
        );
    }

    /// MIGRATION-SAFE: `skip_serializing` (not `skip`) still DESERIALIZES the
    /// field, so a LEGACY snapshot that carries `wall_face_partitions` loads it —
    /// the partition isn't dropped on the upgrade (it migrates to Layer 1 on the
    /// next save). Emulated by injecting the (real serde-shaped) map into the doc
    /// JSON, as an old snapshot would have stored it.
    #[test]
    fn legacy_partition_snapshot_still_deserializes() {
        let mut doc = square_doc();
        let vid = first_wall(&doc);
        doc.commit_partition_cut(
            vid,
            PartitionCut {
                a: Point2f32::new(0.0, 1.5),
                b: Point2f32::new(4.0, 1.5),
            },
        );
        doc.set_partition_region_exposure(
            vid,
            Point2f32::new(2.0, 0.5),
            WallExposure::Ground,
            4.0,
            2.4,
        );
        let map = doc.wall_face_partitions.clone();

        // A legacy snapshot = the doc JSON with the field present (real shape).
        let mut v = serde_json::to_value(&doc).unwrap();
        v.as_object_mut().unwrap().insert(
            "wall_face_partitions".to_string(),
            serde_json::to_value(&map).unwrap(),
        );
        let back: StoreyDoc = serde_json::from_value(v).unwrap();
        assert_eq!(
            back.wall_face_partitions, map,
            "legacy snapshot field still deserializes (migration-safe)"
        );
    }

    /// Committing a cut clears that wall's 1-D `wall_sections`.
    #[test]
    fn commit_cut_clears_wall_sections() {
        let mut doc = square_doc();
        let vid = first_wall(&doc);
        // Pre-existing 1-D section on the wall.
        doc.set_wall_section(vid, 1.0, 3.0, 4.0, WallExposure::SemiExposed);
        assert!(doc.wall_sections.contains_key(&vid));

        doc.commit_partition_cut(
            vid,
            PartitionCut {
                a: Point2f32::new(0.0, 1.0),
                b: Point2f32::new(4.0, 1.0),
            },
        );
        assert!(
            !doc.wall_sections.contains_key(&vid),
            "partition supersedes 1-D sections"
        );
        assert!(doc.is_wall_partitioned(vid));
        assert_eq!(doc.wall_face_partitions[&vid].cuts.len(), 1);
    }

    /// A zero-length cut is ignored (no entry created).
    #[test]
    fn zero_length_cut_is_ignored() {
        let mut doc = square_doc();
        let vid = first_wall(&doc);
        let p = Point2f32::new(2.0, 1.0);
        doc.commit_partition_cut(vid, PartitionCut { a: p, b: p });
        assert!(!doc.is_wall_partitioned(vid));
    }

    /// One diagonal cut → two regions; per-side exposure attribution sticks and a
    /// sample in region A does not bleed into region B.
    #[test]
    fn diagonal_cut_two_regions_distinct_exposures() {
        let mut doc = square_doc();
        let vid = first_wall(&doc);
        let (len, height) = (4.0_f32, 2.4_f32);
        // A diagonal from corner to corner splits the face into two triangles.
        doc.commit_partition_cut(
            vid,
            PartitionCut {
                a: Point2f32::new(0.0, 0.0),
                b: Point2f32::new(len, height),
            },
        );
        let regions = doc.partition_face_regions(vid, len, height);
        assert_eq!(regions.len(), 2, "one cut → two regions");
        // Both unset before attribution.
        assert!(regions.iter().all(|(_, e)| *e == WallExposure::Unset));

        // Sample below the diagonal (large x, small y) is in one triangle;
        // sample above the diagonal (small x, large y) is in the other.
        let below = Point2f32::new(3.0, 0.3);
        let above = Point2f32::new(1.0, 2.0);
        doc.set_partition_region_exposure(vid, below, WallExposure::Ground, len, height);
        doc.set_partition_region_exposure(vid, above, WallExposure::Exposed, len, height);

        let regions = doc.partition_face_regions(vid, len, height);
        // Each region carries the exposure of the sample inside it.
        let below_exp = regions
            .iter()
            .find(|(poly, _)| point_in_polygon(poly, below))
            .map(|(_, e)| *e);
        let above_exp = regions
            .iter()
            .find(|(poly, _)| point_in_polygon(poly, above))
            .map(|(_, e)| *e);
        assert_eq!(below_exp, Some(WallExposure::Ground));
        assert_eq!(above_exp, Some(WallExposure::Exposed));
        assert_ne!(below_exp, above_exp, "no bleed across the cut");
    }

    /// Re-attributing the SAME region replaces (does not duplicate) its exposure.
    #[test]
    fn re_attribute_same_region_replaces() {
        let mut doc = square_doc();
        let vid = first_wall(&doc);
        let (len, height) = (4.0_f32, 2.4_f32);
        doc.commit_partition_cut(
            vid,
            PartitionCut {
                a: Point2f32::new(0.0, 0.0),
                b: Point2f32::new(len, height),
            },
        );
        let s1 = Point2f32::new(3.0, 0.3);
        let s2 = Point2f32::new(3.5, 0.2); // same triangle as s1
        doc.set_partition_region_exposure(vid, s1, WallExposure::Ground, len, height);
        doc.set_partition_region_exposure(vid, s2, WallExposure::Exposed, len, height);
        assert_eq!(
            doc.wall_face_partitions[&vid].regions.len(),
            1,
            "same region updated in place, not duplicated"
        );
        assert_eq!(
            doc.wall_face_partitions[&vid].regions[0].1,
            WallExposure::Exposed
        );
    }

    /// Un-partitioned wall → one full-face rectangle region, Unset.
    #[test]
    fn unpartitioned_wall_is_one_unset_face() {
        let doc = square_doc();
        let vid = first_wall(&doc);
        let regions = doc.partition_face_regions(vid, 4.0, 2.4);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].1, WallExposure::Unset);
        assert_eq!(regions[0].0.len(), 4, "a rectangle");
    }

    /// `InterStorey` is refused as an attribution input — it's the derivation's
    /// stamp (`derive_inter_storey`), never assessor-painted: the call is a
    /// no-op and the region keeps its previous exposure.
    #[test]
    fn set_partition_region_exposure_refuses_inter_storey() {
        let mut doc = square_doc();
        let vid = first_wall(&doc);
        let (len, height) = (4.0_f32, 2.4_f32);
        doc.commit_partition_cut(
            vid,
            PartitionCut {
                a: Point2f32::new(0.0, 1.0),
                b: Point2f32::new(len, 1.0),
            },
        );
        let sample = Point2f32::new(2.0, 0.5);
        doc.set_partition_region_exposure(vid, sample, WallExposure::InterStorey, len, height);
        assert!(
            doc.wall_face_partitions[&vid].regions.is_empty(),
            "InterStorey attribution refused — nothing stored"
        );
        // An existing mark isn't clobbered by a refused InterStorey re-paint.
        doc.set_partition_region_exposure(vid, sample, WallExposure::Ground, len, height);
        doc.set_partition_region_exposure(vid, sample, WallExposure::InterStorey, len, height);
        assert_eq!(
            doc.wall_face_partitions[&vid].regions,
            vec![(sample, WallExposure::Ground)],
            "previous exposure kept"
        );
    }

    /// `clear_wall_partition` returns the wall to the un-partitioned state.
    #[test]
    fn clear_partition_removes_entry() {
        let mut doc = square_doc();
        let vid = first_wall(&doc);
        doc.commit_partition_cut(
            vid,
            PartitionCut {
                a: Point2f32::new(0.0, 1.0),
                b: Point2f32::new(4.0, 1.0),
            },
        );
        assert!(doc.is_wall_partitioned(vid));
        doc.clear_wall_partition(vid);
        assert!(!doc.is_wall_partitioned(vid));
    }
}

/// Two face boxes describe the SAME surface when they overlap by at least this
/// fraction of BOTH their areas — the threshold at which re-attributing is an
/// update rather than a new sub-surface.
const SAME_SURFACE_FRAC: f32 = 0.6;

/// Area of a face box, in m².
fn span_area(along: (f32, f32), height: (f32, f32)) -> f32 {
    ((along.1 - along.0) * (height.1 - height.0)).max(0.0)
}

/// Overlap area of two face boxes, in m².
fn span_overlap_area(
    a_along: (f32, f32),
    a_height: (f32, f32),
    b_along: (f32, f32),
    b_height: (f32, f32),
) -> f32 {
    let along = (a_along.1.min(b_along.1) - a_along.0.max(b_along.0)).max(0.0);
    let height = (a_height.1.min(b_height.1) - a_height.0.max(b_height.0)).max(0.0);
    along * height
}
