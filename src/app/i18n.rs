//! Small static GUI localization authority.
//!
//! This module is intentionally GUI-owned: locale selection and persistence
//! never enter the root daemon, protocol, probe, or hardware decisions.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const LOCALE_PREFERENCE_MAX_BYTES: u64 = 16;
const LOCALE_TEMP_ATTEMPTS: u32 = 32;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(super) enum LocaleId {
    Czech,
    #[default]
    English,
    SimplifiedChinese,
}

impl LocaleId {
    pub(super) const ENABLED: [Self; 3] = [Self::English, Self::SimplifiedChinese, Self::Czech];

    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::Czech => "cs",
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }

    pub(super) const fn display_code(self) -> &'static str {
        match self {
            Self::Czech => "CZ",
            Self::English => "EN",
            Self::SimplifiedChinese => "中文",
        }
    }

    pub(super) const fn html_code(self) -> &'static str {
        self.code()
    }

    pub(super) const fn toggle(self) -> Self {
        match self {
            Self::Czech => Self::ENABLED[0],
            Self::English => Self::ENABLED[1],
            Self::SimplifiedChinese => Self::ENABLED[2],
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "cs" => Some(Self::Czech),
            "en" => Some(Self::English),
            "zh-CN" => Some(Self::SimplifiedChinese),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u16)]
pub(super) enum MessageId {
    AppCompactStatus001,
    AppCompactStatus002,
    AppCompactStatus003,
    AppCompactStatus004,
    AppCompactStatus005,
    AppCompactStatus006,
    AppCompactStatus007,
    AppLabel001,
    AppHint001,
    AppHint002,
    AppHint003,
    AppLabel002,
    AppLabel003,
    AppLabel004,
    CommonUnavailable,
    AppLabel005,
    AppLabel006,
    AppLabel007,
    AppDashboard001,
    AppDashboard002,
    AppDashboard003,
    AppHeader001,
    AppHeader002,
    AppHeader003,
    AppHeader004,
    AppHeader005,
    AppHeader006,
    AppQuickStrip001,
    CommonLoad,
    AppQuickStrip002,
    CommonSleeping,
    CommonKeyboard,
    AppLightingTargetLabel001,
    AppLightingTargetLabel002,
    AppLightingTargetLabel003,
    AppControlDock001,
    AppControlDock002,
    AppControlDock003,
    AppControlDock004,
    AppControlDock005,
    AppControlDock006,
    AppControlDock007,
    AppControlDock008,
    AppControlDock009,
    AppControlDock010,
    AppControlDock011,
    AppControlDock012,
    AppControlDock013,
    CommonOn,
    CommonOff,
    CommonBrightness,
    AppControlDock014,
    AppControlDock015,
    AppControlDock016,
    AppControlDock017,
    CommonApply,
    AppControlDock018,
    AppControlDock019,
    AppControlDock020,
    CommonReadError,
    CommonUnsupported,
    AppAdvancedPanel001,
    AppAdvancedPanel002,
    AppAdvancedPanel003,
    AppAdvancedPanel004,
    AppAdvancedPanel005,
    AppAdvancedPanel006,
    AppAdvancedPanel007,
    AppAdvancedPanel008,
    CommonVramClock,
    AppAdvancedPanel009,
    AppAdvancedPanel010,
    AppAdvancedPanel011,
    AppAdvancedPanel012,
    AppAdvancedPanel013,
    AppAdvancedPanel014,
    AppAdvancedPanel015,
    AppAdvancedPanel016,
    AppHardwarePanel001,
    AppHardwarePanel002,
    CommonReadOnly,
    AppHardwarePanel003,
    AppHardwarePanel004,
    AppHardwarePanel005,
    AppHardwarePanel006,
    AppHardwarePanel007,
    AppHardwarePanel008,
    AppHardwarePanel009,
    AppHardwarePanel010,
    AppHardwarePanel011,
    AppHardwarePanel012,
    AppHardwarePanel013,
    AppHardwarePanel014,
    AppHardwarePanel015,
    AppHardwarePanel016,
    AppHardwarePanel017,
    AppHardwarePanel018,
    AppHardwarePanel019,
    AppPlatformAdvanced001,
    AppPlatformAdvanced002,
    AppPlatformAdvanced003,
    AppPlatformAdvanced004,
    AppPlatformAdvanced005,
    AppPlatformAdvanced006,
    AppPlatformAdvanced007,
    AppPlatformAdvanced008,
    AppPlatformAdvanced009,
    AppPlatformAdvanced010,
    AppPlatformAdvanced011,
    AppPlatformAdvanced012,
    AppPlatformAdvanced013,
    AppPlatformAdvanced014,
    AppPlatformAdvanced015,
    AppPlatformAdvanced016,
    AppPlatformAdvanced017,
    AppPlatformAdvanced018,
    AppPlatformAdvanced019,
    AppPlatformAdvanced020,
    AppPlatformAdvanced021,
    AppPlatformAdvanced022,
    AppPlatformAdvanced023,
    AppPlatformAdvanced024,
    AppPlatformAdvanced025,
    AppPlatformAdvanced026,
    AppPlatformAdvanced027,
    AppPlatformAdvanced028,
    AppPlatformAdvanced029,
    AppPlatformAdvanced030,
    AppPlatformAdvanced031,
    AppPlatformAdvanced032,
    AppPlatformAdvanced033,
    AppBatteryLiveStatus001,
    AppBatteryLiveStatus002,
    AppBatteryLiveStatus003,
    AppBatteryLiveStatus004,
    AppBatteryLiveStatus005,
    AppDualHistoryChart001,
    AppOffsets001,
    AppClockEventLabel001,
    AppClockEventLabel002,
    AppClockEventLabel003,
    AppClockEventLabel004,
    AppClockEventLabel005,
    AppClockEventLabel006,
    AppClockEventLabel007,
    AppClockEventLabel008,
    AppClockEventLabel009,
    AppClockEventLabel010,
    DocsLabel001,
    DocsLabel002,
    DocsLabel003,
    DocsLabel004,
    DocsLabel005,
    DocsLabel006,
    DocsLabel007,
    CommonProject,
    DocsModal001,
    CommonCloseDocumentation,
    DocsModal002,
    DocsAboutPane001,
    DocsAboutPane002,
    DocsAboutPane003,
    DocsAboutPane004,
    DocsAboutPane005,
    DocsAboutPane006,
    DocsAboutPane007,
    CommonLicense,
    DocsAboutPane008,
    DocsAboutPane009,
    DocsAboutPane010,
    DocsAboutPane011,
    DocsAboutPane012,
    DocsAboutPane013,
    DocsAboutPane014,
    DocsAboutPane015,
    DocsAboutPane016,
    DocsAboutPane017,
    DocsAboutPane018,
    DocsAboutPane019,
    DocsAboutPane020,
    DocsUsagePane001,
    DocsUsagePane002,
    DocsUsagePane003,
    DocsUsagePane004,
    DocsUsagePane005,
    DocsUsagePane006,
    DocsUsagePane007,
    DocsUsagePane008,
    DocsUsagePane009,
    DocsUsagePane010,
    DocsUsagePane011,
    DocsUsagePane012,
    DocsUsagePane013,
    DocsUsagePane014,
    DocsUsagePane015,
    DocsUsagePane016,
    DocsUsagePane017,
    DocsUsagePane018,
    DocsHardwarePane001,
    DocsHardwarePane002,
    DocsHardwarePane003,
    DocsHardwarePane004,
    DocsHardwarePane005,
    DocsHardwarePane006,
    DocsHardwarePane007,
    DocsHardwarePane008,
    DocsHardwarePane009,
    DocsHardwarePane010,
    DocsHardwarePane011,
    DocsHardwarePane012,
    DocsHardwarePane013,
    DocsHardwarePane014,
    DocsHardwarePane015,
    DocsHardwarePane016,
    DocsHardwarePane017,
    DocsHardwarePane018,
    DocsApiPane001,
    DocsApiPane002,
    DocsApiPane003,
    DocsApiPane004,
    DocsApiPane005,
    DocsApiPane006,
    DocsApiPane007,
    DocsApiPane008,
    DocsApiPane009,
    DocsApiPane010,
    DocsApiPane011,
    DocsProjectPane001,
    DocsProjectPane002,
    DocsProjectPane003,
    DocsProjectPane004,
    DocsProjectPane005,
    DocsProjectPane006,
    DocsProjectPane007,
    DocsProjectPane008,
    DocsProjectPane009,
    DocsProjectPane010,
    DocsProjectPane011,
    DocsProjectPane012,
    DocsProjectPane013,
    WindowMinimize,
    WindowClose,
    CoolingTelemetry,
    FanModeAuto,
    FanModeMaximum,
    ProfileEco,
    ProfileTurbo,
    HardwareL3Cache,
    HardwareGpuMaximum,
    HardwareVramMaximum,
    PlatformLcdOverride,
    PlatformFirmware,
    ClockSyncBoost,
    DocsSecureBoot,
    DocsRpmProbe,
    DocsEnekResearch,
    DocsBackendOrder,
    StatusAcerControlsConnected,
    StatusAcerNvidiaControlsConnected,
    StatusReadOnlyTelemetryConnected,
    StatusConnectingControls,
    StatusPlatformRefreshed,
    StatusSettingsConfirmed,
    StatusLightingConfirmed,
    StatusAppliedWithoutReadback,
    StatusWritingAndVerifying,
    StatusProfileVerified,
    StatusGpuMismatch,
    StatusPartialCapabilities,
    StatusPlatformReadbackFailed,
    StatusTelemetryConnecting,
    StatusTelemetryReconnecting,
    StatusRetryIn,
    StatusInitializationFailure,
    StatusFanFailure,
    StatusProfileFailure,
    StatusLightingFailure,
    StatusPlatformFailure,
    StatusRefreshFailure,
    StatusCompactSettingsVerified,
    StatusCompactLightingVerified,
    StatusCompactLastApplied,
    StatusCompactVerifying,
    StatusCompactPlatformRefreshed,
    StatusCompactProfileEco,
    StatusCompactProfileQuiet,
    StatusCompactProfileBalanced,
    StatusCompactProfilePerformance,
    StatusCompactProfileTurbo,
    StatusCompactProfileGeneric,
    StatusOffsetUnavailable,
    StatusOffsetCustomOrPartial,
    StatusGpuLimitUnavailable,
    DiagnosticRgb,
    DiagnosticPlatform,
    DiagnosticHardware,
    PlatformFieldBatteryLimit,
    PlatformFieldBatteryCalibration,
    PlatformFieldUsbCharging,
    PlatformFieldKeyboardTimeout,
    PlatformFieldBootSound,
    PlatformFieldLcdOverride,
    PlatformFieldRearLogo,
    DocsStandaloneRelease,
    DocsStandaloneReleaseBody,
    DocsStandaloneReleaseLink,
    DocsArchAur,
    DocsArchAurBody,
    DocsArchAurLink,
}

