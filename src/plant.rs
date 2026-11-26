//! The simulated process ("plant") that the controller acts on.
//!
//! In the real project this is where sensors and pumps go. Here it is a
//! small physics model, so the demo runs on any computer.

/// Anything the controller can drive.
///
/// A `trait` is Rust's version of an abstract base class / interface: a
/// list of methods a type promises to provide. The control loop in
/// main.rs only knows about this trait, so swapping the simulated tank
/// for real hardware later means writing another `impl Plant for ...`
/// and changing nothing else.
///
/// `Send` is a marker meaning "safe to move to another thread". We need
/// it because the plant is handed to the control thread in main.rs.
pub trait Plant: Send {
    /// Apply a control output in 0.0..=1.0 and advance time by `dt` seconds.
    fn step(&mut self, control: f64, dt: f64);

    /// Read the process value (what a sensor would report).
    /// `&self` = read-only access; this method cannot modify the plant.
    fn measure(&self) -> f64;

    /// Set a disturbance in 0.0..=1.0, such as a second pump draining the
    /// tank. It lets the demo show how the controller rejects a load
    /// change.
    ///
    /// A trait method can have a default body. Plants without a
    /// disturbance simply inherit this one, which does nothing.
    fn set_disturbance(&mut self, _amount: f64) {}
}

/// A water tank filled by a pump, with a drain hole at the bottom.
pub struct WaterTank {
    /// Current water level in centimetres.
    level_cm: f64,
    /// Cross-section of the tank in cm^2.
    area_cm2: f64,
    /// Pump flow at full power, in cm^3 per second.
    pump_max_flow: f64,
    /// How fast water leaves through the drain (bigger = faster).
    drain_coeff: f64,
    /// Extra outflow from a second (drain) pump, 0.0..=1.0 of full power.
    disturbance: f64,
}

impl WaterTank {
    pub fn new() -> Self {
        WaterTank {
            level_cm: 0.0,
            area_cm2: 50.0,
            pump_max_flow: 300.0,
            drain_coeff: 20.0,
            disturbance: 0.0,
        }
    }
}

// `Default` is the standard trait for "a sensible default value". Types
// with a no-argument `new` conventionally implement it too, so generic
// code can create one with `WaterTank::default()`.
impl Default for WaterTank {
    fn default() -> Self {
        Self::new()
    }
}

// This block is what makes WaterTank usable wherever a Plant is expected.
impl Plant for WaterTank {
    fn step(&mut self, control: f64, dt: f64) {
        let inflow = control.clamp(0.0, 1.0) * self.pump_max_flow;

        // Torricelli's law: outflow grows with the square root of the
        // level. This makes the tank non-linear, so the same gains behave
        // differently at different levels - a nice thing to demonstrate.
        let outflow = self.drain_coeff * self.level_cm.max(0.0).sqrt();

        // The drain pump can only remove water that is there; at an empty
        // tank it just runs dry.
        let pumped_out = if self.level_cm > 0.0 {
            self.disturbance * self.pump_max_flow * 0.5
        } else {
            0.0
        };

        // Volume change divided by area gives level change.
        self.level_cm += (inflow - outflow - pumped_out) / self.area_cm2 * dt;
        self.level_cm = self.level_cm.max(0.0); // a tank cannot be less than empty
    }

    fn measure(&self) -> f64 {
        self.level_cm
    }

    fn set_disturbance(&mut self, amount: f64) {
        self.disturbance = amount.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pump_on_fills_the_tank() {
        let mut tank = WaterTank::new();
        tank.step(1.0, 1.0);
        assert!(tank.measure() > 0.0);
    }

    #[test]
    fn pump_off_drains_the_tank() {
        let mut tank = WaterTank::new();
        for _ in 0..100 {
            tank.step(1.0, 0.1);
        }
        let full = tank.measure();
        for _ in 0..100 {
            tank.step(0.0, 0.1);
        }
        assert!(tank.measure() < full);
    }

    #[test]
    fn disturbance_lowers_the_level() {
        let run = |disturbance: f64| {
            let mut tank = WaterTank::new();
            tank.set_disturbance(disturbance);
            for _ in 0..200 {
                tank.step(0.5, 0.1);
            }
            tank.measure()
        };
        assert!(run(0.5) < run(0.0));
    }
}
