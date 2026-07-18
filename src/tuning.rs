//! Acer platform profiles plus an optional, exact-GPU NVIDIA OEM capability.
//!
//! Firmware profile control remains available when the NVIDIA driver does not
//! expose the PHN16-72 offset API. When that exact capability is present, its
//! mutations are still composed transactionally with the firmware profile.

use std::error::Error;
use std::fmt;

use crate::hardware::{AcerHardware, HardwareError, PlatformProfile};
use crate::nvidia::{ClockOffsets, NvidiaController, NvidiaError, OffsetSnapshot, PowerTelemetry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpuOffsetState {
    Unavailable,
    Reset,
    OemTurbo,
    CustomOrPartial,
}

#[derive(Clone, Debug)]
pub struct TuningState {
    pub profile: PlatformProfile,
    pub gpu_offsets: GpuOffsetState,
    pub gpu_pstate_count: usize,
    pub gpu_capability_error: Option<String>,
    pub power: Option<PowerTelemetry>,
    pub power_error: Option<String>,
}

#[derive(Debug)]
pub enum TuningError {
    Hardware(HardwareError),
    Nvidia(NvidiaError),
    RolledBack {
        cause: String,
    },
    RollbackFailed {
        cause: String,
        failures: Vec<String>,
    },
    ExternalOffsets {
        state: GpuOffsetState,
    },
}

impl fmt::Display for TuningError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hardware(error) => write!(formatter, "Acer profile: {error}"),
            Self::Nvidia(error) => write!(formatter, "NVIDIA tuning: {error}"),
            Self::RolledBack { cause } => {
                write!(
                    formatter,
                    "profile transaction failed and was rolled back: {cause}"
                )
            }
            Self::RollbackFailed { cause, failures } => write!(
                formatter,
                "profile transaction failed ({cause}) and rollback was incomplete: {}",
                failures.join("; ")
            ),
            Self::ExternalOffsets { state } => write!(
                formatter,
                "refusing to overwrite custom or partial NVIDIA offsets ({state:?})"
            ),
        }
    }
}

impl Error for TuningError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Hardware(error) => Some(error),
            Self::Nvidia(error) => Some(error),
            _ => None,
        }
    }
}

impl From<HardwareError> for TuningError {
    fn from(value: HardwareError) -> Self {
        Self::Hardware(value)
    }
}

impl From<NvidiaError> for TuningError {
    fn from(value: NvidiaError) -> Self {
        Self::Nvidia(value)
    }
}

pub struct ProfileController {
    nvidia: NvidiaCapability,
}

enum NvidiaCapability {
    Available(NvidiaController),
    Unavailable(String),
}

trait FirmwareProfileControl {
    fn current_profile(&self) -> Result<PlatformProfile, HardwareError>;
    fn set_profile(&self, profile: PlatformProfile) -> Result<(), HardwareError>;
}

impl FirmwareProfileControl for AcerHardware {
    fn current_profile(&self) -> Result<PlatformProfile, HardwareError> {
        AcerHardware::current_profile(self)
    }

    fn set_profile(&self, profile: PlatformProfile) -> Result<(), HardwareError> {
        AcerHardware::set_profile(self, profile)
    }
}

trait NvidiaProfileControl {
    fn supported_state_count(&self) -> usize;
    fn snapshot_offsets(&self) -> Result<OffsetSnapshot, NvidiaError>;
    fn apply_oem_turbo(&self) -> Result<(), NvidiaError>;
    fn reset_oem_offsets(&self) -> Result<(), NvidiaError>;
    fn restore_offsets(&self, snapshot: &OffsetSnapshot) -> Result<(), NvidiaError>;
    fn power_telemetry(&self) -> Result<PowerTelemetry, NvidiaError>;
}

impl NvidiaProfileControl for NvidiaController {
    fn supported_state_count(&self) -> usize {
        self.supported_states().len()
    }

    fn snapshot_offsets(&self) -> Result<OffsetSnapshot, NvidiaError> {
        NvidiaController::snapshot_offsets(self)
    }

    fn apply_oem_turbo(&self) -> Result<(), NvidiaError> {
        NvidiaController::apply_oem_turbo(self).map(|_| ())
    }

