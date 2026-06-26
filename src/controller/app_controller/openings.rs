//! Windows/doors tool: the app-shell workflow over `ber_model::ber::openings`.
//! Thin — picks the host wall/roof from a plan tap, prompts size + type, calls
//! the pure ops against `current_record.data.storeys[idx]`, snapshots for undo,
//! and derives the UI-facing view data. All geometry/model mutation lives in the
//! ber_model ops; this only sequences them.
use super::*;
use ber_model::ber::common::ItemId;
use ber_model::ber::fenestration::{DoorKind, Frame, Glazing};
use ber_model::ber::openings::{self, DoorSidelightsSpec, OpeningError, OpeningRef};

/// Which opening the toolbar places next (or `Edit` to open an elevation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningToolKind {
    Window,
    /// A window that may span UP past its host storey's ceiling, as far as the
    /// same-type (construction + exposure) vertically-aligned wall run allows.
    /// Placed on the LOWER storey; the roofline top-clamp is lifted.
    MultiStoreyWindow,
    RoofWindow,
    Door,
    DoorSidelights,
    /// Tap a wall to open its elevation editor (reposition existing openings).
    Edit,
}

/// The host a pending placement landed on (awaiting size + type entry).
#[derive(Clone, Copy)]
pub(super) enum PendingOpeningTarget {
    Wall { wall_id: ItemId, along_m: f32 },
    Roof { region_id: ItemId, plan: (f32, f32) },
}

#[derive(Clone, Copy)]
pub(super) struct PendingOpening {
    pub tool: OpeningToolKind,
    pub target: PendingOpeningTarget,
}

/// One numeric measurement collected, one at a time, in the floating number box
/// during a staged placement (keypad / laser). Ordered per tool by
/// [`OpeningDimFlow::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningDimField {
    Width,
    Height,
    Sill,
    DoorWidth,
    DoorHeight,
    LeftLight,
    RightLight,
    Transom,
}

impl OpeningDimField {
    fn label(self) -> &'static str {
        match self {
            Self::Width => "width (m)",
            Self::Height => "height (m)",
            Self::Sill => "sill above floor (m)",
            Self::DoorWidth => "door width (m)",
            Self::DoorHeight => "door height (m)",
            Self::LeftLight => "left sidelight width (m)",
            Self::RightLight => "right sidelight width (m)",
            Self::Transom => "transom height (m)",
        }
    }
    /// Optional fields offer a Skip — the door+sidelights flanking/top lights.
    fn optional(self) -> bool {
        matches!(self, Self::LeftLight | Self::RightLight | Self::Transom)
    }
    fn hint(self) -> &'static str {
        match self {
            Self::Width | Self::DoorWidth => "e.g. 1.2",
            Self::Height | Self::DoorHeight => "e.g. 1.4",
            Self::Sill => "e.g. 0.9",
            Self::LeftLight | Self::RightLight | Self::Transom => "e.g. 0.4",
        }
    }
}

/// The staged numeric-entry flow for one in-flight placement: the picked host +
/// tool, the ordered fields still to collect, and the values gathered so far.
/// Each field is entered in the floating number box; on the last one the opening
/// is placed. Skipped optional fields hold `None`.
pub(super) struct OpeningDimFlow {
    tool: OpeningToolKind,
    target: PendingOpeningTarget,
    fields: Vec<OpeningDimField>,
    idx: usize,
    values: Vec<Option<f32>>,
}

impl OpeningDimFlow {
    fn new(tool: OpeningToolKind, target: PendingOpeningTarget) -> Self {
        use OpeningDimField::*;
        let fields = match tool {
            OpeningToolKind::Window | OpeningToolKind::MultiStoreyWindow => {
                vec![Width, Height, Sill]
            }
            OpeningToolKind::RoofWindow => vec![Width, Height],
            OpeningToolKind::Door => vec![Width, Height],
            OpeningToolKind::DoorSidelights => {
                vec![DoorWidth, DoorHeight, LeftLight, RightLight, Transom]
            }
            OpeningToolKind::Edit => vec![],
        };
        Self {
            tool,
            target,
            fields,
            idx: 0,
            values: Vec::new(),
        }
    }
    fn current(&self) -> Option<OpeningDimField> {
        self.fields.get(self.idx).copied()
    }
}

