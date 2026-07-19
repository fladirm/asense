//! Capability-driven access to Acer firmware controls exposed through stable
//! kernel and ASense typed sysfs interfaces.
//!
//! The privileged helper is expected to construct this type with
//! [`AcerHardware::discover`].  [`AcerHardware::discover_at`] exists so the
//! exact same discovery and mutation code can be exercised against an
//! isolated sysfs fixture in tests; production callers must not accept that
//! root from untrusted input.

use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::platform::find_wmi_group;

const EXPECTED_VENDOR: &str = "Acer";
const REFERENCE_PRODUCT: &str = "Predator PHN16-72";
const GAMING_WMI_GUID: &str = "7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56";
const PWM_MAX: u32 = 255;

#[derive(Debug)]
pub enum HardwareError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    UnsupportedPlatform {
        vendor: String,
        product: String,
    },
    AcerHwmonNotFound,
    MissingInterface(PathBuf),
    InvalidValue {
        field: &'static str,
        value: String,
    },
    ProfileUnavailable(PlatformProfile),
    ReadbackMismatch {
        field: &'static str,
        expected: String,
        actual: String,
    },
    MutationRolledBack {
        cause: Box<HardwareError>,
    },
    MutationFailsafeMaximum {
        cause: Box<HardwareError>,
        automatic: Box<HardwareError>,
    },
    MutationRecoveryFailed {
        cause: Box<HardwareError>,
        automatic: Box<HardwareError>,
        maximum: Box<HardwareError>,
    },
}

impl fmt::Display for HardwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} {}: {source}", path.display()),
            Self::UnsupportedPlatform { vendor, product } => write!(
                f,
                "unsupported platform (vendor={vendor:?}, product={product:?}); expected an Acer system"
            ),
            Self::AcerHwmonNotFound => write!(f, "acer hwmon interface was not found"),
            Self::MissingInterface(path) => {
                write!(
                    f,
                    "required firmware interface is missing: {}",
                    path.display()
                )
            }
            Self::InvalidValue { field, value } => {
                write!(f, "invalid {field} value: {value:?}")
            }
            Self::ProfileUnavailable(profile) => {
                write!(
                    f,
                    "platform profile {} is not advertised by firmware",
                    profile.as_sysfs()
                )
            }
            Self::ReadbackMismatch {
                field,
                expected,
                actual,
            } => write!(
                f,
                "{field} readback mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::MutationRolledBack { cause } => {
                write!(
                    f,
                    "fan mutation failed and automatic mode was restored: {cause}"
                )
            }
            Self::MutationFailsafeMaximum { cause, automatic } => write!(
                f,
                "fan mutation failed ({cause}); automatic recovery failed ({automatic}), so verified Maximum was applied"
            ),
            Self::MutationRecoveryFailed {
                cause,
                automatic,
                maximum,
            } => write!(
                f,
                "fan mutation failed ({cause}); both automatic recovery ({automatic}) and Maximum failsafe ({maximum}) failed"
            ),
        }
    }
}

