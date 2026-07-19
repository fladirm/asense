use dioxus::prelude::*;

use super::{Language, tr};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY_URL: &str = "https://github.com/fladirm/asense";
const RELEASE_URL: &str = "https://github.com/fladirm/asense/releases/latest";
const PPA_URL: &str = "https://launchpad.net/~fladirmacht/+archive/ubuntu/asense";
const BITCOIN_ADDRESS: &str = "bc1qqdumr0umlaak7tyrrh0jx729z272fv2jr4t5zp";
const BITCOIN_URI: &str = "bitcoin:bc1qqdumr0umlaak7tyrrh0jx729z272fv2jr4t5zp";
const PAYPAL_ACCOUNT: &str = "@fladirm";
const PAYPAL_URL: &str = "https://paypal.me/fladirm";
const PAYPAL_QR_BASE64: &str = include_str!("paypal_qr_base64.txt");

// The PNG is only 885 bytes. Embedding it as a data URI keeps the installed
// desktop binary independent from source-tree and Dioxus CLI asset paths.
const DONATE_QR_DATA_URI: &str = concat!(
    "data:image/png;base64,",
    "iVBORw0KGgoAAAANSUhEUgAAAkwAAAJMAQMAAAAyqmuAAAAABlBMVEUAAAD///+l2Z/d",
    "AAAAAnRSTlP//8i138cAAAAJcEhZcwAACxIAAAsSAdLdfvwAAAMHSURBVHic7dhbbtwwDEBR",
    "7sD736V2wMLDp+xJ2wmdjwBXMAKNTB7lh6As0ceGQEFBQUFBQUFBQUFBQf06SnIcqutcPCev",
    "dZufj721+GNPgYKCGlI9pmkntcQnNpbn9hQoKKg5ZYWZj2WdiVmw6ecTKVBQUM9Slrj9jJEr",
    "UFBQP0XFUdbB19hWoKCgfoAq8/Bm2g+0lmVUhW25UFBQAypHtNF/Py0FCgpqSG3DmuZe0Zn7",
    "Jl6hoKAeoCy9yjOc938lbn4Kh4KC+j5lXTJLuJ40Rfqk73jtzlBQUJ9T+VbiyFrrr0hfWfGZ",
    "KW0XKCioMfUm3gNc82LXCvMCv1/vQEFBDahM6fE+z2/M3Pev5QwFBfX/lNdslLMNjZOt3O5X",
    "9X6+hYKCGlCZuAkGxvAq7r01ChwKCmpIafTQc2SjXK26j5r0ctYaUFBQ36ekH1wlclsz1Tjc",
    "VnD0XygoqDlV/bS1V13lXwNa+V9bKhQU1OfUFhyOd9XWSS/FbjFQUFCPUBLn2HilldK7bVa3",
    "apY8FBTUkNJ7o5Qo5J4eZS5x1m1bQ0FBzajskq/hK1GwJR+64VBQUM9RhbRT7rWZJpsmFBTU",
    "mLJFjcOqRNOMsOu8truVMxQU1PeoCstLnrZFl7Occ18oKKg55f10bRP/27aQY6N8LygoqCF1",
    "+Fdkle0qp1Pnj9iu1zgUFNSQspe6fEXzYqe1zksJi7QAKCioIdWaZmntre+lm1ZbQ0FBDaht",
    "xIE2u2riXsXZc9siFBTUhJJojpJNs31mXopX8uvybUuFgoL6nHKtlbBXa7vk6a1WpCVCQUGN",
    "KStZXXWDqqtp0UBTsHhbh4KCmlN9VOLO5sh0C4CCgppTFZcn29tEW2OVuPDRL1oqFBTUZ5T/",
    "jJjI3ep677zaOiwUFNSc6rXsiy1Rju3qNWu//RtQUFCPUVu1RqLjRyvzVtpQUFBPUWXGxU46",
    "xQblOBQU1JiqyWr+2yNubLcPKCioAZWj1a+Vcy46mPF7/4WCgppQTwwoKCgoKCgoKCgoKCio",
    "X0T9AZqSqyWfhhW5AAAAAElFTkSuQmCC",
);

const RELEASE_INSTALL: &str = r#"sudo add-apt-repository ppa:fladirmacht/asense
sudo apt update
sudo apt install asense"#;

const SOURCE_DEPENDENCIES: &str = r#"sudo apt update
sudo apt install \
  build-essential pkg-config git dkms "linux-headers-$(uname -r)" libelf-dev \
  libgtk-3-dev libwebkit2gtk-4.1-dev libxdo-dev libssl-dev \
  desktop-file-utils python3 mokutil udev"#;

const SOURCE_BUILD: &str = r#"cargo test --locked
cargo build --release --locked --bin asensed --no-default-features
cargo build --release --locked --bin asense --features gui
./install.sh"#;

const API_EXAMPLE: &str = r#"import socket

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/run/asense-control.sock")
f = s.makefile("rwb", buffering=0)
for command in (b"HELLO 2\n", b"CAPS\n"):
    f.write(command)
    print(f.readline(4097).decode().rstrip())"#;

