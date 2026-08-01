use serde::{Deserialize, Serialize};

pub(super) const PROBE_SCHEMA: u8 = 3;
pub(super) const MAX_REPORT_BYTES: usize = 262_144;
pub(super) const MAX_TEXT_BYTES: usize = 96;
pub(super) const MAX_COMMAND_BYTES: usize = 48;
pub(super) const MAX_ITEMS: usize = 32;
pub(super) const MAX_POWER_ITEMS: usize = 8;
pub(super) const MAX_DESCRIPTOR_GEOMETRY: usize = 64;
pub(super) const MAX_HID_PAYLOAD_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeReport {
    pub schema: u8,
    pub provenance: Provenance,
    pub machine: MachineEvidence,
    pub power: PowerEvidence,
    pub drivers: DriverEvidence,
    pub profile: ProfileEvidence,
    pub fans: FanEvidence,
    pub platform: PlatformEvidence,
    pub lighting: Vec<LightingEvidence>,
    pub hid: Vec<HidEvidence>,
    pub privacy: PrivacyEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub report: String,
    pub asense_version: String,
    pub build_commit: Option<String>,
    pub captured_at_utc: String,
    pub capture_duration_ms: u64,
    pub mode: ProbeMode,
    pub daemon: Observation<DaemonIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeMode {
    Passive,
    ExtendedHid,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonIdentity {
    pub protocol: u16,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Observation<T> {
    Value {
        source: SourceId,
        value: T,
    },
    Absent {
        source: SourceId,
        reason: AbsenceReason,
    },
    Error {
        source: SourceId,
        error: ProbeError,
    },
}

impl<T> Observation<T> {
    pub fn value(source: SourceId, value: T) -> Self {
        Self::Value { source, value }
    }

    pub fn absent(source: SourceId, reason: AbsenceReason) -> Self {
        Self::Absent { source, reason }
    }

    pub fn error(source: SourceId, error: ProbeError) -> Self {
        Self::Error { source, error }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceId {
    ControlSocket,
    DmiVendor,
    DmiProduct,
    DmiBoard,
    DmiBios,
    Uname,
    OsRelease,
    PowerSupply,
    ModuleSysfs,
    WmiDriver,
    HwmonDriver,
    ProfileDiscovery,
    KernelPlatformProfile,
    GamingWmiProfile,
    KnownGamingWmiCommands,
    GamingWmiSupportedProfiles,
    FanDiscovery,
    AcerHwmon,
    GamingWmiFan,
    PlatformDiscovery,
    AsenseRgb,
    AsenseBattery,
    AsenseApge,
    HidDriver,
    HidReportDescriptor,
    HidFeatureA1,
    HidSelectorA2,
    HidFeatureA3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AbsenceReason {
    NotExposed,
    NotInstalled,
    NotApplicable,
    DaemonUnavailable,
    IncompleteControlInterface,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeError {
    pub stage: ErrorStage,
    pub class: ErrorClass,
    pub errno: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawValue>,
}

impl ProbeError {
    pub fn new(stage: ErrorStage, class: ErrorClass, errno: Option<i32>) -> Self {
        Self {
            stage,
            class,
            errno,
            raw: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorStage {
    Discover,
    Open,
    Read,
    Decode,
    Protocol,
    Verify,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorClass {
    NotFound,
    PermissionDenied,
    Io,
    Timeout,
    InvalidValue,
    Incompatible,
    Unsupported,
    Oversize,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawValue {
    pub encoding: RawEncoding,
    pub bytes: usize,
    pub value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum RawEncoding {
    #[serde(rename = "u8-hex")]
    U8Hex,
    #[serde(rename = "ascii-token")]
    AsciiToken,
    #[serde(rename = "scalar-hex")]
    ScalarHex,
    #[serde(rename = "hex")]
    Hex,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineEvidence {
    pub dmi: DmiEvidence,
    pub kernel: KernelEvidence,
    pub os: OsEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DmiEvidence {
    pub vendor: Observation<String>,
    pub product: Observation<String>,
    pub board: Observation<String>,
    pub bios: Observation<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KernelEvidence {
    pub release: Observation<String>,
    pub architecture: Observation<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OsEvidence {
    pub id: Observation<String>,
    pub version_id: Observation<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PowerEvidence {
    pub ac: Vec<AcEvidence>,
    pub batteries: Vec<BatteryEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcEvidence {
    pub ordinal: usize,
    pub kind: AcKind,
    pub online: Observation<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcKind {
    Mains,
    Usb,
    UsbC,
    UsbPd,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatteryEvidence {
    pub ordinal: usize,
    pub present: Observation<bool>,
    pub status: Observation<BatteryStatus>,
    pub capacity_percent: Observation<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatteryStatus {
    Charging,
    Discharging,
    Full,
    NotCharging,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DriverEvidence {
    pub modules: Vec<ModuleEvidence>,
    pub wmi: Vec<WmiEvidence>,
    pub hwmon_owner: Observation<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleEvidence {
    pub name: ModuleName,
    pub loaded: Observation<bool>,
    pub version: Observation<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum ModuleName {
    #[serde(rename = "acer_wmi")]
    AcerWmi,
    #[serde(rename = "asense_rgb")]
    AsenseRgb,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WmiEvidence {
    pub guid: String,
    pub instances: usize,
    pub owner: Observation<String>,
    pub groups: Vec<WmiGroup>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub enum WmiGroup {
    #[serde(rename = "asense_diagnostics")]
    AsenseDiagnostics,
    #[serde(rename = "asense_rgb")]
    AsenseRgb,
    #[serde(rename = "rgb_zoned")]
    RgbZoned,
    #[serde(rename = "gaming_fan")]
    GamingFan,
    #[serde(rename = "gaming_profile")]
    GamingProfile,
    #[serde(rename = "asense_battery")]
    AsenseBattery,
    #[serde(rename = "asense_apge")]
    AsenseApge,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileEvidence {
    pub transport: BackendTransport<ProfileBackend>,
    pub current: ProfileCurrent,
    pub choices: Observation<Vec<ProfileChoice>>,
    pub firmware_supported_bitmap: Observation<FirmwareBitmap>,
    pub physical_effect: PhysicalEffect,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BackendTransport<T> {
    Present {
        source: SourceId,
        backend: T,
    },
    Absent {
        source: SourceId,
        reason: AbsenceReason,
    },
    Error {
        source: SourceId,
        error: ProbeError,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileBackend {
    Kernel,
    GamingWmi,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ProfileCurrent {
    Known {
        source: SourceId,
        raw: RawValue,
        profile: ProfileName,
    },
    Unknown {
        source: SourceId,
        raw: RawValue,
    },
    Absent {
        source: SourceId,
        reason: AbsenceReason,
    },
    Error {
        source: SourceId,
        error: ProbeError,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileChoice {
    pub command: String,
    pub profile: ProfileName,
    pub transport_raw: Option<RawValue>,
    pub selectable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileName {
    Quiet,
    Balanced,
    Performance,
    Turbo,
    Eco,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareBitmap {
    pub authority: FirmwareBitmapAuthority,
    pub raw: RawValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FirmwareBitmapAuthority {
    FirmwareAdvisory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PhysicalEffect {
    Unverified,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FanEvidence {
    pub control: FanControl,
    pub rpm: Vec<RpmEvidence>,
    pub pwm: Vec<PwmEvidence>,
    pub temperatures: Vec<TemperatureEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FanControl {
    Present {
        source: SourceId,
        backend: FanBackend,
        modes: FanModes,
    },
    Absent {
        source: SourceId,
        reason: AbsenceReason,
    },
    Error {
        source: SourceId,
        error: ProbeError,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FanBackend {
    KernelPwm,
    GamingWmi,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FanModes {
    pub auto: bool,
    pub manual: bool,
    pub maximum: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RpmEvidence {
    pub channel: u8,
    pub label: String,
    pub read: Observation<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PwmEvidence {
    pub channel: u8,
    pub setpoint_unit: PwmSetpointUnit,
    pub setpoint: Observation<u8>,
    pub mode: Observation<PwmMode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PwmSetpointUnit {
    Pwm255,
    Percent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PwmMode {
    Maximum,
    Manual,
    Auto,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TemperatureEvidence {
    pub channel: u8,
    pub label: String,
    pub millidegrees_c: Observation<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformEvidence {
    pub transport: BackendTransport<PlatformBackend>,
    pub fields: Vec<PlatformFieldEvidence>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlatformBackend {
    GamingWmi,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformFieldEvidence {
    pub name: PlatformField,
    pub expected: bool,
    pub exposed: bool,
    pub source: SourceId,
    pub read: Observation<PlatformValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlatformField {
    BatteryLimit,
    BatteryCalibration,
    UsbOffCharging,
    KeyboardTimeout,
    BootSound,
    LcdOverride,
    RearLogo,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PlatformValue {
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

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LightingEvidence {
    pub id: String,
    pub backend: LightingBackend,
    pub target: LightingTarget,
    pub zones: u8,
    pub modes: LightingModes,
    pub state_readable: bool,
    pub authority: LightingAuthority,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LightingBackend {
    ZonedWmi,
    Enek5130,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LightingTarget {
    Keyboard,
    CoverLogo,
    RearLogo,
    Lightbar,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LightingModes {
    pub static_color: bool,
    pub brightness: bool,
    pub breathing: bool,
    pub neon: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LightingAuthority {
    TransportCapability,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HidEvidence {
    pub role: HidRole,
    pub identity: HidIdentity,
    pub driver: Observation<String>,
    pub descriptor: Observation<HidDescriptor>,
    pub a1: Option<Observation<HidA1>>,
    pub extended: HidExtended,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HidRole {
    Enek5130Lighting,
    AcerEcHidPowerCandidate,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HidIdentity {
    pub bus: HidBus,
    pub vid: String,
    pub pid: String,
    pub name: HidName,
    pub interface: Option<u8>,
    pub usage_page: Option<String>,
    pub usage: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HidBus {
    I2c,
    Usb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum HidName {
    #[serde(rename = "ENEK5130")]
    Enek5130,
    #[serde(rename = "Acer EC HID")]
    AcerEcHid,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HidDescriptor {
    pub bytes: usize,
    pub sha256: String,
    pub feature_reports: Vec<HidFeatureGeometry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HidFeatureGeometry {
    pub id: String,
    pub bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HidA1 {
    pub requested_bytes: usize,
    pub returned_bytes: usize,
    pub payload: RawValue,
    pub targets: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HidExtended {
    pub requested: bool,
    pub selectors: Vec<HidSelectorReceipt>,
    pub a3: Vec<HidA3Evidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HidSelectorReceipt {
    pub target: String,
    pub result: Observation<HidSelectorResult>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HidSelectorResult {
    Sent,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HidA3Evidence {
    pub target: String,
    pub read: Observation<HidReport>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HidReport {
    pub requested_bytes: usize,
    pub returned_bytes: usize,
    pub payload: RawValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyEvidence {
    pub automatic_upload: bool,
    pub persistent_report_id: bool,
    pub default_mutations: Vec<MutationReceipt>,
    pub extended_mutations: Vec<MutationReceipt>,
    pub excluded: Vec<ExcludedIdentity>,
    pub bounded_raw: Vec<RawAllowance>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MutationReceipt {
    pub operation: MutationOperation,
    pub target: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum MutationOperation {
    #[serde(rename = "enek-a2-selector")]
    EnekA2Selector,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExcludedIdentity {
    Serials,
    Uuids,
    Hostname,
    UserIdentity,
    NetworkIdentity,
    BootId,
    StorageIdentity,
    HidSerialAndPhysicalPath,
    Journals,
    RawAcpiTables,
    AbsoluteDevicePaths,
    Environment,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawAllowance {
    pub path: String,
    pub max_bytes: usize,
}

impl ProbeReport {
    pub fn validate(&self) -> Result<(), String> {
        require(self.schema == PROBE_SCHEMA, "probe schema must be 3")?;
        require(
            self.provenance.report == "asense-probe",
            "invalid report token",
        )?;
        validate_text(
            &self.provenance.asense_version,
            MAX_TEXT_BYTES,
            "ASense version",
        )?;
        if let Some(commit) = &self.provenance.build_commit {
            require(
                commit.len() == 40
                    && commit
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "build commit must be lowercase 40-hex",
            )?;
        }
        validate_timestamp(&self.provenance.captured_at_utc)?;
        validate_observation(&self.provenance.daemon, validate_daemon)?;
        validate_machine(&self.machine)?;
        validate_power(&self.power)?;
        validate_drivers(&self.drivers)?;
        validate_profile(&self.profile)?;
        validate_fans(&self.fans)?;
        validate_platform(&self.platform)?;
        require(
            self.lighting.len() <= MAX_ITEMS,
            "too many lighting devices",
        )?;
        for (index, light) in self.lighting.iter().enumerate() {
            validate_token(&light.id, MAX_TEXT_BYTES, "lighting id")?;
            require(
                (1..=32).contains(&light.zones),
                "lighting zone count is out of range",
            )?;
            if index > 0 {
                require(
                    self.lighting[index - 1].id < light.id,
                    "lighting inventory is not stably sorted and unique",
                )?;
            }
        }
        require(self.hid.len() <= MAX_ITEMS, "too many HID devices")?;
        for (index, hid) in self.hid.iter().enumerate() {
            validate_hid(hid)?;
            if index > 0 {
                require(
                    compare_hid(&self.hid[index - 1], hid).is_lt(),
                    "HID inventory is not stably sorted and unique",
                )?;
            }
        }
        validate_privacy(&self.privacy)?;
        let encoded = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("cannot size probe report: {error}"))?;
        require(
            encoded.len() < MAX_REPORT_BYTES,
            "probe report is oversized",
        )
    }
}

fn validate_daemon(value: &DaemonIdentity) -> Result<(), String> {
    require(value.protocol > 0, "daemon protocol must be non-zero")?;
    validate_text(&value.version, MAX_TEXT_BYTES, "daemon version")
}

fn validate_machine(machine: &MachineEvidence) -> Result<(), String> {
    for (label, value) in [
        ("DMI vendor", &machine.dmi.vendor),
        ("DMI product", &machine.dmi.product),
        ("DMI board", &machine.dmi.board),
        ("DMI BIOS", &machine.dmi.bios),
        ("kernel release", &machine.kernel.release),
        ("kernel architecture", &machine.kernel.architecture),
        ("OS ID", &machine.os.id),
        ("OS version ID", &machine.os.version_id),
    ] {
        validate_observation(value, |text| validate_text(text, MAX_TEXT_BYTES, label))?;
    }
    Ok(())
}

fn validate_power(power: &PowerEvidence) -> Result<(), String> {
    require(power.ac.len() <= MAX_POWER_ITEMS, "too many AC supplies")?;
    require(
        power.batteries.len() <= MAX_POWER_ITEMS,
        "too many batteries",
    )?;
    for (index, supply) in power.ac.iter().enumerate() {
        require(supply.ordinal == index, "AC ordinals are not canonical")?;
        validate_observation(&supply.online, |_| Ok(()))?;
    }
    for (index, battery) in power.batteries.iter().enumerate() {
        require(
            battery.ordinal == index,
            "battery ordinals are not canonical",
        )?;
        validate_observation(&battery.present, |_| Ok(()))?;
        validate_observation(&battery.status, |_| Ok(()))?;
        validate_observation(&battery.capacity_percent, |value| {
            require(*value <= 100, "battery capacity exceeds 100")
        })?;
    }
    Ok(())
}

fn validate_drivers(drivers: &DriverEvidence) -> Result<(), String> {
    require(
        drivers.modules.len() == 2,
        "module inventory must have two entries",
    )?;
    require(
        drivers.modules[0].name == ModuleName::AcerWmi
            && drivers.modules[1].name == ModuleName::AsenseRgb,
        "module inventory order is not canonical",
    )?;
    for module in &drivers.modules {
        validate_observation(&module.loaded, |_| Ok(()))?;
        validate_observation(&module.version, |value| {
            validate_token(value, MAX_TEXT_BYTES, "module version")
        })?;
    }
    require(drivers.wmi.len() <= 3, "too many WMI GUIDs")?;
    for item in &drivers.wmi {
        require(is_guid(&item.guid), "invalid WMI GUID")?;
        require(
            item.instances > 0 && item.instances <= 32,
            "invalid WMI instance count",
        )?;
        validate_observation(&item.owner, |value| {
            validate_token(value, MAX_TEXT_BYTES, "WMI driver owner")
        })?;
        require(item.groups.len() <= 7, "too many WMI groups")?;
        require(
            item.groups.windows(2).all(|pair| pair[0] < pair[1]),
            "WMI groups are not sorted and unique",
        )?;
    }
    validate_observation(&drivers.hwmon_owner, |value| {
        validate_token(value, MAX_TEXT_BYTES, "hwmon owner")
    })
}

fn validate_profile(profile: &ProfileEvidence) -> Result<(), String> {
    validate_backend_transport(&profile.transport)?;
    match &profile.current {
        ProfileCurrent::Known { raw, profile, .. } => {
            validate_profile_raw(raw)?;
            require(
                profile_name_for_raw(raw) == Some(*profile),
                "known profile semantic does not match its raw authority",
            )?;
        }
        ProfileCurrent::Unknown { raw, .. } => {
            validate_profile_raw(raw)?;
            require(
                profile_name_for_raw(raw).is_none(),
                "known profile raw was represented as unknown",
            )?;
        }
        ProfileCurrent::Absent { .. } => {}
        ProfileCurrent::Error { error, .. } => validate_error(error)?,
    }
    validate_observation(&profile.choices, |choices| {
        require(choices.len() <= 8, "too many profile choices")?;
        for (index, choice) in choices.iter().enumerate() {
            validate_token(&choice.command, MAX_COMMAND_BYTES, "profile command")?;
            require(
                choices[..index]
                    .iter()
                    .all(|previous| previous.command != choice.command),
                "profile choices are not unique",
            )?;
            require(
                profile_name_for_command(&choice.command) == Some(choice.profile),
                "profile choice command and semantic differ",
            )?;
            if let Some(raw) = &choice.transport_raw {
                validate_raw(raw, 1, &[RawEncoding::U8Hex])?;
                require(
                    profile_name_for_raw(raw) == Some(choice.profile),
                    "profile choice raw and semantic differ",
                )?;
            }
        }
        Ok(())
    })?;
    validate_observation(&profile.firmware_supported_bitmap, |bitmap| {
        validate_raw(&bitmap.raw, 1, &[RawEncoding::U8Hex])
    })
}

fn validate_backend_transport<T>(transport: &BackendTransport<T>) -> Result<(), String> {
    match transport {
        BackendTransport::Present { .. } | BackendTransport::Absent { .. } => Ok(()),
        BackendTransport::Error { error, .. } => validate_error(error),
    }
}

fn validate_profile_raw(raw: &RawValue) -> Result<(), String> {
    match raw.encoding {
        RawEncoding::U8Hex => validate_raw(raw, 1, &[RawEncoding::U8Hex]),
        RawEncoding::AsciiToken => validate_raw(raw, MAX_COMMAND_BYTES, &[RawEncoding::AsciiToken]),
        _ => Err("profile current raw encoding is invalid".to_string()),
    }
}

fn profile_name_for_raw(raw: &RawValue) -> Option<ProfileName> {
    match (raw.encoding, raw.value.as_str()) {
        (RawEncoding::U8Hex, "00") | (RawEncoding::AsciiToken, "quiet") => Some(ProfileName::Quiet),
        (RawEncoding::U8Hex, "01") | (RawEncoding::AsciiToken, "balanced") => {
            Some(ProfileName::Balanced)
        }
        (RawEncoding::U8Hex, "04") | (RawEncoding::AsciiToken, "balanced-performance") => {
            Some(ProfileName::Performance)
        }
        (RawEncoding::U8Hex, "05") | (RawEncoding::AsciiToken, "performance") => {
            Some(ProfileName::Turbo)
        }
        (RawEncoding::U8Hex, "06") | (RawEncoding::AsciiToken, "low-power") => {
            Some(ProfileName::Eco)
        }
        _ => None,
    }
}

fn profile_name_for_command(command: &str) -> Option<ProfileName> {
    let raw = RawValue {
        encoding: RawEncoding::AsciiToken,
        bytes: command.len(),
        value: command.to_string(),
    };
    profile_name_for_raw(&raw)
}

fn validate_fans(fans: &FanEvidence) -> Result<(), String> {
    if let FanControl::Error { error, .. } = &fans.control {
        validate_error(error)?;
    }
    require(fans.rpm.len() <= MAX_ITEMS, "too many RPM channels")?;
    require(fans.pwm.len() <= MAX_ITEMS, "too many PWM channels")?;
    require(
        fans.temperatures.len() <= MAX_ITEMS,
        "too many temperature channels",
    )?;
    for rpm in &fans.rpm {
        require(rpm.channel > 0, "RPM channel must be non-zero")?;
        validate_text(&rpm.label, MAX_TEXT_BYTES, "RPM label")?;
        validate_observation(&rpm.read, |_| Ok(()))?;
        require(
            observation_source(&rpm.read) == SourceId::AcerHwmon,
            "RPM source differs",
        )?;
    }
    for pwm in &fans.pwm {
        require(pwm.channel > 0, "PWM channel must be non-zero")?;
        validate_observation(&pwm.setpoint, |value| match pwm.setpoint_unit {
            PwmSetpointUnit::Pwm255 => Ok(()),
            PwmSetpointUnit::Percent => require(*value <= 100, "fan percentage exceeds 100"),
        })?;
        validate_observation(&pwm.mode, |_| Ok(()))?;
        let setpoint_source = observation_source(&pwm.setpoint);
        require(
            observation_source(&pwm.mode) == setpoint_source,
            "fan mode and setpoint sources differ",
        )?;
        require(
            matches!(
                (setpoint_source, pwm.setpoint_unit),
                (SourceId::AcerHwmon, PwmSetpointUnit::Pwm255)
                    | (SourceId::GamingWmiFan, PwmSetpointUnit::Percent)
            ),
            "fan setpoint unit does not match its source",
        )?;
    }
    for temperature in &fans.temperatures {
        require(
            temperature.channel > 0,
            "temperature channel must be non-zero",
        )?;
        validate_text(&temperature.label, MAX_TEXT_BYTES, "temperature label")?;
        validate_observation(&temperature.millidegrees_c, |value| {
            require(
                (-100_000..=250_000).contains(value),
                "temperature is outside physical bounds",
            )
        })?;
        require(
            observation_source(&temperature.millidegrees_c) == SourceId::AcerHwmon,
            "temperature source differs",
        )?;
    }
    validate_channel_order(fans.rpm.iter().map(|item| item.channel), "RPM")?;
    validate_channel_order(fans.pwm.iter().map(|item| item.channel), "PWM")?;
    validate_channel_order(
        fans.temperatures.iter().map(|item| item.channel),
        "temperature",
    )?;
    Ok(())
}

fn validate_platform(platform: &PlatformEvidence) -> Result<(), String> {
    validate_backend_transport(&platform.transport)?;
    const ORDER: [PlatformField; 7] = [
        PlatformField::BatteryLimit,
        PlatformField::BatteryCalibration,
        PlatformField::UsbOffCharging,
        PlatformField::KeyboardTimeout,
        PlatformField::BootSound,
        PlatformField::LcdOverride,
        PlatformField::RearLogo,
    ];
    require(
        platform.fields.len() == ORDER.len(),
        "platform field count differs",
    )?;
    for ((field, expected), source) in platform.fields.iter().zip(ORDER).zip([
        SourceId::AsenseBattery,
        SourceId::AsenseBattery,
        SourceId::AsenseApge,
        SourceId::AsenseApge,
        SourceId::AsenseRgb,
        SourceId::AsenseRgb,
        SourceId::AsenseRgb,
    ]) {
        require(field.name == expected, "platform field order differs")?;
        require(field.source == source, "platform field source differs")?;
        require(
            observation_source(&field.read) == field.source,
            "platform read source differs from its field",
        )?;
        validate_observation(&field.read, |value| match value {
            PlatformValue::Bool { .. } => Ok(()),
            PlatformValue::UsbThreshold { value } => require(
                matches!(value, 0 | 10 | 20 | 30),
                "USB threshold is invalid",
            ),
            PlatformValue::RearLogo {
                brightness, color, ..
            } => {
                require(*brightness <= 100, "rear logo brightness exceeds 100")?;
                require(is_hex(color, 6), "rear logo color is not six hex digits")
            }
        })?;
    }
    Ok(())
}

fn validate_hid(hid: &HidEvidence) -> Result<(), String> {
    require(is_hex(&hid.identity.vid, 4), "HID VID is invalid")?;
    require(is_hex(&hid.identity.pid, 4), "HID PID is invalid")?;
    require(
        hid.identity.usage_page.is_some() == hid.identity.usage.is_some(),
        "HID usage page and usage must be present together",
    )?;
    if let (Some(page), Some(usage)) = (&hid.identity.usage_page, &hid.identity.usage) {
        require(is_hex(page, 4), "HID usage page is invalid")?;
        require(is_hex(usage, 4), "HID usage is invalid")?;
    }
    require(
        hid.identity.bus == HidBus::I2c && hid.identity.interface.is_none(),
        "allow-listed HID-over-I2C identity has a fabricated interface",
    )?;
    match hid.role {
        HidRole::Enek5130Lighting => {
            require(
                hid.identity.vid == "0cf2"
                    && hid.identity.pid == "5130"
                    && hid.identity.name == HidName::Enek5130,
                "ENEK role and exact HID identity differ",
            )?;
            require(hid.a1.is_some(), "ENEK HID evidence omits A1 status")?;
        }
        HidRole::AcerEcHidPowerCandidate => {
            require(
                hid.identity.vid == "1025"
                    && hid.identity.pid == "174b"
                    && hid.identity.name == HidName::AcerEcHid,
                "Acer EC-HID role and exact identity differ",
            )?;
            require(hid.a1.is_none(), "Acer EC-HID contains ENEK A1 evidence")?;
        }
    }
    require(
        observation_source(&hid.driver) == SourceId::HidDriver,
        "HID driver source differs",
    )?;
    validate_observation(&hid.driver, |value| {
        validate_token(value, MAX_TEXT_BYTES, "HID driver")
    })?;
    require(
        observation_source(&hid.descriptor) == SourceId::HidReportDescriptor,
        "HID descriptor source differs",
    )?;
    validate_observation(&hid.descriptor, |descriptor| {
        require(
            (1..=4096).contains(&descriptor.bytes),
            "HID descriptor is oversized",
        )?;
        require(
            is_hex(&descriptor.sha256, 64),
            "HID descriptor SHA-256 is invalid",
        )?;
        require(
            descriptor.feature_reports.len() <= MAX_DESCRIPTOR_GEOMETRY,
            "too many HID feature reports",
        )?;
        for (index, report) in descriptor.feature_reports.iter().enumerate() {
            require(is_hex(&report.id, 2), "HID report ID is invalid")?;
            require(
                (1..=4096).contains(&report.bytes),
                "HID report geometry is oversized",
            )?;
            if index > 0 {
                require(
                    descriptor.feature_reports[index - 1].id < report.id,
                    "HID report geometry is not sorted and unique",
                )?;
            }
        }
        Ok(())
    })?;
    if let Some(a1) = &hid.a1 {
        require(
            observation_source(a1) == SourceId::HidFeatureA1,
            "A1 source differs",
        )?;
        validate_observation(a1, |a1| {
            require(
                (2..=MAX_HID_PAYLOAD_BYTES).contains(&a1.requested_bytes)
                    && (2..=a1.requested_bytes).contains(&a1.returned_bytes),
                "A1 report is oversized",
            )?;
            validate_raw(&a1.payload, MAX_HID_PAYLOAD_BYTES, &[RawEncoding::Hex])?;
            require(
                a1.payload.bytes == a1.returned_bytes && a1.payload.value.starts_with("a1"),
                "A1 payload length or report ID differs",
            )?;
            require(a1.targets.len() <= MAX_ITEMS, "too many A1 targets")?;
            require(
                a1.targets.iter().all(|target| is_hex(target, 2)),
                "A1 target is invalid",
            )?;
            require(
                a1.targets.windows(2).all(|pair| pair[0] < pair[1]),
                "A1 targets are not sorted and unique",
            )?;
            require(
                a1_targets_from_payload(&a1.payload.value)? == a1.targets,
                "A1 decoded targets differ from its retained payload",
            )
        })?;
    }
    require(
        hid.extended.selectors.len() <= MAX_ITEMS && hid.extended.a3.len() <= MAX_ITEMS,
        "extended HID evidence is oversized",
    )?;
    require(
        hid.extended.requested || hid.extended.selectors.is_empty() && hid.extended.a3.is_empty(),
        "passive HID evidence contains extended receipts",
    )?;
    for selector in &hid.extended.selectors {
        require(is_hex(&selector.target, 2), "A2 target is invalid")?;
        require(
            observation_source(&selector.result) == SourceId::HidSelectorA2,
            "A2 selector source differs",
        )?;
        validate_observation(&selector.result, |_| Ok(()))?;
    }
    for a3 in &hid.extended.a3 {
        require(is_hex(&a3.target, 2), "A3 target is invalid")?;
        require(
            observation_source(&a3.read) == SourceId::HidFeatureA3,
            "A3 report source differs",
        )?;
        validate_observation(&a3.read, |report| {
            require(
                report.requested_bytes <= MAX_HID_PAYLOAD_BYTES
                    && report.returned_bytes <= MAX_HID_PAYLOAD_BYTES,
                "A3 report is oversized",
            )?;
            validate_raw(&report.payload, MAX_HID_PAYLOAD_BYTES, &[RawEncoding::Hex])
        })?;
    }
    Ok(())
}

fn a1_targets_from_payload(value: &str) -> Result<Vec<String>, String> {
    let bytes = decode_hex(value)?;
    require(
        bytes.len() >= 2 && bytes[0] == 0xa1,
        "A1 payload header is invalid",
    )?;
    let count = usize::from(bytes[1]);
    let end = 2_usize
        .checked_add(count)
        .ok_or_else(|| "A1 target count overflows".to_string())?;
    require(
        count <= MAX_ITEMS && end <= bytes.len(),
        "A1 target count is invalid",
    )?;
    let mut targets = bytes[2..end]
        .iter()
        .map(|target| format!("{target:02x}"))
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    Ok(targets)
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    require(value.len().is_multiple_of(2), "hex payload has odd length")?;
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("hex pair is ASCII");
            u8::from_str_radix(pair, 16).map_err(|_| "hex payload is invalid".to_string())
        })
        .collect()
}

fn compare_hid(left: &HidEvidence, right: &HidEvidence) -> std::cmp::Ordering {
    left.role
        .cmp(&right.role)
        .then_with(|| left.identity.usage_page.cmp(&right.identity.usage_page))
        .then_with(|| left.identity.usage.cmp(&right.identity.usage))
        .then_with(|| hid_descriptor_hash(left).cmp(hid_descriptor_hash(right)))
}

fn hid_descriptor_hash(hid: &HidEvidence) -> &str {
    match &hid.descriptor {
        Observation::Value { value, .. } => value.sha256.as_str(),
        Observation::Absent { .. } | Observation::Error { .. } => "",
    }
}

fn validate_privacy(privacy: &PrivacyEvidence) -> Result<(), String> {
    const EXCLUDED: [ExcludedIdentity; 12] = [
        ExcludedIdentity::Serials,
        ExcludedIdentity::Uuids,
        ExcludedIdentity::Hostname,
        ExcludedIdentity::UserIdentity,
        ExcludedIdentity::NetworkIdentity,
        ExcludedIdentity::BootId,
        ExcludedIdentity::StorageIdentity,
        ExcludedIdentity::HidSerialAndPhysicalPath,
        ExcludedIdentity::Journals,
        ExcludedIdentity::RawAcpiTables,
        ExcludedIdentity::AbsoluteDevicePaths,
        ExcludedIdentity::Environment,
    ];
    const RAW: [(&str, usize); 6] = [
        ("profile.current.raw", 48),
        ("profile.choices.value[].transport_raw", 1),
        ("profile.firmware_supported_bitmap.value.raw", 1),
        ("platform.fields[].read.error.raw", 8),
        ("hid[].a1.value.payload", 64),
        ("hid[].extended.a3[].read.value.payload", 64),
    ];
    require(
        !privacy.automatic_upload,
        "probe may not upload automatically",
    )?;
    require(
        !privacy.persistent_report_id,
        "probe may not create a persistent report ID",
    )?;
    require(
        privacy.default_mutations.is_empty(),
        "default probe contains a mutation receipt",
    )?;
    require(
        privacy.excluded == EXCLUDED,
        "privacy exclusion list differs",
    )?;
    require(
        privacy.bounded_raw.len() == RAW.len(),
        "raw allow-list differs",
    )?;
    for (item, (path, max_bytes)) in privacy.bounded_raw.iter().zip(RAW) {
        require(
            item.path == path && item.max_bytes == max_bytes,
            "raw allow-list entry differs",
        )?;
    }
    for mutation in &privacy.extended_mutations {
        require(is_hex(&mutation.target, 2), "mutation target is invalid")?;
    }
    Ok(())
}

fn validate_observation<T>(
    observation: &Observation<T>,
    validate_value: impl FnOnce(&T) -> Result<(), String>,
) -> Result<(), String> {
    match observation {
        Observation::Value { value, .. } => validate_value(value),
        Observation::Absent { .. } => Ok(()),
        Observation::Error { error, .. } => validate_error(error),
    }
}

fn observation_source<T>(observation: &Observation<T>) -> SourceId {
    match observation {
        Observation::Value { source, .. }
        | Observation::Absent { source, .. }
        | Observation::Error { source, .. } => *source,
    }
}

fn validate_channel_order(channels: impl Iterator<Item = u8>, label: &str) -> Result<(), String> {
    let mut previous = None;
    for channel in channels {
        require(
            previous.is_none_or(|previous| channel > previous),
            &format!("{label} channels are not sorted and unique"),
        )?;
        previous = Some(channel);
    }
    Ok(())
}

fn validate_error(error: &ProbeError) -> Result<(), String> {
    if let Some(errno) = error.errno {
        require(errno >= 0, "errno must be non-negative")?;
    }
    if let Some(raw) = &error.raw {
        validate_raw(raw, 8, &[RawEncoding::ScalarHex])?;
    }
    Ok(())
}

fn validate_raw(raw: &RawValue, maximum: usize, allowed: &[RawEncoding]) -> Result<(), String> {
    require(
        allowed.contains(&raw.encoding),
        "raw encoding is not allowed",
    )?;
    require(raw.bytes <= maximum, "raw value exceeds its byte bound")?;
    match raw.encoding {
        RawEncoding::U8Hex | RawEncoding::ScalarHex | RawEncoding::Hex => require(
            raw.value.len() == raw.bytes.saturating_mul(2)
                && raw
                    .value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "raw hex value has invalid length or characters",
        ),
        RawEncoding::AsciiToken => {
            require(raw.value.len() == raw.bytes, "raw ASCII byte count differs")?;
            validate_token(&raw.value, maximum, "raw ASCII token")
        }
    }
}

fn validate_text(value: &str, maximum: usize, label: &str) -> Result<(), String> {
    require(!value.is_empty(), &format!("{label} is empty"))?;
    require(value.len() <= maximum, &format!("{label} is oversized"))?;
    require(
        !value.chars().any(char::is_control),
        &format!("{label} contains control characters"),
    )?;
    require(
        !value.starts_with('/'),
        &format!("{label} contains an absolute path"),
    )
}

fn validate_token(value: &str, maximum: usize, label: &str) -> Result<(), String> {
    validate_text(value, maximum, label)?;
    require(
        value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b':')
        }),
        &format!("{label} is not a bounded token"),
    )
}

fn validate_timestamp(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    require(
        bytes.len() == 20
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[10] == b'T'
            && bytes[13] == b':'
            && bytes[16] == b':'
            && bytes[19] == b'Z'
            && bytes.iter().enumerate().all(|(index, byte)| {
                matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
            }),
        "capture timestamp is not canonical RFC 3339 UTC",
    )
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte)
            }
        })
}

fn require(condition: bool, error: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(error.to_string())
    }
}

pub(super) fn absent_profile() -> ProfileEvidence {
    ProfileEvidence {
        transport: BackendTransport::Absent {
            source: SourceId::ProfileDiscovery,
            reason: AbsenceReason::DaemonUnavailable,
        },
        current: ProfileCurrent::Absent {
            source: SourceId::ProfileDiscovery,
            reason: AbsenceReason::DaemonUnavailable,
        },
        choices: Observation::absent(SourceId::ProfileDiscovery, AbsenceReason::DaemonUnavailable),
        firmware_supported_bitmap: Observation::absent(
            SourceId::GamingWmiSupportedProfiles,
            AbsenceReason::DaemonUnavailable,
        ),
        physical_effect: PhysicalEffect::Unverified,
    }
}

pub(super) fn absent_fans() -> FanEvidence {
    FanEvidence {
        control: FanControl::Absent {
            source: SourceId::FanDiscovery,
            reason: AbsenceReason::DaemonUnavailable,
        },
        rpm: Vec::new(),
        pwm: Vec::new(),
        temperatures: Vec::new(),
    }
}

pub(super) fn absent_platform() -> PlatformEvidence {
    let fields = [
        (PlatformField::BatteryLimit, SourceId::AsenseBattery),
        (PlatformField::BatteryCalibration, SourceId::AsenseBattery),
        (PlatformField::UsbOffCharging, SourceId::AsenseApge),
        (PlatformField::KeyboardTimeout, SourceId::AsenseApge),
        (PlatformField::BootSound, SourceId::AsenseRgb),
        (PlatformField::LcdOverride, SourceId::AsenseRgb),
        (PlatformField::RearLogo, SourceId::AsenseRgb),
    ]
    .into_iter()
    .map(|(name, source)| PlatformFieldEvidence {
        name,
        expected: false,
        exposed: false,
        source,
        read: Observation::absent(source, AbsenceReason::DaemonUnavailable),
    })
    .collect();
    PlatformEvidence {
        transport: BackendTransport::Absent {
            source: SourceId::PlatformDiscovery,
            reason: AbsenceReason::DaemonUnavailable,
        },
        fields,
    }
}

pub(super) fn passive_privacy() -> PrivacyEvidence {
    PrivacyEvidence {
        automatic_upload: false,
        persistent_report_id: false,
        default_mutations: Vec::new(),
        extended_mutations: Vec::new(),
        excluded: vec![
            ExcludedIdentity::Serials,
            ExcludedIdentity::Uuids,
            ExcludedIdentity::Hostname,
            ExcludedIdentity::UserIdentity,
            ExcludedIdentity::NetworkIdentity,
            ExcludedIdentity::BootId,
            ExcludedIdentity::StorageIdentity,
            ExcludedIdentity::HidSerialAndPhysicalPath,
            ExcludedIdentity::Journals,
            ExcludedIdentity::RawAcpiTables,
            ExcludedIdentity::AbsoluteDevicePaths,
            ExcludedIdentity::Environment,
        ],
        bounded_raw: vec![
            RawAllowance {
                path: "profile.current.raw".to_string(),
                max_bytes: 48,
            },
            RawAllowance {
                path: "profile.choices.value[].transport_raw".to_string(),
                max_bytes: 1,
            },
            RawAllowance {
                path: "profile.firmware_supported_bitmap.value.raw".to_string(),
                max_bytes: 1,
            },
            RawAllowance {
                path: "platform.fields[].read.error.raw".to_string(),
                max_bytes: 8,
            },
            RawAllowance {
                path: "hid[].a1.value.payload".to_string(),
                max_bytes: 64,
            },
            RawAllowance {
                path: "hid[].extended.a3[].read.value.payload".to_string(),
                max_bytes: 64,
            },
        ],
    }
}
