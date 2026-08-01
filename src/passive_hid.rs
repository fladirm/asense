//! Bounded passive HID evidence for probe schema 3.
//!
//! This module is intentionally independent from lighting discovery.  It
//! accepts only two exact HID-over-I2C identities, hashes and decodes their
//! report descriptors, and may issue one descriptor-sized GET_FEATURE for the
//! ENEK A1 target list.  It contains no selector, feature write, caller-chosen
//! device path or semantic target mapping.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::passive_diagnostics::{
    DiagnosticAbsence, DiagnosticError, DiagnosticErrorClass, DiagnosticErrorStage,
    DiagnosticObservation, validate_error,
};

const HID_BUS_I2C: u32 = 0x0018;
const ENEK_VENDOR: u32 = 0x0cf2;
const ENEK_PRODUCT: u32 = 0x5130;
const ACER_EC_VENDOR: u32 = 0x1025;
const ACER_EC_PRODUCT: u32 = 0x174b;
const REPORT_TARGET_LIST: u8 = 0xa1;

const MAX_HIDRAW_ENTRIES: usize = 256;
const MAX_HID_CANDIDATES: usize = 8;
const MAX_UEVENT_BYTES: usize = 4096;
const MAX_DESCRIPTOR_BYTES: usize = 4096;
const MAX_FEATURE_REPORTS: usize = 64;
const MAX_FEATURE_REPORT_BYTES: usize = 4096;
const MAX_FEATURE_PAYLOAD_BYTES: usize = 64;
const MAX_A1_TARGETS: usize = 32;
const MAX_HID_STACK_DEPTH: usize = 32;
const MAX_DRIVER_BYTES: usize = 64;

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticHidRole {
    Enek5130Lighting,
    AcerEcHidPowerCandidate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticHidBus {
    I2c,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub(crate) enum DiagnosticHidName {
    #[serde(rename = "ENEK5130")]
    Enek5130,
    #[serde(rename = "Acer EC HID")]
    AcerEcHid,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticHidIdentity {
    pub bus: DiagnosticHidBus,
    pub vid: u16,
    pub pid: u16,
    pub name: DiagnosticHidName,
    pub interface: Option<u8>,
    pub usage_page: Option<u16>,
    pub usage: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticHidFeatureGeometry {
    pub id: u8,
    pub bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticHidDescriptor {
    pub bytes: usize,
    pub sha256: String,
    pub feature_reports: Vec<DiagnosticHidFeatureGeometry>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticHidA1 {
    pub requested_bytes: usize,
    pub returned_bytes: usize,
    pub payload_hex: String,
    pub targets: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticHid {
    pub role: DiagnosticHidRole,
    pub identity: DiagnosticHidIdentity,
    pub driver: DiagnosticObservation<String>,
    pub descriptor: DiagnosticObservation<DiagnosticHidDescriptor>,
    pub a1: Option<DiagnosticObservation<DiagnosticHidA1>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct GlobalState {
    usage_page: Option<u16>,
    report_size: Option<u32>,
    report_count: Option<u32>,
    report_id: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedDescriptor {
    feature_reports: Vec<DiagnosticHidFeatureGeometry>,
    application_usage: Option<(u16, u16)>,
}

#[derive(Debug)]
struct Candidate {
    sysfs: PathBuf,
    node: PathBuf,
    role: DiagnosticHidRole,
    identity: DiagnosticHidIdentity,
}

pub(crate) fn collect_at(root: &Path) -> Vec<DiagnosticHid> {
    collect_at_with(root, &mut read_feature_report)
}

fn collect_at_with<F>(root: &Path, feature_reader: &mut F) -> Vec<DiagnosticHid>
where
    F: FnMut(&Path, u8, usize) -> Result<Vec<u8>, DiagnosticError>,
{
    let sysfs_root = root.join("sys/class/hidraw");
    let dev_root = root.join("dev");
    let Ok(entries) = fs::read_dir(sysfs_root) else {
        return Vec::new();
    };

    let mut entries = entries
        .flatten()
        .filter_map(|entry| {
            let index = hidraw_index(&entry.file_name())?;
            Some((index, entry.path(), dev_root.join(entry.file_name())))
        })
        .take(MAX_HIDRAW_ENTRIES + 1)
        .collect::<Vec<_>>();
    if entries.len() > MAX_HIDRAW_ENTRIES {
        return Vec::new();
    }
    entries.sort_by_key(|entry| entry.0);

    let mut candidates = Vec::new();
    for (_, sysfs, node) in entries {
        let Ok(uevent) = read_bounded(&sysfs.join("device/uevent"), MAX_UEVENT_BYTES) else {
            continue;
        };
        let Some((role, identity)) = parse_allowlisted_identity(&uevent) else {
            continue;
        };
        candidates.push(Candidate {
            sysfs,
            node,
            role,
            identity,
        });
        if candidates.len() == MAX_HID_CANDIDATES {
            break;
        }
    }

    let mut inventory = candidates
        .into_iter()
        .map(|candidate| collect_candidate(candidate, feature_reader))
        .collect::<Vec<_>>();
    inventory.sort_by(|left, right| {
        compare_hid(left, right)
            // Multiple hidraw functions can expose the same privacy-safe
            // identity and descriptor.  Prefer the function that returned
            // the most complete passive evidence instead of whichever
            // volatile hidraw index happened to sort first.
            .then_with(|| evidence_quality(right).cmp(&evidence_quality(left)))
            .then_with(|| a1_payload(left).cmp(a1_payload(right)))
            .then_with(|| driver_value(left).cmp(driver_value(right)))
    });
    // The report deliberately omits volatile hidraw indexes and physical
    // paths.  Functions with the same remaining public identity are therefore
    // indistinguishable and must collapse to one stable record.
    inventory.dedup_by(|left, right| compare_hid(left, right).is_eq());
    inventory
}

fn collect_candidate<F>(mut candidate: Candidate, feature_reader: &mut F) -> DiagnosticHid
where
    F: FnMut(&Path, u8, usize) -> Result<Vec<u8>, DiagnosticError>,
{
    let driver = read_driver(&candidate.sysfs.join("device/driver"));
    let descriptor_path = candidate.sysfs.join("device/report_descriptor");
    let descriptor = match read_descriptor(&descriptor_path) {
        Ok((value, application_usage)) => {
            if let Some((usage_page, usage)) = application_usage {
                candidate.identity.usage_page = Some(usage_page);
                candidate.identity.usage = Some(usage);
            }
            DiagnosticObservation::value(value)
        }
        Err(error) => DiagnosticObservation::error(error),
    };

    let a1 = if candidate.role == DiagnosticHidRole::Enek5130Lighting {
        Some(match &descriptor {
            DiagnosticObservation::Value { value } => value
                .feature_reports
                .iter()
                .find(|geometry| geometry.id == REPORT_TARGET_LIST)
                .map_or_else(
                    || DiagnosticObservation::absent(DiagnosticAbsence::IncompleteInterface),
                    |geometry| read_a1(&candidate.node, geometry.bytes, feature_reader),
                ),
            DiagnosticObservation::Absent { .. } | DiagnosticObservation::Error { .. } => {
                DiagnosticObservation::absent(DiagnosticAbsence::IncompleteInterface)
            }
        })
    } else {
        None
    };

    DiagnosticHid {
        role: candidate.role,
        identity: candidate.identity,
        driver,
        descriptor,
        a1,
    }
}

fn compare_hid(left: &DiagnosticHid, right: &DiagnosticHid) -> std::cmp::Ordering {
    left.role
        .cmp(&right.role)
        .then_with(|| left.identity.usage_page.cmp(&right.identity.usage_page))
        .then_with(|| left.identity.usage.cmp(&right.identity.usage))
        .then_with(|| descriptor_hash(left).cmp(descriptor_hash(right)))
}

fn descriptor_hash(hid: &DiagnosticHid) -> &str {
    match &hid.descriptor {
        DiagnosticObservation::Value { value } => value.sha256.as_str(),
        DiagnosticObservation::Absent { .. } | DiagnosticObservation::Error { .. } => "",
    }
}

fn evidence_quality(hid: &DiagnosticHid) -> (u8, u8, u8) {
    (
        observation_quality(&hid.descriptor),
        hid.a1.as_ref().map_or(0, observation_quality),
        observation_quality(&hid.driver),
    )
}

const fn observation_quality<T>(observation: &DiagnosticObservation<T>) -> u8 {
    match observation {
        DiagnosticObservation::Value { .. } => 2,
        DiagnosticObservation::Error { .. } => 1,
        DiagnosticObservation::Absent { .. } => 0,
    }
}

fn a1_payload(hid: &DiagnosticHid) -> &str {
    match &hid.a1 {
        Some(DiagnosticObservation::Value { value }) => value.payload_hex.as_str(),
        Some(DiagnosticObservation::Absent { .. })
        | Some(DiagnosticObservation::Error { .. })
        | None => "",
    }
}

fn driver_value(hid: &DiagnosticHid) -> &str {
    match &hid.driver {
        DiagnosticObservation::Value { value } => value.as_str(),
        DiagnosticObservation::Absent { .. } | DiagnosticObservation::Error { .. } => "",
    }
}

fn hidraw_index(name: &std::ffi::OsStr) -> Option<u32> {
    let name = name.to_str()?;
    let digits = name.strip_prefix("hidraw")?;
    if digits.is_empty() || digits.len() > 10 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn parse_allowlisted_identity(uevent: &[u8]) -> Option<(DiagnosticHidRole, DiagnosticHidIdentity)> {
    let text = std::str::from_utf8(uevent).ok()?;
    let mut parsed = None;
    for line in text.lines() {
        let Some(value) = line.strip_prefix("HID_ID=") else {
            continue;
        };
        let mut fields = value.trim().split(':');
        let (Some(bus), Some(vendor), Some(product), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        let bus = u32::from_str_radix(bus, 16).ok()?;
        let vendor = u32::from_str_radix(vendor, 16).ok()?;
        let product = u32::from_str_radix(product, 16).ok()?;
        let identity = match (bus, vendor, product) {
            (HID_BUS_I2C, ENEK_VENDOR, ENEK_PRODUCT) => (
                DiagnosticHidRole::Enek5130Lighting,
                DiagnosticHidName::Enek5130,
            ),
            (HID_BUS_I2C, ACER_EC_VENDOR, ACER_EC_PRODUCT) => (
                DiagnosticHidRole::AcerEcHidPowerCandidate,
                DiagnosticHidName::AcerEcHid,
            ),
            _ => return None,
        };
        if parsed.is_some() {
            return None;
        }
        parsed = Some((
            identity.0,
            DiagnosticHidIdentity {
                bus: DiagnosticHidBus::I2c,
                vid: u16::try_from(vendor).ok()?,
                pid: u16::try_from(product).ok()?,
                name: identity.1,
                // Both allow-listed devices are HID-over-I2C.  A USB
                // interface number would be fabricated here, so it is absent.
                interface: None,
                usage_page: None,
                usage: None,
            },
        ));
    }
    parsed
}

fn read_driver(path: &Path) -> DiagnosticObservation<String> {
    match fs::read_link(path) {
        Ok(target) => {
            let Some(driver) = target.file_name().and_then(|name| name.to_str()) else {
                return DiagnosticObservation::error(DiagnosticError::new(
                    DiagnosticErrorStage::Decode,
                    DiagnosticErrorClass::InvalidValue,
                    None,
                ));
            };
            if driver.is_empty()
                || driver.len() > MAX_DRIVER_BYTES
                || !driver
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return DiagnosticObservation::error(DiagnosticError::new(
                    DiagnosticErrorStage::Decode,
                    DiagnosticErrorClass::InvalidValue,
                    None,
                ));
            }
            DiagnosticObservation::value(driver.to_string())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DiagnosticObservation::absent(DiagnosticAbsence::NotExposed)
        }
        Err(error) => {
            DiagnosticObservation::error(io_diagnostic(DiagnosticErrorStage::Read, &error))
        }
    }
}

fn read_descriptor(
    path: &Path,
) -> Result<(DiagnosticHidDescriptor, Option<(u16, u16)>), DiagnosticError> {
    let bytes = read_bounded(path, MAX_DESCRIPTOR_BYTES)?;
    if bytes.is_empty() {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        ));
    }
    let parsed = parse_report_descriptor(&bytes)?;
    let sha256 = hex(&Sha256::digest(&bytes));
    Ok((
        DiagnosticHidDescriptor {
            bytes: bytes.len(),
            sha256,
            feature_reports: parsed.feature_reports,
        },
        parsed.application_usage,
    ))
}

fn read_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, DiagnosticError> {
    let file =
        File::open(path).map_err(|error| io_diagnostic(DiagnosticErrorStage::Open, &error))?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(max_bytes + 1).expect("bounded HID input fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|error| io_diagnostic(DiagnosticErrorStage::Read, &error))?;
    if bytes.len() > max_bytes {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Read,
            DiagnosticErrorClass::Oversize,
            None,
        ));
    }
    Ok(bytes)
}

fn read_a1<F>(
    node: &Path,
    requested_bytes: usize,
    feature_reader: &mut F,
) -> DiagnosticObservation<DiagnosticHidA1>
where
    F: FnMut(&Path, u8, usize) -> Result<Vec<u8>, DiagnosticError>,
{
    if !(2..=MAX_FEATURE_PAYLOAD_BYTES).contains(&requested_bytes) {
        return DiagnosticObservation::error(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            if requested_bytes > MAX_FEATURE_PAYLOAD_BYTES {
                DiagnosticErrorClass::Oversize
            } else {
                DiagnosticErrorClass::InvalidValue
            },
            None,
        ));
    }
    match feature_reader(node, REPORT_TARGET_LIST, requested_bytes) {
        Ok(payload) => match parse_a1_payload(requested_bytes, payload) {
            Ok(value) => DiagnosticObservation::value(value),
            Err(error) => DiagnosticObservation::error(error),
        },
        Err(error) => DiagnosticObservation::error(error),
    }
}

fn parse_a1_payload(
    requested_bytes: usize,
    payload: Vec<u8>,
) -> Result<DiagnosticHidA1, DiagnosticError> {
    if payload.len() < 2 || payload.len() > requested_bytes || payload[0] != REPORT_TARGET_LIST {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        ));
    }
    let count = usize::from(payload[1]);
    let end = 2_usize.checked_add(count).ok_or_else(|| {
        DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        )
    })?;
    if count > MAX_A1_TARGETS || end > payload.len() {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            if count > MAX_A1_TARGETS {
                DiagnosticErrorClass::Oversize
            } else {
                DiagnosticErrorClass::InvalidValue
            },
            None,
        ));
    }
    let mut targets = payload[2..end].to_vec();
    targets.sort_unstable();
    targets.dedup();
    Ok(DiagnosticHidA1 {
        requested_bytes,
        returned_bytes: payload.len(),
        payload_hex: hex(&payload),
        targets,
    })
}

fn read_feature_report(
    node: &Path,
    report_id: u8,
    requested_bytes: usize,
) -> Result<Vec<u8>, DiagnosticError> {
    if !(2..=MAX_FEATURE_PAYLOAD_BYTES).contains(&requested_bytes) {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::Oversize,
            None,
        ));
    }
    let file =
        File::open(node).map_err(|error| io_diagnostic(DiagnosticErrorStage::Open, &error))?;
    let mut report = vec![0_u8; requested_bytes];
    report[0] = report_id;
    let request = get_feature_ioctl(requested_bytes)?;
    // SAFETY: `report` is writable for the exact length encoded in the fixed
    // HIDIOCGFEATURE request and `file` is owned by this process.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request, report.as_mut_ptr()) };
    if result < 0 {
        return Err(io_diagnostic(
            DiagnosticErrorStage::Read,
            &io::Error::last_os_error(),
        ));
    }
    let returned = usize::try_from(result).map_err(|_| {
        DiagnosticError::new(
            DiagnosticErrorStage::Read,
            DiagnosticErrorClass::InvalidValue,
            None,
        )
    })?;
    if returned == 0 || returned > report.len() {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Read,
            DiagnosticErrorClass::InvalidValue,
            None,
        ));
    }
    report.truncate(returned);
    Ok(report)
}

