use std::cell::RefCell;
use std::rc::Rc;

mod config;
mod controller;
mod engine;
mod ui;

use controller::app_controller::AppController;

fn main() -> Result<(), ui::RunError> {
    // Route the library crates' debug traces (`document`, `manip_2d_plan`) to
    // stdout with a message-only format, so they read exactly like the controller's
    // own `println!` lines. Default filter keeps dependency noise out; override
    // with `RUST_LOG` (e.g. `RUST_LOG=warn` to quiet the traces).
    use std::io::Write;
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("error,document=debug,manip_2d_plan=debug"),
    )
    .target(env_logger::Target::Stdout)
    .format(|buf, record| writeln!(buf, "{}", record.args()))
    .init();

    // TODO: replace with platform-appropriate data dir (e.g. ~/.local/share/ber_app)
    // once we package for distribution. cwd is fine for development.
    let conn = engine::open_db("bers.db").expect("Failed to open database");
    let mut controller = AppController::new(conn);
    // Background writer (its own connection, WAL) so idle autosaves don't block
    // the UI on SQLite. If it can't spawn, fall back to synchronous saves.
    match engine::PersistenceWriter::spawn("bers.db".to_string()) {
        Ok(writer) => controller.attach_writer(writer),
        Err(e) => eprintln!("persistence writer unavailable, saving synchronously: {e}"),
    }
    let controller = Rc::new(RefCell::new(controller));
    ui::run(controller)
}
