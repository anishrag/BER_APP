//! The Sketch toolset as CATEGORY DATA — defined once — plus the two
//! renderers over it:
//!
//! - **Portrait** (available height > width): a top RIBBON — a category tab
//!   strip with the active category's tools in a row beneath it.
//! - **Landscape**: a left SIDE PANEL listing every category as a
//!   collapsible group.
//!
//! The buttons emit exactly the events the old single-row toolbar did, so the
//! mode toggles (living area / construction year / exposure / floor steps /
//! ceiling steps) keep their controller-enforced exclusive-mode semantics.

use super::{structure, SketchEvent, SketchView};
use crate::controller::app_controller::{AssessmentPhase, SelectMode, StructureEdit, SurfaceKind};
use crate::number_entry::parse_dimension_expression;
use crate::ui::pages::events::UiEvent;
use crate::ui::widgets::{metre_input, HintExt};
use ber_model::ber::common::ConstructionPhase;
use ber_model::ber::structural::SurfaceCounts;
use domain::sketch::EditTools;

/// One tool/toggle slot inside a category.
#[derive(Clone, Copy)]
pub enum Tool {
    /// Exclusive edit-tool selection (the sketch engine's active tool).
    EditTool(EditTools),
    /// Living-area drawing mode toggle (+ a delete button for the active area).
    LivingArea,
    /// Construction-year (phase-region) tagging mode toggle.
    ConstructionYear,
    /// Wall-exposure painting mode toggle.
    Exposure,
    /// Floor-exposure tool toggle (all-storeys overlaid floor view + select).
    FloorExposure,
    /// Ceiling-exposure tool toggle (all-storeys overlaid ceiling view + select).
    CeilingExposure,
    /// The storey's default ceiling height: a metres field + "Set", showing
    /// the current value when one is set. Re-entering overwrites; on the All
    /// tab one value fills every still-unset storey.
    DefaultCeilingHeight,
    /// The storey's floor-slab thickness to the storey below: a metres field
    /// + "Set", showing the current value when one is set. Upper storeys
    /// only (storey 0 has no slab over another storey) — disabled elsewhere.
    FloorThickness,
    /// Floor-step tagging mode toggle.
    FloorSteps,
    /// Ceiling-step tagging mode toggle (gated on a default ceiling height).
    CeilingSteps,
    /// Roof-point (apex) tagging mode toggle (same gate as the ceiling steps).
    RoofPoints,
    /// Windows/doors placement mode toggle (place openings from the plan, then
    /// reposition them in a per-wall elevation).
    Openings,
    Undo,
    Redo,
    /// Reset the view to pure auto-fit.
    FitView,
    /// Wall-length label visibility — pure page state, no controller event.
    WallLengthLabels,
    /// DEV: save the current plan to the checkpoint file.
    SaveCheckpoint,
    /// DEV: load the checkpoint file, replacing the current sketch session.
    LoadCheckpoint,
    /// DEV: dump the roof/wall derivation state to stdout + clipboard.
    DebugDump,
    /// Read-only autosave status line ("saved Ns ago").
    AutosaveStatus,
}

pub struct Category {
    pub name: &'static str,
    pub tools: &'static [Tool],
}

/// The single source of truth for the Sketch toolset. Both renderers walk
/// this list.
///
/// - **Dimensions** has no collinearity-review button: review prompts are
///   flow-driven (they appear in the prompt dock as suggestions arise after a
///   stroke), so there's nothing to launch manually.
pub const CATEGORIES: &[Category] = &[
    Category {
        name: "Edit",
        tools: &[
            Tool::EditTool(EditTools::DrawPerpWalls),
            Tool::EditTool(EditTools::AddDetachedSection),
            Tool::EditTool(EditTools::Move),
            Tool::EditTool(EditTools::ExtendOrtho),
            Tool::EditTool(EditTools::ExtendCurve),
            Tool::EditTool(EditTools::Trim),
            Tool::EditTool(EditTools::TrimOrtho),
            Tool::EditTool(EditTools::TrimCurve),
        ],
    },
    Category {
        name: "Area",
        tools: &[
            Tool::LivingArea,
            Tool::ConstructionYear,
            Tool::Exposure,
            Tool::FloorExposure,
            Tool::CeilingExposure,
        ],
    },
    Category {
        name: "Heights",
        tools: &[
            Tool::DefaultCeilingHeight,
            Tool::FloorThickness,
            Tool::FloorSteps,
            Tool::CeilingSteps,
            Tool::RoofPoints,
        ],
    },
    Category {
        name: "Openings",
        tools: &[Tool::Openings],
    },
    Category {
        name: "Dimensions",
        tools: &[Tool::WallLengthLabels],
    },
];

