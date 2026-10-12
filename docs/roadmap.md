# Roadmap (revision 2 hardware)

Hardware is locked to **revision 2**. Everything below assumes that.

## Where this fork stands

| | |
|---|---|
| Firmware base | upstream `6368eca` (2025-02-18, `git describe` = `v0.1-12`), imported as a firmware-only snapshot |
| Added on top | power-on banner, non-fatal hardware self-test, `hold_alive` diagnostics, low-voltage interlock, non-fatal bring-up, root CI |
| Bitstreams that can run on it | **only** the upstream `v0.1` release assets - they are the only revision 2 bitstreams ever published |
| Newer upstream (1.0.2) | ships `gamebub-rev4_v1.0.x.uf2` only, and its firmware speaks the *framework* register set with a core command channel; it cannot drive a `v0.1` bitstream |

The last row is the whole reason for the phase order below: on this board the firmware
generation and the bitstream generation have to match, and the bitstream side is the one
that cannot be rebuilt here (that needs Vivado plus the `fpga/verilog/hdmi` sources).

## Phase 0 - a baseline you can always roll back to

Do this before flashing anything. It is the difference between "it does not boot" and
"it does not boot, and here is the exact combination that did".

1. **Make the three bitstreams the same generation.** The SD card in use has a mixed set:

   | file | bytes | sha256 | |
   |---|---|---|---|
   | `system/boot.bit.gz` | 190,775 | `1bd179fd5db85a1cbbe67fb2ac43d3b8fac26325a46e2d62d8eb83fee73aba59` | official `v0.1` |
   | `system/gameboy.bit.gz` | 288,624 | `b0b4355f28e67195a9cc5d49244ec4fc35a724a7f5372644a0657f0c5df32711` | official `v0.1` |
   | `system/gba.bit.gz` | 727,632 | `4df994efbf69a54141d127de3f6e22ead5868157ecbacea5938c86bdca3797b2` | **not** `v0.1`, unknown generation |
   | `system/gba.bit备份.gz` | 712,060 | `6e30a8607e41c9ecffbea628839d7fb4782edb595600f4bad7b45dbe854a8384` | official `v0.1` |

   Replace `system/gba.bit.gz` with `gba.bit备份.gz` (keep the name `gba.bit.gz`), or
   re-download `https://github.com/elipsitz/gamebub/releases/download/v0.1/gba.bit.gz`.
   A mixed set is a plausible cause of the earlier "failed version".

2. **Keep a restore image.** `firmware_merged.bin` on the desktop (4 MiB) contains the
   revision 2 factory firmware (`Device revision`, `system_rev2`, `bit.gz` strings), so a
   full 0x0 write restores a known state. Store it next to the repository, not in it.

3. **Record the NVS state** (serial and revision) before changing it, so it can be put back.

4. **Save the SD card contents** (bitstreams, BIOS files, ROMs, saves) before touching them.

Acceptance for phase 0: the three bitstream files match the hashes above, and a restore
image plus the current NVS values are written down.

## Phase 1 - get the current firmware running

1. Build and flash what is in this repository:
   `cargo run --release --features=rev2` from `firmware/handheld`, then
   `python3 flash_nvs.py --serial <serial> --revision 2` (the firmware now boots even
   without NVS, so this can also be done afterwards).
2. Capture the boot log over UART0 (`GPIO43`/`GPIO44`, 115200) or USB-C, and read it with
   [hardware-diagnostics.md](./hardware-diagnostics.md).
3. Fix only what the log actually reports. The self-test was written without a device, so
   its first real report is the useful one.

Acceptance for phase 1 - not "it powers on", but:

```
=== Hardware Self-Test (non-fatal) ===
[PWR] ... [BTN] ... [IOEXP] ... [FPGA] ... [SYS] ... [SD] ... [RTC] ... [IMU] ... [DAC] ...
=== Self-test complete: clean - boot continues ===
Programmed FPGA, done=true
[UI] Starting UI
```

Known risk points, in the order they would show up: SDIO mount, ILI9488 init, FPGA
`INIT_B` timing, the MAX17048 reading zero for the first seconds (handled: readings below
1 V are ignored), and I2C parts whose `WHO_AM_I` differs from the design (handled: the IMU
accepts `0x69` and `0x6A`).

## Phase 2 - features, chosen so they cannot break the FPGA handshake

Only features that live entirely on the ESP32 side. Upstream paths are from the current
`handheld` branch, which is a porting *source*, not a new base.