/// What the floating box should ask for the current staged-placement field.
pub struct OpeningDimPromptUiData {
    /// Box title, e.g. `"Window · width (m)"`.
    pub title: String,
    /// The active field — the UI binds the matching text buffer + Apply event.
    pub field: OpeningDimField,
    /// 1-based position + count, for a `"(2/3)"` progress hint.
    pub step: usize,
    pub total: usize,
    /// The field offers a Skip (optional door+sidelights lights).
    pub skippable: bool,
    /// Offer the "fill whole surface" shortcut (first field, plain Window, empty
    /// wall) — no measurements, turns the face into a window wall.
    pub offer_full_surface: bool,
    pub hint: String,
}

/// What kind of opening a view rectangle is (for colour/label in plan + elevation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningViewKind {
    Window,
    RoofWindow,
    Door,
    DoorPanel,
    SidelightWindow,
}

/// One opening on the unrolled wall, in wall-local coords (along, sill).
pub struct ElevationOpening {
    pub kind: OpeningViewKind,
    pub along_m: f32,
    pub sill_m: f32,
    pub width_m: f32,
    pub height_m: f32,
    pub selected: bool,
}

/// The unrolled selected wall + its openings, for the elevation overlay.
pub struct ElevationView {
    pub wall_len_m: f32,
    pub base_z: f32,
    /// The roof-profile top as `(along, z)` points across the wall.
    pub top_profile: Vec<(f32, f32)>,
    pub openings: Vec<ElevationOpening>,
}

/// What the prompt dock should ask for, for the in-flight placement.
pub struct OpeningPromptUiData {
    pub tool: OpeningToolKind,
    /// True when the host is a roof region (no sill; free placement).
    pub on_roof: bool,
    /// True when the host WALL has no openings yet — the "full surface / window
    /// wall" shortcut is offered only then (it spans the whole face).
    pub wall_empty: bool,
}

impl AppController {
    // ── mode + tool ──────────────────────────────────────────────────────────

    pub fn openings_mode(&self) -> bool {
        self.openings_mode
    }

    pub fn openings_tool(&self) -> OpeningToolKind {
        self.openings_tool
    }

    /// Enter/leave windows/doors mode. Mutually exclusive with the other area
    /// tools (mirrors `set_exposure_mode`).
    pub fn set_openings_mode(&mut self, on: bool) {
        self.close_wall_partition();
        self.openings_mode = on;
        self.pending_opening = None;
        self.opening_dim = None;
        self.opening_dim_error = None;
        self.elevation_wall = None;
        self.elevation_drag = None;
        self.elevation_selection = None;
        self.roof_window_sel = None;
        self.roof_window_drag = None;
        self.confirm_delete = false;
        if on {
            self.sync_active();
            self.set_living_area_mode(false);
            self.exposure_mode = false;
            self.selected_exposure_wall = None;
            self.exposure_sel = None;
            self.exposure_drag = None;
            self.selected_riser = None;
            self.phase_mode = false;
            self.step_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.roof_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
        }
    }

    /// Leave windows/doors mode + clear all its transient state. Called by the
    /// OTHER area-mode setters so they're mutually exclusive with this tool.
    pub(super) fn exit_openings_mode(&mut self) {
        self.openings_mode = false;
        self.pending_opening = None;
        self.opening_dim = None;
        self.opening_dim_error = None;
        self.elevation_wall = None;
        self.elevation_drag = None;
        self.elevation_selection = None;
        self.roof_window_sel = None;
        self.roof_window_drag = None;
        self.confirm_delete = false;
        // Every sibling area-mode setter routes through here, so this is the one
        // chokepoint that also closes any open wall-face partition view.
        self.close_wall_partition();
    }

    /// Pick a placement/edit tool from the toolbar — enters openings mode (if not
    /// already) and selects the tool in one step. The toolbar's 6 opening tools
    /// all route through here.
    pub fn select_opening_tool(&mut self, tool: OpeningToolKind) {
        if !self.openings_mode {
            self.set_openings_mode(true);
        }
        self.set_openings_tool(tool);
    }

    pub fn set_openings_tool(&mut self, tool: OpeningToolKind) {
        self.openings_tool = tool;
        self.pending_opening = None;
        self.opening_dim = None;
        self.opening_dim_error = None;
        self.confirm_delete = false;
        // Leaving Edit closes any open elevation + clears the surface selection.
        if tool != OpeningToolKind::Edit {
            self.elevation_wall = None;
            self.elevation_drag = None;
            self.elevation_selection = None;
            self.roof_window_sel = None;
            self.roof_window_drag = None;
        }
    }

