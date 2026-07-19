use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};
use std::{error::Error, fmt};

use crate::nvidia::ClockEventReasons;
use crate::platform::{PlatformState, RearLogoState, UsbCharging, parse_state};
use crate::telemetry::{MemoryHardwareInfo, parse_memory_hardware};
use crate::tuning::GpuOffsetState;
pub use crate::{
    hardware::{FanBackend as CapabilityFanBackend, ProfileBackend as CapabilityProfileBackend},
    lighting::{
        LightingBackend as CapabilityLightingBackend, LightingDevice as ControlLightingDevice,
        LightingMode as ControlLightingMode, LightingModes as ControlLightingModes,
        LightingTarget as CapabilityLightingTarget,
    },
    platform::PlatformCapabilities as ControlPlatformCapabilities,
};

pub const CONTROL_SOCKET: &str = "/run/asense-control.sock";
pub const CONTROL_PROTOCOL_VERSION: u16 = 2;
pub(crate) const MAX_CONTROL_COMMAND_BYTES: usize = 192;
pub(crate) const MAX_CONTROL_RESPONSE_LINE_BYTES: usize = 4096;
pub(crate) const MAX_CONTROL_RESPONSE_PAYLOAD_BYTES: usize =
    MAX_CONTROL_RESPONSE_LINE_BYTES - "ERR ".len();
const CAPABILITIES_SCHEMA_VERSION: u8 = 1;
const MAX_CAPABILITY_PROFILES: usize = 8;
const MAX_CAPABILITY_FANS: usize = 8;
const MAX_CAPABILITY_LIGHTING_DEVICES: usize = 8;
const MAX_CAPABILITY_TOKEN_BYTES: usize = 48;
const MAX_CAPABILITY_LABEL_BYTES: usize = 64;
const MAX_CAPABILITY_IDENTITY_BYTES: usize = 96;
const CONTROL_READ_SLICE: Duration = Duration::from_millis(250);
const CONTROL_RESPONSE_DEADLINE: Duration = Duration::from_secs(5);

pub type ControlResult<T> = Result<T, ControlError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlError {
    /// The daemon parsed the request and deliberately refused that command.
    /// The session remains healthy and may be reused.
    CommandRejected(String),
    /// Local request validation failed before anything was sent.
    InvalidRequest(String),
    /// The Unix stream was closed or an I/O operation failed.
    Transport(String),
    /// No complete response arrived within the absolute command deadline.
    Timeout,
    /// The peer response was malformed or incompatible with this client.
    Protocol(String),
}

impl ControlError {
    pub fn invalidates_session(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Timeout | Self::Protocol(_))
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandRejected(error) | Self::InvalidRequest(error) => {
                formatter.write_str(error)
            }
            Self::Transport(error) | Self::Protocol(error) => formatter.write_str(error),
            Self::Timeout => formatter.write_str("control response timed out after 5 seconds"),
        }
    }
}

impl Error for ControlError {}