const API_COMMANDS: &str = r#"PING
CAPS
HARDWARE GET
PLATFORM GET
PROFILE <raw-token-from-CAPS>
FAN AUTO
FAN MAXIMUM
FAN MANUAL <cpu-20..100> <gpu-20..100>
LIGHTING APPLY <device-id> <OFF|STATIC|BREATHING|NEON> <brightness-0..100> <speed-0..9> <RRGGBB> <-|RRGGBB,...>
LIGHTING POWER <device-id> <ON|OFF>
PLATFORM <BATTERY_LIMIT|KEYBOARD_TIMEOUT|BOOT_SOUND|LCD_OVERRIDE> <ON|OFF>
PLATFORM BATTERY_CALIBRATION <START|STOP>
PLATFORM USB_CHARGING <0|10|20|30>
PLATFORM REAR_LOGO <RRGGBB> <brightness-0..100> <ON|OFF>"#;

const REPORTED_ZONED_RGB: &str = r#"AN515-45 AN515-55 AN515-56 AN515-57 AN517-41
PH315-52 PH315-53 PH315-54 PH317-53 PH517-61
PT314-51 PT315-51 PT316-51 PT515-51 PT516-52s"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SupportMark {
    Tested,
    Linux,
    LinuxProbe,
    Known,
    Probe,
}

impl SupportMark {
    const fn icon(self) -> &'static str {
        match self {
            Self::Tested => "✅",
            Self::Linux => "🟢",
            Self::LinuxProbe => "🟢·🔎",
            Self::Known => "🟡",
            Self::Probe => "🔎",
        }
    }

    fn label(self, language: Language) -> &'static str {
        match self {
            Self::Tested => tr(language, "Referenčně otestováno", "Reference tested"),
            Self::Linux => tr(language, "Funkci poskytuje Linux", "Provided by Linux"),
            Self::LinuxProbe => tr(
                language,
                "RPM poskytuje Linux, řízení ověří živý probe",
                "Linux provides RPM; live probe checks control",
            ),
            Self::Known => tr(
                language,
                "Známý Acer controller nebo protokol",
                "Known Acer controller or protocol",
            ),
            Self::Probe => tr(
                language,
                "Zapne se jen po úspěšném živém probe",
                "Enabled only after a successful live probe",
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SupportRow {
    model: &'static str,
    profiles: SupportMark,
    fans: SupportMark,
    lighting: SupportMark,
    platform: SupportMark,
}

const fn core_support_rows() -> [SupportRow; 11] {
    [
        SupportRow {
            model: "PHN16-72",
            profiles: SupportMark::Tested,
            fans: SupportMark::Tested,
            lighting: SupportMark::Tested,
            platform: SupportMark::Tested,
        },
        SupportRow {
            model: "PH16-72",
            profiles: SupportMark::Linux,
            fans: SupportMark::Linux,
            lighting: SupportMark::Probe,
            platform: SupportMark::Probe,
        },
        SupportRow {
            model: "PT14-51",
            profiles: SupportMark::Linux,
            fans: SupportMark::Linux,
            lighting: SupportMark::Probe,
            platform: SupportMark::Probe,
        },
        SupportRow {
            model: "AN515-58",
            profiles: SupportMark::Linux,
            fans: SupportMark::Linux,
            lighting: SupportMark::Known,
            platform: SupportMark::Probe,
        },
        SupportRow {
            model: "PHN16-71",
            profiles: SupportMark::Linux,
            fans: SupportMark::LinuxProbe,
            lighting: SupportMark::Probe,
            platform: SupportMark::Probe,
        },
        SupportRow {
            model: "PH16-71",
            profiles: SupportMark::Linux,
            fans: SupportMark::LinuxProbe,
            lighting: SupportMark::Probe,
            platform: SupportMark::Probe,
        },
        SupportRow {
            model: "PH18-71",
            profiles: SupportMark::Linux,
            fans: SupportMark::LinuxProbe,
            lighting: SupportMark::Probe,
            platform: SupportMark::Probe,
        },
        SupportRow {
            model: "PHN14-51",
            profiles: SupportMark::Probe,
            fans: SupportMark::Probe,
            lighting: SupportMark::Known,
            platform: SupportMark::Probe,
        },
        SupportRow {
            model: "PHN16S-71",
            profiles: SupportMark::Probe,
            fans: SupportMark::Probe,
            lighting: SupportMark::Known,
            platform: SupportMark::Probe,
        },
        SupportRow {
            model: "PHN16-73",
            profiles: SupportMark::Probe,
            fans: SupportMark::Probe,
            lighting: SupportMark::Known,
            platform: SupportMark::Probe,
        },
        SupportRow {
            model: "AN16S-61",
            profiles: SupportMark::Probe,
            fans: SupportMark::Probe,
            lighting: SupportMark::Probe,
            platform: SupportMark::Probe,
        },
    ]
}

const fn reported_zoned_row(model: &'static str) -> SupportRow {
    SupportRow {
        model,
        profiles: SupportMark::Probe,
        fans: SupportMark::Probe,
        lighting: SupportMark::Known,
        platform: SupportMark::Probe,
    }
}

const PREDATOR_CANDIDATES: &str = r#"PH16-71 PH18-71 PH3D15-71 PHN16-71 PT14-51 PT16-51 PTX17-71
PH16-72 PH18-72 PHN14-51 PHN16-72 PHN18-71 PTN16-51 T7001
PH16-73 PH18-73 PHN14-71 PHN16-73 PHN18-72 PHN16S-71 PT14-52T PTN16-71"#;

const NITRO_CANDIDATES: &str = r#"AN14-41 AN16-41 AN16-42 AN16-43 AN16-51 AN16-61 AN16-72 AN16-73
AN16S-61 AN18-61 AN17-41 AN17-42 AN17-51 AN17-71 AN17-72
ANV14-61 ANV14-62 ANV14-71 ANV15-41 ANV15-42 ANV15-51 ANV15-52
ANV16-41 ANV16-42 ANV16-61 ANV16-71 ANV16-72 ANV16S-61 ANV16S-71
ANV17-41 ANV17-61"#;

const LEGACY_NITRO_CANDIDATES: &str = r#"AN515-42 AN515-43 AN515-44 AN515-45 AN515-46 AN515-47 AN515-51s
AN515-52 AN515-53 AN515-54 AN515-55 AN515-56 AN515-57 AN515-58
AN517-41 AN517-42 AN517-43 AN517-51 AN517-52 AN517-53 AN517-54
AN517-55 AN715-41 AN715-51 AN715-52"#;

const OTHER_PREDATOR_CANDIDATES: &str = r#"PH315-52 PH315-53 PH315-54 PH315-55 PH317-53 PH317-54 PH517-51
PH517-52 PH517-61 PH717-71 PH717-72 PT314-51 PT315-51 PT314-52s
PT315-52 PT316-51 PT316-51s PT515-51 PT515-52 PT516-52s PT917-71"#;

const BATTERY_CANDIDATES: &str = r#"A315-24PT A315-44P A315-59 A315-510P A515-45 A515-46-R14K
A715-42G AG15-42P AV15-53P EUN314A-51W AN515-44 AN515-57
AN515-58 AN517-54 ANV15-51 AN16-43-R7N7 ANV16-42 PHN16-71
SF314-34 SF314-43 SFE16-44-R48X SFG14-63-R6PU SFG16-72
SFX14-71G SFX16-61G"#;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DocsTab {
    #[default]
    About,
    Usage,
    Hardware,
    Api,
    Project,
}

impl DocsTab {
    const ALL: [Self; 5] = [
        Self::About,
        Self::Usage,
        Self::Hardware,
        Self::Api,
        Self::Project,
    ];

    fn label(self, language: Language) -> &'static str {
        match self {
            Self::About => tr(language, "O aplikaci", "About"),
            Self::Usage => tr(language, "Použití", "Usage"),
            Self::Hardware => "Hardware",
            Self::Api => "API",
            Self::Project => tr(language, "Projekt", "Project"),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::About => "docs-about",
            Self::Usage => "docs-usage",
            Self::Hardware => "docs-hardware",
            Self::Api => "docs-api",
            Self::Project => "docs-project",
        }
    }
}

