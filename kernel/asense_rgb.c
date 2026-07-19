// SPDX-License-Identifier: GPL-2.0-only
/*
 * Bounded Acer WMI transport for the known Gaming, Battery and APGE method
 * devices. Each endpoint is probed independently and only typed sysfs
 * controls are exposed; userspace cannot issue arbitrary WMI calls.
 */

#include <linux/acpi.h>
#include <linux/bitfield.h>
#include <linux/ctype.h>
#include <linux/device.h>
#include <linux/dmi.h>
#include <linux/kernel.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/pm.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/version.h>
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
#define ASENSE_FAN_BEHAVIOR_SET 0x0e
#define ASENSE_FAN_BEHAVIOR_GET 0x0f
#define ASENSE_FAN_SPEED_SET 0x10
#define ASENSE_FAN_SPEED_GET 0x11
#define ASENSE_EFFECT_SET 0x14
#define ASENSE_EFFECT_GET 0x15
#define ASENSE_MISC_SET 0x16
#define ASENSE_MISC_GET 0x17
#define ASENSE_BATTERY_GET 0x14
#define ASENSE_BATTERY_SET 0x15
#define ASENSE_FOUR_ZONE_ENGINE 0x03
#define ASENSE_STATIC_ENGINE 0x00
#define ASENSE_FOUR_ZONE_ENABLE 0x00000f0000000008ULL

#define ASENSE_FAN_BEHAVIOR_CPU BIT(0)
#define ASENSE_FAN_BEHAVIOR_GPU BIT(3)
#define ASENSE_FAN_BEHAVIOR_STATUS_MASK GENMASK_ULL(7, 0)
#define ASENSE_FAN_BEHAVIOR_ID_MASK GENMASK_ULL(15, 0)
#define ASENSE_FAN_BEHAVIOR_SET_CPU_MODE_MASK GENMASK_ULL(17, 16)
#define ASENSE_FAN_BEHAVIOR_SET_GPU_MODE_MASK GENMASK_ULL(23, 22)
#define ASENSE_FAN_BEHAVIOR_GET_CPU_MODE_MASK GENMASK_ULL(9, 8)
#define ASENSE_FAN_BEHAVIOR_GET_GPU_MODE_MASK GENMASK_ULL(15, 14)

#define ASENSE_FAN_SPEED_STATUS_MASK GENMASK_ULL(7, 0)
#define ASENSE_FAN_SPEED_ID_MASK GENMASK_ULL(7, 0)
#define ASENSE_FAN_SPEED_VALUE_MASK GENMASK_ULL(15, 8)

#define ASENSE_MISC_STATUS_MASK GENMASK_ULL(7, 0)
#define ASENSE_MISC_INDEX_MASK GENMASK_ULL(7, 0)
#define ASENSE_MISC_VALUE_MASK GENMASK_ULL(15, 8)
#define ASENSE_PLATFORM_PROFILE_INDEX 0x0b

#define ASENSE_CPU_FAN_ID 0x01
#define ASENSE_GPU_FAN_ID 0x04

#define ASENSE_FAN_MODE_AUTO 0x01
#define ASENSE_FAN_MODE_MAXIMUM 0x02
#define ASENSE_FAN_MODE_MANUAL 0x03

/* Match the hwmon ABI used by the userspace fan controller. */
#define ASENSE_SYSFS_FAN_MAXIMUM 0
#define ASENSE_SYSFS_FAN_MANUAL 1
#define ASENSE_SYSFS_FAN_AUTOMATIC 2

#define ASENSE_PROFILE_QUIET 0x00
#define ASENSE_PROFILE_BALANCED 0x01
#define ASENSE_PROFILE_BALANCED_PERFORMANCE 0x04
#define ASENSE_PROFILE_PERFORMANCE 0x05
#define ASENSE_PROFILE_LOW_POWER 0x06

#define ASENSE_ZONE_MASK_MAX 0x0f
#define ASENSE_ZONE_FLAG_PREAMBLE BIT(0)

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

enum asense_endpoint_type {
	ASENSE_ENDPOINT_GAMING,
	ASENSE_ENDPOINT_BATTERY,
	ASENSE_ENDPOINT_APGE,
};

struct asense_endpoint {
	enum asense_endpoint_type type;
};

struct asense_zoned_quirk {
	const char *product;
	u8 zone_mask;
	u8 flags;
};

