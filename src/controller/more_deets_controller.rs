use super::app_controller::AppController;
use crate::MoreDeetsWindow;
use slint::Weak;
use std::rc::Rc;

pub struct MoreDeetsController {
    ui: Weak<MoreDeetsWindow>,
    app_controller: Rc<AppController>,
}

impl MoreDeetsController {
    pub fn new(ui: Weak<MoreDeetsWindow>, app_controller: Rc<AppController>) -> Self {
        Self { ui, app_controller }
    }

    pub fn setup_callbacks(&self, _ui: &MoreDeetsWindow) {
        // Logic for more detailed info display
    }
}