impl From<ControlError> for String {
    fn from(error: ControlError) -> Self {
        error.to_string()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfilePowerReceipt {
    pub enforced_limit_mw: u32,
    pub maximum_limit_mw: u32,
    pub clock_event_reasons: ClockEventReasons,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileApplyReceipt {
    pub firmware_profile: String,
    pub gpu_offsets: GpuOffsetState,
    pub gpu_pstate_count: usize,
    pub gpu_capability_available: bool,
    pub power: Option<ProfilePowerReceipt>,
}

pub use crate::hardware::{
    ProfileCapabilities as ControlProfileCapabilities, ProfileChoice as ControlProfileChoice,
};

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlFanRpmChannel {
    pub index: u8,
    pub label: String,
    pub rpm: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlFanCapabilities {
    pub backend: Option<CapabilityFanBackend>,
    pub rpm_channels: Vec<ControlFanRpmChannel>,
    pub auto: bool,
    pub manual: bool,
    pub maximum: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlCapabilities {
    pub vendor: String,
    pub product: String,
    pub reference_model: bool,
    pub profiles: ControlProfileCapabilities,
    pub fans: ControlFanCapabilities,
    pub lighting: Vec<ControlLightingDevice>,
    pub platform: ControlPlatformCapabilities,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HandshakeReceipt {
    protocol: u16,
    daemon_version: String,
}

pub struct ControlClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl ControlClient {
    pub fn connect() -> ControlResult<Self> {
        let stream = UnixStream::connect(CONTROL_SOCKET).map_err(|error| {
            ControlError::Transport(format!("control service unavailable: {error}"))
        })?;
        Self::from_stream(stream)
    }

    fn from_stream(stream: UnixStream) -> ControlResult<Self> {
        stream
            .set_read_timeout(Some(CONTROL_READ_SLICE))
            .map_err(|error| ControlError::Transport(error.to_string()))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .map_err(|error| ControlError::Transport(error.to_string()))?;
        let reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|error| ControlError::Transport(error.to_string()))?,
        );
        let mut client = Self { stream, reader };
        let handshake = client
            .request(&format!("HELLO {CONTROL_PROTOCOL_VERSION}"))
            .map_err(|error| match error {
                ControlError::CommandRejected(error) => ControlError::Protocol(format!(
                    "control protocol negotiation was rejected: {error}"
                )),
                error => error,
            })?;
        let handshake = parse_handshake(&handshake)?;
        if handshake.protocol != CONTROL_PROTOCOL_VERSION {
            return Err(ControlError::Protocol(format!(
                "unsupported control protocol {} from daemon {} (expected {})",
                handshake.protocol, handshake.daemon_version, CONTROL_PROTOCOL_VERSION
            )));
        }
        Ok(client)
    }

    pub fn fan_auto(&mut self) -> ControlResult<()> {
        self.request("FAN AUTO").map(|_| ())
    }

    pub fn ping(&mut self) -> ControlResult<()> {
        self.request("PING").map(|_| ())
    }

    /// Returns the daemon's typed, read-only capability snapshot.
    ///
    /// This is also the intentionally small external API surface for clients
    /// that connect to [`CONTROL_SOCKET`] directly: negotiate `HELLO 2`, then
    /// issue `CAPS`. Hardware paths and transport-specific command IDs never
    /// cross the socket boundary.
    pub fn capabilities(&mut self) -> ControlResult<ControlCapabilities> {
        let response = self.request("CAPS")?;
        parse_control_capabilities(&response)
    }

    pub fn fan_maximum(&mut self) -> ControlResult<()> {
        self.request("FAN MAXIMUM").map(|_| ())
    }

    pub fn fan_manual(&mut self, cpu_percent: u8, gpu_percent: u8) -> ControlResult<()> {
        if !(20..=100).contains(&cpu_percent) || !(20..=100).contains(&gpu_percent) {
            return Err(ControlError::InvalidRequest(
                "manual fan percentage must be within 20..=100".to_string(),
            ));
        }
        self.request(&format!("FAN MANUAL {cpu_percent} {gpu_percent}"))
            .map(|_| ())
    }

    pub fn set_profile(&mut self, profile: &str) -> ControlResult<ProfileApplyReceipt> {
        validate_profile_token(profile, false)?;
        let response = self.request(&format!("PROFILE {profile}"))?;
        parse_profile_apply_receipt(&response)
    }

    pub fn keyboard_state(&mut self) -> ControlResult<String> {
        self.request("RGB GET")
    }

    pub fn memory_hardware_info(&mut self) -> ControlResult<MemoryHardwareInfo> {
        let response = self.request("HARDWARE GET")?;
        parse_memory_hardware(&response)
            .map_err(|error| ControlError::Protocol(format!("invalid hardware response: {error}")))
    }

    pub fn lighting_apply(
        &mut self,
        device_id: &str,
        mode: ControlLightingMode,
        brightness: u8,
        speed: u8,
        color: [u8; 3],
        zone_colors: &[[u8; 3]],
    ) -> ControlResult<String> {
        if !valid_lighting_device_id(device_id)
            || brightness > 100
            || speed > 9
            || zone_colors.len() > 16
        {
            return Err(ControlError::InvalidRequest(
                "invalid typed lighting request".to_string(),
            ));
        }
        let mode = match mode {
            ControlLightingMode::Off => "OFF",
            ControlLightingMode::Static => "STATIC",
            ControlLightingMode::Breathing => "BREATHING",
            ControlLightingMode::Neon => "NEON",
        };
        let zones = if zone_colors.is_empty() {
            "-".to_string()
        } else {
            zone_colors
                .iter()
                .map(|color| format!("{:02x}{:02x}{:02x}", color[0], color[1], color[2]))
                .collect::<Vec<_>>()
                .join(",")
        };
        let command = format!(
            "LIGHTING APPLY {device_id} {mode} {brightness} {speed} {:02x}{:02x}{:02x} {zones}",
            color[0], color[1], color[2]
        );
        if command.len() > MAX_CONTROL_COMMAND_BYTES {
            return Err(ControlError::InvalidRequest(
                "typed lighting request exceeds the control line limit".to_string(),
            ));
        }
        self.request(&command)
    }

    pub fn lighting_power(&mut self, device_id: &str, enabled: bool) -> ControlResult<String> {
        if !valid_lighting_device_id(device_id) {
            return Err(ControlError::InvalidRequest(
                "invalid typed lighting power request".to_string(),
            ));
        }
        self.request(&format!(
            "LIGHTING POWER {device_id} {}",
            if enabled { "ON" } else { "OFF" }
        ))
    }

    pub fn platform_state(&mut self) -> ControlResult<PlatformState> {
        self.platform_request("PLATFORM GET")
    }

    pub fn set_battery_limit(&mut self, enabled: bool) -> ControlResult<PlatformState> {
        self.set_platform_toggle("BATTERY_LIMIT", enabled)
    }

    pub fn set_battery_calibration(&mut self, enabled: bool) -> ControlResult<PlatformState> {
        self.platform_request(if enabled {
            "PLATFORM BATTERY_CALIBRATION START"
        } else {
            "PLATFORM BATTERY_CALIBRATION STOP"
        })
    }

    pub fn set_usb_charging(&mut self, mode: UsbCharging) -> ControlResult<PlatformState> {
        self.platform_request(&format!("PLATFORM USB_CHARGING {}", mode.threshold()))
    }

    pub fn set_keyboard_timeout(&mut self, enabled: bool) -> ControlResult<PlatformState> {
        self.set_platform_toggle("KEYBOARD_TIMEOUT", enabled)
    }

    pub fn set_boot_sound(&mut self, enabled: bool) -> ControlResult<PlatformState> {
        self.set_platform_toggle("BOOT_SOUND", enabled)
    }

    pub fn set_lcd_override(&mut self, enabled: bool) -> ControlResult<PlatformState> {
        self.set_platform_toggle("LCD_OVERRIDE", enabled)
    }

    pub fn set_rear_logo(&mut self, state: RearLogoState) -> ControlResult<PlatformState> {
        if state.brightness > 100 {
            return Err(ControlError::InvalidRequest(
                "rear logo brightness must be within 0..=100".to_string(),
            ));
        }
        self.platform_request(&format!(
            "PLATFORM REAR_LOGO {:02x}{:02x}{:02x} {} {}",
            state.color[0],
            state.color[1],
            state.color[2],
            state.brightness,
            if state.enabled { "ON" } else { "OFF" },
        ))
    }

    fn platform_request(&mut self, command: &str) -> ControlResult<PlatformState> {
        let response = self.request(command)?;
        parse_state(&response)
            .map_err(|error| ControlError::Protocol(format!("invalid platform response: {error}")))
    }

    fn set_platform_toggle(&mut self, name: &str, enabled: bool) -> ControlResult<PlatformState> {
        self.platform_request(&format!(
            "PLATFORM {name} {}",
            if enabled { "ON" } else { "OFF" }
        ))
    }

    fn request(&mut self, command: &str) -> ControlResult<String> {
        self.stream
            .write_all(command.as_bytes())
            .and_then(|_| self.stream.write_all(b"\n"))
            .and_then(|_| self.stream.flush())
            .map_err(|error| ControlError::Transport(format!("control write failed: {error}")))?;

        let response =
            read_response_line(&mut self.reader, Instant::now() + CONTROL_RESPONSE_DEADLINE)?;
        parse_response(&response)
    }
}

fn read_response_line(reader: &mut impl BufRead, deadline: Instant) -> ControlResult<String> {
    let mut response = Vec::with_capacity(256);
    let mut oversized = false;
    loop {
        match reader.fill_buf() {
            Ok([]) if response.is_empty() && !oversized => {
                return Err(ControlError::Transport(
                    "control service closed the connection".to_string(),
                ));
            }
            Ok([]) => {
                return if oversized {
                    Err(ControlError::Protocol(
                        "control response exceeded 4096 bytes".to_string(),
                    ))
                } else {
                    Err(ControlError::Transport(
                        "control service closed before completing its response".to_string(),
                    ))
                };
            }
            Ok(available) => {
                let newline = available.iter().position(|byte| *byte == b'\n');
                let consumed = newline.map_or(available.len(), |position| position + 1);
                let payload_end = newline.unwrap_or(available.len());
                if !oversized {
                    let remaining = MAX_CONTROL_RESPONSE_LINE_BYTES.saturating_sub(response.len());
                    if payload_end > remaining {
                        response.clear();
                        oversized = true;
                    } else {
                        response.extend_from_slice(&available[..payload_end]);
                    }
                }
                reader.consume(consumed);
                if newline.is_some() {
                    return if oversized {
                        Err(ControlError::Protocol(
                            "control response exceeded 4096 bytes".to_string(),
                        ))
                    } else {
                        String::from_utf8(response).map_err(|_| {
                            ControlError::Protocol(
                                "control response was not valid UTF-8".to_string(),
                            )
                        })
                    };
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && Instant::now() < deadline =>
            {
                // SO_RCVTIMEO is surfaced by Linux as EAGAIN/WouldBlock.
                // Firmware-backed commands can legitimately span more than
                // one short receive slice; retry only this transient class
                // and keep a single absolute deadline for the request.
                continue;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(ControlError::Timeout);
            }
            Err(error) => {
                return Err(ControlError::Transport(format!(
                    "control read failed: {error}"
                )));
            }
        }
    }
}

fn parse_response(response: &str) -> ControlResult<String> {
    let response = response.trim();
    if response == "OK" {
        Ok(String::new())
    } else if let Some(value) = response.strip_prefix("OK ") {
        Ok(value.trim().to_string())
    } else if response == "ERR" {
        Err(ControlError::CommandRejected(String::new()))
    } else if let Some(error) = response.strip_prefix("ERR ") {
        Err(ControlError::CommandRejected(error.trim().to_string()))
    } else {
        Err(ControlError::Protocol(
            "invalid response from control service".to_string(),
        ))
    }
}

fn parse_handshake(value: &str) -> ControlResult<HandshakeReceipt> {
    let fields = parse_exact_fields(value, &["protocol", "daemon"])?;
    let protocol = fields[0].parse::<u16>().map_err(|_| {
        ControlError::Protocol("invalid protocol version from control service".to_string())
    })?;
    let daemon_version = fields[1].to_string();
    if daemon_version.is_empty() {
        return Err(ControlError::Protocol(
            "control service returned an empty daemon version".to_string(),
        ));
    }
    Ok(HandshakeReceipt {
        protocol,
        daemon_version,
    })
}

pub(crate) fn encode_control_capabilities(
    capabilities: &ControlCapabilities,
) -> ControlResult<String> {
    let mut capabilities = capabilities.clone();
    capabilities
        .fans
        .rpm_channels
        .sort_by_key(|channel| channel.index);
    capabilities
        .lighting
        .sort_by(|left, right| left.id.cmp(&right.id));
    validate_control_capabilities(&capabilities, false)?;

    let json = serde_json::to_string(&capabilities).map_err(|error| {
        ControlError::InvalidRequest(format!("cannot encode capabilities: {error}"))
    })?;
    let encoded = format!("caps={CAPABILITIES_SCHEMA_VERSION} {json}");
    if encoded.len() > MAX_CONTROL_RESPONSE_PAYLOAD_BYTES {
        return Err(ControlError::InvalidRequest(
            "capability response exceeds the control line limit".to_string(),
        ));
    }
    Ok(encoded)
}

fn parse_control_capabilities(value: &str) -> ControlResult<ControlCapabilities> {
    if value.len() > MAX_CONTROL_RESPONSE_PAYLOAD_BYTES {
        return Err(ControlError::Protocol(
            "capability response exceeds the control line limit".to_string(),
        ));
    }
    let (schema, json) = value
        .split_once(' ')
        .ok_or_else(|| ControlError::Protocol("invalid capability response framing".to_string()))?;
    if schema != format!("caps={CAPABILITIES_SCHEMA_VERSION}") {
        return Err(ControlError::Protocol(
            "unsupported capability schema".to_string(),
        ));
    }
    let capabilities = serde_json::from_str::<ControlCapabilities>(json)
        .map_err(|error| ControlError::Protocol(format!("invalid capability JSON: {error}")))?;
    validate_control_capabilities(&capabilities, true)?;
    Ok(capabilities)
}

fn validate_control_capabilities(
    capabilities: &ControlCapabilities,
    protocol: bool,
) -> ControlResult<()> {
    require_capability(
        capabilities.profiles.choices.len() <= MAX_CAPABILITY_PROFILES
            && capabilities.fans.rpm_channels.len() <= MAX_CAPABILITY_FANS
            && capabilities.lighting.len() <= MAX_CAPABILITY_LIGHTING_DEVICES,
        protocol,
        "capability collection exceeds protocol bounds",
    )?;
    validate_capability_text(
        &capabilities.vendor,
        MAX_CAPABILITY_IDENTITY_BYTES,
        protocol,
    )?;
    validate_capability_text(
        &capabilities.product,
        MAX_CAPABILITY_IDENTITY_BYTES,
        protocol,
    )?;

    require_capability(
        capabilities.profiles.backend.is_some()
            || capabilities.profiles.choices.is_empty() && capabilities.profiles.current.is_none(),
        protocol,
        "profile state requires a profile backend",
    )?;
    for choice in &capabilities.profiles.choices {
        validate_profile_token(&choice.raw, protocol)?;
        validate_capability_text(&choice.label, MAX_CAPABILITY_LABEL_BYTES, protocol)?;
    }
    if let Some(current) = &capabilities.profiles.current {
        validate_profile_token(current, protocol)?;
    }

    require_capability(
        capabilities.fans.backend.is_some()
            || !(capabilities.fans.auto || capabilities.fans.manual || capabilities.fans.maximum),
        protocol,
        "fan modes require a fan backend",
    )?;
    for (position, channel) in capabilities.fans.rpm_channels.iter().enumerate() {
        require_capability(
            (1..=MAX_CAPABILITY_FANS as u8).contains(&channel.index)
                && (position == 0
                    || capabilities.fans.rpm_channels[position - 1].index < channel.index)
                && channel.rpm.is_none_or(|rpm| rpm <= 100_000),
            protocol,
            "fan channel is invalid or indices are not unique and ascending",
        )?;
        validate_capability_text(&channel.label, MAX_CAPABILITY_LABEL_BYTES, protocol)?;
    }

    for (position, device) in capabilities.lighting.iter().enumerate() {
        require_capability(
            (1..=16).contains(&device.zones)
                && (position == 0 || capabilities.lighting[position - 1].id < device.id),
            protocol,
            "invalid lighting zone count or non-ascending device ID",
        )?;
        validate_capability_text(&device.id, MAX_CAPABILITY_TOKEN_BYTES, protocol)?;
    }
    Ok(())
}

fn validate_capability_text(value: &str, maximum: usize, protocol: bool) -> ControlResult<()> {
    require_capability(
        !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control),
        protocol,
        "invalid capability text",
    )
}

fn require_capability(valid: bool, protocol: bool, message: &str) -> ControlResult<()> {
    if valid {
        Ok(())
    } else if protocol {
        Err(ControlError::Protocol(message.to_string()))
    } else {
        Err(ControlError::InvalidRequest(message.to_string()))
    }
}

fn validate_profile_token(value: &str, protocol: bool) -> ControlResult<()> {
    let valid = !value.is_empty()
        && value.len() <= MAX_CAPABILITY_TOKEN_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        });
    require_capability(valid, protocol, "invalid profile token")
}

fn valid_lighting_device_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CAPABILITY_TOKEN_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn parse_profile_apply_receipt(value: &str) -> ControlResult<ProfileApplyReceipt> {
    let fields = parse_exact_fields(
        value,
        &[
            "profile",
            "gpu",
            "pstates",
            "gpu_capability",
            "power_capability",
            "enforced_mw",
            "max_mw",
            "reasons",
        ],
    )?;
    validate_profile_token(fields[0], true)?;
    let firmware_profile = fields[0].to_string();
    let gpu_offsets = match fields[1] {
        "unavailable" => GpuOffsetState::Unavailable,
        "reset" => GpuOffsetState::Reset,
        "oem-turbo" => GpuOffsetState::OemTurbo,
        "custom-or-partial" => GpuOffsetState::CustomOrPartial,
        _ => {
            return Err(ControlError::Protocol(
                "invalid GPU offset state in control receipt".to_string(),
            ));
        }
    };
    let gpu_pstate_count = fields[2].parse::<usize>().map_err(|_| {
        ControlError::Protocol("invalid GPU P-state count in control receipt".to_string())
    })?;
    let gpu_capability_available = parse_capability(fields[3], "GPU")?;
    let power_capability_available = parse_capability(fields[4], "power")?;
    let power = if power_capability_available {
        let enforced_limit_mw = parse_u32_receipt(fields[5], "enforced power limit")?;
        let maximum_limit_mw = parse_u32_receipt(fields[6], "maximum power limit")?;
        let reasons = fields[7].strip_prefix("0x").ok_or_else(|| {
            ControlError::Protocol("invalid clock event reasons in control receipt".to_string())
        })?;
        let reasons = u64::from_str_radix(reasons, 16).map_err(|_| {
            ControlError::Protocol("invalid clock event reasons in control receipt".to_string())
        })?;
        Some(ProfilePowerReceipt {
            enforced_limit_mw,
            maximum_limit_mw,
            clock_event_reasons: ClockEventReasons::from_bits(reasons),
        })
    } else {
        if fields[5..].iter().any(|value| *value != "unavailable") {
            return Err(ControlError::Protocol(
                "inconsistent unavailable power receipt".to_string(),
            ));
        }
        None
    };
    if !gpu_capability_available
        && (gpu_offsets != GpuOffsetState::Unavailable || gpu_pstate_count != 0 || power.is_some())
    {
        return Err(ControlError::Protocol(
            "inconsistent unavailable GPU receipt".to_string(),
        ));
    }
    if gpu_capability_available && gpu_offsets == GpuOffsetState::Unavailable {
        return Err(ControlError::Protocol(
            "inconsistent available GPU receipt".to_string(),
        ));
    }
    Ok(ProfileApplyReceipt {
        firmware_profile,
        gpu_offsets,
        gpu_pstate_count,
        gpu_capability_available,
        power,
    })
}

pub(crate) fn encode_profile_apply_receipt(receipt: &ProfileApplyReceipt) -> String {
    let gpu_offsets = match receipt.gpu_offsets {
        GpuOffsetState::Unavailable => "unavailable",
        GpuOffsetState::Reset => "reset",
        GpuOffsetState::OemTurbo => "oem-turbo",
        GpuOffsetState::CustomOrPartial => "custom-or-partial",
    };
    let gpu_capability = if receipt.gpu_capability_available {
        "available"
    } else {
        "unavailable"
    };
    let (power_capability, enforced, maximum, reasons) = match &receipt.power {
        Some(power) => (
            "available",
            power.enforced_limit_mw.to_string(),
            power.maximum_limit_mw.to_string(),
            format!("0x{:x}", power.clock_event_reasons.bits()),
        ),
        None => (
            "unavailable",
            "unavailable".to_string(),
            "unavailable".to_string(),
            "unavailable".to_string(),
        ),
    };
    format!(
        "profile={} gpu={gpu_offsets} pstates={} gpu_capability={gpu_capability} power_capability={power_capability} enforced_mw={enforced} max_mw={maximum} reasons={reasons}",
        receipt.firmware_profile, receipt.gpu_pstate_count,
    )
}

fn parse_exact_fields<'a>(value: &'a str, names: &[&str]) -> ControlResult<Vec<&'a str>> {
    let tokens: Vec<&str> = value.split_ascii_whitespace().collect();
    if tokens.len() != names.len() {
        return Err(ControlError::Protocol(
            "invalid field count in control response".to_string(),
        ));
    }
    names
        .iter()
        .zip(tokens)
        .map(|(expected, token)| {
            let (name, value) = token.split_once('=').ok_or_else(|| {
                ControlError::Protocol("invalid field in control response".to_string())
            })?;
            if name != *expected || value.is_empty() {
                return Err(ControlError::Protocol(format!(
                    "expected {expected} field in control response"
                )));
            }
            Ok(value)
        })
        .collect()
}

fn parse_capability(value: &str, label: &str) -> ControlResult<bool> {
    match value {
        "available" => Ok(true),
        "unavailable" => Ok(false),
        _ => Err(ControlError::Protocol(format!(
            "invalid {label} capability in control receipt"
        ))),
    }
}

fn parse_u32_receipt(value: &str, label: &str) -> ControlResult<u32> {
    value
        .parse::<u32>()
        .map_err(|_| ControlError::Protocol(format!("invalid {label} in control receipt")))
}

#[cfg(test)]
mod tests {
    use super::{
        CapabilityFanBackend, CapabilityLightingBackend, CapabilityLightingTarget,
        CapabilityProfileBackend, ControlCapabilities, ControlClient, ControlError,
        ControlFanCapabilities, ControlFanRpmChannel, ControlLightingDevice, ControlLightingModes,
        ControlPlatformCapabilities, ControlProfileCapabilities, ControlProfileChoice,
        MAX_CAPABILITY_FANS, MAX_CAPABILITY_LIGHTING_DEVICES, MAX_CAPABILITY_PROFILES,
        MAX_CONTROL_RESPONSE_LINE_BYTES, ProfileApplyReceipt, ProfilePowerReceipt,
        encode_control_capabilities, encode_profile_apply_receipt, parse_control_capabilities,
        parse_handshake, parse_profile_apply_receipt, parse_response, read_response_line,
    };
    use crate::nvidia::ClockEventReasons;
    use crate::tuning::GpuOffsetState;
    use std::io::{BufRead, BufReader, Cursor, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    #[test]
    fn response_status_tokens_are_exact_and_errors_are_classified() {
        assert_eq!(parse_response("OK ready").unwrap(), "ready");
        assert_eq!(parse_response("OK").unwrap(), "");
        assert_eq!(
            parse_response("ERR unsupported command").unwrap_err(),
            ControlError::CommandRejected("unsupported command".to_string())
        );
        assert!(!ControlError::CommandRejected("no".to_string()).invalidates_session());
        assert!(!ControlError::InvalidRequest("no".to_string()).invalidates_session());
        assert!(ControlError::Timeout.invalidates_session());
        assert!(matches!(
            parse_response("OKAY ready"),
            Err(ControlError::Protocol(_))
        ));
        assert!(matches!(
            parse_response("ERROR rejected"),
            Err(ControlError::Protocol(_))
        ));
    }

    #[test]
    fn rejected_command_keeps_the_same_stream_usable() {
        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let reader = BufReader::new(client_stream.try_clone().unwrap());
        let mut client = ControlClient {
            stream: client_stream,
            reader,
        };
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(server_stream.try_clone().unwrap());
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            assert_eq!(command, "FIRST\n");
            server_stream
                .write_all(b"ERR deliberately rejected\n")
                .unwrap();
            command.clear();
            reader.read_line(&mut command).unwrap();
            assert_eq!(command, "SECOND\n");
            server_stream.write_all(b"OK same-session\n").unwrap();
        });

