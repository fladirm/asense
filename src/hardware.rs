//! Fail-closed access to the Acer PHN16-72 firmware controls exposed by
//! `acer_wmi` through sysfs.
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

const EXPECTED_VENDOR: &str = "Acer";
const EXPECTED_PRODUCT: &str = "Predator PHN16-72";
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
    AmbiguousAcerHwmon(Vec<PathBuf>),
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
    MutationRollbackFailed {
        cause: Box<HardwareError>,
        rollback: Box<HardwareError>,
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
                "unsupported platform (vendor={vendor:?}, product={product:?}); expected vendor={EXPECTED_VENDOR:?}, product={EXPECTED_PRODUCT:?}"
            ),
            Self::AcerHwmonNotFound => write!(f, "acer hwmon interface was not found"),
            Self::AmbiguousAcerHwmon(paths) => {
                write!(f, "multiple acer hwmon interfaces found: {paths:?}")
            }
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
            Self::MutationRollbackFailed { cause, rollback } => write!(
                f,
                "fan mutation failed ({cause}) and rollback to automatic mode also failed ({rollback})"
            ),
        }
    }
}

impl Error for HardwareError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::MutationRolledBack { cause } => Some(cause),
            Self::MutationRollbackFailed { cause, .. } => Some(cause),
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
    _platform: VerifiedPlatform,
    root: PathBuf,
    hwmon: PathBuf,
    platform_profile: PathBuf,
    platform_profile_choices: PathBuf,
}

/// Proof that all write-capable `acer_wmi` interfaces belong to the exact
/// PHN16-72 model family reviewed for ASense. Firmware revisions are reported
/// as telemetry; capability probing and mutation readback are the compatibility
/// boundary across BIOS updates.
#[derive(Clone, Debug)]
struct VerifiedPlatform;

