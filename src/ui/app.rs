use chrono::NaiveDate;
use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;

use std::sync::mpsc::{channel, Receiver, Sender};

use crate::controller::app_controller::AppController;
use crate::ui::adapters::sketch_egui::pos2_to_point2f;
use crate::ui::pages::events::{Page, UiEvent};
use crate::ui::pages::new_ber::NewBerEvent;
use crate::ui::pages::sketch::{EguiStroke, SketchEvent, SketchPage};
use crate::ui::pages::{home::HomePage, new_ber::NewBerPage, splash::SplashPage};

pub struct BerApp {
    pub current_page: Page,
    pub app_controller: Rc<RefCell<AppController>>,

    // Page-specific state
    pub splash_state: SplashState,
    pub new_ber_state: NewBerState,
    pub sketch_state: SketchState,
}

#[derive(Default)]
pub struct SketchState {
    /// Backing string for the dimension prompt's text input. Lives across
    /// frames so the user's in-progress typing isn't lost on re-render.
    pub dimension_input: String,
    /// Last error from `apply_dimension`. Shown next to the textbox so the
    /// user can correct without losing what they typed.
    pub dimension_error: Option<String>,
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
}

impl BerApp {
    pub fn new(app_controller: Rc<RefCell<AppController>>) -> Self {
        Self {
            current_page: Page::Splash,
            app_controller,
            splash_state: SplashState::default(),
            new_ber_state: NewBerState::default(),
            sketch_state: SketchState::default(),
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.splash_state.loading_started {
            self.start_loading();
            self.splash_state.loading_started = true;
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
                        controller_ref.dimensioned_walls(),
                        crate::ui::render::ViewParams {
                            zoom: v.zoom,
                            offset_x: v.offset_x,
                            offset_y: v.offset_y,
                        },
                    )
                };

                // TODO: track a real current_storey_idx on the controller once we
                // support multi-storey navigation. For now we always edit storey 0.
                let current_storey_idx = 0;

                let controller = self.app_controller.clone();
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
                    view,
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
                            SketchEvent::StartStroke(p) => {
                                if let Err(e) = controller
                                    .borrow_mut()
                                    .sketch_start_stroke(pos2_to_point2f(p))
                                {
                                    eprintln!("sketch_start_stroke: {}", e);
                                }
                            }
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
                                if let Err(e) = controller.borrow_mut().accept_current_review() {
                                    eprintln!("accept_current_review: {}", e);
                                }
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
                                if let Err(e) = controller.borrow_mut().confirm_replace_storey() {
                                    eprintln!("confirm_replace_storey: {}", e);
                                }
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
                    self.app_controller.borrow_mut().create_ber(name, date);
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
