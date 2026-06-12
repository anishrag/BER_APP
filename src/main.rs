use ber_app::paths::Paths;
use ber_app::ui;

fn main() -> Result<(), ui::RunError> {
    // Route the app's and library crates' debug traces (`ber_app`, `document`,
    // `manip_2d_plan`) to stdout with a message-only format. Default filter
    // keeps dependency noise out; override with `RUST_LOG` (e.g.
    // `RUST_LOG=warn` to quiet the traces).
    use std::io::Write;
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("error,ber_app=debug,document=debug,manip_2d_plan=debug"),
    )
    .target(env_logger::Target::Stdout)
    .format(|buf, record| writeln!(buf, "{}", record.args()))
    .init();

    // Desktop keeps the cwd-relative data dir (./bers.db) by design — dev data
    // lives next to the repo. Android resolves its own dir in android_main.
    let controller = ber_app::bootstrap(&Paths::desktop());
    ui::run(controller)
}
