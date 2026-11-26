//! Real outputs: the two pumps on the VMA409 motor driver, and the
//! MCP4725 DAC for driving an external device.
//!
//! The control loop owns the output drivers (one thread, so a write never
//! waits for a lock held by another thread) and publishes their state to
//! an `OutputBoard` for the screen.
//!
//! Roles in the rig (docs/IO-MAP.md):
//!   Pump A   the controller's output: fills the tank
//!   Pump B   the disturbance: drains the tank, set by the Disturbance
//!            slider, to show how the controller fights a load change
//!   DAC      0–3.3 V analogue output for an external actuator, when the
//!            box is used as a general-purpose controller
//!
//! Safety: every output starts at 0 and returns to 0 when it is no longer
//! selected or the program ends. The e-stop cuts pump power in hardware;
//! the software never relies on itself for that.

pub mod dac;
pub mod pump;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputId {
    PumpA = 0,
    PumpB = 1,
    Dac = 2,
}

pub struct OutputInfo {
    pub id: OutputId,
    pub name: &'static str,
    pub wiring: &'static str,
    pub role: &'static str,
}

pub const OUTPUTS: [OutputInfo; 3] = [
    OutputInfo {
        id: OutputId::PumpA,
        name: "Pump A (VMA409 channel A)",
        wiring: "ENA ← GPIO12 (pin 32), IN1 high, IN2 low",
        role: "Controller output: fills the tank",
    },
    OutputInfo {
        id: OutputId::PumpB,
        name: "Pump B (VMA409 channel B)",
        wiring: "ENB ← GPIO13 (pin 33), IN3 high, IN4 low",
        role: "Disturbance: drains the tank (used with Pump A)",
    },
    OutputInfo {
        id: OutputId::Dac,
        name: "DAC MCP4725",
        wiring: "I2C 0x60: SDA GPIO2 (pin 3), SCL GPIO3 (pin 5), VCC 3.3 V",
        role: "External controller output, 0–3.3 V",
    },
];

/// Every output driver: take a value from 0.0 (off) to 1.0 (full).
pub trait Actuator: Send {
    fn set(&mut self, fraction: f64) -> Result<(), String>;
}

fn open(id: OutputId) -> Result<Box<dyn Actuator>, String> {
    match id {
        OutputId::PumpA => Ok(Box::new(pump::Pump::open(0)?)),
        OutputId::PumpB => Ok(Box::new(pump::Pump::open(1)?)),
        OutputId::Dac => Ok(Box::new(dac::Mcp4725::open()?)),
    }
}

/// Switch every output off, whether or not this program opened it.
/// Run by the systemd service after the program stops (`--safe-outputs`)
/// and from the panic handler.
pub fn all_safe() {
    pump::all_off();
    dac::all_off();
}

/// Where the controller's output goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Output {
    /// Only the simulated tank: nothing real moves.
    SimulationOnly,
    Device(OutputId),
}

impl Output {
    pub fn label(&self) -> &'static str {
        match self {
            Output::SimulationOnly => "Simulation only",
            Output::Device(OutputId::PumpA) => "Pump A",
            Output::Device(OutputId::PumpB) => "Pump B",
            Output::Device(OutputId::Dac) => "DAC 0–3.3 V",
        }
    }
}

/// The output choices on the Control page. Pump B is the disturbance, so
/// it is not offered as the controller output.
pub fn choices() -> Vec<Output> {
    vec![
        Output::SimulationOnly,
        Output::Device(OutputId::PumpA),
        Output::Device(OutputId::Dac),
    ]
}

// ---------------------------------------------------------------------------
// State shown on the screen
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputStatus {
    /// Is this output currently being driven?
    pub active: bool,
    /// The value last written, 0.0..=1.0.
    pub value: f64,
    pub error: Option<String>,
}

pub type OutputBoard = Arc<Mutex<Vec<OutputStatus>>>;

pub fn new_board() -> OutputBoard {
    Arc::new(Mutex::new(vec![OutputStatus::default(); OUTPUTS.len()]))
}

/// One output as the control loop manages it: opened when first needed,
/// closed (and so switched off) when no longer needed.
pub struct Slot {
    pub id: OutputId,
    actuator: Option<Box<dyn Actuator>>,
    /// After a failure, wait before trying to open again.
    retry_at: Instant,
}

impl Slot {
    pub fn new(id: OutputId) -> Self {
        Slot {
            id,
            actuator: None,
            retry_at: Instant::now(),
        }
    }

    /// Drive the output to `wanted`, or release it with `None`.
    pub fn drive(&mut self, wanted: Option<f64>, status: &mut OutputStatus) {
        let Some(value) = wanted else {
            // `let ... else` runs the else block when the pattern does not
            // match: here, when `wanted` is None. Dropping the driver
            // switches the output off (see each driver's `Drop`).
            self.actuator = None;
            *status = OutputStatus::default();
            return;
        };

        if self.actuator.is_none() {
            if Instant::now() < self.retry_at {
                return; // keep the last error on screen until the retry
            }
            match open(self.id) {
                Ok(actuator) => self.actuator = Some(actuator),
                Err(error) => {
                    status.active = false;
                    status.error = Some(error);
                    self.retry_at = Instant::now() + Duration::from_secs(2);
                    return;
                }
            }
        }

        if let Some(actuator) = self.actuator.as_mut() {
            match actuator.set(value) {
                Ok(()) => {
                    *status = OutputStatus {
                        active: true,
                        value,
                        error: None,
                    }
                }
                Err(error) => {
                    // Close it and try again later, in case the device
                    // was unplugged and comes back.
                    self.actuator = None;
                    status.active = false;
                    status.error = Some(error);
                    self.retry_at = Instant::now() + Duration::from_secs(2);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_ids_match_their_position() {
        for (index, info) in OUTPUTS.iter().enumerate() {
            assert_eq!(info.id as usize, index);
        }
    }

    #[test]
    fn released_slot_reports_idle() {
        let mut slot = Slot::new(OutputId::Dac);
        let mut status = OutputStatus {
            active: true,
            value: 0.5,
            error: Some("old".into()),
        };
        slot.drive(None, &mut status);
        assert_eq!(status, OutputStatus::default());
    }
}