struct asense_rgb {
	struct wmi_device *wdev;
	/* Serializes firmware transactions and the resume cache. */
	struct mutex lock;
	struct asense_effect cached_effect;
	struct asense_zones cached_zones;
	u8 last_nonzero_brightness;
	u8 zone_mask;
	bool rgb_cache_valid;
	bool rgb_available;
	bool zone_preamble;
	bool fan_behavior_available;
	bool fan_speed_available;
	bool profile_available;
	bool battery_limit_available;
	bool battery_calibration_available;
	bool usb_available;
	bool timeout_available;
	bool boot_sound_available;
	bool lcd_available;
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

struct asense_profile_choice {
	u8 value;
	const char *name;
};

static const struct asense_profile_choice asense_profile_choices[] = {
	{ ASENSE_PROFILE_LOW_POWER, "low-power" },
	{ ASENSE_PROFILE_QUIET, "quiet" },
	{ ASENSE_PROFILE_BALANCED, "balanced" },
	{ ASENSE_PROFILE_BALANCED_PERFORMANCE, "balanced-performance" },
	{ ASENSE_PROFILE_PERFORMANCE, "performance" },
};

static const struct asense_zoned_quirk asense_zoned_quirks[] = {
	{
		.product = "Predator PHN16-72",
		.zone_mask = 0x0f,
		.flags = ASENSE_ZONE_FLAG_PREAMBLE,
	},
	{
		.product = "Predator PHN14-51",
		.zone_mask = 0x07,
	},
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

static int asense_endpoint_wmi_call(struct asense_rgb *rgb, u32 method,
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
static int asense_scalar_call(struct asense_rgb *rgb, u32 method,
			      u64 payload, u64 *result)
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

	status = wmidev_evaluate_method(rgb->wdev, 0, method, &input, &output);
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

static int asense_scalar_set(struct asense_rgb *rgb, u32 method, u64 payload)
{
	u64 status;
	int error;

	error = asense_scalar_call(rgb, method, payload, &status);
	if (error)
		return error;
	return status == 0 ? 0 : -EREMOTEIO;
}

static int asense_gaming_call(struct asense_rgb *rgb, u32 method,
			      const void *payload, size_t payload_len,
			      bool require_u64, u64 *result)
{
	struct acpi_buffer input = {
		.length = payload_len,
		.pointer = (void *)payload,
	};
	struct acpi_buffer output = { ACPI_ALLOCATE_BUFFER, NULL };
	union acpi_object *object;
	acpi_status status;
	__le32 value32;
	__le64 value64;
	u64 value;
	int error = 0;

	status = wmidev_evaluate_method(rgb->wdev, 0, method, &input, &output);
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
		   object->buffer.pointer && require_u64 &&
		   object->buffer.length >= sizeof(value64)) {
		memcpy(&value64, object->buffer.pointer, sizeof(value64));
		value = le64_to_cpu(value64);
	} else if (object->type == ACPI_TYPE_BUFFER &&
		   object->buffer.pointer && !require_u64 &&
		   object->buffer.length >= sizeof(value64)) {
		memcpy(&value64, object->buffer.pointer, sizeof(value64));
		value = le64_to_cpu(value64);
	} else if (object->type == ACPI_TYPE_BUFFER &&
		   object->buffer.pointer && !require_u64 &&
		   object->buffer.length >= sizeof(value32)) {
		memcpy(&value32, object->buffer.pointer, sizeof(value32));
		value = le32_to_cpu(value32);
	} else {
		error = -EPROTO;
		goto out;
	}
	*result = value;
out:
	kfree(object);
	return error;
}

static int asense_gaming_get(struct asense_rgb *rgb, u32 method,
			     u32 payload, u64 *result)
{
	return asense_gaming_call(rgb, method, &payload, sizeof(payload),
				  true, result);
}

static int asense_gaming_set(struct asense_rgb *rgb, u32 method,
			     u64 payload)
{
	u64 result;
	int error;

	error = asense_gaming_call(rgb, method, &payload, sizeof(payload),
				   false, &result);
	if (error)
		return error;
	return FIELD_GET(ASENSE_FAN_BEHAVIOR_STATUS_MASK, result) == 0 ?
		0 : -EREMOTEIO;
}

static int asense_read_fan_mode(struct asense_rgb *rgb, u16 fan_bitmap,
				u8 *mode)
{
	u64 result;
	u8 value;
	int error;

	if (fan_bitmap != ASENSE_FAN_BEHAVIOR_CPU &&
	    fan_bitmap != ASENSE_FAN_BEHAVIOR_GPU)
		return -EINVAL;
	error = asense_gaming_get(rgb, ASENSE_FAN_BEHAVIOR_GET,
				  FIELD_PREP(ASENSE_FAN_BEHAVIOR_ID_MASK,
					     fan_bitmap),
				  &result);
	if (error)
		return error;
	if (FIELD_GET(ASENSE_FAN_BEHAVIOR_STATUS_MASK, result))
		return -EREMOTEIO;
	if (fan_bitmap == ASENSE_FAN_BEHAVIOR_CPU)
		value = FIELD_GET(ASENSE_FAN_BEHAVIOR_GET_CPU_MODE_MASK,
				  result);
	else
		value = FIELD_GET(ASENSE_FAN_BEHAVIOR_GET_GPU_MODE_MASK,
				  result);
	if (value < ASENSE_FAN_MODE_AUTO || value > ASENSE_FAN_MODE_MANUAL)
		return -EPROTO;
	*mode = value;
	return 0;
}

static int asense_write_fan_mode(struct asense_rgb *rgb, u16 fan_bitmap,
				 u8 mode)
{
	u64 payload;

	if (mode < ASENSE_FAN_MODE_AUTO || mode > ASENSE_FAN_MODE_MANUAL)
		return -EINVAL;
	if (fan_bitmap != ASENSE_FAN_BEHAVIOR_CPU &&
	    fan_bitmap != ASENSE_FAN_BEHAVIOR_GPU)
		return -EINVAL;
	payload = FIELD_PREP(ASENSE_FAN_BEHAVIOR_ID_MASK, fan_bitmap);
	if (fan_bitmap == ASENSE_FAN_BEHAVIOR_CPU)
		payload |= FIELD_PREP(ASENSE_FAN_BEHAVIOR_SET_CPU_MODE_MASK,
				      mode);
	else
		payload |= FIELD_PREP(ASENSE_FAN_BEHAVIOR_SET_GPU_MODE_MASK,
				      mode);
	return asense_gaming_set(rgb, ASENSE_FAN_BEHAVIOR_SET, payload);
}

static int asense_read_fan_speed(struct asense_rgb *rgb, u8 fan, u8 *speed)
{
	u64 result;
	u8 value;
	int error;

	if (fan != ASENSE_CPU_FAN_ID && fan != ASENSE_GPU_FAN_ID)
		return -EINVAL;
	error = asense_gaming_get(rgb, ASENSE_FAN_SPEED_GET,
				  FIELD_PREP(ASENSE_FAN_SPEED_ID_MASK, fan),
				  &result);
	if (error)
		return error;
	if (FIELD_GET(ASENSE_FAN_SPEED_STATUS_MASK, result))
		return -EREMOTEIO;
	value = FIELD_GET(ASENSE_FAN_SPEED_VALUE_MASK, result);
	if (value > 100)
		return -EPROTO;
	*speed = value;
	return 0;
}

static int asense_write_fan_speed(struct asense_rgb *rgb, u8 fan, u8 speed)
{
	u64 payload;

	if ((fan != ASENSE_CPU_FAN_ID && fan != ASENSE_GPU_FAN_ID) ||
	    speed > 100)
		return -EINVAL;
	payload = FIELD_PREP(ASENSE_FAN_SPEED_ID_MASK, fan) |
		  FIELD_PREP(ASENSE_FAN_SPEED_VALUE_MASK, speed);
	return asense_gaming_set(rgb, ASENSE_FAN_SPEED_SET, payload);
}

static int asense_read_profile(struct asense_rgb *rgb, u8 *profile)
{
	u64 result;
	int error;

	error = asense_gaming_get(rgb, ASENSE_MISC_GET,
				  FIELD_PREP(ASENSE_MISC_INDEX_MASK,
					     ASENSE_PLATFORM_PROFILE_INDEX),
				  &result);
	if (error)
		return error;
	if (FIELD_GET(ASENSE_MISC_STATUS_MASK, result))
		return -EREMOTEIO;
	*profile = FIELD_GET(ASENSE_MISC_VALUE_MASK, result);
	return 0;
}

static int asense_write_profile(struct asense_rgb *rgb, u8 profile)
{
	u64 payload;

	payload = FIELD_PREP(ASENSE_MISC_INDEX_MASK,
			     ASENSE_PLATFORM_PROFILE_INDEX) |
		  FIELD_PREP(ASENSE_MISC_VALUE_MASK, profile);
	return asense_gaming_set(rgb, ASENSE_MISC_SET, payload);
}

static const struct asense_profile_choice *asense_profile_by_value(u8 value)
{
	unsigned int index;

	for (index = 0; index < ARRAY_SIZE(asense_profile_choices); index++)
		if (asense_profile_choices[index].value == value)
			return &asense_profile_choices[index];
	return NULL;
}

static const struct asense_profile_choice *asense_profile_by_name(const char *name)
{
	unsigned int index;

	for (index = 0; index < ARRAY_SIZE(asense_profile_choices); index++)
		if (sysfs_streq(name, asense_profile_choices[index].name))
			return &asense_profile_choices[index];
	return NULL;
}

static unsigned int asense_zone_count(u8 mask)
{
	switch (mask) {
	case 0x01:
		return 1;
	case 0x03:
		return 2;
	case 0x07:
		return 3;
	case 0x0f:
		return 4;
	default:
		return 0;
	}
}

static void asense_select_zone_config(struct asense_rgb *rgb)
{
	const char *product = dmi_get_system_info(DMI_PRODUCT_NAME);
	unsigned int index;

	/* Unknown devices must satisfy the complete four-zone getter contract. */
	rgb->zone_mask = ASENSE_ZONE_MASK_MAX;
	rgb->zone_preamble = false;
	if (!product)
		return;
	for (index = 0; index < ARRAY_SIZE(asense_zoned_quirks); index++) {
		if (strcmp(product, asense_zoned_quirks[index].product) != 0)
			continue;
		rgb->zone_mask = asense_zoned_quirks[index].zone_mask;
		rgb->zone_preamble = asense_zoned_quirks[index].flags &
			ASENSE_ZONE_FLAG_PREAMBLE;
		return;
	}
}

static int asense_read_battery(struct asense_rgb *rgb,
			       struct asense_battery_state *state)
{
	u8 input[4] = { 1, 1, 0, 0 };
	u8 result[8];
	int error;

	error = asense_endpoint_wmi_call(rgb, ASENSE_BATTERY_GET, input,
					 sizeof(input), result, sizeof(result));
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

static int asense_write_battery(struct asense_rgb *rgb, u8 function,
				bool enabled)
{
	u8 input[8] = { 1, function, enabled, 0, 0, 0, 0, 0 };
	u8 result[4];
	int error;

	if (function != ASENSE_BATTERY_LIMIT &&
	    function != ASENSE_BATTERY_CALIBRATION)
		return -EINVAL;
	error = asense_endpoint_wmi_call(rgb, ASENSE_BATTERY_SET, input,
					 sizeof(input), result, sizeof(result));
	if (error)
		return error;
	/* BESB byte zero is the firmware return status; remaining bytes are reserved. */
	return result[0] == 0 ? 0 : -EREMOTEIO;
}

static int asense_read_usb(struct asense_rgb *rgb, u8 *threshold)
{
	u64 result;
	int error;

	error = asense_scalar_call(rgb, ASENSE_FUNCTION_GET,
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
	return asense_scalar_set(rgb, ASENSE_FUNCTION_SET, payload);
}

static int asense_read_timeout(struct asense_rgb *rgb, bool *enabled)
{
	u64 result;
	int error;

	error = asense_scalar_call(rgb, ASENSE_FUNCTION_GET,
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
	return asense_scalar_set(rgb, ASENSE_FUNCTION_SET,
				 enabled ? ASENSE_TIMEOUT_SET_ON :
				 ASENSE_TIMEOUT_SET_OFF);
}

static int asense_read_boot_sound(struct asense_rgb *rgb, bool *enabled)
{
	u64 result;
	int error;

	error = asense_scalar_call(rgb, ASENSE_MISC_GET,
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
	return asense_scalar_set(rgb, ASENSE_MISC_SET,
				 enabled ? ASENSE_BOOT_SOUND_SET_ON :
				 ASENSE_BOOT_SOUND_SET_OFF);
}

static int asense_read_lcd(struct asense_rgb *rgb, bool *enabled)
{
	u64 result;
	int error;

	error = asense_scalar_call(rgb, ASENSE_PROFILE_GET,
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
	return asense_scalar_set(rgb, ASENSE_PROFILE_SET,
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
	u8 gate[16] = { logo->enabled, 0, 0, 0, 0, 0, 0, 0, 0, 2 };
	int error;

	error = asense_set_call(rgb, ASENSE_LOGO_SET,
				payload, sizeof(payload));
	if (error)
		return error;
	/* Some firmware reads the physical LBLE power gate from method 0x14. */
	return asense_set_call(rgb, ASENSE_EFFECT_SET, gate, sizeof(gate));
}

static int asense_restore_battery(struct asense_rgb *rgb, u8 function,
				  bool expected)
{
	struct asense_battery_state actual;
	int error;

	error = asense_write_battery(rgb, function, expected);
	if (!error)
		error = asense_read_battery(rgb, &actual);
	if (!error &&
	    ((function == ASENSE_BATTERY_LIMIT && actual.limit != expected) ||
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
	unsigned int count = asense_zone_count(rgb->zone_mask);
	unsigned int zone;
	int error;

	if (!count)
		return -EINVAL;
	memset(zones, 0, sizeof(*zones));
	error = asense_read_effect(rgb, effect);
	if (error)
		return error;
	for (zone = 0; zone < count; zone++) {
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
	unsigned int count = asense_zone_count(rgb->zone_mask);
	int error;
	ssize_t length;

	mutex_lock(&rgb->lock);
	error = asense_read_zones(rgb, &zones);
	if (error)
		length = error;
	else if (count == 1)
		length = sysfs_emit(buffer, "%02x%02x%02x,%u\n",
				    zones.rgb[0][0], zones.rgb[0][1],
				    zones.rgb[0][2], zones.brightness);
	else if (count == 2)
		length = sysfs_emit(buffer, "%02x%02x%02x,%02x%02x%02x,%u\n",
				    zones.rgb[0][0], zones.rgb[0][1], zones.rgb[0][2],
				    zones.rgb[1][0], zones.rgb[1][1], zones.rgb[1][2],
				    zones.brightness);
	else if (count == 3)
		length = sysfs_emit(buffer,
				    "%02x%02x%02x,%02x%02x%02x,%02x%02x%02x,%u\n",
				    zones.rgb[0][0], zones.rgb[0][1], zones.rgb[0][2],
				    zones.rgb[1][0], zones.rgb[1][1], zones.rgb[1][2],
				    zones.rgb[2][0], zones.rgb[2][1], zones.rgb[2][2],
				    zones.brightness);
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
	unsigned int count = asense_zone_count(rgb->zone_mask);
	u64 gaming_sys_info;
	unsigned int zone;
	int error;

	if (!count)
		return -EINVAL;
	/*
	 * Acer's four-zone static transaction has a required scalar preamble.
	 * Method 5 polls the gaming controller; method 2 then enables all four
	 * zones.  Both inputs are eight-byte little-endian scalar payloads.
	 */
	if (rgb->zone_preamble) {
		error = asense_scalar_call(rgb, ASENSE_GAMING_SYS_INFO_GET,
					   0, &gaming_sys_info);
		if (error)
			return error;
		error = asense_scalar_set(rgb, ASENSE_GAMING_LED_SET,
					  ASENSE_FOUR_ZONE_ENABLE);
		if (error)
			return error;
	}

	for (zone = 0; zone < count; zone++) {
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

static int asense_parse_zones(struct asense_rgb *rgb, const char *buffer,
			      size_t count, struct asense_zones *zones)
{
	unsigned int colors[4] = { 0 };
	unsigned int brightness;
	unsigned int zone_count = asense_zone_count(rgb->zone_mask);
	unsigned int zone;
	int offset = 0;
	int matched;

	switch (zone_count) {
	case 1:
		matched = sscanf(buffer, "%06x,%u%n", &colors[0],
				 &brightness, &offset);
		break;
	case 2:
		matched = sscanf(buffer, "%06x,%06x,%u%n", &colors[0],
				 &colors[1], &brightness, &offset);
		break;
	case 3:
		matched = sscanf(buffer, "%06x,%06x,%06x,%u%n", &colors[0],
				 &colors[1], &colors[2], &brightness, &offset);
		break;
	case 4:
		matched = sscanf(buffer, "%06x,%06x,%06x,%06x,%u%n",
				 &colors[0], &colors[1], &colors[2], &colors[3],
				 &brightness, &offset);
		break;
	default:
		return -EINVAL;
	}
	if (matched != zone_count + 1 ||
	    !asense_input_consumed(buffer, count, offset))
		return -EINVAL;
	if (brightness > 100)
		return -ERANGE;
	memset(zones, 0, sizeof(*zones));
	for (zone = 0; zone < zone_count; zone++) {
		if (colors[zone] > 0xffffff)
			return -ERANGE;
		zones->rgb[zone][0] = colors[zone] >> 16;
		zones->rgb[zone][1] = colors[zone] >> 8;
		zones->rgb[zone][2] = colors[zone];
	}
	zones->brightness = brightness;
	return 0;
}

static ssize_t zones_store(struct device *dev, struct device_attribute *attr,
			   const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	struct asense_zones requested, previous, actual;
	struct asense_effect previous_effect, actual_effect;
	struct asense_effect expected_effect;
	bool previous_valid = false;
	int rollback_effect;
	int rollback_zones;
	int error;

	error = asense_parse_zones(rgb, buffer, count, &requested);
	if (error)
		return error;
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
	error = asense_read_battery(rgb, &state);
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
	error = asense_read_battery(rgb, &previous);
	if (!error && !previous.limit_supported)
		error = -EOPNOTSUPP;
	previous_valid = !error;
	if (!error && previous.limit == enabled) {
		mutex_unlock(&rgb->lock);
		return count;
	}
	if (!error)
		error = asense_write_battery(rgb, ASENSE_BATTERY_LIMIT, enabled);
	if (!error)
		error = asense_read_battery(rgb, &actual);
	if (!error && actual.limit != enabled)
		error = -EIO;
	if (error && previous_valid &&
	    asense_restore_battery(rgb, ASENSE_BATTERY_LIMIT, previous.limit))
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
	error = asense_read_battery(rgb, &state);
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
	error = asense_read_battery(rgb, &previous);
	if (!error && !previous.calibration_supported)
		error = -EOPNOTSUPP;
	previous_valid = !error;
	if (!error && previous.calibration == enabled) {
		mutex_unlock(&rgb->lock);
		return count;
	}
	if (!error)
		error = asense_write_battery(rgb, ASENSE_BATTERY_CALIBRATION,
					     enabled);
	if (!error)
		error = asense_read_battery(rgb, &actual);
	if (!error && actual.calibration != enabled)
		error = -EIO;
	if (error && previous_valid &&
	    asense_restore_battery(rgb, ASENSE_BATTERY_CALIBRATION,
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

static ssize_t asense_bool_show(struct device *dev, char *buffer,
				int (*read)(struct asense_rgb *, bool *))
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	bool enabled;
	int error;

	mutex_lock(&rgb->lock);
	error = read(rgb, &enabled);
	mutex_unlock(&rgb->lock);
	return error ? error : sysfs_emit(buffer, "%u\n", enabled);
}

static ssize_t asense_bool_store(struct device *dev, const char *buffer,
				 size_t count,
				 int (*read)(struct asense_rgb *, bool *),
				 int (*write)(struct asense_rgb *, bool),
				 const char *name)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	bool enabled, previous, actual;
	bool previous_valid;
	int error;

	error = kstrtobool(buffer, &enabled);
	if (error)
		return error;
	mutex_lock(&rgb->lock);
	error = read(rgb, &previous);
	previous_valid = !error;
	if (!error && previous != enabled)
		error = write(rgb, enabled);
	if (!error)
		error = read(rgb, &actual);
	if (!error && actual != enabled)
		error = -EIO;
	if (error && previous_valid &&
	    asense_restore_bool(rgb, read, write, previous))
		dev_err(dev, "%s rollback failed\n", name);
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

#define ASENSE_BOOL_ATTRIBUTE(_name, _read, _write, _label) \
static ssize_t _name##_show(struct device *dev, \
			    struct device_attribute *attr, char *buffer) \
{ return asense_bool_show(dev, buffer, _read); } \
static ssize_t _name##_store(struct device *dev, \
			     struct device_attribute *attr, \
			     const char *buffer, size_t count) \
{ return asense_bool_store(dev, buffer, count, _read, _write, _label); }

ASENSE_BOOL_ATTRIBUTE(keyboard_timeout, asense_read_timeout,
		      asense_write_timeout, "keyboard timeout")
ASENSE_BOOL_ATTRIBUTE(boot_sound, asense_read_boot_sound,
		      asense_write_boot_sound, "boot sound")
ASENSE_BOOL_ATTRIBUTE(lcd_override, asense_read_lcd,
		      asense_write_lcd, "LCD override")

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

static ssize_t zone_mask_show(struct device *dev,
			      struct device_attribute *attr, char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);

	return sysfs_emit(buffer, "0x%02x\n", rgb->zone_mask);
}

static int asense_fan_mode_to_sysfs(u8 mode)
{
	switch (mode) {
	case ASENSE_FAN_MODE_MAXIMUM:
		return ASENSE_SYSFS_FAN_MAXIMUM;
	case ASENSE_FAN_MODE_MANUAL:
		return ASENSE_SYSFS_FAN_MANUAL;
	case ASENSE_FAN_MODE_AUTO:
		return ASENSE_SYSFS_FAN_AUTOMATIC;
	default:
		return -EPROTO;
	}
}

static int asense_fan_mode_from_sysfs(u8 mode)
{
	switch (mode) {
	case ASENSE_SYSFS_FAN_MAXIMUM:
		return ASENSE_FAN_MODE_MAXIMUM;
	case ASENSE_SYSFS_FAN_MANUAL:
		return ASENSE_FAN_MODE_MANUAL;
	case ASENSE_SYSFS_FAN_AUTOMATIC:
		return ASENSE_FAN_MODE_AUTO;
	default:
		return -EINVAL;
	}
}

static ssize_t asense_fan_mode_show(struct device *dev, u16 fan_bitmap,
				    char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	u8 mode;
	int value;

	mutex_lock(&rgb->lock);
	value = asense_read_fan_mode(rgb, fan_bitmap, &mode);
	if (!value)
		value = asense_fan_mode_to_sysfs(mode);
	mutex_unlock(&rgb->lock);
	return value < 0 ? value : sysfs_emit(buffer, "%d\n", value);
}

static ssize_t asense_fan_mode_store(struct device *dev, u16 fan_bitmap,
				     const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	u8 previous, actual, requested;
	bool previous_valid;
	int mode;
	int error;

	error = kstrtou8(buffer, 10, &requested);
	if (error)
		return error;
	mode = asense_fan_mode_from_sysfs(requested);
	if (mode < 0)
		return mode;
	mutex_lock(&rgb->lock);
	error = asense_read_fan_mode(rgb, fan_bitmap, &previous);
	previous_valid = !error;
	if (!error && previous == mode)
		goto out;
	if (!error)
		error = asense_write_fan_mode(rgb, fan_bitmap, mode);
	if (!error)
		error = asense_read_fan_mode(rgb, fan_bitmap, &actual);
	if (!error && actual != mode)
		error = -EIO;
	if (error && previous_valid) {
		int rollback = asense_write_fan_mode(rgb, fan_bitmap, previous);

		if (!rollback)
			rollback = asense_read_fan_mode(rgb, fan_bitmap, &actual);
		if (!rollback && actual != previous)
			rollback = -EIO;
		if (rollback)
			dev_err(dev, "fan mode rollback failed\n");
	}
out:
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t asense_fan_speed_show(struct device *dev, u8 fan, char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	u8 speed;
	int error;

	mutex_lock(&rgb->lock);
	error = asense_read_fan_speed(rgb, fan, &speed);
	mutex_unlock(&rgb->lock);
	return error ? error : sysfs_emit(buffer, "%u\n", speed);
}

static ssize_t asense_fan_speed_store(struct device *dev, u8 fan,
				      const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	u8 requested, previous, actual;
	bool previous_valid;
	int error;

	error = kstrtou8(buffer, 10, &requested);
	if (error)
		return error;
	if (requested > 100)
		return -ERANGE;
	mutex_lock(&rgb->lock);
	error = asense_read_fan_speed(rgb, fan, &previous);
	previous_valid = !error;
	if (!error && previous == requested)
		goto out;
	if (!error)
		error = asense_write_fan_speed(rgb, fan, requested);
	if (!error)
		error = asense_read_fan_speed(rgb, fan, &actual);
	if (!error && actual != requested)
		error = -EIO;
	if (error && previous_valid) {
		int rollback = asense_write_fan_speed(rgb, fan, previous);

		if (!rollback)
			rollback = asense_read_fan_speed(rgb, fan, &actual);
		if (!rollback && actual != previous)
			rollback = -EIO;
		if (rollback)
			dev_err(dev, "fan speed rollback failed\n");
	}
out:
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

#define ASENSE_FAN_ATTRIBUTE(_name, _show_fn, _store_fn, _selector) \
static ssize_t _name##_show(struct device *dev, \
			    struct device_attribute *attr, char *buffer) \
{ return _show_fn(dev, _selector, buffer); } \
static ssize_t _name##_store(struct device *dev, \
			     struct device_attribute *attr, \
			     const char *buffer, size_t count) \
{ return _store_fn(dev, _selector, buffer, count); }

ASENSE_FAN_ATTRIBUTE(cpu_mode, asense_fan_mode_show,
		     asense_fan_mode_store, ASENSE_FAN_BEHAVIOR_CPU)
ASENSE_FAN_ATTRIBUTE(gpu_mode, asense_fan_mode_show,
		     asense_fan_mode_store, ASENSE_FAN_BEHAVIOR_GPU)
ASENSE_FAN_ATTRIBUTE(cpu_speed, asense_fan_speed_show,
		     asense_fan_speed_store, ASENSE_CPU_FAN_ID)
ASENSE_FAN_ATTRIBUTE(gpu_speed, asense_fan_speed_show,
		     asense_fan_speed_store, ASENSE_GPU_FAN_ID)

static ssize_t profile_show(struct device *dev, struct device_attribute *attr,
			    char *buffer)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	const struct asense_profile_choice *choice;
	u8 value;
	int error;

	mutex_lock(&rgb->lock);
	error = asense_read_profile(rgb, &value);
	choice = error ? NULL : asense_profile_by_value(value);
	mutex_unlock(&rgb->lock);
	if (error)
		return error;
	if (!choice)
		return -EPROTO;
	return sysfs_emit(buffer, "%s\n", choice->name);
}

static ssize_t profile_store(struct device *dev, struct device_attribute *attr,
			     const char *buffer, size_t count)
{
	struct asense_rgb *rgb = dev_get_drvdata(dev);
	const struct asense_profile_choice *choice;
	u8 previous, actual;
	bool previous_valid;
	int error;

	choice = asense_profile_by_name(buffer);
	if (!choice)
		return -EINVAL;
	mutex_lock(&rgb->lock);
	error = asense_read_profile(rgb, &previous);
	previous_valid = !error && asense_profile_by_value(previous);
	if (!error && !previous_valid)
		error = -EPROTO;
	if (!error && previous == choice->value)
		goto out;
	if (!error)
		error = asense_write_profile(rgb, choice->value);
	if (!error)
		error = asense_read_profile(rgb, &actual);
	if (!error && actual != choice->value)
		error = -EIO;
	if (error && previous_valid) {
		int rollback = asense_write_profile(rgb, previous);

		if (!rollback)
			rollback = asense_read_profile(rgb, &actual);
		if (!rollback && actual != previous)
			rollback = -EIO;
		if (rollback)
			dev_err(dev, "gaming profile rollback failed\n");
	}
out:
	mutex_unlock(&rgb->lock);
	return error ? error : count;
}

static ssize_t choices_show(struct device *dev, struct device_attribute *attr,
			    char *buffer)
{
	return sysfs_emit(buffer,
		"low-power quiet balanced balanced-performance performance\n");
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
static DEVICE_ATTR_RO(zone_mask);
static DEVICE_ATTR_ADMIN_RW(battery_limit);
static DEVICE_ATTR_ADMIN_RW(battery_calibration);
static DEVICE_ATTR_ADMIN_RW(usb_charging);
static DEVICE_ATTR_ADMIN_RW(keyboard_timeout);
static DEVICE_ATTR_ADMIN_RW(boot_sound);
static DEVICE_ATTR_ADMIN_RW(lcd_override);
static DEVICE_ATTR_ADMIN_RW(rear_logo);
static DEVICE_ATTR_ADMIN_RW(cpu_mode);
static DEVICE_ATTR_ADMIN_RW(gpu_mode);
static DEVICE_ATTR_ADMIN_RW(cpu_speed);
static DEVICE_ATTR_ADMIN_RW(gpu_speed);
static DEVICE_ATTR_ADMIN_RW(profile);
static DEVICE_ATTR_RO(choices);

static struct attribute *asense_rgb_attributes[] = {
	&dev_attr_effect.attr,
	&dev_attr_zones.attr,
	&dev_attr_power.attr,
	&dev_attr_zone_mask.attr,
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

	if ((attribute == &dev_attr_effect.attr ||
	     attribute == &dev_attr_zones.attr ||
	     attribute == &dev_attr_power.attr ||
	     attribute == &dev_attr_zone_mask.attr) && !rgb->rgb_available)
		return 0;
	if (attribute == &dev_attr_boot_sound.attr &&
	    !rgb->boot_sound_available)
		return 0;
	if (attribute == &dev_attr_lcd_override.attr && !rgb->lcd_available)
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

static struct attribute *asense_fan_attributes[] = {
	&dev_attr_cpu_mode.attr,
	&dev_attr_gpu_mode.attr,
	&dev_attr_cpu_speed.attr,
	&dev_attr_gpu_speed.attr,
	NULL,
};

static umode_t asense_fan_is_visible(struct kobject *kobject,
				     struct attribute *attribute, int index)
{
	struct device *dev = kobj_to_dev(kobject);
	struct asense_rgb *rgb = dev_get_drvdata(dev);

	if (!rgb->fan_behavior_available)
		return 0;
	if ((attribute == &dev_attr_cpu_speed.attr ||
	     attribute == &dev_attr_gpu_speed.attr) &&
	    !rgb->fan_speed_available)
		return 0;
	return attribute->mode;
}

static const struct attribute_group asense_fan_group = {
	.name = "gaming_fan",
	.attrs = asense_fan_attributes,
	.is_visible = asense_fan_is_visible,
};

static struct attribute *asense_profile_attributes[] = {
	&dev_attr_profile.attr,
	&dev_attr_choices.attr,
	NULL,
};

static umode_t asense_profile_is_visible(struct kobject *kobject,
					 struct attribute *attribute, int index)
{
	struct device *dev = kobj_to_dev(kobject);
	struct asense_rgb *rgb = dev_get_drvdata(dev);

	return rgb->profile_available ? attribute->mode : 0;
}

static const struct attribute_group asense_profile_group = {
	.name = "gaming_profile",
	.attrs = asense_profile_attributes,
	.is_visible = asense_profile_is_visible,
};

static struct attribute *asense_battery_attributes[] = {
	&dev_attr_battery_limit.attr,
	&dev_attr_battery_calibration.attr,
	NULL,
};

static umode_t asense_battery_is_visible(struct kobject *kobject,
					 struct attribute *attribute, int index)
{
	struct device *dev = kobj_to_dev(kobject);
	struct asense_rgb *rgb = dev_get_drvdata(dev);

	if (attribute == &dev_attr_battery_limit.attr &&
	    !rgb->battery_limit_available)
		return 0;
	if (attribute == &dev_attr_battery_calibration.attr &&
	    !rgb->battery_calibration_available)
		return 0;
	return attribute->mode;
}

static const struct attribute_group asense_battery_group = {
	.name = "asense_battery",
	.attrs = asense_battery_attributes,
	.is_visible = asense_battery_is_visible,
};

static struct attribute *asense_apge_attributes[] = {
	&dev_attr_usb_charging.attr,
	&dev_attr_keyboard_timeout.attr,
	NULL,
};

static umode_t asense_apge_is_visible(struct kobject *kobject,
				      struct attribute *attribute, int index)
{
	struct device *dev = kobj_to_dev(kobject);
	struct asense_rgb *rgb = dev_get_drvdata(dev);

	if (attribute == &dev_attr_usb_charging.attr && !rgb->usb_available)
		return 0;
	if (attribute == &dev_attr_keyboard_timeout.attr &&
	    !rgb->timeout_available)
		return 0;
	return attribute->mode;
}

static const struct attribute_group asense_apge_group = {
	.name = "asense_apge",
	.attrs = asense_apge_attributes,
	.is_visible = asense_apge_is_visible,
};

static bool asense_reference_model(void)
{
	return dmi_match(DMI_PRODUCT_NAME, "Predator PHN16-72");
}

static int asense_probe_gaming(struct asense_rgb *rgb)
{
	struct asense_effect effect;
	struct asense_zones zones;
	struct asense_logo logo;
	u8 cpu_mode, gpu_mode;
	u8 cpu_speed, gpu_speed;
	u8 profile;
	bool enabled;
	int error;

	asense_select_zone_config(rgb);
	error = asense_read_rgb_state(rgb, &effect, &zones);
	if (!error) {
		if (!asense_effect_valid(&effect)) {
			dev_warn(&rgb->wdev->dev,
				 "keyboard RGB state is invalid\n");
		} else {
			rgb->rgb_available = true;
			asense_cache_rgb_state(rgb, &effect, &zones);
			rgb->last_nonzero_brightness = effect.brightness ?
				effect.brightness : 100;
		}
	}

	error = asense_read_fan_mode(rgb, ASENSE_FAN_BEHAVIOR_CPU,
				     &cpu_mode);
	if (!error)
		error = asense_read_fan_mode(rgb, ASENSE_FAN_BEHAVIOR_GPU,
					     &gpu_mode);
	if (!error)
		rgb->fan_behavior_available = true;

	error = asense_read_fan_speed(rgb, ASENSE_CPU_FAN_ID, &cpu_speed);
	if (!error)
		error = asense_read_fan_speed(rgb, ASENSE_GPU_FAN_ID,
					      &gpu_speed);
	if (!error)
		rgb->fan_speed_available = true;

	error = asense_read_profile(rgb, &profile);
	if (!error && asense_profile_by_value(profile))
		rgb->profile_available = true;

	rgb->boot_sound_available = !asense_read_boot_sound(rgb, &enabled);
	rgb->lcd_available = !asense_read_lcd(rgb, &enabled) ||
		asense_reference_model();
	rgb->logo_available = !asense_read_logo(rgb, &logo);

	if (!rgb->rgb_available && !rgb->fan_behavior_available &&
	    !rgb->profile_available && !rgb->boot_sound_available &&
	    !rgb->lcd_available && !rgb->logo_available)
		return -ENODEV;
	if (rgb->rgb_available || rgb->boot_sound_available ||
	    rgb->lcd_available || rgb->logo_available) {
		error = devm_device_add_group(&rgb->wdev->dev, &asense_rgb_group);
		if (error)
			return error;
	}
	if (rgb->fan_behavior_available) {
		error = devm_device_add_group(&rgb->wdev->dev,
					      &asense_fan_group);
		if (error)
			return error;
	}
	if (rgb->profile_available)
		return devm_device_add_group(&rgb->wdev->dev,
					     &asense_profile_group);
	return 0;
}

static int asense_probe_battery(struct asense_rgb *rgb)
{
	struct asense_battery_state battery;
	int error;

	error = asense_read_battery(rgb, &battery);
	if (error) {
		if (!asense_reference_model())
			return -ENODEV;
		/* Preserve the reference model's early-boot retry-on-access ABI. */
		rgb->battery_limit_available = true;
		rgb->battery_calibration_available = true;
	} else {
		rgb->battery_limit_available = battery.limit_supported;
		rgb->battery_calibration_available =
			battery.calibration_supported;
	}
	if (!rgb->battery_limit_available &&
	    !rgb->battery_calibration_available)
		return -ENODEV;
	return devm_device_add_group(&rgb->wdev->dev, &asense_battery_group);
}

static int asense_probe_apge(struct asense_rgb *rgb)
{
	bool enabled;
	u8 threshold;

	rgb->usb_available = !asense_read_usb(rgb, &threshold);
	rgb->timeout_available = !asense_read_timeout(rgb, &enabled) ||
		asense_reference_model();
	if (!rgb->usb_available && !rgb->timeout_available)
		return -ENODEV;
	return devm_device_add_group(&rgb->wdev->dev, &asense_apge_group);
}

static int asense_rgb_probe(struct wmi_device *wdev, const void *context)
{
	const struct asense_endpoint *endpoint = context;
	struct asense_rgb *rgb;

	if (!dmi_match(DMI_SYS_VENDOR, "Acer") || !endpoint)
		return -ENODEV;

	rgb = devm_kzalloc(&wdev->dev, sizeof(*rgb), GFP_KERNEL);
	if (!rgb)
		return -ENOMEM;
	rgb->wdev = wdev;
	mutex_init(&rgb->lock);
	dev_set_drvdata(&wdev->dev, rgb);

	switch (endpoint->type) {
	case ASENSE_ENDPOINT_GAMING:
		return asense_probe_gaming(rgb);
	case ASENSE_ENDPOINT_BATTERY:
		return asense_probe_battery(rgb);
	case ASENSE_ENDPOINT_APGE:
		return asense_probe_apge(rgb);
	default:
		return -EINVAL;
	}
}

static const struct asense_endpoint asense_gaming_endpoint = {
	.type = ASENSE_ENDPOINT_GAMING,
};

static const struct asense_endpoint asense_battery_endpoint = {
	.type = ASENSE_ENDPOINT_BATTERY,
};

static const struct asense_endpoint asense_apge_endpoint = {
	.type = ASENSE_ENDPOINT_APGE,
};

static const struct wmi_device_id asense_rgb_id_table[] = {
	{ ASENSE_RGB_GUID, &asense_gaming_endpoint },
	{ ASENSE_BATTERY_GUID, &asense_battery_endpoint },
	{ ASENSE_APGE_GUID, &asense_apge_endpoint },
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
#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 9, 0)
	.no_singleton = true,
#endif
};

/* WMI core prevents a second WMI driver from binding the same endpoint. */
module_wmi_driver(asense_rgb_driver);

MODULE_AUTHOR("ASense contributors");
MODULE_DESCRIPTION("Bounded Acer Gaming, Battery and APGE WMI transport");
MODULE_LICENSE("GPL");
MODULE_VERSION("0.2.1");
