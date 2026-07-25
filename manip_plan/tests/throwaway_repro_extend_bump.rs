//! THROWAWAY REPRO TEST — DO NOT COMMIT.
//!
//! Reproduces the field symptom (Elmwood 2026-07-25): extend-ortho bump with
//! two cut-in corners on a fully-dimensioned rectangle → "8 left" collinearity
//! review of irrelevant pairs (all among pre-existing walls), then dimension
//! prompts for invisible / already-settled walls.
//!
//! Mirrors src/controller/app_controller/strokes.rs `apply_extend_stroke`:
//! extend_ortho → clean_envelope_collinear (remove_collinear_vertices + resync)
//! → collinear_suggestions_for_params(&minted) → review → finish_review cleanup
//! → dimension loop via next_promptable_wall.
//!
//! Run: cargo test -p manip_plan --test throwaway_repro_extend_bump -- --nocapture --test-threads=1

use domain::points::Point2f32;
use manip_plan::{FloorPlan, ParameterId, VertexId};
use std::collections::{HashMap, HashSet};

fn p(x: f32, y: f32) -> Point2f32 {
    Point2f32::new(x, y)
}

fn edge_desc(plan: &FloorPlan, ring: usize, edge: usize) -> String {
    let polys = plan.polygons();
    let r = &polys[ring];
    let n = r.len();
    let a = r[edge % n];
    let b = r[(edge + 1) % n];
    format!(
        "r{ring}/e{edge} ({:.3},{:.3})->({:.3},{:.3}) len={:.3}",
        a.x,
        a.y,
        b.x,
        b.y,
        plan.wall_length(ring, edge).unwrap_or(f32::NAN)
    )
}

/// (vid, position) for every ring-0 vertex.
fn vertex_snapshot(plan: &FloorPlan) -> Vec<(VertexId, Point2f32)> {
    let pts = plan.get_points();
    (0..pts.len())
        .map(|i| (plan.vertex_id_at(0, i).unwrap(), pts[i]))
        .collect()
}

/// wall (start vid) -> its movable param, for identity diffing.
fn wall_param_snapshot(plan: &FloorPlan) -> HashMap<VertexId, Option<ParameterId>> {
    let pts = plan.get_points();
    (0..pts.len())
        .map(|i| {
            (
                plan.vertex_id_at(0, i).unwrap(),
                plan.wall_movable_param(0, i).map(|(q, _)| q),
            )
        })
        .collect()
}

fn dump_suggestions(
    plan: &FloorPlan,
    minted: &[ParameterId],
    sugs: &[manip_plan::CollinearitySuggestion],
    old_vids: &HashSet<VertexId>,
) {
    let minted_set: HashSet<ParameterId> = minted.iter().copied().collect();
    let side = |ring: usize, edge: usize| -> String {
        let vid = plan.vertex_id_at(ring, edge);
        let age = match vid {
            Some(v) if old_vids.contains(&v) => "OLD-WALL",
            Some(_) => "new-wall",
            None => "?",
        };
        let tag = match plan.wall_movable_param(ring, edge) {
            Some((pp, _)) if minted_set.contains(&pp) => format!("param P{pp} MINTED"),
            Some((pp, _)) => format!("param P{pp} pre-existing"),
            None => "anchor(const)".into(),
        };
        format!("{} [{age}, {tag}]", edge_desc(plan, ring, edge))
    };
    println!("  ===> {} suggestion(s) in the review:", sugs.len());
    for (i, s) in sugs.iter().enumerate() {
        println!(
            "  #{i}: axis={:?} sep={:.3} (coords {:.3} vs {:.3})",
            s.axis,
            (s.coord_a - s.coord_b).abs(),
            s.coord_a,
            s.coord_b
        );
        println!("      A: {}", side(s.ring_a, s.edge_a));
        println!("      B: {}", side(s.ring_b, s.edge_b));
    }
}

fn resync(plan: &FloorPlan, known: &mut HashSet<ParameterId>) {
    let active = plan.all_active_params();
    known.retain(|q| active.contains(q));
}

fn run_dimension_loop(plan: &FloorPlan, known: &mut HashSet<ParameterId>, minted: &[ParameterId]) {
    let minted_set: HashSet<ParameterId> = minted.iter().copied().collect();
    let skipped: HashSet<VertexId> = HashSet::new();
    let mut count = 0;
    println!("  dimension prompts (post-review):");
    while let Some((vid, param)) = plan.next_promptable_wall(known, &skipped) {
        let (ring, edge) = plan.find_vertex(vid).unwrap();
        let len = plan.wall_length(ring, edge).unwrap_or(f32::NAN);
        let origin = if minted_set.contains(&param) {
            "minted-by-extend"
        } else {
            "RE-ASKED PRE-EXISTING PARAM"
        };
        let invisible = if len < 0.25 {
            "  <<< TINY/INVISIBLE WALL"
        } else {
            ""
        };
        println!(
            "    prompt: vid {vid} {} sets P{param} [{origin}]{invisible}",
            edge_desc(plan, ring, edge)
        );
        known.insert(param);
        count += 1;
        assert!(count < 60, "runaway prompt loop");
    }
    println!("    ({count} prompt(s) total)");
}

