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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Language {
    Czech,
    #[default]
    English,
}

impl Language {
    fn toggle(self) -> Self {
        match self {
            Self::Czech => Self::English,
            Self::English => Self::Czech,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Czech => "CZ",
            Self::English => "EN",
        }
    }

    fn html_code(self) -> &'static str {
        match self {
            Self::Czech => "cs",
            Self::English => "en",
        }
    }
}

fn tr(language: Language, czech: &'static str, english: &'static str) -> &'static str {
    match language {
        Language::Czech => czech,
        Language::English => english,
    }
}

fn localized_status(language: Language, status: &str) -> String {
    if language == Language::Czech {
        return status.to_string();
    }
    match status {
        "Ovládání Acer připojeno" => "Acer controls connected".to_string(),
        "Ovládání Acer + NVIDIA připojeno" => "Acer + NVIDIA controls connected".to_string(),
        "Připojena telemetrie jen pro čtení" => "Read-only telemetry connected".to_string(),
        "Připojuji ovládání" => "Connecting controls".to_string(),
        "Platforma znovu načtena" => "Platform state refreshed".to_string(),
        "Nastavení potvrzeno firmwarem" => "Settings confirmed by firmware".to_string(),
        "Nastavení podsvícení potvrzeno firmwarem" => {
            "Lighting confirmed by firmware".to_string()
        }
        "Použito · stav nelze přečíst" => "Applied · state readback unavailable".to_string(),
        "Zapisuji a ověřuji firmware" => "Writing and verifying firmware".to_string(),
        _ if status.starts_with("Profil potvrzen: ") => {
            status.replacen("Profil potvrzen:", "Profile verified:", 1)
        }
        _ if status.starts_with("GPU profil není synchronní:") => status.replacen(
            "GPU profil není synchronní:",
            "GPU profile is out of sync:",
            1,
        ),
        _ => status.to_string(),
    }
}

fn compact_status(language: Language, status: &str) -> String {
    for (czech, english) in [
        ("Telemetrie se obnovuje", "Telemetry reconnecting"),
        ("Telemetrie se připojuje", "Telemetry connecting"),
    ] {
        if status.contains(czech) || status.contains(english) {
            return tr(language, czech, english).to_string();
        }
    }

    let profile = status
        .strip_prefix("Profil potvrzen: Acer ")
        .or_else(|| status.strip_prefix("Profile verified: Acer "))
        .and_then(|details| details.split(" ·").next());
    if let Some(profile) = profile {
        let (czech, english) = match profile {
            "low-power" => ("Eco potvrzeno", "Eco verified"),
            "quiet" => ("Tichý potvrzen", "Quiet verified"),
            "balanced" => ("Balanc potvrzen", "Balanced verified"),
            "balanced-performance" => ("Výkon potvrzen", "Performance verified"),
            "performance" => ("Turbo potvrzeno", "Turbo verified"),
            _ => ("Profil potvrzen", "Profile verified"),
        };
        return tr(language, czech, english).to_string();
    }

    for (source_czech, source_english, compact_czech, compact_english) in [
        (
            "Nastavení potvrzeno firmwarem",
            "Settings confirmed by firmware",
            "Nastavení potvrzeno",
            "Settings verified",
        ),
        (
            "Nastavení podsvícení potvrzeno firmwarem",
            "Lighting confirmed by firmware",
            "Podsvícení potvrzeno",
            "Lighting verified",
        ),
        (
            "Použito · stav nelze přečíst",
            "Applied · state readback unavailable",
            "Naposledy použito",
            "Last applied",
        ),
        (
            "Zapisuji a ověřuji firmware",
            "Writing and verifying firmware",
            "Ověřuji nastavení",
            "Verifying settings",
        ),
        (
            "Platforma znovu načtena",
            "Platform state refreshed",
            "Platforma obnovena",
            "Platform refreshed",
        ),
    ] {
        if status == source_czech || status == source_english {
            return tr(language, compact_czech, compact_english).to_string();
        }
    }

    if status.starts_with("Částečné capabilities:") || status.starts_with("Partial capabilities:")
    {
        return tr(language, "Částečný readback", "Partial readback").to_string();
    }

    let mismatch = status
        .strip_prefix("GPU profil není synchronní:")
        .or_else(|| status.strip_prefix("GPU profile is out of sync:"));
    if let Some(mismatch) = mismatch {
        let values = mismatch
            .trim()
            .replace("core ", "")
            .replace("VRAM ", "")
            .replace(" / ", "/");
        return format!("{}: {values}", tr(language, "GPU nesedí", "GPU mismatch"));
    }

    let lower = status.to_ascii_lowercase();
    if lower.contains("rollback") && lower.contains("failed") {
        return tr(language, "Rollback selhal", "Rollback failed").to_string();
    }
    if lower.contains("readback") || lower.contains("verification failed") {
        return tr(
            language,
            "Ověření stavu selhalo",
            "State verification failed",
        )
        .to_string();
    }
    if lower.contains("unsupported") || lower.contains("not supported") {
        return tr(
            language,
            "Firmware funkci nepodporuje",
            "Unsupported by firmware",
        )
        .to_string();
    }
    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("control socket")
    {
        return tr(
            language,
            "Řídicí služba neodpovídá",
            "Control service unavailable",
        )
        .to_string();
    }

    if status.chars().count() > 28 {
        tr(language, "Podrobnosti nahoře", "Details above").to_string()
    } else {
        status.to_string()
    }
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
            status_message: "Připojuji ovládání".to_string(),
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
    Reconnecting,
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
    let mut language = use_signal(Language::default);
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
                        state.view.telemetry_health = TelemetryHealth::Reconnecting;
                        state.view.telemetry_error = Some(format!(
                            "{message}; další pokus za {} s",
                            retry_after.as_secs()
                        ));
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
                                message: format!("acer_wmi telemetry není dostupná: {error}"),
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
            WindowChrome {}
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
                        on_language: move |_| language.set(language().toggle()),
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
fn WindowChrome() -> Element {
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
                    title: "Minimize",
                    onmousedown: move |event| event.stop_propagation(),
                    onclick: move |_| minimize_window.set_minimized(true),
                    span { class: "minimize-mark" }
                }
                button {
                    class: "window-button close",
                    r#type: "button",
                    title: "Close",
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
            view.status_message = "Zapisuji a ověřuji firmware".to_string();
        }
    }
    true
}

