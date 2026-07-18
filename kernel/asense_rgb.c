// SPDX-License-Identifier: GPL-2.0-only
/*
 * Minimal PHN16-72 WMI transport. It binds the otherwise-unbound gaming WMI
 * method device and coexists with the in-tree acer_wmi driver.
 */

#include <linux/acpi.h>
#include <linux/ctype.h>
#include <linux/device.h>
#include <linux/dmi.h>
#include <linux/kernel.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/pm.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/wmi.h>

#define ASENSE_RGB_GUID "7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56"
#define ASENSE_APGE_GUID "61EF69EA-865C-4BC3-A502-A0DEBA0CB531"
#define ASENSE_BATTERY_GUID "79772EC5-04B1-4BFD-843C-61E7F77B6CC9"
#define ASENSE_FUNCTION_SET 0x01
#define ASENSE_FUNCTION_GET 0x02
#define ASENSE_PROFILE_SET 0x01
#define ASENSE_PROFILE_GET 0x03
#define ASENSE_GAMING_LED_SET 0x02
#define ASENSE_GAMING_SYS_INFO_GET 0x05
#define ASENSE_ZONE_SET 0x06
#define ASENSE_ZONE_GET 0x07
#define ASENSE_LOGO_SET 0x0c
#define ASENSE_LOGO_GET 0x0d
#define ASENSE_EFFECT_SET 0x14
#define ASENSE_EFFECT_GET 0x15
#define ASENSE_MISC_SET 0x16
#define ASENSE_MISC_GET 0x17
#define ASENSE_BATTERY_GET 0x14
#define ASENSE_BATTERY_SET 0x15
#define ASENSE_FOUR_ZONE_ENGINE 0x03
#define ASENSE_STATIC_ENGINE 0x00
#define ASENSE_FOUR_ZONE_ENABLE 0x00000f0000000008ULL

#define ASENSE_USB_GET 0x4ULL
#define ASENSE_USB_OFF 0x0a1f00ULL
#define ASENSE_USB_10 0x0a0f00ULL
#define ASENSE_USB_20 0x140f00ULL
#define ASENSE_USB_30 0x1e0f00ULL
#define ASENSE_USB_SET_COMMAND 0x4ULL

#define ASENSE_TIMEOUT_GET 0x88401ULL
#define ASENSE_TIMEOUT_UNINITIALIZED 0x0ULL
#define ASENSE_TIMEOUT_OFF 0x80000ULL
#define ASENSE_TIMEOUT_ON 0x1e0000080000ULL
#define ASENSE_TIMEOUT_SET_OFF 0x88402ULL
#define ASENSE_TIMEOUT_SET_ON 0x1e0000088402ULL

#define ASENSE_BOOT_SOUND_SELECTOR 0x6ULL
#define ASENSE_BOOT_SOUND_OFF 0x0ULL
#define ASENSE_BOOT_SOUND_ON 0x100ULL
#define ASENSE_BOOT_SOUND_SET_OFF 0x6ULL
#define ASENSE_BOOT_SOUND_SET_ON 0x106ULL

#define ASENSE_LCD_SELECTOR 0x0ULL
#define ASENSE_LCD_STATE_VALID BIT_ULL(24)
#define ASENSE_LCD_STATE_ENABLED BIT_ULL(48)
#define ASENSE_LCD_SET_OFF 0x10ULL
#define ASENSE_LCD_SET_ON 0x1000000000010ULL

#define ASENSE_BATTERY_LIMIT BIT(0)
#define ASENSE_BATTERY_CALIBRATION BIT(1)

struct asense_effect {
	u8 mode;
	u8 speed;
	u8 brightness;
	u8 direction;
	u8 red;
	u8 green;
	u8 blue;
	u8 engine;
};

struct asense_zones {
	u8 rgb[4][3];
	u8 brightness;
};

struct asense_rgb {
	struct wmi_device *wdev;
	struct mutex lock;
	struct asense_effect cached_effect;
	struct asense_zones cached_zones;
	u8 last_nonzero_brightness;
	bool rgb_cache_valid;
	bool usb_available;
	bool boot_sound_available;
	bool logo_available;
};

struct asense_battery_state {
	bool limit_supported;
	bool calibration_supported;
	bool limit;
	bool calibration;
};

struct asense_logo {
	u8 red;
	u8 green;
	u8 blue;
	u8 brightness;
	bool enabled;
};

static bool asense_input_consumed(const char *buffer, size_t count, int offset)
{
	while (offset < count && isspace(buffer[offset]))
		offset++;
	return offset == count;
}

static int asense_wmi_call(struct asense_rgb *rgb, u32 method,
			   const void *payload, size_t payload_len,
			   u8 *result, size_t expected_len)
{
	struct acpi_buffer input = {
		.length = payload_len,
		.pointer = (void *)payload,
	};
	struct acpi_buffer output = { ACPI_ALLOCATE_BUFFER, NULL };
	union acpi_object *object;
	acpi_status status;
	int error = 0;

	status = wmidev_evaluate_method(rgb->wdev, 0, method, &input, &output);
	if (ACPI_FAILURE(status))
		return -EIO;

	object = output.pointer;
	if (!object || object->type != ACPI_TYPE_BUFFER ||
	    object->buffer.length != expected_len ||
	    (expected_len && !object->buffer.pointer)) {
		error = -EPROTO;
		goto out;
	}
	if (result)
		memcpy(result, object->buffer.pointer, expected_len);
out:
	kfree(object);
	return error;
}

static int asense_set_call(struct asense_rgb *rgb, u32 method,
			   const void *payload, size_t payload_len)
{
	u8 status[4];
	int error;

	error = asense_wmi_call(rgb, method, payload, payload_len,
				status, sizeof(status));
	if (error)
		return error;
	return status[0] == 0 ? 0 : -EREMOTEIO;
}

