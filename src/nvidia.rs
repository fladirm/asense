//! Fail-closed NVIDIA backend for the exact PHN16-72 RTX 4070 Laptop GPU.
//!
//! This module deliberately exposes no static power-limit setter.  Dynamic
//! Boost and its enforced power ceiling remain owned by NVIDIA's controller;
//! ASense only applies the OEM VF offsets and reports power/clock telemetry.

use std::error::Error;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use libloading::Library;

pub const GPU_PCI_BUS_ID: &str = "0000:01:00.0";
pub const NVIDIA_VENDOR_ID: u16 = 0x10de;
pub const RTX_4070_LAPTOP_DEVICE_ID: u16 = 0x2860;
pub const ACER_SUBSYSTEM_VENDOR_ID: u16 = 0x1025;
pub const PHN16_72_SUBSYSTEM_DEVICE_ID: u16 = 0x1731;
/// AD106 configuration used by the exact PCI/subsystem identity above.
pub const RTX_4070_LAPTOP_SM_COUNT: u32 = 36;
pub const RTX_4070_LAPTOP_CUDA_CORE_COUNT: u32 = 4_608;

pub const OEM_TURBO_CORE_OFFSET_MHZ: i32 = 100;
pub const OEM_TURBO_MEMORY_OFFSET_MHZ: i32 = 200;

const NVML_SUCCESS: c_int = 0;
const NVML_ERROR_UNINITIALIZED: c_int = 1;
const NVML_ERROR_NOT_SUPPORTED: c_int = 3;
const NVML_ERROR_DRIVER_NOT_LOADED: c_int = 9;
const NVML_ERROR_GPU_IS_LOST: c_int = 15;
const NVML_ERROR_RESET_REQUIRED: c_int = 16;
const NVML_ERROR_LIB_RM_VERSION_MISMATCH: c_int = 18;
const NVML_ERROR_NOT_READY: c_int = 27;
const NVML_CLOCK_GRAPHICS: c_int = 0;
const NVML_CLOCK_MEMORY: c_int = 2;
const NVML_TEMPERATURE_GPU: c_int = 0;
const NVML_DEVICE_NAME_BUFFER_SIZE: usize = 96;
const NVML_SYSTEM_DRIVER_VERSION_BUFFER_SIZE: usize = 80;
const BYTES_PER_MIB: u64 = 1024 * 1024;
const NVML_CLOCK_OFFSET_VERSION: u32 =
    std::mem::size_of::<NvmlClockOffset>() as u32 | (1_u32 << 24);
const NVML_MAX_GPU_PERF_PSTATES: usize = 16;
const NVML_PSTATE_UNKNOWN: c_int = 32;

const EXPECTED_STATES: [PerformanceState; 5] = [
    PerformanceState::P0,
    PerformanceState::P3,
    PerformanceState::P4,
    PerformanceState::P5,
    PerformanceState::P8,
];

type NvmlDevice = *mut c_void;
type NvmlReturn = c_int;

type NvmlInitV2 = unsafe extern "C" fn() -> NvmlReturn;
type NvmlShutdown = unsafe extern "C" fn() -> NvmlReturn;
type NvmlErrorString = unsafe extern "C" fn(NvmlReturn) -> *const c_char;
type NvmlDeviceGetHandleByPciBusIdV2 =
    unsafe extern "C" fn(*const c_char, *mut NvmlDevice) -> NvmlReturn;
type NvmlDeviceGetClockOffsets =
    unsafe extern "C" fn(NvmlDevice, *mut NvmlClockOffset) -> NvmlReturn;
type NvmlDeviceSetClockOffsets =
    unsafe extern "C" fn(NvmlDevice, *mut NvmlClockOffset) -> NvmlReturn;
type NvmlDeviceGetSupportedPerformanceStates =
    unsafe extern "C" fn(NvmlDevice, *mut c_int, u32) -> NvmlReturn;
type NvmlDeviceGetU32 = unsafe extern "C" fn(NvmlDevice, *mut u32) -> NvmlReturn;
type NvmlDeviceGetLimitConstraints =
    unsafe extern "C" fn(NvmlDevice, *mut u32, *mut u32) -> NvmlReturn;
type NvmlDeviceGetClockEventReasons = unsafe extern "C" fn(NvmlDevice, *mut u64) -> NvmlReturn;
type NvmlDeviceGetTemperature = unsafe extern "C" fn(NvmlDevice, c_int, *mut u32) -> NvmlReturn;
type NvmlDeviceGetUtilizationRates =
    unsafe extern "C" fn(NvmlDevice, *mut NvmlUtilization) -> NvmlReturn;
type NvmlDeviceGetMemoryInfo = unsafe extern "C" fn(NvmlDevice, *mut NvmlMemory) -> NvmlReturn;
type NvmlDeviceGetClockInfo = unsafe extern "C" fn(NvmlDevice, c_int, *mut u32) -> NvmlReturn;
type NvmlDeviceGetPerformanceState = unsafe extern "C" fn(NvmlDevice, *mut c_int) -> NvmlReturn;
type NvmlDeviceGetName = unsafe extern "C" fn(NvmlDevice, *mut c_char, u32) -> NvmlReturn;
type NvmlSystemGetDriverVersion = unsafe extern "C" fn(*mut c_char, u32) -> NvmlReturn;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct NvmlClockOffset {
    version: u32,
    clock_type: c_int,
    pstate: c_int,
    clock_offset_mhz: i32,
    min_clock_offset_mhz: i32,
    max_clock_offset_mhz: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(i32)]
pub enum PerformanceState {
    P0 = 0,
    P1 = 1,
    P2 = 2,
    P3 = 3,
    P4 = 4,
    P5 = 5,
    P6 = 6,
    P7 = 7,
    P8 = 8,
    P9 = 9,
    P10 = 10,
    P11 = 11,
    P12 = 12,
    P13 = 13,
    P14 = 14,
    P15 = 15,
}

impl TryFrom<c_int> for PerformanceState {
    type Error = NvidiaError;

    fn try_from(value: c_int) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::P0),
            1 => Ok(Self::P1),
            2 => Ok(Self::P2),
            3 => Ok(Self::P3),
            4 => Ok(Self::P4),
            5 => Ok(Self::P5),
            6 => Ok(Self::P6),
            7 => Ok(Self::P7),
            8 => Ok(Self::P8),
            9 => Ok(Self::P9),
            10 => Ok(Self::P10),
            11 => Ok(Self::P11),
            12 => Ok(Self::P12),
            13 => Ok(Self::P13),
            14 => Ok(Self::P14),
            15 => Ok(Self::P15),
            value => Err(NvidiaError::InvalidPerformanceState(value)),
        }
    }
}

impl fmt::Display for PerformanceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "P{}", *self as i32)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ClockDomain {
    Core,
    Memory,
}

impl ClockDomain {
    fn nvml_clock_type(self) -> c_int {
        match self {
            Self::Core => NVML_CLOCK_GRAPHICS,
            Self::Memory => NVML_CLOCK_MEMORY,
        }
    }
}

