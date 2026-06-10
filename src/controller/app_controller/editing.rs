//! Edit-mode interaction for `AppController`: wall/vertex selection,
//! dragging, and the post-drag / post-drop measurement prompts. Split out
//! of the controller core; see "Responsibility placement" in CLAUDE.md.
use super::*;
use manip_2d_plan::{intersect_circles, project_to_circle, solve_axis_wall};

impl AppController {
    /// VertexId of the wall currently selected in Edit mode, if any. The UI
    /// uses this to render the selection highlight.
    pub fn edit_selected_wall(&self) -> Option<VertexId> {
        self.document.edit_state.selected_wall
    }

    /// World-coord endpoints of the currently selected Edit-mode wall, or
    /// `None` if nothing is selected. Returned in the same order the
    /// polygon traverses (wall_a then wall_b).
    pub fn edit_selected_wall_positions(&self) -> Option<((f64, f64), (f64, f64))> {
        let vid = self.document.edit_state.selected_wall?;
        let (ring, edge_start) = self.document.floor_plan.find_vertex(vid)?;
        let polygons = self.document.floor_plan.polygons();
        let ring_pts = polygons.get(ring)?;
        let n = ring_pts.len();
        if n < 2 {
            return None;
        }
        let a = ring_pts.get(edge_start)?;
        let b = ring_pts.get((edge_start + 1) % n)?;
        Some(((a.x as f64, a.y as f64), (b.x as f64, b.y as f64)))
    }

    /// World-coord position of the currently selected Edit-mode vertex
    /// (from double-click insert), or `None`. Used by the UI to render
    /// a highlight marker on the inserted vertex.
    pub fn edit_selected_vertex_position(&self) -> Option<(f64, f64)> {
        let vid = self.document.edit_state.selected_vertex?;
        let (ring, idx) = self.document.floor_plan.find_vertex(vid)?;
        let polygons = self.document.floor_plan.polygons();
        let p = polygons.get(ring)?.get(idx)?;
        Some((p.x as f64, p.y as f64))
    }

    /// Edit-mode primary-button press. Hit-test the world-coord position
    /// against all walls; if any wall is within `tol`, select it and start
    /// a drag. Snapshots the pre-drag value of the wall's controlling
    /// coord so the post-drag prompt can revert on Cancel.
    pub fn edit_mouse_down(&mut self, world_x: f64, world_y: f64, tol: f64) -> bool {
        if !matches!(self.sketch_engine.edit_tool, EditTools::Edit) {
            return false;
        }
        let Some((ring, edge_start)) = self
            .document
            .floor_plan
            .hit_test_wall(world_x, world_y, tol)
        else {
            self.document.edit_state.selected_wall = None;
            self.document.edit_state.dragging = false;
            return false;
        };
        let Some(vid) = self.document.floor_plan.vertex_id_at(ring, edge_start) else {
            return false;
        };
        // Snapshot what kind of coord this wall is anchored to and its
        // current value, so a Cancel on the post-drag prompt can revert.
        if let Some((p, _axis)) = self
            .document
            .floor_plan
            .wall_movable_param(ring, edge_start)
        {
            self.document.edit_state.pre_drag_value = self.document.floor_plan.parameter_value(p);
            self.document.edit_state.pre_drag_kind = Some(MovedCoordKind::Parameter(p));
        } else if let Some((c, _axis)) = self
            .document
            .floor_plan
            .wall_anchor_constant(ring, edge_start)
        {
            self.document.edit_state.pre_drag_value = Some(c);
            self.document.edit_state.pre_drag_kind = Some(MovedCoordKind::Constant(c));
        } else {
            self.document.edit_state.pre_drag_value = None;
            self.document.edit_state.pre_drag_kind = None;
        }
        self.document.edit_state.selected_wall = Some(vid);
        self.document.edit_state.dragging = true;
        self.document.edit_state.drag_snapshot_taken = false;
        true
    }

