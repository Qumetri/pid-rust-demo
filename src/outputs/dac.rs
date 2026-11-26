//! MCP4725 12-bit DAC on I2C: an analogue voltage output.
//!
//! The output goes from 0 V to the board's supply voltage in 4096 steps.
//! Power it from **3.3 V**: most MCP4725 boards pull the I2C lines up to
//! their own supply, and 5 V on the Pi's I2C pins can damage it. The output
//! is then 0–3.3 V. A 0–10 V actuator needs an op-amp stage after it.
//!
//! Power-up value: the chip starts at a value stored in its EEPROM, which
//! is half scale (1.65 V) from the factory. For a safe power-up we store 0
//! there once, the first time we find it set to something else.

use super::Actuator;
use crate::hw::i2c::{I2cDevice, BUS};

/// The MCP4725 answers on one of 0x60–0x67 depending on the chip variant
/// and its A0 pin; most boards use 0x60, Adafruit's uses 0x62.
const ADDRESSES: std::ops::RangeInclusive<u16> = 0x60..=0x67;

/// Command byte: write the DAC register *and* the EEPROM.
const WRITE_DAC_AND_EEPROM: u8 = 0x60;

pub struct Mcp4725 {
    device: I2cDevice,
}

/// Find the DAC by reading from each possible address. Reading is
/// harmless: the chip just returns its status.
fn find() -> Result<(I2cDevice, [u8; 5]), String> {
    let mut last_error = String::from("no MCP4725 found at 0x60–0x67: check wiring");
    for address in ADDRESSES {
        match I2cDevice::open(BUS, address) {
            Ok(mut device) => {
                let mut status = [0u8; 5];
                if device.read(&mut status).is_ok() {
                    return Ok((device, status));
                }
            }
            // No I2C at all: stop and report that instead.
            Err(error) => {
                last_error = error;
                break;
            }
        }
    }
    Err(last_error)
}

/// 0.0..=1.0 to the two bytes of a "fast write" (12-bit value, upper 4
/// bits first, power-down bits 0).
pub fn fast_write_bytes(fraction: f64) -> [u8; 2] {
    let code = (fraction.clamp(0.0, 1.0) * 4095.0).round() as u16;
    [(code >> 8) as u8 & 0x0F, (code & 0xFF) as u8]
}

/// The value the chip will start with at power-up, from its status bytes.
pub fn eeprom_value(status: &[u8; 5]) -> u16 {
    (((status[3] & 0x0F) as u16) << 8) | status[4] as u16
}

impl Mcp4725 {
    pub fn open() -> Result<Self, String> {
        let (mut device, status) = find()?;
        device.write(&fast_write_bytes(0.0))?;
        if eeprom_value(&status) != 0 {
            // Store 0 as the power-up value. The EEPROM survives about a
            // million writes, and this happens only once.
            device.write(&[WRITE_DAC_AND_EEPROM, 0, 0])?;
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(Mcp4725 { device })
    }
}

impl Actuator for Mcp4725 {
    fn set(&mut self, fraction: f64) -> Result<(), String> {
        self.device.write(&fast_write_bytes(fraction))
    }
}

impl Drop for Mcp4725 {
    fn drop(&mut self) {
        let _ = self.device.write(&fast_write_bytes(0.0));
    }
}

pub fn all_off() {
    if let Ok((mut device, _)) = find() {
        let _ = device.write(&fast_write_bytes(0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_write_encodes_twelve_bits() {
        assert_eq!(fast_write_bytes(0.0), [0x00, 0x00]);
        assert_eq!(fast_write_bytes(1.0), [0x0F, 0xFF]);
        assert_eq!(fast_write_bytes(0.5), [0x08, 0x00]); // 2048
        assert_eq!(fast_write_bytes(7.0), [0x0F, 0xFF]); // clamped
    }

    #[test]
    fn reads_the_power_up_value() {
        // Factory default: half scale, 0x800.
        assert_eq!(eeprom_value(&[0xC0, 0x80, 0x00, 0x08, 0x00]), 0x800);
        assert_eq!(eeprom_value(&[0xC0, 0x00, 0x00, 0x00, 0x00]), 0);
    }
}