impl MessageId {
    pub(super) const ALL: [Self; 309] = [
        Self::AppCompactStatus001,
        Self::AppCompactStatus002,
        Self::AppCompactStatus003,
        Self::AppCompactStatus004,
        Self::AppCompactStatus005,
        Self::AppCompactStatus006,
        Self::AppCompactStatus007,
        Self::AppLabel001,
        Self::AppHint001,
        Self::AppHint002,
        Self::AppHint003,
        Self::AppLabel002,
        Self::AppLabel003,
        Self::AppLabel004,
        Self::CommonUnavailable,
        Self::AppLabel005,
        Self::AppLabel006,
        Self::AppLabel007,
        Self::AppDashboard001,
        Self::AppDashboard002,
        Self::AppDashboard003,
        Self::AppHeader001,
        Self::AppHeader002,
        Self::AppHeader003,
        Self::AppHeader004,
        Self::AppHeader005,
        Self::AppHeader006,
        Self::AppQuickStrip001,
        Self::CommonLoad,
        Self::AppQuickStrip002,
        Self::CommonSleeping,
        Self::CommonKeyboard,
        Self::AppLightingTargetLabel001,
        Self::AppLightingTargetLabel002,
        Self::AppLightingTargetLabel003,
        Self::AppControlDock001,
        Self::AppControlDock002,
        Self::AppControlDock003,
        Self::AppControlDock004,
        Self::AppControlDock005,
        Self::AppControlDock006,
        Self::AppControlDock007,
        Self::AppControlDock008,
        Self::AppControlDock009,
        Self::AppControlDock010,
        Self::AppControlDock011,
        Self::AppControlDock012,
        Self::AppControlDock013,
        Self::CommonOn,
        Self::CommonOff,
        Self::CommonBrightness,
        Self::AppControlDock014,
        Self::AppControlDock015,
        Self::AppControlDock016,
        Self::AppControlDock017,
        Self::CommonApply,
        Self::AppControlDock018,
        Self::AppControlDock019,
        Self::AppControlDock020,
        Self::CommonReadError,
        Self::CommonUnsupported,
        Self::AppAdvancedPanel001,
        Self::AppAdvancedPanel002,
        Self::AppAdvancedPanel003,
        Self::AppAdvancedPanel004,
        Self::AppAdvancedPanel005,
        Self::AppAdvancedPanel006,
        Self::AppAdvancedPanel007,
        Self::AppAdvancedPanel008,
        Self::CommonVramClock,
        Self::AppAdvancedPanel009,
        Self::AppAdvancedPanel010,
        Self::AppAdvancedPanel011,
        Self::AppAdvancedPanel012,
        Self::AppAdvancedPanel013,
        Self::AppAdvancedPanel014,
        Self::AppAdvancedPanel015,
        Self::AppAdvancedPanel016,
        Self::AppHardwarePanel001,
        Self::AppHardwarePanel002,
        Self::CommonReadOnly,
        Self::AppHardwarePanel003,
        Self::AppHardwarePanel004,
        Self::AppHardwarePanel005,
        Self::AppHardwarePanel006,
        Self::AppHardwarePanel007,
        Self::AppHardwarePanel008,
        Self::AppHardwarePanel009,
        Self::AppHardwarePanel010,
        Self::AppHardwarePanel011,
        Self::AppHardwarePanel012,
        Self::AppHardwarePanel013,
        Self::AppHardwarePanel014,
        Self::AppHardwarePanel015,
        Self::AppHardwarePanel016,
        Self::AppHardwarePanel017,
        Self::AppHardwarePanel018,
        Self::AppHardwarePanel019,
        Self::AppPlatformAdvanced001,
        Self::AppPlatformAdvanced002,
        Self::AppPlatformAdvanced003,
        Self::AppPlatformAdvanced004,
        Self::AppPlatformAdvanced005,
        Self::AppPlatformAdvanced006,
        Self::AppPlatformAdvanced007,
        Self::AppPlatformAdvanced008,
        Self::AppPlatformAdvanced009,
        Self::AppPlatformAdvanced010,
        Self::AppPlatformAdvanced011,
        Self::AppPlatformAdvanced012,
        Self::AppPlatformAdvanced013,
        Self::AppPlatformAdvanced014,
        Self::AppPlatformAdvanced015,
        Self::AppPlatformAdvanced016,
        Self::AppPlatformAdvanced017,
        Self::AppPlatformAdvanced018,
        Self::AppPlatformAdvanced019,
        Self::AppPlatformAdvanced020,
        Self::AppPlatformAdvanced021,
        Self::AppPlatformAdvanced022,
        Self::AppPlatformAdvanced023,
        Self::AppPlatformAdvanced024,
        Self::AppPlatformAdvanced025,
        Self::AppPlatformAdvanced026,
        Self::AppPlatformAdvanced027,
        Self::AppPlatformAdvanced028,
        Self::AppPlatformAdvanced029,
        Self::AppPlatformAdvanced030,
        Self::AppPlatformAdvanced031,
        Self::AppPlatformAdvanced032,
        Self::AppPlatformAdvanced033,
        Self::AppBatteryLiveStatus001,
        Self::AppBatteryLiveStatus002,
        Self::AppBatteryLiveStatus003,
        Self::AppBatteryLiveStatus004,
        Self::AppBatteryLiveStatus005,
        Self::AppDualHistoryChart001,
        Self::AppOffsets001,
        Self::AppClockEventLabel001,
        Self::AppClockEventLabel002,
        Self::AppClockEventLabel003,
        Self::AppClockEventLabel004,
        Self::AppClockEventLabel005,
        Self::AppClockEventLabel006,
        Self::AppClockEventLabel007,
        Self::AppClockEventLabel008,
        Self::AppClockEventLabel009,
        Self::AppClockEventLabel010,
        Self::DocsLabel001,
        Self::DocsLabel002,
        Self::DocsLabel003,
        Self::DocsLabel004,
        Self::DocsLabel005,
        Self::DocsLabel006,
        Self::DocsLabel007,
        Self::CommonProject,
        Self::DocsModal001,
        Self::CommonCloseDocumentation,
        Self::DocsModal002,
        Self::DocsAboutPane001,
        Self::DocsAboutPane002,
        Self::DocsAboutPane003,
        Self::DocsAboutPane004,
        Self::DocsAboutPane005,
        Self::DocsAboutPane006,
        Self::DocsAboutPane007,
        Self::CommonLicense,
        Self::DocsAboutPane008,
        Self::DocsAboutPane009,
        Self::DocsAboutPane010,
        Self::DocsAboutPane011,
        Self::DocsAboutPane012,
        Self::DocsAboutPane013,
        Self::DocsAboutPane014,
        Self::DocsAboutPane015,
        Self::DocsAboutPane016,
        Self::DocsAboutPane017,
        Self::DocsAboutPane018,
        Self::DocsAboutPane019,
        Self::DocsAboutPane020,
        Self::DocsUsagePane001,
        Self::DocsUsagePane002,
        Self::DocsUsagePane003,
        Self::DocsUsagePane004,
        Self::DocsUsagePane005,
        Self::DocsUsagePane006,
        Self::DocsUsagePane007,
        Self::DocsUsagePane008,
        Self::DocsUsagePane009,
        Self::DocsUsagePane010,
        Self::DocsUsagePane011,
        Self::DocsUsagePane012,
        Self::DocsUsagePane013,
        Self::DocsUsagePane014,
        Self::DocsUsagePane015,
        Self::DocsUsagePane016,
        Self::DocsUsagePane017,
        Self::DocsUsagePane018,
        Self::DocsHardwarePane001,
        Self::DocsHardwarePane002,
        Self::DocsHardwarePane003,
        Self::DocsHardwarePane004,
        Self::DocsHardwarePane005,
        Self::DocsHardwarePane006,
        Self::DocsHardwarePane007,
        Self::DocsHardwarePane008,
        Self::DocsHardwarePane009,
        Self::DocsHardwarePane010,
        Self::DocsHardwarePane011,
        Self::DocsHardwarePane012,
        Self::DocsHardwarePane013,
        Self::DocsHardwarePane014,
        Self::DocsHardwarePane015,
        Self::DocsHardwarePane016,
        Self::DocsHardwarePane017,
        Self::DocsHardwarePane018,
        Self::DocsApiPane001,
        Self::DocsApiPane002,
        Self::DocsApiPane003,
        Self::DocsApiPane004,
        Self::DocsApiPane005,
        Self::DocsApiPane006,
        Self::DocsApiPane007,
        Self::DocsApiPane008,
        Self::DocsApiPane009,
        Self::DocsApiPane010,
        Self::DocsApiPane011,
        Self::DocsProjectPane001,
        Self::DocsProjectPane002,
        Self::DocsProjectPane003,
        Self::DocsProjectPane004,
        Self::DocsProjectPane005,
        Self::DocsProjectPane006,
        Self::DocsProjectPane007,
        Self::DocsProjectPane008,
        Self::DocsProjectPane009,
        Self::DocsProjectPane010,
        Self::DocsProjectPane011,
        Self::DocsProjectPane012,
        Self::DocsProjectPane013,
        Self::WindowMinimize,
        Self::WindowClose,
        Self::CoolingTelemetry,
        Self::FanModeAuto,
        Self::FanModeMaximum,
        Self::ProfileEco,
        Self::ProfileTurbo,
        Self::HardwareL3Cache,
        Self::HardwareGpuMaximum,
        Self::HardwareVramMaximum,
        Self::PlatformLcdOverride,
        Self::PlatformFirmware,
        Self::ClockSyncBoost,
        Self::DocsSecureBoot,
        Self::DocsRpmProbe,
        Self::DocsEnekResearch,
        Self::DocsBackendOrder,
        Self::StatusAcerControlsConnected,
        Self::StatusAcerNvidiaControlsConnected,
        Self::StatusReadOnlyTelemetryConnected,
        Self::StatusConnectingControls,
        Self::StatusPlatformRefreshed,
        Self::StatusSettingsConfirmed,
        Self::StatusLightingConfirmed,
        Self::StatusAppliedWithoutReadback,
        Self::StatusWritingAndVerifying,
        Self::StatusProfileVerified,
        Self::StatusGpuMismatch,
        Self::StatusPartialCapabilities,
        Self::StatusPlatformReadbackFailed,
        Self::StatusTelemetryConnecting,
        Self::StatusTelemetryReconnecting,
        Self::StatusRetryIn,
        Self::StatusInitializationFailure,
        Self::StatusFanFailure,
        Self::StatusProfileFailure,
        Self::StatusLightingFailure,
        Self::StatusPlatformFailure,
        Self::StatusRefreshFailure,
        Self::StatusCompactSettingsVerified,
        Self::StatusCompactLightingVerified,
        Self::StatusCompactLastApplied,
        Self::StatusCompactVerifying,
        Self::StatusCompactPlatformRefreshed,
        Self::StatusCompactProfileEco,
        Self::StatusCompactProfileQuiet,
        Self::StatusCompactProfileBalanced,
        Self::StatusCompactProfilePerformance,
        Self::StatusCompactProfileTurbo,
        Self::StatusCompactProfileGeneric,
        Self::StatusOffsetUnavailable,
        Self::StatusOffsetCustomOrPartial,
        Self::StatusGpuLimitUnavailable,
        Self::DiagnosticRgb,
        Self::DiagnosticPlatform,
        Self::DiagnosticHardware,
        Self::PlatformFieldBatteryLimit,
        Self::PlatformFieldBatteryCalibration,
        Self::PlatformFieldUsbCharging,
        Self::PlatformFieldKeyboardTimeout,
        Self::PlatformFieldBootSound,
        Self::PlatformFieldLcdOverride,
        Self::PlatformFieldRearLogo,
        Self::DocsStandaloneRelease,
        Self::DocsStandaloneReleaseBody,
        Self::DocsStandaloneReleaseLink,
        Self::DocsArchAur,
        Self::DocsArchAurBody,
        Self::DocsArchAurLink,
    ];
}

const _: () = assert!(MessageId::ALL.len() == 309);

#[derive(Clone, Copy)]
struct CatalogEntry {
    cs: &'static str,
    en: &'static str,
}

