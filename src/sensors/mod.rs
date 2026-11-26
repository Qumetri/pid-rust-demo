//! Real sensors: the list of supported drivers and the threads that read them.
//!
//! Design (from docs/PROJECT-NOTES.md, "Sensors as interchangeable signals"):
//!
//! - Every sensor runs in its own thread, at its own rate. A slow sensor
//!   (the DHT-22 needs 2 s between readings) never delays a fast one.
//! - Each thread publishes its latest values into one shared `SensorBoard`.
//!   The control loop and the screen only ever read from the board; they
//!   never talk to hardware themselves.
//! - A value is only trusted while it is fresh. If a sensor stops answering,
//!   its values go stale and the control loop falls back to a safe output.
//!
//! Adding a new sensor type (for example the MPU6050 gyroscope or a light
//! sensor later) means three steps:
//!   1. write a driver file next to this one that implements `Driver`;
//!   2. add a variant to `SensorId` and an entry to `SENSORS`;
//!   3. add one arm to `open_driver`.
//! The screen and the control loop pick the new sensor up from the list.

pub mod dht22;
pub mod ultrasonic;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Every supported sensor type.
///
/// The explicit numbers (`= 0`, `= 1`) let us use an id as an index into
/// `SENSORS` and into the board with `id as usize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorId {
    Ultrasonic = 0,
    Dht22 = 1,
}

/// One quantity a sensor reports, such as "Temperature in °C".
pub struct Signal {
    pub name: &'static str,
    /// Name on its own, for the input choice: "Ultrasonic level".
    pub label: &'static str,
    pub unit: &'static str,
    /// Top of the plot and of the setpoint slider when this signal is the
    /// controller input.
    pub max: f64,
    /// May this signal be the controller's input (process variable)?
    pub controllable: bool,
}

/// Fixed facts about a sensor type: what it is, how it is wired, how
/// often it is read.
///
/// `&'static str` is a string literal baked into the program, like a
/// `const char*` to a literal in C++. `'static` means it lives forever,
/// so the struct can hold it without owning a copy.
pub struct SensorInfo {
    pub id: SensorId,
    pub name: &'static str,
    /// Shown on the sensor screen, so the wiring is never a guess.
    pub wiring: &'static str,
    pub signals: &'static [Signal],
    /// Time between readings.
    pub period: Duration,
    /// After this long without a good reading the values are not trusted.
    pub stale_after: Duration,
}

/// The supported sensors. Order must match the `SensorId` numbers.
pub const SENSORS: [SensorInfo; 2] = [
    SensorInfo {
        id: SensorId::Ultrasonic,
        name: "Ultrasonic RCWL-1601",
        wiring: "Trig GPIO23 (pin 16), Echo GPIO24 (pin 18), VCC 3.3 V",
        signals: &[
            Signal {
                name: "Distance",
                label: "Ultrasonic distance",
                unit: "cm",
                max: 100.0,
                controllable: false,
            },
            // Level = mounting height minus distance: the sensor looks
            // down at the water, so the closer the surface, the fuller the
            // tank. See ultrasonic::MOUNT_HEIGHT_CM.
            Signal {
                name: "Level",
                label: "Ultrasonic level",
                unit: "cm",
                max: 40.0,
                controllable: true,
            },
        ],
        period: Duration::from_millis(50), // 20 Hz
        stale_after: Duration::from_millis(500),
    },
    SensorInfo {
        id: SensorId::Dht22,
        name: "DHT-22 temperature / humidity",
        wiring: "Data GPIO4 (pin 7), VCC 3.3 V, 10 kΩ pull-up",
        // Monitoring only: the DHT-22 is slow and some reads fail, so it
        // must not be a control loop input (see PROJECT-NOTES.md §6).
        signals: &[
            Signal {
                name: "Temperature",
                label: "Temperature",
                unit: "°C",
                max: 50.0,
                controllable: false,
            },
            Signal {
                name: "Humidity",
                label: "Humidity",
                unit: "%",
                max: 100.0,
                controllable: false,
            },
        ],
        period: Duration::from_secs(2),
        stale_after: Duration::from_secs(10),
    },
];