    fn reset_oem_offsets(&self) -> Result<(), NvidiaError> {
        NvidiaController::reset_oem_offsets(self).map(|_| ())
    }

    fn restore_offsets(&self, snapshot: &OffsetSnapshot) -> Result<(), NvidiaError> {
        NvidiaController::restore_offsets(self, snapshot).map(|_| ())
    }

    fn power_telemetry(&self) -> Result<PowerTelemetry, NvidiaError> {
        NvidiaController::power_telemetry(self)
    }
}

impl ProfileController {
    pub fn discover() -> Result<Self, TuningError> {
        let nvidia = match NvidiaController::discover() {
            Ok(controller) => NvidiaCapability::Available(controller),
            Err(error) => NvidiaCapability::Unavailable(error.to_string()),
        };
        // NVIDIA VF offsets are an optional capability. Discovery remains
        // fail-closed inside NvidiaController, but lack of that capability
        // must not make unrelated Acer daemon commands unusable.
        Ok(Self { nvidia })
    }

    pub fn state(&self, hardware: &AcerHardware) -> Result<TuningState, TuningError> {
        let profile = hardware.current_profile()?;
        match &self.nvidia {
            NvidiaCapability::Available(nvidia) => {
                let offsets = nvidia.snapshot_offsets()?;
                // Power limits and throttle reasons are informational. A
                // driver omitting them must not invalidate a verified profile
                // and offset transaction.
                let (power, power_error) = optional_power(nvidia);
                Ok(TuningState {
                    profile,
                    gpu_offsets: classify_offsets(&offsets),
                    gpu_pstate_count: nvidia.supported_states().len(),
                    gpu_capability_error: None,
                    power,
                    power_error,
                })
            }
            NvidiaCapability::Unavailable(error) => Ok(TuningState {
                profile,
                gpu_offsets: GpuOffsetState::Unavailable,
                gpu_pstate_count: 0,
                gpu_capability_error: Some(error.clone()),
                power: None,
                power_error: None,
            }),
        }
    }

    /// Applies the Acer firmware profile independently. When the exact NVIDIA
    /// capability is available, Turbo additionally means the reviewed OEM
    /// +100/+200 MHz offsets on every supported P-state; lower profiles reset
    /// them. That optional two-controller path remains atomic and verified.
    pub fn set_profile(
        &self,
        hardware: &AcerHardware,
        target: PlatformProfile,
    ) -> Result<TuningState, TuningError> {
        match &self.nvidia {
            NvidiaCapability::Available(nvidia) => {
                self.set_profile_with_nvidia(hardware, target, nvidia)
            }
            NvidiaCapability::Unavailable(error) => {
                self.set_firmware_profile(hardware, target, error.clone())
            }
        }
    }

    fn set_profile_with_nvidia<H: FirmwareProfileControl, N: NvidiaProfileControl>(
        &self,
        hardware: &H,
        target: PlatformProfile,
        nvidia: &N,
    ) -> Result<TuningState, TuningError> {
        let previous_profile = hardware.current_profile()?;
        let previous_offsets = nvidia.snapshot_offsets()?;
        let previous_offset_state = classify_offsets(&previous_offsets);
        if previous_offset_state == GpuOffsetState::CustomOrPartial {
            return Err(TuningError::ExternalOffsets {
                state: previous_offset_state,
            });
        }

        let mutation = if target == PlatformProfile::Turbo {
            hardware
                .set_profile(target)
                .map_err(|error| format!("set Acer Turbo: {error}"))
                .and_then(|()| {
                    nvidia
                        .apply_oem_turbo()
                        .map_err(|error| format!("apply OEM GPU offsets: {error}"))
                })
        } else {
            nvidia
                .reset_oem_offsets()
                .map_err(|error| format!("reset OEM GPU offsets: {error}"))
                .and_then(|()| {
                    hardware
                        .set_profile(target)
                        .map_err(|error| format!("set Acer profile {target}: {error}"))
                })
        };

        if let Err(cause) = mutation {
            return Err(self.rollback(
                nvidia,
                hardware,
                target,
                previous_profile,
                &previous_offsets,
                cause,
            ));
        }

        let expected_offsets = if target == PlatformProfile::Turbo {
            GpuOffsetState::OemTurbo
        } else {
            GpuOffsetState::Reset
        };
        let readback = required_nvidia_state(hardware, nvidia)
            .map_err(|error| format!("post-mutation readback failed: {error}"))
            .and_then(|(profile, gpu_offsets)| {
                if profile == target && gpu_offsets == expected_offsets {
                    Ok((profile, gpu_offsets))
                } else {
                    Err(format!(
                        "expected profile={target} gpu={expected_offsets:?}, got profile={profile} gpu={gpu_offsets:?}"
                    ))
                }
            });
        let (profile, gpu_offsets) = finish_transaction(readback, |cause| {
            self.rollback(
                nvidia,
                hardware,
                target,
                previous_profile,
                &previous_offsets,
                cause,
            )
        })?;
        let (power, power_error) = optional_power(nvidia);
        Ok(TuningState {
            profile,
            gpu_offsets,
            gpu_pstate_count: nvidia.supported_state_count(),
            gpu_capability_error: None,
            power,
            power_error,
        })
    }

