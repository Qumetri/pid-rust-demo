//! Live system statistics for the Stats page: CPU, memory, storage,
//! network and power.
//!
//! Everything comes from files the Linux kernel provides (/proc, /sys) plus
//! a few small helper programs (`vcgencmd`, `ip`, `df`, `iwgetid`). Nothing
//! here needs extra crates or root rights.
//!
//! The Stats page only samples while it is open (see src/bin/ui.rs), about
//! once a second, so it costs nothing the rest of the time.
//!
//! On other systems (the window demo on a Mac or PC) the files do not
//! exist: every value then simply reads as "n/a".
//!
//! Power on a Raspberry Pi 4: the board has **no current or input-voltage
//! sensor**, so current and power cannot be measured in software. What it
//! can report is whether the supply voltage has dropped too low
//! (under-voltage) and whether the CPU had to slow down, both now and
//! since boot. The Pi 5 adds a power-management chip that reports voltages
//! and currents (`vcgencmd pmic_read_adc`); this code uses it when present.

use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::time::Instant;

// ---------------------------------------------------------------------------
// What one sample contains
// ---------------------------------------------------------------------------

/// One network interface, such as eth0 (Ethernet) or wlan0 (Wi-Fi).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetStats {
    pub name: String,
    pub up: bool,
    pub address: Option<String>,
    /// Receive and transmit rates in bytes per second, once known.
    pub rx_rate: Option<f64>,
    pub tx_rate: Option<f64>,
    /// Ethernet link speed in Mb/s.
    pub speed_mbps: Option<u32>,
    /// Wi-Fi network name and signal level in dBm (closer to 0 = stronger).
    pub ssid: Option<String>,
    pub signal_dbm: Option<f64>,
}

/// Readings from the Pi 5 power-management chip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PmicReading {
    /// Voltage at the USB-C input.
    pub input_volts: f64,
    /// Power used by the board, summed over its supply rails.
    pub board_watts: f64,
}

/// Decoded `vcgencmd get_throttled` flags.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Throttle {
    pub under_voltage_now: bool,
    pub under_voltage_since_boot: bool,
    /// Frequency capped or CPU throttled.
    pub slowed_now: bool,
    pub slowed_since_boot: bool,
    pub temp_limit_now: bool,
    pub temp_limit_since_boot: bool,
}

/// Everything shown on the Stats page. `Option` = "could not be read".
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub model: Option<String>,
    pub uptime_s: Option<f64>,

    /// Whole-CPU and per-core usage in percent (needs two samples).
    pub cpu_percent: Option<f64>,
    pub core_percent: Vec<f64>,
    pub load: Option<[f64; 3]>,
    pub clock_mhz: Option<f64>,
    pub max_clock_mhz: Option<f64>,
    pub temperature_c: Option<f64>,

    pub mem_total_mb: Option<f64>,
    pub mem_available_mb: Option<f64>,
    pub swap_total_mb: Option<f64>,
    pub swap_free_mb: Option<f64>,

    /// This program: CPU in percent of one core, memory in MB (PSS).
    pub app_cpu_percent: Option<f64>,
    pub app_memory_mb: Option<f64>,

    /// Root file system (the SD card), in bytes.
    pub disk_total: Option<u64>,
    pub disk_used: Option<u64>,

    pub network: Vec<NetStats>,

    pub throttle: Option<Throttle>,
    pub core_volts: Option<f64>,
    pub pmic: Option<PmicReading>,
}

// ---------------------------------------------------------------------------
// The sampler: keeps the previous counters, so rates can be computed
// ---------------------------------------------------------------------------

/// CPU time counters from one line of /proc/stat.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

