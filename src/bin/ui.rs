//! Window demo: the same controller and tank, with a live plot and sliders.
//!
//! Run it with:  cargo run --bin ui
//!
//! Files in src/bin/ each become their own program. This one shares the
//! controller and the tank with the terminal demo through the library.
//!
//! Structure (the same as planned for the Raspberry Pi):
//!
//!   control thread  <--- settings ---  UI thread (sliders)
//!         |                                 ^
//!         +------ history of samples -------+   (plot, redrawn by a timer)
//!
//! Both directions go through one shared, lock-protected struct.

use pid_rust_demo::pid::{Gains, Mode, Pid};
use pid_rust_demo::plant::{Plant, WaterTank};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// The user interface, written in Slint's own markup language.
//
// The `slint!` macro compiles this markup at build time and generates a
// Rust struct called `MainWindow` with a getter and setter for every
// property (`set_level_path`, `get_kp`, ...) and an `on_...` method for
// every callback. Typos in the markup are therefore compile errors.
//
// Design choice: the markup is inline to keep the demo in one file. The
// real project will use separate `.slint` files, which get a live
// preview in the editor.
// ---------------------------------------------------------------------------
slint::slint! {
    import { Slider, ComboBox, VerticalBox, HorizontalBox } from "std-widgets.slint";

    // A reusable row: name on the left, slider in the middle, value on the right.
    component LabeledSlider inherits HorizontalBox {
        in property <string> label;
        in property <float> maximum: 1;
        in property <int> decimals: 2;
        // `in-out` = both the Rust code and this component may change it.
        in-out property <float> value;
        callback changed();

        padding: 0;
        Text {
            text: root.label;
            width: 90px;
            vertical-alignment: center;
        }
        Slider {
            minimum: 0;
            maximum: root.maximum;
            // `<=>` is a two-way binding: moving the slider updates
            // `value`, and setting `value` moves the slider.
            value <=> root.value;
            changed => { root.changed(); }
        }
        Text {
            text: root.value.to-fixed(root.decimals);
            width: 50px;
            horizontal-alignment: right;
            vertical-alignment: center;
        }
    }

    // One line of the plot. The line itself is an SVG path string that
    // the Rust code regenerates about 30 times per second.
    component PlotLine inherits Path {
        width: 100%;
        height: 100%;
        // The Rust code draws in a fixed 1000 x 100 coordinate system;
        // `fit: fill` stretches that to whatever size the window has.
        viewbox-width: 1000;
        viewbox-height: 100;
        fit: fill;
        stroke-width: 2px;
    }

    export component MainWindow inherits Window {
        title: "PID demo - water tank";
        preferred-width: 760px;
        preferred-height: 560px;

        // Written by Rust, read by the UI.
        in property <string> level-path;
        in property <string> setpoint-path;
        in property <string> pump-path;
        in property <string> status;

        // Written by the UI, read by Rust when `settings-changed` fires.
        in-out property <float> setpoint: 20;
        in-out property <float> kp;
        in-out property <float> ki;
        in-out property <float> kd;
        in-out property <int> mode-index: 2;
        callback settings-changed();

        VerticalBox {
            // --- Plot area
            Rectangle {
                background: #14181d;
                border-radius: 6px;
                min-height: 220px;
                vertical-stretch: 1;
                clip: true;

                PlotLine { commands: root.pump-path;     stroke: #5c6773; }
                PlotLine { commands: root.setpoint-path; stroke: #e0af68; }
                PlotLine { commands: root.level-path;    stroke: #4fc1ff; }
            }

            // --- Legend and live numbers
            HorizontalBox {
                padding: 0;
                Text { text: "— level";    color: #4fc1ff; }
                Text { text: "— setpoint"; color: #e0af68; }
                Text { text: "— pump";     color: #5c6773; }
                Text {
                    text: root.status;
                    horizontal-alignment: right;
                    horizontal-stretch: 1;
                }
            }

            // --- Controls
            HorizontalBox {
                padding: 0;
                Text {
                    text: "Mode";
                    width: 90px;
                    vertical-alignment: center;
                }
                ComboBox {
                    model: ["P", "PI", "PID"];
                    current-index <=> root.mode-index;
                    selected => { root.settings-changed(); }
                }
            }
            LabeledSlider {
                label: "Setpoint cm";
                maximum: 40;
                decimals: 1;
                value <=> root.setpoint;
                changed => { root.settings-changed(); }
            }
            LabeledSlider {
                label: "Kp";
                maximum: 1;
                value <=> root.kp;
                changed => { root.settings-changed(); }
            }
            LabeledSlider {
                label: "Ki";
                maximum: 0.5;
                value <=> root.ki;
                changed => { root.settings-changed(); }
            }
            LabeledSlider {
                label: "Kd";
                maximum: 0.5;
                value <=> root.kd;
                changed => { root.settings-changed(); }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Data shared between the two threads
// ---------------------------------------------------------------------------

/// What the user can change from the window.
#[derive(Clone, Copy)]
struct Settings {
    mode: Mode,
    gains: Gains,
    setpoint: f64,
}

/// One measurement, kept for the plot.
#[derive(Clone, Copy)]
struct Sample {
    setpoint: f64,
    level: f64,
    pump: f64,
}

/// Everything both threads need to see.
struct Shared {
    settings: Settings,
    /// A double-ended queue: new samples are pushed at the back and old
    /// ones removed from the front, both cheaply.
    history: VecDeque<Sample>,
}

/// The type both threads hold.
///
/// - `Mutex<T>` wraps the data so it can only be reached through
///   `.lock()`. Unlike C++, where a mutex sits next to the data and you
///   must remember to lock it, here forgetting is a compile error.
/// - `Arc<T>` is a thread-safe reference-counted pointer, like
///   `std::shared_ptr`. Each thread gets its own handle to the same data.
type SharedState = Arc<Mutex<Shared>>;

const DT: f64 = 0.05; // control period: 20 Hz, in real time
const HISTORY_SECONDS: f64 = 30.0; // how much the plot shows
const HISTORY_LEN: usize = (HISTORY_SECONDS / DT) as usize;
const MAX_LEVEL_CM: f64 = 40.0; // top of the plot

// Size of the coordinate system the plot lines are drawn in. Must match
// `viewbox-width` / `viewbox-height` in the markup above.
const PLOT_WIDTH: f64 = 1000.0;
const PLOT_HEIGHT: f64 = 100.0;

/// `main` can return a Result. If creating the window fails, the `?`
/// operator returns the error from `main` and Rust prints it.
fn main() -> Result<(), slint::PlatformError> {
    let initial = Settings {
        mode: Mode::Pid,
        gains: Gains {
            kp: 0.15,
            ki: 0.03,
            kd: 0.05,
        },
        setpoint: 20.0,
    };

    let shared: SharedState = Arc::new(Mutex::new(Shared {
        settings: initial,
        history: VecDeque::with_capacity(HISTORY_LEN),
    }));

    // --- Create the window and show the initial values on the sliders.
    // Slint uses f32 for numbers, our maths uses f64, so we convert with
    // `as`. Rust never converts number types silently.
    let window = MainWindow::new()?;
    window.set_kp(initial.gains.kp as f32);
    window.set_ki(initial.gains.ki as f32);
    window.set_kd(initial.gains.kd as f32);
    window.set_setpoint(initial.setpoint as f32);

    // --- UI -> control thread: copy the slider values into the shared
    // settings whenever one of them changes.
    //
    // The closure must not own the window (the window owns the closure,
    // so that would be a reference cycle that never gets freed). A weak
    // handle breaks the cycle, same idea as `std::weak_ptr`.
    let window_weak = window.as_weak();
    // `Arc::clone` copies the pointer, not the data: both handles refer
    // to the same Shared struct.
    let shared_for_callback = Arc::clone(&shared);
    window.on_settings_changed(move || {
        let window = window_weak.unwrap();
        let settings = Settings {
            mode: match window.get_mode_index() {
                0 => Mode::P,
                1 => Mode::Pi,
                // `_` is the catch-all arm; `match` must cover every
                // possible integer.
                _ => Mode::Pid,
            },
            gains: Gains {
                kp: window.get_kp() as f64,
                ki: window.get_ki() as f64,
                kd: window.get_kd() as f64,
            },
            setpoint: window.get_setpoint() as f64,
        };
        // `.lock()` waits for the mutex and returns a guard. The data is
        // reachable only through the guard, and the mutex unlocks by
        // itself when the guard goes out of scope at the end of this
        // closure. `unwrap` crashes if the other thread panicked while
        // holding the lock, which is acceptable for a demo.
        shared_for_callback.lock().unwrap().settings = settings;
    });

    // --- Start the control thread with its own handle to the shared data.
    // We do not keep the thread handle: the thread loops forever and is
    // ended by the operating system when `main` returns.
    let shared_for_control = Arc::clone(&shared);
    thread::spawn(move || run_control_loop(shared_for_control, initial));

    // --- Control thread -> UI: a timer on the UI thread redraws the plot
    // about 30 times per second from the shared history.
    //
    // Design choice: the UI pulls data on its own schedule instead of the
    // control thread pushing every sample. The control loop then never
    // waits for drawing, and UI objects are only touched from the UI
    // thread, which Slint requires.
    let window_weak = window.as_weak();
    let shared_for_timer = Arc::clone(&shared);
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(33),
        move || {
            // Copy the history out and release the lock straight away, so
            // the control thread is blocked for as short a time as
            // possible. The inner braces end the guard's scope.
            let history: Vec<Sample> = {
                let guard = shared_for_timer.lock().unwrap();
                guard.history.iter().copied().collect()
            };

            let window = window_weak.unwrap();

            // `.iter().map(...)` transforms each sample lazily; the
            // closure `|s| s.level` picks one field. `.into()` converts a
            // Rust String into Slint's string type.
            let levels = history.iter().map(|s| s.level);
            window.set_level_path(build_path(levels, MAX_LEVEL_CM).into());

            let setpoints = history.iter().map(|s| s.setpoint);
            window.set_setpoint_path(build_path(setpoints, MAX_LEVEL_CM).into());

            let pumps = history.iter().map(|s| s.pump);
            window.set_pump_path(build_path(pumps, 1.0).into());

            // `.last()` returns an Option: the history is empty at first.
            if let Some(latest) = history.last() {
                let status = format!(
                    "level {:.1} cm    pump {:.0} %",
                    latest.level,
                    latest.pump * 100.0
                );
                window.set_status(status.into());
            }
        },
    );

    // Show the window and process events until it is closed.
    window.run()
}

/// The fixed-rate loop: read settings, measure, compute, actuate, record.
fn run_control_loop(shared: SharedState, initial: Settings) {
    let mut controller = Pid::new(initial.mode, initial.gains, 0.0, 1.0);
    let mut plant: Box<dyn Plant> = Box::new(WaterTank::new());

    loop {
        // Take a copy of the settings. Settings is `Copy`, so reading the
        // field copies the value out; the temporary guard is dropped at
        // the end of this statement and the mutex is free again before
        // any maths.
        let settings = shared.lock().unwrap().settings;
        controller.set_mode(settings.mode);
        controller.set_gains(settings.gains);

        let level = plant.measure();
        let pump = controller.update(settings.setpoint, level, DT);
        plant.step(pump, DT);

        // Record the sample, dropping the oldest once the plot is full.
        {
            let mut guard = shared.lock().unwrap();
            if guard.history.len() == HISTORY_LEN {
                guard.history.pop_front();
            }
            guard.history.push_back(Sample {
                setpoint: settings.setpoint,
                level,
                pump,
            });
        }

        // Simple pacing. Sleeping for a fixed time lets the period drift
        // by however long the work above took; the real project will
        // sleep until an absolute deadline instead.
        thread::sleep(Duration::from_secs_f64(DT));
    }
}

/// Turn a series of values into an SVG path such as "M 0 100 L 1.7 96 ...".
///
/// `impl Iterator<Item = f64>` means "anything that yields f64 values",
/// so the caller can pass levels, setpoints or pump values without first
/// copying them into a separate list.
///
/// `max_value` is the value drawn at the top edge of the plot.
fn build_path(values: impl Iterator<Item = f64>, max_value: f64) -> String {
    let mut path = String::new();

    // `enumerate` pairs each value with its index: (0, v0), (1, v1), ...
    for (index, value) in values.enumerate() {
        // Oldest sample at the left edge; a full history spans the width.
        let x = index as f64 / (HISTORY_LEN - 1) as f64 * PLOT_WIDTH;

        // Screen y grows downwards, so flip it. Keep a small margin so a
        // line at exactly 0 or max is not cut in half by the plot edge.
        let fraction = (value / max_value).clamp(0.0, 1.0);
        let y = PLOT_HEIGHT - (2.0 + fraction * (PLOT_HEIGHT - 4.0));

        // "M" moves the pen to the first point, "L" draws a line to each
        // following one.
        let command = if index == 0 { 'M' } else { 'L' };
        path.push_str(&format!("{command} {x:.1} {y:.1} "));
    }

    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_gives_empty_path() {
        assert_eq!(build_path(std::iter::empty(), 1.0), "");
    }

    #[test]
    fn path_starts_with_move_then_draws_lines() {
        // A Vec can be turned into an iterator with `into_iter()`.
        let path = build_path(vec![0.0, 1.0].into_iter(), 1.0);
        // Zero is at the bottom (y near 100), the maximum at the top.
        assert!(path.starts_with("M 0.0 98.0 L "));
        assert!(path.trim_end().ends_with(" 2.0"));
    }

    #[test]
    fn values_outside_the_range_stay_inside_the_plot() {
        let path = build_path(vec![-5.0, 99.0].into_iter(), 1.0);
        assert!(path.starts_with("M 0.0 98.0 "));
        assert!(path.trim_end().ends_with(" 2.0"));
    }
}
