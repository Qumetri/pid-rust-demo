//! RCWL-1601 ultrasonic distance sensor (same protocol as the HC-SR04).
//!
//! How the sensor works:
//!   1. We hold the Trig pin high for at least 10 µs.
//!   2. The sensor sends a burst of ultrasound and raises Echo.
//!   3. Echo goes low when the reflection comes back.
//! The length of the Echo pulse is the round-trip time of the sound.
//!
//! Timing the pulse with a clock in our own program would be inaccurate:
//! Linux may pause the thread for a millisecond at any moment, and 1 ms is
//! 17 cm. Instead we ask the kernel for *edge events*. The kernel stamps
//! each edge with the time inside the interrupt handler, so the pulse
//! length is accurate even if our thread wakes up late.
//!
//! Wiring: power the RCWL-1601 from **3.3 V**. Its Echo output then swings
//! 0–3.3 V, which is safe for the Pi. Powered from 5 V, Echo is 5 V and
//! needs a voltage divider before it reaches the Pi's GPIO.

use super::Driver;
use std::time::Duration;

/// GPIO names as the kernel lists them (`gpioinfo`). Looking pins up by
/// name avoids depending on which /dev/gpiochipN the kernel numbered.
pub const TRIG_LINE: &str = "GPIO23";
pub const ECHO_LINE: &str = "GPIO24";

/// Distance from the sensor down to the bottom of the (empty) tank. Level
/// is this minus the measured distance. 40 cm matches the top of the plot.
/// This becomes a per-profile calibration value later.
pub const MOUNT_HEIGHT_CM: f64 = 40.0;

/// Speed of sound in air at about 20 °C, in cm per second.
const SPEED_OF_SOUND_CM_S: f64 = 34_300.0;

/// The sensor's usable range. Readings outside it are reflections or noise.
const MIN_CM: f64 = 2.0;
const MAX_CM: f64 = 450.0;

/// Turn the Echo pulse length into a distance (one way, so halve it).
///
/// Kept separate from the hardware code so it can be unit-tested.
pub fn echo_to_distance_cm(echo_ns: u64) -> Result<f64, String> {
    let distance = echo_ns as f64 / 1e9 * SPEED_OF_SOUND_CM_S / 2.0;
    if (MIN_CM..=MAX_CM).contains(&distance) {
        Ok(distance)
    } else {
        Err(format!("out of range ({distance:.0} cm)"))
    }
}

/// Level in the tank for a measured distance, never below empty.
pub fn distance_to_level_cm(distance_cm: f64) -> f64 {
    (MOUNT_HEIGHT_CM - distance_cm).max(0.0)
}

// ---------------------------------------------------------------------------
// The driver. On Linux it uses the GPIO character device through the
// `gpiocdev` crate. `#[cfg(...)]` includes code only when the condition
// holds, like `#ifdef` in C++.
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub struct Ultrasonic {
    request: gpiocdev::Request,
    trig: gpiocdev::line::Offset,
}

#[cfg(target_os = "linux")]
impl Ultrasonic {
    pub fn open() -> Result<Self, String> {
        use gpiocdev::line::{EdgeDetection, Value};

        let trig = gpiocdev::find_named_line(TRIG_LINE).ok_or(format!("{TRIG_LINE} not found"))?;
        let echo = gpiocdev::find_named_line(ECHO_LINE).ok_or(format!("{ECHO_LINE} not found"))?;

        // One request holds both lines. Each `with_line` selects a line,
        // and the settings after it apply to that line only.
        let request = gpiocdev::Request::builder()
            .on_chip(&trig.chip)
            .with_consumer("pid-ui ultrasonic")
            .with_line(trig.info.offset)
            .as_output(Value::Inactive)
            .with_line(echo.info.offset)
            .as_input()
            .with_edge_detection(EdgeDetection::BothEdges)
            .request()
            // `map_err` converts the library's error type into our String.
            .map_err(|e| format!("cannot use {TRIG_LINE}/{ECHO_LINE}: {e}"))?;

        Ok(Ultrasonic {
            request,
            trig: trig.info.offset,
        })
    }

    /// Wait for the next edge of the given kind and return its kernel
    /// timestamp in nanoseconds.
    fn wait_for(
        &self,
        kind: gpiocdev::line::EdgeKind,
        timeout: Duration,
        what: &str,
    ) -> Result<u64, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let ready = self
                .request
                .wait_edge_event(left)
                .map_err(|e| e.to_string())?;
            if !ready {
                return Err(format!("no echo {what}: check wiring and power"));
            }
            let event = self.request.read_edge_event().map_err(|e| e.to_string())?;
            if event.kind == kind {
                return Ok(event.timestamp_ns);
            }
            // An edge of the other kind (left over or noise): keep waiting.
        }
    }
}

#[cfg(target_os = "linux")]
impl Driver for Ultrasonic {
    fn read(&mut self) -> Result<Vec<f64>, String> {
        use gpiocdev::line::{EdgeKind, Value};

        // Throw away edges left from an earlier, failed measurement.
        while self.request.has_edge_event().map_err(|e| e.to_string())? {
            self.request.read_edge_event().map_err(|e| e.to_string())?;
        }

        // Trigger pulse. `sleep` waits at least 10 µs, usually a little
        // more, which the sensor accepts.
        let set = |value| {
            self.request
                .set_value(self.trig, value)
                .map_err(|e: gpiocdev::Error| e.to_string())
        };
        set(Value::Active)?;
        std::thread::sleep(Duration::from_micros(10));
        set(Value::Inactive)?;

        // The echo starts within a few hundred µs; the longest echo (4.5 m)
        // lasts about 26 ms.
        let rise = self.wait_for(EdgeKind::Rising, Duration::from_millis(30), "start")?;
        let fall = self.wait_for(EdgeKind::Falling, Duration::from_millis(40), "end")?;

        let distance = echo_to_distance_cm(fall.saturating_sub(rise))?;
        Ok(vec![distance, distance_to_level_cm(distance)])
    }
}

/// On other systems (a Mac or PC running the window demo) there is no
/// GPIO, so opening the sensor reports that clearly instead of failing to
/// build.
#[cfg(not(target_os = "linux"))]
pub struct Ultrasonic;

#[cfg(not(target_os = "linux"))]
impl Ultrasonic {
    pub fn open() -> Result<Self, String> {
        Err("GPIO is only available on the Raspberry Pi".to_string())
    }
}

#[cfg(not(target_os = "linux"))]
impl Driver for Ultrasonic {
    fn read(&mut self) -> Result<Vec<f64>, String> {
        Err("GPIO is only available on the Raspberry Pi".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_time_converts_to_distance() {
        // 1 ms round trip = 0.001 s * 34300 cm/s / 2 = 17.15 cm
        let distance = echo_to_distance_cm(1_000_000).unwrap();
        assert!((distance - 17.15).abs() < 1e-9);
    }

    #[test]
    fn readings_outside_the_range_are_rejected() {
        assert!(echo_to_distance_cm(10_000).is_err()); // 0.17 cm
        assert!(echo_to_distance_cm(30_000_000).is_err()); // 5 m
    }

    #[test]
    fn level_is_mount_height_minus_distance() {
        assert_eq!(distance_to_level_cm(15.0), 25.0);
        assert_eq!(distance_to_level_cm(100.0), 0.0); // never below empty
    }
}
