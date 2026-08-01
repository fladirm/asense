//! Privacy-bounded, read-only compatibility evidence.

mod schema;

use std::ffi::CStr;
use std::fs::{self, File};
use std::io::Read as _;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::control::{CONTROL_SOCKET, ControlClient, ControlError};
use crate::hardware::discover_acer_hwmon;
use crate::passive_diagnostics as passive;
use schema::{
    AbsenceReason, AcEvidence, AcKind, BatteryEvidence, BatteryStatus, DaemonIdentity, DmiEvidence,
    DriverEvidence, ErrorClass, ErrorStage, KernelEvidence, MachineEvidence, ModuleEvidence,
    ModuleName, Observation, OsEvidence, PowerEvidence, ProbeError, ProbeMode, ProbeReport,
    Provenance, SourceId, WmiEvidence, WmiGroup, absent_fans, absent_platform, absent_profile,
    passive_privacy,
};

const MAX_INPUT_BYTES: usize = 4096;
const MAX_POWER_DIRECTORY_ENTRIES: usize = 64;
const MAX_SUMMARY_BYTES: usize = 16_384;
const GAMING_GUID: &str = "7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56";
const BATTERY_GUID: &str = "79772EC5-04B1-4BFD-843C-61E7F77B6CC9";
const APGE_GUID: &str = "61EF69EA-865C-4BC3-A502-A0DEBA0CB531";

struct DaemonContext {
    identity: Observation<DaemonIdentity>,
    diagnostics: DaemonDiagnostics,
}

enum DaemonDiagnostics {
    Value(Box<passive::PassiveDiagnostics>),
    Unavailable,
    Error(ProbeError),
}

/// Generates the production report. The control connection performs only the
/// protocol handshake; schema 3 deliberately does not call `CAPS` because
/// historical ENEK capability discovery can send A2 selectors.
pub fn generate() -> Result<String, String> {
    let captured_at_utc = captured_at_utc()?;
    let started = Instant::now();
    let daemon = collect_daemon_context();
    generate_at_with_context(Path::new("/"), captured_at_utc, started, daemon)
}

/// Generates a bounded human-readable view of the same authoritative schema-3
/// report as [`generate`]. The summary performs no additional collection and
/// deliberately omits raw payloads and descriptor hashes.
pub fn generate_summary() -> Result<String, String> {
    summarize_json(&generate()?)
}

/// Fixture-friendly form. The desktop CLI never exposes an alternate root.
pub fn generate_at(root: &Path) -> Result<String, String> {
    let captured_at_utc = captured_at_utc()?;
    let started = Instant::now();
    generate_at_with_context(
        root,
        captured_at_utc,
        started,
        DaemonContext {
            identity: Observation::absent(
                SourceId::ControlSocket,
                AbsenceReason::DaemonUnavailable,
            ),
            diagnostics: DaemonDiagnostics::Unavailable,
        },
    )
}

fn generate_at_with_context(
    root: &Path,
    captured_at_utc: String,
    started: Instant,
    daemon: DaemonContext,
) -> Result<String, String> {
    let machine = collect_machine(root);
    let power = collect_power(root)?;
    let drivers = collect_drivers(root)?;
    let (profile, fans, platform, lighting, hid) = map_daemon_diagnostics(daemon.diagnostics);
    let report = ProbeReport {
        schema: schema::PROBE_SCHEMA,
        provenance: Provenance {
            report: "asense-probe".to_string(),
            asense_version: env!("CARGO_PKG_VERSION").to_string(),
            build_commit: build_commit()?,
            captured_at_utc,
            capture_duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            mode: ProbeMode::Passive,
            daemon: daemon.identity,
        },
        machine,
        power,
        drivers,
        profile,
        fans,
        platform,
        lighting,
        hid,
        privacy: passive_privacy(),
    };
    encode_report(&report)
}

fn encode_report(report: &ProbeReport) -> Result<String, String> {
    report.validate()?;
    let mut output = serde_json::to_string_pretty(report)
        .map_err(|error| format!("cannot encode capability report: {error}"))?;
    output.push('\n');
    if output.len() > schema::MAX_REPORT_BYTES {
        return Err("capability report exceeds the schema-3 byte bound".to_string());
    }
    Ok(output)
}

fn summarize_json(input: &str) -> Result<String, String> {
    let report: ProbeReport = serde_json::from_str(input)
        .map_err(|error| format!("cannot decode generated schema-3 report: {error}"))?;
    summarize_report(&report)
}