impl VerifiedPlatform {
    fn discover_at(root: &Path) -> Result<Self, HardwareError> {
        let vendor_path = rooted(root, "sys/class/dmi/id/sys_vendor");
        let product_path = rooted(root, "sys/class/dmi/id/product_name");
        let vendor = read_trimmed(&vendor_path, "read DMI vendor")?;
        let product = read_trimmed(&product_path, "read DMI product")?;
        if vendor != EXPECTED_VENDOR || product != EXPECTED_PRODUCT {
            return Err(HardwareError::UnsupportedPlatform { vendor, product });
        }
        Ok(Self)
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

        let hwmon_root = rooted(&root, "sys/class/hwmon");
        let entries = fs::read_dir(&hwmon_root).map_err(|source| HardwareError::Io {
            operation: "enumerate hwmon",
            path: hwmon_root.clone(),
            source,
        })?;
        let mut acer_paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| HardwareError::Io {
                operation: "read hwmon entry",
                path: hwmon_root.clone(),
                source,
            })?;
            let path = entry.path();
            let name_path = path.join("name");
            match fs::read_to_string(&name_path) {
                Ok(name) if name.trim() == "acer" => acer_paths.push(path),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(HardwareError::Io {
                        operation: "read hwmon name",
                        path: name_path,
                        source,
                    });
                }
            }
        }
        acer_paths.sort();
        let hwmon = match acer_paths.len() {
            0 => return Err(HardwareError::AcerHwmonNotFound),
            1 => acer_paths.remove(0),
            _ => return Err(HardwareError::AmbiguousAcerHwmon(acer_paths)),
        };

        for name in [
            "fan1_input",
            "fan2_input",
            "pwm1",
            "pwm2",
            "pwm1_enable",
            "pwm2_enable",
            "temp1_input",
            "temp2_input",
        ] {
            let path = hwmon.join(name);
            if !path.is_file() {
                return Err(HardwareError::MissingInterface(path));
            }
        }

        let platform_profile = rooted(&root, "sys/firmware/acpi/platform_profile");
        let platform_profile_choices = rooted(&root, "sys/firmware/acpi/platform_profile_choices");
        for path in [&platform_profile, &platform_profile_choices] {
            if !path.is_file() {
                return Err(HardwareError::MissingInterface(path.clone()));
            }
        }

        Ok(Self {
            _platform: platform,
            root,
            hwmon,
            platform_profile,
            platform_profile_choices,
        })
    }

    pub fn current_profile(&self) -> Result<PlatformProfile, HardwareError> {
        PlatformProfile::from_sysfs(&read_trimmed(
            &self.platform_profile,
            "read platform profile",
        )?)
    }

    pub fn set_profile(&self, profile: PlatformProfile) -> Result<(), HardwareError> {
        let choices = read_trimmed(
            &self.platform_profile_choices,
            "read platform profile choices",
        )?;
        if !choices
            .split_ascii_whitespace()
            .any(|choice| choice == profile.as_sysfs())
        {
            return Err(HardwareError::ProfileUnavailable(profile));
        }
        write_existing(
            &self.platform_profile,
            profile.as_sysfs(),
            "set platform profile",
        )?;
        let actual = read_trimmed(&self.platform_profile, "verify platform profile")?;
        ensure_readback("platform_profile", profile.as_sysfs(), &actual)
    }

    pub fn apply_fan_setting(&self, setting: FanSetting) -> Result<FanState, HardwareError> {
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
                Err(rollback) => Err(HardwareError::MutationRollbackFailed {
                    cause: Box::new(cause),
                    rollback: Box::new(rollback),
                }),
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
        let path = self.hwmon.join(format!("temp{channel}_input"));
        parse_value(&path, "temperature")
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    fn read_fan_channel(&self, channel: u8) -> Result<FanChannelState, HardwareError> {
        let mode_path = self.hwmon.join(format!("pwm{channel}_enable"));
        let pwm_path = self.hwmon.join(format!("pwm{channel}"));
        let rpm_path = self.hwmon.join(format!("fan{channel}_input"));
        let mode = read_optional_fan_mode(&mode_path)?;
        let pwm: u32 = parse_value(&pwm_path, "PWM")?;
        let pwm_raw = u8::try_from(pwm).map_err(|_| HardwareError::InvalidValue {
            field: "PWM",
            value: pwm.to_string(),
        })?;
        let rpm = parse_value(&rpm_path, "fan RPM")?;
        Ok(FanChannelState { mode, pwm_raw, rpm })
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
        let cpu_pwm = percent_to_pwm(cpu_percent);
        let gpu_pwm = percent_to_pwm(gpu_percent);

        // Firmware-safe sequence: remove firmware automation by taking both
        // fans to maximum first, set both target PWM values, and only then
        // expose both channels as manually controlled.  Any failure is caught
        // by apply_fan_setting and rolls both channels back to Automatic.
        self.write_mode(1, FanMode::Maximum)?;
        self.write_mode(2, FanMode::Maximum)?;
        self.verify_mode(1, FanMode::Maximum)?;
        self.verify_mode(2, FanMode::Maximum)?;
        self.write_pwm(1, cpu_pwm)?;
        self.write_pwm(2, gpu_pwm)?;
        self.write_mode(1, FanMode::Manual)?;
        self.write_mode(2, FanMode::Manual)?;
        // The PHN16-72 firmware caches a requested value while Maximum is
        // active. Re-apply it after both channels enter Manual so the final
        // transaction is explicit and symmetric.
        self.write_pwm(1, cpu_pwm)?;
        self.write_pwm(2, gpu_pwm)?;
        self.verify_pwm_percent(1, cpu_percent)?;
        self.verify_pwm_percent(2, gpu_percent)?;
        self.verify_mode(1, FanMode::Manual)?;
        self.verify_mode(2, FanMode::Manual)
    }

    fn force_automatic_unchecked(&self) -> Result<(), HardwareError> {
        // Attempt both writes even when the first one fails, so a partial
        // mutation has the best possible chance of returning to firmware
        // control. Preserve the first error for reporting.
        let first = self.write_mode(1, FanMode::Automatic).err();
        let second = self.write_mode(2, FanMode::Automatic).err();
        if let Some(error) = first.or(second) {
            return Err(error);
        }
        self.verify_mode(1, FanMode::Automatic)?;
        self.verify_mode(2, FanMode::Automatic)
    }

    fn write_mode(&self, channel: u8, mode: FanMode) -> Result<(), HardwareError> {
        write_existing(
            &self.hwmon.join(format!("pwm{channel}_enable")),
            mode.as_sysfs(),
            "set fan mode",
        )
    }

    fn verify_mode(&self, channel: u8, expected: FanMode) -> Result<(), HardwareError> {
        let path = self.hwmon.join(format!("pwm{channel}_enable"));
        let actual = read_trimmed(&path, "verify fan mode")?;
        ensure_readback("pwm_enable", expected.as_sysfs(), &actual)
    }

    fn write_pwm(&self, channel: u8, pwm: u8) -> Result<(), HardwareError> {
        write_existing(
            &self.hwmon.join(format!("pwm{channel}")),
            &pwm.to_string(),
            "set fan PWM",
        )
    }

    fn verify_pwm_percent(&self, channel: u8, expected_percent: u8) -> Result<(), HardwareError> {
        let path = self.hwmon.join(format!("pwm{channel}"));
        let actual = read_trimmed(&path, "verify fan PWM")?;
        let actual_raw = actual
            .parse::<u8>()
            .map_err(|_| HardwareError::InvalidValue {
                field: "PWM",
                value: actual.clone(),
            })?;
        // acer_wmi converts 0..255 to firmware percent and back, so raw-byte
        // equality is not stable. Verify the user-visible percentage instead.
        let actual_percent = pwm_to_percent(actual_raw);
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
        assert!(hardware.hwmon.ends_with("hwmon37"));
        assert_eq!(
            hardware.current_profile().unwrap(),
            PlatformProfile::Balanced
        );
    }

    #[test]
    fn accepts_exact_verified_platform() {
        let fixture = Fixture::new();
        AcerHardware::discover_at(&fixture.root).unwrap();
    }

    #[test]
    fn rejects_wrong_dmi_identity() {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("sys/class/dmi/id/product_name"),
            "Predator PHN16-71\n",
        )
        .unwrap();
        assert!(matches!(
            AcerHardware::discover_at(&fixture.root),
            Err(HardwareError::UnsupportedPlatform { .. })
        ));
    }

    #[test]
    fn accepts_missing_bios_identity_because_model_and_readback_are_authoritative() {
        let fixture = Fixture::new();
        let bios_path = fixture.root.join("sys/class/dmi/id/bios_version");
        fs::remove_file(&bios_path).unwrap();
        AcerHardware::discover_at(&fixture.root).unwrap();
    }

    #[test]
    fn accepts_new_bios_revision_on_the_same_verified_model() {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("sys/class/dmi/id/bios_version"),
            "V1.19\n",
        )
        .unwrap();

        AcerHardware::discover_at(&fixture.root).unwrap();
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
    fn rejects_out_of_range_manual_percentage_without_leaving_manual_mode() {
        let fixture = Fixture::new();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        assert!(
            hardware
                .apply_fan_setting(FanSetting::Manual {
                    cpu_percent: 101,
                    gpu_percent: 50,
                })
                .is_err()
        );
        let state = hardware.read_fan_state().unwrap();
        assert_eq!(state.cpu.mode, Some(FanMode::Automatic));
        assert_eq!(state.gpu.mode, Some(FanMode::Automatic));
    }

    #[test]
    fn rejects_manual_percentage_below_hardware_minimum() {
        let fixture = Fixture::new();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        let error = hardware
            .apply_fan_setting(FanSetting::Manual {
                cpu_percent: 19,
                gpu_percent: 50,
            })
            .unwrap_err();
        assert!(matches!(
            error,
            HardwareError::MutationRolledBack { cause }
                if matches!(*cause, HardwareError::InvalidValue {
                    field: "CPU fan percent",
                    ref value,
                } if value == "19")
        ));
        let state = hardware.read_fan_state().unwrap();
        assert_eq!(state.cpu.mode, Some(FanMode::Automatic));
        assert_eq!(state.gpu.mode, Some(FanMode::Automatic));
    }

    #[test]
    fn accepts_manual_percentage_boundaries() {
        let fixture = Fixture::new();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        let state = hardware
            .apply_fan_setting(FanSetting::Manual {
                cpu_percent: 20,
                gpu_percent: 100,
            })
            .unwrap();
        assert_eq!(state.cpu.mode, Some(FanMode::Manual));
        assert_eq!(state.gpu.mode, Some(FanMode::Manual));
        assert_eq!(state.cpu.pwm_raw, percent_to_pwm(20));
        assert_eq!(state.gpu.pwm_raw, percent_to_pwm(100));
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
    fn readback_failure_after_mutation_rolls_both_channels_back_to_auto() {
        let fixture = Fixture::new();
        let hardware = AcerHardware::discover_at(&fixture.root).unwrap();
        fs::remove_file(fixture.hwmon().join("fan1_input")).unwrap();
        let error = hardware.apply_fan_setting(FanSetting::Maximum).unwrap_err();
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
}
