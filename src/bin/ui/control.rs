//! The control side: the settings the screen can change, the samples the
//! plot shows, and the control loop that runs in its own thread.

use pid_rust_demo::outputs::{Output, OutputBoard, OutputId, Slot, OUTPUTS};
use pid_rust_demo::pid::{Gains, Mode, Options, Pid};
use pid_rust_demo::plant::{Plant, WaterTank};
use pid_rust_demo::sensors::{Input, SensorBoard, SensorInfo, SENSORS};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// `pub` makes an item visible outside this file, here to main.rs and the
// other files of the program. Struct fields need their own `pub`.

/// What the user can change from the screen.
#[derive(Clone, Copy, PartialEq)]
pub struct Settings {
    pub mode: Mode,
    pub gains: Gains,
    pub options: Options,
    pub out_min: f64,
    pub out_max: f64,
    pub setpoint: f64,
    /// Automatic (the controller sets the output) or manual (by hand).
    pub auto: bool,
    pub manual_output: f64,
    /// Second pump draining the tank (real rig) or extra outflow (simulator).
    pub disturbance: f64,
    /// Where the measurement comes from: the simulator or a real sensor.
    pub input: Input,
    /// Where the output goes, besides the simulated tank.
    pub output: Output,
}

/// One measurement, kept for the plot.
#[derive(Clone, Copy)]
pub struct Sample {
    pub setpoint: f64,
    /// `f64::NAN` when the input had no fresh reading. The plot leaves a
    /// gap there instead of drawing a made-up value.
    pub measured: f64,
    pub output: f64,
}

/// Everything the control thread and the UI thread both need to see.
pub struct Shared {
    pub settings: Settings,
    /// A double-ended queue: new samples are pushed at the back and old
    /// ones removed from the front, both cheaply.
    pub history: VecDeque<Sample>,
}

/// The type both threads hold.
///
/// - `Mutex<T>` wraps the data so it can only be reached through
///   `.lock()`. Unlike C++, where a mutex sits next to the data and you
///   must remember to lock it, here forgetting is a compile error.
/// - `Arc<T>` is a thread-safe reference-counted pointer, like
///   `std::shared_ptr`. Each thread gets its own handle to the same data.
pub type SharedState = Arc<Mutex<Shared>>;

/// Control period in seconds: 20 times per second, in real time.
pub const DT: f64 = 0.05;
/// How many seconds the plot shows.
pub const HISTORY_SECONDS: f64 = 30.0;
/// How many samples that is. Constants can be computed from other
/// constants; the compiler works this out before the program runs.
pub const HISTORY_LEN: usize = (HISTORY_SECONDS / DT) as usize;

/// Output used in automatic while the selected input has no fresh
/// reading. Off is the safe choice for a pump: the tank cannot overflow.
const SAFE_OUTPUT: f64 = 0.0;