fn fail_control_request(view: &mut AppState, request: ControlRequest, error: String) {
    view.control_busy = false;
    if request.action.touches_platform() {
        view.platform_busy = false;
        view.platform_error = Some(error.clone());
    }
    let initial_connection_failed =
        request.action == ControlAction::Initialize && view.capabilities.is_none();
    if initial_connection_failed {
        view.controls_enabled = false;
    }
    if request.foreground || initial_connection_failed {
        view.health = HealthState::Warning;
        view.status_message = error;
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
            if matches!(&update.request.action, ControlAction::Profile(_)) {
                view.profile_sync = ProfileTelemetrySync::default();
            }
            if update.request.action.touches_platform() {
                view.platform_error = Some(error.clone());
            }
            if update.request.action == ControlAction::Initialize && view.capabilities.is_none() {
                view.controls_enabled = false;
            }
            if update.request.foreground || update.request.action == ControlAction::Initialize {
                view.health = HealthState::Warning;
                view.status_message = error;
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
                    view.hardware_error = Some(error.clone());
                    diagnostics.push(format!("hardware: {error}"));
                }
            }
            if diagnostics.is_empty() {
                let status = if acer_controls {
                    "Ovládání Acer připojeno"
                } else {
                    "Připojena telemetrie jen pro čtení"
                };
                finish_control_success(view, status);
            } else {
                view.health = HealthState::Warning;
                view.status_message = format!("Částečné capabilities: {}", diagnostics.join("; "));
            }
        }
        ControlOutcome::FanMode(mode) => {
            view.fan_mode = mode;
            finish_control_success(view, "Nastavení potvrzeno firmwarem");
        }
        ControlOutcome::ManualFans(request) => {
            view.fan_mode = FanMode::Manual;
            view.cpu_fan_percent = request.cpu_percent;
            view.gpu_fan_percent = request.gpu_percent;
            finish_control_success(view, "Nastavení potvrzeno firmwarem");
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
            finish_control_success(view, &profile_receipt_status(&receipt));
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
            finish_control_success(view, "Nastavení podsvícení potvrzeno firmwarem");
        }
        ControlOutcome::Platform { action, state } => {
            let read_error = store_platform_state(view, state);
            if let Some(error) = read_error {
                view.health = HealthState::Warning;
                view.status_message = format!("Částečné capabilities: platform: {error}");
            } else if update.request.foreground {
                let status = match action {
                    PlatformAction::Refresh => "Platforma znovu načtena",
                    _ => "Nastavení potvrzeno firmwarem",
                };
                finish_control_success(view, status);
            }
        }
        ControlOutcome::Refresh {
            capabilities,
            lighting,
            platform,
        } => {
            let (_, diagnostics) =
                apply_capability_snapshot(view, capabilities, lighting, platform);
            if diagnostics.is_empty() {
                finish_control_success(view, "Platforma znovu načtena");
            } else {
                view.health = HealthState::Warning;
                view.status_message = format!("Částečné capabilities: {}", diagnostics.join("; "));
            }
        }
    }
}

fn apply_capability_snapshot(
    view: &mut AppState,
    capabilities: ControlCapabilities,
    lighting: Result<KeyboardLightingState, String>,
    platform: Result<PlatformState, String>,
) -> (bool, Vec<String>) {
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
            view.lighting_error = Some(error.clone());
            diagnostics.push(format!("RGB: {error}"));
        }
    }
    match platform {
        Ok(platform) => {
            if let Some(error) = store_platform_state(view, platform) {
                diagnostics.push(format!("platform: {error}"));
            }
        }
        Err(error) => {
            view.platform_error = Some(error.clone());
            diagnostics.push(format!("platform: {error}"));
        }
    }
    (acer_controls, diagnostics)
}

fn store_platform_state(view: &mut AppState, platform: PlatformState) -> Option<String> {
    let error = platform_read_error_summary(platform.read_error_mask);
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

fn platform_read_error_summary(mask: u8) -> Option<String> {
    if mask == 0 {
        return None;
    }
    let mut names = Vec::new();
    for (bit, name) in [
        (READ_ERROR_BATTERY_LIMIT, "battery limit"),
        (READ_ERROR_BATTERY_CALIBRATION, "battery calibration"),
        (READ_ERROR_USB_CHARGING, "USB charging"),
        (READ_ERROR_KEYBOARD_TIMEOUT, "keyboard timeout"),
        (READ_ERROR_BOOT_SOUND, "boot sound"),
        (READ_ERROR_LCD_OVERRIDE, "LCD override"),
        (READ_ERROR_REAR_LOGO, "rear logo"),
    ] {
        if mask & bit != 0 {
            names.push(name);
        }
    }
    Some(format!("readback failed: {}", names.join(", ")))
}

fn finish_control_success(view: &mut AppState, status: &str) {
    view.health = HealthState::Healthy;
    view.status_message = status.to_string();
}

fn lighting_apply_status(state_readable: bool) -> &'static str {
    if state_readable {
        "Nastavení potvrzeno firmwarem"
    } else {
        "Použito · stav nelze přečíst"
    }
}

