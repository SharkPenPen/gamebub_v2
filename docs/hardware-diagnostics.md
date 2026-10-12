# Hardware diagnostics

What the boot log says, and what to do when a line is not what you expect.

This firmware is built so that a broken part can never stop the boot: every
check logs one line, and the device keeps going so that the rest of the report
can still be read. If the FPGA cannot be brought up at all, the device stays in
a diagnostics loop ([`device/test_hardware.rs`](../firmware/handheld/src/device/test_hardware.rs))
instead of dying with a blank screen.

## 1. Where the log comes from

| Part | Source | Where it appears |
|---|---|---|
| ① | chip ROM | UART0 **and** USB-Serial/JTAG (`ESP-ROM:...`, `rst:`, `boot:`, `mode:`, `load:`) |
| ② | second stage bootloader | **UART0 only** |
| ③ | ESP-IDF startup | UART0 and USB |
| ④ | this firmware | UART0 and USB |

* UART0 is `GPIO43` (TX) / `GPIO44` (RX) at 115200 baud.
* The USB-C port carries the ROM banner and the application log as the secondary
  console. The **second stage bootloader only speaks UART0**, so a device that
  produces nothing beyond the ROM lines can only be diagnosed on UART0.
* `sdkconfig.defaults` sets the console and log levels; `main.rs` raises the Rust
  log level to `Info` and quiets the `gpio` target.

## 2. The boot banner

```
I handheld: ################ Game Bub handheld ################
I handheld: # firmware    : 0.1.0
I handheld: # git commit  : 3f2a91c4
I handheld: # device rev  : Some(2)
I handheld: # serial      : Some("0001")
I handheld: # reset reason: POWERON
I handheld: ##################################################
```

| Field | Meaning |
|---|---|
| `firmware` | `Cargo.toml` version |
| `git commit` | commit embedded at build time by `build.rs` - **this is how you confirm the device runs the build you just flashed** |
| `device rev` | the revision stored in NVS by `flash_nvs.py`. `None` means NVS was never flashed; the firmware warns and continues (see below) |
| `serial` | serial number from NVS |
| `reset reason` | why the MCU last restarted, see the table |

### Reset reasons

| Value | Meaning |
|---|---|
| `POWERON` | normal power-on |
| `SW` | software reset (the device reboots after a settings change) |
| `PANIC` | the application panicked |
| `INT_WDT` / `TASK_WDT` / `WDT` | watchdog: something hung. **A repeating watchdog reason is the most important signal here** |
| `BROWNOUT` | the supply dropped |
| `USB` | reset by esptool over USB after flashing (normal) |
| `DEEPSLEEP`, `EXT_PIN`, `SDIO`, `JTAG`, `EFUSE`, `PWR_GLITCH`, `CPU_LOCKUP`, `UNKNOWN` | see Espressif's `esp_reset_reason_t`; `UNKNOWN` on a hand-built board usually means the reset happened before the reason register was meaningful |

If the ROM banner is followed by `rst:0x7 (TG0WDT_SYS_RST)` in a loop, the
second stage bootloader never started. Check the `mode:` field of the ROM line:
this project deliberately flashes a **DIO**-headed bootloader (`espflash.toml`),
and a merged image whose header was rewritten to QIO will not boot.

## 3. The self-test

```
I handheld::device::test_hardware: === Hardware Self-Test (non-fatal) ===
I [PWR] VBUS present: OK
I [PWR] Charging: no (battery full, or no battery fitted)
I [PWR] Fuel gauge MAX17048 (0x36): 4.20 V
I [BTN] Home / Vol+ / Vol- / Power all released: OK
I [IOEXP] TCA9535 (0x20) read OK, inputs = 0xFE1F
I [FPGA] INIT_B high (ready for configuration)
I [FPGA] INIT_B went low while PROGRAM_B asserted: OK
I [FPGA] INIT_B released high: OK
I [FPGA] DONE low (not configured yet, expected)
I [SYS] boot.bit.gz: found
I [SYS] gameboy.bit.gz: found
I [SYS] gba.bit.gz: found
I [SYS] gameboy.bios-dmg.bin: found
I [SYS] gameboy.bios-cgb.bin: found
I [SYS] gba.bios.bin: found
I [SD] mounted at /sdcard: OK (7 entries)
I [RTC] Time: 0000-01-01 00:00:48 (year 0 means it was never set)
I [IMU] chip id 0x69 accepted
I [IMU] Accel: X=0.000 Y=0.000 Z=0.000 g
I [DAC] Headset detected: false, short-circuit L=false R=false
I handheld: === Self-test complete: clean - boot continues ===
```

| Tag | Check | Failure means |
|---|---|---|
| `[PWR]` | VBUS, charging state, MAX17048 voltage | `read failed` -> I2C, pull-ups or soldering on the gauge |
| `[BTN]` | Home / Vol+ / Vol- / Power all released | a pin is stuck or shorted |
| `[IOEXP]` | TCA9535 answers and can be read | I2C problem; on revision 2 this part is also what identifies the board |
| `[FPGA]` | INIT_B high, INIT_B follows PROGRAM_B, INIT_B returns high, DONE low | power, reset wiring or the FPGA itself |
| `[SYS]` | the three bitstreams and the BIOS files are present | a missing bitstream is why the FPGA would stay unconfigured |
| `[SD]` | the card mounts and `/sdcard` can be listed | card or SDIO wiring |
| `[RTC]` | the clock answers and holds a valid date | I2C; year 0 means 2000 and simply never set |
| `[IMU]` | `WHO_AM_I` is accepted, then a sample is read | a failed read only costs motion-controlled games |
| `[DAC]` | headphone detect, short-circuit flags | a reported short means the speaker wiring |

