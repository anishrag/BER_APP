use crate::controller::app_controller::AppController;
use crate::HomeWindow;
use slint::Weak;
use std::rc::Rc;

pub struct HomeController {
    ui: Weak<HomeWindow>,
    app_controller: Rc<AppController>,
}

impl HomeController {
    pub fn new(ui: Weak<HomeWindow>, app_controller: Rc<AppController>) -> Self {
        Self { ui, app_controller }
    }

    pub fn setup_callbacks(&self, _ui: &HomeWindow) {
        // nothing needed for now
    }
}