fn pane_class(active: DocsTab, pane: DocsTab) -> &'static str {
    if active == pane {
        "docs-pane active"
    } else {
        "docs-pane"
    }
}

#[component]
pub(super) fn DocsModal(open: bool, language: Language, on_close: EventHandler<()>) -> Element {
    let mut active_tab = use_signal(DocsTab::default);
    let current = active_tab();

    rsx! {
        div {
            class: if open { "docs-backdrop open" } else { "docs-backdrop" },
            role: "presentation",
            "aria-hidden": if open { "false" } else { "true" },
            onclick: move |_| on_close.call(()),
            onkeydown: move |event| {
                if event.key() == Key::Escape {
                    on_close.call(());
                }
            },

            article {
                class: "docs-modal",
                role: "dialog",
                "aria-modal": "true",
                "aria-labelledby": "docs-title",
                onclick: move |event| event.stop_propagation(),

                header { class: "docs-header",
                    div { class: "docs-title-copy",
                        span { class: "docs-kicker", "ASense {VERSION}" }
                        h2 { id: "docs-title", {tr(language, "Informace a dokumentace", "Information and documentation")} }
                    }
                    div { class: "docs-header-actions",
                        a {
                            class: "docs-github-link",
                            href: REPOSITORY_URL,
                            "GitHub"
                        }
                        button {
                            class: "docs-close",
                            r#type: "button",
                            title: tr(language, "Zavřít dokumentaci", "Close documentation"),
                            "aria-label": tr(language, "Zavřít dokumentaci", "Close documentation"),
                            onclick: move |_| on_close.call(()),
                            "×"
                        }
                    }
                }

                nav { class: "docs-tabs", role: "tablist", "aria-label": tr(language, "Sekce dokumentace", "Documentation sections"),
                    for tab in DocsTab::ALL {
                        button {
                            class: if current == tab { "docs-tab active" } else { "docs-tab" },
                            r#type: "button",
                            role: "tab",
                            "aria-selected": current == tab,
                            "aria-controls": tab.id(),
                            onclick: move |_| active_tab.set(tab),
                            {tab.label(language)}
                        }
                    }
                }

                div { class: "docs-content",
                    AboutPane { active: current, language }
                    UsagePane { active: current, language }
                    HardwarePane { active: current, language }
                    ApiPane { active: current, language }
                    ProjectPane { active: current, language }
                }
            }
        }
    }
}