impl fmt::Display for ClockDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core => formatter.write_str("GPC"),
            Self::Memory => formatter.write_str("MEM"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockOffsets {
    pub core_mhz: i32,
    pub memory_mhz: i32,
}

impl ClockOffsets {
    pub const OEM_TURBO: Self = Self {
        core_mhz: OEM_TURBO_CORE_OFFSET_MHZ,
        memory_mhz: OEM_TURBO_MEMORY_OFFSET_MHZ,
    };

    pub const RESET: Self = Self {
        core_mhz: 0,
        memory_mhz: 0,
    };

    fn for_domain(self, domain: ClockDomain) -> i32 {
        match domain {
            ClockDomain::Core => self.core_mhz,
            ClockDomain::Memory => self.memory_mhz,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OffsetReading {
    pub current_mhz: i32,
    pub minimum_mhz: i32,
    pub maximum_mhz: i32,
}

impl OffsetReading {
    fn accepts(self, value: i32) -> bool {
        (self.minimum_mhz..=self.maximum_mhz).contains(&value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateOffsets {
    pub state: PerformanceState,
    pub core: OffsetReading,
    pub memory: OffsetReading,
}

impl StateOffsets {
    fn reading(&self, domain: ClockDomain) -> OffsetReading {
        match domain {
            ClockDomain::Core => self.core,
            ClockDomain::Memory => self.memory,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OffsetSnapshot {
    pub states: Vec<StateOffsets>,
}

impl OffsetSnapshot {
    pub fn state(&self, state: PerformanceState) -> Option<&StateOffsets> {
        self.states.iter().find(|reading| reading.state == state)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OffsetTransition {
    pub previous: OffsetSnapshot,
    pub current: OffsetSnapshot,
}

/// Raw NVML clock-event reasons plus named accessors for every documented bit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClockEventReasons(u64);

impl ClockEventReasons {
    pub const GPU_IDLE: u64 = 0x0000_0000_0000_0001;
    pub const APPLICATION_CLOCKS: u64 = 0x0000_0000_0000_0002;
    pub const SOFTWARE_POWER_CAP: u64 = 0x0000_0000_0000_0004;
    pub const HARDWARE_SLOWDOWN: u64 = 0x0000_0000_0000_0008;
    pub const SYNC_BOOST: u64 = 0x0000_0000_0000_0010;
    pub const SOFTWARE_THERMAL: u64 = 0x0000_0000_0000_0020;
    pub const HARDWARE_THERMAL: u64 = 0x0000_0000_0000_0040;
    pub const HARDWARE_POWER_BRAKE: u64 = 0x0000_0000_0000_0080;
    pub const DISPLAY_CLOCK: u64 = 0x0000_0000_0000_0100;

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, reason: u64) -> bool {
        self.0 & reason != 0
    }

    pub fn active_labels(self) -> Vec<&'static str> {
        [
            (Self::GPU_IDLE, "idle"),
            (Self::APPLICATION_CLOCKS, "application-clocks"),
            (Self::SOFTWARE_POWER_CAP, "software-power-cap"),
            (Self::HARDWARE_SLOWDOWN, "hardware-slowdown"),
            (Self::SYNC_BOOST, "sync-boost"),
            (Self::SOFTWARE_THERMAL, "software-thermal"),
            (Self::HARDWARE_THERMAL, "hardware-thermal"),
            (Self::HARDWARE_POWER_BRAKE, "hardware-power-brake"),
            (Self::DISPLAY_CLOCK, "display-clock"),
        ]
        .into_iter()
        .filter_map(|(bit, label)| self.contains(bit).then_some(label))
        .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PowerTelemetry {
    /// Instantaneous board power when the mobile driver exposes it.
    pub draw_mw: Option<u32>,
    /// Dynamic controller-enforced ceiling; this is not a requested static cap.
    pub enforced_limit_mw: u32,
    pub default_limit_mw: u32,
    pub minimum_limit_mw: u32,
    pub maximum_limit_mw: u32,
    pub clock_event_reasons: ClockEventReasons,
}

/// A best-effort live NVML sample. Individual unsupported queries remain
/// `None`; they do not make the exact OEM offset capability unavailable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NvidiaLiveTelemetry {
    pub temperature_c: Option<u32>,
    pub utilization_percent: Option<u32>,
    pub power_draw_mw: Option<u32>,
    pub pstate: Option<PerformanceState>,
    pub memory_used_mib: Option<u64>,
    pub memory_total_mib: Option<u64>,
    pub graphics_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    /// At least one live query reported that the initialized NVML session or
    /// device handle can no longer be used. Callers should drop this
    /// controller and run discovery again instead of polling a dead handle
    /// forever.
    pub session_lost: bool,
    pub errors: Vec<String>,
}

/// Slowly-changing NVML data which callers can cache for the lifetime of the
/// controller/driver session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NvidiaStaticInfo {
    pub model: Option<String>,
    pub driver_version: Option<String>,
    pub pci_bus_id: Option<String>,
    pub maximum_graphics_clock_mhz: Option<u32>,
    pub maximum_memory_clock_mhz: Option<u32>,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciIdentity {
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem_device: u16,
}

impl PciIdentity {
    pub const EXPECTED: Self = Self {
        vendor: NVIDIA_VENDOR_ID,
        device: RTX_4070_LAPTOP_DEVICE_ID,
        subsystem_vendor: ACER_SUBSYSTEM_VENDOR_ID,
        subsystem_device: PHN16_72_SUBSYSTEM_DEVICE_ID,
    };
}

#[derive(Debug)]
pub enum NvidiaError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidSysfsHex {
        path: PathBuf,
        value: String,
    },
    WrongGpu {
        bus_id: &'static str,
        expected: PciIdentity,
        actual: PciIdentity,
    },
    LibraryLoad(libloading::Error),
    MissingSymbol {
        symbol: &'static str,
        source: libloading::Error,
    },
    UnavailableCapability {
        operation: &'static str,
    },
    InvalidTelemetry {
        field: &'static str,
        value: String,
    },
    Nvml {
        operation: &'static str,
        code: c_int,
        message: String,
    },
    UnsupportedOffsetTopology {
        expected: Vec<PerformanceState>,
        actual: Vec<PerformanceState>,
    },
    InvalidPerformanceState(c_int),
    OffsetUnsupportedState(PerformanceState),
    AsymmetricOffsetSupport {
        state: PerformanceState,
        core_supported: bool,
        memory_supported: bool,
    },
    MissingSnapshotState(PerformanceState),
    OffsetOutOfRange {
        state: PerformanceState,
        domain: ClockDomain,
        requested_mhz: i32,
        minimum_mhz: i32,
        maximum_mhz: i32,
    },
    ReadbackMismatch {
        state: PerformanceState,
        domain: ClockDomain,
        expected_mhz: i32,
        actual_mhz: i32,
    },
    TransactionRolledBack {
        cause: Box<NvidiaError>,
    },
    TransactionRollbackFailed {
        cause: Box<NvidiaError>,
        rollback_failures: Vec<String>,
    },
    #[cfg(test)]
    InjectedFailure(&'static str),
}

impl fmt::Display for NvidiaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::InvalidSysfsHex { path, value } => {
                write!(
                    formatter,
                    "{} contains invalid hex {value:?}",
                    path.display()
                )
            }
            Self::WrongGpu {
                bus_id,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU {bus_id} identity mismatch: expected {expected:04x?}, got {actual:04x?}"
            ),
            Self::LibraryLoad(source) => {
                write!(formatter, "cannot load libnvidia-ml.so.1: {source}")
            }
            Self::MissingSymbol { symbol, source } => {
                write!(formatter, "NVML symbol {symbol} is unavailable: {source}")
            }
            Self::UnavailableCapability { operation } => {
                write!(formatter, "NVML capability {operation} is unavailable")
            }
            Self::InvalidTelemetry { field, value } => {
                write!(formatter, "NVML returned invalid {field}: {value}")
            }
            Self::Nvml {
                operation,
                code,
                message,
            } => write!(formatter, "NVML {operation} failed ({code}): {message}"),
            Self::UnsupportedOffsetTopology { expected, actual } => write!(
                formatter,
                "NVML VF-offset P-state topology mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::InvalidPerformanceState(value) => {
                write!(formatter, "NVML returned invalid performance state {value}")
            }
            Self::OffsetUnsupportedState(state) => write!(
                formatter,
                "NVML advertises {state}, but clock-offset control is unavailable"
            ),
            Self::AsymmetricOffsetSupport {
                state,
                core_supported,
                memory_supported,
            } => write!(
                formatter,
                "{state} has asymmetric VF-offset support (core={core_supported}, memory={memory_supported})"
            ),
            Self::MissingSnapshotState(state) => {
                write!(formatter, "offset snapshot does not contain {state}")
            }
            Self::OffsetOutOfRange {
                state,
                domain,
                requested_mhz,
                minimum_mhz,
                maximum_mhz,
            } => write!(
                formatter,
                "{state} {domain} offset {requested_mhz} MHz is outside {minimum_mhz}..={maximum_mhz} MHz"
            ),
            Self::ReadbackMismatch {
                state,
                domain,
                expected_mhz,
                actual_mhz,
            } => write!(
                formatter,
                "{state} {domain} offset readback mismatch: expected {expected_mhz} MHz, got {actual_mhz} MHz"
            ),
            Self::TransactionRolledBack { cause } => {
                write!(
                    formatter,
                    "offset transaction failed and was rolled back: {cause}"
                )
            }
            Self::TransactionRollbackFailed {
                cause,
                rollback_failures,
            } => write!(
                formatter,
                "offset transaction failed ({cause}) and rollback was incomplete: {}",
                rollback_failures.join("; ")
            ),
            #[cfg(test)]
            Self::InjectedFailure(label) => write!(formatter, "injected failure: {label}"),
        }
    }
}

impl Error for NvidiaError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::LibraryLoad(source) => Some(source),
            Self::MissingSymbol { source, .. } => Some(source),
            Self::TransactionRolledBack { cause }
            | Self::TransactionRollbackFailed { cause, .. } => Some(cause),
            _ => None,
        }
    }
}

impl NvidiaError {
    /// Returns true for NVML errors which invalidate the initialized runtime
    /// or its device handle. Optional/unsupported metrics deliberately do not
    /// trigger rediscovery.
    pub const fn invalidates_session(&self) -> bool {
        match self {
            Self::Nvml { code, .. } => matches!(
                *code,
                NVML_ERROR_UNINITIALIZED
                    | NVML_ERROR_DRIVER_NOT_LOADED
                    | NVML_ERROR_GPU_IS_LOST
                    | NVML_ERROR_RESET_REQUIRED
                    | NVML_ERROR_LIB_RM_VERSION_MISMATCH
                    | NVML_ERROR_NOT_READY
            ),
            _ => false,
        }
    }
}

struct NvmlSymbols {
    _library: Library,
    shutdown: NvmlShutdown,
    error_string: NvmlErrorString,
    get_handle_by_pci_bus_id: NvmlDeviceGetHandleByPciBusIdV2,
    get_supported_performance_states: Option<NvmlDeviceGetSupportedPerformanceStates>,
    get_clock_offsets: Option<NvmlDeviceGetClockOffsets>,
    set_clock_offsets: Option<NvmlDeviceSetClockOffsets>,
    get_power_usage: Option<NvmlDeviceGetU32>,
    get_enforced_power_limit: Option<NvmlDeviceGetU32>,
    get_default_power_limit: Option<NvmlDeviceGetU32>,
    get_power_limit_constraints: Option<NvmlDeviceGetLimitConstraints>,
    get_clock_event_reasons: Option<NvmlDeviceGetClockEventReasons>,
    get_temperature: Option<NvmlDeviceGetTemperature>,
    get_utilization_rates: Option<NvmlDeviceGetUtilizationRates>,
    get_memory_info: Option<NvmlDeviceGetMemoryInfo>,
    get_clock_info: Option<NvmlDeviceGetClockInfo>,
    get_max_clock_info: Option<NvmlDeviceGetClockInfo>,
    get_performance_state: Option<NvmlDeviceGetPerformanceState>,
    get_name: Option<NvmlDeviceGetName>,
    get_driver_version: Option<NvmlSystemGetDriverVersion>,
}

struct NvmlRuntime {
    symbols: NvmlSymbols,
}

impl NvmlRuntime {
    fn load() -> Result<Self, NvidiaError> {
        // SAFETY: the SONAME is fixed and every symbol is copied while the
        // Library is retained in NvmlSymbols for the full pointer lifetime.
        let library =
            unsafe { Library::new("libnvidia-ml.so.1") }.map_err(NvidiaError::LibraryLoad)?;

        // SAFETY: symbol names and signatures match nvml.h from CUDA 13.3.
        let init: NvmlInitV2 = unsafe { load_symbol(&library, b"nvmlInit_v2\0", "nvmlInit_v2")? };
        let shutdown = unsafe { load_symbol(&library, b"nvmlShutdown\0", "nvmlShutdown")? };
        let error_string =
            unsafe { load_symbol(&library, b"nvmlErrorString\0", "nvmlErrorString")? };
        let get_handle_by_pci_bus_id = unsafe {
            load_symbol(
                &library,
                b"nvmlDeviceGetHandleByPciBusId_v2\0",
                "nvmlDeviceGetHandleByPciBusId_v2",
            )?
        };
        // VF-offset symbols belong only to the optional OEM tuning plane.
        // Their absence must not disable ordinary read-only NVML telemetry.
        let get_clock_offsets =
            unsafe { optional_symbol(&library, b"nvmlDeviceGetClockOffsets\0") };
        let get_supported_performance_states =
            unsafe { optional_symbol(&library, b"nvmlDeviceGetSupportedPerformanceStates\0") };
        let set_clock_offsets =
            unsafe { optional_symbol(&library, b"nvmlDeviceSetClockOffsets\0") };
        // Read-only telemetry entry points are deliberately optional. The OEM
        // offset capability remains usable when a driver omits one diagnostic
        // API, and callers report the missing metric independently.
        let get_power_usage = unsafe { optional_symbol(&library, b"nvmlDeviceGetPowerUsage\0") };
        let get_enforced_power_limit =
            unsafe { optional_symbol(&library, b"nvmlDeviceGetEnforcedPowerLimit\0") };
        let get_default_power_limit =
            unsafe { optional_symbol(&library, b"nvmlDeviceGetPowerManagementDefaultLimit\0") };
        let get_power_limit_constraints =
            unsafe { optional_symbol(&library, b"nvmlDeviceGetPowerManagementLimitConstraints\0") };
        let get_clock_event_reasons =
            unsafe { optional_symbol(&library, b"nvmlDeviceGetCurrentClocksEventReasons\0") };
        let get_temperature = unsafe { optional_symbol(&library, b"nvmlDeviceGetTemperature\0") };
        let get_utilization_rates =
            unsafe { optional_symbol(&library, b"nvmlDeviceGetUtilizationRates\0") };
        let get_memory_info = unsafe { optional_symbol(&library, b"nvmlDeviceGetMemoryInfo\0") };
        let get_clock_info = unsafe { optional_symbol(&library, b"nvmlDeviceGetClockInfo\0") };
        let get_max_clock_info =
            unsafe { optional_symbol(&library, b"nvmlDeviceGetMaxClockInfo\0") };
        let get_performance_state =
            unsafe { optional_symbol(&library, b"nvmlDeviceGetPerformanceState\0") };
        let get_name = unsafe { optional_symbol(&library, b"nvmlDeviceGetName\0") };
        let get_driver_version =
            unsafe { optional_symbol(&library, b"nvmlSystemGetDriverVersion\0") };

        let code = unsafe { init() };
        if code != NVML_SUCCESS {
            return Err(nvml_error_from(error_string, "nvmlInit_v2", code));
        }

        Ok(Self {
            symbols: NvmlSymbols {
                _library: library,
                shutdown,
                error_string,
                get_handle_by_pci_bus_id,
                get_supported_performance_states,
                get_clock_offsets,
                set_clock_offsets,
                get_power_usage,
                get_enforced_power_limit,
                get_default_power_limit,
                get_power_limit_constraints,
                get_clock_event_reasons,
                get_temperature,
                get_utilization_rates,
                get_memory_info,
                get_clock_info,
                get_max_clock_info,
                get_performance_state,
                get_name,
                get_driver_version,
            },
        })
    }

    fn error(&self, operation: &'static str, code: c_int) -> NvidiaError {
        nvml_error_from(self.symbols.error_string, operation, code)
    }
}

impl Drop for NvmlRuntime {
    fn drop(&mut self) {
        // SAFETY: runtime initialization succeeded and this is its sole owner.
        let _ = unsafe { (self.symbols.shutdown)() };
    }
}

unsafe fn load_symbol<T: Copy>(
    library: &Library,
    bytes: &'static [u8],
    name: &'static str,
) -> Result<T, NvidiaError> {
    // SAFETY: the caller provides the exact C signature for this symbol.
    let symbol =
        unsafe { library.get::<T>(bytes) }.map_err(|source| NvidiaError::MissingSymbol {
            symbol: name,
            source,
        })?;
    Ok(*symbol)
}

unsafe fn optional_symbol<T: Copy>(library: &Library, bytes: &'static [u8]) -> Option<T> {
    // SAFETY: callers provide the exact C signature. The copied pointer remains
    // valid because NvmlSymbols retains the Library for its full lifetime.
    unsafe { library.get::<T>(bytes) }
        .ok()
        .map(|symbol| *symbol)
}

fn nvml_error_from(
    error_string: NvmlErrorString,
    operation: &'static str,
    code: c_int,
) -> NvidiaError {
    // SAFETY: NVML owns this NUL-terminated static error string.
    let pointer = unsafe { error_string(code) };
    let message = if pointer.is_null() {
        "unknown NVML error".to_owned()
    } else {
        // SAFETY: checked non-null and NVML guarantees a C string.
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    };
    NvidiaError::Nvml {
        operation,
        code,
        message,
    }
}

/// Exact-GPU NVML controller. Construction fails closed on any identity or
/// P-state topology mismatch.
pub struct NvidiaController {
    runtime: NvmlRuntime,
    device: NvmlDevice,
    states: Vec<PerformanceState>,
    exact_oem_target: bool,
    /// Only the exact OEM discovery path may mutate VF offsets.  The generic
    /// telemetry path deliberately owns the same long-lived NVML handle, but
    /// must remain structurally unable to become a tuning controller merely
    /// because a driver happens to expose offset symbols.
    mutation_authorized: bool,
}

fn open_device(runtime: &NvmlRuntime) -> Result<NvmlDevice, NvidiaError> {
    let bus_id = CString::new(GPU_PCI_BUS_ID).expect("static PCI ID contains no NUL");
    let mut device = std::ptr::null_mut();
    // SAFETY: runtime is initialized, bus_id is NUL terminated, output is valid.
    let code = unsafe { (runtime.symbols.get_handle_by_pci_bus_id)(bus_id.as_ptr(), &mut device) };
    if code != NVML_SUCCESS {
        return Err(runtime.error("nvmlDeviceGetHandleByPciBusId_v2", code));
    }
    if device.is_null() {
        return Err(NvidiaError::Nvml {
            operation: "nvmlDeviceGetHandleByPciBusId_v2",
            code: -1,
            message: "NVML returned a null device handle".to_owned(),
        });
    }
    Ok(device)
}

impl NvidiaController {
    pub fn discover() -> Result<Self, NvidiaError> {
        Self::discover_with_sysfs_root(Path::new("/"))
    }

    /// Opens the canonical laptop dGPU for read-only telemetry without
    /// requiring the PHN16-72 OEM VF-offset ABI. Missing offset symbols or a
    /// different P-state topology only omit offset telemetry; temperature,
    /// utilization, clocks, VRAM and power remain available.
    pub fn discover_telemetry() -> Result<Self, NvidiaError> {
        let exact_oem_target = read_pci_identity(Path::new("/"))
            .is_ok_and(|identity| identity == PciIdentity::EXPECTED);
        let runtime = NvmlRuntime::load()?;
        let device = open_device(&runtime)?;
        let mut controller = Self {
            runtime,
            device,
            states: Vec::new(),
            exact_oem_target,
            mutation_authorized: false,
        };
        controller.states = controller.probe_supported_states().unwrap_or_default();
        Ok(controller)
    }

    fn discover_with_sysfs_root(sysfs_root: &Path) -> Result<Self, NvidiaError> {
        let actual = read_pci_identity(sysfs_root)?;
        if actual != PciIdentity::EXPECTED {
            return Err(NvidiaError::WrongGpu {
                bus_id: GPU_PCI_BUS_ID,
                expected: PciIdentity::EXPECTED,
                actual,
            });
        }

        let runtime = NvmlRuntime::load()?;
        if runtime.symbols.get_supported_performance_states.is_none()
            || runtime.symbols.get_clock_offsets.is_none()
            || runtime.symbols.set_clock_offsets.is_none()
        {
            return Err(NvidiaError::UnavailableCapability {
                operation: "OEM VF clock offsets",
            });
        }
        let device = open_device(&runtime)?;
        let mut controller = Self {
            runtime,
            device,
            states: Vec::new(),
            exact_oem_target: true,
            mutation_authorized: true,
        };
        let states = controller.probe_supported_states()?;
        if states != EXPECTED_STATES {
            return Err(NvidiaError::UnsupportedOffsetTopology {
                expected: EXPECTED_STATES.to_vec(),
                actual: states,
            });
        }
        controller.states = states;
        Ok(controller)
    }

    pub fn supported_states(&self) -> &[PerformanceState] {
        &self.states
    }

    pub const fn is_exact_oem_target(&self) -> bool {
        self.exact_oem_target
    }

    /// Collects live data through the controller's already-initialized NVML
    /// runtime. Unsupported metrics are isolated instead of discarding the
    /// rest of the sample.
    pub fn live_telemetry(&self) -> NvidiaLiveTelemetry {
        let mut errors = Vec::new();
        let mut session_lost = false;
        let temperature_c = collect_live_metric(&mut errors, &mut session_lost, self.temperature());
        let utilization_percent =
            collect_live_metric(&mut errors, &mut session_lost, self.utilization());
        let power_draw_mw = collect_live_metric(
            &mut errors,
            &mut session_lost,
            self.get_optional_u32(
                "nvmlDeviceGetPowerUsage",
                self.runtime.symbols.get_power_usage,
            ),
        );
        let pstate = collect_live_metric(&mut errors, &mut session_lost, self.performance_state());
        let memory = collect_live_metric(&mut errors, &mut session_lost, self.memory_info());
        let graphics_clock_mhz = collect_live_metric(
            &mut errors,
            &mut session_lost,
            self.clock_info(
                "nvmlDeviceGetClockInfo(graphics)",
                self.runtime.symbols.get_clock_info,
                NVML_CLOCK_GRAPHICS,
            ),
        );
        let memory_clock_mhz = collect_live_metric(
            &mut errors,
            &mut session_lost,
            self.clock_info(
                "nvmlDeviceGetClockInfo(memory)",
                self.runtime.symbols.get_clock_info,
                NVML_CLOCK_MEMORY,
            ),
        );

        NvidiaLiveTelemetry {
            temperature_c,
            utilization_percent,
            power_draw_mw,
            pstate,
            memory_used_mib: memory.map(|memory| memory.used / BYTES_PER_MIB),
            memory_total_mib: memory.map(|memory| memory.total / BYTES_PER_MIB),
            graphics_clock_mhz,
            memory_clock_mhz,
            session_lost,
            errors,
        }
    }
    /// Reads data that remains stable for a driver session. TelemetryReader
    /// caches this result instead of polling it with every UI sample.
    pub fn static_info(&self) -> NvidiaStaticInfo {
        let mut errors = Vec::new();
        let model = collect_metric(&mut errors, self.device_name());
        let driver_version = collect_metric(&mut errors, self.driver_version());
        let maximum_graphics_clock_mhz = collect_metric(
            &mut errors,
            self.clock_info(
                "nvmlDeviceGetMaxClockInfo(graphics)",
                self.runtime.symbols.get_max_clock_info,
                NVML_CLOCK_GRAPHICS,
            ),
        );
        let maximum_memory_clock_mhz = collect_metric(
            &mut errors,
            self.clock_info(
                "nvmlDeviceGetMaxClockInfo(memory)",
                self.runtime.symbols.get_max_clock_info,
                NVML_CLOCK_MEMORY,
            ),
        );
        NvidiaStaticInfo {
            model,
            driver_version,
            // Both discovery paths opened this canonical bus address.  OEM
            // identity authorization is intentionally tracked separately
            // from this read-only informational field.
            pci_bus_id: Some(GPU_PCI_BUS_ID.to_owned()),
            maximum_graphics_clock_mhz,
            maximum_memory_clock_mhz,
            errors,
        }
    }

    pub fn snapshot_offsets(&self) -> Result<OffsetSnapshot, NvidiaError> {
        snapshot_device(self)
    }

    /// Applies the exact Acer Turbo offsets to all supported P-states.
    /// Every write is read back; any partial failure restores the snapshot.
    pub fn apply_oem_turbo(&self) -> Result<OffsetTransition, NvidiaError> {
        self.ensure_mutation_authorized()?;
        apply_uniform_offsets(self, ClockOffsets::OEM_TURBO)
    }

    /// Resets ASense-owned VF offsets without touching Dynamic Boost/power.
    pub fn reset_oem_offsets(&self) -> Result<OffsetTransition, NvidiaError> {
        self.ensure_mutation_authorized()?;
        apply_uniform_offsets(self, ClockOffsets::RESET)
    }

    /// Restores a previously captured complete offset snapshot transactionally.
    pub fn restore_offsets(
        &self,
        snapshot: &OffsetSnapshot,
    ) -> Result<OffsetTransition, NvidiaError> {
        self.ensure_mutation_authorized()?;
        let targets = self
            .states
            .iter()
            .map(|state| {
                let reading = snapshot
                    .state(*state)
                    .ok_or(NvidiaError::MissingSnapshotState(*state))?;
                Ok((
                    *state,
                    ClockOffsets {
                        core_mhz: reading.core.current_mhz,
                        memory_mhz: reading.memory.current_mhz,
                    },
                ))
            })
            .collect::<Result<Vec<_>, NvidiaError>>()?;
        apply_offset_targets(self, &targets)
    }

    fn ensure_mutation_authorized(&self) -> Result<(), NvidiaError> {
        if self.mutation_authorized {
            Ok(())
        } else {
            Err(NvidiaError::UnavailableCapability {
                operation: "OEM VF mutation on a read-only telemetry controller",
            })
        }
    }

    pub fn power_telemetry(&self) -> Result<PowerTelemetry, NvidiaError> {
        let draw_mw = self.get_optional_u32(
            "nvmlDeviceGetPowerUsage",
            self.runtime.symbols.get_power_usage,
        )?;
        let enforced_limit_mw = self.get_required_u32(
            "nvmlDeviceGetEnforcedPowerLimit",
            self.runtime.symbols.get_enforced_power_limit,
        )?;
        let default_limit_mw = self.get_required_u32(
            "nvmlDeviceGetPowerManagementDefaultLimit",
            self.runtime.symbols.get_default_power_limit,
        )?;

        let mut minimum_limit_mw = 0;
        let mut maximum_limit_mw = 0;
        // SAFETY: device and both output pointers are valid.
        let get_power_limit_constraints = self.runtime.symbols.get_power_limit_constraints.ok_or(
            NvidiaError::UnavailableCapability {
                operation: "nvmlDeviceGetPowerManagementLimitConstraints",
            },
        )?;
        let code = unsafe {
            get_power_limit_constraints(self.device, &mut minimum_limit_mw, &mut maximum_limit_mw)
        };
        if code != NVML_SUCCESS {
            return Err(self
                .runtime
                .error("nvmlDeviceGetPowerManagementLimitConstraints", code));
        }

        let mut reason_bits = 0_u64;
        // SAFETY: device and output pointer are valid.
        let get_clock_event_reasons = self.runtime.symbols.get_clock_event_reasons.ok_or(
            NvidiaError::UnavailableCapability {
                operation: "nvmlDeviceGetCurrentClocksEventReasons",
            },
        )?;
        let code = unsafe { get_clock_event_reasons(self.device, &mut reason_bits) };
        if code != NVML_SUCCESS {
            return Err(self
                .runtime
                .error("nvmlDeviceGetCurrentClocksEventReasons", code));
        }

        Ok(PowerTelemetry {
            draw_mw,
            enforced_limit_mw,
            default_limit_mw,
            minimum_limit_mw,
            maximum_limit_mw,
            clock_event_reasons: ClockEventReasons::from_bits(reason_bits),
        })
    }

    fn probe_supported_states(&self) -> Result<Vec<PerformanceState>, NvidiaError> {
        let get_supported_performance_states = self
            .runtime
            .symbols
            .get_supported_performance_states
            .ok_or(NvidiaError::UnavailableCapability {
                operation: "nvmlDeviceGetSupportedPerformanceStates",
            })?;
        let mut raw_states = [NVML_PSTATE_UNKNOWN; NVML_MAX_GPU_PERF_PSTATES];
        let bytes = u32::try_from(std::mem::size_of_val(&raw_states))
            .expect("NVML P-state buffer size fits u32");
        // SAFETY: device is valid and raw_states is a writable buffer whose
        // byte size is passed exactly as required by nvml.h.
        let code = unsafe {
            get_supported_performance_states(self.device, raw_states.as_mut_ptr(), bytes)
        };
        if code != NVML_SUCCESS {
            return Err(self
                .runtime
                .error("nvmlDeviceGetSupportedPerformanceStates", code));
        }

        let mut advertised = raw_states
            .into_iter()
            .take_while(|state| *state != NVML_PSTATE_UNKNOWN)
            .map(PerformanceState::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        advertised.sort_unstable();
        advertised.dedup();

        let mut supported = Vec::with_capacity(advertised.len());
        for state in advertised {
            let core = self.read_clock_optional(state, ClockDomain::Core)?;
            let memory = self.read_clock_optional(state, ClockDomain::Memory)?;
            match (core, memory) {
                (Some(_), Some(_)) => supported.push(state),
                (None, None) => return Err(NvidiaError::OffsetUnsupportedState(state)),
                (core, memory) => {
                    return Err(NvidiaError::AsymmetricOffsetSupport {
                        state,
                        core_supported: core.is_some(),
                        memory_supported: memory.is_some(),
                    });
                }
            }
        }
        Ok(supported)
    }

    fn read_clock_optional(
        &self,
        state: PerformanceState,
        domain: ClockDomain,
    ) -> Result<Option<OffsetReading>, NvidiaError> {
        let get_clock_offsets =
            self.runtime
                .symbols
                .get_clock_offsets
                .ok_or(NvidiaError::UnavailableCapability {
                    operation: "nvmlDeviceGetClockOffsets",
                })?;
        let mut info = NvmlClockOffset {
            version: NVML_CLOCK_OFFSET_VERSION,
            clock_type: domain.nvml_clock_type(),
            pstate: state as c_int,
            ..NvmlClockOffset::default()
        };
        // SAFETY: device is valid and info matches nvmlClockOffset_v1_t.
        let code = unsafe { get_clock_offsets(self.device, &mut info) };
        match code {
            NVML_SUCCESS => Ok(Some(OffsetReading {
                current_mhz: info.clock_offset_mhz,
                minimum_mhz: info.min_clock_offset_mhz,
                maximum_mhz: info.max_clock_offset_mhz,
            })),
            NVML_ERROR_NOT_SUPPORTED => Ok(None),
            code => Err(self.runtime.error("nvmlDeviceGetClockOffsets", code)),
        }
    }

    fn temperature(&self) -> Result<Option<u32>, NvidiaError> {
        let Some(function) = self.runtime.symbols.get_temperature else {
            return Ok(None);
        };
        let mut value = 0;
        // SAFETY: device and output pointer are valid; GPU is the only legacy
        // temperature sensor accepted by this API.
        let code = unsafe { function(self.device, NVML_TEMPERATURE_GPU, &mut value) };
        match code {
            NVML_SUCCESS => Ok(Some(value)),
            NVML_ERROR_NOT_SUPPORTED => Ok(None),
            code => Err(self.runtime.error("nvmlDeviceGetTemperature", code)),
        }
    }

    fn utilization(&self) -> Result<Option<u32>, NvidiaError> {
        let Some(function) = self.runtime.symbols.get_utilization_rates else {
            return Ok(None);
        };
        let mut value = NvmlUtilization::default();
        // SAFETY: device and output pointer are valid and NvmlUtilization
        // matches nvmlUtilization_t.
        let code = unsafe { function(self.device, &mut value) };
        match code {
            NVML_SUCCESS if value.gpu <= 100 => Ok(Some(value.gpu)),
            NVML_SUCCESS => Err(NvidiaError::InvalidTelemetry {
                field: "GPU utilization",
                value: value.gpu.to_string(),
            }),
            NVML_ERROR_NOT_SUPPORTED => Ok(None),
            code => Err(self.runtime.error("nvmlDeviceGetUtilizationRates", code)),
        }
    }

    fn memory_info(&self) -> Result<Option<NvmlMemory>, NvidiaError> {
        let Some(function) = self.runtime.symbols.get_memory_info else {
            return Ok(None);
        };
        let mut value = NvmlMemory::default();
        // SAFETY: device and output pointer are valid and NvmlMemory matches
        // nvmlMemory_t v1.
        let code = unsafe { function(self.device, &mut value) };
        match code {
            NVML_SUCCESS if value.used <= value.total => Ok(Some(value)),
            NVML_SUCCESS => Err(NvidiaError::InvalidTelemetry {
                field: "GPU memory",
                value: format!("used={} total={}", value.used, value.total),
            }),
            NVML_ERROR_NOT_SUPPORTED => Ok(None),
            code => Err(self.runtime.error("nvmlDeviceGetMemoryInfo", code)),
        }
    }

    fn performance_state(&self) -> Result<Option<PerformanceState>, NvidiaError> {
        let Some(function) = self.runtime.symbols.get_performance_state else {
            return Ok(None);
        };
        let mut value = NVML_PSTATE_UNKNOWN;
        // SAFETY: device and output pointer are valid.
        let code = unsafe { function(self.device, &mut value) };
        match code {
            NVML_SUCCESS if value == NVML_PSTATE_UNKNOWN => Ok(None),
            NVML_SUCCESS => PerformanceState::try_from(value).map(Some),
            NVML_ERROR_NOT_SUPPORTED => Ok(None),
            code => Err(self.runtime.error("nvmlDeviceGetPerformanceState", code)),
        }
    }

    fn clock_info(
        &self,
        operation: &'static str,
        function: Option<NvmlDeviceGetClockInfo>,
        domain: c_int,
    ) -> Result<Option<u32>, NvidiaError> {
        let Some(function) = function else {
            return Ok(None);
        };
        let mut value = 0;
        // SAFETY: device, clock domain, and output pointer are valid.
        let code = unsafe { function(self.device, domain, &mut value) };
        match code {
            NVML_SUCCESS => Ok(Some(value)),
            NVML_ERROR_NOT_SUPPORTED => Ok(None),
            code => Err(self.runtime.error(operation, code)),
        }
    }

    fn device_name(&self) -> Result<Option<String>, NvidiaError> {
        let Some(function) = self.runtime.symbols.get_name else {
            return Ok(None);
        };
        let mut buffer = [0 as c_char; NVML_DEVICE_NAME_BUFFER_SIZE];
        // SAFETY: device is valid and the writable buffer length is exact.
        let code = unsafe {
            function(
                self.device,
                buffer.as_mut_ptr(),
                NVML_DEVICE_NAME_BUFFER_SIZE as u32,
            )
        };
        match code {
            NVML_SUCCESS => Ok(Some(c_buffer_to_string(&buffer))),
            NVML_ERROR_NOT_SUPPORTED => Ok(None),
            code => Err(self.runtime.error("nvmlDeviceGetName", code)),
        }
    }

    fn driver_version(&self) -> Result<Option<String>, NvidiaError> {
        let Some(function) = self.runtime.symbols.get_driver_version else {
            return Ok(None);
        };
        let mut buffer = [0 as c_char; NVML_SYSTEM_DRIVER_VERSION_BUFFER_SIZE];
        // SAFETY: the writable buffer and its exact length are valid.
        let code = unsafe {
            function(
                buffer.as_mut_ptr(),
                NVML_SYSTEM_DRIVER_VERSION_BUFFER_SIZE as u32,
            )
        };
        match code {
            NVML_SUCCESS => Ok(Some(c_buffer_to_string(&buffer))),
            NVML_ERROR_NOT_SUPPORTED => Ok(None),
            code => Err(self.runtime.error("nvmlSystemGetDriverVersion", code)),
        }
    }

    fn get_required_u32(
        &self,
        operation: &'static str,
        function: Option<NvmlDeviceGetU32>,
    ) -> Result<u32, NvidiaError> {
        let function = function.ok_or(NvidiaError::UnavailableCapability { operation })?;
        let mut value = 0;
        // SAFETY: device and output pointer are valid.
        let code = unsafe { function(self.device, &mut value) };
        if code == NVML_SUCCESS {
            Ok(value)
        } else {
            Err(self.runtime.error(operation, code))
        }
    }

    fn get_optional_u32(
        &self,
        operation: &'static str,
        function: Option<NvmlDeviceGetU32>,
    ) -> Result<Option<u32>, NvidiaError> {
        let Some(function) = function else {
            return Ok(None);
        };
        let mut value = 0;
        // SAFETY: device and output pointer are valid.
        let code = unsafe { function(self.device, &mut value) };
        match code {
            NVML_SUCCESS => Ok(Some(value)),
            NVML_ERROR_NOT_SUPPORTED => Ok(None),
            code => Err(self.runtime.error(operation, code)),
        }
    }
}

trait OffsetDevice {
    fn states(&self) -> &[PerformanceState];
    fn read_offset(
        &self,
        state: PerformanceState,
        domain: ClockDomain,
    ) -> Result<OffsetReading, NvidiaError>;
    fn write_offset(
        &self,
        state: PerformanceState,
        domain: ClockDomain,
        offset_mhz: i32,
    ) -> Result<(), NvidiaError>;
}

impl OffsetDevice for NvidiaController {
    fn states(&self) -> &[PerformanceState] {
        &self.states
    }

    fn read_offset(
        &self,
        state: PerformanceState,
        domain: ClockDomain,
    ) -> Result<OffsetReading, NvidiaError> {
        self.read_clock_optional(state, domain)?.ok_or_else(|| {
            NvidiaError::UnsupportedOffsetTopology {
                expected: EXPECTED_STATES.to_vec(),
                actual: self.states.clone(),
            }
        })
    }

    fn write_offset(
        &self,
        state: PerformanceState,
        domain: ClockDomain,
        offset_mhz: i32,
    ) -> Result<(), NvidiaError> {
        self.ensure_mutation_authorized()?;
        let set_clock_offsets =
            self.runtime
                .symbols
                .set_clock_offsets
                .ok_or(NvidiaError::UnavailableCapability {
                    operation: "nvmlDeviceSetClockOffsets",
                })?;
        let mut info = NvmlClockOffset {
            version: NVML_CLOCK_OFFSET_VERSION,
            clock_type: domain.nvml_clock_type(),
            pstate: state as c_int,
            clock_offset_mhz: offset_mhz,
            ..NvmlClockOffset::default()
        };
        // SAFETY: device is valid and info matches nvmlClockOffset_v1_t.
        let code = unsafe { set_clock_offsets(self.device, &mut info) };
        if code == NVML_SUCCESS {
            Ok(())
        } else {
            Err(self.runtime.error("nvmlDeviceSetClockOffsets", code))
        }
    }
}

fn collect_metric<T>(
    errors: &mut Vec<String>,
    result: Result<Option<T>, NvidiaError>,
) -> Option<T> {
    match result {
        Ok(value) => value,
        Err(error) => {
            errors.push(error.to_string());
            None
        }
    }
}

fn collect_live_metric<T>(
    errors: &mut Vec<String>,
    session_lost: &mut bool,
    result: Result<Option<T>, NvidiaError>,
) -> Option<T> {
    match result {
        Ok(value) => value,
        Err(error) => {
            *session_lost |= error.invalidates_session();
            errors.push(error.to_string());
            None
        }
    }
}

fn c_buffer_to_string(buffer: &[c_char]) -> String {
    let bytes = buffer
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .map(|byte| byte as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn snapshot_device(device: &impl OffsetDevice) -> Result<OffsetSnapshot, NvidiaError> {
    let mut states = Vec::with_capacity(device.states().len());
    for state in device.states() {
        states.push(StateOffsets {
            state: *state,
            core: device.read_offset(*state, ClockDomain::Core)?,
            memory: device.read_offset(*state, ClockDomain::Memory)?,
        });
    }
    Ok(OffsetSnapshot { states })
}

fn apply_uniform_offsets(
    device: &impl OffsetDevice,
    offsets: ClockOffsets,
) -> Result<OffsetTransition, NvidiaError> {
    let targets = device
        .states()
        .iter()
        .map(|state| (*state, offsets))
        .collect::<Vec<_>>();
    apply_offset_targets(device, &targets)
}

fn apply_offset_targets(
    device: &impl OffsetDevice,
    targets: &[(PerformanceState, ClockOffsets)],
) -> Result<OffsetTransition, NvidiaError> {
    let previous = snapshot_device(device)?;

    // Validate the complete transaction before the first mutation.
    for (state, target) in targets {
        let before = previous
            .state(*state)
            .ok_or(NvidiaError::MissingSnapshotState(*state))?;
        for domain in [ClockDomain::Core, ClockDomain::Memory] {
            let requested_mhz = target.for_domain(domain);
            let range = before.reading(domain);
            if !range.accepts(requested_mhz) {
                return Err(NvidiaError::OffsetOutOfRange {
                    state: *state,
                    domain,
                    requested_mhz,
                    minimum_mhz: range.minimum_mhz,
                    maximum_mhz: range.maximum_mhz,
                });
            }
        }
    }

    let mut mutations = Vec::<(PerformanceState, ClockDomain, i32)>::new();
    for (state, target) in targets {
        let before = previous
            .state(*state)
            .ok_or(NvidiaError::MissingSnapshotState(*state))?;
        for domain in [ClockDomain::Core, ClockDomain::Memory] {
            let prior_mhz = before.reading(domain).current_mhz;
            let requested_mhz = target.for_domain(domain);
            if prior_mhz == requested_mhz {
                continue;
            }

            if let Err(cause) = device.write_offset(*state, domain, requested_mhz) {
                return Err(rollback_after_failure(device, &mutations, cause));
            }
            // The setter may have mutated the device even when readback fails.
            mutations.push((*state, domain, prior_mhz));

            match device.read_offset(*state, domain) {
                Ok(readback) if readback.current_mhz == requested_mhz => {}
                Ok(readback) => {
                    let cause = NvidiaError::ReadbackMismatch {
                        state: *state,
                        domain,
                        expected_mhz: requested_mhz,
                        actual_mhz: readback.current_mhz,
                    };
                    return Err(rollback_after_failure(device, &mutations, cause));
                }
                Err(cause) => {
                    return Err(rollback_after_failure(device, &mutations, cause));
                }
            }
        }
    }

    let current = match snapshot_device(device) {
        Ok(snapshot) => snapshot,
        Err(cause) => return Err(rollback_after_failure(device, &mutations, cause)),
    };
    for (state, target) in targets {
        let actual = match current.state(*state) {
            Some(actual) => actual,
            None => {
                return Err(rollback_after_failure(
                    device,
                    &mutations,
                    NvidiaError::MissingSnapshotState(*state),
                ));
            }
        };
        for domain in [ClockDomain::Core, ClockDomain::Memory] {
            let expected_mhz = target.for_domain(domain);
            let actual_mhz = actual.reading(domain).current_mhz;
            if actual_mhz != expected_mhz {
                return Err(rollback_after_failure(
                    device,
                    &mutations,
                    NvidiaError::ReadbackMismatch {
                        state: *state,
                        domain,
                        expected_mhz,
                        actual_mhz,
                    },
                ));
            }
        }
    }

    Ok(OffsetTransition { previous, current })
}

fn rollback_after_failure(
    device: &impl OffsetDevice,
    mutations: &[(PerformanceState, ClockDomain, i32)],
    cause: NvidiaError,
) -> NvidiaError {
    let mut rollback_failures = Vec::new();
    for (state, domain, prior_mhz) in mutations.iter().rev() {
        if let Err(error) = device.write_offset(*state, *domain, *prior_mhz) {
            rollback_failures.push(format!("{state} {domain} write: {error}"));
            continue;
        }
        match device.read_offset(*state, *domain) {
            Ok(readback) if readback.current_mhz == *prior_mhz => {}
            Ok(readback) => rollback_failures.push(format!(
                "{state} {domain} readback: expected {prior_mhz} MHz, got {} MHz",
                readback.current_mhz
            )),
            Err(error) => rollback_failures.push(format!("{state} {domain} readback: {error}")),
        }
    }

    if rollback_failures.is_empty() {
        NvidiaError::TransactionRolledBack {
            cause: Box::new(cause),
        }
    } else {
        NvidiaError::TransactionRollbackFailed {
            cause: Box::new(cause),
            rollback_failures,
        }
    }
}

fn read_pci_identity(root: &Path) -> Result<PciIdentity, NvidiaError> {
    let device_root = root.join("sys/bus/pci/devices").join(GPU_PCI_BUS_ID);
    Ok(PciIdentity {
        vendor: read_hex_u16(&device_root.join("vendor"))?,
        device: read_hex_u16(&device_root.join("device"))?,
        subsystem_vendor: read_hex_u16(&device_root.join("subsystem_vendor"))?,
        subsystem_device: read_hex_u16(&device_root.join("subsystem_device"))?,
    })
}

fn read_hex_u16(path: &Path) -> Result<u16, NvidiaError> {
    let value = fs::read_to_string(path).map_err(|source| NvidiaError::Io {
        path: path.to_owned(),
        source,
    })?;
    let trimmed = value.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u16::from_str_radix(digits, 16).map_err(|_| NvidiaError::InvalidSysfsHex {
        path: path.to_owned(),
        value: trimmed.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    use super::*;

    type Key = (PerformanceState, ClockDomain);

    #[test]
    fn only_runtime_or_device_loss_errors_invalidate_nvml_session() {
        let nvml = |code| NvidiaError::Nvml {
            operation: "test",
            code,
            message: "injected".to_owned(),
        };
        for code in [1, 9, 15, 16, 18, 27] {
            assert!(nvml(code).invalidates_session(), "code={code}");
        }
        for code in [2, NVML_ERROR_NOT_SUPPORTED, 4, 10, 999] {
            assert!(!nvml(code).invalidates_session(), "code={code}");
        }
        assert!(!NvidiaError::UnavailableCapability { operation: "test" }.invalidates_session());
    }

    struct FakeOffsetDevice {
        states: Vec<PerformanceState>,
        values: RefCell<BTreeMap<Key, OffsetReading>>,
        writes: RefCell<Vec<(Key, i32)>>,
        write_attempts: Cell<usize>,
        fail_write_at: Cell<Option<usize>>,
        fail_rollback_value: Cell<Option<i32>>,
    }

    impl FakeOffsetDevice {
        fn new(states: &[PerformanceState], core: i32, memory: i32) -> Self {
            let mut values = BTreeMap::new();
            for state in states {
                values.insert(
                    (*state, ClockDomain::Core),
                    OffsetReading {
                        current_mhz: core,
                        minimum_mhz: -1_000,
                        maximum_mhz: 1_000,
                    },
                );
                values.insert(
                    (*state, ClockDomain::Memory),
                    OffsetReading {
                        current_mhz: memory,
                        minimum_mhz: -2_000,
                        maximum_mhz: 6_000,
                    },
                );
            }
            Self {
                states: states.to_vec(),
                values: RefCell::new(values),
                writes: RefCell::new(Vec::new()),
                write_attempts: Cell::new(0),
                fail_write_at: Cell::new(None),
                fail_rollback_value: Cell::new(None),
            }
        }

        fn offsets(&self, state: PerformanceState) -> ClockOffsets {
            let values = self.values.borrow();
            ClockOffsets {
                core_mhz: values[&(state, ClockDomain::Core)].current_mhz,
                memory_mhz: values[&(state, ClockDomain::Memory)].current_mhz,
            }
        }
    }

    impl OffsetDevice for FakeOffsetDevice {
        fn states(&self) -> &[PerformanceState] {
            &self.states
        }

        fn read_offset(
            &self,
            state: PerformanceState,
            domain: ClockDomain,
        ) -> Result<OffsetReading, NvidiaError> {
            Ok(self.values.borrow()[&(state, domain)])
        }

        fn write_offset(
            &self,
            state: PerformanceState,
            domain: ClockDomain,
            offset_mhz: i32,
        ) -> Result<(), NvidiaError> {
            let attempt = self.write_attempts.get();
            self.write_attempts.set(attempt + 1);
            if self.fail_write_at.get() == Some(attempt) {
                self.fail_write_at.set(None);
                return Err(NvidiaError::InjectedFailure("forward write"));
            }
            if self.fail_rollback_value.get() == Some(offset_mhz) {
                return Err(NvidiaError::InjectedFailure("rollback write"));
            }

            self.writes.borrow_mut().push(((state, domain), offset_mhz));
            self.values
                .borrow_mut()
                .get_mut(&(state, domain))
                .expect("test key exists")
                .current_mhz = offset_mhz;
            Ok(())
        }
    }

    #[test]
    fn oem_transaction_updates_every_state_and_domain() {
        let device = FakeOffsetDevice::new(&EXPECTED_STATES, 0, 0);
        let transition = apply_uniform_offsets(&device, ClockOffsets::OEM_TURBO).unwrap();

        assert_eq!(transition.previous.states.len(), EXPECTED_STATES.len());
        assert_eq!(transition.current.states.len(), EXPECTED_STATES.len());
        for state in EXPECTED_STATES {
            assert_eq!(device.offsets(state), ClockOffsets::OEM_TURBO);
        }
        assert_eq!(device.writes.borrow().len(), EXPECTED_STATES.len() * 2);
    }

    #[test]
    fn failure_mid_transaction_restores_the_complete_snapshot() {
        let device = FakeOffsetDevice::new(&EXPECTED_STATES, 7, 13);
        device.fail_write_at.set(Some(4));

        let error = apply_uniform_offsets(&device, ClockOffsets::OEM_TURBO).unwrap_err();
        assert!(matches!(error, NvidiaError::TransactionRolledBack { .. }));
        for state in EXPECTED_STATES {
            assert_eq!(
                device.offsets(state),
                ClockOffsets {
                    core_mhz: 7,
                    memory_mhz: 13,
                }
            );
        }
    }

    #[test]
    fn rollback_failure_is_never_reported_as_successful_rollback() {
        let device = FakeOffsetDevice::new(&EXPECTED_STATES, 7, 13);
        device.fail_write_at.set(Some(2));
        device.fail_rollback_value.set(Some(7));

        let error = apply_uniform_offsets(&device, ClockOffsets::OEM_TURBO).unwrap_err();
        assert!(matches!(
            error,
            NvidiaError::TransactionRollbackFailed { .. }
        ));
    }

    #[test]
    fn out_of_range_request_performs_no_writes() {
        let device = FakeOffsetDevice::new(&EXPECTED_STATES, 0, 0);
        let error = apply_uniform_offsets(
            &device,
            ClockOffsets {
                core_mhz: 1_001,
                memory_mhz: 200,
            },
        )
        .unwrap_err();

        assert!(matches!(error, NvidiaError::OffsetOutOfRange { .. }));
        assert!(device.writes.borrow().is_empty());
    }

    #[test]
    fn reset_is_a_verified_transaction() {
        let device = FakeOffsetDevice::new(&EXPECTED_STATES, 100, 200);
        apply_uniform_offsets(&device, ClockOffsets::RESET).unwrap();
        for state in EXPECTED_STATES {
            assert_eq!(device.offsets(state), ClockOffsets::RESET);
        }
    }

    #[test]
    fn nvml_clock_offset_layout_matches_cuda_13_header() {
        assert_eq!(std::mem::size_of::<NvmlClockOffset>(), 24);
        assert_eq!(NVML_CLOCK_OFFSET_VERSION, 0x0100_0018);
    }

    #[test]
    #[ignore = "requires the exact PHN16-72 RTX 4070 Laptop target"]
    fn exact_target_read_only_probe() {
        let controller = NvidiaController::discover().unwrap();
        assert_eq!(controller.supported_states(), EXPECTED_STATES);
        assert_eq!(controller.snapshot_offsets().unwrap().states.len(), 5);
        let power = controller.power_telemetry().unwrap();
        assert!(power.minimum_limit_mw <= power.default_limit_mw);
        assert!(power.default_limit_mw <= power.maximum_limit_mw);
        assert!(power.minimum_limit_mw <= power.enforced_limit_mw);
        assert!(power.enforced_limit_mw <= power.maximum_limit_mw);
    }
}
