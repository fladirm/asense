use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::OnceLock;
use std::time::Duration;

use crate::control::{
    CONTROL_PROTOCOL_VERSION, CONTROL_SOCKET, MAX_CONTROL_RESPONSE_LINE_BYTES, ProfileApplyReceipt,
    ProfilePowerReceipt, encode_profile_apply_receipt,
};
use crate::hardware::{AcerHardware, FanSetting, PlatformProfile};
use crate::lighting::{EffectRequest, KeyboardLighting, ZonesRequest, encode_state};
use crate::mutation_lock::MutationGuard;
use crate::platform::{
    PlatformControls, RearLogoState, UsbCharging, encode_state as encode_platform_state,
};
use crate::telemetry::{
    MemoryHardwareInfo, encode_memory_hardware, read_privileged_memory_hardware,
};
use crate::tuning::ProfileController;

static MEMORY_HARDWARE: OnceLock<MemoryHardwareInfo> = OnceLock::new();
const MAX_COMMAND_BYTES: usize = 192;
const MAX_RESPONSE_PAYLOAD_BYTES: usize = MAX_CONTROL_RESPONSE_LINE_BYTES - "ERR ".len();

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
            pending: Vec::with_capacity(MAX_COMMAND_BYTES),
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
        let remaining = MAX_COMMAND_BYTES.saturating_sub(self.pending.len());
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
        self.pending = Vec::with_capacity(MAX_COMMAND_BYTES);
        String::from_utf8(bytes).map_err(|_| "command must be valid UTF-8".to_string())
    }

    #[cfg(test)]
    fn retained_bytes(&self) -> usize {
        self.pending.len()
    }
}

