//! Edit-mode model operations. The drag GEOMETRY (moving the selected vertex or
//! wall to follow the cursor) is pure model mutation and lives here. The rest of
//! edit mode — hit-testing setup, the post-drag / vertex-drop measurement prompts
//! and their solvers — is interaction bound to the controller's prompt types (an
//! intentional shell exception; see "Responsibility placement" in CLAUDE.md), and
//! the actual plan edits it performs are already `manip_2d_plan` calls.
use crate::Document;
use manip_2d_plan::ParameterAxis;

impl Document {
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
