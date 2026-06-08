use chrono::NaiveDate;
use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;

use std::sync::mpsc::{channel, Receiver, Sender};

use crate::controller::app_controller::AppController;
use crate::ui::adapters::sketch_egui::pos2_to_point2f;
use crate::ui::pages::events::{Page, UiEvent};
use crate::ui::pages::new_ber::{parse_year, NewBerEvent};
use crate::ui::pages::sketch::{EguiStroke, SketchEvent, SketchPage};
use crate::ui::pages::{home::HomePage, new_ber::NewBerPage, splash::SplashPage};
use datatypes::ber::common::{ConstructionPhase, ConstructionPhases};

/// Surface a controller call's error (if any) to stderr with a label. Used by
/// the event-dispatch arms whose only failure handling is to log.
fn log_err(label: &str, result: Result<(), String>) {
    if let Err(e) = result {
        eprintln!("{label}: {e}");
    }
}

pub struct BerApp {
    pub current_page: Page,
    pub app_controller: Rc<RefCell<AppController>>,

    // Page-specific state
    pub splash_state: SplashState,
    pub new_ber_state: NewBerState,
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
    /// Backing string for the dimension prompt's text input. Lives across
    /// frames so the user's in-progress typing isn't lost on re-render.
    pub dimension_input: String,
    /// Last error from `apply_dimension`. Shown next to the textbox so the
    /// user can correct without losing what they typed.
    pub dimension_error: Option<String>,
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

#[derive(Default)]
pub struct NewBerState {
    pub name: String,
    pub date_str: String,
    pub date_assessed: Option<NaiveDate>,
    pub date_error: Option<String>,
    /// Year the house was built (→ `ConstructionPhase::Original`).
    pub construction_year: String,
    /// Extensions (→ `Extension1..4`), in order added. Capped at 4. Each has a
    /// user nickname ("Back extension") + a build year.
    pub extensions: Vec<ExtensionInput>,
}

/// One extension row on the New-BER form: a descriptive nickname and a year.
#[derive(Default)]
pub struct ExtensionInput {
    pub nickname: String,
    pub year: String,
}

/// Build the BER's construction phases from the New-BER form: the house year →
/// `Original`, each valid extension year → `Extension1..4`. Unparseable/blank
/// entries are skipped (the house year is gated by `can_create`).
fn collect_construction_phases(state: &NewBerState) -> ConstructionPhases {
    let mut phases = ConstructionPhases::default();
    if let Some(y) = parse_year(&state.construction_year) {
        phases.set_year(ConstructionPhase::Original, y);
    }
    let variants = [
        ConstructionPhase::Extension1,
        ConstructionPhase::Extension2,
        ConstructionPhase::Extension3,
        ConstructionPhase::Extension4,
    ];
    for (variant, ext) in variants.into_iter().zip(state.extensions.iter()) {
        if let Some(y) = parse_year(&ext.year) {
            phases.set_year(variant.clone(), y);
            phases.set_nickname(variant, ext.nickname.clone());
        }
    }
    phases
}

impl BerApp {
    pub fn new(app_controller: Rc<RefCell<AppController>>) -> Self {
        // DEV: `BER_CHECKPOINT=1 cargo run` loads the saved checkpoint plan and
        // jumps straight to the sketch page, so a test case needn't be redrawn.
        let current_page = if std::env::var("BER_CHECKPOINT").is_ok()
            && app_controller.borrow_mut().load_checkpoint()
        {
            Page::Sketch
        } else {
            Page::Splash
        };
        Self {
            current_page,
            app_controller,
            splash_state: SplashState::default(),
            new_ber_state: NewBerState::default(),
            sketch_state: SketchState::default(),
            last_seen_revision: 0,
            last_change_time: 0.0,
        }
    }

    pub fn navigate_to(&mut self, page: Page) {
        self.current_page = page;
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
                // No-op if nothing is pending; otherwise one SQLite write.
                let _ = self.app_controller.borrow_mut().flush_if_dirty();
            }
        }
        // Check for progress updates if on splash screen
        if self.current_page == Page::Splash {
            self.check_progress();
            ctx.request_repaint(); // Keep updating while loading
        }