    fn set_firmware_profile<H: FirmwareProfileControl>(
        &self,
        hardware: &H,
        target: PlatformProfile,
        capability_error: String,
    ) -> Result<TuningState, TuningError> {
        let previous_profile = hardware.current_profile()?;
        if let Err(error) = hardware.set_profile(target) {
            return Err(self.rollback_firmware(
                hardware,
                previous_profile,
                format!("set Acer profile {target}: {error}"),
            ));
        }

        let readback = hardware
            .current_profile()
            .map_err(|error| format!("post-mutation Acer readback failed: {error}"))
            .and_then(|profile| {
                if profile == target {
                    Ok(profile)
                } else {
                    Err(format!("expected profile={target}, got profile={profile}"))
                }
            });
        let profile = finish_transaction(readback, |cause| {
            self.rollback_firmware(hardware, previous_profile, cause)
        })?;
        Ok(TuningState {
            profile,
            gpu_offsets: GpuOffsetState::Unavailable,
            gpu_pstate_count: 0,
            gpu_capability_error: Some(capability_error),
            power: None,
            power_error: None,
        })
    }

    fn rollback<H: FirmwareProfileControl, N: NvidiaProfileControl>(
        &self,
        nvidia: &N,
        hardware: &H,
        forward_target: PlatformProfile,
        previous_profile: PlatformProfile,
        previous_offsets: &OffsetSnapshot,
        cause: String,
    ) -> TuningError {
        let mut failures = Vec::new();

        // Reverse the successful forward ordering. Both restores are still
        // attempted so one failed control plane cannot hide the other one.
        if forward_target == PlatformProfile::Turbo {
            if let Err(error) = nvidia.restore_offsets(previous_offsets) {
                failures.push(format!("restore NVIDIA offsets: {error}"));
            }
            if let Err(error) = hardware.set_profile(previous_profile) {
                failures.push(format!("restore Acer profile: {error}"));
            }
        } else {
            if let Err(error) = hardware.set_profile(previous_profile) {
                failures.push(format!("restore Acer profile: {error}"));
            }
            if let Err(error) = nvidia.restore_offsets(previous_offsets) {
                failures.push(format!("restore NVIDIA offsets: {error}"));
            }
        }

        // A successful setter only means that both restore commands were
        // accepted. Verify the combined final state so a firmware or driver
        // that silently ignores a restore can never be reported as rolled
        // back.
        if failures.is_empty() {
            match hardware.current_profile() {
                Ok(profile) if profile == previous_profile => {}
                Ok(profile) => failures.push(format!(
                    "rollback Acer readback mismatch: expected {previous_profile}, got {profile}"
                )),
                Err(error) => failures.push(format!("rollback Acer readback: {error}")),
            }
            match nvidia.snapshot_offsets() {
                Ok(offsets) if offsets == *previous_offsets => {}
                Ok(_) => failures.push("rollback NVIDIA readback mismatch".to_string()),
                Err(error) => failures.push(format!("rollback NVIDIA readback: {error}")),
            }
        }

        if failures.is_empty() {
            TuningError::RolledBack { cause }
        } else {
            TuningError::RollbackFailed { cause, failures }
        }
    }

