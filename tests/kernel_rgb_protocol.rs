const DRIVER: &str = include_str!("../kernel/asense_rgb.c");

fn compact(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
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
        "asense_scalar_call(rgb, NULL, ASENSE_GAMING_SYS_INFO_GET, 0, &gaming_sys_info)",
        "asense_scalar_set(rgb, NULL, ASENSE_GAMING_LED_SET, ASENSE_FOUR_ZONE_ENABLE)",
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
    assert!(body.contains("for (zone = 0; zone < ARRAY_SIZE(zones->rgb); zone++)"));
}

#[test]
fn binding_is_model_exact_but_bios_agnostic() {
    assert!(DRIVER.contains("dmi_match(DMI_SYS_VENDOR, \"Acer\")"));
    assert!(DRIVER.contains("dmi_match(DMI_PRODUCT_NAME, \"Predator PHN16-72\")"));
    assert!(!DRIVER.contains("DMI_BIOS_VERSION"));
}

#[test]
fn verified_optional_controls_decode_phn16_72_v118_responses() {
    let battery = compact(function_between(
        "static int asense_read_battery",
        "static int asense_write_battery",
    ));
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

    let timeout = compact(function_between(
        "static int asense_read_timeout",
        "static int asense_write_timeout",
    ));
    assert!(timeout.contains("result == ASENSE_TIMEOUT_UNINITIALIZED"));
    assert!(timeout.contains("result == ASENSE_TIMEOUT_OFF"));
    assert!(timeout.contains("result == ASENSE_TIMEOUT_ON"));

    let lcd = compact(function_between(
        "static int asense_read_lcd",
        "static int asense_write_lcd",
    ));
    assert!(lcd.contains("if (!(result & ASENSE_LCD_STATE_VALID)) return -EPROTO"));
    assert!(lcd.contains("*enabled = result & ASENSE_LCD_STATE_ENABLED"));
    assert!(DRIVER.contains("#define ASENSE_LCD_STATE_ENABLED BIT_ULL(48)"));
}

#[test]
fn verified_optional_controls_remain_visible_across_early_boot_probe_failures() {
    let visibility = compact(function_between(
        "static umode_t asense_rgb_is_visible",
        "static const struct attribute_group asense_rgb_group",
    ));
    for attribute in [
        "dev_attr_battery_limit",
        "dev_attr_battery_calibration",
        "dev_attr_keyboard_timeout",
        "dev_attr_lcd_override",
    ] {
        assert!(
            !visibility.contains(attribute),
            "verified attribute must not be hidden by a one-shot probe: {attribute}"
        );
    }

    let battery_limit = compact(function_between(
        "static ssize_t battery_limit_show",
        "static ssize_t battery_limit_store",
    ));
    assert!(battery_limit.contains("if (!error && !state.limit_supported) error = -EOPNOTSUPP"));

    let calibration = compact(function_between(
        "static ssize_t battery_calibration_show",
        "static ssize_t battery_calibration_store",
    ));
    assert!(
        calibration.contains("if (!error && !state.calibration_supported) error = -EOPNOTSUPP")
    );
}

#[test]
fn static_power_reenable_reuses_the_full_transaction() {
    let body = compact(function_between(
        "static ssize_t power_store",
        "static ssize_t battery_limit_show",
    ));
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
    let static_effect = compact(function_between(
        "static struct asense_effect asense_static_effect",
        "static int asense_read_zone",
    ));
    assert!(static_effect.contains(
        "return (struct asense_effect) { .mode = 0, .brightness = brightness, \
         .engine = ASENSE_STATIC_ENGINE, };"
    ));

    let effect = compact(function_between(
        "static int asense_write_effect",
        "static u8 asense_engine_for_mode",
    ));
    assert!(effect.contains(
        "u8 payload[16] = { effect->mode, effect->speed, effect->brightness, 0, \
         effect->direction, effect->red, effect->green, effect->blue, \
         effect->engine, 1, 0, 0, 0, 0, 0, 0, };"
    ));

    let zone = compact(function_between(
        "static int asense_write_zone",
        "static int asense_read_zones",
    ));
    assert!(zone.contains("u8 payload[4] = { BIT(zone), color[0], color[1], color[2] };"));

    let readback = compact(function_between(
        "static int asense_read_effect",
        "static int asense_write_effect",
    ));
    assert!(readback.contains("u64 selector = 1;"));
    assert!(readback.contains("asense_wmi_call(rgb, ASENSE_EFFECT_GET, &selector"));
}

#[test]
fn confirmed_rgb_state_is_cached_at_probe_and_after_mutations() {
    let probe = compact(function_between(
        "static int asense_rgb_probe",
        "static const struct wmi_device_id asense_rgb_id_table",
    ));
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
        let body = compact(function_between(start, end));
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
    let suspend = compact(function_between(
        "static int asense_rgb_suspend",
        "static int asense_rgb_resume",
    ));
    assert!(suspend.contains("mutex_lock(&rgb->lock); mutex_unlock(&rgb->lock);"));
    assert!(!suspend.contains("asense_write_"));

    let resume = compact(function_between(
        "static int asense_rgb_resume",
        "static DEFINE_SIMPLE_DEV_PM_OPS",
    ));
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
