//! Windows/doors tool: the app-shell workflow over `ber_model::ber::openings`.
//! Thin — picks the host wall/roof from a plan tap, prompts size + type, calls
//! the pure ops against `current_record.data.storeys[idx]`, snapshots for undo,
//! and derives the UI-facing view data. All geometry/model mutation lives in the
//! ber_model ops; this only sequences them.
use super::*;
use ber_model::ber::common::{ItemId, Photo};
use ber_model::ber::fenestration::{
    window_shape_from_inputs, ComplexWindowKind, DoorKind, Frame, Glazing, Overshading, WallAnchor,
    WindowShape,
};
use ber_model::ber::openings::{
    self, DoorSidelightsSpec, OpeningError, OpeningRef, SidelightLayout,
};

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
    /// A single window with a non-rectangular OUTLINE (rounded top, circular,
    /// gable triangle, T-shape, …) — the shape + its dims come from the box.
    ComplexWindow,
    /// Tap a wall to open its elevation editor (reposition existing openings).
    Edit,
}

/// A Complex-window placement from the box: the chosen shape kind + the ordered
/// dimension inputs (in `ComplexWindowKind::field_labels` order). The controller
/// turns these into `(width_m, height_m, WindowShape)` via
/// [`window_shape_from_inputs`].
#[derive(Debug, Clone, PartialEq)]
pub struct ComplexWindowPlacement {
    pub kind: ComplexWindowKind,
    pub dims: Vec<f32>,
}

/// How the assessor dimensions a door + sidelights placement. Two input styles
/// that both produce a [`DoorSidelightsSpec`]; toggled in the floating box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DoorSidelightsMode {
    /// Laser-friendly: measure the whole doorset (overall W × H), then the door
    /// leaf within it (W × H). The leftover width becomes sidelight glass; the
    /// strip above the door becomes a transom. Four shots and done.
    #[default]
    Overall,
    /// Type each piece: door W/H, then each sidelight's width + sill, then the
    /// transom height. The original, fully-explicit style.
    Detailed,
}

/// The door+sidelights-specific inputs the floating box collects, tagged by the
/// active [`DoorSidelightsMode`]. The door leaf's own width/height ride on
/// [`AppController::place_opening`]'s `width_m`/`height_m` (both modes have a
/// door), so this carries only the mode's extra measurements.
#[derive(Debug, Clone, PartialEq)]
pub enum DoorSidelightsPlacement {
    /// `Overall` mode: the whole-doorset width/height plus the sidelight layout
    /// and one optional sill for the present sidelight(s).
    Overall {
        overall_w: f32,
        overall_h: f32,
        layout: SidelightLayout,
        sill: Option<f32>,
        transom_semicircular: bool,
    },
    /// `Detailed` mode: each sidelight's width + sill and the transom height
    /// (`None`/`0` → that piece is absent).
    Detailed {
        left: Option<f32>,
        left_sill: Option<f32>,
        right: Option<f32>,
        right_sill: Option<f32>,
        transom: Option<f32>,
        transom_semicircular: bool,
    },
}

/// The host a pending placement landed on (awaiting size + type entry).
#[derive(Debug, Clone, Copy)]
pub(super) enum PendingOpeningTarget {
    /// A wall pick. `wall_id`/`along_m` are the TAP-TIME cache; `anchor` is the
    /// durable parametric location captured at tap. Wall + ceiling ids are
    /// re-minted on every resolve (attaching a photo re-resolves), so at confirm
    /// the live `(wall_id, along_m)` is re-derived from `anchor` — trusting the
    /// stale tap-time id would dangle and error `NoHostRegion`.
    Wall {
        wall_id: ItemId,
        along_m: f32,
        anchor: Option<WallAnchor>,
    },
    Roof {
        region_id: ItemId,
        plan: (f32, f32),
    },
}

#[derive(Clone, Copy)]
pub(super) struct PendingOpening {
    pub tool: OpeningToolKind,
    pub target: PendingOpeningTarget,
}

/// One field's presentation in the multi-field floating box. The UI binds these
/// to buffers POSITIONALLY (Width, Height, then the door+sidelights lights), so
/// there's no field-kind tag — the order is the contract.
pub struct OpeningFieldSpec {
    pub label: String,
    pub hint: &'static str,
    /// Required fields gate the Place button; optional ones (the door+sidelights
    /// lights) may stay blank.
    pub required: bool,
}

