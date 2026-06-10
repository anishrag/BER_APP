//! Ceiling-step tagging: the assessor draws an AXIS-ALIGNED LINE across the
//! storey plan (a near-straight stroke, or a tap + an H/V choice), taps the
//! side of the line where the ceiling steps, optionally pins the line's exact
//! coordinate via a position prompt, then enters the step amount (metres,
//! relative to the storey's default ceiling height; + raised, − lowered).
//!
//! Like floor steps, a ceiling step is NOT persisted as a record of its own.
//! It's session-only controller state whose job is to re-split the resolved
//! ceiling regions at commit (`CeilingRegions::with_z_deltas`, applied in
//! `document::resolve_storey`): the durable record is the per-vertex z on the
//! roof regions the split produces, and on reload the session regions are
//! rebuilt from those (`document::ceiling_step_regions_from_roofs`).
//!
//! The line (axis + coordinate + side) is input UX only — the session record
//! is the REGION. After each assign the shell CANONICALIZES: materialize, then
//! rebuild the session list from the derived roofs, so the in-session list
//! equals the post-reload reconstruction (idempotent reload by construction).
use super::*;
use document::{LinePositionPrompt, LineSide};

/// The in-flight ceiling-step input, advanced stage by stage:
/// tap → [`Orientation`] → H/V → [`Side`] → side tap → ([`Position`] →)
/// [`Amount`] → assign. A stroke skips straight to [`Side`] (the fit decides
/// the orientation). Session-only, outside undo; cleared on storey switch,
/// mode exit, and cancel.
#[derive(Debug, Clone)]
pub(super) enum PendingCeilingStep {
    /// A point was tapped; awaiting the Horizontal/Vertical choice.
    Orientation { point: Point2f32 },
    /// The line is fixed; awaiting the side tap. `axis` is the axis the
    /// line's POSITION varies on (X for a vertical line).
    Side { axis: ParameterAxis, coord_m: f32 },
    /// Side picked; the line's coordinate is being measured from an outline
    /// reference wall. Shell-resolved — this prompt never enters the
    /// document's position-prompt loop (there's no shape behind a line).
    Position {
        axis: ParameterAxis,
        coord_m: f32,
        side: LineSide,
        prompt: LinePositionPrompt,
    },
    /// Awaiting the step amount (the UI's Up/Down buttons own the sign).
    Amount {
        axis: ParameterAxis,
        coord_m: f32,
        side: LineSide,
    },
}

impl AppController {
    /// True when the ceiling-step tool can be used: a storey is being edited
    /// (false on the All tab), its outline is finalised (metric + fully
    /// dimensioned — the line must land in metre space), and its default
    /// ceiling height is set (the deltas are relative to it). Drives the UI
    /// toggle's enablement AND guards the setter (gate double-enforced).
    pub fn ceiling_step_available(&self) -> bool {
        let Some(idx) = self.current_storey_idx else {
            return false;
        };
        let height_set = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .is_some_and(|s| s.ceiling_height_m.is_some());
        height_set && self.outline_finalised()
    }

    /// True when the editor is tagging ceiling steps (drives the UI toggle
    /// and routes canvas strokes/taps to the line input).
    pub fn ceiling_step_mode(&self) -> bool {
        self.ceiling_step_mode
    }

    /// Enter/leave ceiling-step mode. Entering is a NO-OP while the tool is
    /// unavailable (see [`ceiling_step_available`](Self::ceiling_step_available));
    /// otherwise it drops into draw mode (a stroke draws the line straight
    /// away), forces the OUTLINE active (so no carved-area position prompt can
    /// coexist with the line's), and is mutually exclusive with living-area /
    /// exposure / phase / floor-step modes. Leaving clears any half-finished
    /// line.
    pub fn set_ceiling_step_mode(&mut self, on: bool) {
        if on && !self.ceiling_step_available() {
            return;
        }
        self.ceiling_step_mode = on;
        self.pending_ceiling_step = None;
        if on {
            self.living_area_mode = false;
            self.exposure_mode = false;
            self.phase_mode = false;
            self.step_mode = false;
            // A surviving cut-pick would fire on the SAME tap as the ceiling
            // line flow (its choose_* methods have no mode guard), grafting an
            // in-flight phase/step shape under the line — clear them all.
            self.pending_cut = None;
            self.pending_phase_cut = None;
            self.pending_step_cut = None;
            self.activate_shape(ActiveShape::Outline);
            self.set_edit_mode(EditTools::DrawPerpWalls);
        }
    }

