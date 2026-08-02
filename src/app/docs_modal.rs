use dioxus::prelude::*;

use super::{Language, MessageId, text};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY_URL: &str = "https://github.com/fladirm/asense";
const RELEASE_URL: &str = "https://github.com/fladirm/asense/releases/latest";
const PPA_URL: &str = "https://launchpad.net/~fladirmacht/+archive/ubuntu/asense";
const AUR_URL: &str = "https://aur.archlinux.org/packages/asense";
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

const STANDALONE_INSTALL: &str = concat!(
    "sha256sum --check asense-v",
    env!("CARGO_PKG_VERSION"),
    "-ubuntu-26.04-x86_64-installer-*.zip.sha256\n",
    "unzip asense-v",
    env!("CARGO_PKG_VERSION"),
    "-ubuntu-26.04-x86_64-installer-*.zip\n",
    "cd asense-v",
    env!("CARGO_PKG_VERSION"),
    "-ubuntu-26.04-x86_64-installer-*/\n",
    "./install.sh",
);

const AUR_INSTALL: &str = r#"git clone https://aur.archlinux.org/asense.git
cd asense
makepkg -si
sudo asense-configure-user "$USER""#;

const SOURCE_DEPENDENCIES: &str = r#"sudo apt update
sudo apt install \
  build-essential rustc cargo pkg-config git dkms \
  "linux-headers-$(uname -r)" libelf-dev \
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
            Self::Tested => text(language, MessageId::DocsLabel001),
            Self::Linux => text(language, MessageId::DocsLabel002),
            Self::LinuxProbe => text(language, MessageId::DocsLabel003),
            Self::Known => text(language, MessageId::DocsLabel004),
            Self::Probe => text(language, MessageId::DocsLabel005),
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
            Self::About => text(language, MessageId::DocsLabel006),
            Self::Usage => text(language, MessageId::DocsLabel007),
            Self::Hardware => text(language, MessageId::AppAdvancedPanel005),
            Self::Api => "API",
            Self::Project => text(language, MessageId::CommonProject),
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
                        h2 { id: "docs-title", {text(language, MessageId::DocsModal001)} }
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
                            title: text(language, MessageId::CommonCloseDocumentation),
                            "aria-label": text(language, MessageId::CommonCloseDocumentation),
                            onclick: move |_| on_close.call(()),
                            "×"
                        }
                    }
                }

                nav { class: "docs-tabs", role: "tablist", "aria-label": text(language, MessageId::DocsModal002),
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
                                alt: text(language, MessageId::DocsAboutPane001),
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
                                alt: text(language, MessageId::DocsAboutPane002),
                            }
                        }
                        span { "PayPal" }
                    }
                }
                div { class: "docs-donate-copy",
                    span { class: "docs-kicker", {text(language, MessageId::DocsAboutPane003)} }
                    h3 { {text(language, MessageId::DocsAboutPane004)} }
                    p { {text(language, MessageId::DocsAboutPane005)} }
                    div { class: "docs-bitcoin-address", title: BITCOIN_URI, "{BITCOIN_ADDRESS}" }
                    a {
                        class: "docs-paypal-link",
                        href: PAYPAL_URL,
                        title: PAYPAL_ACCOUNT,
                        "PayPal.Me · {PAYPAL_ACCOUNT}"
                    }
                    p { class: "docs-fine-print", {text(language, MessageId::DocsAboutPane006)} }
                }
            }

            div { class: "docs-version-row",
                div { span { {text(language, MessageId::DocsAboutPane007)} } strong { "{VERSION}" } }
                div { span { {text(language, MessageId::CommonLicense)} } strong { "GPL-2.0-only" } }
                div { span { {text(language, MessageId::DocsAboutPane008)} } strong { "PHN16-72" } }
            }

            h3 { {text(language, MessageId::DocsAboutPane009)} }
            p { {text(language, MessageId::DocsAboutPane010)} }
            p { {text(language, MessageId::DocsAboutPane011)} }

            h3 { {text(language, MessageId::DocsAboutPane012)} }
            ul {
                li { {text(language, MessageId::DocsAboutPane013)} }
                li { {text(language, MessageId::DocsAboutPane014)} }
                li { {text(language, MessageId::DocsAboutPane015)} }
                li { {text(language, MessageId::DocsAboutPane016)} }
                li { {text(language, MessageId::DocsAboutPane017)} }
                li { {text(language, MessageId::DocsAboutPane018)} }
                li { {text(language, MessageId::DocsAboutPane019)} }
            }
            p { class: "docs-note", {text(language, MessageId::DocsAboutPane020)} }
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

            h3 { {text(language, MessageId::DocsUsagePane001)} }
            p { {text(language, MessageId::DocsUsagePane002)} }
            a { class: "docs-primary-link", href: PPA_URL, {text(language, MessageId::DocsUsagePane003)} }
            h4 { {text(language, MessageId::DocsUsagePane004)} }
            pre { code { "{RELEASE_INSTALL}" } }

            h3 { {text(language, MessageId::DocsStandaloneRelease)} }
            p { {text(language, MessageId::DocsStandaloneReleaseBody)} }
            a { class: "docs-primary-link", href: RELEASE_URL, {text(language, MessageId::DocsStandaloneReleaseLink)} }
            pre { code { "{STANDALONE_INSTALL}" } }

            h3 { {text(language, MessageId::DocsArchAur)} }
            p { {text(language, MessageId::DocsArchAurBody)} }
            a { class: "docs-primary-link", href: AUR_URL, {text(language, MessageId::DocsArchAurLink)} }
            pre { code { "{AUR_INSTALL}" } }

            h3 { {text(language, MessageId::DocsUsagePane005)} }
            pre { code { "asense\nasense probe > asense-probe.json\nasense probe --summary\nsudo apt remove asense\nsudo apt purge asense" } }
            p { {text(language, MessageId::DocsUsagePane006)} }
            p { {text(language, MessageId::DocsUsagePane007)} }
            p { {text(language, MessageId::DocsUsagePane008)} }

            h3 { {text(language, MessageId::DocsSecureBoot)} }
            p { {text(language, MessageId::DocsUsagePane009)} }
            pre { code { "sudo mokutil --import /var/lib/shim-signed/mok/MOK.der" } }

            h3 { {text(language, MessageId::DocsUsagePane010)} }
            pre { code { "{SOURCE_DEPENDENCIES}" } }
            p { {text(language, MessageId::DocsUsagePane011)} }
            pre { code { "{SOURCE_BUILD}" } }

            h3 { {text(language, MessageId::DocsUsagePane012)} }
            ul {
                li { {text(language, MessageId::DocsUsagePane013)} }
                li { {text(language, MessageId::DocsUsagePane014)} }
                li { {text(language, MessageId::DocsUsagePane015)} }
                li { {text(language, MessageId::DocsUsagePane016)} }
                li { {text(language, MessageId::DocsUsagePane017)} }
                li { {text(language, MessageId::DocsUsagePane018)} }
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

            h3 { {text(language, MessageId::DocsHardwarePane001)} }
            div { class: "docs-support-matrix", role: "table",
                div { class: "docs-support-row docs-support-head", role: "row",
                    span { role: "columnheader", {text(language, MessageId::DocsHardwarePane002)} }
                    span { role: "columnheader", {text(language, MessageId::DocsHardwarePane003)} }
                    span { role: "columnheader", {text(language, MessageId::DocsHardwarePane004)} }
                    span { role: "columnheader", "RGB" }
                    span { role: "columnheader", {text(language, MessageId::DocsHardwarePane005)} }
                }
                for row in core_support_rows() {
                    SupportMatrixRow { row, language }
                }
                for model in REPORTED_ZONED_RGB.split_ascii_whitespace() {
                    SupportMatrixRow { row: reported_zoned_row(model), language }
                }
            }

            div { class: "docs-support-legend",
                span { "✅ " strong { {text(language, MessageId::DocsHardwarePane006)} } }
                span { "🟢 " strong { "Linux" } }
                span { "🟡 " strong { {text(language, MessageId::DocsHardwarePane007)} } }
                span { "🔎 " strong { {text(language, MessageId::DocsHardwarePane008)} } }
                span { "🟢·🔎 " strong { {text(language, MessageId::DocsRpmProbe)} } }
                span { "🤝 " strong { {text(language, MessageId::DocsHardwarePane009)} } }
            }
            p { class: "docs-note", {text(language, MessageId::DocsHardwarePane010)} }

            h3 { {text(language, MessageId::DocsHardwarePane011)} }
            pre { code { {text(language, MessageId::DocsBackendOrder)} } }
            p { class: "docs-note", {text(language, MessageId::DocsHardwarePane012)} }
            p { class: "docs-note", {text(language, MessageId::DocsHardwarePane013)} }

            details { class: "docs-details",
                summary { {text(language, MessageId::DocsHardwarePane014)} }
                pre { code { "{PREDATOR_CANDIDATES}" } }
            }
            details { class: "docs-details",
                summary { {text(language, MessageId::DocsHardwarePane015)} }
                pre { code { "{NITRO_CANDIDATES}" } }
            }
            details { class: "docs-details",
                summary { {text(language, MessageId::DocsHardwarePane016)} }
                pre { code { "{LEGACY_NITRO_CANDIDATES}" } }
            }
            details { class: "docs-details",
                summary { {text(language, MessageId::DocsHardwarePane017)} }
                pre { code { "{OTHER_PREDATOR_CANDIDATES}" } }
            }
            details { class: "docs-details",
                summary { {text(language, MessageId::DocsHardwarePane018)} }
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

            h3 { {text(language, MessageId::DocsApiPane001)} }
            p { {text(language, MessageId::DocsApiPane002)} }
            h4 { "Python" }
            pre { code { "{API_EXAMPLE}" } }
            p { {text(language, MessageId::DocsApiPane003)} }

            h3 { {text(language, MessageId::DocsApiPane004)} }
            pre { code { "{API_COMMANDS}" } }

            h3 { {text(language, MessageId::DocsApiPane005)} }
            ul {
                li { {text(language, MessageId::DocsApiPane006)} }
                li { {text(language, MessageId::DocsApiPane007)} }
                li { {text(language, MessageId::DocsApiPane008)} }
                li { {text(language, MessageId::DocsApiPane009)} }
                li { {text(language, MessageId::DocsApiPane010)} }
            }

            div { class: "docs-api-flow",
                code { "HELLO 2" }
                span { "→" }
                code { "CAPS" }
                span { "→" }
                code { {text(language, MessageId::DocsApiPane011)} }
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

            h3 { {text(language, MessageId::CommonProject)} }
            div { class: "docs-project-grid",
                div { span { {text(language, MessageId::DocsProjectPane001)} } strong { "asense {VERSION}" } }
                div { span { "Rust" } strong { "Edition 2024" } }
                div { span { {text(language, MessageId::DocsProjectPane002)} } strong { "asense · asensed" } }
                div { span { {text(language, MessageId::DocsProjectPane003)} } strong { "asense_core" } }
                div { span { {text(language, MessageId::DocsProjectPane004)} } strong { "Fladirmacht" } }
                div { span { {text(language, MessageId::CommonLicense)} } strong { "GPL-2.0-only" } }
            }
            p { {text(language, MessageId::DocsProjectPane005)} }

            h3 { {text(language, MessageId::DocsProjectPane006)} }
            div { class: "docs-links",
                a { href: REPOSITORY_URL, {text(language, MessageId::DocsProjectPane007)} }
                a { href: RELEASE_URL, {text(language, MessageId::DocsProjectPane008)} }
                a { href: "https://github.com/torvalds/linux/blob/master/drivers/platform/x86/acer-wmi.c", "Linux acer-wmi" }
                a { href: "https://github.com/cleyton1986/predator-sense", {text(language, MessageId::DocsEnekResearch)} }
                a { href: "mailto:fladirmacht@gmail.com", "fladirmacht@gmail.com" }
            }

            h3 { {text(language, MessageId::DocsProjectPane009)} }
            p { {text(language, MessageId::DocsProjectPane010)} }

            h3 { {text(language, MessageId::DocsProjectPane011)} }
            p { {text(language, MessageId::DocsProjectPane012)} }
            p { class: "docs-note", {text(language, MessageId::DocsProjectPane013)} }
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
            assert!(!tab.label(Language::SimplifiedChinese).is_empty());
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
    fn embedded_install_authorities_match_current_release_and_readme() {
        let readme = include_str!("../../README.md");
        let versioned_installer = format!("asense-v{VERSION}-ubuntu-26.04-x86_64-installer-*.zip");
        assert!(STANDALONE_INSTALL.contains(&versioned_installer));
        assert!(readme.contains(&versioned_installer));
        assert!(STANDALONE_INSTALL.contains("./install.sh"));
        assert!(AUR_INSTALL.contains("https://aur.archlinux.org/asense.git"));
        assert!(AUR_INSTALL.contains("makepkg -si"));
        assert!(AUR_INSTALL.contains("sudo asense-configure-user \"$USER\""));
        assert!(readme.contains(AUR_URL));
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
