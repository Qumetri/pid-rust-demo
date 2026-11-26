//! The touchscreen program: the controller with a live plot, its tuning,
//! the sensors and outputs it can use, and the Pi's own statistics.
//!
//! Run it with:  cargo run --bin ui
//! On the Pi:    see deploy/pid-ui.service (fullscreen on the touchscreen)
//! Safe stop:    `ui --safe-outputs` switches every real output off and
//!               exits; the service runs it after the program stops.
//!
//! Four pages, chosen with tabs at the bottom:
//!   Control  plot, auto/manual, setpoint, output, disturbance, input
//!            and output selection
//!   Tuning   mode, gains, action, anti-windup, derivative filter, limits
//!   Sensors  every sensor and output with its wiring and live state
//!   Stats    CPU, memory, storage, network and power of the Pi, sampled
//!            only while the page is open
//!
//! The program is split into small files in this folder:
//!   main.rs        start-up: creates the window and starts the threads
//!   window.slint   the screen layout
//!   control.rs     settings, samples and the control loop
//!   plot.rs        turns values into plot lines
//!   cards.rs       the sensor and output cards
//!   stats_page.rs  the Stats page and its thread
//!
//! How the threads talk to each other:
//!
//!   sensor threads --- latest values ---> SensorBoard
//!                                              |
//!   control thread  <--- settings ---  UI thread (pages, sliders)
//!     |      |                              ^
//!     |      +------ history of samples ----+   (plot, redrawn by a timer)
//!     +-- drives the real outputs, state --> OutputBoard
//!
//!   stats thread (asleep unless the Stats page is open) --> LatestStats
//!
//! Every arrow goes through a shared, lock-protected value.

// `mod` declares the other files in this folder as parts of this program.
mod cards;
mod control;
mod plot;
mod stats_page;

use cards::{output_rows, sensor_rows, update_rows};
use control::{run_control_loop, status_text, Sample, Settings, Shared, SharedState, HISTORY_LEN};
use pid_rust_demo::outputs::{self, Output, OutputBoard};
use pid_rust_demo::pid::{Action, Gains, Mode, Options};
use pid_rust_demo::sensors::{self, Input, SensorBoard};
use plot::build_path;
use slint::{ModelRc, SharedString, VecModel};
use stats_page::{run_stats, stat_rows, LatestStats, CPU_HISTORY_LEN, STATS_PAGE};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// The screen layout lives in window.slint, next to this file. The
// `slint!` macro compiles it while Rust compiles this program, and
// generates the Rust types `MainWindow`, `DeviceRow` and `StatRow`.
slint::slint! {
    export { MainWindow, DeviceRow, StatRow } from "window.slint";
}

/// `main` can return a Result. If creating the window fails, the `?`
/// operator returns the error from `main` and Rust prints it.
fn main() -> Result<(), slint::PlatformError> {
    // `ui --safe-outputs`: switch every real output off and stop. The
    // systemd service runs this after the program ends for any reason.
    if std::env::args().any(|argument| argument == "--safe-outputs") {
        outputs::all_safe();
        return Ok(());
    }
    install_panic_handler();

    // The choices for the Input and Output drop-downs, in order.
    let inputs = sensors::inputs();
    let output_choices = outputs::choices();

    // Start with the simulated tank and nothing real switched on.
    let initial = starting_settings(inputs[0], output_choices[0]);
    let shared: SharedState = Arc::new(Mutex::new(Shared {
        settings: initial,
        history: VecDeque::with_capacity(HISTORY_LEN),
    }));

    // Start the sensor threads. They run whether or not a sensor is the
    // controller input, so the Sensors page always shows live values.
    let board = sensors::new_board();
    sensors::start(&board);
    let output_board = outputs::new_board();

    let window = MainWindow::new()?;
    show_starting_values(&window, &initial, &inputs, &output_choices);
    connect_settings(&window, &shared, inputs, output_choices);
    connect_sensor_switches(&window, &board);
    let latest_stats = start_stats(&window);
    start_control_thread(&shared, &board, &output_board, initial);

    // The timer must live as long as the window: dropping it stops it.
    // `let _timer = ...` keeps it until the end of `main`. (A bare
    // `let _ = ...` would drop it immediately.)
    let _timer = start_screen_updates(&window, &shared, &board, &output_board, &latest_stats);

    // Show the window and process touches until it is closed.
    window.run()
}

/// If any thread panics, switch the outputs off and end the whole
/// program, so systemd restarts it cleanly. Without this, a panic in the
/// control thread would leave the UI running with a pump stuck on.
fn install_panic_handler() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info); // print the usual panic message first
        outputs::all_safe();
        std::process::abort();
    }));
}

