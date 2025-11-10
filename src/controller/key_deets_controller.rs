use super::app_controller::AppController;
use crate::KeyDeetsWindow;
use slint::Weak;
use std::rc::Rc;

pub struct KeyDeetsController {
    ui: Weak<KeyDeetsWindow>,
    app_controller: Rc<AppController>,
}

impl KeyDeetsController {
    pub fn new(ui: Weak<KeyDeetsWindow>, app_controller: Rc<AppController>) -> Self {
        Self { ui, app_controller }
    }

    pub fn setup_callbacks(&self, _ui: &KeyDeetsWindow) {
        // Logic for updating / formatting key details
    }
}
