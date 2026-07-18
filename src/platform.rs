use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const WMI_GUID: &str = "7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56";
const ATTRIBUTE_COUNT: usize = 8;

pub const READ_ERROR_BATTERY_LIMIT: u8 = 1 << 0;
pub const READ_ERROR_BATTERY_CALIBRATION: u8 = 1 << 1;
pub const READ_ERROR_USB_CHARGING: u8 = 1 << 2;
pub const READ_ERROR_KEYBOARD_TIMEOUT: u8 = 1 << 3;
pub const READ_ERROR_BOOT_SOUND: u8 = 1 << 4;
pub const READ_ERROR_LCD_OVERRIDE: u8 = 1 << 5;
pub const READ_ERROR_REAR_LOGO: u8 = 1 << 6;
pub const READ_ERROR_MASK_ALL: u8 = READ_ERROR_BATTERY_LIMIT
    | READ_ERROR_BATTERY_CALIBRATION
    | READ_ERROR_USB_CHARGING
    | READ_ERROR_KEYBOARD_TIMEOUT
    | READ_ERROR_BOOT_SOUND
    | READ_ERROR_LCD_OVERRIDE
    | READ_ERROR_REAR_LOGO;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbCharging {
    Disabled,
    StopAt10Percent,
    StopAt20Percent,
    StopAt30Percent,
}

impl UsbCharging {
    pub const ALL: [Self; 4] = [
        Self::Disabled,
        Self::StopAt10Percent,
        Self::StopAt20Percent,
        Self::StopAt30Percent,
    ];

    pub const fn threshold(self) -> u8 {
        match self {
            Self::Disabled => 0,
            Self::StopAt10Percent => 10,
            Self::StopAt20Percent => 20,
            Self::StopAt30Percent => 30,
        }
    }