/// The interface every sensor driver implements.
///
/// `read` returns one value per entry in the sensor's `signals`, in the
/// same order, or an error message for the screen.
pub trait Driver: Send {
    fn read(&mut self) -> Result<Vec<f64>, String>;
}

/// Create the driver for a sensor. Opening can fail (pin in use, kernel
/// driver not loaded), so this returns a `Result` too.
///
/// `Box<dyn Driver>` lets each arm return a different concrete type.
fn open_driver(id: SensorId) -> Result<Box<dyn Driver>, String> {
    match id {
        SensorId::Ultrasonic => Ok(Box::new(ultrasonic::Ultrasonic::open()?)),
        SensorId::Dht22 => Ok(Box::new(dht22::Dht22::open()?)),
    }
}

// ---------------------------------------------------------------------------
// The shared board
// ---------------------------------------------------------------------------

/// The live state of one sensor, as seen by the screen and the control loop.
#[derive(Clone, Debug, Default)]
pub struct SensorStatus {
    /// Switched on the sensor screen. A disabled sensor's thread releases
    /// its pins and does nothing.
    pub enabled: bool,
    /// Latest good values, one per signal. Empty until the first reading.
    pub values: Vec<f64>,
    /// When `values` was last updated.
    pub last_ok: Option<Instant>,
    /// The most recent error, cleared by the next good reading.
    pub error: Option<String>,
}

/// What the screen shows as the sensor's state.
#[derive(Clone, Debug, PartialEq)]
pub enum Health {
    Off,
    Starting,
    Ok,
    /// No fresh reading; the text says why.
    Failing(String),
}

impl SensorStatus {
    /// The value of signal number `signal`, if it is fresh enough to use.
    pub fn fresh_value(&self, signal: usize, stale_after: Duration) -> Option<f64> {
        // `?` on an Option returns None early, like `?` on a Result.
        let last_ok = self.last_ok?;
        if !self.enabled || last_ok.elapsed() > stale_after {
            return None;
        }
        self.values.get(signal).copied()
    }

    pub fn health(&self, stale_after: Duration) -> Health {
        let fresh = self.last_ok.is_some_and(|t| t.elapsed() <= stale_after);
        match (self.enabled, fresh, &self.error) {
            (false, _, _) => Health::Off,
            (true, true, _) => Health::Ok,
            (true, false, Some(error)) => Health::Failing(error.clone()),
            (true, false, None) if self.last_ok.is_some() => {
                Health::Failing("no recent reading".to_string())
            }
            (true, false, None) => Health::Starting,
        }
    }
}

/// One `SensorStatus` per entry in `SENSORS`, behind one lock.
pub type SensorBoard = Arc<Mutex<Vec<SensorStatus>>>;

/// Create the board with every sensor enabled.
pub fn new_board() -> SensorBoard {
    let statuses = SENSORS
        .iter()
        .map(|_| SensorStatus {
            enabled: true,
            ..Default::default() // remaining fields take their default
        })
        .collect();
    Arc::new(Mutex::new(statuses))
}

/// Start one reading thread per supported sensor.
pub fn start(board: &SensorBoard) {
    for info in &SENSORS {
        let board = Arc::clone(board);
        // `thread::Builder` lets us name the thread; the name shows up in
        // `top -H` and in panic messages.
        thread::Builder::new()
            .name(format!("sensor {:?}", info.id))
            .spawn(move || run_sensor(info, board))
            .expect("could not start sensor thread");
    }
}