#[component]
fn AboutPane(active: DocsTab, language: Language) -> Element {
    let paypal_qr_data_uri = format!("data:image/png;base64,{}", PAYPAL_QR_BASE64.trim());

    rsx! {
        section {
            id: DocsTab::About.id(),
            class: pane_class(active, DocsTab::About),
            role: "tabpanel",
            "aria-label": DocsTab::About.label(language),

            div { class: "docs-donate-card",
                div { class: "docs-donate-codes",
                    a {
                        class: "docs-qr-link",
                        href: BITCOIN_URI,
                        title: BITCOIN_ADDRESS,
                        div { class: "docs-qr-frame",
                            img {
                                class: "docs-qr",
                                src: DONATE_QR_DATA_URI,
                                alt: tr(language, "QR kód pro Bitcoin dar", "Bitcoin donation QR code"),
                            }
                        }
                        span { "Bitcoin" }
                    }
                    a {
                        class: "docs-qr-link",
                        href: PAYPAL_URL,
                        target: "_blank",
                        rel: "noopener noreferrer",
                        title: PAYPAL_ACCOUNT,
                        div { class: "docs-qr-frame",
                            img {
                                class: "docs-qr docs-paypal-qr",
                                src: paypal_qr_data_uri,
                                alt: tr(language, "QR kód pro PayPal dar", "PayPal donation QR code"),
                            }
                        }
                        span { "PayPal" }
                    }
                }
                div { class: "docs-donate-copy",
                    span { class: "docs-kicker", {tr(language, "Dobrovolná podpora", "Optional support")} }
                    h3 { {tr(language, "Podpořit ASense", "Support ASense")} }
                    p { {tr(
                        language,
                        "Bitcoin mainnet nebo PayPal.Me. Dar neodemyká funkce ani nemění licenci nebo podporu.",
                        "Bitcoin mainnet or PayPal.Me. A donation unlocks no features and changes neither the license nor support.",
                    )} }
                    div { class: "docs-bitcoin-address", title: BITCOIN_URI, "{BITCOIN_ADDRESS}" }
                    a {
                        class: "docs-paypal-link",
                        href: PAYPAL_URL,
                        title: PAYPAL_ACCOUNT,
                        "PayPal.Me · {PAYPAL_ACCOUNT}"
                    }
                    p { class: "docs-fine-print", {tr(
                        language,
                        "Před odesláním porovnejte celou adresu v peněžence a posílejte pouze BTC přes Bitcoin mainnet.",
                        "Compare the complete address in your wallet before sending, and send only BTC over Bitcoin mainnet.",
                    )} }
                }
            }

            div { class: "docs-version-row",
                div { span { {tr(language, "Verze", "Version")} } strong { "{VERSION}" } }
                div { span { {tr(language, "Licence", "License")} } strong { "GPL-2.0-only" } }
                div { span { {tr(language, "Referenční model", "Reference model")} } strong { "PHN16-72" } }
            }

            h3 { {tr(language, "Co je ASense", "What ASense is")} }
            p { {tr(
                language,
                "ASense je nativní linuxový ovládací panel pro notebooky Acer Predator, Nitro a příbuzné modely. Nabízí profily výkonu, ventilátory, podsvícení, vybrané volby firmwaru a živou telemetrii bez PredatorSense nebo NitroSense.",
                "ASense is a native Linux control panel for Acer Predator, Nitro and related notebooks. It provides performance profiles, fan control, lighting, selected firmware options and live telemetry without PredatorSense or NitroSense.",
            )} }
            p { {tr(
                language,
                "PHN16-72 je referenčně otestovaná platforma. Na dalších strojích ASense hledá skutečně přítomná Linux, Acer WMI a HID rozhraní a ukáže jen nalezené funkce.",
                "PHN16-72 is the reference-tested platform. On other systems ASense discovers the Linux, Acer WMI and HID interfaces actually present and shows only the capabilities it finds.",
            )} }

            h3 { {tr(language, "Hlavní funkce", "Main features")} }
            ul {
                li { {tr(language, "Volby profilů z živého rozhraní Linux kernelu nebo fallbacku známých příkazů Acer Gaming-WMI s ověřením zápisu.", "Profile choices from the live Linux kernel interface or a known-command Acer Gaming-WMI fallback with write verification.")} }
                li { {tr(language, "Firmware Auto, ruční CPU/GPU a Maximum ventilátory přes kernel PWM nebo Gaming-WMI.", "Firmware Auto, manual CPU/GPU and Maximum fan modes through kernel PWM or Gaming-WMI.")} }
                li { {tr(language, "Teploty, zátěž, až osm RPM kanálů a NVIDIA telemetrie včetně limitů a důvodů omezení.", "Temperatures, load, up to eight RPM channels and NVIDIA telemetry including limits and throttle reasons.")} }
                li { {tr(language, "Přesný PHN16-72 Turbo GPU preset s NVML readbackem a rollbackem.", "Exact PHN16-72 Turbo GPU preset with NVML readback and rollback.")} }
                li { {tr(language, "Jedno až čtyřzónové WMI a ENEK5130 podsvícení klávesnice nebo krytu.", "One-to-four-zone WMI and ENEK5130 keyboard or cover lighting.")} }
                li { {tr(language, "Limit a kalibrace baterie, USB při vypnutí, timeout klávesnice, startovní zvuk, LCD override a zadní logo, pokud je firmware nabízí.", "Battery limit and calibration, USB-off charging, keyboard timeout, boot sound, LCD override and rear-logo controls when firmware exposes them.")} }
                li { {tr(language, "Kompaktní ovládání, rozšířené grafy a hardware informace v češtině i angličtině.", "Compact controls, advanced graphs and hardware information in English and Czech.")} }
            }
            p { class: "docs-note", {tr(
                language,
                "Chybějící funkce se skrývají nezávisle. Notebook může mít profily a RPM bez řízení ventilátorů nebo podsvícení bez voleb baterie.",
                "Missing capabilities are hidden independently. A notebook can have profiles and RPM without fan writes, or lighting without battery options.",
            )} }
        }
    }
}

