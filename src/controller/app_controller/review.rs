//! The collinearity-review cursor: walking the post-stroke merge
//! suggestions, accept/reject/skip, and populating the review queue.
use super::*;

/// In-session state for the "is this pair of walls the same line?" review.
/// Populated by `sketch_end_stroke` and consumed by `accept_current_review` /
/// `reject_current_review`.
#[derive(Debug, Default)]
pub struct CollinearityReview {
    /// All candidate pairs the detector returned for the current floor plan.
    pub suggestions: Vec<CollinearitySuggestion>,
    /// Index of the next suggestion to consider. We walk forward, skipping
    /// any pair that's become "already determined" (yes-side via the live
    /// parametric state, no-side via the `rejections` list).
    pub cursor: usize,
    /// Pairs the user explicitly said No to. Stored as full suggestions so
    /// we have the ring indices available for transitive-rejection checking.
    /// Reused for: if (A,B) was rejected and (A,C) gets approved later
    /// (C joins A's equivalence class), then asking (B,C) is redundant.
    pub rejections: Vec<CollinearitySuggestion>,
}

impl AppController {
    /// COLLINEARITY REVIEW-------------------------------------------
    /// The next collinearity suggestion the UI should ask the user about,
    /// or None if there's nothing pending. Walks forward from the cursor,
    /// skipping suggestions that have become "already determined" since the
    /// list was generated (yes-side via the live parametric state, no-side
    /// via the rejections list).
    pub fn current_review_suggestion(&self) -> Option<&CollinearitySuggestion> {
        let review = self.review.as_ref()?;
        for sug in &review.suggestions[review.cursor..] {
            if !self.review_should_skip(sug) {
                return Some(sug);
            }
        }
        None
    }

    /// Clone of the current suggestion, for cheap UI handoff without borrow gymnastics.
    pub fn current_review_suggestion_clone(&self) -> Option<CollinearitySuggestion> {
        self.current_review_suggestion().cloned()
    }

    /// Number of suggestions still to be considered (including the current).
    pub fn review_remaining(&self) -> usize {
        let Some(review) = self.review.as_ref() else {
            return 0;
        };
        review.suggestions[review.cursor..]
            .iter()
            .filter(|s| !self.review_should_skip(s))
            .count()
    }

    /// User said Yes: apply the suggestion, advance past it, commit.
    pub fn accept_current_review(&mut self) -> Result<(), String> {
        let Some(sug) = self.next_pending_suggestion() else {
            return Ok(());
        };
        self.record_history();
        println!(
            "collinearity YES: ring {}/edge {} & ring {}/edge {} along {:?} (coords {:.2}, {:.2})",
            sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, sug.axis, sug.coord_a, sug.coord_b
        );
        self.document.floor_plan.apply_collinear_suggestion(&sug);
        println!(
            "  → active_parameter_count = {}",
            self.document.floor_plan.active_parameter_count()
        );
        self.advance_review_cursor();
        let result = self.mark_dirty();
        self.after_review_step();
        result
    }

    /// User said No: record the rejection (for transitive skipping), advance past it.
    pub fn reject_current_review(&mut self) {
        let Some(sug) = self.next_pending_suggestion() else {
            return;
        };
        self.record_history();
        println!(
            "collinearity NO:  ring {}/edge {} & ring {}/edge {} along {:?} (coords {:.2}, {:.2})",
            sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, sug.axis, sug.coord_a, sug.coord_b
        );
        if let Some(review) = self.review.as_mut() {
            review.rejections.push(sug);
        }
        self.advance_review_cursor();
        self.after_review_step();
    }

    /// User said Skip-all: clear the entire review without applying anything.
    /// Once the collinearity review is gone the controller picks up the
    /// dimensioning flow, if there's anything to ask about.
    pub fn skip_all_review(&mut self) {
        self.review = None;
        self.finish_review();
    }

    /// Internal: pop the next pending suggestion by cloning it (so callers
    /// don't have to juggle borrows against `self`).
    pub(super) fn next_pending_suggestion(&self) -> Option<CollinearitySuggestion> {
        self.current_review_suggestion().cloned()
    }

