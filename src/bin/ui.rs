//! The touchscreen program: the controller with a live plot, its tuning,
//! and the sensors and outputs it can use.
//!
//! Run it with:  cargo run --bin ui
//! On the Pi:    see deploy/pid-ui.service (fullscreen on the touchscreen)
//! Safe stop:    `ui --safe-outputs` switches every real output off and
//!               exits; the service runs it after the program stops.
//!
//! Three pages, chosen with tabs at the bottom:
//!   Control  plot, auto/manual, setpoint, output, disturbance, input
//!            and output selection
//!   Tuning   mode, gains, action, anti-windup, derivative filter, limits
//!   Sensors  every sensor and output with its wiring and live state
//!
//! Structure:
//!
//!   sensor threads --- latest values ---> SensorBoard
//!                                              |
//!   control thread  <--- settings ---  UI thread (pages, sliders)
//!     |      |                              ^
//!     |      +------ history of samples ----+   (plot, redrawn by a timer)
//!     +-- drives the real outputs, state --> OutputBoard
//!
//! Every arrow goes through a shared, lock-protected struct.

use pid_rust_demo::outputs::{self, Output, OutputBoard, OutputId, Slot, OUTPUTS};
use pid_rust_demo::pid::{Action, Gains, Mode, Options, Pid};
use pid_rust_demo::plant::{Plant, WaterTank};
use pid_rust_demo::sensors::{self, Health, Input, SensorBoard, SensorInfo, SENSORS};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::collections::VecDeque;
use std::rc::Rc;
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
// Sizes are in logical pixels. The touchscreen runs at a scale factor of 2,
// so its 720 x 1280 pixels are a 360 x 640 logical screen; the desktop
// window uses the same size so it previews the panel layout.
// ---------------------------------------------------------------------------
slint::slint! {
    import { Slider, ComboBox, VerticalBox, HorizontalBox, Switch, ScrollView, Palette } from "std-widgets.slint";

    // One card on the Sensors page, for a sensor or an output. `export`
    // makes Slint generate a matching Rust struct, `DeviceRow`.
    export struct DeviceRow {
        name: string,
        wiring: string,
        // What it is for, e.g. "Monitoring only".
        role: string,
        // Live values, one per line; empty when there is nothing to show.
        reading: string,
        // "OK", "Off", "Idle", or the error message.
        status: string,
        // 0 off/idle, 1 starting, 2 ok, 3 failing: the status dot colour.
        health: int,
        // Sensors have an on/off switch; outputs follow the selection.
        has-switch: bool,
        enabled: bool,
    }

    // A reusable row: name on the left, slider in the middle, value on the right.
    component LabeledSlider inherits HorizontalBox {
        in property <string> label;
        in property <float> minimum: 0;
        in property <float> maximum: 1;
        in property <int> decimals: 2;
        // Shown after the value, e.g. "%" or "s".
        in property <string> suffix;
        // `scale` multiplies the shown value: 100 shows 0..1 as 0..100 %.
        in property <float> scale: 1;
        in property <bool> enabled: true;
        // `in-out` = both the Rust code and this component may change it.
        in-out property <float> value;
        callback changed();

        padding: 0;
        // Tall enough to hit with a finger.
        min-height: 44px;
        Text {
            text: root.label;
            width: 96px;
            vertical-alignment: center;
            opacity: root.enabled ? 1 : 0.6;
        }
        Slider {
            minimum: root.minimum;
            maximum: root.maximum;
            enabled: root.enabled;
            // `<=>` is a two-way binding: moving the slider updates
            // `value`, and setting `value` moves the slider.
            value <=> root.value;
            changed => { root.changed(); }
        }
        Text {
            text: (root.value * root.scale).to-fixed(root.decimals) + root.suffix;
            width: 56px;
            horizontal-alignment: right;
            vertical-alignment: center;
        }
    }

    // A label and a drop-down list in one row.
    component LabeledComboBox inherits HorizontalLayout {
        in property <string> label;
        in property <[string]> model;
        in-out property <int> current-index;
        callback selected();

        spacing: 8px;
        min-height: 44px;
        Text {
            text: root.label;
            width: 96px;
            vertical-alignment: center;
        }
        ComboBox {
            model: root.model;
            current-index <=> root.current-index;
            selected => { root.selected(); }
        }
    }

    // A label and an on/off switch in one row.
    component LabeledSwitch inherits HorizontalLayout {
        in property <string> label;
        in-out property <bool> checked;
        callback toggled();

        min-height: 44px;
        Text {
            text: root.label;
            horizontal-stretch: 1;
            vertical-alignment: center;
        }
        Switch {
            checked <=> root.checked;
            toggled => { root.toggled(); }
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

    // A large button for touch: page tabs and two-way choices.
    // `active` draws it highlighted.
    component TouchButton inherits Rectangle {
        in property <string> text;
        in property <bool> active;
        callback clicked();

        min-height: 44px;
        horizontal-stretch: 1;
        border-radius: 8px;
        border-width: 1px;
        border-color: Palette.border;
        background: root.active ? Palette.accent-background
            : touch.pressed ? Palette.border
            : Palette.control-background;

        Text {
            text: root.text;
            font-size: 14px;
            color: root.active ? Palette.accent-foreground : Palette.control-foreground;
            horizontal-alignment: center;
            vertical-alignment: center;
            wrap: word-wrap;
        }
        // `touch :=` names the element so the background above can ask
        // whether it is being pressed.
        touch := TouchArea {
            clicked => { root.clicked(); }
        }
    }

    // A bold heading for a group of controls.
    component Heading inherits Text {
        font-size: 15px;
        font-weight: 700;
    }

    // One sensor or output: what it is, how it is wired, its state and
    // live values, and for sensors a switch to turn it on or off.
    component DeviceCard inherits Rectangle {
        in property <DeviceRow> device;
        callback toggled(bool);

        background: Palette.alternate-background;
        border-radius: 8px;

        HorizontalLayout {
            padding: 12px;
            spacing: 8px;

            VerticalLayout {
                spacing: 4px;
                horizontal-stretch: 1;

                Text {
                    text: root.device.name;
                    font-size: 15px;
                    font-weight: 700;
                    wrap: word-wrap;
                }
                Text {
                    text: root.device.wiring;
                    font-size: 11px;
                    wrap: word-wrap;
                    opacity: 0.7;
                }
                Text {
                    text: root.device.role;
                    font-size: 11px;
                    wrap: word-wrap;
                    opacity: 0.7;
                }
                HorizontalLayout {
                    spacing: 6px;
                    VerticalLayout {
                        alignment: center;
                        Rectangle {
                            width: 10px;
                            height: 10px;
                            border-radius: 5px;
                            background: root.device.health == 2 ? #3fb950
                                : root.device.health == 3 ? #f85149
                                : root.device.health == 1 ? #d29922
                                : #6e7681;
                        }
                    }
                    Text {
                        text: root.device.status;
                        wrap: word-wrap;
                        horizontal-stretch: 1;
                    }
                }
                if root.device.reading != "" : Text {
                    text: root.device.reading;
                    font-size: 18px;
                }
            }
            if root.device.has-switch : VerticalLayout {
                alignment: center;
                Switch {
                    checked: root.device.enabled;
                    toggled => { root.toggled(self.checked); }
                }
            }
        }
    }

    export component MainWindow inherits Window {
        title: "PID controller";
        preferred-width: 360px;
        preferred-height: 640px;

        // --- Written by Rust, read by the UI.
        in property <string> level-path;
        in property <string> setpoint-path;
        in property <string> output-path;
        in property <string> status;
        in property <[DeviceRow]> sensors;
        in property <[DeviceRow]> outputs;
        // Choices for the input and output drop-downs.
        in property <[string]> input-names;
        in property <[string]> output-names;
        // Unit and range of the selected input: the setpoint slider and
        // the plot follow them.
        in property <string> pv-unit: "cm";
        in property <float> pv-max: 40;

        // --- Written by the UI, read by Rust when `settings-changed` fires.
        // Control page
        in-out property <bool> auto: true;
        in-out property <float> setpoint: 20;
        // In manual: the output set by hand. In auto: Rust keeps it equal
        // to the live output, so switching to manual starts from there.
        in-out property <float> manual-output;
        in-out property <float> disturbance;
        in-out property <int> input-index: 0;
        in-out property <int> output-index: 0;
        // Tuning page
        in-out property <int> mode-index: 3;
        in-out property <float> kp;
        in-out property <float> ki;
        in-out property <float> kd;
        in-out property <bool> inverted;
        in-out property <bool> anti-windup: true;
        in-out property <float> d-filter;
        in-out property <float> out-min: 0;
        in-out property <float> out-max: 1;
        callback settings-changed();
        callback sensor-toggled(int, bool);

        // Which page is shown: 0 Control, 1 Tuning, 2 Sensors.
        in-out property <int> page: 0;

        VerticalBox {
            // ---------------------------------------------------------------
            // Page 0: Control.
            // `if` creates the page only while the condition holds. The
            // values live in the root properties above, so nothing is lost
            // when switching pages.
            if root.page == 0 : VerticalLayout {
                spacing: 6px;
                vertical-stretch: 1;

                // Plot area
                Rectangle {
                    background: #14181d;
                    border-radius: 6px;
                    min-height: 120px;
                    vertical-stretch: 1;
                    clip: true;

                    PlotLine { commands: root.output-path;   stroke: #5c6773; }
                    PlotLine { commands: root.setpoint-path; stroke: #e0af68; }
                    PlotLine { commands: root.level-path;    stroke: #4fc1ff; }

                    // Scale of the measurement axis.
                    Text {
                        x: 6px;
                        y: 4px;
                        text: root.pv-max.to-fixed(0) + " " + root.pv-unit;
                        font-size: 11px;
                        color: #8b949e;
                    }
                }

                HorizontalLayout {
                    spacing: 12px;
                    Text { text: "— measured"; color: #4fc1ff; }
                    Text { text: "— setpoint"; color: #e0af68; }
                    Text { text: "— output";   color: #5c6773; }
                }
                // Live numbers, the manual warning, and any output problem.
                Text {
                    text: root.status;
                    wrap: word-wrap;
                }

                HorizontalLayout {
                    spacing: 8px;
                    TouchButton {
                        text: "Auto";
                        active: root.auto;
                        clicked => {
                            root.auto = true;
                            root.settings-changed();
                        }
                    }
                    TouchButton {
                        text: "Manual";
                        active: !root.auto;
                        clicked => {
                            root.auto = false;
                            root.settings-changed();
                        }
                    }
                }
                LabeledSlider {
                    label: "Setpoint " + root.pv-unit;
                    maximum: root.pv-max;
                    decimals: 1;
                    value <=> root.setpoint;
                    changed => { root.settings-changed(); }
                }
                // Moves by itself in auto (showing the output), by hand in manual.
                LabeledSlider {
                    label: root.auto ? "Output" : "Output (hand)";
                    enabled: !root.auto;
                    decimals: 0;
                    scale: 100;
                    suffix: " %";
                    value <=> root.manual-output;
                    changed => { root.settings-changed(); }
                }
                LabeledSlider {
                    label: "Disturbance";
                    decimals: 0;
                    scale: 100;
                    suffix: " %";
                    value <=> root.disturbance;
                    changed => { root.settings-changed(); }
                }
                LabeledComboBox {
                    label: "Input";
                    model: root.input-names;
                    current-index <=> root.input-index;
                    selected => { root.settings-changed(); }
                }
                LabeledComboBox {
                    label: "Output to";
                    model: root.output-names;
                    current-index <=> root.output-index;
                    selected => { root.settings-changed(); }
                }
            }

            // ---------------------------------------------------------------
            // Page 1: Tuning.
            if root.page == 1 : VerticalLayout {
                spacing: 6px;
                vertical-stretch: 1;

                LabeledComboBox {
                    label: "Mode";
                    model: ["P", "PI", "PD", "PID"];
                    current-index <=> root.mode-index;
                    selected => { root.settings-changed(); }
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

                Heading { text: "Action"; }
                HorizontalLayout {
                    spacing: 8px;
                    TouchButton {
                        text: "Normal";
                        active: !root.inverted;
                        clicked => {
                            root.inverted = false;
                            root.settings-changed();
                        }
                    }
                    TouchButton {
                        text: "Inverted";
                        active: root.inverted;
                        clicked => {
                            root.inverted = true;
                            root.settings-changed();
                        }
                    }
                }
                Text {
                    text: root.inverted
                        ? "More output lowers the value (drain pump, cooling)"
                        : "More output raises the value (filling pump, heater)";
                    font-size: 12px;
                    wrap: word-wrap;
                    opacity: 0.7;
                }

                LabeledSwitch {
                    label: "Anti-windup";
                    checked <=> root.anti-windup;
                    toggled => { root.settings-changed(); }
                }
                LabeledSlider {
                    label: "D filter";
                    maximum: 2;
                    decimals: 1;
                    suffix: " s";
                    value <=> root.d-filter;
                    changed => { root.settings-changed(); }
                }

                Heading { text: "Output limits"; }
                LabeledSlider {
                    label: "Minimum";
                    decimals: 0;
                    scale: 100;
                    suffix: " %";
                    value <=> root.out-min;
                    changed => { root.settings-changed(); }
                }
                LabeledSlider {
                    label: "Maximum";
                    decimals: 0;
                    scale: 100;
                    suffix: " %";
                    value <=> root.out-max;
                    changed => { root.settings-changed(); }
                }

                // Pushes the controls to the top.
                Rectangle { vertical-stretch: 1; }
            }

            // ---------------------------------------------------------------
            // Page 2: Sensors and outputs. Longer than the screen, so it
            // scrolls: drag it up and down with a finger.
            if root.page == 2 : ScrollView {
                vertical-stretch: 1;

                VerticalLayout {
                    spacing: 10px;

                    Heading { text: "Controller input"; }
                    LabeledComboBox {
                        label: "Input";
                        model: root.input-names;
                        current-index <=> root.input-index;
                        selected => { root.settings-changed(); }
                    }

                    Heading { text: "Sensors"; }
                    // A repeater: one card per entry in `sensors`. `i` is
                    // the index, `device` the entry.
                    for device[i] in root.sensors : DeviceCard {
                        device: device;
                        toggled(on) => { root.sensor-toggled(i, on); }
                    }

                    Heading { text: "Outputs"; }
                    for device in root.outputs : DeviceCard {
                        device: device;
                    }
                }
            }

            // --- Page tabs, always at the bottom.
            HorizontalLayout {
                spacing: 8px;
                TouchButton {
                    text: "Control";
                    active: root.page == 0;
                    clicked => { root.page = 0; }
                }
                TouchButton {
                    text: "Tuning";
                    active: root.page == 1;
                    clicked => { root.page = 1; }
                }
                TouchButton {
                    text: "Sensors";
                    active: root.page == 2;
                    clicked => { root.page = 2; }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Data shared between the threads
// ---------------------------------------------------------------------------

/// What the user can change from the screen.
#[derive(Clone, Copy, PartialEq)]
struct Settings {
    mode: Mode,
    gains: Gains,
    options: Options,
    out_min: f64,
    out_max: f64,
    setpoint: f64,
    /// Automatic (the controller sets the output) or manual (by hand).
    auto: bool,
    manual_output: f64,
    /// Second pump draining the tank (real rig) or extra outflow (simulator).
    disturbance: f64,
    /// Where the measurement comes from: the simulator or a real sensor.
    input: Input,
    /// Where the output goes, besides the simulated tank.
    output: Output,
}

/// One measurement, kept for the plot.
#[derive(Clone, Copy)]
struct Sample {
    setpoint: f64,
    /// `f64::NAN` when the input had no fresh reading. The plot leaves a
    /// gap there instead of drawing a made-up value.
    measured: f64,
    output: f64,
}

/// Everything the control thread and the UI thread both need to see.
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

/// Output used in automatic while the selected input has no fresh
/// reading. Off is the safe choice for a pump: the tank cannot overflow.
const SAFE_OUTPUT: f64 = 0.0;

// Size of the coordinate system the plot lines are drawn in. Must match
// `viewbox-width` / `viewbox-height` in the markup above.
const PLOT_WIDTH: f64 = 1000.0;
const PLOT_HEIGHT: f64 = 100.0;

/// `main` can return a Result. If creating the window fails, the `?`
/// operator returns the error from `main` and Rust prints it.
fn main() -> Result<(), slint::PlatformError> {
    // `ui --safe-outputs`: switch every real output off and stop. The
    // systemd service runs this after the program ends for any reason.
    if std::env::args().any(|argument| argument == "--safe-outputs") {
        outputs::all_safe();
        return Ok(());
    }

    // If any thread panics, switch the outputs off and end the whole
    // program, so systemd restarts it cleanly. Without this, a panic in
    // the control thread would leave the UI running with a pump stuck on.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        outputs::all_safe();
        std::process::abort();
    }));

    // The choices for the two drop-downs, in order.
    let inputs = sensors::inputs();
    let output_choices = outputs::choices();

    let initial = Settings {
        mode: Mode::Pid,
        gains: Gains {
            kp: 0.15,
            ki: 0.03,
            kd: 0.05,
        },
        options: Options::default(),
        out_min: 0.0,
        out_max: 1.0,
        setpoint: 20.0,
        auto: true,
        manual_output: 0.0,
        disturbance: 0.0,
        input: inputs[0],           // the simulator
        output: output_choices[0], // simulation only
    };

    let shared: SharedState = Arc::new(Mutex::new(Shared {
        settings: initial,
        history: VecDeque::with_capacity(HISTORY_LEN),
    }));

    // --- Start the sensor threads. They run whether or not a sensor is
    // the controller input, so the Sensors page always shows live values.
    let board = sensors::new_board();
    sensors::start(&board);
    let output_board = outputs::new_board();

    // --- Create the window and show the initial values.
    // Slint uses f32 for numbers, our maths uses f64, so we convert with
    // `as`. Rust never converts number types silently.
    let window = MainWindow::new()?;
    window.set_kp(initial.gains.kp as f32);
    window.set_ki(initial.gains.ki as f32);
    window.set_kd(initial.gains.kd as f32);
    window.set_setpoint(initial.setpoint as f32);

    // Lists for the drop-downs and the Sensors page. A Slint model is a
    // list the UI watches: `VecModel` is the ready-made one backed by a
    // Vec. We keep an `Rc` to the card models so the timer can update rows
    // in place.
    let names: Vec<SharedString> = inputs.iter().map(|i| i.label().into()).collect();
    window.set_input_names(ModelRc::new(VecModel::from(names)));
    let names: Vec<SharedString> = output_choices.iter().map(|o| o.label().into()).collect();
    window.set_output_names(ModelRc::new(VecModel::from(names)));
    let sensor_model = Rc::new(VecModel::from(sensor_rows(&board)));
    window.set_sensors(ModelRc::from(sensor_model.clone()));
    let output_model = Rc::new(VecModel::from(output_rows(&output_board)));
    window.set_outputs(ModelRc::from(output_model.clone()));

    // --- UI -> control thread: copy every control into the shared
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

        // `.get()` returns None for an out-of-range index instead of
        // crashing; fall back to the first choice then.
        let input = inputs
            .get(window.get_input_index() as usize)
            .copied()
            .unwrap_or(Input::SimulatedTank);
        let output = output_choices
            .get(window.get_output_index() as usize)
            .copied()
            .unwrap_or(Output::SimulationOnly);

        // A different input can have another unit and range: rescale the
        // setpoint slider and keep the setpoint inside it.
        let previous = shared_for_callback.lock().unwrap().settings;
        if input != previous.input {
            window.set_pv_unit(input.unit().into());
            window.set_pv_max(input.max() as f32);
            window.set_setpoint(window.get_setpoint().min(input.max() as f32));
        }

        let settings = Settings {
            mode: match window.get_mode_index() {
                0 => Mode::P,
                1 => Mode::Pi,
                2 => Mode::Pd,
                // `_` is the catch-all arm; `match` must cover every
                // possible integer.
                _ => Mode::Pid,
            },
            gains: Gains {
                kp: window.get_kp() as f64,
                ki: window.get_ki() as f64,
                kd: window.get_kd() as f64,
            },
            options: Options {
                action: if window.get_inverted() {
                    Action::Inverted
                } else {
                    Action::Normal
                },
                anti_windup: window.get_anti_windup(),
                d_filter_s: window.get_d_filter() as f64,
            },
            out_min: window.get_out_min() as f64,
            out_max: window.get_out_max() as f64,
            setpoint: window.get_setpoint() as f64,
            auto: window.get_auto(),
            manual_output: window.get_manual_output() as f64,
            disturbance: window.get_disturbance() as f64,
            input,
            output,
        };
        // `.lock()` waits for the mutex and returns a guard. The data is
        // reachable only through the guard, and the mutex unlocks by
        // itself when the guard goes out of scope at the end of this
        // statement. `unwrap` crashes if the other thread panicked while
        // holding the lock, which is acceptable for a demo.
        shared_for_callback.lock().unwrap().settings = settings;
    });

    // --- Sensor switches on the Sensors page.
    let board_for_toggle = Arc::clone(&board);
    window.on_sensor_toggled(move |index, on| {
        if let Some(status) = board_for_toggle.lock().unwrap().get_mut(index as usize) {
            status.enabled = on;
        }
    });

    // --- Start the control thread with its own handles to the shared data.
    // We do not keep the thread handle: the thread loops forever and is
    // ended by the operating system when `main` returns.
    let shared_for_control = Arc::clone(&shared);
    let board_for_control = Arc::clone(&board);
    let outputs_for_control = Arc::clone(&output_board);
    thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            run_control_loop(shared_for_control, board_for_control, outputs_for_control, initial)
        })
        .expect("could not start control thread");

    // --- Control thread -> UI: a timer on the UI thread redraws the plot
    // and the cards about 30 times per second.
    //
    // Design choice: the UI pulls data on its own schedule instead of the
    // control thread pushing every sample. The control loop then never
    // waits for drawing, and UI objects are only touched from the UI
    // thread, which Slint requires.
    let window_weak = window.as_weak();
    let shared_for_timer = Arc::clone(&shared);
    let board_for_timer = Arc::clone(&board);
    let outputs_for_timer = Arc::clone(&output_board);
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(33),
        move || {
            // Copy what we need out and release the lock straight away, so
            // the control thread is blocked for as short a time as
            // possible. The inner braces end the guard's scope.
            let (history, settings): (Vec<Sample>, Settings) = {
                let guard = shared_for_timer.lock().unwrap();
                (guard.history.iter().copied().collect(), guard.settings)
            };
            let output_states = outputs_for_timer.lock().unwrap().clone();

            let window = window_weak.unwrap();
            let pv_max = settings.input.max();

            // `.iter().map(...)` transforms each sample lazily; the
            // closure `|s| s.measured` picks one field. `.into()` converts
            // a Rust String into Slint's string type.
            let measured = history.iter().map(|s| s.measured);
            window.set_level_path(build_path(measured, pv_max).into());
            let setpoints = history.iter().map(|s| s.setpoint);
            window.set_setpoint_path(build_path(setpoints, pv_max).into());
            let outputs = history.iter().map(|s| s.output);
            window.set_output_path(build_path(outputs, 1.0).into());

            // `.last()` returns an Option: the history is empty at first.
            if let Some(latest) = history.last() {
                // In auto, let the Output slider follow the live output.
                // Switching to manual then starts exactly from there.
                if settings.auto {
                    window.set_manual_output(latest.output as f32);
                }
                let error = match settings.output {
                    Output::Device(id) => output_states[id as usize].error.clone(),
                    Output::SimulationOnly => None,
                };
                window.set_status(status_text(&settings, latest, error).into());
            }

            // Update only the cards that changed. Replacing the whole model
            // would rebuild every card, and a finger on a switch would lose
            // its press.
            update_rows(&sensor_model, sensor_rows(&board_for_timer));
            update_rows(&output_model, output_rows(&outputs_for_timer));
        },
    );

    // Show the window and process events until it is closed.
    window.run()
}

/// The fixed-rate loop: read settings, measure, compute, actuate, record.
fn run_control_loop(
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
        let s = shared.lock().unwrap().settings;
        controller.set_mode(s.mode);
        controller.set_gains(s.gains);
        controller.set_options(s.options);
        controller.set_limits(s.out_min, s.out_max);

        // Bumpless input switching: start clean on the new signal.
        if s.input != last_input {
            controller.reset();
            last_input = s.input;
        }

        // `Option<f64>`: None when the selected sensor has no fresh value.
        let measurement = match s.input {
            Input::SimulatedTank => Some(plant.measure()),
            Input::Sensor { sensor, signal } => {
                let info: &SensorInfo = &SENSORS[sensor as usize];
                board.lock().unwrap()[sensor as usize].fresh_value(signal, info.stale_after)
            }
        };

        // A `match` on a pair handles all four combinations explicitly.
        let output = match (s.auto, measurement) {
            // Manual: the hand-set value goes out unchanged, and the
            // controller follows along for a bumpless return to auto.
            (false, Some(value)) => {
                controller.track(s.setpoint, value, s.manual_output);
                s.manual_output
            }
            (false, None) => s.manual_output,
            (true, Some(value)) => {
                // Coming back from a failure: do not differentiate across
                // the gap or reuse an integral from before it.
                if !had_reading {
                    controller.reset();
                }
                controller.update(s.setpoint, value, DT)
            }
            (true, None) => SAFE_OUTPUT,
        };
        had_reading = measurement.is_some();

        plant.set_disturbance(s.disturbance);
        plant.step(output, DT);

        // Real outputs. Pump B (the disturbance) only runs together with
        // pump A, i.e. when the real pump rig is being controlled.
        let pump_rig = s.output == Output::Device(OutputId::PumpA);
        {
            let mut states = output_board.lock().unwrap();
            for slot in &mut slots {
                let wanted = if s.output == Output::Device(slot.id) {
                    Some(output)
                } else if slot.id == OutputId::PumpB && pump_rig {
                    Some(s.disturbance)
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
                setpoint: s.setpoint,
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
fn status_text(settings: &Settings, latest: &Sample, output_error: Option<String>) -> String {
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

/// Copy new rows into a model, touching only the rows that differ.
fn update_rows(model: &VecModel<DeviceRow>, rows: Vec<DeviceRow>) {
    for (index, row) in rows.into_iter().enumerate() {
        if model.row_data(index).as_ref() != Some(&row) {
            model.set_row_data(index, row);
        }
    }
}

/// Build the sensor cards from the board's current state.
fn sensor_rows(board: &SensorBoard) -> Vec<DeviceRow> {
    // Clone the statuses so the lock is held only for the copy.
    let statuses = board.lock().unwrap().clone();

    // `zip` walks two lists side by side: each sensor's fixed info with
    // its live status.
    SENSORS
        .iter()
        .zip(statuses)
        .map(|(info, status)| {
            let health = status.health(info.stale_after);
            let reading = if health == Health::Ok {
                format_reading(info, &status.values)
            } else {
                String::new()
            };
            let (code, text) = match health {
                Health::Off => (0, "Off".to_string()),
                Health::Starting => (1, "Starting".to_string()),
                Health::Ok => (2, "OK".to_string()),
                Health::Failing(error) => (3, error),
            };
            let role = if info.signals.iter().any(|s| s.controllable) {
                "Can be the controller input"
            } else {
                "Monitoring only"
            };
            DeviceRow {
                name: info.name.into(),
                wiring: info.wiring.into(),
                role: role.into(),
                reading: reading.into(),
                status: text.into(),
                health: code,
                has_switch: true,
                enabled: status.enabled,
            }
        })
        .collect()
}

/// Build the output cards from the output board.
fn output_rows(board: &OutputBoard) -> Vec<DeviceRow> {
    let states = board.lock().unwrap().clone();
    OUTPUTS
        .iter()
        .zip(states)
        .map(|(info, state)| {
            let (code, status, reading) = match (&state.error, state.active) {
                (Some(error), _) => (3, error.clone(), String::new()),
                (None, true) => (2, "Active".to_string(), format!("{:.0} %", state.value * 100.0)),
                (None, false) => (0, "Idle (not selected)".to_string(), String::new()),
            };
            DeviceRow {
                name: info.name.into(),
                wiring: info.wiring.into(),
                role: info.role.into(),
                reading: reading.into(),
                status: status.into(),
                health: code,
                has_switch: false,
                enabled: state.active,
            }
        })
        .collect()
}

/// "Distance 23.4 cm\nLevel 16.6 cm": one line per signal.
fn format_reading(info: &SensorInfo, values: &[f64]) -> String {
    info.signals
        .iter()
        .zip(values)
        .map(|(signal, value)| format!("{} {value:.1} {}", signal.name, signal.unit))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Turn a series of values into an SVG path such as "M 0 100 L 1.7 96 ...".
///
/// `impl Iterator<Item = f64>` means "anything that yields f64 values",
/// so the caller can pass any of the plotted series without first copying
/// them into a separate list.
///
/// `max_value` is the value drawn at the top edge of the plot. A NaN value
/// (no reading) leaves a gap: the next real value starts a new line.
fn build_path(values: impl Iterator<Item = f64>, max_value: f64) -> String {
    let mut path = String::new();
    // Is the pen on the paper, i.e. does the next point continue a line?
    let mut drawing = false;

    // `enumerate` pairs each value with its index: (0, v0), (1, v1), ...
    for (index, value) in values.enumerate() {
        if value.is_nan() {
            drawing = false;
            continue;
        }

        // Oldest sample at the left edge; a full history spans the width.
        let x = index as f64 / (HISTORY_LEN - 1) as f64 * PLOT_WIDTH;

        // Screen y grows downwards, so flip it. Keep a small margin so a
        // line at exactly 0 or max is not cut in half by the plot edge.
        let fraction = (value / max_value).clamp(0.0, 1.0);
        let y = PLOT_HEIGHT - (2.0 + fraction * (PLOT_HEIGHT - 4.0));

        // "M" moves the pen to the start of a line, "L" draws a line to
        // each following point.
        let command = if drawing { 'L' } else { 'M' };
        path.push_str(&format!("{command} {x:.1} {y:.1} "));
        drawing = true;
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

    #[test]
    fn missing_readings_leave_a_gap() {
        let path = build_path(vec![0.0, f64::NAN, 1.0].into_iter(), 1.0);
        // Two separate lines, each starting with "M", and no "NaN" text.
        assert_eq!(path.matches('M').count(), 2);
        assert!(!path.contains("NaN"));
    }

    #[test]
    fn reading_has_one_line_per_signal() {
        let dht = &SENSORS[sensors::SensorId::Dht22 as usize];
        let text = format_reading(dht, &[21.44, 40.0]);
        assert_eq!(text, "Temperature 21.4 °C\nHumidity 40.0 %");
    }

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
