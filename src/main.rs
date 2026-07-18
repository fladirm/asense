mod app;

// Keep the GUI module's existing `crate::…` paths as thin aliases into the
// shared GUI-free library.  `asensed` links the same library without enabling
// any desktop dependency.
pub use asense_core::{control, hardware, lighting, nvidia, platform, telemetry, tuning};

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let args: Vec<String> = env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("--probe") => probe(),
        Some("--toggle") => toggle(),
        Some(flag) if flag.starts_with('-') => Err(format!("unknown option: {flag}")),
        _ => {
            app::launch();
            Ok(())
        }
    };

    if let Err(error) = result {
        eprintln!("asense: {error}");
        std::process::exit(1);
    }
}

fn toggle() -> Result<(), String> {
    if let Some(pid) = running_gui_pid()? {
        // SAFETY: pid was read from procfs, belongs to our effective UID, and
        // its /proc/<pid>/exe resolves to this exact executable.
        let result = unsafe { libc::kill(pid, libc::SIGTERM) };
        if result != 0 {
            return Err(format!(
                "cannot close running ASense window: {}",
                std::io::Error::last_os_error()
            ));
        }
    } else {
        app::launch();
    }
    Ok(())
}

fn running_gui_pid() -> Result<Option<i32>, String> {
    let own_pid = std::process::id();
    let own_uid = effective_uid()?;
    let own_exe = fs::canonicalize(
        env::current_exe().map_err(|error| format!("cannot resolve ASense executable: {error}"))?,
    )
    .map_err(|error| format!("cannot canonicalize ASense executable: {error}"))?;

    let proc = fs::read_dir("/proc").map_err(|error| format!("cannot inspect procfs: {error}"))?;
    for entry in proc.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own_pid || process_uid(pid) != Some(own_uid) {
            continue;
        }
        let root = entry.path();
        if !same_executable(&root, &own_exe) || !is_gui_process(&root) {
            continue;
        }
        let pid = i32::try_from(pid).map_err(|_| "process id exceeds i32".to_string())?;
        return Ok(Some(pid));
    }
    Ok(None)
}

fn effective_uid() -> Result<u32, String> {
    process_uid(std::process::id()).ok_or_else(|| "cannot determine effective UID".to_string())
}

fn process_uid(pid: u32) -> Option<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_ascii_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn same_executable(process_root: &Path, own_exe: &Path) -> bool {
    fs::read_link(process_root.join("exe"))
        .ok()
        .and_then(|path| fs::canonicalize(path).ok())
        .is_some_and(|path| path == own_exe)
}

fn is_gui_process(process_root: &Path) -> bool {
    let Ok(command_line) = fs::read(process_root.join("cmdline")) else {
        return false;
    };
    let arguments: Vec<&[u8]> = command_line
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
        .collect();
    is_gui_arguments(&arguments)
}

fn is_gui_arguments(arguments: &[&[u8]]) -> bool {
    arguments.len() == 1 || (arguments.len() == 2 && arguments[1] == b"--toggle")
}

fn probe() -> Result<(), String> {
    let hardware = hardware::AcerHardware::discover().map_err(|error| error.to_string())?;
    let telemetry = telemetry::TelemetryReader::new()
        .sample(&hardware)
        .map_err(|error| error.to_string())?;
    println!("{telemetry:#?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_gui_arguments;

    #[test]
    fn only_real_gui_invocations_are_toggle_targets() {
        assert!(is_gui_arguments(&[b"asense"]));
        assert!(is_gui_arguments(&[b"asense", b"--toggle"]));
        assert!(!is_gui_arguments(&[b"asense", b"--probe"]));
        assert!(!is_gui_arguments(&[b"asense", b"--toggle", b"extra"]));
    }
}
