//! A pump on one channel of the VMA409 (L298N) motor driver.
//!
//! The L298N has, per channel, an enable input (ENA/ENB) and two direction
//! inputs (IN1/IN2, IN3/IN4). A pump only ever runs one way, so the
//! direction inputs are wired fixed (IN1 to 3.3 V, IN2 to GND) and only the
//! enable input gets a PWM signal: its duty cycle sets the pump speed. That
//! saves four GPIO pins.
//!
//! Wiring notes:
//! - Remove the ENA/ENB jumpers on the board, or PWM has no effect.
//! - The L298N reads 3.3 V as high, so no level shifter is needed.
//! - Connect the board's GND to the Pi's GND.
//! - The L298N loses about 2 V, so a 12 V supply gives the pump about 10 V.
//! - Small DC pumps usually do not start below 20–40 % duty: raise
//!   "Output min" on the Tuning page to just below that point.

use super::Actuator;
use crate::hw::pwm::{self, PwmChannel};

/// 1 kHz: slow enough for the L298N to switch cleanly, fast enough for a
/// steady pump. It may whine faintly.
const PWM_HZ: u32 = 1000;

pub struct Pump {
    pwm: PwmChannel,
}

impl Pump {
    /// Channel 0 is GPIO12 (pump A), channel 1 is GPIO13 (pump B).
    pub fn open(channel: u32) -> Result<Self, String> {
        Ok(Pump {
            pwm: PwmChannel::open(channel, PWM_HZ)?,
        })
    }
}

impl Actuator for Pump {
    fn set(&mut self, fraction: f64) -> Result<(), String> {
        self.pwm.set(fraction)
    }
}

pub fn all_off() {
    pwm::all_off();
}
