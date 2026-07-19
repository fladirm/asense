const DRIVER: &str = include_str!("../kernel/asense_rgb.c");

fn compact(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn definition(name: &str) -> usize {
    let name = name.split_whitespace().last().unwrap_or(name);
    DRIVER
        .match_indices("\nstatic ")
        .map(|(offset, _)| offset + 1)
        .find(|&offset| {
            DRIVER[offset..].lines().next().is_some_and(|line| {
                line.split(|character: char| character != '_' && !character.is_alphanumeric())
                    .any(|token| token == name)
            })
        })
        .unwrap_or_else(|| panic!("protocol definition must exist: {name}"))
}

fn body(start: &str, end: &str) -> String {
    compact(&DRIVER[definition(start)..definition(end)])
}

fn function_between(start: &str, end: &str) -> &'static str {
    let start = DRIVER.find(start).expect("protocol function must exist");
    let tail = &DRIVER[start..];
    let end = tail
        .find(end)
        .expect("protocol function must have a boundary");
    &tail[..end]
}

#[test]
fn static_rgb_transaction_keeps_the_verified_wmi_order() {
    assert!(DRIVER.contains("#define ASENSE_GAMING_SYS_INFO_GET 0x05"));
    assert!(DRIVER.contains("#define ASENSE_GAMING_LED_SET 0x02"));
    assert!(DRIVER.contains("#define ASENSE_STATIC_ENGINE 0x00"));
    assert!(DRIVER.contains("#define ASENSE_FOUR_ZONE_ENABLE 0x00000f0000000008ULL"));
    assert_eq!(
        0x0000_0f00_0000_0008_u64.to_le_bytes(),
        [0x08, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00]
    );

    let body = function_between(
        "static int asense_write_zones",
        "static ssize_t zones_store",
    );
    let calls = [
        "asense_scalar_call(rgb, ASENSE_GAMING_SYS_INFO_GET, 0, &gaming_sys_info)",
        "asense_scalar_set(rgb, ASENSE_GAMING_LED_SET, ASENSE_FOUR_ZONE_ENABLE)",
        "asense_write_zone(rgb, zone, zones->rgb[zone])",
        "asense_write_effect(rgb, &static_mode)",
        "asense_read_effect(rgb, &actual)",
    ];
    let body = compact(body);
    assert!(
        body.contains(
            "struct asense_effect static_mode = asense_static_effect(zones->brightness);"
        )
    );
    let mut previous = 0;
    for call in calls {
        let position = body
            .find(call)
            .unwrap_or_else(|| panic!("missing protocol step: {call}"));
        assert!(
            position >= previous,
            "protocol step is out of order: {call}"
        );
        previous = position;
    }
    assert!(body.contains("if (rgb->zone_preamble)"));
    assert!(body.contains("for (zone = 0; zone < count; zone++)"));
}

#[test]
fn binding_is_acer_vendor_and_known_endpoint_gated_but_bios_agnostic() {
    assert!(DRIVER.contains("dmi_match(DMI_SYS_VENDOR, \"Acer\")"));
    let probe = body("asense_rgb_probe", "asense_gaming_endpoint");
    assert!(!probe.contains("DMI_PRODUCT_NAME"));
    for endpoint in ["ASENSE_RGB_GUID", "ASENSE_BATTERY_GUID", "ASENSE_APGE_GUID"] {
        assert!(DRIVER.contains(endpoint));
    }
    assert!(DRIVER.contains("{ ASENSE_RGB_GUID, &asense_gaming_endpoint }"));
    assert!(DRIVER.contains("{ ASENSE_BATTERY_GUID, &asense_battery_endpoint }"));
    assert!(DRIVER.contains("{ ASENSE_APGE_GUID, &asense_apge_endpoint }"));
    assert!(!DRIVER.contains("DMI_BIOS_VERSION"));
}

#[test]
fn multi_instance_wmi_binding_is_enabled_when_the_kernel_abi_supports_it() {
    assert!(DRIVER.contains("#include <linux/version.h>"));
    let driver = function_between(
        "static struct wmi_driver asense_rgb_driver",
        "module_wmi_driver(asense_rgb_driver)",
    );
    let driver = compact(driver);
    assert!(driver.contains("#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 9, 0)"));
    assert!(driver.contains(".no_singleton = true"));

    let probe = body("asense_rgb_probe", "asense_gaming_endpoint");
    assert!(probe.contains("dev_set_drvdata(&wdev->dev, rgb)"));
    assert!(DRIVER.contains("wmidev_evaluate_method(rgb->wdev"));
}