fn summarize_report(report: &ProbeReport) -> Result<String, String> {
    report.validate()?;

    let daemon = match &report.provenance.daemon {
        Observation::Value { value, .. } => {
            format!("protocol {} / version {}", value.protocol, value.version)
        }
        Observation::Absent { reason, .. } => format!("absent({})", absence_name(*reason)),
        Observation::Error { error, .. } => format!("error({})", error_name(error)),
    };
    let ac_online = report
        .power
        .ac
        .iter()
        .filter(|supply| matches!(&supply.online, Observation::Value { value: true, .. }))
        .count();
    let ac_offline = report
        .power
        .ac
        .iter()
        .filter(|supply| matches!(&supply.online, Observation::Value { value: false, .. }))
        .count();
    let ac_unknown = report.power.ac.len() - ac_online - ac_offline;
    let modules = report
        .drivers
        .modules
        .iter()
        .map(|module| {
            let name = match module.name {
                schema::ModuleName::AcerWmi => "acer_wmi",
                schema::ModuleName::AsenseRgb => "asense_rgb",
            };
            format!("{name}={}", module_state(module))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let profile_transport =
        backend_transport_name(&report.profile.transport, |backend| match backend {
            schema::ProfileBackend::Kernel => "kernel",
            schema::ProfileBackend::GamingWmi => "gaming-wmi",
        });
    let profile_current = match &report.profile.current {
        schema::ProfileCurrent::Known { profile, .. } => profile_name(*profile).to_string(),
        schema::ProfileCurrent::Unknown { raw, .. } => format!("unknown(raw={})", raw.value),
        schema::ProfileCurrent::Absent { reason, .. } => {
            format!("absent({})", absence_name(*reason))
        }
        schema::ProfileCurrent::Error { error, .. } => format!("error({})", error_name(error)),
    };
    let profile_choices = observation_count(&report.profile.choices);
    let fan_control = match &report.fans.control {
        schema::FanControl::Present { backend, modes, .. } => {
            let mut enabled = Vec::with_capacity(3);
            if modes.auto {
                enabled.push("auto");
            }
            if modes.manual {
                enabled.push("manual");
            }
            if modes.maximum {
                enabled.push("maximum");
            }
            let backend = match backend {
                schema::FanBackend::KernelPwm => "kernel-pwm",
                schema::FanBackend::GamingWmi => "gaming-wmi",
            };
            format!("{backend} modes={}", enabled.join(","))
        }
        schema::FanControl::Absent { reason, .. } => {
            format!("absent({})", absence_name(*reason))
        }
        schema::FanControl::Error { error, .. } => format!("error({})", error_name(error)),
    };
    let rpm_readable = report
        .fans
        .rpm
        .iter()
        .filter(|item| matches!(&item.read, Observation::Value { .. }))
        .count();
    let temperature_readable = report
        .fans
        .temperatures
        .iter()
        .filter(|item| matches!(&item.millidegrees_c, Observation::Value { .. }))
        .count();
    let platform_transport = backend_transport_name(&report.platform.transport, |_| "gaming-wmi");
    let (platform_values, platform_absent, platform_errors) = report.platform.fields.iter().fold(
        (0_usize, 0_usize, 0_usize),
        |(values, absent, errors), field| match &field.read {
            Observation::Value { .. } => (values + 1, absent, errors),
            Observation::Absent { .. } => (values, absent + 1, errors),
            Observation::Error { .. } => (values, absent, errors + 1),
        },
    );
    let platform_error_fields = report
        .platform
        .fields
        .iter()
        .filter(|field| matches!(&field.read, Observation::Error { .. }))
        .map(|field| platform_field_name(field.name))
        .collect::<Vec<_>>();
    let platform_error_fields = if platform_error_fields.is_empty() {
        "none".to_string()
    } else {
        platform_error_fields.join(",")
    };
    let lighting = if report.lighting.is_empty() {
        "none".to_string()
    } else {
        report
            .lighting
            .iter()
            .map(|item| {
                let backend = match item.backend {
                    schema::LightingBackend::ZonedWmi => "zoned-wmi",
                    schema::LightingBackend::Enek5130 => "enek5130",
                };
                let target = lighting_target_name(item.target);
                let state = if item.state_readable {
                    "readable"
                } else {
                    "unreadable"
                };
                format!(
                    "{}={backend}/{target}/{}-zone/state-{state}",
                    item.id, item.zones
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let hid = if report.hid.is_empty() {
        "none".to_string()
    } else {
        report
            .hid
            .iter()
            .map(|item| {
                let role = match item.role {
                    schema::HidRole::Enek5130Lighting => "enek5130-lighting",
                    schema::HidRole::AcerEcHidPowerCandidate => "acer-ec-hid-power-candidate",
                };
                format!("{role}={}:{}", item.identity.vid, item.identity.pid)
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let commit = report
        .provenance
        .build_commit
        .as_deref()
        .unwrap_or("not-embedded");
    let output = format!(
        "ASense compatibility probe summary\n\
Schema: {} | mode: {}\n\
Build: {} | commit: {}\n\
Capture: {} | duration: {} ms\n\
Machine: {} | BIOS: {}\n\
System: {} {} | kernel: {} {}\n\
Daemon: {}\n\
Power: AC online={} offline={} unknown={} | batteries={}\n\
Drivers: {} | WMI GUIDs={} | hwmon-owner={}\n\
Profile: transport={} | current={} | choices={}\n\
Fans: {} | RPM readable={}/{} | temperatures readable={}/{}\n\
Platform: transport={} | values={} absent={} errors={} | error-fields={}\n\
Lighting: {}\n\
HID: {}\n\
Privacy: automatic-upload={} | persistent-id={} | default-mutations={} | excluded-identity-classes={}\n\
Support: attach JSON from `asense probe > asense-probe.json`; capture behavior and probe with the same ASense build, boot and module state.\n",
        report.schema,
        probe_mode_name(report.provenance.mode),
        report.provenance.asense_version,
        commit,
        report.provenance.captured_at_utc,
        report.provenance.capture_duration_ms,
        observation_text(&report.machine.dmi.product),
        observation_text(&report.machine.dmi.bios),
        observation_text(&report.machine.os.id),
        observation_text(&report.machine.os.version_id),
        observation_text(&report.machine.kernel.release),
        observation_text(&report.machine.kernel.architecture),
        daemon,
        ac_online,
        ac_offline,
        ac_unknown,
        report.power.batteries.len(),
        modules,
        report.drivers.wmi.len(),
        observation_text(&report.drivers.hwmon_owner),
        profile_transport,
        profile_current,
        profile_choices,
        fan_control,
        rpm_readable,
        report.fans.rpm.len(),
        temperature_readable,
        report.fans.temperatures.len(),
        platform_transport,
        platform_values,
        platform_absent,
        platform_errors,
        platform_error_fields,
        lighting,
        hid,
        yes_no(report.privacy.automatic_upload),
        yes_no(report.privacy.persistent_report_id),
        report.privacy.default_mutations.len(),
        report.privacy.excluded.len(),
    );
    if output.len() > MAX_SUMMARY_BYTES {
        return Err("probe summary exceeds its byte bound".to_string());
    }
    Ok(output)
}

fn module_state(module: &schema::ModuleEvidence) -> String {
    match &module.loaded {
        Observation::Value { value: true, .. } => match &module.version {
            Observation::Value { value, .. } => format!("loaded({value})"),
            Observation::Absent { .. } => "loaded".to_string(),
            Observation::Error { error, .. } => {
                format!("loaded/version-error({})", error_name(error))
            }
        },
        Observation::Value { value: false, .. } => "not-loaded".to_string(),
        Observation::Absent { reason, .. } => format!("absent({})", absence_name(*reason)),
        Observation::Error { error, .. } => format!("error({})", error_name(error)),
    }
}

fn observation_text(value: &Observation<String>) -> String {
    match value {
        Observation::Value { value, .. } => value.clone(),
        Observation::Absent { reason, .. } => format!("absent({})", absence_name(*reason)),
        Observation::Error { error, .. } => format!("error({})", error_name(error)),
    }
}

fn observation_count<T>(value: &Observation<Vec<T>>) -> String {
    match value {
        Observation::Value { value, .. } => value.len().to_string(),
        Observation::Absent { reason, .. } => format!("absent({})", absence_name(*reason)),
        Observation::Error { error, .. } => format!("error({})", error_name(error)),
    }
}

fn backend_transport_name<T>(
    transport: &schema::BackendTransport<T>,
    present: impl FnOnce(&T) -> &'static str,
) -> String {
    match transport {
        schema::BackendTransport::Present { backend, .. } => present(backend).to_string(),
        schema::BackendTransport::Absent { reason, .. } => {
            format!("absent({})", absence_name(*reason))
        }
        schema::BackendTransport::Error { error, .. } => {
            format!("error({})", error_name(error))
        }
    }
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

const fn probe_mode_name(value: ProbeMode) -> &'static str {
    match value {
        ProbeMode::Passive => "passive",
        ProbeMode::ExtendedHid => "extended-hid",
    }
}

const fn profile_name(value: schema::ProfileName) -> &'static str {
    match value {
        schema::ProfileName::Quiet => "quiet",
        schema::ProfileName::Balanced => "balanced",
        schema::ProfileName::Performance => "performance",
        schema::ProfileName::Turbo => "turbo",
        schema::ProfileName::Eco => "eco",
    }
}

const fn lighting_target_name(value: schema::LightingTarget) -> &'static str {
    match value {
        schema::LightingTarget::Keyboard => "keyboard",
        schema::LightingTarget::CoverLogo => "cover-logo",
        schema::LightingTarget::RearLogo => "rear-logo",
        schema::LightingTarget::Lightbar => "lightbar",
    }
}

const fn platform_field_name(value: schema::PlatformField) -> &'static str {
    match value {
        schema::PlatformField::BatteryLimit => "battery-limit",
        schema::PlatformField::BatteryCalibration => "battery-calibration",
        schema::PlatformField::UsbOffCharging => "usb-off-charging",
        schema::PlatformField::KeyboardTimeout => "keyboard-timeout",
        schema::PlatformField::BootSound => "boot-sound",
        schema::PlatformField::LcdOverride => "lcd-override",
        schema::PlatformField::RearLogo => "rear-logo",
    }
}

const fn absence_name(value: AbsenceReason) -> &'static str {
    match value {
        AbsenceReason::NotExposed => "not-exposed",
        AbsenceReason::NotInstalled => "not-installed",
        AbsenceReason::NotApplicable => "not-applicable",
        AbsenceReason::DaemonUnavailable => "daemon-unavailable",
        AbsenceReason::IncompleteControlInterface => "incomplete-control-interface",
    }
}

fn error_name(error: &ProbeError) -> String {
    format!(
        "{}/{}",
        match error.stage {
            ErrorStage::Discover => "discover",
            ErrorStage::Open => "open",
            ErrorStage::Read => "read",
            ErrorStage::Decode => "decode",
            ErrorStage::Protocol => "protocol",
            ErrorStage::Verify => "verify",
        },
        match error.class {
            ErrorClass::NotFound => "not-found",
            ErrorClass::PermissionDenied => "permission-denied",
            ErrorClass::Io => "io",
            ErrorClass::Timeout => "timeout",
            ErrorClass::InvalidValue => "invalid-value",
            ErrorClass::Incompatible => "incompatible",
            ErrorClass::Unsupported => "unsupported",
            ErrorClass::Oversize => "oversize",
        }
    )
}

fn collect_daemon_context() -> DaemonContext {
    match ControlClient::connect() {
        Ok(mut client) => {
            let Some((protocol, version)) = client.negotiated_daemon() else {
                let error = ProbeError::new(ErrorStage::Protocol, ErrorClass::Incompatible, None);
                return DaemonContext {
                    identity: Observation::error(SourceId::ControlSocket, error.clone()),
                    diagnostics: DaemonDiagnostics::Error(error),
                };
            };
            let identity = Observation::value(
                SourceId::ControlSocket,
                DaemonIdentity {
                    protocol,
                    version: version.to_string(),
                },
            );
            let diagnostics = match client.passive_diagnostics() {
                Ok(diagnostics) => DaemonDiagnostics::Value(Box::new(diagnostics)),
                Err(error) => DaemonDiagnostics::Error(control_probe_error(&error)),
            };
            DaemonContext {
                identity,
                diagnostics,
            }
        }
        Err(error) => {
            let identity = daemon_error(error.clone());
            let diagnostics = match &identity {
                Observation::Absent { .. } => DaemonDiagnostics::Unavailable,
                Observation::Error { error, .. } => DaemonDiagnostics::Error(error.clone()),
                Observation::Value { .. } => unreachable!(),
            };
            DaemonContext {
                identity,
                diagnostics,
            }
        }
    }
}

fn control_probe_error(error: &ControlError) -> ProbeError {
    match error {
        ControlError::Transport(_) => ProbeError::new(ErrorStage::Protocol, ErrorClass::Io, None),
        ControlError::Timeout => ProbeError::new(ErrorStage::Protocol, ErrorClass::Timeout, None),
        ControlError::Protocol(_) | ControlError::CommandRejected(_) => {
            ProbeError::new(ErrorStage::Protocol, ErrorClass::Incompatible, None)
        }
        ControlError::InvalidRequest(_) => {
            ProbeError::new(ErrorStage::Protocol, ErrorClass::InvalidValue, None)
        }
    }
}

fn daemon_error(error: ControlError) -> Observation<DaemonIdentity> {
    match error {
        ControlError::Transport(_) if !Path::new(CONTROL_SOCKET).exists() => {
            Observation::absent(SourceId::ControlSocket, AbsenceReason::DaemonUnavailable)
        }
        ControlError::Transport(_) => Observation::error(
            SourceId::ControlSocket,
            ProbeError::new(ErrorStage::Open, ErrorClass::Io, None),
        ),
        ControlError::Timeout => Observation::error(
            SourceId::ControlSocket,
            ProbeError::new(ErrorStage::Protocol, ErrorClass::Timeout, None),
        ),
        ControlError::Protocol(_) | ControlError::CommandRejected(_) => Observation::error(
            SourceId::ControlSocket,
            ProbeError::new(ErrorStage::Protocol, ErrorClass::Incompatible, None),
        ),
        ControlError::InvalidRequest(_) => Observation::error(
            SourceId::ControlSocket,
            ProbeError::new(ErrorStage::Protocol, ErrorClass::InvalidValue, None),
        ),
    }
}

fn map_daemon_diagnostics(
    diagnostics: DaemonDiagnostics,
) -> (
    schema::ProfileEvidence,
    schema::FanEvidence,
    schema::PlatformEvidence,
    Vec<schema::LightingEvidence>,
    Vec<schema::HidEvidence>,
) {
    match diagnostics {
        DaemonDiagnostics::Value(diagnostics) => {
            let diagnostics = *diagnostics;
            (
                map_profile(diagnostics.profile),
                map_fans(diagnostics.fans),
                map_platform(diagnostics.platform),
                diagnostics.lighting.into_iter().map(map_lighting).collect(),
                diagnostics.hid.into_iter().map(map_hid).collect(),
            )
        }
        DaemonDiagnostics::Unavailable => (
            absent_profile(),
            absent_fans(),
            absent_platform(),
            Vec::new(),
            Vec::new(),
        ),
        DaemonDiagnostics::Error(error) => (
            diagnostic_error_profile(error.clone()),
            diagnostic_error_fans(error.clone()),
            diagnostic_error_platform(error),
            Vec::new(),
            Vec::new(),
        ),
    }
}

fn map_lighting(light: passive::DiagnosticLighting) -> schema::LightingEvidence {
    schema::LightingEvidence {
        id: light.id,
        backend: match light.backend {
            passive::DiagnosticLightingBackend::ZonedWmi => schema::LightingBackend::ZonedWmi,
        },
        target: match light.target {
            passive::DiagnosticLightingTarget::Keyboard => schema::LightingTarget::Keyboard,
        },
        zones: light.zones,
        modes: schema::LightingModes {
            static_color: light.modes.static_color,
            brightness: light.modes.brightness,
            breathing: light.modes.breathing,
            neon: light.modes.neon,
        },
        state_readable: light.state_readable,
        authority: schema::LightingAuthority::TransportCapability,
    }
}

fn map_hid(hid: crate::passive_hid::DiagnosticHid) -> schema::HidEvidence {
    let role = match hid.role {
        crate::passive_hid::DiagnosticHidRole::Enek5130Lighting => {
            schema::HidRole::Enek5130Lighting
        }
        crate::passive_hid::DiagnosticHidRole::AcerEcHidPowerCandidate => {
            schema::HidRole::AcerEcHidPowerCandidate
        }
    };
    let identity = schema::HidIdentity {
        bus: match hid.identity.bus {
            crate::passive_hid::DiagnosticHidBus::I2c => schema::HidBus::I2c,
        },
        vid: format!("{:04x}", hid.identity.vid),
        pid: format!("{:04x}", hid.identity.pid),
        name: match hid.identity.name {
            crate::passive_hid::DiagnosticHidName::Enek5130 => schema::HidName::Enek5130,
            crate::passive_hid::DiagnosticHidName::AcerEcHid => schema::HidName::AcerEcHid,
        },
        interface: hid.identity.interface,
        usage_page: hid.identity.usage_page.map(|value| format!("{value:04x}")),
        usage: hid.identity.usage.map(|value| format!("{value:04x}")),
    };
    let driver = map_observation(hid.driver, SourceId::HidDriver, |value| value);
    let descriptor = map_observation(
        hid.descriptor,
        SourceId::HidReportDescriptor,
        |descriptor| schema::HidDescriptor {
            bytes: descriptor.bytes,
            sha256: descriptor.sha256,
            feature_reports: descriptor
                .feature_reports
                .into_iter()
                .map(|report| schema::HidFeatureGeometry {
                    id: format!("{:02x}", report.id),
                    bytes: report.bytes,
                })
                .collect(),
        },
    );
    let a1 = hid.a1.map(|a1| {
        map_observation(a1, SourceId::HidFeatureA1, |a1| schema::HidA1 {
            requested_bytes: a1.requested_bytes,
            returned_bytes: a1.returned_bytes,
            payload: schema::RawValue {
                encoding: schema::RawEncoding::Hex,
                bytes: a1.returned_bytes,
                value: a1.payload_hex,
            },
            targets: a1
                .targets
                .into_iter()
                .map(|target| format!("{target:02x}"))
                .collect(),
        })
    });
    schema::HidEvidence {
        role,
        identity,
        driver,
        descriptor,
        a1,
        extended: schema::HidExtended {
            requested: false,
            selectors: Vec::new(),
            a3: Vec::new(),
        },
    }
}

fn map_profile(profile: passive::DiagnosticProfile) -> schema::ProfileEvidence {
    let current_source = match &profile.transport {
        passive::DiagnosticObservation::Value { value } => map_source(value.source),
        _ => SourceId::ProfileDiscovery,
    };
    let choices_source = match &profile.transport {
        passive::DiagnosticObservation::Value { value }
            if value.backend == passive::DiagnosticProfileBackend::GamingWmi =>
        {
            SourceId::KnownGamingWmiCommands
        }
        passive::DiagnosticObservation::Value { value } => map_source(value.source),
        _ => SourceId::ProfileDiscovery,
    };
    let transport = match profile.transport {
        passive::DiagnosticObservation::Value { value } => schema::BackendTransport::Present {
            source: map_source(value.source),
            backend: match value.backend {
                passive::DiagnosticProfileBackend::Kernel => schema::ProfileBackend::Kernel,
                passive::DiagnosticProfileBackend::GamingWmi => schema::ProfileBackend::GamingWmi,
            },
        },
        passive::DiagnosticObservation::Absent { reason } => schema::BackendTransport::Absent {
            source: SourceId::ProfileDiscovery,
            reason: map_absence(reason),
        },
        passive::DiagnosticObservation::Error { error } => schema::BackendTransport::Error {
            source: SourceId::ProfileDiscovery,
            error: map_diagnostic_error(error),
        },
    };
    let current = match profile.current {
        passive::DiagnosticObservation::Value { value } => {
            let raw = map_raw(value.raw);
            match value.profile {
                None => schema::ProfileCurrent::Unknown {
                    source: map_source(value.source),
                    raw,
                },
                Some(profile) => schema::ProfileCurrent::Known {
                    source: map_source(value.source),
                    raw,
                    profile: map_profile_name(profile),
                },
            }
        }
        passive::DiagnosticObservation::Absent { reason } => schema::ProfileCurrent::Absent {
            source: current_source,
            reason: map_absence(reason),
        },
        passive::DiagnosticObservation::Error { error } => schema::ProfileCurrent::Error {
            source: current_source,
            error: map_diagnostic_error(error),
        },
    };
    let choices = map_observation(profile.choices, choices_source, |choices| {
        choices
            .into_iter()
            .map(|choice| schema::ProfileChoice {
                command: choice.command,
                profile: map_profile_name(choice.profile),
                transport_raw: choice.transport_raw.map(map_raw),
                selectable: choice.selectable,
            })
            .collect()
    });
    schema::ProfileEvidence {
        transport,
        current,
        choices,
        firmware_supported_bitmap: Observation::absent(
            SourceId::GamingWmiSupportedProfiles,
            AbsenceReason::NotExposed,
        ),
        physical_effect: schema::PhysicalEffect::Unverified,
    }
}

fn map_fans(fans: passive::DiagnosticFans) -> schema::FanEvidence {
    let control = match fans.control {
        passive::DiagnosticObservation::Value { value } => schema::FanControl::Present {
            source: map_source(value.source),
            backend: match value.backend {
                passive::DiagnosticFanBackend::KernelPwm => schema::FanBackend::KernelPwm,
                passive::DiagnosticFanBackend::GamingWmi => schema::FanBackend::GamingWmi,
            },
            modes: schema::FanModes {
                auto: value.modes.auto,
                manual: value.modes.manual,
                maximum: value.modes.maximum,
            },
        },
        passive::DiagnosticObservation::Absent { reason } => schema::FanControl::Absent {
            source: SourceId::FanDiscovery,
            reason: map_absence(reason),
        },
        passive::DiagnosticObservation::Error { error } => schema::FanControl::Error {
            source: SourceId::FanDiscovery,
            error: map_diagnostic_error(error),
        },
    };
    let pwm = fans
        .channels
        .into_iter()
        .map(|channel| {
            let source = map_source(channel.source);
            schema::PwmEvidence {
                channel: channel.channel,
                setpoint_unit: match channel.setpoint_unit {
                    passive::DiagnosticSetpointUnit::Pwm255 => schema::PwmSetpointUnit::Pwm255,
                    passive::DiagnosticSetpointUnit::Percent => schema::PwmSetpointUnit::Percent,
                },
                setpoint: map_observation(channel.setpoint, source, |value| value),
                mode: map_observation(channel.mode, source, |mode| match mode {
                    passive::DiagnosticFanMode::Maximum => schema::PwmMode::Maximum,
                    passive::DiagnosticFanMode::Manual => schema::PwmMode::Manual,
                    passive::DiagnosticFanMode::Auto => schema::PwmMode::Auto,
                }),
            }
        })
        .collect();
    let rpm = fans
        .rpm
        .into_iter()
        .map(|rpm| schema::RpmEvidence {
            channel: rpm.channel,
            label: rpm.label,
            read: map_observation(rpm.read, SourceId::AcerHwmon, |value| value),
        })
        .collect();
    let temperatures = fans
        .temperatures
        .into_iter()
        .map(|temperature| schema::TemperatureEvidence {
            channel: temperature.channel,
            label: temperature.label,
            millidegrees_c: map_observation(temperature.read, SourceId::AcerHwmon, |value| value),
        })
        .collect();
    schema::FanEvidence {
        control,
        rpm,
        pwm,
        temperatures,
    }
}

fn map_platform(platform: passive::DiagnosticPlatform) -> schema::PlatformEvidence {
    let transport = match platform.transport {
        passive::DiagnosticObservation::Value { value } => schema::BackendTransport::Present {
            source: map_source(value.source),
            backend: schema::PlatformBackend::GamingWmi,
        },
        passive::DiagnosticObservation::Absent { reason } => schema::BackendTransport::Absent {
            source: SourceId::PlatformDiscovery,
            reason: map_absence(reason),
        },
        passive::DiagnosticObservation::Error { error } => schema::BackendTransport::Error {
            source: SourceId::PlatformDiscovery,
            error: map_diagnostic_error(error),
        },
    };
    let fields = platform
        .fields
        .into_iter()
        .map(|field| {
            let source = map_source(field.source);
            schema::PlatformFieldEvidence {
                name: map_platform_field(field.name),
                expected: field.expected,
                exposed: field.exposed,
                source,
                read: map_observation(field.read, source, |value| match value {
                    passive::DiagnosticPlatformValue::Bool { value } => {
                        schema::PlatformValue::Bool { value }
                    }
                    passive::DiagnosticPlatformValue::UsbThreshold { value } => {
                        schema::PlatformValue::UsbThreshold { value }
                    }
                    passive::DiagnosticPlatformValue::RearLogo {
                        enabled,
                        brightness,
                        color,
                    } => schema::PlatformValue::RearLogo {
                        enabled,
                        brightness,
                        color,
                    },
                }),
            }
        })
        .collect();
    schema::PlatformEvidence { transport, fields }
}

fn map_observation<T, U>(
    observation: passive::DiagnosticObservation<T>,
    source: SourceId,
    map: impl FnOnce(T) -> U,
) -> Observation<U> {
    match observation {
        passive::DiagnosticObservation::Value { value } => Observation::value(source, map(value)),
        passive::DiagnosticObservation::Absent { reason } => {
            Observation::absent(source, map_absence(reason))
        }
        passive::DiagnosticObservation::Error { error } => {
            Observation::error(source, map_diagnostic_error(error))
        }
    }
}

fn map_source(source: passive::DiagnosticSource) -> SourceId {
    match source {
        passive::DiagnosticSource::KernelPlatformProfile => SourceId::KernelPlatformProfile,
        passive::DiagnosticSource::GamingWmiProfile => SourceId::GamingWmiProfile,
        passive::DiagnosticSource::KnownGamingWmiCommands => SourceId::KnownGamingWmiCommands,
        passive::DiagnosticSource::AcerHwmon => SourceId::AcerHwmon,
        passive::DiagnosticSource::GamingWmiFan => SourceId::GamingWmiFan,
        passive::DiagnosticSource::PlatformDiscovery => SourceId::PlatformDiscovery,
        passive::DiagnosticSource::AsenseRgb => SourceId::AsenseRgb,
        passive::DiagnosticSource::AsenseBattery => SourceId::AsenseBattery,
        passive::DiagnosticSource::AsenseApge => SourceId::AsenseApge,
    }
}

fn map_absence(reason: passive::DiagnosticAbsence) -> AbsenceReason {
    match reason {
        passive::DiagnosticAbsence::NotExposed => AbsenceReason::NotExposed,
        passive::DiagnosticAbsence::IncompleteInterface => {
            AbsenceReason::IncompleteControlInterface
        }
        passive::DiagnosticAbsence::NotApplicable => AbsenceReason::NotApplicable,
    }
}

fn map_diagnostic_error(error: passive::DiagnosticError) -> ProbeError {
    ProbeError {
        stage: match error.stage {
            passive::DiagnosticErrorStage::Discover => ErrorStage::Discover,
            passive::DiagnosticErrorStage::Open => ErrorStage::Open,
            passive::DiagnosticErrorStage::Read => ErrorStage::Read,
            passive::DiagnosticErrorStage::Decode => ErrorStage::Decode,
        },
        class: match error.class {
            passive::DiagnosticErrorClass::NotFound => ErrorClass::NotFound,
            passive::DiagnosticErrorClass::PermissionDenied => ErrorClass::PermissionDenied,
            passive::DiagnosticErrorClass::Io => ErrorClass::Io,
            passive::DiagnosticErrorClass::InvalidValue => ErrorClass::InvalidValue,
            passive::DiagnosticErrorClass::Oversize => ErrorClass::Oversize,
        },
        errno: error.errno,
        raw: error.raw.map(map_raw),
    }
}

fn map_raw(raw: passive::DiagnosticRaw) -> schema::RawValue {
    schema::RawValue {
        encoding: match raw.encoding {
            passive::DiagnosticRawEncoding::U8Hex => schema::RawEncoding::U8Hex,
            passive::DiagnosticRawEncoding::AsciiToken => schema::RawEncoding::AsciiToken,
            passive::DiagnosticRawEncoding::ScalarHex => schema::RawEncoding::ScalarHex,
        },
        bytes: raw.bytes,
        value: raw.value,
    }
}

fn map_profile_name(profile: passive::DiagnosticProfileName) -> schema::ProfileName {
    match profile {
        passive::DiagnosticProfileName::Quiet => schema::ProfileName::Quiet,
        passive::DiagnosticProfileName::Balanced => schema::ProfileName::Balanced,
        passive::DiagnosticProfileName::Performance => schema::ProfileName::Performance,
        passive::DiagnosticProfileName::Turbo => schema::ProfileName::Turbo,
        passive::DiagnosticProfileName::Eco => schema::ProfileName::Eco,
    }
}

fn map_platform_field(field: passive::DiagnosticPlatformFieldName) -> schema::PlatformField {
    match field {
        passive::DiagnosticPlatformFieldName::BatteryLimit => schema::PlatformField::BatteryLimit,
        passive::DiagnosticPlatformFieldName::BatteryCalibration => {
            schema::PlatformField::BatteryCalibration
        }
        passive::DiagnosticPlatformFieldName::UsbOffCharging => {
            schema::PlatformField::UsbOffCharging
        }
        passive::DiagnosticPlatformFieldName::KeyboardTimeout => {
            schema::PlatformField::KeyboardTimeout
        }
        passive::DiagnosticPlatformFieldName::BootSound => schema::PlatformField::BootSound,
        passive::DiagnosticPlatformFieldName::LcdOverride => schema::PlatformField::LcdOverride,
        passive::DiagnosticPlatformFieldName::RearLogo => schema::PlatformField::RearLogo,
    }
}

fn diagnostic_error_profile(error: ProbeError) -> schema::ProfileEvidence {
    schema::ProfileEvidence {
        transport: schema::BackendTransport::Error {
            source: SourceId::ProfileDiscovery,
            error: error.clone(),
        },
        current: schema::ProfileCurrent::Error {
            source: SourceId::ProfileDiscovery,
            error: error.clone(),
        },
        choices: Observation::error(SourceId::ProfileDiscovery, error.clone()),
        firmware_supported_bitmap: Observation::error(SourceId::GamingWmiSupportedProfiles, error),
        physical_effect: schema::PhysicalEffect::Unverified,
    }
}

fn diagnostic_error_fans(error: ProbeError) -> schema::FanEvidence {
    schema::FanEvidence {
        control: schema::FanControl::Error {
            source: SourceId::FanDiscovery,
            error,
        },
        rpm: Vec::new(),
        pwm: Vec::new(),
        temperatures: Vec::new(),
    }
}

fn diagnostic_error_platform(error: ProbeError) -> schema::PlatformEvidence {
    let fields = [
        (schema::PlatformField::BatteryLimit, SourceId::AsenseBattery),
        (
            schema::PlatformField::BatteryCalibration,
            SourceId::AsenseBattery,
        ),
        (schema::PlatformField::UsbOffCharging, SourceId::AsenseApge),
        (schema::PlatformField::KeyboardTimeout, SourceId::AsenseApge),
        (schema::PlatformField::BootSound, SourceId::AsenseRgb),
        (schema::PlatformField::LcdOverride, SourceId::AsenseRgb),
        (schema::PlatformField::RearLogo, SourceId::AsenseRgb),
    ]
    .into_iter()
    .map(|(name, source)| schema::PlatformFieldEvidence {
        name,
        expected: false,
        exposed: false,
        source,
        read: Observation::error(source, error.clone()),
    })
    .collect();
    schema::PlatformEvidence {
        transport: schema::BackendTransport::Error {
            source: SourceId::PlatformDiscovery,
            error,
        },
        fields,
    }
}

fn collect_machine(root: &Path) -> MachineEvidence {
    let dmi_root = rooted(root, "sys/class/dmi/id");
    let (release, architecture) = collect_uname();
    let (os_id, os_version) = collect_os_release(root);
    MachineEvidence {
        dmi: DmiEvidence {
            vendor: read_text_observation(&dmi_root.join("sys_vendor"), SourceId::DmiVendor),
            product: read_text_observation(&dmi_root.join("product_name"), SourceId::DmiProduct),
            board: read_text_observation(&dmi_root.join("board_name"), SourceId::DmiBoard),
            bios: read_text_observation(&dmi_root.join("bios_version"), SourceId::DmiBios),
        },
        kernel: KernelEvidence {
            release,
            architecture,
        },
        os: OsEvidence {
            id: os_id,
            version_id: os_version,
        },
    }
}

fn collect_uname() -> (Observation<String>, Observation<String>) {
    let mut value = MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: `uname` initializes the complete utsname on success. We only
    // assume initialization after a zero return code.
    if unsafe { libc::uname(value.as_mut_ptr()) } != 0 {
        let error = io_probe_error(ErrorStage::Read, &std::io::Error::last_os_error());
        return (
            Observation::error(SourceId::Uname, error.clone()),
            Observation::error(SourceId::Uname, error),
        );
    }
    // SAFETY: established by the successful `uname` call above.
    let value = unsafe { value.assume_init() };
    (
        c_text_observation(value.release.as_ptr(), SourceId::Uname),
        c_text_observation(value.machine.as_ptr(), SourceId::Uname),
    )
}

fn c_text_observation(pointer: *const libc::c_char, source: SourceId) -> Observation<String> {
    // SAFETY: fields in a successful libc utsname are NUL-terminated arrays.
    let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
    decode_observation(bytes, source)
}

fn collect_os_release(root: &Path) -> (Observation<String>, Observation<String>) {
    let path = rooted(root, "etc/os-release");
    let bytes = match read_bounded_bytes(&path, MAX_INPUT_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return (
                Observation::absent(SourceId::OsRelease, AbsenceReason::NotExposed),
                Observation::absent(SourceId::OsRelease, AbsenceReason::NotExposed),
            );
        }
        Err(error) => {
            return (
                Observation::error(SourceId::OsRelease, error.clone()),
                Observation::error(SourceId::OsRelease, error),
            );
        }
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => {
            let error = ProbeError::new(ErrorStage::Decode, ErrorClass::InvalidValue, None);
            return (
                Observation::error(SourceId::OsRelease, error.clone()),
                Observation::error(SourceId::OsRelease, error),
            );
        }
    };
    (
        os_release_value(text, "ID"),
        os_release_value(text, "VERSION_ID"),
    )
}

fn os_release_value(input: &str, key: &str) -> Observation<String> {
    let value = input.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate == key).then_some(value)
    });
    let Some(value) = value else {
        return Observation::absent(SourceId::OsRelease, AbsenceReason::NotExposed);
    };
    match decode_os_release_value(value) {
        Ok(value) if value.is_empty() => {
            Observation::absent(SourceId::OsRelease, AbsenceReason::NotExposed)
        }
        Ok(value) if value.len() <= schema::MAX_TEXT_BYTES => {
            Observation::value(SourceId::OsRelease, value)
        }
        Ok(_) => Observation::error(
            SourceId::OsRelease,
            ProbeError::new(ErrorStage::Decode, ErrorClass::Oversize, None),
        ),
        Err(()) => Observation::error(
            SourceId::OsRelease,
            ProbeError::new(ErrorStage::Decode, ErrorClass::InvalidValue, None),
        ),
    }
}

fn decode_os_release_value(value: &str) -> Result<String, ()> {
    let value = value.trim();
    let (body, quoted) = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        (&value[1..value.len() - 1], true)
    } else {
        (value, false)
    };
    if !quoted && body.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(());
    }
    let mut output = String::with_capacity(body.len());
    let mut escaped = false;
    for character in body.chars() {
        if escaped {
            output.push(character);
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else {
            output.push(character);
        }
    }
    if escaped || output.chars().any(char::is_control) {
        return Err(());
    }
    Ok(output)
}

