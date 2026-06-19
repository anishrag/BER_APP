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
        self.openings_mode = on;
        self.pending_opening = None;
        self.elevation_wall = None;
        self.elevation_drag = None;
        self.elevation_selection = None;
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
        self.elevation_wall = None;
        self.elevation_drag = None;
        self.elevation_selection = None;
    }

    pub fn set_openings_tool(&mut self, tool: OpeningToolKind) {
        self.openings_tool = tool;
        self.pending_opening = None;
        // Leaving Edit closes any open elevation.
        if tool != OpeningToolKind::Edit {
            self.elevation_wall = None;
            self.elevation_drag = None;
            self.elevation_selection = None;
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
                OpeningToolKind::Edit => match openings::pick_wall_along(storey, wx, wy, tol) {
                    Some((id, _, _)) => Pick::Elevation(id),
                    None => Pick::None,
                },
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
            }
            Pick::Wall(wall_id, along_m) => {
                self.pending_opening = Some(PendingOpening {
                    tool,
                    target: PendingOpeningTarget::Wall { wall_id, along_m },
                });
            }
            Pick::Roof(region_id, plan) => {
                self.pending_opening = Some(PendingOpening {
                    tool,
                    target: PendingOpeningTarget::Roof { region_id, plan },
                });
            }
            Pick::None => {}
        }
    }

    /// What the dock should prompt for the in-flight placement, if any.
    pub fn pending_opening_prompt(&self) -> Option<OpeningPromptUiData> {
        self.pending_opening.as_ref().map(|p| OpeningPromptUiData {
            tool: p.tool,
            on_roof: matches!(p.target, PendingOpeningTarget::Roof { .. }),
        })
    }

    pub fn cancel_pending_opening(&mut self) {
        self.pending_opening = None;
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

    // ── elevation editor ──────────────────────────────────────────────────────

    pub fn elevation_wall(&self) -> Option<ItemId> {
        self.elevation_wall
    }

    pub fn close_elevation(&mut self) {
        self.elevation_wall = None;
        self.elevation_drag = None;
        self.elevation_selection = None;
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
        let _ = self.mark_dirty();
    }

    pub fn elevation_pointer_up(&mut self) {
        self.elevation_drag = None;
    }

    /// Delete the opening selected in the elevation (the whole assembly if it's
    /// a member), undoably.
    pub fn delete_selected_opening(&mut self) {
        let Some(r) = self.elevation_selection else {
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