const fn entry(id: MessageId) -> CatalogEntry {
    match id {
        MessageId::AppCompactStatus001 => CatalogEntry {
            cs: "Částečný readback",
            en: "Partial readback",
        },
        MessageId::AppCompactStatus002 => CatalogEntry {
            cs: "GPU nesedí",
            en: "GPU mismatch",
        },
        MessageId::AppCompactStatus003 => CatalogEntry {
            cs: "Rollback selhal",
            en: "Rollback failed",
        },
        MessageId::AppCompactStatus004 => CatalogEntry {
            cs: "Ověření stavu selhalo",
            en: "State verification failed",
        },
        MessageId::AppCompactStatus005 => CatalogEntry {
            cs: "Firmware funkci nepodporuje",
            en: "Unsupported by firmware",
        },
        MessageId::AppCompactStatus006 => CatalogEntry {
            cs: "Řídicí služba neodpovídá",
            en: "Control service unavailable",
        },
        MessageId::AppCompactStatus007 => CatalogEntry {
            cs: "Podrobnosti nahoře",
            en: "Details above",
        },
        MessageId::AppLabel001 => CatalogEntry {
            cs: "Ručně",
            en: "Manual",
        },
        MessageId::AppHint001 => CatalogEntry {
            cs: "Firmware řídí chlazení",
            en: "Firmware controls cooling",
        },
        MessageId::AppHint002 => CatalogEntry {
            cs: "Vlastní pevné otáčky",
            en: "Custom fixed fan speed",
        },
        MessageId::AppHint003 => CatalogEntry {
            cs: "Plný výkon ventilátorů",
            en: "Maximum fan performance",
        },
        MessageId::AppLabel002 => CatalogEntry {
            cs: "Tichý",
            en: "Quiet",
        },
        MessageId::AppLabel003 => CatalogEntry {
            cs: "Balanc",
            en: "Balanced",
        },
        MessageId::AppLabel004 => CatalogEntry {
            cs: "Výkon",
            en: "Performance",
        },
        MessageId::CommonUnavailable => CatalogEntry {
            cs: "Nedostupné",
            en: "Unavailable",
        },
        MessageId::AppLabel005 => CatalogEntry {
            cs: "Připraveno",
            en: "Ready",
        },
        MessageId::AppLabel006 => CatalogEntry {
            cs: "Nastavuji",
            en: "Applying",
        },
        MessageId::AppLabel007 => CatalogEntry {
            cs: "Zkontrolovat",
            en: "Check",
        },
        MessageId::AppDashboard001 => CatalogEntry {
            cs: "Telemetrie se připojuje",
            en: "Telemetry connecting",
        },
        MessageId::AppDashboard002 => CatalogEntry {
            cs: "Telemetrie se obnovuje",
            en: "Telemetry reconnecting",
        },
        MessageId::AppDashboard003 => CatalogEntry {
            cs: "Ovládání notebooku",
            en: "Laptop controls",
        },
        MessageId::AppHeader001 => CatalogEntry {
            cs: "O aplikaci a dokumentace",
            en: "About and documentation",
        },
        MessageId::AppHeader002 => CatalogEntry {
            cs: "Otevřít informace a dokumentaci",
            en: "Open information and documentation",
        },
        MessageId::AppHeader003 => CatalogEntry {
            cs: "Změnit jazyk",
            en: "Change language",
        },
        MessageId::AppHeader004 => CatalogEntry {
            cs: "Skrýt rozšířený panel",
            en: "Hide advanced panel",
        },
        MessageId::AppHeader005 => CatalogEntry {
            cs: "Zobrazit rozšířený panel",
            en: "Show advanced panel",
        },
        MessageId::AppHeader006 => CatalogEntry {
            cs: "Rozšířené",
            en: "Advanced",
        },
        MessageId::AppQuickStrip001 => CatalogEntry {
            cs: "Systémová telemetrie",
            en: "System telemetry",
        },
        MessageId::CommonLoad => CatalogEntry {
            cs: "ZÁTĚŽ",
            en: "LOAD",
        },
        MessageId::AppQuickStrip002 => CatalogEntry {
            cs: "Profil",
            en: "Profile",
        },
        MessageId::CommonSleeping => CatalogEntry {
            cs: "Spí",
            en: "Sleeping",
        },
        MessageId::CommonKeyboard => CatalogEntry {
            cs: "Klávesnice",
            en: "Keyboard",
        },
        MessageId::AppLightingTargetLabel001 => CatalogEntry {
            cs: "Logo víka",
            en: "Cover logo",
        },
        MessageId::AppLightingTargetLabel002 => CatalogEntry {
            cs: "Zadní logo",
            en: "Rear logo",
        },
        MessageId::AppLightingTargetLabel003 => CatalogEntry {
            cs: "Světelná lišta",
            en: "Lightbar",
        },
        MessageId::AppControlDock001 => CatalogEntry {
            cs: "Volby profilů poskytuje živé rozhraní Linux kernelu.",
            en: "Profile choices come from the live Linux kernel interface.",
        },
        MessageId::AppControlDock002 => CatalogEntry {
            cs: "Známé příkazy Acer Gaming-WMI; každá změna se ověřuje zpětným čtením.",
            en: "Known Acer Gaming-WMI commands; every change is verified by readback.",
        },
        MessageId::AppControlDock003 => CatalogEntry {
            cs: "Firmware profily nejsou dostupné.",
            en: "Firmware profiles are unavailable.",
        },
        MessageId::AppControlDock004 => CatalogEntry {
            cs: "Podsvícení",
            en: "Backlight",
        },
        MessageId::AppControlDock005 => CatalogEntry {
            cs: "Stav z firmware",
            en: "Firmware state",
        },
        MessageId::AppControlDock006 => CatalogEntry {
            cs: "Naposledy použito",
            en: "Last applied",
        },
        MessageId::AppControlDock007 => CatalogEntry {
            cs: "Stav neznámý",
            en: "State unknown",
        },
        MessageId::AppControlDock008 => CatalogEntry {
            cs: "Ovládací centrum",
            en: "Control center",
        },
        MessageId::AppControlDock009 => CatalogEntry {
            cs: "Výkonnostní profil Acer",
            en: "Acer performance profile",
        },
        MessageId::AppControlDock010 => CatalogEntry {
            cs: "Ventilátory",
            en: "Fans",
        },
        MessageId::AppControlDock011 => CatalogEntry {
            cs: "RGB klávesnice",
            en: "RGB keyboard",
        },
        MessageId::AppControlDock012 => CatalogEntry {
            cs: "RGB modul není dostupný",
            en: "RGB module is unavailable",
        },
        MessageId::AppControlDock013 => CatalogEntry {
            cs: "Napájení podsvícení klávesnice",
            en: "Keyboard backlight power",
        },
        MessageId::CommonOn => CatalogEntry {
            cs: "Zap",
            en: "On",
        },
        MessageId::CommonOff => CatalogEntry {
            cs: "Vyp",
            en: "Off",
        },
        MessageId::CommonBrightness => CatalogEntry {
            cs: "Jas",
            en: "Brightness",
        },
        MessageId::AppControlDock014 => CatalogEntry {
            cs: "Statické",
            en: "Static",
        },
        MessageId::AppControlDock015 => CatalogEntry {
            cs: "Dech",
            en: "Breathing",
        },
        MessageId::AppControlDock016 => CatalogEntry {
            cs: "Neon",
            en: "Neon",
        },
        MessageId::AppControlDock017 => CatalogEntry {
            cs: "Režim ventilátorů",
            en: "Fan mode",
        },
        MessageId::CommonApply => CatalogEntry {
            cs: "Použít",
            en: "Apply",
        },
        MessageId::AppControlDock018 => CatalogEntry {
            cs: "Řízení ventilátorů není dostupné",
            en: "Fan control unavailable",
        },
        MessageId::AppControlDock019 => CatalogEntry {
            cs: "Vybrány maximální otáčky ventilátorů",
            en: "Maximum fan RPM selected",
        },
        MessageId::AppControlDock020 => CatalogEntry {
            cs: "Vybráno automatické řízení otáček",
            en: "Automatic RPM control selected",
        },
        MessageId::CommonReadError => CatalogEntry {
            cs: "Chyba čtení",
            en: "Read error",
        },
        MessageId::CommonUnsupported => CatalogEntry {
            cs: "Nepodporováno",
            en: "Unsupported",
        },
        MessageId::AppAdvancedPanel001 => CatalogEntry {
            cs: "Důvody omezení taktu",
            en: "Clock / throttle reasons",
        },
        MessageId::AppAdvancedPanel002 => CatalogEntry {
            cs: "Chyba čtení",
            en: "Readback error",
        },
        MessageId::AppAdvancedPanel003 => CatalogEntry {
            cs: "Rozšířené systémové informace",
            en: "Advanced system information",
        },
        MessageId::AppAdvancedPanel004 => CatalogEntry {
            cs: "Metriky",
            en: "Metrics",
        },
        MessageId::AppAdvancedPanel005 => CatalogEntry {
            cs: "Hardware",
            en: "Hardware",
        },
        MessageId::AppAdvancedPanel006 => CatalogEntry {
            cs: "Zařízení",
            en: "Device",
        },
        MessageId::AppAdvancedPanel007 => CatalogEntry {
            cs: "Zátěž CPU",
            en: "CPU workload",
        },
        MessageId::AppAdvancedPanel008 => CatalogEntry {
            cs: "Zátěž GPU",
            en: "GPU workload",
        },
        MessageId::CommonVramClock => CatalogEntry {
            cs: "Takt VRAM",
            en: "VRAM clock",
        },
        MessageId::AppAdvancedPanel009 => CatalogEntry {
            cs: "Příkon GPU",
            en: "GPU power",
        },
        MessageId::AppAdvancedPanel010 => CatalogEntry {
            cs: "Chlazení",
            en: "Cooling",
        },
        MessageId::AppAdvancedPanel011 => CatalogEntry {
            cs: "Systémová zátěž",
            en: "System load",
        },
        MessageId::AppAdvancedPanel012 => CatalogEntry {
            cs: "Teploty",
            en: "Temperatures",
        },
        MessageId::AppAdvancedPanel013 => CatalogEntry {
            cs: "Příkon GPU / limit",
            en: "GPU power / limit",
        },
        MessageId::AppAdvancedPanel014 => CatalogEntry {
            cs: "PŘÍKON",
            en: "POWER",
        },
        MessageId::AppAdvancedPanel015 => CatalogEntry {
            cs: "LIMIT",
            en: "LIMIT",
        },
        MessageId::AppAdvancedPanel016 => CatalogEntry {
            cs: "Domény taktu GPU",
            en: "GPU clock domains",
        },
        MessageId::AppHardwarePanel001 => CatalogEntry {
            cs: "Nezjištěno",
            en: "Unavailable",
        },
        MessageId::AppHardwarePanel002 => CatalogEntry {
            cs: "Procesor",
            en: "Processor",
        },
        MessageId::CommonReadOnly => CatalogEntry {
            cs: "Jen čtení",
            en: "Read only",
        },
        MessageId::AppHardwarePanel003 => CatalogEntry {
            cs: "Aktivní jádra",
            en: "Active cores",
        },
        MessageId::AppHardwarePanel004 => CatalogEntry {
            cs: "Online vlákna",
            en: "Online threads",
        },
        MessageId::AppHardwarePanel005 => CatalogEntry {
            cs: "Aktivní P / E",
            en: "Active P / E cores",
        },
        MessageId::AppHardwarePanel006 => CatalogEntry {
            cs: "Architektura",
            en: "Architecture",
        },
        MessageId::AppHardwarePanel007 => CatalogEntry {
            cs: "Rodina CPU",
            en: "CPU family",
        },
        MessageId::AppHardwarePanel008 => CatalogEntry {
            cs: "Aktuální takt",
            en: "Current clock",
        },
        MessageId::AppHardwarePanel009 => CatalogEntry {
            cs: "Maximální takt",
            en: "Maximum clock",
        },
        MessageId::AppHardwarePanel010 => CatalogEntry {
            cs: "Grafika",
            en: "Graphics",
        },
        MessageId::AppHardwarePanel011 => CatalogEntry {
            cs: "Ovladač",
            en: "Driver",
        },
        MessageId::AppHardwarePanel012 => CatalogEntry {
            cs: "Grafický takt",
            en: "Graphics clock",
        },
        MessageId::AppHardwarePanel013 => CatalogEntry {
            cs: "Systémová paměť",
            en: "System memory",
        },
        MessageId::AppHardwarePanel014 => CatalogEntry {
            cs: "Celkem",
            en: "Total",
        },
        MessageId::AppHardwarePanel015 => CatalogEntry {
            cs: "Typ",
            en: "Type",
        },
        MessageId::AppHardwarePanel016 => CatalogEntry {
            cs: "Rychlost",
            en: "Speed",
        },
        MessageId::AppHardwarePanel017 => CatalogEntry {
            cs: "Kanály",
            en: "Channels",
        },
        MessageId::AppHardwarePanel018 => CatalogEntry {
            cs: "Moduly",
            en: "Modules",
        },
        MessageId::AppHardwarePanel019 => CatalogEntry {
            cs: "Data pouze pro čtení z kernelu a firmware; nedostupné hodnoty se neodhadují.",
            en: "Read-only kernel and firmware data; unavailable values are not inferred.",
        },
        MessageId::AppPlatformAdvanced001 => CatalogEntry {
            cs: "Čekám na firmware readback",
            en: "Waiting for firmware readback",
        },
        MessageId::AppPlatformAdvanced002 => CatalogEntry {
            cs: "Platformní funkce nejsou načtené",
            en: "Platform features are not loaded",
        },
        MessageId::AppPlatformAdvanced003 => CatalogEntry {
            cs: "Načíst znovu",
            en: "Reload",
        },
        MessageId::AppPlatformAdvanced004 => CatalogEntry {
            cs: "Zastavit",
            en: "Stop",
        },
        MessageId::AppPlatformAdvanced005 => CatalogEntry {
            cs: "Spustit",
            en: "Start",
        },
        MessageId::AppPlatformAdvanced006 => CatalogEntry {
            cs: "Kalibrace aktivní",
            en: "Calibration active",
        },
        MessageId::AppPlatformAdvanced007 => CatalogEntry {
            cs: "Firmware plný cyklus baterie",
            en: "Firmware full battery cycle",
        },
        MessageId::AppPlatformAdvanced008 => CatalogEntry {
            cs: "AC napájení je připojené. Ponech adaptér připojený po celý cyklus.",
            en: "AC power is connected. Keep the adapter connected for the entire cycle.",
        },
        MessageId::AppPlatformAdvanced009 => CatalogEntry {
            cs: "ASense z bezpečnostních důvodů nespustí kalibraci jen přes USB-C. Připoj AC adaptér.",
            en: "ASense does not start calibration on USB-C-only power as a safety policy. Connect an AC adapter.",
        },
        MessageId::AppPlatformAdvanced010 => CatalogEntry {
            cs: "AC adaptér je odpojený. Před startem jej připoj.",
            en: "The AC adapter is disconnected. Connect it before starting.",
        },
        MessageId::AppPlatformAdvanced011 => CatalogEntry {
            cs: "Stav AC nelze ověřit. Před startem připoj adaptér a ponech jej připojený.",
            en: "AC state could not be verified. Connect an adapter and keep it connected.",
        },
        MessageId::AppPlatformAdvanced012 => CatalogEntry {
            cs: "Ověřuji",
            en: "Verifying",
        },
        MessageId::AppPlatformAdvanced013 => CatalogEntry {
            cs: "Ověřeno",
            en: "Verified",
        },
        MessageId::AppPlatformAdvanced014 => CatalogEntry {
            cs: "Limit baterie",
            en: "Battery limit",
        },
        MessageId::AppPlatformAdvanced015 => CatalogEntry {
            cs: "Max. 80 %",
            en: "Maximum 80%",
        },
        MessageId::AppPlatformAdvanced016 => CatalogEntry {
            cs: "USB při vypnutí",
            en: "USB while powered off",
        },
        MessageId::AppPlatformAdvanced017 => CatalogEntry {
            cs: "Vypnout při kapacitě",
            en: "Stop at battery level",
        },
        MessageId::AppPlatformAdvanced018 => CatalogEntry {
            cs: "Kalibrace baterie",
            en: "Battery calibration",
        },
        MessageId::AppPlatformAdvanced019 => CatalogEntry {
            cs: "Zvuk při startu",
            en: "Boot sound",
        },
        MessageId::AppPlatformAdvanced020 => CatalogEntry {
            cs: "Zvuk Predator animace",
            en: "Predator boot animation sound",
        },
        MessageId::AppPlatformAdvanced021 => CatalogEntry {
            cs: "Firmware override displeje",
            en: "Firmware display override",
        },
        MessageId::AppPlatformAdvanced022 => CatalogEntry {
            cs: "Timeout klávesnice",
            en: "Keyboard timeout",
        },
        MessageId::AppPlatformAdvanced023 => CatalogEntry {
            cs: "Automatické zhasnutí RGB",
            en: "Automatic RGB timeout",
        },
        MessageId::AppPlatformAdvanced024 => CatalogEntry {
            cs: "Zadní Predator logo",
            en: "Rear Predator logo",
        },
        MessageId::AppPlatformAdvanced025 => CatalogEntry {
            cs: "Napájení, barva a jas",
            en: "Power, color and brightness",
        },
        MessageId::AppPlatformAdvanced026 => CatalogEntry {
            cs: "Barva",
            en: "Color",
        },
        MessageId::AppPlatformAdvanced027 => CatalogEntry {
            cs: "Obnovit",
            en: "Refresh",
        },
        MessageId::AppPlatformAdvanced028 => CatalogEntry {
            cs: "Spustit kalibraci baterie?",
            en: "Start battery calibration?",
        },
        MessageId::AppPlatformAdvanced029 => CatalogEntry {
            cs: "Firmware spustí dlouhý plný cyklus. Ulož práci; notebook během kalibrace nevypínej ani neuspávej.",
            en: "Firmware will start a long full cycle. Save your work; do not power off or suspend the laptop during calibration.",
        },
        MessageId::AppPlatformAdvanced030 => CatalogEntry {
            cs: "Firmware neposkytuje procenta ani dekódovaný signál dokončení. Po cyklu stav obnov; zůstane-li aktivní, kalibraci ručně zastav.",
            en: "Firmware exposes no percentage or decoded completion signal. Refresh after the cycle; if it remains active, stop calibration manually.",
        },
        MessageId::AppPlatformAdvanced031 => CatalogEntry {
            cs: "Před kalibrací doporučujeme vypnout 80% limit nabíjení.",
            en: "Disable the 80% charge limit before calibration.",
        },
        MessageId::AppPlatformAdvanced032 => CatalogEntry {
            cs: "Zrušit",
            en: "Cancel",
        },
        MessageId::AppPlatformAdvanced033 => CatalogEntry {
            cs: "Spustit kalibraci",
            en: "Start calibration",
        },
        MessageId::AppBatteryLiveStatus001 => CatalogEntry {
            cs: "nabíjení",
            en: "charging",
        },
        MessageId::AppBatteryLiveStatus002 => CatalogEntry {
            cs: "vybíjení",
            en: "discharging",
        },
        MessageId::AppBatteryLiveStatus003 => CatalogEntry {
            cs: "plná",
            en: "full",
        },
        MessageId::AppBatteryLiveStatus004 => CatalogEntry {
            cs: "nenabíjí",
            en: "not charging",
        },
        MessageId::AppBatteryLiveStatus005 => CatalogEntry {
            cs: "stav neznámý",
            en: "state unknown",
        },
        MessageId::AppDualHistoryChart001 => CatalogEntry {
            cs: "teď",
            en: "now",
        },
        MessageId::AppOffsets001 => CatalogEntry {
            cs: "smíšené",
            en: "mixed",
        },
        MessageId::AppClockEventLabel001 => CatalogEntry {
            cs: "Žádné omezení",
            en: "No limits",
        },
        MessageId::AppClockEventLabel002 => CatalogEntry {
            cs: "Žádné omezení · GPU nečinná",
            en: "No limits · GPU idle",
        },
        MessageId::AppClockEventLabel003 => CatalogEntry {
            cs: "nečinnost",
            en: "idle",
        },
        MessageId::AppClockEventLabel004 => CatalogEntry {
            cs: "aplikační takty",
            en: "application clocks",
        },
        MessageId::AppClockEventLabel005 => CatalogEntry {
            cs: "softwarový limit příkonu",
            en: "software power cap",
        },
        MessageId::AppClockEventLabel006 => CatalogEntry {
            cs: "hardwarové zpomalení",
            en: "hardware slowdown",
        },
        MessageId::AppClockEventLabel007 => CatalogEntry {
            cs: "softwarový tepelný limit",
            en: "software thermal",
        },
        MessageId::AppClockEventLabel008 => CatalogEntry {
            cs: "hardwarový tepelný limit",
            en: "hardware thermal",
        },
        MessageId::AppClockEventLabel009 => CatalogEntry {
            cs: "hardwarová výkonová brzda",
            en: "hardware power brake",
        },
        MessageId::AppClockEventLabel010 => CatalogEntry {
            cs: "limit displeje",
            en: "display clock",
        },
        MessageId::DocsLabel001 => CatalogEntry {
            cs: "Referenčně otestováno",
            en: "Reference tested",
        },
        MessageId::DocsLabel002 => CatalogEntry {
            cs: "Funkci poskytuje Linux",
            en: "Provided by Linux",
        },
        MessageId::DocsLabel003 => CatalogEntry {
            cs: "RPM poskytuje Linux, řízení ověří živý probe",
            en: "Linux provides RPM; live probe checks control",
        },
        MessageId::DocsLabel004 => CatalogEntry {
            cs: "Známý Acer controller nebo protokol",
            en: "Known Acer controller or protocol",
        },
        MessageId::DocsLabel005 => CatalogEntry {
            cs: "Zapne se jen po úspěšném živém probe",
            en: "Enabled only after a successful live probe",
        },
        MessageId::DocsLabel006 => CatalogEntry {
            cs: "O aplikaci",
            en: "About",
        },
        MessageId::DocsLabel007 => CatalogEntry {
            cs: "Použití",
            en: "Usage",
        },
        MessageId::CommonProject => CatalogEntry {
            cs: "Projekt",
            en: "Project",
        },
        MessageId::DocsModal001 => CatalogEntry {
            cs: "Informace a dokumentace",
            en: "Information and documentation",
        },
        MessageId::CommonCloseDocumentation => CatalogEntry {
            cs: "Zavřít dokumentaci",
            en: "Close documentation",
        },
        MessageId::DocsModal002 => CatalogEntry {
            cs: "Sekce dokumentace",
            en: "Documentation sections",
        },
        MessageId::DocsAboutPane001 => CatalogEntry {
            cs: "QR kód pro Bitcoin dar",
            en: "Bitcoin donation QR code",
        },
        MessageId::DocsAboutPane002 => CatalogEntry {
            cs: "QR kód pro PayPal dar",
            en: "PayPal donation QR code",
        },
        MessageId::DocsAboutPane003 => CatalogEntry {
            cs: "Dobrovolná podpora",
            en: "Optional support",
        },
        MessageId::DocsAboutPane004 => CatalogEntry {
            cs: "Podpořit ASense",
            en: "Support ASense",
        },
        MessageId::DocsAboutPane005 => CatalogEntry {
            cs: "Bitcoin mainnet nebo PayPal.Me. Dar neodemyká funkce ani nemění licenci nebo podporu.",
            en: "Bitcoin mainnet or PayPal.Me. A donation unlocks no features and changes neither the license nor support.",
        },
        MessageId::DocsAboutPane006 => CatalogEntry {
            cs: "Před odesláním porovnejte celou adresu v peněžence a posílejte pouze BTC přes Bitcoin mainnet.",
            en: "Compare the complete address in your wallet before sending, and send only BTC over Bitcoin mainnet.",
        },
        MessageId::DocsAboutPane007 => CatalogEntry {
            cs: "Verze",
            en: "Version",
        },
        MessageId::CommonLicense => CatalogEntry {
            cs: "Licence",
            en: "License",
        },
        MessageId::DocsAboutPane008 => CatalogEntry {
            cs: "Referenční model",
            en: "Reference model",
        },
        MessageId::DocsAboutPane009 => CatalogEntry {
            cs: "Co je ASense",
            en: "What ASense is",
        },
        MessageId::DocsAboutPane010 => CatalogEntry {
            cs: "ASense je nativní linuxový ovládací panel pro notebooky Acer Predator, Nitro a příbuzné modely. Nabízí profily výkonu, ventilátory, podsvícení, vybrané volby firmwaru a živou telemetrii bez PredatorSense nebo NitroSense.",
            en: "ASense is a native Linux control panel for Acer Predator, Nitro and related notebooks. It provides performance profiles, fan control, lighting, selected firmware options and live telemetry without PredatorSense or NitroSense.",
        },
        MessageId::DocsAboutPane011 => CatalogEntry {
            cs: "PHN16-72 je referenčně otestovaná platforma. Na dalších strojích ASense hledá skutečně přítomná Linux, Acer WMI a HID rozhraní a ukáže jen nalezené funkce.",
            en: "PHN16-72 is the reference-tested platform. On other systems ASense discovers the Linux, Acer WMI and HID interfaces actually present and shows only the capabilities it finds.",
        },
        MessageId::DocsAboutPane012 => CatalogEntry {
            cs: "Hlavní funkce",
            en: "Main features",
        },
        MessageId::DocsAboutPane013 => CatalogEntry {
            cs: "Volby profilů z živého rozhraní Linux kernelu nebo fallbacku známých příkazů Acer Gaming-WMI s ověřením zápisu.",
            en: "Profile choices from the live Linux kernel interface or a known-command Acer Gaming-WMI fallback with write verification.",
        },
        MessageId::DocsAboutPane014 => CatalogEntry {
            cs: "Firmware Auto, ruční CPU/GPU a Maximum ventilátory přes kernel PWM nebo Gaming-WMI.",
            en: "Firmware Auto, manual CPU/GPU and Maximum fan modes through kernel PWM or Gaming-WMI.",
        },
        MessageId::DocsAboutPane015 => CatalogEntry {
            cs: "Teploty, zátěž, až osm RPM kanálů a NVIDIA telemetrie včetně limitů a důvodů omezení.",
            en: "Temperatures, load, up to eight RPM channels and NVIDIA telemetry including limits and throttle reasons.",
        },
        MessageId::DocsAboutPane016 => CatalogEntry {
            cs: "Přesný PHN16-72 Turbo GPU preset s NVML readbackem a rollbackem.",
            en: "Exact PHN16-72 Turbo GPU preset with NVML readback and rollback.",
        },
        MessageId::DocsAboutPane017 => CatalogEntry {
            cs: "Jedno až čtyřzónové WMI a ENEK5130 podsvícení klávesnice nebo krytu.",
            en: "One-to-four-zone WMI and ENEK5130 keyboard or cover lighting.",
        },
        MessageId::DocsAboutPane018 => CatalogEntry {
            cs: "Limit a kalibrace baterie, USB při vypnutí, timeout klávesnice, startovní zvuk, LCD override a zadní logo, pokud je firmware nabízí.",
            en: "Battery limit and calibration, USB-off charging, keyboard timeout, boot sound, LCD override and rear-logo controls when firmware exposes them.",
        },
        MessageId::DocsAboutPane019 => CatalogEntry {
            cs: "Kompaktní ovládání, rozšířené grafy a hardware informace v angličtině, češtině a zjednodušené čínštině.",
            en: "Compact controls, advanced graphs and hardware information in English, Czech and Simplified Chinese.",
        },
        MessageId::DocsAboutPane020 => CatalogEntry {
            cs: "Chybějící funkce se skrývají nezávisle. Notebook může mít profily a RPM bez řízení ventilátorů nebo podsvícení bez voleb baterie.",
            en: "Missing capabilities are hidden independently. A notebook can have profiles and RPM without fan writes, or lighting without battery options.",
        },
        MessageId::DocsUsagePane001 => CatalogEntry {
            cs: "Instalace přes Ubuntu PPA",
            en: "Install through the Ubuntu PPA",
        },
        MessageId::DocsUsagePane002 => CatalogEntry {
            cs: "Doporučená instalace je spravovaná přes ASense Ubuntu PPA. APT nainstaluje aplikaci, daemon, DKMS transport a desktopovou integraci společně; Rust není potřeba.",
            en: "The recommended installation is managed through the ASense Ubuntu PPA. APT installs the application, daemon, DKMS transport and desktop integration together; Rust is not required.",
        },
        MessageId::DocsUsagePane003 => CatalogEntry {
            cs: "Otevřít PPA",
            en: "Open PPA",
        },
        MessageId::DocsUsagePane004 => CatalogEntry {
            cs: "Instalace",
            en: "Install",
        },
        MessageId::DocsUsagePane005 => CatalogEntry {
            cs: "Spuštění, diagnostika a odstranění",
            en: "Run, probe and uninstall",
        },
        MessageId::DocsUsagePane006 => CatalogEntry {
            cs: "Před spuštěním probe zavřete okno ASense, aby jednorázový dotaz mohl použít jedinou control session daemonu.",
            en: "Close the ASense window before running the probe so its one-shot request can use the daemon's single control session.",
        },
        MessageId::DocsUsagePane007 => CatalogEntry {
            cs: "Probe vytvoří autoritativní schema-3 JSON s modelem, napájením, profily, ventilátory a známými WMI/HID transporty. Daemonu po HELLO 2 posílá pouze pevný read-only požadavek DIAG PASSIVE; nevolá obecné capability discovery, neposílá ENEK selector ani setter a nic neuploaduje. Vynechává serialy, UUID, hostname, identitu uživatele, sítě, bootu a úložiště, HID fyzické cesty, journal, surové ACPI tabulky, absolutní cesty a prostředí procesu. Volba --summary je jen čitelný souhrn nové capture; JSON před sdílením zkontrolujte.",
            en: "The probe creates the authoritative schema-3 JSON with model, power, profile, fan and known WMI/HID transport evidence. After HELLO 2 it sends only the fixed read-only DIAG PASSIVE request to the daemon; it does not call general capability discovery, send an ENEK selector or setter, or upload anything. It omits serials, UUIDs, hostname, user, network, boot and storage identity, HID physical paths, journals, raw ACPI tables, absolute paths and the process environment. --summary is only a readable view of a fresh capture; review the JSON before sharing.",
        },
        MessageId::DocsUsagePane008 => CatalogEntry {
            cs: "Odinstalace vrátí aktivní fan session do Auto a odstraní služby, DKMS, HWDB, udev pravidla a desktop položku. Profil, podsvícení a další firmware volby zůstávají nastavené.",
            en: "Uninstall returns an active fan session to Auto and removes services, DKMS, HWDB, udev integration and the desktop entry. Profile, lighting and other firmware choices remain configured.",
        },
        MessageId::DocsUsagePane009 => CatalogEntry {
            cs: "DKMS používá distribuční podepisování. Pokud modul hlásí Key was rejected by service, importujte cestu klíče vypsanou DKMS a dokončete MOK enrollment po restartu.",
            en: "DKMS uses the distribution signing setup. If loading reports Key was rejected by service, import the key path printed by DKMS and complete MOK enrollment after reboot.",
        },
        MessageId::DocsUsagePane010 => CatalogEntry {
            cs: "Sestavení ze zdrojů",
            en: "Build from source",
        },
        MessageId::DocsUsagePane011 => CatalogEntry {
            cs: "Použijte Rust nainstalovaný operačním systémem; ASense jej neinstaluje, nepinuje ani nepřepisuje. Poté spusťte:",
            en: "Use the Rust toolchain installed by the operating system; ASense does not install, pin or override it. Then run:",
        },
        MessageId::DocsUsagePane012 => CatalogEntry {
            cs: "Chování ovládání",
            en: "Control behaviour",
        },
        MessageId::DocsUsagePane013 => CatalogEntry {
            cs: "Profily a WMI volby se po zápisu znovu čtou; vícekrokové chyby fan/profil používají rollback.",
            en: "Profile and WMI settings are read back; failed multi-step fan/profile changes use rollback.",
        },
        MessageId::DocsUsagePane014 => CatalogEntry {
            cs: "Ruční ventilátory jsou svázané s GUI session a při odpojení se vrátí do Auto.",
            en: "Manual fan mode is tied to the GUI session and returns to Auto after a disconnect.",
        },
        MessageId::DocsUsagePane015 => CatalogEntry {
            cs: "Potvrzené Maximum zůstane po zavření GUI; restart daemonu a resume vrátí firmware řízení do Auto.",
            en: "A confirmed Maximum remains active after GUI close; daemon restart and resume return firmware control to Auto.",
        },
        MessageId::DocsUsagePane016 => CatalogEntry {
            cs: "HID podsvícení bez getteru ukazuje po startu Neznámý stav a po zápisu Naposledy použito.",
            en: "HID lighting without a getter shows State unknown after discovery and Last applied after a successful write.",
        },
        MessageId::DocsUsagePane017 => CatalogEntry {
            cs: "Kalibrace ukazuje pouze skutečný firmware stav a živé napájení; adaptér ponechte připojený.",
            en: "Calibration shows only real firmware state and live power data; keep the AC adapter connected.",
        },
        MessageId::DocsUsagePane018 => CatalogEntry {
            cs: "GUI běží bez root práv. Typed hardwarové zápisy provádí root-owned asensed; žádná raw WMI/ACPI/EC/HID konzole neexistuje.",
            en: "The GUI is unprivileged. The root-owned asensed helper performs typed hardware writes; no raw WMI/ACPI/EC/HID console is exposed.",
        },
        MessageId::DocsHardwarePane001 => CatalogEntry {
            cs: "Podpora podle funkce",
            en: "Support by feature",
        },
        MessageId::DocsHardwarePane002 => CatalogEntry {
            cs: "Model",
            en: "Model",
        },
        MessageId::DocsHardwarePane003 => CatalogEntry {
            cs: "Profily",
            en: "Profiles",
        },
        MessageId::DocsHardwarePane004 => CatalogEntry {
            cs: "Větráky",
            en: "Fans",
        },
        MessageId::DocsHardwarePane005 => CatalogEntry {
            cs: "Volby",
            en: "Platform",
        },
        MessageId::DocsHardwarePane006 => CatalogEntry {
            cs: "Otestováno",
            en: "Tested",
        },
        MessageId::DocsHardwarePane007 => CatalogEntry {
            cs: "Známý controller",
            en: "Known controller",
        },
        MessageId::DocsHardwarePane008 => CatalogEntry {
            cs: "Živý probe",
            en: "Live probe",
        },
        MessageId::DocsHardwarePane009 => CatalogEntry {
            cs: "Potvrzeno komunitou",
            en: "Community confirmed",
        },
        MessageId::DocsHardwarePane010 => CatalogEntry {
            cs: "Zelená znamená funkci poskytovanou Linuxem. Žlutá je známý Acer protokol/controller, ale control se stejně ukáže až po správné živé odpovědi. PHN16-72 je plně referenčně otestovaný.",
            en: "Green means Linux already provides the feature. Yellow marks a known Acer protocol/controller, but the control still appears only after a valid live response. PHN16-72 is the fully reference-tested platform.",
        },
        MessageId::DocsHardwarePane011 => CatalogEntry {
            cs: "Pořadí backendů",
            en: "Backend order",
        },
        MessageId::DocsHardwarePane012 => CatalogEntry {
            cs: "Kernelové volby profilů pocházejí z živého rozhraní choices. Gaming-WMI fallback nabízí omezenou sadu známých příkazů ovladače, ne seznam vyčtený z firmwaru; probe zdroj označí jako kernel-live nebo known-gaming-wmi-commands.",
            en: "Kernel profile choices come from the live choices interface. The Gaming-WMI fallback exposes the driver's bounded known-command set, not a firmware-enumerated list; the probe labels the source as kernel-live or known-gaming-wmi-commands.",
        },
        MessageId::DocsHardwarePane013 => CatalogEntry {
            cs: "Názvy modelů nejsou allow-list. Jsou to stroje se známou kernelovou podporou nebo užiteční kandidáti k otestování; rozhoduje živé rozhraní konkrétního notebooku.",
            en: "Model names are not an allow-list. They are machines with known kernel support or useful test candidates; the live interface on the actual notebook decides availability.",
        },
        MessageId::DocsHardwarePane014 => CatalogEntry {
            cs: "Aktuální kandidáti PredatorSense",
            en: "Current PredatorSense candidates",
        },
        MessageId::DocsHardwarePane015 => CatalogEntry {
            cs: "Aktuální kandidáti NitroSense",
            en: "Current NitroSense candidates",
        },
        MessageId::DocsHardwarePane016 => CatalogEntry {
            cs: "Starší kandidáti NitroSense",
            en: "Legacy NitroSense candidates",
        },
        MessageId::DocsHardwarePane017 => CatalogEntry {
            cs: "Další kandidáti Predator a Triton",
            en: "Additional Predator and Triton candidates",
        },
        MessageId::DocsHardwarePane018 => CatalogEntry {
            cs: "Hlášené Battery/APGE modely",
            en: "Reported Battery/APGE models",
        },
        MessageId::DocsApiPane001 => CatalogEntry {
            cs: "Lokální typed API",
            en: "Local typed API",
        },
        MessageId::DocsApiPane002 => CatalogEntry {
            cs: "Nainstalovaný desktopový uživatel vlastní Unix socket /run/asense-control.sock s režimem 0600. Příkazy jsou UTF-8, ukončené newline a první příkaz musí být HELLO 2.",
            en: "The installed desktop user owns the 0600 Unix socket /run/asense-control.sock. Commands are UTF-8, newline-terminated, and the first command must be HELLO 2.",
        },
        MessageId::DocsApiPane003 => CatalogEntry {
            cs: "Očekávané odpovědi začínají OK protocol=2 a OK caps=1; druhá pokračuje capability JSONem. Každá odpověď má tvar OK <payload> nebo ERR <message>.",
            en: "Expected replies begin with OK protocol=2 and OK caps=1; the latter continues with capability JSON. Every reply is OK <payload> or ERR <message>.",
        },
        MessageId::DocsApiPane004 => CatalogEntry {
            cs: "Příkazy",
            en: "Commands",
        },
        MessageId::DocsApiPane005 => CatalogEntry {
            cs: "Limity a chování",
            en: "Limits and behaviour",
        },
        MessageId::DocsApiPane006 => CatalogEntry {
            cs: "Příkaz má nejvýše 192 bytů bez newline.",
            en: "A command is limited to 192 bytes excluding the newline.",
        },
        MessageId::DocsApiPane007 => CatalogEntry {
            cs: "Obsah odpovědi má nejvýše 4096 bytů.",
            en: "Response content is limited to 4096 bytes.",
        },
        MessageId::DocsApiPane008 => CatalogEntry {
            cs: "Běžné ERR odmítne pouze daný příkaz a session zůstane použitelná.",
            en: "A normal ERR rejects only that command and leaves the session usable.",
        },
        MessageId::DocsApiPane009 => CatalogEntry {
            cs: "CAPS dodává raw tokeny profilů, device ID a skutečně dostupné režimy; klient je nemá hádat.",
            en: "CAPS supplies raw profile tokens, device IDs and actually available modes; clients must not guess them.",
        },
        MessageId::DocsApiPane010 => CatalogEntry {
            cs: "Není potřeba klientská knihovna a neexistuje obecný raw-call příkaz.",
            en: "No client library is required and no generic raw-call command exists.",
        },
        MessageId::DocsApiPane011 => CatalogEntry {
            cs: "typed příkaz",
            en: "typed command",
        },
        MessageId::DocsProjectPane001 => CatalogEntry {
            cs: "Balík",
            en: "Package",
        },
        MessageId::DocsProjectPane002 => CatalogEntry {
            cs: "Binárky",
            en: "Binaries",
        },
        MessageId::DocsProjectPane003 => CatalogEntry {
            cs: "Knihovna",
            en: "Library",
        },
        MessageId::DocsProjectPane004 => CatalogEntry {
            cs: "Autor",
            en: "Author",
        },
        MessageId::DocsProjectPane005 => CatalogEntry {
            cs: "ASense je poskytováno tak, jak je. GUI běží bez root práv a privilegované typed operace obsluhuje samostatný asensed.",
            en: "ASense is provided as is. The GUI runs unprivileged and a separate asensed helper handles privileged typed operations.",
        },
        MessageId::DocsProjectPane006 => CatalogEntry {
            cs: "Odkazy",
            en: "Links",
        },
        MessageId::DocsProjectPane007 => CatalogEntry {
            cs: "Zdrojový repozitář",
            en: "Source repository",
        },
        MessageId::DocsProjectPane008 => CatalogEntry {
            cs: "Poslední vydání",
            en: "Latest release",
        },
        MessageId::DocsProjectPane009 => CatalogEntry {
            cs: "Vývoj a vydávání",
            en: "Development and releases",
        },
        MessageId::DocsProjectPane010 => CatalogEntry {
            cs: "Release balíky obsahují samostatné GUI a GUI-free daemon binárky, source archive a SHA-256 kontrolní součty. CI kontroluje formát, Clippy, testy, build a DKMS. Podpora přes kernel se řídí upstream acer-wmi.",
            en: "Release assets contain separate GUI and GUI-free daemon binaries, a source archive and SHA-256 checksums. CI checks formatting, Clippy, tests, builds and DKMS. Kernel-backed support follows upstream acer-wmi.",
        },
        MessageId::DocsProjectPane011 => CatalogEntry {
            cs: "Licence a původ výzkumu",
            en: "License and research credit",
        },
        MessageId::DocsProjectPane012 => CatalogEntry {
            cs: "ASense používá vlastní implementaci a testy. Veřejný výzkum wire protokolu ENEK5130 nezávisle zdokumentoval projekt predator-sense. Recovery fráze, privátní klíče ani extended privátní klíče Bitcoin peněženky nejsou v repozitáři ani release balících.",
            en: "ASense uses its own implementation and tests. The predator-sense project independently documented public ENEK5130 wire-protocol research. Bitcoin wallet recovery phrases, private keys and extended private keys are never stored in the repository or release assets.",
        },
        MessageId::DocsProjectPane013 => CatalogEntry {
            cs: "Úplný text GPL-2.0-only je v souboru LICENSE a postup vydání v docs/RELEASING.md ve zdrojovém repozitáři.",
            en: "The complete GPL-2.0-only text is in LICENSE and the release procedure is in docs/RELEASING.md in the source repository.",
        },
        MessageId::WindowMinimize => CatalogEntry {
            cs: "Minimalizovat",
            en: "Minimize",
        },
        MessageId::WindowClose => CatalogEntry {
            cs: "Zavřít",
            en: "Close",
        },
        MessageId::CoolingTelemetry => CatalogEntry {
            cs: "Telemetrie chlazení",
            en: "Cooling telemetry",
        },
        MessageId::FanModeAuto => CatalogEntry {
            cs: "Auto",
            en: "Auto",
        },
        MessageId::FanModeMaximum => CatalogEntry {
            cs: "Maximum",
            en: "Maximum",
        },
        MessageId::ProfileEco => CatalogEntry {
            cs: "Eco",
            en: "Eco",
        },
        MessageId::ProfileTurbo => CatalogEntry {
            cs: "Turbo",
            en: "Turbo",
        },
        MessageId::HardwareL3Cache => CatalogEntry {
            cs: "L3 cache",
            en: "L3 cache",
        },
        MessageId::HardwareGpuMaximum => CatalogEntry {
            cs: "GPU max",
            en: "GPU max",
        },
        MessageId::HardwareVramMaximum => CatalogEntry {
            cs: "VRAM max",
            en: "VRAM max",
        },
        MessageId::PlatformLcdOverride => CatalogEntry {
            cs: "LCD override",
            en: "LCD override",
        },
        MessageId::PlatformFirmware => CatalogEntry {
            cs: "Firmware",
            en: "Firmware",
        },
        MessageId::ClockSyncBoost => CatalogEntry {
            cs: "sync boost",
            en: "sync boost",
        },
        MessageId::DocsSecureBoot => CatalogEntry {
            cs: "Secure Boot",
            en: "Secure Boot",
        },
        MessageId::DocsRpmProbe => CatalogEntry {
            cs: "RPM + probe",
            en: "RPM + probe",
        },
        MessageId::DocsEnekResearch => CatalogEntry {
            cs: "ENEK5130 research",
            en: "ENEK5130 research",
        },
        MessageId::DocsBackendOrder => CatalogEntry {
            cs: "profily:   kernel platform_profile -> Acer Gaming-WMI -> nedostupné\nventilátory: kernel PWM -> Acer Gaming-WMI -> pouze RPM\npodsvícení: zónové WMI nebo rozpoznaný cíl ENEK5130",
            en: "profiles: kernel platform_profile -> Acer Gaming-WMI -> unavailable\nfans:     kernel PWM -> Acer Gaming-WMI -> RPM only\nlighting: zoned WMI or a detected ENEK5130 target",
        },
        MessageId::StatusAcerControlsConnected => CatalogEntry {
            cs: "Ovládání Acer připojeno",
            en: "Acer controls connected",
        },
        MessageId::StatusAcerNvidiaControlsConnected => CatalogEntry {
            cs: "Ovládání Acer + NVIDIA připojeno",
            en: "Acer + NVIDIA controls connected",
        },
        MessageId::StatusReadOnlyTelemetryConnected => CatalogEntry {
            cs: "Připojena telemetrie jen pro čtení",
            en: "Read-only telemetry connected",
        },
        MessageId::StatusConnectingControls => CatalogEntry {
            cs: "Připojuji ovládání",
            en: "Connecting controls",
        },
        MessageId::StatusPlatformRefreshed => CatalogEntry {
            cs: "Platforma znovu načtena",
            en: "Platform state refreshed",
        },
        MessageId::StatusSettingsConfirmed => CatalogEntry {
            cs: "Nastavení potvrzeno firmwarem",
            en: "Settings confirmed by firmware",
        },
        MessageId::StatusLightingConfirmed => CatalogEntry {
            cs: "Nastavení podsvícení potvrzeno firmwarem",
            en: "Lighting confirmed by firmware",
        },
        MessageId::StatusAppliedWithoutReadback => CatalogEntry {
            cs: "Použito · stav nelze přečíst",
            en: "Applied · state readback unavailable",
        },
        MessageId::StatusWritingAndVerifying => CatalogEntry {
            cs: "Zapisuji a ověřuji firmware",
            en: "Writing and verifying firmware",
        },
        MessageId::StatusProfileVerified => CatalogEntry {
            cs: "Profil potvrzen",
            en: "Profile verified",
        },
        MessageId::StatusGpuMismatch => CatalogEntry {
            cs: "GPU profil není synchronní",
            en: "GPU profile is out of sync",
        },
        MessageId::StatusPartialCapabilities => CatalogEntry {
            cs: "Částečné capabilities",
            en: "Partial capabilities",
        },
        MessageId::StatusPlatformReadbackFailed => CatalogEntry {
            cs: "Zpětné čtení platformy selhalo",
            en: "Platform readback failed",
        },
        MessageId::StatusTelemetryConnecting => CatalogEntry {
            cs: "Telemetrie se připojuje",
            en: "Telemetry connecting",
        },
        MessageId::StatusTelemetryReconnecting => CatalogEntry {
            cs: "Telemetrie se obnovuje",
            en: "Telemetry reconnecting",
        },
        MessageId::StatusRetryIn => CatalogEntry {
            cs: "další pokus za",
            en: "retry in",
        },
        MessageId::StatusInitializationFailure => CatalogEntry {
            cs: "Připojení ovládání selhalo",
            en: "Control connection failed",
        },
        MessageId::StatusFanFailure => CatalogEntry {
            cs: "Nastavení ventilátorů selhalo",
            en: "Fan setting failed",
        },
        MessageId::StatusProfileFailure => CatalogEntry {
            cs: "Nastavení profilu selhalo",
            en: "Profile setting failed",
        },
        MessageId::StatusLightingFailure => CatalogEntry {
            cs: "Nastavení podsvícení selhalo",
            en: "Lighting setting failed",
        },
        MessageId::StatusPlatformFailure => CatalogEntry {
            cs: "Nastavení platformy selhalo",
            en: "Platform setting failed",
        },
        MessageId::StatusRefreshFailure => CatalogEntry {
            cs: "Obnovení stavu selhalo",
            en: "State refresh failed",
        },
        MessageId::StatusCompactSettingsVerified => CatalogEntry {
            cs: "Nastavení potvrzeno",
            en: "Settings verified",
        },
        MessageId::StatusCompactLightingVerified => CatalogEntry {
            cs: "Podsvícení potvrzeno",
            en: "Lighting verified",
        },
        MessageId::StatusCompactLastApplied => CatalogEntry {
            cs: "Naposledy použito",
            en: "Last applied",
        },
        MessageId::StatusCompactVerifying => CatalogEntry {
            cs: "Ověřuji nastavení",
            en: "Verifying settings",
        },
        MessageId::StatusCompactPlatformRefreshed => CatalogEntry {
            cs: "Platforma obnovena",
            en: "Platform refreshed",
        },
        MessageId::StatusCompactProfileEco => CatalogEntry {
            cs: "Eco potvrzeno",
            en: "Eco verified",
        },
        MessageId::StatusCompactProfileQuiet => CatalogEntry {
            cs: "Tichý potvrzen",
            en: "Quiet verified",
        },
        MessageId::StatusCompactProfileBalanced => CatalogEntry {
            cs: "Balanc potvrzen",
            en: "Balanced verified",
        },
        MessageId::StatusCompactProfilePerformance => CatalogEntry {
            cs: "Výkon potvrzen",
            en: "Performance verified",
        },
        MessageId::StatusCompactProfileTurbo => CatalogEntry {
            cs: "Turbo potvrzeno",
            en: "Turbo verified",
        },
        MessageId::StatusCompactProfileGeneric => CatalogEntry {
            cs: "Profil potvrzen",
            en: "Profile verified",
        },
        MessageId::StatusOffsetUnavailable => CatalogEntry {
            cs: "nedostupné",
            en: "unavailable",
        },
        MessageId::StatusOffsetCustomOrPartial => CatalogEntry {
            cs: "vlastní/částečné",
            en: "custom/partial",
        },
        MessageId::StatusGpuLimitUnavailable => CatalogEntry {
            cs: "GPU limit nedostupný",
            en: "GPU limit unavailable",
        },
        MessageId::DiagnosticRgb => CatalogEntry {
            cs: "RGB",
            en: "RGB",
        },
        MessageId::DiagnosticPlatform => CatalogEntry {
            cs: "platforma",
            en: "platform",
        },
        MessageId::DiagnosticHardware => CatalogEntry {
            cs: "hardware",
            en: "hardware",
        },
        MessageId::PlatformFieldBatteryLimit => CatalogEntry {
            cs: "limit baterie",
            en: "battery limit",
        },
        MessageId::PlatformFieldBatteryCalibration => CatalogEntry {
            cs: "kalibrace baterie",
            en: "battery calibration",
        },
        MessageId::PlatformFieldUsbCharging => CatalogEntry {
            cs: "USB nabíjení",
            en: "USB charging",
        },
        MessageId::PlatformFieldKeyboardTimeout => CatalogEntry {
            cs: "timeout klávesnice",
            en: "keyboard timeout",
        },
        MessageId::PlatformFieldBootSound => CatalogEntry {
            cs: "zvuk při startu",
            en: "boot sound",
        },
        MessageId::PlatformFieldLcdOverride => CatalogEntry {
            cs: "LCD override",
            en: "LCD override",
        },
        MessageId::PlatformFieldRearLogo => CatalogEntry {
            cs: "zadní logo",
            en: "rear logo",
        },
        MessageId::DocsStandaloneRelease => CatalogEntry {
            cs: "Samostatné vydání",
            en: "Standalone release",
        },
        MessageId::DocsStandaloneReleaseBody => CatalogEntry {
            cs: "Z Releases stáhněte instalační ZIP pro Ubuntu 26.04 x86_64 a odpovídající kontrolní součet. Ověřte jej, rozbalte přesný verzovaný adresář a jako přihlášený uživatel spusťte install.sh. Rust není potřeba.",
            en: "Download the Ubuntu 26.04 x86_64 installer ZIP and matching checksum from Releases. Verify it, extract the exact versioned directory and run install.sh as the logged-in desktop user. Rust is not required.",
        },
        MessageId::DocsStandaloneReleaseLink => CatalogEntry {
            cs: "Otevřít vydání",
            en: "Open releases",
        },
        MessageId::DocsArchAur => CatalogEntry {
            cs: "Arch Linux / AUR",
            en: "Arch Linux / AUR",
        },
        MessageId::DocsArchAurBody => CatalogEntry {
            cs: "Stabilní zdrojový balíček AUR sestaví ASense a DKMS pomocí systémového toolchainu Rust. Po makepkg -si výslovně vyberte účet plochy oprávněný používat soukromý socket příkazem sudo asense-configure-user \"$USER\".",
            en: "The stable AUR source package builds ASense and DKMS with the system Rust toolchain. After makepkg -si, explicitly select the desktop account allowed to use the private socket with sudo asense-configure-user \"$USER\".",
        },
        MessageId::DocsArchAurLink => CatalogEntry {
            cs: "Otevřít balíček AUR",
            en: "Open AUR package",
        },
    }
}