    pub fn from_threshold(value: u8) -> Result<Self, String> {
        match value {
            0 => Ok(Self::Disabled),
            10 => Ok(Self::StopAt10Percent),
            20 => Ok(Self::StopAt20Percent),
            30 => Ok(Self::StopAt30Percent),
            _ => Err("USB charging threshold must be 0, 10, 20, or 30".to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RearLogoState {
    pub enabled: bool,
    pub brightness: u8,
    pub color: [u8; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformState {
    pub battery_limit: Option<bool>,
    pub battery_calibration: Option<bool>,
    pub usb_charging: Option<UsbCharging>,
    pub keyboard_timeout: Option<bool>,
    pub boot_sound: Option<bool>,
    pub lcd_override: Option<bool>,
    pub rear_logo: Option<RearLogoState>,
    pub read_error_mask: u8,
}

pub struct PlatformControls {
    base: PathBuf,
}

impl PlatformControls {
    pub fn discover() -> Result<Self, String> {
        let base = Path::new("/sys/bus/wmi/devices")
            .join(WMI_GUID)
            .join("asense_rgb");
        if !base.is_dir() {
            return Err("ASense platform kernel transport is unavailable".to_string());
        }
        Ok(Self { base })
    }

    #[cfg(test)]
    fn at(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn read(&self) -> Result<PlatformState, String> {
        let mut read_error_mask = 0;
        Ok(PlatformState {
            battery_limit: isolate_getter_failure(
                self.read_optional_bool("battery_limit", "battery limit"),
                "battery_limit",
                READ_ERROR_BATTERY_LIMIT,
                &mut read_error_mask,
            ),
            battery_calibration: isolate_getter_failure(
                self.read_optional_bool("battery_calibration", "battery calibration"),
                "battery_calibration",
                READ_ERROR_BATTERY_CALIBRATION,
                &mut read_error_mask,
            ),
            usb_charging: isolate_getter_failure(
                self.read_optional_usb(),
                "usb_charging",
                READ_ERROR_USB_CHARGING,
                &mut read_error_mask,
            ),
            keyboard_timeout: isolate_getter_failure(
                self.read_optional_bool("keyboard_timeout", "keyboard timeout"),
                "keyboard_timeout",
                READ_ERROR_KEYBOARD_TIMEOUT,
                &mut read_error_mask,
            ),
            boot_sound: isolate_getter_failure(
                self.read_optional_bool("boot_sound", "boot sound"),
                "boot_sound",
                READ_ERROR_BOOT_SOUND,
                &mut read_error_mask,
            ),
            lcd_override: isolate_getter_failure(
                self.read_optional_bool("lcd_override", "LCD override"),
                "lcd_override",
                READ_ERROR_LCD_OVERRIDE,
                &mut read_error_mask,
            ),
            rear_logo: isolate_getter_failure(
                self.read_optional_logo(),
                "rear_logo",
                READ_ERROR_REAR_LOGO,
                &mut read_error_mask,
            ),
            read_error_mask,
        })
    }

    pub fn set_battery_limit(&self, enabled: bool) -> Result<PlatformState, String> {
        self.set_bool("battery_limit", "battery limit", enabled)?;
        self.read()
    }

    pub fn set_battery_calibration(&self, enabled: bool) -> Result<PlatformState, String> {
        self.set_bool("battery_calibration", "battery calibration", enabled)?;
        self.read()
    }

    pub fn set_usb_charging(&self, mode: UsbCharging) -> Result<PlatformState, String> {
        let path = self.require_attribute("usb_charging", "USB charging")?;
        fs::write(&path, format!("{}\n", mode.threshold()))
            .map_err(|error| format!("USB charging rejected: {error}"))?;
        if self.read_optional_usb()? != Some(mode) {
            return Err("USB charging readback mismatch".to_string());
        }
        self.read()
    }

    pub fn set_keyboard_timeout(&self, enabled: bool) -> Result<PlatformState, String> {
        self.set_bool("keyboard_timeout", "keyboard timeout", enabled)?;
        self.read()
    }

    pub fn set_boot_sound(&self, enabled: bool) -> Result<PlatformState, String> {
        self.set_bool("boot_sound", "boot sound", enabled)?;
        self.read()
    }

    pub fn set_lcd_override(&self, enabled: bool) -> Result<PlatformState, String> {
        self.set_bool("lcd_override", "LCD override", enabled)?;
        self.read()
    }

    pub fn set_rear_logo(&self, request: RearLogoState) -> Result<PlatformState, String> {
        if request.brightness > 100 {
            return Err("rear logo brightness must be within 0..=100".to_string());
        }
        let path = self.require_attribute("rear_logo", "rear logo")?;
        let payload = format!(
            "{:02x}{:02x}{:02x},{},{}\n",
            request.color[0],
            request.color[1],
            request.color[2],
            request.brightness,
            u8::from(request.enabled),
        );
        fs::write(&path, payload).map_err(|error| format!("rear logo rejected: {error}"))?;
        if self.read_optional_logo()? != Some(request) {
            return Err("rear logo readback mismatch".to_string());
        }
        self.read()
    }

    fn set_bool(&self, name: &str, label: &str, enabled: bool) -> Result<(), String> {
        let path = self.require_attribute(name, label)?;
        fs::write(&path, if enabled { "1\n" } else { "0\n" })
            .map_err(|error| format!("{label} rejected: {error}"))?;
        if self.read_optional_bool(name, label)? != Some(enabled) {
            return Err(format!("{label} readback mismatch"));
        }
        Ok(())
    }

    fn require_attribute(&self, name: &str, label: &str) -> Result<PathBuf, String> {
        let path = self.base.join(name);
        if path.is_file() {
            Ok(path)
        } else {
            Err(format!("{label} is not supported by this firmware"))
        }
    }

    fn read_optional_bool(&self, name: &str, label: &str) -> Result<Option<bool>, String> {
        let path = self.base.join(name);
        if !path.is_file() {
            return Ok(None);
        }
        let value =
            fs::read_to_string(path).map_err(|error| format!("cannot read {label}: {error}"))?;
        parse_bool(value.trim())
            .map(Some)
            .map_err(|_| format!("invalid {label} readback"))
    }

    fn read_optional_usb(&self) -> Result<Option<UsbCharging>, String> {
        let path = self.base.join("usb_charging");
        if !path.is_file() {
            return Ok(None);
        }
        let value = fs::read_to_string(path)
            .map_err(|error| format!("cannot read USB charging: {error}"))?;
        let threshold = value
            .trim()
            .parse::<u8>()
            .map_err(|_| "invalid USB charging readback".to_string())?;
        UsbCharging::from_threshold(threshold).map(Some)
    }

    fn read_optional_logo(&self) -> Result<Option<RearLogoState>, String> {
        let path = self.base.join("rear_logo");
        if !path.is_file() {
            return Ok(None);
        }
        let value =
            fs::read_to_string(path).map_err(|error| format!("cannot read rear logo: {error}"))?;
        parse_logo(value.trim()).map(Some)
    }
}

fn isolate_getter_failure<T>(
    result: Result<Option<T>, String>,
    name: &str,
    error_bit: u8,
    read_error_mask: &mut u8,
) -> Option<T> {
    match result {
        Ok(value) => value,
        Err(error) => {
            *read_error_mask |= error_bit;
            // PlatformControls is only used by the privileged helper. Keep
            // the detailed sysfs/firmware diagnostic in its journal and send
            // only the bounded capability mask over the control protocol.
            eprintln!("asensed: platform getter {name} failed: {error}");
            None
        }
    }
}

pub fn encode_state(state: PlatformState) -> String {
    let mut encoded = String::with_capacity(192);
    write!(
        &mut encoded,
        "battery_limit={} battery_calibration={} usb_charging={} keyboard_timeout={} boot_sound={} lcd_override={} rear_logo={} read_error_mask={}",
        encode_optional_bool(state.battery_limit),
        encode_optional_bool(state.battery_calibration),
        state
            .usb_charging
            .map(|value| value.threshold().to_string())
            .unwrap_or_else(|| "unsupported".to_string()),
        encode_optional_bool(state.keyboard_timeout),
        encode_optional_bool(state.boot_sound),
        encode_optional_bool(state.lcd_override),
        state.rear_logo.map_or_else(
            || "unsupported".to_string(),
            |logo| format!(
                "{:02x}{:02x}{:02x},{},{}",
                logo.color[0],
                logo.color[1],
                logo.color[2],
                logo.brightness,
                encode_bool(logo.enabled),
            ),
        ),
        state.read_error_mask,
    )
    .expect("writing to a String cannot fail");
    encoded
}

pub fn parse_state(value: &str) -> Result<PlatformState, String> {
    let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
    if fields.len() != ATTRIBUTE_COUNT {
        return Err("invalid platform state field count".to_string());
    }

    let mut battery_limit = None;
    let mut battery_calibration = None;
    let mut usb_charging = None;
    let mut keyboard_timeout = None;
    let mut boot_sound = None;
    let mut lcd_override = None;
    let mut rear_logo = None;
    let mut read_error_mask = None;

    for field in fields {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| "invalid platform state field".to_string())?;
        match key {
            "battery_limit" => set_once(&mut battery_limit, parse_optional_bool(value)?)?,
            "battery_calibration" => {
                set_once(&mut battery_calibration, parse_optional_bool(value)?)?
            }
            "usb_charging" => set_once(&mut usb_charging, parse_optional_usb(value)?)?,
            "keyboard_timeout" => set_once(&mut keyboard_timeout, parse_optional_bool(value)?)?,
            "boot_sound" => set_once(&mut boot_sound, parse_optional_bool(value)?)?,
            "lcd_override" => set_once(&mut lcd_override, parse_optional_bool(value)?)?,
            "rear_logo" => set_once(
                &mut rear_logo,
                if value == "unsupported" {
                    None
                } else {
                    Some(parse_logo(value)?)
                },
            )?,
            "read_error_mask" => set_once(&mut read_error_mask, parse_read_error_mask(value)?)?,
            _ => return Err("unknown platform state field".to_string()),
        }
    }

    Ok(PlatformState {
        battery_limit: battery_limit.ok_or_else(missing_field)?,
        battery_calibration: battery_calibration.ok_or_else(missing_field)?,
        usb_charging: usb_charging.ok_or_else(missing_field)?,
        keyboard_timeout: keyboard_timeout.ok_or_else(missing_field)?,
        boot_sound: boot_sound.ok_or_else(missing_field)?,
        lcd_override: lcd_override.ok_or_else(missing_field)?,
        rear_logo: rear_logo.ok_or_else(missing_field)?,
        read_error_mask: read_error_mask.ok_or_else(missing_field)?,
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), String> {
    if slot.replace(value).is_some() {
        Err("duplicate platform state field".to_string())
    } else {
        Ok(())
    }
}

fn missing_field() -> String {
    "missing platform state field".to_string()
}

fn parse_optional_bool(value: &str) -> Result<Option<bool>, String> {
    if value == "unsupported" {
        Ok(None)
    } else {
        parse_bool(value).map(Some)
    }
}

fn parse_optional_usb(value: &str) -> Result<Option<UsbCharging>, String> {
    if value == "unsupported" {
        return Ok(None);
    }
    let threshold = value
        .parse::<u8>()
        .map_err(|_| "invalid USB charging state".to_string())?;
    UsbCharging::from_threshold(threshold).map(Some)
}

fn parse_read_error_mask(value: &str) -> Result<u8, String> {
    let mask = value
        .parse::<u8>()
        .map_err(|_| "invalid platform read error mask".to_string())?;
    if mask & !READ_ERROR_MASK_ALL == 0 {
        Ok(mask)
    } else {
        Err("unknown platform read error mask bit".to_string())
    }
}

fn parse_logo(value: &str) -> Result<RearLogoState, String> {
    let fields = value.split(',').collect::<Vec<_>>();
    if fields.len() != 3 {
        return Err("invalid rear logo field count".to_string());
    }
    let color = fields[0];
    if color.len() != 6 || !color.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("rear logo color must be exactly RRGGBB".to_string());
    }
    let brightness = fields[1]
        .parse::<u8>()
        .map_err(|_| "invalid rear logo brightness".to_string())?;
    if brightness > 100 {
        return Err("rear logo brightness must be within 0..=100".to_string());
    }
    Ok(RearLogoState {
        enabled: parse_bool(fields[2])?,
        brightness,
        color: [
            u8::from_str_radix(&color[0..2], 16)
                .map_err(|_| "invalid rear logo red channel".to_string())?,
            u8::from_str_radix(&color[2..4], 16)
                .map_err(|_| "invalid rear logo green channel".to_string())?,
            u8::from_str_radix(&color[4..6], 16)
                .map_err(|_| "invalid rear logo blue channel".to_string())?,
        ],
    })
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value {
        "0" | "off" => Ok(false),
        "1" | "on" => Ok(true),
        _ => Err("invalid boolean state".to_string()),
    }
}

const fn encode_bool(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn encode_optional_bool(value: Option<bool>) -> &'static str {
    value.map_or("unsupported", encode_bool)
}

#[cfg(test)]
mod tests {
    use super::{
        PlatformControls, PlatformState, READ_ERROR_BATTERY_LIMIT, READ_ERROR_MASK_ALL,
        READ_ERROR_USB_CHARGING, RearLogoState, UsbCharging, encode_state, parse_state,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn full_state() -> PlatformState {
        PlatformState {
            battery_limit: Some(true),
            battery_calibration: Some(false),
            usb_charging: Some(UsbCharging::StopAt20Percent),
            keyboard_timeout: Some(true),
            boot_sound: Some(false),
            lcd_override: Some(true),
            rear_logo: Some(RearLogoState {
                enabled: true,
                brightness: 75,
                color: [0x12, 0xab, 0xef],
            }),
            read_error_mask: 0,
        }
    }

    #[test]
    fn structured_state_round_trips_and_rejects_unknown_values() {
        let encoded = encode_state(full_state());
        assert_eq!(parse_state(&encoded).unwrap(), full_state());
        assert!(parse_state(&encoded.replace("usb_charging=20", "usb_charging=25")).is_err());
        assert!(parse_state(&encoded.replace("rear_logo=12abef", "rear_logo=#2abef")).is_err());
        assert!(parse_state(&encoded.replace("read_error_mask=0", "read_error_mask=128")).is_err());
        assert!(parse_state(&encoded.replace("read_error_mask=0", "read_error_mask=256")).is_err());
        assert!(parse_state(&format!("{encoded} extra=no")).is_err());
    }

    #[test]
    fn unsupported_capabilities_are_explicit() {
        let mut state = full_state();
        state.battery_limit = None;
        state.rear_logo = None;
        state.read_error_mask = READ_ERROR_BATTERY_LIMIT;
        assert_eq!(parse_state(&encode_state(state)).unwrap(), state);
        assert_eq!(READ_ERROR_MASK_ALL, 0x7f);
    }

    #[test]
    fn getter_failures_are_isolated_from_other_reads_and_mutations() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("asense-platform-{}-{unique}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("battery_limit"), "1\n").unwrap();
        fs::write(root.join("usb_charging"), "30\n").unwrap();
        fs::write(root.join("rear_logo"), "a0b1c2,80,0\n").unwrap();
        let controls = PlatformControls::at(root.clone());
        let state = controls.read().unwrap();
        assert_eq!(state.battery_limit, Some(true));
        assert_eq!(state.usb_charging, Some(UsbCharging::StopAt30Percent));
        assert_eq!(state.rear_logo.unwrap().color, [0xa0, 0xb1, 0xc2]);
        assert_eq!(state.read_error_mask, 0);

        fs::write(root.join("usb_charging"), "31\n").unwrap();
        let state = controls.read().unwrap();
        assert_eq!(state.battery_limit, Some(true));
        assert_eq!(state.usb_charging, None);
        assert_eq!(state.rear_logo.unwrap().color, [0xa0, 0xb1, 0xc2]);
        assert_eq!(state.read_error_mask, READ_ERROR_USB_CHARGING);

        let state = controls.set_battery_limit(false).unwrap();
        assert_eq!(state.battery_limit, Some(false));
        assert_eq!(state.usb_charging, None);
        assert_eq!(state.rear_logo.unwrap().color, [0xa0, 0xb1, 0xc2]);
        assert_eq!(state.read_error_mask, READ_ERROR_USB_CHARGING);
        fs::remove_dir_all(root).unwrap();
    }
}