    /// Internal: move the cursor past the just-handled suggestion and any
    /// that follow which are now resolved. Clears `self.review` when there's
    /// nothing left to ask.
    pub(super) fn advance_review_cursor(&mut self) {
        if let Some(review) = self.review.as_mut() {
            review.cursor += 1;
        } else {
            return;
        }
        // Skip past any suggestions that are now resolved. We re-borrow each
        // iteration so `review_should_skip` can immutably borrow `self` while
        // we mutate `self.review`.
        loop {
            let next_sug = self
                .review
                .as_ref()
                .and_then(|r| r.suggestions.get(r.cursor).cloned());
            let Some(sug) = next_sug else {
                self.review = None;
                return;
            };
            if self.review_should_skip(&sug) {
                if let Some(r) = self.review.as_mut() {
                    r.cursor += 1;
                }
            } else {
                return;
            }
        }
    }

    /// Kick off dimensioning when the review naturally drains to None.
    /// Called from accept/reject paths after `advance_review_cursor`.
    pub(super) fn after_review_step(&mut self) {
        if self.review.is_none() {
            self.finish_review();
        }
    }

    /// Wrap up once the collinearity review is done (drained or skipped).
    /// Accepting "same line" merges two edges onto one coordinate, which leaves
    /// the vertex between them as a redundant straight-through point — drop
    /// those spurious collinear vertices (preserving deliberate subdivisions)
    /// so the user isn't later prompted for a length that can't be set without
    /// changing the shape. Then commit the cleaned outline and start dimensioning.
    pub(super) fn finish_review(&mut self) {
        self.clean_envelope_collinear();
        if let Err(e) = self.mark_dirty() {
            eprintln!("commit_to_ber after collinearity review: {}", e);
        }
        self.maybe_start_dimensioning();
    }

    /// Check the live state to decide if a suggestion is already-resolved
    /// (no need to ask). Either the two edges are already collinear, or the
    /// pair is transitively rejected via an earlier No.
    pub(super) fn review_should_skip(&self, sug: &CollinearitySuggestion) -> bool {
        if self
            .document
            .floor_plan
            .edges_collinear(sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, &sug.axis)
        {
            return true;
        }
        // A prior "same line" accept can collapse one of this pair's edges to
        // zero length (e.g. the short connector of a nib whose two parallel
        // sides were just merged). Asking about a degenerate edge is meaningless
        // — skip it; the end-of-review cleanup drops the vertex entirely.
        let degenerate = |ring, edge| {
            self.document
                .floor_plan
                .wall_length(ring, edge)
                .map_or(true, |l| l < 1e-3)
        };
        if degenerate(sug.ring_a, sug.edge_a) || degenerate(sug.ring_b, sug.edge_b) {
            return true;
        }
        let Some(review) = self.review.as_ref() else {
            return false;
        };
        // Transitive rejection: if any rejected pair is "the same question"
        // as `sug` (i.e. each side matches a side of the rejection through
        // existing equivalences), skip.
        for rej in &review.rejections {
            if rej.axis != sug.axis {
                continue;
            }
            let same = |sa: (usize, usize), ra: (usize, usize)| -> bool {
                (sa.0 == ra.0 && sa.1 == ra.1)
                    || self
                        .document
                        .floor_plan
                        .edges_collinear(sa.0, sa.1, ra.0, ra.1, &rej.axis)
            };
            let sug_a = (sug.ring_a, sug.edge_a);
            let sug_b = (sug.ring_b, sug.edge_b);
            let rej_a = (rej.ring_a, rej.edge_a);
            let rej_b = (rej.ring_b, rej.edge_b);
            let aa_bb = same(sug_a, rej_a) && same(sug_b, rej_b);
            let ab_ba = same(sug_a, rej_b) && same(sug_b, rej_a);
            if aa_bb || ab_ba {
                return true;
            }
        }
        false
    }

    /// Common tail after a stroke mutates the plan: persist, surface any
    /// collinearity questions, then (re)start dimensioning on the result.
    /// Callers snapshot history (`push_history_snapshot`) BEFORE calling this, so
    /// an undo entry exists even if the commit fails.
    pub(super) fn commit_review_redimension(&mut self) -> Result<(), String> {
        self.mark_dirty()?;
        self.populate_collinearity_review();
        self.maybe_start_dimensioning();
        Ok(())
    }

