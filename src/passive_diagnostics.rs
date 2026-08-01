//! Bounded read-only daemon diagnostics used by probe schema 3.
//!
//! This module deliberately has no mutation, NVIDIA or active lighting-
//! controller dependency. It reads only fixed sysfs files and exposes no
//! caller-selected path, firmware method, selector or payload.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::hardware::discover_acer_hwmon;
use crate::passive_hid::{self, DiagnosticHid};
use crate::platform::find_wmi_group;

pub(crate) const PASSIVE_DIAGNOSTICS_SCHEMA: u8 = 1;
pub(crate) const MAX_PASSIVE_DIAGNOSTICS_BYTES: usize = 32_764;
const MAX_LABEL_BYTES: usize = 64;
const MAX_PROFILE_CHOICES: usize = 8;
const MAX_CHANNELS: usize = 8;
const MAX_FILE_BYTES: usize = 256;

const GAMING_WMI_GUID: &str = "7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56";
const BATTERY_WMI_GUID: &str = "79772EC5-04B1-4BFD-843C-61E7F77B6CC9";
const APGE_WMI_GUID: &str = "61EF69EA-865C-4BC3-A502-A0DEBA0CB531";

const TIMEOUT_UNINITIALIZED: u64 = 0;
const TIMEOUT_OFF: u64 = 0x80000;
const TIMEOUT_ON: u64 = 0x1e0000080000;
const BOOT_SOUND_OFF: u64 = 0;
const BOOT_SOUND_ON: u64 = 0x100;
const LCD_STATE_VALID: u64 = 1 << 24;
const LCD_STATE_ENABLED: u64 = 1 << 48;
const USB_STATUS_MASK: u64 = 0xff;
const USB_MODE_MASK: u64 = 0xff << 8;
const USB_THRESHOLD_MASK: u64 = 0xff << 16;
const USB_MODE_ENABLED: u8 = 0x0f;
const USB_MODE_DISABLED: u8 = 0x1f;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PassiveDiagnostics {
    pub schema: u8,
    pub profile: DiagnosticProfile,
    pub fans: DiagnosticFans,
    pub platform: DiagnosticPlatform,
    pub lighting: Vec<DiagnosticLighting>,
    pub hid: Vec<DiagnosticHid>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum DiagnosticObservation<T> {
    Value { value: T },
    Absent { reason: DiagnosticAbsence },
    Error { error: DiagnosticError },
}

impl<T> DiagnosticObservation<T> {
    pub(crate) fn value(value: T) -> Self {
        Self::Value { value }
    }

    pub(crate) fn absent(reason: DiagnosticAbsence) -> Self {
        Self::Absent { reason }
    }

    pub(crate) fn error(error: DiagnosticError) -> Self {
        Self::Error { error }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticAbsence {
    NotExposed,
    IncompleteInterface,
    NotApplicable,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticError {
    pub stage: DiagnosticErrorStage,
    pub class: DiagnosticErrorClass,
    pub errno: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<DiagnosticRaw>,
}

impl DiagnosticError {
    pub(crate) fn new(
        stage: DiagnosticErrorStage,
        class: DiagnosticErrorClass,
        errno: Option<i32>,
    ) -> Self {
        Self {
            stage,
            class,
            errno,
            raw: None,
        }
    }

    pub(crate) fn invalid_raw(raw: DiagnosticRaw) -> Self {
        Self {
            stage: DiagnosticErrorStage::Decode,
            class: DiagnosticErrorClass::InvalidValue,
            errno: None,
            raw: Some(raw),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticErrorStage {
    Discover,
    Open,
    Read,
    Decode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticErrorClass {
    NotFound,
    PermissionDenied,
    Io,
    InvalidValue,
    Oversize,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticRaw {
    pub encoding: DiagnosticRawEncoding,
    pub bytes: usize,
    pub value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub(crate) enum DiagnosticRawEncoding {
    #[serde(rename = "u8-hex")]
    U8Hex,
    #[serde(rename = "ascii-token")]
    AsciiToken,
    #[serde(rename = "scalar-hex")]
    ScalarHex,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticProfile {
    pub transport: DiagnosticObservation<DiagnosticProfileTransport>,
    pub current: DiagnosticObservation<DiagnosticProfileCurrent>,
    pub choices: DiagnosticObservation<Vec<DiagnosticProfileChoice>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticProfileBackend {
    Kernel,
    GamingWmi,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticProfileTransport {
    pub source: DiagnosticSource,
    pub backend: DiagnosticProfileBackend,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticProfileCurrent {
    pub source: DiagnosticSource,
    pub raw: DiagnosticRaw,
    pub profile: Option<DiagnosticProfileName>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticProfileChoice {
    pub command: String,
    pub profile: DiagnosticProfileName,
    pub transport_raw: Option<DiagnosticRaw>,
    pub selectable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticProfileName {
    Quiet,
    Balanced,
    Performance,
    Turbo,
    Eco,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticFans {
    pub control: DiagnosticObservation<DiagnosticFanControl>,
    pub channels: Vec<DiagnosticFanChannel>,
    pub rpm: Vec<DiagnosticRpm>,
    pub temperatures: Vec<DiagnosticTemperature>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticFanControl {
    pub source: DiagnosticSource,
    pub backend: DiagnosticFanBackend,
    pub modes: DiagnosticFanModes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticFanBackend {
    KernelPwm,
    GamingWmi,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticFanModes {
    pub auto: bool,
    pub manual: bool,
    pub maximum: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticFanChannel {
    pub channel: u8,
    pub source: DiagnosticSource,
    pub setpoint_unit: DiagnosticSetpointUnit,
    pub setpoint: DiagnosticObservation<u8>,
    pub mode: DiagnosticObservation<DiagnosticFanMode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticSetpointUnit {
    Pwm255,
    Percent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticFanMode {
    Maximum,
    Manual,
    Auto,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticRpm {
    pub channel: u8,
    pub label: String,
    pub read: DiagnosticObservation<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticTemperature {
    pub channel: u8,
    pub label: String,
    pub read: DiagnosticObservation<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticPlatform {
    pub transport: DiagnosticObservation<DiagnosticPlatformTransport>,
    pub fields: Vec<DiagnosticPlatformField>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticPlatformTransport {
    pub source: DiagnosticSource,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticPlatformField {
    pub name: DiagnosticPlatformFieldName,
    pub expected: bool,
    pub exposed: bool,
    pub source: DiagnosticSource,
    pub read: DiagnosticObservation<DiagnosticPlatformValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticPlatformFieldName {
    BatteryLimit,
    BatteryCalibration,
    UsbOffCharging,
    KeyboardTimeout,
    BootSound,
    LcdOverride,
    RearLogo,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum DiagnosticPlatformValue {
    Bool {
        value: bool,
    },
    UsbThreshold {
        value: u8,
    },
    RearLogo {
        enabled: bool,
        brightness: u8,
        color: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticLighting {
    pub id: String,
    pub backend: DiagnosticLightingBackend,
    pub target: DiagnosticLightingTarget,
    pub zones: u8,
    pub modes: DiagnosticLightingModes,
    pub state_readable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticLightingBackend {
    ZonedWmi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticLightingTarget {
    Keyboard,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticLightingModes {
    pub static_color: bool,
    pub brightness: bool,
    pub breathing: bool,
    pub neon: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticSource {
    KernelPlatformProfile,
    GamingWmiProfile,
    KnownGamingWmiCommands,
    AcerHwmon,
    GamingWmiFan,
    PlatformDiscovery,
    AsenseRgb,
    AsenseBattery,
    AsenseApge,
}

impl PassiveDiagnostics {
    pub(crate) fn collect() -> Self {
        Self::collect_at(Path::new("/"))
    }

    pub(crate) fn collect_at(root: &Path) -> Self {
        Self {
            schema: PASSIVE_DIAGNOSTICS_SCHEMA,
            profile: collect_profile(root),
            fans: collect_fans(root),
            platform: collect_platform(root),
            lighting: collect_lighting(root),
            hid: passive_hid::collect_at(root),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema != PASSIVE_DIAGNOSTICS_SCHEMA {
            return Err("passive diagnostics schema differs".to_string());
        }
        validate_observation(&self.profile.transport, |_| Ok(()))?;
        validate_observation(&self.profile.current, |current| {
            validate_raw(&current.raw)?;
            if current.raw.encoding == DiagnosticRawEncoding::U8Hex && current.raw.bytes != 1 {
                return Err("Gaming-WMI profile raw must contain one byte".to_string());
            }
            Ok(())
        })?;
        validate_observation(&self.profile.choices, |choices| {
            if choices.len() > MAX_PROFILE_CHOICES {
                return Err("too many passive profile choices".to_string());
            }
            for (index, choice) in choices.iter().enumerate() {
                validate_token(&choice.command, 48, "profile command")?;
                if !choice.selectable {
                    return Err("passive profile choice is not selectable".to_string());
                }
                if choices[..index]
                    .iter()
                    .any(|previous| previous.command == choice.command)
                {
                    return Err("passive profile choices are not unique".to_string());
                }
                if let Some(raw) = &choice.transport_raw {
                    validate_raw(raw)?;
                }
            }
            Ok(())
        })?;
        validate_observation(&self.fans.control, |_| Ok(()))?;
        if self.fans.channels.len() > MAX_CHANNELS
            || self.fans.rpm.len() > MAX_CHANNELS
            || self.fans.temperatures.len() > MAX_CHANNELS
        {
            return Err("passive fan inventory is oversized".to_string());
        }
        for channel in &self.fans.channels {
            validate_channel(channel.channel)?;
            validate_observation(&channel.setpoint, |value| match channel.setpoint_unit {
                DiagnosticSetpointUnit::Pwm255 => Ok(()),
                DiagnosticSetpointUnit::Percent if *value <= 100 => Ok(()),
                DiagnosticSetpointUnit::Percent => {
                    Err("Gaming-WMI fan setpoint exceeds 100".to_string())
                }
            })?;
            validate_observation(&channel.mode, |_| Ok(()))?;
        }
        validate_channel_order(
            self.fans.channels.iter().map(|channel| channel.channel),
            "fan setpoint",
        )?;
        for rpm in &self.fans.rpm {
            validate_channel(rpm.channel)?;
            validate_text(&rpm.label, MAX_LABEL_BYTES, "fan label")?;
            validate_observation(&rpm.read, |_| Ok(()))?;
        }
        validate_channel_order(
            self.fans.rpm.iter().map(|channel| channel.channel),
            "fan RPM",
        )?;
        for temperature in &self.fans.temperatures {
            validate_channel(temperature.channel)?;
            validate_text(&temperature.label, MAX_LABEL_BYTES, "temperature label")?;
            validate_observation(&temperature.read, |value| {
                if (-100_000..=250_000).contains(value) {
                    Ok(())
                } else {
                    Err("temperature is outside physical bounds".to_string())
                }
            })?;
        }
        validate_channel_order(
            self.fans.temperatures.iter().map(|channel| channel.channel),
            "temperature",
        )?;
        validate_observation(&self.platform.transport, |_| Ok(()))?;
        const ORDER: [DiagnosticPlatformFieldName; 7] = [
            DiagnosticPlatformFieldName::BatteryLimit,
            DiagnosticPlatformFieldName::BatteryCalibration,
            DiagnosticPlatformFieldName::UsbOffCharging,
            DiagnosticPlatformFieldName::KeyboardTimeout,
            DiagnosticPlatformFieldName::BootSound,
            DiagnosticPlatformFieldName::LcdOverride,
            DiagnosticPlatformFieldName::RearLogo,
        ];
        if self.platform.fields.len() != ORDER.len()
            || !self
                .platform
                .fields
                .iter()
                .zip(ORDER)
                .all(|(field, expected)| field.name == expected)
        {
            return Err("passive platform field order differs".to_string());
        }
        for (field, expected_source) in self.platform.fields.iter().zip([
            DiagnosticSource::AsenseBattery,
            DiagnosticSource::AsenseBattery,
            DiagnosticSource::AsenseApge,
            DiagnosticSource::AsenseApge,
            DiagnosticSource::AsenseRgb,
            DiagnosticSource::AsenseRgb,
            DiagnosticSource::AsenseRgb,
        ]) {
            if field.source != expected_source {
                return Err("passive platform field source differs".to_string());
            }
            validate_observation(&field.read, |value| match value {
                DiagnosticPlatformValue::Bool { .. } => Ok(()),
                DiagnosticPlatformValue::UsbThreshold {
                    value: 0 | 10 | 20 | 30,
                } => Ok(()),
                DiagnosticPlatformValue::UsbThreshold { .. } => {
                    Err("USB threshold is invalid".to_string())
                }
                DiagnosticPlatformValue::RearLogo {
                    brightness, color, ..
                } if *brightness <= 100 && is_hex(color, 6) => Ok(()),
                DiagnosticPlatformValue::RearLogo { .. } => {
                    Err("rear-logo value is invalid".to_string())
                }
            })?;
        }
        if self.lighting.len() > 1 {
            return Err("too many passive zoned-WMI lighting devices".to_string());
        }
        for light in &self.lighting {
            if light.id != "zoned-wmi-keyboard"
                || light.backend != DiagnosticLightingBackend::ZonedWmi
                || light.target != DiagnosticLightingTarget::Keyboard
                || !(1..=4).contains(&light.zones)
                || !light.modes.static_color
                || !light.modes.brightness
                || !light.modes.breathing
                || !light.modes.neon
            {
                return Err("passive zoned-WMI lighting evidence is invalid".to_string());
            }
        }
        passive_hid::validate_inventory(&self.hid)?;
        let encoded = serde_json::to_vec(self)
            .map_err(|error| format!("cannot size passive diagnostics: {error}"))?;
        if encoded.len() > MAX_PASSIVE_DIAGNOSTICS_BYTES {
            return Err("passive diagnostics response is oversized".to_string());
        }
        Ok(())
    }
}

fn collect_lighting(root: &Path) -> Vec<DiagnosticLighting> {
    let wmi_root = rooted(root, "sys/bus/wmi/devices");
    let Some(base) = find_wmi_group(&wmi_root, GAMING_WMI_GUID, "asense_rgb") else {
        return Vec::new();
    };
    let power = base.join("power");
    let effect = base.join("effect");
    let zones = base.join("zones");
    if !power.is_file() || !effect.is_file() || !zones.is_file() {
        return Vec::new();
    }
    let DiagnosticObservation::Value { value: zone_mask } = read_text(&base.join("zone_mask"))
    else {
        return Vec::new();
    };
    let Some(zone_count) = zoned_wmi_zone_count(&zone_mask) else {
        return Vec::new();
    };
    let state_readable = matches!(
        (read_text(&power), read_text(&effect), read_text(&zones)),
        (
            DiagnosticObservation::Value { value: power },
            DiagnosticObservation::Value { value: effect },
            DiagnosticObservation::Value { value: zones },
        ) if !power.is_empty() && !effect.is_empty() && !zones.is_empty()
    );
    vec![DiagnosticLighting {
        id: "zoned-wmi-keyboard".to_string(),
        backend: DiagnosticLightingBackend::ZonedWmi,
        target: DiagnosticLightingTarget::Keyboard,
        zones: zone_count,
        modes: DiagnosticLightingModes {
            static_color: true,
            brightness: true,
            breathing: true,
            neon: true,
        },
        state_readable,
    }]
}

fn zoned_wmi_zone_count(value: &str) -> Option<u8> {
    match value.strip_prefix("0x").unwrap_or(value) {
        "01" | "1" => Some(1),
        "03" | "3" => Some(2),
        "07" | "7" => Some(3),
        "0f" | "0F" | "f" | "F" => Some(4),
        _ => None,
    }
}

pub(crate) fn encode(value: &PassiveDiagnostics) -> Result<String, String> {
    value.validate()?;
    serde_json::to_string(value)
        .map_err(|error| format!("cannot encode passive diagnostics: {error}"))
}

pub(crate) fn parse(value: &str) -> Result<PassiveDiagnostics, String> {
    if value.len() > MAX_PASSIVE_DIAGNOSTICS_BYTES {
        return Err("passive diagnostics response is oversized".to_string());
    }
    let diagnostics: PassiveDiagnostics = serde_json::from_str(value)
        .map_err(|error| format!("invalid passive diagnostics response: {error}"))?;
    diagnostics.validate()?;
    Ok(diagnostics)
}

fn collect_profile(root: &Path) -> DiagnosticProfile {
    if let Some((profile, choices)) = discover_kernel_profile(root) {
        let source = DiagnosticSource::KernelPlatformProfile;
        return DiagnosticProfile {
            transport: DiagnosticObservation::value(DiagnosticProfileTransport {
                source,
                backend: DiagnosticProfileBackend::Kernel,
            }),
            current: read_profile_current(&profile, source, DiagnosticRawEncoding::AsciiToken),
            choices: read_profile_choices(&choices, DiagnosticProfileBackend::Kernel),
        };
    }

    let wmi_root = rooted(root, "sys/bus/wmi/devices");
    let diagnostics = find_wmi_group(&wmi_root, GAMING_WMI_GUID, "asense_diagnostics");
    let production = find_wmi_group(&wmi_root, GAMING_WMI_GUID, "gaming_profile");
    let Some(diagnostics_or_production) = diagnostics.as_ref().or(production.as_ref()) else {
        return DiagnosticProfile {
            transport: DiagnosticObservation::absent(DiagnosticAbsence::NotExposed),
            current: DiagnosticObservation::absent(DiagnosticAbsence::NotExposed),
            choices: DiagnosticObservation::absent(DiagnosticAbsence::NotExposed),
        };
    };
    let source = DiagnosticSource::GamingWmiProfile;
    DiagnosticProfile {
        transport: DiagnosticObservation::value(DiagnosticProfileTransport {
            source,
            backend: DiagnosticProfileBackend::GamingWmi,
        }),
        current: diagnostics.as_ref().map_or_else(
            || {
                read_profile_current(
                    &diagnostics_or_production.join("profile"),
                    source,
                    DiagnosticRawEncoding::AsciiToken,
                )
            },
            |base| {
                read_profile_current(
                    &base.join("profile_raw"),
                    source,
                    DiagnosticRawEncoding::U8Hex,
                )
            },
        ),
        choices: production.map_or_else(
            || DiagnosticObservation::absent(DiagnosticAbsence::NotExposed),
            |base| read_profile_choices(&base.join("choices"), DiagnosticProfileBackend::GamingWmi),
        ),
    }
}

fn discover_kernel_profile(root: &Path) -> Option<(PathBuf, PathBuf)> {
    let class_root = rooted(root, "sys/class/platform-profile");
    let mut candidates = Vec::new();
    if let Ok(entries) = fs::read_dir(class_root) {
        for entry in entries.flatten().take(32) {
            let base = entry.path();
            let profile = base.join("profile");
            let choices = base.join("choices");
            if !profile.is_file() || !choices.is_file() {
                continue;
            }
            let name = fs::read_to_string(base.join("name"))
                .unwrap_or_else(|_| entry.file_name().to_string_lossy().into_owned());
            if name.to_ascii_lowercase().contains("acer") {
                candidates.push((profile, choices));
            }
        }
    }
    candidates.sort();
    candidates.into_iter().next().or_else(|| {
        let profile = rooted(root, "sys/firmware/acpi/platform_profile");
        let choices = rooted(root, "sys/firmware/acpi/platform_profile_choices");
        (profile.is_file() && choices.is_file()).then_some((profile, choices))
    })
}

fn read_profile_current(
    path: &Path,
    source: DiagnosticSource,
    encoding: DiagnosticRawEncoding,
) -> DiagnosticObservation<DiagnosticProfileCurrent> {
    match read_text(path) {
        DiagnosticObservation::Value { value } => {
            let raw = match encoding {
                DiagnosticRawEncoding::AsciiToken => {
                    if validate_token(&value, 48, "profile current").is_err() {
                        return DiagnosticObservation::error(DiagnosticError::new(
                            DiagnosticErrorStage::Decode,
                            DiagnosticErrorClass::InvalidValue,
                            None,
                        ));
                    }
                    DiagnosticRaw {
                        encoding,
                        bytes: value.len(),
                        value: value.clone(),
                    }
                }
                DiagnosticRawEncoding::U8Hex => {
                    if !is_hex(&value, 2) {
                        return DiagnosticObservation::error(DiagnosticError::new(
                            DiagnosticErrorStage::Decode,
                            DiagnosticErrorClass::InvalidValue,
                            None,
                        ));
                    }
                    DiagnosticRaw {
                        encoding,
                        bytes: 1,
                        value: value.to_ascii_lowercase(),
                    }
                }
                DiagnosticRawEncoding::ScalarHex => unreachable!(),
            };
            let profile = match encoding {
                DiagnosticRawEncoding::AsciiToken => profile_for_command(&raw.value),
                DiagnosticRawEncoding::U8Hex => profile_for_gaming_raw(&raw.value),
                DiagnosticRawEncoding::ScalarHex => None,
            };
            DiagnosticObservation::value(DiagnosticProfileCurrent {
                source,
                raw,
                profile,
            })
        }
        DiagnosticObservation::Absent { reason } => DiagnosticObservation::Absent { reason },
        DiagnosticObservation::Error { error } => DiagnosticObservation::Error { error },
    }
}

fn read_profile_choices(
    path: &Path,
    backend: DiagnosticProfileBackend,
) -> DiagnosticObservation<Vec<DiagnosticProfileChoice>> {
    match read_text(path) {
        DiagnosticObservation::Value { value } => {
            let mut choices = Vec::new();
            for command in value.split_ascii_whitespace() {
                let Some(profile) = profile_for_command(command) else {
                    return DiagnosticObservation::error(DiagnosticError::new(
                        DiagnosticErrorStage::Decode,
                        DiagnosticErrorClass::InvalidValue,
                        None,
                    ));
                };
                let transport_raw =
                    (backend == DiagnosticProfileBackend::GamingWmi).then(|| DiagnosticRaw {
                        encoding: DiagnosticRawEncoding::U8Hex,
                        bytes: 1,
                        value: gaming_raw_for_command(command)
                            .expect("known Gaming-WMI command has a raw coordinate")
                            .to_string(),
                    });
                choices.push(DiagnosticProfileChoice {
                    command: command.to_string(),
                    profile,
                    transport_raw,
                    selectable: true,
                });
            }
            if choices.is_empty() || choices.len() > MAX_PROFILE_CHOICES {
                return DiagnosticObservation::error(DiagnosticError::new(
                    DiagnosticErrorStage::Decode,
                    DiagnosticErrorClass::InvalidValue,
                    None,
                ));
            }
            DiagnosticObservation::value(choices)
        }
        DiagnosticObservation::Absent { reason } => DiagnosticObservation::Absent { reason },
        DiagnosticObservation::Error { error } => DiagnosticObservation::Error { error },
    }
}

fn profile_for_command(value: &str) -> Option<DiagnosticProfileName> {
    match value {
        "low-power" => Some(DiagnosticProfileName::Eco),
        "quiet" => Some(DiagnosticProfileName::Quiet),
        "balanced" => Some(DiagnosticProfileName::Balanced),
        "balanced-performance" => Some(DiagnosticProfileName::Performance),
        "performance" => Some(DiagnosticProfileName::Turbo),
        _ => None,
    }
}

fn profile_for_gaming_raw(value: &str) -> Option<DiagnosticProfileName> {
    match value {
        "00" => Some(DiagnosticProfileName::Quiet),
        "01" => Some(DiagnosticProfileName::Balanced),
        "04" => Some(DiagnosticProfileName::Performance),
        "05" => Some(DiagnosticProfileName::Turbo),
        "06" => Some(DiagnosticProfileName::Eco),
        _ => None,
    }
}

fn gaming_raw_for_command(value: &str) -> Option<&'static str> {
    match value {
        "quiet" => Some("00"),
        "balanced" => Some("01"),
        "balanced-performance" => Some("04"),
        "performance" => Some("05"),
        "low-power" => Some("06"),
        _ => None,
    }
}

fn collect_fans(root: &Path) -> DiagnosticFans {
    let hwmon = discover_acer_hwmon(root);
    let wmi_root = rooted(root, "sys/bus/wmi/devices");
    let gaming = find_wmi_group(&wmi_root, GAMING_WMI_GUID, "gaming_fan");
    let diagnostic = find_wmi_group(&wmi_root, GAMING_WMI_GUID, "asense_diagnostics");

    let kernel_complete = hwmon.as_ref().is_some_and(|base| {
        ["pwm1", "pwm2", "pwm1_enable", "pwm2_enable"]
            .iter()
            .all(|name| base.join(name).is_file())
    });
    let gaming_complete = gaming
        .as_ref()
        .is_some_and(|base| base.join("cpu_mode").is_file() && base.join("gpu_mode").is_file());

    let control = if kernel_complete {
        DiagnosticObservation::value(DiagnosticFanControl {
            source: DiagnosticSource::AcerHwmon,
            backend: DiagnosticFanBackend::KernelPwm,
            modes: DiagnosticFanModes {
                auto: true,
                manual: true,
                maximum: true,
            },
        })
    } else if gaming_complete {
        let base = gaming.as_ref().expect("complete Gaming-WMI group exists");
        DiagnosticObservation::value(DiagnosticFanControl {
            source: DiagnosticSource::GamingWmiFan,
            backend: DiagnosticFanBackend::GamingWmi,
            modes: DiagnosticFanModes {
                auto: true,
                manual: base.join("cpu_speed").is_file() && base.join("gpu_speed").is_file(),
                maximum: true,
            },
        })
    } else if diagnostic.is_some()
        || hwmon.as_ref().is_some_and(|base| {
            (1_u8..=2).any(|channel| {
                base.join(format!("pwm{channel}")).is_file()
                    || base.join(format!("pwm{channel}_enable")).is_file()
            })
        })
    {
        DiagnosticObservation::absent(DiagnosticAbsence::IncompleteInterface)
    } else {
        DiagnosticObservation::absent(DiagnosticAbsence::NotExposed)
    };

    let kernel_has_setpoint = hwmon.as_ref().is_some_and(|base| {
        (1_u8..=2).any(|channel| {
            base.join(format!("pwm{channel}")).is_file()
                || base.join(format!("pwm{channel}_enable")).is_file()
        })
    });
    let channels = if kernel_complete {
        collect_kernel_fan_channels(hwmon.as_ref().expect("complete hwmon exists"))
    } else if gaming_complete {
        diagnostic.as_ref().map_or_else(
            || {
                collect_gaming_fan_channels(
                    gaming.as_ref().expect("complete Gaming-WMI group exists"),
                    false,
                )
            },
            |base| collect_gaming_fan_channels(base, true),
        )
    } else if kernel_has_setpoint {
        collect_kernel_fan_channels(hwmon.as_ref().expect("partial hwmon exists"))
    } else if let Some(base) = diagnostic.as_ref() {
        collect_gaming_fan_channels(base, true)
    } else {
        Vec::new()
    };
    let (rpm, temperatures) = hwmon.as_ref().map_or_else(
        || (Vec::new(), Vec::new()),
        |base| (collect_rpm(base), collect_temperatures(base)),
    );
    DiagnosticFans {
        control,
        channels,
        rpm,
        temperatures,
    }
}

fn collect_kernel_fan_channels(base: &Path) -> Vec<DiagnosticFanChannel> {
    (1_u8..=2)
        .map(|channel| DiagnosticFanChannel {
            channel,
            source: DiagnosticSource::AcerHwmon,
            setpoint_unit: DiagnosticSetpointUnit::Pwm255,
            setpoint: read_number(&base.join(format!("pwm{channel}"))),
            mode: read_fan_mode(&base.join(format!("pwm{channel}_enable"))),
        })
        .collect()
}

fn collect_gaming_fan_channels(base: &Path, diagnostic_names: bool) -> Vec<DiagnosticFanChannel> {
    [(1_u8, "cpu"), (2_u8, "gpu")]
        .into_iter()
        .map(|(channel, role)| {
            let prefix = if diagnostic_names { "diagnostic_" } else { "" };
            DiagnosticFanChannel {
                channel,
                source: DiagnosticSource::GamingWmiFan,
                setpoint_unit: DiagnosticSetpointUnit::Percent,
                setpoint: read_number(&base.join(format!("{prefix}{role}_speed"))),
                mode: read_fan_mode(&base.join(format!("{prefix}{role}_mode"))),
            }
        })
        .collect()
}

fn read_fan_mode(path: &Path) -> DiagnosticObservation<DiagnosticFanMode> {
    match read_number::<u8>(path) {
        DiagnosticObservation::Value { value } => match value {
            0 => DiagnosticObservation::value(DiagnosticFanMode::Maximum),
            1 => DiagnosticObservation::value(DiagnosticFanMode::Manual),
            2 => DiagnosticObservation::value(DiagnosticFanMode::Auto),
            _ => DiagnosticObservation::error(DiagnosticError::new(
                DiagnosticErrorStage::Decode,
                DiagnosticErrorClass::InvalidValue,
                None,
            )),
        },
        DiagnosticObservation::Absent { reason } => DiagnosticObservation::Absent { reason },
        DiagnosticObservation::Error { error } => DiagnosticObservation::Error { error },
    }
}

fn collect_rpm(base: &Path) -> Vec<DiagnosticRpm> {
    (1_u8..=MAX_CHANNELS as u8)
        .filter_map(|channel| {
            let path = base.join(format!("fan{channel}_input"));
            path.exists().then(|| DiagnosticRpm {
                channel,
                label: read_label(base, "fan", channel),
                read: read_number(&path),
            })
        })
        .collect()
}

fn collect_temperatures(base: &Path) -> Vec<DiagnosticTemperature> {
    (1_u8..=MAX_CHANNELS as u8)
        .filter_map(|channel| {
            let path = base.join(format!("temp{channel}_input"));
            path.exists().then(|| DiagnosticTemperature {
                channel,
                label: read_label(base, "temp", channel),
                read: read_number(&path),
            })
        })
        .collect()
}

fn read_label(base: &Path, prefix: &str, channel: u8) -> String {
    let fallback = match (prefix, channel) {
        (_, 1) => "CPU".to_string(),
        (_, 2) => "GPU".to_string(),
        ("fan", _) => format!("Fan {channel}"),
        _ => format!("Temperature {channel}"),
    };
    match read_text(&base.join(format!("{prefix}{channel}_label"))) {
        DiagnosticObservation::Value { value }
            if !value.is_empty() && value.len() <= MAX_LABEL_BYTES =>
        {
            value
        }
        _ => fallback,
    }
}

fn collect_platform(root: &Path) -> DiagnosticPlatform {
    let wmi_root = rooted(root, "sys/bus/wmi/devices");
    let gaming_diag = find_wmi_group(&wmi_root, GAMING_WMI_GUID, "asense_diagnostics");
    let battery_diag = find_wmi_group(&wmi_root, BATTERY_WMI_GUID, "asense_diagnostics");
    let apge_diag = find_wmi_group(&wmi_root, APGE_WMI_GUID, "asense_diagnostics");
    let gaming = find_wmi_group(&wmi_root, GAMING_WMI_GUID, "asense_rgb");
    let battery = find_wmi_group(&wmi_root, BATTERY_WMI_GUID, "asense_battery").or_else(|| {
        gaming
            .as_ref()
            .filter(|base| base.join("battery_limit").is_file())
            .cloned()
    });
    let apge = find_wmi_group(&wmi_root, APGE_WMI_GUID, "asense_apge").or_else(|| {
        gaming
            .as_ref()
            .filter(|base| base.join("usb_charging").is_file())
            .cloned()
    });

    let any_transport = gaming_diag.is_some()
        || battery_diag.is_some()
        || apge_diag.is_some()
        || gaming.is_some()
        || battery.is_some()
        || apge.is_some();
    let transport = if any_transport {
        DiagnosticObservation::value(DiagnosticPlatformTransport {
            source: DiagnosticSource::PlatformDiscovery,
        })
    } else {
        DiagnosticObservation::absent(DiagnosticAbsence::NotExposed)
    };

    let battery_raw = battery_diag.as_ref().map_or_else(
        || DiagnosticObservation::absent(DiagnosticAbsence::NotExposed),
        |base| read_text(&base.join("battery_raw")),
    );

    let fields = vec![
        platform_battery_field(
            DiagnosticPlatformFieldName::BatteryLimit,
            0x01,
            3,
            battery.as_ref(),
            &battery_raw,
        ),
        platform_battery_field(
            DiagnosticPlatformFieldName::BatteryCalibration,
            0x02,
            4,
            battery.as_ref(),
            &battery_raw,
        ),
        platform_field(
            DiagnosticPlatformFieldName::UsbOffCharging,
            DiagnosticSource::AsenseApge,
            apge_diag.as_ref().map(|base| base.join("usb_raw")),
            apge.as_ref().map(|base| base.join("usb_charging")),
            decode_usb_raw,
            decode_usb_text,
        ),
        platform_field(
            DiagnosticPlatformFieldName::KeyboardTimeout,
            DiagnosticSource::AsenseApge,
            apge_diag.as_ref().map(|base| base.join("timeout_raw")),
            apge.as_ref().map(|base| base.join("keyboard_timeout")),
            decode_timeout_raw,
            decode_bool_text,
        ),
        platform_field(
            DiagnosticPlatformFieldName::BootSound,
            DiagnosticSource::AsenseRgb,
            gaming_diag.as_ref().map(|base| base.join("boot_sound_raw")),
            gaming.as_ref().map(|base| base.join("boot_sound")),
            decode_boot_sound_raw,
            decode_bool_text,
        ),
        platform_field(
            DiagnosticPlatformFieldName::LcdOverride,
            DiagnosticSource::AsenseRgb,
            gaming_diag.as_ref().map(|base| base.join("lcd_raw")),
            gaming.as_ref().map(|base| base.join("lcd_override")),
            decode_lcd_raw,
            decode_bool_text,
        ),
        platform_field(
            DiagnosticPlatformFieldName::RearLogo,
            DiagnosticSource::AsenseRgb,
            gaming_diag.as_ref().map(|base| base.join("rear_logo_raw")),
            gaming.as_ref().map(|base| base.join("rear_logo")),
            decode_logo_raw,
            decode_logo_text,
        ),
    ];
    DiagnosticPlatform { transport, fields }
}

fn platform_battery_field(
    name: DiagnosticPlatformFieldName,
    support_mask: u8,
    value_index: usize,
    production: Option<&PathBuf>,
    raw_read: &DiagnosticObservation<String>,
) -> DiagnosticPlatformField {
    let normal_name = match name {
        DiagnosticPlatformFieldName::BatteryLimit => "battery_limit",
        DiagnosticPlatformFieldName::BatteryCalibration => "battery_calibration",
        _ => unreachable!(),
    };
    let exposed = production.is_some_and(|base| base.join(normal_name).is_file());
    let expected = !matches!(raw_read, DiagnosticObservation::Absent { .. }) || exposed;
    let read = match raw_read.clone() {
        DiagnosticObservation::Value { value } => match parse_hex_bytes(&value, 8) {
            Ok(bytes) if bytes[0] & support_mask != 0 => {
                DiagnosticObservation::value(DiagnosticPlatformValue::Bool {
                    value: bytes[value_index] != 0,
                })
            }
            Ok(_) => DiagnosticObservation::absent(DiagnosticAbsence::NotApplicable),
            Err(error) => DiagnosticObservation::error(error),
        },
        DiagnosticObservation::Error { error } => DiagnosticObservation::Error { error },
        DiagnosticObservation::Absent { .. } if exposed => production.map_or_else(
            || DiagnosticObservation::absent(DiagnosticAbsence::NotExposed),
            |base| read_normal_value(&base.join(normal_name), decode_bool_text),
        ),
        DiagnosticObservation::Absent { reason } => DiagnosticObservation::Absent { reason },
    };
    DiagnosticPlatformField {
        name,
        expected,
        exposed,
        source: DiagnosticSource::AsenseBattery,
        read,
    }
}

fn platform_field(
    name: DiagnosticPlatformFieldName,
    source: DiagnosticSource,
    raw_path: Option<PathBuf>,
    normal_path: Option<PathBuf>,
    decode_raw: fn(&str) -> Result<DiagnosticPlatformValue, DiagnosticError>,
    decode_normal: fn(&str) -> Result<DiagnosticPlatformValue, DiagnosticError>,
) -> DiagnosticPlatformField {
    let raw_exposed = raw_path.as_ref().is_some_and(|path| path.is_file());
    let exposed = normal_path.as_ref().is_some_and(|path| path.is_file());
    let expected = raw_exposed || exposed;
    let read = if let Some(path) = raw_path.filter(|path| path.is_file()) {
        read_value(&path, decode_raw)
    } else if let Some(path) = normal_path.filter(|path| path.is_file()) {
        read_value(&path, decode_normal)
    } else {
        DiagnosticObservation::absent(DiagnosticAbsence::NotExposed)
    };
    DiagnosticPlatformField {
        name,
        expected,
        exposed,
        source,
        read,
    }
}

fn read_normal_value(
    path: &Path,
    decode: fn(&str) -> Result<DiagnosticPlatformValue, DiagnosticError>,
) -> DiagnosticObservation<DiagnosticPlatformValue> {
    read_value(path, decode)
}

fn read_value<T>(
    path: &Path,
    decode: fn(&str) -> Result<T, DiagnosticError>,
) -> DiagnosticObservation<T> {
    match read_text(path) {
        DiagnosticObservation::Value { value } => match decode(&value) {
            Ok(value) => DiagnosticObservation::value(value),
            Err(error) => DiagnosticObservation::error(error),
        },
        DiagnosticObservation::Absent { reason } => DiagnosticObservation::Absent { reason },
        DiagnosticObservation::Error { error } => DiagnosticObservation::Error { error },
    }
}

fn decode_bool_text(value: &str) -> Result<DiagnosticPlatformValue, DiagnosticError> {
    match value {
        "0" => Ok(DiagnosticPlatformValue::Bool { value: false }),
        "1" => Ok(DiagnosticPlatformValue::Bool { value: true }),
        _ => Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        )),
    }
}

fn decode_usb_text(value: &str) -> Result<DiagnosticPlatformValue, DiagnosticError> {
    let value = value.parse::<u8>().map_err(|_| {
        DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        )
    })?;
    if !matches!(value, 0 | 10 | 20 | 30) {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        ));
    }
    Ok(DiagnosticPlatformValue::UsbThreshold { value })
}

fn decode_logo_text(value: &str) -> Result<DiagnosticPlatformValue, DiagnosticError> {
    let fields = value.split(',').collect::<Vec<_>>();
    if fields.len() != 3 || !is_hex(fields[0], 6) {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        ));
    }
    let brightness = fields[1].parse::<u8>().ok();
    let enabled = fields[2].parse::<u8>().ok();
    match (brightness, enabled) {
        (Some(brightness), Some(enabled)) if brightness <= 100 && enabled <= 1 => {
            Ok(DiagnosticPlatformValue::RearLogo {
                enabled: enabled == 1,
                brightness,
                color: fields[0].to_ascii_lowercase(),
            })
        }
        _ => Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        )),
    }
}

fn decode_usb_raw(value: &str) -> Result<DiagnosticPlatformValue, DiagnosticError> {
    let raw = scalar_raw(value)?;
    let scalar =
        u64::from_str_radix(value, 16).map_err(|_| DiagnosticError::invalid_raw(raw.clone()))?;
    let status = (scalar & USB_STATUS_MASK) as u8;
    let mode = ((scalar & USB_MODE_MASK) >> 8) as u8;
    let threshold = ((scalar & USB_THRESHOLD_MASK) >> 16) as u8;
    if status != 0
        || !matches!(mode, USB_MODE_ENABLED | USB_MODE_DISABLED)
        || !matches!(threshold, 10 | 20 | 30)
    {
        return Err(DiagnosticError::invalid_raw(raw));
    }
    Ok(DiagnosticPlatformValue::UsbThreshold {
        value: if mode == USB_MODE_ENABLED {
            threshold
        } else {
            0
        },
    })
}

fn decode_timeout_raw(value: &str) -> Result<DiagnosticPlatformValue, DiagnosticError> {
    let (scalar, raw) = parse_scalar(value)?;
    match scalar {
        TIMEOUT_UNINITIALIZED | TIMEOUT_OFF => Ok(DiagnosticPlatformValue::Bool { value: false }),
        TIMEOUT_ON => Ok(DiagnosticPlatformValue::Bool { value: true }),
        _ => Err(DiagnosticError::invalid_raw(raw)),
    }
}

fn decode_boot_sound_raw(value: &str) -> Result<DiagnosticPlatformValue, DiagnosticError> {
    let (scalar, raw) = parse_scalar(value)?;
    match scalar {
        BOOT_SOUND_OFF => Ok(DiagnosticPlatformValue::Bool { value: false }),
        BOOT_SOUND_ON => Ok(DiagnosticPlatformValue::Bool { value: true }),
        _ => Err(DiagnosticError::invalid_raw(raw)),
    }
}

fn decode_lcd_raw(value: &str) -> Result<DiagnosticPlatformValue, DiagnosticError> {
    let (scalar, raw) = parse_scalar(value)?;
    if scalar & LCD_STATE_VALID == 0 {
        return Err(DiagnosticError::invalid_raw(raw));
    }
    Ok(DiagnosticPlatformValue::Bool {
        value: scalar & LCD_STATE_ENABLED != 0,
    })
}

fn decode_logo_raw(value: &str) -> Result<DiagnosticPlatformValue, DiagnosticError> {
    let bytes = parse_hex_bytes(value, 8)?;
    let raw = DiagnosticRaw {
        encoding: DiagnosticRawEncoding::ScalarHex,
        bytes: 8,
        value: value.to_ascii_lowercase(),
    };
    if bytes[0] != 0 || bytes[4] > 100 || bytes[5] > 1 {
        return Err(DiagnosticError::invalid_raw(raw));
    }
    Ok(DiagnosticPlatformValue::RearLogo {
        enabled: bytes[5] == 1,
        brightness: bytes[4],
        color: format!("{:02x}{:02x}{:02x}", bytes[1], bytes[2], bytes[3]),
    })
}

fn parse_scalar(value: &str) -> Result<(u64, DiagnosticRaw), DiagnosticError> {
    let raw = scalar_raw(value)?;
    u64::from_str_radix(value, 16)
        .map(|scalar| (scalar, raw.clone()))
        .map_err(|_| DiagnosticError::invalid_raw(raw))
}

fn scalar_raw(value: &str) -> Result<DiagnosticRaw, DiagnosticError> {
    if !is_hex(value, 16) {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        ));
    }
    Ok(DiagnosticRaw {
        encoding: DiagnosticRawEncoding::ScalarHex,
        bytes: 8,
        value: value.to_ascii_lowercase(),
    })
}

fn parse_hex_bytes(value: &str, bytes: usize) -> Result<Vec<u8>, DiagnosticError> {
    if !is_hex(value, bytes * 2) {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        ));
    }
    (0..bytes)
        .map(|index| {
            u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(|_| {
                DiagnosticError::new(
                    DiagnosticErrorStage::Decode,
                    DiagnosticErrorClass::InvalidValue,
                    None,
                )
            })
        })
        .collect()
}

fn read_number<T>(path: &Path) -> DiagnosticObservation<T>
where
    T: std::str::FromStr,
{
    match read_text(path) {
        DiagnosticObservation::Value { value } => value.parse::<T>().map_or_else(
            |_| {
                DiagnosticObservation::error(DiagnosticError::new(
                    DiagnosticErrorStage::Decode,
                    DiagnosticErrorClass::InvalidValue,
                    None,
                ))
            },
            DiagnosticObservation::value,
        ),
        DiagnosticObservation::Absent { reason } => DiagnosticObservation::Absent { reason },
        DiagnosticObservation::Error { error } => DiagnosticObservation::Error { error },
    }
}

fn read_text(path: &Path) -> DiagnosticObservation<String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DiagnosticObservation::absent(DiagnosticAbsence::NotExposed);
        }
        Err(error) => return DiagnosticObservation::error(io_error(&error)),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return DiagnosticObservation::error(io_error(&error)),
    };
    if !metadata.is_file() {
        return DiagnosticObservation::absent(DiagnosticAbsence::NotExposed);
    }
    let mut bytes = Vec::with_capacity(MAX_FILE_BYTES.min(64));
    match file
        .take(u64::try_from(MAX_FILE_BYTES + 1).expect("diagnostic read bound fits u64"))
        .read_to_end(&mut bytes)
    {
        Ok(_) if bytes.len() > MAX_FILE_BYTES => {
            DiagnosticObservation::error(DiagnosticError::new(
                DiagnosticErrorStage::Read,
                DiagnosticErrorClass::Oversize,
                None,
            ))
        }
        Ok(_) => match String::from_utf8(bytes) {
            Ok(value) => DiagnosticObservation::value(value.trim().to_string()),
            Err(_) => DiagnosticObservation::error(DiagnosticError::new(
                DiagnosticErrorStage::Decode,
                DiagnosticErrorClass::InvalidValue,
                None,
            )),
        },
        Err(error) => DiagnosticObservation::error(io_error(&error)),
    }
}

fn io_error(error: &std::io::Error) -> DiagnosticError {
    let class = match error.kind() {
        std::io::ErrorKind::NotFound => DiagnosticErrorClass::NotFound,
        std::io::ErrorKind::PermissionDenied => DiagnosticErrorClass::PermissionDenied,
        _ => DiagnosticErrorClass::Io,
    };
    DiagnosticError::new(DiagnosticErrorStage::Read, class, error.raw_os_error())
}

pub(crate) fn validate_observation<T>(
    value: &DiagnosticObservation<T>,
    validate: impl FnOnce(&T) -> Result<(), String>,
) -> Result<(), String> {
    match value {
        DiagnosticObservation::Value { value } => validate(value),
        DiagnosticObservation::Absent { .. } => Ok(()),
        DiagnosticObservation::Error { error } => validate_error(error),
    }
}

pub(crate) fn validate_error(error: &DiagnosticError) -> Result<(), String> {
    if error.errno.is_some_and(|errno| errno < 0) {
        return Err("diagnostic errno must be non-negative".to_string());
    }
    if let Some(raw) = &error.raw {
        validate_raw(raw)?;
        if raw.encoding != DiagnosticRawEncoding::ScalarHex || raw.bytes > 8 {
            return Err("diagnostic error raw is not a bounded scalar".to_string());
        }
    }
    Ok(())
}

pub(crate) fn validate_raw(raw: &DiagnosticRaw) -> Result<(), String> {
    match raw.encoding {
        DiagnosticRawEncoding::U8Hex => {
            if raw.bytes != 1 || !is_hex(&raw.value, 2) {
                return Err("invalid one-byte diagnostic raw".to_string());
            }
        }
        DiagnosticRawEncoding::AsciiToken => {
            if raw.bytes != raw.value.len() {
                return Err("diagnostic ASCII raw length differs".to_string());
            }
            validate_token(&raw.value, 48, "diagnostic ASCII raw")?;
        }
        DiagnosticRawEncoding::ScalarHex => {
            if raw.bytes == 0 || raw.bytes > 8 || !is_hex(&raw.value, raw.bytes * 2) {
                return Err("invalid diagnostic scalar raw".to_string());
            }
        }
    }
    Ok(())
}

fn validate_channel(channel: u8) -> Result<(), String> {
    if (1..=MAX_CHANNELS as u8).contains(&channel) {
        Ok(())
    } else {
        Err("diagnostic channel is out of range".to_string())
    }
}

fn validate_channel_order(channels: impl Iterator<Item = u8>, label: &str) -> Result<(), String> {
    let mut previous = None;
    for channel in channels {
        if previous.is_some_and(|previous| channel <= previous) {
            return Err(format!(
                "passive {label} channels are not sorted and unique"
            ));
        }
        previous = Some(channel);
    }
    Ok(())
}

pub(crate) fn validate_text(value: &str, max: usize, label: &str) -> Result<(), String> {
    if !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(format!("invalid {label}"))
    }
}

fn validate_token(value: &str, max: usize, label: &str) -> Result<(), String> {
    if !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        Ok(())
    } else {
        Err(format!("invalid {label}"))
    }
}

pub(crate) fn is_hex(value: &str, digits: usize) -> bool {
    value.len() == digits && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn rooted(root: &Path, relative: &str) -> PathBuf {
    root.join(relative.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "asense-passive-diagnostics-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, relative: &str, value: impl AsRef<[u8]>) {
            let path = rooted(&self.0, relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }

        fn directory(&self, relative: &str) -> PathBuf {
            let path = rooted(&self.0, relative);
            fs::create_dir_all(&path).unwrap();
            path
        }

        fn wmi_group(&self, guid: &str, group: &str) -> PathBuf {
            self.directory(&format!("sys/bus/wmi/devices/{guid}/{group}"))
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn raw_profile_02_is_unknown_and_never_creates_a_choice() {
        let tree = TempTree::new();
        let diagnostic = tree.wmi_group(GAMING_WMI_GUID, "asense_diagnostics");
        tree.write(
            diagnostic
                .join("profile_raw")
                .strip_prefix(&tree.0)
                .unwrap()
                .to_str()
                .unwrap(),
            "02\n",
        );

        let profile = collect_profile(&tree.0);
        assert!(matches!(
            profile.transport,
            DiagnosticObservation::Value {
                value: DiagnosticProfileTransport {
                    backend: DiagnosticProfileBackend::GamingWmi,
                    ..
                }
            }
        ));
        assert!(matches!(
            profile.current,
            DiagnosticObservation::Value {
                value: DiagnosticProfileCurrent {
                    raw: DiagnosticRaw { ref value, .. },
                    profile: None,
                    ..
                }
            } if value == "02"
        ));
        assert!(matches!(
            profile.choices,
            DiagnosticObservation::Absent {
                reason: DiagnosticAbsence::NotExposed
            }
        ));

        let production = tree.wmi_group(GAMING_WMI_GUID, "gaming_profile");
        tree.write(
            production
                .join("choices")
                .strip_prefix(&tree.0)
                .unwrap()
                .to_str()
                .unwrap(),
            "low-power quiet balanced balanced-performance performance\n",
        );
        let profile = collect_profile(&tree.0);
        let DiagnosticObservation::Value { value: choices } = profile.choices else {
            panic!("known production choices were not retained");
        };
        assert_eq!(
            choices
                .iter()
                .map(|choice| {
                    choice
                        .transport_raw
                        .as_ref()
                        .expect("Gaming-WMI choice has a raw coordinate")
                        .value
                        .as_str()
                })
                .collect::<Vec<_>>(),
            ["06", "00", "01", "04", "05"]
        );
        assert!(choices.iter().all(|choice| choice.selectable));
        assert!(choices.iter().all(|choice| {
            choice
                .transport_raw
                .as_ref()
                .is_none_or(|raw| raw.value != "02")
        }));

        let legacy_tree = TempTree::new();
        let legacy = legacy_tree.wmi_group(GAMING_WMI_GUID, "gaming_profile");
        legacy_tree.write(
            legacy
                .join("profile")
                .strip_prefix(&legacy_tree.0)
                .unwrap()
                .to_str()
                .unwrap(),
            "balanced-performance\n",
        );
        legacy_tree.write(
            legacy
                .join("choices")
                .strip_prefix(&legacy_tree.0)
                .unwrap()
                .to_str()
                .unwrap(),
            "low-power quiet balanced balanced-performance performance\n",
        );
        assert!(matches!(
            collect_profile(&legacy_tree.0).current,
            DiagnosticObservation::Value {
                value: DiagnosticProfileCurrent {
                    raw: DiagnosticRaw {
                        encoding: DiagnosticRawEncoding::AsciiToken,
                        ref value,
                        ..
                    },
                    profile: Some(DiagnosticProfileName::Performance),
                    ..
                }
            } if value == "balanced-performance"
        ));
    }

    #[test]
    fn fan_control_rpm_and_setpoint_units_remain_independent() {
        let rpm_tree = TempTree::new();
        rpm_tree.write("sys/class/hwmon/hwmon0/name", "acer\n");
        rpm_tree.write("sys/class/hwmon/hwmon0/fan1_input", "3175\n");
        rpm_tree.write("sys/class/hwmon/hwmon0/fan1_label", "CPU fan\n");
        let rpm_only = collect_fans(&rpm_tree.0);
        assert!(matches!(
            rpm_only.control,
            DiagnosticObservation::Absent {
                reason: DiagnosticAbsence::NotExposed
            }
        ));
        assert!(rpm_only.channels.is_empty());
        assert_eq!(rpm_only.rpm.len(), 1);
        assert!(matches!(
            &rpm_only.rpm[0].read,
            DiagnosticObservation::Value { value: 3175 }
        ));

        let control_tree = TempTree::new();
        let diagnostic = control_tree.wmi_group(GAMING_WMI_GUID, "asense_diagnostics");
        for (name, value) in [
            ("diagnostic_cpu_mode", "2\n"),
            ("diagnostic_gpu_mode", "0\n"),
            ("diagnostic_cpu_speed", "37\n"),
            ("diagnostic_gpu_speed", "82\n"),
        ] {
            control_tree.write(
                diagnostic
                    .join(name)
                    .strip_prefix(&control_tree.0)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                value,
            );
        }
        let production = control_tree.wmi_group(GAMING_WMI_GUID, "gaming_fan");
        for name in ["cpu_mode", "gpu_mode", "cpu_speed", "gpu_speed"] {
            control_tree.write(
                production
                    .join(name)
                    .strip_prefix(&control_tree.0)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "0\n",
            );
        }
        let control_only = collect_fans(&control_tree.0);
        assert!(matches!(
            control_only.control,
            DiagnosticObservation::Value {
                value: DiagnosticFanControl {
                    backend: DiagnosticFanBackend::GamingWmi,
                    ..
                }
            }
        ));
        assert!(control_only.rpm.is_empty());
        assert_eq!(control_only.channels.len(), 2);
        assert_eq!(
            control_only.channels[0].setpoint_unit,
            DiagnosticSetpointUnit::Percent
        );
        assert!(matches!(
            &control_only.channels[0].mode,
            DiagnosticObservation::Value {
                value: DiagnosticFanMode::Auto
            }
        ));
        assert!(matches!(
            &control_only.channels[1].mode,
            DiagnosticObservation::Value {
                value: DiagnosticFanMode::Maximum
            }
        ));

        let legacy_tree = TempTree::new();
        let legacy = legacy_tree.wmi_group(GAMING_WMI_GUID, "gaming_fan");
        for (name, value) in [
            ("cpu_mode", "1\n"),
            ("gpu_mode", "2\n"),
            ("cpu_speed", "41\n"),
            ("gpu_speed", "73\n"),
        ] {
            legacy_tree.write(
                legacy
                    .join(name)
                    .strip_prefix(&legacy_tree.0)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                value,
            );
        }
        let legacy_control = collect_fans(&legacy_tree.0);
        assert!(matches!(
            legacy_control.control,
            DiagnosticObservation::Value { .. }
        ));
        assert_eq!(legacy_control.channels.len(), 2);
        assert!(matches!(
            &legacy_control.channels[0].setpoint,
            DiagnosticObservation::Value { value: 41 }
        ));
        assert!(matches!(
            &legacy_control.channels[0].mode,
            DiagnosticObservation::Value {
                value: DiagnosticFanMode::Manual
            }
        ));
    }

    #[test]
    fn platform_fields_keep_exact_raw_validation_and_composite_source() {
        let tree = TempTree::new();
        let gaming = tree.wmi_group(GAMING_WMI_GUID, "asense_diagnostics");
        let battery = tree.wmi_group(BATTERY_WMI_GUID, "asense_diagnostics");
        let apge = tree.wmi_group(APGE_WMI_GUID, "asense_diagnostics");
        for (path, value) in [
            (battery.join("battery_raw"), "0300000100000000\n"),
            (apge.join("usb_raw"), "0000000000140f00\n"),
            (apge.join("timeout_raw"), "00001e0000080000\n"),
            (gaming.join("boot_sound_raw"), "0000000000000100\n"),
            (gaming.join("lcd_raw"), "0000000000000000\n"),
            (gaming.join("rear_logo_raw"), "00aabbcc32010000\n"),
        ] {
            tree.write(path.strip_prefix(&tree.0).unwrap().to_str().unwrap(), value);
        }

        let platform = collect_platform(&tree.0);
        assert!(matches!(
            platform.transport,
            DiagnosticObservation::Value {
                value: DiagnosticPlatformTransport {
                    source: DiagnosticSource::PlatformDiscovery
                }
            }
        ));
        assert!(platform.fields.iter().all(|field| field.expected));
        assert!(platform.fields.iter().all(|field| !field.exposed));
        assert!(matches!(
            &platform.fields[0].read,
            DiagnosticObservation::Value {
                value: DiagnosticPlatformValue::Bool { value: true }
            }
        ));
        assert!(matches!(
            &platform.fields[2].read,
            DiagnosticObservation::Value {
                value: DiagnosticPlatformValue::UsbThreshold { value: 20 }
            }
        ));
        assert!(matches!(
            &platform.fields[5].read,
            DiagnosticObservation::Error {
                error: DiagnosticError {
                    stage: DiagnosticErrorStage::Decode,
                    class: DiagnosticErrorClass::InvalidValue,
                    raw: Some(DiagnosticRaw { value, .. }),
                    ..
                }
            } if value == "0000000000000000"
        ));
        assert!(matches!(
            &platform.fields[6].read,
            DiagnosticObservation::Value {
                value: DiagnosticPlatformValue::RearLogo {
                    enabled: true,
                    brightness: 50,
                    color,
                }
            } if color == "aabbcc"
        ));
    }

    #[test]
    fn zoned_wmi_lighting_is_fixed_read_only_and_zone_mask_derived() {
        let tree = TempTree::new();
        let rgb = tree.wmi_group(GAMING_WMI_GUID, "asense_rgb");
        for (name, value) in [
            ("power", "1\n"),
            ("effect", "0,0,80,0,0,0,0\n"),
            ("zones", "ff0000,00ff00,0000ff,80\n"),
            ("zone_mask", "0x07\n"),
        ] {
            tree.write(
                rgb.join(name)
                    .strip_prefix(&tree.0)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                value,
            );
        }

        let before = fs::read_to_string(rgb.join("effect")).unwrap();
        let lighting = collect_lighting(&tree.0);
        assert_eq!(fs::read_to_string(rgb.join("effect")).unwrap(), before);
        assert_eq!(lighting.len(), 1);
        assert_eq!(lighting[0].id, "zoned-wmi-keyboard");
        assert_eq!(lighting[0].zones, 3);
        assert!(lighting[0].state_readable);
        assert!(lighting[0].modes.static_color);

        tree.write(
            rgb.join("power")
                .strip_prefix(&tree.0)
                .unwrap()
                .to_str()
                .unwrap(),
            "\n",
        );
        assert!(!collect_lighting(&tree.0)[0].state_readable);

        tree.write(
            rgb.join("zone_mask")
                .strip_prefix(&tree.0)
                .unwrap()
                .to_str()
                .unwrap(),
            "0x05\n",
        );
        assert!(collect_lighting(&tree.0).is_empty());
    }

    #[test]
    fn bounded_reads_and_canonical_channel_order_fail_closed() {
        let tree = TempTree::new();
        let diagnostic = tree.wmi_group(GAMING_WMI_GUID, "asense_diagnostics");
        tree.write(
            diagnostic
                .join("profile_raw")
                .strip_prefix(&tree.0)
                .unwrap()
                .to_str()
                .unwrap(),
            vec![b'a'; MAX_FILE_BYTES + 1],
        );
        assert!(matches!(
            collect_profile(&tree.0).current,
            DiagnosticObservation::Error {
                error: DiagnosticError {
                    stage: DiagnosticErrorStage::Read,
                    class: DiagnosticErrorClass::Oversize,
                    ..
                }
            }
        ));

        let rpm_tree = TempTree::new();
        rpm_tree.write("sys/class/hwmon/hwmon0/name", "acer\n");
        rpm_tree.write("sys/class/hwmon/hwmon0/fan1_input", "1000\n");
        let mut diagnostics = PassiveDiagnostics::collect_at(&rpm_tree.0);
        diagnostics.fans.rpm.push(diagnostics.fans.rpm[0].clone());
        assert!(diagnostics.validate().is_err());
    }

    #[test]
    fn maximum_passive_hid_inventory_fits_the_shared_response_envelope() {
        use crate::passive_hid::{
            DiagnosticHid, DiagnosticHidBus, DiagnosticHidDescriptor, DiagnosticHidFeatureGeometry,
            DiagnosticHidIdentity, DiagnosticHidName, DiagnosticHidRole,
        };

        let tree = TempTree::new();
        let mut diagnostics = PassiveDiagnostics::collect_at(&tree.0);
        diagnostics.lighting = vec![DiagnosticLighting {
            id: "zoned-wmi-keyboard".to_string(),
            backend: DiagnosticLightingBackend::ZonedWmi,
            target: DiagnosticLightingTarget::Keyboard,
            zones: 4,
            modes: DiagnosticLightingModes {
                static_color: true,
                brightness: true,
                breathing: true,
                neon: true,
            },
            state_readable: true,
        }];
        diagnostics.hid = (0_u16..8)
            .map(|ordinal| DiagnosticHid {
                role: DiagnosticHidRole::Enek5130Lighting,
                identity: DiagnosticHidIdentity {
                    bus: DiagnosticHidBus::I2c,
                    vid: 0x0cf2,
                    pid: 0x5130,
                    name: DiagnosticHidName::Enek5130,
                    interface: None,
                    usage_page: Some(0xff00),
                    usage: Some(ordinal),
                },
                driver: DiagnosticObservation::absent(DiagnosticAbsence::NotExposed),
                descriptor: DiagnosticObservation::value(DiagnosticHidDescriptor {
                    bytes: 4096,
                    sha256: format!("{ordinal:064x}"),
                    feature_reports: (0_u8..64)
                        .map(|id| DiagnosticHidFeatureGeometry { id, bytes: 4096 })
                        .collect(),
                }),
                a1: Some(DiagnosticObservation::absent(
                    DiagnosticAbsence::IncompleteInterface,
                )),
            })
            .collect();
        let encoded = encode(&diagnostics).unwrap();
        assert!(encoded.len() <= MAX_PASSIVE_DIAGNOSTICS_BYTES);
        assert!(encoded.len() + "OK ".len() <= crate::control::MAX_CONTROL_RESPONSE_LINE_BYTES);
    }
}
