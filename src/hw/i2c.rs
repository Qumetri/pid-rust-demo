//! One device on an I2C bus, through the kernel's /dev/i2c-N files.
//!
//! I2C is a two-wire bus (SDA, SCL) on GPIO2 and GPIO3 (pins 3 and 5).
//! Many devices share it; each has a 7-bit address. On the Pi it has to be
//! switched on once: `dtparam=i2c_arm=on` in /boot/firmware/config.txt
//! (or `sudo raspi-config`), then reboot. /dev/i2c-1 then appears.
//!
//! Devices on the bus of this project:
//!   0x60–0x67  MCP4725 DAC (most boards 0x60, Adafruit's 0x62)
//!   0x68       MPU6050 on the GY-521 board
//!   0x23       BH1750 light sensor, if one is added later
//!   0x48       ADS1115 ADC, if one is added later

/// The bus on the 40-pin header.
pub const BUS: u8 = 1;

#[cfg(target_os = "linux")]
pub use linux::I2cDevice;

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    /// The ioctl request number that selects the device address
    /// (I2C_SLAVE in the kernel header linux/i2c-dev.h).
    const I2C_SLAVE: u64 = 0x0703;

    pub struct I2cDevice {
        file: File,
    }

    impl I2cDevice {
        pub fn open(bus: u8, address: u16) -> Result<Self, String> {
            let path = format!("/dev/i2c-{bus}");
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => {
                        "I2C not enabled: add dtparam=i2c_arm=on".to_string()
                    }
                    _ => format!("cannot open {path}: {e}"),
                })?;

            // Tell the kernel which device the following reads and writes
            // are for.
            //
            // `unsafe` marks code the compiler cannot check: here, a call
            // into the C library. `ioctl` only reads the integer arguments,
            // and the file descriptor is valid because `file` is alive for
            // the whole call, so this is safe in practice. This is the only
            // `unsafe` block in the project.
            let result =
                unsafe { libc::ioctl(file.as_raw_fd(), I2C_SLAVE as _, address as libc::c_ulong) };
            if result < 0 {
                return Err(format!(
                    "cannot select I2C address {address:#04x}: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(I2cDevice { file })
        }

        pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.file
                .write_all(bytes)
                .map_err(|e| format!("I2C write failed ({e}): check wiring"))
        }

        pub fn read(&mut self, buffer: &mut [u8]) -> Result<(), String> {
            self.file
                .read_exact(buffer)
                .map_err(|e| format!("I2C read failed ({e}): check wiring"))
        }

        /// Write a register number, then read that register onwards.
        pub fn read_registers(&mut self, register: u8, buffer: &mut [u8]) -> Result<(), String> {
            self.write(&[register])?;
            self.read(buffer)
        }
    }
}

/// Stand-in for other systems, with the same methods.
#[cfg(not(target_os = "linux"))]
pub struct I2cDevice;

#[cfg(not(target_os = "linux"))]
impl I2cDevice {
    pub fn open(_bus: u8, _address: u16) -> Result<Self, String> {
        Err("I2C is only available on the Raspberry Pi".to_string())
    }
    pub fn write(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Err("I2C is only available on the Raspberry Pi".to_string())
    }
    pub fn read(&mut self, _buffer: &mut [u8]) -> Result<(), String> {
        Err("I2C is only available on the Raspberry Pi".to_string())
    }
    pub fn read_registers(&mut self, _register: u8, _buffer: &mut [u8]) -> Result<(), String> {
        Err("I2C is only available on the Raspberry Pi".to_string())
    }
}