const fn zh_cn(id: MessageId) -> &'static str {
    match id {
        MessageId::AppCompactStatus001 => "部分回读",
        MessageId::AppCompactStatus002 => "GPU 不匹配",
        MessageId::AppCompactStatus003 => "回滚失败",
        MessageId::AppCompactStatus004 => "状态验证失败",
        MessageId::AppCompactStatus005 => "固件不支持此功能",
        MessageId::AppCompactStatus006 => "控制服务不可用",
        MessageId::AppCompactStatus007 => "详情见上方",
        MessageId::AppLabel001 => "手动",
        MessageId::AppHint001 => "由固件控制散热",
        MessageId::AppHint002 => "自定义固定风扇转速",
        MessageId::AppHint003 => "风扇全速运行",
        MessageId::AppLabel002 => "静音",
        MessageId::AppLabel003 => "均衡",
        MessageId::AppLabel004 => "性能",
        MessageId::CommonUnavailable => "不可用",
        MessageId::AppLabel005 => "就绪",
        MessageId::AppLabel006 => "正在应用",
        MessageId::AppLabel007 => "检查",
        MessageId::AppDashboard001 => "正在连接遥测",
        MessageId::AppDashboard002 => "正在重新连接遥测",
        MessageId::AppDashboard003 => "笔记本电脑控制",
        MessageId::AppHeader001 => "关于与文档",
        MessageId::AppHeader002 => "打开信息与文档",
        MessageId::AppHeader003 => "切换语言",
        MessageId::AppHeader004 => "隐藏高级面板",
        MessageId::AppHeader005 => "显示高级面板",
        MessageId::AppHeader006 => "高级",
        MessageId::AppQuickStrip001 => "系统遥测",
        MessageId::CommonLoad => "负载",
        MessageId::AppQuickStrip002 => "性能模式",
        MessageId::CommonSleeping => "休眠",
        MessageId::CommonKeyboard => "键盘",
        MessageId::AppLightingTargetLabel001 => "顶盖徽标",
        MessageId::AppLightingTargetLabel002 => "后部徽标",
        MessageId::AppLightingTargetLabel003 => "灯带",
        MessageId::AppControlDock001 => "性能模式选项来自实时 Linux 内核接口。",
        MessageId::AppControlDock002 => "使用已知的 Acer Gaming-WMI 命令；每项更改均通过回读验证。",
        MessageId::AppControlDock003 => "固件性能模式不可用。",
        MessageId::AppControlDock004 => "背光",
        MessageId::AppControlDock005 => "固件状态",
        MessageId::AppControlDock006 => "上次应用",
        MessageId::AppControlDock007 => "状态未知",
        MessageId::AppControlDock008 => "控制中心",
        MessageId::AppControlDock009 => "Acer 性能模式",
        MessageId::AppControlDock010 => "风扇",
        MessageId::AppControlDock011 => "RGB 键盘",
        MessageId::AppControlDock012 => "RGB 模块不可用",
        MessageId::AppControlDock013 => "键盘背光电源",
        MessageId::CommonOn => "开",
        MessageId::CommonOff => "关",
        MessageId::CommonBrightness => "亮度",
        MessageId::AppControlDock014 => "静态",
        MessageId::AppControlDock015 => "呼吸",
        MessageId::AppControlDock016 => "霓虹",
        MessageId::AppControlDock017 => "风扇模式",
        MessageId::CommonApply => "应用",
        MessageId::AppControlDock018 => "风扇控制不可用",
        MessageId::AppControlDock019 => "已选择最高风扇转速",
        MessageId::AppControlDock020 => "已选择自动转速控制",
        MessageId::CommonReadError => "读取错误",
        MessageId::CommonUnsupported => "不支持",
        MessageId::AppAdvancedPanel001 => "时钟/降频原因",
        MessageId::AppAdvancedPanel002 => "回读错误",
        MessageId::AppAdvancedPanel003 => "高级系统信息",
        MessageId::AppAdvancedPanel004 => "指标",
        MessageId::AppAdvancedPanel005 => "硬件",
        MessageId::AppAdvancedPanel006 => "设备",
        MessageId::AppAdvancedPanel007 => "CPU 工作负载",
        MessageId::AppAdvancedPanel008 => "GPU 工作负载",
        MessageId::CommonVramClock => "VRAM 频率",
        MessageId::AppAdvancedPanel009 => "GPU 功率",
        MessageId::AppAdvancedPanel010 => "散热",
        MessageId::AppAdvancedPanel011 => "系统负载",
        MessageId::AppAdvancedPanel012 => "温度",
        MessageId::AppAdvancedPanel013 => "GPU 功率/限制",
        MessageId::AppAdvancedPanel014 => "功率",
        MessageId::AppAdvancedPanel015 => "限制",
        MessageId::AppAdvancedPanel016 => "GPU 时钟域",
        MessageId::AppHardwarePanel001 => "不可用",
        MessageId::AppHardwarePanel002 => "处理器",
        MessageId::CommonReadOnly => "只读",
        MessageId::AppHardwarePanel003 => "活跃核心",
        MessageId::AppHardwarePanel004 => "在线线程",
        MessageId::AppHardwarePanel005 => "活跃 P/E 核心",
        MessageId::AppHardwarePanel006 => "架构",
        MessageId::AppHardwarePanel007 => "CPU 系列",
        MessageId::AppHardwarePanel008 => "当前频率",
        MessageId::AppHardwarePanel009 => "最高频率",
        MessageId::AppHardwarePanel010 => "显卡",
        MessageId::AppHardwarePanel011 => "驱动程序",
        MessageId::AppHardwarePanel012 => "图形频率",
        MessageId::AppHardwarePanel013 => "系统内存",
        MessageId::AppHardwarePanel014 => "总计",
        MessageId::AppHardwarePanel015 => "类型",
        MessageId::AppHardwarePanel016 => "速度",
        MessageId::AppHardwarePanel017 => "通道",
        MessageId::AppHardwarePanel018 => "模块",
        MessageId::AppHardwarePanel019 => "只读内核和固件数据；不推断不可用的值。",
        MessageId::AppPlatformAdvanced001 => "等待固件读回",
        MessageId::AppPlatformAdvanced002 => "平台功能未加载",
        MessageId::AppPlatformAdvanced003 => "重新加载",
        MessageId::AppPlatformAdvanced004 => "停止",
        MessageId::AppPlatformAdvanced005 => "开始",
        MessageId::AppPlatformAdvanced006 => "校准进行中",
        MessageId::AppPlatformAdvanced007 => "固件完整电池循环",
        MessageId::AppPlatformAdvanced008 => "交流电源已连接。请在整个循环期间保持适配器连接。",
        MessageId::AppPlatformAdvanced009 => {
            "出于安全考虑，ASense 不会仅通过 USB-C 供电启动校准。请连接交流适配器。"
        }
        MessageId::AppPlatformAdvanced010 => "交流适配器已断开。请先连接适配器再开始。",
        MessageId::AppPlatformAdvanced011 => "无法验证 AC 状态。连接适配器并保持连接。",
        MessageId::AppPlatformAdvanced012 => "正在验证",
        MessageId::AppPlatformAdvanced013 => "已验证",
        MessageId::AppPlatformAdvanced014 => "电池充电上限",
        MessageId::AppPlatformAdvanced015 => "最高 80%",
        MessageId::AppPlatformAdvanced016 => "关机时 USB 充电",
        MessageId::AppPlatformAdvanced017 => "达到此电量时停止",
        MessageId::AppPlatformAdvanced018 => "电池校准",
        MessageId::AppPlatformAdvanced019 => "开机声音",
        MessageId::AppPlatformAdvanced020 => "Predator 开机动画声音",
        MessageId::AppPlatformAdvanced021 => "固件显示覆盖设置",
        MessageId::AppPlatformAdvanced022 => "键盘超时",
        MessageId::AppPlatformAdvanced023 => "自动 RGB 超时",
        MessageId::AppPlatformAdvanced024 => "后部 Predator 徽标",
        MessageId::AppPlatformAdvanced025 => "电源、颜色和亮度",
        MessageId::AppPlatformAdvanced026 => "颜色",
        MessageId::AppPlatformAdvanced027 => "刷新",
        MessageId::AppPlatformAdvanced028 => "开始电池校准？",
        MessageId::AppPlatformAdvanced029 => {
            "固件将启动一个耗时较长的完整循环。请保存工作；校准期间不要关机或挂起笔记本电脑。"
        }
        MessageId::AppPlatformAdvanced030 => {
            "固件不提供百分比或可解码的完成信号。循环结束后请刷新状态；如果仍显示为活动，请手动停止校准。"
        }
        MessageId::AppPlatformAdvanced031 => "建议在校准前关闭 80% 充电上限。",
        MessageId::AppPlatformAdvanced032 => "取消",
        MessageId::AppPlatformAdvanced033 => "开始校准",
        MessageId::AppBatteryLiveStatus001 => "充电中",
        MessageId::AppBatteryLiveStatus002 => "放电中",
        MessageId::AppBatteryLiveStatus003 => "已充满",
        MessageId::AppBatteryLiveStatus004 => "未充电",
        MessageId::AppBatteryLiveStatus005 => "状态未知",
        MessageId::AppDualHistoryChart001 => "现在",
        MessageId::AppOffsets001 => "混合",
        MessageId::AppClockEventLabel001 => "无限制",
        MessageId::AppClockEventLabel002 => "无限制 · GPU 闲置",
        MessageId::AppClockEventLabel003 => "空闲",
        MessageId::AppClockEventLabel004 => "应用程序时钟",
        MessageId::AppClockEventLabel005 => "软件功率上限",
        MessageId::AppClockEventLabel006 => "硬件降频",
        MessageId::AppClockEventLabel007 => "软件温度限制",
        MessageId::AppClockEventLabel008 => "硬件温度限制",
        MessageId::AppClockEventLabel009 => "硬件功率制动",
        MessageId::AppClockEventLabel010 => "显示时钟",
        MessageId::DocsLabel001 => "参考机型已测试",
        MessageId::DocsLabel002 => "由 Linux 提供",
        MessageId::DocsLabel003 => "RPM 由 Linux 提供；实时探测检查控制能力",
        MessageId::DocsLabel004 => "已知 Acer 控制器或协议",
        MessageId::DocsLabel005 => "仅在成功实时探测后启用",
        MessageId::DocsLabel006 => "关于",
        MessageId::DocsLabel007 => "使用",
        MessageId::CommonProject => "项目",
        MessageId::DocsModal001 => "信息与文档",
        MessageId::CommonCloseDocumentation => "关闭文档",
        MessageId::DocsModal002 => "文档章节",
        MessageId::DocsAboutPane001 => "Bitcoin 捐款二维码",
        MessageId::DocsAboutPane002 => "PayPal 捐款二维码",
        MessageId::DocsAboutPane003 => "自愿支持",
        MessageId::DocsAboutPane004 => "支持 ASense",
        MessageId::DocsAboutPane005 => {
            "可通过 Bitcoin 主网或 PayPal.Me 捐款。捐款不会解锁任何功能，也不会改变许可证或支持范围。"
        }
        MessageId::DocsAboutPane006 => {
            "发送前请在钱包中核对完整地址，并且只通过 Bitcoin 主网发送 BTC。"
        }
        MessageId::DocsAboutPane007 => "版本",
        MessageId::CommonLicense => "许可证",
        MessageId::DocsAboutPane008 => "参考型号",
        MessageId::DocsAboutPane009 => "ASense 简介",
        MessageId::DocsAboutPane010 => {
            "ASense 是面向 Acer Predator、Nitro 及相关笔记本电脑的原生 Linux 控制面板。无需 PredatorSense 或 NitroSense，即可提供性能模式、风扇控制、灯光、部分固件选项和实时遥测。"
        }
        MessageId::DocsAboutPane011 => {
            "PHN16-72 是参考测试平台。在其他系统上，ASense 会探测实际存在的 Linux、Acer WMI 和 HID 接口，并且只显示实际发现的功能。"
        }
        MessageId::DocsAboutPane012 => "主要功能",
        MessageId::DocsAboutPane013 => {
            "性能模式选项来自实时 Linux 内核接口，或来自已知 Acer Gaming-WMI 命令的回退路径，并对写入进行验证。"
        }
        MessageId::DocsAboutPane014 => {
            "通过内核 PWM 或 Gaming-WMI 提供固件自动、手动 CPU/GPU 以及 Maximum 风扇模式。"
        }
        MessageId::DocsAboutPane015 => {
            "温度、负载、最多八个 RPM 通道，以及包含限制和降频原因的 NVIDIA 遥测。"
        }
        MessageId::DocsAboutPane016 => "精确的 PHN16-72 Turbo GPU 预设，带有 NVML 读回和回滚。",
        MessageId::DocsAboutPane017 => "一至四区 WMI 灯光，以及 ENEK5130 键盘或顶盖灯光。",
        MessageId::DocsAboutPane018 => {
            "当固件提供相应接口时，可控制电池充电上限与校准、关机 USB 供电、键盘超时、开机声音、LCD 覆盖设置和后部徽标。"
        }
        MessageId::DocsAboutPane019 => "紧凑控制、高级图表和硬件信息支持英语、捷克语和简体中文。",
        MessageId::DocsAboutPane020 => {
            "缺失的功能会分别隐藏。笔记本电脑可以有性能模式和 RPM 而没有风扇写入能力，也可以有灯光而没有电池选项。"
        }
        MessageId::DocsUsagePane001 => "通过 Ubuntu PPA 安装",
        MessageId::DocsUsagePane002 => {
            "推荐通过 ASense Ubuntu PPA 管理安装和更新。APT 会一起安装应用程序、守护进程、DKMS 驱动和桌面集成；无需 Rust。"
        }
        MessageId::DocsUsagePane003 => "打开 PPA",
        MessageId::DocsUsagePane004 => "安装",
        MessageId::DocsUsagePane005 => "运行、探测和卸载",
        MessageId::DocsUsagePane006 => {
            "运行探测命令前请关闭 ASense 窗口，以便一次性请求能够使用守护进程唯一的控制会话。"
        }
        MessageId::DocsUsagePane007 => {
            "探测命令会生成权威的 schema-3 JSON，其中包含型号、电源、性能模式、风扇以及已知 WMI/HID 传输的证据。在 HELLO 2 之后，它只向守护进程发送固定的只读 DIAG PASSIVE 请求；不会调用通用功能发现，不会发送 ENEK 选择器或设置命令，也不会上传任何内容。它会排除序列号、UUID、主机名、用户、网络、启动与存储标识、HID 物理路径、日志、原始 ACPI 表、绝对路径和进程环境。--summary 只是一次新采集结果的可读摘要；共享前请自行检查 JSON。"
        }
        MessageId::DocsUsagePane008 => {
            "卸载会将活动的风扇控制会话恢复为 Auto，并删除服务、DKMS、HWDB、udev 集成和桌面条目。性能模式、灯光和其他固件选项会保留当前设置。"
        }
        MessageId::DocsUsagePane009 => {
            "DKMS 使用发行版的签名机制。如果加载模块时报告 Key was rejected by service，请导入 DKMS 输出的密钥路径，并在重启后完成 MOK 注册。"
        }
        MessageId::DocsUsagePane010 => "从源代码构建",
        MessageId::DocsUsagePane011 => {
            "使用操作系统安装的 Rust 工具链；ASense 不会安装、锁定版本或替换它。然后运行："
        }
        MessageId::DocsUsagePane012 => "控制行为",
        MessageId::DocsUsagePane013 => {
            "写入性能模式和 WMI 设置后会进行回读；多步骤风扇/性能模式更改失败时会执行回滚。"
        }
        MessageId::DocsUsagePane014 => "手动风扇模式与 GUI 会话绑定，连接断开后会恢复为 Auto。",
        MessageId::DocsUsagePane015 => {
            "已确认的 Maximum 模式在关闭 GUI 后仍保持有效；守护进程重启或系统从睡眠恢复时，会将固件控制恢复为 Auto。"
        }
        MessageId::DocsUsagePane016 => {
            "没有 getter 的 HID 灯光在发现后显示“状态未知”，成功写入后显示“上次应用”。"
        }
        MessageId::DocsUsagePane017 => {
            "校准只显示真实的固件状态和实时电源数据；请保持交流适配器连接。"
        }
        MessageId::DocsUsagePane018 => {
            "GUI 以非特权身份运行。由 root 拥有的 asensed 守护进程执行类型化硬件写入；不会暴露任何原始 WMI/ACPI/EC/HID 控制台。"
        }
        MessageId::DocsHardwarePane001 => "按功能划分的支持情况",
        MessageId::DocsHardwarePane002 => "型号",
        MessageId::DocsHardwarePane003 => "性能模式",
        MessageId::DocsHardwarePane004 => "风扇",
        MessageId::DocsHardwarePane005 => "平台功能",
        MessageId::DocsHardwarePane006 => "已测试",
        MessageId::DocsHardwarePane007 => "已知控制器",
        MessageId::DocsHardwarePane008 => "实时探测",
        MessageId::DocsHardwarePane009 => "社区确认",
        MessageId::DocsHardwarePane010 => {
            "绿色表示该功能已由 Linux 提供。黄色表示已知的 Acer 协议或控制器，但只有在收到有效的实时响应后才会显示控制项。PHN16-72 是经过完整参考测试的平台。"
        }
        MessageId::DocsHardwarePane011 => "后端优先级",
        MessageId::DocsHardwarePane012 => {
            "内核性能模式选项来自实时 choices 接口。Gaming-WMI 回退只公开驱动程序中有限的已知命令集，而不是固件枚举的列表；探测结果会将来源标记为 kernel-live 或 known-gaming-wmi-commands。"
        }
        MessageId::DocsHardwarePane013 => {
            "型号名称不是允许列表。这些型号具备已知的内核支持，或适合作为测试候选；是否可用由具体笔记本电脑上的实时接口决定。"
        }
        MessageId::DocsHardwarePane014 => "当前 PredatorSense 候选机型",
        MessageId::DocsHardwarePane015 => "当前 NitroSense 候选机型",
        MessageId::DocsHardwarePane016 => "旧版 NitroSense 候选机型",
        MessageId::DocsHardwarePane017 => "其他 Predator 和 Triton 候选机型",
        MessageId::DocsHardwarePane018 => "已报告的 Battery/APGE 型号",
        MessageId::DocsApiPane001 => "本地类型化 API",
        MessageId::DocsApiPane002 => {
            "已配置的桌面用户拥有权限为 0600 的 Unix 套接字 /run/asense-control.sock。命令采用 UTF-8、以换行符结尾，且第一条命令必须是 HELLO 2。"
        }
        MessageId::DocsApiPane003 => {
            "预期响应以 OK protocol=2 和 OK caps=1 开头；后者之后跟随 capability JSON。每个响应的格式都是 OK <payload> 或 ERR <message>。"
        }
        MessageId::DocsApiPane004 => "命令",
        MessageId::DocsApiPane005 => "限制和行为",
        MessageId::DocsApiPane006 => "一条命令的长度限制为 192 个字节（不包括换行符）。",
        MessageId::DocsApiPane007 => "响应内容限制为 4096 字节。",
        MessageId::DocsApiPane008 => "正常的 ERR 仅拒绝该命令并使会话保持可用。",
        MessageId::DocsApiPane009 => {
            "CAPS 提供原始性能模式 token、设备 ID 和实际可用的模式；客户端不得自行猜测。"
        }
        MessageId::DocsApiPane010 => "无需客户端程序库，也不存在通用的原始调用命令。",
        MessageId::DocsApiPane011 => "类型化命令",
        MessageId::DocsProjectPane001 => "软件包",
        MessageId::DocsProjectPane002 => "二进制文件",
        MessageId::DocsProjectPane003 => "程序库",
        MessageId::DocsProjectPane004 => "作者",
        MessageId::DocsProjectPane005 => {
            "ASense 按原样提供。GUI 以非特权身份运行，独立的 asensed 守护进程负责特权类型化操作。"
        }
        MessageId::DocsProjectPane006 => "链接",
        MessageId::DocsProjectPane007 => "源代码仓库",
        MessageId::DocsProjectPane008 => "最新版本",
        MessageId::DocsProjectPane009 => "开发与发布",
        MessageId::DocsProjectPane010 => {
            "发布资产包含独立的 GUI 二进制文件、无 GUI 守护进程二进制文件、源代码归档和 SHA-256 校验和。CI 会检查格式、Clippy、测试、构建和 DKMS。由内核提供的支持遵循上游 acer-wmi。"
        }
        MessageId::DocsProjectPane011 => "许可证与研究致谢",
        MessageId::DocsProjectPane012 => {
            "ASense 使用自己的实现和测试。predator-sense 项目独立记录了公开的 ENEK5130 线协议研究。Bitcoin 钱包恢复短语、私钥和扩展私钥绝不会存储在代码仓库或发布资产中。"
        }
        MessageId::DocsProjectPane013 => {
            "完整的 GPL-2.0-only 许可证文本位于 LICENSE，发布流程位于源代码仓库的 docs/RELEASING.md。"
        }
        MessageId::WindowMinimize => "最小化",
        MessageId::WindowClose => "关闭",
        MessageId::CoolingTelemetry => "散热遥测",
        MessageId::FanModeAuto => "自动",
        MessageId::FanModeMaximum => "最高转速",
        MessageId::ProfileEco => "节能",
        MessageId::ProfileTurbo => "Turbo",
        MessageId::HardwareL3Cache => "L3 缓存",
        MessageId::HardwareGpuMaximum => "GPU 最高频率",
        MessageId::HardwareVramMaximum => "VRAM 最高频率",
        MessageId::PlatformLcdOverride => "LCD 覆盖设置",
        MessageId::PlatformFirmware => "固件",
        MessageId::ClockSyncBoost => "同步加速",
        MessageId::DocsSecureBoot => "Secure Boot",
        MessageId::DocsRpmProbe => "RPM + 探测",
        MessageId::DocsEnekResearch => "ENEK5130 研究",
        MessageId::DocsBackendOrder => {
            "性能模式：kernel platform_profile -> Acer Gaming-WMI -> 不可用\n风扇：    kernel PWM -> Acer Gaming-WMI -> 仅 RPM\n灯光：    分区 WMI 或检测到的 ENEK5130 目标"
        }
        MessageId::StatusAcerControlsConnected => "Acer 控制已连接",
        MessageId::StatusAcerNvidiaControlsConnected => "Acer + NVIDIA 控制已连接",
        MessageId::StatusReadOnlyTelemetryConnected => "已连接只读遥测",
        MessageId::StatusConnectingControls => "正在连接控制服务",
        MessageId::StatusPlatformRefreshed => "平台状态已刷新",
        MessageId::StatusSettingsConfirmed => "设置已由固件确认",
        MessageId::StatusLightingConfirmed => "灯光设置已由固件确认",
        MessageId::StatusAppliedWithoutReadback => "已应用 · 状态回读不可用",
        MessageId::StatusWritingAndVerifying => "正在写入并验证固件",
        MessageId::StatusProfileVerified => "性能模式已验证",
        MessageId::StatusGpuMismatch => "GPU 性能模式不同步",
        MessageId::StatusPartialCapabilities => "部分功能可用",
        MessageId::StatusPlatformReadbackFailed => "平台回读失败",
        MessageId::StatusTelemetryConnecting => "正在连接遥测",
        MessageId::StatusTelemetryReconnecting => "正在重新连接遥测",
        MessageId::StatusRetryIn => "重试倒计时",
        MessageId::StatusInitializationFailure => "控制连接失败",
        MessageId::StatusFanFailure => "风扇设置失败",
        MessageId::StatusProfileFailure => "性能模式设置失败",
        MessageId::StatusLightingFailure => "灯光设置失败",
        MessageId::StatusPlatformFailure => "平台设置失败",
        MessageId::StatusRefreshFailure => "状态刷新失败",
        MessageId::StatusCompactSettingsVerified => "设置已验证",
        MessageId::StatusCompactLightingVerified => "灯光已验证",
        MessageId::StatusCompactLastApplied => "上次应用",
        MessageId::StatusCompactVerifying => "正在验证设置",
        MessageId::StatusCompactPlatformRefreshed => "平台已刷新",
        MessageId::StatusCompactProfileEco => "节能模式已验证",
        MessageId::StatusCompactProfileQuiet => "静音模式已验证",
        MessageId::StatusCompactProfileBalanced => "均衡模式已验证",
        MessageId::StatusCompactProfilePerformance => "性能模式已验证",
        MessageId::StatusCompactProfileTurbo => "Turbo 已验证",
        MessageId::StatusCompactProfileGeneric => "性能模式已验证",
        MessageId::StatusOffsetUnavailable => "不可用",
        MessageId::StatusOffsetCustomOrPartial => "自定义/部分",
        MessageId::StatusGpuLimitUnavailable => "GPU 限制不可用",
        MessageId::DiagnosticRgb => "RGB",
        MessageId::DiagnosticPlatform => "平台",
        MessageId::DiagnosticHardware => "硬件",
        MessageId::PlatformFieldBatteryLimit => "电池充电上限",
        MessageId::PlatformFieldBatteryCalibration => "电池校准",
        MessageId::PlatformFieldUsbCharging => "USB 充电",
        MessageId::PlatformFieldKeyboardTimeout => "键盘超时",
        MessageId::PlatformFieldBootSound => "开机声音",
        MessageId::PlatformFieldLcdOverride => "LCD 覆盖设置",
        MessageId::PlatformFieldRearLogo => "后部徽标",
        MessageId::DocsStandaloneRelease => "独立发行包",
        MessageId::DocsStandaloneReleaseBody => {
            "从 Releases 下载 Ubuntu 26.04 x86_64 安装 ZIP 及其校验和。验证后，解压准确的版本化目录，并以当前登录的桌面用户运行 install.sh；无需安装 Rust。"
        }
        MessageId::DocsStandaloneReleaseLink => "打开发布页面",
        MessageId::DocsArchAur => "Arch Linux / AUR",
        MessageId::DocsArchAurBody => {
            "稳定版 AUR 源码包使用系统提供的 Rust 工具链构建 ASense 和 DKMS。运行 makepkg -si 后，请执行 sudo asense-configure-user \"$USER\"，明确选择可访问私有套接字的桌面账户。"
        }
        MessageId::DocsArchAurLink => "打开 AUR 软件包页面",
    }
}

