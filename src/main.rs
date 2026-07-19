mod app;
mod gui_instance;

// Keep the GUI module's existing `crate::…` paths as thin aliases into the
// shared GUI-free library.  `asensed` links the same library without enabling
// any desktop dependency.
pub use asense_core::{control, hardware, lighting, nvidia, platform, probe, telemetry, tuning};

use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("--probe" | "probe") => print_probe(),
        Some("--toggle") => launch_gui(gui_instance::Mode::Toggle),
        Some(flag) if flag.starts_with('-') => Err(format!("unknown option: {flag}")),
        _ => launch_gui(gui_instance::Mode::Open),
    };

    if let Err(error) = result {
        eprintln!("asense: {error}");
        std::process::exit(1);
    }
}

fn launch_gui(mode: gui_instance::Mode) -> Result<(), String> {
    if let Some(lease) = gui_instance::claim(mode)? {
        app::launch();
        drop(lease);
    }
    Ok(())
}

fn print_probe() -> Result<(), String> {
    print!("{}", probe::generate()?);
    Ok(())
}