#[component]
fn UsagePane(active: DocsTab, language: Language) -> Element {
    rsx! {
        section {
            id: DocsTab::Usage.id(),
            class: pane_class(active, DocsTab::Usage),
            role: "tabpanel",
            "aria-label": DocsTab::Usage.label(language),

            h3 { {tr(language, "Instalace přes Ubuntu PPA", "Install through the Ubuntu PPA")} }
            p { {tr(
                language,
                "Doporučená instalace je spravovaná přes ASense Ubuntu PPA. APT nainstaluje aplikaci, daemon, DKMS transport a desktopovou integraci společně; Rust není potřeba.",
                "The recommended installation is managed through the ASense Ubuntu PPA. APT installs the application, daemon, DKMS transport and desktop integration together; Rust is not required.",
            )} }
            a { class: "docs-primary-link", href: PPA_URL, {tr(language, "Otevřít PPA", "Open PPA")} }
            h4 { {tr(language, "Instalace", "Install")} }
            pre { code { "{RELEASE_INSTALL}" } }

            h3 { {tr(language, "Spuštění, diagnostika a odstranění", "Run, probe and uninstall")} }
            pre { code { "asense\nasense probe > asense-probe.json\nsudo apt remove asense\nsudo apt purge asense" } }
            p { {tr(
                language,
                "Před spuštěním probe zavřete okno ASense, aby jednorázový dotaz mohl použít jedinou control session daemonu.",
                "Close the ASense window before running the probe so its one-shot request can use the daemon's single control session.",
            )} }
            p { {tr(
                language,
                "Probe obsahuje model, profily, ventilátory a známé WMI/HID capability. Lokálnímu daemonu posílá jen HELLO a CAPS, aby podle dostupnosti doplnil typed zóny a režimy ENEK5130; neposílá setter a bez daemonu použije pasivní HID fallback. Vynechává serial, UUID, hostname, uživatele, síťové identifikátory, journal a surové ACPI tabulky. Před sdílením jej přesto zkontrolujte.",
                "The probe contains model, profile, fan and known WMI/HID capability data. It sends only HELLO and CAPS to the local daemon to include typed ENEK5130 zones and modes when available; it sends no setter and uses passive HID fallback without the daemon. It omits serials, UUID, hostname, user and network identifiers, journals and raw ACPI tables. Review it before sharing.",
            )} }
            p { {tr(
                language,
                "Odinstalace vrátí aktivní fan session do Auto a odstraní služby, DKMS, HWDB, udev pravidla a desktop položku. Profil, podsvícení a další firmware volby zůstávají nastavené.",
                "Uninstall returns an active fan session to Auto and removes services, DKMS, HWDB, udev integration and the desktop entry. Profile, lighting and other firmware choices remain configured.",
            )} }

            h3 { "Secure Boot" }
            p { {tr(
                language,
                "DKMS používá distribuční podepisování. Pokud modul hlásí Key was rejected by service, importujte cestu klíče vypsanou DKMS a dokončete MOK enrollment po restartu.",
                "DKMS uses the distribution signing setup. If loading reports Key was rejected by service, import the key path printed by DKMS and complete MOK enrollment after reboot.",
            )} }
            pre { code { "sudo mokutil --import /var/lib/shim-signed/mok/MOK.der" } }

            h3 { {tr(language, "Sestavení ze zdrojů", "Build from source")} }
            pre { code { "{SOURCE_DEPENDENCIES}" } }
            p { {tr(language, "Nainstalujte Rust přes rustup a spusťte:", "Install Rust with rustup, then run:")} }
            pre { code { "{SOURCE_BUILD}" } }

            h3 { {tr(language, "Chování ovládání", "Control behaviour")} }
            ul {
                li { {tr(language, "Profily a WMI volby se po zápisu znovu čtou; vícekrokové chyby fan/profil používají rollback.", "Profile and WMI settings are read back; failed multi-step fan/profile changes use rollback.")} }
                li { {tr(language, "Ruční ventilátory jsou svázané s GUI session a při odpojení se vrátí do Auto.", "Manual fan mode is tied to the GUI session and returns to Auto after a disconnect.")} }
                li { {tr(language, "Potvrzené Maximum zůstane po zavření GUI; restart daemonu a resume vrátí firmware řízení do Auto.", "A confirmed Maximum remains active after GUI close; daemon restart and resume return firmware control to Auto.")} }
                li { {tr(language, "HID podsvícení bez getteru ukazuje po startu Neznámý stav a po zápisu Naposledy použito.", "HID lighting without a getter shows State unknown after discovery and Last applied after a successful write.")} }
                li { {tr(language, "Kalibrace ukazuje pouze skutečný firmware stav a živé napájení; adaptér ponechte připojený.", "Calibration shows only real firmware state and live power data; keep the AC adapter connected.")} }
                li { {tr(language, "GUI běží bez root práv. Typed hardwarové zápisy provádí root-owned asensed; žádná raw WMI/ACPI/EC/HID konzole neexistuje.", "The GUI is unprivileged. The root-owned asensed helper performs typed hardware writes; no raw WMI/ACPI/EC/HID console is exposed.")} }
            }
        }
    }
}