impl Error for HardwareError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::MutationRolledBack { cause } => Some(cause),
            Self::MutationFailsafeMaximum { cause, .. } => Some(cause),
            Self::MutationRecoveryFailed { cause, .. } => Some(cause),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformProfile {
    Eco,
    Quiet,
    Balanced,
    Performance,
    Turbo,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileChoice {
    pub raw: String,
    pub label: String,
    pub selectable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileBackend {
    Kernel,
    AcerGamingWmi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FanBackend {
    KernelPwm,
    AcerGamingWmi,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FanRpmChannel {
    pub index: u8,
    pub label: String,
    pub rpm: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FanCapabilities {
    pub backend: Option<FanBackend>,
    pub rpm_channels: Vec<FanRpmChannel>,
    pub auto: bool,
    pub manual: bool,
    pub maximum: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCapabilities {
    pub backend: Option<ProfileBackend>,
    pub choices: Vec<ProfileChoice>,
    pub current: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HardwareCapabilities {
    pub vendor: String,
    pub product: String,
    pub reference_model: bool,
    pub profiles: ProfileCapabilities,
    pub fans: FanCapabilities,
}

impl PlatformProfile {
    pub const fn as_sysfs(self) -> &'static str {
        match self {
            Self::Eco => "low-power",
            Self::Quiet => "quiet",
            Self::Balanced => "balanced",
            Self::Performance => "balanced-performance",
            // On PHN16-72 acer_wmi maps the generic Linux performance profile
            // to the firmware's Turbo profile.
            Self::Turbo => "performance",
        }
    }

    pub fn from_sysfs(value: &str) -> Result<Self, HardwareError> {
        match value.trim() {
            "low-power" => Ok(Self::Eco),
            "quiet" => Ok(Self::Quiet),
            "balanced" => Ok(Self::Balanced),
            "balanced-performance" => Ok(Self::Performance),
            "performance" => Ok(Self::Turbo),
            value => Err(HardwareError::InvalidValue {
                field: "platform_profile",
                value: value.to_owned(),
            }),
        }
    }
}

impl fmt::Display for PlatformProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Eco => "Eco",
            Self::Quiet => "Quiet",
            Self::Balanced => "Balanced",
            Self::Performance => "Performance",
            Self::Turbo => "Turbo",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FanMode {
    Maximum = 0,
    Manual = 1,
    Automatic = 2,
}

impl FanMode {
    pub fn from_sysfs(value: &str) -> Result<Self, HardwareError> {
        match value.trim() {
            "0" => Ok(Self::Maximum),
            "1" => Ok(Self::Manual),
            "2" => Ok(Self::Automatic),
            value => Err(HardwareError::InvalidValue {
                field: "pwm_enable",
                value: value.to_owned(),
            }),
        }
    }

    const fn as_sysfs(self) -> &'static str {
        match self {
            Self::Maximum => "0",
            Self::Manual => "1",
            Self::Automatic => "2",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FanSetting {
    Automatic,
    Maximum,
    Manual { cpu_percent: u8, gpu_percent: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FanChannelState {
    /// The PHN16-72 firmware can expose working PWM/RPM attributes while its
    /// `pwm*_enable` getter returns `ENXIO`.  Keep that firmware limitation
    /// explicit instead of discarding otherwise valid fan telemetry or
    /// guessing a mode.
    pub mode: Option<FanMode>,
    pub pwm_raw: u8,
    pub rpm: u32,
}

impl FanChannelState {
    pub fn pwm_percent(self) -> f32 {
        f32::from(self.pwm_raw) * 100.0 / PWM_MAX as f32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FanState {
    pub cpu: FanChannelState,
    pub gpu: FanChannelState,
}

#[derive(Clone, Debug)]
pub struct AcerHardware {
    platform: VerifiedPlatform,
    root: PathBuf,
    hwmon: Option<PathBuf>,
    profile: Option<ProfileInterface>,
    fan_backend: Option<FanInterface>,
}

#[derive(Clone, Debug)]
struct VerifiedPlatform {
    vendor: String,
    product: String,
    acer: bool,
}

#[derive(Clone, Debug)]
struct ProfileInterface {
    backend: ProfileBackend,
    profile: PathBuf,
    choices: PathBuf,
}

#[derive(Clone, Debug)]
enum FanInterface {
    KernelPwm { base: PathBuf },
    AcerGamingWmi { base: PathBuf, manual: bool },
}

impl VerifiedPlatform {
    fn discover_at(root: &Path) -> Result<Self, HardwareError> {
        let vendor_path = rooted(root, "sys/class/dmi/id/sys_vendor");
        let product_path = rooted(root, "sys/class/dmi/id/product_name");
        let vendor = read_trimmed(&vendor_path, "read DMI vendor")?;
        let product = read_trimmed(&product_path, "read DMI product")?;
        let acer = vendor.eq_ignore_ascii_case(EXPECTED_VENDOR);
        Ok(Self {
            vendor,
            product,
            acer,
        })
    }
}

impl AcerHardware {
    /// Discover the real machine interfaces under `/`.
    pub fn discover() -> Result<Self, HardwareError> {
        Self::discover_at(Path::new("/"))
    }

    /// Discover against an alternate filesystem root.  This is intended for
    /// tests and must never be wired to a user-controlled privileged CLI flag.
    pub fn discover_at(root: impl AsRef<Path>) -> Result<Self, HardwareError> {
        let root = root.as_ref().to_path_buf();
        let platform = VerifiedPlatform::discover_at(&root)?;

        // Optional kernel and WMI surfaces are independent capability planes.
        // A transient I/O failure in one probe must not hide the others.
        let hwmon = platform.acer.then(|| discover_acer_hwmon(&root)).flatten();
        let kernel_profile = discover_kernel_profile_interface(&root);
        let gaming_profile = platform
            .acer
            .then(|| discover_gaming_wmi_profile_interface(&root))
            .flatten();
        let profile = kernel_profile.or(gaming_profile);
        let fan_backend = platform
            .acer
            .then(|| {
                discover_kernel_fan_interface(hwmon.as_deref())
                    .or_else(|| discover_gaming_wmi_fan_interface(&root, hwmon.as_deref()))
            })
            .flatten();

        Ok(Self {
            platform,
            root,
            hwmon,
            profile,
            fan_backend,
        })
    }

    pub fn current_profile(&self) -> Result<PlatformProfile, HardwareError> {
        PlatformProfile::from_sysfs(&self.current_profile_raw()?)
    }

    pub fn set_profile(&self, profile: PlatformProfile) -> Result<(), HardwareError> {
        self.set_profile_raw(profile.as_sysfs())
    }

    pub fn current_profile_raw(&self) -> Result<String, HardwareError> {
        let profile = self.profile.as_ref().ok_or_else(|| {
            HardwareError::MissingInterface(rooted(&self.root, "sys/class/platform-profile"))
        })?;
        read_trimmed(&profile.profile, "read platform profile")
    }

    pub fn set_profile_raw(&self, requested: &str) -> Result<(), HardwareError> {
        self.require_acer()?;
        if requested.is_empty()
            || requested.len() > 48
            || !requested
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(HardwareError::InvalidValue {
                field: "platform_profile",
                value: requested.to_string(),
            });
        }
        let profile = self.profile.as_ref().ok_or_else(|| {
            HardwareError::MissingInterface(rooted(&self.root, "sys/class/platform-profile"))
        })?;
        let choices = read_trimmed(&profile.choices, "read platform profile choices")?;
        if !choices
            .split_ascii_whitespace()
            .any(|choice| choice == requested)
        {
            return Err(PlatformProfile::from_sysfs(requested).map_or_else(
                |_| HardwareError::InvalidValue {
                    field: "platform_profile",
                    value: requested.to_string(),
                },
                HardwareError::ProfileUnavailable,
            ));
        }
        write_existing(&profile.profile, requested, "set platform profile")?;
        let actual = read_trimmed(&profile.profile, "verify platform profile")?;
        ensure_readback("platform_profile", requested, &actual)
    }

    pub fn profile_choices(&self) -> Result<Vec<ProfileChoice>, HardwareError> {
        let Some(profile) = &self.profile else {
            return Ok(Vec::new());
        };
        let choices = read_trimmed(&profile.choices, "read platform profile choices")?;
        Ok(choices
            .split_ascii_whitespace()
            .map(|raw| ProfileChoice {
                raw: raw.to_string(),
                label: profile_label(raw),
                selectable: self.platform.acer
                    && raw != "custom"
                    && (!self.is_reference_model() || PlatformProfile::from_sysfs(raw).is_ok()),
            })
            .collect())
    }

    pub fn capabilities(&self) -> HardwareCapabilities {
        let profile_choices = self.profile_choices().unwrap_or_default();
        let current = self.current_profile_raw().ok();
        let rpm_channels = self.fan_rpm_channels();
        let (backend, auto, manual, maximum) = match &self.fan_backend {
            Some(FanInterface::KernelPwm { .. }) => (Some(FanBackend::KernelPwm), true, true, true),
            Some(FanInterface::AcerGamingWmi { manual, .. }) => {
                (Some(FanBackend::AcerGamingWmi), true, *manual, true)
            }
            None => (None, false, false, false),
        };
        HardwareCapabilities {
            vendor: self.platform.vendor.clone(),
            product: self.platform.product.clone(),
            reference_model: self.is_reference_model(),
            profiles: ProfileCapabilities {
                backend: self.profile.as_ref().map(|profile| profile.backend),
                choices: profile_choices,
                current,
            },
            fans: FanCapabilities {
                backend,
                rpm_channels,
                auto,
                manual,
                maximum,
            },
        }
    }

    pub fn is_reference_model(&self) -> bool {
        self.platform.acer && self.platform.product == REFERENCE_PRODUCT
    }

    pub fn is_acer(&self) -> bool {
        self.platform.acer
    }

    pub fn product_name(&self) -> &str {
        &self.platform.product
    }

    pub fn apply_fan_setting(&self, setting: FanSetting) -> Result<FanState, HardwareError> {
        self.require_acer()?;
        if self.fan_backend.is_none() {
            return Err(HardwareError::MissingInterface(rooted(
                &self.root,
                "sys/class/hwmon/acer-fan-control",
            )));
        }
        if matches!(setting, FanSetting::Manual { .. })
            && matches!(
                &self.fan_backend,
                Some(FanInterface::AcerGamingWmi { manual: false, .. })
            )
        {
            return Err(HardwareError::MissingInterface(rooted(
                &self.root,
                "sys/bus/wmi/devices/gaming_fan/cpu_speed",
            )));
        }
        let mutation = match setting {
            FanSetting::Automatic => self.set_both_modes(FanMode::Automatic),
            FanSetting::Maximum => self.set_both_modes(FanMode::Maximum),
            FanSetting::Manual {
                cpu_percent,
                gpu_percent,
            } => self.set_manual(cpu_percent, gpu_percent),
        };

        match mutation.and_then(|()| self.read_fan_state()) {
            Ok(state) => Ok(state),
            Err(cause) => match self.force_automatic_unchecked() {
                Ok(()) => Err(HardwareError::MutationRolledBack {
                    cause: Box::new(cause),
                }),
                Err(automatic) => match self.force_maximum_unchecked() {
                    Ok(()) => Err(HardwareError::MutationFailsafeMaximum {
                        cause: Box::new(cause),
                        automatic: Box::new(automatic),
                    }),
                    Err(maximum) => Err(HardwareError::MutationRecoveryFailed {
                        cause: Box::new(cause),
                        automatic: Box::new(automatic),
                        maximum: Box::new(maximum),
                    }),
                },
            },
        }
    }

    pub fn read_fan_state(&self) -> Result<FanState, HardwareError> {
        Ok(FanState {
            cpu: self.read_fan_channel(1)?,
            gpu: self.read_fan_channel(2)?,
        })
    }

    pub(crate) fn read_acer_temp_millidegrees(&self, channel: u8) -> Result<i64, HardwareError> {
        let base = self
            .hwmon
            .as_ref()
            .ok_or(HardwareError::AcerHwmonNotFound)?;
        let path = acer_temperature_path(base, channel);
        parse_value(&path, "temperature")
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    fn read_fan_channel(&self, channel: u8) -> Result<FanChannelState, HardwareError> {
        let rpm = self.read_rpm(channel).unwrap_or(0);
        let (mode, pwm_raw) = match &self.fan_backend {
            Some(FanInterface::KernelPwm { base }) => {
                let mode = read_optional_fan_mode(&base.join(format!("pwm{channel}_enable")))?;
                let pwm: u32 = parse_value(&base.join(format!("pwm{channel}")), "PWM")?;
                let pwm_raw = u8::try_from(pwm).map_err(|_| HardwareError::InvalidValue {
                    field: "PWM",
                    value: pwm.to_string(),
                })?;
                (mode, pwm_raw)
            }
            Some(FanInterface::AcerGamingWmi { base, manual }) => {
                let role = fan_role(channel)?;
                let mode = FanMode::from_sysfs(&read_trimmed(
                    &base.join(format!("{role}_mode")),
                    "read Gaming-WMI fan mode",
                )?)?;
                let percent = if *manual {
                    let value: u8 =
                        parse_value(&base.join(format!("{role}_speed")), "Gaming-WMI fan speed")?;
                    if value > 100 {
                        return Err(HardwareError::InvalidValue {
                            field: "Gaming-WMI fan speed",
                            value: value.to_string(),
                        });
                    }
                    value
                } else {
                    0
                };
                (Some(mode), percent_to_pwm(percent))
            }
            None => (None, 0),
        };
        Ok(FanChannelState { mode, pwm_raw, rpm })
    }

    pub fn fan_rpm_channels(&self) -> Vec<FanRpmChannel> {
        let Some(hwmon) = &self.hwmon else {
            return Vec::new();
        };
        (1_u8..=8)
            .filter_map(|index| {
                let rpm_path = hwmon.join(format!("fan{index}_input"));
                if !rpm_path.is_file() {
                    return None;
                }
                let label_path = hwmon.join(format!("fan{index}_label"));
                let label = fs::read_to_string(label_path)
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| match index {
                        1 => "CPU".to_string(),
                        2 => "GPU".to_string(),
                        _ => format!("Fan {index}"),
                    });
                Some(FanRpmChannel {
                    index,
                    label,
                    rpm: self.read_rpm(index).ok(),
                })
            })
            .collect()
    }

    fn read_rpm(&self, channel: u8) -> Result<u32, HardwareError> {
        let path = self
            .hwmon
            .as_ref()
            .ok_or(HardwareError::AcerHwmonNotFound)?
            .join(format!("fan{channel}_input"));
        parse_value(&path, "fan RPM")
    }

    fn set_both_modes(&self, mode: FanMode) -> Result<(), HardwareError> {
        self.write_mode(1, mode)?;
        self.write_mode(2, mode)?;
        self.verify_mode(1, mode)?;
        self.verify_mode(2, mode)
    }

    fn set_manual(&self, cpu_percent: u8, gpu_percent: u8) -> Result<(), HardwareError> {
        validate_percent("CPU fan percent", cpu_percent)?;
        validate_percent("GPU fan percent", gpu_percent)?;
        // Firmware-safe sequence: remove firmware automation by taking both
        // fans to maximum first, set both target PWM values, and only then
        // expose both channels as manually controlled.  Any failure is caught
        // by apply_fan_setting and rolls both channels back to Automatic.
        self.write_mode(1, FanMode::Maximum)?;
        self.write_mode(2, FanMode::Maximum)?;
        self.verify_mode(1, FanMode::Maximum)?;
        self.verify_mode(2, FanMode::Maximum)?;
        self.write_speed_percent(1, cpu_percent)?;
        self.write_speed_percent(2, gpu_percent)?;
        self.write_mode(1, FanMode::Manual)?;
        self.write_mode(2, FanMode::Manual)?;
        // The PHN16-72 firmware caches a requested value while Maximum is
        // active. Re-apply it after both channels enter Manual so the final
        // transaction is explicit and symmetric.
        self.write_speed_percent(1, cpu_percent)?;
        self.write_speed_percent(2, gpu_percent)?;
        self.verify_pwm_percent(1, cpu_percent)?;
        self.verify_pwm_percent(2, gpu_percent)?;
        self.verify_mode(1, FanMode::Manual)?;
        self.verify_mode(2, FanMode::Manual)
    }

    fn force_automatic_unchecked(&self) -> Result<(), HardwareError> {
        self.force_mode_unchecked(FanMode::Automatic)
    }

    fn force_maximum_unchecked(&self) -> Result<(), HardwareError> {
        self.force_mode_unchecked(FanMode::Maximum)
    }

    fn force_mode_unchecked(&self, mode: FanMode) -> Result<(), HardwareError> {
        // Attempt both writes even when the first one fails, so a partial
        // mutation has the best possible chance of reaching one safe,
        // symmetric firmware state. Preserve the first error for reporting.
        let first = self.write_mode(1, mode).err();
        let second = self.write_mode(2, mode).err();
        if let Some(error) = first.or(second) {
            return Err(error);
        }
        self.verify_mode(1, mode)?;
        self.verify_mode(2, mode)
    }

    fn write_mode(&self, channel: u8, mode: FanMode) -> Result<(), HardwareError> {
        write_existing(&self.mode_path(channel)?, mode.as_sysfs(), "set fan mode")
    }

    fn verify_mode(&self, channel: u8, expected: FanMode) -> Result<(), HardwareError> {
        let path = self.mode_path(channel)?;
        let actual = read_trimmed(&path, "verify fan mode")?;
        ensure_readback("pwm_enable", expected.as_sysfs(), &actual)
    }

    fn write_speed_percent(&self, channel: u8, percent: u8) -> Result<(), HardwareError> {
        let (path, value) = match &self.fan_backend {
            Some(FanInterface::KernelPwm { base }) => (
                base.join(format!("pwm{channel}")),
                percent_to_pwm(percent).to_string(),
            ),
            Some(FanInterface::AcerGamingWmi { base, manual: true }) => (
                base.join(format!("{}_speed", fan_role(channel)?)),
                percent.to_string(),
            ),
            _ => {
                return Err(HardwareError::MissingInterface(rooted(
                    &self.root,
                    "sys/class/hwmon/fan-speed",
                )));
            }
        };
        write_existing(&path, &value, "set fan speed")
    }

    fn verify_pwm_percent(&self, channel: u8, expected_percent: u8) -> Result<(), HardwareError> {
        let path = match &self.fan_backend {
            Some(FanInterface::KernelPwm { base }) => base.join(format!("pwm{channel}")),
            Some(FanInterface::AcerGamingWmi { base, manual: true }) => {
                base.join(format!("{}_speed", fan_role(channel)?))
            }
            _ => {
                return Err(HardwareError::MissingInterface(rooted(
                    &self.root,
                    "sys/class/hwmon/fan-speed",
                )));
            }
        };
        let actual = read_trimmed(&path, "verify fan PWM")?;
        let actual_raw = actual
            .parse::<u8>()
            .map_err(|_| HardwareError::InvalidValue {
                field: "PWM",
                value: actual.clone(),
            })?;
        // acer_wmi converts 0..255 to firmware percent and back, so raw-byte
        // equality is not stable. Verify the user-visible percentage instead.
        let actual_percent = match &self.fan_backend {
            Some(FanInterface::KernelPwm { .. }) => pwm_to_percent(actual_raw),
            Some(FanInterface::AcerGamingWmi { .. }) => actual_raw,
            None => 0,
        };
        if actual_percent == expected_percent {
            Ok(())
        } else {
            Err(HardwareError::ReadbackMismatch {
                field: "PWM",
                expected: format!("{expected_percent}%"),
                actual: format!("{actual_percent}% (raw {actual})"),
            })
        }
    }

    fn mode_path(&self, channel: u8) -> Result<PathBuf, HardwareError> {
        match &self.fan_backend {
            Some(FanInterface::KernelPwm { base }) => Ok(base.join(format!("pwm{channel}_enable"))),
            Some(FanInterface::AcerGamingWmi { base, .. }) => {
                Ok(base.join(format!("{}_mode", fan_role(channel)?)))
            }
            None => Err(HardwareError::MissingInterface(rooted(
                &self.root,
                "sys/class/hwmon/fan-mode",
            ))),
        }
    }

    fn require_acer(&self) -> Result<(), HardwareError> {
        if self.platform.acer {
            Ok(())
        } else {
            Err(HardwareError::UnsupportedPlatform {
                vendor: self.platform.vendor.clone(),
                product: self.platform.product.clone(),
            })
        }
    }
}

pub(crate) fn discover_acer_hwmon(root: &Path) -> Option<PathBuf> {
    let hwmon_root = rooted(root, "sys/class/hwmon");
    let entries = fs::read_dir(&hwmon_root).ok()?;

    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(name) = fs::read_to_string(path.join("name")) else {
            continue;
        };
        if !is_acer_hwmon_identity(&name) {
            continue;
        }
        let driver = hwmon_driver_identity(&path);
        if driver
            .as_deref()
            .is_some_and(|identity| !is_acer_hwmon_identity(identity))
        {
            continue;
        }
        let score = (1_u8..=8)
            .flat_map(|index| {
                [
                    format!("fan{index}_input"),
                    format!("temp{index}_input"),
                    format!("pwm{index}"),
                    format!("pwm{index}_enable"),
                ]
            })
            .filter(|name| path.join(name).is_file())
            .count();
        candidates.push((driver.is_some(), score, path));
    }

    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let (best_driver, best_score, best_path) = candidates.first()?;
    if candidates
        .get(1)
        .is_some_and(|(driver, score, _)| driver == best_driver && score == best_score)
    {
        // An arbitrary writer would be worse than exposing profiles and RPM
        // without fan control.  A future fixture/quirk can make the selection
        // unambiguous without globally rejecting the machine.
        return None;
    }
    Some(best_path.clone())
}

fn is_acer_hwmon_identity(identity: &str) -> bool {
    matches!(
        identity
            .trim()
            .to_ascii_lowercase()
            .replace('_', "-")
            .as_str(),
        "acer" | "acer-wmi"
    )
}

fn hwmon_driver_identity(hwmon: &Path) -> Option<String> {
    let driver = hwmon.join("device/driver");
    fs::read_link(&driver)
        .ok()
        .or_else(|| fs::read_link(driver.join("module")).ok())
        .and_then(|target| {
            target
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
}

fn discover_kernel_profile_interface(root: &Path) -> Option<ProfileInterface> {
    let class_root = rooted(root, "sys/class/platform-profile");
    if let Ok(entries) = fs::read_dir(&class_root) {
        let mut candidates = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let profile = path.join("profile");
            let choices = path.join("choices");
            if !profile.is_file() || !choices.is_file() {
                continue;
            }
            let name = fs::read_to_string(path.join("name"))
                .unwrap_or_else(|_| entry.file_name().to_string_lossy().into_owned());
            if name.to_ascii_lowercase().contains("acer") {
                candidates.push(ProfileInterface {
                    backend: ProfileBackend::Kernel,
                    profile,
                    choices,
                });
            }
        }
        candidates.sort_by(|left, right| left.profile.cmp(&right.profile));
        if let Some(profile) = candidates.into_iter().next() {
            return Some(profile);
        }
    }

    let legacy_profile = rooted(root, "sys/firmware/acpi/platform_profile");
    let legacy_choices = rooted(root, "sys/firmware/acpi/platform_profile_choices");
    if legacy_profile.is_file() && legacy_choices.is_file() {
        return Some(ProfileInterface {
            backend: ProfileBackend::Kernel,
            profile: legacy_profile,
            choices: legacy_choices,
        });
    }

    None
}

fn discover_gaming_wmi_profile_interface(root: &Path) -> Option<ProfileInterface> {
    let gaming = find_wmi_group(
        &rooted(root, "sys/bus/wmi/devices"),
        GAMING_WMI_GUID,
        "gaming_profile",
    );
    let gaming = gaming?;
    let profile = gaming.join("profile");
    let choices = gaming.join("choices");
    if profile.is_file() && choices.is_file() {
        return Some(ProfileInterface {
            backend: ProfileBackend::AcerGamingWmi,
            profile,
            choices,
        });
    }
    None
}

fn discover_kernel_fan_interface(hwmon: Option<&Path>) -> Option<FanInterface> {
    let base = hwmon?;
    let complete = [
        "fan1_input",
        "fan2_input",
        "pwm1",
        "pwm2",
        "pwm1_enable",
        "pwm2_enable",
    ]
    .into_iter()
    .all(|name| base.join(name).is_file())
        && (1_u8..=2).all(|channel| acer_temperature_path(base, channel).is_file());
    let modes_are_readable = (1_u8..=2).all(|channel| {
        read_trimmed(&base.join(format!("pwm{channel}_enable")), "probe fan mode")
            .and_then(|value| FanMode::from_sysfs(&value))
            .is_ok()
    });
    if complete && modes_are_readable {
        return Some(FanInterface::KernelPwm {
            base: base.to_path_buf(),
        });
    }
    None
}

fn discover_gaming_wmi_fan_interface(root: &Path, hwmon: Option<&Path>) -> Option<FanInterface> {
    let base = find_wmi_group(
        &rooted(root, "sys/bus/wmi/devices"),
        GAMING_WMI_GUID,
        "gaming_fan",
    )?;
    if !base.join("cpu_mode").is_file() || !base.join("gpu_mode").is_file() {
        return None;
    }
    let behavior_is_readable = ["cpu_mode", "gpu_mode"].into_iter().all(|name| {
        read_trimmed(&base.join(name), "probe Gaming-WMI fan mode")
            .and_then(|value| FanMode::from_sysfs(&value))
            .is_ok()
    });
    if !behavior_is_readable {
        return None;
    }
    let temperatures_are_readable = hwmon.is_some_and(|base| {
        (1_u8..=2).all(|channel| {
            parse_value::<i64>(&acer_temperature_path(base, channel), "temperature").is_ok()
        })
    });
    let manual = temperatures_are_readable
        && ["cpu_speed", "gpu_speed"].into_iter().all(|name| {
            parse_value::<u8>(&base.join(name), "Gaming-WMI fan speed")
                .is_ok_and(|value| value <= 100)
        });
    Some(FanInterface::AcerGamingWmi { base, manual })
}

fn acer_temperature_path(base: &Path, channel: u8) -> PathBuf {
    let role = match channel {
        1 => "cpu",
        2 => "gpu",
        _ => return base.join(format!("temp{channel}_input")),
    };
    for index in 1_u8..=8 {
        let label = base.join(format!("temp{index}_label"));
        let Ok(label) = fs::read_to_string(label) else {
            continue;
        };
        let input = base.join(format!("temp{index}_input"));
        if input.is_file() && label.trim().to_ascii_lowercase().contains(role) {
            return input;
        }
    }
    base.join(format!("temp{channel}_input"))
}

fn profile_label(raw: &str) -> String {
    match raw {
        "low-power" => "Low power".to_string(),
        "cool" => "Cool".to_string(),
        "quiet" => "Quiet".to_string(),
        "balanced" => "Balanced".to_string(),
        "balanced-performance" => "Balanced performance".to_string(),
        "performance" => "Performance".to_string(),
        "custom" => "Custom".to_string(),
        _ => raw
            .split(['-', '_'])
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn fan_role(channel: u8) -> Result<&'static str, HardwareError> {
    match channel {
        1 => Ok("cpu"),
        2 => Ok("gpu"),
        _ => Err(HardwareError::InvalidValue {
            field: "fan channel",
            value: channel.to_string(),
        }),
    }
}

fn read_optional_fan_mode(path: &Path) -> Result<Option<FanMode>, HardwareError> {
    match read_trimmed(path, "read fan mode") {
        Ok(value) => FanMode::from_sysfs(&value).map(Some),
        Err(error) if is_unavailable_fan_mode(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_unavailable_fan_mode(error: &HardwareError) -> bool {
    matches!(
        error,
        HardwareError::Io { source, .. }
            if source.raw_os_error() == Some(libc::ENXIO)
    )
}

fn rooted(root: &Path, relative: &str) -> PathBuf {
    root.join(relative.trim_start_matches('/'))
}

fn validate_percent(field: &'static str, value: u8) -> Result<(), HardwareError> {
    if (20..=100).contains(&value) {
        Ok(())
    } else {
        Err(HardwareError::InvalidValue {
            field,
            value: value.to_string(),
        })
    }
}

pub fn percent_to_pwm(percent: u8) -> u8 {
    debug_assert!(percent <= 100);
    // Round upward so the kernel's integer PWM -> percent conversion cannot
    // silently select the preceding percentage.
    (u32::from(percent) * PWM_MAX).div_ceil(100) as u8
}

pub fn pwm_to_percent(pwm: u8) -> u8 {
    ((u32::from(pwm) * 100 + (PWM_MAX / 2)) / PWM_MAX) as u8
}

fn ensure_readback(field: &'static str, expected: &str, actual: &str) -> Result<(), HardwareError> {
    if actual.trim() == expected {
        Ok(())
    } else {
        Err(HardwareError::ReadbackMismatch {
            field,
            expected: expected.to_owned(),
            actual: actual.trim().to_owned(),
        })
    }
}

fn read_trimmed(path: &Path, operation: &'static str) -> Result<String, HardwareError> {
    fs::read_to_string(path)
        .map(|value| value.trim().to_owned())
        .map_err(|source| HardwareError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })
}

fn parse_value<T>(path: &Path, field: &'static str) -> Result<T, HardwareError>
where
    T: std::str::FromStr,
{
    let value = read_trimmed(path, "read numeric firmware value")?;
    value
        .parse()
        .map_err(|_| HardwareError::InvalidValue { field, value })
}

fn write_existing(path: &Path, value: &str, operation: &'static str) -> Result<(), HardwareError> {
    // Intentionally no `create(true)`: a misspelled or unavailable sysfs
    // attribute must fail closed rather than creating a normal root-owned file.
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|source| HardwareError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(value.as_bytes())
        .map_err(|source| HardwareError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("asense-hardware-{}-{id}", std::process::id()));
            let dmi = root.join("sys/class/dmi/id");
            let hwmon = root.join("sys/class/hwmon/hwmon37");
            let acpi = root.join("sys/firmware/acpi");
            fs::create_dir_all(&dmi).unwrap();
            fs::create_dir_all(&hwmon).unwrap();
            fs::create_dir_all(&acpi).unwrap();
            fs::write(dmi.join("sys_vendor"), "Acer\n").unwrap();
            fs::write(dmi.join("product_name"), "Predator PHN16-72\n").unwrap();
            fs::write(dmi.join("bios_version"), "V1.18\n").unwrap();
            fs::write(hwmon.join("name"), "acer\n").unwrap();
            fs::write(hwmon.join("fan1_input"), "3100\n").unwrap();
            fs::write(hwmon.join("fan2_input"), "2800\n").unwrap();
            fs::write(hwmon.join("pwm1"), "0\n").unwrap();
            fs::write(hwmon.join("pwm2"), "0\n").unwrap();
            fs::write(hwmon.join("pwm1_enable"), "2\n").unwrap();
            fs::write(hwmon.join("pwm2_enable"), "2\n").unwrap();
            fs::write(hwmon.join("temp1_input"), "65000\n").unwrap();
            fs::write(hwmon.join("temp2_input"), "55000\n").unwrap();
            fs::write(acpi.join("platform_profile"), "balanced\n").unwrap();
            fs::write(
                acpi.join("platform_profile_choices"),
                "low-power quiet balanced balanced-performance performance\n",
            )
            .unwrap();
            Self { root }
        }

        fn hwmon(&self) -> PathBuf {
            self.root.join("sys/class/hwmon/hwmon37")
        }

        fn gaming_root(&self) -> PathBuf {
            self.root.join("sys/bus/wmi/devices").join(GAMING_WMI_GUID)
        }

        fn gaming_fan(&self, with_speed: bool) -> PathBuf {
            let base = self.gaming_root().join("gaming_fan");
            fs::create_dir_all(&base).unwrap();
            fs::write(base.join("cpu_mode"), "2\n").unwrap();
            fs::write(base.join("gpu_mode"), "2\n").unwrap();
            if with_speed {
                fs::write(base.join("cpu_speed"), "50\n").unwrap();
                fs::write(base.join("gpu_speed"), "50\n").unwrap();
            }
            base
        }

        fn gaming_profile(&self) -> PathBuf {
            let base = self.gaming_root().join("gaming_profile");
            fs::create_dir_all(&base).unwrap();
            fs::write(base.join("profile"), "balanced\n").unwrap();
            fs::write(
                base.join("choices"),
                "low-power quiet balanced balanced-performance performance\n",
            )
            .unwrap();
            base
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn discovers_acer_by_name_not_hwmon_number() {
        let fixture = Fixture::new();
        let other = fixture.root.join("sys/class/hwmon/hwmon8");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("name"), "coretemp\n").unwrap();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert!(
            hardware
                .hwmon
                .as_deref()
                .is_some_and(|path| path.ends_with("hwmon37"))
        );
        assert_eq!(
            hardware.current_profile().unwrap(),
            PlatformProfile::Balanced
        );
    }

    #[test]
    fn broken_hwmon_entry_keeps_profile_and_gaming_wmi_available() {
        let fixture = Fixture::new();
        let name = fixture.hwmon().join("name");
        fs::remove_file(&name).unwrap();
        fs::create_dir(&name).unwrap();
        fixture.gaming_fan(true);

        let capabilities = AcerHardware::discover_at(&fixture.root)
            .unwrap()
            .capabilities();
        assert_eq!(capabilities.profiles.backend, Some(ProfileBackend::Kernel));
        assert_eq!(capabilities.fans.backend, Some(FanBackend::AcerGamingWmi));
        assert!(capabilities.fans.rpm_channels.is_empty());
    }

    #[test]
    fn broken_kernel_profile_enumeration_keeps_hwmon_and_wmi_profile_available() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.root.join("sys/firmware/acpi/platform_profile")).unwrap();
        fs::remove_file(
            fixture
                .root
                .join("sys/firmware/acpi/platform_profile_choices"),
        )
        .unwrap();
        fs::write(
            fixture.root.join("sys/class/platform-profile"),
            "not a directory\n",
        )
        .unwrap();
        fixture.gaming_profile();

        let capabilities = AcerHardware::discover_at(&fixture.root)
            .unwrap()
            .capabilities();
        assert_eq!(
            capabilities.profiles.backend,
            Some(ProfileBackend::AcerGamingWmi)
        );
        assert_eq!(capabilities.fans.backend, Some(FanBackend::KernelPwm));
    }

    #[test]
    fn hwmon_driver_identity_rejects_a_foreign_name_collision() {
        let fixture = Fixture::new();
        let other = fixture.root.join("sys/class/hwmon/hwmon8");
        fs::create_dir_all(other.join("device")).unwrap();
        fs::write(other.join("name"), "acer\n").unwrap();
        for index in 1_u8..=8 {
            for attribute in [
                format!("fan{index}_input"),
                format!("temp{index}_input"),
                format!("pwm{index}"),
                format!("pwm{index}_enable"),
            ] {
                fs::write(other.join(attribute), "1\n").unwrap();
            }
        }
        let foreign_driver = fixture.root.join("sys/bus/platform/drivers/not-acer");
        fs::create_dir_all(&foreign_driver).unwrap();
        std::os::unix::fs::symlink(&foreign_driver, other.join("device/driver")).unwrap();

        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert!(
            hardware
                .hwmon
                .as_deref()
                .is_some_and(|path| path.ends_with("hwmon37"))
        );
    }

    #[test]
    fn accepts_a_different_acer_product_without_disabling_discovery() {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("sys/class/dmi/id/product_name"),
            "Predator PHN16-71\n",
        )
        .unwrap();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert_eq!(hardware.product_name(), "Predator PHN16-71");
        assert!(!hardware.is_reference_model());
    }

    #[test]
    fn non_acer_is_read_only_instead_of_failing_generic_discovery() {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("sys/class/dmi/id/sys_vendor"),
            "Other Vendor\n",
        )
        .unwrap();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert!(!hardware.is_acer());
        assert!(hardware.capabilities().fans.backend.is_none());
        assert!(
            hardware
                .profile_choices()
                .unwrap()
                .iter()
                .all(|choice| !choice.selectable)
        );
        assert!(hardware.set_profile_raw("balanced").is_err());
        assert!(hardware.apply_fan_setting(FanSetting::Automatic).is_err());
    }

    #[test]
    fn discovery_is_bios_revision_agnostic() {
        let fixture = Fixture::new();
        let bios_path = fixture.root.join("sys/class/dmi/id/bios_version");
        fs::remove_file(&bios_path).unwrap();
        AcerHardware::discover_at(&fixture.root).unwrap();
        fs::write(bios_path, "V1.19\n").unwrap();
        AcerHardware::discover_at(&fixture.root).unwrap();
    }

    #[test]
    fn profile_only_acer_remains_usable_without_hwmon() {
        let fixture = Fixture::new();
        fs::remove_dir_all(fixture.root.join("sys/class/hwmon")).unwrap();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        let capabilities = hardware.capabilities();
        assert_eq!(capabilities.profiles.backend, Some(ProfileBackend::Kernel));
        assert!(capabilities.fans.backend.is_none());
        hardware.set_profile_raw("quiet").unwrap();
        assert_eq!(hardware.current_profile_raw().unwrap(), "quiet");
    }

    #[test]
    fn acer_class_profile_handler_precedes_the_legacy_global_interface() {
        let fixture = Fixture::new();
        let handler = fixture
            .root
            .join("sys/class/platform-profile/acer-wmi-profile");
        fs::create_dir_all(&handler).unwrap();
        fs::write(handler.join("name"), "acer-wmi\n").unwrap();
        fs::write(handler.join("profile"), "cool\n").unwrap();
        fs::write(
            handler.join("choices"),
            "cool balanced performance custom\n",
        )
        .unwrap();

        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert_eq!(hardware.current_profile_raw().unwrap(), "cool");
        let choices = hardware.profile_choices().unwrap();
        assert_eq!(choices[0].label, "Cool");
        assert!(!choices.last().unwrap().selectable);
        hardware.set_profile_raw("performance").unwrap();
        assert_eq!(
            fs::read_to_string(handler.join("profile")).unwrap(),
            "performance"
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("sys/firmware/acpi/platform_profile"))
                .unwrap()
                .trim(),
            "balanced"
        );
    }

    #[test]
    fn generic_acer_unknown_live_profile_token_is_selectable_and_bounded() {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("sys/class/dmi/id/product_name"),
            "Predator PH18-99\n",
        )
        .unwrap();
        fs::write(
            fixture
                .root
                .join("sys/firmware/acpi/platform_profile_choices"),
            "balanced ultra-cool custom\n",
        )
        .unwrap();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        let choices = hardware.profile_choices().unwrap();
        assert_eq!(choices[1].label, "Ultra Cool");
        assert!(choices[1].selectable);
        hardware.set_profile_raw("ultra-cool").unwrap();
        assert!(hardware.set_profile_raw("not-advertised").is_err());
        assert!(hardware.set_profile_raw("bad token").is_err());
    }

    #[test]
    fn reference_model_only_selects_profiles_known_by_its_profile_transaction() {
        let fixture = Fixture::new();
        fs::write(
            fixture
                .root
                .join("sys/firmware/acpi/platform_profile_choices"),
            "balanced ultra-cool custom\n",
        )
        .unwrap();
        let choices = AcerHardware::discover_at(&fixture.root)
            .unwrap()
            .profile_choices()
            .unwrap();
        assert!(choices[0].selectable);
        assert!(!choices[1].selectable);
        assert!(!choices[2].selectable);
    }

    #[test]
    fn gaming_profile_is_a_fallback_only_when_kernel_profile_is_absent() {
        let fixture = Fixture::new();
        let gaming = fixture.gaming_profile();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert_eq!(
            hardware.capabilities().profiles.backend,
            Some(ProfileBackend::Kernel)
        );

        fs::remove_file(fixture.root.join("sys/firmware/acpi/platform_profile")).unwrap();
        fs::remove_file(
            fixture
                .root
                .join("sys/firmware/acpi/platform_profile_choices"),
        )
        .unwrap();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert_eq!(
            hardware.capabilities().profiles.backend,
            Some(ProfileBackend::AcerGamingWmi)
        );
        hardware.set_profile_raw("performance").unwrap();
        assert_eq!(
            fs::read_to_string(gaming.join("profile")).unwrap(),
            "performance"
        );
    }

    #[test]
    fn discovers_additional_rpm_channels_without_claiming_control() {
        let fixture = Fixture::new();
        fs::write(fixture.hwmon().join("fan3_input"), "1700\n").unwrap();
        fs::write(fixture.hwmon().join("fan3_label"), "System\n").unwrap();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        let fans = hardware.capabilities().fans;
        assert_eq!(fans.rpm_channels.len(), 3);
        assert_eq!(fans.rpm_channels[2].label, "System");
        assert_eq!(fans.rpm_channels[2].rpm, Some(1700));
        assert_eq!(fans.backend, Some(FanBackend::KernelPwm));
    }

    #[test]
    fn temperature_roles_follow_hwmon_labels_instead_of_file_order() {
        let fixture = Fixture::new();
        fs::write(fixture.hwmon().join("temp1_label"), "GPU\n").unwrap();
        fs::write(fixture.hwmon().join("temp2_label"), "CPU Package\n").unwrap();

        assert_eq!(
            acer_temperature_path(&fixture.hwmon(), 1),
            fixture.hwmon().join("temp2_input")
        );
        assert_eq!(
            acer_temperature_path(&fixture.hwmon(), 2),
            fixture.hwmon().join("temp1_input")
        );
    }

    #[test]
    fn incomplete_kernel_pwm_falls_back_to_full_gaming_wmi() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.hwmon().join("pwm2_enable")).unwrap();
        fixture.gaming_fan(true);
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert_eq!(
            hardware.capabilities().fans.backend,
            Some(FanBackend::AcerGamingWmi)
        );
        let state = hardware
            .apply_fan_setting(FanSetting::Manual {
                cpu_percent: 60,
                gpu_percent: 40,
            })
            .unwrap();
        assert_eq!(state.cpu.mode, Some(FanMode::Manual));
        assert_eq!(pwm_to_percent(state.cpu.pwm_raw), 60);
        assert_eq!(pwm_to_percent(state.gpu.pwm_raw), 40);
    }

    #[test]
    fn invalid_kernel_mode_readback_falls_through_to_wmi_or_read_only() {
        let fixture = Fixture::new();
        fs::write(fixture.hwmon().join("pwm2_enable"), "invalid\n").unwrap();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert_eq!(hardware.capabilities().fans.backend, None);

        fixture.gaming_fan(true);
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert_eq!(
            hardware.capabilities().fans.backend,
            Some(FanBackend::AcerGamingWmi)
        );
    }

    #[test]
    fn gaming_wmi_manual_requires_both_watchdog_temperatures() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.hwmon().join("pwm2_enable")).unwrap();
        fs::remove_file(fixture.hwmon().join("temp2_input")).unwrap();
        fixture.gaming_fan(true);
        let fans = AcerHardware::discover_at(&fixture.root)
            .unwrap()
            .capabilities()
            .fans;
        assert_eq!(fans.backend, Some(FanBackend::AcerGamingWmi));
        assert!(fans.auto && fans.maximum);
        assert!(!fans.manual);
    }

    #[test]
    fn gaming_wmi_lookup_accepts_case_insensitive_numbered_device_names() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.root.join("sys/firmware/acpi/platform_profile")).unwrap();
        fs::remove_file(
            fixture
                .root
                .join("sys/firmware/acpi/platform_profile_choices"),
        )
        .unwrap();
        fs::remove_file(fixture.hwmon().join("pwm2_enable")).unwrap();

        let gaming = fixture
            .root
            .join("sys/bus/wmi/devices")
            .join(format!("{}-00", GAMING_WMI_GUID.to_ascii_lowercase()));
        let profile = gaming.join("gaming_profile");
        let fan = gaming.join("gaming_fan");
        fs::create_dir_all(&profile).unwrap();
        fs::create_dir_all(&fan).unwrap();
        fs::write(profile.join("profile"), "balanced\n").unwrap();
        fs::write(profile.join("choices"), "balanced performance\n").unwrap();
        fs::write(fan.join("cpu_mode"), "2\n").unwrap();
        fs::write(fan.join("gpu_mode"), "2\n").unwrap();
        fs::write(fan.join("cpu_speed"), "50\n").unwrap();
        fs::write(fan.join("gpu_speed"), "50\n").unwrap();

        let capabilities = AcerHardware::discover_at(&fixture.root)
            .unwrap()
            .capabilities();
        assert_eq!(
            capabilities.profiles.backend,
            Some(ProfileBackend::AcerGamingWmi)
        );
        assert_eq!(capabilities.fans.backend, Some(FanBackend::AcerGamingWmi));
        assert!(capabilities.fans.manual);
    }

    #[test]
    fn gaming_behavior_without_speed_support_exposes_auto_and_maximum_only() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.hwmon().join("pwm2_enable")).unwrap();
        fixture.gaming_fan(false);
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        let fans = hardware.capabilities().fans;
        assert_eq!(fans.backend, Some(FanBackend::AcerGamingWmi));
        assert!(fans.auto && fans.maximum);
        assert!(!fans.manual);
        hardware.apply_fan_setting(FanSetting::Maximum).unwrap();
        assert!(
            hardware
                .apply_fan_setting(FanSetting::Manual {
                    cpu_percent: 50,
                    gpu_percent: 50,
                })
                .is_err()
        );
    }

    #[test]
    fn platform_profiles_map_to_acer_firmware_modes() {
        assert_eq!(PlatformProfile::Eco.as_sysfs(), "low-power");
        assert_eq!(
            PlatformProfile::Performance.as_sysfs(),
            "balanced-performance"
        );
        assert_eq!(PlatformProfile::Turbo.as_sysfs(), "performance");
        let fixture = Fixture::new();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        hardware.set_profile(PlatformProfile::Turbo).unwrap();
        assert_eq!(hardware.current_profile().unwrap(), PlatformProfile::Turbo);
    }

    #[test]
    fn manual_setting_uses_both_channels_and_verified_pwm() {
        let fixture = Fixture::new();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        let state = hardware
            .apply_fan_setting(FanSetting::Manual {
                cpu_percent: 60,
                gpu_percent: 40,
            })
            .unwrap();
        assert_eq!(state.cpu.mode, Some(FanMode::Manual));
        assert_eq!(state.gpu.mode, Some(FanMode::Manual));
        assert_eq!(state.cpu.pwm_raw, 153);
        assert_eq!(state.gpu.pwm_raw, 102);
    }

    #[test]
    fn firmware_pwm_quantisation_is_verified_in_percent_space() {
        let fixture = Fixture::new();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        fs::write(fixture.hwmon().join("pwm1"), "127\n").unwrap();
        assert!(hardware.verify_pwm_percent(1, 50).is_ok());
        fs::write(fixture.hwmon().join("pwm1"), "124\n").unwrap();
        assert!(matches!(
            hardware.verify_pwm_percent(1, 50),
            Err(HardwareError::ReadbackMismatch { .. })
        ));
    }

    #[test]
    fn any_manual_failure_rolls_both_channels_back_to_auto() {
        let fixture = Fixture::new();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        fs::remove_file(fixture.hwmon().join("pwm2")).unwrap();
        let error = hardware
            .apply_fan_setting(FanSetting::Manual {
                cpu_percent: 70,
                gpu_percent: 70,
            })
            .unwrap_err();
        assert!(matches!(error, HardwareError::MutationRolledBack { .. }));
        assert_eq!(
            fs::read_to_string(fixture.hwmon().join("pwm1_enable"))
                .unwrap()
                .trim(),
            "2"
        );
        assert_eq!(
            fs::read_to_string(fixture.hwmon().join("pwm2_enable"))
                .unwrap()
                .trim(),
            "2"
        );
    }

    #[test]
    fn manual_percentages_are_bounded_and_leave_a_known_mode() {
        for cpu_percent in [0, 19, 101, u8::MAX] {
            let fixture = Fixture::new();
            let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
            assert!(
                hardware
                    .apply_fan_setting(FanSetting::Manual {
                        cpu_percent,
                        gpu_percent: 50
                    })
                    .is_err()
            );
            let state = hardware.read_fan_state().unwrap();
            assert_eq!(
                (state.cpu.mode, state.gpu.mode),
                (Some(FanMode::Automatic), Some(FanMode::Automatic))
            );
        }
        for (cpu_percent, gpu_percent) in [(20, 100), (100, 20)] {
            let fixture = Fixture::new();
            let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
            let state = hardware
                .apply_fan_setting(FanSetting::Manual {
                    cpu_percent,
                    gpu_percent,
                })
                .unwrap();
            assert_eq!(
                (state.cpu.mode, state.gpu.mode),
                (Some(FanMode::Manual), Some(FanMode::Manual))
            );
            assert_eq!(
                (state.cpu.pwm_raw, state.gpu.pwm_raw),
                (percent_to_pwm(cpu_percent), percent_to_pwm(gpu_percent))
            );
        }
    }

    #[test]
    fn pwm_scaling_is_rounded_and_bounded() {
        assert_eq!(percent_to_pwm(0), 0);
        assert_eq!(percent_to_pwm(40), 102);
        assert_eq!(percent_to_pwm(50), 128);
        assert_eq!(percent_to_pwm(100), 255);
        for percent in 20..=100 {
            let encoded = percent_to_pwm(percent);
            assert_eq!(pwm_to_percent(encoded), percent);
            assert_eq!(pwm_to_percent(encoded.saturating_sub(1)), percent);
        }
    }

    #[test]
    fn only_enxio_makes_fan_mode_unavailable() {
        let error = |code| HardwareError::Io {
            operation: "read fan mode",
            path: PathBuf::from("/sys/class/hwmon/hwmon8/pwm1_enable"),
            source: std::io::Error::from_raw_os_error(code),
        };

        assert!(is_unavailable_fan_mode(&error(libc::ENXIO)));
        assert!(!is_unavailable_fan_mode(&error(libc::EAGAIN)));
        assert!(!is_unavailable_fan_mode(&error(libc::ENOENT)));
        assert!(!is_unavailable_fan_mode(&error(libc::EACCES)));
    }

    #[test]
    fn rpm_disappearance_does_not_turn_a_successful_control_readback_into_failure() {
        let fixture = Fixture::new();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        fs::remove_file(fixture.hwmon().join("fan1_input")).unwrap();
        let state = hardware.apply_fan_setting(FanSetting::Maximum).unwrap();
        assert_eq!(state.cpu.mode, Some(FanMode::Maximum));
        assert_eq!(state.cpu.rpm, 0);
        assert_eq!(
            fs::read_to_string(fixture.hwmon().join("pwm1_enable"))
                .unwrap()
                .trim(),
            "0"
        );
        assert_eq!(
            fs::read_to_string(fixture.hwmon().join("pwm2_enable"))
                .unwrap()
                .trim(),
            "0"
        );
    }
}
