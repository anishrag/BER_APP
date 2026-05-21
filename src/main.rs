use std::cell::RefCell;
use std::rc::Rc;

mod controller;
mod engine;
mod ui;

use controller::app_controller::AppController;

fn main() -> Result<(), ui::RunError> {
    let conn = engine::open_db().expect("Failed to open database");
    let controller = Rc::new(RefCell::new(AppController::new(conn)));
    ui::run(controller)
}