pub struct Sampler {
    previous_cpu: Vec<CpuTimes>,
    previous_app: Option<(u64, Instant)>,
    previous_net: HashMap<String, (u64, u64, Instant)>,
    /// Whether this Pi has the Pi 5 power chip; checked once.
    has_pmic: Option<bool>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Sampler {
            previous_cpu: Vec::new(),
            previous_app: None,
            previous_net: HashMap::new(),
            has_pmic: None,
        }
    }

    /// Forget the previous counters. Call this when the Stats page opens,
    /// so the first rates are not averaged over the time it was closed.
    pub fn reset(&mut self) {
        self.previous_cpu.clear();
        self.previous_app = None;
        self.previous_net.clear();
    }

    /// Take one sample. Never fails: unreadable values are `None`.
    pub fn sample(&mut self) -> Snapshot {
        let mut s = Snapshot {
            model: read("/proc/device-tree/model")
                .map(|m| m.trim_end_matches('\0').trim().to_string()),
            uptime_s: read("/proc/uptime")
                .and_then(|t| t.split_whitespace().next()?.parse().ok()),
            load: read("/proc/loadavg").and_then(|t| parse_loadavg(&t)),
            clock_mhz: read_number("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")
                .map(|khz| khz / 1000.0),
            max_clock_mhz: read_number("/sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq")
                .map(|khz| khz / 1000.0),
            temperature_c: read_number("/sys/class/thermal/thermal_zone0/temp")
                .map(|milli| milli / 1000.0),
            ..Default::default()
        };

        // CPU usage: the share of time spent busy since the last sample.
        if let Some(stat) = read("/proc/stat") {
            let now: Vec<CpuTimes> = stat
                .lines()
                .take_while(|l| l.starts_with("cpu"))
                .filter_map(parse_cpu_line)
                .collect();
            if now.len() == self.previous_cpu.len() && !now.is_empty() {
                let usage: Vec<f64> = self
                    .previous_cpu
                    .iter()
                    .zip(&now)
                    .map(|(a, b)| cpu_usage(*a, *b))
                    .collect();
                // Line 0 is the whole CPU, the rest are the cores.
                s.cpu_percent = Some(usage[0]);
                s.core_percent = usage[1..].to_vec();
            }
            self.previous_cpu = now;
        }

        if let Some(info) = read("/proc/meminfo") {
            let m = parse_meminfo(&info);
            s.mem_total_mb = m.get("MemTotal").map(|kb| kb / 1024.0);
            s.mem_available_mb = m.get("MemAvailable").map(|kb| kb / 1024.0);
            s.swap_total_mb = m.get("SwapTotal").map(|kb| kb / 1024.0);
            s.swap_free_mb = m.get("SwapFree").map(|kb| kb / 1024.0);
        }

        // This program's own CPU time (in clock ticks of 1/100 s) and memory.
        if let Some(ticks) = read("/proc/self/stat").and_then(|t| parse_process_ticks(&t)) {
            let now = Instant::now();
            if let Some((previous, then)) = self.previous_app {
                let seconds = (now - then).as_secs_f64();
                if seconds > 0.0 {
                    s.app_cpu_percent = Some((ticks - previous) as f64 / 100.0 / seconds * 100.0);
                }
            }
            self.previous_app = Some((ticks, now));
        }
        s.app_memory_mb = read("/proc/self/smaps_rollup")
            .and_then(|t| parse_meminfo(&t).get("Pss").copied())
            .map(|kb| kb / 1024.0);

        if let Some(out) = run("df", &["-B1", "--output=size,used", "/"]) {
            if let Some((total, used)) = parse_df(&out) {
                s.disk_total = Some(total);
                s.disk_used = Some(used);
            }
        }

        s.network = self.sample_network();

        s.throttle = run("vcgencmd", &["get_throttled"]).and_then(|t| parse_throttled(&t));
        s.core_volts = run("vcgencmd", &["measure_volts", "core"]).and_then(|t| parse_volts(&t));
        if self.has_pmic != Some(false) {
            let reading = run("vcgencmd", &["pmic_read_adc"]).and_then(|t| parse_pmic(&t));
            self.has_pmic = Some(reading.is_some());
            s.pmic = reading;
        }

        s
    }

    fn sample_network(&mut self) -> Vec<NetStats> {
        let mut list = Vec::new();
        let Ok(entries) = fs::read_dir("/sys/class/net") else {
            return list;
        };
        let addresses = run("ip", &["-o", "-4", "addr", "show"])
            .map(|t| parse_ip_addresses(&t))
            .unwrap_or_default();
        let wireless = read("/proc/net/wireless").unwrap_or_default();

        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "lo")
            .collect();
        names.sort();

        for name in names {
            let dir = format!("/sys/class/net/{name}");
            let up = read(&format!("{dir}/operstate")).is_some_and(|s| s.trim() == "up");
            let rx = read_number(&format!("{dir}/statistics/rx_bytes")).map(|v| v as u64);
            let tx = read_number(&format!("{dir}/statistics/tx_bytes")).map(|v| v as u64);

            let mut stats = NetStats {
                name: name.clone(),
                up,
                address: addresses.get(&name).cloned(),
                ..Default::default()
            };

            // Rates from the difference to the previous sample.
            if let (Some(rx), Some(tx)) = (rx, tx) {
                let now = Instant::now();
                if let Some((prx, ptx, then)) = self.previous_net.get(&name) {
                    let seconds = (now - *then).as_secs_f64();
                    if seconds > 0.0 {
                        stats.rx_rate = Some(rx.saturating_sub(*prx) as f64 / seconds);
                        stats.tx_rate = Some(tx.saturating_sub(*ptx) as f64 / seconds);
                    }
                }
                self.previous_net.insert(name.clone(), (rx, tx, now));
            }

            if up {
                // Ethernet reports its link speed; Wi-Fi reports -1 or nothing.
                stats.speed_mbps = read_number(&format!("{dir}/speed"))
                    .filter(|&v| v > 0.0)
                    .map(|v| v as u32);
                stats.signal_dbm = parse_wireless_signal(&wireless, &name);
                if stats.signal_dbm.is_some() {
                    stats.ssid = run("iwgetid", &[name.as_str(), "-r"])
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty());
                }
            }
            list.push(stats);
        }
        list
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn read(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn read_number(path: &str) -> Option<f64> {
    read(path)?.trim().parse().ok()
}

/// Run a helper program and return what it printed, if it succeeded.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

// ---------------------------------------------------------------------------
// Parsers: pure functions on text, so they can be unit-tested with sample
// output instead of a real Pi.
// ---------------------------------------------------------------------------

/// "cpu0 4705 356 584 3699 23 23 0 0 0 0" -> busy and total ticks.
/// The fields are user, nice, system, idle, iowait, irq, softirq, steal...;
/// idle and iowait count as not busy.
pub fn parse_cpu_line(line: &str) -> Option<CpuTimes> {
    let mut parts = line.split_whitespace();
    parts.next()?; // "cpu" or "cpuN"
    let values: Vec<u64> = parts.take(8).filter_map(|v| v.parse().ok()).collect();
    if values.len() < 4 {
        return None;
    }
    let total: u64 = values.iter().sum();
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    Some(CpuTimes {
        busy: total - idle,
        total,
    })
}

/// Percentage of time busy between two samples.
pub fn cpu_usage(before: CpuTimes, after: CpuTimes) -> f64 {
    let total = after.total.saturating_sub(before.total);
    if total == 0 {
        return 0.0;
    }
    after.busy.saturating_sub(before.busy) as f64 / total as f64 * 100.0
}

/// "0.24 0.23 0.30 1/234 5678" -> [0.24, 0.23, 0.30]
pub fn parse_loadavg(text: &str) -> Option<[f64; 3]> {
    let v: Vec<f64> = text
        .split_whitespace()
        .take(3)
        .filter_map(|x| x.parse().ok())
        .collect();
    (v.len() == 3).then(|| [v[0], v[1], v[2]])
}

/// "MemTotal:  926648 kB" lines -> map of name to kB. Also reads the
/// same format in /proc/self/smaps_rollup.
pub fn parse_meminfo(text: &str) -> HashMap<String, f64> {
    text.lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let value = rest.split_whitespace().next()?.parse().ok()?;
            Some((name.trim().to_string(), value))
        })
        .collect()
}