fn profile_receipt_status(receipt: &ProfileApplyReceipt) -> String {
    let offsets = match receipt.gpu_offsets {
        GpuOffsetState::Unavailable => "nedostupné".to_string(),
        GpuOffsetState::Reset => "+0/+0 MHz".to_string(),
        GpuOffsetState::OemTurbo => "+100/+200 MHz".to_string(),
        GpuOffsetState::CustomOrPartial => "vlastní/částečné".to_string(),
    };
    let power = receipt.power.as_ref().map_or_else(
        || "GPU limit nedostupný".to_string(),
        |power| {
            format!(
                "GPU {}/{} W",
                format_milliwatts(power.enforced_limit_mw),
                format_milliwatts(power.maximum_limit_mw)
            )
        },
    );
    format!(
        "Profil potvrzen: Acer {} · VF {offsets} · {power}",
        receipt.firmware_profile
    )
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
    if view
        .status_message
        .starts_with("GPU profil není synchronní:")
    {
        view.health = HealthState::Healthy;
        view.status_message = if nvidia_offsets_available {
            "Ovládání Acer + NVIDIA připojeno"
        } else {
            "Ovládání Acer připojeno"
        }
        .to_string();
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
            view.status_message =
                format!("GPU profil není synchronní: core {core:+} / VRAM {memory:+} MHz");
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
            Self::Auto => "Auto",
            Self::Manual => tr(language, "Ručně", "Manual"),
            Self::Maximum => "Maximum",
        }
    }

    fn hint(self, language: Language) -> &'static str {
        match self {
            Self::Auto => tr(
                language,
                "Firmware řídí chlazení",
                "Firmware controls cooling",
            ),
            Self::Manual => tr(language, "Vlastní pevné otáčky", "Custom fixed fan speed"),
            Self::Maximum => tr(
                language,
                "Plný výkon ventilátorů",
                "Maximum fan performance",
            ),
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
            Self::LowPower => "Eco",
            Self::Quiet => tr(language, "Tichý", "Quiet"),
            Self::Balanced => tr(language, "Balanc", "Balanced"),
            Self::Performance => tr(language, "Výkon", "Performance"),
            Self::Turbo => "Turbo",
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
            return tr(language, "Nedostupné", "Unavailable").to_string();
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
            Self::Healthy => tr(language, "Připraveno", "Ready"),
            Self::Applying => tr(language, "Nastavuji", "Applying"),
            Self::Warning => tr(language, "Zkontrolovat", "Check"),
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
    pub lighting_error: Option<String>,
    pub platform: Option<PlatformState>,
    pub platform_busy: bool,
    pub platform_error: Option<String>,
    pub hardware_error: Option<String>,
    pub platform_revision: u64,
    rear_logo_last_nonzero_brightness: u8,
    pub control_busy: bool,
    pub health: HealthState,
    pub status_message: String,
    pub controls_enabled: bool,
    telemetry_health: TelemetryHealth,
    telemetry_error: Option<String>,
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
            status_message: "Ovládání Acer připojeno".into(),
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
    let localized_control_status = localized_status(language, &state.status_message);
    let telemetry_status = match state.telemetry_health {
        TelemetryHealth::Online => None,
        TelemetryHealth::Connecting => Some(tr(
            language,
            "Telemetrie se připojuje",
            "Telemetry connecting",
        )),
        TelemetryHealth::Reconnecting => Some(tr(
            language,
            "Telemetrie se obnovuje",
            "Telemetry reconnecting",
        )),
    };
    let localized_status_message = telemetry_status.map_or_else(
        || localized_control_status.clone(),
        |status| format!("{localized_control_status} · {status}"),
    );
    let status_title = state.telemetry_error.as_deref().map_or_else(
        || localized_status_message.clone(),
        |error| format!("{localized_status_message}: {error}"),
    );
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

            section { class: "primary-panel", "aria-label": tr(language, "Ovládání notebooku", "Laptop controls"),
                AppHeader {
                    product_name: state.product_name,
                    model_name: state.model_name,
                    health: displayed_health,
                    status_message: status_title,
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

                CoolingOverview { telemetry: telemetry.clone() }

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
                    status_message: localized_status_message,
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
                    platform_error: state.platform_error,
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
                    title: tr(language, "O aplikaci a dokumentace", "About and documentation"),
                    "aria-label": tr(language, "Otevřít informace a dokumentaci", "Open information and documentation"),
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
                    title: tr(language, "Přepnout do angličtiny", "Switch to Czech"),
                    onclick: move |_| on_language.call(()),
                    "{language.code()}"
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
                        tr(language, "Skrýt rozšířený panel", "Hide advanced panel")
                    } else {
                        tr(language, "Zobrazit rozšířený panel", "Show advanced panel")
                    },
                    onclick: move |_| on_advanced.call(!advanced_open),
                    span { class: "toggle-indicator" }
                    {tr(language, "Rozšířené", "Advanced")}
                }
            }
        }
    }
}

#[component]
fn QuickStrip(telemetry: Telemetry, profile: String, language: Language) -> Element {
    rsx! {
        section { class: "quick-strip", "aria-label": tr(language, "Systémová telemetrie", "System telemetry"),
            MetricPill {
                label: "CPU",
                value: temperature(telemetry.cpu_temperature_c),
                level: temperature_level(telemetry.cpu_temperature_c),
            }
            MetricPill {
                label: tr(language, "ZÁTĚŽ", "LOAD"),
                value: percent(telemetry.cpu_load_percent),
                level: "neutral",
            }
            div { class: "profile-pill",
                span { {tr(language, "Profil", "Profile")} }
                strong { "{profile}" }
            }
            MetricPill {
                label: "GPU",
                value: temperature(telemetry.gpu_temperature_c),
                level: temperature_level(telemetry.gpu_temperature_c),
            }
            MetricPill {
                label: tr(language, "ZÁTĚŽ", "LOAD"),
                value: if telemetry.gpu_sleeping {
                    tr(language, "Spí", "Sleeping").to_string()
                } else {
                    percent(telemetry.gpu_load_percent)
                },
                level: "neutral",
            }
        }
    }
}