/// The body of a sensor thread: open the driver, then read it on a fixed
/// schedule for as long as the sensor is enabled.
fn run_sensor(info: &'static SensorInfo, board: SensorBoard) {
    let index = info.id as usize;
    // No driver until the sensor is enabled and opened successfully.
    let mut driver: Option<Box<dyn Driver>> = None;
    let mut next = Instant::now();

    loop {
        let enabled = board.lock().unwrap()[index].enabled;

        if !enabled {
            // Dropping the driver releases its GPIO lines, so another
            // program (or another driver) may use the pins.
            driver = None;
            thread::sleep(Duration::from_millis(200));
            next = Instant::now();
            continue;
        }

        // Open lazily, and retry once a second if it fails: the user may
        // still be wiring the sensor or loading its kernel driver.
        if driver.is_none() {
            match open_driver(info.id) {
                Ok(opened) => driver = Some(opened),
                Err(error) => {
                    board.lock().unwrap()[index].error = Some(error);
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }

        // `as_mut()` turns `&mut Option<Box<..>>` into `Option<&mut Box<..>>`
        // so we can call the driver without taking it out of the Option.
        if let Some(active) = driver.as_mut() {
            let result = active.read();
            let mut guard = board.lock().unwrap();
            let status = &mut guard[index];
            match result {
                Ok(values) => {
                    status.values = values;
                    status.last_ok = Some(Instant::now());
                    status.error = None;
                }
                Err(error) => status.error = Some(error),
            }
        }

        // Sleep until an absolute deadline, so the rate does not drift by
        // however long the reading took. If we fell behind (a slow read),
        // start counting again from now instead of rushing to catch up.
        next += info.period;
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

// ---------------------------------------------------------------------------
// Controller inputs
// ---------------------------------------------------------------------------

/// Where the control loop takes its measurement from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// The simulated water tank in plant.rs.
    SimulatedTank,
    /// Signal number `signal` of a real sensor.
    Sensor { sensor: SensorId, signal: usize },
}

impl Input {
    /// A short label for buttons and the status line.
    pub fn label(&self) -> String {
        match self {
            Input::SimulatedTank => "Simulated tank".to_string(),
            Input::Sensor { sensor, signal } => {
                SENSORS[*sensor as usize].signals[*signal].label.to_string()
            }
        }
    }

    /// Unit of the measurement, for the setpoint slider.
    pub fn unit(&self) -> &'static str {
        match self {
            Input::SimulatedTank => "cm",
            Input::Sensor { sensor, signal } => SENSORS[*sensor as usize].signals[*signal].unit,
        }
    }

    /// Top of the plot and of the setpoint slider.
    pub fn max(&self) -> f64 {
        match self {
            Input::SimulatedTank => 40.0,
            Input::Sensor { sensor, signal } => SENSORS[*sensor as usize].signals[*signal].max,
        }
    }
}

/// Every possible controller input: the simulator first, then each
/// controllable signal of each sensor, in `SENSORS` order.
pub fn inputs() -> Vec<Input> {
    let mut list = vec![Input::SimulatedTank];
    for info in &SENSORS {
        for (signal, s) in info.signals.iter().enumerate() {
            if s.controllable {
                list.push(Input::Sensor {
                    sensor: info.id,
                    signal,
                });
            }
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_ids_match_their_position() {
        for (index, info) in SENSORS.iter().enumerate() {
            assert_eq!(info.id as usize, index);
        }
    }

    #[test]
    fn inputs_start_with_the_simulator_and_skip_monitoring_signals() {
        let list = inputs();
        assert_eq!(list[0], Input::SimulatedTank);
        assert!(list.contains(&Input::Sensor {
            sensor: SensorId::Ultrasonic,
            signal: 1
        }));
        // The DHT-22 is monitoring only.
        assert!(!list.iter().any(|i| matches!(
            i,
            Input::Sensor {
                sensor: SensorId::Dht22,
                ..
            }
        )));
        assert_eq!(list[1].label(), "Ultrasonic level");
    }

    #[test]
    fn stale_values_are_not_used() {
        let status = SensorStatus {
            enabled: true,
            values: vec![1.0, 2.0],
            last_ok: Some(Instant::now() - Duration::from_secs(5)),
            error: None,
        };
        assert_eq!(status.fresh_value(1, Duration::from_secs(10)), Some(2.0));
        assert_eq!(status.fresh_value(1, Duration::from_secs(1)), None);
        assert_eq!(
            status.health(Duration::from_secs(1)),
            Health::Failing("no recent reading".to_string())
        );
    }

    #[test]
    fn disabled_sensor_reports_off_and_no_value() {
        let status = SensorStatus {
            enabled: false,
            values: vec![1.0],
            last_ok: Some(Instant::now()),
            error: None,
        };
        assert_eq!(status.health(Duration::from_secs(1)), Health::Off);
        assert_eq!(status.fresh_value(0, Duration::from_secs(1)), None);
    }
}