#[component]
fn HardwarePane(active: DocsTab, language: Language) -> Element {
    rsx! {
        section {
            id: DocsTab::Hardware.id(),
            class: pane_class(active, DocsTab::Hardware),
            role: "tabpanel",
            "aria-label": DocsTab::Hardware.label(language),

            h3 { {tr(language, "Podpora podle funkce", "Support by feature")} }
            div { class: "docs-support-matrix", role: "table",
                div { class: "docs-support-row docs-support-head", role: "row",
                    span { role: "columnheader", {tr(language, "Model", "Model")} }
                    span { role: "columnheader", {tr(language, "Profily", "Profiles")} }
                    span { role: "columnheader", {tr(language, "Větráky", "Fans")} }
                    span { role: "columnheader", "RGB" }
                    span { role: "columnheader", {tr(language, "Volby", "Platform")} }
                }
                for row in core_support_rows() {
                    SupportMatrixRow { row, language }
                }
                for model in REPORTED_ZONED_RGB.split_ascii_whitespace() {
                    SupportMatrixRow { row: reported_zoned_row(model), language }
                }
            }

            div { class: "docs-support-legend",
                span { "✅ " strong { {tr(language, "Otestováno", "Tested")} } }
                span { "🟢 " strong { "Linux" } }
                span { "🟡 " strong { {tr(language, "Známý controller", "Known controller")} } }
                span { "🔎 " strong { {tr(language, "Živý probe", "Live probe")} } }
                span { "🟢·🔎 " strong { "RPM + probe" } }
                span { "🤝 " strong { {tr(language, "Potvrzeno komunitou", "Community confirmed")} } }
            }
            p { class: "docs-note", {tr(
                language,
                "Zelená znamená funkci poskytovanou Linuxem. Žlutá je známý Acer protokol/controller, ale control se stejně ukáže až po správné živé odpovědi. PHN16-72 je plně referenčně otestovaný.",
                "Green means Linux already provides the feature. Yellow marks a known Acer protocol/controller, but the control still appears only after a valid live response. PHN16-72 is the fully reference-tested platform.",
            )} }

            h3 { {tr(language, "Pořadí backendů", "Backend order")} }
            pre { code { "profiles: kernel platform_profile -> Acer Gaming-WMI -> unavailable\nfans:     kernel PWM -> Acer Gaming-WMI -> RPM only\nlighting: zoned WMI or a detected ENEK5130 target" } }
            p { class: "docs-note", {tr(
                language,
                "Kernelové volby profilů pocházejí z živého rozhraní choices. Gaming-WMI fallback nabízí omezenou sadu známých příkazů ovladače, ne seznam vyčtený z firmwaru; probe zdroj označí jako kernel-live nebo known-gaming-wmi-commands.",
                "Kernel profile choices come from the live choices interface. The Gaming-WMI fallback exposes the driver's bounded known-command set, not a firmware-enumerated list; the probe labels the source as kernel-live or known-gaming-wmi-commands.",
            )} }
            p { class: "docs-note", {tr(
                language,
                "Názvy modelů nejsou allow-list. Jsou to stroje se známou kernelovou podporou nebo užiteční kandidáti k otestování; rozhoduje živé rozhraní konkrétního notebooku.",
                "Model names are not an allow-list. They are machines with known kernel support or useful test candidates; the live interface on the actual notebook decides availability.",
            )} }

            details { class: "docs-details",
                summary { {tr(language, "Aktuální kandidáti PredatorSense", "Current PredatorSense candidates")} }
                pre { code { "{PREDATOR_CANDIDATES}" } }
            }
            details { class: "docs-details",
                summary { {tr(language, "Aktuální kandidáti NitroSense", "Current NitroSense candidates")} }
                pre { code { "{NITRO_CANDIDATES}" } }
            }
            details { class: "docs-details",
                summary { {tr(language, "Starší kandidáti NitroSense", "Legacy NitroSense candidates")} }
                pre { code { "{LEGACY_NITRO_CANDIDATES}" } }
            }
            details { class: "docs-details",
                summary { {tr(language, "Další kandidáti Predator a Triton", "Additional Predator and Triton candidates")} }
                pre { code { "{OTHER_PREDATOR_CANDIDATES}" } }
            }
            details { class: "docs-details",
                summary { {tr(language, "Hlášené Battery/APGE modely", "Reported Battery/APGE models")} }
                pre { code { "{BATTERY_CANDIDATES}" } }
            }
        }
    }
}

