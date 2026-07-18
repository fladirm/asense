use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const RGB_WMI_GUID: &str = "7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56";

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

pub struct KeyboardLighting {
    power: PathBuf,
    effect: PathBuf,
    zones: PathBuf,
}

impl KeyboardLighting {
    pub fn discover() -> Result<Self, String> {
        let base = Path::new("/sys/bus/wmi/devices")
            .join(RGB_WMI_GUID)
            .join("asense_rgb");
        let effect = base.join("effect");
        let zones = base.join("zones");
        let power = base.join("power");
        if !power.is_file() || !effect.is_file() || !zones.is_file() {
            return Err("ASense RGB kernel transport is unavailable".to_string());
        }
        Ok(Self {
            power,
            effect,
            zones,
        })
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
        parse_state(&effect, &zones)
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
        for (index, color) in request.zones.iter().enumerate() {
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
        if actual.mode != 0
            || actual.brightness != request.brightness
            || actual.zones != request.zones
        {
            return Err("keyboard zone readback mismatch".to_string());
        }
        Ok(actual)
    }
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

fn parse_state(effect: &str, zones: &str) -> Result<LightingState, String> {
    let effect = parse_decimal_list::<7>(effect, "keyboard effect")?;
    let (zone_values, zone_brightness) = parse_zones(zones)?;
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

fn parse_zones(input: &str) -> Result<([[u8; 3]; 4], u8), String> {
    let fields = input.trim().split(',').collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err("invalid keyboard zone field count".to_string());
    }
    let mut zones = [[0_u8; 3]; 4];
    for (target, value) in zones.iter_mut().zip(&fields[..4]) {
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
    let brightness = fields[4]
        .parse::<u8>()
        .map_err(|_| "invalid keyboard brightness".to_string())?;
    if brightness > 100 {
        return Err("keyboard brightness out of range".to_string());
    }
    Ok((zones, brightness))
}

fn as_u8(value: u16, label: &str) -> Result<u8, String> {
    u8::try_from(value).map_err(|_| format!("{label} out of range"))
}

#[cfg(test)]
mod tests {
    use super::{EffectRequest, parse_state, validate_effect};

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
}
