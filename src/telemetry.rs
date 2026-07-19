//! Read-only system telemetry for the ASense UI.

use crate::hardware::{
    AcerHardware, FanChannelState, FanRpmChannel, FanState, HardwareError, PlatformProfile,
};
use crate::nvidia::{
    NvidiaController, NvidiaLiveTelemetry, NvidiaPciDevice, NvidiaStaticInfo, PciIdentity,
    RTX_4070_LAPTOP_CUDA_CORE_COUNT, RTX_4070_LAPTOP_SM_COUNT, discover_nvidia_pci_device,
};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const HARDWARE_FREQUENCY_REFRESH_SAMPLES: u8 = 5;
const NVIDIA_SLOW_REFRESH_SAMPLES: u8 = 10;
const NVIDIA_RETRY_MAX_SAMPLES: u8 = 30;

#[derive(Debug)]
pub enum TelemetryError {
    Hardware(HardwareError),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidData {
        source: &'static str,
        detail: String,
    },
}

impl fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hardware(error) => write!(f, "hardware telemetry: {error}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} {}: {source}", path.display()),
            Self::InvalidData { source, detail } => {
                write!(f, "invalid {source} telemetry: {detail}")
            }
        }
    }
}

impl Error for TelemetryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Hardware(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<HardwareError> for TelemetryError {
    fn from(value: HardwareError) -> Self {
        Self::Hardware(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    fn idle_total(self) -> u64 {
        self.idle.saturating_add(self.iowait)
    }

    fn total(self) -> u64 {
        self.user
            .saturating_add(self.nice)
            .saturating_add(self.system)
            .saturating_add(self.idle)
            .saturating_add(self.iowait)
            .saturating_add(self.irq)
            .saturating_add(self.softirq)
            .saturating_add(self.steal)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NvidiaTelemetry {
    pub temperature_c: Option<f32>,
    pub utilization_percent: Option<f32>,
    pub power_w: Option<f32>,
    pub pstate: Option<String>,
    pub memory_used_mib: Option<u64>,
    pub memory_total_mib: Option<u64>,
    pub graphics_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    pub maximum_graphics_clock_mhz: Option<u32>,
    pub maximum_memory_clock_mhz: Option<u32>,
    pub model: Option<String>,
    pub driver_version: Option<String>,
    pub pci_bus_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CpuHardwareInfo {
    pub model: Option<String>,
    pub architecture: Option<String>,
    pub family: Option<u32>,
    pub l3_cache_kib: Option<u64>,
    pub physical_cores: Option<u32>,
    pub logical_processors: Option<u32>,
    pub performance_cores: Option<u32>,
    pub efficiency_cores: Option<u32>,
    pub current_frequency_mhz: Option<u32>,
    pub maximum_frequency_mhz: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GpuHardwareInfo {
    pub model: Option<String>,
    pub vram_total_mib: Option<u64>,
    pub driver_version: Option<String>,
    pub pci_bus_id: Option<String>,
    pub streaming_multiprocessors: Option<u32>,
    pub cuda_cores: Option<u32>,
    pub current_graphics_clock_mhz: Option<u32>,
    pub maximum_graphics_clock_mhz: Option<u32>,
    pub current_memory_clock_mhz: Option<u32>,
    pub maximum_memory_clock_mhz: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemoryHardwareInfo {
    pub total_mib: Option<u64>,
    pub speed_mt_s: Option<u32>,
    pub memory_type: Option<String>,
    pub channels: Option<u32>,
    pub modules: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HardwareInfo {
    pub cpu: CpuHardwareInfo,
    pub gpu: GpuHardwareInfo,
    pub memory: MemoryHardwareInfo,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuTelemetry {
    /// True when PCI runtime PM reports a healthy RTD3 sleep state. Live NVML
    /// fields are intentionally absent in this state.
    pub sleeping: bool,
    pub temperature_c: Option<f32>,
    pub utilization_percent: Option<f32>,
    pub power_w: Option<f32>,
    pub pstate: Option<String>,
    pub memory_used_mib: Option<u64>,
    pub memory_total_mib: Option<u64>,
    pub graphics_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    pub core_offset_mhz: Option<i32>,
    pub memory_offset_mhz: Option<i32>,
    /// `Some(false)` means NVML returned a mixed/partial P-state profile.
    pub offsets_uniform: Option<bool>,
    pub enforced_power_limit_w: Option<f32>,
    pub maximum_power_limit_w: Option<f32>,
    pub clock_event_reasons: Option<u64>,
    /// Diagnostic only. A failed optional NVML query must not hide Acer fan
    /// and temperature telemetry from the UI.
    pub nvidia_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemTelemetry {
    pub cpu_temperature_c: Option<f32>,
    /// `None` on the first sample because CPU load needs two `/proc/stat`
    /// observations.
    pub cpu_utilization_percent: Option<f32>,
    pub memory_used_mib: u64,
    pub memory_total_mib: u64,
    pub gpu: GpuTelemetry,
    /// The two controllable channels used by the compact fan UI.  A machine
    /// with profiles but no fan-control interface reports an unavailable
    /// zeroed state instead of losing the complete telemetry sample.
    pub fans: FanState,
    /// All read-only RPM channels exported by Acer hwmon.  Channels after the
    /// CPU/GPU pair remain telemetry-only; the UI can render fan 3 alongside
    /// the GPU gauge and fan 4+ in advanced diagnostics.
    pub fan_rpm_channels: Vec<FanRpmChannel>,
    /// Raw live kernel/WMI token.  Unknown tokens remain useful telemetry and
    /// must not be rejected merely because the legacy five-profile enum does
    /// not have a variant for them.
    pub profile_raw: Option<String>,
    /// Compatibility view for the reference PHN16-72 controls.  `None` means
    /// that the profile is absent or uses a live token unknown to the fixed
    /// v0.1 enum.
    pub profile: Option<PlatformProfile>,
    pub hardware: HardwareInfo,
    pub power_supply: PowerSupplyTelemetry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatteryStatus {
    Charging,
    Discharging,
    Full,
    NotCharging,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PowerSupplyTelemetry {
    pub battery_percent: Option<u8>,
    pub battery_status: Option<BatteryStatus>,
    pub ac_online: Option<bool>,
    pub usb_power_online: Option<bool>,
}

pub struct TelemetryReader {
    previous_cpu_times: Option<CpuTimes>,
    nvidia: Option<NvidiaController>,
    nvidia_static: Option<NvidiaStaticInfo>,
    nvidia_discovery_error: Option<String>,
    nvidia_pci_bus_id: Option<String>,
    nvidia_pci_identity: Option<PciIdentity>,
    nvidia_exact_oem_target: bool,
    nvidia_runtime_sleeping: bool,
    nvidia_retry: SampleBackoff,
    hardware_info: Option<HardwareInfo>,
    cpu_online_ids: Option<BTreeSet<u32>>,
    hardware_frequency_refresh: u8,
    nvidia_slow: NvidiaSlowTelemetry,
    nvidia_slow_refresh: u8,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct NvidiaSlowTelemetry {
    core_offset_mhz: Option<i32>,
    memory_offset_mhz: Option<i32>,
    offsets_uniform: Option<bool>,
    session_lost: bool,
    errors: Vec<String>,
}

/// Retry timing expressed in telemetry samples. The caller polls at 1 Hz, so
/// this avoids sleeping inside the telemetry worker while bounding discovery
/// attempts to at most one every 30 seconds during a long driver outage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SampleBackoff {
    samples_until_retry: u8,
    next_delay: u8,
}

impl Default for SampleBackoff {
    fn default() -> Self {
        Self {
            samples_until_retry: 0,
            next_delay: 1,
        }
    }
}

impl SampleBackoff {
    fn retry_due(&mut self) -> bool {
        if self.samples_until_retry > 0 {
            self.samples_until_retry -= 1;
        }
        self.samples_until_retry == 0
    }

    fn record_failure(&mut self) {
        self.samples_until_retry = self.next_delay;
        self.next_delay = self
            .next_delay
            .saturating_mul(2)
            .min(NVIDIA_RETRY_MAX_SAMPLES);
    }

    fn record_success(&mut self) {
        *self = Self::default();
    }
}

impl Default for TelemetryReader {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryReader {
    pub fn new() -> Self {
        Self {
            previous_cpu_times: None,
            nvidia: None,
            nvidia_static: None,
            nvidia_discovery_error: None,
            nvidia_pci_bus_id: None,
            nvidia_pci_identity: None,
            nvidia_exact_oem_target: false,
            nvidia_runtime_sleeping: false,
            nvidia_retry: SampleBackoff::default(),
            hardware_info: None,
            cpu_online_ids: None,
            hardware_frequency_refresh: 0,
            nvidia_slow: NvidiaSlowTelemetry::default(),
            nvidia_slow_refresh: 0,
        }
    }

    fn discover_nvidia(&mut self, pci_device: &NvidiaPciDevice, root: &Path) {
        let probe_offsets = self.nvidia_slow_refresh == 0;
        match NvidiaController::discover_telemetry(pci_device, root, probe_offsets) {
            Ok(controller) => {
                if self.nvidia_static.is_none() {
                    self.nvidia_static = Some(controller.static_info());
                }
                self.nvidia = Some(controller);
                self.nvidia_discovery_error = None;
                self.nvidia_retry.record_success();
            }
            Err(error) => {
                self.nvidia = None;
                self.nvidia_static = None;
                self.nvidia_discovery_error = Some(error.to_string());
                self.nvidia_slow = NvidiaSlowTelemetry::default();
                self.nvidia_slow_refresh = 0;
                self.nvidia_retry.record_failure();
            }
        }
    }

    fn refresh_nvidia_lifecycle(&mut self, root: &Path) {
        let Some(pci_device) = discover_nvidia_pci_device(root) else {
            if self.nvidia_pci_bus_id.is_some() {
                self.hardware_info = None;
            }
            self.nvidia_static = None;
            self.nvidia_pci_identity = None;
            self.drop_nvidia_for_runtime_state(None, false);
            return;
        };

        let identity_changed = self.nvidia_pci_bus_id.as_deref() != Some(&pci_device.bus_id)
            || self.nvidia_pci_identity != Some(pci_device.identity);
        if identity_changed {
            self.hardware_info = None;
            self.nvidia_static = None;
        }
        self.nvidia_pci_bus_id = Some(pci_device.bus_id.clone());
        self.nvidia_pci_identity = Some(pci_device.identity);
        self.nvidia_exact_oem_target = pci_device.is_exact_oem_target();
        self.nvidia_runtime_sleeping =
            pci_device.runtime_status == crate::nvidia::NvidiaRuntimeStatus::Suspended;

        if !pci_device.runtime_status.permits_live_nvml() {
            self.drop_nvidia_for_runtime_state(
                Some(&pci_device.bus_id),
                pci_device.runtime_status == crate::nvidia::NvidiaRuntimeStatus::Suspended,
            );
            return;
        }

        let wrong_device = identity_changed
            || self
                .nvidia
                .as_ref()
                .is_some_and(|controller| controller.pci_bus_id() != pci_device.bus_id);
        if wrong_device {
            self.nvidia = None;
            self.nvidia_static = None;
            self.nvidia_slow = NvidiaSlowTelemetry::default();
            self.nvidia_slow_refresh = 0;
            self.nvidia_retry.record_success();
        }
        if self.nvidia.is_none() && self.nvidia_retry.retry_due() {
            self.discover_nvidia(&pci_device, root);
        }
    }

    fn drop_nvidia_for_runtime_state(&mut self, bus_id: Option<&str>, sleeping: bool) {
        self.nvidia = None;
        self.nvidia_discovery_error = None;
        self.nvidia_slow = NvidiaSlowTelemetry::default();
        self.nvidia_slow_refresh = 0;
        self.nvidia_retry.record_success();
        self.nvidia_pci_bus_id = bus_id.map(str::to_owned);
        self.nvidia_runtime_sleeping = sleeping;
        if bus_id.is_none() {
            self.nvidia_exact_oem_target = false;
        }
        // Suspended, transitional and unknown runtime states are all healthy
        // reasons to defer NVML. Only literal `suspended` is presented as
        // "GPU sleeping" in the UI.
    }

    fn lose_nvidia_session(&mut self, detail: String) {
        self.nvidia = None;
        self.nvidia_static = None;
        self.nvidia_discovery_error = Some(detail);
        self.nvidia_slow = NvidiaSlowTelemetry::default();
        self.nvidia_slow_refresh = 0;
        self.nvidia_retry.record_failure();
    }

    /// Forces the next samples to reopen NVML. This is intentionally cheap for
    /// callers handling resume or a known driver reload; ordinary sampling
    /// also detects invalid NVML sessions automatically.
    pub fn invalidate_nvidia_session(&mut self) {
        self.lose_nvidia_session("NVML session invalidated after resume".to_owned());
        self.nvidia_retry.record_success();
    }

    fn refresh_nvidia_slow(&mut self, controller: Option<&NvidiaController>) {
        let Some(controller) = controller else {
            self.nvidia_slow = NvidiaSlowTelemetry::default();
            self.nvidia_slow_refresh = 0;
            return;
        };
        if self.nvidia_slow_refresh > 0 {
            self.nvidia_slow_refresh -= 1;
            return;
        }
        self.nvidia_slow_refresh = NVIDIA_SLOW_REFRESH_SAMPLES;
        let mut slow = NvidiaSlowTelemetry::default();
        if !controller.supported_states().is_empty() {
            match controller.snapshot_offsets() {
                Ok(snapshot) => {
                    let first = snapshot.states.first();
                    let uniform = first.is_some_and(|first| {
                        snapshot.states.iter().all(|state| {
                            state.core.current_mhz == first.core.current_mhz
                                && state.memory.current_mhz == first.memory.current_mhz
                        })
                    });
                    slow.core_offset_mhz = uniform.then(|| {
                        first
                            .expect("uniform snapshot is non-empty")
                            .core
                            .current_mhz
                    });
                    slow.memory_offset_mhz = uniform.then(|| {
                        first
                            .expect("uniform snapshot is non-empty")
                            .memory
                            .current_mhz
                    });
                    slow.offsets_uniform = Some(uniform);
                }
                Err(error) => {
                    slow.session_lost = error.invalidates_session();
                    slow.errors.push(format!("NVML offsets: {error}"));
                }
            }
        }
        self.nvidia_slow = slow;
    }

    pub fn sample(&mut self, hardware: &AcerHardware) -> Result<SystemTelemetry, TelemetryError> {
        self.refresh_nvidia_lifecycle(hardware.root());
        // Keep NVML sample-scoped. Dropping this local at the end of the
        // sample calls nvmlShutdown and leaves RTD3 free to suspend the dGPU
        // after the external workload disappears.
        let mut nvidia_controller = self.nvidia.take();
        self.refresh_nvidia_slow(nvidia_controller.as_ref());
        if self.nvidia_slow.session_lost {
            let detail = self.nvidia_slow.errors.join("; ");
            self.lose_nvidia_session(detail);
            nvidia_controller = None;
        }
        let proc_stat = rooted(hardware.root(), "proc/stat");
        let cpu_times = read_cpu_times(&proc_stat)?;
        let cpu_utilization_percent = self
            .previous_cpu_times
            .and_then(|previous| cpu_utilization(previous, cpu_times));
        self.previous_cpu_times = Some(cpu_times);
        let (memory_used_mib, memory_total_mib) =
            read_memory_usage(&rooted(hardware.root(), "proc/meminfo"))?;

        let acer_cpu_temp = hardware
            .read_acer_temp_millidegrees(1)
            .ok()
            .and_then(millidegrees_to_celsius);
        let acer_gpu_temp = hardware
            .read_acer_temp_millidegrees(2)
            .ok()
            .and_then(millidegrees_to_celsius);
        let cpu_temperature_c =
            find_labeled_temperature(hardware.root(), "coretemp", "Package id 0")?
                .or(acer_cpu_temp);

        let (mut nvidia, mut power, nvidia_errors, lost_session) = match nvidia_controller.as_ref()
        {
            Some(controller) => {
                let live = controller.live_telemetry();
                let mut errors = self
                    .nvidia_static
                    .as_ref()
                    .map(|info| info.errors.clone())
                    .unwrap_or_default();
                errors.extend(live.errors.iter().cloned());
                errors.extend(self.nvidia_slow.errors.iter().cloned());
                let mut lost_session = live.session_lost;
                let power = match controller.power_telemetry() {
                    Ok(power) => Some(power),
                    Err(error) => {
                        lost_session |= error.invalidates_session();
                        errors.push(format!("NVML power: {error}"));
                        None
                    }
                };
                (
                    Some(nvidia_telemetry(&live, self.nvidia_static.as_ref())),
                    power,
                    errors,
                    lost_session,
                )
            }
            None => (
                None,
                None,
                self.nvidia_discovery_error.clone().into_iter().collect(),
                false,
            ),
        };
        if lost_session {
            let detail = if nvidia_errors.is_empty() {
                "NVML session became unavailable".to_owned()
            } else {
                nvidia_errors.join("; ")
            };
            self.lose_nvidia_session(detail);
            nvidia = None;
            power = None;
        }
        let hardware_info =
            self.hardware_snapshot(hardware.root(), memory_total_mib, nvidia.as_ref());
        let core_offset_mhz = self.nvidia_slow.core_offset_mhz;
        let memory_offset_mhz = self.nvidia_slow.memory_offset_mhz;
        let offsets_uniform = self.nvidia_slow.offsets_uniform;
        let nvidia_error = nvidia_errors
            .into_iter()
            .reduce(|left, right| format!("{left}; {right}"));
        let gpu = match nvidia {
            Some(value) => GpuTelemetry {
                sleeping: false,
                temperature_c: value.temperature_c.or(acer_gpu_temp),
                utilization_percent: value.utilization_percent,
                power_w: value.power_w,
                pstate: value.pstate,
                memory_used_mib: value.memory_used_mib,
                memory_total_mib: value.memory_total_mib,
                graphics_clock_mhz: value.graphics_clock_mhz,
                memory_clock_mhz: value.memory_clock_mhz,
                core_offset_mhz,
                memory_offset_mhz,
                offsets_uniform,
                enforced_power_limit_w: power
                    .map(|telemetry| telemetry.enforced_limit_mw as f32 / 1_000.0),
                maximum_power_limit_w: power
                    .map(|telemetry| telemetry.maximum_limit_mw as f32 / 1_000.0),
                clock_event_reasons: power.map(|telemetry| telemetry.clock_event_reasons.bits()),
                nvidia_error,
            },
            None => GpuTelemetry {
                sleeping: self.nvidia_runtime_sleeping,
                temperature_c: acer_gpu_temp,
                utilization_percent: None,
                power_w: None,
                pstate: None,
                memory_used_mib: None,
                memory_total_mib: None,
                graphics_clock_mhz: None,
                memory_clock_mhz: None,
                core_offset_mhz,
                memory_offset_mhz,
                offsets_uniform,
                enforced_power_limit_w: power
                    .map(|telemetry| telemetry.enforced_limit_mw as f32 / 1_000.0),
                maximum_power_limit_w: power
                    .map(|telemetry| telemetry.maximum_limit_mw as f32 / 1_000.0),
                clock_event_reasons: power.map(|telemetry| telemetry.clock_event_reasons.bits()),
                nvidia_error,
            },
        };

        let fan_rpm_channels = hardware.fan_rpm_channels();
        let fans = hardware
            .read_fan_state()
            .unwrap_or_else(|_| unavailable_fan_state(&fan_rpm_channels));
        let profile_raw = hardware.current_profile_raw().ok();
        let profile = profile_raw
            .as_deref()
            .and_then(|raw| PlatformProfile::from_sysfs(raw).ok());

        Ok(SystemTelemetry {
            cpu_temperature_c,
            cpu_utilization_percent,
            memory_used_mib,
            memory_total_mib,
            gpu,
            fans,
            fan_rpm_channels,
            profile_raw,
            profile,
            hardware: hardware_info,
            power_supply: read_power_supply(hardware.root()),
        })
    }

    fn hardware_snapshot(
        &mut self,
        root: &Path,
        memory_total_mib: u64,
        nvidia: Option<&NvidiaTelemetry>,
    ) -> HardwareInfo {
        let exact_oem_gpu = self.nvidia_exact_oem_target;
        let mut info = match self.hardware_info.take() {
            Some(mut cached) => {
                let current_online_ids = cpu_topology_key(root);
                if current_online_ids != self.cpu_online_ids {
                    refresh_cpu_topology(root, &mut cached.cpu);
                    self.cpu_online_ids = current_online_ids;
                }
                if self.hardware_frequency_refresh <= 1 {
                    refresh_cpu_frequencies(root, &mut cached.cpu);
                    self.hardware_frequency_refresh = HARDWARE_FREQUENCY_REFRESH_SAMPLES;
                } else {
                    self.hardware_frequency_refresh -= 1;
                }
                cached
            }
            None => {
                self.hardware_frequency_refresh = HARDWARE_FREQUENCY_REFRESH_SAMPLES;
                self.cpu_online_ids = cpu_topology_key(root);
                read_hardware_info_for_gpu(
                    root,
                    memory_total_mib,
                    self.nvidia_pci_bus_id.as_deref(),
                )
            }
        };
        info.memory.total_mib = Some(memory_total_mib);
        if let Some(bus_id) = self.nvidia_pci_bus_id.as_ref() {
            info.gpu.pci_bus_id = Some(normalize_pci_bus_id(bus_id));
        }
        if let Some(nvidia) = nvidia {
            if let Some(model) = nvidia.model.as_ref() {
                info.gpu.model = Some(model.clone());
            }
            if let Some(total) = nvidia.memory_total_mib {
                info.gpu.vram_total_mib = Some(total);
            }
            if let Some(version) = nvidia.driver_version.as_ref() {
                info.gpu.driver_version = Some(version.clone());
            }
            if let Some(bus_id) = nvidia.pci_bus_id.as_ref() {
                info.gpu.pci_bus_id = Some(normalize_pci_bus_id(bus_id));
            }
            info.gpu.current_graphics_clock_mhz = nvidia.graphics_clock_mhz;
            info.gpu.maximum_graphics_clock_mhz = nvidia.maximum_graphics_clock_mhz;
            info.gpu.current_memory_clock_mhz = nvidia.memory_clock_mhz;
            info.gpu.maximum_memory_clock_mhz = nvidia.maximum_memory_clock_mhz;
        } else {
            // Never present clocks from the previous active NVML epoch as
            // current after the dGPU has entered RTD3.
            info.gpu.current_graphics_clock_mhz = None;
            info.gpu.current_memory_clock_mhz = None;
        }
        if exact_oem_gpu {
            // NvidiaController is constructed only after exact vendor/device
            // and Acer subsystem IDs have been verified in sysfs.
            info.gpu.streaming_multiprocessors = Some(RTX_4070_LAPTOP_SM_COUNT);
            info.gpu.cuda_cores = Some(RTX_4070_LAPTOP_CUDA_CORE_COUNT);
        }
        self.hardware_info = Some(info.clone());
        info
    }
}

fn read_power_supply(root: &Path) -> PowerSupplyTelemetry {
    let Ok(entries) = fs::read_dir(rooted(root, "sys/class/power_supply")) else {
        return PowerSupplyTelemetry::default();
    };
    let mut result = PowerSupplyTelemetry::default();
    let mut saw_ac = false;
    let mut saw_usb = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(kind) = read_trimmed(&path.join("type")) else {
            continue;
        };
        match kind.as_str() {
            "Battery" if result.battery_percent.is_none() => {
                result.battery_percent =
                    read_number::<u8>(&path.join("capacity")).map(|value| value.min(100));
                result.battery_status =
                    read_trimmed(&path.join("status")).map(|status| match status.as_str() {
                        "Charging" => BatteryStatus::Charging,
                        "Discharging" => BatteryStatus::Discharging,
                        "Full" => BatteryStatus::Full,
                        "Not charging" => BatteryStatus::NotCharging,
                        _ => BatteryStatus::Unknown,
                    });
            }
            "Mains" => {
                if let Some(online) = read_number::<u8>(&path.join("online")) {
                    saw_ac = true;
                    result.ac_online = Some(result.ac_online.unwrap_or(false) || online != 0);
                }
            }
            "USB" | "USB_C" | "USB_PD" => {
                if let Some(online) = read_number::<u8>(&path.join("online")) {
                    saw_usb = true;
                    result.usb_power_online =
                        Some(result.usb_power_online.unwrap_or(false) || online != 0);
                }
            }
            _ => {}
        }
    }
    if !saw_ac {
        result.ac_online = None;
    }
    if !saw_usb {
        result.usb_power_online = None;
    }
    result
}

pub fn parse_cpu_times(proc_stat: &str) -> Result<CpuTimes, TelemetryError> {
    let line = proc_stat
        .lines()
        .find(|line| line.starts_with("cpu "))
        .ok_or_else(|| TelemetryError::InvalidData {
            source: "/proc/stat",
            detail: "aggregate cpu line is missing".to_owned(),
        })?;
    let values = line
        .split_ascii_whitespace()
        .skip(1)
        .take(8)
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| TelemetryError::InvalidData {
                    source: "/proc/stat",
                    detail: format!("invalid counter {value:?}"),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() < 8 {
        return Err(TelemetryError::InvalidData {
            source: "/proc/stat",
            detail: format!(
                "expected at least 8 aggregate counters, got {}",
                values.len()
            ),
        });
    }
    Ok(CpuTimes {
        user: values[0],
        nice: values[1],
        system: values[2],
        idle: values[3],
        iowait: values[4],
        irq: values[5],
        softirq: values[6],
        steal: values[7],
    })
}

pub fn cpu_utilization(previous: CpuTimes, current: CpuTimes) -> Option<f32> {
    let total_delta = current.total().checked_sub(previous.total())?;
    if total_delta == 0 {
        return None;
    }
    let idle_delta = current.idle_total().checked_sub(previous.idle_total())?;
    let active_delta = total_delta.saturating_sub(idle_delta);
    Some((active_delta as f64 * 100.0 / total_delta as f64) as f32)
}

fn nvidia_telemetry(
    live: &NvidiaLiveTelemetry,
    static_info: Option<&NvidiaStaticInfo>,
) -> NvidiaTelemetry {
    NvidiaTelemetry {
        temperature_c: live.temperature_c.map(|value| value as f32),
        utilization_percent: live.utilization_percent.map(|value| value as f32),
        power_w: live.power_draw_mw.map(|value| value as f32 / 1_000.0),
        pstate: live.pstate.map(|state| state.to_string()),
        memory_used_mib: live.memory_used_mib,
        memory_total_mib: live.memory_total_mib,
        graphics_clock_mhz: live.graphics_clock_mhz,
        memory_clock_mhz: live.memory_clock_mhz,
        maximum_graphics_clock_mhz: static_info.and_then(|info| info.maximum_graphics_clock_mhz),
        maximum_memory_clock_mhz: static_info.and_then(|info| info.maximum_memory_clock_mhz),
        model: static_info.and_then(|info| info.model.clone()),
        driver_version: static_info.and_then(|info| info.driver_version.clone()),
        pci_bus_id: static_info.and_then(|info| info.pci_bus_id.clone()),
    }
}

fn read_cpu_times(path: &Path) -> Result<CpuTimes, TelemetryError> {
    let input = fs::read_to_string(path).map_err(|source| TelemetryError::Io {
        operation: "read CPU counters",
        path: path.to_path_buf(),
        source,
    })?;
    parse_cpu_times(&input)
}

fn read_memory_usage(path: &Path) -> Result<(u64, u64), TelemetryError> {
    let input = fs::read_to_string(path).map_err(|source| TelemetryError::Io {
        operation: "read memory counters",
        path: path.to_path_buf(),
        source,
    })?;
    parse_memory_usage(&input)
}

pub fn parse_memory_usage(meminfo: &str) -> Result<(u64, u64), TelemetryError> {
    let mut total_kib = None;
    let mut available_kib = None;
    for line in meminfo.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        let target = match name {
            "MemTotal:" => &mut total_kib,
            "MemAvailable:" => &mut available_kib,
            _ => continue,
        };
        let value = fields
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| TelemetryError::InvalidData {
                source: "/proc/meminfo",
                detail: format!("invalid {name} value"),
            })?;
        if fields.next() != Some("kB") {
            return Err(TelemetryError::InvalidData {
                source: "/proc/meminfo",
                detail: format!("{name} is not expressed in kB"),
            });
        }
        *target = Some(value);
    }
    let total_kib = total_kib.ok_or_else(|| TelemetryError::InvalidData {
        source: "/proc/meminfo",
        detail: "MemTotal is missing".to_owned(),
    })?;
    let available_kib = available_kib.ok_or_else(|| TelemetryError::InvalidData {
        source: "/proc/meminfo",
        detail: "MemAvailable is missing".to_owned(),
    })?;
    if available_kib > total_kib {
        return Err(TelemetryError::InvalidData {
            source: "/proc/meminfo",
            detail: "MemAvailable exceeds MemTotal".to_owned(),
        });
    }
    Ok(((total_kib - available_kib) / 1024, total_kib / 1024))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HybridCoreKind {
    Performance,
    Efficiency,
}

#[derive(Clone, Copy, Debug, Default)]
struct CoreRecord {
    kind: Option<HybridCoreKind>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MemoryModule {
    size_mib: u64,
    memory_type: Option<String>,
    speed_mt_s: Option<u32>,
    channel: Option<String>,
}

#[cfg(test)]
fn read_hardware_info(root: &Path, memory_total_mib: u64) -> HardwareInfo {
    read_hardware_info_for_gpu(root, memory_total_mib, None)
}

fn read_hardware_info_for_gpu(
    root: &Path,
    memory_total_mib: u64,
    selected_nvidia_bus_id: Option<&str>,
) -> HardwareInfo {
    let cpuinfo = fs::read_to_string(rooted(root, "proc/cpuinfo")).unwrap_or_default();
    let mut cpu = parse_cpu_hardware_info(&cpuinfo);
    apply_cpu_topology(root, &mut cpu);
    refresh_cpu_frequencies(root, &mut cpu);
    cpu.l3_cache_kib = read_l3_cache_kib(root);

    HardwareInfo {
        cpu,
        gpu: read_nvidia_proc_hardware(root, selected_nvidia_bus_id),
        memory: read_memory_hardware(root, memory_total_mib),
    }
}

fn parse_cpu_hardware_info(cpuinfo: &str) -> CpuHardwareInfo {
    let mut model = None;
    let mut family = None;
    let mut logical_processors = 0_u32;
    let mut physical_cores = BTreeSet::new();
    let mut current_frequency_mhz = None::<u32>;

    for block in cpuinfo.split("\n\n") {
        let mut is_processor = false;
        let mut package = None;
        let mut core = None;
        for line in block.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match name.trim() {
                "processor" => is_processor = value.parse::<u32>().is_ok(),
                "model name" if model.is_none() => {
                    let normalized = value.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
                    if !normalized.is_empty() {
                        model = Some(normalized);
                    }
                }
                "cpu family" if family.is_none() => family = value.parse::<u32>().ok(),
                "physical id" => package = value.parse::<i32>().ok(),
                "core id" => core = value.parse::<i32>().ok(),
                "cpu MHz" => {
                    if let Ok(value) = value.parse::<f64>()
                        && value.is_finite()
                        && (1.0..=20_000.0).contains(&value)
                    {
                        let value = value.round() as u32;
                        current_frequency_mhz =
                            Some(current_frequency_mhz.map_or(value, |current| current.max(value)));
                    }
                }
                _ => {}
            }
        }
        if is_processor {
            logical_processors = logical_processors.saturating_add(1);
            if let (Some(package), Some(core)) = (package, core) {
                physical_cores.insert((package, core));
            }
        }
    }

    CpuHardwareInfo {
        model,
        architecture: (logical_processors > 0).then(|| std::env::consts::ARCH.to_owned()),
        family,
        physical_cores: (!physical_cores.is_empty()).then_some(physical_cores.len() as u32),
        logical_processors: (logical_processors > 0).then_some(logical_processors),
        current_frequency_mhz,
        ..CpuHardwareInfo::default()
    }
}

fn apply_cpu_topology(root: &Path, info: &mut CpuHardwareInfo) {
    let ids = online_cpu_ids(root);
    if ids.is_empty() {
        return;
    }
    info.logical_processors = Some(ids.len() as u32);

    let performance_ids = read_cpu_set(root, "sys/bus/event_source/devices/cpu_core/cpus");
    let efficiency_ids = read_cpu_set(root, "sys/bus/event_source/devices/cpu_atom/cpus");
    let pmu_hybrid_map = performance_ids.is_some() && efficiency_ids.is_some();
    let mut cores = BTreeMap::<(i32, i32), CoreRecord>::new();

    for cpu in ids {
        let topology = rooted(root, &format!("sys/devices/system/cpu/cpu{cpu}/topology"));
        let Some(package) = read_number::<i32>(&topology.join("physical_package_id")) else {
            continue;
        };
        let Some(core) = read_number::<i32>(&topology.join("core_id")) else {
            continue;
        };
        let explicit_kind =
            read_number::<u8>(&topology.join("core_type")).and_then(|value| match value {
                1 => Some(HybridCoreKind::Efficiency),
                2 => Some(HybridCoreKind::Performance),
                _ => None,
            });
        let pmu_kind = if pmu_hybrid_map {
            match (
                performance_ids
                    .as_ref()
                    .is_some_and(|ids| ids.contains(&cpu)),
                efficiency_ids
                    .as_ref()
                    .is_some_and(|ids| ids.contains(&cpu)),
            ) {
                (true, false) => Some(HybridCoreKind::Performance),
                (false, true) => Some(HybridCoreKind::Efficiency),
                // Missing or overlapping membership is ambiguous.
                _ => None,
            }
        } else {
            None
        };
        let record = cores.entry((package, core)).or_default();
        record.kind = merge_core_kind(record.kind, explicit_kind.or(pmu_kind));
    }

    if cores.is_empty() {
        return;
    }
    info.physical_cores = Some(cores.len() as u32);

    // P/E labels are published only when the kernel provides core_type or both
    // Intel hybrid PMU masks for every online physical core. SMT shape and CPU
    // numbering are deliberately not treated as architecture metadata.
    if cores.values().all(|record| record.kind.is_some()) {
        info.performance_cores = Some(
            cores
                .values()
                .filter(|record| record.kind == Some(HybridCoreKind::Performance))
                .count() as u32,
        );
        info.efficiency_cores = Some(
            cores
                .values()
                .filter(|record| record.kind == Some(HybridCoreKind::Efficiency))
                .count() as u32,
        );
    }
}

fn refresh_cpu_topology(root: &Path, info: &mut CpuHardwareInfo) {
    info.physical_cores = None;
    info.logical_processors = None;
    info.performance_cores = None;
    info.efficiency_cores = None;
    apply_cpu_topology(root, info);
}

fn cpu_topology_key(root: &Path) -> Option<BTreeSet<u32>> {
    let ids = online_cpu_ids(root);
    (!ids.is_empty()).then_some(ids)
}

fn read_cpu_set(root: &Path, path: &str) -> Option<BTreeSet<u32>> {
    fs::read_to_string(rooted(root, path))
        .ok()
        .and_then(|value| parse_cpu_list(&value))
}

fn merge_core_kind(
    current: Option<HybridCoreKind>,
    incoming: Option<HybridCoreKind>,
) -> Option<HybridCoreKind> {
    match (current, incoming) {
        (Some(current), Some(incoming)) if current != incoming => None,
        (Some(current), _) => Some(current),
        (None, incoming) => incoming,
    }
}

fn online_cpu_ids(root: &Path) -> BTreeSet<u32> {
    if let Ok(value) = fs::read_to_string(rooted(root, "sys/devices/system/cpu/online"))
        && let Some(ids) = parse_cpu_list(&value)
    {
        return ids;
    }

    let mut ids = BTreeSet::new();
    let cpu_root = rooted(root, "sys/devices/system/cpu");
    let Ok(entries) = fs::read_dir(cpu_root) else {
        return ids;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(cpu) = name
            .to_str()
            .and_then(|name| name.strip_prefix("cpu"))
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let online_path = entry.path().join("online");
        if fs::read_to_string(online_path)
            .map(|value| value.trim() != "0")
            .unwrap_or(cpu == 0)
        {
            ids.insert(cpu);
        }
    }
    ids
}

fn parse_cpu_list(value: &str) -> Option<BTreeSet<u32>> {
    const MAX_CPUS: usize = 4_096;
    let value = value.trim();
    if value.is_empty() {
        return Some(BTreeSet::new());
    }
    let mut cpus = BTreeSet::new();
    for part in value.split(',') {
        let part = part.trim();
        let (start, end) = match part.split_once('-') {
            Some((start, end)) => (start.parse::<u32>().ok()?, end.parse::<u32>().ok()?),
            None => {
                let cpu = part.parse::<u32>().ok()?;
                (cpu, cpu)
            }
        };
        if start > end || end.saturating_sub(start) as usize >= MAX_CPUS {
            return None;
        }
        for cpu in start..=end {
            cpus.insert(cpu);
            if cpus.len() > MAX_CPUS {
                return None;
            }
        }
    }
    Some(cpus)
}

fn refresh_cpu_frequencies(root: &Path, info: &mut CpuHardwareInfo) {
    let mut current_khz = None::<u64>;
    let mut maximum_khz = None::<u64>;
    for cpu in online_cpu_ids(root) {
        let cpufreq = rooted(root, &format!("sys/devices/system/cpu/cpu{cpu}/cpufreq"));
        let current = read_frequency_khz(&cpufreq.join("scaling_cur_freq"))
            .or_else(|| read_frequency_khz(&cpufreq.join("cpuinfo_cur_freq")));
        let maximum = read_frequency_khz(&cpufreq.join("cpuinfo_max_freq"))
            .or_else(|| read_frequency_khz(&cpufreq.join("scaling_max_freq")));
        if let Some(value) = current {
            current_khz = Some(current_khz.map_or(value, |highest| highest.max(value)));
        }
        if let Some(value) = maximum {
            maximum_khz = Some(maximum_khz.map_or(value, |highest| highest.max(value)));
        }
    }
    if let Some(value) = current_khz.and_then(khz_to_mhz) {
        info.current_frequency_mhz = Some(value);
    }
    if let Some(value) = maximum_khz.and_then(khz_to_mhz) {
        info.maximum_frequency_mhz = Some(value);
    }
}

fn read_frequency_khz(path: &Path) -> Option<u64> {
    let value = read_number::<u64>(path)?;
    (1_000..=20_000_000).contains(&value).then_some(value)
}

fn khz_to_mhz(value: u64) -> Option<u32> {
    u32::try_from(value.saturating_add(500) / 1_000).ok()
}

fn read_l3_cache_kib(root: &Path) -> Option<u64> {
    let mut instances = BTreeMap::<String, u64>::new();
    for cpu in online_cpu_ids(root) {
        let cache_root = rooted(root, &format!("sys/devices/system/cpu/cpu{cpu}/cache"));
        let Ok(entries) = fs::read_dir(cache_root) else {
            continue;
        };
        for entry in entries.flatten().take(16) {
            if read_number::<u8>(&entry.path().join("level")) != Some(3) {
                continue;
            }
            let Some(size) = read_trimmed(&entry.path().join("size"))
                .and_then(|value| parse_cache_size_kib(&value))
            else {
                continue;
            };
            let key = read_trimmed(&entry.path().join("shared_cpu_list"))
                .unwrap_or_else(|| format!("cpu{cpu}"));
            instances.entry(key).or_insert(size);
        }
    }
    (!instances.is_empty()).then(|| instances.values().copied().sum())
}

fn parse_cache_size_kib(value: &str) -> Option<u64> {
    let value = value.trim();
    let split = value.find(|character: char| !character.is_ascii_digit())?;
    let number = value[..split].parse::<u64>().ok()?;
    match value[split..].trim().to_ascii_uppercase().as_str() {
        "K" | "KB" | "KIB" => Some(number),
        "M" | "MB" | "MIB" => number.checked_mul(1_024),
        _ => None,
    }
}

fn read_memory_hardware(root: &Path, total_mib: u64) -> MemoryHardwareInfo {
    let modules = read_dmi_memory_modules(root)
        .filter(|modules| !modules.is_empty())
        .or_else(|| read_edac_memory_modules(root));
    summarize_memory_modules(Some(total_mib), modules)
}

pub(crate) fn read_privileged_memory_hardware() -> MemoryHardwareInfo {
    let root = Path::new("/");
    let modules = read_dmi_memory_modules(root)
        .filter(|modules| !modules.is_empty())
        .or_else(|| read_edac_memory_modules(root));
    summarize_memory_modules(None, modules)
}

pub(crate) fn encode_memory_hardware(info: &MemoryHardwareInfo) -> String {
    let memory_type = info
        .memory_type
        .as_deref()
        .filter(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
        .unwrap_or("unknown");
    format!(
        "type={memory_type} speed_mt_s={} channels={} modules={}",
        encode_optional_number(info.speed_mt_s),
        encode_optional_number(info.channels),
        encode_optional_number(info.modules),
    )
}

pub(crate) fn parse_memory_hardware(value: &str) -> Result<MemoryHardwareInfo, String> {
    let mut memory_type = None;
    let mut speed_mt_s = None;
    let mut channels = None;
    let mut modules = None;
    let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
    if fields.len() != 4 {
        return Err("invalid hardware memory field count".to_string());
    }
    for field in fields {
        let (name, value) = field
            .split_once('=')
            .ok_or_else(|| "invalid hardware memory field".to_string())?;
        match name {
            "type" => set_protocol_field(
                &mut memory_type,
                (value != "unknown").then(|| value.to_owned()),
            )?,
            "speed_mt_s" => set_protocol_field(&mut speed_mt_s, parse_optional_number(value)?)?,
            "channels" => set_protocol_field(&mut channels, parse_optional_number(value)?)?,
            "modules" => set_protocol_field(&mut modules, parse_optional_number(value)?)?,
            _ => return Err("unknown hardware memory field".to_string()),
        }
    }
    Ok(MemoryHardwareInfo {
        total_mib: None,
        memory_type: memory_type.ok_or_else(|| "missing hardware memory type".to_string())?,
        speed_mt_s: speed_mt_s.ok_or_else(|| "missing hardware memory speed".to_string())?,
        channels: channels.ok_or_else(|| "missing hardware memory channels".to_string())?,
        modules: modules.ok_or_else(|| "missing hardware memory modules".to_string())?,
    })
}

fn encode_optional_number(value: Option<u32>) -> String {
    value.map_or_else(|| "unknown".to_string(), |value| value.to_string())
}

fn parse_optional_number(value: &str) -> Result<Option<u32>, String> {
    if value == "unknown" {
        Ok(None)
    } else {
        value
            .parse::<u32>()
            .map(Some)
            .map_err(|_| "invalid hardware memory number".to_string())
    }
}

fn set_protocol_field<T>(slot: &mut Option<T>, value: T) -> Result<(), String> {
    if slot.replace(value).is_some() {
        Err("duplicate hardware memory field".to_string())
    } else {
        Ok(())
    }
}

fn summarize_memory_modules(
    total_mib: Option<u64>,
    modules: Option<Vec<MemoryModule>>,
) -> MemoryHardwareInfo {
    let Some(modules) = modules else {
        return MemoryHardwareInfo {
            total_mib,
            ..MemoryHardwareInfo::default()
        };
    };

    let memory_types = modules
        .iter()
        .filter_map(|module| module.memory_type.clone())
        .collect::<BTreeSet<_>>();
    let speeds = modules
        .iter()
        .filter_map(|module| module.speed_mt_s)
        .collect::<BTreeSet<_>>();
    let channels = modules
        .iter()
        .filter_map(|module| module.channel.clone())
        .collect::<BTreeSet<_>>();
    MemoryHardwareInfo {
        total_mib,
        speed_mt_s: (speeds.len() == 1 && modules.iter().all(|module| module.speed_mt_s.is_some()))
            .then(|| *speeds.first().expect("one complete speed exists")),
        memory_type: (memory_types.len() == 1
            && modules.iter().all(|module| module.memory_type.is_some()))
        .then(|| {
            memory_types
                .first()
                .expect("one complete type exists")
                .clone()
        }),
        channels: (modules.iter().all(|module| module.channel.is_some()) && !channels.is_empty())
            .then_some(channels.len() as u32),
        modules: Some(modules.len() as u32),
    }
}

fn read_dmi_memory_modules(root: &Path) -> Option<Vec<MemoryModule>> {
    let entries = fs::read_dir(rooted(root, "sys/firmware/dmi/entries")).ok()?;
    let mut saw_readable_entry = false;
    let mut modules = Vec::new();
    for entry in entries.flatten().take(64) {
        if !entry.file_name().to_string_lossy().starts_with("17-") {
            continue;
        }
        let Some(raw) = read_bounded(&entry.path().join("raw"), 4_096) else {
            continue;
        };
        if let Some(module) = parse_dmi_memory_device(&raw) {
            saw_readable_entry = true;
            if module.size_mib > 0 {
                modules.push(module);
            }
        }
    }
    saw_readable_entry.then_some(modules)
}

fn read_bounded(path: &Path, maximum_bytes: u64) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= maximum_bytes).then_some(bytes)
}

fn parse_dmi_memory_device(raw: &[u8]) -> Option<MemoryModule> {
    if raw.len() < 0x15 || raw[0] != 17 {
        return None;
    }
    let formatted_length = usize::from(raw[1]);
    if formatted_length < 0x15 || formatted_length > raw.len() {
        return None;
    }
    let encoded_size = u16::from_le_bytes([raw[0x0c], raw[0x0d]]);
    let size_mib = match encoded_size {
        0 | 0xffff => 0,
        0x7fff if formatted_length >= 0x20 => {
            u64::from(u32::from_le_bytes(raw[0x1c..0x20].try_into().ok()?) & 0x7fff_ffff)
        }
        value if value & 0x8000 != 0 => u64::from(value & 0x7fff) / 1_024,
        value => u64::from(value),
    };
    let memory_type = dmi_memory_type(raw[0x12]).map(str::to_owned);
    let nominal_speed = read_dmi_u16(raw, formatted_length, 0x15);
    let configured_speed = read_dmi_u16(raw, formatted_length, 0x20);
    let speed_mt_s = configured_speed
        .filter(|speed| *speed != 0 && *speed != u16::MAX)
        .or_else(|| nominal_speed.filter(|speed| *speed != 0 && *speed != u16::MAX))
        .map(u32::from);
    let device_locator = dmi_string(raw, formatted_length, raw[0x10]);
    let bank_locator = dmi_string(raw, formatted_length, raw[0x11]);
    let channel = device_locator
        .as_deref()
        .and_then(memory_channel_label)
        .or_else(|| bank_locator.as_deref().and_then(memory_channel_label));
    Some(MemoryModule {
        size_mib,
        memory_type,
        speed_mt_s,
        channel,
    })
}

fn read_dmi_u16(raw: &[u8], formatted_length: usize, offset: usize) -> Option<u16> {
    (offset + 2 <= formatted_length).then(|| u16::from_le_bytes([raw[offset], raw[offset + 1]]))
}

fn dmi_string(raw: &[u8], formatted_length: usize, index: u8) -> Option<String> {
    if index == 0 || formatted_length >= raw.len() {
        return None;
    }
    raw[formatted_length..]
        .split(|byte| *byte == 0)
        .take_while(|value| !value.is_empty())
        .nth(usize::from(index - 1))
        .and_then(|value| std::str::from_utf8(value).ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn dmi_memory_type(value: u8) -> Option<&'static str> {
    match value {
        0x0f => Some("SDRAM"),
        0x12 => Some("DDR"),
        0x13 => Some("DDR2"),
        0x18 => Some("DDR3"),
        0x1a => Some("DDR4"),
        0x1b => Some("LPDDR"),
        0x1c => Some("LPDDR2"),
        0x1d => Some("LPDDR3"),
        0x1e => Some("LPDDR4"),
        0x22 => Some("DDR5"),
        0x23 => Some("LPDDR5"),
        _ => None,
    }
}

fn read_edac_memory_modules(root: &Path) -> Option<Vec<MemoryModule>> {
    let memory_controllers = fs::read_dir(rooted(root, "sys/devices/system/edac/mc")).ok()?;
    let mut modules = Vec::new();
    for controller in memory_controllers.flatten() {
        if !controller.file_name().to_string_lossy().starts_with("mc") {
            continue;
        }
        let Ok(entries) = fs::read_dir(controller.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with("dimm") {
                continue;
            }
            let size_mib = read_number::<u64>(&entry.path().join("size")).unwrap_or_default();
            if size_mib == 0 {
                continue;
            }
            let memory_type = read_trimmed(&entry.path().join("dimm_mem_type"));
            let speed_mt_s = read_number::<u32>(&entry.path().join("dimm_speed"))
                .or_else(|| read_number::<u32>(&entry.path().join("speed")));
            let channel = read_trimmed(&entry.path().join("dimm_location"))
                .and_then(|value| memory_channel_label(&value));
            modules.push(MemoryModule {
                size_mib,
                memory_type,
                speed_mt_s,
                channel,
            });
        }
    }
    (!modules.is_empty()).then_some(modules)
}

fn memory_channel_label(locator: &str) -> Option<String> {
    let normalized = locator.to_ascii_lowercase().replace([' ', '_'], "-");
    let channel = normalized.find("channel")?;
    let controller = normalized[..channel].rfind("controller").unwrap_or(channel);
    let suffix = &normalized[channel + "channel".len()..];
    let channel_id: String = suffix
        .trim_start_matches('-')
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .collect();
    if channel_id.is_empty() {
        return None;
    }
    Some(format!(
        "{}channel-{channel_id}",
        &normalized[controller..channel]
    ))
}

fn read_nvidia_proc_hardware(root: &Path, selected_bus_id: Option<&str>) -> GpuHardwareInfo {
    let mut info = GpuHardwareInfo::default();
    let gpu_root = rooted(root, "proc/driver/nvidia/gpus");
    if let Ok(entries) = fs::read_dir(gpu_root) {
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let Some(contents) = read_trimmed(&entry.path().join("information")) else {
                continue;
            };
            let mut candidate = GpuHardwareInfo::default();
            for line in contents.lines() {
                let Some((name, value)) = line.split_once(':') else {
                    continue;
                };
                match name.trim() {
                    "Model" if !value.trim().is_empty() => {
                        candidate.model = Some(value.trim().to_owned())
                    }
                    "Bus Location" if !value.trim().is_empty() => {
                        candidate.pci_bus_id = Some(normalize_pci_bus_id(value.trim()))
                    }
                    _ => {}
                }
            }
            let selected = selected_bus_id.map(normalize_pci_bus_id);
            if selected
                .as_deref()
                .is_none_or(|selected| candidate.pci_bus_id.as_deref() == Some(selected))
            {
                info = candidate;
                break;
            }
        }
    }
    if let Some(version) = read_trimmed(&rooted(root, "proc/driver/nvidia/version")) {
        info.driver_version = parse_nvidia_driver_version(&version);
    }
    info
}

fn parse_nvidia_driver_version(version: &str) -> Option<String> {
    version
        .lines()
        .find(|line| line.starts_with("NVRM version:"))?
        .split_ascii_whitespace()
        .find_map(|token| {
            let token = token.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '.'
            });
            (token.contains('.')
                && token
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.'))
            .then(|| token.to_owned())
        })
}

fn normalize_pci_bus_id(bus_id: &str) -> String {
    let bus_id = bus_id.trim();
    let Some((domain, rest)) = bus_id.split_once(':') else {
        return bus_id.to_owned();
    };
    if domain.len() > 4
        && domain
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        format!("{}:{rest}", &domain[domain.len() - 4..])
    } else {
        bus_id.to_owned()
    }
}

fn read_number<T: std::str::FromStr>(path: &Path) -> Option<T> {
    read_trimmed(path)?.parse().ok()
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn find_labeled_temperature(
    root: &Path,
    expected_hwmon_name: &str,
    expected_label: &str,
) -> Result<Option<f32>, TelemetryError> {
    let hwmon_root = rooted(root, "sys/class/hwmon");
    let entries = match fs::read_dir(&hwmon_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(TelemetryError::Io {
                operation: "enumerate temperature sensors",
                path: hwmon_root.clone(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| TelemetryError::Io {
            operation: "read temperature sensor entry",
            path: hwmon_root.clone(),
            source,
        })?;
        let directory = entry.path();
        let name = match fs::read_to_string(directory.join("name")) {
            Ok(name) => name,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(TelemetryError::Io {
                    operation: "read temperature sensor name",
                    path: directory.join("name"),
                    source,
                });
            }
        };
        if name.trim() != expected_hwmon_name {
            continue;
        }
        let sensor_entries = fs::read_dir(&directory).map_err(|source| TelemetryError::Io {
            operation: "enumerate hwmon temperature attributes",
            path: directory.clone(),
            source,
        })?;
        for sensor_entry in sensor_entries {
            let sensor_entry = sensor_entry.map_err(|source| TelemetryError::Io {
                operation: "read temperature attribute",
                path: directory.clone(),
                source,
            })?;
            let path = sensor_entry.path();
            let file_name = sensor_entry.file_name();
            let file_name = file_name.to_string_lossy();
            let Some(index) = file_name
                .strip_prefix("temp")
                .and_then(|name| name.strip_suffix("_label"))
            else {
                continue;
            };
            let label = fs::read_to_string(&path).map_err(|source| TelemetryError::Io {
                operation: "read temperature label",
                path: path.clone(),
                source,
            })?;
            if label.trim() != expected_label {
                continue;
            }
            let input_path = directory.join(format!("temp{index}_input"));
            let input = fs::read_to_string(&input_path).map_err(|source| TelemetryError::Io {
                operation: "read labeled temperature",
                path: input_path,
                source,
            })?;
            let millidegrees =
                input
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| TelemetryError::InvalidData {
                        source: "hwmon",
                        detail: format!("invalid temperature {input:?}"),
                    })?;
            return Ok(millidegrees_to_celsius(millidegrees));
        }
    }
    Ok(None)
}

fn millidegrees_to_celsius(value: i64) -> Option<f32> {
    // Reject nonsensical firmware/sensor values rather than presenting them
    // as trustworthy telemetry.  The lower bound still permits sub-zero
    // diagnostics; the upper bound exceeds any survivable operating point.
    (-50_000..=200_000)
        .contains(&value)
        .then_some(value as f32 / 1000.0)
}

fn unavailable_fan_state(rpm_channels: &[FanRpmChannel]) -> FanState {
    let channel = |index| FanChannelState {
        mode: None,
        pwm_raw: 0,
        rpm: rpm_channels
            .iter()
            .find(|channel| channel.index == index)
            .and_then(|channel| channel.rpm)
            .unwrap_or(0),
    };
    FanState {
        cpu: channel(1),
        gpu: channel(2),
    }
}

fn rooted(root: &Path, relative: &str) -> PathBuf {
    root.join(relative.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn parses_cpu_aggregate_and_computes_interval_load() {
        let previous = parse_cpu_times("cpu  100 5 40 800 20 3 4 1 0 0\ncpu0 1 2 3\n").unwrap();
        let current = parse_cpu_times("cpu  160 5 70 870 30 5 8 2 0 0\n").unwrap();
        // total delta = 177, idle+iowait delta = 80, active = 97.
        let load = cpu_utilization(previous, current).unwrap();
        assert!((load - 54.80226).abs() < 0.001, "load={load}");
    }

    #[test]
    fn cpu_load_rejects_counter_regression_and_zero_interval() {
        let times = parse_cpu_times("cpu 1 2 3 4 5 6 7 8\n").unwrap();
        assert_eq!(cpu_utilization(times, times), None);
        let lower = parse_cpu_times("cpu 0 0 0 0 0 0 0 0\n").unwrap();
        assert_eq!(cpu_utilization(times, lower), None);
    }

    #[test]
    fn maps_long_lived_nvml_sample_and_cached_static_info() {
        let live = NvidiaLiveTelemetry {
            temperature_c: Some(57),
            utilization_percent: Some(41),
            power_draw_mw: Some(72_350),
            pstate: Some(crate::nvidia::PerformanceState::P2),
            memory_used_mib: Some(4096),
            memory_total_mib: Some(8188),
            graphics_clock_mhz: Some(2280),
            memory_clock_mhz: Some(8001),
            session_lost: false,
            errors: Vec::new(),
        };
        let static_info = NvidiaStaticInfo {
            model: Some("NVIDIA GeForce RTX 4070 Laptop GPU".to_owned()),
            driver_version: Some("595.71.05".to_owned()),
            pci_bus_id: Some("0000:01:00.0".to_owned()),
            maximum_graphics_clock_mhz: Some(3105),
            maximum_memory_clock_mhz: Some(8001),
            errors: Vec::new(),
        };
        let parsed = nvidia_telemetry(&live, Some(&static_info));
        assert_eq!(parsed.temperature_c, Some(57.0));
        assert_eq!(parsed.utilization_percent, Some(41.0));
        assert_eq!(parsed.power_w, Some(72.35));
        assert_eq!(parsed.pstate.as_deref(), Some("P2"));
        assert_eq!(parsed.memory_used_mib, Some(4096));
        assert_eq!(parsed.memory_total_mib, Some(8188));
        assert_eq!(parsed.graphics_clock_mhz, Some(2280));
        assert_eq!(parsed.memory_clock_mhz, Some(8001));
        assert_eq!(parsed.maximum_graphics_clock_mhz, Some(3105));
        assert_eq!(parsed.maximum_memory_clock_mhz, Some(8001));
        assert_eq!(
            parsed.model.as_deref(),
            Some("NVIDIA GeForce RTX 4070 Laptop GPU")
        );
        assert_eq!(parsed.driver_version.as_deref(), Some("595.71.05"));
        assert_eq!(parsed.pci_bus_id.as_deref(), Some("0000:01:00.0"));
    }

    #[test]
    fn unavailable_nvml_metrics_remain_optional() {
        let parsed = nvidia_telemetry(&NvidiaLiveTelemetry::default(), None);
        assert_eq!(parsed.temperature_c, None);
        assert_eq!(parsed.power_w, None);
        assert_eq!(parsed.graphics_clock_mhz, None);
        assert_eq!(parsed.pstate, None);
    }

    #[test]
    fn unavailable_fan_control_keeps_read_only_rpm_values() {
        let channels = vec![
            FanRpmChannel {
                index: 1,
                label: "CPU".to_string(),
                rpm: Some(2600),
            },
            FanRpmChannel {
                index: 2,
                label: "GPU".to_string(),
                rpm: Some(2400),
            },
        ];
        let state = unavailable_fan_state(&channels);
        assert_eq!(state.cpu.rpm, 2600);
        assert_eq!(state.gpu.rpm, 2400);
        assert_eq!(state.cpu.mode, None);
        assert_eq!(state.gpu.mode, None);
    }

    #[test]
    fn sample_survives_absent_profile_and_acer_hwmon() {
        let root = minimal_acer_telemetry_fixture("optional-acer-interfaces");
        let hardware = AcerHardware::discover_at(&root).unwrap();
        let sample = offline_reader().sample(&hardware).unwrap();

        assert_eq!(sample.profile_raw, None);
        assert_eq!(sample.profile, None);
        assert!(sample.fan_rpm_channels.is_empty());
        assert_eq!(sample.fans, unavailable_fan_state(&[]));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sample_keeps_unknown_profile_and_additional_rpm_channels() {
        let root = minimal_acer_telemetry_fixture("dynamic-acer-interfaces");
        let profile_root = root.join("sys/firmware/acpi");
        let hwmon = root.join("sys/class/hwmon/hwmon7");
        fs::create_dir_all(&profile_root).unwrap();
        fs::create_dir_all(&hwmon).unwrap();
        fs::write(profile_root.join("platform_profile"), "cool\n").unwrap();
        fs::write(
            profile_root.join("platform_profile_choices"),
            "cool balanced performance\n",
        )
        .unwrap();
        fs::write(hwmon.join("name"), "acer\n").unwrap();
        fs::write(hwmon.join("fan1_input"), "2500\n").unwrap();
        fs::write(hwmon.join("fan2_input"), "2300\n").unwrap();
        fs::write(hwmon.join("fan3_input"), "1700\n").unwrap();
        fs::write(hwmon.join("fan3_label"), "System\n").unwrap();

        let hardware = AcerHardware::discover_at(&root).unwrap();
        let sample = offline_reader().sample(&hardware).unwrap();

        assert_eq!(sample.profile_raw.as_deref(), Some("cool"));
        assert_eq!(sample.profile, None);
        assert_eq!(sample.fans.cpu.rpm, 2500);
        assert_eq!(sample.fans.gpu.rpm, 2300);
        assert_eq!(sample.fan_rpm_channels.len(), 3);
        assert_eq!(sample.fan_rpm_channels[2].label, "System");
        assert_eq!(sample.fan_rpm_channels[2].rpm, Some(1700));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_live_battery_and_adapter_state_without_inventing_calibration_progress() {
        let root = fixture_root("power-supply");
        let battery = root.join("sys/class/power_supply/BAT1");
        let ac = root.join("sys/class/power_supply/ACAD");
        let usb = root.join("sys/class/power_supply/USBC000:001");
        fs::create_dir_all(&battery).unwrap();
        fs::create_dir_all(&ac).unwrap();
        fs::create_dir_all(&usb).unwrap();
        fs::write(battery.join("type"), "Battery\n").unwrap();
        fs::write(battery.join("capacity"), "73\n").unwrap();
        fs::write(battery.join("status"), "Discharging\n").unwrap();
        fs::write(ac.join("type"), "Mains\n").unwrap();
        fs::write(ac.join("online"), "1\n").unwrap();
        fs::write(usb.join("type"), "USB\n").unwrap();
        fs::write(usb.join("online"), "0\n").unwrap();

        assert_eq!(
            read_power_supply(&root),
            PowerSupplyTelemetry {
                battery_percent: Some(73),
                battery_status: Some(BatteryStatus::Discharging),
                ac_online: Some(true),
                usb_power_online: Some(false),
            }
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nvidia_retry_backoff_is_bounded_and_resets_after_success() {
        let mut backoff = SampleBackoff::default();
        assert!(backoff.retry_due());

        let mut delays = Vec::new();
        for _ in 0..7 {
            backoff.record_failure();
            let mut samples = 0_u8;
            loop {
                samples += 1;
                if backoff.retry_due() {
                    break;
                }
            }
            delays.push(samples);
        }
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);

        backoff.record_success();
        assert!(backoff.retry_due());
        backoff.record_failure();
        assert!(backoff.retry_due());
    }

    #[test]
    fn sleeping_nvidia_is_healthy_and_clears_live_epoch_state() {
        let root = minimal_acer_telemetry_fixture("nvidia-suspended");
        let pci = root.join("sys/bus/pci/devices/0000:02:00.0");
        fs::create_dir_all(pci.join("power")).unwrap();
        fs::write(pci.join("vendor"), "0x10de\n").unwrap();
        fs::write(pci.join("device"), "0x28a0\n").unwrap();
        fs::write(pci.join("class"), "0x030200\n").unwrap();
        fs::write(pci.join("subsystem_vendor"), "0x1025\n").unwrap();
        fs::write(pci.join("subsystem_device"), "0x0001\n").unwrap();
        fs::write(pci.join("power/runtime_status"), "suspended\n").unwrap();

        let mut reader = offline_reader();
        reader.nvidia_discovery_error = Some("old NVML failure".to_owned());
        reader.nvidia_pci_bus_id = Some("0000:02:00.0".to_owned());
        reader.nvidia_pci_identity = Some(PciIdentity {
            vendor: 0x10de,
            device: 0x28a0,
            subsystem_vendor: 0x1025,
            subsystem_device: 0x0001,
        });
        reader.nvidia_static = Some(NvidiaStaticInfo {
            model: Some("Cached NVIDIA GPU".to_owned()),
            pci_bus_id: Some("0000:02:00.0".to_owned()),
            ..NvidiaStaticInfo::default()
        });
        reader.nvidia_slow.core_offset_mhz = Some(100);
        reader.nvidia_slow.memory_offset_mhz = Some(200);
        reader.refresh_nvidia_lifecycle(&root);

        assert!(reader.nvidia.is_none());
        assert_eq!(
            reader
                .nvidia_static
                .as_ref()
                .and_then(|info| info.model.as_deref()),
            Some("Cached NVIDIA GPU")
        );
        assert!(reader.nvidia_discovery_error.is_none());
        assert!(reader.nvidia_runtime_sleeping);
        assert_eq!(reader.nvidia_pci_bus_id.as_deref(), Some("0000:02:00.0"));
        assert_eq!(reader.nvidia_slow, NvidiaSlowTelemetry::default());
        assert!(reader.nvidia_retry.retry_due());

        reader.hardware_info = Some(HardwareInfo {
            gpu: GpuHardwareInfo {
                current_graphics_clock_mhz: Some(2_250),
                current_memory_clock_mhz: Some(8_001),
                ..GpuHardwareInfo::default()
            },
            ..HardwareInfo::default()
        });
        let info = reader.hardware_snapshot(&root, 16_384, None);
        assert_eq!(info.gpu.current_graphics_clock_mhz, None);
        assert_eq!(info.gpu.current_memory_clock_mhz, None);
        assert_eq!(info.gpu.pci_bus_id.as_deref(), Some("0000:02:00.0"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transitional_nvidia_states_defer_without_error_or_backoff() {
        for (label, status) in [
            ("suspending", "suspending"),
            ("resuming", "resuming"),
            ("unknown", "future-kernel-state"),
        ] {
            let root = minimal_acer_telemetry_fixture(label);
            let pci = root.join("sys/bus/pci/devices/0000:02:00.0");
            fs::create_dir_all(pci.join("power")).unwrap();
            fs::write(pci.join("vendor"), "0x10de\n").unwrap();
            fs::write(pci.join("device"), "0x28a0\n").unwrap();
            fs::write(pci.join("class"), "0x030200\n").unwrap();
            fs::write(pci.join("subsystem_vendor"), "0x1025\n").unwrap();
            fs::write(pci.join("subsystem_device"), "0x0001\n").unwrap();
            fs::write(pci.join("power/runtime_status"), format!("{status}\n")).unwrap();

            let mut reader = offline_reader();
            reader.refresh_nvidia_lifecycle(&root);
            assert!(reader.nvidia.is_none(), "status={status}");
            assert!(reader.nvidia_discovery_error.is_none(), "status={status}");
            assert!(!reader.nvidia_runtime_sleeping, "status={status}");
            assert!(reader.nvidia_retry.retry_due(), "status={status}");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn absent_sample_controller_clears_slow_nvml_state_and_cadence() {
        let mut reader = offline_reader();
        reader.nvidia_slow.core_offset_mhz = Some(100);
        reader.nvidia_slow.memory_offset_mhz = Some(200);
        reader.nvidia_slow_refresh = NVIDIA_SLOW_REFRESH_SAMPLES;
        reader.refresh_nvidia_slow(None);
        assert_eq!(reader.nvidia_slow, NvidiaSlowTelemetry::default());
        assert_eq!(reader.nvidia_slow_refresh, 0);
    }

    #[test]
    fn dynamic_power_limit_and_throttle_reasons_are_not_slow_cached() {
        let source = include_str!("telemetry.rs");
        let slow_refresh = source
            .split("fn refresh_nvidia_slow")
            .nth(1)
            .unwrap()
            .split("pub fn sample")
            .next()
            .unwrap();
        let sample = source.split("pub fn sample").nth(1).unwrap();

        assert!(!slow_refresh.contains("power_telemetry"));
        assert!(sample.contains("controller.power_telemetry()"));
    }

    #[test]
    fn parses_memory_usage_from_available_memory() {
        let (used_mib, total_mib) = parse_memory_usage(
            "MemTotal:       33554432 kB\nMemFree:         1024 kB\nMemAvailable:    12582912 kB\n",
        )
        .unwrap();
        assert_eq!(total_mib, 32_768);
        assert_eq!(used_mib, 20_480);
        assert!(parse_memory_usage("MemTotal: 100 kB\n").is_err());
        assert!(parse_memory_usage("MemTotal: 100 kB\nMemAvailable: 101 kB\n").is_err());
    }

    #[test]
    fn finds_package_temperature_without_hardcoding_hwmon_number() {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("asense-telemetry-{}-{id}", std::process::id()));
        let wrong = root.join("sys/class/hwmon/hwmon9");
        let coretemp = root.join("sys/class/hwmon/hwmon42");
        fs::create_dir_all(&wrong).unwrap();
        fs::create_dir_all(&coretemp).unwrap();
        fs::write(wrong.join("name"), "nvme\n").unwrap();
        fs::write(coretemp.join("name"), "coretemp\n").unwrap();
        fs::write(coretemp.join("temp17_label"), "Package id 0\n").unwrap();
        fs::write(coretemp.join("temp17_input"), "67375\n").unwrap();
        assert_eq!(
            find_labeled_temperature(&root, "coretemp", "Package id 0").unwrap(),
            Some(67.375)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_online_cpu_topology_core_types_and_frequencies() {
        let root = fixture_root("cpu-hardware");
        fs::create_dir_all(root.join("proc")).unwrap();
        fs::create_dir_all(root.join("sys/devices/system/cpu")).unwrap();
        fs::write(
            root.join("proc/cpuinfo"),
            "processor: 0\nmodel name: Intel(R) Core(TM) i9-14900HX\ncpu family: 6\nphysical id: 0\ncore id: 0\ncpu MHz: 3210.5\n\nprocessor: 1\nmodel name: Intel(R) Core(TM) i9-14900HX\ncpu family: 6\nphysical id: 0\ncore id: 0\ncpu MHz: 3100.0\n",
        )
        .unwrap();
        fs::write(root.join("sys/devices/system/cpu/online"), "0-3\n").unwrap();
        for (cpu, core, core_type, current, maximum) in [
            (0, 0, 2, 3_500_000, 5_600_000),
            (1, 0, 2, 3_400_000, 5_600_000),
            (2, 4, 1, 2_000_000, 4_100_000),
            (3, 8, 1, 2_100_000, 4_100_000),
        ] {
            let cpu_root = root.join(format!("sys/devices/system/cpu/cpu{cpu}"));
            fs::create_dir_all(cpu_root.join("topology")).unwrap();
            fs::create_dir_all(cpu_root.join("cpufreq")).unwrap();
            fs::write(cpu_root.join("topology/physical_package_id"), "0\n").unwrap();
            fs::write(cpu_root.join("topology/core_id"), format!("{core}\n")).unwrap();
            fs::write(
                cpu_root.join("topology/core_type"),
                format!("{core_type}\n"),
            )
            .unwrap();
            fs::write(
                cpu_root.join("cpufreq/scaling_cur_freq"),
                format!("{current}\n"),
            )
            .unwrap();
            fs::write(
                cpu_root.join("cpufreq/cpuinfo_max_freq"),
                format!("{maximum}\n"),
            )
            .unwrap();
        }
        let l3 = root.join("sys/devices/system/cpu/cpu0/cache/index3");
        fs::create_dir_all(&l3).unwrap();
        fs::write(l3.join("level"), "3\n").unwrap();
        fs::write(l3.join("size"), "36864K\n").unwrap();
        fs::write(l3.join("shared_cpu_list"), "0-3\n").unwrap();

        let info = read_hardware_info(&root, 32_768);
        assert_eq!(
            info.cpu.model.as_deref(),
            Some("Intel(R) Core(TM) i9-14900HX")
        );
        assert_eq!(info.cpu.physical_cores, Some(3));
        assert_eq!(info.cpu.logical_processors, Some(4));
        assert_eq!(info.cpu.performance_cores, Some(1));
        assert_eq!(info.cpu.efficiency_cores, Some(2));
        assert_eq!(
            info.cpu.architecture.as_deref(),
            Some(std::env::consts::ARCH)
        );
        assert_eq!(info.cpu.family, Some(6));
        assert_eq!(info.cpu.l3_cache_kib, Some(36_864));
        assert_eq!(info.cpu.current_frequency_mhz, Some(3_500));
        assert_eq!(info.cpu.maximum_frequency_mhz, Some(5_600));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cached_hardware_refreshes_topology_only_when_online_mask_changes() {
        let root = fixture_root("cpu-hotplug-cache");
        fs::create_dir_all(root.join("proc")).unwrap();
        fs::create_dir_all(root.join("sys/devices/system/cpu")).unwrap();
        fs::write(
            root.join("proc/cpuinfo"),
            "processor: 0\nmodel name: Cached CPU model\ncpu family: 6\nphysical id: 0\ncore id: 0\n",
        )
        .unwrap();
        fs::write(root.join("sys/devices/system/cpu/online"), "0-3\n").unwrap();
        for (cpu, core, core_type) in [(0, 0, 2), (1, 0, 2), (2, 4, 1), (3, 8, 1)] {
            let topology = root.join(format!("sys/devices/system/cpu/cpu{cpu}/topology"));
            fs::create_dir_all(&topology).unwrap();
            fs::write(topology.join("physical_package_id"), "0\n").unwrap();
            fs::write(topology.join("core_id"), format!("{core}\n")).unwrap();
            fs::write(topology.join("core_type"), format!("{core_type}\n")).unwrap();
        }

        let mut reader = TelemetryReader {
            previous_cpu_times: None,
            nvidia: None,
            nvidia_static: None,
            nvidia_discovery_error: None,
            nvidia_pci_bus_id: None,
            nvidia_pci_identity: None,
            nvidia_exact_oem_target: false,
            nvidia_runtime_sleeping: false,
            nvidia_retry: SampleBackoff::default(),
            hardware_info: None,
            cpu_online_ids: None,
            hardware_frequency_refresh: 0,
            nvidia_slow: NvidiaSlowTelemetry::default(),
            nvidia_slow_refresh: 0,
        };
        let initial = reader.hardware_snapshot(&root, 32_768, None);
        assert_eq!(initial.cpu.logical_processors, Some(4));
        assert_eq!(initial.cpu.physical_cores, Some(3));
        assert_eq!(initial.cpu.performance_cores, Some(1));
        assert_eq!(initial.cpu.efficiency_cores, Some(2));

        // A full inventory rescan would also replace the cached model. Only
        // the online topology is expected to change here.
        fs::write(
            root.join("proc/cpuinfo"),
            "processor: 0\nmodel name: Must not replace cached model\n",
        )
        .unwrap();
        fs::write(root.join("sys/devices/system/cpu/online"), "0-1\n").unwrap();
        let hotplugged = reader.hardware_snapshot(&root, 32_768, None);
        assert_eq!(hotplugged.cpu.logical_processors, Some(2));
        assert_eq!(hotplugged.cpu.physical_cores, Some(1));
        assert_eq!(hotplugged.cpu.performance_cores, Some(1));
        assert_eq!(hotplugged.cpu.efficiency_cores, Some(0));
        assert_eq!(hotplugged.cpu.model.as_deref(), Some("Cached CPU model"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn leaves_hybrid_counts_unknown_without_kernel_core_type() {
        let root = fixture_root("cpu-no-core-type");
        fs::create_dir_all(root.join("sys/devices/system/cpu/cpu0/topology")).unwrap();
        fs::create_dir_all(root.join("sys/devices/system/cpu/cpu1/topology")).unwrap();
        fs::write(root.join("sys/devices/system/cpu/online"), "0-1\n").unwrap();
        for (cpu, core) in [(0, 0), (1, 4)] {
            let topology = root.join(format!("sys/devices/system/cpu/cpu{cpu}/topology"));
            fs::write(topology.join("physical_package_id"), "0\n").unwrap();
            fs::write(topology.join("core_id"), format!("{core}\n")).unwrap();
            fs::write(
                topology.join("thread_siblings_list"),
                if cpu == 0 { "0-1\n" } else { "1\n" },
            )
            .unwrap();
        }
        let mut info = CpuHardwareInfo::default();
        apply_cpu_topology(&root, &mut info);
        assert_eq!(info.physical_cores, Some(2));
        assert_eq!(info.performance_cores, None);
        assert_eq!(info.efficiency_cores, None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uses_complete_kernel_pmu_masks_without_topology_guessing() {
        let root = fixture_root("cpu-pmu-core-types");
        fs::create_dir_all(root.join("sys/devices/system/cpu")).unwrap();
        fs::create_dir_all(root.join("sys/bus/event_source/devices/cpu_core")).unwrap();
        fs::create_dir_all(root.join("sys/bus/event_source/devices/cpu_atom")).unwrap();
        fs::write(root.join("sys/devices/system/cpu/online"), "0-3\n").unwrap();
        fs::write(
            root.join("sys/bus/event_source/devices/cpu_core/cpus"),
            "0-1\n",
        )
        .unwrap();
        fs::write(
            root.join("sys/bus/event_source/devices/cpu_atom/cpus"),
            "2-3\n",
        )
        .unwrap();
        for (cpu, core) in [(0, 0), (1, 0), (2, 4), (3, 8)] {
            let topology = root.join(format!("sys/devices/system/cpu/cpu{cpu}/topology"));
            fs::create_dir_all(&topology).unwrap();
            fs::write(topology.join("physical_package_id"), "0\n").unwrap();
            fs::write(topology.join("core_id"), format!("{core}\n")).unwrap();
        }
        let mut info = CpuHardwareInfo::default();
        apply_cpu_topology(&root, &mut info);
        assert_eq!(info.performance_cores, Some(1));
        assert_eq!(info.efficiency_cores, Some(2));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_real_smbios_memory_devices_and_protocol_round_trip() {
        let root = fixture_root("dmi-memory");
        for (index, locator) in [
            (0, "Controller0-ChannelA-DIMM0"),
            (1, "Controller0-ChannelB-DIMM0"),
        ] {
            let entry = root.join(format!("sys/firmware/dmi/entries/17-{index}"));
            fs::create_dir_all(&entry).unwrap();
            fs::write(entry.join("raw"), dmi_memory_fixture(locator)).unwrap();
        }
        let info = read_memory_hardware(&root, 32_768);
        assert_eq!(info.total_mib, Some(32_768));
        assert_eq!(info.memory_type.as_deref(), Some("DDR5"));
        assert_eq!(info.speed_mt_s, Some(5_600));
        assert_eq!(info.channels, Some(2));
        assert_eq!(info.modules, Some(2));

        let encoded = encode_memory_hardware(&info);
        let parsed = parse_memory_hardware(&encoded).unwrap();
        assert_eq!(parsed.memory_type.as_deref(), Some("DDR5"));
        assert_eq!(parsed.speed_mt_s, Some(5_600));
        assert_eq!(parsed.channels, Some(2));
        assert_eq!(parsed.modules, Some(2));
        assert!(parse_memory_hardware("type=DDR5 modules=2").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_nvidia_proc_version_and_normalizes_pci_domain() {
        assert_eq!(
            parse_nvidia_driver_version(
                "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  595.71.05  Release Build\n",
            )
            .as_deref(),
            Some("595.71.05")
        );
        assert_eq!(normalize_pci_bus_id("00000000:01:00.0"), "0000:01:00.0");
    }

    #[test]
    fn static_nvidia_identity_matches_the_selected_dynamic_bus() {
        let root = fixture_root("nvidia-proc-selected-bus");
        for (entry, model, bus) in [
            ("00000000:01:00.0", "Generic NVIDIA", "00000000:01:00.0"),
            ("00000000:07:00.0", "Selected NVIDIA", "00000000:07:00.0"),
        ] {
            let path = root.join("proc/driver/nvidia/gpus").join(entry);
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join("information"),
                format!("Model: {model}\nBus Location: {bus}\n"),
            )
            .unwrap();
        }
        let info = read_nvidia_proc_hardware(&root, Some("0000:07:00.0"));
        assert_eq!(info.model.as_deref(), Some("Selected NVIDIA"));
        assert_eq!(info.pci_bus_id.as_deref(), Some("0000:07:00.0"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires a live Acer/NVIDIA laptop"]
    fn live_sample_releases_its_nvml_controller_before_returning() {
        let hardware = AcerHardware::discover().unwrap();
        let mut reader = TelemetryReader::new();
        let _ = reader.sample(&hardware).unwrap();
        assert!(reader.nvidia.is_none());
    }

    fn fixture_root(label: &str) -> PathBuf {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "asense-telemetry-{label}-{}-{id}",
            std::process::id()
        ))
    }

    fn minimal_acer_telemetry_fixture(label: &str) -> PathBuf {
        let root = fixture_root(label);
        let dmi = root.join("sys/class/dmi/id");
        let proc = root.join("proc");
        fs::create_dir_all(&dmi).unwrap();
        fs::create_dir_all(&proc).unwrap();
        fs::write(dmi.join("sys_vendor"), "Acer\n").unwrap();
        fs::write(dmi.join("product_name"), "Acer Test Model\n").unwrap();
        fs::write(proc.join("stat"), "cpu  10 0 5 100 0 0 0 0\n").unwrap();
        fs::write(
            proc.join("meminfo"),
            "MemTotal:       16777216 kB\nMemAvailable:    8388608 kB\n",
        )
        .unwrap();
        root
    }

    fn offline_reader() -> TelemetryReader {
        TelemetryReader {
            previous_cpu_times: None,
            nvidia: None,
            nvidia_static: None,
            nvidia_discovery_error: None,
            nvidia_pci_bus_id: None,
            nvidia_pci_identity: None,
            nvidia_exact_oem_target: false,
            nvidia_runtime_sleeping: false,
            nvidia_retry: SampleBackoff {
                samples_until_retry: NVIDIA_RETRY_MAX_SAMPLES,
                next_delay: NVIDIA_RETRY_MAX_SAMPLES,
            },
            hardware_info: None,
            cpu_online_ids: None,
            hardware_frequency_refresh: 0,
            nvidia_slow: NvidiaSlowTelemetry::default(),
            nvidia_slow_refresh: 0,
        }
    }

    fn dmi_memory_fixture(locator: &str) -> Vec<u8> {
        let mut raw = vec![0_u8; 0x22];
        raw[0] = 17;
        raw[1] = 0x22;
        raw[0x0c..0x0e].copy_from_slice(&16_384_u16.to_le_bytes());
        raw[0x10] = 1;
        raw[0x11] = 2;
        raw[0x12] = 0x22;
        raw[0x15..0x17].copy_from_slice(&5_600_u16.to_le_bytes());
        raw[0x20..0x22].copy_from_slice(&5_600_u16.to_le_bytes());
        raw.extend_from_slice(locator.as_bytes());
        raw.extend_from_slice(b"\0BANK 0\0\0");
        raw
    }
}
