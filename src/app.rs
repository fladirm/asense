use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, TryLockError};
use std::task::{Context, Poll};
use std::time::Duration;

use dioxus::prelude::*;
use dioxus_desktop::tao::dpi::{LogicalSize, PhysicalSize};
use dioxus_desktop::tao::event::{ElementState, Event as TaoEvent, MouseButton, WindowEvent};
use dioxus_desktop::tao::window::ResizeDirection;
use dioxus_desktop::{Config, WindowBuilder, use_window, use_wry_event_handler};
use futures_util::future::poll_fn;
use futures_util::task::AtomicWaker;

use crate::control::{
    CapabilityLightingBackend, CapabilityLightingTarget, CapabilityProfileBackend,
    ControlCapabilities, ControlClient, ControlError, ControlLightingDevice, ControlLightingMode,
    ControlLightingModes, ControlProfileChoice, ControlResult, ProfileApplyReceipt,
};
use crate::hardware::{
    AcerHardware, FanMode as HardwareFanMode, PlatformProfile as HardwareProfile,
};
use crate::nvidia::ClockEventReasons;
use crate::platform::{
    PlatformState, READ_ERROR_BATTERY_CALIBRATION, READ_ERROR_BATTERY_LIMIT, READ_ERROR_BOOT_SOUND,
    READ_ERROR_KEYBOARD_TIMEOUT, READ_ERROR_LCD_OVERRIDE, READ_ERROR_REAR_LOGO,
    READ_ERROR_USB_CHARGING, RearLogoState, UsbCharging,
};
use crate::telemetry::{
    BatteryStatus, HardwareInfo, MemoryHardwareInfo, SystemTelemetry, TelemetryReader,
};
use crate::tuning::GpuOffsetState;

mod docs_modal;
mod i18n;

use i18n::{LocaleId as Language, MessageId, text};

const APP_CSS: &str = include_str!("../assets/style.css");
#[allow(dead_code)]
const APP_CSS_SOURCE: &str = APP_CSS;

// The dashboard is a fixed logical composition. One composited root transform
// scales it uniformly while the native titlebar remains exactly 48 px high;
// no card is ever reflowed into another row or column.
const COMPACT_DESIGN_WIDTH: f64 = 620.0;
const ADVANCED_DESIGN_WIDTH: f64 = 1_200.0;
const WORKSPACE_DESIGN_HEIGHT: f64 = 650.0;
const TITLEBAR_DESIGN_HEIGHT: f64 = 48.0;
const INITIAL_WINDOW_HEIGHT: f64 = 830.0;
const MIN_WINDOW_HEIGHT: f64 = 690.0;
const MAX_WINDOW_HEIGHT: f64 = 1_100.0;
const TELEMETRY_HISTORY_CAPACITY: usize = 120;
const CONTROL_COMMAND_QUEUE_CAPACITY: usize = 1;
const MAX_LIGHTING_ZONES: u8 = 16;
// After an NVML refresh the telemetry reader reuses that snapshot for ten
// following samples, so the next real read occurs on the eleventh. One extra
// sample also covers an old value already queued when the command completes.
const PROFILE_SYNC_GRACE_SAMPLES: u8 = 12;
const PROFILE_MISMATCH_DEBOUNCE_SAMPLES: u8 = 2;
const TELEMETRY_RETRY_MAX_SECONDS: u64 = 8;
const RESIZE_CORRECTION_TIMEOUT: Duration = Duration::from_millis(350);
const RESIZE_SCRIPT: &str = r#"
(() => {
    const viewport = document.querySelector('.window-workspace');
    const stage = document.querySelector('.design-stage');
    if (!viewport || !stage) return;

    window.__asenseResizeObserver?.disconnect();
    window.__asenseModeObserver?.disconnect();

    let pending = false;
    const fit = () => {
        pending = false;
        const designWidth = viewport.classList.contains('advanced') ? 1200 : 620;
        const designHeight = 650;
        const width = viewport.clientWidth;
        const height = viewport.clientHeight;
        const scale = Math.min(width / designWidth, height / designHeight);
        const renderedWidth = designWidth * scale;
        const renderedHeight = designHeight * scale;

        stage.style.setProperty('--ui-scale', String(scale));
        stage.style.setProperty('--offset-x', `${(width - renderedWidth) / 2}px`);
        stage.style.setProperty('--offset-y', `${(height - renderedHeight) / 2}px`);
    };
    const schedule = () => {
        if (pending) return;
        pending = true;
        requestAnimationFrame(fit);
    };

    const resizeObserver = new ResizeObserver(schedule);
    resizeObserver.observe(viewport);
    const modeObserver = new MutationObserver(schedule);
    modeObserver.observe(viewport, { attributes: true, attributeFilter: ['class'] });
    window.__asenseResizeObserver = resizeObserver;
    window.__asenseModeObserver = modeObserver;
    schedule();
})();
"#;

const RAW_DETAIL_MAX_CHARS: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RawDetail(String);

