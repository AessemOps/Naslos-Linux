# GamakayLinux: Rust replacement for the Gamakay driver app (LK67)

Target repo: `/home/aessem/Documents/GamakayLinux` (empty git repo). License: **GPL-3.0-or-later**.
(This plan was saved in the Naslos repo because of plan-mode write limits. Copy it into the target repo's `.kilo/plans/` as the first commit.)

## Goal
Use a Linux-native Rust app to configure the GamaKay LK67 over USB cable, the 2.4G dongle and Bluetooth. Wireless support is limited to what the firmware actually accepts (verified in Phase 0). Runtime overhead must be minimal.

## Decisions
- **v1 features:** device info and battery, lighting (onboard modes, colors, brightness/speed, per-key static colors), key remap on the onboard layers, macros. **No firmware flashing**: there is no code path for it, and any flash/bootloader command IDs are blocklisted.
- **No resident process by default.** Every setting is written to keyboard memory (`support_onboard: 2`), then the program exits. Host-driven effects (visualizer etc.) are deferred to an optional, off-by-default `gamakay daemon` in v2, cable only.
- **Interfaces:** a `gamakay-core` library, a `gamakay` CLI and an **iced** GUI that only runs while open.
- **Transport:** Linux **hidraw** directly (`nix` ioctls `HIDIOCSFEATURE`/`HIDIOCGFEATURE` plus read/write, with sysfs enumeration). No libusb, so the kernel driver is never detached and typing keeps working. The same path covers BT, because BlueZ exposes HOGP devices as hidraw through uhid. No libudev dependency.
- **Device table is data-driven**, modelled on the vendor table. Only the LK67 is tested and supported.
- **Clean-room hygiene:** the vendor `.asar`, JS and assets are extracted into a gitignored `vendor/` and never committed. Only our own protocol notes go into `docs/protocol.md`. The GPL OpenRGB LK67 fork (gitlab.com/kuba.dajek4/open-rgb-gamakay-lk67) may be ported, with an attribution header and a `CREDITS.md` entry.

## Known facts (verify in Phase 0)
- Cable: USB `0461:4002` (vid 1121 / pid 16386), keyboard collection usage page 1 / usage 6, `featureReportByteLength: 65`, `layer: 8`, vendor table name `yzw_lk67`, company `gamakay2`, vendor id 149. Some Banggood units report id 71 (wrong firmware).
- The official app is Electron, so its protocol encoders can be read straight from `main.js`/`index.js` after `npx @electron/asar extract`.
- The dongle VID/PID, BT HID descriptor and the vendor report layout over wireless are unknown.

## Layout
```
Cargo.toml                 # workspace; release: lto, codegen-units=1, strip
crates/gamakay-core/       # protocol encode/decode, device table, Transport trait, models, safety guard
crates/gamakay-hidraw/     # Linux hidraw Transport + enumeration + hotplug (only used by the GUI)
crates/gamakay-cli/        # `gamakay` binary (clap)
crates/gamakay-gui/        # iced app
docs/protocol.md           # command reference with byte layouts + transport support matrix
docs/re-workflow.md        # how to capture/unpack/verify
tests/fixtures/            # decoded packet fixtures (hex), NOT raw pcaps
packaging/udev/70-gamakay.rules   # TAG+="uaccess" for cable + dongle (+ BT hidraw match)
packaging/arch/PKGBUILD    # CachyOS/Arch
CREDITS.md, LICENSE, README.md, .gitignore (vendor/, *.pcapng)
```

## Tasks

### Phase 0: Reconnaissance (no writes to the keyboard from our code)
1. On the Linux host, record `lsusb -v -d 0461:4002`, the hidraw report descriptors (`hid-decode` from hid-tools) for the cable, the dongle and the BT pairing, and the interface/collection that carries vendor feature reports (expect a vendor usage page `0xFFxx`). Note the dongle VID/PID.
2. Extract the official app (latest from gamakay.com/pages/downloads) into `vendor/`. Locate the device table, the transport-selection logic (does it list dongle/BT PIDs?), the packet builders (header, command IDs, checksum, chunking for per-key/keymap/macro data) and the firmware-update commands to blocklist. Write them into `docs/protocol.md`.
3. Transport matrix: in the Windows VM, run the official app over (a) cable passthrough, (b) dongle passthrough, (c) the BT adapter passed through to the VM. Record which features work on each transport.
4. Captures: run Wireshark/usbmon **on the Linux host** against the passed-through device, since QEMU's usb-host goes through usbfs and usbmon sees the URBs. For BT, capture the passed-through adapter and decode the HCI/ATT writes. Change one setting per capture using distinctive values (#FF0000, 0, 100). Use the captures to confirm the JS-derived layouts, and save decoded hex as fixtures.
5. Document how to recover the keyboard (factory-reset key combo, behaviour after a bad write) before Phase 2 sends any writes.
6. Output: `docs/protocol.md` with every v1 command plus the transport matrix. **Gate:** every write command used in v1 has a confirming capture.

### Phase 1: Core, transport, read-only features
7. Workspace skeleton, GPL-3.0 headers, CI (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`).
8. `Transport` trait (`send_feature`, `get_feature`, `write`, `read_timeout`) plus a `MockTransport` that replays fixtures.
9. hidraw backend: enumerate via `/sys/class/hidraw/*/device/uevent` and the report descriptor, and select the vendor collection per transport (cable/dongle/BT).
10. Safety guard in core: only allowlisted command IDs from `docs/protocol.md` can be sent. Validate length and checksum before sending. Provide a global `--dry-run`, which prints packets.
11. Read commands: identify (vendor id, firmware version, connection mode, battery). Warn when the id is not 149, and refuse writes unless `--force` is given.
12. CLI: `gamakay list`, `gamakay info`. udev rule.

### Phase 2: Lighting
13. Models plus encoders for onboard modes, brightness/speed/direction, and a color per mode. Encode per-key static colors using the LK67 LED index map.
14. CLI `gamakay light ...`. Profiles are stored as TOML (`~/.config/gamakay/profiles/*.toml`), applied with `gamakay apply <profile>`.
15. Read back after writing where the protocol allows it.

### Phase 3: Key remap and layers
16. Keycode table (vendor codes ↔ HID usages / names), the LK67 physical layout, read/write per layer, and reset-to-default.
17. CLI `gamakay keymap get|set|reset`, plus the same in TOML profiles.

### Phase 4: Macros
18. Macro encoding (key down/up, delays, repeat modes) within the onboard storage limits, which are enforced before sending. Assigning a macro to a key goes through the keymap.
19. CLI `gamakay macro list|set|delete`.

### Phase 5: iced GUI
20. Keyboard canvas (layout from the device table), plus tabs for Lighting, Keymap (per layer), Macros and Info.
21. Event-driven only. Watch hotplug with inotify on `/dev` while the window is open. Query the battery on demand, or every 60 s at most while visible. No timers or redraws when idle. Device I/O runs on a worker thread.
22. Unsupported features for the current transport (from the matrix) are disabled with an explanation.

### Phase 6: Packaging and docs
23. PKGBUILD (installs the binaries, udev rule and .desktop file). README covering setup, udev and the transport matrix, plus CREDITS.

### Phase 7 (v2, optional): `gamakay daemon`
24. Opt-in, cable only, host-streamed per-key frames. Off by default and never auto-started.

## Performance targets
- CLI: one-shot. No background threads after exit, no polling loops.
- GUI: ~0% CPU when idle, and modest RSS (measure with `/usr/bin/time -v` and `top`). Closing the window ends the process completely.
- Nothing is installed as a service by default.

## Validation
- Unit tests: encoder/decoder round-trips against the Phase 0 fixtures, plus guard tests (blocklisted IDs are rejected, bad lengths are rejected).
- On-device drill per phase, for each supported transport in the matrix: set a value, read it back, power-cycle, confirm it persisted, and confirm typing never stops during writes.
- Cross-check: values written by our tool show up correctly in the official app in the VM, and the reverse.
- Measure performance before release.

## Risks
- **Bricking or corrupting config:** mitigated by the allowlist, capture-confirmed writes only, no flashing, and the reset procedure documented first.
- **Wireless may be typing-only:** handled by degrading per the transport matrix rather than guessing packets.
- **Firmware variants** (id 71 vs 149): detect them and warn. Only proceed with known ids, unless `--force` is given.
- **The BT vendor collection may be missing from the HOGP descriptor:** this is detected in Phase 0. If it is missing, BT support is info/battery only, via the standard Battery Service.
- **VM passthrough:** while the device is passed through, the host loses it. Only test the Linux tool with the device detached from the VM.