/// The values the controller starts with. Change the starting gains or
/// setpoint here.
fn starting_settings(input: Input, output: Output) -> Settings {
    Settings {
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
        input,
        output,
    }
}

/// Put the starting values and the drop-down choices on the screen.
///
/// Slint uses `f32` for numbers and our maths uses `f64`, so we convert
/// with `as`. Rust never converts number types silently.
fn show_starting_values(
    window: &MainWindow,
    initial: &Settings,
    inputs: &[Input],
    output_choices: &[Output],
) {
    window.set_kp(initial.gains.kp as f32);
    window.set_ki(initial.gains.ki as f32);
    window.set_kd(initial.gains.kd as f32);
    window.set_setpoint(initial.setpoint as f32);

    // A Slint "model" is a list the screen watches. `VecModel` is the
    // ready-made one backed by a Vec; `ModelRc` is the handle the window
    // takes.
    let names: Vec<SharedString> = inputs.iter().map(|i| i.label().into()).collect();
    window.set_input_names(ModelRc::new(VecModel::from(names)));
    let names: Vec<SharedString> = output_choices.iter().map(|o| o.label().into()).collect();
    window.set_output_names(ModelRc::new(VecModel::from(names)));
}

/// UI -> control thread: whenever any control changes, read them all and
/// store them in the shared settings.
///
/// `inputs` and `output_choices` are taken by value (not borrowed),
/// because the closure keeps them for as long as the window lives.
fn connect_settings(
    window: &MainWindow,
    shared: &SharedState,
    inputs: Vec<Input>,
    output_choices: Vec<Output>,
) {
    // The closure must not own the window (the window owns the closure,
    // so that would be a reference cycle that is never freed). A weak
    // handle breaks the cycle, the same idea as `std::weak_ptr` in C++.
    let window_weak = window.as_weak();
    // `Arc::clone` copies the pointer, not the data: both handles refer
    // to the same Shared struct.
    let shared = Arc::clone(shared);

    // `move` makes the closure take ownership of everything it uses.
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
        let previous_input = shared.lock().unwrap().settings.input;
        if input != previous_input {
            window.set_pv_unit(input.unit().into());
            window.set_pv_max(input.max() as f32);
            window.set_setpoint(window.get_setpoint().min(input.max() as f32));
        }

        let settings = read_settings(&window, input, output);

        // `.lock()` waits for the mutex and returns a guard. The data is
        // reachable only through the guard, and the mutex unlocks by
        // itself when the guard goes out of scope at the end of this
        // statement. `unwrap` crashes if another thread panicked while
        // holding the lock, which the panic handler then deals with.
        shared.lock().unwrap().settings = settings;
    });
}

/// Read every control on the Control and Tuning pages.
fn read_settings(window: &MainWindow, input: Input, output: Output) -> Settings {
    Settings {
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
    }
}

/// The on/off switches on the sensor cards.
fn connect_sensor_switches(window: &MainWindow, board: &SensorBoard) {
    let board = Arc::clone(board);
    window.on_sensor_toggled(move |index, on| {
        // `if let Some(...)` runs the block only if the index exists.
        if let Some(status) = board.lock().unwrap().get_mut(index as usize) {
            status.enabled = on;
        }
    });
}

/// Start the stats thread, and wake it whenever the Stats tab is opened.
/// Returns the slot where the thread leaves each new sample.
fn start_stats(window: &MainWindow) -> LatestStats {
    // `AtomicBool` is a bool that several threads may read and write
    // without a mutex.
    let stats_open = Arc::new(AtomicBool::new(false));
    let latest_stats: LatestStats = Arc::new(Mutex::new(None));

    let open = Arc::clone(&stats_open);
    let latest = Arc::clone(&latest_stats);
    let handle = thread::Builder::new()
        .name("stats".into())
        .spawn(move || run_stats(open, latest))
        .expect("could not start stats thread");

    // The thread handle lets us wake the thread when the page opens.
    let stats_thread = handle.thread().clone();
    window.on_page_changed(move |page| {
        let open = page == STATS_PAGE;
        stats_open.store(open, Ordering::Relaxed);
        if open {
            stats_thread.unpark();
        }
    });

    latest_stats
}

/// Start the control thread with its own handles to the shared data.
///
/// We do not keep the thread handle: the thread loops forever and is
/// ended by the operating system when the program exits.
fn start_control_thread(
    shared: &SharedState,
    board: &SensorBoard,
    output_board: &OutputBoard,
    initial: Settings,
) {
    let shared = Arc::clone(shared);
    let board = Arc::clone(board);
    let output_board = Arc::clone(output_board);
    thread::Builder::new()
        .name("control".into())
        .spawn(move || run_control_loop(shared, board, output_board, initial))
        .expect("could not start control thread");
}

