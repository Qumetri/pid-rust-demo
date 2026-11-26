//! DHT-22 temperature and humidity sensor, read through the Linux kernel.
//!
//! The DHT-22 sends its 40 data bits as pulses only 26–70 µs long. A
//! normal Linux program cannot time those reliably, which is why reading
//! it from user space often fails. The kernel has a driver for it (named
//! `dht11`, it handles the DHT-22 too) that times the pulses in interrupt
//! context. We only read the result from files under /sys.
//!
//! Setup, once: add this line to /boot/firmware/config.txt and reboot
//!     dtoverlay=dht11,gpiopin=4
//! or load it until the next reboot with
//!     sudo dtoverlay dht11 gpiopin=4
//!
//! The kernel then creates a directory such as
//! /sys/bus/iio/devices/iio:device0 with:
//!     name                        "dht11@4" (older kernels: "dht11")
//!     in_temp_input               temperature in thousandths of a °C
//!     in_humidityrelative_input   humidity in thousandths of a %
//! Reading `in_temp_input` triggers a measurement; the driver caches the
//! result for 2 s, so the humidity read right after it uses the same one.

use super::Driver;
use std::fs;
use std::path::{Path, PathBuf};

const IIO_DEVICES: &str = "/sys/bus/iio/devices";

pub struct Dht22 {
    /// The sensor's directory under /sys/bus/iio/devices.
    dir: PathBuf,
}

impl Dht22 {
    pub fn open() -> Result<Self, String> {
        Self::find_in(Path::new(IIO_DEVICES)).ok_or_else(|| {
            "kernel driver not loaded: add dtoverlay=dht11,gpiopin=4".to_string()
        })
    }

    /// Look through the IIO devices for the DHT driver's one.
    ///
    /// Takes the directory as a parameter so a test can point it at a
    /// fake directory instead of the real /sys.
    fn find_in(devices: &Path) -> Option<Self> {
        // `.ok()?` turns a failed directory read into "not found".
        for entry in fs::read_dir(devices).ok()?.flatten() {
            let dir = entry.path();
            let name = fs::read_to_string(dir.join("name")).unwrap_or_default();
            if is_dht_device_name(name.trim()) {
                return Some(Dht22 { dir });
            }
        }
        None
    }

    /// Read one attribute file and convert thousandths to units.
    fn read_milli(&self, file: &str) -> Result<f64, String> {
        let text = fs::read_to_string(self.dir.join(file)).map_err(|e| {
            // The kernel reports a failed transfer as an I/O error or a
            // timeout. Both are common and harmless: the next read
            // usually works.
            format!("read failed ({e})")
        })?;
        parse_milli(&text)
    }
}

/// The kernel names the device "dht11@<pin>", for example "dht11@4" on
/// kernel 6.18; older kernels used just "dht11".
pub fn is_dht_device_name(name: &str) -> bool {
    name == "dht11" || name.starts_with("dht11@")
}

/// "21400\n" -> 21.4
pub fn parse_milli(text: &str) -> Result<f64, String> {
    text.trim()
        .parse::<i64>()
        .map(|milli| milli as f64 / 1000.0)
        .map_err(|_| format!("unexpected value '{}'", text.trim()))
}

impl Driver for Dht22 {
    fn read(&mut self) -> Result<Vec<f64>, String> {
        let temperature = self.read_milli("in_temp_input")?;
        let humidity = self.read_milli("in_humidityrelative_input")?;

        // The DHT-22's own limits. A value outside them is a corrupted
        // transfer that slipped past the checksum.
        if !(-40.0..=80.0).contains(&temperature) || !(0.0..=100.0).contains(&humidity) {
            return Err(format!(
                "implausible reading ({temperature:.1} °C, {humidity:.1} %)"
            ));
        }
        Ok(vec![temperature, humidity])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_thousandths() {
        assert_eq!(parse_milli("21400\n"), Ok(21.4));
        assert_eq!(parse_milli("-5250"), Ok(-5.25));
        assert!(parse_milli("abc").is_err());
    }

    #[test]
    fn recognises_both_device_names() {
        assert!(is_dht_device_name("dht11@4"));
        assert!(is_dht_device_name("dht11"));
        assert!(!is_dht_device_name("dht12"));
        assert!(!is_dht_device_name("iio_hwmon@4"));
    }

    #[test]
    fn finds_the_device_by_name_and_reads_it() {
        // Build a fake /sys/bus/iio/devices in a temporary directory.
        let root = std::env::temp_dir().join(format!("dht22-test-{}", std::process::id()));
        let other = root.join("iio:device0");
        let dht = root.join("iio:device1");
        fs::create_dir_all(&other).unwrap();
        fs::create_dir_all(&dht).unwrap();
        fs::write(other.join("name"), "something-else\n").unwrap();
        fs::write(dht.join("name"), "dht11@4\n").unwrap();
        fs::write(dht.join("in_temp_input"), "22500\n").unwrap();
        fs::write(dht.join("in_humidityrelative_input"), "41300\n").unwrap();

        let mut sensor = Dht22::find_in(&root).expect("device not found");
        assert_eq!(sensor.read(), Ok(vec![22.5, 41.3]));

        fs::remove_dir_all(&root).unwrap();
    }
}