#[component]
fn CoolingOverview(telemetry: Telemetry) -> Element {
    rsx! {
        section { class: "gauge-grid", "aria-label": "Cooling telemetry",
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
        CapabilityLightingTarget::Keyboard => tr(language, "Klávesnice", "Keyboard"),
        CapabilityLightingTarget::CoverLogo => tr(language, "Logo víka", "Cover logo"),
        CapabilityLightingTarget::RearLogo => tr(language, "Zadní logo", "Rear logo"),
        CapabilityLightingTarget::Lightbar => tr(language, "Světelná lišta", "Lightbar"),
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
    lighting_error: Option<String>,
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
        Some(CapabilityProfileBackend::Kernel) => tr(
            language,
            "Volby profilů poskytuje živé rozhraní Linux kernelu.",
            "Profile choices come from the live Linux kernel interface.",
        ),
        Some(CapabilityProfileBackend::AcerGamingWmi) => tr(
            language,
            "Známé příkazy Acer Gaming-WMI; každá změna se ověřuje zpětným čtením.",
            "Known Acer Gaming-WMI commands; every change is verified by readback.",
        ),
        None => tr(
            language,
            "Firmware profily nejsou dostupné.",
            "Firmware profiles are unavailable.",
        ),
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
        .map_or(tr(language, "Podsvícení", "Backlight"), |device| {
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
        tr(language, "Stav z firmware", "Firmware state")
    } else if lighting_state_last_applied {
        tr(language, "Naposledy použito", "Last applied")
    } else {
        tr(language, "Stav neznámý", "State unknown")
    };

    rsx! {
        section { class: "control-dock", "aria-label": tr(language, "Ovládací centrum", "Control center"),
            div {
                class: "profile-switch",
                style: "grid-template-columns:repeat({profile_count},minmax(0,1fr))",
                title: profile_source_hint,
                "aria-label": tr(language, "Výkonnostní profil Acer", "Acer performance profile"),
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
                    {tr(language, "Ventilátory", "Fans")}
                }
                if lighting_devices.is_empty() {
                    button {
                        class: if dock_tab() == DockTab::Keyboard { "dock-tab active" } else { "dock-tab" },
                        r#type: "button",
                        role: "tab",
                        "aria-selected": dock_tab() == DockTab::Keyboard,
                        disabled: !lighting_available,
                        title: lighting_error.as_deref().unwrap_or(if lighting_available {
                            tr(language, "RGB klávesnice", "RGB keyboard")
                        } else {
                            tr(language, "RGB modul není dostupný", "RGB module is unavailable")
                        }),
                        onclick: move |_| dock_tab.set(DockTab::Keyboard),
                        {tr(language, "Klávesnice", "Keyboard")}
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
                        div { class: "lighting-power", "aria-label": tr(language, "Napájení podsvícení klávesnice", "Keyboard backlight power"),
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
                                    {tr(language, "Zap", "On")}
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
                                    {tr(language, "Vyp", "Off")}
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
                                span { {tr(language, "Jas", "Brightness")} }
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
                                        {tr(language, "Statické", "Static")}
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
                                        {tr(language, "Dech", "Breathing")}
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
                                        {tr(language, "Neon", "Neon")}
                                    }
                                }
                            }
                        }
                    }
                } else {
                    div { class: if manual { "fan-panel manual" } else { "fan-panel" },
                        if fan_control_available {
                            div { class: "mode-switch", "aria-label": tr(language, "Režim ventilátorů", "Fan mode"),
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
                                    {tr(language, "Použít", "Apply")}
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
                                        {tr(
                                            language,
                                            "Řízení ventilátorů není dostupné",
                                            "Fan control unavailable",
                                        )}
                                    } else if selected_fan_mode == FanMode::Maximum {
                                        {tr(
                                            language,
                                            "Vybrány maximální otáčky ventilátorů",
                                            "Maximum fan RPM selected",
                                        )}
                                    } else {
                                        {tr(
                                            language,
                                            "Vybráno automatické řízení otáček",
                                            "Automatic RPM control selected",
                                        )}
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
    health: HealthState,
    language: Language,
) -> Element {
    let class = match health {
        HealthState::Healthy => "status-line healthy",
        HealthState::Applying => "status-line applying",
        HealthState::Warning => "status-line warning",
    };
    let displayed_status = compact_status(language, &status_message);
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
        Some(true) => tr(language, "Zap", "On"),
        Some(false) => tr(language, "Vyp", "Off"),
        None if read_failed => tr(language, "Chyba čtení", "Read error"),
        None => tr(language, "Nepodporováno", "Unsupported"),
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
        tr(language, "Důvody omezení taktu", "Clock / throttle reasons")
    };
    let throttle_summary = if telemetry.gpu_sleeping {
        tr(language, "Spí", "Sleeping").to_string()
    } else if telemetry_error.is_some() {
        tr(language, "Chyba čtení", "Readback error").to_string()
    } else {
        throttle
    };
    let throttle_title = if telemetry.gpu_sleeping {
        "RTD3"
    } else {
        telemetry_error.unwrap_or("")
    };
    let gpu_workload = if telemetry.gpu_sleeping {
        tr(language, "Spí", "Sleeping").to_string()
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
        aside { class: "advanced-panel", "aria-label": tr(language, "Rozšířené systémové informace", "Advanced system information"),
            div { class: "advanced-heading",
                div { class: "advanced-tabs", role: "tablist",
                    button {
                        class: if tab() == AdvancedTab::Metrics { "active" } else { "" },
                        role: "tab",
                        "aria-selected": tab() == AdvancedTab::Metrics,
                        onclick: move |_| tab.set(AdvancedTab::Metrics),
                        {tr(language, "Metriky", "Metrics")}
                    }
                    button {
                        class: if tab() == AdvancedTab::Hardware { "active" } else { "" },
                        role: "tab",
                        "aria-selected": tab() == AdvancedTab::Hardware,
                        onclick: move |_| tab.set(AdvancedTab::Hardware),
                        {tr(language, "Hardware", "Hardware")}
                    }
                    button {
                        class: if tab() == AdvancedTab::Platform { "active" } else { "" },
                        role: "tab",
                        "aria-selected": tab() == AdvancedTab::Platform,
                        onclick: move |_| tab.set(AdvancedTab::Platform),
                        {tr(language, "Zařízení", "Device")}
                    }
                }
            }

            if tab() == AdvancedTab::Metrics {
                div { class: "advanced-content metrics-content",
                div { class: "advanced-kpis",
                AdvancedMetric {
                    label: tr(language, "Zátěž CPU", "CPU workload"),
                    value: percent(telemetry.cpu_load_percent),
                    detail: temperature(telemetry.cpu_temperature_c),
                }
                AdvancedMetric {
                    label: "RAM",
                    value: memory_pair(telemetry.memory_used_mib, telemetry.memory_total_mib),
                    detail: percent(ratio_percent(telemetry.memory_used_mib, telemetry.memory_total_mib)),
                }
                AdvancedMetric {
                    label: tr(language, "Zátěž GPU", "GPU workload"),
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
                    label: tr(language, "Takt VRAM", "VRAM clock"),
                    value: frequency(telemetry.gpu_memory_clock_mhz),
                    detail: gpu_offset_detail("VF MEM", telemetry.gpu_memory_offset_mhz),
                }
                AdvancedMetric {
                    label: tr(language, "Příkon GPU", "GPU power"),
                    value: power(telemetry.gpu_power_w),
                    detail: format!("LIMIT {}", power(telemetry.gpu_enforced_power_limit_w)),
                }
                AdvancedMetric {
                    label: tr(language, "Chlazení", "Cooling"),
                    value: format!("{}/{} RPM", optional_u32(telemetry.cpu_fan_rpm), optional_u32(telemetry.gpu_fan_rpm)),
                    detail: cooling_detail,
                }
                }

                div { class: "advanced-charts",
                DualHistoryChart {
                    language,
                    title: tr(language, "Systémová zátěž", "System load"),
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
                    title: tr(language, "Teploty", "Temperatures"),
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
                    title: tr(language, "Příkon GPU / limit", "GPU power / limit"),
                    primary_label: tr(language, "PŘÍKON", "POWER"),
                    primary_value: power(telemetry.gpu_power_w),
                    primary_points: gpu_power_points,
                    secondary_label: tr(language, "LIMIT", "LIMIT"),
                    secondary_value: power(telemetry.gpu_enforced_power_limit_w),
                    secondary_points: gpu_power_limit_points,
                    y_min: "0 W".to_string(),
                    y_max: format!("{power_ceiling:.0} W"),
                    history_seconds,
                }
                DualHistoryChart {
                    language,
                    title: tr(language, "Domény taktu GPU", "GPU clock domains"),
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
    let unknown = tr(language, "Nezjištěno", "Unavailable");
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
                        h3 { {tr(language, "Procesor", "Processor")} }
                    }
                    span { class: "read-only-badge", {tr(language, "Jen čtení", "Read only")} }
                }
                div { class: "hardware-model", title: "{cpu_model}", "{cpu_model}" }
                div { class: "hardware-facts",
                    HardwareFact {
                        label: tr(language, "Aktivní jádra", "Active cores"),
                        value: optional_hardware_number(info.cpu.physical_cores),
                    }
                    HardwareFact {
                        label: tr(language, "Online vlákna", "Online threads"),
                        value: optional_hardware_number(info.cpu.logical_processors),
                    }
                    HardwareFact {
                        label: tr(language, "Aktivní P / E", "Active P / E cores"),
                        value: core_mix,
                    }
                    HardwareFact {
                        label: tr(language, "Architektura", "Architecture"),
                        value: info.cpu.architecture.as_deref().unwrap_or("—").to_string(),
                    }
                    HardwareFact {
                        label: tr(language, "Rodina CPU", "CPU family"),
                        value: optional_hardware_number(info.cpu.family),
                    }
                    HardwareFact {
                        label: "L3 cache",
                        value: hardware_cache(info.cpu.l3_cache_kib),
                    }
                    HardwareFact {
                        label: tr(language, "Aktuální takt", "Current clock"),
                        value: hardware_frequency(info.cpu.current_frequency_mhz),
                    }
                    HardwareFact {
                        label: tr(language, "Maximální takt", "Maximum clock"),
                        value: hardware_frequency(info.cpu.maximum_frequency_mhz),
                    }
                }
            }

            article { class: "hardware-card gpu-hardware",
                div { class: "hardware-card-heading",
                    div {
                        span { class: "hardware-kind", "GPU" }
                        h3 { {tr(language, "Grafika", "Graphics")} }
                    }
                    span { class: "read-only-badge", {tr(language, "Jen čtení", "Read only")} }
                }
                div { class: "hardware-model", title: "{gpu_model}", "{gpu_model}" }
                div { class: "hardware-facts",
                    HardwareFact {
                        label: "VRAM",
                        value: hardware_capacity(info.gpu.vram_total_mib),
                    }
                    HardwareFact {
                        label: tr(language, "Ovladač", "Driver"),
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
                        label: tr(language, "Grafický takt", "Graphics clock"),
                        value: hardware_frequency(info.gpu.current_graphics_clock_mhz),
                    }
                    HardwareFact {
                        label: "GPU max",
                        value: hardware_frequency(info.gpu.maximum_graphics_clock_mhz),
                    }
                    HardwareFact {
                        label: tr(language, "Takt VRAM", "VRAM clock"),
                        value: hardware_frequency(info.gpu.current_memory_clock_mhz),
                    }
                    HardwareFact {
                        label: "VRAM max",
                        value: hardware_frequency(info.gpu.maximum_memory_clock_mhz),
                    }
                }
            }

            article { class: "hardware-card memory-hardware",
                div { class: "hardware-card-heading",
                    div {
                        span { class: "hardware-kind", "RAM" }
                        h3 { {tr(language, "Systémová paměť", "System memory")} }
                    }
                    span { class: "read-only-badge", {tr(language, "Jen čtení", "Read only")} }
                }
                div { class: "hardware-facts memory-facts",
                    HardwareFact {
                        label: tr(language, "Celkem", "Total"),
                        value: hardware_capacity(info.memory.total_mib),
                    }
                    HardwareFact {
                        label: tr(language, "Typ", "Type"),
                        value: memory_type,
                    }
                    HardwareFact {
                        label: tr(language, "Rychlost", "Speed"),
                        value: info.memory.speed_mt_s.map(|value| format!("{value} MT/s")).unwrap_or_else(|| "—".to_string()),
                    }
                    HardwareFact {
                        label: tr(language, "Kanály", "Channels"),
                        value: optional_hardware_number(info.memory.channels),
                    }
                    HardwareFact {
                        label: tr(language, "Moduly", "Modules"),
                        value: optional_hardware_number(info.memory.modules),
                    }
                }
            }
            p { class: "hardware-note",
                {tr(
                    language,
                    "Data pouze pro čtení z kernelu a firmware; nedostupné hodnoty se neodhadují.",
                    "Read-only kernel and firmware data; unavailable values are not inferred.",
                )}
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
    let unavailable_message = error.clone().unwrap_or_else(|| {
        tr(
            language,
            "Čekám na firmware readback",
            "Waiting for firmware readback",
        )
        .to_string()
    });
    let Some(state) = state else {
        return rsx! {
            section { class: "advanced-content platform-page empty",
                h3 { {tr(language, "Platformní funkce nejsou načtené", "Platform features are not loaded")} }
                p { "{unavailable_message}" }
                button {
                    class: "apply-button",
                    disabled: busy,
                    onclick: move |_| on_action.call(PlatformAction::Refresh),
                    {tr(language, "Načíst znovu", "Reload")}
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
        Some(true) => tr(language, "Zastavit", "Stop"),
        Some(false) => tr(language, "Spustit", "Start"),
        None if calibration_read_failed => tr(language, "Chyba čtení", "Read error"),
        None => tr(language, "Nepodporováno", "Unsupported"),
    };
    let battery_live = battery_live_status(battery_status, battery_percent, language);
    let calibration_detail = if calibration_active {
        format!(
            "{} · {battery_live}",
            tr(language, "Kalibrace aktivní", "Calibration active")
        )
    } else {
        tr(
            language,
            "Firmware plný cyklus baterie",
            "Firmware full battery cycle",
        )
        .to_string()
    };
    let usb_only = usb_power_online == Some(true) && ac_online != Some(true);
    let calibration_start_allowed = ac_online == Some(true) && !usb_only;
    let (power_state_class, power_state_text) = if ac_online == Some(true) {
        (
            "calibration-power-state ready",
            tr(
                language,
                "AC napájení je připojené. Ponech adaptér připojený po celý cyklus.",
                "AC power is connected. Keep the adapter connected for the entire cycle.",
            ),
        )
    } else if usb_only {
        (
            "calibration-power-state warning",
            tr(
                language,
                "ASense z bezpečnostních důvodů nespustí kalibraci jen přes USB-C. Připoj AC adaptér.",
                "ASense does not start calibration on USB-C-only power as a safety policy. Connect an AC adapter.",
            ),
        )
    } else if ac_online == Some(false) {
        (
            "calibration-power-state warning",
            tr(
                language,
                "AC adaptér je odpojený. Před startem jej připoj.",
                "The AC adapter is disconnected. Connect it before starting.",
            ),
        )
    } else {
        (
            "calibration-power-state warning",
            tr(
                language,
                "Stav AC nelze ověřit. Před startem připoj adaptér a ponech jej připojený.",
                "AC state could not be verified. Connect an adapter and keep it connected.",
            ),
        )
    };
    let readback_text = if busy {
        tr(language, "Ověřuji", "Verifying")
    } else if error.is_some() || state.read_error_mask != 0 {
        tr(language, "Chyba čtení", "Read error")
    } else {
        tr(language, "Ověřeno", "Verified")
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
                    label: tr(language, "Limit baterie", "Battery limit"),
                    detail: tr(language, "Max. 80 %", "Maximum 80%"),
                    value: state.battery_limit,
                    read_failed: state.read_error_mask & READ_ERROR_BATTERY_LIMIT != 0,
                    disabled: busy,
                    language,
                    on_change: move |enabled| on_action.call(PlatformAction::BatteryLimit(enabled)),
                }
                div { class: "usb-charging-control device-usb-charging",
                    div { class: "setting-copy",
                        strong { {tr(language, "USB při vypnutí", "USB while powered off")} }
                        span { {tr(language, "Vypnout při kapacitě", "Stop at battery level")} }
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
                        strong { {tr(language, "Kalibrace baterie", "Battery calibration")} }
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
                    label: tr(language, "Zvuk při startu", "Boot sound"),
                    detail: tr(language, "Zvuk Predator animace", "Predator boot animation sound"),
                    value: state.boot_sound,
                    read_failed: state.read_error_mask & READ_ERROR_BOOT_SOUND != 0,
                    disabled: busy,
                    language,
                    on_change: move |enabled| on_action.call(PlatformAction::BootSound(enabled)),
                }
                SettingToggle {
                    class_name: "device-lcd-override",
                    label: "LCD override",
                    detail: tr(language, "Firmware override displeje", "Firmware display override"),
                    value: state.lcd_override,
                    read_failed: state.read_error_mask & READ_ERROR_LCD_OVERRIDE != 0,
                    disabled: busy,
                    language,
                    on_change: move |enabled| on_action.call(PlatformAction::LcdOverride(enabled)),
                }
                SettingToggle {
                    class_name: "device-keyboard-timeout",
                    label: tr(language, "Timeout klávesnice", "Keyboard timeout"),
                    detail: tr(language, "Automatické zhasnutí RGB", "Automatic RGB timeout"),
                    value: state.keyboard_timeout,
                    read_failed: state.read_error_mask & READ_ERROR_KEYBOARD_TIMEOUT != 0,
                    disabled: busy,
                    language,
                    on_change: move |enabled| on_action.call(PlatformAction::KeyboardTimeout(enabled)),
                }
                article { class: "rear-logo-card",
                div { class: "rear-logo-heading",
                    div { class: "setting-copy",
                        strong { {tr(language, "Zadní Predator logo", "Rear Predator logo")} }
                        span { {tr(language, "Napájení, barva a jas", "Power, color and brightness")} }
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
                            {tr(language, "Zap", "On")}
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
                            {tr(language, "Vyp", "Off")}
                        }
                    }
                }
                div { class: "rear-logo-editor",
                    label { class: "logo-color",
                        span { {tr(language, "Barva", "Color")} }
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
                        span { {tr(language, "Jas", "Brightness")} }
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
                        {tr(language, "Použít", "Apply")}
                    }
                }
                }

                div { class: readback_class, title: "{readback_title}",
                    span { "Firmware" }
                    strong { "{readback_text}" }
                    button {
                        disabled: busy,
                        onclick: move |_| on_action.call(PlatformAction::Refresh),
                        {tr(language, "Obnovit", "Refresh")}
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
                            {tr(language, "Spustit kalibraci baterie?", "Start battery calibration?")}
                        }
                        p { id: "calibration-modal-description",
                            {tr(
                                language,
                                "Firmware spustí dlouhý plný cyklus. Ulož práci; notebook během kalibrace nevypínej ani neuspávej.",
                                "Firmware will start a long full cycle. Save your work; do not power off or suspend the laptop during calibration.",
                            )}
                        }
                        div { class: power_state_class,
                            strong { "{power_state_text}" }
                            span { "{battery_live}" }
                        }
                        p { class: "calibration-modal-note",
                            {tr(
                                language,
                                "Firmware neposkytuje procenta ani dekódovaný signál dokončení. Po cyklu stav obnov; zůstane-li aktivní, kalibraci ručně zastav.",
                                "Firmware exposes no percentage or decoded completion signal. Refresh after the cycle; if it remains active, stop calibration manually.",
                            )}
                        }
                        if state.battery_limit == Some(true) {
                            p { class: "calibration-modal-note",
                                {tr(
                                    language,
                                    "Před kalibrací doporučujeme vypnout 80% limit nabíjení.",
                                    "Disable the 80% charge limit before calibration.",
                                )}
                            }
                        }
                        div { class: "calibration-modal-actions",
                            button {
                                class: "modal-cancel",
                                disabled: busy,
                                onclick: move |_| calibration_confirmation.set(false),
                                {tr(language, "Zrušit", "Cancel")}
                            }
                            button {
                                class: "apply-button",
                                disabled: busy || !calibration_start_allowed,
                                onclick: move |_| {
                                    calibration_confirmation.set(false);
                                    on_action.call(PlatformAction::BatteryCalibration(true));
                                },
                                {tr(language, "Spustit kalibraci", "Start calibration")}
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
        Some(BatteryStatus::Charging) => tr(language, "nabíjení", "charging"),
        Some(BatteryStatus::Discharging) => tr(language, "vybíjení", "discharging"),
        Some(BatteryStatus::Full) => tr(language, "plná", "full"),
        Some(BatteryStatus::NotCharging) => tr(language, "nenabíjí", "not charging"),
        Some(BatteryStatus::Unknown) | None => tr(language, "stav neznámý", "state unknown"),
    };
    percent.map_or_else(
        || state.to_string(),
        |percent| format!("{state} · {percent} %"),
    )
}

fn usb_charging_label(mode: UsbCharging, language: Language) -> &'static str {
    match mode {
        UsbCharging::Disabled => tr(language, "Vyp", "Off"),
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
    let history_end = tr(language, "teď", "now");
    let chart_description = if language == Language::Czech {
        format!("Historie {title}, {history_seconds} sekund, osa {y_min} až {y_max}")
    } else {
        format!("History of {title}, {history_seconds} seconds, axis {y_min} to {y_max}")
    };
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
    rsx! {
        label { class: "color-input", title: if language == Language::Czech { format!("Zóna {label}") } else { format!("Zone {label}") },
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
        (_, _, Some(false)) => tr(language, "smíšené", "mixed").to_string(),
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
        return tr(language, "Nedostupné", "Unavailable").to_string();
    };
    let reasons = ClockEventReasons::from_bits(bits);
    if bits == 0 {
        return tr(language, "Žádné omezení", "No limits").to_string();
    }
    if bits == ClockEventReasons::GPU_IDLE {
        return tr(
            language,
            "Žádné omezení · GPU nečinná",
            "No limits · GPU idle",
        )
        .to_string();
    }
    let labels: Vec<&'static str> = [
        (
            ClockEventReasons::GPU_IDLE,
            tr(language, "nečinnost", "idle"),
        ),
        (
            ClockEventReasons::APPLICATION_CLOCKS,
            tr(language, "aplikační takty", "application clocks"),
        ),
        (
            ClockEventReasons::SOFTWARE_POWER_CAP,
            tr(language, "softwarový limit příkonu", "software power cap"),
        ),
        (
            ClockEventReasons::HARDWARE_SLOWDOWN,
            tr(language, "hardwarové zpomalení", "hardware slowdown"),
        ),
        (ClockEventReasons::SYNC_BOOST, "sync boost"),
        (
            ClockEventReasons::SOFTWARE_THERMAL,
            tr(language, "softwarový tepelný limit", "software thermal"),
        ),
        (
            ClockEventReasons::HARDWARE_THERMAL,
            tr(language, "hardwarový tepelný limit", "hardware thermal"),
        ),
        (
            ClockEventReasons::HARDWARE_POWER_BRAKE,
            tr(
                language,
                "hardwarová výkonová brzda",
                "hardware power brake",
            ),
        ),
        (
            ClockEventReasons::DISPLAY_CLOCK,
            tr(language, "limit displeje", "display clock"),
        ),
    ]
    .into_iter()
    .filter_map(|(bit, label)| reasons.contains(bit).then_some(label))
    .collect();
    if labels.is_empty() {
        if language == Language::Czech {
            format!("Neznámý důvod 0x{bits:016x}")
        } else {
            format!("Unknown reason 0x{bits:016x}")
        }
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
        PlatformProfile, ResizeObservation, RuntimeState, TELEMETRY_HISTORY_CAPACITY,
        TITLEBAR_DESIGN_HEIGHT, TelemetryHistory, TelemetryPoint, TelemetrySlot, TelemetryUpdate,
        WORKSPACE_DESIGN_HEIGHT, apply_capability_snapshot, apply_control_update, apply_telemetry,
        aspect_constrained_size, begin_control_request, compact_status, empty_platform_state,
        gpu_offset_detail, graph_points, keyboard_editor_readback, lighting_apply_status,
        lighting_draft_for_device, lighting_mode_visibility, lighting_zone_draft, localized_status,
        logical_window_size, merge_privileged_memory, parse_color_value, parse_lighting_state,
        physical_size_close, power_usage_limit, preferred_lighting_index, rear_logo_state,
        reconcile_profile_telemetry, setting_toggle_text, telemetry_retry_delay,
        workspace_aspect_ratio,
    };
    use crate::control::{
        CapabilityLightingBackend, CapabilityLightingTarget, ControlCapabilities,
        ControlFanCapabilities, ControlLightingDevice, ControlLightingMode, ControlLightingModes,
        ControlPlatformCapabilities, ControlProfileCapabilities, ProfileApplyReceipt,
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
        assert_eq!(runtime.view.status_message, "Připojuji ovládání");
    }

    #[test]
    fn public_release_defaults_to_english() {
        let language = Language::default();

        assert_eq!(language, Language::English);
        assert_eq!(language.code(), "EN");
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
        assert_eq!(state.status_message, "Použito · stav nelze přečíst");
    }

    #[test]
    fn compact_status_keeps_basic_receipts_readable_in_both_languages() {
        let turbo = "Profil potvrzen: Acer performance · VF +100/+200 MHz · GPU 115/140 W";
        assert_eq!(compact_status(Language::Czech, turbo), "Turbo potvrzeno");
        assert_eq!(
            compact_status(
                Language::English,
                "Profile verified: Acer performance · VF +100/+200 MHz · GPU 115/140 W"
            ),
            "Turbo verified"
        );
        assert_eq!(
            compact_status(
                Language::English,
                "Částečné capabilities: platform: readback failed: USB charging"
            ),
            "Partial readback"
        );
        assert_eq!(
            compact_status(
                Language::Czech,
                "GPU profil není synchronní: core +0 / VRAM +200 MHz"
            ),
            "GPU nesedí: +0/+200 MHz"
        );
        assert_eq!(
            compact_status(
                Language::English,
                "an otherwise unknown diagnostic that is deliberately much too long"
            ),
            "Details above"
        );
        for status in [
            "Turbo potvrzeno",
            "Partial readback",
            "GPU nesedí: +0/+200 MHz",
            "Firmware funkci nepodporuje",
            "Control service unavailable",
        ] {
            assert!(status.chars().count() <= 28, "status is too long: {status}");
        }
    }

    #[test]
    fn write_only_lighting_never_claims_firmware_readback() {
        assert_eq!(lighting_apply_status(true), "Nastavení potvrzeno firmwarem");
        assert_eq!(lighting_apply_status(false), "Použito · stav nelze přečíst");
        assert_eq!(
            localized_status(Language::English, lighting_apply_status(false)),
            "Applied · state readback unavailable"
        );
        assert_eq!(
            compact_status(Language::English, lighting_apply_status(false)),
            "Last applied"
        );
        assert_eq!(
            localized_status(
                Language::English,
                "Nastavení podsvícení potvrzeno firmwarem"
            ),
            "Lighting confirmed by firmware"
        );
        assert_eq!(
            compact_status(Language::Czech, "Nastavení podsvícení potvrzeno firmwarem"),
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
        assert_eq!(state.status_message, "Nastavení potvrzeno firmwarem");
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
            assert!(!state.status_message.contains("není synchronní"));
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
        assert!(
            state
                .status_message
                .starts_with("GPU profil není synchronní:")
        );

        reconcile_profile_telemetry(
            &mut state,
            HardwareProfile::Turbo,
            Some(100),
            Some(200),
            Some(true),
        );
        assert_eq!(state.health, HealthState::Healthy);
        assert_eq!(state.status_message, "Ovládání Acer + NVIDIA připojeno");
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
        let mut state = AppState {
            health: HealthState::Warning,
            status_message: "existing warning".to_string(),
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
        assert_eq!(state.status_message, "existing warning");
        assert_eq!(
            state.platform_error.as_deref(),
            Some("platform unavailable")
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
        assert_eq!(state.platform_error.as_deref(), Some("refresh failed"));
        assert_eq!(state.status_message, "refresh failed");
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
            state.lighting_error.as_deref(),
            Some("temporary RGB readback failure")
        );
        assert_eq!(
            diagnostics,
            vec!["RGB: temporary RGB readback failure".to_string()]
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
        assert_eq!(state.status_message, "initialization failed");
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
        assert!(production.contains("WindowChrome {}"));
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
        assert!(!production.contains('…'));
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
        assert!(production.contains("Read-only kernel and firmware data"));
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
        assert!(production.contains("Příkon GPU / limit"));
        assert!(production.contains("GPU power / limit"));
        assert!(!production.contains("GPU výkon a takty"));
    }
}
