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

use super::prompt_dock::parse_dimension_expression;
use super::{SketchEvent, SketchView};
use crate::ui::pages::events::UiEvent;
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
    /// The storey's default ceiling height: a metres field + "Set", showing
    /// the current value when one is set. Re-entering overwrites; on the All
    /// tab one value fills every still-unset storey.
    DefaultCeilingHeight,
    /// Floor-step tagging mode toggle.
    FloorSteps,
    /// Ceiling-step tagging mode toggle (gated on a default ceiling height).
    CeilingSteps,
    Undo,
    Redo,
    /// Reset the view to pure auto-fit.
    FitView,
    /// Replay the last drawn action with its elective snaps disabled.
    UnsnapLast,
    /// Wall-length label visibility — pure page state, no controller event.
    WallLengthLabels,
    /// DEV: save the current plan to the checkpoint file.
    SaveCheckpoint,
    /// DEV: load the checkpoint file, replacing the current sketch session.
    LoadCheckpoint,
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
        tools: &[Tool::LivingArea, Tool::ConstructionYear, Tool::Exposure],
    },
    Category {
        name: "Heights",
        tools: &[
            Tool::DefaultCeilingHeight,
            Tool::FloorSteps,
            Tool::CeilingSteps,
        ],
    },
    Category {
        name: "Drawing",
        tools: &[Tool::Undo, Tool::Redo, Tool::FitView, Tool::UnsnapLast],
    },
    Category {
        name: "Dimensions",
        tools: &[Tool::WallLengthLabels],
    },
    Category {
        name: "State",
        tools: &[
            Tool::SaveCheckpoint,
            Tool::LoadCheckpoint,
            Tool::AutosaveStatus,
        ],
    },
];

/// Cross-frame UI state of the category layout. Lives on `SketchState`.
pub struct CategoryUiState {
    /// Which category tab the portrait ribbon shows. (The landscape side
    /// panel shows all categories, so it ignores this.)
    pub active_category: usize,
    /// Wall-length label visibility (the Dimensions-category toggle). Gates
    /// the storey + living-area length labels on the canvas.
    pub show_wall_lengths: bool,
    /// Backing string for the Heights category's default-ceiling-height
    /// field. Cleared by the shell when a Set succeeds (the field then shows
    /// the stored value as its label); kept on a rejected value so the user
    /// can correct it.
    pub ceiling_height_input: String,
}