static int asense_legacy_wmi_call(const char *guid, u32 method,
				  const void *payload, size_t payload_len,
				  u8 *result, size_t expected_len)
{
	struct acpi_buffer input = {
		.length = payload_len,
		.pointer = (void *)payload,
	};
	struct acpi_buffer output = { ACPI_ALLOCATE_BUFFER, NULL };
	union acpi_object *object;
	acpi_status status;
	int error = 0;

	status = wmi_evaluate_method(guid, 0, method, &input, &output);
	if (ACPI_FAILURE(status))
		return -EIO;

	object = output.pointer;
	if (!object || object->type != ACPI_TYPE_BUFFER ||
	    object->buffer.length < expected_len ||
	    (expected_len && !object->buffer.pointer)) {
		error = -EPROTO;
		goto out;
	}
	memcpy(result, object->buffer.pointer, expected_len);
out:
	kfree(object);
	return error;
}

/*
 * These firmware methods return either an ACPI integer or a little-endian
 * four/eight-byte buffer. Accept only those documented shapes; callers still
 * validate the exact command-specific value.
 */
static int asense_scalar_call(struct asense_rgb *rgb, const char *guid,
			      u32 method, u64 payload, u64 *result)
{
	struct acpi_buffer input = {
		.length = sizeof(payload),
		.pointer = &payload,
	};
	struct acpi_buffer output = { ACPI_ALLOCATE_BUFFER, NULL };
	union acpi_object *object;
	acpi_status status;
	u32 value32;
	u64 value = 0;
	int error = 0;

	if (guid)
		status = wmi_evaluate_method(guid, 0, method, &input, &output);
	else
		status = wmidev_evaluate_method(rgb->wdev, 0, method,
						&input, &output);
	if (ACPI_FAILURE(status))
		return -EIO;

	object = output.pointer;
	if (!object) {
		error = -EPROTO;
		goto out;
	}
	if (object->type == ACPI_TYPE_INTEGER) {
		value = object->integer.value;
	} else if (object->type == ACPI_TYPE_BUFFER &&
		   object->buffer.pointer && object->buffer.length == sizeof(value32)) {
		memcpy(&value32, object->buffer.pointer, sizeof(value32));
		value = value32;
	} else if (object->type == ACPI_TYPE_BUFFER &&
		   object->buffer.pointer && object->buffer.length == sizeof(value)) {
		memcpy(&value, object->buffer.pointer, sizeof(value));
	} else {
		error = -EPROTO;
		goto out;
	}
	*result = value;
out:
	kfree(object);
	return error;
}

static int asense_scalar_set(struct asense_rgb *rgb, const char *guid,
			     u32 method, u64 payload)
{
	u64 status;
	int error;

	error = asense_scalar_call(rgb, guid, method, payload, &status);
	if (error)
		return error;
	return status == 0 ? 0 : -EREMOTEIO;
}

static int asense_read_battery(struct asense_battery_state *state)
{
	u8 input[4] = { 1, 1, 0, 0 };
	u8 result[8];
	int error;

	error = asense_legacy_wmi_call(ASENSE_BATTERY_GUID,
				       ASENSE_BATTERY_GET, input, sizeof(input),
				       result, sizeof(result));
	if (error)
		return error;
	/*
	 * Byte zero is a capability bitmap. Bytes three and four are the
	 * corresponding health-limit and calibration states. Acer firmware uses
	 * nonzero status values, not necessarily one, for enabled states; the
	 * two reserved return bytes are not an operation status. This matches the
	 * upstream acer-wmi-battery protocol and the PHN16-72 V1.18 readback.
	 */
	state->limit_supported = result[0] & ASENSE_BATTERY_LIMIT;
	state->calibration_supported = result[0] & ASENSE_BATTERY_CALIBRATION;
	state->limit = result[3] != 0;
	state->calibration = result[4] != 0;
	return 0;
}

static int asense_write_battery(u8 function, bool enabled)
{
	u8 input[8] = { 1, function, enabled, 0, 0, 0, 0, 0 };
	u8 result[4];
	int error;

	if (function != ASENSE_BATTERY_LIMIT &&
	    function != ASENSE_BATTERY_CALIBRATION)
		return -EINVAL;
	error = asense_legacy_wmi_call(ASENSE_BATTERY_GUID,
				       ASENSE_BATTERY_SET, input, sizeof(input),
				       result, sizeof(result));
	if (error)
		return error;
	/* BESB byte zero is the firmware return status; remaining bytes are reserved. */
	return result[0] == 0 ? 0 : -EREMOTEIO;
}

static int asense_read_usb(struct asense_rgb *rgb, u8 *threshold)
{
	u64 result;
	int error;

	error = asense_scalar_call(rgb, ASENSE_APGE_GUID, ASENSE_FUNCTION_GET,
				   ASENSE_USB_GET, &result);
	if (error)
		return error;
	switch (result) {
	case ASENSE_USB_OFF:
		*threshold = 0;
		break;
	case ASENSE_USB_10:
		*threshold = 10;
		break;
	case ASENSE_USB_20:
		*threshold = 20;
		break;
	case ASENSE_USB_30:
		*threshold = 30;
		break;
	default:
		return -EPROTO;
	}
	return 0;
}

static int asense_write_usb(struct asense_rgb *rgb, u8 threshold)
{
	u64 payload;

	switch (threshold) {
	case 0:
		payload = ASENSE_USB_OFF | ASENSE_USB_SET_COMMAND;
		break;
	case 10:
		payload = ASENSE_USB_10 | ASENSE_USB_SET_COMMAND;
		break;
	case 20:
		payload = ASENSE_USB_20 | ASENSE_USB_SET_COMMAND;
		break;
	case 30:
		payload = ASENSE_USB_30 | ASENSE_USB_SET_COMMAND;
		break;
	default:
		return -EINVAL;
	}
	return asense_scalar_set(rgb, ASENSE_APGE_GUID,
				 ASENSE_FUNCTION_SET, payload);
}

