//! Small, read-only capability report for community hardware issues.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::control::{ControlCapabilities, ControlClient};
use crate::hardware::{AcerHardware, FanBackend, ProfileBackend, discover_acer_hwmon};
use crate::lighting::enek5130::read_only_target_ids;
use crate::lighting::{LightingBackend, LightingTarget};
use crate::platform::{PlatformControls, find_wmi_device, find_wmi_group};

const SCHEMA: u8 = 2;
const MAX_TEXT_BYTES: u64 = 4096;
const MAX_ITEMS: usize = 32;
const GAMING_GUID: &str = "7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56";
const BATTERY_GUID: &str = "79772EC5-04B1-4BFD-843C-61E7F77B6CC9";
const APGE_GUID: &str = "61EF69EA-865C-4BC3-A502-A0DEBA0CB531";
const HID_BUS_I2C: u16 = 0x0018;
const ENEK_VENDOR: u16 = 0x0cf2;
const ENEK_PRODUCT: u16 = 0x5130;
const ENEK_TARGETS_UNQUERIED_REASON: &str =
    "A3 capabilities require A2 target selection through a write ioctl; read-only probe skips A2";
const ENEK_A1_UNQUERIED_REASON: &str = "A1 target list is unavailable; A3 capabilities require A2 \
    target selection through a write ioctl; read-only probe skips A2";

pub fn generate() -> Result<String, String> {
    let control = ControlClient::connect()
        .and_then(|mut client| client.capabilities())
        .ok();
    generate_at_with_capabilities(Path::new("/"), control.as_ref())
}

/// Fixture-friendly form; the desktop CLI never exposes an alternate root.
pub fn generate_at(root: &Path) -> Result<String, String> {
    generate_at_with_capabilities(root, None)
}

/// Keeps filesystem fixtures independent of the host daemon while allowing
/// tests to inject the same typed capability snapshot returned by `CAPS`.
fn generate_at_with_capabilities(
    root: &Path,
    control: Option<&ControlCapabilities>,
) -> Result<String, String> {
    let hardware = AcerHardware::discover_at(root)
        .ok()
        .map(|item| item.capabilities());
    let profiles = hardware.as_ref().map(|item| &item.profiles);
    let fans = hardware.as_ref().map(|item| &item.fans);
    let wmi_root = rooted(root, "sys/bus/wmi/devices");
    let platform = PlatformControls::discover_at(&wmi_root)
        .ok()
        .map(|item| item.capabilities())
        .unwrap_or_default();
    let known_guids = [GAMING_GUID, BATTERY_GUID, APGE_GUID]
        .into_iter()
        .filter(|guid| find_wmi_device(&wmi_root, guid).is_some())
        .collect::<Vec<_>>();
    let hwmon = inspect_hwmon(root);

    let report = json!({
        "schema": SCHEMA,
        "asense": env!("CARGO_PKG_VERSION"),
        "dmi": {
            "vendor": read_dmi(root, "sys_vendor"),
            "product": read_dmi(root, "product_name"),
            "board": read_dmi(root, "board_name"),
            "bios": read_dmi(root, "bios_version"),
        },
        "profiles": {
            "backend": profiles.and_then(|value| value.backend).map(profile_backend),
            "choices_source": profiles.and_then(|value| value.backend).map(profile_choices_source),
            "choices": profiles.map(|value| value.choices.iter().map(|choice| json!({
                "raw": choice.raw,
                "label": choice.label,
                "selectable": choice.selectable,
            })).collect::<Vec<_>>()),
            "current": profiles.and_then(|value| value.current.as_deref()),
        },
        "hwmon": {
            "name": hwmon.as_ref().map(|value| value.name.as_str()),
            "fan_backend": fans.and_then(|value| value.backend).map(fan_backend),
            "fan_inputs": fans.map_or_else(Vec::new, |value| value.rpm_channels.iter().map(|fan| json!({
                "index": fan.index,
                "label": fan.label,
                "rpm": fan.rpm,
            })).collect::<Vec<_>>()),
            "temps": hwmon.as_ref().map_or_else(Vec::new, |value| value.temps.clone()),
            "pwm_nodes": hwmon.as_ref().map_or_else(Vec::new, |value| value.pwm_nodes.clone()),
        },
        "wmi": {
            "known_guids": known_guids,
            "gaming_fan": fans.is_some_and(|value| value.backend == Some(FanBackend::AcerGamingWmi)),
            "gaming_profile": profiles.is_some_and(|value| value.backend == Some(ProfileBackend::AcerGamingWmi)),
            "zoned_rgb": (["rgb_zoned", "asense_rgb"]
                .into_iter()
                .any(|group| find_wmi_group(&wmi_root, GAMING_GUID, group).is_some())),
            "battery_limit": platform.battery_limit,
            "battery_calibration": platform.battery_calibration,
            "usb_off_charging": platform.usb_off_charging,
            "keyboard_timeout": platform.keyboard_timeout,
            "boot_sound": platform.boot_sound,
            "lcd_override": platform.lcd_override,
            "rear_logo": platform.rear_logo,
        },
        "lighting": inspect_lighting(control),
        "hid": inspect_hid(root),
    });
    serde_json::to_string_pretty(&report)
        .map(|mut output| {
            output.push('\n');
            output
        })
        .map_err(|error| format!("cannot encode capability report: {error}"))
}