    /// Edit-mode double-click: insert a new vertex on the wall closest to
    /// `(world_x, world_y)` (within `tol`), place it at the click point,
    /// and start a free-2D drag on it. Returns true if a vertex was
    /// inserted.
    pub fn edit_double_click(&mut self, world_x: f64, world_y: f64, tol: f64) -> bool {
        if !matches!(self.sketch_engine.edit_tool, EditTools::Edit) {
            return false;
        }
        let Some((ring, edge_start)) = self
            .document
            .floor_plan
            .hit_test_wall(world_x, world_y, tol)
        else {
            return false;
        };
        // Snapshot before inserting the vertex — the whole insert + drag
        // + prompt-apply sequence undoes as one action (the model flags the
        // drag snapshot as taken so edit_mouse_drag doesn't take a second one).
        self.record_history();
        let Some(new_vid) = self.document.begin_vertex_insert_drag(
            ring,
            edge_start,
            world_x as f32,
            world_y as f32,
        ) else {
            return false;
        };
        // Push the new topology to BER so the renderer sees the split
        // wall + new vertex immediately. Without this the polygon stays
        // at its pre-insert shape during the drag and only one edge
        // looks like it's following the cursor.
        if let Err(e) = self.mark_dirty() {
            eprintln!("commit_to_ber after insert_vertex_on_wall: {}", e);
        }
        println!(
            "vertex inserted: ring {}/edge {}+1 (vertex_id {}) at ({:.3}, {:.3})",
            ring, edge_start, new_vid, world_x, world_y
        );
        true
    }

    /// Edit-mode drag update. Dispatches based on what's selected:
    ///
    /// * If a `selected_vertex` is set (from a double-click insert),
    ///   updates both its x and y parameters to the cursor's world-coord
    ///   position — free 2D drag.
    /// * Otherwise translates the selected wall via `set_parameter` or
    ///   `shift_constant` (same as before).
    pub fn edit_mouse_drag(&mut self, world_x: f64, world_y: f64) {
        if !self.document.edit_state.dragging {
            return;
        }
        // Take a single undo snapshot at the very first mutation of the
        // current drag. Subsequent frames don't re-snapshot — the whole
        // drag undoes/redoes as one action.
        if !self.document.edit_state.drag_snapshot_taken {
            self.record_history();
            self.document.edit_state.drag_snapshot_taken = true;
        }
        // Push the live drag state to BER so the renderer sees the wall
        // / vertex follow the cursor instead of snapping only on release.
        if self.document.drag_active_to(world_x, world_y) {
            if let Err(e) = self.mark_dirty() {
                eprintln!("commit_to_ber during edit_mouse_drag: {}", e);
            }
        }
    }

    /// Edit-mode drag release. Routes by what was being dragged:
    ///
    /// * Vertex drag → for now, just commits at the rough drop position.
    ///   A post-drop measurement prompt (separate from the wall one) is
    ///   a follow-up.
    /// * Wall drag → if dimensioned, populate the post-drag prompt;
    ///   otherwise commit directly.
    pub fn edit_mouse_up(&mut self) -> Result<(), String> {
        if !self.document.edit_state.dragging {
            return Ok(());
        }
        self.document.edit_state.dragging = false;
        if self.document.edit_state.selected_vertex.is_some() {
            self.mark_dirty()?;
            if self.document.dimensioning.scale_set {
                self.populate_vertex_drop_prompt();
            }
            return Ok(());
        }
        if self.document.dimensioning.scale_set {
            self.populate_edit_drag_prompt();
        } else {
            self.mark_dirty()?;
        }
        Ok(())
    }