Notes:

* The IMU accepts `0x6A` (LSM6DS3TR-C, the part the board was designed with) and
  `0x69` (LSM6DS3, which some boards are populated with - the registers used here
  are identical). `[IMU] chip id 0x69 accepted` is informational.
* The FPGA checks drive `PROGRAM_B` themselves, which also resets the
  configuration logic. That is safe before programming and is exactly what the
  programming sequence does a moment later.
* The summary line is `[PWR]`-tag free by design: `=== Self-test complete: N
  failure(s), M warning(s) - the device may still work; see the tagged lines
  above ===`. Warnings are things a working device can have (no USB attached, no
  headset, an unset clock); failures are things that will not work.

## 4. Diagnostics hold mode

When the FPGA cannot be brought up (a bitstream that cannot be read or does not
program), the firmware does not try to draw a UI - the FPGA drives the LCD, so
there would be nothing to show. Instead:

```
E ============================================================
E [BRINGUP] FPGA BRING-UP FAILED - staying alive in diagnostics
E [BRINGUP] reason: <what failed>
E [BRINGUP] the FPGA drives the LCD, so the screen stays blank
E [BRINGUP] the report below repeats every 5 s on the serial console
E [BRINGUP] hold the power button for 3 s to switch off
E ============================================================
I --- [BRINGUP] diagnostic report (FPGA not running) ---
```

| Behaviour | Detail |
|---|---|
| Status LED | blinks at 1 Hz, so a board without a console is visibly alive |
| Report | the whole self-test repeats every 5 seconds |
| Power off | hold the power button for 3 seconds |
| Battery | below the cutoff with no USB, the device powers itself off rather than draining further |

A missing SD card deliberately does **not** take this path: without a card the
MCU can still drive the LCD, so that case keeps its on-screen error message.

## 5. Low-voltage interlock

`power.rs` holds the single cutoff constant (3.3 V). During `Device::init`, if
the battery is below it while VBUS is absent, the FPGA is left unpowered:

```
E [PWR] PROTECTION: battery at 3.10 V is below the 3.30 V cutoff and VBUS is absent - leaving the FPGA off
```

A reading below 1 V is ignored on purpose: the MAX17048 answers with a zeroed
register for a while after power-up, and acting on that would refuse to start a
healthy device.

## 6. SD card layout

The firmware looks for system files in `/sdcard/system_rev2/` first and falls
back to `/sdcard/system/`:

```
system_rev2/           (or system/)
  boot.bit.gz
  gameboy.bit.gz
  gba.bit.gz
  gameboy.bios-dmg.bin
  gameboy.bios-cgb.bin
  gba.bios.bin
roms/
```

The three bitstreams must match the register layout this firmware speaks. The
upstream **v0.1 release** assets are built for revision 2 hardware and are the
matching generation; bitstreams from later upstream releases target a different
framework and a different memory map.

## 7. Cheat sheet

| What you see | What it means | What to do |
|---|---|---|
| Only the ROM lines, then `rst:0x7` in a loop | the second stage bootloader never ran | check `mode:` in the ROM line; flash a DIO-headed image (`espflash.toml`) |
| `# device rev : None` | NVS was never flashed | run `flash_nvs.py --serial ... --revision 2`; the firmware continues with a warning |
| `[SYS] device reports revision ... - continuing` | built-for and reported revision differ | confirm you flashed the build for this board |
| `[PWR] Fuel gauge ... read failed` in the first seconds | the gauge settles late | fine if a voltage appears afterwards |
| `[PWR] PROTECTION: ... leaving the FPGA off` | flat battery, no USB | charge it; the FPGA is off by design |
| `[IMU] WHO_AM_I read failed ... attempt n/5` | the IMU does not answer | I2C or soldering; motion games will not work |
| `[IMU] accelerometer read failed` | the ID was accepted but the sample failed | same wiring, less severe |
| `[SYS] *.bit.gz: not found` | the FPGA cannot be configured | copy the bitstreams to `/sdcard/system_rev2/` or `/sdcard/system/` |
| `[FPGA] INIT_B low` | the FPGA is not ready | FPGA power and reset |
| `[FPGA] INIT_B did not follow PROGRAM_B` | the configuration handshake does not work | `PROGRAM_B`/`INIT_B` wiring or the FPGA |
| `[FPGA] DONE is already high before programming` | unexpected state | check what else configured the FPGA |
| `[SD] not mounted` | no card or no SDIO | reseat the card, check the socket |
| `[IOEXP] ... read failed` | I2C problem | the expander also carries the game buttons |
| `[LCD] initialization failed` | the panel did not come up | the UI will not be visible |
| `[DAC] bring-up incomplete` | the codec did not configure | no audio; check I2C and the reset line |
| `[RTC] Time: 0000-01-01` | never set (year 0 = 2000) | set the time in the settings menu |
| `[BRINGUP] FPGA BRING-UP FAILED` | see `reason:` and the report below it | the report repeats every 5 s |
| `=== Self-test complete: N failure(s) ...` | at least one `[tag]` line above reported a problem | read the tagged lines |
| `UI thread is gone, dropping a message` | the UI thread stopped | check for a preceding panic |
| `[IRQ] could not subscribe ...` | an input cannot interrupt | that button or the IRQ line will not respond |