fn profile_backend(backend: ProfileBackend) -> &'static str {
    match backend {
        ProfileBackend::Kernel => "kernel",
        ProfileBackend::AcerGamingWmi => "gaming-wmi",
    }
}

fn profile_choices_source(backend: ProfileBackend) -> &'static str {
    match backend {
        ProfileBackend::Kernel => "kernel-live",
        ProfileBackend::AcerGamingWmi => "known-gaming-wmi-commands",
    }
}

fn fan_backend(backend: FanBackend) -> &'static str {
    match backend {
        FanBackend::KernelPwm => "kernel-pwm",
        FanBackend::AcerGamingWmi => "gaming-wmi",
    }
}

fn read_dmi(root: &Path, name: &str) -> Option<String> {
    read_text(&rooted(root, "sys/class/dmi/id").join(name))
}

struct HwmonReport {
    name: String,
    temps: Vec<Value>,
    pwm_nodes: Vec<String>,
}

fn inspect_hwmon(root: &Path) -> Option<HwmonReport> {
    let path = discover_acer_hwmon(root)?;
    let name = read_text(&path.join("name"))?;
    let temps = (1..=MAX_ITEMS)
        .filter_map(|index| {
            let millidegrees_c = read_number(&path.join(format!("temp{index}_input")))?;
            let label = read_text(&path.join(format!("temp{index}_label")))
                .unwrap_or_else(|| format!("Temperature {index}"));
            Some(json!({
                "index": index,
                "label": label,
                "millidegrees_c": millidegrees_c,
            }))
        })
        .collect();
    let pwm_nodes = sorted_entries(&path)
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            is_pwm_node(name).then(|| name.to_string())
        })
        .take(MAX_ITEMS)
        .collect();

    Some(HwmonReport {
        name,
        temps,
        pwm_nodes,
    })
}

fn is_pwm_node(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("pwm") else {
        return false;
    };
    let digits = suffix.strip_suffix("_enable").unwrap_or(suffix);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn read_number(path: &Path) -> Option<i64> {
    read_text(path)?.parse().ok()
}

fn inspect_hid(root: &Path) -> Vec<Value> {
    sorted_entries(&rooted(root, "sys/class/hidraw"))
        .into_iter()
        .filter_map(|path| {
            let uevent = read_text(&path.join("device/uevent"))?;
            let (name, bus, vendor, product) = parse_hid_uevent(&uevent)?;
            if bus != HID_BUS_I2C || vendor != ENEK_VENDOR || product != ENEK_PRODUCT {
                return None;
            }
            let target_ids = path
                .file_name()
                .map(|name| rooted(root, "dev").join(name))
                .and_then(|node| read_only_target_ids(&node).ok());
            let targets_unqueried = target_ids.is_none();
            let targets = target_ids.map_or_else(Vec::new, enek_target_reports);
            let reason = if targets_unqueried {
                ENEK_A1_UNQUERIED_REASON
            } else {
                ENEK_TARGETS_UNQUERIED_REASON
            };
            Some(json!({
                "name": name,
                "vid": "0cf2",
                "pid": "5130",
                "targets": targets,
                "targets_unqueried": targets_unqueried,
                "capabilities_unqueried": true,
                "reason": reason,
            }))
        })
        .take(MAX_ITEMS)
        .collect()
}

fn enek_target_reports(mut target_ids: Vec<u8>) -> Vec<Value> {
    target_ids.sort_unstable();
    target_ids.dedup();
    target_ids
        .into_iter()
        .take(MAX_ITEMS)
        .map(|target| {
            json!({
                "id": format!("{target:02x}"),
                "zones": null,
                "modes": null,
                "state_readable": null,
            })
        })
        .collect()
}

fn inspect_lighting(control: Option<&ControlCapabilities>) -> Vec<Value> {
    control.map_or_else(Vec::new, |capabilities| {
        capabilities
            .lighting
            .iter()
            .take(MAX_ITEMS)
            .map(|device| {
                json!({
                    "id": device.id,
                    "backend": lighting_backend(device.backend),
                    "target": lighting_target(device.target),
                    "zones": device.zones,
                    "modes": {
                        "static_color": device.modes.static_color,
                        "brightness": device.modes.brightness,
                        "breathing": device.modes.breathing,
                        "neon": device.modes.neon,
                    },
                    "state_readable": device.state_readable,
                })
            })
            .collect()
    })
}

fn lighting_backend(backend: LightingBackend) -> &'static str {
    match backend {
        LightingBackend::ZonedWmi => "zoned-wmi",
        LightingBackend::Enek5130 => "enek5130",
    }
}

