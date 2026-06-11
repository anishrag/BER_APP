//! Roof-RIDGE line input: in roof-point mode a STROKE (instead of a tap)
//! draws an axis-aligned ridge line whose endpoints are the stroke's own
//! projected extent. The cross coordinate gets the ceiling-step line's snap +
//! position-prompt treatment ([`StoreyDoc::snap_ceiling_step_line`] /
//! [`StoreyDoc::ceiling_line_position_prompt`], reused); one typed height
//! places BOTH apexes at once (`Ber::add_roof_apex` ×2) — a level ridge. A
//! sloping ridge is entered via the two-point tap flow instead.
//!
//! Like the roof point, the ridge's durable record is `Storey::roof_apexes`
//! (a ridge is just two apexes grouped with the regions they shape —
//! `CeilingRegions::apex_groups` is the grouping authority); the shell owns
//! the in-flight stroke → position → height state machine and the
//! canonicalize step after assign. Outside undo —
//! phases/steps/ceiling-steps/roof-points parity.
use super::ceiling_steps::CeilingLineSnap;
use super::*;
use datatypes::ber::structural::RoofApex;
use document::LinePositionPrompt;

/// The in-flight roof-ridge input, advanced stage by stage: stroke →
/// ([`Position`] when un-snapped →) [`Height`] → assign. Session-only,
/// outside undo; cleared on storey switch, mode exit, and cancel. `extent`
/// is the stroke's projected span on the OTHER axis from `axis` — the ridge's
/// endpoints, taken as drawn; only the cross coordinate (`coord_m`) snaps or
/// gets the position question, so the extent never shifts.
#[derive(Debug, Clone)]
pub(super) enum PendingRoofRidge {
    /// The line's cross coordinate is being measured from an outline
    /// reference wall — the ceiling-step line prompt reused. Shell-resolved
    /// (`StoreyDoc::resolve_ceiling_line_position`); never enters the
    /// document's position-prompt loop. Implies UN-snapped: a snapped line
    /// skips this stage entirely.
    Position {
        axis: ParameterAxis,
        coord_m: f32,
        extent: (f32, f32),
        prompt: LinePositionPrompt,
    },
    /// Cross coordinate resolved; awaiting the ridge height (metres,
    /// ABSOLUTE above the storey floor datum — both endpoints get it).
    Height {
        axis: ParameterAxis,
        coord_m: f32,
        extent: (f32, f32),
        /// `Some` when the line snapped onto a parallel outline wall —
        /// `coord_m` is then the wall's coordinate and the position prompt
        /// was skipped; un-snapping reopens it.
        snap: Option<CeilingLineSnap>,
    },
}