    /// Build the post-drop prompt for the just-inserted vertex: collect up
    /// to 4 candidate distances (V to prev × {X,Y}, V to next × {X,Y}),
    /// then de-duplicate axes where prev and next share that coord
    /// (yielding 3 distinct measurements for the typical case).
    fn populate_vertex_drop_prompt(&mut self) {
        let Some(vid) = self.document.edit_state.selected_vertex else {
            return;
        };
        let Some((ring, idx)) = self.document.floor_plan.find_vertex(vid) else {
            return;
        };
        let polygons = self.document.floor_plan.polygons();
        let Some(ring_pts) = polygons.get(ring) else {
            return;
        };
        let n = ring_pts.len();
        if n < 3 {
            return;
        }
        let v = ring_pts[idx];
        let prev_idx = (idx + n - 1) % n;
        let next_idx = (idx + 1) % n;
        let prev = ring_pts[prev_idx];
        let next = ring_pts[next_idx];

        let prev_vid = match self.document.floor_plan.vertex_id_at(ring, prev_idx) {
            Some(v) => v,
            None => return,
        };
        let next_vid = match self.document.floor_plan.vertex_id_at(ring, next_idx) {
            Some(v) => v,
            None => return,
        };

        let mut measurements: Vec<VertexMeasurement> = Vec::new();
        let tol = 1e-3_f32;
        // X-axis measurements. If prev.x == next.x (vertex inserted on a
        // vertical wall), one measurement covers both.
        let prev_dx = v.x - prev.x;
        let next_dx = v.x - next.x;
        if (prev.x - next.x).abs() < tol {
            if prev_dx.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::X),
                    neighbor_vid: prev_vid,
                    current_distance: prev_dx.abs(),
                    sign: prev_dx.signum(),
                });
            }
        } else {
            if prev_dx.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::X),
                    neighbor_vid: prev_vid,
                    current_distance: prev_dx.abs(),
                    sign: prev_dx.signum(),
                });
            }
            if next_dx.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::X),
                    neighbor_vid: next_vid,
                    current_distance: next_dx.abs(),
                    sign: next_dx.signum(),
                });
            }
        }
        // Y-axis: mirror of the above.
        let prev_dy = v.y - prev.y;
        let next_dy = v.y - next.y;
        if (prev.y - next.y).abs() < tol {
            if prev_dy.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::Y),
                    neighbor_vid: prev_vid,
                    current_distance: prev_dy.abs(),
                    sign: prev_dy.signum(),
                });
            }
        } else {
            if prev_dy.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::Y),
                    neighbor_vid: prev_vid,
                    current_distance: prev_dy.abs(),
                    sign: prev_dy.signum(),
                });
            }
            if next_dy.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::Y),
                    neighbor_vid: next_vid,
                    current_distance: next_dy.abs(),
                    sign: next_dy.signum(),
                });
            }
        }
        // Wall-length measurements: Euclidean distance to each neighbour.
        // The user may prefer measuring along a diagonal wall directly
        // (the new wall they just created by moving V off-axis).
        let prev_dist = ((v.x - prev.x).powi(2) + (v.y - prev.y).powi(2)).sqrt();
        if prev_dist > tol {
            measurements.push(VertexMeasurement {
                kind: VertexMeasurementKind::WallLength,
                neighbor_vid: prev_vid,
                current_distance: prev_dist,
                sign: 1.0,
            });
        }
        let next_dist = ((v.x - next.x).powi(2) + (v.y - next.y).powi(2)).sqrt();
        if next_dist > tol {
            measurements.push(VertexMeasurement {
                kind: VertexMeasurementKind::WallLength,
                neighbor_vid: next_vid,
                current_distance: next_dist,
                sign: 1.0,
            });
        }

        if measurements.is_empty() {
            return;
        }
        self.vertex_drop_prompt = Some(VertexDropPrompt {
            vertex_vid: vid,
            measurements,
            applied: Vec::new(),
        });
    }

    /// Apply one measurement from the vertex-drop prompt. The first apply
    /// snaps V to satisfy that one constraint; the second triggers a joint
    /// solve over both, then the prompt is cleared. Supports any mix of
    /// axis-aligned (linear) and wall-length (circular) measurements.
    pub fn apply_vertex_measurement(&mut self, idx: usize, distance: f32) -> Result<(), String> {
        if !distance.is_finite() || distance < 0.0 {
            return Err(format!(
                "distance must be a non-negative number (got {})",
                distance
            ));
        }
        let prompt = self
            .vertex_drop_prompt
            .clone()
            .ok_or_else(|| "no active vertex-drop prompt".to_string())?;
        let m = prompt
            .measurements
            .get(idx)
            .cloned()
            .ok_or_else(|| "invalid measurement index".to_string())?;
        let vertex_vid = prompt.vertex_vid;
        let (v_ring, v_idx) = self
            .document
            .floor_plan
            .find_vertex(vertex_vid)
            .ok_or_else(|| "vertex no longer exists".to_string())?;
        let polygons = self.document.floor_plan.polygons();
        let v_pos = polygons
            .get(v_ring)
            .and_then(|r| r.get(v_idx))
            .copied()
            .ok_or_else(|| "vertex position unavailable".to_string())?;
        let cur = (v_pos.x, v_pos.y);
        let n_pos = self.neighbor_pos(m.neighbor_vid)?;

        // First applied measurement → snap to its constraint alone.
        // Subsequent applies → joint-solve the latest prior + this one.
        let new_pos = if prompt.applied.is_empty() {
            snap_single(&m, distance, n_pos, cur)
        } else {
            let (prior_idx, prior_val) = *prompt.applied.last().unwrap();
            let prior_m = &prompt.measurements[prior_idx];
            let prior_n_pos = self.neighbor_pos(prior_m.neighbor_vid)?;
            joint_solve(prior_m, prior_val, prior_n_pos, &m, distance, n_pos, cur)
        };

        let x_p = self
            .document
            .floor_plan
            .vertex_x_param(v_ring, v_idx)
            .ok_or_else(|| "vertex.x_expr isn't a parameter".to_string())?;
        let y_p = self
            .document
            .floor_plan
            .vertex_y_param(v_ring, v_idx)
            .ok_or_else(|| "vertex.y_expr isn't a parameter".to_string())?;
        self.document.floor_plan.set_parameter(x_p, new_pos.0);
        self.document.floor_plan.set_parameter(y_p, new_pos.1);
        // Both params land in `known_params` so the new walls get length
        // labels via the standard `walls_with_known_length` path.
        self.document.dimensioning.known_params.insert(x_p);
        self.document.dimensioning.known_params.insert(y_p);
        self.mark_dirty()?;

        if let Some(p) = self.vertex_drop_prompt.as_mut() {
            p.applied.push((idx, distance));
            if p.applied.len() >= 2 {
                self.vertex_drop_prompt = None;
                self.document.edit_state.selected_vertex = None;
                self.document.edit_state.pre_drag_vertex_pos = None;
            }
        }
        Ok(())
    }

    /// World-coord position of a vertex by VertexId. Helper for the
    /// vertex-drop prompt's solve.
    fn neighbor_pos(&self, vid: VertexId) -> Result<(f32, f32), String> {
        let (r, i) = self
            .document
            .floor_plan
            .find_vertex(vid)
            .ok_or_else(|| "neighbour vertex no longer exists".to_string())?;
        let polygons = self.document.floor_plan.polygons();
        let p = polygons
            .get(r)
            .and_then(|ring| ring.get(i))
            .ok_or_else(|| "neighbour position unavailable".to_string())?;
        Ok((p.x, p.y))
    }

    /// Cancel the vertex-drop prompt. Reverts the vertex to its pre-drag
    /// position (the click point on the wall) and clears the prompt and
    /// selection. Pops the drag's undo snapshot for the same reason as
    /// `cancel_edit_drag`.
    pub fn cancel_vertex_drop(&mut self) -> Result<(), String> {
        if self.document.edit_state.drag_snapshot_taken {
            self.history.pop();
            self.document.edit_state.drag_snapshot_taken = false;
        }
        let Some(prompt) = self.vertex_drop_prompt.take() else {
            return Ok(());
        };
        let Some((wx, wy)) = self.document.edit_state.pre_drag_vertex_pos else {
            self.document.edit_state.selected_vertex = None;
            return Ok(());
        };
        if let Some((v_ring, v_idx)) = self.document.floor_plan.find_vertex(prompt.vertex_vid) {
            if let Some(xp) = self.document.floor_plan.vertex_x_param(v_ring, v_idx) {
                self.document.floor_plan.set_parameter(xp, wx);
            }
            if let Some(yp) = self.document.floor_plan.vertex_y_param(v_ring, v_idx) {
                self.document.floor_plan.set_parameter(yp, wy);
            }
        }
        self.document.edit_state.selected_vertex = None;
        self.document.edit_state.pre_drag_vertex_pos = None;
        self.mark_dirty()?;
        Ok(())
    }

    /// Snapshot of the vertex-drop prompt for the UI. Includes all
    /// measurements that haven't yet been applied (i.e. not in
    /// `prompt.applied`), each with a live-recomputed distance and the
    /// neighbour's current world position so the renderer can draw the
    /// matching arrow.
    pub fn vertex_drop_prompt_for_ui(&self) -> Option<VertexDropPromptUiData> {
        let prompt = self.vertex_drop_prompt.as_ref()?;
        let (v_ring, v_idx) = self.document.floor_plan.find_vertex(prompt.vertex_vid)?;
        let polygons = self.document.floor_plan.polygons();
        let v_ring_pts = polygons.get(v_ring)?;
        let v = v_ring_pts.get(v_idx)?;
        let applied_set: std::collections::HashSet<usize> =
            prompt.applied.iter().map(|(i, _)| *i).collect();
        let mut active: Vec<VertexMeasurementUiData> = Vec::new();
        for (i, m) in prompt.measurements.iter().enumerate() {
            if applied_set.contains(&i) {
                continue;
            }
            let (n_ring, n_idx) = self.document.floor_plan.find_vertex(m.neighbor_vid)?;
            let n = polygons.get(n_ring).and_then(|r| r.get(n_idx))?;
            let live = match m.kind {
                VertexMeasurementKind::Axis(ParameterAxis::X) => (v.x - n.x).abs(),
                VertexMeasurementKind::Axis(ParameterAxis::Y) => (v.y - n.y).abs(),
                VertexMeasurementKind::WallLength => {
                    ((v.x - n.x).powi(2) + (v.y - n.y).powi(2)).sqrt()
                }
            };
            active.push(VertexMeasurementUiData {
                idx: i,
                neighbor_pos: (n.x as f64, n.y as f64),
                kind: m.kind,
                current_distance: live,
            });
        }
        Some(VertexDropPromptUiData {
            vertex_pos: (v.x as f64, v.y as f64),
            active,
        })
    }

    /// Construct the post-drag prompt: list every other parallel wall as
    /// a candidate "measure distance from this reference" option, sorted
    /// by traversal order so the same wall consistently shows up first.
    fn populate_edit_drag_prompt(&mut self) {
        let Some(vid) = self.document.edit_state.selected_wall else {
            return;
        };
        let Some((ring, edge_start)) = self.document.floor_plan.find_vertex(vid) else {
            return;
        };
        let Some(kind) = self.document.edit_state.pre_drag_kind else {
            return;
        };
        let polygons = self.document.floor_plan.polygons();
        let Some(ring_pts) = polygons.get(ring) else {
            return;
        };
        let n = ring_pts.len();
        if n < 2 {
            return;
        }
        let a = ring_pts[edge_start];
        let b = ring_pts[(edge_start + 1) % n];
        // Axis-aligned wall: determine orientation. Tolerance against floats.
        let dx = (b.x - a.x).abs();
        let dy = (b.y - a.y).abs();
        let is_vertical = dy > dx;
        let axis = if is_vertical {
            ParameterAxis::X
        } else {
            ParameterAxis::Y
        };
        let moved_value = if is_vertical { a.x } else { a.y };

        let mut options: Vec<MeasurementOption> = Vec::new();
        for (ri, r) in polygons.iter().enumerate() {
            for i in 0..r.len() {
                if ri == ring && i == edge_start {
                    continue;
                }
                let p = r[i];
                let q = r[(i + 1) % r.len()];
                let pdx = (q.x - p.x).abs();
                let pdy = (q.y - p.y).abs();
                let other_is_vertical = pdy > pdx;
                if other_is_vertical != is_vertical {
                    continue;
                }
                let ref_value = if is_vertical { p.x } else { p.y };
                let delta = moved_value - ref_value;
                let distance = delta.abs();
                if distance < 1e-3 {
                    continue;
                }
                let Some(ref_vid) = self.document.floor_plan.vertex_id_at(ri, i) else {
                    continue;
                };
                options.push(MeasurementOption {
                    reference_wall_vid: ref_vid,
                    reference_value: ref_value,
                    sign: delta.signum(),
                    current_distance: distance,
                });
            }
        }
        if options.is_empty() {
            // Nothing parallel to the moved wall — can't form a measurement.
            return;
        }
        self.edit_drag_prompt = Some(EditDragPrompt {
            moved_wall_vid: vid,
            axis,
            kind,
            options,
            current_idx: 0,
        });
    }

    /// Apply the user's entered distance from the current measurement
    /// reference. Updates the moved wall's parameter (or shifts its
    /// anchor constant) and clears the prompt.
    pub fn apply_edit_drag(&mut self, distance: f32) -> Result<(), String> {
        if !distance.is_finite() || distance < 0.0 {
            return Err(format!(
                "distance must be a non-negative number (got {})",
                distance
            ));
        }
        let prompt = self
            .edit_drag_prompt
            .clone()
            .ok_or_else(|| "no active edit-drag prompt".to_string())?;
        let Some(option) = prompt.options.get(prompt.current_idx).cloned() else {
            return Err("invalid measurement index".into());
        };
        let new_value = option.reference_value + option.sign * distance;
        match prompt.kind {
            MovedCoordKind::Parameter(p) => {
                self.document.floor_plan.set_parameter(p, new_value);
            }
            MovedCoordKind::Constant(old_value) => {
                self.document
                    .floor_plan
                    .shift_constant(old_value, prompt.axis, new_value);
            }
        }
        self.mark_dirty()?;
        self.edit_drag_prompt = None;
        self.document.edit_state.pre_drag_value = None;
        self.document.edit_state.pre_drag_kind = None;
        Ok(())
    }

    /// Advance the prompt to the next candidate measurement. Wraps around
    /// when the user reaches the end of the list. No-op if the prompt
    /// isn't active.
    pub fn cycle_edit_drag_measurement(&mut self) {
        if let Some(p) = self.edit_drag_prompt.as_mut() {
            if p.options.is_empty() {
                return;
            }
            p.current_idx = (p.current_idx + 1) % p.options.len();
        }
    }

    /// Cancel the post-drag prompt: revert the moved coord to its pre-drag
    /// value and clear the prompt. Pops the drag's undo snapshot because
    /// the action is effectively erased — no need for the user to undo
    /// after they cancelled.
    pub fn cancel_edit_drag(&mut self) -> Result<(), String> {
        if self.document.edit_state.drag_snapshot_taken {
            self.history.pop();
            self.document.edit_state.drag_snapshot_taken = false;
        }
        let (Some(pre_value), Some(kind)) = (
            self.document.edit_state.pre_drag_value,
            self.document.edit_state.pre_drag_kind,
        ) else {
            self.edit_drag_prompt = None;
            return Ok(());
        };
        let prompt_axis = self.edit_drag_prompt.as_ref().map(|p| p.axis);
        match kind {
            MovedCoordKind::Parameter(p) => {
                self.document.floor_plan.set_parameter(p, pre_value);
            }
            MovedCoordKind::Constant(_old_value) => {
                if let Some(axis) = prompt_axis {
                    // The current constant value may have been mutated by
                    // the drag; look it up from the moved wall to find the
                    // *current* constant, then shift back to pre_value.
                    if let Some(vid) = self.document.edit_state.selected_wall {
                        if let Some((ring, edge_start)) = self.document.floor_plan.find_vertex(vid)
                        {
                            if let Some((current_const, _)) = self
                                .document
                                .floor_plan
                                .wall_anchor_constant(ring, edge_start)
                            {
                                self.document.floor_plan.shift_constant(
                                    current_const,
                                    axis,
                                    pre_value,
                                );
                            }
                        }
                    }
                }
            }
        }
        self.mark_dirty()?;
        self.edit_drag_prompt = None;
        self.document.edit_state.pre_drag_value = None;
        self.document.edit_state.pre_drag_kind = None;
        Ok(())
    }

    /// Snapshot of the active post-drag prompt for the UI to render. Includes
    /// the moved wall + the current reference wall + the distance value to
    /// pre-populate the textbox with.
    pub fn edit_drag_prompt_for_ui(&self) -> Option<EditDragPromptUiData> {
        let prompt = self.edit_drag_prompt.as_ref()?;
        let option = prompt.options.get(prompt.current_idx)?;
        let (moved_ring, moved_edge) = self
            .document
            .floor_plan
            .find_vertex(prompt.moved_wall_vid)?;
        let (ref_ring, ref_edge) = self
            .document
            .floor_plan
            .find_vertex(option.reference_wall_vid)?;
        let polygons = self.document.floor_plan.polygons();
        let moved_ring_pts = polygons.get(moved_ring)?;
        let ref_ring_pts = polygons.get(ref_ring)?;
        let moved_a = moved_ring_pts.get(moved_edge)?;
        let moved_b = moved_ring_pts.get((moved_edge + 1) % moved_ring_pts.len())?;
        let ref_a = ref_ring_pts.get(ref_edge)?;
        let ref_b = ref_ring_pts.get((ref_edge + 1) % ref_ring_pts.len())?;
        Some(EditDragPromptUiData {
            moved_wall: WallPositions {
                wall_a: (moved_a.x as f64, moved_a.y as f64),
                wall_b: (moved_b.x as f64, moved_b.y as f64),
            },
            reference_wall: WallPositions {
                wall_a: (ref_a.x as f64, ref_a.y as f64),
                wall_b: (ref_b.x as f64, ref_b.y as f64),
            },
            current_distance: option.current_distance,
            option_idx: prompt.current_idx,
            option_count: prompt.options.len(),
        })
    }
}