/// User plus system CPU ticks from /proc/self/stat. The program name in
/// field 2 is in parentheses and may contain spaces, so count fields from
/// the closing parenthesis.
pub fn parse_process_ticks(stat: &str) -> Option<u64> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    // After the name: state is field 3, so utime (14) and stime (15) are
    // at index 11 and 12 here.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// `df -B1 --output=size,used /` -> (total, used) in bytes.
pub fn parse_df(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().nth(1)?;
    let mut parts = line.split_whitespace();
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// `ip -o -4 addr show` -> interface name to address, e.g. eth0 -> 192.168.0.7
pub fn parse_ip_addresses(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let name = parts.get(1)?;
            let index = parts.iter().position(|p| *p == "inet")?;
            let address = parts.get(index + 1)?.split('/').next()?;
            Some((name.to_string(), address.to_string()))
        })
        .collect()
}

/// Signal level of one interface from /proc/net/wireless:
/// " wlan0: 0000   54.  -56.  -256  ..." -> -56.0
pub fn parse_wireless_signal(text: &str, interface: &str) -> Option<f64> {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("{interface}:")))?;
    let level = line.split_whitespace().nth(3)?;
    level.trim_end_matches('.').parse().ok()
}

/// "throttled=0x50005" -> decoded flags.
pub fn parse_throttled(text: &str) -> Option<Throttle> {
    let hex = text.trim().strip_prefix("throttled=0x")?;
    let bits = u32::from_str_radix(hex, 16).ok()?;
    let bit = |n: u32| bits & (1 << n) != 0;
    Some(Throttle {
        under_voltage_now: bit(0),
        slowed_now: bit(1) || bit(2),
        temp_limit_now: bit(3),
        under_voltage_since_boot: bit(16),
        slowed_since_boot: bit(17) || bit(18),
        temp_limit_since_boot: bit(19),
    })
}

/// "volt=0.9760V" -> 0.976
pub fn parse_volts(text: &str) -> Option<f64> {
    text.trim()
        .strip_prefix("volt=")?
        .strip_suffix('V')?
        .parse()
        .ok()
}

