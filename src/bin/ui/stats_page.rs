//! The Stats page: a thread that samples the system while the page is
//! open, and the lines the page shows for each sample.

use crate::StatRow;
use pid_rust_demo::stats::{Sampler, Snapshot};
use slint::SharedString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// The Stats page's number in the `page` property of window.slint.
pub const STATS_PAGE: i32 = 3;
/// The CPU graph on the Stats page shows this many one-second samples.
pub const CPU_HISTORY_LEN: usize = 60;

/// Where the stats thread leaves its newest sample for the screen.
/// `None` until there is one, and again after the screen has taken it.
pub type LatestStats = Arc<Mutex<Option<Snapshot>>>;

/// The stats thread: sample once a second while the Stats page is open,
/// sleep otherwise.
pub fn run_stats(open: Arc<AtomicBool>, latest: LatestStats) {
    let mut sampler = Sampler::new();
    loop {
        if !open.load(Ordering::Relaxed) {
            // Sleep until the UI calls `unpark()`. A parked thread uses no
            // CPU at all.
            thread::park();
            // Rates (CPU %, network speed) compare two samples. Take a
            // fresh first sample so they do not span the time the page
            // was closed.
            sampler.reset();
            sampler.sample();
            thread::sleep(Duration::from_millis(300));
            continue;
        }
        let snapshot = sampler.sample();
        *latest.lock().unwrap() = Some(snapshot);
        thread::sleep(Duration::from_secs(1));
    }
}

/// The lines of the Stats page, from one snapshot.
pub fn stat_rows(snapshot: &Snapshot) -> Vec<StatRow> {
    let mut rows = Vec::new();
    // Small helpers to keep the list below readable. A closure can capture
    // `rows` mutably only one at a time, so these are plain functions that
    // take it as a parameter.
    fn heading(rows: &mut Vec<StatRow>, text: &str) {
        rows.push(StatRow {
            heading: true,
            label: text.into(),
            value: SharedString::new(),
            level: 0,
        });
    }
    fn line(rows: &mut Vec<StatRow>, label: &str, value: String, level: i32) {
        rows.push(StatRow {
            heading: false,
            label: label.into(),
            value: value.into(),
            level,
        });
    }
    // "n/a" for a value that could not be read.
    let or_na = |value: Option<String>| value.unwrap_or_else(|| "n/a".to_string());

    heading(&mut rows, "System");
    line(&mut rows, "Model", or_na(snapshot.model.clone()), 0);
    line(
        &mut rows,
        "Uptime",
        or_na(snapshot.uptime_s.map(format_duration)),
        0,
    );

    heading(&mut rows, "CPU");
    let cpu = snapshot.cpu_percent;
    line(
        &mut rows,
        "Usage",
        or_na(cpu.map(|c| format!("{c:.0} %"))),
        level_above(cpu, 80.0, 95.0),
    );
    if !snapshot.core_percent.is_empty() {
        let cores: Vec<String> = snapshot
            .core_percent
            .iter()
            .map(|c| format!("{c:.0}"))
            .collect();
        line(&mut rows, "Per core", format!("{} %", cores.join(" · ")), 0);
    }
    line(
        &mut rows,
        "Load average",
        or_na(
            snapshot
                .load
                .map(|l| format!("{:.2}  {:.2}  {:.2}", l[0], l[1], l[2])),
        ),
        0,
    );
    line(
        &mut rows,
        "Clock",
        or_na(snapshot.clock_mhz.map(|c| match snapshot.max_clock_mhz {
            Some(max) => format!("{c:.0} of {max:.0} MHz"),
            None => format!("{c:.0} MHz"),
        })),
        0,
    );
    line(
        &mut rows,
        "Temperature",
        or_na(snapshot.temperature_c.map(|t| format!("{t:.1} °C"))),
        level_above(snapshot.temperature_c, 70.0, 80.0),
    );

    heading(&mut rows, "Memory");
    if let (Some(total), Some(available)) = (snapshot.mem_total_mb, snapshot.mem_available_mb) {
        let level = if available < 50.0 {
            2
        } else if available < 150.0 {
            1
        } else {
            0
        };
        line(
            &mut rows,
            "RAM used",
            format!("{:.0} of {total:.0} MB", total - available),
            level,
        );
        line(
            &mut rows,
            "RAM available",
            format!("{available:.0} MB"),
            level,
        );
    }
    if let (Some(total), Some(free)) = (snapshot.swap_total_mb, snapshot.swap_free_mb) {
        line(
            &mut rows,
            "Swap used",
            format!("{:.0} of {total:.0} MB", total - free),
            0,
        );
    }
    let app = match (snapshot.app_memory_mb, snapshot.app_cpu_percent) {
        (Some(mb), Some(cpu)) => Some(format!("{mb:.0} MB · {cpu:.1} % CPU")),
        (Some(mb), None) => Some(format!("{mb:.0} MB")),
        _ => None,
    };
    line(&mut rows, "This app", or_na(app), 0);

    heading(&mut rows, "Storage");
    if let (Some(total), Some(used)) = (snapshot.disk_total, snapshot.disk_used) {
        let percent = used as f64 / total as f64 * 100.0;
        line(
            &mut rows,
            "SD card",
            format!(
                "{:.1} of {:.1} GB ({percent:.0} %)",
                used as f64 / 1e9,
                total as f64 / 1e9
            ),
            level_above(Some(percent), 85.0, 95.0),
        );
    } else {
        line(&mut rows, "SD card", "n/a".into(), 0);
    }

    heading(&mut rows, "Network");
    if snapshot.network.is_empty() {
        line(&mut rows, "Interfaces", "n/a".into(), 0);
    }
    for net in &snapshot.network {
        let mut parts = vec![if net.up { "up" } else { "down" }.to_string()];
        if let Some(speed) = net.speed_mbps {
            parts.push(format!("{speed} Mb/s"));
        }
        if let Some(ssid) = &net.ssid {
            parts.push(ssid.clone());
        }
        if let Some(dbm) = net.signal_dbm {
            parts.push(format!("{dbm:.0} dBm"));
        }
        if let Some(address) = &net.address {
            parts.push(address.clone());
        }
        line(&mut rows, &net.name, parts.join(" · "), 0);
        if let (true, Some(rx), Some(tx)) = (net.up, net.rx_rate, net.tx_rate) {
            line(
                &mut rows,
                &format!("{} traffic", net.name),
                format!("↓ {}  ↑ {}", format_rate(rx), format_rate(tx)),
                0,
            );
        }
    }

    heading(&mut rows, "Power");
    match snapshot.throttle {
        Some(t) => {
            let (text, level) = flag_text(t.under_voltage_now, t.under_voltage_since_boot);
            line(&mut rows, "Under-voltage", text, level);
            let (text, level) = flag_text(t.slowed_now, t.slowed_since_boot);
            line(&mut rows, "CPU slowed", text, level);
            let (text, level) = flag_text(t.temp_limit_now, t.temp_limit_since_boot);
            line(&mut rows, "Heat limit", text, level);
        }
        None => line(&mut rows, "Supply state", "n/a".into(), 0),
    }
    line(
        &mut rows,
        "Core voltage",
        or_na(snapshot.core_volts.map(|v| format!("{v:.3} V"))),
        0,
    );
    match snapshot.pmic {
        Some(p) => line(
            &mut rows,
            "Input / power",
            format!("{:.2} V · {:.1} W", p.input_volts, p.board_watts),
            0,
        ),
        None => line(
            &mut rows,
            "Current, power",
            "not measurable on a Pi 4".into(),
            0,
        ),
    }
    // Fixed ratings, so the live values above have something to compare to.
    line(&mut rows, "Supply rating", "5.1 V × 3 A = 15.3 W".into(), 0);
    line(&mut rows, "USB ports", "1.2 A in total, shared".into(), 0);

    rows
}

