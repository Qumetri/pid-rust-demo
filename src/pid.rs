//! The PID controller itself.
//!
//! This file is pure maths: no printing, no threads, no hardware. Keeping
//! it that way makes it trivial to unit-test (see the bottom of the file)
//! and lets the real project reuse it unchanged.
//!
//! Features (the list from docs/PROJECT-NOTES.md §4):
//! - P, PI, PD and PID modes;
//! - derivative on measurement, with an optional low-pass filter;
//! - anti-windup, which can be switched off to show what it prevents;
//! - adjustable output limits;
//! - bumpless switching from manual to automatic;
//! - normal or inverted action.

/// Which terms of the controller are active.
///
/// A Rust `enum` is a closed list of alternatives. The compiler forces
/// every `match` on it to handle all variants, so adding a new mode later
/// produces compile errors at each place that needs updating.
///
/// `derive` asks the compiler to generate common behaviour for us:
/// - `Debug`     lets us print the value with `{:?}`
/// - `Clone/Copy` make it cheap to pass by value, like an `int` in C++
/// - `PartialEq` enables `==`
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    P,
    Pi,
    Pd,
    Pid,
}

impl Mode {
    fn uses_integral(self) -> bool {
        // `matches!` is a shorthand for a `match` that returns true or false.
        matches!(self, Mode::Pi | Mode::Pid)
    }

    fn uses_derivative(self) -> bool {
        matches!(self, Mode::Pd | Mode::Pid)
    }
}

/// Which way the process responds to the output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// More output raises the measurement: a pump filling a tank, a heater.
    Normal,
    /// More output lowers the measurement: a drain pump, a cooling fan, or
    /// an ultrasonic sensor that measures the distance to the water.
    Inverted,
}

/// Tuning values. A plain struct with public fields, like a C++ struct.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gains {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
}

/// Behaviour switches, all of which the touchscreen can change while the
/// controller runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    pub action: Action,
    /// Stop the integral growing while the output is pinned at a limit.
    pub anti_windup: bool,
    /// Time constant of the derivative filter in seconds; 0 = no filter.
    /// A noisy sensor (such as the ultrasonic one) makes the raw D term
    /// jumpy; filtering trades a little delay for a calmer output.
    pub d_filter_s: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            action: Action::Normal,
            anti_windup: true,
            d_filter_s: 0.0,
        }
    }
}

/// The controller state.
///
/// Fields are private (no `pub`), so code outside this file can only use
/// the methods below. That is how Rust does encapsulation: privacy is per
/// module (file), not per class.
pub struct Pid {
    mode: Mode,
    gains: Gains,
    options: Options,
    /// Output limits, e.g. 0.0..=1.0 for "pump off" to "pump full".
    out_min: f64,
    out_max: f64,
    /// Running sum for the I term.
    integral: f64,
    /// Previous measurement, needed for the D term.
    ///
    /// `Option<f64>` means "either a number or nothing". Rust has no null;
    /// on the very first update there is no previous value, and the type
    /// makes us handle that case explicitly instead of reading garbage.
    last_measurement: Option<f64>,
    /// The D term after filtering, kept between calls.
    d_filtered: f64,
}

impl Pid {
    /// Constructor. By convention it is an ordinary function called `new`;
    /// Rust has no special constructor syntax.
    pub fn new(mode: Mode, gains: Gains, out_min: f64, out_max: f64) -> Self {
        Pid {
            mode,
            gains,
            options: Options::default(),
            out_min,
            out_max,
            integral: 0.0,
            last_measurement: None,
            d_filtered: 0.0,
        }
    }

    /// Change the tuning while the controller is running.
    ///
    /// The window demo calls these when a slider moves. They are the only
    /// way to modify the private fields from outside this file.
    pub fn set_gains(&mut self, gains: Gains) {
        self.gains = gains;
    }

    pub fn set_mode(&mut self, mode: Mode) {
        if mode != self.mode {
            // Start the new mode with a clean integral so that whatever
            // the previous mode accumulated does not cause a jump.
            self.integral = 0.0;
            self.mode = mode;
        }
    }

    pub fn set_options(&mut self, options: Options) {
        if options.action != self.options.action {
            // The integral was built up for the opposite direction.
            self.integral = 0.0;
        }
        self.options = options;
    }

    /// Change the output limits. A minimum above the maximum is ignored.
    pub fn set_limits(&mut self, out_min: f64, out_max: f64) {
        if out_min <= out_max {
            self.out_min = out_min;
            self.out_max = out_max;
        }
    }

