use super::{
    LightingBackend, LightingDevice, LightingMode, LightingModes, LightingRequest,
    LightingStateStatus, LightingTarget,
};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

const SYSFS_HIDRAW: &str = "/sys/class/hidraw";
const DEV_ROOT: &str = "/dev";

const HID_BUS_I2C: u32 = 0x0018;
const HID_VENDOR: u32 = 0x0cf2;
const HID_PRODUCT: u32 = 0x5130;

const REPORT_TARGET_LIST: u8 = 0xa1;
const REPORT_TARGET_SELECT: u8 = 0xa2;
const REPORT_TARGET_CAPABILITIES: u8 = 0xa3;
const REPORT_LIGHTING: u8 = 0xa4;

const TARGET_KEYBOARD: u8 = 0x21;
const TARGET_COVER_LOGO: u8 = 0x83;

const MODE_STATIC: u8 = 0x02;
const MODE_BREATHING: u8 = 0x04;
const MODE_NEON: u8 = 0x05;

const TARGET_LIST_REPORT_LEN: usize = 11;
const TARGET_CAPABILITIES_REPORT_LEN: usize = 9;
const TARGET_CAPABILITIES_MIN_LEN: usize = 6;
const LIGHTING_REPORT_LEN: usize = 11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetCapabilities {
    pub target_id: u8,
    pub zone_count: u8,
    pub mode_mask: u32,
}

impl TargetCapabilities {
    fn target(self) -> Option<LightingTarget> {
        match self.target_id {
            TARGET_KEYBOARD => Some(LightingTarget::Keyboard),
            TARGET_COVER_LOGO => Some(LightingTarget::CoverLogo),
            _ => None,
        }
    }

    fn supports_wire_mode(self, mode: u8) -> bool {
        (1..=32).contains(&mode) && self.mode_mask & (1_u32 << (mode - 1)) != 0
    }

    fn supports(self, mode: LightingMode) -> bool {
        let wire_mode = match mode {
            LightingMode::Off | LightingMode::Static => MODE_STATIC,
            LightingMode::Breathing => MODE_BREATHING,
            LightingMode::Neon => MODE_NEON,
        };
        self.supports_wire_mode(wire_mode)
    }

    fn all_zones(self) -> u16 {
        match self.zone_count {
            0 => 0,
            1..=15 => (1_u16 << self.zone_count) - 1,
            _ => u16::MAX,
        }
    }

    fn modes(self) -> LightingModes {
        LightingModes {
            static_color: self.supports(LightingMode::Static),
            brightness: true,
            breathing: self.supports(LightingMode::Breathing),
            neon: self.supports(LightingMode::Neon),
        }
    }
}

#[derive(Debug)]
pub struct Enek5130 {
    node: PathBuf,
    capabilities: Vec<TargetCapabilities>,
    devices: Vec<LightingDevice>,
}

impl Enek5130 {
    pub fn discover() -> Result<Self, String> {
        Self::discover_at(Path::new(SYSFS_HIDRAW), Path::new(DEV_ROOT))
    }