/// What the floating box should collect for the in-flight placement: the fields
/// (always Width + Height, plus the three optional door+sidelights lights), plus
/// whether the "fill whole surface" shortcut is offered.
pub struct OpeningFieldsPrompt {
    /// Box title, e.g. `"Window · size"`.
    pub title: String,
    pub fields: Vec<OpeningFieldSpec>,
    /// Offer the "fill whole surface" shortcut (plain Window on an empty wall) —
    /// no measurements, turns the face into a window wall.
    pub offer_full_surface: bool,
    /// `Some(mode)` for the Door + sidelights tool — the box shows the
    /// Overall/Detailed mode switch and binds/renders the mode's fields and
    /// checkboxes. `None` for every other tool.
    pub door_sidelights: Option<DoorSidelightsMode>,
    /// `Some(kind)` for the Complex-window tool — the box shows the shape
    /// dropdown and the kind's dimension fields. `None` for every other tool.
    pub complex_window: Option<ComplexWindowKind>,
    /// Show the window-type picker (glazing/frame/year/cert, or reuse an existing
    /// type) — for tools that place a window: Window / Multi-storey / Roof window
    /// and the glazed parts of Door + sidelights. A plain solid Door has none.
    pub offer_window_type: bool,
    /// Bumped on every fresh host pick — the UI resets the box's field focus when
    /// it changes, so a new placement starts on the first field.
    pub seq: u64,
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
    /// The window's outline shape (drawn accordingly); `Rectangular` for doors.
    pub shape: WindowShape,
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

/// The window type the placement box resolved to: either an EXISTING shared type
/// (reuse its id) or a NEW set of attributes (find-or-create, so it only mints a
/// type when it differs from every existing one — the user's "only add if
/// different" rule, enforced by `ensure_window_type`'s dedup).
#[derive(Debug, Clone, PartialEq)]
pub enum WindowTypeChoice {
    Existing(ItemId),
    New {
        glazing: Glazing,
        frame: Frame,
        year: u32,
        certified: bool,
        /// Pane gap(s) in mm (one for double, two for triple). Part of identity.
        gap: Vec<u32>,
        /// Optional model name — distinguishes two otherwise-identical types.
        name: Option<String>,
    },
}

impl Default for WindowTypeChoice {
    /// The placement box's starting point when no existing type is picked: a new
    /// double-glazed Wood/UPVC, uncertified, unknown year, no gap/name — the old
    /// hardcoded default, now overridable in the box.
    fn default() -> Self {
        WindowTypeChoice::New {
            glazing: Glazing::Double,
            frame: Frame::WoodUPVC,
            year: 0,
            certified: false,
            gap: Vec::new(),
            name: None,
        }
    }
}

/// A window type already in use on the record — offered in the placement box so a
/// new window can JOIN an existing type by id instead of minting a duplicate.
#[derive(Debug, Clone)]
pub struct ExistingWindowType {
    pub id: ItemId,
    pub glazing: Glazing,
    pub frame: Frame,
    pub year: u32,
    pub certified: bool,
    pub gap: Vec<u32>,
    pub name: Option<String>,
    /// Human label, e.g. `"North bay — Double · Wood/UPVC · 16mm · 2010 · Cert"`.
    pub label: String,
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
            self.ceiling_point_mode = false;
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
                // Capture the DURABLE parametric anchor now — the tap-time
                // `wall_id` re-mints on the next resolve (e.g. attaching a photo
                // re-resolves), so confirm re-derives the live id from `anchor`.
                let anchor = self.pending_wall_anchor(idx, wall_id, along_m);
                let target = PendingOpeningTarget::Wall {
                    wall_id,
                    along_m,
                    anchor,
                };
                self.pending_opening = Some(PendingOpening { tool, target });
                self.opening_dim_error = None;
                self.placement_seq = self.placement_seq.wrapping_add(1);
            }
            Pick::Roof(region_id, plan) => {
                let target = PendingOpeningTarget::Roof { region_id, plan };
                self.pending_opening = Some(PendingOpening { tool, target });
                self.opening_dim_error = None;
                self.placement_seq = self.placement_seq.wrapping_add(1);
            }
            Pick::None => {}
        }
    }

    /// The durable [`WallAnchor`] for a tap on wall `wall_id` at `along_m`, from
    /// the live storey's outline plan — the same anchor `reseed_opening_anchors`
    /// stamps onto placed openings, so the confirm re-home matches. `None` when
    /// the tap point isn't on the outline perimeter (e.g. a riser wall); the
    /// pending target then falls back to the tap-time id at confirm.
    fn pending_wall_anchor(&self, idx: usize, wall_id: ItemId, along_m: f32) -> Option<WallAnchor> {
        let storey = self.current_record.as_ref()?.data.storeys.get(idx)?;
        let plan = &self.document.storeys.get(idx)?.outline.floor_plan;
        let wall = storey.walls.regions.iter().find(|r| r.id == wall_id)?;
        document::opening_wall_anchor(plan, wall, along_m)
    }

    /// Re-home a pending placement's target against the FRESHLY-RESOLVED storey
    /// (call after `materialize`, which re-mints wall + ceiling ids). A Wall
    /// target's `(wall_id, along_m)` is re-derived from its durable anchor; a
    /// Roof target's `region_id` is re-derived from the stored plan point. Both
    /// fall back to the stored ids when re-derivation fails, so a placement that
    /// never churned still lands. All the anchor math lives in `document`.
    fn rehome_pending_target(
        &self,
        idx: usize,
        target: PendingOpeningTarget,
    ) -> PendingOpeningTarget {
        match target {
            PendingOpeningTarget::Wall {
                wall_id,
                along_m,
                anchor,
            } => {
                let (wall_id, along_m) = anchor
                    .and_then(|a| {
                        let storey = self.current_record.as_ref()?.data.storeys.get(idx)?;
                        let plan = &self.document.storeys.get(idx)?.outline.floor_plan;
                        document::resolve_wall_anchor(plan, &storey.walls, &a)
                    })
                    .unwrap_or((wall_id, along_m));
                PendingOpeningTarget::Wall {
                    wall_id,
                    along_m,
                    anchor,
                }
            }
            PendingOpeningTarget::Roof { region_id, plan } => {
                let region_id = self
                    .current_record
                    .as_ref()
                    .and_then(|r| r.data.storeys.get(idx))
                    .and_then(|s| openings::pick_roof_region(s, plan.0 as f64, plan.1 as f64))
                    .map(|(id, _)| id)
                    .unwrap_or(region_id);
                PendingOpeningTarget::Roof { region_id, plan }
            }
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

    /// Window types already in use on this record (referenced by ≥1 window), for
    /// the placement box's "use an existing type" picker — deduped by id, in
    /// registry order. Empty when no windows exist yet (the box then only offers
    /// the new-type fields).
    pub fn existing_window_types(&self) -> Vec<ExistingWindowType> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut used = std::collections::HashSet::new();
        for s in &record.data.storeys {
            for w in &s.windows {
                used.insert(w.window_type_id);
            }
        }
        record
            .data
            .window_types
            .iter()
            .filter(|t| used.contains(&t.id))
            .map(|t| {
                let name = t.specifics.name().map(str::to_owned);
                ExistingWindowType {
                    id: t.id,
                    glazing: t.glazing.clone(),
                    frame: t.frame.clone(),
                    year: t.year,
                    certified: t.certified,
                    gap: t.gap.clone(),
                    name: name.clone(),
                    label: window_type_label(
                        &t.glazing,
                        &t.frame,
                        t.year,
                        t.certified,
                        &t.gap,
                        name.as_deref(),
                    ),
                }
            })
            .collect()
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
        if !matches!(pending.target, PendingOpeningTarget::Wall { .. }) {
            self.pending_opening = Some(pending);
            return Ok(()); // walls only
        }
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        self.materialize();
        // Re-derive the live wall id from the durable anchor (resolve re-mints).
        let PendingOpeningTarget::Wall {
            wall_id, along_m, ..
        } = self.rehome_pending_target(idx, pending.target)
        else {
            self.pending_opening = Some(pending);
            return Ok(());
        };
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
        let type_id = record
            .data
            .ensure_window_type(glazing, frame, year, false, Vec::new(), None);
        let storey = &mut record.data.storeys[idx];
        let result = openings::add_full_wall_window(storey, wall_id, height_m, type_id);
        self.finish_opening(result.map(|_| ()), undo_snap, pending)
    }

    pub fn cancel_pending_opening(&mut self) {
        self.pending_opening = None;
    }

    // ── multi-field placement (floating box + laser) ──────────────────────────

    /// The measurement fields the floating box should collect for the in-flight
    /// placement (width + height together; door+sidelights adds optional lights),
    /// or `None` when no placement is pending. Window type is defaulted, so no
    /// glazing/frame/year fields — the assessor refines those in Edit on surface.
    pub fn opening_fields_prompt(
        &self,
        ds_mode: DoorSidelightsMode,
        cx_kind: ComplexWindowKind,
    ) -> Option<OpeningFieldsPrompt> {
        let p = self.pending_opening.as_ref()?;
        let req = |label: &str, hint| OpeningFieldSpec {
            label: label.to_string(),
            hint,
            required: true,
        };
        let opt = |label: &str, hint| OpeningFieldSpec {
            label: label.to_string(),
            hint,
            required: false,
        };
        let is_ds = p.tool == OpeningToolKind::DoorSidelights;
        let is_cx = p.tool == OpeningToolKind::ComplexWindow;
        // The field list is the positional binding contract with the UI +
        // `place_opening`. Fields 0/1 are always the door leaf's W/H (in Overall
        // mode the whole-doorset W/H come first, so the door leaf shifts to 2/3 —
        // the UI rebinds by mode). See `DoorSidelightsPlacement`. A complex window
        // draws its fields from the chosen shape kind.
        let fields = if is_cx {
            cx_kind
                .field_labels()
                .iter()
                .map(|(label, hint)| req(label, hint))
                .collect()
        } else {
            match (is_ds, ds_mode) {
                (true, DoorSidelightsMode::Overall) => vec![
                    req("overall width (m)", "whole doorset"),
                    req("overall height (m)", "whole doorset"),
                    req("door width (m)", "the leaf"),
                    req("door height (m)", "the leaf"),
                    opt("sill (m)", "0 = floor"),
                ],
                (true, DoorSidelightsMode::Detailed) => vec![
                    req("door width (m)", "e.g. 0.9"),
                    req("door height (m)", "e.g. 2.0"),
                    opt("left light (m)", "0 = none"),
                    opt("left sill (m)", "0 = floor"),
                    opt("right light (m)", "0 = none"),
                    opt("right sill (m)", "0 = floor"),
                    opt("transom (m)", "0 = none"),
                ],
                (false, _) => vec![req("width (m)", "e.g. 1.2"), req("height (m)", "e.g. 1.4")],
            }
        };
        let offer_full_surface = p.tool == OpeningToolKind::Window
            && matches!(p.target, PendingOpeningTarget::Wall { .. })
            && self.pending_wall_empty();
        let offer_window_type = matches!(
            p.tool,
            OpeningToolKind::Window
                | OpeningToolKind::MultiStoreyWindow
                | OpeningToolKind::RoofWindow
                | OpeningToolKind::DoorSidelights
                | OpeningToolKind::ComplexWindow
        );
        Some(OpeningFieldsPrompt {
            title: format!("{} · size", tool_label(p.tool)),
            fields,
            offer_full_surface,
            door_sidelights: is_ds.then_some(ds_mode),
            complex_window: is_cx.then_some(cx_kind),
            offer_window_type,
            seq: self.placement_seq,
        })
    }

    /// The placement inline error (wall too short, …), for the floating box.
    pub fn opening_dim_error(&self) -> Option<&str> {
        self.opening_dim_error.as_deref()
    }

    /// True when the pending placement's host wall has no openings yet.
    fn pending_wall_empty(&self) -> bool {
        let Some(PendingOpeningTarget::Wall { wall_id, .. }) =
            self.pending_opening.as_ref().map(|p| p.target)
        else {
            return false;
        };
        self.current_storey_idx
            .and_then(|idx| self.current_record.as_ref()?.data.storeys.get(idx))
            .map(|s| openings::wall_openings(s, wall_id).is_empty())
            .unwrap_or(false)
    }

    /// Abandon the in-flight placement.
    pub fn cancel_opening_placement(&mut self) {
        self.pending_opening = None;
        self.opening_dim_error = None;
    }

    /// Place the pending opening from the floating box's measurements, with the
    /// window type the box's picker resolved to (`window_type` — an existing
    /// shared type or new attributes, deduped) and a DEFAULT 1 m sill (moved
    /// later in the 2D elevation). `width_m`/`height_m` are the opening (for
    /// door+sidelights, the DOOR leaf); `door_sidelights` carries the mode's
    /// extra measurements (sidelights + transom), and `complex_window` the chosen
    /// shape + dims — each `None` outside its own tool. On a model error the box
    /// stays open with the error shown.
    #[allow(clippy::too_many_arguments)]
    pub fn place_opening(
        &mut self,
        width_m: f32,
        height_m: f32,
        door_sidelights: Option<DoorSidelightsPlacement>,
        complex_window: Option<ComplexWindowPlacement>,
        window_type: WindowTypeChoice,
        overshading: Overshading,
        photos: Vec<(std::path::PathBuf, PhotoQuality)>,
    ) -> Result<(), String> {
        /// Sill the placement defaults to; the assessor drags it in the 2D view.
        const DEFAULT_SILL_M: f32 = 1.0;
        let Some(pending) = self.pending_opening.take() else {
            return Ok(());
        };
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        self.materialize();
        // Re-home the target against the just-resolved storey (resolve re-mints
        // wall + ceiling ids), so the ops below hit the LIVE host, not the stale
        // tap-time id (which would error `NoHostRegion`).
        let target = self.rehome_pending_target(idx, pending.target);
        let undo_snap = self.current_history_snapshot();
        // Store the picked photos into the photo store up front (needs `&self`,
        // before the mutable record borrow below); attached to the new window(s).
        // Each photo carries its own capture tier (dual hi/lo buttons).
        let stored_photos: Vec<Photo> = photos
            .iter()
            .filter_map(|(p, quality)| self.ingest_photo(p, *quality))
            .collect();
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        // A plain solid Door carries no window type — don't touch the type
        // registry for it (the dummy id is never read by the Door arm).
        let type_id = if matches!(pending.tool, OpeningToolKind::Door) {
            ItemId::default()
        } else {
            resolve_window_type(&mut record.data, window_type)
        };
        // Per-window overshading is applied after the add to whatever window(s)
        // this placement appends (one window, or several for door+sidelights).
        let windows_before = record.data.storeys[idx].windows.len();
        let storey = &mut record.data.storeys[idx];
        let result: Result<(), OpeningError> = match (pending.tool, target) {
            (
                OpeningToolKind::Window,
                PendingOpeningTarget::Wall {
                    wall_id, along_m, ..
                },
            ) => openings::add_wall_window(
                storey,
                wall_id,
                along_m,
                width_m,
                height_m,
                DEFAULT_SILL_M,
                type_id,
            )
            .map(|_| ()),
            (
                OpeningToolKind::MultiStoreyWindow,
                PendingOpeningTarget::Wall {
                    wall_id, along_m, ..
                },
            ) => openings::add_multi_storey_window(
                storey,
                wall_id,
                along_m,
                width_m,
                height_m,
                DEFAULT_SILL_M,
                type_id,
            )
            .map(|_| ()),
            (OpeningToolKind::RoofWindow, PendingOpeningTarget::Roof { region_id, plan }) => {
                openings::add_roof_window(storey, region_id, plan, width_m, height_m, type_id)
                    .map(|_| ())
            }
            (
                OpeningToolKind::ComplexWindow,
                PendingOpeningTarget::Wall {
                    wall_id, along_m, ..
                },
            ) => {
                // The shape + its bounding box come from the chosen kind + dims.
                let (w, h, shape) = complex_window
                    .as_ref()
                    .map(|c| window_shape_from_inputs(c.kind, &c.dims))
                    .unwrap_or((width_m, height_m, WindowShape::Rectangular));
                openings::add_shaped_wall_window(
                    storey,
                    wall_id,
                    along_m,
                    w,
                    h,
                    DEFAULT_SILL_M,
                    type_id,
                    shape,
                )
                .map(|_| ())
            }
            (
                OpeningToolKind::Door,
                PendingOpeningTarget::Wall {
                    wall_id, along_m, ..
                },
            ) => openings::add_wall_door(
                storey,
                wall_id,
                along_m,
                width_m,
                height_m,
                DoorKind::Solid,
                "Door".to_string(),
                openings::DEFAULT_DOOR_SILL_M,
            )
            .map(|_| ()),
            (
                OpeningToolKind::DoorSidelights,
                PendingOpeningTarget::Wall {
                    wall_id, along_m, ..
                },
            ) => {
                // Both modes produce a spec; Overall derives the sidelights/transom
                // from the whole-doorset measurements, Detailed takes them directly.
                let spec = match door_sidelights.unwrap_or(DoorSidelightsPlacement::Detailed {
                    left: None,
                    left_sill: None,
                    right: None,
                    right_sill: None,
                    transom: None,
                    transom_semicircular: false,
                }) {
                    DoorSidelightsPlacement::Overall {
                        overall_w,
                        overall_h,
                        layout,
                        sill,
                        transom_semicircular,
                    } => DoorSidelightsSpec::from_overall(
                        overall_w,
                        overall_h,
                        width_m,
                        height_m,
                        layout,
                        sill,
                        transom_semicircular,
                        type_id,
                        "Door".to_string(),
                    ),
                    DoorSidelightsPlacement::Detailed {
                        left,
                        left_sill,
                        right,
                        right_sill,
                        transom,
                        transom_semicircular,
                    } => DoorSidelightsSpec {
                        door_width_m: width_m,
                        door_height_m: height_m,
                        left_sidelight_m: left,
                        right_sidelight_m: right,
                        left_sill_m: left_sill,
                        right_sill_m: right_sill,
                        transom_m: transom,
                        transom_semicircular,
                        window_type_id: type_id,
                        door_type_name: "Door".to_string(),
                    },
                };
                openings::compose_door_sidelights(storey, wall_id, along_m, spec).map(|_| ())
            }
            _ => return Ok(()), // tool/target mismatch — nothing to place
        };
        if result.is_ok() {
            for w in record.data.storeys[idx].windows[windows_before..].iter_mut() {
                w.overshading = overshading;
            }
            // Photos belong to the GROUP (window type), shared by all its
            // windows — append to the resolved type (none for a plain Door).
            if !stored_photos.is_empty() {
                if let Some(t) = record
                    .data
                    .window_types
                    .iter_mut()
                    .find(|t| t.id == type_id)
                {
                    t.photos.extend(stored_photos.iter().cloned());
                }
            }
        }
        match result {
            Ok(()) => {
                self.reseed_opening_anchors(idx);
                let _ = self.mark_dirty();
                self.push_history_snapshot(undo_snap);
                self.opening_dim_error = None;
                Ok(())
            }
            Err(e) => {
                discard_stored_photos(&stored_photos); // placement failed — no leak on retry
                self.pending_opening = Some(pending); // keep the box open
                let msg = opening_error_msg(e);
                self.opening_dim_error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Full-surface "window wall" shortcut: no measurements, the whole face
    /// becomes a window. Walls only.
    pub fn fill_opening_surface(
        &mut self,
        window_type: WindowTypeChoice,
        overshading: Overshading,
        photos: Vec<(std::path::PathBuf, PhotoQuality)>,
    ) -> Result<(), String> {
        let Some(pending) = self.pending_opening.take() else {
            return Ok(());
        };
        if !matches!(pending.target, PendingOpeningTarget::Wall { .. }) {
            self.pending_opening = Some(pending);
            return Ok(());
        }
        let Some(idx) = self.current_storey_idx else {
            return Ok(());
        };
        self.materialize();
        // Re-derive the live wall id from the durable anchor (resolve re-mints).
        let PendingOpeningTarget::Wall {
            wall_id, along_m, ..
        } = self.rehome_pending_target(idx, pending.target)
        else {
            self.pending_opening = Some(pending);
            return Ok(());
        };
        let undo_snap = self.current_history_snapshot();
        let stored_photos: Vec<Photo> = photos
            .iter()
            .filter_map(|(p, quality)| self.ingest_photo(p, *quality))
            .collect();
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
        let type_id = resolve_window_type(&mut record.data, window_type);
        let windows_before = record.data.storeys[idx].windows.len();
        let storey = &mut record.data.storeys[idx];
        match openings::add_full_wall_window(storey, wall_id, height_m, type_id) {
            Ok(_) => {
                for w in record.data.storeys[idx].windows[windows_before..].iter_mut() {
                    w.overshading = overshading;
                }
                // Photos belong to the GROUP (window type), shared by its windows.
                if !stored_photos.is_empty() {
                    if let Some(t) = record
                        .data
                        .window_types
                        .iter_mut()
                        .find(|t| t.id == type_id)
                    {
                        t.photos.extend(stored_photos.iter().cloned());
                    }
                }
                self.reseed_opening_anchors(idx);
                let _ = self.mark_dirty();
                self.push_history_snapshot(undo_snap);
                self.opening_dim_error = None;
                Ok(())
            }
            Err(e) => {
                discard_stored_photos(&stored_photos); // placement failed — no leak on retry
                self.pending_opening = Some(pending);
                let msg = opening_error_msg(e);
                self.opening_dim_error = Some(msg.clone());
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
        // Re-derive the live host from the durable anchor / plan point (resolve
        // re-mints wall + ceiling ids), so the ops below hit the LIVE host.
        let target = self.rehome_pending_target(idx, pending.target);
        let undo_snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        let type_id = record
            .data
            .ensure_window_type(glazing, frame, year, false, Vec::new(), None);
        let storey = &mut record.data.storeys[idx];
        let result = match target {
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
        if !matches!(pending.target, PendingOpeningTarget::Wall { .. }) {
            return Ok(()); // doors only go on walls
        }
        self.materialize();
        // Re-derive the live wall id from the durable anchor (resolve re-mints).
        let PendingOpeningTarget::Wall {
            wall_id, along_m, ..
        } = self.rehome_pending_target(idx, pending.target)
        else {
            return Ok(());
        };
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
        if !matches!(pending.target, PendingOpeningTarget::Wall { .. }) {
            return Ok(());
        }
        self.materialize();
        // Re-derive the live wall id from the durable anchor (resolve re-mints).
        let PendingOpeningTarget::Wall {
            wall_id, along_m, ..
        } = self.rehome_pending_target(idx, pending.target)
        else {
            return Ok(());
        };
        let undo_snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        let window_type_id =
            record
                .data
                .ensure_window_type(glazing, frame, year, false, Vec::new(), None);
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
                left_sill_m: None,
                right_sill_m: None,
                transom_m: transom,
                transom_semicircular: false,
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
    /// Re-seed EVERY storey's opening anchors from its saved walls — the
    /// load-time heal for the VertexId RE-INDEXING a snapshot-less reload causes
    /// (the outline is rebuilt via `from_clean_ring`, re-minting perimeter
    /// VertexIds in the reconstructed footprint's order, so persisted anchors
    /// name the wrong walls and every opening re-homes wrong). Both load paths
    /// call this AFTER the outlines are rebuilt and BEFORE `refresh_all_storeys`
    /// re-mints the walls (while the saved `wall_id`s the placements reference are
    /// still live). Idempotent — a no-op on the snapshot path (VertexIds intact).
    pub(super) fn reseed_all_opening_anchors(&mut self) {
        let n = self
            .current_record
            .as_ref()
            .map_or(0, |r| r.data.storeys.len());
        for i in 0..n {
            self.reseed_opening_anchors(i);
        }
    }

    pub(super) fn reseed_opening_anchors(&mut self, idx: usize) {
        // Snapshot the outline plan first (releases the &document borrow before
        // the &mut current_record one), then re-derive each opening's anchor from
        // its host wall against that plan — the model mutation lives in `document`.
        let Some(plan) = self
            .document
            .storeys
            .get(idx)
            .map(|sd| sd.outline.floor_plan.clone())
        else {
            return;
        };
        if let Some(storey) = self
            .current_record
            .as_mut()
            .and_then(|r| r.data.storeys.get_mut(idx))
        {
            document::reseed_wall_anchors_on_load(&plan, storey);
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
            .map(|(r, kind, along, sill, w, h, shape)| ElevationOpening {
                kind,
                along_m: along,
                sill_m: sill,
                width_m: w,
                height_m: h,
                selected: self.elevation_selection == Some(r),
                shape,
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

    /// Every wall opening on `wall_id`: `(ref, kind, along, sill, width, height,
    /// shape)`. The model owns the gather ([`openings::wall_openings`]); the
    /// shell only maps `is_door` + assembly membership to its own view kind.
    #[allow(clippy::type_complexity)]
    fn wall_openings(
        &self,
        storey: &Storey,
        wall_id: ItemId,
    ) -> Vec<(OpeningRef, OpeningViewKind, f32, f32, f32, f32, WindowShape)> {
        openings::wall_openings(storey, wall_id)
            .into_iter()
            .map(|o| {
                let kind = match (o.is_door, o.assembly_id.is_some()) {
                    (false, false) => OpeningViewKind::Window,
                    (false, true) => OpeningViewKind::SidelightWindow,
                    (true, false) => OpeningViewKind::Door,
                    (true, true) => OpeningViewKind::DoorPanel,
                };
                (
                    o.opening, kind, o.along_m, o.sill_m, o.width_m, o.height_m, o.shape,
                )
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
        OpeningToolKind::ComplexWindow => "Complex window",
        OpeningToolKind::Edit => "Edit",
    }
}

/// Remove photo files copied into the store for a placement that then failed —
/// so a retry doesn't leave orphaned images (each `ingest_photo` mints fresh
/// files). Best-effort; missing files are ignored. A still-running background
/// job for one of these finishes to a now-missing photo and cleans up its own
/// produced files (see `apply_finished_photos`).
fn discard_stored_photos(photos: &[Photo]) {
    for p in photos {
        let _ = std::fs::remove_file(&p.file_path);
        if let Some(t) = &p.thumbnail_path {
            let _ = std::fs::remove_file(t);
        }
    }
}

/// A short label for a window type, e.g.
/// `"North bay — Double · Wood/UPVC · 16mm · 2010 · Cert"`. A name (model) is
/// prefixed; year `0` (unknown) and an empty gap are omitted.
fn window_type_label(
    glazing: &Glazing,
    frame: &Frame,
    year: u32,
    certified: bool,
    gap: &[u32],
    name: Option<&str>,
) -> String {
    let g = match glazing {
        Glazing::Single => "Single".to_string(),
        Glazing::Double => "Double".to_string(),
        Glazing::Triple => "Triple".to_string(),
        Glazing::Other(s) => s.clone(),
    };
    let f = match frame {
        Frame::WoodUPVC => "Wood/UPVC".to_string(),
        Frame::Metal => "Metal".to_string(),
        Frame::Other(s) => s.clone(),
    };
    let mut label = format!("{g} · {f}");
    if !gap.is_empty() {
        let gaps = gap
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("/");
        label.push_str(&format!(" · {gaps}mm"));
    }
    if year != 0 {
        label.push_str(&format!(" · {year}"));
    }
    label.push_str(if certified { " · Cert" } else { " · No cert" });
    match name {
        Some(n) if !n.trim().is_empty() => format!("{} — {label}", n.trim()),
        _ => label,
    }
}

/// Resolve a [`WindowTypeChoice`] to a concrete shared-type id on the record: an
/// existing id (verified to still exist), or a new attribute set find-or-created
/// via [`Ber::ensure_window_type`] (which dedups, so a "new" type that matches an
/// existing one joins it — the "only add if different" rule).
fn resolve_window_type(data: &mut Ber, choice: WindowTypeChoice) -> ItemId {
    match choice {
        WindowTypeChoice::Existing(id) if data.window_types.iter().any(|t| t.id == id) => id,
        // Stale id (type since deleted) → fall back to the default new type.
        WindowTypeChoice::Existing(_) => {
            data.ensure_window_type(Glazing::Double, Frame::WoodUPVC, 0, false, Vec::new(), None)
        }
        WindowTypeChoice::New {
            glazing,
            frame,
            year,
            certified,
            gap,
            name,
        } => data.ensure_window_type(glazing, frame, year, certified, gap, name),
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