        let error = client.request("FIRST").unwrap_err();
        assert_eq!(
            error,
            ControlError::CommandRejected("deliberately rejected".to_string())
        );
        assert!(!error.invalidates_session());
        assert_eq!(client.request("SECOND").unwrap(), "same-session");
        server.join().unwrap();
    }

    #[test]
    fn typed_lighting_power_emits_the_bounded_v2_command() {
        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let reader = BufReader::new(client_stream.try_clone().unwrap());
        let mut client = ControlClient {
            stream: client_stream,
            reader,
        };
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(server_stream.try_clone().unwrap());
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            assert_eq!(command, "LIGHTING POWER zoned-wmi-keyboard ON\n");
            server_stream
                .write_all(b"OK power=on mode=2 speed=4 brightness=80 direction=0 color=010203 zones=010203,010203,010203,010203\n")
                .unwrap();
            command.clear();
            reader.read_line(&mut command).unwrap();
            assert_eq!(command, "LIGHTING POWER zoned-wmi-keyboard OFF\n");
            server_stream
                .write_all(b"OK power=off mode=2 speed=4 brightness=0 direction=0 color=010203 zones=010203,010203,010203,010203\n")
                .unwrap();
        });

        assert!(
            client
                .lighting_power("zoned-wmi-keyboard", true)
                .unwrap()
                .starts_with("power=on mode=2")
        );
        assert!(
            client
                .lighting_power("zoned-wmi-keyboard", false)
                .unwrap()
                .starts_with("power=off mode=2")
        );
        server.join().unwrap();
    }

    #[test]
    fn typed_lighting_power_rejects_invalid_ids_before_writing() {
        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let reader = BufReader::new(client_stream.try_clone().unwrap());
        let mut client = ControlClient {
            stream: client_stream,
            reader,
        };
        let oversized = "a".repeat(super::MAX_CAPABILITY_TOKEN_BYTES + 1);

        for invalid in ["", "keyboard id", "keyboard/0", oversized.as_str()] {
            assert!(matches!(
                client.lighting_power(invalid, true),
                Err(ControlError::InvalidRequest(_))
            ));
        }

        server_stream.set_nonblocking(true).unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(
            server_stream.read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn eof_cannot_frame_a_partial_response_as_a_valid_command_result() {
        let mut reader = BufReader::new(Cursor::new(b"OK partial"));
        assert!(matches!(
            read_response_line(&mut reader, Instant::now() + Duration::from_secs(1)),
            Err(ControlError::Transport(_))
        ));
    }

    #[test]
    fn handshake_requires_exact_named_fields() {
        let receipt = parse_handshake("protocol=2 daemon=0.2.0").unwrap();
        assert_eq!(receipt.protocol, 2);
        assert_eq!(receipt.daemon_version, "0.2.0");
        assert!(parse_handshake("daemon=0.2.0 protocol=2").is_err());
        assert!(parse_handshake("protocol=two daemon=0.2.0").is_err());
        assert!(parse_handshake("protocol=2 daemon=0.2.0 extra=yes").is_err());
    }

    fn example_capabilities() -> ControlCapabilities {
        ControlCapabilities {
            vendor: "Acer".to_string(),
            product: "Predator PHN16-72".to_string(),
            reference_model: true,
            profiles: ControlProfileCapabilities {
                backend: Some(CapabilityProfileBackend::Kernel),
                choices: vec![
                    ControlProfileChoice {
                        raw: "low-power".to_string(),
                        label: "Low power".to_string(),
                        selectable: true,
                    },
                    ControlProfileChoice {
                        raw: "balanced".to_string(),
                        label: "Balanced".to_string(),
                        selectable: true,
                    },
                    ControlProfileChoice {
                        raw: "custom".to_string(),
                        label: "Current custom state".to_string(),
                        selectable: false,
                    },
                ],
                current: Some("balanced".to_string()),
            },
            fans: ControlFanCapabilities {
                backend: Some(CapabilityFanBackend::KernelPwm),
                rpm_channels: vec![
                    ControlFanRpmChannel {
                        index: 2,
                        label: "GPU fan".to_string(),
                        rpm: Some(2_400),
                    },
                    ControlFanRpmChannel {
                        index: 1,
                        label: "CPU fan".to_string(),
                        rpm: Some(2_600),
                    },
                    ControlFanRpmChannel {
                        index: 3,
                        label: "System / GPU 2".to_string(),
                        rpm: None,
                    },
                ],
                auto: true,
                manual: true,
                maximum: true,
            },
            lighting: vec![
                ControlLightingDevice {
                    id: "logo-0".to_string(),
                    backend: CapabilityLightingBackend::Enek5130,
                    target: CapabilityLightingTarget::CoverLogo,
                    zones: 1,
                    modes: ControlLightingModes {
                        static_color: true,
                        brightness: true,
                        breathing: true,
                        neon: false,
                    },
                    state_readable: false,
                },
                ControlLightingDevice {
                    id: "keyboard-0".to_string(),
                    backend: CapabilityLightingBackend::ZonedWmi,
                    target: CapabilityLightingTarget::Keyboard,
                    zones: 4,
                    modes: ControlLightingModes {
                        static_color: true,
                        brightness: true,
                        breathing: true,
                        neon: true,
                    },
                    state_readable: true,
                },
            ],
            platform: ControlPlatformCapabilities {
                battery_limit: true,
                battery_calibration: true,
                usb_off_charging: true,
                keyboard_timeout: false,
                boot_sound: true,
                lcd_override: false,
                rear_logo: true,
            },
        }
    }

    #[test]
    fn capabilities_have_a_bounded_canonical_typed_round_trip() {
        let capabilities = example_capabilities();
        let encoded = encode_control_capabilities(&capabilities).unwrap();
        assert!(encoded.len() + "OK ".len() <= MAX_CONTROL_RESPONSE_LINE_BYTES);
        assert!(!encoded.contains("/sys/"));
        assert!(!encoded.contains("method"));
        assert!(!encoded.contains("packet"));

        let mut expected = capabilities;
        expected
            .fans
            .rpm_channels
            .sort_by_key(|channel| channel.index);
        expected
            .lighting
            .sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(parse_control_capabilities(&encoded).unwrap(), expected);

        let reencoded =
            encode_control_capabilities(&parse_control_capabilities(&encoded).unwrap()).unwrap();
        assert_eq!(reencoded, encoded);
        assert!(encoded.starts_with("caps=1 {"));
        assert!(!encoded.contains(['\r', '\n']));
    }

    #[test]
    fn capabilities_reject_oversized_or_inconsistent_collections() {
        let mut capabilities = example_capabilities();
        capabilities.profiles.choices = (0..=MAX_CAPABILITY_PROFILES)
            .map(|index| ControlProfileChoice {
                raw: format!("profile-{index}"),
                label: format!("Profile {index}"),
                selectable: true,
            })
            .collect();
        assert!(matches!(
            encode_control_capabilities(&capabilities),
            Err(ControlError::InvalidRequest(_))
        ));

        let mut capabilities = example_capabilities();
        capabilities.fans.rpm_channels = (1..=MAX_CAPABILITY_FANS as u8 + 1)
            .map(|index| ControlFanRpmChannel {
                index,
                label: format!("Fan {index}"),
                rpm: None,
            })
            .collect();
        assert!(encode_control_capabilities(&capabilities).is_err());
        assert!(matches!(
            parse_control_capabilities(&format!(
                "caps=1 {}",
                serde_json::to_string(&capabilities).unwrap()
            )),
            Err(ControlError::Protocol(_))
        ));

        let mut capabilities = example_capabilities();
        capabilities.lighting = (0..=MAX_CAPABILITY_LIGHTING_DEVICES)
            .map(|index| ControlLightingDevice {
                id: format!("light-{index}"),
                backend: CapabilityLightingBackend::Enek5130,
                target: CapabilityLightingTarget::Keyboard,
                zones: 1,
                modes: ControlLightingModes::default(),
                state_readable: false,
            })
            .collect();
        assert!(encode_control_capabilities(&capabilities).is_err());

        let mut capabilities = example_capabilities();
        capabilities.profiles.backend = None;
        assert!(encode_control_capabilities(&capabilities).is_err());
    }

    #[test]
    fn malformed_capabilities_fail_closed() {
        let valid = encode_control_capabilities(&example_capabilities()).unwrap();
        let malformed = [
            valid.replace("caps=1", "caps=2"),
            valid.replacen('{', "{\"unknown\":true,", 1),
            valid.replace("\"backend\":\"kernel-pwm\"", "\"backend\":\"invalid\""),
            valid.replace("\"index\":2", "\"index\":1"),
            valid.replace("\"zones\":4", "\"zones\":0"),
            valid.replace("\"vendor\":\"Acer\"", "\"vendor\":\"\\n\""),
        ];
        assert!(malformed.iter().all(|value| matches!(
            parse_control_capabilities(value),
            Err(ControlError::Protocol(_))
        )));
    }

    #[test]
    fn client_negotiates_v2_before_requesting_capabilities() {
        let expected = example_capabilities();
        let encoded = encode_control_capabilities(&expected).unwrap();
        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(server_stream.try_clone().unwrap());
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            assert_eq!(command, "HELLO 2\n");
            server_stream
                .write_all(b"OK protocol=2 daemon=0.2.0\n")
                .unwrap();
            command.clear();
            reader.read_line(&mut command).unwrap();
            assert_eq!(command, "CAPS\n");
            writeln!(server_stream, "OK {encoded}").unwrap();
        });

        let mut client = ControlClient::from_stream(client_stream).unwrap();
        let mut expected = expected;
        expected
            .fans
            .rpm_channels
            .sort_by_key(|channel| channel.index);
        expected
            .lighting
            .sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(client.capabilities().unwrap(), expected);
        server.join().unwrap();
    }

    #[test]
    fn client_does_not_fall_back_after_a_v1_handshake() {
        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(server_stream.try_clone().unwrap());
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            assert_eq!(command, "HELLO 2\n");
            server_stream
                .write_all(b"OK protocol=1 daemon=0.1.1\n")
                .unwrap();
            server_stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            command.clear();
            assert_eq!(reader.read_line(&mut command).unwrap(), 0);
        });

        assert!(matches!(
            ControlClient::from_stream(client_stream),
            Err(ControlError::Protocol(_))
        ));
        server.join().unwrap();
    }

    #[test]
    fn profile_receipt_has_a_stable_typed_round_trip() {
        let receipt = ProfileApplyReceipt {
            firmware_profile: "performance".to_string(),
            gpu_offsets: GpuOffsetState::OemTurbo,
            gpu_pstate_count: 16,
            gpu_capability_available: true,
            power: Some(ProfilePowerReceipt {
                enforced_limit_mw: 115_000,
                maximum_limit_mw: 140_000,
                clock_event_reasons: ClockEventReasons::from_bits(
                    ClockEventReasons::GPU_IDLE | ClockEventReasons::SOFTWARE_POWER_CAP,
                ),
            }),
        };
        let encoded = encode_profile_apply_receipt(&receipt);
        assert_eq!(parse_profile_apply_receipt(&encoded).unwrap(), receipt);
        assert!(encoded.contains("gpu=oem-turbo"));
        assert!(encoded.contains("enforced_mw=115000"));
        assert!(!encoded.contains("OemTurbo"));
    }

    #[test]
    fn unavailable_profile_capabilities_are_consistent_and_fail_closed() {
        let receipt = ProfileApplyReceipt {
            firmware_profile: "balanced".to_string(),
            gpu_offsets: GpuOffsetState::Unavailable,
            gpu_pstate_count: 0,
            gpu_capability_available: false,
            power: None,
        };
        assert_eq!(
            parse_profile_apply_receipt(&encode_profile_apply_receipt(&receipt)).unwrap(),
            receipt
        );
        assert!(
            parse_profile_apply_receipt(
                "profile=balanced gpu=reset pstates=1 gpu_capability=unavailable power_capability=unavailable enforced_mw=unavailable max_mw=unavailable reasons=unavailable"
            )
            .is_err()
        );
        assert!(
            parse_profile_apply_receipt(
                "profile=balanced gpu=unavailable pstates=0 gpu_capability=unavailable power_capability=available enforced_mw=80000 max_mw=140000 reasons=0x0"
            )
            .is_err()
        );
    }

    #[test]
    fn transient_eagain_is_retried_until_one_absolute_deadline() {
        let (client, mut server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(35));
            server.write_all(b"OK ready\n").unwrap();
        });
        let mut reader = BufReader::new(client);
        let response =
            read_response_line(&mut reader, Instant::now() + Duration::from_millis(250)).unwrap();
        producer.join().unwrap();
        assert_eq!(response, "OK ready");
    }

    #[test]
    fn oversized_control_response_is_drained_without_unbounded_allocation() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let producer = std::thread::spawn(move || {
            server
                .write_all(&vec![b'x'; MAX_CONTROL_RESPONSE_LINE_BYTES * 256])
                .unwrap();
            server.write_all(b"\nOK next\n").unwrap();
        });
        let mut reader = BufReader::new(client);
        assert!(
            read_response_line(&mut reader, Instant::now() + Duration::from_secs(2))
                .unwrap_err()
                .to_string()
                .contains("exceeded")
        );
        assert_eq!(
            read_response_line(&mut reader, Instant::now() + Duration::from_secs(2)).unwrap(),
            "OK next"
        );
        producer.join().unwrap();
    }
}