/// The fixed-rate loop: read settings, measure, compute, actuate, record.
pub fn run_control_loop(
    shared: SharedState,
    board: SensorBoard,
    output_board: OutputBoard,
    initial: Settings,
) {
    let mut controller = Pid::new(initial.mode, initial.gains, 0.0, 1.0);
    // The simulated tank always runs on the controller's output, so the
    // demo works with or without real hardware.
    let mut plant: Box<dyn Plant> = Box::new(WaterTank::new());
    let mut last_input = initial.input;
    let mut had_reading = true;
    // One slot per real output; each opens its device when first needed.
    let mut slots: Vec<Slot> = OUTPUTS.iter().map(|info| Slot::new(info.id)).collect();

    loop {
        // Take a copy of the settings. Settings is `Copy`, so reading the
        // field copies the value out; the temporary guard is dropped at
        // the end of this statement and the mutex is free again before
        // any maths.
        let settings = shared.lock().unwrap().settings;
        controller.set_mode(settings.mode);
        controller.set_gains(settings.gains);
        controller.set_options(settings.options);
        controller.set_limits(settings.out_min, settings.out_max);

        // Bumpless input switching: start clean on the new signal.
        if settings.input != last_input {
            controller.reset();
            last_input = settings.input;
        }

        // `Option<f64>`: None when the selected sensor has no fresh value.
        let measurement = match settings.input {
            Input::SimulatedTank => Some(plant.measure()),
            Input::Sensor { sensor, signal } => {
                let info: &SensorInfo = &SENSORS[sensor as usize];
                board.lock().unwrap()[sensor as usize].fresh_value(signal, info.stale_after)
            }
        };

        // A `match` on a pair handles all four combinations explicitly.
        let output = match (settings.auto, measurement) {
            // Manual: the hand-set value goes out unchanged, and the
            // controller follows along for a bumpless return to auto.
            (false, Some(value)) => {
                controller.track(settings.setpoint, value, settings.manual_output);
                settings.manual_output
            }
            (false, None) => settings.manual_output,
            (true, Some(value)) => {
                // Coming back from a failure: do not differentiate across
                // the gap or reuse an integral from before it.
                if !had_reading {
                    controller.reset();
                }
                controller.update(settings.setpoint, value, DT)
            }
            (true, None) => SAFE_OUTPUT,
        };
        had_reading = measurement.is_some();

        plant.set_disturbance(settings.disturbance);
        plant.step(output, DT);

        // Real outputs. Pump B (the disturbance) only runs together with
        // pump A, i.e. when the real pump rig is being controlled.
        let pump_rig = settings.output == Output::Device(OutputId::PumpA);
        {
            let mut states = output_board.lock().unwrap();
            for slot in &mut slots {
                let wanted = if settings.output == Output::Device(slot.id) {
                    Some(output)
                } else if slot.id == OutputId::PumpB && pump_rig {
                    Some(settings.disturbance)
                } else {
                    None
                };
                slot.drive(wanted, &mut states[slot.id as usize]);
            }
        }

        // Record the sample, dropping the oldest once the plot is full.
        {
            let mut guard = shared.lock().unwrap();
            if guard.history.len() == HISTORY_LEN {
                guard.history.pop_front();
            }
            guard.history.push_back(Sample {
                setpoint: settings.setpoint,
                measured: measurement.unwrap_or(f64::NAN),
                output,
            });
        }

        // Simple pacing. Sleeping for a fixed time lets the period drift
        // by however long the work above took; the real project will
        // sleep until an absolute deadline instead (the sensor threads in
        // src/sensors/mod.rs already do).
        thread::sleep(Duration::from_secs_f64(DT));
    }
}

/// The line under the plot: live numbers, or why the output is what it is.
pub fn status_text(settings: &Settings, latest: &Sample, output_error: Option<String>) -> String {
    let unit = settings.input.unit();
    let output = latest.output * 100.0;
    let mut text = match (settings.auto, latest.measured.is_nan()) {
        (true, true) => format!("{}: no reading, output off", settings.input.label()),
        (false, true) => format!("MANUAL  output {output:.0} %  (no reading)"),
        (false, false) => format!(
            "MANUAL  {:.1} {unit}  output {output:.0} %",
            latest.measured
        ),
        (true, false) => format!("{:.1} {unit}  output {output:.0} %", latest.measured),
    };
    text.push_str(&format!("  → {}", settings.output.label()));
    if let Some(error) = output_error {
        text.push_str(&format!("\nOutput problem: {error}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_explains_a_missing_reading() {
        let settings = Settings {
            mode: Mode::Pid,
            gains: Gains {
                kp: 0.1,
                ki: 0.0,
                kd: 0.0,
            },
            options: Options::default(),
            out_min: 0.0,
            out_max: 1.0,
            setpoint: 20.0,
            auto: true,
            manual_output: 0.0,
            disturbance: 0.0,
            input: Input::SimulatedTank,
            output: Output::SimulationOnly,
        };
        let sample = Sample {
            setpoint: 20.0,
            measured: f64::NAN,
            output: 0.0,
        };
        let text = status_text(&settings, &sample, Some("PWM off".into()));
        assert!(text.starts_with("Simulated tank: no reading, output off"));
        assert!(text.ends_with("Output problem: PWM off"));
    }
}