/// Snap V to satisfy a single measurement. The other coord defaults to the
/// current vertex position (axis case) or is computed from the circle's
/// closest-point projection (wall-length case).
fn snap_single(
    m: &VertexMeasurement,
    distance: f32,
    n_pos: (f32, f32),
    cur: (f32, f32),
) -> (f32, f32) {
    match m.kind {
        VertexMeasurementKind::Axis(ParameterAxis::X) => (n_pos.0 + m.sign * distance, cur.1),
        VertexMeasurementKind::Axis(ParameterAxis::Y) => (cur.0, n_pos.1 + m.sign * distance),
        VertexMeasurementKind::WallLength => project_to_circle(cur, n_pos, distance),
    }
}

/// Solve for V given two measurements jointly. Handles every combination
/// of axis / wall-length constraints. Falls back to overriding the prior
/// constraint when the two are linearly dependent (e.g. two X axes) or
/// when the system is infeasible (circles don't reach a fixed coord).
fn joint_solve(
    m1: &VertexMeasurement,
    d1: f32,
    n1: (f32, f32),
    m2: &VertexMeasurement,
    d2: f32,
    n2: (f32, f32),
    cur: (f32, f32),
) -> (f32, f32) {
    use VertexMeasurementKind::*;
    match (m1.kind, m2.kind) {
        (Axis(ParameterAxis::X), Axis(ParameterAxis::Y)) => {
            (n1.0 + m1.sign * d1, n2.1 + m2.sign * d2)
        }
        (Axis(ParameterAxis::Y), Axis(ParameterAxis::X)) => {
            (n2.0 + m2.sign * d2, n1.1 + m1.sign * d1)
        }
        (Axis(a1), Axis(a2)) if std::mem::discriminant(&a1) == std::mem::discriminant(&a2) => {
            // Same axis applied twice — second overrides, other coord stays.
            snap_single(m2, d2, n2, cur)
        }
        (Axis(_), Axis(_)) => snap_single(m2, d2, n2, cur),
        (Axis(axis), WallLength) => solve_axis_wall(axis, m1.sign, d1, n1, n2, d2, cur),
        (WallLength, Axis(axis)) => solve_axis_wall(axis, m2.sign, d2, n2, n1, d1, cur),
        (WallLength, WallLength) => intersect_circles(n1, d1, n2, d2, cur),
    }
}