static int asense_read_timeout(struct asense_rgb *rgb, bool *enabled)
{
	u64 result;
	int error;

	error = asense_scalar_call(rgb, ASENSE_APGE_GUID, ASENSE_FUNCTION_GET,
				   ASENSE_TIMEOUT_GET, &result);
	if (error)
		return error;
	/* V1.18 returns zero until the setting has first been initialized. */
	if (result == ASENSE_TIMEOUT_UNINITIALIZED ||
	    result == ASENSE_TIMEOUT_OFF)
		*enabled = false;
	else if (result == ASENSE_TIMEOUT_ON)
		*enabled = true;
	else
		return -EPROTO;
	return 0;
}

static int asense_write_timeout(struct asense_rgb *rgb, bool enabled)
{
	return asense_scalar_set(rgb, ASENSE_APGE_GUID, ASENSE_FUNCTION_SET,
				 enabled ? ASENSE_TIMEOUT_SET_ON :
				 ASENSE_TIMEOUT_SET_OFF);
}

static int asense_read_boot_sound(struct asense_rgb *rgb, bool *enabled)
{
	u64 result;
	int error;

	error = asense_scalar_call(rgb, NULL, ASENSE_MISC_GET,
				   ASENSE_BOOT_SOUND_SELECTOR, &result);
	if (error)
		return error;
	if (result == ASENSE_BOOT_SOUND_OFF)
		*enabled = false;
	else if (result == ASENSE_BOOT_SOUND_ON)
		*enabled = true;
	else
		return -EPROTO;
	return 0;
}

static int asense_write_boot_sound(struct asense_rgb *rgb, bool enabled)
{
	return asense_scalar_set(rgb, NULL, ASENSE_MISC_SET,
				 enabled ? ASENSE_BOOT_SOUND_SET_ON :
				 ASENSE_BOOT_SOUND_SET_OFF);
}

static int asense_read_lcd(struct asense_rgb *rgb, bool *enabled)
{
	u64 result;
	int error;

	error = asense_scalar_call(rgb, NULL, ASENSE_PROFILE_GET,
				   ASENSE_LCD_SELECTOR, &result);
	if (error)
		return error;
	/*
	 * Older firmware returned only the LCD fields. V1.18 returns a packed
	 * profile status (for example 0x0001ff000101ff00); bit 24 marks the LCD
	 * field valid and bit 48 is its state. Ignore unrelated packed fields.
	 */
	if (!(result & ASENSE_LCD_STATE_VALID))
		return -EPROTO;
	*enabled = result & ASENSE_LCD_STATE_ENABLED;
	return 0;
}

static int asense_write_lcd(struct asense_rgb *rgb, bool enabled)
{
	return asense_scalar_set(rgb, NULL, ASENSE_PROFILE_SET,
				 enabled ? ASENSE_LCD_SET_ON : ASENSE_LCD_SET_OFF);
}

static int asense_read_logo(struct asense_rgb *rgb, struct asense_logo *logo)
{
	u64 selector = 1;
	u8 result[8];
	int error;

	error = asense_wmi_call(rgb, ASENSE_LOGO_GET, &selector,
				sizeof(selector), result, sizeof(result));
	if (error)
		return error;
	if (result[0] != 0 || result[4] > 100 || result[5] > 1)
		return result[0] ? -EREMOTEIO : -EPROTO;
	logo->red = result[1];
	logo->green = result[2];
	logo->blue = result[3];
	logo->brightness = result[4];
	logo->enabled = result[5];
	return 0;
}

static int asense_write_logo(struct asense_rgb *rgb,
			     const struct asense_logo *logo)
{
	u8 payload[6] = {
		1, logo->red, logo->green, logo->blue,
		logo->brightness, logo->enabled,
	};

	return asense_set_call(rgb, ASENSE_LOGO_SET,
			       payload, sizeof(payload));
}

static int asense_restore_battery(u8 function, bool expected)
{
	struct asense_battery_state actual;
	int error;

	error = asense_write_battery(function, expected);
	if (!error)
		error = asense_read_battery(&actual);
	if (!error && ((function == ASENSE_BATTERY_LIMIT &&
			 actual.limit != expected) ||
			(function == ASENSE_BATTERY_CALIBRATION &&
			 actual.calibration != expected)))
		error = -EIO;
	return error;
}

static int asense_restore_usb(struct asense_rgb *rgb, u8 expected)
{
	u8 actual;
	int error;

	error = asense_write_usb(rgb, expected);
	if (!error)
		error = asense_read_usb(rgb, &actual);
	if (!error && actual != expected)
		error = -EIO;
	return error;
}

typedef int (*asense_read_bool_fn)(struct asense_rgb *rgb, bool *enabled);
typedef int (*asense_write_bool_fn)(struct asense_rgb *rgb, bool enabled);

static int asense_restore_bool(struct asense_rgb *rgb,
			       asense_read_bool_fn read,
			       asense_write_bool_fn write, bool expected)
{
	bool actual;
	int error;

	error = write(rgb, expected);
	if (!error)
		error = read(rgb, &actual);
	if (!error && actual != expected)
		error = -EIO;
	return error;
}

static int asense_restore_logo(struct asense_rgb *rgb,
			       const struct asense_logo *expected)
{
	struct asense_logo actual;
	int error;

	error = asense_write_logo(rgb, expected);
	if (!error)
		error = asense_read_logo(rgb, &actual);
	if (!error && memcmp(&actual, expected, sizeof(actual)) != 0)
		error = -EIO;
	return error;
}

static int asense_read_effect(struct asense_rgb *rgb,
			      struct asense_effect *effect)
{
	u64 selector = 1;
	u8 result[16];
	int error;

	error = asense_wmi_call(rgb, ASENSE_EFFECT_GET, &selector,
				sizeof(selector), result, sizeof(result));
	if (error)
		return error;
	if (result[0] != 0)
		return -EREMOTEIO;

	effect->mode = result[1];
	effect->speed = result[2];
	effect->brightness = result[3];
	effect->direction = result[5];
	effect->red = result[6];
	effect->green = result[7];
	effect->blue = result[8];
	effect->engine = result[9];
	return 0;
}

