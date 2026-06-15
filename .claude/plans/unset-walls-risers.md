# Ridge length = region span · WallExposure::Unset · riser walls (api-engineer, 2026-06-12)
# APPROVED with defaults: riser exposure floor=Ground / ceiling=Exposed (editing deferred — not
# reachable by the outline painter; future 3D-tap picker); whole-floor-riser=Ground is the v1
# approximation (no grade model; below-grade perimeter strips also deferred); ridge start-snap
# kept (recoverable via tag); 2D legend entry cosmetic-skip; DEAP Unset restriction deferred to
# the export boundary (noted in the enum doc).

## Slices: A = U-1+L-1+L-2+L-3 (ridge limit + enum) · B = U-2+U-3 (Unset model + render) ·
## C = RS-1+RS-2 (risers). A independent; B before C.

## 1. Ridge length limit (slices in A)
L-1 datatypes: impl CeilingRegion {
  /// The contiguous run of the plan line `p + t·dir` (`t` in METRES, `dir`
  /// unit-length) that stays inside this region's plan polygon and contains
  /// t = 0, as `(t_min, t_max)` with t_min ≤ 0 ≤ t_max. Even-odd over the
  /// ring's crossings; an edge lying along the line contributes its
  /// endpoints' t (the profile_breakpoints convention). `None` when `p` is
  /// not strictly inside (boundary excluded).
  pub fn plan_span_through(&self, p: Point2f64, dir: Point2f64) -> Option<(f64, f64)> }
  (point+direction — datatypes can't see ParameterAxis; SplitLine3D vocabulary.)
  Tests: rectangle full chord; notched ring (interval containing t=0, not union); boundary →
  None; degenerate → None.
L-2 document/roof_point.rs: StoreyDoc::validate_roof_ridge_extent DELETED; free fn
  pub fn validate_roof_ridge_extent(storey: &Storey, axis: ParameterAxis, coord_m: f32,
      lo: f32, len_m: f32) -> Result<(), String>
  — start (coord,lo)/(lo,coord) per the ridge axis convention must be STRICTLY inside one
  region (region_strictly_containing); else NEW stable RIDGE_START_OUTSIDE_CEILING_MSG ("The
  ridge's start doesn't sit inside a ceiling area — a ridge can't start on a wall or an area
  boundary yet."). max_len = t_max of plan_span_through(start, +extent-axis unit). Accept iff
  len ≤ max_len − RIDGE_CLEAR_TOL_M (1e-3 — RENAMED from RIDGE_TOUCH_TOL_M: it's the CLEARANCE
  keeping Length-acceptance ⇒ Height-stage strict containment; the 06-12 touch allowance is
  REVERSED deliberately). Over-length → RIDGE_LENGTH_EXCEEDS_AREA_MSG ("That length would push
  the ridge out of its ceiling area — enter a length that fits") + " (under {max:.2} m from
  that start)." RIDGE_LENGTH_OFF_FOOTPRINT_MSG + bbox logic die. len ≤ 1 cm message unchanged.
  Wall-to-wall test cases FLIP to rejection (boundary-apex limitation made honest+early).
L-3 controller roof_ridges.rs: assign_roof_ridge_length internals — self.materialize() first
  (explicit; don't ride the render loop), fetch storey like assign_roof_ridge_height, call the
  free fn. Pending stays open on Err. Controller test: raised-section scenario — span-length
  rejected WITH range, just-under accepted, then Height succeeds (no-late-rejection, e2e).

## 2. WallExposure::Unset (slices in B; U-1 enum lands in A's datatypes edit)
U-1 datatypes: WallExposure gains
    /// No exposure marked yet. Walls exist (and draw) Unset; DEAP export will
    /// refuse a BER containing Unset regions — that restriction lives at the
    /// future export boundary (the deap mapping layer), not here.
    Unset,
    /// In contact with the ground (floor-step risers; DEAP treats
    /// ground-contact walls separately from Exposed).
    Ground,
  No Default derive (nothing calls it; exposure not part of construction identity — verified).
U-2 document/exposure.rs (signatures unchanged, semantics):
  - wall_coverage: gap fill + no-sections whole-wall → Unset (doc rewrite).
  - set_wall_section: DROP the materialize-the-gaps step — wall_sections stores ONLY painted
    sections; Unset exists only in derived coverage (single-representation rule: in-session ==
    post-reload with no canonicalize). range_at unchanged.
  - wall_exposure_segments: FULL perimeter — unmarked walls → one full-length Unset segment;
    marked walls → sections + Unset gaps (the return-empty arm dies).
  - sections_from_wall_regions: SKIP Unset AND Ground regions (else one save/load marks every
    wall / risers become phantom sections).
  - set_all_remaining_exposed: paint every Unset coverage RANGE Exposed (whole walls AND gaps —
    gap case mandatory post-reload); returns changed as today.
  - resolve.rs comment rewrites only ("ONLY marked walls" paragraph). Persist filter unchanged.
U-3 src/ui/render:
  - style: UNSET_WALL grey (150,150,150); GROUND_WALL earth (e.g. 150,105,60).
  - storey_3d draw_wall_region match gains both arms.
  - DROP the volume-hint band entirely: draw_floor_region loses band quads + band_faded;
    render_at_z_offset bookkeeping; module doc rewrite (every wall now has a real roof-tracing
    region — Unset walls genuinely follow raised/ridged roofs). full_house_3d inherits.
  - 2D color_of gains Unset => grey / Ground => GROUND_WALL exhaustive arms; "Clear" tooltip →
    "back to unset".
TEST WAVE (deliberate flips, change with the code): document exposure
  coverage_fills_gaps_with_exposed / coverage_of_no_sections_is_all_exposed → Unset; resolve
  golden_projection (1 wall region → 4: SemiExposed + three Unset, select by exposure not
  index), height_set_derives..., wall_under_step/ridge identity tests (region indexing),
  sections_from_wall_regions_round_trips (+ assert Unset recovers to NO sections); controller
  tests.rs ~1237–1310, 1417, 1939–1994 (set_all_remaining... now incl. gaps — rename), 4673,
  4768.

## 3. Risers (slices in C)
RS-1 datatypes:
  impl FloorRegions {
    /// Interior FLOOR-RISER faces: for every plan edge piece shared by two
    /// regions whose `level_m` differ (> 1e-4 m), the vertical quad from the
    /// lower to the higher level. exterior[0]/[1] = the base edge at the
    /// LOWER level. Outline edges never emit (a piece must be shared by TWO
    /// regions; regions tile with disjoint interiors).
    pub fn riser_faces(&self) -> Vec<Polygon3D> }
  impl CeilingRegions {
    /// Interior CEILING-RISER faces: shared plan edge pieces where the two
    /// (linear) z profiles differ by > 1e-4 m somewhere → the vertical face
    /// between them (quad; split into two triangles at a profile crossing;
    /// sub-sliver dropped). Continuous fan/ridge seams (equal profiles) emit
    /// nothing. Base edge = the LOWER profile.
    pub fn riser_faces(&self) -> Vec<Polygon3D> }
  impl WallRegions {
    /// Append derived RISER faces with `regenerate`'s amend identity
    /// (injective best-match on the base edge, 1e-2 m) — EXCLUDING previous
    /// ids `self` already holds; unmatched-but-covered inherit cid only;
    /// else default cid. Deterministic re-derivation ⇒ stable identity.
    pub fn with_risers(self, faces: Vec<(Polygon3D, WallExposure)>,
        previous: &WallRegions, default_construction_id: ItemId) -> Self }
  Internals: shared-edge extraction via metric collinearity (PROFILE_EPS_M style — boolean-op
  output, NOT exact vertex equality) + 1D overlap > 1e-3; one private helper shared by both
  riser_faces; extract regenerate's matcher into a private helper both passes call.
  Tests: floor step → one riser quad on the interior boundary lower→upper, none on outline;
  ceiling step → one face z1→z2; fan/ridge seams → none; crossing profiles → two triangles;
  with_risers amend keeps id/cid/opes; id-uniqueness vs perimeter amends; no duplicate ids.
RS-2 document/resolve.rs walls block:
  previous_walls = mem::take(&mut storey.walls) BEFORE regenerate; regenerate(...,
  &previous_walls, ...); risers = floors.riser_faces()→Ground + roofs_for_walls.riser_faces()
  →Exposed; storey.walls = walls.with_risers(risers, &previous_walls, wall_cid).
  Floor risers derive regardless of height; ceiling risers off roofs_for_walls (empty default
  pre-height — no gating needed). The SEAM comment DIES → short derivation statement + v1
  approximations. Perimeter no-duplication holds by construction (outline edges border one
  region). Tests: floor step → Ground riser + unique ids; re-resolve → riser id stable;
  ceiling step → Exposed interior riser coexists with the perimeter 6-vertex jump; reload →
  wall_sections untouched by risers; U-2 fixpoints hold.

## Behaviour traps
1. No-late-rejection invariant (Length ⇒ Height). 2. Stable strings move with tests. 3.
Snapped-start ridges now fail at Length honestly — don't relax. 4. Unset never enters
wall_sections. 5. The Exposed-fill test wave is deliberate. 6. Recovery skips Unset+Ground.
7. Riser id uniqueness (exclusion set). 8. Riser predicate = z-discontinuity, not adjacency.
9. exterior[0]/[1] = base edge everywhere. 10. Serde: variant additions only; no snapshot bump.
11. regenerate + risers stay O(n·m) direct, every materialize. 12. Nothing new enters undo.