#[test]
fn known_endpoints_probe_and_register_independently() {
    let probe = body("asense_rgb_probe", "asense_gaming_endpoint");
    for dispatch in [
        "case ASENSE_ENDPOINT_GAMING: return asense_probe_gaming(rgb)",
        "case ASENSE_ENDPOINT_BATTERY: return asense_probe_battery(rgb)",
        "case ASENSE_ENDPOINT_APGE: return asense_probe_apge(rgb)",
    ] {
        assert!(
            probe.contains(dispatch),
            "missing endpoint dispatch: {dispatch}"
        );
    }

    let gaming = body("asense_probe_gaming", "asense_probe_battery");
    assert!(gaming.contains("devm_device_add_group(&rgb->wdev->dev, &asense_rgb_group)"));
    assert!(gaming.contains("devm_device_add_group(&rgb->wdev->dev, &asense_fan_group)"));
    assert!(gaming.contains("devm_device_add_group(&rgb->wdev->dev, &asense_profile_group)"));
    assert!(!gaming.contains("asense_read_battery"));
    assert!(!gaming.contains("asense_read_usb"));

    let battery = body("asense_probe_battery", "asense_probe_apge");
    assert!(battery.contains("asense_read_battery(rgb, &battery)"));
    assert!(battery.contains("&asense_battery_group"));

    let apge = body("asense_probe_apge", "asense_rgb_probe");
    assert!(apge.contains("asense_read_usb(rgb, &threshold)"));
    assert!(apge.contains("asense_read_timeout(rgb, &enabled)"));
    assert!(apge.contains("&asense_apge_group"));
}

#[test]
fn gaming_fan_surface_is_typed_bounded_and_hwmon_compatible() {
    for definition in [
        "#define ASENSE_FAN_BEHAVIOR_SET 0x0e",
        "#define ASENSE_FAN_BEHAVIOR_GET 0x0f",
        "#define ASENSE_FAN_SPEED_SET 0x10",
        "#define ASENSE_FAN_SPEED_GET 0x11",
        "#define ASENSE_CPU_FAN_ID 0x01",
        "#define ASENSE_GPU_FAN_ID 0x04",
        "#define ASENSE_SYSFS_FAN_MAXIMUM 0",
        "#define ASENSE_SYSFS_FAN_MANUAL 1",
        "#define ASENSE_SYSFS_FAN_AUTOMATIC 2",
    ] {
        assert!(
            DRIVER.contains(definition),
            "missing fan contract: {definition}"
        );
    }

    let read_mode = body("asense_read_fan_mode", "asense_write_fan_mode");
    assert!(read_mode.contains("ASENSE_FAN_BEHAVIOR_STATUS_MASK"));
    assert!(read_mode.contains("value < ASENSE_FAN_MODE_AUTO || value > ASENSE_FAN_MODE_MANUAL"));

    let read_speed = body("asense_read_fan_speed", "asense_write_fan_speed");
    assert!(read_speed.contains("ASENSE_FAN_SPEED_STATUS_MASK"));
    assert!(read_speed.contains("if (value > 100) return -EPROTO"));

    let mode_store = body("asense_fan_mode_store", "asense_fan_speed_show");
    assert!(mode_store.contains("asense_write_fan_mode(rgb, fan_bitmap, mode)"));
    assert!(mode_store.contains("asense_read_fan_mode(rgb, fan_bitmap, &actual)"));
    assert!(mode_store.contains("asense_write_fan_mode(rgb, fan_bitmap, previous)"));

    let speed_visibility = body("asense_fan_is_visible", "asense_fan_group");
    assert!(speed_visibility.contains("!rgb->fan_behavior_available"));
    assert!(speed_visibility.contains("!rgb->fan_speed_available"));
    assert!(DRIVER.contains(".name = \"gaming_fan\""));
}