    fn rollback_firmware<H: FirmwareProfileControl>(
        &self,
        hardware: &H,
        previous_profile: PlatformProfile,
        cause: String,
    ) -> TuningError {
        let mut failures = Vec::new();
        if let Err(error) = hardware.set_profile(previous_profile) {
            failures.push(format!("restore Acer profile: {error}"));
        } else {
            match hardware.current_profile() {
                Ok(profile) if profile == previous_profile => {}
                Ok(profile) => failures.push(format!(
                    "rollback Acer readback mismatch: expected {previous_profile}, got {profile}"
                )),
                Err(error) => failures.push(format!("rollback Acer readback: {error}")),
            }
        }
        if failures.is_empty() {
            TuningError::RolledBack { cause }
        } else {
            TuningError::RollbackFailed { cause, failures }
        }
    }
}

fn required_nvidia_state(
    hardware: &impl FirmwareProfileControl,
    nvidia: &impl NvidiaProfileControl,
) -> Result<(PlatformProfile, GpuOffsetState), TuningError> {
    let profile = hardware.current_profile()?;
    let offsets = nvidia.snapshot_offsets()?;
    Ok((profile, classify_offsets(&offsets)))
}

fn optional_power(nvidia: &impl NvidiaProfileControl) -> (Option<PowerTelemetry>, Option<String>) {
    match nvidia.power_telemetry() {
        Ok(power) => (Some(power), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn finish_transaction<T>(
    readback: Result<T, String>,
    rollback: impl FnOnce(String) -> TuningError,
) -> Result<T, TuningError> {
    readback.map_err(rollback)
}

pub fn classify_offsets(snapshot: &OffsetSnapshot) -> GpuOffsetState {
    if snapshot.states.iter().all(|state| {
        state.core.current_mhz == ClockOffsets::RESET.core_mhz
            && state.memory.current_mhz == ClockOffsets::RESET.memory_mhz
    }) {
        GpuOffsetState::Reset
    } else if snapshot.states.iter().all(|state| {
        state.core.current_mhz == ClockOffsets::OEM_TURBO.core_mhz
            && state.memory.current_mhz == ClockOffsets::OEM_TURBO.memory_mhz
    }) {
        GpuOffsetState::OemTurbo
    } else {
        GpuOffsetState::CustomOrPartial
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::{
        FirmwareProfileControl, GpuOffsetState, NvidiaCapability, NvidiaProfileControl,
        ProfileController, TuningError, classify_offsets,
    };
    use crate::hardware::{HardwareError, PlatformProfile};
    use crate::nvidia::{
        ClockOffsets, NvidiaError, OffsetReading, OffsetSnapshot, PerformanceState, PowerTelemetry,
        StateOffsets,
    };

    struct FakeHardware {
        profile: Cell<PlatformProfile>,
        read_count: Cell<usize>,
        fail_read_at: Cell<Option<usize>>,
        set_count: Cell<usize>,
        ignore_set_at: Cell<Option<usize>>,
    }

    impl FakeHardware {
        fn new(profile: PlatformProfile) -> Self {
            Self {
                profile: Cell::new(profile),
                read_count: Cell::new(0),
                fail_read_at: Cell::new(None),
                set_count: Cell::new(0),
                ignore_set_at: Cell::new(None),
            }
        }
    }

    impl FirmwareProfileControl for FakeHardware {
        fn current_profile(&self) -> Result<PlatformProfile, HardwareError> {
            let call = self.read_count.get();
            self.read_count.set(call + 1);
            if self.fail_read_at.get() == Some(call) {
                self.fail_read_at.set(None);
                return Err(HardwareError::InvalidValue {
                    field: "injected profile readback",
                    value: "failure".to_owned(),
                });
            }
            Ok(self.profile.get())
        }

        fn set_profile(&self, profile: PlatformProfile) -> Result<(), HardwareError> {
            let call = self.set_count.get();
            self.set_count.set(call + 1);
            if self.ignore_set_at.get() == Some(call) {
                self.ignore_set_at.set(None);
                return Ok(());
            }
            self.profile.set(profile);
            Ok(())
        }
    }

    struct FakeNvidia {
        offsets: RefCell<OffsetSnapshot>,
        snapshot_count: Cell<usize>,
        fail_snapshot_at: Cell<Option<usize>>,
        restore_count: Cell<usize>,
        fail_power: Cell<bool>,
    }

    impl FakeNvidia {
        fn new() -> Self {
            Self {
                offsets: RefCell::new(snapshot([0, 0], [0, 0])),
                snapshot_count: Cell::new(0),
                fail_snapshot_at: Cell::new(None),
                restore_count: Cell::new(0),
                fail_power: Cell::new(false),
            }
        }

        fn set_uniform(&self, offsets: ClockOffsets) {
            for state in &mut self.offsets.borrow_mut().states {
                state.core.current_mhz = offsets.core_mhz;
                state.memory.current_mhz = offsets.memory_mhz;
            }
        }
    }

    impl NvidiaProfileControl for FakeNvidia {
        fn supported_state_count(&self) -> usize {
            self.offsets.borrow().states.len()
        }

        fn snapshot_offsets(&self) -> Result<OffsetSnapshot, NvidiaError> {
            let call = self.snapshot_count.get();
            self.snapshot_count.set(call + 1);
            if self.fail_snapshot_at.get() == Some(call) {
                self.fail_snapshot_at.set(None);
                return Err(NvidiaError::InjectedFailure("profile offset readback"));
            }
            Ok(self.offsets.borrow().clone())
        }

        fn apply_oem_turbo(&self) -> Result<(), NvidiaError> {
            self.set_uniform(ClockOffsets::OEM_TURBO);
            Ok(())
        }

        fn reset_oem_offsets(&self) -> Result<(), NvidiaError> {
            self.set_uniform(ClockOffsets::RESET);
            Ok(())
        }

        fn restore_offsets(&self, snapshot: &OffsetSnapshot) -> Result<(), NvidiaError> {
            self.restore_count.set(self.restore_count.get() + 1);
            *self.offsets.borrow_mut() = snapshot.clone();
            Ok(())
        }

        fn power_telemetry(&self) -> Result<PowerTelemetry, NvidiaError> {
            if self.fail_power.get() {
                Err(NvidiaError::InjectedFailure("power telemetry"))
            } else {
                Ok(PowerTelemetry {
                    draw_mw: Some(72_000),
                    enforced_limit_mw: 115_000,
                    default_limit_mw: 115_000,
                    minimum_limit_mw: 20_000,
                    maximum_limit_mw: 140_000,
                    clock_event_reasons: crate::nvidia::ClockEventReasons::from_bits(0),
                })
            }
        }
    }

    fn snapshot(core: [i32; 2], memory: [i32; 2]) -> OffsetSnapshot {
        OffsetSnapshot {
            states: [PerformanceState::P0, PerformanceState::P3]
                .into_iter()
                .enumerate()
                .map(|(index, state)| StateOffsets {
                    state,
                    core: OffsetReading {
                        current_mhz: core[index],
                        minimum_mhz: -1_000,
                        maximum_mhz: 1_000,
                    },
                    memory: OffsetReading {
                        current_mhz: memory[index],
                        minimum_mhz: -2_000,
                        maximum_mhz: 6_000,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn partial_offsets_are_never_classified_as_oem_turbo() {
        assert_eq!(
            classify_offsets(&snapshot([100, 100], [200, 0])),
            GpuOffsetState::CustomOrPartial
        );
        assert_eq!(
            classify_offsets(&snapshot([100, 100], [200, 200])),
            GpuOffsetState::OemTurbo
        );
        assert_eq!(
            classify_offsets(&snapshot([0, 0], [0, 0])),
            GpuOffsetState::Reset
        );
    }

    #[test]
    fn missing_nvidia_capability_keeps_acer_profile_available_with_reason() {
        let controller = ProfileController {
            nvidia: NvidiaCapability::Unavailable("missing exact NVML VF API".to_owned()),
        };
        let hardware = FakeHardware::new(PlatformProfile::Balanced);

        let state = controller
            .set_firmware_profile(
                &hardware,
                PlatformProfile::Turbo,
                "missing exact NVML VF API".to_owned(),
            )
            .unwrap();

        assert_eq!(hardware.profile.get(), PlatformProfile::Turbo);
        assert_eq!(state.profile, PlatformProfile::Turbo);
        assert_eq!(state.gpu_offsets, GpuOffsetState::Unavailable);
        assert_eq!(state.power, None);
        assert_eq!(
            state.gpu_capability_error.as_deref(),
            Some("missing exact NVML VF API")
        );
    }

    #[test]
    fn acer_readback_failure_after_mutation_restores_both_planes() {
        let controller = ProfileController {
            nvidia: NvidiaCapability::Unavailable("test-only".to_owned()),
        };
        let hardware = FakeHardware::new(PlatformProfile::Balanced);
        hardware.fail_read_at.set(Some(1));
        let nvidia = FakeNvidia::new();

        let error = controller
            .set_profile_with_nvidia(&hardware, PlatformProfile::Turbo, &nvidia)
            .unwrap_err();

        assert!(matches!(error, TuningError::RolledBack { .. }));
        assert_eq!(hardware.profile.get(), PlatformProfile::Balanced);
        assert_eq!(
            classify_offsets(&nvidia.offsets.borrow()),
            GpuOffsetState::Reset
        );
        assert_eq!(nvidia.restore_count.get(), 1);
    }

    #[test]
    fn nvidia_readback_failure_after_mutation_restores_both_planes() {
        let controller = ProfileController {
            nvidia: NvidiaCapability::Unavailable("test-only".to_owned()),
        };
        let hardware = FakeHardware::new(PlatformProfile::Balanced);
        let nvidia = FakeNvidia::new();
        nvidia.fail_snapshot_at.set(Some(1));

        let error = controller
            .set_profile_with_nvidia(&hardware, PlatformProfile::Turbo, &nvidia)
            .unwrap_err();

        assert!(matches!(error, TuningError::RolledBack { .. }));
        assert_eq!(hardware.profile.get(), PlatformProfile::Balanced);
        assert_eq!(
            classify_offsets(&nvidia.offsets.borrow()),
            GpuOffsetState::Reset
        );
        assert_eq!(nvidia.restore_count.get(), 1);
    }

    #[test]
    fn silently_ignored_rollback_is_never_reported_as_verified() {
        let controller = ProfileController {
            nvidia: NvidiaCapability::Unavailable("test-only".to_owned()),
        };
        let hardware = FakeHardware::new(PlatformProfile::Balanced);
        let nvidia = FakeNvidia::new();
        hardware.fail_read_at.set(Some(1));
        hardware.ignore_set_at.set(Some(1));

        let error = controller
            .set_profile_with_nvidia(&hardware, PlatformProfile::Turbo, &nvidia)
            .unwrap_err();

        assert!(matches!(error, TuningError::RollbackFailed { .. }));
        assert_eq!(hardware.profile.get(), PlatformProfile::Turbo);
        assert_eq!(nvidia.restore_count.get(), 1);
    }

    #[test]
    fn power_telemetry_failure_is_explicit_but_not_transactional() {
        let controller = ProfileController {
            nvidia: NvidiaCapability::Unavailable("test-only".to_owned()),
        };
        let hardware = FakeHardware::new(PlatformProfile::Balanced);
        let nvidia = FakeNvidia::new();
        nvidia.fail_power.set(true);

        let state = controller
            .set_profile_with_nvidia(&hardware, PlatformProfile::Turbo, &nvidia)
            .unwrap();

        assert_eq!(state.profile, PlatformProfile::Turbo);
        assert_eq!(state.gpu_offsets, GpuOffsetState::OemTurbo);
        assert_eq!(state.power, None);
        assert!(
            state
                .power_error
                .as_deref()
                .is_some_and(|error| { error.contains("power telemetry") })
        );
        assert_eq!(nvidia.restore_count.get(), 0);
    }
}