fn get_feature_ioctl(len: usize) -> Result<libc::c_ulong, DiagnosticError> {
    const IOC_WRITE: u64 = 1;
    const IOC_READ: u64 = 2;
    const IOC_DIR_SHIFT: u32 = 30;
    const IOC_SIZE_SHIFT: u32 = 16;
    const IOC_TYPE_SHIFT: u32 = 8;
    const HIDIOCGFEATURE: u64 = 0x07;

    if !(2..=MAX_FEATURE_PAYLOAD_BYTES).contains(&len) {
        return Err(DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::Oversize,
            None,
        ));
    }
    let request = ((IOC_READ | IOC_WRITE) << IOC_DIR_SHIFT)
        | ((len as u64) << IOC_SIZE_SHIFT)
        | (u64::from(b'H') << IOC_TYPE_SHIFT)
        | HIDIOCGFEATURE;
    libc::c_ulong::try_from(request).map_err(|_| {
        DiagnosticError::new(
            DiagnosticErrorStage::Decode,
            DiagnosticErrorClass::InvalidValue,
            None,
        )
    })
}

fn parse_report_descriptor(bytes: &[u8]) -> Result<ParsedDescriptor, DiagnosticError> {
    let mut offset = 0_usize;
    let mut global = GlobalState::default();
    let mut global_stack = Vec::new();
    let mut collection_stack: Vec<Option<(u16, u16)>> = Vec::new();
    let mut local_usage = None;
    let mut feature_bits = BTreeMap::<u8, u64>::new();
    let mut feature_usages = BTreeSet::new();
    let mut feature_without_usage = false;

    while offset < bytes.len() {
        let prefix = bytes[offset];
        offset += 1;
        if prefix == 0xfe {
            return Err(decode_error(DiagnosticErrorClass::InvalidValue));
        }
        let size = match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => unreachable!(),
        };
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| decode_error(DiagnosticErrorClass::InvalidValue))?;
        let data = &bytes[offset..end];
        offset = end;
        let item_type = (prefix >> 2) & 0x03;
        let tag = prefix >> 4;
        let value = unsigned_item(data);

        match (item_type, tag) {
            // Global: Usage Page, Report Size, Report ID, Report Count.
            (1, 0) => {
                global.usage_page = Some(
                    u16::try_from(value)
                        .map_err(|_| decode_error(DiagnosticErrorClass::InvalidValue))?,
                );
            }
            (1, 7) => global.report_size = Some(value),
            (1, 8) => {
                let report_id = u8::try_from(value)
                    .map_err(|_| decode_error(DiagnosticErrorClass::InvalidValue))?;
                if report_id == 0 {
                    return Err(decode_error(DiagnosticErrorClass::InvalidValue));
                }
                global.report_id = report_id;
            }
            (1, 9) => global.report_count = Some(value),
            (1, 10) => {
                if global_stack.len() == MAX_HID_STACK_DEPTH {
                    return Err(decode_error(DiagnosticErrorClass::Oversize));
                }
                global_stack.push(global);
            }
            (1, 11) => {
                global = global_stack
                    .pop()
                    .ok_or_else(|| decode_error(DiagnosticErrorClass::InvalidValue))?;
            }
            // Local: first Usage owns the following Collection.
            (2, 0) if local_usage.is_none() => {
                local_usage = usage_from_item(global.usage_page, data);
            }
            // Main: Collection.
            (0, 10) => {
                if collection_stack.len() == MAX_HID_STACK_DEPTH {
                    return Err(decode_error(DiagnosticErrorClass::Oversize));
                }
                let parent_application = collection_stack.last().copied().flatten();
                let application = if value == 1 && parent_application.is_none() {
                    local_usage
                } else {
                    parent_application
                };
                collection_stack.push(application);
                local_usage = None;
            }
            // Main: End Collection.
            (0, 12) => {
                collection_stack
                    .pop()
                    .ok_or_else(|| decode_error(DiagnosticErrorClass::InvalidValue))?;
                local_usage = None;
            }
            // Main: Feature.
            (0, 11) => {
                let report_size = u64::from(
                    global
                        .report_size
                        .ok_or_else(|| decode_error(DiagnosticErrorClass::InvalidValue))?,
                );
                let report_count = u64::from(
                    global
                        .report_count
                        .ok_or_else(|| decode_error(DiagnosticErrorClass::InvalidValue))?,
                );
                let bits = report_size
                    .checked_mul(report_count)
                    .filter(|bits| *bits != 0)
                    .ok_or_else(|| decode_error(DiagnosticErrorClass::InvalidValue))?;
                let total = feature_bits.entry(global.report_id).or_default();
                *total = total
                    .checked_add(bits)
                    .ok_or_else(|| decode_error(DiagnosticErrorClass::Oversize))?;
                if feature_bits.len() > MAX_FEATURE_REPORTS {
                    return Err(decode_error(DiagnosticErrorClass::Oversize));
                }
                match collection_stack.last().copied().flatten() {
                    Some(usage) => {
                        feature_usages.insert(usage);
                    }
                    None => feature_without_usage = true,
                }
                local_usage = None;
            }
            // Every other Main item consumes local state.
            (0, _) => local_usage = None,
            _ => {}
        }
    }

    if !global_stack.is_empty() || !collection_stack.is_empty() {
        return Err(decode_error(DiagnosticErrorClass::InvalidValue));
    }
    let mut feature_reports = Vec::with_capacity(feature_bits.len());
    for (id, bits) in feature_bits {
        let payload_bytes = bits
            .checked_add(7)
            .map(|bits| bits / 8)
            .ok_or_else(|| decode_error(DiagnosticErrorClass::Oversize))?;
        let report_bytes = payload_bytes
            .checked_add(u64::from(id != 0))
            .ok_or_else(|| decode_error(DiagnosticErrorClass::Oversize))?;
        let report_bytes = usize::try_from(report_bytes)
            .map_err(|_| decode_error(DiagnosticErrorClass::Oversize))?;
        if report_bytes == 0 || report_bytes > MAX_FEATURE_REPORT_BYTES {
            return Err(decode_error(DiagnosticErrorClass::Oversize));
        }
        feature_reports.push(DiagnosticHidFeatureGeometry {
            id,
            bytes: report_bytes,
        });
    }
    let application_usage = if !feature_without_usage && feature_usages.len() == 1 {
        feature_usages.into_iter().next()
    } else {
        None
    };
    Ok(ParsedDescriptor {
        feature_reports,
        application_usage,
    })
}

