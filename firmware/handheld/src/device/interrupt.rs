use std::{num::NonZeroU32, sync::Arc};

use esp_idf_svc::{
    hal::{
        gpio::{InputMode, InterruptType, Pin, PinDriver},
        task::notification::{Notification, Notifier},
    },
    sys::EspError,
};

use crate::device::drivers::fpga;
use crate::ui;
use crate::worker;

use super::Device;

const FLAG_MCU_IRQ: NonZeroU32 = unsafe { NonZeroU32::new_unchecked(1) };
const FLAG_HOME: NonZeroU32 = unsafe { NonZeroU32::new_unchecked(2) };
const FLAG_POWER: NonZeroU32 = unsafe { NonZeroU32::new_unchecked(4) };
const FLAG_VOL_UP: NonZeroU32 = unsafe { NonZeroU32::new_unchecked(8) };
const FLAG_VOL_DOWN: NonZeroU32 = unsafe { NonZeroU32::new_unchecked(16) };

fn setup_gpio_interrupt(
    pin: &mut PinDriver<'_, impl Pin, impl InputMode>,
    interrupt_type: InterruptType,
    notifier: Arc<Notifier>,
    flags: NonZeroU32,
) -> Result<(), EspError> {
    // SAFETY: only ISR-safe FreeRTOS functions will be called (task notify).
    unsafe {
        pin.subscribe(move || {
            notifier.notify_and_yield(flags);
        })?;
    }
    pin.set_interrupt_type(interrupt_type)?;
    pin.enable_interrupt()?;
    Ok(())
}

/// Subscribe an interrupt, reporting a failure instead of panicking.
fn setup_or_log(
    pin: &mut PinDriver<'_, impl Pin, impl InputMode>,
    interrupt_type: InterruptType,
    notifier: Arc<Notifier>,
    flags: NonZeroU32,
    name: &str,
) {
    if let Err(error) = setup_gpio_interrupt(pin, interrupt_type, notifier, flags) {
        log::warn!("[IRQ] could not subscribe {name}: {error}");
    }
}

impl Device<'_> {
    /// Setup interrupts on the Device interrupt sources:
    ///
    /// * Volume up, volume down, home, and power buttons
    /// * Shared MCU_IRQ line
    pub(super) fn setup_interrupts() {
        // Setup interrupt handler thread.
        std::thread::Builder::new()
            .name("Interrupt".to_string())
            .stack_size(8 * 1024)
            .spawn(|| {
                let notification = Notification::new();

                {
                    let device = &mut Device::get().lock().unwrap();
                    setup_or_log(
                        &mut device.button_home,
                        InterruptType::AnyEdge,
                        notification.notifier(),
                        FLAG_HOME,
                        "the home button",
                    );
                    setup_or_log(
                        &mut device.button_power,
                        InterruptType::AnyEdge,
                        notification.notifier(),
                        FLAG_POWER,
                        "the power button",
                    );
                    setup_or_log(
                        &mut device.button_vol_up,
                        InterruptType::AnyEdge,
                        notification.notifier(),
                        FLAG_VOL_UP,
                        "volume up",
                    );
                    setup_or_log(
                        &mut device.button_vol_down,
                        InterruptType::AnyEdge,
                        notification.notifier(),
                        FLAG_VOL_DOWN,
                        "volume down",
                    );
                    setup_or_log(
                        &mut device.pin_irq,
                        InterruptType::LowLevel,
                        notification.notifier(),
                        FLAG_MCU_IRQ,
                        "the FPGA interrupt line",
                    );
                }

                let mut prev_hdmi_detected: Option<bool> = None;

                loop {
                    let flags = match notification.wait(esp_idf_svc::hal::delay::BLOCK) {
                        Some(flags) => flags.get(),
                        _ => continue,
                    };

                    let mut device = Device::get().lock().unwrap();

                    if (flags & FLAG_HOME.get()) != 0 {
                        let _ = device.button_home.enable_interrupt();
                    }
                    if (flags & FLAG_POWER.get()) != 0 {
                        let _ = device.button_power.enable_interrupt();
                    }
                    if (flags & FLAG_VOL_UP.get()) != 0 {
                        let _ = device.button_vol_up.enable_interrupt();
                    }
                    if (flags & FLAG_VOL_DOWN.get()) != 0 {
                        let _ = device.button_vol_down.enable_interrupt();
                    }

                    let io_expander = match device.io_expander.get_pins() {
                        Ok(pins) => pins,
                        Err(error) => {
                            log::warn!("[IOEXP] read failed in the interrupt handler: {error}");
                            let _ = device.pin_irq.enable_interrupt();
                            continue;
                        }
                    };
                    match device.read_button_state(io_expander) {
                        Ok(buttons) => ui::send(ui::Message::Button(buttons)),
                        Err(_) => log::warn!("[BTN] could not decode the button state"),
                    }

                    match device.parse_hdmi_detect(io_expander) {
                        Ok(hdmi_detected) => {
                            if prev_hdmi_detected != Some(hdmi_detected) {
                                prev_hdmi_detected = Some(hdmi_detected);
                                worker::send(worker::Message::HdmiDetectState(hdmi_detected));
                            }
                        }
                        Err(_) => log::warn!("[HDMI] hot-plug state could not be read"),
                    }

                    if (flags & FLAG_MCU_IRQ.get()) != 0 {
                        log::debug!("Interrupt: MCU_IRQ");
                        // N.B. important to read buttons above to clear i/o expander interrupt

                        // TODO handle other possible interrupt sources, including FPGA

                        // Fuel gauge IRQs.
                        if let Ok(fuel_irq) = device.fuel_gauge.query_alerts() {
                            for (alert, active) in fuel_irq.into_iter() {
                                if active {
                                    worker::send(worker::Message::FuelGaugeAlert(alert));
                                }
                            }
                        }

                        // DAC IRQs.
                        if let Ok(dac_irq) = device.dac.get_interrupt_status() {
                            if dac_irq.headset_detected {
                                match device.dac.get_headphones_detected() {
                                    Ok(has_headphones) => {
                                        worker::send(worker::Message::HeadphoneState(
                                            has_headphones,
                                        ));
                                    }
                                    Err(error) => log::warn!(
                                        "[DAC] headphone state could not be read: {error}"
                                    ),
                                }
                            }
                        }

                        // Read FPGA.
                        // TODO: only read and ack interrupts that we've enabled?
                        match device.fpga.read_u32(fpga::REG_IRQ_STATUS) {
                            Ok(fpga_irq) if fpga_irq != 0 => {
                                if let Err(error) =
                                    device.fpga.write_u32(fpga::REG_IRQ_STATUS, fpga_irq)
                                {
                                    log::warn!("[FPGA] could not clear irq 0x{fpga_irq:08X}: {error}");
                                }
                                worker::send(worker::Message::FpgaIrq(fpga_irq));
                            }
                            Ok(_) => {}
                            Err(error) => {
                                log::warn!("[FPGA] could not read the irq status: {error}")
                            }
                        }

                        let _ = device.pin_irq.enable_interrupt();
                    }
                }
            })
            .unwrap();
    }
}