#[test]
fn gaming_profile_surface_maps_only_known_values_with_readback() {
    for definition in [
        "#define ASENSE_MISC_SET 0x16",
        "#define ASENSE_MISC_GET 0x17",
        "#define ASENSE_PLATFORM_PROFILE_INDEX 0x0b",
        "{ ASENSE_PROFILE_LOW_POWER, \"low-power\" }",
        "{ ASENSE_PROFILE_QUIET, \"quiet\" }",
        "{ ASENSE_PROFILE_BALANCED, \"balanced\" }",
        "{ ASENSE_PROFILE_BALANCED_PERFORMANCE, \"balanced-performance\" }",
        "{ ASENSE_PROFILE_PERFORMANCE, \"performance\" }",
    ] {
        assert!(
            DRIVER.contains(definition),
            "missing profile contract: {definition}"
        );
    }
    let probe = body("asense_probe_gaming", "asense_probe_battery");
    assert!(
        probe.contains(
            "if (!error && asense_profile_by_value(profile)) rgb->profile_available = true"
        )
    );

    let store = body("profile_store", "choices_show");
    assert!(store.contains("choice = asense_profile_by_name(buffer)"));
    assert!(store.contains("error = asense_write_profile(rgb, choice->value)"));
    assert!(store.contains("error = asense_read_profile(rgb, &actual)"));
    assert!(store.contains("asense_write_profile(rgb, previous)"));
    assert!(DRIVER.contains(".name = \"gaming_profile\""));
}

#[test]
fn gaming_response_parser_accepts_defined_prefixes_and_rejects_short_shapes() {
    let parser = body("asense_gaming_call", "asense_gaming_get");
    assert!(parser.contains("object->type == ACPI_TYPE_INTEGER"));
    assert!(parser.contains("require_u64 && object->buffer.length >= sizeof(value64)"));
    assert!(parser.contains("!require_u64 && object->buffer.length >= sizeof(value64)"));
    assert!(parser.contains("!require_u64 && object->buffer.length >= sizeof(value32)"));
    assert!(parser.contains("memcpy(&value64, object->buffer.pointer, sizeof(value64))"));
    assert!(parser.contains("value = le64_to_cpu(value64)"));
    assert!(parser.contains("memcpy(&value32, object->buffer.pointer, sizeof(value32))"));
    assert!(parser.contains("value = le32_to_cpu(value32)"));
    assert!(parser.contains("error = -EPROTO"));
    assert!(!parser.contains("object->buffer.length =="));
}

#[test]
fn zoned_rgb_supports_bounded_one_to_four_zone_topologies() {
    let count = body("asense_zone_count", "asense_select_zone_config");
    for mapping in [
        "case 0x01: return 1",
        "case 0x03: return 2",
        "case 0x07: return 3",
        "case 0x0f: return 4",
    ] {
        assert!(count.contains(mapping), "missing zone mask: {mapping}");
    }

    assert!(DRIVER.contains(".product = \"Predator PHN16-72\""));
    assert!(DRIVER.contains(".flags = ASENSE_ZONE_FLAG_PREAMBLE"));
    assert!(DRIVER.contains(".product = \"Predator PHN14-51\""));
    let quirks = function_between(
        "static const struct asense_zoned_quirk asense_zoned_quirks[]",
        "static bool asense_input_consumed",
    );
    let phn14 = &quirks[quirks.find("Predator PHN14-51").unwrap()..];
    assert!(!phn14.contains("ASENSE_ZONE_FLAG_PREAMBLE"));

    let read = body("asense_read_rgb_state", "asense_read_zones");
    assert!(read.contains("for (zone = 0; zone < count; zone++)"));
    assert!(DRIVER.contains("static DEVICE_ATTR_RO(zone_mask)"));
}

#[test]
fn kernel_surface_never_exposes_raw_wmi_method_dispatch() {
    for forbidden in [
        "DEVICE_ATTR_ADMIN_RW(method)",
        "DEVICE_ATTR_ADMIN_WO(call)",
        "raw_method",
        "raw_payload",
    ] {
        assert!(
            !DRIVER.contains(forbidden),
            "raw surface leaked: {forbidden}"
        );
    }
    assert!(
        DRIVER.contains("WMI core prevents a second WMI driver from binding the same endpoint")
    );
    assert!(!DRIVER.contains("status = wmi_evaluate_method("));
    assert!(DRIVER.contains("status = wmidev_evaluate_method(rgb->wdev"));
}