fn collect_power(root: &Path) -> Result<PowerEvidence, String> {
    let directory = rooted(root, "sys/class/power_supply");
    let entries = match sorted_entries_bounded(&directory, MAX_POWER_DIRECTORY_ENTRIES) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(format!(
                "cannot enumerate the bounded power-supply inventory: {:?}",
                io_error_class(&error)
            ));
        }
    };
    let mut ac = Vec::new();
    let mut batteries = Vec::new();
    for path in entries {
        let kind = match read_text_observation(&path.join("type"), SourceId::PowerSupply) {
            Observation::Value { value, .. } => value,
            Observation::Absent { .. } => continue,
            Observation::Error { .. } => {
                return Err("a power-supply type could not be read safely".to_string());
            }
        };
        match kind.as_str() {
            "Mains" | "USB" | "USB_C" | "USB_PD" if ac.len() < schema::MAX_POWER_ITEMS => {
                let kind = match kind.as_str() {
                    "Mains" => AcKind::Mains,
                    "USB" => AcKind::Usb,
                    "USB_C" => AcKind::UsbC,
                    "USB_PD" => AcKind::UsbPd,
                    _ => unreachable!(),
                };
                ac.push(AcEvidence {
                    ordinal: ac.len(),
                    kind,
                    online: read_bool_observation(&path.join("online"), SourceId::PowerSupply),
                });
            }
            "Battery" if batteries.len() < schema::MAX_POWER_ITEMS => {
                batteries.push(BatteryEvidence {
                    ordinal: batteries.len(),
                    present: read_bool_observation(&path.join("present"), SourceId::PowerSupply),
                    status: read_battery_status(&path.join("status")),
                    capacity_percent: read_capacity(&path.join("capacity")),
                });
            }
            _ => {}
        }
    }
    Ok(PowerEvidence { ac, batteries })
}

