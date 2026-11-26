//! WPSE470 water flow sensor (a Hall-effect turbine, 1–30 l/min).
//!
//! Water spins a small turbine with a magnet; a Hall sensor gives one pulse
//! per turn. The pulse rate is proportional to the flow:
//!     flow (l/min) = pulses per second * 60 / PULSES_PER_LITRE
//!
//! Counting: we ask the kernel for an event on every rising edge. Each
//! event carries a sequence number that counts *every* edge the kernel saw,
//! even ones dropped because our buffer was full. So the difference between
//! two sequence numbers is the exact pulse count, at any flow rate.
//!
//! Wiring caution: the sensor needs 4.7–15 V, and its signal wire usually
//! swings up to its supply voltage. Powered from 5 V, the signal must go
//! through a divider (for example 10 kΩ in series, 20 kΩ to GND) before it
//! reaches the Pi's 3.3 V input. Measure the signal voltage first.

use super::Driver;

pub const FLOW_LINE: &str = "GPIO17";

/// Pulses per litre. **Not verified for the WPSE470**: 450 (7.5 Hz per
/// l/min) is the usual figure for this style of 1–30 l/min sensor, but no
/// datasheet value was found. Calibrate: let exactly 1 litre through, read
/// "Total" on the Sensors page, and replace this with 450 * total shown.
pub const PULSES_PER_LITRE: f64 = 450.0;

/// Pulse count and elapsed time to flow in l/min.
pub fn pulses_to_flow_lpm(pulses: u32, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        return 0.0;
    }
    pulses as f64 / seconds * 60.0 / PULSES_PER_LITRE
}

#[cfg(target_os = "linux")]
pub struct Flow {
    request: gpiocdev::Request,
    /// Sequence number of the last edge counted; the kernel starts at 1.
    last_seqno: u32,
    last_read: std::time::Instant,
    total_pulses: u64,
}

#[cfg(target_os = "linux")]
impl Flow {
    pub fn open() -> Result<Self, String> {
        use gpiocdev::line::EdgeDetection;

        let line = gpiocdev::find_named_line(FLOW_LINE).ok_or(format!("{FLOW_LINE} not found"))?;
        let request = gpiocdev::Request::builder()
            .on_chip(&line.chip)
            .with_consumer("pid-ui flow")
            // Room for many edges between reads; the sequence numbers make
            // the count right even if it overflows.
            .with_kernel_event_buffer_size(1024)
            .with_line(line.info.offset)
            .as_input()
            .with_edge_detection(EdgeDetection::RisingEdge)
            .request()
            .map_err(|e| format!("cannot use {FLOW_LINE}: {e}"))?;
        Ok(Flow {
            request,
            last_seqno: 0,
            last_read: std::time::Instant::now(),
            total_pulses: 0,
        })
    }
}

#[cfg(target_os = "linux")]
impl Driver for Flow {
    fn read(&mut self) -> Result<Vec<f64>, String> {
        // Take every queued edge; only the newest sequence number matters.
        let mut newest = self.last_seqno;
        while self.request.has_edge_event().map_err(|e| e.to_string())? {
            let event = self.request.read_edge_event().map_err(|e| e.to_string())?;
            newest = event.line_seqno;
        }
        // `wrapping_sub` handles the counter rolling over after 4 billion.
        let pulses = newest.wrapping_sub(self.last_seqno);
        self.last_seqno = newest;

        let now = std::time::Instant::now();
        let seconds = (now - self.last_read).as_secs_f64();
        self.last_read = now;
        self.total_pulses += pulses as u64;

        // No pulses is a valid reading (no flow), not an error.
        Ok(vec![
            pulses_to_flow_lpm(pulses, seconds),
            self.total_pulses as f64 / PULSES_PER_LITRE,
        ])
    }
}

#[cfg(not(target_os = "linux"))]
pub struct Flow;

#[cfg(not(target_os = "linux"))]
impl Flow {
    pub fn open() -> Result<Self, String> {
        Err("GPIO is only available on the Raspberry Pi".to_string())
    }
}

#[cfg(not(target_os = "linux"))]
impl Driver for Flow {
    fn read(&mut self) -> Result<Vec<f64>, String> {
        Err("GPIO is only available on the Raspberry Pi".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_rate_converts_to_flow() {
        // 450 pulses per litre: 7.5 pulses per second is 1 l/min.
        assert!((pulses_to_flow_lpm(15, 2.0) - 1.0).abs() < 1e-9);
        assert_eq!(pulses_to_flow_lpm(0, 0.5), 0.0);
        assert_eq!(pulses_to_flow_lpm(10, 0.0), 0.0);
    }
}