    /// Forget the controller's history: the accumulated integral and the
    /// previous measurement.
    ///
    /// Called when the input changes (another sensor, or a sensor that
    /// came back after a failure). Without it the D term would see a jump
    /// between two unrelated measurements, and the integral would carry
    /// over a value built up for a different signal.
    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.last_measurement = None;
        self.d_filtered = 0.0;
    }

    /// +1 or -1: the error's sign for the configured action.
    fn sign(&self) -> f64 {
        match self.options.action {
            Action::Normal => 1.0,
            Action::Inverted => -1.0,
        }
    }

    /// Call this every period while the output is set by hand (manual
    /// mode) instead of `update`.
    ///
    /// It keeps the controller's memory in step with the process, so that
    /// switching back to automatic continues from the manual output
    /// instead of jumping ("bumpless transfer"). It does this by setting
    /// the integral to whatever makes P + I equal the manual output right
    /// now. In P and PD mode there is no integral to absorb the
    /// difference, so a small jump on switching is unavoidable.
    pub fn track(&mut self, setpoint: f64, measurement: f64, output: f64) {
        let error = self.sign() * (setpoint - measurement);
        self.last_measurement = Some(measurement);
        self.d_filtered = 0.0;
        if self.mode.uses_integral() {
            self.integral = output - self.gains.kp * error;
        }
    }

    /// Compute one control step and return the new output.
    ///
    /// `&mut self` means "I need exclusive, mutable access to this Pid".
    /// The compiler guarantees nobody else is reading or writing it at the
    /// same time, which is what makes Rust threads safe later on.
    ///
    /// `dt` is the time since the previous call, in seconds.
    pub fn update(&mut self, setpoint: f64, measurement: f64, dt: f64) -> f64 {
        let sign = self.sign();
        let error = sign * (setpoint - measurement);

        // --- P term: react to the current error.
        let p = self.gains.kp * error;

        // --- D term: react to how fast the measurement is changing.
        //
        // Design choice: we differentiate the *measurement*, not the error.
        // If we used the error, a sudden setpoint change would look like an
        // infinitely fast change and produce a huge output spike
        // ("derivative kick").
        //
        // `match` on an Option: both cases must be covered.
        let raw_d = match self.last_measurement {
            Some(previous) => -sign * self.gains.kd * (measurement - previous) / dt,
            None => 0.0, // first call: no history yet
        };
        self.last_measurement = Some(measurement);

        // Optional first-order low-pass filter: move part of the way from
        // the old filtered value towards the new raw one. The larger the
        // time constant, the smaller each step and the smoother the result.
        let tau = self.options.d_filter_s;
        self.d_filtered = if tau > 0.0 {
            self.d_filtered + (raw_d - self.d_filtered) * dt / (tau + dt)
        } else {
            raw_d
        };

        // --- I term: remove the steady error that P alone leaves behind.
        // We compute a candidate integral first and only keep it if allowed
        // (see anti-windup below).
        let candidate_integral = self.integral + self.gains.ki * error * dt;

        // Pick the active terms. `if` is an expression: it returns a value.
        let i = if self.mode.uses_integral() {
            candidate_integral
        } else {
            0.0
        };
        let d = if self.mode.uses_derivative() {
            self.d_filtered
        } else {
            0.0
        };
        let unclamped = p + i + d;
        let output = unclamped.clamp(self.out_min, self.out_max);

        // --- Anti-windup.
        // A pump cannot do more than 100 %. If the output is already
        // pinned at a limit and we kept integrating, the integral would
        // grow without bound and take a long time to unwind, causing a big
        // overshoot. So: only accept the new integral when the output is
        // not saturated, or when the error is pulling it back inside.
        // With anti-windup switched off we always accept it, which is
        // useful to show the overshoot it otherwise prevents.
        if self.mode.uses_integral() {
            let saturated_high = unclamped > self.out_max && error > 0.0;
            let saturated_low = unclamped < self.out_min && error < 0.0;
            if !self.options.anti_windup || (!saturated_high && !saturated_low) {
                self.integral = candidate_integral;
            }
        }

        output // last expression without `;` is the return value
    }
}