fn read_bool_observation(path: &Path, source: SourceId) -> Observation<bool> {
    map_text_observation(read_text_observation(path, source), |value| {
        match value.as_str() {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(ProbeError::new(
                ErrorStage::Decode,
                ErrorClass::InvalidValue,
                None,
            )),
        }
    })
}

fn read_battery_status(path: &Path) -> Observation<BatteryStatus> {
    map_text_observation(
        read_text_observation(path, SourceId::PowerSupply),
        |value| {
            Ok(match value.as_str() {
                "Charging" => BatteryStatus::Charging,
                "Discharging" => BatteryStatus::Discharging,
                "Full" => BatteryStatus::Full,
                "Not charging" => BatteryStatus::NotCharging,
                _ => BatteryStatus::Unknown,
            })
        },
    )
}

fn read_capacity(path: &Path) -> Observation<u8> {
    map_text_observation(
        read_text_observation(path, SourceId::PowerSupply),
        |value| {
            value
                .parse::<u8>()
                .ok()
                .filter(|value| *value <= 100)
                .ok_or_else(|| ProbeError::new(ErrorStage::Decode, ErrorClass::InvalidValue, None))
        },
    )
}

fn map_text_observation<T>(
    input: Observation<String>,
    convert: impl FnOnce(String) -> Result<T, ProbeError>,
) -> Observation<T> {
    match input {
        Observation::Value { source, value } => match convert(value) {
            Ok(value) => Observation::value(source, value),
            Err(error) => Observation::error(source, error),
        },
        Observation::Absent { source, reason } => Observation::absent(source, reason),
        Observation::Error { source, error } => Observation::error(source, error),
    }
}

