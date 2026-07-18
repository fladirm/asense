use std::env;

fn main() {
    let result = match env::args().nth(1).as_deref() {
        None | Some("--daemon") => asense_core::daemon::run(),
        Some("--failsafe-auto") => asense_core::daemon::failsafe_auto(),
        Some("--resume") => asense_core::daemon::resume_after_sleep(),
        Some("--probe") => asense_core::daemon::probe(),
        Some(option) => Err(format!("unknown asensed option: {option}")),
    };

    if let Err(error) = result {
        eprintln!("asensed: {error}");
        std::process::exit(1);
    }
}
