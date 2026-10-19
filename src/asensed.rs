use std::env;

fn main() {
    let result = match env::args().nth(1).as_deref() {
        None | Some("--daemon") => asense_core::daemon::run(),
        Some("--failsafe-auto") => match asense_core::daemon::failsafe_auto() {
            Ok(asense_core::daemon::FailsafeFanReset::Verified) => Ok(()),
            Ok(asense_core::daemon::FailsafeFanReset::NotExposed) => {
                eprintln!(
                    "asensed: no fan-control interface exposed; firmware Auto was not verified"
                );
                std::process::exit(3);
            }
            Err(error) => Err(error),
        },
        Some("--resume") => asense_core::daemon::resume_after_sleep(),
        Some("--probe") => asense_core::daemon::probe(),
        Some(option) => Err(format!("unknown asensed option: {option}")),
    };

    if let Err(error) = result {
        eprintln!("asensed: {error}");
        std::process::exit(1);
    }
}
