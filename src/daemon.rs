use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::control::{
    CONTROL_PROTOCOL_VERSION, CONTROL_SOCKET, ControlCapabilities, ControlFanCapabilities,
    ControlFanRpmChannel, MAX_CONTROL_COMMAND_BYTES, MAX_CONTROL_RESPONSE_PAYLOAD_BYTES,
    ProfileApplyReceipt, ProfilePowerReceipt, encode_control_capabilities,
    encode_profile_apply_receipt,
};
use crate::hardware::{
    AcerHardware, FanBackend, FanMode as HardwareFanMode, FanSetting, PlatformProfile,
};
use crate::lighting::{
    KeyboardLighting, LightingBackend, LightingController, LightingMode, LightingRequest,
    LightingStateStatus, encode_state,
};
use crate::mutation_lock::MutationGuard;
use crate::nvidia::discover_nvidia_pci_device;
use crate::platform::{
    PlatformControls, RearLogoState, UsbCharging, encode_state as encode_platform_state,
};
use crate::telemetry::{
    MemoryHardwareInfo, encode_memory_hardware, read_privileged_memory_hardware,
};
use crate::tuning::{ProfileController, TuningState};

static MEMORY_HARDWARE: OnceLock<MemoryHardwareInfo> = OnceLock::new();
const ENEK_LIGHTING_CACHE: &str = "/var/lib/asense/enek5130.json";
const NVIDIA_RESUME_PENDING: &str = "/run/asense-nvidia-reconcile";
const NVIDIA_RECONCILE_RETRY: Duration = Duration::from_secs(30);
const NVIDIA_RECONCILE_POLL: Duration = Duration::from_secs(1);
const MAX_LIGHTING_CACHE_BYTES: u64 = 2048;
const MAX_CACHED_LIGHTING_TARGETS: usize = 4;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CachedLighting {
    schema: u8,
    requests: Vec<LightingRequest>,
}

/// Incremental, allocation-bounded line decoder for the privileged protocol.
///
/// `BufRead::read_line` grows its destination until it sees a newline.  A
/// local client could therefore make the root helper allocate an arbitrary
/// amount of memory before the old post-read size check ran. This decoder
/// retains at most `MAX_COMMAND_BYTES` across receive timeouts and terminates
/// the connection immediately at the limit, so a newline-free stream cannot
/// monopolize the single privileged client loop.
struct CommandDecoder {
    pending: Vec<u8>,
}

impl CommandDecoder {
    fn new() -> Self {
        Self {
            pending: Vec::with_capacity(MAX_CONTROL_COMMAND_BYTES),
        }
    }

    /// Returns `Ok(None)` only on a clean EOF with no pending command.
    /// A partial frame at EOF is rejected because newline is the protocol's
    /// commit boundary; transport closure must never execute pending bytes.
    fn read(
        &mut self,
        reader: &mut impl BufRead,
    ) -> std::io::Result<Option<Result<String, String>>> {
        loop {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                self.pending.clear();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "command ended before newline",
                ));
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |position| position + 1);
            let payload_end = newline.unwrap_or(available.len());
            let within_limit = self.push(&available[..payload_end]);
            reader.consume(consumed);

            if !within_limit {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "command exceeded 192 bytes",
                ));
            }

            if newline.is_some() {
                return Ok(Some(self.finish()));
            }
        }
    }

    fn push(&mut self, bytes: &[u8]) -> bool {
        let remaining = MAX_CONTROL_COMMAND_BYTES.saturating_sub(self.pending.len());
        if bytes.len() > remaining {
            self.pending.clear();
            false
        } else {
            self.pending.extend_from_slice(bytes);
            true
        }
    }

    fn finish(&mut self) -> Result<String, String> {
        let bytes = std::mem::take(&mut self.pending);
        self.pending = Vec::with_capacity(MAX_CONTROL_COMMAND_BYTES);
        String::from_utf8(bytes).map_err(|_| "command must be valid UTF-8".to_string())
    }

    #[cfg(test)]
    fn retained_bytes(&self) -> usize {
        self.pending.len()
    }
}

pub fn run() -> Result<(), String> {
    ensure_root()?;
    if let Err(error) = restore_cached_enek_lighting() {
        eprintln!("asense ENEK lighting restore skipped: {error}");
    }
    let listener = activated_listener()?;
    let mut next_nvidia_reconciliation = Instant::now();
    loop {
        if Path::new(NVIDIA_RESUME_PENDING).exists() && Instant::now() >= next_nvidia_reconciliation
        {
            let result = AcerHardware::discover()
                .map_err(|error| error.to_string())
                .and_then(|hardware| reconcile_pending_nvidia(&hardware));
            next_nvidia_reconciliation = Instant::now()
                + if result.is_ok() {
                    NVIDIA_RECONCILE_POLL
                } else {
                    NVIDIA_RECONCILE_RETRY
                };
            if let Err(error) = result {
                eprintln!("asense deferred NVIDIA reconciliation warning: {error}");
            }
        }
        let mut ready = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `ready` points to one initialized pollfd and the listener
        // remains open for the complete bounded wait.
        let status = unsafe { libc::poll(&mut ready, 1, NVIDIA_RECONCILE_POLL.as_millis() as i32) };
        if status == 0 {
            continue;
        }
        if status < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("wait for client failed: {error}"));
        }
        let (stream, _) = listener
            .accept()
            .map_err(|error| format!("accept failed: {error}"))?;
        if let Err(error) = serve_client(stream) {
            eprintln!("asense daemon client error: {error}");
        }
    }
}

pub fn failsafe_auto() -> Result<(), String> {
    ensure_root()?;
    let _guard = MutationGuard::acquire()?;
    let hardware = AcerHardware::discover().map_err(|error| error.to_string())?;
    if hardware.capabilities().fans.backend.is_some() {
        hardware
            .apply_fan_setting(FanSetting::Automatic)
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        Ok(())
    }
}