impl Default for CategoryUiState {
    fn default() -> Self {
        Self {
            active_category: 0,
            show_wall_lengths: true, // labels visible by default
            ceiling_height_input: String::new(),
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
    pub has_floor_plan: bool,
    pub living_area_mode: bool,
    pub active_living_area: Option<usize>,
    pub exposure_mode: bool,
    pub phase_mode: bool,
    /// Construction years exist (entered in Key Details) — without them the
    /// phase picker would be empty, so the toggle is disabled.
    pub has_phase_choices: bool,
    pub step_mode: bool,
    pub ceiling_step_mode: bool,
    pub ceiling_step_available: bool,
    /// The current scope's default ceiling height, when set — the Heights
    /// field shows it ("2.40 m") next to the entry box.
    pub default_ceiling_height: Option<f64>,
    /// Last error from the Heights field's Set, shown inline beside it.
    pub ceiling_height_error: Option<String>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub autosave_status: Option<String>,
}

impl ToolFlags {
    pub fn from_view(view: &SketchView<'_>) -> Self {
        Self {
            edit_tool: view.edit_tool,
            read_only: view.current_storey_idx.is_none(),
            has_floor_plan: view.has_floor_plan,
            living_area_mode: view.living_area_mode,
            active_living_area: view.active_living_area,
            exposure_mode: view.exposure_mode,
            phase_mode: view.phase_mode,
            has_phase_choices: !view.phase_choices.is_empty(),
            step_mode: view.step_mode,
            ceiling_step_mode: view.ceiling_step_mode,
            ceiling_step_available: view.ceiling_step_available,
            default_ceiling_height: view.default_ceiling_height,
            ceiling_height_error: view.ceiling_height_error.map(str::to_owned),
            can_undo: view.can_undo,
            can_redo: view.can_redo,
            autosave_status: view.autosave_status.clone(),
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
}

/// Landscape renderer: every category as a collapsible group, all visible.
pub fn side_panel<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    flags: &ToolFlags,
    state: &mut CategoryUiState,
    on_event: &mut F,
) {
    for cat in CATEGORIES {
        egui::CollapsingHeader::new(cat.name)
            .default_open(true)
            .show(ui, |ui| {
                for tool in cat.tools {
                    tool_ui(ui, *tool, flags, state, on_event);
                }
            });
    }
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
            // the orthogonal outline is drawn.
            let enabled =
                !flags.read_only && (matches!(t, EditTools::DrawPerpWalls) || flags.has_floor_plan);
            let selected = flags.edit_tool == t;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(selected, edit_tool_label(t)),
                )
                .clicked()
            {
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
                .on_hover_text(
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
                        .on_hover_text("Delete this living area (redraw to change one)")
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
                .on_hover_text(
                    "Tag an area with its year of construction: draw an \
                     area (like a living area), then pick the year",
                )
                .on_disabled_hover_text("Enter the construction years in Key Details first")
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
                .on_hover_text(
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
        Tool::DefaultCeilingHeight => {
            // Always available (it's the prerequisite the 3D view and the
            // ceiling steps gate on). Works on the All tab too: there one
            // value fills every still-UNSET storey. Re-entering on a storey
            // overwrites its value; validation (> 0, finite) lives in the
            // controller and its rejection shows inline like the other
            // prompt errors.
            ui.horizontal(|ui| {
                ui.label("Ceiling height");
                if let Some(h) = flags.default_ceiling_height {
                    ui.label(format!("{h:.2} m"));
                }
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.ceiling_height_input)
                        .desired_width(50.0)
                        .hint_text("e.g. 2.4"),
                );
                let submitted = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let set_clicked = ui
                    .button("Set")
                    .on_hover_text(
                        "Set the storey's default ceiling height (on the All tab: \
                         fill every storey that doesn't have one yet)",
                    )
                    .clicked();
                if set_clicked || submitted {
                    if let Some(value) = parse_dimension_expression(&state.ceiling_height_input) {
                        on_event(UiEvent::Sketch(SketchEvent::SetDefaultCeilingHeight(value)));
                    }
                }
            });
            if let Some(err) = &flags.ceiling_height_error {
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
                .on_hover_text(
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
                .on_hover_text(
                    "Raise or lower part of the ceiling: draw a straight line \
                     across the plan, tap the side that steps, then enter the amount",
                )
                .on_disabled_hover_text(
                    "Set the default ceiling height first (the Heights field above)",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetCeilingStepMode(
                    !flags.ceiling_step_mode,
                )));
            }
        }
        Tool::Undo => {
            if ui
                .add_enabled(
                    !flags.read_only && flags.can_undo,
                    egui::Button::new("↶ Undo"),
                )
                .on_hover_text("Undo (Ctrl+Z)")
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
                .on_hover_text("Redo (Ctrl+Shift+Z or Ctrl+Y)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::Redo));
            }
        }
        Tool::FitView => {
            // Available on the All-storeys tab too (view-only, like before).
            if ui
                .button("Fit")
                .on_hover_text("Reset view (zoom/pan)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::ResetView));
            }
        }
        Tool::UnsnapLast => {
            if ui
                .add_enabled(!flags.read_only, egui::Button::new("↩ Un-snap last"))
                .on_hover_text("Replay the last drawn action with its elective snaps disabled")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::UnsnapLast));
            }
        }
        Tool::WallLengthLabels => {
            // Pure page state — flips the canvas's label rendering directly,
            // no controller round-trip.
            ui.toggle_value(&mut state.show_wall_lengths, "📏 Wall lengths")
                .on_hover_text("Show the wall-length labels on the plan");
        }
        Tool::SaveCheckpoint => {
            if ui
                .add_enabled(!flags.read_only, egui::Button::new("📌 Save checkpoint"))
                .on_hover_text(
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
                .on_hover_text(
                    "DEV: load the saved checkpoint plan, replacing the current sketch session",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::LoadCheckpoint));
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
