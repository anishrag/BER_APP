use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;

use std::sync::mpsc::{channel, Receiver};

use crate::controller::app_controller::{AppController, AssessmentPhase};
use crate::ui::adapters::sketch_egui::pos2_to_point2f;
use crate::ui::pages::events::{Page, Section, UiEvent};
use crate::ui::pages::home::{HomeEvent, HomePage, HomeState};
use crate::ui::pages::key_details::{KeyDetailsEvent, KeyDetailsPage, KeyDetailsState};
use crate::ui::pages::sketch::{
    categories, touch, EguiStroke, SketchEvent, SketchPage, SketchView,
};
use crate::ui::pages::splash::SplashPage;
use crate::ui::pages::workspace::workspace_chrome;

/// Surface a controller call's error (if any) to stderr with a label. Used by
/// the event-dispatch arms whose only failure handling is to log.
fn log_err(label: &str, result: Result<(), String>) {
    if let Err(e) = result {
        log::error!("{label}: {e}");
    }
}

pub struct BerApp {
    pub current_page: Page,
    pub app_controller: Rc<RefCell<AppController>>,

    // Page-specific state
    pub splash_state: SplashState,
    pub home_state: HomeState,
    pub key_details_state: KeyDetailsState,
    pub sketch_state: SketchState,

    // Idle-autosave bookkeeping: the controller's edit `revision` last seen, and
    // the app-clock time it last changed. When the revision goes quiet for
    // `IDLE_SAVE_SECS`, we flush un-persisted edits to disk.
    last_seen_revision: u64,
    last_change_time: f64,
}

/// Idle gap (seconds of no edits) before an autosave flush fires.
const IDLE_SAVE_SECS: f64 = 2.5;

#[derive(Default)]
pub struct SketchState {
    /// Two-finger canvas-navigation state (pinch zoom / pan / stroke
    /// retraction) — cross-frame because a gesture spans frames.
    pub touch_nav: touch::TouchNav,
    /// Backing string for the dimension prompt's text input. Lives across
    /// frames so the user's in-progress typing isn't lost on re-render.
    pub dimension_input: String,
    /// Last error from `apply_dimension`. Shown next to the textbox so the
    /// user can correct without losing what they typed.
    pub dimension_error: Option<String>,
    /// Backing string for the position prompt's text input. PREFILLED with
    /// the prompt's current (drawn) distance whenever the prompt identity
    /// changes (see `position_prompt_key`); the user overtypes or accepts.
    pub position_input: String,
    /// Identity `(axis, kind, option_idx)` of the position prompt the input
    /// was last prefilled for. When the visible prompt's identity differs
    /// (next axis, next Wall param, cycled reference), the input is refilled
    /// with its drawn distance — `kind` is part of the identity because two
    /// consecutive Wall prompts can share the same axis and option index.
    /// `None` when no position prompt is showing.
    pub position_prompt_key: Option<(
        manip_plan::ParameterAxis,
        crate::controller::app_controller::PositionPromptKind,
        usize,
    )>,
    /// Last error from `apply_position_dimension`. Shown next to the textbox
    /// so the user can correct without losing what they typed.
    pub position_error: Option<String>,
    /// Backing string for the Edit-drag measurement prompt's input.
    pub edit_drag_input: String,
    /// Last error from `apply_edit_drag` (or the cancel/cycle paths).
    pub edit_drag_error: Option<String>,
    /// Backing strings for the vertex-drop prompt's up-to-4 text boxes.
    /// Keyed by measurement index (matches `VertexMeasurementUiData.idx`).
    pub vertex_drop_inputs: [String; 4],
    /// Last error from `apply_vertex_measurement`.
    pub vertex_drop_error: Option<String>,
    /// Backing strings for the exposure section's typed start/end inputs.
    pub exposure_start_input: String,
    pub exposure_end_input: String,
    /// Backing string for the wall-partition dimension prompt's metre input.
    pub partition_dim_input: String,
    /// Backing string for the floor-step amount input (metres).
    pub step_amount_input: String,
    // Error surfacing follows the established per-flow pattern (one
    // `Option<String>` per prompt panel — `dimension_error`, `position_error`,
    // etc.) rather than a single shared field: each error renders inside the
    // panel that owns the rejected input, and the step / ceiling-step / roof
    // toolbars can be on screen alongside other prompts. The roof family
    // (apex height, ridge length, ridge height) shares ONE field because it
    // shares one toolbar with mutually exclusive stages.
    /// Last error from `assign_step_level_to_pending`. Shown next to the
    /// amount field so the user can correct without losing what they typed;
    /// cleared on success and on cancel.
    pub step_error: Option<String>,
    /// Backing string for the ceiling-step amount input (metres).
    pub ceiling_amount_input: String,
    /// Last error from `assign_ceiling_step_delta`. Same lifecycle as
    /// `step_error` (also cleared by un-snap — the stage it described is gone).
    pub ceiling_step_error: Option<String>,
    /// Last error from the roof-point/ridge assigns (`assign_roof_apex`,
    /// `assign_roof_ridge_length`, `assign_roof_ridge_height`) — one field for
    /// the shared roof toolbar; cleared on success, skip, cancel, and un-snap.
    pub roof_error: Option<String>,
    /// Backing string for the roof-point apex-height input (metres). PREFILLED
    /// with the storey's default ceiling height when the Height stage opens
    /// (via `roof_apex_prefilled`); the user overtypes or accepts.
    pub roof_apex_input: String,
    /// One-shot guard for the prefill above: set when the Height stage's
    /// prefill has been written, cleared when the stage closes, so the user's
    /// in-progress typing isn't clobbered every frame.
    pub roof_apex_prefilled: bool,
    /// Backing string for the roof-ridge LENGTH input (metres). PREFILLED
    /// with the pending ridge's drawn length when the Length stage opens
    /// (via `roof_ridge_length_prefilled`); the user overtypes or skips.
    pub roof_ridge_length_input: String,
    /// One-shot guard for the length prefill, mirroring `roof_apex_prefilled`.
    pub roof_ridge_length_prefilled: bool,
    /// Identity (the open stage's axis) of the roof-RECT side-length input's
    /// last prefill — keyed on the AXIS so the LenX → LenY transition refills
    /// (the rect shares `roof_ridge_length_input`; the two flows are never
    /// pending at once). `None` when no rect length stage is up.
    pub roof_rect_len_key: Option<manip_plan::ParameterAxis>,
    /// Backing string for the new-storey flow's thickness input. PREFILLED
    /// "0.30" when the Thickness stage opens (via the one-shot guard below);
    /// the user overtypes, or "Use 0.30" submits the default directly.
    pub new_storey_thickness_input: String,
    /// One-shot guard for the thickness prefill, mirroring
    /// `roof_apex_prefilled`: set when the stage's prefill has been written,
    /// cleared when the stage closes.
    pub new_storey_thickness_prefilled: bool,
    /// Last error from the new-storey toolbar's actions
    /// (`assign_floor_thickness`, `choose_suggest_outlines`,
    /// `accept_suggestion`) — one field for the one toolbar, whose stages
    /// are mutually exclusive (the roof-family pattern). Shown inline in it;
    /// cleared on each success.
    pub new_storey_error: Option<String>,
    /// Last error from the Heights thickness field's `set_floor_thickness`.
    /// Its own field (not `new_storey_error`): the Heights field and the
    /// flow toolbar can be on screen at once, each error renders in the
    /// panel that owns the rejected input.
    pub floor_thickness_error: Option<String>,
    /// One-shot clear-on-success for the Heights thickness field's input
    /// buffer, mirroring `ceiling_height_input_clear` (the event closure
    /// can't clear `categories.floor_thickness_input` directly).
    pub floor_thickness_input_clear: bool,
    /// Last error from `set_default_ceiling_height`. Shown next to the
    /// Heights field so the user can correct without losing what they typed;
    /// cleared on the next successful Set.
    pub ceiling_height_error: Option<String>,
    /// One-shot clear-on-success for the Heights field's input buffer: the
    /// event closure can't clear `categories.ceiling_height_input` directly
    /// (the render borrows `categories` mutably), so a successful Set raises
    /// this and the next frame's pre-render housekeeping does the clear.
    pub ceiling_height_input_clear: bool,
    /// Category-layout UI state: the ribbon's active tab, the wall-length
    /// label visibility toggle (defaults ON), and the Heights field's
    /// ceiling-height input buffer.
    pub categories: categories::CategoryUiState,
    // ── Windows/doors ──────────────────────────────────────────────────────
    /// Per-field measurement buffers for the staged placement: one is bound to
    /// the floating number box at a time (width → height → sill, or the
    /// door+sidelights fields). The box clears the active one on Apply.
    pub opening_width_input: String,
    pub opening_height_input: String,
    /// Door+sidelights composer inputs (sidelight widths + per-side sills +
    /// transom height + the semicircular-transom toggle). A sidelight's sill
    /// field is only active when its `*_has_sill` box is ticked.
    pub sidelight_left_input: String,
    pub sidelight_left_sill_input: String,
    pub sidelight_left_has_sill: bool,
    pub sidelight_right_input: String,
    pub sidelight_right_sill_input: String,
    pub sidelight_right_has_sill: bool,
    pub transom_input: String,
    pub transom_semicircular: bool,
    /// Window-type picker (the placement box): which shared type a placed window
    /// joins. `wt_existing_id = Some` reuses an existing type by id; `None`
    /// defines a new one from glazing/frame/year/cert (find-or-created on Place,
    /// so a "new" type that matches an existing one joins it — "only add if
    /// different"). Persists across placements so repeated windows of one type
    /// are a single pick.
    pub wt_existing_id: Option<ber_model::ber::common::ItemId>,
    pub wt_glazing: ber_model::ber::fenestration::Glazing,
    pub wt_frame: ber_model::ber::fenestration::Frame,
    pub wt_year_input: String,
    pub wt_certified: bool,
    /// Pane gap(s) in mm for a new window type (comma/space separated for the two
    /// gaps of triple glazing); and an optional model name distinguishing two
    /// otherwise-identical types.
    pub wt_gap_input: String,
    pub wt_name_input: String,
    /// Per-window overshading selected in the placement box (defaults to Average).
    /// Persists across placements; applied to each window as it's placed.
    pub overshading: ber_model::ber::fenestration::Overshading,
    /// Source paths of photos picked in the placement box for the window being
    /// placed (a window photo is per-instance). Cleared at the start of each new
    /// placement; consumed (attached) when the window is placed.
    pub pending_window_photos: Vec<std::path::PathBuf>,
    /// Phase-3 (Objects) entry-panel state: the chosen category (to show its
    /// subtypes), the count / note / "Other"-name buffers, and the photos picked
    /// for the object being added. Cleared after each Add.
    pub object_selected_category: Option<ber_model::ber::objects::ObjectCategory>,
    /// The concrete kind chosen (a subtype picked, or a singleton category).
    /// `None` while a category is selected but its subtype hasn't been picked yet.
    pub object_selected_kind: Option<ber_model::ber::objects::ObjectKind>,
    pub object_count_input: String,
    pub object_note_input: String,
    pub object_other_name: String,
    pub object_pending_photos: Vec<std::path::PathBuf>,
    /// Whether the floating default-ceiling-height entry box is open. Lives here
    /// (not on `categories`, which the view borrows) so the event handler can
    /// toggle it; the view reads it by value. Opened by the Heights tool button,
    /// closed on a successful set or its Close button.
    pub ceiling_entry_open: bool,
    /// Background connection to a Bluetooth laser meter (Leica DISTO), feeding
    /// the number-entry box when it's in Laser mode. Lives across frames so the
    /// link stays up; inert unless the `bluetooth` feature is built.
    pub laser: crate::number_entry::bluetooth::LaserSource,
}