/// Privileged installation/runtime probe.
///
/// Installation verifies that discovery and protocol encoding work, but an
/// absent optional controller is not an installation failure. The same
/// capability snapshot is returned by `CAPS` after socket negotiation.
pub fn probe() -> Result<(), String> {
    ensure_root()?;
    let hardware = AcerHardware::discover().map_err(|error| error.to_string())?;
    let capabilities = collect_control_capabilities(&hardware)?;
    println!(
        "{}",
        encode_control_capabilities(&capabilities).map_err(String::from)?
    );
    Ok(())
}

/// Post-resume safety and split-control reconciliation.
///
/// The kernel PM callback restores the last confirmed keyboard engine state.
/// Here firmware regains fan ownership first, then the optional NVIDIA OEM
/// offsets are reconciled with the profile that survived suspend. Custom
/// offsets remain fail-closed because `set_profile` refuses to overwrite them.
pub fn resume_after_sleep() -> Result<(), String> {
    ensure_root()?;
    let _guard = MutationGuard::acquire()?;
    let hardware = AcerHardware::discover().map_err(|error| error.to_string())?;
    if hardware.capabilities().fans.backend.is_some() {
        hardware
            .apply_fan_setting(FanSetting::Automatic)
            .map_err(|error| format!("post-resume fan safety: {error}"))?;
    }
    if hardware.is_reference_model() {
        let pending = Path::new(NVIDIA_RESUME_PENDING);
        // Record intent before trying NVML. Driver reloads and RTD3 transitions
        // are normal around resume; a transient failure must not silently lose
        // the PHN16-72 offset reconciliation.
        write_pending_nvidia_reconciliation(pending)?;
        if let Some(device) = discover_nvidia_pci_device(Path::new("/"))
            && device.is_exact_oem_target()
            && device.runtime_status.permits_live_nvml()
        {
            match reconcile_reference_profile(&hardware, "post-resume") {
                Ok(()) => remove_pending_nvidia_reconciliation(pending)?,
                Err(error) => {
                    eprintln!("asense post-resume NVIDIA reconciliation deferred: {error}")
                }
            }
        }
    }
    restore_cached_enek_lighting()
        .map_err(|error| format!("post-resume ENEK lighting restore: {error}"))?;
    Ok(())
}

fn reconcile_reference_profile(hardware: &AcerHardware, context: &str) -> Result<(), String> {
    let profile = hardware
        .current_profile()
        .map_err(|error| format!("{context} profile readback: {error}"))?;
    let state = ProfileController::discover()
        .map_err(|error| error.to_string())?
        .set_profile(hardware, profile)
        .map_err(|error| format!("{context} profile reconciliation: {error}"))?;
    require_reconciled_nvidia(&state, context)
}

fn require_reconciled_nvidia(state: &TuningState, context: &str) -> Result<(), String> {
    match state.gpu_capability_error.as_deref() {
        None => Ok(()),
        Some(error) => Err(format!(
            "{context} NVIDIA offset reconciliation remains pending: {error}"
        )),
    }
}

fn write_pending_nvidia_reconciliation(path: &Path) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("cannot defer NVIDIA resume reconciliation: {error}"))?;
    file.write_all(b"pending\n")
        .map_err(|error| format!("cannot record deferred NVIDIA reconciliation: {error}"))
}

fn remove_pending_nvidia_reconciliation(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot clear deferred NVIDIA reconciliation: {error}"
        )),
    }
}

fn finish_pending_nvidia_reconciliation(
    path: &Path,
    result: Result<(), String>,
) -> Result<(), String> {
    result?;
    remove_pending_nvidia_reconciliation(path)
}

fn reconcile_pending_nvidia(hardware: &AcerHardware) -> Result<(), String> {
    let path = Path::new(NVIDIA_RESUME_PENDING);
    if !path.exists() {
        return Ok(());
    }
    if !hardware.is_reference_model() {
        return remove_pending_nvidia_reconciliation(path);
    }
    let Some(device) = discover_nvidia_pci_device(Path::new("/")) else {
        return Ok(());
    };
    if !device.is_exact_oem_target() || !device.runtime_status.permits_live_nvml() {
        return Ok(());
    }

    let _guard = MutationGuard::acquire()?;
    // Resume owns the same lock. It may have completed and removed the marker,
    // or the GPU may have returned to RTD3 while this daemon was waiting.
    if !path.exists() {
        return Ok(());
    }
    let Some(device) = discover_nvidia_pci_device(Path::new("/")) else {
        return Ok(());
    };
    if !device.is_exact_oem_target() || !device.runtime_status.permits_live_nvml() {
        return Ok(());
    }
    finish_pending_nvidia_reconciliation(
        path,
        reconcile_reference_profile(hardware, "deferred post-resume"),
    )
}