    /// UI entry point: a ceiling-step stroke in world (metre) coords. Fits it
    /// to an axis-aligned line ([`StoreyDoc::fit_ceiling_step_line`]) and, on
    /// success, advances straight to the side pick. A stroke that isn't
    /// line-like or doesn't cross the footprint is rejected (logged).
    pub fn apply_ceiling_step_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        // Paste-ready stroke dump (matches the living-area / step-area dumps).
        dump_stroke("ceiling-step", &pts);
        // Flush the live buffer to its slot: the fit validates against
        // `document.outline`, which is only synced on shape switches.
        self.sync_active();
        match self.document.fit_ceiling_step_line(&pts) {
            Ok((axis, coord_m)) => {
                self.pending_ceiling_step = Some(PendingCeilingStep::Side { axis, coord_m });
            }
            Err(e) => eprintln!("ceiling-step line: {}", e),
        }
    }

    /// Canvas tap dispatch for the ceiling-step flow:
    /// - nothing pending (mode on) → the tap places the line's point; the H/V
    ///   picker chooses its orientation next.
    /// - side pending → `sign(tap − coord)` on the line's axis picks the side
    ///   of the line the delta applies to, then the position prompt opens (or
    ///   auto-resolves, logged, when there's no parallel reference wall).
    /// - any other stage → ignored (those are toolbar/prompt-driven).
    pub fn ceiling_step_tap(&mut self, world_x: f32, world_y: f32) {
        match self.pending_ceiling_step.as_ref() {
            None if self.ceiling_step_mode => {
                self.pending_ceiling_step = Some(PendingCeilingStep::Orientation {
                    point: Point2f32::new(world_x, world_y),
                });
            }
            Some(PendingCeilingStep::Side { axis, coord_m }) => {
                let (axis, coord_m) = (*axis, *coord_m);
                let tap = match axis {
                    ParameterAxis::X => world_x,
                    ParameterAxis::Y => world_y,
                };
                let side = if tap >= coord_m {
                    LineSide::Greater
                } else {
                    LineSide::Less
                };
                self.advance_past_side(axis, coord_m, side);
            }
            _ => {}
        }
    }

    /// Side chosen: open the position prompt for the line's coordinate, or —
    /// when the outline has no parallel wall to measure from — accept the
    /// drawn coordinate (the document logs the auto-resolve) and go straight
    /// to the amount.
    fn advance_past_side(&mut self, axis: ParameterAxis, coord_m: f32, side: LineSide) {
        self.pending_ceiling_step = Some(
            match self.document.ceiling_line_position_prompt(axis, coord_m) {
                Some(prompt) => PendingCeilingStep::Position {
                    axis,
                    coord_m,
                    side,
                    prompt,
                },
                None => PendingCeilingStep::Amount {
                    axis,
                    coord_m,
                    side,
                },
            },
        );
    }

    /// H/V picker for a TAPPED line point: a `horizontal` line's position
    /// varies on Y (and a vertical one's on X). Validates the resulting line
    /// crosses the footprint; on rejection the tapped point stays pending so
    /// the user can pick the other orientation or cancel.
    pub fn choose_ceiling_step_orientation(&mut self, horizontal: bool) {
        let Some(PendingCeilingStep::Orientation { point }) = self.pending_ceiling_step.as_ref()
        else {
            return;
        };
        let (axis, coord_m) = if horizontal {
            (ParameterAxis::Y, point.y)
        } else {
            (ParameterAxis::X, point.x)
        };
        match self.document.validate_ceiling_step_line(axis, coord_m) {
            Ok(()) => {
                self.pending_ceiling_step = Some(PendingCeilingStep::Side { axis, coord_m });
            }
            Err(e) => eprintln!("ceiling-step line: {}", e),
        }
    }

    /// True while a tapped point awaits its H/V orientation choice.
    pub fn ceiling_step_orientation_pending(&self) -> bool {
        matches!(
            self.pending_ceiling_step,
            Some(PendingCeilingStep::Orientation { .. })
        )
    }

    /// True while the drawn line awaits its side tap.
    pub fn ceiling_step_side_pending(&self) -> bool {
        matches!(
            self.pending_ceiling_step,
            Some(PendingCeilingStep::Side { .. })
        )
    }

    /// True while the resolved line awaits its step amount.
    pub fn ceiling_step_amount_pending(&self) -> bool {
        matches!(
            self.pending_ceiling_step,
            Some(PendingCeilingStep::Amount { .. })
        )
    }

    /// True while ANY ceiling-step input is in flight (including the position
    /// stage, which shows as the shared position panel). The UI uses this to
    /// keep the toolbar reachable with the mode off and to suppress stroke
    /// capture while a stage is pending.
    pub fn ceiling_step_pending(&self) -> bool {
        self.pending_ceiling_step.is_some()
    }

    /// The pending step line's endpoints (world coords), spanning the outline
    /// bbox, for the UI's dashed-line overlay. `None` before the orientation
    /// is chosen (a point has no line yet) and when nothing is pending.
    pub fn pending_ceiling_step_line(&self) -> Option<((f64, f64), (f64, f64))> {
        let (axis, coord_m) = match self.pending_ceiling_step.as_ref()? {
            PendingCeilingStep::Orientation { .. } => return None,
            PendingCeilingStep::Side { axis, coord_m }
            | PendingCeilingStep::Position { axis, coord_m, .. }
            | PendingCeilingStep::Amount { axis, coord_m, .. } => (*axis, *coord_m),
        };
        let ((min_x, min_y), (max_x, max_y)) = self.document.outline.floor_plan.bounding_box()?;
        Some(match axis {
            ParameterAxis::X => (
                (coord_m as f64, min_y as f64),
                (coord_m as f64, max_y as f64),
            ),
            ParameterAxis::Y => (
                (min_x as f64, coord_m as f64),
                (max_x as f64, coord_m as f64),
            ),
        })
    }

    /// The two bbox halves either side of the pending line (world coords),
    /// for the UI to tint while the user taps the side that steps — the
    /// cut-pick pattern. Empty when the side pick isn't pending.
    pub fn pending_ceiling_step_side_rects(&self) -> Vec<Vec<(f64, f64)>> {
        let Some(PendingCeilingStep::Side { axis, coord_m }) = self.pending_ceiling_step.as_ref()
        else {
            return Vec::new();
        };
        let (axis, coord) = (*axis, *coord_m as f64);
        let Some(((min_x, min_y), (max_x, max_y))) =
            self.document.outline.floor_plan.bounding_box()
        else {
            return Vec::new();
        };
        let (x0, y0, x1, y1) = (min_x as f64, min_y as f64, max_x as f64, max_y as f64);
        let rect =
            |ax: f64, ay: f64, bx: f64, by: f64| vec![(ax, ay), (bx, ay), (bx, by), (ax, by)];
        match axis {
            ParameterAxis::X => vec![rect(x0, y0, coord, y1), rect(coord, y0, x1, y1)],
            ParameterAxis::Y => vec![rect(x0, y0, x1, coord), rect(x0, coord, x1, y1)],
        }
    }

    /// Assign the step amount to the pending line: rejects a near-zero step
    /// (|Δ| < 0.01 m — floor-step parity) and a drop at or past the default
    /// height (the ceiling would reach the floor); the pending line stays
    /// open on rejection so the user can retype. On success: push the
    /// half-rect region (the resolve projection clips it), re-derive, then
    /// CANONICALIZE the session list from the derived roofs so it equals the
    /// post-reload reconstruction. Outside undo — phases/steps parity.
    pub fn assign_ceiling_step_delta(&mut self, delta_m: f32) -> Result<(), String> {
        let Some(PendingCeilingStep::Amount {
            axis,
            coord_m,
            side,
        }) = self.pending_ceiling_step.as_ref()
        else {
            return Ok(()); // nothing awaiting an amount — no-op
        };
        let (axis, coord_m, side) = (*axis, *coord_m, *side);
        if !delta_m.is_finite() || delta_m.abs() < 0.01 {
            return Err("Ceiling step must be at least 0.01 m.".into());
        }
        let idx = self.current_storey_idx.unwrap_or(0);
        let default = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|s| s.ceiling_height_m)
            .ok_or("Set the storey's default ceiling height first.")?;
        if delta_m as f64 <= -default {
            return Err(format!(
                "A {:.2} m drop would put the ceiling at or below the floor \
                 (default height {:.2} m).",
                -delta_m, default
            ));
        }
        let region = self
            .document
            .ceiling_step_region(axis, coord_m, side, delta_m as f64)
            .ok_or("The storey has no outline to step.")?;
        self.ceiling_step_regions
            .entry(idx)
            .or_default()
            .push(region);
        self.pending_ceiling_step = None;
        self.mark_dirty()?;
        // Canonicalize: derive now, then make the session list the EXACT
        // derived pieces (delta = flat z − default). In-session state thus
        // matches what a reload would reconstruct — idempotence by construction.
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
        Ok(())
    }

    /// Discard the in-flight ceiling-step line at any stage.
    pub fn cancel_pending_ceiling_step(&mut self) {
        self.pending_ceiling_step = None;
    }

    /// The current storey's committed ceiling-step regions, as `(ring, label)`
    /// in world coords, for the UI to tint and label (label e.g. "+0.20 m" /
    /// "−0.30 m"). Index-aligned with deletion (see
    /// [`delete_ceiling_step_region`](Self::delete_ceiling_step_region)).
    pub fn ceiling_step_region_polygons(&self) -> Vec<(Vec<(f64, f64)>, String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        self.ceiling_step_regions
            .get(&idx)
            .map(|regions| {
                regions
                    .iter()
                    .map(|r| {
                        let ring = r.polygon.iter().map(|p| (p.x as f64, p.y as f64)).collect();
                        (ring, format!("{:+.2} m", r.delta_m))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Delete the current storey's ceiling-step region at `index` and
    /// re-commit (the roof pieces it had stepped re-flatten to the default
    /// height on the next derive). No-op if out of range.
    pub fn delete_ceiling_step_region(&mut self, index: usize) {
        let idx = self.current_storey_idx.unwrap_or(0);
        let removed = self.ceiling_step_regions.get_mut(&idx).is_some_and(|rs| {
            if index < rs.len() {
                rs.remove(index);
                true
            } else {
                false
            }
        });
        if removed {
            let _ = self.mark_dirty();
        }
    }
}