    // ── plan tap: pick host or open elevation ─────────────────────────────────

    /// A plan tap in windows/doors mode. Edit → open the tapped wall's
    /// elevation; a placement tool → pick the nearest wall (or the roof region)
    /// and stage a `pending_opening` awaiting size entry.
    pub fn openings_pointer_down(&mut self, wx: f64, wy: f64, tol: f64) {
        if self.elevation_wall.is_some() {
            return; // elevation owns the canvas
        }
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        self.materialize();
        let tool = self.openings_tool;
        // Resolve the pick into owned values (drops the storey borrow before we
        // mutate self).
        enum Pick {
            Elevation(ItemId),
            RoofWindowSel(ItemId),
            Wall(ItemId, f32),
            Roof(ItemId, (f32, f32)),
            None,
        }
        let pick = {
            let Some(storey) = self
                .current_record
                .as_ref()
                .and_then(|r| r.data.storeys.get(idx))
            else {
                return;
            };
            match tool {
                // Edit-on-surface: a roof window (interior) selects for plan
                // drag/delete; otherwise the nearest wall opens its elevation.
                OpeningToolKind::Edit => {
                    if let Some(wid) = openings::roof_window_at(storey, wx as f32, wy as f32) {
                        Pick::RoofWindowSel(wid)
                    } else {
                        match openings::pick_wall_along(storey, wx, wy, tol) {
                            Some((id, _, _)) => Pick::Elevation(id),
                            None => Pick::None,
                        }
                    }
                }
                OpeningToolKind::RoofWindow => match openings::pick_roof_region(storey, wx, wy) {
                    Some((id, plan)) => Pick::Roof(id, plan),
                    None => Pick::None,
                },
                _ => match openings::pick_wall_along(storey, wx, wy, tol) {
                    Some((id, along, _len)) => Pick::Wall(id, along),
                    None => Pick::None,
                },
            }
        };
        match pick {
            Pick::Elevation(wall_id) => {
                self.elevation_wall = Some(wall_id);
                self.elevation_selection = None;
                self.pending_opening = None;
                self.roof_window_sel = None;
                self.confirm_delete = false;
            }
            Pick::RoofWindowSel(wid) => {
                self.record_history(); // one snapshot for the (possible) drag
                self.roof_window_sel = Some(wid);
                self.roof_window_drag = Some(wid);
                self.elevation_wall = None;
                self.pending_opening = None;
                self.confirm_delete = false;
            }
            Pick::Wall(wall_id, along_m) => {
                let target = PendingOpeningTarget::Wall { wall_id, along_m };
                self.pending_opening = Some(PendingOpening { tool, target });
                self.start_opening_flow(tool, target);
            }
            Pick::Roof(region_id, plan) => {
                let target = PendingOpeningTarget::Roof { region_id, plan };
                self.pending_opening = Some(PendingOpening { tool, target });
                self.start_opening_flow(tool, target);
            }
            Pick::None => {}
        }
    }

    /// What the dock should prompt for the in-flight placement, if any.
    pub fn pending_opening_prompt(&self) -> Option<OpeningPromptUiData> {
        let p = self.pending_opening.as_ref()?;
        let wall_empty = match p.target {
            PendingOpeningTarget::Wall { wall_id, .. } => self
                .current_storey_idx
                .and_then(|idx| self.current_record.as_ref()?.data.storeys.get(idx))
                .map(|s| openings::wall_openings(s, wall_id).is_empty())
                .unwrap_or(false),
            PendingOpeningTarget::Roof { .. } => false,
        };
        Some(OpeningPromptUiData {
            tool: p.tool,
            on_roof: matches!(p.target, PendingOpeningTarget::Roof { .. }),
            wall_empty,
        })
    }