    fn discover_at(sysfs_root: &Path, dev_root: &Path) -> Result<Self, String> {
        let candidates = controller_candidates(sysfs_root, dev_root);
        if candidates.is_empty() {
            return Err("ENEK5130 lighting controller is unavailable".to_string());
        }

        // One physical ENEK device may expose more than one hidraw function.
        // Identity alone is therefore not sufficient: walk every exact
        // I2C/VID/PID match and keep only the function that answers the bounded
        // A1/A2/A3 lighting contract.
        let mut last_error = None;
        for node in candidates {
            match discover_candidate(&node) {
                Ok((capabilities, devices)) => {
                    return Ok(Self {
                        node,
                        capabilities,
                        devices,
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error
            .unwrap_or_else(|| "ENEK5130 exposes no supported lighting target".to_string()))
    }

    pub fn node(&self) -> &Path {
        &self.node
    }

    pub fn devices(&self) -> &[LightingDevice] {
        &self.devices
    }

    pub fn target_capabilities(&self) -> &[TargetCapabilities] {
        &self.capabilities
    }

    pub fn apply(&self, request: &LightingRequest) -> Result<LightingStateStatus, String> {
        validate_request(request)?;
        let target_id = target_id(request.target)
            .ok_or_else(|| "ENEK5130 does not support this lighting target".to_string())?;

        // Targets and their capabilities can change across resume. Query the
        // controller immediately before every lighting write instead of
        // trusting the discovery-time snapshot.
        let file = open_controller(&self.node)?;
        let targets = read_target_list(&file)?;
        if !targets.contains(&target_id) {
            return Err("ENEK5130 lighting target is no longer present".to_string());
        }
        let caps = read_target_capabilities(&file, target_id)?;
        let reports = lighting_reports(caps, request)?;
        for mut report in reports {
            set_feature(&file, &mut report)?;
        }

        Ok(LightingStateStatus::LastApplied(canonical_request(request)))
    }
}

/// Read the passive A1 target list without selecting a target or changing lighting.
pub fn read_only_target_ids(node: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(node).map_err(|error| {
        format!(
            "cannot open ENEK5130 at {} read-only: {error}",
            node.display()
        )
    })?;
    read_target_list(&file)
}

fn device_from_caps(caps: TargetCapabilities) -> LightingDevice {
    LightingDevice {
        id: format!("enek5130-{:02x}", caps.target_id),
        backend: LightingBackend::Enek5130,
        target: caps.target().expect("known ENEK target was validated"),
        zones: caps.zone_count,
        modes: caps.modes(),
        state_readable: false,
    }
}

fn open_controller(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("cannot open ENEK5130 at {}: {error}", path.display()))
}

fn discover_candidate(
    node: &Path,
) -> Result<(Vec<TargetCapabilities>, Vec<LightingDevice>), String> {
    let file = open_controller(node)?;
    let targets = read_target_list(&file)?;
    let mut capabilities = Vec::with_capacity(2);

    for target_id in [TARGET_KEYBOARD, TARGET_COVER_LOGO] {
        if !targets.contains(&target_id) {
            continue;
        }
        let Ok(caps) = read_target_capabilities(&file, target_id) else {
            continue;
        };
        if caps.target().is_some()
            && [
                LightingMode::Static,
                LightingMode::Breathing,
                LightingMode::Neon,
            ]
            .into_iter()
            .any(|mode| caps.supports(mode))
        {
            capabilities.push(caps);
        }
    }

    if capabilities.is_empty() {
        return Err(format!(
            "ENEK5130 interface {} exposes no supported lighting target",
            node.display()
        ));
    }

    let devices = capabilities.iter().copied().map(device_from_caps).collect();
    Ok((capabilities, devices))
}

fn controller_candidates(sysfs_root: &Path, dev_root: &Path) -> Vec<PathBuf> {
    let mut matches = Vec::new();
    let Ok(entries) = fs::read_dir(sysfs_root) else {
        return matches;
    };
    for entry in entries.flatten() {
        let Ok(uevent) = fs::read_to_string(entry.path().join("device/uevent")) else {
            continue;
        };
        if matches_uevent(&uevent) {
            matches.push(dev_root.join(entry.file_name()));
        }
    }
    matches.sort();
    matches
}

fn matches_uevent(uevent: &str) -> bool {
    let mut id_match = false;
    let mut name_match = false;
    for line in uevent.lines() {
        if let Some(name) = line.strip_prefix("HID_NAME=") {
            name_match |= name.trim() == "ENEK5130";
        }
        if let Some(id) = line.strip_prefix("HID_ID=") {
            let mut fields = id.trim().split(':');
            let (Some(bus), Some(vendor), Some(product), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            id_match |= u32::from_str_radix(bus, 16).ok() == Some(HID_BUS_I2C)
                && u32::from_str_radix(vendor, 16).ok() == Some(HID_VENDOR)
                && u32::from_str_radix(product, 16).ok() == Some(HID_PRODUCT);
        }
    }
    id_match || name_match
}

fn target_id(target: LightingTarget) -> Option<u8> {
    match target {
        LightingTarget::Keyboard => Some(TARGET_KEYBOARD),
        LightingTarget::CoverLogo => Some(TARGET_COVER_LOGO),
        LightingTarget::RearLogo | LightingTarget::Lightbar => None,
    }
}

fn read_target_list(file: &File) -> Result<Vec<u8>, String> {
    let report = get_feature::<TARGET_LIST_REPORT_LEN>(file, REPORT_TARGET_LIST)?;
    parse_target_list(&report)
}

fn parse_target_list(report: &[u8]) -> Result<Vec<u8>, String> {
    if report.len() < 2 || report[0] != REPORT_TARGET_LIST {
        return Err("invalid ENEK5130 target-list report".to_string());
    }
    let count = usize::from(report[1]);
    let end = 2_usize
        .checked_add(count)
        .filter(|end| *end <= report.len())
        .ok_or_else(|| "invalid ENEK5130 target count".to_string())?;
    Ok(report[2..end].to_vec())
}

fn read_target_capabilities(file: &File, target_id: u8) -> Result<TargetCapabilities, String> {
    let mut select = [REPORT_TARGET_SELECT, target_id];
    set_feature(file, &mut select)?;
    let report = get_feature::<TARGET_CAPABILITIES_REPORT_LEN>(file, REPORT_TARGET_CAPABILITIES)?;
    parse_target_capabilities(target_id, &report)
}

fn parse_target_capabilities(
    expected_target: u8,
    report: &[u8],
) -> Result<TargetCapabilities, String> {
    if report.len() < TARGET_CAPABILITIES_MIN_LEN
        || report[0] != REPORT_TARGET_CAPABILITIES
        || report[1] != expected_target
    {
        return Err("invalid ENEK5130 target-capabilities report".to_string());
    }
    let zone_count = report[3];
    if !(1..=16).contains(&zone_count) {
        return Err("ENEK5130 zone count must be within 1..=16".to_string());
    }

    let mut mode_bytes = [0_u8; 4];
    let available = (report.len() - 5).min(mode_bytes.len());
    mode_bytes[..available].copy_from_slice(&report[5..5 + available]);
    let mode_mask = u32::from_le_bytes(mode_bytes);
    let supported_mask =
        (1_u32 << (MODE_STATIC - 1)) | (1_u32 << (MODE_BREATHING - 1)) | (1_u32 << (MODE_NEON - 1));
    if mode_mask & supported_mask == 0 {
        return Err("ENEK5130 target has no supported lighting mode".to_string());
    }

    Ok(TargetCapabilities {
        target_id: report[1],
        zone_count,
        mode_mask,
    })
}

fn validate_request(request: &LightingRequest) -> Result<(), String> {
    if request.brightness > 100 {
        return Err("ENEK5130 brightness must be within 0..=100".to_string());
    }
    if request.speed > 9 {
        return Err("ENEK5130 effect speed must be within 0..=9".to_string());
    }
    if request.zone_colors.len() > 16 {
        return Err("ENEK5130 accepts at most 16 zone colors".to_string());
    }
    Ok(())
}

fn lighting_reports(
    caps: TargetCapabilities,
    request: &LightingRequest,
) -> Result<Vec<[u8; LIGHTING_REPORT_LEN]>, String> {
    if caps.target() != Some(request.target) {
        return Err("ENEK5130 target-capabilities mismatch".to_string());
    }
    if !caps.supports(request.mode) {
        return Err("requested ENEK5130 lighting mode is unavailable".to_string());
    }

    match request.mode {
        LightingMode::Off => Ok(vec![encode_lighting_report(
            caps,
            MODE_STATIC,
            0,
            0,
            [0; 3],
            caps.all_zones(),
        )]),
        LightingMode::Static => static_reports(caps, request),
        LightingMode::Breathing | LightingMode::Neon => {
            let mode = if request.mode == LightingMode::Breathing {
                MODE_BREATHING
            } else {
                MODE_NEON
            };
            Ok(vec![encode_lighting_report(
                caps,
                mode,
                request.brightness,
                request.speed,
                request.color,
                caps.all_zones(),
            )])
        }
    }
}

fn static_reports(
    caps: TargetCapabilities,
    request: &LightingRequest,
) -> Result<Vec<[u8; LIGHTING_REPORT_LEN]>, String> {
    match request.zone_colors.as_slice() {
        [] => Ok(vec![encode_lighting_report(
            caps,
            MODE_STATIC,
            request.brightness,
            0,
            request.color,
            caps.all_zones(),
        )]),
        [color] => Ok(vec![encode_lighting_report(
            caps,
            MODE_STATIC,
            request.brightness,
            0,
            *color,
            caps.all_zones(),
        )]),
        colors if colors.len() == usize::from(caps.zone_count) => Ok(colors
            .iter()
            .enumerate()
            .map(|(index, color)| {
                encode_lighting_report(
                    caps,
                    MODE_STATIC,
                    request.brightness,
                    0,
                    *color,
                    1_u16 << index,
                )
            })
            .collect()),
        _ => Err(format!(
            "ENEK5130 target requires one or {} zone colors",
            caps.zone_count
        )),
    }
}

fn encode_lighting_report(
    caps: TargetCapabilities,
    mode: u8,
    brightness: u8,
    speed: u8,
    color: [u8; 3],
    zones: u16,
) -> [u8; LIGHTING_REPORT_LEN] {
    let flag = if mode == MODE_STATIC {
        if caps.target_id == TARGET_COVER_LOGO {
            1
        } else {
            0
        }
    } else {
        2
    };
    let [zones_low, zones_high] = zones.to_le_bytes();
    [
        REPORT_LIGHTING,
        caps.target_id,
        mode,
        brightness,
        speed,
        flag,
        color[0],
        color[1],
        color[2],
        zones_low,
        zones_high,
    ]
}

fn canonical_request(request: &LightingRequest) -> LightingRequest {
    let mut canonical = request.clone();
    match canonical.mode {
        LightingMode::Off => {
            canonical.brightness = 0;
            canonical.speed = 0;
            canonical.color = [0; 3];
            canonical.zone_colors.clear();
        }
        LightingMode::Static => canonical.speed = 0,
        LightingMode::Breathing | LightingMode::Neon => canonical.zone_colors.clear(),
    }
    canonical
}

fn set_feature(file: &File, report: &mut [u8]) -> Result<(), String> {
    let request = feature_ioctl(0x06, report.len())?;
    // SAFETY: `report` is live and writable for exactly the encoded length,
    // and `file` is an open hidraw descriptor owned by this process.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request, report.as_mut_ptr()) };
    if result < 0 {
        return Err(format!(
            "ENEK5130 HIDIOCSFEATURE failed: {}",
            io::Error::last_os_error()
        ));
    }
    if result as usize != report.len() {
        return Err(format!(
            "ENEK5130 feature write returned {result} bytes, expected {}",
            report.len()
        ));
    }
    Ok(())
}

fn get_feature<const N: usize>(file: &File, report_id: u8) -> Result<Vec<u8>, String> {
    let mut report = [0_u8; N];
    report[0] = report_id;
    let request = feature_ioctl(0x07, report.len())?;
    // SAFETY: `report` is live and writable for exactly the encoded length,
    // and `file` is an open hidraw descriptor owned by this process.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request, report.as_mut_ptr()) };
    if result < 0 {
        return Err(format!(
            "ENEK5130 HIDIOCGFEATURE failed: {}",
            io::Error::last_os_error()
        ));
    }
    let received = result as usize;
    if received == 0 || received > report.len() {
        return Err(format!(
            "ENEK5130 feature read returned invalid length {received}"
        ));
    }
    Ok(report[..received].to_vec())
}

fn feature_ioctl(operation: u8, len: usize) -> Result<libc::c_ulong, String> {
    const IOC_WRITE: u64 = 1;
    const IOC_READ: u64 = 2;
    const IOC_DIR_SHIFT: u32 = 30;
    const IOC_SIZE_SHIFT: u32 = 16;
    const IOC_TYPE_SHIFT: u32 = 8;
    const IOC_MAX_SIZE: usize = (1 << 14) - 1;

    if len == 0 || len > IOC_MAX_SIZE {
        return Err("invalid hidraw feature-report length".to_string());
    }
    let request = ((IOC_READ | IOC_WRITE) << IOC_DIR_SHIFT)
        | ((len as u64) << IOC_SIZE_SHIFT)
        | ((u64::from(b'H')) << IOC_TYPE_SHIFT)
        | u64::from(operation);
    libc::c_ulong::try_from(request).map_err(|_| "hidraw ioctl request overflow".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(target_id: u8, zones: u8, mode_mask: u8) -> TargetCapabilities {
        TargetCapabilities {
            target_id,
            zone_count: zones,
            mode_mask: u32::from(mode_mask),
        }
    }

    fn request(mode: LightingMode) -> LightingRequest {
        LightingRequest {
            target: LightingTarget::Keyboard,
            mode,
            brightness: 80,
            speed: 5,
            color: [12, 34, 56],
            zone_colors: Vec::new(),
        }
    }

    #[test]
    fn matches_exact_i2c_hid_identity() {
        assert!(matches_uevent("HID_NAME=ENEK5130\n"));
        assert!(matches_uevent("HID_ID=0018:00000CF2:00005130\n"));
        assert!(!matches_uevent("HID_ID=0003:00000CF2:00005130\n"));
        assert!(!matches_uevent(
            "HID_NAME=ENEK5130-clone\nHID_ID=0018:00000CF2:00005131\n"
        ));
    }

    #[test]
    fn parses_bounded_target_list() {
        assert_eq!(
            parse_target_list(&[0xa1, 3, 0x65, 0x21, 0x83]).unwrap(),
            vec![0x65, 0x21, 0x83]
        );
        assert!(parse_target_list(&[0xa1, 4, 0x21]).is_err());
        assert!(parse_target_list(&[0xa2, 0]).is_err());
    }

    #[test]
    fn parses_caps_and_rejects_wrong_target_or_zone_count() {
        let parsed = parse_target_capabilities(0x83, &[0xa3, 0x83, 1, 5, 1, 0x3b]).unwrap();
        assert_eq!(parsed.zone_count, 5);
        assert!(parsed.supports(LightingMode::Static));
        assert!(parsed.supports(LightingMode::Breathing));
        assert!(parsed.supports(LightingMode::Neon));
        assert!(parse_target_capabilities(0x21, &[0xa3, 0x83, 1, 5, 1, 0x3b]).is_err());
        assert!(parse_target_capabilities(0x83, &[0xa3, 0x83, 1, 0, 1, 0x3b]).is_err());
        assert!(parse_target_capabilities(0x83, &[0xa3, 0x83, 1, 17, 1, 0x3b]).is_err());
    }

    #[test]
    fn mode_mask_uses_all_available_little_endian_bytes() {
        let parsed =
            parse_target_capabilities(0x21, &[0xa3, 0x21, 1, 4, 1, 0x02, 0x01, 0, 0]).unwrap();
        assert_eq!(parsed.mode_mask, 0x0000_0102);
    }

    #[test]
    fn encodes_exact_a2_and_a4_contracts() {
        let select = [REPORT_TARGET_SELECT, TARGET_KEYBOARD];
        assert_eq!(select, [0xa2, 0x21]);

        let report = encode_lighting_report(
            caps(TARGET_KEYBOARD, 4, 0x1a),
            MODE_STATIC,
            80,
            0,
            [12, 34, 56],
            0x000f,
        );
        assert_eq!(report, [0xa4, 0x21, 0x02, 80, 0, 0, 12, 34, 56, 15, 0]);
    }

    #[test]
    fn keeps_complete_sixteen_bit_zone_mask() {
        let report = encode_lighting_report(
            caps(TARGET_COVER_LOGO, 16, 0x02),
            MODE_STATIC,
            100,
            0,
            [1, 2, 3],
            u16::MAX,
        );
        assert_eq!(&report[9..], &[0xff, 0xff]);
        assert_eq!(report[5], 1);
    }

    #[test]
    fn static_per_zone_builds_one_bounded_report_per_zone() {
        let mut request = request(LightingMode::Static);
        request.zone_colors = vec![[1, 0, 0], [0, 2, 0], [0, 0, 3], [4, 4, 4]];
        let reports = lighting_reports(caps(TARGET_KEYBOARD, 4, 0x02), &request).unwrap();
        assert_eq!(reports.len(), 4);
        assert_eq!(reports[0][9], 1);
        assert_eq!(reports[3][9], 8);
        assert_eq!(&reports[2][6..9], &[0, 0, 3]);
    }

    #[test]
    fn rejects_unsupported_mode_and_invalid_ranges() {
        assert!(
            lighting_reports(caps(TARGET_KEYBOARD, 4, 0x02), &request(LightingMode::Neon)).is_err()
        );
        let mut invalid = request(LightingMode::Static);
        invalid.brightness = 101;
        assert!(validate_request(&invalid).is_err());
        invalid.brightness = 80;
        invalid.speed = 10;
        assert!(validate_request(&invalid).is_err());
    }

    #[test]
    fn canonical_last_applied_does_not_claim_stale_effect_fields() {
        let mut off = request(LightingMode::Off);
        off.zone_colors = vec![[1, 2, 3]];
        let canonical = canonical_request(&off);
        assert_eq!(canonical.brightness, 0);
        assert_eq!(canonical.speed, 0);
        assert_eq!(canonical.color, [0; 3]);
        assert!(canonical.zone_colors.is_empty());
    }

    #[test]
    fn hidraw_ioctl_contains_feature_length() {
        assert_eq!(feature_ioctl(0x06, 2).unwrap(), 0xc002_4806);
        assert_eq!(feature_ioctl(0x07, 11).unwrap(), 0xc00b_4807);
    }
}