    /// After a committed extend/trim splice, drop the collinear vertices it left
    /// redundant — what matters is the resulting ENVELOPE, not that a porch was
    /// added separately. (A flush porch turns its attach corner into a mid-wall
    /// point; that's spurious.) Surviving vertices keep their IDs, so we just
    /// re-sync the dimensioning state to the params/vertices that remain.
    ///
    /// `preserve` is where deliberately-placed subdivision vertices go — e.g. a
    /// point marking a change of flooring type along an otherwise-straight wall,
    /// which is collinear but NOT spurious. None exist yet, so it's empty; when
    /// that feature lands, those vertex IDs are passed here and kept.
    pub(super) fn clean_envelope_collinear(&mut self) {
        let preserve: HashSet<VertexId> = HashSet::new();
        let removed = self
            .document
            .floor_plan
            .remove_collinear_vertices(&preserve);
        if !removed.is_empty() {
            println!(
                "Cleanup: removed {} spurious collinear vertex/vertices {:?}",
                removed.len(),
                removed
            );
        }
        self.resync_dimensioning_to_plan();
    }

    /// Detect near-collinear walls and populate the review if any.
    ///
    /// After AddDetachedSection appends a new ring, pairs entirely within older
    /// rings were either resolved when those rings were drawn or were
    /// intentionally left as distinct walls — don't re-ask. We only
    /// surface pairs that involve the newly-added ring. For DrawPerpWalls
    /// the floor_plan was just replaced with a single ring at index 0,
    /// so the filter is a no-op.
    pub(super) fn populate_collinearity_review(&mut self) {
        // Separations scale to the plan's extent so the same review behaviour
        // holds whether the plan is in pixels (pre-dimension) or metres (an
        // upper storey drawn in world coords). A fixed range tuned for pixels
        // would treat every wall in a metre-scale storey as near-collinear.
        let diag = self.document.floor_plan.bounding_box_diagonal();
        let min_sep = (diag * self.tuning.collinearity.min_separation_ratio).max(1e-4);
        let max_sep = diag * self.tuning.collinearity.max_separation_ratio;
        let newest_ring = self.document.floor_plan.ring_count().saturating_sub(1);
        // When this storey was drawn over a previous one, prefer to KEEP the
        // line that lies on the previous storey's outline as the merge target,
        // so a near-collinear pair collapses onto the storey-below base rather
        // than onto a spurious slipped-nib offset. `apply_collinear_suggestion`
        // rewrites edge B onto edge A, so the wall on the previous outline must
        // be edge A. We measure each side's distance to the nearest parallel
        // edge of the previous ring and put the closer one in the A slot.
        let prev_ring = self.previous_storey_ring();
        // A horizontal previous edge anchors a horizontal pair's y; a vertical
        // one anchors a vertical pair's x (geometry in `manip_2d_plan`).
        let dist_to_prev = |coord: f32, axis: &Axis| -> f32 {
            prev_ring
                .as_ref()
                .and_then(|ring| manip_2d_plan::nearest_parallel_edge_distance(ring, axis, coord))
                .unwrap_or(f32::MAX)
        };
        let suggestions: Vec<_> = self
            .document
            .floor_plan
            .suggest_collinear(min_sep, max_sep)
            .into_iter()
            .filter(|s| s.ring_a == newest_ring || s.ring_b == newest_ring)
            .map(|mut s| {
                if dist_to_prev(s.coord_b, &s.axis) < dist_to_prev(s.coord_a, &s.axis) {
                    std::mem::swap(&mut s.ring_a, &mut s.ring_b);
                    std::mem::swap(&mut s.edge_a, &mut s.edge_b);
                    std::mem::swap(&mut s.coord_a, &mut s.coord_b);
                }
                s
            })
            .collect();
        println!(
            "[new-storey] collinearity scan: plan diag {:.2}, sep range [{:.3}, {:.3}] → {} \
             near-collinear pair(s) to review",
            diag,
            min_sep,
            max_sep,
            suggestions.len()
        );
        if !suggestions.is_empty() {
            self.review = Some(CollinearityReview {
                suggestions,
                cursor: 0,
                rejections: Vec::new(),
            });
        }
    }
}