fn extend_and_review(label: &str, outline: &[Point2f32], stroke: &[Point2f32], accept_all: bool) {
    println!("\n================ {label} (accept_all={accept_all}) ================");
    let mut plan = FloorPlan::from_clean_ring(outline).unwrap();
    let mut known: HashSet<ParameterId> = plan.all_active_params();
    let params_before = known.clone();
    let verts_before = vertex_snapshot(&plan);
    let wall_params_before = wall_param_snapshot(&plan);
    let old_vids: HashSet<VertexId> = verts_before.iter().map(|(v, _)| *v).collect();
    println!(
        "before: {} verts, {} active params (all known): {:?}",
        plan.polygons()[0].len(),
        plan.active_parameter_count(),
        {
            let mut v: Vec<_> = params_before.iter().collect();
            v.sort();
            v
        }
    );

    let _ = manip_plan::take_splice_debug();
    let minted = match plan.extend_ortho(stroke) {
        Ok(m) => m,
        Err(e) => {
            println!("extend_ortho REJECTED: {e}");
            return;
        }
    };
    for ph in manip_plan::take_splice_debug() {
        let pts: Vec<String> = ph
            .points
            .iter()
            .map(|q| format!("({:.3},{:.3})", q.x, q.y))
            .collect();
        println!("  phase '{}': {}", ph.label, pts.join(" "));
    }
    println!("minted {} params: {:?}", minted.len(), minted);

    // ---- identity diff ----
    let active_after = plan.all_active_params();
    let lost_params: Vec<_> = params_before
        .iter()
        .filter(|q| !active_after.contains(q))
        .collect();
    let verts_after = vertex_snapshot(&plan);
    let vids_after: HashSet<VertexId> = verts_after.iter().map(|(v, _)| *v).collect();
    let lost_vids: Vec<_> = verts_before
        .iter()
        .filter(|(v, _)| !vids_after.contains(v))
        .map(|(v, q)| format!("vid {v}@({:.3},{:.3})", q.x, q.y))
        .collect();
    let wall_params_after = wall_param_snapshot(&plan);
    let rekeyed: Vec<String> = wall_params_before
        .iter()
        .filter_map(|(vid, old_p)| {
            let new_p = wall_params_after.get(vid)?;
            (old_p != new_p).then(|| format!("wall@vid {vid}: {:?} -> {:?}", old_p, new_p))
        })
        .collect();
    println!("IDENTITY: pre-existing params lost: {:?}", lost_params);
    println!("IDENTITY: pre-existing vertices lost: {:?}", lost_vids);
    println!(
        "IDENTITY: surviving walls whose movable param CHANGED: {:?}",
        rekeyed
    );

    // controller: clean_envelope_collinear
    let removed = plan.remove_collinear_vertices(&HashSet::new());
    println!("clean_envelope_collinear removed vids: {:?}", removed);
    resync(&plan, &mut known);

    println!(
        "plan diag = {:.3} → review max_sep = {:.3}",
        plan.bounding_box_diagonal(),
        plan.bounding_box_diagonal() * 0.12
    );
    let sugs = plan.collinear_suggestions_for_params(&minted);
    dump_suggestions(&plan, &minted, &sugs, &old_vids);

    if accept_all {
        for s in &sugs {
            let deg = |ring, edge| plan.wall_length(ring, edge).map_or(true, |l| l < 1e-3);
            if plan.edges_collinear(s.ring_a, s.edge_a, s.ring_b, s.edge_b, &s.axis)
                || deg(s.ring_a, s.edge_a)
                || deg(s.ring_b, s.edge_b)
            {
                continue;
            }
            plan.apply_collinear_suggestion(s);
        }
    }
    let removed2 = plan.remove_collinear_vertices(&HashSet::new());
    println!("post-review cleanup removed vids: {:?}", removed2);
    resync(&plan, &mut known);

    println!("final ring:");
    for (i, q) in plan.polygons()[0].iter().enumerate() {
        println!("   v{i} ({:.4}, {:.4})", q.x, q.y);
    }
    run_dimension_loop(&plan, &mut known, &minted);
}

