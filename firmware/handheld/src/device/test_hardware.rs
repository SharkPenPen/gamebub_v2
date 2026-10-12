//! Non-fatal hardware self-test.
//!
//! Nothing in here panics, returns early or propagates an error: a check that
//! fails only bumps a counter and logs a line. That way a half-populated board
//! still prints its complete report instead of dying on the first missing part,
//! which is what makes the boot log worth reading at all.
//!
//! Call [`run_tests`] from `main` after `Device::init`.

use std::fmt::{self, Display};
use std::time::Duration;

use embedded_hal::digital::InputPin;

use crate::util;

use super::Device;

/// Result of one self-test run.
#[derive(Default, Copy, Clone)]
pub struct Summary {
    pub failures: u32,
    pub warnings: u32,
}

impl Summary {
    pub fn is_clean(&self) -> bool {
        self.failures == 0 && self.warnings == 0
    }

    fn warn(&mut self, message: &str) {
        self.warnings += 1;
        log::warn!("{}", message);
    }

    fn fail(&mut self, message: &str) {
        self.failures += 1;
        log::warn!("{}", message);
    }
}

impl Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failure(s), {} warning(s)", self.failures, self.warnings)
    }
}

/// Read an input pin, reporting a driver error as "not high".
fn pin_is_high<P: InputPin>(pin: &mut P) -> bool {
    pin.is_high().unwrap_or(false)
}

/// Pack I/O expander inputs into a word, for logging.
fn pins_to_u16(pins: [bool; 16]) -> u16 {
    pins.iter()
        .enumerate()
        .fold(0u16, |value, (index, &high)| {
            if high {
                value | (1 << index)
            } else {
                value
            }
        })
}

/// Run every hardware check, logging one line per check.
pub fn run_tests(device: &mut Device<'_>) -> Summary {
    let mut summary = Summary::default();
    log::info!("=== Hardware Self-Test (non-fatal) ===");

    check_power(device, &mut summary);
    check_buttons(device, &mut summary);
    check_io_expander(device, &mut summary);
    check_fpga(device, &mut summary);
    check_system_files(&mut summary);
    check_sdcard(device, &mut summary);
    check_rtc(device, &mut summary);
    check_imu(device, &mut summary);
    check_dac(device, &mut summary);

    summary
}

/// USB power, charging state and the battery gauge.
fn check_power(device: &mut Device<'_>, summary: &mut Summary) {
    if device.get_vbus_pgood() {
        log::info!("[PWR] VBUS present: OK");
    } else {
        summary.warn("[PWR] VBUS absent (running from the battery)");
    }

    if device.get_battery_is_charging() {
        log::info!("[PWR] Charging: yes");
    } else {
        summary.warn("[PWR] Charging: no (battery full, or no battery fitted)");
    }

    match device.fuel_gauge.get_battery_voltage() {
        Ok(voltage) => log::info!("[PWR] Fuel gauge MAX17048 (0x36): {:.2} V", voltage),
        Err(_) => summary.fail(
            "[PWR] Fuel gauge MAX17048 (0x36) read failed: check I2C, the pull-ups and the soldering",
        ),
    }
}

/// The four directly-wired buttons must all read as released at boot.
fn check_buttons(device: &mut Device<'_>, summary: &mut Summary) {
    let home = pin_is_high(&mut device.button_home);
    let vol_up = pin_is_high(&mut device.button_vol_up);
    let vol_down = pin_is_high(&mut device.button_vol_down);
    let power = pin_is_high(&mut device.button_power);
    if home && vol_up && vol_down && power {
        log::info!("[BTN] Home / Vol+ / Vol- / Power all released: OK");
    } else {
        summary.fail(&format!(
            "[BTN] a button reads as pressed at boot (home={home} vol_up={vol_up} vol_down={vol_down} power={power}): check for a stuck pin or a short"
        ));
    }
}

/// The I/O expander also carries most of the game buttons on this board.
fn check_io_expander(device: &mut Device<'_>, summary: &mut Summary) {
    match device.io_expander.get_pins() {
        Ok(pins) => log::info!(
            "[IOEXP] TCA9535 (0x20) read OK, inputs = 0x{:04X}",
            pins_to_u16(pins)
        ),
        Err(_) => summary.fail(
            "[IOEXP] TCA9535 (0x20) read failed: check I2C (it is also what identifies revision 2 hardware)",
        ),
    }
}

