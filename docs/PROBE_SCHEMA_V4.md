# ASense probe schema 4

ASense v0.4.0 emits report schema 4 and passive daemon diagnostic schema 2.
Runtime commands continue to negotiate `HELLO 2`. The bounded collection,
typed observations and privacy contract from [schema 3](PROBE_SCHEMA_V3.md)
remain in force with these changes:

- HID role `darfon-lighting-candidate` identifies USB `0d62:ba51`, with the
  canonical name `Darfon USB HID`. Collection reads only its sysfs report
  descriptor, driver identity and decoded usage/feature geometry. It never
  opens that controller for a feature request, emits A1 evidence or runs an
  ENEK selector. Missing USB interface numbers remain `null`.
- Kernel ASCII `performance` may mean generic Performance when the handler
  is unidentified. The identified `acer-wmi` handler, the reference model and
  ASense Gaming-WMI still use their established Turbo interpretation. Gaming
  raw `05` is Turbo; `02` remains unknown. Generic kernel semantics cannot
  override a Gaming-WMI raw coordinate.
- Fan capability describes actual command eligibility. Missing RPM or
  temperatures does not remove Auto/Maximum. Manual requires both valid
  CPU/GPU watchdog temperatures and readable speed setpoints. Per-read errors
  remain independent observations.

The current reader reopens and validates schema-3 reports without rewriting
their historical meanings. Schema-3 documents cannot contain the new USB role
or the new generic Performance interpretation. Passive diagnostic schema 1
is accepted when it contains only its original roles. Unknown schema numbers
and invented role/VID/PID/bus combinations are rejected.

An older v0.3.0 client cannot read diagnostic schema 2 and reports an
incompatible response rather than falling back to active discovery. Upgrade
the GUI and daemon together through one package. New clients can inspect the
old diagnostic schema 1 while preserving its actual reported semantics.

The JSON byte bound remains 262144; the daemon payload bound remains 32764.
There are no new identity fields, raw-payload allowances, default writes,
automatic uploads or persistent report IDs. New JSON and its summary are
generated from the same typed report. Community schema-3 fixtures remain
unchanged and continue to pass their exact SHA-256 and round-trip checks.
