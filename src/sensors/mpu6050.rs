//! GY-521 board with an MPU6050 accelerometer and gyroscope, on I2C.
//!
//! Its role in the rig is not decided yet (PROJECT-NOTES.md §10), so it is
//! monitoring only. It reports three summary values that fit a pump rig:
//!   Tilt       angle from level, from the direction of gravity (°)
//!   Rotation   how fast the board turns (°/s)
//!   Vibration  how far the total acceleration is from 1 g (g)
//!
//! Wiring: VCC 3.3 V or 5 V (the board has its own regulator, and its I2C
//! pull-ups go to 3.3 V, so both are safe for the Pi), GND, SDA to GPIO2
//! (pin 3), SCL to GPIO3 (pin 5). AD0 unconnected gives address 0x68.

use super::Driver;
use crate::hw::i2c::{I2cDevice, BUS};

pub const ADDRESS: u16 = 0x68;

// Register numbers from the MPU6050 register map.
const PWR_MGMT_1: u8 = 0x6B;
const CONFIG: u8 = 0x1A;
const ACCEL_XOUT_H: u8 = 0x3B;

/// Raw counts per unit at the power-on ranges (±2 g, ±250 °/s).
const COUNTS_PER_G: f64 = 16_384.0;
const COUNTS_PER_DEG_S: f64 = 131.0;

pub struct Mpu6050 {
    device: I2cDevice,
}

impl Mpu6050 {
    pub fn open() -> Result<Self, String> {
        let mut device = I2cDevice::open(BUS, ADDRESS)?;
        // The chip starts asleep: clear the sleep bit.
        device
            .write(&[PWR_MGMT_1, 0x00])
            .map_err(|_| format!("no MPU6050 at {ADDRESS:#04x}: check wiring"))?;
        // Built-in low-pass filter at about 44 Hz, to calm pump vibration.
        device.write(&[CONFIG, 0x03])?;
        Ok(Mpu6050 { device })
    }
}

/// The three summary values from raw accelerometer and gyroscope counts.
/// Kept separate from the I2C code so it can be unit-tested.
pub fn summarise(accel: [i16; 3], gyro: [i16; 3]) -> [f64; 3] {
    let a: Vec<f64> = accel.iter().map(|&c| c as f64 / COUNTS_PER_G).collect();
    let g: Vec<f64> = gyro.iter().map(|&c| c as f64 / COUNTS_PER_DEG_S).collect();

    let magnitude = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    // Angle between gravity and the board's Z axis.
    let tilt = if magnitude > 0.0 {
        (a[2] / magnitude).clamp(-1.0, 1.0).acos().to_degrees()
    } else {
        0.0
    };
    let rotation = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
    let vibration = (magnitude - 1.0).abs();
    [tilt, rotation, vibration]
}

impl Driver for Mpu6050 {
    fn read(&mut self) -> Result<Vec<f64>, String> {
        // 14 bytes from ACCEL_XOUT_H: accel X, Y, Z, temperature, gyro
        // X, Y, Z, each a big-endian 16-bit signed number.
        let mut raw = [0u8; 14];
        self.device.read_registers(ACCEL_XOUT_H, &mut raw)?;
        let word = |i: usize| i16::from_be_bytes([raw[i], raw[i + 1]]);

        let values = summarise([word(0), word(2), word(4)], [word(8), word(10), word(12)]);
        Ok(values.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_and_still_reads_zero() {
        // Lying flat: 1 g on Z, nothing turning.
        let [tilt, rotation, vibration] = summarise([0, 0, 16_384], [0, 0, 0]);
        assert!(tilt.abs() < 1e-9);
        assert_eq!(rotation, 0.0);
        assert!(vibration.abs() < 1e-9);
    }

    #[test]
    fn on_its_side_is_ninety_degrees() {
        let [tilt, _, _] = summarise([16_384, 0, 0], [0, 0, 0]);
        assert!((tilt - 90.0).abs() < 1e-9);
    }

    #[test]
    fn turning_shows_rotation() {
        let [_, rotation, _] = summarise([0, 0, 16_384], [131 * 3, 131 * 4, 0]);
        assert!((rotation - 5.0).abs() < 1e-9);
    }
}