/// Tools that apply in EVERY mode, so they live in a persistent strip at the
/// bottom of the toolbar (both layouts) rather than behind a category tab:
/// the view controls, then the dev/State group.
const BOTTOM_VIEW_TOOLS: &[Tool] = &[Tool::Undo, Tool::Redo, Tool::FitView];
const BOTTOM_STATE_TOOLS: &[Tool] = &[
    Tool::SaveCheckpoint,
    Tool::LoadCheckpoint,
    Tool::DebugDump,
    Tool::AutosaveStatus,
];

/// A forced category-layout arrangement for the layout-preview button
/// (`None` on the override = Auto: pick by the canvas aspect ratio).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LayoutOrientation {
    /// Top ribbon (the portrait arrangement).
    Portrait,
    /// Left side panel (the landscape arrangement).
    Landscape,
}

/// Cross-frame UI state of the category layout. Lives on `SketchState`.
pub struct CategoryUiState {
    /// Which category tab the portrait ribbon shows. (The landscape side
    /// panel shows all categories, so it ignores this.)
    pub active_category: usize,
    /// Layout-preview override: force the portrait ribbon or landscape side
    /// panel regardless of aspect ratio; `None` = Auto. A preview/dev
    /// affordance the user requested — cycled by the top-strip "Layout"
    /// button so both arrangements can be tried on one device.
    pub layout_override: Option<LayoutOrientation>,
    /// Wall-length label visibility (the Dimensions-category toggle). Gates
    /// the storey + living-area length labels on the canvas.
    pub show_wall_lengths: bool,
    /// Backing string for the Heights category's default-ceiling-height
    /// field. Cleared by the shell when a Set succeeds (the field then shows
    /// the stored value as its label); kept on a rejected value so the user
    /// can correct it.
    pub ceiling_height_input: String,
    /// Backing string for the Heights category's floor-slab-thickness field.
    /// Same clear-on-success lifecycle as `ceiling_height_input`.
    pub floor_thickness_input: String,
    /// How the number-entry box takes input: the on-screen keypad or a paired
    /// Bluetooth laser meter. Persists across prompts within the session.
    pub input_mode: super::number_box::InputMode,
    /// Set once the assessor picks a mode by hand. Until then the box may
    /// auto-switch to Laser when a connected DISTO is detected; after, their
    /// choice is respected.
    pub input_mode_pinned: bool,
}

impl Default for CategoryUiState {
    fn default() -> Self {
        Self {
            active_category: 0,
            layout_override: None,   // Auto: pick by aspect ratio
            show_wall_lengths: true, // labels visible by default
            ceiling_height_input: String::new(),
            floor_thickness_input: String::new(),
            input_mode: super::number_box::InputMode::default(),
            input_mode_pinned: false,
        }
    }
}