/// World-coord endpoints of one wall.
#[derive(Debug, Clone, Copy)]
pub struct WallPositions {
    pub wall_a: (f64, f64),
    pub wall_b: (f64, f64),
}

/// UI-friendly view of the post-drag measurement prompt: the wall the user
/// just moved + the wall they're measuring distance *from* + the current
/// distance value + which option is selected out of how many.
#[derive(Debug, Clone, Copy)]
pub struct EditDragPromptUiData {
    pub moved_wall: WallPositions,
    pub reference_wall: WallPositions,
    pub current_distance: f32,
    pub option_idx: usize,
    pub option_count: usize,
}

/// What kind of geometric constraint a vertex measurement represents.
/// Axis = linear (|V.x - N.x| or |V.y - N.y|), pins one coord at a time.
/// WallLength = euclidean |V - N|, a circle constraint that needs another
/// measurement for full pinning (circle intersection / substitution).
#[derive(Debug, Clone, Copy)]
pub enum VertexMeasurementKind {
    Axis(ParameterAxis),
    WallLength,
}

/// One candidate measurement for the just-dropped vertex.
#[derive(Debug, Clone)]
pub struct VertexMeasurement {
    pub kind: VertexMeasurementKind,
    /// Neighbour vertex this measurement is from.
    pub neighbor_vid: VertexId,
    /// Live distance: axis → `|V.coord - N.coord|`, wall → `|V - N|`.
    pub current_distance: f32,
    /// For axis measurements: sign of `(V.coord - N.coord)` at populate
    /// time. Preserved on apply so V ends up on the same side of N.
    /// Unused for wall-length measurements (left as 1.0).
    pub sign: f32,
}