pub fn run() -> Result<(), String> {
    ensure_root()?;
    let listener = activated_listener()?;
    loop {
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
    hardware
        .apply_fan_setting(FanSetting::Automatic)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Privileged installation/runtime probe. Optional NVIDIA tuning is reported
/// by the normal capability path and never hides the verified Acer controls.
pub fn probe() -> Result<(), String> {
    ensure_root()?;
    let hardware = AcerHardware::discover().map_err(|error| error.to_string())?;
    hardware
        .read_fan_state()
        .map_err(|error| error.to_string())?;
    hardware
        .current_profile()
        .map_err(|error| error.to_string())?;
    KeyboardLighting::discover()?.read()?;
    PlatformControls::discover()?.read()?;
    let profiles = ProfileController::discover().map_err(|error| error.to_string())?;
    match profiles.state(&hardware) {
        Ok(state) => println!(
            "platform=verified profile={} gpu={:?} gpu_capability={}",
            state.profile.as_sysfs(),
            state.gpu_offsets,
            if state.gpu_capability_error.is_some() {
                "unavailable"
            } else {
                "available"
            }
        ),
        Err(error) => println!(
            "platform=verified profile={} gpu=Unavailable gpu_capability=unavailable diagnostic={error:?}",
            hardware
                .current_profile()
                .map_err(|error| error.to_string())?
                .as_sysfs()
        ),
    }
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
    hardware
        .apply_fan_setting(FanSetting::Automatic)
        .map_err(|error| format!("post-resume fan safety: {error}"))?;
    let profile = hardware
        .current_profile()
        .map_err(|error| format!("post-resume profile readback: {error}"))?;
    ProfileController::discover()
        .map_err(|error| error.to_string())?
        .set_profile(&hardware, profile)
        .map_err(|error| format!("post-resume profile reconciliation: {error}"))?;
    Ok(())
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
    let hardware = AcerHardware::discover().map_err(|error| error.to_string())?;
    let profiles = ProfileController::discover().map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|error| format!("set watchdog timeout: {error}"))?;
    let read_stream = stream.try_clone().map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(read_stream);
    let mut decoder = CommandDecoder::new();
    let mut protocol = ProtocolSession::new();
    let mut fan_session = FanSessionState::Automatic;

    let result = (|| -> Result<(), String> {
        loop {
            let command = match decoder.read(&mut reader) {
                Ok(Some(command)) => command,
                Ok(None) => break Ok(()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if fan_session.needs_thermal_watchdog() && thermal_limit_reached(&hardware) {
                        // Keep the session fail-safe before attempting the
                        // emergency transition so every exit path retries Auto.
                        fan_session = FanSessionState::EmergencyMaximum;
                        let _mutation_guard = MutationGuard::acquire()?;
                        hardware
                            .apply_fan_setting(FanSetting::Maximum)
                            .map_err(|error| format!("thermal failsafe failed: {error}"))?;
                    }
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
            let command_result = execute_command(&hardware, &profiles, command, &mut fan_session);
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
    hardware: &AcerHardware,
    profiles: &ProfileController,
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
    let _mutation_guard = command_is_mutation(&fields)
        .then(MutationGuard::acquire)
        .transpose()?;
    match fields.as_slice() {
        ["PING"] => Ok("ready".to_string()),
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
            let profile =
                PlatformProfile::from_sysfs(profile).map_err(|error| error.to_string())?;
            let state = profiles
                .set_profile(hardware, profile)
                .map_err(|error| error.to_string())?;
            let receipt = ProfileApplyReceipt {
                firmware_profile: state.profile,
                gpu_offsets: state.gpu_offsets,
                gpu_pstate_count: state.gpu_pstate_count,
                gpu_capability_available: state.gpu_capability_error.is_none(),
                power: state.power.map(|power| ProfilePowerReceipt {
                    enforced_limit_mw: power.enforced_limit_mw,
                    maximum_limit_mw: power.maximum_limit_mw,
                    clock_event_reasons: power.clock_event_reasons,
                }),
            };
            Ok(encode_profile_apply_receipt(&receipt))
        }
        ["RGB", "GET"] => {
            let lighting = KeyboardLighting::discover()?;
            lighting.read().map(encode_state)
        }
        ["RGB", "POWER", value] => {
            let enabled = match *value {
                "ON" => true,
                "OFF" => false,
                _ => return Err("keyboard power must be ON or OFF".to_string()),
            };
            let lighting = KeyboardLighting::discover()?;
            lighting.set_power(enabled).map(encode_state)
        }
        [
            "RGB",
            "EFFECT",
            mode,
            speed,
            brightness,
            direction,
            red,
            green,
            blue,
        ] => {
            let lighting = KeyboardLighting::discover()?;
            let request = EffectRequest {
                mode: parse_u8(mode, "effect mode")?,
                speed: parse_u8(speed, "effect speed")?,
                brightness: parse_u8(brightness, "brightness")?,
                direction: parse_u8(direction, "effect direction")?,
                color: [
                    parse_u8(red, "red")?,
                    parse_u8(green, "green")?,
                    parse_u8(blue, "blue")?,
                ],
            };
            lighting.set_effect(request).map(encode_state)
        }
        ["RGB", "ZONES", zone1, zone2, zone3, zone4, brightness] => {
            let lighting = KeyboardLighting::discover()?;
            let request = ZonesRequest {
                zones: [
                    parse_color(zone1)?,
                    parse_color(zone2)?,
                    parse_color(zone3)?,
                    parse_color(zone4)?,
                ],
                brightness: parse_u8(brightness, "brightness")?,
            };
            lighting.set_zones(request).map(encode_state)
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
        _ => false,
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

fn thermal_limit_reached(hardware: &AcerHardware) -> bool {
    let cpu = hardware
        .read_acer_temp_millidegrees(1)
        .ok()
        .is_some_and(|value| value >= 92_000);
    let gpu = hardware
        .read_acer_temp_millidegrees(2)
        .ok()
        .is_some_and(|value| value >= 84_000);
    cpu || gpu
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
    let mut sanitized = String::with_capacity(value.len().min(MAX_RESPONSE_PAYLOAD_BYTES));
    for character in value.chars() {
        let character = match character {
            '\r' | '\n' => ' ',
            value if value.is_control() => '?',
            value => value,
        };
        if sanitized.len() + character.len_utf8() > MAX_RESPONSE_PAYLOAD_BYTES {
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
        CommandDecoder, FanSessionState, MAX_COMMAND_BYTES, MAX_RESPONSE_PAYLOAD_BYTES,
        ProtocolAction, ProtocolSession, command_is_mutation, parse_color, parse_manual_percent,
        parse_on_off, protocol_handshake, sanitize_response_payload, write_response,
    };
    use crate::control::MAX_CONTROL_RESPONSE_LINE_BYTES;
    use std::io::{BufRead, BufReader, Cursor};
    use std::os::unix::net::UnixStream;

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
            "x".repeat(MAX_RESPONSE_PAYLOAD_BYTES * 2)
        );
        let sanitized = sanitize_response_payload(&payload);
        assert!(!sanitized.contains(['\n', '\r']));
        assert!(sanitized.len() <= MAX_RESPONSE_PAYLOAD_BYTES);
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
        let response = protocol_handshake(&["HELLO", "1"])
            .expect("HELLO is a protocol command")
            .unwrap();
        assert!(response.starts_with("protocol=1 daemon="));
        assert!(
            protocol_handshake(&["HELLO", "2"])
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
                if error.contains("expected HELLO 1")
        ));
        assert!(matches!(
            ProtocolSession::new().accept("HELLO 2"),
            ProtocolAction::HandshakeRejected(error)
                if error.contains("unsupported protocol version")
        ));

        let mut session = ProtocolSession::new();
        assert!(matches!(
            session.accept("HELLO 1"),
            ProtocolAction::HandshakeAccepted(receipt)
                if receipt.starts_with("protocol=1 daemon=")
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
        assert!(decoder.retained_bytes() <= MAX_COMMAND_BYTES);
    }

    #[test]
    fn privileged_protocol_accepts_exact_limit_and_rejects_invalid_utf8() {
        let exact = "a".repeat(MAX_COMMAND_BYTES);
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
        let mut reader = BufReader::with_capacity(3, Cursor::new(b"HELLO 1\nPING\n"));
        let mut decoder = CommandDecoder::new();

        assert_eq!(
            decoder.read(&mut reader).unwrap().unwrap().unwrap(),
            "HELLO 1"
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
        assert!(command_is_mutation(&["PLATFORM", "BATTERY_LIMIT", "ON"]));
    }
}