static int asense_write_effect(struct asense_rgb *rgb,
			       const struct asense_effect *effect)
{
	u8 payload[16] = {
		effect->mode, effect->speed, effect->brightness, 0,
		effect->direction, effect->red, effect->green, effect->blue,
		effect->engine, 1, 0, 0, 0, 0, 0, 0,
	};

	return asense_set_call(rgb, ASENSE_EFFECT_SET, payload, sizeof(payload));
}

static u8 asense_engine_for_mode(u8 mode)
{
	return mode == 0 ? ASENSE_STATIC_ENGINE : ASENSE_FOUR_ZONE_ENGINE;
}

static struct asense_effect asense_static_effect(u8 brightness)
{
	return (struct asense_effect) {
		.mode = 0,
		.brightness = brightness,
		.engine = ASENSE_STATIC_ENGINE,
	};
}

static int asense_read_zone(struct asense_rgb *rgb, unsigned int zone,
			    u8 color[3])
{
	u64 selector = BIT(zone);
	u8 result[8];
	int error;

	error = asense_wmi_call(rgb, ASENSE_ZONE_GET, &selector,
				sizeof(selector), result, sizeof(result));
	if (error)
		return error;
	if (result[0] != 0)
		return -EREMOTEIO;
	color[0] = result[1];
	color[1] = result[2];
	color[2] = result[3];
	return 0;
}

static int asense_write_zone(struct asense_rgb *rgb, unsigned int zone,
			     const u8 color[3])
{
	u8 payload[4] = { BIT(zone), color[0], color[1], color[2] };

	return asense_set_call(rgb, ASENSE_ZONE_SET, payload, sizeof(payload));
}

static int asense_read_rgb_state(struct asense_rgb *rgb,
				 struct asense_effect *effect,
				 struct asense_zones *zones)
{
	unsigned int zone;
	int error;

	error = asense_read_effect(rgb, effect);
	if (error)
		return error;
	for (zone = 0; zone < ARRAY_SIZE(zones->rgb); zone++) {
		error = asense_read_zone(rgb, zone, zones->rgb[zone]);
		if (error)
			return error;
	}
	zones->brightness = effect->brightness;
	return 0;
}

static int asense_read_zones(struct asense_rgb *rgb,
			     struct asense_zones *zones)
{
	struct asense_effect effect;

	return asense_read_rgb_state(rgb, &effect, zones);
}

static bool asense_effect_valid(const struct asense_effect *effect)
{
	if (effect->mode > 7 || effect->speed > 9 ||
	    effect->brightness > 100 || effect->direction > 2)
		return false;
	if (effect->mode <= 1 && (effect->speed || effect->direction))
		return false;
	if (effect->mode == 2 && effect->direction)
		return false;
	if ((effect->mode == 3 || effect->mode == 4) &&
	    (effect->direction < 1 || effect->direction > 2))
		return false;
	if (effect->mode >= 5 && effect->direction)
		return false;
	if (effect->engine != asense_engine_for_mode(effect->mode))
		return false;
	return true;
}

static void asense_cache_rgb_state(struct asense_rgb *rgb,
				   const struct asense_effect *effect,
				   const struct asense_zones *zones)
{
	rgb->cached_effect = *effect;
	rgb->cached_zones = *zones;
	rgb->rgb_cache_valid = true;
}

static ssize_t effect_show(struct device *dev, struct device_attribute *attr,
			   char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_effect effect;
	int error;
	ssize_t length;

	mutex_lock(&rgb->lock);
	error = asense_read_effect(rgb, &effect);
	if (error)
		length = error;
	else
		length = sysfs_emit(buffer, "%u,%u,%u,%u,%u,%u,%u\n",
				    effect.mode, effect.speed, effect.brightness,
				    effect.direction, effect.red, effect.green,
				    effect.blue);
	mutex_unlock(&rgb->lock);
	return length;
}