/// Output of `vcgencmd pmic_read_adc` on a Pi 5, lines such as
///     VDD_CORE_A current(7)=0.71234000A
///     VDD_CORE_V volt(15)=0.87000000V
///     EXT5V_V volt(24)=5.13914000V
/// Board power is the sum of volts x amps over rails that report both
/// (rail "X_A" pairs with "X_V"). Returns None on a Pi 4, where the
/// command does not exist.
pub fn parse_pmic(text: &str) -> Option<PmicReading> {
    let mut amps: HashMap<&str, f64> = HashMap::new();
    let mut volts: HashMap<&str, f64> = HashMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(reading)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Some(value) = reading
            .split_once('=')
            .and_then(|(_, v)| v.trim_end_matches(['A', 'V']).parse::<f64>().ok())
        else {
            continue;
        };
        if let Some(rail) = name.strip_suffix("_A") {
            amps.insert(rail, value);
        } else if let Some(rail) = name.strip_suffix("_V") {
            volts.insert(rail, value);
        }
    }
    let input_volts = *volts.get("EXT5V")?;
    let board_watts = amps
        .iter()
        .filter_map(|(rail, a)| volts.get(rail).map(|v| v * a))
        .sum();
    Some(PmicReading {
        input_volts,
        board_watts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_usage_from_two_samples() {
        let a = parse_cpu_line("cpu  100 0 100 700 100 0 0 0 0 0").unwrap();
        let b = parse_cpu_line("cpu  150 0 150 750 150 0 0 0 0 0").unwrap();
        // 100 busy ticks out of 200: 50 %.
        assert_eq!(cpu_usage(a, b), 50.0);
        assert_eq!(cpu_usage(a, a), 0.0);
    }

    #[test]
    fn reads_meminfo_and_loadavg() {
        let m = parse_meminfo("MemTotal:  926648 kB\nMemAvailable:  369664 kB\nPss:   91908 kB\n");
        assert_eq!(m["MemTotal"], 926648.0);
        assert_eq!(m["Pss"], 91908.0);
        assert_eq!(parse_loadavg("0.24 0.23 0.30 1/234 5678"), Some([0.24, 0.23, 0.30]));
    }

    #[test]
    fn process_ticks_survive_spaces_in_the_name() {
        let stat = "3698 (my ui) S 1 3698 3698 0 -1 4194560 100 0 0 0 1234 567 0 0 20 0 7";
        assert_eq!(parse_process_ticks(stat), Some(1234 + 567));
    }

    #[test]
    fn reads_df_and_ip_output() {
        let df = "     1B-blocks        Used\n30000000000 9000000000\n";
        assert_eq!(parse_df(df), Some((30_000_000_000, 9_000_000_000)));
        let ip = "1: lo    inet 127.0.0.1/8 scope host lo\n2: eth0    inet 192.168.0.7/24 brd 192.168.0.255 scope global eth0\n";
        let map = parse_ip_addresses(ip);
        assert_eq!(map["eth0"], "192.168.0.7");
    }

    #[test]
    fn reads_wifi_signal() {
        let text = "Inter-| sta-|   Quality        |   Discarded packets\n face | tus | link level noise |  nwid  crypt\n wlan0: 0000   54.  -56.  -256        0      0\n";
        assert_eq!(parse_wireless_signal(text, "wlan0"), Some(-56.0));
        assert_eq!(parse_wireless_signal(text, "wlan1"), None);
    }

    #[test]
    fn decodes_throttle_flags() {
        let none = parse_throttled("throttled=0x0").unwrap();
        assert_eq!(none, Throttle::default());
        // Under-voltage now, and under-voltage plus throttling since boot.
        let t = parse_throttled("throttled=0x50001").unwrap();
        assert!(t.under_voltage_now && t.under_voltage_since_boot && t.slowed_since_boot);
        assert!(!t.slowed_now && !t.temp_limit_now);
        assert_eq!(parse_throttled("error"), None);
    }

    #[test]
    fn reads_core_voltage() {
        assert_eq!(parse_volts("volt=0.9760V\n"), Some(0.976));
    }

    #[test]
    fn pmic_power_is_volts_times_amps() {
        let text = "  VDD_CORE_A current(7)=2.00000000A\n  VDD_CORE_V volt(15)=0.50000000V\n  \
                    3V3_SYS_A current(1)=0.10000000A\n  3V3_SYS_V volt(9)=3.30000000V\n  \
                    EXT5V_V volt(24)=5.10000000V\n";
        let r = parse_pmic(text).unwrap();
        assert_eq!(r.input_volts, 5.1);
        assert!((r.board_watts - (1.0 + 0.33)).abs() < 1e-9);
        // A Pi 4 prints an error instead.
        assert_eq!(parse_pmic("error=1 error_msg=\"Command not registered\""), None);
    }
}
