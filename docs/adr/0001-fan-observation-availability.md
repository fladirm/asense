# Preserve unavailable fan observations

ASense 0.4 separates fan-control transport, thermal safety and readable state.
Community reports demonstrate working Auto/Maximum without RPM, while the
reference PHN16-72 can return ENXIO for a mode getter before a valid write.
Missing or unreadable telemetry must not be reported as a measured zero or
used to discard an independently usable control interface.

`hardware::FanChannelState::rpm` and `pwm_raw` therefore become `Option<u32>`
and `Option<u8>`. `pwm_percent()` returns `Option<f32>`. Source consumers must
handle `None` as unavailable; `Some(0)` remains a real zero observation.
The desktop retains its last user-requested slider values when the setpoint
is unavailable. It does not manufacture a new zero-percent request.

Kernel Auto/Maximum require the two standard mode attributes and a known mode
or the specifically documented ENXIO getter limitation. Manual additionally
requires readable bounded setpoints and valid CPU/GPU watchdog temperatures.
Every mode write still requires explicit successful mode readback. Unrelated
errors, invalid mode values and failed verification are not accepted.

This changes the Rust source API for the 0.4 minor release. Runtime protocol 2
and the existing optional RPM fields in CAPS remain compatible. Passive
diagnostic schema 2 and probe schema 4 retain distinct absent/unknown/error
observations, while their new readers accept the original diagnostic/report
schemas. Optional setpoint read errors cannot invalidate an independently
verified Auto/Maximum mode; Manual setpoint verification remains strict.

`telemetry::PowerSupplyTelemetry` additionally carries `read_error`. Adapter
aggregation preserves an unknown reading when no source is confirmed online,
and accepts only the standard online values 0/1. A confirmed online source can
coexist with a read-error flag on another source.

`daemon::failsafe_auto()` returns `FailsafeFanReset::{Verified, NotExposed}`.
The CLI returns exit 3 for NotExposed, preserving a separate outcome from
verified Auto (0) and an actual failure (1). Removal may finish on a machine
without fan control, but must not claim a firmware reset was confirmed.
An exposed but unreadable or invalid control is a failure, not NotExposed.
