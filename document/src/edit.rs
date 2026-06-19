//! Edit-mode model operations. The drag GEOMETRY (moving the selected vertex or
//! wall to follow the cursor) is pure model mutation and lives here. The rest of
//! edit mode — hit-testing setup, the post-drag / vertex-drop measurement prompts
//! and their solvers — is interaction bound to the controller's prompt types (an
//! intentional shell exception; see "Responsibility placement" in CLAUDE.md), and
//! the actual plan edits it performs are already `manip_plan` calls.
use crate::StoreyDoc;
use manip_plan::{ParameterAxis, VertexId};

impl StoreyDoc {
    /// Pin vertex `(ring, idx)` at an exact plan position: resolve its x and y
    /// parameters, set both, and register both as known dimensions — so the
    /// adjoining walls get length labels via the standard known-length path.
    /// The shell calls this after solving a vertex measurement (the one model
    /// mutation behind that flow). `Err` if either axis isn't a parameter (an
    /// anchored constant vertex can't be repositioned this way).
    pub fn pin_vertex_position(
        &mut self,
        ring: usize,
        idx: usize,
        new_pos: (f32, f32),
    ) -> Result<(), String> {
        let x_p = self
            .floor_plan
            .vertex_x_param(ring, idx)
            .ok_or_else(|| "vertex.x_expr isn't a parameter".to_string())?;
        let y_p = self
            .floor_plan
            .vertex_y_param(ring, idx)
            .ok_or_else(|| "vertex.y_expr isn't a parameter".to_string())?;
        self.floor_plan.set_parameter(x_p, new_pos.0);
        self.floor_plan.set_parameter(y_p, new_pos.1);
        self.dimensioning.known_params.insert(x_p);
        self.dimensioning.known_params.insert(y_p);
        Ok(())
    }

    /// Insert a new vertex on wall `(ring, edge_start)` at world `(x, y)` and
    /// set up the edit cursor for a free-2D drag on it. The shell has already
    /// taken the undo snapshot covering the whole insert + drag interaction, so
    /// `drag_snapshot_taken` is flagged to suppress a second one. Returns the
    /// new vertex's id, or `None` if the insert failed (cursor untouched).
    pub fn begin_vertex_insert_drag(
        &mut self,
        ring: usize,
        edge_start: usize,
        x: f32,
        y: f32,
    ) -> Option<VertexId> {
        let new_vid = self
            .floor_plan
            .insert_vertex_on_wall(ring, edge_start, x, y)?;
        self.edit_state.selected_wall = None;
        self.edit_state.selected_vertex = Some(new_vid);
        self.edit_state.dragging = true;
        self.edit_state.drag_snapshot_taken = true;
        self.edit_state.pre_drag_vertex_pos = Some((x, y));
        self.edit_state.pre_drag_value = None;
        self.edit_state.pre_drag_kind = None;
        Some(new_vid)
    }

    /// Drag the currently-selected vertex (free 2D) or wall (along its movable
    /// axis) to world `(x, y)`, mutating the live plan. Returns whether anything
    /// actually moved (the shell snapshots once per drag and persists on change).
    pub fn drag_active_to(&mut self, world_x: f64, world_y: f64) -> bool {
        // Vertex drag — free 2D.
        if let Some(vid) = self.edit_state.selected_vertex {
            if let Some((ring, idx)) = self.floor_plan.find_vertex(vid) {
                let x_param = self.floor_plan.vertex_x_param(ring, idx);
                let y_param = self.floor_plan.vertex_y_param(ring, idx);
                if let (Some(xp), Some(yp)) = (x_param, y_param) {
                    self.floor_plan.set_parameter(xp, world_x as f32);
                    self.floor_plan.set_parameter(yp, world_y as f32);
                    return true;
                }
            }
        } else if let Some(vid) = self.edit_state.selected_wall {
            // Wall drag — translate along its movable axis.
            if let Some((ring, edge_start)) = self.floor_plan.find_vertex(vid) {
                if let Some((param, axis)) = self.floor_plan.wall_movable_param(ring, edge_start) {
                    let new_value = match axis {
                        ParameterAxis::X => world_x as f32,
                        ParameterAxis::Y => world_y as f32,
                    };
                    self.floor_plan.set_parameter(param, new_value);
                    return true;
                } else if let Some((old_value, axis)) =
                    self.floor_plan.wall_anchor_constant(ring, edge_start)
                {
                    let new_value = match axis {
                        ParameterAxis::X => world_x as f32,
                        ParameterAxis::Y => world_y as f32,
                    };
                    self.floor_plan.shift_constant(old_value, axis, new_value);
                    return true;
                }
            }
        }
        false
    }
}