#[test]
fn verified_optional_controls_decode_phn16_72_v118_responses() {
    let battery = body("asense_read_battery", "asense_write_battery");
    for required in [
        "state->limit_supported = result[0] & ASENSE_BATTERY_LIMIT",
        "state->calibration_supported = result[0] & ASENSE_BATTERY_CALIBRATION",
        "state->limit = result[3] != 0",
        "state->calibration = result[4] != 0",
    ] {
        assert!(
            battery.contains(required),
            "missing battery decoder: {required}"
        );
    }
    assert!(!battery.contains("result[1] != 0"));
    assert!(!battery.contains("result[2] != 0"));

    let timeout = body("asense_read_timeout", "asense_write_timeout");
    assert!(timeout.contains("result == ASENSE_TIMEOUT_UNINITIALIZED"));
    assert!(timeout.contains("result == ASENSE_TIMEOUT_OFF"));
    assert!(timeout.contains("result == ASENSE_TIMEOUT_ON"));

    let lcd = body("asense_read_lcd", "asense_write_lcd");
    assert!(lcd.contains("if (!(result & ASENSE_LCD_STATE_VALID)) return -EPROTO"));
    assert!(lcd.contains("*enabled = result & ASENSE_LCD_STATE_ENABLED"));
    assert!(DRIVER.contains("#define ASENSE_LCD_STATE_ENABLED BIT_ULL(48)"));
}

#[test]
fn verified_optional_controls_remain_visible_across_early_boot_probe_failures() {
    let visibility = body("asense_rgb_is_visible", "asense_rgb_group");
    for attribute in [
        "dev_attr_battery_limit",
        "dev_attr_battery_calibration",
        "dev_attr_keyboard_timeout",
    ] {
        assert!(
            !visibility.contains(attribute),
            "independent endpoint must not be coupled to RGB visibility: {attribute}"
        );
    }

    let gaming_probe = body("asense_probe_gaming", "asense_probe_battery");
    assert!(gaming_probe.contains(
        "rgb->lcd_available = !asense_read_lcd(rgb, &enabled) || asense_reference_model()"
    ));
    let battery_probe = body("asense_probe_battery", "asense_probe_apge");
    assert!(battery_probe.contains("if (!asense_reference_model()) return -ENODEV"));
    assert!(battery_probe.contains("rgb->battery_limit_available = true"));
    assert!(battery_probe.contains("rgb->battery_calibration_available = true"));

    let battery_limit = body("battery_limit_show", "battery_limit_store");
    assert!(battery_limit.contains("if (!error && !state.limit_supported) error = -EOPNOTSUPP"));

    let calibration = body("battery_calibration_show", "battery_calibration_store");
    assert!(
        calibration.contains("if (!error && !state.calibration_supported) error = -EOPNOTSUPP")
    );
}

#[test]
fn static_power_reenable_reuses_the_full_transaction() {
    let body = body("power_store", "battery_limit_show");
    assert!(!body.contains("asense_write_zone("));
    assert!(body.contains(
        "if (!error && enabled && requested.mode == 0) { \
         requested = asense_static_effect(requested.brightness); \
         requested_zones = previous_zones; \
         requested_zones.brightness = requested.brightness; \
         static_reenable = true; \
         error = asense_write_zones(rgb, &requested_zones); }"
    ));
    assert!(
        body.contains(
            "if (!error && !static_reenable) error = asense_write_effect(rgb, &requested);"
        )
    );
}

#[test]
fn static_rgb_payloads_are_byte_exact() {
    let static_effect = body("asense_static_effect", "asense_read_zone");
    assert!(static_effect.contains(
        "return (struct asense_effect) { .mode = 0, .brightness = brightness, \
         .engine = ASENSE_STATIC_ENGINE, };"
    ));

    let effect = body("asense_write_effect", "asense_engine_for_mode");
    assert!(effect.contains(
        "u8 payload[16] = { effect->mode, effect->speed, effect->brightness, 0, \
         effect->direction, effect->red, effect->green, effect->blue, \
         effect->engine, 1, 0, 0, 0, 0, 0, 0, };"
    ));

    let zone = body("asense_write_zone", "asense_read_zones");
    assert!(zone.contains("u8 payload[4] = { BIT(zone), color[0], color[1], color[2] };"));

    let readback = body("asense_read_effect", "asense_write_effect");
    assert!(readback.contains("u64 selector = 1;"));
    assert!(readback.contains("asense_wmi_call(rgb, ASENSE_EFFECT_GET, &selector"));
}