fn lighting_target(target: LightingTarget) -> &'static str {
    match target {
        LightingTarget::Keyboard => "keyboard",
        LightingTarget::CoverLogo => "cover-logo",
        LightingTarget::RearLogo => "rear-logo",
        LightingTarget::Lightbar => "lightbar",
    }
}

fn parse_hid_uevent(value: &str) -> Option<(String, u16, u16, u16)> {
    let name = value
        .lines()
        .find_map(|line| line.strip_prefix("HID_NAME="))
        .unwrap_or("unknown")
        .trim()
        .to_string();
    let id = value
        .lines()
        .find_map(|line| line.strip_prefix("HID_ID="))?;
    let fields = id.trim().split(':').collect::<Vec<_>>();
    if fields.len() != 3 {
        return None;
    }
    Some((
        name,
        u16::from_str_radix(fields[0], 16).ok()?,
        u16::from_str_radix(fields[1], 16).ok()?,
        u16::from_str_radix(fields[2], 16).ok()?,
    ))
}

fn read_text(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_TEXT_BYTES).read_to_end(&mut bytes).ok()?;
    let value = String::from_utf8_lossy(&bytes).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn sorted_entries(path: &Path) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn rooted(root: &Path, relative: &str) -> PathBuf {
    root.join(relative.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlFanCapabilities;
    use crate::hardware::ProfileCapabilities;
    use crate::lighting::{LightingDevice, LightingModes};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("asense-probe-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, name: &str, value: &str) {
            let path = rooted(&self.0, name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn schema_two_reports_live_capabilities() {
        let tree = TempTree::new();
        for (path, value) in [
            ("sys/class/dmi/id/sys_vendor", "Acer\n"),
            ("sys/class/dmi/id/product_name", "Predator Probe\n"),
            ("sys/class/dmi/id/board_name", "Board\n"),
            ("sys/class/dmi/id/bios_version", "V1.00\n"),
            ("sys/firmware/acpi/platform_profile", "balanced\n"),
            (
                "sys/firmware/acpi/platform_profile_choices",
                "quiet balanced performance\n",
            ),
            (
                "sys/class/hidraw/hidraw0/device/uevent",
                "HID_NAME=ENEK5130\nHID_ID=0018:00000CF2:00005130\n",
            ),
            ("sys/class/hwmon/hwmon37/name", "acer_wmi\n"),
            ("sys/class/hwmon/hwmon37/fan1_input", "3100\n"),
            ("sys/class/hwmon/hwmon37/fan1_label", "CPU fan\n"),
            ("sys/class/hwmon/hwmon37/temp1_input", "65000\n"),
            ("sys/class/hwmon/hwmon37/temp1_label", "CPU package\n"),
            ("sys/class/hwmon/hwmon37/pwm1", "127\n"),
            ("sys/class/hwmon/hwmon37/pwm1_enable", "2\n"),
        ] {
            tree.write(path, value);
        }
        tree.write(
            &format!(
                "sys/bus/wmi/devices/{}-00/asense_rgb/power",
                GAMING_GUID.to_ascii_lowercase()
            ),
            "1\n",
        );
        let report: Value = serde_json::from_str(&generate_at(&tree.0).unwrap()).unwrap();
        assert_eq!(report["schema"], 2);
        assert_eq!(report["dmi"]["product"], "Predator Probe");
        assert_eq!(report["profiles"]["backend"], "kernel");
        assert_eq!(report["profiles"]["choices_source"], "kernel-live");
        assert_eq!(report["wmi"]["known_guids"][0], GAMING_GUID);
        assert_eq!(report["wmi"]["zoned_rgb"], true);
        assert_eq!(report["hwmon"]["name"], "acer_wmi");
        assert_eq!(report["hwmon"]["fan_inputs"][0]["rpm"], 3100);
        assert_eq!(report["hwmon"]["temps"][0]["millidegrees_c"], 65_000);
        assert_eq!(report["hwmon"]["pwm_nodes"][0], "pwm1");
        assert_eq!(report["hwmon"]["pwm_nodes"][1], "pwm1_enable");
        assert_eq!(report["hid"][0]["vid"], "0cf2");
        assert_eq!(report["hid"][0]["targets"], json!([]));
        assert_eq!(report["hid"][0]["targets_unqueried"], true);
        assert_eq!(report["hid"][0]["capabilities_unqueried"], true);
        assert_eq!(report["hid"][0]["reason"], ENEK_A1_UNQUERIED_REASON);
        assert!(report["hid"][0].get("target_ids").is_none());
    }

    #[test]
    fn passive_a1_targets_are_stable_without_invented_a3_capabilities() {
        assert_eq!(
            Value::Array(enek_target_reports(vec![0x83, 0x21, 0x21])),
            json!([
                { "id": "21", "zones": null, "modes": null, "state_readable": null },
                { "id": "83", "zones": null, "modes": null, "state_readable": null },
            ])
        );
    }

    #[test]
    fn gaming_wmi_profile_choices_are_labeled_as_known_commands() {
        let tree = TempTree::new();
        tree.write("sys/class/dmi/id/sys_vendor", "Acer\n");
        tree.write("sys/class/dmi/id/product_name", "Predator Probe\n");
        let profile = format!(
            "sys/bus/wmi/devices/{}-00/gaming_profile",
            GAMING_GUID.to_ascii_lowercase()
        );
        tree.write(&format!("{profile}/profile"), "balanced\n");
        tree.write(
            &format!("{profile}/choices"),
            "low-power quiet balanced performance\n",
        );

        let report: Value = serde_json::from_str(&generate_at(&tree.0).unwrap()).unwrap();
        assert_eq!(report["profiles"]["backend"], "gaming-wmi");
        assert_eq!(
            report["profiles"]["choices_source"],
            "known-gaming-wmi-commands"
        );
    }

    #[test]
    fn daemon_caps_add_typed_lighting_without_exposing_private_identity() {
        let tree = TempTree::new();
        for (path, value) in [
            ("sys/class/dmi/id/sys_vendor", "Acer\n"),
            ("sys/class/dmi/id/product_name", "Predator Probe\n"),
            (
                "sys/class/hidraw/hidraw0/device/uevent",
                "HID_NAME=ENEK5130\nHID_ID=0018:00000CF2:00005130\n",
            ),
        ] {
            tree.write(path, value);
        }
        let control = control_capabilities_with_lighting(vec![
            LightingDevice {
                id: "zoned-wmi-keyboard".to_string(),
                backend: LightingBackend::ZonedWmi,
                target: LightingTarget::Keyboard,
                zones: 4,
                modes: LightingModes::default(),
                state_readable: true,
            },
            LightingDevice {
                id: "enek5130-21".to_string(),
                backend: LightingBackend::Enek5130,
                target: LightingTarget::Keyboard,
                zones: 4,
                modes: LightingModes {
                    static_color: true,
                    brightness: true,
                    breathing: true,
                    neon: false,
                },
                state_readable: false,
            },
        ]);

        let report: Value =
            serde_json::from_str(&generate_at_with_capabilities(&tree.0, Some(&control)).unwrap())
                .unwrap();
        assert_eq!(
            report["lighting"],
            json!([{
                "id": "zoned-wmi-keyboard",
                "backend": "zoned-wmi",
                "target": "keyboard",
                "zones": 4,
                "modes": {
                    "static_color": false,
                    "brightness": false,
                    "breathing": false,
                    "neon": false,
                },
                "state_readable": true,
            }, {
                "id": "enek5130-21",
                "backend": "enek5130",
                "target": "keyboard",
                "zones": 4,
                "modes": {
                    "static_color": true,
                    "brightness": true,
                    "breathing": true,
                    "neon": false,
                },
                "state_readable": false,
            }])
        );
        assert_eq!(report["hid"][0]["targets"], json!([]));
        assert_eq!(report["hid"][0]["targets_unqueried"], true);
        assert_eq!(report["hid"][0]["capabilities_unqueried"], true);
        assert_eq!(report["hid"][0]["reason"], ENEK_A1_UNQUERIED_REASON);
        assert!(!report.to_string().contains("mode_mask"));
        assert!(!report.to_string().contains("SECRET-CONTROL"));
    }

    #[test]
    fn hwmon_probe_is_bounded_and_only_reports_exact_pwm_nodes() {
        let tree = TempTree::new();
        tree.write("sys/class/hwmon/hwmon0/name", "acer\n");
        tree.write("sys/class/hwmon/hwmon0/temp1_input", "99000\n");
        let foreign_device = rooted(&tree.0, "sys/class/hwmon/hwmon0/device");
        fs::create_dir_all(&foreign_device).unwrap();
        std::os::unix::fs::symlink(
            "/sys/bus/platform/drivers/foreign-hwmon",
            foreign_device.join("driver"),
        )
        .unwrap();
        tree.write("sys/class/hwmon/hwmon1/name", "acer-wmi\n");
        tree.write("sys/class/hwmon/hwmon1/temp3_input", "47000\n");
        tree.write("sys/class/hwmon/hwmon1/pwm2", "90\n");
        tree.write("sys/class/hwmon/hwmon1/pwm2_enable", "1\n");
        tree.write("sys/class/hwmon/hwmon1/pwm2_extra", "private\n");

        let report: Value = serde_json::from_str(&generate_at(&tree.0).unwrap()).unwrap();
        assert_eq!(report["hwmon"]["name"], "acer-wmi");
        assert_eq!(report["hwmon"]["temps"][0]["index"], 3);
        assert_eq!(report["hwmon"]["temps"][0]["label"], "Temperature 3");
        assert_eq!(report["hwmon"]["pwm_nodes"], json!(["pwm2", "pwm2_enable"]));
    }

    #[test]
    fn probe_omits_private_fields_and_never_writes() {
        let tree = TempTree::new();
        for (path, value) in [
            ("sys/class/dmi/id/sys_vendor", "Acer\n"),
            ("sys/class/dmi/id/product_name", "Model\n"),
            ("sys/class/dmi/id/product_serial", "SECRET-SERIAL\n"),
            ("sys/class/dmi/id/product_uuid", "SECRET-UUID\n"),
            ("etc/hostname", "SECRET-HOST\n"),
            ("proc/net/dev", "SECRET-NETWORK\n"),
            ("sys/firmware/acpi/tables/DSDT", "SECRET-ACPI\n"),
            (
                "sys/class/hidraw/hidraw0/device/uevent",
                "HID_NAME=ENEK5130\nHID_ID=0018:00000CF2:00005130\n",
            ),
            ("dev/hidraw0", "not a HID device\n"),
        ] {
            tree.write(path, value);
        }
        let before = snapshot(&tree.0);
        let report = generate_at(&tree.0).unwrap();
        assert_eq!(before, snapshot(&tree.0));
        for secret in [
            "SECRET-SERIAL",
            "SECRET-UUID",
            "SECRET-HOST",
            "SECRET-NETWORK",
            "SECRET-ACPI",
        ] {
            assert!(!report.contains(secret));
        }
    }

    fn control_capabilities_with_lighting(lighting: Vec<LightingDevice>) -> ControlCapabilities {
        ControlCapabilities {
            vendor: "SECRET-CONTROL-VENDOR".to_string(),
            product: "SECRET-CONTROL-PRODUCT".to_string(),
            reference_model: false,
            profiles: ProfileCapabilities {
                backend: None,
                choices: Vec::new(),
                current: None,
            },
            fans: ControlFanCapabilities {
                backend: None,
                rpm_channels: Vec::new(),
                auto: false,
                manual: false,
                maximum: false,
            },
            lighting,
            platform: Default::default(),
        }
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(path).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    visit(root, &path, out);
                } else {
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