/// Control thread -> UI: a timer on the UI thread that redraws the screen
/// about 30 times per second. Returns the timer, which must be kept alive.
///
/// Design choice: the UI pulls data on its own schedule instead of the
/// control thread pushing every sample. The control loop then never waits
/// for drawing, and UI objects are only touched from the UI thread, which
/// Slint requires.
fn start_screen_updates(
    window: &MainWindow,
    shared: &SharedState,
    board: &SensorBoard,
    output_board: &OutputBoard,
    latest_stats: &LatestStats,
) -> slint::Timer {
    // Lists for the Sensors and Stats pages. We keep an `Rc` (a
    // single-thread shared pointer) to each, so the timer can update rows
    // in place.
    let sensor_model = Rc::new(VecModel::from(sensor_rows(board)));
    window.set_sensors(ModelRc::from(sensor_model.clone()));
    let output_model = Rc::new(VecModel::from(output_rows(output_board)));
    window.set_outputs(ModelRc::from(output_model.clone()));
    let stats_model = Rc::new(VecModel::<StatRow>::default());
    window.set_stats(ModelRc::from(stats_model.clone()));

    // The timer closure gets its own handles to everything it reads.
    let window_weak = window.as_weak();
    let shared = Arc::clone(shared);
    let board = Arc::clone(board);
    let output_board = Arc::clone(output_board);
    let latest_stats = Arc::clone(latest_stats);
    // The CPU graph's history, owned by the timer closure. It is the only
    // code that touches it, so it needs no lock.
    let mut cpu_history: VecDeque<f64> = VecDeque::with_capacity(CPU_HISTORY_LEN);

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(33),
        move || {
            let window = window_weak.unwrap();
            update_control_page(&window, &shared, &output_board);
            // Update only the cards that changed. Replacing the whole list
            // would rebuild every card, and a finger on a switch would
            // lose its press.
            update_rows(&sensor_model, sensor_rows(&board));
            update_rows(&output_model, output_rows(&output_board));
            update_stats_page(&window, &latest_stats, &stats_model, &mut cpu_history);
        },
    );
    timer
}

/// Redraw the plot and the status line from the control thread's history.
fn update_control_page(window: &MainWindow, shared: &SharedState, output_board: &OutputBoard) {
    // Copy what we need out and release the lock straight away, so the
    // control thread is blocked for as short a time as possible. The
    // inner braces end the guard's scope.
    let (history, settings): (Vec<Sample>, Settings) = {
        let guard = shared.lock().unwrap();
        (guard.history.iter().copied().collect(), guard.settings)
    };

    // `.iter().map(...)` transforms each sample lazily; the closure
    // `|s| s.measured` picks one field. `.into()` converts a Rust String
    // into Slint's string type.
    let pv_max = settings.input.max();
    let measured = history.iter().map(|s| s.measured);
    window.set_level_path(build_path(measured, pv_max, HISTORY_LEN).into());
    let setpoints = history.iter().map(|s| s.setpoint);
    window.set_setpoint_path(build_path(setpoints, pv_max, HISTORY_LEN).into());
    let outputs = history.iter().map(|s| s.output);
    window.set_output_path(build_path(outputs, 1.0, HISTORY_LEN).into());

    // `.last()` returns an Option: the history is empty at first.
    if let Some(latest) = history.last() {
        // In auto, let the Output slider follow the live output, so
        // switching to manual starts exactly from there.
        if settings.auto {
            window.set_manual_output(latest.output as f32);
        }
        let output_error = match settings.output {
            Output::Device(id) => output_board.lock().unwrap()[id as usize].error.clone(),
            Output::SimulationOnly => None,
        };
        window.set_status(status_text(&settings, latest, output_error).into());
    }
}

/// Show the stats thread's newest sample, if the Stats page is open.
fn update_stats_page(
    window: &MainWindow,
    latest_stats: &LatestStats,
    stats_model: &VecModel<StatRow>,
    cpu_history: &mut VecDeque<f64>,
) {
    if window.get_page() != STATS_PAGE {
        // Closed: start a fresh graph next time it opens.
        cpu_history.clear();
        return;
    }
    // `take()` empties the slot, so each sample is shown once.
    let Some(snapshot) = latest_stats.lock().unwrap().take() else {
        return; // no new sample since the last redraw
    };
    if cpu_history.len() == CPU_HISTORY_LEN {
        cpu_history.pop_front();
    }
    cpu_history.push_back(snapshot.cpu_percent.unwrap_or(f64::NAN));
    let cpu = cpu_history.iter().copied();
    window.set_cpu_path(build_path(cpu, 100.0, CPU_HISTORY_LEN).into());
    update_rows(stats_model, stat_rows(&snapshot));
}
