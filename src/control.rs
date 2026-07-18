use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};
use std::{error::Error, fmt};

use crate::hardware::PlatformProfile;
use crate::nvidia::ClockEventReasons;
use crate::platform::{PlatformState, RearLogoState, UsbCharging, parse_state};
use crate::telemetry::{MemoryHardwareInfo, parse_memory_hardware};
use crate::tuning::GpuOffsetState;

pub const CONTROL_SOCKET: &str = "/run/asense-control.sock";
pub const CONTROL_PROTOCOL_VERSION: u16 = 1;
pub(crate) const MAX_CONTROL_RESPONSE_LINE_BYTES: usize = 4096;
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
    pub firmware_profile: PlatformProfile,
    pub gpu_offsets: GpuOffsetState,
    pub gpu_pstate_count: usize,
    pub gpu_capability_available: bool,
    pub power: Option<ProfilePowerReceipt>,
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
        const ALLOWED: [&str; 5] = [
            "low-power",
            "quiet",
            "balanced",
            "balanced-performance",
            "performance",
        ];
        if !ALLOWED.contains(&profile) {
            return Err(ControlError::InvalidRequest(
                "unsupported platform profile".to_string(),
            ));
        }
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

    pub fn keyboard_power(&mut self, enabled: bool) -> ControlResult<String> {
        self.request(if enabled {
            "RGB POWER ON"
        } else {
            "RGB POWER OFF"
        })
    }

    pub fn keyboard_zones(&mut self, zones: [&str; 4], brightness: u8) -> ControlResult<String> {
        if brightness > 100 || zones.iter().any(|color| !valid_color(color)) {
            return Err(ControlError::InvalidRequest(
                "invalid keyboard zone request".to_string(),
            ));
        }
        self.request(&format!(
            "RGB ZONES {} {} {} {} {brightness}",
            zones[0], zones[1], zones[2], zones[3]
        ))
    }

    pub fn keyboard_effect(
        &mut self,
        mode: u8,
        speed: u8,
        brightness: u8,
        direction: u8,
        color: [u8; 3],
    ) -> ControlResult<String> {
        self.request(&format!(
            "RGB EFFECT {mode} {speed} {brightness} {direction} {} {} {}",
            color[0], color[1], color[2]
        ))
    }

    pub fn platform_state(&mut self) -> ControlResult<PlatformState> {
        self.platform_request("PLATFORM GET")
    }

    pub fn set_battery_limit(&mut self, enabled: bool) -> ControlResult<PlatformState> {
        self.platform_request(if enabled {
            "PLATFORM BATTERY_LIMIT ON"
        } else {
            "PLATFORM BATTERY_LIMIT OFF"
        })
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
        self.platform_request(if enabled {
            "PLATFORM KEYBOARD_TIMEOUT ON"
        } else {
            "PLATFORM KEYBOARD_TIMEOUT OFF"
        })
    }

    pub fn set_boot_sound(&mut self, enabled: bool) -> ControlResult<PlatformState> {
        self.platform_request(if enabled {
            "PLATFORM BOOT_SOUND ON"
        } else {
            "PLATFORM BOOT_SOUND OFF"
        })
    }

    pub fn set_lcd_override(&mut self, enabled: bool) -> ControlResult<PlatformState> {
        self.platform_request(if enabled {
            "PLATFORM LCD_OVERRIDE ON"
        } else {
            "PLATFORM LCD_OVERRIDE OFF"
        })
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
    let firmware_profile = PlatformProfile::from_sysfs(fields[0])
        .map_err(|_| ControlError::Protocol("invalid profile in control receipt".to_string()))?;
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
        receipt.firmware_profile.as_sysfs(),
        receipt.gpu_pstate_count,
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

fn valid_color(value: &str) -> bool {
    value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::{
        ControlClient, ControlError, MAX_CONTROL_RESPONSE_LINE_BYTES, ProfileApplyReceipt,
        ProfilePowerReceipt, encode_profile_apply_receipt, parse_handshake,
        parse_profile_apply_receipt, parse_response, read_response_line, valid_color,
    };
    use crate::hardware::PlatformProfile;
    use crate::nvidia::ClockEventReasons;
    use crate::tuning::GpuOffsetState;
    use std::io::{BufRead, BufReader, Cursor, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    #[test]
    fn rgb_color_format_is_exact() {
        assert!(valid_color("00aAfF"));
        assert!(!valid_color("#00aaff"));
        assert!(!valid_color("00aaff00"));
    }

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
    fn eof_cannot_frame_a_partial_response_as_a_valid_command_result() {
        let mut reader = BufReader::new(Cursor::new(b"OK partial"));
        assert!(matches!(
            read_response_line(&mut reader, Instant::now() + Duration::from_secs(1)),
            Err(ControlError::Transport(_))
        ));
    }

    #[test]
    fn handshake_requires_exact_named_fields() {
        let receipt = parse_handshake("protocol=1 daemon=0.1.0").unwrap();
        assert_eq!(receipt.protocol, 1);
        assert_eq!(receipt.daemon_version, "0.1.0");
        assert!(parse_handshake("daemon=0.1.0 protocol=1").is_err());
        assert!(parse_handshake("protocol=one daemon=0.1.0").is_err());
        assert!(parse_handshake("protocol=1 daemon=0.1.0 extra=yes").is_err());
    }

    #[test]
    fn profile_receipt_has_a_stable_typed_round_trip() {
        let receipt = ProfileApplyReceipt {
            firmware_profile: PlatformProfile::Turbo,
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
            firmware_profile: PlatformProfile::Balanced,
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