#[test]
fn rear_logo_drives_color_state_and_the_physical_power_gate() {
    let logo = body("asense_write_logo", "asense_restore_battery");
    assert!(logo.contains(
        "u8 payload[6] = { 1, logo->red, logo->green, logo->blue, \
         logo->brightness, logo->enabled, };"
    ));
    assert!(logo.contains("u8 gate[16] = { logo->enabled, 0, 0, 0, 0, 0, 0, 0, 0, 2 };"));
    let color = logo.find("asense_set_call(rgb, ASENSE_LOGO_SET").unwrap();
    let power = logo
        .find("asense_set_call(rgb, ASENSE_EFFECT_SET, gate")
        .unwrap();
    assert!(color < power);

    let store = body("rear_logo_store", "zone_mask_show");
    assert!(!store.contains("memcmp(&previous, &requested"));
    let calls = [
        "error = asense_read_logo(rgb, &previous)",
        "error = asense_write_logo(rgb, &requested)",
        "error = asense_read_logo(rgb, &actual)",
        "asense_restore_logo(rgb, &previous)",
    ]
    .map(|call| store.find(call).unwrap());
    assert!(calls.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn confirmed_rgb_state_is_cached_at_probe_and_after_mutations() {
    let probe = body("asense_probe_gaming", "asense_probe_battery");
    let probe_read = probe
        .find("error = asense_read_rgb_state(rgb, &effect, &zones);")
        .expect("probe must read the complete RGB state");
    let probe_validate = probe
        .find("if (!asense_effect_valid(&effect))")
        .expect("probe must reject an unsafe effect before caching it");
    let probe_cache = probe
        .find("asense_cache_rgb_state(rgb, &effect, &zones);")
        .expect("probe must cache the confirmed RGB state");
    assert!(probe_read < probe_validate && probe_validate < probe_cache);

    for (start, end) in [
        ("static ssize_t effect_store", "static ssize_t zones_show"),
        ("static ssize_t zones_store", "static ssize_t power_show"),
        (
            "static ssize_t power_store",
            "static ssize_t battery_limit_show",
        ),
    ] {
        let body = body(start, end);
        let readback = body
            .rfind("asense_read_rgb_state(")
            .unwrap_or_else(|| panic!("{start} must read back effect and zones"));
        let cache = body
            .find("asense_cache_rgb_state(")
            .unwrap_or_else(|| panic!("{start} must update the RGB cache"));
        let verification = body
            .rfind("error = -EIO;")
            .unwrap_or_else(|| panic!("{start} must reject mismatched readback"));
        assert!(
            readback < verification && verification < cache,
            "{start} must cache only after verified firmware readback"
        );
    }
}

#[test]
fn system_resume_replays_only_cached_rgb_with_readback() {
    let suspend = body("asense_rgb_suspend", "asense_rgb_resume");
    assert!(suspend.contains("mutex_lock(&rgb->lock); mutex_unlock(&rgb->lock);"));
    assert!(!suspend.contains("asense_write_"));

    let resume = body(
        "static int asense_rgb_resume",
        "static DEFINE_SIMPLE_DEV_PM_OPS",
    );
    for required in [
        "mutex_lock(&rgb->lock)",
        "if (!rgb->rgb_cache_valid) goto out",
        "error = asense_write_zones(rgb, &rgb->cached_zones)",
        "error = asense_write_effect(rgb, &rgb->cached_effect)",
        "error = asense_read_rgb_state(rgb, &actual_effect, &actual_zones)",
        "if (!error) asense_cache_rgb_state(rgb, &actual_effect, &actual_zones)",
        "mutex_unlock(&rgb->lock)",
        "dev_err(dev, \"keyboard RGB resume restore failed: %d\\n\", error)",
        "Keyboard lighting must never make the system resume fail",
        "return 0",
    ] {
        assert!(
            resume.contains(required),
            "missing resume invariant: {required}"
        );
    }
    for forbidden in [
        "asense_write_battery",
        "asense_write_usb",
        "asense_write_timeout",
        "asense_write_boot_sound",
        "asense_write_lcd",
        "asense_write_logo",
        "msleep(",
        "ssleep(",
        "usleep_range(",
        "return error",
    ] {
        assert!(
            !resume.contains(forbidden),
            "resume must not use {forbidden}"
        );
    }

    assert!(DRIVER.contains(
        "DEFINE_SIMPLE_DEV_PM_OPS(asense_rgb_pm_ops,\n\t\t\t\tasense_rgb_suspend, asense_rgb_resume)"
    ));
    assert!(DRIVER.contains(".pm = pm_sleep_ptr(&asense_rgb_pm_ops),"));
}