pub struct SplashState {
    pub progress_text: String,
    pub progress_value: f32,
    pub receiver: Option<Receiver<(String, f32)>>,
    pub loading_started: bool,
}

impl Default for SplashState {
    fn default() -> Self {
        Self {
            progress_text: String::new(),
            progress_value: 0.0,
            receiver: None,
            loading_started: false,
        }
    }
}

impl BerApp {
    pub fn new(app_controller: Rc<RefCell<AppController>>) -> Self {
        // DEV: `BER_CHECKPOINT=1 cargo run` loads the saved checkpoint plan and
        // jumps straight to the sketch section, so a test case needn't be redrawn.
        let current_page = if std::env::var("BER_CHECKPOINT").is_ok()
            && app_controller.borrow_mut().load_checkpoint()
        {
            Page::Workspace(Section::Sketch)
        } else {
            Page::Splash
        };
        let mut app = Self {
            current_page,
            app_controller,
            splash_state: SplashState::default(),
            home_state: HomeState::default(),
            key_details_state: KeyDetailsState::default(),
            sketch_state: SketchState::default(),
            last_seen_revision: 0,
            last_change_time: 0.0,
        };
        // Checkpoint path enters the workspace directly — prefill Key Details
        // the same way New/Load do.
        if matches!(app.current_page, Page::Workspace(_)) {
            app.populate_key_details();
        }
        app
    }

    pub fn navigate_to(&mut self, page: Page) {
        self.current_page = page;
    }

    /// Rebuild the Key Details form's backing strings from the loaded record
    /// (name, date, construction year + extensions reconstructed from its
    /// `ConstructionPhases`). Called on every workspace entry (New / Load /
    /// checkpoint) so the form always mirrors the record.
    fn populate_key_details(&mut self) {
        self.key_details_state = match self.app_controller.borrow().current_key_details() {
            Some((name, date, phases)) => KeyDetailsState::from_record(name, date, &phases),
            None => KeyDetailsState::default(),
        };
    }

    /// Act on a Home-page intent (controller call + navigation + state
    /// population). Runs after the render closure has released its borrows.
    fn handle_home_event(&mut self, event: HomeEvent) {
        match event {
            HomeEvent::NewBer => {
                // The record is created IMMEDIATELY — Key Details edits it live.
                let today = chrono::Local::now().naive_local().date();
                self.app_controller
                    .borrow_mut()
                    .create_ber("Untitled BER".to_string(), today);
                self.populate_key_details();
                self.home_state = HomeState::default();
                self.navigate_to(Page::Workspace(Section::KeyDetails));
            }
            HomeEvent::ToggleLoadPanel => {
                self.home_state.show_load_panel = !self.home_state.show_load_panel;
                self.home_state.pending_delete = None;
                if self.home_state.show_load_panel {
                    self.home_state.rows = self.app_controller.borrow().list_bers();
                }
            }
            HomeEvent::OpenBer(id) => {
                let result = self.app_controller.borrow_mut().open_ber(&id);
                match result {
                    Ok(()) => {
                        self.populate_key_details();
                        self.home_state = HomeState::default();
                        self.navigate_to(Page::Workspace(Section::KeyDetails));
                    }
                    Err(e) => log::error!("open_ber: {e}"),
                }
            }
            HomeEvent::DeleteBer(id) => {
                log_err(
                    "delete_ber",
                    self.app_controller.borrow_mut().delete_ber(&id),
                );
                self.home_state.pending_delete = None;
                self.home_state.rows = self.app_controller.borrow().list_bers();
            }
        }
    }

    pub fn start_loading(&mut self) {
        if self.splash_state.loading_started {
            return;
        }

        let (tx, rx) = channel::<(String, f32)>();
        self.splash_state.receiver = Some(rx);
        self.splash_state.loading_started = true;

        // Start async loading
        self.app_controller.borrow().load_data_async(tx);
    }

    pub fn check_progress(&mut self) {
        let mut should_navigate = false; // Local variable, not borrowing self

        if let Some(receiver) = &self.splash_state.receiver {
            // Immutable borrow starts
            while let Ok((text, progress)) = receiver.try_recv() {
                self.splash_state.progress_text = text; // ✅ OK: different field
                self.splash_state.progress_value = progress; // ✅ OK: different field

                if progress >= 1.0 {
                    should_navigate = true; // ✅ OK: just setting a bool
                                            // NOT calling navigate_to() here!
                }
            }
        } // Immutable borrow ends here

        if should_navigate {
            self.navigate_to(Page::Home); // ✅ OK: borrow is gone now
        }
    }
}