static ssize_t effect_store(struct device *dev, struct device_attribute *attr,
			    const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_effect requested, previous, actual;
	struct asense_zones actual_zones;
	unsigned int values[7];
	bool previous_valid = false;
	int offset = 0;
	int error;

	if (sscanf(buffer, "%u,%u,%u,%u,%u,%u,%u%n",
		   &values[0], &values[1], &values[2], &values[3],
		   &values[4], &values[5], &values[6], &offset) != ARRAY_SIZE(values) ||
	    !asense_input_consumed(buffer, count, offset))
		return -EINVAL;
	if (values[0] > U8_MAX || values[1] > U8_MAX ||
	    values[2] > U8_MAX || values[3] > U8_MAX ||
	    values[4] > U8_MAX || values[5] > U8_MAX || values[6] > U8_MAX)
		return -ERANGE;
	requested = (struct asense_effect) {
		.mode = values[0], .speed = values[1],
		.brightness = values[2], .direction = values[3],
		.red = values[4], .green = values[5], .blue = values[6],
		.engine = asense_engine_for_mode(values[0]),
	};
	if (!asense_effect_valid(&requested))
		return -EINVAL;

	mutex_lock(&rgb->lock);
	error = asense_read_effect(rgb, &previous);
	previous_valid = !error;
	if (!error)
		error = asense_write_effect(rgb, &requested);
	if (!error)
		error = asense_read_rgb_state(rgb, &actual, &actual_zones);
	if (!error && memcmp(&requested, &actual, sizeof(requested)) != 0)
		error = -EIO;
	if (!error)
		asense_cache_rgb_state(rgb, &actual, &actual_zones);
	if (!error && requested.brightness)
		rgb->last_nonzero_brightness = requested.brightness;
	if (error && previous_valid && asense_write_effect(rgb, &previous))
		dev_err(dev, "keyboard effect rollback failed\n");
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t zones_show(struct device *dev, struct device_attribute *attr,
			  char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_zones zones;
	int error;
	ssize_t length;

	mutex_lock(&rgb->lock);
	error = asense_read_zones(rgb, &zones);
	if (error)
		length = error;
	else
		length = sysfs_emit(buffer,
			"%02x%02x%02x,%02x%02x%02x,%02x%02x%02x,%02x%02x%02x,%u\n",
			zones.rgb[0][0], zones.rgb[0][1], zones.rgb[0][2],
			zones.rgb[1][0], zones.rgb[1][1], zones.rgb[1][2],
			zones.rgb[2][0], zones.rgb[2][1], zones.rgb[2][2],
			zones.rgb[3][0], zones.rgb[3][1], zones.rgb[3][2],
			zones.brightness);
	mutex_unlock(&rgb->lock);
	return length;
}

static int asense_write_zones(struct asense_rgb *rgb,
			      const struct asense_zones *zones)
{
	struct asense_effect static_mode = asense_static_effect(zones->brightness);
	struct asense_effect actual;
	u64 gaming_sys_info;
	unsigned int zone;
	int error;

	/*
	 * Acer's four-zone static transaction has a required scalar preamble.
	 * Method 5 polls the gaming controller; method 2 then enables all four
	 * zones.  Both inputs are eight-byte little-endian scalar payloads.
	 */
	error = asense_scalar_call(rgb, NULL, ASENSE_GAMING_SYS_INFO_GET,
				   0, &gaming_sys_info);
	if (error)
		return error;
	error = asense_scalar_set(rgb, NULL, ASENSE_GAMING_LED_SET,
				  ASENSE_FOUR_ZONE_ENABLE);
	if (error)
		return error;

	for (zone = 0; zone < ARRAY_SIZE(zones->rgb); zone++) {
		error = asense_write_zone(rgb, zone, zones->rgb[zone]);
		if (error)
			return error;
	}
	/* Method 20 commits static mode; method 21 must verify that commit. */
	error = asense_write_effect(rgb, &static_mode);
	if (!error)
		error = asense_read_effect(rgb, &actual);
	if (!error && memcmp(&static_mode, &actual, sizeof(static_mode)) != 0)
		error = -EIO;
	return error;
}

static ssize_t zones_store(struct device *dev, struct device_attribute *attr,
			   const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_zones requested, previous, actual;
	struct asense_effect previous_effect, actual_effect;
	struct asense_effect expected_effect;
	unsigned int color[4], brightness, zone;
	bool previous_valid = false;
	int offset = 0;
	int rollback_effect;
	int rollback_zones;
	int error;

	if (sscanf(buffer, "%06x,%06x,%06x,%06x,%u%n",
		   &color[0], &color[1], &color[2], &color[3],
		   &brightness, &offset) != 5 ||
	    !asense_input_consumed(buffer, count, offset))
		return -EINVAL;
	if (brightness > 100)
		return -ERANGE;
	for (zone = 0; zone < ARRAY_SIZE(requested.rgb); zone++) {
		if (color[zone] > 0xFFFFFF)
			return -ERANGE;
		requested.rgb[zone][0] = color[zone] >> 16;
		requested.rgb[zone][1] = color[zone] >> 8;
		requested.rgb[zone][2] = color[zone];
	}
	requested.brightness = brightness;
	expected_effect = asense_static_effect(requested.brightness);

	mutex_lock(&rgb->lock);
	error = asense_read_rgb_state(rgb, &previous_effect, &previous);
	previous_valid = !error;
	if (!error)
		error = asense_write_zones(rgb, &requested);
	if (!error)
		error = asense_read_rgb_state(rgb, &actual_effect, &actual);
	if (!error && memcmp(&requested, &actual, sizeof(requested)) != 0)
		error = -EIO;
	if (!error && memcmp(&expected_effect, &actual_effect,
			     sizeof(expected_effect)) != 0)
		error = -EIO;
	if (!error)
		asense_cache_rgb_state(rgb, &actual_effect, &actual);
	if (!error && requested.brightness)
		rgb->last_nonzero_brightness = requested.brightness;
	if (error && previous_valid) {
		rollback_zones = asense_write_zones(rgb, &previous);
		rollback_effect = asense_write_effect(rgb, &previous_effect);
		if (rollback_zones || rollback_effect)
			dev_err(dev, "keyboard zone rollback failed\n");
	}
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t power_show(struct device *dev, struct device_attribute *attr,
			  char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_effect effect;
	int error;
	ssize_t length;

	mutex_lock(&rgb->lock);
	error = asense_read_effect(rgb, &effect);
	if (error)
		length = error;
	else
		length = sysfs_emit(buffer, "%u\n", effect.brightness != 0);
	mutex_unlock(&rgb->lock);
	return length;
}

static ssize_t power_store(struct device *dev, struct device_attribute *attr,
			   const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_effect previous, requested, actual;
	struct asense_zones previous_zones, requested_zones, actual_zones;
	bool previous_valid = false;
	bool static_reenable = false;
	bool enabled;
	int rollback_effect;
	int rollback_zones;
	int error;

	error = kstrtobool(buffer, &enabled);
	if (error)
		return error;

	mutex_lock(&rgb->lock);
	error = asense_read_rgb_state(rgb, &previous, &previous_zones);
	previous_valid = !error;
	if (!error) {
		requested = previous;
		requested.engine = asense_engine_for_mode(requested.mode);
		if (enabled) {
			if (!requested.brightness)
				requested.brightness = rgb->last_nonzero_brightness;
		} else {
			if (requested.brightness)
				rgb->last_nonzero_brightness = requested.brightness;
			requested.brightness = 0;
		}
	}
	if (!error && enabled && requested.mode == 0) {
		requested = asense_static_effect(requested.brightness);
		requested_zones = previous_zones;
		requested_zones.brightness = requested.brightness;
		static_reenable = true;
		error = asense_write_zones(rgb, &requested_zones);
	}
	if (!error && !static_reenable)
		error = asense_write_effect(rgb, &requested);
	if (!error)
		error = asense_read_rgb_state(rgb, &actual, &actual_zones);
	if (!error && memcmp(&requested, &actual, sizeof(requested)) != 0)
		error = -EIO;
	if (!error &&
	    (memcmp(previous_zones.rgb, actual_zones.rgb,
		    sizeof(previous_zones.rgb)) != 0 ||
	     actual_zones.brightness != requested.brightness))
		error = -EIO;
	if (!error)
		asense_cache_rgb_state(rgb, &actual, &actual_zones);
	if (error && previous_valid) {
		rollback_zones = asense_write_zones(rgb, &previous_zones);
		rollback_effect = asense_write_effect(rgb, &previous);
		if (rollback_zones || rollback_effect)
			dev_err(dev, "keyboard power rollback failed\n");
	}
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t battery_limit_show(struct device *dev,
				  struct device_attribute *attr, char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_battery_state state;
	ssize_t length;
	int error;

	mutex_lock(&rgb->lock);
	error = asense_read_battery(&state);
	if (!error && !state.limit_supported)
		error = -EOPNOTSUPP;
	length = error ? error : sysfs_emit(buffer, "%u\n", state.limit);
	mutex_unlock(&rgb->lock);
	return length;
}

static ssize_t battery_limit_store(struct device *dev,
				   struct device_attribute *attr,
				   const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_battery_state previous, actual;
	bool previous_valid;
	bool enabled;
	int error;

	error = kstrtobool(buffer, &enabled);
	if (error)
		return error;
	mutex_lock(&rgb->lock);
	error = asense_read_battery(&previous);
	if (!error && !previous.limit_supported)
		error = -EOPNOTSUPP;
	previous_valid = !error;
	if (!error && previous.limit == enabled) {
		mutex_unlock(&rgb->lock);
		return count;
	}
	if (!error)
		error = asense_write_battery(ASENSE_BATTERY_LIMIT, enabled);
	if (!error)
		error = asense_read_battery(&actual);
	if (!error && actual.limit != enabled)
		error = -EIO;
	if (error && previous_valid &&
	    asense_restore_battery(ASENSE_BATTERY_LIMIT, previous.limit))
		dev_err(dev, "battery limit rollback failed\n");
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t battery_calibration_show(struct device *dev,
					struct device_attribute *attr,
					char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_battery_state state;
	ssize_t length;
	int error;

	mutex_lock(&rgb->lock);
	error = asense_read_battery(&state);
	if (!error && !state.calibration_supported)
		error = -EOPNOTSUPP;
	length = error ? error : sysfs_emit(buffer, "%u\n", state.calibration);
	mutex_unlock(&rgb->lock);
	return length;
}

static ssize_t battery_calibration_store(struct device *dev,
					 struct device_attribute *attr,
					 const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_battery_state previous, actual;
	bool previous_valid;
	bool enabled;
	int error;

	error = kstrtobool(buffer, &enabled);
	if (error)
		return error;
	mutex_lock(&rgb->lock);
	error = asense_read_battery(&previous);
	if (!error && !previous.calibration_supported)
		error = -EOPNOTSUPP;
	previous_valid = !error;
	if (!error && previous.calibration == enabled) {
		mutex_unlock(&rgb->lock);
		return count;
	}
	if (!error)
		error = asense_write_battery(ASENSE_BATTERY_CALIBRATION, enabled);
	if (!error)
		error = asense_read_battery(&actual);
	if (!error && actual.calibration != enabled)
		error = -EIO;
	if (error && previous_valid &&
	    asense_restore_battery(ASENSE_BATTERY_CALIBRATION,
				   previous.calibration))
		dev_err(dev, "battery calibration rollback failed\n");
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t usb_charging_show(struct device *dev,
				struct device_attribute *attr, char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	u8 threshold;
	ssize_t length;
	int error;

	mutex_lock(&rgb->lock);
	error = asense_read_usb(rgb, &threshold);
	length = error ? error : sysfs_emit(buffer, "%u\n", threshold);
	mutex_unlock(&rgb->lock);
	return length;
}

static ssize_t usb_charging_store(struct device *dev,
				 struct device_attribute *attr,
				 const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	u8 requested, previous, actual;
	bool previous_valid;
	int error;

	error = kstrtou8(buffer, 10, &requested);
	if (error)
		return error;
	if (requested != 0 && requested != 10 &&
	    requested != 20 && requested != 30)
		return -EINVAL;
	mutex_lock(&rgb->lock);
	error = asense_read_usb(rgb, &previous);
	previous_valid = !error;
	if (!error && previous == requested) {
		mutex_unlock(&rgb->lock);
		return count;
	}
	if (!error)
		error = asense_write_usb(rgb, requested);
	if (!error)
		error = asense_read_usb(rgb, &actual);
	if (!error && actual != requested)
		error = -EIO;
	if (error && previous_valid && asense_restore_usb(rgb, previous))
		dev_err(dev, "USB charging rollback failed\n");
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t keyboard_timeout_show(struct device *dev,
				     struct device_attribute *attr,
				     char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	bool enabled;
	ssize_t length;
	int error;

	mutex_lock(&rgb->lock);
	error = asense_read_timeout(rgb, &enabled);
	length = error ? error : sysfs_emit(buffer, "%u\n", enabled);
	mutex_unlock(&rgb->lock);
	return length;
}

static ssize_t keyboard_timeout_store(struct device *dev,
				      struct device_attribute *attr,
				      const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	bool enabled, previous, actual;
	bool previous_valid;
	int error;

	error = kstrtobool(buffer, &enabled);
	if (error)
		return error;
	mutex_lock(&rgb->lock);
	error = asense_read_timeout(rgb, &previous);
	previous_valid = !error;
	if (!error && previous == enabled) {
		mutex_unlock(&rgb->lock);
		return count;
	}
	if (!error)
		error = asense_write_timeout(rgb, enabled);
	if (!error)
		error = asense_read_timeout(rgb, &actual);
	if (!error && actual != enabled)
		error = -EIO;
	if (error && previous_valid &&
	    asense_restore_bool(rgb, asense_read_timeout,
				asense_write_timeout, previous))
		dev_err(dev, "keyboard timeout rollback failed\n");
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t boot_sound_show(struct device *dev,
			       struct device_attribute *attr, char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	bool enabled;
	ssize_t length;
	int error;

	mutex_lock(&rgb->lock);
	error = asense_read_boot_sound(rgb, &enabled);
	length = error ? error : sysfs_emit(buffer, "%u\n", enabled);
	mutex_unlock(&rgb->lock);
	return length;
}

static ssize_t boot_sound_store(struct device *dev,
				struct device_attribute *attr,
				const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	bool enabled, previous, actual;
	bool previous_valid;
	int error;

	error = kstrtobool(buffer, &enabled);
	if (error)
		return error;
	mutex_lock(&rgb->lock);
	error = asense_read_boot_sound(rgb, &previous);
	previous_valid = !error;
	if (!error && previous == enabled) {
		mutex_unlock(&rgb->lock);
		return count;
	}
	if (!error)
		error = asense_write_boot_sound(rgb, enabled);
	if (!error)
		error = asense_read_boot_sound(rgb, &actual);
	if (!error && actual != enabled)
		error = -EIO;
	if (error && previous_valid &&
	    asense_restore_bool(rgb, asense_read_boot_sound,
				asense_write_boot_sound, previous))
		dev_err(dev, "boot sound rollback failed\n");
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t lcd_override_show(struct device *dev,
				 struct device_attribute *attr, char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	bool enabled;
	ssize_t length;
	int error;

	mutex_lock(&rgb->lock);
	error = asense_read_lcd(rgb, &enabled);
	length = error ? error : sysfs_emit(buffer, "%u\n", enabled);
	mutex_unlock(&rgb->lock);
	return length;
}

static ssize_t lcd_override_store(struct device *dev,
				  struct device_attribute *attr,
				  const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	bool enabled, previous, actual;
	bool previous_valid;
	int error;

	error = kstrtobool(buffer, &enabled);
	if (error)
		return error;
	mutex_lock(&rgb->lock);
	error = asense_read_lcd(rgb, &previous);
	previous_valid = !error;
	if (!error && previous == enabled) {
		mutex_unlock(&rgb->lock);
		return count;
	}
	if (!error)
		error = asense_write_lcd(rgb, enabled);
	if (!error)
		error = asense_read_lcd(rgb, &actual);
	if (!error && actual != enabled)
		error = -EIO;
	if (error && previous_valid &&
	    asense_restore_bool(rgb, asense_read_lcd,
				asense_write_lcd, previous))
		dev_err(dev, "LCD override rollback failed\n");
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t rear_logo_show(struct device *dev,
			      struct device_attribute *attr, char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_logo logo;
	ssize_t length;
	int error;

	mutex_lock(&rgb->lock);
	error = asense_read_logo(rgb, &logo);
	length = error ? error :
		sysfs_emit(buffer, "%02x%02x%02x,%u,%u\n",
			   logo.red, logo.green, logo.blue,
			   logo.brightness, logo.enabled);
	mutex_unlock(&rgb->lock);
	return length;
}

static ssize_t rear_logo_store(struct device *dev,
			       struct device_attribute *attr,
			       const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_logo requested, previous, actual;
	unsigned int color, brightness, enabled;
	bool previous_valid;
	int offset = 0;
	int error;

	if (sscanf(buffer, "%06x,%u,%u%n", &color, &brightness,
		   &enabled, &offset) != 3 ||
	    !asense_input_consumed(buffer, count, offset))
		return -EINVAL;
	if (color > 0xffffff || brightness > 100 || enabled > 1)
		return -ERANGE;
	requested = (struct asense_logo) {
		.red = color >> 16,
		.green = color >> 8,
		.blue = color,
		.brightness = brightness,
		.enabled = enabled,
	};

	mutex_lock(&rgb->lock);
	error = asense_read_logo(rgb, &previous);
	previous_valid = !error;
	if (!error && memcmp(&previous, &requested, sizeof(requested)) == 0) {
		mutex_unlock(&rgb->lock);
		return count;
	}
	if (!error)
		error = asense_write_logo(rgb, &requested);
	if (!error)
		error = asense_read_logo(rgb, &actual);
	if (!error && memcmp(&actual, &requested, sizeof(requested)) != 0)
		error = -EIO;
	if (error && previous_valid && asense_restore_logo(rgb, &previous))
		dev_err(dev, "rear logo rollback failed\n");
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static int asense_rgb_suspend(struct device *dev)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);

	/* Userspace is frozen first; wait for any in-flight sysfs transaction. */
	mutex_lock(&rgb->lock);
	mutex_unlock(&rgb->lock);
	return 0;
}

static int asense_rgb_resume(struct device *dev)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_effect expected_effect, actual_effect;
	struct asense_zones actual_zones;
	bool static_mode;
	int error = 0;

	mutex_lock(&rgb->lock);
	if (!rgb->rgb_cache_valid)
		goto out;

	static_mode = rgb->cached_effect.mode == 0;
	expected_effect = rgb->cached_effect;
	if (static_mode) {
		expected_effect = asense_static_effect(rgb->cached_zones.brightness);
		error = asense_write_zones(rgb, &rgb->cached_zones);
	} else {
		error = asense_write_effect(rgb, &rgb->cached_effect);
	}
	if (!error)
		error = asense_read_rgb_state(rgb, &actual_effect, &actual_zones);
	if (!error && memcmp(&expected_effect, &actual_effect,
			     sizeof(expected_effect)) != 0)
		error = -EIO;
	if (!error && static_mode &&
	    memcmp(&rgb->cached_zones, &actual_zones,
		   sizeof(rgb->cached_zones)) != 0)
		error = -EIO;
	if (!error)
		asense_cache_rgb_state(rgb, &actual_effect, &actual_zones);
out:
	mutex_unlock(&rgb->lock);
	if (error) {
		dev_err(dev, "keyboard RGB resume restore failed: %d\n", error);
		/* Keyboard lighting must never make the system resume fail. */
	}
	return 0;
}

static DEFINE_SIMPLE_DEV_PM_OPS(asense_rgb_pm_ops,
				asense_rgb_suspend, asense_rgb_resume);

static DEVICE_ATTR_ADMIN_RW(effect);
static DEVICE_ATTR_ADMIN_RW(zones);
static DEVICE_ATTR_ADMIN_RW(power);
static DEVICE_ATTR_ADMIN_RW(battery_limit);
static DEVICE_ATTR_ADMIN_RW(battery_calibration);
static DEVICE_ATTR_ADMIN_RW(usb_charging);
static DEVICE_ATTR_ADMIN_RW(keyboard_timeout);
static DEVICE_ATTR_ADMIN_RW(boot_sound);
static DEVICE_ATTR_ADMIN_RW(lcd_override);
static DEVICE_ATTR_ADMIN_RW(rear_logo);

static struct attribute *asense_rgb_attributes[] = {
	&dev_attr_effect.attr,
	&dev_attr_zones.attr,
	&dev_attr_power.attr,
	&dev_attr_battery_limit.attr,
	&dev_attr_battery_calibration.attr,
	&dev_attr_usb_charging.attr,
	&dev_attr_keyboard_timeout.attr,
	&dev_attr_boot_sound.attr,
	&dev_attr_lcd_override.attr,
	&dev_attr_rear_logo.attr,
	NULL,
};

static umode_t asense_rgb_is_visible(struct kobject *kobject,
				    struct attribute *attribute, int index)
{
	struct device *dev = kobj_to_dev(kobject);
	struct asense_rgb *rgb = dev_get_drvdata(dev);

	if (attribute == &dev_attr_usb_charging.attr && !rgb->usb_available)
		return 0;
	if (attribute == &dev_attr_boot_sound.attr &&
	    !rgb->boot_sound_available)
		return 0;
	if (attribute == &dev_attr_rear_logo.attr && !rgb->logo_available)
		return 0;
	return attribute->mode;
}

static const struct attribute_group asense_rgb_group = {
	.name = "asense_rgb",
	.attrs = asense_rgb_attributes,
	.is_visible = asense_rgb_is_visible,
};

static int asense_rgb_probe(struct wmi_device *wdev, const void *context)
{
	struct asense_battery_state battery;
	struct asense_effect effect;
	struct asense_zones zones;
	struct asense_logo logo;
	struct asense_rgb *rgb;
	bool enabled;
	u8 threshold;
	int error;

	if (!dmi_match(DMI_SYS_VENDOR, "Acer") ||
	    !dmi_match(DMI_PRODUCT_NAME, "Predator PHN16-72"))
		return -ENODEV;

	rgb = devm_kzalloc(&wdev->dev, sizeof(*rgb), GFP_KERNEL);
	if (!rgb)
		return -ENOMEM;
	rgb->wdev = wdev;
	mutex_init(&rgb->lock);
	dev_set_drvdata(&wdev->dev, rgb);

	/* Getter-only capability probe; module load never changes lighting. */
	error = asense_read_rgb_state(rgb, &effect, &zones);
	if (error)
		return dev_err_probe(&wdev->dev, error,
				     "PHN16-72 keyboard RGB state probe failed\n");
	if (!asense_effect_valid(&effect))
		return dev_err_probe(&wdev->dev, -EPROTO,
				     "PHN16-72 keyboard RGB state is invalid\n");
	asense_cache_rgb_state(rgb, &effect, &zones);
	rgb->last_nonzero_brightness = effect.brightness ? effect.brightness : 100;

	/*
	 * These four controls are verified on this exact model. Keep their sysfs
	 * files present even if an early boot getter is not ready yet; every show
	 * and store performs a fresh readback and stores remain transactional.
	 */
	error = wmi_has_guid(ASENSE_BATTERY_GUID) ?
		asense_read_battery(&battery) : -ENODEV;
	if (error)
		dev_warn(&wdev->dev, "battery capability probe failed: %d\n", error);
	else
		dev_info(&wdev->dev,
			 "battery capabilities: limit=%u calibration=%u\n",
			 battery.limit_supported, battery.calibration_supported);

	rgb->usb_available = wmi_has_guid(ASENSE_APGE_GUID) &&
		!asense_read_usb(rgb, &threshold);
	error = wmi_has_guid(ASENSE_APGE_GUID) ?
		asense_read_timeout(rgb, &enabled) : -ENODEV;
	if (error)
		dev_warn(&wdev->dev, "keyboard timeout probe failed: %d\n", error);

	rgb->boot_sound_available = !asense_read_boot_sound(rgb, &enabled);
	error = asense_read_lcd(rgb, &enabled);
	if (error)
		dev_warn(&wdev->dev, "LCD override probe failed: %d\n", error);

	rgb->logo_available = !asense_read_logo(rgb, &logo);
	return devm_device_add_group(&wdev->dev, &asense_rgb_group);
}

static const struct wmi_device_id asense_rgb_id_table[] = {
	{ ASENSE_RGB_GUID, NULL },
	{ }
};
MODULE_DEVICE_TABLE(wmi, asense_rgb_id_table);

static struct wmi_driver asense_rgb_driver = {
	.driver = {
		.name = "asense_rgb",
		.probe_type = PROBE_PREFER_ASYNCHRONOUS,
		.pm = pm_sleep_ptr(&asense_rgb_pm_ops),
	},
	.id_table = asense_rgb_id_table,
	.probe = asense_rgb_probe,
};
module_wmi_driver(asense_rgb_driver);

MODULE_AUTHOR("ASense contributors");
MODULE_DESCRIPTION("Acer Predator PHN16-72 verified WMI control transport");
MODULE_LICENSE("GPL");
MODULE_VERSION("0.1.1");