fn usage_from_item(global_page: Option<u16>, data: &[u8]) -> Option<(u16, u16)> {
    let value = unsigned_item(data);
    if data.len() == 4 && value > u32::from(u16::MAX) {
        Some(((value >> 16) as u16, value as u16))
    } else {
        Some((global_page?, u16::try_from(value).ok()?))
    }
}

fn unsigned_item(data: &[u8]) -> u32 {
    let mut bytes = [0_u8; 4];
    bytes[..data.len()].copy_from_slice(data);
    u32::from_le_bytes(bytes)
}

fn decode_error(class: DiagnosticErrorClass) -> DiagnosticError {
    DiagnosticError::new(DiagnosticErrorStage::Decode, class, None)
}

fn io_diagnostic(stage: DiagnosticErrorStage, error: &io::Error) -> DiagnosticError {
    let class = match error.kind() {
        io::ErrorKind::NotFound => DiagnosticErrorClass::NotFound,
        io::ErrorKind::PermissionDenied => DiagnosticErrorClass::PermissionDenied,
        _ => DiagnosticErrorClass::Io,
    };
    DiagnosticError::new(
        stage,
        class,
        error.raw_os_error().filter(|errno| *errno >= 0),
    )
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(crate) fn validate_inventory(inventory: &[DiagnosticHid]) -> Result<(), String> {
    if inventory.len() > MAX_HID_CANDIDATES {
        return Err("passive HID inventory is oversized".to_string());
    }
    for (index, hid) in inventory.iter().enumerate() {
        validate_hid(hid)?;
        if index > 0 && !compare_hid(&inventory[index - 1], hid).is_lt() {
            return Err("passive HID inventory is not stably sorted and unique".to_string());
        }
    }
    Ok(())
}

fn validate_hid(hid: &DiagnosticHid) -> Result<(), String> {
    let expected = match hid.role {
        DiagnosticHidRole::Enek5130Lighting => (
            ENEK_VENDOR as u16,
            ENEK_PRODUCT as u16,
            DiagnosticHidName::Enek5130,
        ),
        DiagnosticHidRole::AcerEcHidPowerCandidate => (
            ACER_EC_VENDOR as u16,
            ACER_EC_PRODUCT as u16,
            DiagnosticHidName::AcerEcHid,
        ),
    };
    if hid.identity.bus != DiagnosticHidBus::I2c
        || (hid.identity.vid, hid.identity.pid, hid.identity.name) != expected
        || hid.identity.interface.is_some()
        || hid.identity.usage_page.is_some() != hid.identity.usage.is_some()
    {
        return Err("passive HID identity differs from its exact allow-list".to_string());
    }
    validate_observation(&hid.driver, |driver| {
        if driver.is_empty()
            || driver.len() > MAX_DRIVER_BYTES
            || !driver
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("passive HID driver token is invalid".to_string());
        }
        Ok(())
    })?;
    validate_observation(&hid.descriptor, |descriptor| {
        if descriptor.bytes == 0
            || descriptor.bytes > MAX_DESCRIPTOR_BYTES
            || descriptor.sha256.len() != 64
            || !descriptor
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || descriptor.feature_reports.len() > MAX_FEATURE_REPORTS
        {
            return Err("passive HID descriptor evidence is invalid".to_string());
        }
        let mut prior = None;
        for geometry in &descriptor.feature_reports {
            if geometry.bytes == 0 || geometry.bytes > MAX_FEATURE_REPORT_BYTES {
                return Err("passive HID feature geometry is invalid".to_string());
            }
            if prior.is_some_and(|prior| geometry.id <= prior) {
                return Err("passive HID feature geometry is not sorted and unique".to_string());
            }
            prior = Some(geometry.id);
        }
        Ok(())
    })?;
    match (hid.role, &hid.a1) {
        (DiagnosticHidRole::Enek5130Lighting, Some(a1)) => {
            validate_observation(a1, validate_a1)?;
        }
        (DiagnosticHidRole::Enek5130Lighting, None) => {
            return Err("ENEK passive HID evidence omits A1 status".to_string());
        }
        (DiagnosticHidRole::AcerEcHidPowerCandidate, None) => {}
        (DiagnosticHidRole::AcerEcHidPowerCandidate, Some(_)) => {
            return Err("Acer EC-HID candidate contains ENEK A1 evidence".to_string());
        }
    }
    Ok(())
}

