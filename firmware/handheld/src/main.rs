use anyhow::Context;
use flate2::read::GzDecoder;

use device::Device;

use crate::ui::UI;

mod bitstream;
mod device;
mod kvs;
mod power;
pub mod ui;
mod util;
mod worker;

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::set_max_level(log::LevelFilter::Info);
    esp_idf_svc::log::set_target_level("gpio", log::LevelFilter::Warn).unwrap();

    kvs::Kvs::init()?;
    log_boot_banner();

    // Check that the firmware is compatible with the listed revision.
    cfg_if::cfg_if! {
        if #[cfg(feature = "rev2")] {
            let required_revision = 2;
        } else {
            compile_error!("No board revision selected");
        }
    };
    if kvs::keys::DEVICE_REVISION.get() != Some(required_revision) {
        anyhow::bail!(
            "Incompatible firmware revision: device={:?} firmware={}",
            kvs::keys::DEVICE_REVISION.get(),
            required_revision
        );
    }

    // Proceed to initialize device.
    Device::init()?;
    let mut device = Device::lock();
    let is_sdcard_mounted = device.sdcard.is_some();
    device.set_brightness(kvs::keys::BRIGHTNESS.get().unwrap());

    // Report what the board looks like before anything is programmed. This is
    // the log to read when the device does not come up.
    let self_test = device::test_hardware::run_tests(&mut device);
    if self_test.is_clean() {
        log::info!("=== Self-test complete: clean - boot continues ===");
    } else {
        log::warn!(
            "=== Self-test complete: {} - the device may still work; see the tagged lines above ===",
            self_test
        );
    }

    // Setup workers.
    worker::start();

    // Initial programming FPGA
    fn program_fpga(device: &mut Device) -> anyhow::Result<()> {
        let bitstream =
            util::open_system_file("boot.bit.gz").context("Failed to read bitstream")?;
        let mut bitstream = GzDecoder::new(bitstream);
        device
            .fpga
            .program(&mut bitstream)
            .context("Failed to program FPGA")?;
        device.lcd.enable_fpga_control()?;
        Ok(())
    }
    match program_fpga(&mut device) {
        Ok(()) => {}
        Err(_) if !is_sdcard_mounted => {
            // Without a card there is no bitstream to load, but the MCU can
            // still drive the LCD, so this is reported on screen further down.
        }
        Err(error) => {
            // The FPGA drives the LCD, so a failed bring-up leaves nothing to
            // show: keep the board alive in diagnostics instead.
            device::test_hardware::hold_alive(&mut device, &format!("{error:#}"));
        }
    }

    // Setup UI
    let mut ui = UI::new(&mut device);
    std::mem::drop(device);

    if !is_sdcard_mounted {
        show_fatal_error("Error mounting SD card".into());
    }

    // Run UI in this thread.
    ui.run();
}

/// Print what was built, what it is running on, and why it last reset.
///
/// This is the first thing the application logs, and it is the line to compare
/// when the device does not behave like the firmware that was just flashed.
fn log_boot_banner() {
    log::info!("################ Game Bub handheld ################");
    log::info!("# firmware    : {}", env!("CARGO_PKG_VERSION"));
    log::info!("# git commit  : {}", env!("GIT_COMMIT"));
    log::info!("# device rev  : {:?}", kvs::keys::DEVICE_REVISION.get());
    log::info!("# serial      : {:?}", kvs::keys::DEVICE_SERIAL.get());
    log::info!("# reset reason: {}", reset_reason_name());
    log::info!("##################################################");
}

/// Human-readable name of the reason the MCU last reset.
///
/// A repeating `WDT`/`TASK_WDT` here means something hangs during boot, which
/// is the most useful signal when the device shows nothing on screen.
fn reset_reason_name() -> &'static str {
    // This is a plain C function in some esp-idf-svc versions and a typed
    // wrapper in others, hence the two allow attributes.
    #[allow(unused_unsafe)]
    #[allow(clippy::unnecessary_cast)]
    let reason = unsafe { esp_idf_svc::sys::esp_reset_reason() } as u32;
    match reason {
        0 => "UNKNOWN",
        1 => "POWERON",
        2 => "EXT_PIN",
        3 => "SW",
        4 => "PANIC",
        5 => "INT_WDT",
        6 => "TASK_WDT",
        7 => "WDT",
        8 => "DEEPSLEEP",
        9 => "BROWNOUT",
        10 => "SDIO",
        11 => "USB",
        12 => "JTAG",
        13 => "EFUSE",
        14 => "PWR_GLITCH",
        15 => "CPU_LOCKUP",
        _ => "UNRECOGNIZED",
    }
}

fn show_fatal_error(error: String) {
    Device::lock().lcd.enable_mcu_control().unwrap();
    ui::send(ui::Message::FatalError(error));
    ui::send(ui::Message::Redraw);
}