/// V1: screenshot-shaped bump drawn LARGE enough to survive the cut pipeline's
/// RDP ceiling (0.7 m) and the snap_walls leveller (~0.9 m here): depth 4 m,
/// cut-ins 1.5 m and 1.0 m. (On the tablet the 1.00/0.50 labels are typed-in
/// dimension values applied AFTER the drawn geometry; the drawn steps were
/// bigger.) Attach points 0.2 m in from the bottom corners.
#[test]
fn v1_deep_bump_survives() {
    let rect = [p(0.0, 0.0), p(10.4, 0.0), p(10.4, 8.0), p(0.0, 8.0)];
    let stroke = [
        p(0.2, 0.0),
        p(0.2, -4.0),
        p(3.2, -4.0),
        p(3.2, -2.5),
        p(6.2, -2.5),
        p(6.2, -1.5),
        p(10.2, -1.5),
        p(10.2, 0.0),
    ];
    extend_and_review("V1 deep bump, SKIP/REJECT-ALL", &rect, &stroke, false);
    extend_and_review("V1 deep bump, ACCEPT-ALL", &rect, &stroke, true);
}

/// V2: one cut-in at 0.75 m — above the RDP ceiling (0.7) but below the
/// snap_walls default (~6% of the spliced-ring diag): does the drawn step get
/// silently flattened?
#[test]
fn v2_shallow_cutin_flattened() {
    let rect = [p(0.0, 0.0), p(10.4, 0.0), p(10.4, 8.0), p(0.0, 8.0)];
    let stroke = [
        p(0.2, 0.0),
        p(0.2, -4.0),
        p(3.2, -4.0),
        p(3.2, -3.25), // 0.75 m cut-in
        p(6.2, -3.25),
        p(6.2, -1.5),
        p(10.2, -1.5),
        p(10.2, 0.0),
    ];
    extend_and_review("V2 0.75m cut-in, SKIP/REJECT-ALL", &rect, &stroke, false);
}

/// V3: attach ends land 0.05 m from the bottom corners — inside the cleanup's
/// dedup tolerance — the corner-nudge identity-loss case the PARKED tests in
/// path_edits.rs document.
#[test]
fn v3_attach_near_corner_identity() {
    let rect = [p(0.0, 0.0), p(10.4, 0.0), p(10.4, 8.0), p(0.0, 8.0)];
    let stroke = [
        p(0.05, 0.0),
        p(0.05, -4.0),
        p(3.2, -4.0),
        p(3.2, -2.5),
        p(6.2, -2.5),
        p(6.2, -1.5),
        p(10.35, -1.5),
        p(10.35, 0.0),
    ];
    extend_and_review("V3 attach 5cm from corners", &rect, &stroke, false);
}

/// V4: the Elmwood storey-0 geometry as recovered from the backup: pre-extend
/// outline from the resolved regions, stroke = the resolved bump profile
/// (steps 0.782 m, plus the field's 2 mm run mismatch).
#[test]
fn v4_elmwood_asfound() {
    let outline = [
        p(0.6627, 1.4229),
        p(9.6087, 1.4229),
        p(9.6087, 9.4529),
        p(5.8267, 9.4529),
        p(5.8267, 8.8889),
        p(3.0447, 8.8889),
        p(3.0447, 9.8249),
        p(0.6627, 9.8249),
    ];
    let stroke = [
        p(0.6627, 1.4229),
        p(0.6627, -2.6301),
        p(2.0577, -2.6301),
        p(2.0577, -3.4121),
        p(4.8667, -3.4121),
        p(4.8667, -2.6321),
        p(5.9347, -2.6321),
        p(5.9347, -0.1871),
        p(9.1007, -0.1871),
        p(9.1007, 1.4229),
    ];
    extend_and_review("V4 elmwood as-found", &outline, &stroke, false);
}

/// V5: attach ends genuinely MID-WALL (the field case: "attached straight
/// across from the closest window"), deep survivable profile. The bottom wall
/// splits into two flank stubs; expect suggestion spam pairing each stub with
/// each nearby bump run, and dimension prompts on flanks of the
/// already-dimensioned bottom wall.
#[test]
fn v5_midwall_attach() {
    let rect = [p(0.0, 0.0), p(10.4, 0.0), p(10.4, 8.0), p(0.0, 8.0)];
    let stroke = [
        p(1.5, 0.0),
        p(1.5, -4.0),
        p(4.0, -4.0),
        p(4.0, -2.5),
        p(6.5, -2.5),
        p(6.5, -1.5),
        p(9.0, -1.5),
        p(9.0, 0.0),
    ];
    extend_and_review("V5 mid-wall attach, SKIP/REJECT-ALL", &rect, &stroke, false);
    extend_and_review("V5 mid-wall attach, ACCEPT-ALL", &rect, &stroke, true);
}