fn restore_cached_enek_lighting() -> Result<(), String> {
    let requests = load_cached_lighting(Path::new(ENEK_LIGHTING_CACHE))?;
    if requests.is_empty() {
        return Ok(());
    }
    let controllers = LightingController::discover_all();
    let mut failures = Vec::new();
    for request in requests {
        if let Some(controller) = controllers.iter().find(|controller| {
            controller.backend() == LightingBackend::Enek5130
                && controller
                    .devices()
                    .iter()
                    .any(|device| device.target == request.target)
        }) && let Err(error) = controller.apply(&request)
        {
            failures.push(error);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn load_cached_lighting(path: &Path) -> Result<Vec<LightingRequest>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot open lighting cache: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_LIGHTING_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read lighting cache: {error}"))?;
    if bytes.len() as u64 > MAX_LIGHTING_CACHE_BYTES {
        return Err("lighting cache is too large".to_string());
    }
    let cached: CachedLighting = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid lighting cache: {error}"))?;
    if cached.schema != 1 {
        return Err("unsupported lighting cache schema".to_string());
    }
    if cached.requests.len() > MAX_CACHED_LIGHTING_TARGETS {
        return Err("lighting cache has too many targets".to_string());
    }
    Ok(cached.requests)
}

fn save_cached_lighting(request: &LightingRequest) -> Result<(), String> {
    save_cached_lighting_at(Path::new(ENEK_LIGHTING_CACHE), request)
}

fn save_cached_lighting_at(path: &Path, request: &LightingRequest) -> Result<(), String> {
    let mut requests = load_cached_lighting(path)?;
    requests.retain(|cached| cached.target != request.target);
    requests.push(request.clone());
    let bytes = serde_json::to_vec(&CachedLighting {
        schema: 1,
        requests,
    })
    .map_err(|error| format!("cannot encode lighting cache: {error}"))?;
    if bytes.len() as u64 > MAX_LIGHTING_CACHE_BYTES {
        return Err("lighting cache is too large".to_string());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create lighting cache directory: {error}"))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("cannot open lighting cache for writing: {error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("cannot write lighting cache: {error}"))
}

fn activated_listener() -> Result<UnixListener, String> {
    let pid_ok = env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(std::process::id());
    let fd_count = env::var("LISTEN_FDS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    if !pid_ok || fd_count != 1 {
        return Err("daemon requires exactly one systemd-activated socket".to_string());
    }
    // SAFETY: systemd guarantees fd 3 is the single listening socket described by LISTEN_FDS.
    Ok(unsafe { UnixListener::from_raw_fd(3) })
}

fn serve_client(mut stream: UnixStream) -> Result<(), String> {
    authorize_peer(&stream)?;
    let mut hardware = AcerHardware::discover().map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|error| format!("set watchdog timeout: {error}"))?;
    let read_stream = stream.try_clone().map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(read_stream);
    let mut decoder = CommandDecoder::new();
    let mut protocol = ProtocolSession::new();
    let mut fan_session = FanSessionState::Automatic;
    let mut next_nvidia_reconciliation = Instant::now();

    let result = (|| -> Result<(), String> {
        loop {
            if Instant::now() >= next_nvidia_reconciliation
                && let Err(error) = reconcile_pending_nvidia(&hardware)
            {
                eprintln!("asense deferred NVIDIA reconciliation warning: {error}");
                next_nvidia_reconciliation = Instant::now() + NVIDIA_RECONCILE_RETRY;
            }
            enforce_thermal_watchdog(&mut hardware, &mut fan_session)?;
            let command = match decoder.read(&mut reader) {
                Ok(Some(command)) => command,
                Ok(None) => break Ok(()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(error) => break Err(format!("read failed: {error}")),
            };
            let command = match command {
                Ok(command) => command,
                Err(error) => {
                    write_response(&mut stream, Err(error))?;
                    continue;
                }
            };

            let command = command.trim();
            match protocol.accept(command) {
                ProtocolAction::HandshakeAccepted(response) => {
                    write_response(&mut stream, Ok(response))?;
                    continue;
                }
                ProtocolAction::HandshakeRejected(error) => {
                    write_response(&mut stream, Err(error))?;
                    break Ok(());
                }
                ProtocolAction::Dispatch => {}
            }
            let command_result = execute_command(&mut hardware, command, &mut fan_session);
            let command_succeeded = command_result.is_ok();
            write_response(&mut stream, command_result)?;
            if command_succeeded {
                fan_session.response_delivered();
            }
        }
    })();

    let cleanup = if fan_session.requires_auto_on_disconnect() {
        match MutationGuard::acquire() {
            Ok(_mutation_guard) => hardware
                .apply_fan_setting(FanSetting::Automatic)
                .map(|_| ())
                .map_err(|error| format!("failsafe Auto failed after disconnect: {error}")),
            Err(error) => Err(format!(
                "failsafe Auto could not acquire mutation lock after disconnect: {error}"
            )),
        }
    } else {
        Ok(())
    };
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
    }
}

/// Fan ownership attached to one GUI control session.
///
/// Manual control and every incomplete or emergency transition are leases:
/// losing the client returns ownership to firmware Auto. An explicitly
/// requested Maximum mode becomes persistent only after the hardware readback
/// succeeded and the matching `OK` response was written to the client socket.
/// This mirrors the appliance behavior users expect without weakening failure
/// cleanup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum FanSessionState {
    #[default]
    Automatic,
    PendingMutation,
    Manual,
    MaximumAwaitingResponse,
    PersistentMaximum,
    EmergencyMaximum,
}

impl FanSessionState {
    fn begin_mutation(&mut self) {
        *self = Self::PendingMutation;
    }

    fn automatic_readback_confirmed(&mut self) {
        *self = Self::Automatic;
    }

    fn manual_readback_confirmed(&mut self) {
        *self = Self::Manual;
    }

    fn maximum_readback_confirmed(&mut self) {
        *self = Self::MaximumAwaitingResponse;
    }

    fn response_delivered(&mut self) {
        if *self == Self::MaximumAwaitingResponse {
            *self = Self::PersistentMaximum;
        }
    }

    fn needs_thermal_watchdog(self) -> bool {
        matches!(self, Self::PendingMutation | Self::Manual)
    }

    fn requires_auto_on_disconnect(self) -> bool {
        matches!(
            self,
            Self::PendingMutation
                | Self::Manual
                | Self::MaximumAwaitingResponse
                | Self::EmergencyMaximum
        )
    }
}

fn authorize_peer(stream: &UnixStream) -> Result<(), String> {
    let socket = std::fs::metadata(CONTROL_SOCKET)
        .map_err(|error| format!("cannot verify control socket owner: {error}"))?;
    let mode = socket.mode() & 0o777;
    if mode != 0o600 || socket.uid() == 0 {
        return Err(format!(
            "unsafe control socket ownership or mode (uid={}, mode={mode:#o})",
            socket.uid()
        ));
    }

    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` points to writable storage of exactly `length`
    // bytes and `stream` remains alive for the complete getsockopt call.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if status != 0 {
        return Err(format!(
            "cannot verify control peer credentials: {}",
            std::io::Error::last_os_error()
        ));
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err("kernel returned malformed control peer credentials".to_string());
    }
    // SAFETY: getsockopt succeeded and returned the complete ucred object.
    let credentials = unsafe { credentials.assume_init() };
    if credentials.uid != socket.uid() {
        return Err(format!(
            "unauthorized control peer uid {} (expected {})",
            credentials.uid,
            socket.uid()
        ));
    }
    Ok(())
}

fn execute_command(
    hardware: &mut AcerHardware,
    command: &str,
    fan_session: &mut FanSessionState,
) -> Result<String, String> {
    let fields: Vec<&str> = command.split_ascii_whitespace().collect();
    if matches!(
        fields.as_slice(),
        ["FAN", "AUTO"] | ["FAN", "MAXIMUM"] | ["FAN", "MANUAL", _, _]
    ) {
        // Establish fail-safe cleanup before lock acquisition or parsing. A
        // rejected, interrupted or partially applied fan mutation must never
        // inherit a previously persistent Maximum session state.
        fan_session.begin_mutation();
    }
    if command_is_mutation(&fields) && !hardware.is_acer() {
        return Err("Acer firmware mutations are unavailable on this system".to_string());
    }
    let _mutation_guard = command_is_mutation(&fields)
        .then(MutationGuard::acquire)
        .transpose()?;
    match fields.as_slice() {
        ["PING"] => Ok("ready".to_string()),
        ["CAPS"] => {
            let refreshed = AcerHardware::discover().map_err(|error| error.to_string())?;
            let capabilities = collect_control_capabilities(&refreshed)?;
            *hardware = refreshed;
            encode_control_capabilities(&capabilities).map_err(String::from)
        }
        ["HARDWARE", "GET"] => Ok(encode_memory_hardware(
            MEMORY_HARDWARE.get_or_init(read_privileged_memory_hardware),
        )),
        ["FAN", "AUTO"] => {
            hardware
                .apply_fan_setting(FanSetting::Automatic)
                .map_err(|error| error.to_string())?;
            fan_session.automatic_readback_confirmed();
            Ok("fan=auto".to_string())
        }
        ["FAN", "MAXIMUM"] => {
            hardware
                .apply_fan_setting(FanSetting::Maximum)
                .map_err(|error| error.to_string())?;
            fan_session.maximum_readback_confirmed();
            Ok("fan=maximum".to_string())
        }
        ["FAN", "MANUAL", cpu, gpu] => {
            let cpu = parse_manual_percent(cpu)?;
            let gpu = parse_manual_percent(gpu)?;
            hardware
                .apply_fan_setting(FanSetting::Manual {
                    cpu_percent: cpu,
                    gpu_percent: gpu,
                })
                .map_err(|error| error.to_string())?;
            fan_session.manual_readback_confirmed();
            Ok(format!("fan=manual cpu={cpu} gpu={gpu}"))
        }
        ["PROFILE", profile] => {
            if hardware.capabilities().fans.backend == Some(FanBackend::AcerGamingWmi) {
                let state = hardware.read_fan_state().map_err(|error| {
                    format!("cannot reconcile Gaming-WMI fan mode before profile write: {error}")
                })?;
                if state.cpu.mode != Some(HardwareFanMode::Automatic)
                    || state.gpu.mode != Some(HardwareFanMode::Automatic)
                {
                    fan_session.begin_mutation();
                    hardware
                        .apply_fan_setting(FanSetting::Automatic)
                        .map_err(|error| error.to_string())?;
                    fan_session.automatic_readback_confirmed();
                }
            }
            let receipt = if hardware.is_reference_model() {
                let profile =
                    PlatformProfile::from_sysfs(profile).map_err(|error| error.to_string())?;
                // Opening NVML is reserved for this explicit profile
                // transaction. Merely connecting the GUI to the daemon must
                // not wake a hybrid dGPU, and the controller is dropped when
                // the command finishes.
                let state = ProfileController::discover()
                    .map_err(|error| error.to_string())?
                    .set_profile(hardware, profile)
                    .map_err(|error| error.to_string())?;
                if state.gpu_capability_error.is_none()
                    && let Err(error) =
                        remove_pending_nvidia_reconciliation(Path::new(NVIDIA_RESUME_PENDING))
                {
                    // The profile and offsets are already verified. A stale
                    // marker cleanup failure must not turn success into ERR.
                    eprintln!("asense NVIDIA reconciliation marker warning: {error}");
                }
                ProfileApplyReceipt {
                    firmware_profile: state.profile.as_sysfs().to_string(),
                    gpu_offsets: state.gpu_offsets,
                    gpu_pstate_count: state.gpu_pstate_count,
                    gpu_capability_available: state.gpu_capability_error.is_none(),
                    power: state.power.map(|power| ProfilePowerReceipt {
                        enforced_limit_mw: power.enforced_limit_mw,
                        maximum_limit_mw: power.maximum_limit_mw,
                        clock_event_reasons: power.clock_event_reasons,
                    }),
                }
            } else {
                hardware
                    .set_profile_raw(profile)
                    .map_err(|error| error.to_string())?;
                ProfileApplyReceipt {
                    firmware_profile: hardware
                        .current_profile_raw()
                        .map_err(|error| error.to_string())?,
                    gpu_offsets: crate::tuning::GpuOffsetState::Unavailable,
                    gpu_pstate_count: 0,
                    gpu_capability_available: false,
                    power: None,
                }
            };
            if hardware.capabilities().fans.backend == Some(FanBackend::AcerGamingWmi) {
                let (receipt, warning) = preserve_verified_profile_after_fan_refresh(
                    receipt,
                    hardware.read_fan_state().map(|_| ()),
                );
                if let Some(warning) = warning {
                    eprintln!("{warning}");
                }
                return Ok(encode_profile_apply_receipt(&receipt));
            }
            Ok(encode_profile_apply_receipt(&receipt))
        }
        ["RGB", "GET"] => {
            let lighting = KeyboardLighting::discover()?;
            lighting.read().map(encode_state)
        }
        [
            "LIGHTING",
            "APPLY",
            device_id,
            mode,
            brightness,
            speed,
            color,
            zones,
        ] => {
            let mode = parse_lighting_mode(mode)?;
            let color = parse_color(color)?;
            let zone_colors = parse_zone_colors(zones)?;
            let brightness = parse_u8(brightness, "lighting brightness")?;
            let speed = parse_u8(speed, "lighting speed")?;
            if brightness > 100 || speed > 9 {
                return Err(
                    "lighting brightness must be 0..=100 and speed must be 0..=9".to_string(),
                );
            }

            for controller in LightingController::discover_all() {
                if let Some(device) = controller
                    .devices()
                    .into_iter()
                    .find(|device| device.id == *device_id)
                {
                    let request = LightingRequest {
                        target: device.target,
                        mode,
                        brightness,
                        speed,
                        color,
                        zone_colors,
                    };
                    let status = controller.apply(&request)?;
                    if controller.backend() == LightingBackend::Enek5130
                        && let Err(error) = save_cached_lighting(&request)
                    {
                        eprintln!("asense ENEK lighting cache warning: {error}");
                    }
                    return Ok(encode_lighting_status(status));
                }
            }
            Err("lighting device is unavailable".to_string())
        }
        ["LIGHTING", "POWER", device_id, value] => {
            let enabled = parse_on_off(value, "lighting power")?;
            for controller in LightingController::discover_all() {
                if let Some(device) = controller
                    .devices()
                    .into_iter()
                    .find(|device| device.id == *device_id)
                {
                    return controller
                        .set_power(device.target, enabled)
                        .map(encode_lighting_status);
                }
            }
            Err("lighting device is unavailable".to_string())
        }
        ["PLATFORM", "GET"] => {
            let controls = PlatformControls::discover()?;
            controls.read().map(encode_platform_state)
        }
        ["PLATFORM", "BATTERY_LIMIT", value] => {
            let controls = PlatformControls::discover()?;
            controls
                .set_battery_limit(parse_on_off(value, "battery limit")?)
                .map(encode_platform_state)
        }
        ["PLATFORM", "BATTERY_CALIBRATION", value] => {
            let enabled = match *value {
                "START" => true,
                "STOP" => false,
                _ => return Err("battery calibration must be START or STOP".to_string()),
            };
            let controls = PlatformControls::discover()?;
            controls
                .set_battery_calibration(enabled)
                .map(encode_platform_state)
        }
        ["PLATFORM", "USB_CHARGING", value] => {
            let threshold = parse_u8(value, "USB charging threshold")?;
            let mode = UsbCharging::from_threshold(threshold)?;
            let controls = PlatformControls::discover()?;
            controls.set_usb_charging(mode).map(encode_platform_state)
        }
        ["PLATFORM", "KEYBOARD_TIMEOUT", value] => {
            let controls = PlatformControls::discover()?;
            controls
                .set_keyboard_timeout(parse_on_off(value, "keyboard timeout")?)
                .map(encode_platform_state)
        }
        ["PLATFORM", "BOOT_SOUND", value] => {
            let controls = PlatformControls::discover()?;
            controls
                .set_boot_sound(parse_on_off(value, "boot sound")?)
                .map(encode_platform_state)
        }
        ["PLATFORM", "LCD_OVERRIDE", value] => {
            let controls = PlatformControls::discover()?;
            controls
                .set_lcd_override(parse_on_off(value, "LCD override")?)
                .map(encode_platform_state)
        }
        ["PLATFORM", "REAR_LOGO", color, brightness, value] => {
            let request = RearLogoState {
                enabled: parse_on_off(value, "rear logo power")?,
                brightness: parse_u8(brightness, "rear logo brightness")?,
                color: parse_color(color)?,
            };
            let controls = PlatformControls::discover()?;
            controls.set_rear_logo(request).map(encode_platform_state)
        }
        _ => Err("unsupported command".to_string()),
    }
}

fn preserve_verified_profile_after_fan_refresh<T, E>(
    receipt: T,
    refresh: Result<(), E>,
) -> (T, Option<String>)
where
    E: std::fmt::Display,
{
    let warning = refresh
        .err()
        .map(|error| format!("asense post-profile Gaming-WMI fan refresh warning: {error}"));
    (receipt, warning)
}

fn collect_control_capabilities(hardware: &AcerHardware) -> Result<ControlCapabilities, String> {
    let discovered = hardware.capabilities();
    let lighting = if hardware.is_acer() {
        LightingController::discover_all()
            .into_iter()
            .flat_map(|controller| controller.devices())
            .collect()
    } else {
        Vec::new()
    };
    let platform = if hardware.is_acer() {
        PlatformControls::discover()
            .map(|controls| controls.capabilities())
            .unwrap_or_default()
    } else {
        Default::default()
    };

    Ok(ControlCapabilities {
        vendor: discovered.vendor,
        product: discovered.product,
        reference_model: discovered.reference_model,
        profiles: discovered.profiles,
        fans: ControlFanCapabilities {
            backend: discovered.fans.backend,
            rpm_channels: discovered
                .fans
                .rpm_channels
                .into_iter()
                .map(|channel| ControlFanRpmChannel {
                    index: channel.index,
                    label: channel.label,
                    rpm: channel.rpm,
                })
                .collect(),
            auto: discovered.fans.auto,
            manual: discovered.fans.manual,
            maximum: discovered.fans.maximum,
        },
        lighting,
        platform,
    })
}

#[derive(Debug, Default)]
struct ProtocolSession {
    negotiated: bool,
}

#[derive(Debug)]
enum ProtocolAction {
    HandshakeAccepted(String),
    HandshakeRejected(String),
    Dispatch,
}

impl ProtocolSession {
    fn new() -> Self {
        Self::default()
    }

    fn accept(&mut self, command: &str) -> ProtocolAction {
        if self.negotiated {
            return ProtocolAction::Dispatch;
        }
        let fields: Vec<&str> = command.split_ascii_whitespace().collect();
        let response = protocol_handshake(&fields).unwrap_or_else(|| {
            Err(format!(
                "protocol negotiation required; expected HELLO {CONTROL_PROTOCOL_VERSION}"
            ))
        });
        match response {
            Ok(response) => {
                self.negotiated = true;
                ProtocolAction::HandshakeAccepted(response)
            }
            Err(error) => ProtocolAction::HandshakeRejected(error),
        }
    }
}

fn protocol_handshake(fields: &[&str]) -> Option<Result<String, String>> {
    let ["HELLO", version] = fields else {
        return None;
    };
    let version = match version.parse::<u16>() {
        Ok(version) => version,
        Err(_) => return Some(Err("protocol version must be an integer".to_string())),
    };
    if version != CONTROL_PROTOCOL_VERSION {
        return Some(Err(format!(
            "unsupported protocol version {version}; expected {CONTROL_PROTOCOL_VERSION}"
        )));
    }
    Some(Ok(format!(
        "protocol={CONTROL_PROTOCOL_VERSION} daemon={}",
        env!("CARGO_PKG_VERSION")
    )))
}

fn command_is_mutation(fields: &[&str]) -> bool {
    match fields {
        ["FAN", ..] | ["PROFILE", ..] => true,
        ["RGB", operation, ..] | ["PLATFORM", operation, ..] => *operation != "GET",
        ["LIGHTING", "APPLY" | "POWER", ..] => true,
        _ => false,
    }
}

fn parse_lighting_mode(value: &str) -> Result<LightingMode, String> {
    match value {
        "OFF" => Ok(LightingMode::Off),
        "STATIC" => Ok(LightingMode::Static),
        "BREATHING" => Ok(LightingMode::Breathing),
        "NEON" => Ok(LightingMode::Neon),
        _ => Err("lighting mode must be OFF, STATIC, BREATHING or NEON".to_string()),
    }
}

fn parse_zone_colors(value: &str) -> Result<Vec<[u8; 3]>, String> {
    if value == "-" {
        return Ok(Vec::new());
    }
    let colors = value.split(',').collect::<Vec<_>>();
    if colors.is_empty() || colors.len() > 16 {
        return Err("lighting request must contain 1..=16 zone colors".to_string());
    }
    colors.into_iter().map(parse_color).collect()
}

fn encode_lighting_status(status: LightingStateStatus) -> String {
    match status {
        LightingStateStatus::Firmware(state) => encode_state(state),
        LightingStateStatus::Unknown => "state=unknown".to_string(),
        LightingStateStatus::LastApplied(request) => format!(
            "state=last-applied mode={} brightness={} speed={} color={:02x}{:02x}{:02x} zones={}",
            match request.mode {
                LightingMode::Off => "off",
                LightingMode::Static => "static",
                LightingMode::Breathing => "breathing",
                LightingMode::Neon => "neon",
            },
            request.brightness,
            request.speed,
            request.color[0],
            request.color[1],
            request.color[2],
            request.zone_colors.len()
        ),
    }
}

fn parse_on_off(value: &str, label: &str) -> Result<bool, String> {
    match value {
        "ON" => Ok(true),
        "OFF" => Ok(false),
        _ => Err(format!("{label} must be ON or OFF")),
    }
}

fn parse_u8(value: &str, label: &str) -> Result<u8, String> {
    value
        .parse::<u8>()
        .map_err(|_| format!("{label} must be an integer within 0..=255"))
}

fn parse_color(value: &str) -> Result<[u8; 3], String> {
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("keyboard color must be exactly RRGGBB".to_string());
    }
    Ok([
        u8::from_str_radix(&value[0..2], 16).map_err(|_| "invalid red channel".to_string())?,
        u8::from_str_radix(&value[2..4], 16).map_err(|_| "invalid green channel".to_string())?,
        u8::from_str_radix(&value[4..6], 16).map_err(|_| "invalid blue channel".to_string())?,
    ])
}

fn parse_manual_percent(value: &str) -> Result<u8, String> {
    let value = value
        .parse::<u8>()
        .map_err(|_| "fan percentage must be an integer".to_string())?;
    if !(20..=100).contains(&value) {
        return Err("manual fan percentage must be within 20..=100".to_string());
    }
    Ok(value)
}

fn enforce_thermal_watchdog(
    hardware: &mut AcerHardware,
    fan_session: &mut FanSessionState,
) -> Result<(), String> {
    if !fan_session.needs_thermal_watchdog() {
        return Ok(());
    }
    let cpu = hardware.read_acer_temp_millidegrees(1);
    let gpu = hardware.read_acer_temp_millidegrees(2);
    let unsafe_or_unavailable = cpu.is_err()
        || gpu.is_err()
        || cpu.is_ok_and(|value| value >= 92_000)
        || gpu.is_ok_and(|value| value >= 84_000);
    if !unsafe_or_unavailable {
        return Ok(());
    }

    // Set the fail-safe state before writing so disconnect cleanup still
    // retries Auto if the emergency Maximum transition itself fails.
    *fan_session = FanSessionState::EmergencyMaximum;
    let _mutation_guard = MutationGuard::acquire()?;
    hardware
        .apply_fan_setting(FanSetting::Maximum)
        .map(|_| ())
        .map_err(|error| format!("thermal failsafe failed: {error}"))
}

fn write_response(stream: &mut UnixStream, result: Result<String, String>) -> Result<(), String> {
    let (prefix, payload) = match result {
        Ok(value) => ("OK", value),
        Err(error) => ("ERR", error),
    };
    let payload = sanitize_response_payload(&payload);
    let response = format!("{prefix} {payload}\n");
    stream
        .write_all(response.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|error| error.to_string())
}

fn sanitize_response_payload(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len().min(MAX_CONTROL_RESPONSE_PAYLOAD_BYTES));
    for character in value.chars() {
        let character = match character {
            '\r' | '\n' => ' ',
            value if value.is_control() => '?',
            value => value,
        };
        if sanitized.len() + character.len_utf8() > MAX_CONTROL_RESPONSE_PAYLOAD_BYTES {
            break;
        }
        sanitized.push(character);
    }
    sanitized
}

fn ensure_root() -> Result<(), String> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|error| format!("cannot read process credentials: {error}"))?;
    let effective_uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .and_then(|value| value.parse::<u32>().ok());
    if effective_uid == Some(0) {
        Ok(())
    } else {
        Err("daemon must run as root".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommandDecoder, FanSessionState, ProtocolAction, ProtocolSession, command_is_mutation,
        finish_pending_nvidia_reconciliation, load_cached_lighting, parse_color,
        parse_lighting_mode, parse_manual_percent, parse_on_off, parse_zone_colors,
        preserve_verified_profile_after_fan_refresh, protocol_handshake,
        remove_pending_nvidia_reconciliation, require_reconciled_nvidia, sanitize_response_payload,
        save_cached_lighting_at, write_pending_nvidia_reconciliation, write_response,
    };
    use crate::control::{
        MAX_CONTROL_COMMAND_BYTES, MAX_CONTROL_RESPONSE_LINE_BYTES,
        MAX_CONTROL_RESPONSE_PAYLOAD_BYTES,
    };
    use crate::hardware::PlatformProfile;
    use crate::lighting::{LightingMode, LightingRequest, LightingTarget};
    use crate::tuning::{GpuOffsetState, TuningState};
    use std::io::{BufRead, BufReader, Cursor};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;

    fn cached_request(target: LightingTarget, color: [u8; 3]) -> LightingRequest {
        LightingRequest {
            target,
            mode: LightingMode::Static,
            brightness: 70,
            speed: 0,
            color,
            zone_colors: Vec::new(),
        }
    }

    #[test]
    fn enek_cache_keeps_one_latest_request_per_target() {
        let path = std::env::temp_dir().join(format!(
            "asense-enek-cache-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_file(&path);
        let keyboard = cached_request(LightingTarget::Keyboard, [1, 2, 3]);
        let logo = cached_request(LightingTarget::CoverLogo, [4, 5, 6]);
        let updated_keyboard = cached_request(LightingTarget::Keyboard, [7, 8, 9]);

        save_cached_lighting_at(&path, &keyboard).unwrap();
        save_cached_lighting_at(&path, &logo).unwrap();
        save_cached_lighting_at(&path, &updated_keyboard).unwrap();

        let cached = load_cached_lighting(&path).unwrap();
        assert_eq!(cached, vec![logo, updated_keyboard]);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn deferred_nvidia_reconciliation_marker_is_private_and_removable() {
        let path = std::env::temp_dir().join(format!(
            "asense-nvidia-reconcile-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_file(&path);
        write_pending_nvidia_reconciliation(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "pending\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        remove_pending_nvidia_reconciliation(&path).unwrap();
        assert!(!path.exists());
        remove_pending_nvidia_reconciliation(&path).unwrap();
    }

    #[test]
    fn deferred_nvidia_reconciliation_requires_gpu_proof_and_keeps_failed_intent() {
        let path = std::env::temp_dir().join(format!(
            "asense-nvidia-proof-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_file(&path);
        write_pending_nvidia_reconciliation(&path).unwrap();

        let mut state = TuningState {
            profile: PlatformProfile::Balanced,
            gpu_offsets: GpuOffsetState::Reset,
            gpu_pstate_count: 4,
            gpu_capability_error: None,
            power: None,
            power_error: None,
        };
        assert!(require_reconciled_nvidia(&state, "test").is_ok());
        state.gpu_capability_error = Some("NVML unavailable".to_string());
        let failed = require_reconciled_nvidia(&state, "test");
        assert!(
            failed
                .as_ref()
                .is_err_and(|error| error.contains("remains pending"))
        );
        assert!(finish_pending_nvidia_reconciliation(&path, failed).is_err());
        assert!(path.exists());

        finish_pending_nvidia_reconciliation(&path, Ok(())).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn percent_is_fail_closed() {
        assert_eq!(parse_manual_percent("20").unwrap(), 20);
        assert_eq!(parse_manual_percent("100").unwrap(), 100);
        assert!(parse_manual_percent("0").is_err());
        assert!(parse_manual_percent("19").is_err());
        assert!(parse_manual_percent("101").is_err());
        assert!(parse_manual_percent("-1").is_err());
        assert!(parse_manual_percent("NaN").is_err());
    }

    #[test]
    fn rgb_color_is_exact_and_fail_closed() {
        assert_eq!(parse_color("12aBef").unwrap(), [0x12, 0xab, 0xef]);
        assert!(parse_color("fff").is_err());
        assert!(parse_color("000000junk").is_err());
        assert!(parse_color("zzzzzz").is_err());
    }

    #[test]
    fn platform_boolean_commands_are_exact() {
        assert!(parse_on_off("ON", "test").unwrap());
        assert!(!parse_on_off("OFF", "test").unwrap());
        assert!(parse_on_off("on", "test").is_err());
        assert!(parse_on_off("1", "test").is_err());
    }

    #[test]
    fn post_profile_fan_refresh_failure_preserves_verified_success() {
        let (receipt, warning) = preserve_verified_profile_after_fan_refresh(
            "verified-profile-receipt",
            Err("fan refresh failed"),
        );
        assert_eq!(receipt, "verified-profile-receipt");
        assert_eq!(
            warning.as_deref(),
            Some("asense post-profile Gaming-WMI fan refresh warning: fan refresh failed")
        );
    }

    #[test]
    fn acknowledged_explicit_maximum_survives_client_disconnect() {
        let mut session = FanSessionState::Automatic;
        session.begin_mutation();
        session.maximum_readback_confirmed();

        assert!(session.requires_auto_on_disconnect());
        session.response_delivered();
        assert_eq!(session, FanSessionState::PersistentMaximum);
        assert!(!session.requires_auto_on_disconnect());
        assert!(!session.needs_thermal_watchdog());
    }

    #[test]
    fn manual_and_emergency_maximum_remain_disconnect_leases() {
        let mut session = FanSessionState::Automatic;
        session.begin_mutation();
        session.manual_readback_confirmed();
        session.response_delivered();

        assert_eq!(session, FanSessionState::Manual);
        assert!(session.needs_thermal_watchdog());
        assert!(session.requires_auto_on_disconnect());

        session = FanSessionState::EmergencyMaximum;
        assert!(session.requires_auto_on_disconnect());
        assert!(!session.needs_thermal_watchdog());
    }

    #[test]
    fn pending_or_unacknowledged_maximum_fails_closed_to_auto() {
        let mut session = FanSessionState::PersistentMaximum;
        session.begin_mutation();
        assert_eq!(session, FanSessionState::PendingMutation);
        assert!(session.needs_thermal_watchdog());
        assert!(session.requires_auto_on_disconnect());

        session.maximum_readback_confirmed();
        assert_eq!(session, FanSessionState::MaximumAwaitingResponse);
        assert!(session.requires_auto_on_disconnect());

        session.automatic_readback_confirmed();
        assert_eq!(session, FanSessionState::Automatic);
        assert!(!session.requires_auto_on_disconnect());
    }

    #[test]
    fn response_payload_is_single_line_and_bounded() {
        let payload = format!(
            "first\nsecond\r{}",
            "x".repeat(MAX_CONTROL_RESPONSE_PAYLOAD_BYTES * 2)
        );
        let sanitized = sanitize_response_payload(&payload);
        assert!(!sanitized.contains(['\n', '\r']));
        assert!(sanitized.len() <= MAX_CONTROL_RESPONSE_PAYLOAD_BYTES);
        assert!(sanitized.starts_with("first second "));
    }

    #[test]
    fn complete_response_fits_the_shared_client_line_budget() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        write_response(
            &mut writer,
            Err("x".repeat(MAX_CONTROL_RESPONSE_LINE_BYTES * 2)),
        )
        .unwrap();
        let mut line = Vec::new();
        BufReader::new(reader).read_until(b'\n', &mut line).unwrap();
        assert_eq!(line.last(), Some(&b'\n'));
        assert!(line.len() - 1 <= MAX_CONTROL_RESPONSE_LINE_BYTES);
        assert!(line.starts_with(b"ERR "));
    }

    #[test]
    fn protocol_handshake_is_versioned_and_exact() {
        let response = protocol_handshake(&["HELLO", "2"])
            .expect("HELLO is a protocol command")
            .unwrap();
        assert!(response.starts_with("protocol=2 daemon="));
        assert!(
            protocol_handshake(&["HELLO", "1"])
                .unwrap()
                .unwrap_err()
                .contains("unsupported protocol version")
        );
        assert!(protocol_handshake(&["HELLO", "one"]).unwrap().is_err());
        assert!(protocol_handshake(&["HELLO"]).is_none());
        assert!(protocol_handshake(&["PING"]).is_none());
    }

    #[test]
    fn privileged_session_requires_a_successful_hello_before_commands() {
        assert!(matches!(
            ProtocolSession::new().accept("PING"),
            ProtocolAction::HandshakeRejected(error)
                if error.contains("expected HELLO 2")
        ));
        assert!(matches!(
            ProtocolSession::new().accept("HELLO 1"),
            ProtocolAction::HandshakeRejected(error)
                if error.contains("unsupported protocol version")
        ));

        let mut session = ProtocolSession::new();
        assert!(matches!(
            session.accept("HELLO 2"),
            ProtocolAction::HandshakeAccepted(receipt)
                if receipt.starts_with("protocol=2 daemon=")
        ));
        assert!(matches!(
            session.accept("PROFILE performance"),
            ProtocolAction::Dispatch
        ));
    }

    #[test]
    fn privileged_protocol_closes_immediately_at_the_size_limit() {
        let oversized = "x".repeat(1024 * 1024);
        let input = format!("{oversized}\nPING\n");
        let mut reader = BufReader::with_capacity(17, Cursor::new(input.into_bytes()));
        let mut decoder = CommandDecoder::new();

        let error = decoder.read(&mut reader).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(decoder.retained_bytes() <= MAX_CONTROL_COMMAND_BYTES);
    }

    #[test]
    fn privileged_protocol_accepts_exact_limit_and_rejects_invalid_utf8() {
        let exact = "a".repeat(MAX_CONTROL_COMMAND_BYTES);
        let mut bytes = exact.as_bytes().to_vec();
        bytes.extend_from_slice(b"\n\xff\n");
        let mut reader = BufReader::with_capacity(11, Cursor::new(bytes));
        let mut decoder = CommandDecoder::new();

        assert_eq!(decoder.read(&mut reader).unwrap().unwrap().unwrap(), exact);
        assert_eq!(
            decoder.read(&mut reader).unwrap().unwrap(),
            Err("command must be valid UTF-8".to_string())
        );
    }

    #[test]
    fn privileged_protocol_rejects_eof_before_newline() {
        let mut reader = BufReader::with_capacity(5, Cursor::new(b"PROFILE performance"));
        let mut decoder = CommandDecoder::new();

        let error = decoder.read(&mut reader).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "command ended before newline");
        assert_eq!(decoder.retained_bytes(), 0);
    }

    #[test]
    fn privileged_protocol_accepts_fragmented_newline_terminated_frames() {
        let mut reader = BufReader::with_capacity(3, Cursor::new(b"HELLO 2\nPING\n"));
        let mut decoder = CommandDecoder::new();

        assert_eq!(
            decoder.read(&mut reader).unwrap().unwrap().unwrap(),
            "HELLO 2"
        );
        assert_eq!(decoder.read(&mut reader).unwrap().unwrap().unwrap(), "PING");
        assert!(decoder.read(&mut reader).unwrap().is_none());
    }

    #[test]
    fn mutation_lock_classification_is_fail_closed_for_control_writes() {
        assert!(!command_is_mutation(&["PING"]));
        assert!(!command_is_mutation(&["RGB", "GET"]));
        assert!(!command_is_mutation(&["PLATFORM", "GET"]));
        assert!(command_is_mutation(&["FAN", "AUTO"]));
        assert!(command_is_mutation(&["PROFILE", "performance"]));
        assert!(command_is_mutation(&["RGB", "POWER", "ON"]));
        assert!(command_is_mutation(&["LIGHTING", "APPLY", "enek-keyboard"]));
        assert!(command_is_mutation(&[
            "LIGHTING",
            "POWER",
            "zoned-wmi-keyboard",
            "OFF"
        ]));
        assert!(command_is_mutation(&["PLATFORM", "BATTERY_LIMIT", "ON"]));
    }

    #[test]
    fn typed_lighting_values_are_bounded() {
        assert!(parse_lighting_mode("STATIC").is_ok());
        assert!(parse_lighting_mode("WAVE").is_err());
        assert!(parse_zone_colors("-").unwrap().is_empty());
        assert_eq!(parse_zone_colors("001122,aabbcc").unwrap().len(), 2);
        assert!(parse_zone_colors("001122,invalid").is_err());
        assert!(parse_zone_colors(&vec!["000000"; 17].join(",")).is_err());
    }
}
