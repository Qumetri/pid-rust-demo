//! Hardware PWM through the kernel's /sys/class/pwm interface.
//!
//! The Pi 4 has two hardware PWM channels that can come out on GPIO12 and
//! GPIO13 (pins 32 and 33). They are switched on with one line in
//! /boot/firmware/config.txt, then a reboot:
//!     dtoverlay=pwm-2chan,pin=12,func=4,pin2=13,func2=4
//!
//! Hardware PWM keeps a perfectly steady signal however busy the program
//! is, unlike switching a pin on and off from a thread.
//!
//! Caution: /sys/class/pwm can hold more than one PWM controller. On this
//! Pi, `pwmchip0` is the touchscreen's backlight, so we must not assume a
//! number. We look for the controller by its hardware address instead.
//!
//! Using it is a matter of writing numbers into files:
//!     pwmchipN/export            "0" creates the pwm0 directory
//!     pwmchipN/pwm0/period       cycle length in nanoseconds
//!     pwmchipN/pwm0/duty_cycle   on-time in nanoseconds
//!     pwmchipN/pwm0/enable       "1" starts the output

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const PWM_CLASS: &str = "/sys/class/pwm";

/// The Pi 4's PWM0 block (0x7e20c000 on the VideoCore bus, fe20c000 for
/// the ARM). Its sysfs path contains this text.
const PI4_PWM0: &str = "fe20c000.pwm";

pub const NOT_ENABLED: &str = "PWM not enabled: add dtoverlay=pwm-2chan,pin=12,func=4,pin2=13,func2=4";

/// Find the Pi's own PWM controller among the pwmchipN entries.
fn find_chip() -> Option<PathBuf> {
    for entry in fs::read_dir(PWM_CLASS).ok()?.flatten() {
        // Each pwmchipN is a link to the real device; its target tells us
        // which hardware it belongs to.
        let target = fs::read_link(entry.path()).unwrap_or_default();
        if target.to_string_lossy().contains(PI4_PWM0) {
            return Some(entry.path());
        }
    }
    None
}

/// One PWM output channel.
pub struct PwmChannel {
    dir: PathBuf,
    period_ns: u64,
}

impl PwmChannel {
    /// Set up channel 0 (GPIO12) or 1 (GPIO13) at the given frequency,
    /// starting at 0 % duty.
    pub fn open(channel: u32, frequency_hz: u32) -> Result<Self, String> {
        let chip = find_chip().ok_or(NOT_ENABLED)?;
        let dir = chip.join(format!("pwm{channel}"));

        if !dir.exists() {
            write(&chip.join("export"), &channel.to_string())?;
            // udev sets the new files' permissions a moment after the
            // directory appears, so wait for them to become writable.
            for _ in 0..20 {
                if is_writable(&dir.join("enable")) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }

        let period_ns = 1_000_000_000 / frequency_hz as u64;
        // Duty may never exceed the period, so zero it before changing the
        // period.
        write(&dir.join("duty_cycle"), "0")?;
        write(&dir.join("period"), &period_ns.to_string())?;
        write(&dir.join("enable"), "1")?;
        Ok(PwmChannel { dir, period_ns })
    }

    /// Set the duty cycle as a fraction, 0.0 (off) to 1.0 (fully on).
    pub fn set(&mut self, fraction: f64) -> Result<(), String> {
        let duty = (fraction.clamp(0.0, 1.0) * self.period_ns as f64) as u64;
        write(&self.dir.join("duty_cycle"), &duty.to_string())
    }
}

/// `Drop` runs when the value goes away (like a C++ destructor): leave the
/// output off. A crash or `kill` skips this, which is why the systemd
/// service also runs `ui --safe-outputs` after the program stops.
impl Drop for PwmChannel {
    fn drop(&mut self) {
        let _ = write(&self.dir.join("duty_cycle"), "0");
        let _ = write(&self.dir.join("enable"), "0");
    }
}

/// Switch both channels off, if they exist. Used by `--safe-outputs`.
pub fn all_off() {
    if let Some(chip) = find_chip() {
        for channel in 0..2 {
            let dir = chip.join(format!("pwm{channel}"));
            if dir.exists() {
                let _ = write(&dir.join("duty_cycle"), "0");
                let _ = write(&dir.join("enable"), "0");
            }
        }
    }
}

fn write(path: &Path, value: &str) -> Result<(), String> {
    fs::write(path, value).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn is_writable(path: &Path) -> bool {
    fs::OpenOptions::new().write(true).open(path).is_ok()
}