    /// Place a FULL-SURFACE window — turn the (empty) host wall into a "window
    /// wall" filling the whole face. No size entry: width = wall length, height =
    /// floor-to-roof at the tap, sill 0. Walls only.
    pub fn place_pending_full_surface_window(
        &mut self,
        glazing: Glazing,
        frame: Frame,
        year: u32,
    ) -> Result<(), String> {
        let Some(pending) = self.pending_opening.take() else {
            return Ok(());
        };
        let PendingOpeningTarget::Wall { wall_id, along_m } = pending.target else {
            self.pending_opening = Some(pending);
            return Ok(()); // walls only
        };
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        self.materialize();
        let undo_snap = self.current_history_snapshot();
        // Face height = top_z − base at the tap. `max_sill_for(along, 0)` is
        // exactly that (public), avoiding the crate-private `top_z_at_along`.
        let height_m = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|s| s.walls.regions.iter().find(|r| r.id == wall_id))
            .map(|wall| wall.max_sill_for(along_m as f64, 0.0) as f32)
            .unwrap_or(0.0);
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        let type_id = record.data.ensure_window_type(glazing, frame, year);
        let storey = &mut record.data.storeys[idx];
        let result = openings::add_full_wall_window(storey, wall_id, height_m, type_id);
        self.finish_opening(result.map(|_| ()), undo_snap, pending)
    }

    pub fn cancel_pending_opening(&mut self) {
        self.pending_opening = None;
    }

    // ── staged numeric placement (floating box + laser) ───────────────────────

    /// Begin collecting an opening's measurements one field at a time in the
    /// floating number box (so the laser feeds them). Started on the host pick.
    fn start_opening_flow(&mut self, tool: OpeningToolKind, target: PendingOpeningTarget) {
        self.opening_dim = Some(OpeningDimFlow::new(tool, target));
        self.opening_dim_error = None;
    }

    /// What the floating box should ask for next, if a staged placement is live.
    pub fn opening_dim_prompt(&self) -> Option<OpeningDimPromptUiData> {
        let flow = self.opening_dim.as_ref()?;
        let field = flow.current()?;
        let offer_full_surface =
            flow.idx == 0 && flow.tool == OpeningToolKind::Window && self.flow_wall_empty(flow);
        Some(OpeningDimPromptUiData {
            title: format!("{} · {}", tool_label(flow.tool), field.label()),
            field,
            step: flow.idx + 1,
            total: flow.fields.len(),
            skippable: field.optional(),
            offer_full_surface,
            hint: field.hint().to_string(),
        })
    }

    /// The staged-placement inline error (wall too short, …), for the box.
    pub fn opening_dim_error(&self) -> Option<&str> {
        self.opening_dim_error.as_deref()
    }

    /// True when the flow's host wall has no openings yet (so the full-surface
    /// shortcut is offered).
    fn flow_wall_empty(&self, flow: &OpeningDimFlow) -> bool {
        let PendingOpeningTarget::Wall { wall_id, .. } = flow.target else {
            return false;
        };
        self.current_storey_idx
            .and_then(|idx| self.current_record.as_ref()?.data.storeys.get(idx))
            .map(|s| openings::wall_openings(s, wall_id).is_empty())
            .unwrap_or(false)
    }

    /// Record the entered value for the current field and advance — placing the
    /// opening once the last field is in.
    pub fn apply_opening_dim(&mut self, value: f32) -> Result<(), String> {
        let Some(flow) = self.opening_dim.as_mut() else {
            return Ok(());
        };
        flow.values.push(Some(value));
        flow.idx += 1;
        self.opening_dim_error = None;
        if self
            .opening_dim
            .as_ref()
            .and_then(|f| f.current())
            .is_some()
        {
            return Ok(());
        }
        self.finalize_opening_placement()
    }

    /// Skip the current (optional) field and advance — placing once it was last.
    pub fn skip_opening_dim(&mut self) -> Result<(), String> {
        let Some(flow) = self.opening_dim.as_mut() else {
            return Ok(());
        };
        flow.values.push(None);
        flow.idx += 1;
        self.opening_dim_error = None;
        if self
            .opening_dim
            .as_ref()
            .and_then(|f| f.current())
            .is_some()
        {
            return Ok(());
        }
        self.finalize_opening_placement()
    }

    /// Abandon the in-flight staged placement.
    pub fn cancel_opening_placement(&mut self) {
        self.opening_dim = None;
        self.opening_dim_error = None;
        self.pending_opening = None;
    }

    /// Place the opening from the collected fields, with a DEFAULT window type
    /// (the assessor refines glazing/frame/year later in Edit on surface). On a
    /// model error the flow re-opens at the last field with the error shown.
    fn finalize_opening_placement(&mut self) -> Result<(), String> {
        let Some(flow) = self.opening_dim.take() else {
            return Ok(());
        };
        self.pending_opening = None;
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        self.materialize();
        let undo_snap = self.current_history_snapshot();
        let vals = flow.values.clone();
        let v = |i: usize| vals.get(i).copied().flatten();
        let req = |i: usize| v(i).unwrap_or(0.0);
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        let type_id = record
            .data
            .ensure_window_type(Glazing::Double, Frame::WoodUPVC, 0);
        let storey = &mut record.data.storeys[idx];
        let result: Result<(), OpeningError> = match (flow.tool, flow.target) {
            (OpeningToolKind::Window, PendingOpeningTarget::Wall { wall_id, along_m }) => {
                openings::add_wall_window(storey, wall_id, along_m, req(0), req(1), req(2), type_id)
                    .map(|_| ())
            }
            (
                OpeningToolKind::MultiStoreyWindow,
                PendingOpeningTarget::Wall { wall_id, along_m },
            ) => openings::add_multi_storey_window(
                storey,
                wall_id,
                along_m,
                req(0),
                req(1),
                req(2),
                type_id,
            )
            .map(|_| ()),
            (OpeningToolKind::RoofWindow, PendingOpeningTarget::Roof { region_id, plan }) => {
                openings::add_roof_window(storey, region_id, plan, req(0), req(1), type_id)
                    .map(|_| ())
            }
            (OpeningToolKind::Door, PendingOpeningTarget::Wall { wall_id, along_m }) => {
                openings::add_wall_door(
                    storey,
                    wall_id,
                    along_m,
                    req(0),
                    req(1),
                    DoorKind::Solid,
                    "Door".to_string(),
                    openings::DEFAULT_DOOR_SILL_M,
                )
                .map(|_| ())
            }
            (OpeningToolKind::DoorSidelights, PendingOpeningTarget::Wall { wall_id, along_m }) => {
                openings::compose_door_sidelights(
                    storey,
                    wall_id,
                    along_m,
                    DoorSidelightsSpec {
                        door_width_m: req(0),
                        door_height_m: req(1),
                        left_sidelight_m: v(2),
                        right_sidelight_m: v(3),
                        transom_m: v(4),
                        window_type_id: type_id,
                        door_type_name: "Door".to_string(),
                    },
                )
                .map(|_| ())
            }
            _ => return Ok(()), // tool/target mismatch — nothing to place
        };
        match result {
            Ok(()) => {
                self.reseed_opening_anchors(idx);
                let _ = self.mark_dirty();
                self.push_history_snapshot(undo_snap);
                self.opening_dim_error = None;
                Ok(())
            }
            Err(e) => {
                // Re-open at the last field so the value can be re-entered.
                let mut flow = flow;
                flow.idx = flow.idx.saturating_sub(1);
                flow.values.pop();
                self.opening_dim = Some(flow);
                let msg = opening_error_msg(e);
                self.opening_dim_error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Full-surface "window wall" shortcut from the first staged field: no
    /// measurements, the whole face becomes a window. Walls only.
    pub fn fill_opening_surface(&mut self) -> Result<(), String> {
        let Some(flow) = self.opening_dim.take() else {
            return Ok(());
        };
        let PendingOpeningTarget::Wall { wall_id, along_m } = flow.target else {
            self.opening_dim = Some(flow);
            return Ok(());
        };
        self.pending_opening = None;
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        self.materialize();
        let undo_snap = self.current_history_snapshot();
        let height_m = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|s| s.walls.regions.iter().find(|r| r.id == wall_id))
            .map(|wall| wall.max_sill_for(along_m as f64, 0.0) as f32)
            .unwrap_or(0.0);
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        let type_id = record
            .data
            .ensure_window_type(Glazing::Double, Frame::WoodUPVC, 0);
        let storey = &mut record.data.storeys[idx];
        match openings::add_full_wall_window(storey, wall_id, height_m, type_id) {
            Ok(_) => {
                self.reseed_opening_anchors(idx);
                let _ = self.mark_dirty();
                self.push_history_snapshot(undo_snap);
                Ok(())
            }
            Err(e) => {
                let msg = opening_error_msg(e);
                self.opening_dim_error = Some(msg.clone());
                self.opening_dim = Some(flow);
                Err(msg)
            }
        }
    }

    // ── apply: place the opening ──────────────────────────────────────────────

    /// Place the pending WINDOW (wall or roof) with the entered size + glazing.
    pub fn place_pending_window(
        &mut self,
        width_m: f32,
        height_m: f32,
        sill_m: f32,
        glazing: Glazing,
        frame: Frame,
        year: u32,
    ) -> Result<(), String> {
        let Some(pending) = self.pending_opening.take() else {
            return Ok(());
        };
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        self.materialize();
        let undo_snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        let type_id = record.data.ensure_window_type(glazing, frame, year);
        let storey = &mut record.data.storeys[idx];
        let result = match pending.target {
            PendingOpeningTarget::Wall {
                wall_id, along_m, ..
            } if pending.tool == OpeningToolKind::MultiStoreyWindow => {
                openings::add_multi_storey_window(
                    storey, wall_id, along_m, width_m, height_m, sill_m, type_id,
                )
            }
            PendingOpeningTarget::Wall {
                wall_id, along_m, ..
            } => openings::add_wall_window(
                storey, wall_id, along_m, width_m, height_m, sill_m, type_id,
            ),
            PendingOpeningTarget::Roof { region_id, plan } => {
                openings::add_roof_window(storey, region_id, plan, width_m, height_m, type_id)
            }
        };
        self.finish_opening(result.map(|_| ()), undo_snap, pending)
    }

    /// Place the pending DOOR with the entered size.
    pub fn place_pending_door(&mut self, width_m: f32, height_m: f32) -> Result<(), String> {
        let Some(pending) = self.pending_opening.take() else {
            return Ok(());
        };
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        let PendingOpeningTarget::Wall {
            wall_id, along_m, ..
        } = pending.target
        else {
            return Ok(()); // doors only go on walls
        };
        self.materialize();
        let undo_snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        let storey = &mut record.data.storeys[idx];
        let result = openings::add_wall_door(
            storey,
            wall_id,
            along_m,
            width_m,
            height_m,
            DoorKind::Solid,
            "Door".to_string(),
            openings::DEFAULT_DOOR_SILL_M,
        );
        self.finish_opening(result.map(|_| ()), undo_snap, pending)
    }

    /// Place the pending DOOR+SIDELIGHTS assembly (composer output).
    #[allow(clippy::too_many_arguments)]
    pub fn place_pending_door_sidelights(
        &mut self,
        door_w: f32,
        door_h: f32,
        left: Option<f32>,
        right: Option<f32>,
        transom: Option<f32>,
        glazing: Glazing,
        frame: Frame,
        year: u32,
    ) -> Result<(), String> {
        let Some(pending) = self.pending_opening.take() else {
            return Ok(());
        };
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        let PendingOpeningTarget::Wall {
            wall_id, along_m, ..
        } = pending.target
        else {
            return Ok(());
        };
        self.materialize();
        let undo_snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        let window_type_id = record.data.ensure_window_type(glazing, frame, year);
        let storey = &mut record.data.storeys[idx];
        let result = openings::compose_door_sidelights(
            storey,
            wall_id,
            along_m,
            DoorSidelightsSpec {
                door_width_m: door_w,
                door_height_m: door_h,
                left_sidelight_m: left,
                right_sidelight_m: right,
                transom_m: transom,
                window_type_id,
                door_type_name: "Door".to_string(),
            },
        );
        self.finish_opening(result.map(|_| ()), undo_snap, pending)
    }

    /// Shared tail: on success mark dirty + push the undo snapshot; on failure
    /// restore the pending placement and map the error to a user message.
    fn finish_opening(
        &mut self,
        result: Result<(), OpeningError>,
        undo_snap: HistorySnapshot,
        pending: PendingOpening,
    ) -> Result<(), String> {
        match result {
            Ok(()) => {
                if let Some(idx) = self.current_storey_idx {
                    self.reseed_opening_anchors(idx);
                }
                let _ = self.mark_dirty();
                self.push_history_snapshot(undo_snap);
                Ok(())
            }
            Err(e) => {
                self.pending_opening = Some(pending);
                Err(opening_error_msg(e))
            }
        }
    }

    /// Recompute the durable parametric `wall_anchor` of every wall opening on
    /// storey `idx` from its current (live) placement — so the anchor is the
    /// source of truth after a placement / move / auto-redistribute, and the
    /// resolve re-derives the `wall_id`/`along_m` cache from it (no revert).
    /// Cheap and idempotent; roof openings are left alone.
    pub(super) fn reseed_opening_anchors(&mut self, idx: usize) {
        use ber_model::ber::fenestration::OpeningPlacement;
        let anchor_of = |storey: &ber_model::ber::structural::Storey,
                         plan: &manip_plan::FloorPlan,
                         placement: &Option<OpeningPlacement>| {
            let Some(OpeningPlacement::Wall {
                wall_id, along_m, ..
            }) = placement
            else {
                return None; // roof / unplaced
            };
            let r = storey.walls.regions.iter().find(|r| r.id == *wall_id)?;
            document::opening_wall_anchor(plan, r, *along_m)
        };
        let (win, drs) = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            let Some(sd) = self.document.storeys.get(idx) else {
                return;
            };
            let plan = &sd.outline.floor_plan;
            let win: Vec<_> = storey
                .windows
                .iter()
                .map(|w| anchor_of(storey, plan, &w.placement))
                .collect();
            let drs: Vec<_> = storey
                .doors
                .iter()
                .map(|d| anchor_of(storey, plan, &d.placement))
                .collect();
            (win, drs)
        };
        if let Some(storey) = self
            .current_record
            .as_mut()
            .and_then(|r| r.data.storeys.get_mut(idx))
        {
            for (w, a) in storey.windows.iter_mut().zip(win) {
                if a.is_some() {
                    w.wall_anchor = a;
                }
            }
            for (d, a) in storey.doors.iter_mut().zip(drs) {
                if a.is_some() {
                    d.wall_anchor = a;
                }
            }
        }
    }

    // ── elevation editor ──────────────────────────────────────────────────────

    pub fn elevation_wall(&self) -> Option<ItemId> {
        self.elevation_wall
    }

    pub fn close_elevation(&mut self) {
        self.elevation_wall = None;
        self.elevation_drag = None;
        self.elevation_selection = None;
        self.roof_window_sel = None;
        self.roof_window_drag = None;
        self.confirm_delete = false;
    }

    /// The unrolled selected wall + its openings, for the overlay. `None` when
    /// no elevation is open or its wall has gone.
    pub fn elevation_view(&self) -> Option<ElevationView> {
        let wall_id = self.elevation_wall?;
        let idx = self.current_storey_idx?;
        let storey = self.current_record.as_ref()?.data.storeys.get(idx)?;
        let wall = storey.walls.regions.iter().find(|r| r.id == wall_id)?;
        let (_, _, len, base_z) = wall.base_frame()?;
        // The wall-local top profile is the model's projection (WallRegion).
        let top_profile = wall.top_profile();
        let openings = self
            .wall_openings(storey, wall_id)
            .into_iter()
            .map(|(r, kind, along, sill, w, h)| ElevationOpening {
                kind,
                along_m: along,
                sill_m: sill,
                width_m: w,
                height_m: h,
                selected: self.elevation_selection == Some(r),
            })
            .collect();
        Some(ElevationView {
            wall_len_m: len as f32,
            base_z: base_z as f32,
            top_profile,
            openings,
        })
    }

    /// Press in the elevation at wall-local `(along, height)`: select + grab the
    /// opening under the cursor (if any), snapshotting once for the drag.
    pub fn elevation_pointer_down(&mut self, along_m: f32, height_m: f32) {
        let Some(wall_id) = self.elevation_wall else {
            return;
        };
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let hit = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|s| openings::opening_at(s, wall_id, along_m, height_m));
        if let Some(r) = hit {
            self.record_history();
            self.elevation_drag = Some(r);
            self.elevation_selection = Some(r);
        } else {
            self.elevation_selection = None;
        }
    }

    /// Drag the grabbed opening so its centre follows the cursor (horizontal →
    /// along with auto no-overlap; vertical → sill). No-op if nothing grabbed.
    pub fn elevation_pointer_drag(&mut self, along_m: f32, height_m: f32) {
        let Some(r) = self.elevation_drag else {
            return;
        };
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        // Centre on the cursor: sill = cursor height − half the opening height.
        let half_h = self
            .current_record
            .as_ref()
            .and_then(|rec| rec.data.storeys.get(idx))
            .map(|s| openings::opening_height(s, r) / 2.0)
            .unwrap_or(0.0);
        let new_sill = (height_m - half_h).max(0.0);
        if let Some(record) = self.current_record.as_mut() {
            if let Some(storey) = record.data.storeys.get_mut(idx) {
                let _ = openings::move_opening(storey, r, along_m, new_sill);
            }
        }
        self.reseed_opening_anchors(idx);
        let _ = self.mark_dirty();
    }

    pub fn elevation_pointer_up(&mut self) {
        self.elevation_drag = None;
    }

    /// Drag a grabbed ROOF window in the plan view to a new point (Edit-on-surface
    /// tool). No-op if nothing is grabbed. The drag was snapshotted on pointer-down.
    pub fn openings_pointer_drag(&mut self, wx: f64, wy: f64) {
        let Some(wid) = self.roof_window_drag else {
            return;
        };
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        if let Some(record) = self.current_record.as_mut() {
            if let Some(storey) = record.data.storeys.get_mut(idx) {
                openings::move_roof_window(storey, wid, (wx as f32, wy as f32));
            }
        }
        if let Some(idx) = self.current_storey_idx {
            self.reseed_opening_anchors(idx);
        }
        let _ = self.mark_dirty();
    }

    pub fn openings_pointer_up(&mut self) {
        self.roof_window_drag = None;
    }

    /// The roof window selected in the plan view (Edit-on-surface), for highlight.
    pub fn roof_window_selection(&self) -> Option<ItemId> {
        self.roof_window_sel
    }

    /// Is anything (a wall opening in the elevation, or a roof window in plan)
    /// selected and thus deletable?
    pub fn has_deletable_selection(&self) -> bool {
        self.elevation_selection.is_some() || self.roof_window_sel.is_some()
    }

    /// True while a delete is awaiting confirmation.
    pub fn delete_confirm_pending(&self) -> bool {
        self.confirm_delete
    }

    /// Ask to delete the selected opening — shows a confirm step rather than
    /// deleting outright.
    pub fn request_delete_opening(&mut self) {
        if self.has_deletable_selection() {
            self.confirm_delete = true;
        }
    }

    pub fn cancel_delete(&mut self) {
        self.confirm_delete = false;
    }

    /// Confirm the pending delete and perform it.
    pub fn confirm_delete_opening(&mut self) {
        self.confirm_delete = false;
        self.delete_selected_opening();
    }

    /// Delete the SELECTED opening — a wall opening (the whole assembly if it's a
    /// member) from the elevation, or a roof window selected in plan. Undoable.
    pub fn delete_selected_opening(&mut self) {
        let target = self
            .elevation_selection
            .or_else(|| self.roof_window_sel.map(OpeningRef::Window));
        let Some(r) = target else {
            return;
        };
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let undo_snap = self.current_history_snapshot();
        if let Some(record) = self.current_record.as_mut() {
            if let Some(storey) = record.data.storeys.get_mut(idx) {
                openings::delete_opening(storey, r);
            }
        }
        self.elevation_selection = None;
        self.roof_window_sel = None;
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Every wall opening on `wall_id`: `(ref, kind, along, sill, width, height)`.
    /// The model owns the gather ([`openings::wall_openings`]); the shell only
    /// maps `is_door` + assembly membership to its own view kind.
    fn wall_openings(
        &self,
        storey: &Storey,
        wall_id: ItemId,
    ) -> Vec<(OpeningRef, OpeningViewKind, f32, f32, f32, f32)> {
        openings::wall_openings(storey, wall_id)
            .into_iter()
            .map(|o| {
                let kind = match (o.is_door, o.assembly_id.is_some()) {
                    (false, false) => OpeningViewKind::Window,
                    (false, true) => OpeningViewKind::SidelightWindow,
                    (true, false) => OpeningViewKind::Door,
                    (true, true) => OpeningViewKind::DoorPanel,
                };
                (o.opening, kind, o.along_m, o.sill_m, o.width_m, o.height_m)
            })
            .collect()
    }
}

/// Display name of a placement tool, for the floating-box title.
fn tool_label(tool: OpeningToolKind) -> &'static str {
    match tool {
        OpeningToolKind::Window => "Window",
        OpeningToolKind::MultiStoreyWindow => "Multi-storey window",
        OpeningToolKind::RoofWindow => "Roof window",
        OpeningToolKind::Door => "Door",
        OpeningToolKind::DoorSidelights => "Door + sidelights",
        OpeningToolKind::Edit => "Edit",
    }
}

fn opening_error_msg(e: OpeningError) -> String {
    match e {
        OpeningError::WallTooShort {
            needed_m,
            available_m,
        } => format!("Wall too short: needs {needed_m:.2} m, only {available_m:.2} m available."),
        OpeningError::NoHostRegion => "No wall or roof here to place on.".to_string(),
        OpeningError::ZeroSize => "Width and height must be greater than zero.".to_string(),
    }
}
