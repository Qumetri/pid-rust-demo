//! Entry point: wires the controller to the plant and draws the result.
//!
//! The structure mirrors the real project on purpose:
//!
//!   control thread  --(channel of Samples)-->  main thread (display)
//!
//! On the Raspberry Pi the display side becomes the touchscreen UI and
//! the plant becomes real sensors and pumps; the shape stays the same.

// The controller and the tank live in the library (src/lib.rs), which is
// named after the package with dashes turned into underscores. There are
// no header files and no #include; `use` brings names into scope, like
// `using` in C++.
use pid_rust_demo::pid::{Gains, Mode, Pid};
use pid_rust_demo::plant::{Plant, WaterTank};
use std::sync::mpsc; // "multi-producer, single-consumer" channel
use std::thread;
use std::time::Duration;

/// One row of data sent from the control thread to the display.
struct Sample {
    time_s: f64,
    setpoint: f64,
    level: f64,
    pump: f64,
}

/// Simulation settings. Constants need an explicit type.
const DT: f64 = 0.1; // control period: 10 Hz
const DURATION_S: f64 = 40.0;
const MAX_LEVEL_CM: f64 = 40.0; // top of the plot

fn main() {
    // --- Read the mode from the command line: `cargo run -- pid`
    //
    // `std::env::args()` yields the arguments; `.nth(1)` returns an
    // Option<String> because the user may not have given one.
    let mode = match parse_mode(std::env::args().nth(1)) {
        // `Result` is how Rust reports failure: Ok(value) or Err(reason).
        // There are no exceptions; errors are ordinary return values that
        // the compiler will not let you ignore.
        Ok(mode) => mode,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: cargo run -- [p|pi|pd|pid]");
            std::process::exit(1);
        }
    };

    // Try changing these and re-running to see the effect.
    let gains = Gains {
        kp: 0.15,
        ki: 0.03,
        kd: 0.05,
    };
    let controller = Pid::new(mode, gains, 0.0, 1.0);

    // `Box<dyn Plant>` = "a heap-allocated something that implements
    // Plant", like `std::unique_ptr<Plant>` pointing at a derived class.
    // The control loop below never learns it is a WaterTank.
    let plant: Box<dyn Plant> = Box::new(WaterTank::new());

    // --- Create the channel. `tx` sends, `rx` receives.
    let (tx, rx) = mpsc::channel::<Sample>();

    // --- Start the control thread.
    //
    // `move` transfers ownership of `controller`, `plant` and `tx` into
    // the thread. After this line main() can no longer touch them - the
    // compiler enforces it. That is the core of Rust's thread safety:
    // data has exactly one owner, so two threads cannot fight over it.
    let control_thread = thread::spawn(move || {
        run_control_loop(controller, plant, tx);
    });

    println!("mode: {mode:?}   gains: {gains:?}");
    println!("time   level                                       pump");

    // --- Display loop.
    // Iterating over `rx` blocks until a sample arrives and ends
    // automatically when the sender is dropped (i.e. the control thread
    // finished). No "done" flag is needed.
    for sample in rx {
        // Print one row per half second to keep the output short.
        let step = (sample.time_s / DT).round() as u64;
        if step.is_multiple_of(5) {
            println!("{}", render_row(&sample));
        }
    }

    // Wait for the thread to finish. `join` returns a Result that is Err
    // if the thread panicked; `expect` turns that into a crash with a
    // message, which is fine for a demo.
    control_thread.join().expect("control thread panicked");
}

/// The fixed-rate loop: measure, compute, actuate, report.
///
/// The parameters are taken by value, so this function owns them. `mut`
/// in front of a parameter name allows us to modify our own copy.
fn run_control_loop(mut controller: Pid, mut plant: Box<dyn Plant>, tx: mpsc::Sender<Sample>) {
    let steps = (DURATION_S / DT) as u64;

    for step in 0..steps {
        let time_s = step as f64 * DT;

        // Setpoint profile: go to 20 cm, then to 30 cm at half time.
        // `if` is an expression too, so it can produce a value.
        let setpoint = if time_s < DURATION_S / 2.0 {
            20.0
        } else {
            30.0
        };

        let level = plant.measure();
        let pump = controller.update(setpoint, level, DT);
        plant.step(pump, DT);

        // Send the sample to the display. `send` fails only if the
        // receiver is gone; in that case there is nobody to show data
        // to, so we stop.
        if tx
            .send(Sample {
                time_s,
                setpoint,
                level,
                pump,
            })
            .is_err()
        {
            break;
        }

        // Run 20x faster than real time so the demo finishes in ~2 s.
        // The real project sleeps for the full period instead.
        thread::sleep(Duration::from_secs_f64(DT / 20.0));
    }
    // `tx` is dropped here when the function returns, which closes the
    // channel and ends the `for sample in rx` loop in main().
}

/// Turn the optional command-line argument into a Mode.
///
/// Returns `Result<Mode, String>`: either the mode or an error message.
fn parse_mode(argument: Option<String>) -> Result<Mode, String> {
    // `as_deref()` converts Option<String> into Option<&str> so we can
    // match against string literals.
    match argument.as_deref() {
        None => Ok(Mode::Pid), // default when nothing is given
        Some("p") => Ok(Mode::P),
        Some("pi") => Ok(Mode::Pi),
        Some("pd") => Ok(Mode::Pd),
        Some("pid") => Ok(Mode::Pid),
        Some(other) => Err(format!("unknown mode '{other}'")),
    }
}

/// Draw one text row: a bar for the level, a marker for the setpoint.
///
/// `&Sample` is a borrow: we only look at the sample, we do not take it.
fn render_row(sample: &Sample) -> String {
    const WIDTH: usize = 40;

    // Map a level in cm to a column index.
    let to_column = |value: f64| -> usize {
        let fraction = (value / MAX_LEVEL_CM).clamp(0.0, 1.0);
        (fraction * (WIDTH - 1) as f64).round() as usize
    };

    let level_column = to_column(sample.level);
    let setpoint_column = to_column(sample.setpoint);

    // Build the bar character by character.
    let mut bar = String::with_capacity(WIDTH);
    for column in 0..WIDTH {
        let symbol = if column == setpoint_column {
            '|' // where we want to be
        } else if column <= level_column {
            '#' // where we are
        } else {
            ' '
        };
        bar.push(symbol);
    }

    format!(
        "{:5.1}s {:5.1}cm {} {:3.0}%",
        sample.time_s,
        sample.level,
        bar,
        sample.pump * 100.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing() {
        assert_eq!(parse_mode(None), Ok(Mode::Pid));
        assert_eq!(parse_mode(Some("pi".to_string())), Ok(Mode::Pi));
        assert!(parse_mode(Some("banana".to_string())).is_err());
    }

    /// End-to-end check without threads: PI control must settle on the
    /// setpoint, P-only must not (it leaves a steady-state error).
    #[test]
    fn pi_reaches_setpoint_but_p_does_not() {
        let settle = |mode: Mode| -> f64 {
            let mut pid = Pid::new(
                mode,
                Gains {
                    kp: 0.15,
                    ki: 0.03,
                    kd: 0.05,
                },
                0.0,
                1.0,
            );
            let mut tank = WaterTank::new();
            for _ in 0..3000 {
                let output = pid.update(20.0, tank.measure(), DT);
                tank.step(output, DT);
            }
            tank.measure()
        };

        assert!((settle(Mode::Pi) - 20.0).abs() < 0.1);
        assert!((settle(Mode::P) - 20.0).abs() > 1.0);
    }
}