/// Owned snapshot of everything the tool buttons need to draw themselves
/// (selected/enabled states + the status text). Copied out of [`SketchView`]
/// so the panel closures don't hold a borrow of it.
pub struct ToolFlags {
    pub edit_tool: EditTools,
    /// The All-storeys tab is read-only: every mutating tool is disabled,
    /// matching the old toolbar which offered only Fit there.
    pub read_only: bool,
    /// An edit-drag / vertex-drop measurement prompt is docked. Switching edit
    /// tool would wipe the pre-drag state its Cancel reverts to, so every
    /// Edit-category tool is disabled until the prompt is resolved.
    pub measurement_prompt_active: bool,
    pub has_floor_plan: bool,
    pub living_area_mode: bool,
    pub active_living_area: Option<usize>,
    pub exposure_mode: bool,
    /// The floor-exposure tool is active (all-storeys overlaid floor view).
    pub floor_exposure_mode: bool,
    /// The ceiling-exposure tool is active (all-storeys overlaid ceiling view).
    pub ceiling_exposure_mode: bool,
    pub phase_mode: bool,
    /// Construction years exist (entered in Key Details) — without them the
    /// phase picker would be empty, so the toggle is disabled.
    pub has_phase_choices: bool,
    pub step_mode: bool,
    pub ceiling_step_mode: bool,
    pub ceiling_step_available: bool,
    pub roof_point_mode: bool,
    pub roof_point_available: bool,
    pub openings_mode: bool,
    /// The current scope's default ceiling height, when set — the Heights
    /// field shows it ("2.40 m") next to the entry box.
    pub default_ceiling_height: Option<f64>,
    /// Last error from the Heights field's Set, shown inline beside it.
    pub ceiling_height_error: Option<String>,
    /// On an UPPER storey (index ≥ 1): the only place a floor-slab
    /// thickness applies, so the Heights thickness field is enabled.
    pub upper_storey: bool,
    /// The current storey's floor-slab thickness, when set — the Heights
    /// thickness field shows it ("0.30 m") next to the entry box.
    pub floor_thickness: Option<f64>,
    /// Last error from the thickness field's Set, shown inline beside it.
    pub floor_thickness_error: Option<String>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub autosave_status: Option<String>,
    /// The current assessment phase — selects which toolset the panel shows
    /// (Mapping categories vs. the Structure surface panel).
    pub phase: AssessmentPhase,
    /// The active Structure-phase MODES (Walls/Roofs/Floors; non-exclusive) —
    /// drives which mode buttons read as selected.
    pub structure_kinds: std::collections::BTreeSet<SurfaceKind>,
    /// The active global Select subtool — drives which reads as selected.
    pub structure_select: Option<SelectMode>,
    /// The in-flight "Edit attributes" session (selection grouped by
    /// construction), so the panel renders the group editor. `None` = none.
    pub structure_edit: Option<StructureEdit>,
    /// Current storey's wall / roof / floor counts — the Structure panel's
    /// per-surface read-out.
    pub surface_counts: SurfaceCounts,
    /// The defined construction phases (Original + extensions) — the choices for
    /// the Structure attribute editor's phase dropdown.
    pub phase_choices: Vec<(ConstructionPhase, String)>,
}

impl ToolFlags {
    pub fn from_view(view: &SketchView<'_>) -> Self {
        Self {
            edit_tool: view.edit_tool,
            read_only: view.current_storey_idx.is_none(),
            measurement_prompt_active: view.edit_drag_prompt.is_some()
                || view.vertex_drop_prompt.is_some(),
            has_floor_plan: view.has_floor_plan,
            living_area_mode: view.living_area_mode,
            active_living_area: view.active_living_area,
            exposure_mode: view.exposure_mode,
            floor_exposure_mode: view.floor_exposure_mode,
            ceiling_exposure_mode: view.ceiling_exposure_mode,
            phase_mode: view.phase_mode,
            has_phase_choices: !view.phase_choices.is_empty(),
            step_mode: view.step_mode,
            ceiling_step_mode: view.ceiling_step_mode,
            ceiling_step_available: view.ceiling_step_available,
            roof_point_mode: view.roof_point_mode,
            roof_point_available: view.roof_point_available,
            openings_mode: view.openings_mode,
            default_ceiling_height: view.default_ceiling_height,
            ceiling_height_error: view.ceiling_height_error.map(str::to_owned),
            upper_storey: matches!(view.current_storey_idx, Some(i) if i >= 1),
            floor_thickness: view.floor_thickness,
            floor_thickness_error: view.floor_thickness_error.map(str::to_owned),
            can_undo: view.can_undo,
            can_redo: view.can_redo,
            autosave_status: view.autosave_status.clone(),
            phase: view.current_phase,
            structure_kinds: view.structure_kinds.clone(),
            structure_select: view.structure_select,
            structure_edit: view.structure_edit.cloned(),
            surface_counts: view.surface_counts,
            phase_choices: view.phase_choices.to_vec(),
        }
    }
}