impl RawDetail {
    fn new(value: impl AsRef<str>) -> Self {
        let value = value.as_ref();
        let mut bounded = value.chars().take(RAW_DETAIL_MAX_CHARS).collect::<String>();
        if value.chars().count() > RAW_DETAIL_MAX_CHARS {
            bounded.push('…');
        }
        Self(bounded)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum UiDiagnostic {
    Lighting(RawDetail),
    Platform(PlatformIssue),
    Hardware(RawDetail),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlatformReadErrorSet(u8);

impl PlatformReadErrorSet {
    fn from_mask(mask: u8) -> Option<Self> {
        (mask != 0).then_some(Self(mask))
    }

    fn contains(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PlatformIssue {
    Readback(PlatformReadErrorSet),
    Raw(RawDetail),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiErrorKind {
    Initialization,
    Fan,
    Profile,
    Lighting,
    Platform,
    Refresh,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum UiStatus {
    #[default]
    AcerControlsConnected,
    AcerNvidiaControlsConnected,
    ReadOnlyTelemetryConnected,
    ConnectingControls,
    PlatformRefreshed,
    SettingsConfirmed,
    LightingConfirmed,
    AppliedWithoutReadback,
    WritingAndVerifying,
    ProfileVerified(ProfileApplyReceipt),
    GpuProfileMismatch {
        core_mhz: i32,
        memory_mhz: i32,
    },
    PartialCapabilities(Vec<UiDiagnostic>),
    PlatformReadbackFailed(PlatformReadErrorSet),
    Failure {
        kind: UiErrorKind,
        detail: RawDetail,
    },
}

fn error_kind_message(kind: UiErrorKind) -> MessageId {
    match kind {
        UiErrorKind::Initialization => MessageId::StatusInitializationFailure,
        UiErrorKind::Fan => MessageId::StatusFanFailure,
        UiErrorKind::Profile => MessageId::StatusProfileFailure,
        UiErrorKind::Lighting => MessageId::StatusLightingFailure,
        UiErrorKind::Platform => MessageId::StatusPlatformFailure,
        UiErrorKind::Refresh => MessageId::StatusRefreshFailure,
    }
}

fn render_platform_fields(language: Language, fields: PlatformReadErrorSet) -> String {
    let mut names = Vec::new();
    for (bit, id) in [
        (
            READ_ERROR_BATTERY_LIMIT,
            MessageId::PlatformFieldBatteryLimit,
        ),
        (
            READ_ERROR_BATTERY_CALIBRATION,
            MessageId::PlatformFieldBatteryCalibration,
        ),
        (READ_ERROR_USB_CHARGING, MessageId::PlatformFieldUsbCharging),
        (
            READ_ERROR_KEYBOARD_TIMEOUT,
            MessageId::PlatformFieldKeyboardTimeout,
        ),
        (READ_ERROR_BOOT_SOUND, MessageId::PlatformFieldBootSound),
        (READ_ERROR_LCD_OVERRIDE, MessageId::PlatformFieldLcdOverride),
        (READ_ERROR_REAR_LOGO, MessageId::PlatformFieldRearLogo),
    ] {
        if fields.contains(bit) {
            names.push(text(language, id));
        }
    }
    names.join(", ")
}

fn render_platform_issue(language: Language, issue: &PlatformIssue) -> String {
    match issue {
        PlatformIssue::Readback(fields) => format!(
            "{}: {}",
            text(language, MessageId::StatusPlatformReadbackFailed),
            render_platform_fields(language, *fields)
        ),
        PlatformIssue::Raw(detail) => format!(
            "{}: {}",
            text(language, MessageId::StatusPlatformFailure),
            detail.as_str()
        ),
    }
}

fn render_diagnostic(language: Language, diagnostic: &UiDiagnostic) -> String {
    match diagnostic {
        UiDiagnostic::Lighting(detail) => format!(
            "{}: {}",
            text(language, MessageId::DiagnosticRgb),
            detail.as_str()
        ),
        UiDiagnostic::Platform(PlatformIssue::Readback(fields)) => format!(
            "{}: {}",
            text(language, MessageId::DiagnosticPlatform),
            render_platform_fields(language, *fields)
        ),
        UiDiagnostic::Platform(PlatformIssue::Raw(detail)) => format!(
            "{}: {}",
            text(language, MessageId::DiagnosticPlatform),
            detail.as_str()
        ),
        UiDiagnostic::Hardware(detail) => format!(
            "{}: {}",
            text(language, MessageId::DiagnosticHardware),
            detail.as_str()
        ),
    }
}

fn render_profile_receipt(language: Language, receipt: &ProfileApplyReceipt) -> String {
    let offsets = match receipt.gpu_offsets {
        GpuOffsetState::Unavailable => {
            text(language, MessageId::StatusOffsetUnavailable).to_string()
        }
        GpuOffsetState::Reset => "+0/+0 MHz".to_string(),
        GpuOffsetState::OemTurbo => "+100/+200 MHz".to_string(),
        GpuOffsetState::CustomOrPartial => {
            text(language, MessageId::StatusOffsetCustomOrPartial).to_string()
        }
    };
    let power = receipt.power.as_ref().map_or_else(
        || text(language, MessageId::StatusGpuLimitUnavailable).to_string(),
        |power| {
            format!(
                "GPU {}/{} W",
                format_milliwatts(power.enforced_limit_mw),
                format_milliwatts(power.maximum_limit_mw)
            )
        },
    );
    format!(
        "{}: Acer {} · VF {offsets} · {power}",
        text(language, MessageId::StatusProfileVerified),
        receipt.firmware_profile
    )
}

fn render_ui_status(language: Language, status: &UiStatus) -> String {
    let static_id = match status {
        UiStatus::AcerControlsConnected => Some(MessageId::StatusAcerControlsConnected),
        UiStatus::AcerNvidiaControlsConnected => Some(MessageId::StatusAcerNvidiaControlsConnected),
        UiStatus::ReadOnlyTelemetryConnected => Some(MessageId::StatusReadOnlyTelemetryConnected),
        UiStatus::ConnectingControls => Some(MessageId::StatusConnectingControls),
        UiStatus::PlatformRefreshed => Some(MessageId::StatusPlatformRefreshed),
        UiStatus::SettingsConfirmed => Some(MessageId::StatusSettingsConfirmed),
        UiStatus::LightingConfirmed => Some(MessageId::StatusLightingConfirmed),
        UiStatus::AppliedWithoutReadback => Some(MessageId::StatusAppliedWithoutReadback),
        UiStatus::WritingAndVerifying => Some(MessageId::StatusWritingAndVerifying),
        _ => None,
    };
    if let Some(id) = static_id {
        return text(language, id).to_string();
    }
    match status {
        UiStatus::ProfileVerified(receipt) => render_profile_receipt(language, receipt),
        UiStatus::GpuProfileMismatch {
            core_mhz,
            memory_mhz,
        } => format!(
            "{}: core {core_mhz:+} / VRAM {memory_mhz:+} MHz",
            text(language, MessageId::StatusGpuMismatch)
        ),
        UiStatus::PartialCapabilities(diagnostics) => {
            let details = diagnostics
                .iter()
                .map(|diagnostic| render_diagnostic(language, diagnostic))
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "{}: {details}",
                text(language, MessageId::StatusPartialCapabilities)
            )
        }
        UiStatus::PlatformReadbackFailed(fields) => format!(
            "{}: {}",
            text(language, MessageId::StatusPlatformReadbackFailed),
            render_platform_fields(language, *fields)
        ),
        UiStatus::Failure { kind, detail } => format!(
            "{}: {}",
            text(language, error_kind_message(*kind)),
            detail.as_str()
        ),
        _ => unreachable!("static status was handled above"),
    }
}

fn render_compact_status(language: Language, status: &UiStatus) -> String {
    let id = match status {
        UiStatus::SettingsConfirmed => MessageId::StatusCompactSettingsVerified,
        UiStatus::LightingConfirmed => MessageId::StatusCompactLightingVerified,
        UiStatus::AppliedWithoutReadback => MessageId::StatusCompactLastApplied,
        UiStatus::WritingAndVerifying => MessageId::StatusCompactVerifying,
        UiStatus::PlatformRefreshed => MessageId::StatusCompactPlatformRefreshed,
        UiStatus::PartialCapabilities(_) | UiStatus::PlatformReadbackFailed(_) => {
            MessageId::AppCompactStatus001
        }
        UiStatus::Failure { kind, .. } => error_kind_message(*kind),
        UiStatus::ProfileVerified(receipt) => match receipt.firmware_profile.as_str() {
            "low-power" => MessageId::StatusCompactProfileEco,
            "quiet" => MessageId::StatusCompactProfileQuiet,
            "balanced" => MessageId::StatusCompactProfileBalanced,
            "balanced-performance" => MessageId::StatusCompactProfilePerformance,
            "performance" => MessageId::StatusCompactProfileTurbo,
            _ => MessageId::StatusCompactProfileGeneric,
        },
        UiStatus::GpuProfileMismatch { .. } => MessageId::AppCompactStatus002,
        _ => return render_ui_status(language, status),
    };
    text(language, id).to_string()
}

fn design_width(advanced: bool) -> f64 {
    if advanced {
        ADVANCED_DESIGN_WIDTH
    } else {
        COMPACT_DESIGN_WIDTH
    }
}

fn workspace_aspect_ratio(advanced: bool) -> f64 {
    design_width(advanced) / WORKSPACE_DESIGN_HEIGHT
}

fn logical_window_size(advanced: bool, height: f64) -> LogicalSize<f64> {
    let height = height.clamp(MIN_WINDOW_HEIGHT, MAX_WINDOW_HEIGHT);
    let workspace_height = (height - TITLEBAR_DESIGN_HEIGHT).max(1.0);
    LogicalSize::new(workspace_height * workspace_aspect_ratio(advanced), height)
}

fn physical_size_close(left: PhysicalSize<u32>, right: PhysicalSize<u32>) -> bool {
    left.width.abs_diff(right.width) <= 2 && left.height.abs_diff(right.height) <= 2
}

fn aspect_constrained_size(
    requested: PhysicalSize<u32>,
    accepted: PhysicalSize<u32>,
    advanced: bool,
    scale_factor: f64,
    direction: Option<ResizeDirection>,
) -> PhysicalSize<u32> {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let ratio = workspace_aspect_ratio(advanced);
    let titlebar_height = TITLEBAR_DESIGN_HEIGHT * scale_factor;
    let minimum_height = MIN_WINDOW_HEIGHT * scale_factor;
    let maximum_height = MAX_WINDOW_HEIGHT * scale_factor;
    let requested_width = f64::from(requested.width.max(1));
    let requested_height = f64::from(requested.height.max(1));

    // Infer the dragged axis from the delta against the last accepted size.
    // Horizontal handles drive width, vertical handles drive height and corner
    // handles naturally select whichever normalized delta is larger.
    let width_driven = match direction {
        Some(ResizeDirection::East | ResizeDirection::West) => true,
        Some(ResizeDirection::North | ResizeDirection::South) => false,
        _ => {
            let width_delta = f64::from(requested.width.abs_diff(accepted.width));
            let height_delta_as_width =
                f64::from(requested.height.abs_diff(accepted.height)) * ratio;
            width_delta >= height_delta_as_width
        }
    };
    let height = if width_driven {
        requested_width / ratio + titlebar_height
    } else {
        requested_height
    }
    .clamp(minimum_height, maximum_height);
    let workspace_height = (height - titlebar_height).max(1.0);

    PhysicalSize::new(
        (workspace_height * ratio).round().max(1.0) as u32,
        height.round().max(1.0) as u32,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingResizeCorrection {
    target: PhysicalSize<u32>,
    generation: u64,
    ignore_intermediate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeObservation {
    Ignore,
    NoSchedule,
    ScheduleCorrection,
}

#[derive(Debug)]
struct AspectResizeState {
    advanced: bool,
    accepted: PhysicalSize<u32>,
    pending_correction: Option<PendingResizeCorrection>,
    correction_generation: u64,
    latest_request: Option<PhysicalSize<u32>>,
    correction_scheduled: bool,
    finalize_after_pending: bool,
    direction: Option<ResizeDirection>,
}

impl AspectResizeState {
    fn new(accepted: PhysicalSize<u32>) -> Self {
        Self {
            advanced: false,
            accepted,
            pending_correction: None,
            correction_generation: 0,
            latest_request: None,
            correction_scheduled: false,
            finalize_after_pending: false,
            direction: None,
        }
    }

    fn observe_resize(&mut self, requested: PhysicalSize<u32>) -> ResizeObservation {
        if let Some(pending) = self.pending_correction {
            if physical_size_close(pending.target, requested) {
                self.accepted = requested;
                self.pending_correction = None;
                self.latest_request = None;
                self.finalize_after_pending = false;
                return ResizeObservation::NoSchedule;
            }

            // Updating compact/advanced constraints can emit an intermediate
            // GTK resize before the requested mode target. Keep waiting for
            // that one target; its bounded timeout handles a missing ACK.
            if pending.ignore_intermediate {
                return ResizeObservation::Ignore;
            }

            // A mismatched WM acknowledgement is authoritative. Accept the
            // actual native size and do not replay the same correction.
            self.accepted = requested;
            self.pending_correction = None;
            self.latest_request = None;
            if self.finalize_after_pending {
                self.finalize_after_pending = false;
                self.latest_request = Some(requested);
                if !self.correction_scheduled {
                    self.correction_scheduled = true;
                    return ResizeObservation::ScheduleCorrection;
                }
            }
            return ResizeObservation::NoSchedule;
        }

        self.latest_request = Some(requested);
        if self.correction_scheduled {
            ResizeObservation::NoSchedule
        } else {
            self.correction_scheduled = true;
            ResizeObservation::ScheduleCorrection
        }
    }

    fn begin_pending_correction(
        &mut self,
        target: PhysicalSize<u32>,
        ignore_intermediate: bool,
    ) -> u64 {
        self.correction_generation = self.correction_generation.wrapping_add(1);
        let generation = self.correction_generation;
        self.pending_correction = Some(PendingResizeCorrection {
            target,
            generation,
            ignore_intermediate,
        });
        self.finalize_after_pending = false;
        generation
    }

    fn expire_pending_correction(
        &mut self,
        generation: u64,
        actual: PhysicalSize<u32>,
    ) -> ResizeObservation {
        if self
            .pending_correction
            .is_none_or(|pending| pending.generation != generation)
        {
            return ResizeObservation::Ignore;
        }
        self.pending_correction = None;
        self.latest_request = None;
        self.accepted = actual;
        if self.finalize_after_pending {
            self.finalize_after_pending = false;
            self.latest_request = Some(actual);
            if !self.correction_scheduled {
                self.correction_scheduled = true;
                return ResizeObservation::ScheduleCorrection;
            }
        }
        ResizeObservation::NoSchedule
    }

    /// End a native drag exactly once. An in-flight correction is already the
    /// final snap; otherwise the queued correction consumes the last real size.
    fn finish_drag(&mut self, actual: PhysicalSize<u32>) -> bool {
        if self.direction.take().is_none() {
            return false;
        }
        if self.pending_correction.is_some() {
            self.latest_request = None;
            self.finalize_after_pending = true;
            return false;
        }
        self.finalize_after_pending = false;
        self.latest_request = Some(actual);
        if self.correction_scheduled {
            false
        } else {
            self.correction_scheduled = true;
            true
        }
    }
}

fn schedule_pending_correction_timeout(
    window: &dioxus_desktop::DesktopContext,
    state: &Rc<RefCell<AspectResizeState>>,
    generation: u64,
) {
    let window = window.clone();
    let state = state.clone();
    glib::timeout_add_local_once(RESIZE_CORRECTION_TIMEOUT, move || {
        let actual = window.inner_size();
        let observation = state
            .borrow_mut()
            .expire_pending_correction(generation, actual);
        if observation == ResizeObservation::ScheduleCorrection {
            schedule_aspect_correction(&window, &state);
        }
    });
}

fn schedule_aspect_correction(
    window: &dioxus_desktop::DesktopContext,
    state: &Rc<RefCell<AspectResizeState>>,
) {
    let window = window.clone();
    let state = state.clone();
    glib::idle_add_local_once(move || {
        let (requested, accepted, advanced, direction) = {
            let mut resize = state.borrow_mut();
            resize.correction_scheduled = false;
            let Some(requested) = resize.latest_request.take() else {
                return;
            };
            (
                requested,
                resize.accepted,
                resize.advanced,
                resize.direction,
            )
        };
        let target = aspect_constrained_size(
            requested,
            accepted,
            advanced,
            window.scale_factor(),
            direction,
        );
        let mut resize = state.borrow_mut();
        if physical_size_close(target, requested) {
            resize.accepted = target;
            resize.pending_correction = None;
            resize.finalize_after_pending = false;
            return;
        }
        let generation = resize.begin_pending_correction(target, false);
        drop(resize);
        schedule_pending_correction_timeout(&window, &state, generation);
        window.set_inner_size(target);
    });
}

fn queue_aspect_resize(
    window: &dioxus_desktop::DesktopContext,
    state: &Rc<RefCell<AspectResizeState>>,
    requested: PhysicalSize<u32>,
) {
    if state.borrow_mut().observe_resize(requested) != ResizeObservation::ScheduleCorrection {
        return;
    }
    schedule_aspect_correction(window, state);
}

fn finish_aspect_resize(
    window: &dioxus_desktop::DesktopContext,
    state: &Rc<RefCell<AspectResizeState>>,
) {
    if state.borrow_mut().finish_drag(window.inner_size()) {
        schedule_aspect_correction(window, state);
    }
}

fn set_window_mode(
    window: &dioxus_desktop::DesktopContext,
    state: &Rc<RefCell<AspectResizeState>>,
    advanced: bool,
) {
    let scale_factor = window.scale_factor().max(f64::EPSILON);
    let current = window.inner_size();
    let logical_height =
        (f64::from(current.height) / scale_factor).clamp(MIN_WINDOW_HEIGHT, MAX_WINDOW_HEIGHT);
    let logical_target = logical_window_size(advanced, logical_height);
    let physical_target = logical_target.to_physical::<u32>(scale_factor);
    let generation = {
        let mut resize = state.borrow_mut();
        resize.advanced = advanced;
        resize.latest_request = None;
        resize.finalize_after_pending = false;
        resize.direction = None;
        resize.begin_pending_correction(physical_target, true)
    };
    window.set_min_inner_size(Some(logical_window_size(advanced, MIN_WINDOW_HEIGHT)));
    window.set_max_inner_size(Some(logical_window_size(advanced, MAX_WINDOW_HEIGHT)));
    schedule_pending_correction_timeout(window, state, generation);
    window.set_inner_size(logical_target);
}

pub fn launch() {
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            Config::new()
                .with_background_color((8, 9, 16, 255))
                .with_window(
                    WindowBuilder::new()
                        .with_title("ASense")
                        .with_decorations(false)
                        .with_transparent(false)
                        .with_inner_size(logical_window_size(false, INITIAL_WINDOW_HEIGHT))
                        .with_min_inner_size(logical_window_size(false, MIN_WINDOW_HEIGHT))
                        .with_max_inner_size(logical_window_size(false, MAX_WINDOW_HEIGHT))
                        .with_resizable(true)
                        .with_maximizable(false),
                )
                .with_menu(None),
        )
        .launch(Root);
}

#[derive(Clone)]
struct RuntimeState {
    view: AppState,
}

impl RuntimeState {
    fn boot() -> Self {
        let view = AppState {
            platform_busy: true,
            control_busy: true,
            health: HealthState::Applying,
            status: UiStatus::ConnectingControls,
            controls_enabled: false,
            telemetry_health: TelemetryHealth::Connecting,
            ..AppState::default()
        };
        Self { view }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TelemetryHealth {
    Connecting,
    #[default]
    Online,
    Reconnecting {
        retry_after_seconds: u64,
    },
}

fn render_telemetry_status(
    language: Language,
    health: TelemetryHealth,
) -> Option<(String, String)> {
    match health {
        TelemetryHealth::Online => None,
        TelemetryHealth::Connecting => {
            let status = text(language, MessageId::StatusTelemetryConnecting).to_string();
            Some((status.clone(), status))
        }
        TelemetryHealth::Reconnecting {
            retry_after_seconds,
        } => Some((
            format!(
                "{} · {} {retry_after_seconds} s",
                text(language, MessageId::StatusTelemetryReconnecting),
                text(language, MessageId::StatusRetryIn)
            ),
            text(language, MessageId::StatusTelemetryReconnecting).to_string(),
        )),
    }
}

enum TelemetryUpdate {
    Sample {
        sample: Box<SystemTelemetry>,
        refresh_capabilities: bool,
    },
    Error {
        message: String,
        retry_after: Duration,
    },
}

fn telemetry_retry_delay(consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(3);
    Duration::from_secs((1_u64 << exponent).min(TELEMETRY_RETRY_MAX_SECONDS))
}

struct TelemetrySlotInner {
    latest: Mutex<Option<TelemetryUpdate>>,
    waker: AtomicWaker,
}

#[derive(Clone)]
struct TelemetrySlot {
    inner: Arc<TelemetrySlotInner>,
}

impl Default for TelemetrySlot {
    fn default() -> Self {
        Self {
            inner: Arc::new(TelemetrySlotInner {
                latest: Mutex::new(None),
                waker: AtomicWaker::new(),
            }),
        }
    }
}

impl TelemetrySlot {
    /// Telemetry is state, not an event stream. If the UI stalls, replace the
    /// pending sample instead of accumulating an unbounded history queue.
    fn publish_latest(&self, update: TelemetryUpdate) {
        let mut latest = match self.inner.latest.lock() {
            Ok(latest) => latest,
            Err(poisoned) => poisoned.into_inner(),
        };
        *latest = Some(update);
        drop(latest);
        self.inner.waker.wake();
    }

    fn try_take(&self) -> Option<TelemetryUpdate> {
        match self.inner.latest.try_lock() {
            Ok(mut latest) => latest.take(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().take(),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    async fn receive(&self) -> TelemetryUpdate {
        poll_fn(|context: &mut Context<'_>| {
            if let Some(update) = self.try_take() {
                return Poll::Ready(update);
            }
            self.inner.waker.register(context.waker());
            match self.try_take() {
                Some(update) => Poll::Ready(update),
                None => Poll::Pending,
            }
        })
        .await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlatformAction {
    Refresh,
    BatteryLimit(bool),
    BatteryCalibration(bool),
    UsbCharging(UsbCharging),
    KeyboardTimeout(bool),
    BootSound(bool),
    LcdOverride(bool),
    RearLogo(RearLogoState),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ControlAction {
    Initialize,
    FanMode(FanMode),
    ManualFans(ManualFanRequest),
    Profile(String),
    LightingApply(LightingApplyRequest),
    LightingPower(LightingPowerRequest),
    Platform(PlatformAction),
    Refresh,
}

impl ControlAction {
    fn touches_platform(&self) -> bool {
        matches!(self, Self::Initialize | Self::Platform(_) | Self::Refresh)
    }

    fn error_kind(&self) -> UiErrorKind {
        match self {
            Self::Initialize => UiErrorKind::Initialization,
            Self::FanMode(_) | Self::ManualFans(_) => UiErrorKind::Fan,
            Self::Profile(_) => UiErrorKind::Profile,
            Self::LightingApply(_) | Self::LightingPower(_) => UiErrorKind::Lighting,
            Self::Platform(_) => UiErrorKind::Platform,
            Self::Refresh => UiErrorKind::Refresh,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ControlRequest {
    action: ControlAction,
    foreground: bool,
}

impl ControlRequest {
    fn foreground(action: ControlAction) -> Self {
        Self {
            action,
            foreground: true,
        }
    }

    fn background(action: ControlAction) -> Self {
        Self {
            action,
            foreground: false,
        }
    }
}

#[derive(Debug)]
enum ControlOutcome {
    RefreshedThen {
        refresh: Box<ControlOutcome>,
        result: Result<Box<ControlOutcome>, String>,
    },
    Initialize {
        capabilities: ControlCapabilities,
        lighting: Result<KeyboardLightingState, String>,
        memory_hardware: Result<MemoryHardwareInfo, String>,
        platform: Result<PlatformState, String>,
    },
    FanMode(FanMode),
    ManualFans(ManualFanRequest),
    Profile {
        profile_raw: String,
        receipt: ProfileApplyReceipt,
    },
    LightingApplied {
        request: LightingApplyRequest,
        firmware_state: Option<KeyboardLightingState>,
    },
    LightingPowered(KeyboardLightingState),
    Platform {
        action: PlatformAction,
        state: PlatformState,
    },
    Refresh {
        capabilities: ControlCapabilities,
        lighting: Result<KeyboardLightingState, String>,
        platform: Result<PlatformState, String>,
    },
}

#[derive(Debug)]
struct ControlUpdate {
    request: ControlRequest,
    result: Result<ControlOutcome, String>,
}

struct ControlResultSlotInner {
    pending: Mutex<VecDeque<ControlUpdate>>,
    waker: AtomicWaker,
}

#[derive(Clone)]
struct ControlResultSlot {
    inner: Arc<ControlResultSlotInner>,
}

impl Default for ControlResultSlot {
    fn default() -> Self {
        Self {
            inner: Arc::new(ControlResultSlotInner {
                pending: Mutex::new(VecDeque::new()),
                waker: AtomicWaker::new(),
            }),
        }
    }
}

impl ControlResultSlot {
    /// Control completions are events and must never be coalesced or dropped.
    /// Request submission is single-flight and the command channel is bounded,
    /// so this queue remains naturally bounded while still surviving a delayed
    /// UI consumer without terminating the worker.
    fn publish(&self, update: ControlUpdate) {
        let mut pending = match self.inner.pending.lock() {
            Ok(pending) => pending,
            Err(poisoned) => poisoned.into_inner(),
        };
        pending.push_back(update);
        drop(pending);
        self.inner.waker.wake();
    }

    fn try_take(&self) -> Option<ControlUpdate> {
        match self.inner.pending.try_lock() {
            Ok(mut pending) => pending.pop_front(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().pop_front(),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    async fn receive(&self) -> ControlUpdate {
        poll_fn(|context: &mut Context<'_>| {
            if let Some(update) = self.try_take() {
                return Poll::Ready(update);
            }
            self.inner.waker.register(context.waker());
            match self.try_take() {
                Some(update) => Poll::Ready(update),
                None => Poll::Pending,
            }
        })
        .await
    }
}

#[derive(Clone)]
struct ControlWorker {
    commands: Result<SyncSender<ControlRequest>, Arc<str>>,
}

impl ControlWorker {
    fn start(results: ControlResultSlot) -> Self {
        let (commands, receiver) = sync_channel::<ControlRequest>(CONTROL_COMMAND_QUEUE_CAPACITY);
        let spawn = std::thread::Builder::new()
            .name("asense-control".to_string())
            .spawn(move || {
                let mut control = None;
                while let Ok(request) = receiver.recv() {
                    let result = execute_control_action(&mut control, request.action.clone());
                    results.publish(ControlUpdate { request, result });
                }
            });
        match spawn {
            Ok(_) => Self {
                commands: Ok(commands),
            },
            Err(error) => Self {
                commands: Err(Arc::from(format!("cannot start control worker: {error}"))),
            },
        }
    }

    fn submit(&self, request: ControlRequest) -> Result<(), String> {
        let commands = self.commands.as_ref().map_err(|error| error.to_string())?;
        commands.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => "control worker queue is full".to_string(),
            TrySendError::Disconnected(_) => "control worker is unavailable".to_string(),
        })
    }
}

#[component]
fn Root() -> Element {
    let desktop = use_window();
    let initial_size = desktop.inner_size();
    let aspect_state =
        use_hook(move || Rc::new(RefCell::new(AspectResizeState::new(initial_size))));
    let resize_window = desktop.clone();
    let resize_state = aspect_state.clone();
    let telemetry_resume = use_hook(|| Arc::new(AtomicBool::new(false)));
    let resume_signal = telemetry_resume.clone();
    let _aspect_handler = use_wry_event_handler(move |event, _target| match event {
        TaoEvent::WindowEvent {
            window_id,
            event: WindowEvent::Resized(size),
            ..
        } if *window_id == resize_window.id() => {
            queue_aspect_resize(&resize_window, &resize_state, *size);
        }
        TaoEvent::WindowEvent {
            window_id,
            event:
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                },
            ..
        } if *window_id == resize_window.id() => {
            finish_aspect_resize(&resize_window, &resize_state);
        }
        TaoEvent::WindowEvent {
            window_id,
            event: WindowEvent::Focused(false),
            ..
        } if *window_id == resize_window.id() => {
            finish_aspect_resize(&resize_window, &resize_state);
        }
        TaoEvent::Resumed => resume_signal.store(true, Ordering::Release),
        _ => {}
    });
    let mut runtime = use_signal(RuntimeState::boot);
    let mut language = use_signal(i18n::load_locale_preference);
    let mut advanced_open = use_signal(|| false);
    use_effect(move || {
        let _ = document::eval(RESIZE_SCRIPT);
    });
    let control_results = use_hook(ControlResultSlot::default);
    let result_receiver = control_results.clone();
    let _control_updates = use_future(move || {
        let result_receiver = result_receiver.clone();
        async move {
            loop {
                let update = result_receiver.receive().await;
                apply_control_update(&mut runtime.write().view, update);
            }
        }
    });
    let worker_results = control_results.clone();
    let control_worker = use_hook(move || ControlWorker::start(worker_results));

    let telemetry_slot = use_hook(TelemetrySlot::default);
    let telemetry_receiver = telemetry_slot.clone();
    let resume_control_worker = control_worker.clone();
    let pending_capability_refresh = use_hook(|| Arc::new(AtomicBool::new(false)));
    let pending_refresh = pending_capability_refresh.clone();
    let _telemetry_updates = use_future(move || {
        let telemetry_receiver = telemetry_receiver.clone();
        let resume_control_worker = resume_control_worker.clone();
        let pending_refresh = pending_refresh.clone();
        async move {
            loop {
                let update = telemetry_receiver.receive().await;
                match update {
                    TelemetryUpdate::Sample {
                        sample,
                        refresh_capabilities,
                    } => {
                        let mut state = runtime.write();
                        apply_telemetry(&mut state.view, *sample);
                        state.view.telemetry_health = TelemetryHealth::Online;
                        state.view.telemetry_error = None;
                        drop(state);
                        if refresh_capabilities {
                            pending_refresh.store(true, Ordering::Release);
                        }
                        if pending_refresh.load(Ordering::Acquire)
                            && queue_control_request(
                                runtime,
                                &resume_control_worker,
                                ControlRequest::background(ControlAction::Refresh),
                            )
                        {
                            pending_refresh.store(false, Ordering::Release);
                        }
                    }
                    TelemetryUpdate::Error {
                        message,
                        retry_after,
                    } => {
                        let mut state = runtime.write();
                        state.view.telemetry_health = TelemetryHealth::Reconnecting {
                            retry_after_seconds: retry_after.as_secs(),
                        };
                        state.view.telemetry_error = Some(RawDetail::new(message));
                    }
                }
            }
        }
    });
    let initial_worker = control_worker.clone();
    let initial_results = control_results.clone();
    use_hook(move || {
        let request = ControlRequest::background(ControlAction::Initialize);
        if let Err(error) = initial_worker.submit(request.clone()) {
            initial_results.publish(ControlUpdate {
                request,
                result: Err(error),
            });
        }
    });

    use_hook(move || {
        let telemetry_slot = telemetry_slot.clone();
        let telemetry_resume = telemetry_resume.clone();
        std::thread::spawn(move || {
            let mut hardware = None;
            let mut reader = TelemetryReader::new();
            let mut consecutive_failures = 0_u32;
            let mut refresh_capabilities = false;
            loop {
                if telemetry_resume.swap(false, Ordering::AcqRel) {
                    hardware = None;
                    reader.invalidate_nvidia_session();
                    consecutive_failures = 0;
                    refresh_capabilities = true;
                }
                if hardware.is_none() {
                    match AcerHardware::discover() {
                        Ok(discovered) => hardware = Some(discovered),
                        Err(error) => {
                            consecutive_failures = consecutive_failures.saturating_add(1);
                            let retry_after = telemetry_retry_delay(consecutive_failures);
                            telemetry_slot.publish_latest(TelemetryUpdate::Error {
                                message: error.to_string(),
                                retry_after,
                            });
                            std::thread::sleep(retry_after);
                            continue;
                        }
                    }
                }

                let Some(active_hardware) = hardware.as_ref() else {
                    continue;
                };
                match reader.sample(active_hardware) {
                    Ok(sample) => {
                        consecutive_failures = 0;
                        telemetry_slot.publish_latest(TelemetryUpdate::Sample {
                            sample: Box::new(sample),
                            refresh_capabilities,
                        });
                        refresh_capabilities = false;
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    Err(error) => {
                        // A module reload can renumber the Acer hwmon path. Drop
                        // the stale handle and rediscover it on the next bounded
                        // retry instead of requiring the GUI to be restarted.
                        hardware = None;
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        let retry_after = telemetry_retry_delay(consecutive_failures);
                        telemetry_slot.publish_latest(TelemetryUpdate::Error {
                            message: error.to_string(),
                            retry_after,
                        });
                        std::thread::sleep(retry_after);
                    }
                }
            }
        });
    });

    let state = runtime.read().view.clone();
    let fan_mode_worker = control_worker.clone();
    let manual_fans_worker = control_worker.clone();
    let profile_worker = control_worker.clone();
    let lighting_worker = control_worker.clone();
    let lighting_power_worker = control_worker.clone();
    let platform_worker = control_worker.clone();
    let refresh_worker = control_worker.clone();
    let mode_window = desktop.clone();
    let mode_aspect_state = aspect_state.clone();
    let handle_window = desktop.clone();
    let handle_aspect_state = aspect_state.clone();
    rsx! {
        document::Title { "ASense" }
        style { "{APP_CSS}" }
        div { class: "app-window",
                    WindowChrome { language: language() }
            div {
                class: if advanced_open() { "window-workspace advanced" } else { "window-workspace" },
                div { class: "design-stage",
                    Dashboard {
                        state,
                        language: language(),
                        advanced_open: advanced_open(),
                        on_fan_mode: move |mode| set_fan_mode(runtime, &fan_mode_worker, mode),
                        on_manual_fans: move |request| set_manual_fans(runtime, &manual_fans_worker, request),
                        on_profile: move |profile| set_platform_profile(runtime, &profile_worker, profile),
                        on_lighting: move |request| apply_lighting(runtime, &lighting_worker, request),
                        on_lighting_power: move |request| set_lighting_power(
                            runtime,
                            &lighting_power_worker,
                            request,
                        ),
                        on_platform: move |action| {
                            queue_control_request(
                                runtime,
                                &platform_worker,
                                ControlRequest::foreground(ControlAction::Platform(action)),
                            );
                        },
                        on_language: move |_| {
                            let next = language().toggle();
                            language.set(next);
                            let _ = i18n::save_locale_preference(next);
                        },
                        on_refresh: move |_| {
                            queue_control_request(
                                runtime,
                                &refresh_worker,
                                ControlRequest::foreground(ControlAction::Refresh),
                            );
                        },
                        on_advanced: move |open| {
                            advanced_open.set(open);
                            set_window_mode(&mode_window, &mode_aspect_state, open);
                        },
                    }
                }
            }
            ResizeHandles {
                on_resize_start: move |direction| {
                    let mut resize = handle_aspect_state.borrow_mut();
                    resize.accepted = handle_window.inner_size();
                    resize.direction = Some(direction);
                }
            }
        }
    }
}

#[component]
fn WindowChrome(language: Language) -> Element {
    let window = use_window();
    let drag_window = window.clone();
    let minimize_window = window.clone();
    let close_window = window;

    rsx! {
        header {
            class: "window-chrome",
            onmousedown: move |_| drag_window.drag(),
            div { class: "window-title-group",
                strong { "ASense" }
            }
            div { class: "window-controls",
                button {
                    class: "window-button minimize",
                    r#type: "button",
                    title: text(language, MessageId::WindowMinimize),
                    onmousedown: move |event| event.stop_propagation(),
                    onclick: move |_| minimize_window.set_minimized(true),
                    span { class: "minimize-mark" }
                }
                button {
                    class: "window-button close",
                    r#type: "button",
                    title: text(language, MessageId::WindowClose),
                    onmousedown: move |event| event.stop_propagation(),
                    onclick: move |_| close_window.close(),
                    span { class: "close-mark", "×" }
                }
            }
        }
    }
}

#[component]
fn ResizeHandle(
    direction: ResizeDirection,
    class_name: &'static str,
    on_resize_start: EventHandler<ResizeDirection>,
) -> Element {
    let window = use_window();
    rsx! {
        div {
            class: "resize-handle {class_name}",
            onmousedown: move |event| {
                event.prevent_default();
                event.stop_propagation();
                on_resize_start.call(direction);
                let _ = window.drag_resize_window(direction);
            }
        }
    }
}

#[component]
fn ResizeHandles(on_resize_start: EventHandler<ResizeDirection>) -> Element {
    rsx! {
        ResizeHandle { direction: ResizeDirection::North, class_name: "north", on_resize_start }
        ResizeHandle { direction: ResizeDirection::South, class_name: "south", on_resize_start }
        ResizeHandle { direction: ResizeDirection::East, class_name: "east", on_resize_start }
        ResizeHandle { direction: ResizeDirection::West, class_name: "west", on_resize_start }
        ResizeHandle { direction: ResizeDirection::NorthEast, class_name: "north-east", on_resize_start }
        ResizeHandle { direction: ResizeDirection::NorthWest, class_name: "north-west", on_resize_start }
        ResizeHandle { direction: ResizeDirection::SouthEast, class_name: "south-east", on_resize_start }
        ResizeHandle { direction: ResizeDirection::SouthWest, class_name: "south-west", on_resize_start }
    }
}

fn with_control<T>(
    control: &mut Option<ControlClient>,
    operation: impl FnOnce(&mut ControlClient) -> ControlResult<T>,
) -> Result<T, String> {
    if control.is_none() {
        *control = Some(ControlClient::connect().map_err(String::from)?);
    }
    let result = operation(control.as_mut().expect("control was initialized"));
    if result
        .as_ref()
        .is_err_and(|error| error.invalidates_session())
    {
        *control = None;
    }
    result.map_err(String::from)
}

fn partial_control_result<T>(result: ControlResult<T>) -> ControlResult<Result<T, String>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(error) if error.invalidates_session() => Err(error),
        Err(error) => Ok(Err(error.to_string())),
    }
}

fn parse_lighting_response(response: String) -> ControlResult<KeyboardLightingState> {
    parse_lighting_state(&response)
        .map_err(|error| ControlError::Protocol(format!("invalid RGB response: {error}")))
}

fn keyboard_lighting_snapshot(
    client: &mut ControlClient,
    capabilities: &ControlCapabilities,
) -> ControlResult<KeyboardLightingState> {
    if capabilities.lighting.iter().any(|device| {
        device.target == CapabilityLightingTarget::Keyboard
            && device.backend == CapabilityLightingBackend::ZonedWmi
            && device.state_readable
    }) {
        return client.keyboard_state().and_then(parse_lighting_response);
    }
    if capabilities
        .lighting
        .iter()
        .any(|device| device.target == CapabilityLightingTarget::Keyboard)
    {
        return Ok(KeyboardLightingState {
            available: true,
            ..KeyboardLightingState::default()
        });
    }
    Ok(KeyboardLightingState::default())
}

fn empty_platform_state() -> PlatformState {
    PlatformState {
        battery_limit: None,
        battery_calibration: None,
        usb_charging: None,
        keyboard_timeout: None,
        boot_sound: None,
        lcd_override: None,
        rear_logo: None,
        read_error_mask: 0,
    }
}

fn platform_snapshot(
    client: &mut ControlClient,
    capabilities: &ControlCapabilities,
) -> ControlResult<PlatformState> {
    let platform = capabilities.platform;
    if platform.battery_limit
        || platform.battery_calibration
        || platform.usb_off_charging
        || platform.keyboard_timeout
        || platform.boot_sound
        || platform.lcd_override
        || platform.rear_logo
    {
        client.platform_state()
    } else {
        Ok(empty_platform_state())
    }
}

fn execute_control_action(
    control: &mut Option<ControlClient>,
    action: ControlAction,
) -> Result<ControlOutcome, String> {
    if matches!(&action, ControlAction::Initialize | ControlAction::Refresh) {
        return execute_control_action_inner(control, action);
    }
    let refresh = if control.is_none() {
        Some(reconnect_control(control)?)
    } else {
        None
    };
    let result = execute_control_action_inner(control, action);
    Ok(match refresh {
        Some(refresh) => ControlOutcome::RefreshedThen {
            refresh: Box::new(refresh),
            result: result.map(Box::new),
        },
        None => return result,
    })
}

fn execute_control_action_inner(
    control: &mut Option<ControlClient>,
    action: ControlAction,
) -> Result<ControlOutcome, String> {
    match action {
        ControlAction::Initialize => match initialize_control(control) {
            Ok(first) => Ok(first),
            Err(_) => {
                std::thread::sleep(Duration::from_millis(500));
                initialize_control(control)
            }
        },
        ControlAction::FanMode(mode) => with_control(control, |client| match mode {
            FanMode::Auto => client.fan_auto(),
            FanMode::Manual => Err(ControlError::InvalidRequest(
                "manual fan mode requires explicit fan speeds".to_string(),
            )),
            FanMode::Maximum => client.fan_maximum(),
        })
        .map(|()| ControlOutcome::FanMode(mode)),
        ControlAction::ManualFans(request) => with_control(control, |client| {
            client.fan_manual(request.cpu_percent, request.gpu_percent)
        })
        .map(|()| ControlOutcome::ManualFans(request)),
        ControlAction::Profile(profile_raw) => with_control(control, |client| {
            let receipt = client.set_profile(&profile_raw)?;
            if receipt.firmware_profile != profile_raw {
                return Err(ControlError::Protocol(format!(
                    "control receipt mismatch: requested {}, firmware confirmed {}",
                    profile_raw, receipt.firmware_profile
                )));
            }
            Ok(ControlOutcome::Profile {
                profile_raw,
                receipt,
            })
        }),
        ControlAction::LightingApply(request) => with_control(control, |client| {
            let response = client.lighting_apply(
                &request.device_id,
                request.mode,
                request.brightness,
                request.speed,
                request.color,
                &request.zone_colors,
            )?;
            let firmware_state = if request.state_readable {
                Some(parse_lighting_response(response)?)
            } else {
                None
            };
            Ok(ControlOutcome::LightingApplied {
                request,
                firmware_state,
            })
        }),
        ControlAction::LightingPower(request) => with_control(control, |client| {
            client
                .lighting_power(&request.device_id, request.enabled)
                .and_then(parse_lighting_response)
                .map(ControlOutcome::LightingPowered)
        }),
        ControlAction::Platform(action) => with_control(control, |client| match action {
            PlatformAction::Refresh => client.platform_state(),
            PlatformAction::BatteryLimit(enabled) => client.set_battery_limit(enabled),
            PlatformAction::BatteryCalibration(enabled) => client.set_battery_calibration(enabled),
            PlatformAction::UsbCharging(mode) => client.set_usb_charging(mode),
            PlatformAction::KeyboardTimeout(enabled) => client.set_keyboard_timeout(enabled),
            PlatformAction::BootSound(enabled) => client.set_boot_sound(enabled),
            PlatformAction::LcdOverride(enabled) => client.set_lcd_override(enabled),
            PlatformAction::RearLogo(state) => client.set_rear_logo(state),
        })
        .map(|state| ControlOutcome::Platform { action, state }),
        ControlAction::Refresh => reconnect_control(control),
    }
}

fn initialize_control(control: &mut Option<ControlClient>) -> Result<ControlOutcome, String> {
    with_control(control, |client| {
        let capabilities = client.capabilities()?;
        let lighting = partial_control_result(keyboard_lighting_snapshot(client, &capabilities))?;
        let memory_hardware = partial_control_result(client.memory_hardware_info())?;
        let platform = partial_control_result(platform_snapshot(client, &capabilities))?;
        Ok(ControlOutcome::Initialize {
            capabilities,
            lighting,
            memory_hardware,
            platform,
        })
    })
}

fn reconnect_control(control: &mut Option<ControlClient>) -> Result<ControlOutcome, String> {
    // The daemon deliberately serves one fail-safe session at a time. A second
    // connection would wait in the listen queue until the current client
    // closed. Probe and reuse it, replacing it only after transport failure.
    let current_session_healthy = match control.as_mut() {
        Some(client) => match client.ping() {
            Ok(()) => true,
            Err(error) if error.invalidates_session() => false,
            Err(error) => return Err(error.to_string()),
        },
        None => false,
    };
    if !current_session_healthy {
        *control = None;
        *control = Some(ControlClient::connect().map_err(String::from)?);
    }
    with_control(control, |client| {
        let capabilities = client.capabilities()?;
        let lighting = partial_control_result(keyboard_lighting_snapshot(client, &capabilities))?;
        let platform = partial_control_result(platform_snapshot(client, &capabilities))?;
        Ok(ControlOutcome::Refresh {
            capabilities,
            lighting,
            platform,
        })
    })
}

fn queue_control_request(
    mut runtime: Signal<RuntimeState>,
    worker: &ControlWorker,
    request: ControlRequest,
) -> bool {
    {
        let mut state = runtime.write();
        if !begin_control_request(&mut state.view, request.clone()) {
            return false;
        }
    }
    if let Err(error) = worker.submit(request.clone()) {
        fail_control_request(&mut runtime.write().view, request, error);
        return false;
    }
    true
}

fn begin_control_request(view: &mut AppState, request: ControlRequest) -> bool {
    if view.control_busy {
        return false;
    }
    view.control_busy = true;
    if request.action.touches_platform() {
        view.platform_busy = true;
    }
    if request.foreground {
        view.health = HealthState::Applying;
        if matches!(&request.action, ControlAction::Platform(_)) {
            view.status = UiStatus::WritingAndVerifying;
        }
    }
    true
}

fn fail_control_request(view: &mut AppState, request: ControlRequest, error: String) {
    view.control_busy = false;
    let error_kind = request.action.error_kind();
    let detail = RawDetail::new(error);
    if request.action.touches_platform() {
        view.platform_busy = false;
        view.platform_error = Some(PlatformIssue::Raw(detail.clone()));
    }
    let initial_connection_failed =
        request.action == ControlAction::Initialize && view.capabilities.is_none();
    if initial_connection_failed {
        view.controls_enabled = false;
    }
    if request.foreground || initial_connection_failed {
        view.health = HealthState::Warning;
        view.status = UiStatus::Failure {
            kind: error_kind,
            detail,
        };
    }
}

fn apply_control_update(view: &mut AppState, update: ControlUpdate) {
    view.control_busy = false;
    if update.request.action.touches_platform() {
        view.platform_busy = false;
    }

    let outcome = match update.result {
        Ok(outcome) => outcome,
        Err(error) => {
            let error_kind = update.request.action.error_kind();
            let detail = RawDetail::new(error);
            if matches!(&update.request.action, ControlAction::Profile(_)) {
                view.profile_sync = ProfileTelemetrySync::default();
            }
            if update.request.action.touches_platform() {
                view.platform_error = Some(PlatformIssue::Raw(detail.clone()));
            }
            if update.request.action == ControlAction::Initialize && view.capabilities.is_none() {
                view.controls_enabled = false;
            }
            if update.request.foreground || update.request.action == ControlAction::Initialize {
                view.health = HealthState::Warning;
                view.status = UiStatus::Failure {
                    kind: error_kind,
                    detail,
                };
            }
            return;
        }
    };

    match outcome {
        ControlOutcome::RefreshedThen { refresh, result } => {
            apply_control_update(
                view,
                ControlUpdate {
                    request: ControlRequest::background(ControlAction::Refresh),
                    result: Ok(*refresh),
                },
            );
            apply_control_update(
                view,
                ControlUpdate {
                    request: update.request,
                    result: result.map(|outcome| *outcome),
                },
            );
        }
        ControlOutcome::Initialize {
            capabilities,
            lighting,
            memory_hardware,
            platform,
        } => {
            let (acer_controls, mut diagnostics) =
                apply_capability_snapshot(view, capabilities, lighting, platform);
            match memory_hardware {
                Ok(memory_hardware) => {
                    view.hardware.memory = memory_hardware;
                    view.hardware_error = None;
                }
                Err(error) => {
                    let detail = RawDetail::new(error);
                    view.hardware_error = Some(detail.clone());
                    diagnostics.push(UiDiagnostic::Hardware(detail));
                }
            }
            if diagnostics.is_empty() {
                let status = if acer_controls {
                    UiStatus::AcerControlsConnected
                } else {
                    UiStatus::ReadOnlyTelemetryConnected
                };
                finish_control_success(view, status);
            } else {
                view.health = HealthState::Warning;
                view.status = UiStatus::PartialCapabilities(diagnostics);
            }
        }
        ControlOutcome::FanMode(mode) => {
            view.fan_mode = mode;
            finish_control_success(view, UiStatus::SettingsConfirmed);
        }
        ControlOutcome::ManualFans(request) => {
            view.fan_mode = FanMode::Manual;
            view.cpu_fan_percent = request.cpu_percent;
            view.gpu_fan_percent = request.gpu_percent;
            finish_control_success(view, UiStatus::SettingsConfirmed);
        }
        ControlOutcome::Profile {
            profile_raw,
            receipt,
        } => {
            view.platform_profile_raw = Some(profile_raw.clone());
            let profile = profile_from_raw_for_machine(
                &profile_raw,
                view.capabilities
                    .as_ref()
                    .is_none_or(|capabilities| capabilities.reference_model),
            );
            if let Some(profile) = profile {
                view.platform_profile = profile;
            }
            view.profile_sync = ProfileTelemetrySync {
                target: profile,
                grace_samples: PROFILE_SYNC_GRACE_SAMPLES,
                mismatch_samples: 0,
            };
            finish_control_success(view, UiStatus::ProfileVerified(receipt));
        }
        ControlOutcome::LightingApplied {
            request,
            firmware_state,
        } => {
            let state_readable = firmware_state.is_some();
            if let Some(state) = firmware_state {
                view.lighting = state;
            } else {
                view.last_applied_lighting
                    .retain(|previous| previous.device_id != request.device_id);
                view.last_applied_lighting.push(request);
            }
            finish_control_success(view, lighting_apply_status(state_readable));
        }
        ControlOutcome::LightingPowered(state) => {
            view.lighting = state;
            finish_control_success(view, UiStatus::LightingConfirmed);
        }
        ControlOutcome::Platform { action, state } => match store_platform_state(view, state) {
            Some(PlatformIssue::Readback(fields)) => {
                view.health = HealthState::Warning;
                view.status = UiStatus::PlatformReadbackFailed(fields);
            }
            Some(PlatformIssue::Raw(detail)) => {
                view.health = HealthState::Warning;
                view.status = UiStatus::Failure {
                    kind: UiErrorKind::Platform,
                    detail,
                };
            }
            None if update.request.foreground => {
                let status = match action {
                    PlatformAction::Refresh => UiStatus::PlatformRefreshed,
                    _ => UiStatus::SettingsConfirmed,
                };
                finish_control_success(view, status);
            }
            None => {}
        },
        ControlOutcome::Refresh {
            capabilities,
            lighting,
            platform,
        } => {
            let (_, diagnostics) =
                apply_capability_snapshot(view, capabilities, lighting, platform);
            if diagnostics.is_empty() {
                finish_control_success(view, UiStatus::PlatformRefreshed);
            } else {
                view.health = HealthState::Warning;
                view.status = UiStatus::PartialCapabilities(diagnostics);
            }
        }
    }
}

fn apply_capability_snapshot(
    view: &mut AppState,
    capabilities: ControlCapabilities,
    lighting: Result<KeyboardLightingState, String>,
    platform: Result<PlatformState, String>,
) -> (bool, Vec<UiDiagnostic>) {
    let reference_model = capabilities.reference_model;
    let acer_controls = capabilities.vendor.trim().eq_ignore_ascii_case("acer");
    view.platform_profile_raw = capabilities.profiles.current.clone();
    if let Some(profile) = view
        .platform_profile_raw
        .as_deref()
        .and_then(|raw| profile_from_raw_for_machine(raw, reference_model))
    {
        view.platform_profile = profile;
    }
    view.model_name = format!("{} {}", capabilities.vendor, capabilities.product);
    view.capabilities = Some(capabilities);
    view.controls_enabled = true;

    let mut diagnostics = Vec::new();
    match lighting {
        Ok(lighting) => {
            view.lighting = lighting;
            view.lighting_error = None;
        }
        Err(error) => {
            // A transient getter failure does not revoke the endpoint that
            // the fresh capability snapshot still advertises. Keep the last
            // verified readback (or the unavailable default during initial
            // discovery) and surface the read error separately.
            let detail = RawDetail::new(error);
            view.lighting_error = Some(detail.clone());
            diagnostics.push(UiDiagnostic::Lighting(detail));
        }
    }
    match platform {
        Ok(platform) => {
            if let Some(issue) = store_platform_state(view, platform) {
                diagnostics.push(UiDiagnostic::Platform(issue));
            }
        }
        Err(error) => {
            let issue = PlatformIssue::Raw(RawDetail::new(error));
            view.platform_error = Some(issue.clone());
            diagnostics.push(UiDiagnostic::Platform(issue));
        }
    }
    (acer_controls, diagnostics)
}

fn store_platform_state(view: &mut AppState, platform: PlatformState) -> Option<PlatformIssue> {
    let error =
        PlatformReadErrorSet::from_mask(platform.read_error_mask).map(PlatformIssue::Readback);
    if let Some(logo) = platform.rear_logo
        && logo.brightness > 0
    {
        view.rear_logo_last_nonzero_brightness = logo.brightness;
    }
    view.platform = Some(platform);
    view.platform_error = error.clone();
    view.platform_revision = view.platform_revision.wrapping_add(1);
    error
}

fn finish_control_success(view: &mut AppState, status: UiStatus) {
    view.health = HealthState::Healthy;
    view.status = status;
}

fn lighting_apply_status(state_readable: bool) -> UiStatus {
    if state_readable {
        UiStatus::SettingsConfirmed
    } else {
        UiStatus::AppliedWithoutReadback
    }
}

fn format_milliwatts(value: u32) -> String {
    if value.is_multiple_of(1_000) {
        (value / 1_000).to_string()
    } else {
        format!("{:.1}", f64::from(value) / 1_000.0)
    }
}

fn set_fan_mode(runtime: Signal<RuntimeState>, worker: &ControlWorker, mode: FanMode) {
    // Manual is a two-step operation in the UI: opening the editor must not
    // mutate hardware. Only the explicit Apply action sends FAN MANUAL.
    if mode == FanMode::Manual {
        return;
    }
    queue_control_request(
        runtime,
        worker,
        ControlRequest::foreground(ControlAction::FanMode(mode)),
    );
}

fn set_manual_fans(
    runtime: Signal<RuntimeState>,
    worker: &ControlWorker,
    request: ManualFanRequest,
) {
    queue_control_request(
        runtime,
        worker,
        ControlRequest::foreground(ControlAction::ManualFans(request)),
    );
}

fn set_platform_profile(
    runtime: Signal<RuntimeState>,
    worker: &ControlWorker,
    profile_raw: String,
) {
    queue_control_request(
        runtime,
        worker,
        ControlRequest::foreground(ControlAction::Profile(profile_raw)),
    );
}

fn apply_lighting(
    runtime: Signal<RuntimeState>,
    worker: &ControlWorker,
    request: LightingApplyRequest,
) {
    queue_control_request(
        runtime,
        worker,
        ControlRequest::foreground(ControlAction::LightingApply(request)),
    );
}

fn set_lighting_power(
    runtime: Signal<RuntimeState>,
    worker: &ControlWorker,
    request: LightingPowerRequest,
) {
    queue_control_request(
        runtime,
        worker,
        ControlRequest::foreground(ControlAction::LightingPower(request)),
    );
}

fn clear_profile_mismatch(view: &mut AppState, nvidia_offsets_available: bool) {
    if matches!(view.status, UiStatus::GpuProfileMismatch { .. }) {
        view.health = HealthState::Healthy;
        view.status = if nvidia_offsets_available {
            UiStatus::AcerNvidiaControlsConnected
        } else {
            UiStatus::AcerControlsConnected
        };
    }
}

fn reconcile_profile_telemetry(
    view: &mut AppState,
    hardware_profile: HardwareProfile,
    core_offset_mhz: Option<i32>,
    memory_offset_mhz: Option<i32>,
    offsets_uniform: Option<bool>,
) {
    if view.control_busy {
        return;
    }

    let observed_profile = PlatformProfile::from_hardware(hardware_profile);
    let expected_offsets = if hardware_profile == HardwareProfile::Turbo {
        (100, 200)
    } else {
        (0, 0)
    };
    let offset_readback = match (core_offset_mhz, memory_offset_mhz, offsets_uniform) {
        (Some(core), Some(memory), Some(uniform)) => Some((core, memory, uniform)),
        _ => None,
    };
    let synchronization = offset_readback
        .map(|(core, memory, uniform)| uniform && (core, memory) == expected_offsets);

    if let Some(target) = view.profile_sync.target {
        // The control daemon has already written and read back both firmware
        // and NVIDIA state. Keep that confirmed endpoint while the telemetry
        // thread's slower offset cache catches up with the profile sample.
        if observed_profile == target && synchronization != Some(false) {
            view.platform_profile = target;
            view.platform_profile_raw = Some(target.as_sysfs().to_string());
            view.profile_sync = ProfileTelemetrySync::default();
            clear_profile_mismatch(view, synchronization.is_some());
            return;
        }
        if view.profile_sync.grace_samples > 0 {
            view.profile_sync.grace_samples -= 1;
            return;
        }
        view.profile_sync.target = None;
        view.profile_sync.mismatch_samples = 0;
    }

    match synchronization {
        Some(false) => {
            view.profile_sync.mismatch_samples =
                view.profile_sync.mismatch_samples.saturating_add(1);
            if view.profile_sync.mismatch_samples < PROFILE_MISMATCH_DEBOUNCE_SAMPLES {
                return;
            }
            view.platform_profile = observed_profile;
            view.platform_profile_raw = Some(observed_profile.as_sysfs().to_string());
            let Some((core, memory, _)) = offset_readback else {
                return;
            };
            view.health = HealthState::Warning;
            view.status = UiStatus::GpuProfileMismatch {
                core_mhz: core,
                memory_mhz: memory,
            };
        }
        Some(true) => {
            view.platform_profile = observed_profile;
            view.platform_profile_raw = Some(observed_profile.as_sysfs().to_string());
            view.profile_sync.mismatch_samples = 0;
            clear_profile_mismatch(view, true);
        }
        None => {
            view.platform_profile = observed_profile;
            view.platform_profile_raw = Some(observed_profile.as_sysfs().to_string());
            view.profile_sync.mismatch_samples = 0;
            clear_profile_mismatch(view, false);
        }
    }
}

fn apply_telemetry(view: &mut AppState, sample: SystemTelemetry) {
    let privileged_memory = view.hardware.memory.clone();
    let mut hardware = sample.hardware;
    merge_privileged_memory(&mut hardware.memory, privileged_memory);
    view.hardware = hardware;
    let gpu_aux_fan_rpm = sample
        .fan_rpm_channels
        .iter()
        .find(|channel| channel.index == 3)
        .and_then(|channel| channel.rpm);
    let primary_fan_rpm = |index| {
        sample
            .fan_rpm_channels
            .iter()
            .find(|channel| channel.index == index)
            .and_then(|channel| channel.rpm)
    };
    let additional_fans = sample
        .fan_rpm_channels
        .iter()
        .filter(|channel| channel.index >= 4)
        .filter_map(|channel| channel.rpm.map(|rpm| (channel.label.clone(), rpm)))
        .collect();
    view.telemetry = Telemetry {
        cpu_temperature_c: sample.cpu_temperature_c,
        cpu_load_percent: sample.cpu_utilization_percent,
        memory_used_mib: Some(sample.memory_used_mib),
        memory_total_mib: Some(sample.memory_total_mib),
        cpu_fan_rpm: primary_fan_rpm(1),
        cpu_fan_max_rpm: 8_000,
        gpu_temperature_c: sample.gpu.temperature_c,
        gpu_sleeping: sample.gpu.sleeping,
        gpu_load_percent: sample.gpu.utilization_percent,
        gpu_fan_rpm: primary_fan_rpm(2),
        gpu_fan_max_rpm: 7_000,
        gpu_aux_fan_rpm,
        additional_fans,
        gpu_power_w: sample.gpu.power_w,
        gpu_pstate: sample.gpu.pstate.clone(),
        gpu_memory_used_mib: sample.gpu.memory_used_mib,
        gpu_memory_total_mib: sample.gpu.memory_total_mib,
        gpu_graphics_clock_mhz: sample.gpu.graphics_clock_mhz,
        gpu_memory_clock_mhz: sample.gpu.memory_clock_mhz,
        gpu_core_offset_mhz: sample.gpu.core_offset_mhz,
        gpu_memory_offset_mhz: sample.gpu.memory_offset_mhz,
        gpu_offsets_uniform: sample.gpu.offsets_uniform,
        gpu_enforced_power_limit_w: sample.gpu.enforced_power_limit_w,
        gpu_maximum_power_limit_w: sample.gpu.maximum_power_limit_w,
        gpu_clock_event_reasons: sample.gpu.clock_event_reasons,
        gpu_error: sample.gpu.nvidia_error.clone(),
        battery_percent: sample.power_supply.battery_percent,
        battery_status: sample.power_supply.battery_status,
        ac_online: sample.power_supply.ac_online,
        usb_power_online: sample.power_supply.usb_power_online,
    };
    view.history.push(TelemetryPoint::from(&view.telemetry));
    let reference_model = view
        .capabilities
        .as_ref()
        .is_some_and(|capabilities| capabilities.reference_model);
    if reference_model {
        if view.profile_sync.target.is_none() {
            view.platform_profile_raw = sample.profile_raw.clone();
        }
        if let Some(profile) = sample.profile {
            reconcile_profile_telemetry(
                view,
                profile,
                sample.gpu.core_offset_mhz,
                sample.gpu.memory_offset_mhz,
                sample.gpu.offsets_uniform,
            );
        } else if view.profile_sync.target.is_some() {
            view.profile_sync.grace_samples = view.profile_sync.grace_samples.saturating_sub(1);
            if view.profile_sync.grace_samples == 0 {
                view.profile_sync = ProfileTelemetrySync::default();
                view.platform_profile_raw = sample.profile_raw.clone();
            }
        }
    } else {
        view.profile_sync = ProfileTelemetrySync::default();
        view.platform_profile_raw = sample.profile_raw.clone();
        if let Some(raw) = sample.profile_raw.as_deref()
            && let Some(profile) = profile_from_raw_for_machine(raw, false)
        {
            view.platform_profile = profile;
        }
    }
    if let (Some(cpu_mode), Some(gpu_mode)) = (sample.fans.cpu.mode, sample.fans.gpu.mode)
        && cpu_mode == gpu_mode
    {
        view.fan_mode = match cpu_mode {
            HardwareFanMode::Automatic => FanMode::Auto,
            HardwareFanMode::Manual => FanMode::Manual,
            HardwareFanMode::Maximum => FanMode::Maximum,
        };
    }
    if sample.fans.cpu.mode == Some(HardwareFanMode::Manual)
        && sample.fans.gpu.mode == Some(HardwareFanMode::Manual)
    {
        view.cpu_fan_percent = sample.fans.cpu.pwm_percent().round() as u8;
        view.gpu_fan_percent = sample.fans.gpu.pwm_percent().round() as u8;
    }
}

fn merge_privileged_memory(current: &mut MemoryHardwareInfo, privileged: MemoryHardwareInfo) {
    current.total_mib = current.total_mib.or(privileged.total_mib);
    current.speed_mt_s = current.speed_mt_s.or(privileged.speed_mt_s);
    current.memory_type = current.memory_type.take().or(privileged.memory_type);
    current.channels = current.channels.or(privileged.channels);
    current.modules = current.modules.or(privileged.modules);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FanMode {
    #[default]
    Auto,
    Manual,
    Maximum,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DockTab {
    #[default]
    Fans,
    Keyboard,
}

impl FanMode {
    const ALL: [Self; 3] = [Self::Auto, Self::Manual, Self::Maximum];

    fn label(self, language: Language) -> &'static str {
        match self {
            Self::Auto => text(language, MessageId::FanModeAuto),
            Self::Manual => text(language, MessageId::AppLabel001),
            Self::Maximum => text(language, MessageId::FanModeMaximum),
        }
    }

    fn hint(self, language: Language) -> &'static str {
        match self {
            Self::Auto => text(language, MessageId::AppHint001),
            Self::Manual => text(language, MessageId::AppHint002),
            Self::Maximum => text(language, MessageId::AppHint003),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlatformProfile {
    LowPower,
    Quiet,
    #[default]
    Balanced,
    Performance,
    Turbo,
}

impl PlatformProfile {
    pub const ALL: [Self; 5] = [
        Self::LowPower,
        Self::Quiet,
        Self::Balanced,
        Self::Performance,
        Self::Turbo,
    ];

    fn label(self, language: Language) -> &'static str {
        match self {
            Self::LowPower => text(language, MessageId::ProfileEco),
            Self::Quiet => text(language, MessageId::AppLabel002),
            Self::Balanced => text(language, MessageId::AppLabel003),
            Self::Performance => text(language, MessageId::AppLabel004),
            Self::Turbo => text(language, MessageId::ProfileTurbo),
        }
    }

    fn as_sysfs(self) -> &'static str {
        match self {
            Self::LowPower => "low-power",
            Self::Quiet => "quiet",
            Self::Balanced => "balanced",
            Self::Performance => "balanced-performance",
            Self::Turbo => "performance",
        }
    }

    fn from_hardware(profile: HardwareProfile) -> Self {
        match profile {
            HardwareProfile::Eco => Self::LowPower,
            HardwareProfile::Quiet => Self::Quiet,
            HardwareProfile::Balanced => Self::Balanced,
            HardwareProfile::Performance => Self::Performance,
            HardwareProfile::Turbo => Self::Turbo,
        }
    }

    fn from_raw(raw: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|profile| profile.as_sysfs() == raw)
    }
}

fn profile_from_raw_for_machine(raw: &str, reference_model: bool) -> Option<PlatformProfile> {
    if !reference_model && raw == "performance" {
        return Some(PlatformProfile::Performance);
    }
    PlatformProfile::from_raw(raw)
}

fn profile_label_for_machine(
    raw: &str,
    language: Language,
    reference_model: bool,
) -> Option<&'static str> {
    profile_from_raw_for_machine(raw, reference_model).map(|profile| profile.label(language))
}

fn profile_display_label(
    raw: Option<&str>,
    fallback: PlatformProfile,
    capabilities: Option<&ControlCapabilities>,
    language: Language,
) -> String {
    let Some(raw) = raw else {
        if capabilities.is_some_and(|capabilities| capabilities.profiles.backend.is_none()) {
            return text(language, MessageId::CommonUnavailable).to_string();
        }
        return fallback.label(language).to_string();
    };
    let reference_model = capabilities.is_none_or(|capabilities| capabilities.reference_model);
    if let Some(label) = profile_label_for_machine(raw, language, reference_model) {
        return label.to_string();
    }
    capabilities
        .and_then(|capabilities| {
            capabilities
                .profiles
                .choices
                .iter()
                .find(|choice| choice.raw == raw)
        })
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| raw.replace('-', " "))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HealthState {
    #[default]
    Healthy,
    Applying,
    Warning,
}

impl HealthState {
    fn label(self, language: Language) -> &'static str {
        match self {
            Self::Healthy => text(language, MessageId::AppLabel005),
            Self::Applying => text(language, MessageId::AppLabel006),
            Self::Warning => text(language, MessageId::AppLabel007),
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::Healthy => "health-pill healthy",
            Self::Applying => "health-pill applying",
            Self::Warning => "health-pill warning",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Telemetry {
    pub cpu_temperature_c: Option<f32>,
    pub cpu_load_percent: Option<f32>,
    pub memory_used_mib: Option<u64>,
    pub memory_total_mib: Option<u64>,
    pub cpu_fan_rpm: Option<u32>,
    pub cpu_fan_max_rpm: u32,
    pub gpu_temperature_c: Option<f32>,
    pub gpu_sleeping: bool,
    pub gpu_load_percent: Option<f32>,
    pub gpu_fan_rpm: Option<u32>,
    pub gpu_fan_max_rpm: u32,
    pub gpu_aux_fan_rpm: Option<u32>,
    pub additional_fans: Vec<(String, u32)>,
    pub gpu_power_w: Option<f32>,
    pub gpu_pstate: Option<String>,
    pub gpu_memory_used_mib: Option<u64>,
    pub gpu_memory_total_mib: Option<u64>,
    pub gpu_graphics_clock_mhz: Option<u32>,
    pub gpu_memory_clock_mhz: Option<u32>,
    pub gpu_core_offset_mhz: Option<i32>,
    pub gpu_memory_offset_mhz: Option<i32>,
    pub gpu_offsets_uniform: Option<bool>,
    pub gpu_enforced_power_limit_w: Option<f32>,
    pub gpu_maximum_power_limit_w: Option<f32>,
    pub gpu_clock_event_reasons: Option<u64>,
    pub gpu_error: Option<String>,
    pub battery_percent: Option<u8>,
    pub battery_status: Option<BatteryStatus>,
    pub ac_online: Option<bool>,
    pub usb_power_online: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TelemetryPoint {
    cpu_load_percent: Option<f32>,
    memory_load_percent: Option<f32>,
    gpu_load_percent: Option<f32>,
    gpu_memory_load_percent: Option<f32>,
    cpu_temperature_c: Option<f32>,
    gpu_temperature_c: Option<f32>,
    gpu_power_w: Option<f32>,
    gpu_power_limit_w: Option<f32>,
    gpu_graphics_clock_mhz: Option<f32>,
    gpu_memory_clock_mhz: Option<f32>,
}

impl From<&Telemetry> for TelemetryPoint {
    fn from(value: &Telemetry) -> Self {
        let gpu_zero = value.gpu_sleeping.then_some(0.0);
        Self {
            cpu_load_percent: value.cpu_load_percent,
            memory_load_percent: ratio_percent(value.memory_used_mib, value.memory_total_mib),
            gpu_load_percent: gpu_zero.or(value.gpu_load_percent),
            gpu_memory_load_percent: ratio_percent(
                value.gpu_memory_used_mib,
                value.gpu_memory_total_mib,
            ),
            cpu_temperature_c: value.cpu_temperature_c,
            gpu_temperature_c: value.gpu_temperature_c,
            gpu_power_w: gpu_zero.or(value.gpu_power_w),
            gpu_power_limit_w: value.gpu_enforced_power_limit_w,
            gpu_graphics_clock_mhz: gpu_zero
                .or(value.gpu_graphics_clock_mhz.map(|value| value as f32)),
            gpu_memory_clock_mhz: gpu_zero.or(value.gpu_memory_clock_mhz.map(|value| value as f32)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TelemetryHistory {
    samples: [TelemetryPoint; TELEMETRY_HISTORY_CAPACITY],
    next: usize,
    len: usize,
}

impl Default for TelemetryHistory {
    fn default() -> Self {
        Self {
            samples: [TelemetryPoint::default(); TELEMETRY_HISTORY_CAPACITY],
            next: 0,
            len: 0,
        }
    }
}

impl TelemetryHistory {
    fn push(&mut self, sample: TelemetryPoint) {
        self.samples[self.next] = sample;
        self.next = (self.next + 1) % TELEMETRY_HISTORY_CAPACITY;
        self.len = (self.len + 1).min(TELEMETRY_HISTORY_CAPACITY);
    }

    fn get(&self, logical_index: usize) -> Option<&TelemetryPoint> {
        if logical_index >= self.len {
            return None;
        }
        let start = if self.len == TELEMETRY_HISTORY_CAPACITY {
            self.next
        } else {
            0
        };
        Some(&self.samples[(start + logical_index) % TELEMETRY_HISTORY_CAPACITY])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardLightingState {
    pub available: bool,
    pub powered: bool,
    pub mode: u8,
    pub speed: u8,
    pub brightness: u8,
    pub direction: u8,
    pub color: u32,
    pub zones: [u32; 4],
}

impl Default for KeyboardLightingState {
    fn default() -> Self {
        Self {
            available: false,
            powered: false,
            mode: 0,
            speed: 0,
            brightness: 100,
            direction: 0,
            color: 0x7c_5cff,
            zones: [0x36_d7ff, 0x6e_7cff, 0x9b_6dff, 0xd1_5cff],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ProfileTelemetrySync {
    target: Option<PlatformProfile>,
    grace_samples: u8,
    mismatch_samples: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppState {
    pub product_name: String,
    pub model_name: String,
    pub telemetry: Telemetry,
    pub hardware: HardwareInfo,
    pub history: TelemetryHistory,
    pub fan_mode: FanMode,
    pub cpu_fan_percent: u8,
    pub gpu_fan_percent: u8,
    pub platform_profile: PlatformProfile,
    pub platform_profile_raw: Option<String>,
    pub capabilities: Option<ControlCapabilities>,
    profile_sync: ProfileTelemetrySync,
    pub lighting: KeyboardLightingState,
    last_applied_lighting: Vec<LightingApplyRequest>,
    lighting_error: Option<RawDetail>,
    pub platform: Option<PlatformState>,
    pub platform_busy: bool,
    platform_error: Option<PlatformIssue>,
    hardware_error: Option<RawDetail>,
    pub platform_revision: u64,
    rear_logo_last_nonzero_brightness: u8,
    pub control_busy: bool,
    pub health: HealthState,
    status: UiStatus,
    pub controls_enabled: bool,
    telemetry_health: TelemetryHealth,
    telemetry_error: Option<RawDetail>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            product_name: "ASense".into(),
            model_name: "Acer Predator PHN16-72".into(),
            telemetry: Telemetry {
                cpu_fan_max_rpm: 8_000,
                gpu_fan_max_rpm: 7_000,
                ..Telemetry::default()
            },
            hardware: HardwareInfo::default(),
            history: TelemetryHistory::default(),
            fan_mode: FanMode::Auto,
            cpu_fan_percent: 50,
            gpu_fan_percent: 50,
            platform_profile: PlatformProfile::Balanced,
            platform_profile_raw: Some(PlatformProfile::Balanced.as_sysfs().to_string()),
            capabilities: None,
            profile_sync: ProfileTelemetrySync::default(),
            lighting: KeyboardLightingState::default(),
            last_applied_lighting: Vec::new(),
            lighting_error: None,
            platform: None,
            platform_busy: false,
            platform_error: None,
            hardware_error: None,
            platform_revision: 0,
            rear_logo_last_nonzero_brightness: 100,
            control_busy: false,
            health: HealthState::Healthy,
            status: UiStatus::AcerControlsConnected,
            controls_enabled: true,
            telemetry_health: TelemetryHealth::Online,
            telemetry_error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManualFanRequest {
    pub cpu_percent: u8,
    pub gpu_percent: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LightingApplyRequest {
    device_id: String,
    state_readable: bool,
    mode: ControlLightingMode,
    brightness: u8,
    speed: u8,
    color: [u8; 3],
    zone_colors: Vec<[u8; 3]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LightingPowerRequest {
    device_id: String,
    enabled: bool,
}

#[component]
fn Dashboard(
    state: AppState,
    language: Language,
    advanced_open: bool,
    on_fan_mode: EventHandler<FanMode>,
    on_manual_fans: EventHandler<ManualFanRequest>,
    on_profile: EventHandler<String>,
    on_lighting: EventHandler<LightingApplyRequest>,
    on_lighting_power: EventHandler<LightingPowerRequest>,
    on_platform: EventHandler<PlatformAction>,
    on_language: EventHandler<()>,
    on_refresh: EventHandler<()>,
    on_advanced: EventHandler<bool>,
) -> Element {
    let mut docs_open = use_signal(|| false);
    let telemetry = state.telemetry.clone();
    let localized_control_status = render_ui_status(language, &state.status);
    let compact_control_status = render_compact_status(language, &state.status);
    let telemetry_status = render_telemetry_status(language, state.telemetry_health);
    let localized_status_message = telemetry_status.as_ref().map_or_else(
        || localized_control_status.clone(),
        |(status, _)| format!("{localized_control_status} · {status}"),
    );
    let compact_status_message =
        telemetry_status.map_or(compact_control_status, |(_, compact)| compact);
    let status_title = state.telemetry_error.as_ref().map_or_else(
        || localized_status_message.clone(),
        |error| format!("{localized_status_message}: {}", error.as_str()),
    );
    let localized_platform_error = state
        .platform_error
        .as_ref()
        .map(|issue| render_platform_issue(language, issue));
    let displayed_health = if state.health == HealthState::Applying {
        HealthState::Applying
    } else if state.health == HealthState::Warning
        || state.telemetry_health != TelemetryHealth::Online
    {
        HealthState::Warning
    } else {
        HealthState::Healthy
    };
    let shell_class = if advanced_open {
        "asense-shell advanced"
    } else {
        "asense-shell"
    };

    rsx! {
        main {
            class: shell_class,
            lang: language.html_code(),

            section { class: "primary-panel", "aria-label": text(language, MessageId::AppDashboard003),
                AppHeader {
                    product_name: state.product_name,
                    model_name: state.model_name,
                    health: displayed_health,
                    status_message: status_title.clone(),
                    control_busy: state.control_busy,
                    language,
                    advanced_open,
                    on_info: move |_| docs_open.set(true),
                    on_language,
                    on_refresh,
                    on_advanced,
                }

                QuickStrip {
                    telemetry: telemetry.clone(),
                    profile: profile_display_label(
                        state.platform_profile_raw.as_deref(),
                        state.platform_profile,
                        state.capabilities.as_ref(),
                        language,
                    ),
                    language,
                }

                CoolingOverview { telemetry: telemetry.clone(), language }

                ControlDock {
                    fan_mode: state.fan_mode,
                    cpu_fan_percent: state.cpu_fan_percent,
                    gpu_fan_percent: state.gpu_fan_percent,
                    platform_profile: state.platform_profile,
                    platform_profile_raw: state.platform_profile_raw.clone(),
                    capabilities: state.capabilities.clone(),
                    lighting: state.lighting,
                    last_applied_lighting: state.last_applied_lighting,
                    lighting_error: state.lighting_error,
                    control_busy: state.control_busy,
                    controls_enabled: state.controls_enabled,
                    health: state.health,
                    language,
                    on_fan_mode,
                    on_manual_fans,
                    on_profile,
                    on_lighting,
                    on_lighting_power,
                }

                StatusBar {
                    telemetry,
                    status_message: status_title.clone(),
                    displayed_status: compact_status_message,
                    health: displayed_health,
                    language,
                }
            }

            if advanced_open {
                AdvancedPanel {
                    language,
                    telemetry: state.telemetry,
                    hardware: state.hardware,
                    history: state.history,
                    platform: state.platform,
                    platform_busy: state.control_busy || state.platform_busy,
                    platform_error: localized_platform_error,
                    platform_revision: state.platform_revision,
                    rear_logo_last_nonzero_brightness: state.rear_logo_last_nonzero_brightness,
                    on_platform,
                }
            }

            docs_modal::DocsModal {
                open: docs_open(),
                language,
                on_close: move |_| docs_open.set(false),
            }
        }
    }
}

#[component]
fn AppHeader(
    product_name: String,
    model_name: String,
    health: HealthState,
    status_message: String,
    control_busy: bool,
    language: Language,
    advanced_open: bool,
    on_info: EventHandler<()>,
    on_language: EventHandler<()>,
    on_refresh: EventHandler<()>,
    on_advanced: EventHandler<bool>,
) -> Element {
    rsx! {
        header { class: "app-header",
            div { class: "brand",
                div { class: "brand-mark", "A" }
                div { class: "brand-copy",
                    h1 { "{product_name}" }
                    span { "{model_name}" }
                }
            }
            div { class: "header-actions",
                button {
                    class: "info-toggle",
                    r#type: "button",
                    title: text(language, MessageId::AppHeader001),
                    "aria-label": text(language, MessageId::AppHeader002),
                    onclick: move |_| {
                        on_info.call(());
                        let _ = document::eval(
                            "requestAnimationFrame(() => document.querySelector('.docs-close')?.focus())",
                        );
                    },
                    "?"
                }
                button {
                    class: "language-toggle",
                    r#type: "button",
                    title: text(language, MessageId::AppHeader003),
                    onclick: move |_| on_language.call(()),
                    "{language.display_code()}"
                }
                button {
                    class: health.class(),
                    r#type: "button",
                    title: "{status_message}",
                    disabled: control_busy || health == HealthState::Applying,
                    onclick: move |_| on_refresh.call(()),
                    span { class: "health-dot" }
                    "{health.label(language)}"
                }
                button {
                    class: if advanced_open { "advanced-toggle active" } else { "advanced-toggle" },
                    r#type: "button",
                    role: "switch",
                    "aria-checked": advanced_open,
                    title: if advanced_open {
                        text(language, MessageId::AppHeader004)
                    } else {
                        text(language, MessageId::AppHeader005)
                    },
                    onclick: move |_| on_advanced.call(!advanced_open),
                    span { class: "toggle-indicator" }
                    {text(language, MessageId::AppHeader006)}
                }
            }
        }
    }
}

#[component]
fn QuickStrip(telemetry: Telemetry, profile: String, language: Language) -> Element {
    rsx! {
        section { class: "quick-strip", "aria-label": text(language, MessageId::AppQuickStrip001),
            MetricPill {
                label: "CPU",
                value: temperature(telemetry.cpu_temperature_c),
                level: temperature_level(telemetry.cpu_temperature_c),
            }
            MetricPill {
                label: text(language, MessageId::CommonLoad),
                value: percent(telemetry.cpu_load_percent),
                level: "neutral",
            }
            div { class: "profile-pill",
                span { {text(language, MessageId::AppQuickStrip002)} }
                strong { "{profile}" }
            }
            MetricPill {
                label: "GPU",
                value: temperature(telemetry.gpu_temperature_c),
                level: temperature_level(telemetry.gpu_temperature_c),
            }
            MetricPill {
                label: text(language, MessageId::CommonLoad),
                value: if telemetry.gpu_sleeping {
                    text(language, MessageId::CommonSleeping).to_string()
                } else {
                    percent(telemetry.gpu_load_percent)
                },
                level: "neutral",
            }
        }
    }
}

#[component]
fn CoolingOverview(telemetry: Telemetry, language: Language) -> Element {
    rsx! {
        section { class: "gauge-grid", "aria-label": text(language, MessageId::CoolingTelemetry),
            FanGauge {
                kind: "CPU",
                rpm: telemetry.cpu_fan_rpm,
                max_rpm: telemetry.cpu_fan_max_rpm.max(1),
                temperature_c: telemetry.cpu_temperature_c,
                accent: "cyan",
                secondary_rpm: None,
            }
            FanGauge {
                kind: "GPU",
                rpm: telemetry.gpu_fan_rpm,
                max_rpm: telemetry.gpu_fan_max_rpm.max(1),
                temperature_c: telemetry.gpu_temperature_c,
                accent: "violet",
                secondary_rpm: telemetry.gpu_aux_fan_rpm,
            }
        }
    }
}

fn keyboard_editor_readback(lighting: &KeyboardLightingState) -> Option<(u8, [u32; 4])> {
    if !lighting.available {
        return None;
    }
    let mut editor_colors = lighting.zones;
    if matches!(lighting.mode, 1 | 4) {
        // Breathing and Shifting use the firmware's single effect color. The
        // first color well is also the effect-color editor, so seed it from
        // that readback instead of from the last static zone configuration.
        editor_colors[0] = lighting.color;
    }
    Some((lighting.brightness, editor_colors))
}

fn fan_mode_supported(
    capabilities: Option<&crate::control::ControlFanCapabilities>,
    mode: FanMode,
) -> bool {
    capabilities.is_none_or(|capabilities| match mode {
        FanMode::Auto => capabilities.auto,
        FanMode::Manual => capabilities.manual,
        FanMode::Maximum => capabilities.maximum,
    })
}

fn rgb_bytes(color: u32) -> [u8; 3] {
    [(color >> 16) as u8, (color >> 8) as u8, color as u8]
}

fn preferred_lighting_index(devices: &[ControlLightingDevice]) -> Option<usize> {
    devices
        .iter()
        .position(|device| {
            device.target == CapabilityLightingTarget::Keyboard
                && device.backend == CapabilityLightingBackend::ZonedWmi
                && device.state_readable
        })
        .or_else(|| {
            devices
                .iter()
                .position(|device| device.target == CapabilityLightingTarget::Keyboard)
        })
}

fn lighting_zone_draft(seed: &[u32]) -> Vec<u32> {
    let defaults = KeyboardLightingState::default().zones;
    (0..usize::from(MAX_LIGHTING_ZONES))
        .map(|index| {
            seed.get(index)
                .copied()
                .unwrap_or(defaults[index % defaults.len()])
        })
        .collect()
}

fn lighting_draft_for_device(
    device: &ControlLightingDevice,
    lighting: &KeyboardLightingState,
    last_applied: &[LightingApplyRequest],
) -> (u8, Vec<u32>) {
    if device.state_readable
        && lighting.available
        && let Some((brightness, zones)) = keyboard_editor_readback(lighting)
    {
        return (brightness, lighting_zone_draft(&zones));
    }

    if let Some(request) = last_applied
        .iter()
        .find(|request| request.device_id == device.id)
    {
        let mut colors = request
            .zone_colors
            .iter()
            .map(|color| u32::from_be_bytes([0, color[0], color[1], color[2]]))
            .collect::<Vec<_>>();
        if colors.is_empty() {
            colors.push(u32::from_be_bytes([
                0,
                request.color[0],
                request.color[1],
                request.color[2],
            ]));
        }
        return (request.brightness, lighting_zone_draft(&colors));
    }

    let defaults = KeyboardLightingState::default();
    (defaults.brightness, lighting_zone_draft(&defaults.zones))
}

fn lighting_mode_visibility(modes: Option<ControlLightingModes>) -> (bool, bool, bool, bool) {
    modes.map_or((true, true, true, true), |modes| {
        (
            modes.static_color,
            modes.brightness,
            modes.breathing,
            modes.neon,
        )
    })
}

fn lighting_mode_number(mode: ControlLightingMode) -> u8 {
    match mode {
        ControlLightingMode::Off | ControlLightingMode::Static => 0,
        ControlLightingMode::Breathing => 1,
        ControlLightingMode::Neon => 2,
    }
}

fn lighting_request(
    device_id: String,
    state_readable: bool,
    mode: ControlLightingMode,
    brightness: u8,
    speed: u8,
    color: u32,
    zone_colors: &[u32],
) -> LightingApplyRequest {
    LightingApplyRequest {
        device_id,
        state_readable,
        mode,
        brightness,
        speed,
        color: rgb_bytes(color),
        zone_colors: zone_colors.iter().copied().map(rgb_bytes).collect(),
    }
}

fn lighting_target_label(target: CapabilityLightingTarget, language: Language) -> &'static str {
    match target {
        CapabilityLightingTarget::Keyboard => text(language, MessageId::CommonKeyboard),
        CapabilityLightingTarget::CoverLogo => text(language, MessageId::AppLightingTargetLabel001),
        CapabilityLightingTarget::RearLogo => text(language, MessageId::AppLightingTargetLabel002),
        CapabilityLightingTarget::Lightbar => text(language, MessageId::AppLightingTargetLabel003),
    }
}

#[component]
fn ControlDock(
    fan_mode: FanMode,
    cpu_fan_percent: u8,
    gpu_fan_percent: u8,
    platform_profile: PlatformProfile,
    platform_profile_raw: Option<String>,
    capabilities: Option<ControlCapabilities>,
    lighting: KeyboardLightingState,
    last_applied_lighting: Vec<LightingApplyRequest>,
    lighting_error: Option<RawDetail>,
    control_busy: bool,
    controls_enabled: bool,
    health: HealthState,
    language: Language,
    on_fan_mode: EventHandler<FanMode>,
    on_manual_fans: EventHandler<ManualFanRequest>,
    on_profile: EventHandler<String>,
    on_lighting: EventHandler<LightingApplyRequest>,
    on_lighting_power: EventHandler<LightingPowerRequest>,
) -> Element {
    let mut lighting_devices = capabilities
        .as_ref()
        .map(|capabilities| capabilities.lighting.clone())
        .unwrap_or_default();
    if let Some(preferred) = preferred_lighting_index(&lighting_devices)
        && preferred != 0
    {
        lighting_devices.swap(0, preferred);
    }

    let mut cpu_draft = use_signal(move || cpu_fan_percent);
    let mut gpu_draft = use_signal(move || gpu_fan_percent);
    let initial_manual = fan_mode == FanMode::Manual;
    let mut fan_editor_open = use_signal(move || initial_manual);
    let mut observed_fan_mode = use_signal(move || fan_mode);
    if *observed_fan_mode.peek() != fan_mode {
        let previous = *observed_fan_mode.peek();
        observed_fan_mode.set(fan_mode);
        if fan_mode == FanMode::Manual {
            cpu_draft.set(cpu_fan_percent.clamp(20, 100));
            gpu_draft.set(gpu_fan_percent.clamp(20, 100));
        }
        if previous == FanMode::Manual && fan_mode != FanMode::Manual {
            fan_editor_open.set(false);
        }
    }
    let mut dock_tab = use_signal(DockTab::default);
    let mut selected_lighting_index = use_signal(|| 0_usize);
    let selected_lighting = selected_lighting_index().min(lighting_devices.len().saturating_sub(1));
    let keyboard_device = lighting_devices
        .get(selected_lighting)
        .or_else(|| lighting_devices.first())
        .cloned();
    let zone_count = keyboard_device
        .as_ref()
        .map_or(4, |device| device.zones.clamp(1, MAX_LIGHTING_ZONES));
    let initial_brightness = if lighting.brightness == 0 {
        KeyboardLightingState::default().brightness.max(1)
    } else {
        lighting.brightness
    };
    let mut keyboard_brightness = use_signal(move || initial_brightness);
    let initial_colors = lighting_zone_draft(&lighting.zones);
    let mut zone_colors = use_signal(move || initial_colors);
    let mut lighting_draft_dirty = use_signal(|| false);
    let lighting_editor_readback = keyboard_device.as_ref().and_then(|device| {
        (device.state_readable && lighting.available)
            .then(|| keyboard_editor_readback(&lighting))
            .flatten()
            .map(|(brightness, zones)| (device.id.clone(), brightness, zones))
    });
    let mut observed_lighting_readback = use_signal(|| None::<(String, u8, [u32; 4])>);
    if *observed_lighting_readback.peek() != lighting_editor_readback {
        observed_lighting_readback.set(lighting_editor_readback.clone());
        if let Some((_, brightness, zones)) = lighting_editor_readback {
            if brightness > 0 {
                keyboard_brightness.set(brightness);
            }
            zone_colors.set(lighting_zone_draft(&zones));
            lighting_draft_dirty.set(false);
        }
    }

    let manual_supported = fan_mode_supported(
        capabilities.as_ref().map(|capabilities| &capabilities.fans),
        FanMode::Manual,
    );
    let manual = manual_supported && (fan_mode == FanMode::Manual || fan_editor_open());
    let selected_fan_mode = if manual { FanMode::Manual } else { fan_mode };
    let enabled = controls_enabled && !control_busy && health != HealthState::Applying;
    let mut profile_choices = capabilities.as_ref().map_or_else(
        || {
            PlatformProfile::ALL
                .into_iter()
                .map(|profile| ControlProfileChoice {
                    raw: profile.as_sysfs().to_string(),
                    label: profile.label(language).to_string(),
                    selectable: true,
                })
                .collect::<Vec<_>>()
        },
        |capabilities| capabilities.profiles.choices.clone(),
    );
    let reference_model = capabilities
        .as_ref()
        .is_none_or(|capabilities| capabilities.reference_model);
    for choice in &mut profile_choices {
        if let Some(label) = profile_label_for_machine(&choice.raw, language, reference_model) {
            choice.label = label.to_string();
        }
    }
    let selected_profile_raw =
        platform_profile_raw.unwrap_or_else(|| platform_profile.as_sysfs().to_string());
    let profile_count = profile_choices.len().max(1);
    let profile_source_hint = match capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.profiles.backend)
    {
        Some(CapabilityProfileBackend::Kernel) => text(language, MessageId::AppControlDock001),
        Some(CapabilityProfileBackend::AcerGamingWmi) => {
            text(language, MessageId::AppControlDock002)
        }
        None => text(language, MessageId::AppControlDock003),
    };
    let fan_capabilities = capabilities
        .as_ref()
        .map(|capabilities| capabilities.fans.clone());
    let fan_control_available = fan_capabilities
        .as_ref()
        .is_none_or(|capabilities| capabilities.backend.is_some());
    let supported_fan_modes = FanMode::ALL
        .into_iter()
        .filter(|mode| fan_mode_supported(fan_capabilities.as_ref(), *mode))
        .collect::<Vec<_>>();
    let lighting_available = !lighting_devices.is_empty();
    let lighting_drafts = lighting_devices
        .iter()
        .map(|device| lighting_draft_for_device(device, &lighting, &last_applied_lighting))
        .collect::<Vec<_>>();
    let dock_column_count = 1 + lighting_devices.len().max(1);
    let lighting_modes = keyboard_device.as_ref().map(|device| device.modes);
    let (show_static, show_brightness, show_breathing, show_neon) =
        lighting_mode_visibility(lighting_modes);
    let lighting_action_count =
        usize::from(show_static) + usize::from(show_breathing) + usize::from(show_neon);
    let lighting_device_id = keyboard_device.as_ref().map(|device| device.id.clone());
    let power_on_device = lighting_device_id.clone();
    let power_off_device = lighting_device_id.clone();
    let auto_color_device = lighting_device_id.clone();
    let static_device = lighting_device_id.clone();
    let breathing_device = lighting_device_id.clone();
    let neon_device = lighting_device_id;
    let lighting_control_label = keyboard_device
        .as_ref()
        .map_or(text(language, MessageId::AppControlDock004), |device| {
            lighting_target_label(device.target, language)
        });
    let lighting_state_readable = keyboard_device
        .as_ref()
        .is_none_or(|device| device.state_readable && lighting.available);
    let typed_power_available = keyboard_device.as_ref().is_some_and(|device| {
        device.backend == CapabilityLightingBackend::ZonedWmi
            && device.target == CapabilityLightingTarget::Keyboard
            && device.state_readable
    });
    let selected_last_applied = keyboard_device.as_ref().and_then(|device| {
        last_applied_lighting
            .iter()
            .find(|request| request.device_id == device.id)
    });
    let lighting_state_last_applied = selected_last_applied.is_some();
    let lighting_state_known = lighting_state_readable || lighting_state_last_applied;
    let displayed_lighting_power = if lighting_state_readable {
        lighting.powered
    } else {
        selected_last_applied.is_some_and(|request| request.mode != ControlLightingMode::Off)
    };
    let displayed_lighting_mode = if lighting_state_readable {
        lighting.mode
    } else {
        selected_last_applied
            .map(|request| lighting_mode_number(request.mode))
            .unwrap_or_default()
    };
    let lighting_state_label = if lighting_state_readable {
        text(language, MessageId::AppControlDock005)
    } else if lighting_state_last_applied {
        text(language, MessageId::AppControlDock006)
    } else {
        text(language, MessageId::AppControlDock007)
    };

    rsx! {
        section { class: "control-dock", "aria-label": text(language, MessageId::AppControlDock008),
            div {
                class: "profile-switch",
                style: "grid-template-columns:repeat({profile_count},minmax(0,1fr))",
                title: profile_source_hint,
                "aria-label": text(language, MessageId::AppControlDock009),
                for choice in profile_choices {
                    button {
                        class: if selected_profile_raw == choice.raw { "profile active" } else { "profile" },
                        r#type: "button",
                        disabled: !enabled || !choice.selectable,
                        onclick: {
                            let raw = choice.raw.clone();
                            move |_| on_profile.call(raw.clone())
                        },
                        {choice.label.clone()}
                    }
                }
            }

            div {
                class: "dock-tabs",
                style: "grid-template-columns:repeat({dock_column_count},minmax(0,1fr))",
                role: "tablist",
                button {
                    class: if dock_tab() == DockTab::Fans { "dock-tab active" } else { "dock-tab" },
                    r#type: "button",
                    role: "tab",
                    "aria-selected": dock_tab() == DockTab::Fans,
                    onclick: move |_| dock_tab.set(DockTab::Fans),
                    {text(language, MessageId::AppControlDock010)}
                }
                if lighting_devices.is_empty() {
                    button {
                        class: if dock_tab() == DockTab::Keyboard { "dock-tab active" } else { "dock-tab" },
                        r#type: "button",
                        role: "tab",
                        "aria-selected": dock_tab() == DockTab::Keyboard,
                        disabled: !lighting_available,
                        title: lighting_error.as_ref().map(RawDetail::as_str).unwrap_or(if lighting_available {
                            text(language, MessageId::AppControlDock011)
                        } else {
                            text(language, MessageId::AppControlDock012)
                        }),
                        onclick: move |_| dock_tab.set(DockTab::Keyboard),
                        {text(language, MessageId::CommonKeyboard)}
                    }
                } else {
                    for (index, device) in lighting_devices.iter().enumerate() {
                        button {
                            class: if dock_tab() == DockTab::Keyboard && selected_lighting == index { "dock-tab active" } else { "dock-tab" },
                            r#type: "button",
                            role: "tab",
                            "aria-selected": dock_tab() == DockTab::Keyboard && selected_lighting == index,
                            onclick: {
                                let (draft_brightness, draft_colors) = lighting_drafts[index].clone();
                                move |_| {
                                    keyboard_brightness.set(draft_brightness);
                                    zone_colors.set(draft_colors.clone());
                                    lighting_draft_dirty.set(false);
                                    selected_lighting_index.set(index);
                                    dock_tab.set(DockTab::Keyboard);
                                }
                            },
                            {lighting_target_label(device.target, language)}
                        }
                    }
                }
            }

            div { class: "dock-content",
                if dock_tab() == DockTab::Keyboard {
                    div { class: "keyboard-panel",
                        div { class: "lighting-power", "aria-label": text(language, MessageId::AppControlDock013),
                            div { class: "lighting-label",
                                span { "{lighting_control_label}" }
                                small { "{lighting_state_label}" }
                            }
                            if show_static {
                                button {
                                    class: if lighting_state_known && displayed_lighting_power { "active" } else { "" },
                                    r#type: "button",
                                    disabled: !enabled,
                                    onclick: move |_| {
                                        if let Some(device_id) = power_on_device.clone() {
                                            if typed_power_available && !lighting_draft_dirty() {
                                                on_lighting_power.call(LightingPowerRequest {
                                                    device_id,
                                                    enabled: true,
                                                });
                                            } else {
                                                let colors = zone_colors.read();
                                                on_lighting.call(lighting_request(
                                                    device_id,
                                                    lighting_state_readable,
                                                    ControlLightingMode::Static,
                                                    keyboard_brightness(),
                                                    0,
                                                    colors[0],
                                                    &colors[..usize::from(zone_count)],
                                                ));
                                            }
                                        }
                                    },
                                    {text(language, MessageId::CommonOn)}
                                }
                                button {
                                    class: if lighting_state_known && !displayed_lighting_power { "active" } else { "" },
                                    r#type: "button",
                                    disabled: !enabled,
                                    onclick: move |_| {
                                        if let Some(device_id) = power_off_device.clone() {
                                            if typed_power_available {
                                                on_lighting_power.call(LightingPowerRequest {
                                                    device_id,
                                                    enabled: false,
                                                });
                                            } else {
                                                let colors = zone_colors.read();
                                                on_lighting.call(lighting_request(
                                                    device_id,
                                                    lighting_state_readable,
                                                    ControlLightingMode::Off,
                                                    keyboard_brightness(),
                                                    0,
                                                    colors[0],
                                                    &[],
                                                ));
                                            }
                                        }
                                    },
                                    {text(language, MessageId::CommonOff)}
                                }
                            }
                        }
                        if show_static {
                            div {
                                class: "zone-colors",
                                style: "grid-template-columns:repeat({zone_count},minmax(34px,1fr))",
                                for zone_index in 0..usize::from(zone_count) {
                                    ColorInput {
                                        key: "{zone_index}",
                                        language,
                                        label: zone_index + 1,
                                        value: zone_colors.read()[zone_index],
                                        on_change: move |value| zone_colors.write()[zone_index] = value,
                                        on_commit: {
                                            let device_id = auto_color_device.clone();
                                            move |value| {
                                                let mut colors = zone_colors.peek().clone();
                                                colors[zone_index] = value;
                                                zone_colors.set(colors.clone());
                                                if enabled && displayed_lighting_power
                                                    && let Some(device_id) = device_id.clone()
                                                {
                                                    on_lighting.call(lighting_request(
                                                        device_id,
                                                        lighting_state_readable,
                                                        ControlLightingMode::Static,
                                                        keyboard_brightness(),
                                                        0,
                                                        colors[0],
                                                        &colors[..usize::from(zone_count)],
                                                    ));
                                                } else if !displayed_lighting_power {
                                                    lighting_draft_dirty.set(true);
                                                }
                                            }
                                        },
                                    }
                                }
                            }
                        }
                        if show_brightness {
                            label { class: "light-slider",
                                span { {text(language, MessageId::CommonBrightness)} }
                                input {
                                    r#type: "range",
                                    min: "1",
                                    max: "100",
                                    step: "1",
                                    value: "{keyboard_brightness}",
                                    style: "--value:{keyboard_brightness}%",
                                    disabled: !enabled,
                                    oninput: move |event| {
                                        if let Ok(value) = event.value().parse::<u8>() {
                                            keyboard_brightness.set(value.min(100));
                                            lighting_draft_dirty.set(true);
                                        }
                                    },
                                }
                                strong { "{keyboard_brightness}%" }
                            }
                        }
                        if lighting_action_count > 0 {
                            div {
                                class: "lighting-actions",
                                style: "grid-template-columns:repeat({lighting_action_count},minmax(0,1fr))",
                                if show_static {
                                    button {
                                        class: if lighting_state_known && displayed_lighting_mode == 0 { "active" } else { "" },
                                        r#type: "button",
                                        disabled: !enabled,
                                        onclick: move |_| {
                                            let colors = zone_colors.read();
                                            if let Some(device_id) = static_device.clone() {
                                                on_lighting.call(lighting_request(
                                                    device_id,
                                                    lighting_state_readable,
                                                    ControlLightingMode::Static,
                                                    keyboard_brightness(),
                                                    0,
                                                    colors[0],
                                                    &colors[..usize::from(zone_count)],
                                                ));
                                            }
                                        },
                                        {text(language, MessageId::AppControlDock014)}
                                    }
                                }
                                if show_breathing {
                                    button {
                                        class: if lighting_state_known && displayed_lighting_mode == 1 { "active" } else { "" },
                                        r#type: "button",
                                        disabled: !enabled,
                                        onclick: move |_| {
                                            let color = zone_colors.read()[0];
                                            if let Some(device_id) = breathing_device.clone() {
                                                on_lighting.call(lighting_request(
                                                    device_id,
                                                    lighting_state_readable,
                                                    ControlLightingMode::Breathing,
                                                    keyboard_brightness(),
                                                    0,
                                                    color,
                                                    &[],
                                                ));
                                            }
                                        },
                                        {text(language, MessageId::AppControlDock015)}
                                    }
                                }
                                if show_neon {
                                    button {
                                        class: if lighting_state_known && displayed_lighting_mode == 2 { "active" } else { "" },
                                        r#type: "button",
                                        disabled: !enabled,
                                        onclick: move |_| {
                                            let color = zone_colors.read()[0];
                                            if let Some(device_id) = neon_device.clone() {
                                                on_lighting.call(lighting_request(
                                                    device_id,
                                                    lighting_state_readable,
                                                    ControlLightingMode::Neon,
                                                    keyboard_brightness(),
                                                    5,
                                                    color,
                                                    &[],
                                                ));
                                            }
                                        },
                                        {text(language, MessageId::AppControlDock016)}
                                    }
                                }
                            }
                        }
                    }
                } else {
                    div { class: if manual { "fan-panel manual" } else { "fan-panel" },
                        if fan_control_available {
                            div { class: "mode-switch", "aria-label": text(language, MessageId::AppControlDock017),
                                for mode in supported_fan_modes {
                                    button {
                                        class: if mode == selected_fan_mode { "mode active" } else { "mode" },
                                        r#type: "button",
                                        disabled: !enabled,
                                        title: "{mode.hint(language)}",
                                        onclick: move |_| {
                                            if mode == FanMode::Manual {
                                                fan_editor_open.set(true);
                                            } else {
                                                fan_editor_open.set(false);
                                                on_fan_mode.call(mode);
                                            }
                                        },
                                        "{mode.label(language)}"
                                    }
                                }
                            }
                        }

                        if manual {
                            div { class: "manual-panel",
                                FanSlider {
                                    label: "CPU",
                                    value: cpu_draft(),
                                    disabled: !enabled,
                                    on_change: move |value| cpu_draft.set(value),
                                }
                                FanSlider {
                                    label: "GPU",
                                    value: gpu_draft(),
                                    disabled: !enabled,
                                    on_change: move |value| gpu_draft.set(value),
                                }
                                button {
                                    class: "apply-button",
                                    r#type: "button",
                                    disabled: !enabled || !manual_supported,
                                    onclick: move |_| {
                                        fan_editor_open.set(false);
                                        on_manual_fans.call(ManualFanRequest {
                                            cpu_percent: cpu_draft(),
                                            gpu_percent: gpu_draft(),
                                        });
                                    },
                                    {text(language, MessageId::CommonApply)}
                                }
                            }
                        } else {
                            div {
                                class: if selected_fan_mode == FanMode::Maximum {
                                    "fan-mode-summary maximum"
                                } else {
                                    "fan-mode-summary"
                                },
                                if fan_control_available {
                                    span { class: "fan-summary-dot" }
                                }
                                strong {
                                    if !fan_control_available {
                                        {text(language, MessageId::AppControlDock018)}
                                    } else if selected_fan_mode == FanMode::Maximum {
                                        {text(language, MessageId::AppControlDock019)}
                                    } else {
                                        {text(language, MessageId::AppControlDock020)}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn StatusBar(
    telemetry: Telemetry,
    status_message: String,
    displayed_status: String,
    health: HealthState,
    language: Language,
) -> Element {
    let class = match health {
        HealthState::Healthy => "status-line healthy",
        HealthState::Applying => "status-line applying",
        HealthState::Warning => "status-line warning",
    };
    rsx! {
        footer { class,
            span { class: "status-text", title: "{status_message}", "{displayed_status}" }
            span { class: "power-readout",
                "GPU "
                strong { "{power_usage_limit(telemetry.gpu_power_w, telemetry.gpu_enforced_power_limit_w)}" }
                " · VF "
                strong { "{offsets(telemetry.gpu_core_offset_mhz, telemetry.gpu_memory_offset_mhz, telemetry.gpu_offsets_uniform, language)}" }
            }
        }
    }
}

#[component]
fn SettingToggle(
    class_name: &'static str,
    label: &'static str,
    detail: &'static str,
    value: Option<bool>,
    read_failed: bool,
    disabled: bool,
    language: Language,
    on_change: EventHandler<bool>,
) -> Element {
    let supported = value.is_some();
    let enabled = value.unwrap_or(false);
    let toggle_text = setting_toggle_text(value, read_failed, language);
    rsx! {
        div { class: "setting-toggle {class_name}",
            div { class: "setting-copy",
                strong { "{label}" }
                span { "{detail}" }
            }
            button {
                class: if enabled { "toggle-button active" } else { "toggle-button" },
                disabled: disabled || !supported,
                onclick: move |_| on_change.call(!enabled),
                "{toggle_text}"
            }
        }
    }
}

fn setting_toggle_text(value: Option<bool>, read_failed: bool, language: Language) -> &'static str {
    match value {
        Some(true) => text(language, MessageId::CommonOn),
        Some(false) => text(language, MessageId::CommonOff),
        None if read_failed => text(language, MessageId::CommonReadError),
        None => text(language, MessageId::CommonUnsupported),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AdvancedTab {
    #[default]
    Metrics,
    Hardware,
    Platform,
}

#[component]
fn AdvancedPanel(
    language: Language,
    telemetry: Telemetry,
    hardware: HardwareInfo,
    history: TelemetryHistory,
    platform: Option<PlatformState>,
    platform_busy: bool,
    platform_error: Option<String>,
    platform_revision: u64,
    rear_logo_last_nonzero_brightness: u8,
    on_platform: EventHandler<PlatformAction>,
) -> Element {
    let mut tab = use_signal(AdvancedTab::default);
    let cpu_load_points = graph_points(&history, |point| point.cpu_load_percent, 100.0);
    let ram_points = graph_points(&history, |point| point.memory_load_percent, 100.0);
    let gpu_load_points = graph_points(&history, |point| point.gpu_load_percent, 100.0);
    let vram_points = graph_points(&history, |point| point.gpu_memory_load_percent, 100.0);
    let cpu_temperature_points = graph_points(&history, |point| point.cpu_temperature_c, 110.0);
    let gpu_temperature_points = graph_points(&history, |point| point.gpu_temperature_c, 110.0);
    let power_ceiling = telemetry
        .gpu_enforced_power_limit_w
        .unwrap_or(140.0)
        .max(1.0);
    let gpu_power_points = graph_points(&history, |point| point.gpu_power_w, power_ceiling);
    let gpu_power_limit_points =
        graph_points(&history, |point| point.gpu_power_limit_w, power_ceiling);
    let memory_clock_points = graph_points(&history, |point| point.gpu_memory_clock_mhz, 10_000.0);
    let history_seconds = history.len.max(1);
    let throttle = clock_event_label(telemetry.gpu_clock_event_reasons, language);
    let telemetry_error = telemetry.gpu_error.as_deref();
    let throttle_class = if telemetry.gpu_sleeping {
        "throttle-state"
    } else if telemetry_error.is_some() {
        "throttle-state telemetry-error"
    } else if has_real_throttle(telemetry.gpu_clock_event_reasons) {
        "throttle-state active"
    } else {
        "throttle-state"
    };
    let throttle_label = if telemetry.gpu_sleeping || telemetry_error.is_some() {
        "NVIDIA"
    } else {
        text(language, MessageId::AppAdvancedPanel001)
    };
    let throttle_summary = if telemetry.gpu_sleeping {
        text(language, MessageId::CommonSleeping).to_string()
    } else if telemetry_error.is_some() {
        text(language, MessageId::AppAdvancedPanel002).to_string()
    } else {
        throttle
    };
    let throttle_title = if telemetry.gpu_sleeping {
        "RTD3"
    } else {
        telemetry_error.unwrap_or("")
    };
    let gpu_workload = if telemetry.gpu_sleeping {
        text(language, MessageId::CommonSleeping).to_string()
    } else {
        percent(telemetry.gpu_load_percent)
    };
    let gpu_workload_detail = if telemetry.gpu_sleeping {
        format!("{} · RTD3", temperature(telemetry.gpu_temperature_c))
    } else {
        format!(
            "{} · {}",
            temperature(telemetry.gpu_temperature_c),
            optional_text(telemetry.gpu_pstate.as_deref())
        )
    };
    let cooling_detail = if telemetry.additional_fans.is_empty() {
        telemetry.gpu_aux_fan_rpm.map_or_else(
            || "CPU / GPU".to_string(),
            |rpm| format!("CPU / GPU · F3 {rpm} RPM"),
        )
    } else {
        let additional = telemetry
            .additional_fans
            .iter()
            .map(|(label, rpm)| format!("{label} {rpm}"))
            .collect::<Vec<_>>()
            .join(" · ");
        match telemetry.gpu_aux_fan_rpm {
            Some(rpm) => format!("F3 {rpm} · {additional}"),
            None => additional,
        }
    };

    rsx! {
        aside { class: "advanced-panel", "aria-label": text(language, MessageId::AppAdvancedPanel003),
            div { class: "advanced-heading",
                div { class: "advanced-tabs", role: "tablist",
                    button {
                        class: if tab() == AdvancedTab::Metrics { "active" } else { "" },
                        role: "tab",
                        "aria-selected": tab() == AdvancedTab::Metrics,
                        onclick: move |_| tab.set(AdvancedTab::Metrics),
                        {text(language, MessageId::AppAdvancedPanel004)}
                    }
                    button {
                        class: if tab() == AdvancedTab::Hardware { "active" } else { "" },
                        role: "tab",
                        "aria-selected": tab() == AdvancedTab::Hardware,
                        onclick: move |_| tab.set(AdvancedTab::Hardware),
                        {text(language, MessageId::AppAdvancedPanel005)}
                    }
                    button {
                        class: if tab() == AdvancedTab::Platform { "active" } else { "" },
                        role: "tab",
                        "aria-selected": tab() == AdvancedTab::Platform,
                        onclick: move |_| tab.set(AdvancedTab::Platform),
                        {text(language, MessageId::AppAdvancedPanel006)}
                    }
                }
            }

            if tab() == AdvancedTab::Metrics {
                div { class: "advanced-content metrics-content",
                div { class: "advanced-kpis",
                AdvancedMetric {
                    label: text(language, MessageId::AppAdvancedPanel007),
                    value: percent(telemetry.cpu_load_percent),
                    detail: temperature(telemetry.cpu_temperature_c),
                }
                AdvancedMetric {
                    label: "RAM",
                    value: memory_pair(telemetry.memory_used_mib, telemetry.memory_total_mib),
                    detail: percent(ratio_percent(telemetry.memory_used_mib, telemetry.memory_total_mib)),
                }
                AdvancedMetric {
                    label: text(language, MessageId::AppAdvancedPanel008),
                    value: gpu_workload,
                    detail: gpu_workload_detail,
                }
                AdvancedMetric {
                    label: "VRAM",
                    value: memory_pair(telemetry.gpu_memory_used_mib, telemetry.gpu_memory_total_mib),
                    detail: percent(ratio_percent(telemetry.gpu_memory_used_mib, telemetry.gpu_memory_total_mib)),
                }
                AdvancedMetric {
                    label: "GFX / SM",
                    value: frequency(telemetry.gpu_graphics_clock_mhz),
                    detail: gpu_offset_detail("VF/GPC", telemetry.gpu_core_offset_mhz),
                }
                AdvancedMetric {
                    label: text(language, MessageId::CommonVramClock),
                    value: frequency(telemetry.gpu_memory_clock_mhz),
                    detail: gpu_offset_detail("VF MEM", telemetry.gpu_memory_offset_mhz),
                }
                AdvancedMetric {
                    label: text(language, MessageId::AppAdvancedPanel009),
                    value: power(telemetry.gpu_power_w),
                    detail: format!("LIMIT {}", power(telemetry.gpu_enforced_power_limit_w)),
                }
                AdvancedMetric {
                    label: text(language, MessageId::AppAdvancedPanel010),
                    value: format!("{}/{} RPM", optional_u32(telemetry.cpu_fan_rpm), optional_u32(telemetry.gpu_fan_rpm)),
                    detail: cooling_detail,
                }
                }

                div { class: "advanced-charts",
                DualHistoryChart {
                    language,
                    title: text(language, MessageId::AppAdvancedPanel011),
                    primary_label: "CPU",
                    primary_value: percent(telemetry.cpu_load_percent),
                    primary_points: cpu_load_points,
                    secondary_label: "RAM",
                    secondary_value: percent(ratio_percent(telemetry.memory_used_mib, telemetry.memory_total_mib)),
                    secondary_points: ram_points,
                    y_min: "0 %".to_string(),
                    y_max: "100 %".to_string(),
                    history_seconds,
                }
                DualHistoryChart {
                    language,
                    title: "GPU / VRAM",
                    primary_label: "GPU",
                    primary_value: percent(telemetry.gpu_load_percent),
                    primary_points: gpu_load_points,
                    secondary_label: "VRAM",
                    secondary_value: percent(ratio_percent(telemetry.gpu_memory_used_mib, telemetry.gpu_memory_total_mib)),
                    secondary_points: vram_points,
                    y_min: "0 %".to_string(),
                    y_max: "100 %".to_string(),
                    history_seconds,
                }
                DualHistoryChart {
                    language,
                    title: text(language, MessageId::AppAdvancedPanel012),
                    primary_label: "CPU",
                    primary_value: temperature(telemetry.cpu_temperature_c),
                    primary_points: cpu_temperature_points,
                    secondary_label: "GPU",
                    secondary_value: temperature(telemetry.gpu_temperature_c),
                    secondary_points: gpu_temperature_points,
                    y_min: "0 °C".to_string(),
                    y_max: "110 °C".to_string(),
                    history_seconds,
                }
                DualHistoryChart {
                    language,
                    title: text(language, MessageId::AppAdvancedPanel013),
                    primary_label: text(language, MessageId::AppAdvancedPanel014),
                    primary_value: power(telemetry.gpu_power_w),
                    primary_points: gpu_power_points,
                    secondary_label: text(language, MessageId::AppAdvancedPanel015),
                    secondary_value: power(telemetry.gpu_enforced_power_limit_w),
                    secondary_points: gpu_power_limit_points,
                    y_min: "0 W".to_string(),
                    y_max: format!("{power_ceiling:.0} W"),
                    history_seconds,
                }
                DualHistoryChart {
                    language,
                    title: text(language, MessageId::AppAdvancedPanel016),
                    primary_label: "GFX / 3 GHz",
                    primary_value: frequency(telemetry.gpu_graphics_clock_mhz),
                    primary_points: graph_points(&history, |point| point.gpu_graphics_clock_mhz, 3_000.0),
                    secondary_label: "VRAM / 10 GHz",
                    secondary_value: frequency(telemetry.gpu_memory_clock_mhz),
                    secondary_points: memory_clock_points,
                    y_min: "0 GHz".to_string(),
                    y_max: "3 / 10 GHz".to_string(),
                    history_seconds,
                }
                }

                div { class: throttle_class, title: "{throttle_title}",
                span { "{throttle_label}" }
                strong { "{throttle_summary}" }
                if telemetry_error.is_none() && !telemetry.gpu_sleeping {
                    code { "0x{telemetry.gpu_clock_event_reasons.unwrap_or_default():016x}" }
                }
                }
                }
            } else if tab() == AdvancedTab::Hardware {
                HardwarePanel { language, info: hardware }
            } else {
                PlatformAdvanced {
                    key: "platform-{platform_revision}",
                    state: platform,
                    battery_percent: telemetry.battery_percent,
                    battery_status: telemetry.battery_status,
                    ac_online: telemetry.ac_online,
                    usb_power_online: telemetry.usb_power_online,
                    busy: platform_busy,
                    error: platform_error,
                    last_nonzero_logo_brightness: rear_logo_last_nonzero_brightness,
                    language,
                    on_action: move |action| on_platform.call(action),
                }
            }
        }
    }
}

#[component]
fn HardwarePanel(language: Language, info: HardwareInfo) -> Element {
    let unknown = text(language, MessageId::AppHardwarePanel001);
    let cpu_model = info.cpu.model.as_deref().unwrap_or(unknown).to_string();
    let gpu_model = info.gpu.model.as_deref().unwrap_or(unknown).to_string();
    let memory_type = info
        .memory
        .memory_type
        .as_deref()
        .unwrap_or(unknown)
        .to_string();
    let core_mix = match (info.cpu.performance_cores, info.cpu.efficiency_cores) {
        (Some(performance), Some(efficiency)) => format!("{performance} P / {efficiency} E"),
        _ => "—".to_string(),
    };

    rsx! {
        section { class: "advanced-content hardware-page",
            article { class: "hardware-card cpu-hardware",
                div { class: "hardware-card-heading",
                    div {
                        span { class: "hardware-kind", "CPU" }
                        h3 { {text(language, MessageId::AppHardwarePanel002)} }
                    }
                    span { class: "read-only-badge", {text(language, MessageId::CommonReadOnly)} }
                }
                div { class: "hardware-model", title: "{cpu_model}", "{cpu_model}" }
                div { class: "hardware-facts",
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel003),
                        value: optional_hardware_number(info.cpu.physical_cores),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel004),
                        value: optional_hardware_number(info.cpu.logical_processors),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel005),
                        value: core_mix,
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel006),
                        value: info.cpu.architecture.as_deref().unwrap_or("—").to_string(),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel007),
                        value: optional_hardware_number(info.cpu.family),
                    }
                    HardwareFact {
                        label: text(language, MessageId::HardwareL3Cache),
                        value: hardware_cache(info.cpu.l3_cache_kib),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel008),
                        value: hardware_frequency(info.cpu.current_frequency_mhz),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel009),
                        value: hardware_frequency(info.cpu.maximum_frequency_mhz),
                    }
                }
            }

            article { class: "hardware-card gpu-hardware",
                div { class: "hardware-card-heading",
                    div {
                        span { class: "hardware-kind", "GPU" }
                        h3 { {text(language, MessageId::AppHardwarePanel010)} }
                    }
                    span { class: "read-only-badge", {text(language, MessageId::CommonReadOnly)} }
                }
                div { class: "hardware-model", title: "{gpu_model}", "{gpu_model}" }
                div { class: "hardware-facts",
                    HardwareFact {
                        label: "VRAM",
                        value: hardware_capacity(info.gpu.vram_total_mib),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel011),
                        value: info.gpu.driver_version.as_deref().unwrap_or("—").to_string(),
                    }
                    HardwareFact {
                        label: "PCI",
                        value: info.gpu.pci_bus_id.as_deref().unwrap_or("—").to_string(),
                    }
                    HardwareFact {
                        label: "SM / CUDA",
                        value: gpu_compute_units(
                            info.gpu.streaming_multiprocessors,
                            info.gpu.cuda_cores,
                        ),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel012),
                        value: hardware_frequency(info.gpu.current_graphics_clock_mhz),
                    }
                    HardwareFact {
                        label: text(language, MessageId::HardwareGpuMaximum),
                        value: hardware_frequency(info.gpu.maximum_graphics_clock_mhz),
                    }
                    HardwareFact {
                        label: text(language, MessageId::CommonVramClock),
                        value: hardware_frequency(info.gpu.current_memory_clock_mhz),
                    }
                    HardwareFact {
                        label: text(language, MessageId::HardwareVramMaximum),
                        value: hardware_frequency(info.gpu.maximum_memory_clock_mhz),
                    }
                }
            }

            article { class: "hardware-card memory-hardware",
                div { class: "hardware-card-heading",
                    div {
                        span { class: "hardware-kind", "RAM" }
                        h3 { {text(language, MessageId::AppHardwarePanel013)} }
                    }
                    span { class: "read-only-badge", {text(language, MessageId::CommonReadOnly)} }
                }
                div { class: "hardware-facts memory-facts",
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel014),
                        value: hardware_capacity(info.memory.total_mib),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel015),
                        value: memory_type,
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel016),
                        value: info.memory.speed_mt_s.map(|value| format!("{value} MT/s")).unwrap_or_else(|| "—".to_string()),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel017),
                        value: optional_hardware_number(info.memory.channels),
                    }
                    HardwareFact {
                        label: text(language, MessageId::AppHardwarePanel018),
                        value: optional_hardware_number(info.memory.modules),
                    }
                }
            }
            p { class: "hardware-note",
                {text(language, MessageId::AppHardwarePanel019)}
            }
        }
    }
}

#[component]
fn HardwareFact(label: &'static str, value: String) -> Element {
    rsx! {
        div { class: "hardware-fact",
            span { "{label}" }
            strong { "{value}" }
        }
    }
}

fn optional_hardware_number(value: Option<u32>) -> String {
    value.map_or_else(|| "—".to_string(), |value| value.to_string())
}

fn hardware_frequency(value: Option<u32>) -> String {
    value.map_or_else(|| "—".to_string(), |value| format!("{value} MHz"))
}

fn gpu_compute_units(streaming_multiprocessors: Option<u32>, cuda_cores: Option<u32>) -> String {
    match (streaming_multiprocessors, cuda_cores) {
        (Some(sm), Some(cuda)) => format!("{sm} / {cuda}"),
        (Some(sm), None) => format!("{sm} SM"),
        (None, Some(cuda)) => format!("{cuda} CUDA"),
        (None, None) => "—".to_string(),
    }
}

fn hardware_cache(value: Option<u64>) -> String {
    value.map_or_else(
        || "—".to_string(),
        |value| {
            if value >= 1_024 {
                format!("{:.1} MiB", value as f64 / 1_024.0)
            } else {
                format!("{value} KiB")
            }
        },
    )
}

fn hardware_capacity(value: Option<u64>) -> String {
    value.map_or_else(
        || "—".to_string(),
        |value| {
            if value >= 1_024 {
                format!("{:.1} GiB", value as f64 / 1_024.0)
            } else {
                format!("{value} MiB")
            }
        },
    )
}

fn rear_logo_state(enabled: bool, brightness: u8, color: u32) -> RearLogoState {
    RearLogoState {
        enabled,
        // PHN16-72 keeps the physical rear logo lit when only its logical
        // enable flag is cleared. Brightness zero is the effective hardware
        // off state; keep that detail behind the typed UI request.
        brightness: if enabled { brightness } else { 0 },
        color: [(color >> 16) as u8, (color >> 8) as u8, color as u8],
    }
}

#[component]
fn PlatformAdvanced(
    state: Option<PlatformState>,
    battery_percent: Option<u8>,
    battery_status: Option<BatteryStatus>,
    ac_online: Option<bool>,
    usb_power_online: Option<bool>,
    busy: bool,
    error: Option<String>,
    last_nonzero_logo_brightness: u8,
    language: Language,
    on_action: EventHandler<PlatformAction>,
) -> Element {
    let unavailable_message = error
        .clone()
        .unwrap_or_else(|| text(language, MessageId::AppPlatformAdvanced001).to_string());
    let Some(state) = state else {
        return rsx! {
            section { class: "advanced-content platform-page empty",
                h3 { {text(language, MessageId::AppPlatformAdvanced002)} }
                p { "{unavailable_message}" }
                button {
                    class: "apply-button",
                    disabled: busy,
                    onclick: move |_| on_action.call(PlatformAction::Refresh),
                    {text(language, MessageId::AppPlatformAdvanced003)}
                }
            }
        };
    };

    let initial_logo = state.rear_logo.unwrap_or(RearLogoState {
        enabled: false,
        brightness: 100,
        color: [0x5b, 0x6e, 0xff],
    });
    let logo_enabled = use_signal(move || initial_logo.enabled);
    let initial_logo_brightness = if initial_logo.brightness == 0 {
        last_nonzero_logo_brightness.clamp(1, 100)
    } else {
        initial_logo.brightness
    };
    let mut logo_brightness = use_signal(move || initial_logo_brightness);
    let initial_logo_color = u32::from_be_bytes([
        0,
        initial_logo.color[0],
        initial_logo.color[1],
        initial_logo.color[2],
    ]);
    let mut logo_color = use_signal(move || initial_logo_color);
    let mut calibration_confirmation = use_signal(|| false);
    let calibration_supported = state.battery_calibration.is_some();
    let calibration_active = state.battery_calibration.unwrap_or(false);
    let calibration_read_failed = state.read_error_mask & READ_ERROR_BATTERY_CALIBRATION != 0;
    let calibration_button_text = match state.battery_calibration {
        Some(true) => text(language, MessageId::AppPlatformAdvanced004),
        Some(false) => text(language, MessageId::AppPlatformAdvanced005),
        None if calibration_read_failed => text(language, MessageId::CommonReadError),
        None => text(language, MessageId::CommonUnsupported),
    };
    let battery_live = battery_live_status(battery_status, battery_percent, language);
    let calibration_detail = if calibration_active {
        format!(
            "{} · {battery_live}",
            text(language, MessageId::AppPlatformAdvanced006)
        )
    } else {
        text(language, MessageId::AppPlatformAdvanced007).to_string()
    };
    let usb_only = usb_power_online == Some(true) && ac_online != Some(true);
    let calibration_start_allowed = ac_online == Some(true) && !usb_only;
    let (power_state_class, power_state_text) = if ac_online == Some(true) {
        (
            "calibration-power-state ready",
            text(language, MessageId::AppPlatformAdvanced008),
        )
    } else if usb_only {
        (
            "calibration-power-state warning",
            text(language, MessageId::AppPlatformAdvanced009),
        )
    } else if ac_online == Some(false) {
        (
            "calibration-power-state warning",
            text(language, MessageId::AppPlatformAdvanced010),
        )
    } else {
        (
            "calibration-power-state warning",
            text(language, MessageId::AppPlatformAdvanced011),
        )
    };
    let readback_text = if busy {
        text(language, MessageId::AppPlatformAdvanced012)
    } else if error.is_some() || state.read_error_mask != 0 {
        text(language, MessageId::CommonReadError)
    } else {
        text(language, MessageId::AppPlatformAdvanced013)
    };
    let readback_class = if error.is_some() || state.read_error_mask != 0 {
        "platform-readback warning"
    } else {
        "platform-readback"
    };
    let readback_title = error.as_deref().unwrap_or("");

    rsx! {
        section { class: "advanced-content platform-page",
            div { class: "device-bento",
                SettingToggle {
                    class_name: "device-battery-limit",
                    label: text(language, MessageId::AppPlatformAdvanced014),
                    detail: text(language, MessageId::AppPlatformAdvanced015),
                    value: state.battery_limit,
                    read_failed: state.read_error_mask & READ_ERROR_BATTERY_LIMIT != 0,
                    disabled: busy,
                    language,
                    on_change: move |enabled| on_action.call(PlatformAction::BatteryLimit(enabled)),
                }
                div { class: "usb-charging-control device-usb-charging",
                    div { class: "setting-copy",
                        strong { {text(language, MessageId::AppPlatformAdvanced016)} }
                        span { {text(language, MessageId::AppPlatformAdvanced017)} }
                    }
                    div { class: "usb-thresholds",
                        for mode in UsbCharging::ALL {
                            button {
                                class: if state.usb_charging == Some(mode) { "active" } else { "" },
                                disabled: busy || state.usb_charging.is_none(),
                                onclick: move |_| on_action.call(PlatformAction::UsbCharging(mode)),
                                "{usb_charging_label(mode, language)}"
                            }
                        }
                    }
                }
                div { class: "setting-toggle device-calibration",
                    div { class: "setting-copy",
                        strong { {text(language, MessageId::AppPlatformAdvanced018)} }
                        span { "{calibration_detail}" }
                    }
                    button {
                        class: if calibration_active { "toggle-button active" } else { "toggle-button" },
                        disabled: busy || !calibration_supported,
                        onclick: move |_| {
                            if calibration_active {
                                on_action.call(PlatformAction::BatteryCalibration(false));
                            } else {
                                calibration_confirmation.set(true);
                            }
                        },
                        "{calibration_button_text}"
                    }
                }
                SettingToggle {
                    class_name: "device-boot-sound",
                    label: text(language, MessageId::AppPlatformAdvanced019),
                    detail: text(language, MessageId::AppPlatformAdvanced020),
                    value: state.boot_sound,
                    read_failed: state.read_error_mask & READ_ERROR_BOOT_SOUND != 0,
                    disabled: busy,
                    language,
                    on_change: move |enabled| on_action.call(PlatformAction::BootSound(enabled)),
                }
                SettingToggle {
                    class_name: "device-lcd-override",
                    label: text(language, MessageId::PlatformLcdOverride),
                    detail: text(language, MessageId::AppPlatformAdvanced021),
                    value: state.lcd_override,
                    read_failed: state.read_error_mask & READ_ERROR_LCD_OVERRIDE != 0,
                    disabled: busy,
                    language,
                    on_change: move |enabled| on_action.call(PlatformAction::LcdOverride(enabled)),
                }
                SettingToggle {
                    class_name: "device-keyboard-timeout",
                    label: text(language, MessageId::AppPlatformAdvanced022),
                    detail: text(language, MessageId::AppPlatformAdvanced023),
                    value: state.keyboard_timeout,
                    read_failed: state.read_error_mask & READ_ERROR_KEYBOARD_TIMEOUT != 0,
                    disabled: busy,
                    language,
                    on_change: move |enabled| on_action.call(PlatformAction::KeyboardTimeout(enabled)),
                }
                article { class: "rear-logo-card",
                div { class: "rear-logo-heading",
                    div { class: "setting-copy",
                        strong { {text(language, MessageId::AppPlatformAdvanced024)} }
                        span { {text(language, MessageId::AppPlatformAdvanced025)} }
                    }
                    div { class: "binary-buttons",
                        button {
                            class: if logo_enabled() { "active" } else { "" },
                            r#type: "button",
                            disabled: busy || state.rear_logo.is_none(),
                            onclick: move |_| {
                                on_action.call(PlatformAction::RearLogo(rear_logo_state(
                                    true,
                                    logo_brightness(),
                                    logo_color(),
                                )));
                            },
                            {text(language, MessageId::CommonOn)}
                        }
                        button {
                            class: if !logo_enabled() { "active" } else { "" },
                            r#type: "button",
                            disabled: busy || state.rear_logo.is_none(),
                            onclick: move |_| {
                                on_action.call(PlatformAction::RearLogo(rear_logo_state(
                                    false,
                                    logo_brightness(),
                                    logo_color(),
                                )));
                            },
                            {text(language, MessageId::CommonOff)}
                        }
                    }
                }
                div { class: "rear-logo-editor",
                    label { class: "logo-color",
                        span { {text(language, MessageId::AppPlatformAdvanced026)} }
                        input {
                            r#type: "color",
                            value: "#{logo_color():06x}",
                            disabled: busy || state.rear_logo.is_none(),
                            oninput: move |event| {
                                let value = event.value();
                                if let Some(value) = value.strip_prefix('#')
                                    && let Ok(value) = u32::from_str_radix(value, 16)
                                {
                                    logo_color.set(value);
                                }
                            },
                            onchange: move |event| {
                                let value = event.value();
                                if let Some(value) = parse_color_value(&value) {
                                    logo_color.set(value);
                                    if logo_enabled() && !busy {
                                        on_action.call(PlatformAction::RearLogo(rear_logo_state(
                                            true,
                                            logo_brightness(),
                                            value,
                                        )));
                                    }
                                }
                            },
                        }
                    }
                    label { class: "logo-brightness",
                        span { {text(language, MessageId::CommonBrightness)} }
                        input {
                            r#type: "range", min: "1", max: "100", step: "1",
                            value: "{logo_brightness}",
                            style: "--value:{logo_brightness}%",
                            disabled: busy || state.rear_logo.is_none(),
                            oninput: move |event| {
                                if let Ok(value) = event.value().parse::<u8>() {
                                    logo_brightness.set(value.min(100));
                                }
                            },
                        }
                        strong { "{logo_brightness}%" }
                    }
                    button {
                        class: "apply-button",
                        disabled: busy || state.rear_logo.is_none() || !logo_enabled(),
                        onclick: move |_| {
                            on_action.call(PlatformAction::RearLogo(rear_logo_state(
                                logo_enabled(),
                                logo_brightness(),
                                logo_color(),
                            )));
                        },
                        {text(language, MessageId::CommonApply)}
                    }
                }
                }

                div { class: readback_class, title: "{readback_title}",
                    span { {text(language, MessageId::PlatformFirmware)} }
                    strong { "{readback_text}" }
                    button {
                        disabled: busy,
                        onclick: move |_| on_action.call(PlatformAction::Refresh),
                        {text(language, MessageId::AppPlatformAdvanced027)}
                    }
                }
            }
            if calibration_confirmation() {
                div { class: "calibration-modal-backdrop",
                    div {
                        class: "calibration-modal",
                        role: "dialog",
                        "aria-modal": "true",
                        "aria-labelledby": "calibration-modal-title",
                        "aria-describedby": "calibration-modal-description",
                        h3 { id: "calibration-modal-title",
                            {text(language, MessageId::AppPlatformAdvanced028)}
                        }
                        p { id: "calibration-modal-description",
                            {text(language, MessageId::AppPlatformAdvanced029)}
                        }
                        div { class: power_state_class,
                            strong { "{power_state_text}" }
                            span { "{battery_live}" }
                        }
                        p { class: "calibration-modal-note",
                            {text(language, MessageId::AppPlatformAdvanced030)}
                        }
                        if state.battery_limit == Some(true) {
                            p { class: "calibration-modal-note",
                                {text(language, MessageId::AppPlatformAdvanced031)}
                            }
                        }
                        div { class: "calibration-modal-actions",
                            button {
                                class: "modal-cancel",
                                disabled: busy,
                                onclick: move |_| calibration_confirmation.set(false),
                                {text(language, MessageId::AppPlatformAdvanced032)}
                            }
                            button {
                                class: "apply-button",
                                disabled: busy || !calibration_start_allowed,
                                onclick: move |_| {
                                    calibration_confirmation.set(false);
                                    on_action.call(PlatformAction::BatteryCalibration(true));
                                },
                                {text(language, MessageId::AppPlatformAdvanced033)}
                            }
                        }
                    }
                }
            }
        }
    }
}

fn battery_live_status(
    status: Option<BatteryStatus>,
    percent: Option<u8>,
    language: Language,
) -> String {
    let state = match status {
        Some(BatteryStatus::Charging) => text(language, MessageId::AppBatteryLiveStatus001),
        Some(BatteryStatus::Discharging) => text(language, MessageId::AppBatteryLiveStatus002),
        Some(BatteryStatus::Full) => text(language, MessageId::AppBatteryLiveStatus003),
        Some(BatteryStatus::NotCharging) => text(language, MessageId::AppBatteryLiveStatus004),
        Some(BatteryStatus::Unknown) | None => text(language, MessageId::AppBatteryLiveStatus005),
    };
    percent.map_or_else(
        || state.to_string(),
        |percent| format!("{state} · {percent} %"),
    )
}

fn usb_charging_label(mode: UsbCharging, language: Language) -> &'static str {
    match mode {
        UsbCharging::Disabled => text(language, MessageId::CommonOff),
        UsbCharging::StopAt10Percent => "10 %",
        UsbCharging::StopAt20Percent => "20 %",
        UsbCharging::StopAt30Percent => "30 %",
    }
}

#[component]
fn AdvancedMetric(label: &'static str, value: String, detail: String) -> Element {
    rsx! {
        article { class: "advanced-metric",
            span { "{label}" }
            strong { "{value}" }
            small { title: "{detail}", "{detail}" }
        }
    }
}

#[component]
fn DualHistoryChart(
    language: Language,
    title: &'static str,
    primary_label: &'static str,
    primary_value: String,
    primary_points: String,
    secondary_label: &'static str,
    secondary_value: String,
    secondary_points: String,
    y_min: String,
    y_max: String,
    history_seconds: usize,
) -> Element {
    let history_start = format!("−{history_seconds} s");
    let history_end = text(language, MessageId::AppDualHistoryChart001);
    let chart_description =
        history_chart_description(language, title, history_seconds, &y_min, &y_max);
    rsx! {
        article { class: "history-chart",
            div { class: "chart-heading",
                h3 { "{title}" }
                div { class: "chart-legends",
                    span {
                        class: "chart-legend primary",
                        title: "{primary_label}",
                        "aria-label": "{primary_label}: {primary_value}",
                        strong { "{primary_value}" }
                    }
                    span {
                        class: "chart-legend secondary",
                        title: "{secondary_label}",
                        "aria-label": "{secondary_label}: {secondary_value}",
                        strong { "{secondary_value}" }
                    }
                }
            }
            div { class: "chart-plot",
                svg {
                    class: "spark-chart",
                    view_box: "0 0 100 46",
                    preserve_aspect_ratio: "none",
                    role: "img",
                    "aria-label": "{chart_description}",
                    line { class: "chart-grid", x1: "0", y1: "21", x2: "100", y2: "21" }
                    line { class: "chart-grid", x1: "0", y1: "38", x2: "100", y2: "38" }
                    polyline { class: "chart-line primary", points: "{primary_points}" }
                    polyline { class: "chart-line secondary", points: "{secondary_points}" }
                }
                div { class: "chart-scale", "aria-hidden": "true",
                    span { class: "chart-scale-y-max", "{y_max}" }
                    span { class: "chart-scale-y-min", "{y_min}" }
                    span { class: "chart-scale-x-start", "{history_start}" }
                    span { class: "chart-scale-x-end", "{history_end}" }
                }
            }
        }
    }
}

#[component]
fn MetricPill(label: &'static str, value: String, level: &'static str) -> Element {
    rsx! {
        div { class: "metric-pill {level}",
            span { "{label}" }
            strong { "{value}" }
        }
    }
}

#[component]
fn FanGauge(
    kind: &'static str,
    rpm: Option<u32>,
    max_rpm: u32,
    temperature_c: Option<f32>,
    accent: &'static str,
    secondary_rpm: Option<u32>,
) -> Element {
    let ratio = rpm.unwrap_or_default() as f32 / max_rpm.max(1) as f32;
    let ratio = ratio.clamp(0.0, 1.0);
    let sweep = ratio * 270.0;
    // CSS conic gradients measure from the top while transforms rotate from
    // the positive x-axis. Subtract the missing quarter turn so the needle
    // follows the visible arc from its 225-degree starting point.
    let needle = -225.0 + sweep;
    let style = format!("--sweep:{sweep:.2}deg;--needle:{needle:.2}deg");
    let secondary_needle = secondary_rpm.map(|rpm| {
        let ratio = (rpm as f32 / max_rpm.max(1) as f32).clamp(0.0, 1.0);
        -225.0 + ratio * 270.0
    });
    let rpm_value = rpm
        .map(|value| value.to_string())
        .unwrap_or_else(|| "--".into());

    rsx! {
        article { class: "gauge-card {accent}",
            div { class: "gauge-title",
                span { class: "gauge-kind", "{kind}" }
                span { class: "gauge-temp", "{temperature(temperature_c)}" }
            }
            div { class: "gauge", style: "{style}",
                div { class: "gauge-scale" }
                div { class: "gauge-needle" }
                if let Some(secondary_needle) = secondary_needle {
                    div {
                        class: "gauge-needle",
                        style: "--needle:{secondary_needle:.2}deg;opacity:.62;background:linear-gradient(90deg,rgba(255,255,255,.04),#ffc86b);box-shadow:0 0 1.15cqh #ffc86b",
                    }
                }
                div { class: "gauge-hub" }
                span { class: "scale-min", "0" }
                span { class: "scale-max", "{compact_rpm(max_rpm)}" }
                div { class: "gauge-readout",
                    strong { "{rpm_value}" }
                    if let Some(secondary_rpm) = secondary_rpm {
                        span { "RPM · F3 {secondary_rpm}" }
                    } else {
                        span { "RPM" }
                    }
                }
            }
        }
    }
}

#[component]
fn FanSlider(
    label: &'static str,
    value: u8,
    disabled: bool,
    on_change: EventHandler<u8>,
) -> Element {
    let bounded = value.clamp(20, 100);
    let fill = u16::from(bounded - 20) * 100 / 80;
    rsx! {
        label { class: "fan-slider",
            span { "{label}" }
            input {
                r#type: "range",
                min: "20",
                max: "100",
                step: "1",
                value: "{value}",
                disabled,
                style: "--value:{fill}%",
                oninput: move |event| {
                    if let Ok(value) = event.value().parse::<u8>() {
                        on_change.call(value.clamp(20, 100));
                    }
                },
            }
            strong { "{value}%" }
        }
    }
}

#[component]
fn ColorInput(
    language: Language,
    label: usize,
    value: u32,
    on_change: EventHandler<u32>,
    on_commit: EventHandler<u32>,
) -> Element {
    let title = color_zone_title(language, label);
    rsx! {
        label { class: "color-input", title: "{title}",
            input {
                r#type: "color",
                value: "#{value:06x}",
                oninput: move |event| {
                    if let Some(value) = parse_color_value(&event.value()) {
                        on_change.call(value);
                    }
                },
                onchange: move |event| {
                    if let Some(value) = parse_color_value(&event.value()) {
                        on_commit.call(value);
                    }
                },
            }
            span { "{label}" }
        }
    }
}

fn parse_color_value(value: &str) -> Option<u32> {
    let value = value.strip_prefix('#')?;
    (value.len() == 6)
        .then(|| u32::from_str_radix(value, 16).ok())
        .flatten()
}

fn history_chart_description(
    language: Language,
    title: &str,
    history_seconds: usize,
    y_min: &str,
    y_max: &str,
) -> String {
    match language {
        Language::Czech => {
            format!("Historie {title}, {history_seconds} sekund, osa {y_min} až {y_max}")
        }
        Language::English => {
            format!("History of {title}, {history_seconds} seconds, axis {y_min} to {y_max}")
        }
        Language::SimplifiedChinese => {
            format!("{title}历史记录，{history_seconds} 秒，纵轴从 {y_min} 到 {y_max}")
        }
    }
}

fn color_zone_title(language: Language, index: usize) -> String {
    match language {
        Language::Czech => format!("Zóna {index}"),
        Language::English => format!("Zone {index}"),
        Language::SimplifiedChinese => format!("分区 {index}"),
    }
}

fn unknown_clock_reason(language: Language, bits: u64) -> String {
    match language {
        Language::Czech => format!("Neznámý důvod 0x{bits:016x}"),
        Language::English => format!("Unknown reason 0x{bits:016x}"),
        Language::SimplifiedChinese => format!("未知原因 0x{bits:016x}"),
    }
}

fn parse_lighting_state(response: &str) -> Result<KeyboardLightingState, String> {
    let mut state = KeyboardLightingState {
        available: true,
        ..KeyboardLightingState::default()
    };
    let mut seen = 0_u8;
    for field in response.split_ascii_whitespace() {
        let (name, value) = field
            .split_once('=')
            .ok_or_else(|| "invalid RGB state response".to_string())?;
        match name {
            "power" => {
                state.powered = match value {
                    "on" => true,
                    "off" => false,
                    _ => return Err("invalid RGB power state".to_string()),
                };
                seen |= 1 << 6;
            }
            "mode" => {
                state.mode = parse_response_u8(value, 7, name)?;
                seen |= 1 << 0;
            }
            "speed" => {
                state.speed = parse_response_u8(value, 9, name)?;
                seen |= 1 << 1;
            }
            "brightness" => {
                state.brightness = parse_response_u8(value, 100, name)?;
                seen |= 1 << 2;
            }
            "direction" => {
                state.direction = parse_response_u8(value, 2, name)?;
                seen |= 1 << 3;
            }
            "color" => {
                state.color = parse_response_color(value)?;
                seen |= 1 << 4;
            }
            "zones" => {
                let colors = value.split(',').collect::<Vec<_>>();
                if colors.len() != 4 {
                    return Err("RGB response must contain four zones".to_string());
                }
                for (target, color) in state.zones.iter_mut().zip(colors) {
                    *target = parse_response_color(color)?;
                }
                seen |= 1 << 5;
            }
            _ => return Err("unknown RGB state field".to_string()),
        }
    }
    if seen != 0b111_1111 {
        return Err("incomplete RGB state response".to_string());
    }
    Ok(state)
}

fn parse_response_u8(value: &str, maximum: u8, label: &str) -> Result<u8, String> {
    let value = value
        .parse::<u8>()
        .map_err(|_| format!("invalid RGB {label}"))?;
    if value > maximum {
        return Err(format!("RGB {label} out of range"));
    }
    Ok(value)
}

fn parse_response_color(value: &str) -> Result<u32, String> {
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid RGB color readback".to_string());
    }
    u32::from_str_radix(value, 16).map_err(|_| "invalid RGB color readback".to_string())
}

fn temperature(value: Option<f32>) -> String {
    value
        .map(|v| format!("{v:.0} C"))
        .unwrap_or_else(|| "-- C".into())
}

fn percent(value: Option<f32>) -> String {
    value
        .map(|v| format!("{v:.0}%"))
        .unwrap_or_else(|| "--%".into())
}

fn power(value: Option<f32>) -> String {
    value
        .map(|v| format!("{v:.0} W"))
        .unwrap_or_else(|| "-- W".into())
}

fn offsets(
    core: Option<i32>,
    memory: Option<i32>,
    uniform: Option<bool>,
    language: Language,
) -> String {
    match (core, memory, uniform) {
        (Some(core), Some(memory), Some(true)) => format!("{core:+}/{memory:+} MHz"),
        (_, _, Some(false)) => text(language, MessageId::AppOffsets001).to_string(),
        _ => "--/--".to_string(),
    }
}

fn power_usage_limit(draw: Option<f32>, enforced: Option<f32>) -> String {
    match (draw, enforced) {
        (Some(draw), Some(enforced)) => format!("{draw:.0}/{enforced:.0} W"),
        _ => "--/-- W".to_string(),
    }
}

fn temperature_level(value: Option<f32>) -> &'static str {
    match value {
        Some(value) if value >= 90.0 => "hot",
        Some(value) if value >= 80.0 => "warm",
        _ => "neutral",
    }
}

fn compact_rpm(rpm: u32) -> String {
    if rpm >= 1_000 {
        format!("{:.0}k", rpm as f32 / 1_000.0)
    } else {
        rpm.to_string()
    }
}

fn ratio_percent(used: Option<u64>, total: Option<u64>) -> Option<f32> {
    match (used, total) {
        (Some(used), Some(total)) if total > 0 => {
            Some((used.min(total) as f64 * 100.0 / total as f64) as f32)
        }
        _ => None,
    }
}

fn memory_pair(used: Option<u64>, total: Option<u64>) -> String {
    match (used, total) {
        (Some(used), Some(total)) => format!(
            "{:.1}/{:.1} GiB",
            used as f64 / 1024.0,
            total as f64 / 1024.0
        ),
        _ => "--/-- GiB".to_string(),
    }
}

fn frequency(value: Option<u32>) -> String {
    value
        .map(|value| format!("{value} MHz"))
        .unwrap_or_else(|| "-- MHz".to_string())
}

fn gpu_offset_detail(label: &str, value: Option<i32>) -> String {
    value.map_or_else(
        || format!("{label} -- MHz"),
        |value| format!("{label} {value:+} MHz"),
    )
}

fn optional_u32(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "--".to_string())
}

fn optional_text(value: Option<&str>) -> &str {
    value.unwrap_or("--")
}

fn graph_points(
    history: &TelemetryHistory,
    value: impl Fn(&TelemetryPoint) -> Option<f32>,
    maximum: f32,
) -> String {
    if history.len == 0 || maximum <= 0.0 {
        return String::new();
    }
    let denominator = history.len.saturating_sub(1).max(1) as f32;
    let mut points = String::with_capacity(history.len * 13);
    for index in 0..history.len {
        let Some(sample) = history.get(index) else {
            continue;
        };
        let Some(sample) = value(sample).filter(|value| value.is_finite()) else {
            continue;
        };
        let x = index as f32 * 100.0 / denominator;
        let normalized = (sample / maximum).clamp(0.0, 1.0);
        let y = 38.0 - normalized * 34.0;
        if !points.is_empty() {
            points.push(' ');
        }
        use std::fmt::Write;
        let _ = write!(points, "{x:.2},{y:.2}");
    }
    points
}

fn has_real_throttle(reasons: Option<u64>) -> bool {
    reasons.is_some_and(|bits| bits & !ClockEventReasons::GPU_IDLE != 0)
}

fn clock_event_label(reasons: Option<u64>, language: Language) -> String {
    let Some(bits) = reasons else {
        return text(language, MessageId::CommonUnavailable).to_string();
    };
    let reasons = ClockEventReasons::from_bits(bits);
    if bits == 0 {
        return text(language, MessageId::AppClockEventLabel001).to_string();
    }
    if bits == ClockEventReasons::GPU_IDLE {
        return text(language, MessageId::AppClockEventLabel002).to_string();
    }
    let labels: Vec<&'static str> = [
        (
            ClockEventReasons::GPU_IDLE,
            text(language, MessageId::AppClockEventLabel003),
        ),
        (
            ClockEventReasons::APPLICATION_CLOCKS,
            text(language, MessageId::AppClockEventLabel004),
        ),
        (
            ClockEventReasons::SOFTWARE_POWER_CAP,
            text(language, MessageId::AppClockEventLabel005),
        ),
        (
            ClockEventReasons::HARDWARE_SLOWDOWN,
            text(language, MessageId::AppClockEventLabel006),
        ),
        (
            ClockEventReasons::SYNC_BOOST,
            text(language, MessageId::ClockSyncBoost),
        ),
        (
            ClockEventReasons::SOFTWARE_THERMAL,
            text(language, MessageId::AppClockEventLabel007),
        ),
        (
            ClockEventReasons::HARDWARE_THERMAL,
            text(language, MessageId::AppClockEventLabel008),
        ),
        (
            ClockEventReasons::HARDWARE_POWER_BRAKE,
            text(language, MessageId::AppClockEventLabel009),
        ),
        (
            ClockEventReasons::DISPLAY_CLOCK,
            text(language, MessageId::AppClockEventLabel010),
        ),
    ]
    .into_iter()
    .filter_map(|(bit, label)| reasons.contains(bit).then_some(label))
    .collect();
    if labels.is_empty() {
        unknown_clock_reason(language, bits)
    } else {
        labels.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ADVANCED_DESIGN_WIDTH, APP_CSS_SOURCE, AppState, AspectResizeState, COMPACT_DESIGN_WIDTH,
        ControlAction, ControlOutcome, ControlRequest, ControlResultSlot, ControlUpdate, FanMode,
        HardwareProfile, HealthState, KeyboardLightingState, Language, LightingApplyRequest,
        MAX_LIGHTING_ZONES, MIN_WINDOW_HEIGHT, PROFILE_SYNC_GRACE_SAMPLES, PlatformAction,
        PlatformIssue, PlatformProfile, PlatformReadErrorSet, ResizeObservation, RuntimeState,
        TELEMETRY_HISTORY_CAPACITY, TITLEBAR_DESIGN_HEIGHT, TelemetryHealth, TelemetryHistory,
        TelemetryPoint, TelemetrySlot, TelemetryUpdate, UiDiagnostic, UiErrorKind, UiStatus,
        WORKSPACE_DESIGN_HEIGHT, apply_capability_snapshot, apply_control_update, apply_telemetry,
        aspect_constrained_size, begin_control_request, color_zone_title, empty_platform_state,
        gpu_offset_detail, graph_points, history_chart_description, keyboard_editor_readback,
        lighting_apply_status, lighting_draft_for_device, lighting_mode_visibility,
        lighting_zone_draft, logical_window_size, merge_privileged_memory, parse_color_value,
        parse_lighting_state, physical_size_close, power_usage_limit, preferred_lighting_index,
        rear_logo_state, reconcile_profile_telemetry, render_compact_status,
        render_platform_fields, render_telemetry_status, render_ui_status, setting_toggle_text,
        telemetry_retry_delay, unknown_clock_reason, workspace_aspect_ratio,
    };
    use crate::control::{
        CapabilityLightingBackend, CapabilityLightingTarget, ControlCapabilities,
        ControlFanCapabilities, ControlLightingDevice, ControlLightingMode, ControlLightingModes,
        ControlPlatformCapabilities, ControlProfileCapabilities, ProfileApplyReceipt,
        ProfilePowerReceipt,
    };
    use crate::hardware::{FanChannelState, FanMode as HardwareFanMode, FanRpmChannel, FanState};
    use crate::telemetry::{
        GpuTelemetry, HardwareInfo, MemoryHardwareInfo, PowerSupplyTelemetry, SystemTelemetry,
    };
    use crate::tuning::GpuOffsetState;
    use dioxus_desktop::tao::dpi::PhysicalSize;
    use dioxus_desktop::tao::window::ResizeDirection;
    use std::time::Duration;

    fn css_rule(selector: &str) -> &'static str {
        APP_CSS_SOURCE
            .split(selector)
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap()
    }

    fn production_source() -> &'static str {
        include_str!("app.rs").split("#[cfg(test)]").next().unwrap()
    }

    #[test]
    fn runtime_boot_defers_socket_connection_to_the_worker() {
        let runtime = RuntimeState::boot();
        assert!(runtime.view.control_busy);
        assert!(!runtime.view.controls_enabled);
        assert_eq!(runtime.view.health, HealthState::Applying);
        assert_eq!(runtime.view.status, UiStatus::ConnectingControls);
        assert_eq!(
            render_ui_status(Language::Czech, &runtime.view.status),
            "Připojuji ovládání"
        );
    }

    #[test]
    fn public_release_defaults_to_english() {
        let language = Language::default();

        assert_eq!(language, Language::English);
        assert_eq!(language.code(), "en");
        assert_eq!(language.display_code(), "EN");
        assert_eq!(language.html_code(), "en");
    }

    #[test]
    fn telemetry_maps_third_fan_to_gpu_gauge_and_later_fans_to_diagnostics() {
        let mut state = AppState::default();
        apply_telemetry(
            &mut state,
            SystemTelemetry {
                cpu_temperature_c: Some(60.0),
                cpu_utilization_percent: Some(10.0),
                memory_used_mib: 1_024,
                memory_total_mib: 2_048,
                gpu: GpuTelemetry::default(),
                fans: FanState {
                    cpu: FanChannelState {
                        mode: Some(HardwareFanMode::Automatic),
                        pwm_raw: 0,
                        rpm: 2_100,
                    },
                    gpu: FanChannelState {
                        mode: Some(HardwareFanMode::Automatic),
                        pwm_raw: 0,
                        rpm: 2_200,
                    },
                },
                fan_rpm_channels: vec![
                    FanRpmChannel {
                        index: 1,
                        label: "CPU".to_string(),
                        rpm: Some(2_100),
                    },
                    FanRpmChannel {
                        index: 2,
                        label: "GPU".to_string(),
                        rpm: Some(2_200),
                    },
                    FanRpmChannel {
                        index: 3,
                        label: "GPU 2".to_string(),
                        rpm: Some(2_300),
                    },
                    FanRpmChannel {
                        index: 4,
                        label: "System".to_string(),
                        rpm: Some(2_400),
                    },
                ],
                profile_raw: Some("balanced".to_string()),
                profile: Some(HardwareProfile::Balanced),
                hardware: HardwareInfo::default(),
                power_supply: PowerSupplyTelemetry::default(),
            },
        );

        assert_eq!(state.telemetry.cpu_fan_rpm, Some(2_100));
        assert_eq!(state.telemetry.gpu_fan_rpm, Some(2_200));
        assert_eq!(state.telemetry.gpu_aux_fan_rpm, Some(2_300));
        assert_eq!(
            state.telemetry.additional_fans,
            vec![("System".to_string(), 2_400)]
        );
    }

    #[test]
    fn keyboard_editor_uses_only_confirmed_firmware_readback() {
        assert_eq!(
            keyboard_editor_readback(&KeyboardLightingState::default()),
            None
        );

        let lighting = KeyboardLightingState {
            available: true,
            brightness: 63,
            zones: [0x12_3456, 0xab_cdef, 0x00_1020, 0xfe_dcba],
            ..KeyboardLightingState::default()
        };
        assert_eq!(
            keyboard_editor_readback(&lighting),
            Some((63, [0x12_3456, 0xab_cdef, 0x00_1020, 0xfe_dcba]))
        );

        let effect = KeyboardLightingState {
            mode: 1,
            color: 0x66_33cc,
            ..lighting
        };
        assert_eq!(
            keyboard_editor_readback(&effect),
            Some((63, [0x66_33cc, 0xab_cdef, 0x00_1020, 0xfe_dcba]))
        );
    }

    #[test]
    fn lighting_capabilities_drive_endpoint_modes_and_sixteen_zone_draft() {
        let devices = vec![
            ControlLightingDevice {
                id: "hid-keyboard".to_string(),
                backend: CapabilityLightingBackend::Enek5130,
                target: CapabilityLightingTarget::Keyboard,
                zones: 1,
                modes: ControlLightingModes {
                    static_color: true,
                    brightness: true,
                    breathing: false,
                    neon: false,
                },
                state_readable: false,
            },
            ControlLightingDevice {
                id: "wmi-keyboard".to_string(),
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
        ];

        assert_eq!(preferred_lighting_index(&devices), Some(1));
        assert_eq!(
            lighting_mode_visibility(Some(devices[0].modes)),
            (true, true, false, false)
        );

        let seed = [0x01_0203, 0x04_0506, 0x07_0809, 0x0a_0b0c];
        let zones = lighting_zone_draft(&seed);
        assert_eq!(zones.len(), usize::from(MAX_LIGHTING_ZONES));
        assert_eq!(&zones[..seed.len()], &seed);
        assert_eq!(zones[4], KeyboardLightingState::default().zones[0]);
    }

    #[test]
    fn lighting_drafts_are_kept_per_target_without_a_reactive_effect_loop() {
        let device = ControlLightingDevice {
            id: "enek-logo".to_string(),
            backend: CapabilityLightingBackend::Enek5130,
            target: CapabilityLightingTarget::CoverLogo,
            zones: 1,
            modes: ControlLightingModes {
                static_color: true,
                brightness: true,
                breathing: false,
                neon: false,
            },
            state_readable: false,
        };
        let request = LightingApplyRequest {
            device_id: device.id.clone(),
            state_readable: false,
            mode: ControlLightingMode::Static,
            brightness: 47,
            speed: 0,
            color: [0x12, 0x34, 0x56],
            zone_colors: Vec::new(),
        };

        let (brightness, colors) =
            lighting_draft_for_device(&device, &KeyboardLightingState::default(), &[request]);
        assert_eq!(brightness, 47);
        assert_eq!(colors[0], 0x12_3456);

        let control_dock = production_source()
            .split("fn ControlDock")
            .nth(1)
            .unwrap()
            .split("fn StatusBar")
            .next()
            .unwrap();
        assert!(!control_dock.contains("use_effect"));
        assert!(!control_dock.contains("use_reactive"));
    }

    #[test]
    fn rear_logo_off_uses_zero_brightness_and_on_restores_the_draft() {
        let off = rear_logo_state(false, 63, 0x12_3456);
        assert!(!off.enabled);
        assert_eq!(off.brightness, 0);
        assert_eq!(off.color, [0x12, 0x34, 0x56]);

        let on = rear_logo_state(true, 63, 0x12_3456);
        assert!(on.enabled);
        assert_eq!(on.brightness, 63);
        assert_eq!(on.color, [0x12, 0x34, 0x56]);
    }

    #[test]
    fn color_picker_values_are_strict_six_digit_rgb() {
        assert_eq!(parse_color_value("#12abEF"), Some(0x12_ab_ef));
        assert_eq!(parse_color_value("12abef"), None);
        assert_eq!(parse_color_value("#abc"), None);
        assert_eq!(parse_color_value("#gg0000"), None);
    }

    #[test]
    fn typed_localized_formatters_preserve_fields_in_all_three_locales() {
        assert_eq!(color_zone_title(Language::Czech, 3), "Zóna 3");
        assert_eq!(color_zone_title(Language::English, 3), "Zone 3");
        assert_eq!(color_zone_title(Language::SimplifiedChinese, 3), "分区 3");
        assert_eq!(
            history_chart_description(Language::Czech, "Teploty", 60, "0 °C", "100 °C"),
            "Historie Teploty, 60 sekund, osa 0 °C až 100 °C"
        );
        assert_eq!(
            history_chart_description(Language::English, "Temperatures", 60, "0 °C", "100 °C"),
            "History of Temperatures, 60 seconds, axis 0 °C to 100 °C"
        );
        assert_eq!(
            history_chart_description(Language::SimplifiedChinese, "温度", 60, "0 °C", "100 °C"),
            "温度历史记录，60 秒，纵轴从 0 °C 到 100 °C"
        );
        assert_eq!(
            unknown_clock_reason(Language::SimplifiedChinese, 0x42),
            "未知原因 0x0000000000000042"
        );
    }

    #[test]
    fn typed_status_renderers_cover_every_variant_and_option_branch() {
        let static_statuses = [
            (
                UiStatus::AcerControlsConnected,
                super::MessageId::StatusAcerControlsConnected,
            ),
            (
                UiStatus::AcerNvidiaControlsConnected,
                super::MessageId::StatusAcerNvidiaControlsConnected,
            ),
            (
                UiStatus::ReadOnlyTelemetryConnected,
                super::MessageId::StatusReadOnlyTelemetryConnected,
            ),
            (
                UiStatus::ConnectingControls,
                super::MessageId::StatusConnectingControls,
            ),
            (
                UiStatus::PlatformRefreshed,
                super::MessageId::StatusPlatformRefreshed,
            ),
            (
                UiStatus::SettingsConfirmed,
                super::MessageId::StatusSettingsConfirmed,
            ),
            (
                UiStatus::LightingConfirmed,
                super::MessageId::StatusLightingConfirmed,
            ),
            (
                UiStatus::AppliedWithoutReadback,
                super::MessageId::StatusAppliedWithoutReadback,
            ),
            (
                UiStatus::WritingAndVerifying,
                super::MessageId::StatusWritingAndVerifying,
            ),
        ];
        for language in super::i18n::LocaleId::ENABLED {
            for (status, expected) in &static_statuses {
                assert_eq!(
                    render_ui_status(language, status),
                    super::text(language, *expected)
                );
                assert!(!render_compact_status(language, status).is_empty());
            }
        }

        let profile_cases = [
            (
                "low-power",
                GpuOffsetState::Unavailable,
                None,
                super::MessageId::StatusCompactProfileEco,
            ),
            (
                "quiet",
                GpuOffsetState::Reset,
                Some(ProfilePowerReceipt {
                    enforced_limit_mw: 115_000,
                    maximum_limit_mw: 140_000,
                    clock_event_reasons: crate::nvidia::ClockEventReasons::default(),
                }),
                super::MessageId::StatusCompactProfileQuiet,
            ),
            (
                "balanced",
                GpuOffsetState::OemTurbo,
                None,
                super::MessageId::StatusCompactProfileBalanced,
            ),
            (
                "balanced-performance",
                GpuOffsetState::CustomOrPartial,
                Some(ProfilePowerReceipt {
                    enforced_limit_mw: 115_000,
                    maximum_limit_mw: 140_000,
                    clock_event_reasons: crate::nvidia::ClockEventReasons::default(),
                }),
                super::MessageId::StatusCompactProfilePerformance,
            ),
            (
                "performance",
                GpuOffsetState::Reset,
                None,
                super::MessageId::StatusCompactProfileTurbo,
            ),
            (
                "future-profile",
                GpuOffsetState::Unavailable,
                Some(ProfilePowerReceipt {
                    enforced_limit_mw: 115_000,
                    maximum_limit_mw: 140_000,
                    clock_event_reasons: crate::nvidia::ClockEventReasons::default(),
                }),
                super::MessageId::StatusCompactProfileGeneric,
            ),
        ];
        for language in super::i18n::LocaleId::ENABLED {
            for (profile, offsets, power, compact_id) in &profile_cases {
                let status = UiStatus::ProfileVerified(ProfileApplyReceipt {
                    firmware_profile: (*profile).to_string(),
                    gpu_offsets: *offsets,
                    gpu_pstate_count: 4,
                    gpu_capability_available: true,
                    power: power.clone(),
                });
                let rendered = render_ui_status(language, &status);
                assert!(rendered.starts_with(super::text(
                    language,
                    super::MessageId::StatusProfileVerified
                )));
                assert!(rendered.contains(profile));
                match offsets {
                    GpuOffsetState::Unavailable => assert!(rendered.contains(super::text(
                        language,
                        super::MessageId::StatusOffsetUnavailable
                    ))),
                    GpuOffsetState::Reset => assert!(rendered.contains("+0/+0 MHz")),
                    GpuOffsetState::OemTurbo => assert!(rendered.contains("+100/+200 MHz")),
                    GpuOffsetState::CustomOrPartial => assert!(rendered.contains(super::text(
                        language,
                        super::MessageId::StatusOffsetCustomOrPartial
                    ))),
                }
                if power.is_some() {
                    assert!(rendered.contains("GPU 115/140 W"));
                } else {
                    assert!(rendered.contains(super::text(
                        language,
                        super::MessageId::StatusGpuLimitUnavailable
                    )));
                }
                assert_eq!(
                    render_compact_status(language, &status),
                    super::text(language, *compact_id)
                );
            }
        }

        let all_platform_bits = super::READ_ERROR_BATTERY_LIMIT
            | super::READ_ERROR_BATTERY_CALIBRATION
            | super::READ_ERROR_USB_CHARGING
            | super::READ_ERROR_KEYBOARD_TIMEOUT
            | super::READ_ERROR_BOOT_SOUND
            | super::READ_ERROR_LCD_OVERRIDE
            | super::READ_ERROR_REAR_LOGO;
        let platform_fields = PlatformReadErrorSet::from_mask(all_platform_bits).unwrap();
        let raw_lighting = super::RawDetail::new("rgb-raw-detail");
        let raw_platform = super::RawDetail::new("platform-raw-detail");
        let raw_hardware = super::RawDetail::new("hardware-raw-detail");
        let partial = UiStatus::PartialCapabilities(vec![
            UiDiagnostic::Lighting(raw_lighting),
            UiDiagnostic::Platform(PlatformIssue::Readback(platform_fields)),
            UiDiagnostic::Platform(PlatformIssue::Raw(raw_platform)),
            UiDiagnostic::Hardware(raw_hardware),
        ]);
        for language in super::i18n::LocaleId::ENABLED {
            let fields = render_platform_fields(language, platform_fields);
            for id in [
                super::MessageId::PlatformFieldBatteryLimit,
                super::MessageId::PlatformFieldBatteryCalibration,
                super::MessageId::PlatformFieldUsbCharging,
                super::MessageId::PlatformFieldKeyboardTimeout,
                super::MessageId::PlatformFieldBootSound,
                super::MessageId::PlatformFieldLcdOverride,
                super::MessageId::PlatformFieldRearLogo,
            ] {
                assert!(fields.contains(super::text(language, id)));
            }
            let rendered = render_ui_status(language, &partial);
            assert!(rendered.starts_with(super::text(
                language,
                super::MessageId::StatusPartialCapabilities
            )));
            for raw in [
                "rgb-raw-detail",
                "platform-raw-detail",
                "hardware-raw-detail",
            ] {
                assert!(rendered.contains(raw));
                assert_ne!(rendered, raw);
            }
            assert!(rendered.contains(&fields));
            assert_eq!(
                render_compact_status(language, &partial),
                super::text(language, super::MessageId::AppCompactStatus001)
            );

            let readback = UiStatus::PlatformReadbackFailed(platform_fields);
            assert_eq!(
                render_ui_status(language, &readback),
                format!(
                    "{}: {fields}",
                    super::text(language, super::MessageId::StatusPlatformReadbackFailed)
                )
            );
            assert_eq!(
                render_compact_status(language, &readback),
                super::text(language, super::MessageId::AppCompactStatus001)
            );

            let mismatch = UiStatus::GpuProfileMismatch {
                core_mhz: -125,
                memory_mhz: 250,
            };
            let mismatch_rendered = render_ui_status(language, &mismatch);
            assert!(mismatch_rendered.contains("core -125"));
            assert!(mismatch_rendered.contains("VRAM +250 MHz"));
            assert_eq!(
                render_compact_status(language, &mismatch),
                super::text(language, super::MessageId::AppCompactStatus002)
            );

            for (kind, id) in [
                (
                    UiErrorKind::Initialization,
                    super::MessageId::StatusInitializationFailure,
                ),
                (UiErrorKind::Fan, super::MessageId::StatusFanFailure),
                (UiErrorKind::Profile, super::MessageId::StatusProfileFailure),
                (
                    UiErrorKind::Lighting,
                    super::MessageId::StatusLightingFailure,
                ),
                (
                    UiErrorKind::Platform,
                    super::MessageId::StatusPlatformFailure,
                ),
                (UiErrorKind::Refresh, super::MessageId::StatusRefreshFailure),
            ] {
                let failure = UiStatus::Failure {
                    kind,
                    detail: super::RawDetail::new("bounded-raw-detail"),
                };
                assert_eq!(
                    render_ui_status(language, &failure),
                    format!("{}: bounded-raw-detail", super::text(language, id))
                );
                assert_eq!(
                    render_compact_status(language, &failure),
                    super::text(language, id)
                );
            }

            assert_eq!(
                render_telemetry_status(language, TelemetryHealth::Online),
                None
            );
            let connecting = super::text(language, super::MessageId::StatusTelemetryConnecting);
            assert_eq!(
                render_telemetry_status(language, TelemetryHealth::Connecting),
                Some((connecting.to_string(), connecting.to_string()))
            );
            let reconnecting = super::text(language, super::MessageId::StatusTelemetryReconnecting);
            assert_eq!(
                render_telemetry_status(
                    language,
                    TelemetryHealth::Reconnecting {
                        retry_after_seconds: u64::MAX,
                    }
                ),
                Some((
                    format!(
                        "{} · {} {} s",
                        reconnecting,
                        super::text(language, super::MessageId::StatusRetryIn),
                        u64::MAX
                    ),
                    reconnecting.to_string(),
                ))
            );
        }
    }

    #[test]
    fn raw_detail_is_bounded_and_never_replaces_the_localized_summary() {
        let detail = super::RawDetail::new("x".repeat(super::RAW_DETAIL_MAX_CHARS + 20));
        assert_eq!(
            detail.as_str().chars().count(),
            super::RAW_DETAIL_MAX_CHARS + 1
        );
        assert!(detail.as_str().ends_with('…'));
        let status = UiStatus::Failure {
            kind: UiErrorKind::Fan,
            detail,
        };
        assert!(render_ui_status(Language::English, &status).starts_with("Fan setting failed: "));
        assert!(
            render_ui_status(Language::Czech, &status)
                .starts_with("Nastavení ventilátorů selhalo: ")
        );
        assert!(
            render_ui_status(Language::SimplifiedChinese, &status).starts_with("风扇设置失败: ")
        );
        assert_eq!(
            render_compact_status(Language::SimplifiedChinese, &status),
            super::text(
                Language::SimplifiedChinese,
                super::MessageId::StatusFanFailure
            )
        );
    }

    #[test]
    fn write_only_lighting_apply_preserves_readable_wmi_state() {
        let mut state = AppState {
            lighting: KeyboardLightingState {
                available: true,
                powered: true,
                mode: 1,
                brightness: 63,
                color: 0x12_3456,
                ..KeyboardLightingState::default()
            },
            ..AppState::default()
        };
        let readable_state = state.lighting.clone();
        let request = LightingApplyRequest {
            device_id: "hid-keyboard".to_string(),
            state_readable: false,
            mode: ControlLightingMode::Neon,
            brightness: 88,
            speed: 5,
            color: [0x65, 0x43, 0x21],
            zone_colors: Vec::new(),
        };

        apply_control_update(
            &mut state,
            ControlUpdate {
                request: ControlRequest::foreground(ControlAction::LightingApply(request.clone())),
                result: Ok(ControlOutcome::LightingApplied {
                    request: request.clone(),
                    firmware_state: None,
                }),
            },
        );

        assert_eq!(state.lighting, readable_state);
        assert_eq!(state.last_applied_lighting, vec![request]);
        assert_eq!(state.status, UiStatus::AppliedWithoutReadback);
        assert_eq!(
            render_ui_status(Language::Czech, &state.status),
            "Použito · stav nelze přečíst"
        );
    }

    #[test]
    fn typed_compact_status_keeps_receipts_localized_without_parsing_prose() {
        let turbo = UiStatus::ProfileVerified(ProfileApplyReceipt {
            firmware_profile: "performance".to_string(),
            gpu_offsets: GpuOffsetState::OemTurbo,
            gpu_pstate_count: 4,
            gpu_capability_available: true,
            power: None,
        });
        assert_eq!(
            render_compact_status(Language::Czech, &turbo),
            "Turbo potvrzeno"
        );
        assert_eq!(
            render_compact_status(Language::English, &turbo),
            "Turbo verified"
        );
        let partial = UiStatus::PartialCapabilities(vec![UiDiagnostic::Lighting(
            super::RawDetail::new("temporary RGB readback failure"),
        )]);
        assert_eq!(
            render_compact_status(Language::English, &partial),
            "Partial readback"
        );
        let mismatch = UiStatus::GpuProfileMismatch {
            core_mhz: 0,
            memory_mhz: 200,
        };
        assert_eq!(
            render_compact_status(Language::Czech, &mismatch),
            "GPU nesedí"
        );
        let failure = UiStatus::Failure {
            kind: UiErrorKind::Refresh,
            detail: super::RawDetail::new(
                "an otherwise unknown diagnostic that is deliberately much too long",
            ),
        };
        assert_eq!(
            render_compact_status(Language::English, &failure),
            "State refresh failed"
        );
        for status in [
            "Turbo potvrzeno",
            "Partial readback",
            "GPU nesedí",
            "State refresh failed",
        ] {
            assert!(status.chars().count() <= 28, "status is too long: {status}");
        }
    }

    #[test]
    fn write_only_lighting_never_claims_firmware_readback() {
        assert_eq!(lighting_apply_status(true), UiStatus::SettingsConfirmed);
        assert_eq!(
            lighting_apply_status(false),
            UiStatus::AppliedWithoutReadback
        );
        assert_eq!(
            render_ui_status(Language::English, &lighting_apply_status(false)),
            "Applied · state readback unavailable"
        );
        assert_eq!(
            render_compact_status(Language::English, &lighting_apply_status(false)),
            "Last applied"
        );
        assert_eq!(
            render_ui_status(Language::English, &UiStatus::LightingConfirmed),
            "Lighting confirmed by firmware"
        );
        assert_eq!(
            render_compact_status(Language::Czech, &UiStatus::LightingConfirmed),
            "Podsvícení potvrzeno"
        );
    }

    #[test]
    fn setting_toggle_distinguishes_read_errors_from_unsupported_features() {
        assert_eq!(
            setting_toggle_text(Some(true), false, Language::Czech),
            "Zap"
        );
        assert_eq!(
            setting_toggle_text(Some(false), false, Language::English),
            "Off"
        );
        assert_eq!(
            setting_toggle_text(None, true, Language::Czech),
            "Chyba čtení"
        );
        assert_eq!(
            setting_toggle_text(None, true, Language::English),
            "Read error"
        );
        assert_eq!(
            setting_toggle_text(None, false, Language::Czech),
            "Nepodporováno"
        );
        assert_eq!(
            setting_toggle_text(None, false, Language::English),
            "Unsupported"
        );
    }

    #[test]
    fn foreground_control_request_is_single_flight_until_completion() {
        let mut state = AppState::default();
        let request = ControlRequest::foreground(ControlAction::FanMode(FanMode::Maximum));

        assert!(begin_control_request(&mut state, request.clone()));
        assert!(state.control_busy);
        assert_eq!(state.health, HealthState::Applying);
        assert!(!begin_control_request(
            &mut state,
            ControlRequest::foreground(ControlAction::FanMode(FanMode::Auto)),
        ));

        apply_control_update(
            &mut state,
            ControlUpdate {
                request,
                result: Ok(ControlOutcome::FanMode(FanMode::Maximum)),
            },
        );
        assert!(!state.control_busy);
        assert_eq!(state.health, HealthState::Healthy);
        assert_eq!(state.fan_mode, FanMode::Maximum);
        assert_eq!(state.status, UiStatus::SettingsConfirmed);
    }

    #[test]
    fn verified_profile_transition_ignores_cross_plane_telemetry_until_coherent() {
        let mut state = AppState::default();
        let request = ControlRequest::foreground(ControlAction::Profile(
            PlatformProfile::Turbo.as_sysfs().to_string(),
        ));
        assert!(begin_control_request(&mut state, request.clone()));
        apply_control_update(
            &mut state,
            ControlUpdate {
                request,
                result: Ok(ControlOutcome::Profile {
                    profile_raw: PlatformProfile::Turbo.as_sysfs().to_string(),
                    receipt: ProfileApplyReceipt {
                        firmware_profile: PlatformProfile::Turbo.as_sysfs().to_string(),
                        gpu_offsets: GpuOffsetState::OemTurbo,
                        gpu_pstate_count: 4,
                        gpu_capability_available: true,
                        power: None,
                    },
                }),
            },
        );

        assert_eq!(state.platform_profile, PlatformProfile::Turbo);
        assert_eq!(state.profile_sync.target, Some(PlatformProfile::Turbo));
        assert_eq!(state.profile_sync.grace_samples, PROFILE_SYNC_GRACE_SAMPLES);

        // The firmware profile and cached NVML offsets are sampled on
        // independent schedules, so either half of the pair may arrive first.
        for (profile, core, memory) in [
            (HardwareProfile::Balanced, 0, 0),
            (HardwareProfile::Turbo, 0, 0),
            (HardwareProfile::Balanced, 100, 200),
        ] {
            reconcile_profile_telemetry(&mut state, profile, Some(core), Some(memory), Some(true));
            assert_eq!(state.platform_profile, PlatformProfile::Turbo);
            assert_eq!(state.health, HealthState::Healthy);
            assert!(!matches!(state.status, UiStatus::GpuProfileMismatch { .. }));
        }

        reconcile_profile_telemetry(
            &mut state,
            HardwareProfile::Turbo,
            Some(100),
            Some(200),
            Some(true),
        );
        assert_eq!(state.profile_sync.target, None);
        assert_eq!(state.platform_profile, PlatformProfile::Turbo);
        assert_eq!(state.health, HealthState::Healthy);
    }

    #[test]
    fn persistent_profile_mismatch_still_requires_and_raises_a_warning() {
        let mut state = AppState::default();

        reconcile_profile_telemetry(
            &mut state,
            HardwareProfile::Turbo,
            Some(0),
            Some(0),
            Some(true),
        );
        assert_eq!(state.platform_profile, PlatformProfile::Balanced);
        assert_eq!(state.health, HealthState::Healthy);

        reconcile_profile_telemetry(
            &mut state,
            HardwareProfile::Turbo,
            Some(0),
            Some(0),
            Some(true),
        );
        assert_eq!(state.platform_profile, PlatformProfile::Turbo);
        assert_eq!(state.health, HealthState::Warning);
        assert_eq!(
            state.status,
            UiStatus::GpuProfileMismatch {
                core_mhz: 0,
                memory_mhz: 0,
            }
        );

        reconcile_profile_telemetry(
            &mut state,
            HardwareProfile::Turbo,
            Some(100),
            Some(200),
            Some(true),
        );
        assert_eq!(state.health, HealthState::Healthy);
        assert_eq!(state.status, UiStatus::AcerNvidiaControlsConnected);
    }

    #[test]
    fn gpu_status_pairs_live_draw_with_the_current_enforced_limit() {
        assert_eq!(power_usage_limit(Some(4.2), Some(30.0)), "4/30 W");
        assert_eq!(power_usage_limit(None, Some(30.0)), "--/-- W");
    }

    #[test]
    fn sleeping_gpu_uses_truthful_zero_history_without_invented_offsets() {
        let point = TelemetryPoint::from(&super::Telemetry {
            gpu_sleeping: true,
            gpu_load_percent: Some(87.0),
            gpu_power_w: Some(99.0),
            gpu_graphics_clock_mhz: Some(2_400),
            ..super::Telemetry::default()
        });
        assert_eq!(point.gpu_load_percent, Some(0.0));
        assert_eq!(point.gpu_power_w, Some(0.0));
        assert_eq!(point.gpu_graphics_clock_mhz, Some(0.0));
        assert_eq!(gpu_offset_detail("VF/GPC", None), "VF/GPC -- MHz");
    }

    #[test]
    fn background_platform_refresh_does_not_replace_global_status() {
        let warning = UiStatus::GpuProfileMismatch {
            core_mhz: 0,
            memory_mhz: 200,
        };
        let mut state = AppState {
            health: HealthState::Warning,
            status: warning.clone(),
            ..AppState::default()
        };
        let request = ControlRequest::background(ControlAction::Platform(PlatformAction::Refresh));

        assert!(begin_control_request(&mut state, request.clone()));
        apply_control_update(
            &mut state,
            ControlUpdate {
                request,
                result: Err("platform unavailable".to_string()),
            },
        );

        assert!(!state.control_busy);
        assert!(!state.platform_busy);
        assert_eq!(state.health, HealthState::Warning);
        assert_eq!(state.status, warning);
        assert_eq!(
            state.platform_error,
            Some(super::PlatformIssue::Raw(super::RawDetail::new(
                "platform unavailable"
            )))
        );
    }

    #[test]
    fn failed_refresh_preserves_existing_capabilities_and_controls() {
        let capabilities = ControlCapabilities {
            vendor: "Acer".to_string(),
            product: "Predator PHN16-72".to_string(),
            reference_model: true,
            profiles: ControlProfileCapabilities {
                backend: None,
                choices: Vec::new(),
                current: Some("balanced".to_string()),
            },
            fans: ControlFanCapabilities {
                backend: None,
                rpm_channels: Vec::new(),
                auto: false,
                manual: false,
                maximum: false,
            },
            lighting: Vec::new(),
            platform: ControlPlatformCapabilities::default(),
        };
        let mut state = AppState {
            capabilities: Some(capabilities.clone()),
            controls_enabled: true,
            ..AppState::default()
        };
        let request = ControlRequest::foreground(ControlAction::Refresh);

        assert!(begin_control_request(&mut state, request.clone()));
        apply_control_update(
            &mut state,
            ControlUpdate {
                request,
                result: Err("refresh failed".to_string()),
            },
        );

        assert!(!state.control_busy);
        assert!(!state.platform_busy);
        assert!(state.controls_enabled);
        assert_eq!(state.capabilities, Some(capabilities));
        assert_eq!(
            state.platform_error,
            Some(super::PlatformIssue::Raw(super::RawDetail::new(
                "refresh failed"
            )))
        );
        assert_eq!(
            state.status,
            UiStatus::Failure {
                kind: UiErrorKind::Refresh,
                detail: super::RawDetail::new("refresh failed"),
            }
        );
    }

    #[test]
    fn partial_refresh_preserves_verified_lighting_after_a_readback_error() {
        let verified = KeyboardLightingState {
            available: true,
            powered: true,
            brightness: 63,
            zones: [0x12_3456, 0xab_cdef, 0x00_1020, 0xfe_dcba],
            ..KeyboardLightingState::default()
        };
        let capabilities = ControlCapabilities {
            vendor: "Acer".to_string(),
            product: "Predator PHN16-72".to_string(),
            reference_model: true,
            profiles: ControlProfileCapabilities {
                backend: None,
                choices: Vec::new(),
                current: Some("balanced".to_string()),
            },
            fans: ControlFanCapabilities {
                backend: None,
                rpm_channels: Vec::new(),
                auto: false,
                manual: false,
                maximum: false,
            },
            lighting: vec![ControlLightingDevice {
                id: "wmi-keyboard".to_string(),
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
            }],
            platform: ControlPlatformCapabilities::default(),
        };
        let mut state = AppState {
            lighting: verified.clone(),
            capabilities: Some(capabilities.clone()),
            ..AppState::default()
        };

        let (_, diagnostics) = apply_capability_snapshot(
            &mut state,
            capabilities,
            Err("temporary RGB readback failure".to_string()),
            Ok(empty_platform_state()),
        );

        assert_eq!(state.lighting, verified);
        assert_eq!(
            state.lighting_error,
            Some(super::RawDetail::new("temporary RGB readback failure"))
        );
        assert_eq!(
            diagnostics,
            vec![UiDiagnostic::Lighting(super::RawDetail::new(
                "temporary RGB readback failure"
            ))]
        );
    }

    #[test]
    fn failed_initialize_disables_controls_only_without_a_snapshot() {
        let mut state = RuntimeState::boot().view;
        apply_control_update(
            &mut state,
            ControlUpdate {
                request: ControlRequest::background(ControlAction::Initialize),
                result: Err("initialization failed".to_string()),
            },
        );

        assert!(!state.control_busy);
        assert!(!state.controls_enabled);
        assert_eq!(state.health, HealthState::Warning);
        assert_eq!(
            state.status,
            UiStatus::Failure {
                kind: UiErrorKind::Initialization,
                detail: super::RawDetail::new("initialization failed"),
            }
        );
    }

    #[test]
    fn control_result_slot_preserves_delayed_completions_in_order() {
        let slot = ControlResultSlot::default();
        let first = ControlRequest::foreground(ControlAction::FanMode(FanMode::Auto));
        let second = ControlRequest::foreground(ControlAction::FanMode(FanMode::Maximum));
        slot.publish(ControlUpdate {
            request: first.clone(),
            result: Ok(ControlOutcome::FanMode(FanMode::Auto)),
        });
        slot.publish(ControlUpdate {
            request: second.clone(),
            result: Ok(ControlOutcome::FanMode(FanMode::Maximum)),
        });

        assert_eq!(slot.try_take().unwrap().request, first);
        assert_eq!(slot.try_take().unwrap().request, second);
        assert!(slot.try_take().is_none());
    }

    #[test]
    fn telemetry_slot_keeps_only_the_latest_state() {
        let slot = TelemetrySlot::default();
        slot.publish_latest(TelemetryUpdate::Error {
            message: "old".to_string(),
            retry_after: Duration::from_secs(1),
        });
        slot.publish_latest(TelemetryUpdate::Error {
            message: "new".to_string(),
            retry_after: Duration::from_secs(2),
        });
        match slot.try_take() {
            Some(TelemetryUpdate::Error {
                message,
                retry_after,
            }) => {
                assert_eq!(message, "new");
                assert_eq!(retry_after, Duration::from_secs(2));
            }
            _ => panic!("latest telemetry error was not preserved"),
        }
        assert!(slot.try_take().is_none());
    }

    #[test]
    fn telemetry_reconnect_delay_is_bounded() {
        assert_eq!(telemetry_retry_delay(1), Duration::from_secs(1));
        assert_eq!(telemetry_retry_delay(2), Duration::from_secs(2));
        assert_eq!(telemetry_retry_delay(3), Duration::from_secs(4));
        assert_eq!(telemetry_retry_delay(4), Duration::from_secs(8));
        assert_eq!(telemetry_retry_delay(30), Duration::from_secs(8));
    }

    #[test]
    fn lighting_response_is_exact_and_complete() {
        let state = parse_lighting_state(
            "power=on mode=3 speed=5 brightness=80 direction=2 color=000000 zones=ff0000,00ff00,0000ff,ffffff",
        )
        .unwrap();
        assert_eq!(state.zones[3], 0xff_ffff);
        assert!(parse_lighting_state("mode=0").is_err());
        assert!(parse_lighting_state(
            "power=on mode=0 speed=0 brightness=100 direction=0 color=000000 zones=000000,000000,000000,zzzzzz"
        )
        .is_err());
    }

    #[test]
    fn telemetry_history_is_a_fixed_chronological_ring() {
        let mut history = TelemetryHistory::default();
        for index in 0..(TELEMETRY_HISTORY_CAPACITY + 17) {
            history.push(TelemetryPoint {
                cpu_load_percent: Some(index as f32),
                ..TelemetryPoint::default()
            });
        }
        assert_eq!(history.len, TELEMETRY_HISTORY_CAPACITY);
        assert_eq!(history.get(0).unwrap().cpu_load_percent, Some(17.0));
        assert_eq!(
            history
                .get(TELEMETRY_HISTORY_CAPACITY - 1)
                .unwrap()
                .cpu_load_percent,
            Some((TELEMETRY_HISTORY_CAPACITY + 16) as f32)
        );
        assert!(history.get(TELEMETRY_HISTORY_CAPACITY).is_none());
        let points = graph_points(&history, |point| point.cpu_load_percent, 200.0);
        assert_eq!(
            points.split_ascii_whitespace().count(),
            TELEMETRY_HISTORY_CAPACITY
        );
        history.push(TelemetryPoint {
            cpu_load_percent: Some((TELEMETRY_HISTORY_CAPACITY + 17) as f32),
            ..TelemetryPoint::default()
        });
        let shifted_points = graph_points(&history, |point| point.cpu_load_percent, 200.0);
        assert_ne!(points, shifted_points);
        assert_eq!(history.len, TELEMETRY_HISTORY_CAPACITY);
        assert_eq!(history.get(0).unwrap().cpu_load_percent, Some(18.0));
        assert_eq!(
            history
                .get(TELEMETRY_HISTORY_CAPACITY - 1)
                .unwrap()
                .cpu_load_percent,
            Some((TELEMETRY_HISTORY_CAPACITY + 17) as f32)
        );
        history.push(TelemetryPoint {
            cpu_load_percent: Some((TELEMETRY_HISTORY_CAPACITY + 18) as f32),
            ..TelemetryPoint::default()
        });
        let shifted_again = graph_points(&history, |point| point.cpu_load_percent, 200.0);
        assert_ne!(shifted_points, shifted_again);
        assert_eq!(history.get(0).unwrap().cpu_load_percent, Some(19.0));
        assert_eq!(
            history
                .get(TELEMETRY_HISTORY_CAPACITY - 1)
                .unwrap()
                .cpu_load_percent,
            Some((TELEMETRY_HISTORY_CAPACITY + 18) as f32)
        );
    }

    #[test]
    fn privileged_dmi_fields_survive_unprivileged_telemetry_refreshes() {
        let mut current = MemoryHardwareInfo {
            total_mib: Some(31_744),
            ..MemoryHardwareInfo::default()
        };
        merge_privileged_memory(
            &mut current,
            MemoryHardwareInfo {
                total_mib: None,
                speed_mt_s: Some(5_600),
                memory_type: Some("DDR5".to_string()),
                channels: Some(2),
                modules: Some(2),
            },
        );
        assert_eq!(current.total_mib, Some(31_744));
        assert_eq!(current.speed_mt_s, Some(5_600));
        assert_eq!(current.memory_type.as_deref(), Some("DDR5"));
        assert_eq!(current.channels, Some(2));
        assert_eq!(current.modules, Some(2));
    }

    #[test]
    fn system_controls_live_only_on_advanced_device_page() {
        let production = production_source();
        let control_dock = production
            .split("fn ControlDock")
            .nth(1)
            .unwrap()
            .split("fn StatusBar")
            .next()
            .unwrap();
        let advanced_device = production
            .split("fn PlatformAdvanced")
            .nth(1)
            .unwrap()
            .split("fn usb_charging_label")
            .next()
            .unwrap();

        assert!(!control_dock.contains("DockTab::System"));
        assert!(!control_dock.contains("PlatformAction::BatteryLimit"));
        assert!(!control_dock.contains("PlatformAction::UsbCharging"));
        assert!(advanced_device.contains("PlatformAction::BatteryLimit"));
        assert!(advanced_device.contains("PlatformAction::UsbCharging"));
        assert!(advanced_device.contains("PlatformAction::KeyboardTimeout"));
    }

    #[test]
    fn desktop_coalesces_native_aspect_lock_and_webview_transform_updates() {
        let production = production_source();

        assert!(production.contains("with_decorations(false)"));
        assert!(production.contains("with_resizable(true)"));
        assert!(production.contains("WindowChrome { language: language() }"));
        assert!(production.contains("ResizeHandles {"));
        assert!(production.contains("drag_resize_window"));
        assert!(production.contains("new ResizeObserver(schedule)"));
        assert!(production.contains("requestAnimationFrame(fit)"));
        assert!(production.contains("use_wry_event_handler"));
        assert!(production.contains("WindowEvent::Resized"));
        assert!(production.contains("glib::idle_add_local_once"));
        assert!(production.contains("pending_correction"));
        assert!(!production.contains("foreignObject"));
        assert!(!production.contains("set_zoom_level"));
        assert!(!production.contains("set_geometry_hints"));
        assert!(!production.contains("gtk_window"));
    }

    #[test]
    fn compact_and_advanced_endpoints_preserve_fixed_titlebar_and_workspace_scale() {
        let height = 830.0;
        let compact = logical_window_size(false, height);
        let advanced = logical_window_size(true, height);
        let workspace_height = height - TITLEBAR_DESIGN_HEIGHT;

        assert_eq!(compact.height, height);
        assert_eq!(advanced.height, height);
        assert!((compact.width / workspace_height - workspace_aspect_ratio(false)).abs() < 1e-12);
        assert!((advanced.width / workspace_height - workspace_aspect_ratio(true)).abs() < 1e-12);
        assert_eq!(
            workspace_aspect_ratio(false),
            COMPACT_DESIGN_WIDTH / WORKSPACE_DESIGN_HEIGHT
        );
        assert_eq!(
            workspace_aspect_ratio(true),
            ADVANCED_DESIGN_WIDTH / WORKSPACE_DESIGN_HEIGHT
        );
    }

    #[test]
    fn native_endpoint_sizes_are_clamped_with_a_fixed_titlebar() {
        let tiny = logical_window_size(false, 30.0);
        assert_eq!(tiny.height, MIN_WINDOW_HEIGHT);
        assert_eq!(
            tiny.width,
            (MIN_WINDOW_HEIGHT - TITLEBAR_DESIGN_HEIGHT) * workspace_aspect_ratio(false)
        );
    }

    #[test]
    fn resize_projection_is_affine_idempotent_and_accounts_for_titlebar() {
        for advanced in [false, true] {
            let accepted = logical_window_size(advanced, 830.0).to_physical::<u32>(1.0);
            let horizontal_request = PhysicalSize::new(accepted.width + 137, accepted.height);
            let horizontal = aspect_constrained_size(
                horizontal_request,
                accepted,
                advanced,
                1.0,
                Some(ResizeDirection::East),
            );
            let vertical_request = PhysicalSize::new(accepted.width, accepted.height + 91);
            let vertical = aspect_constrained_size(
                vertical_request,
                accepted,
                advanced,
                1.0,
                Some(ResizeDirection::South),
            );

            for projected in [horizontal, vertical] {
                let workspace_height = f64::from(projected.height) - TITLEBAR_DESIGN_HEIGHT;
                assert!(
                    (f64::from(projected.width) / workspace_height
                        - workspace_aspect_ratio(advanced))
                    .abs()
                        < 0.002
                );
                let repeated = aspect_constrained_size(projected, projected, advanced, 1.0, None);
                assert!(physical_size_close(projected, repeated));
            }
        }
    }

    #[test]
    fn resize_release_and_focus_loss_schedule_at_most_one_final_snap() {
        let accepted = PhysicalSize::new(620, 830);
        let actual = PhysicalSize::new(701, 851);
        let mut resize = AspectResizeState::new(accepted);
        resize.direction = Some(ResizeDirection::East);

        assert!(resize.finish_drag(actual));
        assert_eq!(resize.direction, None);
        assert_eq!(resize.latest_request, Some(actual));
        assert!(resize.correction_scheduled);

        // Focus loss after the left-button release must not schedule a second
        // snap for the same native drag.
        assert!(!resize.finish_drag(actual));
        assert_eq!(resize.latest_request, Some(actual));

        // If a correction was already sent before release, that correction is
        // the one final snap and release only ends the drag.
        let mut in_flight = AspectResizeState::new(accepted);
        in_flight.direction = Some(ResizeDirection::SouthEast);
        let generation = in_flight.begin_pending_correction(actual, false);
        assert!(!in_flight.finish_drag(PhysicalSize::new(700, 850)));
        assert_eq!(in_flight.direction, None);
        assert_eq!(
            in_flight
                .pending_correction
                .map(|pending| pending.generation),
            Some(generation)
        );
        assert!(in_flight.latest_request.is_none());
    }

    #[test]
    fn mismatched_or_timed_out_resize_ack_accepts_actual_size_without_replay() {
        let accepted = PhysicalSize::new(620, 830);
        let target = PhysicalSize::new(700, 900);
        let mismatch = PhysicalSize::new(696, 896);
        let mut resize = AspectResizeState::new(accepted);
        let generation = resize.begin_pending_correction(target, false);

        assert_eq!(
            resize.observe_resize(mismatch),
            ResizeObservation::NoSchedule
        );
        assert_eq!(resize.accepted, mismatch);
        assert!(resize.pending_correction.is_none());
        assert!(resize.latest_request.is_none());
        assert_eq!(
            resize.expire_pending_correction(generation, target),
            ResizeObservation::Ignore
        );

        let next_target = PhysicalSize::new(710, 910);
        let next_actual = PhysicalSize::new(708, 908);
        let next_generation = resize.begin_pending_correction(next_target, false);
        assert_eq!(
            resize.expire_pending_correction(next_generation.wrapping_add(1), next_actual),
            ResizeObservation::Ignore
        );
        assert!(resize.pending_correction.is_some());
        assert_eq!(
            resize.expire_pending_correction(next_generation, next_actual),
            ResizeObservation::NoSchedule
        );
        assert_eq!(resize.accepted, next_actual);
        assert!(resize.pending_correction.is_none());
        assert!(resize.latest_request.is_none());
    }

    #[test]
    fn mode_switch_ignores_intermediate_resize_until_target_or_timeout() {
        let accepted = PhysicalSize::new(620, 830);
        let intermediate = PhysicalSize::new(900, 830);
        let target = PhysicalSize::new(1_200, 830);
        let mut resize = AspectResizeState::new(accepted);
        let generation = resize.begin_pending_correction(target, true);

        assert_eq!(
            resize.observe_resize(intermediate),
            ResizeObservation::Ignore
        );
        assert_eq!(resize.accepted, accepted);
        assert_eq!(
            resize.pending_correction.map(|pending| pending.generation),
            Some(generation)
        );

        assert_eq!(resize.observe_resize(target), ResizeObservation::NoSchedule);
        assert_eq!(resize.accepted, target);
        assert!(resize.pending_correction.is_none());
        assert_eq!(
            resize.expire_pending_correction(generation, intermediate),
            ResizeObservation::Ignore
        );
    }

    #[test]
    fn drag_release_during_pending_correction_gets_one_final_snap_after_bad_ack() {
        let accepted = PhysicalSize::new(620, 830);
        let target = PhysicalSize::new(700, 900);
        let clamped = PhysicalSize::new(696, 896);
        let mut resize = AspectResizeState::new(accepted);
        resize.direction = Some(ResizeDirection::East);
        resize.begin_pending_correction(target, false);

        assert!(!resize.finish_drag(clamped));
        assert!(resize.finalize_after_pending);
        assert_eq!(
            resize.observe_resize(clamped),
            ResizeObservation::ScheduleCorrection
        );
        assert_eq!(resize.direction, None);
        assert_eq!(resize.latest_request, Some(clamped));
        assert!(resize.correction_scheduled);

        // The one final snap is not itself re-finalized if the WM clamps it.
        resize.correction_scheduled = false;
        resize.latest_request = None;
        resize.begin_pending_correction(target, false);
        assert_eq!(
            resize.observe_resize(clamped),
            ResizeObservation::NoSchedule
        );
        assert!(!resize.finalize_after_pending);
        assert!(resize.latest_request.is_none());
    }

    #[test]
    fn drag_release_during_pending_correction_gets_one_final_snap_after_timeout() {
        let accepted = PhysicalSize::new(620, 830);
        let target = PhysicalSize::new(700, 900);
        let actual = PhysicalSize::new(696, 896);
        let mut resize = AspectResizeState::new(accepted);
        resize.direction = Some(ResizeDirection::SouthEast);
        let generation = resize.begin_pending_correction(target, false);

        assert!(!resize.finish_drag(actual));
        assert_eq!(
            resize.expire_pending_correction(generation, actual),
            ResizeObservation::ScheduleCorrection
        );
        assert_eq!(resize.latest_request, Some(actual));
        assert!(resize.correction_scheduled);
    }

    #[test]
    fn css_scales_one_fixed_composited_stage_without_descendant_reflow() {
        assert!(APP_CSS_SOURCE.contains(".design-stage"));
        assert!(APP_CSS_SOURCE.contains("width: 1200px"));
        assert!(APP_CSS_SOURCE.contains("height: 650px"));
        assert!(APP_CSS_SOURCE.contains("contain: layout paint style"));
        assert!(APP_CSS_SOURCE.contains("will-change: transform"));
        assert!(APP_CSS_SOURCE.contains("scale(var(--ui-scale, 1))"));
        assert!(APP_CSS_SOURCE.contains("grid-template-rows: 48px minmax(0, 1fr)"));
        assert!(!APP_CSS_SOURCE.contains("cqh"));
        assert!(!APP_CSS_SOURCE.contains("@media"));
        assert!(!APP_CSS_SOURCE.contains("zoom:"));
    }

    #[test]
    fn every_ui_font_is_at_least_the_balance_button_size() {
        for declaration in APP_CSS_SOURCE.split("font-size:").skip(1) {
            let value = declaration
                .trim_start()
                .split("px")
                .next()
                .unwrap()
                .parse::<f64>()
                .unwrap();
            assert!(value >= 12.0, "font-size {value}px is below 12px");
        }
    }

    #[test]
    fn ui_never_replaces_text_with_an_ellipsis() {
        let production = production_source();
        assert!(!APP_CSS_SOURCE.contains("text-overflow: ellipsis"));
        assert_eq!(production.matches('…').count(), 1);
        assert!(production.contains("bounded.push('…')"));
        assert!(!production.contains("..."));
    }

    #[test]
    fn fan_cards_and_hidden_dock_editors_have_stable_bento_grids() {
        let source = include_str!("app.rs");
        let fan_panel_rule = css_rule(".fan-panel {");
        let manual_panel_rule = css_rule(".fan-panel.manual {");
        let control_button_rule = css_rule(".profile,\n.dock-tab,\n.mode {");
        let fan_summary_rule = css_rule(".fan-mode-summary {");
        let manual_editor_rule = css_rule(".manual-panel {");
        assert!(APP_CSS_SOURCE.contains(".gauge-grid"));
        assert!(APP_CSS_SOURCE.contains("grid-template-columns: repeat(2, minmax(0, 1fr))"));
        assert!(APP_CSS_SOURCE.contains("width: 100%"));
        assert!(APP_CSS_SOURCE.contains("width: 240px"));
        assert!(APP_CSS_SOURCE.contains("height: 240px"));
        assert!(APP_CSS_SOURCE.contains("height: 5.026px"));
        assert!(source.contains("let needle = -225.0 + sweep"));
        assert!(fan_panel_rule.contains("grid-template-rows: 40px 40px"));
        assert!(control_button_rule.contains("height: 40px"));
        assert!(fan_summary_rule.contains("height: 40px"));
        assert!(manual_editor_rule.contains("height: 40px"));
        assert!(manual_panel_rule.contains("grid-template-columns: 1fr"));
        assert!(source.contains("Automatic RPM control selected"));
        assert!(source.contains("Maximum fan RPM selected"));
        assert!(source.contains("řízení otáček"));
        assert!(source.contains("fan-mode-summary"));
        assert!(
            APP_CSS_SOURCE.contains("grid-template-areas: \"power colors\" \"brightness actions\"")
        );
        assert!(APP_CSS_SOURCE.contains("grid-template-columns: repeat(2, minmax(0, 1fr))"));
        assert!(!APP_CSS_SOURCE.contains(".platform-basics"));
    }

    #[test]
    fn advanced_pages_fill_the_stage_with_balanced_bento_tiles() {
        let source = include_str!("app.rs");
        let production = production_source();
        let header_control_rule = css_rule(".language-toggle,\n.health-pill,\n.advanced-toggle {");
        let shell_rule = css_rule(".asense-shell {");
        let advanced_panel_rule = css_rule(".advanced-panel {");
        let hardware_note_rule = css_rule(".hardware-note {");
        assert!(APP_CSS_SOURCE.contains("grid-template-columns: repeat(4, minmax(0, 1fr))"));
        assert!(shell_rule.contains("background:"));
        assert!(header_control_rule.contains("border-radius: 10.121px"));
        assert!(!header_control_rule.contains("border-radius: 999px"));
        assert!(advanced_panel_rule.contains("background: transparent"));
        assert!(!APP_CSS_SOURCE.contains(".advanced-panel::before"));
        assert!(!APP_CSS_SOURCE.contains(".metrics-history"));
        assert!(!APP_CSS_SOURCE.contains(".chart-time"));
        assert!(!production.contains("LIVE · 1 s"));
        assert!(!production.contains("Historie {history.len}"));
        assert!(production.contains("let history_seconds = history.len.max(1);"));
        assert!(production.contains("class: \"chart-scale-y-max\""));
        assert!(production.contains("class: \"chart-scale-y-min\""));
        assert!(production.contains("class: \"chart-scale-x-start\""));
        assert!(production.contains("class: \"chart-scale-x-end\""));
        assert!(production.contains("y_max: \"3 / 10 GHz\".to_string()"));
        assert!(APP_CSS_SOURCE.contains(".chart-plot"));
        assert!(APP_CSS_SOURCE.contains(".chart-scale"));
        assert!(APP_CSS_SOURCE.contains("font-size: 12px"));
        assert!(!APP_CSS_SOURCE.contains(".chart-scale {\n  display:"));
        assert!(APP_CSS_SOURCE.contains(".advanced-charts"));
        assert!(APP_CSS_SOURCE.contains(".advanced-heading"));
        assert!(!APP_CSS_SOURCE.contains(".advanced-heading h2"));
        assert!(!production.contains("Pokročilé metriky"));
        assert!(!production.contains("Advanced metrics"));
        assert!(APP_CSS_SOURCE.contains("grid-template-columns: repeat(2, minmax(0, 1fr))"));
        assert!(APP_CSS_SOURCE.contains(".hardware-page"));
        assert!(APP_CSS_SOURCE.contains(".device-bento"));
        assert!(APP_CSS_SOURCE.contains("grid-template-columns: repeat(12, minmax(0, 1fr))"));
        assert!(APP_CSS_SOURCE.contains("grid-area: logo"));
        assert!(APP_CSS_SOURCE.contains("grid-area: readback"));
        assert!(APP_CSS_SOURCE.contains("grid-template-rows: 374.046px minmax(0, 1fr) 40px"));
        assert!(APP_CSS_SOURCE.contains("grid-template-columns: repeat(5, minmax(0, 1fr))"));
        assert!(hardware_note_rule.contains("grid-column: 1 / -1"));
        assert!(hardware_note_rule.contains("white-space: nowrap"));
        assert_eq!(
            super::text(Language::English, super::MessageId::AppHardwarePanel019,),
            "Read-only kernel and firmware data; unavailable values are not inferred."
        );
        assert!(APP_CSS_SOURCE.contains(".spark-chart"));
        assert!(APP_CSS_SOURCE.contains("overflow: hidden"));
        assert!(source.contains("view_box: \"0 0 100 46\""));
        assert!(source.contains("\"aria-label\": \"{primary_label}: {primary_value}\""));
        assert!(!APP_CSS_SOURCE.contains(".platform-basics"));
    }

    #[test]
    fn every_page_ends_on_the_same_forty_pixel_status_tile() {
        let primary_rule = css_rule(".primary-panel {");
        let metrics_rule = css_rule(".metrics-content {");
        let hardware_rule = css_rule(".hardware-page {");
        let device_rule = css_rule(".device-bento {");
        let platform_page_rule = css_rule(".platform-page {");

        assert!(primary_rule.contains("200px 40px"));
        assert!(metrics_rule.contains("minmax(0, 1fr) 40px"));
        assert!(hardware_rule.contains("minmax(0, 1fr) 40px"));
        assert!(device_rule.contains("repeat(4, minmax(0, 1fr)) 40px"));
        assert!(platform_page_rule.contains("height: 100%"));
        assert!(!platform_page_rule.contains("calc("));

        for selector in [
            ".status-line {",
            ".throttle-state {",
            ".hardware-note {",
            ".platform-readback {",
        ] {
            let rule = css_rule(selector);
            assert!(rule.contains("height: 40px"), "{selector}");
            assert!(rule.contains("border:"), "{selector}");
        }
    }

    #[test]
    fn transient_advanced_errors_reuse_fixed_status_tiles_without_reflow() {
        let production = production_source();

        assert!(production.contains("\"platform-readback warning\""));
        assert!(production.contains("\"throttle-state telemetry-error\""));
        assert!(!production.contains("class: \"platform-error\""));
        assert!(!production.contains("class: \"telemetry-warning\""));
        assert!(!APP_CSS_SOURCE.contains(".platform-page.has-error"));
    }

    #[test]
    fn device_controls_are_two_row_tiles_and_color_wells_are_large() {
        let device_control_rule =
            css_rule(".device-bento .setting-toggle,\n.device-bento .usb-charging-control {");
        let color_rule =
            css_rule(".color-input input[type=\"color\"],\n.logo-color input[type=\"color\"] {");
        let production = production_source();

        assert!(device_control_rule.contains("grid-template-columns: minmax(0, 1fr)"));
        assert!(device_control_rule.contains("grid-template-rows: minmax(0, 1fr) 40px"));
        assert!(color_rule.contains("width: 100%"));
        assert!(color_rule.contains("height: 30px"));
        assert!(color_rule.contains("border-radius: 6px"));
        assert!(
            APP_CSS_SOURCE.contains(".logo-color { grid-template-columns: minmax(0, 1fr) 72px; }")
        );
        assert_eq!(
            super::text(Language::Czech, super::MessageId::AppAdvancedPanel013,),
            "Příkon GPU / limit"
        );
        assert_eq!(
            super::text(Language::English, super::MessageId::AppAdvancedPanel013,),
            "GPU power / limit"
        );
        assert!(!production.contains("GPU výkon a takty"));
    }
}