#[component]
fn SupportMatrixRow(row: SupportRow, language: Language) -> Element {
    rsx! {
        div { class: "docs-support-row", role: "row",
            code { class: "docs-support-model", role: "cell", "{row.model}" }
            for mark in [row.profiles, row.fans, row.lighting, row.platform] {
                span {
                    class: "docs-support-mark",
                    role: "cell",
                    title: mark.label(language),
                    "aria-label": mark.label(language),
                    "{mark.icon()}"
                }
            }
        }
    }
}

#[component]
fn ApiPane(active: DocsTab, language: Language) -> Element {
    rsx! {
        section {
            id: DocsTab::Api.id(),
            class: pane_class(active, DocsTab::Api),
            role: "tabpanel",
            "aria-label": DocsTab::Api.label(language),

            h3 { {tr(language, "Lokální typed API", "Local typed API")} }
            p { {tr(
                language,
                "Nainstalovaný desktopový uživatel vlastní Unix socket /run/asense-control.sock s režimem 0600. Příkazy jsou UTF-8, ukončené newline a první příkaz musí být HELLO 2.",
                "The installed desktop user owns the 0600 Unix socket /run/asense-control.sock. Commands are UTF-8, newline-terminated, and the first command must be HELLO 2.",
            )} }
            h4 { "Python" }
            pre { code { "{API_EXAMPLE}" } }
            p { {tr(
                language,
                "Očekávané odpovědi začínají OK protocol=2 a OK caps=1; druhá pokračuje capability JSONem. Každá odpověď má tvar OK <payload> nebo ERR <message>.",
                "Expected replies begin with OK protocol=2 and OK caps=1; the latter continues with capability JSON. Every reply is OK <payload> or ERR <message>.",
            )} }

            h3 { {tr(language, "Příkazy", "Commands")} }
            pre { code { "{API_COMMANDS}" } }

            h3 { {tr(language, "Limity a chování", "Limits and behaviour")} }
            ul {
                li { {tr(language, "Příkaz má nejvýše 192 bytů bez newline.", "A command is limited to 192 bytes excluding the newline.")} }
                li { {tr(language, "Obsah odpovědi má nejvýše 4096 bytů.", "Response content is limited to 4096 bytes.")} }
                li { {tr(language, "Běžné ERR odmítne pouze daný příkaz a session zůstane použitelná.", "A normal ERR rejects only that command and leaves the session usable.")} }
                li { {tr(language, "CAPS dodává raw tokeny profilů, device ID a skutečně dostupné režimy; klient je nemá hádat.", "CAPS supplies raw profile tokens, device IDs and actually available modes; clients must not guess them.")} }
                li { {tr(language, "Není potřeba klientská knihovna a neexistuje obecný raw-call příkaz.", "No client library is required and no generic raw-call command exists.")} }
            }

            div { class: "docs-api-flow",
                code { "HELLO 2" }
                span { "→" }
                code { "CAPS" }
                span { "→" }
                code { {tr(language, "typed příkaz", "typed command")} }
                span { "→" }
                code { "OK / ERR" }
            }
        }
    }
}