| # | Feature | Upstream source | Value | Risk |
|---|---|---|---|---|
| ~~1~~ | ~~USB device stack: SD card as mass storage, plus a CDC log stream~~ | `device/drivers/usb/{mod,control,descriptors,cdc_stream}.rs` | **declined** - see the decision below | - |
| 2 | Save-file management (backup/restore to SD) | `cart_backup.rs` | high | low |
| 3 | LCD split, backlight driver module | `device/drivers/ili9488.rs`, `lcd_backlight.rs` | medium (structure) | low |
| 4 | UI: notifications, setup wizard, screenshot to SD | `ui/state/notifications.rs`, `ui/state/setup.rs`, `ui/screenshot.rs` | medium-high | low |
| 5 | Panic handler that logs and keeps the console alive | `crash_handler.rs` | medium (a panic is currently silent) | low |
| 6 | Firmware/hardware info helpers | `fwinfo.rs`, `hwinfo.rs` | medium - note the rev2 eFuse is normally unprogrammed, which is why the banner uses NVS | low |
| 7 | Module structure: `bitstream/gameboy.rs` -> `gameboy/`, `drivers/fpga.rs` -> `fpga/`, `fuel_gauge.rs` -> `max17048.rs`, `lcd.rs` -> `ili9488.rs` | same paths upstream | medium (structure) | low |
| 8 | DMG palette / colour-correction helpers | `bitstream/gameboy/dmg_palette.rs`, `bitstream/util/color_correction.rs` | medium | **check first**: if these write framework-era FPGA registers, they are not portable |

**Decision on item 1 (recorded).** The USB device stack is **not wanted**. The
USB-Serial/JTAG console that `sdkconfig.defaults` enables is what makes the boot log
readable over USB-C with nothing but a cable, and that is worth more than mass storage on
this board - they share the same USB pins, so it is one or the other. Logs are read with a
serial terminal; ROMs and saves are moved with a card reader. Item 1 only comes back if
that trade-off is revisited.

**Not portable until a bitstream can be built.** Everything that talks to the framework
register set or the core command channel: `core/{mod,info,settings}.rs` (loading and
running a game core), `device/drivers/fpga/xilinx.rs` and the generation/address
compatibility layer, `ui/state/cores.rs` (the core browser), and the revision 3/4 hardware
drivers (`bq27427`, `ili9806e`, `st7262`). Porting these onto a `v0.1` bitstream
reproduces the failure the previous attempt ran into.

## Phase 3 - simplify and harden

* **Finish making it revision 2 only.** The `rev2` cargo feature still exists in the same
  shape as upstream's revision switch. Hard-code revision 2, drop the feature, and update
  `docs/building.md`, `.vscode/settings.json` and CI, so `cargo build --release` needs no
  flags.
* **Dead code.** Several drivers start with `#![allow(dead_code)]` (`lcd`, `fpga`,
  `fuel_gauge`, `imu`, `io_expander`, `rtc`, `dac`, `usb`). Remove the attribute file by
  file and delete what it was hiding.
* **CI gate.** 56 upstream warnings keep clippy advisory. Clear them, then restore
  `-- -D warnings` so new code cannot add warnings silently.
* **`.gitattributes`** with `* text=auto eol=lf`, to stop the LF/CRLF warnings and make the
  Windows checkout and the Linux runner agree.
* **README**: add a short "building and flashing this fork" section plus the bitstream
  manifest from phase 0.
* **Optional**: register the `hdmi` submodule pinned at
  `380c974933ce0221aeed8bbf8f667d2dd0027961`, so a future bitstream build is reproducible.

## Why this order

The three steps as stated (run it, add features, simplify) are right, with four changes:

1. **Insert phase 0.** It is cheap, and the earlier failure shows the cost of not having
   it: without a recorded known-good combination, every later symptom is ambiguous.
2. **Tighten phase 1's acceptance criterion** from "it boots" to "it prints a clean
   self-test", which the diagnostics added in this fork can actually demonstrate.
3. **Do not run phases 2 and 3 as two waterfalls.** Clean up the module you are about to
   change *while* you change it (items 3 and 7 above are exactly that), and leave the
   global refactor for phase 3. Smaller diffs, easier rollback.
4. **Add the decision gate.** Any feature that needs the framework register set or the
   core command channel waits until a revision 2 bitstream of that generation exists
   (Vivado 2023.2 + `fpga/verilog/hdmi` at `380c974` + the three `handheld_rev2` targets).
   Until then the `v0.1` base is the only one that can run on this board.

## Do these three things next

1. Swap `system/gba.bit.gz` for the `v0.1` copy and verify all three hashes (phase 0).
2. Download the `gamebub-rev2-firmware` artifact from the latest Actions run, write it at
   `0x0` (see [building.md](./building.md)) and capture one full boot log - over USB-C or
   UART0. No local toolchain is needed for either step.
3. Read that log against [hardware-diagnostics.md](./hardware-diagnostics.md) and fix only
   what it reports; phase 2 starts once the self-test comes back clean.
