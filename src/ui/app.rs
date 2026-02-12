use chrono::NaiveDate;
use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;

use std::sync::mpsc::{channel, Receiver, Sender};

use crate::controller::app_controller::AppController;
use crate::ui::pages::events::{Page, UiEvent};
use crate::ui::pages::sketch::{EguiStroke, SketchEvent, SketchPage};
use crate::ui::pages::{home::HomePage, new_ber::NewBerPage, splash::SplashPage};

pub struct BerApp {
    pub current_page: Page,
    pub app_controller: Rc<RefCell<AppController>>,

    // Page-specific state
    pub splash_state: SplashState,
    pub new_ber_state: NewBerState,
    // Add other page states as needed
    // pub sketch_state: SketchState,
    // pub key_deets_state: KeyDeetsState,
    // pub more_deets_state: MoreDeetsState,
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
                let (stroke, mode) = {
                    let controller_ref = self.app_controller.borrow();
                    (controller_ref.sketch_stroke(), controller_ref.sketch_mode())
                };

                // 2️⃣ Convert to egui strokes via adapter
                //let egui_strokes: EguiStroke = adapt_strokes(&strokes);
                let controller = self.app_controller.clone();
                SketchPage::render(
                    ui,
                    stroke.as_ref().map(EguiStroke::from),
                    mode.into(),
                    |event| match event {
                        //SketchEvent::StartStroke(p) => self.app_controller.sketch_start_stroke(p),
                        // SketchEvent::AddPoint(p) => self.app_controller.sketch_add_point(p),
                        // SketchEvent::EndStroke => self.app_controller.sketch_end_stroke(),
                        UiEvent::Sketch(sketch_event) => match sketch_event {
                            SketchEvent::ChangeMode(m) => {
                                controller.borrow_mut().set_sketch_mode(m.into())
                            }
                            _ => {}
                        },
                        _ => {}
                    },
                );
            }
            Page::NewBer => {
                NewBerPage::render(ui, |event| match event {
                    UiEvent::NavigateTo(p) => self.navigate_to(p),
                    _ => {}
                });
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