fn collect_drivers(root: &Path) -> Result<DriverEvidence, String> {
    let modules = [ModuleName::AcerWmi, ModuleName::AsenseRgb]
        .into_iter()
        .map(|name| collect_module(root, name))
        .collect();
    Ok(DriverEvidence {
        modules,
        wmi: collect_wmi(root)?,
        hwmon_owner: collect_hwmon_owner(root),
    })
}

fn collect_module(root: &Path, name: ModuleName) -> ModuleEvidence {
    let token = match name {
        ModuleName::AcerWmi => "acer_wmi",
        ModuleName::AsenseRgb => "asense_rgb",
    };
    let path = rooted(root, "sys/module").join(token);
    let loaded = match fs::metadata(&path) {
        Ok(_) => Observation::value(SourceId::ModuleSysfs, true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Observation::value(SourceId::ModuleSysfs, false)
        }
        Err(error) => Observation::error(
            SourceId::ModuleSysfs,
            io_probe_error(ErrorStage::Discover, &error),
        ),
    };
    let version = if matches!(loaded, Observation::Value { value: true, .. }) {
        read_text_observation(&path.join("version"), SourceId::ModuleSysfs)
    } else {
        Observation::absent(SourceId::ModuleSysfs, AbsenceReason::NotExposed)
    };
    ModuleEvidence {
        name,
        loaded,
        version,
    }
}