/// Portrait renderer: a category tab strip plus the active category's tools
/// in a row beneath it.
pub fn ribbon<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    flags: &ToolFlags,
    state: &mut CategoryUiState,
    on_event: &mut F,
) {
    // The Structure phase swaps the Mapping category tabs for the surface
    // inspector; the bottom strip (undo/redo/fit/dev) stays in both.
    if flags.phase == AssessmentPhase::Structure {
        structure::structure_panel(
            ui,
            &flags.structure_kinds,
            flags.structure_select,
            flags.structure_edit.as_ref(),
            flags.surface_counts,
            &flags.phase_choices,
            !flags.read_only,
            on_event,
        );
        ui.separator();
        bottom_tools(ui, flags, state, on_event);
        return;
    }
    ui.horizontal(|ui| {
        for (i, cat) in CATEGORIES.iter().enumerate() {
            if ui
                .selectable_label(state.active_category == i, cat.name)
                .clicked()
            {
                state.active_category = i;
            }
        }
    });
    let cat = &CATEGORIES[state.active_category.min(CATEGORIES.len() - 1)];
    ui.horizontal_wrapped(|ui| {
        for tool in cat.tools {
            tool_ui(ui, *tool, flags, state, on_event);
        }
    });
    ui.separator();
    bottom_tools(ui, flags, state, on_event);
}

/// Landscape renderer: every category as a collapsible group, all visible,
/// with the always-relevant bottom strip beneath them.
pub fn side_panel<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    flags: &ToolFlags,
    state: &mut CategoryUiState,
    on_event: &mut F,
) {
    if flags.phase == AssessmentPhase::Structure {
        structure::structure_panel(
            ui,
            &flags.structure_kinds,
            flags.structure_select,
            flags.structure_edit.as_ref(),
            flags.surface_counts,
            &flags.phase_choices,
            !flags.read_only,
            on_event,
        );
        ui.separator();
        bottom_tools(ui, flags, state, on_event);
        return;
    }
    for cat in CATEGORIES {
        egui::CollapsingHeader::new(cat.name)
            .default_open(true)
            .show(ui, |ui| {
                for tool in cat.tools {
                    tool_ui(ui, *tool, flags, state, on_event);
                }
            });
    }
    ui.separator();
    bottom_tools(ui, flags, state, on_event);
}

/// The always-relevant bottom strip, shared by both layouts: the view controls
/// (undo / redo / fit) on one row, the dev/State group on the next. Rendered
/// below the category content because these apply in every mode, so they
/// shouldn't hide behind a category tab.
fn bottom_tools<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    flags: &ToolFlags,
    state: &mut CategoryUiState,
    on_event: &mut F,
) {
    ui.horizontal_wrapped(|ui| {
        for tool in BOTTOM_VIEW_TOOLS {
            tool_ui(ui, *tool, flags, state, on_event);
        }
    });
    ui.horizontal_wrapped(|ui| {
        for tool in BOTTOM_STATE_TOOLS {
            tool_ui(ui, *tool, flags, state, on_event);
        }
    });
}

fn edit_tool_label(tool: EditTools) -> &'static str {
    match tool {
        EditTools::DrawPerpWalls => "Draw walls",
        EditTools::AddDetachedSection => "Add section",
        EditTools::Move => "Move",
        EditTools::ExtendOrtho => "Extend Ortho",
        EditTools::ExtendCurve => "Extend Curve",
        EditTools::Trim => "Trim",
        EditTools::TrimOrtho => "Trim Ortho",
        EditTools::TrimCurve => "Trim Curve",
    }
}