impl AppController {
    /// UI entry point: a ridge stroke (world/metre coords) drawn in
    /// roof-point mode. Fits it to an axis-aligned segment
    /// ([`StoreyDoc::fit_roof_ridge_line`] — axis + cross coordinate + the
    /// stroke's own extent as the endpoints), tries the elective wall snap on
    /// the CROSS coordinate (snapped → straight to the height; the extent
    /// does NOT shift — it's on the other axis), and otherwise opens the
    /// position prompt (or auto-resolves, logged, when there's no parallel
    /// reference). A stroke that isn't line-like or lies outside the
    /// footprint is rejected (logged).
    pub fn apply_roof_ridge_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        if !self.roof_point_mode || self.roof_point_pending() || self.roof_ridge_pending() {
            return;
        }
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        // Paste-ready stroke dump (matches the ceiling-step / area dumps).
        dump_stroke("roof-ridge", &pts);
        // Flush the live buffer to its slot: the fit + snap read
        // `document.outline`, which is only synced on shape switches.
        self.sync_active();
        let (axis, coord_m, extent) = match self.document.fit_roof_ridge_line(&pts) {
            Ok(fit) => fit,
            Err(e) => {
                eprintln!("roof ridge: {}", e);
                return;
            }
        };
        match self.document.snap_ceiling_step_line(axis, coord_m) {
            Some((snapped, outline_wall_vid)) => {
                // Snapped: the coordinate IS a reference wall's, exact by
                // construction — no position question left to ask.
                self.pending_roof_ridge = Some(PendingRoofRidge::Height {
                    axis,
                    coord_m: snapped,
                    extent,
                    snap: Some(CeilingLineSnap {
                        drawn_coord_m: coord_m,
                        outline_wall_vid,
                    }),
                });
            }
            None => self.advance_roof_ridge(axis, coord_m, extent),
        }
    }

    /// Open the cross coordinate's position prompt, or — when the outline has
    /// no parallel wall to measure from — accept the drawn coordinate (the
    /// document logs the auto-resolve) and go straight to the height. The
    /// un-snapped path; a snapped line goes to [`PendingRoofRidge::Height`]
    /// directly with its snap riding along.
    fn advance_roof_ridge(&mut self, axis: ParameterAxis, coord_m: f32, extent: (f32, f32)) {
        self.pending_roof_ridge = Some(
            match self.document.ceiling_line_position_prompt(axis, coord_m) {
                Some(prompt) => PendingRoofRidge::Position {
                    axis,
                    coord_m,
                    extent,
                    prompt,
                },
                None => PendingRoofRidge::Height {
                    axis,
                    coord_m,
                    extent,
                    snap: None,
                },
            },
        );
    }

    /// The snap tag's "Don't snap": revert the pending ridge to its DRAWN
    /// cross coordinate, drop the snap, and REOPEN the skipped position
    /// question (or auto-resolve straight back to the height, logged).
    /// Deliberately NOT routed through the generic `choose_snap`/`unsnap_last`
    /// replay — those rewind history and re-run a stroke, but the pending
    /// ridge lives OUTSIDE history. No-op while un-snapped or nothing is
    /// pending.
    pub fn unsnap_roof_ridge(&mut self) {
        match self.pending_roof_ridge.take() {
            Some(PendingRoofRidge::Height {
                axis,
                extent,
                snap: Some(snap),
                ..
            }) => {
                log::debug!(
                    "roof ridge un-snapped from outline wall {:?} back to {:.3}; \
                     reopening the position question",
                    snap.outline_wall_vid,
                    snap.drawn_coord_m
                );
                self.advance_roof_ridge(axis, snap.drawn_coord_m, extent);
            }
            other => self.pending_roof_ridge = other, // not snapped — no-op
        }
    }

    /// The pending ridge's wall snap as ONE standard canvas snap tag, at the
    /// segment's midpoint. `options` is EMPTY and `skippable` is true — the
    /// tag's menu offers exactly "Don't snap", which the UI routes to
    /// [`unsnap_roof_ridge`](Self::unsnap_roof_ridge) (NOT the generic
    /// choose-snap replay — the ridge is outside history). `None` while the
    /// ridge isn't snapped or nothing is pending.
    pub fn roof_ridge_snap_tag(&self) -> Option<SnapTagView> {
        let (axis, snap) = match self.pending_roof_ridge.as_ref()? {
            PendingRoofRidge::Height {
                axis,
                snap: Some(snap),
                ..
            } => (*axis, *snap),
            _ => return None,
        };
        let ((ax, ay), (bx, by)) = self.pending_roof_ridge_line()?;
        let mid = (((ax + bx) / 2.0) as f32, ((ay + by) / 2.0) as f32);
        // The tag's `from` identity is the drawn point: the same midpoint at
        // the pre-snap coordinate (where the ridge would return on un-snap).
        let from = match axis {
            ParameterAxis::X => (snap.drawn_coord_m, mid.1),
            ParameterAxis::Y => (mid.0, snap.drawn_coord_m),
        };
        Some(SnapTagView {
            pos: mid,
            from,
            current_kind: Some(manip_2d_plan::SnapKind::ToParallelWall),
            options: Vec::new(),
            skippable: true,
        })
    }

    /// The two apex points the pending ridge would place, from its (axis,
    /// cross coordinate, extent): a vertical line (`axis` = X) has its
    /// endpoints at `(coord, lo)`/`(coord, hi)`; a horizontal one (`axis` =
    /// Y) at `(lo, coord)`/`(hi, coord)`.
    fn ridge_endpoints(
        axis: ParameterAxis,
        coord_m: f32,
        extent: (f32, f32),
    ) -> (Point2f64, Point2f64) {
        let (lo, hi) = extent;
        match axis {
            ParameterAxis::X => (
                Point2f64::new(coord_m as f64, lo as f64),
                Point2f64::new(coord_m as f64, hi as f64),
            ),
            ParameterAxis::Y => (
                Point2f64::new(lo as f64, coord_m as f64),
                Point2f64::new(hi as f64, coord_m as f64),
            ),
        }
    }

    /// Assign the ridge height to the pending line: materialize (the
    /// validation runs against the RESOLVED roofs), validate
    /// ([`document::validate_roof_ridge`] — both endpoints strictly inside
    /// the same un-apexed flat region, a height that actually slopes it, no
    /// boundary foldover), then stamp BOTH apexes as DURABLE input
    /// (`Ber::add_roof_apex` ×2 — quantization included), re-derive, and
    /// CANONICALIZE the session ceiling-step list from the derived roofs
    /// (the roof-point parity). The pending ridge stays open on rejection so
    /// the user can retype. Outside undo.
    pub fn assign_roof_ridge_height(&mut self, z_m: f32) -> Result<(), String> {
        let Some(PendingRoofRidge::Height {
            axis,
            coord_m,
            extent,
            ..
        }) = self.pending_roof_ridge.as_ref()
        else {
            return Ok(()); // nothing awaiting a height — no-op
        };
        let (a, b) = Self::ridge_endpoints(*axis, *coord_m, *extent);
        let idx = self.current_storey_idx.unwrap_or(0);
        // Bring the derived roofs current — the ridge is validated against
        // the resolved ceiling regions, not the editing buffer.
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to assign a roof ridge to")?;
        document::validate_roof_ridge(storey, a, b, z_m as f64)?;
        let record = self.current_record.as_mut().expect("checked above");
        for p in [a, b] {
            record.data.add_roof_apex(
                idx,
                RoofApex {
                    point: p,
                    z_m: z_m as f64,
                },
            );
        }
        self.pending_roof_ridge = None;
        self.mark_dirty()?;
        // Canonicalize: derive now, then rebuild the session ceiling-step
        // list from the derived roofs (group-aware — a ridge over a stepped
        // piece re-emits ONE merged step area). In-session state thus matches
        // what a reload would reconstruct — idempotence by construction.
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
        Ok(())
    }

    /// Discard the in-flight roof ridge at any stage.
    pub fn cancel_pending_roof_ridge(&mut self) {
        self.pending_roof_ridge = None;
    }

    /// True while the cross coordinate's position question is up (shows as
    /// the shared position panel, kind `Line`).
    pub fn roof_ridge_position_pending(&self) -> bool {
        matches!(
            self.pending_roof_ridge,
            Some(PendingRoofRidge::Position { .. })
        )
    }

    /// True while the resolved ridge awaits its height.
    pub fn roof_ridge_height_pending(&self) -> bool {
        matches!(
            self.pending_roof_ridge,
            Some(PendingRoofRidge::Height { .. })
        )
    }

    /// True while ANY roof-ridge input is in flight. The UI uses this to keep
    /// the toolbar reachable with the mode off and to suppress stroke/tap
    /// capture while a stage is pending.
    pub fn roof_ridge_pending(&self) -> bool {
        self.pending_roof_ridge.is_some()
    }

    /// The pending ridge's SEGMENT endpoints (world coords) for the UI's
    /// dashed overlay — the stroke's own extent, NOT a bbox-spanning line.
    /// `None` when nothing is pending.
    pub fn pending_roof_ridge_line(&self) -> Option<((f64, f64), (f64, f64))> {
        let (axis, coord_m, extent) = match self.pending_roof_ridge.as_ref()? {
            PendingRoofRidge::Position {
                axis,
                coord_m,
                extent,
                ..
            }
            | PendingRoofRidge::Height {
                axis,
                coord_m,
                extent,
                ..
            } => (*axis, *coord_m, *extent),
        };
        let (a, b) = Self::ridge_endpoints(axis, coord_m, extent);
        Some(((a.x, a.y), (b.x, b.y)))
    }

    /// The current storey's COMMITTED ridges as `(end_a, end_b, label)` in
    /// world coords for the UI's segment + label overlay: every apex group
    /// with exactly TWO apexes (the grouping authority,
    /// `CeilingRegions::apex_groups`, over the materialized record). Label
    /// "⌃ 4.20 m" for a level ridge, "4.20→4.50 m" when the two end heights
    /// differ (> 1e-6 — a sloping ridge from the two-point flow). The apex
    /// dots/labels themselves still come from `roof_apex_views`.
    pub fn roof_ridge_views(&self) -> Vec<((f64, f64), (f64, f64), String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return Vec::new();
        };
        storey
            .roofs
            .apex_groups(&storey.roof_apexes)
            .into_iter()
            .filter(|g| g.apex_idxs.len() == 2)
            .map(|g| {
                let a = &storey.roof_apexes[g.apex_idxs[0]];
                let b = &storey.roof_apexes[g.apex_idxs[1]];
                let label = if (a.z_m - b.z_m).abs() > 1e-6 {
                    format!("{:.2}→{:.2} m", a.z_m, b.z_m)
                } else {
                    format!("⌃ {:.2} m", a.z_m)
                };
                ((a.point.x, a.point.y), (b.point.x, b.point.y), label)
            })
            .collect()
    }
}