pub(super) const fn text(locale: LocaleId, id: MessageId) -> &'static str {
    let entry = entry(id);
    match locale {
        LocaleId::Czech => entry.cs,
        LocaleId::English => entry.en,
        LocaleId::SimplifiedChinese => zh_cn(id),
    }
}

pub(super) fn load_locale_preference() -> LocaleId {
    locale_preference_path_from(
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
    .map_or_else(LocaleId::default, |path| load_locale_from_path(&path))
}

pub(super) fn save_locale_preference(locale: LocaleId) -> io::Result<Option<PathBuf>> {
    let Some(path) = locale_preference_path_from(
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    ) else {
        return Ok(None);
    };
    save_locale_to_path(&path, locale)?;
    Ok(Some(path))
}

fn locale_preference_path_from(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(base) = xdg.map(Path::new).filter(|path| path.is_absolute()) {
        return Some(base.join("asense/ui-locale"));
    }
    home.map(Path::new)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(".config/asense/ui-locale"))
}

fn load_locale_from_path(path: &Path) -> LocaleId {
    if fs::symlink_metadata(path).is_err_and(|error| error.kind() != io::ErrorKind::NotFound) {
        return LocaleId::default();
    }
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        return LocaleId::default();
    }
    let Ok(file) = File::open(path) else {
        return LocaleId::default();
    };
    let mut bytes = Vec::new();
    if file
        .take(LOCALE_PREFERENCE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > LOCALE_PREFERENCE_MAX_BYTES
    {
        return LocaleId::default();
    }
    let Ok(value) = std::str::from_utf8(&bytes) else {
        return LocaleId::default();
    };
    let value = value.strip_suffix('\n').unwrap_or(value);
    LocaleId::parse(value).unwrap_or_default()
}

