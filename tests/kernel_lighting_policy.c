/* SPDX-License-Identifier: GPL-2.0-only */
#include <assert.h>
#include <string.h>
#include "asense_rgb_policy.h"

static void boot_records_are_readable_but_not_writable(void)
{
	struct asense_effect boot = {
		.mode = 3, .brightness = 100, .engine = ASENSE_FOUR_ZONE_ENGINE,
	};
	u8 flags = ASENSE_ZONE_FLAG_BOOT_WAVE_ZERO;

	assert(asense_policy_read_valid(flags, &boot));
	assert(!asense_policy_write_valid(flags, &boot));
	assert(!asense_policy_read_valid(0, &boot));
	boot.brightness = 30;
	boot.engine = ASENSE_STATIC_ENGINE;
	assert(asense_policy_read_valid(flags, &boot));
	assert(!asense_policy_write_valid(flags, &boot));
	boot.red = 1;
	assert(!asense_policy_read_valid(flags, &boot));
	boot.red = 0;
	boot.speed = 1;
	assert(!asense_policy_read_valid(flags, &boot));
	boot.speed = 0;
	boot.brightness = 101;
	assert(!asense_policy_read_valid(flags, &boot));
	boot.brightness = 30;
	boot.mode = 4;
	assert(!asense_policy_read_valid(flags, &boot));
}

static void static_tolerance_never_relaxes_dynamic_modes(void)
{
	u8 flags = ASENSE_ZONE_FLAG_FOUR_ZONE_ENGINE |
		   ASENSE_ZONE_FLAG_STALE_STATIC |
		   ASENSE_ZONE_FLAG_STATIC_COLOR_SHADOW;
	struct asense_zones zones = { .rgb = {{255, 0, 0}, {0, 255, 0},
					    {0, 0, 255}, {255, 255, 255}},
				     .brightness = 60 };
	struct asense_zones wrong_zones = zones;
	struct asense_effect request = asense_policy_static_effect(flags, &zones);
	struct asense_effect actual = request;
	unsigned int mode;

	assert(request.engine == ASENSE_FOUR_ZONE_ENGINE);
	assert(asense_policy_write_valid(flags, &request));
	actual.speed = 7;
	actual.direction = 2;
	actual.red = 120;
	assert(asense_policy_read_valid(flags, &actual));
	assert(!asense_policy_write_valid(flags, &actual));
	assert(asense_policy_effect_matches(flags, &request, &actual, true));
	assert(!asense_policy_effect_matches(flags, &request, &actual, false));
	assert(asense_policy_zones_match(&zones, &zones, 4));
	wrong_zones.rgb[3][2] = 0;
	assert(!(asense_policy_effect_matches(flags, &request, &actual, true) &&
		 asense_policy_zones_match(&zones, &wrong_zones, 4)));
	actual.brightness = 59;
	assert(!asense_policy_effect_matches(flags, &request, &actual, true));
	actual = request;
	actual.engine = ASENSE_STATIC_ENGINE;
	assert(!asense_policy_effect_matches(flags, &request, &actual, true));
	for (mode = 1; mode <= 7; mode++) {
		struct asense_effect dynamic = {
			.mode = mode, .brightness = 60,
			.engine = ASENSE_FOUR_ZONE_ENGINE,
		};

		if (mode == 3 || mode == 4) {
			assert(!asense_policy_read_valid(flags, &dynamic));
			dynamic.direction = 1;
		}
		assert(asense_policy_write_valid(flags, &dynamic));
		actual = dynamic;
		actual.direction = 2;
		if (mode == 3 || mode == 4)
			assert(!asense_policy_effect_matches(flags, &dynamic, &actual, true));
		else
			assert(!asense_policy_read_valid(flags, &actual));
	}
}

static void global_static_uses_the_requested_color_and_strict_readback(void)
{
	struct asense_zones zones = { .rgb = {{17, 34, 51}}, .brightness = 40 };
	u8 flags = ASENSE_ZONE_FLAG_GLOBAL_STATIC;
	struct asense_effect request = asense_policy_static_effect(flags, &zones);
	struct asense_effect wrong = request;

	assert(request.engine == ASENSE_STATIC_ENGINE);
	assert(request.red == 17 && request.green == 34 && request.blue == 51);
	assert(asense_policy_write_valid(flags, &request));
	wrong.blue = 52;
	assert(!asense_policy_effect_matches(flags, &request, &wrong, true));
	assert(!asense_policy_effect_matches(flags, &request, &wrong, false));
	zones.brightness = 0;
	request = asense_policy_static_effect(flags, &zones);
	assert(request.red == 17 && request.brightness == 0);
	zones.brightness = 40;
	request = asense_policy_static_effect(flags, &zones);
	assert(request.red == 17 && request.brightness == 40);
}

static void all_supported_zone_counts_verify_each_visible_color(void)
{
	struct asense_zones zones = { .rgb = {{1, 2, 3}, {4, 5, 6},
					    {7, 8, 9}, {10, 11, 12}},
				     .brightness = 80 };
	unsigned int count, zone, color;

	assert(!asense_policy_zones_match(&zones, &zones, 0));
	assert(!asense_policy_zones_match(&zones, &zones, 5));
	for (count = 1; count <= 4; count++) {
		assert(asense_policy_zones_match(&zones, &zones, count));
		for (zone = 0; zone < count; zone++)
			for (color = 0; color < 3; color++) {
				struct asense_zones wrong = zones;

				wrong.rgb[zone][color]++;
				assert(!asense_policy_zones_match(&zones, &wrong, count));
			}
	}
}

int main(void)
{
	boot_records_are_readable_but_not_writable();
	static_tolerance_never_relaxes_dynamic_modes();
	global_static_uses_the_requested_color_and_strict_readback();
	all_supported_zone_counts_verify_each_visible_color();
	return 0;
}
