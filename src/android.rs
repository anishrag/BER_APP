//! Android entry point (crate-type `cdylib`; called by android-activity's
//! glue). Names only winit/android_logger types — eframe stays confined to
//! `src/ui` (the `AndroidApp` handle passes through opaquely).

use winit::platform::android::activity::AndroidApp;

#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            .with_tag("ber_app"),
    );

    let data_dir = app
        .internal_data_path()
        .expect("android: no internal data path for this activity");
    let paths = crate::paths::Paths::in_dir(data_dir);
    let controller = crate::bootstrap(&paths);
    if let Err(e) = crate::ui::run_android(controller, app) {
        log::error!("eframe exited with error: {e}");
    }
}