impl eframe::App for BerApp {
    /// Backstop for the idle autosave: eframe calls `save` periodically and on
    /// shutdown, so edits made within the idle window before a clean exit still
    /// reach disk. No-op when nothing is pending.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        let _ = self.app_controller.borrow_mut().flush_if_dirty();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.splash_state.loading_started {
            self.start_loading();
            self.splash_state.loading_started = true;
        }

        // Idle autosave: edits refresh the in-memory BER (cheap) but no longer
        // write to disk; we flush to SQLite once editing goes quiet. On a change
        // we reset the timer and schedule a wake-up at the idle deadline (egui is
        // reactive — without `request_repaint_after` it would never tick to save).
        {
            let now = ctx.input(|i| i.time);
            let rev = self.app_controller.borrow().revision();
            if rev != self.last_seen_revision {
                self.last_seen_revision = rev;
                self.last_change_time = now;
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(IDLE_SAVE_SECS));
            } else if now - self.last_change_time >= IDLE_SAVE_SECS {
                // No-op if nothing is pending; otherwise hand the write to the
                // background thread (falls back to a sync write if none attached).
                let _ = self.app_controller.borrow_mut().autosave();
            }
        }
        // Check for progress updates if on splash screen
        if self.current_page == Page::Splash {
            self.check_progress();
            ctx.request_repaint(); // Keep updating while loading
        }

        // Android's back button arrives as an ordinary egui key (winit maps
        // KEYCODE_BACK → BrowserBack). Back from the workspace exits the BER
        // (same flush-and-return-Home path as the chrome's Exit); back from
        // Home/Splash is deliberately ignored so the activity's default
        // minimise happens instead of killing the app. Unconditional — it's a
        // reasonable desktop shortcut too (browser-back keyboards).
        let back_pressed =
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::BrowserBack));

        let page = self.current_page;
        // Render current page
        match page {
            Page::Splash => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    SplashPage::render(
                        ui,
                        self.splash_state.progress_text.clone(),
                        self.splash_state.progress_value,
                        |_| {},
                    )
                });
            }
            Page::Home => {
                // Collect intents from the render closure first, then act after
                // it releases its borrow of self.home_state.
                let mut home_events: Vec<HomeEvent> = Vec::new();
                egui::CentralPanel::default().show(ctx, |ui| {
                    HomePage::render(ui, &mut self.home_state, |event| {
                        if let UiEvent::Home(e) = event {
                            home_events.push(e);
                        }
                    });
                });
                for e in home_events {
                    self.handle_home_event(e);
                }
            }
            Page::Workspace(section) => {
                let ber_name = self
                    .app_controller
                    .borrow()
                    .current_record_name()
                    .unwrap_or_default();
                // Chrome intents land in locals (the body closure borrows self).
                let mut next_page: Option<Page> = None;
                let mut exit_clicked = back_pressed;
                workspace_chrome(
                    ctx,
                    section,
                    &ber_name,
                    |event| match event {
                        UiEvent::NavigateTo(p) => next_page = Some(p),
                        UiEvent::ExitBer => exit_clicked = true,
                        _ => {}
                    },
                    |ui| match section {
                        Section::KeyDetails => {
                            // LIVE editor: every field change writes through to
                            // the loaded record (and persists) immediately.
                            let controller = self.app_controller.clone();
                            KeyDetailsPage::render(ui, &mut self.key_details_state, |event| {
                                if let UiEvent::KeyDetails(e) = event {
                                    let mut c = controller.borrow_mut();
                                    match e {
                                        KeyDetailsEvent::Rename(name) => c.rename_ber(name),
                                        KeyDetailsEvent::SetDateAssessed(d) => {
                                            c.set_date_assessed(d)
                                        }
                                        KeyDetailsEvent::SetConstructionPhases(p) => {
                                            c.set_construction_phases(p)
                                        }
                                    }
                                }
                            });
                        }
                        Section::MoreDetails => {
                            ui.heading("More Details");
                            ui.label("More details — coming soon.");
                        }
                        Section::Sketch => {
                            // Bring the resolved geometry current before the immutable borrow
                            // reads it for rendering — edits only `mark_dirty`, so the derive
                            // is deferred to here (once per frame, not per edit).
                            self.app_controller.borrow_mut().materialize();
                            let (
                                stroke,
                                mode,
                                edit_mode,
                                display_mode,
                                ber,
                                review,
                                review_remaining,
                                has_floor_plan,
                                pending_redraw,
                                dim_prompt,
                                dim_scale_set,
                                dim_error,
                                position_prompt,
                                dimensioned_walls,
                                arc_preview,
                                curve_bulge,
                                living_area_mode,
                                living_area_polygons,
                                living_area_wall_labels,
                                active_living_area,
                                pending_cut_regions,
                                exposure_mode,
                                exposure_walls,
                                exposure_selection,
                                riser_views,
                                partition_mode,
                                edit_selected_wall,
                                edit_selected_vertex,
                                edit_drag_prompt,
                                edit_drag_error,
                                vertex_drop_prompt,
                                vertex_drop_error,
                                can_undo,
                                can_redo,
                                phase_mode,
                                phase_region_polygons,
                                pending_phase_polygon,
                                pending_phase_cut_regions,
                                phase_choices,
                                phase_pick_pending,
                                step_mode,
                                step_region_polygons,
                                pending_step_polygon,
                                pending_step_cut_regions,
                                step_pick_pending,
                                ceiling_step_mode,
                                ceiling_step_available,
                                ceiling_step_region_polygons,
                                ceiling_divider_lines,
                                pending_ceiling_step_line,
                                pending_ceiling_step_side_rects,
                                ceiling_step_snap_tag,
                                ceiling_step_orientation_pending,
                                ceiling_step_side_pending,
                                ceiling_step_amount_pending,
                                ceiling_step_pending,
                                roof_point_mode,
                                roof_point_available,
                                roof_apex_views,
                                pending_roof_point,
                                roof_point_snap_tag,
                                roof_point_position_pending,
                                roof_point_height_pending,
                                roof_point_pending,
                                roof_ridge_views,
                                pending_roof_ridge_line,
                                roof_ridge_snap_tag,
                                roof_ridge_position_pending,
                                roof_ridge_length_pending,
                                roof_ridge_length_anchored,
                                pending_roof_ridge_length,
                                roof_ridge_height_pending,
                                roof_ridge_pending,
                                roof_rect_views,
                                pending_roof_rect_outline,
                                roof_rect_position_pending,
                                roof_rect_length_axis,
                                pending_roof_rect_length,
                                roof_rect_height_pending,
                                roof_rect_pending,
                                default_ceiling_height,
                                ceiling_heights_set,
                                new_storey_prompt,
                                floor_thickness,
                                view_params,
                            ) = {
                                let controller_ref = self.app_controller.borrow();
                                let v = controller_ref.view();
                                (
                                    controller_ref.sketch_stroke(),
                                    controller_ref.sketch_mode(),
                                    controller_ref.edit_mode(),
                                    controller_ref.display_mode(),
                                    controller_ref.current_ber_clone(),
                                    controller_ref.current_review_suggestion_clone(),
                                    controller_ref.review_remaining(),
                                    controller_ref.has_floor_plan(),
                                    controller_ref.has_pending_redraw(),
                                    controller_ref.dimension_prompt_for_ui(),
                                    controller_ref.dimensioning_scale_set(),
                                    self.sketch_state.dimension_error.clone(),
                                    controller_ref.position_prompt_for_ui(),
                                    controller_ref.walls_with_known_length(),
                                    controller_ref.pending_arc_preview(),
                                    controller_ref.pending_arc_bulge(),
                                    controller_ref.living_area_mode(),
                                    controller_ref.living_area_polygons(),
                                    controller_ref.living_area_wall_labels(),
                                    controller_ref.active_living_area_index(),
                                    controller_ref.pending_cut_regions(),
                                    controller_ref.exposure_mode(),
                                    controller_ref.wall_exposure_views(),
                                    controller_ref.exposure_selection(),
                                    controller_ref.riser_views(),
                                    controller_ref.partition_mode(),
                                    controller_ref.edit_selected_wall_positions(),
                                    controller_ref.edit_selected_vertex_position(),
                                    controller_ref.edit_drag_prompt_for_ui(),
                                    self.sketch_state.edit_drag_error.clone(),
                                    controller_ref.vertex_drop_prompt_for_ui(),
                                    self.sketch_state.vertex_drop_error.clone(),
                                    controller_ref.can_undo(),
                                    controller_ref.can_redo(),
                                    controller_ref.phase_mode(),
                                    controller_ref.phase_region_polygons(),
                                    controller_ref.pending_phase_polygon(),
                                    controller_ref.pending_phase_cut_regions(),
                                    controller_ref.construction_phase_choices(),
                                    controller_ref.phase_pick_pending(),
                                    controller_ref.step_mode(),
                                    controller_ref.step_region_polygons(),
                                    controller_ref.pending_step_polygon(),
                                    controller_ref.pending_step_cut_regions(),
                                    controller_ref.step_pick_pending(),
                                    controller_ref.ceiling_step_mode(),
                                    controller_ref.ceiling_step_available(),
                                    controller_ref.ceiling_step_region_polygons(),
                                    controller_ref.ceiling_divider_lines(),
                                    controller_ref.pending_ceiling_step_line(),
                                    controller_ref.pending_ceiling_step_side_rects(),
                                    controller_ref.ceiling_step_snap_tag(),
                                    controller_ref.ceiling_step_orientation_pending(),
                                    controller_ref.ceiling_step_side_pending(),
                                    controller_ref.ceiling_step_amount_pending(),
                                    controller_ref.ceiling_step_pending(),
                                    controller_ref.roof_point_mode(),
                                    controller_ref.roof_point_available(),
                                    controller_ref.roof_apex_views(),
                                    controller_ref.pending_roof_point_position(),
                                    controller_ref.roof_point_snap_tag(),
                                    controller_ref.roof_point_position_pending(),
                                    controller_ref.roof_point_height_pending(),
                                    controller_ref.roof_point_pending(),
                                    controller_ref.roof_ridge_views(),
                                    controller_ref.pending_roof_ridge_line(),
                                    controller_ref.roof_ridge_snap_tag(),
                                    controller_ref.roof_ridge_position_pending(),
                                    controller_ref.roof_ridge_length_pending(),
                                    controller_ref.roof_ridge_length_anchored(),
                                    controller_ref.pending_roof_ridge_length(),
                                    controller_ref.roof_ridge_height_pending(),
                                    controller_ref.roof_ridge_pending(),
                                    controller_ref.roof_rect_views(),
                                    controller_ref.pending_roof_rect_outline(),
                                    controller_ref.roof_rect_position_pending(),
                                    controller_ref.roof_rect_length_pending(),
                                    controller_ref.pending_roof_rect_length(),
                                    controller_ref.roof_rect_height_pending(),
                                    controller_ref.roof_rect_pending(),
                                    controller_ref.default_ceiling_height(),
                                    controller_ref.ceiling_heights_set(),
                                    controller_ref.new_storey_prompt(),
                                    controller_ref.floor_thickness(),
                                    crate::ui::render::ViewParams {
                                        zoom: v.zoom,
                                        offset_x: v.offset_x,
                                        offset_y: v.offset_y,
                                        yaw_deg: v.yaw_deg,
                                        pitch_deg: v.pitch_deg,
                                    },
                                )
                            };

                            // Prefill the position-prompt textbox with the prompt's drawn
                            // distance whenever the prompt IDENTITY (axis + selected
                            // reference option) changes — a fresh prompt or a cycled
                            // reference starts from its measured-as-drawn value; the
                            // user's in-progress typing on the SAME prompt is preserved.
                            if let Some(pp) = &position_prompt {
                                let key = (pp.axis, pp.kind, pp.option_idx);
                                if self.sketch_state.position_prompt_key != Some(key) {
                                    self.sketch_state.position_prompt_key = Some(key);
                                    self.sketch_state.position_input =
                                        format!("{:.2}", pp.current_distance);
                                }
                            } else {
                                self.sketch_state.position_prompt_key = None;
                            }

                            // Prefill the roof-point/ridge height box with the
                            // storey's default ceiling height when a Height stage
                            // OPENS (a sensible starting point for "how high is the
                            // ridge?"); the one-shot guard preserves the user's
                            // typing on the same stage across frames. The two flows
                            // share the buffer — they're never pending at once.
                            if roof_point_height_pending
                                || roof_ridge_height_pending
                                || roof_rect_height_pending
                            {
                                if !self.sketch_state.roof_apex_prefilled {
                                    self.sketch_state.roof_apex_prefilled = true;
                                    self.sketch_state.roof_apex_input = default_ceiling_height
                                        .map(|h| format!("{h:.2}"))
                                        .unwrap_or_default();
                                }
                            } else {
                                self.sketch_state.roof_apex_prefilled = false;
                            }

                            // Prefill the ridge LENGTH box with the pending
                            // ridge's drawn length when its stage OPENS — the
                            // stroke is approximate, so the typed value is
                            // the real dimension and the prefill is just the
                            // starting point. Same one-shot guard pattern.
                            if roof_ridge_length_pending {
                                if !self.sketch_state.roof_ridge_length_prefilled {
                                    self.sketch_state.roof_ridge_length_prefilled = true;
                                    self.sketch_state.roof_ridge_length_input =
                                        pending_roof_ridge_length
                                            .map(|l| format!("{l:.2}"))
                                            .unwrap_or_default();
                                }
                            } else {
                                self.sketch_state.roof_ridge_length_prefilled = false;
                            }

                            // Prefill the roof-RECT side-length box (shared
                            // buffer with the ridge length — never both
                            // pending) whenever the open stage's AXIS
                            // changes, so LenX → LenY refills with the drawn
                            // depth while preserving in-progress typing on
                            // the same stage.
                            if let Some(axis) = roof_rect_length_axis {
                                if self.sketch_state.roof_rect_len_key != Some(axis) {
                                    self.sketch_state.roof_rect_len_key = Some(axis);
                                    self.sketch_state.roof_ridge_length_input =
                                        pending_roof_rect_length
                                            .map(|l| format!("{l:.2}"))
                                            .unwrap_or_default();
                                }
                            } else {
                                self.sketch_state.roof_rect_len_key = None;
                            }

                            // Prefill the new-storey thickness box with the typical
                            // 0.30 m when the Thickness stage OPENS — "Use 0.30" /
                            // Enter then accepts it as-is. Same one-shot guard
                            // pattern as the roof prefills, so the user's typing
                            // on the open stage isn't clobbered every frame.
                            let thickness_pending = new_storey_prompt
                                .as_ref()
                                .is_some_and(|p| p.thickness_pending);
                            if thickness_pending {
                                if !self.sketch_state.new_storey_thickness_prefilled {
                                    self.sketch_state.new_storey_thickness_prefilled = true;
                                    self.sketch_state.new_storey_thickness_input =
                                        "0.30".to_string();
                                }
                            } else {
                                self.sketch_state.new_storey_thickness_prefilled = false;
                            }

                            // Deferred clear-on-success for the Heights field's input
                            // (raised by the SetDefaultCeilingHeight Ok path last frame).
                            if self.sketch_state.ceiling_height_input_clear {
                                self.sketch_state.categories.ceiling_height_input.clear();
                                self.sketch_state.ceiling_height_input_clear = false;
                            }
                            // Same deferred clear for the thickness field's input
                            // (raised by the SetFloorThickness Ok path last frame).
                            if self.sketch_state.floor_thickness_input_clear {
                                self.sketch_state.categories.floor_thickness_input.clear();
                                self.sketch_state.floor_thickness_input_clear = false;
                            }

                            let current_storey_idx =
                                self.app_controller.borrow().current_storey_idx();
                            let position_error = self.sketch_state.position_error.clone();
                            let ceiling_height_error =
                                self.sketch_state.ceiling_height_error.clone();
                            let step_error = self.sketch_state.step_error.clone();
                            let ceiling_step_error = self.sketch_state.ceiling_step_error.clone();
                            let roof_error = self.sketch_state.roof_error.clone();
                            let new_storey_error = self.sketch_state.new_storey_error.clone();
                            let floor_thickness_error =
                                self.sketch_state.floor_thickness_error.clone();
                            // Windows/doors view data (its own borrow — kept out
                            // of the big tuple above).
                            let (
                                openings_mode,
                                openings_tool,
                                opening_fields_prompt,
                                opening_dim_error,
                                existing_window_types,
                                elevation_view,
                                roof_window_selected,
                                delete_confirm_pending,
                            ) = {
                                let c = self.app_controller.borrow();
                                let prompt = c.opening_fields_prompt();
                                // The window-type picker list is only needed while a
                                // placement that offers it is in flight.
                                let existing_types = match &prompt {
                                    Some(p) if p.offer_window_type => c.existing_window_types(),
                                    _ => Vec::new(),
                                };
                                (
                                    c.openings_mode(),
                                    c.openings_tool(),
                                    prompt,
                                    c.opening_dim_error().map(str::to_owned),
                                    existing_types,
                                    c.elevation_view(),
                                    c.roof_window_selection(),
                                    c.delete_confirm_pending(),
                                )
                            };
                            // Owned partition view (the overlay takes a `&`); `None`
                            // when not partitioning.
                            let partition_view = self.app_controller.borrow().partition_view();

                            let controller = self.app_controller.clone();
                            let splice_debug = self.app_controller.borrow().splice_debug_overlay();
                            let snap_tags = self.app_controller.borrow().snap_overlay();

                            // Autosave status for the State category, derived
                            // from the idle-autosave bookkeeping above: when the
                            // controller is clean, everything up to the last
                            // edit is persisted, so "saved Ns ago" reports the
                            // time since that edit (the actual write happened
                            // slightly later — close enough for a status line).
                            let autosave_status = {
                                let now = ui.input(|i| i.time);
                                if self.app_controller.borrow().is_dirty() {
                                    Some("autosave pending…".to_string())
                                } else if self.last_change_time > 0.0 {
                                    Some(format!(
                                        "saved {:.0}s ago",
                                        (now - self.last_change_time).max(0.0)
                                    ))
                                } else {
                                    // No edits this session — nothing to report.
                                    None
                                }
                            };

                            // Per-phase checklist + phase state — recomputed
                            // from the (already materialized) editing model for
                            // the floating checklist window + phase signpost.
                            let (
                                current_phase,
                                mapping_complete,
                                surface_counts,
                                structure_kinds,
                                structure_select,
                                checklist,
                            ) = {
                                let c = self.app_controller.borrow();
                                let phase = c.current_phase();
                                let checklist = match phase {
                                    AssessmentPhase::Mapping => c.stage_a_checklist(),
                                    AssessmentPhase::Structure => c.structure_checklist(),
                                    AssessmentPhase::Objects => c.objects_checklist(),
                                };
                                (
                                    phase,
                                    c.mapping_complete(),
                                    c.current_surface_counts(),
                                    c.structure_kinds().clone(),
                                    c.structure_select(),
                                    checklist,
                                )
                            };
                            // External elevation photos (Front/Right/Left/Back)
                            // for the floating "Add external pictures" panel.
                            let external_photos = self.app_controller.borrow().external_photos();

                            // Stage-1 review: whether it's confirmed (the Structure
                            // gate) and, when the review is open, the current side's
                            // elevation + photo for the takeover overlay.
                            let (mapping_reviewed, mapping_review_view) = {
                                let c = self.app_controller.borrow();
                                let view = if c.mapping_review_mode() {
                                    c.mapping_review_view()
                                } else {
                                    None
                                };
                                (c.mapping_reviewed(), view)
                            };
                            let (structure_reviewed, structure_complete, structure_review_view) = {
                                let c = self.app_controller.borrow();
                                let view = if c.structure_review_mode() {
                                    c.structure_review_view()
                                } else {
                                    None
                                };
                                (c.structure_reviewed(), c.structure_complete(), view)
                            };
                            // Phase-3 (Objects) view-data: room pins + the open
                            // room's panel (only while in the Objects phase).
                            let (
                                object_pins,
                                active_object_room,
                                object_review_view,
                                objects_present,
                            ) = {
                                let c = self.app_controller.borrow();
                                if c.current_phase() == AssessmentPhase::Objects {
                                    let rv = if c.object_review_mode() {
                                        c.object_review_view()
                                    } else {
                                        None
                                    };
                                    (
                                        c.object_rooms_view(),
                                        c.active_object_room_view(),
                                        rv,
                                        c.has_object_rooms(),
                                    )
                                } else {
                                    (Vec::new(), None, None, false)
                                }
                            };

                            // Owned copy of the selected region ids so the view
                            // can borrow it without holding the controller.
                            let structure_selected = self
                                .app_controller
                                .borrow()
                                .structure_selected_ids()
                                .clone();
                            // Owned copy of the in-flight "Edit attributes" session.
                            let structure_edit =
                                self.app_controller.borrow().structure_edit().cloned();
                            // The existing constructions a selection can join (only
                            // needed while an edit session is open).
                            let structure_existing_groups = if structure_edit.is_some() {
                                self.app_controller.borrow().structure_existing_groups()
                            } else {
                                Vec::new()
                            };
                            // Owned copy of the reusable construction-layer library
                            // (the build-up editor's pick list).
                            let layer_library = self
                                .app_controller
                                .borrow()
                                .construction_layer_library()
                                .to_vec();
                            // Candidate regions of a pending Structure carve.
                            let structure_cut_regions =
                                self.app_controller.borrow().structure_cut_regions();
                            // The in-flight Structure split ring (highlighted
                            // while it's being dimensioned).
                            let pending_structure_split_polygon = self
                                .app_controller
                                .borrow()
                                .pending_structure_split_polygon();
                            // Staged (not-yet-split) carve rings — the pending
                            // selection shading until "Edit attributes" commits.
                            let pending_structure_carves = self
                                .app_controller
                                .borrow()
                                .pending_structure_carve_polygons();

                            // Floor-exposure tool: mode flags + the overlaid
                            // all-storeys floor view data.
                            let (
                                floor_exposure_mode,
                                floor_exposure_gate_pending,
                                floor_exposure_select,
                                floor_exposure_storeys,
                                floor_exposure_carve_polygon,
                                floor_exposure_cut_regions,
                            ) = {
                                let c = self.app_controller.borrow();
                                (
                                    c.floor_exposure_mode(),
                                    c.floor_exposure_gate_pending(),
                                    c.floor_exposure_select(),
                                    c.floor_exposure_view(),
                                    c.pending_floor_exposure_carve_polygon(),
                                    c.floor_exposure_cut_regions(),
                                )
                            };

                            // Ceiling-exposure tool: mode flags + the overlaid
                            // all-storeys ceiling view data (mirror of the floor
                            // tool above).
                            let (
                                ceiling_exposure_mode,
                                ceiling_exposure_gate_pending,
                                ceiling_exposure_select,
                                ceiling_exposure_storeys,
                                ceiling_exposure_carve_polygon,
                                ceiling_exposure_cut_regions,
                            ) = {
                                let c = self.app_controller.borrow();
                                (
                                    c.ceiling_exposure_mode(),
                                    c.ceiling_exposure_gate_pending(),
                                    c.ceiling_exposure_select(),
                                    c.ceiling_exposure_view(),
                                    c.pending_ceiling_exposure_carve_polygon(),
                                    c.ceiling_exposure_cut_regions(),
                                )
                            };

                            let navigation = self.app_controller.borrow().navigation_mode();
                            let view = SketchView {
                                current: stroke.as_ref().map(EguiStroke::from),
                                mode,
                                display: display_mode,
                                edit_tool: edit_mode,
                                navigation,
                                ber: ber.as_ref(),
                                current_storey_idx,
                                review: review.as_ref(),
                                review_remaining,
                                has_floor_plan,
                                pending_redraw,
                                dim_prompt,
                                dim_input: &mut self.sketch_state.dimension_input,
                                dim_scale_set,
                                dim_error: dim_error.as_deref(),
                                dimensioned_walls: &dimensioned_walls,
                                position_prompt,
                                position_input: &mut self.sketch_state.position_input,
                                position_error: position_error.as_deref(),
                                arc_preview: arc_preview.as_deref(),
                                curve_bulge,
                                living_area_mode,
                                living_area_polygons: &living_area_polygons,
                                living_area_wall_labels: &living_area_wall_labels,
                                active_living_area,
                                pending_cut_regions: &pending_cut_regions,
                                exposure_mode,
                                exposure_walls: &exposure_walls,
                                exposure_selection,
                                riser_views: &riser_views,
                                exposure_start_input: &mut self.sketch_state.exposure_start_input,
                                exposure_end_input: &mut self.sketch_state.exposure_end_input,
                                partition_mode,
                                partition_view: partition_view.as_ref(),
                                partition_dim_input: &mut self.sketch_state.partition_dim_input,
                                phase_mode,
                                phase_region_polygons: &phase_region_polygons,
                                pending_phase_polygon: &pending_phase_polygon,
                                pending_phase_cut_regions: &pending_phase_cut_regions,
                                phase_choices: &phase_choices,
                                phase_pick_pending,
                                step_mode,
                                step_region_polygons: &step_region_polygons,
                                pending_step_polygon: &pending_step_polygon,
                                pending_step_cut_regions: &pending_step_cut_regions,
                                step_pick_pending,
                                step_amount_input: &mut self.sketch_state.step_amount_input,
                                step_error: step_error.as_deref(),
                                ceiling_step_mode,
                                ceiling_step_available,
                                ceiling_step_region_polygons: &ceiling_step_region_polygons,
                                ceiling_divider_lines: &ceiling_divider_lines,
                                pending_ceiling_step_line,
                                pending_ceiling_step_side_rects: &pending_ceiling_step_side_rects,
                                ceiling_step_snap_tag: ceiling_step_snap_tag.as_ref(),
                                ceiling_step_orientation_pending,
                                ceiling_step_side_pending,
                                ceiling_step_amount_pending,
                                ceiling_step_pending,
                                ceiling_amount_input: &mut self.sketch_state.ceiling_amount_input,
                                ceiling_step_error: ceiling_step_error.as_deref(),
                                roof_point_mode,
                                roof_point_available,
                                roof_apex_views: &roof_apex_views,
                                pending_roof_point,
                                roof_point_snap_tag: roof_point_snap_tag.as_ref(),
                                roof_point_position_pending,
                                roof_point_height_pending,
                                roof_point_pending,
                                roof_apex_input: &mut self.sketch_state.roof_apex_input,
                                roof_ridge_views: &roof_ridge_views,
                                pending_roof_ridge_line,
                                roof_ridge_snap_tag: roof_ridge_snap_tag.as_ref(),
                                roof_ridge_position_pending,
                                roof_ridge_length_pending,
                                roof_ridge_length_anchored,
                                roof_ridge_height_pending,
                                roof_ridge_pending,
                                roof_ridge_area_options: self
                                    .app_controller
                                    .borrow()
                                    .roof_ridge_area_options(),
                                roof_rect_views: &roof_rect_views,
                                pending_roof_rect_outline,
                                roof_rect_position_pending,
                                roof_rect_length_axis,
                                roof_rect_height_pending,
                                roof_rect_pending,
                                roof_ridge_length_input: &mut self
                                    .sketch_state
                                    .roof_ridge_length_input,
                                roof_error: roof_error.as_deref(),
                                ceiling_height_error: ceiling_height_error.as_deref(),
                                default_ceiling_height,
                                ceiling_heights_set,
                                new_storey_prompt,
                                new_storey_thickness_input: &mut self
                                    .sketch_state
                                    .new_storey_thickness_input,
                                new_storey_error: new_storey_error.as_deref(),
                                floor_thickness,
                                floor_thickness_error: floor_thickness_error.as_deref(),
                                edit_selected_wall,
                                edit_selected_vertex,
                                edit_drag_prompt,
                                edit_drag_input: &mut self.sketch_state.edit_drag_input,
                                edit_drag_error: edit_drag_error.as_deref(),
                                vertex_drop_prompt,
                                vertex_drop_inputs: &mut self.sketch_state.vertex_drop_inputs,
                                vertex_drop_error: vertex_drop_error.as_deref(),
                                can_undo,
                                can_redo,
                                view_params,
                                splice_debug: &splice_debug,
                                snap_tags: &snap_tags,
                                cat_state: &mut self.sketch_state.categories,
                                laser: &mut self.sketch_state.laser,
                                ceiling_entry_open: self.sketch_state.ceiling_entry_open,
                                autosave_status,
                                openings_mode,
                                openings_tool,
                                opening_fields_prompt,
                                opening_dim_error: opening_dim_error.as_deref(),
                                opening_width_input: &mut self.sketch_state.opening_width_input,
                                opening_height_input: &mut self.sketch_state.opening_height_input,
                                sidelight_left_input: &mut self.sketch_state.sidelight_left_input,
                                sidelight_left_sill_input: &mut self
                                    .sketch_state
                                    .sidelight_left_sill_input,
                                sidelight_left_has_sill: &mut self
                                    .sketch_state
                                    .sidelight_left_has_sill,
                                sidelight_right_input: &mut self.sketch_state.sidelight_right_input,
                                sidelight_right_sill_input: &mut self
                                    .sketch_state
                                    .sidelight_right_sill_input,
                                sidelight_right_has_sill: &mut self
                                    .sketch_state
                                    .sidelight_right_has_sill,
                                transom_input: &mut self.sketch_state.transom_input,
                                transom_semicircular: &mut self.sketch_state.transom_semicircular,
                                wt_existing_id: &mut self.sketch_state.wt_existing_id,
                                wt_glazing: &mut self.sketch_state.wt_glazing,
                                wt_frame: &mut self.sketch_state.wt_frame,
                                wt_year_input: &mut self.sketch_state.wt_year_input,
                                wt_certified: &mut self.sketch_state.wt_certified,
                                wt_gap_input: &mut self.sketch_state.wt_gap_input,
                                wt_name_input: &mut self.sketch_state.wt_name_input,
                                overshading: &mut self.sketch_state.overshading,
                                pending_window_photos: &mut self.sketch_state.pending_window_photos,
                                existing_window_types,
                                elevation_view,
                                roof_window_selected,
                                delete_confirm_pending,
                                checklist,
                                external_photos,
                                current_phase,
                                mapping_complete,
                                mapping_reviewed,
                                mapping_review_view,
                                structure_complete,
                                structure_reviewed,
                                structure_review_view,
                                object_pins,
                                active_object_room,
                                object_selected_category: &mut self
                                    .sketch_state
                                    .object_selected_category,
                                object_selected_kind: &mut self.sketch_state.object_selected_kind,
                                object_count_input: &mut self.sketch_state.object_count_input,
                                object_note_input: &mut self.sketch_state.object_note_input,
                                object_other_name: &mut self.sketch_state.object_other_name,
                                object_pending_photos: &mut self.sketch_state.object_pending_photos,
                                objects_present,
                                object_review_view,
                                surface_counts,
                                structure_kinds,
                                structure_select,
                                structure_selected: &structure_selected,
                                structure_edit: structure_edit.as_ref(),
                                structure_existing_groups,
                                structure_cut_regions: &structure_cut_regions,
                                pending_structure_split_polygon: &pending_structure_split_polygon,
                                pending_structure_carves: &pending_structure_carves,
                                layer_library: &layer_library,
                                floor_exposure_mode,
                                floor_exposure_gate_pending,
                                floor_exposure_select,
                                floor_exposure_storeys: &floor_exposure_storeys,
                                floor_exposure_carve_polygon: &floor_exposure_carve_polygon,
                                floor_exposure_cut_regions: &floor_exposure_cut_regions,
                                ceiling_exposure_mode,
                                ceiling_exposure_gate_pending,
                                ceiling_exposure_select,
                                ceiling_exposure_storeys: &ceiling_exposure_storeys,
                                ceiling_exposure_carve_polygon: &ceiling_exposure_carve_polygon,
                                ceiling_exposure_cut_regions: &ceiling_exposure_cut_regions,
                            };
                            let touch_nav = &mut self.sketch_state.touch_nav;
                            SketchPage::render(ui, view, touch_nav, |event| match event {
                                UiEvent::Sketch(sketch_event) => match sketch_event {
                                    // Logged, not expected: after a two-finger
                                    // CancelStroke, a trailing AddPoint/EndStroke
                                    // can legitimately find no active stroke.
                                    SketchEvent::AddPoint(p) => log_err(
                                        "sketch_add_point",
                                        controller
                                            .borrow_mut()
                                            .sketch_add_point(pos2_to_point2f(p)),
                                    ),
                                    SketchEvent::EndStroke(p) => log_err(
                                        "sketch_end_stroke",
                                        controller
                                            .borrow_mut()
                                            .sketch_end_stroke(pos2_to_point2f(p)),
                                    ),
                                    SketchEvent::CancelStroke => {
                                        controller.borrow_mut().sketch_cancel_stroke();
                                    }
                                    SketchEvent::StartStroke(p) => log_err(
                                        "sketch_start_stroke",
                                        controller
                                            .borrow_mut()
                                            .sketch_start_stroke(pos2_to_point2f(p)),
                                    ),
                                    SketchEvent::ChangeMode(m) => {
                                        // Switching 2D↔3D drops the active tool —
                                        // CAD-style: the canvas returns to plain
                                        // navigation so a stray tap does nothing.
                                        let mut c = controller.borrow_mut();
                                        c.set_sketch_mode(m);
                                        c.enter_navigation_mode();
                                    }
                                    SketchEvent::ChangeEditTool(m) => {
                                        controller.borrow_mut().set_edit_mode(m)
                                    }
                                    SketchEvent::AcceptCollinearity => {
                                        log_err(
                                            "accept_current_review",
                                            controller.borrow_mut().accept_current_review(),
                                        );
                                    }
                                    SketchEvent::RejectCollinearity => {
                                        controller.borrow_mut().reject_current_review();
                                    }
                                    SketchEvent::SkipAllCollinearity => {
                                        controller.borrow_mut().skip_all_review();
                                    }
                                    SketchEvent::ZoomView {
                                        factor,
                                        anchor_x,
                                        anchor_y,
                                        canvas_center_x,
                                        canvas_center_y,
                                    } => {
                                        controller.borrow_mut().zoom_view(
                                            factor,
                                            (anchor_x, anchor_y),
                                            (canvas_center_x, canvas_center_y),
                                        );
                                    }
                                    SketchEvent::PanView { dx, dy } => {
                                        controller.borrow_mut().pan_view(dx, dy);
                                    }
                                    SketchEvent::OrbitView { dyaw, dpitch } => {
                                        controller.borrow_mut().orbit_view(dyaw, dpitch);
                                    }
                                    SketchEvent::ResetView => {
                                        controller.borrow_mut().reset_view();
                                    }
                                    SketchEvent::ReplaceStorey => {
                                        log_err(
                                            "confirm_replace_storey",
                                            controller.borrow_mut().confirm_replace_storey(),
                                        );
                                    }
                                    SketchEvent::AddStoreyFromCurrent => {
                                        controller.borrow_mut().confirm_add_storey_from_current();
                                    }
                                    SketchEvent::IgnoreRedraw => {
                                        controller.borrow_mut().discard_pending_redraw();
                                    }
                                    SketchEvent::ChooseSnap { from, choice } => {
                                        controller.borrow_mut().choose_snap(
                                            domain::points::Point2f32::new(from.0, from.1),
                                            choice,
                                        );
                                    }
                                    SketchEvent::ApplyDimension(value) => {
                                        match controller.borrow_mut().apply_dimension(value) {
                                            Ok(()) => {
                                                self.sketch_state.dimension_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("apply_dimension: {}", e);
                                                self.sketch_state.dimension_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SkipDimension => {
                                        controller.borrow_mut().skip_current_dimension();
                                        self.sketch_state.dimension_error = None;
                                    }
                                    SketchEvent::ApplyPositionDimension(value) => {
                                        match controller
                                            .borrow_mut()
                                            .apply_position_dimension(value)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.position_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("apply_position_dimension: {}", e);
                                                self.sketch_state.position_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SkipPositionDimension => {
                                        // A roof-ridge/rect skip at the last
                                        // location stage can trip the FINALIZE
                                        // placement check: surface it like a
                                        // typed rejection.
                                        match controller.borrow_mut().skip_position_dimension() {
                                            Ok(()) => {
                                                self.sketch_state.position_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("skip_position_dimension: {}", e);
                                                self.sketch_state.position_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::CyclePositionReference => {
                                        controller.borrow_mut().cycle_position_reference();
                                        self.sketch_state.position_error = None;
                                    }
                                    SketchEvent::SetCurveBulge(ratio) => {
                                        controller.borrow_mut().set_pending_arc_bulge(ratio);
                                    }
                                    SketchEvent::CommitCurve => {
                                        log_err(
                                            "commit_pending_arc",
                                            controller.borrow_mut().commit_pending_arc(),
                                        );
                                    }
                                    SketchEvent::SetLivingAreaMode(on) => {
                                        controller.borrow_mut().set_living_area_mode(on);
                                    }
                                    SketchEvent::LivingAreaStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_living_area_stroke_event(&points);
                                    }
                                    SketchEvent::DeleteLivingArea(index) => {
                                        controller.borrow_mut().delete_living_area(index);
                                    }
                                    SketchEvent::ChooseCutRegion { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .choose_cut_region(world_x as f32, world_y as f32);
                                    }
                                    SketchEvent::SetPhaseMode(on) => {
                                        controller.borrow_mut().set_phase_mode(on);
                                    }
                                    SketchEvent::PhaseRegionStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_phase_region_stroke_event(&points);
                                    }
                                    SketchEvent::AssignPhase(phase) => {
                                        controller.borrow_mut().assign_phase_to_pending(phase);
                                    }
                                    SketchEvent::ChoosePhaseCutRegion { world_x, world_y } => {
                                        controller.borrow_mut().choose_phase_cut_region(
                                            world_x as f32,
                                            world_y as f32,
                                        );
                                    }
                                    SketchEvent::CancelPendingPhase => {
                                        controller.borrow_mut().cancel_pending_phase();
                                    }
                                    SketchEvent::DeletePhaseRegion(index) => {
                                        controller.borrow_mut().delete_phase_region(index);
                                    }
                                    SketchEvent::AssignRestOriginal => {
                                        controller.borrow_mut().assign_rest_to_original();
                                    }
                                    SketchEvent::SetStepMode(on) => {
                                        controller.borrow_mut().set_step_mode(on);
                                    }
                                    SketchEvent::StepRegionStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_step_region_stroke_event(&points);
                                    }
                                    SketchEvent::AssignStepLevel(level_m) => {
                                        match controller
                                            .borrow_mut()
                                            .assign_step_level_to_pending(level_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.step_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_step_level_to_pending: {}", e);
                                                self.sketch_state.step_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::ChooseStepCutRegion { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .choose_step_cut_region(world_x as f32, world_y as f32);
                                    }
                                    SketchEvent::CancelPendingStep => {
                                        controller.borrow_mut().cancel_pending_step();
                                        self.sketch_state.step_error = None;
                                    }
                                    SketchEvent::DeleteStepRegion(index) => {
                                        controller.borrow_mut().delete_step_region(index);
                                    }
                                    SketchEvent::SetCeilingStepMode(on) => {
                                        controller.borrow_mut().set_ceiling_step_mode(on);
                                    }
                                    SketchEvent::CeilingStepStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_ceiling_step_stroke_event(&points);
                                    }
                                    SketchEvent::CeilingStepTap { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .ceiling_step_tap(world_x as f32, world_y as f32);
                                    }
                                    SketchEvent::ChooseCeilingStepOrientation { horizontal } => {
                                        controller
                                            .borrow_mut()
                                            .choose_ceiling_step_orientation(horizontal);
                                    }
                                    SketchEvent::AssignCeilingStepDelta(delta_m) => {
                                        match controller
                                            .borrow_mut()
                                            .assign_ceiling_step_delta(delta_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.ceiling_step_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_ceiling_step_delta: {}", e);
                                                self.sketch_state.ceiling_step_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::KeepCeilingSameHeight => {
                                        match controller.borrow_mut().keep_ceiling_same_height() {
                                            Ok(()) => {
                                                self.sketch_state.ceiling_step_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("keep_ceiling_same_height: {}", e);
                                                self.sketch_state.ceiling_step_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::UnsnapCeilingStepLine => {
                                        controller.borrow_mut().unsnap_ceiling_step_line();
                                        // The stage the error described is gone.
                                        self.sketch_state.ceiling_step_error = None;
                                    }
                                    SketchEvent::CancelPendingCeilingStep => {
                                        controller.borrow_mut().cancel_pending_ceiling_step();
                                        self.sketch_state.ceiling_step_error = None;
                                    }
                                    SketchEvent::DeleteCeilingStepRegion(ring) => {
                                        controller.borrow_mut().delete_ceiling_step_region(&ring);
                                    }
                                    SketchEvent::SetRoofPointMode(on) => {
                                        controller.borrow_mut().set_roof_point_mode(on);
                                    }
                                    SketchEvent::RoofPointTap { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .roof_point_tap(world_x as f32, world_y as f32);
                                    }
                                    SketchEvent::AssignRoofApexHeight(z_m) => {
                                        match controller.borrow_mut().assign_roof_apex(z_m) {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_apex: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::UnsnapRoofPoint => {
                                        controller.borrow_mut().unsnap_roof_point();
                                        // The stage the error described is gone.
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::CancelPendingRoofPoint => {
                                        controller.borrow_mut().cancel_pending_roof_point();
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::DeleteRoofApex(index) => {
                                        controller.borrow_mut().delete_roof_apex(index);
                                    }
                                    SketchEvent::RoofRidgeStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_roof_ridge_stroke_event(&points);
                                    }
                                    SketchEvent::AssignRoofRidgeLength(len_m) => {
                                        match controller
                                            .borrow_mut()
                                            .assign_roof_ridge_length(len_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_ridge_length: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SkipRoofRidgeLength => {
                                        // The skip can trip the FINALIZE placement
                                        // check (both positions snapped): surface it
                                        // like a typed rejection.
                                        match controller.borrow_mut().skip_roof_ridge_length() {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("skip_roof_ridge_length: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::AssignRoofRidgeHeight(z_m) => {
                                        match controller.borrow_mut().assign_roof_ridge_height(z_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_ridge_height: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::ChooseRoofRidgeArea(i) => {
                                        if let Err(e) =
                                            controller.borrow_mut().choose_roof_ridge_area(i)
                                        {
                                            log::error!("choose_roof_ridge_area: {}", e);
                                            self.sketch_state.roof_error = Some(e);
                                        }
                                    }
                                    SketchEvent::CentreRoofPosition => {
                                        // The rejection lands on the open
                                        // position prompt, like a typed value.
                                        match controller.borrow_mut().centre_roof_position() {
                                            Ok(()) => {
                                                self.sketch_state.position_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("centre_roof_position: {}", e);
                                                self.sketch_state.position_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::UnsnapRoofRidge => {
                                        controller.borrow_mut().unsnap_roof_ridge();
                                        // The stage the error described is gone.
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::CancelPendingRoofRidge => {
                                        controller.borrow_mut().cancel_pending_roof_ridge();
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::AssignRoofRectLength(len_m) => {
                                        match controller.borrow_mut().assign_roof_rect_length(len_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_rect_length: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SkipRoofRectLength => {
                                        // The LenY skip can trip the FINALIZE
                                        // placement check (no position question
                                        // opened): surface it like a typed
                                        // rejection.
                                        match controller.borrow_mut().skip_roof_rect_length() {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("skip_roof_rect_length: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::AssignRoofRectHeight(z_m) => {
                                        match controller.borrow_mut().assign_roof_rect_height(z_m) {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_rect_height: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::CancelPendingRoofRect => {
                                        controller.borrow_mut().cancel_pending_roof_rect();
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::SetExposureMode(on) => {
                                        controller.borrow_mut().set_exposure_mode(on);
                                    }
                                    SketchEvent::ExposurePointerDown {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .exposure_pointer_down(world_x, world_y, tol);
                                    }
                                    SketchEvent::ExposurePointerDrag { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .exposure_pointer_drag(world_x, world_y);
                                    }
                                    SketchEvent::ExposurePointerUp => {
                                        controller.borrow_mut().exposure_pointer_up();
                                    }
                                    SketchEvent::SetExposureSectionRange { start_m, end_m } => {
                                        controller
                                            .borrow_mut()
                                            .set_exposure_section_range(start_m, end_m);
                                    }
                                    SketchEvent::SetWallExposure(exposure) => {
                                        controller.borrow_mut().set_wall_exposure(exposure);
                                    }
                                    SketchEvent::ClearWallExposure => {
                                        controller.borrow_mut().clear_wall_exposure();
                                    }
                                    SketchEvent::SetAllRemainingExposed => {
                                        controller.borrow_mut().set_all_remaining_exposed();
                                    }
                                    SketchEvent::OpenWallPartition => {
                                        controller.borrow_mut().open_wall_partition();
                                    }
                                    SketchEvent::CloseWallPartition => {
                                        controller.borrow_mut().close_wall_partition();
                                    }
                                    SketchEvent::PartitionStrokePoint { along_m, height_m } => {
                                        controller
                                            .borrow_mut()
                                            .partition_stroke_point(along_m, height_m);
                                    }
                                    SketchEvent::PartitionStrokeEnd => {
                                        controller.borrow_mut().partition_stroke_end();
                                    }
                                    SketchEvent::PartitionSelectRegion { along_m, height_m } => {
                                        controller
                                            .borrow_mut()
                                            .select_partition_region(along_m, height_m);
                                    }
                                    SketchEvent::SetPartitionRegionExposure(exposure) => {
                                        controller
                                            .borrow_mut()
                                            .set_partition_region_exposure(exposure);
                                    }
                                    SketchEvent::PartitionDimEntry(value) => {
                                        controller.borrow_mut().partition_dim_entry(value);
                                    }
                                    SketchEvent::PartitionDimSkip => {
                                        controller.borrow_mut().partition_dim_skip();
                                    }
                                    SketchEvent::ClearWallPartition => {
                                        controller.borrow_mut().clear_active_wall_partition();
                                    }
                                    SketchEvent::SetOpeningsMode(on) => {
                                        controller.borrow_mut().set_openings_mode(on);
                                    }
                                    SketchEvent::SelectOpeningTool(tool) => {
                                        controller.borrow_mut().select_opening_tool(tool);
                                    }
                                    SketchEvent::OpeningsPointerDown {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .openings_pointer_down(world_x, world_y, tol);
                                    }
                                    // The placement methods set their own inline error
                                    // (read back via `opening_dim_error()`), so the
                                    // Result is intentionally dropped here.
                                    SketchEvent::PlaceOpening {
                                        width_m,
                                        height_m,
                                        left,
                                        left_sill,
                                        right,
                                        right_sill,
                                        transom,
                                        transom_semicircular,
                                        window_type,
                                        overshading,
                                        photos,
                                        photo_quality,
                                    } => {
                                        let _ = controller.borrow_mut().place_opening(
                                            width_m,
                                            height_m,
                                            left,
                                            left_sill,
                                            right,
                                            right_sill,
                                            transom,
                                            transom_semicircular,
                                            window_type,
                                            overshading,
                                            photos,
                                            photo_quality,
                                        );
                                    }
                                    SketchEvent::FillOpeningSurface {
                                        window_type,
                                        overshading,
                                        photos,
                                        photo_quality,
                                    } => {
                                        let _ = controller.borrow_mut().fill_opening_surface(
                                            window_type,
                                            overshading,
                                            photos,
                                            photo_quality,
                                        );
                                    }
                                    SketchEvent::CancelOpeningPlacement => {
                                        controller.borrow_mut().cancel_opening_placement();
                                    }
                                    SketchEvent::ElevationPointerDown { along_m, height_m } => {
                                        controller
                                            .borrow_mut()
                                            .elevation_pointer_down(along_m, height_m);
                                    }
                                    SketchEvent::ElevationPointerDrag { along_m, height_m } => {
                                        controller
                                            .borrow_mut()
                                            .elevation_pointer_drag(along_m, height_m);
                                    }
                                    SketchEvent::ElevationPointerUp => {
                                        controller.borrow_mut().elevation_pointer_up();
                                    }
                                    SketchEvent::OpeningsPointerDrag { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .openings_pointer_drag(world_x, world_y);
                                    }
                                    SketchEvent::OpeningsPointerUp => {
                                        controller.borrow_mut().openings_pointer_up();
                                    }
                                    SketchEvent::DeleteSelectedOpening => {
                                        controller.borrow_mut().request_delete_opening();
                                    }
                                    SketchEvent::ConfirmDeleteOpening => {
                                        controller.borrow_mut().confirm_delete_opening();
                                    }
                                    SketchEvent::CancelDeleteOpening => {
                                        controller.borrow_mut().cancel_delete();
                                    }
                                    SketchEvent::CloseElevation => {
                                        controller.borrow_mut().close_elevation();
                                    }
                                    SketchEvent::OpenMappingReview => {
                                        controller.borrow_mut().set_mapping_review_mode(true);
                                    }
                                    SketchEvent::CloseMappingReview => {
                                        controller.borrow_mut().set_mapping_review_mode(false);
                                    }
                                    SketchEvent::StepMappingReviewSide(delta) => {
                                        controller.borrow_mut().step_mapping_review_side(delta);
                                    }
                                    SketchEvent::ConfirmMappingReview => {
                                        controller.borrow_mut().confirm_mapping_review();
                                    }
                                    SketchEvent::MappingReviewPointerDown { x, z } => {
                                        controller.borrow_mut().mapping_review_pointer_down(x, z);
                                    }
                                    SketchEvent::MappingReviewPointerDrag { x, z } => {
                                        controller.borrow_mut().mapping_review_pointer_drag(x, z);
                                    }
                                    SketchEvent::MappingReviewPointerUp => {
                                        controller.borrow_mut().mapping_review_pointer_up();
                                    }
                                    SketchEvent::OpenStructureReview => {
                                        controller.borrow_mut().set_structure_review_mode(true)
                                    }
                                    SketchEvent::CloseStructureReview => {
                                        controller.borrow_mut().set_structure_review_mode(false)
                                    }
                                    SketchEvent::StepStructureReview(delta) => {
                                        controller.borrow_mut().step_structure_review(delta)
                                    }
                                    SketchEvent::ConfirmStructureReview => {
                                        controller.borrow_mut().confirm_structure_review()
                                    }
                                    SketchEvent::ObjectsPointerDown {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .objects_pointer_down(world_x, world_y, tol);
                                    }
                                    SketchEvent::CloseObjectRoom => {
                                        controller.borrow_mut().close_object_room();
                                    }
                                    SketchEvent::RenameObjectRoom(name) => {
                                        controller.borrow_mut().rename_object_room(name);
                                    }
                                    SketchEvent::AddObject {
                                        kind,
                                        count,
                                        note,
                                        photos,
                                        photo_quality,
                                    } => {
                                        controller.borrow_mut().add_object(
                                            kind,
                                            count,
                                            note,
                                            photos,
                                            photo_quality,
                                        );
                                    }
                                    SketchEvent::DeleteObject { room, object } => {
                                        controller.borrow_mut().delete_object(room, object);
                                    }
                                    SketchEvent::DeleteObjectRoom(room) => {
                                        controller.borrow_mut().delete_object_room(room);
                                    }
                                    SketchEvent::OpenObjectReview => {
                                        controller.borrow_mut().set_object_review_mode(true);
                                    }
                                    SketchEvent::CloseObjectReview => {
                                        controller.borrow_mut().set_object_review_mode(false);
                                    }
                                    SketchEvent::SelectObjectReviewKind(idx) => {
                                        controller.borrow_mut().select_object_review(idx);
                                    }
                                    SketchEvent::ConfirmObjectAbsent(cat) => {
                                        controller.borrow_mut().confirm_object_absent(cat);
                                    }
                                    SketchEvent::SkipObjectAbsent(cat) => {
                                        controller.borrow_mut().skip_object_absent(cat);
                                    }
                                    SketchEvent::SetDefaultCeilingHeight(value) => {
                                        match controller
                                            .borrow_mut()
                                            .set_default_ceiling_height(value)
                                        {
                                            Ok(()) => {
                                                // Clear-on-success: the button's label now
                                                // shows the stored value, so the entry box
                                                // resets to empty for the next entry (via
                                                // the one-shot flag — see its doc) and the
                                                // floating box closes. A rejected value
                                                // keeps the box open (it shows the error)
                                                // for correction.
                                                self.sketch_state.ceiling_height_error = None;
                                                self.sketch_state.ceiling_height_input_clear = true;
                                                self.sketch_state.ceiling_entry_open = false;
                                            }
                                            Err(e) => {
                                                log::error!("set_default_ceiling_height: {}", e);
                                                self.sketch_state.ceiling_height_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::OpenCeilingHeightEntry => {
                                        self.sketch_state.ceiling_entry_open = true;
                                    }
                                    SketchEvent::CloseCeilingHeightEntry => {
                                        self.sketch_state.ceiling_entry_open = false;
                                    }
                                    SketchEvent::SetCurrentStorey(target) => {
                                        log_err(
                                            "set_current_storey",
                                            controller.borrow_mut().set_current_storey(target),
                                        );
                                    }
                                    SketchEvent::AddStorey => {
                                        log_err("add_storey", controller.borrow_mut().add_storey());
                                    }
                                    SketchEvent::AssignFloorThickness(v) => {
                                        match controller.borrow_mut().assign_floor_thickness(v) {
                                            Ok(()) => {
                                                self.sketch_state.new_storey_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_floor_thickness: {}", e);
                                                self.sketch_state.new_storey_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SetFloorThickness(v) => {
                                        match controller.borrow_mut().set_floor_thickness(v) {
                                            Ok(()) => {
                                                // Clear-on-success mirrors the ceiling-height
                                                // field: the label now shows the stored value,
                                                // so the entry box resets via the one-shot
                                                // flag; a rejected value stays for correction.
                                                self.sketch_state.floor_thickness_error = None;
                                                self.sketch_state.floor_thickness_input_clear =
                                                    true;
                                            }
                                            Err(e) => {
                                                log::error!("set_floor_thickness: {}", e);
                                                self.sketch_state.floor_thickness_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::ChooseDrawOutline => {
                                        controller.borrow_mut().choose_draw_outline();
                                        self.sketch_state.new_storey_error = None;
                                    }
                                    SketchEvent::ChooseSuggestOutlines => {
                                        match controller.borrow_mut().choose_suggest_outlines() {
                                            Ok(()) => {
                                                self.sketch_state.new_storey_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("choose_suggest_outlines: {}", e);
                                                self.sketch_state.new_storey_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::NextSuggestion => {
                                        controller.borrow_mut().cycle_suggestion(true);
                                    }
                                    SketchEvent::PrevSuggestion => {
                                        controller.borrow_mut().cycle_suggestion(false);
                                    }
                                    SketchEvent::AcceptSuggestion => {
                                        match controller.borrow_mut().accept_suggestion() {
                                            Ok(()) => {
                                                self.sketch_state.new_storey_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("accept_suggestion: {}", e);
                                                self.sketch_state.new_storey_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SetAssessmentPhase(phase) => {
                                        controller.borrow_mut().set_current_phase(phase);
                                    }
                                    SketchEvent::SetStructureKind(kind) => {
                                        controller.borrow_mut().toggle_structure_kind(kind);
                                    }
                                    SketchEvent::SetStructureSelectMode(mode) => {
                                        controller.borrow_mut().set_structure_select_mode(mode);
                                    }
                                    SketchEvent::StructureSurfaceTap {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .toggle_structure_surface_at(world_x, world_y, tol);
                                    }
                                    SketchEvent::DeselectAllStructure => {
                                        controller.borrow_mut().clear_structure_surface_selection();
                                    }
                                    SketchEvent::StructureCarveStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_structure_carve_stroke(&points);
                                    }
                                    SketchEvent::ChooseStructureCutRegion { world_x, world_y } => {
                                        controller.borrow_mut().choose_structure_cut_region(
                                            world_x as f32,
                                            world_y as f32,
                                        );
                                    }
                                    SketchEvent::BeginStructureEdit => {
                                        controller.borrow_mut().begin_structure_edit();
                                    }
                                    SketchEvent::SetActiveStructureGroup(i) => {
                                        controller.borrow_mut().set_active_structure_group(i);
                                    }
                                    SketchEvent::SetStructureGroupAttrs {
                                        group,
                                        ty,
                                        makeup,
                                        phase,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .set_structure_group_attrs(group, ty, makeup, phase);
                                    }
                                    SketchEvent::SetStructureGroupName { group, name } => {
                                        controller
                                            .borrow_mut()
                                            .set_structure_group_name(group, name);
                                    }
                                    SketchEvent::SetStructureGroupExposure {
                                        group_index,
                                        exposure,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .set_structure_group_exposure(group_index, exposure);
                                    }
                                    SketchEvent::BeginSpecifyLayers { group } => {
                                        controller.borrow_mut().begin_specify_layers(group);
                                    }
                                    SketchEvent::EndSpecifyLayers => {
                                        controller.borrow_mut().end_specify_layers();
                                    }
                                    SketchEvent::SetStructureGroupLayers { group, layers } => {
                                        controller
                                            .borrow_mut()
                                            .set_structure_group_layers(group, layers);
                                    }
                                    SketchEvent::BeginViewPhotos { group } => {
                                        controller.borrow_mut().begin_view_photos(group);
                                    }
                                    SketchEvent::EndViewPhotos => {
                                        controller.borrow_mut().end_view_photos();
                                    }
                                    SketchEvent::AddGroupPhoto {
                                        group,
                                        path,
                                        quality,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .add_group_photo(group, &path, quality);
                                    }
                                    SketchEvent::EndStructureEdit => {
                                        controller.borrow_mut().end_structure_edit();
                                    }
                                    SketchEvent::SetFloorExposureMode(on) => {
                                        controller.borrow_mut().set_floor_exposure_mode(on);
                                    }
                                    SketchEvent::ConfirmAllStoreysAdded => {
                                        controller.borrow_mut().confirm_all_storeys_added();
                                    }
                                    SketchEvent::DeferStoreyCount => {
                                        controller.borrow_mut().defer_storey_count();
                                    }
                                    SketchEvent::SetFloorExposureSelectMode(mode) => {
                                        controller
                                            .borrow_mut()
                                            .set_floor_exposure_select_mode(mode);
                                    }
                                    SketchEvent::FloorExposureTap {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .toggle_floor_exposure_at(world_x, world_y, tol);
                                    }
                                    SketchEvent::FloorExposureCarveStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_floor_exposure_carve_stroke(&points);
                                    }
                                    SketchEvent::ChooseFloorExposureCutRegion {
                                        world_x,
                                        world_y,
                                    } => {
                                        controller.borrow_mut().choose_floor_exposure_cut_region(
                                            world_x as f32,
                                            world_y as f32,
                                        );
                                    }
                                    SketchEvent::DeselectAllFloorExposure => {
                                        controller.borrow_mut().clear_floor_exposure_selection();
                                    }
                                    SketchEvent::AssignFloorExposure(exposure) => {
                                        controller.borrow_mut().set_floor_exposure_for(exposure);
                                    }
                                    SketchEvent::SetCeilingExposureMode(on) => {
                                        controller.borrow_mut().set_ceiling_exposure_mode(on);
                                    }
                                    SketchEvent::ConfirmAllStoreysAddedCeiling => {
                                        controller.borrow_mut().confirm_all_storeys_added_ceiling();
                                    }
                                    SketchEvent::DeferStoreyCountCeiling => {
                                        controller.borrow_mut().defer_storey_count_ceiling();
                                    }
                                    SketchEvent::SetCeilingExposureSelectMode(mode) => {
                                        controller
                                            .borrow_mut()
                                            .set_ceiling_exposure_select_mode(mode);
                                    }
                                    SketchEvent::CeilingExposureTap {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .toggle_ceiling_exposure_at(world_x, world_y, tol);
                                    }
                                    SketchEvent::CeilingExposureCarveStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_ceiling_exposure_carve_stroke(&points);
                                    }
                                    SketchEvent::ChooseCeilingExposureCutRegion {
                                        world_x,
                                        world_y,
                                    } => {
                                        controller.borrow_mut().choose_ceiling_exposure_cut_region(
                                            world_x as f32,
                                            world_y as f32,
                                        );
                                    }
                                    SketchEvent::DeselectAllCeilingExposure => {
                                        controller.borrow_mut().clear_ceiling_exposure_selection();
                                    }
                                    SketchEvent::AssignCeilingExposure(exposure) => {
                                        controller.borrow_mut().set_ceiling_exposure_for(exposure);
                                    }
                                    SketchEvent::AddExternalPhoto {
                                        side,
                                        path,
                                        quality,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .add_external_photo(side, &path, quality);
                                    }
                                    SketchEvent::RemoveExternalPhoto { side } => {
                                        controller.borrow_mut().remove_external_photo(side);
                                    }
                                    SketchEvent::EditMouseDown {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .edit_mouse_down(world_x, world_y, tol);
                                    }
                                    SketchEvent::EditMouseDrag { world_x, world_y } => {
                                        controller.borrow_mut().edit_mouse_drag(world_x, world_y);
                                    }
                                    SketchEvent::EditMouseUp => {
                                        log_err(
                                            "edit_mouse_up",
                                            controller.borrow_mut().edit_mouse_up(),
                                        );
                                    }
                                    SketchEvent::ApplyEditDrag(value) => {
                                        match controller.borrow_mut().apply_edit_drag(value) {
                                            Ok(()) => {
                                                self.sketch_state.edit_drag_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("apply_edit_drag: {}", e);
                                                self.sketch_state.edit_drag_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::CycleEditDragMeasurement => {
                                        controller.borrow_mut().cycle_edit_drag_measurement();
                                        self.sketch_state.edit_drag_error = None;
                                    }
                                    SketchEvent::CancelEditDrag => {
                                        log_err(
                                            "cancel_edit_drag",
                                            controller.borrow_mut().cancel_edit_drag(),
                                        );
                                        self.sketch_state.edit_drag_error = None;
                                    }
                                    SketchEvent::EditDoubleClick {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .edit_double_click(world_x, world_y, tol);
                                    }
                                    SketchEvent::ApplyVertexMeasurement { idx, value } => {
                                        match controller
                                            .borrow_mut()
                                            .apply_vertex_measurement(idx, value)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.vertex_drop_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("apply_vertex_measurement: {}", e);
                                                self.sketch_state.vertex_drop_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::Undo => {
                                        log_err("undo", controller.borrow_mut().undo());
                                    }
                                    SketchEvent::ExtendStrokeWorld { points } => {
                                        controller.borrow_mut().apply_extend_stroke_event(&points);
                                    }
                                    SketchEvent::OutlineStrokeWorld { points } => {
                                        controller.borrow_mut().apply_outline_stroke_world(&points);
                                    }
                                    SketchEvent::TrimStrokeWorld { points } => {
                                        controller.borrow_mut().apply_trim_stroke_event(&points);
                                    }
                                    SketchEvent::Redo => {
                                        log_err("redo", controller.borrow_mut().redo());
                                    }
                                    SketchEvent::SaveCheckpoint => {
                                        log_err(
                                            "save_checkpoint",
                                            controller.borrow_mut().save_checkpoint(),
                                        );
                                    }
                                    SketchEvent::LoadCheckpoint => {
                                        // DEV tool: replaces the sketch session with
                                        // the checkpoint (same reconstruction as a
                                        // BER_CHECKPOINT=1 launch). Key Details isn't
                                        // refreshed here — the closure can't re-borrow
                                        // self — acceptable for a dev-only path.
                                        if !controller.borrow_mut().load_checkpoint() {
                                            log::warn!("load_checkpoint: no checkpoint file found");
                                        }
                                    }
                                    SketchEvent::DebugDump => {
                                        // Stdout for the terminal user, clipboard
                                        // for pasting straight into a report.
                                        let dump = controller.borrow_mut().debug_dump_current();
                                        log::info!("{dump}");
                                        ctx.copy_text(dump);
                                    }
                                    SketchEvent::CancelVertexDrop => {
                                        // Note: the toolbar already cleared `inputs`
                                        // when the Cancel button was clicked; touching
                                        // `self.sketch_state.vertex_drop_inputs` here
                                        // would double-borrow it (the toolbar still
                                        // owns the &mut reference during the closure).
                                        log_err(
                                            "cancel_vertex_drop",
                                            controller.borrow_mut().cancel_vertex_drop(),
                                        );
                                        self.sketch_state.vertex_drop_error = None;
                                    }
                                },
                                _ => {}
                            });
                        }
                    },
                );
                if exit_clicked {
                    // Flush + clear the loaded record, then return Home with
                    // all per-record page state reset (autosave means nothing
                    // is lost; no confirm dialog).
                    log_err("exit_ber", self.app_controller.borrow_mut().exit_ber());
                    self.key_details_state = KeyDetailsState::default();
                    self.home_state = HomeState::default();
                    self.sketch_state = SketchState::default();
                    self.navigate_to(Page::Home);
                } else if let Some(p) = next_page {
                    // Leaving the current section drops the active sketch tool —
                    // CAD-style: returning lands in plain navigation, not mid-tool.
                    self.app_controller.borrow_mut().enter_navigation_mode();
                    self.navigate_to(p);
                }
            }
        }
    }
}
