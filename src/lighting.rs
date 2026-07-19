use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::platform::find_wmi_group;

pub mod enek5130;

const RGB_WMI_GUID: &str = "7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LightingBackend {
    ZonedWmi,
    Enek5130,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LightingTarget {
    Keyboard,
    CoverLogo,
    RearLogo,
    Lightbar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LightingMode {
    Off,
    Static,
    Breathing,
    Neon,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct LightingModes {
    pub static_color: bool,
    pub brightness: bool,
    pub breathing: bool,
    pub neon: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct LightingDevice {
    pub id: String,
    pub backend: LightingBackend,
    pub target: LightingTarget,
    pub zones: u8,
    pub modes: LightingModes,
    pub state_readable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct LightingRequest {
    pub target: LightingTarget,
    pub mode: LightingMode,
    pub brightness: u8,
    pub speed: u8,
    pub color: [u8; 3],
    pub zone_colors: Vec<[u8; 3]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LightingStateStatus {
    Firmware(LightingState),
    Unknown,
    LastApplied(LightingRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightingState {
    pub powered: bool,
    pub mode: u8,
    pub speed: u8,
    pub brightness: u8,
    pub direction: u8,
    pub color: [u8; 3],
    pub zones: [[u8; 3]; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectRequest {
    pub mode: u8,
    pub speed: u8,
    pub brightness: u8,
    pub direction: u8,
    pub color: [u8; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZonesRequest {
    pub zones: [[u8; 3]; 4],
    pub brightness: u8,
}

#[derive(Debug)]
pub struct KeyboardLighting {
    power: PathBuf,
    effect: PathBuf,
    zones: PathBuf,
    zone_count: u8,
}

#[derive(Debug)]
pub enum LightingController {
    ZonedWmi(KeyboardLighting),
    Enek5130(enek5130::Enek5130),
}

impl LightingController {
    /// Discover each independent lighting transport without making one
    /// transport a prerequisite for another.
    pub fn discover_all() -> Vec<Self> {
        let mut controllers = Vec::with_capacity(2);
        if let Ok(controller) = KeyboardLighting::discover() {
            controllers.push(Self::ZonedWmi(controller));
        }
        if let Ok(controller) = enek5130::Enek5130::discover() {
            controllers.push(Self::Enek5130(controller));
        }
        controllers
    }

    pub fn backend(&self) -> LightingBackend {
        match self {
            Self::ZonedWmi(_) => LightingBackend::ZonedWmi,
            Self::Enek5130(_) => LightingBackend::Enek5130,
        }
    }

    pub fn devices(&self) -> Vec<LightingDevice> {
        match self {
            Self::ZonedWmi(controller) => vec![LightingDevice {
                id: "zoned-wmi-keyboard".to_string(),
                backend: LightingBackend::ZonedWmi,
                target: LightingTarget::Keyboard,
                zones: controller.zone_count(),
                modes: LightingModes {
                    static_color: true,
                    brightness: true,
                    breathing: true,
                    neon: true,
                },
                state_readable: true,
            }],
            Self::Enek5130(controller) => controller.devices().to_vec(),
        }
    }

    pub fn state(&self) -> Result<LightingStateStatus, String> {
        match self {
            Self::ZonedWmi(controller) => controller.read().map(LightingStateStatus::Firmware),
            Self::Enek5130(_) => Ok(LightingStateStatus::Unknown),
        }
    }

    pub fn apply(&self, request: &LightingRequest) -> Result<LightingStateStatus, String> {
        match self {
            Self::ZonedWmi(controller) => apply_zoned_wmi(controller, request),
            Self::Enek5130(controller) => controller.apply(request),
        }
    }

    pub fn set_power(
        &self,
        target: LightingTarget,
        enabled: bool,
    ) -> Result<LightingStateStatus, String> {
        match self {
            Self::ZonedWmi(controller) if target == LightingTarget::Keyboard => controller
                .set_power(enabled)
                .map(LightingStateStatus::Firmware),
            Self::ZonedWmi(_) => {
                Err("zoned WMI transport only exposes the keyboard target".to_string())
            }
            Self::Enek5130(_) => Err(
                "lighting power control is unsupported for the ENEK5130 HID backend".to_string(),
            ),
        }
    }
}

impl KeyboardLighting {
    pub fn discover() -> Result<Self, String> {
        Self::discover_at(Path::new("/sys/bus/wmi/devices"))
    }

    fn discover_at(wmi_root: &Path) -> Result<Self, String> {
        let base = find_wmi_group(wmi_root, RGB_WMI_GUID, "asense_rgb")
            .ok_or_else(|| "ASense RGB kernel transport is unavailable".to_string())?;
        let effect = base.join("effect");
        let zones = base.join("zones");
        let power = base.join("power");
        if !power.is_file() || !effect.is_file() || !zones.is_file() {
            return Err("ASense RGB kernel transport is unavailable".to_string());
        }
        let zone_count = match fs::read_to_string(base.join("zone_mask")) {
            Ok(mask) => zone_count_from_mask(mask.trim())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 4,
            Err(error) => return Err(format!("cannot read keyboard zone mask: {error}")),
        };
        Ok(Self {
            power,
            effect,
            zones,
            zone_count,
        })
    }

    pub const fn zone_count(&self) -> u8 {
        self.zone_count
    }

    pub fn set_power(&self, enabled: bool) -> Result<LightingState, String> {
        fs::write(&self.power, if enabled { "1\n" } else { "0\n" })
            .map_err(|error| format!("keyboard power rejected: {error}"))?;
        let actual = self.read()?;
        if actual.powered != enabled {
            return Err("keyboard power readback mismatch".to_string());
        }
        Ok(actual)
    }

    pub fn read(&self) -> Result<LightingState, String> {
        let effect = fs::read_to_string(&self.effect)
            .map_err(|error| format!("cannot read keyboard effect: {error}"))?;
        let zones = fs::read_to_string(&self.zones)
            .map_err(|error| format!("cannot read keyboard zones: {error}"))?;
        parse_state_with_zones(&effect, &zones, self.zone_count)
    }

    pub fn set_effect(&self, request: EffectRequest) -> Result<LightingState, String> {
        validate_effect(request)?;
        let payload = format!(
            "{},{},{},{},{},{},{}\n",
            request.mode,
            request.speed,
            request.brightness,
            request.direction,
            request.color[0],
            request.color[1],
            request.color[2]
        );
        fs::write(&self.effect, payload)
            .map_err(|error| format!("keyboard effect rejected: {error}"))?;
        let actual = self.read()?;
        if actual.mode != request.mode
            || actual.speed != request.speed
            || actual.brightness != request.brightness
            || actual.direction != request.direction
            || actual.color != request.color
        {
            return Err("keyboard effect readback mismatch".to_string());
        }
        Ok(actual)
    }

    pub fn set_zones(&self, request: ZonesRequest) -> Result<LightingState, String> {
        if request.brightness > 100 {
            return Err("keyboard brightness must be within 0..=100".to_string());
        }
        let mut payload = String::with_capacity(40);
        for (index, color) in request
            .zones
            .iter()
            .take(usize::from(self.zone_count))
            .enumerate()
        {
            if index != 0 {
                payload.push(',');
            }
            write!(
                &mut payload,
                "{:02x}{:02x}{:02x}",
                color[0], color[1], color[2]
            )
            .expect("writing to a String cannot fail");
        }
        writeln!(&mut payload, ",{}", request.brightness).expect("writing to a String cannot fail");
        fs::write(&self.zones, payload)
            .map_err(|error| format!("keyboard zones rejected: {error}"))?;
        let actual = self.read()?;
        let active = usize::from(self.zone_count);
        if actual.mode != 0
            || actual.brightness != request.brightness
            || actual.zones[..active] != request.zones[..active]
        {
            return Err("keyboard zone readback mismatch".to_string());
        }
        Ok(actual)
    }
}

fn apply_zoned_wmi(
    controller: &KeyboardLighting,
    request: &LightingRequest,
) -> Result<LightingStateStatus, String> {
    if request.target != LightingTarget::Keyboard {
        return Err("zoned WMI transport only exposes the keyboard target".to_string());
    }
    if request.brightness > 100 {
        return Err("keyboard brightness must be within 0..=100".to_string());
    }
    if request.speed > 9 {
        return Err("keyboard effect speed must be within 0..=9".to_string());
    }

    let state = match request.mode {
        LightingMode::Off => controller.set_power(false)?,
        LightingMode::Static => {
            let mut zones = [request.color; 4];
            match request.zone_colors.as_slice() {
                [] => {}
                [color] => zones.fill(*color),
                colors if colors.len() == usize::from(controller.zone_count()) => {
                    zones[..colors.len()].copy_from_slice(colors);
                }
                _ => {
                    return Err(format!(
                        "zoned WMI keyboard requires one or {} colors",
                        controller.zone_count()
                    ));
                }
            }
            controller.set_zones(ZonesRequest {
                zones,
                brightness: request.brightness,
            })?
        }
        LightingMode::Breathing | LightingMode::Neon => {
            if !request.zone_colors.is_empty() {
                return Err("firmware effects accept one global color".to_string());
            }
            controller.set_effect(EffectRequest {
                mode: if request.mode == LightingMode::Breathing {
                    1
                } else {
                    2
                },
                speed: request.speed,
                brightness: request.brightness,
                direction: 0,
                color: request.color,
            })?
        }
    };
    Ok(LightingStateStatus::Firmware(state))
}

pub fn validate_effect(request: EffectRequest) -> Result<(), String> {
    if request.mode > 7 {
        return Err("keyboard effect mode must be within 0..=7".to_string());
    }
    if request.speed > 9 {
        return Err("keyboard effect speed must be within 0..=9".to_string());
    }
    if request.brightness > 100 {
        return Err("keyboard brightness must be within 0..=100".to_string());
    }
    match request.mode {
        0 | 1 if request.speed != 0 || request.direction != 0 => {
            Err("static/breathing modes require speed=0 and direction=0".to_string())
        }
        2 if request.direction != 0 => Err("neon mode requires direction=0".to_string()),
        3 | 4 if !(1..=2).contains(&request.direction) => {
            Err("wave/shifting modes require direction 1 or 2".to_string())
        }
        5..=7 if request.direction != 0 => {
            Err("zoom/meteor/twinkling modes require direction=0".to_string())
        }
        _ => Ok(()),
    }
}

pub fn encode_state(state: LightingState) -> String {
    let zones = state
        .zones
        .iter()
        .map(|color| format!("{:02x}{:02x}{:02x}", color[0], color[1], color[2]))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "power={} mode={} speed={} brightness={} direction={} color={:02x}{:02x}{:02x} zones={zones}",
        if state.powered { "on" } else { "off" },
        state.mode,
        state.speed,
        state.brightness,
        state.direction,
        state.color[0],
        state.color[1],
        state.color[2]
    )
}

#[cfg(test)]
fn parse_state(effect: &str, zones: &str) -> Result<LightingState, String> {
    parse_state_with_zones(effect, zones, 4)
}

fn parse_state_with_zones(
    effect: &str,
    zones: &str,
    zone_count: u8,
) -> Result<LightingState, String> {
    let effect = parse_decimal_list::<7>(effect, "keyboard effect")?;
    let (zone_values, zone_brightness) = parse_zones(zones, zone_count)?;
    let state = LightingState {
        powered: effect[2] != 0,
        mode: as_u8(effect[0], "effect mode")?,
        speed: as_u8(effect[1], "effect speed")?,
        brightness: as_u8(effect[2], "brightness")?,
        direction: as_u8(effect[3], "direction")?,
        color: [
            as_u8(effect[4], "red")?,
            as_u8(effect[5], "green")?,
            as_u8(effect[6], "blue")?,
        ],
        zones: zone_values,
    };
    if state.brightness != zone_brightness {
        return Err("keyboard brightness getters disagree".to_string());
    }
    Ok(state)
}

fn parse_decimal_list<const N: usize>(input: &str, label: &str) -> Result<[u16; N], String> {
    let values = input.trim().split(',').collect::<Vec<_>>();
    if values.len() != N {
        return Err(format!("invalid {label} field count"));
    }
    let mut parsed = [0_u16; N];
    for (target, value) in parsed.iter_mut().zip(values) {
        *target = value
            .parse::<u16>()
            .map_err(|_| format!("invalid {label} value"))?;
    }
    Ok(parsed)
}

fn parse_zones(input: &str, zone_count: u8) -> Result<([[u8; 3]; 4], u8), String> {
    if !(1..=4).contains(&zone_count) {
        return Err("invalid keyboard zone count".to_string());
    }
    let fields = input.trim().split(',').collect::<Vec<_>>();
    if fields.len() != usize::from(zone_count) + 1 {
        return Err("invalid keyboard zone field count".to_string());
    }
    let mut zones = [[0_u8; 3]; 4];
    for (target, value) in zones.iter_mut().zip(&fields[..usize::from(zone_count)]) {
        if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid keyboard zone color".to_string());
        }
        target[0] =
            u8::from_str_radix(&value[0..2], 16).map_err(|_| "invalid red channel".to_string())?;
        target[1] = u8::from_str_radix(&value[2..4], 16)
            .map_err(|_| "invalid green channel".to_string())?;
        target[2] =
            u8::from_str_radix(&value[4..6], 16).map_err(|_| "invalid blue channel".to_string())?;
    }
    let brightness = fields[usize::from(zone_count)]
        .parse::<u8>()
        .map_err(|_| "invalid keyboard brightness".to_string())?;
    if brightness > 100 {
        return Err("keyboard brightness out of range".to_string());
    }
    Ok((zones, brightness))
}

fn zone_count_from_mask(value: &str) -> Result<u8, String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    let mask =
        u8::from_str_radix(value, 16).map_err(|_| "invalid keyboard zone mask".to_string())?;
    match mask {
        0x01 => Ok(1),
        0x03 => Ok(2),
        0x07 => Ok(3),
        0x0f => Ok(4),
        _ => Err("keyboard zone mask must be contiguous within 1..=4 zones".to_string()),
    }
}

fn as_u8(value: u16, label: &str) -> Result<u8, String> {
    u8::try_from(value).map_err(|_| format!("{label} out of range"))
}

#[cfg(test)]
mod tests {
    use super::{
        EffectRequest, KeyboardLighting, LightingController, LightingStateStatus, LightingTarget,
        parse_state, parse_state_with_zones, validate_effect, zone_count_from_mask,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_exact_kernel_readback() {
        let state = parse_state("3,5,80,2,0,0,0\n", "ff0000,00ff00,0000ff,ffffff,80\n").unwrap();
        assert_eq!(state.mode, 3);
        assert_eq!(state.zones[2], [0, 0, 255]);
    }

    #[test]
    fn rejects_disagreeing_brightness_and_bad_effect_contracts() {
        assert!(parse_state("0,0,80,0,0,0,0", "000000,000000,000000,000000,70").is_err());
        assert!(
            validate_effect(EffectRequest {
                mode: 3,
                speed: 2,
                brightness: 80,
                direction: 0,
                color: [0; 3],
            })
            .is_err()
        );
    }

    #[test]
    fn accepts_only_contiguous_one_to_four_zone_masks() {
        assert_eq!(zone_count_from_mask("0x01").unwrap(), 1);
        assert_eq!(zone_count_from_mask("03").unwrap(), 2);
        assert_eq!(zone_count_from_mask("0x07").unwrap(), 3);
        assert_eq!(zone_count_from_mask("0f").unwrap(), 4);
        for invalid in ["0", "2", "5", "f0", "100", "junk"] {
            assert!(zone_count_from_mask(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn parses_three_zone_readback_and_pads_the_fixed_legacy_state() {
        let state = parse_state_with_zones("0,0,60,0,0,0,0", "ff0000,00ff00,0000ff,60", 3).unwrap();
        assert_eq!(state.zones[0], [255, 0, 0]);
        assert_eq!(state.zones[2], [0, 0, 255]);
        assert_eq!(state.zones[3], [0, 0, 0]);
        assert!(parse_state_with_zones("0,0,60,0,0,0,0", "ff0000,60", 3).is_err());
    }

    #[test]
    fn discovers_a_suffixed_three_zone_wmi_device() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("asense-zoned-wmi-{}-{unique}", std::process::id()));
        let base = root
            .join(format!("{}-00", super::RGB_WMI_GUID.to_ascii_lowercase()))
            .join("asense_rgb");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("power"), "1\n").unwrap();
        fs::write(base.join("effect"), "0,0,80,0,0,0,0\n").unwrap();
        fs::write(base.join("zones"), "ff0000,00ff00,0000ff,80\n").unwrap();
        fs::write(base.join("zone_mask"), "0x07\n").unwrap();
        let lighting = KeyboardLighting::discover_at(&root).unwrap();
        assert_eq!(lighting.zone_count(), 3);
        assert_eq!(lighting.read().unwrap().zones[2], [0, 0, 255]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn controller_power_uses_only_the_zoned_wmi_power_attribute() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "asense-zoned-power-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let power = root.join("power");
        let effect = root.join("effect");
        let zones = root.join("zones");
        let effect_before = "2,4,80,0,1,2,3\n";
        let zones_before = "010203,040506,070809,0a0b0c,80\n";
        fs::write(&power, "0\n").unwrap();
        fs::write(&effect, effect_before).unwrap();
        fs::write(&zones, zones_before).unwrap();
        let controller = LightingController::ZonedWmi(KeyboardLighting {
            power: power.clone(),
            effect: effect.clone(),
            zones: zones.clone(),
            zone_count: 4,
        });

        let status = controller
            .set_power(LightingTarget::Keyboard, true)
            .unwrap();
        let LightingStateStatus::Firmware(state) = status else {
            panic!("zoned WMI power must return firmware readback");
        };
        assert!(state.powered);
        assert_eq!(state.mode, 2);
        assert_eq!(state.brightness, 80);
        assert_eq!(fs::read_to_string(&power).unwrap(), "1\n");
        assert_eq!(fs::read_to_string(&effect).unwrap(), effect_before);
        assert_eq!(fs::read_to_string(&zones).unwrap(), zones_before);

        assert!(
            controller
                .set_power(LightingTarget::CoverLogo, false)
                .is_err()
        );
        assert_eq!(fs::read_to_string(&power).unwrap(), "1\n");
        fs::remove_dir_all(root).unwrap();
    }
}
