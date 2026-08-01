const PASSIVE: &str = include_str!("../src/passive_diagnostics.rs");
const PASSIVE_HID: &str = include_str!("../src/passive_hid.rs");
const CONTROL: &str = include_str!("../src/control.rs");
const DAEMON: &str = include_str!("../src/daemon.rs");
const PROBE: &str = include_str!("../src/probe.rs");

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("source start marker must exist");
    let tail = &source[start..];
    let end = tail.find(end).expect("source end marker must exist");
    &tail[..end]
}

#[test]
fn passive_collector_has_only_fixed_read_dependencies() {
    for required in [
        "File::open(path)",
        "read_to_end(&mut bytes)",
        "discover_acer_hwmon",
        "find_wmi_group",
        "collect_lighting(root)",
        "zone_mask",
    ] {
        assert!(
            PASSIVE.contains(required),
            "missing passive read: {required}"
        );
    }
    for forbidden in [
        "crate::lighting",
        "crate::nvidia",
        "MutationGuard",
        "set_feature",
        "write_all",
        "OpenOptions",
        "wmidev_evaluate_method",
        "CAPS",
    ] {
        assert!(
            !PASSIVE.contains(forbidden),
            "passive collector gained a mutation/raw dependency: {forbidden}"
        );
    }
}

#[test]
fn passive_hid_has_one_fixed_get_feature_and_no_write_or_selector_surface() {
    let production = PASSIVE_HID
        .split("#[cfg(test)]")
        .next()
        .expect("passive HID production source exists");
    for required in [
        "File::open(node)",
        "const HIDIOCGFEATURE: u64 = 0x07",
        "read_feature_report",
        "REPORT_TARGET_LIST",
        "parse_report_descriptor",
        "parse_allowlisted_identity",
    ] {
        assert!(
            production.contains(required),
            "missing bounded passive HID primitive: {required}"
        );
    }
    for forbidden in [
        "HIDIOCSFEATURE",
        "OpenOptions",
        ".write(true)",
        "set_feature",
        "REPORT_TARGET_SELECT",
        "REPORT_TARGET_CAPABILITIES",
        "REPORT_LIGHTING",
        "crate::lighting",
        "CAPS",
    ] {
        assert!(
            !production.contains(forbidden),
            "passive HID collector gained a mutation/discovery dependency: {forbidden}"
        );
    }
    assert!(production.contains("(HID_BUS_I2C, ENEK_VENDOR, ENEK_PRODUCT)"));
    assert!(production.contains("(HID_BUS_I2C, ACER_EC_VENDOR, ACER_EC_PRODUCT)"));
    assert!(!production.contains("HID_NAME="));
}

#[test]
fn schema_three_client_and_probe_use_diag_without_caps_fallback() {
    let client = between(
        CONTROL,
        "pub(crate) fn passive_diagnostics",
        "pub fn fan_maximum",
    );
    assert!(client.contains("self.request(\"DIAG PASSIVE\")"));
    assert!(!client.contains("CAPS"));

    let collection = between(PROBE, "fn collect_daemon_context", "fn control_probe_error");
    assert!(collection.contains("client.passive_diagnostics()"));
    assert!(!collection.contains("capabilities()"));
    assert!(!collection.contains("CAPS"));
}

#[test]
fn daemon_diag_branch_only_collects_and_encodes_passive_evidence() {
    let branch = between(DAEMON, "[\"DIAG\", \"PASSIVE\"] => {", "[\"CAPS\"] => {");
    assert!(branch.contains("PassiveDiagnostics::collect()"));
    assert!(branch.contains("encode_passive_diagnostics(&diagnostics)"));
    for forbidden in [
        "hardware.",
        "MutationGuard",
        "LightingController",
        "restore_cached_enek_lighting",
        "reconcile_pending_nvidia",
        "apply_",
        "write_",
        "set_",
    ] {
        assert!(
            !branch.contains(forbidden),
            "DIAG PASSIVE branch gained a side effect: {forbidden}"
        );
    }

    let maintenance = between(
        DAEMON,
        "fn command_activates_runtime_maintenance",
        "fn parse_lighting_mode",
    );
    assert!(maintenance.contains("!matches!(fields, [\"DIAG\", \"PASSIVE\"])"));
}

#[test]
fn additive_passive_command_preserves_the_old_caps_path() {
    let caps = between(DAEMON, "[\"CAPS\"] => {", "[\"HARDWARE\", \"GET\"] => Ok(");
    assert!(caps.contains("AcerHardware::discover()"));
    assert!(caps.contains("collect_control_capabilities(&refreshed)"));
    assert!(caps.contains("encode_control_capabilities(&capabilities)"));
    assert!(CONTROL.contains("self.request(\"CAPS\")"));
}