/// 0 normal, 1 warning above `warn`, 2 problem above `bad`.
fn level_above(value: Option<f64>, warn: f64, bad: f64) -> i32 {
    match value {
        Some(v) if v >= bad => 2,
        Some(v) if v >= warn => 1,
        _ => 0,
    }
}

/// A "now / since boot" flag pair as text and warning level.
fn flag_text(now: bool, since_boot: bool) -> (String, i32) {
    if now {
        ("NOW".to_string(), 2)
    } else if since_boot {
        ("earlier, since boot".to_string(), 1)
    } else {
        ("no".to_string(), 0)
    }
}

/// 5580 seconds -> "1 h 33 min"
fn format_duration(seconds: f64) -> String {
    let minutes = (seconds / 60.0) as u64;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days} d {hours} h {minutes} min")
    } else {
        format!("{hours} h {minutes} min")
    }
}

/// Bytes per second -> "820 B/s", "1.2 KB/s", "3.4 MB/s"
fn format_rate(bytes_per_second: f64) -> String {
    if bytes_per_second >= 1e6 {
        format!("{:.1} MB/s", bytes_per_second / 1e6)
    } else if bytes_per_second >= 1e3 {
        format!("{:.1} KB/s", bytes_per_second / 1e3)
    } else {
        format!("{bytes_per_second:.0} B/s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_formatting() {
        assert_eq!(format_duration(5580.0), "1 h 33 min");
        assert_eq!(format_duration(90_000.0), "1 d 1 h 0 min");
        assert_eq!(format_rate(820.0), "820 B/s");
        assert_eq!(format_rate(1234.0), "1.2 KB/s");
        assert_eq!(level_above(Some(75.0), 70.0, 80.0), 1);
        assert_eq!(level_above(None, 70.0, 80.0), 0);
    }

    #[test]
    fn empty_snapshot_shows_not_available() {
        let rows = stat_rows(&Snapshot::default());
        let model = rows.iter().find(|r| r.label == "Model").unwrap();
        assert_eq!(model.value, "n/a");
        let power = rows.iter().find(|r| r.label == "Current, power").unwrap();
        assert_eq!(power.value, "not measurable on a Pi 4");
    }
}