/// The FPGA configuration pins, before any bitstream has been loaded.
fn check_fpga(device: &mut Device<'_>, summary: &mut Summary) {
    match device.fpga.init_b_is_high() {
        Ok(true) => log::info!("[FPGA] INIT_B high (ready for configuration)"),
        Ok(false) => summary.fail("[FPGA] INIT_B low: the FPGA is not ready to be configured"),
        Err(_) => summary.fail("[FPGA] INIT_B could not be read"),
    }

    // Asserting PROGRAM_B also resets the configuration logic, exactly like the
    // programming sequence does later, so this is safe to do here.
    if device.fpga.set_program_b(true).is_err() {
        summary.fail("[FPGA] PROGRAM_B could not be driven low");
        return;
    }
    std::thread::sleep(Duration::from_millis(1));
    match device.fpga.init_b_is_high() {
        Ok(false) => log::info!("[FPGA] INIT_B went low while PROGRAM_B asserted: OK"),
        Ok(true) => {
            summary.fail("[FPGA] INIT_B did not follow PROGRAM_B: check the wiring or the FPGA")
        }
        Err(_) => summary.fail("[FPGA] INIT_B could not be read while PROGRAM_B was asserted"),
    }

    if device.fpga.set_program_b(false).is_err() {
        summary.fail("[FPGA] PROGRAM_B could not be released");
        return;
    }
    std::thread::sleep(Duration::from_millis(5));
    match device.fpga.init_b_is_high() {
        Ok(true) => log::info!("[FPGA] INIT_B released high: OK"),
        Ok(false) => summary.fail("[FPGA] INIT_B did not return high after PROGRAM_B was released"),
        Err(_) => summary.fail("[FPGA] INIT_B could not be read after PROGRAM_B was released"),
    }

    match device.fpga.done_is_high() {
        Ok(false) => log::info!("[FPGA] DONE low (not configured yet, expected)"),
        Ok(true) => summary.warn("[FPGA] DONE is already high before programming (unexpected)"),
        Err(_) => summary.fail("[FPGA] DONE could not be read"),
    }
}

/// The files the FPGA and the cores need, which live on the SD card.
fn check_system_files(summary: &mut Summary) {
    for name in ["boot.bit.gz", "gameboy.bit.gz", "gba.bit.gz"] {
        match util::open_system_file(name) {
            Ok(_) => log::info!("[SYS] {name}: found"),
            Err(_) => summary.fail(&format!(
                "[SYS] {name}: not found (put it in /sdcard/system_rev2/ or /sdcard/system/)"
            )),
        }
    }

    for name in [
        "gameboy.bios-dmg.bin",
        "gameboy.bios-cgb.bin",
        "gba.bios.bin",
    ] {
        match util::open_system_file(name) {
            Ok(_) => log::info!("[SYS] {name}: found"),
            Err(_) => summary.warn(&format!("[SYS] {name}: not found (needed to boot that core)")),
        }
    }
}

/// The SD card, which is where the bitstreams come from.
fn check_sdcard(device: &mut Device<'_>, summary: &mut Summary) {
    if device.sdcard.is_none() {
        summary.fail("[SD] not mounted: check the card and the SDIO wiring");
        return;
    }

    match std::fs::read_dir("/sdcard") {
        Ok(entries) => log::info!("[SD] mounted at /sdcard: OK ({} entries)", entries.count()),
        Err(_) => summary.warn("[SD] mounted, but /sdcard could not be listed"),
    }
}

/// The real-time clock.
fn check_rtc(device: &mut Device<'_>, summary: &mut Summary) {
    match device.rtc.read_datetime() {
        Ok(Some(dt)) => log::info!(
            "[RTC] Time: {:04}-{:02}-{:02} {:02}:{:02}:{:02} (year 0 means it was never set)",
            dt.years,
            dt.months,
            dt.days,
            dt.hours,
            dt.minutes,
            dt.seconds
        ),
        Ok(None) => summary.warn("[RTC] no valid time (never set, or the backup supply is bad)"),
        Err(_) => summary.warn("[RTC] PCF8563 read failed: check I2C"),
    }
}

/// The accelerometer, which is optional: games without motion controls are fine.
fn check_imu(device: &mut Device<'_>, summary: &mut Summary) {
    match device.imu.read_accel() {
        Ok(sample) => log::info!(
            "[IMU] Accel: X={:.3} Y={:.3} Z={:.3} g",
            sample.x,
            sample.y,
            sample.z
        ),
        Err(_) => summary
            .warn("[IMU] accelerometer read failed: motion-controlled games will not work"),
    }
}

/// The audio codec.
fn check_dac(device: &mut Device<'_>, summary: &mut Summary) {
    match device.dac.get_interrupt_status() {
        Ok(status) => {
            log::info!(
                "[DAC] Headset detected: {}, short-circuit L={} R={}",
                status.headset_detected,
                status.left_short_circuit,
                status.right_short_circuit
            );
            if status.left_short_circuit || status.right_short_circuit {
                summary.fail("[DAC] output short circuit reported: check the speaker wiring");
            }
        }
        Err(_) => {
            summary.warn("[DAC] TLV320DAC3101 status read failed: check I2C and the reset line")
        }
    }
}
