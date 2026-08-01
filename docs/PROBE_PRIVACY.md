# ASense probe privacy and support workflow

`asense probe` creates a local, bounded compatibility report. It does not
upload the report or contact a remote service. The schema-3 JSON is the support
authority; the human-readable summary is only a derived convenience view.

## Commands

Close the ASense window so the one-shot command can use the daemon's single
control session, then run:

```bash
asense probe > asense-probe.json
asense probe --summary
```

Run the two commands consecutively without changing the power source, active
profile or fan mode. Review `asense-probe.json` before attaching it to an
issue. The summary performs a fresh capture and intentionally omits bounded raw
payloads and descriptor hashes; it does not replace the JSON attachment.

## Default read-only path

The default probe:

1. reads bounded local DMI model/board/BIOS, kernel, distribution, power-supply,
   module, known WMI-group and hwmon ownership data;
2. negotiates `HELLO 2` with the local root daemon;
3. requests only the fixed `DIAG PASSIVE` diagnostic response;
4. allows only fixed read operations over known kernel attributes and exact
   supported HID identities, including bounded HID descriptor and ENEK5130 A1
   get-feature reads;
5. validates and size-bounds the complete typed report before printing it.

It does not call `CAPS`, select an ENEK target with A2, send A3/A4 lighting
commands, write a profile/fan/platform control, use a generic EC/WMI/HID
executor, query NVIDIA hardware or wake a suspended discrete GPU. Schema 3
records `mode: "passive"`, `automatic_upload: false` and an empty
`default_mutations` list. A non-empty default mutation list is invalid.

## Included evidence

The report may contain:

- ASense version, embedded source commit when available and capture time;
- Acer vendor, product, board and BIOS labels;
- kernel release/architecture and distribution ID/version;
- anonymous ordinal AC/battery state;
- loaded ASense/Acer module and known driver ownership;
- typed profile, fan/RPM/temperature and platform-control read results;
- known zoned-WMI lighting transport geometry;
- sanitized HID VID:PID, usage, descriptor geometry/hash and bounded known
  feature-report evidence.

Transport presence is not proof of a physical effect. Unknown firmware values
remain typed as unknown; read failures remain typed errors rather than being
turned into supported controls.

## Excluded identity classes

The collector does not read or serialize these twelve classes:

1. serial numbers;
2. UUIDs;
3. hostname;
4. user/account identity;
5. network identity or addresses;
6. boot ID;
7. storage identity;
8. HID serial numbers and physical device paths;
9. journals or system logs;
10. raw ACPI tables;
11. absolute device paths;
12. process environment.

It also creates no persistent report identifier.

## Bounded raw allow-list

Raw scalar evidence is rejected everywhere except these schema paths:

| Schema path | Maximum |
|---|---:|
| `profile.current.raw` | 48 bytes |
| `profile.choices.value[].transport_raw` | 1 byte |
| `profile.firmware_supported_bitmap.value.raw` | 1 byte |
| `platform.fields[].read.error.raw` | 8 bytes |
| `hid[].a1.value.payload` | 64 bytes |
| `hid[].extended.a3[].read.value.payload` | 64 bytes |

The default passive probe never requests extended HID evidence, so its
`extended_mutations` and A3 arrays remain empty. The allow-list is a schema
bound, not permission for a default write.

## Evidence synchronization

A useful compatibility or hardware bug report must describe the state that the
probe actually captured. Record the behavior and generate the report with:

- the same installed ASense build and daemon;
- the same boot, without a reboot in between;
- the same loaded kernel-module state, without unload/reload in between;
- the same AC/battery source;
- the same requested profile, fan and lighting state relevant to the report.

If any of these changes, reproduce the behavior and capture a new report. State
whether suspend/resume or a live AC plug/unplug occurred. Screenshots and prose
without synchronized schema-3 evidence are useful observations, but they are
not sufficient authority for a new hardware mapping.