fn collect_wmi(root: &Path) -> Result<Vec<WmiEvidence>, String> {
    let directory = rooted(root, "sys/bus/wmi/devices");
    let entries = match sorted_entries_bounded(&directory, 256) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "cannot enumerate the bounded WMI inventory: {:?}",
                io_error_class(&error)
            ));
        }
    };
    let authorities = [
        (
            GAMING_GUID,
            &[
                ("asense_diagnostics", WmiGroup::AsenseDiagnostics),
                ("asense_rgb", WmiGroup::AsenseRgb),
                ("rgb_zoned", WmiGroup::RgbZoned),
                ("gaming_fan", WmiGroup::GamingFan),
                ("gaming_profile", WmiGroup::GamingProfile),
            ][..],
        ),
        (
            BATTERY_GUID,
            &[
                ("asense_diagnostics", WmiGroup::AsenseDiagnostics),
                ("asense_battery", WmiGroup::AsenseBattery),
            ][..],
        ),
        (
            APGE_GUID,
            &[
                ("asense_diagnostics", WmiGroup::AsenseDiagnostics),
                ("asense_apge", WmiGroup::AsenseApge),
            ][..],
        ),
    ];
    let mut evidence = Vec::new();
    for (guid, known_groups) in authorities {
        let mut instances = entries
            .iter()
            .filter(|path| wmi_name_matches(path, guid))
            .cloned()
            .collect::<Vec<_>>();
        if instances.is_empty() {
            continue;
        }
        if instances.len() > schema::MAX_ITEMS {
            return Err(format!("WMI GUID {guid} exceeds the instance bound"));
        }
        instances.sort();
        let owner = combine_owners(
            instances
                .iter()
                .map(|path| read_driver_owner(path, SourceId::WmiDriver))
                .collect(),
            SourceId::WmiDriver,
        );
        let mut groups = known_groups
            .iter()
            .filter_map(|(name, group)| {
                instances
                    .iter()
                    .any(|path| path.join(name).is_dir())
                    .then_some(*group)
            })
            .collect::<Vec<_>>();
        groups.sort();
        groups.dedup();
        evidence.push(WmiEvidence {
            guid: guid.to_string(),
            instances: instances.len(),
            owner,
            groups,
        });
    }
    Ok(evidence)
}

fn wmi_name_matches(path: &Path, guid: &str) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let upper = name.to_ascii_uppercase();
    upper == guid
        || upper.strip_prefix(guid).is_some_and(|suffix| {
            suffix.starts_with('-') && suffix[1..].bytes().all(|b| b.is_ascii_digit())
        })
}

fn collect_hwmon_owner(root: &Path) -> Observation<String> {
    let Some(hwmon) = discover_acer_hwmon(root) else {
        return Observation::absent(SourceId::HwmonDriver, AbsenceReason::NotExposed);
    };
    let owner = read_driver_owner(&hwmon.join("device"), SourceId::HwmonDriver);
    match owner {
        Observation::Absent { .. } => map_text_observation(
            read_text_observation(&hwmon.join("name"), SourceId::HwmonDriver),
            |name| Ok(normalize_driver_token(&name)),
        ),
        owner => owner,
    }
}

fn read_driver_owner(path: &Path, source: SourceId) -> Observation<String> {
    for candidate in [path.join("driver"), path.join("driver/module")] {
        match fs::read_link(&candidate) {
            Ok(target) => {
                let Some(name) = target.file_name() else {
                    return Observation::error(
                        source,
                        ProbeError::new(ErrorStage::Decode, ErrorClass::InvalidValue, None),
                    );
                };
                let Some(name) = name.to_str() else {
                    return Observation::error(
                        source,
                        ProbeError::new(ErrorStage::Decode, ErrorClass::InvalidValue, None),
                    );
                };
                let token = normalize_driver_token(name);
                if token.is_empty() || token.len() > schema::MAX_TEXT_BYTES {
                    return Observation::error(
                        source,
                        ProbeError::new(ErrorStage::Decode, ErrorClass::Oversize, None),
                    );
                }
                return Observation::value(source, token);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Observation::error(source, io_probe_error(ErrorStage::Read, &error));
            }
        }
    }
    Observation::absent(source, AbsenceReason::NotExposed)
}

fn combine_owners(owners: Vec<Observation<String>>, source: SourceId) -> Observation<String> {
    let mut value: Option<String> = None;
    for owner in owners {
        match owner {
            Observation::Value { value: owner, .. } => {
                if value.as_ref().is_some_and(|current| current != &owner) {
                    return Observation::error(
                        source,
                        ProbeError::new(ErrorStage::Verify, ErrorClass::InvalidValue, None),
                    );
                }
                value = Some(owner);
            }
            Observation::Error { error, .. } => return Observation::error(source, error),
            Observation::Absent { .. } => {}
        }
    }
    value.map_or_else(
        || Observation::absent(source, AbsenceReason::NotExposed),
        |value| Observation::value(source, value),
    )
}

fn normalize_driver_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn read_text_observation(path: &Path, source: SourceId) -> Observation<String> {
    match read_bounded_bytes(path, schema::MAX_TEXT_BYTES) {
        Ok(Some(bytes)) => decode_observation(&bytes, source),
        Ok(None) => Observation::absent(source, AbsenceReason::NotExposed),
        Err(error) => Observation::error(source, error),
    }
}

fn decode_observation(bytes: &[u8], source: SourceId) -> Observation<String> {
    let value = match std::str::from_utf8(bytes) {
        Ok(value) => value.trim(),
        Err(_) => {
            return Observation::error(
                source,
                ProbeError::new(ErrorStage::Decode, ErrorClass::InvalidValue, None),
            );
        }
    };
    if value.is_empty() {
        Observation::absent(source, AbsenceReason::NotExposed)
    } else if value.len() > schema::MAX_TEXT_BYTES {
        Observation::error(
            source,
            ProbeError::new(ErrorStage::Decode, ErrorClass::Oversize, None),
        )
    } else if value.chars().any(char::is_control) {
        Observation::error(
            source,
            ProbeError::new(ErrorStage::Decode, ErrorClass::InvalidValue, None),
        )
    } else {
        Observation::value(source, value.to_string())
    }
}

fn read_bounded_bytes(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, ProbeError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_probe_error(ErrorStage::Open, &error)),
    };
    let mut bytes = Vec::new();
    file.take(u64::try_from(maximum + 1).expect("probe input bound fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|error| io_probe_error(ErrorStage::Read, &error))?;
    if bytes.len() > maximum {
        return Err(ProbeError::new(
            ErrorStage::Read,
            ErrorClass::Oversize,
            None,
        ));
    }
    Ok(Some(bytes))
}

fn io_probe_error(stage: ErrorStage, error: &std::io::Error) -> ProbeError {
    ProbeError::new(
        stage,
        io_error_class(error),
        error.raw_os_error().filter(|v| *v >= 0),
    )
}

fn io_error_class(error: &std::io::Error) -> ErrorClass {
    match error.kind() {
        std::io::ErrorKind::NotFound => ErrorClass::NotFound,
        std::io::ErrorKind::PermissionDenied => ErrorClass::PermissionDenied,
        std::io::ErrorKind::TimedOut => ErrorClass::Timeout,
        _ => ErrorClass::Io,
    }
}

fn sorted_entries_bounded(path: &Path, maximum: usize) -> std::io::Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        entries.push(entry?.path());
        if entries.len() > maximum {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bounded directory inventory exceeded",
            ));
        }
    }
    entries.sort_by(|left, right| {
        left.file_name()
            .map(|name| name.as_bytes())
            .cmp(&right.file_name().map(|name| name.as_bytes()))
    });
    Ok(entries)
}

fn build_commit() -> Result<Option<String>, String> {
    let Some(commit) = option_env!("ASENSE_BUILD_COMMIT") else {
        return Ok(None);
    };
    if commit.len() == 40
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(Some(commit.to_string()))
    } else {
        Err("ASENSE_BUILD_COMMIT must be lowercase 40-hex when set".to_string())
    }
}

fn captured_at_utc() -> Result<String, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock predates the Unix epoch".to_string())?
        .as_secs();
    let seconds = libc::time_t::try_from(seconds)
        .map_err(|_| "system timestamp does not fit time_t".to_string())?;
    let mut broken_down = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers are valid for the duration of the call. A non-null
    // result initializes the complete `tm` value.
    if unsafe { libc::gmtime_r(&seconds, broken_down.as_mut_ptr()) }.is_null() {
        return Err("cannot convert system time to UTC".to_string());
    }
    // SAFETY: established by the successful gmtime_r call above.
    let value = unsafe { broken_down.assume_init() };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        value.tm_year + 1900,
        value.tm_mon + 1,
        value.tm_mday,
        value.tm_hour,
        value.tm_min,
        value.tm_sec
    ))
}

