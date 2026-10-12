//! Power monitoring constants shared by the boot path and the diagnostics mode.

/// Battery voltage below which the FPGA is not powered up.
///
/// Bringing the FPGA up while the supply is collapsing risks an abnormal
/// power-up, so below this threshold (and with no USB power attached) the MCU
/// leaves the FPGA off and stays in diagnostics instead.
pub const CUTOFF_VOLTAGE: f32 = 3.3;

/// A voltage below this is treated as "the gauge has no reading yet" rather
/// than as a flat battery: the MAX17048 answers with a zeroed register for a
/// while after power-up, and acting on that would refuse to start the FPGA on
/// a healthy battery.
pub const MIN_PLAUSIBLE_VOLTAGE: f32 = 1.0;

/// Whether the battery is too low to power the FPGA.
pub fn is_battery_critical(voltage: f32, vbus_present: bool) -> bool {
    voltage < CUTOFF_VOLTAGE && voltage > MIN_PLAUSIBLE_VOLTAGE && !vbus_present
}