/// LIVE SESSION (2026-07-25 23:04, record 5db492c9): rebuild the EXACT
/// post-extend plan from the saved parametric snapshot and enumerate the
/// review, classifying every pair against the user's law:
/// "only ask when a NEW wall whose dimension is UNKNOWN is close to a wall
///  that IS dimensioned".
/// Requires --features serde.
#[cfg(feature = "serde")]
#[test]
fn live_session_snapshot_enumeration() {
    let path = "/tmp/claude-1000/-home-anishrag-WORK-BER-repos-BER-APP/a971b0a7-e923-4e83-a327-5eba7ed780d4/scratchpad/livetest/snapshot_fp.json";
    let Ok(txt) = std::fs::read_to_string(path) else {
        eprintln!("snapshot_fp.json not present — extract it first");
        return;
    };
    let plan: FloorPlan = serde_json::from_str(&txt).expect("deserialize FloorPlan");
    let known: HashSet<ParameterId> = [0, 1, 2, 3, 4, 5, 6, 7].into_iter().collect();
    let minted: Vec<ParameterId> = vec![8, 9, 10, 11, 12, 13];
    let minted_set: HashSet<ParameterId> = minted.iter().copied().collect();

    println!("\n========== LIVE SESSION plan (from parametric_snapshot) ==========");
    for (i, q) in plan.polygons()[0].iter().enumerate() {
        println!("  v{i} ({:.4},{:.4})", q.x, q.y);
    }
    let diag = plan.bounding_box_diagonal();
    println!("diag={:.4} → max_sep = {:.4}", diag, diag * 0.12);

    let sugs = plan.collinear_suggestions_for_params(&minted);
    println!("===> {} suggestions (tablet showed '11 left')", sugs.len());
    let classify = |ring: usize, edge: usize| -> (String, String) {
        // status of the wall's PERPENDICULAR (movable) param
        let mp = plan.wall_movable_param(ring, edge);
        let perp = match mp {
            Some((pp, _)) if minted_set.contains(&pp) => format!("P{pp} MINTED/unknown"),
            Some((pp, _)) if known.contains(&pp) => format!("P{pp} KNOWN"),
            Some((pp, _)) => format!("P{pp} unknown(old!)"),
            None => "anchor-const (settled)".into(),
        };
        // is the wall's LENGTH dimensioned?
        let deps = plan.wall_param_deps(ring, edge);
        let len_known = !deps.is_empty() && deps.iter().all(|d| known.contains(d));
        (
            perp,
            if len_known {
                "len KNOWN".into()
            } else {
                "len UNKNOWN".into()
            },
        )
    };
    for (i, s) in sugs.iter().enumerate() {
        let (pa, la) = classify(s.ring_a, s.edge_a);
        let (pb, lb) = classify(s.ring_b, s.edge_b);
        let new_a = plan
            .wall_movable_param(s.ring_a, s.edge_a)
            .is_some_and(|(q, _)| minted_set.contains(&q));
        let new_b = plan
            .wall_movable_param(s.ring_b, s.edge_b)
            .is_some_and(|(q, _)| minted_set.contains(&q));
        let verdict = match (new_a, new_b) {
            (true, true) => "NEW x NEW (law: don't ask?)",
            (true, false) | (false, true) => "new-unknown x settled (law: legit)",
            (false, false) => "SETTLED x SETTLED (law: VIOLATION)",
        };
        println!(
            "#{i}: {:?} sep={:.3}  A {} [{pa}; {la}]  B {} [{pb}; {lb}]  => {verdict}",
            s.axis,
            (s.coord_a - s.coord_b).abs(),
            edge_desc(&plan, s.ring_a, s.edge_a),
            edge_desc(&plan, s.ring_b, s.edge_b),
        );
    }
}

/// LIVE SESSION recreation: pre-extend outline (recovered from the spliced
/// ring) + the session's RDP-simplified stroke → confirm the same cleaned
/// ring, minted count, and suggestion count fall out of the pipeline.
#[test]
fn live_session_recreate() {
    let outline = [
        p(1.9992914, 11.347006),
        p(4.9992914, 11.347006),
        p(4.9992914, 10.347006),
        p(7.9992914, 10.347006),
        p(7.9992914, 10.847006),
        p(11.999291, 10.847006),
        p(11.999291, 2.8470058),
        p(11.499291, 2.8470058),
        p(11.499291, -5.152994),
        p(1.9992914, -5.152994),
    ];
    // The session's 'simplified (RDP)' stage (the effective drawn geometry).
    let stroke = [
        p(2.1012232, -4.1216245),
        p(1.7914282, -9.581499),
        p(4.1015806, -9.483887),
        p(4.540835, -11.55001),
        p(7.518005, -11.371055),
        p(7.1600947, -9.418812),
        p(9.2750225, -9.386274),
        p(9.242484, -4.928654),
    ];
    extend_and_review("LIVE recreation", &outline, &stroke, false);
}
