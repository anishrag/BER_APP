use std::rc::Rc;
use slint::Weak;
use crate::SketchWindow;
use crate::controller::app_controller::AppController;

pub struct SketchWindowController {
    ui: Weak<SketchWindow>,
    app_controller: Rc<AppController>,
}

impl SketchWindowController {
    pub fn new(ui: Weak<SketchWindow>, app_controller: Rc<AppController>) -> Self {
        Self { ui, app_controller }
    }

    pub fn setup_callbacks(&self, _ui: &SketchWindow) {
        // Setup stroke events, canvas updates, etc. here
    }
}

