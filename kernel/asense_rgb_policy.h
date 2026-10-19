/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef ASENSE_RGB_POLICY_H
#define ASENSE_RGB_POLICY_H

#ifdef __KERNEL__
#include <linux/types.h>
#else
#include <stdbool.h>
#include <stdint.h>
typedef uint8_t u8;
#endif

#ifndef ASENSE_STATIC_ENGINE
#define ASENSE_STATIC_ENGINE 0x00
#endif
#ifndef ASENSE_FOUR_ZONE_ENGINE
#define ASENSE_FOUR_ZONE_ENGINE 0x03
#endif

#define ASENSE_ZONE_FLAG_FOUR_ZONE_ENGINE (1U << 1)
#define ASENSE_ZONE_FLAG_STALE_STATIC (1U << 2)
#define ASENSE_ZONE_FLAG_BOOT_WAVE_ZERO (1U << 3)
#define ASENSE_ZONE_FLAG_GLOBAL_STATIC (1U << 4)
#define ASENSE_ZONE_FLAG_STATIC_COLOR_SHADOW (1U << 5)

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

static inline u8 asense_policy_engine(u8 flags, u8 mode)
{
	if (flags & ASENSE_ZONE_FLAG_FOUR_ZONE_ENGINE)
		return ASENSE_FOUR_ZONE_ENGINE;
	return mode == 0 ? ASENSE_STATIC_ENGINE : ASENSE_FOUR_ZONE_ENGINE;
}

static inline struct asense_effect asense_policy_static_effect(u8 flags,
						 const struct asense_zones *zones)
{
	struct asense_effect effect = {
		.mode = 0,
		.brightness = zones->brightness,
		.engine = asense_policy_engine(flags, 0),
	};

	if (flags & ASENSE_ZONE_FLAG_GLOBAL_STATIC) {
		effect.red = zones->rgb[0][0];
		effect.green = zones->rgb[0][1];
		effect.blue = zones->rgb[0][2];
	}
	return effect;
}

static inline bool asense_policy_write_valid(u8 flags,
					    const struct asense_effect *effect)
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
	return effect->engine == asense_policy_engine(flags, effect->mode);
}

static inline bool asense_policy_read_valid(u8 flags,
					   const struct asense_effect *effect)
{
	if (asense_policy_write_valid(flags, effect))
		return true;
	/* AN17-51 retains bounded dynamic parameters in a static readback. */
	if ((flags & ASENSE_ZONE_FLAG_STALE_STATIC) && effect->mode == 0 &&
	    effect->speed <= 9 && effect->direction <= 2 &&
	    effect->brightness <= 100 &&
	    effect->engine == asense_policy_engine(flags, 0))
		return true;
	/* Exact AN14-41 boot records from issue #9; never a write encoding. */
	return (flags & ASENSE_ZONE_FLAG_BOOT_WAVE_ZERO) &&
	       effect->mode == 3 && effect->speed == 0 &&
	       effect->direction == 0 && effect->brightness <= 100 &&
	       effect->red == 0 && effect->green == 0 && effect->blue == 0 &&
	       (effect->engine == ASENSE_STATIC_ENGINE ||
		effect->engine == ASENSE_FOUR_ZONE_ENGINE);
}

static inline bool asense_policy_effect_matches(u8 flags,
					const struct asense_effect *expected,
					const struct asense_effect *actual,
					bool zone_transaction)
{
	bool stale_static = (flags & ASENSE_ZONE_FLAG_STALE_STATIC) &&
			    expected->mode == 0;
	bool color_shadow = zone_transaction && expected->mode == 0 &&
			    (flags & ASENSE_ZONE_FLAG_STATIC_COLOR_SHADOW);

	if (!asense_policy_write_valid(flags, expected) ||
	    !asense_policy_read_valid(flags, actual))
		return false;
	if (expected->mode != actual->mode ||
	    expected->brightness != actual->brightness ||
	    expected->engine != actual->engine)
		return false;
	if (!stale_static && (expected->speed != actual->speed ||
			     expected->direction != actual->direction))
		return false;
	if (!color_shadow && (expected->red != actual->red ||
			     expected->green != actual->green ||
			     expected->blue != actual->blue))
		return false;
	return true;
}

static inline bool asense_policy_zones_match(const struct asense_zones *expected,
					    const struct asense_zones *actual,
					    unsigned int count)
{
	unsigned int zone, color;

	if (!count || count > 4 || expected->brightness > 100 ||
	    expected->brightness != actual->brightness)
		return false;
	for (zone = 0; zone < count; zone++)
		for (color = 0; color < 3; color++)
			if (expected->rgb[zone][color] != actual->rgb[zone][color])
				return false;
	return true;
}

#endif