        let page = self.current_page;
        // Render current page
        egui::CentralPanel::default().show(ctx, |ui| match page {
            Page::Splash => SplashPage::render(
                ui,
                self.splash_state.progress_text.clone(),
                self.splash_state.progress_value,
                |_| {},
            ),
            Page::Home => {
                HomePage::render(ui, |event| match event {
                    UiEvent::NavigateTo(p) => self.navigate_to(p),
                    _ => {}
                });
            }
            Page::Sketch => {
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
                    view,
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
                        crate::ui::render::ViewParams {
                            zoom: v.zoom,
                            offset_x: v.offset_x,
                            offset_y: v.offset_y,
                        },
                    )
                };

                let current_storey_idx = self.app_controller.borrow().current_storey_idx();

                let controller = self.app_controller.clone();
                let splice_debug = self.app_controller.borrow().splice_debug_overlay();
                SketchPage::render(
                    ui,
                    stroke.as_ref().map(EguiStroke::from),
                    mode,
                    display_mode,
                    edit_mode,
                    ber.as_ref(),
                    current_storey_idx,
                    review.as_ref(),
                    review_remaining,
                    has_floor_plan,
                    pending_redraw,
                    dim_prompt,
                    &mut self.sketch_state.dimension_input,
                    dim_scale_set,
                    dim_error.as_deref(),
                    &dimensioned_walls,
                    arc_preview.as_deref(),
                    curve_bulge,
                    living_area_mode,
                    &living_area_polygons,
                    &living_area_wall_labels,
                    active_living_area,
                    &pending_cut_regions,
                    exposure_mode,
                    &exposure_walls,
                    exposure_selection,
                    &mut self.sketch_state.exposure_start_input,
                    &mut self.sketch_state.exposure_end_input,
                    phase_mode,
                    &phase_region_polygons,
                    &pending_phase_polygon,
                    &pending_phase_cut_regions,
                    &phase_choices,
                    phase_pick_pending,
                    edit_selected_wall,
                    edit_selected_vertex,
                    edit_drag_prompt,
                    &mut self.sketch_state.edit_drag_input,
                    edit_drag_error.as_deref(),
                    vertex_drop_prompt,
                    &mut self.sketch_state.vertex_drop_inputs,
                    vertex_drop_error.as_deref(),
                    can_undo,
                    can_redo,
                    view,
                    &splice_debug,
                    |event| match event {
                        UiEvent::Sketch(sketch_event) => match sketch_event {
                            SketchEvent::AddPoint(p) => controller
                                .borrow_mut()
                                .sketch_add_point(pos2_to_point2f(p))
                                .expect("Failed to add point"),
                            SketchEvent::EndStroke(p) => controller
                                .borrow_mut()
                                .sketch_end_stroke(pos2_to_point2f(p))
                                .expect("Failed to end stroke"),
                            SketchEvent::StartStroke(p) => log_err(
                                "sketch_start_stroke",
                                controller
                                    .borrow_mut()
                                    .sketch_start_stroke(pos2_to_point2f(p)),
                            ),
                            SketchEvent::ChangeMode(m) => {
                                controller.borrow_mut().set_sketch_mode(m)
                            }
                            SketchEvent::ChangeDisplay(m) => {
                                controller.borrow_mut().set_display_mode(m)
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
                            SketchEvent::ApplyDimension(value) => {
                                match controller.borrow_mut().apply_dimension(value) {
                                    Ok(()) => {
                                        self.sketch_state.dimension_error = None;
                                    }
                                    Err(e) => {
                                        eprintln!("apply_dimension: {}", e);
                                        self.sketch_state.dimension_error = Some(e);
                                    }
                                }
                            }
                            SketchEvent::SkipDimension => {
                                controller.borrow_mut().skip_current_dimension();
                                self.sketch_state.dimension_error = None;
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
                                controller
                                    .borrow_mut()
                                    .choose_phase_cut_region(world_x as f32, world_y as f32);
                            }
                            SketchEvent::CancelPendingPhase => {
                                controller.borrow_mut().cancel_pending_phase();
                            }
                            SketchEvent::DeletePhaseRegion(index) => {
                                controller.borrow_mut().delete_phase_region(index);
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
                            SketchEvent::SetCurrentStorey(target) => {
                                log_err(
                                    "set_current_storey",
                                    controller.borrow_mut().set_current_storey(target),
                                );
                            }
                            SketchEvent::AddStorey => {
                                log_err("add_storey", controller.borrow_mut().add_storey());
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
                                log_err("edit_mouse_up", controller.borrow_mut().edit_mouse_up());
                            }
                            SketchEvent::ApplyEditDrag(value) => {
                                match controller.borrow_mut().apply_edit_drag(value) {
                                    Ok(()) => {
                                        self.sketch_state.edit_drag_error = None;
                                    }
                                    Err(e) => {
                                        eprintln!("apply_edit_drag: {}", e);
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
                                match controller.borrow_mut().apply_vertex_measurement(idx, value) {
                                    Ok(()) => {
                                        self.sketch_state.vertex_drop_error = None;
                                    }
                                    Err(e) => {
                                        eprintln!("apply_vertex_measurement: {}", e);
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
                            _ => {}
                        },
                        _ => {}
                    },
                );
            }
            Page::NewBer => {
                // Collect intents from the form closure first, then act after the
                // closure releases its borrow of self.new_ber_state.
                let mut create_clicked = false;
                let mut next_page: Option<Page> = None;

                NewBerPage::render(ui, &mut self.new_ber_state, |event| match event {
                    UiEvent::NavigateTo(p) => next_page = Some(p),
                    UiEvent::NewBer(NewBerEvent::CreateClicked) => {
                        create_clicked = true;
                    }
                    _ => {}
                });

                if create_clicked {
                    let name = std::mem::take(&mut self.new_ber_state.name);
                    let date = self
                        .new_ber_state
                        .date_assessed
                        .unwrap_or_else(|| chrono::Local::now().naive_local().date());
                    let phases = collect_construction_phases(&self.new_ber_state);
                    {
                        let mut ctrl = self.app_controller.borrow_mut();
                        ctrl.create_ber(name, date);
                        ctrl.set_construction_phases(phases);
                    }
                    self.new_ber_state = NewBerState::default();
                    next_page = Some(Page::Home);
                }

                if let Some(p) = next_page {
                    self.navigate_to(p);
                }
            }
            Page::KeyDeets => {
                ui.heading("Key Details");
                ui.label("TODO: Implement key details page");
            }
            Page::MoreDeets => {
                ui.heading("More Details");
                ui.label("TODO: Implement more details page");
            }
        });
    }
}