fn save_locale_to_path(path: &Path, locale: LocaleId) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "locale path has no parent"))?;
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true).mode(0o700);
    directory.create(parent)?;

    let file_name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "locale path has no file name")
    })?;
    let mut last_collision = None;
    for attempt in 0..LOCALE_TEMP_ATTEMPTS {
        let temporary = parent.join(format!(".{file_name}.tmp-{}-{attempt}", std::process::id()));
        let open = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary);
        let mut file = match open {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        };
        let operation = (|| {
            file.write_all(locale.code().as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)
        })();
        if operation.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return operation;
    }
    Err(last_collision.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "locale temporary name exhausted",
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        LOCALE_PREFERENCE_MAX_BYTES, LocaleId, MessageId, load_locale_from_path,
        locale_preference_path_from, save_locale_to_path, text,
    };
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use sha2::{Digest as _, Sha256};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn temporary_directory() -> PathBuf {
        let ordinal = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("asense-i18n-{}-{ordinal}", std::process::id()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn enabled_locales_are_exact_and_default_to_english() {
        assert_eq!(LocaleId::default(), LocaleId::English);
        assert_eq!(
            LocaleId::ENABLED,
            [
                LocaleId::English,
                LocaleId::SimplifiedChinese,
                LocaleId::Czech,
            ]
        );
        assert_eq!(LocaleId::English.code(), "en");
        assert_eq!(LocaleId::Czech.code(), "cs");
        assert_eq!(LocaleId::SimplifiedChinese.code(), "zh-CN");
        assert_eq!(LocaleId::English.display_code(), "EN");
        assert_eq!(LocaleId::Czech.display_code(), "CZ");
        assert_eq!(LocaleId::SimplifiedChinese.display_code(), "中文");
        assert_eq!(LocaleId::English.toggle(), LocaleId::SimplifiedChinese);
        assert_eq!(LocaleId::SimplifiedChinese.toggle(), LocaleId::Czech);
        assert_eq!(LocaleId::Czech.toggle(), LocaleId::English);
    }

    #[test]
    fn every_message_has_complete_static_enabled_catalog_entries() {
        let mut seen = std::collections::HashSet::new();
        for (ordinal, id) in MessageId::ALL.into_iter().enumerate() {
            assert!(seen.insert(id), "duplicate catalog ID {id:?}");
            assert_eq!(id as usize, ordinal, "catalog ID omitted or reordered");
            assert!(
                !text(LocaleId::English, id).is_empty(),
                "missing English for {id:?}"
            );
            assert!(
                !text(LocaleId::Czech, id).is_empty(),
                "missing Czech for {id:?}"
            );
            assert!(
                !text(LocaleId::SimplifiedChinese, id).is_empty(),
                "missing Simplified Chinese for {id:?}"
            );
        }
        assert_eq!(seen.len(), MessageId::ALL.len());
        assert_eq!(
            text(LocaleId::Czech, MessageId::AppHeader001),
            "O aplikaci a dokumentace"
        );
        assert_eq!(
            text(LocaleId::English, MessageId::AppHeader001),
            "About and documentation"
        );
        assert_eq!(
            text(LocaleId::Czech, MessageId::WindowMinimize),
            "Minimalizovat"
        );
        assert_eq!(
            text(LocaleId::English, MessageId::WindowMinimize),
            "Minimize"
        );
        assert_eq!(
            text(LocaleId::SimplifiedChinese, MessageId::WindowMinimize),
            "最小化"
        );
    }

    #[test]
    fn frozen_english_czech_catalog_matches_r5_1_with_two_reviewed_corrections() {
        // The first 240 IDs are the exact R5.1 direct-catalog authority. The
        // digest includes each stable variant name plus both legacy-language
        // values. Its only reviewed copy changes are the now-generic language
        // switch label and the explicit three-language About sentence.
        assert_eq!(MessageId::DocsProjectPane013 as usize, 239);
        let mut digest = Sha256::new();
        for id in MessageId::ALL.into_iter().take(240) {
            for value in [
                format!("{id:?}"),
                text(LocaleId::Czech, id).to_owned(),
                text(LocaleId::English, id).to_owned(),
            ] {
                let bytes = value.as_bytes();
                digest.update(
                    u32::try_from(bytes.len())
                        .expect("bounded catalog value length fits u32")
                        .to_le_bytes(),
                );
                digest.update(bytes);
            }
        }
        assert_eq!(
            digest.finalize().as_slice(),
            &[
                0x70, 0x9e, 0x30, 0x0d, 0x72, 0x32, 0x4d, 0x6b, 0x08, 0xfb, 0xc6, 0x57, 0xae, 0xd8,
                0x98, 0xaa, 0xe4, 0x4e, 0x83, 0xfc, 0x91, 0x18, 0xd6, 0x31, 0x52, 0xe3, 0x45, 0xa2,
                0xcd, 0x61, 0x88, 0xe3,
            ]
        );
    }

    #[test]
    fn simplified_chinese_has_no_english_fallback_entries() {
        let identical = MessageId::ALL
            .into_iter()
            .filter(|id| text(LocaleId::SimplifiedChinese, *id) == text(LocaleId::English, *id))
            .collect::<Vec<_>>();
        assert_eq!(
            identical,
            [
                MessageId::ProfileTurbo,
                MessageId::DocsSecureBoot,
                MessageId::DiagnosticRgb,
                MessageId::DocsArchAur,
            ]
        );
        for locale in LocaleId::ENABLED {
            for id in MessageId::ALL {
                assert!(
                    !text(locale, id).contains(['{', '}']),
                    "static catalog entry {locale:?}/{id:?} contains a placeholder"
                );
            }
        }
    }

    #[test]
    fn simplified_chinese_catalog_hash_is_review_authority() {
        let mut digest = Sha256::new();
        for id in MessageId::ALL {
            for value in [
                format!("{id:?}"),
                text(LocaleId::SimplifiedChinese, id).to_owned(),
            ] {
                let bytes = value.as_bytes();
                digest.update(
                    u32::try_from(bytes.len())
                        .expect("bounded catalog value length fits u32")
                        .to_le_bytes(),
                );
                digest.update(bytes);
            }
        }
        assert_eq!(
            digest.finalize().as_slice(),
            &[
                0xfd, 0x15, 0xe1, 0xaa, 0x53, 0xbf, 0xa9, 0xdb, 0x5b, 0x80, 0x77, 0x22, 0x91, 0xdc,
                0x6b, 0x5c, 0x89, 0x1d, 0xa7, 0xde, 0xc8, 0xda, 0xe6, 0x1d, 0x8e, 0x8c, 0x8a, 0x7e,
                0x77, 0xa0, 0x3c, 0x9d,
            ]
        );
    }

    #[test]
    fn protected_technical_tokens_survive_every_catalog() {
        let protected = [
            (
                MessageId::DocsUsagePane007,
                &[
                    "schema-3",
                    "JSON",
                    "WMI/HID",
                    "HELLO 2",
                    "DIAG PASSIVE",
                    "ENEK",
                    "UUID",
                    "ACPI",
                    "--summary",
                ][..],
            ),
            (
                MessageId::DocsUsagePane009,
                &["DKMS", "Key was rejected by service", "MOK"][..],
            ),
            (
                MessageId::DocsApiPane002,
                &["0600", "/run/asense-control.sock", "UTF-8", "HELLO 2"][..],
            ),
            (
                MessageId::DocsApiPane003,
                &[
                    "OK protocol=2",
                    "OK caps=1",
                    "JSON",
                    "OK <payload>",
                    "ERR <message>",
                ][..],
            ),
            (
                MessageId::DocsProjectPane012,
                &["ASense", "predator-sense", "ENEK5130", "Bitcoin"][..],
            ),
            (
                MessageId::DocsProjectPane013,
                &["GPL-2.0-only", "LICENSE", "docs/RELEASING.md"][..],
            ),
            (
                MessageId::DocsStandaloneReleaseBody,
                &[
                    "Releases",
                    "Ubuntu 26.04",
                    "x86_64",
                    "ZIP",
                    "install.sh",
                    "Rust",
                ][..],
            ),
            (
                MessageId::DocsArchAurBody,
                &[
                    "AUR",
                    "ASense",
                    "DKMS",
                    "Rust",
                    "makepkg -si",
                    "sudo asense-configure-user \"$USER\"",
                ][..],
            ),
        ];
        for locale in LocaleId::ENABLED {
            for (id, tokens) in protected {
                let rendered = text(locale, id);
                for token in tokens {
                    assert!(
                        rendered.contains(token),
                        "{locale:?}/{id:?} lost protected token {token:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn locale_switch_handler_has_no_hardware_or_socket_path() {
        let app = include_str!("../app.rs");
        let handler = app
            .split_once("on_language: move |_| {")
            .expect("Dashboard has a locale handler")
            .1
            .split_once("on_refresh: move |_| {")
            .expect("locale handler is bounded before refresh")
            .0;
        assert!(handler.contains("language().toggle()"));
        assert!(handler.contains("language.set(next)"));
        assert!(handler.contains("save_locale_preference(next)"));
        for forbidden in [
            "queue_control_request",
            "ControlAction",
            "ControlRequest",
            "control_worker",
            "ControlClient",
            "/run/asense-control.sock",
        ] {
            assert!(
                !handler.contains(forbidden),
                "locale handler reaches forbidden path {forbidden}"
            );
        }
    }

    #[test]
    fn production_surfaces_have_no_legacy_bilingual_or_prose_state_adapter() {
        let app = include_str!("../app.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("app production source exists");
        let modal = include_str!("docs_modal.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("modal production source exists");
        for (name, source) in [("app", app), ("docs modal", modal)] {
            for forbidden in [
                "fn tr(",
                "tr(language,",
                "fn localized_status(",
                "fn compact_status(",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "{name} retained forbidden legacy i18n/state adapter {forbidden:?}"
                );
            }
        }
        let app_state = app
            .split_once("struct AppState {")
            .expect("typed AppState exists")
            .1
            .split_once("impl Default for AppState")
            .expect("AppState definition is bounded")
            .0;
        assert!(!app_state.contains("status_message: String"));

        let catalog = include_str!("i18n.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("catalog production source exists");
        assert!(catalog.contains("match locale"));
        assert!(catalog.contains("LocaleId::English => entry.en"));
        assert!(catalog.contains("LocaleId::Czech => entry.cs"));
        assert!(catalog.contains("LocaleId::SimplifiedChinese => zh_cn(id)"));
        assert!(!catalog.contains("unwrap_or_else(|| entry.en"));
        assert!(!catalog.contains("unwrap_or_else(|| zh_cn"));
    }

    #[test]
    fn preference_path_uses_only_absolute_xdg_then_absolute_home() {
        assert_eq!(
            locale_preference_path_from(
                Some(OsStr::new("/tmp/xdg")),
                Some(OsStr::new("/tmp/home"))
            ),
            Some(PathBuf::from("/tmp/xdg/asense/ui-locale"))
        );
        assert_eq!(
            locale_preference_path_from(
                Some(OsStr::new("relative")),
                Some(OsStr::new("/tmp/home"))
            ),
            Some(PathBuf::from("/tmp/home/.config/asense/ui-locale"))
        );
        assert_eq!(locale_preference_path_from(None, None), None);
    }

    #[test]
    fn preference_is_bounded_exact_and_atomically_replaced() {
        let root = temporary_directory();
        let path = root.join("config/asense/ui-locale");
        assert_eq!(load_locale_from_path(&path), LocaleId::English);
        save_locale_to_path(&path, LocaleId::Czech).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"cs\n");
        assert_eq!(load_locale_from_path(&path), LocaleId::Czech);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        save_locale_to_path(&path, LocaleId::English).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"en\n");
        assert_eq!(load_locale_from_path(&path), LocaleId::English);
        save_locale_to_path(&path, LocaleId::SimplifiedChinese).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"zh-CN\n");
        assert_eq!(load_locale_from_path(&path), LocaleId::SimplifiedChinese);
        assert!(!fs::read_dir(path.parent().unwrap()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_oversized_non_utf8_and_non_file_preferences_fail_to_english() {
        let root = temporary_directory();
        let path = root.join("ui-locale");
        let invalid_values = vec![
            b"CS".to_vec(),
            b"cs ".to_vec(),
            b"cs\n\n".to_vec(),
            vec![0xff],
            vec![b'x'; LOCALE_PREFERENCE_MAX_BYTES as usize + 1],
        ];
        for bytes in invalid_values {
            fs::write(&path, bytes).unwrap();
            assert_eq!(load_locale_from_path(&path), LocaleId::English);
        }
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(load_locale_from_path(&path), LocaleId::English);
        fs::remove_dir_all(root).unwrap();
    }
}