fn validate_a1(a1: &DiagnosticHidA1) -> Result<(), String> {
    if !(2..=MAX_FEATURE_PAYLOAD_BYTES).contains(&a1.requested_bytes)
        || !(2..=a1.requested_bytes).contains(&a1.returned_bytes)
        || a1.payload_hex.len() != a1.returned_bytes * 2
        || !a1.payload_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !a1.payload_hex.starts_with("a1")
        || a1.targets.len() > MAX_A1_TARGETS
        || !a1.targets.windows(2).all(|pair| pair[0] < pair[1])
    {
        return Err("passive ENEK A1 evidence is invalid".to_string());
    }
    Ok(())
}

fn validate_observation<T>(
    observation: &DiagnosticObservation<T>,
    validate: impl FnOnce(&T) -> Result<(), String>,
) -> Result<(), String> {
    match observation {
        DiagnosticObservation::Value { value } => validate(value),
        DiagnosticObservation::Absent { .. } => Ok(()),
        DiagnosticObservation::Error { error } => validate_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_ORDINAL: AtomicU64 = AtomicU64::new(0);

    fn descriptor_with_reports() -> Vec<u8> {
        vec![
            0x06, 0x00, 0xff, // Usage Page ff00
            0x09, 0x01, // Usage 0001
            0xa1, 0x01, // Collection Application
            0x75, 0x08, // Report Size 8
            0x85, 0xa1, 0x95, 0x0b, 0xb1, 0x02, // A1: 11 + ID = 12
            0x85, 0xa2, 0x95, 0x01, 0xb1, 0x02, // A2: 1 + ID = 2
            0x85, 0xa3, 0x95, 0x0b, 0xb1, 0x02, // A3: 11 + ID = 12
            0x85, 0xa4, 0x95, 0x0a, 0xb1, 0x02, // A4: 10 + ID = 11
            0xc0,
        ]
    }

    fn test_root() -> PathBuf {
        let ordinal = TEMP_ORDINAL.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "asense-passive-hid-{}-{ordinal}",
            std::process::id()
        ))
    }

    #[test]
    fn identity_requires_exact_hid_id_and_ignores_name_substrings() {
        assert!(
            parse_allowlisted_identity(
                b"HID_NAME=1025174B:00 6243:0001\nHID_ID=0018:00006243:00000001\n"
            )
            .is_none()
        );
        assert_eq!(
            parse_allowlisted_identity(b"HID_ID=0018:00001025:0000174B\n")
                .unwrap()
                .0,
            DiagnosticHidRole::AcerEcHidPowerCandidate
        );
        assert!(parse_allowlisted_identity(b"HID_ID=0003:00001025:0000174B\n").is_none());
        assert!(parse_allowlisted_identity(b"HID_NAME=ENEK5130\n").is_none());
    }

    #[test]
    fn descriptor_parser_aggregates_exact_feature_geometry_and_usage() {
        let parsed = parse_report_descriptor(&descriptor_with_reports()).unwrap();
        assert_eq!(parsed.application_usage, Some((0xff00, 0x0001)));
        assert_eq!(
            parsed.feature_reports,
            vec![
                DiagnosticHidFeatureGeometry {
                    id: 0xa1,
                    bytes: 12
                },
                DiagnosticHidFeatureGeometry { id: 0xa2, bytes: 2 },
                DiagnosticHidFeatureGeometry {
                    id: 0xa3,
                    bytes: 12
                },
                DiagnosticHidFeatureGeometry {
                    id: 0xa4,
                    bytes: 11
                },
            ]
        );
    }

    #[test]
    fn descriptor_parser_handles_push_pop_and_rejects_malformed_or_oversize_geometry() {
        let pushed = [
            0x06, 0x05, 0xff, 0x09, 0x01, 0xa1, 0x01, 0xa4, 0x75, 0x08, 0x85, 0xa0, 0x95, 0x40,
            0xb1, 0x02, 0xb4, 0xc0,
        ];
        let parsed = parse_report_descriptor(&pushed).unwrap();
        assert_eq!(parsed.application_usage, Some((0xff05, 0x0001)));
        assert_eq!(parsed.feature_reports[0].bytes, 65);
        assert!(parse_report_descriptor(&[0xfe, 0x00, 0x00]).is_err());
        assert!(parse_report_descriptor(&[0xb4]).is_err());
        assert!(parse_report_descriptor(&[0xc0]).is_err());

        let oversize = [
            0x75, 0x20, // 32 bits
            0x96, 0x01, 0x04, // count 1025 => 4100 bytes
            0x85, 0x01, 0xb1, 0x02,
        ];
        assert!(parse_report_descriptor(&oversize).is_err());
    }

    #[test]
    fn a1_keeps_exact_payload_and_sorts_unique_targets() {
        let a1 = parse_a1_payload(12, vec![0xa1, 4, 0x83, 0x21, 0x65, 0x21, 0, 0]).unwrap();
        assert_eq!(a1.requested_bytes, 12);
        assert_eq!(a1.returned_bytes, 8);
        assert_eq!(a1.payload_hex, "a104832165210000");
        assert_eq!(a1.targets, [0x21, 0x65, 0x83]);
        assert!(parse_a1_payload(12, vec![0xa1, 3, 0x21]).is_err());
        assert!(parse_a1_payload(12, vec![0xa2, 0]).is_err());
    }

    #[test]
    fn fake_collector_uses_descriptor_sized_a1_without_any_selector() {
        let root = test_root();
        let device = root.join("sys/class/hidraw/hidraw7/device");
        fs::create_dir_all(&device).unwrap();
        fs::create_dir_all(root.join("dev")).unwrap();
        fs::write(
            device.join("uevent"),
            b"HID_NAME=untrusted name\nHID_ID=0018:00000CF2:00005130\n",
        )
        .unwrap();
        fs::write(device.join("report_descriptor"), descriptor_with_reports()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            "../../../../bus/hid/drivers/hid-generic",
            device.join("driver"),
        )
        .unwrap();

        let mut calls = Vec::new();
        let mut reader = |node: &Path, report_id: u8, bytes: usize| {
            calls.push((node.to_path_buf(), report_id, bytes));
            Ok(vec![0xa1, 3, 0x83, 0x21, 0x65, 0, 0, 0, 0, 0, 0, 0])
        };
        let inventory = collect_at_with(&root, &mut reader);
        assert_eq!(inventory.len(), 1);
        assert_eq!(calls, [(root.join("dev/hidraw7"), 0xa1, 12)]);
        assert_eq!(inventory[0].identity.usage_page, Some(0xff00));
        assert_eq!(inventory[0].identity.usage, Some(0x0001));
        assert_eq!(inventory[0].identity.interface, None);
        assert_eq!(
            inventory[0].a1,
            Some(DiagnosticObservation::value(DiagnosticHidA1 {
                requested_bytes: 12,
                returned_bytes: 12,
                payload_hex: "a10383216500000000000000".to_string(),
                targets: vec![0x21, 0x65, 0x83],
            }))
        );
        validate_inventory(&inventory).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn acer_ec_candidate_is_detection_only_and_never_reads_a1() {
        let root = test_root();
        let device = root.join("sys/class/hidraw/hidraw2/device");
        fs::create_dir_all(&device).unwrap();
        fs::write(device.join("uevent"), b"HID_ID=0018:00001025:0000174B\n").unwrap();
        fs::write(
            device.join("report_descriptor"),
            [
                0x06, 0x05, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x75, 0x08, 0x85, 0xa0, 0x95, 0x40, 0xb1,
                0x02, 0xc0,
            ],
        )
        .unwrap();
        let mut reader = |_: &Path, _: u8, _: usize| -> Result<Vec<u8>, DiagnosticError> {
            panic!("EC-HID detection must not issue a feature request")
        };
        let inventory = collect_at_with(&root, &mut reader);
        assert_eq!(inventory.len(), 1);
        assert_eq!(
            inventory[0].role,
            DiagnosticHidRole::AcerEcHidPowerCandidate
        );
        assert_eq!(inventory[0].identity.usage_page, Some(0xff05));
        assert_eq!(inventory[0].identity.usage, Some(0x0001));
        assert!(inventory[0].a1.is_none());
        validate_inventory(&inventory).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_public_functions_keep_the_successful_passive_a1() {
        let root = test_root();
        fs::create_dir_all(root.join("dev")).unwrap();
        for index in [7, 8] {
            let device = root.join(format!("sys/class/hidraw/hidraw{index}/device"));
            fs::create_dir_all(&device).unwrap();
            fs::write(device.join("uevent"), b"HID_ID=0018:00000CF2:00005130\n").unwrap();
            fs::write(device.join("report_descriptor"), descriptor_with_reports()).unwrap();
        }

        let mut reader = |node: &Path, _: u8, _: usize| {
            if node.ends_with("hidraw7") {
                Err(DiagnosticError::new(
                    DiagnosticErrorStage::Read,
                    DiagnosticErrorClass::Io,
                    Some(libc::EIO),
                ))
            } else {
                Ok(vec![0xa1, 2, 0x83, 0x21])
            }
        };
        let inventory = collect_at_with(&root, &mut reader);
        assert_eq!(inventory.len(), 1);
        assert!(matches!(
            &inventory[0].a1,
            Some(DiagnosticObservation::Value { value })
                if value.targets == [0x21, 0x83]
        ));
        validate_inventory(&inventory).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