#[component]
fn ProjectPane(active: DocsTab, language: Language) -> Element {
    rsx! {
        section {
            id: DocsTab::Project.id(),
            class: pane_class(active, DocsTab::Project),
            role: "tabpanel",
            "aria-label": DocsTab::Project.label(language),

            h3 { {tr(language, "Projekt", "Project")} }
            div { class: "docs-project-grid",
                div { span { {tr(language, "Balík", "Package")} } strong { "asense {VERSION}" } }
                div { span { "Rust" } strong { "Edition 2024" } }
                div { span { {tr(language, "Binárky", "Binaries")} } strong { "asense · asensed" } }
                div { span { {tr(language, "Knihovna", "Library")} } strong { "asense_core" } }
                div { span { {tr(language, "Autor", "Author")} } strong { "Fladirmacht" } }
                div { span { {tr(language, "Licence", "License")} } strong { "GPL-2.0-only" } }
            }
            p { {tr(
                language,
                "ASense je poskytováno tak, jak je. GUI běží bez root práv a privilegované typed operace obsluhuje samostatný asensed.",
                "ASense is provided as is. The GUI runs unprivileged and a separate asensed helper handles privileged typed operations.",
            )} }

            h3 { {tr(language, "Odkazy", "Links")} }
            div { class: "docs-links",
                a { href: REPOSITORY_URL, {tr(language, "Zdrojový repozitář", "Source repository")} }
                a { href: RELEASE_URL, {tr(language, "Poslední vydání", "Latest release")} }
                a { href: "https://github.com/torvalds/linux/blob/master/drivers/platform/x86/acer-wmi.c", "Linux acer-wmi" }
                a { href: "https://github.com/cleyton1986/predator-sense", "ENEK5130 research" }
                a { href: "mailto:fladirmacht@gmail.com", "fladirmacht@gmail.com" }
            }

            h3 { {tr(language, "Vývoj a vydávání", "Development and releases")} }
            p { {tr(
                language,
                "Release balíky obsahují samostatné GUI a GUI-free daemon binárky, source archive a SHA-256 kontrolní součty. CI kontroluje formát, Clippy, testy, build a DKMS. Podpora přes kernel se řídí upstream acer-wmi.",
                "Release assets contain separate GUI and GUI-free daemon binaries, a source archive and SHA-256 checksums. CI checks formatting, Clippy, tests, builds and DKMS. Kernel-backed support follows upstream acer-wmi.",
            )} }

            h3 { {tr(language, "Licence a původ výzkumu", "License and research credit")} }
            p { {tr(
                language,
                "ASense používá vlastní implementaci a testy. Veřejný výzkum wire protokolu ENEK5130 nezávisle zdokumentoval projekt predator-sense. Recovery fráze, privátní klíče ani extended privátní klíče Bitcoin peněženky nejsou v repozitáři ani release balících.",
                "ASense uses its own implementation and tests. The predator-sense project independently documented public ENEK5130 wire-protocol research. Bitcoin wallet recovery phrases, private keys and extended private keys are never stored in the repository or release assets.",
            )} }
            p { class: "docs-note", {tr(
                language,
                "Úplný text GPL-2.0-only je v souboru LICENSE a postup vydání v docs/RELEASING.md ve zdrojovém repozitáři.",
                "The complete GPL-2.0-only text is in LICENSE and the release procedure is in docs/RELEASING.md in the source repository.",
            )} }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_models_between(readme: &str, start: &str, end: &str) -> Vec<String> {
        let section = readme
            .split_once(start)
            .unwrap_or_else(|| panic!("README is missing section {start}"))
            .1
            .split_once(end)
            .unwrap_or_else(|| panic!("README section {start} is missing terminator {end}"))
            .0;
        section
            .lines()
            .filter_map(|line| {
                line.strip_prefix("| <code>").and_then(|row| {
                    row.split_once("</code> |")
                        .map(|(model, _)| model.replace("&#8209;", "-"))
                })
            })
            .collect()
    }

    #[test]
    fn documentation_tabs_have_stable_ids_and_localized_labels() {
        let mut ids = std::collections::BTreeSet::new();
        for tab in DocsTab::ALL {
            assert!(ids.insert(tab.id()));
            assert!(!tab.label(Language::Czech).is_empty());
            assert!(!tab.label(Language::English).is_empty());
        }
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn embedded_donation_identity_is_consistent() {
        assert!(BITCOIN_URI.ends_with(BITCOIN_ADDRESS));
        assert_eq!(PAYPAL_URL, "https://paypal.me/fladirm");
        assert_eq!(PAYPAL_ACCOUNT, "@fladirm");
        assert!(DONATE_QR_DATA_URI.starts_with("data:image/png;base64,"));
        assert!(DONATE_QR_DATA_URI.len() > 1_000);
        assert!(PAYPAL_QR_BASE64.trim().len() > 10_000);
        assert!(!PAYPAL_QR_BASE64.trim().contains(char::is_whitespace));
    }

    #[test]
    fn embedded_api_matches_protocol_contract() {
        assert!(API_EXAMPLE.contains("HELLO 2"));
        assert!(API_EXAMPLE.contains("CAPS"));
        assert!(API_COMMANDS.contains("LIGHTING POWER"));
        assert!(API_COMMANDS.contains("BATTERY_CALIBRATION"));
        assert!(API_COMMANDS.contains("REAR_LOGO"));
    }

    #[test]
    fn modal_and_readme_support_matrices_have_identical_models_and_order() {
        let readme = include_str!("../../README.md");
        let mut modal_models = core_support_rows()
            .into_iter()
            .map(|row| row.model.to_owned())
            .collect::<Vec<_>>();
        modal_models.extend(
            REPORTED_ZONED_RGB
                .split_ascii_whitespace()
                .map(str::to_owned),
        );
        let readme_models = table_models_between(readme, "## Supported hardware", "**Legend:**");
        assert_eq!(readme_models, modal_models);
    }

    #[test]
    fn modal_trigger_and_panes_preserve_navigation_state_by_structure() {
        let app_source = include_str!("../app.rs");
        let header = app_source
            .split("fn AppHeader")
            .nth(1)
            .unwrap()
            .split("fn QuickStrip")
            .next()
            .unwrap();
        assert!(header.find("info-toggle").unwrap() < header.find("language-toggle").unwrap());
        assert!(app_source.contains("docs_modal::DocsModal"));
        assert!(super::super::APP_CSS_SOURCE.contains(".docs-backdrop.open"));
        assert!(super::super::APP_CSS_SOURCE.contains(".docs-pane.active"));
        assert!(super::super::APP_CSS_SOURCE.contains("overflow-y: auto"));
    }
}