// ---------------------------------------------------------------------------
// Unit tests live next to the code. `#[cfg(test)]` means this module is only
// compiled for `cargo test`, so it adds nothing to the real program.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*; // bring Pid, Mode, Gains into scope

    const GAINS: Gains = Gains {
        kp: 2.0,
        ki: 1.0,
        kd: 0.5,
    };

    #[test]
    fn p_mode_is_proportional_to_error() {
        let mut pid = Pid::new(Mode::P, GAINS, -100.0, 100.0);
        // error = 3, kp = 2  ->  output 6
        assert_eq!(pid.update(10.0, 7.0, 0.1), 6.0);
    }

    #[test]
    fn output_respects_limits() {
        let mut pid = Pid::new(Mode::P, GAINS, 0.0, 1.0);
        assert_eq!(pid.update(100.0, 0.0, 0.1), 1.0);
        assert_eq!(pid.update(-100.0, 0.0, 0.1), 0.0);
        pid.set_limits(0.2, 0.6);
        assert_eq!(pid.update(100.0, 0.0, 0.1), 0.6);
        assert_eq!(pid.update(-100.0, 0.0, 0.1), 0.2);
    }

    #[test]
    fn integral_accumulates_over_time() {
        let mut pid = Pid::new(Mode::Pi, GAINS, -100.0, 100.0);
        let first = pid.update(1.0, 0.0, 0.1);
        let second = pid.update(1.0, 0.0, 0.1);
        // Same error both times, so only the growing I term can raise it.
        assert!(second > first);
    }

    #[test]
    fn pd_mode_has_no_integral() {
        let mut pid = Pid::new(Mode::Pd, GAINS, -100.0, 100.0);
        let first = pid.update(1.0, 0.0, 0.1);
        let second = pid.update(1.0, 0.0, 0.1);
        // Constant error and measurement: P only, no growth.
        assert_eq!(first, second);
    }

    #[test]
    fn integral_does_not_wind_up_while_saturated() {
        let mut pid = Pid::new(Mode::Pi, GAINS, 0.0, 1.0);
        // Hold a large error for a long time; output is pinned at 1.0.
        for _ in 0..1000 {
            pid.update(100.0, 0.0, 0.1);
        }
        // The error flips sign. Without anti-windup the stored integral
        // would keep the output at 1.0 for many more steps.
        assert_eq!(pid.update(0.0, 1.0, 0.1), 0.0);
    }

    #[test]
    fn without_anti_windup_the_integral_does_wind_up() {
        let mut pid = Pid::new(Mode::Pi, GAINS, 0.0, 1.0);
        pid.set_options(Options {
            anti_windup: false,
            ..Options::default()
        });
        for _ in 0..1000 {
            pid.update(100.0, 0.0, 0.1);
        }
        // The huge stored integral keeps the output pinned high.
        assert_eq!(pid.update(0.0, 1.0, 0.1), 1.0);
    }

    #[test]
    fn inverted_action_flips_the_output() {
        let mut pid = Pid::new(Mode::P, GAINS, -100.0, 100.0);
        pid.set_options(Options {
            action: Action::Inverted,
            ..Options::default()
        });
        // Measurement below setpoint: an inverted process needs *less*
        // output to rise, so the output goes negative.
        assert_eq!(pid.update(10.0, 7.0, 0.1), -6.0);
    }

    #[test]
    fn derivative_filter_softens_a_jump() {
        let gains = Gains {
            kp: 0.0,
            ki: 0.0,
            kd: 1.0,
        };
        let mut raw = Pid::new(Mode::Pd, gains, -100.0, 100.0);
        let mut filtered = Pid::new(Mode::Pd, gains, -100.0, 100.0);
        filtered.set_options(Options {
            d_filter_s: 0.5,
            ..Options::default()
        });
        for pid in [&mut raw, &mut filtered] {
            pid.update(0.0, 0.0, 0.1);
        }
        // The measurement jumps by 1: the raw D term reacts fully at once.
        let raw_d = raw.update(0.0, 1.0, 0.1);
        let filtered_d = filtered.update(0.0, 1.0, 0.1);
        assert_eq!(raw_d, -10.0);
        assert!(filtered_d < 0.0 && filtered_d > raw_d / 2.0);
    }

    #[test]
    fn manual_to_automatic_is_bumpless() {
        let gains = Gains {
            kp: 0.15,
            ki: 0.03,
            kd: 0.0,
        };
        let mut pid = Pid::new(Mode::Pi, gains, 0.0, 1.0);
        // Hand-set output 40 % while the level sits 5 below the setpoint.
        for _ in 0..10 {
            pid.track(20.0, 15.0, 0.4);
        }
        // Back to automatic: the first output is the manual one plus only
        // one step of integral growth.
        let output = pid.update(20.0, 15.0, 0.05);
        assert!((output - 0.4).abs() < 0.01, "jumped to {output}");
    }

    #[test]
    fn reset_clears_integral_and_derivative_history() {
        let mut pid = Pid::new(Mode::Pid, GAINS, -100.0, 100.0);
        pid.update(1.0, 0.0, 0.1);
        pid.update(1.0, 0.0, 0.1);
        pid.reset();
        // A fresh controller gives exactly the same first output.
        let mut fresh = Pid::new(Mode::Pid, GAINS, -100.0, 100.0);
        assert_eq!(pid.update(1.0, 5.0, 0.1), fresh.update(1.0, 5.0, 0.1));
    }

    #[test]
    fn no_derivative_kick_on_setpoint_change() {
        let mut pid = Pid::new(
            Mode::Pid,
            Gains {
                kp: 0.0,
                ki: 0.0,
                kd: 5.0,
            },
            -100.0,
            100.0,
        );
        pid.update(0.0, 1.0, 0.1);
        // Setpoint jumps, measurement does not move: D term must stay zero.
        assert_eq!(pid.update(50.0, 1.0, 0.1), 0.0);
    }
}