/// Active post-vertex-drop prompt. After releasing a newly-inserted
/// vertex, the user is shown text boxes for each relevant neighbour
/// distance and fills them in to pin the vertex precisely.
#[derive(Debug, Clone)]
pub struct VertexDropPrompt {
    /// The newly-inserted vertex.
    pub vertex_vid: VertexId,
    /// Candidate measurements — up to 5 typically (3 H/V de-duped + 2
    /// wall-lengths to prev and next).
    pub measurements: Vec<VertexMeasurement>,
    /// Applied measurements (index into `measurements`, entered value)
    /// in chronological order. The prompt is cleared once 2 are applied
    /// — at that point the joint solve has fully pinned V's position.
    pub applied: Vec<(usize, f32)>,
}

/// UI-friendly snapshot of a vertex-drop prompt. Holds vertex/neighbour
/// world coords so the renderer can draw an arrow per active measurement
/// and the UI can label each text box.
#[derive(Debug, Clone)]
pub struct VertexDropPromptUiData {
    /// The newly-inserted vertex's world position.
    pub vertex_pos: (f64, f64),
    /// One entry per still-active measurement (axis not yet pinned).
    pub active: Vec<VertexMeasurementUiData>,
}

#[derive(Debug, Clone, Copy)]
pub struct VertexMeasurementUiData {
    /// Original index in `VertexDropPrompt.measurements` — used by Apply
    /// to know which measurement the user is responding to.
    pub idx: usize,
    /// World position of the neighbour.
    pub neighbor_pos: (f64, f64),
    /// Kind of constraint this measurement represents.
    pub kind: VertexMeasurementKind,
    /// Live distance.
    pub current_distance: f32,
}

