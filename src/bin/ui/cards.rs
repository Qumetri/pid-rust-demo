//! The cards on the Sensors page: one per sensor and per output, built
//! from the live state on the sensor and output boards.

use crate::DeviceRow;
use pid_rust_demo::outputs::{OutputBoard, OUTPUTS};
use pid_rust_demo::sensors::{Health, SensorBoard, SensorInfo, SENSORS};
use slint::{Model, VecModel};

/// Copy new rows into a model, touching only the rows that differ.
///
/// `<T: Clone + PartialEq + 'static>` makes this work for any row type
/// that can be copied and compared, like a C++ template with constraints.
pub fn update_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    // A different number of rows (a network interface appeared): replace
    // the whole list.
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (index, row) in rows.into_iter().enumerate() {
        if model.row_data(index).as_ref() != Some(&row) {
            model.set_row_data(index, row);
        }
    }
}

/// Build the sensor cards from the board's current state.
pub fn sensor_rows(board: &SensorBoard) -> Vec<DeviceRow> {
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
pub fn output_rows(board: &OutputBoard) -> Vec<DeviceRow> {
    let states = board.lock().unwrap().clone();
    OUTPUTS
        .iter()
        .zip(states)
        .map(|(info, state)| {
            let (code, status, reading) = match (&state.error, state.active) {
                (Some(error), _) => (3, error.clone(), String::new()),
                (None, true) => (
                    2,
                    "Active".to_string(),
                    format!("{:.0} %", state.value * 100.0),
                ),
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
pub fn format_reading(info: &SensorInfo, values: &[f64]) -> String {
    info.signals
        .iter()
        .zip(values)
        .map(|(signal, value)| format!("{} {value:.1} {}", signal.name, signal.unit))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pid_rust_demo::sensors;

    #[test]
    fn reading_has_one_line_per_signal() {
        let dht = &SENSORS[sensors::SensorId::Dht22 as usize];
        let text = format_reading(dht, &[21.44, 40.0]);
        assert_eq!(text, "Temperature 21.4 °C\nHumidity 40.0 %");
    }
}
