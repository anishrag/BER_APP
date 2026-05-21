use std::cell::RefCell;
use std::rc::Rc;

mod controller;
mod engine;
mod ui;

use controller::app_controller::AppController;

fn main() -> Result<(), ui::RunError> {
    // TODO: replace with platform-appropriate data dir (e.g. ~/.local/share/ber_app)
    // once we package for distribution. cwd is fine for development.
    let conn = engine::open_db("bers.db").expect("Failed to open database");
    let controller = Rc::new(RefCell::new(AppController::new(conn)));
    ui::run(controller)
}