/// One candidate measurement for the post-drag prompt: "distance from this
/// other wall to the moved one." The user can cycle through these to find
/// one they can practically measure with a tape.
#[derive(Debug, Clone)]
pub struct MeasurementOption {
    /// Reference wall's start-vertex id.
    pub reference_wall_vid: VertexId,
    /// Reference's perpendicular coord (the world-coord value the moved
    /// wall is measured *from*).
    pub reference_value: f32,
    /// Sign of `(moved_value - reference_value)` at the time the prompt
    /// was constructed. Preserved when the user enters a distance so the
    /// moved wall stays on the same side of the reference as it was after
    /// the drag.
    pub sign: f32,
    /// Current `|moved_value - reference_value|` (post-drag). Shown to the
    /// user as the "current" measurement before they edit it.
    pub current_distance: f32,
}

#[derive(Debug, Clone)]
pub struct EditDragPrompt {
    /// VertexId of the moved wall.
    pub moved_wall_vid: VertexId,
    /// Perpendicular axis the wall moves along.
    pub axis: ParameterAxis,
    /// Whether the moved coord is a Parameter or a Constant.
    pub kind: MovedCoordKind,
    /// Candidate measurements; the user cycles through them.
    pub options: Vec<MeasurementOption>,
    /// Index of the option currently being asked about.
    pub current_idx: usize,
}