fn rooted(root: &Path, relative: &str) -> PathBuf {
    root.join(relative.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("asense-probe3-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, name: &str, value: impl AsRef<[u8]>) {
            let path = rooted(&self.0, name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }

        fn directory(&self, name: &str) -> PathBuf {
            let path = rooted(&self.0, name);
            fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn frozen_schema_three_fixtures_reopen_validate_and_round_trip() {
        for (name, input) in [
            (
                "known",
                include_str!("../tests/fixtures/probe-v3/known.json"),
            ),
            (
                "unknown-error",
                include_str!("../tests/fixtures/probe-v3/unknown-error.json"),
            ),
            (
                "profile-read-error",
                include_str!("../tests/fixtures/probe-v3/profile-read-error.json"),
            ),
            (
                "absent-rpm-only",
                include_str!("../tests/fixtures/probe-v3/absent-rpm-only.json"),
            ),
        ] {
            let report: ProbeReport = serde_json::from_str(input)
                .unwrap_or_else(|error| panic!("fixture {name} does not deserialize: {error}"));
            report
                .validate()
                .unwrap_or_else(|error| panic!("fixture {name} is invalid: {error}"));
            let encoded = serde_json::to_string_pretty(&report).unwrap();
            let reopened: ProbeReport = serde_json::from_str(&encoded).unwrap();
            assert_eq!(reopened, report, "fixture {name} changed after reopen");
            assert_eq!(serde_json::to_string_pretty(&reopened).unwrap(), encoded);
        }
    }

    #[test]
    fn human_summary_is_deterministic_bounded_and_derived_from_schema_three() {
        let input = include_str!("../tests/fixtures/probe-v3/known.json");
        let first = summarize_json(input).unwrap();
        let second = summarize_json(input).unwrap();
        assert_eq!(first, second);
        assert!(first.ends_with('\n'));
        assert!(first.len() <= MAX_SUMMARY_BYTES);
        for expected in [
            "ASense compatibility probe summary",
            "Schema: 3 | mode: passive",
            "Machine: Predator PHN16-72 | BIOS: V1.18",
            "Daemon: protocol 2 / version 0.3.0",
            "Power: AC online=1 offline=0 unknown=0 | batteries=1",
            "Drivers: acer_wmi=loaded, asense_rgb=loaded(0.3.0) | WMI GUIDs=3 | hwmon-owner=asense_rgb",
            "Profile: transport=kernel | current=balanced | choices=5",
            "Fans: kernel-pwm modes=auto,manual,maximum | RPM readable=2/2",
            "Lighting: zoned-wmi-keyboard=zoned-wmi/keyboard/4-zone/state-readable",
            "Privacy: automatic-upload=no | persistent-id=no | default-mutations=0 | excluded-identity-classes=12",
        ] {
            assert!(first.contains(expected), "summary omitted {expected:?}");
        }
    }

    #[test]
    fn human_summary_preserves_typed_errors_but_omits_raw_hid_evidence() {
        let summary = summarize_json(include_str!(
            "../tests/fixtures/probe-v3/unknown-error.json"
        ))
        .unwrap();
        assert!(summary.contains("Daemon: error(protocol/incompatible)"));
        assert!(
            summary.contains("Profile: transport=gaming-wmi | current=unknown(raw=02) | choices=5")
        );
        assert!(summary.contains(
            "Platform: transport=gaming-wmi | values=0 absent=4 errors=3 | error-fields=keyboard-timeout,lcd-override,rear-logo"
        ));
        assert!(
            summary.contains(
                "HID: enek5130-lighting=0cf2:5130, acer-ec-hid-power-candidate=1025:174b"
            )
        );
        for omitted in [
            "a10321658300000000000000",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ] {
            assert!(!summary.contains(omitted));
        }
    }

    #[test]
    fn schema_three_rejects_unknown_fields_and_noncanonical_profile_raw() {
        let mut fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/probe-v3/unknown-error.json"
        ))
        .unwrap();
        fixture["provenance"]["unexpected"] = Value::Bool(true);
        assert!(serde_json::from_value::<ProbeReport>(fixture).is_err());

        let mut fixture: ProbeReport = serde_json::from_str(include_str!(
            "../tests/fixtures/probe-v3/unknown-error.json"
        ))
        .unwrap();
        if let schema::ProfileCurrent::Unknown { raw, .. } = &mut fixture.profile.current {
            raw.value = "02ff".to_string();
        } else {
            panic!("fixture does not contain unknown current profile");
        }
        assert!(fixture.validate().is_err());

        let mut fixture: ProbeReport = serde_json::from_str(include_str!(
            "../tests/fixtures/probe-v3/unknown-error.json"
        ))
        .unwrap();
        fixture.hid.swap(0, 1);
        assert!(fixture.validate().is_err());

        let mut fixture: ProbeReport = serde_json::from_str(include_str!(
            "../tests/fixtures/probe-v3/unknown-error.json"
        ))
        .unwrap();
        fixture.hid[0].driver = Observation::value(SourceId::DmiVendor, "hid-generic".to_string());
        assert!(fixture.validate().is_err());

        let mut fixture: ProbeReport = serde_json::from_str(include_str!(
            "../tests/fixtures/probe-v3/unknown-error.json"
        ))
        .unwrap();
        let Some(Observation::Value { value, .. }) = &mut fixture.hid[0].a1 else {
            panic!("fixture does not contain passive A1 evidence");
        };
        value.targets.pop();
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn local_schema_three_collection_is_passive_private_and_source_labeled() {
        let tree = TempTree::new();
        for (path, value) in [
            ("sys/class/dmi/id/sys_vendor", "Acer\n"),
            ("sys/class/dmi/id/product_name", "Predator Probe\n"),
            ("sys/class/dmi/id/board_name", "Board\n"),
            ("sys/class/dmi/id/bios_version", "V1.00\n"),
            ("etc/os-release", "ID=testos\nVERSION_ID=\"3.0\"\n"),
            ("sys/class/power_supply/AC/type", "Mains\n"),
            ("sys/class/power_supply/AC/online", "1\n"),
            ("sys/class/power_supply/BAT/type", "Battery\n"),
            ("sys/class/power_supply/BAT/present", "1\n"),
            ("sys/class/power_supply/BAT/status", "Charging\n"),
            ("sys/class/power_supply/BAT/capacity", "81\n"),
            ("sys/module/asense_rgb/version", "0.3.0\n"),
            ("sys/class/hwmon/hwmon0/name", "acer\n"),
            ("sys/class/hwmon/hwmon0/fan1_input", "3100\n"),
            ("sys/class/dmi/id/product_serial", "SECRET-SERIAL\n"),
            ("sys/class/dmi/id/board_serial", "SECRET-BOARD-SERIAL\n"),
            ("sys/class/dmi/id/product_uuid", "SECRET-UUID\n"),
            ("etc/hostname", "SECRET-HOST\n"),
            ("etc/machine-id", "SECRET-MACHINE-ID\n"),
            ("proc/sys/kernel/random/boot_id", "SECRET-BOOT-ID\n"),
            ("proc/net/dev", "SECRET-NETWORK\n"),
            ("sys/class/net/wlan0/address", "SECRET-MAC\n"),
            ("sys/class/block/nvme0n1/device/serial", "SECRET-STORAGE\n"),
            ("var/log/journal/private", "SECRET-JOURNAL\n"),
            ("sys/firmware/acpi/tables/DSDT", "SECRET-ACPI\n"),
        ] {
            tree.write(path, value);
        }
        tree.directory("sys/module/acer_wmi");
        tree.directory("sys/module/asense_rgb");
        let gaming = tree.directory(&format!("sys/bus/wmi/devices/{GAMING_GUID}"));
        tree.directory(&format!(
            "sys/bus/wmi/devices/{GAMING_GUID}/asense_diagnostics"
        ));
        tree.directory(&format!("sys/bus/wmi/devices/{GAMING_GUID}/gaming_profile"));
        tree.directory(&format!("sys/bus/wmi/devices/{GAMING_GUID}/gaming_fan"));
        symlink("../../drivers/asense_rgb", gaming.join("driver")).unwrap();
        let hwmon_device = tree.directory("sys/class/hwmon/hwmon0/device");
        symlink("../../drivers/acer-wmi", hwmon_device.join("driver")).unwrap();

        let before = snapshot(&tree.0);
        let output = generate_at(&tree.0).unwrap();
        assert_eq!(before, snapshot(&tree.0));
        let report: ProbeReport = serde_json::from_str(&output).unwrap();
        report.validate().unwrap();
        assert_eq!(report.schema, 3);
        assert_eq!(report.power.ac[0].kind, AcKind::Mains);
        assert_eq!(report.power.batteries[0].ordinal, 0);
        assert_eq!(report.drivers.wmi[0].guid, GAMING_GUID);
        assert_eq!(
            report.drivers.wmi[0].groups,
            vec![
                WmiGroup::AsenseDiagnostics,
                WmiGroup::GamingFan,
                WmiGroup::GamingProfile
            ]
        );
        assert_eq!(
            report.provenance.daemon,
            Observation::absent(SourceId::ControlSocket, AbsenceReason::DaemonUnavailable)
        );
        assert!(report.privacy.default_mutations.is_empty());
        for secret in [
            "SECRET-SERIAL",
            "SECRET-BOARD-SERIAL",
            "SECRET-UUID",
            "SECRET-HOST",
            "SECRET-MACHINE-ID",
            "SECRET-BOOT-ID",
            "SECRET-NETWORK",
            "SECRET-MAC",
            "SECRET-STORAGE",
            "SECRET-JOURNAL",
            "SECRET-ACPI",
        ] {
            assert!(!output.contains(secret));
        }
        assert!(!output.contains("/sys/"));
        assert!(!output.contains("CAPS"));
    }

    #[test]
    fn schema_three_order_is_deterministic_after_normalizing_capture_metadata() {
        let tree = TempTree::new();
        for (path, value) in [
            ("sys/class/dmi/id/sys_vendor", "Acer\n"),
            ("sys/class/dmi/id/product_name", "Predator Probe\n"),
            ("sys/class/power_supply/ZAC/type", "Mains\n"),
            ("sys/class/power_supply/ZAC/online", "0\n"),
            ("sys/class/power_supply/AAC/type", "Mains\n"),
            ("sys/class/power_supply/AAC/online", "1\n"),
        ] {
            tree.write(path, value);
        }

        let normalize = |output: String| {
            let mut report: ProbeReport = serde_json::from_str(&output).unwrap();
            report.provenance.captured_at_utc = "2026-08-01T12:00:00Z".to_string();
            report.provenance.capture_duration_ms = 0;
            encode_report(&report).unwrap()
        };
        assert_eq!(
            normalize(generate_at(&tree.0).unwrap()),
            normalize(generate_at(&tree.0).unwrap())
        );
    }

    #[test]
    fn lighting_inventory_must_be_sorted_unique_and_nonzero() {
        let mut fixture: ProbeReport =
            serde_json::from_str(include_str!("../tests/fixtures/probe-v3/known.json")).unwrap();
        fixture.lighting[0].zones = 0;
        assert!(fixture.validate().is_err());

        let mut fixture: ProbeReport =
            serde_json::from_str(include_str!("../tests/fixtures/probe-v3/known.json")).unwrap();
        fixture.lighting.push(fixture.lighting[0].clone());
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn invalid_utf8_is_a_typed_decode_error_not_lossy_text() {
        let tree = TempTree::new();
        tree.write("sys/class/dmi/id/sys_vendor", [0xff, 0xfe]);
        let report: ProbeReport = serde_json::from_str(&generate_at(&tree.0).unwrap()).unwrap();
        assert_eq!(
            report.machine.dmi.vendor,
            Observation::error(
                SourceId::DmiVendor,
                ProbeError::new(ErrorStage::Decode, ErrorClass::InvalidValue, None)
            )
        );
    }

    #[test]
    fn power_supply_order_is_stable_before_names_are_discarded() {
        let tree = TempTree::new();
        for (name, online) in [("ZAC", "0\n"), ("AAC", "1\n")] {
            tree.write(&format!("sys/class/power_supply/{name}/type"), "Mains\n");
            tree.write(&format!("sys/class/power_supply/{name}/online"), online);
        }
        let report: ProbeReport = serde_json::from_str(&generate_at(&tree.0).unwrap()).unwrap();
        assert_eq!(report.power.ac[0].ordinal, 0);
        assert_eq!(report.power.ac[1].ordinal, 1);
        assert_eq!(
            report.power.ac[0].online,
            Observation::value(SourceId::PowerSupply, true)
        );
        assert_eq!(
            report.power.ac[1].online,
            Observation::value(SourceId::PowerSupply, false)
        );
    }

    #[test]
    fn passive_hid_mapping_preserves_exact_geometry_identity_and_a1_payload() {
        use crate::passive_hid::{
            DiagnosticHid, DiagnosticHidA1, DiagnosticHidBus, DiagnosticHidDescriptor,
            DiagnosticHidFeatureGeometry, DiagnosticHidIdentity, DiagnosticHidName,
            DiagnosticHidRole,
        };

        let mapped = map_hid(DiagnosticHid {
            role: DiagnosticHidRole::Enek5130Lighting,
            identity: DiagnosticHidIdentity {
                bus: DiagnosticHidBus::I2c,
                vid: 0x0cf2,
                pid: 0x5130,
                name: DiagnosticHidName::Enek5130,
                interface: None,
                usage_page: Some(0xff00),
                usage: Some(0x0001),
            },
            driver: passive::DiagnosticObservation::value("hid-generic".to_string()),
            descriptor: passive::DiagnosticObservation::value(DiagnosticHidDescriptor {
                bytes: 24,
                sha256: "ab".repeat(32),
                feature_reports: vec![DiagnosticHidFeatureGeometry {
                    id: 0xa1,
                    bytes: 12,
                }],
            }),
            a1: Some(passive::DiagnosticObservation::value(DiagnosticHidA1 {
                requested_bytes: 12,
                returned_bytes: 6,
                payload_hex: "a10383652100".to_string(),
                targets: vec![0x21, 0x65, 0x83],
            })),
        });

        assert_eq!(mapped.role, schema::HidRole::Enek5130Lighting);
        assert_eq!(mapped.identity.bus, schema::HidBus::I2c);
        assert_eq!(mapped.identity.interface, None);
        assert_eq!(mapped.identity.usage_page.as_deref(), Some("ff00"));
        assert_eq!(
            mapped.descriptor,
            Observation::value(
                SourceId::HidReportDescriptor,
                schema::HidDescriptor {
                    bytes: 24,
                    sha256: "ab".repeat(32),
                    feature_reports: vec![schema::HidFeatureGeometry {
                        id: "a1".to_string(),
                        bytes: 12,
                    }],
                }
            )
        );
        let Some(Observation::Value { source, value }) = mapped.a1 else {
            panic!("mapped ENEK evidence omits A1 value");
        };
        assert_eq!(source, SourceId::HidFeatureA1);
        assert_eq!(value.payload.value, "a10383652100");
        assert_eq!(value.targets, ["21", "65", "83"]);
        assert!(!mapped.extended.requested);
        assert!(mapped.extended.selectors.is_empty());
        assert!(mapped.extended.a3.is_empty());
    }

    #[test]
    fn passive_zoned_wmi_lighting_maps_to_transport_capability_only() {
        let mapped = map_lighting(passive::DiagnosticLighting {
            id: "zoned-wmi-keyboard".to_string(),
            backend: passive::DiagnosticLightingBackend::ZonedWmi,
            target: passive::DiagnosticLightingTarget::Keyboard,
            zones: 3,
            modes: passive::DiagnosticLightingModes {
                static_color: true,
                brightness: true,
                breathing: true,
                neon: true,
            },
            state_readable: false,
        });
        assert_eq!(mapped.id, "zoned-wmi-keyboard");
        assert_eq!(mapped.backend, schema::LightingBackend::ZonedWmi);
        assert_eq!(mapped.target, schema::LightingTarget::Keyboard);
        assert_eq!(mapped.zones, 3);
        assert!(!mapped.state_readable);
        assert_eq!(
            mapped.authority,
            schema::LightingAuthority::TransportCapability
        );
    }

    #[test]
    fn daemon_handshake_constant_remains_protocol_two() {
        assert_eq!(crate::control::CONTROL_PROTOCOL_VERSION, 2);
    }

    #[test]
    fn rejected_passive_command_retains_daemon_identity_and_marks_sections_incompatible() {
        let tree = TempTree::new();
        let error = ProbeError::new(ErrorStage::Protocol, ErrorClass::Incompatible, None);
        let output = generate_at_with_context(
            &tree.0,
            "2026-08-01T12:00:00Z".to_string(),
            Instant::now(),
            DaemonContext {
                identity: Observation::value(
                    SourceId::ControlSocket,
                    DaemonIdentity {
                        protocol: 2,
                        version: "0.2.2".to_string(),
                    },
                ),
                diagnostics: DaemonDiagnostics::Error(error.clone()),
            },
        )
        .unwrap();
        let report: ProbeReport = serde_json::from_str(&output).unwrap();
        assert_eq!(
            report.provenance.daemon,
            Observation::value(
                SourceId::ControlSocket,
                DaemonIdentity {
                    protocol: 2,
                    version: "0.2.2".to_string(),
                }
            )
        );
        assert_eq!(
            report.profile.transport,
            schema::BackendTransport::Error {
                source: SourceId::ProfileDiscovery,
                error: error.clone(),
            }
        );
        assert_eq!(
            report.fans.control,
            schema::FanControl::Error {
                source: SourceId::FanDiscovery,
                error: error.clone(),
            }
        );
        assert_eq!(
            report.platform.transport,
            schema::BackendTransport::Error {
                source: SourceId::PlatformDiscovery,
                error,
            }
        );
        assert!(!output.contains("CAPS"));
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(path).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    visit(root, &path, out);
                } else if path.is_file() {
                    out.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        visit(root, root, &mut out);
        out
    }
}