/// One tool slot. Emits the SAME events the old toolbar did; gating that used
/// to hide widgets (no floor plan, All-storeys tab, no construction years)
/// now disables them instead, so the layout stays stable.
fn tool_ui<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    tool: Tool,
    flags: &ToolFlags,
    state: &mut CategoryUiState,
    on_event: &mut F,
) {
    match tool {
        Tool::EditTool(t) => {
            // DrawPerpWalls is the prerequisite step — always available.
            // Every other tool operates on an existing shape and is gated
            // on `has_floor_plan` so the assessor can't pick them until
            // the orthogonal outline is drawn. While a measurement prompt
            // is docked, ALL edit tools (including Move, which created it)
            // are disabled: switching tools wipes the pre-drag state the
            // prompt's Cancel reverts to.
            let enabled = !flags.read_only
                && !flags.measurement_prompt_active
                && (matches!(t, EditTools::DrawPerpWalls) || flags.has_floor_plan);
            let selected = flags.edit_tool == t;
            let mut resp = ui.add_enabled(
                enabled,
                egui::Button::selectable(selected, edit_tool_label(t)),
            );
            if flags.measurement_prompt_active {
                resp = resp.disabled_hint("Finish or cancel the measurement first");
            }
            if resp.clicked() {
                on_event(UiEvent::Sketch(SketchEvent::ChangeEditTool(t)));
            }
        }
        Tool::LivingArea => {
            let enabled = !flags.read_only && flags.has_floor_plan;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(flags.living_area_mode, "🏠 Living areas"),
                )
                .hint(
                    "Draw potential living-space sub-areas inside the storey \
                     (drawn & dimensioned the same way; largest chosen later)",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetLivingAreaMode(
                    !flags.living_area_mode,
                )));
            }
            // In living-area mode, delete the active area (living areas are
            // non-editable — redraw to change).
            if flags.living_area_mode {
                if let Some(idx) = flags.active_living_area {
                    if ui
                        .button("🗑 Delete area")
                        .hint("Delete this living area (redraw to change one)")
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::DeleteLivingArea(idx)));
                    }
                }
            }
        }
        Tool::ConstructionYear => {
            let enabled = !flags.read_only && flags.has_floor_plan && flags.has_phase_choices;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(flags.phase_mode, "📅 Construction year"),
                )
                .hint(
                    "Tag an area with its year of construction: draw an \
                     area (like a living area), then pick the year",
                )
                .disabled_hint("Enter the construction years in Key Details first")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetPhaseMode(
                    !flags.phase_mode,
                )));
            }
        }
        Tool::Exposure => {
            let enabled = !flags.read_only && flags.has_floor_plan;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(flags.exposure_mode, "🌡 Exposure"),
                )
                .hint(
                    "Mark each storey wall's heat-loss exposure: tap a wall, \
                     then pick exposed / semi-exposed / non-heat-loss",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetExposureMode(
                    !flags.exposure_mode,
                )));
            }
        }
        Tool::FloorExposure => {
            // Spans the whole storey stack, so it's available whenever a floor
            // plan exists (independent of the current storey tab — it shows
            // every storey regardless). Enabled on the All tab too.
            let enabled = flags.has_floor_plan;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(flags.floor_exposure_mode, "▦ Floor exposure"),
                )
                .hint(
                    "Mark each floor's heat-loss exposure across every storey: \
                     confirm all storeys are added (runs the party auto-pass), \
                     then select floors and pick ground / exposed / semi / \
                     non-heat-loss",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetFloorExposureMode(
                    !flags.floor_exposure_mode,
                )));
            }
        }
        Tool::CeilingExposure => {
            // Spans the whole storey stack (independent of the current tab), so
            // it's available whenever a floor plan exists. Enabled on the All
            // tab too. Mirrors the floor-exposure tool.
            let enabled = flags.has_floor_plan;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(flags.ceiling_exposure_mode, "▤ Ceiling exposure"),
                )
                .hint(
                    "Mark each ceiling's heat-loss exposure across every storey: \
                     confirm all storeys are added (runs the party auto-pass), \
                     then select ceilings and pick exposed roof / party / unheated",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetCeilingExposureMode(
                    !flags.ceiling_exposure_mode,
                )));
            }
        }
        Tool::Openings => {
            let enabled = !flags.read_only && flags.has_floor_plan;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(flags.openings_mode, "🪟 Windows/Doors"),
                )
                .hint(
                    "Place windows, roof windows and doors: pick a type, tap near \
                     a wall to drop it, then enter its size. Tap a wall in Edit to \
                     reposition openings on its elevation.",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetOpeningsMode(
                    !flags.openings_mode,
                )));
            }
        }
        Tool::DefaultCeilingHeight => {
            // Always available (it's the prerequisite the 3D view and the
            // ceiling steps gate on). Works on the All tab too: there one
            // value fills every still-UNSET storey. Re-entering on a storey
            // overwrites its value; validation (> 0, finite) lives in the
            // controller and its rejection shows inline like the other
            // prompt errors.
            // Entry happens in the shared floating number box (keypad OR laser
            // — so the ceiling height can be measured with the Disto). This
            // button just shows the current value and opens that box.
            ui.horizontal(|ui| {
                ui.label("Default ceiling height");
                let current = flags
                    .default_ceiling_height
                    .map(|h| format!("{h:.2} m"))
                    .unwrap_or_else(|| "— not set".to_string());
                if ui
                    .button(format!("✎ {current}"))
                    .hint(
                        "Enter or measure the storey's default ceiling height (opens the \
                         floating entry; on the All tab: fill every storey that doesn't \
                         have one yet)",
                    )
                    .clicked()
                {
                    // Prefill with the current value (overtype or accept) — direct
                    // on the buffer we own here — then ask the shell to open the
                    // floating box (its open flag lives on `SketchState`).
                    state.ceiling_height_input = flags
                        .default_ceiling_height
                        .map(|h| format!("{h:.2}"))
                        .unwrap_or_default();
                    on_event(UiEvent::Sketch(SketchEvent::OpenCeilingHeightEntry));
                }
            });
            if let Some(err) = &flags.ceiling_height_error {
                ui.label(
                    egui::RichText::new(format!("⚠ {}", err))
                        .color(egui::Color32::from_rgb(255, 120, 90)),
                );
            }
        }
        Tool::FloorThickness => {
            // Upper storeys only — storey 0 sits on the ground, the All tab
            // has no single storey to stamp. Mirrors the ceiling-height
            // field: shows the stored value, re-entering overwrites,
            // validation (finite ≥ 0) lives in the controller and its
            // rejection shows inline. Disabled (not hidden) so the Heights
            // layout stays stable across storey tabs.
            ui.add_enabled_ui(flags.upper_storey, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Floor thickness");
                    if let Some(t) = flags.floor_thickness {
                        ui.label(format!("{t:.2} m"));
                    }
                    let submitted =
                        metre_input(ui, &mut state.floor_thickness_input, 50.0, "e.g. 0.3")
                            .submitted;
                    let set_clicked = ui
                        .button("Set")
                        .hint(
                            "Set the floor-slab thickness to the storey below \
                             (it sets this storey's floor level)",
                        )
                        .disabled_hint("Only an upper storey has a floor slab to the storey below")
                        .clicked();
                    if set_clicked || submitted {
                        if let Some(value) =
                            parse_dimension_expression(&state.floor_thickness_input)
                        {
                            on_event(UiEvent::Sketch(SketchEvent::SetFloorThickness(
                                value as f64,
                            )));
                        }
                    }
                });
            });
            if let Some(err) = &flags.floor_thickness_error {
                ui.label(
                    egui::RichText::new(format!("⚠ {}", err))
                        .color(egui::Color32::from_rgb(255, 120, 90)),
                );
            }
        }
        Tool::FloorSteps => {
            let enabled = !flags.read_only && flags.has_floor_plan;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(flags.step_mode, "↕ Floor steps"),
                )
                .hint(
                    "Mark a floor area as raised or lowered: draw an area \
                     (like a living area), then enter the step amount",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetStepMode(!flags.step_mode)));
            }
        }
        Tool::CeilingSteps => {
            // Disabled until the storey has a default ceiling height (the
            // deltas are relative to it) — the controller setter re-enforces
            // the gate.
            let enabled = !flags.read_only && flags.has_floor_plan && flags.ceiling_step_available;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(flags.ceiling_step_mode, "⬆ Ceiling steps"),
                )
                .hint(
                    "Raise or lower part of the ceiling: draw a straight line \
                     across the plan and tap the side that steps, or draw a \
                     closed area (like a floor step), then enter the amount",
                )
                .disabled_hint("Set the default ceiling height first (the Heights field above)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetCeilingStepMode(
                    !flags.ceiling_step_mode,
                )));
            }
        }
        Tool::RoofPoints => {
            // Same gate as the ceiling steps (a default ceiling height +
            // finalised outline — the apex fans a derived ceiling region);
            // the controller setter re-enforces it.
            let enabled = !flags.read_only && flags.has_floor_plan && flags.roof_point_available;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(flags.roof_point_mode, "⛰ Roof points"),
                )
                .hint(
                    "Slope part of the roof: tap a high (or low) point inside \
                     a ceiling area, or draw a straight stroke for a ridge, \
                     then enter the height",
                )
                .disabled_hint("Set the default ceiling height first (the Heights field above)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetRoofPointMode(
                    !flags.roof_point_mode,
                )));
            }
        }
        Tool::Undo => {
            if ui
                .add_enabled(
                    !flags.read_only && flags.can_undo,
                    egui::Button::new("↶ Undo"),
                )
                .hint("Undo (Ctrl+Z)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::Undo));
            }
        }
        Tool::Redo => {
            if ui
                .add_enabled(
                    !flags.read_only && flags.can_redo,
                    egui::Button::new("↷ Redo"),
                )
                .hint("Redo (Ctrl+Shift+Z or Ctrl+Y)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::Redo));
            }
        }
        Tool::FitView => {
            // Available on the All-storeys tab too (view-only, like before).
            if ui
                .button("Fit")
                .hint("Reset view (zoom/pan, and the 3D orbit angle)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::ResetView));
            }
        }
        Tool::WallLengthLabels => {
            // Pure page state — flips the canvas's label rendering directly,
            // no controller round-trip.
            ui.toggle_value(&mut state.show_wall_lengths, "📏 Wall lengths")
                .hint("Show the wall-length labels on the plan");
        }
        Tool::SaveCheckpoint => {
            if ui
                .add_enabled(!flags.read_only, egui::Button::new("📌 Save checkpoint"))
                .hint(
                    "DEV: save this plan as the checkpoint, reloaded on \
                     `BER_CHECKPOINT=1 cargo run` or via Load checkpoint",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SaveCheckpoint));
            }
        }
        Tool::LoadCheckpoint => {
            if ui
                .button("📂 Load checkpoint")
                .hint("DEV: load the saved checkpoint plan, replacing the current sketch session")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::LoadCheckpoint));
            }
        }
        Tool::DebugDump => {
            if ui
                .button("🐞 Debug dump")
                .hint(
                    "DEV: print the roof/wall derivation dump to the terminal \
                     and copy it to the clipboard",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::DebugDump));
            }
        }
        Tool::AutosaveStatus => {
            // Refreshes on repaint, so it can read a touch stale between
            // frames — fine for a passive status line.
            if let Some(status) = &flags.autosave_status {
                ui.weak(status);
            }
        }
    }
}
